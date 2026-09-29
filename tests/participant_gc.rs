//! `post participant gc`: what it collects, what it must never touch, and what
//! a collected session gets back. Every test runs against a throwaway root.

mod common;

use common::{assert_success, from_stdout, register_alpha_beta, Sandbox};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const DAY: u64 = 24 * 60 * 60;

/// `YYYY-MM-DDTHH:MM:SSZ` for `days` before now (civil-from-days, so the test
/// needs no date crate).
fn days_ago(days: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    let seconds = now - days * DAY;
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

fn digest(key: &str) -> String {
    format!("{:x}", Sha256::digest(key.as_bytes()))
}

fn record_path(sandbox: &Sandbox, id: &str) -> PathBuf {
    sandbox
        .mail_root
        .join("participants")
        .join(id)
        .join("participant.json")
}

fn patch(sandbox: &Sandbox, id: &str, change: impl FnOnce(&mut Value)) {
    let path = record_path(sandbox, id);
    let mut record: Value =
        serde_json::from_slice(&fs::read(&path).expect("record")).expect("record JSON");
    change(&mut record);
    fs::write(
        &path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&record).expect("bytes")
        ),
    )
    .expect("write record");
}

/// Bind a session for `key` and age it: last seen `idle_days` ago.
fn bound_idle(sandbox: &Sandbox, harness: &str, key: &str, idle_days: u64) -> String {
    let output = sandbox.run_in_env(
        &[
            "participant",
            "bind",
            "--harness",
            harness,
            "--key",
            key,
            "--json",
        ],
        None,
        &sandbox.path,
        &[],
    );
    assert_success(&output);
    let value: Value = from_stdout(&output);
    let id = value["participant"]["id"].as_str().expect("id").to_owned();
    let stamp = days_ago(idle_days);
    patch(sandbox, &id, |record| {
        record["last_seen"] = json!(stamp);
    });
    id
}

/// Give a participant real state: a channel membership, which writes its
/// channel memberships.
fn give_state(sandbox: &Sandbox, id: &str, channel: &str) {
    let output =
        sandbox.run_as_participant(&["chat", channel, "--join", "--json"], id, &sandbox.path);
    assert_success(&output);
    // Joining refreshes last_seen; the caller ages it again.
}

fn gc(sandbox: &Sandbox, apply: bool) -> Value {
    let mut args = vec!["participant", "gc", "--json"];
    if apply {
        args.push("--apply");
    }
    let output = sandbox.run_without_identity(&args, &sandbox.path);
    assert_success(&output);
    from_stdout(&output)
}

fn strings(value: &Value) -> Vec<String> {
    let mut items: Vec<String> = value
        .as_array()
        .expect("array")
        .iter()
        .map(|item| item.as_str().expect("string").to_owned())
        .collect();
    items.sort();
    items
}

fn sorted(ids: &[&String]) -> Vec<String> {
    let mut items: Vec<String> = ids.iter().map(|id| (*id).clone()).collect();
    items.sort();
    items
}

fn tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, at: &Path, found: &mut BTreeMap<PathBuf, Vec<u8>>) {
        let Ok(entries) = fs::read_dir(at) else {
            return;
        };
        for entry in entries {
            let entry = entry.expect("tree entry");
            let path = entry.path();
            let relative = path.strip_prefix(root).expect("relative").to_path_buf();
            if entry.file_type().expect("entry type").is_dir() {
                found.insert(relative, b"<dir>".to_vec());
                walk(root, &path, found);
            } else {
                found.insert(relative, fs::read(&path).expect("tree file"));
            }
        }
    }
    let mut found = BTreeMap::new();
    walk(root, root, &mut found);
    found
}

fn tombstones(sandbox: &Sandbox) -> Vec<Value> {
    fs::read_to_string(sandbox.mail_root.join("participants/archived.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).expect("tombstone JSON"))
        .collect()
}

fn show_key(sandbox: &Sandbox, harness: &str, key: &str) -> Value {
    let output = sandbox.run_without_identity(
        &[
            "participant",
            "show",
            "--harness",
            harness,
            "--key",
            key,
            "--json",
        ],
        &sandbox.path,
    );
    assert_success(&output);
    from_stdout(&output)
}

/// One run collects each tier, keeps what it must, and a dry run said the same
/// thing without touching the store (invariant 6: dry run equals apply, and a
/// second apply finds nothing).
#[test]
fn gc_dry_run_matches_apply_and_a_second_apply_finds_nothing() {
    let sandbox = Sandbox::new();
    let empty = bound_idle(&sandbox, "claude", "empty-and-old", 40);
    let with_state = bound_idle(&sandbox, "claude", "stateful-and-old", 40);
    give_state(&sandbox, &with_state, "gc-state");
    patch(&sandbox, &with_state, |record| {
        record["last_seen"] = json!(days_ago(40));
    });
    let recent = bound_idle(&sandbox, "claude", "empty-but-recent", 1);
    let lineage = bound_idle(&sandbox, "claude", "empty-but-in-a-lineage", 40);
    patch(&sandbox, &lineage, |record| {
        record["lineage"] = json!("ember");
        record["lineage_since"] = json!("2026-01-01T00:00:00Z");
    });

    let before = tree(&sandbox.mail_root);
    let dry = gc(&sandbox, false);
    assert_eq!(
        tree(&sandbox.mail_root),
        before,
        "a dry run changes nothing"
    );
    assert_eq!(dry["ok"], true);
    assert_eq!(dry["applied"], false);
    assert_eq!(strings(&dry["deleted"]), sorted(&[&empty]));
    assert_eq!(strings(&dry["archived"]), sorted(&[&with_state]));
    assert_eq!(dry["kept"]["recent"], 1, "{dry}");
    assert_eq!(dry["kept"]["lineage"], 1, "{dry}");

    let applied = gc(&sandbox, true);
    assert_eq!(applied["applied"], true);
    assert_eq!(
        applied["deleted"], dry["deleted"],
        "apply does what the dry run said"
    );
    assert_eq!(applied["archived"], dry["archived"]);
    assert_eq!(applied["kept"], dry["kept"]);

    assert!(!record_path(&sandbox, &empty).exists(), "tier 1 deletes");
    assert!(!record_path(&sandbox, &with_state).exists());
    assert!(
        sandbox
            .mail_root
            .join("participants-archive")
            .join(&with_state)
            .join("participant.json")
            .is_file(),
        "tier 2 moves the whole record aside"
    );
    assert!(record_path(&sandbox, &recent).exists());
    assert!(record_path(&sandbox, &lineage).exists());
    let marks = tombstones(&sandbox);
    assert_eq!(marks.len(), 1);
    assert_eq!(marks[0]["id"], empty.as_str());
    assert_eq!(marks[0]["conversation_key_digest"], digest("empty-and-old"));

    let again = gc(&sandbox, true);
    assert_eq!(again["deleted"], json!([]));
    assert_eq!(again["archived"], json!([]));
}

/// Invariant 1: mail is never lost. A stale participant with an unread letter,
/// a letter still pending in its workspace, or a letter in its own inbox is
/// kept, whatever its age.
#[test]
fn gc_never_collects_a_participant_that_has_mail() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let sender = sandbox.bind_claude("gc-sender", &beta, Some("beta"))["id"]
        .as_str()
        .expect("sender id")
        .to_owned();

    // Unread routed mail: the recipient has state and an unread letter.
    let unread = sandbox.bind_claude("gc-unread", &alpha, Some("alpha"))["id"]
        .as_str()
        .expect("id")
        .to_owned();
    let sent = sandbox.run_as_participant(
        &[
            "send",
            "--to",
            &format!("participant:{unread}"),
            "--body",
            "waiting for you",
            "--json",
        ],
        &sender,
        &beta,
    );
    assert_success(&sent);
    patch(&sandbox, &unread, |record| {
        record["last_seen"] = json!(days_ago(60));
    });
    patch(&sandbox, &sender, |record| {
        record["last_seen"] = json!(days_ago(1));
    });

    let planned = gc(&sandbox, false);
    assert!(
        !strings(&planned["deleted"]).contains(&unread)
            && !strings(&planned["archived"]).contains(&unread),
        "a participant with mail was planned for collection: {planned}"
    );
    let applied = gc(&sandbox, true);
    assert!(
        !strings(&applied["deleted"]).contains(&unread)
            && !strings(&applied["archived"]).contains(&unread),
        "{applied}"
    );
    assert!(record_path(&sandbox, &unread).exists());
    let read = sandbox.run_as_participant(&["inbox"], &unread, &alpha);
    assert_success(&read);
    let inbox: Value = from_stdout(&read);
    assert_eq!(inbox["count"], 1, "the letter is still there: {inbox}");
}

/// Invariants 2 and 3: a collected session resumes with the same id; a
/// tombstone stops another key taking the id in the meantime; an archived
/// record comes back whole, cursors and all.
#[test]
fn gc_keeps_ids_stable_and_restores_archived_state() {
    // Two keys whose digests share their first eight hex characters.
    const FIRST: &str = "collision-key-25835";
    const SECOND: &str = "collision-key-54347";
    assert_eq!(&digest(FIRST)[..8], &digest(SECOND)[..8]);

    let sandbox = Sandbox::new();
    let first = bound_idle(&sandbox, "native", FIRST, 40);
    assert_eq!(first.len(), "native-".len() + 8);
    let applied = gc(&sandbox, true);
    assert_eq!(applied["deleted"], json!([first]));

    // The freed id stays taken for its key: the other key gets the longer id.
    let other = sandbox.run_without_identity(
        &[
            "participant",
            "bind",
            "--harness",
            "native",
            "--key",
            SECOND,
            "--json",
        ],
        &sandbox.path,
    );
    assert_success(&other);
    let other: Value = from_stdout(&other);
    let other_id = other["participant"]["id"].as_str().expect("id");
    assert_ne!(other_id, first);
    assert_eq!(other_id.len(), "native-".len() + 12, "{other_id}");

    // The first key comes back under its original id.
    let back = sandbox.run_without_identity(
        &[
            "participant",
            "bind",
            "--harness",
            "native",
            "--key",
            FIRST,
            "--json",
        ],
        &sandbox.path,
    );
    assert_success(&back);
    let back: Value = from_stdout(&back);
    assert_eq!(back["participant"]["id"], first.as_str());

    // Tier 2: state comes back whole.
    let stateful = bound_idle(&sandbox, "claude", "resumes-later", 1);
    give_state(&sandbox, &stateful, "gc-resume");
    let state_dir = sandbox.mail_root.join("participants").join(&stateful);
    let memberships = fs::read(state_dir.join("channels.json")).expect("memberships exist");
    patch(&sandbox, &stateful, |record| {
        record["last_seen"] = json!(days_ago(60));
    });
    let archived = gc(&sandbox, true);
    assert_eq!(archived["archived"], json!([stateful]));
    assert!(!state_dir.exists());

    // Invariant 8: the non-minting lookup says so, and creates nothing.
    let before = tree(&sandbox.mail_root);
    let shown = show_key(&sandbox, "claude", "resumes-later");
    assert_eq!(shown["status"], "archived", "{shown}");
    assert_eq!(tree(&sandbox.mail_root), before, "a lookup mints nothing");

    let resumed = sandbox.run_without_identity(
        &[
            "participant",
            "bind",
            "--harness",
            "claude",
            "--key",
            "resumes-later",
            "--json",
        ],
        &sandbox.path,
    );
    assert_success(&resumed);
    let resumed: Value = from_stdout(&resumed);
    assert_eq!(resumed["participant"]["id"], stateful.as_str());
    assert_eq!(
        fs::read(state_dir.join("channels.json")).expect("memberships restored"),
        memberships,
        "the archived state came back byte for byte"
    );
    assert!(!sandbox
        .mail_root
        .join("participants-archive")
        .join(&stateful)
        .exists());
}

/// Invariant 4: liveness is never collected. An active lease, a watch that beat
/// recently, a lineage's holder, and a doorbell subscription each keep a
/// record that would otherwise be old and empty.
#[test]
fn gc_keeps_anything_still_alive() {
    let sandbox = Sandbox::new();
    let leased = bound_idle(&sandbox, "claude", "long-lease", 40);
    patch(&sandbox, &leased, |record| {
        record["lease_hours"] = json!(24 * 90);
    });
    let watching = bound_idle(&sandbox, "claude", "still-watching", 40);
    fs::write(
        sandbox
            .mail_root
            .join("participants")
            .join(&watching)
            .join("watch.heartbeat"),
        format!(
            "{} 1000\n",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_secs()
        ),
    )
    .expect("heartbeat");
    let subscribed = bound_idle(&sandbox, "claude", "subscribed", 40);
    let prefs = sandbox.mail_root.join("doorbell/prefs");
    fs::create_dir_all(&prefs).expect("prefs dir");
    fs::write(prefs.join(format!("{subscribed}.json")), b"{}\n").expect("prefs");
    let founder = bound_idle(&sandbox, "claude", "lineage-holder", 40);
    patch(&sandbox, &founder, |record| {
        record["lineage"] = json!("ember");
        record["lineage_since"] = json!("2026-01-01T00:00:00Z");
    });
    let idle = bound_idle(&sandbox, "claude", "truly-idle", 40);

    let applied = gc(&sandbox, true);
    assert_eq!(applied["deleted"], json!([idle]), "{applied}");
    assert_eq!(applied["archived"], json!([]));
    for id in [&leased, &watching, &subscribed, &founder] {
        assert!(record_path(&sandbox, id).exists(), "{id} was collected");
    }
    // The fixture's own seeded participant is active too.
    assert!(applied["kept"]["active"].as_u64() >= Some(1), "{applied}");
    assert_eq!(applied["kept"]["live_watch"], 1, "{applied}");
    assert_eq!(applied["kept"]["subscribed"], 1, "{applied}");
    assert_eq!(applied["kept"]["lineage"], 1, "{applied}");
}

/// A `participant bind --new` record is an errand's identity: it leases for an
/// hour and is collectable after a day, not a week.
#[test]
fn ephemeral_records_are_collected_after_a_day() {
    let sandbox = Sandbox::new();
    let output = sandbox.run_without_identity(
        &[
            "participant",
            "bind",
            "--new",
            "--harness",
            "claude",
            "--json",
        ],
        &sandbox.path,
    );
    assert_success(&output);
    let value: Value = from_stdout(&output);
    let id = value["participant"]["id"].as_str().expect("id").to_owned();
    assert_eq!(value["participant"]["lease_hours"], 1);
    assert_eq!(value["participant"]["ephemeral"], true);
    let ordinary = bound_idle(&sandbox, "claude", "ordinary-two-days", 2);
    patch(&sandbox, &id, |record| {
        record["last_seen"] = json!(days_ago(2));
    });

    let applied = gc(&sandbox, true);
    assert_eq!(applied["deleted"], json!([id]), "{applied}");
    assert!(record_path(&sandbox, &ordinary).exists());
    assert_eq!(applied["kept"]["recent"], 1);
}
