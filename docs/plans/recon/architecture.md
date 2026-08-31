# Plan B architecture: stateful read layer

Status: architecture for plan compilation. This document makes design decisions;
it does not authorize implementation or reopen the locked rulings in
`docs/plans/plan-b-goal-lock.md:52-84`.

## 0. Reconciliation with the 0.8.0 checkout

Five facts in the live checkout change how the locked goal must be implemented.

1. **Messages are not stored in JSONL.** Direct mail is one `.mail` file per
   message, parsed as JSON envelope + `\n---\n` + raw body
   (`src/mailbox.rs:566-593`); channel history is one immutable `.msg` file per
   message with the same framing (`src/channel.rs:1009-1056`,
   `src/channel.rs:1082-1108`). The only relevant directory readers sort file
   paths (`src/mailbox.rs:645-665`, `src/channel.rs:1280-1293`). There is no
   message-history JSONL byte offset or line number to persist. The only JSONL
   path found here is best-effort `crossed-send.jsonl` telemetry: it appends one
   line and reverse-scans text for a recent refusal
   (`src/channel.rs:783-848`); it is not a message store.
2. **0.8.0 already has per-room/per-channel consumption state.** It is an exact
   seen-ID set in `<room>/channel-state.json`, not a scalar cursor
   (`src/channel_state.rs:1-21`, `src/channel_state.rs:78-115`). The set exists
   specifically so a late imported ID below the current maximum remains unread
   (`src/channel_state.rs:10-15`, `src/channel_state.rs:728-743`). The new design
   must replace or absorb this state, not layer an unrelated watermark beside
   it.
3. **Direct mail already has authoritative physical read state.** A successful
   consuming `read` hard-links inbox mail into `read/` and removes the inbox
   link only after stdout succeeds (`src/commands/read.rs:40-80`,
   `src/mailbox.rs:809-820`). A mail cursor is therefore advisory metadata; it
   must never make a still-unread inbox file disappear before that move commits.
4. **The locked watch line reference has drifted.** Current
   `src/commands/watch.rs:266-273` resolves mailbox paths. The hard ring path is now
   `src/commands/watch.rs:738-837`, especially its read-only channel branch at
   `src/commands/watch.rs:782-815`. The plan should protect the behavior and
   that block, not stale line numbers.
5. **The categorical ring wording is broader than current behavior.** A watch
   captures consumed channel IDs at startup and skips them as backlog
   (`src/commands/watch.rs:275-283`, `src/commands/watch.rs:788-795`). Acceptance
   row 4 instead arms the watch before catchup and requires later sends to ring
   (`docs/plans/plan-b-goal-lock.md:39-44`). This architecture preserves that
   testable live-watch invariant and the existing startup floor. If "catchup
   never suppresses rings" was intended to make a newly started watch replay
   already-caught-up backlog, that is incompatible with 0.8 behavior and needs
   an explicit contract override; it is not silently assumed here.

These are flags, not attempts to reopen the goal. The architecture follows the
locked room ownership, consuming-read, read-only-listing, linear-search, and
advisory-degradation rulings (`docs/plans/plan-b-goal-lock.md:57-63`). I do flag
one interpretation as wrong: a "cursor" cannot mean one maximum message ID.
That would reintroduce the late-arrival loss already reproduced by the current
tests (`src/commands/chat.rs:1305-1354`). Here, **cursor** means the persisted
read-position abstraction; its value is an exact set of consumed message IDs.

## 1. State module and cursor file

### 1.1 One deep module

Replace the channel-only state implementation with one `cursor_state` module.
Do not add a second pass-through state layer over `channel_state.rs`. The module
owns JSON parsing, v0.8 import, advisory degradation, the per-room lock, exact
set union, mail-move ordering, serialization, and atomic replacement. Callers
see only:

- a read guard/snapshot with `mail_has_seen(id)` and
  `channel_has_seen(channel, id)` queries; and
- one consumption operation taking a fixed delta of mail moves and
  `(channel, id-set)` additions.

Existing `--discard-through` and `--seen-by` should move behind this same
interface rather than retain an alternate state writer. Their current behavior
already depends on exact membership, not on the maximum ID
(`src/channel_state.rs:152-168`, `src/commands/chat.rs:620-703`). The deletion
test is decisive: deleting this module would otherwise scatter locking,
degradation, migration, and whole-map union across `read`, `chat`, `catchup`,
`channels`, `inbox`, `watch`, and `doctor`.

### 1.2 Exact on-disk shape

Path: `<root>/<room>/cursors.json`, mode `0600`.

```json
{
  "version": 1,
  "mail": {
    "seen": [
      "20260831-171234-a1b2c3"
    ]
  },
  "channels": {
    "machineroom-devbox": {
      "seen": [
        "20260831-171234-123456-a1b2c3"
      ]
    }
  }
}
```

Rules:

- `mail.seen` is the one per-room mail cursor. Each channel entry is the one
  per-room/per-channel cursor. Values are sorted, duplicate-free arrays of full
  canonical message IDs. Maps and sets serialize in lexical order, pretty, with
  one trailing newline.
- Channel IDs must satisfy the current 29-byte
  `YYYYmmdd-HHMMSS-UUUUUU-<6 hex>` predicate
  (`src/channel.rs:1213-1224`). Mail IDs must satisfy the current 22-byte
  `YYYYmmdd-HHMMSS-<6 hex>` predicate (`src/mailbox.rs:609-623`). IDs use UTC
  for the sortable timestamp while human `sent` remains local time
  (`src/mailbox.rs:917-962`).
- Missing channel keys and missing/empty sets mean nothing seen. Unknown fields,
  a non-1 version, invalid IDs, unreadable bytes, or malformed JSON make the
  **whole snapshot** advisory-invalid. The command emits one sanitized stderr
  warning and uses an empty snapshot: all current messages are unread. It never
  partially salvages a document and never fails a read. Here "all unread" means
  an empty consumed set; permanent eligibility rules such as `from != self` and
  channel membership still apply.
- A content-invalid regular file is replaced by the next admitted consuming
  write using only the IDs that write actually consumed. This loses prior
  advisory knowledge but cannot lose message history; the safe failure is
  rereading. `doctor` remains the place to diagnose the file before that
  happens.
- A symlink or non-regular `cursors.json` also degrades to all unread on
  read-only surfaces, but a writer refuses it without following or replacing
  it. Likewise, an unwritable store can still fail a write. The locked "never an
  error" rule is safe only as a **read-time load rule**; it cannot require a
  mutating command to claim persistence against an unsafe or unwritable path.
  That narrower interpretation is an explicit safety flag, not silent scope
  expansion.
- State grows linearly. Retention and compaction are out of scope. Keep the
  current 50,000-ID per-channel warning policy; the present code already records
  why watermark-plus-exceptions is unsafe without an arrival sequence
  (`src/channel_state.rs:31-48`). Add the same warning threshold for `mail.seen`.

- **Ruling:** the cursor value is an exact sorted set of message IDs.
- **Losing option, maximum message ID:** smaller, but it permanently hides an ID
that arrives later below the maximum and cannot represent an out-of-order direct
mail read (`src/channel_state.rs:122-168`, `src/commands/read.rs:25-47`).
- **Losing option, byte offset:** no shared append stream exists; records are
separate immutable files.
- **Losing option, line count:** same structural mismatch, plus directory arrival
order is not a durable order.

### 1.3 v0.8 transition

`cursors.json` is canonical once it exists. When it is absent:

1. load a valid v0.8 `channel-state.json` in memory as the channel baseline;
2. start `mail.seen` empty; and
3. write neither file on read-only commands.

The first consuming operation reloads that baseline under the new room lock,
unions its fixed delta, and writes `cursors.json`. It leaves
`channel-state.json` untouched as rollback evidence and never dual-writes it.
If neither file exists, or if the legacy file cannot be safely parsed, the
baseline is empty/all unread. This makes the acceptance-row-7 store work without
creating anything, while preserving valid state in real 0.8 rooms. v0.8 state
is itself lazily loaded today (`src/channel_state.rs:91-98`,
`src/channel_state.rs:344-468`). Mixed old/new writers are not supported; the
goal lock already declares no compatibility window (`docs/plans/plan-b-goal-lock.md:52-63`).

- **Ruling:** new unified file with read-only legacy import and one-way first-write
materialization.
- **Losing option, extend `channel-state.json` with mail:** less migration code,
but contradicts the locked unified cursor naming, makes the file name false, and
keeps old binaries capable of misparsing a new mail-bearing shape.
- **Losing option, ignore valid v0.8 state:** mechanically satisfies "missing =
all unread" but forces active rooms to reread their entire history after upgrade.

### 1.4 Lock and atomic replacement

Use `<root>/<room>/.cursors.lock`, mode `0600`, for the whole document. A writer:

1. creates the room directory only on a writer path;
2. opens the lock with `O_CLOEXEC|O_NOFOLLOW`, takes `LOCK_EX`, and verifies that
   the held descriptor and on-path entry are the same solitary regular inode;
3. reloads canonical/legacy state under the lock;
4. performs any direct-mail moves;
5. unions only the fixed IDs selected before stdout; and
6. atomically replaces `cursors.json` while still holding the lock.

Use the existing synced-temp + rename primitive: it refuses a symlink, preserves
mode, writes/syncs the temporary file, renames it, and syncs the parent
(`src/mailbox.rs:674-721`). Whole-map reload and union under one room lock is the
existing lost-update defense (`src/channel_state.rs:171-250`). Harden the new
lock to the root-fence inode checks rather than copying the weaker current
channel lock; the root lock rechecks the inode after flock
(`src/migration_fence.rs:200-264`), while the old channel lock only uses
`O_NOFOLLOW` + flock (`src/channel_state.rs:484-510`).

A listing may open an existing cursor lock read-only and take `LOCK_SH` while it
loads state and computes counts. It must not create the lock or room directory.
If the lock is absent, an absent cursor is a valid empty snapshot; a present
cursor without its lock degrades to all unread and is a doctor warning. This
keeps listings read-only while giving established state a coherent snapshot
against concurrent cursor writers.

## 2. Fence integration and mutation ordering

### 2.1 Existing admission seam

Writer admission already belongs at the command dispatcher. It classifies the
command, admits a writer before dispatch, and activates the read-only guard for
protected reads (`src/commands/mod.rs:21-39`). An enrolled writer retains the
same root flock through stdout and its `after_stdout` mutation by capturing the
admission object in the callback (`src/commands/mod.rs:79-85`). Stdout is fully
written and flushed before that callback runs (`src/app.rs:139-156`). The actual
generation check and retained root lock are at
`src/migration_fence.rs:359-426`.

Add `Catchup(_) => true` and `Search(_) => false` to the authoritative
classification matrix. Plain `read` and plain `chat` remain writers; `read
--peek`, `chat --peek`, `chat --history`, `chat --since`, listings, and
`watch --snapshot` remain read-only as they are now
(`src/migration_fence.rs:484-515`). An empty `catchup` is still a writer-intent
command and is refused under a fence without the active generation, matching an
empty plain consuming `chat` today.

### 2.2 No wider root-lock window

Do not reacquire the root fence inside cursor code. `read`, plain `chat`, and
`catchup` return one `after_stdout` callback; the dispatcher-held admission is
already alive inside it. Cursor code takes the narrower per-room lock only in
that callback, not while selecting, rendering, or writing stdout. That preserves
the existing emit-then-consume boundary used by chat
(`src/commands/chat.rs:139-151`, `src/commands/chat.rs:251-262`) and mail
(`src/commands/read.rs:40-80`).

`catchup --all` necessarily holds the already-admitted root flock for its own
selection and stdout, exactly as any enrolled consuming read does; it does not
add an earlier admission or a second lock interval. Search and unread listings
take no root writer admission. A read-only shared `.cursors.lock` guards only a
room snapshot and is not a store mutation.

### 2.3 Fixed delta and mail ordering

Selection captures exact full IDs before rendering. A message appearing between
selection and the callback is not in the delta and stays unread. This is the
same fixed-batch invariant current chat documents at
`src/commands/chat.rs:139-151` and tests for late arrival at
`src/commands/chat.rs:1305-1354`.

For fresh direct mail, the cursor transaction orders side effects as follows:

1. stdout succeeds;
2. under `.cursors.lock`, create the no-replace `read/<id>.mail` hard link and
   remove `inbox/<id>.mail`;
3. only then add the ID to `mail.seen`; and
4. atomically replace `cursors.json`.

If creating the read link fails, do not mark the ID. If the read link commits
but inbox unlink fails, mark the ID (the body was emitted and the read copy is
committed), then return the existing delivered-output failure describing the
duplicate. If the cursor write fails after a successful move, the physical
inbox state remains authoritative and the advisory cursor may lag; it must never
lead the move. For a batch mail catchup, stop moving at the first hard-link
failure, persist IDs whose read copies committed, and leave the rest to re-show.
This makes every partial failure conservative. Channel sends continue to mark
the sender's own ID in the same unified state;
the read predicate must still exclude `from == self` if that best-effort mark
failed, as current chat does (`src/commands/chat.rs:692-699`).

### 2.4 Read-only surfaces

The following only load a snapshot and never create a room, lock, cursor,
banner-day, heartbeat, or temp file:

- `read --peek`, `chat --peek`, `chat --history`, and `chat --since`;
- `inbox`, `channels`, `search`, `schema`, and `doctor` without `--fix`;
- `watch --snapshot`; and
- the watch startup channel floor.

The dispatcher already suppresses first-run mutation for protected read paths
(`src/commands/mod.rs:34-39`), and mailbox directory enumeration already returns
an empty set instead of creating under that guard (`src/mailbox.rs:645-665`).
Unread computation is cursor-READ-only even on an unfenced legacy store.

Long watch remains special: startup admission is dropped before the loop, and
each heartbeat re-admits narrowly (`src/commands/mod.rs:40-43`,
`src/commands/watch.rs:491-497`). Plan B must not put cursor writes in that loop.

## 3. `post catchup`

### 3.1 CLI

```text
post catchup [<channel> | --mail | --all] [--framing auto|full|compact]
```

- No selector is an alias for `--all`; explicit `--all` exists for scripts.
- A positional channel catches up exactly one channel and requires membership.
- `--mail` catches up direct mail only.
- `--all` catches up direct mail, then every joined channel in lexical channel
  order.
- The three selectors conflict. There is no `--room`: catchup acts as the
  registered room resolved from `POST_FROM`/cwd, like channel operations
  (`src/channel.rs:152-166`). There is no `--since`, `--history`, `--peek`, or
  `--limit` in v1. `catchup` means the complete unread slice; `chat` and `inbox`
  already supply non-consuming and bounded views.
- Global `--json` and `--pretty` apply. Redirecting a non-empty catchup to
  `/dev/null` refuses before output/state mutation, preserving the existing
  consuming-chat safety rule (`src/commands/chat.rs:167-186`).

System join/profile events are channel messages and participate in unreadness,
matching plain chat. Own channel messages do not. Any unread channel entry that
cannot be parsed fails that target closed before stdout, so no unseen channel
ID is consumed without being rendered (`src/commands/chat.rs:628-701`). Mail
files that cannot be parsed are warned and left unread; valid mail in the batch
can still be delivered and moved, matching inbox's current isolation posture
(`src/commands/inbox.rs:7-35`).

### 3.2 Output

JSON is one stable envelope for every selector:

```json
{
  "ok": true,
  "room": "post-devbox",
  "targets": [
    {
      "source": "mail",
      "framing": {"source": "another_ai_agent", "authority": false, "laws": ["..."]},
      "messages": [{"envelope": {}, "body": "..."}],
      "count": 1
    },
    {
      "source": "channel",
      "channel": "machineroom-devbox",
      "framing": {"source": "multiple_ai_agents", "authority": false, "laws": ["..."]},
      "messages": [{"id": "...", "from": "...", "sent": "...", "body": "..."}],
      "count": 2
    }
  ],
  "count": 3
}
```

The example abbreviates existing envelopes and law arrays; implementation reuses
the real `Envelope`, `Framing`, `ChannelFraming`, and `ChatMessageItem` shapes
(`src/output.rs:135-193`, `src/output.rs:478-523`). Selected targets remain in
`targets` with `messages: []` and `count: 0`, so `--all` says what it inspected.
Top-level `count` is the sum. There is no `advanced` claim in bytes emitted
before the callback; success exit after the callback is the commit receipt.

Human output uses one section per non-empty target and a final total. An entirely
empty result is one line, exit 0: `post: caught up (0 unread)`. An immediate
second invocation is therefore empty after a successful first invocation and
fresh process start.

**Framing proposal (open contract question, not a locked ruling):** for new
multi-message `catchup` and `search` surfaces, `auto` emits the compact framing
once per non-empty invocation, above all sections/results. `full` emits the full
wall once; `compact` explicitly emits the compact line once; there is no `none`.
JSON carries structured framing on each catchup target and once at top level for
search. Empty results contain no body and need no human banner, but retain the
JSON framing field for shape stability. This keeps every body-bearing surface
framed without multiplying a wall by target or message. Existing `read`/`chat`
framing behavior is unchanged; their modes are already defined at
`CONTRACT.md:188-200` and `CONTRACT.md:286-302`.

### 3.3 Relationship to `chat --since` and watch fenceposts

`chat --since <id>` remains an exclusive, cursorless historical read. It ignores
seen state (`src/commands/chat.rs:115-151`, `src/commands/chat.rs:669-701`) and
never advances it. Watch digest text deliberately lowers the first ID's final
character to `!`, so exclusive `--since` includes the first ring ID
(`src/commands/watch.rs:95-151`). Plan B does not reinterpret either bound.

Consequences:

- following a ring's copyable `--since` suffix is read-only and a later catchup
  may repeat those messages;
- `catchup <channel>` consumes all currently unread messages, not only one
  digest interval; and
- a watcher started after catchup uses the updated consumed set as its startup
  backlog floor, matching 0.8. The locked no-suppression test applies to a
  watcher already armed before catchup, as acceptance row 4 specifies; and
- rings never read or write `cursors.json` after startup. A running watcher keeps
  the read-only channel floor captured into `WatchTarget` at startup
  (`src/commands/watch.rs:18-31`, `src/commands/watch.rs:275-283`). A concurrent
  catchup cannot retroactively suppress that process's rings.

## 4. Unread counts

### 4.1 JSON and text additions

Keep every existing field and meaning.

- `channels` adds top-level `room: string|null` and `unread: number|null` to
  each channel item. `room` is the registered `POST_FROM`/cwd identity used for
  counts. A joined channel gets a number; a non-member channel or an invocation
  with no registered acting room gets `null`, never a misleading zero. The
  existing `messages` field remains the raw `.msg` file count
  (`src/commands/channels.rs:8-53`, `src/channel.rs:1302-1338`). Text appends
  `, N unread` only for joined channels.
- `inbox` adds top-level `unread_count`. Existing `unread` and `count` retain
  their physical-inbox compatibility meaning; in a healthy store all three
  agree. If a committed read copy remains hard-linked in inbox after an unlink
  failure, `unread_count` may be lower because `mail.seen` suppresses the known
  duplicate, while the legacy fields still expose it for reconciliation. The
  current output shape is at `src/output.rs:469-476`. These additions do not
  silently change parsers that consume the existing `messages`, `unread`, or
  `count` fields, satisfying ruling 9.

### 4.2 Exact computation

Load the invoking room's cursor snapshot once before scanning and, when an
existing lock is available, retain its shared guard through the listing. That
single snapshot is the count baseline for every channel in the response.

For a member channel:

```text
unread = count(message file where
  filename_id is not in channels[channel].seen
  and (the message is unreadable or parsed.from != acting_room))
```

An unreadable unseen file counts as one unhandled item because consuming chat
would stop on it and watch would ring it; an already-seen malformed file is
ignored, matching current chat's predicate (`src/commands/chat.rs:661-699`).
Events count like other channel messages. Parse only IDs absent from the seen
set; caught-up channels need directory enumeration and membership checks but no
message-body opens.

For inbox:

```text
unread_count = count(parseable inbox mail whose id is not in mail.seen)
```

Malformed/I/O-unreadable files remain excluded from the numeric count and
reported through existing warnings/`skipped_unreadable`, preserving
`src/commands/inbox.rs:7-35`. Neither listing writes recovery state when the
snapshot is missing or invalid.

### 4.3 Cost and rejected shortcut

Current `post channels` already enumerates all channel message directories to
populate raw totals (`src/channel.rs:1302-1338`). A read-only measurement on
2026-08-31 against `/home/trey-agent/.claude-mail` found:

```text
post 0.8.0
8 channels / 943 .msg files / 1,340,620 bytes total
post-devbox is a member of 1 channel / 621 .msg files
527 inbox+read+archive .mail links / 673,056 bytes total
```

The measurement used `post channels | jq` plus read-only `find ... -name
'*.msg'/'*.mail'` counts and byte sums. On this live room, a missing cursor is
the worst channel-count case: enumerate 621 paths and parse 621 small files.
Once caught up, it still enumerates those 621 paths but parses none. Across a
room joined to all live channels, the upper bound is 943 path checks and at most
943 parses, linear in visible history and roughly 1.34 MiB today. Search is linear
in the same visible files plus party-visible mail.

There is no safe length/offset shortcut. Subtracting seen count from total files overstates
when a sender's own-ID mark failed, cannot validate stale/nonexistent seen IDs,
and loses the conservative unreadable-file rule. A byte offset or line count
does not exist in a per-file store. The implementation should reuse the one
`message_files` enumeration already needed for `messages`, not perform a second
directory scan.

- **Ruling:** one path enumeration per channel, exact set membership, and parse
only unseen candidates.
- **Losing option, directory length minus set length:** cheap but not exact under
own-send mark failure and manual/legacy state.
- **Losing option, introduce an unread counter/index:** makes listings cheap by
adding a second mutable truth source that can drift; ruling 8 rejects the same
trade for search.

## 5. `post search`

### 5.1 CLI and match semantics

```text
post search <pattern> [--mail | --channel <channel>]
            [--limit <1..=1000>] [--framing auto|full|compact]
```

- Default scope is all party-visible direct mail plus all joined channels.
- `--mail` restricts to direct mail. `--channel` restricts to one channel and
  requires membership; they conflict. There is no `--room` and no cursor effect.
- Default limit is 100; the hard maximum is 1000. Zero/unlimited is rejected.
  Search records are returned newest first by a deterministic key derived from
  the UTC ID timestamp, channel microseconds (mail uses zero), full ID, and
  source. Exact cross-source order inside one second is unknowable for mail and
  is not claimed.
- Pattern matching is a case-insensitive **literal Unicode substring** over
  body, subject, sender ID, and message ID. The result records which fields
  matched. Empty/control-containing patterns are rejected. There is no regex in
  v1; one-channel regex history already exists as `chat --history --grep`
  (`src/commands/chat.rs:300-325`).

- **Ruling:** literal substring by default and only in v1.
- **Losing option, regex default:** more expressive but makes ordinary punctuation
special, produces avoidable usage errors, and does not improve the session-start
"find this phrase" job.

### 5.2 Visibility boundary

Resolve one registered acting room before collecting candidates. Then:

- direct-mail candidates come from that room's `inbox/` and `read/`, plus
  archive entries whose parsed envelope has `from == room || to == room`; this
  is the same party filter used by direct `read` fallback
  (`src/commands/read.rs:95-123`);
- deduplicate mail by full ID, preferring inbox, then read, then archive; and
- load each channel's `members.json` and reject/filter **before opening any
  message file**. Only channels whose member map contains the acting room may be
  scanned. Plain chat enforces the same membership boundary before message
  enumeration (`src/commands/chat.rs:640-660`).

A non-member result is a security bug even if only its ID or count leaks. An
invalid membership file fails that channel closed. Malformed party-visible mail
or member-channel messages warn and skip, as cursorless history does
(`src/commands/chat.rs:676-689`); malformed bytes never widen scope.

### 5.3 Output

JSON:

```json
{
  "ok": true,
  "framing": {"source": "multiple_ai_agents", "authority": false, "laws": ["..."]},
  "room": "post-devbox",
  "pattern": "fence",
  "match": "literal_case_insensitive",
  "results": [
    {
      "source": "channel",
      "channel": "machineroom-devbox",
      "id": "...",
      "from": "...",
      "sent": "...",
      "subject": "...",
      "preview": "...",
      "matched": ["body"]
    }
  ],
  "count": 1,
  "limit": 100,
  "truncated": false
}
```

Mail results use `source: "mail"`, `channel: null`, and add `kind`. `preview` is
a sanitized, newline-flattened excerpt capped at 160 Unicode scalar values;
full bodies remain available through `read` or `chat --history`. Human output is
one sanitized line per result with source, full ID, sender, `sent`, subject, and
preview. A no-match result is exit 0, `results: []`, `count: 0`,
`truncated: false`. Determine `truncated` by finding one match beyond the cap;
do not claim a total match count that was not computed.

Search is always read-only. It does not consult or advance `cursors.json`, move
mail, stamp a banner-day, or affect watch.

## 6. Contract, schema, doctor, and smoke amendments

### 6.1 `CONTRACT.md` outline

1. **Non-negotiable laws / immutable history:** add cursor state to the list of
   mutable delivery state; state never edits message/history files. Keep the
   current mail move and append-only channel laws (`CONTRACT.md:31-40`).
2. **On-disk format:** specify `cursors.json` v1 and `.cursors.lock`, exact
   seen-ID semantics, sorted pretty serialization, 0600 mode, atomic replace,
   advisory invalid/missing behavior, v0.8 read-only import, first-write
   materialization, and no dual writes. Amend the current channel-state section
   at `CONTRACT.md:115-151` rather than leaving two contradictory models.
3. **Fence matrix:** list `catchup` as a writer and `search` as read-only. State
   that listings and watch snapshots may take an existing shared cursor flock
   but create nothing. Preserve the root-flock-through-stdout rule at
   `CONTRACT.md:51-80`.
4. **Commands:** add the exact catchup/search grammar and output shapes; amend
   `channels`/`inbox` fields. Preserve `chat --since` exclusivity and the watch
   digest fencepost text at `CONTRACT.md:326-414`.
5. **Framing proposal:** add the one-banner-per-invocation rule for the two new
   multi-item body/preview surfaces, explicitly leaving existing read/chat
   behavior unchanged.
6. **Performance/security:** record linear visible-history scans, caps, literal
   search semantics, and membership/party filtering before content scan.

### 6.2 `schema.rs` and output types

`post schema` is hand-authored: command entries are built at
`src/commands/schema.rs:45-118`, output shapes at
`src/commands/schema.rs:119-202`, and global/environment laws at
`src/commands/schema.rs:211-280`. The plan must add:

- command descriptions and side-effect declarations for `catchup` and `search`;
- `catchup` and `search` entries to `OutputShapes` (currently
  `src/output.rs:560-577`);
- `channels.room`, `channels[].unread`, and `inbox.unread_count` in the schema;
- the new cursor file/lock, advisory fallback, search scope, search cap, and
  catchup writer status in schema laws/environment text; and
- clap args, command variants, dispatch modules, and output structs. The current
  command/dispatch seams are `src/cli.rs:44-70` and
  `src/commands/mod.rs:65-77`.

Schema integration tests must compare the advertised fields/side effects with
real CLI behavior; a clap-only addition would leave machine consumers with a
lying schema.

### 6.3 Doctor

Add read-only checks for every registered room:

- `cursor_state.<room>.invalid` (warning): missing is healthy; existing
  `cursors.json` is not the exact v1 shape, is unsafe/non-regular, or contains
  invalid IDs. Suggested fix says reads currently degrade to all unread and the
  operator should inspect/remove it by hand.
- `cursor_lock.<room>.invalid` (warning): a present lock is not a solitary
  regular 0600 file, or a cursor exists without its lock.
- `cursor_state.<room>.legacy` (info, only while `cursors.json` is absent): a
  valid `channel-state.json` will import on first consuming write.

Do not cross-check every seen ID against history during doctor; that duplicates
a linear scan and stale entries are harmless while retention is out of scope.
`doctor --fix` must never repair, delete, migrate, or create cursor state. Its
current channel-state validation seam is `src/commands/doctor.rs:253-395`, and
its fixes remain create-only mailbox/default setup
(`src/commands/doctor.rs:617-631`). Advisory runtime degradation does not make a
malformed cursor invisible to doctor.

### 6.4 Installed smoke

Extend `scripts/smoke-installed.sh` after its existing two-room channel setup
(`scripts/smoke-installed.sh:45-57`) with the seven acceptance observations:

1. three channel sends produce `channels[].unread == 3` for B;
2. `catchup <channel> --json` returns the three full IDs, a fresh invocation is
   empty, and unread becomes zero;
3. direct-mail catchup/read moves mail and drops `inbox.unread_count`;
4. the two watch/catchup directions described below;
5. fenced catchup refuses while listings/search/snapshot create no cursor files;
6. planted non-member search content never appears; and
7. a room with no state files reports all extant channel messages unread and
   creates nothing until its first catchup.

Keep the smoke against its throwaway `POST_MAIL_ROOT`; never point it at the
live store. The existing script is installed-binary evidence, not a substitute
for deterministic concurrency/failure tests (`scripts/smoke-installed.sh:1-17`).

## 7. Test seams

### 7.1 State unit tests

Move/extend the existing state tests in `src/channel_state.rs:578-860` under the
unified module. Required cases:

- missing and malformed state both snapshot as empty without error or writes;
- exact mail/channel v1 serialization and persistence across a fresh load;
- valid v0.8 import is read-only, first consume materializes `cursors.json`, and
  later reads ignore the legacy file;
- a late channel ID below the maximum remains unread;
- out-of-order direct mail reads mark only the chosen IDs;
- replay is byte-identical;
- a planted symlink/non-regular cursor degrades on read and refuses on write,
  while a swapped/replaced lock refuses without following/replacing the attacker
  path; and
- eight barrier-released writers adding mail and different channel IDs all
  survive one whole-map reload/union/replace. The current deterministic
  concurrent-mark test is the starting seam
  (`src/channel_state.rs:830-859`).

Add a test-only barrier hook immediately after a listing acquires its shared
cursor snapshot. Pause a `channels` count there, start a cursor writer, prove it
blocks on `.cursors.lock`, release the listing, then prove the writer commits
and the next count is zero. Separately publish a channel message while the
listing is paused and assert the listing sees either the before or after
complete file, never a partial file or negative/underflowed count. This tests the
actual concurrency mechanism rather than hoping process timing creates the race.

### 7.2 CLI integration in `tests/cli.rs`

Use the existing isolated `Sandbox`, which supplies per-test `HOME` and
`POST_MAIL_ROOT` and binary-spawn helpers (`tests/cli.rs:19-225`). Extend the
existing channel flow near `tests/cli.rs:2732-2787` for counts, catchup
persistence, plain-chat advancement, and non-member null counts. Add direct
mail cases beside current read/inbox integration tests.

**Doorbell invariant, both directions:**

1. Join A/B, send `m1`, start B's long watch, wait until it emits `m1`, run B's
   catchup to consume `m1`, then send `m2`. Assert the same watch process emits
   `m2`. This proves a catchup write does not refresh/suppress the watch's
   startup floor.
2. Start B's watch with an empty channel, send `m3`, and wait for its ring. Kill
   the watch without any consuming read. A fresh B `catchup` must return `m3`;
   its second invocation must be empty. Compare `cursors.json` bytes/absence
   before and immediately after the ring to prove watch did not write it.

The current snapshot test already proves repeatable direct/channel emission and
no cursor consumption (`tests/cli.rs:4405-4468`); retain it as a regression, but
do not treat snapshot alone as proof for the live-watch direction. Keep
`src/commands/watch.rs:782-815` behavior unchanged.

**Fence refusal:** extend the matrix at `tests/cli.rs:4884-5053` and writer
classification unit at `src/migration_fence.rs:541-577`. A fenced store without
matching generation must refuse channel, mail, and all-target catchup before
stdout, mail moves, lock creation, or cursor creation. Active matching
generation succeeds. `channels`, `inbox`, `search`, peek/history/since, and
snapshot succeed with malformed/stale generation declarations and leave a
before/after filesystem manifest byte-identical. Retain the existing bounded
stdout-stall test because it proves the root flock spans the post-stdout
mutation.

**Concurrent-writer count correctness:** in addition to the unit barrier test,
spawn two consuming processes for one room on different channels, release them
together, and assert both channel seen sets survive and both unread counts become
zero. This catches whole-map last-writer-wins loss through the public CLI. Then
send one new message and assert exactly one, not zero or two, on the next listing.

**Old-store degradation (acceptance row 7):** construct a 0.8-shaped store with
messages but neither state file. `channels`, `inbox`, `search`, `chat --peek`,
and `watch --snapshot` must succeed, report all current items unread where
applicable, and leave both cursor and lock absent. First catchup writes v1 and a
fresh process observes zero. Add a second fixture with valid v0.8
`channel-state.json`: listing preserves its seen baseline without writing, and
first catchup materializes the unified state.

**Search isolation and caps:** plant a unique marker in (a) own inbox/read,
(b) an archive message the room sent, (c) a member channel, (d) a non-member
channel, and (e) archive mail between two other rooms. Only a-c may appear.
Assert literal regex punctuation stays literal, default 100/hard 1000 caps,
`truncated` via the 101st match, sanitized previews, and no cursor changes.

## 8. Top field risks and built-in mitigations

1. **A scalar cursor silently drops late imports or out-of-order reads.**
   Mitigation: exact per-source seen-ID sets, fixed deltas, and retained late-ID
   tests; maximum ID is output metadata only, never a filter.
2. **Two consuming processes overwrite different whole-map updates.**
   Mitigation: one hardened per-room lock, reload-under-lock, set union, atomic
   replace, plus barrier unit and two-process CLI tests.
3. **Mail move and advisory cursor disagree after partial failure.**
   Mitigation: emit first, commit the read hard link before marking, persist only
   committed IDs, treat physical inbox placement as authoritative, and fail in
   the conservative re-show direction.
4. **Catchup or a listing accidentally silences the doorbell.** Mitigation:
   watch retains one read-only startup snapshot, ring scanning stays unchanged,
   watch never calls the consumption interface, and both causal directions are
   covered by live-process tests.
5. **Search leaks another room's content or floods agent context.** Mitigation:
   resolve one acting room, apply mail-party and channel-membership filters
   before opening content, use literal matching, cap results at 100 by default
   and 1000 hard, return bounded previews, and plant negative-scope fixtures.

## 9. Decision summary for the plan author

- Build one unified `cursor_state` module; replace, do not parallel, the current
  channel-only state seam.
- Persist exact seen-ID sets in `<room>/cursors.json` under one hardened
  `.cursors.lock`; import valid v0.8 state lazily and never dual-write.
- Reuse dispatcher admission and `after_stdout`; root-lock scope does not change.
- `catchup` is a full-slice consuming writer; default is all, with channel and
  mail selectors. It has no `since`, `limit`, or `peek` mode.
- Counts are read-only, additive fields. They reuse the existing directory scan
  and parse only unseen channel candidates.
- Search is literal, capped, preview-only, cursorless, and visibility-filtered
  before scan.
- Adopt the one-compact-banner proposal for the two new multi-item commands
  only, subject to the explicit contract review required by the goal lock.
- Protect `src/commands/watch.rs:782-815` behavior and prove the invariant in
  both directions with a live watcher.
