//! `post bridge deliver`: the bridge-only import of one relayed participant
//! letter (design: docs/plans/bridge-participant-address-design.md, rev 3.1,
//! "Destination delivery" and "The import command's contract").
//!
//! Exit 0 means post reached a decision, printed as exactly one
//! `post.bridge-deliver.v1` object. Every other exit is a retry for the
//! bridge. The admission record (`participants/<id>/imports/<mail-id>.json`)
//! is written before the inbox bytes (F3-R2-A): from the moment an imported
//! letter is visible, its frozen origin already exists.

use crate::bridge_topology;
use crate::cli::{BridgeArgs, BridgeCommand, BridgeDeliverArgs};
use crate::command_result::CommandResult;
use crate::error::{AppError, AppResult, ErrorCode};
use crate::imports::{
    hex_sha256, inbox_file, load_record, record_path, valid_mail_id, valid_sender_participant,
    valid_sha256, AdmissionRecord, MAX_IMPORT_BYTES, RECORD_VERSION,
};
use crate::mailbox::{exclusive_atomic_write, Context};
use crate::participant::{self, Address, AddressKind};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) const DELIVER_SCHEMA: &str = "post.bridge-deliver.v1";
const DETAIL_MAX_CHARS: usize = 512;

pub(super) fn run(context: &Context, args: BridgeArgs, pretty: bool) -> AppResult<CommandResult> {
    match args.command {
        BridgeCommand::Deliver(args) => deliver(context, args, pretty),
    }
}

#[derive(Debug, Serialize)]
struct DeliverOutput {
    ok: bool,
    schema: &'static str,
    outcome: &'static str,
    reason: Option<&'static str>,
    participant: String,
    mail_id: String,
    source_host: String,
    sha256: String,
    admitted_at: Option<String>,
    replay: bool,
    detail: Option<String>,
}

/// One decision. `Delivered` carries the record's `admitted_at`.
enum Decision {
    Delivered { admitted_at: String, replay: bool },
    Rejected(&'static str, String),
    Retry(&'static str, String),
}

fn deliver(context: &Context, args: BridgeDeliverArgs, pretty: bool) -> AppResult<CommandResult> {
    // Structural checks on the arguments: an ordinary usage error (exit 2),
    // which the bridge treats as `invalid_invocation`, a retry.
    participant::validate_participant_id(&args.participant)?;
    if !bridge_topology::valid_host(&args.source_host) {
        return Err(AppError::invalid_argument(format!(
            "--source-host '{}' must match ^[a-z0-9-]{{1,32}}$",
            args.source_host
        ))
        .input(args.source_host.clone())
        .reason("invalid host"));
    }
    if !valid_mail_id(&args.mail_id) {
        return Err(AppError::invalid_argument(format!(
            "--mail-id '{}' must match YYYYmmdd-HHMMSS-<6 hex>",
            args.mail_id
        ))
        .input(args.mail_id.clone())
        .reason("invalid mail id"));
    }
    if !valid_sha256(&args.sha256) {
        return Err(
            AppError::invalid_argument("--sha256 must be 64 lowercase hex characters")
                .input(args.sha256.clone())
                .reason("invalid digest"),
        );
    }
    let bytes = match bridge_topology::read_regular(&args.file, MAX_IMPORT_BYTES) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            return Err(AppError::invalid_argument(format!(
                "--file '{}' does not exist",
                args.file.display()
            ))
            .reason("missing file"))
        }
        Err(reason) => {
            return Err(AppError::invalid_argument(format!(
                "--file must be a regular file of at most {MAX_IMPORT_BYTES} bytes: {reason}"
            ))
            .reason("invalid file"))
        }
    };
    let digest = hex_sha256(&bytes);

    // Post's migration fence, like every writer. A refusal is a decided
    // retry (`fenced`) with nothing written.
    let admission = match crate::migration_fence::admit(context, true) {
        Ok(admission) => Some(admission),
        Err(error) => {
            let reason = if error.details.reason.as_deref()
                == Some("migration generation is not admitted")
            {
                "fenced"
            } else {
                "io_error"
            };
            return finish(
                &args,
                &digest,
                Decision::Retry(reason, error.message),
                None,
                pretty,
            );
        }
    };

    let own_host = match bridge_topology::load_config(context) {
        Ok(Some(config)) => config.host,
        Ok(None) => {
            let decision = Decision::Retry(
                "topology_unavailable",
                "this host has no bridge/config.json".to_owned(),
            );
            return finish(&args, &digest, decision, admission, pretty);
        }
        Err(reason) => {
            return finish(
                &args,
                &digest,
                Decision::Retry("topology_unavailable", reason),
                admission,
                pretty,
            )
        }
    };
    if args.source_host == own_host {
        return Err(AppError::invalid_argument(format!(
            "--source-host '{}' is this host; a letter cannot be imported from itself",
            args.source_host
        ))
        .input(args.source_host.clone())
        .reason("source host is this host"));
    }

    let decision = decide(context, &args, &own_host, &bytes, &digest);
    finish(&args, &digest, decision, admission, pretty)
}

fn decide(
    context: &Context,
    args: &BridgeDeliverArgs,
    own_host: &str,
    bytes: &[u8],
    digest: &str,
) -> Decision {
    if digest != args.sha256 {
        return Decision::Retry(
            "digest_mismatch",
            format!("--sha256 {} but the file digest is {digest}", args.sha256),
        );
    }
    let (from, from_participant) = match check_envelope(args, own_host, bytes) {
        Ok(checked) => checked,
        Err(rejection) => return rejection,
    };

    // Admission and `post participant end` serialize on this lock.
    let _lock = match participant::lock(context) {
        Ok(lock) => lock,
        Err(error) => return Decision::Retry("io_error", error.message),
    };

    let (record, replay) = match load_record(context, &args.participant, &args.mail_id) {
        Err(reason) => return Decision::Retry("import_record_unreadable", reason),
        Ok(Some(record)) => {
            if record.source_host != args.source_host || record.sha256 != digest {
                return Decision::Rejected(
                    "id_collision",
                    format!(
                        "mail {} was already admitted from {} with sha256 {}",
                        args.mail_id, record.source_host, record.sha256
                    ),
                );
            }
            // A valid, matching record is proof the admission checks passed;
            // they are not re-run (rev 3.1 correction 2).
            (record, true)
        }
        Ok(None) => {
            let inbox = inbox_file(context, &args.participant, &args.mail_id);
            match fs::symlink_metadata(&inbox) {
                Ok(_) => {
                    return Decision::Rejected(
                        "id_collision",
                        format!(
                            "{} exists with no admission record; its origin cannot be proven",
                            inbox.display()
                        ),
                    )
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Decision::Retry(
                        "io_error",
                        format!("cannot inspect {}: {error}", inbox.display()),
                    )
                }
            }
            if let Some(refusal) = admission_checks(context, args, &from) {
                return refusal;
            }
            match write_record(context, args, digest, &from_participant) {
                Ok(record) => (record, false),
                Err(decision) => return decision,
            }
        }
    };
    fault("d1");

    let inbox = inbox_file(context, &args.participant, &args.mail_id);
    if let Some(decision) = complete_inbox(&inbox, bytes) {
        return decision;
    }
    fault("d2");

    // The canonical file and its record exist: the letter is delivered. A
    // routing receipt failure leaves it pending for the next reader to route,
    // exactly as `post send` does.
    let address = Address {
        kind: AddressKind::Participant,
        name: args.participant.clone(),
    };
    if let Err(error) =
        crate::cursor_state::routing::route_message_locked(context, &address, &args.mail_id)
    {
        eprintln!(
            "post: warning: imported mail {} was delivered but remains pending because routing failed: {}",
            args.mail_id, error.message
        );
    }
    Decision::Delivered {
        admitted_at: record.admitted_at,
        replay,
    }
}

/// Structural envelope checks; they read only the bytes and this host's
/// identity and run on every attempt. On success, the envelope's `from` and
/// `from_participant`.
fn check_envelope(
    args: &BridgeDeliverArgs,
    own_host: &str,
    bytes: &[u8],
) -> Result<(String, String), Decision> {
    let envelope = crate::mailbox::parse_mail_bytes(&args.file, bytes)
        .map_err(|error| Decision::Rejected("malformed", error.message))?
        .envelope;
    let malformed = |detail: String| Err(Decision::Rejected("malformed", detail));
    let mismatch = |detail: String| Err(Decision::Rejected("to_mismatch", detail));
    if envelope.id != args.mail_id {
        return malformed(format!(
            "envelope id '{}' does not equal --mail-id '{}'",
            envelope.id, args.mail_id
        ));
    }
    if envelope.address_kind.as_deref() != Some("participant") {
        return malformed(format!(
            "address_kind must be 'participant', found {:?}",
            envelope.address_kind
        ));
    }
    if envelope.to != args.participant {
        return mismatch(format!(
            "envelope to '{}' does not equal --participant '{}'",
            envelope.to, args.participant
        ));
    }
    if envelope.to_host.as_deref() != Some(own_host) {
        return mismatch(format!(
            "envelope to_host {:?} is not this host '{own_host}'",
            envelope.to_host
        ));
    }
    let sender = match envelope.from_participant {
        Some(sender) if valid_sender_participant(&sender) => sender,
        other => {
            return malformed(format!(
                "from_participant {other:?} must be a valid participant id without '@'"
            ))
        }
    };
    if let Err(reason) = crate::mailbox::validate_room_name(&envelope.from) {
        return malformed(format!(
            "from '{}' is not a room name: {reason}",
            envelope.from
        ));
    }
    Ok((envelope.from, sender))
}

/// The checks that read mutable state. They run only for a letter with no
/// admission record.
fn admission_checks(context: &Context, args: &BridgeDeliverArgs, from: &str) -> Option<Decision> {
    // Trust fact 2: `from` is a placeholder homed under remote/<source-host>/.
    match crate::output::room_home(context, from) {
        Err(reason) => return Some(Decision::Retry("topology_unavailable", reason)),
        Ok(crate::output::RoomHome::Placeholder(host)) if host == args.source_host => {}
        Ok(home) => {
            let detail = match home {
                crate::output::RoomHome::Local => {
                    format!(
                        "from '{from}' names a local room, not a placeholder of {}",
                        args.source_host
                    )
                }
                crate::output::RoomHome::Placeholder(other) => format!(
                    "from '{from}' is a placeholder of {other}, not of {}",
                    args.source_host
                ),
                crate::output::RoomHome::Unregistered => {
                    format!("from '{from}' is no placeholder of {}", args.source_host)
                }
            };
            return Some(Decision::Rejected("forged_from", detail));
        }
    }
    // Conclusive lookups only (F3-3).
    match participant::load(context, &args.participant) {
        Ok(None) => {
            return Some(Decision::Rejected(
                "unknown_participant",
                format!(
                    "participant '{}' does not exist on this host",
                    args.participant
                ),
            ))
        }
        Ok(Some(record)) if record.ended_at.is_some() => {
            return Some(Decision::Rejected(
                "ended_participant",
                format!("participant '{}' has ended", args.participant),
            ))
        }
        Ok(Some(_)) => {}
        Err(error) => return Some(Decision::Retry("participant_unreadable", error.message)),
    }
    let rooms = match context.load_rooms() {
        Ok(rooms) => rooms,
        Err(error) => return Some(Decision::Retry("topology_unavailable", error.message)),
    };
    let target = Address {
        kind: AddressKind::Participant,
        name: args.participant.clone(),
    };
    match super::send::ensure_route_allowed(context, &rooms, from, &target) {
        Ok(()) => None,
        Err(error) if error.code == ErrorCode::BlockedRoute => {
            Some(Decision::Rejected("blocked_route", error.message))
        }
        Err(error) if error.code == ErrorCode::IoError => {
            Some(Decision::Retry("io_error", error.message))
        }
        Err(error) => Some(Decision::Retry("inventory_degraded", error.message)),
    }
}

/// D1, the admission point.
fn write_record(
    context: &Context,
    args: &BridgeDeliverArgs,
    digest: &str,
    from_participant: &str,
) -> Result<AdmissionRecord, Decision> {
    let admitted_at = participant::format_rfc3339(std::time::SystemTime::now())
        .map_err(|error| Decision::Retry("io_error", error.message))?;
    let record = AdmissionRecord {
        v: RECORD_VERSION,
        participant: args.participant.clone(),
        mail_id: args.mail_id.clone(),
        source_host: args.source_host.clone(),
        sha256: digest.to_owned(),
        from_participant: from_participant.to_owned(),
        admitted_at,
    };
    let path = record_path(context, &args.participant, &args.mail_id);
    let io = |detail: String| Decision::Retry("io_error", detail);
    let directory = path.parent().expect("record path has a parent");
    fs::create_dir_all(directory)
        .map_err(|error| io(format!("cannot create {}: {error}", directory.display())))?;
    let mut encoded = serde_json::to_vec(&record)
        .map_err(|error| io(format!("cannot encode admission record: {error}")))?;
    encoded.push(b'\n');
    exclusive_atomic_write(&path, &encoded)
        .map_err(|error| io(format!("cannot write {}: {error}", path.display())))?;
    Ok(record)
}

/// D2: create the canonical inbox file from the verified bytes, or verify it.
fn complete_inbox(inbox: &Path, bytes: &[u8]) -> Option<Decision> {
    let directory = inbox.parent().expect("inbox path has a parent");
    if let Err(error) = fs::create_dir_all(directory) {
        return Some(Decision::Retry(
            "io_error",
            format!("cannot create {}: {error}", directory.display()),
        ));
    }
    match exclusive_atomic_write(inbox, bytes) {
        Ok(()) => return None,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Some(Decision::Retry(
                "io_error",
                format!("cannot write {}: {error}", inbox.display()),
            ))
        }
    }
    match bridge_topology::read_regular(inbox, MAX_IMPORT_BYTES) {
        Ok(Some(existing)) if existing == bytes => None,
        Ok(Some(_)) => Some(Decision::Rejected(
            "id_collision",
            format!(
                "{} already holds different bytes; both are preserved",
                inbox.display()
            ),
        )),
        Ok(None) => Some(Decision::Retry(
            "io_error",
            format!("{} vanished while it was verified", inbox.display()),
        )),
        Err(reason) => Some(Decision::Retry("io_error", reason)),
    }
}

fn finish(
    args: &BridgeDeliverArgs,
    digest: &str,
    decision: Decision,
    admission: Option<crate::migration_fence::WriteAdmission>,
    pretty: bool,
) -> AppResult<CommandResult> {
    let (outcome, reason, admitted_at, replay, detail) = match decision {
        Decision::Delivered {
            admitted_at,
            replay,
        } => ("delivered", None, Some(admitted_at), replay, None),
        Decision::Rejected(reason, detail) => ("rejected", Some(reason), None, false, Some(detail)),
        Decision::Retry(reason, detail) => ("retry", Some(reason), None, false, Some(detail)),
    };
    let output = DeliverOutput {
        ok: true,
        schema: DELIVER_SCHEMA,
        outcome,
        reason,
        participant: args.participant.clone(),
        mail_id: args.mail_id.clone(),
        source_host: args.source_host.clone(),
        sha256: digest.to_owned(),
        admitted_at,
        replay,
        detail: detail.map(|text| truncate(&crate::output::sanitize_text_header(&text))),
    };
    let stdout = crate::output::json(&output, pretty)?;
    // Hold the writer admission until stdout is written, like every writer.
    Ok(CommandResult::after_stdout(stdout, move || {
        drop(admission);
        Ok(())
    }))
}

fn truncate(text: &str) -> String {
    text.chars().take(DETAIL_MAX_CHARS).collect()
}

/// Test-only crash and pause points around D1 and D2, compiled out of
/// release builds. `POST_TEST_DELIVER_FAULT` holds comma-separated items:
/// `crash-after-d1`, `crash-after-d2` (exit 86 with nothing further written),
/// or `pause-after-d1:<path>` / `pause-after-d2:<path>` (write `<path>.paused`,
/// then wait up to 30 s for `<path>` to exist).
#[cfg(debug_assertions)]
fn fault(point: &str) {
    let Ok(spec) = std::env::var("POST_TEST_DELIVER_FAULT") else {
        return;
    };
    for item in spec.split(',') {
        if item == format!("crash-after-{point}") {
            std::process::exit(86);
        }
        if let Some(release) = item.strip_prefix(&format!("pause-after-{point}:")) {
            let release = PathBuf::from(release);
            let mut marker = release.clone().into_os_string();
            marker.push(".paused");
            let _ = fs::write(&marker, b"1\n");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while !release.exists() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }
}

#[cfg(not(debug_assertions))]
fn fault(_point: &str) {}
