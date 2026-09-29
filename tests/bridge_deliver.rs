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
    // The replay routes: the exact receipt exists for the admitted recipient
    // and the letter reads. Last, because the consuming read changes state.
    assert_routed_to_the_recipient_and_readable(&rig);
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
    // Routing is bound by the admission decision, not the rule added since:
    // the recipient can read what the bridge will ack as delivered.
    assert_routed_to_the_recipient_and_readable(&rig);
}

#[test]
fn a_blocking_rule_added_between_admission_and_routing_does_not_strand_the_letter() {
    let rig = Rig::new();
    let (file, bytes) = rig.standard_letter();
    let release = rig.sandbox.path.join("release-rule");
    let paused = rig.sandbox.path.join("release-rule.paused");
    let fault = format!("pause-after-d1:{}", release.display());
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
    fs::write(
        rig.root().join("rules.json"),
        json!({"blocked": [{"from": "*", "to": "*", "reason": "closed mid-delivery"}]}).to_string(),
    )
    .unwrap();
    fs::write(&release, b"go").unwrap();
    let value = decision(&finish(child), &rig.recipient, PEER, MAIL_ID, &hex(&bytes));
    assert_outcome(&value, "delivered", None);
    assert_eq!(value["replay"], json!(false));
    assert_routed_to_the_recipient_and_readable(&rig);
}

/// The routing receipt names exactly the admitted participant, and a
/// consuming `post read` of the id returns the letter.
fn assert_routed_to_the_recipient_and_readable(rig: &Rig) {
    let receipt_path = rig
        .root()
        .join("participants")
        .join(&rig.recipient)
        .join("routing")
        .join(format!("{MAIL_ID}.json"));
    let receipt: Value =
        serde_json::from_slice(&fs::read(&receipt_path).expect("deliver wrote a routing receipt"))
            .expect("receipt json");
    assert_eq!(receipt["recipients"], json!([rig.recipient]), "{receipt}");
    let read = rig.reader_json(&["read", MAIL_ID, "--json"]);
    assert_ne!(read.get("pending"), Some(&json!(true)), "{read}");
    assert_eq!(read["envelope"]["id"], json!(MAIL_ID), "{read}");
}

#[test]
fn a_failed_routing_receipt_is_a_retry_never_a_delivery() {
    let rig = Rig::new();
    let (file, bytes) = rig.standard_letter();
    // A file where the routing directory belongs: the receipt cannot be written.
    let routing = rig
        .root()
        .join("participants")
        .join(&rig.recipient)
        .join("routing");
    fs::create_dir_all(routing.parent().unwrap()).unwrap();
    fs::write(&routing, b"not a directory").unwrap();
    let value = rig.deliver(&file, &bytes);
    assert_outcome(&value, "retry", Some("io_error"));
    fs::remove_file(&routing).unwrap();
    let again = rig.deliver(&file, &bytes);
    assert_outcome(&again, "delivered", None);
    assert_eq!(again["replay"], json!(true));
    assert_routed_to_the_recipient_and_readable(&rig);
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
    assert_eq!(value["replay"], json!(true), "the record already existed");
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
    assert_eq!(value["replay"], json!(true), "the record already existed");
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
    assert_eq!(value["replay"], json!(false), "no record existed");
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
    assert_eq!(value["replay"], json!(true), "the record already existed");
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

/// `participant gc` may collect an idle recipient while a letter for it is on
/// its way. The import holds the participants lock, and brings the record back
/// under the same id instead of refusing the letter as `unknown_participant`.
#[test]
fn a_recipient_collected_by_gc_is_brought_back_not_refused() {
    let rig = Rig::new();
    let (file, bytes) = rig.standard_letter();
    let record = rig
        .root()
        .join("participants")
        .join(&rig.recipient)
        .join("participant.json");
    let mut aged = read_json(&record);
    aged["last_seen"] = json!("2026-01-01T00:00:00Z");
    fs::write(&record, serde_json::to_vec_pretty(&aged).unwrap()).unwrap();
    let collected = rig.sandbox.run_without_identity(
        &["participant", "gc", "--apply", "--json"],
        &rig.sandbox.path,
    );
    assert!(collected.status.success(), "{}", stderr(&collected));
    assert!(
        !record.exists(),
        "gc collected the idle recipient: {}",
        stdout(&collected)
    );

    let value = rig.deliver(&file, &bytes);
    assert_outcome(&value, "delivered", None);
    assert!(record.is_file(), "the recipient is back under its own id");
    assert!(rig.inbox_path().is_file(), "and the letter is in its inbox");
    assert_eq!(read_json(&record)["id"], json!(rig.recipient));
}

/// Give the recipient real state (a channel membership), let it go idle past
/// the archive limit, and run `participant gc --apply`. Returns the state file's
/// bytes; the record is then in the archive, not in `participants/`.
fn archive_recipient(rig: &Rig) -> Vec<u8> {
    let joined = rig.sandbox.run_as_participant(
        &["chat", "bridge-archive", "--join", "--json"],
        &rig.recipient,
        &rig.sandbox.path,
    );
    assert!(joined.status.success(), "{}", stderr(&joined));
    let dir = rig.root().join("participants").join(&rig.recipient);
    let state = fs::read(dir.join("channels.json")).expect("the membership is state");
    let record = dir.join("participant.json");
    let mut aged = read_json(&record);
    aged["last_seen"] = json!("2026-01-01T00:00:00Z");
    fs::write(&record, serde_json::to_vec_pretty(&aged).unwrap()).unwrap();
    let collected = rig.sandbox.run_without_identity(
        &["participant", "gc", "--apply", "--json"],
        &rig.sandbox.path,
    );
    assert!(collected.status.success(), "{}", stderr(&collected));
    let collected: Value = serde_json::from_slice(&collected.stdout).expect("gc JSON");
    assert_eq!(
        collected["archived"],
        json!([rig.recipient]),
        "gc archived the idle recipient: {collected}"
    );
    assert!(!record.exists());
    state
}

/// The archived case: a recipient with state is moved whole to the archive, and
/// a delivery restores it (state included) under the lock, then delivers.
#[test]
fn a_recipient_archived_by_gc_is_restored_whole_and_delivered_to() {
    let rig = Rig::new();
    let (file, bytes) = rig.standard_letter();
    let state = archive_recipient(&rig);
    let archive = rig.root().join("participants-archive").join(&rig.recipient);
    assert!(archive.is_dir(), "the record is in the archive");

    let value = rig.deliver(&file, &bytes);
    assert_outcome(&value, "delivered", None);
    let dir = rig.root().join("participants").join(&rig.recipient);
    assert_eq!(
        read_json(&dir.join("participant.json"))["id"],
        json!(rig.recipient)
    );
    assert!(!archive.exists(), "the archive moved back");
    assert_eq!(
        fs::read(dir.join("channels.json")).unwrap(),
        state,
        "its state came back byte for byte"
    );
    assert_eq!(fs::read(rig.inbox_path()).unwrap(), bytes);
    assert!(rig.record_path().is_file());
}

/// A crash after D1 leaves only the admission record. If gc archives the idle
/// recipient before the rerun, the record is inside the archive: the delivery
/// restores it, finds its own admission record, and completes it as a replay
/// (same `admitted_at`) instead of failing to write a second one.
#[test]
fn a_rerun_after_the_recipient_was_archived_finds_its_admission_record() {
    let rig = Rig::new();
    let (file, bytes) = rig.standard_letter();
    crash(&rig, "d1", &file, &bytes);
    let admitted = read_json(&rig.record_path())["admitted_at"].clone();
    archive_recipient(&rig);
    assert!(!rig.record_path().exists(), "the record is in the archive");

    let value = rig.deliver(&file, &bytes);
    assert_outcome(&value, "delivered", None);
    assert_eq!(value["replay"], json!(true), "{value}");
    assert_eq!(value["admitted_at"], admitted, "the original admitted_at");
    assert_eq!(fs::read(rig.inbox_path()).unwrap(), bytes);
}

/// The admission record that comes back with the archive is the letter's
/// identity: different bytes under the same id are still an `id_collision`, and
/// nothing is delivered.
#[test]
fn different_bytes_after_the_recipient_was_archived_are_still_an_id_collision() {
    let rig = Rig::new();
    let (file, bytes) = rig.standard_letter();
    crash(&rig, "d1", &file, &bytes);
    archive_recipient(&rig);
    let (other_file, other_bytes) = rig.letter_file(&rig.envelope(), "other");
    assert_ne!(bytes, other_bytes);

    let value = rig.deliver(&other_file, &other_bytes);
    assert_outcome(&value, "rejected", Some("id_collision"));
    assert!(!rig.inbox_path().exists(), "nothing was delivered");
}

/// The restore belongs to admission, after the checks that need no recipient: a
/// letter refused for its sender (`forged_from`) leaves an archived recipient in
/// the archive.
#[test]
fn a_refused_letter_does_not_restore_an_archived_recipient() {
    let rig = Rig::new();
    archive_recipient(&rig);
    let mut envelope = rig.envelope();
    envelope.insert("from".into(), json!("space"));
    let (file, bytes) = rig.letter_file(&envelope, "forged");
    let before = rig.snapshot();
    let value = rig.deliver(&file, &bytes);
    assert_outcome(&value, "rejected", Some("forged_from"));
    assert_eq!(rig.snapshot(), before, "the archive stayed where it was");
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

// ---------------------------------------------------------------------------
// Origin (design "The receiving side"): an imported letter whose
// `from_participant` equals the reader's own id. After delivery the `from`
// placeholder is removed or re-homed, so the admission record is the only
// remote evidence left; every call site must still read the letter as remote
// with the admitted host, and never as the reader's own.

/// How the topology changes after a colliding letter is admitted.
#[derive(Clone, Copy, Debug)]
enum Topology {
    /// The placeholder is unregistered.
    Removed,
    /// The placeholder is re-homed under another host.
    RehomedRemote,
    /// A local room now carries the placeholder's name.
    RehomedLocal,
}

impl Rig {
    fn reader(&self, args: &[&str]) -> Output {
        let output = self.sandbox.run_as_participant(
            args,
            &self.recipient,
            &self.sandbox.home.join("claude-space"),
        );
        assert!(
            output.status.success(),
            "{args:?}\nstdout: {}\nstderr: {}",
            stdout(&output),
            stderr(&output)
        );
        output
    }

    fn reader_json(&self, args: &[&str]) -> Value {
        serde_json::from_str(&stdout(&self.reader(args))).expect("reader JSON")
    }

    /// Admit a letter whose `from_participant` is the recipient's own id,
    /// then change the topology so R7's placeholder evidence is gone.
    fn deliver_colliding(&self, topology: Topology) {
        let mut envelope = self.envelope();
        envelope.insert("from_participant".into(), json!(self.recipient));
        let (file, bytes) = self.letter_file(&envelope, "colliding");
        assert_outcome(&self.deliver(&file, &bytes), "delivered", None);
        let mut rooms = self.rooms();
        match topology {
            Topology::Removed => {
                rooms.remove(PLACEHOLDER);
            }
            Topology::RehomedRemote => {
                let path = self.root().join("remote").join("other").join(PLACEHOLDER);
                fs::create_dir_all(&path).expect("re-homed placeholder");
                rooms.insert(PLACEHOLDER.into(), json!(path));
            }
            Topology::RehomedLocal => {
                let path = self.sandbox.home.join(PLACEHOLDER);
                fs::create_dir_all(&path).expect("local room");
                rooms.insert(PLACEHOLDER.into(), json!(path));
            }
        }
        self.set_rooms(Value::Object(rooms));
    }

    fn admitted_reply(&self) -> String {
        format!("participant:{}@{PEER}", self.recipient)
    }
}

fn unread_item(inbox: &Value) -> Value {
    let unread = inbox["unread"].as_array().expect("unread array");
    let item = unread
        .iter()
        .find(|item| item["id"] == json!(MAIL_ID))
        .unwrap_or_else(|| panic!("the colliding import is not unread: {inbox}"));
    item.clone()
}

fn assert_admitted_remote(value: &Value, reply: &str) {
    assert_eq!(value["origin"], json!("remote"), "{value}");
    assert_eq!(value["reply_to_participant"], json!(reply), "{value}");
}

#[test]
fn origin_inbox_lists_a_colliding_import_as_unread_with_the_admitted_reply() {
    let rig = Rig::new();
    rig.deliver_colliding(Topology::Removed);
    let inbox = rig.reader_json(&["inbox", "--json"]);
    assert_admitted_remote(&unread_item(&inbox), &rig.admitted_reply());
    let text = stdout(&rig.reader(&["inbox", "--text"]));
    assert!(
        text.contains(&format!("reply={}", rig.admitted_reply())),
        "{text}"
    );
}

#[test]
fn origin_read_of_a_colliding_import_is_not_own_and_consumes_it() {
    let rig = Rig::new();
    rig.deliver_colliding(Topology::Removed);
    let text = stdout(&rig.reader(&["read", MAIL_ID, "--peek"]));
    assert!(!text.contains("own: true"), "{text}");
    assert!(
        text.contains(&format!("reply={}", rig.admitted_reply())),
        "{text}"
    );
    let read = rig.reader_json(&["read", MAIL_ID, "--json"]);
    assert_ne!(read.get("own"), Some(&json!(true)), "{read}");
    assert_admitted_remote(&read["envelope"], &rig.admitted_reply());
    let inbox = rig.reader_json(&["inbox", "--json"]);
    assert_eq!(inbox["unread_count"], json!(0), "read consumed it: {inbox}");
}

#[test]
fn origin_routing_names_the_recipient_of_a_colliding_import_on_reroute() {
    let rig = Rig::new();
    rig.deliver_colliding(Topology::Removed);
    let receipt_path = rig
        .root()
        .join("participants")
        .join(&rig.recipient)
        .join("routing")
        .join(format!("{MAIL_ID}.json"));
    let receipt: Value =
        serde_json::from_slice(&fs::read(&receipt_path).expect("receipt")).expect("receipt json");
    assert_eq!(receipt["recipients"], json!([rig.recipient]));
    // Route it again from pending, with only the record left as evidence.
    fs::remove_file(&receipt_path).expect("drop receipt");
    let pending = rig.reader_json(&["inbox", "--json"]);
    assert_eq!(pending["pending"], json!(1), "{pending}");
    // A consuming read routes pending mail first.
    let read = rig.reader_json(&["read", MAIL_ID, "--json"]);
    assert_ne!(read.get("own"), Some(&json!(true)), "{read}");
    assert_ne!(read.get("pending"), Some(&json!(true)), "{read}");
    assert_admitted_remote(&read["envelope"], &rig.admitted_reply());
    let receipt: Value =
        serde_json::from_slice(&fs::read(&receipt_path).expect("re-routed receipt"))
            .expect("receipt json");
    assert_eq!(receipt["recipients"], json!([rig.recipient]));
}

#[test]
fn origin_watch_snapshot_reports_a_colliding_import_as_remote() {
    let rig = Rig::new();
    rig.deliver_colliding(Topology::Removed);
    let events = stdout(&rig.reader(&["watch", "--snapshot"]));
    let event = events
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("event json"))
        .find(|event| event["id"] == json!(MAIL_ID))
        .unwrap_or_else(|| panic!("watch hid the colliding import: {events}"));
    assert_admitted_remote(&event, &rig.admitted_reply());
}

#[test]
fn origin_search_and_catchup_keep_a_colliding_import_remote() {
    let rig = Rig::new();
    rig.deliver_colliding(Topology::Removed);
    let search = rig.reader_json(&["search", "hello", "--mail", "--json"]);
    let hit = search["results"]
        .as_array()
        .expect("results")
        .iter()
        .find(|hit| hit["id"] == json!(MAIL_ID))
        .unwrap_or_else(|| panic!("search hid the colliding import: {search}"))
        .clone();
    assert_admitted_remote(&hit, &rig.admitted_reply());
    let catchup = rig.reader_json(&["catchup", "--json"]);
    let envelope = catchup["targets"]
        .as_array()
        .expect("targets")
        .iter()
        .flat_map(|target| target["messages"].as_array().cloned().unwrap_or_default())
        .map(|message| message["envelope"].clone())
        .find(|envelope| envelope["id"] == json!(MAIL_ID))
        .unwrap_or_else(|| panic!("catchup hid the colliding import: {catchup}"));
    assert_admitted_remote(&envelope, &rig.admitted_reply());
}

#[test]
fn origin_keeps_the_admitted_host_after_the_placeholder_is_removed_or_rehomed() {
    for topology in [Topology::RehomedRemote, Topology::RehomedLocal] {
        let rig = Rig::new();
        rig.deliver_colliding(topology);
        let inbox = rig.reader_json(&["inbox", "--json"]);
        let item = unread_item(&inbox);
        assert_eq!(item["origin"], json!("remote"), "{topology:?}: {item}");
        assert_eq!(
            item["reply_to_participant"],
            json!(rig.admitted_reply()),
            "{topology:?}: {item}"
        );
    }
}

#[test]
fn origin_with_an_unreadable_or_mismatched_record_is_unknown_and_never_own() {
    for corrupt in ["garbage", "digest"] {
        let rig = Rig::new();
        rig.deliver_colliding(Topology::RehomedLocal);
        let record = rig.record_path();
        match corrupt {
            "garbage" => fs::write(&record, b"{not json").expect("corrupt record"),
            _ => {
                let mut value: Value =
                    serde_json::from_slice(&fs::read(&record).expect("record")).expect("json");
                value["sha256"] = json!("0".repeat(64));
                fs::write(&record, value.to_string()).expect("mismatched record");
            }
        }
        let inbox = rig.reader_json(&["inbox", "--json"]);
        let item = unread_item(&inbox);
        assert_eq!(item["origin"], json!("unknown"), "{corrupt}: {item}");
        assert!(
            item.get("reply_to_participant").is_none_or(Value::is_null),
            "{corrupt}: no participant reply without a valid record: {item}"
        );
        let text = stdout(&rig.reader(&["read", MAIL_ID, "--peek"]));
        assert!(!text.contains("own: true"), "{corrupt}: {text}");
        assert!(!text.contains("reply=participant:"), "{corrupt}: {text}");
    }
}
