use crate::channel::{list_channels, message_files, parse_channel_message, ChannelPaths};
use crate::cli::WatchArgs;
use crate::command_result::CommandResult;
use crate::error::{AppError, AppResult};
use crate::mailbox::{mail_files, parse_mail, Context};
use crate::output::{InboxItem, WatchEvent};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub(super) fn run(context: &Context, args: WatchArgs) -> AppResult<CommandResult> {
    let (room, inbox, _) = context.resolved_mailbox_dirs(args.room)?;
    // A typo'd --room silently watches a fresh empty mailbox forever, so
    // unlike inbox (whose empty listing is immediately visible) watch warns.
    if !context.load_rooms()?.contains_key(&room) {
        eprintln!("post: warning: room '{room}' is not registered; watching a new empty mailbox");
    }
    let mut seen: HashSet<PathBuf> = HashSet::new();
    // Prime the channel backlog WITHOUT emitting. channels/<name>/messages/
    // is the full append-only history (unlike the inbox, which holds only
    // unread), so a doorbell that replayed it on every startup would be pure
    // noise. Watch surfaces channel messages that ARRIVE while it runs; the
    // reader's cursor owns the backlog. The room inbox keeps its existing
    // surface-on-startup behavior — its volume is bounded to unread.
    for path in room_channel_message_paths(context, &room) {
        seen.insert(path);
    }
    let mut scan_failing = false;
    loop {
        // A doorbell that dies is silently useless: transient scan failures
        // (mailbox trashed and recreated, permission blips) degrade to an
        // empty batch and polling continues. Only stdout failure is fatal —
        // if events can't reach the consumer, exiting IS the notification.
        let batch = match scan_batch(context, &room, &inbox, &mut seen) {
            Ok(batch) => {
                if scan_failing {
                    scan_failing = false;
                    eprintln!("post: warning: watch scan recovered for room '{room}'");
                }
                batch
            }
            Err(error) => {
                if !scan_failing {
                    scan_failing = true;
                    eprintln!(
                        "post: warning: watch scan failed for room '{room}' (will keep polling): {}",
                        error.message
                    );
                }
                Vec::new()
            }
        };
        if !batch.is_empty() {
            emit(&batch, args.text)?;
            if args.once {
                return Ok(CommandResult::success(String::new()));
            }
        }
        std::thread::sleep(Duration::from_millis(args.interval_ms));
    }
}

fn scan_batch(
    context: &Context,
    room: &str,
    inbox: &Path,
    seen: &mut HashSet<PathBuf>,
) -> AppResult<Vec<WatchEvent>> {
    let mut batch = Vec::new();
    for path in mail_files(inbox)? {
        if !seen.insert(path.clone()) {
            continue;
        }
        match parse_mail(&path) {
            Ok(mail) => batch.push(WatchEvent::mail(room, InboxItem::from(mail.envelope))),
            // Consumed by a concurrent read between scan and parse: no longer unread.
            Err(_) if !path.exists() => {}
            Err(error) => {
                // Debug-quote both the path AND the message: a crafted
                // filename rides into the error text too, and neither may
                // inject lines into the warning stream.
                eprintln!(
                    "post: warning: unreadable mail {:?}: {:?}",
                    path.display().to_string(),
                    error.message
                );
                // Ring anyway — a malformed delivery must not silence the
                // doorbell — but echo nothing from the file except its name.
                let id = path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .unwrap_or("<non-utf8 filename>")
                    .to_owned();
                batch.push(WatchEvent::unreadable(room, id));
            }
        }
    }
    // Channels the room belongs to. NEVER touches a cursor — a doorbell
    // notifies, it does not consume (contract 013246 watch invariant). New
    // paths only: the backlog was primed into `seen` before the loop.
    for path in room_channel_message_paths(context, room) {
        if !seen.insert(path.clone()) {
            continue;
        }
        match parse_channel_message(&path) {
            Ok(parsed) => batch.push(WatchEvent::channel_message(parsed.message)),
            // Channel messages are append-only and never moved, but a send
            // caught mid-write can momentarily fail to parse; ring anyway,
            // echoing only the filename-derived id — same discipline as mail.
            Err(_) if !path.exists() => {}
            Err(error) => {
                eprintln!(
                    "post: warning: unreadable channel message {:?}: {:?}",
                    path.display().to_string(),
                    error.message
                );
                let id = path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .unwrap_or("<non-utf8 filename>")
                    .to_owned();
                batch.push(WatchEvent::unreadable(room, id));
            }
        }
    }
    Ok(batch)
}

/// Every `.msg` path across the channels `room` belongs to. Best-effort: a
/// transient read error degrades to fewer paths, never a killed doorbell
/// (same posture as the inbox scan's degrade-and-keep-polling). ponytail:
/// re-enumerates via list_channels each call; fine for a handful of
/// channels, revisit with a lighter membership scan if that grows.
fn room_channel_message_paths(context: &Context, room: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let Ok(summaries) = list_channels(context) else {
        return paths;
    };
    for summary in summaries {
        if !summary.members.contains_key(room) {
            continue;
        }
        if let Ok(channel) = ChannelPaths::new(context, &summary.info.name) {
            if let Ok(files) = message_files(&channel.messages) {
                paths.extend(files);
            }
        }
    }
    paths
}

fn emit(batch: &[WatchEvent], text: bool) -> AppResult<()> {
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    for event in batch {
        let line = if text {
            event.text_line()
        } else {
            crate::output::json(event, false)?
        };
        output
            .write_all(line.as_bytes())
            .and_then(|_| output.flush())
            .map_err(|error| AppError::io("write watch event", Path::new("<stdout>"), error))?;
    }
    Ok(())
}
