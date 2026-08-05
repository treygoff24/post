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
#[command(
    override_usage = "post send --to <ROOM> [OPTIONS] --body <TEXT>\n       \
     post send --to <ROOM> [OPTIONS] --body-file <PATH>\n       \
     post send --to <ROOM> [OPTIONS] < BODY_FILE\n\n\
     The three body forms are alternatives: pass exactly one, or none to read stdin."
)]
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

    /// Inline message body text.
    #[arg(long, value_name = "TEXT", conflicts_with_all = ["body_file", "file"])]
    pub body: Option<String>,

    /// Read the message body from this UTF-8 file.
    #[arg(
        long = "body-file",
        value_name = "PATH",
        value_hint = clap::ValueHint::FilePath,
        conflicts_with = "file"
    )]
    pub body_file: Option<PathBuf>,

    /// Deprecated positional spelling of --body-file; omit every body source to read stdin.
    #[arg(value_name = "FILE", value_hint = clap::ValueHint::FilePath)]
    pub file: Option<PathBuf>,
}

#[derive(Debug, Args)]
#[command(
    override_usage = "post chat <CHANNEL>                             (read new messages)\n       \
     post chat <CHANNEL> --peek                      (read without advancing)\n       \
     post chat <CHANNEL> --history <N>               (last N messages, cursor untouched)\n       \
     post chat <CHANNEL> --since <ID>                (messages after ID, cursor untouched)\n       \
     post chat <CHANNEL> --discard                   (advance past unread without printing)\n       \
     post chat <CHANNEL> --join                      (join, creating on first join)\n       \
     post chat <CHANNEL> --send --body <TEXT>        (send inline text)\n       \
     post chat <CHANNEL> --send --body-file <PATH>   (send a file's contents)\n       \
     post chat <CHANNEL> --send < BODY_FILE          (send stdin)\n\n\
     These forms are alternatives; pass exactly one. --body/--body-file imply --send."
)]
pub(crate) struct ChatArgs {
    /// Channel name.
    #[arg(value_name = "CHANNEL", value_parser = nonempty_without_controls)]
    pub name: String,

    /// Join the channel (creates it on first join); recorded in history.
    #[arg(long, conflicts_with_all = ["send", "peek", "discard", "body", "body_file", "file", "subject"])]
    pub join: bool,

    /// Send a message; the body comes from --body, --body-file, or stdin.
    #[arg(long, conflicts_with_all = ["peek", "discard"])]
    pub send: bool,

    /// Optional subject; only meaningful when sending.
    #[arg(long, default_value = "", value_parser = without_controls)]
    pub subject: String,

    /// Inline message body text; implies --send.
    #[arg(long, value_name = "TEXT", conflicts_with_all = ["body_file", "file", "peek", "discard"])]
    pub body: Option<String>,

    /// Read the message body from this UTF-8 file; implies --send.
    #[arg(
        long = "body-file",
        value_name = "PATH",
        value_hint = clap::ValueHint::FilePath,
        conflicts_with_all = ["file", "peek", "discard"]
    )]
    pub body_file: Option<PathBuf>,

    /// Deprecated positional spelling of --body-file; requires --send.
    #[arg(
        value_name = "FILE",
        value_hint = clap::ValueHint::FilePath,
        requires = "send"
    )]
    pub file: Option<PathBuf>,

    /// Read without advancing the cursor.
    #[arg(long)]
    pub peek: bool,

    /// Show the last N messages regardless of read state; never advances the cursor.
    #[arg(long, value_name = "N", conflicts_with_all = ["send", "join", "discard", "body", "body_file", "file"])]
    pub history: Option<usize>,

    /// Only messages with id strictly after this id (ignores the cursor); never advances the cursor.
    #[arg(long, value_name = "ID", conflicts_with_all = ["send", "join", "discard", "body", "body_file", "file"], value_parser = nonempty_without_controls)]
    pub since: Option<String>,

    /// Advance the cursor past every unread message without printing them.
    #[arg(long, conflicts_with_all = ["peek", "send", "join"])]
    pub discard: bool,
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
    /// Mailbox room; repeat to merge rooms, or omit for cwd resolution.
    #[arg(long, value_name = "ROOM", value_parser = NonEmptyStringValueParser::new())]
    pub room: Vec<String>,

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
