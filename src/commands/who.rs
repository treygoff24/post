use crate::cli::WhoArgs;
use crate::command_result::CommandResult;
use crate::error::AppResult;
use crate::mailbox::Context;
use crate::output::{self, WhoActingParticipant, WhoOutput, WhoParticipant, WhoRoom};
use crate::participant::Resolved;
use crate::presence;
use std::collections::BTreeMap;

const STALE_DELIVERY_NOTE: &str = "mail already frozen to a stale participant is not reassigned when its lease expires; activity affects new recipient selection only";

pub(super) fn run(context: &Context, args: WhoArgs, pretty: bool) -> AppResult<CommandResult> {
    let rooms = context.load_rooms()?;
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
    let mut legacy_rooms = Vec::new();
    for room in selected {
        let presence = presence::read_presence(context, &room)?;
        legacy_rooms.push(WhoRoom {
            room: presence.room,
            live_watch: presence.live_watch,
            last_seen: presence.last_seen,
        });
    }
    legacy_rooms.sort_by(|a, b| a.room.cmp(&b.room));

    let (resolved, resolution_failed) = match crate::participant::resolve(context) {
        Ok(resolved) => (resolved, false),
        Err(_) => (Resolved::Unbound, true),
    };
    let acting_id = resolved
        .participant()
        .map(|participant| participant.id.clone());
    let now = std::time::SystemTime::now();
    let acting = match &resolved {
        Resolved::Bound {
            participant,
            provenance,
        } => {
            let (unread, pending) = mail_counts(context, participant)?;
            WhoActingParticipant {
                status: "bound".to_owned(),
                state: Some(participant.state(now).as_str().to_owned()),
                last_seen: participant.last_seen.clone(),
                id: Some(participant.id.clone()),
                harness: Some(participant.harness.clone()),
                provenance: Some(provenance.as_str().to_owned()),
                workspace: participant.workspace.clone(),
                lineage: participant.lineage.clone(),
                unread,
                pending,
                fix: None,
            }
        }
        Resolved::Unbound => WhoActingParticipant {
            status: "unbound".to_owned(),
            state: None,
            last_seen: None,
            id: None,
            harness: None,
            provenance: None,
            workspace: None,
            lineage: None,
            unread: BTreeMap::new(),
            pending: BTreeMap::new(),
            fix: Some("run: post participant bind".to_owned()),
        },
    };
    let participant_presence_context = Context {
        root: context.root.join(crate::participant::PARTICIPANTS_DIR),
        home: context.home.clone(),
    };
    let mut participants = Vec::new();
    let participant_records = match crate::participant::list(context) {
        Ok(participants) => participants,
        Err(_) if resolution_failed => Vec::new(),
        Err(error) => return Err(error),
    };
    for participant in participant_records {
        let (unread, pending) = mail_counts(context, &participant)?;
        let presence = presence::read_presence(&participant_presence_context, &participant.id)?;
        let state = participant.state(now).as_str().to_owned();
        participants.push(WhoParticipant {
            id: participant.id,
            harness: participant.harness,
            state,
            last_seen: participant.last_seen,
            lineage: participant.lineage,
            workspace: participant.workspace,
            unread,
            pending,
            live_watch: presence.live_watch,
            watch_last_seen: presence.last_seen,
        });
    }
    participants.sort_by(|left, right| {
        let left_acting = acting_id.as_deref() == Some(left.id.as_str());
        let right_acting = acting_id.as_deref() == Some(right.id.as_str());
        right_acting.cmp(&left_acting).then(left.id.cmp(&right.id))
    });
    if args.text {
        let mut rendered = String::new();
        if acting.status == "unbound" {
            rendered.push_str("participant: unbound (run: post participant bind)\n");
        } else {
            rendered.push_str(&format!(
                "participant: {}  state={}  last-seen={}  harness={}  provenance={}  workspace={}  lineage={}  unread={:?}  pending={:?}\n",
                acting.id.as_deref().unwrap_or("unbound"),
                acting.state.as_deref().unwrap_or("unbound"),
                acting.last_seen.as_deref().unwrap_or("legacy"),
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
            let seen = entry.last_seen.as_deref().unwrap_or("legacy");
            let watch_seen = entry.watch_last_seen.as_deref().unwrap_or("never");
            rendered.push_str(&format!(
                "participant {}  state={}  last-seen={seen}  harness={}  lineage={}  workspace={}  live-watch={live}  watch-last-seen={watch_seen}  unread={:?}  pending={:?}\n",
                output::sanitize_text_header(&entry.id),
                entry.state,
                output::sanitize_text_header(&entry.harness),
                entry.lineage.as_deref().map(output::sanitize_text_header).unwrap_or_else(|| "none".to_owned()),
                entry.workspace.as_deref().map(output::sanitize_text_header).unwrap_or_else(|| "none".to_owned()),
                entry.unread,
                entry.pending,
            ));
        }
        if participants
            .iter()
            .any(|participant| participant.state == "stale")
        {
            rendered.push_str(&format!("activity-note: {STALE_DELIVERY_NOTE}\n"));
        }
        for entry in &legacy_rooms {
            let live = if entry.live_watch { "yes" } else { "no" };
            let seen = entry.last_seen.as_deref().unwrap_or("never");
            rendered.push_str(&format!(
                "legacy-room {}  live-watch={live}  last-seen={seen}\n",
                output::sanitize_text_header(&entry.room)
            ));
        }
        return Ok(CommandResult::success(rendered));
    }
    let count = participants.len();
    let activity_note = participants
        .iter()
        .any(|participant| participant.state == "stale")
        .then(|| STALE_DELIVERY_NOTE.to_owned());
    CommandResult::json(
        &WhoOutput {
            ok: true,
            participant: acting,
            participants,
            legacy_rooms,
            count,
            activity_note,
        },
        pretty,
    )
}

fn mail_counts(
    context: &Context,
    participant: &crate::participant::Participant,
) -> AppResult<(BTreeMap<String, usize>, BTreeMap<String, usize>)> {
    let mut unread = BTreeMap::new();
    let mut pending = BTreeMap::new();
    for address in super::inbox::visible_addresses(participant) {
        let label = format!("{}:{}", address.kind.as_str(), address.name);
        unread.insert(
            label.clone(),
            crate::cursor_state::eligibility::unread_mail(context, participant, &address)?.len(),
        );
        pending.insert(
            label,
            crate::cursor_state::routing::provisional_pending_for(context, participant, &address)?
                .len(),
        );
    }
    Ok((unread, pending))
}
