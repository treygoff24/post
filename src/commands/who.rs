use crate::cli::WhoArgs;
use crate::command_result::CommandResult;
use crate::error::AppResult;
use crate::mailbox::Context;
use crate::output::{self, WhoOutput, WhoRoom};
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
    let mut entries = Vec::new();
    for room in selected {
        let presence = presence::read_presence(context, &room)?;
        entries.push(WhoRoom {
            room: presence.room,
            live_watch: presence.live_watch,
            last_seen: presence.last_seen,
        });
    }
    entries.sort_by(|a, b| a.room.cmp(&b.room));
    if args.text {
        let mut rendered = String::new();
        if entries.is_empty() {
            rendered.push_str("post: no rooms to report\n");
        } else {
            for entry in &entries {
                let live = if entry.live_watch { "yes" } else { "no" };
                let seen = entry.last_seen.as_deref().unwrap_or("never");
                rendered.push_str(&format!(
                    "{}  live-watch={live}  last-seen={seen}\n",
                    output::sanitize_text_header(&entry.room)
                ));
            }
        }
        return Ok(CommandResult::success(rendered));
    }
    let count = entries.len();
    CommandResult::json(
        &WhoOutput {
            ok: true,
            rooms: entries,
            count,
        },
        pretty,
    )
}
