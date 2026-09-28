# post papercuts: lane-repo (beads, sitrep, docs, skill, CLI surface)

Read-only pass, 2026-09-28. Source HEAD is `f9239eb` on Forgejo `main`. The installed Mac binary is `post 0.9.0 (build f784a3b)`. Evidence tags: **VERIFIED** means I re-checked it against source or a live read-only command today. **INFERRED** means reasoned but not proven. "Type" is one of code bug, design invites the mistake, docs/guidance gap, or environment outside post.

## 0. Headline

1. The project's own record is mostly caught up. All 22 papercuts from the 2026-09-22 triage are resolved except Porch scrolling. The 16 overnight children are closed. What remains is 28 open beads: 3 P1, 4 P2, 21 P3. The oldest was opened 2026-09-16, so nothing is more than 12 days old.
2. The most consequential live problem is not in a bead. `post who` still takes **38 s on this Mac** (1,795 participants; 33 s of it is system time) because the fix for it (`post-gxz`, merged at `857ab38` on 2026-09-27) is on `main` but **not in the installed binary**. The bead is still open, and the skill's doorbell liveness recipe tells agents to run exactly that command.
3. The recurring design decisions behind the friction are:
   - (a) Per-participant and per-store directory walks with no retirement policy, so scans grow with the participant count.
   - (b) "Unbound" is a soft, exit-0 state that hides an identity that was claimed but cannot be resolved.
   - (c) A room name is a directory name, so any stale name can mint a mailbox and a rename is a multi-system procedure.
   - (d) Producers and consumers (installed binary, skill, bridge, delegate, Porch, hooks) drift independently, and post only partly guards against it.

## 1. Beads snapshot

Counts: 146 total, 28 open, 5 in_progress, 113 closed. `bd status` also reports 1 blocked.

- **Open P1:**
  - `post-gxz`: `who` is slow. Fixed on main, bead not updated (cluster A).
  - `post-37k`: Trey's GitHub push/release decision. Open since 2026-09-23, so 5 days waiting on Trey. It blocks epic `post-aqw`, whose 16/16 children are closed.
  - `post-aqw`: the epic itself.
- **Open P2:**
  - `post-276`: participant GC.
  - `post-cq8`: delegate `--allow-self`.
  - `post-b18`: unknown explicit `POST_PARTICIPANT` exits 0.
  - `post-7av`: a decision about Lumen.
- **Open P3 bugs:** `3uq`, `hg5`, `8pn`, `6ep`, `ixq`, `qr6`, `7df`.
- **Open P3 tasks:** `40n`, `4hy`, `4kv`, `4th`, `86k`, `9xf`, `adx`, `b6o`, `gcs`, `hox`, `pe2`, `0jg`, `0m4`, `e2a`.
- **Stale in_progress leases:**
  - `post-y2q` (2026-09-06, "left open deliberately"), `post-ooq` (09-05), `post-14t` (09-16), and `post-e31` (09-23, lease expired 5 days ago).
  - `bd ready` reports 27 ready, so these pollute the work graph. Close them, or release the leases with `bd reclaim`.
- **Missing beads:** the merge message for `857ab38` says two follow-ups were "filed separately". They are (i) a stale build-script manifest that needs `touch build.rs` to rebuild, and (ii) "the one remaining minor" from the Sol re-check. I searched both `bd -C post` and `bd -C hq` (search for `build.rs` and related terms) and found neither. Either they were never filed or they have not synced. The stale-manifest one is a real gate hazard, because a stale manifest can make the skill-manifest check pass or fail wrongly.
- **Tracking drift:** `post-gxz` is P1-open although fixed. `post-b6o` (the bind warning) duplicates item 4 of `issue.md`. STATE.md says "Not re-checked" for two items. `STATE.md` is 5 days stale (last updated 2026-09-23, before bridge channels-by-default, `post-ojh`, `post-0m4`, the who fix and the Loom skill change), and it is gitignored and local-only.

### Recently closed bugs: pattern (last ~40 closed)

The closed bugs are about 60% "silent failure" (a wrong or empty result with exit 0) and about 25% "producer/consumer contract drift". Individual bugs:

- **Contract drift:**
  - `post-c93`, `post-vof`, `post-aqw.4`: the doorbell and native notice rejected typed participant mail.
  - `post-d4p`: Porch launch broke on new profile fields.
  - `post-e23`: hooks baked a version-pinned node path.
- **Silent failure:**
  - `post-jpw`: every seat rendered as the room's profile.
  - `post-0ku`: a join flooded the joiner with the whole history.
  - `post-261`: a participant list over 1 MiB froze the doorbell (fixed by raising the read cap to 64 MiB).
  - `post-aqw.9`: the bridge would have re-exported delivered mail.
  - `post-9op`: self-echo.

## 2. Clusters

### A. The participant store grows without bound, and every host-wide command walks all of it

- **Symptom:** `post who` took 47 s at 3.1k participants and 100 s at 4.1k on the devbox. `doctor` and `channels` also re-listed participants per unrouted message and per channel. The bead `post-gxz` says it "timed out past 30 s at 4,820". The devbox reached 4,820 participants, 2,907 of them loom test phantoms from `bind --new` per REPL session.
- **Status:**
  - **`who`/`doctor`/`channels`: FIXED on main.**
    - Commits `ec7f3e0` and `c0abb14`, merged as `857ab38`. They build one shared received-address index and one per-address count index per command.
    - Reported times on a copy of the live store: who 102.6 s to 0.12 s, doctor 4.2 s to 0.38 s, channels 1.67 s to 0.09 s. `tests/scaling.rs` holds 3 s deadline tests at 2,000 participants.
    - **Not installed here.** The installed binary is `f784a3b`, dated 2026-09-25, which predates the who fix (VERIFIED: `git merge-base --is-ancestor ec7f3e0 f784a3b` fails). I re-measured today: `post who --json` takes 38.1 s wall with 33.0 s system time on 1,795 records. `post who --room post --json` takes 0.06 s.
    - I did not check the devbox. STATE.md says both hosts ran `20fc657` on 2026-09-23, so they should be slow too (INFERRED).
    - `post-gxz` remains open in beads, with no note that it is fixed but not yet installed.
  - **`post-276` participant GC: LIVE, no implementation.** Nothing retires never-used participants. Live evidence on this Mac:
    - `post doctor --json` returns 1,335 checks, of which 1,217 are `participant.<id>.stale` info entries. That is 91% noise, and it is unactionable because nothing can retire them.
    - The overall status is `broken` (exit 1).
    - Leftover `.participant.json.<pid>.<n>.tmp` files from killed writers are also unswept, per the bead.
  - **`post-86k`: LIVE, deferred since 2026-09-16.** `received_addresses` re-reads every receipt per command. It was measured at 0.58 s versus 0.38 s at 10k messages for inbox and watch. `ec7f3e0` built a `ReceivedIndex` for who/doctor/channels. I did not trace whether inbox and watch use that index, so treat the bead as still open (unverified).
  - **`post-8pn`: LIVE.** `unread_channel` (`src/eligibility.rs:355-367`) parses every channel file and only then filters history by id. `unread_channel_skipping_consumed` (`:394`) already filters by id first.
  - **`post-hg5`: LIVE, unprofiled.** 8 concurrent `watch --snapshot` runs each cost about 5x the CPU of one. That caps the participants a host can scan per interval.
  - **`post-9xf`, `post-7df`:** bridge-side O(hosts x rooms) per candidate and per-tick re-hashing. These are cost-only nits.
- **Root cause (VERIFIED for who, doctor, channels):** the shape of the store. Every participant is a directory and every read derives from a full listing.
  - `participant::list` (`src/participant.rs:613`) is called from `who`, `doctor`, `channel_state::roster`, `lineage::members`, `profile`, `participant list`, and `output.rs:2438`.
  - Any per-participant computation that itself scans all stores is O(n^2). The 2026-09-27 commit message reports 23 of every 25 ms per participant were spent in the received-address scan.
  - The design decision behind the bugs is "participants are permanent and per-conversation", with no lifecycle end. `bind --new` mints a record forever. Retirement is by lease, and leases only mark records stale, never delete them.
- **Fix shape:**
  1. Simple root-cause fix: implement `post-276` GC. It should archive reversibly under `.participants.lock` when the lease is expired and there is no mail, cursors, joins, lineage or recent heartbeat, done by `doctor --fix` or a `participant gc` verb. Shorten the default lease for `bind --new` and other test-style binds.
  2. Stop treating a lease-stale participant as a first-class thing to report. Roll the `stale` info entries up into one count, so `doctor` can reach "healthy".
  3. Have the skill recommend `who --room <room>`.
  4. Keep the scaling test as the guard. Add a wider test for watch and inbox (`post-86k`).
  5. Tracking work: install `857ab38`, update the `post-gxz` bead, and add a timing assertion to the install smoke.
  - Band-aids already in place: the 64 MiB read cap in `post-261`, and the manual quarantine of loom phantoms (1,676 moved).

### B. An identity that was claimed but cannot be resolved degrades to exit 0 and "nothing" (`post-b18`)

- **Symptom:** `POST_PARTICIPANT=claude-deadbeef post watch --snapshot` exits 0 with empty stdout and a stderr line `participant: unbound (run: post participant bind)`. The doorbell supervisor sets `POST_PARTICIPANT` explicitly, so a participant whose record disappears looks like "no mail" on every scan.
- **LIVE, VERIFIED today** on a throwaway root.
  - Also verified: `post inbox --json` with an unknown explicit `POST_PARTICIPANT` exits 0 with `{"ok":true,"count":0,"participant":"unbound",...,"room":"<cwd basename>"}`. It silently falls back to legacy cwd-room mode, and the JSON also carries a `participant_fix`.
  - An agent that reads only the JSON would see `ok:true, count:0`.
- **Root cause (VERIFIED):** `Resolved` has only `Bound` and `Unbound`. In `src/participant.rs:376-382`, a set `POST_PARTICIPANT` whose record is missing returns `Resolved::Unbound`. The by-session index path at `:395-404` does the same when the record mismatches. So "no identity" and "claimed identity that does not resolve" are one state. Compare `POST_FROM`, whose env-var help says set-but-invalid is "a loud error, never a silent fallback". `POST_PARTICIPANT` is documented as "never mints a missing record", but not as "silently unbinds".
- **Type:** design invites the mistake (a soft "unbound" state), and the skill and docs are silent on it. `references/watch.md` says snapshot "is read-only even when unbound", which is true but does not say an explicitly named missing id also reads as empty.
- **Fix shape:** add a third state, `Resolved::Dangling(id)` (or return an error), from `resolve()`. Make explicit-env dangling a typed error (`no_participant`, exit 65) for every reader and writer, and keep ambient or unbound at exit 0 for hooks. That one change fixes `watch --snapshot` and `inbox`, and makes the legacy cwd fallback opt-in only when no identity is claimed. The bead's narrower fix (only `watch --snapshot`) would leave `inbox` wrong.
- **Related and LIVE:** `post-b6o`, a warning when a cwd-inferred bind lands in a workspace another lineage already uses. That was the trigger for the byline bug (`issue.md`, resolved by `db6151c` and `7c47a68`). It is only a warning-level safeguard and is still open.

### C. A room name is a directory name (stale names mint mailboxes; a rename is a multi-system procedure)

- **Symptom cluster:**
  - `post-6ep`: after the 09-23 renames, empty `<root>/<old>/{inbox,read}` directories reappeared on both hosts. All seven on the Mac and `trey` on the devbox; zero files in each.
  - `post-4hy`: a rename on a bridged host also needs a hand edit of `bridge/rooms/owners.json` with the bridge paused, or the collision persists with no error on the renaming side.
  - `post-4kv`: three rename follow-ups. A test can pass on a slow send start. `doctor`'s `rename_old_recreated` uses `Path::exists`, so a dangling symlink is missed. `chat --send` still reads its body after taking the rename lock.
  - `post-ixq`: `rooms add` stores the path raw, `//` included, and Porch normalizes it, so its signing cross-check fails.
  - **Live on this Mac today:** `post doctor` reports `error room.brief-app-mac.inbox_missing`, `.read_missing`, and `room.locus.read_missing`. Doctor status is `broken` because of missing empty directories, while `6ep` says empty directories are being created that nobody wants.
- **LIVE:** all of these.
  - `post-ixq` VERIFIED at `src/commands/rooms.rs:98` (`rooms.insert(args.name.clone(), args.path)`).
  - `post-6ep`'s creating path is unproven per the bead, but the code has an obvious by-name creator (below).
- **Root cause:**
  - (VERIFIED) `Context::mailbox_dirs` (`src/mailbox.rs:479-499`) creates `<root>/<room>/{inbox,read}` for any room name that is not mid-rename, whether or not `rooms.json` registers it, whenever the command is not read-only. The comment in `src/commands/watch.rs:499-503` names it as "the legacy branch's `mailbox_dirs`", and watch warns "watching a new empty mailbox" for an unregistered room. So any non-read-only call carrying a stale name recreates the mailbox. Candidates are `inbox --room <old>`, `read`, a non-snapshot `watch --room <old>`, a resident registered with the old name, or a session still holding the old `POST_FROM` pin, which STATE.md ruling 12 says exists.
  - (INFERRED, not proven) the bead's guess that the bridge tick creates them.
  - This is the design decision that "the room name is the directory name" with a lazy create-on-first-touch.
  - Renames need three coordinated systems: `rooms.json`, the mailbox directory, and the bridge `owners.json`, and only the first two are covered by `post rooms rename`.
- **Type:** design invites the mistake. It is mostly not a bug in one function.
- **Fix shape:**
  - Simple: make `mailbox_dirs` create only for registered rooms or an explicit `--create`, and return `unknown_room` otherwise. That stops stale-name minting, including typos, and makes `6ep` and the `doctor` "missing directory" error class disappear together.
  - Then: let `doctor` treat an empty unregistered directory as info, and have `--fix` remove or ignore it.
  - `post-4hy`: give `rooms rename` an `--release-bridge-owner` step (or a bridge `release <name>` verb). The current instruction ("edit the file with the bridge paused") is a manual edit of bridge state.
  - `post-ixq`: normalize lexically on `rooms add`, and on load of existing entries.

### D. Producers and consumers drift, and the guards only partly cover it

- **D1. Installed build provenance and drift (LIVE, VERIFIED).**
  - `post version` reports `build_sha f784a3b`, subject "temp gate candidate: post-ojh r3 over 7c3e859". That commit is on **no branch** (`git branch -a --contains f784a3b` is empty), so an agent cannot check out what it is running. `main` has 4 newer commits (the who fix and the Loom skill change).
  - `post contract skill-manifest --verify ~/.claude-shared/skills/post` returns `verdict: drift`, mismatched `SKILL.md`, exit 1. The Loom paragraph landed after the install. The guard works. What is missing is anything that notifies the agent or the owner when it drifts.
  - The 2026-09-22 sweep already recorded "post version --json lies about what is shipped" (`e64b906` reported for code containing `b58932a`).
  - **Root cause:** the served skill is a symlink into a live checkout (`~/.agents/skill-library/post` to `~/Code/post/skills/post`), while the binary is installed separately. There is no release step that pairs them. (VERIFIED for the symlinks.)
  - **Fix shape:** the manifest guard exists. Run `--verify` in `post doctor` (or the session-start hook) and report drift once. Install scripts should refuse to install from an unreachable commit. Or serve the skill from a copy that ships with the binary.
- **D2. `post-cq8` (P2), LIVE, VERIFIED.**
  - `~/.delegate/src/delegate_agent/notify.py:141` still passes `--allow-self`. post removed that flag in `3d4875f`. `src/` has no occurrence.
  - Every delegate `--notify room:` ping therefore fails with exit 2, `invalid_argument`.
  - The bead already lists the options. Lean is (b): delegate binds its own participant so post's no-self-send rule stays intact. It crosses repos, so it needs a decision from Trey.
  - This is an "API removal with an undiscovered consumer" bug. A cheap prevention is a contract-sample or grep check across known consumers before removing a flag.
- **D3. `post-qr6`.** The bridge cap can be below 1 MiB but `health.json` does not advertise it, so post cannot refuse early and a letter parks forever with no receipt. Same class: a 4 KiB envelope cap, and room and lineage names with no length cap. Fix: the bridge advertises live caps in `health.json`. Requires a non-default config to hit, so low.
- **D4. `post-0jg`:** four bridge doc and installer nits (SPEC deploy-window sentence, install notice on hand-started timer, `localheld` docstring, an untested `rm -rf || :`). Chore.
- **D5. Missed prevention noted in the docs:** the sweep lists "one event schema, several independently coded consumers" as root cause 01. It was fixed by `post contract samples` and the skill manifest. There is still no shared `post` client library. The Python doorbell, the JS hooks, Porch and delegate each re-implement CLI parsing.

### E. Watch and doorbell: mostly fixed, residual edges

- **Fixed since triage (VERIFIED in the explainer and STATE):**
  - The Linux self-wake CPU loop (`fd16003`, `81c0f40`). The triage's "backlog" diagnosis was wrong. 93% down to 1.7% of a core.
  - Cursor read race (`ff20812`).
  - Post-commit cursor lock wait (`e6ee2a9`, 2 s bound).
  - The doorbell supervisor: one per host, default-on, resident support.
  - `watch --reason`.
- **`post-3uq` (P3), LIVE by source.** A participant with no `inbox/` is watched on its anchor non-recursively, so an inbox created afterwards raises no event until the slow pass. Arrival can lag by up to max(10x interval, 30 s). Nothing is lost. `refresh_target_dirs` is at `src/commands/watch.rs:1087` today (the bead cites `:867`, so the line drifted). Fix: re-derive that target's directories when a wake finds an inbox that did not exist at registration.
- **`post-hox` (P3).** Transient read error in the migration fence gives one warning per atomic-rename race. Noise, not wrong results.
- **`post-pe2` (P3).** The idle doorbell monitor is workspace-aggregate, not participant-scoped. It is a decision (should the Codex monitor bind as a participant or stay a workspace bell?), not a bug. It may now be moot: the supervisor replaced the per-agent timers, and the STATE says all 22 devbox timers are off. Confirm and close, or document.
- **Claude Monitor expiry (environment outside post).** The harness caps the Monitor (30 min main, 10 min delegate lanes, "observed 2026-09-22"), and expiry is silent. This is documented in `post-mail-doorbell.md`. No post-side fix beyond a supervisor sink for Claude outside Herdr. The `post-doorbell` supervisor only rings Herdr panes.
- **Herdr dependence (design).** Idle wake outside Herdr is: Claude uses a Monitor, Codex has "no idle wake" (per the doc), Cursor and Grok use their own wrappers. Four different mechanisms are documented for one behavior.

### F. Join, hints, and small contract edges

- **`post-adx` (P3), LIVE, VERIFIED at `src/commands/chat.rs:2134-2137`.** For an already-explicit member running `--join --backlog`, `history_hint` is `post chat <ch> --leave && post chat <ch> --join --backlog`. Every other `history_hint` is a read, so an agent that runs hints blindly will leave, rejoin at the backlog floor, and post a join event. If `--leave` succeeds and `--join` fails, the `&&` leaves the agent out of the channel. Fix: keep `history_hint` read-only and put the sequence in `backlog_rejoin_hint`. Accepted for the ship by ruling 8 and documented.
- **`post-40n` (P3).** `identity continue --acknowledge` does not name the terms digest it acknowledges, so terms changed between preview and acknowledgement are accepted silently. Fix: `--acknowledge <digest>`.
- **`post-gcs`, `post-86k`, `post-hox` (P3, deferred since 2026-09-16 freeze).** Residuals from the participants build. No new reports since.
- **`post-4th` (P3).** `scripts/gate.sh` has no timeout around `cargo test`, so a hanging test hangs the gate. Fix: wrap it in a named-stage timeout.

### G. CLI surface: inconsistencies and redundancy (VERIFIED from `--help` on the installed build)

- **Two opposite output defaults.**
  - `send/read/chat/catchup/search` are text by default with `--json`.
  - `inbox/channels/who/rooms/profile/doctor/schema` are JSON by default, and `inbox/channels/who/watch` also take `--text`.
  - `watch` is NDJSON by default. `doctor --brief` is human-only and conflicts with `--json`.
  - The global `--json` help string is repeated on every subcommand and is stale. It says "Emit JSON for send/read/chat; inbox/rooms/channels/profile/schema/doctor are already JSON", which omits `who`, `catchup`, `search`, `watch`, `delivery`, and ignores `--text` entirely.
- **Flag names do not mean the same thing.**
  - `--from` is the sender identity on `send`, and the start point (`--from now`) on `watch`.
  - The room concept is `--room` (`inbox`, `read`, `watch`, `who`), `--workspace` (`participant bind`), `--to <ROOM>` (`send`), and `--own <ROOM>` (`watch`).
  - `inbox --room` and `read --room` say "defaults to the room containing cwd or cwd basename", which is the legacy behavior. It contradicts the participant-first model the skill teaches.
- **Help text is out of date.**
  - `send --to <ROOM>` is documented as "Registered recipient room", but it accepts `workspace:`, `lineage:`, `participant:` and `participant:<id>@<host>`.
  - `inbox --adopt` says "(implemented by P.2)", which is project-phase text leaked into user help.
  - `send [FILE]` and `chat [FILE]` are "Deprecated positional" but still work. This is a live trap. `post chat ops --send "hello"` looks for a file named `hello`, and a 6,000-character body arg fails as `io_error`/ENAMETOOLONG (sweep report).
  - `post chat --help` documents the stdin body forms but not the read-side stdin refusal (`input_ambiguous`), which only the skill explains.
- **Overlap and redundancy.**
  - `read`, `catchup`, `chat` (unread read), `search`, and `chat --history` are five ways to consume or view mail and channel traffic.
  - `participant list`, `who`, `identity list`, `profile list`, and `channels` all list people or memberships. `who` is the only one that costs O(host).
  - `schema` and `contract` overlap. `contract` (`samples`, `skill-manifest`) is used by installers, and `schema` by agents.
  - `bridge deliver` is listed in `post --help` while its own help says "Humans and agents never need these".
  - `post-doorbell` is a separate Python binary with its own flag vocabulary (`--channel`, `--room`, `--focused`, `--desktop`, `select --pane`). It is absent from `post --help` and needs its own discovery path.
  - Each hook installer hand-rolls its argument parsing (which is why `--help` failed, fixed in `947d3bc`).
- **No delete verbs.** Rooms, channels (archive only) and participants can only be created or hidden. That is the store-hygiene half of cluster A, and it is why `6ep`-style leftovers and `rm` on the store are the only cleanup.
- **Exit codes.** They are well defined by `post schema` (0, 2, 65, 66, 69, 70, 75, 77, 78) and documented. The friction is elsewhere: readers exit 0 when the result is meaningless (cluster B). `doctor` exits 1 for `broken` even if the only errors are auto-creatable empty directories (cluster C).

### H. The skill (`skills/post/SKILL.md` and references)

Size: SKILL.md is 262 lines, and the six references add another 907, for 1,169 lines. That is heavy for a mail tool, and an estimated third of it (my rough count, not measured) is host-operator material (bridge enrollment, rename procedure, doorbell supervisor install, resident commands) that most agents never need.

- **Contradicts or omits CLI behavior (VERIFIED):**
  1. `references/post-mail-doorbell.md` "Is the doorbell alive?" tells agents to run `post who --json` and find their row. On the installed build that is 38 s at 1,795 participants, and about 100 s at 4k. Use `post who --room <workspace> --json` (0.06 s today) or `post participant show`. Even after the fix it prints one row per participant on the host.
  2. `SKILL.md` "Your participant" says that without a binding "writer commands fail and print the fix". True for writers. It is silent that readers (`inbox`, `watch --snapshot`) exit 0 with `participant: unbound` and can fall back to a cwd-inferred room, or, with a claimed-but-missing `POST_PARTICIPANT`, read as empty (cluster B). Agents that see `ok:true, count:0` will conclude "no mail".
  3. It says "When this skill and the binary disagree, the binary is right", yet the served skill can lag or lead the installed binary and only `post contract skill-manifest --verify` notices. Nothing tells an agent to run it.
  4. The self-probe in `post-mail-doorbell.md` (`post send --to participant:<your-id>`) is the only documented way to prove a doorbell works. It costs a real send, and a wrong `--to` writes to the live store. There is no `post watch --self-test`.
  5. Skill documents `post-doorbell` commands that `post --help` never mentions.
- **Missing failure modes agents hit:**
  - The delegate `--notify room` breakage (`post-cq8`) is not mentioned, and neither is the fact that `--allow-self` is gone.
  - The dense stdin rule (`input_ambiguous`, `ssh -n`, `/dev/null`) sits in a bullet and is easy to miss. `post chat --help` does not carry it.
  - Nothing tells an agent when `post doctor` is safe to trust: on this Mac it is permanently `broken`, with three errors and 11 warnings among 1,335 checks, so agents will learn to ignore it.
- **Too long or dense for what it does:** the Herdr paragraph is a single 15-line run-on covering armed defaults, `subscribe`, `enable --focused`, `disable`, `mute`, `status`, `unarmed (ambiguous)`, `select --pane`, and `resident add`. The cross-host section is about 60 lines and mixes agent-facing sends with operator-facing bridge procedure (rename between pausing the bridge and releasing the old name in `owners.json`).
- **Usage evidence that the tool is heavier than the demand.** `thoughts/shared/handoffs/2026-09-21-herd-experiment.md` records that delegate's agent-to-agent mail across 24 devbox repos carried about 28 messages ever, 3 of them peer to peer. The proposal also flags the herd channel as unproven (`post-e2a`, `post-0m4`).
- **Fix shape:**
  - Keep SKILL.md to sends, reads, channels, and identity. Move Herdr supervisor commands, the rename procedure, and bridge operations to operator references.
  - Correct the liveness recipe to `post who --room`.
  - Add one sentence on unbound readers (until cluster B is fixed) and one on drift detection.
  - Update the misleading help strings in section G.

### I. Guidance and rulings already recorded (for the report's context, not friction)

- Pinned rulings that constrain any fix: no auto-send of bare stdin, keep the focused-pane guard opt-in, do not auto-reset cursors, do not weaken canonical-path checks (`diag-A`, `diag-B`). GitHub push and release are Trey's gate (`post-37k`). Astra's judgment carried the overnight design.
- `post-0m4` (2026-09-25): measure the "share evidence, hold conclusions" channel guidance before building a claim feature. Not yet evaluated.

## 3. Scale and performance

What the repo says, and what I measured:

| Item | Repo evidence | Status |
|---|---|---|
| `post who` | 47 s at 3,145 participants; 100 s at 4.1k; commit says 102.6 s at 4,139 on the live devbox store copy. 23 of every 25 ms per participant went to `routing::received_addresses`, which opens every store, and every participant is itself a store. | Fixed at `ec7f3e0`/`c0abb14` (0.12 s). **Not installed**: installed `f784a3b` measures 38.1 s on 1,795 participants (sys 33 s). |
| `post doctor` | 4.2 s (one full participant list per legacy unrouted message, 162 on the store); now 0.38-0.43 s. This Mac: 1.1 s, 1,335 checks, of which 1,217 are stale-participant info. | Fixed on main. Output volume is still the problem. |
| `post channels` | 1.67 s; rereading every participant record, its channel state and `members.json` once per channel. Now `ChannelRoster` loads once (0.09 s). This Mac: 2.2 s on the installed build. | Fixed on main, not installed. |
| `post watch` | The original CPU report (49-67% of a core) was diagnosed as backlog-proportional. That was wrong: on Linux the watch woke itself by reading its own cursor file (`fd16003`). Full scan cost at 10k messages is about 1.3 s. `post-aqw.3` ruled the index unnecessary. | Fixed. `post-3uq` lag is minor. `post-hg5`: 8 concurrent snapshots cost about 5x CPU each (contention, unprofiled). |
| inbox / watch at scale | `post-86k`: 0.58 s versus 0.38 s at 10k messages, from per-command receipt re-reads. Deferred since 2026-09-16. | Open. |
| doorbell | `post-261`: a participant list over 1 MiB froze discovery on the devbox. Fixed by raising the read cap to 64 MiB. | Band-aid on growth, not on the cause. |
| bridge | `post-9xf` (O(hosts x rooms) per candidate, fine to about 1,000 letters), `post-7df` (re-hashing an oversize blob each tick). | Cost-only, watch tick duration. |

**The design decision that keeps generating these bugs:** participants are permanent per-conversation directories, host-wide commands derive their answers by walking them (per participant, per store, per channel, per message), and nothing ever retires a record. Each fix (index per command, size cap) makes one command cheap and leaves the count growing, so the next command that walks per participant is quadratic again. The two structural answers are `post-276` GC to bound n, and a persistent, incrementally maintained index (received addresses, per-address counts, channel roster) instead of a per-command index. `post-aqw.3` measured and rejected the persistent watch index for watch, but it did not address host-wide reports. Until GC ships, a devbox running loom test suites will regrow the store (2,907 phantoms in about a day).

## 4. Suggested order (for the report)

1. Install `main` (`857ab38` and later) on both hosts, note it on `post-gxz`, and close it. Add a `post who` timing probe to the install smoke.
2. Cluster B: introduce a distinct "claimed but unresolved" identity state that fails loudly (`post-b18`, covers `inbox` too).
3. Cluster A: `post-276` GC and the doctor stale-participant roll-up.
4. Cluster C: `mailbox_dirs` create only for registered rooms, which fixes `6ep` and the doctor missing-directory errors together. Then `post-4hy` and `post-ixq`.
5. `post-cq8` (a decision for Trey: delegate binds its own participant).
6. Skill diet and the doorbell liveness fix. Rewrite the stale help strings. Clean the stale `in_progress` beads and file the two missing follow-ups.
7. Leave P3 residuals (`hox`, `gcs`, `pe2`, `0jg`, `9xf`, `7df`) until they are reported again.
