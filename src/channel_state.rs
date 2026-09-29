//! Compatibility read seam for the pre-Plan-B channel state.
//!
//! New state lives in cursor_state. This module keeps the names used by the
//! existing channel, watch, doctor, and consuming-read callers while the
//! consuming callers migrate to Delta directly.

use crate::cursor_state;
use crate::error::{AppError, AppResult, ErrorCode};
use crate::mailbox::{atomic_replace, Context};
use crate::model::BlockingRule;
use crate::participant::Participant;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;

const PARTICIPANT_CHANNELS_VERSION: u64 = 1;
const MEMBERSHIP_STARTS_VERSION: u64 = 1;

/// Per-channel membership-start watermarks for one participant, kept in a
/// sibling of `channels.json` rather than inside it: `channels.json` parses
/// with `deny_unknown_fields`, so an in-file field would make an older post
/// fail EVERY channel operation for the participant. A file older binaries
/// never open is the additive path — they ignore it entirely, and a newer
/// file an older feature build reads is likewise tolerated (no
/// `deny_unknown_fields` here either).
pub(crate) const MEMBERSHIP_STARTS_FILE: &str = "membership-starts.json";

/// `post chat --join --backlog`: the watermark that sorts before every real
/// message id, so the whole backlog reads as unread — the pre-join-from-now
/// behavior, kept as an explicit opt-in.
pub(crate) const BACKLOG_MEMBERSHIP_START: &str = "00000000-000000-000000";

/// `YYYYMMDD-HHMMSS-ffffff`: a channel message id without its hash suffix.
fn is_membership_start(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 22
        && bytes[..8].iter().all(u8::is_ascii_digit)
        && bytes[8] == b'-'
        && bytes[9..15].iter().all(u8::is_ascii_digit)
        && bytes[15] == b'-'
        && bytes[16..22].iter().all(u8::is_ascii_digit)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ParticipantChannels {
    joined: BTreeSet<String>,
    left: BTreeSet<String>,
    /// Explicit-join instant per channel, as a message-id watermark. A joined
    /// channel with no recorded instant (old-format state) falls back to the
    /// participant's `created` at read time; nothing is backfilled here.
    starts: BTreeMap<String, String>,
}

impl ParticipantChannels {
    pub(crate) fn load(participant: &Participant) -> AppResult<Self> {
        let (joined, left) = load_sets(participant)?;
        let starts = load_starts(participant)?;
        Ok(Self {
            joined,
            left,
            starts,
        })
    }

    /// The instant this participant's membership in `channel` began, as a
    /// message-id watermark: a message whose id sorts before it is history —
    /// never unread. `None` when the participant is not an effective member.
    /// An explicit join uses its recorded instant; a joined channel with no
    /// recorded instant (old-format state) and legacy workspace membership
    /// both fall back to the participant's own `created`.
    pub(crate) fn membership_start(
        &self,
        context: &Context,
        participant: &Participant,
        channel: &str,
    ) -> AppResult<Option<String>> {
        if !self.effective(context, participant, channel)? {
            return Ok(None);
        }
        let recorded = if self.joined.contains(channel) {
            self.starts.get(channel).cloned()
        } else {
            None
        };
        // A member whose `created` cannot be parsed keeps the pre-watermark
        // rule — everything unread — rather than an indeterminate floor that
        // would hide mail it has never seen.
        Ok(Some(
            recorded
                .or_else(|| participant.created_watermark())
                .unwrap_or_else(|| BACKLOG_MEMBERSHIP_START.to_owned()),
        ))
    }

    pub(crate) fn effective(
        &self,
        context: &Context,
        participant: &Participant,
        channel: &str,
    ) -> AppResult<bool> {
        self.effective_among(
            participant,
            channel,
            &mut LegacyMembers::new(context, channel),
        )
    }

    fn effective_among(
        &self,
        participant: &Participant,
        channel: &str,
        members: &mut LegacyMembers,
    ) -> AppResult<bool> {
        crate::channel::validate_channel_name(channel)?;
        if self.joined.contains(channel) {
            return Ok(true);
        }
        if self.left.contains(channel) {
            return Ok(false);
        }
        let Some(workspace) = participant.workspace.as_ref() else {
            return Ok(false);
        };
        Ok(members.get()?.contains_key(workspace))
    }

    pub(crate) fn joined_names(&self) -> &BTreeSet<String> {
        &self.joined
    }

    pub(crate) fn explicitly_left(&self, channel: &str) -> bool {
        self.left.contains(channel)
    }

    /// Test fixture: join with the `--backlog` watermark, so fixtures that seed
    /// fixed historical ids keep the membership semantics they were written
    /// for. Join-from-now is exercised through `join_at` and the CLI.
    #[cfg(test)]
    pub(crate) fn join(
        context: &Context,
        participant: &Participant,
        channel: &str,
    ) -> AppResult<bool> {
        Self::join_at(context, participant, channel, BACKLOG_MEMBERSHIP_START)
    }

    /// Join `channel` recording `start` (a `YYYYMMDD-HHMMSS-ffffff` id
    /// watermark) as the membership start. `BACKLOG_MEMBERSHIP_START` is the
    /// `--backlog` spelling: it sorts before every real message, so the whole
    /// history stays unread.
    pub(crate) fn join_at(
        context: &Context,
        participant: &Participant,
        channel: &str,
        start: &str,
    ) -> AppResult<bool> {
        crate::channel::validate_channel_name(channel)?;
        if !is_membership_start(start) {
            return Err(crate::error::AppError::invalid_argument(format!(
                "membership start '{start}' is not a channel-id watermark"
            )));
        }
        mutate(context, participant, |state| {
            state.left.remove(channel);
            let inserted = state.joined.insert(channel.to_owned());
            if inserted {
                state.starts.insert(channel.to_owned(), start.to_owned());
            }
            Ok(inserted)
        })
    }

    #[allow(dead_code)] // CLI --leave wiring is owned by the integration surface lane.
    pub(crate) fn leave(
        context: &Context,
        participant: &Participant,
        channel: &str,
    ) -> AppResult<bool> {
        crate::channel::validate_channel_name(channel)?;
        mutate(context, participant, |state| {
            let was_effective = state.effective(context, participant, channel)?;
            state.joined.remove(channel);
            // The membership start resets with the membership: a rejoin
            // records its own instant rather than inheriting this one.
            state.starts.remove(channel);
            state.left.insert(channel.to_owned());
            Ok(was_effective)
        })
    }
}

pub(crate) fn effective_channels(
    context: &Context,
    participant: &Participant,
) -> AppResult<Vec<String>> {
    let state = ParticipantChannels::load(participant)?;
    let mut names = state.joined_names().clone();
    let directory = context.root.join(crate::channel::CHANNELS_DIR);
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(names.into_iter().collect())
        }
        Err(error) => {
            return Err(crate::error::AppError::io(
                "list channels",
                &directory,
                error,
            ))
        }
    };
    for entry in entries {
        let entry = entry
            .map_err(|error| crate::error::AppError::io("read channel entry", &directory, error))?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let paths = match crate::channel::ChannelPaths::new(context, &name) {
            Ok(paths) if paths.exists() => paths,
            _ => continue,
        };
        if let Err(error) = paths.load_info() {
            eprintln!(
                "post: warning: skipped channel {:?} with invalid channel info: {:?}",
                name, error.message
            );
            continue;
        }
        if state.explicitly_left(&name) {
            names.remove(&name);
            continue;
        }
        if state.joined_names().contains(&name) {
            continue;
        }
        if let Some(workspace) = participant.workspace.as_ref() {
            match paths.load_members() {
                Ok(members) if members.contains_key(workspace) => {
                    names.insert(name);
                }
                Ok(_) => {}
                Err(error) => eprintln!(
                    "post: warning: skipped channel with invalid legacy membership: {}",
                    error.message
                ),
            }
        }
    }
    Ok(names.into_iter().collect())
}

pub(crate) fn participants_for_join_validation(
    context: &Context,
    channel: &str,
    actor: &Participant,
    actor_address: &str,
    blocked: &[BlockingRule],
) -> AppResult<Vec<Participant>> {
    let evidence = join_evidence(context, channel)?;
    let legacy_members = crate::channel::ChannelPaths::new(context, channel)?.load_members()?;
    let root = context.root.join(crate::participant::PARTICIPANTS_DIR);
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(crate::error::AppError::io(
                "list participants for channel admission",
                &root,
                error,
            ))
        }
    };
    let mut participants = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            crate::error::AppError::io("read participant admission entry", &root, error)
        })?;
        let file_type = entry.file_type().map_err(|error| {
            crate::error::AppError::io("inspect participant admission entry", &entry.path(), error)
        })?;
        if !file_type.is_dir() || entry.file_name() == "by-session" {
            continue;
        }
        let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
            eprintln!(
                "post: warning: skipped non-UTF-8 participant directory during channel admission"
            );
            continue;
        };
        let participant = match crate::participant::load(context, &id) {
            Ok(Some(participant)) => participant,
            Ok(None) => {
                let participant_dir = entry.path();
                if invalid_record_could_block(evidence.get(&id), actor, actor_address, blocked) {
                    return Err(invalid_join_participant_state(
                        &id,
                        &participant_dir.join("participant.json"),
                        format!(
                            "participant record is missing, so membership in channel '{channel}' cannot be validated against a blocked route"
                        ),
                        "for example, from a backup or by re-running `post participant bind` as that participant if the identity is recoverable",
                    ));
                }
                eprintln!(
                    "post: warning: skipped participant {id:?} with missing participant.json"
                );
                continue;
            }
            Err(error) if error.code == crate::error::ErrorCode::ConfigInvalid => {
                let participant_dir = entry.path();
                if invalid_record_could_block(evidence.get(&id), actor, actor_address, blocked) {
                    return Err(invalid_join_participant_state(
                        &id,
                        &participant_dir.join("participant.json"),
                        format!(
                            "participant record cannot be validated for possible membership in channel '{channel}': {}",
                            error.message
                        ),
                        "for example, from a backup or by re-running `post participant bind` as that participant if the identity is recoverable",
                    ));
                }
                eprintln!(
                    "post: warning: skipped corrupt participant {:?}: {:?}",
                    id, error.message
                );
                continue;
            }
            Err(error) => return Err(error),
        };
        let state = match ParticipantChannels::load(&participant) {
            Ok(state) => state,
            Err(error) if error.code == crate::error::ErrorCode::ConfigInvalid => {
                let could_be_member = evidence.contains_key(&participant.id)
                    || participant
                        .workspace
                        .as_ref()
                        .is_some_and(|workspace| legacy_members.contains_key(workspace));
                let candidate_address = participant.workspace.as_deref().unwrap_or(&participant.id);
                if could_be_member
                    && has_blocked_pair(
                        actor_address,
                        &actor.id,
                        candidate_address,
                        &participant.id,
                        blocked,
                    )
                {
                    return Err(invalid_join_participant_state(
                        &participant.id,
                        &participant.dir.join("channels.json"),
                        format!(
                            "participant may belong to channel '{channel}', but its membership record is unreadable and a blocked route could apply: {}",
                            error.message
                        ),
                        "for example, from a backup",
                    ));
                }
                eprintln!(
                    "post: warning: skipped invalid participant channels {:?}: {:?}",
                    participant.id, error.message
                );
                continue;
            }
            Err(error) => return Err(error),
        };
        if state.effective(context, &participant, channel)? {
            participants.push(participant);
        }
    }
    participants.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(participants)
}

/// A channel's legacy workspace membership (`members.json`), read on first
/// need and reused for every participant asked about that channel.
struct LegacyMembers<'a> {
    context: &'a Context,
    channel: &'a str,
    members: Option<crate::channel::MemberMap>,
}

impl<'a> LegacyMembers<'a> {
    fn new(context: &'a Context, channel: &'a str) -> Self {
        Self {
            context,
            channel,
            members: None,
        }
    }

    fn get(&mut self) -> AppResult<&crate::channel::MemberMap> {
        Ok(match &mut self.members {
            Some(members) => members,
            empty => empty.insert(
                crate::channel::ChannelPaths::new(self.context, self.channel)?.load_members()?,
            ),
        })
    }
}

/// Every participant and its channel state, read once (on first use) for a
/// caller that asks `effective_participants` about many channels. `post
/// channels` asked per channel, rereading every participant record, its
/// channel state, and the channel's members.json once per participant: 44
/// channels at 4,139 participants took 1.6 s (post-gxz).
///
/// A participant whose membership file is invalid cannot be placed in any
/// channel, so the roster leaves it out and records it in `skipped` (after the
/// first `effective_participants`): a caller that prints a roster must print
/// those entries beside it, or the roster silently under-reports.
pub(crate) struct ChannelRoster<'a> {
    context: &'a Context,
    participants: Option<Vec<(Participant, ParticipantChannels)>>,
    skipped: Vec<crate::channel::SkippedFile>,
}

impl<'a> ChannelRoster<'a> {
    pub(crate) fn new(context: &'a Context) -> Self {
        Self {
            context,
            participants: None,
            skipped: Vec::new(),
        }
    }

    /// Members left out for an invalid membership file. Filled by the first
    /// `effective_participants` call.
    pub(crate) fn skipped(&self) -> &[crate::channel::SkippedFile] {
        &self.skipped
    }

    pub(crate) fn effective_participants(&mut self, channel: &str) -> AppResult<Vec<Participant>> {
        let context = self.context;
        let mut members = LegacyMembers::new(context, channel);
        let mut effective = Vec::new();
        for (participant, state) in self.load()? {
            if state.effective_among(participant, channel, &mut members)? {
                effective.push(participant.clone());
            }
        }
        effective.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(effective)
    }

    fn load(&mut self) -> AppResult<&[(Participant, ParticipantChannels)]> {
        if self.participants.is_none() {
            let (participants, skipped) = roster(self.context)?;
            self.participants = Some(participants);
            self.skipped = skipped;
        }
        Ok(self.participants.as_deref().unwrap_or_default())
    }
}

type Roster = (
    Vec<(Participant, ParticipantChannels)>,
    Vec<crate::channel::SkippedFile>,
);

fn roster(context: &Context) -> AppResult<Roster> {
    let mut participants = Vec::new();
    let mut skipped = Vec::new();
    for participant in crate::participant::list(context)? {
        let state = match ParticipantChannels::load(&participant) {
            Ok(state) => state,
            Err(error) if error.code == crate::error::ErrorCode::ConfigInvalid => {
                skipped.push(crate::channel::SkippedFile::member(&participant.id, &error));
                continue;
            }
            Err(error) => return Err(error),
        };
        participants.push((participant, state));
    }
    Ok((participants, skipped))
}

fn join_evidence(
    context: &Context,
    channel: &str,
) -> AppResult<BTreeMap<String, BTreeSet<String>>> {
    let paths = crate::channel::ChannelPaths::new(context, channel)?;
    if !paths.messages.is_dir() {
        return Ok(BTreeMap::new());
    }
    let mut evidence = BTreeMap::<String, BTreeSet<String>>::new();
    for path in crate::channel::message_files(&paths.messages)? {
        let Ok(parsed) = crate::channel::parse_channel_message(&path) else {
            continue;
        };
        if parsed.message.event.as_deref() != Some(crate::channel::JOIN_EVENT) {
            continue;
        }
        if let Some(participant) = parsed.message.from_participant {
            evidence
                .entry(participant)
                .or_default()
                .insert(parsed.message.from);
        }
    }
    Ok(evidence)
}

fn invalid_record_could_block(
    membership_evidence: Option<&BTreeSet<String>>,
    actor: &Participant,
    actor_address: &str,
    blocked: &[BlockingRule],
) -> bool {
    membership_evidence.is_some_and(|addresses| !addresses.is_empty())
        && blocked.iter().any(|rule| {
            rule.from == "*"
                || rule.to == "*"
                || rule.from == actor_address
                || rule.to == actor_address
                || rule.from == actor.id
                || rule.to == actor.id
        })
}

fn invalid_join_participant_state(
    participant_id: &str,
    state_path: &std::path::Path,
    reason: impl Into<String>,
    recovery_example: &str,
) -> AppError {
    let file_name = state_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("participant state file");
    let reason = format!(
        "participant '{participant_id}': {}; {file_name} path is '{}'",
        reason.into(),
        state_path.display()
    );
    AppError::new(
        ErrorCode::ConfigInvalid,
        format!("configuration '{}' is invalid: {reason}", state_path.display()),
        format!(
            "Restore or repair {file_name} at '{}' for participant '{}' ({recovery_example}), then retry.",
            state_path.display(), participant_id
        ),
    )
    .path(state_path.display().to_string())
    .reason(reason)
}

fn has_blocked_pair(
    actor_address: &str,
    actor_id: &str,
    candidate_address: &str,
    candidate_id: &str,
    blocked: &[BlockingRule],
) -> bool {
    blocked.iter().any(|rule| {
        rule.matches_route(actor_address, candidate_address)
            || rule.matches_route(candidate_address, actor_address)
            || rule.matches_route(actor_id, candidate_id)
            || rule.matches_route(candidate_id, actor_id)
    })
}

/// Reads and validates `channels.json` alone; the membership-start sibling
/// is `load_starts`. Doctor validates each file separately so a defect is
/// reported against the file that holds it.
fn load_sets(participant: &Participant) -> AppResult<(BTreeSet<String>, BTreeSet<String>)> {
    let path = participant.dir.join("channels.json");
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((BTreeSet::new(), BTreeSet::new()))
        }
        Err(error) => {
            return Err(crate::error::AppError::io(
                "read participant channels",
                &path,
                error,
            ))
        }
    };
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Stored {
        version: u64,
        joined: Vec<String>,
        left: Vec<String>,
    }
    let stored: Stored = serde_json::from_slice(&bytes).map_err(|error| {
        crate::error::AppError::config(&path, format!("invalid participant channels JSON: {error}"))
    })?;
    if stored.version != PARTICIPANT_CHANNELS_VERSION {
        return Err(crate::error::AppError::config(
            &path,
            format!(
                "unsupported participant channels version {}",
                stored.version
            ),
        ));
    }
    let joined = parse_names(&path, stored.joined)?;
    let left = parse_names(&path, stored.left)?;
    if joined.iter().any(|name| left.contains(name)) {
        return Err(crate::error::AppError::config(
            &path,
            "participant channels joined/left sets overlap",
        ));
    }
    Ok((joined, left))
}

/// Doctor: the participant's `channels.json` loads, or the error saying why not.
pub(crate) fn validate_channels_file(participant: &Participant) -> AppResult<()> {
    load_sets(participant).map(|_| ())
}

/// Doctor: the participant's `membership-starts.json` loads (shape, version,
/// channel names, watermark form), or the error saying why not.
pub(crate) fn validate_membership_starts_file(participant: &Participant) -> AppResult<()> {
    load_starts(participant).map(|_| ())
}

fn load_starts(participant: &Participant) -> AppResult<BTreeMap<String, String>> {
    let path = participant.dir.join(MEMBERSHIP_STARTS_FILE);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => {
            return Err(crate::error::AppError::io(
                "read membership starts",
                &path,
                error,
            ))
        }
    };
    // Tolerant reader: no `deny_unknown_fields`, so a newer post's fields
    // survive a downgrade. `starts` is defaulted so the first written form is
    // not the only one this build accepts.
    #[derive(serde::Deserialize)]
    struct Stored {
        version: u64,
        #[serde(default)]
        starts: BTreeMap<String, String>,
    }
    let stored: Stored = serde_json::from_slice(&bytes).map_err(|error| {
        crate::error::AppError::config(&path, format!("invalid membership starts JSON: {error}"))
    })?;
    if stored.version != MEMBERSHIP_STARTS_VERSION {
        return Err(crate::error::AppError::config(
            &path,
            format!("unsupported membership starts version {}", stored.version),
        ));
    }
    for (name, start) in &stored.starts {
        crate::channel::validate_channel_name(name).map_err(|error| {
            crate::error::AppError::config(
                &path,
                format!("invalid channel name '{name}': {}", error.message),
            )
        })?;
        if !is_membership_start(start) {
            return Err(crate::error::AppError::config(
                &path,
                format!("invalid membership start '{start}' for channel '{name}'"),
            ));
        }
    }
    Ok(stored.starts)
}

fn mutate<T>(
    context: &Context,
    participant: &Participant,
    change: impl FnOnce(&mut ParticipantChannels) -> AppResult<T>,
) -> AppResult<T> {
    let _lock = crate::participant::lock(context)?;
    fs::create_dir_all(&participant.dir).map_err(|error| {
        crate::error::AppError::io("create participant directory", &participant.dir, error)
    })?;
    let mut state = ParticipantChannels::load(participant)?;
    let before = state.clone();
    let result = change(&mut state)?;
    for file in write_order(&before, &state) {
        match file {
            StateFile::Starts => write_starts(participant, &state.starts)?,
            StateFile::Channels => write_channels(participant, &state)?,
        }
    }
    Ok(result)
}

/// The two files a membership mutation may rewrite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StateFile {
    Starts,
    Channels,
}

/// Which files change, in crash-safe order. A mutation that adds or changes a
/// start (join) commits the watermark first: an orphan start is harmless, but
/// a join committed without its start would fall back to `created` and flood
/// the backlog into unread. A mutation that only removes starts (leave)
/// commits `channels.json` first, for the same reason inverted: removing the
/// start before the leave lands would leave a joined channel with no start.
fn write_order(before: &ParticipantChannels, after: &ParticipantChannels) -> Vec<StateFile> {
    let starts_changed = before.starts != after.starts;
    let channels_changed = before.joined != after.joined || before.left != after.left;
    let adds_start = after
        .starts
        .iter()
        .any(|(channel, start)| before.starts.get(channel) != Some(start));
    let mut order = Vec::new();
    if starts_changed && adds_start {
        order.push(StateFile::Starts);
    }
    if channels_changed {
        order.push(StateFile::Channels);
    }
    if starts_changed && !adds_start {
        order.push(StateFile::Starts);
    }
    order
}

fn write_starts(participant: &Participant, starts: &BTreeMap<String, String>) -> AppResult<()> {
    #[derive(serde::Serialize)]
    struct Stored<'a> {
        version: u64,
        starts: &'a BTreeMap<String, String>,
    }
    let path = participant.dir.join(MEMBERSHIP_STARTS_FILE);
    let mut bytes = serde_json::to_vec_pretty(&Stored {
        version: MEMBERSHIP_STARTS_VERSION,
        starts,
    })
    .map_err(|error| {
        crate::error::AppError::config(&path, format!("serialize membership starts: {error}"))
    })?;
    bytes.push(b'\n');
    atomic_replace(&path, &bytes)
        .map_err(|error| crate::error::AppError::io("write membership starts", &path, error))
}

fn write_channels(participant: &Participant, state: &ParticipantChannels) -> AppResult<()> {
    #[derive(serde::Serialize)]
    struct Stored<'a> {
        version: u64,
        joined: &'a BTreeSet<String>,
        left: &'a BTreeSet<String>,
    }
    let path = participant.dir.join("channels.json");
    let mut bytes = serde_json::to_vec_pretty(&Stored {
        version: PARTICIPANT_CHANNELS_VERSION,
        joined: &state.joined,
        left: &state.left,
    })
    .map_err(|error| {
        crate::error::AppError::config(&path, format!("serialize participant channels: {error}"))
    })?;
    bytes.push(b'\n');
    atomic_replace(&path, &bytes)
        .map_err(|error| crate::error::AppError::io("write participant channels", &path, error))
}

fn parse_names(path: &std::path::Path, names: Vec<String>) -> AppResult<BTreeSet<String>> {
    let mut prior: Option<&str> = None;
    for name in &names {
        crate::channel::validate_channel_name(name).map_err(|error| {
            crate::error::AppError::config(
                path,
                format!("invalid channel name '{name}': {}", error.message),
            )
        })?;
        if prior.is_some_and(|value| value >= name.as_str()) {
            return Err(crate::error::AppError::config(
                path,
                "participant channel arrays must be sorted and duplicate-free",
            ));
        }
        prior = Some(name);
    }
    Ok(names.into_iter().collect())
}

#[derive(Debug, Default)]
pub(crate) struct ChannelState {
    channels: BTreeMap<String, BTreeSet<String>>,
}

impl ChannelState {
    pub(crate) fn load(context: &Context, room: &str) -> AppResult<Self> {
        Ok(Self {
            channels: cursor_state::Snapshot::load(context, room).into_channels(),
        })
    }

    pub(crate) fn into_channels(self) -> BTreeMap<String, BTreeSet<String>> {
        self.channels
    }
}

pub(crate) fn stored_shape_is_valid(bytes: &[u8]) -> bool {
    cursor_state::legacy_stored_shape_is_valid(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{test_root, trash_test_root};
    use std::fs;

    #[test]
    fn unparseable_created_falls_back_to_the_backlog_floor() {
        let root = test_root("channelstate-created-fallback");
        let context = Context {
            root: root.clone(),
            home: root.clone(),
        };
        let mut participant = crate::participant::bind_test_actor(&context, "alpha");
        // Joined with no recorded start (old-format state).
        let state = ParticipantChannels {
            joined: BTreeSet::from(["tax".to_owned()]),
            left: BTreeSet::new(),
            starts: BTreeMap::new(),
        };
        participant.created = "2026-08-16 20:00:00 -0400".to_owned();
        assert_eq!(
            state
                .membership_start(&context, &participant, "tax")
                .expect("start")
                .as_deref(),
            Some("20260817-000000-000000"),
            "a parseable created is the floor"
        );
        participant.created = "not a timestamp".to_owned();
        assert_eq!(
            state
                .membership_start(&context, &participant, "tax")
                .expect("start")
                .as_deref(),
            Some(BACKLOG_MEMBERSHIP_START),
            "an unparseable created keeps everything unread"
        );
        trash_test_root(&root);
    }

    #[test]
    fn a_join_writes_its_start_first_and_a_leave_writes_channels_first() {
        let start = "20260923-120000-000000".to_owned();
        let empty = ParticipantChannels::default();
        let joined = ParticipantChannels {
            joined: BTreeSet::from(["tax".to_owned()]),
            left: BTreeSet::new(),
            starts: BTreeMap::from([("tax".to_owned(), start.clone())]),
        };
        let left = ParticipantChannels {
            joined: BTreeSet::new(),
            left: BTreeSet::from(["tax".to_owned()]),
            starts: BTreeMap::new(),
        };
        // Join: a crash after the first write leaves an orphan start, never a
        // joined channel without one.
        assert_eq!(
            write_order(&empty, &joined),
            [StateFile::Starts, StateFile::Channels]
        );
        // Leave: a crash after the first write leaves the channel already left,
        // never joined with its start removed.
        assert_eq!(
            write_order(&joined, &left),
            [StateFile::Channels, StateFile::Starts]
        );
        // Rejoin after leave: the new start lands first.
        let mut rejoined = joined.clone();
        rejoined
            .starts
            .insert("tax".to_owned(), "20260924-120000-000000".to_owned());
        assert_eq!(
            write_order(&left, &rejoined),
            [StateFile::Starts, StateFile::Channels]
        );
        assert!(write_order(&joined, &joined).is_empty());
    }

    #[test]
    fn participant_leave_is_individual_and_preserves_cursor_state() {
        let root = test_root("participant-channel-leave");
        let context = Context {
            root: root.clone(),
            home: root.clone(),
        };
        let channel = root.join("channels/tax");
        fs::create_dir_all(channel.join("messages")).expect("channel messages");
        fs::write(
            channel.join("channel.json"),
            r#"{"name":"tax","created":"2026-09-16","created_by":"alpha"}"#,
        )
        .expect("channel info");
        fs::write(channel.join("members.json"), r#"{"alpha":"2026-09-16"}"#)
            .expect("legacy workspace membership");
        let participant = |id: &str| crate::participant::Participant {
            version: 1,
            id: id.to_owned(),
            harness: "test".to_owned(),
            conversation_key_digest: format!("digest-{id}"),
            created: "2026-09-16".to_owned(),
            last_seen: None,
            lease_hours: 24,
            ephemeral: false,
            ended_at: None,
            workspace: Some("alpha".to_owned()),
            workspace_path: None,
            lineage: None,
            lineage_since: None,
            display_name: None,
            dir: root.join("participants").join(id),
        };
        let a = participant("test-aaaaaaaa");
        let b = participant("test-bbbbbbbb");
        fs::create_dir_all(&a.dir).expect("participant A");
        fs::create_dir_all(&b.dir).expect("participant B");
        fs::write(
            b.dir.join("cursors.json"),
            b"{\"version\":2,\"mail\":{},\"channels\":{\"tax\":{\"seen\":[\"20990916-030000-000001-aaaaaa\"]}}}\n",
        )
        .expect("B cursor");
        let before = fs::read(b.dir.join("cursors.json")).expect("cursor before");

        assert!(ParticipantChannels::load(&a)
            .expect("A state")
            .effective(&context, &a, "tax")
            .expect("A effective"));
        assert!(ParticipantChannels::leave(&context, &b, "tax").expect("B leaves"));
        assert!(ParticipantChannels::load(&a)
            .expect("A state")
            .effective(&context, &a, "tax")
            .expect("A stays effective"));
        assert!(!ParticipantChannels::load(&b)
            .expect("B state")
            .effective(&context, &b, "tax")
            .expect("B no longer effective"));
        assert_eq!(
            fs::read(b.dir.join("cursors.json")).expect("cursor after"),
            before,
            "membership mutation must not rewrite seen state"
        );
        trash_test_root(&root);
    }
}
