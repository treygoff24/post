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
    let writes = migration_fence::classify_write(&cli.command);
    let long_watch = matches!(&cli.command, Command::Watch(args) if !args.snapshot);
    let explicit_bootstrap = explicit_participant_bootstrap(&cli.command);
    let participant_is_required = participant_required(&cli.command);
    let resolution_required = participant_is_required || plain_participant_bind(&cli.command);
    let (resolved_participant, resolution_error) = if explicit_bootstrap {
        (crate::participant::Resolved::Unbound, None)
    } else {
        match crate::participant::resolve(&context) {
            Ok(resolved) => (resolved, None),
            Err(error) if resolution_required => return Err(error),
            Err(error) => (crate::participant::Resolved::Unbound, Some(error.message)),
        }
    };
    let mut admission = if writes {
        Some(migration_fence::admit(&context, true)?)
    } else {
        None
    };
    if participant_is_required && resolved_participant.participant().is_none() {
        return Err(AppError::no_participant(
            crate::participant::bind_key_available()?,
        ));
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
                command: crate::cli::ParticipantCommand::Show,
            })
        );
    let annotate_unbound_json = report_unbound && unbound_json_listing(&cli.command);
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
    if report_unbound {
        eprintln!("participant: unbound (run: post participant bind)");
        if let Some(error) = resolution_error.as_deref() {
            eprintln!("participant resolution error: {error}");
        }
        if annotate_unbound_json {
            annotate_unbound(&mut result, pretty, resolution_error.as_deref())?;
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

fn unbound_json_listing(command: &Command) -> bool {
    use crate::cli::{OwnerCommand, ParticipantCommand, ProfileCommand, RoomsCommand};
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
        Command::Profile(args) => matches!(args.command, None | Some(ProfileCommand::Show(_))),
        Command::Owner(args) => matches!(args.command, None | Some(OwnerCommand::Show)),
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
            Some(ProfileCommand::Set(_) | ProfileCommand::Clear)
        ),
        _ => false,
    }
}

fn annotate_unbound(
    result: &mut CommandResult,
    pretty: bool,
    resolution_error: Option<&str>,
) -> AppResult<()> {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&result.stdout) else {
        return Ok(());
    };
    let Some(object) = value.as_object_mut() else {
        return Ok(());
    };
    let ok = object.remove("ok").unwrap_or(serde_json::Value::Bool(true));
    let participant_present = object.contains_key("participant");
    let payload: std::collections::BTreeMap<String, serde_json::Value> =
        std::mem::take(object).into_iter().collect();
    #[derive(serde::Serialize)]
    struct AnnotatedUnbound {
        ok: serde_json::Value,
        #[serde(flatten)]
        payload: std::collections::BTreeMap<String, serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        participant: Option<&'static str>,
        participant_fix: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        participant_error: Option<String>,
    }
    let output = AnnotatedUnbound {
        ok,
        payload,
        participant: (!participant_present).then_some("unbound"),
        participant_fix: "run: post participant bind",
        participant_error: resolution_error.map(str::to_owned),
    };
    result.stdout = crate::output::json(&output, pretty)?;
    Ok(())
}
