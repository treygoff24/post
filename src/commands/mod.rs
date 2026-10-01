mod bridge;
mod byte_budget;
mod catchup;
mod channels;
mod chat;
mod contract;
mod delivery;
mod doctor;
mod identity;
mod inbox;
mod owner;
mod participant;
mod participant_auto_gc;
mod participant_gc;
mod profile;
mod read;
mod rooms;
mod schema;
mod search;
mod send;
mod version;
pub mod watch;
mod who;

use crate::cli::{Cli, Command};
use crate::command_result::CommandResult;
use crate::error::AppResult;
use crate::error::{AppError, ErrorCode};
use crate::mailbox::Context;
use crate::migration_fence;
use serde::Serialize;

pub(crate) fn execute(mut cli: Cli) -> AppResult<CommandResult> {
    let pretty = cli.pretty;
    let json = cli.json;
    if matches!(&cli.command, Command::Version) {
        return version::run(json, pretty);
    }
    // The contract is compiled in: no mailbox, participant, or fence.
    if let Command::Contract(args) = &cli.command {
        return contract::run(args, pretty);
    }
    let context = Context::from_env()?;
    // Bridge-only commands act for the bridge, never for a participant: no
    // participant resolution, no activity touch, and no first-run defaults.
    // They take their own fence admission so a refusal is a decided answer.
    if let Command::Bridge(args) = cli.command {
        return bridge::run(&context, args, pretty);
    }
    // A read never reads stdin: input on it is refused before anything is
    // resolved, admitted, bound, routed, or marked read, and before any
    // return path (an unbound marker included) could ignore it.
    if let Command::Read(args) = &cli.command {
        read::refuse_unintended_stdin(args, json, pretty)?;
    }
    let writes =
        migration_fence::classify_write(&cli.command) || participant_registry_write(&cli.command);
    let long_watch = matches!(&cli.command, Command::Watch(args) if !args.snapshot);
    let explicit_bootstrap = explicit_participant_bootstrap(&cli.command);
    let participant_is_required = participant_required(&cli.command);
    let resolution_required = participant_is_required || plain_participant_bind(&cli.command);
    // Who is acting. A claim that names no record (`participant_missing`) is
    // an error for every command that reads or writes as a participant; only
    // the commands that diagnose or repair identity, or never act as a
    // participant at all, carry it as a field and run. Any other resolution
    // failure keeps its old shape: an error for a command that needs a
    // participant, an advisory for the rest.
    //
    // The one claim that is repaired rather than reported: an explicit
    // `POST_PARTICIPANT` naming a record `participant gc` collected (Loom
    // exports one from a `bind --new` record, and it can sit idle for a day).
    // Restoring it changes the store, so only a command that is a writer does
    // it, and only once the migration fence has admitted that command (below):
    // a refused write leaves the collected record where it was. A command that
    // only reads reports the claim (`participant_missing`, whose fix is
    // `post participant restore <id>`) and creates nothing.
    let mut claim_to_restore = None;
    let (mut resolved_participant, resolution_error) = if explicit_bootstrap {
        (crate::participant::Resolved::Unbound, None)
    } else {
        match crate::participant::resolve(&context) {
            Ok(resolved) => (resolved, None),
            Err(error)
                if error.code == ErrorCode::ParticipantMissing
                    && !tolerates_resolution_error(&cli.command, &error, resolution_required) =>
            {
                if writes && crate::participant::explicit_claim_is_collected(&context)? {
                    claim_to_restore = Some(error);
                    (crate::participant::Resolved::Unbound, None)
                } else {
                    // A reader, or a claim nothing ever held: the claim is
                    // wrong, and says so.
                    return Err(error);
                }
            }
            Err(error) if tolerates_resolution_error(&cli.command, &error, resolution_required) => {
                (crate::participant::Resolved::Unbound, Some(error))
            }
            Err(error) => return Err(error),
        }
    };
    // Unbound readers get an explicit marker on stdout instead of running:
    // their bodies would otherwise guess a room from the working directory.
    if resolved_participant.participant().is_none()
        && resolution_error.is_none()
        && claim_to_restore.is_none()
    {
        if let Some(marker) = unbound_reader_marker(&context, &cli.command, json)? {
            return Ok(marker);
        }
    }
    let mut admission = if writes {
        Some(migration_fence::admit(&context, true)?)
    } else {
        None
    };
    // Admitted: now the collected claim is brought back, under the lock, as
    // the same participant, before the command runs.
    let mut revived_claim = None;
    if let Some(error) = claim_to_restore {
        match crate::participant::revive_explicit_claim(&context) {
            Ok(Some(revived)) => {
                revived_claim = Some(BoundNow {
                    id: revived.id.clone(),
                    workspace: revived.workspace.clone(),
                });
                resolved_participant = crate::participant::Resolved::Bound {
                    participant: Box::new(revived),
                    provenance: crate::participant::Provenance::ExplicitEnv,
                };
            }
            // Gone between the check and the lock: the claim is wrong.
            Ok(None) => return Err(error),
            Err(cause) => {
                return Err(error.reason(format!(
                    "the collected record could not be restored: {}",
                    cause.message
                )))
            }
        }
    }
    // A write run with a harness conversation key but no record yet binds the
    // session first, exactly as `participant bind --harness <h> --key <key>`
    // would (same deterministic id), then proceeds. Without a key it is the
    // bind-command error.
    let mut lazy_binding = None;
    if participant_is_required && resolved_participant.participant().is_none() {
        lazy_binding = match lazy_mint_key(&cli.command) {
            true => crate::participant::ambient_key()?,
            false => None,
        };
        if lazy_binding.is_none() {
            return Err(AppError::no_participant(
                crate::participant::bind_key_available()?,
            ));
        }
    }
    let enrolled_watch = long_watch
        && admission
            .as_ref()
            .is_some_and(migration_fence::WriteAdmission::is_enrolled);
    // Listings, peeks, snapshots, and other readers are pure regardless of
    // whether a participant is bound or whether the store is enrolled.
    let fenced_read = enrolled_watch || !writes;
    let _read_only = crate::mailbox::enter_read_only_command(fenced_read);
    if !fenced_read && !matches!(&cli.command, Command::Doctor(_)) {
        context.prepare_first_run()?;
    }
    // A write that brought its collected participant back says so, like one
    // that bound its session; a reader treats the participant as bound and
    // empty, and its output keeps its own shape.
    let mut bound_now = revived_claim.filter(|_| writes);
    if let Some((harness, key)) = lazy_binding {
        let cwd = std::env::current_dir().map_err(|error| {
            AppError::io(
                "resolve current directory to bind this session",
                ".".as_ref(),
                error,
            )
        })?;
        let minted = crate::participant::bind(&context, &cwd, None, Some((&harness, &key)), false)?;
        bound_now = Some(BoundNow {
            id: minted.id.clone(),
            workspace: minted.workspace.clone(),
        });
        resolved_participant = crate::participant::resolve(&context)?;
    }
    if writes && !participant_command_manages_activity(&cli.command) {
        if let Some(participant) = resolved_participant.participant() {
            crate::participant::touch(&context, &participant.id)?;
        }
    }
    // Startup admission only proves that the watch may enter its setup phase;
    // heartbeat admissions must be able to take the lock independently.
    if long_watch {
        drop(admission.take());
    }
    let report_unbound = resolved_participant.participant().is_none()
        && !writes
        && !matches!(
            &cli.command,
            Command::Participant(crate::cli::ParticipantArgs {
                command: crate::cli::ParticipantCommand::Show(_),
            })
        );
    let annotate_unbound_json = report_unbound && unbound_json_listing(&cli.command);
    // `schema` and `doctor` never leave an identity line on stderr: `schema`
    // is about the tool, not the session, and `doctor` already carries the
    // identity state in its own output (`participant`, and `participant_missing`
    // for a claim that names no record), so the line is noise beside it.
    let stderr_identity_line = !matches!(&cli.command, Command::Schema | Command::Doctor(_));
    // clap enforces `conflicts_with = "json"` only when the global flag
    // FOLLOWS the subcommand; `post --json <cmd> --text` parses fine. Every
    // human-only flag is therefore re-checked here, ordering-independent.
    let human_only_flag = match &cli.command {
        Command::Doctor(args) if args.brief => Some("--brief"),
        Command::Channels(args) if args.text => Some("--text"),
        Command::Who(args) if args.text => Some("--text"),
        Command::Inbox(args) if args.text => Some("--text"),
        Command::Watch(args) if args.text => Some("--text"),
        _ => None,
    };
    if let (Some(flag), true) = (human_only_flag, cli.json) {
        return Err(AppError::new(
            ErrorCode::InvalidArgument,
            format!("{flag} cannot be used with --json: it selects the human-only output"),
            format!("Drop {flag} for the JSON output, or drop --json for the human form."),
        ));
    }
    let channels_text = matches!(&cli.command, Command::Channels(args) if args.text);
    // Commands that create or write a room's mailbox by room name (send's
    // canonical inbox, legacy read's inbox/read move and room cursors, legacy
    // chat's room cursors), and catchup, whose after-stdout commit writes
    // seen ids keyed by the workspace it resolved, hold the shared
    // room-rename lock from before their body loads rooms.json or resolves
    // its actor until their deferred after-stdout commits finish, so
    // `rooms rename` (which holds it exclusively) can never move a room out
    // from under them. Lock order: the migration fence admission (above) is
    // the only lock taken before it; every store lock the body takes comes
    // after it. See `mailbox::RENAME_LOCK_FILE`.
    //
    // `send` reads its body (stdin, --body-file, or --body, with the oversize
    // and watch-NDJSON checks) BEFORE taking the lock: a stalled stdin
    // producer must never hold a shared lock, because flock gives the
    // exclusive `rooms rename` waiter no priority and it would wait forever.
    let send_body = match &mut cli.command {
        Command::Send(args) => Some(send::read_send_body(args)?),
        _ => None,
    };
    let rename_lock = if writes
        && matches!(
            &cli.command,
            Command::Send(_) | Command::Read(_) | Command::Chat(_) | Command::Catchup(_)
        ) {
        Some(context.lock_rename(false)?)
    } else {
        None
    };
    let mut result = match cli.command {
        Command::Participant(args) => participant::run(&context, args, json, pretty),
        Command::Identity(args) => identity::run(&context, args, json, pretty),
        Command::Doctor(args) => doctor::run(&context, args, pretty),
        Command::Send(args) => send::run(
            &context,
            args,
            send_body.expect("send body is read before the rename lock"),
            json,
            pretty,
        ),
        Command::Chat(args) => chat::run(&context, args, json, pretty),
        Command::Channels(args) => channels::run(&context, args, pretty),
        Command::Inbox(args) => inbox::run(&context, args, pretty),
        Command::Read(args) => read::run(&context, args, json, pretty),
        Command::Catchup(args) => catchup::run(&context, args, json, pretty),
        Command::Search(args) => search::run(&context, args, json, pretty),
        Command::Rooms(args) => rooms::run(&context, args, pretty),
        Command::Profile(args) => profile::run(&context, args, json, pretty),
        Command::Owner(args) => owner::run(&context, args, pretty),
        Command::Schema => schema::run(&context, pretty),
        Command::Watch(args) => watch::run(&context, args),
        Command::Who(args) => who::run(&context, args, pretty),
        Command::Version => unreachable!("version dispatches before mailbox context resolution"),
        Command::Contract(_) => {
            unreachable!("contract dispatches before mailbox context resolution")
        }
        Command::Bridge(_) => unreachable!("bridge dispatches before participant resolution"),
        Command::Delivery(args) => delivery::run(&context, args, json, pretty),
    }?;
    if let Some(lock) = rename_lock {
        result = result.holding(lock);
    }
    if let Some(bound_now) = bound_now.as_ref() {
        annotate_bound_now(&mut result, bound_now);
    }
    if report_unbound {
        // The marker on stdout is what an agent sees; stderr keeps the human
        // line for text mode only, and never appears under --json.
        if !json && stderr_identity_line {
            eprintln!("{}", unbound_stderr_line(resolution_error.as_ref()));
        }
        match resolution_error.as_ref() {
            Some(error) => {
                if !json && stderr_identity_line {
                    eprintln!("participant resolution error: {}", error.message);
                }
            }
            None if channels_text => {
                result.stdout = format!("post: {}\n{}", unbound_hint(), result.stdout);
            }
            None => {}
        }
        if annotate_unbound_json {
            annotate_unbound(&mut result, pretty, resolution_error.as_ref())?;
        }
    }
    if !long_watch && writes {
        let admission = admission.expect("writer admission exists");
        let action = result.after_stdout.take();
        result.after_stdout = Some(Box::new(move || {
            let _admission = admission;
            action.map_or(Ok(()), |action| action())
        }));
    }
    Ok(result)
}

/// Whether a failed identity resolution lets the command run with the failure
/// carried as a field. A claim that names no record (`participant_missing`)
/// runs only where the command diagnoses or repairs identity, or never acts as
/// a participant; any other failure runs unless the command needs a participant.
fn tolerates_resolution_error(
    command: &Command,
    error: &AppError,
    resolution_required: bool,
) -> bool {
    if error.code != ErrorCode::ParticipantMissing {
        return !resolution_required;
    }
    use crate::cli::{IdentityCommand, ParticipantCommand, ProfileCommand};
    match command {
        Command::Who(_)
        | Command::Doctor(_)
        | Command::Schema
        | Command::Rooms(_)
        | Command::Owner(_) => true,
        Command::Participant(args) => matches!(
            &args.command,
            ParticipantCommand::Show(_)
                | ParticipantCommand::Bind(_)
                | ParticipantCommand::List
                | ParticipantCommand::Gc(_)
                | ParticipantCommand::Restore(_)
        ),
        Command::Identity(args) => {
            matches!(
                &args.command,
                IdentityCommand::List | IdentityCommand::Show(_)
            )
        }
        Command::Profile(args) => matches!(
            &args.command,
            Some(ProfileCommand::List(_))
                | Some(ProfileCommand::Show(crate::cli::ProfileShowArgs {
                    participant: Some(_)
                }))
                | Some(ProfileCommand::Avatar(crate::cli::ProfileAvatarArgs {
                    command: crate::cli::ProfileAvatarCommand::Show(crate::cli::ProfileShowArgs {
                        participant: Some(_)
                    })
                }))
        ),
        _ => false,
    }
}

/// The participant commands that change the participant registry without
/// being a participant's own lifecycle: `gc --apply` collects records, and
/// `restore` brings one back. Both take the migration fence like any writer.
fn participant_registry_write(command: &Command) -> bool {
    matches!(
        command,
        Command::Participant(crate::cli::ParticipantArgs {
            command: crate::cli::ParticipantCommand::Gc(crate::cli::ParticipantGcArgs {
                apply: true
            }) | crate::cli::ParticipantCommand::Restore(_),
        })
    )
}

/// The status line a text-mode run leaves on stderr when nothing answers to
/// its identity. A claim that names no record is `missing`, not `unbound`, and
/// its fix is the error's own (`unset POST_PARTICIPANT && post participant
/// bind`, not a bare `bind`, which the stale claim would keep winning over).
fn unbound_stderr_line(resolution_error: Option<&AppError>) -> String {
    match resolution_error.filter(|error| error.code == ErrorCode::ParticipantMissing) {
        Some(error) => format!(
            "participant: missing (run: {})",
            error
                .details
                .exact_fix
                .as_deref()
                .unwrap_or(&error.suggested_fix)
        ),
        None => "participant: unbound (run: post participant bind)".to_owned(),
    }
}

/// The one line an unbound session is told, and the fix that suits it: a
/// harness session binds itself; a bare shell mints an identity.
pub(super) fn unbound_hint() -> &'static str {
    if crate::participant::bind_key_available().unwrap_or(false) {
        "This session is not bound to a post participant yet, so nothing can be addressed to it. Send a message, or run `post participant bind`, to bind it. To look at one room without binding, `post inbox --room <name>` and `post watch --snapshot --room <name>` take the room explicitly."
    } else {
        "This session is not bound to a post participant yet, so nothing can be addressed to it. Run `post participant bind --new`, then run the printed `export POST_PARTICIPANT=...` command, to bind it. To look at one room without binding, `post inbox --room <name>` and `post watch --snapshot --room <name>` take the room explicitly."
    }
}

#[derive(Serialize)]
struct UnboundMarker {
    ok: bool,
    participant: Option<&'static str>,
    bound: bool,
    hint: &'static str,
}

/// `participant: null, bound: false, hint`: the explicit stdout answer of a
/// reader that has no participant to read for.
fn unbound_marker_json(pretty: bool) -> AppResult<String> {
    crate::output::json(
        &UnboundMarker {
            ok: true,
            participant: None,
            bound: false,
            hint: unbound_hint(),
        },
        pretty,
    )
}

/// The marker for a reader whose body would guess a room from the working
/// directory. `inbox` and `watch --snapshot` answer for themselves (their
/// output has a shape of its own, and an explicit `--room` still runs).
fn unbound_reader_marker(
    context: &Context,
    command: &Command,
    json: bool,
) -> AppResult<Option<CommandResult>> {
    let reader = match command {
        Command::Chat(_) => !migration_fence::classify_write(command),
        Command::Search(_) => true,
        Command::Read(_) => !participant_required(command),
        // `profile` and `profile show` with no target read the acting
        // participant's profile; a named target needs no participant.
        // Compatibility, and the one exception: unbound inside a registered
        // room, they still show that room's legacy workspace entry, marked
        // `participant: "unbound"`, because Porch's launch check runs `post
        // profile show` in its owner room before it binds and requires that
        // shape. Remove once Porch checks only after binding.
        Command::Profile(args) => {
            matches!(
                &args.command,
                None | Some(crate::cli::ProfileCommand::Show(
                    crate::cli::ProfileShowArgs { participant: None }
                ))
            ) && !profile::cwd_in_registered_room(context)?
        }
        _ => false,
    };
    if !reader {
        return Ok(None);
    }
    let mut stdout = if json {
        unbound_marker_json(false)?
    } else {
        format!("post: {}\n", unbound_hint())
    };
    // `--max-bytes` caps final stdout, marker included: fall back to the bare
    // marker (no hint), and refuse a cap too small even for that.
    let max_bytes = match command {
        Command::Chat(args) => args.max_bytes,
        Command::Read(args) => args.max_bytes,
        _ => None,
    };
    if let Some(max_bytes) = max_bytes.filter(|max| stdout.len() > *max) {
        stdout = if json {
            "{\"ok\":true,\"participant\":null,\"bound\":false}\n".to_owned()
        } else {
            "post: session not bound; run `post participant bind`\n".to_owned()
        };
        if stdout.len() > max_bytes {
            return Err(byte_budget::scaffold_too_large(max_bytes, stdout.len()));
        }
    }
    Ok(Some(CommandResult::success(stdout)))
}

/// What a lazy bind reports in a write's receipt.
struct BoundNow {
    id: String,
    workspace: Option<String>,
}

/// Commands that bind a session on its first write. Consuming reads and
/// writes do; readers get the marker; lifecycle, identity, profile and
/// delivery commands, and long watches, keep the bind-command error.
fn lazy_mint_key(command: &Command) -> bool {
    match command {
        Command::Send(_) | Command::Catchup(_) => true,
        Command::Read(_) | Command::Chat(_) => participant_required(command),
        Command::Inbox(args) => args.adopt,
        _ => false,
    }
}

/// `"bound_now": {"id", "workspace"}` in a JSON receipt, or one line at the end
/// of a text one. Output that is neither a single JSON object nor plain text
/// (a stream of JSON lines) is left alone.
fn annotate_bound_now(result: &mut CommandResult, bound_now: &BoundNow) {
    let trimmed = result.stdout.trim_start();
    let looks_json = trimmed.starts_with('{') || trimmed.starts_with('[');
    if looks_json {
        let Ok(serde_json::Value::Object(object)) =
            serde_json::from_str::<serde_json::Value>(&result.stdout)
        else {
            return;
        };
        let member = serde_json::json!({ "id": bound_now.id, "workspace": bound_now.workspace });
        let opening = result
            .stdout
            .find('{')
            .expect("a parsed object has an opening brace");
        let separator = if object.is_empty() { "" } else { "," };
        result
            .stdout
            .insert_str(opening + 1, &format!("\"bound_now\":{member}{separator}"));
        return;
    }
    let mut line = format!("post: bound this session as participant {}", bound_now.id);
    if let Some(workspace) = bound_now.workspace.as_deref() {
        line.push_str(&format!(" (workspace {workspace})"));
    }
    if !result.stdout.is_empty() && !result.stdout.ends_with('\n') {
        result.stdout.push('\n');
    }
    result.stdout.push_str(&line);
    result.stdout.push('\n');
}

fn unbound_json_listing(command: &Command) -> bool {
    use crate::cli::{ParticipantCommand, ProfileCommand, RoomsCommand};
    match command {
        Command::Rooms(args) => !matches!(
            args.command,
            Some(RoomsCommand::Add(_) | RoomsCommand::SetPath(_))
        ),
        Command::Channels(_)
        | Command::Inbox(_)
        | Command::Doctor(_)
        | Command::Schema
        | Command::Who(_) => true,
        Command::Participant(crate::cli::ParticipantArgs {
            command: ParticipantCommand::List,
        }) => true,
        Command::Profile(args) => matches!(
            &args.command,
            Some(ProfileCommand::Show(show)) if show.participant.is_some()
        ),
        // `owner show` reports the host's owner record, which does not depend
        // on who is asking; Porch's launch check also refuses any key it does
        // not know in it, so it carries no unbound marker.
        _ => false,
    }
}

fn explicit_participant_bootstrap(command: &Command) -> bool {
    matches!(
        command,
        Command::Participant(crate::cli::ParticipantArgs {
            command: crate::cli::ParticipantCommand::Bind(args),
        }) if args.fresh || args.key.is_some()
    )
}

fn plain_participant_bind(command: &Command) -> bool {
    matches!(
        command,
        Command::Participant(crate::cli::ParticipantArgs {
            command: crate::cli::ParticipantCommand::Bind(args),
        }) if !args.fresh && args.key.is_none()
    )
}

fn participant_command_manages_activity(command: &Command) -> bool {
    matches!(command, Command::Participant(_))
}

fn participant_required(command: &Command) -> bool {
    use crate::cli::{IdentityCommand, ProfileCommand};
    match command {
        Command::Participant(crate::cli::ParticipantArgs {
            command:
                crate::cli::ParticipantCommand::Touch
                | crate::cli::ParticipantCommand::Describe(_)
                | crate::cli::ParticipantCommand::End
                | crate::cli::ParticipantCommand::Notice { .. },
        }) => true,
        Command::Send(_) | Command::Catchup(_) | Command::Delivery(_) => true,
        Command::Read(args) => {
            args.ack || (!args.peek && args.offset.is_none() && args.length.is_none())
        }
        Command::Chat(_) => migration_fence::classify_write(command),
        Command::Watch(args) => !args.snapshot,
        Command::Inbox(args) => args.adopt,
        Command::Identity(args) => !matches!(
            &args.command,
            IdentityCommand::List | IdentityCommand::Show(_)
        ),
        Command::Profile(args) => matches!(
            &args.command,
            Some(
                ProfileCommand::Set(_)
                    | ProfileCommand::Clear
                    | ProfileCommand::Avatar(crate::cli::ProfileAvatarArgs {
                        command: crate::cli::ProfileAvatarCommand::Set { .. }
                            | crate::cli::ProfileAvatarCommand::Clear
                    })
            )
        ),
        _ => false,
    }
}

/// Add the unbound marker (`participant: null` unless the command reports its
/// own participant, `bound: false`, `hint`) to a JSON listing, or the
/// `participant_missing` field when a claim names no record.
fn annotate_unbound(
    result: &mut CommandResult,
    pretty: bool,
    resolution_error: Option<&AppError>,
) -> AppResult<()> {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&result.stdout) else {
        return Ok(());
    };
    let Some(object) = value.as_object_mut() else {
        return Ok(());
    };
    if object.contains_key("bound") {
        // The command answered for itself (unbound `inbox`).
        return Ok(());
    }
    let ok = object.remove("ok").unwrap_or(serde_json::Value::Bool(true));
    // A command that reports its own participant object keeps it; the old
    // `"unbound"` string and an absent key both become `null`.
    let own_participant = object
        .remove("participant")
        .filter(|value| value.is_object() || value.as_str().is_some_and(|id| id != "unbound"));
    let payload: std::collections::BTreeMap<String, serde_json::Value> =
        std::mem::take(object).into_iter().collect();
    #[derive(serde::Serialize)]
    struct Annotated {
        ok: serde_json::Value,
        #[serde(flatten)]
        payload: std::collections::BTreeMap<String, serde_json::Value>,
        participant: serde_json::Value,
        bound: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        hint: Option<&'static str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        participant_missing: Option<participant::MissingReport>,
        #[serde(skip_serializing_if = "Option::is_none")]
        participant_error: Option<String>,
    }
    // A claim that names no record is `participant_missing`; any other reason
    // the claim could not be resolved (a malformed id, a corrupt record) keeps
    // its own message, as `participant_error`. Either replaces the hint: the
    // claim, not the absence of one, is what the reader has to fix.
    let (missing_report, resolution_message) = match resolution_error {
        Some(error) if error.code == ErrorCode::ParticipantMissing => {
            (Some(participant::MissingReport::from_error(error)), None)
        }
        Some(error) => (None, Some(error.message.clone())),
        None => (None, None),
    };
    let output = Annotated {
        ok,
        payload,
        participant: own_participant.unwrap_or(serde_json::Value::Null),
        bound: false,
        hint: resolution_error.is_none().then(unbound_hint),
        participant_missing: missing_report,
        participant_error: resolution_message,
    };
    result.stdout = crate::output::json(&output, pretty)?;
    Ok(())
}
