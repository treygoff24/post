#!/usr/bin/env node
// Cursor CLI hook adapter: injects metadata-only "new post mail" notifications
// into model context at sessionStart / beforeSubmitPrompt / root postToolUse.
// Everything shared by the four harness hooks lives in mail-hook-core.mjs; this
// file only says what Cursor's payloads look like. Adapter recipe and contract:
// docs/ADAPTERS.md.
//
// Contract deltas vs the Claude adapter, from Cursor CLI 2026.08.11-e8db854
// (hooks.json + additional_context carriers in the CLI bundle):
// - event names are Cursor camelCase (sessionStart / beforeSubmitPrompt /
//   postToolUse), not Claude PascalCase;
// - room is resolved from absolute `cwd`, else the first absolute
//   `workspace_roots[]` entry (user-level hooks often run with cwd ~/.cursor
//   and put the project in workspace_roots); missing both fails open;
// - session id is `session_id` or `conversation_id`;
// - subagent suppression keys on nonempty `subagent_id` (and Claude-compat
//   `agent_id`); `agent_type` and `is_background_agent` are NOT discriminators;
// - native output is `additional_context`; Claude nested hookSpecificOutput
//   is included so enableClaudeNestedHookSpecificOutputCompatibility still
//   injects. hookEventName equals the firing Cursor event name;
// - sessionStart resets per-session dedupe so a resume still reminds;
// - Cursor exports no ambient session key to the agent's shell, so the agent
//   needs its participant id printed at start: this adapter always mints at
//   session start (no lazy minting) and prints the id.
//
// Test overrides (all optional):
//   POST_CURSOR_HOOK_BIN         path to the post binary
//   POST_CURSOR_HOOK_STATE_DIR   state directory (default <tmpdir>/post-cursor-mail)
//   POST_CURSOR_HOOK_THROTTLE_MS postToolUse throttle (default 30000)

import path from "node:path";
import { runMailHook, safeIdentityPart } from "./mail-hook-core.mjs";

const PHASES = {
  sessionStart: "start",
  beforeSubmitPrompt: "prompt",
  postToolUse: "tool",
};

function resolveCwd(input) {
  if (typeof input.cwd === "string" && path.isAbsolute(input.cwd)) return input.cwd;
  if (Array.isArray(input.workspace_roots)) {
    for (const root of input.workspace_roots) {
      if (typeof root === "string" && path.isAbsolute(root)) return root;
    }
  }
  return null;
}

function sessionIdOf(input) {
  for (const value of [input.session_id, input.conversation_id]) {
    if (typeof value === "string" && value.trim() !== "") return value;
  }
  return null;
}

function isSubagent(input) {
  return (
    (typeof input.subagent_id === "string" && input.subagent_id !== "") ||
    (typeof input.agent_id === "string" && input.agent_id !== "")
  );
}

function bindingLine(id) {
  const line = safeIdentityPart(id)
    ? `[post] participant ${id}; prefix Post commands with POST_PARTICIPANT=${id}`
    : null;
  return line && Buffer.byteLength(line, "utf8") <= 256 ? line : null;
}

runMailHook({
  harness: "cursor",
  envPrefix: "POST_CURSOR_HOOK",
  stateDirName: "post-cursor-mail",
  text: {
    waiting: "Unread agent mail",
    manualCheck: "Manual check, from the project directory: post inbox",
  },
  lazyMint: false,
  bindingLine,
  parse(input) {
    const event = input.hook_event_name;
    if (!Object.hasOwn(PHASES, event)) return null;
    if (isSubagent(input)) return null;
    const sessionRaw = sessionIdOf(input);
    if (sessionRaw === null) return null;
    const cwd = resolveCwd(input);
    if (cwd === null) return null;
    return { event, phase: PHASES[event], sessionRaw, cwd };
  },
  payload: (event, context) => ({
    additional_context: context,
    hookSpecificOutput: { hookEventName: event, additionalContext: context },
  }),
});
