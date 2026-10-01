//! The frozen Porch corpus through the real CLI, plus suffix/attention boundaries.
mod common;
use common::{assert_success, from_stdout, join_channel, register_alpha_beta, stdout, Sandbox};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn review_doctor_accepts_valid_emotes_and_warns_on_unreadable_records() {
    let (s, a, _) = setup();
    set(&s, &a, "bolt");
    let receipt = ok(&s, &["chat", "ops", "--emote", "hop", "--json"], &a);
    let dir = s.mail_root.join("channels/ops/messages");
    let valid = dir.join(format!(
        "{}.emote",
        receipt["message"]["id"].as_str().unwrap()
    ));
    let corrupt = dir.join("20990930-100000-000001-aaaaaa.emote");
    fs::write(&corrupt, b"broken").unwrap();
    let output = run(&s, &["doctor", "--json"], &a);
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    let checks = value["checks"].as_array().unwrap();
    assert!(
        !checks
            .iter()
            .any(|c| c.to_string().contains(valid.to_str().unwrap())),
        "{value}"
    );
    let warning = checks
        .iter()
        .find(|c| c.to_string().contains(corrupt.to_str().unwrap()))
        .expect("corrupt emote warning");
    assert_eq!(warning["severity"], "warning");
    assert!(warning.to_string().contains("envelope-separator"));
    assert!(!warning.to_string().contains("stray_file"));
    assert!(valid.is_file() && corrupt.is_file());
}

#[test]
fn review_bubble_is_returned_with_a_rule_and_never_reported_as_skipped() {
    let (s, a, _) = setup();
    let id = "20990930-100000-000002-aaaaaa";
    let text =
        fs::read_to_string(corpus().join("emotes/records/bubble/payload-missing.emote")).unwrap();
    let (header, _) = text.split_once("\n---\n").unwrap();
    let mut value: Value = serde_json::from_str(header).unwrap();
    value["id"] = id.into();
    value["channel"] = "ops".into();
    fs::write(
        s.mail_root
            .join(format!("channels/ops/messages/{id}.emote")),
        format!("{value}\n---\n"),
    )
    .unwrap();
    let history = ok(&s, &["chat", "ops", "--history", "10", "--json"], &a);
    assert_eq!(history["messages"][0]["id"], id);
    assert_eq!(history["messages"][0]["emote_rule"], "payload-missing");
    assert!(history
        .get("skipped_files")
        .is_none_or(|v| v.as_array().unwrap().is_empty()));
    let exact = ok(
        &s,
        &[
            "chat",
            "ops",
            "--message",
            id,
            "--max-bytes",
            "8000",
            "--json",
        ],
        &a,
    );
    assert_eq!(exact["emote_rule"], "payload-missing");
}

#[test]
fn review_emote_send_respects_output_mode_and_names_stdin_refusal() {
    let (s, a, _) = setup();
    set(&s, &a, "bolt");
    let output = run(&s, &["chat", "ops", "--emote", "hop"], &a);
    assert_success(&output);
    assert!(serde_json::from_slice::<Value>(&output.stdout).is_err());
    assert!(stdout(&output).contains("hop"));
}

#[test]
fn review_emote_stdin_refusal_names_the_emote_command() {
    let (s, a, _) = setup();
    set(&s, &a, "bolt");
    let before = common::tree_snapshot(&s.mail_root);
    let participant = s.test_participant("alpha");
    let output = s.run_in_env(
        &["chat", "ops", "--emote", "hop", "--json"],
        Some("unintended body"),
        &a,
        &[("POST_PARTICIPANT", &participant)],
    );
    assert!(!output.status.success());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert!(error["error"]["message"]
        .as_str()
        .unwrap()
        .contains("emote"));
    assert!(!error["error"]["message"]
        .as_str()
        .unwrap()
        .contains("is a read"));
    assert_eq!(common::tree_snapshot(&s.mail_root), before);
}

#[test]
#[cfg(target_os = "linux")]
fn review_ack_prefix_does_not_open_unrelated_files() {
    use std::ffi::CString;
    use std::io::Read;
    use std::os::fd::FromRawFd;
    use std::os::unix::ffi::OsStrExt;
    let (s, a, _) = setup();
    plant(&s, false, true);
    let dir = s.mail_root.join("channels/ops/messages");
    let target = dir.join("20990930-100000-000001-aaaaaa.msg");
    let unrelated = dir.join("20990930-100000-999999-aaaaaa.msg");
    fs::write(&unrelated, b"unrelated unreadable message").unwrap();
    let raw = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
    assert!(raw >= 0);
    let mut watcher = unsafe { fs::File::from_raw_fd(raw) };
    let watch = |path: &Path| {
        let name = CString::new(path.as_os_str().as_bytes()).unwrap();
        let id = unsafe { libc::inotify_add_watch(raw, name.as_ptr(), libc::IN_OPEN) };
        assert!(id >= 0);
        id
    };
    let target_watch = watch(&target);
    let unrelated_watch = watch(&unrelated);
    let value = ok(
        &s,
        &["chat", "ops", "--ack", "20990930-100000-000001", "--json"],
        &a,
    );
    assert_eq!(value["id"], "20990930-100000-000001-aaaaaa");
    // The child has exited, so its filesystem opens are already queued.
    // Observe real opens, including a positive target control; no timing or
    // memory threshold and no test-only production seam is involved.
    let mut buffer = [0u8; 4096];
    let n = watcher
        .read(&mut buffer)
        .expect("target open must produce an event");
    let mut offset = 0;
    let mut saw_target = false;
    while offset < n {
        assert!(offset + 16 <= n);
        let id = i32::from_ne_bytes(buffer[offset..offset + 4].try_into().unwrap());
        let length =
            u32::from_ne_bytes(buffer[offset + 12..offset + 16].try_into().unwrap()) as usize;
        assert_ne!(
            id, unrelated_watch,
            "acknowledgment opened an unrelated message"
        );
        saw_target |= id == target_watch;
        offset += 16 + length;
    }
    assert_eq!(offset, n);
    assert!(
        saw_target,
        "positive control: acknowledgment opened its target"
    );
}

#[test]
fn review_emote_write_cap_refuses_large_real_headers_without_publication() {
    let (s, a, _) = setup();
    set(&s, &a, "limit-freeze-1280");
    let display_name = "\u{10400}".repeat(32);
    ok(
        &s,
        &["profile", "set", "--name", &display_name, "--json"],
        &a,
    );
    let lineage = "z".repeat(240);
    ok(&s, &["identity", "new", &lineage, "--json"], &a);
    let participant = s.test_participant("alpha");
    let address = "a".repeat(256);
    let before = common::tree_snapshot(&s.mail_root.join("channels/ops/messages"));
    let output = s.run_in_env(
        &["chat", "ops", "--emote", "chatter", "--json"],
        None,
        &a,
        &[
            ("POST_PARTICIPANT", &participant),
            ("POST_SENDER_ADDRESS", &address),
        ],
    );
    assert!(
        !output.status.success(),
        "oversized real header was published"
    );
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "invalid_argument");
    assert!(
        error["error"]["message"].as_str().unwrap().contains("3072"),
        "{error}"
    );
    assert_eq!(
        common::tree_snapshot(&s.mail_root.join("channels/ops/messages")),
        before
    );
}

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/porch-contract")
}
fn run(s: &Sandbox, args: &[&str], cwd: &Path) -> std::process::Output {
    s.run_as_participant(
        args,
        &s.test_participant(cwd.file_name().unwrap().to_str().unwrap()),
        cwd,
    )
}
fn ok(s: &Sandbox, args: &[&str], cwd: &Path) -> Value {
    let o = run(s, args, cwd);
    assert_success(&o);
    from_stdout(&o)
}
fn setup() -> (Sandbox, PathBuf, PathBuf) {
    let s = Sandbox::new();
    let (a, b) = register_alpha_beta(&s);
    join_channel(&s, "ops", &a);
    join_channel(&s, "ops", &b);
    for cwd in [&a, &b] {
        ok(&s, &["chat", "ops", "--json"], cwd);
    }
    for entry in fs::read_dir(s.mail_root.join("channels/ops/messages")).unwrap() {
        fs::remove_file(entry.unwrap().path()).unwrap();
    }
    fs::write(
        s.mail_root.join("channels/ops/members.json"),
        r#"{"alpha":"2026-09-30 10:00:00 +0000","beta":"2026-09-30 10:00:00 +0000"}"#,
    )
    .unwrap();
    (s, a, b)
}
fn set(s: &Sandbox, cwd: &Path, pack: &str) -> Value {
    ok(
        s,
        &[
            "profile",
            "avatar",
            "set",
            "--file",
            corpus()
                .join(format!("avatars/valid/{pack}.json"))
                .to_str()
                .unwrap(),
            "--json",
        ],
        cwd,
    )
}
fn plant(s: &Sandbox, emotes: bool, ordinary: bool) {
    let dir = s.mail_root.join("channels/ops/messages");
    if ordinary {
        for (id, body) in [
            ("20990930-100000-000001-aaaaaa", "ordinary"),
            ("20990930-100000-000003-aaaaaa", "@beta ordinary mention"),
        ] {
            fs::write(dir.join(format!("{id}.msg")),format!("{}\n---\n{body}",json!({"id":id,"from":"alpha","from_participant":s.test_participant("alpha"),"channel":"ops","sent":"2099-09-30 10:00:00 +0000"}))).unwrap();
        }
    }
    if emotes {
        let id = "20990930-100000-000002-aaaaaa";
        let fixture =
            fs::read_to_string(corpus().join("emotes/records/playable/body-ignored.emote"))
                .unwrap();
        let (head, _) = fixture.split_once("\n---\n").unwrap();
        let mut v: Value = serde_json::from_str(head).unwrap();
        v["id"] = id.into();
        v["from"] = "alpha".into();
        v["channel"] = "ops".into();
        v["mentions"] = json!(["beta"]);
        v["emote"]["at"] = s.test_participant("beta").into();
        v["emote"]["planted"] = "@beta @test-beta".into();
        fs::write(
            dir.join(format!("{id}.emote")),
            format!("{}\n---\n@beta", v),
        )
        .unwrap();
        fs::write(
            dir.join("20990930-100000-000004-aaaaaa.emote"),
            b"corrupt @beta",
        )
        .unwrap();
        // Older or manually planted compatibility records must stay opaque too.
        v["id"] = "20990930-100000-000005-aaaaaa".into();
        fs::write(
            dir.join("20990930-100000-000005-aaaaaa.msg"),
            format!("{}\n---\n@beta", v),
        )
        .unwrap();
    }
}
fn scrub(v: &mut Value) {
    match v {
        Value::Object(m) => {
            m.remove("sent");
            m.remove("hint");
            m.remove("history_hint");
            m.remove("cursors");
            m.remove("stale");
            for v in m.values_mut() {
                scrub(v);
            }
        }
        Value::Array(a) => {
            for v in a {
                scrub(v);
            }
        }
        _ => {}
    }
}
fn snapshot(s: &Sandbox, cwd: &Path, extra: &[&str], unbound: bool) -> Vec<Value> {
    let mut args = vec!["watch", "--snapshot", "--json"];
    if !extra.contains(&"--limit") {
        args.extend(["--limit", "0"]);
    }
    args.extend_from_slice(extra);
    let o = if unbound {
        s.run_without_identity(&args, cwd)
    } else {
        run(s, &args, cwd)
    };
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    stdout(&o)
        .lines()
        .map(|line| {
            let mut v = serde_json::from_str(line).unwrap();
            scrub(&mut v);
            v
        })
        .collect()
}

#[test]
fn avatar_corpus_exact_rules_and_canonical_limits() {
    let (s, a, _) = setup();
    let mut count = 0;
    for kind in ["valid", "invalid"] {
        for e in fs::read_dir(corpus().join(format!("avatars/{kind}"))).unwrap() {
            let p = e.unwrap().path();
            let args = [
                "profile",
                "avatar",
                "set",
                "--file",
                p.to_str().unwrap(),
                "--json",
            ];
            let o = run(&s, &args, &a);
            if kind == "valid" {
                assert_success(&o);
                let v: Value = from_stdout(&o);
                assert!(v["avatar"].is_object());
                let stored = fs::read(
                    s.mail_root
                        .join("avatars")
                        .join(format!("{}.json", s.test_participant("alpha"))),
                )
                .unwrap();
                assert_eq!(stored.last(), Some(&b'\n'));
                assert!(stored.len() <= 16385);
                if p.file_stem().unwrap() == "limit-canonical-16384" {
                    assert_eq!(stored.len(), 16385);
                }
            } else {
                assert!(!o.status.success(), "{} accepted", p.display());
                let v: Value = serde_json::from_slice(&o.stderr).unwrap();
                let rule = p
                    .file_stem()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .split("--")
                    .next()
                    .unwrap();
                assert_eq!(
                    v["error"]["details"]["rules"],
                    json!([rule]),
                    "{}",
                    p.display()
                );
            }
            count += 1;
        }
    }
    assert_eq!(count, 51);
}
#[test]
fn freeze_corpus_exact_bytes_from_cli() {
    let (s, a, _) = setup();
    let mut count = 0;
    for e in fs::read_dir(corpus().join("emotes/freeze")).unwrap() {
        let p = e.unwrap().path();
        let n = p.file_stem().unwrap().to_str().unwrap();
        let (pack, emote) = n.split_once('.').unwrap();
        set(&s, &a, pack);
        let v = ok(&s, &["chat", "ops", "--emote", emote, "--json"], &a);
        let frozen = json!({"frames":v["message"]["emote"]["frames"],"steps":v["message"]["emote"]["steps"]});
        assert_eq!(
            serde_json::to_vec(&frozen).unwrap(),
            fs::read(&p).unwrap(),
            "{}",
            p.display()
        );
        assert!(v.get("crossed").is_none());
        let id = v["message"]["id"].as_str().unwrap();
        let bytes = fs::read(
            s.mail_root
                .join(format!("channels/ops/messages/{id}.emote")),
        )
        .unwrap();
        assert!(bytes.ends_with(b"\n---\n"));
        assert!(bytes.len() - 5 <= 3072);
        count += 1;
    }
    assert_eq!(count, 10);
}
#[test]
fn avatar_silent_storage_and_revalidation() {
    let (s, a, b) = setup();
    let before = fs::read_dir(s.mail_root.join("channels/ops/messages"))
        .unwrap()
        .count();
    let v = set(&s, &a, "bolt");
    let id = v["participant"].as_str().unwrap();
    assert_eq!(ok(&s, &["profile", "show"], &a)["avatar"], v["avatar"]);
    let list = ok(&s, &["profile", "list", "--avatars", "--json"], &b);
    assert!(list["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["participant"] == id && p["avatar"] == v["avatar"]));
    // An old-style profiles.json rewrite cannot erase the sibling avatar.
    fs::write(s.mail_root.join("profiles.json"), "{}\n").unwrap();
    assert_eq!(
        ok(&s, &["profile", "avatar", "show", id], &b)["avatar"],
        v["avatar"]
    );
    fs::write(
        s.mail_root.join(format!("avatars/{id}.json")),
        "{\"format\":2}",
    )
    .unwrap();
    let shown = ok(&s, &["profile", "avatar", "show", id], &b);
    assert!(shown["avatar"].is_null());
    assert!(shown["warnings"].to_string().contains("format-unsupported"));
    let o = run(&s, &["chat", "ops", "--emote", "hop", "--json"], &a);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr)
        .contains("set an avatar first (post profile avatar set)"));
    ok(&s, &["profile", "avatar", "clear"], &a);
    assert_eq!(
        fs::read_dir(s.mail_root.join("channels/ops/messages"))
            .unwrap()
            .count(),
        before
    );
}
#[test]
fn emote_retrieval_targets_duplicates_and_pagination() {
    let (s, a, b) = setup();
    set(&s, &a, "blob");
    let target = s.test_participant("beta");
    let v = ok(
        &s,
        &["chat", "ops", "--emote", "wave", "--at", &target, "--json"],
        &a,
    );
    let id = v["message"]["id"].as_str().unwrap();
    // Playback is frozen: clearing the avatar and using a future library
    // cannot invalidate a structurally valid historical payload.
    ok(&s, &["profile", "avatar", "clear"], &a);
    let emote_path = s
        .mail_root
        .join(format!("channels/ops/messages/{id}.emote"));
    let raw = fs::read_to_string(&emote_path).unwrap();
    let (header, _) = raw.split_once("\n---\n").unwrap();
    let mut header: Value = serde_json::from_str(header).unwrap();
    header["emote"]["library"] = "builtin-99".into();
    fs::write(&emote_path, format!("{}\n---\nignored", header)).unwrap();
    let h = ok(&s, &["chat", "ops", "--history", "1", "--json"], &b);
    assert_eq!(
        h["messages"][0]["emote"]["frames"],
        v["message"]["emote"]["frames"]
    );
    assert!(h.get("skipped_files").is_none());
    assert_eq!(h["count"], 1);
    assert_eq!(h["messages"][0]["id"], id);
    let exact = ok(
        &s,
        &[
            "chat",
            "ops",
            "--message",
            id,
            "--max-bytes",
            "8000",
            "--json",
        ],
        &b,
    );
    assert!(exact.to_string().contains("\"emote\""));
    for flag in ["--ack", "--seen-by", "--discard-through"] {
        let o = run(&s, &["chat", "ops", flag, id, "--json"], &b);
        assert_eq!(o.status.code(), Some(66));
        assert!(String::from_utf8_lossy(&o.stderr)
            .contains("emote records are never reply, seen-by or unread targets"));
    }
    let o = s.run_in(
        &[
            "chat", "ops", "--send", "--body", "reply", "--re", id, "--json",
        ],
        None,
        &b,
    );
    assert_eq!(o.status.code(), Some(66));
    assert_eq!(
        ok(&s, &["chat", "ops", "--since", id, "--json"], &b)["count"],
        0
    );
    assert_eq!(
        ok(
            &s,
            &["chat", "ops", "--history", "1", "--grep", ".", "--json"],
            &b
        )["count"],
        0
    );
    let path = s.mail_root.join(format!("channels/ops/messages/{id}.msg"));
    fs::write(
        path,
        format!(
            "{}\n---\nregular",
            json!({"id":id,"from":"alpha","channel":"ops","sent":"2026-09-30 10:00:00 +0000"})
        ),
    )
    .unwrap();
    let h = ok(&s, &["chat", "ops", "--history", "1", "--json"], &b);
    assert_eq!(h["messages"][0]["body"], "regular");
    assert!(h["skipped_files"].to_string().contains("duplicate_id"));
    plant(&s, true, true);
    let prefix = run(
        &s,
        &[
            "chat",
            "ops",
            "--message",
            "20990930-100000",
            "--max-bytes",
            "8000",
            "--json",
        ],
        &b,
    );
    assert_eq!(
        prefix.status.code(),
        Some(65),
        "one prefix spans both suffixes"
    );
    let error: Value = serde_json::from_slice(&prefix.stderr).unwrap();
    assert_eq!(error["error"]["code"], "ambiguous_id");
    let page = ok(
        &s,
        &[
            "chat",
            "ops",
            "--since",
            "20990930-100000-000001-aaaaaa",
            "--max-bytes",
            "2000",
            "--json",
        ],
        &b,
    );
    assert!(page["count"].as_u64().unwrap() > 0);
    assert_eq!(
        page["count"].as_u64().unwrap() as usize,
        page["messages"].as_array().unwrap().len()
    );
    assert!(serde_json::to_vec(&page).unwrap().len() < 2000);
    assert_eq!(page["messages"][0]["id"], "20990930-100000-000002-aaaaaa");
    assert_eq!(page["has_more"], true);
    let searched = ok(&s, &["search", "beta", "--json"], &b);
    let hits = searched["results"].as_array().unwrap();
    assert!(hits
        .iter()
        .any(|h| h["id"] == "20990930-100000-000005-aaaaaa" && h["event"] == "emote"));
    for id in [
        "20990930-100000-000002-aaaaaa",
        "20990930-100000-000004-aaaaaa",
    ] {
        assert!(
            !searched.to_string().contains(id),
            "search enumerates .msg only: {searched}"
        );
    }
}

#[test]
fn watch_snapshot_matrix_matches_ordinary_baseline_with_missing_and_corrupt_cursors() {
    for cursor in ["healthy", "missing", "corrupt"] {
        for extra in [
            vec![],
            vec!["--reason", "channel"],
            vec!["--reason", "mention"],
            vec!["--reason", "mail"],
            vec!["--digest"],
            vec!["--limit", "1"],
        ] {
            for ordinary in [false, true] {
                let (s, a, b) = setup();
                plant(&s, false, ordinary);
                let path = s.mail_root.join(format!(
                    "participants/{}/cursors.json",
                    s.test_participant("beta")
                ));
                if cursor == "missing" {
                    let _ = fs::remove_file(&path);
                } else if cursor == "corrupt" {
                    fs::write(&path, "broken").unwrap();
                }
                let baseline = snapshot(&s, &b, &extra, false);
                let room_base = snapshot(&s, &a, &["--room", "beta"], true);
                if ordinary && extra.is_empty() {
                    assert!(!baseline.is_empty(), "ordinary bound control");
                    assert!(!room_base.is_empty(), "ordinary room control");
                }
                plant(&s, true, false);
                let with = snapshot(&s, &b, &extra, false);
                assert_eq!(baseline, with, "{cursor} {extra:?} ordinary={ordinary}");
                // Explicit room snapshots are the resident-command path.
                let room_with = snapshot(&s, &a, &["--room", "beta"], true);
                assert_eq!(room_base, room_with, "room {cursor} ordinary={ordinary}");
            }
        }
    }
}
#[test]
fn unread_consumption_catchup_crossed_and_discard_match_baseline() {
    for command in [
        vec!["channels", "--json"],
        vec!["chat", "ops", "--peek", "--json"],
        vec!["chat", "ops", "--json"],
        vec!["catchup", "--json"],
        vec!["chat", "ops", "--send", "--body", "response", "--json"],
        vec![
            "chat",
            "ops",
            "--discard-through",
            "20990930-100000-000003-aaaaaa",
            "--json",
        ],
    ] {
        for ordinary in [false, true] {
            if !ordinary && command.contains(&"--discard-through") {
                continue;
            }
            let (s, _, b) = setup();
            plant(&s, false, ordinary);
            let cursor = s.mail_root.join(format!(
                "participants/{}/cursors.json",
                s.test_participant("beta")
            ));
            let before_cursor = fs::read(&cursor).ok();
            let dir = s.mail_root.join("channels/ops/messages");
            let before_files: Vec<_> = fs::read_dir(&dir)
                .unwrap()
                .map(|e| {
                    let p = e.unwrap().path();
                    let bytes = fs::read(&p).unwrap();
                    (p, bytes)
                })
                .collect();
            let mut baseline = ok(&s, &command, &b);
            for e in fs::read_dir(&dir).unwrap() {
                fs::remove_file(e.unwrap().path()).unwrap();
            }
            for (p, bytes) in before_files {
                fs::write(p, bytes).unwrap();
            }
            match before_cursor {
                Some(bytes) => fs::write(&cursor, bytes).unwrap(),
                None => {
                    let _ = fs::remove_file(&cursor);
                }
            }
            plant(&s, true, false);
            let mut with = ok(&s, &command, &b);
            for id in [
                "20990930-100000-000002-aaaaaa",
                "20990930-100000-000004-aaaaaa",
                "20990930-100000-000005-aaaaaa",
            ] {
                assert!(!with.to_string().contains(id), "{command:?} {with}");
            }
            if command.contains(&"--send") {
                if ordinary {
                    assert_eq!(with["crossed"]["unseen"], 2);
                } else {
                    assert!(with.get("crossed").is_none(), "{with}");
                }
                for v in [&mut baseline, &mut with] {
                    v["message"]["id"] = "generated".into();
                }
            }
            if command[0] == "channels" {
                // Total legacy .msg history includes the compatibility event;
                // its unread count and every delivery field must stay equal.
                for v in [&mut baseline, &mut with] {
                    for c in v["channels"].as_array_mut().unwrap() {
                        c.as_object_mut().unwrap().remove("messages");
                    }
                }
            }
            scrub(&mut baseline);
            scrub(&mut with);
            assert_eq!(baseline, with, "{command:?} ordinary={ordinary}");
        }
    }
}
#[test]
fn corrupt_emotes_are_history_diagnostics_never_attention() {
    let (s, _, b) = setup();
    plant(&s, true, false);
    assert!(snapshot(&s, &b, &[], false).is_empty());
    for flag in ["--ack", "--seen-by", "--discard-through"] {
        let out = run(
            &s,
            &[
                "chat",
                "ops",
                flag,
                "20990930-100000-000005-aaaaaa",
                "--json",
            ],
            &b,
        );
        assert_eq!(out.status.code(), Some(66));
    }
    let h = ok(&s, &["chat", "ops", "--history", "20", "--json"], &b);
    assert!(h["skipped_files"]
        .to_string()
        .contains("unreadable_emote: envelope-separator"));
    assert!(h["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["emote_rule"] == "payload-unknown-field"));
    assert!(h["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["event"] == "emote" && m["body"] == "" && m.get("mentions").is_none()));
}
