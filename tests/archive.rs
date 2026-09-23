mod common;

use common::{assert_success, from_stdout, join_channel, register_alpha_beta, Sandbox};
use serde_json::Value;
use std::fs;
use std::path::Path;

fn channels(sandbox: &Sandbox, cwd: &Path, extra: &[&str]) -> Value {
    let mut args = vec!["channels"];
    args.extend_from_slice(extra);
    let output = sandbox.run_in(&args, None, cwd);
    assert_success(&output);
    from_stdout(&output)
}

fn names(listing: &Value) -> Vec<String> {
    listing["channels"]
        .as_array()
        .expect("channels array")
        .iter()
        .map(|item| item["name"].as_str().expect("name").to_owned())
        .collect()
}

fn chat(sandbox: &Sandbox, cwd: &Path, args: &[&str]) -> Value {
    let mut full = vec!["chat"];
    full.extend_from_slice(args);
    full.push("--json");
    let output = sandbox.run_in(&full, None, cwd);
    assert_success(&output);
    from_stdout(&output)
}

fn message_count(sandbox: &Sandbox, channel: &str) -> usize {
    fs::read_dir(
        sandbox
            .mail_root
            .join("channels")
            .join(channel)
            .join("messages"),
    )
    .expect("messages dir")
    .count()
}

#[test]
fn archive_hides_from_listing_keeps_history_and_unarchive_restores() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    join_channel(&sandbox, "live", &alpha);
    join_channel(&sandbox, "old", &alpha);
    chat(&sandbox, &alpha, &["old", "--send", "--body", "last word"]);
    let before = message_count(&sandbox, "old");

    let archived = chat(&sandbox, &alpha, &["old", "--archive"]);
    assert_eq!(archived["archived"], true);
    assert_eq!(archived["changed"], true);
    assert!(archived["archived_by"].is_string());
    // History untouched: archive is a sidecar, never an event message.
    assert_eq!(message_count(&sandbox, "old"), before);

    let default = channels(&sandbox, &alpha, &[]);
    assert_eq!(names(&default), ["live"]);
    assert_eq!(default["archived_hidden"], 1);

    let only = channels(&sandbox, &alpha, &["--archived"]);
    assert_eq!(names(&only), ["old"]);
    assert_eq!(only["channels"][0]["archived"], true);
    assert!(only["channels"][0]["archived_at"].is_string());

    let all = channels(&sandbox, &alpha, &["--all"]);
    assert_eq!(names(&all), ["live", "old"]);
    assert_eq!(all["archived_hidden"], 0);

    // Idempotent both ways.
    let again = chat(&sandbox, &alpha, &["old", "--archive"]);
    assert_eq!(again["changed"], false);
    let restored = chat(&sandbox, &alpha, &["old", "--unarchive"]);
    assert_eq!(restored["archived"], false);
    assert_eq!(restored["changed"], true);
    let restored_again = chat(&sandbox, &alpha, &["old", "--unarchive"]);
    assert_eq!(restored_again["changed"], false);
    assert_eq!(names(&channels(&sandbox, &alpha, &[])), ["live", "old"]);

    // The log only grows: archive, unarchive.
    let sidecar: Value = serde_json::from_slice(
        &fs::read(sandbox.mail_root.join("channels/old/archive.json")).expect("archive.json"),
    )
    .expect("archive.json JSON");
    let actions: Vec<&str> = sidecar["log"]
        .as_array()
        .expect("log")
        .iter()
        .map(|entry| entry["action"].as_str().expect("action"))
        .collect();
    assert_eq!(actions, ["archive", "unarchive"]);
    assert!(sidecar["archived"].is_null());
}

#[test]
fn a_new_post_resurrects_but_a_join_does_not() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    join_channel(&sandbox, "quiet", &alpha);
    chat(&sandbox, &alpha, &["quiet", "--send", "--body", "before"]);
    chat(&sandbox, &alpha, &["quiet", "--archive"]);

    // A join is an event: Porch's automatic owner joins must not un-archive.
    join_channel(&sandbox, "quiet", &beta);
    assert_eq!(
        names(&channels(&sandbox, &alpha, &["--archived"])),
        ["quiet"]
    );

    // A conversational post is newer than the mark: live again, no sidecar write.
    let sidecar = sandbox.mail_root.join("channels/quiet/archive.json");
    let sidecar_before = fs::read(&sidecar).expect("sidecar");
    chat(
        &sandbox,
        &beta,
        &[
            "quiet",
            "--send",
            "--anyway",
            "--body",
            "back from the dead",
        ],
    );
    assert_eq!(names(&channels(&sandbox, &alpha, &[])), ["quiet"]);
    assert_eq!(fs::read(&sidecar).expect("sidecar"), sidecar_before);
}

#[test]
fn an_empty_channel_stays_archived_until_its_first_post() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    join_channel(&sandbox, "empty", &alpha);
    chat(&sandbox, &alpha, &["empty", "--archive"]);
    assert_eq!(
        names(&channels(&sandbox, &alpha, &["--archived"])),
        ["empty"]
    );
    chat(&sandbox, &alpha, &["empty", "--send", "--body", "first"]);
    assert_eq!(names(&channels(&sandbox, &alpha, &[])), ["empty"]);
}

#[test]
fn a_non_member_may_archive_and_search_archived_history() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    join_channel(&sandbox, "alpha-only", &alpha);
    chat(
        &sandbox,
        &alpha,
        &[
            "alpha-only",
            "--send",
            "--body",
            "the ZephyrMarker decision",
        ],
    );

    // beta never joined; ordinary search cannot see the channel.
    let plain = sandbox.run_in(&["search", "ZephyrMarker", "--json"], None, &beta);
    assert_success(&plain);
    let plain: Value = from_stdout(&plain);
    assert_eq!(plain["count"], 0);

    let archived = chat(&sandbox, &beta, &["alpha-only", "--archive"]);
    assert_eq!(archived["changed"], true);

    let found = sandbox.run_in(
        &["search", "ZephyrMarker", "--archived", "--json"],
        None,
        &beta,
    );
    assert_success(&found);
    let found: Value = from_stdout(&found);
    assert_eq!(found["count"], 1);
    assert_eq!(found["results"][0]["channel"], "alpha-only");

    // Once resurrected, the membership rule applies again.
    chat(&sandbox, &alpha, &["alpha-only", "--unarchive"]);
    let after = sandbox.run_in(
        &["search", "ZephyrMarker", "--archived", "--json"],
        None,
        &beta,
    );
    assert_success(&after);
    let after: Value = from_stdout(&after);
    assert_eq!(after["count"], 0);
}

#[test]
fn archive_refuses_a_missing_channel_and_creates_nothing() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let output = sandbox.run_in(&["chat", "ghost", "--archive", "--json"], None, &alpha);
    assert!(
        !output.status.success(),
        "archive of a missing channel succeeded"
    );
    assert!(!sandbox.mail_root.join("channels/ghost").exists());
}

#[test]
fn a_corrupt_archive_file_fails_open_and_doctor_reports_it() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    join_channel(&sandbox, "bent", &alpha);
    fs::write(
        sandbox.mail_root.join("channels/bent/archive.json"),
        b"{not json",
    )
    .expect("write");
    assert_eq!(names(&channels(&sandbox, &alpha, &[])), ["bent"]);
    let doctor = sandbox.run_in(&["doctor"], None, &alpha);
    let text = String::from_utf8_lossy(&doctor.stdout);
    assert!(
        text.contains("channel.bent.archive_invalid"),
        "doctor did not report the bad archive.json: {text}"
    );
}
