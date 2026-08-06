mod channels;
mod chat;
mod doctor;
mod inbox;
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

pub(crate) fn execute(cli: Cli) -> AppResult<CommandResult> {
    let context = Context::from_env()?;
    if !matches!(&cli.command, Command::Doctor(_)) {
        context.prepare_first_run()?;
    }
    match cli.command {
        Command::Doctor(args) => doctor::run(&context, args, cli.pretty),
        Command::Send(args) => send::run(&context, args, cli.json, cli.pretty),
        Command::Chat(args) => chat::run(&context, args, cli.json, cli.pretty),
        Command::Channels(args) => channels::run(&context, args, cli.pretty),
        Command::Inbox(args) => inbox::run(&context, args, cli.pretty),
        Command::Read(args) => read::run(&context, args, cli.json, cli.pretty),
        Command::Rooms(args) => rooms::run(&context, args, cli.pretty),
        Command::Profile(args) => profile::run(&context, args, cli.pretty),
        Command::Schema => schema::run(cli.pretty),
        Command::Watch(args) => watch::run(&context, args),
        Command::Who(args) => who::run(&context, args, cli.pretty),
    }
}
