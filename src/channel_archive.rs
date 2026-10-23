//! Channel archive: hide a channel from the live listing without touching it.
//!
//! Trey ruling 2026-09-22: channels are never deleted; any agent (and Trey,
//! through Porch) may archive one, which drops it out of `post channels` and
//! Porch's picker until someone resurrects it.
//!
//! The state lives in `channels/<name>/archive.json`, beside the channel and
//! outside its history. Deliberately NOT an event message: history is bridged
//! to other hosts, and every existing reader refuses unknown event kinds, so
//! an archive event would make older binaries fail to read the channel. The
//! sidecar is invisible to them, raises no doorbell, and archive stays a
//! host-local view choice.
//!
//! A channel is archived when `archive.json` names a mark AND no
//! conversational (non-event) message is newer than the mark's `through` id.
//! A new post therefore resurrects the channel on its own, with no write to
//! the sidecar; joins and profile announcements are events and do not, so
//! Porch's automatic owner joins cannot quietly un-archive everything. The
//! `log` array only grows: explicit archive/unarchive and automatic idle
//! archive transitions are recorded.

use crate::channel::{self, ChannelPaths};
use crate::error::{AppError, AppResult, ErrorCode};
use crate::mailbox::{atomic_replace, local_timestamp_micros, Context};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

pub(crate) const ARCHIVE_FILE: &str = "archive.json";
const ARCHIVE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ArchiveMark {
    /// Newest conversational message id when archived; None when the
    /// channel had no conversational messages yet.
    pub through: Option<String>,
    pub at: String,
    pub by_participant: String,
    pub by_room: String,
    /// Absent on manual marks written by older binaries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ArchiveLogEntry {
    pub action: String,
    pub at: String,
    pub by_participant: String,
    pub by_room: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub through: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ArchiveFile {
    pub version: u32,
    #[serde(default)]
    pub archived: Option<ArchiveMark>,
    #[serde(default)]
    pub log: Vec<ArchiveLogEntry>,
}

pub(crate) fn archive_path(paths: &ChannelPaths) -> PathBuf {
    paths.dir.join(ARCHIVE_FILE)
}

/// Read the sidecar. Absent means never archived.
pub(crate) fn load(paths: &ChannelPaths) -> AppResult<Option<ArchiveFile>> {
    let path = archive_path(paths);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(AppError::io("read channel archive state", &path, error)),
    };
    let file: ArchiveFile = serde_json::from_slice(&bytes)
        .map_err(|error| AppError::config(&path, format!("invalid archive.json: {error}")))?;
    if file.version != ARCHIVE_VERSION {
        return Err(AppError::config(
            &path,
            format!(
                "archive.json version {} is not supported (expected {ARCHIVE_VERSION})",
                file.version
            ),
        ));
    }
    Ok(Some(file))
}

/// Newest message id in the channel that is conversation, not an event.
/// Scans newest-first, so an active channel costs one parse. Unreadable
/// files are skipped: archive state is a view filter, and doctor owns
/// malformed-message findings.
pub(crate) fn newest_conversational_id(paths: &ChannelPaths) -> AppResult<Option<String>> {
    if !paths.messages.is_dir() {
        return Ok(None);
    }
    for path in channel::message_files(&paths.messages)?.into_iter().rev() {
        if let Ok(parsed) = channel::parse_channel_message(&path) {
            if parsed.message.event.is_none() {
                return Ok(Some(parsed.message.id));
            }
        }
    }
    Ok(None)
}

/// The mark currently in force, or None when the channel is live — either
/// never archived, explicitly unarchived, or resurrected by a newer post.
pub(crate) fn effective_mark(paths: &ChannelPaths) -> AppResult<Option<ArchiveMark>> {
    let Some(file) = load(paths)? else {
        return Ok(None);
    };
    let Some(mark) = file.archived else {
        return Ok(None);
    };
    let files = channel::message_files(&paths.messages)?;
    Ok(mark_in_force(mark, &files))
}

fn mark_in_force(mark: ArchiveMark, files: &[PathBuf]) -> Option<ArchiveMark> {
    // Only files past the mark can revive it. In particular an auto-archive
    // records the newest filename, so quiet archived channels open no bodies.
    for path in files {
        let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if mark.through.as_deref().is_some_and(|through| id <= through) {
            continue;
        }
        if channel::parse_channel_message(path).is_ok_and(|p| p.message.event.is_none()) {
            return None;
        }
    }
    Some(mark)
}

/// Lazily enforce the 24h lifecycle. Call outside the channels lock. A candidate
/// is rechecked under that lock so a racing send/archive can never be hidden by
/// a stale listing. Listing on a read-only store fails open (the caller reports
/// the channel live), rather than losing access to history.
pub(crate) fn refresh_idle(
    context: &Context,
    paths: &ChannelPaths,
    files: &[PathBuf],
) -> AppResult<Option<ArchiveMark>> {
    let file = load(paths)?;
    if let Some(mark) = file.as_ref().and_then(|file| file.archived.clone()) {
        if let Some(mark) = mark_in_force(mark, files) {
            return Ok(Some(mark));
        }
    }
    if !is_idle(paths, file.as_ref(), files)? {
        return Ok(None);
    }
    // Readers opportunistically take writer admission only for a transition;
    // a fenced store stays readable and unchanged. A writer command already
    // holds admission from dispatch and must not recursively acquire its flock.
    let _admission = if crate::mailbox::read_only_command() {
        Some(crate::migration_fence::admit(context, true)?)
    } else {
        None
    };
    let _lock = channel::lock_channels(context)?;
    let files = channel::message_files(&paths.messages)?;
    let mut file = load(paths)?.unwrap_or(ArchiveFile {
        version: ARCHIVE_VERSION,
        archived: None,
        log: Vec::new(),
    });
    if let Some(mark) = file
        .archived
        .clone()
        .and_then(|mark| mark_in_force(mark, &files))
    {
        return Ok(Some(mark));
    }
    if !is_idle(paths, Some(&file), &files)? {
        return Ok(None);
    }
    let (_, at) = local_timestamp_micros()?;
    let mark = ArchiveMark {
        through: files
            .iter()
            .filter_map(|p| p.file_stem()?.to_str())
            .max()
            .map(str::to_owned),
        at: at.clone(),
        by_participant: "post".to_owned(),
        by_room: String::new(),
        reason: Some("idle".to_owned()),
    };
    file.log.push(ArchiveLogEntry {
        action: "auto_archive".to_owned(),
        at,
        by_participant: mark.by_participant.clone(),
        by_room: String::new(),
        through: mark.through.clone(),
    });
    file.archived = Some(mark.clone());
    save(paths, &file)?;
    Ok(Some(mark))
}

fn is_idle(paths: &ChannelPaths, file: Option<&ArchiveFile>, files: &[PathBuf]) -> AppResult<bool> {
    is_idle_at(paths, file, files, std::time::SystemTime::now())
}

fn is_idle_at(
    paths: &ChannelPaths,
    file: Option<&ArchiveFile>,
    files: &[PathBuf],
    now: std::time::SystemTime,
) -> AppResult<bool> {
    use std::time::Duration;
    let cutoff = now
        .checked_sub(Duration::from_secs(24 * 60 * 60))
        .expect("current time exceeds one day");
    // An explicit unarchive grants a fresh day even when history is old.
    if file
        .and_then(|f| f.log.last())
        .filter(|e| e.action == "unarchive")
        .and_then(|e| crate::participant::parse_sent_timestamp(&e.at))
        .is_some_and(|at| at > cutoff)
    {
        return Ok(false);
    }
    let cutoff_id = crate::mailbox::utc_id_watermark(cutoff)?;
    if let Some(newest) = files.iter().filter_map(|p| p.file_stem()?.to_str()).max() {
        // Ignore the random suffix: exactly 24 hours is already idle.
        return Ok(newest.get(..cutoff_id.len()).unwrap_or(newest) <= cutoff_id.as_str());
    }
    // A channel with no messages ages from creation. An invalid timestamp is
    // indeterminate, never a reason to hide a conversation.
    Ok(
        crate::participant::parse_sent_timestamp(&paths.load_info()?.created)
            .is_some_and(|at| at <= cutoff),
    )
}

fn save(paths: &ChannelPaths, file: &ArchiveFile) -> AppResult<()> {
    let path = archive_path(paths);
    let mut bytes = serde_json::to_vec_pretty(file)
        .map_err(|error| AppError::config(&path, format!("serialize archive.json: {error}")))?;
    bytes.push(b'\n');
    atomic_replace(&path, &bytes)
        .map_err(|error| AppError::io("write channel archive state", &path, error))
}

#[derive(Debug, Serialize)]
pub(crate) struct ArchiveOutcome {
    /// State after the command.
    pub archived: bool,
    /// False when the channel was already in the requested state.
    pub changed: bool,
    pub mark: Option<ArchiveMark>,
}

/// Archive (`archive = true`) or unarchive a channel. Any bound participant
/// may do either; membership is not required. Idempotent: asking for the
/// state the channel is already in writes nothing.
pub(crate) fn set_archived(
    context: &Context,
    channel_name: &str,
    archive: bool,
) -> AppResult<ArchiveOutcome> {
    let rooms = context.load_rooms()?;
    let (room, _) = channel::acting_room(context, &rooms)?;
    let actor = context.sender()?;
    let paths = ChannelPaths::new(context, channel_name)?;
    let _lock = channel::lock_channels(context)?;
    if !paths.exists() {
        return Err(AppError::new(
            ErrorCode::NotFound,
            format!("channel '{channel_name}' does not exist"),
            "List channels with `post channels --all`; archive never creates a channel.",
        )
        .input(channel_name));
    }

    let current = effective_mark(&paths)?;
    if current.is_some() == archive {
        return Ok(ArchiveOutcome {
            archived: archive,
            changed: false,
            mark: current,
        });
    }

    let mut file = load(&paths)?.unwrap_or(ArchiveFile {
        version: ARCHIVE_VERSION,
        archived: None,
        log: Vec::new(),
    });
    let (_, at) = local_timestamp_micros()?;
    let through = if archive {
        newest_conversational_id(&paths)?
    } else {
        None
    };
    let mark = ArchiveMark {
        through: through.clone(),
        at: at.clone(),
        by_participant: actor.participant.id.clone(),
        by_room: room.clone(),
        reason: None,
    };
    file.log.push(ArchiveLogEntry {
        action: if archive { "archive" } else { "unarchive" }.to_owned(),
        at,
        by_participant: mark.by_participant.clone(),
        by_room: room,
        through,
    });
    file.archived = archive.then(|| mark.clone());

    save(&paths, &file)?;
    Ok(ArchiveOutcome {
        archived: archive,
        changed: true,
        mark: archive.then_some(mark),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_threshold_is_twenty_four_hours_in_utc() {
        let paths = ChannelPaths {
            dir: PathBuf::new(),
            messages: PathBuf::new(),
            channel_json: PathBuf::new(),
            members_json: PathBuf::new(),
        };
        let now = crate::participant::parse_sent_timestamp("2026-10-20 00:00:00 +0000").unwrap();
        for (id, idle) in [
            ("20261019-010000-000000-abcdef", false), // 23 hours
            ("20261019-000000-000000-abcdef", true),  // exactly 24 hours
            ("20261018-235959-999999-abcdef", true),
            ("20261021-000000-000000-abcdef", false), // future clock
        ] {
            let files = vec![PathBuf::from(format!("{id}.msg"))];
            assert_eq!(is_idle_at(&paths, None, &files, now).unwrap(), idle, "{id}");
        }
    }
}
