mod common;

use common::{
    assert_success, from_stderr, from_stdout, register_alpha_beta, write_bad_channel,
    write_channel_message, Sandbox,
};
use post::output::{ErrorEnvelope, SearchOutput};
use std::collections::{BTreeMap, BTreeSet};
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
fn search_mail_is_visible_history_for_recipient_and_sender_after_consumption() {
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

    assert_eq!(
        search_ids.iter().cloned().collect::<BTreeSet<_>>(),
        ids.iter().cloned().collect::<BTreeSet<_>>(),
        "history search must return exactly both same-second sends"
    );
    let repeated = sandbox.run_as_participant(
        &["search", marker, "--mail", "--json"],
        &beta_participant,
        &beta,
    );
    assert_success(&repeated);
    let repeated: SearchOutput = from_stdout(&repeated);
    assert_eq!(
        repeated
            .results
            .iter()
            .map(|result| result.id.clone())
            .collect::<Vec<_>>(),
        search_ids,
        "same-second history order must still be deterministic"
    );
    let read = parsed
        .results
        .iter()
        .find(|result| result.id == ids[0])
        .expect("consumed mail remains searchable");
    assert!(read.already_read);
    assert!(!read.own);
    assert!(!read.pending);
    assert_eq!(
        read.reply_to_participant,
        Some(format!("participant:{alpha_participant}"))
    );
    assert_eq!(read.reply_to_shared, "alpha");
    assert_eq!(before, tree_bytes(&sandbox.mail_root));

    let own = sandbox.run_as_participant(
        &["search", &format!("{marker} first"), "--mail", "--json"],
        &alpha_participant,
        &alpha,
    );
    assert_success(&own);
    let own: SearchOutput = from_stdout(&own);
    assert_eq!(own.count, 1);
    assert_eq!(own.results[0].id, ids[0]);
    assert!(own.results[0].own);
    assert!(!own.results[0].pending);
}

#[test]
fn search_channel_is_member_history_after_read_and_for_the_sender() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let alpha_participant = sandbox.test_participant("alpha");
    let beta_participant = sandbox.test_participant("beta");
    for (participant, cwd) in [(&alpha_participant, &alpha), (&beta_participant, &beta)] {
        assert_success(&sandbox.run_as_participant(
            &["chat", "history-search", "--join", "--json"],
            participant,
            cwd,
        ));
    }
    let sent = sandbox.run_as_participant(
        &[
            "chat",
            "history-search",
            "--send",
            "--anyway",
            "--body",
            "channel-history-proof",
            "--json",
        ],
        &alpha_participant,
        &alpha,
    );
    assert_success(&sent);
    let sent: serde_json::Value = from_stdout(&sent);
    let id = sent["message"]["id"].as_str().expect("channel id");
    assert_success(&sandbox.run_as_participant(
        &["chat", "history-search", "--json"],
        &beta_participant,
        &beta,
    ));

    for (participant, cwd, own, already_read) in [
        (&beta_participant, &beta, false, true),
        (&alpha_participant, &alpha, true, true),
    ] {
        let output = sandbox.run_as_participant(
            &[
                "search",
                "channel-history-proof",
                "--channel",
                "history-search",
                "--json",
            ],
            participant,
            cwd,
        );
        assert_success(&output);
        let output: SearchOutput = from_stdout(&output);
        assert_eq!(output.count, 1);
        assert_eq!(output.results[0].id, id);
        assert_eq!(output.results[0].own, own);
        assert_eq!(output.results[0].already_read, already_read);
    }
}

#[test]
fn search_imported_roomless_channel_sender_keeps_host_and_reply_address() {
    let sandbox = Sandbox::new();
    let (_alpha, beta) = register_alpha_beta(&sandbox);
    let participant = sandbox.test_participant("beta");
    assert_success(&sandbox.run_as_participant(
        &["chat", "remote-search", "--join", "--json"],
        &participant,
        &beta,
    ));
    let sender = "claude-76a2b853";
    let message_id = "20260925-120000-000001-a1b2c3";
    let message = serde_json::json!({
        "id": message_id,
        "from": sender,
        "from_participant": sender,
        "from_host": "mac",
        "display_name": "Rook",
        "channel": "remote-search",
        "subject": "",
        "sent": "2026-09-25 12:00:00 -0500"
    });
    let path = sandbox
        .mail_root
        .join("channels/remote-search/messages")
        .join(format!("{message_id}.msg"));
    fs::write(path, format!("{message}\n---\nremote-search-proof"))
        .expect("imported channel message");

    let json = sandbox.run_as_participant(
        &[
            "search",
            "remote-search-proof",
            "--channel",
            "remote-search",
            "--json",
        ],
        &participant,
        &beta,
    );
    assert_success(&json);
    let result: SearchOutput = from_stdout(&json);
    assert_eq!(result.count, 1);
    assert_eq!(result.results[0].origin, "remote");
    assert_eq!(result.results[0].from_host.as_deref(), Some("mac"));
    assert_eq!(
        result.results[0].reply_to_participant.as_deref(),
        Some("participant:claude-76a2b853@mac")
    );
    assert_eq!(
        result.results[0].reply_to_shared,
        "participant:claude-76a2b853@mac"
    );
    let text = sandbox.run_as_participant(
        &[
            "search",
            "remote-search-proof",
            "--channel",
            "remote-search",
        ],
        &participant,
        &beta,
    );
    assert_success(&text);
    assert!(common::stdout(&text).contains("Rook [claude-76a2b853@mac]"));
    assert!(common::stdout(&text).contains("participant:claude-76a2b853@mac"));
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

// --- --since / --until: inclusive UTC calendar days over the id prefix ---

fn day_fixture(sandbox: &Sandbox) -> PathBuf {
    let (_alpha, beta) = register_alpha_beta(sandbox);
    channel_fixture(sandbox, "days", r#"{"alpha":"joined","beta":"joined"}"#);
    for (id, body) in [
        ("20260814-235959-000001-aaaaaa", "dayneedle before"),
        ("20260815-000000-000001-aaaaab", "dayneedle first-boundary"),
        ("20260816-120000-000001-aaaaac", "dayneedle middle"),
        ("20260817-235959-000001-aaaaad", "dayneedle last-boundary"),
        ("20260818-000000-000001-aaaaae", "dayneedle after"),
    ] {
        write_channel_message(sandbox, "days", id, "alpha", "", body);
    }
    beta
}

fn day_search(sandbox: &Sandbox, beta: &Path, extra: &[&str]) -> Vec<String> {
    let mut args = vec!["search", "dayneedle", "--channel", "days", "--json"];
    args.extend_from_slice(extra);
    let output = sandbox.run_in(&args, None, beta);
    assert_success(&output);
    let parsed: SearchOutput = from_stdout(&output);
    let mut ids: Vec<String> = parsed.results.into_iter().map(|r| r.id).collect();
    ids.sort();
    ids
}

#[test]
fn search_since_until_is_inclusive_on_both_boundary_days() {
    let sandbox = Sandbox::new();
    let beta = day_fixture(&sandbox);
    let ids = day_search(
        &sandbox,
        &beta,
        &["--since", "2026-08-15", "--until", "2026-08-17"],
    );
    assert_eq!(
        ids,
        [
            "20260815-000000-000001-aaaaab",
            "20260816-120000-000001-aaaaac",
            "20260817-235959-000001-aaaaad",
        ]
    );
}

#[test]
fn search_since_only_and_until_only_are_independent() {
    let sandbox = Sandbox::new();
    let beta = day_fixture(&sandbox);
    let since = day_search(&sandbox, &beta, &["--since", "2026-08-17"]);
    assert_eq!(
        since,
        [
            "20260817-235959-000001-aaaaad",
            "20260818-000000-000001-aaaaae"
        ]
    );
    let until = day_search(&sandbox, &beta, &["--until", "2026-08-15"]);
    assert_eq!(
        until,
        [
            "20260814-235959-000001-aaaaaa",
            "20260815-000000-000001-aaaaab"
        ]
    );
    assert_eq!(day_search(&sandbox, &beta, &[]).len(), 5);
}

#[test]
fn search_single_day_and_outside_range_and_json_echo() {
    let sandbox = Sandbox::new();
    let beta = day_fixture(&sandbox);
    let one = day_search(
        &sandbox,
        &beta,
        &["--since", "2026-08-16", "--until", "2026-08-16"],
    );
    assert_eq!(one, ["20260816-120000-000001-aaaaac"]);
    assert!(day_search(&sandbox, &beta, &["--since", "2027-01-01"]).is_empty());
    assert!(day_search(&sandbox, &beta, &["--until", "2025-01-01"]).is_empty());

    let output = sandbox.run_in(
        &[
            "search",
            "dayneedle",
            "--channel",
            "days",
            "--json",
            "--since",
            "2026-08-16",
            "--until",
            "2026-08-17",
        ],
        None,
        &beta,
    );
    assert_success(&output);
    let value: serde_json::Value = from_stdout(&output);
    assert_eq!(value["since"], "2026-08-16");
    assert_eq!(value["until"], "2026-08-17");
    let unfiltered: serde_json::Value = from_stdout(&sandbox.run_in(
        &["search", "dayneedle", "--channel", "days", "--json"],
        None,
        &beta,
    ));
    assert!(unfiltered.get("since").is_none() && unfiltered.get("until").is_none());
}

#[test]
fn search_rejects_bad_dates_and_inverted_range() {
    let sandbox = Sandbox::new();
    let beta = day_fixture(&sandbox);
    for (flag, value) in [
        ("--since", "2026-02-30"),
        ("--since", "2026-1-5"),
        ("--until", "yesterday"),
        ("--until", "20260815"),
        ("--since", "2026-13-01"),
        ("--until", ""),
    ] {
        let output = sandbox.run_in(&["search", "dayneedle", "--json", flag, value], None, &beta);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{flag} {value:?} must be rejected"
        );
    }
    // Leap day is real in 2028, not in 2027.
    assert_success(&sandbox.run_in(
        &[
            "search",
            "dayneedle",
            "--channel",
            "days",
            "--since",
            "2028-02-29",
        ],
        None,
        &beta,
    ));
    let inverted = sandbox.run_in(
        &[
            "search",
            "dayneedle",
            "--json",
            "--since",
            "2026-08-17",
            "--until",
            "2026-08-16",
        ],
        None,
        &beta,
    );
    assert_eq!(inverted.status.code(), Some(2));
    let text = String::from_utf8_lossy(&inverted.stderr).into_owned()
        + &String::from_utf8_lossy(&inverted.stdout);
    assert!(text.contains("later than --until"), "{text}");
}

#[test]
fn search_limit_counts_filtered_hits() {
    let sandbox = Sandbox::new();
    let beta = day_fixture(&sandbox);
    // Newest two overall are outside the range; limit 2 must still fill from inside.
    let output = sandbox.run_in(
        &[
            "search",
            "dayneedle",
            "--channel",
            "days",
            "--json",
            "--until",
            "2026-08-16",
            "--limit",
            "2",
        ],
        None,
        &beta,
    );
    assert_success(&output);
    let parsed: SearchOutput = from_stdout(&output);
    assert_eq!(parsed.count, 2);
    assert!(parsed.truncated, "three hits are in range, limit is 2");
    assert!(parsed.results.iter().all(|r| r.id.as_str() < "20260817"));
}

#[test]
fn search_day_filter_applies_to_mail_by_id_day() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let alpha_participant = sandbox.test_participant("alpha");
    let beta_participant = sandbox.test_participant("beta");
    let sent = sandbox.run_as_participant(
        &[
            "send",
            "--to",
            "workspace:beta",
            "--body",
            "mailneedle",
            "--json",
        ],
        &alpha_participant,
        &alpha,
    );
    assert_success(&sent);
    let sent: serde_json::Value = from_stdout(&sent);
    let id = sent["envelope"]["id"].as_str().expect("id");
    let day = format!("{}-{}-{}", &id[..4], &id[4..6], &id[6..8]);
    let count = |extra: &[&str]| {
        let mut args = vec!["search", "mailneedle", "--mail", "--json"];
        args.extend_from_slice(extra);
        let output = sandbox.run_as_participant(&args, &beta_participant, &beta);
        assert_success(&output);
        from_stdout::<SearchOutput>(&output).count
    };
    assert_eq!(count(&["--since", &day, "--until", &day]), 1);
    assert_eq!(
        count(&["--since", "2000-01-01", "--until", "2000-01-02"]),
        0
    );
    assert_eq!(count(&["--since", "2999-01-01"]), 0);
}
