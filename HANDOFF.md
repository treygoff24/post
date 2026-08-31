# HANDOFF — Plan B workflow mid-run, babysit watch active

Written 2026-08-31 ~19:50Z at a 5hr-limit account rotation. Transient: the
session that consumes this deletes it (or rewrites it at its own closeout).
Work graph truth is beads (`post-esa` epic); narrative is this file + bead
`post-rsq` comments + the babysit log at `~/tmp/plan-b-babysit-log.md`.

## Where things stand

**Plan B (stateful read layer) is EXECUTING** as delegate workflow
`wf_9484b4e85ae6`, launched from the integration worktree
`.worktrees/plan-plan-b-stateful-read-layer` (branch
`plan/plan-b-stateful-read-layer`). 9 tasks, 6 waves. Progress at handoff:

- **B1 (cursor_state module) DONE and merged green** — commit `0acef93` (merge
  of `809fb9a`), 4/4 verify rows (cargo test cursor_state, full suite, clippy
  -D warnings, fmt). Panel: opus + sol + omp-retry. Adjudication: decision
  `fix` — one ADOPTED blocker **B1-R1** (mode-000 messages dir discards the
  parsed v1 baseline → permanent cursor loss; lane also deleted the base test
  guarding it). **fix:B1 had not yet appeared in the journal at handoff** —
  first thing to verify on resume.
- **B2 + B3 executing** (wave-2 pipelining off B1's merge; codex/luna lanes).
- **close:1 gate** rejected once for unacknowledged coordinator work, then
  **CO-B1-1 and CO-B1-2 were acknowledged** (plan-unpark, at 0acef93) —
  close-retry:1 was in flight at handoff.

## Resume ritual (next session)

1. `cd /home/trey-agent/Code/post/.worktrees/plan-plan-b-stateful-read-layer`
   (workflow commands ONLY work from this cwd).
2. `delegate workflow status wf_9484b4e85ae6` + tail
   `.delegate/workflows/wf_9484b4e85ae6/journal.jsonl`.
3. **If failed again with `SupervisorWatchdogExit`**: read
   `~/tmp/wf-status-samples.log` (1Hz status sampler, pid 3041795, self-stops
   ~20:35Z) around the death timestamp — a `failed`/`killed`/READFAIL sample
   identifies the true watchdog trigger (C3 below). Then resume:
   `DELEGATE_WORKFLOW_WATCHDOG_TIMEOUT_SECONDS=3600 delegate workflow run --resume wf_9484b4e85ae6`
   (journal replay caches all finished agents).
4. Re-arm the watch: background `delegate workflow wait wf_9484b4e85ae6`,
   /babysit loop ~20 min (Trey delegated the interval), log at
   `~/tmp/plan-b-babysit-log.md` (header has territory/read-only rules and the
   review-time checklist).
5. At workflow terminal success: final review per babysit skill step 6 — run
   `tests/acceptance.sh` yourself on final HEAD, walk the checklist, verdict +
   rulings list to Trey. Merged spine lands on the plan branch; close
   fast-forwards main; then commit/push Forgejo.

## Open items (from babysit log — read it for full detail)

- **C3 (open):** runs 1+2 were killed by the supervisor watchdog for an
  unknown trigger. Established: NOT heartbeat staleness (env var 600s was live
  in run 2, death 28s after last event). Suspects: `terminal` or
  `state_missing` status.json reads. Runtime never logs the true reason
  (papercut pc2_a75176e09d6dd6fc). Run 3 carries 3600s timeout, verified in
  /proc.
- **N1:** reviewer route `cursor` uses invalid model `grok-4.6-xhigh-fast`
  (needs `cursor-` prefix); every review round burns 2 failed attempts then
  self-heals via omp retry. Deliberate: leave alone (script edit breaks replay
  hash). Fix the routing table for FUTURE compiles:
  `docs/plans/plan-b-stateful-read-layer.md` line ~177.
- **CO-B1-1 (acknowledged, work owed):** delete dead
  `require_activated_for_state_migration` (migration_fence.rs) + the `const _`
  binding in channel_state.rs:16-17 — route into B5's lane or do at
  coordinator level before close. Ruling: delete, not gate.
- **CO-B1-2 (acknowledged, work owed):** B6 scope addition — rewrite
  POST_ARX_GENERATION schema entry (schema.rs:279) to the real cursor
  contract, drop `.channel-state.v1.bak` claim. Verify cmds in
  `.delegate/plan-state.json` under coordinator.
- **W (new, unjudged):** claude-2 stale delegate run = abandoned first
  adjudicate attempt, harmless zombie; `delegate cancel claude-2` if it
  lingers.
- Loose end from last handoff, still pending: atlasos (trey-cell) owes
  deletion of their shadowing `~/.local/bin/post`.

## Infra changes made this session (survive rotation)

- `~/.delegate/config.work.json`: earlier `stallMinutes: 20` got reverted by
  provisioning — durable tuning belongs in `config.work.local.json` (not yet
  done; watchdog is handled via env var on resume instead).
- Papercuts filed: pc2_8046adb9f3417794 (stall watchdog vs Sol), 
  pc2_a75176e09d6dd6fc (5s workflow watchdog default + unlogged trigger).

## Rulings this session (also on bead post-rsq)

1. Watchdog timeout raised via env var + attempted config (workflow died
   healthy twice; resume is lossless via journal replay).
2. Cursor reviewer route left broken mid-run (self-heals; replay-hash risk).
3. CO-B1-1: delete the fence function outright. CO-B1-2: accept into B6 scope.
4. Coordinator acks issued at 0acef93 so wave-1 close could proceed.
