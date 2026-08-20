mod channels;
mod chat;
mod doctor;
mod inbox;
mod owner;
mod profile;
mod read;
mod rooms;
mod schema;
mod send;
mod watch;
mod who;

use crate::cli::{Cli, Command};
use crate::command_result::CommandResult;
use crate::error::AppResult;
use crate::mailbox::Context;
use crate::migration_fence;

pub(crate) fn execute(cli: Cli) -> AppResult<CommandResult> {
    let context = Context::from_env()?;
    let writes = migration_fence::classify_write(&cli.command);
    let long_watch = matches!(&cli.command, Command::Watch(args) if !args.snapshot);
    let mut admission = if writes {
        Some(migration_fence::admit(&context, true)?)
    } else {
        None
    };
    let enrolled_watch = long_watch
        && admission
            .as_ref()
            .is_some_and(migration_fence::WriteAdmission::is_enrolled);
    let fenced_read =
        enrolled_watch || (!writes && migration_fence::read_only_must_not_mutate(&context));
    let _read_only = crate::mailbox::enter_read_only_command(fenced_read);
    if !fenced_read && !matches!(&cli.command, Command::Doctor(_)) {
        context.prepare_first_run()?;
    }
    // Startup admission only proves that the watch may enter its setup phase;
    // heartbeat admissions must be able to take the lock independently.
    if long_watch {
        drop(admission.take());
    }
    let pretty = cli.pretty;
    let json = cli.json;
    let mut result = match cli.command {
        Command::Doctor(args) => doctor::run(&context, args, pretty),
        Command::Send(args) => send::run(&context, args, json, pretty),
        Command::Chat(args) => chat::run(&context, args, json, pretty),
        Command::Channels(args) => channels::run(&context, args, pretty),
        Command::Inbox(args) => inbox::run(&context, args, pretty),
        Command::Read(args) => read::run(&context, args, json, pretty),
        Command::Rooms(args) => rooms::run(&context, args, pretty),
        Command::Profile(args) => profile::run(&context, args, pretty),
        Command::Owner(args) => owner::run(&context, args, pretty),
        Command::Schema => schema::run(&context, pretty),
        Command::Watch(args) => watch::run(&context, args),
        Command::Who(args) => who::run(&context, args, pretty),
    }?;
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
