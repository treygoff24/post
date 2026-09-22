# Verified progress sharing: the disconfirm primitive and the herd experiment

Written 2026-09-21 after reading Park, Kontonis, Garg, Krishnamurthy, and Papailiopoulos, "Scaling Discovery through Test-Time Communication" (arXiv 2609.21032), alongside Anthropic's "Patterns and Problems in Emerging Multiagent Systems" (Aug 13, 2026). Trey asked for two things to revisit later: a design for the one primitive worth stealing, and a written experiment. Nothing here is approved for build. It is a proposal.

## What the paper established, in one paragraph

Identical agents with no roles and no orchestrator, each with a private scratch directory, sharing an append-only log and a verifier they can query, beat the best of the same number of independent agents on open-ended search tasks: on ARC-AGI-3, five talking Sonnets matched 33 silent ones. The mechanism is that a shared, verified breakthrough turns "one agent must clear every stage" into "whoever clears stage j hands it to everyone." It fails in two known ways. Without a queryable verifier (Terminal-Bench, where the check runs only after the agents finish), a talking pair lost to an independent pair. With too little budget per agent (0.2x each), five talking agents lost to one agent with the same total. The paper's SOTA numbers rest on one or two team runs; the ARC result is the only one with real statistics.

Our own data agrees with the paper's diagnosis. Across 24 repos on the devbox, delegate's agent-to-agent mail has carried about 28 messages ever, three of them peer to peer. The rest are child-to-coordinator status reports. Our workflows decompose the goal into disjoint beads with disjoint files before any agent starts, so there is nothing for one lane to tell another. The messaging feature sits unused because the topology gives agents no shared objective, not because the agents are uncooperative.

## The primitive: a disconfirmation edge

The thing to steal is not the log (post already is one) and not the leaderboard (a convention covers it). It is the paper's separate disconfirmation record, combined with a prompt line that tells every agent to spend part of its effort trying to falsify the leading idea. Anthropic's post gives the reason this matters: on hidden-profile tasks, agents "systematically fail to surface and press decisive private information that contradicts group consensus," and that failure does not saturate with model capability. A shared channel invites exactly this herding. The countermeasure has to be structural, because the disposition is not there by default.

A disconfirmation must be cheap to file, impossible to miss when someone reads the claim it attacks, and socially expected. The third is a prompt property; the first two are post properties, and only the second might need code.

### Phase 0, conventions only, no code

A herd channel (say `herd-<task>`) carries four kinds of message, distinguished by a leading token in the body. Post's `--re <ID>` reply edge and `--history --grep` are enough to make them findable.

```
CLAIM   score=0.912 family=skyline+contact repro="make bench" built-on=<msg-id or none>
        <what changed and why it should hold>

DISCONFIRM  re <claim-id>   (sent with --re <claim-id>)
        <what I tried, the measured result, why the claim does not hold or does not generalize>

ADOPT   re <claim-id>       (sent with --re <claim-id>)
        <what I am taking, and the one variation I am keeping>

SLOT    family=<approach>   (or a `fleet` claim, which already exists)
```

Rules that go in the herd prompt, adapted from the paper's Appendix A.2 with the Anthropic finding folded in:

A CLAIM without a `score=` and a `repro=` is prose, not a claim, and nobody adopts it. Before adopting, an agent reads the claim's replies; an open DISCONFIRM with a repro blocks adoption until the claimant answers it. Every agent spends a fixed share of its turns trying to disconfirm the current best claim (one in four is the paper's spirit, not its number), and a DISCONFIRM with a repro earns the same credit as an improvement, because silence about a contrary result is the exact failure we are guarding against. An agent that adopts keeps one real difference from the adopted approach until the end. Mechanical lanes do not post; the channel is for agents with budget to explore.

This is the whole Phase 0. It costs a channel, a prompt, and a convention doc. Run the experiment below on it.

### Phase 1, code, only if Phase 0 shows the need

Creation gate, answered honestly. Consumer: herd agents deciding whether to adopt a claim. Gate: the "no adoption past an open disconfirm" rule cannot be enforced by reading if disconfirms are hard to find. Observed defect: none yet in our stack; the defect is Anthropic's measured hidden-profile failure plus our zero peer-to-peer traffic, which is why this stays gated on Phase 0 evidence. Deletion condition: if the experiment shows disconfirms are filed and found reliably under the plain `--re` convention, or never filed at all, this code does not get written.

If Phase 0 shows agents filing disconfirms but readers missing them, the minimal code is one typed reply edge:

`post chat <ch> --send --re <ID> --disconfirm` records the reply as a disconfirmation of `<ID>`, immutable like everything else in a channel. Chat, history, and read output render a contested claim with a count beside it (`[2 disconfirm]`) and render the disconfirmation with its own marker, so a reader cannot see a claim without seeing that it is contested. `post chat <ch> --disconfirms <ID>` lists the disconfirmations of one message with the cursor untouched.

Nothing resolves a disconfirmation. Post is append-only and has no state machine; the claimant replies and the reader judges. If a need for "conceded" appears later, it is another reply edge, not a mutation.

No `--claim` flag, no scoreboard command, no adoption tracking. `--history --grep 'CLAIM'` sorted by hand is the leaderboard until someone shows it is not enough.

## The experiment

Purpose: find out whether a persistent, heterogeneous herd of agents on a post channel beats our workflow on a search-shaped problem, and whether the difference comes from the communication or from the persistence and heterogeneity.

### Task selection

The task needs a scalar score agents can compute on demand in under a minute, a real ceiling nobody has hit, no obvious decomposition into disjoint subtasks, and private worktrees. Veritas perf is the leading candidate (an `astra-perf` lane already exists, so there is a number and a bench). A second candidate is any benchmark-shaped optimization we care about. A feature build is the wrong task; the paper's own result says the workflow should win there, and we would learn nothing.

Set the budget in output tokens, matched across arms, with wall clock as a secondary measure. Do not set it low: the paper's coordination tax means a starved herd loses by construction.

### Three arms

Arm A, current practice. Bead the goal through `writing-plans`, run it through the delegate workflow with whatever lanes the plan calls for, coordinator merges and gates.

Arm B, the herd. Four or five persistent agents in Herdr panes, different families (a Fable, an Astra, a Sol, an Opus, one cheaper family), each in its own worktree, on one post channel under the Phase 0 conventions and prompt. No orchestrator. The gate is the verifier; agents run it themselves.

Arm C, the control. The same agents, same prompt minus the channel rules, same worktrees, same budget, no channel. Best score across them counts. This is the paper's best@k. It is the arm that tells us whether B's edge, if any, came from talking or from persistence and family diversity.

If the workflow lanes in Arm A do not get the paper's search-hygiene lines (keep working while budget remains, switch approach family after three non-improving attempts), add them, or the comparison confounds prompt with topology.

### What we measure

The primary number is the best score per arm at the matched budget, with the score-versus-tokens curve alongside it, because the paper's teams lose early and win late and a single endpoint can hide that. In Arm B we also count CLAIMs, DISCONFIRMs, and ADOPTs, and how many adoptions followed an open disconfirm; the primitive's job is to make that last number zero. We read the built-on chain of B's winning approach to see whether it combined ideas from more than one agent, which is the paper's qualitative signature and the thing a workflow structurally cannot produce. For herding, we count distinct approach families live at the halfway point in B versus C.

### Outcomes and what each one means

B beats A and C by a margin larger than run-to-run noise: communication earned it. Move search-shaped work (perf, hard bugs, design exploration, argument hardening) to herds; keep plan-shaped work on the workflow. Consider Phase 1 code.

B and C both beat A, and B is not clearly ahead of C: persistence and heterogeneity earned it, the channel did not. Persistent named agents are worth it on their own; the channel stays a convenience.

A beats both: the task was more decomposable than it looked, or the budget starved the herd. Rerun once with a larger budget before concluding anything, then leave the workflow alone.

Run each arm at least twice before believing any ordering. The paper's n=1 SOTA claims are the cautionary tale.

## Things we should do regardless of the experiment

Two, and both are cheap.

Give every workflow lane the paper's search-hygiene lines when the lane's task has a score. "A working result is not the finish line" and "switch approach family after three non-improving attempts" are single-agent rules that cost nothing and that our briefs do not say.

Stop routing chatter to mechanical lanes. If a lane is doing token-heavy mechanical work on a cheap model, it should neither read nor post to a channel. The coordination tax is real and it lands hardest on the agents with the least room to use what they read.

Delegate's own mail stays as it is. It is a status pipe between coordinator and children, it works as one, and turning it into post would solve a problem nobody has.

## Sources

Park et al., "Scaling Discovery through Test-Time Communication," arXiv 2609.21032 (Sep 17, 2026); local copy at `~/Downloads/2609.21032v1.pdf`. The communication prompt is Appendix A.2, the Terminal-Bench negative result is Section 3.4 and Table 3, and the budget ablation is Figure 4.

Anthropic, "Patterns and Problems in Emerging Multiagent Systems," https://www.anthropic.com/research/multiagent-systems (Aug 13, 2026). The hidden-profile finding is under "Epistemic failures"; the branch-name conformity example (18 of 30 agents chose `mvp-game-loop`) is under "Failures from conformity."

Devbox mail census, 2026-09-21: `find ~/Code/*/.delegate/mail -path '*sent*'` across 24 repos found about 28 messages, 3 of them peer to peer (codex-31 to codex-2 in trey-goff; codex-6 to codex-5 and to omp-2 in ultimate-harness).
