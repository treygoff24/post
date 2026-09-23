//! `post bridge deliver`: the destination half of a host-qualified
//! participant DM (design rev 3.1, "Destination delivery", "The import
//! command's contract", and "Crash states and recovery").
//!
//! Every test runs against a temporary store. The fault hook
//! (`POST_TEST_DELIVER_FAULT`) exists only in debug builds, which is what
//! `cargo test` runs.

mod common;

use common::{post_command, register_room, stderr, stdout, tree_snapshot, Sandbox};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Output, Stdio};
use std::time::{Duration, Instant};

const OWN_HOST: &str = "here";
const PEER: &str = "peer";
const PLACEHOLDER: &str = "peer-room";
const SENDER: &str = "codex-peer0001";
const MAIL_ID: &str = "20260923-010000-a1b2c3";

struct Rig {
    sandbox: Sandbox,
    recipient: String,
}

impl Rig {
    fn new() -> Self {
        let sandbox = Sandbox::new();
        let bridge = sandbox.mail_root.join("bridge");
        fs::create_dir_all(&bridge).expect("bridge dir");
        fs::write(
            bridge.join("config.json"),
            json!({"host": OWN_HOST, "relay_url": "ssh://relay.invalid/relay.git"}).to_string(),
        )
        .expect("bridge config");
        common::create_default_room_paths(&sandbox);
        let placeholder = sandbox
            .mail_root
            .join("remote")
            .join(PEER)
            .join(PLACEHOLDER);
        fs::create_dir_all(&placeholder).expect("placeholder dir");
        register_room(&sandbox, PLACEHOLDER, &placeholder);
        let recipient = sandbox.test_participant("claude-space");
        Self { sandbox, recipient }
    }

    fn root(&self) -> &Path {
        &self.sandbox.mail_root
    }

    fn envelope(&self) -> serde_json::Map<String, Value> {
        json!({
            "id": MAIL_ID,
            "from": PLACEHOLDER,
            "to": self.recipient,
            "kind": "letter",
            "subject": "hello",
            "sent": "2026-09-23 01:00:00 -0500",
            "from_participant": SENDER,
            "address_kind": "participant",
            "to_host": OWN_HOST,
            "sender_provenance": "participant-binding"
        })
        .as_object()
        .expect("object")
        .clone()
    }

    /// Write letter bytes to a relay temp file; returns (path, bytes).
    fn letter_file(
        &self,
        envelope: &serde_json::Map<String, Value>,
        name: &str,
    ) -> (PathBuf, Vec<u8>) {
        let bytes = format!(
            "{}\n---\nbody of {name}\n",
            serde_json::to_string_pretty(envelope).expect("encode envelope")
        )
        .into_bytes();
        let relay = self.sandbox.path.join("relay");
        fs::create_dir_all(&relay).expect("relay dir");
        let path = relay.join(format!("{name}.tmp"));
        fs::write(&path, &bytes).expect("letter file");
        (path, bytes)
    }

    fn standard_letter(&self) -> (PathBuf, Vec<u8>) {
        self.letter_file(&self.envelope(), "standard")
    }

    fn record_path(&self) -> PathBuf {
        self.root()
            .join("participants")
            .join(&self.recipient)
            .join("imports")
            .join(format!("{MAIL_ID}.json"))
    }

    fn inbox_path(&self) -> PathBuf {
        self.root()
            .join("participants")
            .join(&self.recipient)
            .join("inbox")
            .join(format!("{MAIL_ID}.mail"))
    }

    fn command(
        &self,
        participant: &str,
        source_host: &str,
        mail_id: &str,
        sha256: &str,
        file: &Path,
        envs: &[(&str, &str)],
    ) -> std::process::Command {
        let mut command = post_command();
        command
            .args([
                "bridge",
                "deliver",
                "--participant",
                participant,
                "--source-host",
                source_host,
                "--mail-id",
                mail_id,
                "--sha256",
                sha256,
                "--file",
            ])
            .arg(file)
            .arg("--json")
            .current_dir(&self.sandbox.path)
            .env("HOME", &self.sandbox.home)
            .env("POST_MAIL_ROOT", &self.sandbox.mail_root)
            .env_remove("POST_TEST_DELIVER_FAULT")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in envs {
            command.env(key, value);
        }
        command
    }

    fn raw(
        &self,
        participant: &str,
        source_host: &str,
        mail_id: &str,
        sha256: &str,
        file: &Path,
        envs: &[(&str, &str)],
    ) -> Output {
        self.command(participant, source_host, mail_id, sha256, file, envs)
            .output()
            .expect("run post bridge deliver")
    }

    /// Deliver the standard arguments for `file`/`bytes` and return the
    /// decision, asserting the frozen contract on the way.
    fn deliver(&self, file: &Path, bytes: &[u8]) -> Value {
        self.deliver_from(PEER, file, bytes, &[])
    }

    fn deliver_from(
        &self,
        source: &str,
        file: &Path,
        bytes: &[u8],
        envs: &[(&str, &str)],
    ) -> Value {
        let sha = hex(bytes);
        let output = self.raw(&self.recipient, source, MAIL_ID, &sha, file, envs);
        decision(&output, &self.recipient, source, MAIL_ID, &sha)
    }

    /// The store, minus the lock files every writer may create.
    fn snapshot(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut tree = tree_snapshot(self.root());
        tree.retain(|path, _| {
            !path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".lock"))
        });
        tree
    }

    fn set_rooms(&self, rooms: Value) {
        fs::write(self.root().join("rooms.json"), rooms.to_string()).expect("rooms.json");
    }

    fn rooms(&self) -> serde_json::Map<String, Value> {
        serde_json::from_slice::<Value>(&fs::read(self.root().join("rooms.json")).expect("rooms"))
            .expect("rooms json")
            .as_object()
            .expect("rooms object")
            .clone()
    }
}

/// One envelope edit in the revalidation table.
type Mutation = dyn Fn(&mut serde_json::Map<String, Value>);

fn hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Parse one decision and check the frozen contract for every decided
/// outcome: exit 0, one object, the exact schema, echoed fields, post's own
/// digest, and `admitted_at` non-null exactly for `delivered`.
fn decision(output: &Output, participant: &str, source: &str, mail_id: &str, sha: &str) -> Value {
    assert_eq!(
        output.status.code(),
        Some(0),
        "a decided outcome exits 0\nstdout: {}\nstderr: {}",
        stdout(output),
        stderr(output)
    );
    let text = stdout(output);
    assert_eq!(text.lines().count(), 1, "exactly one JSON line: {text}");
    let value: Value = serde_json::from_str(&text).expect("decision JSON");
    let object = value.as_object().expect("decision object");
    let keys: Vec<&str> = object.keys().map(String::as_str).collect();
    let mut expected = vec![
        "ok",
        "schema",
        "outcome",
        "reason",
        "participant",
        "mail_id",
        "source_host",
        "sha256",
        "admitted_at",
        "replay",
        "detail",
    ];
    expected.sort_unstable();
    let mut sorted = keys.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, expected, "exact key set");
    assert_eq!(value["ok"], json!(true));
    assert_eq!(value["schema"], json!("post.bridge-deliver.v1"));
    assert_eq!(value["participant"], json!(participant));
    assert_eq!(value["mail_id"], json!(mail_id));
    assert_eq!(value["source_host"], json!(source));
    assert!(value["replay"].is_boolean());
    let outcome = value["outcome"].as_str().expect("outcome string");
    assert!(
        matches!(outcome, "delivered" | "rejected" | "retry"),
        "{outcome}"
    );
    assert_eq!(
        value["admitted_at"].is_string(),
        outcome == "delivered",
        "admitted_at is non-null exactly for delivered: {value}"
    );
    if outcome == "delivered" {
        assert!(value["reason"].is_null());
        assert_eq!(
            value["sha256"],
            json!(sha),
            "post's digest equals the bridge's"
        );
    } else {
        assert!(value["reason"].is_string(), "{value}");
        assert!(value["detail"]
            .as_str()
            .is_some_and(|d| d.chars().count() <= 512));
    }
    value
}

fn assert_outcome(value: &Value, outcome: &str, reason: Option<&str>) {
    assert_eq!(value["outcome"], json!(outcome), "{value}");
    assert_eq!(value["reason"], json!(reason), "{value}");
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).expect("read json")).expect("parse json")
}

// ---------------------------------------------------------------- happy path

#[test]
fn a_first_delivery_writes_the_record_then_the_inbox_file_and_routes_it() {
    let rig = Rig::new();
    let (file, bytes) = rig.standard_letter();
    let value = rig.deliver(&file, &bytes);
    assert_outcome(&value, "delivered", None);
    assert_eq!(value["replay"], json!(false));
    let record = read_json(&rig.record_path());
    assert_eq!(
        record
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        [
            "admitted_at",
            "from_participant",
            "mail_id",
            "participant",
            "sha256",
            "source_host",
            "v"
        ]
    );
    assert_eq!(record["v"], json!(1));
    assert_eq!(record["participant"], json!(rig.recipient));
    assert_eq!(record["source_host"], json!(PEER));
    assert_eq!(record["sha256"], json!(hex(&bytes)));
    assert_eq!(record["from_participant"], json!(SENDER));
    assert_eq!(record["admitted_at"], value["admitted_at"]);
    assert_eq!(
        fs::read(rig.inbox_path()).unwrap(),
        bytes,
        "canonical bytes unchanged"
    );
    // Routed like a local participant letter: the recipient reads it.
    let peek = rig.sandbox.run_as_participant(
        &["read", MAIL_ID, "--peek", "--json"],
        &rig.recipient,
        &rig.sandbox.home.join("claude-space"),
    );
    assert!(peek.status.success(), "{}", stderr(&peek));
    // A second identical call is a replay with the original admitted_at.
    let again = rig.deliver(&file, &bytes);
    assert_outcome(&again, "delivered", None);
    assert_eq!(again["replay"], json!(true));
    assert_eq!(again["admitted_at"], value["admitted_at"]);
}

// ----------------------------------------------- structural revalidation

#[test]
fn each_envelope_revalidation_rule_rejects_alone_and_writes_nothing() {
    let rig = Rig::new();
    let cases: Vec<(&str, Box<Mutation>, &str)> = vec![
        (
            "participant disagreement",
            Box::new(|e| {
                e.insert("to".into(), json!("someone-else"));
            }),
            "to_mismatch",
        ),
        (
            "mail-id disagreement",
            Box::new(|e| {
                e.insert("id".into(), json!("20260923-010000-ffffff"));
            }),
            "malformed",
        ),
        (
            "address_kind workspace",
            Box::new(|e| {
                e.insert("address_kind".into(), json!("workspace"));
            }),
            "malformed",
        ),
        (
            "address_kind absent",
            Box::new(|e| {
                e.remove("address_kind");
            }),
            "malformed",
        ),
        (
            "foreign to_host",
            Box::new(|e| {
                e.insert("to_host".into(), json!("elsewhere"));
            }),
            "to_mismatch",
        ),
        (
            "to_host absent",
            Box::new(|e| {
                e.remove("to_host");
            }),
            "to_mismatch",
        ),
        (
            "from_participant with @",
            Box::new(|e| {
                e.insert("from_participant".into(), json!("codex-peer0001@peer"));
            }),
            "malformed",
        ),
        (
            "from_participant grammar",
            Box::new(|e| {
                e.insert("from_participant".into(), json!("a/b"));
            }),
            "malformed",
        ),
        (
            "from_participant absent",
            Box::new(|e| {
                e.remove("from_participant");
            }),
            "malformed",
        ),
        (
            "from room grammar",
            Box::new(|e| {
                e.insert("from".into(), json!("a/b"));
            }),
            "malformed",
        ),
        (
            "from not homed at the source host (unregistered)",
            Box::new(|e| {
                e.insert("from".into(), json!("nobody-room"));
            }),
            "forged_from",
        ),
        (
            "from names a local room",
            Box::new(|e| {
                e.insert("from".into(), json!("claude-space"));
            }),
            "forged_from",
        ),
    ];
    let before = rig.snapshot();
    for (label, mutate, reason) in cases {
        let mut envelope = rig.envelope();
        mutate(&mut envelope);
        let (file, bytes) = rig.letter_file(&envelope, "case");
        let value = rig.deliver(&file, &bytes);
        assert_eq!(value["reason"], json!(reason), "{label}: {value}");
        assert_outcome(&value, "rejected", Some(reason));
        assert_eq!(rig.snapshot(), before, "{label}: nothing written");
    }
    // Garbage bytes are malformed too.
    let relay = rig.sandbox.path.join("relay/garbage.tmp");
    fs::write(&relay, b"not a letter").unwrap();
    let value = rig.deliver(&relay, b"not a letter");
    assert_outcome(&value, "rejected", Some("malformed"));
    assert_eq!(rig.snapshot(), before);
}

#[test]
fn a_from_placeholder_of_another_host_is_forged_from_with_nothing_written() {
    let rig = Rig::new();
    let other = rig.root().join("remote/other/other-room");
    fs::create_dir_all(&other).unwrap();
    register_room(&rig.sandbox, "other-room", &other);
    let mut envelope = rig.envelope();
    envelope.insert("from".into(), json!("other-room"));
    let (file, bytes) = rig.letter_file(&envelope, "forged");
    let before = rig.snapshot();
    let value = rig.deliver(&file, &bytes);
    assert_outcome(&value, "rejected", Some("forged_from"));
    assert!(
        value["detail"]
            .as_str()
            .unwrap()
            .contains("placeholder of other"),
        "{value}"
    );
    assert_eq!(rig.snapshot(), before);
    assert!(!rig.record_path().exists() && !rig.inbox_path().exists());
}

#[test]
fn a_digest_mismatch_is_a_retry_not_a_rejection() {
    let rig = Rig::new();
    let (file, _) = rig.standard_letter();
    let before = rig.snapshot();
    let wrong = "0".repeat(64);
    let output = rig.raw(&rig.recipient, PEER, MAIL_ID, &wrong, &file, &[]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_outcome(&value, "retry", Some("digest_mismatch"));
    assert_eq!(
        value["sha256"],
        json!(hex(&fs::read(&file).unwrap())),
        "post's own digest"
    );
    assert!(value["admitted_at"].is_null());
    assert_eq!(rig.snapshot(), before);
}

#[test]
fn argument_grammar_failures_are_usage_errors_with_nothing_written() {
    let rig = Rig::new();
    let (file, bytes) = rig.standard_letter();
    let sha = hex(&bytes);
    let before = rig.snapshot();
    let missing = rig.sandbox.path.join("relay/missing.tmp");
    let cases: Vec<(&str, String, &str, &str, String, PathBuf)> = vec![
        (
            "source-host grammar",
            rig.recipient.clone(),
            "Peer!",
            MAIL_ID,
            sha.clone(),
            file.clone(),
        ),
        (
            "source-host is this host",
            rig.recipient.clone(),
            OWN_HOST,
            MAIL_ID,
            sha.clone(),
            file.clone(),
        ),
        (
            "participant grammar",
            "a/b".into(),
            PEER,
            MAIL_ID,
            sha.clone(),
            file.clone(),
        ),
        (
            "participant with ':'",
            "a:b".into(),
            PEER,
            MAIL_ID,
            sha.clone(),
            file.clone(),
        ),
        (
            "mail-id grammar",
            rig.recipient.clone(),
            PEER,
            "not-an-id",
            sha.clone(),
            file.clone(),
        ),
        (
            "sha256 uppercase",
            rig.recipient.clone(),
            PEER,
            MAIL_ID,
            sha.to_uppercase(),
            file.clone(),
        ),
        (
            "sha256 short",
            rig.recipient.clone(),
            PEER,
            MAIL_ID,
            "abc".into(),
            file.clone(),
        ),
        (
            "missing file",
            rig.recipient.clone(),
            PEER,
            MAIL_ID,
            sha.clone(),
            missing,
        ),
        (
            "directory file",
            rig.recipient.clone(),
            PEER,
            MAIL_ID,
            sha.clone(),
            rig.sandbox.path.clone(),
        ),
    ];
    for (label, participant, source, mail_id, digest, path) in cases {
        let output = rig.raw(&participant, source, mail_id, &digest, &path, &[]);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{label}: {}",
            stderr(&output)
        );
        assert!(stdout(&output).is_empty(), "{label}: no decision on stdout");
        assert_eq!(rig.snapshot(), before, "{label}");
    }
}

#[test]
fn an_unreadable_bridge_config_is_retryable_topology_unavailable() {
    let rig = Rig::new();
    let (file, bytes) = rig.standard_letter();
    for config in [None, Some("{not json"), Some(r#"{"host":"BAD HOST"}"#)] {
        let path = rig.root().join("bridge/config.json");
        match config {
            None => {
                let _ = fs::remove_file(&path);
            }
            Some(text) => fs::write(&path, text).unwrap(),
        }
        let before = rig.snapshot();
        let value = rig.deliver(&file, &bytes);
        assert_outcome(&value, "retry", Some("topology_unavailable"));
        assert_eq!(rig.snapshot(), before, "{config:?}");
    }
}

#[test]
fn a_missing_or_broken_rooms_registry_is_retryable_topology_unavailable() {
    let rig = Rig::new();
    let (file, bytes) = rig.standard_letter();
    fs::write(rig.root().join("rooms.json"), "{broken").unwrap();
    let before = rig.snapshot();
    assert_outcome(
        &rig.deliver(&file, &bytes),
        "retry",
        Some("topology_unavailable"),
    );
    assert_eq!(rig.snapshot(), before);
    fs::remove_file(rig.root().join("rooms.json")).unwrap();
    let before = rig.snapshot();
    assert_outcome(
        &rig.deliver(&file, &bytes),
        "retry",
        Some("topology_unavailable"),
    );
    assert_eq!(
        rig.snapshot(),
        before,
        "a missing registry is never 'no placeholder'"
    );
}

// ---------------------------------------------- admission record first

fn crash(rig: &Rig, point: &str, file: &Path, bytes: &[u8]) {
    let fault = format!("crash-after-{point}");
    let output = rig.raw(
        &rig.recipient,
        PEER,
        MAIL_ID,
        &hex(bytes),
        file,
        &[("POST_TEST_DELIVER_FAULT", fault.as_str())],
    );
    assert_eq!(
        output.status.code(),
        Some(86),
        "the injected crash fired: {}",
        stderr(&output)
    );
    assert!(stdout(&output).is_empty(), "a crash prints no decision");
}

#[test]
fn a_crash_after_d1_leaves_only_the_record_and_the_rerun_completes_it() {
    let rig = Rig::new();
    let (file, bytes) = rig.standard_letter();
    crash(&rig, "d1", &file, &bytes);
    assert!(rig.record_path().is_file(), "D1 wrote the admission record");
    assert!(
        !rig.inbox_path().exists(),
        "no inbox bytes before the record"
    );
    let admitted = read_json(&rig.record_path())["admitted_at"].clone();
    let value = rig.deliver(&file, &bytes);
    assert_outcome(&value, "delivered", None);
    assert_eq!(value["replay"], json!(true));
    assert_eq!(value["admitted_at"], admitted, "the original admitted_at");
    assert_eq!(fs::read(rig.inbox_path()).unwrap(), bytes);
}

#[test]
fn a_crash_after_d2_leaves_record_and_inbox_and_the_rerun_verifies_them() {
    let rig = Rig::new();
    let (file, bytes) = rig.standard_letter();
    crash(&rig, "d2", &file, &bytes);
    assert!(rig.record_path().is_file());
    assert_eq!(fs::read(rig.inbox_path()).unwrap(), bytes);
    let admitted = read_json(&rig.record_path())["admitted_at"].clone();
    let before = rig.snapshot();
    let value = rig.deliver(&file, &bytes);
    assert_outcome(&value, "delivered", None);
    assert_eq!(value["replay"], json!(true));
    assert_eq!(value["admitted_at"], admitted);
    let after = rig.snapshot();
    let changed: Vec<_> = after
        .iter()
        .filter(|(path, bytes)| before.get(*path) != Some(*bytes))
        .map(|(path, _)| path.clone())
        .collect();
    assert!(
        changed
            .iter()
            .all(|path| path.to_string_lossy().contains("/routing/")
                || path.to_string_lossy().contains(".routing")),
        "a replay writes nothing but the routing receipt: {changed:?}"
    );
    assert_eq!(fs::read(rig.inbox_path()).unwrap(), bytes);
}

// ------------------------------- replay skips the mutable admission checks

#[test]
fn a_replay_after_the_placeholder_is_removed_still_delivers() {
    let rig = Rig::new();
    let (file, bytes) = rig.standard_letter();
    crash(&rig, "d1", &file, &bytes);
    let mut rooms = rig.rooms();
    rooms.remove(PLACEHOLDER);
    rig.set_rooms(Value::Object(rooms));
    let value = rig.deliver(&file, &bytes);
    assert_outcome(&value, "delivered", None);
    assert_eq!(value["replay"], json!(true));
    assert_eq!(fs::read(rig.inbox_path()).unwrap(), bytes);
}

#[test]
fn a_replay_after_the_placeholder_is_rehomed_still_delivers() {
    let rig = Rig::new();
    let (file, bytes) = rig.standard_letter();
    crash(&rig, "d1", &file, &bytes);
    let rehomed = rig.root().join("remote/other").join(PLACEHOLDER);
    fs::create_dir_all(&rehomed).unwrap();
    let mut rooms = rig.rooms();
    rooms.insert(PLACEHOLDER.into(), json!(rehomed.to_string_lossy()));
    rig.set_rooms(Value::Object(rooms));
    let value = rig.deliver(&file, &bytes);
    assert_outcome(&value, "delivered", None);
    assert_eq!(value["replay"], json!(true));
}

#[test]
fn a_replay_after_the_participant_ends_still_delivers() {
    let rig = Rig::new();
    let (file, bytes) = rig.standard_letter();
    crash(&rig, "d1", &file, &bytes);
    let ended = rig.sandbox.run_as_participant(
        &["participant", "end", "--json"],
        &rig.recipient,
        &rig.sandbox.home.join("claude-space"),
    );
    assert!(ended.status.success(), "{}", stderr(&ended));
    assert!(rig.sandbox.read_participant(&rig.recipient)["ended_at"].is_string());
    let value = rig.deliver(&file, &bytes);
    assert_outcome(&value, "delivered", None);
    assert_eq!(value["replay"], json!(true));
    assert_eq!(fs::read(rig.inbox_path()).unwrap(), bytes);
}

#[test]
fn a_replay_after_a_blocking_rule_is_added_still_delivers() {
    let rig = Rig::new();
    let (file, bytes) = rig.standard_letter();
    crash(&rig, "d1", &file, &bytes);
    fs::write(
        rig.root().join("rules.json"),
        json!({"blocked": [{"from": "*", "to": "*", "reason": "closed after admission"}]})
            .to_string(),
    )
    .unwrap();
    let value = rig.deliver(&file, &bytes);
    assert_outcome(&value, "delivered", None);
    assert_eq!(value["replay"], json!(true));
    assert_eq!(fs::read(rig.inbox_path()).unwrap(), bytes);
}

// ------------------------------------------------------------- collisions

#[test]
fn a_cross_host_same_bytes_retry_is_an_id_collision_and_changes_nothing() {
    let rig = Rig::new();
    let (file, bytes) = rig.standard_letter();
    assert_outcome(&rig.deliver(&file, &bytes), "delivered", None);
    let record = fs::read(rig.record_path()).unwrap();
    let inbox = fs::read(rig.inbox_path()).unwrap();
    let before = rig.snapshot();
    let value = rig.deliver_from("other", &file, &bytes, &[]);
    assert_outcome(&value, "rejected", Some("id_collision"));
    assert_eq!(fs::read(rig.record_path()).unwrap(), record);
    assert_eq!(fs::read(rig.inbox_path()).unwrap(), inbox);
    assert_eq!(rig.snapshot(), before);
}

#[test]
fn different_bytes_under_an_admitted_id_are_an_id_collision() {
    let rig = Rig::new();
    let (file, bytes) = rig.standard_letter();
    assert_outcome(&rig.deliver(&file, &bytes), "delivered", None);
    let mut envelope = rig.envelope();
    envelope.insert("subject".into(), json!("changed"));
    let (changed, changed_bytes) = rig.letter_file(&envelope, "changed");
    let before = rig.snapshot();
    let value = rig.deliver(&changed, &changed_bytes);
    assert_outcome(&value, "rejected", Some("id_collision"));
    assert_eq!(rig.snapshot(), before);
}

#[test]
fn an_unrecorded_inbox_file_with_identical_bytes_is_an_id_collision() {
    let rig = Rig::new();
    let (file, bytes) = rig.standard_letter();
    fs::create_dir_all(rig.inbox_path().parent().unwrap()).unwrap();
    fs::write(rig.inbox_path(), &bytes).unwrap();
    let before = rig.snapshot();
    let value = rig.deliver(&file, &bytes);
    assert_outcome(&value, "rejected", Some("id_collision"));
    assert_eq!(
        rig.snapshot(),
        before,
        "no origin is invented for an unrecorded file"
    );
    assert!(!rig.record_path().exists());
}

#[test]
fn a_matching_record_with_a_mismatched_inbox_file_is_an_id_collision_preserving_both() {
    let rig = Rig::new();
    let (file, bytes) = rig.standard_letter();
    crash(&rig, "d1", &file, &bytes);
    fs::create_dir_all(rig.inbox_path().parent().unwrap()).unwrap();
    fs::write(rig.inbox_path(), b"someone else's bytes").unwrap();
    let record = fs::read(rig.record_path()).unwrap();
    let value = rig.deliver(&file, &bytes);
    assert_outcome(&value, "rejected", Some("id_collision"));
    assert_eq!(fs::read(rig.record_path()).unwrap(), record);
    assert_eq!(fs::read(rig.inbox_path()).unwrap(), b"someone else's bytes");
}

#[test]
fn an_unreadable_admission_record_is_a_retry_and_writes_nothing() {
    let rig = Rig::new();
    let (file, bytes) = rig.standard_letter();
    for garbage in [
        "{not json".to_owned(),
        json!({"v": 2, "participant": rig.recipient, "mail_id": MAIL_ID, "source_host": PEER,
               "sha256": hex(&bytes), "from_participant": SENDER, "admitted_at": "2026-09-23T06:00:00Z"})
        .to_string(),
        json!({"v": 1, "participant": rig.recipient, "mail_id": MAIL_ID, "source_host": PEER,
               "sha256": hex(&bytes), "from_participant": SENDER, "admitted_at": "2026-09-23T06:00:00Z",
               "extra": true})
        .to_string(),
    ] {
        fs::create_dir_all(rig.record_path().parent().unwrap()).unwrap();
        fs::write(rig.record_path(), &garbage).unwrap();
        let before = rig.snapshot();
        let value = rig.deliver(&file, &bytes);
        assert_outcome(&value, "retry", Some("import_record_unreadable"));
        assert_eq!(rig.snapshot(), before, "{garbage}");
        assert!(!rig.inbox_path().exists());
    }
}

// ----------------------------------------------------- participant lookups

#[test]
fn an_absent_participant_is_terminal_and_an_unreadable_one_is_retryable() {
    let rig = Rig::new();
    let mut envelope = rig.envelope();
    envelope.insert("to".into(), json!("ghost-00000000"));
    let (file, bytes) = rig.letter_file(&envelope, "ghost");
    let before = rig.snapshot();
    let sha = hex(&bytes);
    let output = rig.raw("ghost-00000000", PEER, MAIL_ID, &sha, &file, &[]);
    let value = decision(&output, "ghost-00000000", PEER, MAIL_ID, &sha);
    assert_outcome(&value, "rejected", Some("unknown_participant"));
    assert_eq!(rig.snapshot(), before);

    let (file, bytes) = rig.standard_letter();
    let record = rig
        .root()
        .join("participants")
        .join(&rig.recipient)
        .join("participant.json");
    fs::write(&record, "{corrupt").unwrap();
    let before = rig.snapshot();
    let value = rig.deliver(&file, &bytes);
    assert_outcome(&value, "retry", Some("participant_unreadable"));
    assert_eq!(rig.snapshot(), before);
}

#[test]
fn an_ended_participant_is_terminal_with_nothing_written() {
    let rig = Rig::new();
    let (file, bytes) = rig.standard_letter();
    let ended = rig.sandbox.run_as_participant(
        &["participant", "end", "--json"],
        &rig.recipient,
        &rig.sandbox.home.join("claude-space"),
    );
    assert!(ended.status.success(), "{}", stderr(&ended));
    let before = rig.snapshot();
    let value = rig.deliver(&file, &bytes);
    assert_outcome(&value, "rejected", Some("ended_participant"));
    assert_eq!(rig.snapshot(), before);
}

#[test]
fn a_blocked_route_to_a_workspace_less_recipient_writes_nothing() {
    let rig = Rig::new();
    let solo = rig.sandbox.seed_session_only_participant();
    fs::write(
        rig.root().join("rules.json"),
        json!({"blocked": [{"from": "*", "to": "*", "reason": "no inbound DMs"}]}).to_string(),
    )
    .unwrap();
    let mut envelope = rig.envelope();
    envelope.insert("to".into(), json!(solo));
    let (file, bytes) = rig.letter_file(&envelope, "blocked");
    let sha = hex(&bytes);
    let before = rig.snapshot();
    let output = rig.raw(&solo, PEER, MAIL_ID, &sha, &file, &[]);
    let value = decision(&output, &solo, PEER, MAIL_ID, &sha);
    assert_outcome(&value, "rejected", Some("blocked_route"));
    assert_eq!(
        rig.snapshot(),
        before,
        "no admission record, no inbox bytes"
    );
    let imports = rig.root().join("participants").join(&solo).join("imports");
    assert!(!imports.exists());
}

#[test]
fn under_a_migration_fence_nothing_is_written() {
    let rig = Rig::new();
    let (file, bytes) = rig.standard_letter();
    fs::write(rig.root().join(".post-arx.lock"), b"").unwrap();
    common::write_fence_state_locked(rig.root(), 7);
    let before = rig.snapshot();
    let value = rig.deliver(&file, &bytes);
    assert_outcome(&value, "retry", Some("fenced"));
    assert_eq!(rig.snapshot(), before);
}

// ------------------------------------------------------------ concurrency

fn wait_for(path: &Path, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !path.exists() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn finish(child: Child) -> Output {
    child.wait_with_output().expect("wait for child")
}

/// A reader that polls while delivery is paused must never see the letter
/// as local-own or without remote origin.
fn assert_reader_never_sees_local_origin(rig: &Rig) -> usize {
    let mut seen = 0;
    for _ in 0..5 {
        let snapshot = rig.sandbox.run_as_participant(
            &["watch", "--snapshot"],
            &rig.recipient,
            &rig.sandbox.home.join("claude-space"),
        );
        assert!(snapshot.status.success(), "{}", stderr(&snapshot));
        for line in stdout(&snapshot).lines() {
            let event: Value = serde_json::from_str(line).expect("event json");
            if event["id"] != json!(MAIL_ID) {
                continue;
            }
            seen += 1;
            assert_eq!(event["origin"], json!("remote"), "{event}");
            assert_ne!(
                event["reply_to_participant"],
                json!(format!("participant:{}", rig.recipient)),
                "{event}"
            );
        }
    }
    seen
}

#[test]
fn a_concurrent_reader_during_paused_delivery_never_sees_a_local_origin() {
    for point in ["d1", "d2"] {
        let rig = Rig::new();
        // The collision case: the remote sender's id equals the reader's own.
        let mut envelope = rig.envelope();
        envelope.insert("from_participant".into(), json!(rig.recipient));
        let (file, bytes) = rig.letter_file(&envelope, "collide");
        let release = rig.sandbox.path.join(format!("release-{point}"));
        let paused = rig.sandbox.path.join(format!("release-{point}.paused"));
        let fault = format!("pause-after-{point}:{}", release.display());
        let child = rig
            .command(
                &rig.recipient,
                PEER,
                MAIL_ID,
                &hex(&bytes),
                &file,
                &[("POST_TEST_DELIVER_FAULT", fault.as_str())],
            )
            .spawn()
            .expect("spawn paused delivery");
        wait_for(&paused, "the paused delivery");
        assert!(
            rig.record_path().is_file(),
            "{point}: record exists before any inbox bytes"
        );
        assert_eq!(rig.inbox_path().exists(), point == "d2");
        let during = assert_reader_never_sees_local_origin(&rig);
        // After D1 the letter is invisible; after D2 the reader sees it
        // (pending, unrouted), so the check above is not vacuous.
        assert_eq!(during > 0, point == "d2", "{point}: seen {during} times");
        fs::write(&release, b"go").unwrap();
        let output = finish(child);
        let value = decision(&output, &rig.recipient, PEER, MAIL_ID, &hex(&bytes));
        assert_outcome(&value, "delivered", None);
        assert!(
            assert_reader_never_sees_local_origin(&rig) > 0,
            "{point}: seen after delivery"
        );
    }
}

#[test]
fn ending_the_participant_concurrently_with_admission_serializes() {
    let rig = Rig::new();
    let (file, bytes) = rig.standard_letter();
    let release = rig.sandbox.path.join("release-end");
    let paused = rig.sandbox.path.join("release-end.paused");
    let fault = format!("pause-after-d1:{}", release.display());
    let delivery = rig
        .command(
            &rig.recipient,
            PEER,
            MAIL_ID,
            &hex(&bytes),
            &file,
            &[("POST_TEST_DELIVER_FAULT", fault.as_str())],
        )
        .spawn()
        .expect("spawn paused delivery");
    wait_for(&paused, "the paused delivery");
    let mut end = post_command();
    end.args(["participant", "end", "--json"])
        .current_dir(rig.sandbox.home.join("claude-space"))
        .env("HOME", &rig.sandbox.home)
        .env("POST_MAIL_ROOT", &rig.sandbox.mail_root)
        .env("POST_PARTICIPANT", &rig.recipient)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut end = end.spawn().expect("spawn participant end");
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        end.try_wait().expect("poll end").is_none(),
        "participant end must wait for the admission holding the lock"
    );
    assert!(rig.sandbox.read_participant(&rig.recipient)["ended_at"].is_null());
    fs::write(&release, b"go").unwrap();
    let value = decision(
        &finish(delivery),
        &rig.recipient,
        PEER,
        MAIL_ID,
        &hex(&bytes),
    );
    assert_outcome(&value, "delivered", None);
    let ended = finish(end);
    assert!(ended.status.success(), "{}", stderr(&ended));
    assert!(rig.sandbox.read_participant(&rig.recipient)["ended_at"].is_string());
    assert_eq!(fs::read(rig.inbox_path()).unwrap(), bytes);
}
