# Project Instructions for AI Agents

This file provides instructions and context for AI coding agents working on this project.

## Issue tracking — beads (house rules)

This project uses **bd (beads)** as the work ledger. `bd prime` for commands; `bd ready` on arrival.

- **Beads is the work graph only** — tasks, bugs, dependencies, close-reasons. **Journal, state/sitrep, and memory files are the narrative and continuity layer and we use them heavily.** Beads never replaces them; a close-reason should point at the journal entry or commit that holds the story.
- Model decisions-needed-from-Trey as blocker beads (human-checkpoint-as-blocker-edge), so dependent work can't be picked up by mistake.
- Create the bead before starting substantial work; close with `--reason`.
- `bd remember` is welcome *alongside* memory files, not instead of them.
- Git behavior comes from this room's own rules (commits ungated, pushes gated — global CLAUDE.md), never from beads tooling.

Do not let `bd` tooling re-inject its managed CLAUDE.md/AGENTS.md block; this section replaces it deliberately.


## Build & Test

_Add your build and test commands here_

```bash
# Example:
# npm install
# npm test
```

## Architecture Overview

_Add a brief overview of your project architecture_

## Conventions & Patterns

_Add your project-specific conventions here_
