//! `post delivery <mail-id>`: where a host-qualified letter the acting
//! participant sent stands (design rev 3.1, "Delivery states and
//! `post delivery`"). Read-only.
//!
//! Evidence lives under `bridge/` on the sending host and is written by the
//! bridge. The design names only `pmail-status`; the other three paths and
//! shapes are defined here, post-side, and documented in the schema:
//!
//! - `bridge/pmail-status/<id>.json`: `{v:1, id, blocked_reason?, last_error?, at?}`
//!   for a letter still queued (unknown keys ignored);
//! - `bridge/pmail-published/<id>.json`: `{v:1, id, host, sha256, commit, at}`,
//!   written only after the push succeeds (unknown keys ignored);
//! - `bridge/pmail-acked/<id>.json`: the destination's receipt, exactly
//!   `{v, status, origin, host, participant, id, sha256, reason, at}`;
//! - `bridge/pmail-conflicts/<id>.json`: present when a later receipt
//!   disagreed with the first; reported as `conflict: true`.
//!
//! A workspace letter to a room homed on another host is answered from
//! `bridge/room-acked/<id>.json` (the sending bridge's record of the
//! receiver's verdict), then the plain-text `bridge/published/<id>` marker,
//! then the room's current registration as a placeholder while the letter
//! still waits in its inbox (`queued`); see
//! `workspace_letter`.
//!
//! Precedence: a receipt (it may outrun the published marker), then the
//! marker, then queued. Any evidence file that exists but does not validate
//! makes the answer `unknown`, naming the file and the error: never a guess.

use crate::bridge_topology::{bridge_dir, load_config, read_regular};
use crate::cli::DeliveryArgs;
use crate::command_result::CommandResult;
use crate::error::{AppError, AppResult, ErrorCode};
use crate::imports::{hex_sha256, valid_mail_id, valid_sha256, ACKED_REJECTED_REASONS};
use crate::mailbox::Context;
use serde::Serialize;
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const SCHEMA: &str = "post.delivery.v1";
const ARCHIVE_MAX_BYTES: u64 = 64 * 1024 * 1024;
const EVIDENCE_MAX_BYTES: u64 = 16 * 1024;

#[derive(Debug, Serialize)]
struct DeliveryOutput {
    ok: bool,
    schema: &'static str,
    id: String,
    /// queued | published | received | rejected | unknown | unsupported
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    participant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    host: Option<String>,
    /// The room a workspace letter was sent to (workspace letters only).
    #[serde(skip_serializing_if = "Option::is_none")]
    room: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sha256: Option<String>,
    conflict: bool,
    /// The rejection reason (rejected) or why no state exists (unsupported).
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    blocked_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    published_at: Option<String>,
    /// Seconds since `published_at` (published only).
    #[serde(skip_serializing_if = "Option::is_none")]
    age_s: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    acked_at: Option<String>,
    /// The evidence file that failed validation (unknown only).
    #[serde(skip_serializing_if = "Option::is_none")]
    evidence_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    evidence_error: Option<String>,
}

impl DeliveryOutput {
    fn new(id: &str, state: &'static str) -> Self {
        Self {
            ok: true,
            schema: SCHEMA,
            id: id.to_owned(),
            state,
            participant: None,
            host: None,
            room: None,
            sha256: None,
            conflict: false,
            reason: None,
            blocked_reason: None,
            last_error: None,
            commit: None,
            published_at: None,
            age_s: None,
            acked_at: None,
            evidence_file: None,
            evidence_error: None,
        }
    }
}

/// An evidence file that exists but cannot be trusted.
struct Corrupt {
    file: PathBuf,
    error: String,
}

pub(super) fn run(
    context: &Context,
    args: DeliveryArgs,
    json_output: bool,
    pretty: bool,
) -> AppResult<CommandResult> {
    let actor = context.sender()?.participant;
    let id = args.id;
    if !valid_mail_id(&id) {
        return Err(AppError::invalid_argument(format!(
            "'{id}' is not a mail id (YYYYMMDD-HHMMSS-hex)"
        ))
        .input(id.clone())
        .reason("invalid mail id"));
    }
    let archive = context.root.join("archive").join(format!("{id}.mail"));
    let not_found = || {
        AppError::new(
            ErrorCode::NotFound,
            format!("no letter {id} sent by participant {} is in the archive", actor.id),
            "Check the id `post send` printed; only the letter's sender can see its delivery state.",
        )
        .id(id.clone())
    };
    let bytes = match read_regular(&archive, ARCHIVE_MAX_BYTES) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return Err(not_found()),
        Err(error) => {
            return Err(AppError::config(
                &archive,
                format!("archive letter is unreadable: {error}"),
            ))
        }
    };
    let mail = crate::mailbox::parse_mail_bytes(&archive, &bytes)?;
    let envelope = &mail.envelope;
    if envelope.id != id {
        return Err(AppError::config(
            &archive,
            format!("archive letter carries id {}, not {id}", envelope.id),
        ));
    }
    // Same visibility as reading your own sent mail: only the sender sees it.
    if !crate::output::mail_authored_locally_by(context, &actor.id, envelope) {
        return Err(not_found());
    }
    if envelope.address_kind.as_deref() == Some("workspace") {
        return workspace_letter(context, &id, &bytes, &envelope.to, json_output, pretty);
    }
    let (Some(host), Some("participant")) =
        (envelope.to_host.clone(), envelope.address_kind.as_deref())
    else {
        let mut output = DeliveryOutput::new(&id, "unsupported");
        output.reason = Some(
            "only participant:<id>@<host> letters have a delivery state; this letter was delivered locally"
                .to_owned(),
        );
        return render(&output, json_output, pretty);
    };
    let digest = hex_sha256(&bytes);
    let mut output = DeliveryOutput::new(&id, "queued");
    output.participant = Some(envelope.to.clone());
    output.host = Some(host.clone());
    output.sha256 = Some(digest.clone());
    let bridge = bridge_dir(context);
    let evidence = |dir: &str| bridge.join(dir).join(format!("{id}.json"));
    let decided = decide(
        context,
        &Letter {
            id: &id,
            participant: &envelope.to,
            host: &host,
            sha256: &digest,
        },
        &evidence,
        &mut output,
    );
    if let Err(corrupt) = decided {
        output.state = "unknown";
        output.reason = None;
        output.evidence_file = Some(corrupt.file.display().to_string());
        output.evidence_error = Some(corrupt.error);
    }
    render(&output, json_output, pretty)
}

/// A workspace letter: the bridge carries it only when the room is homed on
/// another host. Evidence is read regardless of the room's current
/// registration (a room may have been rehomed since); only with no evidence
/// does the registration decide between `queued` and `unsupported`.
fn workspace_letter(
    context: &Context,
    id: &str,
    bytes: &[u8],
    room: &str,
    json_output: bool,
    pretty: bool,
) -> AppResult<CommandResult> {
    let mut output = DeliveryOutput::new(id, "queued");
    output.room = Some(room.to_owned());
    output.sha256 = Some(hex_sha256(bytes));
    let bridge = bridge_dir(context);
    match decide_workspace(context, id, room, &bridge, &mut output) {
        Ok(true) => {}
        Ok(false) => {
            output.state = "unsupported";
            output.sha256 = None;
            output.host = None;
            output.room = None;
        }
        Err(corrupt) => {
            output.state = "unknown";
            output.reason = None;
            output.evidence_file = Some(corrupt.file.display().to_string());
            output.evidence_error = Some(corrupt.error);
        }
    }
    render(&output, json_output, pretty)
}

/// `Ok(false)` means this letter has no delivery state (`output.reason` says
/// why); `Ok(true)` means `output.state` and its fields are set.
fn decide_workspace(
    context: &Context,
    id: &str,
    room: &str,
    bridge: &Path,
    output: &mut DeliveryOutput,
) -> Result<bool, Corrupt> {
    let acked = bridge.join("room-acked").join(format!("{id}.json"));
    if let Some(record) = read_object(&acked)? {
        let sha256 = output.sha256.clone().unwrap_or_default();
        let (host, status, reason, at) =
            validate_room_ack(&record, id, room, &sha256).map_err(|error| Corrupt {
                file: acked.clone(),
                error,
            })?;
        output.state = if status == "delivered" {
            "received"
        } else {
            "rejected"
        };
        output.host = Some(host);
        output.reason = reason;
        output.acked_at = Some(at);
        return Ok(true);
    }
    let registered = crate::output::room_home(context, room);
    let host = match &registered {
        Ok(crate::output::RoomHome::Placeholder(host)) => Some(host.clone()),
        _ => None,
    };
    let marker = bridge.join("published").join(id);
    if let Some(bytes) = read_regular(&marker, EVIDENCE_MAX_BYTES).map_err(|error| Corrupt {
        file: marker.clone(),
        error,
    })? {
        let text = String::from_utf8_lossy(&bytes);
        let commit = text.trim();
        let hex = commit.bytes().all(|byte| byte.is_ascii_hexdigit());
        if !(hex && (commit.len() == 40 || commit.len() == 64)) {
            return Err(Corrupt {
                file: marker,
                error: "not a commit hash".to_owned(),
            });
        }
        output.state = "published";
        output.commit = Some(commit.to_owned());
        output.host = host;
        return Ok(true);
    }
    match registered {
        Ok(crate::output::RoomHome::Placeholder(_)) => {
            // The bridge collects the letter from the room's inbox (and tidies
            // it away once published). A room rehomed after a local delivery
            // is not evidence that this letter ever crossed hosts.
            let waiting = context
                .root
                .join(room)
                .join("inbox")
                .join(format!("{id}.mail"));
            if std::fs::symlink_metadata(&waiting).is_ok_and(|meta| meta.is_file()) {
                output.state = "queued";
                output.host = host;
                Ok(true)
            } else {
                output.reason = Some(format!(
                    "no bridge record of this letter, and it is not waiting in '{room}' for the bridge"
                ));
                Ok(false)
            }
        }
        Ok(crate::output::RoomHome::Local) => {
            output.reason = Some(format!(
                "delivered locally: '{room}' is a room on this host"
            ));
            Ok(false)
        }
        Ok(crate::output::RoomHome::Unregistered) => {
            output.reason = Some(format!(
                "'{room}' is not registered on this host and the bridge holds no record of this letter"
            ));
            Ok(false)
        }
        Err(error) => Err(Corrupt {
            file: context.root.join("rooms.json"),
            error,
        }),
    }
}

/// The bridge's room verdict: exactly `{v, id, host, room, status, reason,
/// sha256, at}`; returns (host, status, reason, at).
fn validate_room_ack(
    record: &Map<String, Value>,
    id: &str,
    room: &str,
    sha256: &str,
) -> Result<(String, &'static str, Option<String>, String), String> {
    let mut keys: Vec<&str> = record.keys().map(String::as_str).collect();
    keys.sort_unstable();
    let exact = [
        "at", "host", "id", "reason", "room", "sha256", "status", "v",
    ];
    if keys != exact {
        return Err(format!(
            "record keys are {keys:?}, expected exactly {exact:?}"
        ));
    }
    check_version(record)?;
    expect("id", string(record, "id")?, id)?;
    let recorded_room = string(record, "room")?;
    if !recorded_room.eq_ignore_ascii_case(room) {
        return Err(format!("room is {recorded_room}, expected {room}"));
    }
    let host = string(record, "host")?;
    if host.is_empty() {
        return Err("host is empty".to_owned());
    }
    let recorded = string(record, "sha256")?;
    if !valid_sha256(recorded) {
        return Err("sha256 is not 64 lowercase hex characters".to_owned());
    }
    expect("sha256", recorded, sha256)?;
    let at = string(record, "at")?;
    if crate::participant::parse_rfc3339(at).is_none() {
        return Err("at is not RFC 3339".to_owned());
    }
    let reason = optional_string(record, "reason")?;
    let status = match (string(record, "status")?, reason.as_deref()) {
        ("delivered", None) => "delivered",
        ("delivered", Some(_)) => return Err("a delivered record carries no reason".to_owned()),
        ("rejected", Some(reason)) if !reason.is_empty() => "rejected",
        ("rejected", _) => return Err("a rejected record needs a reason".to_owned()),
        (other, _) => return Err(format!("status {other} is not delivered or rejected")),
    };
    Ok((host.to_owned(), status, reason, at.to_owned()))
}

/// What every evidence file must agree with.
struct Letter<'a> {
    id: &'a str,
    participant: &'a str,
    host: &'a str,
    sha256: &'a str,
}

fn decide(
    context: &Context,
    letter: &Letter<'_>,
    evidence: &dyn Fn(&str) -> PathBuf,
    output: &mut DeliveryOutput,
) -> Result<(), Corrupt> {
    let conflicts = evidence("pmail-conflicts");
    match std::fs::symlink_metadata(&conflicts) {
        Ok(_) => output.conflict = true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(Corrupt {
                file: conflicts,
                error: format!("cannot inspect: {error}"),
            })
        }
    }
    let acked = evidence("pmail-acked");
    let receipt = read_object(&acked)?;
    if output.conflict && receipt.is_none() {
        // A conflict is recorded against a first receipt; without that
        // receipt the evidence is lost, not queued or published.
        return Err(Corrupt {
            file: conflicts,
            error: "a receipt conflict is recorded but bridge/pmail-acked has no receipt"
                .to_owned(),
        });
    }
    if let Some(receipt) = receipt {
        let own_host =
            match load_config(context) {
                Ok(Some(config)) => config.host,
                Ok(None) => return Err(Corrupt {
                    file: acked,
                    error:
                        "bridge/config.json is absent, so the receipt's origin cannot be checked"
                            .to_owned(),
                }),
                Err(error) => {
                    return Err(Corrupt {
                        file: acked,
                        error: format!("cannot check the receipt's origin: {error}"),
                    })
                }
            };
        let (status, reason, at) =
            validate_receipt(&receipt, letter, &own_host).map_err(|error| Corrupt {
                file: acked.clone(),
                error,
            })?;
        output.state = if status == "delivered" {
            "received"
        } else {
            "rejected"
        };
        output.reason = reason;
        output.acked_at = Some(at);
        return Ok(());
    }
    let marker = evidence("pmail-published");
    if let Some(published) = read_object(&marker)? {
        let (commit, at) = validate_marker(&published, letter).map_err(|error| Corrupt {
            file: marker.clone(),
            error,
        })?;
        let age = crate::participant::parse_rfc3339(&at).map(|stamp| {
            SystemTime::now()
                .duration_since(stamp)
                .map(|age| age.as_secs())
                .map_err(|ahead| ahead.duration())
        });
        // Allow the same clock skew as the health check; a marker from
        // further in the future is corrupt, never fresh.
        if let Some(Err(ahead)) = age {
            if ahead > crate::bridge_topology::MAX_CLOCK_SKEW {
                return Err(Corrupt {
                    file: marker,
                    error: format!("at is {} s in the future", ahead.as_secs()),
                });
            }
        }
        output.state = "published";
        output.age_s = age.map(|age| age.unwrap_or(0));
        output.commit = Some(commit);
        output.published_at = Some(at);
        return Ok(());
    }
    let status = evidence("pmail-status");
    if let Some(queued) = read_object(&status)? {
        let (blocked, last_error) = validate_status(&queued, letter).map_err(|error| Corrupt {
            file: status.clone(),
            error,
        })?;
        output.blocked_reason = blocked;
        output.last_error = last_error;
    }
    Ok(())
}

/// `Ok(None)` when the file is absent; a present file must be a JSON object.
fn read_object(path: &Path) -> Result<Option<Map<String, Value>>, Corrupt> {
    let corrupt = |error: String| Corrupt {
        file: path.to_path_buf(),
        error,
    };
    let Some(bytes) = read_regular(path, EVIDENCE_MAX_BYTES).map_err(corrupt)? else {
        return Ok(None);
    };
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(Value::Object(object)) => Ok(Some(object)),
        Ok(_) => Err(corrupt("not a JSON object".to_owned())),
        Err(error) => Err(corrupt(format!("not valid JSON: {error}"))),
    }
}

fn string<'a>(object: &'a Map<String, Value>, key: &str) -> Result<&'a str, String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{key} must be a string"))
}

fn optional_string(object: &Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(format!("{key} must be a string or null")),
    }
}

fn check_version(object: &Map<String, Value>) -> Result<(), String> {
    if object.get("v").and_then(Value::as_u64) == Some(1) {
        Ok(())
    } else {
        Err("v must be 1".to_owned())
    }
}

fn expect(field: &str, found: &str, wanted: &str) -> Result<(), String> {
    if found == wanted {
        Ok(())
    } else {
        Err(format!("{field} is {found}, expected {wanted}"))
    }
}

/// The design's receipt rules: exact key set, origin is this host, host is
/// the letter's `to_host`, participant and id match, sha256 is the archive
/// digest, and status/reason come from the vocabulary.
fn validate_receipt(
    receipt: &Map<String, Value>,
    letter: &Letter<'_>,
    own_host: &str,
) -> Result<(&'static str, Option<String>, String), String> {
    let mut keys: Vec<&str> = receipt.keys().map(String::as_str).collect();
    keys.sort_unstable();
    let exact = [
        "at",
        "host",
        "id",
        "origin",
        "participant",
        "reason",
        "sha256",
        "status",
        "v",
    ];
    if keys != exact {
        return Err(format!(
            "receipt keys are {keys:?}, expected exactly {exact:?}"
        ));
    }
    check_version(receipt)?;
    expect("origin", string(receipt, "origin")?, own_host)?;
    expect("host", string(receipt, "host")?, letter.host)?;
    expect(
        "participant",
        string(receipt, "participant")?,
        letter.participant,
    )?;
    expect("id", string(receipt, "id")?, letter.id)?;
    let sha256 = string(receipt, "sha256")?;
    if !valid_sha256(sha256) {
        return Err("sha256 is not 64 lowercase hex characters".to_owned());
    }
    expect("sha256", sha256, letter.sha256)?;
    let at = string(receipt, "at")?;
    if crate::participant::parse_rfc3339(at).is_none() {
        return Err("at is not RFC 3339".to_owned());
    }
    let reason = optional_string(receipt, "reason")?;
    let status = match (string(receipt, "status")?, reason.as_deref()) {
        ("delivered", None) => "delivered",
        ("delivered", Some(_)) => return Err("a delivered receipt carries no reason".to_owned()),
        ("rejected", Some(reason)) if ACKED_REJECTED_REASONS.contains(&reason) => "rejected",
        ("rejected", Some(reason)) => {
            return Err(format!(
                "rejection reason {reason} is not in the vocabulary"
            ))
        }
        ("rejected", None) => return Err("a rejected receipt needs a reason".to_owned()),
        (other, _) => return Err(format!("status {other} is not delivered or rejected")),
    };
    Ok((status, reason, at.to_owned()))
}

/// The published marker: `{v:1, id, host, sha256, commit, at}`.
fn validate_marker(
    marker: &Map<String, Value>,
    letter: &Letter<'_>,
) -> Result<(String, String), String> {
    check_version(marker)?;
    expect("id", string(marker, "id")?, letter.id)?;
    expect("host", string(marker, "host")?, letter.host)?;
    expect("sha256", string(marker, "sha256")?, letter.sha256)?;
    let commit = string(marker, "commit")?;
    if commit.is_empty() {
        return Err("commit is empty".to_owned());
    }
    let at = string(marker, "at")?;
    if crate::participant::parse_rfc3339(at).is_none() {
        return Err("at is not RFC 3339".to_owned());
    }
    Ok((commit.to_owned(), at.to_owned()))
}

/// The bridge's queued status: `{v:1, id, blocked_reason?, last_error?, at?}`.
fn validate_status(
    status: &Map<String, Value>,
    letter: &Letter<'_>,
) -> Result<(Option<String>, Option<String>), String> {
    check_version(status)?;
    expect("id", string(status, "id")?, letter.id)?;
    optional_string(status, "at")?;
    Ok((
        optional_string(status, "blocked_reason")?,
        optional_string(status, "last_error")?,
    ))
}

fn render(output: &DeliveryOutput, json_output: bool, pretty: bool) -> AppResult<CommandResult> {
    if json_output {
        return CommandResult::json(output, pretty);
    }
    let sanitize = crate::output::sanitize_text_header;
    let mut text = format!("post: delivery {}: {}", sanitize(&output.id), output.state);
    if let (Some(participant), Some(host)) = (&output.participant, &output.host) {
        text.push_str(&format!(
            " (participant:{}@{})",
            sanitize(participant),
            sanitize(host)
        ));
    } else if let Some(room) = &output.room {
        match &output.host {
            Some(host) => text.push_str(&format!(
                " (workspace:{}@{})",
                sanitize(room),
                sanitize(host)
            )),
            None => text.push_str(&format!(" (workspace:{})", sanitize(room))),
        }
    }
    text.push('\n');
    let mut line = |label: &str, value: &Option<String>| {
        if let Some(value) = value {
            text.push_str(&format!("{label}: {}\n", sanitize(value)));
        }
    };
    line("reason", &output.reason);
    line("blocked_reason", &output.blocked_reason);
    line("last_error", &output.last_error);
    line("commit", &output.commit);
    line("published_at", &output.published_at);
    line("acked_at", &output.acked_at);
    line("evidence_file", &output.evidence_file);
    line("evidence_error", &output.evidence_error);
    if let Some(age) = output.age_s {
        text.push_str(&format!("age_s: {age}\n"));
    }
    if output.conflict {
        text.push_str(
            "conflict: true (a later receipt disagreed with the first; the first stands)\n",
        );
    }
    Ok(CommandResult::success(text))
}
