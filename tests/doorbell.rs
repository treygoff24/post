mod common;

use common::{
    assert_success, from_stdout, join_channel, register_alpha_beta, write_bad_channel,
    write_channel_message, Sandbox,
};
use post::output::{
    CatchupOutput, CatchupTarget, ChannelsOutput, ChatReadOutput, ChatSendOutput, InboxOutput,
    SendOutput, WatchEvent,
};
use std::fs;
use std::io::{BufRead, BufReader};
use std::process::{Child, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const WATCH_INTERVAL_MS: &str = "100";

/// A live watch with a line reader so a test can synchronize on the startup
/// floor or on a particular ring without sleeping for an arbitrary interval.
struct LiveWatch {
    child: Option<Child>,
    lines: Receiver<String>,
    reader: Option<JoinHandle<()>>,
}

impl LiveWatch {
    fn start(sandbox: &Sandbox, room: &str, extra_args: &[&str]) -> Self {
        let mut command = common::post_command();
        command
            .args(["watch", "--room", room, "--interval-ms", WATCH_INTERVAL_MS])
            .args(extra_args)
            .current_dir(&sandbox.path)
            .env("HOME", &sandbox.home)
            .env("POST_MAIL_ROOT", &sandbox.mail_root)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .stdin(Stdio::null());
        let mut child = command.spawn().expect("spawn live watch");
        let stdout = child.stdout.take().expect("watch stdout pipe");
        let (sender, lines) = mpsc::channel();
        let reader = thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                let Ok(line) = line else { break };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            child: Some(child),
            lines,
            reader: Some(reader),
        }
    }

    fn next_event(&self, timeout: Duration) -> Option<WatchEvent> {
        let line = self.lines.recv_timeout(timeout).ok()?;
        Some(serde_json::from_str(&line).unwrap_or_else(|error| {
            panic!("watch line was not a WatchEvent: {error}\nline: {line}")
        }))
    }

    fn assert_alive(&mut self) {
        if let Some(status) = self
            .child
            .as_mut()
            .expect("live watch child")
            .try_wait()
            .expect("probe live watch")
        {
            panic!("live watch exited with {status}");
        }
    }
}

impl Drop for LiveWatch {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn wait_for_heartbeat(sandbox: &Sandbox, room: &str, watch: &mut LiveWatch) {
    let heartbeat = sandbox.mail_root.join(room).join("watch.heartbeat");
    let deadline = Instant::now() + Duration::from_secs(2);
    while !heartbeat.is_file() {
        watch.assert_alive();
        assert!(
            Instant::now() < deadline,
            "watch never created heartbeat at {}",
            heartbeat.display()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn join_and_clear(sandbox: &Sandbox, channel: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let (alpha, beta) = register_alpha_beta(sandbox);
    join_channel(sandbox, channel, &alpha);
    join_channel(sandbox, channel, &beta);
    assert_success(&sandbox.run_in(&["chat", channel, "--discard", "--json"], None, &beta));
    (alpha, beta)
}

fn send_channel(
    sandbox: &Sandbox,
    channel: &str,
    sender: &std::path::Path,
    body: &str,
) -> ChatSendOutput {
    let output = sandbox.run_in(
        &[
            "chat", channel, "--send", "--anyway", "--body", body, "--json",
        ],
        None,
        sender,
    );
    assert_success(&output);
    from_stdout(&output)
}

fn catchup_channel(sandbox: &Sandbox, channel: &str, room: &std::path::Path) -> CatchupOutput {
    let output = sandbox.run_in(&["catchup", channel, "--json"], None, room);
    assert_success(&output);
    from_stdout(&output)
}

fn assert_channel_event(event: WatchEvent, expected_id: &str, expected_channel: &str) {
    match event {
        WatchEvent::ChannelMessage {
            id,
            channel,
            preview,
            ..
        } => {
            assert_eq!(id, expected_id);
            assert_eq!(channel, expected_channel);
            assert!(preview.is_some(), "channel ring should carry its preview");
        }
        other => panic!("expected channel ring for {expected_id}, got {other:?}"),
    }
}

#[test]
fn armed_watch_rings_new_message_after_catchup_consumes_previous_message() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = join_and_clear(&sandbox, "tax");
    let first = send_channel(&sandbox, "tax", &alpha, "first backlog");

    let mut watch = LiveWatch::start(&sandbox, "beta", &[]);
    wait_for_heartbeat(&sandbox, "beta", &mut watch);
    // The watch was armed before catchup, so its startup scan must ring the
    // first message before the consuming command moves it to seen.
    assert_channel_event(
        watch
            .next_event(Duration::from_secs(2))
            .expect("watch startup ring for first message"),
        &first.message.id,
        "tax",
    );

    let consumed = catchup_channel(&sandbox, "tax", &beta);
    assert_eq!(consumed.count, 1);
    assert!(matches!(
        &consumed.targets[..],
        [CatchupTarget::Channel { messages, count: 1, .. }]
            if messages[0].message.id == first.message.id
    ));

    let second = send_channel(&sandbox, "tax", &alpha, "second after catchup");
    assert_channel_event(
        watch
            .next_event(Duration::from_secs(2))
            .expect("watch ring after catchup"),
        &second.message.id,
        "tax",
    );
}

#[test]
fn watch_ring_never_advances_cursor_and_later_catchup_still_returns_message() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = join_and_clear(&sandbox, "tax");
    let cursor_path = sandbox.mail_root.join("beta/cursors.json");
    let before = fs::read(&cursor_path).expect("baseline cursor from channel discard");

    let mut watch = LiveWatch::start(&sandbox, "beta", &[]);
    wait_for_heartbeat(&sandbox, "beta", &mut watch);
    let sent = send_channel(&sandbox, "tax", &alpha, "ring but do not consume");
    assert_channel_event(
        watch
            .next_event(Duration::from_secs(2))
            .expect("watch ring for unconsumed message"),
        &sent.message.id,
        "tax",
    );
    drop(watch);

    assert_eq!(
        fs::read(&cursor_path).expect("cursor remains after watch"),
        before,
        "a notification must not advance the channel cursor"
    );
    let consumed = catchup_channel(&sandbox, "tax", &beta);
    assert_eq!(consumed.count, 1);
    assert!(matches!(
        &consumed.targets[..],
        [CatchupTarget::Channel { messages, count: 1, .. }]
            if messages[0].message.id == sent.message.id
    ));
}

#[test]
fn watch_started_after_catchup_uses_unified_floor_without_replaying_backlog() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = join_and_clear(&sandbox, "tax");
    let first = send_channel(&sandbox, "tax", &alpha, "already caught up");
    let consumed = catchup_channel(&sandbox, "tax", &beta);
    assert_eq!(consumed.count, 1);

    let mut watch = LiveWatch::start(&sandbox, "beta", &[]);
    wait_for_heartbeat(&sandbox, "beta", &mut watch);
    assert!(
        watch.next_event(Duration::from_millis(300)).is_none(),
        "a watch started after catchup replayed {}",
        first.message.id
    );

    let second = send_channel(&sandbox, "tax", &alpha, "new after watch startup");
    assert_channel_event(
        watch
            .next_event(Duration::from_secs(2))
            .expect("watch ring for post-start message"),
        &second.message.id,
        "tax",
    );
}

#[test]
fn old_store_without_state_is_read_only_until_first_catchup() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    write_bad_channel(
        &sandbox,
        "tax",
        Some(r#"{"alpha":"2026-08-20 12:00:00 -0500","beta":"2026-08-20 12:00:00 -0500"}"#),
        true,
        r#"{"name":"tax","created":"2026-08-20 12:00:00 -0500","created_by":"alpha"}"#,
    );
    let first_id = "20260820-120000-000001-aaaaaa";
    let second_id = "20260820-120000-000002-bbbbbb";
    write_channel_message(&sandbox, "tax", first_id, "alpha", "", "first");
    write_channel_message(&sandbox, "tax", second_id, "alpha", "", "second");
    let mail = sandbox.run_in(
        &[
            "send",
            "--to",
            "beta",
            "--body",
            "mail in old store",
            "--json",
        ],
        None,
        &alpha,
    );
    assert_success(&mail);
    let mail: SendOutput = from_stdout(&mail);

    let mut before_reads = fs::read_dir(&sandbox.mail_root)
        .expect("old-store root")
        .map(|entry| entry.expect("old-store entry").file_name())
        .collect::<Vec<_>>();
    before_reads.sort();

    let channels = sandbox.run_in(&["channels"], None, &beta);
    assert_success(&channels);
    let channels: ChannelsOutput = from_stdout(&channels);
    let tax = channels
        .channels
        .iter()
        .find(|channel| channel.name == "tax")
        .expect("old-store channel is listed");
    assert_eq!(
        tax.unread,
        Some(2),
        "a cursorless old store starts all-unread"
    );
    let inbox = sandbox.run_in(&["inbox", "--room", "beta"], None, &beta);
    assert_success(&inbox);
    let inbox: InboxOutput = from_stdout(&inbox);
    assert_eq!(inbox.count, 1);
    assert_eq!(inbox.unread_count, 1);
    let chat = sandbox.run_in(&["chat", "tax", "--peek", "--json"], None, &beta);
    assert_success(&chat);
    let chat: ChatReadOutput = from_stdout(&chat);
    assert_eq!(chat.count, 2, "cursorless old channel starts all-unread");
    let snapshot = sandbox.run(&["watch", "--room", "beta", "--snapshot"]);
    assert_success(&snapshot);
    let events: Vec<WatchEvent> = String::from_utf8_lossy(&snapshot.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).expect("snapshot event"))
        .collect();
    assert!(events
        .iter()
        .any(|event| { matches!(event, WatchEvent::ChannelMessage { id, .. } if id == first_id) }));
    assert!(events.iter().any(|event| {
        matches!(event, WatchEvent::Mail { item, .. } if item.id == mail.envelope.id)
    }));
    assert!(!sandbox.mail_root.join("beta/cursors.json").exists());
    assert!(!sandbox.mail_root.join("beta/.cursors.lock").exists());
    assert!(!sandbox.mail_root.join("beta/channel-state.json").exists());
    let mut after_reads = fs::read_dir(&sandbox.mail_root)
        .expect("old-store root after reads")
        .map(|entry| entry.expect("old-store entry").file_name())
        .collect::<Vec<_>>();
    after_reads.sort();
    assert_eq!(
        before_reads, after_reads,
        "read-only old-store surfaces mutated the root"
    );

    let caught = catchup_channel(&sandbox, "tax", &beta);
    assert_eq!(caught.count, 2);
    assert!(sandbox.mail_root.join("beta/cursors.json").is_file());
    assert!(!sandbox.mail_root.join("beta/channel-state.json").exists());
    let empty = catchup_channel(&sandbox, "tax", &beta);
    assert_eq!(empty.count, 0);
}

#[test]
fn valid_old_channel_state_is_read_only_then_materializes_on_first_catchup() {
    let sandbox = Sandbox::new();
    let (_alpha, beta) = register_alpha_beta(&sandbox);
    write_bad_channel(
        &sandbox,
        "tax",
        Some(r#"{"alpha":"2026-08-20 12:00:00 -0500","beta":"2026-08-20 12:00:00 -0500"}"#),
        true,
        r#"{"name":"tax","created":"2026-08-20 12:00:00 -0500","created_by":"alpha"}"#,
    );
    let first_id = "20260820-120000-000001-aaaaaa";
    let second_id = "20260820-120000-000002-bbbbbb";
    write_channel_message(&sandbox, "tax", first_id, "alpha", "", "seen baseline");
    write_channel_message(&sandbox, "tax", second_id, "alpha", "", "unseen tail");
    fs::create_dir_all(sandbox.mail_root.join("beta")).expect("create legacy room");
    let legacy = sandbox.mail_root.join("beta/channel-state.json");
    let legacy_bytes = serde_json::json!({
        "version": 2,
        "channels": {"tax": {"seen": [first_id]}}
    })
    .to_string()
    .into_bytes();
    fs::write(&legacy, &legacy_bytes).expect("write valid v0.8 channel state");

    let mut before_reads = fs::read_dir(&sandbox.mail_root)
        .expect("legacy root")
        .map(|entry| entry.expect("legacy entry").file_name())
        .collect::<Vec<_>>();
    before_reads.sort();

    let channels_before = sandbox.run_in(&["channels"], None, &beta);
    assert_success(&channels_before);
    let channels_before: ChannelsOutput = from_stdout(&channels_before);
    let tax = channels_before
        .channels
        .iter()
        .find(|channel| channel.name == "tax")
        .expect("legacy channel is listed");
    assert_eq!(tax.unread, Some(1), "legacy seen baseline is preserved");
    let chat = sandbox.run_in(&["chat", "tax", "--peek", "--json"], None, &beta);
    assert_success(&chat);
    let chat: ChatReadOutput = from_stdout(&chat);
    assert_eq!(
        chat.count, 1,
        "legacy seen baseline leaves only the tail unread"
    );
    let snapshot = sandbox.run(&["watch", "--room", "beta", "--snapshot"]);
    assert_success(&snapshot);
    let events: Vec<WatchEvent> = String::from_utf8_lossy(&snapshot.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).expect("snapshot event"))
        .collect();
    assert!(!events
        .iter()
        .any(|event| { matches!(event, WatchEvent::ChannelMessage { id, .. } if id == first_id) }));
    assert!(events.iter().any(|event| {
        matches!(event, WatchEvent::ChannelMessage { id, .. } if id == second_id)
    }));
    assert_eq!(
        fs::read(&legacy).expect("legacy state unchanged"),
        legacy_bytes
    );
    assert!(!sandbox.mail_root.join("beta/cursors.json").exists());
    assert!(!sandbox.mail_root.join("beta/.cursors.lock").exists());
    let mut after_reads = fs::read_dir(&sandbox.mail_root)
        .expect("legacy root after reads")
        .map(|entry| entry.expect("legacy entry").file_name())
        .collect::<Vec<_>>();
    after_reads.sort();
    assert_eq!(
        before_reads, after_reads,
        "legacy read-only surfaces mutated the root"
    );

    let caught = catchup_channel(&sandbox, "tax", &beta);
    assert_eq!(caught.count, 1);
    assert!(matches!(
        &caught.targets[..],
        [CatchupTarget::Channel { messages, count: 1, .. }]
            if messages[0].message.id == second_id
    ));
    assert!(sandbox.mail_root.join("beta/cursors.json").is_file());
    assert_eq!(
        fs::read(&legacy).expect("legacy rollback evidence"),
        legacy_bytes
    );
}
