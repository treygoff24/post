# post

House rules for the work ledger (this section replaces the bd-managed block on purpose; do not let `bd` re-inject one):

- **Beads is the work graph only** — tasks, bugs, dependencies, close-reasons. **Journal, state/sitrep, and memory files are the narrative and continuity layer and we use them heavily.** Beads never replaces them; a close-reason should point at the journal entry or commit that holds the story.
- Model decisions that need the maintainer as blocker beads (human-checkpoint-as-blocker-edge), so dependent work can't be picked up by mistake.
- `bd remember` is welcome *alongside* memory files, not instead of them.
- Git behavior (what may be committed or pushed, and when) comes from the maintainer's own rules, never from beads tooling.

Read [CONTRIBUTING.md](CONTRIBUTING.md) for development checks and resource
limits. Run the narrow tests for the code being changed while iterating;
`./scripts/test.sh gate` is the integrated gate before release. Its success
must include the release binary, hook suites, schema and bridge tests.
[CONTRACT.md](CONTRACT.md) defines the CLI and storage behavior; read the
relevant part before changing it. [docs/RELEASING.md](docs/RELEASING.md)
defines public snapshot preparation, artifact verification and publication.

A local `STATE.md`, when present, contains working notes. It is optional,
is not part of the public source, and cannot replace the tracked contributor
and release instructions. A checkout without `.beads/` needs no work ledger
setup merely to contribute.
