//! Compatibility read seam for the pre-Plan-B channel state.
//!
//! New state lives in cursor_state. This module keeps the names used by the
//! existing channel, watch, doctor, and consuming-read callers while the
//! consuming callers migrate to Delta directly.

use crate::cursor_state;
use crate::error::AppResult;
use crate::mailbox::Context;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Default)]
pub(crate) struct ChannelState {
    channels: BTreeMap<String, BTreeSet<String>>,
}

impl ChannelState {
    pub(crate) fn load(context: &Context, room: &str) -> AppResult<Self> {
        Ok(Self {
            channels: cursor_state::Snapshot::load(context, room).into_channels(),
        })
    }

    pub(crate) fn has_seen(&self, channel: &str, id: &str) -> bool {
        self.channels
            .get(channel)
            .is_some_and(|seen| seen.contains(id))
    }

    pub(crate) fn into_channels(self) -> BTreeMap<String, BTreeSet<String>> {
        self.channels
    }
}

pub(crate) fn stored_shape_is_valid(bytes: &[u8]) -> bool {
    cursor_state::legacy_stored_shape_is_valid(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{test_root, trash_test_root};
    use std::fs;

    const ID1: &str = "20260831-171234-000001-a1b2c3";
    const ID2: &str = "20260831-171234-000002-b2c3d4";

    #[test]
    fn load_prefers_materialized_cursor_over_legacy_channel_state() {
        let root = test_root("channelstate-delegation");
        let context = Context {
            root: root.clone(),
            home: root.clone(),
        };
        fs::create_dir_all(root.join("alpha")).expect("room");
        fs::write(
            root.join("alpha/channel-state.json"),
            format!(r#"{{"version":2,"channels":{{"tax":{{"seen":["{ID1}"]}}}}}}"#),
        )
        .expect("legacy");
        fs::write(
            root.join("alpha/cursors.json"),
            format!(
                r#"{{
  "version": 1,
  "mail": {{"seen": []}},
  "channels": {{"tax": {{"seen": ["{ID2}"]}}}}
}}
"#
            ),
        )
        .expect("cursor");

        let state = ChannelState::load(&context, "alpha").expect("load");
        assert!(!state.has_seen("tax", ID1));
        assert!(state.has_seen("tax", ID2));
        trash_test_root(&root);
    }
}
