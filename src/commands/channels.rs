use crate::channel::{list_channels_with, Scan};
use crate::cli::ChannelsArgs;
use crate::command_result::CommandResult;
use crate::cursor_state::{eligibility, routing};
use crate::error::AppResult;
use crate::mailbox::Context;
use crate::output::{self, ChannelListItem, ChannelsOutput};

pub(super) fn run(context: &Context, args: ChannelsArgs, pretty: bool) -> AppResult<CommandResult> {
    // One unreadable channel file or message never fails the listing: it is
    // skipped and reported (contract 2026-09-28 section 3).
    let (summaries, mut skipped) = list_channels_with(context, Scan::Tolerant)?;

    let resolved = crate::participant::resolve(context)?;
    let participant = resolved.participant();
    let acting_room = participant.and_then(|actor| actor.workspace.clone());
    let mut channels = Vec::new();
    let mut archived_hidden = 0;
    let mut roster = crate::channel_state::ChannelRoster::new(context);
    for summary in summaries {
        let archived = summary.archived.is_some();
        let listed = if args.all {
            true
        } else if args.archived {
            archived
        } else {
            !archived
        };
        if !listed {
            if archived {
                archived_hidden += 1;
            }
            continue;
        }
        let effective_members = roster.effective_participants(&summary.info.name)?;
        let is_member = participant
            .is_some_and(|actor| effective_members.iter().any(|member| member.id == actor.id));
        let unread = match (participant, is_member) {
            (Some(actor), true) => {
                let scan = eligibility::unread_channel_with(
                    context,
                    actor,
                    &summary.info.name,
                    Scan::Tolerant,
                )?;
                skipped.extend(
                    scan.skipped
                        .into_iter()
                        .map(|file| file.in_channel(&summary.info.name)),
                );
                Some(scan.items.len())
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
            archived,
            archived_at: summary.archived.as_ref().map(|mark| mark.at.clone()),
            archived_by: summary
                .archived
                .as_ref()
                .map(|mark| mark.by_participant.clone()),
        });
    }

    // Members the roster left out (an invalid membership file) are missing from
    // every `participants` list above: say so beside the listing, not only on
    // stderr.
    let skipped_members = roster.skipped().to_vec();

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
            rendered.push_str(if args.archived {
                "post: no archived channels\n"
            } else {
                "post: no channels\n"
            });
        } else {
            for channel in &channels {
                let archived_note = match (&channel.archived_at, &channel.archived_by) {
                    (Some(at), Some(by)) => format!(
                        ", archived {} by {}",
                        output::sanitize_text_header(at),
                        output::sanitize_text_header(by)
                    ),
                    _ => String::new(),
                };
                rendered.push_str(&format!(
                    "#{}  ({} members, {} messages, by {}{archived_note})\n",
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
        if archived_hidden > 0 {
            rendered.push_str(&format!(
                "({archived_hidden} archived channel(s) hidden; `post channels --archived --text` lists them)\n"
            ));
        }
        if let Some(notice) = crate::channel::skipped_notice(&skipped) {
            rendered.push_str(&notice);
        }
        if let Some(notice) = crate::channel::skipped_members_notice(&skipped_members) {
            rendered.push_str(&notice);
        }
        return Ok(CommandResult::success(rendered));
    }
    let count = channels.len();
    let mut value = serde_json::to_value(ChannelsOutput {
        ok: true,
        channels,
        count,
        archived_hidden,
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
    skipped.extend(skipped_members);
    if !skipped.is_empty() {
        object.insert("skipped".to_owned(), serde_json::json!(skipped));
    }
    let rendered = output::json(&value, pretty)?;
    Ok(CommandResult::success(rendered))
}
