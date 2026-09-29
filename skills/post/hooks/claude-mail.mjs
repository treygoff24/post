#!/usr/bin/env node
// Claude Code hook adapter: injects metadata-only "new post mail" notifications
// into model context at SessionStart / UserPromptSubmit / root PostToolUse.
// Everything shared by the four harness hooks lives in mail-hook-core.mjs; this
// file only says what Claude Code's payloads look like. Adapter recipe and
// contract: docs/ADAPTERS.md.
//
// Contract deltas vs the other harnesses, from the Claude Code hooks reference
// (code.claude.com/docs/en/hooks, fetched 2026-07-30):
// - the room is resolved from the hook's `cwd` by post itself (no --room pin):
//   a session inside a registered room tree gets that room's mail; any other
//   cwd is not minted at all until the agent's first write (see the core);
// - subagent suppression keys on `agent_id` ONLY: present iff the hook fired
//   inside a subagent. `agent_type` is NOT a discriminator: it is also set on
//   the main thread when the session was launched with `--agent`;
// - `hookSpecificOutput.hookEventName` must equal the firing event's name;
// - injected text is factual and non-imperative (imperative "system" phrasing
//   trips Claude's prompt-injection defenses per the docs);
// - on --resume, mid-session injections replay from the transcript but
//   SessionStart re-runs with source "resume"/"fork"; its state reset makes
//   still-unread mail surface fresh, which is the correct reminder.
//
// Test overrides (all optional):
//   POST_CLAUDE_HOOK_BIN         path to the post binary
//   POST_CLAUDE_HOOK_STATE_DIR   state directory (default <tmpdir>/post-claude-mail)
//   POST_CLAUDE_HOOK_THROTTLE_MS PostToolUse throttle (default 30000)

import path from "node:path";
import { runMailHook } from "./mail-hook-core.mjs";

const PHASES = {
  SessionStart: "start",
  UserPromptSubmit: "prompt",
  PostToolUse: "tool",
  SessionEnd: "end",
};

runMailHook({
  harness: "claude",
  envPrefix: "POST_CLAUDE_HOOK",
  stateDirName: "post-claude-mail",
  text: {
    waiting: "Unread agent mail",
    manualCheck: "Manual check, from the project directory: post inbox",
  },
  lazyMint: true, // Claude Code exports CLAUDE_CODE_SESSION_ID to every Bash call
  parse(input) {
    const event = input.hook_event_name;
    if (!Object.hasOwn(PHASES, event)) return null;
    // agent_id is present iff this hook fired inside a subagent; a subagent's
    // context is invisible to the user and its tool cadence would re-surface
    // the same mail repeatedly, so suppress for every event kind.
    if (typeof input.agent_id === "string" && input.agent_id !== "") return null;
    if (typeof input.session_id !== "string" || input.session_id.trim() === "") return null;
    if (typeof input.cwd !== "string" || !path.isAbsolute(input.cwd)) return null;
    return { event, phase: PHASES[event], sessionRaw: input.session_id, cwd: input.cwd };
  },
  payload: (event, context) => ({ hookSpecificOutput: { hookEventName: event, additionalContext: context } }),
});
