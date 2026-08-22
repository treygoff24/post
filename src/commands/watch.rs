use crate::channel::{message_files, parse_channel_message, ChannelPaths, CHANNELS_DIR};
use crate::channel_state::ChannelState;
use crate::cli::WatchArgs;
use crate::command_result::CommandResult;
use crate::error::{AppError, AppResult, ErrorCode};
use crate::mailbox::{mail_files, parse_mail, Context};
use crate::migration_fence;
use crate::output::{InboxItem, WatchEvent, WatchReason};
use notify::{RecursiveMode, Watcher};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

struct WatchTarget {
    room: String,
    inbox: PathBuf,
    /// Dirs this target watches: its inbox plus every membership channel's
    /// messages dir. Registered up front, re-derived on the slow pass (r2:
    /// re-register when a new channel dir appears or a watched dir is
    /// replaced under a stale watch).
    dirs: BTreeSet<PathBuf>,
    /// Per-channel seen-sets, read-only: the startup floor for the doorbell.
    /// A missing or unreadable state file means "nothing seen": ring for
    /// everything, the safe direction for a doorbell.
    channel_seen: HashMap<String, BTreeSet<String>>,
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
        let room_dir = context.root.join(&room);
        if !snapshot && crate::mailbox::read_only_command() && !room_dir.is_dir() {
            return Err(AppError::new(
                ErrorCode::NotFound,
                format!("room directory '{}' does not exist", room_dir.display()),
                "Enrolled long watch does not create mailboxes; create or initialize the room directory before watching.",
            ));
        }
        let dirs = target_dirs(context, &room, &inbox);
        targets.push(WatchTarget {
            channel_seen: load_channel_seen(context, &room),
            room,
            inbox,
            dirs,
            seen: HashSet::new(),
            scan_failing: false,
        });
    }
    // Ring for anything not yet handled. Load each channel's seen-set as a
    // read-only floor — watch NEVER writes one — and emit unseen messages. A
    // doorbell rings until handled: reading marks messages seen, so a handled
    // backlog never re-rings, but a REPLACEMENT doorbell after the original
    // dies mid-session (as ours did, repeatedly) still surfaces anything that
    // landed in the gap — including an id sorting BELOW the newest handled id
    // (the bridged late arrival) — instead of silently priming past it. The
    // room inbox keeps its own surface-on-startup behavior.
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
                &target.channel_seen,
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
    // Presence first: the watch is live from this moment, and backend
    // registration (FSEvents especially) can take hundreds of ms — `post who`
    // must not report a dead watch during that window.
    touch_heartbeats(context, &targets, interval_ms)?;
    // Register every watch BEFORE the first scan (r2): nothing created in
    // the gap can be missed, because the first pass inside the loop is an
    // unconditional scan. Any registration failure falls back to polling
    // with one warning — behavior identical to the pre-event loop.
    let desired: BTreeSet<PathBuf> = targets
        .iter()
        .flat_map(|target| target.dirs.iter().cloned())
        .collect();
    let (mut wake, event_mode) = match NotifyWake::register(&desired) {
        Ok(backend) => (Box::new(backend) as Box<dyn WakeSource>, true),
        Err(error) => {
            eprintln!(
                "post: warning: filesystem events unavailable ({error}); falling back to polling every {interval_ms} ms"
            );
            (Box::new(PollWake) as Box<dyn WakeSource>, false)
        }
    };
    run_watch_loop(
        context,
        &mut targets,
        &mut emitted_channel_ids,
        interval_ms,
        once,
        text,
        &mut wake,
        event_mode,
    )
}

/// First pass is unconditional (r2: scan once immediately after
/// registration), then the loop blocks on a wake source. Events are WAKE
/// HINTS for the existing full scan, never truth (r2): no per-event
/// incremental state exists anywhere.
#[allow(clippy::too_many_arguments)] // watch loop wiring; a param struct would add nothing
fn run_watch_loop(
    context: &Context,
    targets: &mut [WatchTarget],
    emitted_channel_ids: &mut HashSet<String>,
    interval_ms: u64,
    once: bool,
    text: bool,
    wake: &mut Box<dyn WakeSource>,
    event_mode: bool,
) -> AppResult<CommandResult> {
    touch_heartbeats(context, targets, interval_ms)?;
    let mut batch = scan_targets(context, targets, emitted_channel_ids, |_| true);
    if !batch.is_empty() {
        emit(&batch, text)?;
        if once {
            return Ok(CommandResult::success(String::new()));
        }
    }
    // The slow periodic pass keeps running even in event mode (r2): presence
    // and the migration-fence check need periodic passes, and re-registration
    // needs them to catch new or replaced channel dirs. Scaled x10 off the
    // poll interval, floored at 30 s — quiet enough to stay out of the way,
    // tight enough that a stale watch goes at most one window without news.
    // The POLL fallback has no slow pass: it scans every tick, behavior
    // identical to the pre-event loop.
    let slow_ticks = ((interval_ms * 10).max(30_000) / interval_ms.max(1)).max(1) as u32;
    let mut slow_every = if event_mode { slow_ticks } else { 1 };
    let mut ticks_since_slow = 0u32;
    // Heartbeat cadence stays at --interval-ms even when wakes arrive faster
    // or slower than ticks (`post who` presence depends on it).
    let mut last_beat = Instant::now();
    loop {
        batch = match wake.wait(Duration::from_millis(interval_ms)) {
            None => {
                // Backend died mid-run: degrade to polling rather than to
                // silence, with one warning like the startup fallback. The
                // poll fallback scans every tick, like today.
                eprintln!(
                    "post: warning: filesystem event backend failed; falling back to polling every {interval_ms} ms"
                );
                *wake = Box::new(PollWake);
                slow_every = 1;
                Vec::new()
            }
            Some(Wake::TimedOut) => {
                touch_heartbeats(context, targets, interval_ms)?;
                last_beat = Instant::now();
                ticks_since_slow += 1;
                if ticks_since_slow >= slow_every {
                    ticks_since_slow = 0;
                    // Re-derive the watched-dir set (new channel dirs, dirs
                    // replaced by rm+mkdir) and hand deltas to the backend
                    // before the unconditional rescan (r2).
                    refresh_target_dirs(context, targets);
                    let desired: BTreeSet<PathBuf> = targets
                        .iter()
                        .flat_map(|target| target.dirs.iter().cloned())
                        .collect();
                    wake.reconcile(&desired);
                    scan_targets(context, targets, emitted_channel_ids, |_| true)
                } else {
                    Vec::new()
                }
            }
            Some(Wake::Events(dirs)) => {
                if last_beat.elapsed() >= Duration::from_millis(interval_ms) {
                    touch_heartbeats(context, targets, interval_ms)?;
                    last_beat = Instant::now();
                }
                // Rescan every affected target through the full existing scan
                // path — never an incremental one (r2).
                scan_targets(context, targets, emitted_channel_ids, |target| {
                    target.dirs.iter().any(|dir| dirs.contains(dir))
                })
            }
        };
        if !batch.is_empty() {
            emit(&batch, text)?;
            if once {
                return Ok(CommandResult::success(String::new()));
            }
        }
    }
}

fn touch_heartbeats(context: &Context, targets: &[WatchTarget], interval_ms: u64) -> AppResult<()> {
    for target in targets {
        let _admission = migration_fence::admit(context, true)?;
        crate::presence::touch_heartbeat(context, &target.room, interval_ms);
    }
    Ok(())
}

/// One full scan pass over the selected targets, preserving the old loop's
/// degrade-and-keep-polling posture: a transient scan failure warns once and
/// never kills the doorbell; only stdout failure is fatal.
fn scan_targets(
    context: &Context,
    targets: &mut [WatchTarget],
    emitted_channel_ids: &mut HashSet<String>,
    selected: impl Fn(&WatchTarget) -> bool,
) -> Vec<WatchEvent> {
    let mut batch = Vec::new();
    for target in targets.iter_mut().filter(|target| selected(target)) {
        match scan_batch(
            context,
            &target.room,
            &target.inbox,
            &target.channel_seen,
            &mut target.seen,
            emitted_channel_ids,
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
    batch
}

fn refresh_target_dirs(context: &Context, targets: &mut [WatchTarget]) {
    for target in targets {
        target.dirs = target_dirs(context, &target.room, &target.inbox);
    }
}

/// A filesystem wake. Events are HINTS, never truth (r2): a wake triggers the
/// same full `scan_batch` the poll loop always ran.
enum Wake {
    /// Dirs with observed activity, intersected with the watched set.
    Events(BTreeSet<PathBuf>),
    /// The wait window elapsed with no event (poll tick).
    TimedOut,
}

/// Where the watch loop's blocking wait comes from: the notify backend in
/// production, a deterministic stub in tests. The loop never touches
/// filesystem events directly — wakes are hints, scans are truth.
trait WakeSource {
    /// Block up to `timeout`. `None` means the source died and the caller
    /// must fall back to polling.
    fn wait(&mut self, timeout: Duration) -> Option<Wake>;
    /// Align registrations with the desired watched-dir set. No-op for
    /// backends that hold no registration state.
    fn reconcile(&mut self, _desired: &BTreeSet<PathBuf>) {}
}

/// The fallback backend: sleep the full window, wake as a tick.
struct PollWake;

impl WakeSource for PollWake {
    fn wait(&mut self, timeout: Duration) -> Option<Wake> {
        std::thread::sleep(timeout);
        Some(Wake::TimedOut)
    }
}

/// The production backend: FSEvents/inotify watches over the watched-dir
/// set. Events are debounced by folding everything queued behind the first
/// delivery into one wake.
struct NotifyWake {
    watcher: notify::RecommendedWatcher,
    receiver: mpsc::Receiver<Result<notify::Event, notify::Error>>,
    watched: BTreeSet<PathBuf>,
    /// Path -> (device, inode) at registration time: a dir replaced by
    /// rm+mkdir keeps its path but changes identity, and its stale watch
    /// goes silent — reconcile re-registers on identity change (r2).
    identities: HashMap<PathBuf, (u64, u64)>,
}

impl NotifyWake {
    /// Register every dir up front (r2). Any registration failure aborts:
    /// the caller falls back to polling with one warning (M3).
    fn register(dirs: &BTreeSet<PathBuf>) -> notify::Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |result| {
            // Send fails only if the loop hung up; a dead doorbell's events
            // are unreadable anyway — same best-effort posture as presence.
            let _ = sender.send(result);
        })?;
        let mut identities: HashMap<PathBuf, (u64, u64)> = HashMap::new();
        for dir in dirs {
            watcher.watch(dir, RecursiveMode::NonRecursive)?;
            if let Some(id) = dir_id(dir) {
                identities.insert(dir.clone(), id);
            }
        }
        Ok(Self {
            watcher,
            receiver,
            watched: dirs.clone(),
            identities,
        })
    }

    fn collect(&self, dirs: &mut BTreeSet<PathBuf>, event: &notify::Event) {
        // Access events are our own scans reading files — ignoring them
        // keeps the doorbell from ringing itself. Everything else is a hint
        // worth a look; the full scan dedupes whatever is noise.
        if matches!(event.kind, notify::EventKind::Access(_)) {
            return;
        }
        for path in &event.paths {
            for dir in &self.watched {
                if path.starts_with(dir) {
                    dirs.insert(dir.clone());
                }
            }
        }
    }
}

impl WakeSource for NotifyWake {
    fn wait(&mut self, timeout: Duration) -> Option<Wake> {
        let mut dirs = BTreeSet::new();
        match self.receiver.recv_timeout(timeout) {
            Ok(Ok(event)) => self.collect(&mut dirs, &event),
            // Watcher overflow or delivery error: rescan unconditionally
            // (r2) — every watched dir is an affected hint.
            Ok(Err(_)) => dirs = self.watched.clone(),
            Err(RecvTimeoutError::Timeout) => return Some(Wake::TimedOut),
            // Sender dropped: the backend died; the caller falls back to
            // polling rather than to silence.
            Err(RecvTimeoutError::Disconnected) => return None,
        }
        // Debounce/batch: fold everything already queued into this wake.
        while let Ok(result) = self.receiver.try_recv() {
            match result {
                Ok(event) => self.collect(&mut dirs, &event),
                Err(_) => dirs = self.watched.clone(),
            }
        }
        dirs.retain(|dir| self.watched.contains(dir));
        if dirs.is_empty() {
            // A filtered-out event (e.g. our own reads): behave as a tick so
            // heartbeat cadence stays uniform.
            return Some(Wake::TimedOut);
        }
        Some(Wake::Events(dirs))
    }

    fn reconcile(&mut self, desired: &BTreeSet<PathBuf>) {
        for gone in self
            .watched
            .difference(desired)
            .cloned()
            .collect::<Vec<_>>()
        {
            let _ = self.watcher.unwatch(gone.as_path());
            self.identities.remove(gone.as_path());
            self.watched.remove(gone.as_path());
        }
        for dir in desired {
            let id = dir_id(dir);
            match (self.watched.get(dir), id) {
                // New dir: register it.
                (None, _) => {
                    if let Err(error) = self.watcher.watch(dir, RecursiveMode::NonRecursive) {
                        eprintln!(
                            "post: warning: cannot watch {:?}: {:?}",
                            dir.display().to_string(),
                            error.to_string()
                        );
                        continue;
                    }
                    self.watched.insert(dir.clone());
                    if let Some(id) = id {
                        self.identities.insert(dir.clone(), id);
                    }
                }
                // Known dir whose identity changed (rm+mkdir replacement):
                // the stale watch is silent; re-register under the same path.
                (Some(_), Some(id)) if self.identities.get(dir) != Some(&id) => {
                    let _ = self.watcher.unwatch(dir.as_path());
                    if let Err(error) = self.watcher.watch(dir, RecursiveMode::NonRecursive) {
                        eprintln!(
                            "post: warning: cannot re-watch {:?}: {:?}",
                            dir.display().to_string(),
                            error.to_string()
                        );
                    }
                    self.identities.insert(dir.clone(), id);
                }
                // Known dir whose metadata vanished mid-reconcile: forget the
                // identity so the next pass treats it as changed if needed.
                (Some(_), None) => {
                    self.identities.remove(dir);
                }
                _ => {}
            }
        }
    }
}

/// (device, inode) of a directory, or None when it does not exist.
fn dir_id(path: &Path) -> Option<(u64, u64)> {
    std::fs::metadata(path)
        .ok()
        .map(|meta| (meta.dev(), meta.ino()))
}
fn scan_batch(
    context: &Context,
    room: &str,
    inbox: &Path,
    channel_seen: &HashMap<String, BTreeSet<String>>,
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
        // Already seen = read before watch started; skip without parsing
        // (the filename stem is the id).
        if let (Some(seen_set), Some(id)) = (channel_seen.get(&channel), message_id) {
            if seen_set.contains(id) {
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

/// Every channel `room` belongs to, as (channel name, messages dir).
/// Best-effort: a transient read error degrades to fewer channels, never a
/// killed doorbell (same posture as the inbox scan's degrade-and-keep-polling).
/// ponytail: re-enumerates via list_channels each call; fine for a handful of
/// channels, revisit with a lighter membership scan if that grows.
fn membership_channels(context: &Context, room: &str) -> Vec<(String, PathBuf)> {
    let mut channels = Vec::new();
    let channels_dir = context.root.join(CHANNELS_DIR);
    let entries = match std::fs::read_dir(&channels_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return channels,
        Err(error) => {
            eprintln!(
                "post: warning: cannot list channels directory {:?}: {:?}",
                channels_dir.display().to_string(),
                error.to_string()
            );
            return channels;
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
        channels.push((name, channel.messages));
    }
    channels
}

/// Every `.msg` path across the channels `room` belongs to.
fn room_channel_message_paths(context: &Context, room: &str) -> Vec<(String, PathBuf)> {
    let mut paths = Vec::new();
    for (name, dir) in membership_channels(context, room) {
        match message_files(&dir) {
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

/// The dir set one target watches: its inbox plus every membership channel's
/// messages dir. Registered before the first scan; re-derived on the slow
/// pass so re-registration catches new or replaced channel dirs (r2).
fn target_dirs(context: &Context, room: &str, inbox: &Path) -> BTreeSet<PathBuf> {
    // Canonicalize every watched dir: FSEvents (macOS) reports canonical
    // paths — TMPDIR runs through /var -> /private/var — and the wake
    // mapping compares event paths against this set. One path form for
    // registration, wake matching, and re-registration.
    let mut dirs: BTreeSet<PathBuf> = membership_channels(context, room)
        .into_iter()
        .map(|(_, dir)| std::fs::canonicalize(&dir).unwrap_or(dir))
        .collect();
    dirs.insert(std::fs::canonicalize(inbox).unwrap_or_else(|_| inbox.to_path_buf()));
    dirs
}

/// Each channel's seen-set for `room`, read-only — the startup floor for the
/// doorbell. Deciding what is unhandled never writes state (the watch
/// invariant). A missing or unreadable state file means "nothing seen": ring
pub(super) fn load_channel_seen(
    context: &Context,
    room: &str,
) -> HashMap<String, BTreeSet<String>> {
    match ChannelState::load(context, room) {
        Ok(state) => state.into_channels().into_iter().collect(),
        Err(error) => {
            eprintln!(
                "post: warning: unreadable channel state for room {room:?}: {}",
                error.message
            );
            HashMap::new()
        }
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
                sender_address: None,
                sender_provenance: None,
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

    /// A wake source the test drives directly: an optional side effect fires
    /// on the first wait (simulating a delivery arriving mid-watch), then the
    /// queued wakes play out. Empty queue behaves like a poll tick.
    struct StubWake {
        on_first_wait: Option<Box<dyn FnOnce()>>,
        scheduled: std::collections::VecDeque<Wake>,
    }

    impl WakeSource for StubWake {
        fn wait(&mut self, _timeout: Duration) -> Option<Wake> {
            if let Some(action) = self.on_first_wait.take() {
                action();
            }
            Some(self.scheduled.pop_front().unwrap_or(Wake::TimedOut))
        }
    }

    fn mail_bytes(id: &str, from: &str, to: &str) -> Vec<u8> {
        format!(
            r#"{{"id":"{id}","from":"{from}","to":"{to}","kind":"letter","subject":"","sent":"2026-08-22 00:00:00 +0000"}}
---
body
"#
        )
        .into_bytes()
    }

    #[test]
    fn event_wake_drives_the_loop_and_once_exits_after_emitting() {
        let root = test_root("watch-event-loop");
        let room_dir = root.join("alpha");
        let inbox = room_dir.join("inbox");
        fs::create_dir_all(&inbox).expect("create inbox");
        fs::write(
            root.join("rooms.json"),
            format!(
                r#"{{"alpha": {}}}
"#,
                serde_json::to_string(&room_dir).expect("serialize room path")
            ),
        )
        .expect("write rooms.json");
        let context = Context {
            root: root.clone(),
            home: root.clone(),
        };
        // A target with nothing in it yet; the delivery arrives DURING the
        // first wait, as a real filesystem event would.
        let mut targets = vec![WatchTarget {
            channel_seen: HashMap::new(),
            room: "alpha".to_owned(),
            inbox: inbox.clone(),
            dirs: target_dirs(&context, "alpha", &inbox),
            seen: HashSet::new(),
            scan_failing: false,
        }];
        let wake_dirs = targets[0].dirs.clone();
        let deliver_inbox = inbox.clone();
        let stub = StubWake {
            on_first_wait: Some(Box::new(move || {
                fs::write(
                    deliver_inbox.join("20260822-000000-000001-abc123.mail"),
                    mail_bytes("20260822-000000-000001-abc123", "beta", "alpha"),
                )
                .expect("deliver mid-watch mail");
            })),
            scheduled: [Wake::Events(wake_dirs.into_iter().collect())]
                .into_iter()
                .collect(),
        };
        let mut wake: Box<dyn WakeSource> = Box::new(stub);
        let mut emitted_channel_ids = HashSet::new();
        // once=true: run_watch_loop returns Ok ONLY after emitting a non-empty
        // batch — so Ok proves the event wake produced an emit.
        run_watch_loop(
            &context,
            &mut targets,
            &mut emitted_channel_ids,
            1000,
            true,
            false,
            &mut wake,
            true,
        )
        .expect("loop emits and exits");
        assert!(
            room_dir.join("watch.heartbeat").exists(),
            "event mode must still touch presence heartbeats"
        );
        trash_test_root(&root);
    }

    #[test]
    fn notify_backend_rings_for_a_file_created_after_watch_starts() {
        let base = test_root("watch-notify-backend");
        let dir = base.join("messages");
        fs::create_dir_all(&dir).expect("create watched dir");
        let canonical = fs::canonicalize(&dir).expect("canonicalize");
        let mut desired = BTreeSet::new();
        desired.insert(canonical.clone());
        let mut backend = NotifyWake::register(&desired).expect("register watches");
        fs::write(dir.join("20260822-000000-000001-def456.msg"), b"junk")
            .expect("create message after watch start");
        match backend.wait(Duration::from_secs(5)) {
            Some(Wake::Events(dirs)) => assert!(
                dirs.contains(&canonical),
                "event must name the watched dir, got {dirs:?}"
            ),
            Some(Wake::TimedOut) => panic!("backend never woke for the new file"),
            None => panic!("backend died instead of waking"),
        }
        trash_test_root(&base);
    }

    #[test]
    fn reconcile_re_registers_a_replaced_watched_dir() {
        let base = test_root("watch-notify-reconcile");
        let dir = base.join("messages");
        fs::create_dir_all(&dir).expect("create watched dir");
        let canonical = fs::canonicalize(&dir).expect("canonicalize");
        let mut desired = BTreeSet::new();
        desired.insert(canonical.clone());
        let mut backend = NotifyWake::register(&desired).expect("register watches");
        // rm + mkdir: same path, new inode — the stale watch goes silent,
        // which is exactly what reconcile must detect and repair (r2).
        fs::remove_dir_all(&dir).expect("remove dir");
        fs::create_dir_all(&dir).expect("recreate dir");
        backend.reconcile(&desired);
        fs::write(dir.join("20260822-000000-000002-abc789.msg"), b"junk")
            .expect("create message after replacement");
        match backend.wait(Duration::from_secs(5)) {
            Some(Wake::Events(dirs)) => assert!(
                dirs.contains(&canonical),
                "re-registered watch must ring, got {dirs:?}"
            ),
            Some(Wake::TimedOut) => panic!("stale watch stayed silent after reconcile"),
            None => panic!("backend died instead of waking"),
        }
        trash_test_root(&base);
    }
}
