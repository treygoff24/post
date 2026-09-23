//! A2: a `post chat` read refuses stdin that carries input, and only that.
//! Each stdin kind is its own test. Every refusal must leave the unread
//! message unread and send nothing.

mod common;

use common::{
    assert_success, post_command, register_alpha_beta, stderr, write_channel_message, Sandbox,
};
use serde_json::Value;
use std::fs::{self, File};
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

const UNREAD: &str = "20260923-070000-000001-cafe01";

struct Fixture {
    sandbox: Sandbox,
    alpha: PathBuf,
    participant: String,
}

/// A bound member of `tax` with one unread message from beta.
fn fixture() -> Fixture {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let participant = sandbox.bind_claude("stdin-guard", &alpha, Some("alpha"))["id"]
        .as_str()
        .expect("participant id")
        .to_owned();
    assert_success(&sandbox.run_as_participant(
        &["chat", "tax", "--join", "--json"],
        &participant,
        &alpha,
    ));
    write_channel_message(&sandbox, "tax", UNREAD, "beta", "unread", "still unread");
    Fixture {
        sandbox,
        alpha,
        participant,
    }
}

fn chat_with_stdin(fixture: &Fixture, args: &[&str], stdin: Stdio) -> (Output, Duration) {
    let started = Instant::now();
    let output = post_command()
        .args(args)
        .current_dir(&fixture.alpha)
        .env("HOME", &fixture.sandbox.home)
        .env("POST_MAIL_ROOT", &fixture.sandbox.mail_root)
        .env("POST_PARTICIPANT", &fixture.participant)
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run post");
    (output, started.elapsed())
}

fn message_files(fixture: &Fixture) -> usize {
    fs::read_dir(fixture.sandbox.mail_root.join("channels/tax/messages"))
        .expect("channel messages")
        .count()
}

/// Read ids with a stdin of /dev/null (a normal consuming read).
fn read_ids(fixture: &Fixture) -> Vec<String> {
    let (output, _) = chat_with_stdin(fixture, &["chat", "tax", "--json"], Stdio::null());
    assert_success(&output);
    let value: Value = serde_json::from_slice(&output.stdout).expect("chat JSON");
    value["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .map(|message| message["id"].as_str().expect("id").to_owned())
        .collect()
}

fn assert_normal_read(output: &Output) {
    assert!(output.status.success(), "a normal read: {}", stderr(output));
    let value: Value = serde_json::from_slice(&output.stdout).expect("chat JSON");
    let ids: Vec<&str> = value["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .map(|message| message["id"].as_str().expect("id"))
        .collect();
    assert!(
        ids.contains(&UNREAD),
        "the read delivered the message: {ids:?}"
    );
}

/// The refusal names both corrections, consumed nothing, and sent nothing.
fn assert_refused(fixture: &Fixture, output: &Output, code: &str, before_files: usize) {
    assert_eq!(output.status.code(), Some(2), "{}", stderr(output));
    assert!(output.stdout.is_empty(), "a refusal prints no read");
    let error: Value = serde_json::from_slice(&output.stderr).expect("error JSON");
    assert_eq!(error["error"]["code"], code, "{error}");
    let fix = error["error"]["details"]["exact_fix"]
        .as_str()
        .expect("exact_fix");
    assert!(
        fix.starts_with("post chat 'tax' --send --body-file -"),
        "{fix}"
    );
    assert!(fix.contains("< /dev/null"), "{fix}");
    assert_eq!(message_files(fixture), before_files, "nothing was sent");
    assert!(
        read_ids(fixture).contains(&UNREAD.to_owned()),
        "nothing was marked seen"
    );
}

#[test]
fn dev_null_stdin_is_a_normal_read() {
    let fixture = fixture();
    let (output, _) = chat_with_stdin(&fixture, &["chat", "tax", "--json"], Stdio::null());
    assert_normal_read(&output);
}

#[test]
fn empty_regular_file_stdin_is_a_normal_read() {
    let fixture = fixture();
    let path = fixture.sandbox.path.join("empty-stdin");
    File::create(&path).expect("create empty file");
    let (output, _) = chat_with_stdin(
        &fixture,
        &["chat", "tax", "--json"],
        Stdio::from(File::open(&path).expect("open empty file")),
    );
    assert_normal_read(&output);
}

#[test]
fn pipe_at_eof_stdin_is_a_normal_read() {
    let fixture = fixture();
    let (reader, writer) = std::io::pipe().expect("pipe");
    drop(writer);
    let (output, _) = chat_with_stdin(&fixture, &["chat", "tax", "--json"], Stdio::from(reader));
    assert_normal_read(&output);
}

#[test]
fn nonempty_pipe_stdin_is_refused_and_the_send_fix_runs_as_written() {
    let fixture = fixture();
    let before = message_files(&fixture);
    let (reader, mut writer) = std::io::pipe().expect("pipe");
    writer.write_all(b"meant to be sent").expect("write body");
    drop(writer);
    let (output, _) = chat_with_stdin(&fixture, &["chat", "tax", "--json"], Stdio::from(reader));
    assert_refused(&fixture, &output, "invalid_argument", before);

    // The exact_fix runs as written with the same producer re-attached.
    let error: Value = serde_json::from_slice(&output.stderr).expect("error JSON");
    let fix = error["error"]["details"]["exact_fix"]
        .as_str()
        .expect("exact_fix")
        .replacen("post ", &format!("'{}' ", env!("CARGO_BIN_EXE_post")), 1);
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg(&fix)
        .current_dir(&fixture.alpha)
        .env("HOME", &fixture.sandbox.home)
        .env("POST_MAIL_ROOT", &fixture.sandbox.mail_root)
        .env("POST_PARTICIPANT", &fixture.participant)
        .env_remove("POST_FROM")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run the fix");
    child
        .stdin
        .take()
        .expect("fix stdin")
        .write_all(b"meant to be sent")
        .expect("pipe the body");
    let sent = child.wait_with_output().expect("fix output");
    assert!(sent.status.success(), "{}", stderr(&sent));
    assert_eq!(message_files(&fixture), before + 1, "the fix sent the body");
}

#[test]
fn nonempty_regular_file_stdin_is_refused() {
    let fixture = fixture();
    let before = message_files(&fixture);
    let path = fixture.sandbox.path.join("body-stdin");
    fs::write(&path, "meant to be sent").expect("write body file");
    let (output, _) = chat_with_stdin(
        &fixture,
        &["chat", "tax", "--json"],
        Stdio::from(File::open(&path).expect("open body file")),
    );
    assert_refused(&fixture, &output, "invalid_argument", before);
}

#[test]
fn heredoc_stdin_is_refused() {
    let fixture = fixture();
    let before = message_files(&fixture);
    let script = format!(
        "'{}' chat tax --json <<'EOF'\nmeant to be sent\nEOF\n",
        env!("CARGO_BIN_EXE_post")
    );
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(&script)
        .current_dir(&fixture.alpha)
        .env("HOME", &fixture.sandbox.home)
        .env("POST_MAIL_ROOT", &fixture.sandbox.mail_root)
        .env("POST_PARTICIPANT", &fixture.participant)
        .env_remove("POST_FROM")
        .stdin(Stdio::null())
        .output()
        .expect("run heredoc");
    assert_refused(&fixture, &output, "invalid_argument", before);
}

#[test]
fn socket_stdin_with_a_queued_byte_is_refused() {
    let fixture = fixture();
    let before = message_files(&fixture);
    let (reader, mut writer) = UnixStream::pair().expect("socket pair");
    writer.write_all(b"x").expect("queue a byte");
    let (output, _) = chat_with_stdin(
        &fixture,
        &["chat", "tax", "--json"],
        Stdio::from(std::os::fd::OwnedFd::from(reader)),
    );
    drop(writer);
    assert_refused(&fixture, &output, "invalid_argument", before);
}

#[test]
fn peek_with_piped_input_is_refused_too() {
    let fixture = fixture();
    let before = message_files(&fixture);
    let (reader, mut writer) = std::io::pipe().expect("pipe");
    writer.write_all(b"meant to be sent").expect("write body");
    drop(writer);
    let (output, _) = chat_with_stdin(
        &fixture,
        &["chat", "tax", "--peek", "--json"],
        Stdio::from(reader),
    );
    assert_refused(&fixture, &output, "invalid_argument", before);
}

#[test]
fn delayed_writer_inside_the_bound_is_refused() {
    let fixture = fixture();
    let before = message_files(&fixture);
    let (reader, mut writer) = std::io::pipe().expect("pipe");
    let producer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        writer.write_all(b"late body").expect("write body");
        writer
    });
    let (output, _) = chat_with_stdin(&fixture, &["chat", "tax", "--json"], Stdio::from(reader));
    drop(producer.join().expect("producer"));
    assert_refused(&fixture, &output, "invalid_argument", before);
}

#[test]
fn silent_open_pipe_past_the_bound_is_input_ambiguous() {
    let fixture = fixture();
    let before = message_files(&fixture);
    let (reader, mut writer) = std::io::pipe().expect("pipe");
    let hold = Duration::from_secs(1);
    let producer = std::thread::spawn(move || {
        // Slower than the bound: the input arrives after post has decided.
        std::thread::sleep(hold);
        let _ = writer.write_all(b"too late");
    });
    let (output, elapsed) =
        chat_with_stdin(&fixture, &["chat", "tax", "--json"], Stdio::from(reader));
    assert!(
        elapsed < hold,
        "the wait is bounded, not held until the producer finishes: {elapsed:?}"
    );
    assert_refused(&fixture, &output, "input_ambiguous", before);
    producer.join().expect("producer");
}

/// Every file under the store, with its bytes: a refused consuming flag must
/// leave all of it, cursors included, byte-identical.
fn store_bytes(fixture: &Fixture) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    fn walk(path: &std::path::Path, out: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(path).expect("read store dir") {
            let path = entry.expect("store entry").path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.insert(path.clone(), fs::read(&path).expect("read store file"));
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    walk(&fixture.sandbox.mail_root, &mut out);
    out
}

fn cursor_files(fixture: &Fixture) -> Vec<PathBuf> {
    store_bytes(fixture)
        .into_keys()
        .filter(|path| path.file_name().is_some_and(|name| name == "cursors.json"))
        .collect()
}

/// `--ack <id>` and `--discard-through <id>` consume read state, so they run
/// the same guard as a read.
const CONSUMING_FLAGS: [&str; 2] = ["--ack", "--discard-through"];

#[test]
fn consuming_flags_with_a_queued_byte_are_refused_and_change_nothing() {
    for flag in CONSUMING_FLAGS {
        let fixture = fixture();
        let before_files = message_files(&fixture);
        let before = store_bytes(&fixture);
        let (reader, mut writer) = std::io::pipe().expect("pipe");
        writer.write_all(b"meant to be sent").expect("write body");
        drop(writer);
        let (output, _) = chat_with_stdin(
            &fixture,
            &["chat", "tax", flag, UNREAD, "--json"],
            Stdio::from(reader),
        );
        assert_eq!(output.status.code(), Some(2), "{flag}: {}", stderr(&output));
        let error: Value = serde_json::from_slice(&output.stderr).expect("error JSON");
        assert_eq!(
            error["error"]["code"], "invalid_argument",
            "{flag}: {error}"
        );
        assert_eq!(
            store_bytes(&fixture),
            before,
            "{flag}: the store, cursors included, is byte-identical"
        );
        assert_refused(&fixture, &output, "invalid_argument", before_files);
    }
}

#[test]
fn consuming_flags_with_dev_null_stdin_consume_as_before() {
    for flag in CONSUMING_FLAGS {
        let fixture = fixture();
        let (output, _) = chat_with_stdin(
            &fixture,
            &["chat", "tax", flag, UNREAD, "--json"],
            Stdio::null(),
        );
        assert!(output.status.success(), "{flag}: {}", stderr(&output));
        assert!(
            !cursor_files(&fixture).is_empty(),
            "{flag}: the consuming flag wrote a cursor"
        );
        assert!(
            !read_ids(&fixture).contains(&UNREAD.to_owned()),
            "{flag}: the message was consumed"
        );
    }
}

#[test]
fn consuming_flags_with_a_silent_open_pipe_are_input_ambiguous() {
    for flag in CONSUMING_FLAGS {
        let fixture = fixture();
        let before_files = message_files(&fixture);
        let before = store_bytes(&fixture);
        let (reader, writer) = std::io::pipe().expect("pipe");
        let (output, _) = chat_with_stdin(
            &fixture,
            &["chat", "tax", flag, UNREAD, "--json"],
            Stdio::from(reader),
        );
        drop(writer);
        assert_eq!(output.status.code(), Some(2), "{flag}: {}", stderr(&output));
        assert_eq!(
            store_bytes(&fixture),
            before,
            "{flag}: the store, cursors included, is byte-identical"
        );
        assert_refused(&fixture, &output, "input_ambiguous", before_files);
    }
}

#[test]
fn the_refusal_names_the_ssh_case() {
    let fixture = fixture();
    let (reader, writer) = std::io::pipe().expect("pipe");
    let (output, _) = chat_with_stdin(&fixture, &["chat", "tax", "--json"], Stdio::from(reader));
    drop(writer);
    let error: Value = serde_json::from_slice(&output.stderr).expect("error JSON");
    let text = error.to_string();
    assert!(text.contains("ssh -n"), "{text}");
}
