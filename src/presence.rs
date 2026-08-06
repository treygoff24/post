//! Watch presence heartbeats — read-only inference of live watches.
//!
//! Each long-running `post watch` poll touches `<mail-root>/<room>/watch.heartbeat`
//! with a unix-seconds stamp plus the poll interval. `post who` reads those
//! files: a watch is "live" when the stamp is not in the future and younger
//! than `interval*2 + slack`. No PIDs, no process info — presence must never
//! become a kill list. Snapshot polls never touch heartbeats.

use crate::error::{AppError, AppResult};
use crate::mailbox::Context;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Default interval assumed for legacy heartbeats that only stored a stamp.
const DEFAULT_INTERVAL_MS: u64 = 1000;
/// Extra grace on top of `interval*2` so scheduling jitter does not flap.
const SLACK_MS: u64 = 2000;

pub(crate) fn heartbeat_path(context: &Context, room: &str) -> PathBuf {
    context.root.join(room).join("watch.heartbeat")
}

/// Best-effort: a failed touch must never kill the doorbell. Never mint a
/// room directory that does not already exist — watch must not recreate a
/// mailbox that was moved aside mid-session. Never follow symlinks.
pub(crate) fn touch_heartbeat(context: &Context, room: &str, interval_ms: u64) {
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
    let payload = format!("{now} {interval_ms}\n");
    let _ = write_heartbeat_nofollow(&path, payload.as_bytes());
}

/// Open/create `path` without following symlinks, require a solitary regular
/// file, write as mode 0600. Existing paths are opened without truncate so a
/// hard-linked victim cannot be clobbered before the nlink check; O_NONBLOCK
/// keeps FIFOs from hanging the watch poll. Returns Err on symlink (ELOOP),
/// hard link, non-regular file, or other failure.
fn write_heartbeat_nofollow(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    match OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(mut file) => {
            // fstat the open fd before any mutation — never trust path metadata.
            let metadata = file.metadata()?;
            if !metadata.file_type().is_file() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "heartbeat path is not a regular file",
                ));
            }
            if metadata.nlink() != 1 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "heartbeat path has unexpected link count",
                ));
            }
            file.set_len(0)?;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            file.write_all(bytes)?;
            return file.sync_all();
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
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
    let (stamp, interval_ms) = parse_heartbeat(raw.trim());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let live_watch = stamp.is_some_and(|ts| is_live(now, ts, interval_ms));
    let last_seen = stamp.map(format_unix);
    Ok(Presence {
        room: room.to_owned(),
        live_watch,
        last_seen,
    })
}

fn parse_heartbeat(raw: &str) -> (Option<u64>, u64) {
    let mut parts = raw.split_whitespace();
    let stamp = parts.next().and_then(|value| value.parse::<u64>().ok());
    let interval_ms = parts
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_INTERVAL_MS);
    (stamp, interval_ms)
}

fn is_live(now_secs: u64, stamp_secs: u64, interval_ms: u64) -> bool {
    if stamp_secs > now_secs {
        return false;
    }
    let age_ms = now_secs.saturating_sub(stamp_secs).saturating_mul(1000);
    let max_age_ms = interval_ms.saturating_mul(2).saturating_add(SLACK_MS);
    age_ms <= max_age_ms
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
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

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
        touch_heartbeat(&context, "alpha", 1000);
        let presence = read_presence(&context, "alpha").expect("read");
        assert!(presence.live_watch);
        assert!(presence.last_seen.is_some());
        let meta = std::fs::metadata(heartbeat_path(&context, "alpha")).expect("meta");
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
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
        std::fs::write(&path, "1 1000\n").expect("write stale");
        let presence = read_presence(&context, "alpha").expect("read");
        assert!(!presence.live_watch);
        assert_eq!(presence.last_seen.as_deref(), Some("1"));
        trash_test_root(&root);
    }

    #[test]
    fn ten_second_interval_stays_live_between_polls() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        // Age 9s with a 10s poll interval: within interval*2 + slack.
        assert!(is_live(now, now.saturating_sub(9), 10_000));
        // Age well past the window.
        assert!(!is_live(now, now.saturating_sub(30), 10_000));
    }

    #[test]
    fn future_stamp_is_never_live() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(!is_live(now, now.saturating_add(3600), 1000));
    }

    #[test]
    fn heartbeat_write_does_not_follow_symlink() {
        let root = test_root("presence-symlink");
        let context = Context {
            root: root.clone(),
            home: root.clone(),
        };
        let room = root.join("alpha");
        std::fs::create_dir_all(&room).expect("room");
        let victim = root.join("victim.txt");
        std::fs::write(&victim, b"SAFE\n").expect("victim");
        let hb = room.join("watch.heartbeat");
        std::os::unix::fs::symlink(&victim, &hb).expect("plant symlink");
        touch_heartbeat(&context, "alpha", 1000);
        let contents = std::fs::read_to_string(&victim).expect("read victim");
        assert_eq!(contents, "SAFE\n", "must not write through symlink");
        // Symlink target unchanged; heartbeat path is still the symlink.
        assert!(std::fs::symlink_metadata(&hb)
            .expect("meta")
            .file_type()
            .is_symlink());
        trash_test_root(&root);
    }

    #[test]
    fn existing_heartbeat_perms_normalized_to_0600() {
        let root = test_root("presence-perms");
        let context = Context {
            root: root.clone(),
            home: root.clone(),
        };
        let path = heartbeat_path(&context, "alpha");
        std::fs::create_dir_all(path.parent().unwrap()).expect("dir");
        std::fs::write(&path, "1 1000\n").expect("seed");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        touch_heartbeat(&context, "alpha", 1000);
        let mode = std::fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        trash_test_root(&root);
    }

    #[test]
    fn heartbeat_write_does_not_clobber_through_hard_link() {
        let root = test_root("presence-hardlink");
        let context = Context {
            root: root.clone(),
            home: root.clone(),
        };
        let room = root.join("alpha");
        std::fs::create_dir_all(&room).expect("room");
        let victim = root.join("victim.txt");
        std::fs::write(&victim, b"SAFE\n").expect("victim");
        let hb = room.join("watch.heartbeat");
        std::fs::hard_link(&victim, &hb).expect("plant hard link");
        assert_eq!(
            std::fs::metadata(&hb).expect("meta").nlink(),
            2,
            "fixture must be a hard link"
        );
        touch_heartbeat(&context, "alpha", 1000);
        let contents = std::fs::read_to_string(&victim).expect("read victim");
        assert_eq!(contents, "SAFE\n", "must not truncate/write through hard link");
        let mode = std::fs::metadata(&victim).expect("victim meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o644, "must not chmod the hard-linked victim");
        trash_test_root(&root);
    }

    #[test]
    fn heartbeat_write_does_not_hang_or_write_fifo() {
        let root = test_root("presence-fifo");
        let context = Context {
            root: root.clone(),
            home: root.clone(),
        };
        let room = root.join("alpha");
        std::fs::create_dir_all(&room).expect("room");
        let hb = room.join("watch.heartbeat");
        let c_path = std::ffi::CString::new(hb.as_os_str().as_encoded_bytes()).expect("c path");
        let rc = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
        assert_eq!(rc, 0, "mkfifo: {}", std::io::Error::last_os_error());
        // Must return promptly (O_NONBLOCK) and leave the FIFO untouched.
        let started = std::time::Instant::now();
        touch_heartbeat(&context, "alpha", 1000);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "FIFO open must not block the watch poll"
        );
        let meta = std::fs::metadata(&hb).expect("fifo meta");
        assert!(
            !meta.file_type().is_file(),
            "FIFO must remain a non-regular node"
        );
        trash_test_root(&root);
    }
}
