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
    let output = sandbox.run_as_claude(
        &["send", "--to", "anywhere", "--body", "must not write"],
        "unbound-but-bindable-key",
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
    assert_eq!(version["store_version"], 1);
    assert!(version["build_sha"]
        .as_str()
        .is_some_and(|sha| !sha.is_empty()));
    assert_eq!(version["capabilities"], serde_json::json!(["participants"]));
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
    assert_eq!(value["store_version"], 1);
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
    assert_eq!(value["capabilities"], serde_json::json!(["participants"]));
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
fn participant_review_existing_colon_and_reserved_rooms_load_but_doctor_reports_them() {
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
        format!(
            r#"{{"participant:foo":"{0}","participants":"{0}"}}"#,
            workspace.display()
        ),
    )
    .expect("colliding rooms");
    let output = sandbox.run(&["rooms"]);
    assert_success(&output);
    let rooms: Value = from_stdout(&output);
    assert_eq!(rooms["count"], 2);

    let doctor = sandbox.run(&["doctor"]);
    assert_eq!(doctor.status.code(), Some(1));
    let doctor: Value = from_stdout(&doctor);
    let messages = doctor["checks"]
        .as_array()
        .expect("doctor checks")
        .iter()
        .filter_map(|check| check["message"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(messages.contains("typed address"), "{messages}");
    assert!(messages.contains("reserved"), "{messages}");
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

#[test]
fn participant_round2_explicit_bootstrap_ignores_inherited_identity_and_ambiguity() {
    let sandbox = Sandbox::new();

    let fresh = sandbox.run_in_env(
        &["participant", "bind", "--new"],
        None,
        &sandbox.path,
        &[("POST_PARTICIPANT", "test-default")],
    );
    assert_success(&fresh);
    let fresh_id = exported_participant(&fresh);
    assert_ne!(fresh_id, "test-default");

    let keyed = sandbox.run_in_env(
        &[
            "participant",
            "bind",
            "--harness",
            "child",
            "--key",
            "independent-child",
            "--json",
        ],
        None,
        &sandbox.path,
        &[("POST_PARTICIPANT", "test-default")],
    );
    assert_success(&keyed);
    let keyed: Value = from_stdout(&keyed);
    assert_ne!(participant_id(&keyed), "test-default");
    assert_eq!(keyed["provenance"], "explicit-bootstrap");

    let ambiguous = sandbox.run_in_env(
        &[
            "participant",
            "bind",
            "--harness",
            "child",
            "--key",
            "ambiguous-parent-child",
            "--json",
        ],
        None,
        &sandbox.path,
        &[
            ("CODEX_THREAD_ID", "inherited-thread"),
            ("CODEX_SESSION_ID", "different-inherited-session"),
        ],
    );
    assert_success(&ambiguous);
    let ambiguous: Value = from_stdout(&ambiguous);
    assert!(participant_id(&ambiguous).starts_with("child-"));
    assert_eq!(ambiguous["provenance"], "explicit-bootstrap");
}

#[test]
fn participant_round2_resolution_errors_are_advisory_on_read_only_surfaces() {
    let sandbox = Sandbox::new();
    let before = tree(&sandbox.mail_root);
    for args in [
        vec!["schema"],
        vec!["doctor"],
        vec!["who"],
        vec!["participant", "show"],
        vec!["participant", "list"],
    ] {
        let output = sandbox.run_in_env(
            &args,
            None,
            &sandbox.path,
            &[("POST_PARTICIPANT", "bad:id")],
        );
        assert_success(&output);
        let value: Value = from_stdout(&output);
        assert_eq!(value["ok"], true, "{args:?}: {}", common::stdout(&output));
        assert!(
            value["participant_error"]
                .as_str()
                .is_some_and(|message| message.contains("must not contain ':'")),
            "{args:?}: {}",
            common::stdout(&output)
        );
        assert_eq!(tree(&sandbox.mail_root), before, "{args:?} mutated state");
    }

    let plain_bind = sandbox.run_in_env(
        &["participant", "bind", "--json"],
        None,
        &sandbox.path,
        &[("POST_PARTICIPANT", "bad:id")],
    );
    assert_eq!(plain_bind.status.code(), Some(2));
}

#[test]
fn participant_round2_plain_rebind_preserves_existing_workspace() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let first = sandbox.bind_claude("stable-workspace-key", &beta, Some("beta"));
    let id = participant_id(&first).to_owned();

    let rebound = sandbox.run_as_claude(
        &["participant", "bind", "--json"],
        "stable-workspace-key",
        &alpha,
    );
    assert_success(&rebound);
    let rebound: Value = from_stdout(&rebound);
    assert_eq!(participant_id(&rebound), id);
    assert_eq!(rebound["participant"]["workspace"], "beta");
    assert_eq!(sandbox.read_participant(&id)["workspace"], "beta");
}

#[test]
fn participant_round2_binding_provenance_wins_when_cwd_differs() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let bound = sandbox.bind_claude("binding-provenance", &beta, Some("beta"));
    let actor = participant_id(&bound).to_owned();

    let direct = sandbox.run_as_participant(
        &["send", "--to", "alpha", "--body", "direct", "--json"],
        &actor,
        &alpha,
    );
    assert_success(&direct);
    let direct: Value = from_stdout(&direct);
    assert_eq!(direct["envelope"]["from"], "beta");
    assert_eq!(
        direct["envelope"]["sender_provenance"],
        "participant-binding"
    );

    let joined = sandbox.run_as_participant(
        &["chat", "round2-provenance", "--join", "--json"],
        &actor,
        &beta,
    );
    assert_success(&joined);
    let channel = sandbox.run_as_participant(
        &[
            "chat",
            "round2-provenance",
            "--send",
            "--anyway",
            "--body",
            "channel",
            "--json",
        ],
        &actor,
        &alpha,
    );
    assert_success(&channel);
    let channel: Value = from_stdout(&channel);
    assert_eq!(channel["message"]["from"], "beta");
    assert_eq!(
        channel["message"]["sender_provenance"],
        "participant-binding"
    );
}

#[test]
fn participant_round2_collision_race_preserves_both_keys() {
    const FIRST: &str = "collision-key-25835";
    const SECOND: &str = "collision-key-54347";
    assert_eq!(&digest(FIRST)[..8], &digest(SECOND)[..8]);

    let sandbox = Sandbox::new();
    let mut children = Vec::new();
    for key in [FIRST, SECOND].into_iter().cycle().take(24) {
        let child = common::post_command()
            .args([
                "participant",
                "bind",
                "--harness",
                "native",
                "--key",
                key,
                "--json",
            ])
            .current_dir(&sandbox.path)
            .env_clear()
            .env("HOME", &sandbox.home)
            .env("POST_MAIL_ROOT", &sandbox.mail_root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn colliding bind");
        children.push((key, child));
    }

    let mut ids_by_key: BTreeMap<&str, std::collections::BTreeSet<String>> = BTreeMap::new();
    for (key, child) in children {
        let output = child.wait_with_output().expect("wait colliding bind");
        assert_success(&output);
        let output: Value = from_stdout(&output);
        ids_by_key
            .entry(key)
            .or_default()
            .insert(participant_id(&output).to_owned());
    }
    assert_eq!(ids_by_key[FIRST].len(), 1, "{ids_by_key:?}");
    assert_eq!(ids_by_key[SECOND].len(), 1, "{ids_by_key:?}");
    let first_id = ids_by_key[FIRST].first().expect("first id");
    let second_id = ids_by_key[SECOND].first().expect("second id");
    assert_ne!(first_id, second_id);
    assert_eq!(
        sandbox.read_participant(first_id)["conversation_key_digest"],
        digest(FIRST)
    );
    assert_eq!(
        sandbox.read_participant(second_id)["conversation_key_digest"],
        digest(SECOND)
    );
}

#[test]
fn participant_round2_fully_unbound_read_only_forms_work_without_mutation() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    assert_success(&sandbox.run_in(&["chat", "round2-read", "--join", "--json"], None, &alpha));
    assert_success(&sandbox.run_in(&["chat", "round2-read", "--join", "--json"], None, &beta));
    let sent_channel = sandbox.run_in(
        &[
            "chat",
            "round2-read",
            "--send",
            "--anyway",
            "--body",
            "needle channel body",
            "--json",
        ],
        None,
        &beta,
    );
    assert_success(&sent_channel);
    let sent_channel: Value = from_stdout(&sent_channel);
    let channel_id = sent_channel["message"]["id"]
        .as_str()
        .expect("channel id")
        .to_owned();
    let sent_mail: SendOutput = from_stdout(&sandbox.run_in(
        &["send", "--to", "alpha", "--body", "needle direct", "--json"],
        None,
        &beta,
    ));

    let commands = vec![
        vec!["rooms".to_owned()],
        vec!["version".to_owned(), "--json".to_owned()],
        vec![
            "watch".to_owned(),
            "--snapshot".to_owned(),
            "--room".to_owned(),
            "alpha".to_owned(),
        ],
        vec![
            "read".to_owned(),
            sent_mail.envelope.id,
            "--room".to_owned(),
            "alpha".to_owned(),
            "--peek".to_owned(),
            "--json".to_owned(),
        ],
        vec!["profile".to_owned(), "show".to_owned()],
        vec![
            "search".to_owned(),
            "needle".to_owned(),
            "--channel".to_owned(),
            "round2-read".to_owned(),
            "--json".to_owned(),
        ],
        vec![
            "chat".to_owned(),
            "round2-read".to_owned(),
            "--peek".to_owned(),
            "--json".to_owned(),
        ],
        vec![
            "chat".to_owned(),
            "round2-read".to_owned(),
            "--history".to_owned(),
            "1".to_owned(),
            "--json".to_owned(),
        ],
        vec![
            "chat".to_owned(),
            "round2-read".to_owned(),
            "--since".to_owned(),
            channel_id.clone(),
            "--json".to_owned(),
        ],
        vec![
            "chat".to_owned(),
            "round2-read".to_owned(),
            "--seen-by".to_owned(),
            channel_id.clone(),
            "--json".to_owned(),
        ],
        vec![
            "chat".to_owned(),
            "round2-read".to_owned(),
            "--message".to_owned(),
            channel_id,
            "--max-bytes".to_owned(),
            "4096".to_owned(),
            "--json".to_owned(),
        ],
    ];
    for command in commands {
        let args = command.iter().map(String::as_str).collect::<Vec<_>>();
        let before = tree(&sandbox.mail_root);
        let output = sandbox.run_without_identity(&args, &alpha);
        assert_success(&output);
        assert_eq!(
            tree(&sandbox.mail_root),
            before,
            "fully unbound read mutated state: {args:?}"
        );
    }
}

#[test]
fn participant_round2_workspace_less_actor_gets_rebind_fix_not_room_shadowing() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let bound = sandbox.run(&[
        "participant",
        "bind",
        "--harness",
        "shell",
        "--key",
        "workspace-less",
        "--json",
    ]);
    assert_success(&bound);
    let bound: Value = from_stdout(&bound);
    let actor = participant_id(&bound).to_owned();
    assert!(bound["participant"]["workspace"].is_null());
    let before = tree(&sandbox.mail_root);

    let output =
        sandbox.run_as_participant(&["chat", "any-channel", "--peek", "--json"], &actor, &alpha);
    let error: ErrorEnvelope = from_stderr(&output);
    assert_eq!(error.error.code, "unknown_room");
    assert!(error.error.message.contains("has no workspace"));
    assert_eq!(
        error.error.details.exact_fix.as_deref(),
        Some("post participant bind --workspace 'alpha'")
    );
    assert!(!error.error.suggested_fix.contains("post rooms add"));
    assert_eq!(tree(&sandbox.mail_root), before);
}

#[test]
fn participant_round2_unbound_annotation_keeps_ok_first() {
    let sandbox = Sandbox::new();
    let output = sandbox.run_without_identity(&["rooms"], &sandbox.path);
    assert_success(&output);
    let raw = common::stdout(&output);
    assert!(
        raw.starts_with("{\"ok\":true"),
        "unexpected key order: {raw}"
    );
    let value: Value = serde_json::from_str(&raw).expect("annotated rooms JSON");
    assert_eq!(value["participant"], "unbound");
}
