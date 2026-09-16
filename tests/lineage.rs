mod common;

use common::{assert_success, from_stderr, from_stdout, stderr, stdout, Sandbox};
use post::output::ErrorEnvelope;
use serde_json::Value;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
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
    assert_eq!(shown["voices"][0]["withdrawn_gaps"], 0);
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
    write_body(
        &terms,
        b"Any harness and any model may continue.\nNo content test may reject continuation.\n",
    );
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

    let continued = run_as(
        &sandbox,
        &continuer,
        &["identity", "continue", "ember", "--acknowledge"],
    );
    assert_success(&continued);
    let receipt: Value = from_stdout(&continued);
    assert_eq!(receipt["event"], "continue");
    assert_eq!(receipt["changed"], true);
    assert_eq!(sandbox.read_participant(&continuer)["lineage"], "ember");

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
fn lineage_foreign_voice_withdraw_is_refused_and_revision_withdrawal_keeps_only_a_gap() {
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
    assert!(gap_json["withdrawn_at"].is_string());

    let shown = run_as(&sandbox, &other, &["identity", "show", "ember", "--voices"]);
    assert_success(&shown);
    let shown: Value = from_stdout(&shown);
    assert_eq!(shown["voices"][0]["participant"], founder);
    assert_eq!(shown["voices"][0]["revisions"], 0);
    assert_eq!(shown["voices"][0]["withdrawn_gaps"], 1);
    assert_eq!(shown["rendered_voices"][0], "[post] one voice withdrawn");
    assert!(!shown["rendered_voices"][0]
        .as_str()
        .expect("gap rendering")
        .contains(&founder));
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
    let existing = run_as(&sandbox, &actor, &["identity", "new", "ember"]);
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
