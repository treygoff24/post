mod common;

use common::{
    assert_success, from_stderr, from_stdout, join_channel, register_alpha_beta,
    write_channel_message, Sandbox,
};
use post::output::{
    ChatDiscardThroughOutput, ChatReadOutput, ErrorEnvelope, ReadOutput, SeenByOutput,
};
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn cursor(sandbox: &Sandbox, room: &str) -> serde_json::Value {
    serde_json::from_slice(
        &fs::read(sandbox.mail_root.join(room).join("cursors.json")).expect("read cursor state"),
    )
    .expect("cursor state is JSON")
}

fn seen_contains(set: &serde_json::Value, id: &str) -> bool {
    set["seen"]
        .as_array()
        .is_some_and(|ids| ids.iter().any(|entry| entry.as_str() == Some(id)))
}

#[test]
fn read_moves_mail_then_records_mail_seen() {
    let sandbox = Sandbox::new();
    let sent = sandbox.send_json("consuming-read", "body");
    let inbox = sandbox
        .mail_root
        .join("claude-space/inbox")
        .join(format!("{}.mail", sent.envelope.id));
    let read = sandbox
        .mail_root
        .join("claude-space/read")
        .join(format!("{}.mail", sent.envelope.id));

    let output = sandbox.run(&[
        "read",
        &sent.envelope.id,
        "--room",
        "claude-space",
        "--json",
    ]);
    assert_success(&output);
    let rendered: ReadOutput = from_stdout(&output);
    assert_eq!(rendered.envelope.id, sent.envelope.id);
    assert!(!rendered.already_read);
    assert!(!inbox.exists(), "successful read removes the inbox link");
    assert!(read.exists(), "successful read commits the read link");

    let state = cursor(&sandbox, "claude-space");
    assert!(seen_contains(&state["mail"], &sent.envelope.id));
}

#[test]
fn read_link_failure_leaves_mail_unmarked_and_inbox_intact() {
    let sandbox = Sandbox::new();
    let sent = sandbox.send_json("link-failure", "body");
    let inbox = sandbox
        .mail_root
        .join("claude-space/inbox")
        .join(format!("{}.mail", sent.envelope.id));
    let read = sandbox
        .mail_root
        .join("claude-space/read")
        .join(format!("{}.mail", sent.envelope.id));
    fs::write(&read, b"pre-existing destination").expect("plant destination collision");

    let output = sandbox.run(&[
        "read",
        &sent.envelope.id,
        "--room",
        "claude-space",
        "--json",
    ]);
    assert_eq!(
        output.status.code(),
        Some(75),
        "stderr: {}",
        common::stderr(&output)
    );
    let error: ErrorEnvelope = from_stderr(&output);
    assert_eq!(error.error.code, "io_error");
    assert!(inbox.exists(), "link failure must leave the inbox copy");
    assert_eq!(
        fs::read(&read).expect("read destination"),
        b"pre-existing destination"
    );
    let state_path = sandbox.mail_root.join("claude-space/cursors.json");
    if state_path.exists() {
        let state = cursor(&sandbox, "claude-space");
        assert!(!seen_contains(&state["mail"], &sent.envelope.id));
    }
}

#[cfg(unix)]
#[test]
fn unlink_failure_marks_seen_and_retry_uses_the_committed_read_copy() {
    let sandbox = Sandbox::new();
    let sent = sandbox.send_json("unlink-failure", "body");
    let room = sandbox.mail_root.join("claude-space");
    let inbox_dir = room.join("inbox");
    let inbox = inbox_dir.join(format!("{}.mail", sent.envelope.id));
    let read = room.join("read").join(format!("{}.mail", sent.envelope.id));
    fs::set_permissions(&inbox_dir, fs::Permissions::from_mode(0o500))
        .expect("make inbox directory non-writable");
    let output = sandbox.run(&[
        "read",
        &sent.envelope.id,
        "--room",
        "claude-space",
        "--json",
    ]);
    fs::set_permissions(&inbox_dir, fs::Permissions::from_mode(0o700))
        .expect("restore inbox directory permissions");

    assert_eq!(
        output.status.code(),
        Some(70),
        "stderr: {}",
        common::stderr(&output)
    );
    let error: ErrorEnvelope = from_stderr(&output);
    assert_eq!(error.error.code, "delivered_output_failure");
    assert!(
        inbox.exists(),
        "unlink failure leaves the physical inbox link"
    );
    assert!(read.exists(), "read link committed before unlink failure");
    let state = cursor(&sandbox, "claude-space");
    assert!(seen_contains(&state["mail"], &sent.envelope.id));

    let retry = sandbox.run(&[
        "read",
        &sent.envelope.id,
        "--room",
        "claude-space",
        "--json",
    ]);
    assert_success(&retry);
    let rendered: ReadOutput = from_stdout(&retry);
    assert!(
        rendered.already_read,
        "cursor-marked duplicate must not retry the move"
    );
}

#[cfg(unix)]
#[test]
fn cursor_write_failure_after_move_keeps_physical_state_authoritative() {
    let sandbox = Sandbox::new();
    let first = sandbox.send_json("cursor-seed", "seed");
    assert_success(&sandbox.run(&[
        "read",
        &first.envelope.id,
        "--room",
        "claude-space",
        "--json",
    ]));
    let sent = sandbox.send_json("cursor-write-failure", "body");
    let room = sandbox.mail_root.join("claude-space");
    let inbox = room
        .join("inbox")
        .join(format!("{}.mail", sent.envelope.id));
    let read = room.join("read").join(format!("{}.mail", sent.envelope.id));
    fs::set_permissions(&room, fs::Permissions::from_mode(0o500))
        .expect("make room directory non-writable");
    let output = sandbox.run(&[
        "read",
        &sent.envelope.id,
        "--room",
        "claude-space",
        "--json",
    ]);
    fs::set_permissions(&room, fs::Permissions::from_mode(0o700))
        .expect("restore room directory permissions");

    assert_eq!(
        output.status.code(),
        Some(75),
        "stderr: {}",
        common::stderr(&output)
    );
    let error: ErrorEnvelope = from_stderr(&output);
    assert_eq!(error.error.code, "io_error");
    let rendered: ReadOutput = from_stdout(&output);
    assert_eq!(rendered.envelope.id, sent.envelope.id);
    assert!(
        !inbox.exists(),
        "the physical move committed before cursor failure"
    );
    assert!(
        read.exists(),
        "the read link is authoritative after cursor failure"
    );
    let state = cursor(&sandbox, "claude-space");
    assert!(!seen_contains(&state["mail"], &sent.envelope.id));

    let retry = sandbox.run(&[
        "read",
        &sent.envelope.id,
        "--room",
        "claude-space",
        "--json",
    ]);
    assert_success(&retry);
    let replay: ReadOutput = from_stdout(&retry);
    assert!(
        replay.already_read,
        "a moved message must not be consumed twice"
    );
}

#[test]
fn chat_late_arrival_below_seen_ids_stays_unread() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    join_channel(&sandbox, "tax", &alpha);
    join_channel(&sandbox, "tax", &beta);
    assert_success(&sandbox.run_in(&["chat", "tax", "--discard", "--json"], None, &beta));
    let sent = sandbox.run_in(
        &[
            "chat", "tax", "--send", "--anyway", "--body", "first", "--json",
        ],
        None,
        &alpha,
    );
    assert_success(&sent);
    let sent: post::output::ChatSendOutput = from_stdout(&sent);
    let consumed: ChatReadOutput =
        from_stdout(&sandbox.run_in(&["chat", "tax", "--json"], None, &beta));
    assert_eq!(consumed.count, 1);

    let late_id = "20000101-000000-000001-aaaaaa";
    write_channel_message(&sandbox, "tax", late_id, "alpha", "", "late");
    let peek: ChatReadOutput =
        from_stdout(&sandbox.run_in(&["chat", "tax", "--peek", "--json"], None, &beta));
    assert_eq!(peek.count, 1);
    assert_eq!(peek.messages[0].message.id, late_id);
    assert_ne!(peek.messages[0].message.id, sent.message.id);
}

#[test]
fn discard_through_and_seen_by_use_the_unified_cursor() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    join_channel(&sandbox, "tax", &alpha);
    join_channel(&sandbox, "tax", &beta);
    let sent: post::output::ChatSendOutput = from_stdout(&sandbox.run_in(
        &[
            "chat", "tax", "--send", "--anyway", "--body", "target", "--json",
        ],
        None,
        &alpha,
    ));

    let discarded: ChatDiscardThroughOutput = from_stdout(&sandbox.run_in(
        &[
            "chat",
            "tax",
            "--discard-through",
            &sent.message.id,
            "--json",
        ],
        None,
        &beta,
    ));
    assert!(discarded.advanced);
    assert_eq!(discarded.target, sent.message.id);

    let seen: SeenByOutput = from_stdout(&sandbox.run_in(
        &["chat", "tax", "--seen-by", &sent.message.id, "--json"],
        None,
        &beta,
    ));
    assert_eq!(seen.seen_by, vec!["alpha".to_owned(), "beta".to_owned()]);
    let state = cursor(&sandbox, "beta");
    assert!(state["channels"]["tax"]["seen"]
        .as_array()
        .is_some_and(|ids| ids
            .iter()
            .any(|entry| { entry.as_str() == Some(sent.message.id.as_str()) })));
}
