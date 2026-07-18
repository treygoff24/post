use crate::command_result::CommandResult;
use crate::error::AppResult;
use crate::mailbox::Context;
use crate::output::{RoomOutput, RoomsOutput};

pub(super) fn run(context: &Context, pretty: bool) -> AppResult<CommandResult> {
    let rooms = context.load_rooms()?;
    let rules = context.load_rules(&rooms)?;
    let output_rooms: Vec<_> = rooms
        .into_iter()
        .map(|(name, path)| {
            let blocked = rules
                .blocked
                .iter()
                .filter(|rule| rule.targets(&name))
                .cloned()
                .collect();
            RoomOutput {
                name,
                path,
                blocked,
            }
        })
        .collect();
    let count = output_rooms.len();
    let output = RoomsOutput {
        ok: true,
        rooms: output_rooms,
        count,
    };
    CommandResult::json(&output, pretty)
}
