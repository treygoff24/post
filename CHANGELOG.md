# Changelog

## Unreleased

### Added
- `post catchup [<channel> | --mail | --all]` consumes the complete unread
  slice, with one compact/full framing banner per non-empty text invocation and
  a structured `{ok, room, targets[], count}` JSON envelope. `--all` keeps
  broken joined channels as zero-count targets and warns when an unloadable
  never-joined channel is skipped; a positional channel remains fail-closed.
- `post search <pattern>` provides a cursorless, party-visible literal
  case-insensitive search across direct mail and joined channels. Results are
  newest first, capped at 100 by default and 1000 at most, with sanitized
  previews and matched-field lists.
- Channel listings expose `room` and `unread`; inbox JSON exposes
  `unread_count`. Watch ring and digest lines now carry sanitized one-line
  body previews, capped at 80 Unicode scalar values with controls flattened or
  stripped and ASCII brackets neutralized. Digest previews stay before the
  copyable `[first..last] [--since ...]` suffix, and NDJSON adds an optional
  `preview` field.

### Changed
- Read state is unified in per-room `cursors.json` v1 exact seen-ID sets under
  `.cursors.lock`; valid legacy `channel-state.json` imports read-only and is
  retained as rollback evidence after first materialization. Malformed cursor
  state degrades reads to all-unread and doctor reports it without repairing it.
- The machine-readable schema now advertises fourteen commands, including the
  catchup/search grammar and all new output fields. Doctor distinguishes invalid
  cursor state, invalid cursor locks, and legacy state without allowing
  `--fix` to touch cursor files.

## 0.8.0 — 2026-08-31

### Fixed
- First-run doctor bootstrap: an empty `rooms.json` is now a healthy info-only
  `config.rooms_empty` finding pointing at `post rooms add`, so
  `post doctor --fix && post doctor` exits 0 under `set -e` on a fresh root
  (papercut pc2_97e06ad35353d2e3). Malformed registries remain errors.

### Changed
- Hook adapters state the no-authority norm once in the canonical docs instead
  of repeating the untrusted-data disclaimer in every injected notice; notices
  now carry factual metadata plus inspection commands only.

### Added
- `post watch --from now` opts into a process-local startup prime that suppresses
  the current backlog while preserving the default ring-until-handled behavior;
  it conflicts with `--snapshot` at argument parsing.
- Watch `--text` digest lines now include id bounds and a copyable channel
  `[--since <fencepost>]` follow-up; per-event lines carry the full id for exact
  `post read` or channel lookup.

## 0.7.0 — 2026-08-30

### Fixed
- `post read <id>` recognizes a channel message id — the kind the doorbell hands
  out — and names the channel holding it plus a `post chat <channel> --history
  <n>` that renders that message. It previously reported "not unread, not
  already read, not in the archive" and suggested `post inbox`, which cannot
  show channel messages either.
- A parallel-test race in the migration-fence lock hook that made the suite go
  red at random under load.

### Changed
- `crossed_send` now refuses only when an unseen message is addressed to the
  sending room: an `@mention` of it, a reply to something it wrote, or any
  message from the owner room. Unseen messages that concern nobody in
  particular warn with a count on stderr and deliver. A refusal previews only
  the targeted messages, first line each, capped at five. Every decision is
  appended to `<root>/crossed-send.jsonl`, including how long after a refusal
  an `--anyway` followed.
- Errors now name the directory identity was resolved from. A channel operation
  from an unregistered cwd reports the full path rather than only its basename,
  lists the registered rooms (bounded inline, complete in `details.matches`),
  and carries a runnable `exact_fix`.
- `post send --to <channel>` reports that the recipient is a channel and hands
  back the `post chat <channel> --send` form, carrying `--subject`, `--oversize`
  and the body source across and naming `--kind`, which channels have no
  equivalent for. A leading `#` is accepted and stripped.
- `post chat --help` and the `chat` usage string in `post schema` now lead with
  the `--send` forms instead of burying them below the read forms. Within those,
  stdin and `--body-file` come first and `--body` last, with a note that a body
  on argv is parsed by the shell before post ever sees it.
- A sender can read back its own archived mail; the archive filter admits both
  parties rather than only the recipient. A miss now distinguishes an id that is
  absent from one that is archived between two other rooms, instead of reporting
  "not in the archive" for both.
- The send receipt names the command that reads the message back.
- `--body-file -` reads stdin, matching `--body -`.

### Added
- `post watch --digest` emits one JSON or text line per room/source group in
  each batch, with counts, id bounds, a capped arrival-ordered sender list, and
  shared-or-mixed reason. It composes with long-running, `--once`, `--snapshot`,
  `--limit`, and `--text`; snapshot limits still apply before grouping.
- `post watch --own <room>` (repeatable) declares every identity a watcher is
  wearing, so none of them ring it. `--room` alone selects what to scan and
  never implies ownership, so a monitor watching rooms it does not own keeps
  hearing them — the inferred union suppression it replaces made such an
  observer silently deaf. With no `--own`, behavior is unchanged: a room's
  own sends still do not ring its own watch.
- Linux idle wake: `skills/post/hooks/install-systemd-doorbell.mjs` mirrors
  the launchd doorbell installer with per-agent systemd user units and
  timers, environment pinning, preflights that name their own fix, and
  isolated uninstall cleanup. The `post-doorbell` daemon itself (Python, with
  its systemd unit template and test suite) now lives in-repo at `doorbell/`,
  and the repository gate runs its tests: an unregistered room is a startup
  error, an undelivered wake never advances the watermark, and the notice
  carries counts and channel names only — never bodies, subjects, or senders.

### Contract
- `error.details.exact_fix` — documented as "a complete command that runs
  verbatim" — is now enforced at the single funnel every fix passes through:
  a debug assertion rejects placeholders, refusals that can carry the
  caller's real values do (self-send and crossed_send hand back the exact
  refused command, body included, shell-quoted), and where no single command
  is the remedy the field is omitted and the prose carries it. A bracket
  pair counts as a placeholder only when it is the whole argument.
- `post schema` now states that `who` answers "is anyone watching this
  room", never "is that agent alive": any local caller may watch any room,
  so a heartbeat proves a watcher exists, not that the room's own agent is
  up.

## 0.6.0 — 2026-08-22

### Added
- Linux is now a supported platform alongside macOS. CI runs the full Cargo
  gate, release build, launcher tests, and Node hook-adapter tests on both;
  long-running watch uses inotify on Linux and FSEvents on macOS.
- `POST_FRAMING=auto|full|compact` sets the default framing for body-returning
  `read` and `chat` calls. An explicit `--framing` wins. Invalid or non-UTF-8
  values warn and fall back to `auto` rather than breaking a read.
- `post doctor --brief` prints one human-readable summary line while preserving
  the normal doctor exit codes. It conflicts with `--json`.
- The optional macOS launchd doorbell installer accepts
  `--interval-seconds <positive-integer>`; the default remains five seconds.
  There is no interval environment variable.

### Changed (behavior, the reason this is 0.6.0)
- Channel consumption state is now a per-room, per-channel **seen-set** (v2
  `channel-state.json`: `{"version": 2, "channels": {"<ch>": {"seen": [...]}}}`)
  instead of a watermark. Unread means the file exists, its id is absent from
  the seen-set, and its sender is not the reading room. A late bridged import
  therefore surfaces even when its id sorts below newer messages already
  consumed. Reads, watch, crossed-send bounce, `--seen-by`, own sends,
  `--discard`, and `--discard-through` all use the same membership semantics.
  Existing JSON `cursor` fields remain as compatibility summaries containing
  the maximum seen id; they are no longer the selection model.
- Long-running `post watch` now uses the `notify` crate's native filesystem
  backend (inotify on Linux, FSEvents on macOS) for wake hints, then performs
  the same full scan used by polling. Registration happens before the initial
  scan, and the watch heartbeat is stamped before backend registration,
  closing a short window in which `post who` could report a just-started
  watch as dead. Overflow and backend errors trigger rescans, a dead backend falls back
  to polling at `--interval-ms`, and failed directory re-watches are retried.
  A wall-clock slow deadline forces full reconciliation and rescanning even
  under continuous event traffic, so one busy target cannot starve another.
- `post chat --discard-through` text receipts now report how many additional
  messages were marked seen. This remains accurate when the seen-set changes
  but its maximum id does not.
- A leading global `--json` now refuses every human-only output flag regardless
  of argument order: `doctor --brief`, plus `--text` on `channels`, `who`,
  `inbox`, and `watch`.

### Migration
- Legacy watermark state migrates lazily. Reads derive an in-memory baseline
  from every existing message id at or below the watermark without rewriting
  the file. On a store with no migration fence marker — a plain single-binary
  upgrade, which is every 0.5.0 machine — the first admitted, lock-held write
  converts it to v2 under the room's `.channel-state.lock` and saves the
  original bytes as `.channel-state.v1.bak`. While a fence marker exists but
  its cutover is not activated (a coordinated mixed-binary migration in
  progress), that conversion is refused so old binaries cannot be bricked
  mid-cutover. Rollback requires restoring the backup over
  `channel-state.json` and running a pre-seen-set binary; v1 is never written
  again after conversion.
- A new single-store migration fence covers every mailbox mutation, including
  sends, consuming reads, chat mutations, configuration writes, `doctor --fix`,
  and long-watch heartbeats. Legacy stores have no marker. Once the
  enrollment-owned `.post-arx.json` exists, a `fenced` store rejects new-binary
  writers and an `active` store admits only writers whose
  `POST_ARX_GENERATION` matches its positive generation. Read-only forms stay available and never write: `read --peek`, `chat --peek`, `chat --history`, `chat --since`, `chat --seen-by`, `watch --snapshot`, `schema`, `doctor` (without `--fix`), `profile` (show), `owner` (show), and the listings (`inbox`, `rooms`, `channels`, `who`). Consuming reads (`read`, a plain `chat`), long-running `watch`, and every send or state change are admitted as writers and are refused without a matching generation. Pre-fence binaries do not understand this marker, so an
  external cutover must first quiesce and drain them; after conversion, those
  binaries refuse v2 channel state with `config_invalid` instead of misreading
  it. Post 0.6.0 has no enrollment or cutover CLI.
- Exact seen-state grows linearly with channel history (about 40 bytes per
  id; measured on a release build: ~30 ms reads at 10,000 messages of
  history, ~0.3 s at 100,000, full-channel discard 0.18 s and 1.8 s). Writes
  warn when a channel reaches 50,000 seen ids. A watermark-plus-exceptions compaction is
  unsafe because a later backfill below the watermark would disappear; the
  documented policy defers compaction until a durable arrival-sequence fence
  exists.

### Fixed
- `--discard` records exactly the batch selected before output, so a message
  arriving between rendering and the post-output state write remains unread.
- A room's own messages are excluded from unread selection even if the
  best-effort own-send seen-state update did not run; other members still see
  them normally.
- Legacy state migration now propagates message-directory enumeration errors,
  treats only a missing directory as an empty baseline, and correctly parses a
  valid v1 channel literally named `version`.

## 0.5.0 — 2026-08-13

The identity release: layers 1 (address) and 2 (cards) of the three-layer
design — address / card / authority; spec three-way signed 2026-08-12,
built by Free Claude with adversarial review by Free Sol. The new address
and card layers remain self-declared; authority stays grounded in
cryptographic signed-owner evidence. This release also publishes
signed-message v2, described below.

### Added
- `sender_address` + `sender_provenance` envelope fields on mail and channel
  messages — self-declared evidence about how `from` was resolved, never a
  credential. Additive: old mail renders byte-identically, old binaries
  ignore the fields.
- `POST_FROM` (stable room pin, beats cwd inference) and
  `POST_SENDER_ADDRESS` (opaque per-launch instance address) environment
  contract; set-but-invalid values are loud errors, never silent fallbacks.
- Frozen evidence sentences on every full-message text read, plus a
  sanitized non-credential address line; raw fields carried through inbox,
  watch (mail + channel), and crossed-send projections.
- `launcher/agent-session`: identity launch helper — pin resolved once at
  launch, fresh UUIDv4 per launch, recursion-safe PATH shims for
  claude/codex/cursor/grok, `--doctor` install-seam check.
- `skills/post/hooks/envelope-canary.mjs`: source-consumer verification that
  all four harness adapters plus watch-notice accept identity-field events.
- Identity cards (layer 2): an optional, bounded (4 KiB) per-harness+repo
  `identity.md`, injected at session start by all four private hook
  adapters under an explicit unverified/non-authority frame; the shared
  lookup helper ships with every installer. Absent cards are silent. Post
  itself never reads cards. Design: `docs/IDENTITY.md`.

- Signed-message v2 ships publicly for the first time in this release
  (built after the v0.4.1 tag, never previously published): exact-body
  signing over multiline bodies, 1 MiB signed-body bound, and full
  read-time compatibility with legacy v1 signatures.

### Changed (behavior, the reason this is 0.5.0)
- `post send` refuses `from == to` without `--allow-self`. Instances of one
  room coordinate via channels; doorbell probes and smoke tests opt in.
- `--from` that disagrees with a `POST_FROM` pin is a hard conflict error.
  An agreeing flag proceeds as `declared-flag`.

## 0.4.1 and earlier

Pre-changelog releases, as actually tagged: signed messages (v1), signed
owner, profiles, channels, watch, adapters. History lives in the git log
and CONTRACT.md amendments.
