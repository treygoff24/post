#[cfg(test)]
use crate::channel;
use crate::channel::ChannelPaths;
use crate::cli::{CatchupArgs, FramingMode};
use crate::command_result::CommandResult;
use crate::cursor_state::{self, Delta, MailMove, ParticipantCursors};
use crate::error::{AppError, AppResult, ErrorCode};
use crate::mailbox::{self, Context};
use crate::model::ChannelMessage;
use crate::output::{self, CatchupMailItem, CatchupOutput, CatchupTarget, ChatMessageItem};
use serde::Serialize;
use std::collections::BTreeMap;

pub(super) fn run(
    context: &Context,
    args: CatchupArgs,
    json_output: bool,
    pretty: bool,
) -> AppResult<CommandResult> {
    let participant = context.sender()?.participant;
    let room = participant
        .workspace
        .clone()
        .unwrap_or_else(|| participant.id.clone());
    cursor_state::routing::route_for_participant(context, &participant)?;
    let framing = mailbox::resolve_framing(args.framing);

    let selector = if args.mail {
        Selector::Mail
    } else if let Some(channel) = args.channel {
        Selector::Channel(crate::channel::strip_channel_sigil(context, &channel))
    } else {
        // No selector is intentionally the same operation as --all.
        Selector::All
    };
    let selector_for_refusal = selector.clone();

    let (rendered, delta, mail_addresses) = {
        let _read_only = mailbox::enter_read_only_command(true);
        let mut delta = Delta::default();
        let mut mail_addresses = BTreeMap::new();
        let mut targets = Vec::new();
        // Message files a scan could not parse: skipped, reported once.
        let mut skipped: Vec<crate::channel::SkippedFile> = Vec::new();

        match selector {
            Selector::Mail => {
                let (messages, moves, addresses) = collect_mail(context, &participant)?;
                delta.mail_moves = moves;
                mail_addresses = addresses;
                targets.push(CatchupTarget::Mail {
                    count: messages.len(),
                    framing: mail_framing(framing),
                    messages,
                    selected_count: None,
                    has_more: None,
                });
            }
            Selector::Channel(channel_name) => {
                let paths = member_channel_paths(context, &channel_name, &participant)?;
                let owner = mailbox::resolve_owner(context)?;
                let (messages, ids) = collect_channel(
                    context,
                    &participant,
                    &channel_name,
                    &paths,
                    owner.as_ref(),
                    &mut skipped,
                )?;
                if !ids.is_empty() {
                    delta.channel_seen.push((channel_name.clone(), ids));
                }
                targets.push(CatchupTarget::Channel {
                    channel: channel_name,
                    count: messages.len(),
                    framing: channel_framing(framing),
                    messages,
                    selected_count: None,
                    has_more: None,
                });
            }
            Selector::All => {
                let joined = joined_channels(context, &participant)?;
                let owner = if joined.is_empty() {
                    None
                } else {
                    mailbox::resolve_owner(context)?
                };
                let (messages, moves, addresses) = collect_mail(context, &participant)?;
                delta.mail_moves = moves;
                mail_addresses = addresses;
                targets.push(CatchupTarget::Mail {
                    count: messages.len(),
                    framing: mail_framing(framing),
                    messages,
                    selected_count: None,
                    has_more: None,
                });
                for (channel_name, paths) in joined {
                    let (messages, ids) = match collect_channel(
                        context,
                        &participant,
                        &channel_name,
                        &paths,
                        owner.as_ref(),
                        &mut skipped,
                    ) {
                        Ok(result) => result,
                        Err(error) => {
                            eprintln!(
                                "post: warning: skipped channel {:?}: {}",
                                channel_name, error.message
                            );
                            (Vec::new(), Vec::new())
                        }
                    };
                    if !ids.is_empty() {
                        delta.channel_seen.push((channel_name.clone(), ids));
                    }
                    targets.push(CatchupTarget::Channel {
                        channel: channel_name,
                        count: messages.len(),
                        framing: channel_framing(framing),
                        messages,
                        selected_count: None,
                        has_more: None,
                    });
                }
            }
        }

        let selected_count = targets.iter().map(CatchupTarget::count).sum();
        let rendered = match args.max_bytes {
            Some(max_bytes) => {
                let remainders =
                    CatchupRemainderIndex::new(context, &participant, &targets, &room, max_bytes)?;
                // The skipped-file list grows with the number of corrupt files and
                // the budget does not: a bounded read carries a count and the
                // first few ids, and names the command that lists them all.
                let list_all = "post channels --json";
                let admission = if json_output {
                    let json_sizes = CatchupJsonSizes::new(&targets, framing, pretty)?;
                    super::byte_budget::admit_with_skipped_detail(
                        !skipped.is_empty(),
                        selected_count,
                        |detail| {
                            let report =
                                crate::channel::BoundedSkipped::new(&skipped, detail, list_all);
                            super::byte_budget::admit_prefix_measured(
                                selected_count,
                                max_bytes,
                                |count| {
                                    measure_budgeted_catchup_json(
                                        &room,
                                        &targets,
                                        count,
                                        max_bytes,
                                        pretty,
                                        &json_sizes,
                                        &remainders,
                                        &report,
                                    )
                                },
                                |count| {
                                    render_budgeted_catchup_json(
                                        &room,
                                        &targets,
                                        count,
                                        framing,
                                        max_bytes,
                                        pretty,
                                        &remainders,
                                        &report,
                                    )
                                },
                            )
                        },
                    )?
                } else {
                    let text_sizes = CatchupTextSizes::new(&targets);
                    // The skipped-files line rides in front of the body, so it
                    // is charged to the byte budget in measure and render alike,
                    // and it shrinks before it would cost a message.
                    super::byte_budget::admit_with_skipped_detail(
                        !skipped.is_empty(),
                        selected_count,
                        |detail| {
                            let notice =
                                crate::channel::bounded_skipped_notice(&skipped, detail, list_all)
                                    .unwrap_or_default();
                            super::byte_budget::admit_prefix_measured(
                                selected_count,
                                max_bytes,
                                |count| {
                                    Ok(notice.len().saturating_add(measure_budgeted_catchup_text(
                                        &room,
                                        &targets,
                                        count,
                                        framing,
                                        max_bytes,
                                        &text_sizes,
                                        &remainders,
                                    )))
                                },
                                |count| {
                                    Ok(format!(
                                        "{notice}{}",
                                        render_budgeted_catchup_text(
                                            &room,
                                            &targets,
                                            count,
                                            framing,
                                            max_bytes,
                                            &remainders,
                                        )
                                    ))
                                },
                            )
                        },
                    )?
                };
                if admission.count > 0 && output::stdout_is_null_device() {
                    return Err(null_stdout_refusal(&selector_for_refusal, admission.count));
                }
                let admitted = prefix_counts(&targets, admission.count);
                restrict_delta(&targets, &admitted, &mut delta);
                admission.rendered
            }
            None => {
                if selected_count > 0 && output::stdout_is_null_device() {
                    return Err(null_stdout_refusal(&selector_for_refusal, selected_count));
                }
                if json_output {
                    #[derive(Serialize)]
                    struct CatchupReceipt<'a> {
                        #[serde(flatten)]
                        catchup: CatchupOutput,
                        /// Message files that could not be parsed and were left out.
                        #[serde(skip_serializing_if = "<[_]>::is_empty")]
                        skipped: &'a [crate::channel::SkippedFile],
                    }
                    output::json(
                        &CatchupReceipt {
                            catchup: CatchupOutput {
                                ok: true,
                                room: room.clone(),
                                targets,
                                count: selected_count,
                                selected_count: None,
                                has_more: None,
                                byte_limit: None,
                                omitted: None,
                            },
                            skipped: &skipped,
                        },
                        pretty,
                    )?
                } else {
                    format!(
                        "{}{}",
                        crate::channel::skipped_notice(&skipped).unwrap_or_default(),
                        render_text(&room, &targets, selected_count, framing)
                    )
                }
            }
        };
        (rendered, delta, mail_addresses)
    };

    if delta.mail_moves.is_empty() && delta.channel_seen.is_empty() {
        return Ok(CommandResult::success(rendered));
    }

    let context = context.clone();
    let participant = participant.clone();
    Ok(CommandResult::after_stdout(rendered, move || {
        let mut grouped: BTreeMap<String, (crate::participant::Address, Vec<String>)> =
            BTreeMap::new();
        for mail in &delta.mail_moves {
            if let Some(address) = mail_addresses.get(&mail.id) {
                grouped
                    .entry(super::inbox::address_label(address))
                    .or_insert_with(|| (address.clone(), Vec::new()))
                    .1
                    .push(mail.id.clone());
            }
        }
        for (_, (address, ids)) in grouped {
            ParticipantCursors::consume_mail(&context, &participant, &address, &ids)?;
        }
        for (channel, ids) in delta.channel_seen {
            ParticipantCursors::consume_channel(&context, &participant, &channel, &ids)?;
        }
        Ok(())
    }))
}

#[derive(Debug, Clone)]
enum Selector {
    Mail,
    Channel(String),
    All,
}

#[derive(Serialize)]
struct CatchupBudgetView<'a> {
    ok: bool,
    room: &'a str,
    targets: Vec<CatchupTargetBudgetView<'a>>,
    count: usize,
    selected_count: usize,
    has_more: bool,
    byte_limit: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    omitted: Option<output::ByteOmission>,
    /// Message files that could not be parsed and were left out: the first few
    /// only, with the full count and the command that lists them all beside it
    /// (a bounded read cannot afford an unbounded list).
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    skipped: &'a [crate::channel::SkippedFile],
    #[serde(skip_serializing_if = "Option::is_none")]
    skipped_total: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    skipped_hint: Option<&'a str>,
}

#[derive(Serialize)]
#[serde(tag = "source", rename_all = "snake_case")]
enum CatchupTargetBudgetView<'a> {
    Mail {
        framing: output::Framing,
        messages: &'a [CatchupMailItem],
        count: usize,
        selected_count: usize,
        has_more: bool,
    },
    Channel {
        channel: &'a str,
        framing: output::ChannelFraming,
        messages: &'a [ChatMessageItem],
        count: usize,
        selected_count: usize,
        has_more: bool,
    },
}

struct CatchupJsonSizes {
    target_array_extra: Vec<usize>,
}

impl CatchupJsonSizes {
    fn new(targets: &[CatchupTarget], framing: FramingMode, pretty: bool) -> AppResult<Self> {
        let candidates = targets
            .iter()
            .map(|target| {
                let messages = match target {
                    CatchupTarget::Mail { messages, .. } => {
                        super::byte_budget::JsonArrayPrefix::new(messages, pretty, 4)
                    }
                    CatchupTarget::Channel { messages, .. } => {
                        super::byte_budget::JsonArrayPrefix::new(messages, pretty, 4)
                    }
                }?;
                (0..=target.count())
                    .map(|count| {
                        super::byte_budget::JsonFragment::new(
                            &catchup_target_budget_view(target, count, framing, false),
                            pretty,
                        )
                        .map(|fragment| fragment.with_array_prefix(&messages, count))
                    })
                    .collect::<AppResult<Vec<_>>>()
            })
            .collect::<AppResult<Vec<_>>>()?;

        let target_count = targets.len();
        let array_layout = if target_count == 0 {
            0
        } else if pretty {
            2usize.saturating_mul(target_count - 1).saturating_add(4)
        } else {
            target_count - 1
        };
        let embedded =
            |fragment: &super::byte_budget::JsonFragment| fragment.embedded_bytes(pretty, 4);
        let mut current = candidates
            .iter()
            .map(|target| embedded(&target[0]))
            .fold(0usize, usize::saturating_add);
        let selected_count: usize = targets.iter().map(CatchupTarget::count).sum();
        let mut target_array_extra = Vec::with_capacity(selected_count + 1);
        target_array_extra.push(current.saturating_add(array_layout));
        for target in &candidates {
            for count in 1..target.len() {
                current = current
                    .saturating_sub(embedded(&target[count - 1]))
                    .saturating_add(embedded(&target[count]));
                target_array_extra.push(current.saturating_add(array_layout));
            }
        }
        Ok(Self { target_array_extra })
    }

    fn extra_bytes(&self, admitted_count: usize) -> usize {
        self.target_array_extra[admitted_count]
    }
}

fn catchup_target_budget_view<'a>(
    target: &'a CatchupTarget,
    count: usize,
    framing: FramingMode,
    include_messages: bool,
) -> CatchupTargetBudgetView<'a> {
    match target {
        CatchupTarget::Mail { messages, .. } => CatchupTargetBudgetView::Mail {
            framing: mail_framing(framing),
            messages: if include_messages {
                &messages[..count]
            } else {
                &messages[..0]
            },
            count,
            selected_count: messages.len(),
            has_more: count < messages.len(),
        },
        CatchupTarget::Channel {
            channel, messages, ..
        } => CatchupTargetBudgetView::Channel {
            channel,
            framing: channel_framing(framing),
            messages: if include_messages {
                &messages[..count]
            } else {
                &messages[..0]
            },
            count,
            selected_count: messages.len(),
            has_more: count < messages.len(),
        },
    }
}

#[allow(clippy::too_many_arguments)] // one more borrowed view of the same result
fn render_budgeted_catchup_json(
    room: &str,
    targets: &[CatchupTarget],
    admitted_count: usize,
    framing: FramingMode,
    max_bytes: usize,
    pretty: bool,
    remainders: &CatchupRemainderIndex,
    skipped: &crate::channel::BoundedSkipped,
) -> AppResult<String> {
    output::json(
        &catchup_budget_view(
            room,
            targets,
            admitted_count,
            framing,
            max_bytes,
            remainders,
            skipped,
        ),
        pretty,
    )
}

#[allow(clippy::too_many_arguments)] // one more borrowed view of the same result
fn measure_budgeted_catchup_json(
    room: &str,
    targets: &[CatchupTarget],
    admitted_count: usize,
    max_bytes: usize,
    pretty: bool,
    sizes: &CatchupJsonSizes,
    remainders: &CatchupRemainderIndex,
    skipped: &crate::channel::BoundedSkipped,
) -> AppResult<usize> {
    let selected_count = targets.iter().map(CatchupTarget::count).sum();
    let omitted = remainders.omission(admitted_count);
    output::json_len(
        &CatchupBudgetView {
            ok: true,
            room,
            targets: Vec::new(),
            count: admitted_count,
            selected_count,
            has_more: admitted_count < selected_count,
            byte_limit: max_bytes,
            omitted,
            skipped: &skipped.shown,
            skipped_total: skipped.total,
            skipped_hint: skipped.hint.as_deref(),
        },
        pretty,
    )
    .map(|scaffold| scaffold.saturating_add(sizes.extra_bytes(admitted_count)))
}

fn catchup_budget_view<'a>(
    room: &'a str,
    targets: &'a [CatchupTarget],
    admitted_count: usize,
    framing: FramingMode,
    max_bytes: usize,
    remainders: &CatchupRemainderIndex,
    skipped: &'a crate::channel::BoundedSkipped,
) -> CatchupBudgetView<'a> {
    let admitted = prefix_counts(targets, admitted_count);
    let views = targets
        .iter()
        .zip(&admitted)
        .map(|(target, &count)| catchup_target_budget_view(target, count, framing, true))
        .collect();
    let selected_count = targets.iter().map(CatchupTarget::count).sum();
    let omitted = remainders.omission(admitted_count);
    CatchupBudgetView {
        ok: true,
        room,
        targets: views,
        count: admitted_count,
        selected_count,
        has_more: admitted_count < selected_count,
        byte_limit: max_bytes,
        omitted,
        skipped: &skipped.shown,
        skipped_total: skipped.total,
        skipped_hint: skipped.hint.as_deref(),
    }
}

fn render_budgeted_catchup_text(
    room: &str,
    targets: &[CatchupTarget],
    admitted_count: usize,
    framing: FramingMode,
    max_bytes: usize,
    remainders: &CatchupRemainderIndex,
) -> String {
    let selected_count: usize = targets.iter().map(CatchupTarget::count).sum();
    if selected_count == 0 {
        return format!("post: caught up (0 unread; byte_limit={max_bytes})\n");
    }
    let admitted = prefix_counts(targets, admitted_count);
    let omission = remainders.omission(admitted_count);
    let mut rendered = omission.as_ref().map_or_else(String::new, |omitted| {
        catchup_omission_notice(omitted, admitted_count, max_bytes)
    });
    if admitted_count > 0 {
        let has_channel = targets
            .iter()
            .zip(&admitted)
            .any(|(target, count)| *count > 0 && matches!(target, CatchupTarget::Channel { .. }));
        render_framing(&mut rendered, framing, has_channel);
        for (target, &count) in targets.iter().zip(&admitted) {
            match target {
                CatchupTarget::Mail { messages, .. } if count > 0 => {
                    rendered.push_str(&catchup_mail_header(count, messages.len()));
                    for item in &messages[..count] {
                        render_mail_item(&mut rendered, item);
                    }
                }
                CatchupTarget::Channel {
                    channel, messages, ..
                } if count > 0 => {
                    rendered.push_str(&catchup_channel_header(
                        room,
                        channel,
                        count,
                        messages.len(),
                    ));
                    for item in &messages[..count] {
                        render_channel_item(&mut rendered, item);
                    }
                }
                _ => {}
            }
        }
    }
    rendered.push_str(&catchup_budget_footer(
        admitted_count,
        selected_count,
        max_bytes,
    ));
    rendered
}

fn catchup_omission_notice(
    omitted: &output::ByteOmission,
    admitted_count: usize,
    max_bytes: usize,
) -> String {
    format!(
        "post: catchup shown {admitted_count} complete; {} omitted by shared byte limit {max_bytes}\n\
post: first remainder is {}{} message {} ({} body bytes); {} omitted mention(s)\n\
post: continue with {}\n",
        omitted.count,
        omitted.source,
        omitted
            .channel
            .as_deref()
            .map(|channel| format!(" #{}", output::sanitize_text_header(channel)))
            .unwrap_or_default(),
        output::sanitize_text_header(&omitted.first_id),
        omitted.first_body_bytes,
        omitted.mention_count,
        omitted.continuation,
    )
}

fn catchup_mail_header(count: usize, selected_count: usize) -> String {
    format!("=== mail ({count} complete of {selected_count}) ===\n")
}

fn catchup_channel_header(
    room: &str,
    channel: &str,
    count: usize,
    selected_count: usize,
) -> String {
    format!(
        "=== #{} ({count} complete of {selected_count}; reading as {}) ===\n",
        output::sanitize_text_header(channel),
        output::sanitize_text_header(room)
    )
}

fn catchup_budget_footer(admitted_count: usize, selected_count: usize, max_bytes: usize) -> String {
    if admitted_count < selected_count {
        format!(
            "post: catchup partial ({admitted_count} complete; {} remain unread)\n",
            selected_count - admitted_count
        )
    } else {
        format!(
            "post: caught up ({admitted_count} unread; complete within byte_limit={max_bytes})\n"
        )
    }
}

struct CatchupTextSizes {
    prefixes: Vec<Vec<usize>>,
}

impl CatchupTextSizes {
    fn new(targets: &[CatchupTarget]) -> Self {
        let prefixes = targets
            .iter()
            .map(|target| {
                let mut sizes = vec![0usize];
                match target {
                    CatchupTarget::Mail { messages, .. } => {
                        for item in messages {
                            let mut rendered = String::new();
                            render_mail_item(&mut rendered, item);
                            sizes.push(
                                sizes
                                    .last()
                                    .copied()
                                    .unwrap_or(0)
                                    .saturating_add(rendered.len()),
                            );
                        }
                    }
                    CatchupTarget::Channel { messages, .. } => {
                        for item in messages {
                            let mut rendered = String::new();
                            render_channel_item(&mut rendered, item);
                            sizes.push(
                                sizes
                                    .last()
                                    .copied()
                                    .unwrap_or(0)
                                    .saturating_add(rendered.len()),
                            );
                        }
                    }
                }
                sizes
            })
            .collect();
        Self { prefixes }
    }
}

fn measure_budgeted_catchup_text(
    room: &str,
    targets: &[CatchupTarget],
    admitted_count: usize,
    framing: FramingMode,
    max_bytes: usize,
    sizes: &CatchupTextSizes,
    remainders: &CatchupRemainderIndex,
) -> usize {
    let selected_count: usize = targets.iter().map(CatchupTarget::count).sum();
    if selected_count == 0 {
        return format!("post: caught up (0 unread; byte_limit={max_bytes})\n").len();
    }
    let admitted = prefix_counts(targets, admitted_count);
    let omission = remainders.omission(admitted_count);
    let mut bytes = omission.as_ref().map_or(0, |omitted| {
        catchup_omission_notice(omitted, admitted_count, max_bytes).len()
    });
    if admitted_count > 0 {
        let has_channel = targets
            .iter()
            .zip(&admitted)
            .any(|(target, count)| *count > 0 && matches!(target, CatchupTarget::Channel { .. }));
        let mut banner = String::new();
        render_framing(&mut banner, framing, has_channel);
        bytes += banner.len();
        for (index, (target, &count)) in targets.iter().zip(&admitted).enumerate() {
            if count == 0 {
                continue;
            }
            bytes += match target {
                CatchupTarget::Mail { messages, .. } => {
                    catchup_mail_header(count, messages.len()).len()
                }
                CatchupTarget::Channel {
                    channel, messages, ..
                } => catchup_channel_header(room, channel, count, messages.len()).len(),
            };
            bytes += sizes.prefixes[index][count];
        }
    }
    bytes + catchup_budget_footer(admitted_count, selected_count, max_bytes).len()
}

fn prefix_counts(targets: &[CatchupTarget], mut admitted: usize) -> Vec<usize> {
    targets
        .iter()
        .map(|target| {
            let count = admitted.min(target.count());
            admitted -= count;
            count
        })
        .collect()
}

fn restrict_delta(targets: &[CatchupTarget], admitted: &[usize], delta: &mut Delta) {
    let mail_count: usize = targets
        .iter()
        .zip(admitted)
        .filter_map(|(target, count)| {
            matches!(target, CatchupTarget::Mail { .. }).then_some(*count)
        })
        .sum();
    delta.mail_moves.truncate(mail_count);
    for (channel, ids) in &mut delta.channel_seen {
        let count = targets
            .iter()
            .zip(admitted)
            .find_map(|(target, count)| match target {
                CatchupTarget::Channel {
                    channel: target_channel,
                    ..
                } if target_channel == channel => Some(*count),
                _ => None,
            })
            .unwrap_or(0);
        ids.truncate(count);
    }
    delta.channel_seen.retain(|(_, ids)| !ids.is_empty());
}

enum CatchupRemainderSource {
    Mail,
    Channel(String),
}

struct CatchupRemainderItem {
    source: CatchupRemainderSource,
    id: String,
    body_bytes: usize,
    mentioned: bool,
    remaining_targets: usize,
    continuation: String,
}

struct CatchupRemainderIndex {
    items: Vec<CatchupRemainderItem>,
    mention_suffix: Vec<usize>,
}

impl CatchupRemainderIndex {
    fn new(
        context: &Context,
        participant: &crate::participant::Participant,
        targets: &[CatchupTarget],
        room: &str,
        max_bytes: usize,
    ) -> AppResult<Self> {
        let mention_targets = crate::channel::MentionTargets::of_participant(participant);
        let mut remaining_by_target = vec![0usize; targets.len()];
        let mut remaining_targets = 0usize;
        for (index, target) in targets.iter().enumerate().rev() {
            if target.count() > 0 {
                remaining_targets += 1;
            }
            remaining_by_target[index] = remaining_targets;
        }
        let mut items = Vec::new();
        for (index, target) in targets.iter().enumerate() {
            match target {
                CatchupTarget::Mail { messages, .. } => {
                    for item in messages {
                        let address = item
                            .envelope
                            .address
                            .as_ref()
                            .and_then(output::WatchAddress::to_address);
                        let projection = address.as_ref().map_or_else(
                            || super::read::ReadProjection::legacy(context),
                            |address| {
                                super::read::ReadProjection::participant(
                                    context,
                                    address,
                                    crate::output::mail_authored_locally_by(
                                        context,
                                        &participant.id,
                                        &item.envelope,
                                    ),
                                    item.envelope.pending,
                                    true,
                                )
                            },
                        );
                        items.push(CatchupRemainderItem {
                            source: CatchupRemainderSource::Mail,
                            id: item.envelope.id.clone(),
                            body_bytes: item.body.len(),
                            mentioned: false,
                            remaining_targets: remaining_by_target[index],
                            continuation: super::read::measured_omission_continuation(
                                room,
                                &item.envelope,
                                &item.body,
                                false,
                                max_bytes,
                                projection,
                            )?,
                        });
                    }
                }
                CatchupTarget::Channel {
                    channel, messages, ..
                } => {
                    for item in messages {
                        items.push(CatchupRemainderItem {
                            source: CatchupRemainderSource::Channel(channel.clone()),
                            id: item.message.id.clone(),
                            body_bytes: item.body.len(),
                            mentioned: mention_targets.addressed_by(&item.message, &item.body),
                            remaining_targets: remaining_by_target[index],
                            continuation: super::chat::measured_omission_continuation(
                                context,
                                channel,
                                room,
                                &item.message,
                                &item.body,
                                item.signed_verified,
                                max_bytes,
                            )?,
                        });
                    }
                }
            }
        }
        let mut mention_suffix = vec![0usize; items.len() + 1];
        let mut mentions = 0usize;
        for (index, item) in items.iter().enumerate().rev() {
            mentions += usize::from(item.mentioned);
            mention_suffix[index] = mentions;
        }
        Ok(Self {
            items,
            mention_suffix,
        })
    }

    fn omission(&self, admitted_count: usize) -> Option<output::ByteOmission> {
        let first = self.items.get(admitted_count)?;
        let (source, channel) = match &first.source {
            CatchupRemainderSource::Mail => ("mail", None),
            CatchupRemainderSource::Channel(channel) => ("channel", Some(channel.clone())),
        };
        Some(output::ByteOmission {
            reason: "byte_limit".to_owned(),
            count: self.items.len() - admitted_count,
            source: source.to_owned(),
            channel,
            first_id: first.id.clone(),
            first_body_bytes: first.body_bytes,
            mention_count: self.mention_suffix[admitted_count],
            remaining_targets: Some(first.remaining_targets),
            continuation: first.continuation.clone(),
        })
    }
}

fn mail_framing(mode: FramingMode) -> output::Framing {
    match mode {
        FramingMode::Auto => output::Framing::default(),
        FramingMode::Full => output::Framing::full(),
        FramingMode::Compact => output::Framing::compact(),
    }
}

fn channel_framing(mode: FramingMode) -> output::ChannelFraming {
    match mode {
        FramingMode::Auto => output::ChannelFraming::default(),
        FramingMode::Full => output::ChannelFraming::full(),
        FramingMode::Compact => output::ChannelFraming::compact(),
    }
}

type CollectedMail = (
    Vec<CatchupMailItem>,
    Vec<MailMove>,
    BTreeMap<String, crate::participant::Address>,
);

fn collect_mail(
    context: &Context,
    participant: &crate::participant::Participant,
) -> AppResult<CollectedMail> {
    let mut messages = Vec::new();
    let mut moves = Vec::new();
    let mut addresses = BTreeMap::new();
    for address in super::inbox::visible_addresses(context, participant)? {
        for item in cursor_state::eligibility::unread_mail(context, participant, &address)? {
            let id = item.envelope.id.clone();
            messages.push(CatchupMailItem {
                envelope: output::MessageEnvelope::new(
                    context,
                    item.envelope,
                    false,
                    Some(&address),
                ),
                body: item.body,
            });
            moves.push(MailMove { id: id.clone() });
            addresses.insert(id, address.clone());
        }
    }
    messages.sort_by(|left, right| left.envelope.id.cmp(&right.envelope.id));
    moves.sort_by(|left, right| left.id.cmp(&right.id));
    Ok((messages, moves, addresses))
}

fn member_channel_paths(
    context: &Context,
    channel_name: &str,
    participant: &crate::participant::Participant,
) -> AppResult<ChannelPaths> {
    let paths = ChannelPaths::new(context, channel_name)?;
    let quoted = mailbox::shell_quote(channel_name);
    if !paths.exists() {
        return Err(crate::channel::channel_not_found(
            context,
            channel_name,
            crate::channel::ChannelUse::Read,
        ));
    }
    let membership = crate::channel_state::ParticipantChannels::load(participant)?;
    if !membership.effective(context, participant, channel_name)? {
        return Err(AppError::new(
            ErrorCode::NotAMember,
            format!(
                "participant '{}' is not a member of channel '{channel_name}'",
                participant.id
            ),
            format!("Join first with `post chat {quoted} --join`, then retry the read."),
        )
        .input(participant.id.clone())
        .reason("participant is not an effective channel member"));
    }
    Ok(paths)
}

fn joined_channels(
    context: &Context,
    participant: &crate::participant::Participant,
) -> AppResult<Vec<(String, ChannelPaths)>> {
    let mut channels = Vec::new();
    for name in crate::channel_state::effective_channels(context, participant)? {
        let paths = match ChannelPaths::new(context, &name) {
            Ok(paths) => paths,
            Err(_) => continue,
        };
        if !paths.exists() {
            continue;
        }
        channels.push((name, paths));
    }
    channels.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(channels)
}

fn collect_channel(
    context: &Context,
    participant: &crate::participant::Participant,
    channel_name: &str,
    _paths: &ChannelPaths,
    owner: Option<&crate::mailbox::ResolvedOwner>,
    skipped: &mut Vec<crate::channel::SkippedFile>,
) -> AppResult<(Vec<ChatMessageItem>, Vec<String>)> {
    let mut messages = Vec::new();
    let mut seen_ids = Vec::new();
    let scan = cursor_state::eligibility::unread_channel_with(
        context,
        participant,
        channel_name,
        crate::channel::Scan::Tolerant,
    )?;
    skipped.extend(
        scan.skipped
            .into_iter()
            .map(|file| file.in_channel(channel_name)),
    );
    for item in scan.items {
        let message = item.message;
        let body = item.body;
        let signed_verified = mailbox::signed_status(owner, &message, &body, channel_name)
            .map(|status| matches!(status, mailbox::SignedStatus::Verified { .. }));
        seen_ids.push(message.id.clone());
        messages.push(ChatMessageItem::new(
            context,
            message,
            body,
            signed_verified,
        ));
    }
    messages.sort_by(|left, right| left.message.id.cmp(&right.message.id));
    seen_ids.sort();
    Ok((messages, seen_ids))
}

fn null_stdout_refusal(selector: &Selector, count: usize) -> AppError {
    let selector = match selector {
        Selector::Mail => " --mail".to_owned(),
        Selector::Channel(channel) => format!(" {}", mailbox::shell_quote(channel)),
        Selector::All => String::new(),
    };
    let fix = format!("post catchup{selector}");
    AppError::new(
        ErrorCode::InvalidArgument,
        format!(
            "refusing to consume {count} unread catchup message(s) into /dev/null"
        ),
        format!("Run `{fix}` with stdout connected, or inspect with `post chat`/`post inbox --text` first."),
    )
    .exact_fix(fix)
    .input("stdout")
    .reason("stdout is the null device and this read would advance a cursor")
}

fn render_text(
    _room: &str,
    targets: &[CatchupTarget],
    count: usize,
    framing: FramingMode,
) -> String {
    if count == 0 {
        return "post: caught up (0 unread)\n".to_owned();
    }
    let mut rendered = String::new();
    let has_channel = targets.iter().any(|target| {
        matches!(
            target,
            CatchupTarget::Channel { count, .. } if *count > 0
        )
    });
    render_framing(&mut rendered, framing, has_channel);
    for target in targets {
        match target {
            CatchupTarget::Mail {
                messages, count, ..
            } if *count > 0 => {
                rendered.push_str(&format!("mail · {count} unread\n\n"));
                for item in messages {
                    render_mail_item(&mut rendered, item);
                }
            }
            CatchupTarget::Channel {
                channel,
                messages,
                count,
                ..
            } if *count > 0 => {
                rendered.push_str(&format!(
                    "#{} · {count} unread\n\n",
                    output::sanitize_text_header(channel)
                ));
                for item in messages {
                    render_channel_item(&mut rendered, item);
                }
            }
            _ => {}
        }
    }
    rendered
}

fn render_framing(rendered: &mut String, framing: FramingMode, has_channel: bool) {
    match framing {
        FramingMode::Auto => {}
        FramingMode::Compact => {
            rendered.push_str("--- AI AGENT CATCHUP (compact framing) ---\n");
            if has_channel {
                rendered.push_str(output::LAW_COMPACT_MULTI);
                rendered.push('\n');
            }
            rendered.push_str(output::LAW_COMPACT);
            rendered.push('\n');
        }
        FramingMode::Full => {
            rendered.push_str(
                "============= AI AGENT CATCHUP — READ THIS FRAMING FIRST =============\n",
            );
            if has_channel {
                rendered.push_str(output::LAW_MULTI);
                rendered.push('\n');
            }
            rendered.push_str(output::LAW_DATA);
            rendered.push('\n');
            rendered.push_str(output::LAW_AUTHORITY);
            rendered.push('\n');
            rendered.push_str(output::LAW_PERMISSION);
            rendered.push('\n');
            rendered.push_str(output::LAW_VERIFY);
            rendered.push('\n');
            rendered
                .push_str("====================================================================\n");
        }
    }
}

fn render_mail_item(rendered: &mut String, item: &CatchupMailItem) {
    let envelope = &item.envelope;
    rendered.push_str(&output::message_header(
        &output::sender_label(output::SenderAttribution::from(&envelope.envelope)),
        &envelope.sent,
        &envelope.id,
        output::reply_address(
            item.envelope.reply_to_participant.as_deref(),
            &item.envelope.reply_to_shared,
        ),
        None,
        &envelope.subject,
        Some(&envelope.kind.to_string()),
    ));
    output::render_gutter_body(rendered, &item.body);
    rendered.push('\n');
}

fn render_channel_item(rendered: &mut String, item: &ChatMessageItem) {
    let message: &ChannelMessage = &item.message;
    rendered.push_str(&output::message_header(
        &output::sender_label(output::SenderAttribution::from(message)),
        &message.sent,
        &message.id,
        output::reply_address(item.reply_to_participant.as_deref(), &item.reply_to_shared),
        message.re.as_deref(),
        &message.subject,
        message
            .event
            .as_deref()
            .map(crate::channel::event_label)
            .as_deref(),
    ));
    if let Some(verified) = item.signed_verified {
        rendered.push_str(if verified {
            "[signature verified]\n"
        } else {
            "[SIGNATURE FAILED]\n"
        });
    }
    output::render_gutter_body(rendered, &item.body);
    rendered.push('\n');
}

impl CatchupTarget {
    fn count(&self) -> usize {
        match self {
            Self::Mail { count, .. } | Self::Channel { count, .. } => *count,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{test_root, trash_test_root};
    use std::fs;

    #[test]
    fn selected_delta_does_not_consume_messages_arriving_after_selection() {
        let root = test_root("catchup-delta-fixed");
        let context = Context {
            root: root.clone(),
            home: root.clone(),
        };
        let messages = root.join("channels/tax/messages");
        fs::create_dir_all(&messages).expect("message directory");
        fs::write(
            root.join("channels/tax/channel.json"),
            r#"{"name":"tax","created":"2026-08-20 12:00:00 -0500","created_by":"alpha"}"#,
        )
        .expect("channel info");
        fs::write(
            root.join("channels/tax/members.json"),
            r#"{"alpha":"2026-08-20 12:00:00 -0500"}"#,
        )
        .expect("legacy membership");
        let participant = crate::participant::bind_test_actor(&context, "alpha");
        let first_id = "20260831-171234-000001-a1b2c3";
        let late_id = "20260831-171234-000002-b2c3d4";
        let write_message = |id: &str| {
            let message = ChannelMessage {
                emote: None,
                id: id.to_owned(),
                from: "beta".to_owned(),
                channel: "tax".to_owned(),
                subject: String::new(),
                sent: "2026-08-31 17:12:34 +0000".to_owned(),
                from_participant: None,
                from_host: None,
                from_lineage: None,
                address_kind: None,
                event: None,
                display_name: None,
                pfp: None,
                re: None,
                mentions: Vec::new(),
                signature_ref: None,
                sender_address: None,
                sender_provenance: None,
            };
            fs::write(
                messages.join(format!("{id}.msg")),
                channel::encode_message(&message, "body").expect("encode message"),
            )
            .expect("write message");
        };
        write_message(first_id);

        let paths = ChannelPaths::new(&context, "tax").expect("channel paths");
        let (_selected, selected_ids) =
            collect_channel(&context, &participant, "tax", &paths, None, &mut Vec::new())
                .expect("collect");
        write_message(late_id);

        ParticipantCursors::consume_channel(&context, &participant, "tax", &selected_ids)
            .expect("consume fixed delta");
        let persisted: serde_json::Value = serde_json::from_slice(
            &fs::read(participant.dir.join("cursors.json")).expect("cursor state"),
        )
        .expect("valid cursor state");
        let seen = persisted["channels"]["tax"]["seen"]
            .as_array()
            .expect("channel seen set");
        assert!(seen.iter().any(|id| id == first_id));
        assert!(!seen.iter().any(|id| id == late_id));
        trash_test_root(&root);
    }
}
