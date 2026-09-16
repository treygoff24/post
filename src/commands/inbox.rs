use crate::cli::InboxArgs;
use crate::command_result::CommandResult;
use crate::cursor_state::{eligibility, routing};
use crate::error::{AppError, AppResult};
use crate::mailbox::Context;
use crate::model::{Envelope, MailKind};
use crate::output;
use crate::participant::{Address, AddressKind, Participant, Resolved};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Serialize)]
struct InboxItemV2 {
    id: String,
    from: String,
    origin: String,
    kind: MailKind,
    subject: String,
    sent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pfp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sender_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sender_provenance: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    from_participant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    from_lineage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_to_participant: Option<String>,
    reply_to_shared: String,
}

impl InboxItemV2 {
    fn new(context: &Context, envelope: Envelope) -> Self {
        let reply = output::reply_metadata(
            context,
            &envelope.from,
            envelope.from_participant.as_deref(),
            envelope.sender_provenance.as_deref(),
        );
        Self {
            id: envelope.id,
            from: envelope.from,
            origin: reply.origin,
            kind: envelope.kind,
            subject: envelope.subject,
            sent: envelope.sent,
            display_name: envelope.display_name,
            pfp: envelope.pfp,
            sender_address: envelope.sender_address,
            sender_provenance: envelope.sender_provenance,
            from_participant: envelope.from_participant,
            from_lineage: envelope.from_lineage,
            reply_to_participant: reply.participant,
            reply_to_shared: reply.shared,
        }
    }
}

#[derive(Serialize)]
struct InboxOutputV2 {
    ok: bool,
    room: String,
    participant: String,
    unread: Vec<InboxItemV2>,
    count: usize,
    skipped_unreadable: usize,
    unread_count: usize,
    pending: usize,
    pending_by_address: BTreeMap<String, usize>,
}

#[derive(Serialize)]
struct AdoptOutput {
    ok: bool,
    participant: String,
    lineage: String,
    routed: usize,
    pending: usize,
}

pub(super) fn run(context: &Context, args: InboxArgs, pretty: bool) -> AppResult<CommandResult> {
    let resolved = crate::participant::resolve(context)?;
    if args.adopt {
        let participant = resolved.participant().ok_or_else(|| {
            AppError::no_participant(crate::participant::bind_key_available().unwrap_or(false))
        })?;
        return adopt(context, participant, pretty);
    }

    match resolved {
        Resolved::Bound { participant, .. } => list_bound(context, &participant, args, pretty),
        Resolved::Unbound => list_unbound(context, args, pretty),
    }
}

fn list_bound(
    context: &Context,
    participant: &Participant,
    args: InboxArgs,
    pretty: bool,
) -> AppResult<CommandResult> {
    let addresses = if let Some(room) = args.room {
        let rooms = context.load_rooms()?;
        let room = context.resolved_room(Some(room), &rooms)?;
        vec![Address {
            kind: AddressKind::Workspace,
            name: room,
        }]
    } else {
        visible_addresses(context, participant)?
    };
    let mut unread = Vec::new();
    let mut pending_by_address = BTreeMap::new();
    for address in &addresses {
        for item in eligibility::unread_mail(context, participant, address)? {
            unread.push(InboxItemV2::new(context, item.envelope));
        }
        let pending = routing::provisional_pending_for(context, participant, address)?.len();
        pending_by_address.insert(address_label(address), pending);
    }
    unread.sort_by(|left, right| left.id.cmp(&right.id));
    unread.dedup_by(|left, right| left.id == right.id);
    let count = unread.len();
    let pending = pending_by_address.values().sum();
    let room = participant
        .workspace
        .clone()
        .unwrap_or_else(|| participant.id.clone());
    if args.text {
        let mut rendered = format!(
            "participant: {}\npost: inbox for {} ({count} unread; pending {pending})\n",
            output::sanitize_text_header(&participant.id),
            output::sanitize_text_header(&room)
        );
        for mail in unread {
            let subject = if mail.subject.is_empty() {
                String::new()
            } else {
                format!("  {:?}", mail.subject)
            };
            let sender = output::sender_label_quoted(
                &mail.from,
                mail.display_name.as_deref(),
                mail.pfp.as_deref(),
            );
            rendered.push_str(&format!(
                "{}  [{}] from {}{}\n",
                output::sanitize_text_header(&mail.id),
                mail.kind,
                sender,
                subject
            ));
            render_reply_targets(
                &mut rendered,
                mail.reply_to_participant.as_deref(),
                &mail.reply_to_shared,
            );
        }
        return Ok(CommandResult::success(rendered));
    }
    CommandResult::json(
        &InboxOutputV2 {
            ok: true,
            room,
            participant: participant.id.clone(),
            unread,
            count,
            skipped_unreadable: 0,
            unread_count: count,
            pending,
            pending_by_address,
        },
        pretty,
    )
}

fn list_unbound(context: &Context, args: InboxArgs, pretty: bool) -> AppResult<CommandResult> {
    let rooms = context.load_rooms()?;
    let room = context.resolved_room(args.room, &rooms)?;
    let address = Address {
        kind: AddressKind::Workspace,
        name: room.clone(),
    };
    let pending = routing::pending_count(context, &address)?;
    if args.text {
        return Ok(CommandResult::success(format!(
            "participant: unbound (run: post participant bind)\npost: inbox for {} (pending {pending}; unread unavailable)\n",
            output::sanitize_text_header(&room)
        )));
    }
    let mut pending_by_address = BTreeMap::new();
    pending_by_address.insert(address_label(&address), pending);
    CommandResult::json(
        &InboxOutputV2 {
            ok: true,
            room,
            participant: "unbound".to_owned(),
            unread: Vec::new(),
            count: 0,
            skipped_unreadable: 0,
            unread_count: 0,
            pending,
            pending_by_address,
        },
        pretty,
    )
}

fn adopt(context: &Context, participant: &Participant, pretty: bool) -> AppResult<CommandResult> {
    let lineage = participant.lineage.clone().ok_or_else(|| {
        AppError::invalid_argument(
            "inbox --adopt requires the acting participant to have a current lineage",
        )
    })?;
    let report = routing::route_pending(
        context,
        &Address {
            kind: AddressKind::Lineage,
            name: lineage.clone(),
        },
    )?;
    CommandResult::json(
        &AdoptOutput {
            ok: true,
            participant: participant.id.clone(),
            lineage,
            routed: report.routed.len(),
            pending: report.pending,
        },
        pretty,
    )
}

pub(crate) fn visible_addresses(
    context: &Context,
    participant: &Participant,
) -> AppResult<Vec<Address>> {
    let mut addresses = Vec::new();
    if let Some(workspace) = participant.workspace.as_ref() {
        addresses.push(Address {
            kind: AddressKind::Workspace,
            name: workspace.clone(),
        });
    }
    addresses.push(Address {
        kind: AddressKind::Participant,
        name: participant.id.clone(),
    });
    if let Some(lineage) = participant.lineage.as_ref() {
        addresses.push(Address {
            kind: AddressKind::Lineage,
            name: lineage.clone(),
        });
    }
    addresses.extend(routing::received_addresses(context, participant)?);
    addresses.sort_by(|left, right| {
        left.kind
            .as_str()
            .cmp(right.kind.as_str())
            .then(left.name.cmp(&right.name))
    });
    addresses.dedup_by(|left, right| left == right);
    Ok(addresses)
}

pub(crate) fn address_label(address: &Address) -> String {
    format!("{}:{}", address.kind.as_str(), address.name)
}

pub(crate) fn render_reply_targets(rendered: &mut String, participant: Option<&str>, shared: &str) {
    match participant {
        Some(participant) => rendered.push_str(&format!(
            "  reply_to_participant: {} (local, sender only)\n",
            output::sanitize_text_header(participant)
        )),
        None => {
            rendered.push_str("  reply_to_participant: unavailable (message crossed the bridge)\n")
        }
    }
    rendered.push_str(&format!(
        "  reply_to_shared: {} (shared fan-out)\n",
        output::sanitize_text_header(shared)
    ));
}

#[cfg(test)]
mod profile_render_tests {
    use crate::output::sender_label_quoted;

    #[test]
    fn inbox_line_sender_absent_profile_is_byte_identical() {
        assert_eq!(sender_label_quoted("beta", None, None), "\"beta\"");
    }

    #[test]
    fn inbox_line_sender_renders_stamped_profile() {
        assert_eq!(
            sender_label_quoted("beta", Some("Lantern"), Some("🏮")),
            "🏮 Lantern (\"beta\")"
        );
    }
}
