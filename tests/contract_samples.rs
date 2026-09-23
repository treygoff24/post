//! D: post's JSON output as a tested contract.
//!
//! Every sample under `contract/samples/` is produced here by running the real
//! commands against a temporary store; none is written by hand. The volatile
//! parts (ids, timestamps, sandbox paths, digests, the build sha) are replaced
//! with fixed values of the same format, so a sample keeps each field's
//! presence, type, and enum value while staying byte-stable across runs and
//! hosts. Arrays of objects are ordered by their normalized content, because
//! ids minted in the same second sort by their random suffix.
//!
//! `POST_UPDATE_CONTRACT=1 cargo test --test contract_samples` rewrites the
//! samples. Without it, any difference between a producer and its checked-in
//! sample fails: a shape change must be a reviewed sample change.

mod common;

use common::{assert_success, register_alpha_beta, register_room, write_custom_mail, Sandbox};
use regex::Regex;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::OnceLock;

/// The stable format-preserving stand-ins.
const TIME_DATE: &str = "2026-01-01";
const TIME_CLOCK: &str = "00:00:00";
const BUILD_SHA_SENTINEL: &str = "unknown";

fn samples_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("contract/samples")
}

struct Normalizer {
    /// Sandbox path spellings, longest first, each replaced by `/SANDBOX`.
    paths: Vec<String>,
    ids: HashMap<String, String>,
    mail_ids: usize,
    channel_ids: usize,
}

/// Mail ids (`YYYYMMDD-HHMMSS-hex6`) and channel message ids
/// (`YYYYMMDD-HHMMSS-NNNNNN-hex6`) in one pass: a mail-id pattern alone also
/// matches the head of a channel id.
fn store_id() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b\d{8}-\d{6}(?P<seq>-\d{6})?-[0-9a-fA-F]{6}\b").unwrap())
}

fn timestamp() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"\b\d{4}-\d{2}-\d{2}(?P<sep>[ T])\d{2}:\d{2}:\d{2}(?P<frac>\.\d+)?(?P<zone>Z| ?[+-]\d{2}:?\d{2})?",
        )
        .unwrap()
    })
}

fn digest() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b[0-9a-f]{64}\b").unwrap())
}

impl Normalizer {
    fn new(sandbox: &Sandbox) -> Self {
        let mut paths = vec![sandbox.path.to_string_lossy().into_owned()];
        if let Ok(canonical) = fs::canonicalize(&sandbox.path) {
            paths.push(canonical.to_string_lossy().into_owned());
        }
        paths.sort_by_key(|path| std::cmp::Reverse(path.len()));
        paths.dedup();
        Self {
            paths,
            ids: HashMap::new(),
            mail_ids: 0,
            channel_ids: 0,
        }
    }

    /// Everything but ids: paths, timestamps, digests.
    fn scrub(&self, text: &str) -> String {
        let mut text = text.to_owned();
        for path in &self.paths {
            text = text.replace(path.as_str(), "/SANDBOX");
        }
        let text = timestamp().replace_all(&text, |caps: &regex::Captures| {
            let frac = caps
                .name("frac")
                .map(|frac| ".".to_owned() + &"0".repeat(frac.as_str().len() - 1))
                .unwrap_or_default();
            let zone = match caps.name("zone").map(|zone| zone.as_str()) {
                None => String::new(),
                Some("Z") => "Z".to_owned(),
                Some(zone) => {
                    let lead = if zone.starts_with(' ') { " " } else { "" };
                    let colon = if zone.contains(':') { ":" } else { "" };
                    format!("{lead}+00{colon}00")
                }
            };
            format!("{TIME_DATE}{}{TIME_CLOCK}{frac}{zone}", &caps["sep"])
        });
        digest().replace_all(&text, "0".repeat(64)).into_owned()
    }

    fn mask_ids(text: &str) -> String {
        store_id().replace_all(text, "<id>").into_owned()
    }

    fn renumber(&mut self, text: &str) -> String {
        store_id()
            .replace_all(text, |caps: &regex::Captures| {
                let original = caps[0].to_owned();
                if let Some(known) = self.ids.get(&original) {
                    return known.clone();
                }
                let stand_in = if caps.name("seq").is_some() {
                    self.channel_ids += 1;
                    format!(
                        "20260101-000000-{:06}-c{:05x}",
                        self.channel_ids, self.channel_ids
                    )
                } else {
                    self.mail_ids += 1;
                    format!("20260101-{:06}-a{:05x}", self.mail_ids, self.mail_ids)
                };
                self.ids.insert(original, stand_in.clone());
                stand_in
            })
            .into_owned()
    }

    /// Scrub every string, pin the build sha, and order arrays of objects by
    /// their id-masked content. Ids are renumbered afterwards, in document
    /// order, so one original id keeps one stand-in within a sample.
    fn prepare(&self, value: Value) -> Value {
        match value {
            Value::String(text) => Value::String(self.scrub(&text)),
            Value::Array(items) => {
                let mut items: Vec<Value> =
                    items.into_iter().map(|item| self.prepare(item)).collect();
                if items.iter().all(Value::is_object) {
                    items.sort_by_cached_key(|item| Self::mask_ids(&item.to_string()));
                }
                Value::Array(items)
            }
            Value::Object(fields) => {
                let mut out = Map::new();
                for (key, field) in fields {
                    let field = if key == "build_sha" && field.is_string() {
                        Value::String(BUILD_SHA_SENTINEL.to_owned())
                    } else {
                        self.prepare(field)
                    };
                    out.insert(self.scrub(&key), field);
                }
                Value::Object(out)
            }
            other => other,
        }
    }

    fn renumber_value(&mut self, value: Value) -> Value {
        match value {
            Value::String(text) => Value::String(self.renumber(&text)),
            Value::Array(items) => Value::Array(
                items
                    .into_iter()
                    .map(|item| self.renumber_value(item))
                    .collect(),
            ),
            Value::Object(fields) => Value::Object(
                fields
                    .into_iter()
                    .map(|(key, field)| (self.renumber(&key), self.renumber_value(field)))
                    .collect(),
            ),
            other => other,
        }
    }

    /// Ids are numbered per sample, in position order: items that differ only
    /// by id then take their numbers from where they sit, never from the
    /// random suffix that ordered them in an earlier sample.
    fn restart_ids(&mut self) {
        self.ids.clear();
        self.mail_ids = 0;
        self.channel_ids = 0;
    }

    /// One JSON document.
    fn document(&mut self, output: &Output) -> String {
        self.restart_ids();
        let value: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "not one JSON document ({error}): {}",
                String::from_utf8_lossy(&output.stdout)
            )
        });
        let value = self.prepare(value);
        let value = self.renumber_value(value);
        format!(
            "{}\n",
            serde_json::to_string_pretty(&value).expect("render sample")
        )
    }

    /// JSON lines (watch): one event per line, ordered like an array.
    fn lines(&mut self, output: &Output) -> String {
        self.restart_ids();
        let text = String::from_utf8_lossy(&output.stdout);
        let events: Vec<Value> = text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                serde_json::from_str(line)
                    .unwrap_or_else(|error| panic!("not a JSON line ({error}): {line}"))
            })
            .collect();
        assert!(!events.is_empty(), "a watch sample needs events");
        let Value::Array(events) = self.prepare(Value::Array(events)) else {
            unreachable!("an array stays an array");
        };
        events
            .into_iter()
            .map(|event| format!("{}\n", self.renumber_value(event)))
            .collect()
    }
}

fn edit_participant(sandbox: &Sandbox, id: &str, edit: impl FnOnce(&mut Map<String, Value>)) {
    let path = sandbox
        .mail_root
        .join("participants")
        .join(id)
        .join("participant.json");
    let mut record: Value = serde_json::from_slice(&fs::read(&path).expect("participant record"))
        .expect("participant JSON");
    edit(record.as_object_mut().expect("participant object"));
    fs::write(
        &path,
        format!("{}\n", serde_json::to_string_pretty(&record).unwrap()),
    )
    .expect("write participant record");
}

fn participant_id(bound: &Value) -> String {
    bound["participant"]["id"]
        .as_str()
        .or_else(|| bound["id"].as_str())
        .expect("participant id")
        .to_owned()
}

/// A store holding one representative of every event variant a consumer
/// reads, and the command outputs taken from it.
fn produce() -> BTreeMap<&'static str, String> {
    let sandbox = Sandbox::new();
    let mut normalizer = Normalizer::new(&sandbox);
    let (alpha, beta) = register_alpha_beta(&sandbox);

    // The reader, continuing lineage `ember`, and a sender in beta.
    let reader_bind = sandbox.bind_claude("contract-reader", &alpha, Some("alpha"));
    let reader = participant_id(&reader_bind);
    let lineage_dir = sandbox.mail_root.join("lineages/ember");
    fs::create_dir_all(&lineage_dir).expect("lineage dir");
    fs::write(
        lineage_dir.join("lineage.json"),
        r#"{"name":"ember","founder":"founder-id","created":"2026-09-16 00:00:00 +0000","host":"test"}"#,
    )
    .expect("lineage record");
    edit_participant(&sandbox, &reader, |record| {
        record.insert("lineage".to_owned(), json!("ember"));
        record.insert("lineage_since".to_owned(), json!("2026-09-19T20:00:00Z"));
    });
    let sender = participant_id(&sandbox.bind_codex("contract-sender", &beta, Some("beta")));
    // Bound before any mail is sent, so the history is addressed to it too.
    let degraded = participant_id(&sandbox.bind_claude("contract-degraded", &alpha, Some("alpha")));
    let as_reader = |args: &[&str]| sandbox.run_as_participant(args, &reader, &alpha);
    let as_sender = |args: &[&str]| sandbox.run_as_participant(args, &sender, &beta);
    assert_success(&as_reader(&[
        "profile", "set", "--name", "Reader", "--pfp", "📮",
    ]));

    // Workspace, participant, and lineage mail. Participant mail without a
    // routing receipt is what a snapshot reports as pending.
    for to in ["alpha", &format!("participant:{reader}"), "lineage:ember"] {
        assert_success(&as_sender(&[
            "send",
            "--to",
            to,
            "--kind",
            "note",
            "--subject",
            "contract",
            "--body",
            "body",
            "--json",
        ]));
    }

    // Remote origin: a sender whose workspace is a remote placeholder.
    let remote = sandbox.mail_root.join("remote/peer-host/remote-room");
    fs::create_dir_all(&remote).expect("remote placeholder");
    register_room(&sandbox, "remote-room", &remote);
    let inbox = sandbox.mail_root.join("alpha/inbox");
    let remote_id = "20260101-000100-bbbbb1";
    write_custom_mail(
        &inbox,
        remote_id,
        &json!({"id":remote_id,"from":"remote-room","to":"alpha","kind":"note","subject":"remote","sent":"2026-09-16 04:01:01 -0500","address_kind":"workspace"}),
        "remote",
    );
    // Unreadable mail: a stored file that is not an envelope.
    fs::write(inbox.join("20260101-000200-bbbbb2.mail"), "not an envelope")
        .expect("unreadable mail");

    // A channel both belong to: one plain message and one mentioning alpha.
    for (participant, cwd) in [(&reader, &alpha), (&degraded, &alpha), (&sender, &beta)] {
        assert_success(&sandbox.run_as_participant(
            &["chat", "tax", "--join", "--json"],
            participant,
            cwd,
        ));
    }
    for body in ["plain channel note", "@alpha a mention"] {
        assert_success(&as_sender(&[
            "chat", "tax", "--send", "--anyway", "--body", body, "--json",
        ]));
    }
    // An unreadable channel message in a channel the reader belongs to.
    assert_success(&as_reader(&["chat", "broken", "--join", "--json"]));
    fs::write(
        sandbox
            .mail_root
            .join("channels/broken/messages/20260101-000300-000001-bbbbb3.msg"),
        "not a message",
    )
    .expect("unreadable channel message");

    let mut samples = BTreeMap::new();
    let watch = as_reader(&["watch", "--snapshot"]);
    assert!(
        watch.status.success(),
        "{}",
        String::from_utf8_lossy(&watch.stderr)
    );
    samples.insert("watch-snapshot.jsonl", normalizer.lines(&watch));
    let digest = as_reader(&["watch", "--snapshot", "--digest"]);
    assert!(
        digest.status.success(),
        "{}",
        String::from_utf8_lossy(&digest.stderr)
    );
    samples.insert("watch-snapshot-digest.jsonl", normalizer.lines(&digest));

    // The listings refuse a store with a corrupt channel message outright
    // (`channels` fails `config_invalid`), so the planted corruption leaves
    // before the per-command documents are taken.
    fs::remove_file(
        sandbox
            .mail_root
            .join("channels/broken/messages/20260101-000300-000001-bbbbb3.msg"),
    )
    .expect("remove the unreadable channel message");
    fs::remove_file(inbox.join("20260101-000200-bbbbb2.mail")).expect("remove the unreadable mail");

    // One JSON document per command a consumer reads.
    let documents: [(&str, Output); 11] = [
        ("inbox.json", as_reader(&["inbox"])),
        ("chat.json", as_reader(&["chat", "tax", "--peek", "--json"])),
        ("who.json", as_reader(&["who"])),
        ("profile-show.json", as_reader(&["profile", "show"])),
        (
            "profile-list.json",
            as_reader(&["profile", "list", "--json"]),
        ),
        ("channels.json", as_reader(&["channels"])),
        (
            "participant-show.json",
            as_reader(&["participant", "show", "--json"]),
        ),
        (
            "participant-bind.json",
            as_reader(&["participant", "bind", "--json"]),
        ),
        ("rooms.json", as_reader(&["rooms", "--json"])),
        ("version.json", as_reader(&["version", "--json"])),
        ("doctor.json", as_reader(&["doctor"])),
    ];
    for (name, output) in documents {
        assert!(
            output.status.success() || name == "doctor.json",
            "{name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        samples.insert(name, normalizer.document(&output));
    }

    // Cursor re-report: a participant whose cursor state is unusable sees its
    // history as unread, and each event says so.
    let cursor = sandbox
        .mail_root
        .join("participants")
        .join(&degraded)
        .join("cursors.json");
    fs::write(&cursor, "{ not json").expect("corrupt cursor");
    let reread = sandbox.run_as_participant(&["watch", "--snapshot"], &degraded, &alpha);
    assert!(
        reread.status.success(),
        "{}",
        String::from_utf8_lossy(&reread.stderr)
    );
    samples.insert(
        "watch-snapshot-cursor-unusable.jsonl",
        normalizer.lines(&reread),
    );

    // `post bridge deliver` (post.bridge-deliver.v1): one sample per
    // outcome. The bridge codes against these, so each comes from a real
    // decision: a delivery to the reader from the remote placeholder, a
    // rejection for an absent participant, and a retry on a digest mismatch.
    let bridge = sandbox.mail_root.join("bridge");
    fs::create_dir_all(&bridge).expect("bridge dir");
    fs::write(
        bridge.join("config.json"),
        r#"{"host":"test-host","relay_url":"ssh://relay.invalid/relay.git"}"#,
    )
    .expect("bridge config");
    let relay = sandbox.path.join("relay");
    let letter = |to: &str, id: &str| -> (PathBuf, String) {
        let envelope = json!({
            "id": id, "from": "remote-room", "to": to, "kind": "letter",
            "subject": "dm", "sent": "2026-09-23 01:00:00 -0500",
            "from_participant": "codex-peer0001", "address_kind": "participant",
            "to_host": "test-host", "sender_provenance": "participant-binding"
        });
        write_custom_mail(&relay, id, &envelope, "hello across hosts\n");
        let path = relay.join(format!("{id}.mail"));
        let digest = sha256_hex(&fs::read(&path).expect("relay letter"));
        (path, digest)
    };
    let deliver = |participant: &str, id: &str, sha: &str, file: &Path| {
        let file = file.to_string_lossy().into_owned();
        sandbox.run(&[
            "bridge",
            "deliver",
            "--participant",
            participant,
            "--source-host",
            "peer-host",
            "--mail-id",
            id,
            "--sha256",
            sha,
            "--file",
            &file,
            "--json",
        ])
    };
    let (delivered_file, delivered_sha) = letter(&reader, "20260101-000400-ccccc1");
    let (ghost_file, ghost_sha) = letter("ghost-00000000", "20260101-000500-ccccc2");
    let (retry_file, _) = letter(&reader, "20260101-000600-ccccc3");
    let bridge_documents = [
        (
            "bridge-deliver-delivered.json",
            deliver(
                &reader,
                "20260101-000400-ccccc1",
                &delivered_sha,
                &delivered_file,
            ),
        ),
        (
            "bridge-deliver-rejected.json",
            deliver(
                "ghost-00000000",
                "20260101-000500-ccccc2",
                &ghost_sha,
                &ghost_file,
            ),
        ),
        (
            "bridge-deliver-retry.json",
            deliver(
                &reader,
                "20260101-000600-ccccc3",
                &"0".repeat(64),
                &retry_file,
            ),
        ),
    ];
    for (name, output) in bridge_documents {
        assert!(
            output.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        samples.insert(name, normalizer.document(&output));
    }
    samples
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(bytes))
}

#[test]
fn contract_samples_match_the_real_producers() {
    let produced = produce();
    let dir = samples_dir();
    if std::env::var_os("POST_UPDATE_CONTRACT").is_some_and(|value| value == "1") {
        fs::create_dir_all(&dir).expect("samples dir");
        for entry in fs::read_dir(&dir).expect("samples dir") {
            let path = entry.expect("sample entry").path();
            if path.is_file() && !produced.contains_key(path.file_name().unwrap().to_str().unwrap())
            {
                fs::remove_file(&path).expect("drop a sample no producer writes");
            }
        }
        for (name, body) in &produced {
            fs::write(dir.join(name), body).expect("write sample");
        }
        return;
    }
    let mut drift = Vec::new();
    for (name, body) in &produced {
        match fs::read_to_string(dir.join(name)) {
            Ok(existing) if &existing == body => {}
            Ok(existing) => drift.push(format!(
                "--- {name} (checked in)\n{existing}\n+++ {name} (produced)\n{body}"
            )),
            Err(error) => drift.push(format!("{name}: missing ({error})")),
        }
    }
    let on_disk: Vec<String> = fs::read_dir(&dir)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
                .filter(|name| !produced.contains_key(name.as_str()))
                .collect()
        })
        .unwrap_or_default();
    for name in on_disk {
        drift.push(format!("{name}: checked in but no producer writes it"));
    }
    assert!(
        drift.is_empty(),
        "contract samples drifted from the producers; review, then regenerate with POST_UPDATE_CONTRACT=1:\n{}",
        drift.join("\n")
    );
}

/// `post contract samples --dir` must hand consumers exactly the checked-in
/// samples: the embedded list in src/commands/contract.rs and the directory
/// agree in both directions, byte for byte.
#[test]
fn embedded_samples_are_the_checked_in_samples() {
    let sandbox = Sandbox::new();
    let out = sandbox.path.join("emitted");
    let output = sandbox.run(&[
        "contract",
        "samples",
        "--dir",
        out.to_str().expect("utf-8 path"),
    ]);
    assert_success(&output);
    let listed: Value = serde_json::from_slice(&output.stdout).expect("contract samples JSON");
    let read = |dir: &Path| -> BTreeMap<String, Vec<u8>> {
        fs::read_dir(dir)
            .expect("samples dir")
            .map(|entry| {
                let path = entry.expect("sample").path();
                (
                    path.file_name().unwrap().to_string_lossy().into_owned(),
                    fs::read(&path).expect("sample bytes"),
                )
            })
            .collect()
    };
    let emitted = read(&out);
    let checked_in = read(&samples_dir());
    assert_eq!(
        emitted.keys().collect::<Vec<_>>(),
        checked_in.keys().collect::<Vec<_>>(),
        "the embedded sample list and contract/samples/ disagree"
    );
    assert_eq!(
        emitted, checked_in,
        "an embedded sample differs from its file"
    );
    let names: Vec<&str> = listed["samples"]
        .as_array()
        .expect("samples list")
        .iter()
        .map(|name| name.as_str().expect("name"))
        .collect();
    assert_eq!(
        names,
        checked_in.keys().map(String::as_str).collect::<Vec<_>>()
    );

    // The printing form carries the same text.
    let printed = sandbox.run(&["contract", "samples"]);
    assert_success(&printed);
    let printed: Value = serde_json::from_slice(&printed.stdout).expect("printed samples");
    for (name, bytes) in &checked_in {
        assert_eq!(
            printed["samples"][name].as_str().map(str::as_bytes),
            Some(bytes.as_slice()),
            "{name}"
        );
    }
}
