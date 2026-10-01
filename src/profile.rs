//! Participant profiles: display names + emoji pfps. PRESENTATION ONLY —
//! identity is always the immutable participant id and reply address. Auth,
//! routing, blocks, cursors, and signed-message verification never consult a
//! profile. Profiles are stamped into message envelopes at send time so
//! history renders as-sent (renames never rewrite the transcript).
//!
//! Keying (2026-09-22): a profile belongs to ONE participant and is stored
//! under the typed key `participant:<id>`. Bare (workspace-keyed) entries are
//! the pre-2026-09-22 format; they were shared by every participant bound to
//! that workspace, which let a newly bound participant inherit another
//! participant's persona. Bare entries never stamp; `post doctor` reports
//! them (never auto-migrated: `participant::list` skips malformed records,
//! so "sole participant" cannot be proven), and `post profile set` by a
//! participant bound to that workspace retires the entry.

use crate::error::{AppError, AppResult, ErrorCode};
use crate::mailbox::{atomic_replace, shell_quote, Context};
use crate::model::RoomMap;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

use crate::mailbox::refused_profile_char;

pub(crate) const PROFILES_FILE: &str = "profiles.json";
pub(crate) const MAX_NAME_CHARS: usize = 32;
pub(crate) const PARTICIPANT_KEY_PREFIX: &str = "participant:";

/// Registry key for a participant's own profile.
pub(crate) fn participant_key(participant_id: &str) -> String {
    format!("{PARTICIPANT_KEY_PREFIX}{participant_id}")
}

/// The participant id behind a typed registry key, or None for a legacy
/// (workspace-keyed) entry.
pub(crate) fn participant_of_key(key: &str) -> Option<&str> {
    key.strip_prefix(PARTICIPANT_KEY_PREFIX)
}

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
pub(crate) fn skeleton(value: &str) -> String {
    value
        .nfkc()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Validate a display name for `own_room`. Rejects control characters,
/// over-long names, and names whose skeleton imitates the configured signed
/// owner (A0a Decision 4: legacy fallback reserves `trey`, feature-absent
/// reserves nothing) or any registered room id other than the caller's own.
/// `owner_room` is the resolved owner's room id — the caller decides how to
/// resolve it (profile set loads owner.json, per the Decision 3 matrix;
/// send-time stamping may only consult the room registry).
pub(crate) fn validate_display_name(
    name: &str,
    own_room: &str,
    rooms: &RoomMap,
    owner_room: Option<&str>,
) -> AppResult<()> {
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
    // The owner reservation mirrors the pre-A0a unconditional 'trey' check:
    // it deliberately excludes own_room, because the owner room's display
    // name must never be able to claim the owner namespace either.
    if let Some(owner_room) = owner_room {
        if skeleton(owner_room) == skel {
            return Err(invalid(
                &format!("display name imitates the signed owner '{owner_room}'"),
                name,
            ));
        }
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
/// among the participants that are ACTIVE now (`active` ids) plus registered
/// legacy rooms, so the sigil identifies among the agents actually present. An
/// ended or stale participant's entry does not lock its sigil forever (a later
/// session continuing the same lineage may take it back); a sigil is
/// presentation, not authority.
pub(crate) fn validate_pfp(
    pfp: &str,
    own_room: &str,
    profiles: &ProfileMap,
    rooms: &RoomMap,
    active: &std::collections::BTreeSet<String>,
) -> AppResult<()> {
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
    // Uniqueness counts participant-keyed entries and registered legacy
    // rooms: an unregistered (hand-edited) bare entry never stamps or
    // renders, so it must not squat a sigil either.
    for (key, profile) in profiles {
        let counts = counts_for_sigil_uniqueness(key, rooms, active);
        if key != own_room && counts && profile.pfp.as_deref() == Some(pfp) {
            return Err(pfp_taken(pfp, key));
        }
    }
    Ok(())
}

/// Whether the entry under `key` competes for sigil uniqueness right now: a
/// participant-keyed entry while that participant is `active`, a legacy
/// workspace-keyed entry while its room is registered. `profile set` refuses
/// on it and `profile list` reports it, so the two cannot disagree.
pub(crate) fn counts_for_sigil_uniqueness(
    key: &str,
    rooms: &RoomMap,
    active: &std::collections::BTreeSet<String>,
) -> bool {
    match participant_of_key(key) {
        Some(id) => active.contains(id),
        None => rooms.contains_key(key),
    }
}

/// A sigil is unique among the profiles that render now, so the refusal has to
/// name who holds it and what frees it. "Pick another emoji" is useless advice
/// at the case this rule actually meets: a continued lineage is a new
/// participant id, and while the previous participant's lease is unexpired the
/// continuation cannot take the lineage's sigil back. What frees a sigil is the
/// HOLDER's own action or its lease lapsing -- `post participant end` and
/// `post profile clear` act on the acting participant only, so this refusal
/// never tells the caller to run them against someone else, and never points at
/// editing profiles.json by hand.
fn pfp_taken(pfp: &str, holder_key: &str) -> AppError {
    // Quoted because this is embedded in a command the caller may run: a pfp is
    // one non-ASCII grapheme, so it is not ASCII shell syntax -- but quoting is
    // what makes that independence, rather than a property of today's
    // validator, the reason the suggestion is safe to paste.
    let sigil = shell_quote(pfp);
    let (holder, next) = match participant_of_key(holder_key) {
        Some(id) => (
            format!("participant:{id}"),
            format!(
                "Another participant holds this sigil while it is active, and a continued lineage does not inherit it (the same lineage continued is a new participant id). Pick a different emoji, or set the sigil once it is free with `post profile set --pfp {sigil}`: the holder releases it by running `post participant end` or `post profile clear` itself, and a lease that lapses stops it counting. Nothing you can run ends or clears another participant."
            ),
        ),
        None => (
            format!("registered room '{holder_key}'"),
            format!(
                "A legacy workspace-keyed entry for '{holder_key}' holds this sigil. It never stamps and retires itself. Pick a different emoji, or have the participant that replies as '{holder_key}' set the sigil for its own entry with `post profile set --pfp {sigil}` (its own key wins over the legacy one)."
            ),
        ),
    };
    AppError::new(
        ErrorCode::InvalidArgument,
        format!("profile value is invalid: pfp is already the sigil of {holder}"),
        next,
    )
    .input(pfp.escape_debug().to_string())
    .reason(format!("pfp is already the sigil of {holder}"))
}

/// `profile set --name` refusal: another participant that is live right now
/// already holds the name (compared ignoring case), the same shape as
/// [`pfp_taken`]. The holder is named by id; it is not told anything.
pub(crate) fn name_taken(name: &str, holder_id: &str) -> AppError {
    AppError::new(
        ErrorCode::InvalidArgument,
        format!(
            "profile value is invalid: name is already the name of participant:{holder_id}"
        ),
        "Another participant holds this name while it is live (a live watch and activity within 10 minutes), because a name is how peers address each other. Pick a different name, or set it once it is free with `post profile set --name '<name>'`: the holder releases it by running `post participant end` or `post profile clear` itself, and it stops counting once that participant is no longer live. Nothing you can run ends or clears another participant.",
    )
    .input(name.escape_debug().to_string())
    .reason(format!("name is already the name of participant:{holder_id}"))
}

/// Resolve the profile to stamp for the acting `participant_id` (replying as
/// `room`) at send time, re-validating the registry values: a hand-edited
/// profiles.json must be inert as an injection or imitation path, so invalid
/// fields are dropped (never stamped) rather than trusted because they are on
/// disk. Only the participant's own `participant:<id>` entry stamps; a legacy
/// workspace-keyed entry never does (it was shared by every participant in
/// the workspace, which is the identity collision this rule closes).
/// Cross-entry pfp uniqueness is deliberately not re-checked here: a
/// duplicated sigil is cosmetic, not an injection.
pub(crate) fn stamp_for(
    context: &Context,
    participant_id: &str,
    room: &str,
    rooms: &RoomMap,
) -> Profile {
    let Ok(mut profiles) = load_profiles(context) else {
        return Profile::default();
    };
    let mut profile = profiles
        .remove(&participant_key(participant_id))
        .unwrap_or_default();
    // Send-time stamping is transport and never loads the trust anchor (A0a
    // Decision 3 column). The room registry already reserves a configured
    // owner's room id (registration is enforced at owner load), so the only
    // reservation derivable here without loading owner.json is the legacy
    // one, exactly when it applies: a registered 'trey' room. Feature-absent
    // (no owner.json, no trey room) reserves nothing, per Decision 4.
    let owner_hint = rooms
        .contains_key(crate::mailbox::LEGACY_OWNER_ROOM)
        .then_some(crate::mailbox::LEGACY_OWNER_ROOM);
    drop_invalid_fields(&mut profile, room, rooms, owner_hint);
    profile
}

/// Drop any field that no longer validates (a hand-edited profiles.json must
/// be inert everywhere, not just at stamp time). Shared by `stamp_for` and
/// `profile set`, which preserves the unset field of an existing entry and
/// must not carry a planted value into announcements. Cross-room pfp
/// uniqueness is deliberately not re-checked: a duplicated sigil is
/// cosmetic, not an injection. Returns true if anything was dropped.
pub(crate) fn drop_invalid_fields(
    profile: &mut Profile,
    room: &str,
    rooms: &RoomMap,
    owner_room: Option<&str>,
) -> bool {
    let mut dropped = false;
    if let Some(name) = &profile.name {
        if validate_display_name(name, room, rooms, owner_room).is_err() {
            profile.name = None;
            dropped = true;
        }
    }
    if let Some(pfp) = &profile.pfp {
        let mut graphemes = pfp.graphemes(true);
        let single = graphemes.next().is_some() && graphemes.next().is_none();
        if !single || pfp.is_ascii() || pfp.chars().any(refused_profile_char) {
            profile.pfp = None;
            dropped = true;
        }
    }
    dropped
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
        validate_display_name("Lantern 🏮", "pact", &rooms, Some("trey")).expect("plain name ok");
        // Own room id is fine as a display name.
        validate_display_name("pact", "pact", &rooms, Some("trey")).expect("own id ok");
        assert!(validate_display_name("T r e y", "pact", &rooms, Some("trey")).is_err());
        assert!(validate_display_name("Wade Discovery", "pact", &rooms, Some("trey")).is_err());
        assert!(validate_display_name("evil\nname", "pact", &rooms, Some("trey")).is_err());
        assert!(validate_display_name("   ", "pact", &rooms, Some("trey")).is_err());
        assert!(validate_display_name(&"x".repeat(33), "pact", &rooms, Some("trey")).is_err());
        assert!(
            validate_display_name("🏮🏮", "pact", &rooms, Some("trey")).is_err(),
            "no letters"
        );
        // Bidi controls are Cf, not Cc — must be refused explicitly (wade F1).
        assert!(validate_display_name("evil\u{202E}name", "pact", &rooms, Some("trey")).is_err());
        assert!(validate_display_name("evil\u{2066}name", "pact", &rooms, Some("trey")).is_err());
        assert!(validate_display_name("evil\u{2028}name", "pact", &rooms, Some("trey")).is_err());
        // NFKC collapses fullwidth forms into the skeleton (wade F2).
        assert!(validate_display_name("ｔｒｅｙ", "pact", &rooms, Some("trey")).is_err());
        // A0a Decision 4: the reservation follows the CONFIGURED owner.
        // (The rooms helper is shadowed by the binding above, so the map is
        // built inline.)
        let mara = [("mara", "/tmp"), ("pact", "/tmp")]
            .into_iter()
            .map(|(name, path)| (name.to_owned(), path.to_owned()))
            .collect();
        assert!(
            validate_display_name("Mara", "pact", &mara, Some("mara")).is_err(),
            "configured owner's id is reserved"
        );
        assert!(
            validate_display_name("T r e y", "pact", &mara, Some("mara")).is_ok(),
            "trey is neither the owner nor a registered room under this config"
        );
        assert!(
            validate_display_name("Mara", "mara", &mara, Some("mara")).is_err(),
            "owner reservation applies even inside the owner room (mirrors the legacy trey rule)"
        );
        // Feature-absent (no owner.json, trey unregistered): no reservation.
        assert!(
            validate_display_name("T r e y", "pact", &rooms, None).is_ok(),
            "no owner configured: trey is a free display name"
        );
    }

    #[test]
    fn pfp_rules() {
        let room_map = rooms(&["pact", "atlasos"]);
        let mut profiles = ProfileMap::new();
        profiles.insert(
            "atlasos".to_owned(),
            Profile {
                name: None,
                pfp: Some("🐋".to_owned()),
            },
        );
        // A hand-edited entry for an unregistered room must not squat a
        // sigil (Sol re-gate MEDIUM).
        profiles.insert(
            "ghost".to_owned(),
            Profile {
                name: None,
                pfp: Some("👻".to_owned()),
            },
        );
        let active: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        validate_pfp("⚖️", "pact", &profiles, &room_map, &active)
            .expect("VS16 emoji is one grapheme");
        validate_pfp("👩‍🚀", "pact", &profiles, &room_map, &active)
            .expect("ZWJ emoji is one grapheme");
        validate_pfp("👻", "pact", &profiles, &room_map, &active)
            .expect("unregistered entry does not reserve a sigil");
        assert!(
            validate_pfp("🏮🐋", "pact", &profiles, &room_map, &active).is_err(),
            "two emoji"
        );
        assert!(
            validate_pfp("x", "pact", &profiles, &room_map, &active).is_err(),
            "ascii"
        );
        assert!(
            validate_pfp("", "pact", &profiles, &room_map, &active).is_err(),
            "empty"
        );
        let taken =
            validate_pfp("🐋", "pact", &profiles, &room_map, &active).expect_err("taken sigil");
        // The refusal names the holder and the action that takes the sigil --
        // and it must stop there: neither ending a participant nor editing
        // profiles.json is something the caller can do to someone else.
        assert!(
            taken.message.contains("atlasos"),
            "the refusal must name the holder: {}",
            taken.message
        );
        assert!(
            taken.suggested_fix.contains("post profile set --pfp '🐋'"),
            "the refusal must name the action that takes the sigil: {}",
            taken.suggested_fix
        );
        assert!(
            !taken.suggested_fix.contains("participant end atlasos")
                && !taken.suggested_fix.contains("profiles.json"),
            "the refusal must not hand the caller a command that acts on another participant or a hand edit: {}",
            taken.suggested_fix
        );
        // The same rule for a live participant holder: `post participant end`
        // and `post profile clear` are the holder's own verbs (neither takes an
        // id), so naming the holder and its own action is the whole remedy.
        let holder = "fire";
        let mut live = ProfileMap::new();
        live.insert(
            format!("participant:{holder}"),
            Profile {
                name: None,
                pfp: Some("🔥".to_owned()),
            },
        );
        let active: std::collections::BTreeSet<String> = [holder.to_owned()].into_iter().collect();
        let held =
            validate_pfp("🔥", "pact", &live, &room_map, &active).expect_err("held by a live peer");
        assert!(
            held.message.contains("participant:fire"),
            "the refusal must name the live holder: {}",
            held.message
        );
        assert!(
            held.suggested_fix.contains("post profile set --pfp '🔥'")
                && !held.suggested_fix.contains("participant end fire")
                && !held.suggested_fix.contains("profiles.json"),
            "the refusal must not tell the caller to end another participant: {}",
            held.suggested_fix
        );
        validate_pfp("🐋", "atlasos", &profiles, &room_map, &active)
            .expect("re-setting own sigil ok");
        assert!(
            validate_pfp("\u{202E}", "pact", &profiles, &room_map, &active).is_err(),
            "bidi pfp"
        );
        // U+2028 is a single non-ASCII grapheme — the Grok CRITICAL.
        assert!(
            validate_pfp("\u{2028}", "pact", &profiles, &room_map, &active).is_err(),
            "line-separator pfp"
        );
        assert!(
            validate_display_name("evil\u{061C}name", "pact", &rooms(&["pact"]), Some("trey"))
                .is_err(),
            "ALM (U+061C) refused"
        );
    }

    #[test]
    fn drop_invalid_fields_catches_planted_pfp_preserved_by_set() {
        // The Sol final-gate HIGH: `profile set --name New` used to preserve
        // a hand-edited pfp unvalidated, letting U+2028 ride into the
        // channel announcement body.
        let rooms = rooms(&["pact"]);
        let mut profile = Profile {
            name: Some("Lantern".to_owned()),
            pfp: Some("\u{2028}".to_owned()),
        };
        assert!(drop_invalid_fields(
            &mut profile,
            "pact",
            &rooms,
            Some("trey")
        ));
        assert_eq!(profile.name.as_deref(), Some("Lantern"));
        assert_eq!(profile.pfp, None, "planted pfp must drop");
        assert!(
            !drop_invalid_fields(&mut profile, "pact", &rooms, Some("trey")),
            "clean profile drops nothing"
        );
    }

    #[test]
    fn stamp_for_uses_only_the_participants_own_entry_and_drops_invalid_values() {
        use crate::mailbox::Context;
        use std::fs;
        let root = crate::test_support::test_root("profile-stamp");
        // trey is registered, so the LEGACY owner fallback applies (A0a
        // Decision 4: feature-absent reserves nothing; the legacy reservation
        // requires a registered trey room).
        fs::write(
            root.join("rooms.json"),
            r#"{"alpha": "/tmp", "trey": "/tmp"}"#,
        )
        .expect("rooms");
        // Hand-edited registry: an imitation name + two-emoji pfp under one
        // participant, a valid entry under another participant, and a legacy
        // workspace-keyed entry that must never stamp for anyone.
        fs::write(
            root.join(PROFILES_FILE),
            r#"{"participant:test-bad": {"name": "trey", "pfp": "🏮🐋"},
                "participant:test-good": {"name": "Lantern", "pfp": "🏮"},
                "alpha": {"name": "Shared Persona", "pfp": "👻"}}"#,
        )
        .expect("profiles");
        let context = Context {
            root: root.clone(),
            home: root.clone(),
        };
        let rooms: RoomMap = [
            ("alpha".to_owned(), "/tmp".to_owned()),
            ("trey".to_owned(), "/tmp".to_owned()),
        ]
        .into_iter()
        .collect();
        let bad = stamp_for(&context, "test-bad", "alpha", &rooms);
        assert_eq!(bad.name, None, "imitation name must not stamp");
        assert_eq!(bad.pfp, None, "two-emoji pfp must not stamp");
        let good = stamp_for(&context, "test-good", "alpha", &rooms);
        assert_eq!(good.name.as_deref(), Some("Lantern"));
        assert_eq!(good.pfp.as_deref(), Some("🏮"));
        // A participant with no entry of its own, bound to a workspace that
        // still has a legacy entry, gets NOTHING: the workspace persona is not
        // inherited (the 2026-09-22 identity collision).
        let fresh = stamp_for(&context, "test-fresh", "alpha", &rooms);
        assert_eq!(
            fresh,
            Profile::default(),
            "legacy workspace entry never stamps"
        );
        // Session-only participant (reply address == its id, unregistered)
        // stamps its own entry: registration of the room is not required.
        let solo = stamp_for(&context, "test-good", "test-good", &rooms);
        assert_eq!(solo.name.as_deref(), Some("Lantern"));
        crate::test_support::trash_test_root(&root);
    }
}
