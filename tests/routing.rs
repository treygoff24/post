mod common;

use common::{assert_success, from_stdout, register_alpha_beta, write_custom_mail, Sandbox};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

fn bind(sandbox: &Sandbox, key: &str, cwd: &Path, workspace: &str) -> String {
    sandbox.bind_claude(key, cwd, Some(workspace))["id"]
        .as_str()
        .expect("participant id")
        .to_owned()
}

fn send_as(sandbox: &Sandbox, participant: &str, cwd: &Path, to: &str, body: &str) -> Value {
    let output = sandbox.run_as_participant(
        &["send", "--to", to, "--body", body, "--json"],
        participant,
        cwd,
    );
    assert_success(&output);
    from_stdout(&output)
}

fn inbox_as(sandbox: &Sandbox, participant: &str, cwd: &Path) -> Value {
    let output = sandbox.run_as_participant(&["inbox"], participant, cwd);
    assert_success(&output);
    from_stdout(&output)
}

fn assert_reply_targets(value: &Value, participant: &str, shared: &str) {
    assert_eq!(value["origin"], "local");
    assert_eq!(
        value["reply_to_participant"],
        format!("participant:{participant}")
    );
    assert_eq!(value["reply_to_shared"], shared);
}

#[test]
fn routing_reply_origin_distinguishes_local_unknown_and_remote_with_collision() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let recipient = bind(&sandbox, "origin-recipient", &alpha, "alpha");
    let colliding_local = bind(&sandbox, "origin-collision", &beta, "beta");
    let inbox = sandbox.mail_root.join("alpha/inbox");

    let unknown = "20990916-040100-aa0001";
    write_custom_mail(
        &inbox,
        unknown,
        &json!({"id":unknown,"from":"legacy-room","to":"alpha","kind":"note","subject":"unknown","sent":"2026-09-16 04:01:00 -0500","from_participant":"claude-deadbeef","address_kind":"workspace"}),
        "unknown",
    );
    let unknown_read =
        sandbox.run_as_participant(&["read", unknown, "--peek", "--json"], &recipient, &alpha);
    assert_success(&unknown_read);
    let unknown_read: Value = from_stdout(&unknown_read);
    assert_eq!(unknown_read["envelope"]["origin"], "unknown");
    assert!(unknown_read["envelope"]["reply_to_participant"].is_null());
    assert_eq!(unknown_read["envelope"]["reply_to_shared"], "legacy-room");

    let peers = sandbox.mail_root.join("bridge/rooms/peers");
    fs::create_dir_all(&peers).expect("peer topology");
    fs::write(peers.join("peer.json"), br#"{"remote-room":"/remote"}"#).expect("peer room map");
    let remote = "20990916-040101-aa0002";
    write_custom_mail(
        &inbox,
        remote,
        &json!({"id":remote,"from":"remote-room","to":"alpha","kind":"note","subject":"remote","sent":"2026-09-16 04:01:01 -0500","from_participant":colliding_local,"address_kind":"workspace"}),
        "remote",
    );
    let remote_read =
        sandbox.run_as_participant(&["read", remote, "--peek", "--json"], &recipient, &alpha);
    assert_success(&remote_read);
    let remote_read: Value = from_stdout(&remote_read);
    assert_eq!(remote_read["envelope"]["origin"], "remote");
    assert!(remote_read["envelope"]["reply_to_participant"].is_null());
    assert_eq!(remote_read["envelope"]["reply_to_shared"], "remote-room");
}

#[test]
fn routing_who_reports_per_address_pending_separately_from_unread() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let a = bind(&sandbox, "who-a", &alpha, "alpha");
    let b = bind(&sandbox, "who-b", &alpha, "alpha");
    let sender = bind(&sandbox, "who-sender", &beta, "beta");

    send_as(&sandbox, &sender, &beta, "workspace:alpha", "routed unread");
    let pending_id = "20990916-035959-fade01";
    write_custom_mail(
        &sandbox.mail_root.join("alpha/inbox"),
        pending_id,
        &json!({
            "id": pending_id,
            "from": "beta",
            "to": "alpha",
            "kind": "note",
            "subject": "pending",
            "sent": "2026-09-16 03:59:59 -0500",
            "from_participant": sender,
            "address_kind": "workspace"
        }),
        "pending only",
    );

    let output = sandbox.run_as_participant(&["who"], &a, &alpha);
    assert_success(&output);
    let output: Value = from_stdout(&output);
    assert_eq!(output["participant"]["unread"]["workspace:alpha"], 1);
    assert_eq!(output["participant"]["pending"]["workspace:alpha"], 1);
    assert!(output["participant"].get("pending_total").is_none());
    for id in [&a, &b] {
        let listed = output["participants"]
            .as_array()
            .expect("participants")
            .iter()
            .find(|participant| participant["id"].as_str() == Some(id.as_str()))
            .expect("listed participant");
        assert_eq!(listed["unread"]["workspace:alpha"], 1);
        assert_eq!(listed["pending"]["workspace:alpha"], 1);
        assert!(listed.get("pending_total").is_none());
    }
}

#[test]
fn routing_every_structured_message_projection_exposes_both_reply_targets() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let sender = bind(&sandbox, "reply-sender", &alpha, "alpha");
    let recipient = bind(&sandbox, "reply-recipient", &beta, "beta");
    let marker = "reply-target-matrix";
    let sent = send_as(&sandbox, &sender, &alpha, "workspace:beta", marker);
    let mail_id = sent["envelope"]["id"].as_str().expect("mail id");

    let inbox = inbox_as(&sandbox, &recipient, &beta);
    let inbox_message = inbox["unread"]
        .as_array()
        .expect("unread")
        .iter()
        .find(|message| message["id"] == mail_id)
        .expect("inbox message");
    assert_reply_targets(inbox_message, &sender, "alpha");

    let read_peek =
        sandbox.run_as_participant(&["read", mail_id, "--peek", "--json"], &recipient, &beta);
    assert_success(&read_peek);
    let read_peek: Value = from_stdout(&read_peek);
    assert_reply_targets(&read_peek["envelope"], &sender, "alpha");

    let watch = sandbox.run_as_participant(&["watch", "--snapshot", "--json"], &recipient, &beta);
    assert_success(&watch);
    let watch_message: Value = String::from_utf8_lossy(&watch.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).expect("watch event JSON"))
        .find(|event: &Value| event["id"] == mail_id)
        .expect("watch mail event");
    assert_reply_targets(&watch_message, &sender, "alpha");

    let search =
        sandbox.run_as_participant(&["search", marker, "--mail", "--json"], &recipient, &beta);
    assert_success(&search);
    let search: Value = from_stdout(&search);
    let search_message = search["results"]
        .as_array()
        .expect("search results")
        .iter()
        .find(|message| message["id"] == mail_id)
        .expect("search mail result");
    assert_reply_targets(search_message, &sender, "alpha");

    for (participant, cwd) in [(&sender, &alpha), (&recipient, &beta)] {
        let joined = sandbox.run_as_participant(
            &["chat", "reply-matrix", "--join", "--json"],
            participant,
            cwd,
        );
        assert_success(&joined);
    }
    let chat_send = sandbox.run_as_participant(
        &[
            "chat",
            "reply-matrix",
            "--send",
            "--anyway",
            "--body",
            marker,
            "--json",
        ],
        &sender,
        &alpha,
    );
    assert_success(&chat_send);
    let chat_send: Value = from_stdout(&chat_send);
    let channel_id = chat_send["message"]["id"].as_str().expect("channel id");
    let chat = sandbox.run_as_participant(
        &["chat", "reply-matrix", "--peek", "--json"],
        &recipient,
        &beta,
    );
    assert_success(&chat);
    let chat: Value = from_stdout(&chat);
    let chat_message = chat["messages"]
        .as_array()
        .expect("chat messages")
        .iter()
        .find(|message| message["id"] == channel_id)
        .expect("chat message");
    assert_reply_targets(chat_message, &sender, "alpha");

    let catchup = sandbox.run_as_participant(&["catchup", "--mail", "--json"], &recipient, &beta);
    assert_success(&catchup);
    let catchup: Value = from_stdout(&catchup);
    let catchup_message = &catchup["targets"][0]["messages"][0]["envelope"];
    assert_eq!(catchup_message["id"], mail_id);
    assert_reply_targets(catchup_message, &sender, "alpha");

    let read = sandbox.run_as_participant(&["read", mail_id, "--json"], &recipient, &beta);
    assert_success(&read);
    let read: Value = from_stdout(&read);
    assert_eq!(read["already_read"], true);
    assert_reply_targets(&read["envelope"], &sender, "alpha");
}

#[test]
fn routing_crossed_send_uses_the_participants_seen_eligibility_snapshot() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let a = bind(&sandbox, "crossed-a", &alpha, "alpha");
    let b = bind(&sandbox, "crossed-b", &beta, "beta");
    for (participant, cwd) in [(&a, &alpha), (&b, &beta)] {
        let joined = sandbox.run_as_participant(
            &["chat", "eligibility-crossed", "--join", "--json"],
            participant,
            cwd,
        );
        assert_success(&joined);
    }

    let sent = sandbox.run_as_participant(
        &[
            "chat",
            "eligibility-crossed",
            "--send",
            "--anyway",
            "--body",
            "@beta please read",
            "--json",
        ],
        &a,
        &alpha,
    );
    assert_success(&sent);
    let consumed =
        sandbox.run_as_participant(&["chat", "eligibility-crossed", "--json"], &b, &beta);
    assert_success(&consumed);

    let reply = sandbox.run_as_participant(
        &[
            "chat",
            "eligibility-crossed",
            "--send",
            "--body",
            "handled",
            "--json",
        ],
        &b,
        &beta,
    );
    assert_success(&reply);
}

#[test]
fn routing_frozen_workspace_and_lineage_deliveries_survive_rebind_and_leave() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let recipient = bind(&sandbox, "history-recipient", &alpha, "alpha");
    let sender = bind(&sandbox, "history-sender", &beta, "beta");

    let workspace = send_as(&sandbox, &sender, &beta, "workspace:alpha", "old workspace");
    let workspace_id = workspace["envelope"]["id"].as_str().expect("workspace id");
    let rebound = sandbox.run_as_participant(
        &["participant", "bind", "--workspace", "beta", "--json"],
        &recipient,
        &beta,
    );
    assert_success(&rebound);
    let after_rebind = inbox_as(&sandbox, &recipient, &beta);
    assert!(after_rebind["unread"]
        .as_array()
        .expect("rebound unread")
        .iter()
        .any(|message| message["id"] == workspace_id));

    patch_participant(&sandbox, &recipient, |record| {
        record["lineage"] = json!("ember");
        record["lineage_since"] = json!("2026-09-16T08:00:00Z");
    });
    let lineage = sandbox.mail_root.join("lineages/ember");
    fs::create_dir_all(&lineage).expect("lineage directory");
    fs::write(
        lineage.join("lineage.json"),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&json!({
                "version": 1,
                "name": "ember",
                "founder": recipient,
                "created": "2026-09-16T08:00:00Z",
                "host": "test"
            }))
            .expect("lineage JSON")
        ),
    )
    .expect("lineage record");
    let lineage_mail = send_as(&sandbox, &sender, &beta, "lineage:ember", "old lineage");
    let lineage_id = lineage_mail["envelope"]["id"].as_str().expect("lineage id");
    patch_participant(&sandbox, &recipient, |record| {
        record["lineage"] = Value::Null;
        record["lineage_since"] = Value::Null;
    });
    let after_leave = inbox_as(&sandbox, &recipient, &beta);
    assert!(after_leave["unread"]
        .as_array()
        .expect("post-leave unread")
        .iter()
        .any(|message| message["id"] == lineage_id));
    let read = sandbox.run_as_participant(&["read", lineage_id, "--json"], &recipient, &beta);
    assert_success(&read);
}

#[test]
fn routing_receipt_digest_mismatch_fails_inbox_catchup_and_read_consistently() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let recipient = bind(&sandbox, "digest-recipient", &alpha, "alpha");
    let sender = bind(&sandbox, "digest-sender", &beta, "beta");
    let sent = send_as(&sandbox, &sender, &beta, "workspace:alpha", "before tamper");
    let id = sent["envelope"]["id"].as_str().expect("mail id");
    let canonical = sandbox.mail_root.join(format!("alpha/inbox/{id}.mail"));
    fs::write(
        &canonical,
        fs::read_to_string(&canonical).unwrap() + "tamper",
    )
    .expect("tamper");

    for args in [
        vec!["inbox", "--json"],
        vec!["catchup", "--mail", "--json"],
        vec!["read", id, "--json"],
    ] {
        let output = sandbox.run_as_participant(&args, &recipient, &alpha);
        assert_eq!(
            output.status.code(),
            Some(78),
            "{args:?}: {}",
            common::stderr(&output)
        );
        let error: post::output::ErrorEnvelope = common::from_stderr(&output);
        assert_eq!(error.error.code, "config_invalid");
        assert!(error.error.message.contains("digest"));
    }
}

#[test]
fn routing_pending_is_labeled_and_excluded_from_unread_across_read_and_watch() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let recipient = bind(&sandbox, "pending-recipient", &alpha, "alpha");
    let sender = bind(&sandbox, "pending-sender", &beta, "beta");
    let id = "20990916-041000-fade10";
    write_custom_mail(
        &sandbox.mail_root.join("alpha/inbox"),
        id,
        &json!({"id":id,"from":"beta","to":"alpha","kind":"note","subject":"pending","sent":"2026-09-16 04:10:00 -0500","from_participant":sender,"address_kind":"workspace"}),
        "pending body",
    );

    let inbox = inbox_as(&sandbox, &recipient, &alpha);
    assert_eq!(inbox["unread_count"], 0);
    assert_eq!(inbox["pending_by_address"]["workspace:alpha"], 1);
    let peek = sandbox.run_as_participant(&["read", id, "--peek", "--json"], &recipient, &alpha);
    assert_success(&peek);
    let peek: Value = from_stdout(&peek);
    assert_eq!(peek["pending"], true);
    let watch = sandbox.run_as_participant(&["watch", "--snapshot"], &recipient, &alpha);
    assert_success(&watch);
    let event: Value = serde_json::from_slice(&watch.stdout).expect("one watch event");
    assert_eq!(event["pending"], true);
    assert_eq!(event["address"], json!({"kind":"workspace","name":"alpha"}));
}

#[test]
fn routing_explicit_own_read_succeeds_without_changing_unread() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let sender = bind(&sandbox, "own-reader", &alpha, "alpha");
    let sibling = bind(&sandbox, "own-sibling", &alpha, "alpha");
    let sent = send_as(&sandbox, &sender, &alpha, "workspace:alpha", "inspect mine");
    let id = sent["envelope"]["id"].as_str().expect("mail id");
    assert_eq!(inbox_as(&sandbox, &sender, &alpha)["unread_count"], 0);
    assert_eq!(inbox_as(&sandbox, &sibling, &alpha)["unread_count"], 1);
    let read = sandbox.run_as_participant(&["read", id, "--json"], &sender, &alpha);
    assert_success(&read);
    let read: Value = from_stdout(&read);
    assert_eq!(read["own"], true);
    assert_eq!(inbox_as(&sandbox, &sender, &alpha)["unread_count"], 0);
    assert_eq!(inbox_as(&sandbox, &sibling, &alpha)["unread_count"], 1);
    assert!(!sandbox
        .mail_root
        .join("participants")
        .join(sender)
        .join("cursors.json")
        .exists());
}

#[test]
fn routing_participant_watch_ignores_legacy_own_room_suppression() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let a = bind(&sandbox, "own-watch-a", &alpha, "alpha");
    let b = bind(&sandbox, "own-watch-b", &alpha, "alpha");
    for participant in [&a, &b] {
        let joined = sandbox.run_as_participant(
            &["chat", "siblings", "--join", "--json"],
            participant,
            &alpha,
        );
        assert_success(&joined);
    }
    let sent = sandbox.run_as_participant(
        &[
            "chat", "siblings", "--send", "--anyway", "--body", "sibling", "--json",
        ],
        &a,
        &alpha,
    );
    assert_success(&sent);
    let sent: Value = from_stdout(&sent);
    let id = sent["message"]["id"].as_str().expect("channel id");
    let watched =
        sandbox.run_as_participant(&["watch", "--snapshot", "--own", "alpha"], &b, &alpha);
    assert_success(&watched);
    assert!(String::from_utf8_lossy(&watched.stdout).contains(id));
}

#[test]
fn routing_chat_ack_recovers_pending_workspace_mail_before_consuming_channel() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let a = bind(&sandbox, "ack-a", &alpha, "alpha");
    let b = bind(&sandbox, "ack-b", &beta, "beta");
    for (participant, cwd) in [(&a, &alpha), (&b, &beta)] {
        assert_success(&sandbox.run_as_participant(
            &["chat", "ack-route", "--join", "--json"],
            participant,
            cwd,
        ));
    }
    let channel = sandbox.run_as_participant(
        &[
            "chat",
            "ack-route",
            "--send",
            "--anyway",
            "--body",
            "ack me",
            "--json",
        ],
        &b,
        &beta,
    );
    assert_success(&channel);
    let channel: Value = from_stdout(&channel);
    let channel_id = channel["message"]["id"].as_str().expect("channel id");
    let pending_id = "20990916-042000-fade20";
    write_custom_mail(
        &sandbox.mail_root.join("alpha/inbox"),
        pending_id,
        &json!({"id":pending_id,"from":"beta","to":"alpha","kind":"note","subject":"recover","sent":"2026-09-16 04:20:00 -0500","from_participant":b,"address_kind":"workspace"}),
        "recover",
    );
    let ack = sandbox.run_as_participant(
        &["chat", "ack-route", "--ack", channel_id, "--json"],
        &a,
        &alpha,
    );
    assert_success(&ack);
    assert!(sandbox
        .mail_root
        .join(format!("alpha/routing/{pending_id}.json"))
        .is_file());
}

#[test]
fn routing_blocked_pending_is_held_without_stalling_catchup_or_bind() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let recipient = bind(&sandbox, "blocked-recipient", &alpha, "alpha");
    let sender = bind(&sandbox, "blocked-sender", &beta, "beta");
    fs::write(
        sandbox.mail_root.join("rules.json"),
        r#"{"blocked":[{"from":"beta","to":"alpha","reason":"hold"}]}"#,
    )
    .expect("blocked rule");
    let id = "20990916-043000-fade30";
    write_custom_mail(
        &sandbox.mail_root.join("alpha/inbox"),
        id,
        &json!({"id":id,"from":"beta","to":"alpha","kind":"note","subject":"held","sent":"2026-09-16 04:30:00 -0500","from_participant":sender,"address_kind":"workspace"}),
        "held",
    );
    let inbox = inbox_as(&sandbox, &recipient, &alpha);
    assert_eq!(inbox["pending_by_address"]["workspace:alpha"], 0);
    let catchup = sandbox.run_as_participant(&["catchup", "--mail", "--json"], &recipient, &alpha);
    assert!(catchup.status.success());
    assert!(common::stderr(&catchup).contains("left unroutable mail"));
    assert!(common::stderr(&catchup).contains("hold"));
    assert!(!sandbox
        .mail_root
        .join(format!("alpha/routing/{id}.json"))
        .exists());
    let rebound = sandbox.run_as_participant(
        &["participant", "bind", "--workspace", "alpha", "--json"],
        &recipient,
        &alpha,
    );
    assert!(rebound.status.success());
    assert!(common::stderr(&rebound).contains("left unroutable mail"));
}

#[test]
fn routing_workspace_less_chat_state_stays_under_participant_directory() {
    let sandbox = Sandbox::new();
    let actor = sandbox.run(&[
        "participant",
        "bind",
        "--harness",
        "shell",
        "--key",
        "no-workspace-chat",
        "--json",
    ]);
    assert_success(&actor);
    let actor: Value = from_stdout(&actor);
    let id = actor["participant"]["id"]
        .as_str()
        .expect("actor id")
        .to_owned();
    assert!(actor["participant"]["workspace"].is_null());
    assert_success(&sandbox.run_as_participant(
        &["chat", "session-only", "--join", "--json"],
        &id,
        &sandbox.path,
    ));
    let other = sandbox.test_participant("claude-space");
    fs::create_dir_all(sandbox.home.join("claude-space")).expect("other workspace");
    assert_success(&sandbox.run_as_participant(
        &["chat", "session-only", "--join", "--json"],
        &other,
        &sandbox.home.join("claude-space"),
    ));
    assert_success(&sandbox.run_as_participant(
        &[
            "chat",
            "session-only",
            "--send",
            "--anyway",
            "--body",
            "hello",
            "--json",
        ],
        &other,
        &sandbox.home.join("claude-space"),
    ));
    let read = sandbox.run_as_participant(
        &["chat", "session-only", "--framing", "compact"],
        &id,
        &sandbox.path,
    );
    assert_success(&read);
    assert!(!sandbox.mail_root.join(&id).exists());
    assert!(sandbox
        .mail_root
        .join("participants")
        .join(&id)
        .join("cursors.json")
        .exists());
}

#[test]
fn routing_skips_corrupt_participant_and_delivers_to_valid_sibling() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let valid = bind(&sandbox, "valid-sibling", &alpha, "alpha");
    let sender = bind(&sandbox, "corrupt-sender", &beta, "beta");
    let corrupt = sandbox.mail_root.join("participants/claude-deadbeef");
    fs::create_dir_all(&corrupt).expect("corrupt participant dir");
    fs::write(corrupt.join("participant.json"), b"{not json").expect("corrupt participant");
    let sent = sandbox.run_as_participant(
        &[
            "send",
            "--to",
            "workspace:alpha",
            "--body",
            "still routes",
            "--json",
        ],
        &sender,
        &beta,
    );
    assert!(sent.status.success());
    assert!(common::stderr(&sent).contains("skipped corrupt participant"));
    let sent: Value = from_stdout(&sent);
    let id = sent["envelope"]["id"].as_str().expect("mail id");
    let receipt: Value = serde_json::from_slice(
        &fs::read(sandbox.mail_root.join(format!("alpha/routing/{id}.json"))).unwrap(),
    )
    .unwrap();
    assert!(receipt["recipients"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value == &valid));
}

#[test]
fn routing_two_participants_consume_independently_and_canonical_file_stays_put() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let a = bind(&sandbox, "routing-a", &alpha, "alpha");
    let b = bind(&sandbox, "routing-b", &alpha, "alpha");
    let c = bind(&sandbox, "routing-c", &beta, "beta");

    let sent = send_as(&sandbox, &c, &beta, "workspace:alpha", "third-party");
    let id = sent["envelope"]["id"].as_str().expect("mail id");
    assert_eq!(inbox_as(&sandbox, &a, &alpha)["unread_count"], 1);
    assert_eq!(inbox_as(&sandbox, &b, &alpha)["unread_count"], 1);

    let read_a = sandbox.run_as_participant(&["read", id, "--json"], &a, &alpha);
    assert_success(&read_a);
    assert_eq!(inbox_as(&sandbox, &a, &alpha)["unread_count"], 0);
    assert_eq!(inbox_as(&sandbox, &b, &alpha)["unread_count"], 1);
    assert!(sandbox
        .mail_root
        .join(format!("alpha/inbox/{id}.mail"))
        .is_file());

    let read_b = sandbox.run_as_participant(&["read", id, "--json"], &b, &alpha);
    assert_success(&read_b);
    assert_eq!(inbox_as(&sandbox, &b, &alpha)["unread_count"], 0);

    let receipt: Value = serde_json::from_slice(
        &fs::read(sandbox.mail_root.join(format!("alpha/routing/{id}.json")))
            .expect("routing receipt"),
    )
    .expect("receipt JSON");
    let recipients = receipt["recipients"].as_array().expect("recipients");
    assert!(recipients.iter().any(|value| value == &a));
    assert!(recipients.iter().any(|value| value == &b));

    let sent = send_as(&sandbox, &a, &alpha, "workspace:alpha", "sibling");
    let sibling = sent["envelope"]["id"].as_str().expect("sibling id");
    assert_eq!(inbox_as(&sandbox, &a, &alpha)["unread_count"], 0);
    assert_eq!(inbox_as(&sandbox, &b, &alpha)["unread_count"], 1);
    assert!(sandbox
        .mail_root
        .join(format!("alpha/inbox/{sibling}.mail"))
        .is_file());
}

#[test]
fn routing_missing_receipt_is_pending_until_writer_recovers_it() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let a = bind(&sandbox, "recovery-a", &alpha, "alpha");
    let c = bind(&sandbox, "recovery-c", &beta, "beta");
    let id = "20990916-030000-aaa111";
    let inbox = sandbox.mail_root.join("alpha/inbox");
    fs::create_dir_all(&inbox).expect("workspace inbox");
    write_custom_mail(
        &inbox,
        id,
        &json!({
            "id": id,
            "from": "beta",
            "to": "alpha",
            "kind": "note",
            "subject": "pending",
            "sent": "2026-09-16 03:00:00 -0500",
            "from_participant": c,
            "address_kind": "workspace"
        }),
        "recover me",
    );
    let receipt = sandbox.mail_root.join(format!("alpha/routing/{id}.json"));
    let listed = inbox_as(&sandbox, &a, &alpha);
    assert_eq!(listed["pending"], 1);
    assert!(
        !receipt.exists(),
        "display-only inbox must not publish routing"
    );

    let read = sandbox.run_as_participant(&["read", id, "--json"], &a, &alpha);
    assert_success(&read);
    assert!(receipt.is_file(), "consuming read recovers missing receipt");
}

#[test]
fn routing_display_only_forms_leave_routing_and_cursor_tree_untouched() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let a = bind(&sandbox, "display-a", &alpha, "alpha");
    let c = bind(&sandbox, "display-c", &beta, "beta");
    let id = "20990916-030100-aaa112";
    let inbox = sandbox.mail_root.join("alpha/inbox");
    fs::create_dir_all(&inbox).expect("workspace inbox");
    write_custom_mail(
        &inbox,
        id,
        &json!({
            "id": id,
            "from": "beta",
            "to": "alpha",
            "kind": "note",
            "subject": "pending",
            "sent": "2026-09-16 03:01:00 -0500",
            "from_participant": c,
            "address_kind": "workspace"
        }),
        "display only",
    );
    let before = tree(&sandbox.mail_root);
    for args in [
        vec!["inbox"],
        vec!["read", id, "--peek", "--json"],
        vec!["watch", "--snapshot", "--json"],
        vec!["channels"],
        vec!["search", "display", "--mail", "--json"],
    ] {
        let output = sandbox.run_as_participant(&args, &a, &alpha);
        assert_success(&output);
    }
    assert_eq!(tree(&sandbox.mail_root), before);
}

#[test]
fn routing_exact_seen_sets_leave_a_late_older_id_unread() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let a = bind(&sandbox, "late-a", &alpha, "alpha");
    let c = bind(&sandbox, "late-c", &beta, "beta");
    let inbox = sandbox.mail_root.join("alpha/inbox");
    fs::create_dir_all(&inbox).expect("workspace inbox");
    let newer = "20990916-030300-bbb222";
    write_custom_mail(
        &inbox,
        newer,
        &json!({"id":newer,"from":"beta","to":"alpha","kind":"note","subject":"newer","sent":"2026-09-16 03:03:00 -0500","from_participant":c,"address_kind":"workspace"}),
        "newer",
    );
    let read = sandbox.run_as_participant(&["read", newer, "--json"], &a, &alpha);
    assert_success(&read);

    let older = "20990916-030200-aaa111";
    write_custom_mail(
        &inbox,
        older,
        &json!({"id":older,"from":"beta","to":"alpha","kind":"note","subject":"older","sent":"2026-09-16 03:02:00 -0500","from_participant":c,"address_kind":"workspace"}),
        "older late arrival",
    );
    let listed = inbox_as(&sandbox, &a, &alpha);
    assert_eq!(listed["unread_count"], 0, "unrouted older mail is pending");
    assert_eq!(listed["pending"], 1);
    let read = sandbox.run_as_participant(&["read", older, "--json"], &a, &alpha);
    assert_success(&read);
    let read: Value = from_stdout(&read);
    assert!(
        read.get("already_read").is_none(),
        "late older id must be fresh: {read}"
    );
    let listed = inbox_as(&sandbox, &a, &alpha);
    assert_eq!(listed["unread_count"], 0);
}

#[test]
fn routing_lifecycle_excludes_stale_and_ended_fanout_but_not_participant_target() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let active = bind(&sandbox, "life-active", &alpha, "alpha");
    let stale = bind(&sandbox, "life-stale", &alpha, "alpha");
    let ended = bind(&sandbox, "life-ended", &alpha, "alpha");
    let sender = bind(&sandbox, "life-sender", &beta, "beta");
    patch_participant(&sandbox, &stale, |record| {
        record["last_seen"] = json!("2000-01-01T00:00:00Z");
        record["lease_hours"] = json!(1);
    });
    patch_participant(&sandbox, &ended, |record| {
        record["ended_at"] = json!("2026-09-16T08:00:00Z");
    });

    let sent = send_as(&sandbox, &sender, &beta, "workspace:alpha", "active only");
    let id = sent["envelope"]["id"].as_str().expect("workspace id");
    let receipt: Value = serde_json::from_slice(
        &fs::read(sandbox.mail_root.join(format!("alpha/routing/{id}.json")))
            .expect("workspace receipt"),
    )
    .expect("workspace receipt JSON");
    let recipients = receipt["recipients"].as_array().expect("recipients");
    assert!(recipients.iter().any(|value| value == &active));
    assert!(!recipients.iter().any(|value| value == &stale));
    assert!(!recipients.iter().any(|value| value == &ended));
    patch_participant(&sandbox, &active, |record| {
        record["ended_at"] = json!("2026-09-16T08:01:00Z");
    });
    let frozen_read = sandbox.run_as_participant(&["read", id, "--json"], &active, &alpha);
    assert_success(&frozen_read);

    let direct = send_as(
        &sandbox,
        &sender,
        &beta,
        &format!("participant:{ended}"),
        "durable direct",
    );
    let direct_id = direct["envelope"]["id"].as_str().expect("direct id");
    let direct_receipt: Value = serde_json::from_slice(
        &fs::read(
            sandbox
                .mail_root
                .join(format!("participants/{ended}/routing/{direct_id}.json")),
        )
        .expect("direct receipt"),
    )
    .expect("direct receipt JSON");
    assert_eq!(direct_receipt["recipients"], json!([ended]));
}

#[test]
fn routing_sender_only_workspace_stays_pending_until_another_participant_arrives() {
    let sandbox = Sandbox::new();
    let solo = sandbox.path.join("solo");
    fs::create_dir_all(&solo).expect("solo workspace");
    let rooms_path = sandbox.mail_root.join("rooms.json");
    let mut rooms: Value =
        serde_json::from_slice(&fs::read(&rooms_path).expect("rooms")).expect("rooms JSON");
    rooms["solo"] = json!(solo.display().to_string());
    fs::write(
        &rooms_path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&rooms).expect("serialize rooms")
        ),
    )
    .expect("write rooms");
    let a = bind(&sandbox, "solo-a", &solo, "solo");
    let sent = send_as(&sandbox, &a, &solo, "workspace:solo", "wait for sibling");
    let id = sent["envelope"]["id"].as_str().expect("solo id");
    let receipt = sandbox.mail_root.join(format!("solo/routing/{id}.json"));
    assert!(!receipt.exists(), "sender-only fanout must remain pending");

    let b = bind(&sandbox, "solo-b", &solo, "solo");
    assert!(
        receipt.is_file(),
        "second participant bind must route pending mail"
    );
    let read = sandbox.run_as_participant(&["read", id, "--json"], &b, &solo);
    assert_success(&read);
    assert!(receipt.is_file());
}

#[test]
fn routing_lineage_receipt_names_blocked_exclusion_and_allowed_affiliate() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let allowed_path = sandbox.home.join("claude-space");
    let blocked = bind(&sandbox, "lineage-blocked", &alpha, "alpha");
    let allowed = bind(&sandbox, "lineage-allowed", &allowed_path, "claude-space");
    let sender = bind(&sandbox, "lineage-sender", &beta, "beta");
    for id in [&blocked, &allowed] {
        patch_participant(&sandbox, id, |record| {
            record["lineage"] = json!("ember");
            record["lineage_since"] = json!("2026-09-16T08:00:00Z");
        });
    }
    let lineage = sandbox.mail_root.join("lineages/ember");
    fs::create_dir_all(&lineage).expect("lineage directory");
    fs::write(
        lineage.join("lineage.json"),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&json!({
                "name": "ember",
                "founder": blocked,
                "created": "2026-09-16T08:00:00Z",
                "host": "test"
            }))
            .expect("lineage JSON")
        ),
    )
    .expect("write lineage");
    fs::write(
        sandbox.mail_root.join("rules.json"),
        r#"{"blocked":[{"from":"beta","to":"alpha","reason":"lineage block"}]}"#,
    )
    .expect("blocked rule");

    let sent = send_as(&sandbox, &sender, &beta, "lineage:ember", "lineage fanout");
    let id = sent["envelope"]["id"].as_str().expect("lineage id");
    let receipt: Value = serde_json::from_slice(
        &fs::read(lineage.join(format!("routing/{id}.json"))).expect("lineage receipt"),
    )
    .expect("lineage receipt JSON");
    assert_eq!(receipt["recipients"], json!([allowed]));
    assert_eq!(
        receipt["excluded"],
        json!([{"participant": blocked, "reason": "blocked-route"}])
    );
}

#[test]
fn routing_workspace_less_participants_use_explicit_channel_membership() {
    let sandbox = Sandbox::new();
    let a = sandbox.bind_claude("session-only-a", &sandbox.path, None)["id"]
        .as_str()
        .expect("A id")
        .to_owned();
    let b = sandbox.bind_claude("session-only-b", &sandbox.path, None)["id"]
        .as_str()
        .expect("B id")
        .to_owned();
    for participant in [&a, &b] {
        let joined = sandbox.run_as_participant(
            &["chat", "session-only", "--join", "--json"],
            participant,
            &sandbox.path,
        );
        assert_success(&joined);
    }
    let sent = sandbox.run_as_participant(
        &[
            "chat",
            "session-only",
            "--send",
            "--body",
            "session-only message",
            "--json",
        ],
        &a,
        &sandbox.path,
    );
    assert_success(&sent);
    let peek = sandbox.run_as_participant(&["chat", "session-only", "--json"], &b, &sandbox.path);
    assert_success(&peek);
    let peek: Value = from_stdout(&peek);
    assert!(peek["count"].as_u64().is_some_and(|count| count >= 1));
    assert!(peek["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .any(|message| message["body"] == "session-only message"));
    let cursor = sandbox
        .mail_root
        .join("participants")
        .join(&b)
        .join("cursors.json");
    let before = fs::read(&cursor).expect("B cursor before leave");
    let left = sandbox.run_as_participant(
        &["chat", "session-only", "--leave", "--json"],
        &b,
        &sandbox.path,
    );
    assert_success(&left);
    assert_eq!(fs::read(&cursor).expect("B cursor after leave"), before);
    let still_member = sandbox.run_as_participant(
        &[
            "chat",
            "session-only",
            "--send",
            "--body",
            "A still posts",
            "--json",
        ],
        &a,
        &sandbox.path,
    );
    assert_success(&still_member);
    let refused = sandbox.run_as_participant(
        &[
            "chat",
            "session-only",
            "--send",
            "--body",
            "B cannot post",
            "--json",
        ],
        &b,
        &sandbox.path,
    );
    assert_eq!(refused.status.code(), Some(65));
}

#[test]
fn routing_lineage_mail_waits_for_adopt_and_excludes_later_affiliate() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let adopter = bind(&sandbox, "adopt-a", &alpha, "alpha");
    let later = bind(&sandbox, "adopt-later", &alpha, "alpha");
    let sender = bind(&sandbox, "adopt-sender", &beta, "beta");
    let lineage = sandbox.mail_root.join("lineages/ember");
    fs::create_dir_all(&lineage).expect("lineage directory");
    fs::write(
        lineage.join("lineage.json"),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&json!({
                "name": "ember",
                "founder": adopter,
                "created": "2026-09-16T08:00:00Z",
                "host": "test"
            }))
            .expect("lineage JSON")
        ),
    )
    .expect("write lineage");
    let sent = send_as(
        &sandbox,
        &sender,
        &beta,
        "lineage:ember",
        "held lineage mail",
    );
    let id = sent["envelope"]["id"].as_str().expect("lineage id");
    let receipt = lineage.join(format!("routing/{id}.json"));
    assert!(!receipt.exists());

    patch_participant(&sandbox, &adopter, |record| {
        record["lineage"] = json!("ember");
        record["lineage_since"] = json!("2026-09-16T08:01:00Z");
    });
    let adopted = sandbox.run_as_participant(&["inbox", "--adopt"], &adopter, &alpha);
    assert_success(&adopted);
    let frozen: Value = serde_json::from_slice(&fs::read(&receipt).expect("adopt receipt"))
        .expect("adopt receipt JSON");
    assert_eq!(frozen["recipients"], json!([adopter]));

    patch_participant(&sandbox, &later, |record| {
        record["lineage"] = json!("ember");
        record["lineage_since"] = json!("2026-09-16T08:02:00Z");
    });
    assert_eq!(inbox_as(&sandbox, &later, &alpha)["unread_count"], 0);
}

fn patch_participant(sandbox: &Sandbox, id: &str, patch: impl FnOnce(&mut Value)) {
    let path = sandbox
        .mail_root
        .join("participants")
        .join(id)
        .join("participant.json");
    let mut record: Value = serde_json::from_slice(&fs::read(&path).expect("participant record"))
        .expect("participant JSON");
    patch(&mut record);
    fs::write(
        path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&record).expect("serialize participant")
        ),
    )
    .expect("write participant record");
}

fn tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, current: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        let Ok(entries) = fs::read_dir(current) else {
            return;
        };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|entry| entry.path());
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else if path.is_file() {
                out.insert(
                    path.strip_prefix(root)
                        .expect("relative path")
                        .to_path_buf(),
                    fs::read(path).expect("read tree file"),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}
