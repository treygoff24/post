//! Room profiles: display names + emoji pfps. PRESENTATION ONLY — identity
//! is always the immutable room id. Auth, routing, blocks, cursors, and
//! signed-message verification never consult a profile. Profiles are stamped
//! into message envelopes at send time so history renders as-sent (renames
//! never rewrite the transcript).

use crate::error::{AppError, AppResult, ErrorCode};
use crate::mailbox::{atomic_replace, Context};
use crate::model::RoomMap;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use unicode_segmentation::UnicodeSegmentation;

pub(crate) const PROFILES_FILE: &str = "profiles.json";
pub(crate) const MAX_NAME_CHARS: usize = 32;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pfp: Option<String>,
}

pub(crate) type ProfileMap = BTreeMap<String, Profile>;

pub(crate) fn load_profiles(context: &Context) -> AppResult<ProfileMap> {
    let path = context.root.join(PROFILES_FILE);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ProfileMap::new())
        }
        Err(error) => return Err(AppError::io("read profiles", &path, error)),
    };
    serde_json::from_slice(&bytes)
        .map_err(|error| AppError::config(&path, format!("invalid JSON object: {error}")))
}

pub(crate) fn write_profiles(context: &Context, profiles: &ProfileMap) -> AppResult<()> {
    let path = context.root.join(PROFILES_FILE);
    let mut bytes = serde_json::to_vec_pretty(profiles)
        .map_err(|error| AppError::io("serialize profiles", &path, error))?;
    bytes.push(b'\n');
    atomic_replace(&path, &bytes)
        .map_err(|error| AppError::io("atomically update profiles", &path, error))
}

/// Case/whitespace/punctuation-insensitive skeleton used for imitation
/// checks: lowercased alphanumerics only, so "T r e y", "trey_", and "TREY"
/// all collide with "trey".
fn skeleton(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Validate a display name for `own_room`. Rejects control characters,
/// over-long names, and names whose skeleton imitates "trey" or any
/// registered room id other than the caller's own.
pub(crate) fn validate_display_name(
    name: &str,
    own_room: &str,
    rooms: &RoomMap,
) -> AppResult<()> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(invalid("display name is empty", name));
    }
    if name.chars().any(char::is_control) {
        // A newline here could forge a whole message block in rendered
        // chat/watch output; refuse every control character outright.
        return Err(invalid("display name contains control characters", name));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(invalid(
            &format!("display name exceeds {MAX_NAME_CHARS} characters"),
            name,
        ));
    }
    let skel = skeleton(trimmed);
    if skel.is_empty() {
        return Err(invalid("display name has no letters or digits", name));
    }
    if skel == "trey" {
        return Err(invalid("display name imitates 'trey'", name));
    }
    for room in rooms.keys() {
        if room != own_room && skeleton(room) == skel {
            return Err(invalid(
                &format!("display name imitates existing room '{room}'"),
                name,
            ));
        }
    }
    Ok(())
}

/// Validate a pfp: exactly one grapheme cluster (so multi-codepoint emoji
/// like ⚖️ and 👩‍🚀 pass while two-emoji strings fail), no control characters,
/// not ASCII (an ASCII pfp like "[" would just be line noise), and unique
/// across rooms so the sigil actually identifies.
pub(crate) fn validate_pfp(
    pfp: &str,
    own_room: &str,
    profiles: &ProfileMap,
) -> AppResult<()> {
    let mut graphemes = pfp.graphemes(true);
    let first = graphemes.next();
    if first.is_none() || graphemes.next().is_some() {
        return Err(invalid("pfp must be exactly one emoji (one grapheme cluster)", pfp));
    }
    if pfp.chars().any(char::is_control) {
        return Err(invalid("pfp contains control characters", pfp));
    }
    if pfp.is_ascii() {
        return Err(invalid("pfp must be an emoji, not ASCII", pfp));
    }
    for (room, profile) in profiles {
        if room != own_room && profile.pfp.as_deref() == Some(pfp) {
            return Err(invalid(
                &format!("pfp is already the sigil of room '{room}'"),
                pfp,
            ));
        }
    }
    Ok(())
}

fn invalid(reason: &str, input: &str) -> AppError {
    AppError::new(
        ErrorCode::InvalidArgument,
        format!("profile value is invalid: {reason}"),
        "Pick a short name (<=32 chars, no control characters, no imitation of other identities) or a single unique emoji.",
    )
    .input(input.escape_debug().to_string())
    .reason(reason.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rooms(names: &[&str]) -> RoomMap {
        names
            .iter()
            .map(|n| ((*n).to_owned(), "/tmp".to_owned()))
            .collect()
    }

    #[test]
    fn display_name_rules() {
        let rooms = rooms(&["pact", "wade-discovery"]);
        validate_display_name("Lantern 🏮", "pact", &rooms).expect("plain name ok");
        // Own room id is fine as a display name.
        validate_display_name("pact", "pact", &rooms).expect("own id ok");
        assert!(validate_display_name("T r e y", "pact", &rooms).is_err());
        assert!(validate_display_name("Wade Discovery", "pact", &rooms).is_err());
        assert!(validate_display_name("evil\nname", "pact", &rooms).is_err());
        assert!(validate_display_name("   ", "pact", &rooms).is_err());
        assert!(validate_display_name(&"x".repeat(33), "pact", &rooms).is_err());
        assert!(validate_display_name("🏮🏮", "pact", &rooms).is_err(), "no letters");
    }

    #[test]
    fn pfp_rules() {
        let mut profiles = ProfileMap::new();
        profiles.insert(
            "atlasos".to_owned(),
            Profile { name: None, pfp: Some("🐋".to_owned()) },
        );
        validate_pfp("⚖️", "pact", &profiles).expect("VS16 emoji is one grapheme");
        validate_pfp("👩‍🚀", "pact", &profiles).expect("ZWJ emoji is one grapheme");
        assert!(validate_pfp("🏮🐋", "pact", &profiles).is_err(), "two emoji");
        assert!(validate_pfp("x", "pact", &profiles).is_err(), "ascii");
        assert!(validate_pfp("", "pact", &profiles).is_err(), "empty");
        assert!(validate_pfp("🐋", "pact", &profiles).is_err(), "taken sigil");
        validate_pfp("🐋", "atlasos", &profiles).expect("re-setting own sigil ok");
    }
}
