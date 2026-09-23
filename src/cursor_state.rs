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
use std::time::{Duration, Instant};

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
            ParticipantCursorRead::Invalid { reason, .. } => {
                // Fail open: a read degrades to an empty snapshot, so every
                // eligible message is reported as unread. That is a large,
                // misleading change in what a monitor sees, so the warning
                // names the participant, the file, and the reason, and points
                // at the machine-readable signal: `post watch` marks the events
                // and digests it projects from this state, and `post doctor`
                // reports the finding after the fact.
                //
                // Every one of those three values is debug-quoted, not
                // interpolated: this is the hostile-state reader, so the path
                // (the mailbox root is attacker-influenced) and the reason
                // (which quotes the file's own bytes) can carry newlines and
                // control text that would otherwise forge extra diagnostic
                // lines in the output a harness is reading. Same discipline as
                // the unreadable-message warnings (`{:?}` on the same values).
                eprintln!(
                    "post: warning: participant {:?} cursor state at {:?} is unusable ({:?}): every eligible message in every joined channel and address is reported unread until it is repaired; `post watch` marks what it re-reports and `post doctor` reports the finding",
                    participant.id,
                    participant_cursor_path(participant),
                    reason
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
        update_participant(context, participant, LockWait::Blocking, |state| {
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
        Self::consume_channel_waiting(context, participant, channel, ids, LockWait::Blocking)
    }

    /// `consume_channel` for a best-effort caller that must not wait on the
    /// cursor lock past `budget`. A timeout is an ordinary error that names
    /// the lock; nothing is written.
    pub(crate) fn consume_channel_within(
        context: &Context,
        participant: &Participant,
        channel: &str,
        ids: &[String],
        budget: Duration,
    ) -> AppResult<CursorAdvance> {
        Self::consume_channel_waiting(context, participant, channel, ids, LockWait::Within(budget))
    }

    fn consume_channel_waiting(
        context: &Context,
        participant: &Participant,
        channel: &str,
        ids: &[String],
        wait: LockWait,
    ) -> AppResult<CursorAdvance> {
        channel::validate_channel_name(channel)?;
        for id in ids {
            validate_channel_id(id)?;
        }
        update_participant(context, participant, wait, |state| {
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
        update_participant(context, participant, LockWait::Blocking, |state| {
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

/// Why this participant's cursor state cannot be used, when it exists and is
/// unusable. `None` covers both the healthy case and the missing file (a
/// participant that has consumed nothing yet). Runtime reads degrade to an
/// empty snapshot and warn; this is what `post doctor` reports so the degrade
/// is discoverable after the fact rather than only in a live process's stderr.
pub(crate) fn participant_cursor_defect(participant: &Participant) -> Option<String> {
    match read_participant_cursor(&participant_cursor_path(participant)) {
        ParticipantCursorRead::Invalid { reason, .. } => Some(reason),
        ParticipantCursorRead::Missing | ParticipantCursorRead::Valid(_) => None,
    }
}

/// Why a participant's cursor state could not be read. `transient` marks a
/// failure that a second attempt can plausibly fix (a metadata lookup or read
/// error); content failures -- symlink, not a solitary regular file, wrong
/// version, malformed JSON, invalid ids -- are deterministic and are never
/// retried into a success.
enum ParticipantCursorRead {
    Missing,
    Valid(ParticipantCursors),
    Invalid { reason: String, transient: bool },
}

impl ParticipantCursorRead {
    fn invalid(reason: impl Into<String>) -> Self {
        Self::Invalid {
            reason: reason.into(),
            transient: false,
        }
    }

    fn transient(reason: impl Into<String>) -> Self {
        Self::Invalid {
            reason: reason.into(),
            transient: true,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ParticipantCursorDocument {
    version: u64,
    mail: BTreeMap<String, CursorSet>,
    channels: BTreeMap<String, CursorSet>,
}

/// One retry covers a state file that a concurrent writer is halfway through
/// replacing; it does NOT cover a malformed one, which must stay visible
/// instead of being retried until something else answers.
fn read_participant_cursor(path: &Path) -> ParticipantCursorRead {
    let first = read_participant_cursor_once(path);
    match first {
        ParticipantCursorRead::Invalid {
            transient: true, ..
        } => read_participant_cursor_once(path),
        other => other,
    }
}

/// One open, one verdict. The cursor file is opened once with
/// `O_NOFOLLOW|O_NONBLOCK|O_CLOEXEC`, and both the solitary-regular-file
/// check and the content read go through that held descriptor, the same
/// pattern as `mailbox::read_owner_file`. Two pathname operations (inspect,
/// then read) let a replacement between them pair one file's metadata with
/// another's content.
fn read_participant_cursor_once(path: &Path) -> ParticipantCursorRead {
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
            return ParticipantCursorRead::invalid(NOT_SOLITARY_REGULAR_FILE);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // A dangling symlink is a replaced state file, not an absent one.
            return match fs::symlink_metadata(path) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    ParticipantCursorRead::invalid(NOT_SOLITARY_REGULAR_FILE)
                }
                _ => ParticipantCursorRead::Missing,
            };
        }
        Err(error) => {
            return ParticipantCursorRead::transient(format!("cannot open file: {error}"));
        }
    };
    let metadata = match file.metadata() {
        Ok(metadata) => metadata,
        Err(error) => {
            return ParticipantCursorRead::transient(format!("cannot inspect file: {error}"));
        }
    };
    if !metadata.file_type().is_file() || metadata.nlink() != 1 {
        return ParticipantCursorRead::invalid(NOT_SOLITARY_REGULAR_FILE);
    }
    // Test seam: runs after the held descriptor passed its checks and before
    // the read, the exact window a path-based reread used to expose.
    #[cfg(test)]
    if let Some(hook) = CURSOR_READ_HOOK.with(|slot| slot.borrow_mut().take()) {
        hook();
    }
    let mut raw = Vec::new();
    if let Err(error) = std::io::Read::read_to_end(&mut file, &mut raw) {
        return ParticipantCursorRead::transient(format!("cannot read file: {error}"));
    }
    parse_participant_cursor(&raw)
        .map(ParticipantCursorRead::Valid)
        .unwrap_or_else(ParticipantCursorRead::invalid)
}

const NOT_SOLITARY_REGULAR_FILE: &str =
    "not a solitary regular file (a symlink, directory, or multiply-linked file is refused)";

#[cfg(test)]
thread_local! {
    static CURSOR_READ_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
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
    wait: LockWait,
    update: impl FnOnce(&mut ParticipantCursors) -> AppResult<T>,
) -> AppResult<T> {
    fs::create_dir_all(&participant.dir).map_err(|error| {
        AppError::io(
            "create participant cursor directory",
            &participant.dir,
            error,
        )
    })?;
    let _lock = lock_cursor_dir(&participant.dir, wait)?;
    let path = participant_cursor_path(participant);
    ensure_cursor_destination_safe(&path)?;
    let mut state = match read_participant_cursor(&path) {
        ParticipantCursorRead::Missing => ParticipantCursors::default(),
        ParticipantCursorRead::Valid(state) => state,
        ParticipantCursorRead::Invalid { reason, .. } => {
            return Err(AppError::config(
                &path,
                format!("participant cursors.json is malformed or unsafe ({reason}); refusing to discard its read state"),
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

/// How long a cursor-lock acquisition may wait for another holder. Every
/// consuming transaction blocks, as it always has; only the best-effort
/// post-commit seen update after a channel send takes a deadline, because the
/// message is already durable and an unbounded wait there held a finished
/// send's receipt hostage to whoever held the lock.
#[derive(Debug, Clone, Copy)]
pub(crate) enum LockWait {
    Blocking,
    Within(Duration),
}

/// Take `flock(LOCK_EX)` on `file`, either blocking or by polling
/// `LOCK_EX|LOCK_NB` with capped exponential backoff until `budget` elapses.
fn acquire_exclusive(file: &File, path: &Path, wait: LockWait) -> AppResult<()> {
    let budget = match wait {
        LockWait::Blocking => {
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == -1 {
                return Err(AppError::io(
                    "lock cursor state",
                    path,
                    std::io::Error::last_os_error(),
                ));
            }
            return Ok(());
        }
        LockWait::Within(budget) => budget,
    };
    let deadline = Instant::now() + budget;
    let mut backoff = Duration::from_millis(1);
    loop {
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        match error.raw_os_error() {
            Some(code) if code == libc::EWOULDBLOCK || code == libc::EINTR => {}
            _ => return Err(AppError::io("lock cursor state", path, error)),
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(AppError::io(
                "lock cursor state",
                path,
                format!("another process held the lock for longer than the {budget:?} budget"),
            ));
        }
        std::thread::sleep(backoff.min(deadline - now));
        backoff = (backoff * 2).min(Duration::from_millis(50));
    }
}

/// Block on one participant's `.cursors.lock` (the lock every participant
/// cursor writer holds across reload and replace). `rooms rename` takes it
/// for each `cursors.json` it rewrites, after its rename, participant, and
/// rooms locks; no holder of this lock ever takes those, so the order is
/// rename → participants → rooms → cursors.
pub(crate) fn lock_participant_cursors(participant_dir: &Path) -> AppResult<File> {
    lock_cursor_dir(participant_dir, LockWait::Blocking)
}

fn lock_cursor_dir(directory: &Path, wait: LockWait) -> AppResult<File> {
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
    acquire_exclusive(&file, &path, wait)?;
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
    consume_inner(context, room, delta, None, None, LockWait::Blocking).map(|_| ())
}

#[allow(dead_code)]
pub(crate) fn consume_channel(
    context: &Context,
    room: &str,
    channel: &str,
    ids: Vec<String>,
) -> AppResult<CursorAdvance> {
    consume_channel_waiting(context, room, channel, ids, LockWait::Blocking)
}

/// Legacy-room `consume_channel` bounded by `budget` on the cursor lock; see
/// `ParticipantCursors::consume_channel_within`.
pub(crate) fn consume_channel_within(
    context: &Context,
    room: &str,
    channel: &str,
    ids: Vec<String>,
    budget: Duration,
) -> AppResult<CursorAdvance> {
    consume_channel_waiting(context, room, channel, ids, LockWait::Within(budget))
}

fn consume_channel_waiting(
    context: &Context,
    room: &str,
    channel: &str,
    ids: Vec<String>,
    wait: LockWait,
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
        wait,
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
        LockWait::Blocking,
    )
}

fn consume_inner(
    context: &Context,
    room: &str,
    delta: Delta,
    outcome_channel: Option<&str>,
    through: Option<(&str, &str)>,
    wait: LockWait,
) -> AppResult<CursorAdvance> {
    validate_delta(&delta)?;
    let path = cursor_path(context, room)?;
    let parent = path
        .parent()
        .ok_or_else(|| AppError::invalid_argument("cursor path has no room directory"))?;
    fs::create_dir_all(parent)
        .map_err(|error| AppError::io("create cursor state directory", parent, error))?;
    let _lock = lock_room_cursors(context, room, wait)?;
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

fn lock_room_cursors(context: &Context, room: &str, wait: LockWait) -> AppResult<File> {
    let directory = cursor_path(context, room)?
        .parent()
        .expect("room cursor path has a parent")
        .to_path_buf();
    lock_cursor_dir(&directory, wait)
}

fn trusted_lock_metadata(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_file() && metadata.nlink() == 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel_state::ParticipantChannels;
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
    fn unread_channel_skipping_consumed_matches_the_complete_projection() {
        let (root, context) = context("fast-unread");
        let participant = crate::participant::bind_test_actor(&context, "alpha");
        ParticipantChannels::join(&context, &participant, "tax").expect("join channel");
        seed_message(&root, "tax", ID1, "beta");
        seed_message(&root, "tax", ID2, "beta");
        fs::write(
            root.join(CHANNELS_DIR).join("tax").join("channel.json"),
            r#"{"name":"tax","created":"2026-08-31 17:12:34 +0000","created_by":"alpha"}"#,
        )
        .expect("channel info");
        ParticipantCursors::consume_channel(&context, &participant, "tax", &[ID1.to_owned()])
            .expect("consume ID1");

        let full = super::eligibility::unread_channel(&context, &participant, "tax").expect("full");
        let fast =
            super::eligibility::unread_channel_skipping_consumed(&context, &participant, "tax")
                .expect("fast");
        // Equivalence is the whole license for the fast path: same ids, same
        // order, same bodies. Its one difference -- a consumed body is never
        // opened -- is not observed here on purpose: only the watch passes may
        // use this projection, and they run the complete validation that
        // reports a consumed file's corruption (`commands::watch` tests own
        // that assertion).
        assert_eq!(
            full.iter()
                .map(|item| (item.message.id.clone(), item.body.clone()))
                .collect::<Vec<_>>(),
            fast.iter()
                .map(|item| (item.message.id.clone(), item.body.clone()))
                .collect::<Vec<_>>(),
        );
        assert_eq!(fast.len(), 1, "exactly the unseen message is unread");
        assert_eq!(fast[0].message.id, ID2);
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

    /// A6: a participant cursor file that is a symlink (live or dangling) is
    /// still refused on read, never followed and never treated as missing.
    #[cfg(unix)]
    #[test]
    fn participant_cursor_symlink_is_refused_on_read() {
        let (root, context) = context("participant-symlink");
        let participant = crate::participant::bind_test_actor(&context, "alpha");
        fs::create_dir_all(&participant.dir).expect("participant dir");
        let valid = format!(
            "{{\"version\":2,\"mail\":{{}},\"channels\":{{\"tax\":{{\"seen\":[\"{ID1}\"]}}}}}}\n"
        );
        fs::write(root.join("target.json"), &valid).expect("target");
        let cursor = participant.dir.join(CURSORS_FILE);
        std::os::unix::fs::symlink(root.join("target.json"), &cursor).expect("symlink");
        assert!(participant_cursor_defect(&participant)
            .is_some_and(|reason| reason.contains("solitary regular file")));
        assert!(!ParticipantCursors::load(&context, &participant).channel_has_seen("tax", ID1));

        fs::remove_file(&cursor).expect("drop live symlink");
        std::os::unix::fs::symlink(root.join("absent.json"), &cursor).expect("dangling");
        assert!(participant_cursor_defect(&participant).is_some());
        trash_test_root(&root);
    }

    /// A6: the file validated is the file read. Between the check and the
    /// read the path is swapped for a symlink to a different, valid cursor
    /// document; a pathname reread would accept that content under the
    /// original file's verdict. The held descriptor still reads the original.
    #[cfg(unix)]
    #[test]
    fn participant_cursor_replaced_between_check_and_read_keeps_one_verdict() {
        let (root, context) = context("participant-swap");
        let participant = crate::participant::bind_test_actor(&context, "alpha");
        fs::create_dir_all(&participant.dir).expect("participant dir");
        let document = |id: &str| {
            format!(
                "{{\"version\":2,\"mail\":{{}},\"channels\":{{\"tax\":{{\"seen\":[\"{id}\"]}}}}}}\n"
            )
        };
        let cursor = participant.dir.join(CURSORS_FILE);
        fs::write(&cursor, document(ID1)).expect("original cursor");
        let impostor = root.join("impostor.json");
        fs::write(&impostor, document(ID2)).expect("impostor");
        let swap_link = participant.dir.join("swap.tmp");
        std::os::unix::fs::symlink(&impostor, &swap_link).expect("stage symlink");
        let (swap_from, swap_to) = (swap_link.clone(), cursor.clone());
        CURSOR_READ_HOOK.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                fs::rename(&swap_from, &swap_to).expect("swap cursor for symlink");
            }));
        });

        match read_participant_cursor_once(&cursor) {
            ParticipantCursorRead::Valid(state) => {
                assert!(
                    state.channel_has_seen("tax", ID1),
                    "read the validated file"
                );
                assert!(!state.channel_has_seen("tax", ID2), "never the impostor");
            }
            _ => panic!("the held descriptor's content is the verdict"),
        }
        assert!(cursor.is_symlink(), "the hook really swapped the path");
        trash_test_root(&root);
    }

    /// A1: the bounded variant gives up once its budget is spent while
    /// another open file description holds the lock, names the lock in the
    /// error, and writes nothing; after release it succeeds. A timer thread
    /// releases the holder after 1s, so an unbounded regression returns Ok
    /// late (and fails `expect_err`) instead of wedging the suite.
    #[cfg(unix)]
    #[test]
    fn bounded_participant_lock_times_out_names_the_lock_and_writes_nothing() {
        let (root, context) = context("bounded-lock");
        let participant = crate::participant::bind_test_actor(&context, "alpha");
        fs::create_dir_all(&participant.dir).expect("participant dir");
        let lock_path = participant.dir.join(CURSORS_LOCK_FILE);
        let holder = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .mode(0o600)
            .open(&lock_path)
            .expect("open lock as holder");
        assert_eq!(unsafe { libc::flock(holder.as_raw_fd(), libc::LOCK_EX) }, 0);
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(1));
            drop(holder);
        });

        let started = Instant::now();
        let error = ParticipantCursors::consume_channel_within(
            &context,
            &participant,
            "tax",
            &[ID1.to_owned()],
            Duration::from_millis(150),
        )
        .expect_err("a held lock must time out, not block");
        let waited = started.elapsed();
        assert!(
            waited >= Duration::from_millis(150) && waited < Duration::from_secs(5),
            "waited {waited:?}"
        );
        assert_eq!(error.code, ErrorCode::IoError);
        assert!(
            error.message.contains(CURSORS_LOCK_FILE),
            "error must name the lock: {}",
            error.message
        );
        assert!(!participant.dir.join(CURSORS_FILE).exists());

        releaser.join().expect("release holder");
        ParticipantCursors::consume_channel_within(
            &context,
            &participant,
            "tax",
            &[ID1.to_owned()],
            Duration::from_millis(150),
        )
        .expect("a free lock is taken within the budget");
        assert!(ParticipantCursors::load(&context, &participant).channel_has_seen("tax", ID1));
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
