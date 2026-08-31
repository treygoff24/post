use crate::channel::{acting_room, list_channels};
use crate::cli::ChannelsArgs;
use crate::command_result::CommandResult;
use crate::cursor_state::Snapshot;
use crate::error::AppResult;
use crate::mailbox::Context;
use crate::output::{self, ChannelListItem, ChannelsOutput};

pub(super) fn run(context: &Context, args: ChannelsArgs, pretty: bool) -> AppResult<CommandResult> {
    let summaries = list_channels(context)?;

    // Get acting room for unread count calculation
    let rooms = context.load_rooms()?;
    let acting_room = acting_room(context, &rooms).map(|(room, _)| room).ok();

    // Load cursor state for acting room (if any)
    let cursor_snapshot = acting_room
        .as_ref()
        .map(|room| Snapshot::load(context, room));

    let channels: Vec<ChannelListItem> = summaries
        .into_iter()
        .map(|summary| {
            // Check if acting room is a member of this channel
            let is_member = acting_room
                .as_ref()
                .map(|room| summary.members.contains_key(room))
                .unwrap_or(false);

            // Calculate unread count if acting room is a member
            let unread = if is_member {
                acting_room.as_ref().and_then(|_room| {
                    cursor_snapshot.as_ref().map(|snapshot| {
                        // Count unseen messages in this channel
                        let seen_set = snapshot.channel_seen_count(&summary.info.name);
                        summary.messages.saturating_sub(seen_set)
                    })
                })
            } else {
                None // Non-member gets null, not 0
            };

            ChannelListItem {
                name: summary.info.name,
                created: summary.info.created,
                created_by: summary.info.created_by,
                description: summary.info.description,
                members: summary.members.into_keys().collect(),
                messages: summary.messages,
                room: acting_room.clone(),
                unread,
            }
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
