//! Host-wide reports must cost roughly linear time in the participant count.
//!
//! post-gxz: `post who` took 47 s at 3.1k participants and 100 s at 4.1k, and
//! never finished inside a 2 s startup budget. Every participant re-scanned
//! every store, and every participant is itself a store, so the report was
//! quadratic. `post doctor` and `post channels` re-listed every participant
//! once per unrouted message and once per channel. These tests build a store
//! a few thousand participants wide and hold each report to a deadline the
//! old per-participant code could not meet, while checking that the counts
//! it reports are still right.

mod common;

use common::{
    assert_success, from_stdout, register_alpha_beta, run_under_deadline, write_custom_mail,
    Sandbox,
};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const PARTICIPANTS: usize = 2_000;
const UNROUTED: usize = 200;
const CHANNELS: usize = 200;
/// Measured on the devbox in a debug build (2026-09-27): each report takes
/// 0.2-0.35 s on disk or tmpfs. The per-participant code took over 120 s for
/// `who`, and 10.7 s for `channels` and 8.9 s for `doctor` even on tmpfs.
const DEADLINE: Duration = Duration::from_secs(3);

struct WideStore {
    sandbox: Sandbox,
    alpha: PathBuf,
    alpha_ids: Vec<String>,
    beta_ids: Vec<String>,
    sender: String,
}

/// Records written directly, as `participant bind` writes them: a third bound
/// to alpha, a third to beta, a third session-only, every lease current.
fn seed_participants(sandbox: &Sandbox) -> (Vec<String>, Vec<String>) {
    let mut alpha = Vec::new();
    let mut beta = Vec::new();
    for index in 0..PARTICIPANTS {
        let id = format!("scale-{index:05}");
        let workspace = match index % 3 {
            0 => Some("alpha"),
            1 => Some("beta"),
            _ => None,
        };
        let dir = sandbox.mail_root.join("participants").join(&id);
        fs::create_dir_all(&dir).expect("create participant dir");
        let record = json!({
            "version": 1,
            "id": id,
            "harness": "test",
            "conversation_key_digest": format!("{index:064x}"),
            "created": "2026-01-01 00:00:00 +0000",
            "last_seen": "2099-01-01T00:00:00Z",
            "lease_hours": 24,
            "workspace": workspace,
            "workspace_path": Value::Null,
            "lineage": Value::Null,
            "lineage_since": Value::Null
        });
        fs::write(
            dir.join("participant.json"),
            format!("{}\n", serde_json::to_string_pretty(&record).expect("json")),
        )
        .expect("write participant record");
        match workspace {
            Some("alpha") => alpha.push(id),
            Some("beta") => beta.push(id),
            _ => {}
        }
    }
    (alpha, beta)
}

fn wide_store() -> WideStore {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let (alpha_ids, beta_ids) = seed_participants(&sandbox);
    let sender = beta_ids[0].clone();

    // One routed broadcast: its frozen receipt names every alpha participant.
    let sent = sandbox.run_as_participant(
        &[
            "send",
            "--to",
            "workspace:alpha",
            "--body",
            "routed",
            "--json",
        ],
        &sender,
        &beta,
    );
    assert_success(&sent);
    let sent: Value = from_stdout(&sent);
    let routed_id = sent["envelope"]["id"].as_str().expect("mail id").to_owned();
    // One recipient consumes it, before any unrouted mail exists for `read` to
    // route on the way in.
    let reader = alpha_ids[1].clone();
    let read = sandbox.run_as_participant(&["read", &routed_id, "--json"], &reader, &alpha);
    assert_success(&read);

    // Legacy unrouted mail: no receipt, so every report resolves its
    // recipients -- the whole active roster -- for itself.
    for index in 0..UNROUTED {
        let id = format!("20990916-035959-{index:06x}");
        write_custom_mail(
            &sandbox.mail_root.join("alpha/inbox"),
            &id,
            &json!({
                "id": id,
                "from": "beta",
                "to": "alpha",
                "kind": "note",
                "subject": "unrouted",
                "sent": "2026-09-16 03:59:59 -0500",
                "from_participant": sender,
                "address_kind": "workspace"
            }),
            "unrouted",
        );
    }

    // Channels alpha belongs to by legacy workspace membership, so every
    // alpha participant is an effective member of each.
    for index in 0..CHANNELS {
        let dir = sandbox
            .mail_root
            .join("channels")
            .join(format!("scale-{index:02}"));
        fs::create_dir_all(dir.join("messages")).expect("create channel");
        fs::write(
            dir.join("channel.json"),
            serde_json::to_string_pretty(&json!({
                "name": format!("scale-{index:02}"),
                "created": "2026-01-01 00:00:00 +0000",
                "created_by": "beta"
            }))
            .expect("json"),
        )
        .expect("write channel.json");
        fs::write(
            dir.join("members.json"),
            r#"{"alpha": "2026-01-01 00:00:00 +0000"}"#,
        )
        .expect("write members.json");
    }

    WideStore {
        sandbox,
        alpha,
        alpha_ids,
        beta_ids,
        sender,
    }
}

fn timed(
    store: &WideStore,
    args: &[&str],
    actor: &str,
    deadline: Duration,
) -> std::process::Output {
    let started = Instant::now();
    let output = run_under_deadline(&store.sandbox, args, &store.alpha, actor, deadline);
    eprintln!("post {} took {:?}", args.join(" "), started.elapsed());
    output
}

fn listed<'a>(who: &'a Value, id: &str) -> &'a Value {
    who["participants"]
        .as_array()
        .expect("participants")
        .iter()
        .find(|entry| entry["id"] == id)
        .unwrap_or_else(|| panic!("{id} listed"))
}

#[test]
fn who_stays_fast_across_thousands_of_participants() {
    let store = wide_store();
    let actor = &store.alpha_ids[0];
    let output = timed(&store, &["who", "--json"], actor, DEADLINE);
    assert_success(&output);
    let who: Value = from_stdout(&output);

    assert!(who["count"].as_u64().expect("count") >= PARTICIPANTS as u64);
    assert_eq!(who["participant"]["id"], actor.as_str());
    assert_eq!(who["participant"]["unread"]["workspace:alpha"], 1);
    assert_eq!(who["participant"]["pending"]["workspace:alpha"], UNROUTED);
    let other = listed(&who, store.alpha_ids.last().expect("alpha"));
    assert_eq!(other["unread"]["workspace:alpha"], 1);
    assert_eq!(other["pending"]["workspace:alpha"], UNROUTED);
    // Read state is per participant: the reader's cursor, no one else's.
    let reader = listed(&who, &store.alpha_ids[1]);
    assert_eq!(reader["unread"]["workspace:alpha"], 0);
    assert_eq!(reader["pending"]["workspace:alpha"], UNROUTED);
    // The sender sees its own store through sender history, with nothing
    // unread or pending there; a beta bystander does not see it at all.
    let sender = listed(&who, &store.sender);
    assert_eq!(sender["unread"]["workspace:alpha"], 0);
    assert_eq!(sender["pending"]["workspace:alpha"], 0);
    let bystander = listed(&who, &store.beta_ids[1]);
    assert!(bystander["unread"].get("workspace:alpha").is_none());
}

#[test]
fn channels_reads_the_roster_once_not_once_per_channel() {
    let store = wide_store();
    let actor = &store.alpha_ids[0];
    // Explicit state overrides legacy workspace membership both ways.
    let leaver = &store.alpha_ids[1];
    let left = store.sandbox.run_as_participant(
        &["chat", "scale-00", "--leave", "--json"],
        leaver,
        &store.alpha,
    );
    assert_success(&left);
    let joiner = "scale-00002"; // session-only: no workspace membership
    let joined = store.sandbox.run_as_participant(
        &["chat", "scale-01", "--join", "--json"],
        joiner,
        &store.sandbox.path,
    );
    assert_success(&joined);

    let channels = timed(&store, &["channels", "--json"], actor, DEADLINE);
    assert_success(&channels);
    let channels: Value = from_stdout(&channels);
    assert_eq!(channels["pending"], UNROUTED);
    let listed = channels["channels"].as_array().expect("channels");
    assert_eq!(listed.len(), CHANNELS);
    let mut alpha_members = store.alpha_ids.clone();
    alpha_members.push(store.sandbox.test_participant("alpha"));
    alpha_members.sort();
    for channel in listed {
        let mut expected = alpha_members.clone();
        match channel["name"].as_str().expect("name") {
            "scale-00" => expected.retain(|id| id != leaver),
            "scale-01" => expected.push(joiner.to_owned()),
            _ => {}
        }
        expected.sort();
        let participants: Vec<String> =
            serde_json::from_value(channel["participants"].clone()).expect("participants");
        assert_eq!(participants, expected, "{}", channel["name"]);
    }
}

#[test]
fn doctor_resolves_recipients_once_per_store_not_once_per_message() {
    let store = wide_store();
    let actor = &store.alpha_ids[0];
    let doctor = timed(&store, &["doctor", "--json"], actor, DEADLINE);
    let doctor: Value = from_stdout(&doctor);
    assert_eq!(doctor["participant"]["id"], actor.as_str());
    assert_eq!(doctor["pending"]["workspace:alpha"], UNROUTED);
}
