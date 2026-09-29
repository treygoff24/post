//! `post doctor`, `post who`, and `post rooms` report the store truthfully:
//! a healthy store reads healthy, the bridge's own complaints show up where
//! agents look, and a name a peer host owns is not handed out twice. Every
//! test uses a throwaway store; nothing here touches the real mail store.

mod common;

use common::{
    assert_success, create_default_room_paths, from_stderr, from_stdout, stderr, stdout, Sandbox,
};
use post::output::{DoctorOutput, ErrorEnvelope, RoomsOutput};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().expect("parent directory")).expect("create parent");
    fs::write(path, contents).expect("write fixture");
}

/// The default rooms' directories and the archive: what a store that has
/// carried mail before has. (The rooms' own `inbox/` and `read/` appear only
/// with a room's first mail, so they stay absent.)
fn healthy_store(sandbox: &Sandbox) {
    create_default_room_paths(sandbox);
    fs::create_dir_all(sandbox.mail_root.join("archive")).expect("create archive");
}

/// `count` participants whose leases ran out long ago (plus `unleased` that
/// never had a lease record), the way a store that has run for weeks looks.
fn seed_stale_participants(sandbox: &Sandbox, count: usize, unleased: usize) {
    for index in 0..count + unleased {
        let id = format!("old-session-{index:03}");
        let record = json!({
            "version": 1,
            "id": id,
            "harness": "claude",
            "conversation_key_digest": format!("{index:064x}"),
            "created": "2026-01-01 00:00:00 +0000",
            "last_seen": if index < count { json!("2026-01-02T00:00:00Z") } else { Value::Null },
            "lease_hours": 24,
            "workspace": Value::Null,
            "workspace_path": Value::Null,
            "lineage": Value::Null,
            "lineage_since": Value::Null,
        });
        write(
            &sandbox
                .mail_root
                .join("participants")
                .join(&id)
                .join("participant.json"),
            &format!("{record}\n"),
        );
    }
}

/// Run `post doctor <args>` and parse the report, whatever its exit code.
fn doctor(sandbox: &Sandbox, args: &[&str]) -> (Option<i32>, DoctorOutput) {
    let mut argv = vec!["doctor"];
    argv.extend_from_slice(args);
    let output = sandbox.run(&argv);
    (output.status.code(), from_stdout(&output))
}

fn ids(report: &DoctorOutput) -> Vec<&str> {
    report
        .checks
        .iter()
        .map(|check| check.id.as_str())
        .collect()
}

// ---------------------------------------------------------------------------
// Task 5: doctor honesty
// ---------------------------------------------------------------------------

/// The Mac's shape: registered rooms nobody has written to (no inbox/ or
/// read/), hundreds of expired sessions, no bridge. Doctor used to report
/// every one of those as a finding and call the store "broken".
#[test]
fn doctor_calls_a_healthy_store_healthy() {
    let sandbox = Sandbox::new();
    healthy_store(&sandbox);
    seed_stale_participants(&sandbox, 40, 5);
    for room in ["claude-space", "pact", "agent-memory"] {
        assert!(
            !sandbox.mail_root.join(room).join("inbox").exists(),
            "fixture: {room} has never received mail"
        );
    }

    let (code, report) = doctor(&sandbox, &[]);
    assert_eq!(code, Some(0), "{:?}", report.checks);
    assert!(report.ok, "{:?}", report.checks);
    assert_eq!(report.status, "healthy");
    assert_eq!(report.count, 0);
    assert!(
        !ids(&report)
            .iter()
            .any(|id| id.contains("inbox_missing") || id.contains("read_missing")),
        "an unwritten room is not a fault: {:?}",
        ids(&report)
    );

    // 45 expired sessions are one line, not 45.
    let stale: Vec<_> = report
        .checks
        .iter()
        .filter(|check| check.id.starts_with("participant"))
        .collect();
    assert_eq!(stale.len(), 1, "{:?}", ids(&report));
    assert_eq!(stale[0].id, "participants.stale");
    assert_eq!(stale[0].severity, post::output::DoctorSeverity::Info);
    assert!(
        stale[0].message.contains("45 participant(s)")
            && stale[0].message.contains("40 with an expired lease")
            && stale[0].message.contains("5 with no lease record"),
        "the one line carries the count: {}",
        stale[0].message
    );
    assert!(
        report.checks.len() <= 3,
        "a healthy store's report stays short: {:?}",
        ids(&report)
    );
}

#[test]
fn doctor_fix_does_not_invent_room_mailboxes() {
    let sandbox = Sandbox::new();
    healthy_store(&sandbox);
    let output = sandbox.run(&["doctor", "--fix"]);
    let report: DoctorOutput = from_stdout(&output);
    assert!(report.ok, "{:?}", report.checks);
    assert!(
        !sandbox.mail_root.join("claude-space/inbox").exists()
            && !sandbox.mail_root.join("claude-space/read").exists(),
        "--fix must not create the directories doctor no longer asks for"
    );
}

#[test]
fn doctor_severity_trims_the_report_and_says_so() {
    let sandbox = Sandbox::new();
    healthy_store(&sandbox);
    seed_stale_participants(&sandbox, 3, 0);
    // One warning (a bridge attention item) and one error (a missing archive
    // directory).
    write(
        &sandbox.mail_root.join("bridge/health.json"),
        &json!({"v": 1, "attention": [
            {"kind": "held", "id": "m1", "summary": "letter m1 is held", "fix": "post-bridge release m1"}
        ]})
        .to_string(),
    );
    fs::remove_dir(sandbox.mail_root.join("archive")).expect("break the archive");

    let (_, full) = doctor(&sandbox, &[]);
    assert_eq!(full.severity_filter, None, "no filter, no field");
    let full_ids = ids(&full);
    assert!(full_ids.contains(&"participants.stale"), "{full_ids:?}");
    assert!(
        full_ids.iter().any(|id| id.starts_with("bridge.attention")),
        "{full_ids:?}"
    );
    assert_eq!(full.status, "broken", "{:?}", full.checks);

    let (warn_code, warn) = doctor(&sandbox, &["--severity", "warn"]);
    assert_eq!(warn_code, Some(1));
    assert_eq!(warn.severity_filter.as_deref(), Some("warn"));
    assert!(
        !ids(&warn).contains(&"participants.stale"),
        "{:?}",
        ids(&warn)
    );
    assert!(
        ids(&warn)
            .iter()
            .any(|id| id.starts_with("bridge.attention")),
        "warn keeps warnings: {:?}",
        ids(&warn)
    );
    assert!(
        warn.checks
            .iter()
            .all(|check| check.severity != post::output::DoctorSeverity::Info),
        "{:?}",
        warn.checks
    );

    let (error_code, error) = doctor(&sandbox, &["--severity", "error"]);
    assert_eq!(error_code, Some(1));
    assert_eq!(error.severity_filter.as_deref(), Some("error"));
    assert!(
        error
            .checks
            .iter()
            .all(|check| check.severity == post::output::DoctorSeverity::Error),
        "error keeps errors only: {:?}",
        error.checks
    );
    assert!(!error.checks.is_empty());
    assert_eq!(error.count, error.checks.len());

    // Once the error is repaired, the same store is clean at `error` and
    // degraded (warnings remain) at `warn`, and each says which lens it used.
    fs::create_dir(sandbox.mail_root.join("archive")).expect("repair the archive");
    let (clean_code, clean) = doctor(&sandbox, &["--severity", "error"]);
    assert_eq!(clean_code, Some(0), "{:?}", clean.checks);
    assert!(clean.ok);
    assert_eq!(clean.status, "healthy");
    assert_eq!(clean.severity_filter.as_deref(), Some("error"));
    let (degraded_code, degraded) = doctor(&sandbox, &["--severity", "warn"]);
    assert_eq!(degraded_code, Some(1));
    assert_eq!(degraded.status, "degraded");

    let refused = sandbox.run(&["doctor", "--severity", "info"]);
    assert_eq!(
        refused.status.code(),
        Some(2),
        "stderr: {}",
        stderr(&refused)
    );
}

#[test]
fn doctor_shows_the_bridges_attention_items_with_their_fixes() {
    let sandbox = Sandbox::new();
    healthy_store(&sandbox);
    write(
        &sandbox.mail_root.join("bridge/health.json"),
        &json!({
            "v": 1,
            "ok": true,
            "attention": [
                {"kind": "quarantined", "id": "20260928-1", "summary": "inbound letter 20260928-1 was quarantined", "fix": "post bridge release 20260928-1"},
                {"kind": "collision", "summary": "room 'tax' exists on both hosts", "fix": "rename one with `post rooms rename`"},
                {"kind": "held"}
            ]
        })
        .to_string(),
    );

    let (code, report) = doctor(&sandbox, &[]);
    assert_eq!(code, Some(1));
    assert_eq!(report.status, "degraded", "warnings are not errors");
    assert!(!report.ok);
    let attention: Vec<_> = report
        .checks
        .iter()
        .filter(|check| check.id.starts_with("bridge.attention"))
        .collect();
    assert_eq!(attention.len(), 3, "{:?}", ids(&report));
    for check in &attention {
        assert_eq!(check.severity, post::output::DoctorSeverity::Warning);
        assert!(!check.fixable);
    }
    let quarantined = attention
        .iter()
        .find(|check| check.id == "bridge.attention.quarantined.20260928-1")
        .expect("id carries kind and item id");
    assert_eq!(
        quarantined.message,
        "inbound letter 20260928-1 was quarantined"
    );
    assert_eq!(quarantined.suggested_fix, "post bridge release 20260928-1");
    let collision = attention
        .iter()
        .find(|check| check.id == "bridge.attention.collision")
        .expect("an item without an id");
    assert_eq!(
        collision.suggested_fix,
        "rename one with `post rooms rename`"
    );
    let bare = attention
        .iter()
        .find(|check| check.id == "bridge.attention.held")
        .expect("an item with only a kind");
    assert!(
        !bare.suggested_fix.is_empty(),
        "an item with no fix still points somewhere"
    );
}

#[test]
fn doctor_tolerates_an_absent_or_broken_bridge_health_file() {
    let sandbox = Sandbox::new();
    healthy_store(&sandbox);
    let health = sandbox.mail_root.join("bridge/health.json");

    for (label, contents) in [
        ("absent", None),
        ("malformed", Some("{ this is not json")),
        ("no attention key", Some(r#"{"v":1,"ok":true}"#)),
        ("empty attention", Some(r#"{"attention":[]}"#)),
        ("wrong type", Some(r#"{"attention":"none"}"#)),
    ] {
        if let Some(contents) = contents {
            write(&health, contents);
        }
        let (code, report) = doctor(&sandbox, &[]);
        assert_eq!(code, Some(0), "{label}: {:?}", report.checks);
        assert!(report.ok, "{label}: {:?}", report.checks);
        assert!(
            !ids(&report).iter().any(|id| id.starts_with("bridge.")),
            "{label}: {:?}",
            ids(&report)
        );
    }
}

#[test]
fn doctor_reports_skill_drift_as_a_warning_with_its_fix() {
    let sandbox = Sandbox::new();
    healthy_store(&sandbox);
    let served = sandbox.home.join(".agents/skill-library/post");

    // No served skill: nothing to compare, nothing to report.
    let (code, report) = doctor(&sandbox, &[]);
    assert_eq!(code, Some(0), "{:?}", report.checks);
    assert!(!ids(&report).contains(&"skill.drift"));

    // A served skill that is not the one this binary was built with.
    write(&served.join("SKILL.md"), "# a stale skill\n");
    let (code, report) = doctor(&sandbox, &[]);
    assert_eq!(code, Some(1), "{:?}", report.checks);
    assert_eq!(
        report.status, "degraded",
        "drift is a warning, not an error"
    );
    let drift = report
        .checks
        .iter()
        .find(|check| check.id == "skill.drift")
        .expect("drift is reported");
    assert_eq!(drift.severity, post::output::DoctorSeverity::Warning);
    assert!(
        drift.message.contains("SKILL.md"),
        "the message names what differs: {}",
        drift.message
    );
    assert!(
        drift.suggested_fix.contains("install-post.sh"),
        "the fix says how to converge: {}",
        drift.suggested_fix
    );

    // The served skill IS this build's skill (a symlink to the checkout the
    // test binary was built from): no drift.
    fs::remove_dir_all(&served).expect("remove the stale copy");
    std::os::unix::fs::symlink(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("skills/post"),
        &served,
    )
    .expect("link the served skill to the build's copy");
    let (code, report) = doctor(&sandbox, &[]);
    assert_eq!(code, Some(0), "{:?}", report.checks);
    assert!(!ids(&report).contains(&"skill.drift"), "{:?}", ids(&report));
}

// ---------------------------------------------------------------------------
// Task 6: who shows bridge attention and the doorbell supervisor
// ---------------------------------------------------------------------------

fn who_json(sandbox: &Sandbox) -> Value {
    let output = sandbox.run(&["who", "--json"]);
    assert_success(&output);
    from_stdout(&output)
}

fn participant<'a>(who: &'a Value, id: &str) -> &'a Value {
    who["participants"]
        .as_array()
        .expect("participants")
        .iter()
        .find(|entry| entry["id"] == id)
        .unwrap_or_else(|| panic!("participant {id} in {who}"))
}

fn age_file(path: &Path, age: Duration) {
    let file = fs::File::options()
        .write(true)
        .open(path)
        .expect("open health file");
    file.set_modified(SystemTime::now() - age)
        .expect("age the health file");
}

#[test]
fn who_counts_the_bridges_attention_items() {
    let sandbox = Sandbox::new();

    let quiet = who_json(&sandbox);
    assert!(
        quiet.get("bridge_attention").is_none(),
        "no bridge, no field: {quiet}"
    );
    assert!(!stdout(&sandbox.run(&["who", "--text"])).contains("bridge_attention"));

    write(
        &sandbox.mail_root.join("bridge/health.json"),
        &json!({"attention": [
            {"kind": "held", "id": "a", "summary": "one", "fix": "x"},
            {"kind": "collision", "summary": "two", "fix": "y"}
        ]})
        .to_string(),
    );
    let loud = who_json(&sandbox);
    assert_eq!(loud["bridge_attention"], 2, "{loud}");
    let text = stdout(&sandbox.run(&["who", "--text"]));
    assert!(text.contains("bridge_attention: 2"), "{text}");

    // Every field consumers already parse is still there.
    let me = participant(&loud, "test-default");
    for key in ["id", "harness", "state", "unread", "pending", "live_watch"] {
        assert!(me.get(key).is_some(), "who --json lost {key}: {me}");
    }
    for key in ["ok", "participants", "legacy_rooms", "count"] {
        assert!(loud.get(key).is_some(), "who --json lost {key}: {loud}");
    }
}

#[test]
fn who_live_watch_reflects_an_armed_doorbell_subscription() {
    let sandbox = Sandbox::new();
    let quiet_id = sandbox.seed_session_only_participant();
    let health = sandbox.mail_root.join("doorbell/health.json");
    write(
        &health,
        &json!({
            "bindings": [
                {"participant": "test-default", "armed": true},
                {"participant": quiet_id, "armed": false}
            ],
            "residents": [{"room": "claude-space", "armed": true}]
        })
        .to_string(),
    );

    let who = who_json(&sandbox);
    assert_eq!(who["doorbell"], "fresh", "{who}");
    let armed = participant(&who, "test-default");
    assert_eq!(armed["live_watch"], true, "{armed}");
    assert_eq!(armed["doorbell_armed"], true, "{armed}");
    let unarmed = participant(&who, &quiet_id);
    assert_eq!(unarmed["live_watch"], false, "{unarmed}");
    assert!(unarmed.get("doorbell_armed").is_none(), "{unarmed}");
    let room = who["legacy_rooms"]
        .as_array()
        .expect("legacy rooms")
        .iter()
        .find(|room| room["room"] == "claude-space")
        .expect("claude-space");
    assert_eq!(room["live_watch"], true, "{room}");
    assert_eq!(room["doorbell_armed"], true, "{room}");
    let text = stdout(&sandbox.run(&["who", "--text"]));
    assert!(text.contains("doorbell=armed"), "{text}");
}

#[test]
fn who_ignores_a_doorbell_file_that_cannot_vouch_and_says_so() {
    let sandbox = Sandbox::new();
    let health = sandbox.mail_root.join("doorbell/health.json");
    let armed = json!({"bindings": [{"participant": "test-default", "armed": true}]}).to_string();

    // A supervisor that stopped refreshing its file (dead) arms nothing.
    write(&health, &armed);
    age_file(&health, Duration::from_secs(600));
    let stale = who_json(&sandbox);
    assert_eq!(stale["doorbell"], "stale", "{stale}");
    assert_eq!(participant(&stale, "test-default")["live_watch"], false);
    assert!(participant(&stale, "test-default")
        .get("doorbell_armed")
        .is_none());
    let text = stdout(&sandbox.run(&["who", "--text"]));
    assert!(
        text.contains("doorbell: stale"),
        "live-watch=no must not read as 'nobody is armed': {text}"
    );

    // An unreadable file is the same, and says so.
    write(&health, "{ not json");
    let broken = who_json(&sandbox);
    assert_eq!(broken["doorbell"], "unreadable", "{broken}");
    assert_eq!(participant(&broken, "test-default")["live_watch"], false);
    assert!(stdout(&sandbox.run(&["who", "--text"])).contains("doorbell: unreadable"));

    // No file at all: the field is absent and `live_watch` is the heartbeat
    // alone, as before.
    fs::remove_file(&health).expect("remove the health file");
    let absent = who_json(&sandbox);
    assert!(absent.get("doorbell").is_none(), "{absent}");
    assert_eq!(participant(&absent, "test-default")["live_watch"], false);
}

// ---------------------------------------------------------------------------
// Task 8: rooms on a bridged host
// ---------------------------------------------------------------------------

fn bridged_sandbox() -> Sandbox {
    let sandbox = Sandbox::new();
    healthy_store(&sandbox);
    write(
        &sandbox.mail_root.join("bridge/config.json"),
        r#"{"host":"trey"}"#,
    );
    write(
        &sandbox.mail_root.join("bridge/rooms/peers/devbox.json"),
        r#"{"v":1,"host":"devbox","rooms":["tax","hq"]}"#,
    );
    sandbox
}

fn checkout(sandbox: &Sandbox, name: &str) -> String {
    let dir = sandbox.path.join("checkouts").join(name);
    fs::create_dir_all(&dir).expect("create checkout");
    dir.to_string_lossy().into_owned()
}

#[test]
fn rooms_add_refuses_a_name_a_peer_host_publishes() {
    let sandbox = bridged_sandbox();
    let path = checkout(&sandbox, "tax");
    let before = fs::read(sandbox.mail_root.join("rooms.json")).expect("rooms snapshot");

    let refused = sandbox.run(&["rooms", "add", "tax", &path]);
    assert_eq!(
        refused.status.code(),
        Some(2),
        "stderr: {}",
        stderr(&refused)
    );
    let error: ErrorEnvelope = from_stderr(&refused);
    assert_eq!(error.error.code, "invalid_argument");
    assert_eq!(error.error.details.host.as_deref(), Some("devbox"));
    assert_eq!(error.error.details.room.as_deref(), Some("tax"));
    assert!(
        error
            .error
            .message
            .contains("published by peer host 'devbox'"),
        "{}",
        error.error.message
    );
    let fix = error.error.details.exact_fix.expect("a runnable fix");
    assert_eq!(fix, format!("post rooms add 'tax-trey' '{path}'"));
    assert_eq!(
        fs::read(sandbox.mail_root.join("rooms.json")).expect("rooms after"),
        before,
        "the refusal wrote nothing"
    );

    // The suggested name registers.
    assert_success(&sandbox.run_fix(&fix, &sandbox.path));
    let listing: RoomsOutput = from_stdout(&sandbox.run(&["rooms"]));
    assert!(listing.rooms.iter().any(|room| room.name == "tax-trey"));
    assert!(!listing.rooms.iter().any(|room| room.name == "tax"));
}

#[test]
fn a_peers_room_name_is_matched_case_insensitively() {
    let sandbox = bridged_sandbox();
    let path = checkout(&sandbox, "tax-upper");
    let refused = sandbox.run(&["rooms", "add", "TAX", &path]);
    assert_eq!(
        refused.status.code(),
        Some(2),
        "stderr: {}",
        stderr(&refused)
    );
    let error: ErrorEnvelope = from_stderr(&refused);
    assert_eq!(error.error.details.host.as_deref(), Some("devbox"));
}

#[test]
fn the_ownership_memory_also_protects_a_name() {
    let sandbox = bridged_sandbox();
    write(
        &sandbox.mail_root.join("bridge/rooms/owners.json"),
        r#"{"legal":{"host":"devbox"},"mine":{"host":"trey"}}"#,
    );
    let legal = checkout(&sandbox, "legal");
    let refused = sandbox.run(&["rooms", "add", "legal", &legal]);
    assert_eq!(
        refused.status.code(),
        Some(2),
        "stderr: {}",
        stderr(&refused)
    );

    // A name this host owns itself is not a peer's.
    let mine = checkout(&sandbox, "mine");
    assert_success(&sandbox.run(&["rooms", "add", "mine", &mine]));
}

#[test]
fn rooms_add_is_untouched_without_a_bridge_or_with_unreadable_publications() {
    // Not bridged: a peers file alone proves nothing about this host.
    let sandbox = Sandbox::new();
    healthy_store(&sandbox);
    write(
        &sandbox.mail_root.join("bridge/rooms/peers/devbox.json"),
        r#"{"v":1,"host":"devbox","rooms":["tax"]}"#,
    );
    let tax = checkout(&sandbox, "tax");
    assert_success(&sandbox.run(&["rooms", "add", "tax", &tax]));

    // Bridged, but every publication is unusable: malformed, mislabeled, or
    // written under another host's file name.
    let sandbox = Sandbox::new();
    healthy_store(&sandbox);
    write(
        &sandbox.mail_root.join("bridge/config.json"),
        r#"{"host":"trey"}"#,
    );
    write(
        &sandbox.mail_root.join("bridge/rooms/peers/devbox.json"),
        "{ nope",
    );
    write(
        &sandbox.mail_root.join("bridge/rooms/peers/other.json"),
        r#"{"v":1,"host":"someone-else","rooms":["hq"]}"#,
    );
    write(&sandbox.mail_root.join("bridge/rooms/owners.json"), "[1,2]");
    for name in ["tax", "hq"] {
        let path = checkout(&sandbox, name);
        assert_success(&sandbox.run(&["rooms", "add", name, &path]));
    }
}

#[test]
fn rooms_rename_refuses_a_peers_name_too() {
    let sandbox = bridged_sandbox();
    let path = checkout(&sandbox, "scratch");
    assert_success(&sandbox.run(&["rooms", "add", "scratch", &path]));

    let refused = sandbox.run(&["rooms", "rename", "scratch", "hq"]);
    assert_eq!(
        refused.status.code(),
        Some(2),
        "stderr: {}",
        stderr(&refused)
    );
    let error: ErrorEnvelope = from_stderr(&refused);
    assert_eq!(error.error.details.host.as_deref(), Some("devbox"));
    assert_eq!(
        error.error.details.exact_fix.as_deref(),
        Some("post rooms rename 'scratch' 'hq-trey'")
    );
    let listing: RoomsOutput = from_stdout(&sandbox.run(&["rooms"]));
    assert!(listing.rooms.iter().any(|room| room.name == "scratch"));
}
