//! Unlisted channels: a channel.json flag that keeps a channel (Loom's
//! per-session agent channels) out of the default `post channels` listing,
//! and so out of the owner's clients, without archiving it.

mod common;

use common::{
    assert_success, from_stderr, from_stdout, join_channel, register_alpha_beta, Sandbox,
};
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

fn channel_json(sandbox: &Sandbox, channel: &str) -> Value {
    let path = sandbox
        .mail_root
        .join("channels")
        .join(channel)
        .join("channel.json");
    serde_json::from_slice(&fs::read(&path).expect("channel.json")).expect("channel.json JSON")
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
fn join_unlisted_creates_the_channel_unlisted_from_its_first_write() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    join_channel(&sandbox, "ops", &alpha);

    let joined = chat(&sandbox, &alpha, &["agents-s1", "--join", "--unlisted"]);
    assert_eq!(joined["created"], true, "{joined}");
    assert_eq!(joined["unlisted"], true, "{joined}");
    assert_eq!(channel_json(&sandbox, "agents-s1")["unlisted"], true);
    // A listed channel's file never carries the key: absent means listed.
    assert!(
        channel_json(&sandbox, "ops").get("unlisted").is_none(),
        "{}",
        channel_json(&sandbox, "ops")
    );

    let default = channels(&sandbox, &alpha, &[]);
    assert_eq!(names(&default), ["ops"]);
    assert_eq!(default["unlisted_hidden"], 1);
    assert_eq!(default["archived_hidden"], 0);
    assert_eq!(default["channels"][0]["unlisted"], false);

    let only = channels(&sandbox, &alpha, &["--unlisted"]);
    assert_eq!(names(&only), ["agents-s1"]);
    assert_eq!(only["channels"][0]["unlisted"], true);
    assert_eq!(only["unlisted_hidden"], 0);

    // The channel works as before: send and read.
    chat(
        &sandbox,
        &alpha,
        &["agents-s1", "--send", "--body", "hello"],
    );
    assert_eq!(names(&channels(&sandbox, &alpha, &[])), ["ops"]);

    let text = sandbox.run_in(&["channels", "--text"], None, &alpha);
    assert_success(&text);
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(
        text.contains("1 unlisted channel(s) hidden; `post channels --unlisted --text`"),
        "{text}"
    );
}

#[test]
fn unlist_and_relist_mirror_archive_for_any_participant() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    join_channel(&sandbox, "ops", &alpha);
    chat(&sandbox, &alpha, &["ops", "--send", "--body", "last word"]);
    let before = message_count(&sandbox, "ops");

    // beta never joined: membership is not required, as with --archive.
    let unlisted = chat(&sandbox, &beta, &["ops", "--unlist"]);
    assert_eq!(unlisted["ok"], true);
    assert_eq!(unlisted["channel"], "ops");
    assert_eq!(unlisted["unlisted"], true);
    assert_eq!(unlisted["changed"], true);
    assert_eq!(channel_json(&sandbox, "ops")["unlisted"], true);
    assert_eq!(message_count(&sandbox, "ops"), before, "history untouched");
    assert!(names(&channels(&sandbox, &alpha, &[])).is_empty());

    let again = chat(&sandbox, &beta, &["ops", "--unlist"]);
    assert_eq!(again["changed"], false);
    assert_eq!(again["unlisted"], true);

    let relisted = chat(&sandbox, &alpha, &["ops", "--relist"]);
    assert_eq!(relisted["unlisted"], false);
    assert_eq!(relisted["changed"], true);
    assert!(channel_json(&sandbox, "ops").get("unlisted").is_none());
    assert_eq!(names(&channels(&sandbox, &alpha, &[])), ["ops"]);
    let relisted_again = chat(&sandbox, &alpha, &["ops", "--relist"]);
    assert_eq!(relisted_again["changed"], false);

    let text = sandbox.run_in(&["chat", "ops", "--unlist"], None, &alpha);
    assert_success(&text);
    assert!(
        String::from_utf8_lossy(&text.stdout).starts_with("post: unlisted #ops;"),
        "{}",
        String::from_utf8_lossy(&text.stdout)
    );

    // Never creates a channel.
    let missing = sandbox.run_in(&["chat", "ghost", "--unlist", "--json"], None, &alpha);
    assert_eq!(missing.status.code(), Some(66));
    let error: Value = from_stderr(&missing);
    assert_eq!(error["error"]["code"], "not_found", "{error}");
    assert!(!sandbox.mail_root.join("channels/ghost").exists());

    // The forms are alternatives.
    for refused in [
        &["chat", "ops", "--unlist", "--relist"][..],
        &["chat", "ops", "--unlist", "--archive"][..],
        &["chat", "ops", "--unlist", "--join"][..],
        &["chat", "ops", "--unlisted"][..],
    ] {
        let output = sandbox.run_in(refused, None, &alpha);
        assert!(!output.status.success(), "{refused:?} must be refused");
    }
}

#[test]
fn listing_counts_each_hidden_channel_once_and_flags_select_exactly() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    for name in ["live", "arch", "hid", "both"] {
        join_channel(&sandbox, name, &alpha);
    }
    chat(&sandbox, &alpha, &["arch", "--archive"]);
    chat(&sandbox, &alpha, &["hid", "--unlist"]);
    chat(&sandbox, &alpha, &["both", "--unlist"]);
    chat(&sandbox, &alpha, &["both", "--archive"]);

    let default = channels(&sandbox, &alpha, &[]);
    assert_eq!(names(&default), ["live"]);
    assert_eq!(default["archived_hidden"], 1, "{default}");
    assert_eq!(default["unlisted_hidden"], 2, "{default}");

    // --archived shows listed archived channels only; unlisted stay hidden.
    let archived = channels(&sandbox, &alpha, &["--archived"]);
    assert_eq!(names(&archived), ["arch"]);
    assert_eq!(archived["unlisted_hidden"], 2, "{archived}");
    assert_eq!(archived["archived_hidden"], 0, "{archived}");

    // --unlisted shows every unlisted channel, archived or not.
    let unlisted = channels(&sandbox, &alpha, &["--unlisted"]);
    assert_eq!(names(&unlisted), ["both", "hid"]);
    assert_eq!(unlisted["channels"][0]["archived"], true);
    assert_eq!(unlisted["channels"][0]["unlisted"], true);
    assert_eq!(unlisted["channels"][1]["archived"], false);
    assert_eq!(unlisted["unlisted_hidden"], 0);
    assert_eq!(unlisted["archived_hidden"], 1, "{unlisted}");

    let all = channels(&sandbox, &alpha, &["--all"]);
    assert_eq!(names(&all), ["arch", "both", "hid", "live"]);
    assert_eq!(all["archived_hidden"], 0);
    assert_eq!(all["unlisted_hidden"], 0);
    for item in all["channels"].as_array().unwrap() {
        let name = item["name"].as_str().unwrap();
        assert_eq!(
            item["unlisted"],
            name == "hid" || name == "both",
            "{name}: {item}"
        );
    }

    for refused in [
        &["channels", "--unlisted", "--all"][..],
        &["channels", "--unlisted", "--archived"][..],
    ] {
        let output = sandbox.run_in(refused, None, &alpha);
        assert!(!output.status.success(), "{refused:?} must be refused");
    }
}

#[test]
fn description_updates_and_later_joins_keep_the_channel_unlisted() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    chat(&sandbox, &alpha, &["agents-s2", "--join", "--unlisted"]);

    // A member's description update rewrites channel.json; the flag survives.
    chat(
        &sandbox,
        &alpha,
        &["agents-s2", "--join", "--description", "session notes"],
    );
    let info = channel_json(&sandbox, "agents-s2");
    assert_eq!(info["description"], "session notes");
    assert_eq!(info["unlisted"], true, "{info}");

    // A new member's join with a description (the non-member rewrite path)
    // keeps it too, and a plain join reports the state without relisting.
    let joined = chat(
        &sandbox,
        &beta,
        &["agents-s2", "--join", "--description", "renamed"],
    );
    assert_eq!(joined["unlisted"], true, "{joined}");
    let info = channel_json(&sandbox, "agents-s2");
    assert_eq!(info["description"], "renamed");
    assert_eq!(info["unlisted"], true, "{info}");
    let plain = chat(&sandbox, &beta, &["agents-s2", "--join"]);
    assert_eq!(plain["already_member"], true);
    assert_eq!(plain["unlisted"], true, "{plain}");
    assert_eq!(
        names(&channels(&sandbox, &alpha, &["--unlisted"])),
        ["agents-s2"]
    );
}

#[test]
fn join_unlisted_marks_an_existing_listed_channel() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    join_channel(&sandbox, "ops", &alpha);
    join_channel(&sandbox, "dev", &alpha);

    // A new member's join.
    let joined = chat(&sandbox, &beta, &["ops", "--join", "--unlisted"]);
    assert_eq!(joined["created"], false);
    assert_eq!(joined["already_member"], false);
    assert_eq!(joined["unlisted"], true, "{joined}");
    // An existing member's join.
    let again = chat(&sandbox, &alpha, &["dev", "--join", "--unlisted"]);
    assert_eq!(again["already_member"], true);
    assert_eq!(again["unlisted"], true, "{again}");

    assert!(names(&channels(&sandbox, &alpha, &[])).is_empty());
    assert_eq!(channel_json(&sandbox, "ops")["unlisted"], true);
    assert_eq!(channel_json(&sandbox, "dev")["unlisted"], true);
}

#[test]
fn a_refused_look_alike_join_suggests_a_fix_that_stays_unlisted() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    join_channel(&sandbox, "night-porch", &alpha);
    let refused = sandbox.run_in(
        &["chat", "night-porsh", "--join", "--unlisted", "--json"],
        None,
        &alpha,
    );
    assert!(!refused.status.success());
    let error: Value = from_stderr(&refused);
    assert_eq!(
        error["error"]["details"]["exact_fix"], "post chat 'night-porch' --join --unlisted",
        "{error}"
    );
    assert!(
        error["error"]["suggested_fix"]
            .as_str()
            .is_some_and(|fix| fix.contains("--join --unlisted --create")),
        "{error}"
    );
}
