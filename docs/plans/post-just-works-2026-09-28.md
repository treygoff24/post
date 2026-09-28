# Post just works: build contract for the 2026-09-28 fix wave

Trey ruled on 2026-09-28 (coordinator session): agreed across the board to `thoughts/shared/papercuts-report-2026-09-28.md` plus the coordinator's fresh-eyes review, including retiring `--anyway` and moving the bridge into this repo. The goal: post is the tool an agent would want to use. Agents are its only users.

This file holds the cross-lane interfaces. Each lane's brief says which files it owns. If your lane needs an interface to behave differently from what is written here, say so in your report instead of diverging.

## How agents actually use post (Mac data, last 14 days)

- About 10% of sessions run post at all. `chat` is over half of all post commands. Channels are 2-8 agent project rooms with long messages (median about 800 characters, p90 about 2,800).
- 36% of post commands discard stderr (`2>/dev/null`). **Anything an agent must see goes to stdout** (in the JSON envelope, or the text output). Stderr is for diagnostics humans read.
- Agents guess JSON field names (`.members // .member_count`). Field names must be stable and documented by `post schema`.
- The crossed-send guard was bypassed with `--anyway` 73% of the time, including when the crossed message was addressed to the sender; agents now pass `--anyway` pre-emptively.
- 1,799 participant records since 09-14; 54 ever held state; 1,197 have no workspace.

## Interfaces every lane must agree on

### 1. Identity states (lane: identity)

- `participant_missing` (new error code, exit 65): an explicit claim (`POST_PARTICIPANT`, or an ambient session-index entry) names a record that does not exist. Every command except `participant show`, `who` and `doctor` fails with it. Its `suggested_fix` names the exact rebind command. Diagnostic commands report it as a field, exit 0.
- **Unbound** (no explicit claim; an ambient harness key with no index entry, or no key at all): read-only commands (`inbox`, `watch --snapshot`, `chat --peek/--history`, `channels`, `search`, `read --peek`) exit 0 with an explicit stdout marker: JSON `"participant": null, "bound": false` plus a one-line `hint`; text mode prints one line saying the session is not bound yet and nothing can be addressed to it. **No cwd-room fallback for readers any more.** Explicit `--room <name>` on `inbox`/`watch --snapshot` stays for command sinks (supervisor, hq).
- **Lazy minting.** A write command (send, `chat --send`, `chat --join`, consuming reads, `--adopt`) run with an ambient harness key and no record mints the record exactly as `participant bind --harness <h> --key <key>` would (same deterministic id), then proceeds. The receipt carries `"bound_now": {"id": ..., "workspace": ...}` and text mode says so in one line. Without an ambient key it fails `no_participant` with the bind command in the fix.
- `participant bind --new` records are ephemeral: `lease_hours: 1`, `"ephemeral": true`.
- `post participant gc` (dry run by default, `--apply` to act), JSON output `{"deleted": [...], "archived": [...], "kept": {"reason": count}}`:
  - Tier 1, delete with a tombstone line in `participants/archived.jsonl`: not active, last seen older than 7 days (24 hours if ephemeral), directory holds only `participant.json`, `activation-notice`, `.cursors.lock`, `heartbeat`, and nothing names the id (no lineage, supervisor subscription, routing receipt, pending or held mail). The tombstone keeps occupying its id for width-8 collision selection.
  - Tier 2, move to `<root>/participants-archive/<id>/`: not active, last seen older than 30 days, has state, and **no unread or pending mail of any kind**. `bind` for that key restores it before minting.
  - Never touched: active lease, fresh heartbeat, unread/pending/held mail addressed to it, a lineage's current holder, a supervisor subscription.
  - Frozen unread mail on stale participants is retained, never rerouted.

### 2. Crossed sends (lane: channels)

- A channel send always delivers. `--anyway` stays accepted as a hidden no-op so habitual commands keep working; it is gone from help, hints and the skill.
- The send receipt carries what crossed: JSON `"crossed": {"unseen": N, "addressed_to_you": M, "messages": [{"id","from","display_name","sent","addressed_to_you","body"}]}` (full body for messages addressed to the sender or replying to it, a 300-character preview otherwise, at most 10 messages, newest last). Text mode prints the crossings after the sent line, addressed ones first and in full. Sending does **not** mark crossed messages as read.
- `crossed-send.jsonl` logging keeps working with outcome `delivered_crossed`.

### 3. Channel event kinds (lanes: channels and bridge)

- Known kinds: message (no `event`), `join`, `profile`. Any other `event` value is an opaque system event: rendered as `[event: <kind>]` in text, passed through in JSON with its kind, excluded from unread counts, never blocks a cursor, never fails a read, a listing, catch-up or a send. The bridge imports and relays unknown kinds unchanged.
- A corrupt or unparseable channel message file never fails a whole command: listings and reads skip it and report it once in stdout (JSON `"skipped": [{"id","reason"}]`; text: one line).

### 4. Bridge health and attention (lanes: bridge and surface)

- `~/.claude-mail/bridge/health.json` gains `"attention": [{"kind": str, "id": str|null, "summary": str, "fix": str}]`, empty when nothing needs a human or agent. Kinds at least: `refused_letter` (terminal refusal of an outbound letter; id = letter id), `unrelayable_letter`, `quarantined_inbound`, `name_collision`.
- `ok` stays a liveness flag; `attention` is the "something is stuck" list.
- `post doctor` shows each attention item as a warning with its fix; `post who` (text and JSON) shows `bridge_attention: <count>` when nonzero.
- A terminal refusal of an outbound letter writes a system letter into the **sending participant's** inbox (or the sending room when there is no participant): subject `Undeliverable: <original subject>`, body naming the letter id, recipient, reason, and the exact re-send command. Then the outbox entry is retired.

### 5. Version and install (lane: surface)

- `post --version` and `post version` both print `post <semver> (build <short-sha>[-dirty], ...)`.
- The installer refuses to install a commit that no branch on `origin` contains.

### 6. Output rules (all lanes)

- `--json` output is pure JSON on stdout; no banners on stderr under `--json` (the receipt already carries identity). Text mode never prints JSON on stderr.
- A read-only listing never fails because of one bad item.
- Anything degraded, skipped or surprising is reported in stdout, not only stderr.
- Every error's `suggested_fix` is a command that works when pasted.
- Help text teaches `--body-file <path>` or stdin heredoc for message bodies; `--body` only for short one-liners.

## Test rules (all lanes)

- Tests never touch `~/.claude-mail`: use the existing test support to root the store in a temp dir with a cleared environment.
- Every new behavior gets a test that fails on the old code. Run it against the old code once and see it fail.
- The project gate is `scripts/gate.sh` (fmt, clippy `-D warnings`, all tests, release build, node hook tests, launcher tests, `post schema`). Lanes run it before reporting.
