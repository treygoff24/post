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
    // A field NOT set on this call was preserved from disk and may be a
    // hand-edited plant; re-validate the merged entry so nothing invalid is
    // stored or carried into the announcement line below.
    if crate::profile::drop_invalid_fields(entry, &room, &rooms) {
        eprintln!(
            "post: warning: dropped an invalid stored profile field for '{room}' (hand-edited registry values never render)"
        );
    }
    let profile = entry.clone();

    // Enumerate the announcement targets BEFORE committing the registry:
    // if the channel listing fails, the whole command fails pre-commit, so
    // a retry still sees the change and still announces it (history stays
    // honest). Per-channel announcement failures after commit only warn.
    let targets = if name_changed || pfp_changed {
        member_channels(context, &room)?
    } else {
        Vec::new()
    };
    write_profiles(context, &profiles)?;

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
    let announced = announce(context, &room, &line, &targets);

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
    // Lock BEFORE resolving the acting room, same as `set`: resolving first
    // races a concurrent `rooms add` and can clear a stale room's profile.
    let _lock = context.lock_rooms()?;
    let rooms = context.load_rooms()?;
    let room = channel::acting_room(context, &rooms)?;
    let mut profiles = load_profiles(context)?;
    let existed = profiles.remove(&room).is_some();
    // Same pre-commit ordering as `set`: listing failure aborts before the
    // registry write so a retry still announces the change.
    let targets = if existed {
        member_channels(context, &room)?
    } else {
        Vec::new()
    };
    write_profiles(context, &profiles)?;
    let line = format!("=== {room} cleared their profile ===");
    let announced = announce(context, &room, &line, &targets);
    let profile = Profile::default();
    let output = ProfileOutput {
        ok: true,
        room: &room,
        profile: &profile,
        announced,
    };
    Ok(CommandResult::json(&output, pretty)?.registration_committed())
}

/// Channels the room belongs to — the announcement targets, resolved before
/// the registry commit so listing failures fail the command pre-commit.
fn member_channels(context: &Context, room: &str) -> AppResult<Vec<String>> {
    Ok(channel::list_channels(context)?
        .into_iter()
        .filter(|summary| summary.members.contains_key(room))
        .map(|summary| summary.info.name)
        .collect())
}

/// Write the profile event into each target channel. Failures don't roll
/// back the already-committed registry; they surface as warnings (the
/// profile is already true, the announcement is courtesy).
fn announce(context: &Context, room: &str, line: &str, targets: &[String]) -> Vec<String> {
    let mut announced = Vec::new();
    for name in targets {
        let result = ChannelPaths::new(context, name).and_then(|paths| {
            channel::write_event(context, &paths, room, name, line, PROFILE_EVENT)
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
