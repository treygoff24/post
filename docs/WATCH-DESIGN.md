# `post watch` — the doorbell

The original 2026-07-21 design used polling only. Version 0.6.0 keeps its two
load-bearing decisions — scans are truth and watch never returns bodies — while
adding native filesystem events as wake hints. This document describes the
shipped design.

## Problem

Before `post watch`, agents learned about arrivals only when their own loops
polled. Session-local directory watchers proved the need for a first-class
doorbell: one blocking command whose stdout emits one line per arrival, so a
harness monitor can notify its agent without reading message bodies.

## Command

```
post watch [--room <name>]... [--once | --snapshot [--limit <N>]] [--interval-ms <N>] [--text]
```

- Room resolution is identical to `inbox` (explicit `--room`, else the
  registered room containing cwd, else cwd basename). Repeat `--room` to merge
  several rooms. A long-running watch accepts an unregistered room with a
  warning and creates its mailbox directories. Snapshot mode instead warns,
  scans nothing, and creates nothing, because lifecycle hooks can run from an
  arbitrary cwd.
- Default output: NDJSON, one object per event (machine-first, matching
  inbox's JSON default). `--text` for the human line format, mirroring
  inbox's text lines. `--text` conflicts with `--json`; bare `--json` is
  accepted and redundant.
- `--interval-ms`: wait and heartbeat cadence, and the polling-fallback
  interval; default 1000, clamped 100..=60000 by clap.
- `--once`: exit 0 after the first batch that emits at least one event
  (lets an agent await a single delivery without watch-loop plumbing).
- `--snapshot`: scan exactly once and exit. `--limit N` emits only the last N
  events from that scan; zero is unlimited. Watch never consumes omitted
  events.
- Otherwise runs until killed. Stdout is flushed after every batch (a
  monitor must never wait on a buffered line).

Event shapes:

```
{"event":"mail","room":R,"id":I,"from":F,"kind":K,"subject":S,"sent":T,"reason":"mail"}
{"event":"unreadable","room":R,"id":I,"reason":"mail"|"channel"}
{"event":"channel_message","channel":C,"id":I,"from":F,"subject":S,"sent":T,"reason":"channel"|"mention"}
```

Text mode: `<id>  [<kind>] from <from>  "subject"` (inbox's line format,
subject debug-quoted so control characters render escaped, never raw), a
channel-prefixed message line, or `<id>  [?] unreadable envelope`.

## Lens (a): the framing boundary

Watch emits ENVELOPE METADATA ONLY — the same fields `inbox` already lists
without a banner (id/from/kind/subject/sent). Body content never appears on
any watch surface, in any mode, including error paths. Consumption stays
where it always was: `post read`, which enforces the framing banner. Watch
is therefore "inbox, streamed" — it cannot become a framing bypass because
it has no access path to the body in its output code (the parse result's
body field is dropped at the only construction site; a test asserts a
distinctive body string never appears in watch output).

Subject/from are attacker-influenced (hand-written mail files bypass send's
control-character validation), so text mode debug-escapes the subject and
NDJSON serializes through serde (escaping is structural). A crafted subject
cannot fake a framing banner or split an event line in either mode.

## Lens (b): event-loss windows

Design choice: **scan truth, event hints.** Long-running watch registers a
`notify` backend before its first unconditional scan. inotify on Linux and
FSEvents on macOS can wake the loop early, but an event is never interpreted as
a delivery. It only selects targets for the existing full scan.

- **Registration before scan.** Anything created before registration is found
  by the first scan. Anything created after registration either wakes the loop
  or is found by the slow full-scan deadline. There is no scan-then-register
  hole.
- **Overflow means rescan everything.** A `need_rescan` event or backend error
  marks every watched directory as affected. If the backend dies, watch warns
  once and falls back to polling at `--interval-ms` rather than going silent.
- **Reconciliation is wall-clock based.** The slow pass re-derives channel
  directories, re-registers directories replaced at the same path, retries a
  failed re-watch, and scans every target. Its deadline is checked after every
  wake, so continuous traffic for one room cannot starve another room's scan.
- **Atomic delivery is load-bearing and already guaranteed.** post commits
  mail via exclusive hard-link after a synced temp write (CONTRACT.md,
  on-disk format). A directory listing therefore never sees a partial
  file — the link either exists with full content or doesn't. No
  rename-vs-create event-type hazard exists because we never consume FS
  events at all.
- **Startup emits existing unread.** The "mail arrived just before the
  watcher started" hole is closed structurally: watch's first batch IS the
  current unread set. Semantics: watch = "stream of unread mail, starting
  now." An agent whose inbox is empty gets silence; an agent with backlog
  gets the backlog. (Re-arming a watch re-emits current unread — idempotent
  for any consumer that keys on id, and arguably the correct reminder.)
- **The one accepted loss window, documented:** mail that arrives and is
  consumed by a concurrent `post read` before any scan observes it is never
  emitted. This is out of scope: the
  watcher's own agent is normally the only reader of its room, and reads
  it performs are prompted by the watch itself. A second concurrent reader
  of the same room is a protocol anomaly, not a watch defect.
- Files that vanish between scan and parse (consumed mid-batch) are
  skipped silently and marked seen — they are no longer unread.
- Malformed mail DIVERGES from inbox: inbox skips with a warning; watch
  emits an `unreadable` event (plus the same stderr warning). Rationale: a
  doorbell that stays silent on a malformed delivery is a doorbell an
  attacker (or a botched hand-write) can suppress; the agent should ring,
  then investigate with `post read`/`doctor`. The event carries id only —
  nothing from the malformed content is echoed (lens (a) again: a file
  whose envelope failed validation gets NOTHING quoted from it).

## Alternatives rejected

- **Filesystem events as truth:** coalescing, overflow, directory replacement,
  and backend failure can all omit or blur events. Full scans remain the only
  source of delivery truth.
- **Raw platform APIs:** direct kqueue, FSEvents, or inotify code would put
  platform-specific unsafe machinery in the correctness path. The `notify`
  crate supplies the wake backend while the portable scan path preserves the
  contract.
- **Emitting nothing at startup (pure "from now on" semantics):** leaves
  the classic arm-vs-arrival race to every consumer; rejected in favor of
  closing it structurally.

## Non-goals

Watch never moves, mutates, or deletes mail; never touches rules or rooms
config; never prints body content; and never mutates channel seen-state. A
long-running watch refreshes its presence heartbeat, while its delivery
dedupe remains process memory. It is not a daemon and starts nothing in the
background.

## Contract / surface changes

- CONTRACT.md and `post schema` define the normative command and event shapes.
- Tests cover startup backlog, live native wake, polling fallback, overflow
  rescans, replaced-directory re-registration, slow-pass starvation,
  `--once`, snapshot bounds, body exclusion, unreadable events, and multi-room
  channel deduplication.
