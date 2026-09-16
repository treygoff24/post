mod common;

use common::{assert_success, from_stderr, from_stdout, register_alpha_beta, Sandbox};
use post::output::{ErrorEnvelope, SendOutput};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;

fn digest(key: &str) -> String {
    format!("{:x}", Sha256::digest(key.as_bytes()))
}

fn participant_id(output: &Value) -> &str {
    output["participant"]["id"]
        .as_str()
        .expect("participant id")
}

fn exported_participant(output: &std::process::Output) -> String {
    let line = common::stdout(output);
    line.strip_prefix("export POST_PARTICIPANT=")
        .and_then(|value| value.strip_suffix('\n'))
        .unwrap_or_else(|| panic!("unexpected bootstrap output: {line:?}"))
        .to_owned()
}

fn tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, at: &Path, found: &mut BTreeMap<PathBuf, Vec<u8>>) {
        let entries = match fs::read_dir(at) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => panic!("read {}: {error}", at.display()),
        };
        for entry in entries {
            let entry = entry.expect("tree entry");
            let path = entry.path();
            let relative = path.strip_prefix(root).expect("relative").to_path_buf();
            let kind = entry.file_type().expect("entry type");
            if kind.is_dir() {
                found.insert(relative.clone(), b"<dir>".to_vec());
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

#[test]
fn participant_two_keys_mint_distinct_ids_and_rebind_is_idempotent() {
    let sandbox = Sandbox::new();
    let first = sandbox.bind_claude("conversation-one", &sandbox.path, None);
    let second = sandbox.bind_claude("conversation-two", &sandbox.path, None);
    let repeated = sandbox.bind_claude("conversation-one", &sandbox.path, None);
    assert_ne!(participant_id(&first), participant_id(&second));
    assert_eq!(participant_id(&first), participant_id(&repeated));
    assert_eq!(
        sandbox.read_participant(participant_id(&first))["conversation_key_digest"],
        digest("conversation-one")
    );
}

#[test]
fn participant_bind_key_bootstrap_is_idempotent_and_prints_export() {
    let sandbox = Sandbox::new();
    let args = [
        "participant",
        "bind",
        "--harness",
        "native",
        "--key",
        "stable-native-key",
    ];
    let first = sandbox.run(&args);
    assert_success(&first);
    let first_id = exported_participant(&first);
    assert!(first_id.starts_with("native-"));
    let second = sandbox.run(&args);
    assert_success(&second);
    assert_eq!(exported_participant(&second), first_id);

    let json = sandbox.run(&[
        "participant",
        "bind",
        "--harness",
        "native",
        "--key",
        "stable-native-key",
        "--json",
    ]);
    assert_success(&json);
    let json: Value = from_stdout(&json);
    assert_eq!(json["id"], first_id);
    assert_eq!(participant_id(&json), first_id);
}

#[test]
fn participant_bind_new_uses_fresh_uuid_and_default_shell_harness() {
    let sandbox = Sandbox::new();
    let first = sandbox.run(&["participant", "bind", "--new"]);
    let second = sandbox.run(&["participant", "bind", "--new"]);
    assert_success(&first);
    assert_success(&second);
    let first = exported_participant(&first);
    let second = exported_participant(&second);
    assert!(first.starts_with("shell-"));
    assert!(second.starts_with("shell-"));
    assert_ne!(first, second, "--new must use a fresh UUID key");
}

#[test]
fn participant_explicit_bind_workspace_rebinds_existing_record() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let bound = sandbox.bind_claude("explicit-rebind", &alpha, Some("alpha"));
    let id = participant_id(&bound).to_owned();
    let rebound = sandbox.run_as_participant(
        &["participant", "bind", "--workspace", "beta", "--json"],
        &id,
        &sandbox.path,
    );
    assert_success(&rebound);
    let rebound: Value = from_stdout(&rebound);
    assert_eq!(rebound["participant"]["id"], id);
    assert_eq!(rebound["participant"]["workspace"], "beta");
    assert_eq!(sandbox.read_participant(&id)["workspace"], "beta");
}

#[test]
fn participant_native_harness_labels_ignore_post_harness_override() {
    let sandbox = Sandbox::new();
    let output = sandbox.run_in_env(
        &["participant", "bind", "--json"],
        None,
        &sandbox.path,
        &[
            ("CLAUDE_CODE_SESSION_ID", "canonical-claude-key"),
            ("POST_HARNESS", "not-claude"),
        ],
    );
    assert_success(&output);
    let output: Value = from_stdout(&output);
    assert!(participant_id(&output).starts_with("claude-"));
    assert_eq!(output["participant"]["harness"], "claude");

    let launcher = sandbox.run_in_env(
        &["participant", "bind", "--json"],
        None,
        &sandbox.path,
        &[
            ("POST_SENDER_ADDRESS", "address-label.repo-key.launch-key"),
            ("POST_HARNESS", "launcher-label"),
        ],
    );
    assert_success(&launcher);
    let launcher: Value = from_stdout(&launcher);
    assert!(participant_id(&launcher).starts_with("launcher-label-"));
    assert_eq!(launcher["participant"]["harness"], "launcher-label");

    let fresh = sandbox.run_in_env(
        &["participant", "bind", "--new", "--json"],
        None,
        &sandbox.path,
        &[("POST_HARNESS", "fresh-label")],
    );
    assert_success(&fresh);
    let fresh: Value = from_stdout(&fresh);
    assert!(participant_id(&fresh).starts_with("fresh-label-"));
}

#[test]
fn participant_concurrent_bind_converges_on_one_record() {
    let sandbox = Sandbox::new();
    let key = "one-concurrent-conversation";
    let mut children = Vec::new();
    for _ in 0..8 {
        children.push(
            common::post_command()
                .args(["participant", "bind"])
                .current_dir(&sandbox.path)
                .env("HOME", &sandbox.home)
                .env("POST_MAIL_ROOT", &sandbox.mail_root)
                .env_remove("POST_PARTICIPANT")
                .env("CLAUDE_CODE_SESSION_ID", key)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn concurrent bind"),
        );
    }
    let mut ids = Vec::new();
    for child in children {
        let output = child.wait_with_output().expect("wait concurrent bind");
        assert_success(&output);
        let value: Value = from_stdout(&output);
        ids.push(participant_id(&value).to_owned());
    }
    assert!(ids.iter().all(|id| id == &ids[0]), "ids diverged: {ids:?}");
    let records = fs::read_dir(sandbox.mail_root.join("participants"))
        .expect("participants")
        .filter_map(Result::ok)
        .filter(|entry| entry.path().join("participant.json").is_file())
        .filter(|entry| entry.file_name().to_string_lossy() != "test-default")
        .count();
    assert_eq!(records, 1, "one conversation must mint one record");
}

#[test]
fn participant_crash_after_record_before_index_reconverges() {
    let sandbox = Sandbox::new();
    let key = "crash-cut-conversation";
    let first = sandbox.bind_codex(key, &sandbox.path, None);
    let id = participant_id(&first).to_owned();
    let index = sandbox
        .mail_root
        .join("participants/by-session/codex")
        .join(digest(key));
    fs::remove_file(&index).expect("simulate crash before index write");

    let rebound = sandbox.bind_codex(key, &sandbox.path, None);
    assert_eq!(participant_id(&rebound), id);
    assert_eq!(
        fs::read_to_string(index).expect("repaired index"),
        format!("{id}\n")
    );
}

#[test]
fn participant_dangling_index_re_mints_same_deterministic_record() {
    let sandbox = Sandbox::new();
    let key = "dangling-index-conversation";
    let first = sandbox.bind_claude(key, &sandbox.path, None);
    let id = participant_id(&first).to_owned();
    fs::remove_file(
        sandbox
            .mail_root
            .join("participants")
            .join(&id)
            .join("participant.json"),
    )
    .expect("simulate missing index target");
    let rebound = sandbox.bind_claude(key, &sandbox.path, None);
    assert_eq!(participant_id(&rebound), id);
    assert_eq!(sandbox.read_participant(&id)["id"], id);
}

#[test]
fn participant_digest_mismatch_extends_id_to_twelve_hex() {
    let sandbox = Sandbox::new();
    let key = "collision-conversation";
    let actual = digest(key);
    let short = format!("claude-{}", &actual[..8]);
    let dir = sandbox.mail_root.join("participants").join(&short);
    fs::create_dir_all(&dir).expect("collision dir");
    fs::write(
        dir.join("participant.json"),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&serde_json::json!({
                "version": 1,
                "id": short,
                "harness": "claude",
                "conversation_key_digest": "f".repeat(64),
                "created": "2026-09-16 00:00:00 +0000"
            }))
            .expect("collision record")
        ),
    )
    .expect("write collision");
    let bound = sandbox.bind_claude(key, &sandbox.path, None);
    assert_eq!(participant_id(&bound), format!("claude-{}", &actual[..12]));
    assert_eq!(
        sandbox.read_participant(&short)["conversation_key_digest"],
        "f".repeat(64)
    );
}

#[test]
fn participant_read_only_unbound_commands_create_nothing() {
    let sandbox = Sandbox::new_unseeded();
    let before = tree(&sandbox.mail_root);
    for args in [
        &["who"] as &[&str],
        &["inbox"],
        &["doctor"],
        &["channels"],
        &["schema"],
    ] {
        let output = sandbox.run_unbound(args, &sandbox.path);
        assert!(
            output.status.success() || args[0] == "doctor",
            "{} failed unexpectedly: {}",
            args[0],
            common::stderr(&output)
        );
        let combined = format!("{}{}", common::stdout(&output), common::stderr(&output));
        assert!(
            combined.contains("unbound"),
            "{} omitted unbound: {combined}",
            args[0]
        );
        assert_eq!(
            tree(&sandbox.mail_root),
            before,
            "{} mutated the store",
            args[0]
        );
    }
}

#[test]
fn participant_unbound_send_fails_with_exact_bind_fix() {
    let sandbox = Sandbox::new_unseeded();
    let output = sandbox.run_unbound(
        &["send", "--to", "anywhere", "--body", "must not write"],
        &sandbox.path,
    );
    assert_eq!(output.status.code(), Some(65));
    let error: ErrorEnvelope = from_stderr(&output);
    assert_eq!(error.error.code, "no_participant");
    assert_eq!(
        error.error.details.exact_fix.as_deref(),
        Some("post participant bind")
    );
    assert!(error.error.message.contains("run: post participant bind"));
    assert!(tree(&sandbox.mail_root).is_empty());
}

#[test]
fn participant_typed_targets_write_canonical_store_and_stamp_sender_fields() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let sender = sandbox.bind_claude("typed-sender", &alpha, Some("alpha"));
    let sender_id = participant_id(&sender).to_owned();
    let receiver = sandbox.bind_codex("typed-receiver", &sandbox.path, Some("beta"));
    let receiver_id = participant_id(&receiver).to_owned();
    let lineage_dir = sandbox.mail_root.join("lineages/ember");
    fs::create_dir_all(&lineage_dir).expect("lineage dir");
    fs::write(
        lineage_dir.join("lineage.json"),
        r#"{"name":"ember","founder":"founder-id","created":"2026-09-16 00:00:00 +0000","host":"test"}"#,
    )
    .expect("lineage record");

    let lineage_send = sandbox.run_as_participant(
        &[
            "send",
            "--to",
            "lineage:ember",
            "--kind",
            "letter",
            "--body",
            "lineage body",
            "--json",
        ],
        &sender_id,
        &alpha,
    );
    assert_success(&lineage_send);
    let lineage: SendOutput = from_stdout(&lineage_send);
    assert_eq!(lineage.envelope.kind.as_str(), "letter");
    assert_eq!(lineage.envelope.from, "alpha");
    assert_eq!(
        lineage.envelope.from_participant.as_deref(),
        Some(sender_id.as_str())
    );
    assert_eq!(lineage.envelope.address_kind.as_deref(), Some("lineage"));
    assert!(lineage_dir
        .join("inbox")
        .join(format!("{}.mail", lineage.envelope.id))
        .is_file());

    let sender_record_path = sandbox
        .mail_root
        .join("participants")
        .join(&sender_id)
        .join("participant.json");
    let mut sender_record = sandbox.read_participant(&sender_id);
    sender_record["lineage"] = Value::String("ember".to_owned());
    fs::write(
        &sender_record_path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&sender_record).expect("sender record")
        ),
    )
    .expect("affiliate sender fixture");

    let participant_send = sandbox.run_as_participant(
        &[
            "send",
            "--to",
            &format!("participant:{receiver_id}"),
            "--kind",
            "signal",
            "--body",
            "participant body",
            "--json",
        ],
        &sender_id,
        &alpha,
    );
    assert_success(&participant_send);
    let participant: SendOutput = from_stdout(&participant_send);
    assert_eq!(participant.envelope.kind.as_str(), "signal");
    assert_eq!(
        participant.envelope.address_kind.as_deref(),
        Some("participant")
    );
    assert_eq!(participant.envelope.from_lineage.as_deref(), Some("ember"));
    assert!(sandbox
        .mail_root
        .join("participants")
        .join(receiver_id)
        .join("inbox")
        .join(format!("{}.mail", participant.envelope.id))
        .is_file());

    assert_success(&sandbox.run_as_participant(
        &["chat", "participant-fields", "--join", "--json"],
        &sender_id,
        &alpha,
    ));
    let channel = sandbox.run_as_participant(
        &[
            "chat",
            "participant-fields",
            "--send",
            "--anyway",
            "--body",
            "channel body",
            "--json",
        ],
        &sender_id,
        &alpha,
    );
    assert_success(&channel);
    let channel: Value = from_stdout(&channel);
    assert_eq!(channel["message"]["from_participant"], sender_id);
    assert_eq!(channel["message"]["from_lineage"], "ember");
    assert_eq!(channel["message"]["address_kind"], "channel");
}

#[test]
fn participant_old_format_envelope_still_parses() {
    let old: post::output::Envelope = serde_json::from_value(serde_json::json!({
        "id": "20260916-010203-abcdef",
        "from": "alpha",
        "to": "beta",
        "kind": "note",
        "subject": "old",
        "sent": "2026-09-16 01:02:03 +0000"
    }))
    .expect("0.9.0 envelope parses");
    assert!(old.from_participant.is_none());
    assert!(old.from_lineage.is_none());
    assert!(old.address_kind.is_none());
}

#[test]
fn participant_version_json_advertises_store_and_capabilities() {
    let sandbox = Sandbox::new_unseeded();
    let output = sandbox.run_unbound(&["version", "--json"], &sandbox.path);
    assert_success(&output);
    let version: Value = from_stdout(&output);
    assert_eq!(version["store_version"], 2);
    assert!(version["build_sha"]
        .as_str()
        .is_some_and(|sha| !sha.is_empty()));
    assert!(version["capabilities"]
        .as_array()
        .expect("capabilities")
        .iter()
        .any(|value| value == "participants"));
    assert!(tree(&sandbox.mail_root).is_empty());
}

#[test]
fn participant_rooms_add_rejects_new_reserved_store_name() {
    let sandbox = Sandbox::new();
    let path = sandbox.home.join("reserved-target");
    fs::create_dir_all(&path).expect("target");
    let before = fs::read(sandbox.mail_root.join("rooms.json")).expect("rooms before");
    let output = sandbox.run(&[
        "rooms",
        "add",
        "participants",
        path.to_str().expect("path utf8"),
    ]);
    assert!(!output.status.success());
    let error: ErrorEnvelope = from_stderr(&output);
    assert_eq!(error.error.code, "invalid_argument");
    assert_eq!(
        fs::read(sandbox.mail_root.join("rooms.json")).expect("rooms after"),
        before
    );
}

#[test]
fn participant_codex_conflict_is_an_error_not_a_guess() {
    let sandbox = Sandbox::new();
    let output = sandbox.run_in_env(
        &["participant", "bind"],
        None,
        &sandbox.path,
        &[
            ("CODEX_THREAD_ID", "thread-a"),
            ("CODEX_SESSION_ID", "session-b"),
        ],
    );
    assert_eq!(output.status.code(), Some(2));
    let error: ErrorEnvelope = from_stderr(&output);
    assert_eq!(error.error.code, "invalid_argument");
    assert!(error.error.message.contains("differ"));
}

#[test]
fn participant_review_bound_and_unbound_read_only_forms_preserve_complete_tree() {
    let commands: Vec<Vec<&str>> = vec![
        vec!["version", "--json"],
        vec!["participant", "show"],
        vec!["participant", "list"],
        vec!["inbox", "--room", "alpha"],
        vec!["who"],
        vec!["channels"],
        vec!["doctor"],
        vec!["schema"],
        vec!["rooms"],
        vec!["read", "missing", "--room", "alpha", "--peek"],
        vec!["chat", "missing", "--peek"],
        vec!["watch", "--snapshot", "--room", "alpha"],
        vec!["profile", "show", "alpha"],
        vec!["owner", "show"],
        vec!["search", "needle", "--mail", "--json"],
    ];
    for bound in [false, true] {
        for args in &commands {
            let sandbox = Sandbox::new();
            let (alpha, _beta) = register_alpha_beta(&sandbox);
            let actor = sandbox.test_participant("alpha");
            let before = tree(&sandbox.mail_root);
            let output = if bound {
                sandbox.run_as_participant(args, &actor, &alpha)
            } else {
                sandbox.run_unbound(args, &alpha)
            };
            let _ = output;
            assert_eq!(
                tree(&sandbox.mail_root),
                before,
                "{} read-only command mutated the store: {args:?}",
                if bound { "bound" } else { "unbound" }
            );
        }
    }
}

#[test]
fn participant_review_version_is_pure_under_broken_or_ambiguous_identity() {
    let broken = Sandbox::new();
    let dir = broken.mail_root.join("participants/broken-id");
    fs::create_dir_all(&dir).expect("broken participant dir");
    fs::write(dir.join("participant.json"), b"{not json").expect("broken participant");
    let before = tree(&broken.mail_root);
    let output = broken.run_in_env(
        &["version", "--json"],
        None,
        &broken.path,
        &[("POST_PARTICIPANT", "broken-id")],
    );
    assert_success(&output);
    let value: Value = from_stdout(&output);
    assert_eq!(value["store_version"], 2);
    assert_eq!(tree(&broken.mail_root), before);

    let ambiguous = Sandbox::new_unseeded();
    let before = tree(&ambiguous.mail_root);
    let output = ambiguous.run_in_env(
        &["version", "--json"],
        None,
        &ambiguous.path,
        &[
            ("CLAUDE_CODE_SESSION_ID", "claude-key"),
            ("CODEX_THREAD_ID", "codex-key"),
            ("PATH", "/definitely/missing"),
        ],
    );
    assert_success(&output);
    let value: Value = from_stdout(&output);
    assert!(value["capabilities"]
        .as_array()
        .expect("capabilities")
        .iter()
        .any(|value| value == "participants"));
    assert_eq!(tree(&ambiguous.mail_root), before);
}

#[test]
fn participant_review_no_key_diagnostic_names_real_bootstrap_sequence() {
    let sandbox = Sandbox::new_unseeded();
    let output = sandbox.run_unbound(
        &["send", "--to", "nowhere", "--body", "body"],
        &sandbox.path,
    );
    let error: ErrorEnvelope = from_stderr(&output);
    assert_eq!(error.error.code, "no_participant");
    assert!(error.error.details.exact_fix.is_none());
    assert!(error.error.message.contains("post participant bind --new"));
    assert!(error.error.message.contains("export POST_PARTICIPANT="));

    let keyed = Sandbox::new_unseeded();
    let output = keyed.run_as_claude(
        &["send", "--to", "nowhere", "--body", "body"],
        "usable-key",
        &keyed.path,
    );
    let error: ErrorEnvelope = from_stderr(&output);
    assert_eq!(
        error.error.details.exact_fix.as_deref(),
        Some("post participant bind")
    );
}

#[test]
fn participant_review_existing_colon_room_loads_but_typed_namespace_collision_fails() {
    let sandbox = Sandbox::new();
    let workspace = sandbox.home.join("legacy-colon");
    fs::create_dir_all(&workspace).expect("legacy workspace");
    fs::write(
        sandbox.mail_root.join("rooms.json"),
        format!(r#"{{"legacy:room":"{}"}}"#, workspace.display()),
    )
    .expect("legacy rooms");
    fs::write(sandbox.mail_root.join("rules.json"), r#"{"blocked":[]}"#).expect("rules");
    let output = sandbox.run(&["rooms"]);
    assert_success(&output);
    let value: Value = from_stdout(&output);
    assert_eq!(value["rooms"][0]["name"], "legacy:room");

    fs::write(
        sandbox.mail_root.join("rooms.json"),
        format!(r#"{{"participant:foo":"{}"}}"#, workspace.display()),
    )
    .expect("colliding rooms");
    let output = sandbox.run(&["rooms"]);
    let error: ErrorEnvelope = from_stderr(&output);
    assert_eq!(error.error.code, "config_invalid");
    assert!(error
        .error
        .details
        .reason
        .as_deref()
        .is_some_and(|reason| reason.contains("typed address namespace")));
}

#[test]
fn participant_review_lineage_load_survives_later_room_collision() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let actor = sandbox.test_participant("alpha");
    let lineage = sandbox.mail_root.join("lineages/ember");
    fs::create_dir_all(&lineage).expect("lineage dir");
    fs::write(
        lineage.join("lineage.json"),
        r#"{"name":"ember","founder":"founder","created":"2026-09-16","host":"test"}"#,
    )
    .expect("lineage record");
    let room_path = sandbox.home.join("later-ember-room");
    fs::create_dir_all(&room_path).expect("later room");
    let mut rooms: serde_json::Map<String, Value> =
        serde_json::from_slice(&fs::read(sandbox.mail_root.join("rooms.json")).expect("rooms"))
            .expect("rooms JSON");
    rooms.insert(
        "ember".to_owned(),
        Value::String(room_path.display().to_string()),
    );
    fs::write(
        sandbox.mail_root.join("rooms.json"),
        serde_json::to_vec_pretty(&rooms).expect("rooms bytes"),
    )
    .expect("colliding room registry");

    let output = sandbox.run_as_participant(
        &[
            "send",
            "--to",
            "lineage:ember",
            "--body",
            "typed lineage remains addressable",
        ],
        &actor,
        &alpha,
    );
    assert_success(&output);
}

#[test]
fn participant_review_rooms_add_rejects_existing_lineage_name() {
    let sandbox = Sandbox::new();
    let lineage = sandbox.mail_root.join("lineages/ember");
    fs::create_dir_all(&lineage).expect("lineage dir");
    fs::write(
        lineage.join("lineage.json"),
        r#"{"name":"ember","founder":"founder","created":"2026-09-16","host":"test"}"#,
    )
    .expect("lineage record");
    let workspace = sandbox.home.join("ember-room");
    fs::create_dir_all(&workspace).expect("workspace");
    let output = sandbox.run(&[
        "rooms",
        "add",
        "ember",
        workspace.to_str().expect("workspace utf8"),
    ]);
    let error: ErrorEnvelope = from_stderr(&output);
    assert_eq!(error.error.code, "invalid_argument");
    assert!(error.error.message.contains("lineage"));
}
