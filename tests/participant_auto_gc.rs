//! The automatic participant cleanup: `post participant bind` runs what
//! `participant gc --apply` would, at most once a day per store, and never
//! changes what bind answers. Every test runs against a throwaway root.

mod common;

use common::{assert_success, from_stdout, Sandbox};
use serde_json::{json, Value};
use std::fs;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::process::Output;
use std::time::{SystemTime, UNIX_EPOCH};

const DAY: u64 = 24 * 60 * 60;

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_secs()
}

/// `YYYY-MM-DDTHH:MM:SSZ` for `seconds` since the epoch.
fn rfc3339(seconds: u64) -> String {
    let (days_since_epoch, of_day) = (seconds / DAY, seconds % DAY);
    let z = days_since_epoch as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        of_day / 3600,
        of_day % 3600 / 60,
        of_day % 60
    )
}

fn stamp_path(sandbox: &Sandbox) -> PathBuf {
    sandbox.mail_root.join(".auto-gc.stamp")
}

fn log_path(sandbox: &Sandbox) -> PathBuf {
    sandbox.mail_root.join(".auto-gc.log")
}

fn record_path(sandbox: &Sandbox, id: &str) -> PathBuf {
    sandbox
        .mail_root
        .join("participants")
        .join(id)
        .join("participant.json")
}

fn set_stamp_age(sandbox: &Sandbox, age_secs: u64) {
    fs::write(stamp_path(sandbox), format!("{}\n", now_secs() - age_secs)).expect("write stamp");
}

fn stamp_value(sandbox: &Sandbox) -> u64 {
    fs::read_to_string(stamp_path(sandbox))
        .expect("stamp exists")
        .trim()
        .parse()
        .expect("stamp is Unix seconds")
}

fn log_lines(sandbox: &Sandbox) -> Vec<Value> {
    fs::read_to_string(log_path(sandbox))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).expect("log line is JSON"))
        .collect()
}

fn bind_with(sandbox: &Sandbox, key: &str, envs: &[(&str, &str)]) -> Output {
    sandbox.run_in_env(
        &[
            "participant",
            "bind",
            "--harness",
            "claude",
            "--key",
            key,
            "--json",
        ],
        None,
        &sandbox.path,
        envs,
    )
}

/// A bind answer with every timestamp blanked, so two answers compare by
/// everything but the clock.
fn shape(output: &Output) -> (Option<i32>, Value, Vec<u8>) {
    fn blank(value: &mut Value) {
        match value {
            Value::String(text) if text.len() >= 20 && text.as_bytes()[4] == b'-' => {
                *text = "T".into();
            }
            Value::Array(items) => items.iter_mut().for_each(blank),
            Value::Object(map) => map.values_mut().for_each(blank),
            _ => {}
        }
    }
    let mut stdout: Value = from_stdout(output);
    blank(&mut stdout);
    (output.status.code(), stdout, output.stderr.clone())
}

/// Bind `key`, then age its record to `idle_days` since last seen. With no
/// state, such a record is tier 1 after a week.
fn bound_idle(sandbox: &Sandbox, key: &str, idle_days: u64) -> String {
    let output = bind_with(sandbox, key, &[("POST_AUTO_GC", "0")]);
    assert_success(&output);
    let value: Value = from_stdout(&output);
    let id = value["participant"]["id"].as_str().expect("id").to_owned();
    let path = record_path(sandbox, &id);
    let mut record: Value =
        serde_json::from_slice(&fs::read(&path).expect("record")).expect("json");
    record["last_seen"] = json!(rfc3339(now_secs() - idle_days * DAY));
    fs::write(&path, format!("{record}\n")).expect("write record");
    id
}

/// A fresh store: bind wrote a stamp and collected nothing, even with a
/// collectible record present.
#[test]
fn a_store_with_no_stamp_gets_one_and_collects_nothing() {
    let sandbox = Sandbox::new();
    let old = bound_idle(&sandbox, "auto-old", 10);
    assert!(
        !stamp_path(&sandbox).exists(),
        "fixture: POST_AUTO_GC=0 wrote nothing"
    );

    assert_success(&bind_with(&sandbox, "auto-live", &[]));
    let stamp = stamp_value(&sandbox);
    assert!(now_secs().abs_diff(stamp) < 60, "stamp is now: {stamp}");
    assert!(
        record_path(&sandbox, &old).exists(),
        "nothing collected on day zero"
    );
    assert!(log_lines(&sandbox).is_empty());
}

/// Old stamp plus a collectible record: bind collects it, logs one line,
/// advances the stamp, and answers exactly as it does with the switch off.
#[test]
fn an_old_stamp_collects_logs_and_leaves_bind_unchanged() {
    let sandbox = Sandbox::new();
    assert_success(&bind_with(&sandbox, "auto-live", &[("POST_AUTO_GC", "0")]));
    let baseline = bind_with(&sandbox, "auto-live", &[("POST_AUTO_GC", "0")]);
    assert_success(&baseline);

    let old = bound_idle(&sandbox, "auto-old", 10);
    let stale_at = now_secs() - DAY - 60;
    set_stamp_age(&sandbox, DAY + 60);
    let automatic = bind_with(&sandbox, "auto-live", &[]);
    assert_success(&automatic);

    assert!(!record_path(&sandbox, &old).exists(), "collected: {old}");
    assert_eq!(shape(&automatic), shape(&baseline));
    assert!(stamp_value(&sandbox) > stale_at + DAY / 2, "stamp advanced");

    let lines = log_lines(&sandbox);
    assert_eq!(lines.len(), 1, "{lines:?}");
    let line = &lines[0];
    assert_eq!(line["ok"], true);
    assert_eq!(line["deleted"], json!([old]));
    assert_eq!(line["archived"], json!([]));
    assert_eq!(line["error"], Value::Null);
    assert!(line["kept"].is_object());
    assert!(line["at"].as_str().is_some_and(|at| at.ends_with('Z')));
    assert!(line["post_build"].is_string());

    // The id in the log is enough to bring it back.
    let restored =
        sandbox.run_without_identity(&["participant", "restore", &old, "--json"], &sandbox.path);
    assert_success(&restored);
    assert!(record_path(&sandbox, &old).exists());
}

/// A stamp younger than a day: nothing is collected and the stamp stays.
#[test]
fn a_young_stamp_collects_nothing() {
    let sandbox = Sandbox::new();
    let old = bound_idle(&sandbox, "auto-old", 10);
    set_stamp_age(&sandbox, DAY - 600);
    let before = stamp_value(&sandbox);

    assert_success(&bind_with(&sandbox, "auto-live", &[]));
    assert!(record_path(&sandbox, &old).exists());
    assert_eq!(
        stamp_value(&sandbox),
        before,
        "a run that did not happen leaves the stamp"
    );
    assert!(log_lines(&sandbox).is_empty());
}

/// The switch off means no stamp, no lock file, no log, no collection.
#[test]
fn post_auto_gc_zero_touches_nothing() {
    let sandbox = Sandbox::new();
    let old = bound_idle(&sandbox, "auto-old", 10);
    // Even a due stamp: the switch wins over it.
    set_stamp_age(&sandbox, 3 * DAY);
    let before = stamp_value(&sandbox);
    let _ = fs::remove_file(sandbox.mail_root.join(".auto-gc.lock"));

    assert_success(&bind_with(&sandbox, "auto-live", &[("POST_AUTO_GC", "0")]));
    assert!(record_path(&sandbox, &old).exists());
    assert_eq!(stamp_value(&sandbox), before);
    assert!(!log_path(&sandbox).exists());
    assert!(!sandbox.mail_root.join(".auto-gc.lock").exists());

    // And in a fresh store, no stamp appears.
    let fresh = Sandbox::new();
    assert_success(&bind_with(&fresh, "auto-live", &[("POST_AUTO_GC", "0")]));
    assert!(!stamp_path(&fresh).exists());
}

/// Another process holds the lock: this bind skips, silently.
#[test]
fn a_held_lock_skips_the_run_without_touching_bind() {
    let sandbox = Sandbox::new();
    assert_success(&bind_with(&sandbox, "auto-live", &[("POST_AUTO_GC", "0")]));
    let baseline = bind_with(&sandbox, "auto-live", &[("POST_AUTO_GC", "0")]);
    let old = bound_idle(&sandbox, "auto-old", 10);
    set_stamp_age(&sandbox, 3 * DAY);
    let before = stamp_value(&sandbox);

    let holder = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(sandbox.mail_root.join(".auto-gc.lock"))
        .expect("open lock");
    assert_eq!(
        unsafe { libc::flock(holder.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let held = bind_with(&sandbox, "auto-live", &[]);
    assert_success(&held);
    assert_eq!(shape(&held), shape(&baseline));
    assert!(record_path(&sandbox, &old).exists(), "skipped while held");
    assert_eq!(stamp_value(&sandbox), before);
    assert!(log_lines(&sandbox).is_empty());

    // Released, the next bind runs.
    assert_eq!(unsafe { libc::flock(holder.as_raw_fd(), libc::LOCK_UN) }, 0);
    assert_success(&bind_with(&sandbox, "auto-live", &[]));
    assert!(!record_path(&sandbox, &old).exists());
}

/// gc fails partway: bind's answer and exit are unchanged and the log says so.
/// The failure is an archive root that is a file, so tier 2 cannot move a
/// record into it.
#[test]
fn a_gc_failure_leaves_bind_unchanged_and_logs_it() {
    let sandbox = Sandbox::new();
    assert_success(&bind_with(&sandbox, "auto-live", &[("POST_AUTO_GC", "0")]));
    let baseline = bind_with(&sandbox, "auto-live", &[("POST_AUTO_GC", "0")]);

    let stateful = bound_idle(&sandbox, "auto-stateful", 1);
    let joined = sandbox.run_as_participant(
        &["chat", "auto-room", "--join", "--json"],
        &stateful,
        &sandbox.path,
    );
    assert_success(&joined);
    let path = record_path(&sandbox, &stateful);
    let mut record: Value =
        serde_json::from_slice(&fs::read(&path).expect("record")).expect("json");
    record["last_seen"] = json!(rfc3339(now_secs() - 40 * DAY));
    fs::write(&path, format!("{record}\n")).expect("write record");
    fs::write(
        sandbox.mail_root.join("participants-archive"),
        "not a directory",
    )
    .expect("block archive root");

    set_stamp_age(&sandbox, 3 * DAY);
    let failed = bind_with(&sandbox, "auto-live", &[]);
    assert_success(&failed);
    assert_eq!(shape(&failed), shape(&baseline));
    assert!(failed.stderr.is_empty(), "nothing new on stderr");

    let lines = log_lines(&sandbox);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["ok"], false, "{lines:?}");
    assert!(lines[0]["error"]
        .as_str()
        .is_some_and(|error| !error.is_empty()));
    assert_eq!(lines[0]["deleted"], json!([]));
}

/// A log past 1 MiB moves to `.1` (replacing an older one) before the append.
#[test]
fn the_log_rotates_past_a_mebibyte() {
    let sandbox = Sandbox::new();
    assert_success(&bind_with(&sandbox, "auto-live", &[("POST_AUTO_GC", "0")]));
    let filler = format!(
        "{}\n",
        json!({"at": "2026-01-01T00:00:00Z", "ok": true, "pad": "x".repeat(1000)})
    );
    let big = filler.repeat(1100);
    assert!(big.len() > 1024 * 1024);
    fs::write(log_path(&sandbox), &big).expect("seed log");
    fs::write(
        sandbox.mail_root.join(".auto-gc.log.1"),
        "older generation\n",
    )
    .expect("seed .1");

    set_stamp_age(&sandbox, 3 * DAY);
    assert_success(&bind_with(&sandbox, "auto-live", &[]));
    assert_eq!(
        fs::read_to_string(sandbox.mail_root.join(".auto-gc.log.1")).expect(".1"),
        big,
        "the oversized log became .1, replacing the older one"
    );
    assert_eq!(
        log_lines(&sandbox).len(),
        1,
        "a fresh log with the new line only"
    );

    // Under the cap, the line is appended.
    set_stamp_age(&sandbox, 3 * DAY);
    assert_success(&bind_with(&sandbox, "auto-live", &[]));
    assert_eq!(log_lines(&sandbox).len(), 2);
}

fn stale_line(sandbox: &Sandbox) -> (Option<i32>, String) {
    let output = sandbox.run(&["doctor", "--json"]);
    let report: Value = from_stdout(&output);
    let message = report["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .find(|check| check["id"] == "participants.stale")
        .unwrap_or_else(|| panic!("{report}"))["message"]
        .as_str()
        .expect("message")
        .to_owned();
    (output.status.code(), message)
}

/// Doctor's stale-participants line says when the last automatic cleanup ran,
/// from the log's last line, and never changes the exit code.
#[test]
fn doctor_reports_the_last_automatic_cleanup() {
    let sandbox = Sandbox::new();
    // A record idle two days is stale (24 h lease) but not collectible, so
    // the Info line exists.
    let _ = bound_idle(&sandbox, "auto-two-days", 2);
    let (code_without, without) = stale_line(&sandbox);
    assert!(without.contains("ran: never"), "{without}");

    let older = json!({"at": "2026-01-01T00:00:00Z", "ok": true, "deleted": ["a"], "archived": [], "kept": {}, "error": null, "post_build": "x"});
    let last = json!({"at": "2026-09-01T12:34:56Z", "ok": true, "deleted": ["claude-1", "claude-2"], "archived": ["claude-3"], "kept": {}, "error": null, "post_build": "x"});
    fs::write(log_path(&sandbox), format!("{older}\n{last}\n")).expect("seed log");
    let (code_with, with) = stale_line(&sandbox);
    assert!(
        with.contains("ran 2026-09-01T12:34:56Z and deleted 2 record(s) and archived 1"),
        "{with}"
    );
    assert_eq!(
        code_with, code_without,
        "Info only: the exit code does not move"
    );

    let failed = json!({"at": "2026-09-02T00:00:00Z", "ok": false, "deleted": [], "archived": [], "kept": {}, "error": "boom", "post_build": "x"});
    fs::write(log_path(&sandbox), format!("{last}\n{failed}\n")).expect("seed log");
    let (_, failing) = stale_line(&sandbox);
    assert!(
        failing.contains("ran 2026-09-02T00:00:00Z and failed: boom"),
        "{failing}"
    );
}

/// The schema names the switch.
#[test]
fn the_schema_documents_the_switch() {
    let sandbox = Sandbox::new();
    let output = sandbox.run(&["schema"]);
    assert_success(&output);
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(text.contains("POST_AUTO_GC"), "schema environment list");
}

/// The stamp, lock, and log at the store root are not rooms, participants, or
/// doctor findings.
#[test]
fn the_bookkeeping_files_are_invisible_to_rooms_and_doctor() {
    let sandbox = Sandbox::new();
    let output = sandbox.run(&["doctor", "--json"]);
    let before: Value = from_stdout(&output);
    set_stamp_age(&sandbox, 3 * DAY);
    assert_success(&bind_with(&sandbox, "auto-live", &[]));
    assert!(stamp_path(&sandbox).exists());
    assert!(sandbox.mail_root.join(".auto-gc.lock").exists());
    assert!(log_path(&sandbox).exists());
    let after: Value = from_stdout(&sandbox.run(&["doctor", "--json"]));
    let ids = |report: &Value| -> Vec<String> {
        report["checks"]
            .as_array()
            .expect("checks")
            .iter()
            .map(|check| check["id"].as_str().expect("id").to_owned())
            .collect()
    };
    let new: Vec<String> = ids(&after)
        .into_iter()
        .filter(|id| !ids(&before).contains(id))
        .collect();
    assert!(
        new.iter().all(|id| id.starts_with("participant")),
        "{new:?}"
    );
    assert!(!new.iter().any(|id| id.contains("auto-gc")), "{new:?}");
}
