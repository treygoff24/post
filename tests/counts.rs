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

    // Read one message
    let _ = sandbox.run(&["read", &sent1.envelope.id, "--room", "claude-space"]);

    // Check inbox again - unread_count should be reduced
    let output = sandbox.run(&["inbox", "--room", "claude-space", "--json"]);
    assert_success(&output);
    let inbox: InboxOutput = from_stdout(&output);
    assert_eq!(inbox.count, 2); // File count reduced
    assert_eq!(inbox.unread_count, 1); // 2 files - 1 seen = 1 unread
}
