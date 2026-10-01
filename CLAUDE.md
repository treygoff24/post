# post

House rules for the work ledger (this section replaces the bd-managed block on purpose; do not let `bd` re-inject one):

- **Beads is the work graph only** — tasks, bugs, dependencies, close-reasons. **Journal, state/sitrep, and memory files are the narrative and continuity layer and we use them heavily.** Beads never replaces them; a close-reason should point at the journal entry or commit that holds the story.
- Model decisions that need the maintainer as blocker beads (human-checkpoint-as-blocker-edge), so dependent work can't be picked up by mistake.
- `bd remember` is welcome *alongside* memory files, not instead of them.
- Git behavior (what may be committed or pushed, and when) comes from the maintainer's own rules, never from beads tooling.

`STATE.md` is the live sitrep (local only, gitignored). Heavy gates and the release flow: `STATE.md` Pointers and `docs/RELEASING.md`.
