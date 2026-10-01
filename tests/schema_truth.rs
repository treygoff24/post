//! `post schema` cannot lie. Agents guess JSON field names, so a field the
//! schema never mentions is a field they will guess wrong, and a flag the
//! usage line omits is a flag they will not find. Three checks tie the schema
//! to what the binary really prints and accepts:
//!
//! 1. every field of every `post contract samples` output is named in the
//!    schema shape of the command that produced it (samples come from the
//!    real producers; see tests/contract_samples.rs);
//! 2. the fields this wave added are named, checked against live output;
//! 3. every long option a command's `--help` lists is in its schema usage.
//!
//! Every test uses a throwaway store.

mod common;

use common::{
    assert_documented, assert_success, documented_keys, from_stdout, help_options, names,
    register_alpha_beta, undocumented, usage_options, Sandbox,
};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

fn schema(sandbox: &Sandbox) -> Value {
    let output = sandbox.run(&["schema"]);
    assert_success(&output);
    from_stdout(&output)
}

/// The text of one output shape: its lines, joined.
fn shape(schema: &Value, name: &str) -> String {
    schema["output_shapes"][name]
        .as_array()
        .unwrap_or_else(|| panic!("schema has no output shape named {name}"))
        .iter()
        .map(|line| line.as_str().expect("shape line").to_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The instrument itself: it looks inside nested objects and arrays, exempts
/// only data-keyed maps, and reports a nested field the shape never names.
#[test]
fn the_field_check_reaches_nested_objects_and_exempts_only_data_keyed_maps() {
    let value = json!({
        "envelope": {"kind": "note", "extra": {"deep": 1}},
        "items": [{"inner": {"leaf": 1}}],
        "pending": {"lineage:ember": 1},
        "unread": [{"id": "m1"}],
    });
    let keys = documented_keys(&value);
    for wanted in [
        "envelope", "kind", "extra", "deep", "items", "inner", "leaf", "id",
    ] {
        assert!(keys.contains(wanted), "{wanted} missing from {keys:?}");
    }
    assert!(
        !keys.contains("lineage:ember"),
        "a data-keyed map's keys are data: {keys:?}"
    );

    let shape = "envelope ({kind})\nitems[]";
    let problem = undocumented(
        "s",
        shape,
        &json!({"envelope": {"kind": 1, "secret": 2}}),
        "t",
    )
    .expect("a nested field the shape never names is caught");
    assert!(
        problem.contains("secret") && !problem.contains("kind"),
        "{problem}"
    );
    assert_eq!(
        undocumented("s", shape, &json!({"envelope": {"kind": 1}}), "t"),
        None
    );
}

#[test]
fn review_avatar_maps_are_exempt_only_at_documented_paths() {
    let value = json!({
        "avatar": {"body": {"idle": []}, "head": {"idle": []}, "emotes": {"wave": {}}},
        "messages": [{"emote": {"frames": {"body": {"idle": ""}, "head": {"idle": ""}}}}],
        "unrelated": {"body": {"undocumented_body": 1}, "head": {"undocumented_head": 1}, "emotes": {"undocumented_emotes": 1}}
    });
    let keys = documented_keys(&value);
    assert!(!keys.contains("idle") && !keys.contains("wave"));
    for key in [
        "undocumented_body",
        "undocumented_head",
        "undocumented_emotes",
    ] {
        assert!(
            keys.contains(key),
            "an unrelated {key} must still be checked"
        );
    }
}

/// Which schema shape describes each sample the contract ships. A new sample
/// with no entry here fails the test: say what documents it.
fn shape_for_sample(name: &str) -> &'static str {
    match name {
        "channels.json" => "channels",
        "chat.json" => "chat_read",
        "chat-emote.json" => "chat_send",
        "doctor.json" => "doctor",
        "inbox.json" => "inbox",
        "participant-bind.json" | "participant-describe.json" | "participant-show.json" => {
            "participant"
        }
        "profile-list.json" | "profile-show.json" | "profile-avatar.json" => "profile",
        "rooms.json" => "rooms",
        "version.json" => "version",
        "who.json" => "who",
        name if name.starts_with("bridge-deliver-") => "bridge",
        name if name.starts_with("watch-") => "watch",
        other => panic!(
            "contract sample {other} has no schema shape mapped in tests/schema_truth.rs; \
             name the output shape that documents it"
        ),
    }
}

#[test]
fn every_contract_sample_field_is_in_the_schema() {
    let sandbox = Sandbox::new();
    let schema = schema(&sandbox);
    let samples = sandbox.run(&["contract", "samples"]);
    assert_success(&samples);
    let samples: Value = from_stdout(&samples);
    let samples = samples["samples"].as_object().expect("samples object");
    assert!(samples.len() >= 10, "the contract ships its samples");

    let mut checked = 0;
    let mut problems = Vec::new();
    for (name, text) in samples {
        let shape_name = shape_for_sample(name);
        let shape = shape(&schema, shape_name);
        let text = text.as_str().expect("sample text");
        // `.jsonl` samples are one event per line; `.json` samples are one
        // (pretty-printed) document.
        let documents: Vec<&str> = if name.ends_with(".jsonl") {
            text.lines()
                .filter(|line| !line.trim().is_empty())
                .collect()
        } else {
            vec![text]
        };
        for document in documents {
            let value: Value = serde_json::from_str(document)
                .unwrap_or_else(|error| panic!("{name} is not JSON: {error}\n{document}"));
            problems.extend(undocumented(shape_name, &shape, &value, name));
            checked += 1;
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    assert!(checked >= samples.len(), "every sample was read");
}

fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
    fs::write(path, contents).expect("write fixture");
}

#[test]
fn fields_added_by_this_wave_are_in_the_schema_and_in_live_output() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let schema = schema(&sandbox);

    // send: `warnings`, live from a body that looks like pasted watch output.
    let ndjson = r#"{"event":"channel_message","channel":"commons","id":"20260804-224402-425133-e9857f","from":"sol","subject":"","sent":"2026-08-04 22:44:02 -0400"}"#;
    let sent = sandbox.run_in(
        &["send", "--to", "beta", "--json", "--body", ndjson],
        None,
        &alpha,
    );
    assert_success(&sent);
    let sent: Value = from_stdout(&sent);
    assert!(sent.get("warnings").is_some(), "fixture: {sent}");
    assert_documented("send_json", &shape(&schema, "send_json"), &sent, "send");

    // doctor: `severity_filter`, live under --severity.
    let doctor = sandbox.run(&["doctor", "--severity", "warn"]);
    let doctor: Value = from_stdout(&doctor);
    assert!(doctor.get("severity_filter").is_some(), "fixture: {doctor}");
    assert_documented("doctor", &shape(&schema, "doctor"), &doctor, "doctor");

    // who: bridge_attention, doorbell, doorbell_armed on both row kinds.
    write(
        &sandbox.mail_root.join("bridge/health.json"),
        &json!({"attention": [{"kind": "held", "summary": "s", "fix": "f"}]}).to_string(),
    );
    write(
        &sandbox.mail_root.join("doorbell/health.json"),
        &json!({
            "bindings": [{"participant": "test-default", "armed": true}],
            "residents": [{"room": "claude-space", "armed": true}]
        })
        .to_string(),
    );
    let who = sandbox.run(&["who", "--json"]);
    assert_success(&who);
    let who: Value = from_stdout(&who);
    assert!(who.get("bridge_attention").is_some(), "fixture: {who}");
    assert!(who.get("doorbell").is_some(), "fixture: {who}");
    assert!(
        who["participants"]
            .as_array()
            .expect("participants")
            .iter()
            .any(|entry| entry.get("doorbell_armed").is_some()),
        "fixture: {who}"
    );
    assert!(
        who["legacy_rooms"]
            .as_array()
            .expect("legacy_rooms")
            .iter()
            .any(|entry| entry.get("doorbell_armed").is_some()),
        "fixture: {who}"
    );
    assert_documented("who", &shape(&schema, "who"), &who, "who");
}

/// The identity states the wave added, read from live output: every field a
/// missing claim, an unbound watch, `participant gc`, and a `bind --new` record
/// print is named in the schema, and the status values `participant show`
/// answers with are all listed.
#[test]
fn the_identity_states_are_documented_and_live() {
    let sandbox = Sandbox::new();
    let schema = schema(&sandbox);
    let participant = shape(&schema, "participant");
    for word in [
        "bound",
        "unbound",
        "missing",
        "archived",
        "ephemeral",
        "lease_hours",
        "applied",
    ] {
        assert!(names(&participant, word), "participant shape lacks {word}");
    }
    let cwd = sandbox.path.clone();

    // A claim that names no record: show, who, doctor.
    let show = sandbox.run_unbound(&["participant", "show"], &cwd);
    assert_success(&show);
    let show: Value = from_stdout(&show);
    assert_eq!(show["status"], "missing", "fixture: {show}");
    assert_documented("participant", &participant, &show, "participant show");
    let gc = sandbox.run_unbound(&["participant", "gc"], &cwd);
    assert_success(&gc);
    let gc: Value = from_stdout(&gc);
    assert!(gc.get("applied").is_some(), "fixture: {gc}");
    assert_documented("participant", &participant, &gc, "participant gc");
    let who = sandbox.run_unbound(&["who"], &cwd);
    let who: Value = from_stdout(&who);
    assert!(who.get("participant_missing").is_some(), "fixture: {who}");
    assert_documented("who", &shape(&schema, "who"), &who, "who, claim missing");
    let doctor = sandbox.run_unbound(&["doctor", "--fix"], &cwd);
    let doctor: Value = from_stdout(&doctor);
    assert!(
        doctor.get("participant_missing").is_some(),
        "fixture: {doctor}"
    );
    assert_documented(
        "doctor",
        &shape(&schema, "doctor"),
        &doctor,
        "doctor, claim missing",
    );

    // No claim at all: the unbound snapshot line a hook reads.
    let snapshot = sandbox.run_without_identity(&["watch", "--snapshot"], &cwd);
    assert_success(&snapshot);
    let line: Value = from_stdout(&snapshot);
    assert_eq!(line["event"], "unbound", "fixture: {line}");
    assert_documented("watch", &shape(&schema, "watch"), &line, "watch --snapshot");

    // A `bind --new` record is ephemeral with a one-hour lease.
    let bound = sandbox.run_in_env(&["participant", "bind", "--new", "--json"], None, &cwd, &[]);
    assert_success(&bound);
    let bound: Value = from_stdout(&bound);
    assert_eq!(bound["participant"]["ephemeral"], true, "fixture: {bound}");
    assert_eq!(bound["participant"]["lease_hours"], 1, "fixture: {bound}");
    assert_documented(
        "participant",
        &participant,
        &bound,
        "participant bind --new",
    );
}

/// Age a participant record so `participant gc` collects it.
fn age_record(sandbox: &Sandbox, id: &str) {
    let path = sandbox
        .mail_root
        .join("participants")
        .join(id)
        .join("participant.json");
    let mut record: Value =
        serde_json::from_slice(&fs::read(&path).expect("record")).expect("record JSON");
    record["last_seen"] = json!("2020-01-01T00:00:00Z");
    fs::write(&path, serde_json::to_vec_pretty(&record).expect("bytes")).expect("write record");
}

fn run_gc(sandbox: &Sandbox, apply: bool) -> Value {
    let mut args = vec!["participant", "gc", "--json"];
    if apply {
        args.push("--apply");
    }
    let output = sandbox.run_without_identity(&args, &sandbox.path);
    assert_success(&output);
    from_stdout(&output)
}

fn run_restore(sandbox: &Sandbox, id: &str) -> std::process::Output {
    sandbox.run_without_identity(&["participant", "restore", id, "--json"], &sandbox.path)
}

/// `participant gc` and `participant restore` are listed in the schema's
/// usage, described in full, and every field their real output prints is
/// named in the participant shape: a dry run and an apply with something to
/// delete, something to archive, and records kept; a restore from a tombstone,
/// from an archive, and of a record already present; and the error for an id
/// nothing ever held.
#[test]
fn participant_gc_and_restore_are_documented_and_live() {
    let sandbox = Sandbox::new();
    let schema = schema(&sandbox);
    let entry = schema["commands"]
        .as_array()
        .expect("commands")
        .iter()
        .find(|command| command["name"] == "participant")
        .expect("the participant command");
    let usage = entry["usage"].as_str().expect("usage");
    for verb in [
        "post participant gc [--apply]",
        "post participant restore <id>",
    ] {
        assert!(usage.contains(verb), "usage lacks `{verb}`: {usage}");
    }
    let effects = entry["side_effects"].as_str().expect("side_effects");
    // Specific enough that no other sentence of the entry can satisfy them:
    // `idempotently`, `fenced writer`, and `participant_missing` all occur
    // elsewhere in this command's text.
    for phrase in [
        "is a dry run unless --apply",
        "--apply is a fenced writer",
        "leaving a tombstone line in participants/archived.jsonl",
        "to participants-archive/<id>/",
        "idempotent: an id already present answers restored=false",
        "fails participant_missing, exit 65",
    ] {
        assert!(
            effects.contains(phrase),
            "the participant side effects never say `{phrase}`: {effects}"
        );
    }
    let participant = shape(&schema, "participant");
    // The restore line alone must name every top-level field of a restore
    // answer: `restored` and `from` are also words elsewhere in the shape, so
    // the whole-shape check below cannot catch a line that stopped naming them.
    let restore_line = participant
        .lines()
        .find(|line| line.starts_with("restore ("))
        .expect("the participant shape has a `restore (` line");
    let cwd = sandbox.path.clone();

    // One record that holds nothing (tier 1) and one that holds a channel
    // membership (tier 2), both long idle.
    let bare = sandbox.run_in_env(&["participant", "bind", "--new", "--json"], None, &cwd, &[]);
    assert_success(&bare);
    let bare: Value = from_stdout(&bare);
    let bare = bare["participant"]["id"].as_str().expect("id").to_owned();
    let stateful = sandbox.run_in_env(
        &[
            "participant",
            "bind",
            "--harness",
            "claude",
            "--key",
            "schema-truth-restore",
            "--json",
        ],
        None,
        &cwd,
        &[],
    );
    assert_success(&stateful);
    let stateful: Value = from_stdout(&stateful);
    let stateful = stateful["participant"]["id"]
        .as_str()
        .expect("id")
        .to_owned();
    let joined = sandbox.run_as_participant(
        &["chat", "schema-truth-room", "--join", "--json"],
        &stateful,
        &cwd,
    );
    assert_success(&joined);
    age_record(&sandbox, &bare);
    age_record(&sandbox, &stateful);

    let dry = run_gc(&sandbox, false);
    assert_eq!(dry["applied"], false, "fixture: {dry}");
    assert_eq!(dry["deleted"], json!([bare]), "fixture: {dry}");
    assert_eq!(dry["archived"], json!([stateful]), "fixture: {dry}");
    assert!(dry["kept"].is_object(), "fixture: {dry}");
    assert_documented("participant", &participant, &dry, "participant gc");
    let applied = run_gc(&sandbox, true);
    assert_eq!(applied["applied"], true, "fixture: {applied}");
    assert_eq!(applied["deleted"], dry["deleted"], "fixture: {applied}");
    assert_eq!(applied["archived"], dry["archived"], "fixture: {applied}");
    assert_documented(
        "participant",
        &participant,
        &applied,
        "participant gc --apply",
    );

    for (id, from) in [(&bare, "tombstone"), (&stateful, "archive")] {
        let output = run_restore(&sandbox, id);
        assert_success(&output);
        let restored: Value = from_stdout(&output);
        assert_eq!(restored["restored"], true, "fixture: {restored}");
        assert_eq!(restored["from"], from, "fixture: {restored}");
        for key in restored.as_object().expect("object").keys() {
            assert!(
                names(restore_line, key),
                "the restore line never names `{key}`: {restore_line}"
            );
        }
        assert_documented(
            "participant",
            &participant,
            &restored,
            &format!("participant restore ({from})"),
        );
        // Again: already present.
        let output = run_restore(&sandbox, id);
        assert_success(&output);
        let again: Value = from_stdout(&output);
        assert_eq!(again["restored"], false, "fixture: {again}");
        assert!(again.get("from").is_none(), "fixture: {again}");
        assert_documented(
            "participant",
            &participant,
            &again,
            "participant restore (present)",
        );
    }

    // An id nothing ever held: the error the schema promises, exit 65.
    let never = run_restore(&sandbox, "claude-0badf00d");
    assert_eq!(never.status.code(), Some(65), "{never:?}");
    let error: Value = common::from_stderr(&never);
    assert_eq!(
        error["error"]["code"], "participant_missing",
        "fixture: {error}"
    );
    assert!(
        restore_line.contains("participant_missing") && restore_line.contains("exit 65"),
        "the restore line must name its error and exit code: {restore_line}"
    );
}

/// The instrument for the check below: a usage naming only `--discard-through`
/// must not count as naming `--discard`, and a `-x, --long` help line counts.
#[test]
fn the_option_lexers_keep_prefix_related_options_apart() {
    let usage = "post chat <channel> [--discard-through <id>] [--body-file <path> | --body <text>]";
    let named = usage_options(usage);
    assert!(named.contains("--discard-through") && named.contains("--body-file"));
    assert!(!named.contains("--discard"), "{named:?}");
    let help = "Usage: post x\n\nOptions:\n  -b, --body <TEXT>  the body\n      --discard  drop\n      --limit <N>\n  -h, --help\n";
    let listed: Vec<String> = help_options(help).into_iter().collect();
    assert_eq!(listed, ["--body", "--discard", "--limit"]);
    assert!(
        !named.contains(&listed[1]),
        "--discard is not in that usage"
    );
}

#[test]
fn every_option_a_command_helps_with_is_in_its_schema_usage() {
    let sandbox = Sandbox::new();
    let schema = schema(&sandbox);
    let commands = schema["commands"].as_array().expect("commands");
    let mut checked = 0;
    for command in commands {
        let name = command["name"].as_str().expect("command name");
        let usage = command["usage"].as_str().expect("usage");
        let help = sandbox.run(&[name, "--help"]);
        assert_success(&help);
        let help = common::stdout(&help);
        let listed = help_options(&help);
        let in_usage = usage_options(usage);
        for option in &listed {
            assert!(
                in_usage.contains(option),
                "`post {name} --help` lists {option}, but the schema usage for `{name}` omits it:\n{usage}"
            );
        }
        checked += listed.len();
    }
    assert!(checked > 20, "the check saw the help options ({checked})");
}

/// A flag hidden from `--help` still exists, so the schema (which agents read
/// to learn what runs) names it.
#[test]
fn the_schema_names_send_s_hidden_allow_self() {
    let sandbox = Sandbox::new();
    let schema = schema(&sandbox);
    let send = schema["commands"]
        .as_array()
        .expect("commands")
        .iter()
        .find(|command| command["name"] == "send")
        .expect("send in schema");
    assert!(send["usage"]
        .as_str()
        .expect("usage")
        .contains("--allow-self"));
    assert!(send["side_effects"]
        .as_str()
        .expect("side effects")
        .contains("hidden from --help"));
}
