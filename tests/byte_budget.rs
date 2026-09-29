mod common;

use common::{
    assert_success, from_stdout, register_alpha_beta, seed_channel_fixture, seed_fence_store,
    write_bad_channel, write_channel_message, write_custom_mail, Sandbox,
};
use post::output::{CatchupOutput, CatchupTarget, ChatReadOutput};
use serde_json::json;
use serde_json::Value;
use std::fs;

fn channel_fixture(sandbox: &Sandbox, name: &str, room: &str) {
    write_bad_channel(
        sandbox,
        name,
        Some(&format!(r#"{{"{room}":"2026-09-06 12:00:00 +0000"}}"#)),
        true,
        &format!(
            r#"{{"name":"{name}","created":"2026-09-06 12:00:00 +0000","created_by":"{room}"}}"#
        ),
    );
}

fn write_mentioned_channel_message(
    sandbox: &Sandbox,
    channel: &str,
    id: &str,
    body: &str,
    mentioned_room: &str,
) {
    let envelope = json!({
        "id": id,
        "from": "alpha",
        "channel": channel,
        "subject": "mention",
        "sent": "2026-09-06 12:00:00 +0000",
        "mentions": [mentioned_room]
    });
    fs::write(
        sandbox
            .mail_root
            .join(format!("channels/{channel}/messages/{id}.msg")),
        format!(
            "{}\n---\n{body}",
            serde_json::to_string_pretty(&envelope).expect("mention envelope")
        ),
    )
    .expect("mentioned channel message");
}

fn continuation_budget(command: &str) -> usize {
    let parts: Vec<&str> = command.split_whitespace().collect();
    let index = parts
        .iter()
        .position(|part| *part == "--max-bytes")
        .expect("continuation max-bytes");
    parts[index + 1]
        .parse()
        .expect("numeric continuation budget")
}

fn follow_continuation_chain(
    sandbox: &Sandbox,
    cwd: &std::path::Path,
    initial_command: &str,
) -> (String, Vec<(usize, usize)>) {
    let mut command = initial_command.to_owned();
    let mut reconstructed = String::new();
    let mut ranges = Vec::new();
    for _ in 0..512 {
        let output = sandbox.run_fix(&command, cwd);
        assert_success(&output);
        let value: Value = from_stdout(&output);
        let start = value["range"]["start"].as_u64().expect("range start") as usize;
        let end = value["range"]["end_exclusive"].as_u64().expect("range end") as usize;
        let cap = value["byte_limit"].as_u64().expect("byte limit") as usize;
        assert!(output.stdout.len() <= cap);
        if value["next_offset"].is_null() {
            assert!(end >= start, "terminal empty/EOF slice may be zero-width");
        } else {
            assert!(end > start, "every nonterminal slice must progress");
        }
        reconstructed.push_str(value["body_slice"].as_str().expect("body slice"));
        ranges.push((start, end));
        let Some(next) = value["continuation"].as_str() else {
            return (reconstructed, ranges);
        };
        command = next.to_owned();
    }
    panic!("continuation chain did not terminate in 512 steps")
}

fn seen_ids(sandbox: &Sandbox, room: &str, channel: &str) -> Vec<String> {
    let participant = sandbox.test_participant(room);
    let path = sandbox
        .mail_root
        .join("participants")
        .join(participant)
        .join("cursors.json");
    if !path.exists() {
        return Vec::new();
    }
    let state: Value =
        serde_json::from_slice(&fs::read(path).expect("read cursor")).expect("cursor JSON");
    state["channels"][channel]["seen"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

fn mail_seen_ids(sandbox: &Sandbox, room: &str) -> Vec<String> {
    let participant = sandbox.test_participant(room);
    let path = sandbox
        .mail_root
        .join("participants")
        .join(participant)
        .join("cursors.json");
    if !path.exists() {
        return Vec::new();
    }
    let state: Value =
        serde_json::from_slice(&fs::read(path).expect("read cursor")).expect("cursor JSON");
    state["mail"][format!("workspace:{room}")]["seen"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

#[cfg(unix)]
fn assert_stdout_io_failure(output: &std::process::Output) {
    assert_eq!(
        output.status.code(),
        Some(75),
        "stderr: {}",
        common::stderr(output)
    );
    let error: Value = common::from_stderr(output);
    assert_eq!(error["error"]["code"], "io_error");
    assert_eq!(error["error"]["details"]["operation"], "write stdout");
}

#[test]
fn chat_budget_admits_only_a_complete_prefix_and_consumes_only_that_prefix() {
    let sandbox = Sandbox::new();
    let (_alpha, beta) = register_alpha_beta(&sandbox);
    channel_fixture(&sandbox, "bounded", "beta");
    let ids = [
        "20990906-120000-000001-aaaaa1",
        "20990906-120000-000002-aaaaa2",
        "20990906-120000-000003-aaaaa3",
    ];
    write_channel_message(&sandbox, "bounded", ids[0], "alpha", "small", "first");
    write_channel_message(
        &sandbox,
        "bounded",
        ids[1],
        "alpha",
        "whale",
        &"x".repeat(8_000),
    );
    write_channel_message(&sandbox, "bounded", ids[2], "alpha", "later", "third");

    let output = sandbox.run_in(
        &[
            "chat",
            "bounded",
            "--limit",
            "0",
            "--max-bytes",
            "2000",
            "--json",
        ],
        None,
        &beta,
    );
    assert_success(&output);
    assert!(
        output.stdout.len() <= 2_000,
        "stdout exceeded the byte limit"
    );
    let typed: ChatReadOutput = from_stdout(&output);
    assert_eq!(typed.selected_count, Some(3));
    assert_eq!(typed.byte_limit, Some(2_000));
    assert_eq!(typed.omitted.as_ref().map(|omitted| omitted.count), Some(2));
    let value: Value = from_stdout(&output);
    assert_eq!(value["count"], 1);
    assert_eq!(value["selected_count"], 3);
    assert_eq!(value["messages"][0]["id"], ids[0]);
    assert_eq!(value["omitted"]["reason"], "byte_limit");
    assert_eq!(value["omitted"]["count"], 2);
    assert_eq!(value["omitted"]["first_id"], ids[1]);
    assert_eq!(value["has_more"], true);
    assert_eq!(seen_ids(&sandbox, "beta", "bounded"), vec![ids[0]]);
}

#[test]
fn channel_utf8_slices_reconstruct_the_body_and_never_consume() {
    let sandbox = Sandbox::new();
    let (_alpha, beta) = register_alpha_beta(&sandbox);
    channel_fixture(&sandbox, "slices", "beta");
    let id = "20990906-120000-000011-bbbbb1";
    let body = format!("Aé🙂\"\\\n\u{1f}Z{}", "é🙂quote\"slash\\\n".repeat(180));
    write_channel_message(&sandbox, "slices", id, "alpha", "slice", &body);

    let mut offset = 0usize;
    let mut reconstructed = String::new();
    loop {
        let offset_arg = offset.to_string();
        let output = sandbox.run_in(
            &[
                "chat",
                "slices",
                "--message",
                id,
                "--offset",
                &offset_arg,
                "--max-bytes",
                "1400",
                "--json",
            ],
            None,
            &beta,
        );
        assert_success(&output);
        assert!(output.stdout.len() <= 1_400);
        let value: Value = from_stdout(&output);
        assert!(value.get("body").is_none());
        assert_eq!(value["range"]["start"], offset);
        let end = value["range"]["end_exclusive"].as_u64().expect("slice end") as usize;
        assert!(end > offset, "successful partial slices must progress");
        reconstructed.push_str(value["body_slice"].as_str().expect("body slice"));
        match value["next_offset"].as_u64() {
            Some(next) => {
                assert_eq!(next as usize, end);
                offset = end;
            }
            None => break,
        }
    }
    assert_eq!(reconstructed.as_bytes(), body.as_bytes());
    assert!(seen_ids(&sandbox, "beta", "slices").is_empty());

    let retreated = sandbox.run_in(
        &[
            "chat",
            "slices",
            "--message",
            id,
            "--offset",
            "0",
            "--length",
            "2",
            "--max-bytes",
            "1400",
            "--json",
        ],
        None,
        &beta,
    );
    assert_success(&retreated);
    let retreated: Value = from_stdout(&retreated);
    assert_eq!(retreated["body_slice"], "A");
    assert_eq!(retreated["range"]["end_exclusive"], 1);
    assert_eq!(retreated["next_offset"], 1);

    let split_scalar = sandbox.run_in(
        &[
            "chat",
            "slices",
            "--message",
            id,
            "--offset",
            "2",
            "--max-bytes",
            "1400",
            "--json",
        ],
        None,
        &beta,
    );
    assert_eq!(split_scalar.status.code(), Some(2));
    assert!(split_scalar.stdout.is_empty());

    let too_short = sandbox.run_in(
        &[
            "chat",
            "slices",
            "--message",
            id,
            "--offset",
            "1",
            "--length",
            "1",
            "--max-bytes",
            "1400",
            "--json",
        ],
        None,
        &beta,
    );
    assert_eq!(too_short.status.code(), Some(2));
    assert!(too_short.stdout.is_empty());
}

#[test]
fn direct_mail_budget_slices_and_exact_ack_preserve_unrelated_unread_mail() {
    let sandbox = Sandbox::new();
    let (_alpha, beta) = register_alpha_beta(&sandbox);
    let inbox = sandbox.mail_root.join("beta/inbox");
    let beta_participant = sandbox.test_participant("beta");
    fs::create_dir_all(&inbox).expect("inbox");
    let ids = [
        "20990906-115959-aaaaa1",
        "20990906-120000-aaaaa2",
        "20990906-120001-aaaaa3",
    ];
    let body = format!("é🙂\"\\\n{}", "payload-é🙂\n".repeat(220));
    for (id, message_body) in [
        (ids[0], "older"),
        (ids[1], body.as_str()),
        (ids[2], "newer"),
    ] {
        write_custom_mail(
            &inbox,
            id,
            &json!({
                "id": id,
                "from": "alpha",
                "to": "beta",
                "kind": "note",
                "subject": "bounded mail",
                "sent": "2026-09-06 12:00:00 +0000"
            }),
            message_body,
        );
    }

    let bounded = sandbox.run_as_participant(
        &[
            "read",
            ids[1],
            "--room",
            "beta",
            "--max-bytes",
            "1200",
            "--json",
        ],
        &beta_participant,
        &beta,
    );
    assert_success(&bounded);
    assert!(bounded.stdout.len() <= 1_200);
    let bounded_json: Value = from_stdout(&bounded);
    assert_eq!(bounded_json["count"], 0);
    assert!(bounded_json.get("body").is_none());
    assert_eq!(bounded_json["omitted"]["first_id"], ids[1]);

    let mut offset = 0usize;
    let mut reconstructed = String::new();
    loop {
        let offset_arg = offset.to_string();
        let sliced = sandbox.run_as_participant(
            &[
                "read",
                ids[1],
                "--room",
                "beta",
                "--offset",
                &offset_arg,
                "--max-bytes",
                "1200",
                "--json",
            ],
            &beta_participant,
            &beta,
        );
        assert_success(&sliced);
        assert!(sliced.stdout.len() <= 1_200);
        let value: Value = from_stdout(&sliced);
        reconstructed.push_str(value["body_slice"].as_str().expect("body slice"));
        match value["next_offset"].as_u64() {
            Some(next) => {
                assert!(next as usize > offset);
                offset = next as usize;
            }
            None => break,
        }
    }
    assert_eq!(reconstructed.as_bytes(), body.as_bytes());

    let ack = sandbox.run_as_participant(
        &["read", ids[1], "--room", "beta", "--ack", "--json"],
        &beta_participant,
        &beta,
    );
    assert_success(&ack);
    let state: Value = serde_json::from_slice(
        &fs::read(
            sandbox
                .mail_root
                .join("participants")
                .join(&beta_participant)
                .join("cursors.json"),
        )
        .expect("cursor"),
    )
    .expect("cursor JSON");
    assert_eq!(state["mail"]["workspace:beta"]["seen"], json!([ids[1]]));

    let malformed_id = "20990906-120002-aaaaa4";
    fs::write(inbox.join(format!("{malformed_id}.mail")), "malformed")
        .expect("malformed mail target");
    let malformed_ack = sandbox.run_in(
        &["read", malformed_id, "--room", "beta", "--ack", "--json"],
        None,
        &beta,
    );
    assert_eq!(malformed_ack.status.code(), Some(78));
    assert!(malformed_ack.stdout.is_empty());
    let state: Value = serde_json::from_slice(
        &fs::read(
            sandbox
                .mail_root
                .join("participants")
                .join(&beta_participant)
                .join("cursors.json"),
        )
        .expect("cursor"),
    )
    .expect("cursor JSON");
    assert_eq!(state["mail"]["workspace:beta"]["seen"], json!([ids[1]]));
}

#[test]
fn channel_exact_ack_marks_only_its_target_even_for_a_rescued_old_mention() {
    let sandbox = Sandbox::new();
    let (_alpha, beta) = register_alpha_beta(&sandbox);
    channel_fixture(&sandbox, "ack", "beta");
    let ids = [
        "20990906-120000-000021-ccccc1",
        "20990906-120000-000022-ccccc2",
        "20990906-120000-000023-ccccc3",
    ];
    write_channel_message(&sandbox, "ack", ids[0], "alpha", "older", "older");
    let mention = json!({
        "id": ids[1],
        "from": "alpha",
        "channel": "ack",
        "subject": "rescued mention",
        "sent": "2026-09-06 12:00:00 +0000",
        "mentions": ["beta"]
    });
    fs::write(
        sandbox
            .mail_root
            .join(format!("channels/ack/messages/{}.msg", ids[1])),
        format!(
            "{}\n---\n@beta important",
            serde_json::to_string_pretty(&mention).expect("mention envelope")
        ),
    )
    .expect("mention message");
    write_channel_message(&sandbox, "ack", ids[2], "alpha", "newer", "newer");

    let rescued = sandbox.run_in(
        &["chat", "ack", "--peek", "--limit", "1", "--json"],
        None,
        &beta,
    );
    assert_success(&rescued);
    let rescued: Value = from_stdout(&rescued);
    assert_eq!(rescued["messages"][0]["id"], ids[1]);
    assert_eq!(rescued["messages"][1]["id"], ids[2]);
    assert_eq!(rescued["skipped"], 1);

    let ack = sandbox.run_in(&["chat", "ack", "--ack", ids[1], "--json"], None, &beta);
    assert_success(&ack);
    assert_eq!(seen_ids(&sandbox, "beta", "ack"), vec![ids[1]]);

    let peek = sandbox.run_in(
        &["chat", "ack", "--peek", "--limit", "0", "--json"],
        None,
        &beta,
    );
    assert_success(&peek);
    let peek: Value = from_stdout(&peek);
    assert_eq!(peek["count"], 2);
    assert_eq!(peek["messages"][0]["id"], ids[0]);
    assert_eq!(peek["messages"][1]["id"], ids[2]);

    let bad_id = "20990906-120000-000024-ccccc4";
    fs::write(
        sandbox
            .mail_root
            .join(format!("channels/ack/messages/{bad_id}.msg")),
        "malformed",
    )
    .expect("malformed target");
    let bad = sandbox.run_in(&["chat", "ack", "--ack", bad_id, "--json"], None, &beta);
    assert_eq!(bad.status.code(), Some(78));
    assert!(bad.stdout.is_empty());
    assert_eq!(seen_ids(&sandbox, "beta", "ack"), vec![ids[1]]);
}

#[test]
fn catchup_uses_one_budget_across_targets_and_consumes_only_complete_prefixes() {
    let sandbox = Sandbox::new();
    let (_alpha, beta) = register_alpha_beta(&sandbox);
    channel_fixture(&sandbox, "mixed", "beta");
    let inbox = sandbox.mail_root.join("beta/inbox");
    fs::create_dir_all(&inbox).expect("inbox");
    let mail_id = "20990906-120100-ddddd1";
    write_custom_mail(
        &inbox,
        mail_id,
        &json!({
            "id": mail_id,
            "from": "alpha",
            "to": "beta",
            "kind": "note",
            "subject": "small mail",
            "sent": "2026-09-06 12:01:00 +0000"
        }),
        "mail body",
    );
    let channel_ids = [
        "20990906-120100-000001-ddddd2",
        "20990906-120100-000002-ddddd3",
        "20990906-120100-000003-ddddd4",
    ];
    write_channel_message(
        &sandbox,
        "mixed",
        channel_ids[0],
        "alpha",
        "small channel",
        "channel body",
    );
    let whale = json!({
        "id": channel_ids[1],
        "from": "alpha",
        "channel": "mixed",
        "subject": "addressed whale",
        "sent": "2026-09-06 12:01:00 +0000",
        "mentions": ["beta"]
    });
    fs::write(
        sandbox
            .mail_root
            .join(format!("channels/mixed/messages/{}.msg", channel_ids[1])),
        format!(
            "{}\n---\n@beta {}",
            serde_json::to_string_pretty(&whale).expect("whale envelope"),
            "w".repeat(8_000)
        ),
    )
    .expect("whale message");
    write_channel_message(
        &sandbox,
        "mixed",
        channel_ids[2],
        "alpha",
        "later channel",
        "later body",
    );

    let output = sandbox.run_in(
        &["catchup", "--all", "--max-bytes", "3000", "--json"],
        None,
        &beta,
    );
    assert_success(&output);
    assert!(output.stdout.len() <= 3_000);
    let typed: CatchupOutput = from_stdout(&output);
    assert_eq!(typed.selected_count, Some(4));
    assert_eq!(typed.has_more, Some(true));
    assert!(matches!(
        typed.targets.as_slice(),
        [
            CatchupTarget::Mail {
                selected_count: Some(1),
                has_more: Some(false),
                ..
            },
            CatchupTarget::Channel {
                selected_count: Some(3),
                has_more: Some(true),
                ..
            }
        ]
    ));
    let value: Value = from_stdout(&output);
    assert_eq!(value["selected_count"], 4);
    assert_eq!(value["count"], 2);
    assert_eq!(value["targets"][0]["source"], "mail");
    assert_eq!(value["targets"][0]["count"], 1);
    assert_eq!(value["targets"][1]["source"], "channel");
    assert_eq!(value["targets"][1]["count"], 1);
    assert_eq!(value["targets"][1]["selected_count"], 3);
    assert_eq!(value["omitted"]["source"], "channel");
    assert_eq!(value["omitted"]["channel"], "mixed");
    assert_eq!(value["omitted"]["first_id"], channel_ids[1]);
    assert_eq!(mail_seen_ids(&sandbox, "beta"), vec![mail_id]);
    assert_eq!(seen_ids(&sandbox, "beta", "mixed"), vec![channel_ids[0]]);

    let text = sandbox.run_in(
        &[
            "catchup",
            "mixed",
            "--max-bytes",
            "3000",
            "--framing",
            "compact",
        ],
        None,
        &beta,
    );
    assert_success(&text);
    assert!(text.stdout.len() <= 3_000);
    assert!(common::stdout(&text).contains("catchup partial"));
    let pretty = sandbox.run_in(
        &[
            "catchup",
            "mixed",
            "--max-bytes",
            "3000",
            "--json",
            "--pretty",
        ],
        None,
        &beta,
    );
    assert_success(&pretty);
    assert!(pretty.stdout.len() <= 3_000);
    let pretty: Value = from_stdout(&pretty);
    assert_eq!(pretty["count"], 0);
    assert_eq!(pretty["omitted"]["first_id"], channel_ids[1]);
    assert_eq!(seen_ids(&sandbox, "beta", "mixed"), vec![channel_ids[0]]);

    let crossed = sandbox.run_in(
        &["chat", "mixed", "--send", "--body", "reply", "--json"],
        None,
        &beta,
    );
    // A send always delivers now; what catchup left unread is what crossed it.
    assert_eq!(
        crossed.status.code(),
        Some(0),
        "stderr: {}",
        common::stderr(&crossed)
    );
    let receipt: Value = from_stdout(&crossed);
    assert_eq!(receipt["ok"], true);
    assert_eq!(receipt["crossed"]["unseen"], 2, "{receipt}");
}

fn seed_pretty_catchup_fixture(sandbox: &Sandbox) -> std::path::PathBuf {
    let (_alpha, beta) = register_alpha_beta(sandbox);
    channel_fixture(sandbox, "pretty", "beta");
    let inbox = sandbox.mail_root.join("beta/inbox");
    fs::create_dir_all(&inbox).expect("pretty inbox");
    let mail_id = "20990906-120150-abc101";
    write_custom_mail(
        &inbox,
        mail_id,
        &json!({
            "id": mail_id,
            "from": "alpha",
            "to": "beta",
            "kind": "note",
            "subject": "pretty mail",
            "sent": "2026-09-06 12:01:50 +0000"
        }),
        "mail body",
    );
    write_channel_message(
        sandbox,
        "pretty",
        "20990906-120150-000001-abc102",
        "alpha",
        "pretty channel",
        "channel body",
    );
    beta
}

/// The smallest cap that admits the complete output: the output embeds its own
/// byte limit, so iterate until the output length equals the cap it was run at.
fn stable_full_size(mut run: impl FnMut(usize) -> std::process::Output) -> usize {
    let mut cap = 100_000;
    for _ in 0..8 {
        let output = run(cap);
        assert_success(&output);
        if output.stdout.len() == cap {
            return cap;
        }
        cap = output.stdout.len();
    }
    panic!("output length never settled on its own cap");
}

#[test]
fn pretty_catchup_admits_mail_and_channel_messages_at_their_exact_full_size() {
    // Each run gets a fresh fixture because a catchup consumes what it admits.
    let run = |cap: usize| {
        let sandbox = Sandbox::new();
        let beta = seed_pretty_catchup_fixture(&sandbox);
        sandbox.run_in(
            &[
                "catchup",
                "--all",
                "--max-bytes",
                &cap.to_string(),
                "--json",
                "--pretty",
            ],
            None,
            &beta,
        )
    };
    let exact = stable_full_size(run);

    let full = run(exact);
    assert_success(&full);
    assert_eq!(full.stdout.len(), exact);
    let full: Value = from_stdout(&full);
    assert_eq!(full["count"], 2);
    assert_eq!(full["targets"][0]["count"], 1);
    assert_eq!(full["targets"][1]["count"], 1);
    assert_eq!(full["has_more"], false);

    let short = run(exact - 1);
    assert_success(&short);
    assert!(short.stdout.len() < exact);
    let short: Value = from_stdout(&short);
    assert!(short["count"].as_u64().expect("count") < 2, "{short}");
    assert_eq!(short["has_more"], true);
}

#[test]
fn budget_caps_json_pretty_and_text_after_utf8_and_escape_encoding() {
    let body = "ASCII é🙂 quote=\" slash=\\ tabs=\t newline=\n control=\u{1f}".repeat(8);
    let id = "20990906-120200-000001-eeeee1";
    let seed = || {
        let sandbox = Sandbox::new();
        let (_alpha, beta) = register_alpha_beta(&sandbox);
        channel_fixture(&sandbox, "formats", "beta");
        write_channel_message(&sandbox, "formats", id, "alpha", "formats", &body);
        (sandbox, beta)
    };
    let modes: [(&str, &[&str]); 3] = [
        ("json", &["--json"]),
        ("pretty", &["--json", "--pretty"]),
        ("text", &["--framing", "compact"]),
    ];
    for (mode, flags) in modes {
        let (sandbox, beta) = seed();
        let run = |cap: usize| {
            let cap = cap.to_string();
            let mut args = vec!["chat", "formats", "--peek", "--max-bytes", cap.as_str()];
            args.extend_from_slice(flags);
            sandbox.run_in(&args, None, &beta)
        };
        let exact = stable_full_size(&run);
        let full = run(exact);
        assert_eq!(full.stdout.len(), exact, "{mode}");
        let short = run(exact - 1);
        assert_success(&short);
        if mode == "text" {
            let full = common::stdout(&full);
            assert!(full.contains("ASCII é🙂 quote=\" slash=\\ tabs="), "{full}");
            let short = common::stdout(&short);
            assert!(!short.contains("ASCII é🙂"), "{mode}: {short}");
        } else {
            let value: Value = from_stdout(&full);
            assert_eq!(value["count"], 1, "{mode}");
            assert_eq!(value["messages"][0]["body"], body, "{mode}");
            let short: Value = from_stdout(&short);
            assert_eq!(short["count"], 0, "{mode}: {short}");
            assert_eq!(short["omitted"]["first_id"], id, "{mode}");
        }
    }
}

#[test]
fn auto_text_peeks_and_consuming_reads_stay_quiet_and_budgeted_consumption_marks_seen() {
    let sandbox = Sandbox::new();
    let (_alpha, beta) = register_alpha_beta(&sandbox);
    channel_fixture(&sandbox, "banner-budget", "beta");
    write_channel_message(
        &sandbox,
        "banner-budget",
        "20990906-120250-000001-eeee21",
        "alpha",
        "banner",
        "body",
    );
    let first_peek = sandbox.run_in(
        &["chat", "banner-budget", "--peek", "--max-bytes", "3000"],
        None,
        &beta,
    );
    assert_success(&first_peek);
    assert!(!common::stdout(&first_peek).contains("READ THIS FRAMING FIRST"));

    let ordinary_peek = sandbox.run_in(&["chat", "banner-budget", "--peek"], None, &beta);
    assert_success(&ordinary_peek);
    assert!(!common::stdout(&ordinary_peek).contains("READ THIS FRAMING FIRST"));
    assert!(seen_ids(&sandbox, "beta", "banner-budget").is_empty());

    let consuming = Sandbox::new();
    let (_alpha, beta) = register_alpha_beta(&consuming);
    channel_fixture(&consuming, "banner-consume", "beta");
    let id = "20990906-120250-000001-eeee22";
    write_channel_message(&consuming, "banner-consume", id, "alpha", "banner", "body");
    let read = consuming.run_in(
        &["chat", "banner-consume", "--max-bytes", "3000"],
        None,
        &beta,
    );
    assert_success(&read);
    assert!(!common::stdout(&read).contains("READ THIS FRAMING FIRST"));
    assert_eq!(seen_ids(&consuming, "beta", "banner-consume"), vec![id]);
}

#[test]
fn fenced_auto_text_stays_quiet_and_writes_nothing() {
    let sandbox = Sandbox::new_unseeded();
    seed_fence_store(&sandbox, r#"{"state":"fenced","generation":7}"#);
    seed_channel_fixture(&sandbox);
    let before = common::tree_snapshot(&sandbox.mail_root);

    let cwd = sandbox.home.join("dest");
    let ordinary = sandbox.run_in(&["chat", "tax", "--peek"], None, &cwd);
    assert_success(&ordinary);
    assert!(common::stdout(&ordinary).contains("fixture"));
    assert!(!common::stdout(&ordinary).contains("READ THIS FRAMING FIRST"));
    let budgeted = sandbox.run_in(
        &["chat", "tax", "--peek", "--max-bytes", "3000"],
        None,
        &cwd,
    );
    assert_success(&budgeted);
    assert!(common::stdout(&budgeted).contains("fixture"));
    assert!(!common::stdout(&budgeted).contains("READ THIS FRAMING FIRST"));
    assert_eq!(common::tree_snapshot(&sandbox.mail_root), before);
}

#[test]
fn too_small_scaffolds_fail_on_stderr_without_stdout_or_consumption() {
    let sandbox = Sandbox::new();
    let (_alpha, beta) = register_alpha_beta(&sandbox);
    channel_fixture(&sandbox, "minimum", "beta");
    let channel_id = "20990906-120300-000001-fffff1";
    write_channel_message(
        &sandbox,
        "minimum",
        channel_id,
        "alpha",
        "large",
        &"x".repeat(2_000),
    );
    let inbox = sandbox.mail_root.join("beta/inbox");
    fs::create_dir_all(&inbox).expect("inbox");
    let mail_id = "20990906-120300-fffff2";
    write_custom_mail(
        &inbox,
        mail_id,
        &json!({
            "id": mail_id,
            "from": "alpha",
            "to": "beta",
            "kind": "note",
            "subject": "large",
            "sent": "2026-09-06 12:03:00 +0000"
        }),
        &"y".repeat(2_000),
    );

    for args in [
        vec!["chat", "minimum", "--max-bytes", "1", "--json"],
        vec![
            "read",
            mail_id,
            "--room",
            "beta",
            "--max-bytes",
            "1",
            "--json",
        ],
        vec!["catchup", "--all", "--max-bytes", "1", "--json"],
    ] {
        let output = sandbox.run_in(&args, None, &beta);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let error: Value = common::from_stderr(&output);
        assert_eq!(error["error"]["code"], "invalid_argument");
        assert!(error["error"]["message"]
            .as_str()
            .expect("message")
            .contains("requires at least"));
    }
    assert!(seen_ids(&sandbox, "beta", "minimum").is_empty());
    assert!(inbox.join(format!("{mail_id}.mail")).exists());
}

#[test]
fn budgeted_peek_keeps_mention_rescue_order_and_reports_omitted_mentions() {
    let sandbox = Sandbox::new();
    let (_alpha, beta) = register_alpha_beta(&sandbox);
    channel_fixture(&sandbox, "mentions", "beta");
    let mut ids = Vec::new();
    for index in 1..=17 {
        let id = format!("20990906-120400-{index:06}-{:06x}", index);
        if index <= 9 {
            write_mentioned_channel_message(
                &sandbox,
                "mentions",
                &id,
                &format!("@beta {}", "m".repeat(1_500)),
                "beta",
            );
        } else {
            write_channel_message(
                &sandbox,
                "mentions",
                &id,
                "alpha",
                "newest",
                &"n".repeat(300),
            );
        }
        ids.push(id);
    }
    let output = sandbox.run_in(
        &[
            "chat",
            "mentions",
            "--peek",
            "--limit",
            "8",
            "--max-bytes",
            "1500",
            "--json",
        ],
        None,
        &beta,
    );
    assert_success(&output);
    assert!(output.stdout.len() <= 1_500);
    let value: Value = from_stdout(&output);
    assert_eq!(value["selected_count"], 17);
    assert_eq!(value["omitted"]["first_id"], ids[0]);
    assert_eq!(value["omitted"]["mention_count"], 9);
    assert_eq!(value["skipped"], Value::Null);
    let continuation = value["omitted"]["continuation"]
        .as_str()
        .expect("continuation command");
    let slice = sandbox.run_fix(continuation, &beta);
    assert_success(&slice);
    let slice: Value = from_stdout(&slice);
    assert!(!slice["body_slice"].as_str().expect("body slice").is_empty());
    assert!(seen_ids(&sandbox, "beta", "mentions").is_empty());
}

#[test]
fn omission_continuations_measure_large_slice_scaffolds_for_chat_read_and_catchup() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    channel_fixture(&sandbox, "metadata", "beta");
    let channel_id = "20990906-120425-000001-abcd01";
    let mentions: Vec<String> = (0..240).map(|index| format!("room-{index:03}")).collect();
    let body = format!("{}é🙂\"\\\n\u{1f}tail", "a".repeat(500));
    let message = json!({
        "id": channel_id,
        "from": "alpha",
        "channel": "metadata",
        "subject": "s".repeat(1024),
        "sent": "2026-09-06 12:04:25 +0000",
        "mentions": mentions,
    });
    fs::write(
        sandbox
            .mail_root
            .join(format!("channels/metadata/messages/{channel_id}.msg")),
        format!(
            "{}\n---\n{body}",
            serde_json::to_string_pretty(&message).expect("metadata message")
        ),
    )
    .expect("write metadata message");

    let chat = sandbox.run_in(
        &[
            "chat",
            "metadata",
            "--peek",
            "--max-bytes",
            "1800",
            "--json",
        ],
        None,
        &beta,
    );
    assert_success(&chat);
    let chat: Value = from_stdout(&chat);
    let chat_command = chat["omitted"]["continuation"]
        .as_str()
        .expect("chat continuation");
    assert!(continuation_budget(chat_command) > 4_096);
    let (chat_body, chat_ranges) = follow_continuation_chain(&sandbox, &beta, chat_command);
    assert_eq!(chat_body.as_bytes(), body.as_bytes());
    assert!(chat_ranges
        .iter()
        .any(|(start, end)| *start < 10 && *end >= 10));
    assert!(chat_ranges
        .iter()
        .any(|(start, end)| *start < 100 && *end >= 100));
    assert!(chat_ranges.len() > 1);

    let catchup = sandbox.run_in(
        &["catchup", "metadata", "--max-bytes", "1800", "--json"],
        None,
        &beta,
    );
    assert_success(&catchup);
    let catchup: Value = from_stdout(&catchup);
    let catchup_command = catchup["omitted"]["continuation"]
        .as_str()
        .expect("catchup continuation");
    assert!(continuation_budget(catchup_command) > 4_096);
    let (catchup_body, catchup_ranges) =
        follow_continuation_chain(&sandbox, &beta, catchup_command);
    assert_eq!(catchup_body.as_bytes(), body.as_bytes());
    assert!(catchup_ranges.len() > 1);
    assert!(seen_ids(&sandbox, "beta", "metadata").is_empty());

    let inbox = sandbox.mail_root.join("beta/inbox");
    let beta_participant = sandbox.test_participant("beta");
    let alpha_participant = sandbox.test_participant("alpha");
    let beta_record = sandbox
        .mail_root
        .join("participants")
        .join(&beta_participant)
        .join("participant.json");
    let beta_json: Value =
        serde_json::from_slice(&fs::read(&beta_record).expect("beta participant"))
            .expect("beta participant JSON");
    assert!(beta_json["last_seen"].is_string());
    assert!(beta_json["lease_hours"]
        .as_u64()
        .is_some_and(|hours| hours > 0));
    let expected_mail = format!("{}é🙂\"\\\n\u{1f}mail-tail", "m".repeat(8_000));
    let sent = sandbox.run_as_participant(
        &[
            "send",
            "--to",
            "workspace:beta",
            "--subject",
            &"m".repeat(1024),
            "--body",
            &expected_mail,
            "--json",
        ],
        &alpha_participant,
        &alpha,
    );
    assert_success(&sent);
    let sent: Value = from_stdout(&sent);
    let mail_id = sent["envelope"]["id"].as_str().expect("mail id");
    let receipt_path = sandbox
        .mail_root
        .join(format!("beta/routing/{mail_id}.json"));
    assert!(
        inbox.join(format!("{mail_id}.mail")).is_file(),
        "canonical mail missing"
    );
    let receipt: Value = serde_json::from_slice(&fs::read(&receipt_path).expect("routing receipt"))
        .expect("routing receipt JSON");
    assert!(
        receipt["recipients"]
            .as_array()
            .expect("recipients")
            .iter()
            .any(|recipient| recipient == &beta_participant),
        "beta recipient missing: {receipt}"
    );
    let read = sandbox.run_as_participant(
        &[
            "read",
            mail_id,
            "--room",
            "beta",
            "--max-bytes",
            "3000",
            "--json",
        ],
        &beta_participant,
        &beta,
    );
    assert_success(&read);
    let read: Value = from_stdout(&read);
    let read_command = read["omitted"]["continuation"]
        .as_str()
        .expect("read continuation");
    let (mail_body, mail_ranges) = follow_continuation_chain(&sandbox, &beta, read_command);
    assert_eq!(mail_body.as_bytes(), expected_mail.as_bytes());
    assert!(mail_ranges
        .iter()
        .any(|(start, end)| *start < 10 && *end >= 10));
    assert!(mail_ranges
        .iter()
        .any(|(start, end)| *start < 100 && *end >= 100));
    assert!(mail_ranges.len() > 1);
    assert!(seen_ids(&sandbox, "beta", "metadata").is_empty());
    assert!(inbox.join(format!("{mail_id}.mail")).exists());

    assert_success(&sandbox.run_in(
        &["chat", "metadata", "--ack", channel_id, "--json"],
        None,
        &beta,
    ));
    assert_eq!(seen_ids(&sandbox, "beta", "metadata"), vec![channel_id]);
    assert_success(&sandbox.run_as_participant(
        &["read", mail_id, "--room", "beta", "--ack", "--json"],
        &beta_participant,
        &beta,
    ));
    assert!(inbox.join(format!("{mail_id}.mail")).exists());
}

#[test]
fn count_window_and_byte_omissions_remain_distinct_in_one_result() {
    let sandbox = Sandbox::new();
    let (_alpha, beta) = register_alpha_beta(&sandbox);
    channel_fixture(&sandbox, "two-remainders", "beta");
    for index in 0..30 {
        let id = format!("20990906-120450-{index:06}-{index:06x}");
        write_channel_message(
            &sandbox,
            "two-remainders",
            &id,
            "alpha",
            "bounded",
            &"x".repeat(300),
        );
    }
    let output = sandbox.run_in(
        &[
            "chat",
            "two-remainders",
            "--peek",
            "--limit",
            "25",
            "--max-bytes",
            "2500",
            "--json",
        ],
        None,
        &beta,
    );
    assert_success(&output);
    let value: Value = from_stdout(&output);
    let emitted = value["count"].as_u64().expect("emitted count") as usize;
    let byte_omitted = value["omitted"]["count"].as_u64().expect("byte omission") as usize;
    assert_eq!(value["skipped"], 5);
    assert_eq!(value["selected_count"], 25);
    assert!(emitted > 0);
    assert!(byte_omitted > 0);
    assert_eq!(emitted + byte_omitted, 25);
    assert_ne!(byte_omitted, 5, "byte omission must not alias skipped");
    assert!(seen_ids(&sandbox, "beta", "two-remainders").is_empty());
}

#[test]
fn malformed_selected_messages_are_reported_not_hidden_by_a_budget() {
    let sandbox = Sandbox::new();
    let (_alpha, beta) = register_alpha_beta(&sandbox);
    channel_fixture(&sandbox, "malformed", "beta");
    let good_id = "20990906-120500-000001-111111";
    let bad_id = "20990906-120500-000002-222222";
    write_channel_message(&sandbox, "malformed", good_id, "alpha", "good", "small");
    fs::write(
        sandbox
            .mail_root
            .join(format!("channels/malformed/messages/{bad_id}.msg")),
        "malformed",
    )
    .expect("malformed message");

    let consuming = sandbox.run_in(
        &["chat", "malformed", "--max-bytes", "1200", "--json"],
        None,
        &beta,
    );
    // The unreadable file used to fail the whole read (exit 78). Consumption is
    // per emitted id, so skipping it cannot move a cursor past it: the good
    // message is emitted and consumed, the bad one is reported and stays unseen.
    assert_eq!(
        consuming.status.code(),
        Some(0),
        "stderr: {}",
        common::stderr(&consuming)
    );
    let consuming_json: Value = from_stdout(&consuming);
    assert_eq!(consuming_json["messages"][0]["id"], good_id);
    assert_eq!(consuming_json["skipped_files"][0]["id"], bad_id);
    assert_eq!(seen_ids(&sandbox, "beta", "malformed"), vec![good_id]);

    let history = sandbox.run_in(
        &[
            "chat",
            "malformed",
            "--history",
            "2",
            "--max-bytes",
            "1200",
            "--json",
        ],
        None,
        &beta,
    );
    assert_eq!(history.status.code(), Some(0));
    let history_json: Value = from_stdout(&history);
    assert_eq!(history_json["selected_count"], 1);
    assert_eq!(history_json["messages"][0]["id"], good_id);
    assert_eq!(history_json["skipped_files"][0]["id"], bad_id);
}

#[test]
fn no_flag_preserves_legacy_json_shape() {
    let sandbox = Sandbox::new();
    let (_alpha, beta) = register_alpha_beta(&sandbox);
    channel_fixture(&sandbox, "legacy", "beta");
    write_channel_message(
        &sandbox,
        "legacy",
        "20990906-120600-000001-333333",
        "alpha",
        "legacy",
        "body",
    );
    let output = sandbox.run_in(&["chat", "legacy", "--peek", "--json"], None, &beta);
    assert_success(&output);
    let value: Value = from_stdout(&output);
    let object = value.as_object().expect("chat object");
    assert!(!object.contains_key("selected_count"));
    assert!(!object.contains_key("byte_limit"));
    assert!(!object.contains_key("omitted"));

    let empty_catchup = sandbox.run_in(&["catchup", "--mail", "--json"], None, &beta);
    assert_success(&empty_catchup);
    let value: Value = from_stdout(&empty_catchup);
    let object = value.as_object().expect("catchup object");
    assert_eq!(
        object.keys().cloned().collect::<Vec<_>>(),
        ["count", "ok", "room", "targets"]
    );
}

#[test]
fn complete_budgeted_direct_read_consumes_after_full_body_output() {
    let sandbox = Sandbox::new();
    let (_alpha, beta) = register_alpha_beta(&sandbox);
    let inbox = sandbox.mail_root.join("beta/inbox");
    fs::create_dir_all(&inbox).expect("inbox");
    let id = "20990906-120650-333334";
    write_custom_mail(
        &inbox,
        id,
        &json!({
            "id": id,
            "from": "alpha",
            "to": "beta",
            "kind": "note",
            "subject": "small",
            "sent": "2026-09-06 12:06:50 +0000"
        }),
        "complete",
    );
    let output = sandbox.run_in(
        &[
            "read",
            id,
            "--room",
            "beta",
            "--max-bytes",
            "2000",
            "--json",
        ],
        None,
        &beta,
    );
    assert_success(&output);
    assert!(output.stdout.len() <= 2_000);
    let value: Value = from_stdout(&output);
    assert_eq!(value["count"], 1);
    assert_eq!(value["body"], "complete");
    assert_eq!(mail_seen_ids(&sandbox, "beta"), vec![id]);
}

#[test]
fn slice_overflow_eof_and_mode_conflicts_are_explicit() {
    let sandbox = Sandbox::new();
    let (_alpha, beta) = register_alpha_beta(&sandbox);
    channel_fixture(&sandbox, "args", "beta");
    let id = "20990906-120700-000001-444444";
    write_channel_message(&sandbox, "args", id, "alpha", "args", "é");
    let total = "é".len().to_string();
    let eof = sandbox.run_in(
        &[
            "chat",
            "args",
            "--message",
            id,
            "--offset",
            &total,
            "--max-bytes",
            "1400",
            "--json",
        ],
        None,
        &beta,
    );
    assert_success(&eof);
    let eof: Value = from_stdout(&eof);
    assert_eq!(eof["body_slice"], "");
    assert!(eof["next_offset"].is_null());

    let overflow = sandbox.run_in(
        &[
            "chat",
            "args",
            "--message",
            id,
            "--offset",
            "2",
            "--length",
            &usize::MAX.to_string(),
            "--max-bytes",
            "1400",
            "--json",
        ],
        None,
        &beta,
    );
    assert_eq!(overflow.status.code(), Some(2));
    assert!(common::stderr(&overflow).contains("overflows"));

    let invalid_cases: Vec<Vec<&str>> = vec![
        vec!["chat", "args", "--message", id],
        vec![
            "chat",
            "args",
            "--message",
            id,
            "--max-bytes",
            "1400",
            "--peek",
        ],
        vec!["chat", "args", "--ack", id, "--max-bytes", "1400"],
        vec!["read", "id", "--offset", "0"],
        vec!["read", "id", "--ack", "--peek"],
        vec!["catchup", "--max-bytes", "0"],
    ];
    for args in invalid_cases {
        let output = sandbox.run_in(&args, None, &beta);
        assert_eq!(output.status.code(), Some(2), "args: {args:?}");
        assert!(output.stdout.is_empty());
    }
}

#[cfg(unix)]
#[test]
fn failed_stdout_applies_no_budgeted_read_or_exact_ack_delta() {
    let sandbox = Sandbox::new();
    let (_alpha, beta) = register_alpha_beta(&sandbox);
    channel_fixture(&sandbox, "flush", "beta");
    let channel_id = "20990906-120800-000001-555555";
    write_channel_message(
        &sandbox,
        "flush",
        channel_id,
        "alpha",
        "flush",
        "small body",
    );
    let failed_text =
        sandbox.run_in_broken_stdout(&["chat", "flush", "--max-bytes", "2000"], &beta);
    assert_stdout_io_failure(&failed_text);
    assert!(seen_ids(&sandbox, "beta", "flush").is_empty());
    let failed_read =
        sandbox.run_in_broken_stdout(&["chat", "flush", "--max-bytes", "2000", "--json"], &beta);
    assert_stdout_io_failure(&failed_read);
    assert!(seen_ids(&sandbox, "beta", "flush").is_empty());

    let failed_catchup = sandbox.run_in_broken_stdout(
        &["catchup", "--all", "--max-bytes", "2000", "--json"],
        &beta,
    );
    assert_stdout_io_failure(&failed_catchup);
    assert!(seen_ids(&sandbox, "beta", "flush").is_empty());

    let failed_ack =
        sandbox.run_in_broken_stdout(&["chat", "flush", "--ack", channel_id, "--json"], &beta);
    assert_stdout_io_failure(&failed_ack);
    assert!(seen_ids(&sandbox, "beta", "flush").is_empty());

    let inbox = sandbox.mail_root.join("beta/inbox");
    fs::create_dir_all(&inbox).expect("inbox");
    let mail_id = "20990906-120800-555556";
    write_custom_mail(
        &inbox,
        mail_id,
        &json!({
            "id": mail_id,
            "from": "alpha",
            "to": "beta",
            "kind": "note",
            "subject": "flush",
            "sent": "2026-09-06 12:08:00 +0000"
        }),
        "mail body",
    );
    let failed_mail_read = sandbox.run_in_broken_stdout(
        &[
            "read",
            mail_id,
            "--room",
            "beta",
            "--max-bytes",
            "2000",
            "--json",
        ],
        &beta,
    );
    assert_stdout_io_failure(&failed_mail_read);
    assert!(mail_seen_ids(&sandbox, "beta").is_empty());
    let failed_mail_ack = sandbox.run_in_broken_stdout(
        &["read", mail_id, "--room", "beta", "--ack", "--json"],
        &beta,
    );
    assert_stdout_io_failure(&failed_mail_ack);
    assert!(mail_seen_ids(&sandbox, "beta").is_empty());
    assert_eq!(
        fs::read(sandbox.read_only_stdout_path()).expect("stdout sentinel"),
        Sandbox::READ_ONLY_STDOUT_SENTINEL
    );
}

#[cfg(unix)]
#[test]
fn strict_stdout_preserves_committed_delivery_and_registration_rules() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let delivered = sandbox.run_in_broken_stdout(
        &["send", "--to", "beta", "--body", "committed delivery"],
        &alpha,
    );
    // A send that landed never exits nonzero, even when its receipt cannot be
    // written: a nonzero exit made callers resend, and duplicates landed (the
    // 2026-09-28 double-send). Registration-style success plus a stderr note.
    assert_eq!(delivered.status.code(), Some(0));
    assert!(
        common::stderr(&delivered).contains("committed"),
        "the lost receipt is noted on stderr: {}",
        common::stderr(&delivered)
    );
    assert_eq!(
        fs::read_dir(sandbox.mail_root.join("beta/inbox"))
            .expect("delivered inbox")
            .count(),
        1
    );

    let gamma = sandbox.path.join("gamma");
    fs::create_dir(&gamma).expect("gamma workspace");
    let registered = sandbox.run_in_broken_stdout(
        &["rooms", "add", "gamma", gamma.to_str().expect("gamma path")],
        &sandbox.path,
    );
    assert_eq!(registered.status.code(), Some(0));
    let rooms: Value = from_stdout(&sandbox.run(&["rooms"]));
    assert!(rooms["rooms"]
        .as_array()
        .expect("rooms")
        .iter()
        .any(|room| room["name"] == "gamma"));
}
