pub mod doctor;
pub mod inbox;
pub mod read;
pub mod rooms;
pub mod schema;
pub mod send;

use crate::cli::{Cli, Command};
pub use crate::command_result::CommandResult;
use crate::error::AppResult;
use crate::mailbox::Context;

pub fn execute(cli: Cli) -> AppResult<CommandResult> {
    let context = Context::from_env()?;
    if !matches!(&cli.command, Command::Doctor(_)) {
        context.prepare_first_run()?;
    }
    match cli.command {
        Command::Doctor(args) => doctor::run(&context, args, cli.pretty),
        Command::Send(args) => send::run(&context, args, cli.json, cli.pretty),
        Command::Inbox(args) => inbox::run(&context, args, cli.pretty),
        Command::Read(args) => read::run(&context, args, cli.json, cli.pretty),
        Command::Rooms => rooms::run(&context, cli.pretty),
        Command::Schema => schema::run(cli.pretty),
    }
}
