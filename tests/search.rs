mod common;

use common::{
    assert_success, from_stderr, from_stdout, register_alpha_beta, write_bad_channel,
    write_channel_message, Sandbox,
};
use post::output::{ErrorEnvelope, SearchOutput};
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
fn search_matches_the_same_participant_eligibility_as_inbox_and_is_cursorless() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let alpha_participant = sandbox.test_participant("alpha");
    let beta_participant = sandbox.test_participant("beta");
    let marker = "EligibilityMarker";

    let mut ids = Vec::new();
    for body in [format!("{marker} first"), format!("{marker} second")] {
        let sent = sandbox.run_as_participant(
            &["send", "--to", "workspace:beta", "--body", &body, "--json"],
            &alpha_participant,
            &alpha,
        );
        assert_success(&sent);
        let sent: serde_json::Value = from_stdout(&sent);
        ids.push(sent["envelope"]["id"].as_str().expect("mail id").to_owned());
    }

    let consumed =
        sandbox.run_as_participant(&["read", &ids[0], "--json"], &beta_participant, &beta);
    assert_success(&consumed);

    let inbox = sandbox.run_as_participant(&["inbox", "--json"], &beta_participant, &beta);
    assert_success(&inbox);
    let inbox: serde_json::Value = from_stdout(&inbox);
    let inbox_ids: Vec<String> = inbox["unread"]
        .as_array()
        .expect("inbox unread")
        .iter()
        .map(|item| item["id"].as_str().expect("inbox id").to_owned())
        .collect();

    let before = tree_bytes(&sandbox.mail_root);
    let output = sandbox.run_as_participant(
        &["search", marker, "--mail", "--json"],
        &beta_participant,
        &beta,
    );
    assert_success(&output);
    let parsed: SearchOutput = from_stdout(&output);
    let search_ids: Vec<String> = parsed
        .results
        .iter()
        .map(|result| result.id.clone())
        .collect();

    assert_eq!(search_ids, inbox_ids);
    assert_eq!(search_ids, vec![ids[1].clone()]);
    assert_eq!(
        parsed.results[0].reply_to_participant,
        Some(format!("participant:{alpha_participant}"))
    );
    assert_eq!(parsed.results[0].reply_to_shared, "alpha");
    assert_eq!(before, tree_bytes(&sandbox.mail_root));
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
