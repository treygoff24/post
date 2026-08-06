//! Watch presence heartbeats — read-only inference of live watches.
//!
//! Each `post watch` poll touches `<mail-root>/<room>/watch.heartbeat` with a
//! unix-seconds timestamp. `post who` reads those files: a watch is "live" when
//! the stamp is fresher than [`LIVE_SECS`]. No PIDs, no process info — presence
//! must never become a kill list.

use crate::error::{AppError, AppResult};
use crate::mailbox::Context;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// A watch is live when its heartbeat is newer than this many seconds.
pub(crate) const LIVE_SECS: u64 = 5;

pub(crate) fn heartbeat_path(context: &Context, room: &str) -> PathBuf {
    context.root.join(room).join("watch.heartbeat")
}

/// Best-effort: a failed touch must never kill the doorbell. Never mint a
/// room directory that does not already exist — watch must not recreate a
/// mailbox that was moved aside mid-session.
pub(crate) fn touch_heartbeat(context: &Context, room: &str) {
    let path = heartbeat_path(context, room);
    let Some(parent) = path.parent() else {
        return;
    };
    if !parent.is_dir() {
        return;
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let _ = std::fs::write(&path, format!("{now}\n"));
}

#[derive(Debug, Clone)]
pub(crate) struct Presence {
    pub room: String,
    pub live_watch: bool,
    pub last_seen: Option<String>,
}

pub(crate) fn read_presence(context: &Context, room: &str) -> AppResult<Presence> {
    let path = heartbeat_path(context, room);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Presence {
                room: room.to_owned(),
                live_watch: false,
                last_seen: None,
            });
        }
        Err(error) => {
            return Err(AppError::io("read watch heartbeat", &path, error));
        }
    };
    let stamp = raw.trim().parse::<u64>().ok();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let live_watch = stamp.is_some_and(|ts| now.saturating_sub(ts) <= LIVE_SECS);
    let last_seen = stamp.map(format_unix);
    Ok(Presence {
        room: room.to_owned(),
        live_watch,
        last_seen,
    })
}

fn format_unix(secs: u64) -> String {
    // Local civil stamp without chrono: enough for presence, not a calendar.
    // Format mirrors mail `sent` loosely as unix epoch seconds for machines;
    // humans get the number plus an age hint from live_watch.
    format!("{secs}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{test_root, trash_test_root};

    #[test]
    fn missing_heartbeat_is_not_live() {
        let root = test_root("presence-missing");
        let context = Context {
            root: root.clone(),
            home: root.clone(),
        };
        let presence = read_presence(&context, "alpha").expect("read");
        assert!(!presence.live_watch);
        assert!(presence.last_seen.is_none());
        trash_test_root(&root);
    }

    #[test]
    fn fresh_heartbeat_is_live() {
        let root = test_root("presence-live");
        let context = Context {
            root: root.clone(),
            home: root.clone(),
        };
        std::fs::create_dir_all(root.join("alpha")).expect("room dir");
        touch_heartbeat(&context, "alpha");
        let presence = read_presence(&context, "alpha").expect("read");
        assert!(presence.live_watch);
        assert!(presence.last_seen.is_some());
        trash_test_root(&root);
    }

    #[test]
    fn stale_heartbeat_is_not_live() {
        let root = test_root("presence-stale");
        let context = Context {
            root: root.clone(),
            home: root.clone(),
        };
        let path = heartbeat_path(&context, "alpha");
        std::fs::create_dir_all(path.parent().unwrap()).expect("dir");
        std::fs::write(&path, "1\n").expect("write stale");
        let presence = read_presence(&context, "alpha").expect("read");
        assert!(!presence.live_watch);
        assert_eq!(presence.last_seen.as_deref(), Some("1"));
        trash_test_root(&root);
    }
}
