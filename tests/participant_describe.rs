//! `post participant describe`: a session records the model, effort, and
//! directory it runs under, as a `runtime` object on its participant record.

mod common;

use common::{
    assert_migration_refused, assert_success, from_stderr, from_stdout, register_alpha_beta,
    seed_fence_store, Sandbox,
};
use post::output::ErrorEnvelope;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

fn record_path(sandbox: &Sandbox, id: &str) -> PathBuf {
    sandbox
        .mail_root
        .join("participants")
        .join(id)
        .join("participant.json")
}

/// A participant bound in `alpha`, with its id and a way to run as it.
fn bound(sandbox: &Sandbox, key: &str) -> (String, PathBuf) {
    let (alpha, _beta) = register_alpha_beta(sandbox);
    let id = sandbox.bind_claude(key, &alpha, Some("alpha"))["participant"]["id"]
        .as_str()
        .expect("participant id")
        .to_owned();
    (id, alpha)
}

fn describe(sandbox: &Sandbox, id: &str, cwd: &Path, args: &[&str]) -> std::process::Output {
    let mut full = vec!["participant", "describe"];
    full.extend_from_slice(args);
    sandbox.run_as_participant(&full, id, cwd)
}

fn describe_json(sandbox: &Sandbox, id: &str, cwd: &Path, args: &[&str]) -> Value {
    let mut full = args.to_vec();
    full.push("--json");
    let output = describe(sandbox, id, cwd, &full);
    assert_success(&output);
    from_stdout(&output)
}

fn error_of(output: &std::process::Output) -> ErrorEnvelope {
    assert!(!output.status.success(), "expected a refusal");
    from_stderr(output)
}

fn is_rfc3339_utc(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 20
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[10] == b'T'
        && bytes[13] == b':'
        && bytes[16] == b':'
        && bytes[19] == b'Z'
}

#[test]
fn describe_sets_merges_and_clears_runtime() {
    let sandbox = Sandbox::new();
    let (id, alpha) = bound(&sandbox, "describe-merge");
    assert!(
        sandbox.read_participant(&id).get("runtime").is_none(),
        "a participant that never described itself has no runtime member"
    );

    let first = describe_json(
        &sandbox,
        &id,
        &alpha,
        &[
            "--model",
            "claude-opus-5-5",
            "--effort",
            "high",
            "--cwd",
            "/work/porch",
        ],
    );
    assert_eq!(first["ok"], true);
    assert_eq!(first["id"], id);
    assert!(
        first.get("status").is_none() && first.get("provenance").is_none(),
        "the answer is exactly {{ok, id, participant}}: {first}"
    );
    let runtime = &first["participant"]["runtime"];
    assert_eq!(runtime["model"], "claude-opus-5-5");
    assert_eq!(runtime["effort"], "high");
    assert_eq!(runtime["cwd"], "/work/porch");
    assert!(
        is_rfc3339_utc(runtime["updated"].as_str().expect("updated")),
        "updated is RFC3339 UTC: {runtime}"
    );
    assert_eq!(sandbox.read_participant(&id)["runtime"], *runtime);

    // A flag given replaces that one field; the others keep their value.
    let second = describe_json(&sandbox, &id, &alpha, &["--effort", "low"]);
    let runtime = &second["participant"]["runtime"];
    assert_eq!(runtime["model"], "claude-opus-5-5");
    assert_eq!(runtime["effort"], "low");
    assert_eq!(runtime["cwd"], "/work/porch");
    assert_eq!(sandbox.read_participant(&id)["runtime"], *runtime);

    // Fields can arrive one at a time on a fresh record: absent stays absent.
    let other = sandbox.bind_claude("describe-single", &alpha, Some("alpha"))["participant"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let single = describe_json(&sandbox, &other, &alpha, &["--cwd", "/only/cwd"]);
    let runtime = single["participant"]["runtime"]
        .as_object()
        .expect("runtime object");
    assert_eq!(runtime["cwd"], "/only/cwd");
    assert!(!runtime.contains_key("model") && !runtime.contains_key("effort"));

    // --clear removes the whole object, and clearing again is still ok.
    let cleared = describe_json(&sandbox, &id, &alpha, &["--clear"]);
    assert_eq!(cleared["ok"], true);
    assert!(cleared["participant"].get("runtime").is_none());
    assert!(sandbox.read_participant(&id).get("runtime").is_none());
    let again = describe_json(&sandbox, &id, &alpha, &["--clear"]);
    assert!(again["participant"].get("runtime").is_none());

    // Set after a clear starts from nothing, not from the old values.
    let fresh = describe_json(&sandbox, &id, &alpha, &["--model", "m2"]);
    let runtime = fresh["participant"]["runtime"]
        .as_object()
        .expect("runtime");
    assert_eq!(runtime["model"], "m2");
    assert!(!runtime.contains_key("effort") && !runtime.contains_key("cwd"));
}

#[test]
fn describe_text_is_one_confirmation_line() {
    let sandbox = Sandbox::new();
    let (id, alpha) = bound(&sandbox, "describe-text");
    let output = describe(
        &sandbox,
        &id,
        &alpha,
        &["--model", "opus", "--cwd", "/work/porch"],
    );
    assert_success(&output);
    let text = common::stdout(&output);
    assert_eq!(text.lines().count(), 1, "{text:?}");
    assert!(text.contains(&id) && text.contains("model=opus"), "{text}");
    assert!(text.contains("cwd=/work/porch"), "{text}");
    assert!(common::stderr(&output).is_empty(), "silent on stderr too");

    let cleared = describe(&sandbox, &id, &alpha, &["--clear"]);
    assert_success(&cleared);
    assert!(common::stdout(&cleared).contains("cleared"));
}

#[test]
fn describe_refuses_bad_input_and_writes_nothing() {
    let sandbox = Sandbox::new();
    let (id, alpha) = bound(&sandbox, "describe-refuse");
    describe_json(&sandbox, &id, &alpha, &["--model", "keep-me"]);
    let path = record_path(&sandbox, &id);
    let before = fs::read(&path).expect("record before refusals");

    let too_long = "x".repeat(65);
    let long_path = format!("/{}", "a".repeat(4096));
    let cases: Vec<(&str, Vec<&str>)> = vec![
        ("no flags at all", vec![]),
        ("--clear with --model", vec!["--clear", "--model", "m"]),
        ("--clear with --effort", vec!["--clear", "--effort", "e"]),
        ("--clear with --cwd", vec!["--clear", "--cwd", "/x"]),
        ("empty model", vec!["--model", ""]),
        ("empty effort", vec!["--effort", ""]),
        ("65-char model", vec!["--model", &too_long]),
        ("65-char effort", vec!["--effort", &too_long]),
        ("control char in model", vec!["--model", "a\u{7}b"]),
        ("newline in effort", vec!["--effort", "hi\nthere"]),
        ("relative cwd", vec!["--cwd", "work/porch"]),
        ("empty cwd", vec!["--cwd", ""]),
        ("control char in cwd", vec!["--cwd", "/work/\tporch"]),
        ("cwd over 4096 bytes", vec!["--cwd", &long_path]),
        // A valid field next to an invalid one must not half-apply.
        (
            "valid model, bad cwd",
            vec!["--model", "new", "--cwd", "rel"],
        ),
    ];
    for (label, args) in cases {
        let output = describe(&sandbox, &id, &alpha, &args);
        let error = error_of(&output);
        assert_eq!(error.error.code, "invalid_argument", "{label}");
        assert!(output.stdout.is_empty(), "{label}: nothing on stdout");
        assert_eq!(
            fs::read(&path).expect("record after refusal"),
            before,
            "{label}: the record must be byte-identical"
        );
    }

    // The limits themselves are accepted: 64 characters (multi-byte counts as
    // characters) and a 4096-byte path.
    let sixty_four = "é".repeat(64);
    let ok_path = format!("/{}", "a".repeat(4095));
    let accepted = describe_json(
        &sandbox,
        &id,
        &alpha,
        &["--effort", &sixty_four, "--cwd", &ok_path],
    );
    assert_eq!(accepted["participant"]["runtime"]["effort"], sixty_four);
    assert_eq!(accepted["participant"]["runtime"]["cwd"], ok_path);
    // A path need not exist.
    describe_json(
        &sandbox,
        &id,
        &alpha,
        &["--cwd", "/does/not/exist/anywhere"],
    );
}

#[test]
fn describe_without_a_participant_is_the_unbound_error() {
    let sandbox = Sandbox::new_unseeded();
    let output = sandbox.run_without_identity(
        &["participant", "describe", "--model", "m", "--json"],
        &sandbox.path,
    );
    assert_eq!(
        output.status.code(),
        Some(65),
        "{}",
        common::stderr(&output)
    );
    let error: ErrorEnvelope = from_stderr(&output);
    assert_eq!(error.error.code, "no_participant");
    assert!(
        common::tree_snapshot(&sandbox.mail_root).is_empty(),
        "an unbound describe creates nothing"
    );
}

#[test]
fn runtime_appears_in_show_list_and_who_and_only_where_described() {
    let sandbox = Sandbox::new();
    let (described, alpha) = bound(&sandbox, "described");
    let plain = sandbox.bind_claude("plain", &alpha, Some("alpha"))["participant"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    describe_json(
        &sandbox,
        &described,
        &alpha,
        &["--model", "opus", "--effort", "high", "--cwd", "/work/a"],
    );
    let expected = sandbox.read_participant(&described)["runtime"].clone();
    assert_eq!(expected["model"], "opus");

    let show: Value = from_stdout(&{
        let output =
            sandbox.run_as_participant(&["participant", "show", "--json"], &described, &alpha);
        assert_success(&output);
        output
    });
    assert_eq!(show["participant"]["runtime"], expected);
    let show_plain: Value = from_stdout(&sandbox.run_as_participant(
        &["participant", "show", "--json"],
        &plain,
        &alpha,
    ));
    assert!(show_plain["participant"].get("runtime").is_none());

    let list: Value = from_stdout(&sandbox.run_as_participant(
        &["participant", "list", "--json"],
        &plain,
        &alpha,
    ));
    let records = list["participants"].as_array().expect("participants");
    let record = |id: &str| {
        records
            .iter()
            .find(|record| record["id"] == id)
            .unwrap_or_else(|| panic!("{id} in list"))
    };
    assert_eq!(record(&described)["runtime"], expected);
    assert!(record(&plain).get("runtime").is_none());

    // `who`: on each participants[] entry and on the acting participant.
    let who: Value =
        from_stdout(&sandbox.run_as_participant(&["who", "--json"], &described, &alpha));
    let entries = who["participants"].as_array().expect("who participants");
    let entry = |id: &str| {
        entries
            .iter()
            .find(|entry| entry["id"] == id)
            .unwrap_or_else(|| panic!("{id} in who"))
    };
    assert_eq!(entry(&described)["runtime"], expected);
    assert!(entry(&plain).get("runtime").is_none());
    assert_eq!(who["participant"]["id"], described);
    assert_eq!(who["participant"]["runtime"], expected);
    let who_plain: Value =
        from_stdout(&sandbox.run_as_participant(&["who", "--json"], &plain, &alpha));
    assert!(who_plain["participant"].get("runtime").is_none());
}

#[test]
fn describe_is_silent_and_counts_as_activity() {
    let sandbox = Sandbox::new();
    let (id, alpha) = bound(&sandbox, "describe-silent");
    let peer = sandbox.bind_claude("describe-peer", &alpha, Some("alpha"))["participant"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    // Age the lease stamp so a refresh is visible.
    let path = record_path(&sandbox, &id);
    let mut record: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    record["last_seen"] = Value::String("2026-01-01T00:00:00Z".to_owned());
    fs::write(&path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();

    let before = common::tree_snapshot(&sandbox.mail_root);
    let answer = describe_json(&sandbox, &id, &alpha, &["--model", "opus"]);
    let after = common::tree_snapshot(&sandbox.mail_root);

    assert_ne!(
        answer["participant"]["last_seen"], "2026-01-01T00:00:00Z",
        "describe refreshes last_seen like touch"
    );
    let changed: Vec<_> = after
        .iter()
        .filter(|(path, bytes)| before.get(*path) != Some(*bytes))
        .map(|(path, _)| {
            path.strip_prefix(&sandbox.mail_root)
                .expect("under the mail root")
                .display()
                .to_string()
        })
        .collect();
    assert!(
        changed
            .iter()
            .all(|path| path == &format!("participants/{id}/participant.json")),
        "only the record changes (no mail, no channel event, no cursor): {changed:?}"
    );
    assert!(
        before.keys().all(|path| after.contains_key(path)),
        "nothing is removed"
    );
    assert!(
        after.keys().all(|path| before.contains_key(path)),
        "nothing is created: no inbox, outbox, or channel message"
    );
    let inbox: Value =
        from_stdout(&sandbox.run_as_participant(&["inbox", "--json"], &peer, &alpha));
    assert_eq!(inbox["count"].as_u64().unwrap_or(0), 0, "no mail: {inbox}");
}

#[test]
fn describe_is_a_fenced_writer() {
    let sandbox = Sandbox::new();
    seed_fence_store(&sandbox, r#"{"state":"fenced","generation":7}"#);
    let path = record_path(&sandbox, "test-default");
    let before = fs::read(&path).expect("seeded record");
    let output = sandbox.run_in(
        &["participant", "describe", "--model", "m", "--json"],
        None,
        &sandbox.home.join("dest"),
    );
    assert_migration_refused(&output);
    assert_eq!(fs::read(&path).expect("record after refusal"), before);
}

#[test]
fn records_without_runtime_load_and_a_runtime_record_survives_other_commands() {
    let sandbox = Sandbox::new();
    let (id, alpha) = bound(&sandbox, "describe-compat");
    // A record exactly as every pre-describe post wrote it: no runtime member.
    let record = sandbox.read_participant(&id);
    assert!(record.get("runtime").is_none());
    let show: Value =
        from_stdout(&sandbox.run_as_participant(&["participant", "show", "--json"], &id, &alpha));
    assert_eq!(show["bound"], true);
    assert!(show["participant"].get("runtime").is_none());

    // A hand-edited record with a runtime missing `updated`, plus a member
    // this binary has never heard of, still loads: serde defaults the first
    // and ignores the second (no deny_unknown_fields on a participant record).
    let path = record_path(&sandbox, &id);
    let mut edited = record;
    edited["runtime"] = serde_json::json!({"model": "legacy-edit", "future_field": 1});
    edited["member_from_a_future_post"] = serde_json::json!({"anything": true});
    fs::write(&path, serde_json::to_vec_pretty(&edited).unwrap()).unwrap();
    let show: Value =
        from_stdout(&sandbox.run_as_participant(&["participant", "show", "--json"], &id, &alpha));
    assert_eq!(show["participant"]["runtime"]["model"], "legacy-edit");

    // Ordinary writers rewrite the record without dropping runtime.
    let described = describe_json(&sandbox, &id, &alpha, &["--effort", "high"]);
    assert_eq!(described["participant"]["runtime"]["model"], "legacy-edit");
    let touched: Value =
        from_stdout(&sandbox.run_as_participant(&["participant", "touch", "--json"], &id, &alpha));
    assert_eq!(touched["participant"]["runtime"]["effort"], "high");
    let end = sandbox.run_as_participant(&["participant", "end", "--json"], &id, &alpha);
    assert_success(&end);
    assert_eq!(
        sandbox.read_participant(&id)["runtime"]["effort"],
        "high",
        "end keeps runtime"
    );
}
