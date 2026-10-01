use crate::cli::WhoArgs;
use crate::command_result::CommandResult;
use crate::cursor_state::eligibility::MailCounts;
use crate::cursor_state::ParticipantCursors;
use crate::error::{AppError, AppResult, ErrorCode};
use crate::mailbox::Context;
use crate::output::{
    self, WhoActingParticipant, WhoBridgeHealth, WhoOutput, WhoParticipant, WhoRoom,
};
use crate::participant::Resolved;
use crate::presence;
use std::collections::{BTreeMap, BTreeSet};

/// Text-only footer: `lease=` is liveness of the participant's binding, which
/// readers kept taking for "they saw my message". JSON keeps its `state` key.
const LEASE_NOT_ATTENTION_HINT: &str = "lease is not attention; for 'did they read it' use `post chat <channel> --seen-by <message-id>`";

const STALE_DELIVERY_NOTE: &str = "mail already frozen to a stale participant is not reassigned when its lease expires; activity affects new recipient selection only";

pub(super) fn run(
    context: &Context,
    args: WhoArgs,
    json: bool,
    pretty: bool,
) -> AppResult<CommandResult> {
    if let Some(role) = args.role.as_deref() {
        if !crate::participant::RUNTIME_ROLES.contains(&role) {
            return Err(AppError::invalid_argument(format!(
                "--role must be one of {}",
                crate::participant::RUNTIME_ROLES.join(", ")
            )));
        }
    }
    let rooms = context.load_rooms()?;
    let room_filter_requested = !args.room.is_empty();
    let selected: Vec<String> = if args.room.is_empty() {
        rooms.keys().cloned().collect()
    } else {
        let mut unique = Vec::new();
        for room in args.room {
            let resolved = context.resolved_room(Some(room), &rooms)?;
            if !unique.contains(&resolved) {
                unique.push(resolved);
            }
        }
        unique
    };
    // `--room` restricts the whole report, not just the legacy heartbeat rows:
    // a scoped request must not answer with every participant on the host. A
    // participant belongs to a room through the workspace it bound in (the same
    // field `post who` prints); a session-only participant has no workspace and
    // is therefore not part of a scoped report.
    //
    // `scope` is `None` when `--room` was omitted, and that difference is load
    // bearing: `selected` then holds EVERY registered room (the heartbeat rows
    // an unscoped caller expects), so filtering participant rows through it
    // would look like no filter at all while silently dropping every
    // session-only participant -- the ones whose only address is their own id,
    // which the unscoped command promises to list.
    let scope: Option<BTreeSet<&str>> =
        room_filter_requested.then(|| selected.iter().map(String::as_str).collect());
    let doorbell = presence::read_doorbell(context);
    // The count of stuck or refused letters and collisions the bridge lists in
    // its health file: who is where agents look first, and the bridge's own
    // `ok` only says it is running.
    let bridge = super::doctor::bridge_attention(context);
    let bridge_attention = bridge.items.len();
    // A bridged host whose health file cannot be read has no attention count
    // to show, and a missing count must not read as "nothing stuck".
    let bridge_health = bridge.unreadable.as_deref().map(|reason| WhoBridgeHealth {
        reason: super::doctor::bridge_health_message(reason),
        fix: super::doctor::BRIDGE_HEALTH_FIX.to_owned(),
    });
    let mut legacy_rooms = Vec::new();
    for room in &selected {
        let presence = presence::read_presence(context, room)?;
        let doorbell_armed = doorbell.rooms.contains(&presence.room);
        legacy_rooms.push(WhoRoom {
            live_watch: presence.live_watch || doorbell_armed,
            room: presence.room,
            last_seen: presence.last_seen,
            doorbell_armed,
        });
    }
    legacy_rooms.sort_by(|a, b| a.room.cmp(&b.room));

    // A claim that names no record is not "unbound" and not "no mail": who
    // reports its participant as `missing` with the fix, and still lists
    // everyone else. The `bound: false` and `participant_missing` fields
    // beside it are added by the dispatcher, which resolves the same claim for
    // every read-only listing.
    let (resolved, resolution_failed, missing) = match crate::participant::resolve(context) {
        Ok(resolved) => (resolved, false, None),
        Err(error) if error.code == ErrorCode::ParticipantMissing => (
            Resolved::Unbound,
            true,
            Some(super::participant::MissingReport::from_error(&error)),
        ),
        Err(_) => (Resolved::Unbound, true, None),
    };
    let acting_id = resolved
        .participant()
        .map(|participant| participant.id.clone());
    let now = std::time::SystemTime::now();
    let mut counts = MailCounts::new(context);
    let acting = match &resolved {
        Resolved::Bound {
            participant,
            provenance,
        } => {
            let (unread, pending) = mail_counts(context, &mut counts, participant)?;
            WhoActingParticipant {
                status: "bound".to_owned(),
                state: Some(participant.state_label(now).to_owned()),
                last_seen: participant.last_seen.clone(),
                id: Some(participant.id.clone()),
                harness: Some(participant.harness.clone()),
                provenance: Some(provenance.as_str().to_owned()),
                workspace: participant.workspace.clone(),
                lineage: participant.lineage.clone(),
                runtime: participant.runtime.clone(),
                unread,
                pending,
                fix: None,
            }
        }
        Resolved::Unbound => WhoActingParticipant {
            status: if missing.is_some() {
                "missing"
            } else {
                "unbound"
            }
            .to_owned(),
            state: None,
            last_seen: None,
            id: None,
            harness: None,
            provenance: None,
            workspace: None,
            lineage: None,
            runtime: None,
            unread: BTreeMap::new(),
            pending: BTreeMap::new(),
            fix: Some(match &missing {
                Some(report) => format!("run: {}", report.fix()),
                None => "run: post participant bind".to_owned(),
            }),
        },
    };
    let participant_presence_context = Context {
        root: context.root.join(crate::participant::PARTICIPANTS_DIR),
        home: context.home.clone(),
    };
    let mut participants = Vec::new();
    // Damaged records are left out of the roster and named in `skipped`, on
    // stdout: a caller that discards stderr must not read a roster with a
    // hole in it as complete. A damaged record's workspace is unknowable, so
    // a workspace scope cannot filter them out either.
    let (participant_records, skipped) = match crate::participant::list_with_skipped(context) {
        Ok(listing) => listing,
        Err(_) if resolution_failed => (Vec::new(), Vec::new()),
        Err(error) => return Err(error),
    };
    crate::participant::warn_skipped(&skipped);
    // Names and sigils come from the profile registry. A damaged registry
    // must not take the roster down, so it reads as "no names" here.
    let profiles = crate::profile::load_profiles(context).unwrap_or_default();
    // `--live`: who is here now, with the profile and runtime facts the text
    // line shows. Keyed by id; only live participants that pass the filters.
    let mut live_peers: BTreeMap<String, crate::peers::Peer> = BTreeMap::new();
    for participant in participant_records {
        if let Some(scope) = scope.as_ref() {
            if !participant
                .workspace
                .as_deref()
                .is_some_and(|workspace| scope.contains(workspace))
            {
                continue;
            }
        }
        let (unread, pending) = mail_counts(context, &mut counts, &participant)?;
        let presence = presence::read_presence(&participant_presence_context, &participant.id)?;
        let state = participant.state_label(now).to_owned();
        let doorbell_armed = doorbell.participants.contains(&participant.id);
        let live_watch = presence.live_watch || doorbell_armed;
        let entry = profiles.get(&crate::profile::participant_key(&participant.id));
        let name = entry
            .and_then(|entry| entry.name.as_deref())
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty());
        let pfp = entry.and_then(|entry| entry.pfp.clone());
        if args.live {
            let age = crate::peers::activity_age(&participant, now)
                .filter(|age| live_watch && *age <= crate::peers::LIVE_WINDOW);
            let Some(age) = age else { continue };
            let runtime = participant.runtime.as_ref();
            if let Some(role) = args.role.as_deref() {
                if runtime.and_then(|runtime| runtime.role.as_deref()) != Some(role) {
                    continue;
                }
            }
            if let Some(selector) = args.repo.as_deref() {
                let repo = runtime.and_then(|runtime| runtime.repo.as_deref());
                if !repo.is_some_and(|repo| crate::peers::repo_matches(repo, selector)) {
                    continue;
                }
            }
            live_peers.insert(
                participant.id.clone(),
                crate::peers::Peer {
                    participant: participant.clone(),
                    name: name.clone(),
                    pfp: pfp.clone(),
                    age_secs: age.as_secs(),
                },
            );
        }
        participants.push(WhoParticipant {
            id: participant.id,
            harness: participant.harness,
            state,
            last_seen: participant.last_seen,
            lineage: participant.lineage,
            workspace: participant.workspace,
            name,
            pfp,
            runtime: participant.runtime,
            unread,
            pending,
            live_watch,
            watch_last_seen: presence.last_seen,
            doorbell_armed,
        });
    }
    participants.sort_by(|left, right| {
        let left_acting = acting_id.as_deref() == Some(left.id.as_str());
        let right_acting = acting_id.as_deref() == Some(right.id.as_str());
        right_acting.cmp(&left_acting).then(left.id.cmp(&right.id))
    });
    if args.live && !args.text && !json {
        // One line per live participant, in the roster's order.
        let rendered: String = participants
            .iter()
            .filter_map(|entry| live_peers.get(&entry.id))
            .map(|peer| format!("{}\n", crate::peers::live_line(peer)))
            .collect();
        return Ok(CommandResult::success(rendered));
    }
    if args.live {
        // The rooms' own heartbeats are not participants.
        legacy_rooms.clear();
    }
    if args.text {
        let mut rendered = String::new();
        if let Some(report) = &missing {
            rendered.push_str(&format!(
                "participant: missing ({}) Fix: {}\n",
                output::sanitize_text_header(&report.message),
                report.suggested_fix
            ));
        } else if acting.status == "unbound" {
            rendered.push_str("participant: unbound (run: post participant bind)\n");
        } else {
            rendered.push_str(&format!(
                "participant: {}  lease={}  last-seen={}  harness={}  provenance={}  workspace={}  lineage={}  unread={:?}  pending={:?}\n",
                acting.id.as_deref().unwrap_or("unbound"),
                acting.state.as_deref().unwrap_or("unbound"),
                acting
                    .last_seen
                    .as_deref()
                    .unwrap_or("no lease record"),
                acting.harness.as_deref().unwrap_or("unbound"),
                acting.provenance.as_deref().unwrap_or("unbound"),
                acting.workspace.as_deref().unwrap_or("none"),
                acting.lineage.as_deref().unwrap_or("none"),
                acting.unread,
                acting.pending,
            ));
        }
        for entry in &participants {
            let live = if entry.live_watch { "yes" } else { "no" };
            let seen = entry.last_seen.as_deref().unwrap_or("no lease record");
            let watch_seen = entry.watch_last_seen.as_deref().unwrap_or("never");
            let armed = if entry.doorbell_armed {
                "  doorbell=armed"
            } else {
                ""
            };
            rendered.push_str(&format!(
                "participant {}  lease={}  last-seen={seen}  harness={}  lineage={}  workspace={}  live-watch={live}  watch-last-seen={watch_seen}{armed}  unread={:?}  pending={:?}\n",
                output::sanitize_text_header(&entry.id),
                entry.state,
                output::sanitize_text_header(&entry.harness),
                entry.lineage.as_deref().map(output::sanitize_text_header).unwrap_or_else(|| "none".to_owned()),
                entry.workspace.as_deref().map(output::sanitize_text_header).unwrap_or_else(|| "none".to_owned()),
                entry.unread,
                entry.pending,
            ));
        }
        if !skipped.is_empty() {
            rendered.push_str(&skipped_line(&skipped));
        }
        if participants
            .iter()
            .any(|participant| matches!(participant.state.as_str(), "stale" | "no lease record"))
        {
            rendered.push_str(&format!("activity-note: {STALE_DELIVERY_NOTE}\n"));
        }
        if !matches!(acting.status.as_str(), "unbound" | "missing") || !participants.is_empty() {
            rendered.push_str(&format!("hint: {LEASE_NOT_ATTENTION_HINT}\n"));
        }
        for entry in &legacy_rooms {
            let live = if entry.live_watch { "yes" } else { "no" };
            let seen = entry.last_seen.as_deref().unwrap_or("never");
            let armed = if entry.doorbell_armed {
                "  doorbell=armed"
            } else {
                ""
            };
            rendered.push_str(&format!(
                "legacy-room {}  live-watch={live}  last-seen={seen}{armed}\n",
                output::sanitize_text_header(&entry.room)
            ));
        }
        // A doorbell file that cannot vouch for anything is said out loud: a
        // reader seeing live-watch=no must know the supervisor's word was not
        // counted, not that nobody is armed.
        if matches!(doorbell.state, "stale" | "unreadable") {
            rendered.push_str(&format!(
                "doorbell: {} (live-watch counts `post watch` heartbeats only; the supervisor's armed subscriptions were not counted)\n",
                doorbell.state
            ));
        }
        if bridge_attention > 0 {
            rendered.push_str(&format!(
                "bridge_attention: {bridge_attention} (run `post doctor` for each item and its fix)\n"
            ));
        }
        if let Some(health) = &bridge_health {
            rendered.push_str(&format!(
                "bridge_health: {} Fix: {}\n",
                health.reason, health.fix
            ));
        }
        return Ok(CommandResult::success(rendered));
    }
    let count = participants.len();
    let activity_note = participants
        .iter()
        .any(|participant| matches!(participant.state.as_str(), "stale" | "no lease record"))
        .then(|| STALE_DELIVERY_NOTE.to_owned());
    CommandResult::json(
        &WhoOutput {
            ok: true,
            participant: acting,
            participants,
            legacy_rooms,
            count,
            skipped,
            activity_note,
            bridge_attention,
            bridge_health,
            doorbell: (doorbell.state != "absent").then(|| doorbell.state.to_owned()),
        },
        pretty,
    )
}

/// The one text line for participants whose records could not be read. Ids
/// and reasons come from the store, so both are sanitized like every other
/// stored value on a `who --text` line.
fn skipped_line(skipped: &[crate::participant::SkippedParticipant]) -> String {
    let named = skipped
        .iter()
        .map(|record| {
            format!(
                "{} ({})",
                output::sanitize_text_header(&record.id),
                output::sanitize_text_header(&record.reason)
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "skipped: {} participant record(s) could not be read and are missing from this roster: {named}. Fix: repair each participants/<id>/participant.json, or remove that directory if the participant is gone.\n",
        skipped.len()
    )
}

fn mail_counts(
    context: &Context,
    counts: &mut MailCounts,
    participant: &crate::participant::Participant,
) -> AppResult<(BTreeMap<String, usize>, BTreeMap<String, usize>)> {
    let mut unread = BTreeMap::new();
    let mut pending = BTreeMap::new();
    let cursors = ParticipantCursors::load(context, participant);
    let received = counts.received(participant)?;
    for address in super::inbox::visible_addresses_among(participant, received) {
        let label = super::inbox::address_label(&address);
        unread.insert(
            label.clone(),
            counts.unread(participant, &cursors, &address)?,
        );
        pending.insert(label, counts.pending(participant, &address)?);
    }
    Ok((unread, pending))
}
