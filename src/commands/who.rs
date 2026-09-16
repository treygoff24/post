use crate::cli::WhoArgs;
use crate::command_result::CommandResult;
use crate::error::AppResult;
use crate::mailbox::Context;
use crate::output::{self, WhoActingParticipant, WhoOutput, WhoParticipant, WhoRoom};
use crate::participant::Resolved;
use crate::presence;

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

    let resolved = crate::participant::resolve(context)?;
    let acting_id = resolved
        .participant()
        .map(|participant| participant.id.clone());
    let acting = match &resolved {
        Resolved::Bound {
            participant,
            provenance,
        } => WhoActingParticipant {
            status: "bound".to_owned(),
            id: Some(participant.id.clone()),
            harness: Some(participant.harness.clone()),
            provenance: Some(provenance.as_str().to_owned()),
            workspace: participant.workspace.clone(),
            lineage: participant.lineage.clone(),
            fix: None,
        },
        Resolved::Unbound => WhoActingParticipant {
            status: "unbound".to_owned(),
            id: None,
            harness: None,
            provenance: None,
            workspace: None,
            lineage: None,
            fix: Some("run: post participant bind".to_owned()),
        },
    };
    let participant_presence_context = Context {
        root: context.root.join(crate::participant::PARTICIPANTS_DIR),
        home: context.home.clone(),
    };
    let mut participants = Vec::new();
    for participant in crate::participant::list(context)? {
        let presence = presence::read_presence(&participant_presence_context, &participant.id)?;
        participants.push(WhoParticipant {
            id: participant.id,
            harness: participant.harness,
            lineage: participant.lineage,
            workspace: participant.workspace,
            live_watch: presence.live_watch,
            last_seen: presence.last_seen,
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
                "participant: {}  harness={}  provenance={}  workspace={}  lineage={}\n",
                acting.id.as_deref().unwrap_or("unbound"),
                acting.harness.as_deref().unwrap_or("unbound"),
                acting.provenance.as_deref().unwrap_or("unbound"),
                acting.workspace.as_deref().unwrap_or("none"),
                acting.lineage.as_deref().unwrap_or("none"),
            ));
        }
        for entry in &participants {
            let live = if entry.live_watch { "yes" } else { "no" };
            let seen = entry.last_seen.as_deref().unwrap_or("never");
            rendered.push_str(&format!(
                "participant {}  harness={}  lineage={}  workspace={}  live-watch={live}  last-seen={seen}\n",
                output::sanitize_text_header(&entry.id),
                output::sanitize_text_header(&entry.harness),
                entry.lineage.as_deref().map(output::sanitize_text_header).unwrap_or_else(|| "none".to_owned()),
                entry.workspace.as_deref().map(output::sanitize_text_header).unwrap_or_else(|| "none".to_owned()),
            ));
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
    CommandResult::json(
        &WhoOutput {
            ok: true,
            participant: acting,
            participants,
            legacy_rooms,
            count,
        },
        pretty,
    )
}
