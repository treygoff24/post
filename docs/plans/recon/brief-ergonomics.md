# Recon lane: agent-ergonomics audit of post 0.8.0 (audit-only)

Read the skill at
`~/.agents/skill-library/agent-ergonomics-and-intuitiveness-maximization-for-cli-tools/SKILL.md`
and apply it in **audit-only mode** to the installed `post` binary (on PATH,
v0.8.0). You are gathering evidence for Plan B — a stateful read layer adding
per-agent cursors, `post catchup`, real unread counts, and `post search`.
Context: `docs/plans/plan-b-goal-lock.md` in this repo.

## Method

- Probe the LIVE binary against a throwaway store: `export
  POST_MAIL_ROOT=/tmp/planb-ergo-$$` (absolute path), create rooms/channels,
  send messages, and exercise the read surface as an agent would: `channels`,
  `inbox`, `chat --peek`, `chat --since`, `read --peek`, `watch --snapshot`,
  `schema`, `doctor`. NEVER point at the real `~/.claude-mail`.
- Score per the skill's rubric where it fits; where it doesn't, plain findings.

## Focus (weight toward Plan B's surface)

1. Where does an agent arriving at a busy store waste turns today? Walk the
   actual "what did I miss" journey and log every dead end.
2. What output/flag design would make `catchup`, unread counts, and `search`
   maximally ergonomic for agents (json-mode discoverability, `schema`
   coverage, exit codes, empty-state output, id/fencepost handoffs between
   commands)?
3. General 0.8.0 ergonomics findings outside Plan B scope go in a separate
   final section marked OUT OF SCOPE (they become future beads, not Plan B
   work).

## Output

One file: `docs/plans/recon/ergonomics-audit.md`. Findings as short entries:
observation → evidence (the exact command + output snippet) → recommendation.
Every claimed behavior must come from a command you actually ran; paste the
real output, trimmed. No fabricated transcripts.

## Rules

- Write boundary: `docs/plans/recon/ergonomics-audit.md` only. Do not modify
  src or any other file. Do not commit. Never run tree-wide git state
  commands (stash/checkout/restore) — other lanes share this tree.
- Clean up your throwaway store dir when done.
