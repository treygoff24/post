# Overnight run 2026-09-22 → 23: fix every live post papercut

Start here. This file is the handoff from session b487cc2e (Claude, Mac). It
carries Trey's rulings, the team setup, the lane plan, and pointers. The
diagnosis lives in `docs/triage-2026-09-22/`.

## Trey's rulings (2026-09-22, late evening ET, in session b487cc2e)

- **Do all of it:** every root cause, the refactors, the new features, and the band-aids.
- **Skip the writing-plans workflow.** The lead writes the plan, gets it reviewed, patches it on its own judgment, and builds.
- **Team:**
  - Claude (this lead, at higher effort) orchestrates.
  - A persistent Codex CLI session running **GPT-6 Astra at high effort** is the partner. It's the reviewer and design judgment for the plan and every design, refactor, and new feature. Trey prompts it himself.
- **Coordinate in a post channel made for tonight.** Both agents bind a post identity, create the channel, and arm doorbells so each hears the other.
- **Bridge v2:** take over `post-bridge-v2` in claude-space and ship it.
- **Dead doorbells:** disable or kill the timers whose panes are gone.
- **Installs:** install anything on both machines overnight. That includes post, Porch, the hooks and doorbells, and switching both machines' live bridge to v2.
- **Skill:** use the `writing-for-agents` skill to update the post skill fully for all agents once the work lands.
- **Don't wake Trey for anything.** No `ask`, no Telegram. "Y'all got this."
- **Focused pane (the lead's ruling; Trey didn't answer):** waking the focused herdr pane is an opt-in each doorbell turns on, off by default. Trey can reverse this in the morning.

GitHub stays gated: no GitHub push, PR, tag, or release tonight. Forgejo pushes are fine.

## First steps after relaunch

1. **Read the diagnosis:**
   - `docs/triage-2026-09-22/explainer.html`: the 8 root causes and the plan
   - `diag-A.md`: watch, doorbells, cursors
   - `diag-B.md`: CLI and tooling
   - `diag-C.md`: cross-machine
   - `open-cuts-2026-09-22.jsonl`: the live cuts
   - The explainer's footer names the lane claims that were corrected. Trust the explainer over the diag files where they disagree.
2. **Set up the team:**
   - Bind your post participant.
   - Create the tonight channel (suggested name `post-overnight`) and post the plan link there.
   - Arm a doorbell. For Claude that's the Monitor-wrapped `post watch` from `skills/post/references/post-mail-doorbell.md`. It expires silently, so re-arm it each time you return to idle.
   - Help Astra arm its own: find its harness and pane with `herdr agent list`, then use `install-codex-doorbell.mjs`.
   - The doorbells are themselves under repair, so run belt and braces: check the channel at every wave boundary, whatever the doorbell does.
3. **Plan, then review, then patch:**
   - Write the plan: waves, lanes, file ownership, gates.
   - Create beads for it: an epic plus one child per root cause.
   - Hand the plan and each design (above all the doorbell supervisor and bridge v2) to Astra in the channel.
   - Patch on your own judgment, and log each accepted or rejected point with a one-line reason.
4. **Build in waves.** Close out in the morning (last section).

## Lane plan (the proposal Trey approved; refine it in the plan)

**Lead (Claude)** plans, integrates, runs every gate on the merged code, and does all live verification. A lane's report is a claim until the lead re-runs its checks.

**Builders are GPT-6 Luna at xhigh:** `delegate codex work --model gpt-6-luna --reasoning-effort xhigh --isolation worktree --resumable`. Each gets disjoint file ownership, and fixes go back through `delegate followup` to the lane that already holds the context.

| Lane | Owns | Wave |
|---|---|---|
| **A: post CLI** | bounded post-commit lock wait on send; refuse a piped read (never auto-send); `who` prints `lease=` and hints `--seen-by`; `rooms set-path`; `profile list` | 1 |
| **B: watch core** | `watch --reason mail\|channel\|mention`; cursor read race (open once, check that open file, keep failing open); later the watch index refactor (measure first) | 1 → 5 |
| **C: hooks** | Python doorbell accepts typed addresses; Codex monitor logs "pane not found" and the real error text; installers handle `--help`; bridge test fixtures canonicalize temp paths; Porch `PYTHONPATH=tests` note; stable SPEC-v2 link | 1 |
| **D: contract** | post publishes golden samples of every event and `--json` output that other tools read; Porch, the doorbell, and the hooks test against them; the install smoke runs Porch's launch checks | 2 |
| **E: doorbell** | one doorbell supervisor per host with a small sink per harness (herdr, cmux, Claude); lifetime tied to the participant; loud failure; focus opt-in | 3, after the Astra design review |
| **F: bridge v2** | finish, test, and deploy `post-bridge-v2` on both machines; then allowlist `loom-build` on both; then `participant:<id>@<host>` | 4, runs on the devbox, can start right away |

**Review:**
- Every plan and design goes to the Astra partner.
- Code for each wave gets two reviewers from different families: `delegate codex safe --model gpt-6-sol --reasoning-effort high` plus GLM 5.3 (`delegate omp safe --model glm`).
- Bridge v2 also gets a Cursor Grok 4.7 xhigh attack pass.
- Each nontrivial fix round gets a fresh review.

**Order:** A, B, C, and F start together. D needs Wave 1's final output shapes. E needs `--reason` and D's samples. The watch index comes last.

**Set aside:** Porch mouse scrolling under herdr inside cmux. Reproducing it needs Trey's terminal stack; leave it as a note for him.

**Proof of done:**
- `scripts/gate.sh` passes on both machines.
- Porch's full suite passes (the known pre-existing failures are listed below).
- The doorbell and hook tests pass.
- A real end-to-end ring: a message arrives, a live devbox herdr pane is woken, and the delivery is logged.
- Install on both machines, then re-run Porch's launch check.
- One real cross-machine message in each direction on bridge v2.
- A second bridge tick makes no changes.

## Live facts you'll need

- **Installed now:**
  - post c808283 on both machines. Backups: Mac `~/.local/bin/post-{580ab81,fd58981}.bak`; devbox `post.bak-20260922-prearchive`.
  - Porch 39ca6df on both. It fixes the launch break where Porch refused post's new `key`/`legacy` fields.
- **Porch checkouts:**
  - The Mac's `~/Code/porch-tui` main has diverged and is dirty. Don't reset it; use a worktree off `origin/main`. `~/Code/porch-tui-wt-profilekeys` is one already, at 39ca6df.
  - The devbox's `~/Code/porch-tui` is on 39ca6df. Its `pull.rebase` setting refuses a dirty tree, so update with `git fetch` plus `git merge --ff-only`.
- **Dead doorbells:**
  - 15 of the 19 `post-codex-doorbell@*` timers on the devbox point at herdr panes that are gone: astra, astra-elv, astra-exa, atlas-astra, atlas-fable, atlas-opus, codex-atlas, deslop-astra, porch-aster, porch-cricket, porch-kettle, porch-moth, porch-vesper, reed-cos, sill.
  - Live, so leave them alone: vale-recall, linden, atlas-reviewer, atlas-quill.
  - Re-check each with `herdr agent get <name>` before disabling it.
- **Bridge:**
  - Both machines run the single-file v1 `~/.local/bin/post-bridge-sweep`. The Mac runs it from launchd `com.treygoff.post-bridge`; the devbox from the systemd user unit `post-bridge.timer`.
  - The Mac also runs `com.treygoff.agent-post-cell-bridge`. Its purpose is unknown; identify it before you change the bridge.
  - Each machine's config is a static `peers` map, and `channels` is null.
  - v1 logs `unknown_envelope_keys` (address_kind, from_lineage, from_participant) about 30k times on the Mac and 39k on the devbox. Check whether sender attribution is lost across the bridge.
  - v2 lives at devbox `~/Code/claude-space` on branch `post-bridge-v2`, head `db01766`, 18 commits ahead of its remote, with staged edits to bridge source from an unknown session. Inspect those edits and keep them. Trey handed the branch over.
- **Gate hazards:**
  - The launcher tests need a Cargo that honors `target-dir`. If the estate shim interferes, set `POST_REAL_CARGO`.
  - Mac socket tests fail on long worktree paths; use a short `TMPDIR`.
  - Known Porch failures unrelated to this work: `test_canary` (the committed ship tree has unwaived canaries), a flaky lock test in `test_recovery`, and two draft-lock tests in `test_ui_operate` that fail in a Mac worktree.
- **Papercuts:** close each cut on the host and ledger where it lives (`papercuts resolve --file <ledger> --note ...`). The ids and ledger paths are in `open-cuts-2026-09-22.jsonl`.

## Morning closeout (Trey reads this first)

- Update `docs/triage-2026-09-22/explainer.html` into a closeout: what shipped, what's installed where, what was verified live, and what's still open and why.
- Open it in Aside on the Mac with `open -a Aside <path>`.
- Final message: the outcome in one sentence, then the rulings you made overnight, each with its reason and downside, then what you need from him.
- Update `STATE.md` (local, gitignored) and close the beads, each with a reason.
