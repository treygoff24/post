//! Where a damaged store's error lands.
//!
//! `post who` and `post doctor` answer every participant from one read of the
//! host's stores (post-gxz). Each participant's answer must still be the one
//! its own walk of the stores gave: a receipt names it and the walk stops
//! there, so a later unreadable receipt, or an inbox that cannot be listed,
//! fails only the participants whose walk reaches it. These fixtures pin the
//! outcomes the per-participant code gave for an unlistable inbox, an
//! unreadable receipt, a corrupt receipt, and a blocked route.

mod common;

use common::{assert_success, from_stdout, register_alpha_beta, write_custom_mail, Sandbox};
use serde_json::{json, Value};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;

/// chmod 000 for the life of the guard; the sandbox cannot remove a tree it
/// cannot read, so the mode comes back even when an assertion panics.
struct Locked {
    path: PathBuf,
    mode: u32,
}

impl Locked {
    fn new(path: &Path) -> Self {
        let mode = fs::metadata(path).expect("stat").permissions().mode();
        fs::set_permissions(path, fs::Permissions::from_mode(0o000)).expect("chmod 000");
        Self {
            path: path.to_path_buf(),
            mode,
        }
    }
}

impl Drop for Locked {
    fn drop(&mut self) {
        fs::set_permissions(&self.path, fs::Permissions::from_mode(self.mode))
            .expect("restore mode");
    }
}

fn bind(sandbox: &Sandbox, key: &str, cwd: &Path, workspace: &str) -> String {
    sandbox.bind_claude(key, cwd, Some(workspace))["id"]
        .as_str()
        .expect("participant id")
        .to_owned()
}

fn send(sandbox: &Sandbox, sender: &str, cwd: &Path, to: &str) -> String {
    let output = sandbox.run_as_participant(
        &["send", "--to", to, "--body", "hello", "--json"],
        sender,
        cwd,
    );
    assert_success(&output);
    let sent: Value = from_stdout(&output);
    sent["envelope"]["id"].as_str().expect("mail id").to_owned()
}

fn json_of(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout is not JSON ({error}): {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn check_ids(doctor: &Value) -> Vec<String> {
    doctor["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .map(|check| check["id"].as_str().expect("id").to_owned())
        .collect()
}

fn check<'a>(doctor: &'a Value, id: &str) -> &'a Value {
    doctor["checks"]
        .as_array()
        .expect("checks")
        .iter()
        .find(|check| check["id"] == id)
        .unwrap_or_else(|| panic!("no {id} check in {:?}", check_ids(doctor)))
}

#[test]
fn doctor_reports_an_unlistable_inbox_on_the_address_that_owns_it() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let sender = bind(&sandbox, "sender", &alpha, "alpha");
    let reader = bind(&sandbox, "reader", &beta, "beta");
    let bystander = bind(&sandbox, "bystander", &alpha, "alpha");
    send(&sandbox, &sender, &alpha, "workspace:beta");
    let _locked = Locked::new(&sandbox.mail_root.join("beta/inbox"));

    // The receipt names the reader, so its walk never lists beta's inbox: the
    // address resolves and only its own pending projection fails.
    let doctor = json_of(&sandbox.run_as_participant(&["doctor", "--json"], &reader, &beta));
    assert!(
        !check_ids(&doctor).contains(&"projection.addresses".to_owned()),
        "{:?}",
        check_ids(&doctor)
    );
    let owned = check(&doctor, "projection.workspace.beta");
    assert_eq!(
        owned["path"],
        sandbox.mail_root.join("beta/routing").display().to_string()
    );
    assert_eq!(doctor["pending"][format!("participant:{reader}")], 0);

    // Nothing in beta names the bystander, so its walk lists that inbox and
    // address discovery fails there.
    let doctor = json_of(&sandbox.run_as_participant(&["doctor", "--json"], &bystander, &alpha));
    assert!(check(&doctor, "projection.addresses")["message"]
        .as_str()
        .expect("message")
        .contains("beta/inbox"));
    assert_eq!(doctor["pending"], json!({}));

    // `who` projects the bystander too, so it fails as it always did.
    let who = sandbox.run_as_participant(&["who", "--json"], &reader, &beta);
    assert!(!who.status.success());
    assert!(String::from_utf8_lossy(&who.stderr).contains("beta/inbox"));
}

#[test]
fn an_unreadable_receipt_fails_only_the_walks_that_reach_it() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let reader = bind(&sandbox, "reader", &alpha, "alpha");
    let sender = bind(&sandbox, "sender", &beta, "beta");
    let outsider = bind(&sandbox, "outsider", &beta, "beta");
    for _ in 0..3 {
        send(&sandbox, &sender, &beta, "workspace:alpha");
    }

    // A receipt that cannot be read, placed after a readable one in directory
    // order: every alpha walk has stopped at a receipt naming it by then.
    let routing = sandbox.mail_root.join("alpha/routing");
    let json_entries = || -> Vec<PathBuf> {
        fs::read_dir(&routing)
            .expect("list routing")
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
            .collect()
    };
    let unreadable = (0..64)
        .map(|index| routing.join(format!("20990101-000000-{index:06x}.json")))
        .find(|candidate| {
            fs::write(candidate, "{}").expect("write receipt");
            if json_entries().first() != Some(candidate) {
                return true;
            }
            fs::remove_file(candidate).expect("remove candidate");
            false
        })
        .expect("a name that does not list first");
    assert!(json_entries().len() >= 4, "three readable receipts and it");
    let _locked = Locked::new(&unreadable);

    // Scoped to alpha, every projected walk stops before it.
    let who = sandbox.run_as_participant(&["who", "--room", "alpha", "--json"], &reader, &alpha);
    assert_success(&who);
    let who: Value = from_stdout(&who);
    assert_eq!(who["participant"]["unread"]["workspace:alpha"], 3);

    let doctor = json_of(&sandbox.run_as_participant(&["doctor", "--json"], &reader, &alpha));
    let ids = check_ids(&doctor);
    assert!(!ids.contains(&"projection.addresses".to_owned()), "{ids:?}");
    assert_eq!(doctor["pending"]["workspace:alpha"], 0);
    assert!(ids
        .iter()
        .any(|id| id.starts_with("routing.receipt.workspace.alpha.")));

    // Nothing in alpha names the outsider: its walk reaches the receipt.
    let doctor = json_of(&sandbox.run_as_participant(&["doctor", "--json"], &outsider, &beta));
    let stem = unreadable
        .file_stem()
        .and_then(|value| value.to_str())
        .expect("stem");
    assert!(check(&doctor, "projection.addresses")["message"]
        .as_str()
        .expect("message")
        .contains(stem));
    let who = sandbox.run_as_participant(&["who", "--json"], &reader, &alpha);
    assert!(!who.status.success());
}

#[test]
fn a_corrupt_receipt_is_skipped_and_warned_about_once() {
    let sandbox = Sandbox::new();
    let (alpha, beta) = register_alpha_beta(&sandbox);
    let reader = bind(&sandbox, "reader", &alpha, "alpha");
    let sender = bind(&sandbox, "sender", &beta, "beta");
    bind(&sandbox, "outsider", &beta, "beta");
    send(&sandbox, &sender, &beta, "workspace:alpha");
    fs::write(
        sandbox
            .mail_root
            .join("alpha/routing/20990101-000000-c0ffee.json"),
        "not json",
    )
    .expect("write corrupt receipt");

    let who = sandbox.run_as_participant(&["who", "--json"], &reader, &alpha);
    assert!(
        who.status.success(),
        "{}",
        String::from_utf8_lossy(&who.stderr)
    );
    let stderr = String::from_utf8_lossy(&who.stderr);
    assert_eq!(
        stderr.matches("corrupt routing receipt skipped").count(),
        1,
        "{stderr}"
    );
    let who = json_of(&who);
    assert_eq!(who["participant"]["unread"]["workspace:alpha"], 1);
}

#[test]
fn blocked_unrouted_mail_is_held_never_pending() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    // rules.json blocks every route into agent-memory.
    let armed_path = sandbox.home.join("agent-memory");
    let armed = bind(&sandbox, "armed", &armed_path, "agent-memory");
    let reader = bind(&sandbox, "reader", &alpha, "alpha");
    let unrouted = |workspace: &str, id: &str| {
        write_custom_mail(
            &sandbox.mail_root.join(workspace).join("inbox"),
            id,
            &json!({
                "id": id,
                "from": "beta",
                "to": workspace,
                "kind": "note",
                "subject": "unrouted",
                "sent": "2026-09-16 03:59:59 -0500",
                "address_kind": "workspace"
            }),
            "unrouted",
        );
    };
    unrouted("agent-memory", "20990916-035959-b10c00");
    unrouted("alpha", "20990916-035959-0a0a00");

    let who = sandbox.run_as_participant(&["who", "--json"], &armed, &armed_path);
    assert_success(&who);
    let who: Value = from_stdout(&who);
    assert_eq!(who["participant"]["pending"]["workspace:agent-memory"], 0);
    assert_eq!(listed(&who, &reader)["pending"]["workspace:alpha"], 1);

    let doctor = json_of(&sandbox.run_as_participant(&["doctor", "--json"], &armed, &armed_path));
    assert_eq!(doctor["pending"]["workspace:agent-memory"], 0);
    assert!(
        check(&doctor, "routing.held.workspace.agent-memory")["message"]
            .as_str()
            .expect("message")
            .contains("20990916-035959-b10c00")
    );
    let channels = sandbox.run_as_participant(&["channels", "--json"], &armed, &armed_path);
    assert_success(&channels);
    assert_eq!(from_stdout::<Value>(&channels)["pending"], 0);
}

fn listed<'a>(who: &'a Value, id: &str) -> &'a Value {
    who["participants"]
        .as_array()
        .expect("participants")
        .iter()
        .find(|entry| entry["id"] == id)
        .unwrap_or_else(|| panic!("{id} listed"))
}
