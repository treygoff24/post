pub mod doctor;
pub mod inbox;
pub mod read;
pub mod rooms;
pub mod schema;
pub mod send;

use crate::cli::{Cli, Command};
use crate::error::AppResult;
use crate::Context;

pub struct CommandResult {
    pub stdout: String,
    pub exit_code: i32,
}

impl CommandResult {
    pub fn success(stdout: String) -> Self {
        Self {
            stdout,
            exit_code: 0,
        }
    }
}

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
