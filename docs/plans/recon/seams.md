# Plan B seam map

This is a map of the current 0.8.0 checkout. It records code that was read,
not a proposed implementation. The goal lock calls the new state a cursor;
the current channel implementation calls its durable state a seen-set. That
distinction matters below: `cursor` in existing receipts is only a max-seen-id
summary, not the filtering model (`src/channel_state.rs:61-76`,
`src/channel_state.rs:108-115`).

## 1. Writer admission and the migration fence

### Admission entry point

`src/commands/mod.rs:21-38` is the single dispatcher prelude for every command.
It builds `Context`, classifies the parsed command with
`migration_fence::classify_write`, admits writers with
`migration_fence::admit`, and sets the thread-local read-only guard. It only
calls `Context::prepare_first_run` when the invocation is not a fenced read and
is not `doctor` (`src/commands/mod.rs:34-38`). The dispatch table is
`src/commands/mod.rs:65-77`.

`Context::from_env` resolves `HOME` and the absolute `POST_MAIL_ROOT` override,
refusing a relative root before any mailbox work (`src/mailbox.rs:102-128`).

`WriteAdmission` owns the optional fence-lock file and records whether the
store is enrolled (`src/migration_fence.rs:37-46`). The admission object is
held through ordinary command output and its post-output action: the
dispatcher moves it into an `after_stdout` closure at
`src/commands/mod.rs:79-85`. `finish_command_result` writes and flushes stdout
first, then invokes that action (`src/app.rs:139-156`). Dropping the captured
`File` releases the flock.

### Fence state, generation, and root flock

The writer-only environment declaration is parsed by
`current_generation` (`src/migration_fence.rs:155-176`). Fence state is read
from `.post-arx.json` with `O_NOFOLLOW`, regular-file/link-count checks, a 4 KiB
cap, strict JSON parsing, and positive generation validation
(`src/migration_fence.rs:271-304`). `read_state` is never called for a read
solely to validate `POST_ARX_GENERATION`; `read_only_must_not_mutate` treats a
set declaration as read-only protection and treats a present or malformed
state as protected (`src/migration_fence.rs:306-316`).

The solitary migration lock is `<root>/.post-arx.lock`. `lock` opens it with
`O_NOFOLLOW`, optionally creates it only for fence setup, checks a solitary
regular-file inode, takes `LOCK_EX`, then rechecks the path inode so an
unlink/replace race is refused (`src/migration_fence.rs:179-264`). `admit_generation`
is the writer gate (`src/migration_fence.rs:359-426`). A missing root or a root
with no state admits only a legacy writer with no generation. A fenced state
refuses all ordinary writes. An active state requires a supplied generation
equal to the state generation and retains the already-open lock in the returned
admission.

The contract describes the same split and explicitly says the root flock is
held through mutation, stdout, and the after-stdout cursor/read move, while a
long non-snapshot watch holds it only around heartbeats
(`CONTRACT.md:51-80`).

### Which commands take which path

`classify_write` is the authoritative current matrix
(`src/migration_fence.rs:484-515`):

| Command form | Writer admission | Additional state/registry lock |
|---|---|---|
| `doctor --fix`, `send`, `rooms add`, `owner init`, `profile set/clear` | yes | `rooms add`, owner, and profile registry mutations take `.rooms.lock`; `rooms add` calls `Context::lock_rooms` at `src/commands/rooms.rs:23-35`, profile set/clear use it at `src/commands/profile.rs:35-46` and `src/commands/profile.rs:126-132`, and owner init uses it at `src/commands/owner.rs:40-46`. |
| `read` without `--peek` | yes | no separate mail-state flock; its move is deferred until after stdout. |
| plain `chat`, `chat --limit`, `chat --discard`, `chat --discard-through`, join/send forms | yes | join takes the global channel membership lock (`src/channel.rs:125-149`, acquired at `src/channel.rs:255-264`); channel seen-set mutations take the per-room lock described below. Channel message sends use exclusive file creation, not a channel send lock (`src/channel.rs:1009-1056`). |
| `watch` without `--snapshot` | yes at startup, then per heartbeat | startup admission is dropped before entering the long loop (`src/commands/mod.rs:40-43`); each heartbeat re-admits independently. |
| `read --peek`, `chat --peek`, `chat --history`, `chat --since`, `chat --seen-by`, `watch --snapshot`, listings, `schema`, `doctor` without `--fix`, profile/owner show | no | these run under the read-only guard when the store is enrolled/fenced. |

The plain-chat branch is a writer because it consumes unread messages; history
and since are explicitly excluded from the writer test in
`src/migration_fence.rs:499-511`. The CLI currently has no `catchup` or
`search` variant (`src/cli.rs:44-70`), so a new command must enter both this
classification matrix and the dispatch table.

There are three non-migration locks relevant to Plan B:

1. `.rooms.lock` is an exclusive registry lock (`src/mailbox.rs:207-224`).
2. `channels/.channels.lock` protects membership/config mutation only; channel
   message sends do not take it (`src/channel.rs:125-149`).
3. `<root>/<room>/.channel-state.lock` protects the whole room seen-set map.
   `seal` creates the room parent, takes the lock, reloads, computes the union,
   and atomically replaces the state while holding it
   (`src/channel_state.rs:171-240`); the lock helper uses a private
   `O_NOFOLLOW` file and `LOCK_EX` (`src/channel_state.rs:484-510`).

The current parent-creation behavior is a writer-only seam: `mailbox_dirs`
returns paths without creating them under the read-only guard, but creates
`inbox` and `read` directories otherwise (`src/mailbox.rs:413-431`). A new
cursor file must not cause a read-only listing or watch scan to create a room
directory.

## 2. Consuming read paths

### Direct mail: `read`

`src/commands/read.rs:9-81` resolves the room and inbox/read paths, finds a
   filename-prefix match, parses the mail, renders it, and returns immediately
   for `--peek` (`src/commands/read.rs:15-43`). A plain read returns an
   `after_stdout` action that calls `exclusive_move` from inbox to read
   (`src/commands/read.rs:45-80`). `exclusive_move` hard-links the destination
   before removing the source (`src/mailbox.rs:809-820`). Thus the current
   direct-mail consumed state is physical inbox placement; there is no mail
   cursor file today.

`already_read` serves read/archive copies and explicitly consumes nothing
(`src/commands/read.rs:83-180`). It filters global archive candidates to mail
   this room sent or received (`src/commands/read.rs:95-123`), and a channel id
   is redirected to `post chat ... --history ...` (`src/commands/read.rs:127-149`).

### Channel mail: plain `chat`

`chat::read` resolves the acting room, then chooses either a cursorless
`AfterId` collection for `--history`/`--since`, or `read_batch` over the
room's `ChannelState` seen-set (`src/commands/chat.rs:106-137`). The complete
selected id list is captured before display trimming
(`src/commands/chat.rs:139-151`). `--discard` uses that full list
(`src/commands/chat.rs:152-166`); ordinary reads apply the newest-25 display
bound, with mentions rescued from the skipped prefix
(`src/commands/chat.rs:265-298`).

The read returns a success result for peek/cursorless/empty cases. Otherwise
it returns `CommandResult::after_stdout`; the callback unions exactly the ids
selected before trimming through `ChannelState::mark_seen`
(`src/commands/chat.rs:251-262`). This is the direct hook for any new
cursor-advance operation. It is intentionally emit-then-consume: a broken
stdout leaves the seen-set unchanged, and a message arriving between selection
and the callback is not swallowed (`src/commands/chat.rs:139-143`; the race is
covered by `src/commands/chat.rs:1907-1937`).

`read_batch` calls `ChannelState::load` and `collect_batch` with
`UnreadRule::NotInSeen`; `collect_batch` checks membership, skips seen ids,
parses each selected `.msg`, excludes the acting room's own messages, and
sorts by id (`src/commands/chat.rs:601-703`). A consuming read fails closed on
an unreadable selected message; cursorless history/since warns and skips it
(`src/commands/chat.rs:676-689`).

The durable channel state already has the Plan B-shaped room/channel seam:
`channel-state.json` is a v2 map of channel names to sorted seen-id sets, with
legacy v1 watermark conversion in memory and first-write conversion under the
room lock (`src/channel_state.rs:1-29`, `src/channel_state.rs:344-468`). The
goal lock's proposed `cursors.json` therefore needs an explicit coexistence or
replacement decision; the current code has no separate cursor file.

## 3. Watch ring path

### Required backlog invariant

The brief's approximate `watch.rs:266-273` points to a shifted location in
this checkout. The current implementation says, verbatim,
`src/commands/watch.rs:295-302`:

```text
    // Ring for anything not yet handled. Load each channel's seen-set as a
    // read-only floor — watch NEVER writes one — and emit unseen messages. A
    // doorbell rings until handled: reading marks messages seen, so a handled
    // backlog never re-rings, but a REPLACEMENT doorbell after the original
    // dies mid-session (as ours did, repeatedly) still surfaces anything that
    // landed in the gap — including an id sorting BELOW the newest handled id
    // (the bridged late arrival) — instead of silently priming past it. The
    // room inbox keeps its own surface-on-startup behavior.
```

`watch::run` resolves requested rooms, obtains each inbox path, and loads a
read-only channel-seen floor into each `WatchTarget`
(`src/commands/watch.rs:222-283`). `load_channel_seen` catches a bad/missing
state file and falls back to an empty in-memory floor, never writes it
(`src/commands/watch.rs:957-973`).

The scan is a full directory scan, not an incremental cursor. Direct mail is
listed and parsed; malformed files produce an `unreadable` ring. Channel
messages are listed from every joined channel, skipped when their id is in the
startup seen floor, deduplicated in process memory, and parsed into envelope
metadata only (`src/commands/watch.rs:738-837`). The channel branch explicitly
says it never touches a cursor (`src/commands/watch.rs:782-795`). The
in-memory `seen` paths and `emitted_channel_ids` are not durable read state.

Snapshot mode scans once and emits, with no heartbeat and no consumption
(`src/commands/watch.rs:321-351`). Long-running mode touches a heartbeat before
registration and enters `run_watch_loop` (`src/commands/watch.rs:353-394`).
The first loop pass is unconditional, emits the backlog, and returns for
`--once` (`src/commands/watch.rs:396-420`). Filesystem events are wake hints;
affected targets are rescanned through the same full scan, and the slow pass
re-registers directories before a full rescan (`src/commands/watch.rs:421-489`).

Heartbeat admission is narrow by design: `touch_heartbeats` calls
`migration_fence::admit(context, true)` immediately around each heartbeat
touch (`src/commands/watch.rs:491-497`). The loop repeats that function on
timed-out ticks and fast event wakes (`src/commands/watch.rs:447-466`). A
fence/generation refusal therefore terminates the watch without holding the
root lock across scanning or stdout.

### Ring output and fencepost ids

`WatchDigest::text_line` emits `[first_id..last_id]` and, for channel sources,
adds a copyable `--since` argument generated by `digest_since_fencepost`
(`src/commands/watch.rs:95-111`, `src/commands/watch.rs:141-151`). The helper
replaces the final id character with `!`, which sorts below a valid id so the
exclusive `chat --since` comparison includes `first_id`.

`emit` writes and flushes every text/JSON event or digest line directly to
stdout (`src/commands/watch.rs:976-1004`). It does not return a body-bearing
`CommandResult`, so watch delivery itself has no after-stdout cursor callback.

## 4. Listings and count seams

### Channels

`channels::run` maps `channel::list_channels` summaries into
`ChannelListItem` values (`src/commands/channels.rs:8-19`). The summary's
`messages` count is the length of `message_files`, which lists regular `.msg`
files and sorts paths but does not parse their envelopes
(`src/channel.rs:1280-1293`, `src/channel.rs:1302-1338`). It is therefore a
raw per-channel file count, including own messages, event messages, and files
that would fail message parsing; a directory listing error is represented as
zero for that channel (`src/channel.rs:1327-1334`).

The JSON projection is defined by `ChannelListItem` and `ChannelsOutput`
(`src/output.rs:199-216`), and text renders the same raw `messages` number
(`src/commands/channels.rs:21-42`). The additive field seam is
`ChannelListItem` plus the `channels::run` mapping. The current code has no
per-room unread count in a channel item; the exact new field name is not
present and is therefore not inferred here.

### Direct-mail inbox

`inbox::run` resolves the room/inbox, scans every `.mail` path, parses valid
messages, warns and skips malformed or unreadable files, sorts valid items by
id, and sets `count = unread.len()` (`src/commands/inbox.rs:7-35`). It then
constructs `InboxOutput { ok, room, unread, count, skipped_unreadable }`
(`src/commands/inbox.rs:63-70`; shape at `src/output.rs:469-476`). The existing
`unread` field is already a vector and `count` is already a physical-inbox
count; it is not cursor-derived. A new cursor-aware scalar must be additive
or the existing vector/count compatibility changes.

### Read-only status and creation caveat

`channels` does not call `mailbox_dirs`; `inbox` does, and the latter can create
missing inbox/read directories outside the read-only guard
(`src/mailbox.rs:413-431`). Under enrolled/fenced execution the dispatcher
sets the guard and suppresses those creates (`src/commands/mod.rs:34-38`).
Unread counting must not introduce cursor writes, room-parent creation, or
heartbeat/banner writes into listings.

## 5. Store primitives, ids, ordering, and `--since`

### File records and parsers

Direct mail is one file per message: `parse_mail` reads the whole file, splits
the JSON envelope from the raw body at `\n---\n`, validates the envelope, and
requires the filename stem to equal the envelope id
(`src/mailbox.rs:566-593`). `mail_files` scans a directory for regular
`.mail` files and sorts the paths lexicographically
(`src/mailbox.rs:645-665`).

Channel messages use the same envelope/separator/body layout. `parse_channel_message`
validates JSON, message fields, canonical id, and filename/id agreement
(`src/channel.rs:1082-1109`). `message_files` scans regular `.msg` files and
sorts paths lexicographically (`src/channel.rs:1280-1293`). There is no generic
JSONL store abstraction: the only JSONL append/read code found is the
best-effort crossed-send telemetry, which appends one line and later scans the
whole file as text (`src/channel.rs:783-848`).

### Id formats and ordering

Mail ids are generated from a UTC timestamp plus a six-hex suffix
(`src/mailbox.rs:917-942`, `src/mailbox.rs:957-962`) and validated as
`YYYYmmdd-HHMMSS-<6 hex>` (`src/mailbox.rs:609-623`). Channel ids use UTC
timestamp, six decimal microsecond digits, and a six-hex suffix
(`src/mailbox.rs:879-887`; canonical validation at
`src/channel.rs:1143-1224`). The microsecond component exists specifically to
avoid same-second high-water skips.

The code gives distinct id strings a total lexicographic order within a scan,
not a durable global arrival order. `collect_batch` filters `--since` with
strict `id > bound` and sorts the resulting messages by id
(`src/commands/chat.rs:661-703`). `ChannelState::unseen_candidates` uses the
same string comparison for an at-or-below target
(`src/channel_state.rs:289-307`). `find_channel_message` sorts ids inside each
channel and computes history depth from that order
(`src/channel.rs:1235-1274`). Cross-channel ids are never compared as one
ordered stream. Backfilled or concurrently published files can therefore be
lexically ordered without being an arrival sequence; the current seen-set is
what preserves a late id below a newer consumed id.

### Atomic publication

`exclusive_atomic_write_with` writes a private `0600` temp file, syncs it,
hard-links it into an absent destination as the commit point, and only then
cleans up/syncs the parent (`src/mailbox.rs:762-807`). It never replaces an
existing message file. `exclusive_atomic_write` supplies the normal cleanup
wrapper (`src/mailbox.rs:667-672`). `atomic_replace` writes a synced temp file
and renames it over the destination while refusing symlink replacement and
preserving the prior mode (`src/mailbox.rs:674-722`). Channel state uses the
latter under its room lock (`src/channel_state.rs:206-240`).

Direct sends publish inbox then archive with the exclusive primitive
(`src/commands/send.rs:283-337`); channel sends publish
`messages/<id>.msg` the same way and retry an `AlreadyExists` collision
(`src/channel.rs:1009-1056`). Any new cursor write needs the same atomic
replace/lock discipline, not an append to message history.

## 6. CLI schema and doctor seams

The parsed command enum and current twelve-command surface are in
`src/cli.rs:44-70`; command execution dispatch is
`src/commands/mod.rs:65-77`. `post schema` is hand-assembled, not reflected
from clap: command usages and side-effect prose are listed at
`src/commands/schema.rs:45-118`, output field strings at
`src/commands/schema.rs:119-202`, and environment/law text at
`src/commands/schema.rs:232-280`. The serializable `OutputShapes` struct is
`src/output.rs:560-577`. A new `catchup` or `search` command and every new
output field therefore need all of: CLI enum/args, dispatch/module,
schema command entry, output-shape entry, and the corresponding output struct.

Doctor's channel pass is the existing state-file inspection seam. It validates
channel metadata, members, messages, and each registered room's
`channel-state.json` through the shared parser (`src/commands/doctor.rs:253-395`).
The mailbox scan only walks archive/inbox/read and parses `.mail`
(`src/commands/doctor.rs:528-580`). `doctor --fix` only creates root/defaults,
archive, and missing room inbox/read directories
(`src/commands/doctor.rs:617-631`). There is currently no `cursors.json`
check, no cursor-lock check, and no cursor repair path. Adding one must keep
the advisory/fail-open rule from the goal lock separate from `doctor --fix`'s
create-only behavior.

`doctor` itself is read-only unless `--fix`; its output/finding accounting is
at `src/commands/doctor.rs:17-95`. Under a fence, `doctor` without `--fix`
remains in the read-only path, while `doctor --fix` is classified as a writer
by `src/migration_fence.rs:484-498`.

## 7. Test infrastructure and existing coverage

### Integration harness

`tests/cli.rs` defines a per-test `Sandbox` with a unique temp path, isolated
`HOME`, and isolated `POST_MAIL_ROOT` (`tests/cli.rs:19-88`). `run_in` and
`run_in_env` spawn the compiled binary with piped stdout/stderr, clear identity
and fence environment variables by default, and optionally feed stdin
(`tests/cli.rs:90-194`). `Drop` removes only that sandbox
(`tests/cli.rs:212-225`). Helpers for fence fixtures, channel fixtures,
room registration, joining, and hand-written messages are at
`tests/cli.rs:3255-3441`.

### Existing channel/read seams

- `channel_two_room_flow_lists_members_and_advances_read_cursor` exercises
  join, channel send, peek, plain consuming read, empty reread, and channel
  listing (`tests/cli.rs:2732-2787`).
- `src/commands/chat.rs:1197-1234` covers batch ordering, emit-then-consume,
  and re-show after no advance.
- `src/commands/chat.rs:1357-1418` covers cursorless since/history and the
  newest-25 catch-up trim.
- `src/commands/chat.rs:1855-1937` covers fail-closed unreadable messages and
  the concurrent-arrival callback boundary.
- `src/channel_state.rs:578-860` covers persistence, replay, per-room/per-channel
  isolation, late arrivals, lock exclusion, and concurrent marks. The v1/v2
  migration/fence cases continue at `src/channel_state.rs:894-1050`.

### Existing watch/doorbell seams

- `tests/cli.rs:4126-4162` covers startup backlog, live direct mail, and no
  body emission.
- `tests/cli.rs:4243-4264` pins default startup backlog replay; this is the
  integration counterpart to the quoted source invariant.
- `tests/cli.rs:4297-4366` round-trips a digest's `[first..last]` and exclusive
  `--since` fencepost.
- `tests/cli.rs:4405-4468` proves snapshot emits direct/channel metadata twice
  without consuming mail or channel state; `tests/cli.rs:2899-3061` covers
  multi-room deduplication and the watched-vs-owned distinction.
- `src/commands/watch.rs:1062-1159` covers digest grouping, sender caps,
  snapshot limits, and empty batches; `src/commands/watch.rs:1164-1556`
  covers ownership suppression, event wakes, starvation, and directory
  re-registration.

### Existing fence seams

`tests/cli.rs:4884-4984` covers legacy/fenced/active command matrices and
read-only behavior with malformed generation declarations. The bounded stdout
stall test proves the root lock remains held through a consuming chat read's
after-stdout state move (`tests/cli.rs:5003-5053`). The running-watch test
proves heartbeat re-admission and termination after fence change
(`tests/cli.rs:5145-5210`). Unit transition/admission cases are also in
`src/migration_fence.rs:541-712`.

### Smoke and broad gates

`scripts/smoke-installed.sh:1-59` is a `set -eu` installed-binary smoke. It
creates a throwaway root, runs doctor bootstrap, registers two rooms, checks
default watch backlog, checks `--from now`, checks the parse conflict with
snapshot, then joins/sends on a channel and round-trips a digest `--since`
fencepost. It currently has no catchup, unread-count, mail-cursor, or search
assertions.

The broad Rust/Node/doorbell/schema gate is `scripts/gate.sh:20-60`, with the
same required commands listed in `CONTRIBUTING.md:5-24`. Plan B's new
integration tests belong in `tests/cli.rs`; small state/ordering tests can sit
beside `channel_state.rs` or `chat.rs`; the installed-binary acceptance rows
can extend the smoke script after its existing channel section.

## Danger list

These are current invariants Plan B must preserve. Each item names the anchor
where the invariant is implemented and the accidental break to avoid.

1. **Fence admission must remain one held lock.**
   `src/commands/mod.rs:79-85`, `src/migration_fence.rs:402-426`, and
   `src/app.rs:139-156` keep an enrolled writer's root flock through stdout and
   its after-stdout cursor/read move. Reacquiring for a cursor callback or
   moving the write outside the captured admission lets cutover slip through.
2. **Read-only forms must stay non-mutating under a fence.**
   `src/commands/mod.rs:34-38`, `src/mailbox.rs:413-431`, and
   `src/migration_fence.rs:306-316` prevent root/room/cursor initialization on
   peek, listings, schema, doctor, and snapshot paths. Calling a cursor writer
   from a count or watch scan would turn observation into admission or create
   state behind a fenced store.
3. **Default watch must replay backlog and keep late lower ids visible.**
   `src/commands/watch.rs:295-302` and `src/channel_state.rs:424-468` make the
   startup seen floor read-only and preserve a late id below a newer seen id.
   Replacing the floor with a max-id cursor or treating catchup as watch state
   would silence a legitimate ring.
4. **Watch rings never advance consumption state.**
   `src/commands/watch.rs:782-795` and `src/commands/watch.rs:957-973` load
   state only; `tests/cli.rs:4405-4468` pins repeatable snapshot output. A
   shared cursor write in watch would make a notification erase the message
   catchup is meant to report.
5. **Consuming reads advance only after successful output.**
   `src/commands/chat.rs:251-262`, `src/commands/read.rs:45-80`, and
   `src/app.rs:139-156` enforce emit-then-consume. Advancing before flush, or
   marking a broader rescan than the selected ids, loses unread messages on a
   broken pipe or a mid-flight arrival.
6. **The selected chat batch is a fixed id set.**
   `src/commands/chat.rs:139-151` and `tests/cli.rs:1907-1937` show that a
   callback must mark the ids captured before display trimming. Re-enumerating
   all unread messages in the callback can consume a message that arrived after
   the receipt was rendered.
7. **Seen-set locking must remain whole-map and per-room.**
   `src/channel_state.rs:171-240` and `src/channel_state.rs:484-510` reload,
   union, and atomically replace under one room lock. A per-channel file write
   or unlocked read-modify-write can lose a concurrent mark on another channel.
8. **Membership is the visibility boundary for channels.**
   `src/commands/chat.rs:640-660` and `src/channel.rs:152-166` require a
   registered acting room and channel membership. A search or unread-count scan
   that walks every channel without filtering membership leaks another room's
   messages.
9. **Id comparison is exclusive and string-based.**
   `src/commands/chat.rs:669-671` and `src/commands/watch.rs:141-151` define
   `--since` and the digest fencepost. Changing `id > bound`, dropping
   microseconds, or using an inclusive fencepost duplicates or omits the first
   ring message.
10. **Message/history files are immutable and atomically published.**
    `src/mailbox.rs:762-820` and `src/channel.rs:1009-1056` use no-replace
    hard-link commits for message files, while `CONTRACT.md:90-110` makes
    inbox/archive/channel history durable. A cursor implementation must not
    rewrite message files or use a non-atomic append where a reader can see a
    partial record.
11. **Unread count semantics differ between channels and inbox.**
    `src/channel.rs:1327-1334` counts `.msg` files without parsing, while
    `src/commands/inbox.rs:11-35` counts only parseable inbox mail. Reusing one
    raw-file counter for both changes existing error/visibility behavior and can
    report malformed channel files as message totals or misstate unreadness.
12. **Doctor repair remains create-only.**
    `src/commands/doctor.rs:617-631` never repairs mail, history, membership,
    or cursor state. Letting `doctor --fix` rewrite a corrupt advisory cursor
    would violate the current no-data-loss repair boundary.
13. **Schema cannot drift from the CLI.**
    `src/commands/schema.rs:45-118`, `src/commands/schema.rs:119-202`, and
    `src/output.rs:560-577` are manually maintained. Adding a command or field
    only to clap/output makes `post schema` lie to machine consumers.
14. **Direct mail's physical move and any new mail cursor must agree.**
    `src/commands/read.rs:45-80` moves inbox to read only after stdout, while
    `src/commands/inbox.rs:7-35` defines unread from inbox contents. A second
    mail cursor that advances independently can report unread zero while the
    message remains in inbox, or report unread twice after a move failure.
