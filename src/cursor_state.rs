//! Per-participant consumption state plus a read-only legacy room reader.

#[path = "eligibility.rs"]
pub(crate) mod eligibility;
#[path = "routing.rs"]
pub(crate) mod routing;

use crate::channel::{self, CHANNELS_DIR};
use crate::error::{AppError, AppResult, ErrorCode};
use crate::mailbox::{atomic_replace, exclusive_move, Context, MoveError};
use crate::participant::{Address, Participant};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub(crate) const CURSORS_FILE: &str = "cursors.json";
pub(crate) const CURSORS_LOCK_FILE: &str = ".cursors.lock";
pub(crate) const STATE_VERSION: u64 = 1;
pub(crate) const SEEN_SET_WARN: usize = 50_000;

const PARTICIPANT_STATE_VERSION: u64 = 2;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ParticipantCursors {
    mail: BTreeMap<String, BTreeSet<String>>,
    channels: BTreeMap<String, BTreeSet<String>>,
}

impl ParticipantCursors {
    /// Missing or malformed participant state is an empty snapshot for reads.
    /// Writers reload under the lock and refuse malformed state rather than
    /// repairing it by discarding evidence.
    pub(crate) fn load(_context: &Context, participant: &Participant) -> Self {
        match read_participant_cursor(&participant_cursor_path(participant)) {
            ParticipantCursorRead::Missing => Self::default(),
            ParticipantCursorRead::Valid(state) => state,
            ParticipantCursorRead::Invalid => {
                eprintln!(
                    "post: warning: participant '{}' has invalid cursors.json; treating every eligible message as unread",
                    participant.id
                );
                Self::default()
            }
        }
    }

    pub(crate) fn mail_has_seen(&self, address: &Address, id: &str) -> bool {
        self.mail
            .get(&mail_key(address))
            .is_some_and(|seen| seen.contains(id))
    }

    pub(crate) fn channel_has_seen(&self, channel: &str, id: &str) -> bool {
        self.channels
            .get(channel)
            .is_some_and(|seen| seen.contains(id))
    }

    pub(crate) fn consume_mail(
        context: &Context,
        participant: &Participant,
        address: &Address,
        ids: &[String],
    ) -> AppResult<usize> {
        for id in ids {
            validate_mail_id(id)?;
        }
        update_participant(context, participant, |state| {
            let seen = state.mail.entry(mail_key(address)).or_default();
            let before = seen.len();
            seen.extend(ids.iter().cloned());
            Ok(seen.len() - before)
        })
    }

    pub(crate) fn consume_channel(
        context: &Context,
        participant: &Participant,
        channel: &str,
        ids: &[String],
    ) -> AppResult<CursorAdvance> {
        channel::validate_channel_name(channel)?;
        for id in ids {
            validate_channel_id(id)?;
        }
        update_participant(context, participant, |state| {
            let seen = state.channels.entry(channel.to_owned()).or_default();
            let prior = seen.last().cloned();
            let before = seen.len();
            seen.extend(ids.iter().cloned());
            let marked = seen.len() - before;
            let cursor = seen.last().cloned().or(prior.clone()).unwrap_or_default();
            Ok(CursorAdvance {
                prior,
                cursor,
                advanced: marked > 0,
                marked,
            })
        })
    }

    pub(crate) fn consume_channel_through(
        context: &Context,
        participant: &Participant,
        channel: &str,
        target: &str,
    ) -> AppResult<CursorAdvance> {
        channel::validate_channel_name(channel)?;
        validate_channel_id(target)?;
        update_participant(context, participant, |state| {
            let seen = state.channels.entry(channel.to_owned()).or_default();
            let prior = seen.last().cloned();
            let candidates = unseen_candidates(context, channel, seen, Some(target))?;
            let marked = candidates.len();
            seen.extend(candidates);
            let cursor = seen.last().cloned().or(prior.clone()).unwrap_or_default();
            Ok(CursorAdvance {
                prior,
                cursor,
                advanced: marked > 0,
                marked,
            })
        })
    }
}

fn validate_mail_id(id: &str) -> AppResult<()> {
    if is_canonical_mail_id(id) {
        Ok(())
    } else {
        Err(AppError::invalid_argument(format!(
            "mail cursor id '{id}' is not canonical"
        )))
    }
}

fn validate_channel_id(id: &str) -> AppResult<()> {
    if channel::is_canonical_channel_message_id(id) {
        Ok(())
    } else {
        Err(AppError::invalid_argument(format!(
            "channel cursor id '{id}' is not canonical"
        )))
    }
}

fn mail_key(address: &Address) -> String {
    format!("{}:{}", address.kind.as_str(), address.name)
}

fn participant_cursor_path(participant: &Participant) -> PathBuf {
    participant.dir.join(CURSORS_FILE)
}

enum ParticipantCursorRead {
    Missing,
    Valid(ParticipantCursors),
    Invalid,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ParticipantCursorDocument {
    version: u64,
    mail: BTreeMap<String, CursorSet>,
    channels: BTreeMap<String, CursorSet>,
}

fn read_participant_cursor(path: &Path) -> ParticipantCursorRead {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return ParticipantCursorRead::Missing;
        }
        Err(_) => return ParticipantCursorRead::Invalid,
    };
    if !metadata.file_type().is_file() || metadata.nlink() != 1 {
        return ParticipantCursorRead::Invalid;
    }
    let raw = match fs::read(path) {
        Ok(raw) => raw,
        Err(_) => return ParticipantCursorRead::Invalid,
    };
    parse_participant_cursor(&raw)
        .map(ParticipantCursorRead::Valid)
        .unwrap_or(ParticipantCursorRead::Invalid)
}

fn parse_participant_cursor(raw: &[u8]) -> Result<ParticipantCursors, String> {
    let document: ParticipantCursorDocument =
        serde_json::from_slice(raw).map_err(|error| error.to_string())?;
    if document.version != PARTICIPANT_STATE_VERSION {
        return Err(format!(
            "unsupported participant cursor version {}",
            document.version
        ));
    }
    let mut mail = BTreeMap::new();
    for (address, set) in document.mail {
        let (kind, name) = address
            .split_once(':')
            .ok_or_else(|| format!("invalid mail cursor address '{address}'"))?;
        if !matches!(kind, "workspace" | "lineage" | "participant") {
            return Err(format!("invalid mail cursor address kind '{kind}'"));
        }
        crate::mailbox::validate_component(name)
            .map_err(|reason| format!("invalid mail cursor address '{address}': {reason}"))?;
        mail.insert(address, parse_ids(set.seen, IdKind::Mail)?);
    }
    let mut channels = BTreeMap::new();
    for (channel, set) in document.channels {
        channel::validate_channel_name(&channel)
            .map_err(|_| format!("invalid channel name '{channel}'"))?;
        channels.insert(channel, parse_ids(set.seen, IdKind::Channel)?);
    }
    Ok(ParticipantCursors { mail, channels })
}

fn serialize_participant_cursor(state: &ParticipantCursors) -> AppResult<Vec<u8>> {
    #[derive(Serialize)]
    struct StoredSet<'a> {
        seen: &'a BTreeSet<String>,
    }
    #[derive(Serialize)]
    struct StoredDocument<'a> {
        version: u64,
        mail: BTreeMap<&'a str, StoredSet<'a>>,
        channels: BTreeMap<&'a str, StoredSet<'a>>,
    }
    let document = StoredDocument {
        version: PARTICIPANT_STATE_VERSION,
        mail: state
            .mail
            .iter()
            .map(|(key, seen)| (key.as_str(), StoredSet { seen }))
            .collect(),
        channels: state
            .channels
            .iter()
            .map(|(key, seen)| (key.as_str(), StoredSet { seen }))
            .collect(),
    };
    let mut bytes = serde_json::to_vec_pretty(&document).map_err(|error| {
        AppError::new(
            ErrorCode::IoError,
            format!("failed to serialize participant cursor state: {error}"),
            "Retry the consuming command; the cursor was not updated.",
        )
    })?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn update_participant<T>(
    _context: &Context,
    participant: &Participant,
    update: impl FnOnce(&mut ParticipantCursors) -> AppResult<T>,
) -> AppResult<T> {
    fs::create_dir_all(&participant.dir).map_err(|error| {
        AppError::io(
            "create participant cursor directory",
            &participant.dir,
            error,
        )
    })?;
    let _lock = lock_cursor_dir(&participant.dir)?;
    let path = participant_cursor_path(participant);
    ensure_cursor_destination_safe(&path)?;
    let mut state = match read_participant_cursor(&path) {
        ParticipantCursorRead::Missing => ParticipantCursors::default(),
        ParticipantCursorRead::Valid(state) => state,
        ParticipantCursorRead::Invalid => {
            return Err(AppError::config(
                &path,
                "participant cursors.json is malformed or unsafe; refusing to discard its read state",
            ));
        }
    };
    let before = state.clone();
    let result = update(&mut state)?;
    if state != before {
        let bytes = serialize_participant_cursor(&state)?;
        atomic_replace(&path, &bytes)
            .map_err(|error| AppError::io("atomically update participant cursors", &path, error))?;
    }
    Ok(result)
}

fn lock_cursor_dir(directory: &Path) -> AppResult<File> {
    let path = directory.join(CURSORS_LOCK_FILE);
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
        .map_err(|error| {
            if error.raw_os_error() == Some(libc::ELOOP) {
                cursor_write_refused(&path, "lock path is a symlink")
            } else {
                AppError::io("open cursor state lock", &path, error)
            }
        })?;
    let before = file
        .metadata()
        .map_err(|error| AppError::io("inspect cursor state lock", &path, error))?;
    if !trusted_lock_metadata(&before) {
        return Err(cursor_write_refused(
            &path,
            "lock must be a solitary regular file",
        ));
    }
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == -1 {
        return Err(AppError::io(
            "lock cursor state",
            &path,
            std::io::Error::last_os_error(),
        ));
    }
    let after = file
        .metadata()
        .map_err(|error| AppError::io("reinspect cursor state lock", &path, error))?;
    let on_path = fs::symlink_metadata(&path).map_err(|error| {
        cursor_write_refused(
            &path,
            if error.kind() == std::io::ErrorKind::NotFound {
                "lock path disappeared while acquiring its flock"
            } else {
                "lock path cannot be inspected after acquiring its flock"
            },
        )
    })?;
    if !trusted_lock_metadata(&after)
        || !trusted_lock_metadata(&on_path)
        || after.dev() != on_path.dev()
        || after.ino() != on_path.ino()
        || after.nlink() != on_path.nlink()
    {
        return Err(cursor_write_refused(
            &path,
            "lock path changed while acquiring its flock",
        ));
    }
    if after.permissions().mode() & 0o777 != 0o600 {
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|error| AppError::io("restrict cursor state lock", &path, error))?;
    }
    Ok(file)
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CursorAdvance {
    pub prior: Option<String>,
    pub cursor: String,
    pub advanced: bool,
    pub marked: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct MailMove {
    pub id: String,
    pub source: PathBuf,
    pub destination: PathBuf,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct Delta {
    pub mail_moves: Vec<MailMove>,
    pub channel_seen: Vec<(String, Vec<String>)>,
}

#[derive(Debug, Clone, Default)]
struct State {
    mail: BTreeSet<String>,
    channels: BTreeMap<String, BTreeSet<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Snapshot {
    mail: BTreeSet<String>,
    channels: BTreeMap<String, BTreeSet<String>>,
}

impl Snapshot {
    pub(crate) fn load(context: &Context, room: &str) -> Self {
        let path = match cursor_path(context, room) {
            Ok(path) => path,
            Err(_) => return Self::default(),
        };
        match read_cursor(&path) {
            CursorRead::Missing => load_legacy_state(context, room)
                .unwrap_or_default()
                .into_snapshot(),
            CursorRead::Valid(state) => state.into_snapshot(),
            CursorRead::Invalid => {
                warn_invalid_cursor(room);
                Self::default()
            }
        }
    }

    #[allow(dead_code)]
    pub(crate) fn mail_has_seen(&self, id: &str) -> bool {
        self.mail.contains(id)
    }

    #[allow(dead_code)]
    pub(crate) fn channel_has_seen(&self, channel: &str, id: &str) -> bool {
        self.channels
            .get(channel)
            .is_some_and(|seen| seen.contains(id))
    }

    pub(crate) fn into_channels(self) -> BTreeMap<String, BTreeSet<String>> {
        self.channels
    }

    #[allow(dead_code)]
    pub(crate) fn max_seen(&self, channel: &str) -> Option<&str> {
        self.channels
            .get(channel)
            .and_then(|seen| seen.last())
            .map(String::as_str)
    }

    #[allow(dead_code)]
    pub(crate) fn channel_seen_count(&self, channel: &str) -> usize {
        self.channels.get(channel).map(|set| set.len()).unwrap_or(0)
    }
}

impl State {
    fn into_snapshot(self) -> Snapshot {
        Snapshot {
            mail: self.mail,
            channels: self.channels,
        }
    }
}

#[allow(dead_code)]
pub(crate) fn consume(context: &Context, room: &str, delta: Delta) -> AppResult<()> {
    if delta.mail_moves.is_empty() && delta.channel_seen.iter().all(|(_, ids)| ids.is_empty()) {
        return Ok(());
    }
    consume_inner(context, room, delta, None, None).map(|_| ())
}

#[allow(dead_code)]
pub(crate) fn consume_channel(
    context: &Context,
    room: &str,
    channel: &str,
    ids: Vec<String>,
) -> AppResult<CursorAdvance> {
    if ids.is_empty() {
        let prior = Snapshot::load(context, room)
            .max_seen(channel)
            .map(str::to_owned);
        return Ok(CursorAdvance {
            cursor: prior.clone().unwrap_or_default(),
            prior,
            advanced: false,
            marked: 0,
        });
    }
    consume_inner(
        context,
        room,
        Delta {
            mail_moves: Vec::new(),
            channel_seen: vec![(channel.to_owned(), ids)],
        },
        Some(channel),
        None,
    )
}

#[allow(dead_code)]
pub(crate) fn consume_channel_through(
    context: &Context,
    room: &str,
    channel: &str,
    target: &str,
) -> AppResult<CursorAdvance> {
    consume_inner(
        context,
        room,
        Delta::default(),
        Some(channel),
        Some((channel, target)),
    )
}

fn consume_inner(
    context: &Context,
    room: &str,
    delta: Delta,
    outcome_channel: Option<&str>,
    through: Option<(&str, &str)>,
) -> AppResult<CursorAdvance> {
    validate_delta(&delta)?;
    let path = cursor_path(context, room)?;
    let parent = path
        .parent()
        .ok_or_else(|| AppError::invalid_argument("cursor path has no room directory"))?;
    fs::create_dir_all(parent)
        .map_err(|error| AppError::io("create cursor state directory", parent, error))?;
    let _lock = lock_room_cursors(context, room)?;
    ensure_cursor_destination_safe(&path)?;
    let mut state = load_for_write(context, room, &path)?;
    let prior = outcome_channel.and_then(|channel| {
        state
            .channels
            .get(channel)
            .and_then(|seen| seen.last())
            .cloned()
    });

    let through_ids = match through {
        Some((channel, target)) => {
            let empty = BTreeSet::new();
            let seen = state.channels.get(channel).unwrap_or(&empty);
            unseen_candidates(context, channel, seen, Some(target))?
        }
        None => BTreeSet::new(),
    };

    let mut committed_mail = BTreeSet::new();
    let mut move_errors = Vec::new();
    for mail_move in delta.mail_moves {
        match exclusive_move(&mail_move.source, &mail_move.destination) {
            Ok(()) => {
                committed_mail.insert(mail_move.id);
            }
            Err(error @ MoveError::Link(_)) => {
                move_errors.push(mail_move_error(&mail_move, error));
            }
            Err(error @ MoveError::Unlink(_)) => {
                committed_mail.insert(mail_move.id.clone());
                move_errors.push(mail_move_error(&mail_move, error));
            }
        }
    }

    let mut requested_channels: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (channel, ids) in delta.channel_seen {
        requested_channels.entry(channel).or_default().extend(ids);
    }
    if let Some((channel, _)) = through {
        requested_channels
            .entry(channel.to_owned())
            .or_default()
            .extend(through_ids);
    }

    let mut marked_channels = 0;
    for (channel, additions) in requested_channels {
        let seen = state.channels.entry(channel.clone()).or_default();
        let fresh: Vec<String> = additions.difference(seen).cloned().collect();
        marked_channels += fresh.len();
        seen.extend(fresh);
        if seen.len() >= SEEN_SET_WARN {
            eprintln!(
                "post: warning: channel '{channel}' seen-set holds {} ids; reads/acks scale linearly",
                seen.len()
            );
        }
    }

    let fresh_mail: Vec<String> = committed_mail.difference(&state.mail).cloned().collect();
    let changed = !fresh_mail.is_empty() || marked_channels > 0;
    state.mail.extend(fresh_mail);
    if state.mail.len() >= SEEN_SET_WARN {
        eprintln!(
            "post: warning: mail seen-set holds {} ids; reads/acks scale linearly",
            state.mail.len()
        );
    }

    if changed {
        replace_state(&path, &state)?;
    }
    let mut move_errors = move_errors.into_iter();
    if let Some(error) = move_errors.next() {
        for warning in move_errors {
            eprintln!(
                "post: warning: additional mail move failure: {}",
                warning.message
            );
        }
        return Err(error);
    }

    let cursor = outcome_channel
        .and_then(|channel| state.channels.get(channel))
        .and_then(|seen| seen.last())
        .cloned()
        .or(prior.clone())
        .unwrap_or_default();
    Ok(CursorAdvance {
        prior,
        cursor,
        advanced: marked_channels > 0,
        marked: marked_channels,
    })
}

fn validate_delta(delta: &Delta) -> AppResult<()> {
    for mail_move in &delta.mail_moves {
        if !is_canonical_mail_id(&mail_move.id) {
            return Err(AppError::invalid_argument(format!(
                "mail cursor id '{}' is not canonical",
                mail_move.id
            )));
        }
    }
    for (channel, ids) in &delta.channel_seen {
        channel::validate_channel_name(channel)?;
        for id in ids {
            if !channel::is_canonical_channel_message_id(id) {
                return Err(AppError::invalid_argument(format!(
                    "channel cursor id '{}' is not canonical",
                    id
                )));
            }
        }
    }
    Ok(())
}

fn cursor_path(context: &Context, room: &str) -> AppResult<PathBuf> {
    crate::mailbox::validate_room_name(room).map_err(|reason| {
        AppError::new(
            ErrorCode::InvalidArgument,
            format!("room '{room}' is invalid: {reason}"),
            "Pass a single room name without path separators.",
        )
    })?;
    Ok(context.root.join(room).join(CURSORS_FILE))
}

enum CursorRead {
    Missing,
    Valid(State),
    Invalid,
}

fn read_cursor(path: &Path) -> CursorRead {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return CursorRead::Missing,
        Err(_) => return CursorRead::Invalid,
    };
    if !metadata.file_type().is_file() || metadata.nlink() != 1 {
        return CursorRead::Invalid;
    }
    let raw = match fs::read(path) {
        Ok(raw) => raw,
        Err(_) => return CursorRead::Invalid,
    };
    parse_cursor(&raw).map_or_else(|_| CursorRead::Invalid, CursorRead::Valid)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CursorSet {
    seen: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CursorDocument {
    version: u64,
    mail: CursorSet,
    channels: BTreeMap<String, CursorSet>,
}

fn parse_cursor(raw: &[u8]) -> Result<State, String> {
    let document: CursorDocument =
        serde_json::from_slice(raw).map_err(|error| error.to_string())?;
    if document.version != STATE_VERSION {
        return Err(format!(
            "unsupported cursor state version {} (this binary writes {STATE_VERSION})",
            document.version
        ));
    }
    let mail = parse_ids(document.mail.seen, IdKind::Mail)?;
    let mut channels = BTreeMap::new();
    for (channel, set) in document.channels {
        channel::validate_channel_name(&channel)
            .map_err(|_| format!("invalid channel name '{channel}'"))?;
        channels.insert(channel, parse_ids(set.seen, IdKind::Channel)?);
    }
    Ok(State { mail, channels })
}

enum IdKind {
    Mail,
    Channel,
}

fn parse_ids(ids: Vec<String>, kind: IdKind) -> Result<BTreeSet<String>, String> {
    let mut previous = None;
    let mut result = BTreeSet::new();
    for id in ids {
        let valid = match kind {
            IdKind::Mail => is_canonical_mail_id(&id),
            IdKind::Channel => channel::is_canonical_channel_message_id(&id),
        };
        if !valid {
            return Err(format!("invalid {} id '{id}'", id_kind_name(&kind)));
        }
        if previous
            .as_deref()
            .is_some_and(|prior: &str| prior >= id.as_str())
        {
            return Err(format!(
                "{} ids must be sorted and duplicate-free",
                id_kind_name(&kind)
            ));
        }
        previous = Some(id.clone());
        result.insert(id);
    }
    Ok(result)
}

fn id_kind_name(kind: &IdKind) -> &'static str {
    match kind {
        IdKind::Mail => "mail",
        IdKind::Channel => "channel",
    }
}

fn serialize_state(state: &State) -> AppResult<Vec<u8>> {
    #[derive(Serialize)]
    struct StoredSet<'a> {
        seen: &'a BTreeSet<String>,
    }
    #[derive(Serialize)]
    struct StoredDocument<'a> {
        version: u64,
        mail: StoredSet<'a>,
        channels: BTreeMap<&'a str, StoredSet<'a>>,
    }
    let document = StoredDocument {
        version: STATE_VERSION,
        mail: StoredSet { seen: &state.mail },
        channels: state
            .channels
            .iter()
            .map(|(channel, seen)| (channel.as_str(), StoredSet { seen }))
            .collect(),
    };
    let mut bytes = serde_json::to_vec_pretty(&document).map_err(|error| {
        AppError::new(
            ErrorCode::IoError,
            format!("failed to serialize cursor state: {error}"),
            "Retry the consuming command; the cursor was not updated.",
        )
    })?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn replace_state(path: &Path, state: &State) -> AppResult<()> {
    ensure_cursor_destination_safe(path)?;
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.permissions().mode() & 0o777 != 0o600 {
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))
                .map_err(|error| AppError::io("restrict cursor state", path, error))?;
        }
    }
    let bytes = serialize_state(state)?;
    atomic_replace(path, &bytes)
        .map_err(|error| AppError::io("atomically update cursor state", path, error))
}

fn ensure_cursor_destination_safe(path: &Path) -> AppResult<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(cursor_write_refused(path, "cursor path is a symlink"))
        }
        Ok(metadata) if !metadata.file_type().is_file() || metadata.nlink() != 1 => Err(
            cursor_write_refused(path, "cursor path is not a solitary regular file"),
        ),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(AppError::io("inspect cursor state", path, error)),
    }
}

fn cursor_write_refused(path: &Path, reason: &str) -> AppError {
    AppError::new(
        ErrorCode::ConfigInvalid,
        format!("cursor state '{}' is unsafe: {reason}", path.display()),
        "Move the unsafe cursor path aside by hand, then retry the consuming command.",
    )
    .path(path.display().to_string())
    .reason(reason)
}

fn load_for_write(context: &Context, room: &str, path: &Path) -> AppResult<State> {
    match read_cursor(path) {
        CursorRead::Valid(state) => Ok(state),
        CursorRead::Missing => load_legacy_state(context, room),
        CursorRead::Invalid => Ok(State::default()),
    }
}

fn load_legacy_state(context: &Context, room: &str) -> AppResult<State> {
    let path = match crate::channel::channel_state_path(context, room) {
        Ok(path) => path,
        Err(_) => return Ok(State::default()),
    };
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(State::default());
        }
        Err(_) => {
            warn_invalid_cursor(room);
            return Ok(State::default());
        }
    };
    if !metadata.file_type().is_file() || metadata.nlink() != 1 {
        warn_invalid_cursor(room);
        return Ok(State::default());
    }
    let raw = match fs::read(&path) {
        Ok(raw) => raw,
        Err(_) => {
            warn_invalid_cursor(room);
            return Ok(State::default());
        }
    };
    match parse_legacy(&raw) {
        Ok(LegacyStored::V1(cursors)) => migrate_v1(context, cursors),
        Ok(LegacyStored::V2(state)) => Ok(state),
        Err(_) => {
            warn_invalid_cursor(room);
            Ok(State::default())
        }
    }
}

enum LegacyStored {
    V1(BTreeMap<String, String>),
    V2(State),
}

fn parse_legacy(raw: &[u8]) -> Result<LegacyStored, String> {
    let value: serde_json::Value =
        serde_json::from_slice(raw).map_err(|error| error.to_string())?;
    let object = value
        .as_object()
        .ok_or_else(|| "legacy state is not a JSON object".to_owned())?;
    if let Ok(cursors) = serde_json::from_value::<BTreeMap<String, String>>(value.clone()) {
        for (channel, cursor) in &cursors {
            channel::validate_channel_name(channel)
                .map_err(|_| format!("invalid legacy channel name '{channel}'"))?;
            if !channel::is_canonical_channel_message_id(cursor) {
                return Err(format!("legacy channel '{channel}' has an invalid cursor"));
            }
        }
        return Ok(LegacyStored::V1(cursors));
    }
    let version = object
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| "legacy state has no supported version".to_owned())?;
    if version != 2 {
        return Err(format!(
            "unsupported legacy channel-state version {version}"
        ));
    }
    let channels = object
        .get("channels")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| "legacy state channels is not an object".to_owned())?;
    let mut parsed = BTreeMap::new();
    for (channel, entry) in channels {
        channel::validate_channel_name(channel)
            .map_err(|_| format!("invalid legacy channel name '{channel}'"))?;
        let seen = entry
            .as_object()
            .and_then(|object| object.get("seen"))
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| format!("legacy channel '{channel}' has no seen array"))?;
        let mut set = BTreeSet::new();
        for id in seen {
            let id = id
                .as_str()
                .ok_or_else(|| format!("legacy channel '{channel}' has a non-string id"))?;
            if !channel::is_canonical_channel_message_id(id) {
                return Err(format!("legacy channel '{channel}' has an invalid id"));
            }
            set.insert(id.to_owned());
        }
        parsed.insert(channel.clone(), set);
    }
    Ok(LegacyStored::V2(State {
        mail: BTreeSet::new(),
        channels: parsed,
    }))
}

pub(crate) fn legacy_stored_shape_is_valid(bytes: &[u8]) -> bool {
    parse_legacy(bytes).is_ok()
}

fn migrate_v1(context: &Context, cursors: BTreeMap<String, String>) -> AppResult<State> {
    let mut channels = BTreeMap::new();
    for (channel, cursor) in cursors {
        let directory = context
            .root
            .join(CHANNELS_DIR)
            .join(&channel)
            .join("messages");
        let files = match channel::message_files(&directory) {
            Ok(files) => files,
            Err(error) if error.code == ErrorCode::IoError => match fs::metadata(&directory) {
                Err(metadata_error) if metadata_error.kind() == std::io::ErrorKind::NotFound => {
                    Vec::new()
                }
                _ => return Err(error),
            },
            Err(error) => return Err(error),
        };
        let mut seen = BTreeSet::new();
        for path in files {
            if let Some(id) = path.file_stem().and_then(|value| value.to_str()) {
                if id <= cursor.as_str() && channel::is_canonical_channel_message_id(id) {
                    seen.insert(id.to_owned());
                }
            }
        }
        channels.insert(channel, seen);
    }
    Ok(State {
        mail: BTreeSet::new(),
        channels,
    })
}

fn unseen_candidates(
    context: &Context,
    channel: &str,
    seen: &BTreeSet<String>,
    target: Option<&str>,
) -> AppResult<BTreeSet<String>> {
    let directory = context
        .root
        .join(CHANNELS_DIR)
        .join(channel)
        .join("messages");
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(error) => {
            return Err(AppError::io(
                "list channel messages directory",
                &directory,
                error,
            ))
        }
    };
    let mut paths = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|error| AppError::io("read channel messages entry", &directory, error))?
            .path();
        if path.is_file() && path.extension().and_then(|value| value.to_str()) == Some("msg") {
            paths.push(path);
        }
    }
    paths.sort();
    let mut candidates = BTreeSet::new();
    for path in paths {
        let Some(id) = path.file_stem().and_then(|value| value.to_str()) else {
            channel::parse_channel_message(&path)?;
            continue;
        };
        if seen.contains(id) || target.is_some_and(|target| id > target) {
            continue;
        }
        channel::parse_channel_message(&path)?;
        candidates.insert(id.to_owned());
    }
    Ok(candidates)
}

fn mail_move_error(mail_move: &MailMove, error: MoveError) -> AppError {
    match error {
        MoveError::Link(error) if error.kind() == std::io::ErrorKind::AlreadyExists => AppError::new(
            ErrorCode::IoError,
            format!(
                "cannot mark mail '{}' read because '{}' already exists",
                mail_move.id,
                mail_move.destination.display()
            ),
            "Run post doctor; resolve the duplicate without deleting either copy.",
        )
        .input(mail_move.id.clone())
        .reason("read destination already exists"),
        MoveError::Link(error) => AppError::io(
            "move mail from inbox to read",
            &mail_move.destination,
            error,
        ),
        MoveError::Unlink(error) => AppError::new(
            ErrorCode::DeliveredOutputFailure,
            format!(
                "mail '{}' was printed but could not be removed from inbox '{}': {error}; it now appears in both inbox and read",
                mail_move.id,
                mail_move.source.display()
            ),
            "Do not treat the next inbox listing of this id as new mail; run post doctor and reconcile the duplicate links by hand.",
        )
        .input(mail_move.id.clone())
        .reason("inbox link removal failed after read link was committed"),
    }
}

fn warn_invalid_cursor(room: &str) {
    eprintln!(
        "post: warning: cursor state for room '{room}' is invalid or unavailable; treating all messages as unread"
    );
}

fn is_canonical_mail_id(id: &str) -> bool {
    let id = id.as_bytes();
    id.len() == 22
        && id[..8].iter().all(u8::is_ascii_digit)
        && id[8] == b'-'
        && id[9..15].iter().all(u8::is_ascii_digit)
        && id[15] == b'-'
        && id[16..].iter().all(u8::is_ascii_hexdigit)
}

fn lock_room_cursors(context: &Context, room: &str) -> AppResult<File> {
    let directory = cursor_path(context, room)?
        .parent()
        .expect("room cursor path has a parent")
        .to_path_buf();
    lock_cursor_dir(&directory)
}

fn trusted_lock_metadata(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_file() && metadata.nlink() == 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ChannelMessage;
    use crate::test_support::{test_root, trash_test_root};
    use std::sync::{Arc, Barrier};

    const MAIL_ID: &str = "20260831-171234-a1b2c3";
    const ID1: &str = "20260831-171234-000001-a1b2c3";
    const ID2: &str = "20260831-171234-000002-b2c3d4";
    const ID3: &str = "20260831-171234-000003-c3d4e5";

    fn context(label: &str) -> (std::path::PathBuf, Context) {
        let root = test_root(&format!("cursorstate-{label}"));
        (
            root.clone(),
            Context {
                root: root.clone(),
                home: root,
            },
        )
    }

    fn seed_message(root: &Path, channel: &str, id: &str, from: &str) {
        let directory = root.join(CHANNELS_DIR).join(channel).join("messages");
        fs::create_dir_all(&directory).expect("create message directory");
        let message = ChannelMessage {
            id: id.to_owned(),
            from: from.to_owned(),
            channel: channel.to_owned(),
            subject: String::new(),
            sent: "2026-08-31 17:12:34 +0000".to_owned(),
            from_participant: None,
            from_lineage: None,
            address_kind: None,
            event: None,
            display_name: None,
            pfp: None,
            re: None,
            mentions: Vec::new(),
            signature_ref: None,
            sender_address: None,
            sender_provenance: None,
        };
        fs::write(
            directory.join(format!("{id}.msg")),
            channel::encode_message(&message, "body").expect("encode message"),
        )
        .expect("write message");
    }

    fn write_legacy_v1(root: &Path, room: &str, bytes: &str) {
        fs::create_dir_all(root.join(room)).expect("create room");
        fs::write(root.join(room).join("channel-state.json"), bytes).expect("write legacy");
    }

    #[test]
    fn missing_and_malformed_cursor_are_empty_without_writes() {
        let (root, context) = context("advisory");
        let before = fs::read_dir(&root).expect("root").count();
        let missing = Snapshot::load(&context, "alpha");
        assert!(!missing.mail_has_seen(MAIL_ID));
        assert!(!root.join("alpha").exists());
        assert_eq!(before, fs::read_dir(&root).expect("root").count());
        fs::create_dir_all(root.join("alpha")).expect("room");
        fs::write(root.join("alpha/cursors.json"), b"{not json").expect("malformed");
        let legacy = format!(r#"{{"tax":"{ID1}"}}"#);
        fs::write(root.join("alpha/channel-state.json"), &legacy).expect("legacy");
        let malformed = Snapshot::load(&context, "alpha");
        assert!(!malformed.mail_has_seen(MAIL_ID));
        assert!(!malformed.channel_has_seen("tax", ID1));
        assert_eq!(
            fs::read(root.join("alpha/cursors.json")).expect("read"),
            b"{not json"
        );
        assert_eq!(
            fs::read(root.join("alpha/channel-state.json")).expect("read legacy"),
            legacy.as_bytes()
        );
        assert!(!root.join("alpha/.cursors.lock").exists());
        trash_test_root(&root);
    }

    #[cfg(unix)]
    #[test]
    fn participant_cursor_lock_refuses_symlink_hardlink_and_fifo_without_mutation() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::symlink;

        for kind in ["symlink", "hardlink", "fifo"] {
            let (root, context) = context(&format!("hostile-lock-{kind}"));
            let participant = crate::participant::bind_test_actor(&context, "alpha");
            let lock = participant.dir.join(CURSORS_LOCK_FILE);
            let victim = root.join("victim");
            fs::write(&victim, b"untouched").expect("seed victim");
            match kind {
                "symlink" => symlink(&victim, &lock).expect("plant symlink"),
                "hardlink" => fs::hard_link(&victim, &lock).expect("plant hard link"),
                "fifo" => {
                    let raw = CString::new(lock.as_os_str().as_bytes()).expect("fifo path");
                    assert_eq!(unsafe { libc::mkfifo(raw.as_ptr(), 0o600) }, 0);
                }
                _ => unreachable!(),
            }
            let error = ParticipantCursors::consume_mail(
                &context,
                &participant,
                &Address {
                    kind: crate::participant::AddressKind::Workspace,
                    name: "alpha".to_owned(),
                },
                &[MAIL_ID.to_owned()],
            )
            .expect_err("hostile cursor lock must be refused");
            assert_eq!(error.code, ErrorCode::ConfigInvalid, "{kind}");
            assert_eq!(fs::read(&victim).expect("victim survives"), b"untouched");
            assert!(!participant.dir.join("cursors.json").exists());
            trash_test_root(&root);
        }
    }

    #[test]
    fn exact_v1_serialization_round_trips_mail_and_channels() {
        let (root, context) = context("roundtrip");
        consume(
            &context,
            "alpha",
            Delta {
                mail_moves: Vec::new(),
                channel_seen: vec![("tax".to_owned(), vec![ID2.to_owned(), ID1.to_owned()])],
            },
        )
        .expect("consume channel set");
        let path = root.join("alpha/cursors.json");
        let bytes = fs::read_to_string(&path).expect("read cursor");
        assert_eq!(
            bytes,
            r#"{
  "version": 1,
  "mail": {
    "seen": []
  },
  "channels": {
    "tax": {
      "seen": [
        "20260831-171234-000001-a1b2c3",
        "20260831-171234-000002-b2c3d4"
      ]
    }
  }
}
"#
        );
        let snapshot = Snapshot::load(&context, "alpha");
        assert!(snapshot.channel_has_seen("tax", ID1));
        assert!(snapshot.channel_has_seen("tax", ID2));
        trash_test_root(&root);
    }

    #[test]
    fn legacy_import_is_read_only_then_materialized_without_touching_legacy() {
        let (root, context) = context("legacy");
        seed_message(&root, "tax", ID1, "beta");
        seed_message(&root, "tax", ID2, "beta");
        seed_message(&root, "tax", ID3, "beta");
        let legacy = r#"{"tax":"20260831-171234-000002-b2c3d4"}"#;
        write_legacy_v1(&root, "alpha", legacy);
        let snapshot = Snapshot::load(&context, "alpha");
        assert!(snapshot.channel_has_seen("tax", ID1));
        assert!(snapshot.channel_has_seen("tax", ID2));
        assert!(!snapshot.channel_has_seen("tax", ID3));
        let legacy_path = root.join("alpha/channel-state.json");
        assert_eq!(fs::read(&legacy_path).expect("legacy"), legacy.as_bytes());
        consume_channel(&context, "alpha", "tax", vec![ID3.to_owned()]).expect("materialize");
        assert_eq!(fs::read(&legacy_path).expect("legacy"), legacy.as_bytes());
        assert!(root.join("alpha/cursors.json").is_file());
        let snapshot = Snapshot::load(&context, "alpha");
        assert!(snapshot.channel_has_seen("tax", ID1));
        assert!(snapshot.channel_has_seen("tax", ID2));
        assert!(snapshot.channel_has_seen("tax", ID3));
        trash_test_root(&root);
    }

    #[test]
    fn legacy_v2_import_is_read_only_then_materialized_without_touching_legacy() {
        let (root, context) = context("legacy-v2");
        let legacy = format!(
            r#"{{"version":2,"channels":{{"tax":{{"seen":["{}","{}"]}}}}}}"#,
            ID1, ID2
        );
        write_legacy_v1(&root, "alpha", &legacy);
        let legacy_path = root.join("alpha/channel-state.json");
        let snapshot = Snapshot::load(&context, "alpha");
        assert!(snapshot.channel_has_seen("tax", ID1));
        assert!(snapshot.channel_has_seen("tax", ID2));
        assert!(!snapshot.channel_has_seen("tax", ID3));
        assert!(!root.join("alpha/cursors.json").exists());
        assert_eq!(fs::read(&legacy_path).expect("legacy"), legacy.as_bytes());

        consume_channel(&context, "alpha", "tax", vec![ID3.to_owned()]).expect("materialize");
        let snapshot = Snapshot::load(&context, "alpha");
        assert!(snapshot.channel_has_seen("tax", ID1));
        assert!(snapshot.channel_has_seen("tax", ID2));
        assert!(snapshot.channel_has_seen("tax", ID3));
        assert_eq!(fs::read(&legacy_path).expect("legacy"), legacy.as_bytes());
        trash_test_root(&root);
    }

    #[test]
    fn legacy_v1_migration_error_refuses_write_without_losing_prior_channels() {
        let (root, context) = context("legacy-v1-permission");
        seed_message(&root, "aaa", ID1, "beta");
        let locked_directory = root.join(CHANNELS_DIR).join("zzz").join("messages");
        fs::create_dir_all(&locked_directory).expect("locked directory");
        let legacy = format!(r#"{{"aaa":"{}","zzz":"{}"}}"#, ID1, ID2);
        write_legacy_v1(&root, "alpha", &legacy);
        fs::set_permissions(&locked_directory, fs::Permissions::from_mode(0o000))
            .expect("lock messages directory");

        let error = consume_channel(&context, "alpha", "aaa", vec![ID3.to_owned()])
            .expect_err("writer must refuse unreadable legacy channel");
        assert_eq!(error.code, ErrorCode::IoError);
        assert!(!root.join("alpha/cursors.json").exists());

        fs::set_permissions(&locked_directory, fs::Permissions::from_mode(0o700))
            .expect("restore messages directory");
        let snapshot = Snapshot::load(&context, "alpha");
        assert!(snapshot.channel_has_seen("aaa", ID1));
        trash_test_root(&root);
    }

    #[test]
    fn late_channel_id_below_maximum_stays_unread() {
        let (root, context) = context("late");
        consume_channel(
            &context,
            "alpha",
            "tax",
            vec![ID1.to_owned(), ID3.to_owned()],
        )
        .expect("mark out of order");
        seed_message(&root, "tax", ID2, "beta");
        let snapshot = Snapshot::load(&context, "alpha");
        assert!(snapshot.channel_has_seen("tax", ID1));
        assert!(snapshot.channel_has_seen("tax", ID3));
        assert!(!snapshot.channel_has_seen("tax", ID2));
        trash_test_root(&root);
    }

    #[test]
    fn out_of_order_mail_marks_only_the_chosen_ids() {
        let (root, context) = context("mail");
        let inbox = root.join("alpha/inbox");
        let read = root.join("alpha/read");
        fs::create_dir_all(&inbox).expect("inbox");
        fs::create_dir_all(&read).expect("read");
        let mail_ids = [
            "20260831-171234-a1b2c3",
            "20260831-171235-b2c3d4",
            "20260831-171236-c3d4e5",
        ];
        for id in mail_ids {
            fs::write(inbox.join(format!("{id}.mail")), b"mail").expect("mail");
        }
        consume(
            &context,
            "alpha",
            Delta {
                mail_moves: vec![
                    MailMove {
                        id: mail_ids[2].to_owned(),
                        source: inbox.join(format!("{}.mail", mail_ids[2])),
                        destination: read.join(format!("{}.mail", mail_ids[2])),
                    },
                    MailMove {
                        id: mail_ids[0].to_owned(),
                        source: inbox.join(format!("{}.mail", mail_ids[0])),
                        destination: read.join(format!("{}.mail", mail_ids[0])),
                    },
                ],
                channel_seen: Vec::new(),
            },
        )
        .expect("consume selected mail");
        let snapshot = Snapshot::load(&context, "alpha");
        assert!(snapshot.mail_has_seen(mail_ids[0]));
        assert!(!snapshot.mail_has_seen(mail_ids[1]));
        assert!(snapshot.mail_has_seen(mail_ids[2]));
        assert!(inbox.join(format!("{}.mail", mail_ids[1])).exists());
        trash_test_root(&root);
    }

    #[test]
    fn mail_move_failure_does_not_stop_later_moves() {
        let (root, context) = context("mail-failure-continues");
        let inbox = root.join("alpha/inbox");
        let read = root.join("alpha/read");
        fs::create_dir_all(&inbox).expect("inbox");
        fs::create_dir_all(&read).expect("read");
        let duplicate_id = "20260831-171234-a1b2c3";
        let later_id = "20260831-171235-b2c3d4";
        fs::write(
            inbox.join(format!("{duplicate_id}.mail")),
            b"duplicate unread",
        )
        .expect("duplicate inbox mail");
        fs::write(inbox.join(format!("{later_id}.mail")), b"later unread")
            .expect("later inbox mail");
        fs::write(read.join(format!("{duplicate_id}.mail")), b"existing read")
            .expect("existing read copy");

        let error = consume(
            &context,
            "alpha",
            Delta {
                mail_moves: vec![
                    MailMove {
                        id: duplicate_id.to_owned(),
                        source: inbox.join(format!("{duplicate_id}.mail")),
                        destination: read.join(format!("{duplicate_id}.mail")),
                    },
                    MailMove {
                        id: later_id.to_owned(),
                        source: inbox.join(format!("{later_id}.mail")),
                        destination: read.join(format!("{later_id}.mail")),
                    },
                ],
                channel_seen: Vec::new(),
            },
        )
        .expect_err("duplicate destination should still surface an error");
        assert_eq!(error.code, ErrorCode::IoError);
        assert!(error.message.contains(duplicate_id));
        assert!(inbox.join(format!("{duplicate_id}.mail")).exists());
        assert!(read.join(format!("{duplicate_id}.mail")).exists());
        assert!(!inbox.join(format!("{later_id}.mail")).exists());
        assert!(read.join(format!("{later_id}.mail")).exists());
        let snapshot = Snapshot::load(&context, "alpha");
        assert!(!snapshot.mail_has_seen(duplicate_id));
        assert!(snapshot.mail_has_seen(later_id));
        trash_test_root(&root);
    }

    #[test]
    fn symlinked_cursor_degrades_on_read_and_refuses_write() {
        let (root, context) = context("symlink");
        fs::create_dir_all(root.join("alpha")).expect("room");
        fs::write(root.join("target.json"), b"{}").expect("target");
        let target_before = fs::read(root.join("target.json")).expect("target");
        std::os::unix::fs::symlink(root.join("target.json"), root.join("alpha/cursors.json"))
            .expect("symlink");
        let snapshot = Snapshot::load(&context, "alpha");
        assert!(!snapshot.channel_has_seen("tax", ID1));
        let error = consume_channel(&context, "alpha", "tax", vec![ID1.to_owned()])
            .expect_err("writer must refuse symlink");
        assert_eq!(error.code, ErrorCode::ConfigInvalid);
        assert!(root.join("alpha/cursors.json").is_symlink());
        assert_eq!(
            fs::read(root.join("target.json")).expect("target"),
            target_before
        );
        trash_test_root(&root);
    }

    #[test]
    fn concurrent_writers_union_the_whole_map() {
        let (root, context) = context("concurrent");
        let barrier = Arc::new(Barrier::new(8));
        let mut handles = Vec::new();
        for worker in 0..8 {
            let context = context.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                let channel = format!("chan{worker}");
                let id = format!("20260831-171234-00000{worker}-a1b2c3");
                consume_channel(&context, "alpha", &channel, vec![id]).expect("consume");
            }));
        }
        for handle in handles {
            handle.join().expect("worker");
        }
        let snapshot = Snapshot::load(&context, "alpha");
        for worker in 0..8 {
            let channel = format!("chan{worker}");
            let id = format!("20260831-171234-00000{worker}-a1b2c3");
            assert!(snapshot.channel_has_seen(&channel, &id));
        }
        assert_eq!(
            fs::metadata(root.join("alpha/.cursors.lock"))
                .expect("lock")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        trash_test_root(&root);
    }
}
