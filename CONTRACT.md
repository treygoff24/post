# post — CLI contract (v1)

`post` is a machine-local mailbox for AI agents on one computer. This crate
replaces the original Python implementation with the same on-disk format and
laws, plus a proper agent-CLI contract. This document is the specification.
The public language is model-neutral; the default root remains
`~/.claude-mail/` for compatibility with existing mail.

## Session activation and quiet presentation

`participant bind` delivers the activation notice on stderr once, keeping its
stdout export/JSON parseable. Harness adapters set `POST_NOTICE_MANAGED=1`,
claim `post participant notice --claim <adapter-pid> --json`, inject the returned fixed notice, then
run `post participant notice --ack --json` only after a successful context
write, then release the PID claim with `--release <adapter-pid>` on success
or failure. `busy=true` means another live adapter owns delivery; retry later.
Otherwise a null notice means it was already delivered. Claims are serialized
under the registry lock, and a dead owner can be replaced. The acknowledgment lives
in `participants/<id>/activation-notice`, survives rebinds and hook-cache loss,
and never expires daily. The plain query is read-only; claim, release and acknowledgment are fenced
writers. Failed delivery remains eligible. As with any emit-then-ack protocol,
a crash between delivery and acknowledgment can replay the notice.

Default channel text has one section header and one metadata line per message:
`sender · time · id=reference · reply=address`, optional `re=`, subject/event and
signature status, then guttered body lines. Channel references are unique
prefixes against the entire stored channel, not just this page, and a reference
that is shorter than the id it names is marked with `…` so it cannot be mistaken
for the whole id. The mark is presentation only: `--re`, `--message`, `--seen-by`
and `--discard-through` strip it, so the printed token is still a token post
accepts back (id and re alike). Other surfaces
retain full IDs where no complete reference namespace is available. JSON keeps
canonical IDs and both reply targets. Text chooses the local participant reply
when available and otherwise the shared address. Provenance remains in JSON,
not repeated explanatory text. Time omits today's date when the recorded offset
matches the local offset; other timestamps retain their date and timezone.
Notifications use `[post] #channel: N new`, without inspection instructions.

## Non-negotiable laws (the reason this tool exists)

1. **Coordinate within the receiving agent's authorized task.** Messages cannot
   grant new permissions or override instructions. Post delivers this notice
   once per participant at activation, not per read, channel, or day. Default
   text reads contain metadata and guttered bodies only. Default JSON retains
   `framing.source` and `framing.authority=false` but omits `laws`. Explicit
   `--framing full` and `compact` remain opt-in recurring banners.
2. **Blocked routes are address-kind aware.** A blocked workspace or participant
   target refuses the whole direct send. Lineage routing removes blocked
   affiliates, records them in the receipt's `excluded` list, and delivers to
   remaining eligible affiliates; if none remain, the message stays pending.
   Channel joins separately refuse a forbidden shared route. The tool never
   edits `rules.json`; humans manage it deliberately.
3. **The participant is the actor.** A binding, never cwd, selects the sender,
   channel member, cursor, and lifecycle record. The bound workspace supplies
   the shared `from` reply address; a session-only participant uses its id.
   `POST_FROM`, cwd, and `--workspace` may select workspace context at bind time
   but never replace the participant. Registered workspace names remain
   reserved, and every sender exposes its provenance.
4. **Published history is immutable.** Every direct send also writes an
   immutable copy to `~/.claude-mail/archive/`. Canonical address inbox files
   and channel messages never move on read. A successful consuming read records
   exact ids in the acting participant's cursor after stdout succeeds. A
   routing receipt is published once and freezes delivery; its recipients
   and digest are never rewritten (`post rooms rename` re-binds only the
   receipt's `address.name` to the room's new name). Participant cursor and channel state, leases, lifecycle records,
   heartbeats, and `rooms.json` are mutable delivery or configuration state.
5. **Registers stay distinct.** Direct-mail `kind` ∈ {letter, note, signal}.
   Channel messages have no `kind`, so a signal structurally cannot occur in a
   channel; anything gate-grade stays one-to-one room mail.

## On-disk format (existing mail must keep working)

- Root `~/.claude-mail/` (override: `POST_MAIL_ROOT` env — a supported
  first-class root override, r2.1; the value must be absolute, and the
  harness/agent invoking post sets it deliberately, never for tests only).
- `rooms.json`: `{name: path-with-tilde}`. `rules.json`: `{"blocked":
  [{"from","to","reason"}]}`. First run creates empty defaults
  (`{"blocked": []}` and `{}`). New config files use mode
  `0600`.
- Migration fence (enrollment-owned): `POST_ARX_GENERATION` is parsed only for
  writers. A missing declaration is the ordinary legacy writer mode only while
  `.post-arx.json` is absent; an enrolled writer must present the positive
  generation matching the `active` state. Missing, zero, stale, malformed, or
  non-UTF-8 declarations refuse before any mailbox mutation. Reads do not parse
  or reject this variable. `.post-arx.json` contains exactly `{"state":"fenced"
  |"active","generation":<positive integer>}` and any state file arms
  read-only protection; `.post-arx.lock` is its solitary regular-file flock
  anchor. Legacy writes with no state and no generation skip that lock, while
  enrolled writers require the existing trusted lock; refusal never creates it.
  Enrolled ordinary writes hold the root flock through mutation. Each long
  non-snapshot watch iteration re-admits under that flock before its heartbeat
  and holds it through lease/heartbeat renewal, pending routing, and the full
  read-only projection scan; stdout is written only after release. The lock
  inode is never unlinked or recreated. External cutover must quiesce and drain
  legacy writers, take the same flock, fence, wait out enrolled writers by
  lock ownership, then copy and activate; a writer's slow stdout and its
  after-stdout cursor update remain inside the same lock hold, so slow
  stdout can block the lock; operator quiesce and timeout handling must bound
  that drain. Post exposes no enrollment or cutover CLI; external cutover
  tooling owns these transitions. Neither state nor lock may be deleted. The
  state, lock, and
  actual state atomic temporary-name namespace
  are reserved room names; the actual `.post-arx.json` temporary name is
  `..post-arx.json.<pid>.<nonce>.tmp`, and no lock temporary namespace is
  produced or reserved.
- Under an enrolled/fenced store, read-only forms stay available and never
  write: `read --peek`, direct-mail and channel body slices, `chat --peek`,
  `chat --history`, `chat --since`, `chat --seen-by`, `search`, `watch
  --snapshot`, `schema`, `version`, `doctor` (without `--fix`), `profile`
  (show), `owner` (show), `participant show`, `participant list`, `identity
  list`, `identity show`, the listings (`inbox`, `rooms`, `channels`,
  `who`), and `rooms rename --dry-run`. A real `rooms rename` is a writer:
  it is admitted through the fence and refused without a matching
  generation.
- The same read-only forms remain available without a participant binding and
  never mint or initialize participant state. An unbound session gets an
  explicit answer on stdout (see "Identity states"); the human line on stderr
  is text mode only and never appears under `--json`. `post participant show`
  carries its own unbound payload; `post version` bypasses participant
  resolution. `post watch --snapshot` stdout remains NDJSON or empty, never
  prose except the text-mode unbound line.
- Consuming reads, exact `--ack` forms, `catchup`, long-running `watch`, and
  every send or state change are admitted as writers and are refused without a
  matching generation. Admitted read-only forms create no root/room directory,
  banner-day, heartbeat, or cursor writes. A long non-snapshot watch re-admits
  before every heartbeat. It exits nonzero if the store generation changes or
  the state file disappears. Under a same-generation fence or transient
  admission error it keeps scanning and notifying read-only, skips routing and
  lease/heartbeat refresh, warns once per episode, and retries. Every snapshot
  form is read-only and never
  touches a heartbeat, lease, routing receipt, cursor, or mailbox directory.
- `post rooms add` takes an advisory exclusive flock on `.rooms.lock`, then
  reloads, validates, and atomically replaces `rooms.json` while preserving its
  mode. It never writes `rules.json`. A symlinked `rooms.json` is refused rather
  than detached.
- `.rename.lock` is the room-rename flock. `post rooms rename` holds it
  exclusively from before it takes any other store lock until `rooms.json`
  commits. Every command that creates or writes a room's mailbox by room name
  holds it shared: `post send`, `post read`, and `post chat` (when they write)
  and `post catchup` (whose after-stdout commit records seen ids under the
  workspace it resolved) from before their body loads `rooms.json` or resolves its actor until their
  after-stdout cursor commit finishes (`post send` reads its body from stdin,
  `--body-file`, or `--body` before taking the lock, so a stalled stdin
  producer never holds it); `post doctor --fix` while it creates
  room directories; and a writing `post watch` through its target setup only
  (the long loop creates no room directory). Read-only commands do not take
  it. Lock order is migration-fence admission, then `.rename.lock`, then
  `.participants.lock`, then `.rooms.lock`, then a participant's
  `.cursors.lock`; nothing takes `.rename.lock` while holding another store
  lock, so it adds no cycle. A send issued during a rename waits for it and
  then resolves the name against the committed registry. The lock does not
  outlive a crashed rename, so the rename journal covers that window (see
  `rename-journal.json` under `post rooms rename`): while it stands, no
  writer creates or writes a mailbox directory for a room it names as
  `old` or `new`.
- Canonical mail is stored by address: workspace mail at
  `<root>/<room>/inbox/<id>.mail`, lineage mail at
  `<root>/lineages/<name>/inbox/<id>.mail`, and participant mail at
  `<root>/participants/<id>/inbox/<id>.mail`. A mail file is a JSON envelope,
  then `\n---\n`, then the raw body. The envelope keeps the legacy `id`, `from`,
  `to`, `kind`, `subject`, and `sent` fields and may add `from_participant`,
  `from_lineage`, and `address_kind`. The directory named `inbox` under a
  lineage is canonical address storage, not a lineage-owned inbox or read
  cursor.
- Envelope JSON is indented with two spaces and ASCII-escapes non-ASCII
  characters with lowercase `\u` escapes.
- Each canonical address store has `routing/<id>.json` receipts beside its
  inbox. A receipt freezes the recipient ids and message digest in a one-time
  publication and is never otherwise rewritten; `post rooms rename` re-binds a
  workspace receipt's `address.name` to the new name, because readers refuse a
  receipt whose address is not theirs. Missing receipt means pending. The
  archive copy remains `archive/<id>.mail`.
  Canonical mail never moves on read; `<room>/read/` is read-only legacy state.
- Mail writes are atomic. New mail files are published with an exclusive final
  create after a synced temporary write, so an existing inbox or archive file
  is never replaced. The exclusive hard-link is the commit point: later temp
  cleanup or directory-sync failures produce a warning but never report the
  committed write as failed. Send publishes inbox first and archive second, so
  an inbox failure cannot strand an archive-only message. Routing receipt
  publication is atomic under `.participants.lock`. Default config creation is
  exclusive and never replaces existing content.
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
- Channel membership is participant-scoped in
  `participants/<id>/channels.json`. An explicit join applies to that
  participant. Legacy `members.json` workspace membership supplies a default
  that one participant may leave without removing a sibling. Sending and
  reading require effective membership; non-members fail with `not_a_member`.
  An explicit join's instant is stored in `participants/<id>/membership-starts.json`
  (`{version: 1, starts: {<channel>: "YYYYMMDD-HHMMSS-ffffff"}}`), a sibling
  file because `channels.json` parses with `deny_unknown_fields`; older
  binaries never open it. A joined channel with no recorded start falls back to
  the participant's `created`. A malformed file (shape, version, channel name,
  or watermark form) makes that participant's channel operations fail with
  `config_invalid`, as a malformed `channels.json` does; `post doctor`
  reports it as `participant.<id>.membership_starts_invalid` (error, detect
  only) with the file's path.
- Cursor state is separate from message history: exact seen sets for every mail
  address and channel the participant consumed. Workspace and lineage fan-out
  exclude the sending participant. An explicit `participant:<self>` delivery
  is the exception: it is initially unread to self and is consumed normally
  when read. Channel messages from the participant itself are excluded. Plain
  bounded reads consume only ids actually emitted after stdout succeeds; peeks
  and watch never mutate state.
- The canonical file is `<root>/participants/<id>/cursors.json`, version 2:

  ```json
  {
    "version": 2,
    "mail": {
      "workspace:codex": {"seen": ["20260831-171234-a1b2c3"]}
    },
    "channels": {
      "machineroom-devbox": {
        "seen": ["20260831-171234-123456-a1b2c3"]
      }
    }
  }
  ```

  Mail keys are typed addresses (`workspace:`, `lineage:`, or `participant:`).
  Values are sorted, duplicate-free arrays of canonical ids; maps and sets
  serialize lexically, pretty-printed, with one trailing newline. State is mode
  `0600` and every replacement is atomic.
- Every cursor mutation holds an exclusive `flock` on
  `<root>/participants/<id>/.cursors.lock` across reload, exact-set union, and
  atomic replacement. The lock is a solitary regular inode checked after
  acquisition and is mode `0600`; concurrent acknowledgements therefore keep
  the whole map instead of losing marks. Seen-sets only grow, so a late id
  below newer consumed ids still surfaces. Growth is linear in history; a
  50,000-id warning remains the operational threshold and watermark compaction
  is unsafe while late backfills can arrive.
- Missing, malformed, unknown-field, wrong-version, invalid-id, symlinked, or
  non-regular cursor state is advisory-invalid on read-only loads: the whole
  snapshot becomes empty, one sanitized warning goes to stderr, and eligible
  messages are treated as unread. `post watch` states that on what it emits
  rather than leaving it to the warning: every event projected from such a
  participant carries `"cursor_unusable": true` (the key is absent when the
  state is readable, so a healthy event keeps its exact previous shape), a
  `--text` event line from it is prefixed `[cursor unusable: re-reporting
  history]`, and a `--digest --text` group from it renders `<count> re-reported
  cursor unusable` in place of `<count> new`. Nothing is reset, rewritten, or filtered; the
  degrade is reported as what it is. A consuming writer refuses an unsafe or
  unwritable cursor/lock path rather than claiming persistence. `doctor`
  diagnoses these files but never repairs them.
- Room-level `cursors.json`, `channel-state.json`, and `read/` are legacy,
  read-only evidence. Participant reads do not import or materialize them.
  `doctor` labels them as legacy state.

## Commands

Global flags: `--json` (machine envelopes; inbox/rooms/channels/profile/owner/
who/schema/doctor are already JSON, while send/read/chat/catchup/search switch
from text),
and `--pretty`. `--room` is not global; it is a command option for
inbox/read/watch/who only. A leading global `--json` refuses the same
human-only forms as a trailing one: doctor `--brief`, and `--text` on
channels/who/inbox/watch. No prompts ever; no color; stdout = results, stderr =
diagnostics/errors.
On Unix, result stdout is written directly to inherited fd1 through an
unbuffered strict writer rather than Rust's EBADF-tolerant `StdoutRaw` wrapper.
An invalid or read-only descriptor cannot count as a successful emit, and no
`after_stdout` cursor update, catchup delta, or exact acknowledgement runs.
A `post send` that landed never exits nonzero: when its receipt cannot be
written, the mail is on disk exactly once, the exit code is 0, and one stderr
line says the change was committed (a nonzero exit made callers resend, and the
resend was a second copy). Other committed channel mutations keep the
non-retryable `delivered_output_failure`; committed room-registration output
failures retain their existing success semantics. A read-only command whose
reader closed the pipe (`post who --text | head`) stops quietly with its own
exit code rather than reporting a retryable `io_error`; only a command that
still owes a state change after its output (a consuming read) keeps the error.
`--json` output is one JSON document on stdout with nothing on stderr, so
`2>&1 | jq` parses it; what is degraded or worth a second look rides in the
document. A command run with `--text` or `--brief` reports its failures as
prose on stderr; every other failure is the JSON error envelope. `post
--version` prints the same line as `post version`
(`post <semver> (build <short-sha>[-dirty], store v2; ...)`); `-dirty` marks a
build from a tree with uncommitted tracked changes.

- `post send --to <workspace:<room>|lineage:<name>|participant:<id>|bare-name>
  [--from <name>] [--kind letter|note|signal] [--subject <s>] [--oversize]
  [<body> | --body <text> | --body-file <path> | stdin]`: the body forms are
  mutually exclusive alternatives; a bare positional argument is the body, the
  same as `--body` (the deprecated positional FILE is gone: the recipient is
  always `--to`, and a bare argument that looks like a path, meaning one token
  with no whitespace that contains `/` or ends in a file extension such as
  `.md`, `.txt`, or `.json` (a URL is text), is refused whether or not the file
  exists, with the `--body-file` command that sends that file; `--body` or
  stdin carries a literal path); prefer stdin (a quoted
  heredoc) or `--body-file`, and keep `--body` for short plain one-liners, since
  the shell parses argv before post sees it. A body-file path that does not
  exist is `invalid_argument` (a usage error)
  rather than a retryable `io_error`. Bare targets resolve deterministically in
  workspace → lineage → participant priority; typed `kind:name` addresses bypass
  that priority. Refuses: unknown recipient, a blocked direct target (quotes
  reason), reserved-name impersonation, subjects over 1 KiB, empty body, and
  bodies over 32 KiB unless `--oversize` records explicit intent. A complete
  Post watch-event NDJSON line does not block legitimate forensic traffic; the
  receipt carries a warning (`warnings` under `--json`, a `post: warning:` line
  in text). Success
  (text): `post: sent <kind> <id> <from> -> <to>` followed by a participant
  readback command. Success (json): full envelope + `archived: true`, plus
  `warnings` (strings) only when there is something to say. The hidden
  `--allow-self` (delegate's completion pings pass it) retargets a send whose
  `--to` is the sender's own room or lineage to the sender's own participant
  inbox and says so in the receipt (a `post:` note in text; under `--json` a
  `retargeted` object `{from, to, note}` naming what `--to` resolved to, where
  it went, and why); other members of that room do not receive
  it, and any other target is unaffected. Rules are
  reloaded after payload construction immediately
  before each inbox publication attempt. If inbox commits but archive
  publication fails, `delivered_unarchived` is non-retryable and the message
  must not be resent. Workspace and lineage fan-out exclude the sending
  participant; an explicit `participant:<self>` target is readable by self.
  Lineage sends instead exclude blocked affiliates and route to any remaining
  eligible recipients, recording exclusions in the receipt.
- `post inbox [--room <name>] [--text] [--adopt]`: read-only listing, oldest
  first. Bound JSON is `{ok, participant, room, unread, count,
  skipped_unreadable, unread_count, pending, pending_by_address, held}`. `pending` is
  separate from `unread_count` and is never added to it. An unbound listing
  returns no unread items, reports provisional pending workspace mail, writes
  nothing, and puts its notice on stderr. `--adopt` is the writer form: it
  routes held mail for the caller's current lineage to the active affiliates
  eligible then, without giving later affiliates that backlog.
  Receipt-less mail already in an address inbox appears here only through the
  `pending` and `pending_by_address` counts, never as pending ids; `--text`
  marks the pending count. `watch --snapshot` lists each provisionally eligible
  id with `pending: true`. For eligible workspace or participant mail, a bound
  consuming `post read <id>` publishes the frozen receipt and consumes that id,
  while an admitted long watch routes a new arrival on its next scan. Held
  lineage mail stays held until `post inbox --adopt`; neither read nor long
  watch adopts it.
- `post read <id-or-prefix> [--room <name>] [--peek] [--max-bytes <n>] [--framing auto|full|compact]`
  — prints a sender/time/id/reply header and a `| `-prefixed body. JSON gives
  `{ok, framing, envelope, body}` with unchanged canonical envelope/body bytes.
  A consuming read records the exact id only after successful stdout; peeks
  and slices write nothing. Headers strip controls except tab; bodies preserve
  tab/newline but strip other controls. `auto` is quiet. Explicit `full` and
  `compact` request recurring policy text. JSON source/authority stay stable;
  `laws` is omitted in auto and present only in explicit banner modes.
  Ambiguous prefix: error listing the matches. A prefix matching nothing unread
  may resolve participant-visible canonical mail or a message the participant
  sent; that inspection is cursorless and reports the appropriate already-read
  or own-message state. Legacy room `read/` is not a participant delivery
  source.
  `--max-bytes <n>` is opt-in and caps actual final stdout bytes, including
  UTF-8, JSON escaping, pretty whitespace, framing, omission metadata, and the
  trailing newline. A complete body is returned and consumed normally only if
  the full result fits. Otherwise `body` is absent, `count: 0`,
  `selected_count: 1`, `has_more: true`, and bounded `omitted` metadata names
  the id, body byte count, and a safe slice command; the mail remains unread.
  If that required scaffold does not fit, the command returns
  `invalid_argument` on stderr with the measured minimum, emits zero stdout,
  and changes no read state. Omitting the flag preserves existing behavior and
  output shape.
  Omission continuation commands carry a separately measured slice budget
  sufficient for that exact stored envelope at the body's widest possible
  continuation offsets plus its costliest encoded UTF-8 scalar (or EOF
  scaffold). The fixed-point calculation remeasures after decimal budget width
  changes, so every emitted continuation remains runnable across 9/10,
  99/100, and later scalar-cost boundaries. It does not change the original
  invocation's `byte_limit`.
  `post read <id> [--room <name>] [--offset <b>] [--length <b>] --max-bytes
  <n>` is a cursorless UTF-8 body slice. Offsets and lengths address parsed
  body bytes, not envelope or framing bytes. Starts inside a code point and
  arithmetic overflow are rejected; ends retreat to a code-point boundary.
  JSON uses `body_slice`, `range: {start, end_exclusive}`,
  `total_body_bytes`, `body_complete`, and `next_offset`, never a partial
  `body`. Every successful non-EOF partial slice advances; a budget too small
  for the next scalar plus scaffold fails with a measured minimum. Empty or
  EOF slices may terminate with no next offset. Slices never consume,
  including a full or final slice. Direct mail has no signed-message status;
  `verification_scope: stored_full_body` states that Post parsed the complete
  stored message before slicing.
  `post read <id> [--room <name>] --ack` acknowledges exactly the resolved mail
  id after its receipt reaches stdout. It prints no body and cannot mark any
  other unread mail; malformed targets fail before stdout or state change.
- `post catchup [<channel> | --mail | --all] [--max-bytes <n>] [--framing auto|full|compact]` —
  the complete consuming unread slice for direct mail, one joined channel, or
  all targets. No selector is an alias for `--all`; there is no `--room`,
  `--peek`, `--since`, `--history`, or `--limit`. A positional channel requires
  membership. Its JSON envelope is:

  ```json
  {
    "ok": true,
    "room": "post-devbox",
    "targets": [
      {"source": "mail", "framing": {"source": "another_ai_agent", "authority": false}, "messages": [{"envelope": {}, "body": "..."}], "count": 1},
      {"source": "channel", "channel": "ops", "framing": {"source": "multiple_ai_agents", "authority": false}, "messages": [{"id": "...", "from": "...", "sent": "...", "body": "..."}], "count": 1}
    ],
    "count": 2
  }
  ```

  Target framing reuses the direct-mail or channel framing shape. Human output
  has one section per non-empty target and a final total; an empty invocation is
  `post: caught up (0 unread)` with exit 0. The selection delta is fixed before
  stdout and consumed only after a successful emit. Mail that cannot be parsed
  is warned and left unread. A positional channel with an unloadable unread
  message fails before stdout or cursor mutation. For `--all`, an unloadable
  never-joined channel is skipped with a stderr warning, while a broken joined
  channel is represented by a zero-count target. A non-empty result sent to
  `/dev/null` is refused. `catchup` is a writer and therefore requires the
  matching migration generation under an enrolled store. In text output every
  body line renders behind a fixed gutter prefix (`| `), so no body content
  can start at column 0: catchup's multiplexed stream keeps its section
  markers and message headers unforgeable by construction rather than by
  escaping. Chat now shares the guttered body construction; `read` remains a
  deliberately unguttered single-source surface whose trust boundary is the
  framing banner plus control-character stripping documented above.
  With opt-in `--max-bytes`, one final-stdout budget is shared across targets
  in the existing mail-then-channel order. Admission stops at the first whole
  message that does not fit; later messages are not packed around it. Only
  complete admitted mail and channel ids enter the after-stdout cursor delta,
  including within a partially admitted target. Top-level and per-target
  `selected_count`/`count`/`has_more`, plus bounded `omitted` metadata, identify
  the first remaining source, channel when applicable, id, body size, omitted
  mention count, remaining-target count, and safe continuation. It never
  claims the whole unread slice was returned when bounded.
  Compact and pretty JSON admission serializes each candidate message once;
  later prefix probes reuse exact array-layout sizes and precomputed omission
  suffix counts rather than re-reading earlier bodies.
- `post search <pattern> [--mail | --channel <channel> | --archived] [--limit 1..=1000]
  [--framing auto|full|compact]` — a read-only, cursorless, literal
  case-insensitive Unicode substring search. Default scope is
  participant-visible direct mail plus channels where the acting participant
  is an effective member. `--archived` instead searches every archived
  channel on the host with no membership check and no mail; archived history
  is the one channel surface open to non-members, so agents can find a
  channel to resurrect. `--mail`, `--channel`, and `--archived` conflict; there is no
  `--room`. A named channel requires effective membership, and visibility checks happen
  before message content is opened. The default limit is 100 and the hard cap
  is 1000. Results are deterministic newest-first by UTC id components, with
  sanitized previews capped at 160 Unicode scalar values; `truncated` means a
  match existed beyond the returned cap, not a total count.

  ```json
  {
    "ok": true,
    "framing": {"source": "multiple_ai_agents", "authority": false},
    "room": "post-devbox",
    "pattern": "fence",
    "match": "literal_case_insensitive",
    "results": [{"source": "channel", "channel": "ops", "id": "...", "from": "...", "sent": "...", "subject": "...", "preview": "...", "matched": ["body"]}],
    "count": 1,
    "limit": 100,
    "truncated": false
  }
  ```

  Mail results use `source: "mail"`, `channel: null`, and add `kind`; channel
  results use `source: "channel"`, a channel name, and no `kind`. Imported
  roomless channel results carry `from_host`, render `<id>@<host>` in text,
  and return `reply_to_participant: "participant:<id>@<host>"`. Reply fields
  follow the origin rules, and provisional mail is labeled pending. No-match is
  exit 0 with an empty result array. Search writes no routing receipt, cursor,
  or banner-day state, never moves mail, and does not affect watch.
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
  `*`, `archive`, `rooms.json`, `rules.json`, `.rooms.lock`, `.rename.lock`,
  `rename-journal.json`, and the
  `.rooms.json.*.tmp`, `.post-arx.json`, `.post-arx.lock`, and
  `..post-arx.json.*.tmp` atomic-write namespace; paths with control characters;
  missing/non-directory paths; and any registration targeted by a blocking rule
  (including `to: "*"`), quoting the rule's reason verbatim.
  Validation and replacement are one flock-protected transaction. It never
  creates the workspace, overwrites an existing registration, or modifies
  rules.json beyond the one registration it performs. When the refused name
  case-folds to a remote placeholder, the error names the owning host in
  `details.host` and its `exact_fix` adds the room under the estate's
  `<name>-<host-suffix>` convention (learned from this host's own suffixed
  registrations, else the bridge host id); a local duplicate keeps the
  `set-path` hint. A local room `<base>-<s>` teaches suffix `<s>` only when
  `<base>` is a remote placeholder and the room's registered directory is
  itself named `<base>` (ASCII case-insensitive), so a checkout like
  `hq-devbox` at `.../hq` votes and a project like `cos-crons` at
  `.../cos-crons` does not. The offered candidate is the first, in order of
  the learned suffixes by rank and then the bridge host id, that is a valid
  and untaken room name, names no lineage, and has no blocked route to it;
  when none survives, no `exact_fix` is offered. On a bridged host (one with
  `bridge/config.json`), a name a peer host publishes on the bridge is refused
  the same way, by `add` and by `rename`'s new name: `invalid_argument` with
  `details.host` naming the publisher and an `exact_fix` that registers
  `<name>-<this host>` (the same candidate rules apply, and the candidate must
  not itself be published). The publications read are
  `bridge/rooms/peers/<host>.json` (`{"v":1,"host":"<host>","rooms":[...]}`,
  which must name its own host) and the entries for other hosts in
  `bridge/rooms/owners.json`; names match ASCII case-insensitively. The files
  are advisory and only as current as the bridge's last tick, so the check
  judges its evidence. Evidence is fresh when `bridge/health.json` ticked
  within three of its `interval_s` and every publication read cleanly (an
  enrolled peer with no publication file counts as unread). A listed name
  refuses on any evidence; when the evidence is not fresh the message says it
  may be out of date and how long ago the bridge last confirmed its state (or
  that the age cannot be told). A name the files do not list is accepted; when
  the evidence is not fresh (stale or missing health, a malformed, oversize,
  or mislabeled file, a missing publication) `add`'s stdout carries a
  `warnings` array and `rename`'s receipt a `warnings` entry saying the peer
  names could not be verified and how old the evidence is. A host with no
  `bridge/config.json` is never checked and says nothing. Resuming an
  interrupted rename is exempt.
- `post rooms set-path <name> <path> [--dry-run]` — re-points a local room's
  workspace (discovery) path under the same locks and validation as `add`;
  it never moves mail or history, never rewrites participant records, and
  always refuses remote placeholders in either direction.
- `post rooms rename <old> <new> [--dry-run]` — renames a local room under
  the participant and rooms locks. It moves `<root>/<old>` to
  `<root>/<new>` with a single rename, rewrites every live reference to the
  name (participant `workspace` fields, participant cursor `workspace:<old>`
  keys, each read and replaced under that participant's `.cursors.lock`,
  which is held from before the file is read until the rename commits or
  rolls back; `channels/*/members.json` keys, bare `profiles.json` keys, and the
  `address.name` of every `routing/<id>.json` receipt bound to
  `workspace:<old>`, written at its moved location exactly as routing would
  write it for `<new>`; when `members.json` or `profiles.json` already has a
  bare `<new>` key, the renamed room's entry replaces it and the receipt
  carries a warning naming each overwritten key and its store), then commits
  `rooms.json` last. The `rooms.json` write is inside the rollback: any
  failure up to and including it restores every rewritten file to its
  original bytes and moves the directory back. If a restore itself fails,
  or the mailbox cannot move back because `<root>/<old>` exists again,
  stderr says so, the journal below stays, and the error's fix is the
  resume command (which then refuses on the recreated `<root>/<old>`).
  Before its first store change the rename atomically writes
  `<root>/rename-journal.json` (`{"v":1,"old":…,"new":…,"started_at":…}`),
  and it removes the journal after `rooms.json` commits or a clean rollback
  finishes. A journal that survives (a crash, or a failed rollback) is an
  interrupted rename. `post doctor` reports it as the error
  `rooms.rename_interrupted` with the resume command, plus the error
  `rooms.rename_old_recreated` when `<root>/<old>` exists again beside
  `<root>/<new>`. Every other `post rooms rename` refuses
  (`invalid_argument`, `exact_fix` `post rooms rename '<old>' '<new>'`).
  While the journal stands, `post send` to either named room, a legacy
  room's mailbox or cursor write for either, and anything else that would
  create `<root>/<old>` or `<root>/<new>` refuses with `config_invalid`
  and the same `exact_fix`, creating nothing. Rerunning that same pair resumes it:
  when `<root>/<old>` is gone and `<root>/<new>` exists, the move is skipped,
  the rewrites are re-planned from `<root>/<new>` (each is idempotent), and
  `rooms.json` commits. When `rooms.json` already names `<new>`, only
  pending rewrites run. The receipt says `resumed: true`. A resume refuses,
  and never merges, when `<root>/<old>` exists again (by hand, an older
  `post`, or a writer outside this binary): it lists that directory's files in
  `details.matches` so they can be moved by hand. Published
  history is never rewritten: archive letters, channel messages, and the
  moved directory's other contents keep the old name. It refuses an unknown or
  remote-placeholder old room, any `add` check on the new name (including
  placeholder duplicates), a case-only rename, an existing `<root>/<new>`,
  an `owner.json` or `rules.json` naming the old room, and on a bridged host
  any `bridge/health.json` that is not fresh or whose `local_held` counters
  are not both integer 0 (retryable `bridge_guard_unavailable`; `ok:false`
  alone does not refuse). Because the counters carry forward on busy and
  quiet ticks, a bridged rename also scans `archive/*.mail` with the
  bridge's own outbound candidate rule: a workspace-addressed envelope
  (`address_kind` absent or `"workspace"`, no `to_host` key) whose `to` is
  exactly `<old>` and that has no `bridge/received/<id>`,
  `bridge/published/<id>`, or `bridge/delivered/*/*/<id>` marker must have a
  `bridge/local-held/<id>.json` record; each marker counts only when its
  target exists, so a dangling symlink is absent, as it is to the bridge.
  Any such letter without a present record refuses
  with the same retryable `bridge_guard_unavailable`, naming the count and up
  to 8 ids (`details.matches`); the bridge stamps holds on its next full
  tick. Envelopes that do not parse are skipped, as the bridge skips them. `--dry-run` runs every check and writes nothing.
- `post chat <channel> --send [--re <id>] [--subject <s>]
  [--oversize] [--signature-ref <tag>] (--body <text> | --body-file <path> |
  stdin)`: sends to a shared channel as the bound participant. A session-only
  participant may join explicitly; a workspace may supply legacy default
  membership. `--body`/`--body-file` imply `--send`, so the verb is
  optional once a body is named; the deprecated positional FILE still requires
  it. After a successful local write, JSON includes
  `cross_host:{status:"queued"|"local_only"|"unconfirmed",reason?}`. `queued` means this host's
  fresh bridge advertises channel support, this channel is selected by policy,
  and an enrolled peer exists; it does not promise delivery. `local_only` names
  a lasting reason the post will stay here: no bridge config, disabled or denied
  sync, a name the bridge refuses, or no peer. `unconfirmed` means bridge health
  is missing, stale, or incomplete, or a fresh bridge predates roomless relay.
  A roomless post sent through that older bridge is backfilled after its upgrade;
  do not resend. Check `post doctor` or the bridge. Both states print one stderr
  line. A roomless participant (`from ==
  from_participant`) crosses with bridge v2 r6.3; the relay stamps `from_host`.
  Remote reads render `<id>@<host>` and give
  `reply_to_participant` and `reply_to_shared` as
  `participant:<id>@<host>`; a local participant id
  collision is quarantined. The same 1 KiB subject limit, 32 KiB body guard, and warn-only watch-event
  detection used by direct mail run before the append-only channel write.
  Bodies are scanned for `@<room>` word-boundary mentions of registered rooms
  (stamped into the envelope as `mentions`). `--re <id>` stamps a reply to a
  prior message in the same channel (full id or unique prefix). A send always
  delivers. When ordinary unseen messages from other participants exist in the
  channel, the JSON receipt carries `crossed: {unseen, addressed_to_you,
  messages[]}` (at most 10 messages, newest last, each `{id, from,
  display_name?, sent, addressed_to_you, body}` plus `signed_verified?`,
  `sender_address?`, and `sender_provenance?` when they apply): the whole body
  for a message addressed to the sender, a 300-character preview otherwise;
  text mode prints them after the sent line. The crossed messages stay unread.
  System join/profile events never count as crossed. A plain read whose stdout is the null device is refused
  before anything is emitted, consuming nothing; `--discard` is the deliberate
  way to mark every currently-existing unseen message seen without printing
  them, and reports `{ok, channel, room, discarded, cursor}` (`cursor` is the
  max seen id — a compatibility summary of the underlying seen-set).
  `--discard-through <id>` is the targeted form: it marks every
  currently-existing unseen id at or below one message as seen and nothing
  beyond it, for a reader (such as a phone client) that has rendered up to a
  known id and wants to ack only that much.
  `<id>` is a full message id or a prefix unique within that channel, resolved
  against the channel's message filenames — an id from another channel is
  `not_found`, an ambiguous prefix is `ambiguous_id`. It refuses with
  `config_invalid` when an unreadable message sits in the affected range
  (every currently-existing unseen id at or below the target): a message that
  cannot be rendered has certainly not been read, and nothing is consumed past
  it. It is replay-safe — a target whose whole range is already seen is
  success with `advanced: false` and a byte-identical state file, not
  an error, so a lost response can simply be retried. JSON is
  `{ok, channel, room, target, prior_cursor, cursor, advanced, discarded}`;
  text is a one-line summary. Unlike every body-returning read, this one
  mutates BEFORE emitting its receipt, because the receipt's whole job is to
  report the state that is now stored; nothing is skipped unreported, since a
  retry replays as a no-op.
  `--ack <id>` is the narrower exact-id acknowledgement used after isolated
  slices. It resolves membership and a full or unique-prefix target normally,
  parses that exact stored record, prints no body, and adds only that id to the
  seen-set after stdout succeeds. It does not acknowledge an earlier range,
  auto-join, or bypass route or malformed-record checks. Explicit
  acknowledgement is operator intent, not proof that every slice was fetched.
  `--discard-through` retains its range semantics and is not the
  acknowledgement for an isolated sliced message.
  A plain consuming read defaults to displaying the oldest 25 unread when the
  backlog is larger. It consumes only what it emits; `skipped` reports how many
  newer messages remain unread and text says `N newer message(s) remain unread
  — run again to continue`. Explicit `--limit <n>` emits the oldest N unread;
  `--limit 0` means unlimited. With `--peek` the bound remains a newest-slice
  display-only glance and nothing is consumed; its `skipped` count is the
  omitted older remainder.
  Unread starts at the participant's membership start (join from now): the
  instant of its explicit join, reset by a rejoin after `--leave`, or its own
  `created` under legacy workspace membership. The start is a UTC
  `YYYYMMDD-HHMMSS-ffffff` watermark compared against message ids, which carry
  the same UTC prefix; a message whose id sorts before it is history. History
  is never unread: it is absent from `post channels` unread counts, plain
  consuming reads and their `has_more`, a send's `crossed` block, `post watch` channel
  and mention events (live and `--snapshot`), catchup, and `--discard`/
  `--discard-through` counts. It stays readable: `--peek` glances at every
  unseen message including history (its @mention rescue skips history), and
  `--history`, `--grep`, `--since`, and `post search` ignore read state. A join
  that makes a new member reports `history_before_join` (messages older than
  the start) and a runnable `history_hint` (`post chat '<ch>' --history 20`);
  an `already_member` join (the channel is already in the explicit joined
  set) records nothing and reports `history_before_join` null. Every join
  receipt carries `backlog_ignored`, true only when an explicit member passed
  `--backlog`: its start is kept, and `history_hint` is then
  `post chat '<ch>' --leave && post chat '<ch>' --join --backlog`, the
  sequence that makes the whole backlog unread; otherwise an already-member
  `history_hint` is null. A legacy workspace member's `--join` still
  records an explicit join and posts the join event, but its start is the one
  it already had (its `created` watermark, or the backlog floor if `created`
  is unparseable), never now, so no unread message becomes history;
  `history_before_join` counts ids below that floor. `--join --backlog` (valid
  only with `--join`, including from a legacy member) records a start before
  every message, restoring the all-unread join. A bridge-imported channel message keeps its original id,
  so an import older than a member's start reads as history.
  The membership start is a participant's, so it binds only participant reads.
  Unbound room mode has no participant and therefore no start and no floor:
  `post watch --snapshot --room <room>` run without a participant binding
  keeps the old rule that
  every channel message the room has not seen is new, so a message from
  before any join still surfaces there, including as `reason: mention`. Join from now does not cover room
  mode.
  `--seen-by <id>` is a read-only listing of member participants whose seen-set
  contains that message (`cursor` fields in JSON output are max-seen-id
  summaries for compatibility, never the model). Consuming reads fail closed:
  an unreadable/unparseable unseen `.msg` file makes a plain read return
  `config_invalid` with the seen-set untouched (a read consumes only messages
  it emitted, never one it could not). Non-consuming `--history`/`--since`
  reads warn on stderr and skip unreadable messages. The crossing check applies
  the same posture: an unreadable unseen file from another participant is left
  out of `crossed` and named in the receipt's `skipped` list (`[{id, reason}]`)
  while the send delivers; malformed files already in the seen-set are
  ignored. Requires
  membership; otherwise `not_a_member` with suggested fix `post chat <channel>
  --join`. Success JSON: `{ok, message}`. The channel message is committed to
  `channels/<name>/messages/<id>.msg`; after a committed send, stdout failure
  is `delivered_output_failure` and must not be blindly retried.
- `post chat <channel> [--peek] [--max-bytes <n>] [--framing auto|full|compact]` — reads new channel
  messages as the bound participant. Requires effective membership; otherwise
  `not_a_member`. Text
  output includes a channel header plus messages (reply references
  render as `↳ re <short-id> (<sender>: preview…)`). JSON output is
  `{ok, framing, channel, room, peek, messages, count, skipped?, has_more}` and
  preserves parsed message bodies unchanged (`re` and `mentions` when present).
  Text-mode headers, provenance/status lines, and bodies are sanitized at the
  output boundary; chat body lines render behind `| ` so body content cannot
  reach column 0 and imitate a header or trust marker. After stdout succeeds,
  a non-peek read records only its emitted page in that participant's seen-set;
  `--peek` records nothing. `--framing auto` is quiet; `full` and `compact`
  explicitly request recurring banners. No mode consults or writes banner-day
  state. The flag is rejected on
  `--send`/`--join`/`--discard`/`--discard-through`/`--seen-by`, which return
  no bodies. `--history <n> [--grep <regex>]` and `--since <id>`
  are cursorless; `--grep` is a case-insensitive Rust regex over body/subject/from/id.
  Opt-in `--max-bytes` runs after existing count/history/mention-rescue
  selection and admits a contiguous prefix of complete messages. `count` is
  the complete emitted count; `selected_count` is the pre-byte selection;
  count-window `skipped` remains distinct from `omitted.count`; `has_more`
  covers either remainder. The first byte omission reports bounded identity,
  raw body byte size, and how many omitted selected messages mention the
  acting room, so rescued mentions are never silently lost. A whale first
  yields metadata plus a usable slice command and no cursor movement, not a
  false empty inbox. Seen ids are built only after byte admission.
  Automatic text is quiet under both ordinary and byte-budgeted reads,
  including fenced/read-only operation. Old banner-day files are ignored and
  untouched. A no-budget `/dev/null` refusal still precedes rendering.
  acting-room id; sanitized room text is presentation only. This last rule
  intentionally corrects the pre-existing no-flag edge case where a currently
  valid format character could redirect the stamp to another room's path.
  `post chat <channel> --message <id> [--offset <b>] [--length <b>]
  --max-bytes <n>` is the channel analogue of the direct-mail slice and is
  always cursorless. It resolves the id within the named joined channel,
  parses the full stored record, and verifies any owner signature against the
  complete stored body before emitting `signed_verified`. Slice JSON states
  `verification_scope: stored_full_body`; the slice itself is not independently
  signed. `body_slice`, byte-range, progress, EOF, and minimum-scaffold rules
  match direct mail.
- `post chat <channel> --archive | --unarchive` — any bound participant,
  membership not required, idempotent (`changed: false` when already in that
  state). Writes only `channels/<name>/archive.json`
  (`{version: 1, archived: {through, at, by_participant, by_room} | null,
  log: [...]}`, log append-only); never writes history, never deletes. A
  channel is archived while `archived` is set and no conversational (non-event)
  message id exceeds `through`; a new post therefore resurrects it without a
  write, and join/profile events never do. Archive state is host-local: the
  sidecar is not bridged. JSON: `{ok, channel, archived, changed,
  archived_at?, archived_by?}`.
- `post channels [--archived | --all] [--text]` — read-only listing of channels, members, creation metadata,
  descriptions, and message counts: `{ok, channels, count, archived_hidden}`.
  Archived channels are omitted by default and counted in `archived_hidden`;
  `--archived` lists only them, `--all` lists both; each item carries
  `archived` and, when archived, `archived_at` and `archived_by`. An unreadable
  `archive.json` lists the channel as live and is a `doctor` finding. Each JSON channel
  item keeps `name`, `created`, `created_by`, `description?`, `members`, and
  `messages`, adds `participants` for host-local effective members, and reports
  the acting participant's workspace as `room` or `null`. `unread` is the exact
  participant eligibility count, or `null` when unbound or not an effective
  member. `messages` remains the raw message-file count. Listing is read-only
  and never creates cursor state.
- `post who [--room <name>]... [--text]`: read-only participant directory.
  JSON is `{ok, participant, participants, legacy_rooms, activity_note?, count,
  bridge_attention?, bridge_health?, doorbell?}`. `bridge_attention` is the number of items in
  `bridge/health.json`'s `attention` list and is present only when nonzero
  (`post doctor` lists each with its fix); `--text` prints
  `bridge_attention: <n>`. `bridge_health` (`{reason, fix}`) is present only
  on a bridged host (`bridge/config.json` exists) whose `bridge/health.json`
  is missing, malformed, or has no `attention` list, where a missing count
  means nothing; `--text` prints `bridge_health: <reason> Fix: <fix>`.
  `live_watch` is true for a fresh `post watch`
  heartbeat or an armed doorbell-supervisor subscription read from
  `doorbell/health.json` (rewritten at least every 30 s; a file older than 90 s
  belongs to a dead supervisor and counts for nothing). `doorbell_armed` on a
  participant or legacy-room row (present only when true) says which, and
  `doorbell` (`fresh`, `stale`, or `unreadable`; present only when the file
  exists) says whether the supervisor's word was counted, with a `doorbell:`
  line in text for the last two.
  The caller is first and carries binding provenance. Every participant row
  reports `id`, `harness`, lifecycle `state`, `last_seen` when present,
  optional lineage/workspace, watch state, and separate `unread` and `pending`
  address maps. A legacy row without `last_seen` reports `state: "no lease
  record"` and is stale for recipient selection. `legacy_rooms` retains old
  room heartbeat rows. `--room` scopes the report to the participants bound to
  the selected rooms; omitting it is the whole host, which includes
  session-only participants (no workspace) whose only address is their own id.
  It never reports PIDs or process information.
- `post schema` — the full machine contract: commands, flags, output shapes,
  error codes, exit codes, laws.
- `post doctor [--fix] [--brief] [--severity warn|error]` — validates root exists, rooms.json/rules.json parse
  and have sane shapes, room paths exist (warn), stray non-.mail files,
  malformed envelopes, and channel state including malformed channel metadata,
  membership, and messages. For each registered room it also checks
  `cursors.json` and `.cursors.lock` without repairing them:
  `cursor_state.<room>.invalid` is a warning for malformed, wrong-version,
  invalid-id, unsafe, or non-regular cursor state; `cursor_lock.<room>.invalid`
  is a warning for a missing lock alongside a cursor or a non-solitary/non-0600
  lock; and `cursor_state.<room>.legacy` is an info check when a valid legacy
  `channel-state.json` remains as read-only history. Participant cursors never
  import or materialize legacy room cursor state. Once `cursors.json` exists, an
  invalid legacy file is inert rollback evidence and the existing
  `channel_state.<room>.invalid` check is downgraded from error to warning.
  Suggested fixes direct an operator to inspect or repair cursor state by hand.
  Participant cursor state gets the same treatment through
  `participant.<id>.cursors_unusable` (warning): the file is diagnosed, never
  discarded or repaired, and its suggested fix says so -- preserving the ids is
  the repair, because the seen-set is the only record of what that participant
  has read.
  `--fix` creates the missing root, `archive/`, and default config files only
  — never touches rules content, mail, channel history, membership, cursor
  state, or cursor locks, and never creates a room's `inbox/` or `read/`
  (they appear with the room's first mail, so their absence is neither
  reported nor repaired). Expired participants are one info check,
  `participants.stale`, with a count, the delete and archive counts `post
  participant gc` would act on, and a fix naming its dry run and `--apply`.
  Each item in `bridge/health.json`'s `attention` list is a warning
  `bridge.attention.<kind>[.<id>]` carrying the bridge's own fix. On a bridged
  host (`bridge/config.json` exists) a health file that is missing,
  unreadable, malformed, or has no `attention` list is the warning
  `bridge.health_unreadable` with a fix, because a bridge that cannot report
  is not one with nothing to report; an unbridged host is silent. A served skill at `~/.agents/skill-library/post`
  that differs from the copy this binary was built with is the warning
  `skill.drift` (absent path: nothing). `--severity warn` drops info checks and
  `--severity error` lists errors only, but `ok`, `status`, `count`, and the
  exit code always cover every check: a store with a warning and no error
  exits 1 under `--severity error` with an empty `checks` list.
  `severity_filter` names the threshold and `filtered_out` counts the findings
  it hid (`count` = findings listed + `filtered_out`); `--brief` says
  `N findings, M of them hidden by --severity error`. Doctor
  also reports delivered mail with a missing or mismatched archive copy for
  manual reconciliation, and a mail id present in both `inbox/` and `read/`:
  identical content is `state.read_duplicate` (warning — an interrupted
  consume left the inbox copy behind; remove it by hand), differing content is
  `state.read_duplicate_mismatch` (error — reconcile by hand, delete nothing).
  `rooms.json` may be an empty JSON object on a fresh
  mailbox: doctor reports it as an info-only `config.rooms_empty` check with a
  `post rooms add` suggestion and exits 0, so `doctor --fix && doctor` succeeds
  under `set -e` before the first room is registered. Malformed, non-object, or
  non-string registries remain `config.rooms_invalid` errors. `--brief` prints
  exactly one human-readable summary line, conflicts with `--json`, and
  preserves the doctor exit dictionary: 0 healthy / 1 findings / 3 fix-failed.
  Fail-closed channel checks that name an unreadable participant's
  `participant.json` or `channels.json` require restoring or repairing that
  file from a backup, then retrying. A re-bind can recreate only a missing
  deterministic participant record; it does not repair either malformed file.
  Deleting the participant record or directory is not a repair; retirement is
  a separate operator decision after copying the state aside.
- `post watch [--room <name>]... [--once | --snapshot [--limit <n>]] [--from now]
  [--interval-ms <ms>] [--digest] [--text]` — the
  doorbell: blocks and streams one event per arriving direct mail or joined
  channel message so any harness monitor becomes a notifier. Long-running and
  `--once` forms require a participant binding; only snapshot remains available
  unbound. Room resolution as
  `inbox` (unregistered explicit rooms are accepted with a one-line stderr
  warning, since a silent watch on a typo'd name never rings). Scans of the
  inbox directory and joined channel messages are the truth source; a native
  filesystem watcher (inotify on Linux, FSEvents on macOS) supplies wake hints
  that trigger a scan early, and a slow periodic pass re-registers replaced
  directories and rescans regardless of hints. Overflow marks every watched
  directory for a rescan; a failed re-watch remains pending for the next slow
  pass. That pass uses a wall-clock deadline checked after every wake, so
  continuous events for one target cannot starve another. `--interval-ms` (default
  1000ms, clamped 100–60000) bounds the poll cadence that remains the fallback
  when no native watcher is available or it fails mid-run. The exclusive-link
  delivery commit means a listing never sees a partial direct-mail file, and
  the first batch emits the current unread backlog plus channel messages not
  in the participant's seen-set, so there is no start-vs-arrival loss window. `--from now`
  (which conflicts with `--snapshot`) instead performs one discarded startup
  scan per target to seed only process-local suppression state; it emits nothing
  from that backlog, and only messages arriving after that scan ring. Omitting
  `--from` preserves the backlog-replay behavior above. Emits envelope metadata
  and, for readable messages, one sanitized body preview; it never emits a full
  body or consumes state. Consumption and its framing banner stay exclusively
  with `post read`, `post chat`, or `post catchup`. Default output is one NDJSON
  object per line. Every event carries `address: {kind, name}`; `room` appears
  only when `address.kind == workspace`. Mail events add `id`, `from`, `origin`,
  optional `reply_to_participant`, `reply_to_shared`, optional `pending: true`,
  `kind`, `subject`, `sent`, `reason: mail`, and optional `preview`. Unreadable
  events add `id`, `reason: mail|channel`, and `channel` when the failure is a
  channel message. Channel events add `channel`, `id`, `from`, `origin`, the
  reply targets, `subject`, `sent`, `reason: channel|mention`, and optional
  `preview`. An event projected from a participant whose cursor state exists but
  cannot be read adds `cursor_unusable: true` (absent otherwise, so a readable
  participant's event is byte-identical to the pre-marker form); in `--text`
  mode such an event line is prefixed `[cursor unusable: re-reporting history]`,
  because the projection then reports consumed history as unread rather than
  fresh traffic. A bound participant suppresses only channel messages whose
  `from_participant` is itself; `--own` remains a legacy unbound-snapshot
  control.
  Known bridge transport evidence or a bridged `from` workspace sets
  `origin: remote` before local participant lookup. A coincident local record
  never produces `reply_to_participant`; remote origin exposes the shared reply
  only.
  With `--digest`, each batch instead emits one object per `(address, source)`
  group, ordered by the first underlying event:
  `{"event":"digest", address, room?, source, count, first_id, last_id, from,
  reason, pending?, preview?, cursor_unusable?}`. `pending: true` is present
  when the group is provisional; `cursor_unusable: true` is present when the
  group was projected from unusable participant cursor state, and the group's
  text says `re-reported cursor unusable` instead of `new`. A digest aggregate
  has no origin or reply target.
  `source` is `mail` or `channel:<name>`; `from` de-duplicates senders in
  arrival order and caps them at five followed by `"+N more"`; `reason` is the
  shared per-event reason or `mixed`. Readable ring lines carry a trailing
  sanitized single-line preview: the first 80 Unicode scalar values, followed
  by `…` when truncated. Newlines and tabs flatten, all other control
  characters are stripped, and ASCII `[`/`]` become full-width brackets. A
  digest uses the most recent readable preview in that batch. Its text is
  `#<channel>: N new (<sender> ×<count>, ...)  <preview> [<first_id>..<last_id>]
  [--since <fencepost>]` for channels, or `mail: N new (...)  <preview>
  [<first_id>..<last_id>]` for direct mail. The preview comes before the bounds
  and copyable suffix, so the true fencepost remains the rightmost parseable
  group. Unreadable events have no preview and retain their debug-quoted id;
  NDJSON omits `preview` when no readable body exists. The channel fencepost is
  strictly below `first_id` because `post chat --since <id>` returns ids strictly
  greater than its bound; thus the copyable suffix includes the whole digest at
  emission time. Sender counts are omitted when all are one and the
  parenthesized list is omitted when no sender parsed.
  Each bound long-running poll touches
  `participants/<id>/watch.heartbeat`; long-running watch cannot run unbound.
  `post who` reports participant heartbeat state and legacy room rows without
  PIDs. Snapshot mode never writes heartbeats or refreshes `last_seen`.
  A watch is live when the stamp is not in the future and age is at most
  `interval*2 + slack` (legacy single-number stamps assume a 1000ms interval).
  `--text` mirrors inbox/channel line formats with the full message id, subject,
  sender, channel, and unreadable id all debug-escaped or sanitized as applicable
  — the attacker-reachable fields
  (crafted subjects and `from` in hand-written mail/messages; filenames, which
  no envelope validation ever touches) cannot forge an event line, and stderr
  warnings debug-quote both path and message for the same reason. `--once`
  exits 0 after the first non-empty batch. Stdout is flushed per batch.
  Transient scan failures (mailbox removed or recreated mid-watch, permission
  blips) degrade to an empty scan with one stderr warning per outage and
  polling continues; corrupt or unreadable channel stores warn on stderr and do
  not suppress healthy joined channels. A dead event backend falls back to
  polling. A same-generation fence or transient admission error keeps the scan
  read-only, pauses routing and lease/heartbeat refresh, warns once per episode,
  and retries. Only a missing enrolled state file or generation mismatch is a
  fatal admission error (`config_invalid`, exit 78); stdout failure is also
  fatal (`io_error`, exit 75).
  Caveat: all watch warnings (unregistered room, scan outages, channel-store
  diagnostics) are stderr-only; a consumer that captures just stdout will not
  see them. Never moves, alters, or deletes mail; keeps notification seen-state
  only in process memory; never mutates channel seen-sets. An id consumed by a
  read stays suppressed after a watcher restart; an unconsumed id may ring
  again. Adapter-level per-participant notification dedupe across hook
  invocations is the adapter's responsibility; Post has no durable watcher-
  notification store. The heartbeat is presence state, not delivery state.
  Known accepted window: direct mail arriving AND
  consumed by a concurrent reader within one interval is never emitted because
  it was never observed unread. Channel messages are append-only; watch holds
  its startup seen-set snapshot in memory (read-only), so a channel read
  during the same watch does not erase a later notification. `--snapshot` (conflicts with `--once`;
  `--interval-ms` has no effect) is the nonblocking poll for bounded lifecycle
  hooks: it performs exactly one scan of unread or provisionally eligible mail
  plus joined-channel messages outside the participant's seen-set, then exits
  0. An empty scan
  emits nothing; a non-empty scan emits the ordinary NDJSON/text batch. A
  direct-mail scan failure is a nonzero error envelope — never a false empty —
  while per-channel failures keep the watch posture (stderr warning, healthy
  channels still ring). Bound targets come from the participant binding,
  regardless of cwd. Only an unbound legacy workspace preview resolves
  `--room` or cwd; an unregistered cwd then warns on stderr, scans nothing,
  creates no mailbox directories, and exits 0. Snapshot mode shares every
  other watch invariant: envelope metadata plus sanitized previews only, no
  full body, mail movement, or cursor writes. Snapshot-only `--limit <n>` admits the last `n` underlying events in scan order
  and warns on stderr when earlier events are omitted; `--limit 0` is unlimited.
  Optional digest grouping happens after that limit. The flag affects emission
  only — omitted events remain unread — and omitting it preserves the unbounded
  snapshot behavior.

## Read-layer notes (amendment, 2026-09-01)

- `catchup` is the complete-slice writer; `search`, `inbox`, `channels`,
  `schema`, `version`, `doctor` without `--fix`, participant and identity
  listings, and `watch --snapshot` are read-only. Display-only forms compute
  provisional eligibility for unrouted mail and label or count it as pending;
  they publish no routing receipt and change no cursor, lease, or affiliation.
  An absent cursor, lock, room, participant, or banner-day file is never created
  by those commands. Watch captures its participant channel floor at startup
  and never writes cursor state.
- Channel unread counts use the participant's exact seen set and effective
  membership. Inbox `unread_count` includes only receipt-backed messages whose
  frozen recipient set contains that participant and whose id is not in its
  address seen set. Workspace and lineage mail from that participant is
  excluded; explicit `participant:<self>` mail starts unread to self and stops
  counting after an ordinary consuming read. Pending is counted separately.
  Malformed or unreadable files remain excluded and are
  reported through existing warnings.
- Search is a linear scan of visible history plus party-visible mail. The
  default result cap is 100 and the hard cap is 1000; matching is literal,
  case-insensitive Unicode substring over body, subject, sender id, and message
  id. Resolve the acting participant and apply receipt visibility and effective
  channel-membership filters before opening message content. Bounded sanitized previews keep
  search from flooding an agent context; regex and indexing are out of scope.
- Framing is part of the trust boundary for body-bearing surfaces. Catchup and
  search print one banner per non-empty text invocation above all sections or
  results only when explicitly requested. All body-bearing commands use quiet
  auto output; full/compact flags opt into recurring banners. Legacy
  `POST_FRAMING=compact` maps to auto, so existing sessions adopt quiet output
  at binary cutover without an environment restart. JSON keeps source/authority
  metadata and omits laws in auto. No surface offers a separate `none` mode.

## Participant lifecycle and read-only projection (amendment, 2026-09-16)

- A participant is active iff `ended_at` is absent and `last_seen` falls within
  its recorded `lease_hours`. A new bind records
  `POST_PARTICIPANT_LEASE_HOURS`, or 24 hours when it is unset. Later binds,
  touches, and writer renewals refresh `last_seen` while preserving the
  recorded lease unless the variable is explicitly set, in which case they
  re-apply it. The variable applies only to the acting participant; `end`
  never consults it. A record with no `last_seen` is stale until bind or touch.
- Adapters call `post participant touch` during supported prompt/tool events.
  Only the shipped Claude adapter registers `post participant end`, on
  SessionEnd; the shipped Codex, Cursor, and Grok adapters register no end
  hook. End is idempotent. A later bind clears `ended_at`, reactivates the same
  id, and preserves its historical affiliation.
- Routing selects from the active set. Workspace fan-out uses active
  participants bound there; lineage fan-out uses active affiliates. `post who`
  lists every participant and labels each activity state.
  Lineage affiliation survives stale and ended states and is cleared only by
  explicit leave. `post identity show <name>` lists those historical affiliates
  and gives each an `active` flag.
- Activity affects new recipient selection only. A frozen recipient keeps read
  access after its lease expires. Mail frozen to an abandoned session during
  its remaining lease is not reassigned at expiry; only later sends use the new
  active set. Participant-targeted mail remains durable regardless of state.

## Identity states (amendment, 2026-09-28)

- A session is bound, unbound, or claims a record that does not exist. These
  are three different answers, and no command guesses a room from the working
  directory for any of them.
- **Missing claim.** An explicit claim (`POST_PARTICIPANT`, or a harness
  session whose by-session index names a record that is gone) that resolves to
  no record is the error `participant_missing`, exit 65, never "unbound" and
  never an empty inbox. `details` carries `id`, `input` and `exact_fix`
  (`unset POST_PARTICIPANT && post participant bind` when the session has a
  harness key, else `post participant bind --new`; `post participant bind` for a
  dangling session index). Every command that reads or writes as a participant
  fails with it. The commands that diagnose or repair identity (`participant
  show`, `who`, `doctor`, and the read-only listings) exit 0 and carry it as
  fields instead: `bound: false` and `participant_missing: {claim=
  POST_PARTICIPANT|session-index, id?, message, suggested_fix, exact_fix?}`.
  `who` and `doctor` also report their `participant.status` as `missing` with
  the repair in `fix`; doctor's status, count and exit code are unchanged (a
  missing claim is a diagnosis, not a store fault), and `doctor --brief` names
  it on its one line.
- **Unbound reader.** With no claim at all (no `POST_PARTICIPANT`, and a harness
  key with no record or no key), readers exit 0 with an explicit marker on
  stdout. Readers that would otherwise guess a room (`chat` without a send or
  join, `search`, `read --peek`, `profile` without a target) print JSON
  `{"ok":true,"participant":null,"bound":false,"hint":"..."}` or one line of
  text. Listings (`inbox`, `channels`, `who`, `doctor`, `rooms`, `owner`,
  `participant list`, `schema`) keep their own shape and add `bound: false` and
  `hint` (with `participant: null` unless they report a participant object of
  their own). `watch --snapshot` with no `--room` prints one NDJSON line
  `{"event":"unbound","participant":null,"bound":false,"hint":"..."}` (text
  mode: the hint as prose). An explicit `--room` on `inbox` and `watch
  --snapshot` still runs.
- **Lazy bind.** A write (`send`, `chat --send`, `chat --join`, a consuming
  `read`, `catchup`, `inbox --adopt`) run with a harness conversation key and no
  record binds the session first, exactly as `participant bind --harness <h>
  --key <key>` would (same deterministic id), then proceeds. Its JSON receipt
  carries `bound_now: {id, workspace}` and a text receipt ends with `post:
  bound this session as participant <id>`. Without a key the write fails
  `no_participant` with the bind command in its fix.
- **`participant show`.** `status` is `bound` (a record answers), `unbound` (no
  claim; `bound: false`, `fix`), `missing` (as above, with `participant_error`
  and `participant_missing`), or `archived` (`participant show --harness <h>
  --key <key>` only: `participant gc` moved the record aside, and `post
  participant bind` restores it).
- **Ephemeral records.** A `participant bind --new` record carries `lease_hours:
  1` and `"ephemeral": true` (absent on every other record), so it ages out
  within the hour and `participant gc` collects it after 24 hours of idleness
  instead of 7 days.
- **`participant gc`** is a dry run unless `--apply`, from one plan, so the dry
  run and the apply list the same ids. Output: `{"ok":true,"applied":bool,
  "deleted":[ids],"archived":[ids],"kept":{reason:count}}`. Tier 1 deletes a
  record that is not active, was last seen more than 7 days ago (24 hours when
  ephemeral), holds only scaffolding, and that nothing names; a tombstone line
  in `participants/archived.jsonl` keeps the id occupied for its key. Tier 2
  moves a record last seen more than 30 days ago that has state but no unread,
  pending or held mail to `participants-archive/<id>/`; `bind` restores it.
  Never collected: an active lease, a fresh watch heartbeat, a lineage's holder,
  a doorbell subscription, an in-flight outbound sender, or anyone with unread,
  pending or held mail (frozen unread mail is retained, never rerouted).
  `post doctor`'s `participants.stale` quotes the same plan's delete and archive
  counts and names `post participant gc` (dry run) and `--apply`.

## Lineage voice withdrawal (amendment, 2026-09-16)

- Unqualified `post identity voice withdraw` first selects the acting
  participant's current lineage when it contains that participant's voice or
  gap. A pending gap finishes cleanup. A settled gap returns `changed: false`
  with a `--lineage <name>` hint and never falls through to another lineage.
- Cross-lineage fallback runs only when the current lineage has neither the
  caller's voice nor its gap. One current or cleanup-pending candidate is
  selected; when none exists, one settled gap may return its no-op result.
  Multiple candidates refuse with `details.matches` and one command per
  candidate in `suggested_fix`; ambiguity supplies no `exact_fix` and changes
  nothing.
- `post identity voice withdraw --lineage <name>` targets the caller's voice
  without rejoining. It deliberately uses the named lineage directory even
  when `lineage.json` is damaged. A pending gap is treated as withdrawn and a
  retry completes cleanup.

## Profiles (amendment, 2026-08-05; re-keyed 2026-09-22)

- `post profile set [--name <name>] [--pfp <emoji>]` / `show [room|participant:<id>|<id>]`
  / `clear` — a display name + emoji sigil that belongs to ONE participant,
  stored in root `profiles.json` (reserved as a room name) under the rooms
  lock, keyed `participant:<id>`. `set` and `clear` act on the acting
  participant only. Bare (workspace-keyed) entries are the pre-2026-09-22
  format: they were shared by every participant bound to the workspace, which
  let a newly bound participant inherit a peer's persona (the 2026-09-22
  incident). Bare entries NEVER stamp; `show <room>` still displays one with
  `legacy: true`; `post doctor` reports each one and never migrates it (a
  malformed participant record would make "sole participant" a guess); a `set`
  by a participant bound to that workspace — or, for a pre-change session-only
  participant, one whose bare id was the key — retires the bare entry
  (`retired_legacy_entry` in the result). A session-only participant (no
  workspace) stamps its own entry. Profile-change announcements go to the
  acting participant's effective channel memberships.
- PRESENTATION ONLY: no profile value may influence identity, auth, routing,
  blocked routes, cursors, room resolution, or signed-owner verification. The
  immutable `(room-id)` suffix is a HARD INVARIANT of every render path that
  shows a display name (chat banners, read, inbox --text, watch --text);
  no future renderer may drop or truncate it. Residual non-NFKC homoglyph
  imitation risk is accepted BECAUSE of this invariant.
- Validation: name <=32 chars, trimmed, refuses the shared character predicate
  (Cc + bidi controls incl. U+061C + U+2028/U+2029), NFKC-skeleton imitation
  check against `trey` and all room ids; pfp is exactly one grapheme cluster,
  non-ASCII, unique across participants and registered legacy rooms. The same
  predicate is enforced at set time, at envelope parse time (mail and
  channel), and in text sanitization, and registry values are re-validated at
  stamp time — only the acting participant's own entry ever stamps.
- Stamping: `display_name`/`pfp` are optional envelope fields written at send
  time (absent-when-unset keeps pre-profile JSON/NDJSON byte-identical, and
  absent-profile text output stays byte-identical). History renders as-sent;
  renames never rewrite stored messages. Text bylines render the stamped
  profile (`🔥 Name [participant] (room)`), else the lineage
  (`lineage [participant] (room)`), else the reply address with the
  participant (`room [participant]`; legacy mail with no participant stays the
  bare `room`, byte-identical to before). The `[participant]` id is never
  dropped when stamped. Old workspace-stamped envelopes therefore render
  their stamped name with the participant id rather than lineage-first; the
  stored bytes are untouched. A name, pfp,
  or clear change emits a `profile` event message naming the participant in
  each of the room's channels; the channel list is
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
  `inferred-basename` | `participant-binding`. `participant-binding` means the
  bound participant supplied the shared reply address without a `--from` flag
  or `POST_FROM` assertion. Both fields are transport evidence; neither
  affects routing, blocks, cursors, membership, profiles, or signed-message
  verification. Old mail and old stores keep reading; old binaries ignore the
  new fields.
- Initial workspace selection at bind time is explicit `--workspace` > `POST_FROM`
  pin > the registered workspace containing cwd > no workspace; rebinding an
  existing participant preserves its stored workspace unless `--workspace` or
  `POST_FROM` explicitly updates it (cwd alone never moves a bound participant).
  An unregistered cwd never becomes a basename workspace. None of these selects the actor. Legacy
  unbound read-only forms resolve only their documented `--room`/cwd context. A
  bound participant supplies the sender and channel identity; an explicit
  `--from` must agree with its reply address. Session-only participants may join
  channels without a registered workspace.
- A set-but-invalid `POST_FROM` or `POST_SENDER_ADDRESS` is a loud error,
  never a silent fallback to inference. The pin's grammar is `--from`'s;
  the address must be ≤256 bytes with no control or whitespace characters.
- The environment is set by the `launcher/agent-session` helper (or a
  per-harness shim in `launcher/shims/`): pin resolved once at launch
  (explicit `--room`, else the registered room containing the launch cwd,
  realpath-safe), fresh 128-bit UUID per launch, address
  `<harness>.<repo-key>.<uuid>` where repo-key is
  `<repo-slug>-<8-hex path hash>`. No match means no workspace pin; bind may
  infer context from cwd. The launcher never binds or exports
  `POST_PARTICIPANT`; hooks bind from their conversation key or event payload.
  Stale inherited workspace pins are cleared on every launch, and
  `agent-session --doctor` checks that seam.
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

- **Before the participant redesign, room-self mail was opt-in.** Participants
  and lineages supersede that rule. Workspace and lineage fan-out exclude the
  sending participant; an explicit participant self-target is routable and
  readable.
- **Pin/flag conflict is a hard error.** `--from` that DISAGREES with a
  POST_FROM pin refuses loudly (a prepared command carrying `--from` inside a
  pinned session is exactly the ambiguity the identity layer eliminates). An
  agreeing `--from` proceeds as `declared-flag`.

## Codex room convention

Codex may use a narrow registered workspace path, normally
`~/.codex/post-room`, registered as `codex`, to supply a shared reply address
and legacy channel membership defaults. The bound participant remains the
actor even when later commands run elsewhere. A session-only participant may
join channels explicitly without a workspace. Registering all of `~/.codex` is
avoided so ordinary config or skill work does not become workspace context.

## Error contract

Envelope on stderr: `{ok: false, error: {code, message, details, retryable,
suggested_fix}}`. Codes (stable): `unknown_room`, `blocked_route`,
`reserved_sender`, `empty_body`, `ambiguous_id`, `not_found`,
`invalid_argument`, `config_invalid`, `duplicate_workspace`, `io_error`,
`delivered_output_failure`, `delivered_unarchived`, `not_a_member`,
`crossed_send` (reserved: no command produces it now that a crossed channel send
always delivers).
Pre-commit `io_error` is retryable with exit 75. `duplicate_workspace` and
`not_a_member` are non-retryable with exit 65. Both delivered variants are
non-retryable with exit 70: `delivered_output_failure` means a channel
mutation committed but stdout receipt failed (a direct `post send` that landed
exits 0 instead, with a stderr note); `delivered_unarchived`
means inbox delivery committed but archive publication failed. Room
registration stdout failure after commit is reported as success with best-effort
diagnostics, not `delivered_output_failure`. Exit codes per the agent-CLI
standard: 2 usage, 65 validation (unknown_room, reserved_sender, empty_body,
ambiguous_id, duplicate_workspace, not_a_member), 66 not_found, 77
blocked_route (permission class), 78 config_invalid, 70 post-commit/internal
failure, 75 retryable pre-commit I/O.

## Quality gate

`cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D
warnings`, `cargo test --all-targets --all-features`, `cargo build --release`,
the Node hook and launcher suites, and a release-binary schema smoke. Tests must
cover: full send/inbox/read roundtrip against a temp `POST_MAIL_ROOT`; the
armed-route refusal quoting the reason; participant binding, typed targets,
and sender provenance; banner/framing present in BOTH text and json
read output; prefix matching incl. ambiguity; empty-inbox exit 0; atomic
write behavior (no partial .mail on simulated failure); envelope
deserialization of every output shape; migration: a mail file in the original
on-disk format reads back identically; channel join/send/read with cursor
advancement and `--peek`; channel watch backlog/live events without full bodies
or cursor advancement; malformed channel isolation; blocked-route channel sharing
refusal; `not_a_member`; and schema/help consistency for `participant`,
  `identity`, `send`, `chat`, `channels`, `inbox`, `read`, `catchup`, `search`,
  `rooms`, `profile`, `owner`, `schema`, `doctor`, `watch`, `who`, and `version`,
  every watch event variant, catchup/search schema-vs-reality, and the cursor
  diagnostics that `doctor --fix` leaves untouched.

## Stack

Rust 2021+, clap 4 derive, serde/serde_json, thiserror or anyhow at the
edge; keep dependencies minimal (no tokio — everything is local sync I/O).
Layout per the rust-agent-cli skill: src/main.rs, src/cli.rs, src/commands/,
src/output.rs, src/error.rs, src/lib.rs, tests/cli.rs.
