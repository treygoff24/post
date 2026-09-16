mod common;

use common::{
    assert_success, from_stdout, register_alpha_beta, write_bad_channel, write_channel_message,
    write_custom_mail, Sandbox,
};
use post::output::{DoctorOutput, DoctorSeverity, SchemaOutput};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

fn json_object(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "expected JSON output: {error}\nstdout: {}\nstderr: {}",
            common::stdout(output),
            common::stderr(output)
        )
    })
}

fn keys(value: &Value) -> BTreeSet<String> {
    value
        .as_object()
        .expect("JSON object")
        .keys()
        .cloned()
        .collect()
}

fn assert_keys_in_shape(shape: &[String], expected: &[&str]) {
    let shape = shape.join("\n");
    for field in expected {
        assert!(
            shape.contains(field),
            "schema shape omitted field {field:?}: {shape}"
        );
    }
}

fn assert_keys_are_documented(actual: &BTreeSet<String>, shape: &[String]) {
    let shape = shape.join("\n");
    for field in actual {
        assert!(
            shape.contains(field),
            "real output field {field:?} is absent from schema shape: {shape}"
        );
    }
}

#[test]
fn participant_identity_adopt_and_version_schema_surface_is_complete() {
    let sandbox = Sandbox::new();
    let schema: SchemaOutput = from_stdout(&sandbox.run(&["schema"]));
    for (name, required) in [
        (
            "participant",
            vec![
                "show",
                "bind",
                "--workspace",
                "--harness",
                "--key",
                "--new",
                "touch",
                "end",
                "list",
            ],
        ),
        (
            "identity",
            vec![
                "list",
                "show <name>",
                "--voices",
                "new <name>",
                "continue <name>",
                "--acknowledge",
                "voice add",
                "voice withdraw",
                "terms set",
                "--body-file",
            ],
        ),
        ("version", vec!["--json"]),
    ] {
        let command = schema
            .commands
            .iter()
            .find(|command| command.name == name)
            .unwrap_or_else(|| panic!("missing {name} schema command"));
        for token in required {
            assert!(
                command.usage.contains(token),
                "{name} omitted {token}: {}",
                command.usage
            );
        }
    }
    let inbox = schema
        .commands
        .iter()
        .find(|command| command.name == "inbox")
        .expect("inbox schema command");
    assert!(inbox.usage.contains("--adopt"));
    assert!(!inbox.side_effects.contains("P.2"));
    assert!(!inbox.side_effects.contains("not_yet"));
    let identity = schema
        .commands
        .iter()
        .find(|command| command.name == "identity")
        .expect("identity schema command");
    assert!(!identity.side_effects.contains("P.3"));
    assert!(!identity.side_effects.contains("not_yet"));
    assert!(!schema.output_shapes.identity.join("\n").contains("not_yet"));
    assert_eq!(schema.store_version, 2);
    assert_eq!(
        schema.capabilities,
        vec!["participants", "lineages", "routing-receipts", "cursors-v2"]
    );
    let watch = schema
        .commands
        .iter()
        .find(|command| command.name == "watch")
        .expect("watch schema command");
    assert!(!watch.side_effects.contains("creates missing mailbox"));
    assert!(watch.side_effects.contains("empty scan emits nothing"));

    for args in [
        &["participant", "--help"] as &[&str],
        &["participant", "bind", "--help"],
        &["participant", "touch", "--help"],
        &["participant", "end", "--help"],
        &["identity", "--help"],
        &["identity", "show", "--help"],
        &["identity", "continue", "--help"],
        &["identity", "voice", "add", "--help"],
        &["identity", "terms", "set", "--help"],
        &["inbox", "--help"],
        &["version", "--help"],
    ] {
        assert_success(&sandbox.run(args));
    }
    let participant_help = common::stdout(&sandbox.run(&["participant", "--help"]));
    assert!(!participant_help
        .lines()
        .any(|line| line.trim_start().starts_with("new ")));
}

fn option_names(text: &str) -> BTreeSet<String> {
    text.split_whitespace()
        .filter_map(|token| {
            let token = token.get(token.find("--")?..)?;
            let name = token
                .split(|character: char| {
                    matches!(character, '<' | '>' | '|' | ']' | ')' | ',' | '=')
                })
                .next()?;
            (!name.is_empty()).then(|| name.to_owned())
        })
        .collect()
}

fn declared_option_names(help_options: &str) -> BTreeSet<String> {
    help_options
        .lines()
        .filter_map(|line| {
            let line = line.trim_start();
            line.starts_with("--")
                .then(|| line.split_whitespace().next())
                .flatten()
                .map(|token| token.trim_end_matches(',').to_owned())
        })
        .collect()
}

#[test]
fn schema_matches_catchup_and_search_help_and_json() {
    let sandbox = Sandbox::new();
    let (_alpha, beta) = register_alpha_beta(&sandbox);
    write_bad_channel(
        &sandbox,
        "tax",
        Some(r#"{"alpha":"joined","beta":"joined"}"#),
        true,
        r#"{"name":"tax","created":"2026-08-20 12:00:00 +0000","created_by":"alpha"}"#,
    );
    let mail_id = "20260820-120000-aaaaaa";
    let inbox = sandbox.mail_root.join("beta/inbox");
    fs::create_dir_all(&inbox).expect("inbox");
    write_custom_mail(
        &inbox,
        mail_id,
        &serde_json::json!({
            "id": mail_id,
            "from": "alpha",
            "to": "beta",
            "kind": "note",
            "subject": "schema mail",
            "sent": "2026-08-20 12:00:00 +0000"
        }),
        "schema surface marker",
    );
    let channel_id = "20260820-120000-000001-aaaaaa";
    fs::write(
        sandbox
            .mail_root
            .join(format!("channels/tax/messages/{channel_id}.msg")),
        format!(
            "{{\"id\":\"{channel_id}\",\"from\":\"alpha\",\"channel\":\"tax\",\"subject\":\"schema channel\",\"sent\":\"2026-08-20 12:00:00 +0000\"}}\n---\nschema surface marker"
        ),
    )
    .expect("channel message");
    write_bad_channel(
        &sandbox,
        "private",
        Some(r#"{"alpha":"joined"}"#),
        true,
        r#"{"name":"private","created":"2026-08-20 12:00:00 +0000","created_by":"alpha"}"#,
    );

    let schema: SchemaOutput = from_stdout(&sandbox.run(&["schema"]));
    let catchup = schema
        .commands
        .iter()
        .find(|command| command.name == "catchup")
        .expect("catchup command in schema");
    let search = schema
        .commands
        .iter()
        .find(|command| command.name == "search")
        .expect("search command in schema");
    for token in [
        "<channel>",
        "--mail",
        "--all",
        "--framing auto|full|compact",
    ] {
        assert!(
            catchup.usage.contains(token),
            "catchup usage omitted {token}"
        );
    }
    for token in [
        "<pattern>",
        "--mail",
        "--channel <channel>",
        "--limit <1..=1000>",
        "--framing auto|full|compact",
    ] {
        assert!(search.usage.contains(token), "search usage omitted {token}");
    }
    for (usage, args, tokens) in [
        (
            &catchup.usage,
            vec!["catchup", "--all", "--help"],
            vec!["--mail", "--all", "--framing"],
        ),
        (
            &search.usage,
            vec!["search", "marker", "--help"],
            vec!["--mail", "--channel", "--limit", "--framing"],
        ),
    ] {
        let help = sandbox.run(&args);
        assert_success(&help);
        let text = common::stdout(&help);
        for token in tokens {
            assert!(text.contains(token), "help omitted {token}: {text}");
        }
        let mut help_options = option_names(&text);
        for global in ["--json", "--pretty", "--help"] {
            help_options.remove(global);
        }
        assert_eq!(
            help_options,
            option_names(usage),
            "schema usage and clap help disagree about command options"
        );
    }

    let catchup_output = sandbox.run_in(&["catchup", "--all", "--json"], None, &beta);
    assert_success(&catchup_output);
    let catchup_json = json_object(&catchup_output);
    let catchup_top = ["ok", "room", "targets", "count"];
    assert_keys_in_shape(&schema.output_shapes.catchup, &catchup_top);
    let catchup_top_keys = keys(&catchup_json);
    assert_keys_are_documented(&catchup_top_keys, &schema.output_shapes.catchup);
    assert_eq!(
        catchup_top_keys,
        catchup_top.iter().map(|key| (*key).to_owned()).collect()
    );
    let targets = catchup_json["targets"].as_array().expect("catchup targets");
    assert!(targets.iter().any(|target| target["source"] == "mail"));
    assert!(targets.iter().any(|target| target["source"] == "channel"));
    let target_fields = ["source", "framing", "messages", "count"];
    assert_keys_in_shape(&schema.output_shapes.catchup, &target_fields);
    for target in targets {
        let actual = keys(target);
        assert_keys_are_documented(&actual, &schema.output_shapes.catchup);
        assert!(actual.contains("source"));
        assert!(actual.contains("framing"));
        assert!(actual.contains("messages"));
        assert!(actual.contains("count"));
        let framing_keys = keys(&target["framing"]);
        assert_eq!(
            framing_keys,
            ["source", "authority", "laws"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
        let messages = target["messages"].as_array().expect("target messages");
        for message in messages {
            let message_keys = keys(message);
            if target["source"] == "mail" {
                assert!(message_keys.contains("envelope"));
                assert!(message_keys.contains("body"));
            } else {
                for field in ["id", "from", "channel", "subject", "sent", "body"] {
                    assert!(
                        message_keys.contains(field),
                        "channel catchup message omitted {field}: {message}"
                    );
                }
            }
        }
        if target["source"] == "channel" {
            assert!(actual.contains("channel"));
        }
    }

    write_channel_message(
        &sandbox,
        "tax",
        "20260820-120001-000001-bbbbbb",
        "alpha",
        "searchable after catchup",
        "schema surface marker",
    );
    let search_output = sandbox.run_in(&["search", "schema surface marker", "--json"], None, &beta);
    assert_success(&search_output);
    let search_json = json_object(&search_output);
    let search_top = [
        "ok",
        "framing",
        "room",
        "pattern",
        "match",
        "results",
        "count",
        "limit",
        "truncated",
        "participant",
        "pending",
    ];
    assert_keys_in_shape(&schema.output_shapes.search, &search_top);
    assert_keys_in_shape(
        &schema.output_shapes.search,
        &["source", "authority", "laws"],
    );
    let search_top_keys = keys(&search_json);
    assert_keys_are_documented(&search_top_keys, &schema.output_shapes.search);
    assert_eq!(
        search_top_keys,
        search_top.iter().map(|key| (*key).to_owned()).collect()
    );
    let results = search_json["results"].as_array().expect("search results");
    assert!(!results.is_empty());
    let framing_keys = keys(&search_json["framing"]);
    assert_eq!(
        framing_keys,
        ["source", "authority", "laws"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    );
    let result_fields = [
        "source", "channel", "id", "from", "sent", "subject", "preview", "matched",
    ];
    assert_keys_in_shape(&schema.output_shapes.search, &result_fields);
    assert_keys_in_shape(&schema.output_shapes.search, &["kind"]);
    for result in results {
        let actual = keys(result);
        assert_keys_are_documented(&actual, &schema.output_shapes.search);
        for field in result_fields {
            assert!(
                actual.contains(field),
                "search result omitted {field}: {result}"
            );
        }
        if result["source"] == "mail" {
            assert!(
                actual.contains("kind"),
                "mail result omitted kind: {result}"
            );
        } else {
            assert!(
                !actual.contains("kind"),
                "channel result unexpectedly has kind: {result}"
            );
        }
    }

    let channels_output = sandbox.run_in(&["channels"], None, &beta);
    assert_success(&channels_output);
    let channels_json = json_object(&channels_output);
    assert_keys_in_shape(&schema.output_shapes.channels, &["room", "unread"]);
    for channel in channels_json["channels"].as_array().expect("channels") {
        let channel_keys = keys(channel);
        assert!(channel_keys.contains("room"));
        assert!(channel_keys.contains("unread"));
    }
    let private = channels_json["channels"]
        .as_array()
        .expect("channels")
        .iter()
        .find(|channel| channel["name"] == "private")
        .expect("private channel");
    assert_eq!(private["room"], "beta");
    assert!(private["unread"].is_null());

    let inbox_output = sandbox.run_in(&["inbox", "--room", "beta"], None, &beta);
    assert_success(&inbox_output);
    let inbox_json = json_object(&inbox_output);
    assert_keys_in_shape(&schema.output_shapes.inbox, &["unread_count"]);
    assert!(keys(&inbox_json).contains("unread_count"));
}

#[test]
fn schema_matches_budget_slice_and_exact_ack_surfaces() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    write_bad_channel(
        &sandbox,
        "bounded",
        Some(r#"{"beta":"joined"}"#),
        true,
        r#"{"name":"bounded","created":"2026-09-06 12:00:00 +0000","created_by":"beta"}"#,
    );
    let channel_id = "20990906-121000-000001-666666";
    write_channel_message(
        &sandbox,
        "bounded",
        channel_id,
        "alpha",
        "schema budget",
        &"x".repeat(4_000),
    );
    let body = "y".repeat(4_000);
    let sent = sandbox.run_in(
        &[
            "send",
            "--to",
            "workspace:beta",
            "--subject",
            "schema budget",
            "--body",
            &body,
            "--json",
        ],
        None,
        &alpha,
    );
    assert_success(&sent);
    let sent = json_object(&sent);
    let mail_id = sent["envelope"]["id"].as_str().expect("mail id").to_owned();
    let schema: SchemaOutput = from_stdout(&sandbox.run(&["schema"]));
    for (command, tokens) in [
        (
            "chat",
            vec!["--max-bytes", "--message", "--offset", "--length", "--ack"],
        ),
        ("read", vec!["--max-bytes", "--offset", "--length", "--ack"]),
        ("catchup", vec!["--max-bytes"]),
    ] {
        let usage = &schema
            .commands
            .iter()
            .find(|item| item.name == command)
            .expect("command in schema")
            .usage;
        for token in tokens {
            assert!(usage.contains(token), "{command} schema omitted {token}");
        }
    }
    for command in ["chat", "read"] {
        let usage = &schema
            .commands
            .iter()
            .find(|item| item.name == command)
            .expect("command in schema")
            .usage;
        let help = sandbox.run(&[command, "--help"]);
        assert_success(&help);
        let help_text = common::stdout(&help);
        let options = help_text
            .split_once("Options:")
            .map(|(_, options)| options)
            .expect("clap options section");
        let mut help_options = declared_option_names(options);
        for global in ["--json", "--pretty", "--help"] {
            help_options.remove(global);
        }
        assert_eq!(
            help_options,
            option_names(usage),
            "{command} schema usage and clap help disagree"
        );
    }

    let chat_budget = sandbox.run_in(
        &["chat", "bounded", "--peek", "--max-bytes", "1400", "--json"],
        None,
        &beta,
    );
    assert_success(&chat_budget);
    let chat_budget = json_object(&chat_budget);
    assert_keys_are_documented(&keys(&chat_budget), &schema.output_shapes.chat_read);
    assert_keys_in_shape(
        &schema.output_shapes.chat_read,
        &["selected_count", "byte_limit", "omitted"],
    );

    let chat_slice = sandbox.run_in(
        &[
            "chat",
            "bounded",
            "--message",
            channel_id,
            "--offset",
            "0",
            "--max-bytes",
            "1400",
            "--json",
        ],
        None,
        &beta,
    );
    assert_success(&chat_slice);
    assert_keys_are_documented(
        &keys(&json_object(&chat_slice)),
        &schema.output_shapes.chat_slice,
    );

    let beta_participant = sandbox.test_participant("beta");
    let read_budget = sandbox.run_as_participant(
        &[
            "read",
            &mail_id,
            "--room",
            "beta",
            "--peek",
            "--max-bytes",
            "1200",
            "--json",
        ],
        &beta_participant,
        &beta,
    );
    assert_success(&read_budget);
    assert_keys_are_documented(
        &keys(&json_object(&read_budget)),
        &schema.output_shapes.read_budget,
    );

    let read_slice = sandbox.run_as_participant(
        &[
            "read",
            &mail_id,
            "--room",
            "beta",
            "--offset",
            "0",
            "--max-bytes",
            "1200",
            "--json",
        ],
        &beta_participant,
        &beta,
    );
    assert_success(&read_slice);
    assert_keys_are_documented(
        &keys(&json_object(&read_slice)),
        &schema.output_shapes.read_slice,
    );

    let catchup = sandbox.run_in(
        &["catchup", "--all", "--max-bytes", "1400", "--json"],
        None,
        &beta,
    );
    assert_success(&catchup);
    let catchup = json_object(&catchup);
    assert_keys_are_documented(&keys(&catchup), &schema.output_shapes.catchup);
    assert_keys_in_shape(
        &schema.output_shapes.catchup,
        &["targets[].selected_count", "targets[].has_more"],
    );
    for target in catchup["targets"].as_array().expect("catchup targets") {
        let target_keys = keys(target);
        assert!(target_keys.contains("selected_count"));
        assert!(target_keys.contains("has_more"));
    }

    let chat_ack = sandbox.run_in(
        &["chat", "bounded", "--ack", channel_id, "--json"],
        None,
        &beta,
    );
    assert_success(&chat_ack);
    assert_keys_are_documented(
        &keys(&json_object(&chat_ack)),
        &schema.output_shapes.chat_ack,
    );
    let read_ack = sandbox.run_in(
        &["read", &mail_id, "--room", "beta", "--ack", "--json"],
        None,
        &beta,
    );
    assert_success(&read_ack);
    assert_keys_are_documented(
        &keys(&json_object(&read_ack)),
        &schema.output_shapes.read_ack,
    );
}

#[test]
fn doctor_reports_cursor_state_without_repairing_it() {
    let sandbox = Sandbox::new();
    let room_dir = sandbox.mail_root.join("claude-space");
    fs::create_dir_all(room_dir.join("inbox")).expect("inbox");
    fs::create_dir_all(room_dir.join("read")).expect("read");
    let cursor = room_dir.join("cursors.json");
    fs::write(&cursor, b"{malformed").expect("malformed cursor");
    #[cfg(unix)]
    fs::set_permissions(&cursor, fs::Permissions::from_mode(0o600)).expect("cursor mode");
    let before = fs::read(&cursor).expect("cursor bytes");

    let diagnosed = sandbox.run(&["doctor"]);
    assert_eq!(diagnosed.status.code(), Some(1));
    let report: DoctorOutput = from_stdout(&diagnosed);
    let invalid = report
        .checks
        .iter()
        .find(|check| check.id == "cursor_state.claude-space.invalid")
        .expect("malformed cursor check");
    assert_eq!(invalid.severity, DoctorSeverity::Warning);
    assert!(invalid.message.contains("participant reads ignore it"));

    let fixed = sandbox.run(&["doctor", "--fix"]);
    assert_eq!(fixed.status.code(), Some(1));
    assert_eq!(fs::read(&cursor).expect("cursor after --fix"), before);
    assert!(!room_dir.join(".cursors.lock").exists());
}

#[test]
fn doctor_reports_legacy_state_as_info_and_downgrades_inert_legacy_errors() {
    let sandbox = Sandbox::new();
    let room_dir = sandbox.mail_root.join("claude-space");
    fs::create_dir_all(room_dir.join("inbox")).expect("inbox");
    fs::create_dir_all(room_dir.join("read")).expect("read");
    let legacy = room_dir.join("channel-state.json");
    fs::write(&legacy, r#"{"version":2,"channels":{"tax":{"seen":[]}}}"#).expect("legacy state");
    let diagnosed = sandbox.run(&["doctor"]);
    let report: DoctorOutput = from_stdout(&diagnosed);
    let info = report
        .checks
        .iter()
        .find(|check| check.id == "cursor_state.claude-space.legacy")
        .expect("legacy info check");
    assert_eq!(info.severity, DoctorSeverity::Info);

    fs::write(&legacy, b"not json").expect("malformed legacy state");
    let cursor = room_dir.join("cursors.json");
    fs::write(
        &cursor,
        b"{\"version\":1,\"mail\":{\"seen\":[]},\"channels\":{}}\n",
    )
    .expect("valid cursor state");
    #[cfg(unix)]
    {
        fs::set_permissions(&cursor, fs::Permissions::from_mode(0o600)).expect("cursor mode");
        let lock = room_dir.join(".cursors.lock");
        let file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&lock)
            .expect("cursor lock");
        drop(file);
    }
    let diagnosed = sandbox.run(&["doctor"]);
    let report: DoctorOutput = from_stdout(&diagnosed);
    let legacy_invalid = report
        .checks
        .iter()
        .find(|check| check.id == "channel_state.claude-space.invalid")
        .expect("inert legacy check");
    assert_eq!(legacy_invalid.severity, DoctorSeverity::Warning);
}
