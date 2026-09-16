mod byte_budget;
mod catchup;
mod channels;
mod chat;
mod doctor;
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

pub(crate) fn execute(cli: Cli) -> AppResult<CommandResult> {
    let pretty = cli.pretty;
    let json = cli.json;
    if matches!(&cli.command, Command::Version) {
        return version::run(json, pretty);
    }
    let context = Context::from_env()?;
    let resolved_participant = crate::participant::resolve(&context)?;
    let writes = migration_fence::classify_write(&cli.command);
    let long_watch = matches!(&cli.command, Command::Watch(args) if !args.snapshot);
    let mut admission = if writes {
        Some(migration_fence::admit(&context, true)?)
    } else {
        None
    };
    if participant_required(&cli.command) && resolved_participant.participant().is_none() {
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
    // Startup admission only proves that the watch may enter its setup phase;
    // heartbeat admissions must be able to take the lock independently.
    if long_watch {
        drop(admission.take());
    }
    let report_unbound = resolved_participant.participant().is_none()
        && matches!(
            &cli.command,
            Command::Inbox(_)
                | Command::Channels(_)
                | Command::Doctor(_)
                | Command::Schema
                | Command::Rooms(_)
        );
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
    let mut result = match cli.command {
        Command::Participant(args) => participant::run(&context, args, json, pretty),
        Command::Identity(_) => Err(AppError::not_yet("P.3")),
        Command::Doctor(args) => doctor::run(&context, args, pretty),
        Command::Send(args) => send::run(&context, args, json, pretty),
        Command::Chat(args) => chat::run(&context, args, json, pretty),
        Command::Channels(args) => channels::run(&context, args, pretty),
        Command::Inbox(args) if args.adopt => Err(AppError::not_yet("P.2")),
        Command::Inbox(args) => inbox::run(&context, args, pretty),
        Command::Read(args) => read::run(&context, args, json, pretty),
        Command::Catchup(args) => catchup::run(&context, args, json, pretty),
        Command::Search(args) => search::run(&context, args, json, pretty),
        Command::Rooms(args) => rooms::run(&context, args, pretty),
        Command::Profile(args) => profile::run(&context, args, pretty),
        Command::Owner(args) => owner::run(&context, args, pretty),
        Command::Schema => schema::run(&context, pretty),
        Command::Watch(args) => watch::run(&context, args),
        Command::Who(args) => who::run(&context, args, pretty),
        Command::Version => unreachable!("version dispatches before mailbox context resolution"),
    }?;
    if report_unbound {
        annotate_unbound(&mut result, pretty)?;
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

fn participant_required(command: &Command) -> bool {
    use crate::cli::{IdentityCommand, ProfileCommand};
    match command {
        Command::Send(_) | Command::Catchup(_) => true,
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

fn annotate_unbound(result: &mut CommandResult, pretty: bool) -> AppResult<()> {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&result.stdout) else {
        result
            .stdout
            .insert_str(0, "participant: unbound (run: post participant bind)\n");
        return Ok(());
    };
    let Some(object) = value.as_object_mut() else {
        return Ok(());
    };
    object.insert(
        "participant".to_owned(),
        serde_json::Value::String("unbound".to_owned()),
    );
    object.insert(
        "participant_fix".to_owned(),
        serde_json::Value::String("run: post participant bind".to_owned()),
    );
    result.stdout = crate::output::json(&value, pretty)?;
    Ok(())
}
