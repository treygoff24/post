use crate::channel::{self, ChannelPaths};
use crate::cli::{FramingMode, SearchArgs};
use crate::command_result::CommandResult;
use crate::error::{AppError, AppResult, ErrorCode};
use crate::mailbox::{self, Context};
use crate::model::{ParsedChannelMessage, ParsedMail};
use crate::output::{self, Framing, SearchOutput, SearchResult};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

const PREVIEW_LIMIT: usize = 160;

pub(super) fn run(
    context: &Context,
    args: SearchArgs,
    json_output: bool,
    pretty: bool,
) -> AppResult<CommandResult> {
    let rooms = context.load_rooms()?;
    let (room, _) = channel::acting_room(context, &rooms)?;
    let framing = mailbox::resolve_framing(args.framing);
    let pattern = LiteralPattern::new(&args.pattern);
    let mut matches = MatchAccumulator::new(args.limit);

    let search_mail = args.channel.is_none() || args.mail;
    let search_channels = !args.mail;

    if search_mail {
        collect_mail(context, &room, &pattern, &mut matches)?;
    }

    if let Some(channel_name) = args.channel.as_deref() {
        let paths = member_channel_paths(context, channel_name, &room)?;
        collect_channel(channel_name, &paths, &pattern, &mut matches)?;
    } else if search_channels {
        for (channel_name, paths) in joined_channel_paths(context, &room)? {
            collect_channel(&channel_name, &paths, &pattern, &mut matches)?;
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

    let rendered = if json_output {
        output::json(
            &SearchOutput {
                ok: true,
                framing: search_framing(framing, search_channels),
                room: room.clone(),
                pattern: args.pattern.clone(),
                match_kind: "literal_case_insensitive".to_owned(),
                results,
                count,
                limit: args.limit,
                truncated,
            },
            pretty,
        )?
    } else {
        render_text(&room, &args.pattern, &results, framing, search_channels)
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
    room: &str,
    pattern: &LiteralPattern,
    matches: &mut MatchAccumulator,
) -> AppResult<()> {
    let room_dir = context.root.join(room);
    let archive = context.root.join("archive");
    let directories = [room_dir.join("inbox"), room_dir.join("read"), archive];
    let mut candidates: BTreeMap<String, MailCandidate> = BTreeMap::new();

    for directory in directories {
        for path in mailbox::mail_files(&directory)? {
            let parsed = match mailbox::parse_mail(&path) {
                Ok(parsed) => parsed,
                Err(error) => {
                    warn_mail(&path, &error);
                    continue;
                }
            };
            if parsed.envelope.from != room && parsed.envelope.to != room {
                continue;
            }
            let id = parsed.envelope.id.clone();
            candidates.entry(id).or_insert(MailCandidate { parsed });
        }
    }

    for (_id, candidate) in candidates {
        let ParsedMail { envelope, body } = candidate.parsed;
        let matched = matched_fields(
            pattern,
            &body,
            &envelope.subject,
            &envelope.from,
            &envelope.id,
        );
        if matched.is_empty() {
            continue;
        }
        let id = envelope.id.clone();
        let result = SearchResult {
            source: "mail".to_owned(),
            channel: None,
            id: id.clone(),
            from: envelope.from,
            sent: envelope.sent,
            subject: envelope.subject,
            preview: preview(&body),
            matched,
            kind: Some(envelope.kind),
        };
        matches.push(SearchHit {
            key: SortKey::new(&id, "mail", None),
            result,
        });
    }
    Ok(())
}

#[derive(Debug)]
struct MailCandidate {
    parsed: ParsedMail,
}

fn collect_channel(
    channel_name: &str,
    paths: &ChannelPaths,
    pattern: &LiteralPattern,
    matches: &mut MatchAccumulator,
) -> AppResult<()> {
    for path in message_files(&paths.messages)? {
        let parsed = match channel::parse_channel_message(&path) {
            Ok(parsed) => parsed,
            Err(error) => {
                warn_channel(&path, &error);
                continue;
            }
        };
        if parsed.message.channel != channel_name {
            eprintln!(
                "post: warning: skipped channel message '{}' whose envelope names channel '{}'",
                path.display(),
                parsed.message.channel
            );
            continue;
        }
        push_channel_match(channel_name, parsed, pattern, matches);
    }
    Ok(())
}

fn push_channel_match(
    channel_name: &str,
    parsed: crate::model::ParsedChannelMessage,
    pattern: &LiteralPattern,
    matches: &mut MatchAccumulator,
) {
    let ParsedChannelMessage { message, body } = parsed;
    let matched = matched_fields(pattern, &body, &message.subject, &message.from, &message.id);
    if matched.is_empty() {
        return;
    }
    let id = message.id.clone();
    let result = SearchResult {
        source: "channel".to_owned(),
        channel: Some(channel_name.to_owned()),
        id: id.clone(),
        from: message.from,
        sent: message.sent,
        subject: message.subject,
        preview: preview(&body),
        matched,
        kind: None,
    };
    matches.push(SearchHit {
        key: SortKey::new(&id, "channel", Some(channel_name)),
        result,
    });
}

fn member_channel_paths(
    context: &Context,
    channel_name: &str,
    room: &str,
) -> AppResult<ChannelPaths> {
    let paths = ChannelPaths::new(context, channel_name)?;
    if !paths.exists() {
        return Err(AppError::new(
            ErrorCode::NotFound,
            format!("channel '{channel_name}' does not exist"),
            format!(
                "Create it with `post chat {} --join`.",
                mailbox::shell_quote(channel_name)
            ),
        )
        .input(channel_name)
        .reason("no channel.json under the channels directory"));
    }
    let members = paths.load_members()?;
    if !members.contains_key(room) {
        return Err(AppError::new(
            ErrorCode::NotAMember,
            format!("room '{room}' is not a member of channel '{channel_name}'"),
            format!(
                "Join first with `post chat {} --join`, then retry the search.",
                mailbox::shell_quote(channel_name)
            ),
        )
        .input(room)
        .reason("reader is absent from members.json"));
    }
    Ok(paths)
}

fn joined_channel_paths(context: &Context, room: &str) -> AppResult<Vec<(String, ChannelPaths)>> {
    let directory = context.root.join(channel::CHANNELS_DIR);
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(AppError::io("list channels directory", &directory, error)),
    };
    let mut channels = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|error| AppError::io("read channels entry", &directory, error))?;
        if !entry.path().is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let paths = match ChannelPaths::new(context, &name) {
            Ok(paths) => paths,
            Err(_) => continue,
        };
        if !paths.exists() {
            continue;
        }
        let members = match paths.load_members() {
            Ok(members) => members,
            Err(error) => {
                // A malformed membership document closes only this channel;
                // never enumerate its messages on an uncertain boundary.
                eprintln!(
                    "post: warning: skipped channel '{}' because its membership file is invalid: {}",
                    name, error.message
                );
                continue;
            }
        };
        if members.contains_key(room) {
            channels.push((name, paths));
        }
    }
    channels.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(channels)
}

fn message_files(directory: &Path) -> AppResult<Vec<PathBuf>> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(AppError::io(
                "list channel messages directory",
                directory,
                error,
            ))
        }
    };
    let mut files = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|error| AppError::io("read channel messages entry", directory, error))?;
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|value| value.to_str()) == Some("msg") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn warn_mail(path: &Path, error: &AppError) {
    let kind = if error.code == ErrorCode::IoError {
        "unreadable"
    } else {
        "malformed"
    };
    eprintln!(
        "post: warning: skipped {kind} mail '{}': {}",
        path.display(),
        error.message
    );
}

fn warn_channel(path: &Path, error: &AppError) {
    let kind = if error.code == ErrorCode::IoError {
        "unreadable"
    } else {
        "malformed"
    };
    eprintln!(
        "post: warning: skipped {kind} channel message '{}': {}",
        path.display(),
        error.message
    );
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
            "{source} {} from {} at {} subject={:?} preview={}\n",
            output::sanitize_text_header(&result.id),
            output::sanitize_text_header(&result.from),
            output::sanitize_text_header(&result.sent),
            output::sanitize_text_header(&result.subject),
            output::sanitize_text_body(&result.preview),
        ));
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
