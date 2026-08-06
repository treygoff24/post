use crate::channel::list_channels;
use crate::cli::ChannelsArgs;
use crate::command_result::CommandResult;
use crate::error::AppResult;
use crate::mailbox::Context;
use crate::output::{self, ChannelListItem, ChannelsOutput};

pub(super) fn run(context: &Context, args: ChannelsArgs, pretty: bool) -> AppResult<CommandResult> {
    let summaries = list_channels(context)?;
    let channels: Vec<ChannelListItem> = summaries
        .into_iter()
        .map(|summary| ChannelListItem {
            name: summary.info.name,
            created: summary.info.created,
            created_by: summary.info.created_by,
            description: summary.info.description,
            members: summary.members.into_keys().collect(),
            messages: summary.messages,
        })
        .collect();
    if args.text {
        let mut rendered = String::new();
        if channels.is_empty() {
            rendered.push_str("post: no channels\n");
        } else {
            for channel in &channels {
                rendered.push_str(&format!(
                    "#{}  ({} members, {} messages, by {})\n",
                    output::sanitize_text_header(&channel.name),
                    channel.members.len(),
                    channel.messages,
                    output::sanitize_text_header(&channel.created_by)
                ));
                if let Some(description) = &channel.description {
                    rendered.push_str(&format!(
                        "  {}\n",
                        output::sanitize_text_header(description)
                    ));
                }
            }
        }
        return Ok(CommandResult::success(rendered));
    }
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
