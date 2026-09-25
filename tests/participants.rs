mod common;

use common::{
    assert_success, from_stderr, from_stdout, register_alpha_beta, write_custom_mail, Sandbox,
};
use post::output::{ErrorEnvelope, SendOutput};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
#[cfg(unix)]
use std::{fs::OpenOptions, os::fd::AsRawFd, os::unix::fs::OpenOptionsExt, thread, time::Duration};

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

fn edit_participant(
    sandbox: &Sandbox,
    id: &str,
    edit: impl FnOnce(&mut serde_json::Map<String, Value>),
) -> Value {
    let path = sandbox
        .mail_root
        .join("participants")
        .join(id)
        .join("participant.json");
    let mut value: Value =
        serde_json::from_slice(&fs::read(&path).expect("read participant record"))
            .expect("participant JSON");
    edit(value.as_object_mut().expect("participant object"));
    fs::write(
        &path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&value).expect("serialize participant")
        ),
    )
    .expect("write participant record");
    value
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
fn roomless_channel_send_reports_when_it_stays_local() {
    let sandbox = Sandbox::new();
    let bound = sandbox.bind_codex("roomless-receipt", &sandbox.path, None);
    let id = participant_id(&bound);
    assert_success(&sandbox.run_as_participant(
        &["chat", "local-only", "--join", "--json"],
        id,
        &sandbox.path,
    ));
    let sent = sandbox.run_as_participant(
        &["chat", "local-only", "--body", "hello", "--json"],
        id,
        &sandbox.path,
    );
    assert!(sent.status.success(), "{}", common::stderr(&sent));
    let receipt: Value = from_stdout(&sent);
    assert_eq!(receipt["message"]["from"], id);
    assert_eq!(receipt["cross_host"]["status"], "local_only");
    assert_eq!(receipt["cross_host"]["reason"], "no bridge config");
    assert!(common::stderr(&sent).contains("sent locally only: no bridge config"));

    let bridge = sandbox.mail_root.join("bridge");
    fs::create_dir_all(&bridge).expect("bridge directory");
    fs::write(
        bridge.join("config.json"),
        r#"{"host":"mac","channels":{"mode":"all","deny":["local-only"]}}"#,
    )
    .expect("deny config");
    let denied = sandbox.run_as_participant(
        &[
            "chat",
            "local-only",
            "--body",
            "still local",
            "--anyway",
            "--json",
        ],
        id,
        &sandbox.path,
    );
    assert!(denied.status.success(), "{}", common::stderr(&denied));
    let receipt: Value = from_stdout(&denied);
    assert_eq!(
        receipt["cross_host"]["reason"],
        "channel is denied by this host"
    );
    assert!(common::stderr(&denied).contains("sent locally only: channel is denied by this host"));

    fs::write(
        bridge.join("config.json"),
        r#"{"host":"mac","channels":null}"#,
    )
    .expect("channels-off config");
    let off = sandbox.run_as_participant(
        &[
            "chat",
            "local-only",
            "--body",
            "sync off",
            "--anyway",
            "--json",
        ],
        id,
        &sandbox.path,
    );
    assert!(off.status.success(), "{}", common::stderr(&off));
    let receipt: Value = from_stdout(&off);
    assert_eq!(receipt["cross_host"]["reason"], "channel sync is off");

    fs::write(bridge.join("config.json"), r#"{"host":"mac"}"#).expect("sync-all config");
    fs::create_dir_all(bridge.join("registry")).expect("bridge registry");
    fs::write(
        bridge.join("registry").join("hosts.json"),
        r#"{"v":1,"hosts":["mac","trey"]}"#,
    )
    .expect("peer registry");
    let no_tick = sandbox.run_as_participant(
        &[
            "chat",
            "local-only",
            "--body",
            "no tick",
            "--anyway",
            "--json",
        ],
        id,
        &sandbox.path,
    );
    assert!(no_tick.status.success(), "{}", common::stderr(&no_tick));
    let receipt: Value = from_stdout(&no_tick);
    assert_eq!(
        receipt["cross_host"]["reason"],
        "no running bridge reported"
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
fn participant_text_surfaces_render_own_profile_then_lineage_with_participant_id() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let rowan = sandbox.bind_codex("byline-rowan", &alpha, Some("alpha"));
    let rowan = participant_id(&rowan).to_owned();
    let fable = sandbox.bind_claude("byline-fable", &alpha, Some("alpha"));
    let fable = participant_id(&fable).to_owned();
    let reader = sandbox.bind_claude("byline-reader", &alpha, Some("alpha"));
    let reader = participant_id(&reader).to_owned();

    for (participant, lineage) in [(&rowan, "rowan"), (&fable, "fable")] {
        edit_participant(&sandbox, participant, |record| {
            record.insert("lineage".to_owned(), Value::String(lineage.to_owned()));
            record.insert(
                "lineage_since".to_owned(),
                Value::String("2026-09-19T20:00:00Z".to_owned()),
            );
        });
    }
    assert_success(&sandbox.run_as_participant(
        &["profile", "set", "--name", "Cairn", "--pfp", "🪨"],
        &rowan,
        &alpha,
    ));

    for participant in [&rowan, &fable, &reader] {
        assert_success(&sandbox.run_as_participant(
            &["chat", "shared-byline", "--join", "--json"],
            participant,
            &alpha,
        ));
    }
    for (participant, body) in [
        (&rowan, "rowan-byline-render-probe"),
        (&fable, "fable-byline-render-probe"),
    ] {
        assert_success(&sandbox.run_as_participant(
            &[
                "chat",
                "shared-byline",
                "--send",
                "--anyway",
                "--body",
                body,
                "--json",
            ],
            participant,
            &alpha,
        ));
    }
    let mail = sandbox.run_as_participant(
        &[
            "send",
            "--to",
            &format!("participant:{reader}"),
            "--body",
            "rowan-byline-render-probe mail",
            "--json",
        ],
        &rowan,
        &alpha,
    );
    assert_success(&mail);
    let mail: Value = from_stdout(&mail);
    let mail_id = mail["envelope"]["id"].as_str().expect("mail id");

    // Rowan set a profile: it is Rowan's own (participant-keyed), so the text
    // byline renders it, keeps the participant id, and Fable — same workspace,
    // no profile of its own — renders by lineage and id, never as Cairn.
    let rowan_label = format!("🪨 Cairn [{rowan}] (alpha)");
    let fable_label = format!("fable [{fable}] (alpha)");
    let rowan_quoted_label = format!("🪨 Cairn [{rowan}] (\"alpha\")");
    let fable_quoted_label = format!("fable [{fable}] (\"alpha\")");

    let chat = sandbox.run_as_participant(
        &["chat", "shared-byline", "--history", "20"],
        &reader,
        &alpha,
    );
    assert_success(&chat);
    let chat = common::stdout(&chat);
    assert!(chat.contains(&rowan_label), "chat omitted lineage: {chat}");
    assert!(chat.contains(&fable_label), "chat omitted lineage: {chat}");

    let inbox = sandbox.run_as_participant(&["inbox", "--text"], &reader, &alpha);
    assert_success(&inbox);
    let inbox = common::stdout(&inbox);
    assert!(
        inbox.contains(&rowan_quoted_label),
        "inbox omitted lineage: {inbox}"
    );

    let read = sandbox.run_as_participant(&["read", mail_id, "--peek"], &reader, &alpha);
    assert_success(&read);
    let read = common::stdout(&read);
    assert!(read.contains(&rowan_label), "read omitted lineage: {read}");

    let search = sandbox.run_as_participant(&["search", "byline-render-probe"], &reader, &alpha);
    assert_success(&search);
    let search = common::stdout(&search);
    assert!(
        search.contains(&rowan_label),
        "search omitted Rowan lineage: {search}"
    );
    assert!(
        search.contains(&fable_label),
        "search omitted Fable lineage: {search}"
    );
    let search_json = sandbox.run_as_participant(
        &["search", "byline-render-probe", "--json"],
        &reader,
        &alpha,
    );
    assert_success(&search_json);
    let search_json: Value = from_stdout(&search_json);
    for (lineage, participant) in [("rowan", &rowan), ("fable", &fable)] {
        assert!(
            search_json["results"]
                .as_array()
                .expect("search results")
                .iter()
                .any(|result| {
                    result["from_lineage"] == lineage
                        && result["from_participant"] == participant.as_str()
                }),
            "search JSON omitted {lineage} attribution: {search_json}"
        );
    }

    let watch = sandbox.run_as_participant(&["watch", "--snapshot", "--text"], &reader, &alpha);
    assert_success(&watch);
    let watch = common::stdout(&watch);
    assert!(
        watch.contains(&rowan_quoted_label),
        "watch omitted Rowan lineage: {watch}"
    );
    assert!(
        watch.contains(&fable_quoted_label),
        "watch omitted Fable lineage: {watch}"
    );
    let watch_json =
        sandbox.run_as_participant(&["watch", "--snapshot", "--json"], &reader, &alpha);
    assert_success(&watch_json);
    let watch_json = common::stdout(&watch_json);
    let watch_events = watch_json
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("watch event JSON"))
        .collect::<Vec<_>>();
    for (lineage, participant) in [("rowan", &rowan), ("fable", &fable)] {
        assert!(
            watch_events.iter().any(|event| {
                event["from_lineage"] == lineage
                    && event["from_participant"] == participant.as_str()
            }),
            "watch JSON omitted {lineage} attribution: {watch_json}"
        );
    }

    let digest = sandbox.run_as_participant(
        &["watch", "--snapshot", "--digest", "--text"],
        &reader,
        &alpha,
    );
    assert_success(&digest);
    let digest = common::stdout(&digest);
    assert!(
        digest.contains(&rowan_label),
        "watch digest omitted Rowan lineage: {digest}"
    );
    assert!(
        digest.contains(&fable_label),
        "watch digest omitted Fable lineage: {digest}"
    );

    let catchup = sandbox.run_as_participant(&["catchup", "--all"], &reader, &alpha);
    assert_success(&catchup);
    let catchup = common::stdout(&catchup);
    assert!(
        catchup.contains(&rowan_label),
        "catchup omitted Rowan lineage: {catchup}"
    );
    assert!(
        catchup.contains(&fable_label),
        "catchup omitted Fable lineage: {catchup}"
    );
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
    assert_eq!(
        version["capabilities"],
        serde_json::json!(["participants", "lineages", "routing-receipts", "cursors-v2"])
    );
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
    let commands: Vec<(Vec<&str>, Option<&str>)> = vec![
        (vec!["version", "--json"], None),
        (vec!["participant", "show"], None),
        (vec!["participant", "list"], None),
        (vec!["inbox", "--room", "alpha"], None),
        (vec!["who"], None),
        (vec!["channels"], None),
        (vec!["doctor"], Some("doctor_findings")),
        (vec!["schema"], None),
        (vec!["rooms"], None),
        (
            vec!["read", "missing", "--room", "alpha", "--peek"],
            Some("not_found"),
        ),
        (
            vec![
                "read",
                "20260916-040001-a1b2c4",
                "--room",
                "alpha",
                "--offset",
                "0",
                "--length",
                "4",
                "--max-bytes",
                "4096",
                "--json",
            ],
            None,
        ),
        (vec!["chat", "missing", "--peek"], Some("missing_chat")),
        (vec!["watch", "--snapshot", "--room", "alpha"], None),
        (vec!["profile", "show", "alpha"], None),
        (vec!["owner", "show"], None),
        (vec!["search", "needle", "--mail", "--json"], None),
    ];
    for bound in [false, true] {
        for (args, expected) in &commands {
            let sandbox = Sandbox::new();
            let (alpha, _beta) = register_alpha_beta(&sandbox);
            let actor = sandbox.test_participant("alpha");
            write_custom_mail(
                &sandbox.mail_root.join("alpha/inbox"),
                "20260916-040001-a1b2c4",
                &serde_json::json!({
                    "id": "20260916-040001-a1b2c4",
                    "from": "beta",
                    "to": "alpha",
                    "kind": "note",
                    "subject": "slice",
                    "sent": "2026-09-16 00:00:00 +0000"
                }),
                "slice body",
            );
            let before = tree(&sandbox.mail_root);
            let output = if bound {
                sandbox.run_as_participant(args, &actor, &alpha)
            } else {
                sandbox.run_unbound(args, &alpha)
            };
            match expected {
                None => assert_success(&output),
                Some("doctor_findings") => {
                    assert_eq!(output.status.code(), Some(1), "{args:?}");
                    let value: Value = from_stdout(&output);
                    assert_eq!(value["ok"], false, "{args:?}");
                }
                Some("missing_chat") => {
                    let error: ErrorEnvelope = from_stderr(&output);
                    assert_eq!(
                        error.error.code,
                        if bound { "not_a_member" } else { "not_found" },
                        "{args:?}"
                    );
                }
                Some(code) => {
                    let error: ErrorEnvelope = from_stderr(&output);
                    assert_eq!(error.error.code, *code, "{args:?}");
                }
            }
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
fn participant_round4_unbound_streams_keep_stdout_protocol_and_budget() {
    let empty = Sandbox::new();
    let empty_output = empty.run_without_identity(
        &["watch", "--snapshot", "--room", "claude-space"],
        &empty.path,
    );
    assert_success(&empty_output);
    assert_eq!(
        empty_output.stdout.len(),
        0,
        "empty NDJSON stream was poisoned"
    );
    assert!(common::stderr(&empty_output).contains("participant: unbound"));

    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let id = "20260916-040000-a1b2c3";
    write_custom_mail(
        &sandbox.mail_root.join("alpha/inbox"),
        id,
        &serde_json::json!({
            "id": id,
            "from": "beta",
            "to": "alpha",
            "kind": "note",
            "subject": "round four",
            "sent": "2026-09-16 04:00:00 -0500"
        }),
        "budgeted body",
    );

    let actor = sandbox.test_participant("alpha");
    let baseline = sandbox.run_as_participant(
        &[
            "read",
            id,
            "--room",
            "alpha",
            "--offset",
            "0",
            "--length",
            "8",
            "--max-bytes",
            "4096",
            "--json",
        ],
        &actor,
        &alpha,
    );
    assert_success(&baseline);
    let cap = baseline.stdout.len().to_string();
    let budgeted_args = vec![
        "read",
        id,
        "--room",
        "alpha",
        "--offset",
        "0",
        "--length",
        "8",
        "--max-bytes",
        cap.as_str(),
        "--json",
    ];
    let budgeted = sandbox.run_without_identity(&budgeted_args, &alpha);
    assert_success(&budgeted);
    assert!(
        budgeted.stdout.len() <= baseline.stdout.len(),
        "unbound notice exceeded the {}-byte budget: {} bytes",
        baseline.stdout.len(),
        budgeted.stdout.len()
    );
    assert!(common::stderr(&budgeted).contains("participant: unbound"));

    let watched = sandbox.run_without_identity(&["watch", "--snapshot", "--room", "alpha"], &alpha);
    assert_success(&watched);
    assert!(!watched.stdout.is_empty());
    for line in common::stdout(&watched).lines() {
        serde_json::from_str::<Value>(line)
            .unwrap_or_else(|error| panic!("invalid watch NDJSON line {line:?}: {error}"));
    }
    assert!(common::stderr(&watched).contains("participant: unbound"));
}

#[test]
fn participant_round4_typed_participant_blocks_use_recipient_workspace() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let sender = sandbox.bind_claude("blocked-sender", &alpha, Some("alpha"));
    let sender_id = participant_id(&sender).to_owned();
    let receiver = sandbox.bind_codex("blocked-receiver", &beta, Some("beta"));
    let receiver_id = participant_id(&receiver).to_owned();

    fs::write(
        sandbox.mail_root.join("rules.json"),
        r#"{"blocked":[{"from":"alpha","to":"beta","reason":"beta is blocked"}]}"#,
    )
    .expect("block beta");
    let blocked = sandbox.run_as_participant(
        &[
            "send",
            "--to",
            &format!("participant:{receiver_id}"),
            "--body",
            "must not land",
        ],
        &sender_id,
        &alpha,
    );
    let error: ErrorEnvelope = from_stderr(&blocked);
    assert_eq!(error.error.code, "blocked_route");

    fs::write(
        sandbox.mail_root.join("rules.json"),
        r#"{"blocked":[{"from":"alpha","to":"claude-space","reason":"other target"}]}"#,
    )
    .expect("allow beta");
    assert_success(&sandbox.run_as_participant(
        &[
            "send",
            "--to",
            &format!("participant:{receiver_id}"),
            "--body",
            "allowed",
        ],
        &sender_id,
        &alpha,
    ));

    let workspace_less = sandbox.run(&[
        "participant",
        "bind",
        "--harness",
        "shell",
        "--key",
        "workspace-less-block-target",
        "--json",
    ]);
    assert_success(&workspace_less);
    let workspace_less: Value = from_stdout(&workspace_less);
    let workspace_less_id = participant_id(&workspace_less).to_owned();
    assert!(workspace_less["participant"]["workspace"].is_null());
    fs::write(
        sandbox.mail_root.join("rules.json"),
        r#"{"blocked":[{"from":"alpha","to":"*","reason":"all routes blocked"}]}"#,
    )
    .expect("block wildcard");
    let wildcard = sandbox.run_as_participant(
        &[
            "send",
            "--to",
            &format!("participant:{workspace_less_id}"),
            "--body",
            "must not land",
        ],
        &sender_id,
        &alpha,
    );
    let error: ErrorEnvelope = from_stderr(&wildcard);
    assert_eq!(error.error.code, "blocked_route");
}

#[test]
fn participant_round4_lineage_recipient_filtering_remains_pending_p2() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let sender = sandbox.bind_claude("lineage-pending-sender", &alpha, Some("alpha"));
    let sender_id = participant_id(&sender).to_owned();
    let lineage_dir = sandbox.mail_root.join("lineages/round4-lineage");
    fs::create_dir_all(&lineage_dir).expect("lineage dir");
    fs::write(
        lineage_dir.join("lineage.json"),
        r#"{"name":"round4-lineage","founder":"founder","created":"2026-09-16 00:00:00 +0000","host":"test"}"#,
    )
    .expect("lineage record");
    fs::write(
        sandbox.mail_root.join("rules.json"),
        r#"{"blocked":[{"from":"alpha","to":"beta","reason":"recipient workspace blocked"}]}"#,
    )
    .expect("recipient rule");

    // P.1 only holds lineage-addressed mail. P.2 must filter blocked affiliates
    // while freezing the receipt; this assertion prevents P.1 from pretending
    // it can decide before recipient selection exists.
    assert_success(&sandbox.run_as_participant(
        &[
            "send",
            "--to",
            "lineage:round4-lineage",
            "--body",
            "held for P.2 routing",
        ],
        &sender_id,
        &alpha,
    ));
}

#[test]
fn participant_round4_bound_sender_assertions_refuse_disagreement() {
    let sandbox = Sandbox::new();
    let (_alpha, beta) = register_alpha_beta(&sandbox);
    let bound = sandbox.bind_claude("sender-disagreement", &beta, Some("beta"));
    let actor = participant_id(&bound).to_owned();

    let flag = sandbox.run_as_participant(
        &[
            "send",
            "--to",
            "alpha",
            "--from",
            "alpha",
            "--body",
            "wrong flag",
        ],
        &actor,
        &beta,
    );
    let error: ErrorEnvelope = from_stderr(&flag);
    assert_eq!(error.error.code, "invalid_argument");
    assert!(error
        .error
        .message
        .contains("conflicts with bound participant"));

    let pin = sandbox.run_in_env(
        &["send", "--to", "alpha", "--body", "wrong pin"],
        None,
        &beta,
        &[("POST_PARTICIPANT", actor.as_str()), ("POST_FROM", "alpha")],
    );
    let error: ErrorEnvelope = from_stderr(&pin);
    assert_eq!(error.error.code, "invalid_argument");
    assert!(error.error.message.contains("workspace pin"));

    assert_success(&sandbox.run_as_participant(
        &["chat", "pin-conflict", "--join", "--json"],
        &actor,
        &beta,
    ));
    let channel = sandbox.run_in_env(
        &[
            "chat",
            "pin-conflict",
            "--send",
            "--anyway",
            "--body",
            "wrong channel pin",
        ],
        None,
        &beta,
        &[("POST_PARTICIPANT", actor.as_str()), ("POST_FROM", "alpha")],
    );
    let error: ErrorEnvelope = from_stderr(&channel);
    assert_eq!(error.error.code, "invalid_argument");
    assert!(error.error.message.contains("workspace pin"));
}

#[test]
fn participant_round4_bound_matching_cwd_still_uses_binding_provenance() {
    let sandbox = Sandbox::new();
    let (_alpha, beta) = register_alpha_beta(&sandbox);
    let bound = sandbox.bind_claude("matching-cwd-provenance", &beta, Some("beta"));
    let actor = participant_id(&bound).to_owned();
    let direct = sandbox.run_as_participant(
        &["send", "--to", "alpha", "--body", "bound", "--json"],
        &actor,
        &beta,
    );
    assert_success(&direct);
    let direct: Value = from_stdout(&direct);
    assert_eq!(
        direct["envelope"]["sender_provenance"],
        "participant-binding"
    );

    assert_success(&sandbox.run_as_participant(
        &["chat", "matching-cwd", "--join", "--json"],
        &actor,
        &beta,
    ));
    let channel = sandbox.run_as_participant(
        &[
            "chat",
            "matching-cwd",
            "--send",
            "--anyway",
            "--body",
            "bound channel",
            "--json",
        ],
        &actor,
        &beta,
    );
    assert_success(&channel);
    let channel: Value = from_stdout(&channel);
    assert_eq!(
        channel["message"]["sender_provenance"],
        "participant-binding"
    );
}

#[test]
fn participant_round4_native_keys_reject_empty_or_whitespace() {
    for variable in [
        "CLAUDE_CODE_SESSION_ID",
        "CODEX_THREAD_ID",
        "CODEX_SESSION_ID",
    ] {
        for value in ["", "   "] {
            let sandbox = Sandbox::new_unseeded();
            let output = sandbox.run_in_env(
                &["participant", "bind", "--json"],
                None,
                &sandbox.path,
                &[(variable, value)],
            );
            let error: ErrorEnvelope = from_stderr(&output);
            assert_eq!(error.error.code, "invalid_argument", "{variable}={value:?}");
            assert!(
                error.error.message.contains(variable),
                "{}",
                error.error.message
            );
            assert!(
                !sandbox.mail_root.join("participants").exists(),
                "{variable}={value:?} minted a participant"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn participant_round4_rooms_add_waits_for_participants_lock() {
    let sandbox = Sandbox::new();
    let room = sandbox.path.join("round4-room");
    fs::create_dir(&room).expect("room path");
    let lock_path = sandbox.mail_root.join(".participants.lock");
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&lock_path)
        .expect("open participants lock");
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);

    let mut child = common::post_command()
        .args([
            "rooms",
            "add",
            "round4-room",
            room.to_str().expect("UTF-8 room path"),
        ])
        .current_dir(&sandbox.path)
        .env_clear()
        .env("HOME", &sandbox.home)
        .env("POST_MAIL_ROOT", &sandbox.mail_root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn rooms add");
    for _ in 0..20 {
        if child.try_wait().expect("probe rooms add").is_some() {
            panic!("rooms add bypassed .participants.lock");
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) }, 0);
    let output = child.wait_with_output().expect("wait rooms add");
    assert!(
        output.status.success(),
        "rooms add failed after lock release: {}",
        common::stderr(&output)
    );
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
    assert_eq!(
        value["capabilities"],
        serde_json::json!(["participants", "lineages", "routing-receipts", "cursors-v2"])
    );
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
fn participant_identity_new_rejects_lineage_that_imitates_a_workspace() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let actor = sandbox.test_participant("alpha");

    let output =
        sandbox.run_as_participant(&["identity", "new", "A l p h a", "--json"], &actor, &alpha);

    let error: ErrorEnvelope = from_stderr(&output);
    assert_eq!(error.error.code, "invalid_argument");
    assert!(
        error.error.message.contains("imitates")
            && error.error.message.contains("workspace 'alpha'"),
        "unexpected error: {}",
        error.error.message
    );
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
        if args == ["doctor"] {
            assert_eq!(output.status.code(), Some(1));
        } else {
            assert_success(&output);
        }
        let value: Value = from_stdout(&output);
        if args != ["doctor"] {
            assert_eq!(value["ok"], true, "{args:?}: {}", common::stdout(&output));
        }
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

    let malformed = Sandbox::new();
    let record = malformed
        .mail_root
        .join("participants/broken/participant.json");
    fs::create_dir_all(record.parent().expect("record parent")).expect("participant dir");
    fs::write(&record, b"{not json").expect("malformed participant");
    let before = tree(&malformed.mail_root);
    let who = malformed.run_in_env(
        &["who"],
        None,
        &malformed.path,
        &[("POST_PARTICIPANT", "broken")],
    );
    assert!(who.status.success());
    assert!(common::stderr(&who).contains("skipped corrupt participant"));
    let who: Value = from_stdout(&who);
    assert!(who["participant_error"]
        .as_str()
        .is_some_and(|message| message.contains("invalid participant JSON")));
    assert_eq!(tree(&malformed.mail_root), before);
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
        (vec!["rooms".to_owned()], None),
        (vec!["version".to_owned(), "--json".to_owned()], None),
        (
            vec![
                "watch".to_owned(),
                "--snapshot".to_owned(),
                "--room".to_owned(),
                "alpha".to_owned(),
            ],
            None,
        ),
        (
            vec![
                "read".to_owned(),
                sent_mail.envelope.id,
                "--room".to_owned(),
                "alpha".to_owned(),
                "--peek".to_owned(),
                "--json".to_owned(),
            ],
            None,
        ),
        (vec!["profile".to_owned(), "show".to_owned()], None),
        (
            vec![
                "search".to_owned(),
                "needle".to_owned(),
                "--channel".to_owned(),
                "round2-read".to_owned(),
                "--json".to_owned(),
            ],
            None,
        ),
        (
            vec![
                "chat".to_owned(),
                "round2-read".to_owned(),
                "--peek".to_owned(),
                "--json".to_owned(),
            ],
            Some("not_a_member"),
        ),
        (
            vec![
                "chat".to_owned(),
                "round2-read".to_owned(),
                "--history".to_owned(),
                "1".to_owned(),
                "--json".to_owned(),
            ],
            Some("not_a_member"),
        ),
        (
            vec![
                "chat".to_owned(),
                "round2-read".to_owned(),
                "--since".to_owned(),
                channel_id.clone(),
                "--json".to_owned(),
            ],
            Some("not_a_member"),
        ),
        (
            vec![
                "chat".to_owned(),
                "round2-read".to_owned(),
                "--seen-by".to_owned(),
                channel_id.clone(),
                "--json".to_owned(),
            ],
            Some("not_a_member"),
        ),
        (
            vec![
                "chat".to_owned(),
                "round2-read".to_owned(),
                "--message".to_owned(),
                channel_id,
                "--max-bytes".to_owned(),
                "4096".to_owned(),
                "--json".to_owned(),
            ],
            Some("not_a_member"),
        ),
    ];
    for (command, expected_error) in commands {
        let args = command.iter().map(String::as_str).collect::<Vec<_>>();
        let before = tree(&sandbox.mail_root);
        let output = sandbox.run_without_identity(&args, &alpha);
        if let Some(expected) = expected_error {
            let error: ErrorEnvelope = from_stderr(&output);
            assert_eq!(error.error.code, expected, "{args:?}");
        } else {
            assert_success(&output);
        }
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
    assert_eq!(error.error.code, "not_a_member");
    assert!(error.error.message.contains(&actor));
    assert!(error
        .error
        .suggested_fix
        .contains("post chat 'any-channel' --join"));
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

#[test]
fn participant_lifecycle_touch_end_and_bind_reactivation() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let bound = sandbox.bind_claude("lifecycle-session", &alpha, Some("alpha"));
    let id = participant_id(&bound).to_owned();
    let initial = sandbox.read_participant(&id);
    let initial_seen = initial["last_seen"].as_str().expect("last_seen");
    assert_eq!(initial_seen.len(), 20);
    assert_eq!(&initial_seen[10..11], "T");
    assert!(initial_seen.ends_with('Z'));
    assert_eq!(initial["lease_hours"], 24);
    assert!(initial["ended_at"].is_null());

    edit_participant(&sandbox, &id, |record| {
        record.insert(
            "last_seen".to_owned(),
            Value::String("2020-01-01T00:00:00Z".to_owned()),
        );
        record.insert("lease_hours".to_owned(), Value::from(24));
        record.insert("lineage".to_owned(), Value::String("ember".to_owned()));
        record.insert(
            "lineage_since".to_owned(),
            Value::String("2026-09-16T00:00:00Z".to_owned()),
        );
    });
    let touched = sandbox.run_in_env(
        &["participant", "touch", "--json"],
        None,
        &alpha,
        &[
            ("POST_PARTICIPANT", &id),
            ("POST_PARTICIPANT_LEASE_HOURS", "7"),
        ],
    );
    assert_success(&touched);
    let touched: Value = from_stdout(&touched);
    assert_ne!(touched["participant"]["last_seen"], "2020-01-01T00:00:00Z");
    assert_eq!(touched["participant"]["lease_hours"], 7);

    let ended = sandbox.run_as_participant(&["participant", "end", "--json"], &id, &alpha);
    assert_success(&ended);
    let ended: Value = from_stdout(&ended);
    assert!(ended["participant"]["ended_at"].as_str().is_some());
    let record = sandbox
        .mail_root
        .join("participants")
        .join(&id)
        .join("participant.json");
    let ended_bytes = fs::read(&record).expect("ended participant bytes");
    let ended_again = sandbox.run_as_participant(&["participant", "end", "--json"], &id, &alpha);
    assert_success(&ended_again);
    assert_eq!(
        fs::read(&record).expect("idempotent end bytes"),
        ended_bytes,
        "end must preserve its first ended_at"
    );

    let ended_who = sandbox.run_as_participant(&["who"], &id, &alpha);
    assert_success(&ended_who);
    let ended_who: Value = from_stdout(&ended_who);
    assert_eq!(ended_who["participant"]["state"], "ended");

    let rebound = sandbox.run_in_env(
        &["participant", "bind", "--json"],
        None,
        &beta,
        &[
            ("CLAUDE_CODE_SESSION_ID", "lifecycle-session"),
            ("POST_PARTICIPANT_LEASE_HOURS", "5"),
        ],
    );
    assert_success(&rebound);
    let rebound: Value = from_stdout(&rebound);
    assert_eq!(participant_id(&rebound), id);
    assert!(rebound["participant"]["ended_at"].is_null());
    assert_eq!(rebound["participant"]["lease_hours"], 5);
    assert_eq!(rebound["participant"]["lineage"], "ember");
    assert_eq!(rebound["participant"]["workspace"], "alpha");
    assert_ne!(rebound["participant"]["last_seen"], "2020-01-01T00:00:00Z");
}

#[test]
fn participant_touch_preserves_recorded_lease_unless_env_explicitly_overrides_it() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let bound = sandbox.bind_claude("lease-preservation", &alpha, Some("alpha"));
    let id = participant_id(&bound).to_owned();
    edit_participant(&sandbox, &id, |record| {
        record.insert(
            "last_seen".to_owned(),
            Value::String("2020-01-01T00:00:00Z".to_owned()),
        );
        record.insert("lease_hours".to_owned(), Value::from(7));
    });

    let preserved = sandbox.run_as_participant(&["participant", "touch", "--json"], &id, &alpha);
    assert_success(&preserved);
    let preserved: Value = from_stdout(&preserved);
    assert_eq!(preserved["participant"]["lease_hours"], 7);
    assert_ne!(
        preserved["participant"]["last_seen"],
        "2020-01-01T00:00:00Z"
    );

    let overridden = sandbox.run_in_env(
        &["participant", "touch", "--json"],
        None,
        &alpha,
        &[
            ("POST_PARTICIPANT", &id),
            ("POST_PARTICIPANT_LEASE_HOURS", "3"),
        ],
    );
    assert_success(&overridden);
    let overridden: Value = from_stdout(&overridden);
    assert_eq!(overridden["participant"]["lease_hours"], 3);

    edit_participant(&sandbox, &id, |record| {
        record.insert("lease_hours".to_owned(), Value::from(7));
    });
    let sent = sandbox.run_as_participant(
        &[
            "send",
            "--to",
            "workspace:beta",
            "--body",
            "refresh without lease override",
            "--json",
        ],
        &id,
        &alpha,
    );
    assert_success(&sent);
    assert_eq!(sandbox.read_participant(&id)["lease_hours"], 7);

    let rebound = sandbox.bind_claude("lease-preservation", &alpha, Some("alpha"));
    assert_eq!(participant_id(&rebound), id);
    assert_eq!(rebound["participant"]["lease_hours"], 7);
}

#[test]
fn participant_lifecycle_missing_lease_is_stale_until_rebind() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let bound = sandbox.bind_claude("missing-lease-rebind", &alpha, Some("alpha"));
    let id = participant_id(&bound).to_owned();
    edit_participant(&sandbox, &id, |record| {
        record.remove("last_seen");
        record.insert("lineage".to_owned(), Value::String("ember".to_owned()));
    });

    let before = sandbox.run_as_participant(&["who"], &id, &alpha);
    assert_success(&before);
    let before: Value = from_stdout(&before);
    assert_eq!(before["participant"]["state"], "no lease record");

    let rebound = sandbox.run_as_claude(
        &["participant", "bind", "--json"],
        "missing-lease-rebind",
        &alpha,
    );
    assert_success(&rebound);
    let rebound: Value = from_stdout(&rebound);
    assert_eq!(participant_id(&rebound), id);
    assert_eq!(rebound["participant"]["workspace"], "alpha");
    assert_eq!(rebound["participant"]["lineage"], "ember");
    assert!(rebound["participant"]["last_seen"].as_str().is_some());

    let after = sandbox.run_as_participant(&["who"], &id, &alpha);
    assert_success(&after);
    let after: Value = from_stdout(&after);
    assert_eq!(after["participant"]["state"], "active");
}

#[test]
fn participant_lifecycle_malformed_timestamps_are_skipped_and_doctor_names_them() {
    for (suffix, timestamp) in [
        ("letter", "2026-09-16T0x:00:00Z"),
        ("low-byte", "2026-09-16T0/:00:00Z"),
    ] {
        let sandbox = Sandbox::new();
        let (alpha, _beta) = register_alpha_beta(&sandbox);
        let key = format!("malformed-lifecycle-time-{suffix}");
        let bound = sandbox.bind_claude(&key, &alpha, Some("alpha"));
        let id = participant_id(&bound).to_owned();
        edit_participant(&sandbox, &id, |record| {
            record.insert("last_seen".to_owned(), Value::String(timestamp.to_owned()));
        });

        let output = sandbox.run_as_participant(&["participant", "list"], &id, &alpha);
        assert_eq!(output.status.code(), Some(0));
        assert!(common::stderr(&output).contains("skipped corrupt participant"));
        assert!(common::stderr(&output).contains("RFC3339"));
        let listed: Value = from_stdout(&output);
        assert!(!listed["participants"]
            .as_array()
            .expect("participants")
            .iter()
            .any(|participant| participant["id"] == id));

        let doctor = sandbox.run_as_participant(&["doctor"], &id, &alpha);
        assert_eq!(doctor.status.code(), Some(1));
        let doctor: Value = from_stdout(&doctor);
        assert!(doctor["checks"]
            .as_array()
            .expect("checks")
            .iter()
            .any(|check| check["id"] == format!("participant.{id}.invalid")
                && check["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("RFC3339"))));
    }
}

#[test]
fn participant_lifecycle_central_writer_refresh_and_read_only_stability() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let bound = sandbox.bind_claude("central-lifecycle", &alpha, Some("alpha"));
    let id = participant_id(&bound).to_owned();

    edit_participant(&sandbox, &id, |record| {
        record.insert(
            "last_seen".to_owned(),
            Value::String("2020-01-01T00:00:00Z".to_owned()),
        );
    });
    let send = sandbox.run_in_env(
        &["send", "--to", "beta", "--body", "refresh", "--json"],
        None,
        &alpha,
        &[
            ("POST_PARTICIPANT", &id),
            ("POST_PARTICIPANT_LEASE_HOURS", "3"),
        ],
    );
    assert_success(&send);
    let refreshed = sandbox.read_participant(&id);
    assert_ne!(refreshed["last_seen"], "2020-01-01T00:00:00Z");
    assert_eq!(refreshed["lease_hours"], 3);

    edit_participant(&sandbox, &id, |record| {
        record.insert(
            "last_seen".to_owned(),
            Value::String("2020-01-01T00:00:00Z".to_owned()),
        );
    });
    let slice_id = "20260916-040002-a1b2c5";
    write_custom_mail(
        &sandbox.mail_root.join("alpha/inbox"),
        slice_id,
        &serde_json::json!({
            "id": slice_id,
            "from": "beta",
            "to": "alpha",
            "kind": "note",
            "subject": "lifecycle slice",
            "sent": "2026-09-16 04:00:02 -0500"
        }),
        "lifecycle body",
    );
    let message = sandbox
        .mail_root
        .join("alpha/inbox")
        .join(format!("{slice_id}.mail"));
    let receipt_dir = sandbox.mail_root.join("alpha/routing");
    fs::create_dir_all(&receipt_dir).expect("routing receipt directory");
    let digest = format!(
        "{:x}",
        Sha256::digest(fs::read(&message).expect("canonical mail"))
    );
    fs::write(
        receipt_dir.join(format!("{slice_id}.json")),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&serde_json::json!({
                "version": 1,
                "message": slice_id,
                "digest": digest,
                "address": {"kind": "workspace", "name": "alpha"},
                "recipients": [id],
                "routed_at": "2026-09-16T04:00:02Z",
                "routed_by": "test"
            }))
            .expect("serialize routing receipt")
        ),
    )
    .expect("write routing receipt");
    for (args, expected) in [
        (vec!["participant", "show"], None),
        (vec!["participant", "list"], None),
        (vec!["who"], None),
        (vec!["rooms"], None),
        (vec!["channels"], None),
        (vec!["inbox", "--room", "alpha"], None),
        (vec!["doctor"], Some("doctor_findings")),
        (vec!["schema"], None),
        (vec!["version", "--json"], None),
        (
            vec!["read", "missing", "--room", "alpha", "--peek"],
            Some("not_found"),
        ),
        (
            vec![
                "read",
                slice_id,
                "--room",
                "alpha",
                "--offset",
                "0",
                "--length",
                "4",
                "--max-bytes",
                "4096",
                "--json",
            ],
            None,
        ),
        (vec!["chat", "missing", "--peek"], Some("not_a_member")),
        (vec!["watch", "--snapshot", "--room", "alpha"], None),
        (vec!["profile", "show", "alpha"], None),
        (vec!["owner", "show"], None),
        (vec!["search", "needle", "--mail", "--json"], None),
    ] {
        let before = tree(&sandbox.mail_root);
        let output = sandbox.run_as_participant(&args, &id, &alpha);
        match expected {
            None => assert_success(&output),
            Some("doctor_findings") => {
                assert_eq!(output.status.code(), Some(1), "{args:?}");
                let value: Value = from_stdout(&output);
                assert_eq!(value["ok"], false, "{args:?}");
            }
            Some(code) => {
                let error: ErrorEnvelope = from_stderr(&output);
                assert_eq!(error.error.code, code, "{args:?}");
            }
        }
        assert_eq!(tree(&sandbox.mail_root), before, "read mutated: {args:?}");
    }
    assert_eq!(
        sandbox.read_participant(&id)["last_seen"],
        "2020-01-01T00:00:00Z"
    );
}

#[test]
fn participant_lifecycle_who_reports_active_stale_ended_and_crash_gap() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let active = sandbox.bind_claude("who-active", &alpha, Some("alpha"));
    let stale = sandbox.bind_claude("who-stale", &alpha, Some("alpha"));
    let ended = sandbox.bind_claude("who-ended", &alpha, Some("alpha"));
    let active_id = participant_id(&active).to_owned();
    let stale_id = participant_id(&stale).to_owned();
    let ended_id = participant_id(&ended).to_owned();
    let missing_lease_id = sandbox.test_participant("alpha");
    edit_participant(&sandbox, &missing_lease_id, |record| {
        record.remove("last_seen");
    });
    edit_participant(&sandbox, &stale_id, |record| {
        record.insert(
            "last_seen".to_owned(),
            Value::String("2020-01-01T00:00:00Z".to_owned()),
        );
        record.insert("lease_hours".to_owned(), Value::from(1));
    });
    edit_participant(&sandbox, &ended_id, |record| {
        record.insert(
            "ended_at".to_owned(),
            Value::String("2026-09-16T00:00:00Z".to_owned()),
        );
    });

    let output = sandbox.run_as_participant(&["who"], &active_id, &alpha);
    assert_success(&output);
    let who: Value = from_stdout(&output);
    assert_eq!(who["participant"]["state"], "active");
    assert!(who["participant"]["last_seen"].as_str().is_some());
    let rows = who["participants"]
        .as_array()
        .expect("participant rows")
        .iter()
        .map(|row| (row["id"].as_str().expect("row id"), row))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(rows[active_id.as_str()]["state"], "active");
    assert_eq!(rows[stale_id.as_str()]["state"], "stale");
    assert_eq!(rows[ended_id.as_str()]["state"], "ended");
    assert_eq!(rows[missing_lease_id.as_str()]["state"], "no lease record");
    assert!(rows[missing_lease_id.as_str()]["last_seen"].is_null());
    assert!(who["activity_note"]
        .as_str()
        .is_some_and(|note| note.contains("not reassigned")));

    let doctor = sandbox.run_as_participant(&["doctor"], &active_id, &alpha);
    let doctor: Value = from_stdout(&doctor);
    let messages = doctor["checks"]
        .as_array()
        .expect("doctor checks")
        .iter()
        .filter_map(|check| check["message"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(messages.contains(&stale_id), "{messages}");
    assert!(messages.contains(&missing_lease_id), "{messages}");
    assert!(messages.contains("no lease record"), "{messages}");
    assert!(messages.contains("not reassigned"), "{messages}");
}

#[test]
fn participant_lifecycle_validates_renewal_lease_but_end_preserves_it() {
    let unbound = Sandbox::new_unseeded();
    for command in ["touch", "end"] {
        let output = unbound.run_as_claude(
            &["participant", command, "--json"],
            "bindable-lifecycle-key",
            &unbound.path,
        );
        let error: ErrorEnvelope = from_stderr(&output);
        assert_eq!(error.error.code, "no_participant");
        assert_eq!(
            error.error.details.exact_fix.as_deref(),
            Some("post participant bind")
        );
    }

    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let bound = sandbox.bind_claude("invalid-lease", &alpha, Some("alpha"));
    let id = participant_id(&bound).to_owned();
    let path = sandbox
        .mail_root
        .join("participants")
        .join(&id)
        .join("participant.json");
    let before = fs::read(&path).expect("participant before invalid touch");
    let output = sandbox.run_in_env(
        &["participant", "touch", "--json"],
        None,
        &alpha,
        &[
            ("POST_PARTICIPANT", &id),
            ("POST_PARTICIPANT_LEASE_HOURS", "0"),
        ],
    );
    let error: ErrorEnvelope = from_stderr(&output);
    assert_eq!(error.error.code, "invalid_argument");
    assert!(error.error.message.contains("positive integer"));
    assert_eq!(
        fs::read(&path).expect("participant after invalid touch"),
        before
    );

    let ended = sandbox.run_in_env(
        &["participant", "end", "--json"],
        None,
        &alpha,
        &[
            ("POST_PARTICIPANT", &id),
            ("POST_PARTICIPANT_LEASE_HOURS", "bogus"),
        ],
    );
    assert_success(&ended);
    let ended: Value = from_stdout(&ended);
    assert_eq!(ended["participant"]["lease_hours"], 24);
    assert!(ended["participant"]["ended_at"].as_str().is_some());
    assert_eq!(sandbox.read_participant(&id)["lease_hours"], 24);
}

/// 2026-09-22 identity-collision regression (Vale's probe): a profile belongs
/// to one participant. Asserts on ENVELOPE fields (display_name/pfp) for both
/// channel messages and direct mail, not on rendered text.
#[test]
fn participant_profile_belongs_to_the_participant_not_the_workspace() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let fire = sandbox.bind_claude("profile-fire", &alpha, Some("alpha"));
    let fire = participant_id(&fire).to_owned();
    let vale = sandbox.bind_codex("profile-vale", &alpha, Some("alpha"));
    let vale = participant_id(&vale).to_owned();
    let reader = sandbox.bind_claude("profile-reader", &alpha, Some("alpha"));
    let reader = participant_id(&reader).to_owned();
    for participant in [&fire, &vale, &reader] {
        assert_success(&sandbox.run_as_participant(
            &["chat", "faces", "--join", "--json"],
            participant,
            &alpha,
        ));
    }
    // Legacy workspace-keyed entry planted for alpha: nobody inherits it.
    std::fs::write(
        sandbox.mail_root.join("profiles.json"),
        r#"{"alpha": {"name": "Shared Persona", "pfp": "👻"}}"#,
    )
    .expect("legacy profiles.json");

    let set = sandbox.run_as_participant(
        &["profile", "set", "--name", "Fire Fable", "--pfp", "🔥"],
        &fire,
        &alpha,
    );
    assert_success(&set);
    let set: Value = from_stdout(&set);
    assert_eq!(set["participant"], fire.as_str());
    assert_eq!(set["key"], format!("participant:{fire}"));
    assert_eq!(
        set["retired_legacy_entry"], "alpha",
        "set from the workspace retires the legacy entry"
    );

    let send = |participant: &str, body: &str| -> Value {
        let out = sandbox.run_as_participant(
            &[
                "chat", "faces", "--send", "--anyway", "--body", body, "--json",
            ],
            participant,
            &alpha,
        );
        assert_success(&out);
        from_stdout(&out)
    };
    let mail = |participant: &str, body: &str| -> Value {
        let out = sandbox.run_as_participant(
            &[
                "send",
                "--to",
                &format!("participant:{reader}"),
                "--body",
                body,
                "--json",
            ],
            participant,
            &alpha,
        );
        assert_success(&out);
        from_stdout(&out)
    };

    let fire_chat = send(&fire, "fire speaks");
    assert_eq!(fire_chat["message"]["display_name"], "Fire Fable");
    assert_eq!(fire_chat["message"]["pfp"], "🔥");
    let vale_chat = send(&vale, "vale speaks");
    assert!(
        vale_chat["message"]["display_name"].is_null(),
        "peer profile leaked into channel stamp: {vale_chat}"
    );
    assert!(vale_chat["message"]["pfp"].is_null());
    let vale_mail = mail(&vale, "vale mails");
    assert!(
        vale_mail["envelope"]["display_name"].is_null(),
        "peer profile leaked into mail stamp: {vale_mail}"
    );
    assert!(vale_mail["envelope"]["pfp"].is_null());
    let fire_mail = mail(&fire, "fire mails");
    assert_eq!(fire_mail["envelope"]["display_name"], "Fire Fable");

    // Vale sets its own; Fire's stamp is unchanged. Vale clears; Fire's stays.
    assert_success(&sandbox.run_as_participant(
        &["profile", "set", "--name", "Vale", "--pfp", "🌾"],
        &vale,
        &alpha,
    ));
    assert_eq!(send(&vale, "vale again")["message"]["display_name"], "Vale");
    assert_eq!(
        send(&fire, "fire again")["message"]["display_name"],
        "Fire Fable"
    );
    assert_success(&sandbox.run_as_participant(&["profile", "clear"], &vale, &alpha));
    assert!(send(&vale, "vale cleared")["message"]["display_name"].is_null());
    assert_eq!(
        send(&fire, "fire after peer clear")["message"]["display_name"],
        "Fire Fable"
    );
    // History is as-sent: the first Vale message still has no stamp, Fire's first keeps its name.
    let first_id = fire_chat["message"]["id"].as_str().expect("id").to_owned();
    let history = sandbox.run_as_participant(
        &["chat", "faces", "--history", "50", "--json"],
        &reader,
        &alpha,
    );
    assert_success(&history);
    let history: Value = from_stdout(&history);
    let first = history["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|m| m["id"] == first_id.as_str())
        .expect("first");
    assert_eq!(first["display_name"], "Fire Fable");

    // A participant with NO workspace (bound from an unregistered cwd) can set
    // a profile and it stamps.
    let nowhere = sandbox.mail_root.join("nowhere");
    std::fs::create_dir_all(&nowhere).expect("unregistered cwd");
    let solo = sandbox.bind_codex("profile-solo", &nowhere, None);
    assert!(
        solo["participant"]["workspace"].is_null(),
        "solo must be session-only: {solo}"
    );
    let solo = participant_id(&solo).to_owned();
    assert_success(&sandbox.run_as_participant(
        &["chat", "faces", "--join", "--json"],
        &solo,
        &nowhere,
    ));
    assert_success(&sandbox.run_as_participant(
        &["profile", "set", "--name", "Solo", "--pfp", "🧭"],
        &solo,
        &nowhere,
    ));
    let solo_out = sandbox.run_as_participant(
        &[
            "chat",
            "faces",
            "--send",
            "--anyway",
            "--body",
            "solo speaks",
            "--json",
        ],
        &solo,
        &nowhere,
    );
    assert_success(&solo_out);
    let solo_chat: Value = from_stdout(&solo_out);
    assert_eq!(solo_chat["message"]["display_name"], "Solo");
    assert_eq!(solo_chat["message"]["pfp"], "🧭");
    assert_eq!(
        solo_chat["message"]["from"],
        solo.as_str(),
        "session-only reply address is the participant id"
    );
    // profile show <participant-id> and show participant:<id> resolve the same entry.
    let shown = sandbox.run_as_participant(&["profile", "show", &solo], &reader, &alpha);
    assert_success(&shown);
    let shown: Value = from_stdout(&shown);
    assert_eq!(shown["profile"]["name"], "Solo");
    assert_eq!(shown["participant"], solo.as_str());
}

/// A5: `profile list` reports sigil occupancy with the same predicate
/// `profile set` refuses on. Every entry the listing marks `holds_sigil` is
/// refused to another participant, every entry it does not is accepted, and a
/// holder whose lease lapses flips both at once.
#[test]
fn profile_list_agrees_with_the_sigil_uniqueness_refusal() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let fire = sandbox.bind_claude("list-fire", &alpha, Some("alpha"));
    let fire = participant_id(&fire).to_owned();
    let vale = sandbox.bind_codex("list-vale", &alpha, Some("alpha"));
    let vale = participant_id(&vale).to_owned();
    assert_success(&sandbox.run_as_participant(
        &["profile", "set", "--name", "Fire", "--pfp", "🔥"],
        &fire,
        &alpha,
    ));
    // Legacy entries: `beta` is a registered room (holds its sigil), `ghost`
    // is not registered (never renders, so never holds one).
    let profiles_path = sandbox.mail_root.join("profiles.json");
    let mut profiles: Value =
        serde_json::from_slice(&fs::read(&profiles_path).expect("profiles")).expect("JSON");
    profiles["beta"] = serde_json::json!({"name": "Beta Room", "pfp": "👻"});
    profiles["ghost"] = serde_json::json!({"pfp": "👾"});
    fs::write(
        &profiles_path,
        serde_json::to_vec_pretty(&profiles).unwrap(),
    )
    .expect("plant");

    let list = |who: &str| -> Vec<Value> {
        let out = sandbox.run_as_participant(&["profile", "list", "--json"], who, &alpha);
        assert_success(&out);
        let out: Value = from_stdout(&out);
        assert_eq!(out["ok"], true);
        out["profiles"].as_array().expect("profiles").clone()
    };
    let entry = |entries: &[Value], key: &str| -> Value {
        entries
            .iter()
            .find(|entry| entry["key"] == key)
            .unwrap_or_else(|| panic!("no {key} in {entries:?}"))
            .clone()
    };
    let try_set =
        |pfp: &str| sandbox.run_as_participant(&["profile", "set", "--pfp", pfp], &vale, &alpha);

    let entries = list(&vale);
    let fire_key = format!("participant:{fire}");
    let fire_entry = entry(&entries, &fire_key);
    assert_eq!(fire_entry["participant"], fire.as_str());
    assert_eq!(fire_entry["workspace"], "alpha");
    assert_eq!(fire_entry["name"], "Fire");
    assert_eq!(fire_entry["pfp"], "🔥");
    assert_eq!(fire_entry["lease"], "active");
    assert_eq!(fire_entry["holds_sigil"], true);
    let beta_entry = entry(&entries, "beta");
    assert_eq!(beta_entry["legacy"], true);
    assert!(beta_entry.get("lease").is_none());
    assert_eq!(beta_entry["holds_sigil"], true);
    assert_eq!(entry(&entries, "ghost")["holds_sigil"], false);

    for (pfp, holder) in [("🔥", fire_key.as_str()), ("👻", "beta")] {
        let refused = try_set(pfp);
        assert_eq!(refused.status.code(), Some(2), "{pfp} must be refused");
        assert!(
            common::stderr(&refused).contains(holder),
            "refusal names the listed holder {holder}: {}",
            common::stderr(&refused)
        );
    }
    assert_success(&try_set("👾"));

    // Fire's lease lapses: the listing releases the sigil and set agrees.
    let record_path = sandbox
        .mail_root
        .join("participants")
        .join(&fire)
        .join("participant.json");
    let mut record: Value =
        serde_json::from_slice(&fs::read(&record_path).expect("record")).expect("JSON");
    record["last_seen"] = Value::String("2000-01-01T00:00:00Z".to_owned());
    fs::write(&record_path, serde_json::to_vec_pretty(&record).unwrap()).expect("age fire");
    let fire_entry = entry(&list(&vale), &fire_key);
    assert_eq!(fire_entry["lease"], "stale");
    assert_eq!(fire_entry["holds_sigil"], false);
    assert_success(&try_set("🔥"));

    let text = sandbox.run_as_participant(&["profile", "list"], &vale, &alpha);
    assert_success(&text);
    let text = common::stdout(&text);
    assert!(
        text.contains(&format!("profile participant:{vale}  name=none  pfp=🔥  workspace=alpha  lease=active  holds-sigil=yes")),
        "{text}"
    );
    assert!(
        text.contains(
            "profile beta  name=Beta Room  pfp=👻  workspace=beta  lease=legacy  holds-sigil=yes"
        ),
        "{text}"
    );
    assert!(
        text.contains("note: sigil occupancy is lease-dependent"),
        "{text}"
    );
}
