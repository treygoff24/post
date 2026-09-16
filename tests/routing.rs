mod common;

use common::{
    assert_success, from_stdout, register_alpha_beta, register_room, write_channel_message,
    write_custom_mail, Sandbox,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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

fn printed_readback(text: &str) -> &str {
    text.lines()
        .find_map(|line| line.strip_prefix("post: read it back with: "))
        .expect("send text readback command")
}

fn run_printed_readback(
    sandbox: &Sandbox,
    command: &str,
    participant: &str,
    cwd: &Path,
) -> std::process::Output {
    let script = command.replacen("post ", &format!("'{}' ", env!("CARGO_BIN_EXE_post")), 1);
    Command::new("sh")
        .arg("-c")
        .arg(script)
        .current_dir(cwd)
        .env("HOME", &sandbox.home)
        .env("POST_MAIL_ROOT", &sandbox.mail_root)
        .env("POST_PARTICIPANT", participant)
        .env_remove("POST_FROM")
        .env_remove("POST_SENDER_ADDRESS")
        .env_remove("POST_ARX_GENERATION")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run printed readback")
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

    let remote = sandbox.mail_root.join("remote/peer-host/remote-room");
    fs::create_dir_all(&remote).expect("remote placeholder");
    let rooms_path = sandbox.mail_root.join("rooms.json");
    let mut rooms: Value = serde_json::from_slice(&fs::read(&rooms_path).expect("rooms registry"))
        .expect("rooms JSON");
    rooms["remote-room"] = json!(remote);
    fs::write(
        &rooms_path,
        format!("{}\n", serde_json::to_string_pretty(&rooms).unwrap()),
    )
    .expect("register remote placeholder");
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

    let missing_remote = sandbox.mail_root.join("remote/peer-host/missing-remote");
    let mut rooms: Value = serde_json::from_slice(&fs::read(&rooms_path).expect("rooms registry"))
        .expect("rooms JSON");
    rooms["missing-remote"] = json!(missing_remote);
    fs::write(
        &rooms_path,
        format!("{}\n", serde_json::to_string_pretty(&rooms).unwrap()),
    )
    .expect("register missing remote placeholder");
    let missing = "20990916-040102-aa0003";
    write_custom_mail(
        &inbox,
        missing,
        &json!({"id":missing,"from":"missing-remote","to":"alpha","kind":"note","subject":"missing remote","sent":"2026-09-16 04:01:02 -0500","from_participant":colliding_local,"address_kind":"workspace"}),
        "missing remote",
    );
    let missing_read =
        sandbox.run_as_participant(&["read", missing, "--peek", "--json"], &recipient, &alpha);
    assert_success(&missing_read);
    let missing_read: Value = from_stdout(&missing_read);
    assert_eq!(missing_read["envelope"]["origin"], "remote");
    assert!(missing_read["envelope"]["reply_to_participant"].is_null());
    assert_eq!(
        missing_read["envelope"]["reply_to_shared"],
        "missing-remote"
    );
}

#[test]
fn routing_remote_channel_slice_json_exposes_contextual_reply_metadata() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let actor = bind(&sandbox, "remote-slice-actor", &alpha, "alpha");
    assert_success(&sandbox.run_as_participant(
        &["chat", "tax", "--join", "--json"],
        &actor,
        &alpha,
    ));
    let remote = sandbox
        .mail_root
        .join("remote")
        .join("peer-host")
        .join("remote-workspace");
    fs::create_dir_all(&remote).expect("remote placeholder");
    assert_success(&sandbox.run(&[
        "rooms",
        "add",
        "remote-workspace",
        remote.to_string_lossy().as_ref(),
    ]));
    let id = "20990916-051100-000001-acde11";
    let messages = sandbox.mail_root.join("channels/tax/messages");
    fs::create_dir_all(&messages).expect("channel messages");
    fs::write(
        messages.join(format!("{id}.msg")),
        format!(
            "{}\n---\nremote slice body",
            json!({
                "id": id,
                "from": "remote-workspace",
                "channel": "tax",
                "subject": "remote slice",
                "sent": "2026-09-16 05:11:00 -0500",
                "from_participant": actor,
                "address_kind": "channel",
                "sender_provenance": "participant-binding"
            })
        ),
    )
    .expect("write remote channel message");

    let slice = sandbox.run_as_participant(
        &[
            "chat",
            "tax",
            "--message",
            id,
            "--offset",
            "0",
            "--length",
            "6",
            "--max-bytes",
            "4000",
            "--json",
        ],
        &actor,
        &alpha,
    );
    assert_success(&slice);
    let slice: Value = from_stdout(&slice);
    assert_eq!(slice["origin"], "remote");
    assert!(slice.get("reply_to_participant").is_none());
    assert_eq!(slice["reply_to_shared"], "remote-workspace");
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
fn routing_digest_mismatch_isolates_one_message_and_explicit_read_fails_closed() {
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
    let good = send_as(&sandbox, &sender, &beta, "workspace:alpha", "good sibling");
    let good_id = good["envelope"]["id"].as_str().expect("good id");

    let inbox = sandbox.run_as_participant(&["inbox", "--json"], &recipient, &alpha);
    assert!(inbox.status.success(), "{}", common::stderr(&inbox));
    assert!(common::stderr(&inbox).contains("digest mismatch"));
    let inbox: Value = from_stdout(&inbox);
    assert_eq!(inbox["skipped_unreadable"], 1);
    assert!(inbox["unread"]
        .as_array()
        .expect("unread")
        .iter()
        .any(|item| item["id"] == good_id));

    for args in [
        vec!["watch", "--snapshot"],
        vec!["search", "good sibling", "--mail", "--json"],
    ] {
        let output = sandbox.run_as_participant(&args, &recipient, &alpha);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            common::stderr(&output)
        );
        assert!(common::stderr(&output).contains("digest mismatch"));
        assert!(common::stdout(&output).contains(good_id));
    }
    let who = sandbox.run_as_participant(&["who"], &recipient, &alpha);
    assert!(who.status.success(), "{}", common::stderr(&who));
    assert!(common::stderr(&who).contains("digest mismatch"));
    let catchup = sandbox.run_as_participant(&["catchup", "--mail", "--json"], &recipient, &alpha);
    assert!(catchup.status.success(), "{}", common::stderr(&catchup));
    assert!(common::stderr(&catchup).contains("digest mismatch"));
    assert!(common::stdout(&catchup).contains(good_id));

    let explicit = sandbox.run_as_participant(&["read", id, "--json"], &recipient, &alpha);
    assert_eq!(explicit.status.code(), Some(78));
    let error: post::output::ErrorEnvelope = common::from_stderr(&explicit);
    assert_eq!(error.error.code, "config_invalid");
    assert!(error.error.message.contains("digest"));
}

#[test]
fn routing_corrupt_receipt_isolated_from_other_workspaces_and_bind() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let gamma = sandbox.path.join("gamma");
    fs::create_dir(&gamma).expect("gamma workspace");
    register_room(&sandbox, "gamma", &gamma);
    let gamma_actor = bind(&sandbox, "gamma-reader", &gamma, "gamma");

    let bad_id = "20990916-043100-acde01";
    write_custom_mail(
        &sandbox.mail_root.join("alpha/inbox"),
        bad_id,
        &json!({"id":bad_id,"from":"beta","to":"alpha","kind":"note","subject":"bad receipt","sent":"2026-09-16 04:31:00 -0500","address_kind":"workspace"}),
        "bad receipt",
    );
    let routing = sandbox.mail_root.join("alpha/routing");
    fs::create_dir_all(&routing).expect("routing directory");
    fs::write(routing.join(format!("{bad_id}.json")), b"{corrupt").expect("corrupt receipt");

    for args in [
        vec!["inbox"],
        vec!["watch", "--snapshot"],
        vec!["catchup", "--mail", "--json"],
        vec!["channels"],
    ] {
        let output = sandbox.run_as_participant(&args, &gamma_actor, &gamma);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            common::stderr(&output)
        );
        assert!(common::stderr(&output).contains("corrupt routing receipt"));
    }
    let rebound = sandbox.run_as_claude(
        &["participant", "bind", "--workspace", "alpha", "--json"],
        "corrupt-receipt-sibling",
        &alpha,
    );
    assert!(rebound.status.success(), "{}", common::stderr(&rebound));
    assert!(common::stderr(&rebound).contains("corrupt routing receipt"));
}

#[test]
fn routing_watch_isolates_corrupt_receipt_in_the_watched_workspace() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let recipient = bind(&sandbox, "same-address-recipient", &alpha, "alpha");
    let sender = bind(&sandbox, "same-address-sender", &beta, "beta");
    let bad = send_as(
        &sandbox,
        &sender,
        &beta,
        "workspace:alpha",
        "corrupt receipt sibling",
    );
    let bad_id = bad["envelope"]["id"].as_str().expect("bad id");
    let good = send_as(
        &sandbox,
        &sender,
        &beta,
        "workspace:alpha",
        "healthy receipt sibling",
    );
    let good_id = good["envelope"]["id"].as_str().expect("good id");
    fs::write(
        sandbox
            .mail_root
            .join(format!("alpha/routing/{bad_id}.json")),
        b"{corrupt",
    )
    .expect("corrupt watched receipt");

    let watched = sandbox.run_as_participant(&["watch", "--snapshot"], &recipient, &alpha);
    assert!(watched.status.success(), "{}", common::stderr(&watched));
    assert!(common::stderr(&watched).contains("corrupt routing receipt"));
    let events = common::stdout(&watched)
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("watch event"))
        .collect::<Vec<_>>();
    assert!(events.iter().any(|event| event["id"] == good_id));
    assert!(events
        .iter()
        .any(|event| event["event"] == "unreadable" && event["id"] == bad_id));
}

#[test]
fn routing_corrupt_participant_channels_do_not_break_other_members() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let corrupt = bind(&sandbox, "corrupt-channels", &alpha, "alpha");
    let healthy = bind(&sandbox, "healthy-channels", &beta, "beta");
    fs::write(
        sandbox
            .mail_root
            .join("participants")
            .join(&corrupt)
            .join("channels.json"),
        b"{corrupt",
    )
    .expect("corrupt channels state");

    let joined =
        sandbox.run_as_participant(&["chat", "healthy", "--join", "--json"], &healthy, &beta);
    assert!(joined.status.success(), "{}", common::stderr(&joined));
    assert!(common::stderr(&joined).contains("invalid participant channels"));
    let listed = sandbox.run_as_participant(&["channels"], &healthy, &beta);
    assert!(listed.status.success(), "{}", common::stderr(&listed));
    assert!(common::stderr(&listed).contains("invalid participant channels"));
    let doctor = sandbox.run_as_participant(&["doctor"], &healthy, &beta);
    assert_eq!(doctor.status.code(), Some(1));
    let doctor: Value = from_stdout(&doctor);
    assert!(doctor["checks"]
        .as_array()
        .expect("doctor checks")
        .iter()
        .any(|check| check["id"] == format!("participant.{corrupt}.channels_invalid")));
}

#[test]
fn routing_join_treats_unknown_participant_membership_conservatively() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let gamma = sandbox.path.join("gamma");
    fs::create_dir(&gamma).expect("gamma workspace");
    register_room(&sandbox, "gamma", &gamma);
    let alpha_actor = bind(&sandbox, "conservative-alpha", &alpha, "alpha");
    let gamma_actor = bind(&sandbox, "conservative-gamma", &gamma, "gamma");
    assert_success(&sandbox.run_as_participant(
        &["chat", "tax", "--join", "--json"],
        &gamma_actor,
        &gamma,
    ));
    fs::write(
        sandbox
            .mail_root
            .join("participants")
            .join(&gamma_actor)
            .join("channels.json"),
        b"{corrupt",
    )
    .expect("corrupt gamma membership");
    fs::write(
        sandbox.mail_root.join("rules.json"),
        r#"{"blocked":[{"from":"alpha","to":"gamma","reason":"separate workspaces"}]}"#,
    )
    .expect("write blocked route");

    let join =
        sandbox.run_as_participant(&["chat", "tax", "--join", "--json"], &alpha_actor, &alpha);
    assert!(!join.status.success());
    let error: post::output::ErrorEnvelope = common::from_stderr(&join);
    assert_eq!(error.error.code, "blocked_route");
    assert!(error.error.message.contains(&gamma_actor));
    let alpha_channels = sandbox
        .mail_root
        .join("participants")
        .join(&alpha_actor)
        .join("channels.json");
    assert!(!alpha_channels.exists());

    let listed = sandbox.run_as_participant(&["channels"], &alpha_actor, &alpha);
    assert!(listed.status.success(), "{}", common::stderr(&listed));
    assert!(common::stderr(&listed).contains("invalid participant channels"));
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
fn routing_malformed_pending_mail_counts_unreadable_and_never_held() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let actor = bind(&sandbox, "malformed-counts", &alpha, "alpha");
    let id = "20990916-051200-acde12";
    let inbox = sandbox.mail_root.join("alpha/inbox");
    fs::create_dir_all(&inbox).expect("alpha inbox");
    fs::write(inbox.join(format!("{id}.mail")), b"not mail").expect("malformed mail");

    let listed = sandbox.run_as_participant(&["inbox"], &actor, &alpha);
    assert!(listed.status.success(), "{}", common::stderr(&listed));
    let listed: Value = from_stdout(&listed);
    assert_eq!(listed["skipped_unreadable"], 1);
    assert_eq!(listed["held"], 0);

    let catchup = sandbox.run_as_participant(&["catchup", "--mail", "--json"], &actor, &alpha);
    assert!(catchup.status.success(), "{}", common::stderr(&catchup));
    assert!(common::stderr(&catchup).contains("skipped unreadable pending mail"));
    assert!(!common::stderr(&catchup).contains("held"));
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
fn routing_direct_self_read_consumes_because_sender_is_a_frozen_recipient() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let actor = sandbox.test_participant("alpha");
    let sent = send_as(
        &sandbox,
        &actor,
        &alpha,
        &format!("participant:{actor}"),
        "direct self consumes",
    );
    let id = sent["envelope"]["id"].as_str().expect("mail id");
    assert_eq!(inbox_as(&sandbox, &actor, &alpha)["unread_count"], 1);

    let read = sandbox.run_as_participant(&["read", id, "--json"], &actor, &alpha);
    assert_success(&read);
    let read: Value = from_stdout(&read);
    assert_eq!(read["own"], true);
    assert!(read.get("pending").is_none());
    assert_eq!(inbox_as(&sandbox, &actor, &alpha)["unread_count"], 0);
}

#[test]
fn routing_pending_own_read_succeeds_and_excluded_own_ack_is_a_noop() {
    let pending = Sandbox::new_unseeded();
    let solo = pending.path.join("solo");
    fs::create_dir_all(&solo).expect("solo workspace");
    assert_success(&pending.run(&["rooms", "add", "solo", solo.to_string_lossy().as_ref()]));
    let actor = pending.bind_claude("solo-own", &solo, Some("solo"))["id"]
        .as_str()
        .expect("solo actor")
        .to_owned();
    let sent = send_as(&pending, &actor, &solo, "workspace:solo", "pending own");
    let id = sent["envelope"]["id"].as_str().expect("pending id");
    assert!(!pending
        .mail_root
        .join(format!("solo/routing/{id}.json"))
        .exists());
    let read = pending.run_as_participant(&["read", id, "--json"], &actor, &solo);
    assert_success(&read);
    let read: Value = from_stdout(&read);
    assert_eq!(read["own"], true);
    assert_eq!(read["pending"], true);

    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let sender = bind(&sandbox, "excluded-ack-sender", &alpha, "alpha");
    let _sibling = bind(&sandbox, "excluded-ack-sibling", &alpha, "alpha");
    let sent = send_as(
        &sandbox,
        &sender,
        &alpha,
        "workspace:alpha",
        "ack inspection",
    );
    let id = sent["envelope"]["id"].as_str().expect("mail id");
    let cursor = sandbox
        .mail_root
        .join("participants")
        .join(&sender)
        .join("cursors.json");
    let seeded = b"{\"version\":2,\"mail\":{},\"channels\":{}}\n";
    fs::write(&cursor, seeded).expect("seed cursor");
    let ack = sandbox.run_as_participant(&["read", id, "--ack", "--json"], &sender, &alpha);
    assert_success(&ack);
    let ack: Value = from_stdout(&ack);
    assert_eq!(ack["acknowledged"], false);
    assert_eq!(fs::read(&cursor).expect("cursor after ack"), seeded);
}

#[test]
fn routing_pending_lineage_ack_names_unrouted_delivery_not_sender_history() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let actor = bind(&sandbox, "pending-lineage-reader", &alpha, "alpha");
    let sender = bind(&sandbox, "pending-lineage-sender", &beta, "beta");
    patch_participant(&sandbox, &actor, |record| {
        record["lineage"] = json!("Ember Grove!");
        record["lineage_since"] = json!("2026-09-16T10:00:00Z");
    });
    let lineage = sandbox.mail_root.join("lineages/Ember Grove!");
    fs::create_dir_all(lineage.join("inbox")).expect("lineage inbox");
    fs::write(
        lineage.join("lineage.json"),
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
    let id = "20990916-045000-acde50";
    write_custom_mail(
        &lineage.join("inbox"),
        id,
        &json!({
            "id": id,
            "from": "beta",
            "to": "Ember Grove!",
            "kind": "note",
            "subject": "pending lineage",
            "sent": "2026-09-16 04:50:00 -0500",
            "from_participant": sender,
            "address_kind": "lineage"
        }),
        "pending lineage",
    );
    let ack = sandbox.run_as_participant(&["read", id, "--ack"], &actor, &alpha);
    assert_success(&ack);
    let text = common::stdout(&ack);
    assert!(text.contains("pending delivery"), "{text}");
    assert!(text.contains("not yet routed"), "{text}");
    assert!(!text.contains("sender history"), "{text}");
}

#[test]
fn routing_sender_history_discovers_pending_unattended_workspace() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let empty = sandbox.path.join("empty");
    fs::create_dir(&empty).expect("empty workspace");
    assert_success(&sandbox.run(&["rooms", "add", "empty", empty.to_string_lossy().as_ref()]));
    let actor = bind(&sandbox, "unattended-sender", &alpha, "alpha");
    let sent = send_as(
        &sandbox,
        &actor,
        &alpha,
        "workspace:empty",
        "unattended sender history",
    );
    let id = sent["envelope"]["id"].as_str().expect("mail id");
    assert!(!sandbox
        .mail_root
        .join(format!("empty/routing/{id}.json"))
        .exists());

    let read = sandbox.run_as_participant(&["read", id, "--json"], &actor, &alpha);
    assert_success(&read);
    let read: Value = from_stdout(&read);
    assert_eq!(read["own"], true);
    assert_eq!(read["pending"], true);
    let search = sandbox.run_as_participant(
        &["search", "unattended sender history", "--mail", "--json"],
        &actor,
        &alpha,
    );
    assert_success(&search);
    let search: Value = from_stdout(&search);
    assert_eq!(search["count"], 1);
    assert_eq!(search["results"][0]["id"], id);
    assert_eq!(search["results"][0]["own"], true);
    assert_eq!(search["results"][0]["pending"], true);
}

#[test]
fn routing_send_text_prints_a_runnable_readback_for_routed_and_pending_own_mail() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let actor = sandbox.test_participant("alpha");
    let sent = sandbox.run_as_participant(
        &["send", "--to", "workspace:beta", "--body", "printed routed"],
        &actor,
        &alpha,
    );
    assert_success(&sent);
    let command = printed_readback(&common::stdout(&sent)).to_owned();
    let read = run_printed_readback(&sandbox, &command, &actor, &alpha);
    assert_success(&read);
    assert!(common::stdout(&read).contains("own: true"));

    let self_sent = sandbox.run_as_participant(
        &[
            "send",
            "--to",
            &format!("participant:{actor}"),
            "--body",
            "printed self",
        ],
        &actor,
        &alpha,
    );
    assert_success(&self_sent);
    assert!(
        common::stdout(&self_sent)
            .contains("sender is a frozen recipient and the message is initially unread"),
        "{}",
        common::stdout(&self_sent)
    );

    let pending = Sandbox::new_unseeded();
    let solo = pending.path.join("solo");
    fs::create_dir_all(&solo).expect("solo workspace");
    assert_success(&pending.run(&["rooms", "add", "solo", solo.to_string_lossy().as_ref()]));
    let actor = pending.bind_claude("printed-pending", &solo, Some("solo"))["id"]
        .as_str()
        .expect("pending actor")
        .to_owned();
    let sent = pending.run_as_participant(
        &[
            "send",
            "--to",
            "workspace:solo",
            "--body",
            "printed pending",
        ],
        &actor,
        &solo,
    );
    assert_success(&sent);
    let command = printed_readback(&common::stdout(&sent)).to_owned();
    let read = run_printed_readback(&pending, &command, &actor, &solo);
    assert_success(&read);
    let text = common::stdout(&read);
    assert!(text.contains("own: true"), "{text}");
    assert!(text.contains("pending: true"), "{text}");
}

#[test]
fn routing_text_renderers_do_not_invent_private_or_bridge_replies_for_unknown_origin() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let actor = bind(&sandbox, "unknown-renderer", &alpha, "alpha");
    let peer = bind(&sandbox, "unknown-renderer-peer", &beta, "beta");
    let id = "20990916-044000-acde40";
    write_custom_mail(
        &sandbox.mail_root.join("alpha/inbox"),
        id,
        &json!({"id":id,"from":"mystery","to":"alpha","kind":"note","subject":"unknown","sent":"2026-09-16 04:40:00 -0500","from_participant":"missing-local","address_kind":"workspace"}),
        "unknown-renderer-proof",
    );
    assert_success(&sandbox.run_as_participant(
        &["participant", "bind", "--workspace", "alpha", "--json"],
        &actor,
        &alpha,
    ));

    for args in [
        vec!["inbox", "--text"],
        vec!["read", id, "--peek"],
        vec!["search", "unknown-renderer-proof", "--mail"],
        vec!["watch", "--snapshot", "--text"],
    ] {
        let output = sandbox.run_as_participant(&args, &actor, &alpha);
        assert_success(&output);
        let text = common::stdout(&output);
        assert!(
            !text.contains("participant:missing-local"),
            "{args:?}: {text}"
        );
        assert!(!text.contains("crossed the bridge"), "{args:?}: {text}");
        if args[0] != "watch" {
            assert!(text.contains("origin: unknown"), "{args:?}: {text}");
            assert!(
                text.contains("sender not known on this host"),
                "{args:?}: {text}"
            );
        }
    }
    let catchup = sandbox.run_as_participant(&["catchup", "--mail"], &actor, &alpha);
    assert_success(&catchup);
    let text = common::stdout(&catchup);
    assert!(!text.contains("participant:missing-local"), "{text}");
    assert!(!text.contains("crossed the bridge"), "{text}");
    assert!(text.contains("origin: unknown"), "{text}");
    assert!(text.contains("sender not known on this host"), "{text}");

    for (participant, cwd) in [(&actor, &alpha), (&peer, &beta)] {
        assert_success(&sandbox.run_as_participant(
            &["chat", "unknown-renderer", "--join", "--json"],
            participant,
            cwd,
        ));
    }
    let channel_id = "20990916-044001-000001-acde41";
    write_channel_message(
        &sandbox,
        "unknown-renderer",
        channel_id,
        "mystery",
        "unknown",
        "unknown-channel-proof",
    );
    for args in [
        vec!["chat", "unknown-renderer", "--peek"],
        vec![
            "search",
            "unknown-channel-proof",
            "--channel",
            "unknown-renderer",
        ],
        vec!["watch", "--snapshot", "--text"],
    ] {
        let output = sandbox.run_as_participant(&args, &actor, &alpha);
        assert_success(&output);
        let text = common::stdout(&output);
        assert!(!text.contains("crossed the bridge"), "{args:?}: {text}");
        if args[0] != "watch" {
            assert!(text.contains("origin: unknown"), "{args:?}: {text}");
            assert!(
                text.contains("sender not known on this host"),
                "{args:?}: {text}"
            );
        }
    }
    let catchup = sandbox.run_as_participant(&["catchup", "unknown-renderer"], &actor, &alpha);
    assert_success(&catchup);
    let text = common::stdout(&catchup);
    assert!(!text.contains("crossed the bridge"));
    assert!(text.contains("origin: unknown"), "{text}");
    assert!(text.contains("sender not known on this host"), "{text}");
}

#[test]
fn routing_read_modes_share_context_projection_and_pending_own_state() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let sender = sandbox.test_participant("alpha");
    let recipient = sandbox.test_participant("beta");
    let sent = send_as(
        &sandbox,
        &sender,
        &alpha,
        "workspace:beta",
        "project every mode",
    );
    let id = sent["envelope"]["id"].as_str().expect("mail id");
    let mut projections = Vec::new();
    for args in [
        vec!["read", id, "--peek", "--json"],
        vec!["read", id, "--peek", "--max-bytes", "20000", "--json"],
        vec![
            "read",
            id,
            "--offset",
            "0",
            "--length",
            "7",
            "--max-bytes",
            "20000",
            "--json",
        ],
    ] {
        let output = sandbox.run_as_participant(&args, &recipient, &beta);
        assert_success(&output);
        let output: Value = from_stdout(&output);
        projections.push(output);
    }
    for projection in &projections {
        assert_eq!(projection["envelope"]["origin"], "local");
        assert_eq!(
            projection["envelope"]["reply_to_participant"],
            format!("participant:{sender}")
        );
        assert_eq!(
            projection["envelope"]["address"],
            json!({"kind":"workspace","name":"beta"})
        );
        assert!(projection.get("own").is_none());
        assert!(projection.get("pending").is_none());
    }

    let pending = Sandbox::new_unseeded();
    let solo = pending.path.join("solo");
    fs::create_dir_all(&solo).expect("solo workspace");
    assert_success(&pending.run(&["rooms", "add", "solo", solo.to_string_lossy().as_ref()]));
    let actor = pending.bind_claude("pending-projection", &solo, Some("solo"))["id"]
        .as_str()
        .expect("actor")
        .to_owned();
    let sent = send_as(
        &pending,
        &actor,
        &solo,
        "workspace:solo",
        "pending projection",
    );
    let id = sent["envelope"]["id"].as_str().expect("pending id");
    for args in [
        vec!["read", id, "--peek", "--max-bytes", "20000", "--json"],
        vec![
            "read",
            id,
            "--offset",
            "0",
            "--length",
            "7",
            "--max-bytes",
            "20000",
            "--json",
        ],
    ] {
        let output = pending.run_as_participant(&args, &actor, &solo);
        assert_success(&output);
        let output: Value = from_stdout(&output);
        assert_eq!(output["own"], true);
        assert_eq!(output["pending"], true);
        assert_eq!(output["envelope"]["pending"], true);
    }
}

#[test]
fn routing_text_read_modes_share_own_pending_and_consumption_state() {
    let pending = Sandbox::new_unseeded();
    let solo = pending.path.join("solo");
    fs::create_dir_all(&solo).expect("solo workspace");
    assert_success(&pending.run(&["rooms", "add", "solo", solo.to_string_lossy().as_ref()]));
    let actor = pending.bind_claude("text-state", &solo, Some("solo"))["id"]
        .as_str()
        .expect("actor")
        .to_owned();
    let sent = pending.run_as_participant(
        &[
            "send",
            "--to",
            "workspace:solo",
            "--body",
            "pending text state",
            "--json",
        ],
        &actor,
        &solo,
    );
    assert_success(&sent);
    let sent: Value = from_stdout(&sent);
    let id = sent["envelope"]["id"].as_str().expect("pending id");
    for args in [
        vec!["read", id, "--peek", "--max-bytes", "10000"],
        vec![
            "read",
            id,
            "--offset",
            "0",
            "--length",
            "10",
            "--max-bytes",
            "10000",
        ],
    ] {
        let output = pending.run_as_participant(&args, &actor, &solo);
        assert_success(&output);
        let text = common::stdout(&output);
        assert!(text.contains("own: true"), "{args:?}: {text}");
        assert!(text.contains("pending: true"), "{args:?}: {text}");
        assert!(text.contains("not yet routed"), "{args:?}: {text}");
        assert!(!text.contains("unread unchanged"), "{args:?}: {text}");
    }

    let direct = pending.run_as_participant(
        &[
            "send",
            "--to",
            &format!("participant:{actor}"),
            "--body",
            "self consumption",
            "--json",
        ],
        &actor,
        &solo,
    );
    assert_success(&direct);
    let direct: Value = from_stdout(&direct);
    let direct_id = direct["envelope"]["id"].as_str().expect("direct id");
    let read = pending.run_as_participant(&["read", direct_id], &actor, &solo);
    assert_success(&read);
    let text = common::stdout(&read);
    assert!(text.contains("own: true"), "{text}");
    assert!(text.contains("consumed for this participant"), "{text}");
    assert_eq!(inbox_as(&pending, &actor, &solo)["unread_count"], 0);
    let reread = pending.run_as_participant(&["read", direct_id, "--peek"], &actor, &solo);
    assert_success(&reread);
    let text = common::stdout(&reread);
    assert!(text.contains("exact-id cursor"), "{text}");
    assert!(!text.contains("read/archive"), "{text}");
}

#[test]
fn routing_text_read_state_wording_uses_recipient_status() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let recipient = bind(&sandbox, "wording-recipient", &alpha, "alpha");
    let sender = bind(&sandbox, "wording-sender", &beta, "beta");
    let body = "x".repeat(4_000);

    let incoming = send_as(&sandbox, &sender, &beta, "workspace:alpha", &body);
    let incoming_id = incoming["envelope"]["id"].as_str().expect("incoming id");
    let unread = sandbox.run_as_participant(
        &["read", incoming_id, "--peek", "--max-bytes", "2400"],
        &recipient,
        &alpha,
    );
    assert_success(&unread);
    assert!(common::stdout(&unread).contains("mail remains unread"));
    assert_success(&sandbox.run_as_participant(
        &["read", incoming_id, "--json"],
        &recipient,
        &alpha,
    ));
    let already_read = sandbox.run_as_participant(
        &["read", incoming_id, "--peek", "--max-bytes", "2400"],
        &recipient,
        &alpha,
    );
    assert_success(&already_read);
    assert!(common::stdout(&already_read).contains("mail remains already read"));

    let outgoing = send_as(&sandbox, &recipient, &alpha, "workspace:beta", &body);
    let outgoing_id = outgoing["envelope"]["id"].as_str().expect("outgoing id");
    let history = sandbox.run_as_participant(
        &["read", outgoing_id, "--peek", "--max-bytes", "2400"],
        &recipient,
        &alpha,
    );
    assert_success(&history);
    assert!(common::stdout(&history).contains("mail remains sender history (never unread for you)"));

    let self_delivery = send_as(
        &sandbox,
        &recipient,
        &alpha,
        &format!("participant:{recipient}"),
        &body,
    );
    let self_id = self_delivery["envelope"]["id"].as_str().expect("self id");
    let self_slice = sandbox.run_as_participant(
        &[
            "read",
            self_id,
            "--offset",
            "0",
            "--length",
            "10",
            "--max-bytes",
            "10000",
        ],
        &recipient,
        &alpha,
    );
    assert_success(&self_slice);
    let self_text = common::stdout(&self_slice);
    assert!(self_text.contains("own: true (explicit self-delivery; unread unchanged)"));
    assert!(self_text.contains("unread; never consumed"));
    assert!(!self_text.contains("sender-history inspection"));

    let pending = Sandbox::new_unseeded();
    let solo = pending.path.join("solo");
    fs::create_dir_all(&solo).expect("solo workspace");
    assert_success(&pending.run(&["rooms", "add", "solo", solo.to_string_lossy().as_ref()]));
    let actor = pending.bind_claude("wording-pending", &solo, Some("solo"))["id"]
        .as_str()
        .expect("pending actor")
        .to_owned();
    let sent = send_as(&pending, &actor, &solo, "workspace:solo", &body);
    let pending_id = sent["envelope"]["id"].as_str().expect("pending id");
    let pending_omission = pending.run_as_participant(
        &["read", pending_id, "--peek", "--max-bytes", "2400"],
        &actor,
        &solo,
    );
    assert_success(&pending_omission);
    assert!(common::stdout(&pending_omission).contains("mail remains pending (not yet routed)"));
    let pending_slice = pending.run_as_participant(
        &[
            "read",
            pending_id,
            "--offset",
            "0",
            "--length",
            "10",
            "--max-bytes",
            "10000",
        ],
        &actor,
        &solo,
    );
    assert_success(&pending_slice);
    let pending_text = common::stdout(&pending_slice);
    assert!(pending_text.contains("pending (not yet routed); never consumed"));
    assert!(!pending_text.contains("still unread"));
}

#[test]
fn routing_legacy_read_history_keeps_legacy_already_read_wording() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let id = "20990916-051000-acde10";
    write_custom_mail(
        &sandbox.mail_root.join("alpha/read"),
        id,
        &json!({"id":id,"from":"beta","to":"alpha","kind":"note","subject":"legacy","sent":"2026-09-16 05:10:00 -0500"}),
        "legacy history",
    );
    let read = sandbox.run_without_identity(&["read", id, "--room", "alpha", "--peek"], &alpha);
    assert_success(&read);
    let text = common::stdout(&read);
    assert!(
        text.contains("served from the read/archive store"),
        "{text}"
    );
    assert!(!text.contains("participant's exact-id cursor"), "{text}");
    assert!(!text.contains("canonical mail stayed in place"), "{text}");
}

#[test]
fn routing_read_and_catchup_continuations_run_for_every_address_kind() {
    for kind in ["workspace", "participant", "lineage"] {
        let sandbox = Sandbox::new();
        let (alpha, beta) = register_alpha_beta(&sandbox);
        let actor = bind(&sandbox, &format!("continuation-{kind}"), &alpha, "alpha");
        let peer = bind(
            &sandbox,
            &format!("continuation-{kind}-peer"),
            &beta,
            "beta",
        );
        let target = match kind {
            "workspace" => "workspace:alpha".to_owned(),
            "participant" => format!("participant:{actor}"),
            "lineage" => {
                patch_participant(&sandbox, &actor, |record| {
                    record["lineage"] = json!("Ember Grove!");
                    record["lineage_since"] = json!("2026-09-16T10:00:00Z");
                });
                let lineage = sandbox.mail_root.join("lineages/Ember Grove!");
                fs::create_dir_all(&lineage).expect("lineage directory");
                fs::write(
                    lineage.join("lineage.json"),
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
                "lineage:Ember Grove!".to_owned()
            }
            _ => unreachable!(),
        };
        let sender = if kind == "participant" { &actor } else { &peer };
        let sender_cwd = if kind == "participant" { &alpha } else { &beta };
        let body = format!("{kind}-continuation-{}", "x".repeat(6000));
        let sent = send_as(&sandbox, sender, sender_cwd, &target, &body);
        let id = sent["envelope"]["id"].as_str().expect("mail id");

        let bounded = sandbox.run_as_participant(
            &["read", id, "--max-bytes", "2600", "--json"],
            &actor,
            &alpha,
        );
        assert_success(&bounded);
        let bounded: Value = from_stdout(&bounded);
        let bounded_command = bounded["omitted"]["continuation"]
            .as_str()
            .expect("bounded continuation");
        assert_eq!(bounded_command.contains(" --room "), kind == "workspace");
        let resumed = run_printed_readback(&sandbox, bounded_command, &actor, &alpha);
        assert_success(&resumed);
        let resumed: Value = from_stdout(&resumed);
        assert_eq!(
            resumed["own"].as_bool().unwrap_or(false),
            kind == "participant"
        );

        let slice = sandbox.run_as_participant(
            &[
                "read",
                id,
                "--offset",
                "0",
                "--length",
                "10",
                "--max-bytes",
                "2600",
                "--json",
            ],
            &actor,
            &alpha,
        );
        assert_success(&slice);
        let slice: Value = from_stdout(&slice);
        let slice_command = slice["continuation"].as_str().expect("slice continuation");
        assert_eq!(slice_command.contains(" --room "), kind == "workspace");
        assert_success(&run_printed_readback(
            &sandbox,
            slice_command,
            &actor,
            &alpha,
        ));

        let catchup = sandbox.run_as_participant(
            &["catchup", "--mail", "--max-bytes", "2600", "--json"],
            &actor,
            &alpha,
        );
        assert_success(&catchup);
        let catchup: Value = from_stdout(&catchup);
        let catchup_command = catchup["omitted"]["continuation"]
            .as_str()
            .expect("catchup continuation");
        assert_eq!(catchup_command.contains(" --room "), kind == "workspace");
        let resumed = run_printed_readback(&sandbox, catchup_command, &actor, &alpha);
        assert_success(&resumed);
        let resumed: Value = from_stdout(&resumed);
        assert_eq!(
            resumed["own"].as_bool().unwrap_or(false),
            kind == "participant"
        );
    }
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
fn routing_workspace_less_channel_watch_uses_participant_address_even_with_lineage_target() {
    let sandbox = Sandbox::new();
    let actor = sandbox.run(&[
        "participant",
        "bind",
        "--harness",
        "shell",
        "--key",
        "workspace-less-watch",
        "--json",
    ]);
    assert_success(&actor);
    let actor: Value = from_stdout(&actor);
    let id = actor["participant"]["id"]
        .as_str()
        .expect("actor id")
        .to_owned();
    patch_participant(&sandbox, &id, |record| {
        record["lineage"] = json!("ember");
        record["lineage_since"] = json!("2026-09-16T04:41:00Z");
    });
    assert_success(&sandbox.run_as_participant(
        &["chat", "typed-watch", "--join", "--json"],
        &id,
        &sandbox.path,
    ));
    let channel_id = "20990916-044100-000001-acde42";
    write_channel_message(
        &sandbox,
        "typed-watch",
        channel_id,
        "mystery",
        "typed",
        "typed watch body",
    );
    let malformed_id = "20990916-044101-000001-acde43";
    fs::write(
        sandbox
            .mail_root
            .join(format!("channels/typed-watch/messages/{malformed_id}.msg")),
        b"malformed sibling",
    )
    .expect("malformed channel sibling");

    let watched = sandbox.run_as_participant(&["watch", "--snapshot"], &id, &sandbox.path);
    assert!(watched.status.success(), "watch failed: {watched:?}");
    assert!(
        common::stderr(&watched).contains("unreadable channel message"),
        "malformed sibling was not diagnosed: {}",
        common::stderr(&watched)
    );
    let events: Vec<Value> = common::stdout(&watched)
        .lines()
        .map(|line| serde_json::from_str(line).expect("watch event"))
        .collect();
    for event in events
        .iter()
        .filter(|event| event["id"] == channel_id || event["id"] == malformed_id)
    {
        assert_eq!(
            event["address"],
            json!({"kind":"participant","name":id}),
            "channel event borrowed a direct-mail target: {event}"
        );
        assert!(event.get("room").is_none());
    }
    assert!(events.iter().any(|event| event["id"] == channel_id));
    assert!(events.iter().any(|event| event["id"] == malformed_id));
}

#[test]
fn routing_watch_digest_keeps_pending_separate_from_delivered() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let recipient = bind(&sandbox, "digest-recipient", &alpha, "alpha");
    let sender = bind(&sandbox, "digest-sender", &beta, "beta");
    send_as(
        &sandbox,
        &sender,
        &beta,
        "workspace:alpha",
        "delivered digest",
    );
    let pending_id = "20990916-044200-acde44";
    write_custom_mail(
        &sandbox.mail_root.join("alpha/inbox"),
        pending_id,
        &json!({"id":pending_id,"from":"beta","to":"alpha","kind":"note","subject":"pending digest","sent":"2026-09-16 04:42:00 -0500","from_participant":sender,"address_kind":"workspace"}),
        "pending digest",
    );

    let watched =
        sandbox.run_as_participant(&["watch", "--snapshot", "--digest"], &recipient, &alpha);
    assert_success(&watched);
    let digests: Vec<Value> = common::stdout(&watched)
        .lines()
        .map(|line| serde_json::from_str(line).expect("digest"))
        .filter(|digest: &Value| digest["source"] == "mail")
        .collect();
    assert_eq!(digests.len(), 2, "delivered and pending must not merge");
    assert!(digests
        .iter()
        .any(|digest| digest["pending"] == true && digest["count"] == 1));
    assert!(digests
        .iter()
        .any(|digest| digest.get("pending").is_none() && digest["count"] == 1));

    let text = sandbox.run_as_participant(
        &["watch", "--snapshot", "--digest", "--text"],
        &recipient,
        &alpha,
    );
    assert_success(&text);
    let text = common::stdout(&text);
    assert!(
        text.lines().any(|line| line.contains("new pending")),
        "pending digest marker missing: {text}"
    );
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
    assert_eq!(inbox["held"], 1);
    let catchup = sandbox.run_as_participant(&["catchup", "--mail", "--json"], &recipient, &alpha);
    assert!(catchup.status.success());
    assert!(common::stderr(&catchup).contains("left blocked mail held"));
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
    assert!(common::stderr(&rebound).contains("left blocked mail held"));
    let doctor = sandbox.run_as_participant(&["doctor"], &recipient, &alpha);
    assert_eq!(doctor.status.code(), Some(1));
    let doctor: Value = from_stdout(&doctor);
    assert!(
        doctor["checks"]
            .as_array()
            .expect("doctor checks")
            .iter()
            .any(|check| check["id"] == "routing.held.workspace.alpha"
                && check["message"]
                    .as_str()
                    .is_some_and(|message| message.contains(id))),
        "doctor did not report held id: {doctor}"
    );
}

#[test]
fn routing_legacy_workspace_member_blocks_participant_join_and_lists_both_projections() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let alpha_participant = sandbox.test_participant("alpha");
    let beta_participant = sandbox.test_participant("beta");
    let legacy_remote = sandbox.path.join("legacy-remote");
    fs::create_dir_all(&legacy_remote).expect("legacy remote workspace");
    let rooms_path = sandbox.mail_root.join("rooms.json");
    let mut rooms: Value =
        serde_json::from_slice(&fs::read(&rooms_path).expect("rooms")).expect("rooms JSON");
    rooms["legacy-remote"] = json!(legacy_remote);
    fs::write(
        &rooms_path,
        format!("{}\n", serde_json::to_string_pretty(&rooms).expect("rooms")),
    )
    .expect("add legacy remote without participant");
    let channel = sandbox.mail_root.join("channels/legacy-block");
    fs::create_dir_all(channel.join("messages")).expect("legacy channel messages");
    fs::write(
        channel.join("channel.json"),
        r#"{"name":"legacy-block","created":"2026-09-16 04:31:00 -0500","created_by":"beta"}"#,
    )
    .expect("legacy channel info");
    fs::write(
        channel.join("members.json"),
        r#"{"beta":"joined","legacy-remote":"joined"}"#,
    )
    .expect("legacy members");
    fs::write(
        sandbox.mail_root.join("rules.json"),
        r#"{"blocked":[{"from":"alpha","to":"legacy-remote","reason":"legacy member block"}]}"#,
    )
    .expect("blocked rule");

    let refused = sandbox.run_as_participant(
        &["chat", "legacy-block", "--join", "--json"],
        &alpha_participant,
        &alpha,
    );
    assert_eq!(refused.status.code(), Some(77));
    let error: post::output::ErrorEnvelope = common::from_stderr(&refused);
    assert_eq!(error.error.code, "blocked_route");
    assert!(error.error.message.contains("legacy member block"));

    let listed = sandbox.run_as_participant(&["channels"], &beta_participant, &beta);
    assert_success(&listed);
    let listed: Value = from_stdout(&listed);
    let legacy = listed["channels"]
        .as_array()
        .expect("channels")
        .iter()
        .find(|item| item["name"] == "legacy-block")
        .expect("legacy channel");
    assert_eq!(legacy["members"], json!(["beta", "legacy-remote"]));
    assert_eq!(legacy["participants"], json!([beta_participant]));
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
    let read = sandbox.run_as_participant(&["chat", "session-only"], &id, &sandbox.path);
    assert_success(&read);
    assert!(!sandbox.mail_root.join(&id).exists());
    assert!(sandbox
        .mail_root
        .join("participants")
        .join(&id)
        .join("cursors.json")
        .exists());
    assert!(sandbox
        .mail_root
        .join("participants")
        .join(&id)
        .join("banner-day")
        .is_file());
}

#[test]
fn routing_skips_corrupt_participant_and_delivers_to_valid_sibling() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let valid_a = bind(&sandbox, "valid-sibling-a", &alpha, "alpha");
    let valid_b = bind(&sandbox, "valid-sibling-b", &alpha, "alpha");
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
    let recipients = receipt["recipients"].as_array().expect("recipients");
    assert!(recipients.iter().any(|value| value == &valid_a));
    assert!(recipients.iter().any(|value| value == &valid_b));
    assert!(recipients.len() >= 2);
    assert!(!recipients.iter().any(|value| value == "claude-deadbeef"));
}

#[test]
fn routing_doctor_keeps_reporting_when_pending_projection_hits_a_bad_receipt() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let actor = bind(&sandbox, "doctor-projection", &alpha, "alpha");
    let routing = sandbox.mail_root.join("alpha/routing");
    fs::create_dir_all(&routing).expect("routing directory");
    fs::write(
        routing.join("20260916-043100-acde01.json"),
        b"{not a receipt",
    )
    .expect("malformed receipt");

    let doctor = sandbox.run_as_participant(&["doctor"], &actor, &alpha);
    assert_eq!(doctor.status.code(), Some(1));
    let doctor: Value = from_stdout(&doctor);
    assert_eq!(doctor["participant"]["id"], actor);
    assert_eq!(doctor["status"], "broken");
    assert!(doctor["checks"]
        .as_array()
        .expect("doctor checks")
        .iter()
        .any(|check| check["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("routing.receipt."))
            && check["message"]
                .as_str()
                .is_some_and(|message| message.contains("invalid routing receipt"))));
    let brief = sandbox.run_as_participant(&["doctor", "--brief"], &actor, &alpha);
    assert_eq!(brief.status.code(), Some(1));
    assert!(common::stdout(&brief).contains("findings"));
    assert!(!common::stdout(&brief).contains("doctor: ok"));
}

#[test]
fn routing_doctor_participant_resolution_errors_drive_every_output_mode() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let run = |args: &[&str]| {
        common::post_command()
            .args(args)
            .current_dir(&alpha)
            .env("HOME", &sandbox.home)
            .env("POST_MAIL_ROOT", &sandbox.mail_root)
            .env("POST_PARTICIPANT", "malformed:participant")
            .env_remove("POST_FROM")
            .env_remove("POST_SENDER_ADDRESS")
            .env_remove("POST_ARX_GENERATION")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("run doctor with malformed participant binding")
    };

    for args in [
        &["doctor"][..],
        &["--json", "doctor"],
        &["--pretty", "doctor"],
    ] {
        let output = run(args);
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        let report: Value = from_stdout(&output);
        assert_eq!(report["status"], "broken");
        assert!(report["checks"]
            .as_array()
            .expect("doctor checks")
            .iter()
            .any(|check| check["id"] == "participant.binding.invalid"));
        assert!(report["participant_error"]
            .as_str()
            .is_some_and(|message| message.contains("malformed:participant")));
    }

    let brief = run(&["doctor", "--brief"]);
    assert_eq!(brief.status.code(), Some(1));
    assert!(common::stdout(&brief).contains("findings"));
    assert!(!common::stdout(&brief).contains("doctor: ok"));
}

#[test]
fn routing_explicit_workspace_inbox_keeps_the_selected_room_after_rebind() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let actor = bind(&sandbox, "explicit-room-actor", &alpha, "alpha");
    let sender = bind(&sandbox, "explicit-room-sender", &beta, "beta");
    let sent = send_as(&sandbox, &sender, &beta, "workspace:alpha", "alpha history");
    let id = sent["envelope"]["id"].as_str().expect("mail id");
    assert_success(&sandbox.run_as_participant(
        &["participant", "bind", "--workspace", "beta", "--json"],
        &actor,
        &beta,
    ));

    let listed = sandbox.run_as_participant(&["inbox", "--room", "alpha"], &actor, &beta);
    assert_success(&listed);
    let listed: Value = from_stdout(&listed);
    assert_eq!(listed["room"], "alpha");
    assert!(listed["unread"]
        .as_array()
        .expect("unread")
        .iter()
        .any(|item| item["id"] == id));
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
    assert_success(&sandbox.run_as_participant(
        &["chat", "display-only", "--join", "--json"],
        &a,
        &alpha,
    ));
    assert_success(&sandbox.run_as_participant(
        &["chat", "display-only", "--join", "--json"],
        &c,
        &beta,
    ));
    let channel_send = sandbox.run_as_participant(
        &[
            "chat",
            "display-only",
            "--send",
            "--anyway",
            "--body",
            "display channel",
            "--json",
        ],
        &c,
        &beta,
    );
    assert_success(&channel_send);
    let channel_send: Value = from_stdout(&channel_send);
    let channel_id = channel_send["message"]["id"]
        .as_str()
        .expect("channel id")
        .to_owned();
    let before = tree(&sandbox.mail_root);
    for args in vec![
        vec!["inbox"],
        vec!["read", id, "--peek", "--json"],
        vec!["watch", "--snapshot", "--json"],
        vec!["channels"],
        vec!["search", "display", "--mail", "--json"],
        vec!["who"],
        vec!["doctor"],
        vec!["chat", "display-only", "--peek", "--json"],
        vec!["chat", "display-only", "--history", "10", "--json"],
        vec![
            "chat",
            "display-only",
            "--since",
            channel_id.as_str(),
            "--json",
        ],
    ] {
        let output = sandbox.run_as_participant(&args, &a, &alpha);
        if args == ["doctor"] {
            assert_eq!(output.status.code(), Some(1));
        } else {
            assert_success(&output);
        }
    }
    assert_eq!(tree(&sandbox.mail_root), before);
}

#[test]
fn routing_bound_room_overrides_reject_reserved_and_traversal_names_without_mutation() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let actor = bind(&sandbox, "room-override", &alpha, "alpha");
    let before = tree(&sandbox.mail_root);
    for args in [
        vec!["inbox", "--room", "participants"],
        vec![
            "read",
            "20260916-000000-acde01",
            "--room",
            "../alpha",
            "--peek",
        ],
    ] {
        let output = sandbox.run_as_participant(&args, &actor, &alpha);
        assert!(
            !output.status.success(),
            "reserved override was admitted: {args:?}"
        );
        let error: post::output::ErrorEnvelope = common::from_stderr(&output);
        assert!(matches!(
            error.error.code.as_str(),
            "invalid_argument" | "not_found"
        ));
        assert_eq!(tree(&sandbox.mail_root), before, "{args:?} mutated state");
    }
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
                out.insert(
                    path.strip_prefix(root)
                        .expect("relative directory")
                        .to_path_buf(),
                    b"<dir>".to_vec(),
                );
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
