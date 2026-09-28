# Lane: live health and friction of `post` (Mac + devbox agent), observed 2026-09-28 ~17:14-17:25 CDT

Read-only throughout. Mac user `treygoff`; devbox verified `id -un` = `trey-agent`. Raw captures (doctor/who/channels/rooms JSON, summarizer) are in this scratchpad dir: `mac-doctor.json`, `dev-doctor.json`, `mac-who.json`, `dev-who.json`, `mac-channels.json`, `dev-channels.json`, `summ.py`.

Classification key: BY-DESIGN / KNOWN (bead) / NEW.

## 0. Headline findings

1. **NEW: the two hosts run different `post` builds, neither is the build STATE.md names.** Mac `f784a3b` ("temp gate candidate: post-ojh r3 over 7c3e859", 2026-09-25), devbox `82bfa35` (binary dated 2026-09-28 04:44). STATE.md says both run `20fc657`. `82bfa35` is not an object in the Mac repo (`git cat-file` fails), so the devbox is running a build with no traceable commit here. STATE.md "Where things stand" is stale on this point.
2. **KNOWN post-gxz, but the fix is not installed on the Mac.** `post who` on the Mac takes 30-37 s at 1,794 participants (`real 0m36.8s user 4.5 sys 31.7`; second run `real 0m29.98s`, sys 25.6). HEAD contains ec7f3e0/857ab38 ("who/doctor/channels read each store once"), but the Mac binary predates it. The devbox binary has it: `who --json` over 4,299 participants takes **0.36 s**. `post-gxz` is still `open`. Timings on the Mac may be inflated by another lane running `post who --json` at the same time (pid 53116/59670 seen at 99% CPU, load average 7.9), but the sys-time share (~85%) matches the known per-participant walk.
3. **NEW: the bridge reports `ok: true` while 3 Mac-to-devbox letters are permanently stuck, and no `post` command shows them.** The letters are `20260918-015518-1abc11`, `20260918-020917-1a1516` (to hq-devbox) and `20260923-181209-2f27c3` (to cos-devbox). They are quarantined `forged_self` on the devbox and sit as `outbound_waiting` (Mac health) and `quarantined`/`forensic` (devbox health). `post doctor` mentions none of it. `post delivery <id>` only answers for the original sender participant (the current Mac session got `not_found`, exit 66; the unbound devbox got `no_participant`, exit 65). The only way to see the stuck letters is to read `bridge/health.json` and `bridge/log-conditions.json` by hand.
4. **NEW: a mail-carrying stale binary on `PATH` in both hosts' non-interactive contexts, and `--version` cannot tell builds apart.**
   - Mac `~/.cargo/bin/post` is 0.6.0 (Aug 22).
   - Devbox `/usr/local/bin/post` (root-owned, Aug 31) prints `post 0.9.0` but is an old build. It has no `version` subcommand and its `doctor --brief` reports 54 findings against the current binary's 33.
   - `post --version` prints only `post 0.9.0`; the build SHA appears only via `post version`. A hook or systemd unit with a minimal PATH gets the old binary and looks like a current one.
5. **NEW: `post doctor` output is 93-98% info noise, and `--brief` points the reader to a report that is huge.** Mac: 1,335 checks, of which 1,321 are info (1,217 are `participant.<id>.stale`); JSON is 535 KB. Devbox: 3,971 checks, 3,938 info (3,836 stale); JSON is 1.6 MB. `--brief` says "14 findings (run post doctor for detail)"; the "findings" are the 3 errors + 11 warnings, but the detail command dumps everything with no severity filter. Top-level `status: "broken"` is driven by 3 missing directories on the Mac (14 on the devbox).

## 1. Mac (treygoff)

**Binaries.** `which -a post`: `~/.local/bin/post` (inode 730538316, Sep 25 15:25, 6.6 MB) first, then `~/.cargo/bin/post` (0.6.0, Aug 22, inode 648094344). Running: `post version` = `0.9.0 (build f784a3b, store v2; participants,lineages,routing-receipts,cursors-v2)`.

**Processes.**
- Doorbell supervisor: node pid 953, started 08:41:45 today (boot 08:40:55), `post_version "0.9.0 (f784a3b)"`, matches the installed binary. Health: herdr_ok, post_ok, no watch_failures, 1,277 snapshots, 15,121 discoveries. `retired_recent` shows panes retiring normally.
- `lock_helper` python pid 1461: its lock command line dumps a whole python program in `ps` (cosmetic).
- Bridge is launchd `com.treygoff.post-bridge`, interval 15 s, `runs = 1830`, last exit 0, not running between ticks (by design).
- `dev.post.codex-doorbell.locus` is loaded but not running; retired leftovers in `~/Library/LaunchAgents`: `agent-post-cell-bridge.plist.retired-20260925`, `com.treygoff.post-bridge.plist.bak-20260923`, `dev.post.codex-doorbell.tower-astra.plist(.bak-stale-node-...)`.
- **No `post watch` processes at all** (ps and pattern grep). STATE.md's "eight orphaned Loom `post watch` processes" no longer exist. BY-DESIGN consequence: `live_watch: false` for all 1,794 participants and `watch-last-seen=never`, so `post who`'s "live watch" column carries no information now (doorbell replaced watchers; see NEW-6).

**Store.** `~/.claude-mail` 127 MB; 1,795 participant dirs (22 MB), channels 36 MB, bridge 60 MB (launchd.log 2.4 MB + rotated 11 MB, `local-held` 881 dirs), `~/post-relay` 27 MB.
- Participants (who): 1,794 = 1,217 stale + 509 ended + 68 active. Harness mix: codex 946, claude 641, cursor 185, loom 10, grok 9. 1,196 have no workspace; 324 are in a room literally named `workspace`.
- 13 participants hold 86 unread messages, 9 of them stale and 4 ended (biggest: `claude-1215ae9d` ended 2026-09-18 with 42 unread; `claude-3f510603` stale with 14). BY-DESIGN per doctor text ("mail frozen to a stale participant is not reassigned"); KNOWN post-276 (participant GC).
- Rooms 79 registered; channels 49 (+1 archived hidden); 79 legacy rooms.
- `~/.claude-mail` room dirs: 4 unregistered empty dirs (`fleet`, `personal-agent-benchmark`, `santoro-metals`, `testroom`), all empty; `locus` has an `inbox/20260923-061016-eb01b5.mail` + routing json but no `read/` dir.

**Doctor.** `status: broken`, ok false, exit 1, stderr empty, 1.0-1.3 s. Non-info codes:
- error `room.brief-app-mac.inbox_missing`, `room.brief-app-mac.read_missing`: the whole `brief-app-mac` directory does not exist though the room is registered (KNOWN in STATE.md; not the same as post-6ep, which is about empty dirs left behind; here the dir was never made or was removed).
- error `room.locus.read_missing`: **NEW since STATE.md** (STATE only lists brief-app-mac). `locus/` was created 2026-09-23 01:10 with inbox+routing only.
- 11 warnings `profiles.<ws>.legacy_workspace_key` (brief-app-mac, claude-space, codex, home-dashboard, hq, pangram-cli, sol-devbox-codex, sol, tower-fable, trey, wade-discovery). STATE.md says 13; now 11 (fewer, consistent with re-runs of `profile set`). KNOWN/BY-DESIGN.
- info: 1,217 `participant.*.stale`, 77 `legacy_room_state.*.read`, 8 `legacy_room_state.*.cursors`, 18 `cursor_state.*.legacy`, 1 `owner.state`.

**Timing (Mac, installed f784a3b).** `doctor` 0.95-1.26 s; `who --json` 36.8 s and 30.0 s (`who --text` 30 s); `who --room post-repo` 0.2 s; `channels --json` 2.2 s (sys 1.96 s), `channels --text` 1.9 s; `rooms` 0.008 s; `inbox` 0.026 s; `watch --snapshot` 0.037 s; `participant show` 0.005 s. `who` gives no progress output for 30+ s.

**Bridge health (Mac).** `ok: true`, last_fetch/last_push ok, `route_contested 0`, 32 rooms published, `local_held.holds 879 (faults 0)`. Peers configured in `bridge/config.json`: trey (5 rooms), fc, sol. Log noise: `room_retired {host trey, room cos}` is emitted every tick (5,601 times in the current log since 2026-09-25) alongside `fetch` and `health`; `quiet` 14,269 lines. `bridge/launchd.err` contains 8+ repeats of "You have not agreed to the Xcode license agreements" (file mtime Sep 21, so stale, but it shows the bridge's git calls once broke on a macOS toolchain prompt with no doctor/health signal). Backups pile up in `bridge/` (`config.json.bak-*` x6, `chan-tip-fc.bak-20260925`).

## 2. Devbox (trey-agent)

**Binaries.** `~/.local/bin/post` (inode 5713983, Sep 28 04:44:47, 8.5 MB), build `82bfa35`. Also `~/.local/bin/post-f2e6e22.bak` (Sep 26), `/usr/local/bin/post` (Aug 31, root, stale, see headline 4), `/usr/local/bin/post-0.8.0.bak`, `/usr/local/bin/post-doorbell` (Aug 25 legacy) and `~/.local/bin/post-doorbell.legacy-8a71f174` (moved by the install).

**Processes.** Doorbell supervisor node pid 443 (started 08:57, `post_version 0.9.0 (82bfa35)`; matches the installed binary, so no stale-inode process; health has 23 state entries vs 7 on the Mac). `post-bridge.timer` active (every ~15 s; `post-bridge.service` `static`, drop-ins `post-bin.conf`/`accuracy.conf`). No `post watch` processes. Each sweep costs ~1.8 s CPU / ~28 MB (journal), roughly 12% of a core continuously; not a fault, just cost (compare post-hg5).

**Store.** `~/.claude-mail` 179 MB; participants 95 MB (4,301 dirs); channels 39 MB; `~/post-relay` 43 MB; bridge 35 MB.
- Participants (who): 4,299 = 3,836 stale + 326 ended + 137 active. codex 2,139, loom 1,477, claude 621. 3,128 have no workspace; 369 in `lumen`, 341 in `loom`.
- 392 participants hold **2,461 unread** messages, 384 of them stale (e.g. `codex-c9400894` 23 unread since 2026-09-18). KNOWN post-276. Consequence for observers: `post watch --snapshot --room hq-devbox` (no `--limit`) emits **908 events** and `--room cos-devbox` emits 119, including mail from 2026-09-16, so a snapshot from an unbound observer replays weeks of ghost mail (no summary line). NEW-ish UX.
- Rooms 79, channels 46 (+1 archived hidden), channel `pending: 164` for the unbound caller. Message counts differ from the Mac for the same channels (e.g. `build` Mac 912 vs devbox 861, `astra` 82 vs 88, `atlas-ts7` 602 vs 603). I did not establish whether this is lag, the deny list (`devbox-build`, `cos`, ... in Mac config), or a divergence; flag for the bridge lane.
- Empty/stray room dirs: 49 registered rooms with empty inbox+read (expected; mail routes via participants). Unregistered dirs: `Code`, `trey-agent` (both look like cwd-basename-derived), `chuck-improve`, `claude-life-guest`, `definitely-not-a-room` (test artifact with a `watch.heartbeat`), `deslop-tooling-lumen`, `workspace-lumen`. All empty except the heartbeat. Registered rooms with no dir: atlas-ts7-engine, cricket, kettle, moth, plumb-devbox, sill (inbox+read), porch, rowan-devbox (read). This is the post-6ep family but in the opposite direction from "empty leftovers"; the bead only describes left-behind empty dirs.
- The shell has **no bound participant**: every command prints `participant: unbound (run: post participant bind)` on stderr, even with `--json` and even read-only ones, exit still 0. `post delivery` cannot be used at all (exit 65). Binding is a mutator so I did not run it.

**Doctor.** `status: broken`, exit 1, 0.7-1.0 s. 33 findings: 14 errors (`room.<r>.inbox_missing`/`read_missing` for atlas-ts7-engine, cricket, kettle, moth, plumb-devbox, sill; `read_missing` for porch and rowan-devbox), 19 warnings (17 `profiles.*.legacy_workspace_key`, of which `lumen` = 369 participants, `cos-devbox` = 20, `brief-app` = 12, `porch` = 15; 2 `room.<r>.workspace_missing`: astra-perf at `/home/trey-agent/Code/veritas-lanes/astra-perf`, dwp-portals-phase1 at `/home/trey-agent/Code/dwp-portals-phase1`). 3,938 info (3,836 stale, 71 legacy read, 26 legacy cursors, 4 legacy cursor_state, 1 owner). The 54 findings the `/usr/local/bin/post` build reports are from the stale binary, not the store.

**Bridge health (devbox).** `ok: true, quiet: true`, `route_contested 0`, 44 rooms published, `local_held.holds 398 (faults 0)`, `quarantined: 3` and `outbound_unrelayable: [20260901-172552-373e58, 20260901-174219-54a564, 20260901-194313-bc0d3b]` (reason `envelope from must be one path-safe component`, `outbound_ignored` since Sep 1; three unrelayable letters that also never surface in `post` commands). Log rotates at 10 MB (`log.jsonl.1` 10,485,775 bytes, covering 2026-09-23 to today, dominated by `route_contested` 35,486 and `quiet` 19,820 lines). `outbound_typed_skipped ... path: local, to_host: null` fires on every tick for every local participant letter (noise). `bridge.service` shows "activating (start)" at any moment, fine. Repeating journal noise only.

## 3. Contradictions with STATE.md

- "Mac and devbox run 20fc657": both hosts report different SHAs; neither is 20fc657.
- "Bridge ... guard acceptance all true (holds 879/383)": Mac holds 879 as stated; devbox holds are now 398 (was 383). Health `ok: true`, `route_contested 0` still holds.
- "Eight orphaned Loom `post watch` processes on the Mac": none exist now.
- "13 legacy workspace-keyed profiles on the Mac": 11 now.
- "The Mac's doctor errors brief-app-mac...": true, plus a **new** `room.locus.read_missing`.
- STATE.md says nothing of the 3 permanently quarantined Mac-to-devbox letters or the 3 unrelayable devbox letters.
- STATE lists `post-gxz` (who 47 s) as P1-open: fixed in HEAD, installed only on the devbox, still slow on the Mac; the bead is not closed.

## 4. UX surprises hit while running commands

1. `post who --text | head -30` prints, after the first rows, a JSON error on stderr: `{"ok":false,"error":{"code":"io_error","message":"failed to write stdout '<stdout>': Broken pipe (os error 32)",... "retryable":true, ...}}`. A closed pipe is an expected condition for a reader; reporting it as `io_error`, `retryable: true` with a "check that '<stdout>' exists" fix is misleading noise. NEW.
2. Unbound-participant nag on stderr for every command on the devbox agent home, including `--json`, exit 0 (see section 2). The shell on the devbox is the standard "agent shell", so this fires constantly. Partly BY-DESIGN (fix is offered in the message); the chatter itself is NEW.
3. `post watch --snapshot` (unbound, unregistered cwd room `trey-agent`): warns `room "trey-agent" is not registered; snapshot scans nothing and creates nothing` and exits 0 with empty stdout. Same shape as KNOWN post-b18 (unknown explicit POST_PARTICIPANT exits 0 with empty stdout), a second trigger.
4. `post doctor`: exit 1 "findings present" is also returned when the findings are only stale-participant info-plus-3-missing-dirs; `status: broken` reads alarming (headline 5). Exit code semantics match the documented `exit_codes` array; the naming does not match the severity.
5. `who` JSON has `participants[*].workspace` absent for 1,196 (Mac) / 3,128 (devbox) rows and `live_watch: false` for all; the "who" report cannot answer "who is reachable now" once watches are gone (doorbell binding state lives in `doorbell/health.json`, not in `who`). NEW.
6. `post delivery <id>` only works for the sending participant; error `not_found ... only the letter's sender can see its delivery state`. Unbound agents and operators cannot audit stuck letters (headline 3). NEW.
7. The stale Mac `~/.cargo/bin/post` returns its parse failure as JSON (`unrecognized subcommand 'version'`); harmless but it is how the stale binary reveals itself.

## 5. Known-bead cross-reference (from `bd -C /Users/treygoff/Code/post list`)

post-gxz (who slow, fixed in tree, not installed on Mac), post-276 (participant GC: 1,217 + 3,836 stale, 2,547 unread frozen to stale/ended), post-6ep (empty/missing room dirs), post-b18 (watch snapshot exit 0 empty), post-hg5 (watch/snapshot CPU contention; concurrent `who` during my Mac run), post-4hy/post-4kv (rename residue). Everything under "NEW" above has no matching bead in the current list.

## 6. Commands NOT run, and why

`post doctor --fix`, `post participant bind/touch/end`, `post send/join/leave/ack/read/catchup/chat`, `post profile set`, `post identity`, `post rooms add/set-path/rename`, `post bridge deliver`: mutators. `post read` skipped (marks read unless `--peek`; not needed). `post who` on the devbox was not timed under `time` repeatedly (one 0.36 s run plus one data run). `launchctl print` was used only on two labels; no `kickstart`/restart. Minor side effects of my own: on the devbox I wrote and deleted a few dotfiles under `/tmp` (`/tmp/.x`, `.xe`, `.w`, `.we`, `.o`, `.e`) to capture output, and on the Mac one `/tmp/dummy_ignore` (removed); none are in post's stores.
