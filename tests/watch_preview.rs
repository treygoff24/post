use post::output::{InboxItem, MailKind, WatchAddress, WatchEvent, WatchReason};
use post::sanitize_preview;

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
fn watch_typed_snapshot_contract_matches_checked_in_ndjson_fixture() {
    let item = |id: &str, second: &str, subject: &str, pending: bool| InboxItem {
        id: id.to_owned(),
        from: "beta".to_owned(),
        origin: "unknown".to_owned(),
        reply_to_participant: None,
        reply_to_shared: "beta".to_owned(),
        pending,
        kind: MailKind::Note,
        subject: subject.to_owned(),
        sent: format!("2026-09-16 05:00:{second} -0500"),
        display_name: None,
        pfp: None,
        sender_address: None,
        sender_provenance: None,
    };
    let events = [
        WatchEvent::mail(
            "alpha",
            item("20260916-050001-aa0001", "01", "workspace", false),
            None,
        ),
        WatchEvent::mail(
            "lineage:Ember Grove!",
            item("20260916-050002-aa0002", "02", "lineage", false),
            None,
        ),
        WatchEvent::mail(
            "participant:claude-deadbeefcafe",
            item("20260916-050003-aa0003", "03", "participant", false),
            None,
        ),
        WatchEvent::mail(
            "alpha",
            item("20260916-050004-aa0004", "04", "pending", true),
            None,
        ),
    ];
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
