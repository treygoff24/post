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

fn nonempty_search_pattern(value: &str) -> Result<String, String> {
    if value.is_empty() {
        return Err("value must not be empty".to_owned());
    }
    if value.chars().any(crate::mailbox::refused_profile_char) {
        return Err("value must not contain control or directional characters".to_owned());
    }
    Ok(value.to_owned())
}

fn without_controls(value: &str) -> Result<String, String> {
    if value.chars().any(char::is_control) {
        return Err("value must not contain control characters".to_owned());
    }
    Ok(value.to_owned())
}

fn search_limit(value: &str) -> Result<usize, String> {
    let limit = value
        .parse::<usize>()
        .map_err(|_| "limit must be an integer from 1 through 1000".to_owned())?;
    if (1..=1000).contains(&limit) {
        Ok(limit)
    } else {
        Err("limit must be an integer from 1 through 1000".to_owned())
    }
}

fn positive_bytes(value: &str) -> Result<usize, String> {
    value
        .parse::<usize>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| "byte count must be a positive integer".to_owned())
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
    /// Print JSON instead of text. Commands that print text by default (send, read, chat, catchup, search, version, delivery, profile) print one JSON object; the rest already print JSON, and the ones with a text flag print their human form only when it is given; watch always streams NDJSON.
    #[arg(long, global = true)]
    pub json: bool,

    /// Pretty-print JSON output.
    #[arg(long, global = true)]
    pub pretty: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
// Parsed once per short-lived CLI process; boxing only ChatArgs would spread
// indirection through every command classifier for no runtime leverage.
#[allow(clippy::large_enum_variant)]
pub(crate) enum Command {
    /// Show, bind, or list conversation participants.
    Participant(ParticipantArgs),
    /// Inspect or change optional persistent lineages.
    Identity(IdentityArgs),
    /// Send mail; write the body on stdin (heredoc) or with --body-file.
    Send(SendArgs),
    /// Join, send to, or read a shared channel (group chat).
    Chat(ChatArgs),
    /// List channels with their members.
    Channels(ChannelsArgs),
    /// List unread mail, oldest first.
    Inbox(InboxArgs),
    /// Read one unread message by full id or unique prefix.
    Read(ReadArgs),
    /// Consume unread mail/channel targets; --max-bytes admits a complete prefix.
    Catchup(CatchupArgs),
    /// Search party-visible mail and joined channels by literal substring.
    Search(SearchArgs),
    /// List or register rooms.
    Rooms(RoomsArgs),
    /// Show or change your participant's display name and emoji pfp (presentation only; identity stays the participant id).
    Profile(ProfileArgs),
    /// Configure or show the signed owner: the trust anchor whose messages carry verification badges.
    Owner(OwnerArgs),
    /// Print the complete machine-readable CLI contract.
    Schema,
    /// Diagnose mailbox configuration and state.
    Doctor(DoctorArgs),
    /// Stream direct-mail and joined-channel notifications as one event per line; runs until killed (--snapshot scans once and exits).
    Watch(WatchArgs),
    /// Report participants and their leases, plus each room's watch heartbeat and last seen (no PIDs).
    Who(WhoArgs),
    /// Print build, store, and capability information.
    Version,
    /// Emit the output contract compiled into this binary: normalized samples
    /// of the JSON consumers read.
    Contract(ContractArgs),
    /// Bridge-only entry points. Humans and agents never need these.
    #[command(hide = true)]
    Bridge(BridgeArgs),
    /// Show where a host-qualified letter you sent stands: queued, published, received, or rejected.
    Delivery(DeliveryArgs),
}

#[derive(Debug, Args)]
pub(crate) struct DeliveryArgs {
    /// The mail id `post send` printed for a participant:<id>@<host> letter.
    #[arg(value_name = "MAIL_ID", value_parser = NonEmptyStringValueParser::new())]
    pub id: String,
}

#[derive(Debug, Args)]
pub(crate) struct BridgeArgs {
    #[command(subcommand)]
    pub command: BridgeCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum BridgeCommand {
    /// Admit one relayed participant letter into its recipient's inbox and print the decision (post.bridge-deliver.v1).
    Deliver(BridgeDeliverArgs),
}

#[derive(Debug, Args)]
pub(crate) struct BridgeDeliverArgs {
    /// Recipient participant id on this host.
    #[arg(long, value_name = "ID")]
    pub participant: String,
    /// The peer host whose relay branch carried the letter.
    #[arg(long, value_name = "HOST")]
    pub source_host: String,
    /// The letter's mail id (its pmail file name without .mail).
    #[arg(long, value_name = "MAIL_ID")]
    pub mail_id: String,
    /// Lowercase hex sha256 of the letter bytes, as the bridge computed it.
    #[arg(long, value_name = "HEX")]
    pub sha256: String,
    /// A private regular file holding the letter's exact bytes.
    #[arg(long, value_name = "PATH")]
    pub file: std::path::PathBuf,
}

#[derive(Debug, Args)]
pub(crate) struct ContractArgs {
    #[command(subcommand)]
    pub command: ContractCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ContractCommand {
    /// Print the embedded samples as one JSON object, or write them as files into --dir.
    Samples {
        /// Write each sample as a file here (created if missing) instead of printing them.
        #[arg(long, value_name = "DIR")]
        dir: Option<std::path::PathBuf>,
    },
    /// Print the sha256 manifest of the skill bundle this binary was built with; --verify checks a served skill path against it.
    SkillManifest {
        /// A served skill directory (for example ~/.agents/skill-library/post) to check; exits 1 on drift.
        #[arg(long, value_name = "PATH")]
        verify: Option<std::path::PathBuf>,
    },
}

#[derive(Debug, Args)]
pub(crate) struct ParticipantArgs {
    #[command(subcommand)]
    pub command: ParticipantCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ParticipantCommand {
    /// Read the pending activation notice; --ack records successful delivery by a harness adapter.
    Notice {
        #[arg(long, conflicts_with_all = ["claim", "release"])]
        ack: bool,
        /// Reserve delivery for this live adapter PID; competing adapters report busy.
        #[arg(long, value_name = "PID", conflicts_with = "release", value_parser = clap::value_parser!(u32).range(1..=i32::MAX as i64))]
        claim: Option<u32>,
        /// Release this adapter PID's reservation after failed output.
        #[arg(long, value_name = "PID", value_parser = clap::value_parser!(u32).range(1..=i32::MAX as i64))]
        release: Option<u32>,
    },
    /// Show the acting participant, or unbound when none is bound. With --harness and --key, look that conversation up without minting.
    Show(ParticipantShowArgs),
    /// Bind this harness conversation, minting its deterministic participant only when absent.
    Bind(ParticipantBindArgs),
    /// Refresh the acting participant's activity lease.
    Touch,
    /// Explicitly end the acting participant session.
    End,
    /// List every participant record in this local store.
    List,
    /// Collect participant records that hold nothing (dry run unless --apply).
    Gc(ParticipantGcArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ParticipantShowArgs {
    /// Harness slug of the conversation to look up (with --key); never mints a record.
    #[arg(
        long,
        value_name = "SLUG",
        value_parser = nonempty_without_controls,
        requires = "key"
    )]
    pub harness: Option<String>,

    /// Conversation key to look up (with --harness): the record it maps to, or bound: false.
    #[arg(
        long,
        value_name = "CONVERSATION_KEY",
        value_parser = nonempty_without_controls,
        requires = "harness"
    )]
    pub key: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct ParticipantGcArgs {
    /// Act on the plan. Without it, report what would be deleted or archived and change nothing.
    #[arg(long)]
    pub apply: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ParticipantBindArgs {
    /// Pin workspace context to a registered room instead of inferring it from cwd.
    #[arg(long, value_name = "ROOM", value_parser = nonempty_without_controls)]
    pub workspace: Option<String>,

    /// Harness slug. Required with --key (the participant id is derived from harness and key); an optional label for --new (default: shell).
    #[arg(long, value_name = "SLUG", value_parser = nonempty_without_controls)]
    pub harness: Option<String>,

    /// Deterministic conversation key for a shell without harness-provided identity. Requires --harness.
    #[arg(
        long,
        value_name = "CONVERSATION_KEY",
        value_parser = nonempty_without_controls,
        requires = "harness",
        conflicts_with = "fresh"
    )]
    pub key: Option<String>,

    /// Mint from a fresh UUID conversation key (default harness: shell).
    #[arg(long = "new", conflicts_with = "key")]
    pub fresh: bool,
}

#[derive(Debug, Args)]
pub(crate) struct IdentityArgs {
    #[command(subcommand)]
    pub command: IdentityCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum IdentityCommand {
    /// List lineages without loading voice bodies.
    List,
    /// Show one lineage; voice bodies load only with --voices.
    Show(IdentityShowArgs),
    /// Found a lineage and affiliate the acting participant.
    New(IdentityNameArgs),
    /// Affiliate the acting participant with an existing lineage.
    Continue(IdentityContinueArgs),
    /// Leave the acting participant's current lineage.
    Leave,
    /// Add or withdraw the acting participant's lineage voice.
    Voice(IdentityVoiceArgs),
    /// Set lineage terms.
    Terms(IdentityTermsArgs),
}

#[derive(Debug, Args)]
pub(crate) struct IdentityShowArgs {
    #[arg(value_name = "NAME", value_parser = nonempty_without_controls)]
    pub name: String,
    #[arg(long)]
    pub voices: bool,
}

#[derive(Debug, Args)]
pub(crate) struct IdentityNameArgs {
    #[arg(value_name = "NAME", value_parser = nonempty_without_controls)]
    pub name: String,
}

#[derive(Debug, Args)]
pub(crate) struct IdentityContinueArgs {
    #[arg(value_name = "NAME", value_parser = nonempty_without_controls)]
    pub name: String,
    #[arg(long)]
    pub acknowledge: bool,
}

#[derive(Debug, Args)]
pub(crate) struct IdentityVoiceArgs {
    #[command(subcommand)]
    pub command: IdentityVoiceCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum IdentityVoiceCommand {
    /// Add or replace this participant's current voice from a UTF-8 file.
    Add(IdentityBodyFileArgs),
    /// Withdraw this participant's voice.
    Withdraw(IdentityVoiceWithdrawArgs),
}

#[derive(Debug, Args)]
pub(crate) struct IdentityVoiceWithdrawArgs {
    /// Select the lineage containing this participant's voice.
    #[arg(long, value_name = "NAME", value_parser = nonempty_without_controls)]
    pub lineage: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct IdentityTermsArgs {
    #[command(subcommand)]
    pub command: IdentityTermsCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum IdentityTermsCommand {
    /// Set the current lineage terms from a UTF-8 file.
    Set(IdentityBodyFileArgs),
}

#[derive(Debug, Args)]
pub(crate) struct IdentityBodyFileArgs {
    #[arg(long = "body-file", value_name = "PATH", value_hint = clap::ValueHint::FilePath)]
    pub body_file: PathBuf,
}

#[derive(Debug, Args)]
#[command(
    override_usage = "post catchup [<CHANNEL> | --mail | --all] [--max-bytes N] [--framing auto|full|compact]"
)]
pub(crate) struct CatchupArgs {
    /// Channel name; catches up exactly this joined channel.
    #[arg(value_name = "CHANNEL", value_parser = nonempty_without_controls, conflicts_with_all = ["mail", "all"])]
    pub channel: Option<String>,

    /// Catch up direct mail only.
    #[arg(long, conflicts_with_all = ["channel", "all"])]
    pub mail: bool,

    /// Catch up direct mail and every joined channel (the default).
    #[arg(long, conflicts_with_all = ["channel", "mail"])]
    pub all: bool,

    /// Banner form for body-bearing catchup output. Auto emits one compact
    /// banner per non-empty invocation; JSON carries the structured framing.
    #[arg(long, value_enum)]
    pub framing: Option<FramingMode>,

    /// Apply one shared cap to final stdout across targets in their existing
    /// mail-then-channel order.
    #[arg(long = "max-bytes", value_name = "N", value_parser = positive_bytes)]
    pub max_bytes: Option<usize>,
}

#[derive(Debug, Args)]
#[command(
    override_usage = "post search <PATTERN> [--mail | --channel <CHANNEL>] [--limit <1..=1000>] [--framing auto|full|compact]"
)]
pub(crate) struct SearchArgs {
    /// Literal, case-insensitive Unicode substring to find.
    #[arg(value_name = "PATTERN", value_parser = nonempty_search_pattern)]
    pub pattern: String,

    /// Restrict the search to direct mail visible to the acting participant.
    #[arg(long, conflicts_with = "channel")]
    pub mail: bool,

    /// Restrict the search to one channel; the acting participant must be a member.
    #[arg(
        long,
        value_name = "CHANNEL",
        value_parser = nonempty_without_controls,
        conflicts_with = "mail"
    )]
    pub channel: Option<String>,

    /// Search only archived channels, membership not required; no mail.
    #[arg(long, conflicts_with_all = ["mail", "channel"])]
    pub archived: bool,

    /// Maximum number of results (default 100; hard cap 1000).
    #[arg(
        long,
        value_name = "N",
        default_value_t = 100,
        value_parser = search_limit
    )]
    pub limit: usize,

    /// Banner form for non-empty text output; JSON carries structured framing.
    #[arg(long, value_enum)]
    pub framing: Option<FramingMode>,
}

#[derive(Debug, Args)]
#[command(
    override_usage = "post send --to <ROOM> [OPTIONS] <<'EOF'      (body on stdin: heredoc or pipe, SAFEST)\n       \
     post send --to <ROOM> [OPTIONS] --body-file <PATH>   (body from a UTF-8 file; --body-file - reads stdin)\n       \
     post send --to <ROOM> [OPTIONS] --body <TEXT>        (short one-liners only)\n\n\
     Pass exactly one body source, or none to read stdin. A body on argv is parsed by your shell\n\
     first: backticks and $(...) inside double quotes execute and splice their output into the\n\
     message, and $1.63B expands. Prose belongs on stdin or in a file; a heredoc with a quoted\n\
     delimiter ('EOF') passes it through untouched. A bare [BODY] argument is the same as --body."
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

    /// Read the message body from this UTF-8 file (`-` reads stdin). The safe
    /// spelling for anything longer than one plain line.
    #[arg(
        long = "body-file",
        value_name = "PATH",
        value_hint = clap::ValueHint::FilePath,
        conflicts_with_all = ["body", "text"]
    )]
    pub body_file: Option<PathBuf>,

    /// Inline body text, for a short plain one-liner only. Your shell parses
    /// it before post sees it: `$1.63B` expands inside double quotes, an
    /// apostrophe ends single quotes, and backticks run commands. Anything
    /// else goes on stdin (`<<'EOF'`) or through --body-file.
    #[arg(long, value_name = "TEXT", conflicts_with_all = ["body_file", "text"])]
    pub body: Option<String>,

    /// Allow a body larger than the default 32 KiB safety limit.
    #[arg(long)]
    pub oversize: bool,

    /// Deliver to the sender's own participant inbox when `--to` names the
    /// sender's own room or lineage. A room send never reaches its own
    /// sender, so a session that pings its own room (delegate completion
    /// notices) would otherwise hear nothing. No effect on any other target.
    #[arg(long = "allow-self", hide = true)]
    pub allow_self: bool,

    /// A short body given directly, the same as --body; omit every body
    /// source to read stdin.
    #[arg(value_name = "BODY")]
    pub text: Option<String>,
}

impl SendArgs {
    /// The inline body, whichever spelling carried it (`--body` or the bare
    /// argument; clap refuses both at once).
    pub(crate) fn take_inline_body(&mut self) -> Option<String> {
        self.body.take().or_else(|| self.text.take())
    }
}

#[derive(Debug, Args)]
#[command(
    // To send, lead with sending. The read forms used to occupy the first nine
    // lines and the --send forms the last three, which reads as a read-only
    // command to anyone scanning the top of --help -- and three papercuts say
    // exactly that, one of them after three tries to find the syntax.
    //
    // Within the send forms, stdin leads and argv comes last. post cannot see
    // the shell and must not try to detect substituted prose, so the only
    // defence is making the safe path the obvious one: an agent that copies the
    // first form it reads copies a heredoc. The reverse ordering -- which this
    // usage block briefly had -- put the argv form first and would have taught
    // exactly the habit that spliced command output into #build.
    override_usage = "post chat <CHANNEL> --send < BODY_FILE               (send prose: heredoc or pipe, SAFEST)\n       \
     post chat <CHANNEL> --send --body-file <PATH>        (send prose from a file)\n       \
     post chat <CHANNEL> --send --body-file -             (send prose on stdin)\n       \
     post chat <CHANNEL> --send --body <TEXT>             (send a short line; argv only)\n       \
       ...prose belongs on stdin or in a file. A body on argv is parsed by your shell first:\n       \
       ...backticks and $(...) inside double quotes execute and splice their output into the message.\n       \
       ...add [--re ID] to reply, [--oversize] to exceed the size cap; a send always delivers and its receipt lists what crossed it\n       \
     post chat <CHANNEL> [--framing auto|full|compact] (read new messages; default oldest 25 unread)\n       \
     post chat <CHANNEL> --peek [--framing MODE]     (read without advancing)\n       \
     post chat <CHANNEL> --limit <N> [--framing MODE] (oldest N unread; --limit 0 = all)\n       \
     post chat <CHANNEL> --history <N> [--grep PAT] [--framing MODE] (last N messages, cursor untouched)\n       \
     post chat <CHANNEL> --since <ID> [--framing MODE] (messages after ID, cursor untouched)\n       \
     post chat <CHANNEL> --message <ID> [--offset B] [--length B] --max-bytes N (cursorless UTF-8 body slice)\n       \
     post chat <CHANNEL> --ack <ID>                    (mark exactly one message seen)\n       \
     post chat <CHANNEL> --discard                   (mark all unread seen without printing)\n       \
     post chat <CHANNEL> --discard-through <MSG_ID>  (mark unread at or before MSG_ID seen)\n       \
     post chat <CHANNEL> --seen-by <MSG_ID>          (which members have MSG_ID in their seen-set)\n       \
     post chat <CHANNEL> --join [--description TEXT] [--create] (join; creates the channel on first join, spelled lowercase-with-hyphens; --create forces a new one beside a look-alike)\n\n\
     post chat <CHANNEL> --leave                      (leave for this participant only)\n       \
     post chat <CHANNEL> --archive | --unarchive      (hide from / restore to `post channels`; never deletes)\n\n\
     These forms are alternatives; pass exactly one. --body/--body-file imply --send.\n\
     Direct mail to a single room is a different verb: `post send --to <ROOM>`."
)]
pub(crate) struct ChatArgs {
    /// Channel name.
    #[arg(value_name = "CHANNEL", value_parser = nonempty_without_controls)]
    pub name: String,

    /// Join the channel (creates it on first join); recorded in history.
    /// Unread starts at the join instant — pre-join messages are history,
    /// readable with --history; --backlog restores the old all-unread join.
    #[arg(long, conflicts_with_all = ["send", "peek", "discard", "body", "body_file", "subject", "seen_by", "history", "since", "limit", "grep", "re"])]
    pub join: bool,

    /// With --join: keep the whole backlog unread instead of starting unread
    /// from the join instant.
    #[arg(long, requires = "join")]
    pub backlog: bool,

    /// With --join: create the channel even when a similarly named one exists
    /// (a different spelling, or one typo away). Without it, joining a name
    /// that looks like an existing channel is refused and names that channel.
    #[arg(long, requires = "join")]
    pub create: bool,

    /// Archive the channel for everyone on this host: hidden from `post
    /// channels` and Porch, never deleted. Any participant may archive; a new
    /// post in the channel un-archives it automatically.
    #[arg(long, conflicts_with_all = ["unarchive", "leave", "join", "send", "peek", "discard", "body", "body_file", "subject", "seen_by", "history", "since", "limit", "grep", "re", "discard_through", "message", "ack", "framing", "max_bytes", "offset", "length", "oversize", "signature_ref", "description"])]
    pub archive: bool,

    /// Return an archived channel to the live `post channels` listing.
    #[arg(long, conflicts_with_all = ["archive", "leave", "join", "send", "peek", "discard", "body", "body_file", "subject", "seen_by", "history", "since", "limit", "grep", "re", "discard_through", "message", "ack", "framing", "max_bytes", "offset", "length", "oversize", "signature_ref", "description"])]
    pub unarchive: bool,

    /// Leave the channel for this participant only; preserves every seen id.
    #[arg(long, conflicts_with_all = ["join", "send", "peek", "discard", "body", "body_file", "subject", "seen_by", "history", "since", "limit", "grep", "re", "discard_through", "message", "ack", "framing", "max_bytes", "offset", "length", "oversize", "signature_ref", "description"])]
    pub leave: bool,

    /// Set or update the channel description (norms carrier); with --join.
    /// Cap 1 KiB. Any member may update.
    #[arg(long, value_name = "TEXT", value_parser = without_controls, requires = "join")]
    pub description: Option<String>,

    /// Send a message; the body comes from --body, --body-file, or stdin.
    #[arg(long, conflicts_with_all = ["peek", "discard", "seen_by"])]
    pub send: bool,

    /// Accepted and ignored: a send always delivers. Habitual commands that
    /// still pass it keep working; the receipt reports what crossed instead.
    #[arg(long, hide = true)]
    pub anyway: bool,

    /// Reply to a prior message id (or unique prefix) in this channel.
    #[arg(long = "re", value_name = "MSG_ID", value_parser = nonempty_without_controls, conflicts_with_all = ["join", "peek", "discard", "seen_by", "history", "since", "limit", "grep"])]
    pub re: Option<String>,

    /// Optional subject, limited to 1 KiB; only meaningful when sending.
    #[arg(long, default_value = "", value_parser = without_controls)]
    pub subject: String,

    /// Inline message body text; implies --send. A shell can expand `$1.63B`
    /// inside double quotes or end single quotes at an apostrophe; use
    /// --body-file or stdin for shell-sensitive prose.
    #[arg(long, value_name = "TEXT", conflicts_with_all = ["body_file", "peek", "discard", "seen_by"])]
    pub body: Option<String>,

    /// Read the message body from this UTF-8 file; implies --send.
    #[arg(
        long = "body-file",
        value_name = "PATH",
        value_hint = clap::ValueHint::FilePath,
        conflicts_with_all = ["peek", "discard", "seen_by"]
    )]
    pub body_file: Option<PathBuf>,

    /// Allow a body larger than the default 32 KiB safety limit.
    #[arg(long, conflicts_with_all = ["join", "peek", "discard", "seen_by"])]
    pub oversize: bool,

    /// Signed-message-v2 sidecar tag: stamps the envelope locator
    /// {"version":2,"tag":<TAG>} on the sent message. A locator, never a
    /// verdict — authority is computed at read time. Signed bodies are
    /// capped at 1 MiB regardless of --oversize. Only meaningful when
    /// sending.
    #[arg(long = "signature-ref", value_name = "TAG", value_parser = nonempty_without_controls, conflicts_with_all = ["join", "peek", "discard", "seen_by", "history", "since", "limit", "grep"])]
    pub signature_ref: Option<String>,

    /// Never a body and never a file. Captured only so a stray word after the
    /// channel (`post chat ops --send "hello"`) gets an answer that names the
    /// real body forms instead of opening a file called `hello`.
    #[arg(value_name = "STRAY", hide = true, num_args = 0..)]
    pub stray: Vec<String>,

    /// Read without advancing the cursor.
    #[arg(long)]
    pub peek: bool,

    /// Show the last N messages regardless of read state; never advances the cursor.
    #[arg(long, value_name = "N", conflicts_with_all = ["send", "join", "discard", "body", "body_file", "seen_by", "re"])]
    pub history: Option<usize>,

    /// Filter --history by case-insensitive regex (requires --history).
    #[arg(long, value_name = "PATTERN", requires = "history", conflicts_with_all = ["send", "join", "discard", "body", "body_file", "seen_by", "re", "limit", "since"])]
    pub grep: Option<String>,

    /// Only messages with id strictly after this id (ignores the cursor); never advances the cursor.
    #[arg(long, value_name = "ID", conflicts_with_all = ["send", "join", "discard", "body", "body_file", "seen_by", "re", "grep"], value_parser = nonempty_without_controls)]
    pub since: Option<String>,

    /// Advance the cursor past every unread message without printing them.
    #[arg(long, conflicts_with_all = ["peek", "send", "join", "seen_by"])]
    pub discard: bool,

    /// Bounded catch-up: consume only the oldest N unread (default 25 when
    /// omitted). `--limit 0` means unlimited. Use `--peek` for the newest-slice
    /// glance without advancing the cursor.
    #[arg(long, value_name = "N", conflicts_with_all = ["send", "join", "discard", "history", "since", "body", "body_file", "seen_by", "re", "grep"])]
    pub limit: Option<usize>,

    /// Mark every currently unseen message at or before MSG_ID (full id, or a
    /// prefix unique within the channel) as seen without printing bodies.
    /// Idempotent: a range that is already fully seen succeeds with
    /// `advanced: false`. Refuses when an unreadable unseen message sits in
    /// that range.
    #[arg(
        long = "discard-through",
        value_name = "MSG_ID",
        value_parser = nonempty_without_controls,
        conflicts_with_all = ["send", "join", "peek", "discard", "seen_by", "body", "body_file", "history", "since", "limit", "grep", "re", "subject", "oversize", "description"]
    )]
    pub discard_through: Option<String>,

    /// List member participants whose seen-set contains this message (read-only).
    #[arg(long = "seen-by", value_name = "MSG_ID", value_parser = nonempty_without_controls, conflicts_with_all = ["send", "join", "peek", "discard", "body", "body_file", "history", "since", "limit", "re", "grep", "subject", "oversize"])]
    pub seen_by: Option<String>,

    /// Banner form for body-returning reads: auto (default, quiet), full
    /// (every invocation), or compact (one-line reminder).
    /// Rejected on send/join/discard/discard-through/seen-by, which return no
    /// bodies and must not look like they honored it.
    /// Presentation when absent: POST_FRAMING env (auto|full|compact), else
    /// auto. An explicit value always wins over the environment.
    #[arg(long, value_enum, conflicts_with_all = ["send", "join", "discard", "discard_through", "seen_by", "body", "body_file"])]
    pub framing: Option<FramingMode>,

    /// Read one channel message body by UTF-8 byte range without consuming it.
    /// Requires --max-bytes; full ids and channel-unique prefixes are accepted.
    #[arg(
        long,
        value_name = "ID",
        value_parser = nonempty_without_controls,
        requires = "max_bytes",
        conflicts_with_all = ["send", "join", "peek", "discard", "discard_through", "seen_by", "body", "body_file", "history", "since", "limit", "grep", "re", "subject", "oversize", "description", "signature_ref"]
    )]
    pub message: Option<String>,

    /// Acknowledge exactly one resolved channel message id without printing
    /// its body or marking any earlier/later message seen.
    #[arg(
        long,
        value_name = "ID",
        value_parser = nonempty_without_controls,
        conflicts_with_all = ["send", "join", "peek", "discard", "discard_through", "seen_by", "body", "body_file", "history", "since", "limit", "grep", "re", "subject", "oversize", "description", "signature_ref", "message", "offset", "length", "max_bytes", "framing"]
    )]
    pub ack: Option<String>,

    /// Start a --message body slice at this UTF-8 byte offset (default 0).
    #[arg(
        long,
        value_name = "B",
        requires_all = ["message", "max_bytes"]
    )]
    pub offset: Option<usize>,

    /// Cap source body bytes considered for a --message slice. The final
    /// stdout cap remains --max-bytes.
    #[arg(
        long,
        value_name = "B",
        value_parser = positive_bytes,
        requires_all = ["message", "max_bytes"]
    )]
    pub length: Option<usize>,

    /// Cap final stdout bytes for a body-returning read. The cap includes
    /// UTF-8, escaping, framing, omission metadata, and the trailing newline.
    #[arg(
        long = "max-bytes",
        value_name = "N",
        value_parser = positive_bytes,
        conflicts_with_all = ["send", "join", "discard", "discard_through", "seen_by", "body", "body_file", "subject", "oversize", "description", "re", "signature_ref"]
    )]
    pub max_bytes: Option<usize>,
}

#[derive(Debug, Args)]
pub(crate) struct ChannelsArgs {
    /// Emit human-readable text instead of the default JSON.
    #[arg(long, conflicts_with = "json")]
    pub text: bool,

    /// List only archived channels (hidden from the default listing).
    #[arg(long, conflicts_with = "all")]
    pub archived: bool,

    /// List live and archived channels together.
    #[arg(long, conflicts_with = "archived")]
    pub all: bool,
}

#[derive(Debug, Args)]
pub(crate) struct WhoArgs {
    /// Restrict the whole report to these rooms: participant rows are the
    /// participants bound to a selected room, plus that room's heartbeat row.
    /// Omit for every registered room.
    #[arg(long, value_name = "ROOM", value_parser = NonEmptyStringValueParser::new())]
    pub room: Vec<String>,

    /// Emit human-readable text instead of the default JSON.
    #[arg(long, conflicts_with = "json")]
    pub text: bool,
}

#[derive(Debug, Args)]
pub(crate) struct InboxArgs {
    /// Room to read instead of the acting participant's own addresses. An unbound session
    /// has no addresses, so it sees only this room's pending summary (never its unread mail).
    #[arg(long, value_name = "ROOM", value_parser = NonEmptyStringValueParser::new())]
    pub room: Option<String>,

    /// Emit human-readable text instead of the default JSON.
    #[arg(long, conflicts_with = "json")]
    pub text: bool,

    /// Deliver held mail addressed to your lineage to its current members (needs a lineage; see `post identity`).
    #[arg(long)]
    pub adopt: bool,
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
    /// Change a local room's workspace (discovery) path; mail and history stay put.
    SetPath(RoomsSetPathArgs),
    /// Rename a local room, moving its mailbox and rewriting live references; history keeps the old name.
    Rename(RoomsRenameArgs),
}

#[derive(Debug, Args)]
pub(crate) struct RoomsRenameArgs {
    /// Registered local room to rename.
    #[arg(value_name = "OLD", value_parser = nonempty_without_controls)]
    pub old: String,

    /// New room name; must pass the same checks `add` applies.
    #[arg(value_name = "NEW", value_parser = nonempty_without_controls)]
    pub new: String,

    /// Validate and report the change without writing anything.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub(crate) struct RoomsSetPathArgs {
    /// Registered local room to re-point.
    #[arg(value_name = "NAME", value_parser = nonempty_without_controls)]
    pub name: String,

    /// Existing workspace directory; absolute or starting with ~/.
    #[arg(
        value_name = "PATH",
        value_hint = clap::ValueHint::DirPath,
        value_parser = nonempty_without_controls
    )]
    pub path: String,

    /// Validate and report the change without writing rooms.json.
    #[arg(long)]
    pub dry_run: bool,
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
#[command(
    after_help = "Migration: the old `post profile --name <NAME>` spelling is now `post profile set --name <NAME>` (add `--pfp <EMOJI>` to set the sigil in the same command)."
)]
pub(crate) struct ProfileArgs {
    #[command(subcommand)]
    pub command: Option<ProfileCommand>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ProfileCommand {
    /// Set the acting participant's display name and/or pfp; announces the change in its channels.
    Set(ProfileSetArgs),
    /// Show a profile: the acting participant's by default, or another participant's.
    Show(ProfileShowArgs),
    /// Remove the acting participant's profile; bylines fall back to its lineage, else the bare id.
    Clear,
    /// List every profile with its holder, sigil, and lease (read-only; text, or JSON with --json).
    List,
}

#[derive(Debug, Args)]
pub(crate) struct ProfileSetArgs {
    /// Display name, <=32 chars; may not imitate the signed owner or another room id.
    #[arg(long, value_name = "NAME", value_parser = nonempty_without_controls)]
    pub name: Option<String>,

    /// Exactly one emoji (one grapheme cluster), unique among profiles held now (active participants and registered legacy rooms).
    #[arg(long, value_name = "EMOJI", value_parser = nonempty_without_controls)]
    pub pfp: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct ProfileShowArgs {
    /// The participant whose profile to show: `participant:<id>` or a bare
    /// participant id. Defaults to the acting participant. A name that is not
    /// a participant shows that room's legacy entry (never stamped).
    #[arg(value_name = "PARTICIPANT", value_parser = NonEmptyStringValueParser::new())]
    pub participant: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct OwnerArgs {
    #[command(subcommand)]
    pub command: Option<OwnerCommand>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum OwnerCommand {
    /// Declare the signed owner: validate the room registration and values,
    /// then create owner.json atomically (create-only; never replaces).
    Init(OwnerInitArgs),
    /// Print the resolved owner (post-derivation) as JSON.
    Show,
}

#[derive(Debug, Args)]
#[command(
    override_usage = "post owner init --room <name> [--marker <glyph>] [--label <text>] [--sidecar-dir <abs>] [--allowed-signers <abs>] [--principal <principal>] [--namespace <namespace>]"
)]
pub(crate) struct OwnerInitArgs {
    /// Registered room that signs ("the owner").
    #[arg(long, value_name = "ROOM", value_parser = NonEmptyStringValueParser::new())]
    pub room: String,

    /// One non-ASCII visible glyph preceding the 🔏 lock on the signed wire
    /// (default 🧔). Must be a single grapheme; control/bidi characters,
    /// ASCII, and edge ZWJ are refused.
    #[arg(long, value_name = "GLYPH", value_parser = nonempty_without_controls)]
    pub marker: Option<String>,

    /// Verification label, 1-32 chars; verified renders always show it as
    /// "<label> (<room>)" so the immutable room id never disappears.
    #[arg(long, value_name = "TEXT", value_parser = nonempty_without_controls)]
    pub label: Option<String>,

    /// Absolute sidecar ROOT; code appends sigs/ under it. Default: the
    /// registered room's resolved path.
    #[arg(long = "sidecar-dir", value_name = "ABS", value_hint = clap::ValueHint::DirPath)]
    pub sidecar_dir: Option<PathBuf>,

    /// Absolute allowed_signers path. Default: <sidecar_dir>/allowed_signers.
    #[arg(long = "allowed-signers", value_name = "ABS", value_hint = clap::ValueHint::FilePath)]
    pub allowed_signers: Option<PathBuf>,

    /// ssh-keygen principal: [A-Za-z0-9._@-], 1-128 bytes. Default <room>@porch.
    #[arg(long, value_name = "PRINCIPAL")]
    pub principal: Option<String>,

    /// allowed_signers namespace: [A-Za-z0-9._@-], 1-64 bytes. Default <room>-porch.
    #[arg(long, value_name = "NAMESPACE")]
    pub namespace: Option<String>,
}

/// How much framing a body-returning read prints. The laws bind in every
/// mode. Automatic reads are quiet; explicit full/compact retain opt-in banners.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum FramingMode {
    /// Quiet message headers and bodies; activation notice is once per participant.
    #[default]
    Auto,
    /// Force the full multi-line banner on every invocation.
    Full,
    /// Explicitly request a condensed policy banner on every invocation.
    Compact,
}

#[derive(Debug, Args)]
pub(crate) struct ReadArgs {
    /// Full message id or a unique prefix.
    #[arg(value_name = "ID_OR_PREFIX", value_parser = NonEmptyStringValueParser::new())]
    pub id: String,

    /// Look only at this room's mail; by default every address the acting participant receives at.
    #[arg(long, value_name = "ROOM", value_parser = NonEmptyStringValueParser::new())]
    pub room: Option<String>,

    /// Read without marking the message read.
    #[arg(long)]
    pub peek: bool,
    /// Presentation when absent: POST_FRAMING env (auto|full|compact), else
    /// auto. An explicit value always wins over the environment.
    #[arg(long, value_enum)]
    pub framing: Option<FramingMode>,

    /// Cap final stdout bytes for a full-body read, including framing,
    /// escaping, omission metadata, and the trailing newline.
    #[arg(
        long = "max-bytes",
        value_name = "N",
        value_parser = positive_bytes,
        conflicts_with = "ack"
    )]
    pub max_bytes: Option<usize>,

    /// Start a cursorless body slice at this UTF-8 byte offset. The slice is
    /// explicit even at offset zero and never marks the mail read.
    #[arg(
        long,
        value_name = "B",
        requires = "max_bytes",
        conflicts_with_all = ["peek", "ack"]
    )]
    pub offset: Option<usize>,

    /// Cap source body bytes considered for a cursorless slice.
    #[arg(
        long,
        value_name = "B",
        value_parser = positive_bytes,
        requires = "max_bytes",
        conflicts_with_all = ["peek", "ack"]
    )]
    pub length: Option<usize>,

    /// Acknowledge exactly this resolved mail id without printing its body.
    #[arg(long, conflicts_with_all = ["peek", "framing", "max_bytes", "offset", "length"])]
    pub ack: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum WatchFrom {
    /// Suppress everything pending when the watch starts; only later arrivals ring.
    /// This enum is intentionally single-valued for now so future starts such
    /// as `--from <id>` can grow without changing the flag shape.
    Now,
}

/// A delivery reason `watch --reason` can select. Mirrors the `reason` field
/// every watch event and digest already carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum WatchReasonFilter {
    /// Direct or workspace mail, including unreadable mail files.
    Mail,
    /// Channel messages that do not mention you, and every unreadable channel
    /// message (its body cannot be read, so a mention cannot be detected).
    Channel,
    /// Readable channel messages that mention you.
    Mention,
}

#[derive(Debug, Args)]
pub(crate) struct WatchArgs {
    /// Mailbox room; repeat to merge rooms, or omit for cwd resolution.
    #[arg(long, value_name = "ROOM", value_parser = NonEmptyStringValueParser::new())]
    pub room: Vec<String>,

    /// Rooms this watcher IS, whose own channel messages are therefore not news
    /// to it. Repeat per room. Defaults to empty: watching a room is not the
    /// same as being it, and an observer that selects rooms it does not own
    /// must keep receiving their traffic.
    #[arg(long = "own", value_name = "ROOM", value_parser = NonEmptyStringValueParser::new())]
    pub own: Vec<String>,

    /// Exit 0 after the first batch that emits at least one event.
    #[arg(long)]
    pub once: bool,

    /// Scan exactly once and exit 0: a nonblocking poll for lifecycle hooks. An
    /// empty scan emits nothing; --interval-ms has no effect.
    #[arg(long, conflicts_with = "once")]
    pub snapshot: bool,

    /// Start after the current backlog instead of ringing it; currently only `now`.
    #[arg(
        long = "from",
        value_name = "WHEN",
        value_enum,
        conflicts_with = "snapshot"
    )]
    pub from: Option<WatchFrom>,

    /// In snapshot mode, emit only the last N events in scan order; 0 means
    /// unlimited. Omitted events remain unread because watch never consumes.
    #[arg(long, value_name = "N", requires = "snapshot")]
    pub limit: Option<usize>,

    /// Wait and heartbeat cadence in milliseconds; also the scan cadence when
    /// the native event backend is unavailable and watch polls instead.
    #[arg(long, value_name = "MS", default_value_t = 1000,
          value_parser = clap::value_parser!(u64).range(100..=60_000))]
    pub interval_ms: u64,

    /// Emit human-readable lines instead of the default NDJSON events.
    #[arg(long, conflicts_with = "json")]
    pub text: bool,

    /// Emit one summary per room/source group in each batch instead of one
    /// line per event. Composes with JSON/text and all watch modes.
    #[arg(long)]
    pub digest: bool,

    /// Deliver only events with this reason; repeat to combine. Omitted means
    /// every reason. Applied after the scan, so filtered events never ring and
    /// never count toward --limit or --once; with --digest, groups are built from
    /// the selected events only. Limit: an unreadable channel message always has
    /// reason `channel`, because its body (and any mention in it) cannot be read,
    /// so `--reason mention` alone does not surface it.
    #[arg(long = "reason", value_name = "REASON", value_enum)]
    pub reason: Vec<WatchReasonFilter>,
}

#[derive(Debug, Args)]
pub(crate) struct DoctorArgs {
    /// Create missing directories and default config files only.
    #[arg(long)]
    pub fix: bool,

    /// Print one summary line instead of the full JSON report (human-only;
    /// conflicts with --json).
    #[arg(long, conflicts_with = "json")]
    pub brief: bool,

    /// Report only findings at this level or worse: `warn` drops the
    /// informational lines, `error` keeps errors only. Status, count, and the
    /// exit code follow what is reported, so `--severity error` exits 0 on a
    /// store whose only findings are warnings.
    #[arg(long, value_enum, value_name = "LEVEL")]
    pub severity: Option<DoctorSeverityFilter>,
}

/// The `post doctor --severity` threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum DoctorSeverityFilter {
    /// Warnings and errors.
    Warn,
    /// Errors only.
    Error,
}

impl DoctorSeverityFilter {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}
