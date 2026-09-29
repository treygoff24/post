//! What `post participant gc` leaves behind, and the moves it is made of.
//!
//! Participant ids are deterministic (`<harness>-<digest prefix>`), so a
//! record that holds nothing can be deleted and the next `bind` for its
//! session key mints it again under the same id. Two things keep that true:
//!
//! - a tombstone line in `participants/archived.jsonl` records who a deleted id
//!   belonged to, so another key cannot take a freed 8-character id and push
//!   the first key's re-mint onto the 12-character one (the id would change and
//!   every letter addressed to the old id would be orphaned);
//! - a record worth keeping is moved whole to `participants-archive/<id>/`,
//!   which `bind` moves back before minting anything.
//!
//! The planner that decides what to collect lives with the command
//! (`commands/participant_gc.rs`); this file is the storage layer it and
//! `participant::resolve`/`bind` share. Every function that changes state
//! expects the caller to hold the participants lock.

use super::{index_path, PARTICIPANTS_DIR};
use crate::error::{AppError, AppResult};
use crate::mailbox::Context;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

/// Tombstones live beside the records they replace: `participants/archived.jsonl`.
/// Listing skips it (only directories are participants).
pub(crate) const TOMBSTONES_FILE: &str = "archived.jsonl";
/// Records moved aside by tier 2, outside every scan of `participants/`.
pub(crate) const ARCHIVE_DIR: &str = "participants-archive";
/// A deleted directory is renamed here before it is removed, so it vanishes
/// from `participants/` in one step; a kill before the removal leaves only
/// this leftover, which the next run sweeps.
const DELETING_PREFIX: &str = ".deleting-";
const MAX_TOMBSTONE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ARCHIVED_RECORD_BYTES: u64 = 64 * 1024;

/// One deleted participant, as `participants/archived.jsonl` keeps it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Tombstone {
    pub id: String,
    pub harness: String,
    pub conversation_key_digest: String,
    pub created: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ephemeral: bool,
    /// What a recreated record needs beyond the id: a tombstone written before
    /// these fields existed simply leaves them unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_hours: Option<u64>,
    pub reason: String,
    pub archived_at: String,
}

/// Who an id that has no live record belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Holder {
    Nobody,
    /// Deleted by tier 1; the key digest it belonged to.
    Tombstone {
        digest: String,
    },
    /// Moved aside by tier 2, restorable; the key digest it belongs to.
    Archived {
        digest: String,
    },
}

pub(crate) fn tombstones_path(context: &Context) -> PathBuf {
    context.root.join(PARTICIPANTS_DIR).join(TOMBSTONES_FILE)
}

pub(crate) fn archive_root(context: &Context) -> PathBuf {
    context.root.join(ARCHIVE_DIR)
}

pub(crate) fn archived_dir(context: &Context, id: &str) -> PathBuf {
    archive_root(context).join(id)
}

/// Who holds `id` now that no live record does. An archived record wins over a
/// tombstone: restoring it is the more specific answer.
pub(crate) fn holder(context: &Context, id: &str) -> AppResult<Holder> {
    if let Some(digest) = archived_digest(context, id)? {
        return Ok(Holder::Archived { digest });
    }
    Ok(match tombstone_digest(context, id)? {
        Some(digest) => Holder::Tombstone { digest },
        None => Holder::Nobody,
    })
}

fn archived_digest(context: &Context, id: &str) -> AppResult<Option<String>> {
    let path = archived_dir(context, id).join(super::RECORD_FILE);
    let Some(bytes) = super::read_bounded_optional(&path, MAX_ARCHIVED_RECORD_BYTES)? else {
        return Ok(None);
    };
    // An archived record that cannot be read still occupies its id: an empty
    // digest matches no key, so the id is held by someone else.
    let digest = serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|value| {
            value
                .get("conversation_key_digest")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_default();
    Ok(Some(digest))
}

fn tombstone_digest(context: &Context, id: &str) -> AppResult<Option<String>> {
    Ok(latest_tombstone(context, id)?.map(|tombstone| tombstone.conversation_key_digest))
}

/// The newest tombstone for `id`: what a recreated record is built from.
pub(crate) fn latest_tombstone(context: &Context, id: &str) -> AppResult<Option<Tombstone>> {
    Ok(read_tombstones(context)?
        .into_iter()
        .rev()
        .find(|tombstone| tombstone.id == id))
}

/// Every readable tombstone, oldest first. An unreadable line is skipped: it
/// can only make an id look free again, and the id's key re-mints it either way.
pub(crate) fn read_tombstones(context: &Context) -> AppResult<Vec<Tombstone>> {
    let path = tombstones_path(context);
    let Some(bytes) = super::read_bounded_optional(&path, MAX_TOMBSTONE_BYTES)? else {
        return Ok(Vec::new());
    };
    Ok(String::from_utf8_lossy(&bytes)
        .lines()
        .filter_map(|line| serde_json::from_str::<Tombstone>(line).ok())
        .collect())
}

/// Append one tombstone durably. Called before the directory it describes goes
/// away, so a kill in between leaves a live record plus a harmless tombstone.
pub(crate) fn append_tombstone(context: &Context, tombstone: &Tombstone) -> AppResult<()> {
    let path = tombstones_path(context);
    let parent = path.parent().expect("tombstones have a parent");
    fs::create_dir_all(parent)
        .map_err(|error| AppError::io("create participants directory", parent, error))?;
    let mut line = serde_json::to_vec(tombstone)
        .map_err(|error| AppError::io("serialize participant tombstone", &path, error))?;
    line.push(b'\n');
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)
        .map_err(|error| AppError::io("open participant tombstones", &path, error))?;
    file.write_all(&line)
        .and_then(|()| file.sync_all())
        .map_err(|error| AppError::io("append participant tombstone", &path, error))
}

/// Delete a participant's directory: rename it out of `participants/` in one
/// step, then remove it.
pub(crate) fn delete_dir(context: &Context, id: &str) -> AppResult<()> {
    let dir = context.root.join(PARTICIPANTS_DIR).join(id);
    let root = archive_root(context);
    fs::create_dir_all(&root)
        .map_err(|error| AppError::io("create participant archive", &root, error))?;
    let aside = root.join(format!("{DELETING_PREFIX}{id}"));
    if aside.exists() {
        fs::remove_dir_all(&aside)
            .map_err(|error| AppError::io("clear stale deleted participant", &aside, error))?;
    }
    fs::rename(&dir, &aside)
        .map_err(|error| AppError::io("move participant out of the registry", &dir, error))?;
    // The participant is gone from every scan. A failure past this point only
    // leaves the leftover for the next run's sweep.
    let _ = fs::remove_dir_all(&aside);
    Ok(())
}

/// Remove directories a previous run renamed aside and did not finish deleting.
pub(crate) fn sweep_leftovers(context: &Context) {
    let Ok(entries) = fs::read_dir(archive_root(context)) else {
        return;
    };
    for entry in entries.flatten() {
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(DELETING_PREFIX))
        {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// Move a participant's directory, whole, to the archive.
pub(crate) fn archive_dir(context: &Context, id: &str) -> AppResult<()> {
    let dir = context.root.join(PARTICIPANTS_DIR).join(id);
    let root = archive_root(context);
    fs::create_dir_all(&root)
        .map_err(|error| AppError::io("create participant archive", &root, error))?;
    let target = archived_dir(context, id);
    if target.exists() {
        return Err(AppError::config(
            &target,
            "an archived record already holds this participant id",
        ));
    }
    fs::rename(&dir, &target).map_err(|error| AppError::io("archive participant", &dir, error))
}

/// Move an archived participant back into the registry, whole: cursors,
/// memberships and inbox included, so a resumed session re-reads nothing.
pub(crate) fn restore(context: &Context, id: &str) -> AppResult<()> {
    let from = archived_dir(context, id);
    let to = context.root.join(PARTICIPANTS_DIR).join(id);
    fs::rename(&from, &to)
        .map_err(|error| AppError::io("restore archived participant", &from, error))
}

/// Undo `restore` for a record that turned out to be unreadable: an archive
/// that cannot be read is evidence, so it goes back where it was found rather
/// than being left half-restored in the registry.
pub(crate) fn unrestore(context: &Context, id: &str) -> AppResult<()> {
    let from = context.root.join(PARTICIPANTS_DIR).join(id);
    let to = archived_dir(context, id);
    fs::rename(&from, &to)
        .map_err(|error| AppError::io("return unreadable participant to the archive", &from, error))
}

/// Remove the by-session index entry for a key, but only while it still names
/// `id` (a rebind may already have pointed it elsewhere).
pub(crate) fn remove_index(
    context: &Context,
    harness: &str,
    digest: &str,
    id: &str,
) -> AppResult<()> {
    let path = index_path(context, harness, digest);
    let Some(bytes) = super::read_bounded_optional(&path, 256)? else {
        return Ok(());
    };
    if String::from_utf8_lossy(&bytes).trim() != id {
        return Ok(());
    }
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(AppError::io(
            "remove participant session index",
            &path,
            error,
        )),
    }
}

/// Whether a participant-directory entry is scaffolding rather than state:
/// nothing here is worth keeping once the record itself may go.
pub(crate) fn is_stateless_entry(name: &str, path: &Path) -> bool {
    match name {
        "participant.json" | "activation-notice" | "activation-claim" | ".cursors.lock"
        | "watch.heartbeat" => true,
        // Scaffolding directories a first delivery or import leaves behind.
        "inbox" | "routing" | "imports" => fs::read_dir(path)
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(false),
        _ => false,
    }
}
