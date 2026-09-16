//! Compatibility read seam for the pre-Plan-B channel state.
//!
//! New state lives in cursor_state. This module keeps the names used by the
//! existing channel, watch, doctor, and consuming-read callers while the
//! consuming callers migrate to Delta directly.

use crate::cursor_state;
use crate::error::AppResult;
use crate::mailbox::{atomic_replace, Context};
use crate::participant::Participant;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;

const PARTICIPANT_CHANNELS_VERSION: u64 = 1;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ParticipantChannels {
    joined: BTreeSet<String>,
    left: BTreeSet<String>,
}

impl ParticipantChannels {
    pub(crate) fn load(participant: &Participant) -> AppResult<Self> {
        let path = participant.dir.join("channels.json");
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default())
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
            crate::error::AppError::config(
                &path,
                format!("invalid participant channels JSON: {error}"),
            )
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
        Ok(Self { joined, left })
    }

    pub(crate) fn effective(
        &self,
        context: &Context,
        participant: &Participant,
        channel: &str,
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
        Ok(crate::channel::ChannelPaths::new(context, channel)?
            .load_members()?
            .contains_key(workspace))
    }

    pub(crate) fn joined_names(&self) -> &BTreeSet<String> {
        &self.joined
    }

    pub(crate) fn explicitly_left(&self, channel: &str) -> bool {
        self.left.contains(channel)
    }

    pub(crate) fn join(
        context: &Context,
        participant: &Participant,
        channel: &str,
    ) -> AppResult<bool> {
        crate::channel::validate_channel_name(channel)?;
        mutate(context, participant, |state| {
            state.left.remove(channel);
            Ok(state.joined.insert(channel.to_owned()))
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

pub(crate) fn effective_participants(
    context: &Context,
    channel: &str,
) -> AppResult<Vec<Participant>> {
    participants_for_channel(context, channel, false)
}

pub(crate) fn participants_for_join_validation(
    context: &Context,
    channel: &str,
) -> AppResult<Vec<Participant>> {
    participants_for_channel(context, channel, true)
}

fn participants_for_channel(
    context: &Context,
    channel: &str,
    include_unknown: bool,
) -> AppResult<Vec<Participant>> {
    let mut participants = Vec::new();
    for participant in crate::participant::list(context)? {
        let state = match ParticipantChannels::load(&participant) {
            Ok(state) => state,
            Err(error) if error.code == crate::error::ErrorCode::ConfigInvalid => {
                eprintln!(
                    "post: warning: skipped invalid participant channels {:?}: {:?}",
                    participant.id, error.message
                );
                if include_unknown {
                    participants.push(participant);
                }
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
    if state != before {
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
            crate::error::AppError::config(
                &path,
                format!("serialize participant channels: {error}"),
            )
        })?;
        bytes.push(b'\n');
        atomic_replace(&path, &bytes).map_err(|error| {
            crate::error::AppError::io("write participant channels", &path, error)
        })?;
    }
    Ok(result)
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

    #[allow(dead_code)]
    pub(crate) fn has_seen(&self, channel: &str, id: &str) -> bool {
        self.channels
            .get(channel)
            .is_some_and(|seen| seen.contains(id))
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

    const ID1: &str = "20260831-171234-000001-a1b2c3";
    const ID2: &str = "20260831-171234-000002-b2c3d4";

    #[test]
    fn load_prefers_materialized_cursor_over_legacy_channel_state() {
        let root = test_root("channelstate-delegation");
        let context = Context {
            root: root.clone(),
            home: root.clone(),
        };
        fs::create_dir_all(root.join("alpha")).expect("room");
        fs::write(
            root.join("alpha/channel-state.json"),
            format!(r#"{{"version":2,"channels":{{"tax":{{"seen":["{ID1}"]}}}}}}"#),
        )
        .expect("legacy");
        fs::write(
            root.join("alpha/cursors.json"),
            format!(
                r#"{{
  "version": 1,
  "mail": {{"seen": []}},
  "channels": {{"tax": {{"seen": ["{ID2}"]}}}}
}}
"#
            ),
        )
        .expect("cursor");

        let state = ChannelState::load(&context, "alpha").expect("load");
        assert!(!state.has_seen("tax", ID1));
        assert!(state.has_seen("tax", ID2));
        trash_test_root(&root);
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
