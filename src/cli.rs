use crate::model::MailKind;
use clap::builder::NonEmptyStringValueParser;
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

fn nonempty_without_controls(value: &str) -> Result<String, String> {
    if value.is_empty() {
        return Err("value must not be empty".to_owned());
    }
    without_controls(value)
}

fn without_controls(value: &str) -> Result<String, String> {
    if value.chars().any(char::is_control) {
        return Err("value must not contain control characters".to_owned());
    }
    Ok(value.to_owned())
}

#[derive(Debug, Parser)]
#[command(
    name = "post",
    version,
    about = "Machine-local mailbox for AI agents",
    long_about = None,
    arg_required_else_help = true,
    subcommand_required = true,
    color = clap::ColorChoice::Never,
    rename_all = "kebab-case"
)]
pub(crate) struct Cli {
    /// Emit JSON for send/read/chat; inbox/rooms/channels/schema/doctor are already JSON.
    #[arg(long, global = true)]
    pub json: bool,

    /// Pretty-print JSON output.
    #[arg(long, global = true)]
    pub pretty: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Send mail from --body, FILE, or stdin.
    Send(SendArgs),
    /// Join, send to, or read a shared channel (group chat).
    Chat(ChatArgs),
    /// List channels with their members.
    Channels,
    /// List unread mail, oldest first.
    Inbox(InboxArgs),
    /// Read one unread message by full id or unique prefix.
    Read(ReadArgs),
    /// List or register rooms.
    Rooms(RoomsArgs),
    /// Print the complete machine-readable CLI contract.
    Schema,
    /// Diagnose mailbox configuration and state.
    Doctor(DoctorArgs),
    /// Stream direct-mail and joined-channel notifications as one event per line; runs until killed (--snapshot scans once and exits).
    Watch(WatchArgs),
}

#[derive(Debug, Args)]
pub(crate) struct SendArgs {
    /// Registered recipient room.
    #[arg(long, value_name = "ROOM", value_parser = NonEmptyStringValueParser::new())]
    pub to: String,

    /// Explicit sender identity; registered room names are reserved.
    #[arg(long = "from", value_name = "NAME", value_parser = nonempty_without_controls)]
    pub sender: Option<String>,

    /// Message register.
    #[arg(long, value_enum, default_value_t = MailKind::Note)]
    pub kind: MailKind,

    /// Optional subject.
    #[arg(long, default_value = "", value_parser = without_controls)]
    pub subject: String,

    /// Inline message body.
    #[arg(long, value_name = "TEXT", conflicts_with = "file")]
    pub body: Option<String>,

    /// Read the message body from this UTF-8 file; omit for stdin.
    #[arg(value_name = "FILE", value_hint = clap::ValueHint::FilePath)]
    pub file: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub(crate) struct ChatArgs {
    /// Channel name.
    #[arg(value_name = "CHANNEL", value_parser = nonempty_without_controls)]
    pub name: String,

    /// Join the channel (creates it on first join); recorded in history.
    #[arg(long, conflicts_with_all = ["send", "peek", "body", "file", "subject"])]
    pub join: bool,

    /// Send a message from --body, FILE, or stdin.
    #[arg(long, conflicts_with = "peek")]
    pub send: bool,

    /// Optional subject for --send.
    #[arg(long, default_value = "", value_parser = without_controls, requires = "send")]
    pub subject: String,

    /// Inline message body for --send.
    #[arg(long, value_name = "TEXT", conflicts_with = "file", requires = "send")]
    pub body: Option<String>,

    /// Read the message body for --send from this UTF-8 file; omit for stdin.
    #[arg(value_name = "FILE", value_hint = clap::ValueHint::FilePath, requires = "send")]
    pub file: Option<PathBuf>,

    /// Read without advancing the cursor.
    #[arg(long)]
    pub peek: bool,
}

#[derive(Debug, Args)]
pub(crate) struct InboxArgs {
    /// Mailbox room; defaults to the room containing cwd or cwd basename.
    #[arg(long, value_name = "ROOM", value_parser = NonEmptyStringValueParser::new())]
    pub room: Option<String>,

    /// Emit human-readable text instead of the default JSON.
    #[arg(long, conflicts_with = "json")]
    pub text: bool,
}

#[derive(Debug, Args)]
pub(crate) struct RoomsArgs {
    #[command(subcommand)]
    pub command: Option<RoomsCommand>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum RoomsCommand {
    /// Register an existing workspace directory as a room.
    Add(RoomsAddArgs),
}

#[derive(Debug, Args)]
pub(crate) struct RoomsAddArgs {
    /// Path-safe room name to reserve and receive mail under.
    #[arg(value_name = "NAME", value_parser = nonempty_without_controls)]
    pub name: String,

    /// Existing workspace directory; absolute or starting with ~/.
    #[arg(
        value_name = "PATH",
        value_hint = clap::ValueHint::DirPath,
        value_parser = nonempty_without_controls
    )]
    pub path: String,
}

#[derive(Debug, Args)]
pub(crate) struct ReadArgs {
    /// Full message id or a unique prefix.
    #[arg(value_name = "ID_OR_PREFIX", value_parser = NonEmptyStringValueParser::new())]
    pub id: String,

    /// Mailbox room; defaults to the room containing cwd or cwd basename.
    #[arg(long, value_name = "ROOM", value_parser = NonEmptyStringValueParser::new())]
    pub room: Option<String>,

    /// Read without moving the message to read/.
    #[arg(long)]
    pub peek: bool,
}

#[derive(Debug, Args)]
pub(crate) struct WatchArgs {
    /// Mailbox room; defaults to the room containing cwd or cwd basename.
    #[arg(long, value_name = "ROOM", value_parser = NonEmptyStringValueParser::new())]
    pub room: Option<String>,

    /// Exit 0 after the first batch that emits at least one event.
    #[arg(long)]
    pub once: bool,

    /// Scan exactly once and exit 0: a nonblocking poll for lifecycle hooks. An
    /// empty scan emits nothing; --interval-ms has no effect.
    #[arg(long, conflicts_with = "once")]
    pub snapshot: bool,

    /// Poll cadence in milliseconds.
    #[arg(long, value_name = "MS", default_value_t = 1000,
          value_parser = clap::value_parser!(u64).range(100..=60_000))]
    pub interval_ms: u64,

    /// Emit human-readable lines instead of the default NDJSON events.
    #[arg(long, conflicts_with = "json")]
    pub text: bool,
}

#[derive(Debug, Args)]
pub(crate) struct DoctorArgs {
    /// Create missing directories and default config files only.
    #[arg(long)]
    pub fix: bool,
}
