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
use crate::model::{ChannelMessage, ParsedChannelMessage, RoomMap};
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

pub(crate) type MemberMap = BTreeMap<String, String>;

/// Reader-owned cursor map, channel name -> last-read message id. Lives in
/// the READER's room tree (root/<room>/channel-state.json), never in the
/// channel dir: the channel tree stays append-only-by-senders, and a lost
/// or corrupt cursor can only ever hurt its own room.
#[allow(dead_code)] // consumed by the read/cursor lane's patch (contract 013246 item 4)
pub(crate) type ChannelStateMap = BTreeMap<String, String>;

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

/// One lock for all membership mutation across every channel: joins are
/// rare and human-paced, so a global lock is simpler than per-channel
/// locks and cannot deadlock. Message sends never take it — exclusive
/// file creation is their arbiter.
fn lock_channels(context: &Context) -> AppResult<File> {
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

/// Resolve the acting room for channel operations. Identity is always
/// inferred from cwd — there is deliberately no --from/--room override on
/// channel commands, so the only way to act as a room is from inside its
/// tree. Membership additionally requires the room to be registered:
/// cursors and join records need a durable identity, and the cwd-basename
/// fallback is not one.
pub(crate) fn acting_room(context: &Context, rooms: &RoomMap) -> AppResult<String> {
    let room = context.infer_from_cwd(rooms)?;
    if rooms.contains_key(&room) {
        return Ok(room);
    }
    Err(AppError::new(
        ErrorCode::UnknownRoom,
        format!(
            "channel operations require a registered room; cwd resolves to '{room}', which is not in rooms.json"
        ),
        "Run from inside a registered room tree, or register one with `post rooms add <name> <path>`.",
    )
    .input(room)
    .reason("cwd is outside every registered room tree"))
}

pub(crate) struct JoinOutcome {
    pub room: String,
    pub channel_created: bool,
    pub already_member: bool,
    pub event_id: Option<String>,
}

pub(crate) fn join(
    context: &Context,
    channel: &str,
    description: Option<&str>,
) -> AppResult<JoinOutcome> {
    let rooms = context.load_rooms()?;
    let room = acting_room(context, &rooms)?;
    let paths = ChannelPaths::new(context, channel)?;
    let _lock = lock_channels(context)?;

    if let Some(description) = description {
        validate_description(description)?;
    }

    let mut members = paths.load_members()?;
    if members.contains_key(&room) {
        // Already a member: --description still updates the norms carrier.
        if let Some(description) = description {
            write_description(&paths, description)?;
        }
        return Ok(JoinOutcome {
            room,
            channel_created: false,
            already_member: true,
            event_id: None,
        });
    }

    // Blocked routes bar shared membership, both directions, before any
    // state is written. Checked under the lock so a concurrent join of the
    // blocked counterpart cannot slip in between check and write.
    let rules = context.load_rules(&rooms)?;
    for member in members.keys() {
        if let Some(rule) = rules
            .blocked
            .iter()
            .find(|rule| rule.matches_route(&room, member) || rule.matches_route(member, &room))
        {
            return Err(AppError::new(
                ErrorCode::BlockedRoute,
                format!(
                    "joining '{channel}' would put '{room}' and existing member '{member}' in one channel, and that route is blocked: {}",
                    rule.reason
                ),
                "Do not route around this block. Ask the human operator to review rules.json.",
            )
            .input(format!("{room} <-> {member}"))
            .reason(rule.reason.clone())
            .rule(rule.clone()));
        }
    }

    fs::create_dir_all(&paths.messages).map_err(|error| {
        AppError::io("create channel messages directory", &paths.messages, error)
    })?;

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
        },
    )?;
    members.insert(room.clone(), sent);
    let mut bytes = serde_json::to_vec_pretty(&members).map_err(|error| {
        AppError::io(
            "serialize channel members",
            &paths.members_json,
            std::io::Error::new(std::io::ErrorKind::InvalidData, error),
        )
    })?;
    bytes.push(b'\n');
    atomic_replace(&paths.members_json, &bytes)
        .map_err(|error| AppError::io("write channel members", &paths.members_json, error))?;

    Ok(JoinOutcome {
        room,
        channel_created,
        already_member: false,
        event_id: Some(event_id),
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
    pub anyway: bool,
    pub re: Option<&'a str>,
}

pub(crate) fn send(
    context: &Context,
    channel: &str,
    options: SendOptions<'_>,
) -> AppResult<ChannelMessage> {
    let rooms = context.load_rooms()?;
    let room = acting_room(context, &rooms)?;
    let paths = ChannelPaths::new(context, channel)?;
    let quoted = crate::mailbox::shell_quote(channel);
    if !paths.exists() {
        return Err(AppError::new(
            ErrorCode::NotFound,
            format!("channel '{channel}' does not exist"),
            format!("Create it with `post chat {quoted} --join`, then retry the send."),
        )
        .input(channel)
        .reason("no channel.json under the channels directory"));
    }
    let members = paths.load_members()?;
    if !members.contains_key(&room) {
        return Err(AppError::new(
            ErrorCode::NotAMember,
            format!("room '{room}' is not a member of channel '{channel}'"),
            format!("Join first with `post chat {quoted} --join`, then retry the send."),
        )
        .input(room)
        .reason("sender is absent from members.json"));
    }
    if options.body.trim().is_empty() {
        return Err(AppError::new(
            ErrorCode::EmptyBody,
            "message body is empty after trimming whitespace",
            format!(
                "Retry with `post chat {quoted} --send --body '<text>'` or a non-empty FILE/stdin."
            ),
        )
        .input("message body")
        .reason("empty or whitespace-only"));
    }

    // Crossed-send bounce: humans see incoming while typing; agents get the
    // equivalent at the send point. Check-then-append has a TOCTOU window
    // (another room can land a message between check and exclusive create);
    // that occasional slip is accepted. Corrupting the store is not.
    if !options.anyway {
        if let Some(error) = crossed_send_bounce(context, &paths, channel, &room)? {
            return Err(error);
        }
    }

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
        },
    )?;
    let file = paths.messages.join(format!("{id}.msg"));
    Ok(parse_channel_message(&file)?.message)
}

/// Unread messages from other rooms past the sender's cursor. Empty = clear
/// to send. Bounded to the last 10 for the bounce payload.
fn crossed_send_bounce(
    context: &Context,
    paths: &ChannelPaths,
    channel: &str,
    room: &str,
) -> AppResult<Option<AppError>> {
    use crate::channel_state::ChannelState;
    use crate::error::MissedChannelMessage;

    let state = ChannelState::load(context, room)?;
    let cursor = state.cursor(channel);
    let mut missed = Vec::new();
    for path in message_files(&paths.messages)? {
        let parsed = match parse_channel_message(&path) {
            Ok(parsed) => parsed,
            Err(_) => continue,
        };
        let unread = cursor.is_none_or(|last| parsed.message.id.as_str() > last);
        // System events (join/profile) are not conversation the sender needs
        // to revise against; bounce only on ordinary messages from others.
        if !unread || parsed.message.from == room || parsed.message.event.is_some() {
            continue;
        }
        missed.push(MissedChannelMessage {
            id: parsed.message.id,
            from: parsed.message.from,
            subject: parsed.message.subject,
            sent: parsed.message.sent,
            body: parsed.body,
        });
    }
    if missed.is_empty() {
        return Ok(None);
    }
    let total = missed.len();
    if missed.len() > 10 {
        missed = missed.split_off(missed.len() - 10);
    }
    let fix = format!(
        "post chat {} --send --anyway --body '<revised text>'",
        crate::mailbox::shell_quote(channel)
    );
    Ok(Some(
        AppError::new(
            ErrorCode::CrossedSend,
            format!(
                "channel '{channel}' has {total} unread message(s) past your cursor; send was not delivered (showing the last {})",
                missed.len()
            ),
            format!(
                "Read the missed messages, revise, then retry with `--anyway` to deliver regardless: `{fix}`."
            ),
        )
        .exact_fix(fix)
        .input(channel)
        .reason("channel tip advanced past sender cursor")
        .missed(missed),
    ))
}

/// Resolve a full id or unique prefix within this channel's messages/.
/// Only accepts paths that parse as channel messages whose envelope id matches
/// the filename stem — never trust a bare `.msg` name alone.
pub(crate) fn resolve_message_id(paths: &ChannelPaths, prefix: &str) -> AppResult<String> {
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
            "Pass a full message id or a unique prefix from `post chat <channel> --history`.",
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
        }
        search_from = at + 1;
    }
    found.into_iter().collect()
}

fn is_mention_boundary_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
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
    let profile = crate::profile::stamp_for(context, opts.room, &rooms);
    for attempt in 0..256 {
        let (id_timestamp, sent) = local_timestamp_micros()?;
        let id = new_mail_id(&id_timestamp, attempt)?;
        let message = ChannelMessage {
            id: id.clone(),
            from: opts.room.to_owned(),
            channel: opts.channel.to_owned(),
            subject: opts.subject.to_owned(),
            sent,
            event: opts.event.map(str::to_owned),
            display_name: profile.name.clone(),
            pfp: profile.pfp.clone(),
            re: opts.re.clone(),
            mentions: opts.mentions.clone(),
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
    if let Some(event) = &message.event {
        if event != JOIN_EVENT && event != PROFILE_EVENT {
            return Err(AppError::config(
                path,
                format!(
                    "channel message event '{event}' is unknown; only '{JOIN_EVENT}' and '{PROFILE_EVENT}' exist"
                ),
            ));
        }
    }
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
}

pub(crate) fn list_channels(context: &Context) -> AppResult<Vec<ChannelSummary>> {
    let dir = context.root.join(CHANNELS_DIR);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(AppError::io("list channels directory", &dir, error)),
    };
    let mut summaries = Vec::new();
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
        summaries.push(ChannelSummary {
            info: paths.load_info()?,
            members: paths.load_members()?,
            messages,
        });
    }
    summaries.sort_by(|left, right| left.info.name.cmp(&right.info.name));
    Ok(summaries)
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
        (
            root.clone(),
            Context {
                root: root.clone(),
                home: root,
            },
        )
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
            event: None,
            display_name: None,
            pfp: None,
            re: None,
            mentions: vec![],
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
            event: None,
            display_name: None,
            pfp: None,
            re: None,
            mentions: vec![],
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
            event: None,
            display_name: None,
            pfp: None,
            re: None,
            mentions: vec![],
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
        fs::write(
            root.join("profiles.json"),
            r#"{"alpha": {"name": "Lantern", "pfp": "🏮"}}"#,
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
            },
        )
        .expect("send");
        // Rename after the first send; the stored first message must keep
        // the old name (history renders as-sent).
        fs::write(
            root.join("profiles.json"),
            r#"{"alpha": {"name": "Coldwell", "pfp": "🏮"}}"#,
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
    }

    #[test]
    fn malformed_re_is_refused_at_parse() {
        let message = ChannelMessage {
            id: "20260722-013000-000001-abc123".to_owned(),
            from: "alpha".to_owned(),
            channel: "tax".to_owned(),
            subject: String::new(),
            sent: "2026-07-22 01:30:00 -0500".to_owned(),
            event: None,
            display_name: None,
            pfp: None,
            re: Some("aaaaaaaéx".to_owned()),
            mentions: vec![],
        };
        let error = validate_channel_message(Path::new("<test>"), &message)
            .expect_err("non-canonical re must be refused");
        assert_eq!(error.code.as_str(), "config_invalid");
    }
}
