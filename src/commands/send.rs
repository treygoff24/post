use crate::channel::ChannelPaths;
use crate::cli::SendArgs;
use crate::command_result::CommandResult;
use crate::error::{AppError, AppResult, ErrorCode};
use crate::mailbox::{
    closest_room, declared_env_pin, declared_sender_address, encode_mail, exclusive_atomic_write,
    local_timestamp, new_mail_id, shell_quote, validate_envelope, Context,
};
use crate::model::{Envelope, RoomMap, SenderProvenance};
use crate::output::{self, SendOutput};
use std::fs;
use std::io::{self, IsTerminal, Read};

const MAX_BODY_BYTES: usize = 32 * 1024;
const MAX_SUBJECT_BYTES: usize = 1024;

/// Read the send body up front, before dispatch takes the shared rename lock
/// (see `commands::execute`). The oversize and watch-NDJSON checks run here,
/// with the read; the empty-body check stays after target resolution.
pub(super) fn read_send_body(args: &mut SendArgs) -> AppResult<String> {
    let fix_prefix = send_fix_prefix(args);
    let inline = args.body.take();
    read_body(BodySource {
        inline,
        body_file: args.body_file.as_deref(),
        file: args.file.as_deref(),
        fix_prefix,
        oversize: args.oversize,
    })
}

pub(super) fn run(
    context: &Context,
    args: SendArgs,
    body: String,
    json_output: bool,
    pretty: bool,
) -> AppResult<CommandResult> {
    // Process env is read exactly once, at the public entry; everything
    // below takes resolved identity as an input. Unit tests inject
    // EnvIdentity::none() so a live launcher pin in the developer's shell
    // can never leak into a parallel test process's shared environment.
    let identity = EnvIdentity::from_env()?;
    // The body was already read by `read_send_body`; the in-body read point
    // hands it over unchanged.
    run_with_body(context, args, json_output, pretty, identity, move |_| {
        Ok(body)
    })
}

/// Identity resolved from the process environment, injected downward.
pub(super) struct EnvIdentity {
    pub pin: Option<String>,
    pub address: Option<String>,
}

impl EnvIdentity {
    fn from_env() -> AppResult<Self> {
        Ok(Self {
            pin: declared_env_pin()?,
            address: declared_sender_address()?,
        })
    }

    #[cfg(test)]
    const fn none() -> Self {
        Self {
            pin: None,
            address: None,
        }
    }
}

fn run_with_body<F>(
    context: &Context,
    args: SendArgs,
    json_output: bool,
    pretty: bool,
    identity: EnvIdentity,
    read_body: F,
) -> AppResult<CommandResult>
where
    F: FnOnce(BodySource<'_>) -> AppResult<String>,
{
    run_with_body_and_id(
        context,
        args,
        json_output,
        pretty,
        identity,
        read_body,
        new_mail_id,
    )
}

fn run_with_body_and_id<F, G>(
    context: &Context,
    mut args: SendArgs,
    json_output: bool,
    pretty: bool,
    identity: EnvIdentity,
    read_body: F,
    mut next_id: G,
) -> AppResult<CommandResult>
where
    F: FnOnce(BodySource<'_>) -> AppResult<String>,
    G: FnMut(&str, u64) -> AppResult<String>,
{
    let rooms = context.load_rooms()?;
    // Built before `sender` is consumed, so a body-input fix can echo the
    // exact flags this invocation used.
    let fix_prefix = send_fix_prefix(&args);
    let actor = context.sender()?;
    let (sender, provenance) = {
        if let Some(declared) = args.sender.as_deref() {
            if let Some(pinned) = identity.pin.as_deref() {
                if pinned != declared {
                    return Err(AppError::new(
                        ErrorCode::InvalidArgument,
                        format!(
                            "--from '{declared}' conflicts with the POST_FROM pin '{pinned}' set by this session's launcher"
                        ),
                        "Drop --from to send as the pinned workspace, or re-bind the participant deliberately.",
                    )
                    .input(declared)
                    .reason("explicit sender disagrees with the environment pin"));
                }
            }
            if declared != actor.from {
                return Err(AppError::new(
                    ErrorCode::InvalidArgument,
                    format!(
                        "--from '{declared}' conflicts with bound participant '{}' reply address '{}'",
                        actor.participant.id, actor.from
                    ),
                    "Drop --from; participant binding determines the sender reply address.",
                )
                .input(declared)
                .reason("explicit sender disagrees with bound participant"));
            }
            // `--from` is now only an assertion about the bound reply
            // address, but retains its legacy anti-impersonation location
            // guard. Omitting it is the normal participant-native path.
            context.ensure_sender_allowed(declared, &rooms)?;
        }
        if let Some(pinned) = identity.pin.as_deref() {
            if pinned != actor.from {
                return Err(AppError::new(
                    ErrorCode::InvalidArgument,
                    format!(
                        "POST_FROM workspace pin '{pinned}' conflicts with bound participant '{}' reply address '{}'",
                        actor.participant.id, actor.from
                    ),
                    "Run `post participant bind --workspace <room>` to change workspace context deliberately.",
                )
                .input(pinned)
                .reason("workspace pin disagrees with bound participant"));
            }
        }
        let provenance = if args.sender.is_some() {
            SenderProvenance::DeclaredFlag
        } else if identity.pin.is_some() {
            SenderProvenance::DeclaredEnv
        } else {
            SenderProvenance::ParticipantBinding
        };
        if identity.pin.is_some() {
            eprintln!(
                "post: sending as '{}' (POST_FROM pin; bound participant {})",
                actor.from, actor.participant.id
            );
        } else {
            eprintln!(
                "post: sending as '{}' (bound participant {})",
                actor.from, actor.participant.id
            );
        }
        (actor.from.clone(), provenance)
    };
    let sender_address = identity.address;
    match crate::bridge_topology::resolve_host_qualified(context, &args.to)? {
        Some(crate::bridge_topology::HostQualified::Remote { id, host }) => {
            return send_remote(
                context,
                RemoteSend {
                    args,
                    rooms: &rooms,
                    actor: &actor,
                    sender,
                    provenance,
                    sender_address,
                    fix_prefix,
                    id,
                    host,
                },
                json_output,
                pretty,
                read_body,
                next_id,
            );
        }
        Some(crate::bridge_topology::HostQualified::Local(address)) => {
            args.to = format!("{}:{}", address.kind.as_str(), address.name);
        }
        None => {}
    }
    let resolved_target = match crate::participant::resolve_target(context, &args.to) {
        Ok(target) => Some(target),
        Err(error) if error.code == ErrorCode::UnknownRoom => None,
        Err(error) => return Err(error),
    };

    if resolved_target.is_none() {
        // Rooms and channels are disjoint namespaces, so a channel name reaching
        // --to used to produce a flat "room is unknown" that never mentioned the
        // destination exists under a different verb. Three papercuts are that
        // sentence. Check the channel registry before claiming ignorance; a
        // leading '#' is accepted here because agents type the rendered form.
        let channel_candidate = args.to.strip_prefix('#').unwrap_or(&args.to);
        let is_channel = ChannelPaths::new(context, channel_candidate)
            .map(|paths| paths.exists())
            .unwrap_or(false);
        if is_channel {
            // PROSE ONLY, for every invocation. A channel is a different
            // protocol from a room, and the cross-protocol correction cannot be
            // built without lying: channels carry no message kind, `post chat`
            // has no --from (it sends as the current bound participant), and a body
            // from stdin cannot be reconstructed into an argument. A `post chat ... --send` command assembled from
            // what survives would run as written while silently dropping the
            // caller's register, sender assertion, or body bytes -- so the
            // refusal names the channel verb and the re-supply instead of
            // handing back a command that is not the caller's invocation. This
            // supersedes the exact_fix this branch used to publish for
            // invocations whose flags all mapped across.
            let guidance = format!(
                "Channels take a different verb: run `post chat {} --send`, re-supplying the body with --body '<text>' or --body-file <path> (the original stdin stream is not preserved in a correction) and the subject with --subject '<text>'. `post send`'s --kind and --from have no channel equivalent: a channel carries no message kind, and `post chat` sends as the current bound participant. No corrected command is offered because none can carry this invocation's kind, sender, and body source.",
                shell_quote(channel_candidate)
            );
            return Err(AppError::new(
                ErrorCode::UnknownRoom,
                format!(
                    "'{}' is a channel, not a room; `post send --to` delivers direct mail to rooms only",
                    args.to
                ),
                guidance,
            )
            .input(args.to.clone())
            .reason("recipient names a channel, not a room"));
        }

        let suggestion = closest_room(&args.to, &rooms);
        let mut error = AppError::new(
            ErrorCode::UnknownRoom,
            match suggestion {
                Some(room) => format!(
                    "recipient room '{}' is unknown; did you mean '{room}'?",
                    args.to
                ),
                None => format!("recipient room '{}' is unknown", args.to),
            },
            "Run `post rooms` to list rooms or `post channels` to list channels, then retry with `post send --to <registered-room>` or `post chat <CHANNEL> --send`.",
        )
        .input(args.to.clone())
        .reason("recipient is absent from rooms.json")
        .matches(rooms.keys().cloned().collect());
        if let Some(room) = suggestion {
            error = error.did_you_mean(room);
        }
        return Err(error);
    }
    let target = resolved_target.expect("known target was checked above");

    validate_subject(&args.subject)?;
    let inline = args.body.take();
    let body = read_body(BodySource {
        inline,
        body_file: args.body_file.as_deref(),
        file: args.file.as_deref(),
        fix_prefix: fix_prefix.clone(),
        oversize: args.oversize,
    })?;
    if body.trim().is_empty() {
        return Err(AppError::new(
            ErrorCode::EmptyBody,
            "message body is empty after trimming whitespace",
            format!(
                "Retry with `{fix_prefix}` and a non-empty body on stdin (heredoc or pipe), or pass --body-file."
            ),
        )
        // Runs as written: the prefix reads the body from stdin. It used to say
        // `--body '<text>'`, which is not a command and points at argv, the
        // form the shell parses before post ever sees it.
        .exact_fix(fix_prefix.clone())
        .input("message body")
        .reason("empty or whitespace-only"));
    }

    // Send-time stamping: profiles are presentation only; identity stays
    // `sender` + `from_participant`. Absent profile leaves both fields off the
    // envelope. Registry values are re-validated; only the acting
    // participant's own entry stamps — profiles are a per-participant contract.
    let profile = crate::profile::stamp_for(context, &actor.participant.id, &sender, &rooms);
    let (id_timestamp, sent) = local_timestamp()?;
    let archive = context.root.join("archive");
    // A letter sent straight to a participant is written into that
    // participant's own directory. `participant gc` collects idle records
    // under the participants lock and re-checks each one's inbox under it, so
    // this write holds the same lock, and first confirms the record is still
    // there (a collected one comes back under its id) instead of creating a
    // directory with mail and no record. Routing takes the lock again below,
    // so it is released before then.
    let delivery_lock = hold_target_record(context, &target)?;
    let mut inbox = None;
    let mut delivered = None;
    for attempt in 0..256 {
        let id = next_id(&id_timestamp, attempt)?;
        let envelope = Envelope {
            id: id.clone(),
            from: sender.clone(),
            to: target.name.clone(),
            kind: args.kind,
            subject: args.subject.clone(),
            sent: sent.clone(),
            from_participant: Some(actor.participant.id.clone()),
            from_lineage: actor.lineage.clone(),
            address_kind: Some(target.kind.as_str().to_owned()),
            to_host: None,
            display_name: profile.name.clone(),
            pfp: profile.pfp.clone(),
            sender_address: sender_address.clone(),
            sender_provenance: Some(provenance.as_str().to_owned()),
        };
        validate_envelope(std::path::Path::new("<generated mail>"), &envelope)?;
        let payload = encode_mail(&envelope, &body)?;
        if inbox.is_none() {
            ensure_route_allowed(context, &rooms, &sender, &target)?;
            if target.kind == crate::participant::AddressKind::Workspace {
                crate::mailbox::ensure_room_not_mid_rename(context, &target.name)?;
            }
            fs::create_dir_all(&archive)
                .map_err(|error| AppError::io("create archive directory", &archive, error))?;
            let target_inbox = target.inbox(context)?;
            fs::create_dir_all(&target_inbox).map_err(|error| {
                AppError::io("create canonical target inbox", &target_inbox, error)
            })?;
            inbox = Some(target_inbox);
        }
        let inbox = inbox.as_ref().expect("mailbox was initialized");
        let archive_path = archive.join(format!("{id}.mail"));
        let inbox_path = inbox.join(format!("{id}.mail"));
        ensure_route_allowed(context, &rooms, &sender, &target)?;
        match exclusive_atomic_write(&inbox_path, &payload) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(AppError::io(
                    "exclusively write inbox mail",
                    &inbox_path,
                    error,
                ));
            }
        }
        match exclusive_atomic_write(&archive_path, &payload) {
            Ok(()) => {
                delivered = Some(envelope);
                break;
            }
            Err(error) => {
                return Err(AppError::delivered_unarchived(
                    &id,
                    &inbox_path,
                    &archive_path,
                    error,
                ));
            }
        }
    }
    drop(delivery_lock);
    let envelope = delivered.ok_or_else(|| {
        AppError::new(
            ErrorCode::IoError,
            "could not allocate a unique message id after 256 attempts",
            "Retry the same send command; if this repeats, run `post doctor`.",
        )
    })?;

    // The canonical mail and archive copy are already committed. A receipt
    // failure deliberately leaves the message pending so the next admitted
    // writer can recover it without a duplicate send.
    let receipt = match crate::cursor_state::routing::route_message(context, &target, &envelope.id)
    {
        Ok(receipt) => receipt,
        Err(error) => {
            eprintln!(
                "post: warning: mail {} was delivered but remains pending because routing failed: {}",
                envelope.id, error.message
            );
            None
        }
    };
    let delivery_status = match receipt.as_ref() {
        Some(receipt) if receipt.recipients.contains(&actor.participant.id) => {
            "sender is a frozen recipient and the message is initially unread"
        }
        Some(_) => "sender is not a frozen recipient; unread is recipient-specific",
        None => "no frozen recipient exists yet; message is pending sender history",
    };

    let rendered = if json_output {
        output::json(
            &SendOutput {
                ok: true,
                envelope,
                archived: true,
                delivery: None,
            },
            pretty,
        )?
    } else {
        format!(
            "post: sent {} {} {} -> {}\npost: canonical message retained at {}:{}; {delivery_status}\npost: read it back with: post read {}\n",
            envelope.kind,
            envelope.id,
            envelope.from,
            envelope.to,
            target.kind.as_str(),
            target.name,
            crate::mailbox::shell_quote(&envelope.id),
        )
    };
    Ok(CommandResult::committed(rendered))
}

/// Everything a host-qualified send carries past sender resolution.
struct RemoteSend<'a> {
    args: SendArgs,
    rooms: &'a RoomMap,
    actor: &'a crate::participant::Sender,
    sender: String,
    provenance: SenderProvenance,
    sender_address: Option<String>,
    fix_prefix: String,
    id: String,
    host: String,
}

/// Queue a letter for `participant:<id>@<host>` (design "The sender side").
/// Nothing is written until the sender, the bridge's capabilities, the
/// subject, the body, and the local rules all pass; then the letter goes to
/// `archive/<mail-id>.mail` only. It never enters a workspace inbox, a
/// participant inbox, or `outbox/`, and it is not routed: the bridge carries
/// it, and the receipt says `queued`, never delivered.
fn send_remote<F, G>(
    context: &Context,
    send: RemoteSend<'_>,
    json_output: bool,
    pretty: bool,
    read_body: F,
    mut next_id: G,
) -> AppResult<CommandResult>
where
    F: FnOnce(BodySource<'_>) -> AppResult<String>,
    G: FnMut(&str, u64) -> AppResult<String>,
{
    let RemoteSend {
        mut args,
        rooms,
        actor,
        sender,
        provenance,
        sender_address,
        fix_prefix,
        id: recipient,
        host,
    } = send;
    // 1. The sender prerequisite: a real local room to reply to.
    match output::room_home(context, &sender) {
        Ok(output::RoomHome::Local) => {}
        Ok(_) => {
            return Err(AppError::new(
                ErrorCode::RemoteSenderUnroutable,
                format!(
                    "participant {} sends from '{sender}', which is not a local room; a remote recipient could not reply",
                    actor.participant.id
                ),
                "Bind to a local room first: `post participant bind --workspace <room>`, naming a room from `post rooms` that is not under remote/.",
            )
            .input(sender.clone())
            .reason("sender workspace is not a registered local room"));
        }
        Err(error) => {
            return Err(AppError::new(
                ErrorCode::ConfigInvalid,
                format!("cannot establish whether '{sender}' is a local room: {error}"),
                "Run `post doctor` and repair rooms.json before sending.",
            ))
        }
    }
    // 2. The capability guard, before anything is written.
    match crate::bridge_topology::bridge_health(context, std::time::SystemTime::now()) {
        crate::bridge_topology::BridgeHealth::Ready => {}
        crate::bridge_topology::BridgeHealth::Unsupported(missing) => {
            return Err(AppError::new(
                ErrorCode::BridgeUnsupported,
                format!(
                    "this host's running bridge predates participant mail (missing: {})",
                    missing.join(", ")
                ),
                "Upgrade the post bridge on this host, then retry; nothing was written.",
            )
            .reason(format!("bridge/health.json lacks {}", missing.join(", "))));
        }
        crate::bridge_topology::BridgeHealth::Unavailable(detail) => {
            return Err(AppError::new(
                ErrorCode::BridgeStatusUnavailable,
                format!("cannot establish that this host's bridge carries participant mail: {detail}"),
                "Check that the post bridge is running (it refreshes bridge/health.json every tick), then retry; nothing was written.",
            )
            .reason(detail));
        }
    }
    validate_subject(&args.subject)?;
    let inline = args.body.take();
    let body = read_body(BodySource {
        inline,
        body_file: args.body_file.as_deref(),
        file: args.file.as_deref(),
        fix_prefix: fix_prefix.clone(),
        oversize: args.oversize,
    })?;
    if body.trim().is_empty() {
        return Err(AppError::new(
            ErrorCode::EmptyBody,
            "message body is empty after trimming whitespace",
            format!(
                "Retry with `{fix_prefix}` and a non-empty body on stdin (heredoc or pipe), or pass --body-file."
            ),
        )
        .exact_fix(fix_prefix)
        .input("message body")
        .reason("empty or whitespace-only"));
    }
    let address = format!("participant:{recipient}@{host}");
    ensure_remote_route_allowed(context, rooms, &sender, &address)?;
    let profile = crate::profile::stamp_for(context, &actor.participant.id, &sender, rooms);
    let (id_timestamp, sent) = local_timestamp()?;
    let archive = context.root.join("archive");
    let mut queued = None;
    for attempt in 0..256 {
        let id = next_id(&id_timestamp, attempt)?;
        let envelope = Envelope {
            id: id.clone(),
            from: sender.clone(),
            to: recipient.clone(),
            kind: args.kind,
            subject: args.subject.clone(),
            sent: sent.clone(),
            from_participant: Some(actor.participant.id.clone()),
            from_lineage: actor.lineage.clone(),
            address_kind: Some(
                crate::participant::AddressKind::Participant
                    .as_str()
                    .to_owned(),
            ),
            to_host: Some(host.clone()),
            display_name: profile.name.clone(),
            pfp: profile.pfp.clone(),
            sender_address: sender_address.clone(),
            sender_provenance: Some(provenance.as_str().to_owned()),
        };
        validate_envelope(std::path::Path::new("<generated mail>"), &envelope)?;
        let payload = encode_mail(&envelope, &body)?;
        let cap = crate::imports::REMOTE_MAX_MAIL_BYTES;
        if payload.len() as u64 > cap {
            return Err(AppError::new(
                ErrorCode::InvalidArgument,
                format!(
                    "this letter is {} bytes; the bridge carries at most {cap} bytes to another host, even with --oversize",
                    payload.len()
                ),
                "Shorten the body, or send a path the recipient can fetch instead of the content; nothing was written.",
            )
            .input("message body")
            .reason(format!("letter exceeds the {cap}-byte bridge limit")));
        }
        // Created only once the letter is known to fit: a refusal writes
        // nothing, not even an empty archive directory.
        fs::create_dir_all(&archive)
            .map_err(|error| AppError::io("create archive directory", &archive, error))?;
        let archive_path = archive.join(format!("{id}.mail"));
        match exclusive_atomic_write(&archive_path, &payload) {
            Ok(()) => {
                queued = Some(envelope);
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(AppError::io(
                    "exclusively write archive mail",
                    &archive_path,
                    error,
                ))
            }
        }
    }
    let envelope = queued.ok_or_else(|| {
        AppError::new(
            ErrorCode::IoError,
            "could not allocate a unique message id after 256 attempts",
            "Retry the same send command; if this repeats, run `post doctor`.",
        )
    })?;
    let rendered = if json_output {
        output::json(
            &SendOutput {
                ok: true,
                envelope,
                archived: true,
                delivery: Some(output::SendDelivery {
                    state: "queued".to_owned(),
                    host,
                }),
            },
            pretty,
        )?
    } else {
        format!(
            "post: sent {} {} {} -> {address}\npost: queued for {host}; not yet delivered.\npost: check it with: post delivery {}\n",
            envelope.kind,
            envelope.id,
            envelope.from,
            crate::mailbox::shell_quote(&envelope.id),
        )
    };
    Ok(CommandResult::committed(rendered))
}

/// Local rules for a remote recipient, whose workspace this host cannot see:
/// only a rule that blocks this sender (or every sender) to every
/// destination (`to == "*"`) can apply, and it does. The destination host
/// enforces its own rules at admission.
fn ensure_remote_route_allowed(
    context: &Context,
    rooms: &RoomMap,
    sender: &str,
    address: &str,
) -> AppResult<()> {
    let rules = context.load_rules(rooms)?;
    let Some(rule) = rules
        .blocked
        .iter()
        .find(|rule| (rule.from == "*" || rule.from == sender) && rule.to == "*")
    else {
        return Ok(());
    };
    Err(AppError::new(
        ErrorCode::BlockedRoute,
        format!("route {sender} -> {address} is blocked: {}", rule.reason),
        "Do not route around this block. Ask the human operator to review rules.json.",
    )
    .input(format!("{sender} -> {address}"))
    .reason(rule.reason.clone())
    .rule(rule.clone()))
}

/// For a letter addressed to a participant: take the participants lock and
/// confirm the target's record is still there, bringing a collected one back.
/// Other targets keep their own stores and need neither.
fn hold_target_record(
    context: &Context,
    target: &crate::participant::Address,
) -> AppResult<Option<std::fs::File>> {
    if target.kind != crate::participant::AddressKind::Participant {
        return Ok(None);
    }
    let lock = crate::participant::lock(context)?;
    if crate::participant::revive_locked(context, &target.name)?.is_none() {
        return Err(AppError::new(
            ErrorCode::NotFound,
            format!("participant target '{}' no longer exists", target.name),
            "Run `post participant list`, then retry with an existing target.",
        ));
    }
    Ok(Some(lock))
}

pub(crate) fn ensure_route_allowed(
    context: &Context,
    rooms: &RoomMap,
    sender: &str,
    target: &crate::participant::Address,
) -> AppResult<()> {
    // Lineage fan-out records blocked affiliates as per-recipient exclusions
    // in the frozen receipt; an allowed affiliate must still receive it.
    if target.kind == crate::participant::AddressKind::Lineage {
        return Ok(());
    }
    let rules = context.load_rules(rooms)?;
    let resolved = crate::cursor_state::routing::resolved_recipients(context, target)?;
    let mut recipient_workspaces = Vec::new();
    for id in resolved {
        let participant = crate::participant::load(context, &id)?.ok_or_else(|| {
            AppError::new(
                ErrorCode::NotFound,
                format!("participant target '{id}' no longer exists"),
                "Run `post participant list`, then retry with an existing target.",
            )
        })?;
        recipient_workspaces.push(participant.workspace);
    }
    if target.kind == crate::participant::AddressKind::Workspace && recipient_workspaces.is_empty()
    {
        recipient_workspaces.push(Some(target.name.clone()));
    }
    let Some(rule) = rules.blocked.iter().find(|rule| {
        recipient_workspaces.iter().any(|workspace| {
            workspace.as_deref().map_or(
                (rule.from == "*" || rule.from == sender) && rule.to == "*",
                |workspace| rule.matches_route(sender, workspace),
            )
        })
    }) else {
        return Ok(());
    };
    let recipient = format!("{}:{}", target.kind.as_str(), target.name);
    Err(AppError::new(
        ErrorCode::BlockedRoute,
        format!("route {sender} -> {recipient} is blocked: {}", rule.reason),
        "Do not route around this block. Ask the human operator to review rules.json.",
    )
    .input(format!("{sender} -> {recipient}"))
    .reason(rule.reason.clone())
    .rule(rule.clone()))
}

/// The three mutually exclusive body sources plus the verbatim command prefix
/// used to build executable fixes. Every fix this module emits has to run
/// as-is: the recurring papercut was a suggested fix the parser then rejected.
pub(super) struct BodySource<'a> {
    pub inline: Option<String>,
    pub body_file: Option<&'a std::path::Path>,
    pub file: Option<&'a std::path::Path>,
    pub fix_prefix: String,
    pub oversize: bool,
}

/// Rebuild the invocation that got us here, so a fix can append the corrected
/// body flag and still be copy-pasteable.
pub(super) fn send_fix_prefix(args: &SendArgs) -> String {
    let mut prefix = format!("post send --to {}", crate::mailbox::shell_quote(&args.to));
    if let Some(sender) = &args.sender {
        prefix.push_str(&format!(" --from {}", crate::mailbox::shell_quote(sender)));
    }
    // --kind always survives into the fix: an exact-fix that silently
    // dropped a non-default kind would retry the send as a `note`.
    prefix.push_str(&format!(" --kind {}", args.kind.as_str()));
    if !args.subject.is_empty() {
        prefix.push_str(&format!(
            " --subject {}",
            crate::mailbox::shell_quote(&args.subject)
        ));
    }
    if args.oversize {
        prefix.push_str(" --oversize");
    }
    prefix
}

pub(super) fn read_body(source: BodySource<'_>) -> AppResult<String> {
    let oversize = source.oversize;
    let body = read_body_unchecked(source)?;
    if !oversize && body.len() > MAX_BODY_BYTES {
        return Err(AppError::new(
            ErrorCode::InvalidArgument,
            format!(
                "message body is {} bytes; the maximum without --oversize is {MAX_BODY_BYTES} bytes",
                body.len()
            ),
            "Inspect the body source. If the size is intentional, add --oversize to the original command and retry.",
        )
        .input("message body")
        .reason(format!("body exceeds {MAX_BODY_BYTES}-byte safety limit")));
    }
    if body.lines().any(is_watch_event_line) {
        eprintln!(
            "post: warning: message body contains Post watch-event NDJSON; shell command substitution may have inserted watch output. Sending anyway; use --body-file for prose containing shell syntax."
        );
    }
    Ok(body)
}

pub(super) fn validate_subject(subject: &str) -> AppResult<()> {
    if subject.len() <= MAX_SUBJECT_BYTES {
        return Ok(());
    }
    Err(AppError::new(
        ErrorCode::InvalidArgument,
        format!(
            "message subject is {} bytes; the maximum is {MAX_SUBJECT_BYTES} bytes",
            subject.len()
        ),
        "Move long text into the message body and keep --subject at or below 1024 bytes.",
    )
    .input("--subject")
    .reason(format!(
        "subject exceeds {MAX_SUBJECT_BYTES}-byte safety limit"
    )))
}

fn read_body_unchecked(source: BodySource<'_>) -> AppResult<String> {
    if let Some(body) = source.inline {
        // `--body -` is the Unix stdin sentinel, not literal text: before this
        // rule an agent piping a body alongside `--body -` silently posted "-"
        // and lost the real message (caught live in #commons, 2026-07-31).
        // A literal one-dash body, if ever wanted, still works via stdin.
        if body != "-" {
            // An inline body that is exactly an existing file's path is almost
            // always a reach for --body-file (three garbled channel posts from
            // one careful agent, 2026-07-31). Reject with the intended command;
            // a literal path-shaped body still works via stdin or a body file.
            if std::path::Path::new(&body).is_file() {
                let fix = format!(
                    "{} --body-file {}",
                    source.fix_prefix,
                    crate::mailbox::shell_quote(&body)
                );
                return Err(AppError::new(
                    ErrorCode::InvalidArgument,
                    "--body is inline text, but its value is an existing file path",
                    format!(
                        "Run `{fix}` to send the file's contents, or pipe the literal text on stdin."
                    ),
                )
                .exact_fix(fix)
                .input("--body")
                .reason("inline body names an existing file"));
            }
            return Ok(body);
        }
    }
    if let Some(path) = source.body_file.or(source.file) {
        // `-` means stdin here for the same reason it does for `--body`: it is
        // the Unix convention, every other CLI honours it, and post did not --
        // it opened a literal file named "-", failed NotFound, and suggested
        // `--body '-'`, which happens to work only by accident. `/dev/stdin`
        // falls through to the ordinary file read below and works on Unix.
        if path.as_os_str() != "-" {
            return read_body_file(path, &source.fix_prefix);
        }
    }
    if io::stdin().is_terminal() {
        // No exact_fix here, deliberately. Every candidate needs content only
        // the caller has, so any command printed would be a template, and this
        // field promises something that runs as written. Same call as the
        // POST_FROM branch in acting_room: when no complete command exists,
        // say so in prose rather than print a fill-in-the-blank.
        let prefix = &source.fix_prefix;
        return Err(AppError::new(
            ErrorCode::InvalidArgument,
            "message body is missing and stdin is a terminal; post never prompts or waits for interactive input",
            format!(
                "Pipe the body on stdin -- `{prefix} <<'EOF' ... EOF` -- or pass --body-file, or --body for a short line."
            ),
        )
        .input("stdin")
        .reason("interactive terminal input is not allowed"));
    }
    let mut body = String::new();
    io::stdin()
        .lock()
        .read_to_string(&mut body)
        .map_err(|error| {
            AppError::io(
                "read message body from stdin",
                std::path::Path::new("<stdin>"),
                error,
            )
        })?;
    Ok(body)
}

fn is_watch_event_line(line: &str) -> bool {
    let Ok(serde_json::Value::Object(fields)) = serde_json::from_str(line) else {
        return false;
    };
    let has_strings = |names: &[&str]| {
        names
            .iter()
            .all(|name| fields.get(*name).and_then(|value| value.as_str()).is_some())
    };
    match fields.get("event").and_then(|value| value.as_str()) {
        Some("mail") => has_strings(&["room", "id", "from", "kind", "subject", "sent"]),
        Some("unreadable") => has_strings(&["room", "id"]),
        Some("channel_message") => has_strings(&["channel", "id", "from", "subject", "sent"]),
        _ => false,
    }
}

fn read_body_file(path: &std::path::Path, fix_prefix: &str) -> AppResult<String> {
    let display = path.display().to_string();
    match fs::read_to_string(path) {
        Ok(body) => Ok(body),
        // The recurring mistake is inline message text landing in the body
        // FILE slot. A path that does not exist is a usage error, not a
        // retryable I/O fault, so it reports as invalid_argument and spells
        // out the corrected command in full.
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let fix = format!(
                "{fix_prefix} --body {}",
                crate::mailbox::shell_quote(&display)
            );
            Err(AppError::new(
                ErrorCode::InvalidArgument,
                format!(
                    "message body file '{display}' does not exist; that argument is a path to a body FILE, not inline message text"
                ),
                format!("If you meant to send that as text, run `{fix}`."),
            )
            .exact_fix(fix)
            .input(display.clone())
            .path(display)
            .reason("body file path does not exist"))
        }
        // Inline prose longer than any file name can be is not a path at all:
        // NAME_MAX is 255 bytes on every Unix post supports, so a 6000-
        // character paste lands here. It is the same usage mistake as a
        // nonexistent path, but the payload must NOT be echoed back -- the
        // generic io_error echoed it in the message and again inside a
        // --body-file fix that could never work, which is how a pasted
        // paragraph becomes a screenful of itself. No runnable fix can carry
        // that payload, so this names the remedy instead, like the oversize
        // body error does.
        Err(error) if error.kind() == io::ErrorKind::InvalidFilename => Err(AppError::new(
            ErrorCode::InvalidArgument,
            format!(
                "the message body argument is {} bytes long, too long to be a file name; that argument is a path to a body FILE, not inline message text",
                display.len()
            ),
            "Send the text with `--body '<text>'`, or pipe it on stdin, instead of a positional path.",
        )
        .reason(error.to_string())
        .operation("read UTF-8 message body file")),
        Err(error) => Err(
            AppError::io("read UTF-8 message body file", path, error).exact_fix(format!(
                "{fix_prefix} --body-file {}",
                crate::mailbox::shell_quote(&display)
            )),
        ),
    }
}

/// The body-bearing flag to append to an `exact_fix`, reproducing the channel
/// the caller actually used. Returns empty when the body arrived on stdin: no
/// command can carry it, and inventing a `--body '<text>'` placeholder is how
/// this field starts lying.
pub(crate) fn send_body_flag(inline: Option<&str>, body_file: Option<&std::path::Path>) -> String {
    if let Some(text) = inline {
        return format!(" --body {}", crate::mailbox::shell_quote(text));
    }
    if let Some(path) = body_file {
        if path.as_os_str() != "-" {
            return format!(
                " --body-file {}",
                crate::mailbox::shell_quote(&path.display().to_string())
            );
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::{hold_target_record, run_with_body, run_with_body_and_id, EnvIdentity};
    use crate::cli::SendArgs;
    use crate::error::ErrorCode;
    use crate::mailbox::Context;
    use crate::model::MailKind;
    use crate::test_support::{test_root, trash_test_root};
    use std::fs;

    fn test_context(label: &str) -> (std::path::PathBuf, Context) {
        let root = test_root(&format!("send-{label}"));
        // home == root in these fixtures, so "~" resolves inside the sandbox.
        fs::write(
            root.join("rooms.json"),
            r#"{"claude-space": "~/claude-space"}"#,
        )
        .expect("write rooms config");
        fs::write(root.join("rules.json"), r#"{"blocked":[]}"#).expect("write rules config");
        (
            root.clone(),
            Context {
                root: root.clone(),
                home: root,
            },
        )
    }

    fn test_identity(context: &Context, workspace: &str) -> EnvIdentity {
        crate::participant::bind_test_actor(context, workspace);
        EnvIdentity::none()
    }

    #[test]
    fn body_after_rule_add_is_refused_before_any_mail_write() {
        let (root, context) = test_context("order");
        let result = run_with_body(
            &context,
            SendArgs {
                to: "claude-space".to_owned(),
                sender: Some("race-test".to_owned()),
                kind: MailKind::Note,
                subject: String::new(),
                body: None,
                body_file: None,
                oversize: false,
                file: None,
            },
            false,
            false,
            test_identity(&context, "race-test"),
            |_| {
                fs::write(
                    root.join("rules.json"),
                    r#"{"blocked":[{"from":"race-test","to":"claude-space","reason":"added while body was read"}]}"#,
                )
                .expect("add rule while body reader is open");
                Ok("body from controllable stream".to_owned())
            },
        );
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("rule added during body read must block delivery"),
        };
        assert_eq!(error.code.as_str(), "blocked_route");
        assert!(!root.join("archive").exists());
        assert!(!root.join("claude-space/inbox").exists());
        trash_test_root(&root);
    }

    #[test]
    fn inbox_id_collision_retries_before_writing_any_archive_copy() {
        let (root, context) = test_context("collision");
        let (inbox, _) = context
            .mailbox_dirs("claude-space")
            .expect("recipient mailbox paths");
        fs::create_dir_all(&inbox).expect("create recipient mailbox");
        let collision_id = "20260715-120000-aaaaaa";
        let fresh_id = "20260715-120000-bbbbbb";
        fs::write(inbox.join(format!("{collision_id}.mail")), "existing mail")
            .expect("create colliding inbox mail");
        let mut ids = [collision_id, fresh_id].into_iter();

        let result = run_with_body_and_id(
            &context,
            SendArgs {
                to: "claude-space".to_owned(),
                sender: Some("collision-test".to_owned()),
                kind: MailKind::Note,
                subject: String::new(),
                body: Some("new mail".to_owned()),
                body_file: None,
                oversize: false,
                file: None,
            },
            false,
            false,
            test_identity(&context, "collision-test"),
            |source| Ok(source.inline.expect("inline body")),
            |_, _| Ok(ids.next().expect("test provides two ids").to_owned()),
        )
        .expect("send should retry the colliding id");

        assert!(result.stdout.contains(fresh_id));
        assert_eq!(
            fs::read_to_string(inbox.join(format!("{collision_id}.mail")))
                .expect("read colliding inbox mail"),
            "existing mail"
        );
        assert!(!root.join(format!("archive/{collision_id}.mail")).exists());
        assert!(inbox.join(format!("{fresh_id}.mail")).is_file());
        assert!(root.join(format!("archive/{fresh_id}.mail")).is_file());
        trash_test_root(&root);
    }

    #[test]
    fn id_collision_retry_rechecks_a_new_blocking_rule() {
        let (root, context) = test_context("collision-rule");
        let (inbox, _) = context
            .mailbox_dirs("claude-space")
            .expect("recipient mailbox paths");
        fs::create_dir_all(&inbox).expect("create recipient mailbox");
        let collision_id = "20260715-120000-111111";
        let fresh_id = "20260715-120000-222222";
        fs::write(inbox.join(format!("{collision_id}.mail")), "existing mail")
            .expect("create colliding inbox mail");

        let result = run_with_body_and_id(
            &context,
            SendArgs {
                to: "claude-space".to_owned(),
                sender: Some("rule-race".to_owned()),
                kind: MailKind::Note,
                subject: String::new(),
                body: Some("new mail".to_owned()),
                body_file: None,
                oversize: false,
                file: None,
            },
            false,
            false,
            test_identity(&context, "rule-race"),
            |source| Ok(source.inline.expect("inline body")),
            |_, attempt| {
                if attempt == 1 {
                    fs::write(
                        root.join("rules.json"),
                        r#"{"blocked":[{"from":"rule-race","to":"claude-space","reason":"added after collision"}]}"#,
                    )
                    .expect("add blocking rule before retry");
                }
                Ok(if attempt == 0 { collision_id } else { fresh_id }.to_owned())
            },
        );

        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("new blocking rule must stop the retry"),
        };
        assert_eq!(error.code.as_str(), "blocked_route");
        assert_eq!(
            fs::read_to_string(inbox.join(format!("{collision_id}.mail"))).unwrap(),
            "existing mail"
        );
        assert!(!inbox.join(format!("{fresh_id}.mail")).exists());
        assert_eq!(
            fs::read_dir(root.join("archive"))
                .expect("list archive")
                .count(),
            0
        );
        trash_test_root(&root);
    }

    #[test]
    fn archive_id_collision_preserves_old_archive_and_reports_committed_delivery() {
        let (root, context) = test_context("archive-collision");
        let id = "20260715-120000-a1c1d1";
        let archive = root.join("archive");
        fs::create_dir_all(&archive).expect("create archive fixture");
        let archive_path = archive.join(format!("{id}.mail"));
        fs::write(&archive_path, "immutable old archive").expect("create archive collision");

        let result = run_with_body_and_id(
            &context,
            SendArgs {
                to: "claude-space".to_owned(),
                sender: Some("archive-collision".to_owned()),
                kind: MailKind::Note,
                subject: String::new(),
                body: Some("new delivery".to_owned()),
                body_file: None,
                oversize: false,
                file: None,
            },
            false,
            false,
            test_identity(&context, "archive-collision"),
            |source| Ok(source.inline.expect("inline body")),
            |_, _| Ok(id.to_owned()),
        );

        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("archive collision must report delivered_unarchived"),
        };
        assert_eq!(error.code.as_str(), "delivered_unarchived");
        assert!(!error.retryable);
        assert_eq!(
            fs::read_to_string(&archive_path).unwrap(),
            "immutable old archive"
        );
        assert!(root.join(format!("claude-space/inbox/{id}.mail")).is_file());
        trash_test_root(&root);
    }

    #[test]
    fn exhausting_all_message_ids_leaves_existing_mail_untouched_and_is_retryable() {
        let (root, context) = test_context("id-exhaustion");
        let (inbox, _) = context
            .mailbox_dirs("claude-space")
            .expect("recipient mailbox paths");
        fs::create_dir_all(&inbox).expect("create recipient mailbox");
        let id = "20260715-120000-eeeeee";
        let inbox_path = inbox.join(format!("{id}.mail"));
        fs::write(&inbox_path, "existing inbox mail").expect("create collision fixture");

        let result = run_with_body_and_id(
            &context,
            SendArgs {
                to: "claude-space".to_owned(),
                sender: Some("exhaustion-test".to_owned()),
                kind: MailKind::Note,
                subject: String::new(),
                body: Some("new mail".to_owned()),
                body_file: None,
                oversize: false,
                file: None,
            },
            false,
            false,
            test_identity(&context, "exhaustion-test"),
            |source| Ok(source.inline.expect("inline body")),
            |_, _| Ok(id.to_owned()),
        );

        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("256 collisions must exhaust id allocation"),
        };
        assert_eq!(error.code.as_str(), "io_error");
        assert!(error.retryable);
        assert_eq!(
            fs::read_to_string(&inbox_path).unwrap(),
            "existing inbox mail"
        );
        assert_eq!(fs::read_dir(&inbox).expect("list inbox").count(), 1);
        assert_eq!(
            fs::read_dir(root.join("archive"))
                .expect("list archive")
                .count(),
            0
        );
        trash_test_root(&root);
    }

    #[test]
    fn participant_review_unit_send_stamps_actor_fields() {
        let (root, context) = test_context("participant-fields");
        let result = run_with_body(
            &context,
            SendArgs {
                to: "claude-space".to_owned(),
                sender: Some("unit-sender".to_owned()),
                kind: MailKind::Note,
                subject: String::new(),
                body: Some("body".to_owned()),
                body_file: None,
                oversize: false,
                file: None,
            },
            true,
            false,
            test_identity(&context, "unit-sender"),
            |source| Ok(source.inline.expect("inline body")),
        )
        .expect("unit send");
        let receipt: serde_json::Value =
            serde_json::from_str(&result.stdout).expect("send receipt JSON");
        assert!(receipt["envelope"]["from_participant"].is_string());
        assert_eq!(receipt["envelope"]["address_kind"], "workspace");
        trash_test_root(&root);
    }

    fn participant_target(id: &str) -> crate::participant::Address {
        crate::participant::Address {
            kind: crate::participant::AddressKind::Participant,
            name: id.to_owned(),
        }
    }

    /// A letter sent straight to a participant is written into that
    /// participant's own directory. If `participant gc` collected the record
    /// between the target being resolved and the write, the write finds it
    /// again (same id) instead of leaving mail in a directory with no record.
    #[test]
    fn a_letter_to_a_participant_collected_meanwhile_finds_its_record_back() {
        use crate::commands::participant_gc::test_seed::{days_ago, seed_in};
        let (root, context) = test_context("collected-target");
        let now = std::time::SystemTime::now();
        let (id, _) = seed_in(&root, "collected-target", &days_ago(40, now), false, None);
        let plan = crate::commands::participant_gc::plan(&context, now).expect("plan");
        let applied =
            crate::commands::participant_gc::apply_plan(&context, &plan, now, &mut |_| Ok(()))
                .expect("apply");
        assert_eq!(applied.deleted, vec![id.clone()]);
        assert!(crate::participant::load(&context, &id)
            .expect("load")
            .is_none());

        let held = hold_target_record(&context, &participant_target(&id))
            .expect("hold the target's record")
            .expect("a participant target takes the lock");
        assert!(
            crate::participant::load(&context, &id)
                .expect("load")
                .is_some(),
            "the record is back before anything is written into its directory"
        );
        drop(held);

        // A target that was never a participant is refused, not created.
        let refused = hold_target_record(&context, &participant_target("claude-ffffffff"));
        assert_eq!(
            refused.expect_err("unknown target").code,
            ErrorCode::NotFound
        );
        assert!(!root.join("participants/claude-ffffffff").exists());
        // Rooms and lineages keep their own stores and take no lock.
        let room = crate::participant::Address {
            kind: crate::participant::AddressKind::Workspace,
            name: "claude-space".to_owned(),
        };
        assert!(hold_target_record(&context, &room).expect("room").is_none());
        trash_test_root(&root);
    }

    /// The whole path, not just the helper: a send whose target is collected
    /// while it is in flight waits for the collection (which holds the lock),
    /// then finds the record back under its id and delivers into it.
    #[test]
    fn a_send_in_flight_while_its_target_is_collected_lands_in_a_record() {
        use crate::commands::participant_gc::test_seed::{days_ago, seed_in};
        use std::sync::mpsc;
        use std::time::Duration;
        let (root, context) = test_context("in-flight");
        let now = std::time::SystemTime::now();
        let (target, _) = seed_in(&root, "in-flight-target", &days_ago(40, now), false, None);
        let plan = crate::commands::participant_gc::plan(&context, now).expect("plan");
        assert_eq!(plan.actions.len(), 1, "the target is the one candidate");

        let (ready_tx, ready_rx) = mpsc::channel::<()>();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let (done_tx, done_rx) = mpsc::channel();
        let worker_context = Context {
            root: context.root.clone(),
            home: context.home.clone(),
        };
        let to = format!("participant:{target}");
        let worker = std::thread::spawn(move || {
            // The actor is per thread, and binding takes the lock: bind first.
            let identity = test_identity(&worker_context, "in-flight-sender");
            ready_tx.send(()).expect("ready");
            go_rx.recv().expect("go");
            let result = run_with_body(
                &worker_context,
                SendArgs {
                    to,
                    sender: Some("in-flight-sender".to_owned()),
                    kind: MailKind::Note,
                    subject: String::new(),
                    body: Some("mid-flight".to_owned()),
                    body_file: None,
                    oversize: false,
                    file: None,
                },
                true,
                false,
                identity,
                |source| Ok(source.inline.expect("inline body")),
            );
            done_tx.send(result.map(|sent| sent.stdout)).expect("done");
        });
        ready_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the sender is ready");

        let mut checks = 0;
        let applied =
            crate::commands::participant_gc::apply_plan(&context, &plan, now, &mut |step| {
                if step == crate::commands::participant_gc::Step::BeforeCheck {
                    checks += 1;
                    // The collection holds the lock and has not moved anything:
                    // the send starts now, sees its target, and must wait.
                    go_tx.send(()).expect("go");
                    std::thread::sleep(Duration::from_millis(500));
                    assert!(
                        done_rx.try_recv().is_err(),
                        "the send finished while the collection held the lock"
                    );
                }
                Ok(())
            })
            .expect("apply");
        assert_eq!(checks, 1);
        assert_eq!(
            applied.deleted,
            vec![target.clone()],
            "collected as planned"
        );

        let receipt = done_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the send finishes once the lock is free")
            .expect("and delivers");
        worker.join().expect("worker");
        let receipt: serde_json::Value = serde_json::from_str(&receipt).expect("receipt JSON");
        let letter = receipt["envelope"]["id"].as_str().expect("mail id");
        assert!(
            crate::participant::load(&context, &target)
                .expect("load")
                .is_some(),
            "the record is back under its id"
        );
        assert!(
            root.join("participants")
                .join(&target)
                .join("inbox")
                .join(format!("{letter}.mail"))
                .is_file(),
            "the letter is in it"
        );
        trash_test_root(&root);
    }

    /// The write takes the participants lock, the one `participant gc` holds
    /// while it re-checks and removes a record: a send that arrives during a
    /// collection waits for it rather than writing into a directory being moved.
    #[test]
    fn a_letter_to_a_participant_waits_for_the_participants_lock() {
        let (root, context) = test_context("target-lock");
        crate::participant::bind_test_actor(&context, "lock-target");
        let target = participant_target(
            &crate::participant::resolve(&context)
                .expect("resolve")
                .participant()
                .expect("bound")
                .id,
        );
        let guard = crate::participant::lock(&context).expect("hold the lock");
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker_context = Context {
            root: context.root.clone(),
            home: context.home.clone(),
        };
        let worker = std::thread::spawn(move || {
            let held = hold_target_record(&worker_context, &target).expect("hold");
            sender.send(()).expect("signal");
            drop(held);
        });
        assert!(
            receiver
                .recv_timeout(std::time::Duration::from_millis(400))
                .is_err(),
            "the send went ahead while the lock was held"
        );
        drop(guard);
        receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the send proceeds once the lock is free");
        worker.join().expect("worker");
        trash_test_root(&root);
    }
}
