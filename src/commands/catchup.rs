use crate::channel::{self, ChannelPaths};
use crate::cli::{CatchupArgs, FramingMode};
use crate::command_result::CommandResult;
use crate::cursor_state::{self, Delta, MailMove, Snapshot};
use crate::error::{AppError, AppResult, ErrorCode};
use crate::mailbox::{self, Context};
use crate::model::ChannelMessage;
use crate::output::{self, CatchupMailItem, CatchupOutput, CatchupTarget, ChatMessageItem};
use std::fs;
use std::path::{Path, PathBuf};

pub(super) fn run(
    context: &Context,
    args: CatchupArgs,
    json_output: bool,
    pretty: bool,
) -> AppResult<CommandResult> {
    let rooms = context.load_rooms()?;
    let (room, _) = channel::acting_room(context, &rooms)?;
    let framing = mailbox::resolve_framing(args.framing);
    let snapshot = Snapshot::load(context, &room);

    let selector = if args.mail {
        Selector::Mail
    } else if let Some(channel) = args.channel {
        Selector::Channel(channel)
    } else {
        // No selector is intentionally the same operation as --all.
        Selector::All
    };
    let selector_for_refusal = selector.clone();

    let mut delta = Delta::default();
    let mut targets = Vec::new();

    match selector {
        Selector::Mail => {
            let (messages, moves) = collect_mail(context, &room, &snapshot)?;
            delta.mail_moves = moves;
            targets.push(CatchupTarget::Mail {
                count: messages.len(),
                framing: mail_framing(framing),
                messages,
            });
        }
        Selector::Channel(channel_name) => {
            let paths = member_channel_paths(context, &channel_name, &room)?;
            let owner = mailbox::resolve_owner(context)?;
            let (messages, ids) =
                collect_channel(&room, &channel_name, &paths, &snapshot, owner.as_ref())?;
            if !ids.is_empty() {
                delta.channel_seen.push((channel_name.clone(), ids));
            }
            targets.push(CatchupTarget::Channel {
                channel: channel_name,
                count: messages.len(),
                framing: channel_framing(framing),
                messages,
            });
        }
        Selector::All => {
            let joined = joined_channels(context, &room)?;
            let owner = if joined.is_empty() {
                None
            } else {
                mailbox::resolve_owner(context)?
            };
            let (messages, moves) = collect_mail(context, &room, &snapshot)?;
            delta.mail_moves = moves;
            targets.push(CatchupTarget::Mail {
                count: messages.len(),
                framing: mail_framing(framing),
                messages,
            });
            for (channel_name, paths) in joined {
                let (messages, ids) =
                    collect_channel(&room, &channel_name, &paths, &snapshot, owner.as_ref())?;
                if !ids.is_empty() {
                    delta.channel_seen.push((channel_name.clone(), ids));
                }
                targets.push(CatchupTarget::Channel {
                    channel: channel_name,
                    count: messages.len(),
                    framing: channel_framing(framing),
                    messages,
                });
            }
        }
    }

    let count = targets.iter().map(CatchupTarget::count).sum();
    if count > 0 && output::stdout_is_null_device() {
        return Err(null_stdout_refusal(&selector_for_refusal, count));
    }

    let rendered = if json_output {
        output::json(
            &CatchupOutput {
                ok: true,
                room: room.clone(),
                targets,
                count,
            },
            pretty,
        )?
    } else {
        render_text(&room, &targets, count, framing)
    };

    if delta.mail_moves.is_empty() && delta.channel_seen.is_empty() {
        return Ok(CommandResult::success(rendered));
    }

    let context = context.clone();
    Ok(CommandResult::after_stdout(rendered, move || {
        cursor_state::consume(&context, &room, delta)
    }))
}

#[derive(Debug, Clone)]
enum Selector {
    Mail,
    Channel(String),
    All,
}

fn mail_framing(mode: FramingMode) -> output::Framing {
    match mode {
        FramingMode::Auto | FramingMode::Full => output::Framing::default(),
        FramingMode::Compact => output::Framing::compact(),
    }
}

fn channel_framing(mode: FramingMode) -> output::ChannelFraming {
    match mode {
        FramingMode::Auto | FramingMode::Full => output::ChannelFraming::default(),
        FramingMode::Compact => output::ChannelFraming::compact(),
    }
}

fn collect_mail(
    context: &Context,
    room: &str,
    snapshot: &Snapshot,
) -> AppResult<(Vec<CatchupMailItem>, Vec<MailMove>)> {
    let (inbox, read) = context.mailbox_dirs(room)?;
    let mut messages = Vec::new();
    let mut moves = Vec::new();
    for path in mailbox::mail_files(&inbox)? {
        let Some(filename_id) = path.file_stem().and_then(|value| value.to_str()) else {
            warn_mail(
                &path,
                &AppError::config(&path, "mail filename is not valid UTF-8"),
            );
            continue;
        };
        // A duplicate left in inbox after a partial move is already consumed
        // once its id is in mail.seen. Do not reparse a known duplicate.
        if snapshot.mail_has_seen(filename_id) {
            continue;
        }
        let parsed = match mailbox::parse_mail(&path) {
            Ok(parsed) => parsed,
            Err(error) => {
                warn_mail(&path, &error);
                continue;
            }
        };
        let id = parsed.envelope.id.clone();
        messages.push(CatchupMailItem {
            envelope: parsed.envelope,
            body: parsed.body,
        });
        moves.push(MailMove {
            id: id.clone(),
            source: path,
            destination: read.join(format!("{id}.mail")),
        });
    }
    Ok((messages, moves))
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

fn member_channel_paths(
    context: &Context,
    channel_name: &str,
    room: &str,
) -> AppResult<ChannelPaths> {
    let paths = ChannelPaths::new(context, channel_name)?;
    let quoted = mailbox::shell_quote(channel_name);
    if !paths.exists() {
        return Err(AppError::new(
            ErrorCode::NotFound,
            format!("channel '{channel_name}' does not exist"),
            format!("Create it with `post chat {quoted} --join`."),
        )
        .input(channel_name)
        .reason("no channel.json under the channels directory"));
    }
    let members = paths.load_members()?;
    if !members.contains_key(room) {
        return Err(AppError::new(
            ErrorCode::NotAMember,
            format!("room '{room}' is not a member of channel '{channel_name}'"),
            format!("Join first with `post chat {quoted} --join`, then retry the read."),
        )
        .input(room)
        .reason("reader is absent from members.json"));
    }
    Ok(paths)
}

fn joined_channels(context: &Context, room: &str) -> AppResult<Vec<(String, ChannelPaths)>> {
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
        let members = paths.load_members()?;
        if members.contains_key(room) {
            channels.push((name, paths));
        }
    }
    channels.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(channels)
}

fn collect_channel(
    room: &str,
    channel_name: &str,
    paths: &ChannelPaths,
    snapshot: &Snapshot,
    owner: Option<&crate::mailbox::ResolvedOwner>,
) -> AppResult<(Vec<ChatMessageItem>, Vec<String>)> {
    let mut messages = Vec::new();
    let mut seen_ids = Vec::new();
    for path in channel_message_files(&paths.messages)? {
        let Some(filename_id) = path.file_stem().and_then(|value| value.to_str()) else {
            return Err(channel::parse_channel_message(&path)
                .expect_err("non-UTF-8 filename must fail closed"));
        };
        if snapshot.channel_has_seen(channel_name, filename_id) {
            continue;
        }
        let parsed = channel::parse_channel_message(&path)?;
        if parsed.message.from == room {
            continue;
        }
        let crate::model::ParsedChannelMessage { message, body } = parsed;
        let signed_verified = mailbox::signed_status(owner, &message, &body, channel_name)
            .map(|status| matches!(status, mailbox::SignedStatus::Verified { .. }));
        seen_ids.push(message.id.clone());
        messages.push(ChatMessageItem {
            message,
            body,
            signed_verified,
        });
    }
    messages.sort_by(|left, right| left.message.id.cmp(&right.message.id));
    seen_ids.sort();
    Ok((messages, seen_ids))
}

fn channel_message_files(directory: &Path) -> AppResult<Vec<PathBuf>> {
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
    room: &str,
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
                rendered.push_str(&format!("=== mail ({count} unread) ===\n"));
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
                    "=== #{} ({count} unread; reading as {}) ===\n",
                    output::sanitize_text_header(channel),
                    output::sanitize_text_header(room)
                ));
                for item in messages {
                    render_channel_item(&mut rendered, item);
                }
            }
            _ => {}
        }
    }
    rendered.push_str(&format!("post: caught up ({count} unread)\n"));
    rendered
}

fn render_framing(rendered: &mut String, framing: FramingMode, has_channel: bool) {
    match framing {
        FramingMode::Auto | FramingMode::Compact => {
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
    let subject = if envelope.subject.is_empty() {
        String::new()
    } else {
        format!(
            "   Subject: {}",
            output::sanitize_text_header(&envelope.subject)
        )
    };
    rendered.push_str(&format!(
        "--- {}   {}   {}{subject} ---\n",
        output::sender_label(
            &envelope.from,
            envelope.display_name.as_deref(),
            envelope.pfp.as_deref()
        ),
        output::sanitize_text_header(&envelope.sent),
        output::sanitize_text_header(&envelope.id),
    ));
    if let Some(sentence) = envelope
        .sender_provenance
        .as_deref()
        .and_then(output::provenance_sentence)
    {
        rendered.push_str(&format!("Sender evidence: {sentence}\n"));
    }
    if let Some(address) = envelope.sender_address.as_deref() {
        rendered.push_str(&format!(
            "Sender address: {} (self-declared instance tag, opaque and non-routable)\n",
            output::sanitize_text_header(address)
        ));
    }
    render_gutter_body(rendered, &item.body);
}

fn render_channel_item(rendered: &mut String, item: &ChatMessageItem) {
    let message: &ChannelMessage = &item.message;
    let event = message
        .event
        .as_deref()
        .map(|event| format!("[{event}] "))
        .unwrap_or_default();
    let subject = if message.subject.is_empty() {
        String::new()
    } else {
        format!(
            "   Subject: {}",
            output::sanitize_text_header(&message.subject)
        )
    };
    rendered.push_str(&format!(
        "--- {event}{}   {}   {}{subject} ---\n",
        output::sender_label(
            &message.from,
            message.display_name.as_deref(),
            message.pfp.as_deref()
        ),
        output::sanitize_text_header(&message.sent),
        output::sanitize_text_header(&message.id),
    ));
    if let Some(sentence) = message
        .sender_provenance
        .as_deref()
        .and_then(output::provenance_sentence)
    {
        rendered.push_str(&format!("[sender evidence: {sentence}]\n"));
    }
    if let Some(address) = message.sender_address.as_deref() {
        rendered.push_str(&format!(
            "[sender address: {} — self-declared instance tag, opaque and non-routable]\n",
            output::sanitize_text_header(address)
        ));
    }
    render_gutter_body(rendered, &item.body);
}

/// COORD-B2-1 (Option B ruling): every body line in catchup's multiplexed
/// stream sits behind this gutter, so untrusted body content can never reach
/// column 0 and forge a `=== ... ===` section marker or `--- ... ---` message
/// header. Single-source surfaces (read, chat) deliberately do not gutter;
/// CONTRACT.md records the divergence.
const BODY_GUTTER: &str = "  | ";

fn render_gutter_body(rendered: &mut String, body: &str) {
    let sanitized = output::sanitize_text_body(body);
    let trimmed = sanitized.strip_suffix('\n').unwrap_or(&sanitized);
    for line in trimmed.split('\n') {
        rendered.push_str(BODY_GUTTER);
        rendered.push_str(line);
        rendered.push('\n');
    }
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

    #[test]
    fn empty_text_has_no_banner() {
        let targets = vec![CatchupTarget::Mail {
            framing: output::Framing::default(),
            messages: Vec::new(),
            count: 0,
        }];
        assert_eq!(
            render_text("alpha", &targets, 0, FramingMode::Auto),
            "post: caught up (0 unread)\n"
        );
    }

    #[test]
    fn nonempty_text_has_one_compact_banner() {
        let targets = vec![CatchupTarget::Mail {
            framing: output::Framing::default(),
            messages: Vec::new(),
            count: 1,
        }];
        let rendered = render_text("alpha", &targets, 1, FramingMode::Auto);
        assert_eq!(rendered.matches("AI AGENT CATCHUP").count(), 1);
        assert!(rendered.contains(output::LAW_COMPACT));
    }
}
