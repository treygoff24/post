//! Channel store: shared append-only group-chat history.
//!
//! Contract: mail 20260722-013246 (pinned), amended by 013434 (microsecond
//! ids). A channel is NOT a room and its messages are NOT mail: no kind
//! field (the kinds law stays untouched), no per-recipient copies, no
//! archive/ — messages/ is itself the immutable record because nothing is
//! ever moved or deleted from it. Membership is explicit and open to any
//! registered room; joins are recorded both in members.json (the index)
//! and as an event message in the history (the record). Blocked routes
//! bar shared membership at join time; channels never carry what a route
//! may not.

use crate::error::{AppError, AppResult, ErrorCode};
use crate::mailbox::{
    ascii_escape_json, atomic_replace, exclusive_atomic_write, local_timestamp_micros, new_mail_id,
    validate_room_name, Context,
};
use crate::model::{ChannelMessage, ParsedChannelMessage, RoomMap, SenderProvenance};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Read as _;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

pub(crate) const CHANNELS_DIR: &str = "channels";
const CHANNELS_LOCK_FILE: &str = ".channels.lock";
pub(crate) const JOIN_EVENT: &str = "join";
/// Profile-change announcement ("=== pact is now Lantern 🏮 (pact) ===").
pub(crate) const PROFILE_EVENT: &str = "profile";

/// A system event whose kind this build does not know. It is data, not
/// conversation: rendered as `[event: <kind>]`, passed through in JSON with its
/// kind, never unread, never a reason to refuse a read, listing, catch-up or
/// send. Known kinds (`join`, `profile`) keep their existing behavior.
pub(crate) fn is_opaque_event(message: &crate::model::ChannelMessage) -> bool {
    message
        .event
        .as_deref()
        .is_some_and(|kind| kind != JOIN_EVENT && kind != PROFILE_EVENT)
}

/// The bracketed label a text header shows for an event message. Known kinds
/// keep their bare name (`[join]`); an unknown kind reads `[event: <kind>]` so
/// a reader can tell "system event I have no name for" from a known one.
pub(crate) fn event_label(kind: &str) -> String {
    if kind == JOIN_EVENT || kind == PROFILE_EVENT {
        return kind.to_owned();
    }
    // The kind comes from a file another program wrote. It is shown on one
    // header line, so control characters never reach the terminal and a huge
    // value cannot flood it.
    let shown: String = kind
        .chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .take(64)
        .collect();
    format!("event: {shown}")
}

pub(crate) type MemberMap = BTreeMap<String, String>;

#[allow(dead_code)] // consumed by the read/cursor + doctor lanes' patches
pub(crate) fn channel_state_path(context: &Context, room: &str) -> AppResult<PathBuf> {
    validate_room_name(room).map_err(|reason| {
        AppError::new(
            ErrorCode::InvalidArgument,
            format!("room '{room}' is invalid: {reason}"),
            "Pass a single room name without path separators.",
        )
    })?;
    Ok(context.root.join(room).join("channel-state.json"))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelInfo {
    pub name: String,
    pub created: String,
    pub created_by: String,
    /// Norms carrier for the channel; any member may update via
    /// `--join --description`. Cap 1 KiB. Absent on pre-description stores.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug)]
pub(crate) struct ChannelPaths {
    #[allow(dead_code)] // consumed by the doctor lane's patch
    pub dir: PathBuf,
    pub messages: PathBuf,
    pub channel_json: PathBuf,
    pub members_json: PathBuf,
}

impl ChannelPaths {
    pub(crate) fn new(context: &Context, channel: &str) -> AppResult<Self> {
        validate_channel_name(channel)?;
        let dir = context.root.join(CHANNELS_DIR).join(channel);
        Ok(Self {
            messages: dir.join("messages"),
            channel_json: dir.join("channel.json"),
            members_json: dir.join("members.json"),
            dir,
        })
    }

    pub(crate) fn exists(&self) -> bool {
        self.channel_json.is_file()
    }

    pub(crate) fn load_members(&self) -> AppResult<MemberMap> {
        let bytes = match fs::read(&self.members_json) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(MemberMap::new());
            }
            Err(error) => {
                return Err(AppError::io(
                    "read channel members",
                    &self.members_json,
                    error,
                ))
            }
        };
        serde_json::from_slice(&bytes).map_err(|error| {
            AppError::config(&self.members_json, format!("invalid JSON object: {error}"))
        })
    }

    pub(crate) fn load_info(&self) -> AppResult<ChannelInfo> {
        let bytes = fs::read(&self.channel_json)
            .map_err(|error| AppError::io("read channel info", &self.channel_json, error))?;
        serde_json::from_slice(&bytes).map_err(|error| {
            AppError::config(&self.channel_json, format!("invalid JSON object: {error}"))
        })
    }
}

pub(crate) fn validate_channel_name(value: &str) -> AppResult<()> {
    validate_room_name(value).map_err(|reason| {
        AppError::new(
            ErrorCode::InvalidArgument,
            format!("channel '{value}' is invalid: {reason}"),
            "Pass a single path-safe channel name without '/' or '\\'.",
        )
        .input(value)
        .reason(reason)
    })
}

/// `#ops` as typed is the channel `ops`: `#` is how a channel renders, not part
/// of its name. The exception is a store that already holds a channel literally
/// named `#ops` (older posts created those): that channel is the one meant, so
/// the name is left alone. Every command that takes a channel name goes through
/// this, so the rendered name works anywhere.
pub(crate) fn strip_channel_sigil(context: &Context, name: &str) -> String {
    let bare = name.trim_start_matches('#');
    if bare.is_empty() || bare.len() == name.len() {
        return name.to_owned();
    }
    if ChannelPaths::new(context, name).is_ok_and(|paths| paths.exists()) {
        return name.to_owned();
    }
    bare.to_owned()
}

/// The names of every channel directory that holds a `channel.json`, sorted.
pub(crate) fn existing_channel_names(context: &Context) -> Vec<String> {
    let dir = context.root.join(CHANNELS_DIR);
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().join("channel.json").is_file())
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .collect();
    names.sort();
    names
}

/// The spelling a NEW channel gets: lowercase, spaces and underscores as
/// hyphens, hyphen runs collapsed, no leading or trailing hyphen. Dots are
/// kept as typed: reserved names such as `.rooms.json.x.tmp` are still channel
/// names the bridge has to classify, so normalizing must not turn one into another.
///
/// The Mac store held both `Night Porch` and `night-porch` because every agent
/// spelled the name its own way and each spelling silently made a new channel.
/// Returns the normalized name and the characters it had to drop because a
/// channel name cannot carry them (anything but letters, digits, `-` and `.`).
pub(crate) fn normalize_channel_name(given: &str) -> (String, Vec<char>) {
    let mut out = String::new();
    let mut dropped = Vec::new();
    for c in given.trim().chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if c == '.' {
            out.push('.');
        } else if c == '-' || c == '_' || c.is_whitespace() {
            if !out.ends_with('-') {
                out.push('-');
            }
        } else if !dropped.contains(&c) {
            dropped.push(c);
        }
    }
    let trimmed = out.trim_matches('-');
    (trimmed.to_owned(), dropped)
}

/// A name with everything but letters and digits removed, for "same name,
/// different punctuation" comparisons.
fn name_skeleton(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn edit_distance(left: &str, right: &str) -> usize {
    let right: Vec<char> = right.chars().collect();
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    for (row, l) in left.chars().enumerate() {
        let mut current = vec![row + 1];
        for (column, r) in right.iter().enumerate() {
            let substitute = previous[column] + usize::from(l != *r);
            current.push(
                substitute
                    .min(previous[column + 1] + 1)
                    .min(current[column] + 1),
            );
        }
        previous = current;
    }
    previous[right.len()]
}

/// Whether two channel names are plausibly the same channel spelled twice.
///
/// Same letters and digits with different punctuation or case always counts.
/// Beyond that a single typo counts once the name is long enough to make a
/// typo likelier than a coincidence. Numbers are identity (`ops-1` is not
/// `ops-2`), and so are one- or two-letter suffixes (`review-a`, `review-b`):
/// agents number and letter sibling channels on purpose.
fn names_look_alike(left: &str, right: &str) -> bool {
    let (a, b) = (name_skeleton(left), name_skeleton(right));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a == b {
        return true;
    }
    let digits = |text: &str| {
        text.chars()
            .filter(char::is_ascii_digit)
            .collect::<String>()
    };
    if digits(&a) != digits(&b) {
        return false;
    }
    let tokens = |name: &str| -> Vec<String> {
        name.split(|c: char| !c.is_alphanumeric())
            .filter(|token| !token.is_empty())
            .map(str::to_lowercase)
            .collect()
    };
    let (left_tokens, right_tokens) = (tokens(left), tokens(right));
    if left_tokens.len() == right_tokens.len() {
        let differing: Vec<usize> = (0..left_tokens.len())
            .filter(|&i| left_tokens[i] != right_tokens[i])
            .collect();
        if let [i] = differing[..] {
            if left_tokens[i].chars().count() <= 2 && right_tokens[i].chars().count() <= 2 {
                return false;
            }
        }
    }
    let shorter = a.chars().count().min(b.chars().count());
    let allowed = match shorter {
        0..=5 => 0,
        6..=9 => 1,
        _ => 2,
    };
    allowed > 0 && edit_distance(&a, &b) <= allowed
}

/// The existing channel `wanted` is most likely a second spelling of, if any.
fn similar_channel(context: &Context, wanted: &str) -> Option<String> {
    let names = existing_channel_names(context);
    // An exact skeleton match beats a typo match.
    names
        .iter()
        .find(|name| name_skeleton(name) == name_skeleton(wanted))
        .or_else(|| names.iter().find(|name| names_look_alike(name, wanted)))
        .cloned()
}

/// What the caller asked for alongside `--join`, so a suggested command can
/// repeat it faithfully.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct JoinIntent {
    pub create: bool,
    pub backlog: bool,
    pub has_description: bool,
}

impl JoinIntent {
    /// The join command for `name`, or `None` when this invocation carries
    /// something (`--description`) a rebuilt command would silently drop.
    fn command(&self, name: &str, force_create: bool) -> Option<String> {
        if self.has_description {
            return None;
        }
        let mut command = format!("post chat {} --join", crate::mailbox::shell_quote(name));
        if self.backlog {
            command.push_str(" --backlog");
        }
        if force_create {
            command.push_str(" --create");
        }
        Some(command)
    }
}

/// The channel a join resolves to.
pub(crate) struct JoinTarget {
    pub name: String,
    /// What the caller typed, when the stored name is a normalized form of it.
    pub normalized_from: Option<String>,
}

/// Decide which channel `chat <given> --join` means, before anything is
/// written.
///
/// An existing channel is used exactly as named, whatever its spelling: stores
/// that already hold `Night Porch` keep working. A new name is normalized; if
/// that lands on an existing channel, or on one that differs only by letter
/// case, it joins that channel under its STORED spelling, and if it merely
/// looks like one it is refused with the join that was probably meant and the
/// `--create` that forces the new channel.
///
/// Letter case is never a reason to create: the Mac filesystem treats `ops` and
/// `Ops` as one directory, so a channel written under one spelling and recorded
/// under the other splits its own membership from its messages, and a
/// case-sensitive filesystem would grow two channels. `--create` therefore
/// overrides only genuine look-alikes, never a case difference.
///
/// The caller holds the channels lock (see [`join`]): the directory scan here
/// and the creation that follows must not interleave with another join.
pub(crate) fn plan_join(
    context: &Context,
    given: &str,
    intent: JoinIntent,
) -> AppResult<JoinTarget> {
    // Exact directory names, not `exists()`: on a case-insensitive filesystem
    // `Night-Porch` would "exist" through `night-porch` and the channel would end
    // up named two ways.
    let existing_names = existing_channel_names(context);
    if existing_names.iter().any(|name| name == given) {
        return Ok(JoinTarget {
            name: given.to_owned(),
            normalized_from: None,
        });
    }
    let (wanted, dropped) = normalize_channel_name(given);
    let shown = crate::output::sanitize_text_header(given);
    if wanted.is_empty() {
        return Err(AppError::new(
            ErrorCode::InvalidArgument,
            format!("'{shown}' has no letters or digits to make a channel name from"),
            "Name the channel with letters, digits and hyphens, for example `post chat 'night-porch' --join`.",
        )
        .input(shown)
        .reason("normalizing the name leaves nothing"));
    }
    if !dropped.is_empty() {
        let listed: String = dropped.iter().collect();
        let mut error = AppError::new(
            ErrorCode::InvalidArgument,
            format!(
                "channel name '{shown}' has characters a channel name cannot carry ({}); the normalized form is '{wanted}'",
                crate::output::sanitize_text_header(&listed)
            ),
            format!(
                "Channel names use lowercase letters, digits and hyphens. Use the normalized form: {}.",
                intent
                    .command(&wanted, false)
                    .map_or_else(|| format!("'{wanted}'"), |command| format!("`{command}`"))
            ),
        )
        .input(shown)
        .reason("channel names allow only letters, digits, '-' and '.'");
        if let Some(command) = intent.command(&wanted, false) {
            error = error.exact_fix(command);
        }
        return Err(error);
    }
    validate_channel_name(&wanted)?;
    // The exact spelling wins; failing that, the stored spelling that differs
    // only by case. Compared by lowercase text, not by asking the filesystem,
    // so a case-sensitive store resolves exactly as a case-insensitive one.
    let stored = existing_names
        .iter()
        .find(|name| **name == wanted)
        .or_else(|| {
            existing_names
                .iter()
                .find(|name| name.to_lowercase() == wanted)
        });
    if let Some(stored) = stored {
        // A different spelling of a channel that exists is that channel: two
        // agents told to join `Night Porch` both land in #night-porch. Nothing
        // is created, so there is nothing to ask about, and `--create` has
        // nothing to override.
        return Ok(JoinTarget {
            name: stored.clone(),
            normalized_from: Some(given.to_owned()),
        });
    }
    if !intent.create {
        if let Some(existing) = similar_channel(context, &wanted) {
            return Err(did_you_mean(&shown, &wanted, &existing, intent));
        }
    }
    let normalized_from = (wanted != given).then(|| given.to_owned());
    Ok(JoinTarget {
        name: wanted,
        normalized_from,
    })
}

fn did_you_mean(shown: &str, wanted: &str, existing: &str, intent: JoinIntent) -> AppError {
    let join = intent.command(existing, false);
    let mut fix = format!(
        "Join the existing channel with {}",
        quote_command(&join, existing)
    );
    fix.push_str(&format!(
        ", or create a new one on purpose with {}",
        quote_command(&intent.command(wanted, true), wanted)
    ));
    fix.push('.');
    let mut error = AppError::new(
        ErrorCode::InvalidArgument,
        format!(
            "channel '{shown}' does not exist; did you mean #{}? Nothing was created",
            crate::output::sanitize_text_header(existing)
        ),
        fix,
    )
    .input(shown.to_owned())
    .reason("a channel with a near-identical name already exists");
    if let Some(join) = join {
        error = error.exact_fix(join);
    }
    error
}

fn quote_command(command: &Option<String>, name: &str) -> String {
    match command {
        Some(command) => format!("`{command}`"),
        None => format!(
            "`post chat {} --join` with the same options",
            crate::mailbox::shell_quote(name)
        ),
    }
}

/// How the caller was using the channel that turned out not to exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChannelUse {
    Read,
    Send,
}

/// The one error for "no channel by that name", shared by every command.
///
/// A registered room is not a channel, and typing `post chat <room>` is the
/// natural way to try to reach one: say it is a room and give the send that
/// reaches it, never a hint toward creating a channel of the same name. A name
/// close to an existing channel gets that channel back. Only a name that is
/// neither gets the create hint.
pub(crate) fn channel_not_found(context: &Context, name: &str, usage: ChannelUse) -> AppError {
    let shown = crate::output::sanitize_text_header(name);
    let quoted = crate::mailbox::shell_quote(name);
    if context
        .load_rooms()
        .is_ok_and(|rooms| rooms.contains_key(name))
    {
        return AppError::new(
            ErrorCode::NotFound,
            format!("'{shown}' is a registered room, not a channel"),
            format!(
                "Message the room with `post send --to {quoted} --body-file -` (body on stdin, a heredoc works); `post channels` lists the channels that exist."
            ),
        )
        .input(shown)
        .reason("the name belongs to a registered room and no channel has it");
    }
    let then = match usage {
        ChannelUse::Read => "",
        ChannelUse::Send => ", then retry the send",
    };
    if let Some(existing) = similar_channel(context, &normalize_channel_name(name).0)
        .or_else(|| similar_channel(context, name))
    {
        let join = format!(
            "post chat {} --join",
            crate::mailbox::shell_quote(&existing)
        );
        return AppError::new(
            ErrorCode::NotFound,
            format!(
                "channel '{shown}' does not exist; did you mean #{}?",
                crate::output::sanitize_text_header(&existing)
            ),
            format!(
                "Join the channel that does exist with `{join}`{then}, or create this one on purpose with `post chat {quoted} --join --create`."
            ),
        )
        .exact_fix(join)
        .input(shown)
        .reason("no channel.json under the channels directory");
    }
    AppError::new(
        ErrorCode::NotFound,
        format!("channel '{shown}' does not exist"),
        format!("Create it with `post chat {quoted} --join`{then}."),
    )
    .input(shown)
    .reason("no channel.json under the channels directory")
}

/// One lock for all membership mutation across every channel: joins are
/// rare and human-paced, so a global lock is simpler than per-channel
/// locks and cannot deadlock. Message sends never take it — exclusive
/// file creation is their arbiter.
pub(crate) fn lock_channels(context: &Context) -> AppResult<File> {
    let dir = context.root.join(CHANNELS_DIR);
    fs::create_dir_all(&dir)
        .map_err(|error| AppError::io("create channels directory", &dir, error))?;
    let path = dir.join(CHANNELS_LOCK_FILE);
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&path)
        .map_err(|error| AppError::io("open channels lock", &path, error))?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == -1 {
        return Err(AppError::io(
            "lock channels registry",
            &path,
            std::io::Error::last_os_error(),
        ));
    }
    Ok(file)
}

/// Resolve the acting room for channel operations. A bound participant's
/// workspace is authoritative; unbound read-only commands retain the legacy
/// POST_FROM-or-cwd lookup. There is deliberately no --from/--room override
/// on channel commands. Membership additionally requires a registered room.
pub(crate) fn acting_room(
    context: &Context,
    rooms: &RoomMap,
) -> AppResult<(String, SenderProvenance)> {
    let (room, provenance) = match crate::participant::resolve(context) {
        Ok(crate::participant::Resolved::Bound { participant, .. }) => {
            let room = participant
                .workspace
                .clone()
                .unwrap_or_else(|| participant.id.clone());
            let provenance = match crate::mailbox::declared_env_pin()? {
                Some(pin) => {
                    if pin != room {
                        return Err(AppError::new(
                            ErrorCode::InvalidArgument,
                            format!(
                                "POST_FROM workspace pin '{pin}' conflicts with bound participant '{}' reply address '{room}'",
                                participant.id
                            ),
                            "Run `post participant bind --workspace <room>` to change workspace context deliberately.",
                        )
                        .input(pin)
                        .reason("workspace pin disagrees with bound participant"));
                    }
                    SenderProvenance::DeclaredEnv
                }
                None => SenderProvenance::ParticipantBinding,
            };
            (room, provenance)
        }
        Ok(crate::participant::Resolved::Unbound) => {
            context.resolved_room_with_provenance(None, rooms)?
        }
        Err(_) if crate::mailbox::read_only_command() => {
            context.resolved_room_with_provenance(None, rooms)?
        }
        Err(error) => return Err(error),
    };
    // A participant without workspace context is still a first-class channel
    // actor; its participant id is the shared reply address. Legacy code
    // required a registered room here, which made session-only participants
    // unable to join despite having durable membership/read state.
    Ok((room, provenance))
}

/// How the acting room was resolved, in words for a human or an agent. Text
/// mode prints it as a banner; under `--json` there is no banner, so errors that
/// depend on who was acting carry it themselves.
pub(crate) fn acting_source(provenance: SenderProvenance) -> &'static str {
    match provenance {
        SenderProvenance::DeclaredEnv => "POST_FROM pin",
        SenderProvenance::DeclaredFlag => "explicit flag",
        SenderProvenance::InferredCwd | SenderProvenance::InferredBasename => {
            "identity inferred from cwd"
        }
        SenderProvenance::ParticipantBinding => "participant binding",
    }
}

pub(crate) struct JoinOutcome {
    pub room: String,
    pub channel_created: bool,
    pub already_member: bool,
    pub event_id: Option<String>,
    /// Messages that predated this join and now read as history. Absent on
    /// an already-member response, which records no new membership start.
    pub history_before_join: Option<usize>,
}

/// Join the channel `given` means. Which channel that is (an existing one, a
/// normalized spelling, a refusal because it looks like another) is decided
/// UNDER the channels lock, in the same critical section that creates it:
/// deciding first and locking after let two joins of look-alike names both find
/// nothing and create two channels. `announce` runs once the name is settled and
/// before anything is written.
pub(crate) fn join(
    context: &Context,
    given: &str,
    description: Option<&str>,
    backlog: bool,
    create: bool,
    announce: impl FnOnce(&JoinTarget),
) -> AppResult<(JoinTarget, JoinOutcome)> {
    let _lock = lock_channels(context)?;
    let target = plan_join(
        context,
        given,
        JoinIntent {
            create,
            backlog,
            has_description: description.is_some(),
        },
    )?;
    announce(&target);
    let outcome = join_resolved(context, &target.name, description, backlog)?;
    Ok((target, outcome))
}

/// The membership and channel writes of a join, with the channels lock held by
/// the caller.
fn join_resolved(
    context: &Context,
    channel: &str,
    description: Option<&str>,
    backlog: bool,
) -> AppResult<JoinOutcome> {
    let rooms = context.load_rooms()?;
    let (room, provenance) = acting_room(context, &rooms)?;
    let actor = context.sender()?;
    let paths = ChannelPaths::new(context, channel)?;

    if let Some(description) = description {
        validate_description(description)?;
    }

    let membership = crate::channel_state::ParticipantChannels::load(&actor.participant)?;
    if membership.joined_names().contains(channel) {
        // Already a member: --description still updates the norms carrier.
        if let Some(description) = description {
            write_description(&paths, description)?;
        }
        return Ok(JoinOutcome {
            room,
            channel_created: false,
            already_member: true,
            event_id: None,
            history_before_join: None,
        });
    }

    // A legacy workspace member already has a membership start (its
    // `created`). Its explicit join keeps that floor rather than minting now,
    // so no message it has not yet read turns into history.
    let legacy_start = membership.membership_start(context, &actor.participant, channel)?;

    // Blocked routes bar shared membership, both directions, before any
    // state is written. Checked under the lock so a concurrent join of the
    // blocked counterpart cannot slip in between check and write.
    let rules = context.load_rules(&rooms)?;
    let mut existing_members: Vec<(String, String)> =
        crate::channel_state::participants_for_join_validation(
            context,
            channel,
            &actor.participant,
            &room,
            &rules.blocked,
        )?
        .into_iter()
        .map(|member| {
            let address = member
                .workspace
                .clone()
                .unwrap_or_else(|| member.id.clone());
            (member.id, address)
        })
        .collect();
    for workspace in paths.load_members()?.into_keys() {
        if !existing_members
            .iter()
            .any(|(_, address)| address == &workspace)
        {
            existing_members.push((workspace.clone(), workspace));
        }
    }
    for (member_id, member_address) in existing_members {
        if let Some(rule) = rules.blocked.iter().find(|rule| {
            rule.matches_route(&room, &member_address)
                || rule.matches_route(&member_address, &room)
                || rule.matches_route(&actor.participant.id, &member_id)
                || rule.matches_route(&member_id, &actor.participant.id)
        }) {
            return Err(AppError::new(
                ErrorCode::BlockedRoute,
                format!(
                    "joining '{channel}' would put '{room}' and existing member '{member}' in one channel, and that route is blocked: {}",
                    rule.reason,
                    member = member_id
                ),
                "Do not route around this block. Ask the human operator to review rules.json.",
            )
            .input(format!("{} <-> {}", actor.participant.id, member_id))
            .reason(rule.reason.clone())
            .rule(rule.clone()));
        }
    }

    fs::create_dir_all(&paths.messages).map_err(|error| {
        AppError::io("create channel messages directory", &paths.messages, error)
    })?;

    // The membership start: join-from-now records this instant, so the whole
    // pre-join backlog is history rather than unread; `--backlog` records the
    // minimum watermark instead, keeping every message unread (the old
    // behavior, as an opt-in). A legacy member keeps the start it already had.
    // The count mirrors exactly what the member will not see as unread:
    // existing ids that sort below the start.
    let start = if backlog {
        crate::channel_state::BACKLOG_MEMBERSHIP_START.to_owned()
    } else if let Some(existing) = legacy_start {
        existing
    } else {
        local_timestamp_micros()?.0
    };
    let history_before_join = message_files(&paths.messages)?
        .iter()
        .filter(|path| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|id| id < start.as_str())
        })
        .count();

    let (_, sent) = local_timestamp_micros()?;
    let channel_created = if paths.exists() {
        if let Some(description) = description {
            write_description(&paths, description)?;
        }
        false
    } else {
        let info = ChannelInfo {
            name: channel.to_owned(),
            created: sent.clone(),
            created_by: room.clone(),
            description: description
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
        };
        write_channel_info(&paths, &info)?;
        true
    };

    // Event first, membership second: a crash between the two leaves a
    // visible join event without membership, which the next join attempt
    // repairs (at worst a duplicate event). The other order leaves a
    // member whose join the history never shows — a permanent hole.
    let event_id = write_message(
        context,
        &paths,
        WriteMessage {
            room: &room,
            channel,
            subject: "",
            body: &format!("=== {room} joined ==="),
            event: Some(JOIN_EVENT),
            re: None,
            mentions: Vec::new(),
            signature_tag: None,
            provenance,
        },
    )?;
    crate::channel_state::ParticipantChannels::join_at(
        context,
        &actor.participant,
        channel,
        &start,
    )?;

    Ok(JoinOutcome {
        room,
        channel_created,
        already_member: false,
        event_id: Some(event_id),
        history_before_join: Some(history_before_join),
    })
}

const MAX_DESCRIPTION_BYTES: usize = 1024;

pub(crate) fn validate_description(description: &str) -> AppResult<()> {
    if description.len() <= MAX_DESCRIPTION_BYTES {
        return Ok(());
    }
    Err(AppError::new(
        ErrorCode::InvalidArgument,
        format!(
            "channel description is {} bytes; the maximum is {MAX_DESCRIPTION_BYTES} bytes",
            description.len()
        ),
        "Keep --description at or below 1024 bytes (same cap as subjects).",
    )
    .input("--description")
    .reason(format!(
        "description exceeds {MAX_DESCRIPTION_BYTES}-byte safety limit"
    )))
}

fn write_channel_info(paths: &ChannelPaths, info: &ChannelInfo) -> AppResult<()> {
    let mut bytes = serde_json::to_vec_pretty(info).map_err(|error| {
        AppError::io(
            "serialize channel info",
            &paths.channel_json,
            std::io::Error::new(std::io::ErrorKind::InvalidData, error),
        )
    })?;
    bytes.push(b'\n');
    atomic_replace(&paths.channel_json, &bytes)
        .map_err(|error| AppError::io("write channel info", &paths.channel_json, error))
}

fn write_description(paths: &ChannelPaths, description: &str) -> AppResult<()> {
    let mut info = paths.load_info()?;
    info.description = if description.is_empty() {
        None
    } else {
        Some(description.to_owned())
    };
    write_channel_info(paths, &info)
}

pub(crate) struct SendOptions<'a> {
    pub subject: &'a str,
    pub body: &'a str,
    pub re: Option<&'a str>,
    /// Signed-v2 sidecar tag; when present the envelope is stamped with the
    /// exact locator `{"version": 2, "tag": <tag>}`. Validated at the CLI.
    pub signature_tag: Option<&'a str>,
}

/// What a send produced: the message, plus what crossed it on the way out.
pub(crate) struct SentMessage {
    pub message: ChannelMessage,
    /// Present only when someone else's messages were unseen by the sender at
    /// the moment it sent. Absent (not empty) on a clean send.
    pub crossed: Option<Crossed>,
    /// Unseen files that could not be parsed. They never stop a send.
    pub skipped: Vec<SkippedFile>,
    /// Things the send degraded on and the caller should say on stdout.
    pub warnings: Vec<String>,
}

pub(crate) fn send(
    context: &Context,
    channel: &str,
    options: SendOptions<'_>,
) -> AppResult<SentMessage> {
    let rooms = context.load_rooms()?;
    let (room, provenance) = acting_room(context, &rooms)?;
    let actor = context.sender()?;
    let paths = ChannelPaths::new(context, channel)?;
    let quoted = crate::mailbox::shell_quote(channel);
    if !paths.exists() {
        return Err(channel_not_found(context, channel, ChannelUse::Send));
    }
    let membership = crate::channel_state::ParticipantChannels::load(&actor.participant)?;
    if !membership.effective(context, &actor.participant, channel)? {
        return Err(AppError::new(
            ErrorCode::NotAMember,
            format!(
                "participant '{}' is not a member of channel '{channel}' (acting as room '{room}', {})",
                actor.participant.id,
                acting_source(provenance)
            ),
            format!("Join first with `post chat {quoted} --join`, then retry the send."),
        )
        .input(actor.participant.id)
        .reason(
            "participant is neither explicitly joined nor covered by a legacy workspace default",
        ));
    }
    if options.body.trim().is_empty() {
        return Err(AppError::new(
            ErrorCode::EmptyBody,
            "message body is empty after trimming whitespace",
            format!(
                "Put the message on stdin or in a file: `post chat {quoted} --send --body-file -` reads stdin (a heredoc works), or pass --body 'short text' for a one-liner."
            ),
        )
        .input("message body")
        .reason("empty or whitespace-only"));
    }

    // What crossed this send. A send always delivers: the old guard refused when
    // something unseen was addressed to the sender, and agents answered it with
    // `--anyway` 73% of the time -- including when the crossed message really was
    // addressed to them -- until they typed it pre-emptively and the guard
    // measured nothing but its own bypass rate. The receipt now carries what
    // crossed instead, so the sender learns it without paying a retry. Check and
    // append still have a TOCTOU window (another room can land a message between
    // the two); that slip is accepted. Corrupting the store is not.
    let mut warnings = Vec::new();
    let (crossed, skipped) =
        match crossed_report(context, &paths, channel, &room, &actor.participant) {
            Ok(report) => report,
            // Checking is best-effort: a failure here must not cost the message.
            Err(error) => {
                warnings.push(format!(
                    "could not check what crossed this send: {}",
                    error.message
                ));
                (None, Vec::new())
            }
        };

    let re = match options.re {
        Some(prefix) => Some(resolve_message_id(&paths, prefix)?),
        None => None,
    };
    let mentions = extract_mentions(options.body, &rooms);
    let id = write_message(
        context,
        &paths,
        WriteMessage {
            room: &room,
            channel,
            subject: options.subject,
            body: options.body,
            event: None,
            re,
            mentions,
            signature_tag: options.signature_tag,
            provenance,
        },
    )?;
    if let Some(crossed) = crossed.as_ref() {
        log_crossed_event(
            context,
            channel,
            &room,
            crossed.unseen,
            crossed.addressed_to_you,
        );
    }
    let file = paths.messages.join(format!("{id}.msg"));
    Ok(SentMessage {
        message: parse_channel_message(&file)?.message,
        crossed,
        skipped,
        warnings,
    })
}

/// How strictly a reader treats a channel message file it cannot parse.
///
/// `Tolerant` is every read-only listing and read: the file is skipped and
/// reported. `Strict` keeps the fail-closed behavior for the few callers that
/// must not act on a partial picture (the `post watch` doorbell's validation
/// pass, and consuming cursor bookkeeping).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scan {
    Strict,
    Tolerant,
}

/// A file in a channel's `messages/` that could not be parsed. Readers skip it
/// and say so; it never fails a listing, a read, a catch-up or a send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SkippedFile {
    /// The file stem, which is the message id when the file is well-formed.
    pub id: String,
    pub reason: String,
    /// Set by listings that cover several channels (`channels`, `search`,
    /// `catchup`); a single-channel read already names its channel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
}

impl SkippedFile {
    /// Only corruption-shaped failures are skippable: a bad envelope or a file
    /// that cannot be read. Anything else (a broken cursor store, a lock) is not
    /// about this file and still propagates.
    pub(crate) fn from_error(path: &Path, error: &AppError) -> Option<Self> {
        if !matches!(error.code, ErrorCode::ConfigInvalid | ErrorCode::IoError) {
            return None;
        }
        let id = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("?")
            .to_owned();
        let detail = error
            .details
            .reason
            .clone()
            .unwrap_or_else(|| error.message.clone());
        let reason = match error.code {
            ErrorCode::IoError => format!("could not be read: {detail}"),
            _ => detail,
        };
        Some(Self {
            id,
            reason: one_line(&reason, 300),
            channel: None,
        })
    }

    pub(crate) fn in_channel(mut self, channel: &str) -> Self {
        self.channel = Some(channel.to_owned());
        self
    }
}

/// Collapse to one printable line of at most `cap` characters.
fn one_line(text: &str, cap: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    preview(&flat, cap)
}

/// The single stdout line a text-mode command prints when it skipped files, or
/// `None` when it skipped nothing.
pub(crate) fn skipped_notice(skipped: &[SkippedFile]) -> Option<String> {
    notice_line(skipped, SkippedDetail::Listed, "--json lists all")
}

/// How much of the skipped-file list a byte-bounded read carries.
///
/// The list grows with the number of corrupt files and the budget does not, so
/// a bounded read can never afford the whole list: with enough corrupt files
/// the list alone would exceed `--max-bytes` and the read would fail with no
/// output, letting corrupt files block the readable messages beside them. A
/// bounded read carries the count and the first few ids, and names the command
/// that lists the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SkippedDetail {
    /// Up to [`BOUNDED_SKIPPED_SHOWN`] entries, each with a short reason.
    Listed,
    /// The count only: for a budget too tight to afford even the listed form
    /// without dropping a message.
    CountOnly,
}

/// Entries a bounded read lists before reporting the rest as a count.
pub(crate) const BOUNDED_SKIPPED_SHOWN: usize = 3;
const BOUNDED_REASON_CHARS: usize = 60;

/// The skipped-file report of one byte-bounded JSON read. Serialize `shown`
/// as the list, `total` and `hint` beside it when present.
#[derive(Debug, Clone, Default)]
pub(crate) struct BoundedSkipped {
    pub shown: Vec<SkippedFile>,
    /// How many files were skipped in all. Set only when `shown` is not the
    /// whole list, so an unabridged report reads exactly like an unbounded one.
    pub total: Option<usize>,
    /// The runnable command that lists every skipped file. Set with `total`.
    pub hint: Option<String>,
}

impl BoundedSkipped {
    pub(crate) fn new(all: &[SkippedFile], detail: SkippedDetail, list_all_with: &str) -> Self {
        let take = match detail {
            SkippedDetail::Listed => BOUNDED_SKIPPED_SHOWN,
            SkippedDetail::CountOnly => 0,
        };
        let shown: Vec<SkippedFile> = all
            .iter()
            .take(take)
            .map(|file| SkippedFile {
                reason: one_line(&file.reason, BOUNDED_REASON_CHARS),
                ..file.clone()
            })
            .collect();
        let abridged = shown.len() < all.len();
        Self {
            total: abridged.then_some(all.len()),
            hint: abridged.then(|| list_all_with.to_owned()),
            shown,
        }
    }
}

/// [`skipped_notice`] for a byte-bounded text read: at most the first few
/// files, and the command that lists all of them.
pub(crate) fn bounded_skipped_notice(
    skipped: &[SkippedFile],
    detail: SkippedDetail,
    list_all_with: &str,
) -> Option<String> {
    notice_line(skipped, detail, &format!("`{list_all_with}` lists all"))
}

fn notice_line(skipped: &[SkippedFile], detail: SkippedDetail, lists_all: &str) -> Option<String> {
    const SHOWN: usize = BOUNDED_SKIPPED_SHOWN;
    if skipped.is_empty() {
        return None;
    }
    if detail == SkippedDetail::CountOnly {
        return Some(format!(
            "post: skipped {} unreadable message file(s); {lists_all}.\n",
            skipped.len()
        ));
    }
    let mut parts = Vec::new();
    for file in skipped.iter().take(SHOWN) {
        let name = match &file.channel {
            Some(channel) => format!("#{channel}/{}", file.id),
            None => file.id.clone(),
        };
        parts.push(format!(
            "{} ({})",
            crate::output::sanitize_text_header(&name),
            crate::output::sanitize_text_header(&one_line(&file.reason, 80))
        ));
    }
    let more = skipped.len().saturating_sub(SHOWN);
    let tail = if more > 0 {
        format!(", and {more} more ({lists_all})")
    } else {
        String::new()
    };
    Some(format!(
        "post: skipped {} unreadable message file(s): {}{tail}; other messages are unaffected. Move the file aside or restore it.\n",
        skipped.len(),
        parts.join(", ")
    ))
}

/// One message that crossed a send.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct CrossedMessage {
    pub id: String,
    pub from: String,
    pub display_name: Option<String>,
    pub sent: String,
    pub addressed_to_you: bool,
    /// Full for a message addressed to the sender; a 300-character preview
    /// otherwise.
    pub body: String,
    /// Only on signed-looking messages from the owner room: whether the
    /// signature verifies against the complete stored body. A body is never
    /// shown as the owner's word without this verdict beside it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signed_verified: Option<bool>,
    /// Identity fields as the sender declared them, carried raw: a crossed
    /// send is the concurrent-instance moment where attribution matters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sender_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sender_provenance: Option<String>,
}

/// Everything from others the sender had not seen when it sent, capped at
/// [`CROSSED_MESSAGE_CAP`] messages, newest last.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Crossed {
    pub unseen: usize,
    pub addressed_to_you: usize,
    pub messages: Vec<CrossedMessage>,
}

/// At most this many messages ride in a receipt.
const CROSSED_MESSAGE_CAP: usize = 10;
/// A message not addressed to the sender is shown as this many characters.
const CROSSED_PREVIEW_CHARS: usize = 300;

/// The crossed-send audit log line for a send that delivered over an unseen tip.
fn log_crossed_event(context: &Context, channel: &str, room: &str, unseen: usize, targeted: usize) {
    let path = context.root.join("crossed-send.jsonl");
    let Some(now_ms) = epoch_millis() else { return };
    let line = format!(
        "{{\"epoch_ms\":{now_ms},\"room\":\"{}\",\"channel\":\"{}\",\"unseen\":{unseen},\"targeted\":{targeted},\"outcome\":\"delivered_crossed\"}}\n",
        ascii_escape_json(room),
        ascii_escape_json(channel),
    );
    // Telemetry must never be able to fail a send, so every error here is
    // deliberately dropped: a lost audit line costs a data point, a failed send
    // costs the message.
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)
        .and_then(|mut file| std::io::Write::write_all(&mut file, line.as_bytes()));
}

fn epoch_millis() -> Option<u128> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|elapsed| elapsed.as_millis())
}

/// What crossed this send: the sender's unseen messages from others.
///
/// The line for "addressed to you" is the one this store draws everywhere else:
/// a message that @mentions the room, replies to something the room wrote, or
/// comes from the owner room is conversation the sender must not miss. A signed
/// owner message binds its whole body, so adding "@you" to one would break the
/// signature -- the owner clause is a consequence, not deference.
///
/// Corrupt unseen files are skipped and returned; system events (join/profile
/// and unknown kinds) are not conversation the sender needs to revise against.
fn crossed_report(
    context: &Context,
    paths: &ChannelPaths,
    channel: &str,
    room: &str,
    participant: &crate::participant::Participant,
) -> AppResult<(Option<Crossed>, Vec<SkippedFile>)> {
    use crate::cursor_state::eligibility::unread_channel_with;

    // The owner is resolved once: it decides which sender counts as "addressed
    // to you" and which crossed bodies carry a signature verdict. A broken
    // owner.json stops this report (the send says so in `warnings` and still
    // delivers) rather than showing a signed-looking body without its verdict.
    let owner = crate::mailbox::resolve_owner(context)?;
    let owner_room = owner.as_ref().map(|owner| owner.room.clone());
    let scan = unread_channel_with(context, participant, channel, Scan::Tolerant)?;
    let skipped = scan.skipped;
    let mut items: Vec<(ChannelMessage, String, bool)> = Vec::new();
    for item in scan.items {
        if item.message.event.is_some() {
            continue;
        }
        let message = item.message;
        let addressed = message.mentions.iter().any(|name| name == room)
            || owner_room.as_deref() == Some(message.from.as_str())
            || message
                .re
                .as_deref()
                .is_some_and(|parent| message_author_is(paths, parent, room));
        items.push((message, item.body, addressed));
    }
    let unseen = items.len();
    if unseen == 0 {
        return Ok((None, skipped));
    }
    let addressed_count = items.iter().filter(|(_, _, addressed)| *addressed).count();

    // Which messages ride in the receipt. Addressed ones first: they are the
    // reason to read the crossing at all, so ten newer chatter messages must not
    // push one out. Remaining slots go to the newest of the rest. `items` is in
    // id order, so sorting the picked indexes restores newest-last.
    let addressed_at: Vec<usize> = (0..unseen).filter(|&i| items[i].2).collect();
    let other_at: Vec<usize> = (0..unseen).filter(|&i| !items[i].2).collect();
    let take_addressed = addressed_at.len().min(CROSSED_MESSAGE_CAP);
    let mut picked: Vec<usize> = addressed_at[addressed_at.len() - take_addressed..].to_vec();
    let room_left = CROSSED_MESSAGE_CAP - take_addressed;
    picked.extend_from_slice(&other_at[other_at.len().saturating_sub(room_left)..]);
    picked.sort_unstable();

    let messages = picked
        .into_iter()
        .map(|index| {
            let (message, body, addressed) = &items[index];
            // Verified against the complete stored body, before any preview cut.
            let signed_verified = owner
                .as_ref()
                .filter(|owner| message.from == owner.room)
                .and_then(|owner| {
                    crate::mailbox::signed_status(Some(owner), message, body, channel)
                })
                .map(|status| matches!(status, crate::mailbox::SignedStatus::Verified { .. }));
            let body = body.trim_end();
            let body = if *addressed {
                body.to_owned()
            } else {
                preview(body, CROSSED_PREVIEW_CHARS)
            };
            CrossedMessage {
                id: message.id.clone(),
                from: message.from.clone(),
                display_name: message.display_name.clone(),
                sent: message.sent.clone(),
                addressed_to_you: *addressed,
                body,
                signed_verified,
                sender_address: message.sender_address.clone(),
                sender_provenance: message.sender_provenance.clone(),
            }
        })
        .collect();
    Ok((
        Some(Crossed {
            unseen,
            addressed_to_you: addressed_count,
            messages,
        }),
        skipped,
    ))
}

/// `text` cut to `cap` characters, with an ellipsis when it was cut.
fn preview(text: &str, cap: usize) -> String {
    if text.chars().count() <= cap {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(cap).collect();
    cut.push('\u{2026}');
    cut
}

/// Was `id` written by `room`? Used only to decide whether a reply is aimed here.
fn message_author_is(paths: &ChannelPaths, id: &str, room: &str) -> bool {
    let path = paths.messages.join(format!("{id}.msg"));
    parse_channel_message(&path).is_ok_and(|parsed| parsed.message.from == room)
}

/// Resolve a full id or unique prefix within this channel's messages/.
/// Only accepts paths that parse as channel messages whose envelope id matches
/// the filename stem — never trust a bare `.msg` name alone.
pub(crate) fn resolve_message_id(paths: &ChannelPaths, prefix: &str) -> AppResult<String> {
    // A reference printed by `post chat` carries a truncation mark when it is a
    // prefix; strip it so the token post printed resolves when pasted back.
    let prefix = crate::output::unmark_reference(prefix);
    let mut matches = Vec::new();
    for path in message_files(&paths.messages)? {
        let Ok(parsed) = parse_channel_message(&path) else {
            continue;
        };
        let id = parsed.message.id;
        if id == prefix || id.starts_with(prefix) {
            matches.push(id);
        }
    }
    match matches.len() {
        0 => Err(AppError::new(
            ErrorCode::NotFound,
            format!("no message in channel matching id/prefix '{prefix}'"),
            "Pass a full message id or a unique prefix. `post chat <channel> --history 25` lists recent ids, and `post chat <channel> --message <id> --max-bytes 8000` reads one message.",
        )
        .input(prefix)
        .reason("no matching message id")),
        1 => Ok(matches.pop().expect("len 1")),
        _ => {
            matches.sort();
            Err(AppError::new(
                ErrorCode::AmbiguousId,
                format!(
                    "message id/prefix '{prefix}' matches {} messages",
                    matches.len()
                ),
                "Pass a longer unique prefix.",
            )
            .input(prefix)
            .matches(matches)
            .reason("ambiguous message id prefix"))
        }
    }
}

/// Word-boundary `@<room>` matches against registered room names. One
/// longest registered-name match per `@` occurrence so prefix pairs like
/// `foo`/`foo.bar` do not double-stamp. Matching is case-sensitive to the
/// registered spelling.
pub(crate) fn extract_mentions(body: &str, rooms: &RoomMap) -> Vec<String> {
    use std::collections::BTreeSet;
    let mut names: Vec<&String> = rooms.keys().collect();
    names.sort_by_key(|name| std::cmp::Reverse(name.len()));
    let mut found = BTreeSet::new();
    let mut search_from = 0;
    while let Some(rel) = body[search_from..].find('@') {
        let at = search_from + rel;
        if at > 0 {
            let before = body[..at].chars().next_back().unwrap_or('\0');
            if is_mention_boundary_char(before) {
                search_from = at + 1;
                continue;
            }
        }
        let after_at = at + '@'.len_utf8();
        let mut matched: Option<&String> = None;
        for name in &names {
            if !body[after_at..].starts_with(name.as_str()) {
                continue;
            }
            let end = after_at + name.len();
            let ok_end = body[end..]
                .chars()
                .next()
                .is_none_or(|c| !is_mention_boundary_char(c));
            if ok_end {
                matched = Some(name);
                break;
            }
        }
        if let Some(name) = matched {
            found.insert(name.clone());
            // Resume past the whole matched name: a registered name that
            // itself contains `@` (legal) must not re-trigger on its embedded
            // `@` and double-stamp the same token.
            search_from = after_at + name.len();
        } else {
            search_from = at + 1;
        }
    }
    found.into_iter().collect()
}

fn is_mention_boundary_char(c: char) -> bool {
    // Unicode-aware: ASCII-only would treat `é` as a boundary and stamp
    // `@foo` out of `@fooé`. Keep `_`/`-` as name continuations for the
    // registered room-name alphabet.
    c.is_alphanumeric() || c == '_' || c == '-'
}

/// System-line writer for non-join events (currently profile changes).
/// Body is CLI-composed, never user text.
pub(crate) fn write_event(
    context: &Context,
    paths: &ChannelPaths,
    room: &str,
    channel: &str,
    body: &str,
    event: &str,
    provenance: SenderProvenance,
) -> AppResult<String> {
    write_message(
        context,
        paths,
        WriteMessage {
            room,
            channel,
            subject: "",
            body,
            event: Some(event),
            re: None,
            mentions: Vec::new(),
            signature_tag: None,
            provenance,
        },
    )
}

struct WriteMessage<'a> {
    room: &'a str,
    channel: &'a str,
    subject: &'a str,
    body: &'a str,
    event: Option<&'a str>,
    re: Option<String>,
    mentions: Vec<String>,
    signature_tag: Option<&'a str>,
    /// How `room` was resolved; stamped as evidence on the envelope.
    provenance: SenderProvenance,
}

/// Writes one message with a fresh microsecond-resolution id, retrying on
/// the (astronomically rare) same-microsecond hash collision.
fn write_message(
    context: &Context,
    paths: &ChannelPaths,
    opts: WriteMessage<'_>,
) -> AppResult<String> {
    // Send-time stamping: history renders names as they were when the
    // message was sent; renames never rewrite the transcript. Registry
    // values are re-validated at stamp time so a hand-edited profiles.json
    // is inert as an injection path.
    let rooms = context.load_rooms()?;
    let sender_address = crate::mailbox::declared_sender_address()?;
    let actor = context.sender()?;
    let profile = crate::profile::stamp_for(context, &actor.participant.id, opts.room, &rooms);
    if actor.from != opts.room {
        return Err(AppError::new(
            ErrorCode::InvalidArgument,
            format!(
                "channel sender '{}' disagrees with bound participant '{}' reply address '{}'",
                opts.room, actor.participant.id, actor.from
            ),
            "Re-bind with the intended workspace before writing to a channel.",
        ));
    }
    for attempt in 0..256 {
        let (id_timestamp, sent) = local_timestamp_micros()?;
        let id = new_mail_id(&id_timestamp, attempt)?;
        let message = ChannelMessage {
            id: id.clone(),
            from: opts.room.to_owned(),
            channel: opts.channel.to_owned(),
            subject: opts.subject.to_owned(),
            sent,
            from_participant: Some(actor.participant.id.clone()),
            from_host: None,
            from_lineage: actor.lineage.clone(),
            address_kind: Some("channel".to_owned()),
            event: opts.event.map(str::to_owned),
            display_name: profile.name.clone(),
            pfp: profile.pfp.clone(),
            re: opts.re.clone(),
            mentions: opts.mentions.clone(),
            signature_ref: opts
                .signature_tag
                .map(|tag| serde_json::json!({ "version": 2, "tag": tag })),
            sender_address: sender_address.clone(),
            sender_provenance: Some(opts.provenance.as_str().to_owned()),
        };
        validate_channel_message(Path::new("<generated message>"), &message)?;
        let payload = encode_message(&message, opts.body)?;
        let path = paths.messages.join(format!("{id}.msg"));
        match exclusive_atomic_write(&path, &payload) {
            Ok(()) => return Ok(id),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(AppError::io(
                    "exclusively write channel message",
                    &path,
                    error,
                ));
            }
        }
    }
    Err(AppError::new(
        ErrorCode::IoError,
        "could not allocate a unique channel message id after 256 attempts",
        "Retry the same command; if this repeats, run `post doctor`.",
    ))
}

pub(crate) fn encode_message(message: &ChannelMessage, body: &str) -> AppResult<Vec<u8>> {
    let payload = serde_json::to_string_pretty(message).map_err(|error| {
        AppError::new(
            ErrorCode::IoError,
            format!(
                "failed to serialize channel message '{}': {error}",
                message.id
            ),
            "Retry the same command; if this repeats, report `post --version`.",
        )
    })?;
    let mut payload = ascii_escape_json(&payload);
    payload.push_str("\n---\n");
    payload.push_str(body);
    Ok(payload.into_bytes())
}

pub(crate) fn parse_channel_message(path: &Path) -> AppResult<ParsedChannelMessage> {
    let mut raw = String::new();
    File::open(path)
        .and_then(|mut file| file.read_to_string(&mut raw))
        .map_err(|error| AppError::io("read channel message file", path, error))?;
    let (head, body) = raw.split_once("\n---\n").ok_or_else(|| {
        AppError::config(
            path,
            "channel message has no '\\n---\\n' separator; restore a valid .msg file or move it aside",
        )
    })?;
    let message: ChannelMessage = serde_json::from_str(head)
        .map_err(|error| AppError::config(path, format!("malformed message JSON: {error}")))?;
    validate_channel_message(path, &message)?;
    if path.file_stem().and_then(|value| value.to_str()) != Some(message.id.as_str()) {
        return Err(AppError::config(
            path,
            format!(
                "channel message filename must be '{}.msg' to match its id",
                message.id
            ),
        ));
    }
    Ok(ParsedChannelMessage {
        message,
        body: body.to_owned(),
    })
}

pub(crate) fn validate_channel_message(path: &Path, message: &ChannelMessage) -> AppResult<()> {
    for (field, value) in [
        ("id", message.id.as_str()),
        ("from", message.from.as_str()),
        ("channel", message.channel.as_str()),
        ("sent", message.sent.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(AppError::config(
                path,
                format!("channel message field '{field}' is empty"),
            ));
        }
    }
    // The channel field is rendered UNQUOTED in watch text lines ("#name"),
    // so a hand-written .msg with a control character here could forge event
    // lines. The CLI mints only validated names; enforce the same at parse.
    if let Err(reason) = crate::mailbox::validate_component(&message.channel) {
        return Err(AppError::config(
            path,
            format!(
                "channel message field 'channel' ('{}') is not a path-safe name: {reason}",
                message.channel.escape_debug()
            ),
        ));
    }
    if message.channel.chars().any(char::is_control) {
        return Err(AppError::config(
            path,
            "channel message field 'channel' contains control characters",
        ));
    }
    // YYYYmmdd-HHMMSS-UUUUUU-<6 hex>: microsecond resolution keeps
    // lexicographic order ~= arrival order, which the reader's high-water
    // cursor depends on for completeness.
    if !is_canonical_channel_message_id(&message.id) {
        return Err(AppError::config(
            path,
            format!(
                "channel message id '{}' must match YYYYmmdd-HHMMSS-UUUUUU-<6 hex>",
                message.id
            ),
        ));
    }
    // `event` is deliberately not validated against a closed list. Any kind
    // beyond `join` and `profile` is an opaque system event (see
    // `is_opaque_event`): additive fields were always tolerated, and a newer
    // peer or the bridge adding a kind must not wedge every reader on this
    // host (contract 2026-09-28 section 3).
    // Stamped profile fields render unquoted in chat banners and watch
    // text lines; a control character smuggled into a hand-written .msg
    // could forge whole lines, so refuse them at parse like `channel`.
    for (field, value) in [
        ("display_name", &message.display_name),
        ("pfp", &message.pfp),
    ] {
        if let Some(value) = value {
            if value.chars().any(crate::mailbox::refused_profile_char) {
                return Err(AppError::config(
                    path,
                    format!(
                        "channel message field '{field}' contains control, bidi, or line-separator characters"
                    ),
                ));
            }
        }
    }
    // `re` is untrusted store data rendered into text output; it must be a
    // canonical message id so short_id and reply resolution never panic or
    // trust a mismatched filename.
    if let Some(re) = &message.re {
        if !is_canonical_channel_message_id(re) {
            return Err(AppError::config(
                path,
                format!(
                    "channel message field 're' ('{}') must be a canonical channel message id",
                    re.escape_debug()
                ),
            ));
        }
    }
    for mention in &message.mentions {
        if mention.chars().any(crate::mailbox::refused_profile_char)
            || validate_room_name(mention).is_err()
        {
            return Err(AppError::config(
                path,
                format!(
                    "channel message mentions entry '{}' is not a valid room name",
                    mention.escape_debug()
                ),
            ));
        }
    }
    Ok(())
}

/// Canonical channel message id: `YYYYmmdd-HHMMSS-UUUUUU-<6 hex>` (29 bytes, ASCII).
pub(crate) fn is_canonical_channel_message_id(id: &str) -> bool {
    let id = id.as_bytes();
    id.len() == 29
        && id[..8].iter().all(u8::is_ascii_digit)
        && id[8] == b'-'
        && id[9..15].iter().all(u8::is_ascii_digit)
        && id[15] == b'-'
        && id[16..22].iter().all(u8::is_ascii_digit)
        && id[22] == b'-'
        && id[23..].iter().all(u8::is_ascii_hexdigit)
}

/// Find a channel message by full id or unique-enough prefix, across channels.
///
/// `post read` answers a miss by saying the id is "not unread, not already read,
/// not in the archive" and pointing at `post inbox` — two statements that are
/// both true and both useless when the id came from the doorbell, which hands
/// out channel message ids. Channel messages are not mail and never will be in
/// any of those three places, so the miss path has to look where they actually
/// live before claiming the id does not exist.
///
/// A miss is already an error path, so a stat per channel is affordable; the
/// scan stops at the first channel holding a match.
pub(crate) fn find_channel_message(
    context: &Context,
    prefix: &str,
) -> Option<(String, String, usize)> {
    let root = context.root.join(CHANNELS_DIR);
    let mut names: Vec<String> = fs::read_dir(&root)
        .ok()?
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .collect();
    names.sort();
    for channel in names {
        let messages = root.join(&channel).join("messages");
        let Ok(entries) = fs::read_dir(&messages) else {
            continue;
        };
        let mut ids: Vec<String> = entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                if path.extension().and_then(|value| value.to_str()) != Some("msg") {
                    return None;
                }
                path.file_stem()
                    .and_then(|value| value.to_str())
                    .map(str::to_owned)
            })
            .collect();
        // Ids sort chronologically, which is what makes the depth below correct.
        ids.sort();
        if let Some(position) = ids.iter().position(|id| id.starts_with(prefix)) {
            // `--history N` renders the last N messages, so the depth that
            // includes the target is its distance from the end, inclusive.
            // `--since <id>` would NOT do -- it renders messages AFTER the id and
            // so would show everything except the one that was asked about.
            let depth = ids.len() - position;
            return Some((channel, ids[position].clone(), depth));
        }
    }
    None
}

pub(crate) fn message_files(directory: &Path) -> AppResult<Vec<PathBuf>> {
    let mut files = Vec::new();
    let entries = fs::read_dir(directory)
        .map_err(|error| AppError::io("list channel messages directory", directory, error))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| AppError::io("read channel messages entry", directory, error))?;
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|value| value.to_str()) == Some("msg") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

pub(crate) struct ChannelSummary {
    pub info: ChannelInfo,
    pub members: MemberMap,
    pub messages: usize,
    /// Archive mark in force, or None for a live channel.
    pub archived: Option<crate::channel_archive::ArchiveMark>,
}

// The strict listing: every read-only surface here uses `list_channels_with`
// tolerantly, but the fail-closed form stays for callers that must not act on
// a partial picture.
#[allow(dead_code)]
pub(crate) fn list_channels(context: &Context) -> AppResult<Vec<ChannelSummary>> {
    Ok(list_channels_with(context, Scan::Strict)?.0)
}

/// The channel listing with one bad channel directory (an unreadable
/// `channel.json` or `members.json`) skipped and reported instead of failing
/// every listing on the host.
pub(crate) fn list_channels_with(
    context: &Context,
    scan: Scan,
) -> AppResult<(Vec<ChannelSummary>, Vec<SkippedFile>)> {
    let dir = context.root.join(CHANNELS_DIR);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Vec::new(), Vec::new()))
        }
        Err(error) => return Err(AppError::io("list channels directory", &dir, error)),
    };
    let mut summaries = Vec::new();
    let mut skipped = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| AppError::io("read channels entry", &dir, error))?;
        if !entry.path().is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let paths = match ChannelPaths::new(context, &name) {
            Ok(paths) => paths,
            // A stray non-channel directory (or one with an invalid name)
            // is skipped rather than failing the whole listing.
            Err(_) => continue,
        };
        if !paths.exists() {
            continue;
        }
        let messages = match message_files(&paths.messages) {
            Ok(files) => files.len(),
            Err(_) => 0,
        };
        let loaded = paths
            .load_info()
            .map_err(|error| ("channel.json", error))
            .and_then(|info| {
                paths
                    .load_members()
                    .map(|members| (info, members))
                    .map_err(|error| ("members.json", error))
            });
        let (info, members) = match loaded {
            Ok(loaded) => loaded,
            Err((file, error)) if scan == Scan::Tolerant => {
                let reason = error
                    .details
                    .reason
                    .clone()
                    .unwrap_or_else(|| error.message.clone());
                skipped.push(SkippedFile {
                    id: file.to_owned(),
                    reason: one_line(&reason, 300),
                    channel: Some(name),
                });
                continue;
            }
            Err((_, error)) => return Err(error),
        };
        summaries.push(ChannelSummary {
            info,
            members,
            messages,
            // Fail open: an unreadable archive.json lists the channel as live
            // (visible) instead of failing the listing and every watch that
            // enumerates channels. `post doctor` reports the bad file.
            archived: crate::channel_archive::effective_mark(&paths).unwrap_or(None),
        });
    }
    summaries.sort_by(|left, right| left.info.name.cmp(&right.info.name));
    Ok((summaries, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{test_root, trash_test_root};
    use std::fs;

    fn test_context(label: &str, rooms: &str, rules: &str) -> (PathBuf, Context) {
        let root = test_root(&format!("channel-{label}"));
        fs::write(root.join("rooms.json"), rooms).expect("write rooms config");
        fs::write(root.join("rules.json"), rules).expect("write rules config");
        let context = Context {
            root: root.clone(),
            home: root.clone(),
        };
        crate::participant::bind_test_actor(&context, "alpha");
        (root, context)
    }

    fn rooms_json(root: &Path) -> String {
        // Two registered rooms whose trees live under the test root so
        // acting_room can be steered by cwd-independent means: tests call
        // the store primitives directly instead of relying on cwd.
        format!(
            r#"{{"alpha": "{0}/alpha", "beta": "{0}/beta"}}"#,
            root.display()
        )
    }

    #[test]
    fn microsecond_ids_sort_chronologically_and_validate() {
        let (root, _context) = test_context("ids", r#"{"alpha": "/tmp"}"#, r#"{"blocked":[]}"#);
        let (first_ts, _) = local_timestamp_micros().expect("timestamp");
        let first = new_mail_id(&first_ts, 0).expect("id");
        std::thread::sleep(std::time::Duration::from_millis(2));
        let (second_ts, _) = local_timestamp_micros().expect("timestamp");
        let second = new_mail_id(&second_ts, 0).expect("id");
        assert!(first < second, "{first} must sort before {second}");
        let message = ChannelMessage {
            id: first.clone(),
            from: "alpha".to_owned(),
            channel: "tax".to_owned(),
            subject: String::new(),
            sent: "2026-07-22 01:00:00 -0500".to_owned(),
            from_participant: None,
            from_host: None,
            from_lineage: None,
            address_kind: None,
            event: None,
            display_name: None,
            pfp: None,
            re: None,
            mentions: vec![],
            signature_ref: None,
            sender_address: None,
            sender_provenance: None,
        };
        validate_channel_message(Path::new("<test>"), &message).expect("valid id shape");
        trash_test_root(&root);
    }

    #[test]
    fn control_characters_in_channel_field_are_refused_at_parse() {
        // watch text lines print "#<channel>" unquoted; a newline smuggled
        // into a hand-written .msg envelope must die in the validator.
        let message = ChannelMessage {
            id: "20260722-013000-000001-abc123".to_owned(),
            from: "alpha".to_owned(),
            channel: "tax\nFORGED".to_owned(),
            subject: String::new(),
            sent: "2026-07-22 01:30:00 -0500".to_owned(),
            from_participant: None,
            from_host: None,
            from_lineage: None,
            address_kind: None,
            event: None,
            display_name: None,
            pfp: None,
            re: None,
            mentions: vec![],
            signature_ref: None,
            sender_address: None,
            sender_provenance: None,
        };
        let error = validate_channel_message(Path::new("<test>"), &message)
            .expect_err("control characters in channel must be refused");
        assert_eq!(error.code.as_str(), "config_invalid");
    }

    #[test]
    fn second_resolution_mail_id_is_refused_for_channel_messages() {
        let message = ChannelMessage {
            id: "20260722-012000-abcdef".to_owned(),
            from: "alpha".to_owned(),
            channel: "tax".to_owned(),
            subject: String::new(),
            sent: "2026-07-22 01:20:00 -0500".to_owned(),
            from_participant: None,
            from_host: None,
            from_lineage: None,
            address_kind: None,
            event: None,
            display_name: None,
            pfp: None,
            re: None,
            mentions: vec![],
            signature_ref: None,
            sender_address: None,
            sender_provenance: None,
        };
        let error = validate_channel_message(Path::new("<test>"), &message)
            .expect_err("second-resolution ids must be refused");
        assert_eq!(error.code.as_str(), "config_invalid");
    }

    #[test]
    fn join_records_event_then_membership_and_send_roundtrips() {
        let (root, context) = test_context("join", "{}", r#"{"blocked":[]}"#);
        fs::write(root.join("rooms.json"), rooms_json(&root)).expect("rooms");
        fs::create_dir_all(root.join("alpha")).expect("alpha tree");
        let paths = ChannelPaths::new(&context, "tax").expect("paths");
        fs::create_dir_all(&paths.messages).expect("messages dir");

        // Drive the store primitives directly (acting_room is cwd-derived
        // and tests must not depend on process cwd).
        let event_id = write_message(
            &context,
            &paths,
            WriteMessage {
                room: "alpha",
                channel: "tax",
                subject: "",
                body: "=== alpha joined ===",
                event: Some(JOIN_EVENT),
                re: None,
                mentions: Vec::new(),
                signature_tag: None,
                provenance: SenderProvenance::InferredCwd,
            },
        )
        .expect("join event");
        let mut members = MemberMap::new();
        members.insert("alpha".to_owned(), "2026-07-22 01:00:00 -0500".to_owned());
        let bytes = serde_json::to_vec_pretty(&members).expect("members json");
        atomic_replace(&paths.members_json, &bytes).expect("members write");

        let message_id = write_message(
            &context,
            &paths,
            WriteMessage {
                room: "alpha",
                channel: "tax",
                subject: "hello",
                body: "first message",
                event: None,
                re: None,
                mentions: Vec::new(),
                signature_tag: None,
                provenance: SenderProvenance::InferredCwd,
            },
        )
        .expect("send");
        assert!(
            event_id < message_id,
            "event must sort before the later send"
        );

        let files = message_files(&paths.messages).expect("list");
        assert_eq!(files.len(), 2);
        let parsed = parse_channel_message(&files[1]).expect("parse");
        assert_eq!(parsed.message.from, "alpha");
        assert_eq!(parsed.message.channel, "tax");
        assert_eq!(parsed.message.event, None);
        assert_eq!(parsed.body, "first message");
        let event = parse_channel_message(&files[0]).expect("parse event");
        assert_eq!(event.message.event.as_deref(), Some(JOIN_EVENT));
        trash_test_root(&root);
    }

    #[test]
    fn send_stamps_profile_as_of_send_time_and_rename_does_not_retcon() {
        let (root, context) = test_context("stamp", "{}", r#"{"blocked":[]}"#);
        fs::write(root.join("rooms.json"), rooms_json(&root)).expect("rooms");
        let actor = context.sender().expect("test actor").participant.id;
        let key = crate::profile::participant_key(&actor);
        fs::write(
            root.join("profiles.json"),
            format!(r#"{{"{key}": {{"name": "Lantern", "pfp": "🏮"}}}}"#),
        )
        .expect("profiles");
        let paths = ChannelPaths::new(&context, "tax").expect("paths");
        fs::create_dir_all(&paths.messages).expect("messages dir");

        let first = write_message(
            &context,
            &paths,
            WriteMessage {
                room: "alpha",
                channel: "tax",
                subject: "",
                body: "hi",
                event: None,
                re: None,
                mentions: Vec::new(),
                signature_tag: None,
                provenance: SenderProvenance::InferredCwd,
            },
        )
        .expect("send");
        // Rename after the first send; the stored first message must keep
        // the old name (history renders as-sent).
        fs::write(
            root.join("profiles.json"),
            format!(r#"{{"{key}": {{"name": "Coldwell", "pfp": "🏮"}}}}"#),
        )
        .expect("rename");
        let second = write_message(
            &context,
            &paths,
            WriteMessage {
                room: "alpha",
                channel: "tax",
                subject: "",
                body: "yo",
                event: None,
                re: None,
                mentions: Vec::new(),
                signature_tag: None,
                provenance: SenderProvenance::InferredCwd,
            },
        )
        .expect("send");

        let parse = |id: &str| {
            parse_channel_message(&paths.messages.join(format!("{id}.msg"))).expect("parse")
        };
        assert_eq!(
            parse(&first).message.display_name.as_deref(),
            Some("Lantern")
        );
        assert_eq!(
            parse(&second).message.display_name.as_deref(),
            Some("Coldwell")
        );
        assert_eq!(parse(&second).message.pfp.as_deref(), Some("🏮"));
        trash_test_root(&root);
    }

    #[test]
    fn blocked_route_bars_shared_membership_in_both_directions() {
        let (root, context) = test_context("blocked", "{}", r#"{"blocked":[]}"#);
        fs::write(root.join("rooms.json"), rooms_json(&root)).expect("rooms");
        fs::write(
            root.join("rules.json"),
            r#"{"blocked":[{"from":"*","to":"beta","reason":"armed instrument"}]}"#,
        )
        .expect("rules");
        let rooms = context.load_rooms().expect("load rooms");
        let rules = context.load_rules(&rooms).expect("load rules");

        // beta is already a member; alpha joining must be refused because
        // alpha -> beta is blocked, even though beta -> alpha is not.
        let members: MemberMap = [("beta".to_owned(), "t".to_owned())].into_iter().collect();
        let refused = members.keys().any(|member| {
            rules.blocked.iter().any(|rule| {
                rule.matches_route("alpha", member) || rule.matches_route(member, "alpha")
            })
        });
        assert!(refused, "pairwise check must catch the alpha->beta block");
        trash_test_root(&root);
    }

    #[test]
    fn list_channels_skips_strays_and_counts_messages() {
        let (root, context) = test_context("list", "{}", r#"{"blocked":[]}"#);
        fs::write(root.join("rooms.json"), rooms_json(&root)).expect("rooms");
        let paths = ChannelPaths::new(&context, "tax").expect("paths");
        fs::create_dir_all(&paths.messages).expect("messages dir");
        let info = ChannelInfo {
            name: "tax".to_owned(),
            created: "2026-07-22 01:00:00 -0500".to_owned(),
            created_by: "alpha".to_owned(),
            description: None,
        };
        let bytes = serde_json::to_vec_pretty(&info).expect("info json");
        atomic_replace(&paths.channel_json, &bytes).expect("info write");
        write_message(
            &context,
            &paths,
            WriteMessage {
                room: "alpha",
                channel: "tax",
                subject: "",
                body: "hi",
                event: None,
                re: None,
                mentions: Vec::new(),
                signature_tag: None,
                provenance: SenderProvenance::InferredCwd,
            },
        )
        .expect("send");
        // A stray directory without channel.json must not break the listing.
        fs::create_dir_all(root.join(CHANNELS_DIR).join("not-a-channel")).expect("stray");

        let summaries = list_channels(&context).expect("list");
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].info.name, "tax");
        assert_eq!(summaries[0].messages, 1);
        trash_test_root(&root);
    }

    #[test]
    fn embedded_at_in_registered_name_does_not_double_stamp() {
        use crate::model::RoomMap;
        use std::collections::BTreeMap;
        let mut rooms: RoomMap = BTreeMap::new();
        rooms.insert("foo.@bar".to_owned(), "/tmp".into());
        rooms.insert("bar".to_owned(), "/tmp".into());
        // One token, one mention: the embedded `@` inside the matched longer
        // name must not re-trigger and also stamp `bar`.
        assert_eq!(
            extract_mentions("@foo.@bar one token", &rooms),
            vec!["foo.@bar".to_owned()]
        );
        // A standalone `@bar` still stamps normally.
        assert_eq!(
            extract_mentions("plain @bar here", &rooms),
            vec!["bar".to_owned()]
        );
    }

    #[test]
    fn extract_mentions_takes_longest_registered_name_per_at() {
        use crate::model::RoomMap;
        use std::collections::BTreeMap;
        let mut rooms: RoomMap = BTreeMap::new();
        for name in [
            "foo",
            "foo.bar",
            "baz",
            "baz+qux",
            "café",
            "café.x",
            "claude",
            "claude-space",
        ] {
            rooms.insert(name.to_owned(), "/tmp".into());
        }
        assert_eq!(
            extract_mentions("ping @foo.bar please", &rooms),
            vec!["foo.bar".to_owned()]
        );
        assert_eq!(
            extract_mentions("see @baz+qux", &rooms),
            vec!["baz+qux".to_owned()]
        );
        assert_eq!(
            extract_mentions("hi @café.x", &rooms),
            vec!["café.x".to_owned()]
        );
        // Existing hyphen case: longer name wins; shorter must not also stamp.
        assert_eq!(
            extract_mentions("hey @claude-space", &rooms),
            vec!["claude-space".to_owned()]
        );
        assert_eq!(
            extract_mentions("hey @claude please", &rooms),
            vec!["claude".to_owned()]
        );
        // Non-ASCII letters are word characters: `@fooé` is not `@foo`, and
        // a leading `é` does not open a mention either.
        assert!(
            extract_mentions("ping @fooé please", &rooms).is_empty(),
            "@fooé must not stamp room foo"
        );
        assert!(
            extract_mentions("é@foo trailing", &rooms).is_empty(),
            "é@foo must not stamp: leading letter is a word char"
        );
        assert_eq!(
            extract_mentions("hi @café thanks", &rooms),
            vec!["café".to_owned()],
            "registered Unicode room names still stamp"
        );
    }

    #[test]
    fn malformed_re_is_refused_at_parse() {
        let message = ChannelMessage {
            id: "20260722-013000-000001-abc123".to_owned(),
            from: "alpha".to_owned(),
            channel: "tax".to_owned(),
            subject: String::new(),
            sent: "2026-07-22 01:30:00 -0500".to_owned(),
            from_participant: None,
            from_host: None,
            from_lineage: None,
            address_kind: None,
            event: None,
            display_name: None,
            pfp: None,
            re: Some("aaaaaaaéx".to_owned()),
            mentions: vec![],
            signature_ref: None,
            sender_address: None,
            sender_provenance: None,
        };
        let error = validate_channel_message(Path::new("<test>"), &message)
            .expect_err("non-canonical re must be refused");
        assert_eq!(error.code.as_str(), "config_invalid");
    }

    #[test]
    fn participant_review_unit_channel_send_stamps_actor_fields() {
        let (root, context) = test_context("participant-fields", "{}", r#"{"blocked":[]}"#);
        fs::write(root.join("rooms.json"), rooms_json(&root)).expect("rooms");
        let paths = ChannelPaths::new(&context, "tax").expect("paths");
        fs::create_dir_all(&paths.messages).expect("messages dir");
        let id = write_message(
            &context,
            &paths,
            WriteMessage {
                room: "alpha",
                channel: "tax",
                subject: "",
                body: "body",
                event: None,
                re: None,
                mentions: Vec::new(),
                signature_tag: None,
                provenance: SenderProvenance::InferredCwd,
            },
        )
        .expect("channel send");
        let raw = fs::read_to_string(paths.messages.join(format!("{id}.msg"))).expect("message");
        let head = raw.split_once("\n---\n").expect("separator").0;
        let message: serde_json::Value = serde_json::from_str(head).expect("message JSON");
        assert!(message["from_participant"].is_string());
        assert_eq!(message["address_kind"], "channel");
        trash_test_root(&root);
    }
}
