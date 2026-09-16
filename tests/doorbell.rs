mod common;

use common::{assert_success, from_stdout, join_channel, register_alpha_beta, Sandbox};
use serde_json::Value;

#[test]
fn watch_snapshot_is_cursorless_then_channel_catchup_consumes_for_participant() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    join_channel(&sandbox, "doorbell", &alpha);
    join_channel(&sandbox, "doorbell", &beta);
    let sent = sandbox.run_in(
        &[
            "chat", "doorbell", "--send", "--anyway", "--body", "ring", "--json",
        ],
        None,
        &alpha,
    );
    assert_success(&sent);
    let sent: Value = from_stdout(&sent);
    let id = sent["message"]["id"].as_str().expect("message id");
    let participant = sandbox.test_participant("beta");
    let cursor = sandbox
        .mail_root
        .join("participants")
        .join(&participant)
        .join("cursors.json");
    let before = std::fs::read(&cursor).ok();

    let snapshot = sandbox.run_in(&["watch", "--snapshot", "--json"], None, &beta);
    assert_success(&snapshot);
    assert!(common::stdout(&snapshot).contains(id));
    assert_eq!(std::fs::read(&cursor).ok(), before);

    let catchup = sandbox.run_in(&["catchup", "doorbell", "--json"], None, &beta);
    assert_success(&catchup);
    let catchup: Value = from_stdout(&catchup);
    assert!(catchup["targets"][0]["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .any(|message| message["id"] == id));
    let cursor: Value =
        serde_json::from_slice(&std::fs::read(&cursor).expect("participant cursor after catchup"))
            .expect("cursor JSON");
    assert!(cursor["channels"]["doorbell"]["seen"]
        .as_array()
        .expect("seen ids")
        .iter()
        .any(|seen| seen == id));
}

#[test]
fn direct_mail_snapshot_never_consumes_or_moves_canonical_file() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let sent = sandbox.run_in(
        &[
            "send",
            "--to",
            "workspace:beta",
            "--body",
            "mail ring",
            "--json",
        ],
        None,
        &alpha,
    );
    assert_success(&sent);
    let sent: Value = from_stdout(&sent);
    let id = sent["envelope"]["id"].as_str().expect("mail id");
    let beta_participant = sandbox.test_participant("beta");
    let snapshot =
        sandbox.run_as_participant(&["watch", "--snapshot", "--json"], &beta_participant, &beta);
    assert_success(&snapshot);
    assert!(common::stdout(&snapshot).contains(id));
    assert!(sandbox
        .mail_root
        .join(format!("beta/inbox/{id}.mail"))
        .is_file());
    let inbox = sandbox.run_as_participant(&["inbox", "--json"], &beta_participant, &beta);
    assert_success(&inbox);
    let inbox: Value = from_stdout(&inbox);
    assert_eq!(inbox["unread_count"], 1);
}
