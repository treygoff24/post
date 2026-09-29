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

/// The fields a real output carries that the shape must name: the top-level
/// keys, and the keys of the objects inside its top-level arrays. Nested
/// objects that may be keyed by data (`unread{address:count}`) are skipped.
fn documented_keys(value: &Value) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    let Some(object) = value.as_object() else {
        return keys;
    };
    for (key, child) in object {
        keys.insert(key.clone());
        if let Some(items) = child.as_array() {
            for item in items.iter().filter_map(Value::as_object) {
                keys.extend(item.keys().cloned());
            }
        }
    }
    keys
}

fn assert_documented(shape_name: &str, shape: &str, value: &Value, origin: &str) {
    let missing: Vec<String> = documented_keys(value)
        .into_iter()
        .filter(|key| !names(shape, key))
        .collect();
    assert!(
        missing.is_empty(),
        "{origin}: the `{shape_name}` shape in `post schema` never names {missing:?}\nshape:\n{shape}"
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
            assert_documented(shape_name, &shape, &value, name);
            checked += 1;
        }
    }
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
            &["participant_missing", "gc", "deleted", "archived", "kept"][..],
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
