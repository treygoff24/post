# post: papercuts report, 2026-09-28

Prepared for Trey. Nothing has been fixed; this is diagnosis and recommendation only.

## Bottom line

Most of the pain agents logged over the last ten weeks is already fixed. Almost all of it traces back to one design mistake, identity being a *place* (a directory or room name) instead of a *thing*, and the participants redesign of 09-16 killed that root. What is still hurting is a different set of problems, and they are more structural than any single papercut:

1. **Nobody can say what is actually running.** The Mac runs a build that is on no branch. The devbox runs a different build that is not in the Mac's repo at all. The sitrep names a third. The fix for the 30–38 second `post who` is on `main` but not installed on the Mac.
2. **The tool succeeds silently with a wrong or empty answer** in at least six places. A typo'd identity reads as "no mail." The bridge says `ok: true` with six letters permanently stuck. `post doctor` says `broken` on both hosts forever and is 93–98% noise, so agents learn to ignore it.
3. **One unknown channel event kind wedges a channel for every reader** (reproduced today). Bridged channels now sync between hosts by default, and the two hosts run different builds, so the first time anyone adds a new event kind this is a live hazard.
4. **Identity minting has no lifecycle.** 93% of the Mac's 1,796 participant records have never done anything. That population is what made `who` quadratic, and one path on the send/read critical path is still superlinear under a global lock.
5. **The bridge has no terminal states.** A refused letter stays "waiting" forever, invisible to the agent that sent it, and room names are host-local strings used as cross-host identity.

My recommended order is at the end. The short version: fix deployment discipline first (cheap, and it currently hides everything else), then make degraded states loud instead of silent, then bound the participant population, then do the surface cleanup.

## What this rests on

- **Ledger cuts:** 3,386 entries on the Mac and 1,286 on the devbox were searched by tag and keyword for post-related text. That gave **215 unique cuts** (145 Mac, 122 devbox, 52 on both), from 2026-07-15 to 2026-09-27: 169 resolved, 32 open, 14 archived; 46 major, 169 minor. Reporters were about half Codex, half Claude Code. A cut that never mentions a post command or the word "post" would have been missed by the filter.
- **Ledger "resolved" is not evidence.** Every analyst checked current source instead. Several cuts were marked resolved by a workaround; several open ones were already fixed in code.
- **Live state:** read-only probes of both hosts (the devbox as its agent user).
- **Reproductions:** 11 scenarios run against a throwaway store with the *installed* Mac build (six commits behind `main`). Every command ran with a cleared environment and its own store; I checked afterward that nothing under the real store was touched.
- **Method:** seven Sonnet analysts (five on ledger slices, one on the beads backlog, docs, skill and CLI surface, one on live state), then three Opus investigators (bridge root causes, reproductions, and an independent check of the structural claims plus a simplification audit).
- **Two small slips to disclose:** the live-state and bridge investigators each briefly wrote a scratch file under `/tmp` on the devbox and deleted it. Nothing else was written outside scratch space.
- The per-topic detail behind every section is in `thoughts/shared/evidence/papercuts-2026-09-28/` (file list at the end).

## The root causes, in order of leverage

### 1. Deployment: no one can tell what is running (live now)

- **Mac** runs build `f784a3b`, labelled "temp gate candidate." That commit is on **no branch**. `main` has six newer commits, including the `who`/`doctor`/`channels` scaling fix.
- **Devbox** runs `82bfa35`, which does not exist in the Mac repo. The sitrep says both hosts run a third build. The devbox's `who` takes 0.36 s at 4,299 participants; the Mac's takes 30–38 s at 1,794. The fix is real, just uninstalled on the machine you use most.
- **Stale binaries on PATH.** The Mac has a 0.6.0 build in `~/.cargo/bin`; the devbox has a root-owned August 31 build in `/usr/local/bin` that prints `post 0.9.0` and has no `version` subcommand. `post --version` prints no build ID, so a hook with a minimal PATH runs the old binary and looks current.
- **The skill is served from the live repo checkout**, while the binary is installed separately. `post contract skill-manifest --verify` currently reports drift on the Mac. The check exists; nothing tells an agent or you when it fails.
- The scaling bead is still open and the sitrep is five days stale.

**Root cause:** there is no release step that pairs binary, skill and expected state, and installs from unmerged "candidate" commits are allowed. The installer's checks are good; they just do not refuse an unreachable commit and do not run at agent start.

**Fix:** the installer refuses any commit no branch contains; `post --version` prints the build ID; `doctor` runs the skill-manifest check and warns about extra `post` binaries on PATH; the install smoke test asserts `who` under a couple of seconds at the live participant count. Then install `main` on both hosts and close the scaling bead. Small, low risk, and it removes the biggest source of "is this bug real?" confusion.

### 2. Silent success with a wrong answer

One rule keeps getting violated case by case: *a command should not report success when its answer is meaningless.* Each command picked its own posture at the call site. Observed or verified:

- **A claimed identity that does not exist reads as "no mail."** With `POST_PARTICIPANT` set to a missing record, `inbox` exits 0 with `count: 0` (falling back to the working-directory room), `watch --snapshot` exits 0 and shows that room's events, while `send` fails with exit 65. Nothing names the bad id. Reproduced. (Open bead on `watch --snapshot` only; `inbox` is affected too.) The cause is that `resolve()` returns the same "unbound" state for "no identity" and "claimed identity that does not resolve." An independent check found two more places that swallow the error, so an error-only fix at `resolve()` would not be enough. The four harness hooks currently treat this as "no mail"; the doorbell supervisor scrapes the word "unbound" from stderr to compensate.
- **The bridge reports `ok: true` with mail stuck** (root cause 5 below). No `post` command shows it.
- **`post doctor` cries wolf.** Status is `broken` on both hosts permanently (3 errors on the Mac, 14 on the devbox), all from "missing empty room directories" that are normal. Of 1,335 Mac checks, 1,217 are stale-participant info lines. There is no severity filter.
- **A corrupt message file in a channel** makes `channels` and `search` fail entirely for members (exit 78), and makes `watch` print the same warning four times per scan. Reproduced.
- **`post read <id>` with piped input** exits 0, consumes the mail and silently discards the input; `chat` refuses the same input. Reproduced.
- **`--json` with stderr merged** (`2>&1 | jq`): the identity banner breaks the JSON, then post exits 70 *even though the send landed*, which is exactly how a double-send happened on 09-23. Reproduced.
- **A watch that keeps running after its participant changed** (below, section 7) never rings for the new workspace.
- `post who --text | head` prints a "retryable io_error" for an ordinary closed pipe.

**Fix shape:** one written rule (explicit identity that does not resolve is a typed error; a read-only listing never fails on one bad item and says what it skipped; a machine-visible marker whenever output is degraded, following the good example already in the cursor-unusable marker), then apply it at each site. That closes the identity cases, the corrupt-file cases, the stdin case and the `--json` banner together.

### 3. Closed formats across skewed hosts (the sleeper)

Any channel message event other than `join` or `profile` is rejected as `config_invalid`. On the throwaway store, one hand-written `future_kind` event caused: every plain read, `--peek`, `--discard` and `channels` call to fail with exit 78; the cursor could not advance past it; every send was refused, and the refusal's suggested fix ("catch up with `post chat`") is the command that fails. Only `--anyway` on each send, or deleting the file, gets out. The bridge's channel importer has the same closed list.

Additive *fields* are tolerated; additive *values* are not. Bridged channels now sync by default and the two hosts run different builds, so this stops being theoretical the day someone adds an event kind. No bead covers it.

**Fix:** unknown kind becomes an opaque system event (rendered as a generic line, excluded from unread, never blocks the cursor), in post and the bridge, with one test that feeds a future kind through every reader. It must be installed fleet-wide **before** any new kind is written.

### 4. Identity minting has no lifecycle

- **Population:** the Mac has 1,796 participant records against about 1,040 direct letters ever sent. 1,677 (93%) hold nothing but the record itself. Only 44 have ever consumed anything, 19 joined a channel, 3 have a lineage. Every record has a 24-hour lease, and nothing ever archives or deletes one. The devbox has 4,301; 2,907 of the devbox's records were once test phantoms from Loom creating a new participant per REPL session (about 1,676 have since been moved aside by hand).
- **Consequence:** every command that lists participants is proportional to that count, and each earlier fix made one command cheap while the count kept growing. `who` was quadratic (47 s at 3.1k, 100 s at 4.1k on the devbox). It is fixed on `main`.
- **One superlinear path survives** and nobody had named it: when reading, catching up, chatting or watching, post re-lists every participant *once per unrouted message* while holding the exclusive, unbounded participant lock. Today that is a fraction of a second on the Mac (13 unrouted letters); the devbox store copy had 162 unrouted letters at 4.3k participants, which is up to roughly 11 seconds of exclusive lock per read if they share a workspace. That lock is also taken on every write command's presence update, so it can stall an agent's send behind another agent's read.
- **Not the problem:** per-command "read everything once" is cheap now (10–40 ms at 1.8k participants for `inbox` and `watch --snapshot`). I do **not** recommend a persistent index: the bridge and operators write into the store directly, so an index would become a second source of truth that drifts, and a bounded participant count makes it unnecessary.

**Fix:** (a) resolve recipients once per pass in the routing step (about ten lines plus a scaling test; low risk); (b) participant cleanup with two tiers: delete-and-tombstone records that hold nothing, and move aside records that have state but no unread mail. IDs are deterministic, so an empty record can be re-created identically; (c) short "ephemeral" leases for `bind --new`. The design, the eight invariants cleanup must keep, and the tests that would prove them are in the structure file. **Your decision:** retention windows, and what to do with frozen unread mail on stale participants (86 letters on the Mac, 2,461 on the devbox). Cleanup must default to retaining it.

### 5. The bridge has no terminal states, and names are not global

Root causes were traced in the bridge code (the installed copy matches a branch in the claude-space repo; the `main` branch there is a stale v1).

- **Six stuck letters.** Three Mac→devbox letters carry `from: brief-app`. That name is a real room on the devbox too, because the hq repo is checked out on both hosts and each registered its own `brief-app`. The devbox correctly refuses a peer claiming a name it owns (`forged_self`). But the sender only removes an outbox letter when it sees a `delivered` receipt, so a refusal has no end state: it is re-checked every tick, logged once, counted in no health flag, and never reported back to the sending agent. The three devbox letters are local test letters with `from: lane/...`; the bridge validates the sender name *before* checking whether the recipient even needs relaying, so letters that never needed the bridge are flagged forever.
- **Recoverable**, out of band: re-send the one that still matters (the layout note to the Lumen room) from `brief-app-mac`, then pause the Mac bridge and remove the three outbox files by name. I did not run this; the exact steps are in the bridge file.
- **`ok: true`** only gates on store faults, git failures, collisions, channel divergence and stale fetch. `post doctor` and `post who` never read bridge state.
- **Channel history divergence** (e.g. `build` 912 on the Mac vs 861 on the devbox): not lag and not the deny list. The bridge silently skips any message whose sender name is no longer a room the host owns. After the 09-23 renames, 189 `atlasos`, 61 `agent-memory` and other old posts on the Mac, and 107 posts from `trey` plus 32 from `hq` on the devbox, can never publish. Nothing sent since 09-25 diverges.
- **`room_retired` every tick** (5,601 times): the devbox's channel deny list contains `cos`, and a denied channel name also unpublishes a same-named *room*, so the Mac's placeholder reads as retired and that event is not deduplicated.
- **Correction to the earlier picture:** the empty room directories after renames (open bead on the old names) are created by the **bridge** on every tick for every peer room, not by post. Deleting post's by-name creation code would fix nothing, because in production it cannot be reached.
- **Per-tick cost grows with history** (open bead on the O(n) scan, wider than filed): full ticks re-read every archive letter and open every channel message before checking whether it was already handled. The Mac has about 1,040 letters and 8,732 channel files; median tick to health is 3 s, worst 803 s. Nothing is ever garbage-collected (`local-held`: 879 records on the Mac, 398 on the devbox).
- **Log design:** a fsync per line, a duplicate copy to launchd/journald, a single 10 MB rotation, and a heartbeat line for every quiet tick. The large `route_contested`/`quiet` counts are mostly old (all before 09-23 08:00Z), so they overstate the current problem.
- **Litter:** the `config.json.bak-*` files are hand backups made before editing pin lists; every pin is now redundant.

**Fix shape (in order):** (1) an `attention` list in the bridge's health file that `post doctor` and `post who` read, with each item carrying its own exact fix; (2) when the sender sees a terminal refusal, write a system letter into the *sending room* ("your letter X was refused: forged_self; re-send as brief-app-mac") and retire the outbox entry, so the agent that sent it learns through the mail tool; (3) check routing before validating the sender name; (4) one durable "decided" marker per letter so ticks only touch new ids; (5) stop channel deny from unpublishing rooms, deduplicate `room_retired`, drop quiet-tick logging and the duplicate stdout stream; (6) on a bridged host, `rooms add` should default to a host-qualified name or refuse a name a peer publishes; delete the pin lists and let the config become static.

### 6. The agent-facing surface: plausible guesses parse, then fail late

The dominant papercut *shape* in July–August, and most of it is fixed by better errors. What still bites:

- **Shell quoting of `--body`** (six reports in eleven days, one posted 127 KB of command output into a channel). Post cannot see what the shell ate. Documentation did not stop it, and the skill's own teaching example and several of post's error hints still use the fragile double-quoted `--body` form. Fix: teach stdin or `--body-file` everywhere post itself suggests a command, demote `--body` to "short, single line, no `$`, backtick or apostrophe," and add a rule to the shell-safety hook (the only layer that can see the command string).
- **The deprecated positional `FILE` on `send` and `chat`** is why `chat ops --send "hello"` opens a file named `hello` and a long body fails with a filename-too-long error. Nothing in the estate uses it. Delete it.
- **Room vs channel:** `send --to <channel>` was fixed; the reverse is not. `chat <room-name>` says "not a member, join first" or "does not exist, create it with `--join`," which would mint a stray channel and never mentions `post send`. The two read errors also disagree on exit code (65 vs 66). Reproduced.
- **The crossed-send guard hands out its own bypass.** Its suggested fix is `--anyway`. If the addressed message is beyond the first 25 unread, `--history`, `--peek` and a single page do not clear it; only paging until nothing remains does (reproduced). So the "permanent block" cuts were guidance failures, not a code lock, but the wording ("body on stdin") is also wrong for `--body` sends. Fix: name the ids and the exact read that clears them; keep `--anyway` as the last resort.
- **Vocabulary:** `who` shows a lease as `active`, and someone held a launch 30 minutes believing an auditor was reading. The text output now carries a hint; the JSON value is unchanged.
- **Small, still live:** `post read <channel-id>` points at `--history` when `--message <id>` exists; `participant bind --key` requires `--harness` though its help implies a default; global `--json` help is stale for nine commands; `inbox --adopt` help contains leaked project-phase text; `--room`, `--workspace`, `--to` and `--from` mean different things on different commands; `#name` is rendered by `channels` but rejected by `chat`.
- **The skill is 1,169 lines with its references.** The doorbell liveness recipe tells agents to run `post who --json`, which is the 38-second command. It does not say readers can exit 0 when unbound, and it mixes operator procedures (bridge enrollment, renames, supervisor install) with agent basics.
- **Direct mail has no reply link or crossed-mail check** (channels do). One observation only.
- **A misposted message cannot be retracted.** Five reports of posting as the wrong agent (all before the redesign). This is a product decision, not a fix.

### 7. Wake paths and watch behavior

- The spin bug (a watch waking itself by reading its own cursor file on Linux) is fixed and installed since 09-23. The concurrency cost remains (eight simultaneous snapshots cost about five times the CPU each).
- **Re-arming a watch replays everything still unread.** By design (watch never consumes), but the digest word "new" means "unread," and the recommended Monitor recipe re-arms every 10–30 minutes. Not reproduced as a bug in four variants, so the earlier "replays already-read mail" cut looks like a misunderstanding of this.
- **Observed (new):** a long-running `watch` snapshots its workspace at startup. After a participant is rebound to another workspace it keeps running but never rings for the new workspace's mail, and after `participant end` it keeps running with no notice. The earlier "silent death after rebind" cut may be this. Fix: re-resolve on the slow pass and exit or warn with a named message.
- **Four wake mechanisms** exist for one behavior (hooks at turn boundaries, the Herdr-only supervisor, the Claude Monitor, per-agent timers plus a legacy Python doorbell). The Monitor's silent 30-minute expiry is a harness limit post cannot fix. `who`'s `live_watch` column is now always false on both hosts (no watch processes run), yet Porch and the hq digest still read it.
- **Hook conflict line:** `POST_PARTICIPANT conflicts with this hook session key` was injected 5,284 times across 51 Codex sessions, with no once-per-session memory. It is the same logic copied into four byte-identical hook files.

### 8. Post describes itself by hand, and eight parsers read it

- `post schema` is hand-written and already wrong (the `watch` shape omits several fields the events carry). The `contract samples` and install-smoke checks exist and work when run.
- Eight independent parsers of `watch` events (four identical hook copies plus the notice hook, notify monitor, supervisor, legacy doorbell, canary and Loom) handle unknown input four different ways. Three read `who`; six read `rooms`. Porch runs one strict exact-key check that has already broken once on an added field.
- The `--allow-self` flag was removed with no inventory of callers, so delegate's completion notification has failed since then (open bead; needs your call on how delegate should identify itself).
- Two beads named in the last merge message ("filed separately," one about a stale build manifest that needs `touch build.rs`) do not exist in either bead database.

## Reproduced today (installed build; none of these had a bug entry)

| Finding | Severity |
|---|---|
| One unknown channel event kind blocks reads, `channels`, catch-up and every send | high |
| `post read <id>` with piped input consumes mail and discards the input | medium |
| A corrupt message file fails `channels` and `search` for members, warns four times in `watch` | medium |
| `--json` with stderr merged and piped into `jq` breaks the parse and exits 70 though the send landed | medium |
| `chat <room-name>` steers toward creating a stray channel | low-medium |
| Long `watch` keeps a stale workspace after rebind or `participant end` | medium |
| Crossed-send refusal names the bypass, not the read that clears it | low-medium |
| Bound `watch --room <unregistered>` gives no warning and exits 0; unbound `inbox --room x` exits 0 while bound gives `not_found` | low |
| `participant bind --key` fails without `--harness`, contradicting its help | low |
| Text mode still prints JSON errors on stderr | low |

## Corrections to what the analysts first said

I had an independent Opus pass check the structural claims, and it overturned three:

- **The empty room directories are not created by post.** The first analysts named a code path in post; it cannot be reached in production and I could not trigger it. The bridge creates them.
- **The "about 28 messages ever" figure** in the herd-experiment note counts delegate's own mail across 24 repos, not post. The Mac store shows 1,040 direct letters since mid-July and about 9,000 channel messages, 100–900 a day lately. Channels carry the load, so "post is barely used" is the wrong premise for a deletion argument. What is out of proportion is the identity and wake machinery, not the mail.
- **The doorbell supervisor does not read an unresolved identity as "no mail"** (it scrapes stderr for "unbound"); the four harness hooks do.

## Recommended changes, ranked by value per unit of risk

| # | Change | What it retires | Risk | Whose call |
|---|---|---|---|---|
| 1 | Install `main` on both hosts from a reachable commit; installer refuses unreachable commits; `--version` prints the build ID; doctor checks skill drift and extra PATH binaries; smoke test times `who` | The 30–38 s `who`, the untraceable builds, stale-binary confusion | low | routine |
| 2 | Routing resolves recipients once per pass; add a read/catch-up scaling test | The remaining superlinear path under the global lock | very low | routine |
| 3 | Unresolved identity becomes a typed error (fix both swallow sites); hooks rebind on that code; drop the stderr scrape | Silent "no mail," open identity bead, the `inbox` variant | low | routine |
| 4 | Tolerant event kinds in post and the bridge, one test through every reader, shipped fleet-wide before any new kind | The channel-wedge hazard | low | routine |
| 5 | Doctor honesty: drop the missing-room-directory errors, roll stale participants into one count, add a severity filter, show bridge attention items | Permanent "broken," noise agents learn to ignore | low | routine |
| 6 | Bridge terminal states: attention list, bounce to the sending room, route-before-validate, decided markers, log and deny-list cleanup, host-qualified default room names | Stuck-and-invisible mail, empty-directory beads, tick cost | medium, spans repos | routine, through the bridge's own gate |
| 7 | Participant cleanup plus ephemeral leases (design and tests in the structure file) | The root of the scaling family, the 64 MiB read-cap band-aid | medium | **you:** retention windows, frozen unread mail, automatic vs manual, Loom's leases |
| 8 | Surface diet: delete positional `FILE`; no banner under `--json`; hide `bridge` from top-level help; fix stale help strings; stdin guard on `read`; teach stdin/`--body-file` everywhere; hook rule for backticks and `$` in `--body` | The most repeated agent mistakes | low each | routine |
| 9 | One shared hook module (warn once), and generate the contract from the types so `schema` cannot lie; check known consumer repos before removing a flag | Hook drift, 5,284 injections, the removed-flag breakage | low-medium | routine; delegate's identity choice is yours |
| 10 | Delete legacy pieces: cwd inference for readers, legacy room consume paths, legacy Python doorbell, per-agent timer installers | Dead code that only produces misleading reads | low | **you:** confirm nothing depends on them (the Mac status-line script runs a bare `post inbox`; one loaded Codex doorbell job) |

**Deliberately not recommended:** a persistent index (see section 4); renaming "room" to "workspace" across the CLI (seven consumer codebases, cosmetic gain); changing output defaults; a shared client library for other repos. Tolerant readers plus the generated contract get most of that value.

**Decisions only you can make:** participant retention and frozen-unread policy; whether to delete cwd inference for readers and the legacy doorbell paths; how delegate identifies itself so completion pings work; whether to add a retract verb (or accept append-only) and whether direct mail gets reply links or should be documented as one-shot; and the standing GitHub push and release ruling. One dependency worth knowing: the tolerant-event-kind fix should be in whatever you push and install before anyone adds a new event kind.

## Ledger and tracking housekeeping

- Resolve as fixed in the ledger: the profile-sharing cuts (fixed three days after they were logged), the phantom-unread cuts, the `who` slowness cuts once the fix is installed, and the watch CPU cuts.
- Four beads sit "in progress" with expired or abandoned leases; the scaling bead is fixed but open; the sitrep is five days stale and gitignored.
- **Has no bead:** unknown event kinds, the `chat <room>` hint, a retract verb, the missing "you are bound to workspace W" hint on not-a-member, the `read <channel-id>` hint, the stale bridge spec pointer in the skill, `read` stdin guard, corrupt-file handling in `channels`/`search`, the bridge attention/bounce work, the PATH shadow check, the stale-workspace watch, and the two "filed separately" follow-ups.

## What is not verified

- Four cuts remain unexplained: a watch dying about seven seconds after a rebind, a digest re-emitting an already-read message, a hook-announced mail id "not found" after the redesign, and a channel `--join` that created a second same-named channel across hosts (probably fixed by bridge sync on by default, deployment unchecked).
- A tiny race where the cursor file is renamed between open and stat (inferred from reading, not testable).
- The reproductions used the installed build, not `main`; `main`'s scaling fix was checked by its tests and by the devbox timing, not re-run on the Mac.
- The devbox build `82bfa35` could not be traced from here.
- Bridge behavior when its channel importer meets an unknown event kind was not traced (whether it stops or quarantines).

## Detail files

All in `thoughts/shared/evidence/papercuts-2026-09-28/`: five ledger slices (`ledger-1` through `ledger-5`, by date range, each cluster with cut ids, status and evidence), `backlog-cli-skill.md`, `live-state-both-hosts.md`, `bridge-root-causes.md`, `repro-throwaway-store.md` (exact commands and outputs), `structure-and-simplification.md` (lock table, consumer parsers, surface keep/merge/delete table, cleanup design), and `all-215-cuts.json`.
