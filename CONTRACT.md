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
   room's tree (refused with the exact fix). Default sender: the registered
   room containing cwd, else cwd's basename. Resolved sender always appears in
   output (no silent ambient inference). Channel identity is stricter: `post
   chat` has no `--from` and no `--room`; the acting room is always the
   registered room containing cwd.
4. **Everything observable, append-only.** Every direct send also writes an
   immutable copy to `~/.claude-mail/archive/`. Nothing in the tool deletes
   mail; `read` moves inbox → read/ within the recipient's dir. Channel
   history is append-only under `channels/<name>/messages/`; channel reads only
   move per-room cursors forward after successful output.
5. **Registers stay distinct.** Direct-mail `kind` ∈ {letter, note, signal}.
   Channel messages have no `kind`, so a signal structurally cannot occur in a
   channel; anything gate-grade stays one-to-one room mail.

## On-disk format (existing mail must keep working)

- Root `~/.claude-mail/` (override: `POST_MAIL_ROOT` env, for tests).
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
  channel, subject, sent, event. Normal messages have no event; joins are
  recorded as event messages. Channel files are append-only: nothing in
  `messages/` is moved, edited, or deleted by reads.
- Channel membership is by registered room name. Joining records the acting
  room in `members.json`; blocked-route checks prevent any two rooms that are
  structurally blocked from sharing the same channel. Sending and reading
  require membership; non-members fail with `not_a_member`.
- Channel cursors are per room and per channel, separate from message history.
  A plain channel read advances only the acting room's cursor, only after a
  successful emit, and only to the last emitted message. `--peek` and `watch`
  never advance channel cursors. A sender's own message is advanced past only
  when the sender was already caught up; unread messages from others keep the
  cursor back so they still surface.

## Commands

Global flags: `--json` (machine envelopes; default for inbox/rooms/channels/
schema/doctor is already JSON — `--json` on send/read/chat switches them from
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
- `post read <id-or-prefix> [--room <name>] [--peek]` — prints framing banner
  + envelope + body (text default; `--json` gives `{ok, framing, envelope,
  body}`); after stdout succeeds, moves to read/ unless `--peek`. Text-mode
  envelope headers strip control characters except tab, and body output strips
  control characters except tab and newline, so neither can rewrite the
  framing banner. JSON mode preserves the parsed envelope and body unchanged
  as the byte-faithful surface. Ambiguous prefix: error listing
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
- `post chat <channel> --join` — joins a shared channel as the registered room
  containing cwd; creates the channel on first join; records a join event in
  append-only history; returns `{ok, channel, room, created, already_member,
  event_id}` with `--json`. Refuses unregistered cwd identity, invalid channel
  name, and blocked shared membership. There is deliberately no `--room` or
  `--from` override.
- `post chat <channel> --send [--subject <s>] [--oversize] (--body <text> |
  --body-file <path> | stdin)` — sends to a shared channel as the registered room
  containing cwd. `--body`/`--body-file` imply `--send`, so the verb is
  optional once a body is named; the deprecated positional FILE still requires
  it. The same 1 KiB subject limit, 32 KiB body guard, and warn-only watch-event
  detection used by direct mail run before the append-only channel write. A plain read whose
  stdout is the null device is refused before anything is emitted, leaving the
  cursor untouched; `--discard` is the deliberate way to advance past unread
  messages without printing them, and reports
  `{ok, channel, room, discarded, cursor}`. Requires
  membership; otherwise `not_a_member` with suggested fix `post chat <channel>
  --join`. Success JSON: `{ok, message}`. The channel message is committed to
  `channels/<name>/messages/<id>.msg`; after a committed send, stdout failure
  is `delivered_output_failure` and must not be blindly retried.
- `post chat <channel> [--peek]` — reads new channel messages as the registered
  room containing cwd. Requires membership; otherwise `not_a_member`. Text
  output includes the channel framing banner plus messages. JSON output is
  `{ok, framing, channel, room, peek, messages, count}` and preserves parsed
  message bodies unchanged. Text-mode message headers and bodies are sanitized
  at the output boundary so crafted controls cannot rewrite the framing banner.
  After stdout succeeds, a non-peek read advances only that room's cursor;
  `--peek` never advances.
- `post channels` — read-only listing of channels, members, creation metadata,
  and message counts: `{ok, channels, count}`.
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
- `post watch [--room <name>] [--once | --snapshot] [--interval-ms <ms>]
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
  direct mail `{"event":"mail", room, id, from, kind, subject, sent}`;
  unreadable direct mail or channel messages `{"event":"unreadable", room,
  id}` (filename-derived id, nothing quoted from the file); channel messages
  `{"event":"channel_message", channel, id, from, subject, sent}`. A room's
  own channel messages are never news to it and never ring its own watch.
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
  writes.

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
`delivered_output_failure`, `delivered_unarchived`, `not_a_member`.
Pre-commit `io_error` is retryable with exit 75. `duplicate_workspace` and
`not_a_member` are non-retryable with exit 65. Both delivered variants are
non-retryable with exit 70: `delivered_output_failure` means a direct send or
channel mutation committed but stdout receipt failed; `delivered_unarchived`
means inbox delivery committed but archive publication failed. Room
registration stdout failure after commit is reported as success with best-effort
diagnostics, not `delivered_output_failure`. Exit codes per the agent-CLI
standard: 2 usage, 65 validation (unknown_room, reserved_sender, empty_body,
ambiguous_id, duplicate_workspace, not_a_member), 66 not_found, 77
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
refusal; `not_a_member`; and schema/help consistency for all nine commands and
every watch event variant.

## Stack

Rust 2021+, clap 4 derive, serde/serde_json, thiserror or anyhow at the
edge; keep dependencies minimal (no tokio — everything is local sync I/O).
Layout per the rust-agent-cli skill: src/main.rs, src/cli.rs, src/commands/,
src/output.rs, src/error.rs, src/lib.rs, tests/cli.rs.
