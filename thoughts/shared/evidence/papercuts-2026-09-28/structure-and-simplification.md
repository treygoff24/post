# post: structure review and simplification audit (lane opus-structure)

Read-only review at HEAD `f9239eb` (source), 2026-09-28. The installed Mac binary is `f784a3b`. I checked the analyst lanes' claims against source. The only live commands I ran were `post version`, `post --help`, `post participant show --json`, three timed runs each of `post inbox --json` and `post watch --snapshot --json --limit 0`, two runs of `post participant list --json`, and read-only `stat`/`ls`/python counts over `~/.claude-mail`. I did not run `who`. A background search enumerated the external consumers for (f).

## Headline

1. **The simplification premise is misattributed.** The "~28 messages ever" figure counts delegate's own mail (`find ~/Code/*/.delegate/mail -path '*sent*'`, `docs/plans/verified-progress-sharing-2026-09-21.md:102`), not post. Post on this Mac carried **1,040 direct letters since mid-July** (`~/.claude-mail/archive`, recently about 5-30 a day) and about **9k channel messages**, 100-900 a day from 09-14 to 09-28 (`commons` 1,068, `machineroom-devbox` 921, `build` 912, ...). Channels are the load-bearing feature. Direct mail is modest but real.
   - What is actually out of proportion is **identity minting**: 1,796 participant records against 1,040 letters ever.
   - Only 44 records have ever consumed anything (`cursors.json`), 19 have joined a channel, 6 have ever had an inbox, and 3 have a lineage. **1,677 of 1,796 (93%) hold only `participant.json` + `activation-notice`.** Every record has `lease_hours: 24`.
2. **Lane-repo's by-name room-directory creator is wrong: post-6ep's creator is the bridge, not post.**
   - `Context::mailbox_dirs`'s create branch (`src/mailbox.rs:491-497`) can't be reached in production at HEAD: every non-read-only path to it needs a bound participant, and a bound participant takes a different branch.
   - The directories come from `~/Code/claude-space/post-bridge/sweep.py:687-690`, which runs `ensure_dir` on `<root>/<name>/{inbox,read}` for **every peer room on every tick**.
   - The bead's own timeline fits: the owner release made the old names peer rooms, and dirs reappeared in the same minute as the tick's `room_registered`.
3. **One superlinear path survives the who fix, on the send/read critical path.**
   - `routing::route_pending` re-lists every participant **once per unrouted message** while holding the exclusive, unbounded `.participants.lock` (`src/routing.rs:102-143` → `route_message_locked:659` → `filtered_recipients:733-744` → `resolved_recipients:801-825` → `participant::list_active`).
   - `resolved_once` (`routing.rs:750`) was added for doctor's `pending_summary` but not here.
   - The path runs from `read`, `catchup`, `chat` reads, and every long-watch scan, and every write command's `touch` queues behind it.
4. **The "unbound" soft state has already grown a stderr-scraping workaround in the consumer that matters most.** `doorbell-supervisor.mjs:1588-1591` greps stderr for `participant: unbound` and records `snapshot_unbound`. So the supervisor does not silently read "no mail", but only because it parses prose. The four harness hooks do treat it as no mail (`claude-mail.mjs:602-627`: exit 0 + no lines = no mail).

---

## PART 1: verification

### a. `Resolved` has only Bound/Unbound (post-b18): **CONFIRMED, and the scope is wider than the bead**

- **The enum:** `src/participant.rs:207-213` has only `Bound` and `Unbound`.
- **Explicit id with no record:** `resolve()` returns `Unbound` at `:375-383`. It returns `Unbound` only after `validate_participant_id`, so a malformed id does error.
- **Ambient session key whose index names a missing or mismatched record:** also `Unbound` (`:390-402`), as is a width-12 miss (`:414`).
- **Writers already fail loudly.** `commands/mod.rs:67-71` returns `no_participant` (exit 65) for every `participant_required` command (`mod.rs:243-268`: send, catchup, delivery, consuming read, chat writes, long watch, `inbox --adopt`, identity mutators, profile set/clear, participant touch/end/notice).
- **Readers do not.**
- **Does `inbox` fall back to legacy cwd-room mode? YES.** `inbox.rs:91-94` calls `list_unbound` → `context.resolved_room(args.room, &rooms)` (`inbox.rs:189-191`). That is `POST_FROM` pin, then cwd inference (`mailbox.rs:385-393`).
  - It reports `ok:true, participant:"unbound", count:0, unread:[]` (`inbox.rs:208-219`), and unread is never computed in this mode ("unread unavailable").
  - Knock-on: `~/Code/homestead/home/bin/fc-statusline:96` runs a bare `post inbox` and counts `unread[]`, so from an unbound context it can only ever show zero.

**Callers that change if unresolvable claims become a distinct state** (listed file:line):

| Site | Today | Needed change |
|---|---|---|
| `commands/mod.rs:52-59` dispatcher | Err from `resolve` is swallowed into `Unbound` for non-required commands | Stop swallowing the new code |
| `channel.rs:158-187` `acting_room` | `Unbound` and read-only `Err` both fall back to POST_FROM/cwd room | Refuse on dangling. **A second swallow site: an `Err` variant alone would not fix `chat --peek/--history` or `channels`** |
| `inbox.rs:83-94` | `Unbound` → legacy room listing | Propagate |
| `watch.rs:510-514, :575-590` | `Unbound` → cwd room, or "scans nothing" | Propagate |
| `read.rs:17-20` | `Unbound` → `run_legacy` (peek only is reachable) | Propagate |
| `search.rs:17`, `channels.rs:12`, `profile.rs:204`, `routing.rs:660` | `?` or match | Propagate (routing's `routed_by` falls back to "post") |
| `who.rs:58-60`, `doctor.rs:78-136`, `commands/participant.rs:198-211` (show) | Already treat Err as a reportable field | Keep reporting it as a field (show `status: "missing"`), exit 0. These are the diagnostic surfaces |
| `participant::require` `:428-435` | `no_participant` | Add a distinct code so the fix text says "record missing, rebind", not "bind" |

**What breaks if it becomes a hard error:**

- **Four harness hooks** (`skills/post/hooks/{claude,codex,cursor,grok}-mail.mjs`). They pass their own cached id via `POST_PARTICIPANT` (`claude-mail.mjs:384`).
  - Today a vanished record gives silent empty mail every turn.
  - After the change, a nonzero exit goes through the existing failure path (`:604-610`): one diagnostic per streak, and `participant show` on SessionStart.
  - This is not a break but a visible failure. It should self-heal: on the new code, re-run `setupParticipant` (`bind --harness --key` re-mints the **same id**, because ids are a pure function of harness + key digest, `participant.rs:716-739`).
- **Doorbell supervisor.**
  - A nonzero exit already maps to `recordFailure(sub,"snapshot")` with backoff (`doorbell-supervisor.mjs:1583-1586`).
  - The stderr regex at `:1588` becomes dead code. **No break.**
  - Command-sink subs pass no participant (`:1577`), so they are unaffected.
- **Launcher** (`launcher/agent-session`) never sets `POST_PARTICIPANT`, so it is unaffected. `install-systemd-doorbell.mjs:263-276` already preflights with `participant show`.
- **Porch** (`porch-tui presence.py`/`roomcheck.py`), **hq** `exception-digest.py`, **Loom** `post.ts`: only affected if they export a dangling `POST_PARTICIPANT`. None does, as far as the consumer search found.
  - Loom's phantoms (`bind --new` per REPL) already fail on long watch, which needs a participant.
- **Recommendation:**
  - Make an explicit claim (`POST_PARTICIPANT`) that resolves to nothing a typed error for every command except `participant show`/`who`/`doctor`.
  - Treat the ambient by-session-index dangling case the same way. It only arises after GC or a manual quarantine, and a hook rebind fixes it.

### b. `mailbox_dirs` mints dirs for unregistered names: **PARTLY WRONG**

- The function would mint `<root>/<room>/{inbox,read}` for any valid name when not read-only (`mailbox.rs:479-499`).
- **But no production path reaches the create branch:**

| Caller | Reached when | Creates? |
|---|---|---|
| `read.rs:31` via `resolved_mailbox_dirs` (`mailbox.rs:396-404`) → `run_legacy` | Only when unbound. An unbound consuming read is refused earlier (`mod.rs:253-255` `participant_required`), so only `--peek/--offset/--length` get here, which are read-only (`migration_fence.rs:647-649`) | **No** |
| `watch.rs:597` (Unbound branch) | Long watch needs a participant (`mod.rs:257`), so only `--snapshot` reaches it, which is read-only (`migration_fence.rs:666`). Unregistered names already `continue` before it (`watch.rs:582-590`) | **No** |
| Tests: `mailbox.rs:1960,1969`, `migration_fence.rs:822`, `send.rs:1014,1058,1158` | test-only | n/a |

- **The comment at `watch.rs:499-503`** ("Target setup can create ... by name") is stale.
- **Other creators at HEAD:**
  - `send.rs:332-337` creates the target inbox only, for registered or typed targets that `resolve_target` accepted (`participant.rs:290-346`).
  - `routing.rs:720` creates `routing/`.
  - `doctor --fix` creates inbox+read for registered rooms only (`doctor.rs:1207-1215`).
  - **The bridge's `sweep.py:687-690` creates inbox+read for every peer room every tick.** That is post-6ep.
  - The Mac's four unregistered empty dirs (`testroom` 07-15, `santoro-metals` 07-16, `fleet` 07-17, `personal-agent-benchmark` 07-19, birth times from `stat`) predate participants. They came from legacy-era binaries, like the ones still on minimal PATHs (Mac `~/.cargo/bin/post` 0.6.0; devbox `/usr/local/bin/post` Aug 31, per lane-live).
- **Doctor contradicts send.** `locus/` holds `inbox/` + `routing/` and no `read/`, which is exactly what `send` + `publish_receipt` create. Doctor calls the missing `read/` an **error** (`doctor.rs:398-407`), although `read/` is only written by the legacy consume path that bound commands never take.
- **Would "create only for registered rooms" break anything?** No. It also would **fix nothing**, because the branch is dead.
  - Legacy room mode is read-only now.
  - `rooms add` creates no directories (no `create_dir` in `rooms.rs` production code).
  - The bridge makes its own directories.
- **Right fix:**
  1. Make `mailbox_dirs` a pure path function and delete the create branch.
  2. Delete doctor's `inbox_missing`/`read_missing` errors, since lazy creation makes "absent" normal.
  3. Stop `sweep.py:689-690` from creating local `<name>/{inbox,read}` for peer rooms (the placeholder is `remote/<host>/<name>`).

### c. Remaining O(all participants / all stores) work: **PARTLY CONFIRMED; quantified**

- **Measurements on this Mac** (installed f784a3b, 1,796 participants, 79 rooms, 3 lineages; 969 `.mail` outside archive; 100 routing receipts):
  - `inbox --json`: 0.01-0.02 s (3 runs).
  - `watch --snapshot --json --limit 0`: 0.03-0.04 s (3 runs).
  - `participant list --json` (full parse of 1,796 records): 0.03 s.
- So O(P) per command is cheap at 1.8k on warm cache. The problem is multiplication.

| Path | What it scans per invocation | Order |
|---|---|---|
| `inbox` (bound), `watch --snapshot`, `search` | `visible_addresses` → `received_addresses` → **`ReceivedIndex::read` of the whole host** (`routing.rs:270-275, 403-427`): every store (`store_addresses` = rooms + every lineage dir + every participant dir, a `stat` each, `:461-502`), every routing receipt, and a parse of every canonical inbox `.mail` on the host. A single-participant question pays for the host-wide index. | O(P + R + M) |
| **`read`, `catchup`, `chat` reads, `participant bind`, long-watch scans** | `route_for_participant` → `route_pending` per address (`routing.rs:102-170`) → **for each unrouted message: `participant::list` (all P records)**, under exclusive `.participants.lock` (`routing.rs:103`) | **O(U × P) under the global lock** |
| `send` to workspace/lineage | `route_message` → `resolved_recipients` → `list_active` once (lineage: `members()` + `list_active`, i.e. 2 lists) | O(P) |
| `who`, `doctor`, `channels` | Fixed at 857ab38: one shared `ReceivedIndex` + `MailCounts` + `ChannelRoster` | O(P + R + M) |
| `profile list`, `participant list`, `lineage members` (`lineage.rs:46`), `seen_by` (`chat.rs:2439`) | `participant::list` | O(P) |
| Supervisor, per discovery/refresh | `participant list --json` (needs the 64 MiB cap band-aid) + `channels --all --json` | O(P) each |
| Supervisor, per armed participant per scan | one snapshot, O(P+R+M) | O(armed × P) per interval, which is why `post-hg5` concurrent snapshots contend |

- **U today:** the Mac has 13 unrouted workspace letters (tower-fable 8, prospera-radar 2, three rooms with 1 each). A read by a tower-fable participant does 8 full listings, about 0.25 s holding the global lock.
  - The devbox has 4.3k participants (each listing about 2.4× the cost) and 162 legacy unrouted letters on the store copy (the 857ab38 message). That is up to about 11 s of exclusive lock per read if they sit in one workspace. I did not measure the per-workspace distribution there.
  - Unrouted letters stay unrouted while their workspace has no active participant, so this cost repeats on every read.
- **`tests/scaling.rs`** covers only who, channels and doctor (`:185,:213,:256`). It has no read/catchup/route case.
- **Mac `who` timing (not re-run):** lane-live and lane-repo each measured 30-38 s (about 85% sys) on f784a3b. That is consistent with P × (P stats) = about 3.2M syscalls.

### d. Channel parser rejects unknown event kinds: **CONFIRMED**

- **Post:** `src/channel.rs:1182-1191` (in `validate_channel_message`, called from `parse_channel_message:1109-1122`) raises `ConfigInvalid` for any `event` other than `join`/`profile`.
- **The bridge has the same closed enum:** `post-bridge-wt-channels/post-bridge/bridgelib/channels.py:240` raises `invalid_event`, so an unknown kind would also fail cross-host import.

Readers and their posture on one unknown-kind file:

| Reader | Site | Effect |
|---|---|---|
| `chat` cursor read | `chat.rs:1849-1862` (`fail_closed`) | **Whole read fails** |
| `chat --history/--since` | same, not fail-closed | warn and skip |
| `chat --ack` / `--message` | `chat.rs:271, :324` | fail |
| Consuming transactions (ack, discard-through) | `cursor_state.rs:1202, :1211` | **Blocks cursor advance for the channel** |
| `channels` listing | `channels.rs:38` → `eligibility::unread_channel` → `eligibility.rs:421` | **Whole listing fails** (lane-5 H) |
| `catchup` | `catchup.rs:942` → `unread_channel` | fails |
| `search` | `search.rs:43-64` → `visible_channel` → `eligibility.rs:495` | fails |
| `watch` (live + snapshot) | `watch.rs:1699, :1771, :1937` | emits `unreadable` events. The supervisor turns these into "unreadable in #chan" notices (`doorbell-supervisor.mjs:463-469`) |
| `doctor` | `doctor.rs:719` | reports it as a corrupt message |
| own-message checks, roster, archive | `channel.rs:879/891`, `channel_state.rs:479`, `channel_archive.rs:95` | silently skip |
| bridge import (Python) | `channels.py:240` | raises. Whether the tick then stops or quarantines was not traced |
| External watch parsers | D (supervisor) and B (watch-notice) reject whole batches on an unknown `event` *line*; A and C drop it; I (legacy doorbell) raises; K (Loom) ignores `event` | 4 postures |

### e. flock sites: **one exclusive unbounded global lock sits on the send/read critical path**

| Lock | Site | Mode / bound | Protects | Holders | Critical path? |
|---|---|---|---|---|---|
| `.participants.lock` | `participant.rs:591-611` | EX, **unbounded** | participant records, by-session index, receipts publication, channel state `mutate`, lineage store | `touch` on **every write command** (`mod.rs:94-98`, via `participant.rs:453-460`); bind/end; `route_message` (send); **`route_pending` (read/catchup/chat/watch scans), held for U × P**; `bridge deliver` per letter (`bridge.rs:176`); `channel_state::mutate` (`:666`); lineage ops (`lineage_store.rs:271..639`); `rooms add/set-path/rename` (`rooms.rs:42,130,217`); notice ack/claim | **YES.** The one that can stall an agent for seconds to minutes: a devbox reader in a workspace with a large unrouted backlog, or a rename walking every participant's cursors |
| `.rename.lock` | `mailbox.rs:245-270` | SH for send/read/chat/catchup for the **whole command** (`mod.rs:138-145`); EX for rename (`rooms.rs:216`); SH in doctor --fix and non-read-only watch setup | room directory names | as listed | Only rename waits. `send` reads its body before the lock (`mod.rs:130-137`) but **`chat --send` reads stdin inside `chat::run` while holding SH** (post-4kv), so a stalled producer starves a rename indefinitely (flock has no writer priority) |
| `.rooms.lock` | `mailbox.rs:272-290` | EX, unbounded | rooms.json, profiles, owner | rooms add/set-path/rename, profile set/clear (`profile.rs:95,326`), owner init | No (short) |
| `channels/.lock` | `channel.rs:127-145` | EX, unbounded | channel membership | join/leave (`channel.rs:219`), archive (`channel_archive.rs:143`) | Not on send (the comment says sends never take it) |
| per-participant `.cursors.lock` | `cursor_state.rs:437-479, 482-490` | EX; `Blocking` for consuming transactions, `Within(2 s)` only for the post-channel-send seen update (`e6ee2a9`) | cursors.json | consuming reads, rename | Per participant; short holds |
| per-room legacy cursors lock | `cursor_state.rs:1265` | EX, blocking | legacy room cursors | legacy consume, effectively unreachable (see b) | No |
| migration fence | `migration_fence.rs:192-250` | EX, unbounded, **held for the whole write command** when the store is enrolled (`admit_generation:518-526`) | generation cutover | every write command on enrolled stores (cells); no-op on the Mac (no state file) | Serializes all writers on enrolled cells. Dormant here |

- **Fix:**
  1. Make `route_pending` resolve recipients once per pass.
  2. Move `touch` out of the global lock: it is a single-record atomic replace, and a per-record write needs no registry lock beyond what `bind` already uses.
  3. Give every EX acquisition on user-facing paths a bounded wait with a typed `busy` error naming the lock, generalizing the existing `LockWait::Within`.

### f. Hand-parsers of post output: **CONFIRMED; the count is worse than "each consumer"**

The search enumerated 8 codebases that run post and 7 that parse its output. None shares a parser with another, and the only shared code is inside the post repo (supervisor ↔ its installer).

- **`watch` event parsers: 8 independent implementations:**
  - A, the four harness hooks. These are byte-identical copies, `validSnapshotEvent` at `claude-mail.mjs:334`. Add the installed copy in `~/.claude/hooks`, already out of date, and A alone is 5 places to edit.
  - B `watch-notice.mjs:163`
  - C `codex-notify-monitor.mjs:176-205`
  - D `doorbell-supervisor.mjs:325-365`
  - F `install-codex-doorbell.mjs:338-345`
  - H `envelope-canary.mjs`
  - I legacy Python `doorbell/post-doorbell:151`
  - K Loom `~/Code/loom/src/modules/post/post.ts:1070`

  They disagree on unknown input: D and B reject the whole batch, A and C drop the event, I raises, K ignores `event`.
- **`who` parsers: 3**, all external:
  - Loom `warp/tail.ts:82`
  - porch-tui `presence.py:15`, which reads `rooms[].live_watch`
  - hq `exception-digest.py:195`, which reads `live_watch` and `last_seen`

  `live_watch` is now always false on both hosts (no watch processes; lane-live), so both presence consumers are reading a dead column.
- **`rooms` parsers: 6** (agent-session ×2, install-codex-doorbell, legacy doorbell, post-bridge v1 and v2, porch-tui `mentions.py`).
- **Exact-schema consumer:** porch-tui `roomcheck.py:64` `_require_exact_keys` on `profile show`/`owner show` is the one strict consumer. Adding a field there breaks Porch (it already happened, d00062).
- **Error JSON on stderr** is parsed by the supervisor, Loom and delegate.

---

## PART 2: simplification audit

### Framing

- The evidence doesn't support "post is mostly unused". The 28-message figure is delegate's `.delegate/mail`.
- The Mac store shows channel traffic in the hundreds per day and direct mail at single to double digits per day.
- **What does not earn its keep is the machinery around identity and waking.**
  - One participant is minted per agent session (about 150/day on the Mac, and 2,907 Loom phantoms in about a day on the devbox). 93% of them never do anything.
  - Four wake paths.
  - Legacy cwd-room semantics kept alive for readers only.
  - A self-description (`schema`, help strings) maintained by hand beside the compiled contract.

### (i) Surface table

"Callers" counts `rg` hits for the feature in skills/post/hooks, SKILL.md, references, launcher, doorbell/, tests, docs, README, CONTRACT (my counts), plus the external consumers from (f).

| Surface | Verdict | Reason (caller evidence) |
|---|---|---|
| `send`, `chat` (join/leave/send/read/--history), `channels`, `inbox`, `watch` | **keep** | The core. Channels carry most traffic. `watch --snapshot` is the hook and supervisor contract |
| Deprecated positional `FILE` on `send`/`chat` (`cli.rs:433, :552`) | **delete** | Live trap (`chat ops --send "hello"` opens a file named `hello`; a long body gives ENAMETOOLONG). No consumer uses it: delegate, Porch and Loom all use `--body`/`--body-file`. Retires lane-1 cl.1-2 and lane-4 cl.2 residue |
| Legacy cwd-room inference for **readers** (`resolved_room` in `inbox.rs:191`, `read.rs:31`, `watch.rs:514`, `channel.rs:184-187`) | **delete** (Trey confirms) | Writers already refuse unbound (`mod.rs:67`), so this now only produces misleading empty reads (`ok:true,count:0`). Blast radius: homestead `fc-statusline` bare `post inbox` (already shows 0 when unbound) and unbound devbox agent shells. Keep explicit `--room` for command sinks: the supervisor's command-sink subs, `codex-notify-monitor`, `install-codex-doorbell`, hq `inbox --room` |
| Legacy room consume paths: `cursor_state::consume` room delta, chat `participant: None` consume arms (`chat.rs:300,1007,1541,1586`), `mailbox_dirs` create branch, `read/` dirs | **delete** | Unreachable for writes (they need a participant). Also removes doctor's `read_missing` class |
| `post-doorbell` legacy Python (`doorbell/post-doorbell`, `--own`) + per-agent timer installers (`install-systemd-doorbell.mjs`, `install-codex-doorbell.mjs`, `codex-notify-monitor.mjs`) | **delete** after Trey confirms no host depends on them | The supervisor replaced them. The devbox's 22 timers are off (STATE). The Mac's `dev.post.codex-doorbell.locus` is loaded but not running. `/usr/local/bin/post-doorbell` (Aug 25) is a stale leftover on the devbox. `--own` exists only for the Python doorbell (15 of 21 refs are in doorbell/) |
| Wake paths: hooks (turn boundary) / supervisor (Herdr idle) / Claude Monitor / per-agent timers | **keep hooks + supervisor; demote Monitor to a documented fallback; delete timers** | The Monitor's silent 30-minute expiry is a harness limit post can't fix. Two paths, both owned by post, is the stable shape |
| `schema` vs `contract` | **merge**: `contract schema` generated from the serde types or samples; `schema` kept as an alias for one release | `schema` is hand-written and already wrong (watch shape lacks `cursor_unusable`, `display_name`, `pfp`, `sender_*`; `schema.rs:387-392`, lane-5 F). `contract` is compiled and used by installers (hooks 7 refs) |
| Five ways to read: `read`, `catchup`, `chat`, `chat --history`, `search` | **keep all; unify one rule** | Each has a distinct job (one id / budgeted bulk consume / channel unread / non-consuming view / grep). The friction was consumption semantics (lane-3 cl.5, lane-4 cl.3), not the count. One sentence in help: "only `read` (no `--peek`), `catchup`, and bare `chat <ch>` consume" |
| Five list commands: `who`, `participant list`, `identity list`, `profile list`, `channels` | **keep `who` + `channels` for agents; demote `participant list`, `profile list`, `identity list` to operator references** | `participant list` is the supervisor's discovery feed (D, E), so it stays as machine surface. `who` is parsed by Porch, Loom and hq. **Drop `live_watch` from `who`** (always false now) after porch-tui and hq move to supervisor health, or report doorbell-armed from `doorbell/health.json` instead |
| `bridge` in top-level help | **demote** (clap `hide`) | Its own help says humans and agents never need it. Only post-bridge v2 calls `bridge deliver --json` |
| Global `--json` help string | **fix** | It is wrong for 9 subcommands and ignores `--text` (lane-repo G) |
| Two output defaults (text for send/read/chat/catchup/search; JSON for inbox/who/rooms/...) | **keep, document once** | Changing defaults breaks 7 parsing codebases. Low value per risk |
| Stderr banner under `--json` (`send.rs:168-176`, `chat.rs:2352`) | **delete under `--json`** | It caused a double-send (lane-5 G). The receipt already carries identity |
| Room vs channel (two nouns) | **keep both; add `--workspace` as the canonical flag spelling, keep `--room` as an alias** | The nouns are real (addressed mailbox vs group). A rename would churn 7 consumer codebases |
| `identity` (lineages) | **keep, demote in skill** | 3 lineages / 3 of 1,796 participants on the Mac. Used by routing and adopt. Deletion is a Trey product call, not a papercut fix |
| `delivery` | **keep; widen** | Only the sender participant can query it, so operators can't see stuck letters (lane-live headline 3). Let `doctor` read `bridge/health.json` quarantine/unrelayable lists |
| `owner` | **keep** | Porch's trust anchor (roomcheck.py) |
| `inbox --adopt` help text "(implemented by P.2)" | **fix string** | Leaked project-phase text |
| `participant bind --new` | **keep, but default an ephemeral lease** | Loom mints one per REPL (`post.ts:506`), which produced the 2,907 devbox phantoms |
| Skill (1,169 lines incl. refs) | **split** | Agent SKILL.md: send/read/chat/identity. Operator reference: bridge, rename, supervisor install, residents. Fix the liveness recipe (`who --json` → `participant show`) |
| Four copies of the harness hook (A) | **merge into one shared module + thin per-harness adapters** | Byte-identical logic in 4 files + installed copies. The conflict line fired 5,284 times in 51 sessions (lane-5 I) and needs one fix, not four |

### (ii) Participant lifecycle: the smallest coherent redesign

**Facts that shape it**

- Ids are deterministic: `participant_id(harness, sha256(key), 8|12)` (`participant.rs:716-739`). A record-only participant can therefore be deleted and **re-minted with the same id** by the next `bind` for that session key. That makes GC of empty records reversible.
- 93% of records are empty (`participant.json` + `activation-notice` only).
- The state worth keeping lives in the participant dir (`cursors.json`, `channels.json`, `membership-starts.json`, `inbox/`, `routing/`, `imports/`) or elsewhere: routing receipts in room and lineage stores that name the id, lineage `current`, supervisor subscriptions/residents, and bridge `local-held`/delivery records keyed by `from_participant`.

**States:** `active` (lease live) → `stale` (lease expired) or `ended` → **`archived`** (new) → restored on rebind.

**Default leases:**

- Keep 24 h for harness sessions (hooks touch on every prompt/tool).
- `bind --new` and `bind --harness … --key <uuid>` bootstraps default to an **ephemeral** class (for example a 1 h lease plus an `ephemeral: true` flag), so Loom and test phantoms archive within hours.

**GC rule (one verb, `post participant gc`: dry run by default, `--apply` to act; `doctor --fix` and the supervisor's daily tick may call it):**

- **Tier 1 (delete + tombstone).** Condition: not active, `last_seen` older than R1 (proposal: 24 h for ephemeral, 7 d otherwise), and the dir holds nothing but `participant.json`, `activation-notice`, `.cursors.lock`, and a heartbeat older than R1.
  - It must also hold no lineage, no subscription/resident, and not be named by any routing receipt or pending/provisional mail (answered from one `ReceivedIndex` + `PendingMail` pass).
  - Action: append a tombstone line (id, harness, digest, created, last_seen, workspace, reason) to `participants/archived.jsonl`, remove the by-session entry, and remove the dir.
- **Tier 2 (move aside).** Condition: not active and `last_seen` older than R2 (proposal: 30 d), with state (cursors or channels) but **no unread** (no receipt naming it with an id outside its cursor, no unconsumed mail in its own inbox, no pending/held mail for it, no bridge-held outbound it sent).
  - Action: move the dir to `<root>/participants-archive/<id>/`, outside every scan.
  - Restore: `bind` for that key moves it back before minting.
- **Never:** active lease; fresh heartbeat; unread or pending mail addressed to it; a lineage's current holder; a supervisor subscription; outbound bridge letters it sent that are not yet received or rejected (`delivery` is sender-only).
  - **Frozen unread mail on stale or ended participants** (Mac: 86 letters on 13 participants; devbox: 2,461 on 392) is a policy question. Retain forever, or re-route to the workspace/lineage after N days? **That is Trey's decision.** GC must default to retain.

**Invariants a GC must keep, and the tests that prove them (each red-proofed by a mutation):**

1. **No mail loss.** For every letter in any store, the set of participants that can still read it after GC ⊇ the set before, unless all its recipients had consumed it.
   - Test: seed an unread workspace receipt naming a stale participant → gc → record retained and `inbox` shows it.
   - Mutation: drop the receipt check → red.
2. **Id stability.** For every (harness, key), `bind` after GC yields the same id as before, including the width-8 collision case.
   - Test: two keys colliding at 8 chars; GC the width-8 holder; rebind the width-12 key → still width-12. The tombstone must keep occupying the width-8 id in `select_record`.
   - Mutation: ignore tombstones → red.
3. **Cursor continuity.** A resumed session after a tier-2 archive sees its consumed set restored.
   - Test: consume → archive → rebind → no re-delivery.
4. **Liveness.** GC never touches an active, heartbeating, subscribed or lineage-current participant (injectable clock).
5. **Crash and concurrency safety.**
   - Each participant is archived under `.participants.lock` in small batches: the tombstone is written before the dir move or delete, and the by-session entry is removed after.
   - A kill at any fault-injection point (the pattern in `migration_fence.rs` tests) leaves the participant either wholly live or wholly archived. A concurrent `bind` for the same key either sees the record or re-mints the same id.
6. **Idempotence.** A second `gc --apply` is a no-op, and the dry run and apply lists match.
7. **Bridge.** A remote letter to `participant:<id>@host` for an archived id restores the record or rejects with a delivery-visible receipt, never a silent park (test in `tests/bridge_deliver.rs`).
8. **Reporting.** `doctor` shows one `participants.stale` count line, not one check per participant. `participant show <id>` reports `archived`.

**Persistent incrementally maintained index vs per-command indexes: do not build it.**

- Once GC runs, the scanned participant count P is bounded by active participants plus the retention window (hundreds). Per-command O(P + R + M) is milliseconds (measured 10-40 ms at P = 1,796).
- The store has **non-post writers**: the Python bridge writes channel files and placeholder mail directly (`sweep.py`), and operators quarantine by `mv` (the 1,676 phantoms).
  - A persistent index would therefore need mtime-validated invalidation or become a second source of truth that drifts.
  - `post-aqw.3` already rejected one for watch on the same grounds.
- The one remaining superlinear path (`route_pending`) is fixed by the existing once-per-pass pattern.
- If P ever needs it after GC, add a **derived, deletable cache** keyed on directory mtimes, never authoritative.

### (iii) Ranked changes: best value per unit of risk

| # | Change | Retires | Risk / size | Who decides |
|---|---|---|---|---|
| 1 | **Ship `main` to the Mac and rebuild the devbox from a reachable commit.** Install refuses to install a SHA no branch contains; `post --version` prints the build SHA; install-smoke asserts `who` under 2 s at the live P | lane-repo A (who 30-38 s), D1 (`f784a3b`/`82bfa35` untraceable), lane-live headlines 1, 2, 4 | Low; ops plus a few script lines | Routine (Forgejo). Closing post-gxz is routine |
| 2 | **`route_pending` resolves recipients once per pass** (reuse `resolved_once`); add a scaling test for read/catchup at P = 2k with U = 50 | (e) critical-path global-lock hold O(U×P); lane-3 cl.4 class; part of post-86k | Very low; about 10 lines + 1 test | Routine |
| 3 | **Dangling identity is a typed error** (`participant_missing`, exit 65) from `resolve()`; remove the two swallow sites (`mod.rs:58`, `channel.rs:184-187`); hooks rebind on that code; delete the supervisor's stderr regex | post-b18, lane-5 H 70bc26, lane-live UX 3, lane-repo B | Low; the consumers already handle nonzero (see a) | Routine (the bead already specifies it). CHANGELOG entry |
| 4 | **Doctor honesty.** Drop room `inbox_missing`/`read_missing` (lazy dirs are normal; `read/` is legacy); one `participants.stale` count line; `--severity warn` filter; surface bridge quarantine/unrelayable ids from `bridge/health.json` | "status broken" forever on both hosts (3 and 14 errors), 91-98% info noise, lane-3 cl.9, lane-live headlines 3 and 5 | Low | Routine |
| 5 | **Participant GC/archive + ephemeral leases** per (ii) | Cluster A root, 64 MiB cap band-aid (post-261), supervisor discovery cost, part of post-hg5, post-276 | Medium; new verb and invariants 1-8 | **Trey:** retention R1/R2, frozen-unread policy, auto (supervisor tick) vs manual, whether Loom's `bind --new` gets an ephemeral lease |
| 6 | **Stop minting room dirs.** Remove `sweep.py:689-690` ensure_dir for peer names; make `mailbox_dirs` pure; delete legacy room consume arms | post-6ep (actual creator), doctor `read_missing` class, dead code | Low; cross-repo (claude-space) | Routine; bridge change goes through its own gate |
| 7 | **Tolerant channel event kinds** in post (`channel.rs:1183`) and the bridge (`channels.py:240`): unknown kind = opaque system event (render `[event: kind]`, exclude from unread, never block cursors); one test feeds `future_kind` through chat/watch/catchup/channels/search/doctor | lane-2 cl.7 class; prerequisite for any new kind across skewed hosts | Low | Routine; must ship fleet-wide **before** any new kind is written |
| 8 | **One hook module + warn-once conflict** (shared JS for the four harness hooks; the conflict warning persists `conflictWarned` like `lifecycleWarned`) | lane-5 I (5,284 injections), 4-way hook drift, part of lane-4 cl.7 | Low-medium | Routine |
| 9 | **Generated contract.** `post contract schema` derived from serde types or samples; `schema` becomes an alias; a consumer-inventory check (grep the 8 known consumer repos) before any flag removal | lane-5 F (schema lies; the `--allow-self` removal broke delegate, post-cq8) | Medium | Routine. The post-cq8 remedy itself is Trey's (a delegate-side identity choice) |
| 10 | **Surface diet.** Delete positional FILE; hide `bridge`; fix the global `--json` help and the `--adopt` string; no stderr banner under `--json`; delete cwd inference for unbound readers; delete the Python doorbell, `--own`, and per-agent timer installers; split the skill | lane-1 cl.1-2, lane-4 cl.2, lane-5 G, lane-repo G/H | Low per item | Routine for help, banner and FILE. **Trey** for cwd-inference removal (fc-statusline / unbound shells) and for deleting the legacy doorbell and timer paths (confirm no host still uses them) |

**Not recommended:**

- A persistent index (see ii).
- Renaming "room" → "workspace" across the CLI (churn across 7 consumer codebases for cosmetic gain).
- Changing output defaults.
- A `post` client library for external consumers. Items 7 and 9 plus tolerant readers give most of the value without a cross-language package to maintain.

## Corrections to the analyst lanes

- **lane-repo C "Root cause (VERIFIED)" for post-6ep is wrong.** `mailbox_dirs` cannot create in production. The bridge (`sweep.py:687-690`) is the creator, which the bead had inferred. The proposed "create only for registered rooms" fix would change nothing.
- **lane-repo B / lane-5 H:** "the doorbell supervisor ... looks like no mail" is **partly wrong**. The supervisor scrapes stderr (`doorbell-supervisor.mjs:1588`) and records `snapshot_unbound`. The harness hooks are the consumers that do read it as no mail.
- **lane-repo B's fix is incomplete** as written. An `Err`-returning `resolve()` is still swallowed at `mod.rs:58` and, for read-only chat and channels, at `channel.rs:184-187`.
- **lane-repo A / lane-5 cross-cutting 1** say inbox/watch each run "O(host) through ReceivedIndex::read". Correct, but that is cheap (10-40 ms here). The costly survivor is `route_pending`'s O(U×P) under the global lock, which no lane named.
- **The herd-experiment "28 messages" figure** measures delegate's mail, not post.
