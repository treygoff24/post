use super::{routing, ParticipantCursors};
use crate::channel::{self, ChannelPaths};
use crate::channel_state::ParticipantChannels;
use crate::error::{AppError, AppResult};
use crate::mailbox::{parse_mail, Context};
use crate::model::{ChannelMessage, Envelope, ParsedMail};
use crate::participant::{Address, Participant};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

#[derive(Debug)]
pub(crate) struct EligibleMail {
    pub path: PathBuf,
    pub envelope: Envelope,
    pub body: String,
    /// True only when the immutable receipt names this participant. This is
    /// the sole authority for participant cursor consumption.
    pub recipient: bool,
    /// Sender-history visibility is independent from receipt eligibility.
    pub own: bool,
    /// No receipt exists yet. Pending mail is never consumable.
    pub pending: bool,
}

#[derive(Debug)]
pub(crate) struct EligibleChannelMessage {
    pub path: PathBuf,
    pub message: ChannelMessage,
    pub body: String,
    pub own: bool,
    pub already_read: bool,
}

#[derive(Debug, Default)]
pub(crate) struct MailSnapshot {
    pub items: Vec<EligibleMail>,
    pub skipped_unreadable: usize,
}

pub(crate) fn unread_mail(
    context: &Context,
    participant: &Participant,
    address: &Address,
) -> AppResult<Vec<EligibleMail>> {
    Ok(unread_mail_snapshot(context, participant, address)?.items)
}

pub(crate) fn unread_mail_snapshot(
    context: &Context,
    participant: &Participant,
    address: &Address,
) -> AppResult<MailSnapshot> {
    let cursors = ParticipantCursors::load(context, participant);
    let mut snapshot = visible_mail_snapshot(context, participant, address)?;
    snapshot
        .items
        .retain(|item| item.recipient && !cursors.mail_has_seen(address, &item.envelope.id));
    Ok(snapshot)
}

/// One receipt/digest/parser/status path for every participant mail projection.
///
/// A message is visible when the participant is a frozen receipt recipient,
/// authored it, or is provisionally eligible while the receipt is absent.
/// The returned status is reused by read, search, and unread selection so
/// visibility and consumption cannot drift into command-local predicates.
pub(crate) fn visible_mail(
    context: &Context,
    participant: &Participant,
    address: &Address,
) -> AppResult<Vec<EligibleMail>> {
    Ok(visible_mail_snapshot(context, participant, address)?.items)
}

pub(crate) fn visible_mail_snapshot(
    context: &Context,
    participant: &Participant,
    address: &Address,
) -> AppResult<MailSnapshot> {
    let directory = routing::inbox_path(context, address);
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(MailSnapshot::default())
        }
        Err(error) => {
            return Err(AppError::io(
                "list canonical mail directory",
                &directory,
                error,
            ))
        }
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|error| AppError::io("read canonical mail entry", &directory, error))?;
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|value| value.to_str()) == Some("mail") {
            paths.push(path);
        }
    }
    paths.sort();

    let provisional: BTreeSet<String> =
        routing::provisional_pending_for_quiet(context, participant, address)?
            .into_iter()
            .collect();
    let mut visible = Vec::new();
    let mut skipped_unreadable = 0;
    for path in paths {
        let Some(id) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        let receipt = match routing::receipt(context, address, id) {
            Ok(receipt) => receipt,
            Err(error) if error.code == crate::error::ErrorCode::ConfigInvalid => {
                routing::warn_once(
                    routing::receipt_path(context, address, id),
                    format!("corrupt routing receipt skipped: {}", error.message),
                );
                skipped_unreadable += 1;
                continue;
            }
            Err(error) => return Err(error),
        };
        if let Some(receipt) = receipt.as_ref() {
            match parse_routed_mail(&path, receipt) {
                Ok(parsed) => {
                    let own = parsed.envelope.from_participant.as_deref()
                        == Some(participant.id.as_str());
                    let recipient = receipt.recipients.contains(&participant.id);
                    if recipient || own {
                        visible.push(EligibleMail {
                            path,
                            envelope: parsed.envelope,
                            body: parsed.body,
                            recipient,
                            own,
                            pending: false,
                        });
                    }
                }
                Err(error)
                    if matches!(
                        error.code,
                        crate::error::ErrorCode::ConfigInvalid | crate::error::ErrorCode::IoError
                    ) =>
                {
                    routing::warn_once(
                        path.clone(),
                        format!(
                            "routed mail skipped after digest mismatch or parse failure: {}",
                            error.message
                        ),
                    );
                    skipped_unreadable += 1;
                }
                Err(error) => return Err(error),
            }
            continue;
        }
        let parsed = match parse_mail(&path) {
            Ok(parsed) => parsed,
            // Pending malformed siblings are not visible evidence. An
            // explicit read still parses its matching path and reports the
            // corruption rather than turning it into a not-found result.
            Err(_) if receipt.is_none() => {
                skipped_unreadable += 1;
                continue;
            }
            Err(error) => return Err(error),
        };
        let own = parsed.envelope.from_participant.as_deref() == Some(participant.id.as_str());
        if !own && !provisional.contains(id) {
            continue;
        }
        visible.push(EligibleMail {
            path,
            envelope: parsed.envelope,
            body: parsed.body,
            recipient: false,
            own,
            pending: true,
        });
    }
    Ok(MailSnapshot {
        items: visible,
        skipped_unreadable,
    })
}

pub(crate) fn strict_visible_routed_mail(
    context: &Context,
    participant: &Participant,
    address: &Address,
    path: &std::path::Path,
) -> AppResult<Option<EligibleMail>> {
    let Some(id) = path.file_stem().and_then(|value| value.to_str()) else {
        return Ok(None);
    };
    let Some(receipt) = routing::receipt(context, address, id)? else {
        return Ok(None);
    };
    let parsed = parse_routed_mail(path, &receipt)?;
    let own = parsed.envelope.from_participant.as_deref() == Some(participant.id.as_str());
    let recipient = receipt.recipients.contains(&participant.id);
    if !recipient && !own {
        return Ok(None);
    }
    Ok(Some(EligibleMail {
        path: path.to_path_buf(),
        envelope: parsed.envelope,
        body: parsed.body,
        recipient,
        own,
        pending: false,
    }))
}

fn parse_routed_mail(path: &std::path::Path, receipt: &routing::Receipt) -> AppResult<ParsedMail> {
    let bytes = fs::read(path).map_err(|error| AppError::io("read routed mail", path, error))?;
    if sha256(&bytes) != receipt.digest {
        return Err(AppError::config(
            path,
            "routed mail digest mismatch: bytes do not match the frozen routing receipt",
        ));
    }
    parse_mail(path)
}

pub(crate) fn unread_channel(
    context: &Context,
    participant: &Participant,
    channel_name: &str,
) -> AppResult<Vec<EligibleChannelMessage>> {
    Ok(visible_channel(context, participant, channel_name)?
        .into_iter()
        .filter(|item| !item.own && !item.already_read)
        .collect())
}

/// Complete channel history for an effective member. Read state and sender
/// status are annotations, not visibility filters.
pub(crate) fn visible_channel(
    context: &Context,
    participant: &Participant,
    channel_name: &str,
) -> AppResult<Vec<EligibleChannelMessage>> {
    let membership = ParticipantChannels::load(participant)?;
    if !membership.effective(context, participant, channel_name)? {
        return Ok(Vec::new());
    }
    let cursors = ParticipantCursors::load(context, participant);
    let paths = ChannelPaths::new(context, channel_name)?;
    if !paths.exists() {
        return Ok(Vec::new());
    }
    let mut visible = Vec::new();
    for path in channel::message_files(&paths.messages)? {
        let parsed = channel::parse_channel_message(&path)?;
        let own = parsed.message.from_participant.as_deref() == Some(participant.id.as_str());
        let already_read = cursors.channel_has_seen(channel_name, &parsed.message.id);
        visible.push(EligibleChannelMessage {
            path,
            message: parsed.message,
            body: parsed.body,
            own,
            already_read,
        });
    }
    visible.sort_by(|left, right| left.message.id.cmp(&right.message.id));
    Ok(visible)
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
