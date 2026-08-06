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

pub(crate) fn join(context: &Context, channel: &str) -> AppResult<JoinOutcome> {
    let rooms = context.load_rooms()?;
    let room = acting_room(context, &rooms)?;
    let paths = ChannelPaths::new(context, channel)?;
    let _lock = lock_channels(context)?;

    let mut members = paths.load_members()?;
    if members.contains_key(&room) {
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
        false
    } else {
        let info = ChannelInfo {
            name: channel.to_owned(),
            created: sent.clone(),
            created_by: room.clone(),
        };
        let mut bytes = serde_json::to_vec_pretty(&info).map_err(|error| {
            AppError::io(
                "serialize channel info",
                &paths.channel_json,
                std::io::Error::new(std::io::ErrorKind::InvalidData, error),
            )
        })?;
        bytes.push(b'\n');
        atomic_replace(&paths.channel_json, &bytes)
            .map_err(|error| AppError::io("write channel info", &paths.channel_json, error))?;
        true
    };

    // Event first, membership second: a crash between the two leaves a
    // visible join event without membership, which the next join attempt
    // repairs (at worst a duplicate event). The other order leaves a
    // member whose join the history never shows — a permanent hole.
    let event_id = write_message(
        context,
        &paths,
        &room,
        channel,
        "",
        &format!("=== {room} joined ==="),
        Some(JOIN_EVENT),
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

pub(crate) fn send(
    context: &Context,
    channel: &str,
    subject: &str,
    body: &str,
) -> AppResult<ChannelMessage> {
    let rooms = context.load_rooms()?;
    let room = acting_room(context, &rooms)?;
    let paths = ChannelPaths::new(context, channel)?;
    if !paths.exists() {
        return Err(AppError::new(
            ErrorCode::NotFound,
            format!("channel '{channel}' does not exist"),
            format!("Create it with `post chat {channel} --join`, then retry the send."),
        )
        .input(channel)
        .reason("no channel.json under the channels directory"));
    }
    let members = paths.load_members()?;
    if !members.contains_key(&room) {
        return Err(AppError::new(
            ErrorCode::NotAMember,
            format!("room '{room}' is not a member of channel '{channel}'"),
            format!("Join first with `post chat {channel} --join`, then retry the send."),
        )
        .input(room)
        .reason("sender is absent from members.json"));
    }
    if body.trim().is_empty() {
        return Err(AppError::new(
            ErrorCode::EmptyBody,
            "message body is empty after trimming whitespace",
            format!("Retry with `post chat {channel} --send --body '<text>'` or a non-empty FILE/stdin."),
        )
        .input("message body")
        .reason("empty or whitespace-only"));
    }
    let id = write_message(context, &paths, &room, channel, subject, body, None)?;
    let file = paths.messages.join(format!("{id}.msg"));
    Ok(parse_channel_message(&file)?.message)
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
    write_message(context, paths, room, channel, "", body, Some(event))
}

/// Writes one message with a fresh microsecond-resolution id, retrying on
/// the (astronomically rare) same-microsecond hash collision.
fn write_message(
    context: &Context,
    paths: &ChannelPaths,
    room: &str,
    channel: &str,
    subject: &str,
    body: &str,
    event: Option<&str>,
) -> AppResult<String> {
    // Send-time stamping: history renders names as they were when the
    // message was sent; renames never rewrite the transcript. Registry
    // values are re-validated at stamp time so a hand-edited profiles.json
    // is inert as an injection path.
    let rooms = context.load_rooms()?;
    let profile = crate::profile::stamp_for(context, room, &rooms);
    for attempt in 0..256 {
        let (id_timestamp, sent) = local_timestamp_micros()?;
        let id = new_mail_id(&id_timestamp, attempt)?;
        let message = ChannelMessage {
            id: id.clone(),
            from: room.to_owned(),
            channel: channel.to_owned(),
            subject: subject.to_owned(),
            sent,
            event: event.map(str::to_owned),
            display_name: profile.name.clone(),
            pfp: profile.pfp.clone(),
        };
        validate_channel_message(Path::new("<generated message>"), &message)?;
        let payload = encode_message(&message, body)?;
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
    let id = message.id.as_bytes();
    let valid_id = id.len() == 29
        && id[..8].iter().all(u8::is_ascii_digit)
        && id[8] == b'-'
        && id[9..15].iter().all(u8::is_ascii_digit)
        && id[15] == b'-'
        && id[16..22].iter().all(u8::is_ascii_digit)
        && id[22] == b'-'
        && id[23..].iter().all(u8::is_ascii_hexdigit);
    if !valid_id {
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
    Ok(())
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
            "alpha",
            "tax",
            "",
            "=== alpha joined ===",
            Some(JOIN_EVENT),
        )
        .expect("join event");
        let mut members = MemberMap::new();
        members.insert("alpha".to_owned(), "2026-07-22 01:00:00 -0500".to_owned());
        let bytes = serde_json::to_vec_pretty(&members).expect("members json");
        atomic_replace(&paths.members_json, &bytes).expect("members write");

        let message_id = write_message(
            &context,
            &paths,
            "alpha",
            "tax",
            "hello",
            "first message",
            None,
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

        let first = write_message(&context, &paths, "alpha", "tax", "", "hi", None).expect("send");
        // Rename after the first send; the stored first message must keep
        // the old name (history renders as-sent).
        fs::write(
            root.join("profiles.json"),
            r#"{"alpha": {"name": "Coldwell", "pfp": "🏮"}}"#,
        )
        .expect("rename");
        let second = write_message(&context, &paths, "alpha", "tax", "", "yo", None).expect("send");

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
        };
        let bytes = serde_json::to_vec_pretty(&info).expect("info json");
        atomic_replace(&paths.channel_json, &bytes).expect("info write");
        write_message(&context, &paths, "alpha", "tax", "", "hi", None).expect("send");
        // A stray directory without channel.json must not break the listing.
        fs::create_dir_all(root.join(CHANNELS_DIR).join("not-a-channel")).expect("stray");

        let summaries = list_channels(&context).expect("list");
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].info.name, "tax");
        assert_eq!(summaries[0].messages, 1);
        trash_test_root(&root);
    }
}
