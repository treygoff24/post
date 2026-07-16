use super::{CommandResult, Context};
use crate::error::AppResult;
use crate::output::{self, BlockingRuleOutput, RoomOutput, RoomsOutput};

pub fn run(context: &Context, pretty: bool) -> AppResult<CommandResult> {
    let rooms = context.load_rooms()?;
    let rules = context.load_rules(&rooms)?;
    let output_rooms: Vec<_> = rooms
        .into_iter()
        .map(|(name, path)| {
            let blocked = rules
                .blocked
                .iter()
                .filter(|rule| rule.to == "*" || rule.to == name)
                .map(BlockingRuleOutput::from)
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
    Ok(CommandResult::success(output::json(&output, pretty)?))
}
