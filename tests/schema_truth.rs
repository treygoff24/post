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

use common::{assert_success, from_stdout, register_alpha_beta, Sandbox};
use serde_json::{json, Value};
use std::collections::BTreeSet;
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

/// True when `word` appears in `text` as a whole identifier: `id` is not
/// documented by `identity`, and `bound` is not documented by `bound_now`.
fn names(text: &str, word: &str) -> bool {
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    text.match_indices(word).any(|(start, _)| {
        let before = text[..start].chars().next_back();
        let after = text[start + word.len()..].chars().next();
        !before.is_some_and(is_word) && !after.is_some_and(is_word)
    })
}

/// Objects whose keys are data (room names, participant ids, addresses), not
/// schema: the shape describes them as `name{key:value}` and their keys are
/// whatever the store holds, so those keys are not required in the shape.
/// Every other nested object is traversed. An entry here is a promise that
/// the shape documents the map's value, not its keys.
const DATA_KEYED_MAPS: &[&str] = &["unread", "pending", "pending_by_address", "rewritten"];

/// The fields a real output carries that the shape must name: every key of
/// every object at any depth (nested objects and objects inside arrays
/// included), except the keys of the data-keyed maps above.
fn documented_keys(value: &Value) -> BTreeSet<String> {
    fn collect(value: &Value, keys: &mut BTreeSet<String>) {
        match value {
            Value::Object(object) => {
                for (key, child) in object {
                    keys.insert(key.clone());
                    // Only a map is exempt: `unread` in an inbox is an array
                    // of envelopes whose fields must be named.
                    let data_keyed = child.is_object() && DATA_KEYED_MAPS.contains(&key.as_str());
                    if !data_keyed {
                        collect(child, keys);
                    }
                }
            }
            Value::Array(items) => items.iter().for_each(|item| collect(item, keys)),
            _ => {}
        }
    }
    let mut keys = BTreeSet::new();
    collect(value, &mut keys);
    keys
}

/// What is wrong when `value` carries a field the shape never names.
fn undocumented(shape_name: &str, shape: &str, value: &Value, origin: &str) -> Option<String> {
    let missing: Vec<String> = documented_keys(value)
        .into_iter()
        .filter(|key| !names(shape, key))
        .collect();
    (!missing.is_empty()).then(|| {
        format!("{origin}: the `{shape_name}` shape in `post schema` never names {missing:?}")
    })
}

fn assert_documented(shape_name: &str, shape: &str, value: &Value, origin: &str) {
    if let Some(problem) = undocumented(shape_name, shape, value, origin) {
        panic!("{problem}\nshape:\n{shape}");
    }
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

/// Which schema shape describes each sample the contract ships. A new sample
/// with no entry here fails the test: say what documents it.
fn shape_for_sample(name: &str) -> &'static str {
    match name {
        "channels.json" => "channels",
        "chat.json" => "chat_read",
        "doctor.json" => "doctor",
        "inbox.json" => "inbox",
        "participant-bind.json" | "participant-show.json" => "participant",
        "profile-list.json" | "profile-show.json" => "profile",
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

/// Fields other lanes of the wave add to their own commands. Their producers
/// are those lanes' code, so this only pins that the schema names them; the
/// integrated build's contract samples and the live checks above carry the
/// rest. Per docs/plans/post-just-works-2026-09-28.md.
#[test]
fn the_schema_declares_the_fields_the_contract_names() {
    let sandbox = Sandbox::new();
    let schema = schema(&sandbox);
    for (shape_name, fields) in [
        ("chat_send", &["crossed", "skipped", "bound_now"][..]),
        ("chat_read", &["skipped_files", "bound"][..]),
        ("chat_join", &["bound_now"][..]),
        ("send_json", &["bound_now", "warnings"][..]),
        ("inbox", &["bound", "bound_now", "hint"][..]),
        ("read_json", &["bound", "bound_now"][..]),
        ("channels", &["bound", "skipped"][..]),
        ("search", &["bound", "skipped"][..]),
        ("catchup", &["skipped", "bound_now"][..]),
        (
            "participant",
            &[
                "participant_missing",
                "gc",
                "deleted",
                "archived",
                "kept",
                "restore",
                "restored",
                "from",
            ][..],
        ),
        ("who", &["participant_missing", "bridge_attention"][..]),
        ("doctor", &["participant_missing", "severity_filter"][..]),
    ] {
        let text = shape(&schema, shape_name);
        for field in fields {
            assert!(
                names(&text, field),
                "the `{shape_name}` shape never names `{field}`:\n{text}"
            );
        }
    }
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
        participant.contains("participant_missing") && participant.contains("exit 65"),
        "the participant shape must name the restore error and its exit code:\n{participant}"
    );
}

/// The long options a command's `--help` lists, excluding the global flags.
fn help_options(help: &str) -> BTreeSet<String> {
    let mut options = BTreeSet::new();
    let mut in_options = false;
    for line in help.lines() {
        if line.trim_end().ends_with(':') && !line.starts_with(' ') {
            in_options = line.starts_with("Options");
            continue;
        }
        if !in_options {
            continue;
        }
        let trimmed = line.trim_start();
        // An option line starts with `-x, --long` or `--long`; a possible
        // value (`- now: ...`) or a prose line that mentions a flag does not.
        let mut chars = trimmed.chars();
        let short_form = chars.next() == Some('-')
            && chars.next().is_some_and(|c| c.is_ascii_alphabetic())
            && chars.next() == Some(',');
        if !(trimmed.starts_with("--") || short_form) {
            continue;
        }
        if let Some(start) = trimmed.find("--") {
            let name: String = trimmed[start..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                .collect();
            options.insert(name);
        }
    }
    for global in ["--help", "--version", "--json", "--pretty"] {
        options.remove(global);
    }
    options
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
        if !help.status.success() {
            continue;
        }
        let help = common::stdout(&help);
        let listed = help_options(&help);
        for option in &listed {
            assert!(
                usage.contains(option.as_str()),
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
