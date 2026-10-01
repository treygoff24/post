//! The peer directory: `post who --live`, addressing by profile name or
//! `repo:`, unique live names, and the extra `participant describe` fields.
//! Participants are made live the way production does it: a fresh watch
//! heartbeat in their record directory plus a recent activity stamp.

mod common;

use common::{
    assert_success, from_stderr, from_stdout, join_channel, register_alpha_beta, stderr, stdout,
    Sandbox,
};
use post::output::ErrorEnvelope;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{SystemTime, UNIX_EPOCH};

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_secs()
}

/// RFC3339 UTC for `secs_ago` seconds before now.
fn rfc3339_ago(secs_ago: u64) -> String {
    let secs = unix_now() - secs_ago;
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn record_path(sandbox: &Sandbox, id: &str) -> PathBuf {
    sandbox
        .mail_root
        .join("participants")
        .join(id)
        .join("participant.json")
}

/// A participant bound in `alpha`.
fn peer(sandbox: &Sandbox, key: &str, alpha: &Path) -> String {
    sandbox.bind_claude(key, alpha, Some("alpha"))["participant"]["id"]
        .as_str()
        .expect("participant id")
        .to_owned()
}

/// A fresh `post watch` heartbeat, as a running watch writes it.
fn watch(sandbox: &Sandbox, id: &str) {
    fs::write(
        sandbox
            .mail_root
            .join("participants")
            .join(id)
            .join("watch.heartbeat"),
        format!("{} 60000\n", unix_now()),
    )
    .expect("heartbeat");
}

/// Rewrite the activity stamp, and `runtime.updated` with it when present.
fn stamp(sandbox: &Sandbox, id: &str, secs_ago: u64) {
    let path = record_path(sandbox, id);
    let mut record = sandbox.read_participant(id);
    let when = rfc3339_ago(secs_ago);
    record["last_seen"] = json!(when);
    if record.get("runtime").is_some() {
        record["runtime"]["updated"] = json!(when);
    }
    fs::write(path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
}

fn run(sandbox: &Sandbox, id: &str, cwd: &Path, args: &[&str]) -> Output {
    sandbox.run_as_participant(args, id, cwd)
}

fn ok(sandbox: &Sandbox, id: &str, cwd: &Path, args: &[&str]) -> Value {
    let output = run(sandbox, id, cwd, args);
    assert_success(&output);
    from_stdout(&output)
}

fn describe(sandbox: &Sandbox, id: &str, cwd: &Path, args: &[&str]) -> Output {
    let mut full = vec!["participant", "describe"];
    full.extend_from_slice(args);
    run(sandbox, id, cwd, &full)
}

fn name(sandbox: &Sandbox, id: &str, cwd: &Path, name: &str, pfp: &str) {
    let output = run(
        sandbox,
        id,
        cwd,
        &["profile", "set", "--name", name, "--pfp", pfp],
    );
    assert_success(&output);
}

fn error_of(output: &Output) -> ErrorEnvelope {
    assert!(!output.status.success(), "expected a refusal");
    from_stderr(output)
}

fn who_live(sandbox: &Sandbox, id: &str, cwd: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["who", "--live"];
    args.extend_from_slice(extra);
    run(sandbox, id, cwd, &args)
}

fn live_ids(who: &Value) -> Vec<String> {
    who["participants"]
        .as_array()
        .expect("participants")
        .iter()
        .map(|entry| entry["id"].as_str().expect("id").to_owned())
        .collect()
}

fn inbox_count(sandbox: &Sandbox, id: &str, cwd: &Path) -> u64 {
    ok(sandbox, id, cwd, &["inbox", "--json"])["count"]
        .as_u64()
        .unwrap_or(0)
}

/// An observer plus three live peers in different repos.
struct Fixture {
    sandbox: Sandbox,
    alpha: PathBuf,
    observer: String,
    fern: String,
    treadle: String,
    quill: String,
}

fn fixture() -> Fixture {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let observer = peer(&sandbox, "dir-observer", &alpha);
    let fern = peer(&sandbox, "dir-fern", &alpha);
    let treadle = peer(&sandbox, "dir-treadle", &alpha);
    let quill = peer(&sandbox, "dir-quill", &alpha);
    for (id, name_, pfp, repo, branch, title, role, state) in [
        (
            &fern,
            "Fern",
            "🌿",
            "/work/porch",
            "main",
            "Porch roster",
            "interactive",
            "working",
        ),
        (
            &treadle,
            "Treadle",
            "🧵",
            "/work/loom",
            "feat/peers",
            "Peer lane",
            "child",
            "idle",
        ),
        (
            &quill,
            "Quill",
            "🪶",
            "/other/post",
            "dev",
            "Docs",
            "headless",
            "idle",
        ),
    ] {
        name(&sandbox, id, &alpha, name_, pfp);
        assert_success(&describe(
            &sandbox,
            id,
            &alpha,
            &[
                "--repo", repo, "--branch", branch, "--title", title, "--role", role, "--state",
                state,
            ],
        ));
        watch(&sandbox, id);
    }
    watch(&sandbox, &observer);
    Fixture {
        sandbox,
        alpha,
        observer,
        fern,
        treadle,
        quill,
    }
}

#[test]
fn describe_stores_the_peer_fields_and_removes_one_at_a_time() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let id = peer(&sandbox, "describe-peer", &alpha);
    let parent = peer(&sandbox, "describe-parent", &alpha);

    let output = describe(
        &sandbox,
        &id,
        &alpha,
        &[
            "--model",
            "opus",
            "--repo",
            "/work/porch",
            "--branch",
            "feat/x",
            "--title",
            "A headline 🦊",
            "--role",
            "child",
            "--parent",
            &parent,
            "--state",
            "working",
            "--pane",
            "w-3:2",
            "--harness-session",
            "sess-123",
            "--json",
        ],
    );
    assert_success(&output);
    let runtime = &from_stdout::<Value>(&output)["participant"]["runtime"];
    for (field, value) in [
        ("model", "opus"),
        ("repo", "/work/porch"),
        ("branch", "feat/x"),
        ("title", "A headline 🦊"),
        ("role", "child"),
        ("parent", parent.as_str()),
        ("state", "working"),
        ("pane", "w-3:2"),
        ("harness_session", "sess-123"),
    ] {
        assert_eq!(runtime[field], value, "{field}: {runtime}");
    }
    assert_eq!(sandbox.read_participant(&id)["runtime"], *runtime);

    // Last write wins per field; the others are untouched.
    let again = describe(&sandbox, &id, &alpha, &["--state", "idle", "--json"]);
    let runtime = &from_stdout::<Value>(&again)["participant"]["runtime"];
    assert_eq!(runtime["state"], "idle");
    assert_eq!(runtime["title"], "A headline 🦊");

    // --unset is repeatable, takes the JSON names, and accepts the kebab alias.
    let unset = describe(
        &sandbox,
        &id,
        &alpha,
        &[
            "--unset",
            "title",
            "--unset",
            "harness_session",
            "--unset",
            "pane",
            "--json",
        ],
    );
    assert_success(&unset);
    let runtime = from_stdout::<Value>(&unset)["participant"]["runtime"].clone();
    for gone in ["title", "harness_session", "pane"] {
        assert!(runtime.get(gone).is_none(), "{gone} removed: {runtime}");
    }
    assert_eq!(runtime["repo"], "/work/porch");
    let alias = describe(
        &sandbox,
        &id,
        &alpha,
        &["--unset", "harness-session", "--json"],
    );
    assert_success(&alias);

    // Setting one field while removing another in the same call works.
    let both = describe(
        &sandbox,
        &id,
        &alpha,
        &["--branch", "main", "--unset", "parent", "--json"],
    );
    let runtime = &from_stdout::<Value>(&both)["participant"]["runtime"];
    assert_eq!(runtime["branch"], "main");
    assert!(runtime.get("parent").is_none());

    // Removing the last declared field leaves no runtime member at all.
    let rest: Vec<&str> = ["model", "repo", "branch", "role", "state"]
        .iter()
        .flat_map(|field| ["--unset", field])
        .chain(["--json"])
        .collect();
    let emptied = describe(&sandbox, &id, &alpha, &rest);
    assert_success(&emptied);
    assert!(from_stdout::<Value>(&emptied)["participant"]
        .get("runtime")
        .is_none());
    assert!(sandbox.read_participant(&id).get("runtime").is_none());

    // Text form names what is set.
    let text = describe(&sandbox, &id, &alpha, &["--repo", "/r", "--state", "idle"]);
    let text = stdout(&text);
    assert!(
        text.contains("repo=/r") && text.contains("state=idle"),
        "{text}"
    );
}

#[test]
fn describe_refuses_bad_peer_input_and_writes_nothing() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let id = peer(&sandbox, "describe-bad", &alpha);
    assert_success(&describe(&sandbox, &id, &alpha, &["--branch", "kept"]));
    let path = record_path(&sandbox, &id);
    let before = fs::read(&path).unwrap();
    let long = |n: usize| "x".repeat(n);
    let cases: Vec<(&str, Vec<String>)> = vec![
        ("relative repo", vec!["--repo".into(), "work/porch".into()]),
        ("empty branch", vec!["--branch".into(), String::new()]),
        ("branch over 255", vec!["--branch".into(), long(256)]),
        ("title over 80", vec!["--title".into(), long(81)]),
        (
            "title with a bidi override",
            vec!["--title".into(), "a\u{202e}b".into()],
        ),
        (
            "title with a newline",
            vec!["--title".into(), "a\nb".into()],
        ),
        ("unknown role", vec!["--role".into(), "daemon".into()]),
        ("unknown state", vec!["--state".into(), "busy".into()]),
        ("pane over 64", vec!["--pane".into(), long(65)]),
        (
            "harness session over 128",
            vec!["--harness-session".into(), long(129)],
        ),
        (
            "parent that does not exist",
            vec!["--parent".into(), "test-nobody".into()],
        ),
        (
            "unknown --unset field",
            vec!["--unset".into(), "colour".into()],
        ),
        (
            "set and unset the same field",
            vec![
                "--title".into(),
                "t".into(),
                "--unset".into(),
                "title".into(),
            ],
        ),
        (
            "clear with unset",
            vec!["--clear".into(), "--unset".into(), "title".into()],
        ),
        (
            "ended with a field",
            vec!["--ended".into(), "--title".into(), "t".into()],
        ),
        ("ended with clear", vec!["--ended".into(), "--clear".into()]),
    ];
    for (label, args) in cases {
        let mut full: Vec<&str> = args.iter().map(String::as_str).collect();
        // A valid field rides along: a refused call must not store it either.
        full.extend(["--model", "must-not-land"]);
        let error = error_of(&describe(&sandbox, &id, &alpha, &full));
        assert_eq!(error.error.code.as_str(), "invalid_argument", "{label}");
        assert_eq!(fs::read(&path).unwrap(), before, "{label} wrote something");
    }
}

#[test]
fn who_live_lists_live_participants_only_with_the_ten_minute_window() {
    let fx = fixture();
    let (s, alpha) = (&fx.sandbox, &fx.alpha);
    // Quill stops watching (no live heartbeat): not live however recent.
    fs::write(
        s.mail_root
            .join("participants")
            .join(&fx.quill)
            .join("watch.heartbeat"),
        format!("{} 60000\n", unix_now() - 3600),
    )
    .unwrap();
    // A watching participant whose last activity is 11 minutes old is out;
    // one at 9 minutes is in.
    let quiet = peer(s, "dir-quiet", alpha);
    let recent = peer(s, "dir-recent", alpha);
    for id in [&quiet, &recent] {
        assert_success(&describe(s, id, alpha, &["--title", "t"]));
        watch(s, id);
    }
    stamp(s, &quiet, 11 * 60);
    stamp(s, &recent, 9 * 60);
    // An ended participant is not live even with a fresh heartbeat.
    let gone = peer(s, "dir-gone", alpha);
    watch(s, &gone);
    assert_success(&run(s, &gone, alpha, &["participant", "end"]));

    let json = ok(s, &fx.observer, alpha, &["who", "--live", "--json"]);
    let mut ids = live_ids(&json);
    ids.sort();
    let mut expected = vec![
        fx.observer.clone(),
        fx.fern.clone(),
        fx.treadle.clone(),
        recent.clone(),
    ];
    expected.sort();
    assert_eq!(ids, expected, "{json}");
    assert_eq!(json["count"], 4);
    assert!(json["participants"]
        .as_array()
        .unwrap()
        .iter()
        .all(|entry| entry["live_watch"] == true));

    // Without --live the same roster still lists everybody (name and pfp ride
    // along for those that have a profile).
    let all = ok(s, &fx.observer, alpha, &["who", "--json"]);
    let all_ids = live_ids(&all);
    for id in [&quiet, &gone, &fx.quill] {
        assert!(all_ids.contains(id), "plain who still lists {id}");
    }
    let fern = all["participants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == fx.fern.as_str())
        .unwrap();
    assert_eq!(fern["name"], "Fern");
    assert_eq!(fern["pfp"], "🌿");
    assert_eq!(fern["runtime"]["repo"], "/work/porch");
    let nameless = all["participants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == quiet.as_str())
        .unwrap();
    assert!(nameless.get("name").is_none() && nameless.get("pfp").is_none());
}

#[test]
fn who_live_text_is_one_line_per_participant() {
    let fx = fixture();
    let (s, alpha) = (&fx.sandbox, &fx.alpha);
    // The observer has no profile and no runtime: the id and the age remain.
    let output = who_live(s, &fx.observer, alpha, &[]);
    assert_success(&output);
    let text = stdout(&output);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 4, "{text}");
    let line_for = |needle: &str| {
        lines
            .iter()
            .find(|line| line.contains(needle))
            .copied()
            .unwrap_or_else(|| panic!("no line for {needle}: {text}"))
    };
    let fern = line_for("Fern");
    assert!(
        fern.starts_with("🌿 Fern · porch@main · Porch roster · working · "),
        "{fern}"
    );
    assert!(
        fern.ends_with('s') && fern.rsplit(" · ").next().unwrap().len() <= 3,
        "age is a short label: {fern}"
    );
    assert!(
        line_for("Treadle").starts_with("🧵 Treadle · loom@feat/peers · Peer lane · idle · "),
        "{text}"
    );
    let observer = line_for(&fx.observer);
    assert!(
        observer.starts_with(&format!("{} · ", fx.observer))
            && observer.matches(" · ").count() == 1,
        "an unknown name, location, title and state are omitted: {observer}"
    );
    assert!(
        !text.contains("participant:") && !text.contains("hint:"),
        "only peer lines: {text}"
    );
}

#[test]
fn who_live_filters_by_role_and_repo() {
    let fx = fixture();
    let (s, alpha) = (&fx.sandbox, &fx.alpha);
    watch(s, &fx.quill);
    let ids = |extra: &[&str]| {
        let mut args = vec!["--json"];
        args.extend_from_slice(extra);
        let output = who_live(s, &fx.observer, alpha, &args);
        assert_success(&output);
        let mut ids = live_ids(&from_stdout(&output));
        ids.sort();
        ids
    };
    assert_eq!(ids(&["--role", "child"]), vec![fx.treadle.clone()]);
    assert_eq!(ids(&["--role", "interactive"]), vec![fx.fern.clone()]);
    assert_eq!(ids(&["--role", "headless"]), vec![fx.quill.clone()]);
    assert_eq!(ids(&["--repo", "porch"]), vec![fx.fern.clone()]);
    assert_eq!(ids(&["--repo", "/work/loom"]), vec![fx.treadle.clone()]);
    assert_eq!(ids(&["--repo", "/other/post"]), vec![fx.quill.clone()]);
    assert_eq!(
        ids(&["--repo", "/work/loom/"]),
        vec![fx.treadle.clone()],
        "a trailing slash is ignored"
    );
    assert!(
        ids(&["--repo", "loom/"]).is_empty(),
        "a name is a basename, not a fragment"
    );
    assert!(ids(&["--repo", "nowhere"]).is_empty());
    assert_eq!(
        ids(&["--role", "child", "--repo", "porch"]),
        Vec::<String>::new(),
        "both filters must hold"
    );
    let bad = who_live(s, &fx.observer, alpha, &["--role", "daemon"]);
    assert_eq!(error_of(&bad).error.code.as_str(), "invalid_argument");
    // The filters belong to --live.
    let alone = run(s, &fx.observer, alpha, &["who", "--role", "child"]);
    assert!(!alone.status.success());
}

#[test]
fn send_reaches_a_live_participant_by_name_or_repo() {
    let fx = fixture();
    let (s, alpha) = (&fx.sandbox, &fx.alpha);
    for (to, id, via, name_) in [
        ("Treadle", &fx.treadle, "name", Some("Treadle")),
        ("tReAdLe", &fx.treadle, "name", Some("Treadle")),
        ("repo:porch", &fx.fern, "repo", Some("Fern")),
        ("repo:/work/porch", &fx.fern, "repo", Some("Fern")),
        (fx.fern.as_str(), &fx.fern, "id", Some("Fern")),
        (
            &format!("participant:{}", fx.fern),
            &fx.fern,
            "id",
            Some("Fern"),
        ),
    ] {
        let before = inbox_count(s, id, alpha);
        let receipt = ok(
            s,
            &fx.observer,
            alpha,
            &["send", "--to", to, "--json", "--body", "hello"],
        );
        assert_eq!(receipt["resolved"]["id"], id.as_str(), "{to}: {receipt}");
        assert_eq!(receipt["resolved"]["via"], via, "{to}");
        assert_eq!(receipt["resolved"]["name"].as_str(), name_, "{to}");
        assert_eq!(receipt["envelope"]["to"], id.as_str(), "{to}");
        assert_eq!(receipt["envelope"]["address_kind"], "participant", "{to}");
        assert_eq!(inbox_count(s, id, alpha), before + 1, "{to} arrived");
    }
    // A room is still a room and carries no `resolved`.
    let room = ok(
        s,
        &fx.observer,
        alpha,
        &["send", "--to", "beta", "--json", "--body", "to the room"],
    );
    assert!(room.get("resolved").is_none(), "{room}");
    assert_eq!(room["envelope"]["address_kind"], "workspace");
}

#[test]
fn an_ambiguous_name_or_repo_lists_candidates_and_sends_nothing() {
    let fx = fixture();
    let (s, alpha) = (&fx.sandbox, &fx.alpha);
    // A second participant in the porch repo (names are unique, repos are not).
    let twin = peer(s, "dir-twin", alpha);
    assert_success(&describe(
        s,
        &twin,
        alpha,
        &[
            "--repo",
            "/elsewhere/porch",
            "--title",
            "Twin",
            "--state",
            "working",
        ],
    ));
    watch(s, &twin);
    let before: Vec<u64> = [&fx.fern, &twin, &fx.treadle, &fx.quill]
        .iter()
        .map(|id| inbox_count(s, id, alpha))
        .collect();

    let output = run(
        s,
        &fx.observer,
        alpha,
        &["send", "--to", "repo:porch", "--json", "--body", "x"],
    );
    let error = error_of(&output);
    assert_eq!(error.error.code.as_str(), "ambiguous_recipient");
    assert_eq!(output.status.code(), Some(65));
    let envelope: Value = from_stderr(&output);
    let candidates = envelope["error"]["details"]["candidates"]
        .as_array()
        .expect("candidates");
    assert_eq!(candidates.len(), 2, "{envelope}");
    let fern = candidates
        .iter()
        .find(|c| c["id"] == fx.fern.as_str())
        .expect("fern listed");
    assert_eq!(fern["name"], "Fern");
    assert_eq!(fern["repo"], "/work/porch");
    assert_eq!(fern["title"], "Porch roster");
    assert_eq!(fern["state"], "working");
    let twin_entry = candidates
        .iter()
        .find(|c| c["id"] == twin.as_str())
        .expect("twin listed");
    assert!(twin_entry.get("name").is_none(), "no profile, no name key");
    let message = envelope["error"]["message"].as_str().unwrap();
    assert!(
        message.contains(&fx.fern) && message.contains(&twin) && message.contains("Porch roster"),
        "{message}"
    );

    // An absolute path narrows it; so does the exact id.
    let narrowed = ok(
        s,
        &fx.observer,
        alpha,
        &[
            "send",
            "--to",
            "repo:/elsewhere/porch",
            "--json",
            "--body",
            "x",
        ],
    );
    assert_eq!(narrowed["resolved"]["id"], twin.as_str());
    let after: Vec<u64> = [&fx.fern, &twin, &fx.treadle, &fx.quill]
        .iter()
        .map(|id| inbox_count(s, id, alpha))
        .collect();
    assert_eq!(
        after,
        vec![before[0], before[1] + 1, before[2], before[3]],
        "only the narrowed send landed"
    );
}

#[test]
fn unknown_and_not_live_recipients_are_refused_before_anything_is_sent() {
    let fx = fixture();
    let (s, alpha) = (&fx.sandbox, &fx.alpha);
    // Quill goes quiet: its heartbeat dies.
    fs::remove_file(
        s.mail_root
            .join("participants")
            .join(&fx.quill)
            .join("watch.heartbeat"),
    )
    .unwrap();
    let before = inbox_count(s, &fx.quill, alpha);
    for to in ["Quill", "repo:post", "repo:no-such-repo"] {
        let output = run(
            s,
            &fx.observer,
            alpha,
            &["send", "--to", to, "--json", "--body", "x"],
        );
        let error = error_of(&output);
        assert_eq!(error.error.code.as_str(), "unknown_recipient", "{to}");
        assert_eq!(output.status.code(), Some(65), "{to}");
        assert!(stderr(&output).contains("post who --live"), "{to}");
    }
    assert_eq!(inbox_count(s, &fx.quill, alpha), before, "nothing was sent");

    // A string that is no one's name keeps today's error.
    let output = run(
        s,
        &fx.observer,
        alpha,
        &[
            "send",
            "--to",
            "nobody-by-that-name",
            "--json",
            "--body",
            "x",
        ],
    );
    assert_eq!(error_of(&output).error.code.as_str(), "unknown_room");
}

#[test]
fn an_exact_id_and_every_existing_form_win_over_a_name() {
    let fx = fixture();
    let (s, alpha) = (&fx.sandbox, &fx.alpha);
    // Quill takes the *other* participant's id as its profile name. An exact
    // id must still mean the participant with that id.
    let victim = peer(s, "dir-victim", alpha);
    let clash = fx.treadle.clone();
    // Release Treadle's name, give Quill a name that spells Treadle's id.
    assert_success(&run(s, &fx.quill, alpha, &["profile", "clear"]));
    name(s, &fx.quill, alpha, &clash, "🪵");
    let before_treadle = inbox_count(s, &fx.treadle, alpha);
    let before_quill = inbox_count(s, &fx.quill, alpha);
    let receipt = ok(
        s,
        &fx.observer,
        alpha,
        &["send", "--to", &clash, "--json", "--body", "id wins"],
    );
    assert_eq!(receipt["resolved"]["id"], fx.treadle.as_str());
    assert_eq!(receipt["resolved"]["via"], "id");
    assert_eq!(inbox_count(s, &fx.treadle, alpha), before_treadle + 1);
    assert_eq!(inbox_count(s, &fx.quill, alpha), before_quill);

    // An id works whether or not the participant is live.
    assert!(!victim.is_empty());
    let dormant = ok(
        s,
        &fx.observer,
        alpha,
        &["send", "--to", &victim, "--json", "--body", "not live"],
    );
    assert_eq!(dormant["resolved"]["via"], "id");

    // A room named like a participant's name keeps meaning the room.
    let room = ok(
        s,
        &fx.observer,
        alpha,
        &[
            "send",
            "--to",
            "alpha",
            "--allow-self",
            "--json",
            "--body",
            "r",
        ],
    );
    assert_eq!(
        room["envelope"]["address_kind"], "participant",
        "retargeted to self by --allow-self"
    );
    assert_eq!(room["retargeted"]["from"], "workspace:alpha");
    assert!(
        room.get("resolved").is_none(),
        "a room is not a participant: {room}"
    );
}

#[test]
fn profile_set_name_refuses_a_name_a_live_participant_holds() {
    let fx = fixture();
    let (s, alpha) = (&fx.sandbox, &fx.alpha);
    let output = run(
        s,
        &fx.observer,
        alpha,
        &["profile", "set", "--name", "tREADLE"],
    );
    let error = error_of(&output);
    assert_eq!(error.error.code.as_str(), "invalid_argument");
    assert!(
        error
            .error
            .message
            .contains(&format!("participant:{}", fx.treadle)),
        "names the holder: {}",
        error.error.message
    );
    assert_eq!(output.status.code(), Some(2));
    let profiles: Value =
        serde_json::from_slice(&fs::read(s.mail_root.join("profiles.json")).unwrap()).unwrap();
    assert!(
        profiles
            .get(format!("participant:{}", fx.observer))
            .is_none(),
        "nothing was written: {profiles}"
    );

    // The holder may keep or restyle its own name.
    assert_success(&run(
        s,
        &fx.treadle,
        alpha,
        &["profile", "set", "--name", "TREADLE"],
    ));
    // Once the holder is no longer live the name is free.
    stamp(s, &fx.treadle, 11 * 60);
    assert_success(&run(
        s,
        &fx.observer,
        alpha,
        &["profile", "set", "--name", "Treadle", "--pfp", "🪡"],
    ));
    // Pfp-only sets never trip the name rule.
    assert_success(&run(s, &fx.fern, alpha, &["profile", "set", "--pfp", "🍃"]));
}

#[test]
fn ending_drops_the_runtime_and_describe_ended_does_the_same() {
    let sandbox = Sandbox::new();
    let (alpha, _beta) = register_alpha_beta(&sandbox);
    let ending = peer(&sandbox, "end-runtime", &alpha);
    let via_describe = peer(&sandbox, "end-describe", &alpha);
    for id in [&ending, &via_describe] {
        assert_success(&describe(
            &sandbox,
            id,
            &alpha,
            &["--model", "opus", "--repo", "/work/r", "--state", "working"],
        ));
        assert!(sandbox.read_participant(id).get("runtime").is_some());
    }

    let end = run(&sandbox, &ending, &alpha, &["participant", "end", "--json"]);
    assert_success(&end);
    let record = sandbox.read_participant(&ending);
    assert!(record.get("runtime").is_none(), "{record}");
    assert!(record["ended_at"].is_string());

    let output = describe(&sandbox, &via_describe, &alpha, &["--ended", "--json"]);
    assert_success(&output);
    let answer: Value = from_stdout(&output);
    assert_eq!(answer["ok"], true);
    assert_eq!(answer["id"], via_describe.as_str());
    assert!(answer["participant"]["ended_at"].is_string());
    assert!(answer["participant"].get("runtime").is_none());
    let record = sandbox.read_participant(&via_describe);
    assert!(record.get("runtime").is_none() && record["ended_at"].is_string());
    let ended_at = record["ended_at"].clone();

    // Repeating it is fine and does not move ended_at.
    assert_success(&describe(&sandbox, &via_describe, &alpha, &["--ended"]));
    assert_eq!(
        sandbox.read_participant(&via_describe)["ended_at"],
        ended_at
    );

    // The text form says so.
    let other = peer(&sandbox, "end-text", &alpha);
    let text = stdout(&describe(&sandbox, &other, &alpha, &["--ended"]));
    assert!(
        text.contains("ended") && text.contains("runtime cleared"),
        "{text}"
    );
}

#[test]
fn an_ended_participant_is_not_addressable_by_name() {
    let fx = fixture();
    let (s, alpha) = (&fx.sandbox, &fx.alpha);
    assert_success(&describe(s, &fx.treadle, alpha, &["--ended"]));
    // Its heartbeat is still fresh, yet it is gone from the directory.
    let json = ok(s, &fx.observer, alpha, &["who", "--live", "--json"]);
    assert!(!live_ids(&json).contains(&fx.treadle));
    let output = run(
        s,
        &fx.observer,
        alpha,
        &["send", "--to", "Treadle", "--json", "--body", "x"],
    );
    assert_eq!(error_of(&output).error.code.as_str(), "unknown_recipient");
    // And the name is free again for a newcomer.
    assert_success(&run(
        s,
        &fx.observer,
        alpha,
        &["profile", "set", "--name", "Treadle", "--pfp", "🪡"],
    ));
}

/// The channel the emote tests post to, with `alpha` and `beta` as members
/// (the same shape the porch contract tests use).
fn emote_channel(fx: &Fixture) -> (String, PathBuf) {
    let s = &fx.sandbox;
    let beta = s.path.join("beta");
    join_channel(s, "ops", &fx.alpha);
    join_channel(s, "ops", &beta);
    // A sender with an avatar, a member to aim at, and a non-member.
    let corpus =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("contract/porch/avatars/valid/blob.json");
    let sender = peer(s, "dir-emoter", &fx.alpha);
    name(s, &sender, &fx.alpha, "Emoter", "🎭");
    assert_success(&run(
        s,
        &sender,
        &fx.alpha,
        &[
            "profile",
            "avatar",
            "set",
            "--file",
            corpus.to_str().unwrap(),
        ],
    ));
    (sender, beta)
}

#[test]
fn emote_at_resolves_a_live_member_by_name_or_repo() {
    let fx = fixture();
    let s = &fx.sandbox;
    if !Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("contract/porch/avatars/valid/blob.json")
        .is_file()
    {
        // The frozen corpus lives beside the porch contract tests.
        return;
    }
    let (sender, _beta) = emote_channel(&fx);
    watch(s, &sender);
    assert_success(&run(s, &sender, &fx.alpha, &["chat", "ops", "--join"]));
    for id in [&fx.fern, &fx.treadle] {
        assert_success(&run(s, id, &fx.alpha, &["chat", "ops", "--join"]));
    }
    let emote = |at: &str| {
        run(
            s,
            &sender,
            &fx.alpha,
            &["chat", "ops", "--emote", "wave", "--at", at, "--json"],
        )
    };
    let target_of = |output: &Output| -> String {
        assert_success(output);
        let id = from_stdout::<Value>(output)["message"]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let raw = fs::read_to_string(
            s.mail_root
                .join(format!("channels/ops/messages/{id}.emote")),
        )
        .unwrap();
        let (header, _) = raw.split_once("\n---\n").unwrap();
        serde_json::from_str::<Value>(header).unwrap()["emote"]["at"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    // The existing forms are unchanged: an id, and an exact display name.
    assert_eq!(target_of(&emote(&fx.fern)), fx.fern);
    assert_eq!(target_of(&emote("Fern")), fx.fern);
    // New: any case, and repo:.
    assert_eq!(target_of(&emote("tREADLE")), fx.treadle);
    assert_eq!(target_of(&emote("repo:loom")), fx.treadle);
    assert_eq!(target_of(&emote("repo:/work/porch")), fx.fern);
    // Not a member of the channel: nothing resolves it.
    let missing = emote("repo:post");
    assert_eq!(error_of(&missing).error.code.as_str(), "unknown_recipient");
}

#[test]
fn a_name_stops_resolving_once_its_holder_is_quiet_for_over_ten_minutes() {
    let fx = fixture();
    let (s, alpha) = (&fx.sandbox, &fx.alpha);
    let drowsy = peer(s, "dir-drowsy", alpha);
    name(s, &drowsy, alpha, "Drowsy", "💤");
    watch(s, &drowsy);

    stamp(s, &drowsy, 11 * 60);
    let before = inbox_count(s, &drowsy, alpha);
    let output = run(
        s,
        &fx.observer,
        alpha,
        &["send", "--to", "drowsy", "--json", "--body", "x"],
    );
    assert_eq!(error_of(&output).error.code.as_str(), "unknown_recipient");
    assert_eq!(inbox_count(s, &drowsy, alpha), before, "nothing was sent");
    // Its own id still reaches it, live or not.
    let by_id = ok(
        s,
        &fx.observer,
        alpha,
        &["send", "--to", &drowsy, "--json", "--body", "x"],
    );
    assert_eq!(by_id["resolved"]["via"], "id");

    stamp(s, &drowsy, 9 * 60);
    let by_name = ok(
        s,
        &fx.observer,
        alpha,
        &["send", "--to", "drowsy", "--json", "--body", "x"],
    );
    assert_eq!(by_name["resolved"]["id"], drowsy.as_str());
    assert_eq!(by_name["resolved"]["via"], "name");
}
