mod common;

use common::{
    assert_success, from_stderr, from_stdout, register_alpha_beta, write_bad_channel,
    write_channel_message, write_custom_mail, Sandbox,
};
use post::output::{ErrorEnvelope, SearchOutput};
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

fn tree_bytes(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, path: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        let Ok(entries) = fs::read_dir(path) else {
            return;
        };
        for entry in entries {
            let entry = entry.expect("read manifest entry");
            let path = entry.path();
            if path.is_dir() {
                visit(root, &path, out);
            } else if path.is_file() {
                out.insert(
                    path.strip_prefix(root)
                        .expect("manifest path under root")
                        .to_owned(),
                    fs::read(&path).expect("read manifest file"),
                );
            }
        }
    }

    let mut out = BTreeMap::new();
    visit(root, root, &mut out);
    out
}

fn mail_envelope(id: &str, from: &str, to: &str, subject: &str) -> serde_json::Value {
    json!({
        "id": id,
        "from": from,
        "to": to,
        "kind": "note",
        "subject": subject,
        "sent": "2026-08-20 12:00:00 +0000"
    })
}

fn channel_fixture(sandbox: &Sandbox, name: &str, members: &str) {
    write_bad_channel(
        sandbox,
        name,
        Some(members),
        true,
        &format!(
            "{{\"name\":\"{name}\",\"created\":\"2026-08-20 12:00:00 +0000\",\"created_by\":\"alpha\"}}"
        ),
    );
}

#[test]
fn search_is_party_visible_deduplicated_and_cursorless() {
    let sandbox = Sandbox::new();
    let (_alpha, beta) = register_alpha_beta(&sandbox);
    let marker = "VisibilityMarker";

    let inbox_id = "20260820-120000-aaaaaa";
    let read_id = "20260820-120001-bbbbbb";
    let own_archive_id = "20260820-120002-cccccc";
    let member_channel_id = "20260820-120000-000001-dddddd";
    let nonmember_channel_id = "20260820-120000-000002-eeeeee";
    let third_party_archive_id = "20260820-120003-ffffff";

    let inbox = sandbox.mail_root.join("beta/inbox");
    let read = sandbox.mail_root.join("beta/read");
    fs::create_dir_all(&inbox).expect("inbox");
    fs::create_dir_all(&read).expect("read");
    write_custom_mail(
        &inbox,
        inbox_id,
        &mail_envelope(inbox_id, "alpha", "beta", "inbox subject"),
        marker,
    );
    write_custom_mail(
        &read,
        read_id,
        &mail_envelope(read_id, "alpha", "beta", "read subject"),
        marker,
    );

    let archive = sandbox.mail_root.join("archive");
    fs::create_dir_all(&archive).expect("archive");
    write_custom_mail(
        &archive,
        own_archive_id,
        &mail_envelope(own_archive_id, "beta", "alpha", "sent subject"),
        marker,
    );
    write_custom_mail(
        &archive,
        third_party_archive_id,
        &mail_envelope(third_party_archive_id, "alpha", "gamma", "private subject"),
        marker,
    );

    channel_fixture(&sandbox, "member", r#"{"alpha":"joined","beta":"joined"}"#);
    write_channel_message(
        &sandbox,
        "member",
        member_channel_id,
        "alpha",
        "channel subject",
        marker,
    );
    channel_fixture(&sandbox, "private", r#"{"alpha":"joined"}"#);
    write_channel_message(
        &sandbox,
        "private",
        nonmember_channel_id,
        "alpha",
        "private channel subject",
        marker,
    );

    // A duplicate id in read/ must not create a second result; inbox wins.
    let duplicate_id = "20260820-120004-111111";
    write_custom_mail(
        &inbox,
        duplicate_id,
        &mail_envelope(duplicate_id, "alpha", "beta", "inbox wins"),
        marker,
    );
    write_custom_mail(
        &read,
        duplicate_id,
        &mail_envelope(duplicate_id, "alpha", "beta", "read loses"),
        marker,
    );

    let before = tree_bytes(&sandbox.mail_root);
    let output = sandbox.run_in(&["search", marker, "--json"], None, &beta);
    assert_success(&output);
    let parsed: SearchOutput = from_stdout(&output);
    let ids: Vec<&str> = parsed
        .results
        .iter()
        .map(|result| result.id.as_str())
        .collect();
    assert_eq!(parsed.match_kind, "literal_case_insensitive");
    assert_eq!(parsed.count, 5);
    assert_eq!(ids.len(), 5);
    for expected in [
        inbox_id,
        read_id,
        own_archive_id,
        member_channel_id,
        duplicate_id,
    ] {
        assert!(
            ids.contains(&expected),
            "missing visible result {expected}: {ids:?}"
        );
    }
    assert!(!ids.contains(&nonmember_channel_id));
    assert!(!ids.contains(&third_party_archive_id));
    assert_eq!(
        parsed
            .results
            .iter()
            .filter(|result| result.id == duplicate_id)
            .count(),
        1
    );
    assert!(parsed.results.iter().all(|result| result.preview == marker));
    assert!(parsed
        .results
        .iter()
        .any(|result| result.source == "channel"));
    assert!(parsed.results.iter().any(|result| result.source == "mail"));
    assert_eq!(before, tree_bytes(&sandbox.mail_root));
    assert!(!sandbox.mail_root.join("beta/cursors.json").exists());
}

#[test]
fn search_literal_caps_and_sanitizes_previews() {
    let sandbox = Sandbox::new();
    let (_alpha, beta) = register_alpha_beta(&sandbox);
    channel_fixture(&sandbox, "literal", r#"{"alpha":"joined","beta":"joined"}"#);

    for index in 1..=101 {
        let id = format!("20260820-120000-{index:06}-aa{index:04x}");
        let body = if index == 101 {
            "[literal]\nline\u{0000} with control".to_owned()
        } else {
            "[literal]".to_owned()
        };
        write_channel_message(&sandbox, "literal", &id, "alpha", "", &body);
    }

    let output = sandbox.run_in(
        &["search", "[", "--channel", "literal", "--json"],
        None,
        &beta,
    );
    assert_success(&output);
    let parsed: SearchOutput = from_stdout(&output);
    assert_eq!(parsed.count, 100);
    assert_eq!(parsed.limit, 100);
    assert!(
        parsed.truncated,
        "the 101st literal match must set truncated"
    );
    assert!(parsed
        .results
        .iter()
        .all(|result| result.matched == vec!["body".to_owned()]));
    assert!(parsed
        .results
        .iter()
        .all(|result| !result.preview.contains('\n')));
    assert!(parsed
        .results
        .iter()
        .all(|result| !result.preview.contains('\0')));

    let uncapped = sandbox.run_in(
        &[
            "search",
            "[",
            "--channel",
            "literal",
            "--limit",
            "1000",
            "--json",
        ],
        None,
        &beta,
    );
    assert_success(&uncapped);
    let uncapped: SearchOutput = from_stdout(&uncapped);
    assert_eq!(uncapped.count, 101);
    assert!(!uncapped.truncated);

    let invalid = sandbox.run_in(
        &[
            "search",
            "[",
            "--channel",
            "literal",
            "--limit",
            "1001",
            "--json",
        ],
        None,
        &beta,
    );
    assert_eq!(invalid.status.code(), Some(2));
}

#[test]
fn invalid_membership_closes_explicit_channel_without_opening_messages() {
    let sandbox = Sandbox::new();
    let (_alpha, beta) = register_alpha_beta(&sandbox);
    channel_fixture(&sandbox, "broken", "not-json");
    let id = "20260820-120000-000001-abcdef";
    write_channel_message(&sandbox, "broken", id, "alpha", "", "must stay closed");

    let output = sandbox.run_in(
        &["search", "closed", "--channel", "broken", "--json"],
        None,
        &beta,
    );
    assert_eq!(output.status.code(), Some(78));
    assert!(output.stdout.is_empty());
    let error: ErrorEnvelope = from_stderr(&output);
    assert_eq!(error.error.code, "config_invalid");
    assert!(sandbox
        .mail_root
        .join(format!("channels/broken/messages/{id}.msg"))
        .exists());
}

#[test]
fn search_succeeds_on_fenced_store_with_stale_generation() {
    let sandbox = Sandbox::new();
    common::seed_fence_store(&sandbox, r#"{"state":"active","generation":7}"#);
    let before = tree_bytes(&sandbox.mail_root);
    let output = sandbox.run_in_env(
        &["search", "anything", "--mail", "--json"],
        None,
        &sandbox.home.join("dest"),
        &[("POST_ARX_GENERATION", "6")],
    );
    assert_success(&output);
    let parsed: SearchOutput = from_stdout(&output);
    assert_eq!(parsed.room, "dest");
    assert_eq!(parsed.count, 0);
    assert!(!parsed.truncated);
    assert_eq!(before, tree_bytes(&sandbox.mail_root));
}
