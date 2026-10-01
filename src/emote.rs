//! Silent records have their own suffix. Attention readers never call this module.
use crate::avatar;
use crate::channel::{self, ChannelPaths, SkippedFile};
use crate::error::{AppError, AppResult, ErrorCode};
use crate::mailbox::{exclusive_atomic_write, local_timestamp_micros, new_mail_id, Context};
use crate::model::{ChannelMessage, ParsedChannelMessage};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) fn files(directory: &Path) -> AppResult<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(directory).map_err(|e| AppError::io("list emotes", directory, e))? {
        let path = entry
            .map_err(|e| AppError::io("read emote entry", directory, e))?
            .path();
        if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("emote") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}
pub(crate) fn history_files(directory: &Path) -> AppResult<Vec<PathBuf>> {
    let mut files = channel::message_files(directory)?;
    files.extend(self::files(directory)?);
    files.sort_by(|a, b| {
        a.file_stem()
            .cmp(&b.file_stem())
            .then_with(|| b.extension().cmp(&a.extension()))
    });
    Ok(files)
}

pub(crate) fn decode(
    bytes: &[u8],
    stem: Option<&str>,
) -> Result<(ChannelMessage, Option<&'static str>), &'static str> {
    let Some(split) = bytes.windows(5).position(|b| b == b"\n---\n") else {
        return Err("envelope-separator");
    };
    if split > 4096 {
        return Err("envelope-header-too-large");
    }
    let v: Value = serde_json::from_slice(&bytes[..split]).map_err(|_| "envelope-json")?;
    let message: ChannelMessage = serde_json::from_value(v).map_err(|_| "envelope-fields")?;
    channel::validate_channel_message(Path::new("<emote>"), &message)
        .map_err(|_| "envelope-fields")?;
    if message.event.as_deref() != Some("emote") {
        return Err("envelope-event");
    }
    if stem.is_some_and(|s| s != message.id) {
        return Err("envelope-id-mismatch");
    }
    let rule = avatar::payload_rule(message.emote.as_ref());
    Ok((message, rule))
}
pub(crate) fn parse(path: &Path) -> AppResult<(ParsedChannelMessage, Option<&'static str>)> {
    let bytes = fs::read(path).map_err(|e| AppError::io("read emote", path, e))?;
    let (mut message, rule) = decode(&bytes, path.file_stem().and_then(|s| s.to_str()))
        .map_err(|rule| AppError::config(path, format!("unreadable_emote: {rule}")).reason(rule))?;
    // These planted fields and bodies cannot acquire attention or authority.
    message.mentions.clear();
    message.re = None;
    message.signature_ref = None;
    message.subject.clear();
    Ok((
        ParsedChannelMessage {
            message,
            body: String::new(),
        },
        rule,
    ))
}
pub(crate) fn diagnostic(path: &Path, code: &str, rule: &str) -> SkippedFile {
    SkippedFile {
        id: path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_owned(),
        reason: format!("{code}: {rule}"),
        channel: None,
    }
}
pub(crate) fn send(
    context: &Context,
    name: &str,
    emote_name: &str,
    at: Option<&str>,
) -> AppResult<ChannelMessage> {
    let rooms = context.load_rooms()?;
    let (room, provenance) = channel::acting_room(context, &rooms)?;
    let actor = context.sender()?;
    let paths = ChannelPaths::new(context, name)?;
    if !paths.exists() {
        return Err(channel::channel_not_found(
            context,
            name,
            channel::ChannelUse::Send,
        ));
    }
    let membership = crate::channel_state::ParticipantChannels::load(&actor.participant)?;
    if !membership.effective(context, &actor.participant, name)? {
        return Err(AppError::new(
            ErrorCode::NotAMember,
            "emote sender is not a channel member",
            format!(
                "Join with `post chat {} --join`.",
                crate::mailbox::shell_quote(name)
            ),
        ));
    }
    let (pack, _) = avatar::load(context, &actor.participant.id);
    let pack = pack.ok_or_else(|| {
        AppError::new(
            ErrorCode::InvalidArgument,
            "no valid stored avatar",
            "set an avatar first (post profile avatar set)",
        )
    })?;
    let (source, frozen) = avatar::freeze(&pack, emote_name).ok_or_else(|| {
        let mut names: Vec<String> = avatar::builtins()["emotes"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        if let Some(custom) = pack.get("emotes").and_then(Value::as_object) {
            names.extend(custom.keys().cloned());
        }
        names.sort();
        names.dedup();
        AppError::new(
            ErrorCode::InvalidArgument,
            "unknown emote",
            format!("Choose an emote: {}", names.join(", ")),
        )
        .matches(names)
    })?;
    let mut payload = json!({"name":emote_name,"source":source,"library":"builtin-1","steps":frozen["steps"],"frames":frozen["frames"]});
    if let Some(at) = at {
        let at = at
            .trim_start_matches('@')
            .strip_prefix("participant:")
            .unwrap_or(at.trim_start_matches('@'));
        let mut roster = crate::channel_state::ChannelRoster::new(context);
        let members = roster.effective_participants(name)?;
        let profiles = crate::profile::load_profiles(context)?;
        let found: Vec<_> = members
            .iter()
            .filter(|p| {
                p.id == at
                    || p.lineage.as_deref() == Some(at)
                    || profiles
                        .get(&crate::profile::participant_key(&p.id))
                        .and_then(|p| p.name.as_deref())
                        == Some(at)
            })
            .collect();
        match found.as_slice() {
            [p] => payload["at"] = p.id.clone().into(),
            [] => {
                return Err(AppError::new(
                    ErrorCode::InvalidArgument,
                    "emote target is not a channel member",
                    "Pass a member participant id or unique display name.",
                ))
            }
            _ => {
                return Err(AppError::new(
                    ErrorCode::AmbiguousId,
                    "emote target matches multiple members",
                    "Pass the full participant id.",
                ))
            }
        }
    }
    let profile = crate::profile::stamp_for(context, &actor.participant.id, &room, &rooms);
    let sender_address = crate::mailbox::declared_sender_address()?;
    for attempt in 0..256 {
        let (timestamp, sent) = local_timestamp_micros()?;
        let id = new_mail_id(&timestamp, attempt)?;
        if paths.messages.join(format!("{id}.msg")).exists() {
            continue;
        }
        let message = ChannelMessage {
            id: id.clone(),
            from: room.clone(),
            channel: name.to_owned(),
            subject: String::new(),
            sent,
            emote: Some(payload.clone()),
            event: Some("emote".into()),
            from_participant: Some(actor.participant.id.clone()),
            from_host: None,
            from_lineage: actor.lineage.clone(),
            address_kind: Some("channel".into()),
            display_name: profile.name.clone(),
            pfp: profile.pfp.clone(),
            re: None,
            mentions: Vec::new(),
            signature_ref: None,
            sender_address: sender_address.clone(),
            sender_provenance: Some(provenance.as_str().into()),
        };
        channel::validate_channel_message(Path::new("<generated emote>"), &message)?;
        let bytes = channel::encode_message(&message, "")?;
        if bytes.len() - 5 > 3072 {
            return Err(AppError::invalid_argument(
                "emote encoded header exceeds 3072 bytes",
            ));
        }
        let path = paths.messages.join(format!("{id}.emote"));
        match exclusive_atomic_write(&path, &bytes) {
            Ok(()) => return Ok(message),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(AppError::io("publish emote", &path, e)),
        }
    }
    Err(AppError::invalid_argument(
        "could not allocate a unique emote id",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn record_corpus() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/porch-contract/emotes/records");
        let mut count = 0;
        for kind in ["playable", "bubble", "omitted"] {
            for e in fs::read_dir(root.join(kind)).unwrap() {
                let path = e.unwrap().path();
                let filename = path.file_stem().unwrap().to_str().unwrap();
                let expected = filename.split("--").next().unwrap();
                let result = decode(&fs::read(&path).unwrap(), None);
                match kind {
                    "playable" => assert!(
                        matches!(result, Ok((_, None))),
                        "{}: {result:?}",
                        path.display()
                    ),
                    "bubble" => assert_eq!(result.unwrap().1, Some(expected), "{}", path.display()),
                    _ => assert_eq!(result.unwrap_err(), expected, "{}", path.display()),
                }
                count += 1;
            }
        }
        assert_eq!(count, 20);
    }
}
