//! Admission records for imported participant letters, and the one origin
//! lookup that reads them (design rev 3.1, F3-5 and F3-R2-A).
//!
//! `participants/<id>/imports/<mail-id>.json` is written by
//! `post bridge deliver` before the inbox bytes. It is the admission point,
//! the idempotence ledger, and the frozen origin of the letter.

use crate::bridge_topology;
use crate::mailbox::Context;
use crate::participant;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

pub(crate) const IMPORTS_DIR: &str = "imports";
pub(crate) const RECORD_VERSION: u64 = 1;
const RECORD_MAX_BYTES: u64 = 4096;
/// Post's cap on one relayed letter. The bridge's default is 1 MiB and it is
/// configurable, so post accepts a margin above it rather than the bridge's
/// exact number.
pub(crate) const MAX_IMPORT_BYTES: u64 = 8 * 1024 * 1024;

/// The admission record: admission point, idempotence ledger, and frozen
/// origin in one immutable file. Exact keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AdmissionRecord {
    pub v: u64,
    pub participant: String,
    pub mail_id: String,
    pub source_host: String,
    pub sha256: String,
    pub from_participant: String,
    pub admitted_at: String,
}

pub(crate) fn record_path(context: &Context, participant: &str, mail_id: &str) -> PathBuf {
    context
        .root
        .join(participant::PARTICIPANTS_DIR)
        .join(participant)
        .join(IMPORTS_DIR)
        .join(format!("{mail_id}.json"))
}

/// Read an admission record. `Ok(None)` only when conclusively absent. Any
/// unreadable, oversize, or invalid record is `Err`: it never becomes a
/// terminal answer and never licenses a write.
pub(crate) fn load_record(
    context: &Context,
    participant: &str,
    mail_id: &str,
) -> Result<Option<AdmissionRecord>, String> {
    let path = record_path(context, participant, mail_id);
    let Some(bytes) = bridge_topology::read_regular(&path, RECORD_MAX_BYTES)? else {
        return Ok(None);
    };
    let record: AdmissionRecord = serde_json::from_slice(&bytes).map_err(|error| {
        format!(
            "{} is not a valid admission record: {error}",
            path.display()
        )
    })?;
    let problem = if record.v != RECORD_VERSION {
        Some("v must be 1")
    } else if record.participant != participant {
        Some("participant does not match its directory")
    } else if record.mail_id != mail_id {
        Some("mail_id does not match its file name")
    } else if !bridge_topology::valid_host(&record.source_host) {
        Some("source_host is not a valid host")
    } else if !valid_sha256(&record.sha256) {
        Some("sha256 must be 64 lowercase hex characters")
    } else if !valid_sender_participant(&record.from_participant) {
        Some("from_participant is not a valid remote participant id")
    } else if participant::parse_rfc3339(&record.admitted_at).is_none() {
        Some("admitted_at must be an RFC3339 timestamp")
    } else {
        None
    };
    match problem {
        Some(problem) => Err(format!("{}: {problem}", path.display())),
        None => Ok(Some(record)),
    }
}

pub(crate) fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(crate) fn valid_mail_id(value: &str) -> bool {
    let id = value.as_bytes();
    id.len() == 22
        && id[..8].iter().all(u8::is_ascii_digit)
        && id[8] == b'-'
        && id[9..15].iter().all(u8::is_ascii_digit)
        && id[15] == b'-'
        && id[16..].iter().all(u8::is_ascii_hexdigit)
}

/// A sender id minted on another host: a valid participant id with no `@`.
pub(crate) fn valid_sender_participant(value: &str) -> bool {
    participant::validate_participant_id(value).is_ok() && !value.contains('@')
}

pub(crate) fn inbox_file(context: &Context, participant: &str, mail_id: &str) -> PathBuf {
    context
        .root
        .join(participant::PARTICIPANTS_DIR)
        .join(participant)
        .join("inbox")
        .join(format!("{mail_id}.mail"))
}

pub(crate) fn hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// What an admission record proves about one mail envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ImportOrigin {
    /// A valid record whose sha256 matches the canonical inbox bytes: the
    /// letter came from `host`, sent by that host's `participant`.
    Remote { host: String, participant: String },
    /// A record exists but is unreadable, invalid, or does not match the
    /// inbox bytes. The origin is unknown; it is never local.
    Unavailable,
}

/// The one origin lookup for host-qualified participant letters (F3-5).
/// `None` means the letter carries no import evidence (no `to_host`, or no
/// record), and the R7 rule (`output::remote_origin`) decides. The answer
/// comes from the record, never from the current placeholder table, so a
/// letter keeps its host after its placeholder is removed or re-homed.
pub(crate) fn import_origin(
    context: &Context,
    envelope: &crate::model::Envelope,
) -> Option<ImportOrigin> {
    if envelope.to_host.is_none() || envelope.address_kind.as_deref() != Some("participant") {
        return None;
    }
    if participant::validate_participant_id(&envelope.to).is_err() || !valid_mail_id(&envelope.id) {
        return Some(ImportOrigin::Unavailable);
    }
    match load_record(context, &envelope.to, &envelope.id) {
        Ok(None) => None,
        Err(_) => Some(ImportOrigin::Unavailable),
        Ok(Some(record)) => {
            let inbox = inbox_file(context, &envelope.to, &envelope.id);
            match bridge_topology::read_regular(&inbox, MAX_IMPORT_BYTES) {
                Ok(Some(bytes)) if hex_sha256(&bytes) == record.sha256 => {
                    Some(ImportOrigin::Remote {
                        host: record.source_host,
                        participant: record.from_participant,
                    })
                }
                _ => Some(ImportOrigin::Unavailable),
            }
        }
    }
}
