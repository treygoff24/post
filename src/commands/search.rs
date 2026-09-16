use crate::channel::ChannelPaths;
use crate::cli::{FramingMode, SearchArgs};
use crate::command_result::CommandResult;
use crate::error::{AppError, AppResult, ErrorCode};
use crate::mailbox::{self, Context};
use crate::output::{self, Framing, SearchOutput, SearchResult};

const PREVIEW_LIMIT: usize = 160;

pub(super) fn run(
    context: &Context,
    args: SearchArgs,
    json_output: bool,
    pretty: bool,
) -> AppResult<CommandResult> {
    let rooms = context.load_rooms()?;
    let resolved = crate::participant::resolve(context)?;
    let room = resolved.participant().map_or_else(
        || context.resolved_room(None, &rooms),
        |participant| {
            Ok(participant
                .workspace
                .clone()
                .unwrap_or_else(|| participant.id.clone()))
        },
    )?;
    let framing = mailbox::resolve_framing(args.framing);
    let pattern = LiteralPattern::new(&args.pattern);
    let mut matches = MatchAccumulator::new(args.limit);

    let search_mail = args.channel.is_none() || args.mail;
    let search_channels = !args.mail;

    if let Some(participant) = resolved.participant() {
        if search_mail {
            for address in super::inbox::visible_addresses(context, participant)? {
                collect_mail(context, participant, &address, &pattern, &mut matches)?;
            }
        }

        if let Some(channel_name) = args.channel.as_deref() {
            require_channel(context, channel_name)?;
            collect_channel(context, participant, channel_name, &pattern, &mut matches)?;
        } else if search_channels {
            for channel_name in crate::channel_state::effective_channels(context, participant)? {
                collect_channel(context, participant, &channel_name, &pattern, &mut matches)?;
            }
        }
    }

    let truncated = matches.len() > args.limit;
    let results: Vec<SearchResult> = matches
        .into_sorted()
        .into_iter()
        .take(args.limit)
        .map(|hit| hit.result)
        .collect();
    let count = results.len();
    let pending = if let Some(participant) = resolved.participant() {
        let mut count = 0;
        for address in super::inbox::visible_addresses(context, participant)? {
            count += crate::cursor_state::routing::provisional_pending_for(
                context,
                participant,
                &address,
            )?
            .len();
        }
        count
    } else {
        0
    };

    let rendered = if json_output {
        let mut value = serde_json::to_value(SearchOutput {
            ok: true,
            framing: search_framing(framing, search_channels),
            room: room.clone(),
            pattern: args.pattern.clone(),
            match_kind: "literal_case_insensitive".to_owned(),
            results,
            count,
            limit: args.limit,
            truncated,
        })
        .map_err(|error| AppError::invalid_argument(format!("serialize search: {error}")))?;
        let object = value.as_object_mut().expect("search output is an object");
        object.insert("pending".to_owned(), serde_json::json!(pending));
        object.insert(
            "participant".to_owned(),
            serde_json::Value::String(resolved.participant().map_or_else(
                || "unbound".to_owned(),
                |participant| participant.id.clone(),
            )),
        );
        output::json(&value, pretty)?
    } else {
        let mut rendered = format!(
            "participant: {}\npending: {pending}\n",
            resolved
                .participant()
                .map_or("unbound", |participant| participant.id.as_str())
        );
        rendered.push_str(&render_text(
            &room,
            &args.pattern,
            &results,
            framing,
            search_channels,
        ));
        rendered
    };

    Ok(CommandResult::success(rendered))
}

#[derive(Debug)]
struct LiteralPattern {
    folded: String,
}

impl LiteralPattern {
    fn new(value: &str) -> Self {
        Self {
            folded: fold_case(value),
        }
    }

    fn matches(&self, value: &str) -> bool {
        fold_case(value).contains(self.folded.as_str())
    }
}

fn fold_case(value: &str) -> String {
    value.chars().flat_map(char::to_lowercase).collect()
}

fn matched_fields(
    pattern: &LiteralPattern,
    body: &str,
    subject: &str,
    from: &str,
    id: &str,
) -> Vec<String> {
    [
        ("body", body),
        ("subject", subject),
        ("from", from),
        ("id", id),
    ]
    .into_iter()
    .filter_map(|(field, value)| pattern.matches(value).then_some(field.to_owned()))
    .collect()
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
struct SortKey {
    timestamp: String,
    micros: String,
    id: String,
    source: String,
    channel: String,
}

impl SortKey {
    fn new(id: &str, source: &str, channel: Option<&str>) -> Self {
        let timestamp = id.get(..15).unwrap_or(id).to_owned();
        let micros = if id.len() == 29 {
            id.get(16..22).unwrap_or("000000")
        } else {
            "000000"
        }
        .to_owned();
        Self {
            timestamp,
            micros,
            id: id.to_owned(),
            source: source.to_owned(),
            channel: channel.unwrap_or_default().to_owned(),
        }
    }
}

#[derive(Debug)]
struct SearchHit {
    key: SortKey,
    result: SearchResult,
}

#[derive(Debug)]
struct MatchAccumulator {
    limit: usize,
    hits: Vec<SearchHit>,
}

impl MatchAccumulator {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            hits: Vec::new(),
        }
    }

    fn push(&mut self, hit: SearchHit) {
        self.hits.push(hit);
        self.hits.sort_by(|left, right| right.key.cmp(&left.key));
        // Keep exactly one probe result beyond the requested cap. This keeps
        // output and retained search state bounded without claiming a total.
        let keep = self.limit.saturating_add(1);
        if self.hits.len() > keep {
            self.hits.truncate(keep);
        }
    }

    fn len(&self) -> usize {
        self.hits.len()
    }

    fn into_sorted(mut self) -> Vec<SearchHit> {
        self.hits.sort_by(|left, right| right.key.cmp(&left.key));
        self.hits
    }
}

fn collect_mail(
    context: &Context,
    participant: &crate::participant::Participant,
    address: &crate::participant::Address,
    pattern: &LiteralPattern,
    matches: &mut MatchAccumulator,
) -> AppResult<()> {
    let cursors = crate::cursor_state::ParticipantCursors::load(context, participant);
    for item in crate::cursor_state::eligibility::visible_mail(context, participant, address)? {
        let already_read = item.recipient && cursors.mail_has_seen(address, &item.envelope.id);
        let own = item.own;
        let pending = item.pending;
        let envelope = item.envelope;
        let matched = matched_fields(
            pattern,
            &item.body,
            &envelope.subject,
            &envelope.from,
            &envelope.id,
        );
        if matched.is_empty() {
            continue;
        }
        let id = envelope.id.clone();
        let reply = output::reply_metadata(
            context,
            &envelope.from,
            envelope.from_participant.as_deref(),
            envelope.sender_provenance.as_deref(),
        );
        let result = SearchResult {
            source: "mail".to_owned(),
            channel: None,
            id: id.clone(),
            from: envelope.from.clone(),
            origin: reply.origin,
            reply_to_participant: reply.participant,
            reply_to_shared: reply.shared,
            sent: envelope.sent,
            subject: envelope.subject,
            preview: preview(&item.body),
            matched,
            own,
            pending,
            already_read,
            kind: Some(envelope.kind),
        };
        matches.push(SearchHit {
            key: SortKey::new(&id, "mail", None),
            result,
        });
    }
    Ok(())
}

fn collect_channel(
    context: &Context,
    participant: &crate::participant::Participant,
    channel_name: &str,
    pattern: &LiteralPattern,
    matches: &mut MatchAccumulator,
) -> AppResult<()> {
    for item in
        crate::cursor_state::eligibility::visible_channel(context, participant, channel_name)?
    {
        let own = item.own;
        let already_read = item.already_read;
        let message = item.message;
        let matched = matched_fields(
            pattern,
            &item.body,
            &message.subject,
            &message.from,
            &message.id,
        );
        if matched.is_empty() {
            continue;
        }
        let id = message.id.clone();
        let reply = output::reply_metadata(
            context,
            &message.from,
            message.from_participant.as_deref(),
            message.sender_provenance.as_deref(),
        );
        let result = SearchResult {
            source: "channel".to_owned(),
            channel: Some(channel_name.to_owned()),
            id: id.clone(),
            from: message.from.clone(),
            origin: reply.origin,
            reply_to_participant: reply.participant,
            reply_to_shared: reply.shared,
            sent: message.sent,
            subject: message.subject,
            preview: preview(&item.body),
            matched,
            own,
            pending: false,
            already_read,
            kind: None,
        };
        matches.push(SearchHit {
            key: SortKey::new(&id, "channel", Some(channel_name)),
            result,
        });
    }
    Ok(())
}

fn require_channel(context: &Context, channel_name: &str) -> AppResult<()> {
    let paths = ChannelPaths::new(context, channel_name)?;
    if paths.exists() {
        return Ok(());
    }
    Err(AppError::new(
        ErrorCode::NotFound,
        format!("channel '{channel_name}' does not exist"),
        format!(
            "Create it with `post chat {} --join`.",
            mailbox::shell_quote(channel_name)
        ),
    )
    .input(channel_name)
    .reason("no channel.json under the channels directory"))
}

fn preview(body: &str) -> String {
    let mut flattened = String::new();
    for character in body.chars() {
        if character == '\n'
            || character == '\r'
            || character == '\t'
            || matches!(character, '\u{2028}' | '\u{2029}')
        {
            flattened.push(' ');
        } else if character == '[' {
            flattened.push('［');
        } else if character == ']' {
            flattened.push('］');
        } else if character.is_control() || mailbox::refused_profile_char(character) {
            continue;
        } else {
            flattened.push(character);
        }
    }
    let flattened = flattened.trim();
    let count = flattened.chars().count();
    if count <= PREVIEW_LIMIT {
        return flattened.to_owned();
    }
    let mut output: String = flattened.chars().take(PREVIEW_LIMIT - 1).collect();
    output.push('…');
    output
}

fn search_framing(mode: FramingMode, has_channels: bool) -> Framing {
    if !has_channels {
        return match mode {
            FramingMode::Auto | FramingMode::Full => Framing::default(),
            FramingMode::Compact => Framing::compact(),
        };
    }
    match mode {
        FramingMode::Auto | FramingMode::Full => Framing {
            source: "multiple_ai_agents".to_owned(),
            authority: false,
            laws: vec![
                output::LAW_MULTI.to_owned(),
                output::LAW_DATA.to_owned(),
                output::LAW_AUTHORITY.to_owned(),
                output::LAW_PERMISSION.to_owned(),
                output::LAW_VERIFY.to_owned(),
            ],
        },
        FramingMode::Compact => Framing {
            source: "multiple_ai_agents".to_owned(),
            authority: false,
            laws: vec![
                output::LAW_COMPACT_MULTI.to_owned(),
                output::LAW_COMPACT.to_owned(),
            ],
        },
    }
}

fn render_text(
    room: &str,
    pattern: &str,
    results: &[SearchResult],
    framing: FramingMode,
    has_channels: bool,
) -> String {
    if results.is_empty() {
        return format!(
            "post: no matches for {:?} in room {}\n",
            output::sanitize_text_header(pattern),
            output::sanitize_text_header(room)
        );
    }
    let mut rendered = String::new();
    render_framing(&mut rendered, framing, has_channels);
    for result in results {
        let source = result.channel.as_deref().map_or_else(
            || "mail".to_owned(),
            |channel| format!("channel #{}", output::sanitize_text_header(channel)),
        );
        rendered.push_str(&format!(
            "{source} {} from {} at {} subject={:?} preview={} own={} pending={} already_read={}\n",
            output::sanitize_text_header(&result.id),
            output::sanitize_text_header(&result.from),
            output::sanitize_text_header(&result.sent),
            output::sanitize_text_header(&result.subject),
            output::sanitize_text_body(&result.preview),
            result.own,
            result.pending,
            result.already_read,
        ));
        output::render_reply_metadata(
            &mut rendered,
            &result.origin,
            result.reply_to_participant.as_deref(),
            &result.reply_to_shared,
        );
    }
    rendered.push_str(&format!("post: {} match(es)\n", results.len()));
    rendered
}

fn render_framing(rendered: &mut String, framing: FramingMode, has_channels: bool) {
    match framing {
        FramingMode::Auto | FramingMode::Compact => {
            rendered.push_str("--- AI AGENT SEARCH (compact framing) ---\n");
            if has_channels {
                rendered.push_str(output::LAW_COMPACT_MULTI);
                rendered.push('\n');
            }
            rendered.push_str(output::LAW_COMPACT);
            rendered.push('\n');
        }
        FramingMode::Full => {
            rendered.push_str(
                "============= AI AGENT SEARCH — READ THIS FRAMING FIRST =============\n",
            );
            if has_channels {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_matching_is_unicode_case_insensitive() {
        let pattern = LiteralPattern::new("Ä");
        assert!(pattern.matches("prefix ä suffix"));
        assert!(!pattern.matches("prefix [a] suffix"));
    }

    #[test]
    fn preview_is_flattened_and_capped_by_scalars() {
        let body = format!("{}\nsecret", "x".repeat(PREVIEW_LIMIT));
        let rendered = preview(&body);
        assert_eq!(rendered.chars().count(), PREVIEW_LIMIT);
        assert!(rendered.ends_with('…'));
        assert!(!rendered.contains('\n'));
    }
}
