use clap::builder::NonEmptyStringValueParser;
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
pub struct Cli {
    /// Emit JSON for send/read (inbox/rooms/schema/doctor are already JSON).
    #[arg(long, global = true)]
    pub json: bool,

    /// Pretty-print JSON output.
    #[arg(long, global = true)]
    pub pretty: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Send mail from --body, FILE, or stdin.
    Send(SendArgs),
    /// List unread mail, oldest first.
    Inbox(InboxArgs),
    /// Read one unread message by full id or unique prefix.
    Read(ReadArgs),
    /// List registered rooms and applicable blocking rules.
    Rooms,
    /// Print the complete machine-readable CLI contract.
    Schema,
    /// Diagnose mailbox configuration and state.
    Doctor(DoctorArgs),
}

#[derive(Debug, Args)]
pub struct SendArgs {
    /// Registered recipient room.
    #[arg(long, value_name = "ROOM", value_parser = NonEmptyStringValueParser::new())]
    pub to: String,

    /// Explicit sender identity; registered room names are reserved.
    #[arg(long = "from", value_name = "NAME", value_parser = NonEmptyStringValueParser::new())]
    pub sender: Option<String>,

    /// Message register.
    #[arg(long, value_enum, default_value_t = MailKind::Note)]
    pub kind: MailKind,

    /// Optional subject.
    #[arg(long, default_value = "")]
    pub subject: String,

    /// Inline message body.
    #[arg(long, value_name = "TEXT", conflicts_with = "file")]
    pub body: Option<String>,

    /// Read the message body from this UTF-8 file; omit for stdin.
    #[arg(value_name = "FILE", value_hint = clap::ValueHint::FilePath)]
    pub file: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct InboxArgs {
    /// Mailbox room; defaults to the room containing cwd or cwd basename.
    #[arg(long, value_name = "ROOM", value_parser = NonEmptyStringValueParser::new())]
    pub room: Option<String>,

    /// Emit human-readable text instead of the default JSON.
    #[arg(long, conflicts_with = "json")]
    pub text: bool,
}

#[derive(Debug, Args)]
pub struct ReadArgs {
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
pub struct DoctorArgs {
    /// Create missing directories and default config files only.
    #[arg(long)]
    pub fix: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MailKind {
    Letter,
    Note,
    Signal,
}

impl MailKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Letter => "letter",
            Self::Note => "note",
            Self::Signal => "signal",
        }
    }
}

impl std::fmt::Display for MailKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
