mod common;

use common::{
    assert_documented, assert_success, from_stdout, help_options, register_alpha_beta,
    usage_options, write_bad_channel, write_channel_message, write_custom_mail, Sandbox,
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

fn shape_top_level_keys(shape: &[String]) -> BTreeSet<String> {
    shape
        .iter()
        .map(|field| {
            field
                .split(|character: char| {
                    character.is_whitespace() || matches!(character, '(' | '[' | '{' | '=' | '?')
                })
                .next()
                .expect("shape field name")
                .to_owned()
        })
        .collect()
}

fn expected_keys(fields: &[&str]) -> BTreeSet<String> {
    fields.iter().map(|field| (*field).to_owned()).collect()
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
                "describe",
                "--clear",
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
                "voice withdraw [--lineage <name>]",
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
        vec![
            "participants",
            "lineages",
            "routing-receipts",
            "cursors-v2",
            "avatars-v1",
            "emotes-v1"
        ]
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
        &["participant", "describe", "--help"],
        &["participant", "end", "--help"],
        &["identity", "--help"],
        &["identity", "show", "--help"],
        &["identity", "continue", "--help"],
        &["identity", "voice", "add", "--help"],
        &["identity", "voice", "withdraw", "--help"],
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

#[test]
fn schema_states_canonical_cursor_history_and_bound_watch_truth() {
    let sandbox = Sandbox::new();
    let output = sandbox.run(&["schema"]);
    let schema_text = common::stdout(&output);
    let schema: SchemaOutput = from_stdout(&output);
    let command = |name: &str| {
        schema
            .commands
            .iter()
            .find(|command| command.name == name)
            .unwrap_or_else(|| panic!("{name} command in schema"))
    };
    let watch = command("watch");
    assert!(watch
        .side_effects
        .contains("long-running watch requires a bound participant"));
    assert!(watch.side_effects.contains("bounded sanitized previews"));
    assert!(watch
        .side_effects
        .contains("snapshot is the read-only unbound exception"));
    assert!(!watch.side_effects.contains("legacy unbound watches"));
    assert!(!watch.side_effects.contains("never emits body content"));
    assert!(watch.side_effects.contains("deduplicates channel messages"));
    assert!(watch.side_effects.contains("--snapshot"));
    assert!(watch
        .side_effects
        .contains("never mutates channel seen-sets"));
    assert!(watch
        .default_output
        .contains("mail | unreadable | channel_message"));

    // Channel reads publish membership semantics, not watermarks.
    let chat = command("chat");
    assert!(chat.side_effects.contains("consumes only emitted ids"));
    assert!(chat
        .side_effects
        .contains("--seen-by lists members whose seen-set contains an id"));
    for stale in [
        "advances the reader's own cursor",
        "cursors passed an id",
        "past the sender cursor",
        "never advances channel cursors",
        "an invalid value is a loud error",
    ] {
        assert!(
            !schema_text.contains(stale),
            "stale contract phrase still published: {stale}"
        );
    }

    let laws = schema.laws.join("\n");
    assert!(laws.contains(
        "Already-read participant mail stays in its canonical inbox and remains retrievable"
    ));
    assert!(laws.contains("legacy room cursor, read/, and channel-state files are read-only"));
    assert!(laws.contains("never imported into participant cursor state"));
    assert!(laws.contains("Search is participant-visible history"));
    assert!(!laws.contains("retrievable by id or prefix from the read store"));
    assert!(!laws.contains("legacy channel-state.json is imported"));

    let environment = schema.environment.join("\n");
    assert!(environment.contains("legacy cursor state is never imported or materialized"));
    assert!(!environment.contains("valid legacy channel-state.json imports"));
    assert!(environment.contains("falls back to auto (presentation never breaks a read"));

    macro_rules! assert_shape {
        ($name:literal, $shape:expr, [$($field:literal),* $(,)?]) => {
            assert_eq!(
                shape_top_level_keys($shape),
                expected_keys(&[$($field),*]),
                "{} output shape top-level fields drifted",
                $name
            );
        };
    }
    assert_shape!(
        "participant",
        &schema.output_shapes.participant,
        [
            "show/bind/touch/end:",
            "describe",
            "list:",
            "restore",
            "notice:",
            "participant_missing",
            "gc"
        ]
    );
    assert_shape!(
        "identity",
        &schema.output_shapes.identity,
        ["list:", "show:", "new/continue/leave:", "voice"]
    );
    assert_shape!(
        "version",
        &schema.output_shapes.version,
        [
            "ok",
            "version",
            "build_sha",
            "store_version",
            "store",
            "store_error",
            "capabilities"
        ]
    );
    assert_shape!(
        "contract",
        &schema.output_shapes.contract,
        [
            "samples:",
            "samples_dir:",
            "skill_manifest:",
            "skill_verify:"
        ]
    );
    assert_shape!(
        "doctor",
        &schema.output_shapes.doctor,
        [
            "ok",
            "status",
            "root",
            "checks",
            "count",
            "fixed",
            "exit_codes",
            "participant",
            "pending",
            "participant_fix",
            "participant_error",
            "bound",
            "participant_missing",
            "severity_filter",
            "filtered_out"
        ]
    );
    assert_shape!(
        "inbox",
        &schema.output_shapes.inbox,
        [
            "ok",
            "room",
            "participant",
            "unread",
            "count",
            "skipped_unreadable",
            "unread_count",
            "pending",
            "pending_by_address",
            "held",
            "bound",
            "bound_now",
            "hint"
        ]
    );
    assert_shape!(
        "read_json",
        &schema.output_shapes.read_json,
        [
            "ok",
            "framing",
            "envelope",
            "body",
            "own",
            "pending",
            "already_read",
            "bound",
            "bound_now"
        ]
    );
    assert_shape!(
        "read_budget",
        &schema.output_shapes.read_budget,
        [
            "ok",
            "framing",
            "envelope",
            "body",
            "own",
            "pending",
            "already_read",
            "count",
            "selected_count",
            "has_more",
            "byte_limit",
            "omitted"
        ]
    );
    assert_shape!(
        "read_slice",
        &schema.output_shapes.read_slice,
        [
            "ok",
            "framing",
            "envelope",
            "body_slice",
            "range",
            "total_body_bytes",
            "body_complete",
            "next_offset",
            "continuation",
            "already_read",
            "own",
            "pending",
            "verification_scope",
            "byte_limit"
        ]
    );
    assert_shape!(
        "read_ack",
        &schema.output_shapes.read_ack,
        ["ok", "room", "id", "already_read", "acknowledged"]
    );
    assert_shape!(
        "rooms",
        &schema.output_shapes.rooms,
        ["ok", "rooms", "count", "warnings", "set-path:", "rename:"]
    );
    assert_shape!(
        "schema",
        &schema.output_shapes.schema,
        [
            "ok",
            "name",
            "contract_version",
            "store_version",
            "capabilities",
            "participant",
            "participant_error",
            "global_flags",
            "commands",
            "output_shapes",
            "error_shape",
            "error_codes",
            "exit_codes",
            "doctor_exit_codes",
            "laws",
            "environment",
            "owner"
        ]
    );
    assert_shape!(
        "send_json",
        &schema.output_shapes.send_json,
        [
            "ok",
            "envelope",
            "archived",
            "delivery",
            "cross_host",
            "resolved",
            "retargeted",
            "bound_now",
            "warnings"
        ]
    );
    assert_shape!(
        "chat_join",
        &schema.output_shapes.chat_join,
        [
            "ok",
            "channel",
            "room",
            "created",
            "already_member",
            "backlog_ignored",
            "event_id",
            "history_before_join",
            "history_hint",
            "bound_now",
            "normalized_from"
        ]
    );
    assert_shape!(
        "chat_send",
        &schema.output_shapes.chat_send,
        [
            "ok",
            "message",
            "cross_host",
            "bound_now",
            "crossed",
            "skipped",
            "warnings"
        ]
    );
    assert_shape!(
        "chat_read",
        &schema.output_shapes.chat_read,
        [
            "ok",
            "framing",
            "channel",
            "room",
            "peek",
            "messages",
            "count",
            "skipped",
            "has_more",
            "selected_count",
            "byte_limit",
            "omitted",
            "bound",
            "bound_now",
            "skipped_files"
        ]
    );
    assert_shape!(
        "chat_slice",
        &schema.output_shapes.chat_slice,
        [
            "ok",
            "framing",
            "channel",
            "room",
            "message",
            "origin",
            "reply_to_participant",
            "reply_to_shared",
            "body_slice",
            "range",
            "total_body_bytes",
            "body_complete",
            "next_offset",
            "continuation",
            "signed_verified",
            "emote_rule",
            "verification_scope",
            "byte_limit"
        ]
    );
    assert_shape!(
        "chat_ack",
        &schema.output_shapes.chat_ack,
        ["ok", "channel", "room", "id", "acknowledged"]
    );
    assert_shape!(
        "chat_discard",
        &schema.output_shapes.chat_discard,
        ["ok", "channel", "room", "discarded", "cursor"]
    );
    assert_shape!(
        "chat_discard_through",
        &schema.output_shapes.chat_discard_through,
        [
            "ok",
            "channel",
            "room",
            "target",
            "prior_cursor",
            "cursor",
            "advanced",
            "discarded"
        ]
    );
    assert_shape!(
        "catchup",
        &schema.output_shapes.catchup,
        [
            "ok",
            "room",
            "targets",
            "count",
            "selected_count",
            "has_more",
            "byte_limit",
            "omitted",
            "bound_now",
            "skipped"
        ]
    );
    assert_shape!(
        "search",
        &schema.output_shapes.search,
        [
            "ok",
            "participant",
            "pending",
            "framing",
            "room",
            "pattern",
            "match",
            "results",
            "count",
            "limit",
            "truncated",
            "bound",
            "skipped"
        ]
    );
    assert_shape!(
        "channels",
        &schema.output_shapes.channels,
        [
            "ok",
            "channels",
            "count",
            "archived_hidden",
            "participant",
            "pending",
            "bound",
            "skipped"
        ]
    );
    assert_shape!(
        "profile",
        &schema.output_shapes.profile,
        [
            "ok",
            "room",
            "profile",
            "avatar",
            "warnings",
            "announced",
            "list:"
        ]
    );
    assert_shape!(
        "watch",
        &schema.output_shapes.watch,
        [
            "mail:",
            "unreadable:",
            "channel_message:",
            "unbound:",
            "digest:"
        ]
    );
    assert_shape!(
        "who",
        &schema.output_shapes.who,
        [
            "ok",
            "participant",
            "participants",
            "legacy_rooms",
            "activity_note",
            "count",
            "skipped",
            "bridge_attention",
            "bridge_health",
            "doorbell",
            "bound",
            "participant_missing"
        ]
    );
    assert_eq!(
        schema
            .output_shapes
            .read_json
            .iter()
            .find(|field| field.starts_with("envelope "))
            .expect("read envelope shape"),
        "envelope (id, from, to, kind, subject, sent, from_participant?, from_lineage?, address_kind?, display_name?, pfp?, sender_address?, sender_provenance?, origin, reply_to_participant?, reply_to_shared, pending?, address{kind,name}?)"
    );
    assert!(schema
        .global_flags
        .iter()
        .any(|flag| flag.contains("participant binding") && flag.contains("never cwd")));
}

#[test]
fn schema_matches_catchup_and_search_help_and_json() {
    let sandbox = Sandbox::new();
    let (_, beta) = register_alpha_beta(&sandbox);
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
        assert_eq!(
            help_options(&text),
            usage_options(usage),
            "schema usage and clap help disagree about command options"
        );
    }

    let catchup_output = sandbox.run_in(&["catchup", "--all", "--json"], None, &beta);
    assert_success(&catchup_output);
    let catchup_json = json_object(&catchup_output);
    let catchup_top = ["ok", "room", "targets", "count"];
    assert_documented(
        "catchup",
        &schema.output_shapes.catchup.join("\n"),
        &catchup_json,
        "catchup --all",
    );
    let catchup_top_keys = keys(&catchup_json);
    assert_eq!(
        catchup_top_keys,
        catchup_top.iter().map(|key| (*key).to_owned()).collect()
    );
    let targets = catchup_json["targets"].as_array().expect("catchup targets");
    assert!(targets.iter().any(|target| target["source"] == "mail"));
    assert!(targets.iter().any(|target| target["source"] == "channel"));
    for target in targets {
        let actual = keys(target);
        assert!(actual.contains("source"));
        assert!(actual.contains("framing"));
        assert!(actual.contains("messages"));
        assert!(actual.contains("count"));
        let framing_keys = keys(&target["framing"]);
        assert_eq!(
            framing_keys,
            ["source", "authority"]
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
    assert_documented(
        "search",
        &schema.output_shapes.search.join("\n"),
        &search_json,
        "search --json",
    );
    let search_top_keys = keys(&search_json);
    assert_eq!(
        search_top_keys,
        search_top.iter().map(|key| (*key).to_owned()).collect()
    );
    let results = search_json["results"].as_array().expect("search results");
    assert!(!results.is_empty());
    let framing_keys = keys(&search_json["framing"]);
    assert_eq!(
        framing_keys,
        ["source", "authority"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    );
    let result_fields = [
        "source", "channel", "id", "from", "sent", "subject", "preview", "matched",
    ];
    for result in results {
        let actual = keys(result);
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

    // The channels keys are pinned by the contract sample and the schema-truth
    // check; what only this fixture has is a channel the reader is not in.
    let channels_output = sandbox.run_in(&["channels"], None, &beta);
    assert_success(&channels_output);
    let channels_json = json_object(&channels_output);
    let private = channels_json["channels"]
        .as_array()
        .expect("channels")
        .iter()
        .find(|channel| channel["name"] == "private")
        .expect("private channel");
    assert_eq!(private["room"], "beta");
    assert!(private["unread"].is_null());
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
    for command in ["chat", "read"] {
        let usage = &schema
            .commands
            .iter()
            .find(|item| item.name == command)
            .expect("command in schema")
            .usage;
        let help = sandbox.run(&[command, "--help"]);
        assert_success(&help);
        assert_eq!(
            help_options(&common::stdout(&help)),
            usage_options(usage),
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
    assert_documented(
        "chat_read",
        &schema.output_shapes.chat_read.join("\n"),
        &chat_budget,
        "chat --max-bytes",
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
    assert_documented(
        "chat_slice",
        &schema.output_shapes.chat_slice.join("\n"),
        &json_object(&chat_slice),
        "chat --message",
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
    let read_budget = json_object(&read_budget);
    assert_documented(
        "read_budget",
        &schema.output_shapes.read_budget.join("\n"),
        &read_budget,
        "read --max-bytes",
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
    let read_slice = json_object(&read_slice);
    assert_documented(
        "read_slice",
        &schema.output_shapes.read_slice.join("\n"),
        &read_slice,
        "read --offset",
    );

    let catchup = sandbox.run_in(
        &["catchup", "--all", "--max-bytes", "1400", "--json"],
        None,
        &beta,
    );
    assert_success(&catchup);
    let catchup = json_object(&catchup);
    assert_documented(
        "catchup",
        &schema.output_shapes.catchup.join("\n"),
        &catchup,
        "catchup --max-bytes",
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
    assert_documented(
        "chat_ack",
        &schema.output_shapes.chat_ack.join("\n"),
        &json_object(&chat_ack),
        "chat --ack",
    );
    let read_ack = sandbox.run_in(
        &["read", &mail_id, "--room", "beta", "--ack", "--json"],
        None,
        &beta,
    );
    assert_success(&read_ack);
    assert_documented(
        "read_ack",
        &schema.output_shapes.read_ack.join("\n"),
        &json_object(&read_ack),
        "read --ack",
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
    // With no cursors.json the malformed legacy file is the only channel
    // state, so it is a real error and the run must not report healthy.
    let diagnosed = sandbox.run(&["doctor"]);
    assert_eq!(diagnosed.status.code(), Some(1), "{diagnosed:?}");
    let report: DoctorOutput = from_stdout(&diagnosed);
    let live_invalid = report
        .checks
        .iter()
        .find(|check| check.id == "channel_state.claude-space.invalid")
        .expect("live legacy check");
    assert_eq!(live_invalid.severity, DoctorSeverity::Error);

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
