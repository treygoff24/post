//! The automatic participant cleanup: `post participant bind` runs the same
//! pass as `post participant gc --apply` at most once a day per store, so
//! records do not pile up without anyone scheduling anything.
//!
//! Files, all beside `.participants.lock` at the store root (dot-named files
//! at the root are not rooms, participants, or doctor findings):
//!
//! - `.auto-gc.stamp`: Unix seconds of the last automatic run, written before
//!   the run so a crash cannot cause a retry storm. A store with no stamp gets
//!   one and no run: the first cleanup comes a day later.
//! - `.auto-gc.lock`: taken non-blocking and exclusive before the stamp is
//!   read; a holder means another process is on it, so this one skips.
//! - `.auto-gc.log`: one JSON line per run; past 1 MiB it moves to
//!   `.auto-gc.log.1` (replacing an older one). Every id it names comes back
//!   with `post participant restore <id>`.
//!
//! Best effort by construction: nothing here returns an error, prints, or
//! changes what bind answers. `POST_AUTO_GC=0` turns the whole thing off.

use super::participant_gc::{self, Report};
use crate::error::AppResult;
use crate::mailbox::Context;
use crate::participant::format_rfc3339;
use serde_json::json;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_LOG_BYTES: u64 = 1024 * 1024;
const STAMP_FILE: &str = ".auto-gc.stamp";
const LOCK_FILE: &str = ".auto-gc.lock";
const LOG_FILE: &str = ".auto-gc.log";

fn enabled() -> bool {
    std::env::var_os("POST_AUTO_GC").is_none_or(|value| value != "0")
}

fn secs(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Run the cleanup if this store is due. Never fails and never prints.
pub(super) fn maybe_run(context: &Context) {
    if enabled() {
        let _ = try_run(context);
    }
}

fn try_run(context: &Context) -> Option<()> {
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(context.root.join(LOCK_FILE))
        .ok()?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == -1 {
        return None;
    }
    let stamp = context.root.join(STAMP_FILE);
    let now = SystemTime::now();
    let last = fs::read_to_string(&stamp)
        .ok()
        .and_then(|text| text.trim().parse::<u64>().ok());
    let due = last.is_some_and(|at| {
        let now = secs(now);
        // A stamp from the future (the clock moved back) would otherwise
        // block the cleanup until the clock caught up.
        now >= at.saturating_add(INTERVAL.as_secs()) || at > now + INTERVAL.as_secs()
    });
    if last.is_some() && !due {
        return None;
    }
    // Before the run, so a crash mid-run does not retry on the next bind. A
    // store with no stamp gets one here and no run.
    fs::write(&stamp, format!("{}\n", secs(now))).ok()?;
    if !due {
        return None;
    }
    let outcome = participant_gc::execute(context, true);
    append_log(context, now, &outcome)
}

fn append_log(context: &Context, now: SystemTime, outcome: &AppResult<Report>) -> Option<()> {
    let line = match outcome {
        Ok(report) => json!({
            "at": format_rfc3339(now).ok()?,
            "ok": true,
            "deleted": report.deleted,
            "archived": report.archived,
            "kept": report.kept,
            "error": null,
            "post_build": option_env!("POST_BUILD_SHA").unwrap_or("unknown"),
        }),
        Err(error) => json!({
            "at": format_rfc3339(now).ok()?,
            "ok": false,
            "deleted": [],
            "archived": [],
            "kept": {},
            "error": error.message,
            "post_build": option_env!("POST_BUILD_SHA").unwrap_or("unknown"),
        }),
    };
    let path = context.root.join(LOG_FILE);
    if fs::metadata(&path).is_ok_and(|meta| meta.len() > MAX_LOG_BYTES) {
        let mut older = path.clone().into_os_string();
        older.push(".1");
        fs::rename(&path, PathBuf::from(older)).ok()?;
    }
    let mut file: File = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&path)
        .ok()?;
    file.write_all(format!("{line}\n").as_bytes()).ok()
}

/// What `post doctor` says about the last automatic run, from the log's last
/// line: when it ran and what it removed, or "never".
pub(super) fn last_run_summary(context: &Context) -> String {
    let last = fs::read_to_string(context.root.join(LOG_FILE))
        .ok()
        .and_then(|text| {
            text.lines()
                .rev()
                .find_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        });
    let Some(entry) = last else {
        return "the last automatic participant cleanup ran: never".to_owned();
    };
    let count = |key: &str| entry[key].as_array().map_or(0, Vec::len);
    let at = entry["at"].as_str().unwrap_or("unknown time");
    if entry["ok"] == true {
        format!(
            "the last automatic participant cleanup ran {at} and deleted {} record(s) and archived {} ({LOG_FILE} at the store root names them; `post participant restore <id>` brings one back)",
            count("deleted"),
            count("archived"),
        )
    } else {
        format!(
            "the last automatic participant cleanup ran {at} and failed: {}",
            entry["error"].as_str().unwrap_or("unknown error")
        )
    }
}
