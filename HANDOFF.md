# HANDOFF — resume Plan B on the devbox

Written 2026-08-31 on the Mac at the end of the v0.8.0 ship session. Transient:
the session that consumes this deletes it. Work graph truth is beads; this file
is the narrative bridge across machines.

## Where things stand

**v0.8.0 is fully shipped.** GitHub release live and verified
(https://github.com/treygoff24/post/releases/tag/v0.8.0), Mac `~/.local/bin/post`
and devbox host `/usr/local/bin/post` both upgraded (sha256-verified; devbox keeps
`post-0.7.0.bak`), announcements posted in `#machineroom-devbox`. Follow-up
canonicalization pass (2026-08-31, Trey ruling): trey/matt/jc cell
`/usr/local/bin/post` all at 0.8.0, root-owned, exactly one `.bak` each, the
0.5/0.6/pre-* rollback graveyard purged at root level everywhere; in-cell smoke
green. Cell binaries update from the Mac via host incus at each release — the
arrangement is codified in docs/RELEASING.md. Pending on the trey-cell resident
(atlasos, green-lit in `#machineroom-devbox`): delete their shadowing
`~/.local/bin/post` + rollback siblings and re-arm their watch, so PATH falls
through to the canonical copy. What shipped:

- `watch --from now` — opt-in backlog suppression (one discarded pre-loop scan;
  fail-open). The default backlog replay is a deliberate recovery invariant
  (`src/commands/watch.rs:266-273`) — never flip it.
- Actionable ring lines: digest lines carry `[first..last]` ids and a ready-made
  `--since 'fencepost'` (last id char replaced with `!` so exclusive `--since`
  includes first_id). CONTRACT.md documents both.
- Doctor: empty store is healthy (Info `config.rooms_empty`, not Error) — fixed
  papercut pc2_97e06ad35353d2e3.
- Hook-notice disclaimer trim: the no-authority norm is stated once canonically
  (post SKILL.md, README Laws, global CLAUDE.md); the per-notice repetition is
  gone from all Mac hook adapters (8 installed copies deployed).
  `skills/post/hooks/identity-card.mjs` FRAME deliberately untouched. The devbox
  has **no post hook adapters**, so nothing to deploy there.
- `scripts/smoke-installed.sh` — live smoke for any installed binary against a
  throwaway root (promoted from the release verification; RELEASING.md points
  at it).

## The task: Plan B — stateful read layer (bead `post-rsq`)

Per-agent channel cursors, `post catchup`, real unread counts, search. Kills the
"610 unread ???" problem. **Level 2 reviewed plan** per writing-plans: Phase 0
goal lock → one architect pass + adversarial fresh-context plan-reviewer →
plan-lint → compile to beads (+ workflow if lanes warrant).

### Phase 0 rulings — already decided (recorded on post-rsq)

Trey delegated these to the agent ("you decide: which will be best for agents
like you using the tool"):

1. **Cursor granularity: per-room-per-channel**, plus one per-room mail cursor.
   Agents catch up channel-by-channel; a room-level cursor would mark every
   channel read after one catchup; per-message read-marks are write
   amplification with no catchup value.
2. **No rollout compat window.** Cursors are new orthogonal state; default watch
   stays cursor-free, armed doorbells unaffected. Hard invariant: advancing a
   cursor via `catchup` must NOT suppress watch rings — cursors drive
   catchup/unread counts, never the doorbell.
3. **Retention/expiry: out of scope** for Plan B.
4. **Body previews in watch lines: yes** (Trey ruled directly) — capped ~80
   chars, sanitized, untrusted-framed.
5. **CLI read-time framing** (the binary's "banner diet" layer — third layer of
   the disclaimer question) is an open item for Plan B's contract pass, not
   pre-decided.

So the goal lock is mostly pre-resolved: confirm scope with Trey in two
sentences, then go to architecture. Planned recon: run the
`agent-ergonomics-and-intuitiveness-maximization-for-cli-tools` skill in
audit-only mode as Phase 1 evidence.

## After Plan B

Plan C (mentions, notify levels, pins, ack) authors only once B's contract
freezes. Note `chat --re ID` reply threading already exists — don't redesign it.

## Loose ends carried over

- Mac doctor warning `profiles.claude-space.inert` — pre-existing, flagged to
  Trey, not ours to fix (Free Claude's room).
- Stale delegate run `omp-12` (group `perfwave`, ~10d old) on the Mac — predates
  this session, left alone.
- Devbox fc/sol run self-managed post binaries (notified of 0.8.0, upgrade is
  theirs). Long-running watches everywhere keep the old inode until restarted.
- `~/.claude-shared/rules/post-mail-doorbell.md` (Mac) now documents the backlog
  replay + `--from now` opt-out; propagates to the devbox cell via nightly
  estate-sync — verify it landed if a devbox session needs it sooner.
- bd papercut pc2_7fd388d3b60c3939: read-only `bd show` re-exports
  `.beads/issues.jsonl` and dirties the tree — avoid bd calls between release
  preflight and upload.
