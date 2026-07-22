use crate::channel::{
    channel_state_path, list_channels, message_files, parse_channel_message, ChannelPaths,
    ChannelStateMap,
};
use crate::cli::WatchArgs;
use crate::command_result::CommandResult;
use crate::error::{AppError, AppResult};
use crate::mailbox::{mail_files, parse_mail, Context};
use crate::output::{InboxItem, WatchEvent};
use std::collections::{HashMap, HashSet};
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
    // Ring for anything not yet handled. Load each channel's cursor as a
    // read-only floor — watch NEVER writes a cursor — and emit messages with
    // id > floor. A doorbell rings until handled: reading advances the real
    // cursor, so a handled backlog never re-rings, but a REPLACEMENT doorbell
    // after the original dies mid-session (as ours did, repeatedly) still
    // surfaces anything that landed in the gap, instead of silently priming
    // past it. The room inbox keeps its own surface-on-startup behavior.
    let floors = load_channel_floors(context, &room);
    let mut scan_failing = false;
    loop {
        // A doorbell that dies is silently useless: transient scan failures
        // (mailbox trashed and recreated, permission blips) degrade to an
        // empty batch and polling continues. Only stdout failure is fatal —
        // if events can't reach the consumer, exiting IS the notification.
        let batch = match scan_batch(context, &room, &inbox, &floors, &mut seen) {
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
    floors: &HashMap<String, String>,
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
    // notifies, it does not consume (contract 013246 watch invariant).
    for (channel, path) in room_channel_message_paths(context, room) {
        if !seen.insert(path.clone()) {
            continue;
        }
        // At or below the reader's cursor = already read before watch
        // started; skip without parsing (the filename stem is the id).
        if let (Some(floor), Some(id)) = (
            floors.get(&channel),
            path.file_stem().and_then(|value| value.to_str()),
        ) {
            if id <= floor.as_str() {
                continue;
            }
        }
        match parse_channel_message(&path) {
            // A room's own words are never news: its own sends don't ring
            // its own doorbell (they still ring every other member's).
            Ok(parsed) if parsed.message.from == room => {}
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
fn room_channel_message_paths(context: &Context, room: &str) -> Vec<(String, PathBuf)> {
    let mut paths = Vec::new();
    let Ok(summaries) = list_channels(context) else {
        return paths;
    };
    for summary in summaries {
        if !summary.members.contains_key(room) {
            continue;
        }
        let name = summary.info.name;
        if let Ok(channel) = ChannelPaths::new(context, &name) {
            if let Ok(files) = message_files(&channel.messages) {
                for path in files {
                    paths.push((name.clone(), path));
                }
            }
        }
    }
    paths
}

/// Each channel's last-read id for `room`, read-only — the startup floor for
/// the doorbell. Reading the cursor to decide what is unhandled never writes
/// it (the watch invariant). A missing or unreadable state file means "no
/// floor": ring for everything, the safe direction for a doorbell.
fn load_channel_floors(context: &Context, room: &str) -> HashMap<String, String> {
    let mut floors = HashMap::new();
    let Ok(state_path) = channel_state_path(context, room) else {
        return floors;
    };
    if let Ok(bytes) = std::fs::read(&state_path) {
        if let Ok(map) = serde_json::from_slice::<ChannelStateMap>(&bytes) {
            floors.extend(map);
        }
    }
    floors
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::encode_message;
    use crate::model::ChannelMessage;
    use crate::test_support::{test_root, trash_test_root};
    use std::fs;

    #[test]
    fn scan_batch_never_rings_for_the_rooms_own_messages() {
        let root = test_root("watch-ownfilter");
        let inbox = root.join("alpha").join("inbox");
        fs::create_dir_all(&inbox).expect("create inbox");
        let dir = root.join("channels").join("tax");
        fs::create_dir_all(dir.join("messages")).expect("create channel dirs");
        fs::write(
            dir.join("channel.json"),
            r#"{"name":"tax","created":"2026-07-22 01:00:00 -0500","created_by":"alpha"}"#,
        )
        .expect("write channel.json");
        fs::write(
            dir.join("members.json"),
            r#"{"alpha":"2026-07-22 01:00:00 -0500","beta":"2026-07-22 01:00:00 -0500"}"#,
        )
        .expect("write members.json");
        for (id, from) in [
            ("20260722-013000-000001-aaa111", "alpha"),
            ("20260722-013000-000002-bbb222", "beta"),
        ] {
            let message = ChannelMessage {
                id: id.to_owned(),
                from: from.to_owned(),
                channel: "tax".to_owned(),
                subject: String::new(),
                sent: "2026-07-22 01:30:00 -0500".to_owned(),
                event: None,
            };
            let bytes = encode_message(&message, "body").expect("encode");
            fs::write(dir.join("messages").join(format!("{id}.msg")), bytes)
                .expect("write message");
        }
        let context = Context {
            root: root.clone(),
            home: root.clone(),
        };
        let mut seen = HashSet::new();
        let batch = scan_batch(&context, "alpha", &inbox, &HashMap::new(), &mut seen)
            .expect("scan");
        let froms: Vec<&str> = batch
            .iter()
            .filter_map(|event| match event {
                WatchEvent::ChannelMessage { from, .. } => Some(from.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(froms, vec!["beta"], "own message must not ring own doorbell");
        trash_test_root(&root);
    }
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
