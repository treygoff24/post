//! `post participant bind --new --adopt <id>`: a new participant takes over an
//! ended participant's display name, sigil, channel memberships and read
//! cursors, and the adoption is recorded on both records.

mod common;

use common::{
    assert_success, from_stderr, from_stdout, register_alpha_beta, stderr, write_channel_message,
    Sandbox,
};
use post::output::ErrorEnvelope;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

fn record(sandbox: &Sandbox, id: &str) -> Value {
    sandbox.read_participant(id)
}

fn dir(sandbox: &Sandbox, id: &str) -> PathBuf {
    sandbox.mail_root.join("participants").join(id)
}

/// An old participant in `alpha` with a name, a sigil, a joined channel and a
/// consumed channel message. Returns (old id, alpha cwd).
fn seeded_old(sandbox: &Sandbox) -> (String, PathBuf) {
    let (alpha, _beta) = register_alpha_beta(sandbox);
    let old = sandbox.bind_claude("adopt-old", &alpha, Some("alpha"))["participant"]["id"]
        .as_str()
        .expect("id")
        .to_owned();
    assert_success(&sandbox.run_as_participant(
        &["profile", "set", "--name", "Capcom", "--pfp", "🌙"],
        &old,
        &alpha,
    ));
    assert_success(&sandbox.run_as_participant(
        &["chat", "ops", "--join", "--backlog", "--json"],
        &old,
        &alpha,
    ));
    write_channel_message(
        sandbox,
        "ops",
        "20260101-000000-000001-abcdef",
        "beta",
        "hello",
        "history",
    );
    let read = sandbox.run_as_participant(&["chat", "ops", "--json"], &old, &alpha);
    assert_success(&read);
    assert!(common::stdout(&read).contains("\"count\":1"));
    (old, alpha)
}

fn end(sandbox: &Sandbox, id: &str, cwd: &Path) {
    assert_success(&sandbox.run_as_participant(&["participant", "end"], id, cwd));
}

fn adopt(sandbox: &Sandbox, id: &str, cwd: &Path) -> std::process::Output {
    sandbox.run_in_env(
        &["participant", "bind", "--new", "--adopt", id, "--json"],
        None,
        cwd,
        &[],
    )
}

#[test]
fn adopting_an_active_participant_is_refused_and_names_the_holder() {
    let sandbox = Sandbox::new();
    let (old, alpha) = seeded_old(&sandbox);
    let before = fs::read_dir(sandbox.mail_root.join("participants"))
        .unwrap()
        .count();
    let out = adopt(&sandbox, &old, &alpha);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    let error: ErrorEnvelope = from_stderr(&out);
    assert!(
        error.error.message.contains(&old),
        "{}",
        error.error.message
    );
    assert!(error.error.message.contains("still active"));
    assert!(
        error.error.message.contains("claude"),
        "names the holder's harness"
    );
    assert!(error.error.suggested_fix.contains("post participant end"));
    // The refusal mints nothing and records no adoption.
    let after = fs::read_dir(sandbox.mail_root.join("participants"))
        .unwrap()
        .count();
    assert_eq!(before, after);
    assert!(record(&sandbox, &old).get("adopted_by").is_none());
}

#[test]
fn adopting_an_ended_participant_carries_name_sigil_channels_and_cursor() {
    let sandbox = Sandbox::new();
    let (old, alpha) = seeded_old(&sandbox);
    end(&sandbox, &old, &alpha);
    let out = adopt(&sandbox, &old, &alpha);
    assert_success(&out);
    let json: Value = from_stdout(&out);
    let new = json["id"].as_str().expect("new id").to_owned();
    assert_ne!(new, old);
    assert_eq!(json["adopted"]["from"], old.as_str());
    assert_eq!(json["adopted"]["name"], "Capcom");
    assert_eq!(json["adopted"]["pfp"], "🌙");
    assert_eq!(json["adopted"]["channels"], serde_json::json!(["ops"]));

    // Profile entry moved to the new key.
    let profiles: Value =
        serde_json::from_slice(&fs::read(sandbox.mail_root.join("profiles.json")).unwrap())
            .unwrap();
    assert_eq!(profiles[format!("participant:{new}")]["name"], "Capcom");
    assert_eq!(profiles[format!("participant:{new}")]["pfp"], "🌙");

    // Membership and the read cursor came across.
    let channels = fs::read_to_string(dir(&sandbox, &new).join("channels.json")).unwrap();
    assert!(channels.contains("ops"), "{channels}");
    let cursors = fs::read_to_string(dir(&sandbox, &new).join("cursors.json")).unwrap();
    assert!(
        cursors.contains("20260101-000000-000001-abcdef"),
        "{cursors}"
    );

    // Both records say so, with the time.
    let old_record = record(&sandbox, &old);
    let new_record = record(&sandbox, &new);
    assert_eq!(old_record["adopted_by"], new.as_str());
    assert_eq!(new_record["adopted_from"], old.as_str());
    assert!(old_record["adopted_at"].is_string());
    assert_eq!(old_record["adopted_at"], new_record["adopted_at"]);
}

#[test]
fn adopting_an_already_adopted_participant_names_the_adopter() {
    let sandbox = Sandbox::new();
    let (old, alpha) = seeded_old(&sandbox);
    end(&sandbox, &old, &alpha);
    let first: Value = from_stdout(&adopt(&sandbox, &old, &alpha));
    let adopter = first["id"].as_str().unwrap().to_owned();
    let out = adopt(&sandbox, &old, &alpha);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    let error: ErrorEnvelope = from_stderr(&out);
    assert!(error.error.message.contains("already adopted"));
    assert!(
        error.error.message.contains(&adopter),
        "{}",
        error.error.message
    );
}

#[test]
fn profile_set_takes_a_sigil_from_an_ended_holder_and_says_so() {
    let sandbox = Sandbox::new();
    let (old, alpha) = seeded_old(&sandbox);
    end(&sandbox, &old, &alpha);
    let other = sandbox.bind_codex("adopt-other", &alpha, Some("alpha"))["participant"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let out = sandbox.run_as_participant(&["profile", "set", "--pfp", "🌙"], &other, &alpha);
    assert_success(&out);
    let json: Value = from_stdout(&out);
    assert_eq!(json["profile"]["pfp"], "🌙");
    let note = json["sigil_freed_from"].as_str().expect("freed note");
    assert!(note.contains(&old) && note.contains("ended"), "{note}");
}
