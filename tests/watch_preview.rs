mod common;

use common::{assert_success, register_alpha_beta, write_custom_mail, Sandbox};
use post::output::{InboxItem, MailKind, WatchAddress, WatchEvent, WatchReason};
use post::sanitize_preview;
use serde_json::{json, Value};
use std::fs;

#[test]
fn watch_text_line_includes_sanitized_preview_cap_at_80_chars() {
    // Test with a body longer than 80 characters
    let long_body = "a".repeat(100);
    let sanitized_preview = sanitize_preview(&long_body);
    let event = WatchEvent::mail(
        "test-room",
        InboxItem {
            id: "20260831-224243-test01".to_owned(),
            from: "test-sender".to_owned(),
            from_participant: None,
            from_lineage: None,
            origin: "unknown".to_owned(),
            reply_to_participant: None,
            reply_to_shared: "test-sender".to_owned(),
            pending: false,
            kind: MailKind::Note,
            subject: "test subject".to_owned(),
            sent: "2026-08-31 22:42:43 +0000".to_owned(),
            display_name: None,
            pfp: None,
            sender_address: None,
            sender_provenance: None,
        },
        Some(sanitized_preview),
    );

    let line = event.text_line();
    // Extract just the preview part (after the subject)
    let parts: Vec<&str> = line.split("  ").collect();
    let preview = parts.last().unwrap_or(&"").trim_end();

    // The preview should be at most 81 Unicode characters (80 + truncation)
    assert!(
        preview.chars().count() <= 81,
        "preview should be at most 81 chars (80 + truncation), got {}",
        preview.chars().count()
    );
    // If the original was longer than 80 chars, the preview should be truncated
    if long_body.len() > 80 && preview.chars().count() == 81 {
        assert!(
            preview.ends_with("…"),
            "truncated preview should end with ellipsis"
        );
    }
}

#[test]
fn watch_text_line_sanitizes_control_characters_and_newlines() {
    // Test with control characters and newlines
    let body_with_controls = "test\nbody\r\nwith\tcontrol\x00chars\x1b[31m";
    let sanitized_preview = sanitize_preview(body_with_controls);
    let event = WatchEvent::mail(
        "test-room",
        InboxItem {
            id: "20260831-224243-test02".to_owned(),
            from: "test-sender".to_owned(),
            from_participant: None,
            from_lineage: None,
            origin: "unknown".to_owned(),
            reply_to_participant: None,
            reply_to_shared: "test-sender".to_owned(),
            pending: false,
            kind: MailKind::Note,
            subject: "test subject".to_owned(),
            sent: "2026-08-31 22:42:43 +0000".to_owned(),
            display_name: None,
            pfp: None,
            sender_address: None,
            sender_provenance: None,
        },
        Some(sanitized_preview),
    );
    // Newlines and tabs flatten to spaces; NUL and ESC are dropped outright,
    // and the `[` that followed ESC goes full-width like any other bracket.
    assert_eq!(
        sanitize_preview(body_with_controls),
        "test body  with controlchars［31m"
    );
    let line = event.text_line();
    assert!(!line.contains('\x00'));
    assert!(!line.contains('\x1b'));
    assert!(!line.contains('\t'));
}

#[test]
fn watch_text_line_neutralizes_square_brackets_to_prevent_fencepost_forging() {
    // Test with square brackets that could forge fenceposts
    let hostile_body = "test [--since 'attacker-id'] body with [brackets]";
    let sanitized_preview = sanitize_preview(hostile_body);
    let event = WatchEvent::mail(
        "test-room",
        InboxItem {
            id: "20260831-224243-test03".to_owned(),
            from: "test-sender".to_owned(),
            from_participant: None,
            from_lineage: None,
            origin: "unknown".to_owned(),
            reply_to_participant: None,
            reply_to_shared: "test-sender".to_owned(),
            pending: false,
            kind: MailKind::Note,
            subject: "test subject".to_owned(),
            sent: "2026-08-31 22:42:43 +0000".to_owned(),
            display_name: None,
            pfp: None,
            sender_address: None,
            sender_provenance: None,
        },
        Some(sanitized_preview),
    );

    let line = event.text_line();
    // Extract just the preview part
    let parts: Vec<&str> = line.split("  ").collect();
    let preview = parts.last().unwrap_or(&"").trim_end();

    // Square brackets should be replaced with full-width versions
    assert!(preview.contains("［"));
    assert!(preview.contains("］"));
    // Original brackets should not be present in preview
    assert!(!preview.contains("[--since 'attacker-id']"));
}

#[test]
fn watch_channel_message_includes_preview() {
    let event = WatchEvent::ChannelMessage {
        address: WatchAddress {
            kind: "workspace".to_owned(),
            name: "test-room".to_owned(),
        },
        room: "test-room".to_owned(),
        channel: "test-channel".to_owned(),
        id: "20260831-224243-test04".to_owned(),
        from: "test-sender".to_owned(),
        from_participant: None,
        from_host: None,
        from_lineage: None,
        origin: "unknown".to_owned(),
        reply_to_participant: None,
        reply_to_shared: "test-sender".to_owned(),
        subject: "test subject".to_owned(),
        sent: "2026-08-31 22:42:43 +0000".to_owned(),
        display_name: None,
        pfp: None,
        sender_address: None,
        sender_provenance: None,
        reason: WatchReason::Channel,
        preview: Some("test channel preview".to_owned()),
    };

    let line = event.text_line();
    assert!(line.contains("  test channel preview"));
}

#[test]
fn watch_unreadable_message_has_no_preview() {
    let event = WatchEvent::Unreadable {
        address: WatchAddress {
            kind: "workspace".to_owned(),
            name: "test-room".to_owned(),
        },
        room: "test-room".to_owned(),
        id: "20260831-224243-test05".to_owned(),
        reason: WatchReason::Mail,
        channel: None,
        preview: None, // Unreadable messages have no body to preview
    };

    let line = event.text_line();
    assert_eq!(
        line,
        "\"20260831-224243-test05\"  [?] unreadable envelope\n"
    );
}

#[test]
fn watch_ndjson_includes_preview_field() {
    let event = WatchEvent::mail(
        "test-room",
        InboxItem {
            id: "20260831-224243-test06".to_owned(),
            from: "test-sender".to_owned(),
            from_participant: None,
            from_lineage: None,
            origin: "unknown".to_owned(),
            reply_to_participant: None,
            reply_to_shared: "test-sender".to_owned(),
            pending: false,
            kind: MailKind::Note,
            subject: "test subject".to_owned(),
            sent: "2026-08-31 22:42:43 +0000".to_owned(),
            display_name: None,
            pfp: None,
            sender_address: None,
            sender_provenance: None,
        },
        Some("test preview".to_owned()),
    );

    let json = serde_json::to_string(&event).expect("serialize");
    assert!(json.contains("\"preview\":\"test preview\""));
}

#[test]
fn watch_ndjson_omits_preview_field_when_none() {
    let event = WatchEvent::mail(
        "test-room",
        InboxItem {
            id: "20260831-224243-test07".to_owned(),
            from: "test-sender".to_owned(),
            from_participant: None,
            from_lineage: None,
            origin: "unknown".to_owned(),
            reply_to_participant: None,
            reply_to_shared: "test-sender".to_owned(),
            pending: false,
            kind: MailKind::Note,
            subject: "test subject".to_owned(),
            sent: "2026-08-31 22:42:43 +0000".to_owned(),
            display_name: None,
            pfp: None,
            sender_address: None,
            sender_provenance: None,
        },
        None,
    );

    let json = serde_json::to_string(&event).expect("serialize");
    assert!(!json.contains("preview"));
}

#[test]
fn watch_text_marks_pending_mail() {
    let event = WatchEvent::mail(
        "alpha",
        InboxItem {
            id: "20260916-050004-aa0004".to_owned(),
            from: "beta".to_owned(),
            from_participant: None,
            from_lineage: None,
            origin: "unknown".to_owned(),
            reply_to_participant: None,
            reply_to_shared: "beta".to_owned(),
            pending: true,
            kind: MailKind::Note,
            subject: "pending".to_owned(),
            sent: "2026-09-16 05:00:04 -0500".to_owned(),
            display_name: None,
            pfp: None,
            sender_address: None,
            sender_provenance: None,
        },
        Some("held preview".to_owned()),
    );
    let text = event.text_line();
    assert!(text.contains(" pending "), "pending marker missing: {text}");
}

#[test]
fn watch_typed_snapshot_contract_matches_checked_in_ndjson_fixture() {
    let sandbox = Sandbox::new_unseeded();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let actor = "claude-deadbeefcafe";
    let actor_dir = sandbox.mail_root.join("participants").join(actor);
    fs::create_dir_all(&actor_dir).expect("actor directory");
    let write_actor = |workspace: Option<&str>, lineage: Option<&str>| {
        fs::write(
            actor_dir.join("participant.json"),
            format!(
                "{}\n",
                serde_json::to_string_pretty(&json!({
                    "version": 1,
                    "id": actor,
                    "harness": "claude",
                    "conversation_key_digest": "0".repeat(64),
                    "created": "2026-09-16 05:00:00 -0500",
                    "last_seen": "2099-01-01T00:00:00Z",
                    "lease_hours": 24,
                    "workspace": workspace,
                    "workspace_path": workspace.map(|_| alpha.clone()),
                    "lineage": lineage,
                    "lineage_since": lineage.map(|_| "2026-09-16T10:00:00Z")
                }))
                .expect("actor JSON")
            ),
        )
        .expect("write actor");
    };
    write_actor(Some("alpha"), None);
    let sender = sandbox.test_participant("beta");
    let send = |target: &str, subject: &str, body: &str| {
        let output = sandbox.run_as_participant(
            &[
                "send",
                "--to",
                target,
                "--subject",
                subject,
                "--body",
                body,
                "--json",
            ],
            &sender,
            &beta,
        );
        assert_success(&output);
    };
    send("workspace:alpha", "workspace", "workspace preview");

    let lineage_dir = sandbox.mail_root.join("lineages/Ember Grove!");
    fs::create_dir_all(&lineage_dir).expect("lineage directory");
    fs::write(
        lineage_dir.join("lineage.json"),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&json!({
                "version": 1,
                "name": "Ember Grove!",
                "founder": actor,
                "created": "2026-09-16T10:00:00Z",
                "host": "test"
            }))
            .unwrap()
        ),
    )
    .expect("lineage record");
    write_actor(Some("alpha"), Some("Ember Grove!"));
    send("lineage:Ember Grove!", "lineage", "lineage preview");
    send(
        &format!("participant:{actor}"),
        "participant",
        "participant preview",
    );
    write_actor(None, Some("Ember Grove!"));

    let channel = sandbox.mail_root.join("channels/typed-snapshot");
    fs::create_dir_all(channel.join("messages")).expect("channel messages");
    fs::write(
        channel.join("channel.json"),
        r#"{"name":"typed-snapshot","created":"2026-09-16 05:00:00 -0500","created_by":"beta"}"#,
    )
    .expect("channel info");
    fs::write(channel.join("members.json"), b"{}\n").expect("legacy members");
    for participant in [actor, sender.as_str()] {
        fs::write(
            sandbox
                .mail_root
                .join("participants")
                .join(participant)
                .join("channels.json"),
            r#"{"version":1,"joined":["typed-snapshot"],"left":[]}"#,
        )
        .expect("participant channels");
    }
    let channel_message = sandbox.run_as_participant(
        &[
            "chat",
            "typed-snapshot",
            "--send",
            "--anyway",
            "--subject",
            "channel",
            "--body",
            "channel preview",
            "--json",
        ],
        &sender,
        &beta,
    );
    assert_success(&channel_message);

    let pending_id = "20990916-050004-aa0004";
    write_custom_mail(
        &sandbox
            .mail_root
            .join("participants")
            .join(actor)
            .join("inbox"),
        pending_id,
        &json!({
            "id": pending_id,
            "from": "beta",
            "to": actor,
            "kind": "note",
            "subject": "pending",
            "sent": "2026-09-16 05:00:04 -0500",
            "from_participant": sender,
            "address_kind": "participant"
        }),
        "pending preview",
    );

    let snapshot = sandbox.run_as_participant(&["watch", "--snapshot"], actor, &sandbox.path);
    assert!(snapshot.status.success(), "{}", common::stderr(&snapshot));
    let canonical = [
        (
            "workspace",
            "20260916-050001-aa0001",
            "2026-09-16 05:00:01 -0500",
        ),
        (
            "lineage",
            "20260916-050002-aa0002",
            "2026-09-16 05:00:02 -0500",
        ),
        (
            "participant",
            "20260916-050003-aa0003",
            "2026-09-16 05:00:03 -0500",
        ),
        (
            "pending",
            "20260916-050004-aa0004",
            "2026-09-16 05:00:04 -0500",
        ),
        (
            "channel",
            "20260916-050005-000001-aa0005",
            "2026-09-16 05:00:05 -0500",
        ),
    ];
    let events = common::stdout(&snapshot)
        .lines()
        .map(|line| {
            let mut event: Value = serde_json::from_str(line).expect("snapshot event");
            let subject = event["subject"].as_str().expect("subject");
            let (_, id, sent) = canonical
                .iter()
                .find(|(name, _, _)| *name == subject)
                .expect("known fixture subject");
            event["id"] = json!(id);
            event["sent"] = json!(sent);
            if !event["reply_to_participant"].is_null() {
                event["reply_to_participant"] = json!("participant:test-sender");
            }
            if !event["from_participant"].is_null() {
                event["from_participant"] = json!("test-sender");
            }
            event
        })
        .collect::<Vec<_>>();
    let rendered = events
        .iter()
        .map(|event| serde_json::to_string(event).expect("watch event JSON"))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    assert_eq!(
        rendered,
        include_str!("fixtures/watch-snapshot-typed.ndjson")
    );
}
