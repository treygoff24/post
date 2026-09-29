//! The channels lane of the 2026-09-28 "post just works" wave.
//!
//! Channels are post's main use: 2-8 agents in long project rooms. These tests
//! pin the loop staying smooth and never wedged: an unknown event kind or a
//! corrupt file is skipped and reported instead of failing a reader, a crossed
//! send always delivers and shows what crossed, and the names an agent guesses
//! (`#ops`, `Ops_Room`, a room name, a stray positional) get a useful answer.
//!
//! Every test roots its store in a throwaway sandbox (`Sandbox` clears the
//! identity environment and sets POST_MAIL_ROOT); nothing touches the real mail
//! store.

mod common;

use common::{
    assert_success, from_stderr, from_stdout, join_channel, register_alpha_beta, register_room,
    stderr, stdout, write_bad_channel, write_channel_message, Sandbox,
};
use post::output::ErrorEnvelope;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

const A: &str = "20260922-163423-000001-abcdef";
const E: &str = "20260922-163423-000002-abcdef";
const B: &str = "20260922-163423-000003-abcdef";
const C: &str = "20260922-163423-000004-c0ffee";

/// Two registered rooms, both joined to `ops` from the start of its history.
fn ops_room() -> (Sandbox, PathBuf, PathBuf) {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    join_channel(&sandbox, "ops", &alpha);
    join_channel(&sandbox, "ops", &beta);
    // Each room's own join left a system event for the other to read; consume
    // them so a test's unread counts are exactly the messages it writes.
    for room in [&alpha, &beta] {
        assert_success(&sandbox.run_in(&["chat", "ops", "--json"], None, room));
    }
    (sandbox, alpha, beta)
}

fn run(sandbox: &Sandbox, args: &[&str], cwd: &Path) -> std::process::Output {
    sandbox.run_in(args, None, cwd)
}

fn ok_json(sandbox: &Sandbox, args: &[&str], cwd: &Path) -> Value {
    let output = run(sandbox, args, cwd);
    assert_success(&output);
    from_stdout(&output)
}

/// A hand-written message file with arbitrary extra envelope fields, the way a
/// newer peer or the bridge would leave one.
fn write_raw_message(sandbox: &Sandbox, channel: &str, id: &str, extra: Value, body: &str) {
    let mut envelope = json!({
        "id": id,
        "from": "alpha",
        "channel": channel,
        "subject": "",
        "sent": "2026-09-22 16:34:23 -0500",
    });
    for (key, value) in extra.as_object().expect("extra fields") {
        envelope[key] = value.clone();
    }
    let path = sandbox
        .mail_root
        .join("channels")
        .join(channel)
        .join("messages")
        .join(format!("{id}.msg"));
    fs::write(
        path,
        format!(
            "{}\n---\n{body}",
            serde_json::to_string_pretty(&envelope).expect("envelope")
        ),
    )
    .expect("write raw channel message");
}

fn write_corrupt_message(sandbox: &Sandbox, channel: &str, id: &str) {
    let path = sandbox
        .mail_root
        .join("channels")
        .join(channel)
        .join("messages")
        .join(format!("{id}.msg"));
    fs::write(
        path,
        format!("{{ \"id\": \"{id}\", \"from\": \"alpha\", BROKEN"),
    )
    .expect("corrupt");
}

fn message_ids(value: &Value) -> Vec<String> {
    value["messages"]
        .as_array()
        .expect("messages array")
        .iter()
        .map(|message| message["id"].as_str().expect("id").to_owned())
        .collect()
}

fn channel_row<'a>(channels: &'a Value, name: &str) -> &'a Value {
    channels["channels"]
        .as_array()
        .expect("channels array")
        .iter()
        .find(|row| row["name"] == name)
        .unwrap_or_else(|| panic!("channel {name} listed in {channels}"))
}

// ---------------------------------------------------------------------------
// 1. Unknown event kinds are opaque system events, never a wedge.
// ---------------------------------------------------------------------------

#[test]
fn a_future_event_kind_passes_through_every_reader_and_a_send() {
    let (sandbox, _alpha, beta) = ops_room();
    write_channel_message(&sandbox, "ops", A, "alpha", "", "hello from alpha");
    write_raw_message(
        &sandbox,
        "ops",
        E,
        json!({"event": "future_kind"}),
        "=== something new happened ===",
    );
    write_channel_message(&sandbox, "ops", B, "alpha", "", "second from alpha");

    // Listing: the opaque event is not an unread message.
    let channels = ok_json(&sandbox, &["channels"], &beta);
    assert_eq!(
        channel_row(&channels, "ops")["unread"],
        2,
        "the opaque event must not count as unread: {channels}"
    );

    // --peek and the consuming read leave it out of the unread selection.
    let peeked = ok_json(&sandbox, &["chat", "ops", "--peek", "--json"], &beta);
    assert_eq!(message_ids(&peeked), vec![A, B], "peek: {peeked}");

    // --history shows it, kind intact in JSON and as `[event: <kind>]` in text.
    let history = ok_json(
        &sandbox,
        &["chat", "ops", "--history", "10", "--json"],
        &beta,
    );
    let event = history["messages"]
        .as_array()
        .expect("history messages")
        .iter()
        .find(|message| message["id"] == E)
        .unwrap_or_else(|| panic!("history must carry the opaque event: {history}"));
    assert_eq!(event["event"], "future_kind");
    let text = run(&sandbox, &["chat", "ops", "--history", "10"], &beta);
    assert_success(&text);
    assert!(
        stdout(&text).contains("[event: future_kind]"),
        "text history must render the kind: {}",
        stdout(&text)
    );

    // Search reads the whole history, event included.
    let found = ok_json(&sandbox, &["search", "something new", "--json"], &beta);
    assert_eq!(found["count"], 1, "search: {found}");

    // A send with unread traffic and the event both in the channel delivers.
    let sent = ok_json(
        &sandbox,
        &["chat", "ops", "--send", "--body", "reply", "--json"],
        &beta,
    );
    assert_eq!(sent["ok"], true);
    assert_eq!(
        sent["crossed"]["unseen"], 2,
        "the opaque event is not a crossed message: {sent}"
    );

    // The consuming read succeeds and is never blocked by the event.
    let read = ok_json(&sandbox, &["chat", "ops", "--json"], &beta);
    assert_eq!(message_ids(&read), vec![A, B], "read: {read}");
    let again = ok_json(&sandbox, &["chat", "ops", "--json"], &beta);
    assert_eq!(again["count"], 0, "the cursor advanced past both: {again}");

    // --discard, --discard-through and catchup all cross the event.
    write_channel_message(
        &sandbox,
        "ops",
        "20260922-163423-000010-abcdef",
        "alpha",
        "",
        "third",
    );
    let discarded = ok_json(&sandbox, &["chat", "ops", "--discard", "--json"], &beta);
    assert_eq!(discarded["discarded"], 1, "discard: {discarded}");
    let through = ok_json(
        &sandbox,
        &["chat", "ops", "--discard-through", E, "--json"],
        &beta,
    );
    assert_eq!(through["ok"], true, "discard-through: {through}");
    write_channel_message(
        &sandbox,
        "ops",
        "20260922-163423-000011-abcdef",
        "alpha",
        "",
        "fourth",
    );
    let caught = ok_json(&sandbox, &["catchup", "--json"], &beta);
    assert_eq!(caught["ok"], true, "catchup: {caught}");
    let ops = caught["targets"]
        .as_array()
        .expect("targets")
        .iter()
        .find(|target| target["channel"] == "ops")
        .expect("ops target");
    assert_eq!(ops["count"], 1, "catchup: {caught}");
}

// ---------------------------------------------------------------------------
// 2. A corrupt file is skipped and reported on stdout, never a failed command.
// ---------------------------------------------------------------------------

#[test]
fn a_corrupt_message_file_is_skipped_and_reported_by_every_reader() {
    let (sandbox, _alpha, beta) = ops_room();
    write_channel_message(&sandbox, "ops", A, "alpha", "", "hello from alpha");
    write_channel_message(&sandbox, "ops", B, "alpha", "", "second from alpha");
    write_corrupt_message(&sandbox, "ops", C);

    let skipped_ids = |value: &Value, key: &str| -> Vec<String> {
        value[key]
            .as_array()
            .unwrap_or_else(|| panic!("`{key}` array missing: {value}"))
            .iter()
            .map(|entry| {
                assert!(
                    entry["reason"]
                        .as_str()
                        .is_some_and(|reason| !reason.is_empty()),
                    "every skipped entry carries a reason: {value}"
                );
                entry["id"].as_str().expect("skipped id").to_owned()
            })
            .collect()
    };

    let channels = ok_json(&sandbox, &["channels"], &beta);
    assert_eq!(skipped_ids(&channels, "skipped"), vec![C], "{channels}");
    assert_eq!(channel_row(&channels, "ops")["unread"], 2, "{channels}");

    let text = run(&sandbox, &["channels", "--text"], &beta);
    assert_success(&text);
    assert_eq!(
        stdout(&text)
            .lines()
            .filter(|line| line.contains(C))
            .count(),
        1,
        "text reports the skipped file once, on one line: {}",
        stdout(&text)
    );

    let found = ok_json(&sandbox, &["search", "hello", "--json"], &beta);
    assert_eq!(
        found["count"], 1,
        "search still finds the good message: {found}"
    );
    assert_eq!(skipped_ids(&found, "skipped"), vec![C], "{found}");

    // `chat` already uses `skipped` for the count of window-skipped messages,
    // so its unreadable-file list is `skipped_files`.
    let peek = ok_json(&sandbox, &["chat", "ops", "--peek", "--json"], &beta);
    assert_eq!(message_ids(&peek), vec![A, B], "{peek}");
    assert_eq!(skipped_ids(&peek, "skipped_files"), vec![C], "{peek}");
    let history = ok_json(
        &sandbox,
        &["chat", "ops", "--history", "5", "--json"],
        &beta,
    );
    assert_eq!(skipped_ids(&history, "skipped_files"), vec![C], "{history}");
    let peek_text = run(&sandbox, &["chat", "ops", "--peek"], &beta);
    assert_success(&peek_text);
    assert!(
        stdout(&peek_text).contains(C) && stdout(&peek_text).contains("skipped"),
        "text reads report the skipped file on stdout: {}",
        stdout(&peek_text)
    );
    assert!(
        !stderr(&peek_text).contains(C),
        "the report is stdout, not only stderr: {}",
        stderr(&peek_text)
    );

    // A send delivers even though an unseen file cannot be parsed.
    let sent = ok_json(
        &sandbox,
        &["chat", "ops", "--send", "--body", "reply", "--json"],
        &beta,
    );
    assert_eq!(sent["ok"], true);

    let caught = ok_json(&sandbox, &["catchup", "--json"], &beta);
    assert_eq!(skipped_ids(&caught, "skipped"), vec![C], "{caught}");
    let ops = caught["targets"]
        .as_array()
        .expect("targets")
        .iter()
        .find(|target| target["channel"] == "ops")
        .expect("ops target");
    assert_eq!(ops["count"], 2, "the good messages still arrive: {caught}");

    // The consuming read now finds nothing new; the corrupt file never blocks.
    let read = ok_json(&sandbox, &["chat", "ops", "--json"], &beta);
    assert_eq!(read["count"], 0, "{read}");
    assert_eq!(skipped_ids(&read, "skipped_files"), vec![C], "{read}");
    let discarded = ok_json(&sandbox, &["chat", "ops", "--discard", "--json"], &beta);
    assert_eq!(discarded["ok"], true, "{discarded}");
}

// ---------------------------------------------------------------------------
// 3. Crossed sends always deliver and show what crossed.
// ---------------------------------------------------------------------------

#[test]
fn a_crossed_send_delivers_and_reports_what_crossed_without_marking_it_read() {
    let (sandbox, alpha, beta) = ops_room();
    let long_other = "x".repeat(1000);
    for body in ["@beta please look at the deploy", long_other.as_str()] {
        assert_success(&run(
            &sandbox,
            &["chat", "ops", "--send", "--body", body, "--json"],
            &alpha,
        ));
    }

    let sent = ok_json(
        &sandbox,
        &["chat", "ops", "--send", "--body", "on it", "--json"],
        &beta,
    );
    assert_eq!(sent["ok"], true, "a crossed send delivers: {sent}");
    let crossed = &sent["crossed"];
    assert_eq!(crossed["unseen"], 2, "{sent}");
    assert_eq!(crossed["addressed_to_you"], 1, "{sent}");
    let messages = crossed["messages"].as_array().expect("crossed messages");
    assert_eq!(messages.len(), 2);
    // Newest last.
    assert!(messages[0]["id"].as_str() < messages[1]["id"].as_str());
    assert_eq!(messages[0]["addressed_to_you"], true);
    assert_eq!(messages[0]["body"], "@beta please look at the deploy");
    assert_eq!(messages[0]["from"], "alpha");
    for field in [
        "id",
        "from",
        "display_name",
        "sent",
        "addressed_to_you",
        "body",
    ] {
        assert!(
            messages[0].get(field).is_some(),
            "crossed message carries `{field}`: {sent}"
        );
    }
    assert_eq!(messages[1]["addressed_to_you"], false);
    let preview = messages[1]["body"].as_str().expect("preview");
    assert!(
        preview.chars().count() <= 301 && preview.starts_with(&"x".repeat(300)),
        "a non-addressed message is a 300-character preview: {} chars",
        preview.chars().count()
    );

    // Sending did not mark the crossed messages read.
    let still_unread = ok_json(&sandbox, &["chat", "ops", "--peek", "--json"], &beta);
    assert_eq!(still_unread["count"], 2, "{still_unread}");

    // The audit log keeps working, with the new outcome.
    let log = fs::read_to_string(sandbox.mail_root.join("crossed-send.jsonl"))
        .expect("crossed-send.jsonl");
    let event: Value = serde_json::from_str(log.lines().last().expect("a log line")).expect("json");
    assert_eq!(event["outcome"], "delivered_crossed", "{log}");
    assert_eq!(event["unseen"], 2);
    assert_eq!(event["targeted"], 1);
}

#[test]
fn a_clean_send_carries_no_crossed_block() {
    let (sandbox, alpha, _beta) = ops_room();
    let sent = ok_json(
        &sandbox,
        &["chat", "ops", "--send", "--body", "quiet room", "--json"],
        &alpha,
    );
    assert!(sent.get("crossed").is_none(), "nothing crossed: {sent}");
    assert!(
        !sandbox.mail_root.join("crossed-send.jsonl").exists(),
        "a clean send logs nothing"
    );
}

#[test]
fn crossed_text_receipt_lists_addressed_messages_first_and_in_full() {
    let (sandbox, alpha, beta) = ops_room();
    let multi = "@beta first line\nsecond line of the request\nthird line";
    let sends = [
        ("ordinary chatter that concerns nobody", "1"),
        (multi, "2"),
        ("more chatter", "3"),
    ];
    for (body, _) in sends {
        assert_success(&run(
            &sandbox,
            &["chat", "ops", "--send", "--body", body],
            &alpha,
        ));
    }
    let output = run(&sandbox, &["chat", "ops", "--send", "--body", "ack"], &beta);
    assert_success(&output);
    let text = stdout(&output);
    let mut lines = text.lines();
    assert!(
        lines
            .next()
            .is_some_and(|line| line.starts_with("post: sent #ops ")),
        "the sent line leads: {text}"
    );
    let addressed_at = text
        .find("second line of the request")
        .expect("addressed body in full");
    let chatter_at = text
        .find("ordinary chatter")
        .expect("other message previewed");
    assert!(
        addressed_at < chatter_at,
        "addressed messages come first even though they arrived later: {text}"
    );
    assert!(
        text.contains("third line"),
        "addressed body is complete: {text}"
    );
    assert!(
        text.contains("3 unseen") && text.contains("1 addressed to you"),
        "the summary counts both: {text}"
    );
    assert!(
        !stderr(&output).contains("unseen"),
        "the crossing is stdout, not a stderr warning: {}",
        stderr(&output)
    );
}

#[test]
fn a_reply_to_your_message_counts_as_addressed_and_the_preview_is_capped_at_ten() {
    let (sandbox, alpha, beta) = ops_room();
    let mine = ok_json(
        &sandbox,
        &[
            "chat",
            "ops",
            "--send",
            "--body",
            "beta's question",
            "--json",
        ],
        &beta,
    );
    let mine_id = mine["message"]["id"].as_str().expect("id").to_owned();
    // alpha replies (addressed by reply, not by mention), then 12 more.
    assert_success(&run(
        &sandbox,
        &[
            "chat",
            "ops",
            "--send",
            "--re",
            &mine_id,
            "--body",
            "the answer",
        ],
        &alpha,
    ));
    for n in 0..12 {
        let body = format!("noise {n}");
        assert_success(&run(
            &sandbox,
            &["chat", "ops", "--send", "--body", &body],
            &alpha,
        ));
    }
    let sent = ok_json(
        &sandbox,
        &["chat", "ops", "--send", "--body", "thanks", "--json"],
        &beta,
    );
    let crossed = &sent["crossed"];
    assert_eq!(crossed["unseen"], 13, "{sent}");
    assert_eq!(crossed["addressed_to_you"], 1, "{sent}");
    let messages = crossed["messages"].as_array().expect("messages");
    assert_eq!(messages.len(), 10, "at most ten are shown: {sent}");
    assert!(
        messages
            .iter()
            .any(|m| m["body"] == "the answer" && m["addressed_to_you"] == true),
        "the reply is kept even though ten newer messages exist: {sent}"
    );
    let ids: Vec<&str> = messages
        .iter()
        .map(|m| m["id"].as_str().expect("id"))
        .collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    assert_eq!(ids, sorted, "newest last: {sent}");
}

#[test]
fn anyway_is_a_hidden_no_op_and_no_hint_still_names_it() {
    let (sandbox, alpha, beta) = ops_room();
    assert_success(&run(
        &sandbox,
        &["chat", "ops", "--send", "--body", "@beta look"],
        &alpha,
    ));
    // Habitual commands keep working, crossed or not.
    let sent = ok_json(
        &sandbox,
        &[
            "chat", "ops", "--send", "--anyway", "--body", "ok", "--json",
        ],
        &beta,
    );
    assert_eq!(sent["ok"], true);
    assert_eq!(sent["crossed"]["addressed_to_you"], 1, "{sent}");

    for args in [
        &["chat", "--help"][..],
        &["--help"][..],
        &["send", "--help"][..],
    ] {
        let help = run(&sandbox, args, &alpha);
        assert!(
            !stdout(&help).contains("anyway"),
            "`post {}` help must not teach --anyway",
            args.join(" ")
        );
    }
}

// ---------------------------------------------------------------------------
// 4. Joining and naming.
// ---------------------------------------------------------------------------

#[test]
fn a_new_channel_name_is_normalized_and_odd_characters_are_refused_with_the_fix() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);

    let created = ok_json(
        &sandbox,
        &["chat", "Night_Porch Two", "--join", "--json"],
        &alpha,
    );
    assert_eq!(created["channel"], "night-porch-two", "{created}");
    assert_eq!(created["created"], true);
    assert_eq!(created["normalized_from"], "Night_Porch Two");
    assert!(sandbox
        .mail_root
        .join("channels/night-porch-two/channel.json")
        .is_file());
    assert!(!sandbox.mail_root.join("channels/Night_Porch Two").exists());

    let refused = run(&sandbox, &["chat", "Ops Room!", "--join", "--json"], &alpha);
    assert_eq!(refused.status.code(), Some(2), "{}", stderr(&refused));
    let error: ErrorEnvelope = from_stderr(&refused);
    assert_eq!(error.error.code, "invalid_argument");
    assert_eq!(
        error.error.details.exact_fix.as_deref(),
        Some("post chat 'ops-room' --join"),
        "the fix names the normalized form: {}",
        stderr(&refused)
    );
    assert!(!sandbox.mail_root.join("channels/ops-room").exists());
    let ran = sandbox.run_fix("post chat 'ops-room' --join", &alpha);
    assert_success(&ran);
    assert!(sandbox
        .mail_root
        .join("channels/ops-room/channel.json")
        .is_file());
}

#[test]
fn creating_a_channel_next_to_a_near_duplicate_asks_first() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    ok_json(
        &sandbox,
        &["chat", "night-porch", "--join", "--json"],
        &alpha,
    );

    // Another spelling of a channel that exists is that channel: nothing is
    // created, so a second agent told to join `Night Porch` just lands in it.
    for spelling in ["night porch", "Night_Porch", "#Night-Porch"] {
        let joined = ok_json(&sandbox, &["chat", spelling, "--join", "--json"], &alpha);
        assert_eq!(joined["channel"], "night-porch", "{spelling}: {joined}");
        assert_eq!(joined["created"], false, "{spelling}: {joined}");
    }
    assert!(!sandbox.mail_root.join("channels/night porch").exists());

    for guess in ["nightporch", "night-porsh"] {
        let refused = run(&sandbox, &["chat", guess, "--join", "--json"], &alpha);
        assert_eq!(
            refused.status.code(),
            Some(2),
            "{guess}: {}",
            stderr(&refused)
        );
        let error: ErrorEnvelope = from_stderr(&refused);
        assert!(
            error.error.message.contains("did you mean #night-porch"),
            "{guess}: {}",
            error.error.message
        );
        assert_eq!(
            error.error.details.exact_fix.as_deref(),
            Some("post chat 'night-porch' --join"),
            "{guess}"
        );
        assert!(
            error.error.suggested_fix.contains("--create"),
            "{guess}: the fix must say how to force a new channel: {}",
            error.error.suggested_fix
        );
        assert!(
            !sandbox.mail_root.join("channels").join(guess).exists(),
            "{guess}: a refused join creates nothing"
        );
    }

    // The exact fix is the join that was meant, and it runs as written.
    let ran = sandbox.run_fix("post chat 'night-porch' --join", &alpha);
    assert_success(&ran);

    // --create forces a genuinely new channel.
    let forced = ok_json(
        &sandbox,
        &["chat", "nightporch", "--join", "--create", "--json"],
        &alpha,
    );
    assert_eq!(forced["channel"], "nightporch");
    assert_eq!(forced["created"], true);

    // Numbered siblings are not near-duplicates of each other.
    ok_json(&sandbox, &["chat", "ops-1", "--join", "--json"], &alpha);
    let sibling = ok_json(&sandbox, &["chat", "ops-2", "--join", "--json"], &alpha);
    assert_eq!(sibling["created"], true, "{sibling}");
}

#[test]
fn an_existing_odd_channel_name_stays_reachable_by_its_exact_name() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    write_bad_channel(
        &sandbox,
        "Night Porch",
        Some(r#"{"alpha":"2026-09-22 16:34:23 +0000"}"#),
        true,
        r#"{"name":"Night Porch","created":"2026-09-22 16:34:23 +0000","created_by":"alpha"}"#,
    );
    write_channel_message(&sandbox, "Night Porch", A, "beta", "", "old history");
    ok_json(
        &sandbox,
        &["chat", "night-porch", "--join", "--create", "--json"],
        &alpha,
    );

    let read = ok_json(
        &sandbox,
        &["chat", "Night Porch", "--history", "5", "--json"],
        &alpha,
    );
    assert_eq!(message_ids(&read), vec![A], "{read}");
    let joined = ok_json(
        &sandbox,
        &["chat", "Night Porch", "--join", "--json"],
        &alpha,
    );
    assert_eq!(joined["channel"], "Night Porch");
    assert_eq!(
        joined["created"], false,
        "joining an existing name never creates: {joined}"
    );
    assert!(
        joined.get("normalized_from").is_none(),
        "an existing name is used as given: {joined}"
    );
}

#[test]
fn a_leading_hash_is_accepted_wherever_a_channel_name_is() {
    let (sandbox, alpha, beta) = ops_room();
    let joined = ok_json(&sandbox, &["chat", "#fresh", "--join", "--json"], &alpha);
    assert_eq!(joined["channel"], "fresh", "{joined}");
    assert!(!sandbox.mail_root.join("channels/#fresh").exists());

    let sent = ok_json(
        &sandbox,
        &[
            "chat",
            "#ops",
            "--send",
            "--body",
            "via the rendered name",
            "--json",
        ],
        &alpha,
    );
    assert_eq!(sent["message"]["channel"], "ops", "{sent}");
    let read = ok_json(&sandbox, &["chat", "#ops", "--peek", "--json"], &beta);
    assert_eq!(read["channel"], "ops", "{read}");
    assert_eq!(read["count"], 1);
    let found = ok_json(
        &sandbox,
        &["search", "rendered", "--channel", "#ops", "--json"],
        &beta,
    );
    assert_eq!(found["count"], 1, "{found}");
    let caught = ok_json(&sandbox, &["catchup", "#ops", "--json"], &beta);
    assert_eq!(caught["targets"][0]["channel"], "ops", "{caught}");
}

#[test]
fn a_channel_literally_named_with_a_hash_keeps_working() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    write_bad_channel(
        &sandbox,
        "#legacy",
        Some(r#"{"alpha":"2026-09-22 16:34:23 +0000"}"#),
        true,
        r##"{"name":"#legacy","created":"2026-09-22 16:34:23 +0000","created_by":"alpha"}"##,
    );
    write_channel_message(&sandbox, "#legacy", A, "beta", "", "legacy history");
    let read = ok_json(&sandbox, &["chat", "#legacy", "--json"], &alpha);
    assert_eq!(message_ids(&read), vec![A], "{read}");
}

// ---------------------------------------------------------------------------
// 5. A room is not a channel.
// ---------------------------------------------------------------------------

#[test]
fn chat_with_a_room_name_says_it_is_a_room_and_agrees_on_the_exit_code() {
    let (sandbox, _alpha, beta) = ops_room();
    let gamma = sandbox.path.join("gamma");
    fs::create_dir(&gamma).expect("gamma path");
    register_room(&sandbox, "gamma", &gamma);

    for args in [
        vec!["chat", "gamma"],
        vec!["chat", "gamma", "--json"],
        vec!["chat", "gamma", "--peek"],
        vec!["chat", "gamma", "--history", "5"],
        vec!["chat", "gamma", "--send", "--body", "hi"],
        vec!["chat", "gamma", "--discard"],
    ] {
        let output = run(&sandbox, &args, &beta);
        assert_eq!(
            output.status.code(),
            Some(66),
            "{args:?} must be not_found (66), stderr: {}",
            stderr(&output)
        );
        let text = stderr(&output);
        assert!(text.contains("registered room"), "{args:?}: {text}");
        assert!(text.contains("post send --to 'gamma'"), "{args:?}: {text}");
        assert!(
            !text.contains("--join"),
            "{args:?} must never steer toward creating a channel: {text}"
        );
    }

    // A name that is neither a room nor a channel keeps the create hint.
    let nowhere = run(&sandbox, &["chat", "nosuchthing", "--peek"], &beta);
    assert_eq!(nowhere.status.code(), Some(66), "{}", stderr(&nowhere));
    assert!(stderr(&nowhere).contains("--join"), "{}", stderr(&nowhere));
}

// ---------------------------------------------------------------------------
// 6. No banners on stderr under --json.
// ---------------------------------------------------------------------------

#[test]
fn chat_json_prints_nothing_on_stderr() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let joined = run(&sandbox, &["chat", "quiet", "--join", "--json"], &alpha);
    assert_eq!(joined.status.code(), Some(0));
    assert_eq!(stderr(&joined), "", "join --json: stderr must be empty");
    let sent = run(
        &sandbox,
        &["chat", "quiet", "--send", "--body", "hello", "--json"],
        &alpha,
    );
    assert_eq!(sent.status.code(), Some(0));
    // The one line a send may still print is the delivery diagnostic (no bridge
    // here, so "sent locally only"); the identity banner is gone.
    let leftover: Vec<String> = stderr(&sent)
        .lines()
        .filter(|line| !line.contains("sent locally only"))
        .map(str::to_owned)
        .collect();
    assert!(
        leftover.is_empty(),
        "send --json: no identity banner on stderr: {leftover:?}"
    );
    let receipt: Value = from_stdout(&sent);
    assert_eq!(receipt["cross_host"]["status"], "local_only", "{receipt}");

    // Text mode keeps its identity line for humans.
    let text = run(
        &sandbox,
        &["chat", "quiet", "--send", "--body", "again"],
        &alpha,
    );
    assert_success(&text);
    assert!(
        stderr(&text).contains("sending to #quiet"),
        "{}",
        stderr(&text)
    );
}

// ---------------------------------------------------------------------------
// 7. A stray positional is never a file and never a body.
// ---------------------------------------------------------------------------

#[test]
fn a_stray_positional_after_the_channel_is_refused_and_names_the_body_forms() {
    let (sandbox, alpha, _beta) = ops_room();
    let before = fs::read_dir(sandbox.mail_root.join("channels/ops/messages"))
        .expect("messages")
        .count();
    let dir = alpha.clone();
    fs::write(dir.join("hello"), "FILE CONTENTS MUST NOT BE SENT").expect("decoy file");

    for args in [
        vec!["chat", "ops", "--send", "hello"],
        vec!["chat", "ops", "--send", "hello world"],
        vec!["chat", "ops", "--send", "hello", "world", "--json"],
        vec!["chat", "ops", "hello"],
    ] {
        let output = run(&sandbox, &args, &alpha);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            stderr(&output)
        );
        let error: ErrorEnvelope = from_stderr(&output);
        assert_eq!(error.error.code, "invalid_argument", "{args:?}");
        let fix = &error.error.suggested_fix;
        assert!(
            fix.contains("--body-file") && fix.contains("--body") && fix.contains("<<'EOF'"),
            "{args:?}: the refusal names every body form: {fix}"
        );
        assert_eq!(
            error.error.details.exact_fix, None,
            "{args:?}: no command can carry a body nobody gave"
        );
    }
    let after = fs::read_dir(sandbox.mail_root.join("channels/ops/messages"))
        .expect("messages")
        .count();
    assert_eq!(before, after, "a refused positional sends nothing");
}
