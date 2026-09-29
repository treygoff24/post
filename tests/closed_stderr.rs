//! A closed stderr never panics post. std's `eprintln!` panics on a failed
//! write, so a caller that closed the pipe early (`post ... 2>&1 | head -1`)
//! saw exit 101 and, where the notice came before the effect, no effect at
//! all. Each test runs text mode (the pre-effect banners are skipped under
//! `--json`), first with an open stderr to prove the banner is written, then
//! with a stderr pipe whose reader is already closed, and checks the exit code
//! and that the effect still landed.

mod common;

use common::{assert_success, post_command, register_alpha_beta, Sandbox};
use std::fs;
use std::path::Path;
use std::process::{Output, Stdio};

fn run_with_stderr(
    sandbox: &Sandbox,
    participant: &str,
    cwd: &Path,
    args: &[&str],
    closed: bool,
) -> Output {
    let stderr = if closed {
        let (reader, writer) = std::io::pipe().expect("stderr pipe");
        drop(reader);
        Stdio::from(writer)
    } else {
        Stdio::piped()
    };
    post_command()
        .args(args)
        .current_dir(cwd)
        .env("HOME", &sandbox.home)
        .env("POST_MAIL_ROOT", &sandbox.mail_root)
        .env("POST_PARTICIPANT", participant)
        .env_remove("POST_FROM")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(stderr)
        .output()
        .expect("run post")
}

fn count(dir: &Path) -> usize {
    fs::read_dir(dir).map_or(0, |entries| entries.count())
}

#[test]
fn chat_send_with_a_closed_stderr_sends_and_exits_zero() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let participant = sandbox.bind_claude("closed-stderr-chat", &alpha, Some("alpha"))["id"]
        .as_str()
        .expect("participant id")
        .to_owned();
    assert_success(&sandbox.run_as_participant(
        &["chat", "tax", "--join", "--json"],
        &participant,
        &alpha,
    ));
    let messages = sandbox.mail_root.join("channels/tax/messages");
    let before = count(&messages);
    // Control: with stderr open, text mode writes the banner before sending.
    let open = run_with_stderr(
        &sandbox,
        &participant,
        &alpha,
        &["chat", "tax", "--send", "--body", "open stderr"],
        false,
    );
    assert_success(&open);
    assert!(
        String::from_utf8_lossy(&open.stderr).contains("post: sending to #tax as room"),
        "the banner precedes the send: {}",
        String::from_utf8_lossy(&open.stderr)
    );
    assert_eq!(count(&messages), before + 1, "the control sent");
    let output = run_with_stderr(
        &sandbox,
        &participant,
        &alpha,
        &["chat", "tax", "--send", "--body", "closed stderr"],
        true,
    );
    assert_ne!(output.status.code(), Some(101), "post panicked");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(count(&messages), before + 2, "the message was sent");
}

#[test]
fn send_with_a_closed_stderr_delivers_and_exits_zero() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let participant = sandbox.bind_claude("closed-stderr-send", &alpha, Some("alpha"))["id"]
        .as_str()
        .expect("participant id")
        .to_owned();
    let inbox = sandbox.mail_root.join("beta/inbox");
    let before = count(&inbox);
    // Control: with stderr open, text mode writes the banner before delivering.
    let open = run_with_stderr(
        &sandbox,
        &participant,
        &alpha,
        &["send", "--to", "beta", "--body", "open stderr"],
        false,
    );
    assert_success(&open);
    assert!(
        String::from_utf8_lossy(&open.stderr).contains("post: sending as 'alpha'"),
        "the banner precedes delivery: {}",
        String::from_utf8_lossy(&open.stderr)
    );
    assert_eq!(count(&inbox), before + 1, "the control delivered");
    let output = run_with_stderr(
        &sandbox,
        &participant,
        &alpha,
        &["send", "--to", "beta", "--body", "closed stderr"],
        true,
    );
    assert_ne!(output.status.code(), Some(101), "post panicked");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(count(&inbox), before + 2, "the mail was delivered");
}
