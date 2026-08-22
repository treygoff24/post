//! Reader-owned channel seen-sets (v2).
//!
//! Lives at `<mail-root>/<room>/channel-state.json` (via
//! `channel::channel_state_path`). Shape v2:
//! `{"version": 2, "channels": {"<channel>": {"seen": ["<id>", ...]}}}` —
//! ids sorted, written pretty, replaced atomically under
//! `<root>/<room>/.channel-state.lock` (flock held across reload, mutate,
//! replace).
//!
//! The state is the exact set of message ids this room has consumed (read,
//! discarded, acked, or sent itself). Unread = file exists ∧ id ∉ seen ∧
//! from ≠ self. A bridged LATE ARRIVAL — a foreign id that sorts below the
//! newest seen id — is simply absent from the set, so it surfaces on the
//! next read. There are no watermarks and no arrival-time tracking: the
//! seen-set only grows, and an id is never un-seen.
//!
//! Legacy v1 stores hold a bare `{channel: last-read-id}` watermark map.
//! They migrate lazily: reads convert in memory (seen := every id currently
//! in messages/ that is ≤ the watermark); the first lock-held write converts
//! the file to v2 and backs the v1 bytes up alongside as
//! `.channel-state.v1.bak`. After a v2 write, v1 is never written again.
//!
//! Mixed binaries: a pre-seen-set binary cannot parse v2 state (its loader
//! expects the bare v1 map) and fails closed with `config_invalid` rather
//! than misreading it. Per the repo's migration-fence pattern
//! (`migration_fence.rs`), a store only reaches v2 through an enrolled,
//! generation-gated cutover — stale writers refuse at the fence before any
//! state mutation.
//!
//! Growth is O(channel history) — the same order as messages/ itself, which
//! every read already scans. Accepted (ponytail): no compaction until a
//! channel proves it needs one. If one ever does, the recorded policy is to
//! compact to {watermark + exception list} once the seen prefix is
//! contiguous with the messages directory, under a version bump.

use crate::channel::{channel_state_path, CHANNELS_DIR};
use crate::error::{AppError, AppResult, ErrorCode};
use crate::mailbox::{atomic_replace, Context};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;

pub(crate) const CHANNEL_STATE_LOCK_FILE: &str = ".channel-state.lock";
pub(crate) const STATE_VERSION: u64 = 2;
/// Where the legacy v1 cursor bytes land before the first v2 write, for
/// rollback: copy this back over `channel-state.json` (and use a pre-seen-set
/// binary) to return to the watermark model.
pub(crate) const V1_BACKUP_FILE: &str = ".channel-state.v1.bak";

/// What one seen-set mutation actually did, decided while the room lock was
/// held. Adding only already-seen ids changes nothing: `advanced: false`
/// with a byte-identical state file is the replay case, not an error — a
/// retried ack must be indistinguishable from the first one.
///
/// `prior`/`cursor` are the max seen id before/after the call. JSON output
/// keeps the field name `cursor` as a compatibility summary of the
/// underlying seen-set; it is not the model.
#[derive(Debug, Clone)]
pub(crate) struct CursorAdvance {
    pub prior: Option<String>,
    pub cursor: String,
    pub advanced: bool,
    /// Ids newly added by this call (zero on a replay).
    pub marked: usize,
}

#[derive(Debug, Default)]
pub(crate) struct ChannelState {
    channels: BTreeMap<String, BTreeSet<String>>,
}

/// What the raw bytes on disk parsed as, decided fresh under the lock so the
/// backup decision can never race a concurrent writer.
enum Stored {
    Missing,
    V1(BTreeMap<String, String>),
    V2(BTreeMap<String, BTreeSet<String>>),
}

impl ChannelState {
    pub(crate) fn load(context: &Context, room: &str) -> AppResult<Self> {
        match load_stored(context, room)? {
            Stored::Missing => Ok(Self::default()),
            Stored::V1(cursors) => Ok(migrate_v1_in_memory(context, &cursors)),
            Stored::V2(channels) => Ok(Self { channels }),
        }
    }

    /// Whether `id` has been consumed in `channel`: present in the seen-set,
    /// or — on a not-yet-written v1 store — implied by the migrated baseline.
    pub(crate) fn has_seen(&self, channel: &str, id: &str) -> bool {
        self.channels
            .get(channel)
            .is_some_and(|seen| seen.contains(id))
    }

    /// Max seen id for `channel`, the compatibility summary reported in the
    /// `cursor` output fields. Not the model; never used for filtering.
    pub(crate) fn max_seen(&self, channel: &str) -> Option<&str> {
        self.channels
            .get(channel)
            .and_then(|seen| seen.last())
            .map(String::as_str)
    }

    /// Consume the whole map (watch's read-only startup snapshot).
    pub(crate) fn into_channels(self) -> BTreeMap<String, BTreeSet<String>> {
        self.channels
    }

    /// Record exactly these ids as seen for `channel` (atomic set union).
    ///
    /// Callers must invoke this only AFTER the corresponding messages were
    /// fully emitted (the crash-safety invariant: never consume unemitted
    /// messages). A replay whose ids are all already seen adds nothing:
    /// `advanced: false`, state byte-identical.
    pub(crate) fn mark_seen<I>(
        context: &Context,
        room: &str,
        channel: &str,
        ids: I,
    ) -> AppResult<CursorAdvance>
    where
        I: IntoIterator<Item = String>,
    {
        let additions: BTreeSet<String> = ids.into_iter().collect();
        seal(context, room, channel, |state| {
            let empty = BTreeSet::new();
            let fresh: BTreeSet<String> = additions
                .difference(state.channels.get(channel).unwrap_or(&empty))
                .cloned()
                .collect();
            if fresh.is_empty() {
                Ok(None)
            } else {
                Ok(Some(fresh))
            }
        })
    }

    /// Mark every currently-existing unseen id ≤ `target` as seen — the
    /// targeted `--discard-through <id>` ack. Refuses when an unreadable
    /// message sits in the affected range: a message that cannot be rendered
    /// has certainly not been read. Replay-safe: a target whose whole range
    /// is already seen adds nothing (`advanced: false`).
    pub(crate) fn mark_seen_through(
        context: &Context,
        room: &str,
        channel: &str,
        target: &str,
    ) -> AppResult<CursorAdvance> {
        seal(context, room, channel, |state| {
            let empty = BTreeSet::new();
            let seen = state.channels.get(channel).unwrap_or(&empty);
            unseen_candidates(context, channel, seen, Some(target))
        })
    }

    /// Mark every currently-existing unseen id as seen — the deliberate
    /// `--discard` clear-my-backlog op.
    pub(crate) fn mark_all_seen(
        context: &Context,
        room: &str,
        channel: &str,
    ) -> AppResult<CursorAdvance> {
        seal(context, room, channel, |state| {
            let empty = BTreeSet::new();
            let seen = state.channels.get(channel).unwrap_or(&empty);
            unseen_candidates(context, channel, seen, None)
        })
    }
}

/// Reload, compute additions, back up any v1 bytes, union, and atomically
/// replace — ALL under the room's cursor flock. Without the lock, two
/// processes acking different channels each write back a whole-map snapshot
/// taken before the other's write, and the loser's marks are lost.
fn seal<F>(context: &Context, room: &str, channel: &str, compute: F) -> AppResult<CursorAdvance>
where
    F: FnOnce(&mut ChannelState) -> AppResult<Option<BTreeSet<String>>>,
{
    let path = channel_state_path(context, room)?;
    // A room that has never received mail has no <root>/<room>/ yet, and
    // neither the lock file nor atomic_replace can create parents — without
    // this, such a room can read but never consume, re-showing the backlog
    // forever (found by live smoke; the lane's unit tests pre-created the
    // room dir).
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| AppError::io("create channel state directory", parent, error))?;
    }
    let _lock = lock_room_cursors(context, room)?;
    // Reload under the lock: our caller's snapshot may be stale.
    let mut state = ChannelState::load(context, room)?;
    let prior_max = state.max_seen(channel).map(str::to_owned);
    let Some(additions) = compute(&mut state)? else {
        let cursor = prior_max.clone().unwrap_or_default();
        return Ok(CursorAdvance {
            prior: prior_max,
            cursor,
            advanced: false,
            marked: 0,
        });
    };
    // A v1→v2 REPLACEMENT on disk bricks a pre-seen-set binary (it cannot
    // parse v2), so it is legal only once the migration fence reports an
    // activated cutover. In-memory migrated reads stay allowed unfenced.
    let raw = match std::fs::read(&path) {
        Ok(raw) => Some(raw),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(AppError::io("read channel state", &path, error)),
    };
    let legacy_v1 = raw
        .as_deref()
        .is_some_and(|raw| matches!(parse_stored(raw), Ok(Stored::V1(_))));
    if legacy_v1 {
        crate::migration_fence::require_activated_for_state_migration(context)?;
        let backup = path.with_file_name(V1_BACKUP_FILE);
        atomic_replace(
            &backup,
            raw.as_deref().expect("legacy_v1 implies the file was read"),
        )
        .map_err(|error| AppError::io("back up legacy channel state", &backup, error))?;
    }
    for id in &additions {
        state
            .channels
            .entry(channel.to_owned())
            .or_default()
            .insert(id.clone());
    }
    let bytes = serialize_v2(&state)?;
    atomic_replace(&path, &bytes)
        .map_err(|error| AppError::io("atomically update channel state", &path, error))?;
    let cursor = state
        .max_seen(channel)
        .expect("a union that added ids leaves at least one")
        .to_owned();
    Ok(CursorAdvance {
        prior: prior_max,
        cursor,
        advanced: true,
        marked: additions.len(),
    })
}

/// Every currently-existing unseen message id in the channel, optionally
/// bounded above by `target`, with each candidate parse-checked: an
/// unreadable `.msg` in the affected range refuses the mutation (fail-closed,
/// same rule as a consuming read). Runs under the room's cursor lock via
/// `seal`.
fn unseen_candidates(
    context: &Context,
    channel: &str,
    seen: &BTreeSet<String>,
    target: Option<&str>,
) -> AppResult<Option<BTreeSet<String>>> {
    let directory = context
        .root
        .join(CHANNELS_DIR)
        .join(channel)
        .join("messages");
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
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
        let entry = entry
            .map_err(|error| AppError::io("read channel messages entry", &directory, error))?;
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|value| value.to_str()) == Some("msg") {
            paths.push(path);
        }
    }
    paths.sort();
    let mut candidates = BTreeSet::new();
    for path in paths {
        let Some(id) = path.file_stem().and_then(|value| value.to_str()) else {
            // A non-UTF-8 stem cannot be ordered against the target, so it
            // cannot be shown to sit beyond it: treat it as in-range and let
            // the parse check refuse rather than guess.
            crate::channel::parse_channel_message(&path)?;
            continue;
        };
        if seen.contains(id) {
            continue;
        }
        if target.is_some_and(|target| id > target) {
            continue;
        }
        crate::channel::parse_channel_message(&path)?;
        candidates.insert(id.to_owned());
    }
    if candidates.is_empty() {
        Ok(None)
    } else {
        Ok(Some(candidates))
    }
}

fn serialize_v2(state: &ChannelState) -> AppResult<Vec<u8>> {
    #[derive(serde::Serialize)]
    struct StoredChannel<'a> {
        seen: &'a BTreeSet<String>,
    }
    #[derive(serde::Serialize)]
    struct StoredV2<'a> {
        version: u64,
        channels: BTreeMap<&'a str, StoredChannel<'a>>,
    }
    let stored = StoredV2 {
        version: STATE_VERSION,
        channels: state
            .channels
            .iter()
            .map(|(channel, seen)| (channel.as_str(), StoredChannel { seen }))
            .collect(),
    };
    let mut bytes = serde_json::to_vec_pretty(&stored).map_err(|error| {
        AppError::new(
            ErrorCode::IoError,
            format!("failed to serialize channel state: {error}"),
            "Retry the read; the seen-set was not updated.",
        )
    })?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Read and classify the raw state bytes. A bare string→string map is the
/// legacy v1 watermark file; anything else must be exactly the v2 shape.
fn load_stored(context: &Context, room: &str) -> AppResult<Stored> {
    let path = channel_state_path(context, room)?;
    let raw = match std::fs::read(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Stored::Missing),
        Err(error) => return Err(AppError::io("read channel state", &path, error)),
    };
    parse_stored(&raw).map_err(|reason| corrupt_state_error(&path, &reason))
}

fn parse_stored(raw: &[u8]) -> Result<Stored, String> {
    let value: serde_json::Value =
        serde_json::from_slice(raw).map_err(|error| format!("not valid JSON: {error}"))?;
    let object = value
        .as_object()
        .ok_or_else(|| "not a JSON object".to_owned())?;
    match object.get("version") {
        Some(version) => {
            let version = version
                .as_u64()
                .ok_or_else(|| "\"version\" is not an integer".to_owned())?;
            if version != STATE_VERSION {
                return Err(format!(
                    "unsupported channel-state version {version} (this binary writes {STATE_VERSION})"
                ));
            }
            let channels_value = object
                .get("channels")
                .ok_or_else(|| "v2 state is missing \"channels\"".to_owned())?;
            let channels = channels_value
                .as_object()
                .ok_or_else(|| "\"channels\" is not a JSON object".to_owned())?;
            let mut parsed = BTreeMap::new();
            for (channel, entry) in channels {
                let seen = entry
                    .as_object()
                    .ok_or_else(|| format!("channel '{channel}' entry is not a JSON object"))?
                    .get("seen")
                    .ok_or_else(|| format!("channel '{channel}' entry is missing \"seen\""))?
                    .as_array()
                    .ok_or_else(|| format!("channel '{channel}' \"seen\" is not an array"))?
                    .iter()
                    .enumerate()
                    .map(|(index, id)| {
                        id.as_str().map(str::to_owned).ok_or_else(|| {
                            format!("channel '{channel}' seen[{index}] is not a string")
                        })
                    })
                    .collect::<Result<BTreeSet<String>, String>>()?;
                parsed.insert(channel.clone(), seen);
            }
            Ok(Stored::V2(parsed))
        }
        None => {
            let cursors: BTreeMap<String, String> =
                serde_json::from_slice(raw).map_err(|error| {
                    format!(
                        "not a v1 {{channel: last-read-id}} map or a v2 seen-set document: {error}"
                    )
                })?;
            Ok(Stored::V1(cursors))
        }
    }
}

/// Doctor shares the parser: a file is valid iff it is a legacy v1 watermark
/// map or a well-formed v2 seen-set document.
pub(crate) fn stored_shape_is_valid(bytes: &[u8]) -> bool {
    parse_stored(bytes).is_ok()
}

/// In-memory v1 → v2 conversion (no write): the exact baseline is every id
/// currently in messages/ that is ≤ the watermark. A later-arriving OLDER id
/// is then absent from the set ⇒ unread, which is precisely the late-arrival
/// property the watermark model lacked.
fn migrate_v1_in_memory(context: &Context, cursors: &BTreeMap<String, String>) -> ChannelState {
    let mut channels = BTreeMap::new();
    for (channel, cursor) in cursors {
        let mut seen = BTreeSet::new();
        let directory = context
            .root
            .join(CHANNELS_DIR)
            .join(channel)
            .join("messages");
        if let Ok(files) = crate::channel::message_files(&directory) {
            for path in files {
                if let Some(id) = path.file_stem().and_then(|value| value.to_str()) {
                    if id <= cursor.as_str() {
                        seen.insert(id.to_owned());
                    }
                }
            }
        }
        channels.insert(channel.clone(), seen);
    }
    ChannelState { channels }
}

fn corrupt_state_error(path: &std::path::Path, reason: &str) -> AppError {
    AppError::new(
        ErrorCode::ConfigInvalid,
        format!("channel state at {} is invalid: {reason}", path.display()),
        format!(
            "Fix or remove {} — removing only re-shows channel backlog, it cannot lose messages.",
            path.display()
        ),
    )
    .path(path.display().to_string())
    .reason(reason.to_owned())
}

/// Exclusive interprocess lock over one room's seen-set map. The returned
/// file holds the lock; it releases when dropped (or when the process dies,
/// so a crashed reader cannot wedge the room).
///
/// The room directory must already exist — callers create it first, because a
/// lock file is not the thing that should be quietly conjuring room state.
fn lock_room_cursors(context: &Context, room: &str) -> AppResult<File> {
    let path = channel_state_path(context, room)?.with_file_name(CHANNEL_STATE_LOCK_FILE);
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&path)
        .map_err(|error| AppError::io("open channel cursor lock", &path, error))?;
    // SAFETY: `file` owns a live descriptor for the whole call, and the
    // return code is checked before the lock is assumed held.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == -1 {
        return Err(AppError::io(
            "lock channel cursors",
            &path,
            std::io::Error::last_os_error(),
        ));
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ChannelMessage;
    use crate::test_support::{test_root, trash_test_root};

    const ID1: &str = "20260722-013000-000001-aaa111";
    const ID2: &str = "20260722-013000-000002-bbb222";
    const ID3: &str = "20260722-014000-000001-ccc333";

    fn state_context(label: &str) -> (std::path::PathBuf, Context) {
        let root = test_root(&format!("chanstate-{label}"));
        std::fs::create_dir_all(root.join("alpha")).expect("create room dir");
        (
            root.clone(),
            Context {
                root: root.clone(),
                home: root,
            },
        )
    }

    fn seed_message(root: &std::path::Path, channel: &str, id: &str, from: &str) {
        let dir = root.join(CHANNELS_DIR).join(channel);
        std::fs::create_dir_all(dir.join("messages")).expect("create channel dirs");
        let message = ChannelMessage {
            id: id.to_owned(),
            from: from.to_owned(),
            channel: channel.to_owned(),
            subject: String::new(),
            sent: "2026-07-22 01:30:00 -0500".to_owned(),
            event: None,
            display_name: None,
            pfp: None,
            re: None,
            mentions: vec![],
            signature_ref: None,
            sender_address: None,
            sender_provenance: None,
        };
        let bytes = crate::channel::encode_message(&message, "body").expect("encode");
        std::fs::write(dir.join("messages").join(format!("{id}.msg")), bytes)
            .expect("write message");
    }

    fn write_v1_state(root: &std::path::Path, room: &str, json: &str) {
        std::fs::write(root.join(room).join("channel-state.json"), json).expect("write v1 state");
    }

    fn read_state_bytes(root: &std::path::Path, room: &str) -> Vec<u8> {
        std::fs::read(root.join(room).join("channel-state.json")).expect("read state file")
    }

    /// Enroll and activate the migration fence on this test root: the cutover
    /// is complete, so v1→v2 disk replacements are admitted.
    fn enroll_activated_fence(root: &std::path::Path) {
        let context = Context {
            root: root.to_owned(),
            home: root.to_owned(),
        };
        crate::migration_fence::fence(&context, 1).expect("fence store");
        crate::migration_fence::activate(&context, 1).expect("activate store");
    }

    #[test]
    fn mark_seen_works_for_a_room_that_never_received_mail() {
        // No <root>/<room>/ directory exists yet: the mutation must create it
        // rather than failing forever until the room's first mail arrives.
        let (root, context) = state_context("freshroom");
        ChannelState::mark_seen(&context, "never-mailed", "taxonomy", [ID1.to_owned()])
            .expect("mark_seen must create the room state directory");
        let state = ChannelState::load(&context, "never-mailed").expect("reload");
        assert!(state.has_seen("taxonomy", ID1));
        trash_test_root(&root);
    }

    #[test]
    fn missing_state_file_is_empty_state() {
        let (root, context) = state_context("missing");
        let state = ChannelState::load(&context, "alpha").expect("load");
        assert!(!state.has_seen("taxonomy", ID1));
        assert_eq!(state.max_seen("taxonomy"), None);
        trash_test_root(&root);
    }

    #[test]
    fn mark_seen_persists_and_reloads() {
        let (root, context) = state_context("mark");
        ChannelState::mark_seen(&context, "alpha", "taxonomy", [ID1.to_owned()])
            .expect("mark seen");
        let state = ChannelState::load(&context, "alpha").expect("reload");
        assert!(state.has_seen("taxonomy", ID1));
        assert!(!state.has_seen("taxonomy", ID2));
        assert_eq!(state.max_seen("taxonomy"), Some(ID1));
        trash_test_root(&root);
    }

    #[test]
    fn replaying_a_marked_id_is_not_an_advance_and_leaves_the_file_byte_identical() {
        let (root, context) = state_context("replay");
        ChannelState::mark_seen(&context, "alpha", "taxonomy", [ID2.to_owned()])
            .expect("mark seen");
        let before = read_state_bytes(&root, "alpha");

        let replay = ChannelState::mark_seen(&context, "alpha", "taxonomy", [ID2.to_owned()])
            .expect("replaying the same id is success, not an error");
        assert_eq!(replay.prior.as_deref(), Some(ID2));
        assert_eq!(replay.cursor, ID2);
        assert!(!replay.advanced);
        assert_eq!(replay.marked, 0);
        assert_eq!(
            read_state_bytes(&root, "alpha"),
            before,
            "a replay must leave the state file byte-identical"
        );

        let behind = ChannelState::mark_seen(&context, "alpha", "taxonomy", [ID1.to_owned()])
            .expect("an id below the newest seen is still just a union member");
        assert_eq!(behind.prior.as_deref(), Some(ID2));
        assert_eq!(behind.cursor, ID2, "max seen does not move backward");
        assert!(behind.advanced, "ID1 itself was newly recorded");
        assert_eq!(behind.marked, 1);

        trash_test_root(&root);
    }

    #[test]
    fn seen_sets_are_per_channel_and_per_room() {
        let (root, context) = state_context("perchan");
        std::fs::create_dir_all(root.join("beta")).expect("create second room dir");
        ChannelState::mark_seen(&context, "alpha", "taxonomy", [ID1.to_owned()]).expect("mark");
        ChannelState::mark_seen(&context, "alpha", "build", [ID2.to_owned()]).expect("mark");
        ChannelState::mark_seen(&context, "beta", "taxonomy", [ID3.to_owned()]).expect("mark");
        let alpha = ChannelState::load(&context, "alpha").expect("reload alpha");
        let beta = ChannelState::load(&context, "beta").expect("reload beta");
        assert!(alpha.has_seen("taxonomy", ID1));
        assert!(!alpha.has_seen("build", ID1));
        assert!(alpha.has_seen("build", ID2));
        assert!(beta.has_seen("taxonomy", ID3));
        assert!(!beta.has_seen("taxonomy", ID1));
        trash_test_root(&root);
    }

    #[test]
    fn v1_store_migrates_in_memory_on_read_without_writing() {
        let (root, context) = state_context("v1read");
        seed_message(&root, "tax", ID1, "beta");
        seed_message(&root, "tax", ID2, "beta");
        seed_message(&root, "tax", ID3, "beta");
        write_v1_state(
            &root,
            "alpha",
            r#"{"tax": "20260722-013000-000002-bbb222"}"#,
        );
        // The v1 watermark is ID2's full stem: the baseline is ids <= it.
        let state = ChannelState::load(&context, "alpha").expect("migrating load");
        assert!(
            state.has_seen("tax", ID1),
            "id below the watermark migrates seen"
        );
        assert!(
            state.has_seen("tax", ID2),
            "the watermark id itself migrates seen"
        );
        assert!(
            !state.has_seen("tax", ID3),
            "id above the watermark stays unread"
        );
        // Read-only paths do NOT rewrite the file.
        let raw = read_state_bytes(&root, "alpha");
        serde_json::from_slice::<BTreeMap<String, String>>(&raw)
            .expect("file must still be the bare v1 map after a read");
        trash_test_root(&root);
    }

    #[test]
    fn first_write_converts_v1_to_v2_with_backup_and_exact_baseline() {
        let (root, context) = state_context("v1write");
        seed_message(&root, "tax", ID1, "beta");
        seed_message(&root, "tax", ID2, "beta");
        seed_message(&root, "tax", ID3, "beta");
        let v1 = r#"{"tax": "20260722-013000-000002-bbb222"}"#;
        write_v1_state(&root, "alpha", v1);
        // Conversion replaces the file on disk, which requires the activated
        // migration cutover (finding 4).
        enroll_activated_fence(&root);

        // A late-arriving OLDER id would be absent from the baseline...
        ChannelState::mark_seen(&context, "alpha", "tax", [ID3.to_owned()])
            .expect("first lock-held write converts the store");
        let raw = read_state_bytes(&root, "alpha");
        let converted: serde_json::Value =
            serde_json::from_slice(&raw).expect("converted state is JSON");
        assert_eq!(converted["version"], 2, "the file is now v2");
        let seen = converted["channels"]["tax"]["seen"]
            .as_array()
            .expect("seen array");
        let seen: Vec<&str> = seen.iter().filter_map(|v| v.as_str()).collect();
        assert_eq!(
            seen,
            vec![ID1, ID2, ID3],
            "baseline = every existing id <= the v1 watermark, plus the new mark"
        );
        let backup = root.join("alpha").join(V1_BACKUP_FILE);
        let backed_up = std::fs::read_to_string(&backup).expect("v1 backup exists");
        assert_eq!(backed_up, v1, "backup holds the original v1 bytes");
        // And v1 is never written again: another mutation stays v2.
        ChannelState::mark_seen(&context, "alpha", "other", [ID1.to_owned()]).expect("mark");
        let again: serde_json::Value =
            serde_json::from_slice(&read_state_bytes(&root, "alpha")).expect("JSON");
        assert_eq!(again["version"], 2);
        trash_test_root(&root);
    }

    #[test]
    fn late_arrival_below_the_newest_seen_id_is_unseen() {
        // The M4 core property the watermark model could not express: T1 and
        // T3 consumed, T2 arrives afterwards (the bridge) and MUST surface.
        let (root, context) = state_context("latearrival");
        ChannelState::mark_seen(&context, "alpha", "tax", [ID1.to_owned()]).expect("read T1");
        ChannelState::mark_seen(&context, "alpha", "tax", [ID3.to_owned()]).expect("send T3");
        seed_message(&root, "tax", ID2, "beta");
        let state = ChannelState::load(&context, "alpha").expect("reload");
        assert!(state.has_seen("tax", ID1));
        assert!(state.has_seen("tax", ID3));
        assert!(
            !state.has_seen("tax", ID2),
            "the bridged late arrival is unread"
        );
        assert_eq!(state.max_seen("tax"), Some(ID3));
        trash_test_root(&root);
    }

    #[test]
    fn mark_seen_through_marks_exactly_the_existing_unseen_span() {
        let (root, context) = state_context("through");
        seed_message(&root, "tax", ID1, "beta");
        seed_message(&root, "tax", ID2, "beta");
        seed_message(&root, "tax", ID3, "beta");
        // T2 arrives late, AFTER T1 was consumed but BEFORE the ack through T3.
        ChannelState::mark_seen(&context, "alpha", "tax", [ID1.to_owned()]).expect("read T1");
        seed_message(&root, "tax", ID3, "beta");
        let outcome = ChannelState::mark_seen_through(&context, "alpha", "tax", ID3).expect("ack");
        assert!(outcome.advanced);
        assert_eq!(outcome.marked, 2, "exactly T2 and T3 were newly recorded");
        assert_eq!(outcome.prior.as_deref(), Some(ID1));
        assert_eq!(outcome.cursor, ID3);
        let replay =
            ChannelState::mark_seen_through(&context, "alpha", "tax", ID3).expect("replay");
        assert!(!replay.advanced);
        assert_eq!(replay.marked, 0);
        trash_test_root(&root);
    }

    #[test]
    fn mark_seen_through_refuses_an_unreadable_message_in_range() {
        let (root, context) = state_context("throughbad");
        seed_message(&root, "tax", ID1, "beta");
        seed_message(&root, "tax", ID2, "beta");
        let path = root
            .join(CHANNELS_DIR)
            .join("tax")
            .join("messages")
            .join(format!("{ID2}.msg"));
        std::fs::write(&path, b"garbage").expect("corrupt T2");
        let error = ChannelState::mark_seen_through(&context, "alpha", "tax", ID3)
            .expect_err("unreadable in-range message must refuse");
        assert_eq!(error.code.as_str(), "config_invalid");
        let state = ChannelState::load(&context, "alpha").expect("reload");
        assert!(
            !state.has_seen("tax", ID1),
            "refusal leaves the set untouched"
        );
        trash_test_root(&root);
    }

    #[test]
    fn mark_all_seen_consumes_every_currently_existing_unseen_id() {
        let (root, context) = state_context("allseen");
        seed_message(&root, "tax", ID1, "beta");
        seed_message(&root, "tax", ID2, "beta");
        let outcome = ChannelState::mark_all_seen(&context, "alpha", "tax").expect("discard");
        assert!(outcome.advanced);
        assert_eq!(outcome.marked, 2);
        assert_eq!(outcome.cursor, ID2);
        // An empty backlog replays as success without an advance.
        let empty = ChannelState::mark_all_seen(&context, "alpha", "tax").expect("empty");
        assert!(!empty.advanced);
        assert_eq!(empty.cursor, ID2);
        trash_test_root(&root);
    }

    #[test]
    fn cursor_lock_is_private_and_excludes_a_second_writer() {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        use std::os::unix::io::AsRawFd;

        let (root, context) = state_context("lock");
        let lock = super::lock_room_cursors(&context, "alpha").expect("acquire cursor lock");
        let lock_path = root.join("alpha").join(CHANNEL_STATE_LOCK_FILE);
        let contender = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&lock_path)
            .expect("open second lock handle");
        assert_eq!(
            unsafe { libc::flock(contender.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            -1,
            "a second process must not hold the same room's cursor lock"
        );
        assert_eq!(
            std::io::Error::last_os_error().kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert_eq!(
            std::fs::metadata(&lock_path)
                .expect("inspect cursor lock")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        // Release is proved by `concurrent_marks_on_two_channels_both_survive`,
        // whose eight workers take the lock BLOCKING in turn — an unreleased
        // lock hangs it. It is deliberately not asserted here: re-acquiring
        // through an fd that was already open when the holder closed is racy
        // on Darwin (observed EWOULDBLOCK under parallel test load), and that
        // shape never occurs in post, which opens the lock fresh per call.
        drop(lock);
        trash_test_root(&root);
    }

    #[test]
    fn concurrent_marks_on_two_channels_both_survive() {
        // The exact race the lock exists for: two writers holding whole-map
        // snapshots taken before the other's write. Unlocked, the loser's
        // channel silently reverts to its pre-mutation set.
        let (root, context) = state_context("concurrent");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let mut handles = Vec::new();
        for worker in 0..8 {
            let context = context.clone();
            let barrier = std::sync::Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                let channel = format!("chan{worker}");
                let id = format!("20260722-013000-00000{worker}-aaa111");
                barrier.wait();
                ChannelState::mark_seen(&context, "alpha", &channel, [id]).expect("mark seen");
            }));
        }
        for handle in handles {
            handle.join().expect("worker thread");
        }
        let state = ChannelState::load(&context, "alpha").expect("reload");
        for worker in 0..8 {
            let id = format!("20260722-013000-00000{worker}-aaa111");
            assert!(
                state.has_seen(&format!("chan{worker}"), &id),
                "channel {worker}'s mark was lost"
            );
        }
        trash_test_root(&root);
    }

    #[test]
    fn corrupt_state_is_a_config_error_not_a_panic() {
        let (root, context) = state_context("corrupt");
        std::fs::write(root.join("alpha").join("channel-state.json"), b"{not json")
            .expect("write corrupt");
        let error = ChannelState::load(&context, "alpha").expect_err("corrupt state must error");
        assert_eq!(error.code.as_str(), "config_invalid");
        trash_test_root(&root);
    }

    #[test]
    fn unknown_version_is_a_loud_config_error_never_a_silent_guess() {
        let (root, context) = state_context("futurever");
        write_v1_state(&root, "alpha", r#"{"version": 99, "channels": {}}"#);
        let error = ChannelState::load(&context, "alpha").expect_err("unknown version");
        assert_eq!(error.code.as_str(), "config_invalid");
        trash_test_root(&root);
    }

    #[test]
    fn malformed_v2_shape_is_a_config_error() {
        let (root, context) = state_context("badv2");
        write_v1_state(
            &root,
            "alpha",
            r#"{"version": 2, "channels": {"tax": {"ids": []}}}"#,
        );
        let error = ChannelState::load(&context, "alpha").expect_err("missing seen key");
        assert_eq!(error.code.as_str(), "config_invalid");
        trash_test_root(&root);
    }

    #[test]
    fn unfenced_v1_conversion_is_refused_and_the_v1_file_stays_intact() {
        let (root, context) = state_context("v1unfenced");
        seed_message(&root, "tax", ID1, "beta");
        seed_message(&root, "tax", ID2, "beta");
        let v1 = r#"{"tax": "20260722-013000-000002-bbb222"}"#;
        write_v1_state(&root, "alpha", v1);

        let error = ChannelState::mark_seen(&context, "alpha", "tax", [ID3.to_owned()])
            .expect_err("unfenced conversion must refuse");
        assert_eq!(error.code.as_str(), "config_invalid");
        assert!(
            error.message.contains("cutover"),
            "refusal must name the cutover requirement: {}",
            error.message
        );
        // The v1 bytes are untouched and no backup was taken.
        assert_eq!(read_state_bytes(&root, "alpha"), v1.as_bytes());
        assert!(
            !root.join("alpha").join(V1_BACKUP_FILE).exists(),
            "no backup may appear behind a refusal"
        );
        trash_test_root(&root);
    }

    #[test]
    fn enrolled_store_converts_v1_to_v2_with_backup() {
        let (root, context) = state_context("v1enrolled");
        seed_message(&root, "tax", ID1, "beta");
        let v1 = r#"{"tax": "20260722-013000-000001-aaa111"}"#;
        write_v1_state(&root, "alpha", v1);
        enroll_activated_fence(&root);

        ChannelState::mark_seen(&context, "alpha", "tax", [ID2.to_owned()])
            .expect("activated cutover admits the conversion");
        let converted: serde_json::Value =
            serde_json::from_slice(&read_state_bytes(&root, "alpha")).expect("v2 JSON");
        assert_eq!(converted["version"], 2);
        let backup = std::fs::read_to_string(root.join("alpha").join(V1_BACKUP_FILE))
            .expect("v1 backup exists");
        assert_eq!(backup, v1, "backup holds the original v1 bytes");
        trash_test_root(&root);
    }

    #[test]
    fn restored_backup_on_an_unfenced_store_stays_v1() {
        let (root, context) = state_context("v1restore");
        seed_message(&root, "tax", ID1, "beta");
        let v1 = r#"{"tax": "20260722-013000-000001-aaa111"}"#;
        write_v1_state(&root, "alpha", v1);
        enroll_activated_fence(&root);
        ChannelState::mark_seen(&context, "alpha", "tax", [ID2.to_owned()])
            .expect("convert while enrolled");

        // Rollback: restore the v1 bytes and unenroll the cutover.
        let backup = std::fs::read(root.join("alpha").join(V1_BACKUP_FILE)).expect("backup");
        std::fs::write(root.join("alpha").join("channel-state.json"), backup).expect("restore");
        std::fs::remove_file(root.join(crate::migration_fence::STATE_FILE))
            .expect("unenroll store");

        let error = ChannelState::mark_seen(&context, "alpha", "tax", [ID2.to_owned()])
            .expect_err("restored v1 on an unfenced store must stay v1");
        assert_eq!(error.code.as_str(), "config_invalid");
        assert_eq!(
            read_state_bytes(&root, "alpha"),
            v1.as_bytes(),
            "the restored v1 file is untouched"
        );
        trash_test_root(&root);
    }
}
