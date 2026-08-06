use crate::channel::{self, ChannelPaths, PROFILE_EVENT};
use crate::cli::{ProfileArgs, ProfileCommand, ProfileSetArgs, ProfileShowArgs};
use crate::command_result::CommandResult;
use crate::error::{AppError, AppResult, ErrorCode};
use crate::mailbox::Context;
use crate::profile::{load_profiles, validate_display_name, validate_pfp, write_profiles, Profile};
use serde::Serialize;

#[derive(Serialize)]
struct ProfileOutput<'a> {
    ok: bool,
    room: &'a str,
    profile: &'a Profile,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    announced: Vec<String>,
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
    let room = channel::acting_room(context, &rooms)?;
    let mut profiles = load_profiles(context)?;
    if let Some(name) = &args.name {
        validate_display_name(name, &room, &rooms)?;
    }
    if let Some(pfp) = &args.pfp {
        validate_pfp(pfp, &room, &profiles)?;
    }
    // Trim before store: untrimmed whitespace pads the gap before the
    // rendered (room) suffix (wade F3).
    let trimmed_name = args.name.map(|name| name.trim().to_owned());
    let entry = profiles.entry(room.clone()).or_default();
    let name_changed = trimmed_name.is_some() && entry.name != trimmed_name;
    let pfp_changed = args.pfp.is_some() && entry.pfp != args.pfp;
    if let Some(name) = trimmed_name {
        entry.name = Some(name);
    }
    if let Some(pfp) = args.pfp {
        entry.pfp = Some(pfp);
    }
    let profile = entry.clone();
    write_profiles(context, &profiles)?;

    // Announce the rename in every channel this room belongs to, AFTER the
    // registry write so the announcement is stamped with the new profile.
    // Announcement failures don't roll back the profile; they surface as
    // warnings (the profile is already true, the announcement is courtesy).
    let mut announced = Vec::new();
    if name_changed || pfp_changed {
        let line = match &profile.pfp {
            Some(pfp) => format!(
                "=== {room} is now {} {pfp} ({room}) ===",
                profile.name.as_deref().unwrap_or(&room)
            ),
            None => format!(
                "=== {room} is now {} ({room}) ===",
                profile.name.as_deref().unwrap_or(&room)
            ),
        };
        for summary in channel::list_channels(context)? {
            if !summary.members.contains_key(&room) {
                continue;
            }
            let paths = ChannelPaths::new(context, &summary.info.name)?;
            match channel::write_event(
                context,
                &paths,
                &room,
                &summary.info.name,
                &line,
                PROFILE_EVENT,
            ) {
                Ok(_) => announced.push(summary.info.name),
                Err(error) => eprintln!(
                    "post: warning: could not announce rename in #{}: {}",
                    summary.info.name, error.message
                ),
            }
        }
    }

    let output = ProfileOutput {
        ok: true,
        room: &room,
        profile: &profile,
        announced,
    };
    Ok(CommandResult::json(&output, pretty)?.registration_committed())
}

fn show(context: &Context, args: ProfileShowArgs, pretty: bool) -> AppResult<CommandResult> {
    let rooms = context.load_rooms()?;
    let room = match args.room {
        Some(room) => room,
        None => channel::acting_room(context, &rooms)?,
    };
    let profiles = load_profiles(context)?;
    let profile = profiles.get(&room).cloned().unwrap_or_default();
    let output = ProfileOutput {
        ok: true,
        room: &room,
        profile: &profile,
        announced: Vec::new(),
    };
    CommandResult::json(&output, pretty)
}

fn clear(context: &Context, pretty: bool) -> AppResult<CommandResult> {
    let rooms = context.load_rooms()?;
    let room = channel::acting_room(context, &rooms)?;
    let _lock = context.lock_rooms()?;
    let mut profiles = load_profiles(context)?;
    profiles.remove(&room);
    write_profiles(context, &profiles)?;
    let profile = Profile::default();
    let output = ProfileOutput {
        ok: true,
        room: &room,
        profile: &profile,
        announced: Vec::new(),
    };
    Ok(CommandResult::json(&output, pretty)?.registration_committed())
}
