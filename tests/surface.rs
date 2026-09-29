//! The command surface tells the truth on stdout, parses under `--json`, and
//! never cries wolf. Each test builds a throwaway store (`Sandbox`) and runs
//! the real binary; nothing here touches the real mail store.

mod common;

use common::{
    assert_success, from_stdout, post_command, register_alpha_beta, stderr, stdout, InboxView,
    Sandbox,
};
use post::output::SendOutput;
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::{Output, Stdio};

fn count(dir: &Path) -> usize {
    fs::read_dir(dir).map_or(0, |entries| entries.count())
}

/// A bound participant in workspace `alpha`, with its id.
fn alpha_participant(sandbox: &Sandbox, key: &str, alpha: &Path) -> String {
    sandbox.bind_claude(key, alpha, Some("alpha"))["id"]
        .as_str()
        .expect("participant id")
        .to_owned()
}

/// Run with a stdout pipe whose reader is already gone: the shape of
/// `post ... | head` after `head` exited. Writing to it fails with EPIPE.
#[cfg(unix)]
fn run_with_closed_stdout(
    sandbox: &Sandbox,
    participant: &str,
    cwd: &Path,
    args: &[&str],
) -> Output {
    let (reader, writer) = std::io::pipe().expect("stdout pipe");
    drop(reader);
    post_command()
        .args(args)
        .current_dir(cwd)
        .env("HOME", &sandbox.home)
        .env("POST_MAIL_ROOT", &sandbox.mail_root)
        .env("POST_PARTICIPANT", participant)
        .stdin(Stdio::null())
        .stdout(Stdio::from(writer))
        .stderr(Stdio::piped())
        .output()
        .expect("run post with a closed stdout pipe")
}

// ---------------------------------------------------------------------------
// Task 4: delegate completion pings (`--allow-self`)
// ---------------------------------------------------------------------------

/// The argv delegate-agent's `notify.py::notify_argv` builds for a `room:`
/// target, message included: one metadata line with an em dash.
fn delegate_ping_argv<'a>(room: &'a str, message: &'a str) -> Vec<&'a str> {
    vec![
        "send",
        "--to",
        room,
        "--kind",
        "signal",
        "--subject",
        "delegate",
        "--allow-self",
        "--body",
        message,
    ]
}

const DELEGATE_MESSAGE: &str = "delegate 7f3a9c2e done codex/gpt-5 412s \u{2014} alpha";

#[test]
fn a_delegate_ping_to_the_senders_own_room_reaches_the_sender() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let coordinator = alpha_participant(&sandbox, "coordinator", &alpha);

    let sent = sandbox.run_as_participant(
        &delegate_ping_argv("alpha", DELEGATE_MESSAGE),
        &coordinator,
        &alpha,
    );
    assert_success(&sent);
    let receipt = stdout(&sent);
    assert!(receipt.contains("sent signal"), "receipt: {receipt}");
    assert!(
        receipt.contains("--allow-self"),
        "the receipt says why the ping went to the participant inbox: {receipt}"
    );

    // The coordinator's own inbox now holds the ping.
    let inbox = sandbox.run_as_participant(&["inbox", "--json"], &coordinator, &alpha);
    assert_success(&inbox);
    let inbox: Value = from_stdout(&inbox);
    let unread = inbox["unread"].as_array().expect("unread list");
    assert_eq!(unread.len(), 1, "inbox: {inbox}");
    assert_eq!(unread[0]["kind"], "signal", "inbox: {inbox}");
    assert_eq!(unread[0]["subject"], "delegate", "inbox: {inbox}");
    let id = unread[0]["id"].as_str().expect("mail id").to_owned();
    let read = sandbox.run_as_participant(&["read", &id, "--json"], &coordinator, &alpha);
    assert_success(&read);
    let read: Value = from_stdout(&read);
    assert_eq!(read["body"], DELEGATE_MESSAGE, "read: {read}");
}

#[test]
fn without_allow_self_a_room_ping_skips_its_own_sender() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let coordinator = alpha_participant(&sandbox, "coordinator", &alpha);

    let argv: Vec<&str> = delegate_ping_argv("alpha", DELEGATE_MESSAGE)
        .into_iter()
        .filter(|arg| *arg != "--allow-self")
        .collect();
    assert_success(&sandbox.run_as_participant(&argv, &coordinator, &alpha));

    let inbox: InboxView =
        from_stdout(&sandbox.run_as_participant(&["inbox", "--json"], &coordinator, &alpha));
    assert_eq!(inbox.count, 0, "the control: no flag, no self-delivery");
}

#[test]
fn allow_self_widens_nothing_beyond_the_senders_own_room() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let coordinator = alpha_participant(&sandbox, "coordinator", &alpha);
    let peer = sandbox.bind_claude("beta-peer", &beta, Some("beta"))["id"]
        .as_str()
        .expect("participant id")
        .to_owned();

    // A ping to ANOTHER room with the flag is an ordinary send.
    let sent = sandbox.run_as_participant(
        &delegate_ping_argv("beta", DELEGATE_MESSAGE),
        &coordinator,
        &alpha,
    );
    assert_success(&sent);
    assert!(
        !stdout(&sent).contains("--allow-self"),
        "no retargeting note for a room the sender is not in: {}",
        stdout(&sent)
    );
    let own: InboxView =
        from_stdout(&sandbox.run_as_participant(&["inbox", "--json"], &coordinator, &alpha));
    assert_eq!(own.count, 0);
    let theirs: InboxView =
        from_stdout(&sandbox.run_as_participant(&["inbox", "--json"], &peer, &beta));
    assert_eq!(theirs.count, 1);
}

#[test]
fn allow_self_covers_the_senders_own_lineage_and_only_the_sender() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let coordinator = alpha_participant(&sandbox, "coordinator", &alpha);
    let peer = sandbox.bind_claude("beta-peer", &beta, Some("beta"))["id"]
        .as_str()
        .expect("participant id")
        .to_owned();
    assert_success(&sandbox.run_as_participant(
        &["identity", "new", "ember"],
        &coordinator,
        &alpha,
    ));
    assert_success(&sandbox.run_as_participant(&["identity", "continue", "ember"], &peer, &beta));

    // A ping to the sender's own lineage, which the peer shares.
    let sent = sandbox.run_as_participant(
        &delegate_ping_argv("lineage:ember", DELEGATE_MESSAGE),
        &coordinator,
        &alpha,
    );
    assert_success(&sent);
    assert!(stdout(&sent).contains("--allow-self"), "{}", stdout(&sent));

    let own: InboxView =
        from_stdout(&sandbox.run_as_participant(&["inbox", "--json"], &coordinator, &alpha));
    assert_eq!(own.count, 1, "the sender hears its own lineage ping");
    let theirs: InboxView =
        from_stdout(&sandbox.run_as_participant(&["inbox", "--json"], &peer, &beta));
    assert_eq!(theirs.count, 0, "the flag reaches the sender, nobody else");
}

#[test]
fn a_retry_fix_keeps_allow_self() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let coordinator = alpha_participant(&sandbox, "coordinator", &alpha);

    // An empty body fails before delivery and hands back a runnable retry
    // prefix; a retry that dropped the flag would change who hears the ping.
    let refused = sandbox.run_as_participant(
        &[
            "send",
            "--to",
            "alpha",
            "--allow-self",
            "--body",
            "",
            "--json",
        ],
        &coordinator,
        &alpha,
    );
    assert!(!refused.status.success(), "{}", stdout(&refused));
    let error: Value = serde_json::from_str(&stderr(&refused)).expect("error envelope");
    let fix = error["error"]["details"]["exact_fix"]
        .as_str()
        .expect("an exact_fix");
    assert!(
        fix.contains("--allow-self"),
        "the retry keeps the flag: {fix}"
    );
}

/// The text receipt says why the ping went to the sender's own inbox; the JSON
/// receipt must say it too, or a consumer sees a recipient it did not ask for.
#[test]
fn an_allow_self_json_receipt_names_the_retarget_and_only_then() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let coordinator = alpha_participant(&sandbox, "coordinator", &alpha);

    let mut argv = delegate_ping_argv("alpha", DELEGATE_MESSAGE);
    argv.push("--json");
    let sent = sandbox.run_as_participant(&argv, &coordinator, &alpha);
    assert_success(&sent);
    let receipt: Value = from_stdout(&sent);
    let retargeted = &receipt["retargeted"];
    assert_eq!(retargeted["from"], "workspace:alpha", "receipt: {receipt}");
    assert_eq!(
        retargeted["to"],
        format!("participant:{coordinator}"),
        "receipt: {receipt}"
    );
    assert!(
        retargeted["note"]
            .as_str()
            .is_some_and(|note| note.contains("--allow-self") && note.contains("participant inbox")),
        "receipt: {receipt}"
    );
    // Nothing else about the receipt changed: the parsed type still reads it.
    let typed: SendOutput = serde_json::from_value(receipt).expect("a send receipt");
    assert!(typed.retargeted.is_some());

    // A send that was not retargeted carries no such field, flag or not.
    let plain = sandbox.run_as_participant(
        &[
            "send",
            "--to",
            "beta",
            "--allow-self",
            "--body",
            "hello",
            "--json",
        ],
        &coordinator,
        &alpha,
    );
    assert_success(&plain);
    let plain: Value = from_stdout(&plain);
    assert!(plain.get("retargeted").is_none(), "receipt: {plain}");
}

#[test]
fn allow_self_stays_out_of_the_help_agents_read() {
    let sandbox = Sandbox::new();
    let help = sandbox.run(&["send", "--help"]);
    assert_success(&help);
    assert!(
        !stdout(&help).contains("allow-self"),
        "--allow-self is accepted for delegate's pings and hidden: {}",
        stdout(&help)
    );
}

// ---------------------------------------------------------------------------
// Task 2: send under --json, and a send that landed never exits nonzero
// ---------------------------------------------------------------------------

#[test]
fn send_json_leaves_stderr_empty_and_stdout_parses() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let sender = alpha_participant(&sandbox, "json-sender", &alpha);

    let output = sandbox.run_as_participant(
        &[
            "send",
            "--to",
            "beta",
            "--body",
            "quiet under json",
            "--json",
        ],
        &sender,
        &alpha,
    );
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        stderr(&output),
        "",
        "--json must leave stderr empty so `2>&1 | jq` parses"
    );
    let receipt: SendOutput = from_stdout(&output);
    assert!(receipt.ok);

    // The same command's stdout and stderr, merged the way a shell does.
    let merged = sandbox.run_in_env(
        &["send", "--to", "beta", "--body", "merged streams", "--json"],
        None,
        &alpha,
        &[("POST_PARTICIPANT", &sender)],
    );
    let mut both = merged.stdout.clone();
    both.extend_from_slice(&merged.stderr);
    serde_json::from_slice::<Value>(&both).expect("stdout+stderr is one JSON document");
}

#[cfg(unix)]
#[test]
fn a_send_that_landed_exits_zero_when_its_receipt_cannot_be_written() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let sender = alpha_participant(&sandbox, "closed-stdout-send", &alpha);
    let inbox = sandbox.mail_root.join("beta/inbox");
    let before = count(&inbox);

    for (index, extra) in [&["--json"][..], &[][..]].into_iter().enumerate() {
        let mut args = vec!["send", "--to", "beta", "--body", "lands once"];
        args.extend_from_slice(extra);
        let output = run_with_closed_stdout(&sandbox, &sender, &alpha, &args);
        assert_eq!(
            output.status.code(),
            Some(0),
            "a send that landed must not exit nonzero (a retry is a second copy): {}",
            stderr(&output)
        );
        assert!(
            stderr(&output).contains("committed"),
            "the lost receipt is noted on stderr: {}",
            stderr(&output)
        );
        assert_eq!(
            count(&inbox),
            before + index + 1,
            "exactly one mail per send"
        );
    }
}

#[cfg(unix)]
#[test]
fn read_only_commands_exit_quietly_when_the_reader_goes_away() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let participant = alpha_participant(&sandbox, "closed-stdout-reader", &alpha);

    // `post who --text | head` after head has exited: an ordinary Unix
    // stop, not a retryable io_error.
    for args in [
        &["who", "--text"][..],
        &["who", "--json"][..],
        &["rooms"][..],
        &["inbox", "--text"][..],
        &["channels", "--text"][..],
        &["doctor", "--brief"][..],
    ] {
        let output = run_with_closed_stdout(&sandbox, &participant, &alpha, args);
        assert!(
            matches!(output.status.code(), Some(0) | Some(1)),
            "{args:?}: exit {:?}, stderr: {}",
            output.status.code(),
            stderr(&output)
        );
        assert!(
            !stderr(&output).contains("io_error") && !stderr(&output).contains("retryable"),
            "{args:?}: a closed reader is not a retryable io_error: {}",
            stderr(&output)
        );
        assert_eq!(
            stderr(&output),
            "",
            "{args:?}: nothing to say on stderr for a reader that left"
        );
    }
}

#[test]
fn errors_under_a_human_flag_are_prose_and_default_errors_stay_json() {
    let sandbox = Sandbox::new();

    // `--text` asked for people-readable output: its errors are prose.
    let text = sandbox.run(&["inbox", "--text", "--room", "nosuch"]);
    assert!(!text.status.success());
    let text_error = stderr(&text);
    assert!(
        !text_error.trim_start().starts_with('{'),
        "a --text failure is prose, not a JSON envelope: {text_error}"
    );
    assert!(text_error.contains("nosuch"), "stderr: {text_error}");

    // With no human flag the JSON envelope is the contract hooks parse.
    let default = sandbox.run(&["inbox", "--room", "nosuch"]);
    assert!(!default.status.success());
    let envelope: Value = common::from_stderr(&default);
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "not_found");

    // `--json` is the same envelope, explicitly requested.
    let json = sandbox.run(&["inbox", "--json", "--room", "nosuch"]);
    assert!(!json.status.success());
    let envelope: Value = common::from_stderr(&json);
    assert_eq!(envelope["error"]["code"], "not_found");
}

// ---------------------------------------------------------------------------
// Task 3: `send`'s positional argument is the body, not a file
// ---------------------------------------------------------------------------

fn body_of(sandbox: &Sandbox, participant: &str, cwd: &Path, id: &str) -> String {
    let read =
        sandbox.run_as_participant(&["read", id, "--json", "--room", "beta"], participant, cwd);
    assert_success(&read);
    let read: Value = from_stdout(&read);
    read["body"].as_str().expect("body").to_owned()
}

#[test]
fn a_bare_argument_is_the_message_body() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let sender = alpha_participant(&sandbox, "positional-sender", &alpha);
    let reader = sandbox.bind_claude("positional-reader", &beta, Some("beta"))["id"]
        .as_str()
        .expect("participant id")
        .to_owned();

    // Short prose lands in the body slot and is sent as written.
    let short = sandbox.run_as_participant(
        &["send", "--to", "beta", "hello there", "--json"],
        &sender,
        &alpha,
    );
    assert_success(&short);
    let short: SendOutput = from_stdout(&short);
    assert_eq!(
        body_of(&sandbox, &reader, &beta, &short.envelope.id),
        "hello there"
    );

    // A body past NAME_MAX (255 bytes), which the old file slot rejected.
    let prose = "x".repeat(6000);
    let long =
        sandbox.run_as_participant(&["send", "--to", "beta", &prose, "--json"], &sender, &alpha);
    assert_success(&long);
    let long: SendOutput = from_stdout(&long);
    assert_eq!(body_of(&sandbox, &reader, &beta, &long.envelope.id), prose);
}

/// The old positional FILE, used with a file that is absent, or relative to a
/// different directory than the one the agent is in, must not send the path as
/// the message: the receipt would look right and the body would be wrong.
#[test]
fn a_path_shaped_bare_argument_is_refused_even_when_the_file_is_absent() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let sender = alpha_participant(&sandbox, "path-shaped-sender", &alpha);
    let reader = sandbox.bind_claude("path-shaped-reader", &beta, Some("beta"))["id"]
        .as_str()
        .expect("participant id")
        .to_owned();
    let delivered = || count(&sandbox.mail_root.join("beta/inbox"));

    for path in [
        "missing-notes.md",
        "report.json",
        "docs/plan.txt",
        "./today",
        "../notes/today",
        "/nowhere/at/all",
        "~/notes",
    ] {
        assert!(!alpha.join(path).exists(), "fixture: {path} must be absent");
        let refused =
            sandbox.run_as_participant(&["send", "--to", "beta", path, "--json"], &sender, &alpha);
        assert_eq!(
            refused.status.code(),
            Some(2),
            "{path}: stdout {} stderr {}",
            stdout(&refused),
            stderr(&refused)
        );
        let error: Value = common::from_stderr(&refused);
        assert_eq!(error["error"]["code"], "invalid_argument", "{path}");
        let fix = error["error"]["details"]["exact_fix"]
            .as_str()
            .unwrap_or_else(|| panic!("{path}: no exact_fix in {error}"));
        assert!(
            fix.contains("--body-file") && fix.contains(path),
            "{path}: the fix names the value: {fix}"
        );
        assert_eq!(delivered(), 0, "{path}: nothing was sent");
    }

    // The fix, run where the file lives, sends that file's contents.
    fs::create_dir_all(alpha.join("docs")).expect("docs dir");
    fs::write(alpha.join("docs/plan.txt"), "the plan\n").expect("write the plan");
    let refused = sandbox.run_as_participant(
        &["send", "--to", "beta", "docs/plan.txt", "--json"],
        &sender,
        &alpha,
    );
    assert_eq!(
        refused.status.code(),
        Some(2),
        "stderr: {}",
        stderr(&refused)
    );
    let error: Value = common::from_stderr(&refused);
    let fix = error["error"]["details"]["exact_fix"]
        .as_str()
        .expect("an exact_fix")
        .to_owned();
    assert!(fix.contains("--body-file"), "fix: {fix}");
    assert_eq!(delivered(), 0, "the refused send delivered nothing");
    let ran = sandbox.run_fix(&format!("{fix} --json"), &alpha);
    assert_success(&ran);
    let sent: SendOutput = from_stdout(&ran);
    assert_eq!(
        body_of(&sandbox, &reader, &beta, &sent.envelope.id),
        "the plan\n"
    );

    // Text that only resembles a path still sends: prose, numbers, a word
    // with a period, and a URL. `--body` carries a literal path on purpose.
    for text in [
        "hello there",
        "ok",
        "done.",
        "v1.2",
        "0.9.0",
        "see docs/plan.txt for details",
        "https://example.com/a/b",
    ] {
        let before = delivered();
        let sent =
            sandbox.run_as_participant(&["send", "--to", "beta", text, "--json"], &sender, &alpha);
        assert_success(&sent);
        let sent: SendOutput = from_stdout(&sent);
        assert_eq!(delivered(), before + 1, "{text}");
        assert_eq!(
            body_of(&sandbox, &reader, &beta, &sent.envelope.id),
            text,
            "{text}"
        );
    }
    let explicit = sandbox.run_as_participant(
        &[
            "send",
            "--to",
            "beta",
            "--body",
            "docs/absent.txt",
            "--json",
        ],
        &sender,
        &alpha,
    );
    assert_success(&explicit);
    let explicit: SendOutput = from_stdout(&explicit);
    assert_eq!(
        body_of(&sandbox, &reader, &beta, &explicit.envelope.id),
        "docs/absent.txt"
    );
}

#[test]
fn a_recipient_given_by_position_is_named_as_the_mistake() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let sender = alpha_participant(&sandbox, "positional-recipient", &alpha);

    let refused = sandbox.run_as_participant(&["send", "beta", "--body", "hello"], &sender, &alpha);
    assert_eq!(
        refused.status.code(),
        Some(2),
        "stderr: {}",
        stderr(&refused)
    );
    let error = stderr(&refused);
    assert!(
        error.contains("--to") && error.contains("message body"),
        "the remedy says the recipient is --to and a bare argument is the body: {error}"
    );
    assert!(
        !error.contains("body FILE"),
        "the old file wording is gone: {error}"
    );
}

#[test]
fn a_bare_body_cannot_be_combined_with_another_body_source() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let sender = alpha_participant(&sandbox, "two-bodies", &alpha);
    fs::write(alpha.join("b.txt"), "x").expect("write body file");

    for args in [
        &["send", "--to", "beta", "one", "--body", "two"][..],
        &["send", "--to", "beta", "one", "--body-file", "b.txt"][..],
    ] {
        let refused = sandbox.run_as_participant(args, &sender, &alpha);
        assert_eq!(
            refused.status.code(),
            Some(2),
            "{args:?}: {}",
            stderr(&refused)
        );
    }
    assert_eq!(count(&sandbox.mail_root.join("beta/inbox")), 0);
}

#[test]
fn send_help_teaches_the_safe_body_forms_first() {
    let sandbox = Sandbox::new();
    let help = sandbox.run(&["send", "--help"]);
    assert_success(&help);
    let text = stdout(&help);
    assert!(
        !text.contains("[FILE]"),
        "the deprecated positional is gone: {text}"
    );
    assert!(
        text.contains("[BODY]"),
        "the bare body argument is documented: {text}"
    );
    assert!(
        text.contains("short one-liners"),
        "--body is scoped to short one-liners: {text}"
    );
    let heredoc = text.find("<<'EOF'").expect("usage teaches the heredoc");
    let body_flag = text.find("--body <TEXT>").expect("usage lists --body");
    assert!(
        heredoc < body_flag,
        "the heredoc comes before --body in the usage: {text}"
    );
    assert!(text.contains("--body-file"), "help: {text}");
}
