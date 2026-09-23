//! Host-qualified participant addresses and the send side of a participant
//! DM across hosts (design rev 3.1, "The address" and "The sender side").
//!
//! Every test runs against a temporary store. A refused send must leave the
//! store byte-identical; an accepted one adds exactly `archive/<id>.mail`.

mod common;

use common::{stderr, stdout, tree_snapshot, Sandbox};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const OWN_HOST: &str = "here";
const PEER: &str = "peer";
const OTHER: &str = "other";
const REMOTE_ID: &str = "codex-far00001";

struct Rig {
    sandbox: Sandbox,
    sender: String,
}

impl Rig {
    /// A bridged host `here` with enrolled peers `peer` and `other`, a fresh
    /// F3 health file, and a sender bound to the local room `claude-space`.
    fn new() -> Self {
        let sandbox = Sandbox::new();
        common::create_default_room_paths(&sandbox);
        let rig = Self {
            sender: sandbox.test_participant("claude-space"),
            sandbox,
        };
        rig.write_bridge("config.json", &json!({"host": OWN_HOST}).to_string());
        rig.write_registry(&json!({"v": 1, "hosts": [OWN_HOST, PEER, OTHER]}).to_string());
        rig.write_health(
            &["typed-outbound-exclusion", "participant-mail-v1"],
            SystemTime::now(),
            30,
        );
        rig
    }

    fn root(&self) -> &Path {
        &self.sandbox.mail_root
    }

    fn bridge(&self) -> PathBuf {
        self.root().join("bridge")
    }

    fn write_bridge(&self, relative: &str, contents: &str) {
        let path = self.bridge().join(relative);
        fs::create_dir_all(path.parent().expect("parent")).expect("bridge dir");
        fs::write(path, contents).expect("bridge file");
    }

    fn write_registry(&self, contents: &str) {
        self.write_bridge("registry/hosts.json", contents);
    }

    fn write_health(&self, capabilities: &[&str], ticked: SystemTime, interval_s: u64) {
        self.write_bridge(
            "health.json",
            &json!({
                "v": 1,
                "capabilities": capabilities,
                "ticked_at": rfc3339(ticked),
                "interval_s": interval_s,
                "published_without_receipt": 0
            })
            .to_string(),
        );
    }

    fn cwd(&self) -> PathBuf {
        self.sandbox.home.join("claude-space")
    }

    fn send_as(&self, participant: &str, to: &str, json_output: bool) -> Output {
        let mut args = vec!["send", "--to", to, "--body", "hello across hosts"];
        if json_output {
            args.push("--json");
        }
        self.sandbox
            .run_as_participant(&args, participant, &self.cwd())
    }

    fn send(&self, to: &str) -> Output {
        self.send_as(&self.sender, to, true)
    }

    /// The store minus lock files and participant records (a send refreshes
    /// the actor's `last_seen`, which is not a mail write).
    fn snapshot(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut tree = tree_snapshot(self.root());
        tree.retain(|path, _| {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            !name.ends_with(".lock") && name != "participant.json"
        });
        tree
    }

    fn archive_file(&self, id: &str) -> PathBuf {
        self.root().join("archive").join(format!("{id}.mail"))
    }
}

/// `YYYY-MM-DDTHH:MM:SS+00:00`, the bridge's stamp format.
fn rfc3339(at: SystemTime) -> String {
    let seconds = at
        .duration_since(UNIX_EPOCH)
        .expect("after epoch")
        .as_secs() as i64;
    let (days, rem) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}+00:00",
        rem / 3600,
        (rem / 60) % 60,
        rem % 60
    )
}

/// A refusal: the exact code, exit, and retryable flag, on stderr as JSON.
fn assert_refused(output: &Output, code: &str, exit: i32, retryable: bool) -> Value {
    assert_eq!(
        output.status.code(),
        Some(exit),
        "{code}\nstdout: {}\nstderr: {}",
        stdout(output),
        stderr(output)
    );
    assert!(stdout(output).is_empty(), "no stdout on refusal");
    let line = stderr(output)
        .lines()
        .rev()
        .find(|line| line.starts_with('{'))
        .map(str::to_owned)
        .unwrap_or_else(|| panic!("no JSON error: {}", stderr(output)));
    let value: Value = serde_json::from_str(&line).expect("error JSON");
    assert_eq!(value["error"]["code"], json!(code), "{value}");
    assert_eq!(value["error"]["retryable"], json!(retryable), "{value}");
    value
}

/// Run `send` and assert it was refused with nothing written.
fn assert_refused_unchanged(rig: &Rig, to: &str, code: &str, exit: i32, retryable: bool) -> Value {
    let before = rig.snapshot();
    let value = assert_refused(&rig.send(to), code, exit, retryable);
    assert_eq!(rig.snapshot(), before, "{to}: a refusal writes nothing");
    value
}

/// Run `send` and assert it queued: exactly `archive/<id>.mail` is new,
/// nothing else changed, and the receipt says queued. Returns the receipt.
fn assert_queued(rig: &Rig, to: &str, id: &str, host: &str) -> Value {
    let before = rig.snapshot();
    let output = rig.send(to);
    assert!(
        output.status.success(),
        "{to}\nstdout: {}\nstderr: {}",
        stdout(&output),
        stderr(&output)
    );
    let receipt: Value = serde_json::from_str(&stdout(&output)).expect("send JSON");
    assert_eq!(
        receipt["delivery"],
        json!({"state": "queued", "host": host})
    );
    let envelope = &receipt["envelope"];
    assert_eq!(envelope["to"], json!(id));
    assert_eq!(envelope["to_host"], json!(host));
    assert_eq!(envelope["address_kind"], json!("participant"));
    assert_eq!(envelope["from"], json!("claude-space"));
    assert_eq!(envelope["from_participant"], json!(rig.sender));
    let mail_id = envelope["id"].as_str().expect("id");
    let mut after = rig.snapshot();
    let added = after
        .remove(&rig.archive_file(mail_id))
        .expect("the letter is in archive/");
    let text = String::from_utf8(added).expect("utf8 letter");
    assert!(text.contains(&format!("\"to_host\": \"{host}\"")), "{text}");
    assert_eq!(after, before, "{to}: only archive/{mail_id}.mail is new");
    receipt
}

// ---------------------------------------------------------------------------
// Address resolution

#[test]
fn an_exact_local_id_containing_at_stays_local() {
    let rig = Rig::new();
    let legacy = "legacy@peer";
    let dir = rig.root().join("participants").join(legacy);
    fs::create_dir_all(&dir).expect("legacy dir");
    fs::write(
        dir.join("participant.json"),
        json!({
            "version": 1,
            "id": legacy,
            "harness": "test",
            "conversation_key_digest": "0".repeat(64),
            "created": "2026-01-01 00:00:00 +0000",
            "last_seen": "2099-01-01T00:00:00Z",
            "lease_hours": 24,
            "workspace": "pact",
            "workspace_path": null,
            "lineage": null,
            "lineage_since": null
        })
        .to_string(),
    )
    .expect("legacy record");
    let output = rig.send(&format!("participant:{legacy}"));
    assert!(output.status.success(), "{}", stderr(&output));
    let receipt: Value = serde_json::from_str(&stdout(&output)).expect("send JSON");
    assert!(receipt.get("delivery").is_none(), "{receipt}");
    assert!(receipt["envelope"].get("to_host").is_none(), "{receipt}");
    let id = receipt["envelope"]["id"].as_str().expect("id");
    assert!(dir.join("inbox").join(format!("{id}.mail")).is_file());
}

#[test]
fn a_host_qualified_address_splits_at_the_last_at_and_checks_the_host_grammar() {
    let rig = Rig::new();
    assert_queued(
        &rig,
        &format!("participant:{REMOTE_ID}@{PEER}"),
        REMOTE_ID,
        PEER,
    );
    for bad in [
        format!("participant:a@b@{PEER}"),
        format!("participant:@{PEER}"),
        format!("participant:{REMOTE_ID}@Peer"),
        format!("participant:{REMOTE_ID}@"),
        format!("participant:{REMOTE_ID}@{}", "x".repeat(33)),
        format!("participant:{REMOTE_ID}@pe_er"),
    ] {
        assert_refused_unchanged(&rig, &bad, "invalid_argument", 2, false);
    }
}

#[test]
fn the_own_host_resolves_locally_unchanged() {
    let rig = Rig::new();
    let recipient = rig.sandbox.test_participant("pact");
    let output = rig.send(&format!("participant:{recipient}@{OWN_HOST}"));
    assert!(output.status.success(), "{}", stderr(&output));
    let receipt: Value = serde_json::from_str(&stdout(&output)).expect("send JSON");
    assert!(receipt.get("delivery").is_none(), "{receipt}");
    let envelope = &receipt["envelope"];
    assert_eq!(envelope["to"], json!(recipient));
    assert!(envelope.get("to_host").is_none(), "{envelope}");
    let id = envelope["id"].as_str().expect("id");
    let inbox = rig
        .root()
        .join("participants")
        .join(&recipient)
        .join("inbox")
        .join(format!("{id}.mail"));
    assert!(inbox.is_file(), "own host delivers to the local inbox");
    // An unknown id on the own host is the local resolver's answer, not a
    // workspace of the same name.
    assert_refused_unchanged(
        &rig,
        &format!("participant:pact@{OWN_HOST}"),
        "not_found",
        66,
        false,
    );
}

#[test]
fn an_invalid_registry_is_topology_unavailable_with_no_archive_write() {
    let rig = Rig::new();
    let to = format!("participant:{REMOTE_ID}@{PEER}");
    for (label, contents) in [
        ("garbage", Some("{not json".to_owned())),
        (
            "extra key",
            Some(json!({"v": 1, "hosts": [PEER], "x": 1}).to_string()),
        ),
        (
            "wrong version",
            Some(json!({"v": 2, "hosts": [PEER]}).to_string()),
        ),
        (
            "bad host",
            Some(json!({"v": 1, "hosts": ["Bad Host"]}).to_string()),
        ),
        (
            "duplicate",
            Some(json!({"v": 1, "hosts": [PEER, PEER]}).to_string()),
        ),
        (
            "hosts not array",
            Some(json!({"v": 1, "hosts": PEER}).to_string()),
        ),
        (
            "over 64 hosts",
            Some(
                json!({"v": 1, "hosts": (0..65).map(|n| format!("h{n}")).collect::<Vec<_>>()})
                    .to_string(),
            ),
        ),
        ("missing", None),
    ] {
        let path = rig.bridge().join("registry/hosts.json");
        match contents {
            Some(contents) => fs::write(&path, contents).expect("registry"),
            None => fs::remove_file(&path).expect("drop registry"),
        }
        let value = assert_refused_unchanged(&rig, &to, "topology_unavailable", 75, true);
        assert!(
            value["error"]["message"]
                .as_str()
                .unwrap_or("")
                .contains("hosts.json"),
            "{label}: {value}"
        );
    }
    // No fallback to config peers when the registry is gone.
    rig.write_bridge(
        "config.json",
        &json!({"host": OWN_HOST, "peers": {PEER: {}}}).to_string(),
    );
    assert_refused_unchanged(&rig, &to, "topology_unavailable", 75, true);
    // An unreadable bridge config is the same retryable answer.
    rig.write_registry(&json!({"v": 1, "hosts": [OWN_HOST, PEER]}).to_string());
    rig.write_bridge("config.json", "{\"host\": 7}");
    assert_refused_unchanged(&rig, &to, "topology_unavailable", 75, true);
}

#[test]
fn an_unknown_host_and_a_missing_bridge_are_refused() {
    let rig = Rig::new();
    let value = assert_refused_unchanged(
        &rig,
        &format!("participant:{REMOTE_ID}@nowhere"),
        "unknown_host",
        65,
        false,
    );
    assert_eq!(value["error"]["details"]["matches"], json!([OTHER, PEER]));
    fs::remove_file(rig.bridge().join("config.json")).expect("drop config");
    assert_refused_unchanged(
        &rig,
        &format!("participant:{REMOTE_ID}@{PEER}"),
        "no_bridge",
        78,
        false,
    );
}

#[test]
fn a_peer_restriction_excludes_a_registered_host() {
    let rig = Rig::new();
    rig.write_bridge(
        "config.json",
        &json!({"host": OWN_HOST, "peers": {PEER: {"relay": "x"}}}).to_string(),
    );
    let value = assert_refused_unchanged(
        &rig,
        &format!("participant:{REMOTE_ID}@{OTHER}"),
        "unknown_host",
        65,
        false,
    );
    assert_eq!(value["error"]["details"]["matches"], json!([PEER]));
    assert_queued(
        &rig,
        &format!("participant:{REMOTE_ID}@{PEER}"),
        REMOTE_ID,
        PEER,
    );
}

#[test]
fn at_is_refused_for_new_ids_while_existing_records_still_load() {
    let rig = Rig::new();
    let before = rig.snapshot();
    let bind = rig.sandbox.run_without_identity(
        &[
            "participant",
            "bind",
            "--harness",
            "ev@il",
            "--key",
            "k1",
            "--json",
        ],
        &rig.cwd(),
    );
    assert_eq!(bind.status.code(), Some(2), "{}", stderr(&bind));
    assert!(
        stderr(&bind).contains("invalid_argument"),
        "{}",
        stderr(&bind)
    );
    assert_eq!(rig.snapshot(), before, "no record minted");
    let ids: Vec<String> = fs::read_dir(rig.root().join("participants"))
        .expect("participants")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert!(ids.iter().all(|id| !id.contains('@')), "{ids:?}");
    // A remote-address segment containing '@' is refused too.
    assert_refused_unchanged(
        &rig,
        &format!("participant:ev@il@{PEER}"),
        "invalid_argument",
        2,
        false,
    );
    // An existing record with '@' still loads (see
    // an_exact_local_id_containing_at_stays_local for the send path).
    let dir = rig.root().join("participants").join("old@id");
    fs::create_dir_all(&dir).expect("dir");
    let mut record = rig.sandbox.read_participant(&rig.sender);
    record["id"] = json!("old@id");
    fs::write(dir.join("participant.json"), record.to_string()).expect("record");
    let shown =
        rig.sandbox
            .run_as_participant(&["participant", "show", "--json"], "old@id", &rig.cwd());
    assert!(shown.status.success(), "{}", stderr(&shown));
    assert!(stdout(&shown).contains("old@id"), "{}", stdout(&shown));
}

// ---------------------------------------------------------------------------
// Send

#[test]
fn a_remote_send_writes_only_the_archive_and_reports_queued_in_json_and_text() {
    let rig = Rig::new();
    let to = format!("participant:{REMOTE_ID}@{PEER}");
    let receipt = assert_queued(&rig, &to, REMOTE_ID, PEER);
    assert_eq!(receipt["archived"], json!(true));
    let before = rig.snapshot();
    let text = rig.send_as(&rig.sender, &to, false);
    assert!(text.status.success(), "{}", stderr(&text));
    let text = stdout(&text);
    assert!(
        text.contains(&format!("queued for {PEER}; not yet delivered.")),
        "{text}"
    );
    assert!(!text.to_lowercase().contains("delivered to"), "{text}");
    assert_eq!(
        rig.snapshot().len(),
        before.len() + 1,
        "one more archive file"
    );
}

#[test]
fn a_host_qualified_address_never_falls_back_to_a_workspace() {
    let rig = Rig::new();
    let pact_inbox = rig.sandbox.home.join("pact").join("inbox");
    // `pact` is a local room. An unenrolled host is refused, never the room.
    assert_refused_unchanged(&rig, "participant:pact@nowhere", "unknown_host", 65, false);
    // A broken topology is refused, never the room.
    fs::remove_file(rig.bridge().join("registry/hosts.json")).expect("drop registry");
    assert_refused_unchanged(
        &rig,
        "participant:pact@peer",
        "topology_unavailable",
        75,
        true,
    );
    rig.write_registry(&json!({"v": 1, "hosts": [OWN_HOST, PEER]}).to_string());
    // A valid remote send to an id that names a local room leaves the room
    // untouched: assert_queued proves only archive/ changed.
    assert_queued(&rig, "participant:pact@peer", "pact", PEER);
    assert!(
        !pact_inbox.exists() || fs::read_dir(&pact_inbox).expect("inbox").next().is_none(),
        "the pact room inbox stays empty"
    );
}

#[test]
fn typed_letters_never_enter_outbox_or_a_workspace_inbox() {
    let rig = Rig::new();
    let before = tree_snapshot(rig.root());
    for id in ["pact", "claude-space", REMOTE_ID] {
        let output = rig.send(&format!("participant:{id}@{PEER}"));
        assert!(output.status.success(), "{}", stderr(&output));
    }
    let after = tree_snapshot(rig.root());
    for path in after.keys().filter(|path| !before.contains_key(*path)) {
        let relative = path.strip_prefix(rig.root()).expect("under root");
        let first = relative.components().next().expect("component");
        assert!(
            first.as_os_str() == "archive"
                || path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(".lock")),
            "only archive/ may gain a file, got {}",
            relative.display()
        );
    }
    assert!(
        !rig.root().join("outbox").exists(),
        "post never writes outbox/"
    );
    for room in ["pact", "claude-space", "agent-memory"] {
        let inbox = rig.sandbox.home.join(room).join("inbox");
        let count = fs::read_dir(&inbox)
            .map(|entries| entries.count())
            .unwrap_or(0);
        assert_eq!(count, 0, "{room} inbox gained mail");
    }
}

#[test]
fn a_sender_without_a_local_room_is_remote_sender_unroutable() {
    let rig = Rig::new();
    let to = format!("participant:{REMOTE_ID}@{PEER}");
    let solo = rig.sandbox.seed_session_only_participant();
    let before = rig.snapshot();
    let output = rig.send_as(&solo, &to, true);
    let value = assert_refused(&output, "remote_sender_unroutable", 65, false);
    assert!(
        value["error"]["suggested_fix"]
            .as_str()
            .unwrap_or("")
            .contains("post participant bind --workspace"),
        "{value}"
    );
    assert_eq!(rig.snapshot(), before);
    // A sender whose room is now a remote placeholder is refused the same way.
    let mut rooms: serde_json::Map<String, Value> =
        serde_json::from_slice(&fs::read(rig.root().join("rooms.json")).expect("rooms"))
            .expect("rooms json");
    let placeholder = rig.root().join("remote").join(OTHER).join("claude-space");
    fs::create_dir_all(&placeholder).expect("placeholder");
    rooms.insert("claude-space".into(), json!(placeholder));
    fs::write(
        rig.root().join("rooms.json"),
        Value::Object(rooms).to_string(),
    )
    .expect("rooms");
    assert_refused_unchanged(&rig, &to, "remote_sender_unroutable", 65, false);
}

#[test]
fn the_capability_guard_refuses_before_anything_is_written() {
    let rig = Rig::new();
    let to = format!("participant:{REMOTE_ID}@{PEER}");
    let now = SystemTime::now();
    for (label, capabilities) in [
        ("no exclusion", vec!["participant-mail-v1"]),
        ("no participant mail", vec!["typed-outbound-exclusion"]),
        ("neither", vec![]),
    ] {
        rig.write_health(&capabilities, now, 30);
        let value = assert_refused_unchanged(&rig, &to, "bridge_unsupported", 69, false);
        assert!(value["error"]["message"].is_string(), "{label}");
    }
    let both = ["typed-outbound-exclusion", "participant-mail-v1"];
    let stale = now - Duration::from_secs(91);
    rig.write_health(&both, stale, 30);
    assert_refused_unchanged(&rig, &to, "bridge_status_unavailable", 75, true);
    let future = now + Duration::from_secs(3600);
    rig.write_health(&both, future, 30);
    assert_refused_unchanged(&rig, &to, "bridge_status_unavailable", 75, true);
    // A future stamp gets at most 5 s of clock skew, not a second window.
    rig.write_health(&both, now + Duration::from_secs(90), 30);
    assert_refused_unchanged(&rig, &to, "bridge_status_unavailable", 75, true);
    // The boundary stamps come from a fresh clock read: `now` is seconds old
    // by here, and rfc3339 drops subseconds, so a stale base would drift a
    // stamp across the 5 s allowance. 7 s ahead stays outside it after both.
    rig.write_health(&both, SystemTime::now() + Duration::from_secs(7), 30);
    assert_refused_unchanged(&rig, &to, "bridge_status_unavailable", 75, true);
    for broken in [
        "{not json".to_owned(),
        json!({"capabilities": both, "interval_s": 30}).to_string(),
        json!({"capabilities": both, "ticked_at": rfc3339(now)}).to_string(),
        json!({"capabilities": both, "ticked_at": rfc3339(now), "interval_s": 0}).to_string(),
        json!({"capabilities": "typed-outbound-exclusion", "ticked_at": rfc3339(now), "interval_s": 30}).to_string(),
        json!({"capabilities": both, "ticked_at": "yesterday", "interval_s": 30}).to_string(),
    ] {
        rig.write_bridge("health.json", &broken);
        assert_refused_unchanged(&rig, &to, "bridge_status_unavailable", 75, true);
    }
    fs::remove_file(rig.bridge().join("health.json")).expect("drop health");
    assert_refused_unchanged(&rig, &to, "bridge_status_unavailable", 75, true);
    // Fresh within three intervals, with both capabilities: queued.
    rig.write_health(&both, SystemTime::now() - Duration::from_secs(80), 30);
    assert_queued(&rig, &to, REMOTE_ID, PEER);
    // A stamp 4 s ahead is inside the 5 s allowance even after truncation, so
    // together with the 7 s refusal this pins the allowance between 4 and 6 s.
    rig.write_health(&both, SystemTime::now() + Duration::from_secs(4), 30);
    assert_queued(&rig, &to, REMOTE_ID, PEER);
}

#[test]
fn a_remote_letter_larger_than_the_bridge_carries_is_refused_before_writing() {
    // The bridge caps a letter at 1 MiB (BRIDGE_MAX_MAIL_BYTES, default and
    // ceiling). A bigger one would sit queued forever, so --oversize cannot
    // queue it; below the cap, --oversize still works.
    let rig = Rig::new();
    let to = format!("participant:{REMOTE_ID}@{PEER}");
    let send = |bytes: usize| {
        let body = rig.sandbox.path.join(format!("body-{bytes}.txt"));
        fs::write(&body, "x".repeat(bytes)).expect("body file");
        let body = body.to_string_lossy().into_owned();
        rig.sandbox.run_as_participant(
            &[
                "send",
                "--to",
                &to,
                "--body-file",
                &body,
                "--oversize",
                "--json",
            ],
            &rig.sender,
            &rig.cwd(),
        )
    };
    // The snapshot records files only; check the archive directory itself.
    let archive = rig.root().join("archive");
    if archive.exists() {
        fs::remove_dir(&archive).expect("the rig's archive directory is empty");
    }
    let before = rig.snapshot();
    let value = assert_refused(&send(1024 * 1024), "invalid_argument", 2, false);
    assert!(
        !archive.exists(),
        "an undeliverable letter created the archive directory"
    );
    assert!(
        value["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("1048576")),
        "{value}"
    );
    assert_eq!(
        rig.snapshot(),
        before,
        "an undeliverable letter writes nothing"
    );
    let output = send(64 * 1024);
    assert!(output.status.success(), "{}", stderr(&output));
    let receipt: Value = serde_json::from_str(&stdout(&output)).expect("send JSON");
    assert_eq!(receipt["delivery"]["state"], json!("queued"));
}

#[test]
fn a_local_wildcard_rule_blocks_a_remote_send_with_nothing_written() {
    let rig = Rig::new();
    let rules_path = rig.root().join("rules.json");
    let mut rules: Value =
        serde_json::from_slice(&fs::read(&rules_path).expect("rules")).expect("rules json");
    rules["blocked"]
        .as_array_mut()
        .expect("blocked array")
        .push(json!({"from": "claude-space", "to": "*", "reason": "quiet hours"}));
    fs::write(&rules_path, rules.to_string()).expect("rules");
    assert_refused_unchanged(
        &rig,
        &format!("participant:{REMOTE_ID}@{PEER}"),
        "blocked_route",
        77,
        false,
    );
}

// ---------------------------------------------------------------------------
// post delivery

impl Rig {
    /// Queue one remote letter; returns (mail id, archive sha256).
    fn queue(&self) -> (String, String) {
        let receipt = assert_queued(
            self,
            &format!("participant:{REMOTE_ID}@{PEER}"),
            REMOTE_ID,
            PEER,
        );
        let id = receipt["envelope"]["id"].as_str().expect("id").to_owned();
        let sha = sha256_hex(&fs::read(self.archive_file(&id)).expect("archive"));
        (id, sha)
    }

    fn delivery_as(&self, participant: &str, id: &str, json_output: bool) -> Output {
        let mut args = vec!["delivery", id];
        if json_output {
            args.push("--json");
        }
        self.sandbox
            .run_as_participant(&args, participant, &self.cwd())
    }

    /// `post delivery --json` for the sender; asserts success and the schema.
    fn delivery(&self, id: &str) -> Value {
        let output = self.delivery_as(&self.sender, id, true);
        assert!(output.status.success(), "{}", stderr(&output));
        let value: Value = serde_json::from_str(&stdout(&output)).expect("delivery JSON");
        assert_eq!(value["schema"], json!("post.delivery.v1"));
        assert_eq!(value["id"], json!(id));
        value
    }

    fn evidence(&self, dir: &str, id: &str) -> PathBuf {
        self.bridge().join(dir).join(format!("{id}.json"))
    }

    fn write_evidence(&self, dir: &str, id: &str, contents: &str) {
        let path = self.evidence(dir, id);
        fs::create_dir_all(path.parent().expect("parent")).expect("evidence dir");
        fs::write(path, contents).expect("evidence");
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

fn receipt(id: &str, sha: &str, status: &str, reason: Option<&str>) -> Value {
    json!({
        "v": 1,
        "status": status,
        "origin": OWN_HOST,
        "host": PEER,
        "participant": REMOTE_ID,
        "id": id,
        "sha256": sha,
        "reason": reason,
        "at": "2026-09-23T05:00:00Z"
    })
}

fn marker(id: &str, sha: &str) -> Value {
    json!({"v": 1, "id": id, "host": PEER, "sha256": sha, "commit": "abc123", "at": "2026-09-23T04:00:00+00:00"})
}

#[test]
fn delivery_reports_each_state_from_its_evidence() {
    let rig = Rig::new();
    let (id, sha) = rig.queue();
    let queued = rig.delivery(&id);
    assert_eq!(queued["state"], json!("queued"));
    assert_eq!(queued["participant"], json!(REMOTE_ID));
    assert_eq!(queued["host"], json!(PEER));
    assert_eq!(queued["sha256"], json!(sha));
    assert_eq!(queued["conflict"], json!(false));
    rig.write_evidence(
        "pmail-status",
        &id,
        &json!({"v": 1, "id": id, "blocked_reason": "peer_not_effective", "last_error": "relay push failed", "at": "2026-09-23T04:00:00Z", "extra": 1}).to_string(),
    );
    let blocked = rig.delivery(&id);
    assert_eq!(blocked["state"], json!("queued"));
    assert_eq!(blocked["blocked_reason"], json!("peer_not_effective"));
    assert_eq!(blocked["last_error"], json!("relay push failed"));
    rig.write_evidence("pmail-published", &id, &marker(&id, &sha).to_string());
    let published = rig.delivery(&id);
    assert_eq!(published["state"], json!("published"));
    assert_eq!(published["commit"], json!("abc123"));
    assert!(published["age_s"].as_u64().is_some(), "{published}");
    assert!(published.get("blocked_reason").is_none(), "{published}");
    rig.write_evidence(
        "pmail-acked",
        &id,
        &receipt(&id, &sha, "delivered", None).to_string(),
    );
    let received = rig.delivery(&id);
    assert_eq!(received["state"], json!("received"));
    assert_eq!(received["acked_at"], json!("2026-09-23T05:00:00Z"));
    assert!(received.get("reason").is_none(), "{received}");
    // A receipt that outran the marker still reaches its terminal state.
    fs::remove_file(rig.evidence("pmail-published", &id)).expect("drop marker");
    assert_eq!(rig.delivery(&id)["state"], json!("received"));
    rig.write_evidence(
        "pmail-acked",
        &id,
        &receipt(&id, &sha, "rejected", Some("blocked_route")).to_string(),
    );
    let rejected = rig.delivery(&id);
    assert_eq!(rejected["state"], json!("rejected"));
    assert_eq!(rejected["reason"], json!("blocked_route"));
    // The text form names the state and the address.
    let text = stdout(&rig.delivery_as(&rig.sender, &id, false));
    assert!(
        text.contains(&format!("{id}: rejected (participant:{REMOTE_ID}@{PEER})")),
        "{text}"
    );
    assert!(text.contains("reason: blocked_route"), "{text}");
}

#[test]
fn delivery_accepts_the_destination_bridges_own_rejection_reasons() {
    // The destination bridge refuses name_collision and unpublished_sender
    // from its own sender binding before `post bridge deliver` runs, so they
    // reach the sender only through the acked receipt.
    let rig = Rig::new();
    let (id, sha) = rig.queue();
    for reason in ["name_collision", "unpublished_sender"] {
        rig.write_evidence(
            "pmail-acked",
            &id,
            &receipt(&id, &sha, "rejected", Some(reason)).to_string(),
        );
        let rejected = rig.delivery(&id);
        assert_eq!(rejected["state"], json!("rejected"), "{reason}: {rejected}");
        assert_eq!(rejected["reason"], json!(reason), "{rejected}");
    }
}

#[test]
fn delivery_reports_corrupt_evidence_as_unknown_never_a_guess() {
    let rig = Rig::new();
    let (id, sha) = rig.queue();
    let other_sha = "0".repeat(64);
    let mut extra = receipt(&id, &sha, "delivered", None);
    extra["note"] = json!("x");
    let mut missing = receipt(&id, &sha, "delivered", None);
    missing.as_object_mut().expect("object").remove("at");
    let cases: Vec<(&str, String)> = vec![
        ("pmail-acked", "{not json".to_owned()),
        ("pmail-acked", extra.to_string()),
        ("pmail-acked", missing.to_string()),
        (
            "pmail-acked",
            receipt(&id, &other_sha, "delivered", None).to_string(),
        ),
        ("pmail-acked", {
            let mut value = receipt(&id, &sha, "delivered", None);
            value["origin"] = json!(OTHER);
            value.to_string()
        }),
        ("pmail-acked", {
            let mut value = receipt(&id, &sha, "delivered", None);
            value["host"] = json!(OTHER);
            value.to_string()
        }),
        ("pmail-acked", {
            let mut value = receipt(&id, &sha, "delivered", None);
            value["participant"] = json!("someone-else");
            value.to_string()
        }),
        (
            "pmail-acked",
            receipt(&id, &sha, "rejected", Some("flaky_network")).to_string(),
        ),
        (
            "pmail-acked",
            receipt(&id, &sha, "rejected", None).to_string(),
        ),
        (
            "pmail-acked",
            receipt(&id, &sha, "delivered", Some("blocked_route")).to_string(),
        ),
        ("pmail-acked", receipt(&id, &sha, "maybe", None).to_string()),
        ("pmail-published", "[]".to_owned()),
        ("pmail-published", {
            let mut value = marker(&id, &other_sha);
            value["sha256"] = json!(other_sha);
            value.to_string()
        }),
        ("pmail-published", {
            let mut value = marker(&id, &sha);
            value["commit"] = json!("");
            value.to_string()
        }),
        ("pmail-published", {
            let mut value = marker(&id, &sha);
            value["host"] = json!(OTHER);
            value.to_string()
        }),
        ("pmail-published", {
            // A marker from the future is not a fresh one.
            let mut value = marker(&id, &sha);
            value["at"] = json!("2099-01-01T00:00:00+00:00");
            value.to_string()
        }),
        ("pmail-status", "{not json".to_owned()),
        ("pmail-status", json!({"v": 2, "id": id}).to_string()),
        (
            "pmail-status",
            json!({"v": 1, "id": id, "blocked_reason": 7}).to_string(),
        ),
    ];
    for (dir, contents) in cases {
        for clean in ["pmail-acked", "pmail-published", "pmail-status"] {
            let _ = fs::remove_file(rig.evidence(clean, &id));
        }
        rig.write_evidence(dir, &id, &contents);
        let value = rig.delivery(&id);
        assert_eq!(
            value["state"],
            json!("unknown"),
            "{dir} {contents}: {value}"
        );
        assert!(
            value["evidence_file"]
                .as_str()
                .is_some_and(|file| file.ends_with(&format!("{dir}/{id}.json"))),
            "{dir}: {value}"
        );
        assert!(value["evidence_error"]
            .as_str()
            .is_some_and(|error| !error.is_empty()));
    }
    // A receipt whose origin cannot be checked (no bridge config) is unknown.
    for clean in ["pmail-acked", "pmail-published", "pmail-status"] {
        let _ = fs::remove_file(rig.evidence(clean, &id));
    }
    rig.write_evidence(
        "pmail-acked",
        &id,
        &receipt(&id, &sha, "delivered", None).to_string(),
    );
    fs::remove_file(rig.bridge().join("config.json")).expect("drop config");
    assert_eq!(rig.delivery(&id)["state"], json!("unknown"));
}

#[test]
fn delivery_of_an_unknown_or_invalid_id_is_refused() {
    let rig = Rig::new();
    let missing = rig.delivery_as(&rig.sender, "20260923-000000-abcdef", true);
    assert_refused(&missing, "not_found", 66, false);
    let invalid = rig.delivery_as(&rig.sender, "../etc", true);
    assert_refused(&invalid, "invalid_argument", 2, false);
}

#[test]
fn delivery_of_workspace_mail_is_unsupported() {
    let rig = Rig::new();
    let output = rig.send("pact");
    assert!(output.status.success(), "{}", stderr(&output));
    let sent: Value = serde_json::from_str(&stdout(&output)).expect("send JSON");
    let id = sent["envelope"]["id"].as_str().expect("id");
    let value = rig.delivery(id);
    assert_eq!(value["state"], json!("unsupported"));
    assert!(value.get("host").is_none(), "{value}");
}

#[test]
fn delivery_is_visible_only_to_the_sender_and_writes_nothing() {
    let rig = Rig::new();
    let (id, _) = rig.queue();
    let other = rig.sandbox.test_participant("pact");
    assert_ne!(other, rig.sender);
    let before = tree_snapshot(rig.root());
    let refused = rig.delivery_as(&other, &id, true);
    assert_refused(&refused, "not_found", 66, false);
    assert_eq!(rig.delivery(&id)["state"], json!("queued"));
    let mut after = tree_snapshot(rig.root());
    after.retain(|path, _| !path.to_string_lossy().ends_with(".lock"));
    let mut before = before;
    before.retain(|path, _| !path.to_string_lossy().ends_with(".lock"));
    assert_eq!(after, before, "post delivery is read-only");
}

#[test]
fn delivery_reports_a_receipt_conflict() {
    let rig = Rig::new();
    let (id, sha) = rig.queue();
    rig.write_evidence(
        "pmail-acked",
        &id,
        &receipt(&id, &sha, "delivered", None).to_string(),
    );
    rig.write_evidence("pmail-conflicts", &id, "{}");
    let value = rig.delivery(&id);
    assert_eq!(value["state"], json!("received"));
    assert_eq!(value["conflict"], json!(true));
    // A conflict presupposes a first receipt; without it the evidence is
    // lost, not queued or published.
    fs::remove_file(rig.evidence("pmail-acked", &id)).expect("drop receipt");
    for marker_present in [false, true] {
        if marker_present {
            rig.write_evidence("pmail-published", &id, &marker(&id, &sha).to_string());
        }
        let value = rig.delivery(&id);
        assert_eq!(value["state"], json!("unknown"), "{value}");
        assert_eq!(value["conflict"], json!(true), "{value}");
        assert!(
            value["evidence_file"]
                .as_str()
                .is_some_and(|file| file.ends_with(&format!("pmail-conflicts/{id}.json"))),
            "{value}"
        );
    }
}
