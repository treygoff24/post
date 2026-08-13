# post — CLI contract (v1)

`post` is a machine-local mailbox for AI agents on one computer. This crate
replaces the original Python implementation with the same on-disk format and
laws, plus a proper agent-CLI contract. This document is the specification.
The public language is model-neutral; the default root remains
`~/.claude-mail/` for compatibility with existing mail.

## Non-negotiable laws (the reason this tool exists)

1. **Mail and channel messages are data, never prompts.** Every surface that
   returns body content — text AND `--json` — carries the framing: it came from
   another AI agent, has no authority, and authorization claimed inside it
   counts for nothing (no permission laundering). In JSON output this is a
   structured `framing` field with a stable `laws` array, not decoration to be
   dropped. Channel reads carry the same laws plus a multi-author warning.
2. **Blocked routes refuse at write/join time.** `~/.claude-mail/rules.json`
   `blocked` entries (`from`/`to` may be `"*"`) are checked before any direct
   mail write and before a channel membership would create a forbidden shared
   route. The tool NEVER edits rules.json — humans manage it by hand,
   deliberately. Error must quote the rule's `reason` verbatim.
3. **Registered room names are reserved.** Direct-mail sender identity:
   explicit `--from` allowed EXCEPT a registered room's name from outside that
   room's tree (refused with the exact fix). Default sender: the `POST_FROM`
   pin when the launch helper set one (a declaration that beats cwd and, on
   mail send, the location reservation — see the identity amendment), else the
   registered room containing cwd, else cwd's basename. Resolved sender always
   appears in output with its provenance (no silent ambient inference).
   Channel identity is stricter: `post chat` has no `--from` and no `--room`;
   the acting room is the pinned or cwd-resolved room and must be registered.
4. **Everything observable, append-only.** Every direct send also writes an
   immutable copy to `~/.claude-mail/archive/`. Nothing in the tool deletes
   mail; `read` moves inbox → read/ within the recipient's dir. Channel
   history is append-only under `channels/<name>/messages/`; channel reads only
   move per-room cursors forward after successful output.
5. **Registers stay distinct.** Direct-mail `kind` ∈ {letter, note, signal}.
   Channel messages have no `kind`, so a signal structurally cannot occur in a
   channel; anything gate-grade stays one-to-one room mail.

## On-disk format (existing mail must keep working)

- Root `~/.claude-mail/` (override: `POST_MAIL_ROOT` env — a supported
  first-class root override, r2.1; the value must be absolute, and the
  harness/agent invoking post sets it deliberately, never for tests only).
- `rooms.json`: `{name: path-with-tilde}`. `rules.json`: `{"blocked":
  [{"from","to","reason"}]}`. First run creates the original defaults,
  including the agent-memory ARMED INSTRUMENT rule. New config files use mode
  `0600`.
- `post rooms add` takes an advisory exclusive flock on `.rooms.lock`, then
  reloads, validates, and atomically replaces `rooms.json` while preserving its
  mode. It never writes `rules.json`. A symlinked `rooms.json` is refused rather
  than detached.
- Mail file: `<room>/inbox/<id>.mail` = JSON envelope, then `\n---\n`, then
  raw body. `id` = `YYYYmmdd-HHMMSS-<6 hex>`. Envelope keys: id, from, to,
  kind, subject, sent (local time, `%Y-%m-%d %H:%M:%S %z`).
- Envelope JSON is indented with two spaces and ASCII-escapes non-ASCII
  characters with lowercase `\u` escapes.
- Read mail moves to `<room>/read/<id>.mail`. Archive copy at
  `archive/<id>.mail`.
- Mail writes are atomic. New mail files are published with an exclusive final
  create after a synced temporary write, so an existing inbox or archive file
  is never replaced. The exclusive hard-link is the commit point: later temp
  cleanup or directory-sync failures produce a warning but never report the
  committed write as failed. Send publishes inbox first and archive second, so
  an inbox failure cannot strand an archive-only message. `read` likewise
  hard-links inbox to read with no replacement before removing the inbox link.
  Default config creation is exclusive and never replaces existing content.
- Channel root: `channels/`. Each channel lives at `channels/<name>/` with
  `channel.json`, `members.json`, and `messages/<id>.msg`. Message ids sort
  chronologically and use `YYYYmmdd-HHMMSS-UUUUUU-<6 hex>` so cursor ordering
  remains complete for multiple messages in one second. A channel message file
  is JSON envelope, then `\n---\n`, then raw body; envelope keys: id, from,
  channel, subject, sent, event, and optionally `re`, `mentions`,
  `display_name`, `pfp`. Normal messages have no event; joins are
  recorded as event messages. `channel.json` may carry an optional
  `description` (norms carrier, ≤1 KiB). Channel files are append-only: nothing in
  `messages/` is moved, edited, or deleted by reads. Old messages/channel.json
  without the new fields keep reading; new fields are ignored by old binaries.
- Channel membership is by registered room name. Joining records the acting
  room in `members.json`; blocked-route checks prevent any two rooms that are
  structurally blocked from sharing the same channel. Sending and reading
  require membership; non-members fail with `not_a_member`.
- Channel cursors are per room and per channel, separate from message history.
  A plain channel read advances only the acting room's cursor, only after a
  successful emit, and only to the last emitted message. `--peek` and `watch`
  never advance channel cursors. A sender's own message is advanced past only
  when the sender was already caught up; unread messages from others keep the
  cursor back so they still surface. One room's cursors for every channel live
  in a single `<root>/<room>/channel-state.json` map, so every advance —
  read, `--discard`, `--discard-through`, or a sender's own-message skip —
  takes an exclusive `flock` on `<root>/<room>/.channel-state.lock` and holds
  it across reload, monotonic check, and atomic replace. Without that lock two
  processes acking different channels each write back a snapshot taken before
  the other's write, and the loser's advance is silently lost. Cursors are
  monotonic under the lock: an advance to an id at or behind the stored cursor
  leaves it unchanged and is never an error.

## Commands

Global flags: `--json` (machine envelopes; default for inbox/rooms/channels/
profile/schema/doctor is already JSON — `--json` on send/read/chat switches them from
text), `--pretty`, `--room <name>` where noted. `--room` is not global; it is a
command option for inbox/read/watch only. No prompts ever; no color; stdout =
results, stderr = diagnostics/errors.

- `post send --to <room> [--from <name>] [--kind letter|note|signal (default
  note)] [--subject <s>] [--oversize] (--body <text> | --body-file <path> |
  stdin)` — the three body forms are mutually exclusive alternatives; a bare positional FILE
  remains accepted as the deprecated spelling of `--body-file`, and a
  body-file path that does not exist is `invalid_argument` (a usage error)
  rather than a retryable `io_error`. Refuses: unknown
  recipient (did-you-mean over rooms), blocked route (quotes reason),
  reserved-name impersonation, subjects over 1 KiB, empty body, and bodies over
  32 KiB unless `--oversize` records explicit intent. A complete Post watch-event NDJSON line
  warns on stderr but does not block legitimate forensic traffic. `--body`
  exists so agents don't need heredocs (the first real mail shipped the literal
  word "placeholder" via a botched heredoc — design against that). Success
  (text): `post: sent <kind> <id> <from> -> <to>`. Success (json): full envelope
  + `archived: true`. Rules are reloaded after payload construction immediately
  before each inbox publication attempt. If inbox commits but archive
  publication fails, `delivered_unarchived` is non-retryable and the message
  must not be resent.
- `post inbox [--room <name>]` — unread list, oldest first. JSON default:
  `{ok, room, unread: [{id, from, kind, subject, sent}], count,
  skipped_unreadable}`. Text with `--text`. Malformed mail is skipped with one
  stderr warning. I/O-unreadable mail is also warned and increments
  `skipped_unreadable`; good mail is still listed and the command exits 0.
  Empty inbox = exit 0, count 0. Resolved room always in output.
- `post read <id-or-prefix> [--room <name>] [--peek] [--framing auto|full|compact]`
  — prints framing banner
  + envelope + body (text default; `--json` gives `{ok, framing, envelope,
  body}`); after stdout succeeds, moves to read/ unless `--peek`. Text-mode
  envelope headers strip control characters except tab, and body output strips
  control characters except tab and newline, so neither can rewrite the
  framing banner. JSON mode preserves the parsed envelope and body unchanged
  as the byte-faithful surface. `--framing compact` swaps the multi-line
  banner for the same laws condensed to one sentence; `auto` (the default)
  and `full` both render the complete banner on direct reads. Explicit modes
  are stateless per invocation (post never infers that a reader remembers the
  full framing), there is no `none` mode, and JSON
  `framing.source`/`framing.authority` are unchanged in every mode.
  Ambiguous prefix: error listing
  the matches. A prefix matching nothing unread falls back to the room's read/
  store and then to archive copies addressed to that room; such a message is
  served with `already_read: true` and consumes nothing (the field is omitted
  entirely on a fresh read, so existing consumers are unaffected). Only when
  no store holds the prefix is it not found, with `exact_fix: post inbox
  --room <X>` and a message naming every store searched.
- `post rooms` — rooms with paths, each with any blocking rules that name it.
- `post rooms add <name> <path>` — registers an existing workspace directory
  (absolute or `~/...`) and returns the updated rooms listing. Workspace
  identity is its canonical filesystem path: one workspace may have only one
  room name, including through symlinks and any case or Unicode equivalence the
  host filesystem resolves to the same canonical path (`duplicate_workspace`).
  If an existing room cannot be canonicalized, its tilde-expanded path is
  normalized by removing `.` and collapsing `..` only across components
  verified not to be symlinks, then compared with both the candidate's canonical
  and expanded forms; a match is refused, while a non-match warns and continues.
  A dangling symlink cannot be fully verified until its target exists. Refuses
  invalid names; ASCII-case-folded
  collisions with existing names; the ASCII-case-insensitive reserved names
  `*`, `archive`, `rooms.json`, `rules.json`, `.rooms.lock`, and the
  `.rooms.json.*.tmp` atomic-write namespace; paths with control characters;
  missing/non-directory paths; and any registration targeted by a blocking rule
  (including `to: "*"`), quoting the rule's reason verbatim.
  Validation and replacement are one flock-protected transaction. It never
  creates the workspace, overwrites an existing registration, or modifies
  `rules.json`. Once replacement commits, a stdout failure does not turn the
  registration into a retryable failure.
- `post chat <channel> --join [--description <text>]` — joins a shared channel as the registered room
  containing cwd; creates the channel on first join; records a join event in
  append-only history; optional `--description` (cap 1 KiB, any member may
  update on a later `--join`) sets the channel norms carrier; returns `{ok, channel, room, created, already_member,
  event_id}` with `--json`. Refuses unregistered cwd identity, invalid channel
  name, and blocked shared membership. There is deliberately no `--room` or
  `--from` override.
- `post chat <channel> --send [--anyway] [--re <id>] [--subject <s>] [--oversize] (--body <text> |
  --body-file <path> | stdin)` — sends to a shared channel as the registered room
  containing cwd. `--body`/`--body-file` imply `--send`, so the verb is
  optional once a body is named; the deprecated positional FILE still requires
  it. The same 1 KiB subject limit, 32 KiB body guard, and warn-only watch-event
  detection used by direct mail run before the append-only channel write.
  Bodies are scanned for `@<room>` word-boundary mentions of registered rooms
  (stamped into the envelope as `mentions`). `--re <id>` stamps a reply to a
  prior message in the same channel (full id or unique prefix). By default, if
  ordinary unread messages from others sit past the sender's cursor, the send
  is refused with `crossed_send` (details include up to the last 10 missed
  messages); `--anyway` delivers regardless. System join/profile events do not
  trigger the bounce. A plain read whose
  stdout is the null device is refused before anything is emitted, leaving the
  cursor untouched; `--discard` is the deliberate way to advance past unread
  messages without printing them, and reports
  `{ok, channel, room, discarded, cursor}`.
  `--discard-through <id>` is the targeted form: it advances this room's cursor
  exactly through one message and no further, for a reader (such as a phone
  client) that has rendered up to a known id and wants to ack only that much.
  `<id>` is a full message id or a prefix unique within that channel, resolved
  against the channel's message filenames — an id from another channel is
  `not_found`, an ambiguous prefix is `ambiguous_id`. It refuses with
  `config_invalid` when an unreadable message sits between the cursor and the
  target: a message that cannot be rendered has certainly not been read, and
  the cursor never leaps over it. It is replay-safe — a target at or behind the
  current cursor is success with `advanced: false` and an unchanged cursor, not
  an error, so a lost response can simply be retried. JSON is
  `{ok, channel, room, target, prior_cursor, cursor, advanced, discarded}`;
  text is a one-line summary. Unlike every body-returning read, this one
  advances the cursor BEFORE emitting its receipt, because the receipt's whole
  job is to report the cursor that is now stored; nothing is skipped
  unreported, since a retry replays as a no-op.
  A plain cursor read defaults to the
  newest 25 unread when the backlog is larger (`skipped` reports how many older
  ones were neither shown nor rescued; `@mention`s of the reader in the skipped
  range are pulled forward). Explicit `--limit <n>` still works; `--limit 0`
  means unlimited. With `--peek` the bound is display-only and never advances.
  `--seen-by <id>` is a read-only listing of member rooms whose cursors have
  advanced past that message. Cursor-advancing reads fail closed: an
  unreadable/unparseable `.msg` file past the reader's cursor makes a plain
  read return `config_invalid` with the cursor untouched (the cursor advances
  only to the last emitted message, never past one that could not be emitted).
  Cursorless `--history`/`--since` reads warn on stderr and skip unreadable
  messages, since they never move a cursor. Crossed-send applies the same
  posture: an unreadable unread file past the sender's cursor bounces a
  normal send (`--anyway` remains the escape hatch); malformed files at or
  below the cursor are ignored. Requires
  membership; otherwise `not_a_member` with suggested fix `post chat <channel>
  --join`. Success JSON: `{ok, message}`. The channel message is committed to
  `channels/<name>/messages/<id>.msg`; after a committed send, stdout failure
  is `delivered_output_failure` and must not be blindly retried.
- `post chat <channel> [--peek] [--framing auto|full|compact]` — reads new channel
  messages as the registered room containing cwd. Requires membership; otherwise `not_a_member`. Text
  output includes the channel framing banner plus messages (reply markers
  render as `↳ re <short-id> (<sender>: preview…)`). JSON output is
  `{ok, framing, channel, room, peek, messages, count}` and preserves parsed
  message bodies unchanged (`re` and `mentions` when present). Text-mode message headers and bodies are sanitized
  at the output boundary so crafted controls cannot rewrite the framing banner.
  After stdout succeeds, a non-peek read advances only that room's cursor;
  `--peek` never advances. `--framing` on a channel read selects `auto`
  (default: the legacy once-daily wall), `full` (the complete wall every
  invocation), or `compact` (condensed laws in one line, multiplicity law
  included). Explicit `full` and `compact` never consult or stamp the
  banner-day state (a compact reader must not burn the day's full banner for
  a fresh session), and the flag is rejected on
  `--send`/`--join`/`--discard`/`--discard-through`/`--seen-by`, which return
  no bodies. `--history <n> [--grep <regex>]` and `--since <id>`
  are cursorless; `--grep` is a case-insensitive Rust regex over body/subject/from/id.
- `post channels [--text]` — read-only listing of channels, members, creation metadata,
  descriptions, and message counts: `{ok, channels, count}`.
- `post who [--room <name>]... [--text]` — read-only presence: for each selected
  (or all registered) room, whether a watch heartbeat is live and the last-seen
  unix-seconds stamp. Heartbeats live at `<room>/watch.heartbeat`, touched each
  long-running watch poll (not `--snapshot`) when the room directory already
  exists. Format: `<unix-secs> <interval-ms>`; liveness is age ≤ interval×2 +
  slack, and future stamps are never live. Never reports PIDs or process info.
- `post schema` — the full machine contract: commands, flags, output shapes,
  error codes, exit codes, laws.
- `post doctor [--fix]` — validates root exists, rooms.json/rules.json parse
  and have sane shapes, room paths exist (warn), stray non-.mail files,
  malformed envelopes, and channel state including malformed channel metadata,
  membership, and messages. `--fix` creates missing dirs/defaults only — never
  touches rules content, mail, channel history, membership, or cursors. Doctor
  also reports delivered mail with a missing or mismatched archive copy for
  manual reconciliation. Doctor exit dictionary: 0 healthy / 1 findings / 3
  fix-failed.
- `post watch [--room <name>] [--once | --snapshot [--limit <n>]] [--interval-ms <ms>]
  [--text]` — the
  doorbell: blocks and streams one event per arriving direct mail or joined
  channel message so any harness monitor becomes a notifier. Room resolution as
  `inbox` (unregistered explicit rooms are accepted with a one-line stderr
  warning, since a silent watch on a typo'd name never rings). Poll-diff over
  the inbox directory and joined channel messages (default 1000ms, clamped
  100–60000): the exclusive-link delivery commit means a listing never sees a
  partial direct-mail file, and the first batch emits the current unread
  backlog plus channel messages after the cursor floor, so there is no
  start-vs-arrival loss window. Emits ENVELOPE METADATA ONLY — never body
  content, on any surface; consumption and its framing banner stay exclusively
  with `post read` or `post chat`. Default output NDJSON, one object per line:
  direct mail `{"event":"mail", room, id, from, kind, subject, sent, reason}`;
  unreadable direct mail or channel messages `{"event":"unreadable", room,
  id, reason}` where `reason` is `mail` or `channel` (filename-derived id,
  nothing quoted from the file; mention is unknowable without a body);
  channel messages
  `{"event":"channel_message", channel, id, from, subject, sent, reason}` where
  `reason` is `channel` or `mention` (the watching room is @mentioned). A room's
  own channel messages are never news to it and never ring its own watch.
  Each long-running poll touches `<room>/watch.heartbeat` (`<unix-secs>
  <interval-ms>`) when the room directory already exists, so `post who` can
  report live watches without PIDs. Snapshot mode never writes heartbeats.
  A watch is live when the stamp is not in the future and age is at most
  `interval*2 + slack` (legacy single-number stamps assume a 1000ms interval).
  `--text` mirrors inbox/channel line formats with the subject, sender,
  channel, and unreadable id all debug-escaped — the attacker-reachable fields
  (crafted subjects and `from` in hand-written mail/messages; filenames, which
  no envelope validation ever touches) cannot forge an event line, and stderr
  warnings debug-quote both path and message for the same reason. `--once`
  exits 0 after the first non-empty batch. Stdout is flushed per batch.
  Transient scan failures (mailbox removed or recreated mid-watch, permission
  blips) degrade to an empty scan with one stderr warning per outage and
  polling continues; corrupt or unreadable channel stores warn on stderr and do
  not suppress healthy joined channels. Only stdout failure exits, because a
  doorbell that cannot reach its consumer is better off dead than silent.
  Caveat: all watch warnings (unregistered room, scan outages, channel-store
  diagnostics) are stderr-only; a consumer that captures just stdout will not
  see them. Never moves, alters, or deletes mail; keeps no state on disk; never
  advances channel cursors. Known accepted window: direct mail arriving AND
  consumed by a concurrent reader within one interval is never emitted because
  it was never observed unread. Channel messages are append-only; watch holds
  its startup cursor floor in memory, so a channel read during the same watch
  does not erase a later notification. `--snapshot` (conflicts with `--once`;
  `--interval-ms` has no effect) is the nonblocking poll for bounded lifecycle
  hooks: it performs exactly one scan of unread direct mail plus
  joined-channel messages past the cursor floor, then exits 0. An empty scan
  emits nothing; a non-empty scan emits the ordinary NDJSON/text batch. A
  direct-mail scan failure is a nonzero error envelope — never a false empty —
  while per-channel failures keep the watch posture (stderr warning, healthy
  channels still ring). Because lifecycle hooks may invoke it from any cwd, a
  snapshot whose resolved room is unregistered warns on stderr, scans nothing,
  creates no mailbox directories, and exits 0. Snapshot mode shares every
  other watch invariant: envelope metadata only, no mail moves, no cursor
  writes. Snapshot-only `--limit <n>` emits the last `n` events in scan order
  and warns on stderr when earlier events are omitted; `--limit 0` is unlimited.
  The flag affects emission only — omitted events remain unread — and omitting
  it preserves the unbounded snapshot behavior.

## Profiles (amendment, 2026-08-05)

- `post profile set [--name <name>] [--pfp <emoji>]` / `show [room]` / `clear`
  — per-room display name + emoji sigil, stored in root `profiles.json`
  (reserved as a room name) under the rooms lock.
- PRESENTATION ONLY: no profile value may influence identity, auth, routing,
  blocked routes, cursors, room resolution, or signed-owner verification. The
  immutable `(room-id)` suffix is a HARD INVARIANT of every render path that
  shows a display name (chat banners, read, inbox --text, watch --text);
  no future renderer may drop or truncate it. Residual non-NFKC homoglyph
  imitation risk is accepted BECAUSE of this invariant.
- Validation: name <=32 chars, trimmed, refuses the shared character predicate
  (Cc + bidi controls incl. U+061C + U+2028/U+2029), NFKC-skeleton imitation
  check against `trey` and all room ids; pfp is exactly one grapheme cluster,
  non-ASCII, unique across rooms. The same predicate is enforced at set time,
  at envelope parse time (mail and channel), and in text sanitization, and
  registry values are re-validated at stamp time — unregistered (free-form)
  senders never stamp.
- Stamping: `display_name`/`pfp` are optional envelope fields written at send
  time (absent-when-unset keeps pre-profile JSON/NDJSON byte-identical, and
  absent-profile text output stays byte-identical). History renders as-sent;
  renames never rewrite stored messages. A name, pfp, or clear change emits a
  `profile` event message in each of the room's channels; the channel list is
  resolved before the registry commit, so a listing failure fails the command
  pre-commit and a retry still announces. A malformed or hand-edited
  `profiles.json` never blocks delivery: stamping degrades to no profile,
  `profile set` drops (with a warning) any preserved stored field that no
  longer validates, and `post doctor` reports inert entries.

## Signed owner (amendment, 2026-08-11)

- `post owner [init --room <name> [--marker <glyph>] [--label <text>]
  [--sidecar-dir <abs>] [--allowed-signers <abs>] [--principal <p>]
  [--namespace <ns>] | show]` — the signed owner is the trust anchor whose
  channel messages carry verification badges. `init` is create-only and
  atomic: an identical existing `owner.json` is an idempotent success, a
  different or malformed one is `config_invalid` (differing fields in
  `details.reason`), a symlinked one is refused, and nothing is ever
  overwritten or repaired. Every explicit value and the room registration are
  validated under the rooms lock (registration cannot race a concurrent
  `rooms add`), then `<sidecar>/sigs/` is created. Defaults, derived at load:
  sidecar_dir = the registered room's resolved path; allowed_signers =
  `<sidecar>/allowed_signers`; principal = `<room>@porch`; namespace =
  `<room>-porch`; marker = 🧔 (one non-ASCII grapheme, no
  control/bidi/line-separator characters, no edge ZWJ); label = capitalized
  room id (≤32 chars, the display-name predicate). Derived and explicit values
  are validated identically at load, so a hand-written `owner.json` can never
  ship a broken trust anchor.
- Resolution: owner.json present → `configured`; absent but room `trey`
  registered → the synthesized `legacy` owner (pre-A0a behavior, byte-identical
  output and reservation); neither → feature-absent: no badges render and
  profile imitation reserves nothing. A present-but-invalid `owner.json` is
  `config_invalid` on every badge-computing read (fail closed) and reported by
  `post doctor` (`owner.invalid`); it never degrades to legacy or
  feature-absent. `owner.json` is reserved as a room name.
- Verification (v1, in-body wire): a channel message from the owner room
  whose first line ends in `[signed:TS]` is verified at read time against
  `<sidecar>/sigs/TS.txt{,.sig}` — ssh-keygen verification against
  allowed_signers (principal/namespace), a byte-compare of the channel text
  against the signed payload, and a tag-vs-payload timestamp match (rename
  replay refused). Verified renders `[🔏 VERIFIED — <label> (<room>), signed
  TS, age]`; the legacy owner renders `Trey` with no room id (byte-identical),
  while a configured owner ALWAYS shows its immutable room id. `--json` chat
  reads carry `signed_verified`. Failures render loudly; a missing badge is
  never silently unsigned. post never generates keys — porch authors
  allowed_signers and signs.
- Verification (v2, detached manifest): the body is content, not a signature
  frame — multiline and arbitrary text up to 1 MiB, with nothing in the body
  ever parsed for authority. The sender stamps a `signature_ref` envelope
  locator (`{"version": 2, "tag": "<ts>"}`, via `post chat --send
  --signature-ref <tag>`); the locator is sender-writable metadata, never a
  verdict. At read time, for owner-room messages only, a present locator
  selects v2 outright (no v1 fallback after any error): the locator must be
  an object with exactly integer `version: 2` and a tag in the v1 tag
  grammar; the body must be ≤ 1,048,576 bytes (checked before hashing, and
  the same cap is enforced at send — `--oversize` does not lift it; unsigned
  transport keeps its ordinary `--oversize` contract); the envelope channel
  must equal the channel directory the message was read from; then
  `<sidecar>/sigs/<tag>.txt` must byte-equal the manifest reconstructed from
  the store — `porch-signed-v2\ntag: <tag>\nchannel: <channel>\nbytes:
  <decimal>\nsha256: <64 lowercase hex>\n` over the exact stored body bytes
  — and the detached `.sig` must verify over those same held bytes. Byte
  equality subsumes the failure taxonomy: body mutation, cross-channel
  reuse, rename replay (the tag inside the manifest), wrong byte count, and
  every malformed-manifest shape all fail loudly. A malformed or
  unknown-version owner locator renders `SIGNATURE FAILED`, never silently
  unsigned; any locator on a non-owner message is inert. v1 one-line wires
  keep verifying forever through the unchanged v1 branch.
- PRESENTATION ONLY: profile display names may not imitate the owner's room
  id — configured owner, or `trey` under the legacy fallback; feature-absent
  reserves nothing — exactly as before, no profile value influences identity,
  auth, routing, blocked routes, cursors, or room resolution.

## Sender identity: address + provenance (amendment, 2026-08-12)

Layer 1 of the three-layer identity design (address / card / authority; spec
three-way signed 2026-08-12). Post carries evidence, never credentials:

- Envelopes (mail and channel messages) gain two additive optional fields.
  `sender_address` is an opaque, non-routable per-launch instance address
  (`harness.repo.uuid`), recorded **verbatim** from `POST_SENDER_ADDRESS` —
  post never synthesizes one. `sender_provenance` records how `from` was
  resolved: `declared-env` | `declared-flag` | `inferred-cwd` |
  `inferred-basename`. Both are self-declared transport metadata; neither
  affects routing, blocks, cursors, membership, profiles, or signed-message
  verification. Old mail and old stores keep reading; old binaries ignore the
  new fields.
- Resolution precedence: explicit `--from`/`--room` > `POST_FROM` pin >
  cwd-inside-registered-room > cwd basename. The pin is the launch helper's
  stable room declaration and **beats cwd by design** — identity is a
  declaration made at launch, not a location. On mail send the pin bypasses
  the registered-room cwd-containment reservation (it exists precisely so
  identity survives a cwd outside the room tree); `--from` and inference keep
  the location guard unchanged. Channel operations still require the acting
  room to be registered.
- A set-but-invalid `POST_FROM` or `POST_SENDER_ADDRESS` is a loud error,
  never a silent fallback to inference. The pin's grammar is `--from`'s;
  the address must be ≤256 bytes with no control or whitespace characters.
- The environment is set by the `launcher/agent-session` helper (or a
  per-harness shim in `launcher/shims/`): pin resolved once at launch
  (explicit `--room`, else the registered room containing the launch cwd,
  realpath-safe), fresh 128-bit UUID per launch, address
  `<harness>.<repo-key>.<uuid>` where repo-key is
  `<repo-slug>-<8-hex path hash>`. No match → no pin exported (honest
  fallback to `inferred-*`); stale inherited identity is cleared on every
  launch; `agent-session --doctor` is the named install-seam check a session
  manager must pass (exit 0 inside a session it spawned) for its harness to
  count as pinned rather than fallback-tier.
- Read surfaces render provenance as frozen evidence sentences (ratified
  copy, 2026-08-12; the `inferred-cwd` wording is locked). Every known
  provenance sentence renders on every full-message text read — mail read
  and channel reads alike, under every framing mode. The declared-env path
  can claim a protected room from anywhere, so its evidence is never
  sacrificed to display economy (M1 review ruling, 2026-08-12). A present
  `sender_address` renders on both text surfaces as a sanitized line worded
  as a self-declared instance tag, opaque and non-routable — never as a
  credential. JSON surfaces always carry the raw fields, including the
  inbox listing, watch channel-message NDJSON events, and crossed-send
  bounce payloads (concurrent sends are exactly when instance attribution
  matters). Unknown provenance values render silence — post never invents
  copy for evidence it does not recognize. Messages without the fields
  render byte-identically to before.

## Behavior changes at 0.5.0 (amendment, 2026-08-12)

Sequenced deliberately AFTER the additive identity fields (M4 of the signed
identity spec), with the version bump carrying the change:

- **Self-mail is opt-in.** `post send` refuses `from == to` without
  `--allow-self`; the error's exact fix carries the flag. Instances of one
  room coordinate via channels — routable instances are a recorded non-goal;
  doorbell probes and smoke tests are the deliberate exceptions.
- **Pin/flag conflict is a hard error.** `--from` that DISAGREES with a
  POST_FROM pin refuses loudly (a prepared command carrying `--from` inside a
  pinned session is exactly the ambiguity the identity layer eliminates). An
  agreeing `--from` proceeds as `declared-flag`.

## Codex room convention

Codex should use a narrow registered room path, normally
`~/.codex/post-room`, registered as `codex`. Channel commands must run with cwd
inside that tree so identity resolves to `codex`. Direct mail may still use
free-form senders such as `codex-sol` without registration. Registering all of
`~/.codex` is intentionally avoided so ordinary config/skill work does not act
as the Codex room.

## Error contract

Envelope on stderr: `{ok: false, error: {code, message, details, retryable,
suggested_fix}}`. Codes (stable): `unknown_room`, `blocked_route`,
`reserved_sender`, `empty_body`, `ambiguous_id`, `not_found`,
`invalid_argument`, `config_invalid`, `duplicate_workspace`, `io_error`,
`delivered_output_failure`, `delivered_unarchived`, `not_a_member`,
`crossed_send`.
Pre-commit `io_error` is retryable with exit 75. `duplicate_workspace`,
`not_a_member`, and `crossed_send` are non-retryable with exit 65. Both delivered variants are
non-retryable with exit 70: `delivered_output_failure` means a direct send or
channel mutation committed but stdout receipt failed; `delivered_unarchived`
means inbox delivery committed but archive publication failed. Room
registration stdout failure after commit is reported as success with best-effort
diagnostics, not `delivered_output_failure`. Exit codes per the agent-CLI
standard: 2 usage, 65 validation (unknown_room, reserved_sender, empty_body,
ambiguous_id, duplicate_workspace, not_a_member, crossed_send), 66 not_found, 77
blocked_route (permission class), 78 config_invalid, 70 post-commit/internal
failure, 75 retryable pre-commit I/O.

## Quality gate

`cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D
warnings`, `cargo test --all-features`, `cargo build --release`. Tests must
cover: full send/inbox/read roundtrip against a temp `POST_MAIL_ROOT`; the
armed-route refusal quoting the reason; reserved-name refusal + free-form
sender + cwd-basename default; banner/framing present in BOTH text and json
read output; prefix matching incl. ambiguity; empty-inbox exit 0; atomic
write behavior (no partial .mail on simulated failure); envelope
deserialization of every output shape; migration: a mail file in the original
on-disk format reads back identically; channel join/send/read with cursor
advancement and `--peek`; channel watch backlog/live events without bodies or
cursor advancement; malformed channel isolation; blocked-route channel sharing
refusal; `not_a_member`; and schema/help consistency for all twelve commands and
every watch event variant.

## Stack

Rust 2021+, clap 4 derive, serde/serde_json, thiserror or anyhow at the
edge; keep dependencies minimal (no tokio — everything is local sync I/O).
Layout per the rust-agent-cli skill: src/main.rs, src/cli.rs, src/commands/,
src/output.rs, src/error.rs, src/lib.rs, tests/cli.rs.
