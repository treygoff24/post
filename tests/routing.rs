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
        record["last_seen"] = json!("2000-01-01 00:00:00 +0000");
        record["lease_hours"] = json!(1);
    });
    patch_participant(&sandbox, &ended, |record| {
        record["ended_at"] = json!("2026-09-16 03:00:00 -0500");
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
    let read = sandbox.run_as_participant(&["read", id, "--json"], &b, &solo);
    assert_success(&read);
    assert!(receipt.is_file());
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
