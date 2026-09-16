mod common;

use common::{
    assert_success, from_stdout, join_channel, register_alpha_beta, write_custom_mail, Sandbox,
};
use serde_json::Value;
use std::path::Path;
use std::process::{Child, Stdio};

fn start_watch(sandbox: &Sandbox, participant: &str, cwd: &Path) -> std::process::Child {
    let mut child = common::post_command()
        .args(["watch", "--interval-ms", "100"])
        .current_dir(cwd)
        .env("HOME", &sandbox.home)
        .env("POST_MAIL_ROOT", &sandbox.mail_root)
        .env("POST_PARTICIPANT", participant)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .spawn()
        .expect("spawn live watch");
    let heartbeat = sandbox
        .mail_root
        .join("participants")
        .join(participant)
        .join("watch.heartbeat");
    for _ in 0..100 {
        if heartbeat.is_file() {
            return child;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    child.kill().expect("stop heartbeat-less watch");
    let output = child
        .wait_with_output()
        .expect("collect heartbeat-less watch");
    panic!(
        "watch heartbeat never appeared: stdout={} stderr={}",
        common::stdout(&output),
        common::stderr(&output)
    )
}

fn stop_watch(mut child: Child) -> std::process::Output {
    std::thread::sleep(std::time::Duration::from_millis(300));
    child.kill().expect("stop live watch");
    child.wait_with_output().expect("collect live watch")
}

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

#[test]
fn restarted_live_watch_rings_again_without_advancing_participant_seen_state() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    join_channel(&sandbox, "restart", &alpha);
    join_channel(&sandbox, "restart", &beta);
    let sent = sandbox.run_in(
        &[
            "chat",
            "restart",
            "--send",
            "--anyway",
            "--body",
            "restart ring",
            "--json",
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

    for attempt in 0..2 {
        let watched = sandbox.run_as_participant(
            &["watch", "--once", "--interval-ms", "100"],
            &participant,
            &beta,
        );
        assert_success(&watched);
        assert!(
            common::stdout(&watched).contains(id),
            "restart attempt {attempt} did not ring"
        );
        assert_eq!(std::fs::read(&cursor).ok(), before);
    }
}

#[test]
fn live_participant_watch_routes_bridge_arrival_and_emits_without_consuming() {
    let sandbox = Sandbox::new();
    let (_alpha, beta) = register_alpha_beta(&sandbox);
    let recipient = sandbox.test_participant("beta");
    let sender = sandbox.test_participant("alpha");
    let mut child = common::post_command()
        .args(["watch", "--once", "--interval-ms", "100"])
        .current_dir(&beta)
        .env("HOME", &sandbox.home)
        .env("POST_MAIL_ROOT", &sandbox.mail_root)
        .env("POST_PARTICIPANT", &recipient)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .spawn()
        .expect("spawn live watch");
    let heartbeat = sandbox
        .mail_root
        .join("participants")
        .join(&recipient)
        .join("watch.heartbeat");
    for _ in 0..300 {
        if heartbeat.is_file() {
            break;
        }
        if child.try_wait().expect("poll live watch").is_some() {
            let output = child.wait_with_output().expect("collect failed watch");
            panic!(
                "watch exited before heartbeat\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    if !heartbeat.is_file() {
        child.kill().expect("stop heartbeat-less watch");
        let output = child
            .wait_with_output()
            .expect("collect heartbeat-less watch");
        panic!(
            "watch did not publish heartbeat\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let id = "20990916-050000-beef01";
    write_custom_mail(
        &sandbox.mail_root.join("beta/inbox"),
        id,
        &serde_json::json!({"id":id,"from":"alpha","to":"beta","kind":"note","subject":"bridge","sent":"2026-09-16 05:00:00 -0500","from_participant":sender,"address_kind":"workspace"}),
        "bridge arrival",
    );
    let output = child.wait_with_output().expect("watch exits after event");
    assert_success(&output);
    let event: Value = serde_json::from_slice(&output.stdout).expect("watch event");
    assert_eq!(event["id"], id);
    assert!(event.get("pending").is_none());
    assert!(sandbox
        .mail_root
        .join(format!("beta/routing/{id}.json"))
        .is_file());
    assert!(sandbox
        .mail_root
        .join(format!("beta/inbox/{id}.mail"))
        .is_file());
    assert!(!sandbox
        .mail_root
        .join("participants")
        .join(recipient)
        .join("cursors.json")
        .exists());
}

#[test]
fn armed_watch_rings_again_after_catchup_consumes_the_previous_message() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    join_channel(&sandbox, "armed", &alpha);
    join_channel(&sandbox, "armed", &beta);
    let recipient = sandbox.test_participant("beta");
    let child = start_watch(&sandbox, &recipient, &beta);

    let first = sandbox.run_in(
        &[
            "chat",
            "armed",
            "--send",
            "--anyway",
            "--body",
            "first ring",
            "--json",
        ],
        None,
        &alpha,
    );
    assert_success(&first);
    let first: Value = from_stdout(&first);
    let first_id = first["message"]["id"].as_str().expect("first id");
    std::thread::sleep(std::time::Duration::from_millis(250));
    assert_success(&sandbox.run_as_participant(&["catchup", "armed", "--json"], &recipient, &beta));
    let second = sandbox.run_in(
        &[
            "chat",
            "armed",
            "--send",
            "--anyway",
            "--body",
            "second ring",
            "--json",
        ],
        None,
        &alpha,
    );
    assert_success(&second);
    let second: Value = from_stdout(&second);
    let second_id = second["message"]["id"].as_str().expect("second id");

    let output = stop_watch(child);
    assert!(output.status.success() || output.status.code().is_none());
    let text = common::stdout(&output);
    assert!(text.contains(first_id), "first ring missing: {text}");
    assert!(text.contains(second_id), "second ring missing: {text}");
}

#[test]
fn watch_started_after_catchup_uses_unified_floor_without_replaying_backlog() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    join_channel(&sandbox, "floor", &alpha);
    join_channel(&sandbox, "floor", &beta);
    let recipient = sandbox.test_participant("beta");
    let backlog = sandbox.run_in(
        &[
            "chat", "floor", "--send", "--anyway", "--body", "backlog", "--json",
        ],
        None,
        &alpha,
    );
    assert_success(&backlog);
    let backlog: Value = from_stdout(&backlog);
    let backlog_id = backlog["message"]["id"].as_str().expect("backlog id");
    assert_success(&sandbox.run_as_participant(&["catchup", "floor", "--json"], &recipient, &beta));

    let child = start_watch(&sandbox, &recipient, &beta);
    let live = sandbox.run_in(
        &[
            "chat", "floor", "--send", "--anyway", "--body", "live", "--json",
        ],
        None,
        &alpha,
    );
    assert_success(&live);
    let live: Value = from_stdout(&live);
    let live_id = live["message"]["id"].as_str().expect("live id");
    let output = stop_watch(child);
    let text = common::stdout(&output);
    assert!(
        !text.contains(backlog_id),
        "consumed backlog replayed: {text}"
    );
    assert!(text.contains(live_id), "live ring missing: {text}");
}
