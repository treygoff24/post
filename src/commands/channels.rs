use crate::channel::list_channels;
use crate::command_result::CommandResult;
use crate::error::AppResult;
use crate::mailbox::Context;
use crate::output::{self, ChannelListItem, ChannelsOutput};

pub(super) fn run(context: &Context, pretty: bool) -> AppResult<CommandResult> {
    let summaries = list_channels(context)?;
    let channels: Vec<ChannelListItem> = summaries
        .into_iter()
        .map(|summary| ChannelListItem {
            name: summary.info.name,
            created: summary.info.created,
            created_by: summary.info.created_by,
            members: summary.members.into_keys().collect(),
            messages: summary.messages,
        })
        .collect();
    let count = channels.len();
    let rendered = output::json(
        &ChannelsOutput {
            ok: true,
            channels,
            count,
        },
        pretty,
    )?;
    Ok(CommandResult::success(rendered))
}
