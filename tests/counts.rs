use post::output::InboxOutput;
mod common;
use common::*;

#[test]
fn basic_inbox_unread_count() {
    let sandbox = Sandbox::new();

    // Send some mail to the default room
    let sent1 = sandbox.send_json("test-sender", "test body 1");
    let _sent2 = sandbox.send_json("test-sender", "test body 2");
    let _sent3 = sandbox.send_json("test-sender", "test body 3");

    // Check inbox - should have unread_count matching file count initially
    let output = sandbox.run(&["inbox", "--room", "claude-space", "--json"]);
    assert_success(&output);
    let inbox: InboxOutput = from_stdout(&output);
    assert_eq!(inbox.count, 3);
    assert_eq!(inbox.unread_count, 3); // Should match file count initially

    // Read one message: its file leaves the inbox and its id enters
    // mail.seen. The two remaining inbox ids are still absent from seen, so
    // count and unread_count agree — the contract's healthy-store promise.
    // (The pre-fix formula count - |seen| reported 1 here: a seen id whose
    // file already left the inbox deflated the number.)
    let _ = sandbox.run(&["read", &sent1.envelope.id, "--room", "claude-space"]);

    let output = sandbox.run(&["inbox", "--room", "claude-space", "--json"]);
    assert_success(&output);
    let inbox: InboxOutput = from_stdout(&output);
    assert_eq!(inbox.count, 2); // File count reduced
    assert_eq!(inbox.unread_count, 2); // Neither remaining id is in mail.seen

    // The contract's divergence case: a failed unlink leaves the consumed
    // mail's file duplicated back in the inbox. Its id IS in mail.seen, so
    // count exposes the physical file while unread_count excludes it —
    // proving unread_count is the per-id predicate, not the raw file count.
    let room_dir = sandbox.mail_root.join("claude-space");
    let read_copy = room_dir
        .join("read")
        .join(format!("{}.mail", sent1.envelope.id));
    let inbox_dup = room_dir
        .join("inbox")
        .join(format!("{}.mail", sent1.envelope.id));
    std::fs::copy(&read_copy, &inbox_dup).expect("plant failed-unlink duplicate");

    let output = sandbox.run(&["inbox", "--room", "claude-space", "--json"]);
    assert_success(&output);
    let inbox: InboxOutput = from_stdout(&output);
    assert_eq!(inbox.count, 3); // Physical files, duplicate included
    assert_eq!(inbox.unread_count, 2); // The seen duplicate is excluded
}
