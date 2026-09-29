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
        let reply = output::mail_reply_metadata(context, &envelope);
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
    #[serde(skip_serializing_if = "Option::is_none")]
    room: Option<String>,
    /// `null` when the session is not bound to a participant.
    participant: Option<String>,
    unread: Vec<InboxItemV2>,
    count: usize,
    skipped_unreadable: usize,
    unread_count: usize,
    pending: usize,
    pending_by_address: BTreeMap<String, usize>,
    held: usize,
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
    let selected_room = args.room.clone();
    let addresses = if let Some(room) = selected_room.as_ref() {
        vec![crate::participant::resolve_target(
            context,
            &format!("workspace:{room}"),
        )?]
    } else {
        visible_addresses(context, participant)?
    };
    let mut unread = Vec::new();
    let mut pending_by_address = BTreeMap::new();
    let mut skipped_unreadable = 0;
    let mut held = 0;
    for address in &addresses {
        let snapshot = eligibility::unread_mail_snapshot(context, participant, address)?;
        skipped_unreadable += snapshot.skipped_unreadable;
        for item in snapshot.items {
            unread.push(InboxItemV2::new(context, item.envelope));
        }
        let pending = routing::provisional_pending_for(context, participant, address)?.len();
        pending_by_address.insert(address_label(address), pending);
        held += routing::held_for(context, participant, address)?.len();
    }
    unread.sort_by(|left, right| left.id.cmp(&right.id));
    unread.dedup_by(|left, right| left.id == right.id);
    let count = unread.len();
    let pending = pending_by_address.values().sum();
    let room = selected_room.unwrap_or_else(|| {
        participant
            .workspace
            .clone()
            .unwrap_or_else(|| participant.id.clone())
    });
    if args.text {
        let mut rendered = format!(
            "participant: {}\npost: inbox for {} ({count} unread; pending {pending}; held {held}; skipped unreadable {skipped_unreadable})\n",
            output::sanitize_text_header(&participant.id),
            output::sanitize_text_header(&room)
        );
        for mail in unread {
            let subject = if mail.subject.is_empty() {
                String::new()
            } else {
                format!("  {:?}", mail.subject)
            };
            let sender = output::sender_label_quoted(output::SenderAttribution {
                from: &mail.from,
                from_participant: mail.from_participant.as_deref(),
                from_host: None,
                from_lineage: mail.from_lineage.as_deref(),
                display_name: mail.display_name.as_deref(),
                pfp: mail.pfp.as_deref(),
            });
            rendered.push_str(&format!(
                "{}  [{}] from {}{}\n",
                output::sanitize_text_header(&mail.id),
                mail.kind,
                sender,
                subject
            ));
            render_reply_targets(
                &mut rendered,
                &mail.origin,
                mail.reply_to_participant.as_deref(),
                &mail.reply_to_shared,
            );
        }
        return Ok(CommandResult::success(rendered));
    }
    CommandResult::json(
        &InboxOutputV2 {
            ok: true,
            room: Some(room),
            participant: Some(participant.id.clone()),
            unread,
            count,
            skipped_unreadable,
            unread_count: count,
            pending,
            pending_by_address,
            held,
        },
        pretty,
    )
}

/// An unbound session has no addresses, so nothing is unread for it. With no
/// `--room` it gets the unbound marker (empty lists, `participant: null`); the
/// working directory never picks a room for it. An explicit `--room` is a
/// command sink asking about that room's own pending mail, so the summary is
/// still answered, with the same marker fields alongside.
fn list_unbound(context: &Context, args: InboxArgs, pretty: bool) -> AppResult<CommandResult> {
    let Some(requested) = args.room else {
        if args.text {
            return Ok(CommandResult::success(format!(
                "post: {}\n",
                super::unbound_hint()
            )));
        }
        return CommandResult::json(
            &empty_unbound_output(None, 0, 0, 0, BTreeMap::new()),
            pretty,
        );
    };
    let rooms = context.load_rooms()?;
    let room = context.resolved_room(Some(requested), &rooms)?;
    let address = Address {
        kind: AddressKind::Workspace,
        name: room.clone(),
    };
    let pending_summary = routing::pending_summary(context, &address)?;
    let pending = pending_summary.pending.len();
    let held = pending_summary.held.len();
    let skipped_unreadable = pending_summary.unreadable.len();
    if args.text {
        return Ok(CommandResult::success(format!(
            "post: {}\npost: inbox for {} (pending {pending}; held {held}; skipped unreadable {skipped_unreadable}; unread unavailable)\n",
            super::unbound_hint(),
            output::sanitize_text_header(&room)
        )));
    }
    let mut pending_by_address = BTreeMap::new();
    pending_by_address.insert(address_label(&address), pending);
    CommandResult::json(
        &empty_unbound_output(
            Some(room),
            skipped_unreadable,
            pending,
            held,
            pending_by_address,
        ),
        pretty,
    )
}

fn empty_unbound_output(
    room: Option<String>,
    skipped_unreadable: usize,
    pending: usize,
    held: usize,
    pending_by_address: BTreeMap<String, usize>,
) -> InboxOutputV2 {
    InboxOutputV2 {
        ok: true,
        room,
        participant: None,
        unread: Vec::new(),
        count: 0,
        skipped_unreadable,
        unread_count: 0,
        pending,
        pending_by_address,
        held,
    }
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
    Ok(visible_addresses_among(
        participant,
        routing::received_addresses(context, participant)?,
    ))
}

/// `visible_addresses` from the participant's already-resolved received
/// addresses (`post who` resolves every participant's in one pass).
pub(crate) fn visible_addresses_among(
    participant: &Participant,
    received: Vec<Address>,
) -> Vec<Address> {
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
    addresses.extend(received);
    addresses.sort_by(|left, right| {
        left.kind
            .as_str()
            .cmp(right.kind.as_str())
            .then(left.name.cmp(&right.name))
    });
    addresses.dedup_by(|left, right| left == right);
    addresses
}

pub(crate) fn address_label(address: &Address) -> String {
    format!("{}:{}", address.kind.as_str(), address.name)
}

pub(crate) fn render_reply_targets(
    rendered: &mut String,
    origin: &str,
    participant: Option<&str>,
    shared: &str,
) {
    output::render_reply_metadata(rendered, origin, participant, shared);
}
