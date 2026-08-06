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
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

use crate::mailbox::refused_profile_char;

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
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(ProfileMap::new()),
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
/// checks: NFKC-normalized (so fullwidth/compatibility forms collapse),
/// then lowercased alphanumerics only, so "T r e y", "trey_", "TREY", and
/// "ｔｒｅｙ" all collide with "trey". Non-NFKC homoglyphs (e.g. Cyrillic Т)
/// still pass; that residual risk is accepted because the immutable
/// (room-id) suffix is a hard invariant on every render path.
fn skeleton(value: &str) -> String {
    value
        .nfkc()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Validate a display name for `own_room`. Rejects control characters,
/// over-long names, and names whose skeleton imitates "trey" or any
/// registered room id other than the caller's own.
pub(crate) fn validate_display_name(name: &str, own_room: &str, rooms: &RoomMap) -> AppResult<()> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(invalid("display name is empty", name));
    }
    // One shared predicate with the parse-time checks and the renderer's
    // sanitizer: a newline or bidi control here could forge rendered lines
    // or visually reorder the load-bearing (room-id) suffix.
    if name.chars().any(refused_profile_char) {
        return Err(invalid(
            "display name contains control, bidi, or line-separator characters",
            name,
        ));
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
pub(crate) fn validate_pfp(pfp: &str, own_room: &str, profiles: &ProfileMap) -> AppResult<()> {
    let mut graphemes = pfp.graphemes(true);
    let first = graphemes.next();
    if first.is_none() || graphemes.next().is_some() {
        return Err(invalid(
            "pfp must be exactly one emoji (one grapheme cluster)",
            pfp,
        ));
    }
    if pfp.chars().any(refused_profile_char) {
        return Err(invalid(
            "pfp contains control, bidi, or line-separator characters",
            pfp,
        ));
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

/// Resolve the profile to stamp for `room` at send time, re-validating the
/// registry values: a hand-edited profiles.json must be inert as an
/// injection or imitation path, so invalid fields are dropped (never
/// stamped) rather than trusted because they are on disk. Unregistered
/// senders (free-form --from names) get no profile at all — profiles are a
/// per-room contract. Cross-room pfp uniqueness is deliberately not
/// re-checked here: a duplicated sigil is cosmetic, not an injection.
pub(crate) fn stamp_for(context: &Context, room: &str, rooms: &RoomMap) -> Profile {
    if !rooms.contains_key(room) {
        return Profile::default();
    }
    let Ok(mut profiles) = load_profiles(context) else {
        return Profile::default();
    };
    let mut profile = profiles.remove(room).unwrap_or_default();
    if let Some(name) = &profile.name {
        if validate_display_name(name, room, rooms).is_err() {
            profile.name = None;
        }
    }
    if let Some(pfp) = &profile.pfp {
        let mut graphemes = pfp.graphemes(true);
        let single = graphemes.next().is_some() && graphemes.next().is_none();
        if !single || pfp.is_ascii() || pfp.chars().any(refused_profile_char) {
            profile.pfp = None;
        }
    }
    profile
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
        assert!(
            validate_display_name("🏮🏮", "pact", &rooms).is_err(),
            "no letters"
        );
        // Bidi controls are Cf, not Cc — must be refused explicitly (wade F1).
        assert!(validate_display_name("evil\u{202E}name", "pact", &rooms).is_err());
        assert!(validate_display_name("evil\u{2066}name", "pact", &rooms).is_err());
        assert!(validate_display_name("evil\u{2028}name", "pact", &rooms).is_err());
        // NFKC collapses fullwidth forms into the skeleton (wade F2).
        assert!(validate_display_name("ｔｒｅｙ", "pact", &rooms).is_err());
    }

    #[test]
    fn pfp_rules() {
        let mut profiles = ProfileMap::new();
        profiles.insert(
            "atlasos".to_owned(),
            Profile {
                name: None,
                pfp: Some("🐋".to_owned()),
            },
        );
        validate_pfp("⚖️", "pact", &profiles).expect("VS16 emoji is one grapheme");
        validate_pfp("👩‍🚀", "pact", &profiles).expect("ZWJ emoji is one grapheme");
        assert!(
            validate_pfp("🏮🐋", "pact", &profiles).is_err(),
            "two emoji"
        );
        assert!(validate_pfp("x", "pact", &profiles).is_err(), "ascii");
        assert!(validate_pfp("", "pact", &profiles).is_err(), "empty");
        assert!(
            validate_pfp("🐋", "pact", &profiles).is_err(),
            "taken sigil"
        );
        validate_pfp("🐋", "atlasos", &profiles).expect("re-setting own sigil ok");
        assert!(
            validate_pfp("\u{202E}", "pact", &profiles).is_err(),
            "bidi pfp"
        );
        // U+2028 is a single non-ASCII grapheme — the Grok CRITICAL.
        assert!(
            validate_pfp("\u{2028}", "pact", &profiles).is_err(),
            "line-separator pfp"
        );
        assert!(
            validate_display_name("evil\u{061C}name", "pact", &rooms(&["pact"])).is_err(),
            "ALM (U+061C) refused"
        );
    }

    #[test]
    fn stamp_for_drops_invalid_registry_values_and_freeform_senders() {
        use crate::mailbox::Context;
        use std::fs;
        let root = crate::test_support::test_root("profile-stamp");
        fs::write(root.join("rooms.json"), r#"{"alpha": "/tmp"}"#).expect("rooms");
        // Hand-edited registry: imitation name, two-emoji pfp, plus one
        // valid entry for an unregistered sender.
        fs::write(
            root.join(PROFILES_FILE),
            r#"{"alpha": {"name": "trey", "pfp": "🏮🐋"}, "ghost": {"name": "Ghost", "pfp": "👻"}}"#,
        )
        .expect("profiles");
        let context = Context {
            root: root.clone(),
            home: root.clone(),
        };
        let rooms: RoomMap = [("alpha".to_owned(), "/tmp".to_owned())]
            .into_iter()
            .collect();
        let stamped = stamp_for(&context, "alpha", &rooms);
        assert_eq!(stamped.name, None, "imitation name must not stamp");
        assert_eq!(stamped.pfp, None, "two-emoji pfp must not stamp");
        let ghost = stamp_for(&context, "ghost", &rooms);
        assert_eq!(ghost, Profile::default(), "free-form sender never stamps");
        crate::test_support::trash_test_root(&root);
    }
}
