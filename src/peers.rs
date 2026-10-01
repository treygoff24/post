//! The peer directory: which participants are live right now, and how to
//! find one by profile name or repository. Presentation and addressing only:
//! a name or a repo is a convenience for picking a recipient, never identity
//! or authority. Whatever resolves is the participant's immutable id, and the
//! send then runs with unchanged routing, blocks, and auth.
//!
//! A participant is *live* when it has a live watch (a fresh `post watch`
//! heartbeat, or the doorbell supervisor has it armed) and its latest activity
//! stamp (the later of `runtime.updated` and `last_seen`) is within
//! [`LIVE_WINDOW`]. An ended participant is never live.

use crate::error::{AppError, AppResult, ErrorCode, RecipientCandidate};
use crate::mailbox::Context;
use crate::participant::{self, Participant};
use crate::presence;
use crate::profile;
use std::path::Path;
use std::time::{Duration, SystemTime};

/// How recent a participant's activity stamp must be for it to count as live.
pub(crate) const LIVE_WINDOW: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone)]
pub(crate) struct Peer {
    pub participant: Participant,
    /// The participant's own profile name and sigil (`post profile set`).
    pub name: Option<String>,
    pub pfp: Option<String>,
    /// Seconds since the participant's latest activity stamp.
    pub age_secs: u64,
}

impl Peer {
    pub(crate) fn repo(&self) -> Option<&str> {
        self.participant
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.repo.as_deref())
    }

    pub(crate) fn branch(&self) -> Option<&str> {
        self.participant
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.branch.as_deref())
    }

    pub(crate) fn title(&self) -> Option<&str> {
        self.participant
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.title.as_deref())
    }

    pub(crate) fn work_state(&self) -> Option<&str> {
        self.participant
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.state.as_deref())
    }
}

/// Every live participant, in id order. Damaged records are skipped without
/// a warning: a recipient lookup must not talk about unrelated records.
pub(crate) fn live_peers(context: &Context) -> AppResult<Vec<Peer>> {
    let (participants, _) = participant::list_with_skipped(context)?;
    if participants.is_empty() {
        return Ok(Vec::new());
    }
    let now = SystemTime::now();
    let doorbell = presence::read_doorbell(context);
    let presence_context = Context {
        root: context.root.join(participant::PARTICIPANTS_DIR),
        home: context.home.clone(),
    };
    let profiles = profile::load_profiles(context)?;
    let mut peers = Vec::new();
    for record in participants {
        let Some(age) = activity_age(&record, now) else {
            continue;
        };
        if age > LIVE_WINDOW {
            continue;
        }
        let watching = doorbell.participants.contains(&record.id)
            || presence::read_presence(&presence_context, &record.id)?.live_watch;
        if !watching {
            continue;
        }
        let entry = profiles.get(&profile::participant_key(&record.id));
        peers.push(Peer {
            name: entry
                .and_then(|entry| entry.name.as_deref())
                .map(|name| name.trim().to_owned())
                .filter(|name| !name.is_empty()),
            pfp: entry.and_then(|entry| entry.pfp.clone()),
            age_secs: age.as_secs(),
            participant: record,
        });
    }
    Ok(peers)
}

/// Time since the participant's latest activity stamp; `None` for an ended
/// participant or one with no readable stamp. A stamp in the future (clock
/// skew) counts as age zero.
pub(crate) fn activity_age(record: &Participant, now: SystemTime) -> Option<Duration> {
    if record.ended_at.is_some() {
        return None;
    }
    let stamps = [
        record.last_seen.as_deref(),
        record
            .runtime
            .as_ref()
            .map(|runtime| runtime.updated.as_str()),
    ];
    let latest = stamps
        .into_iter()
        .flatten()
        .filter_map(participant::parse_rfc3339)
        .max()?;
    Some(now.duration_since(latest).unwrap_or(Duration::ZERO))
}

fn same_name(left: &str, right: &str) -> bool {
    left.trim().to_lowercase() == right.trim().to_lowercase()
}

/// Live peers whose profile name is `name`, ignoring case.
pub(crate) fn by_name<'a>(peers: &'a [Peer], name: &str) -> Vec<&'a Peer> {
    peers
        .iter()
        .filter(|peer| {
            peer.name
                .as_deref()
                .is_some_and(|held| same_name(held, name))
        })
        .collect()
}

/// Whether `repo` (a stored absolute path) is the repository `selector`
/// names: an absolute path matches the stored path exactly (trailing slashes
/// ignored), anything else matches its basename, ignoring case.
pub(crate) fn repo_matches(repo: &str, selector: &str) -> bool {
    if selector.starts_with('/') {
        return trim_slashes(repo) == trim_slashes(selector);
    }
    Path::new(repo)
        .file_name()
        .and_then(|base| base.to_str())
        .is_some_and(|base| base.to_lowercase() == selector.to_lowercase())
}

fn trim_slashes(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() && path.starts_with('/') {
        "/"
    } else {
        trimmed
    }
}

/// Live peers in the repository `selector` names (basename or absolute path).
pub(crate) fn by_repo<'a>(peers: &'a [Peer], selector: &str) -> Vec<&'a Peer> {
    peers
        .iter()
        .filter(|peer| peer.repo().is_some_and(|repo| repo_matches(repo, selector)))
        .collect()
}

/// A stored value as one clean line fragment: control, bidi, and tab
/// characters never reach a terminal or an error message.
pub(crate) fn clean(value: &str) -> String {
    crate::output::sanitize_text_header(value).replace('\t', " ")
}

/// `repo@branch`, or whichever half is known.
fn location(peer: &Peer) -> Option<String> {
    let repo = peer
        .repo()
        .and_then(|repo| Path::new(repo).file_name())
        .and_then(|base| base.to_str())
        .map(clean)
        .filter(|base| !base.is_empty());
    let branch = peer.branch().map(clean);
    match (repo, branch) {
        (Some(repo), Some(branch)) => Some(format!("{repo}@{branch}")),
        (Some(repo), None) => Some(repo),
        (None, Some(branch)) => Some(branch),
        (None, None) => None,
    }
}

/// `5s`, `7m`, `3h`: the age as the one coarse unit it fits.
pub(crate) fn age_label(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        _ => format!("{}h", secs / 3600),
    }
}

/// The `post who --live` text line:
/// `<pfp> <name|id> · <repo>@<branch> · <title> · <state> · <age>`, each
/// unknown part left out.
pub(crate) fn live_line(peer: &Peer) -> String {
    let who = clean(peer.name.as_deref().unwrap_or(&peer.participant.id));
    let first = match peer.pfp.as_deref().map(clean).filter(|pfp| !pfp.is_empty()) {
        Some(pfp) => format!("{pfp} {who}"),
        None => who,
    };
    let mut parts = vec![first];
    parts.extend(location(peer));
    parts.extend(peer.title().map(clean));
    parts.extend(peer.work_state().map(clean));
    parts.push(age_label(peer.age_secs));
    parts.join(" · ")
}

/// One candidate as an ambiguity error lists it: id, name, repo, title, state.
fn candidate(peer: &Peer) -> String {
    let mut facts = Vec::new();
    facts.extend(peer.name.as_deref().map(clean));
    facts.extend(location(peer));
    facts.extend(peer.title().map(|title| format!("\"{}\"", clean(title))));
    facts.extend(peer.work_state().map(clean));
    if facts.is_empty() {
        clean(&peer.participant.id)
    } else {
        format!("{} ({})", clean(&peer.participant.id), facts.join(", "))
    }
}

/// The refusal for a name or repo that more than one live participant
/// answers to. Nothing is sent; the exact id always resolves.
pub(crate) fn ambiguous(raw: &str, matches: &[&Peer]) -> AppError {
    let listed = matches
        .iter()
        .map(|peer| candidate(peer))
        .collect::<Vec<_>>()
        .join("; ");
    AppError::new(
        ErrorCode::AmbiguousRecipient,
        format!(
            "'{}' matches {} live participants: {listed}",
            clean(raw),
            matches.len()
        ),
        "Nothing was sent. Retry with the exact participant id (`--to <id>`), or narrow it with `repo:<absolute-path>`; `post who --live` lists who is here.",
    )
    .input(raw)
    .reason("more than one live participant answers to this name or repo")
    .candidates(
        matches
            .iter()
            .map(|peer| RecipientCandidate {
                id: peer.participant.id.clone(),
                name: peer.name.as_deref().map(clean),
                repo: peer.repo().map(clean),
                title: peer.title().map(clean),
                state: peer.work_state().map(clean),
            })
            .collect(),
    )
}

/// The refusal for a name or repo no live participant answers to.
pub(crate) fn unknown(raw: &str, what: &str) -> AppError {
    AppError::new(
        ErrorCode::UnknownRecipient,
        format!("no live participant {what}"),
        "Nothing was sent. Run `post who --live` to see who is here, then retry with a name, `repo:<name>`, or an exact participant id. A participant counts as live while it runs `post watch` and has been active in the last 10 minutes.",
    )
    .input(raw)
    .reason("no live participant answers to this name or repo")
}

/// The live participant (other than `own_id`) already holding profile name
/// `name`, if any.
pub(crate) fn live_name_holder(
    context: &Context,
    name: &str,
    own_id: &str,
) -> AppResult<Option<Peer>> {
    Ok(live_peers(context)?.into_iter().find(|peer| {
        peer.participant.id != own_id
            && peer
                .name
                .as_deref()
                .is_some_and(|held| same_name(held, name))
    }))
}

/// Whether any participant (live or not) has declared profile name `name`.
pub(crate) fn known_name(context: &Context, name: &str) -> bool {
    profile::load_profiles(context).is_ok_and(|profiles| {
        profiles.iter().any(|(key, entry)| {
            profile::participant_of_key(key).is_some()
                && entry
                    .name
                    .as_deref()
                    .is_some_and(|held| same_name(held, name))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::participant::Runtime;
    use std::path::PathBuf;

    fn record(last_seen: Option<&str>, runtime: Option<Runtime>) -> Participant {
        Participant {
            version: 1,
            id: "test-peer".to_owned(),
            harness: "test".to_owned(),
            conversation_key_digest: "0".repeat(64),
            created: "2026-09-16 00:00:00 +0000".to_owned(),
            last_seen: last_seen.map(str::to_owned),
            lease_hours: 24,
            workspace: None,
            workspace_path: None,
            lineage: None,
            lineage_since: None,
            display_name: None,
            ephemeral: false,
            ended_at: None,
            runtime,
            dir: PathBuf::new(),
        }
    }

    fn peer(runtime: Runtime, name: Option<&str>, pfp: Option<&str>, age_secs: u64) -> Peer {
        Peer {
            participant: record(None, Some(runtime)),
            name: name.map(str::to_owned),
            pfp: pfp.map(str::to_owned),
            age_secs,
        }
    }

    fn at(rfc3339: &str) -> SystemTime {
        participant::parse_rfc3339(rfc3339).expect("test time")
    }

    #[test]
    fn activity_age_uses_the_later_stamp_and_ignores_ended_records() {
        let now = at("2026-10-01T12:00:00Z");
        let runtime = |updated: &str| Runtime {
            updated: updated.to_owned(),
            ..Runtime::default()
        };
        // last_seen alone.
        let age = activity_age(&record(Some("2026-10-01T11:55:00Z"), None), now);
        assert_eq!(age, Some(Duration::from_secs(300)));
        // The later of runtime.updated and last_seen wins, either way round.
        let newer_runtime = record(
            Some("2026-10-01T11:00:00Z"),
            Some(runtime("2026-10-01T11:58:00Z")),
        );
        assert_eq!(
            activity_age(&newer_runtime, now),
            Some(Duration::from_secs(120))
        );
        let newer_seen = record(
            Some("2026-10-01T11:59:00Z"),
            Some(runtime("2026-10-01T11:00:00Z")),
        );
        assert_eq!(
            activity_age(&newer_seen, now),
            Some(Duration::from_secs(60))
        );
        // A stamp in the future (clock skew) is age zero, not "never".
        let future = record(Some("2026-10-01T12:00:30Z"), None);
        assert_eq!(activity_age(&future, now), Some(Duration::ZERO));
        // No stamp, or an unparseable one, is not live.
        assert_eq!(activity_age(&record(None, None), now), None);
        assert_eq!(activity_age(&record(Some("junk"), None), now), None);
        // Ended is never live, whatever the stamp says.
        let mut ended = record(Some("2026-10-01T11:59:00Z"), None);
        ended.ended_at = Some("2026-10-01T11:59:00Z".to_owned());
        assert_eq!(activity_age(&ended, now), None);
    }

    #[test]
    fn the_live_window_is_ten_minutes_inclusive() {
        let now = at("2026-10-01T12:00:00Z");
        let inside = activity_age(&record(Some("2026-10-01T11:50:00Z"), None), now).unwrap();
        let outside = activity_age(&record(Some("2026-10-01T11:49:59Z"), None), now).unwrap();
        assert!(inside <= LIVE_WINDOW, "exactly 10 minutes is still live");
        assert!(outside > LIVE_WINDOW, "10 minutes and a second is not");
    }

    #[test]
    fn repo_selectors_match_a_basename_or_an_absolute_path() {
        assert!(repo_matches("/home/me/Code/post", "post"));
        assert!(
            repo_matches("/home/me/Code/post", "POST"),
            "basename ignores case"
        );
        assert!(repo_matches("/home/me/Code/post", "/home/me/Code/post"));
        assert!(repo_matches("/home/me/Code/post/", "/home/me/Code/post"));
        assert!(repo_matches("/home/me/Code/post", "/home/me/Code/post/"));
        assert!(
            !repo_matches("/home/me/Code/post", "/home/me/Code/POST"),
            "a path is exact"
        );
        assert!(!repo_matches("/home/me/Code/post", "/Code/post"));
        assert!(!repo_matches("/home/me/Code/post", "pos"));
        assert!(
            !repo_matches("/home/me/Code/post", "Code"),
            "not a parent directory"
        );
        assert!(!repo_matches("/home/me/Code/postal", "post"));
        assert!(repo_matches("/", "/"));
    }

    #[test]
    fn names_match_ignoring_case_and_surrounding_space() {
        let runtime = Runtime::default();
        let peers = vec![
            peer(runtime.clone(), Some("Treadle"), None, 1),
            peer(runtime.clone(), Some("Fern"), None, 1),
            peer(runtime, None, None, 1),
        ];
        assert_eq!(by_name(&peers, "treadle").len(), 1);
        assert_eq!(by_name(&peers, " TREADLE ").len(), 1);
        assert!(by_name(&peers, "Tread").is_empty());
        assert!(by_name(&peers, "").is_empty());
    }

    #[test]
    fn live_lines_omit_what_is_unknown() {
        let full = Runtime {
            repo: Some("/home/me/Code/porch".to_owned()),
            branch: Some("main".to_owned()),
            title: Some("Peer directory".to_owned()),
            state: Some("working".to_owned()),
            ..Runtime::default()
        };
        assert_eq!(
            live_line(&peer(full.clone(), Some("Treadle"), Some("🧵"), 42)),
            "🧵 Treadle · porch@main · Peer directory · working · 42s"
        );
        // No profile: the id stands in for the name and there is no sigil.
        assert_eq!(
            live_line(&peer(full, None, None, 125)),
            "test-peer · porch@main · Peer directory · working · 2m"
        );
        let only_repo = Runtime {
            repo: Some("/x/post".to_owned()),
            ..Runtime::default()
        };
        assert_eq!(
            live_line(&peer(only_repo, Some("Fern"), None, 7200)),
            "Fern · post · 2h"
        );
        assert_eq!(
            live_line(&peer(Runtime::default(), None, None, 0)),
            "test-peer · 0s"
        );
        // Stored text is untrusted: controls and bidi never reach the line.
        let hostile = Runtime {
            title: Some("a\u{1b}[31mb\u{202e}c\td".to_owned()),
            ..Runtime::default()
        };
        assert_eq!(
            live_line(&peer(hostile, Some("Fe\nrn"), None, 1)),
            "Fern · a[31mbc d · 1s"
        );
    }

    #[test]
    fn ambiguity_lists_every_candidate_and_the_ids_to_retry_with() {
        let one = peer(
            Runtime {
                repo: Some("/w/post".to_owned()),
                title: Some("First".to_owned()),
                state: Some("idle".to_owned()),
                ..Runtime::default()
            },
            Some("Fern"),
            None,
            1,
        );
        let mut two = peer(Runtime::default(), Some("Fern"), None, 1);
        two.participant.id = "test-other".to_owned();
        let error = ambiguous("fern", &[&one, &two]);
        assert_eq!(error.code, ErrorCode::AmbiguousRecipient);
        assert!(
            error
                .message
                .contains("test-peer (Fern, post, \"First\", idle)"),
            "{}",
            error.message
        );
        assert!(
            error.message.contains("test-other (Fern)"),
            "{}",
            error.message
        );
        let candidates = error.details.candidates.expect("candidates");
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].id, "test-peer");
        assert_eq!(candidates[0].repo.as_deref(), Some("/w/post"));
        assert_eq!(candidates[0].state.as_deref(), Some("idle"));
        assert_eq!(candidates[1].title, None);
    }
}
