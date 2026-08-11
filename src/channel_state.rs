//! Reader-owned channel read cursors.
//!
//! Lives at `<mail-root>/<room>/channel-state.json` (via
//! `channel::channel_state_path`), shape `{"<channel>": "<last-read-id>"}`.
//! The channel tree stays append-only-by-senders; a reader's cursor dying or
//! resetting can never corrupt shared history — it only re-shows backlog.
//!
//! The file holds every channel's cursor for one room, so two processes acking
//! *different* channels still read-modify-write the same map. Every mutation
//! therefore runs under `<mail-root>/<room>/.channel-state.lock` (flock),
//! held across reload + monotonic check + atomic replace.

use crate::channel::{channel_state_path, ChannelStateMap};
use crate::error::{AppError, AppResult, ErrorCode};
use crate::mailbox::{atomic_replace, Context};
use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;

pub(crate) const CHANNEL_STATE_LOCK_FILE: &str = ".channel-state.lock";

/// What one cursor advance actually did, decided while the room lock was held.
/// `advanced: false` with an unchanged `cursor` is the replay case, not an
/// error: a retried ack must be indistinguishable from the first one.
#[derive(Debug, Clone)]
pub(crate) struct CursorAdvance {
    pub prior: Option<String>,
    pub cursor: String,
    pub advanced: bool,
}

#[derive(Debug, Default)]
pub(crate) struct ChannelState {
    cursors: ChannelStateMap,
}

impl ChannelState {
    pub(crate) fn load(context: &Context, room: &str) -> AppResult<Self> {
        let path = channel_state_path(context, room)?;
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(AppError::io("read channel state", &path, error));
            }
        };
        let cursors: ChannelStateMap = serde_json::from_str(&raw).map_err(|error| {
            AppError::new(
                ErrorCode::ConfigInvalid,
                format!(
                    "channel state at {} is not valid JSON: {error}",
                    path.display()
                ),
                format!(
                    "Fix or remove {} — removing only re-shows channel backlog, it cannot lose messages.",
                    path.display()
                ),
            )
        })?;
        Ok(Self { cursors })
    }

    pub(crate) fn cursor(&self, channel: &str) -> Option<&str> {
        self.cursors.get(channel).map(String::as_str)
    }

    /// Persist `last_read_id` as the cursor for `channel`.
    ///
    /// Callers must invoke this only AFTER the corresponding messages were
    /// fully emitted (the crash-safety invariant: never advance past
    /// unemitted messages). A stale advance (id at or behind the stored
    /// cursor) is a no-op, so replayed or crossed reads never move it
    /// backward.
    ///
    /// Reload, monotonic check, and replace all run under the room's cursor
    /// flock. Without it, two processes acking different channels each write
    /// back a whole-map snapshot taken before the other's write, and the
    /// loser's advance is lost.
    pub(crate) fn advance(
        context: &Context,
        room: &str,
        channel: &str,
        last_read_id: &str,
    ) -> AppResult<CursorAdvance> {
        let path = channel_state_path(context, room)?;
        // A room that has never received mail has no <root>/<room>/ yet, and
        // neither the lock file nor atomic_replace can create parents —
        // without this, such a room can read but never advance, re-showing
        // the backlog forever (found by live smoke; the lane's unit tests
        // pre-created the room dir).
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| AppError::io("create channel state directory", parent, error))?;
        }
        let _lock = lock_room_cursors(context, room)?;
        let mut state = Self::load(context, room)?;
        let prior = state.cursors.get(channel).cloned();
        if let Some(current) = prior.clone() {
            if current.as_str() >= last_read_id {
                return Ok(CursorAdvance {
                    prior,
                    cursor: current,
                    advanced: false,
                });
            }
        }
        state
            .cursors
            .insert(channel.to_owned(), last_read_id.to_owned());
        let mut bytes = serde_json::to_vec_pretty(&state.cursors).map_err(|error| {
            AppError::new(
                ErrorCode::IoError,
                format!("failed to serialize channel state: {error}"),
                "Retry the read; the cursor was not advanced.",
            )
        })?;
        bytes.push(b'\n');
        atomic_replace(&path, &bytes)
            .map_err(|error| AppError::io("atomically update channel state", &path, error))?;
        Ok(CursorAdvance {
            prior,
            cursor: last_read_id.to_owned(),
            advanced: true,
        })
    }
}

/// Exclusive interprocess lock over one room's cursor map. The returned file
/// holds the lock; it releases when dropped (or when the process dies, so a
/// crashed reader cannot wedge the room).
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
    use crate::test_support::{test_root, trash_test_root};

    const ID1: &str = "20260722-013000-000001-aaa111";
    const ID2: &str = "20260722-013000-000002-bbb222";

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

    #[test]
    fn advance_works_for_a_room_that_never_received_mail() {
        // No <root>/<room>/ directory exists yet: advance must create it
        // rather than failing forever until the room's first mail arrives.
        let (root, context) = state_context("freshroom");
        ChannelState::advance(
            &context,
            "never-mailed",
            "taxonomy",
            "20260722-013000-000001-abc123",
        )
        .expect("advance must create the room state directory");
        let state = ChannelState::load(&context, "never-mailed").expect("reload");
        assert_eq!(
            state.cursor("taxonomy"),
            Some("20260722-013000-000001-abc123")
        );
        trash_test_root(&root);
    }

    #[test]
    fn missing_state_file_is_empty_state() {
        let (root, context) = state_context("missing");
        let state = ChannelState::load(&context, "alpha").expect("load");
        assert!(state.cursor("taxonomy").is_none());
        trash_test_root(&root);
    }

    #[test]
    fn advance_persists_and_reloads() {
        let (root, context) = state_context("advance");
        ChannelState::advance(
            &context,
            "alpha",
            "taxonomy",
            "20260722-013000-000001-abc123",
        )
        .expect("advance");
        let state = ChannelState::load(&context, "alpha").expect("reload");
        assert_eq!(
            state.cursor("taxonomy"),
            Some("20260722-013000-000001-abc123")
        );
        trash_test_root(&root);
    }

    #[test]
    fn advance_never_regresses() {
        let (root, context) = state_context("noregress");
        ChannelState::advance(
            &context,
            "alpha",
            "taxonomy",
            "20260722-013000-000002-bbb222",
        )
        .expect("advance");
        ChannelState::advance(
            &context,
            "alpha",
            "taxonomy",
            "20260722-013000-000001-aaa111",
        )
        .expect("stale advance is a no-op, not an error");
        let state = ChannelState::load(&context, "alpha").expect("reload");
        assert_eq!(
            state.cursor("taxonomy"),
            Some("20260722-013000-000002-bbb222")
        );
        trash_test_root(&root);
    }

    #[test]
    fn cursors_are_per_channel_and_per_room() {
        let (root, context) = state_context("perchan");
        std::fs::create_dir_all(root.join("beta")).expect("create second room dir");
        ChannelState::advance(
            &context,
            "alpha",
            "taxonomy",
            "20260722-013000-000001-abc123",
        )
        .expect("advance");
        ChannelState::advance(&context, "alpha", "build", "20260722-014000-000001-def456")
            .expect("advance");
        ChannelState::advance(
            &context,
            "beta",
            "taxonomy",
            "20260722-015000-000001-fed789",
        )
        .expect("advance");
        let alpha = ChannelState::load(&context, "alpha").expect("reload alpha");
        let beta = ChannelState::load(&context, "beta").expect("reload beta");
        assert_eq!(
            alpha.cursor("taxonomy"),
            Some("20260722-013000-000001-abc123")
        );
        assert_eq!(alpha.cursor("build"), Some("20260722-014000-000001-def456"));
        assert_eq!(
            beta.cursor("taxonomy"),
            Some("20260722-015000-000001-fed789")
        );
        trash_test_root(&root);
    }

    #[test]
    fn advance_reports_prior_and_new_cursor_and_replay_is_not_an_advance() {
        let (root, context) = state_context("outcome");
        let first = ChannelState::advance(&context, "alpha", "taxonomy", ID2).expect("advance");
        assert_eq!(first.prior, None);
        assert_eq!(first.cursor, ID2);
        assert!(first.advanced);

        let replay = ChannelState::advance(&context, "alpha", "taxonomy", ID2)
            .expect("replaying the same target is success, not an error");
        assert_eq!(replay.prior.as_deref(), Some(ID2));
        assert_eq!(replay.cursor, ID2);
        assert!(!replay.advanced);

        let behind = ChannelState::advance(&context, "alpha", "taxonomy", ID1).expect("stale");
        assert_eq!(
            behind.cursor, ID2,
            "a target behind the cursor never moves it"
        );
        assert!(!behind.advanced);
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
        // Release is proved by `concurrent_advances_on_two_channels_both_survive`,
        // whose eight workers take the lock BLOCKING in turn — an unreleased
        // lock hangs it. It is deliberately not asserted here: re-acquiring
        // through an fd that was already open when the holder closed is racy
        // on Darwin (observed EWOULDBLOCK under parallel test load), and that
        // shape never occurs in post, which opens the lock fresh per call.
        drop(lock);
        trash_test_root(&root);
    }

    #[test]
    fn concurrent_advances_on_two_channels_both_survive() {
        // The exact race the lock exists for: two writers hold whole-map
        // snapshots taken before the other's write. Unlocked, the loser's
        // channel silently reverts to its pre-advance cursor.
        let (root, context) = state_context("concurrent");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let mut handles = Vec::new();
        for worker in 0..8 {
            let context = context.clone();
            let barrier = std::sync::Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                let channel = format!("chan{worker}");
                let target = format!("20260722-013000-00000{worker}-aaa111");
                barrier.wait();
                ChannelState::advance(&context, "alpha", &channel, &target).expect("advance");
            }));
        }
        for handle in handles {
            handle.join().expect("worker thread");
        }
        let state = ChannelState::load(&context, "alpha").expect("reload");
        for worker in 0..8 {
            assert_eq!(
                state.cursor(&format!("chan{worker}")),
                Some(format!("20260722-013000-00000{worker}-aaa111").as_str()),
                "channel {worker}'s advance was lost"
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
}
