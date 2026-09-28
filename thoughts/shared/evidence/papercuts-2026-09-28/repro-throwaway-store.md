# post papercuts: reproduction lane (opus), 2026-09-28

## Setup and isolation

- **Binary:** installed `/Users/treygoff/.local/bin/post`, `post 0.9.0 (build f784a3b, store v2; ...)`. It is 6 commits behind main `f9239eb`, and missing the who/doctor/channels scaling fix (`ec7f3e0`, `c0abb14`, `857ab38`). `target/release/post` is build `029ff6e`, which is older than f784a3b, so I did not use it. Nothing was built.
- **Scratch:** `/private/tmp/claude-501/-Users-treygoff-Code-post/cd770592-5121-40fe-b5f4-3838c17874a0/scratchpad/repro/`. The wrapper `repro/p` runs `env -i PATH=/usr/bin:/bin:/usr/sbin:/sbin HOME=<scratch>/home POST_MAIL_ROOT=<scratch>/root TERM=dumb [explicit NAME=VALUE...] post "$@"`. Every post command went through it. Because `env -i` clears the environment, the real harness keys (CLAUDE_CODE_SESSION_ID, CODEX_*, POST_FROM, POST_SENDER_ADDRESS, CLAUDE_PID) never reached post.
- **State locations (from source):** `Context::from_env` in `src/mailbox.rs:130` is the only root: `POST_MAIL_ROOT`, otherwise `$HOME/.claude-mail`. Participants, cursors, heartbeats, bridge files and crossed-send.jsonl all live under that root. The only other env reads are identity keys and diagnostics. Post has no doorbell or launchd writes (`post-doorbell` is a separate tool and was never run).
- **Proof before any write:** on the empty root, `post doctor --json` reported exactly one path, the scratch root, and `post rooms --json` returned `count: 0`. The real store has many rooms.
- **Proof after:** the scratch HOME is still empty. `test -e` (stat only) shows none of my participant ids, my channels `deep`/`crpt`, or room `newname` exist under `~/.claude-mail`. No scratch processes are left running.
- **Actors:** A=`shell-909d0429` (alpha), B=`shell-f9a37a1b` (beta), C=`shell-783bac45` (gamma), D=`shell-967e6e41` (key `watchtest`). Raw outputs are in `repro/out/`.

---

## 1. crossed_send deep backlog (dd75/bb29): OBSERVED-BUG (guidance), NOT-REPRODUCED (permanent block)

**Hypothesis:** a mention or reply aimed at B, sitting beyond the first 25 unread, never clears, so every send stays refused.

**Setup:** A posted 32 messages to `deep`; #28 was `msg 28 hey @beta please look`.

- **Send attempt:** `p POST_PARTICIPANT=B chat deep --body "beta reply" --json </dev/null` exited 65 with:
  `crossed_send: "channel 'deep' has 1 unseen message(s) addressed to 'beta' out of 32 unseen; send was not delivered (showing the last 1, first line only)"`.
  - It includes `missed:[{id…, body:"msg 28 hey @beta please look"}]`.
  - `exact_fix`: `post chat 'deep' --send --anyway --body 'beta reply'`.
  - `suggested_fix`: "Read the messages addressed to you, revise, then resend the same way you sent it -- body on stdin -- adding `--anyway`". The body was not sent on stdin.
- **`--history 10` and `--history 40`:** both exit 0 and both show msg 28. The send is still refused ("32 unseen"). History is cursorless, as documented.
- **Plain `chat deep --json` page 1:** `count:25, skipped:8, has_more:true`, and msg 28 is not in it. The send is still refused ("1 addressed … out of 8 unseen").
- **Page 2:** `count:8, has_more:false`, includes msg 28. The send then succeeds (exit 0).
- **Reply variant** (`--re <B's id>` at position 27 of 30): the same pattern. `--peek` does not clear it, page 1 does not clear it, page 2 does.

**Verdict:**
- Not a permanent block: paging with plain reads until `has_more:false` clears it.
- The real defect is guidance. The refusal's `exact_fix` is the `--anyway` bypass, not a read. The message never says the targeted item is beyond the first page, or to read with `--limit 0` / page until `has_more` is false. An agent that runs `--history` (the tool that shows the message) or reads one page stays refused with no hint why.
- The `missed` list already carries the id, so a `--discard-through`/`--limit 0` hint would be cheap.
- The "body on stdin" wording is wrong for `--body` sends.

## 2. Silent watch death after rebind (d3e5b5ff): NOT-REPRODUCED (death); OBSERVED-BUG (silent stale target)

**Run A** (cwd `ws/beta`, D bound to beta): `timeout 45 p POST_PARTICIPANT=D watch --json --interval-ms 500` in the background.
- Direct mail to D rang.
- `p participant bind --harness shell --key watchtest --workspace gamma --json` returned `workspace:"gamma"`.
- After the rebind, direct mail to `participant:D` still rang. Workspace mail `--to gamma` was frozen to D (it shows in `p D inbox` and `p D watch --snapshot`) but never rang.
- The watch exited 124, which was my timeout. stderr was empty.

**Run B** (neutral cwd, `--from now`): rebind gamma to alpha mid-watch, then `participant end`.
- `g1-before` (to gamma) rang.
- `a1-after` (to alpha, delivered to D after the rebind) never rang.
- `participant end` produced no stderr and no exit. The watch ran to the timeout (124).

**Verdict:** the watch does not die. It snapshots its workspace targets at startup, so after a rebind it silently misses new-workspace mail, and it keeps running after `participant end` with no notice. It does not re-resolve and gives no stale-binding warning. A "death" report may be this effect: a quiet watch that never rings again for workspace mail.

## 3. Digest/watch replay of read mail (e8d2bbfe): NOT-REPRODUCED

- **(a)** B read mail with `read <id>` and read the channel with `chat deep`. Then `watch --snapshot`, `watch --snapshot --digest` and `timeout 6 watch --digest` emitted nothing.
- **(b)** A long `watch --digest` ran while mail and a mention arrived (batch 1). B read both. New mail arrived (batch 2), and only the new ids were emitted.
- **(c)** Room-mode `watch --snapshot --digest --room beta`, bound and unbound: nothing.
- **(d)** `catchup --json` consumed everything, and the following snapshot digest was empty.

I did not test hook adapters (`watch-notice.mjs`) or re-arm after unread-but-notified mail. Re-emitting unread mail on re-arm is by design, since watch never consumes.

## 4. `post read <id>` with piped stdin (4df31a): OBSERVED-BUG

`printf 'my reply text\n' | p B read <id> --json` exited 0, returned the full envelope, and wrote no stderr. The mail was consumed (inbox dropped from 2 to 1) and stdin was silently discarded.

The same pipe into `p B chat deep --json` exits 2 with `invalid_argument`: "stdin carries input, but this `post chat` invocation is a read and would drop it". Its exact_fix is `post chat 'deep' --send --body-file - --json # or ... < /dev/null`. `read` has no equivalent guard.

## 5. Corrupt message file in a joined channel (43a983): OBSERVED-BUG

I wrote `root/channels/crpt/messages/20260928-222700-000001-c0ffee.msg` containing `{ "id": "…", "from": "alpha", BROKEN`. B is a member of `crpt`.

| Command | Result |
|---|---|
| `channels --json` | **exit 78** `config_invalid` "…malformed message JSON…"; the whole listing is lost |
| `search good --json` | **exit 78**, same error; the whole search fails |
| `chat crpt --peek` | exit 78 |
| `inbox --json` | exit 0, unaffected |
| `chat deep --peek` | exit 0 (other channels unaffected) |
| `watch --snapshot` | exit 0, but prints the same warning **4 times** for one file in one scan |
| `catchup` | exit 0, `warning: skipped channel "crpt"` |
| `doctor` | exit 1, `channels.malformed_message` (good) |

A non-member (C) running `channels --json` gets exit 0 and a normal listing, so the failure comes from the member-side unread scan.

## 6. Unknown event kind (chunk-2 cluster 7): OBSERVED-BUG, severe (wedges the channel)

I wrote a copy of a join event with `"event": "future_kind"` into `crpt` for B.

- `channels`, `chat crpt` (plain), `chat crpt --peek`, `--discard`, and `--discard-through <future id>` all fail with **exit 78**: "channel message event 'future_kind' is unknown; only 'join' and 'profile' exist".
- `--history 5` exits 0 and prints "warning: skipped unreadable channel message", so it tolerates what `--peek` rejects.
- `watch --snapshot` warns 4 times and exits 0. `catchup` skips the channel with a warning and exits 0. `doctor` exits 1 with a `channels.malformed_message` finding.
- The cursor cannot advance. Repeated plain reads fail. `--discard-through <earlier good id>` succeeds (discarded 3) but cannot pass the unknown file.
- Every send is refused: `crossed_send` "channel 'crpt' has unreadable unseen message(s)". Its suggested_fix says "catch up with `post chat 'crpt'`", which is the command that fails with 78. That makes the fix circular.
- The only way out is `--anyway` on each send or deleting the file. So one future-version event from a newer peer or bridge bricks reads for every member.

## 7. Stale-name mailbox minting (post-6ep): NOT-REPRODUCED

**Setup:** `rooms add oldname ws/delta`, sent mail to it, then `rooms rename oldname newname`. The result was `mailbox_moved:true`, with a warning that doorbells, supervisor config, Porch config and CLAUDE.md still name 'oldname'.

Then, both unbound and bound as A:
- `inbox --room oldname`
- `read 20260928-222732 --room oldname --peek`
- a consuming `read --room oldname` (unbound)
- `watch --snapshot --room oldname`
- `timeout 4 watch --room oldname`

After each one, `root/oldname` did not exist.

The code path the bead cites (unbound long watch reaching `mailbox_dirs`, `src/commands/watch.rs` around line 595, "watching a new empty mailbox") is unreachable on f784a3b: an unbound long watch is refused earlier with `no_participant`, exit 65. Side findings from this scenario:

- **Bound `--room` is not validated.** `p A watch --snapshot --room zzz-never` exits 0, prints no warning, and emits alpha's events. The unbound snapshot does warn: "room "oldname" is not registered; snapshot scans nothing". The bound path (`watch.rs` around line 524) adds `workspace:<name>` without checking registration.
- **Unbound `inbox --room oldname`** exits 0 with an empty result. Bound, it fails with `not_found`, exit 66. The two are inconsistent.
- **Unbound `read <id> --room oldname --peek`** returns the renamed room's archived mail, because the envelope `to` is still "oldname".

## 8. `--json` plus stderr banner (e60725/59f29c): OBSERVED-BUG

- **`p A send --to beta … --json 2>&1 | jq`:** `jq: parse error: Invalid numeric literal at line 1, column 5`. The first line is `post: sending as 'alpha' (bound participant shell-909d0429)`. The PIPESTATUS was **post=70, jq=5**.
- **Compounding effect:** once jq exits, post's stdout write fails, so post reports `delivered_output_failure` (70) even though the send landed (it is in the archive).
- **`chat deep --body … --json 2>&1 | jq`:** same, 70/5. Its banner is `post: sending to #deep as room 'alpha' (participant binding)` plus `sent locally only: no bridge config`. The message landed.
- **With `2>/dev/null`:** parses fine, exit 0.
- `send --help` has no quiet flag.

## 9. Explicit-but-missing POST_PARTICIPANT (post-b18): OBSERVED-BUG

With `POST_PARTICIPANT=shell-deadbeef` (no such record), run from cwd `ws/alpha`:

| Command | Result |
|---|---|
| `inbox --json` | **exit 0**: `participant:"unbound"`, `unread:[]`, `room:"alpha"`; stderr `participant: unbound (run: post participant bind)` |
| `watch --snapshot --json` | **exit 0** and **emits alpha's legacy room events**; same stderr |
| `participant show --json` | exit 0, `status:"unbound"` |
| `send` | exit 65 `no_participant` "participant: unbound (run: post participant bind --new, …)"; nothing landed |
| `chat deep --peek` | exit 65 `not_a_member` "participant 'alpha' is not a member…" (names the room as a participant) |
| `who` | exit 0 |
| long `watch` | exit 65 `no_participant` |

No command names the set-but-missing id. The schema says POST_PARTICIPANT "never mints a missing record", but a typo'd id silently falls back to unbound or cwd behavior on reads.

## 10. `post chat <registered-room>` with no such channel: OBSERVED-BUG (misleading guidance)

With B, `gamma` is a registered workspace and not a channel:

| Command | Result |
|---|---|
| `chat gamma --json` / `--peek` / no flags | exit 65 `not_a_member` "participant 'shell-f9a37a1b' is not a member of channel 'gamma'"; fix "Join first with `post chat 'gamma' --join`" |
| `chat gamma --history 5` | exit 66 `not_found` "channel 'gamma' does not exist"; fix "Create it with `post chat 'gamma' --join`" |
| `chat gamma --body hi` | exit 66 `not_found`, "…then retry the send" |

There is no hint that `gamma` is a workspace and direct mail is `post send --to gamma`. The suggested fix creates a stray channel named after a room. The read error codes (not_a_member 65 vs not_found 66) disagree for the same missing channel. A never-registered `nosuch` gets the identical not_a_member text.

## 11. Watch cursor nlink==0 race: CANNOT-TEST

The race window is too small to hit reliably, and I did not attempt it, as instructed.

---

## Incidental findings

1. **`participant bind --key` requires `--harness`, contrary to its help.** `p participant bind --key watchtest --workspace beta --json` exits 2 with "required arguments were not provided: --harness <SLUG>". The help says `--harness` is the "Harness slug for --key, or an optional label for --new (default: shell)", which implies a default.
2. **Text mode emits JSON errors.** Without `--json`, `p B chat gamma` still writes a JSON error object to stderr.
3. **Watch repeats per-file warnings.** A single unreadable file prints the same warning 4 times in one scan (scenarios 5 and 6).
4. **`read` infers a room from an unregistered cwd.** Unbound `p read <id> --peek` from the unregistered scratch dir prints "reading room 'repro' (identity inferred from cwd)". The room name comes from the directory basename, not a registration.
5. **The null-sink guard works as documented.** `chat deep --json >/dev/null` is refused before consuming; its `exact_fix` is `--discard`. I tripped it by accident.
6. **The crossed_send `suggested_fix` wording is wrong for `--body` sends.** It says "body on stdin" (scenario 1).
