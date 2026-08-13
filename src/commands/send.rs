use crate::cli::SendArgs;
use crate::command_result::CommandResult;
use crate::error::{AppError, AppResult, ErrorCode};
use crate::mailbox::{
    closest_room, declared_env_pin, declared_sender_address, encode_mail, exclusive_atomic_write,
    local_timestamp, new_mail_id, validate_envelope, Context,
};
use crate::model::{Envelope, RoomMap, SenderProvenance};
use crate::output::{self, SendOutput};
use std::fs;
use std::io::{self, IsTerminal, Read};

const MAX_BODY_BYTES: usize = 32 * 1024;
const MAX_SUBJECT_BYTES: usize = 1024;

pub(super) fn run(
    context: &Context,
    args: SendArgs,
    json_output: bool,
    pretty: bool,
) -> AppResult<CommandResult> {
    run_with_body(context, args, json_output, pretty, read_body)
}

fn run_with_body<F>(
    context: &Context,
    args: SendArgs,
    json_output: bool,
    pretty: bool,
    read_body: F,
) -> AppResult<CommandResult>
where
    F: FnOnce(BodySource<'_>) -> AppResult<String>,
{
    run_with_body_and_id(context, args, json_output, pretty, read_body, new_mail_id)
}

fn run_with_body_and_id<F, G>(
    context: &Context,
    mut args: SendArgs,
    json_output: bool,
    pretty: bool,
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
    let (sender, provenance) = match args.sender {
        Some(sender) => {
            // Pin vs flag DISAGREEMENT is a hard error (M4): a prepared
            // command carrying --from inside a pinned session is exactly the
            // ambiguity the identity layer exists to eliminate. An AGREEING
            // flag is not a conflict and proceeds as declared-flag.
            if let Some(pinned) = declared_env_pin()? {
                if pinned != sender {
                    return Err(AppError::new(
                        ErrorCode::InvalidArgument,
                        format!(
                            "--from '{sender}' conflicts with the POST_FROM pin '{pinned}' set by this session's launcher"
                        ),
                        "Drop --from to send as the pinned identity, or unset POST_FROM if this shell should not be pinned.",
                    )
                    .input(sender)
                    .reason("explicit sender disagrees with the environment pin"));
                }
            }
            (sender, SenderProvenance::DeclaredFlag)
        }
        None => match declared_env_pin()? {
            Some(pinned) => {
                // M4 made a disagreeing --from a hard error, so the old
                // "pass --from to send as someone else" advice would name a
                // command guaranteed to fail. Tell the truth instead.
                eprintln!(
                    "post: sending as '{pinned}' (POST_FROM pin; the pin governs this session — to send as another identity, use a shell without POST_FROM set)"
                );
                (pinned, SenderProvenance::DeclaredEnv)
            }
            None => {
                let (inferred, provenance) = context.infer_from_cwd(&rooms)?;
                // Sender identity is derived from cwd, so a prepared command run
                // from the wrong tree posts as that tree's room. Name the resolved
                // sender on stderr before anything is written; the success receipt
                // is otherwise the first place it appears, which is too late.
                eprintln!(
                    "post: sending as '{inferred}' (identity inferred from cwd); pass --from <NAME> to send as someone else"
                );
                (inferred, provenance)
            }
        },
    };
    // The POST_FROM pin deliberately bypasses the cwd-containment
    // reservation: the pin exists precisely so identity survives a cwd
    // outside the room's tree (specimen 21). It is still only a declaration —
    // recorded as `declared-env` and rendered as evidence at read time, never
    // as a credential. Flag and inference keep the location guard unchanged.
    if provenance != SenderProvenance::DeclaredEnv {
        context.ensure_sender_allowed(&sender, &rooms)?;
    }
    let sender_address = declared_sender_address()?;

    // Self-mail refusal (M4): instances of one room coordinate via channels;
    // routable instances are a recorded non-goal. --allow-self is the
    // deliberate exception for doorbell probes and smoke tests.
    if sender == args.to && !args.allow_self {
        let fix = format!("{fix_prefix} --allow-self --body '<text>'");
        return Err(AppError::new(
            ErrorCode::InvalidArgument,
            format!("refusing to send mail from '{sender}' to itself"),
            format!(
                "Instances of one room coordinate via channels. For a deliberate self-send (doorbell probe, smoke test), run `{fix}`."
            ),
        )
        .exact_fix(fix)
        .input(args.to.clone())
        .reason("from == to without --allow-self"));
    }

    if !rooms.contains_key(&args.to) {
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
            "Run `post rooms`, then retry with `post send --to <registered-room> ...`.",
        )
        .input(args.to.clone())
        .reason("recipient is absent from rooms.json");
        if let Some(room) = suggestion {
            error = error.did_you_mean(room);
        }
        return Err(error);
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
                "Retry with `{fix_prefix} --body '<text>'` or pass a non-empty body file/stdin."
            ),
        )
        .exact_fix(format!("{fix_prefix} --body '<text>'"))
        .input("message body")
        .reason("empty or whitespace-only"));
    }

    // Send-time stamping: profiles are presentation only; identity stays
    // `sender`. Absent profile leaves both fields off the envelope.
    // Registry values are re-validated and free-form (unregistered) senders
    // never stamp — profiles are a per-room contract.
    let profile = crate::profile::stamp_for(context, &sender, &rooms);
    let (id_timestamp, sent) = local_timestamp()?;
    let archive = context.root.join("archive");
    let mut inbox = None;
    let mut delivered = None;
    for attempt in 0..256 {
        let id = next_id(&id_timestamp, attempt)?;
        let envelope = Envelope {
            id: id.clone(),
            from: sender.clone(),
            to: args.to.clone(),
            kind: args.kind,
            subject: args.subject.clone(),
            sent: sent.clone(),
            display_name: profile.name.clone(),
            pfp: profile.pfp.clone(),
            sender_address: sender_address.clone(),
            sender_provenance: Some(provenance.as_str().to_owned()),
        };
        validate_envelope(std::path::Path::new("<generated mail>"), &envelope)?;
        let payload = encode_mail(&envelope, &body)?;
        if inbox.is_none() {
            ensure_route_allowed(context, &rooms, &sender, &args.to)?;
            fs::create_dir_all(&archive)
                .map_err(|error| AppError::io("create archive directory", &archive, error))?;
            inbox = Some(context.mailbox_dirs(&args.to)?.0);
        }
        let inbox = inbox.as_ref().expect("mailbox was initialized");
        let archive_path = archive.join(format!("{id}.mail"));
        let inbox_path = inbox.join(format!("{id}.mail"));
        ensure_route_allowed(context, &rooms, &sender, &args.to)?;
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
    let envelope = delivered.ok_or_else(|| {
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
            },
            pretty,
        )?
    } else {
        format!(
            "post: sent {} {} {} -> {}\n",
            envelope.kind, envelope.id, envelope.from, envelope.to
        )
    };
    Ok(CommandResult::committed(rendered))
}

fn ensure_route_allowed(
    context: &Context,
    rooms: &RoomMap,
    sender: &str,
    recipient: &str,
) -> AppResult<()> {
    let rules = context.load_rules(rooms)?;
    let Some(rule) = rules
        .blocked
        .iter()
        .find(|rule| rule.matches_route(sender, recipient))
    else {
        return Ok(());
    };
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
    if args.allow_self {
        prefix.push_str(" --allow-self");
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
        return read_body_file(path, &source.fix_prefix);
    }
    if io::stdin().is_terminal() {
        let fix = format!("{} --body '<text>'", source.fix_prefix);
        return Err(AppError::new(
            ErrorCode::InvalidArgument,
            "message body is missing and stdin is a terminal; post never prompts or waits for interactive input",
            format!("Run `{fix}`, or pass `--body-file <PATH>`, or pipe the body on stdin."),
        )
        .exact_fix(fix)
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
        Err(error) => Err(
            AppError::io("read UTF-8 message body file", path, error).exact_fix(format!(
                "{fix_prefix} --body-file {}",
                crate::mailbox::shell_quote(&display)
            )),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{run_with_body, run_with_body_and_id};
    use crate::cli::SendArgs;
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
                allow_self: false,
                file: None,
            },
            false,
            false,
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
            .expect("create recipient mailbox");
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
                allow_self: false,
                file: None,
            },
            false,
            false,
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
            .expect("create recipient mailbox");
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
                allow_self: false,
                file: None,
            },
            false,
            false,
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
                allow_self: false,
                file: None,
            },
            false,
            false,
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
            .expect("create recipient mailbox");
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
                allow_self: false,
                file: None,
            },
            false,
            false,
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
}
