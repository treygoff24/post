use crate::channel::{self, ChannelPaths, PROFILE_EVENT};
use crate::cli::{ProfileArgs, ProfileCommand, ProfileSetArgs, ProfileShowArgs};
use crate::command_result::CommandResult;
use crate::error::{AppError, AppResult, ErrorCode};
use crate::mailbox::Context;
use crate::profile::{
    load_profiles, participant_key, participant_of_key, validate_display_name, validate_pfp,
    write_profiles, Profile,
};
use serde::Serialize;

#[derive(Serialize)]
struct ProfileOutput<'a> {
    ok: bool,
    /// Shared reply address of the acting (or shown) participant.
    room: &'a str,
    /// Participant the profile belongs to; absent for a legacy workspace-keyed
    /// entry, which never stamps.
    #[serde(skip_serializing_if = "Option::is_none")]
    participant: Option<&'a str>,
    /// Registry key the entry lives under.
    key: &'a str,
    profile: &'a Profile,
    /// True for a legacy workspace-keyed entry (shown only; never stamped).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    legacy: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    announced: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retired_legacy_entry: Option<String>,
}

pub(super) fn run(context: &Context, args: ProfileArgs, pretty: bool) -> AppResult<CommandResult> {
    match args.command {
        Some(ProfileCommand::Set(args)) => set(context, args, pretty),
        Some(ProfileCommand::Show(args)) => show(context, args, pretty),
        Some(ProfileCommand::Clear) => clear(context, pretty),
        None => show(context, ProfileShowArgs { room: None }, pretty),
    }
}

fn set(context: &Context, args: ProfileSetArgs, pretty: bool) -> AppResult<CommandResult> {
    if args.name.is_none() && args.pfp.is_none() {
        return Err(AppError::new(
            ErrorCode::InvalidArgument,
            "nothing to set: pass --name and/or --pfp",
            "Retry with `post profile set --name '<name>' --pfp '<emoji>'` (either flag alone is fine).",
        ));
    }
    // Rooms lock doubles as the profiles lock: both are rare, human-paced
    // registry mutations, and one lock cannot deadlock. Taken BEFORE the
    // rooms load so the imitation check can't race a concurrent
    // `rooms add` (validating against a stale map would let a name imitate
    // the just-registered room).
    let _lock = context.lock_rooms()?;
    let rooms = context.load_rooms()?;
    let (room, provenance) = channel::acting_room(context, &rooms)?;
    // The profile belongs to the acting PARTICIPANT, never to the workspace:
    // two participants bound to one workspace must never share a persona.
    let actor = context.sender()?;
    let participant_id = actor.participant.id.clone();
    let key = participant_key(&participant_id);
    // Decision 3 matrix: profile set loads the trust anchor, so the owner
    // reservation is checked against the SAME registry snapshot the imitation
    // check uses (and a broken owner.json fails this command closed).
    let owner_resolution = crate::mailbox::load_owner_with_rooms(context, &rooms)?;
    let owner_room = crate::mailbox::resolved_owner_room(&owner_resolution);
    let mut profiles = load_profiles(context)?;
    if let Some(name) = &args.name {
        validate_display_name(name, &room, &rooms, owner_room)?;
    }
    // A legacy workspace-keyed entry for the actor's own workspace is retired
    // by this set (it was shared by everyone in the workspace, which is the
    // collision this keying closes). Retire it BEFORE the pfp uniqueness
    // check so a participant can keep the sigil the legacy entry held.
    // A session-only participant's pre-change entry was keyed by its bare id;
    // that is retired the same way.
    let retired_legacy_entry = actor
        .participant
        .workspace
        .as_deref()
        .or(Some(participant_id.as_str()))
        .filter(|legacy| profiles.contains_key(*legacy))
        .map(str::to_owned);
    if let Some(legacy) = &retired_legacy_entry {
        profiles.remove(legacy);
    }
    if let Some(pfp) = &args.pfp {
        let active: std::collections::BTreeSet<String> = crate::participant::list_active(context)?
            .into_iter()
            .map(|record| record.id)
            .collect();
        validate_pfp(pfp, &key, &profiles, &rooms, &active)?;
    }
    // Trim before store: untrimmed whitespace pads the gap before the
    // rendered (room) suffix (wade F3).
    let trimmed_name = args.name.map(|name| name.trim().to_owned());
    let entry = profiles.entry(key.clone()).or_default();
    let name_changed = trimmed_name.is_some() && entry.name != trimmed_name;
    let pfp_changed = args.pfp.is_some() && entry.pfp != args.pfp;
    if let Some(name) = trimmed_name {
        entry.name = Some(name);
    }
    if let Some(pfp) = args.pfp {
        entry.pfp = Some(pfp);
    }
    // A field NOT set on this call was preserved from disk and may be a
    // hand-edited plant; re-validate the merged entry so nothing invalid is
    // stored or carried into the announcement line below.
    if crate::profile::drop_invalid_fields(entry, &room, &rooms, owner_room) {
        eprintln!(
            "post: warning: dropped an invalid stored profile field for '{key}' (hand-edited registry values never render)"
        );
    }
    let profile = entry.clone();

    // Enumerate the announcement targets BEFORE committing the registry:
    // if the channel listing fails, the whole command fails pre-commit, so
    // a retry still sees the change and still announces it (history stays
    // honest). Per-channel announcement failures after commit only warn.
    let targets = if name_changed || pfp_changed {
        crate::channel_state::effective_channels(context, &actor.participant)?
    } else {
        Vec::new()
    };
    write_profiles(context, &profiles)?;

    let line = match &profile.pfp {
        Some(pfp) => format!(
            "=== {room}: participant {participant_id} is now {} {pfp} ===",
            profile.name.as_deref().unwrap_or(&participant_id)
        ),
        None => format!(
            "=== {room}: participant {participant_id} is now {} ===",
            profile.name.as_deref().unwrap_or(&participant_id)
        ),
    };
    let announced = announce(context, &room, &line, &targets, provenance);

    let output = ProfileOutput {
        ok: true,
        room: &room,
        participant: Some(&participant_id),
        key: &key,
        profile: &profile,
        legacy: false,
        announced,
        retired_legacy_entry,
    };
    Ok(CommandResult::json(&output, pretty)?.registration_committed())
}

fn show(context: &Context, args: ProfileShowArgs, pretty: bool) -> AppResult<CommandResult> {
    let rooms = context.load_rooms()?;
    let profiles = load_profiles(context)?;
    // Target resolution: none = the acting participant; `participant:<id>`
    // or a bare participant id with an entry = that participant; any other
    // bare name = a legacy workspace-keyed entry (shown, never stamped).
    let (room, key, participant, legacy) = match args.room {
        None => {
            // Read-only and binding-free: an unbound shell still resolves its
            // cwd room and sees that room's legacy entry (never stamped).
            let (room, _) = channel::acting_room(context, &rooms)?;
            match crate::participant::resolve(context) {
                Ok(crate::participant::Resolved::Bound { participant, .. }) => {
                    let id = participant.id;
                    (room, participant_key(&id), Some(id), false)
                }
                _ => (room.clone(), room, None, true),
            }
        }
        Some(target) => {
            if let Some(id) = participant_of_key(&target) {
                let id = id.to_owned();
                let room = crate::participant::load(context, &id)?
                    .and_then(|record| record.workspace)
                    .unwrap_or_else(|| id.clone());
                (room, target.clone(), Some(id), false)
            } else if profiles.contains_key(&participant_key(&target)) {
                let room = crate::participant::load(context, &target)?
                    .and_then(|record| record.workspace)
                    .unwrap_or_else(|| target.clone());
                (room, participant_key(&target), Some(target), false)
            } else {
                (target.clone(), target, None, true)
            }
        }
    };
    let profile = profiles.get(&key).cloned().unwrap_or_default();
    let output = ProfileOutput {
        ok: true,
        room: &room,
        participant: participant.as_deref(),
        key: &key,
        profile: &profile,
        legacy,
        announced: Vec::new(),
        retired_legacy_entry: None,
    };
    CommandResult::json(&output, pretty)
}

fn clear(context: &Context, pretty: bool) -> AppResult<CommandResult> {
    // Lock BEFORE resolving the acting room, same as `set`: resolving first
    // races a concurrent `rooms add` and can clear a stale room's profile.
    let _lock = context.lock_rooms()?;
    let rooms = context.load_rooms()?;
    let (room, provenance) = channel::acting_room(context, &rooms)?;
    let actor = context.sender()?;
    let participant_id = actor.participant.id.clone();
    let key = participant_key(&participant_id);
    let mut profiles = load_profiles(context)?;
    // Only the actor's own entry goes; a legacy workspace entry (if any) is
    // left alone here (it never stamps) so clearing one participant cannot
    // change what another participant renders as.
    let existed = profiles.remove(&key).is_some();
    // Same pre-commit ordering as `set`: listing failure aborts before the
    // registry write so a retry still announces the change.
    let targets = if existed {
        crate::channel_state::effective_channels(context, &actor.participant)?
    } else {
        Vec::new()
    };
    write_profiles(context, &profiles)?;
    let line = format!("=== {room}: participant {participant_id} cleared their profile ===");
    let announced = announce(context, &room, &line, &targets, provenance);
    let profile = Profile::default();
    let output = ProfileOutput {
        ok: true,
        room: &room,
        participant: Some(&participant_id),
        key: &key,
        profile: &profile,
        legacy: false,
        announced,
        retired_legacy_entry: None,
    };
    Ok(CommandResult::json(&output, pretty)?.registration_committed())
}

/// Write the profile event into each target channel. Failures don't roll
/// back the already-committed registry; they surface as warnings (the
/// profile is already true, the announcement is courtesy).
fn announce(
    context: &Context,
    room: &str,
    line: &str,
    targets: &[String],
    provenance: crate::model::SenderProvenance,
) -> Vec<String> {
    let mut announced = Vec::new();
    for name in targets {
        let result = ChannelPaths::new(context, name).and_then(|paths| {
            channel::write_event(context, &paths, room, name, line, PROFILE_EVENT, provenance)
        });
        match result {
            Ok(_) => announced.push(name.clone()),
            Err(error) => eprintln!(
                "post: warning: could not announce profile change in #{name}: {}",
                error.message
            ),
        }
    }
    announced
}
