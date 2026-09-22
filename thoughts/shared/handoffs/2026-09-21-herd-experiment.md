# Handoff 2026-09-21: herd experiment, tomorrow

Trey went to bed after a discussion of arXiv 2609.21032 ("Scaling Discovery through Test-Time Communication") and said we will "do all of this, these experiments and everything tomorrow." Nothing is built. Nothing is approved for build beyond what this file says.

## What exists

- The proposal: `docs/plans/verified-progress-sharing-2026-09-21.md` (commit 3922605). It holds the disconfirm-primitive design (Phase 0 conventions, Phase 1 code gated on Phase 0 evidence), the three-arm experiment (A workflow, B herd on a post channel, C same herd with no channel), and two changes worth making regardless.
- The bead: `post-e2a`, "Decide: run the herd vs workflow experiment." It is a decision for Trey, not a task to start unprompted.
- Evidence gathered tonight: delegate's agent-to-agent mail across 24 devbox repos has carried about 28 messages ever, 3 peer to peer. Census command is in the proposal's Sources.

## Tomorrow, in order

1. Trey confirms the task. Veritas perf is the candidate; the first check is what its bench measures and how fast it runs, because the experiment needs a scalar score computable on demand in about a minute. If the bench is slower or the metric is noisy, pick another task before anything else.
2. Phase 0 setup is conventions only: a herd channel, the herd prompt (rules are in the proposal), one worktree per agent, the search-hygiene lines added to the Arm A briefs so prompt is not confounded with topology.
3. Run Arm C before Arm B if only one can run first; C is the control that tells us whether any herd advantage is communication or just persistence and family diversity.
4. Each arm at least twice before believing an ordering.

## Not to do

- Do not write the Phase 1 `--disconfirm` flag first. The creation gate in the proposal says it is built only if Phase 0 shows disconfirms being filed and missed.
- Do not put cheap mechanical lanes on the channel.
- Do not change delegate's mail.
- GitHub stays gated; Forgejo push is done.
