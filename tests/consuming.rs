mod common;

use common::{
    assert_success, from_stdout, join_channel, register_alpha_beta, write_channel_message, Sandbox,
};
use serde_json::Value;
use std::fs;

fn participant_cursor(sandbox: &Sandbox, workspace: &str) -> Value {
    let participant = sandbox.test_participant(workspace);
    serde_json::from_slice(
        &fs::read(
            sandbox
                .mail_root
                .join("participants")
                .join(participant)
                .join("cursors.json"),
        )
        .expect("participant cursor"),
    )
    .expect("cursor JSON")
}

fn seen(state: &Value, channel: &str) -> Vec<String> {
    state["channels"][channel]["seen"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

#[cfg(unix)]
#[test]
fn failed_emit_records_no_participant_seen_id() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let sent = sandbox.run_in(
        &[
            "send",
            "--to",
            "workspace:beta",
            "--body",
            "emit failure",
            "--json",
        ],
        None,
        &alpha,
    );
    assert_success(&sent);
    let sent: Value = from_stdout(&sent);
    let id = sent["envelope"]["id"].as_str().expect("mail id");
    let failed = sandbox.run_in_broken_stdout(&["read", id, "--json"], &beta);
    assert_eq!(failed.status.code(), Some(75));
    let error: post::output::ErrorEnvelope = common::from_stderr(&failed);
    assert_eq!(error.error.code, "io_error");
    assert_eq!(
        error.error.details.operation.as_deref(),
        Some("write stdout")
    );
    let participant = sandbox.test_participant("beta");
    let cursor = sandbox
        .mail_root
        .join("participants")
        .join(participant)
        .join("cursors.json");
    if cursor.exists() {
        let state: Value =
            serde_json::from_slice(&fs::read(cursor).expect("cursor")).expect("cursor JSON");
        assert!(!state["mail"]["workspace:beta"]["seen"]
            .as_array()
            .is_some_and(|ids| ids.iter().any(|entry| entry == id)));
    }
    assert!(sandbox
        .mail_root
        .join(format!("beta/inbox/{id}.mail"))
        .is_file());
}

#[test]
fn bounded_channel_consumption_marks_only_emitted_page() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    join_channel(&sandbox, "paged", &alpha);
    join_channel(&sandbox, "paged", &beta);
    assert_success(&sandbox.run_in(&["chat", "paged", "--discard", "--json"], None, &beta));
    let ids = [
        "20000101-000000-000001-aaaa01",
        "20000101-000000-000002-aaaa02",
        "20000101-000000-000003-aaaa03",
    ];
    for id in ids {
        write_channel_message(&sandbox, "paged", id, "alpha", "page", id);
    }
    let first = sandbox.run_in(&["chat", "paged", "--limit", "2", "--json"], None, &beta);
    assert_success(&first);
    let first: Value = from_stdout(&first);
    assert_eq!(first["count"], 2);
    let state = participant_cursor(&sandbox, "beta");
    let seen = seen(&state, "paged");
    assert!(seen.contains(&ids[0].to_owned()));
    assert!(seen.contains(&ids[1].to_owned()));
    assert!(!seen.contains(&ids[2].to_owned()));
    let second = sandbox.run_in(&["chat", "paged", "--json"], None, &beta);
    assert_success(&second);
    let second: Value = from_stdout(&second);
    assert_eq!(second["messages"][0]["id"], ids[2]);
}

#[test]
fn late_older_channel_id_remains_unread_and_seen_by_names_participants() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let alpha_participant = sandbox.test_participant("alpha");
    let beta_participant = sandbox.test_participant("beta");
    join_channel(&sandbox, "late", &alpha);
    join_channel(&sandbox, "late", &beta);
    assert_success(&sandbox.run_in(&["chat", "late", "--discard", "--json"], None, &beta));
    let sent = sandbox.run_in(
        &[
            "chat", "late", "--send", "--anyway", "--body", "newer", "--json",
        ],
        None,
        &alpha,
    );
    assert_success(&sent);
    let sent: Value = from_stdout(&sent);
    let newer = sent["message"]["id"].as_str().expect("newer id").to_owned();
    assert_success(&sandbox.run_in(&["chat", "late", "--json"], None, &beta));
    let older = "20000101-000000-000001-aaaa01";
    write_channel_message(&sandbox, "late", older, "alpha", "older", "older");
    let peek = sandbox.run_in(&["chat", "late", "--peek", "--json"], None, &beta);
    assert_success(&peek);
    let peek: Value = from_stdout(&peek);
    assert!(peek["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .any(|message| message["id"] == older));

    let seen_by = sandbox.run_in(
        &["chat", "late", "--seen-by", &newer, "--json"],
        None,
        &beta,
    );
    assert_success(&seen_by);
    let seen_by: Value = from_stdout(&seen_by);
    let readers = seen_by["seen_by"].as_array().expect("seen by");
    assert!(readers.iter().any(|reader| reader == &beta_participant));
    assert!(readers.iter().any(|reader| reader == &alpha_participant));
}
