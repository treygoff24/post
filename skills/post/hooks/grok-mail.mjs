#!/usr/bin/env node
// Grok Build hook adapter: injects metadata-only "new post mail" notifications
// into model context at UserPromptSubmit. Everything shared by the four harness
// hooks lives in mail-hook-core.mjs; this file only says what Grok's payloads
// look like. Adapter recipe and contract: docs/ADAPTERS.md.
//
// Contract deltas vs the Claude adapter, from Grok Build 1.0.3
// (~/.grok/docs/user-guide/10-hooks.md and the grok binary):
// - Grok's Claude-compat scan of ~/.claude/settings.json does NOT make
//   claude-mail work: exec-form `args` are dropped (target becomes bare
//   `node`), and SessionStart / PostToolUse stdout is ignored. Registering
//   those events and committing seen-state would hide mail the model never
//   saw. This adapter is UserPromptSubmit only; the first prompt of a new
//   session still surfaces the launch backlog (empty per-session state) and
//   plays the part of session start;
// - stdin is camelCase (`hookEventName`, `sessionId`, `cwd` / `workspaceRoot`)
//   with snake_case aliases and GROK_HOOK_EVENT;
// - hookEventName values may be `UserPromptSubmit` or `user_prompt_submit`;
// - output is Claude nested hookSpecificOutput with hookEventName
//   `UserPromptSubmit` (the nested shape Grok already documents for Stop);
// - Grok exports no ambient session key to the agent's shell, so the agent
//   needs its participant id printed: this adapter always mints on the first
//   prompt (no lazy minting) and prints the id.
//
// Test overrides (all optional):
//   POST_GROK_HOOK_BIN         path to the post binary
//   POST_GROK_HOOK_STATE_DIR   state directory (default <tmpdir>/post-grok-mail)

import path from "node:path";
import { runMailHook, safeIdentityPart } from "./mail-hook-core.mjs";

const CANONICAL_EVENT = "UserPromptSubmit";
const EVENTS = new Set(["UserPromptSubmit", "user_prompt_submit"]);

function eventNameOf(input) {
  const raw = input.hookEventName ?? input.hook_event_name ?? process.env.GROK_HOOK_EVENT;
  return typeof raw === "string" ? raw : "";
}

function sessionIdOf(input) {
  for (const value of [input.sessionId, input.session_id]) {
    if (typeof value === "string" && value.trim() !== "") return value;
  }
  return null;
}

function resolveCwd(input) {
  for (const value of [input.cwd, input.workspaceRoot, input.workspace_root]) {
    if (typeof value === "string" && path.isAbsolute(value)) return value;
  }
  return null;
}

function isSubagent(input) {
  return (
    (typeof input.subagent_id === "string" && input.subagent_id !== "") ||
    (typeof input.subagentId === "string" && input.subagentId !== "") ||
    (typeof input.agent_id === "string" && input.agent_id !== "") ||
    (typeof input.agentId === "string" && input.agentId !== "")
  );
}

function bindingLine(id) {
  const line = safeIdentityPart(id)
    ? `[post] participant ${id}; prefix Post commands with POST_PARTICIPANT=${id}`
    : null;
  return line && Buffer.byteLength(line, "utf8") <= 256 ? line : null;
}

runMailHook({
  harness: "grok",
  envPrefix: "POST_GROK_HOOK",
  stateDirName: "post-grok-mail",
  text: {
    waiting: "Unread agent mail",
    manualCheck: "Manual check, from the project directory: post inbox",
  },
  lazyMint: false,
  startsOnFirstPrompt: true,
  bindingLine,
  parse(input) {
    if (!EVENTS.has(eventNameOf(input))) return null;
    if (isSubagent(input)) return null;
    const sessionRaw = sessionIdOf(input);
    if (sessionRaw === null) return null;
    const cwd = resolveCwd(input);
    if (cwd === null) return null;
    return { event: CANONICAL_EVENT, phase: "prompt", sessionRaw, cwd };
  },
  payload: (_event, context) => ({
    hookSpecificOutput: { hookEventName: CANONICAL_EVENT, additionalContext: context },
  }),
});
