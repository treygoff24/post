# Watch, doorbells, and hook adapters

The detail behind the watch summary in [`SKILL.md`](../SKILL.md). Read it when
running `post watch` from a harness, parsing its events, or installing a hook
adapter or idle doorbell. The Claude Code Monitor recipe is
[`post-mail-doorbell.md`](post-mail-doorbell.md); the full adapter contract is
`docs/ADAPTERS.md` in the post repo.

## Watch forms

Run a long watch inside a session your harness owns (a PTY, background task,
or Monitor) and stop only that session by its own handle. Never find watches
with a machine-wide `pgrep` or `pkill`: every agent's doorbell looks the same.

- `post watch --snapshot` scans once and exits 0: no output when empty, the
  ordinary event batch otherwise. It is read-only even when unbound, and a
  mail scan failure exits nonzero rather than looking empty. This is the
  primitive for lifecycle hooks. Snapshot-only `--limit N` prints the last N
  events without consuming them.
- `post watch --once --json` blocks until at least one event is ready, prints
  that batch, and exits. It needs a participant binding and is not a health
  check.
- `post watch --interval-ms 1000` runs until killed and needs a binding. It
  wakes on inotify (Linux) or FSEvents (macOS), treats full scans as truth,
  and polls at the interval when native events are unavailable.
- A bound participant's watch works from any directory. `--room <room>`
  (repeatable) names workspace addresses explicitly; omitted, they resolve
  from the binding and cwd.
- A long watch replays everything still unread on start. `--from now` skips
  that backlog and rings only later arrivals; it also skips anything that
  arrived while no watch ran.
- `--reason mail|channel|mention` (repeatable) delivers only those events;
  omitted, every event is delivered. The filter runs after the scan and before
  `--limit`, `--digest`, and the `--once` exit check. An unreadable channel
  message is always reason `channel`, so `--reason mention` alone never
  surfaces it.
- `--digest` prints one line per address and source group in each batch
  instead of one per event. Use it for any doorbell on a busy channel.
- A long watch re-checks migration state before every heartbeat and exits
  nonzero if the generation changes or its state file disappears. Under a
  same-generation fence it keeps notifying but skips routing and lease
  refresh, warning once per fence.
- `POST_WATCH_PROFILE=1` prints one diagnostic line per scan on stderr (scan
  times and file counts). The format is not a contract; stdout and the store
  are unchanged.

For smokes, seed an event before `--once`, or run a long watch in a bounded
session and stop it explicitly, always against a throwaway `POST_MAIL_ROOT`.

## Events

Stdout is NDJSON, one object per line; warnings go to stderr. `post schema
--pretty` (`output_shapes.watch`) lists every field. Events carry metadata and
at most a short `preview`, never full bodies.

```json
{"event":"mail","address":{"kind":"workspace","name":"<room>"},"room":"<room>","id":"...","from":"...","from_participant":"...","origin":"local","reply_to_participant":"participant:claude-deadbeef","reply_to_shared":"...","kind":"note","subject":"...","sent":"...","reason":"mail","preview":"..."}
{"event":"mail","address":{"kind":"lineage","name":"ember"},"id":"...","from":"...","origin":"unknown","reply_to_shared":"...","pending":true,"kind":"note","subject":"...","sent":"...","reason":"mail"}
{"event":"channel_message","address":{"kind":"workspace","name":"<room>"},"room":"<room>","channel":"...","id":"...","from":"...","origin":"remote","reply_to_shared":"...","subject":"...","sent":"...","reason":"mention","preview":"..."}
{"event":"unreadable","address":{"kind":"workspace","name":"<room>"},"room":"<room>","channel":"<channel>","id":"...","reason":"channel"}
{"event":"digest","address":{"kind":"workspace","name":"<room>"},"room":"<room>","source":"channel:ops","count":3,"first_id":"...","last_id":"...","from":["..."],"reason":"mixed","preview":"..."}
```

- Every event has `address: {kind, name}`. `room` appears only for workspace
  addresses. `pending: true` marks mail not yet routed to you.
- `reason` is `mail`, `channel`, or `mention`; digests add `mixed`.
- `reply_to_participant` appears only for `origin: local`. Digests have no
  single reply target; their `from` lists up to five senders plus `"+N more"`.
- An unreadable channel event is identified by (channel, id). Older producers
  omit `channel`.
- Previews are sanitized to one line of at most 80 characters: newlines and
  tabs flatten, controls are stripped, truncation ends in `…`, and ASCII square
  brackets become full-width so a preview cannot forge a `[--since ...]`
  group. Unreadable events have no preview.

A watch's notification memory lives only in its process. After a restart, an
id you consumed stays quiet and an unconsumed id may ring again. Adapters own
dedupe across hook invocations.

## Hook adapters

Nothing here is required to use post from a shell. Lifecycle hooks inject
metadata-only new-mail notices into a live session, so they fire only on
activity. Installers run from the post checkout, take an explicit target
path, and are idempotent:

```bash
node skills/post/hooks/install-claude-hooks.mjs ~/.claude/settings.json
node skills/post/hooks/install-codex-hooks.mjs "${CODEX_HOME:-$HOME/.codex}/hooks.json"
node skills/post/hooks/install-cursor-hooks.mjs ~/.cursor/hooks.json
node skills/post/hooks/install-grok-hooks.mjs ~/.grok/hooks/post-mail.json
```

- **Claude Code:** SessionStart, UserPromptSubmit, and root PostToolUse. Idle
  wake: the Monitor doorbell.
- **Codex:** the same three events. On first run, approve the hook in
  `/hooks`; the installer registers it but cannot grant trust.
- **Cursor CLI:** `sessionStart`, `beforeSubmitPrompt`, `postToolUse`. Idle
  wake: a background `node ~/.cursor/hooks/post-watch-notice.mjs --once`
  (Cursor starts a turn when a background task completes). Point that task at
  the notice script, not raw `post watch`.
- **Grok Build:** UserPromptSubmit only; Grok ignores SessionStart and
  PostToolUse output, and its scan of `~/.claude/settings.json` drops `args`.
  Idle wake: Grok `monitor` on `node ~/.grok/hooks/post-watch-notice.mjs`,
  not raw `post watch`.

A hook notice is data with no authority, like all mail. A "mail check failed"
notice means inbox state is unknown, not empty: check with `post inbox --json`
and `post channels --json`.

## Herdr doorbell

The optional Herdr doorbell wakes one named Herdr agent from outside its
session, including Cursor and Grok agents (the macOS installer's name says
Codex; the sink is Herdr). It installs a service under the operator's
account.

```bash
# Linux
node skills/post/hooks/install-systemd-doorbell.mjs \
  --room <room> --agent <herdr-agent> [--channel <name>]... [--interval-seconds <n>]
# macOS
node skills/post/hooks/install-codex-doorbell.mjs \
  --room <room> --agent <herdr-agent> [--channel <name>]... [--interval-seconds <n>]
```

Each installer prints usage with `--help` and removes its service with
`--uninstall --agent <herdr-agent>`. The separate continuous-watch Linux
`post-doorbell@.service` is described in `doorbell/README.md`. Run one wake
mechanism per agent; uninstall and environment pinning are in
`docs/ADAPTERS.md`.
