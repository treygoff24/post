//! `post participant gc`: what it collects, what it must never touch, and what
//! a collected session gets back. Every test runs against a throwaway root.

mod common;

use common::{assert_success, from_stderr, from_stdout, register_alpha_beta, Sandbox};
use post::output::ErrorEnvelope;
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

fn record(sandbox: &Sandbox, id: &str) -> Value {
    serde_json::from_slice(&fs::read(record_path(sandbox, id)).expect("record on disk"))
        .expect("record JSON")
}

/// Loom exports one `POST_PARTICIPANT` from a `bind --new` record, and that
/// record can sit idle past the day gc allows. A command that writes repairs
/// the claim, under the same id, and says so once with `bound_now`. A command
/// that only reads restores nothing: it is told the claim is collected and how
/// to bring it back, and the store is untouched.
#[test]
fn an_explicit_claim_on_a_deleted_record_gets_the_same_participant_back() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let minted = sandbox.run_without_identity(
        &[
            "participant",
            "bind",
            "--new",
            "--harness",
            "claude",
            "--json",
        ],
        &alpha,
    );
    assert_success(&minted);
    let minted: Value = from_stdout(&minted);
    let id = minted["participant"]["id"].as_str().expect("id").to_owned();
    assert_eq!(minted["participant"]["workspace"], "alpha", "{minted}");
    let stale = days_ago(2);
    patch(&sandbox, &id, |record| record["last_seen"] = json!(stale));
    let applied = gc(&sandbox, true);
    assert!(strings(&applied["deleted"]).contains(&id), "{applied}");
    assert!(!record_path(&sandbox, &id).exists());

    // Readers restore nothing. They report the claim as missing, with a fix
    // that names `participant restore <id>`, and create nothing.
    let restore_fix = format!("post participant restore {id}");
    let before = tree(&sandbox.mail_root);
    for args in [
        &["inbox", "--json"] as &[&str],
        &["read", "any-id", "--peek", "--json"],
        &["watch", "--snapshot", "--json"],
    ] {
        let read = sandbox.run_as_participant(args, &id, &alpha);
        assert_eq!(read.status.code(), Some(65), "{args:?}: {read:?}");
        let error: ErrorEnvelope = from_stderr(&read);
        assert_eq!(error.error.code, "participant_missing", "{args:?}");
        assert_eq!(
            error.error.details.exact_fix.as_deref(),
            Some(restore_fix.as_str()),
            "{args:?}"
        );
        assert!(
            error.error.message.contains("deleted"),
            "{args:?}: {error:?}"
        );
        assert_eq!(
            tree(&sandbox.mail_root),
            before,
            "{args:?} changed the store"
        );
        assert!(!record_path(&sandbox, &id).exists(), "{args:?}");
    }
    // The diagnostic surfaces carry the same fix as a field and still run.
    let shown = sandbox.run_as_participant(&["participant", "show", "--json"], &id, &alpha);
    assert_success(&shown);
    let shown: Value = from_stdout(&shown);
    let missing = shown
        .get("participant_missing")
        .unwrap_or_else(|| panic!("show lacks participant_missing: {shown}"));
    assert_eq!(missing["exact_fix"], restore_fix.as_str(), "{shown}");
    assert_eq!(tree(&sandbox.mail_root), before, "show changed the store");

    // A write brings it back, same id, and says so once.
    let touched = sandbox.run_as_participant(&["participant", "touch", "--json"], &id, &alpha);
    assert!(touched.status.success(), "{}", common::stderr(&touched));
    let touched: Value = from_stdout(&touched);
    assert_eq!(touched["bound_now"]["id"], id.as_str(), "{touched}");
    let back = record(&sandbox, &id);
    assert_eq!(back["id"], id.as_str());
    assert_eq!(back["ephemeral"], true);
    assert_eq!(back["workspace"], "alpha");
    assert_eq!(back["lease_hours"], 1);
    assert_ne!(
        back["last_seen"],
        json!(stale),
        "the claim proves it is alive"
    );
    assert!(
        !strings(&gc(&sandbox, false)["deleted"]).contains(&id),
        "a revived record is not collected again on the next pass"
    );

    // A write after another collection reports the revival once, with the same
    // id, and goes out as that participant.
    patch(&sandbox, &id, |record| record["last_seen"] = json!(stale));
    let applied = gc(&sandbox, true);
    assert!(strings(&applied["deleted"]).contains(&id), "{applied}");
    let sent = sandbox.run_as_participant(
        &["send", "--to", "beta", "--json", "--body", "back again"],
        &id,
        &alpha,
    );
    assert!(sent.status.success(), "{}", common::stderr(&sent));
    let receipt: Value = from_stdout(&sent);
    assert_eq!(receipt["bound_now"]["id"], id.as_str(), "{receipt}");
    assert_eq!(receipt["bound_now"]["workspace"], "alpha", "{receipt}");
    assert_eq!(receipt["envelope"]["from_participant"], id.as_str());
    let second = sandbox.run_as_participant(
        &["send", "--to", "beta", "--json", "--body", "and again"],
        &id,
        &alpha,
    );
    assert_success(&second);
    let second: Value = from_stdout(&second);
    assert!(second.get("bound_now").is_none(), "{second}");
}

/// The same for a tier-2 record: a write brings an archived participant back
/// whole (state included); a reader leaves the archive alone.
#[test]
fn an_explicit_claim_on_an_archived_record_restores_it_whole() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let by_reader = bound_idle(&sandbox, "claude", "archived-then-read", 1);
    let by_writer = bound_idle(&sandbox, "claude", "archived-then-written", 1);
    for id in [&by_reader, &by_writer] {
        give_state(&sandbox, id, "gc-claim");
        patch(&sandbox, id, |record| {
            record["last_seen"] = json!(days_ago(60));
        });
    }
    let memberships = |id: &str| {
        fs::read(
            sandbox
                .mail_root
                .join("participants")
                .join(id)
                .join("channels.json"),
        )
        .expect("memberships")
    };
    let reader_state = memberships(&by_reader);
    let writer_state = memberships(&by_writer);
    let applied = gc(&sandbox, true);
    assert_eq!(
        strings(&applied["archived"]),
        sorted(&[&by_reader, &by_writer])
    );
    let archive = |id: &str| sandbox.mail_root.join("participants-archive").join(id);
    let memberships_in_archive =
        |id: &str| fs::read(archive(id).join("channels.json")).expect("state");
    assert!(archive(&by_reader).exists() && !record_path(&sandbox, &by_reader).exists());

    // A reader leaves the archive where it is and says how to restore it.
    let before = tree(&sandbox.mail_root);
    let read = sandbox.run_as_participant(&["inbox", "--json"], &by_reader, &alpha);
    assert_eq!(read.status.code(), Some(65), "{read:?}");
    let error: ErrorEnvelope = from_stderr(&read);
    assert_eq!(error.error.code, "participant_missing");
    assert_eq!(
        error.error.details.exact_fix,
        Some(format!("post participant restore {by_reader}"))
    );
    assert!(error.error.message.contains("archived"), "{error:?}");
    assert_eq!(
        tree(&sandbox.mail_root),
        before,
        "a reader restores nothing"
    );
    assert!(archive(&by_reader).exists() && !record_path(&sandbox, &by_reader).exists());
    assert_eq!(
        memberships_in_archive(&by_reader),
        reader_state,
        "the archived state is untouched"
    );

    let sent = sandbox.run_as_participant(
        &[
            "send",
            "--to",
            "beta",
            "--json",
            "--body",
            "from the archive",
        ],
        &by_writer,
        &alpha,
    );
    assert!(sent.status.success(), "{}", common::stderr(&sent));
    let receipt: Value = from_stdout(&sent);
    assert_eq!(receipt["bound_now"]["id"], by_writer.as_str(), "{receipt}");
    assert_eq!(receipt["envelope"]["from_participant"], by_writer.as_str());
    assert!(!archive(&by_writer).exists());
    assert_eq!(memberships(&by_writer), writer_state);
}

/// `participant_missing` stays for a claim nothing ever held, and for a
/// collected record that cannot be brought back (and then says why).
#[test]
fn an_explicit_claim_nothing_can_restore_is_still_participant_missing() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);

    let never = sandbox.run_as_participant(&["inbox", "--json"], "claude-0badf00d", &alpha);
    assert_eq!(never.status.code(), Some(65), "{never:?}");
    let error: ErrorEnvelope = from_stderr(&never);
    assert_eq!(error.error.code, "participant_missing");
    assert!(
        !record_path(&sandbox, "claude-0badf00d").exists(),
        "nothing was minted"
    );

    // An archived record that is not a record cannot be restored: the error says
    // so instead of pretending the id never existed, and nothing is invented.
    let broken = sandbox
        .mail_root
        .join("participants-archive/claude-b10c0ded");
    fs::create_dir_all(&broken).expect("archive dir");
    fs::write(broken.join("participant.json"), b"not json").expect("garbage record");
    let refused = sandbox.run_as_participant(
        &["send", "--to", "beta", "--json", "--body", "hello"],
        "claude-b10c0ded",
        &alpha,
    );
    assert_eq!(refused.status.code(), Some(65), "{refused:?}");
    let error: ErrorEnvelope = from_stderr(&refused);
    assert_eq!(error.error.code, "participant_missing");
    assert!(
        error
            .error
            .details
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("could not be restored")),
        "{error:?}"
    );
    assert!(!record_path(&sandbox, "claude-b10c0ded").exists());
    assert_eq!(
        fs::read(broken.join("participant.json")).expect("the archive is untouched"),
        b"not json"
    );
}

/// The migration fence decides first. A write whose claim names a collected
/// record is refused and restores nothing: no record or directory comes back,
/// no tombstone or index changes, and nothing is stamped alive.
#[cfg(unix)]
#[test]
fn a_fenced_write_with_a_collected_claim_restores_nothing() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let deleted = bound_idle(&sandbox, "claude", "fenced-deleted", 40);
    let archived = bound_idle(&sandbox, "claude", "fenced-archived", 1);
    give_state(&sandbox, &archived, "gc-fenced");
    patch(&sandbox, &archived, |record| {
        record["last_seen"] = json!(days_ago(60));
    });
    let applied = gc(&sandbox, true);
    assert_eq!(applied["deleted"], json!([deleted]), "{applied}");
    assert_eq!(applied["archived"], json!([archived]), "{applied}");
    fs::write(sandbox.mail_root.join(".post-arx.lock"), b"").expect("migration lock");
    common::write_fence_state_locked(&sandbox.mail_root, 7);
    let before = tree(&sandbox.mail_root);

    for id in [&deleted, &archived] {
        for args in [
            &["send", "--to", "beta", "--json", "--body", "fenced"] as &[&str],
            &["participant", "touch", "--json"],
            &["chat", "fenced-room", "--join", "--json"],
        ] {
            let output = sandbox.run_as_participant(args, id, &alpha);
            common::assert_migration_refused(&output);
            assert_eq!(
                tree(&sandbox.mail_root),
                before,
                "{args:?} as {id} changed the store"
            );
        }
        assert!(!record_path(&sandbox, id).exists(), "{id} came back");
    }

    // A claim nothing ever held is still the claim's error, not the fence's:
    // it has nothing to restore, so it never waits on admission.
    let never = sandbox.run_as_participant(
        &["send", "--to", "beta", "--json", "--body", "fenced"],
        "claude-0badf00d",
        &alpha,
    );
    assert_eq!(never.status.code(), Some(65), "{never:?}");
    let error: ErrorEnvelope = from_stderr(&never);
    assert_eq!(error.error.code, "participant_missing");
    assert_eq!(tree(&sandbox.mail_root), before);
}

/// A participant that `participant gc` already collected is not a send target:
/// the send says so and writes nothing, in particular no directory holding mail
/// and no record. (A send already in flight when the collection happens is the
/// other case; it finds its record back, see `send::tests`.)
#[test]
fn a_letter_sent_to_an_already_collected_participant_is_refused_and_writes_nothing() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let target = bound_idle(&sandbox, "claude", "collected-target", 40);
    let applied = gc(&sandbox, true);
    assert_eq!(applied["deleted"], json!([target]), "{applied}");
    let before = tree(&sandbox.mail_root);

    let sent = sandbox.run_in(
        &[
            "send",
            "--to",
            &format!("participant:{target}"),
            "--json",
            "--body",
            "are you still there",
        ],
        None,
        &alpha,
    );
    assert_eq!(sent.status.code(), Some(66), "{sent:?}");
    let error: ErrorEnvelope = from_stderr(&sent);
    assert_eq!(error.error.code, "not_found");
    assert!(!sandbox
        .mail_root
        .join("participants")
        .join(&target)
        .exists());
    let after = tree(&sandbox.mail_root);
    let created: Vec<_> = after
        .keys()
        .filter(|path| !before.contains_key(*path))
        .collect();
    assert!(
        created
            .iter()
            .all(|path| !path.to_string_lossy().contains(&target)),
        "nothing was written for the target: {created:?}"
    );
}

/// Bringing a collected record back is done under the participants lock, the
/// lock `participant gc` collects under: a session that claims the record while
/// the lock is held waits for it rather than racing a collection.
#[cfg(unix)]
#[test]
fn reviving_a_claimed_record_waits_for_the_participants_lock() {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    use std::process::Stdio;
    use std::time::Duration;

    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let minted = sandbox.run_without_identity(
        &[
            "participant",
            "bind",
            "--new",
            "--harness",
            "claude",
            "--json",
        ],
        &alpha,
    );
    assert_success(&minted);
    let minted: Value = from_stdout(&minted);
    let id = minted["participant"]["id"].as_str().expect("id").to_owned();
    patch(&sandbox, &id, |record| {
        record["last_seen"] = json!(days_ago(2));
    });
    let applied = gc(&sandbox, true);
    assert!(strings(&applied["deleted"]).contains(&id), "{applied}");

    let lock = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(sandbox.mail_root.join(".participants.lock"))
        .expect("open participants lock");
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);
    let mut child = common::post_command()
        .args(["participant", "touch", "--json"])
        .current_dir(&alpha)
        .env_clear()
        .env("HOME", &sandbox.home)
        .env("POST_MAIL_ROOT", &sandbox.mail_root)
        .env("POST_PARTICIPANT", &id)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn touch");
    for _ in 0..20 {
        if child.try_wait().expect("probe touch").is_some() {
            panic!("the claim was revived without the participants lock");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !record_path(&sandbox, &id).exists(),
        "nothing moved while locked"
    );
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) }, 0);
    let output = child.wait_with_output().expect("wait for touch");
    assert_success(&output);
    assert!(
        record_path(&sandbox, &id).exists(),
        "revived once the lock was free"
    );
}

fn restore(sandbox: &Sandbox, id: &str) -> std::process::Output {
    sandbox.run_without_identity(&["participant", "restore", id, "--json"], &sandbox.path)
}

/// `participant restore` on a tier-2 record: the archive comes back whole, the
/// answer says where from, and asking again changes nothing.
#[test]
fn participant_restore_brings_an_archived_record_back_whole_and_is_idempotent() {
    let sandbox = Sandbox::new();
    let id = bound_idle(&sandbox, "claude", "restore-archived", 1);
    give_state(&sandbox, &id, "gc-restore");
    let dir = sandbox.mail_root.join("participants").join(&id);
    let memberships = fs::read(dir.join("channels.json")).expect("memberships exist");
    patch(&sandbox, &id, |record| {
        record["last_seen"] = json!(days_ago(60));
    });
    assert_eq!(gc(&sandbox, true)["archived"], json!([id]));
    let archive = sandbox.mail_root.join("participants-archive").join(&id);
    assert!(archive.exists() && !dir.exists());

    let output = restore(&sandbox, &id);
    assert_success(&output);
    let restored: Value = from_stdout(&output);
    assert_eq!(restored["ok"], true, "{restored}");
    assert_eq!(restored["id"], id.as_str());
    assert_eq!(restored["restored"], true, "{restored}");
    assert_eq!(restored["from"], "archive", "{restored}");
    assert_eq!(restored["participant"]["id"], id.as_str());
    assert!(!archive.exists(), "the archive moved back");
    assert_eq!(
        fs::read(dir.join("channels.json")).expect("memberships restored"),
        memberships,
        "state came back byte for byte"
    );
    assert_eq!(
        show_key(&sandbox, "claude", "restore-archived")["bound"],
        true
    );
    assert!(
        !strings(&gc(&sandbox, false)["archived"]).contains(&id),
        "a restored record is proof of life: the next pass keeps it"
    );

    // Again: already present is ok with nothing changed.
    let before = tree(&sandbox.mail_root);
    let again = restore(&sandbox, &id);
    assert_success(&again);
    let again: Value = from_stdout(&again);
    assert_eq!(again["ok"], true, "{again}");
    assert_eq!(again["restored"], false, "{again}");
    assert!(again.get("from").is_none(), "{again}");
    assert_eq!(again["participant"]["id"], id.as_str());
    assert_eq!(tree(&sandbox.mail_root), before, "nothing was written");
}

/// A tier-1 record held no state; restoring it recreates the same participant
/// (id, lease, workspace, display name) and its session mapping.
#[test]
fn participant_restore_recreates_a_deleted_record_under_its_own_id() {
    let sandbox = Sandbox::new();
    let id = bound_idle(&sandbox, "claude", "restore-deleted", 40);
    patch(&sandbox, &id, |record| {
        record["display_name"] = json!("Ember");
        record["lease_hours"] = json!(12);
    });
    let original = record(&sandbox, &id);
    assert_eq!(gc(&sandbox, true)["deleted"], json!([id]));
    assert!(!record_path(&sandbox, &id).exists());
    assert_eq!(
        show_key(&sandbox, "claude", "restore-deleted")["bound"],
        false
    );

    let output = restore(&sandbox, &id);
    assert_success(&output);
    let restored: Value = from_stdout(&output);
    assert_eq!(restored["restored"], true, "{restored}");
    assert_eq!(restored["from"], "tombstone", "{restored}");
    let back = record(&sandbox, &id);
    for field in [
        "id",
        "harness",
        "conversation_key_digest",
        "created",
        "workspace",
        "display_name",
        "lease_hours",
        "ephemeral",
    ] {
        assert_eq!(back[field], original[field], "{field}: {back}");
    }
    assert_ne!(back["last_seen"], original["last_seen"], "stamped alive");
    let shown = show_key(&sandbox, "claude", "restore-deleted");
    assert_eq!(shown["bound"], true, "{shown}");
    assert_eq!(shown["participant"]["id"], id.as_str(), "{shown}");

    let again = restore(&sandbox, &id);
    assert_success(&again);
    let again: Value = from_stdout(&again);
    assert_eq!(again["restored"], false, "{again}");
}

/// An id nothing holds is `participant_missing` and creates nothing, not even
/// the participants lock; a collected record that is not a record is the same
/// error, says why, and is left where it was.
#[test]
fn participant_restore_of_an_id_nothing_holds_is_participant_missing() {
    let sandbox = Sandbox::new();
    let kept = bound_idle(&sandbox, "claude", "restore-neighbor", 1);
    // Binding took the participants lock and left its file behind; without it
    // the comparison below can tell whether a wrong id takes the lock.
    fs::remove_file(sandbox.mail_root.join(".participants.lock")).expect("lock file");
    let before = tree(&sandbox.mail_root);

    let never = restore(&sandbox, "claude-0badf00d");
    assert_eq!(never.status.code(), Some(65), "{never:?}");
    let error: ErrorEnvelope = from_stderr(&never);
    assert_eq!(error.error.code, "participant_missing");
    assert_eq!(error.error.details.id.as_deref(), Some("claude-0badf00d"));
    assert_eq!(tree(&sandbox.mail_root), before, "nothing was created");
    assert!(record_path(&sandbox, &kept).exists());

    let malformed = restore(&sandbox, "../escape");
    assert_eq!(malformed.status.code(), Some(2), "{malformed:?}");
    assert_eq!(tree(&sandbox.mail_root), before);

    let broken = sandbox
        .mail_root
        .join("participants-archive/claude-b10c0ded");
    fs::create_dir_all(&broken).expect("archive dir");
    fs::write(broken.join("participant.json"), b"not json").expect("garbage record");
    let refused = restore(&sandbox, "claude-b10c0ded");
    assert_eq!(refused.status.code(), Some(65), "{refused:?}");
    let error: ErrorEnvelope = from_stderr(&refused);
    assert_eq!(error.error.code, "participant_missing");
    assert!(
        error
            .error
            .details
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("could not be restored")),
        "{error:?}"
    );
    assert!(!record_path(&sandbox, "claude-b10c0ded").exists());
    assert_eq!(
        fs::read(broken.join("participant.json")).expect("the archive is untouched"),
        b"not json"
    );
}

/// Restoring and applying a collection change the participant registry, so a
/// migration fence refuses them like any other writer, and nothing moves.
#[cfg(unix)]
#[test]
fn participant_restore_and_gc_apply_are_refused_under_a_migration_fence() {
    let sandbox = Sandbox::new();
    let collected = bound_idle(&sandbox, "claude", "fenced-restore", 40);
    assert_eq!(gc(&sandbox, true)["deleted"], json!([collected]));
    let idle = bound_idle(&sandbox, "claude", "fenced-gc", 40);
    fs::write(sandbox.mail_root.join(".post-arx.lock"), b"").expect("migration lock");
    common::write_fence_state_locked(&sandbox.mail_root, 7);

    common::assert_migration_refused(&restore(&sandbox, &collected));
    assert!(!record_path(&sandbox, &collected).exists());
    let applied =
        sandbox.run_without_identity(&["participant", "gc", "--apply", "--json"], &sandbox.path);
    common::assert_migration_refused(&applied);
    assert!(
        record_path(&sandbox, &idle).exists(),
        "nothing was collected"
    );
}

/// A session whose own `POST_PARTICIPANT` names nothing can still restore some
/// other participant: `restore` acts on the id it is given, not on the claim.
#[test]
fn participant_restore_runs_for_a_session_with_a_stale_claim() {
    let sandbox = Sandbox::new();
    let id = bound_idle(&sandbox, "claude", "restore-by-stale", 40);
    assert_eq!(gc(&sandbox, true)["deleted"], json!([id]));

    let output = sandbox.run_as_participant(
        &["participant", "restore", &id, "--json"],
        "claude-0badf00d",
        &sandbox.path,
    );
    assert_success(&output);
    let restored: Value = from_stdout(&output);
    assert_eq!(restored["restored"], true, "{restored}");
    assert!(record_path(&sandbox, &id).exists());
    assert!(
        !record_path(&sandbox, "claude-0badf00d").exists(),
        "the stale claim itself was not revived or minted"
    );
}

/// Restoring takes the participants lock, the one `participant gc` collects
/// under, so it cannot race a collection.
#[cfg(unix)]
#[test]
fn participant_restore_waits_for_the_participants_lock() {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    use std::process::Stdio;
    use std::time::Duration;

    let sandbox = Sandbox::new();
    let id = bound_idle(&sandbox, "claude", "restore-locked", 40);
    assert_eq!(gc(&sandbox, true)["deleted"], json!([id]));

    let lock = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(sandbox.mail_root.join(".participants.lock"))
        .expect("open participants lock");
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);
    let mut child = common::post_command()
        .args(["participant", "restore", &id, "--json"])
        .current_dir(&sandbox.path)
        .env_clear()
        .env("HOME", &sandbox.home)
        .env("POST_MAIL_ROOT", &sandbox.mail_root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn restore");
    for _ in 0..20 {
        if child.try_wait().expect("probe restore").is_some() {
            panic!("the record was restored without the participants lock");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !record_path(&sandbox, &id).exists(),
        "nothing came back while locked"
    );
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) }, 0);
    let output = child.wait_with_output().expect("wait for restore");
    assert_success(&output);
    assert!(record_path(&sandbox, &id).exists());
}
