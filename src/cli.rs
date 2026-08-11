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
    /// Emit JSON for send/read/chat; inbox/rooms/channels/profile/schema/doctor are already JSON.
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
    Channels(ChannelsArgs),
    /// List unread mail, oldest first.
    Inbox(InboxArgs),
    /// Read one unread message by full id or unique prefix.
    Read(ReadArgs),
    /// List or register rooms.
    Rooms(RoomsArgs),
    /// Show or change this room's display name and emoji pfp (presentation only; identity stays the room id).
    Profile(ProfileArgs),
    /// Print the complete machine-readable CLI contract.
    Schema,
    /// Diagnose mailbox configuration and state.
    Doctor(DoctorArgs),
    /// Stream direct-mail and joined-channel notifications as one event per line; runs until killed (--snapshot scans once and exits).
    Watch(WatchArgs),
    /// Report which rooms have a live watch and when they were last seen (no PIDs).
    Who(WhoArgs),
}

#[derive(Debug, Args)]
#[command(
    override_usage = "post send --to <ROOM> [OPTIONS] [--oversize] --body <TEXT>\n       \
     post send --to <ROOM> [OPTIONS] [--oversize] --body-file <PATH>\n       \
     post send --to <ROOM> [OPTIONS] [--oversize] < BODY_FILE\n\n\
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

    /// Optional subject, limited to 1 KiB.
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

    /// Allow a body larger than the default 32 KiB safety limit.
    #[arg(long)]
    pub oversize: bool,

    /// Deprecated positional spelling of --body-file; omit every body source to read stdin.
    #[arg(value_name = "FILE", value_hint = clap::ValueHint::FilePath)]
    pub file: Option<PathBuf>,
}

#[derive(Debug, Args)]
#[command(
    override_usage = "post chat <CHANNEL> [--framing auto|full|compact] (read new messages; default last 25 unread)\n       \
     post chat <CHANNEL> --peek [--framing MODE]     (read without advancing)\n       \
     post chat <CHANNEL> --limit <N> [--framing MODE] (last N unread; --limit 0 = all)\n       \
     post chat <CHANNEL> --history <N> [--grep PAT] [--framing MODE] (last N messages, cursor untouched)\n       \
     post chat <CHANNEL> --since <ID> [--framing MODE] (messages after ID, cursor untouched)\n       \
     post chat <CHANNEL> --discard                   (advance past unread without printing)\n       \
     post chat <CHANNEL> --seen-by <MSG_ID>          (which members' cursors passed MSG_ID)\n       \
     post chat <CHANNEL> --join [--description TEXT] (join, creating on first join)\n       \
     post chat <CHANNEL> --send [--anyway] [--re ID] [--oversize] --body <TEXT>\n       \
     post chat <CHANNEL> --send [--anyway] [--re ID] [--oversize] --body-file <PATH>\n       \
     post chat <CHANNEL> --send [--anyway] [--re ID] [--oversize] < BODY_FILE\n\n\
     These forms are alternatives; pass exactly one. --body/--body-file imply --send."
)]
pub(crate) struct ChatArgs {
    /// Channel name.
    #[arg(value_name = "CHANNEL", value_parser = nonempty_without_controls)]
    pub name: String,

    /// Join the channel (creates it on first join); recorded in history.
    #[arg(long, conflicts_with_all = ["send", "peek", "discard", "body", "body_file", "file", "subject", "seen_by", "history", "since", "limit", "grep", "re", "anyway"])]
    pub join: bool,

    /// Set or update the channel description (norms carrier); with --join.
    /// Cap 1 KiB. Any member may update.
    #[arg(long, value_name = "TEXT", value_parser = without_controls, requires = "join")]
    pub description: Option<String>,

    /// Send a message; the body comes from --body, --body-file, or stdin.
    #[arg(long, conflicts_with_all = ["peek", "discard", "seen_by"])]
    pub send: bool,

    /// Deliver even when unread messages sit past the sender's cursor.
    #[arg(long, conflicts_with_all = ["join", "peek", "discard", "seen_by", "history", "since", "limit", "grep"])]
    pub anyway: bool,

    /// Reply to a prior message id (or unique prefix) in this channel.
    #[arg(long = "re", value_name = "MSG_ID", value_parser = nonempty_without_controls, conflicts_with_all = ["join", "peek", "discard", "seen_by", "history", "since", "limit", "grep"])]
    pub re: Option<String>,

    /// Optional subject, limited to 1 KiB; only meaningful when sending.
    #[arg(long, default_value = "", value_parser = without_controls)]
    pub subject: String,

    /// Inline message body text; implies --send.
    #[arg(long, value_name = "TEXT", conflicts_with_all = ["body_file", "file", "peek", "discard", "seen_by"])]
    pub body: Option<String>,

    /// Read the message body from this UTF-8 file; implies --send.
    #[arg(
        long = "body-file",
        value_name = "PATH",
        value_hint = clap::ValueHint::FilePath,
        conflicts_with_all = ["file", "peek", "discard", "seen_by"]
    )]
    pub body_file: Option<PathBuf>,

    /// Allow a body larger than the default 32 KiB safety limit.
    #[arg(long, conflicts_with_all = ["join", "peek", "discard", "seen_by"])]
    pub oversize: bool,

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
    #[arg(long, value_name = "N", conflicts_with_all = ["send", "join", "discard", "body", "body_file", "file", "seen_by", "anyway", "re"])]
    pub history: Option<usize>,

    /// Filter --history by case-insensitive regex (requires --history).
    #[arg(long, value_name = "PATTERN", requires = "history", conflicts_with_all = ["send", "join", "discard", "body", "body_file", "file", "seen_by", "anyway", "re", "limit", "since"])]
    pub grep: Option<String>,

    /// Only messages with id strictly after this id (ignores the cursor); never advances the cursor.
    #[arg(long, value_name = "ID", conflicts_with_all = ["send", "join", "discard", "body", "body_file", "file", "seen_by", "anyway", "re", "grep"], value_parser = nonempty_without_controls)]
    pub since: Option<String>,

    /// Advance the cursor past every unread message without printing them.
    #[arg(long, conflicts_with_all = ["peek", "send", "join", "seen_by"])]
    pub discard: bool,

    /// Bounded catch-up: show only the last N unread (default 25 when omitted).
    /// `--limit 0` means unlimited. Mentions of the reading room in the
    /// skipped range are never silently dropped.
    #[arg(long, value_name = "N", conflicts_with_all = ["send", "join", "discard", "history", "since", "body", "body_file", "file", "seen_by", "anyway", "re", "grep"])]
    pub limit: Option<usize>,

    /// List member rooms whose cursors have advanced past this message (read-only).
    #[arg(long = "seen-by", value_name = "MSG_ID", value_parser = nonempty_without_controls, conflicts_with_all = ["send", "join", "peek", "discard", "body", "body_file", "file", "history", "since", "limit", "anyway", "re", "grep", "subject", "oversize"])]
    pub seen_by: Option<String>,

    /// Banner form for body-returning reads: auto (default, once-daily full
    /// wall), full (every invocation), or compact (one-line reminder).
    /// Rejected on send/join/discard/seen-by, which return no bodies and
    /// must not look like they honored it.
    #[arg(long, value_enum, default_value_t = FramingMode::Auto, conflicts_with_all = ["send", "join", "discard", "seen_by", "body", "body_file", "file"])]
    pub framing: FramingMode,
}

#[derive(Debug, Args)]
pub(crate) struct ChannelsArgs {
    /// Emit human-readable text instead of the default JSON.
    #[arg(long, conflicts_with = "json")]
    pub text: bool,
}

#[derive(Debug, Args)]
pub(crate) struct WhoArgs {
    /// Restrict to these rooms; omit for every registered room.
    #[arg(long, value_name = "ROOM", value_parser = NonEmptyStringValueParser::new())]
    pub room: Vec<String>,

    /// Emit human-readable text instead of the default JSON.
    #[arg(long, conflicts_with = "json")]
    pub text: bool,
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
pub(crate) struct ProfileArgs {
    #[command(subcommand)]
    pub command: Option<ProfileCommand>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ProfileCommand {
    /// Set your display name and/or pfp; announces the change in your channels.
    Set(ProfileSetArgs),
    /// Show a room's profile (defaults to your own room).
    Show(ProfileShowArgs),
    /// Remove your profile; rendering falls back to the bare room id.
    Clear,
}

#[derive(Debug, Args)]
pub(crate) struct ProfileSetArgs {
    /// Display name, <=32 chars; may not imitate 'trey' or another room id.
    #[arg(long, value_name = "NAME", value_parser = nonempty_without_controls)]
    pub name: Option<String>,

    /// Exactly one emoji (one grapheme cluster), unique across rooms.
    #[arg(long, value_name = "EMOJI", value_parser = nonempty_without_controls)]
    pub pfp: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct ProfileShowArgs {
    /// Room to show; defaults to the room resolved from cwd.
    #[arg(value_name = "ROOM", value_parser = NonEmptyStringValueParser::new())]
    pub room: Option<String>,
}

/// How much framing a body-returning read prints. The laws bind in every
/// mode; compact is for sessions that have already internalized the full
/// text. Explicit modes (full, compact) are stateless — post never infers
/// that a reader remembers — and never touch the banner-day state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub(crate) enum FramingMode {
    /// Legacy presentation (default; byte-compatible): full laws everywhere
    /// except text chat, which shows the full wall once per room per day and
    /// a one-line reminder for the rest of the day.
    #[default]
    Auto,
    /// Force the full multi-line trust-boundary banner on every invocation.
    Full,
    /// One-line reminder carrying the same laws in condensed form.
    Compact,
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

    /// Banner form: auto (default), full, or a one-line compact reminder.
    #[arg(long, value_enum, default_value_t = FramingMode::Auto)]
    pub framing: FramingMode,
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
