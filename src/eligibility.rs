use super::{routing, ParticipantCursors};
use crate::channel::{self, ChannelPaths};
use crate::channel_state::ParticipantChannels;
use crate::error::{AppError, AppResult};
use crate::mailbox::{parse_mail, Context};
use crate::model::{ChannelMessage, Envelope};
use crate::participant::{Address, Participant};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;

#[derive(Debug)]
pub(crate) struct EligibleMail {
    pub path: PathBuf,
    pub envelope: Envelope,
    pub body: String,
}

#[derive(Debug)]
pub(crate) struct EligibleChannelMessage {
    pub path: PathBuf,
    pub message: ChannelMessage,
    pub body: String,
}

pub(crate) fn unread_mail(
    context: &Context,
    participant: &Participant,
    address: &Address,
) -> AppResult<Vec<EligibleMail>> {
    let cursors = ParticipantCursors::load(context, participant);
    let directory = routing::inbox_path(context, address);
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
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

    let mut unread = Vec::new();
    for path in paths {
        let Some(id) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        if cursors.mail_has_seen(address, id) {
            continue;
        }
        let Some(receipt) = routing::receipt(context, address, id)? else {
            continue;
        };
        if !receipt
            .recipients
            .iter()
            .any(|recipient| recipient == &participant.id)
        {
            continue;
        }
        let bytes =
            fs::read(&path).map_err(|error| AppError::io("read routed mail", &path, error))?;
        if sha256(&bytes) != receipt.digest {
            return Err(AppError::config(
                &path,
                "routed mail bytes do not match the frozen routing receipt digest",
            ));
        }
        let parsed = match parse_mail(&path) {
            Ok(parsed) => parsed,
            Err(error) if error.code == crate::error::ErrorCode::ConfigInvalid => {
                eprintln!(
                    "post: warning: skipped malformed routed mail '{}': {}",
                    path.display(),
                    error.message
                );
                continue;
            }
            Err(error) => return Err(error),
        };
        if parsed.envelope.from_participant.as_deref() == Some(participant.id.as_str()) {
            continue;
        }
        unread.push(EligibleMail {
            path,
            envelope: parsed.envelope,
            body: parsed.body,
        });
    }
    Ok(unread)
}

pub(crate) fn unread_channel(
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
    let mut unread = Vec::new();
    for path in channel::message_files(&paths.messages)? {
        let Some(id) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        if cursors.channel_has_seen(channel_name, id) {
            continue;
        }
        let parsed = channel::parse_channel_message(&path)?;
        if parsed.message.from_participant.as_deref() == Some(participant.id.as_str()) {
            continue;
        }
        unread.push(EligibleChannelMessage {
            path,
            message: parsed.message,
            body: parsed.body,
        });
    }
    unread.sort_by(|left, right| left.message.id.cmp(&right.message.id));
    Ok(unread)
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
