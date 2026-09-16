mod common;

use common::{
    assert_success, from_stderr, from_stdout, register_alpha_beta, stderr, stdout, Sandbox,
};
use post::output::ErrorEnvelope;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Output;

fn run_as(sandbox: &Sandbox, participant: &str, args: &[&str]) -> Output {
    sandbox.run_in_env(
        args,
        None,
        &sandbox.path,
        &[("POST_PARTICIPANT", participant)],
    )
}

fn write_body(path: &Path, body: &[u8]) {
    fs::write(path, body).expect("write body fixture");
}

fn seed_two_lineage_voices(sandbox: &Sandbox, withdraw_ember: bool) -> (String, PathBuf, PathBuf) {
    let actor = sandbox.test_participant("claude-space");
    let other = sandbox.test_participant("pact");
    let body = sandbox.path.join("voice.md");
    assert_success(&run_as(sandbox, &actor, &["identity", "new", "ember"]));
    write_body(&body, b"ember voice\n");
    assert_success(&run_as(
        sandbox,
        &actor,
        &[
            "identity",
            "voice",
            "add",
            "--body-file",
            body.to_str().expect("UTF-8 fixture path"),
        ],
    ));
    if withdraw_ember {
        assert_success(&run_as(sandbox, &actor, &["identity", "voice", "withdraw"]));
    }
    assert_success(&run_as(sandbox, &actor, &["identity", "leave"]));

    assert_success(&run_as(sandbox, &other, &["identity", "new", "ash"]));
    assert_success(&run_as(sandbox, &actor, &["identity", "continue", "ash"]));
    write_body(&body, b"ash voice\n");
    assert_success(&run_as(
        sandbox,
        &actor,
        &[
            "identity",
            "voice",
            "add",
            "--body-file",
            body.to_str().expect("UTF-8 fixture path"),
        ],
    ));
    assert_success(&run_as(sandbox, &actor, &["identity", "leave"]));
    assert_success(&run_as(sandbox, &actor, &["identity", "continue", "ember"]));

    let ember_voice = sandbox
        .mail_root
        .join("lineages/ember/voices")
        .join(format!("{actor}.md"));
    let ash_voice = sandbox
        .mail_root
        .join("lineages/ash/voices")
        .join(format!("{actor}.md"));
    (actor, ember_voice, ash_voice)
}

#[test]
fn lineage_new_founds_and_affiliates_without_exposing_voice_text_in_discovery() {
    let sandbox = Sandbox::new();
    let founder = sandbox.test_participant("claude-space");
    let observer = sandbox.test_participant("pact");

    let output = run_as(&sandbox, &founder, &["identity", "new", "ember"]);
    assert_success(&output);
    let receipt: Value = from_stdout(&output);
    assert_eq!(receipt["event"], "new");
    assert_eq!(receipt["participant"], founder);
    assert_eq!(receipt["lineage"], "ember");

    let lineage: Value = serde_json::from_slice(
        &fs::read(sandbox.mail_root.join("lineages/ember/lineage.json"))
            .expect("read lineage record"),
    )
    .expect("parse lineage record");
    assert_eq!(lineage["version"], 1);
    assert_eq!(lineage["name"], "ember");
    assert_eq!(lineage["founder"], founder);
    assert!(lineage["created"]
        .as_str()
        .is_some_and(|value| !value.is_empty()));
    assert!(lineage["host"]
        .as_str()
        .is_some_and(|value| !value.is_empty()));
    assert_eq!(sandbox.read_participant(&founder)["lineage"], "ember");
    assert!(sandbox.read_participant(&founder)["lineage_since"].is_string());
    assert!(!sandbox
        .mail_root
        .join("lineages/ember/members.json")
        .exists());

    let voice = sandbox.path.join("founder-voice.md");
    write_body(&voice, b"VOICE-TEXT-MUST-STAY-OPT-IN\n");
    let output = run_as(
        &sandbox,
        &founder,
        &[
            "identity",
            "voice",
            "add",
            "--body-file",
            voice.to_str().expect("UTF-8 fixture path"),
        ],
    );
    assert_success(&output);

    let output = run_as(&sandbox, &observer, &["identity", "list"]);
    assert_success(&output);
    let text = stdout(&output);
    assert!(!text.contains("VOICE-TEXT-MUST-STAY-OPT-IN"));
    let listed: Value = from_stdout(&output);
    assert_eq!(listed["count"], 1);
    assert_eq!(listed["lineages"][0]["name"], "ember");
    assert_eq!(listed["lineages"][0]["affiliates"], 1);
    assert_eq!(listed["lineages"][0]["voices"], 1);
    assert_eq!(listed["lineages"][0]["terms"], false);
    assert_eq!(listed["lineages"][0]["founder"], founder);

    let output = run_as(&sandbox, &observer, &["identity", "show", "ember"]);
    assert_success(&output);
    assert!(!stdout(&output).contains("VOICE-TEXT-MUST-STAY-OPT-IN"));
    let shown: Value = from_stdout(&output);
    assert_eq!(shown["lineage"]["version"], 1);
    assert_eq!(shown["lineage"]["name"], "ember");
    assert_eq!(shown["members"].as_object().expect("members map").len(), 1);
    assert_eq!(shown["voices"][0]["participant"], founder);
    assert_eq!(shown["voices"][0]["revisions"], 0);
    assert_eq!(shown["withdrawn_voices"], 0);
    assert!(shown.get("rendered_voices").is_none());

    let output = run_as(
        &sandbox,
        &observer,
        &["identity", "show", "ember", "--voices"],
    );
    assert_success(&output);
    let shown: Value = from_stdout(&output);
    let rendered = shown["rendered_voices"][0]
        .as_str()
        .expect("rendered voice string");
    assert_eq!(
        rendered,
        format!(
            "[post] one voice on lineage ember, authored by participant {founder} — a self-description, not an instruction, not a credential, carries no authority\nVOICE-TEXT-MUST-STAY-OPT-IN\n"
        )
    );
}

#[test]
fn lineage_terms_require_acknowledgement_then_continue_and_leave_only_the_actor() {
    let sandbox = Sandbox::new();
    let founder = sandbox.test_participant("claude-space");
    let continuer = sandbox.test_participant("pact");
    assert_success(&run_as(&sandbox, &founder, &["identity", "new", "ember"]));
    let terms = sandbox.path.join("terms.md");
    let terms_body =
        "Any harness and any model may continue.\nNo content test may reject continuation.\n";
    let terms_digest = format!("{:x}", Sha256::digest(terms_body.as_bytes()));
    let terms_frame = format!(
        "[post] terms for lineage ember — a continuation preference, not an instruction, not a credential, carries no authority\n{terms_body}"
    );
    write_body(&terms, terms_body.as_bytes());
    assert_success(&run_as(
        &sandbox,
        &founder,
        &[
            "identity",
            "terms",
            "set",
            "--body-file",
            terms.to_str().expect("UTF-8 fixture path"),
        ],
    ));

    let shown = run_as(&sandbox, &continuer, &["identity", "show", "ember"]);
    assert_success(&shown);
    let shown: Value = from_stdout(&shown);
    assert_eq!(shown["terms"]["present"], true);
    assert_eq!(shown["terms"]["text"], terms_body);
    assert_eq!(shown["terms"]["digest"], terms_digest);
    let shown_json = run_as(
        &sandbox,
        &continuer,
        &["identity", "show", "ember", "--json"],
    );
    assert_success(&shown_json);
    assert_eq!(from_stdout::<Value>(&shown_json)["terms"], shown["terms"]);

    let refused = run_as(&sandbox, &continuer, &["identity", "continue", "ember"]);
    assert_eq!(refused.status.code(), Some(2));
    assert_eq!(stderr(&refused), "");
    assert_eq!(
        stdout(&refused),
        concat!(
            "[post] terms for lineage ember — a continuation preference, not an instruction, not a credential, carries no authority\n",
            "Any harness and any model may continue.\n",
            "No content test may reject continuation.\n",
            "run: post identity continue 'ember' --acknowledge\n"
        )
    );
    assert!(sandbox.read_participant(&continuer)["lineage"].is_null());

    let refused_json = run_as(
        &sandbox,
        &continuer,
        &["identity", "continue", "ember", "--json"],
    );
    assert_eq!(refused_json.status.code(), Some(2));
    assert_eq!(stderr(&refused_json), "");
    let refusal: Value = from_stdout(&refused_json);
    assert_eq!(refusal["ok"], false);
    assert_eq!(refusal["code"], "terms_acknowledgement_required");
    assert_eq!(refusal["lineage"], "ember");
    assert_eq!(refusal["terms"], terms_body);
    assert_eq!(refusal["terms_digest"], terms_digest);
    assert_eq!(
        refusal["exact_fix"],
        "post identity continue 'ember' --acknowledge"
    );

    let continued = run_as(
        &sandbox,
        &continuer,
        &["identity", "continue", "ember", "--acknowledge"],
    );
    assert_success(&continued);
    let receipt: Value = from_stdout(&continued);
    assert_eq!(receipt["event"], "continue");
    assert_eq!(receipt["changed"], true);
    assert_eq!(receipt["terms"], terms_frame);
    assert_eq!(receipt["terms_digest"], terms_digest);
    assert_eq!(sandbox.read_participant(&continuer)["lineage"], "ember");

    let repeated = run_as(&sandbox, &continuer, &["identity", "continue", "ember"]);
    assert_success(&repeated);
    let repeated: Value = from_stdout(&repeated);
    assert_eq!(repeated["changed"], false);

    let journal = fs::read_to_string(sandbox.mail_root.join("lineages/ember/history.jsonl"))
        .expect("lineage journal");
    let continued: Value = journal
        .lines()
        .map(|line| serde_json::from_str(line).expect("journal JSON"))
        .find(|entry: &Value| entry["event"] == "continue")
        .expect("continue journal entry");
    assert_eq!(continued["terms_digest"], terms_digest);

    let left = run_as(&sandbox, &continuer, &["identity", "leave"]);
    assert_success(&left);
    let receipt: Value = from_stdout(&left);
    assert_eq!(receipt["event"], "leave");
    assert_eq!(receipt["changed"], true);
    assert!(sandbox.read_participant(&continuer)["lineage"].is_null());
    assert!(sandbox.read_participant(&continuer)["lineage_since"].is_null());
    assert_eq!(sandbox.read_participant(&founder)["lineage"], "ember");
}

#[test]
fn lineage_no_own_voice_withdraw_is_refused_and_revision_withdrawal_keeps_a_gap() {
    let sandbox = Sandbox::new();
    let founder = sandbox.test_participant("claude-space");
    let other = sandbox.test_participant("pact");
    assert_success(&run_as(&sandbox, &founder, &["identity", "new", "ember"]));
    assert_success(&run_as(
        &sandbox,
        &other,
        &["identity", "continue", "ember"],
    ));
    let body = sandbox.path.join("voice.md");
    write_body(&body, b"first voice\n");
    assert_success(&run_as(
        &sandbox,
        &founder,
        &[
            "identity",
            "voice",
            "add",
            "--body-file",
            body.to_str().expect("UTF-8 fixture path"),
        ],
    ));
    let founder_voice = sandbox
        .mail_root
        .join("lineages/ember/voices")
        .join(format!("{founder}.md"));

    let refused = run_as(&sandbox, &other, &["identity", "voice", "withdraw"]);
    assert!(!refused.status.success());
    let error: ErrorEnvelope = from_stderr(&refused);
    assert_eq!(error.error.code, "not_found");
    assert_eq!(
        fs::read_to_string(&founder_voice).expect("founder voice"),
        "first voice\n"
    );
    assert!(!sandbox
        .mail_root
        .join("lineages/ember/voices")
        .join(format!("{other}.gap"))
        .exists());

    write_body(&body, b"second voice\n");
    let revised = run_as(
        &sandbox,
        &founder,
        &[
            "identity",
            "voice",
            "add",
            "--body-file",
            body.to_str().expect("UTF-8 fixture path"),
        ],
    );
    assert_success(&revised);
    let receipt: Value = from_stdout(&revised);
    assert_eq!(receipt["event"], "voice_revise");
    assert_eq!(receipt["revisions"], 1);
    let history = sandbox
        .mail_root
        .join("lineages/ember/voices")
        .join(format!("{founder}.history/1.md"));
    assert_eq!(
        fs::read_to_string(&history).expect("voice history"),
        "first voice\n"
    );
    assert_eq!(
        fs::read_to_string(&founder_voice).expect("current voice"),
        "second voice\n"
    );

    let withdrawn = run_as(&sandbox, &founder, &["identity", "voice", "withdraw"]);
    assert_success(&withdrawn);
    assert!(!founder_voice.exists());
    assert!(!history.parent().expect("history parent").exists());
    let gap = sandbox
        .mail_root
        .join("lineages/ember/voices")
        .join(format!("{founder}.gap"));
    let gap_json: Value =
        serde_json::from_slice(&fs::read(&gap).expect("withdrawal gap")).expect("gap JSON");
    assert_eq!(gap_json["version"], 1);
    assert_eq!(gap_json["withdrawals"], 1);
    assert_eq!(gap_json["cleanup_pending"], false);
    assert!(gap_json.get("participant").is_none());
    assert!(gap_json.get("withdrawn_at").is_none());

    let shown = run_as(&sandbox, &other, &["identity", "show", "ember", "--voices"]);
    assert_success(&shown);
    let shown: Value = from_stdout(&shown);
    assert!(shown["voices"].as_array().expect("voices index").is_empty());
    assert_eq!(shown["withdrawn_voices"], 1);
    assert_eq!(shown["rendered_voices"][0], "[post] one voice withdrawn");
}

#[test]
fn lineage_rejects_room_reserved_existing_and_second_affiliation_names() {
    let sandbox = Sandbox::new();
    let actor = sandbox.test_participant("claude-space");
    for name in ["claude-space", "participants", "routing", "bad:name"] {
        let output = run_as(&sandbox, &actor, &["identity", "new", name]);
        assert!(!output.status.success(), "{name} unexpectedly accepted");
        let error: ErrorEnvelope = from_stderr(&output);
        assert_eq!(
            error.error.code,
            "invalid_argument",
            "stderr: {}",
            stderr(&output)
        );
    }

    assert_success(&run_as(&sandbox, &actor, &["identity", "new", "ember"]));
    let replay = run_as(&sandbox, &actor, &["identity", "new", "ember"]);
    assert_success(&replay);
    assert_eq!(from_stdout::<Value>(&replay)["changed"], false);
    let other = sandbox.test_participant("pact");
    let existing = run_as(&sandbox, &other, &["identity", "new", "ember"]);
    assert!(!existing.status.success());
    let error: ErrorEnvelope = from_stderr(&existing);
    assert!(error.error.message.contains("already exists"));

    let second_dir = sandbox.mail_root.join("lineages/ash");
    fs::create_dir_all(second_dir.join("voices")).expect("second lineage");
    fs::write(
        second_dir.join("lineage.json"),
        concat!(
            "{\"version\":1,\"name\":\"ash\",",
            "\"created\":\"now\",\"founder\":\"other\",\"host\":\"test\"}\n"
        ),
    )
    .expect("second lineage record");
    let second = run_as(&sandbox, &actor, &["identity", "continue", "ash"]);
    assert!(!second.status.success());
    let error: ErrorEnvelope = from_stderr(&second);
    assert!(error.error.message.contains("post identity leave"));
    assert_eq!(
        error.error.details.exact_fix.as_deref(),
        Some("post identity leave")
    );
    assert_eq!(sandbox.read_participant(&actor)["lineage"], "ember");
}

#[test]
fn lineage_voice_content_rules_and_truncated_journal_tail_are_enforced() {
    let sandbox = Sandbox::new();
    let actor = sandbox.test_participant("claude-space");
    assert_success(&run_as(&sandbox, &actor, &["identity", "new", "ember"]));
    let body = sandbox.path.join("voice.md");

    write_body(&body, &[b'x'; 4097]);
    let oversize = run_as(
        &sandbox,
        &actor,
        &[
            "identity",
            "voice",
            "add",
            "--body-file",
            body.to_str().expect("UTF-8 fixture path"),
        ],
    );
    assert!(!oversize.status.success());
    let error: ErrorEnvelope = from_stderr(&oversize);
    assert!(error.error.message.contains("maximum is 4096"));

    write_body(&body, b"bad\0voice");
    let controls = run_as(
        &sandbox,
        &actor,
        &[
            "identity",
            "voice",
            "add",
            "--body-file",
            body.to_str().expect("UTF-8 fixture path"),
        ],
    );
    assert!(!controls.status.success());
    let error: ErrorEnvelope = from_stderr(&controls);
    assert!(error.error.message.contains("control characters"));

    write_body(&body, &[0xff, 0xfe]);
    let invalid_utf8 = run_as(
        &sandbox,
        &actor,
        &[
            "identity",
            "voice",
            "add",
            "--body-file",
            body.to_str().expect("UTF-8 fixture path"),
        ],
    );
    assert!(!invalid_utf8.status.success());
    let error: ErrorEnvelope = from_stderr(&invalid_utf8);
    assert!(error.error.message.contains("not valid UTF-8"));

    let journal = sandbox.mail_root.join("lineages/ember/history.jsonl");
    OpenOptions::new()
        .append(true)
        .open(&journal)
        .expect("open lineage journal")
        .write_all(b"{\"at\":\"truncated\"")
        .expect("append truncated tail");
    let shown = run_as(&sandbox, &actor, &["identity", "show", "ember"]);
    assert_success(&shown);
    let shown: Value = from_stdout(&shown);
    assert_eq!(shown["lineage"]["name"], "ember");
}

#[test]
fn lineage_journal_records_each_mutation_with_the_actor() {
    let sandbox = Sandbox::new();
    let founder = sandbox.test_participant("claude-space");
    let other = sandbox.test_participant("pact");
    let body = sandbox.path.join("body.md");
    write_body(&body, b"voice\n");
    assert_success(&run_as(&sandbox, &founder, &["identity", "new", "ember"]));
    assert_success(&run_as(
        &sandbox,
        &other,
        &["identity", "continue", "ember"],
    ));
    assert_success(&run_as(
        &sandbox,
        &founder,
        &[
            "identity",
            "voice",
            "add",
            "--body-file",
            body.to_str().expect("UTF-8 fixture path"),
        ],
    ));
    write_body(&body, b"revised\n");
    assert_success(&run_as(
        &sandbox,
        &founder,
        &[
            "identity",
            "voice",
            "add",
            "--body-file",
            body.to_str().expect("UTF-8 fixture path"),
        ],
    ));
    assert_success(&run_as(
        &sandbox,
        &founder,
        &[
            "identity",
            "terms",
            "set",
            "--body-file",
            body.to_str().expect("UTF-8 fixture path"),
        ],
    ));
    assert_success(&run_as(
        &sandbox,
        &founder,
        &["identity", "voice", "withdraw"],
    ));
    assert_success(&run_as(&sandbox, &other, &["identity", "leave"]));

    let history = fs::read_to_string(sandbox.mail_root.join("lineages/ember/history.jsonl"))
        .expect("lineage journal");
    let entries: Vec<Value> = history
        .lines()
        .map(|line| serde_json::from_str(line).expect("journal entry JSON"))
        .collect();
    let events: Vec<&str> = entries
        .iter()
        .map(|entry| entry["event"].as_str().expect("journal event"))
        .collect();
    assert_eq!(
        events,
        [
            "new",
            "continue",
            "voice_add",
            "voice_revise",
            "terms_set",
            "voice_withdraw",
            "leave"
        ]
    );
    assert_eq!(entries[0]["participant"], founder);
    assert_eq!(entries[1]["participant"], other);
    assert!(entries.iter().all(|entry| entry["at"].is_string()));
}

#[test]
fn lineage_withdrawn_voice_is_anonymous_in_every_show_mode() {
    let sandbox = Sandbox::new();
    let founder = sandbox.test_participant("claude-space");
    let author = sandbox.test_participant("pact");
    let body = sandbox.path.join("voice.md");
    write_body(&body, b"private withdrawn voice\n");
    assert_success(&run_as(&sandbox, &founder, &["identity", "new", "ember"]));
    assert_success(&run_as(
        &sandbox,
        &author,
        &["identity", "continue", "ember"],
    ));
    assert_success(&run_as(
        &sandbox,
        &author,
        &[
            "identity",
            "voice",
            "add",
            "--body-file",
            body.to_str().expect("UTF-8 fixture path"),
        ],
    ));
    assert_success(&run_as(
        &sandbox,
        &author,
        &["identity", "voice", "withdraw"],
    ));
    assert_success(&run_as(&sandbox, &author, &["identity", "leave"]));

    for args in [
        vec!["identity", "show", "ember"],
        vec!["identity", "show", "ember", "--json"],
        vec!["identity", "show", "ember", "--voices"],
        vec!["identity", "show", "ember", "--voices", "--json"],
    ] {
        let shown = run_as(&sandbox, &founder, &args);
        assert_success(&shown);
        assert!(
            !stdout(&shown).contains(&author),
            "withdrawn author leaked for {args:?}: {}",
            stdout(&shown)
        );
        let shown: Value = from_stdout(&shown);
        assert_eq!(shown["withdrawn_voices"], 1);
        assert!(shown["voices"].as_array().expect("voices index").is_empty());
    }
}

#[test]
fn lineage_pending_withdrawal_hides_stale_content_and_readd_preserves_gap() {
    let sandbox = Sandbox::new();
    let actor = sandbox.test_participant("claude-space");
    let body = sandbox.path.join("voice.md");
    write_body(&body, b"stale voice must stay hidden\n");
    assert_success(&run_as(&sandbox, &actor, &["identity", "new", "ember"]));
    assert_success(&run_as(
        &sandbox,
        &actor,
        &[
            "identity",
            "voice",
            "add",
            "--body-file",
            body.to_str().expect("UTF-8 fixture path"),
        ],
    ));

    let voices = sandbox.mail_root.join("lineages/ember/voices");
    let current = voices.join(format!("{actor}.md"));
    let gap = voices.join(format!("{actor}.gap"));
    fs::write(
        &gap,
        "{\"version\":1,\"withdrawals\":1,\"cleanup_pending\":true}\n",
    )
    .expect("pending gap");
    let history = voices.join(format!("{actor}.history"));
    fs::create_dir_all(&history).expect("stale history");
    fs::write(history.join("1.md"), "older voice").expect("stale history content");
    let temporary = voices.join(format!(".{actor}.md.crash.tmp"));
    fs::write(&temporary, "stale temporary").expect("stale voice temporary");

    let shown = run_as(&sandbox, &actor, &["identity", "show", "ember", "--voices"]);
    assert_success(&shown);
    assert!(!stdout(&shown).contains("stale voice must stay hidden"));
    let shown: Value = from_stdout(&shown);
    assert!(shown["voices"].as_array().expect("voices index").is_empty());
    assert_eq!(shown["withdrawn_voices"], 1);
    assert_eq!(
        shown["rendered_voices"],
        serde_json::json!(["[post] one voice withdrawn"])
    );

    let pending_withdraw = run_as(&sandbox, &actor, &["identity", "voice", "withdraw"]);
    assert_success(&pending_withdraw);
    let pending_receipt: Value = from_stdout(&pending_withdraw);
    assert_eq!(pending_receipt["changed"], true);
    assert!(pending_receipt.get("hint").is_none());
    assert!(!current.exists());
    assert!(!history.exists());
    assert!(!temporary.exists());
    let repaired_gap: Value =
        serde_json::from_slice(&fs::read(&gap).expect("repaired gap")).expect("repaired gap JSON");
    assert_eq!(repaired_gap["cleanup_pending"], false);
    let history_jsonl = fs::read_to_string(sandbox.mail_root.join("lineages/ember/history.jsonl"))
        .expect("lineage journal");
    assert_eq!(
        history_jsonl
            .matches("\"event\":\"voice_withdraw\"")
            .count(),
        1
    );

    write_body(&body, b"new voice\n");
    assert_success(&run_as(
        &sandbox,
        &actor,
        &[
            "identity",
            "voice",
            "add",
            "--body-file",
            body.to_str().expect("UTF-8 fixture path"),
        ],
    ));
    assert_eq!(
        fs::read_to_string(&current).expect("new current voice"),
        "new voice\n"
    );
    assert!(!history.exists());
    assert!(!temporary.exists());
    let gap_json: Value =
        serde_json::from_slice(&fs::read(&gap).expect("durable gap")).expect("durable gap JSON");
    assert_eq!(gap_json["withdrawals"], 1);
    assert_eq!(gap_json["cleanup_pending"], false);

    let shown = run_as(&sandbox, &actor, &["identity", "show", "ember", "--voices"]);
    assert_success(&shown);
    let shown: Value = from_stdout(&shown);
    assert_eq!(shown["voices"][0]["participant"], actor);
    assert_eq!(shown["withdrawn_voices"], 1);
    assert!(shown["rendered_voices"][0]
        .as_str()
        .expect("current rendered voice")
        .contains("new voice"));
    assert_eq!(shown["rendered_voices"][1], "[post] one voice withdrawn");

    let leftover = voices.join(format!(".{actor}.md.leftover.tmp"));
    fs::write(&leftover, "leftover").expect("leftover temporary");
    assert_success(&run_as(
        &sandbox,
        &actor,
        &["identity", "voice", "withdraw"],
    ));
    assert!(!leftover.exists());
    let gap_json: Value =
        serde_json::from_slice(&fs::read(&gap).expect("second gap")).expect("second gap JSON");
    assert_eq!(gap_json["withdrawals"], 2);
    assert_eq!(gap_json["cleanup_pending"], false);
}

#[test]
fn lineage_leave_clears_authoritative_affiliation_despite_broken_metadata() {
    for mode in ["missing", "malformed", "room-collision"] {
        let sandbox = Sandbox::new();
        let actor = sandbox.test_participant("claude-space");
        assert_success(&run_as(&sandbox, &actor, &["identity", "new", "ember"]));
        let record = sandbox.mail_root.join("lineages/ember/lineage.json");
        match mode {
            "missing" => fs::remove_file(&record).expect("remove lineage record"),
            "malformed" => fs::write(&record, "not JSON\n").expect("corrupt lineage record"),
            "room-collision" => {
                let room = sandbox.path.join("ember-room");
                fs::create_dir_all(&room).expect("room path");
                let mut rooms: Value = serde_json::from_slice(
                    &fs::read(sandbox.mail_root.join("rooms.json")).expect("rooms"),
                )
                .expect("rooms JSON");
                rooms["ember"] = Value::String(room.to_string_lossy().into_owned());
                fs::write(
                    sandbox.mail_root.join("rooms.json"),
                    format!(
                        "{}\n",
                        serde_json::to_string_pretty(&rooms).expect("rooms JSON")
                    ),
                )
                .expect("room collision");
            }
            _ => unreachable!(),
        }
        let left = run_as(&sandbox, &actor, &["identity", "leave"]);
        assert_success(&left);
        let receipt: Value = from_stdout(&left);
        assert_eq!(receipt["changed"], true, "mode {mode}");
        assert!(sandbox.read_participant(&actor)["lineage"].is_null());
        assert!(sandbox.read_participant(&actor)["lineage_since"].is_null());
    }
}

#[test]
fn lineage_list_skips_unreadable_records_and_reports_warnings() {
    let sandbox = Sandbox::new();
    let good = sandbox.test_participant("claude-space");
    let bad = sandbox.test_participant("pact");
    assert_success(&run_as(&sandbox, &good, &["identity", "new", "ember"]));
    assert_success(&run_as(&sandbox, &bad, &["identity", "new", "ash"]));
    fs::write(
        sandbox.mail_root.join("lineages/ash/lineage.json"),
        "not JSON\n",
    )
    .expect("corrupt lineage record");

    let listed = run_as(&sandbox, &good, &["identity", "list"]);
    assert_success(&listed);
    let listed: Value = from_stdout(&listed);
    assert_eq!(listed["count"], 1);
    assert_eq!(listed["lineages"][0]["name"], "ember");
    assert_eq!(listed["warnings"].as_array().expect("warnings").len(), 1);
    assert!(listed["warnings"][0]
        .as_str()
        .expect("warning text")
        .contains("ash"));
}

#[test]
fn lineage_post_commit_journal_and_stdout_failures_report_committed_state() {
    let sandbox = Sandbox::new();
    let actor = sandbox.test_participant("claude-space");
    fs::create_dir_all(sandbox.mail_root.join("lineages/ember/history.jsonl"))
        .expect("unwritable journal shape");
    let created = run_as(&sandbox, &actor, &["identity", "new", "ember"]);
    assert_success(&created);
    let receipt: Value = from_stdout(&created);
    assert_eq!(receipt["changed"], true);
    assert_eq!(receipt["warnings"].as_array().expect("warnings").len(), 1);
    assert_eq!(sandbox.read_participant(&actor)["lineage"], "ember");

    let replay = run_as(&sandbox, &actor, &["identity", "new", "ember"]);
    assert_success(&replay);
    assert_eq!(from_stdout::<Value>(&replay)["changed"], false);

    let sandbox = Sandbox::new();
    let founder = sandbox.test_participant("claude-space");
    let continuer = sandbox.test_participant("pact");
    assert_success(&run_as(&sandbox, &founder, &["identity", "new", "ember"]));
    let journal = sandbox.mail_root.join("lineages/ember/history.jsonl");
    fs::remove_file(&journal).expect("remove journal");
    fs::create_dir(&journal).expect("unwritable journal shape");
    let continued = run_as(&sandbox, &continuer, &["identity", "continue", "ember"]);
    assert_success(&continued);
    assert_eq!(
        from_stdout::<Value>(&continued)["warnings"]
            .as_array()
            .expect("warnings")
            .len(),
        1
    );
    assert_eq!(sandbox.read_participant(&continuer)["lineage"], "ember");
    let left = run_as(&sandbox, &continuer, &["identity", "leave"]);
    assert_success(&left);
    assert_eq!(
        from_stdout::<Value>(&left)["warnings"]
            .as_array()
            .expect("warnings")
            .len(),
        1
    );
    assert!(sandbox.read_participant(&continuer)["lineage"].is_null());

    let sandbox = Sandbox::new();
    let (alpha, _) = register_alpha_beta(&sandbox);
    let actor = sandbox.test_participant("alpha");
    let failed = sandbox.run_in_broken_stdout(&["identity", "new", "ember"], &alpha);
    assert_eq!(failed.status.code(), Some(70));
    let error: ErrorEnvelope = from_stderr(&failed);
    assert_eq!(error.error.code, "delivered_output_failure");
    assert_eq!(sandbox.read_participant(&actor)["lineage"], "ember");
}

#[test]
fn lineage_append_repairs_torn_tail_before_future_events() {
    let sandbox = Sandbox::new();
    let actor = sandbox.test_participant("claude-space");
    let body = sandbox.path.join("terms.md");
    write_body(&body, b"terms\n");
    assert_success(&run_as(&sandbox, &actor, &["identity", "new", "ember"]));
    let journal = sandbox.mail_root.join("lineages/ember/history.jsonl");
    OpenOptions::new()
        .append(true)
        .open(&journal)
        .expect("journal")
        .write_all(b"{\"at\":\"torn\"")
        .expect("torn tail");
    for _ in 0..2 {
        assert_success(&run_as(
            &sandbox,
            &actor,
            &[
                "identity",
                "terms",
                "set",
                "--body-file",
                body.to_str().expect("UTF-8 fixture path"),
            ],
        ));
    }
    let entries: Vec<Value> = fs::read_to_string(&journal)
        .expect("repaired journal")
        .lines()
        .map(|line| serde_json::from_str(line).expect("every journal line parses"))
        .collect();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0]["event"], "new");
    assert_eq!(entries[1]["event"], "terms_set");
    assert_eq!(entries[2]["event"], "terms_set");
}

#[test]
fn lineage_withdraw_after_leave_resolves_one_voice_and_refuses_ambiguity() {
    let sandbox = Sandbox::new();
    let actor = sandbox.test_participant("claude-space");
    let body = sandbox.path.join("voice.md");
    write_body(&body, b"voice\n");
    assert_success(&run_as(&sandbox, &actor, &["identity", "new", "ember"]));
    assert_success(&run_as(
        &sandbox,
        &actor,
        &[
            "identity",
            "voice",
            "add",
            "--body-file",
            body.to_str().expect("UTF-8 fixture path"),
        ],
    ));
    assert_success(&run_as(&sandbox, &actor, &["identity", "leave"]));
    assert_success(&run_as(
        &sandbox,
        &actor,
        &["identity", "voice", "withdraw"],
    ));
    assert!(sandbox
        .mail_root
        .join("lineages/ember/voices")
        .join(format!("{actor}.gap"))
        .exists());

    let sandbox = Sandbox::new();
    let actor = sandbox.test_participant("claude-space");
    let other = sandbox.test_participant("pact");
    let body = sandbox.path.join("voice.md");
    write_body(&body, b"voice\n");
    assert_success(&run_as(&sandbox, &actor, &["identity", "new", "ember"]));
    assert_success(&run_as(
        &sandbox,
        &actor,
        &[
            "identity",
            "voice",
            "add",
            "--body-file",
            body.to_str().expect("UTF-8 fixture path"),
        ],
    ));
    assert_success(&run_as(&sandbox, &actor, &["identity", "leave"]));
    assert_success(&run_as(&sandbox, &other, &["identity", "new", "ash"]));
    assert_success(&run_as(&sandbox, &actor, &["identity", "continue", "ash"]));
    assert_success(&run_as(
        &sandbox,
        &actor,
        &["identity", "voice", "withdraw"],
    ));
    assert!(sandbox
        .mail_root
        .join("lineages/ember/voices")
        .join(format!("{actor}.gap"))
        .exists());

    let sandbox = Sandbox::new();
    let actor = sandbox.test_participant("claude-space");
    let body = sandbox.path.join("voice.md");
    write_body(&body, b"voice\n");
    assert_success(&run_as(&sandbox, &actor, &["identity", "new", "ember"]));
    assert_success(&run_as(
        &sandbox,
        &actor,
        &[
            "identity",
            "voice",
            "add",
            "--body-file",
            body.to_str().expect("UTF-8 fixture path"),
        ],
    ));
    assert_success(&run_as(&sandbox, &actor, &["identity", "leave"]));
    let ash = sandbox.mail_root.join("lineages/ash/voices");
    fs::create_dir_all(&ash).expect("second lineage voices");
    fs::write(ash.join(format!("{actor}.md")), "other voice\n").expect("second voice");
    let refused = run_as(&sandbox, &actor, &["identity", "voice", "withdraw"]);
    assert!(!refused.status.success());
    let error: ErrorEnvelope = from_stderr(&refused);
    assert_eq!(error.error.code, "invalid_argument");
    assert!(error.error.message.contains("ash"));
    assert!(error.error.message.contains("ember"));
}

#[test]
fn lineage_withdraw_retry_on_current_gap_does_not_delete_another_lineage_voice() {
    let sandbox = Sandbox::new();
    let (actor, ember_voice, ash_voice) = seed_two_lineage_voices(&sandbox, true);
    assert!(!ember_voice.exists());
    assert_eq!(
        fs::read_to_string(&ash_voice).expect("ash voice before retry"),
        "ash voice\n"
    );

    for _ in 0..2 {
        let retry = run_as(&sandbox, &actor, &["identity", "voice", "withdraw"]);
        assert_success(&retry);
        let receipt: Value = from_stdout(&retry);
        assert_eq!(receipt["changed"], false);
        assert_eq!(
            receipt["hint"],
            "no current voice here; to withdraw a voice on another lineage, continue that lineage first"
        );
        assert_eq!(
            fs::read_to_string(&ash_voice).expect("ash voice survives retry"),
            "ash voice\n"
        );
    }
}

#[test]
fn lineage_pending_withdraw_retry_finishes_current_lineage_without_falling_through() {
    let sandbox = Sandbox::new();
    let (actor, ember_voice, ash_voice) = seed_two_lineage_voices(&sandbox, false);
    let voices = ember_voice.parent().expect("ember voices");
    let gap = voices.join(format!("{actor}.gap"));
    fs::write(
        &gap,
        "{\"version\":1,\"withdrawals\":1,\"cleanup_pending\":true}\n",
    )
    .expect("pending ember gap");

    let retry = run_as(&sandbox, &actor, &["identity", "voice", "withdraw"]);
    assert_success(&retry);
    let receipt: Value = from_stdout(&retry);
    assert_eq!(receipt["changed"], true);
    assert!(receipt.get("hint").is_none());
    assert!(!ember_voice.exists());
    assert_eq!(
        fs::read_to_string(&ash_voice).expect("ash voice survives pending retry"),
        "ash voice\n"
    );
    let gap_json: Value = serde_json::from_slice(&fs::read(&gap).expect("finished ember gap"))
        .expect("finished ember gap JSON");
    assert_eq!(gap_json["cleanup_pending"], false);
    let journal = fs::read_to_string(sandbox.mail_root.join("lineages/ember/history.jsonl"))
        .expect("ember journal");
    assert_eq!(journal.matches("\"event\":\"voice_withdraw\"").count(), 1);
}

#[test]
fn lineage_bad_gap_skips_list_entry_but_show_warns_and_renders_healthy_voices() {
    let sandbox = Sandbox::new();
    let good = sandbox.test_participant("claude-space");
    let bad = sandbox.test_participant("pact");
    let healthy = sandbox.test_participant("agent-memory");
    let body = sandbox.path.join("voice.md");
    assert_success(&run_as(&sandbox, &good, &["identity", "new", "ember"]));
    assert_success(&run_as(&sandbox, &bad, &["identity", "new", "ash"]));
    write_body(&body, b"voice hidden by bad gap\n");
    assert_success(&run_as(
        &sandbox,
        &bad,
        &[
            "identity",
            "voice",
            "add",
            "--body-file",
            body.to_str().expect("UTF-8 fixture path"),
        ],
    ));
    assert_success(&run_as(
        &sandbox,
        &healthy,
        &["identity", "continue", "ash"],
    ));
    write_body(&body, b"healthy voice remains visible\n");
    assert_success(&run_as(
        &sandbox,
        &healthy,
        &[
            "identity",
            "voice",
            "add",
            "--body-file",
            body.to_str().expect("UTF-8 fixture path"),
        ],
    ));
    let voices = sandbox.mail_root.join("lineages/ash/voices");
    fs::write(voices.join(format!("{bad}.gap")), "not JSON\n").expect("corrupt gap");
    fs::write(
        voices.join("ghost.gap"),
        "{\"version\":2,\"withdrawals\":1,\"cleanup_pending\":false}\n",
    )
    .expect("unsupported gap version");

    let listed = run_as(&sandbox, &good, &["identity", "list"]);
    assert_success(&listed);
    let listed: Value = from_stdout(&listed);
    assert_eq!(listed["count"], 1);
    assert_eq!(listed["lineages"][0]["name"], "ember");
    assert_eq!(
        listed["warnings"].as_array().expect("list warnings").len(),
        1
    );

    let shown = run_as(&sandbox, &good, &["identity", "show", "ash", "--voices"]);
    assert_success(&shown);
    let shown_text = stdout(&shown);
    assert!(!shown_text.contains("voice hidden by bad gap"));
    assert!(shown_text.contains("healthy voice remains visible"));
    let shown: Value = from_stdout(&shown);
    assert_eq!(shown["voices"].as_array().expect("healthy voices").len(), 1);
    assert_eq!(shown["voices"][0]["participant"], healthy);
    assert_eq!(
        shown["warnings"].as_array().expect("show warnings").len(),
        2
    );
    assert!(!shown["warnings"].to_string().contains(&bad));
}

#[test]
fn lineage_new_repair_finishes_founder_affiliation_after_interrupted_create() {
    let sandbox = Sandbox::new();
    let actor = sandbox.test_participant("claude-space");
    let dir = sandbox.mail_root.join("lineages/ember");
    fs::create_dir_all(dir.join("voices")).expect("interrupted lineage directory");
    fs::write(
        dir.join("lineage.json"),
        format!(
            "{{\"version\":1,\"name\":\"ember\",\"created\":\"now\",\"founder\":\"{actor}\",\"host\":\"test\"}}\n"
        ),
    )
    .expect("interrupted lineage record");
    assert!(sandbox.read_participant(&actor)["lineage"].is_null());

    let repaired = run_as(&sandbox, &actor, &["identity", "new", "ember"]);
    assert_success(&repaired);
    let receipt: Value = from_stdout(&repaired);
    assert_eq!(receipt["changed"], true);
    assert_eq!(receipt["lineage"], "ember");
    assert_eq!(sandbox.read_participant(&actor)["lineage"], "ember");
    assert!(sandbox.read_participant(&actor)["lineage_since"].is_string());
    let journal = fs::read_to_string(dir.join("history.jsonl")).expect("repaired journal");
    assert_eq!(journal.matches("\"event\":\"new\"").count(), 1);
}

#[test]
fn lineage_terms_use_voice_content_rules_and_show_reads_record_version() {
    let sandbox = Sandbox::new();
    let actor = sandbox.test_participant("claude-space");
    assert_success(&run_as(&sandbox, &actor, &["identity", "new", "ember"]));
    let body = sandbox.path.join("terms.md");
    write_body(&body, &[b'x'; 4097]);
    let oversize = run_as(
        &sandbox,
        &actor,
        &[
            "identity",
            "terms",
            "set",
            "--body-file",
            body.to_str().expect("UTF-8 fixture path"),
        ],
    );
    assert!(!oversize.status.success());
    assert!(from_stderr::<ErrorEnvelope>(&oversize)
        .error
        .message
        .contains("maximum is 4096"));
    write_body(&body, b"bad\0terms");
    let controls = run_as(
        &sandbox,
        &actor,
        &[
            "identity",
            "terms",
            "set",
            "--body-file",
            body.to_str().expect("UTF-8 fixture path"),
        ],
    );
    assert!(!controls.status.success());
    assert!(from_stderr::<ErrorEnvelope>(&controls)
        .error
        .message
        .contains("control characters"));

    #[cfg(unix)]
    {
        let target = sandbox.path.join("terms-target.md");
        let link = sandbox.path.join("terms-link.md");
        write_body(&target, b"valid terms behind a symlink\n");
        std::os::unix::fs::symlink(&target, &link).expect("terms symlink");
        let symlinked = run_as(
            &sandbox,
            &actor,
            &[
                "identity",
                "terms",
                "set",
                "--body-file",
                link.to_str().expect("UTF-8 fixture path"),
            ],
        );
        assert!(!symlinked.status.success());
        assert!(!sandbox.mail_root.join("lineages/ember/terms.md").exists());
    }

    let record = sandbox.mail_root.join("lineages/ember/lineage.json");
    let mut lineage: Value =
        serde_json::from_slice(&fs::read(&record).expect("lineage record")).expect("lineage JSON");
    lineage["version"] = Value::from(7);
    fs::write(
        &record,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&lineage).expect("lineage JSON")
        ),
    )
    .expect("rewrite lineage version");
    let shown = run_as(&sandbox, &actor, &["identity", "show", "ember"]);
    assert_success(&shown);
    assert_eq!(from_stdout::<Value>(&shown)["lineage"]["version"], 7);
}
