//! `participant bind --new --adopt <id>`: a new participant takes over an
//! ended (or lapsed) one's display name, sigil and channel memberships.

use super::{
    format_rfc3339, load, lock, validate_participant_id, write_record, Participant,
    ParticipantState,
};
use crate::error::{AppError, AppResult, ErrorCode};
use crate::mailbox::Context;
use serde::Serialize;
use std::time::SystemTime;

/// What an adoption carried over, as `--json` reports it.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Adopted {
    pub from: String,
    pub at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pfp: Option<String>,
    pub channels: Vec<String>,
}

/// The adoptable record for `id`, or the reason it cannot be adopted. Pure
/// read: callers run it before minting anything so a refusal leaves no
/// orphan participant behind.
pub(crate) fn check(context: &Context, id: &str) -> AppResult<Participant> {
    validate_participant_id(id)?;
    let old = load(context, id)?.ok_or_else(|| {
        AppError::new(
            ErrorCode::InvalidArgument,
            format!("cannot adopt participant '{id}': no such participant record"),
            "Check the id with `post participant list`; a collected record can be brought back with `post participant restore <id>` first.",
        )
    })?;
    refuse_unless_adoptable(&old)?;
    Ok(old)
}

fn refuse_unless_adoptable(old: &Participant) -> AppResult<()> {
    if let Some(adopter) = &old.adopted_by {
        return Err(AppError::new(
            ErrorCode::InvalidArgument,
            format!(
                "cannot adopt participant '{}': it was already adopted by participant '{adopter}'{}",
                old.id,
                old.adopted_at
                    .as_deref()
                    .map(|at| format!(" at {at}"))
                    .unwrap_or_default()
            ),
            format!("An identity is adopted once. Adopt '{adopter}' instead if it has since ended, or bind with `post participant bind --new` for a fresh identity."),
        ));
    }
    if old.state(SystemTime::now()) == ParticipantState::Active {
        let name = old
            .display_name
            .as_deref()
            .map(|name| format!(", named '{name}'"))
            .unwrap_or_default();
        let workspace = old
            .workspace
            .as_deref()
            .map(|room| format!(" in workspace '{room}'"))
            .unwrap_or_default();
        let lease = old
            .last_seen
            .as_deref()
            .map(|seen| format!("last seen {seen}, lease {} h", old.lease_hours))
            .unwrap_or_else(|| "lease live".to_owned());
        return Err(AppError::new(
            ErrorCode::InvalidArgument,
            format!(
                "cannot adopt participant '{}': it is still active, held by its {} session{name}{workspace} ({lease})",
                old.id, old.harness
            ),
            format!(
                "Adoption needs the holder gone: it is not ended and its lease is live. The holder ends itself with `post participant end` (as `POST_PARTICIPANT={} post participant end`); retry once it is ended or its lease has lapsed.",
                old.id
            ),
        ));
    }
    Ok(())
}

/// Transfer `old_id`'s name, sigil and channels to `new` and record the
/// adoption on both records. The claim on the old record is written first,
/// under the participants lock, so two adopters cannot both win.
pub(crate) fn adopt(
    context: &Context,
    mut new: Participant,
    old_id: &str,
) -> AppResult<(Participant, Adopted)> {
    let at = format_rfc3339(SystemTime::now())?;
    let mut old = {
        let _lock = lock(context)?;
        let mut old = load(context, old_id)?.ok_or_else(|| {
            AppError::invalid_argument(format!(
                "cannot adopt participant '{old_id}': no such participant record"
            ))
        })?;
        if old.id == new.id {
            return Err(AppError::invalid_argument(
                "a participant cannot adopt itself",
            ));
        }
        refuse_unless_adoptable(&old)?;
        old.adopted_by = Some(new.id.clone());
        old.adopted_at = Some(at.clone());
        write_record(&old)?;
        old
    };
    match transfer(context, &old, &new) {
        Ok((name, pfp, channels)) => {
            let _lock = lock(context)?;
            new.display_name = old.display_name.clone();
            new.adopted_from = Some(old.id.clone());
            new.adopted_at = Some(at.clone());
            write_record(&new)?;
            Ok((
                new,
                Adopted {
                    from: old.id,
                    at,
                    name,
                    pfp,
                    channels,
                },
            ))
        }
        Err(error) => {
            // Release the claim so the adoption can be retried.
            let _lock = lock(context)?;
            old.adopted_by = None;
            old.adopted_at = None;
            let _ = write_record(&old);
            Err(error)
        }
    }
}

type Transferred = (Option<String>, Option<String>, Vec<String>);

fn transfer(context: &Context, old: &Participant, new: &Participant) -> AppResult<Transferred> {
    // Profile entry (display name and sigil) under the profiles lock.
    let (name, pfp) = {
        let _rooms = context.lock_rooms()?;
        let mut profiles = crate::profile::load_profiles(context)?;
        let held = profiles
            .get(&crate::profile::participant_key(&old.id))
            .cloned();
        match held {
            Some(profile) if profile.name.is_some() || profile.pfp.is_some() => {
                let carried = (profile.name.clone(), profile.pfp.clone());
                profiles.insert(crate::profile::participant_key(&new.id), profile);
                crate::profile::write_profiles(context, &profiles)?;
                carried
            }
            _ => (None, None),
        }
    };
    let channels = crate::channel_state::adopt_memberships(context, old, new)?;
    crate::cursor_state::ParticipantCursors::adopt_channels(context, old, new, &channels)?;
    Ok((name, pfp, channels))
}
