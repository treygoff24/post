use crate::channel::{
    channel_state_path, message_files, parse_channel_message, ChannelPaths, ChannelStateMap,
    CHANNELS_DIR,
};
use crate::cli::WatchArgs;
use crate::command_result::CommandResult;
use crate::error::{AppError, AppResult};
use crate::mailbox::{mail_files, parse_mail, Context};
use crate::output::{InboxItem, WatchEvent, WatchReason};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

struct WatchTarget {
    room: String,
    inbox: PathBuf,
    floors: HashMap<String, String>,
    seen: HashSet<PathBuf>,
    scan_failing: bool,
}

pub(super) fn run(context: &Context, args: WatchArgs) -> AppResult<CommandResult> {
    let WatchArgs {
        room: requested_rooms,
        once,
        snapshot,
        limit,
        interval_ms,
        text,
    } = args;
    let rooms = context.load_rooms()?;
    let requested_rooms = if requested_rooms.is_empty() {
        vec![context.resolved_room(None, &rooms)?]
    } else {
        requested_rooms
            .into_iter()
            .map(|room| context.resolved_room(Some(room), &rooms))
            .collect::<AppResult<Vec<_>>>()?
    };
    let mut unique_rooms = HashSet::new();
    let mut targets = Vec::new();
    for room in requested_rooms {
        if !unique_rooms.insert(room.clone()) {
            continue;
        }
        if !rooms.contains_key(&room) {
            // Snapshot is the lifecycle-hook poll and may fire from ANY cwd — an
            // unregistered room means "this directory has no mailbox", so scan
            // nothing and, critically, create nothing (a machine-wide hook would
            // otherwise mint a junk mailbox per project directory ever visited).
            if snapshot {
                eprintln!(
                    "post: warning: room {room:?} is not registered; snapshot scans nothing and creates nothing"
                );
                continue;
            }
            // A typo'd --room silently watches a fresh empty mailbox forever, so
            // unlike inbox (whose empty listing is immediately visible) watch warns.
            eprintln!(
                "post: warning: room {room:?} is not registered; watching a new empty mailbox"
            );
        }
        let (inbox, _) = context.mailbox_dirs(&room)?;
        targets.push(WatchTarget {
            floors: load_channel_floors(context, &room),
            room,
            inbox,
            seen: HashSet::new(),
            scan_failing: false,
        });
    }
    // Ring for anything not yet handled. Load each channel's cursor as a
    // read-only floor — watch NEVER writes a cursor — and emit messages with
    // id > floor. A doorbell rings until handled: reading advances the real
    // cursor, so a handled backlog never re-rings, but a REPLACEMENT doorbell
    // after the original dies mid-session (as ours did, repeatedly) still
    // surfaces anything that landed in the gap, instead of silently priming
    // past it. The room inbox keeps its own surface-on-startup behavior.
    // Grows for the life of the watch (like each target's seen set): bounded
    // by total channel messages, a few bytes each — deliberate, not a leak.
    let mut emitted_channel_ids = HashSet::new();
    // Snapshot is a one-shot poll for lifecycle hooks — it must not mint or
    // refresh a presence heartbeat, or `post who` would report a live watch
    // for five seconds after a hook that already exited.
    if snapshot {
        // One scan, then out: the hook-facing poll. Unlike the loop below, a
        // direct-mail scan failure propagates as a real error — a lifecycle
        // hook must never mistake "could not look" for "inbox empty".
        // Per-channel degradation stays inside scan_batch, unchanged.
        let mut batch = Vec::new();
        for target in &mut targets {
            batch.extend(scan_batch(
                context,
                &target.room,
                &target.inbox,
                &target.floors,
                &mut target.seen,
                &mut emitted_channel_ids,
            )?);
        }
        if let Some(limit) = limit.filter(|limit| *limit > 0) {
            let omitted = batch.len().saturating_sub(limit);
            if omitted > 0 {
                batch.drain(..omitted);
                let noun = if omitted == 1 { "event" } else { "events" };
                eprintln!(
                    "post: snapshot limit omitted {omitted} earlier {noun} (use --limit 0 for all)"
                );
            }
        }
        if !batch.is_empty() {
            emit(&batch, text)?;
        }
        return Ok(CommandResult::success(String::new()));
    }
    loop {
        // Presence: refresh heartbeats every poll so `post who` sees live watches.
        // Snapshot never reaches here. Stamp carries interval_ms so liveness
        // scales with --interval-ms instead of a fixed LIVE_SECS window.
        for target in &targets {
            crate::presence::touch_heartbeat(context, &target.room, interval_ms);
        }
        // A doorbell that dies is silently useless: transient scan failures
        // (mailbox trashed and recreated, permission blips) degrade to an
        // empty batch and polling continues. Only stdout failure is fatal —
        // if events can't reach the consumer, exiting IS the notification.
        let mut batch = Vec::new();
        for target in &mut targets {
            match scan_batch(
                context,
                &target.room,
                &target.inbox,
                &target.floors,
                &mut target.seen,
                &mut emitted_channel_ids,
            ) {
                Ok(events) => {
                    if target.scan_failing {
                        target.scan_failing = false;
                        eprintln!(
                            "post: warning: watch scan recovered for room {:?}",
                            target.room
                        );
                    }
                    batch.extend(events);
                }
                Err(error) => {
                    if !target.scan_failing {
                        target.scan_failing = true;
                        eprintln!(
                            "post: warning: watch scan failed for room {:?} (will keep polling): {}",
                            target.room, error.message
                        );
                    }
                }
            }
        }
        if !batch.is_empty() {
            emit(&batch, text)?;
            if once {
                return Ok(CommandResult::success(String::new()));
            }
        }
        std::thread::sleep(Duration::from_millis(interval_ms));
    }
}

fn scan_batch(
    context: &Context,
    room: &str,
    inbox: &Path,
    floors: &HashMap<String, String>,
    seen: &mut HashSet<PathBuf>,
    emitted_channel_ids: &mut HashSet<String>,
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
                batch.push(WatchEvent::unreadable(room, id, WatchReason::Mail));
            }
        }
    }
    // Channels the room belongs to. NEVER touches a cursor — a doorbell
    // notifies, it does not consume (contract 013246 watch invariant).
    for (channel, path) in room_channel_message_paths(context, room) {
        if !seen.insert(path.clone()) {
            continue;
        }
        let message_id = path.file_stem().and_then(|value| value.to_str());
        // At or below the reader's cursor = already read before watch
        // started; skip without parsing (the filename stem is the id).
        if let (Some(floor), Some(id)) = (floors.get(&channel), message_id) {
            if id <= floor.as_str() {
                continue;
            }
        }
        let dedupe_id = message_id
            .map(str::to_owned)
            .unwrap_or_else(|| path.display().to_string());
        if emitted_channel_ids.contains(&dedupe_id) {
            continue;
        }
        match parse_channel_message(&path) {
            // A room's own words are never news: its own sends don't ring
            // its own doorbell (they still ring every other member's).
            Ok(parsed) if parsed.message.from == room => {}
            Ok(parsed) => {
                emitted_channel_ids.insert(parsed.message.id.clone());
                batch.push(WatchEvent::channel_message(parsed.message, room));
            }
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
                let id = message_id.unwrap_or("<non-utf8 filename>").to_owned();
                emitted_channel_ids.insert(dedupe_id);
                batch.push(WatchEvent::unreadable(room, id, WatchReason::Channel));
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
    let channels_dir = context.root.join(CHANNELS_DIR);
    let entries = match std::fs::read_dir(&channels_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return paths,
        Err(error) => {
            eprintln!(
                "post: warning: cannot list channels directory {:?}: {:?}",
                channels_dir.display().to_string(),
                error.to_string()
            );
            return paths;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                eprintln!(
                    "post: warning: cannot read channels entry {:?}: {:?}",
                    channels_dir.display().to_string(),
                    error.to_string()
                );
                continue;
            }
        };
        if !entry.path().is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            eprintln!(
                "post: warning: skipped channel with non-UTF-8 name under {:?}",
                channels_dir.display().to_string()
            );
            continue;
        };
        let channel = match ChannelPaths::new(context, &name) {
            Ok(channel) => channel,
            Err(error) => {
                eprintln!(
                    "post: warning: skipped invalid channel {:?}: {:?}",
                    name, error.message
                );
                continue;
            }
        };
        if !channel.exists() {
            continue;
        }
        if let Err(error) = channel.load_info() {
            eprintln!(
                "post: warning: skipped channel {:?}: {:?}",
                name, error.message
            );
            continue;
        }
        let members = match channel.load_members() {
            Ok(members) => members,
            Err(error) => {
                eprintln!(
                    "post: warning: skipped channel {:?}: {:?}",
                    name, error.message
                );
                continue;
            }
        };
        if !members.contains_key(room) {
            continue;
        }
        match message_files(&channel.messages) {
            Ok(files) => {
                for path in files {
                    paths.push((name.clone(), path));
                }
            }
            Err(error) => {
                eprintln!(
                    "post: warning: skipped channel {:?}: {:?}",
                    name, error.message
                );
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
                display_name: None,
                pfp: None,
                re: None,
                mentions: vec![],
                signature_ref: None,
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
        let mut emitted_channel_ids = HashSet::new();
        let batch = scan_batch(
            &context,
            "alpha",
            &inbox,
            &HashMap::new(),
            &mut seen,
            &mut emitted_channel_ids,
        )
        .expect("scan");
        let froms: Vec<&str> = batch
            .iter()
            .filter_map(|event| match event {
                WatchEvent::ChannelMessage { from, .. } => Some(from.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            froms,
            vec!["beta"],
            "own message must not ring own doorbell"
        );
        trash_test_root(&root);
    }
}
