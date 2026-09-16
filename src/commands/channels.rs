use crate::channel::list_channels;
use crate::cli::ChannelsArgs;
use crate::command_result::CommandResult;
use crate::cursor_state::{eligibility, routing};
use crate::error::AppResult;
use crate::mailbox::Context;
use crate::output::{self, ChannelListItem, ChannelsOutput};

pub(super) fn run(context: &Context, args: ChannelsArgs, pretty: bool) -> AppResult<CommandResult> {
    let summaries = list_channels(context)?;

    let resolved = crate::participant::resolve(context)?;
    let participant = resolved.participant();
    let acting_room = participant.and_then(|actor| actor.workspace.clone());
    let mut channels = Vec::new();
    for summary in summaries {
        let effective_members =
            crate::channel_state::effective_participants(context, &summary.info.name)?;
        let is_member = participant
            .is_some_and(|actor| effective_members.iter().any(|member| member.id == actor.id));
        let unread = match (participant, is_member) {
            (Some(actor), true) => {
                Some(eligibility::unread_channel(context, actor, &summary.info.name)?.len())
            }
            _ => None,
        };
        let participants: Vec<String> = effective_members
            .iter()
            .map(|member| member.id.clone())
            .collect();
        let mut members: Vec<String> = summary.members.into_keys().collect();
        members.extend(
            effective_members
                .iter()
                .filter_map(|member| member.workspace.clone()),
        );
        members.sort();
        members.dedup();
        channels.push(ChannelListItem {
            name: summary.info.name,
            created: summary.info.created,
            created_by: summary.info.created_by,
            description: summary.info.description,
            members,
            participants,
            messages: summary.messages,
            room: acting_room.clone(),
            unread,
        });
    }

    let pending = if let Some(participant) = participant {
        let mut count = 0;
        for address in super::inbox::visible_addresses(context, participant)? {
            count += routing::provisional_pending_for(context, participant, &address)?.len();
        }
        count
    } else {
        let mut count = 0;
        for room in context.load_rooms()?.into_keys() {
            count += routing::pending_count(
                context,
                &crate::participant::Address {
                    kind: crate::participant::AddressKind::Workspace,
                    name: room,
                },
            )?;
        }
        count
    };

    if args.text {
        let mut rendered = String::new();
        rendered.push_str(&format!(
            "participant: {}\npending: {pending}\n",
            participant.map_or("unbound", |actor| actor.id.as_str())
        ));
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
    let mut value = serde_json::to_value(ChannelsOutput {
        ok: true,
        channels,
        count,
    })
    .map_err(|error| {
        crate::error::AppError::invalid_argument(format!("serialize channels: {error}"))
    })?;
    let object = value.as_object_mut().expect("channels output is an object");
    object.insert(
        "participant".to_owned(),
        serde_json::Value::String(
            participant.map_or_else(|| "unbound".to_owned(), |actor| actor.id.clone()),
        ),
    );
    object.insert("pending".to_owned(), serde_json::json!(pending));
    let rendered = output::json(&value, pretty)?;
    Ok(CommandResult::success(rendered))
}
