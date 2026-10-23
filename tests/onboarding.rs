mod common;

use common::{assert_success, from_stdout, Sandbox};
use serde_json::Value;
use std::fs;
use std::process::{Command, Output};

// No fixture rooms, identity variables, hooks, owner, relay, or user config:
// this exercises the README's first-run path with the real default store.
fn run(sandbox: &Sandbox, room: &str, participant: Option<&str>, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_post"));
    command
        .env_clear()
        .env("HOME", &sandbox.home)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .current_dir(sandbox.home.join(room))
        .args(args);
    if let Some(id) = participant {
        command.env("POST_PARTICIPANT", id);
    }
    command.output().expect("run standalone post")
}

#[test]
fn clean_home_setup_and_two_shell_agents_exchange_mail_without_a_harness() {
    let sandbox = Sandbox::new_unseeded();
    for room in ["alice", "bob"] {
        fs::create_dir_all(sandbox.home.join(room)).unwrap();
    }
    let root = sandbox.home.join(".claude-mail");
    assert!(!root.exists(), "the setup precondition is a clean home");
    let fixed = run(&sandbox, "alice", None, &["doctor", "--fix", "--json"]);
    assert_success(&fixed);
    assert!(
        root.join("rooms.json").is_file(),
        "uses the compatible default root"
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(root.join("rooms.json")).unwrap()).unwrap(),
        serde_json::json!({}),
        "first run must not seed someone else's workspace map"
    );
    let mut actors = Vec::new();
    for room in ["alice", "bob"] {
        let directory = sandbox.home.join(room);
        assert_success(&run(
            &sandbox,
            room,
            None,
            &["rooms", "add", room, directory.to_str().unwrap()],
        ));
        let bound = run(&sandbox, room, None, &["participant", "bind", "--new"]);
        assert_success(&bound);
        let exported = common::stdout(&bound);
        let id = exported
            .trim()
            .strip_prefix("export POST_PARTICIPANT=")
            .expect("shell bootstrap export");
        actors.push(id.to_owned());
    }
    assert_ne!(actors[0], actors[1], "two terminals get separate actors");
    let sent = run(
        &sandbox,
        "alice",
        Some(&actors[0]),
        &["send", "--to", "bob", "--body", "Build is green.", "--json"],
    );
    assert_success(&sent);
    let sent: Value = from_stdout(&sent);
    let id = sent["envelope"]["id"].as_str().unwrap();
    let inbox = run(&sandbox, "bob", Some(&actors[1]), &["inbox", "--json"]);
    assert_success(&inbox);
    assert!(
        common::stdout(&inbox).contains(id),
        "recipient sees the sent id"
    );
    let read = run(&sandbox, "bob", Some(&actors[1]), &["read", id, "--json"]);
    assert_success(&read);
    assert!(common::stdout(&read).contains("Build is green."));
    let reply_to = format!("participant:{}", actors[0]);
    assert_success(&run(
        &sandbox,
        "bob",
        Some(&actors[1]),
        &["send", "--to", &reply_to, "--body", "Thanks, rebasing now."],
    ));
    let reply = run(&sandbox, "alice", Some(&actors[0]), &["inbox", "--json"]);
    assert_success(&reply);
    assert!(common::stdout(&reply).contains("bob"));
    assert_success(&run(&sandbox, "alice", None, &["doctor", "--brief"]));
    assert!(!root.join("owner.json").exists(), "signatures are optional");
    assert!(
        !root.join("bridge").exists(),
        "cross-host relay is optional"
    );
}
