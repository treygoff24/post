# Adversarial plan review: Plan B — stateful read layer

You are a fresh-context adversarial plan reviewer. The plan is
`docs/plans/plan-b-stateful-read-layer.md` in this repo. It already passes
`plan-lint` — do not re-run mechanical lint; your job is what lint cannot see.
You did not write this plan; your only loyalty is to the run that will execute
it with no human watching.

Context documents (read all four):

- `docs/plans/plan-b-goal-lock.md` — the locked goal and rulings (settled; a
  plan/lock conflict is a finding, relitigating a ruling is not)
- `docs/plans/plan-b-stateful-read-layer.md` — the plan under review
- `docs/plans/recon/architecture.md` — the architecture the tasks reference
- `docs/plans/recon/seams.md` — code-seam evidence with file:line anchors

Attack surfaces, in priority order:

1. **Dependency and ownership truth.** Will wave 2 (B2/B3/B4) actually build
   and test in parallel worktrees? Check for hidden shared files the
   owned_files lists miss (Cargo.toml? src/lib.rs after B1? output.rs use by
   B2/B5 for Framing reuse? commands/mod.rs re-exports?). A compile-time
   collision between parallel lanes is a blocker finding.
2. **The B1 wrapper shim.** B1 keeps channel_state.rs as thin wrappers so
   callers compile; B3 deletes them. Verify every current channel_state caller
   is owned by B3 or survives unchanged — `rg` the callers yourself. If B2 or
   B4 also call channel_state/cursor_state paths that B3 rewires, the waves
   are misordered.
3. **Acceptance honesty.** For each task, could a lane satisfy the verify rows
   while shipping the wrong thing? Name the cheapest gaming path the
   acceptance text fails to block.
4. **Invariant coverage.** The doorbell invariant, fence read-only guarantees,
   and mail-move ordering: does some verify row actually bind each one, or is
   any invariant proven only by prose?
5. **Contract seam.** B6 amends CONTRACT.md last. Is anything in B2–B5
   underdetermined without contract text existing first (flag wording, JSON
   field names) such that parallel lanes will drift? The Interfaces section is
   supposed to pin this — find what it fails to pin.
6. **Anything the recon evidence contradicts** in the plan or architecture.

Report format — the checkout is read-only for you; emit the full report as your final message (it is captured):

- `## Verdict` — SHIP / SHIP WITH FIXES / DO NOT SHIP
- `## Findings` — numbered, each: severity (blocker/major/minor), the claim,
  the evidence (file:line you actually read), and the smallest fix.
- `## Checked, no finding` — surfaces you examined that came back clean, one
  line each, so silence is distinguishable from omission.

Rules: read-only; write nothing. Cite only lines you
actually read. An "I could not verify X" is a valid finding; a guessed anchor
is not.
