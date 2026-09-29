# Operator procedures

For whoever runs a host, not for an agent in the middle of a task: installing
hooks and the doorbell supervisor, cleaning up participants, running and
repairing the bridge, and checking the skill bundle itself. Agents read
[`SKILL.md`](../SKILL.md). The binary is the authority on flags: `post <command>
--help` and `post schema --pretty`.

## Hook adapters: install

Lifecycle hooks inject metadata-only mail notices into a live session. They
fire only on activity; idle wake is the supervisor below. Installers run from the
post checkout, take an explicit target path, and are idempotent:

```bash
node skills/post/hooks/install-claude-hooks.mjs ~/.claude/settings.json
node skills/post/hooks/install-codex-hooks.mjs "${CODEX_HOME:-$HOME/.codex}/hooks.json"
node skills/post/hooks/install-cursor-hooks.mjs ~/.cursor/hooks.json
node skills/post/hooks/install-grok-hooks.mjs ~/.grok/hooks/post-mail.json
```

- Each installs a small adapter beside a shared `mail-hook-core.mjs` (mode
  0644). Every file, the settings or hooks file included, is first written to a
  temp file beside its destination and renamed into place only once all the
  writes have succeeded, so a failed write (full disk, unwritable directory)
  leaves what was installed untouched and removes its temps. The core is
  renamed before the adapter that imports it. Rerunning prints
  `adapter updated` when either changed.
- **Claude Code:** SessionStart, UserPromptSubmit, root PostToolUse, and
  SessionEnd (the only adapter that ends the participant). **Codex:** the first
  three; approve the hook in `/hooks` on first run, since the installer cannot
  grant trust. **Cursor:** `sessionStart`, `beforeSubmitPrompt`, `postToolUse`.
  **Grok:** UserPromptSubmit only; its first prompt plays session start.
- Claude and Codex hooks bind at session start only when the session's
  directory is inside a registered room (`post rooms add <name> <path>`). Any
  other directory, and any delegated run (`DELEGATE_RUN_ID`), defers: the hook
  asks `post participant show --harness <h> --key <k> --json` each turn and adopts
  the id once the agent's first write has bound it. Cursor and Grok always bind
  and print the id, because their agents cannot see an ambient session key.
  If the lookup itself gets no answer (a timeout, a crash, a `post` too old to
  take `--harness`), the hook leaves the session unbound, says so once, and does
  not mint; a session that finds `post` missing or broken is likewise told once
  per session, not on every prompt and tool call. Update `post` to fix either.
- If a hook reports `participant_missing`, it rebinds once with `post participant
  bind --harness <h> --key <k>` and retries; a failure after that is reported as
  an UNKNOWN inbox, never as an empty one.
- Test overrides: `POST_<HARNESS>_HOOK_BIN`, `POST_<HARNESS>_HOOK_STATE_DIR`,
  `POST_<HARNESS>_HOOK_THROTTLE_MS`, `POST_<HARNESS>_HOOK_DEADLINE_MS` (`CLAUDE`,
  `CODEX`, `CURSOR`, `GROK`). A hook runs every `post` call, the notice release
  included, inside one 4.5 s deadline (the default of `..._DEADLINE_MS`), under
  the 5 s timeout Codex installs. The full adapter contract is `docs/ADAPTERS.md`
  in the post repo.

## Doorbell supervisor: install, migrate, uninstall

One supervisor per host (launchd on macOS, a systemd user service on Linux)
wakes idle Herdr panes and residents; agent-facing commands are in
[`watch.md`](watch.md). From the post checkout:

```bash
node skills/post/hooks/install-doorbell-supervisor.mjs --dry-run
node skills/post/hooks/install-doorbell-supervisor.mjs          # waits for the lock and a healthy first tick
node skills/post/hooks/install-doorbell-supervisor.mjs --uninstall
```

The install is idempotent and records each step in
`$POST_MAIL_ROOT/doorbell/install-receipt.json`, so an interrupted run resumes.
Export `POST_MAIL_ROOT` first if the host does not use `~/.claude-mail`. Logs:
`~/Library/Logs/post-doorbell-supervisor.log` (macOS),
`~/.local/state/post-doorbell/supervisor.log` (Linux). An old Python
`~/.local/bin/post-doorbell` is moved aside with its hash, not deleted.

The per-agent timer installers and the Python doorbell are gone. A timer that is
still installed keeps working until you migrate it:

```bash
node skills/post/hooks/install-doorbell-supervisor.mjs --list-legacy [--json]
node skills/post/hooks/install-doorbell-supervisor.mjs --migrate <agent>   # exit 0 migrated, 3 kept on its timer
node skills/post/hooks/install-doorbell-supervisor.mjs --restore-legacy    # roll back
```

Migration carries the timer's rooms, channels, and pane to the supervisor, then
disables the timer, and records `migrations` in the receipt. A migrated agent
rings for a superset of what it did before: mentions from any channel it joined
now ring too. To drop a timer without migrating it, unload it yourself
(`launchctl bootout gui/$(id -u)/dev.post.codex-doorbell.<agent>` on macOS;
`systemctl --user disable --now post-codex-doorbell@<agent>.timer` on Linux).
Run one wake mechanism per agent.

## Participant gc

Participant records pile up (1,799 on one Mac in two weeks, 54 of them ever
holding state). `post participant gc` is a dry run; add `--apply` to act. It
prints `{"deleted": [...], "archived": [...], "kept": {"<reason>": <count>}}`.

- **Delete** (a tombstone line goes to `participants/archived.jsonl`, and keeps
  the id occupied for collision selection): not active, last seen over 7 days
  ago (24 hours if ephemeral), the directory holds only `participant.json`,
  `activation-notice`, `.cursors.lock`, and `heartbeat`, and nothing names the
  id (no lineage, supervisor subscription, routing receipt, or pending or held
  mail).
- **Archive** (moved to `<root>/participants-archive/<id>/`): not active, last
  seen over 30 days ago, has state, and no unread or pending mail of any kind.
  The next `bind` for that key restores it before minting.
- **Never touched:** an active lease, a fresh heartbeat, unread, pending, or held
  mail addressed to it, a lineage's current holder, a supervisor subscription.
  Frozen unread mail on stale participants is kept, never rerouted.

Run the dry run and read `kept` before `--apply`; `--help` lists the flags this
build ships.

## Bridge

The bridge source and its spec live in `bridge/` in the post repo (its README,
and `SPEC-v2.md` for host enrollment, the registry, room ownership, channel
import). Read the spec rather than guessing anything v2-only. Each host runs one
tick per timer interval: fetch every peer's branch of `estate/post-relay`,
deliver inbound mail, publish outbound mail, push, and write health.

| What | Where |
| --- | --- |
| Action log, one JSON line per action, no bodies | `$POST_MAIL_ROOT/bridge/log.jsonl` (rotates at 10 MiB) |
| Health | `bridge/health.json` |
| Topology | `bridge/config.json`: `host`, `relay_url`, `peers`, optional `channels` |
| Rejected input, kept for forensics | `bridge/quarantine/` |

On Linux, `journalctl --user -u post-bridge.service -n 50` shows the last ticks.

**Health.** `bridge/health.json` has `ok` and `reason`, and `ok` is a liveness
flag only. Reasons: `fetch_stale` (no successful fetch within 600 s; check the
relay key, the forge, and the log), `fenced` (`.post-arx.json` exists in the mail
root; the tick changes nothing until it is gone), `busy`, `deadline`, and
`internal_error` (transient unless repeated), and `config_error` (exit 2; the
config or environment is wrong and every tick fails the same way). v1 adds
`undeliverable`. A quarantined message does not make health false; health counts
it under `quarantined`. v2 also reports `rooms.collisions`, `channels.diverged`,
and `channels.rewritten`.

**Attention.** `health.json` carries `attention`, a list of `{kind, id, summary,
fix}`, empty when nothing is stuck: `refused_letter`, `unrelayable_letter`,
`quarantined_inbound`, `name_collision`, and others. `post doctor` shows each as
a warning with its fix, and `post who` prints `bridge_attention: <count>` when
nonzero. A terminal refusal of an outbound letter also writes an `Undeliverable:
<subject>` letter into the sender's inbox naming the letter id, recipient,
reason, and the exact re-send command, then retires the outbox entry.

### Name collisions: one host renames its copy

The destination checks the sender's workspace name against its own rooms. When
that name is also a real room there, the mail is quarantined and never
delivered (v1 records `forged_from`; v2 marks the name contested and records
`name_collision`), and the sender's `ok` hides it. Under v2, mail to a contested
name routes nowhere and health reads `room_name_collision` until one side
renames. A name belongs to one host: the room's home keeps the bare name, and
the other host's copy takes a host suffix. `post rooms add` refuses a name
another host already publishes and prints the suffixed command. To fix an
existing clash, rename the copy:

```bash
post rooms rename <old> <old>-<host> --dry-run --json   # every check, no writes
post rooms rename <old> <old>-<host> --json
```

The rename moves `<root>/<old>` and rewrites the live state that names it. On a
bridged host it refuses (`bridge_guard_unavailable`, retryable) unless
`bridge/health.json` ticked within three intervals, its `local_held` faults and
unaccounted candidates are both 0, and every archive letter the bridge would
export for `<old>` has a `bridge/local-held/<id>.json` hold. It also refuses
while `owner.json` names `<old>`: pin the owner's principal, namespace, and
label and point `room` at the new name first. Around it:

- **Pause the bridge.** Its import does not take post's rename lock. Stop the
  timer (`systemctl --user stop post-bridge.timer`, or `launchctl bootout` the
  Mac job) right after a tick, rename inside the freshness window, then start it.
- **Release the old name in the bridge.** Each bridge records a name's owner in
  `bridge/rooms/owners.json` and keeps claiming a name its own host vacated. After
  the rename, health still reads `room_name_collision` until the host that
  renamed deletes the `<old>` entry from its own `owners.json`. With the bridge
  paused, delete that one entry and resume. Leave `owners.last.json` alone.
  Within two ticks the home host is the only claimant and health reads `ok`.
- **Re-arm watchers** armed on the old name, and update config outside post's
  store that names it (Porch `owner_room`, doorbell configs, CLAUDE.md files).
  The receipt's `warnings` name these. A running session keeps the `POST_FROM`
  pin it resolved at launch, so it sends as `<old>` until it restarts.
- **After a crash**, `post doctor` reports `rooms.rename_interrupted`; rerun the
  same pair to resume from `$POST_MAIL_ROOT/rename-journal.json`.

The destination learns the new name from your host's published room list on the
next tick and quarantines mail from a name it has not yet seen published
(`unpublished_sender`), so wait one tick before the first send. Under v1, the
destination needs the new name in its `peers` config instead.

## The skill bundle itself

`build.rs` compiles a sha256 manifest of every file under `skills/post/`
(`SKILL.md`, `references/`, `hooks/`, `agents/`; dot-files ignored) into the
binary. `post contract skill-manifest --json` prints it, and `post contract
skill-manifest --verify <served-path>` checks a served copy or symlink against
it (exit 0 on `match`, 1 on `drift`, naming mismatched, missing, and extra
files). Nothing is checked in: editing any skill file changes the manifest at
the next `cargo build`, so rebuild before verifying, and verify the served
directory (for example `~/.agents/skill-library/post`) after every install.
