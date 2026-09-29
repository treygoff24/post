#!/usr/bin/env node
// Codex hook adapter: injects metadata-only "new post mail" notifications into
// model context at SessionStart / UserPromptSubmit / root PostToolUse.
// Everything shared by the four harness hooks lives in mail-hook-core.mjs; this
// file only says what Codex's payloads look like. Adapter recipe and contract:
// docs/ADAPTERS.md.
//
// Contract (the shared invariants are documented in the core):
// - runs `post watch --snapshot` from the hook cwd, so post resolves the
//   deepest registered room itself (read-only, envelope-only);
// - subagent PostToolUse events are suppressed;
// - PostToolUse scans are throttled to one per 30s via state-file mtime;
// - per-session dedupe keyed by session_id; SessionStart resets it;
// - a failed scan emits ONE generic diagnostic per failure streak, never a
//   fake empty inbox, never a per-event flood;
// - strictly fail-open: any internal error emits `{}` and exits 0;
// - injected context carries valid direct-mail ids and count-only summaries
//   for channel/unreadable mail: no subject, sender, body, or filename data;
// - listed ids/channel names are capped; the mail notice stays under 4 KiB,
//   and the optional participant line keeps merged context under 4352 bytes;
// - dedupe/fail-streak state commits only after a successful synchronous
//   stdout write of the final JSON payload (all bytes on fd 1).
//
// Codex exec mode is not detectable from the hook payload (SessionStart carries
// session_id, cwd, model, source), so a `codex exec` run in a registered room is
// minted as usual; a delegate child (DELEGATE_RUN_ID) and any run in an
// unregistered cwd are not minted until their first write.
//
// Test overrides (all optional):
//   POST_CODEX_HOOK_BIN         path to the post binary
//   POST_CODEX_HOOK_STATE_DIR   state directory (default <tmpdir>/post-codex-mail)
//   POST_CODEX_HOOK_THROTTLE_MS PostToolUse throttle (default 30000)

import path from "node:path";
import { runMailHook } from "./mail-hook-core.mjs";

const PHASES = {
  SessionStart: "start",
  UserPromptSubmit: "prompt",
  PostToolUse: "tool",
};

function isSubagent(input) {
  return Boolean(
    input.agent_id ||
      input.agent_type ||
      input.is_subagent ||
      input.subagent ||
      input.subagent_id ||
      input.parent_session_id
  );
}

runMailHook({
  harness: "codex",
  envPrefix: "POST_CODEX_HOOK",
  stateDirName: "post-codex-mail",
  text: {
    waiting: "New mail",
    manualCheck: "Check manually from the project directory with: post inbox",
  },
  lazyMint: true, // Codex exports CODEX_THREAD_ID to every shell call
  parse(input) {
    const event = input.hook_event_name;
    if (!Object.hasOwn(PHASES, event)) return null;
    if (event === "PostToolUse" && isSubagent(input)) return null;
    if (typeof input.session_id !== "string" || input.session_id.trim() === "") return null;
    if (typeof input.cwd !== "string" || !path.isAbsolute(input.cwd)) return null;
    return { event, phase: PHASES[event], sessionRaw: input.session_id, cwd: input.cwd };
  },
  payload: (event, context) => ({ hookSpecificOutput: { hookEventName: event, additionalContext: context } }),
});
