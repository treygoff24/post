//! Reader-owned channel read cursors.
//!
//! Lives at `<mail-root>/<room>/channel-state.json` (via
//! `channel::channel_state_path`), shape `{"<channel>": "<last-read-id>"}`.
//! The channel tree stays append-only-by-senders; a reader's cursor dying or
//! resetting can never corrupt shared history — it only re-shows backlog.

use crate::channel::{channel_state_path, ChannelStateMap};
use crate::error::{AppError, AppResult, ErrorCode};
use crate::mailbox::{atomic_replace, Context};

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
    pub(crate) fn advance(
        context: &Context,
        room: &str,
        channel: &str,
        last_read_id: &str,
    ) -> AppResult<()> {
        let mut state = Self::load(context, room)?;
        if state
            .cursors
            .get(channel)
            .is_some_and(|current| current.as_str() >= last_read_id)
        {
            return Ok(());
        }
        state
            .cursors
            .insert(channel.to_owned(), last_read_id.to_owned());
        let path = channel_state_path(context, room)?;
        // A room that has never received mail has no <root>/<room>/ yet, and
        // atomic_replace cannot create parents — without this, such a room
        // can read but never advance, re-showing the backlog forever (found
        // by live smoke; the lane's unit tests pre-created the room dir).
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| AppError::io("create channel state directory", parent, error))?;
        }
        let mut bytes = serde_json::to_vec_pretty(&state.cursors).map_err(|error| {
            AppError::new(
                ErrorCode::IoError,
                format!("failed to serialize channel state: {error}"),
                "Retry the read; the cursor was not advanced.",
            )
        })?;
        bytes.push(b'\n');
        atomic_replace(&path, &bytes)
            .map_err(|error| AppError::io("atomically update channel state", &path, error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{test_root, trash_test_root};

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
    fn corrupt_state_is_a_config_error_not_a_panic() {
        let (root, context) = state_context("corrupt");
        std::fs::write(root.join("alpha").join("channel-state.json"), b"{not json")
            .expect("write corrupt");
        let error = ChannelState::load(&context, "alpha").expect_err("corrupt state must error");
        assert_eq!(error.code.as_str(), "config_invalid");
        trash_test_root(&root);
    }
}
