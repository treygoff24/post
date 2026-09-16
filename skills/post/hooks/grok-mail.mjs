#!/usr/bin/env node
// Grok Build hook adapter: injects metadata-only "new post mail" notifications
// into model context at UserPromptSubmit. The Grok twin of claude-mail.mjs;
// adapter recipe and contract: docs/ADAPTERS.md.
//
// Contract deltas vs the Claude adapter, from Grok Build 1.0.3
// (~/.grok/docs/user-guide/10-hooks.md and the grok binary):
// - Grok's Claude-compat scan of ~/.claude/settings.json does NOT make
//   claude-mail work: exec-form `args` are dropped (target becomes bare
//   `node`), and SessionStart / PostToolUse stdout is ignored. Registering
//   those events and committing seen-state would hide mail the model never
//   saw. This adapter is UserPromptSubmit only; the first prompt of a new
//   session still surfaces the launch backlog (empty per-session state).
// - stdin is camelCase (`hookEventName`, `sessionId`, `cwd` / `workspaceRoot`)
//   with snake_case aliases and GROK_HOOK_EVENT;
// - hookEventName values may be `UserPromptSubmit` or `user_prompt_submit`;
// - output is Claude nested hookSpecificOutput with hookEventName
//   `UserPromptSubmit` (the nested shape Grok already documents for Stop).
//
// Shared invariants: envelope-metadata only, one diagnostic per failure
// streak, fail-open, always exit 0, commit state only after a successful
// synchronous stdout write, exclusive random-named state temps.
//
// Test overrides (all optional):
//   POST_GROK_HOOK_BIN         path to the post binary
//   POST_GROK_HOOK_STATE_DIR   state directory (default <tmpdir>/post-grok-mail)

import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { createHash, randomBytes } from "node:crypto";
import { spawnSync } from "node:child_process";

const CANONICAL_EVENT = "UserPromptSubmit";
const EVENTS = new Set(["UserPromptSubmit", "user_prompt_submit"]);
const LIST_CAP = 20;
const CONTEXT_MAX = 4096;
const MERGED_CONTEXT_MAX = CONTEXT_MAX + 256;
const NAME_MAX = 255;
const IDENTITY_PART_MAX = 4096;
const UNREADABLE_ID_MAX = 255;
const MAIL_ID = /^\d{8}-\d{6}-[0-9a-fA-F]{6}$/;
const CHANNEL_ID = /^\d{8}-\d{6}-\d{6}-[0-9a-fA-F]{6}$/;
const ROOM_NAME = /^[A-Za-z0-9._-]+$/;
const CONTROL_CHARS = /[\u0000-\u001f\u007f]/;
const HARNESS = "grok";
const SESSION_DEADLINE_MS = 4500;
const VERSION_PROBE_FAILED =
  "[post] could not verify installed post capabilities (version query failed or timed out); repair: cd ~/Code/post && cargo build --release && install -m 0755 target/release/post ~/.local/bin/post";
const PARTICIPANTS_MISSING =
  "[post] installed post lacks the participants capability; repair: cd ~/Code/post && cargo build --release && install -m 0755 target/release/post ~/.local/bin/post";
const PARTICIPANT_SETUP_FAILED =
  "[post] participant setup failed; inbox state is UNKNOWN (not empty). Retry setup or run: post participant bind";
const LIFECYCLE_WARNING =
  "[post] participant lifecycle update unavailable; continuing without presence refresh";

function writeAllSync(fd, data) {
  const buf = Buffer.isBuffer(data) ? data : Buffer.from(data);
  let offset = 0;
  while (offset < buf.length) {
    const n = fs.writeSync(fd, buf, offset, buf.length - offset);
    if (n <= 0) throw new Error("short write");
    offset += n;
  }
}

function emit(payload) {
  writeAllSync(1, JSON.stringify(payload));
}

function tryEmit(payload) {
  try {
    emit(payload);
    return true;
  } catch {
    return false;
  }
}

function stateDir() {
  return process.env.POST_GROK_HOOK_STATE_DIR || path.join(os.tmpdir(), "post-grok-mail");
}

function readState(file) {
  try {
    const parsed = JSON.parse(fs.readFileSync(file, "utf8"));
    return {
      seen: Array.isArray(parsed.seen) ? parsed.seen.filter((k) => typeof k === "string") : [],
      failStreak: Number.isInteger(parsed.failStreak) ? parsed.failStreak : 0,
      initialized: parsed.initialized === true,
      participantId: typeof parsed.participantId === "string" ? parsed.participantId : null,
      lifecycleWarned: parsed.lifecycleWarned === true,
    };
  } catch {
    return { seen: [], failStreak: 0, initialized: false, participantId: null, lifecycleWarned: false };
  }
}

function writeState(file, state) {
  try {
    fs.mkdirSync(path.dirname(file), { recursive: true });
    const dir = path.dirname(file);
    const tmp = path.join(
      dir,
      `.${path.basename(file)}.${process.pid}.${randomBytes(8).toString("hex")}.tmp`
    );
    let fd;
    try {
      const flags =
        fs.constants.O_WRONLY |
        fs.constants.O_CREAT |
        fs.constants.O_EXCL |
        (fs.constants.O_NOFOLLOW || 0);
      fd = fs.openSync(tmp, flags, 0o600);
      writeAllSync(fd, JSON.stringify(state));
      fs.closeSync(fd);
      fd = undefined;
      fs.renameSync(tmp, file);
    } catch (error) {
      if (fd !== undefined) {
        try {
          fs.closeSync(fd);
        } catch {
          // Best-effort close before unlink.
        }
      }
      try {
        fs.unlinkSync(tmp);
      } catch {
        // Only this process's temp; ignore if create failed first.
      }
      throw error;
    }
  } catch {
    // Fail-open: lost state means at worst a duplicate reminder.
  }
}

function postBinary() {
  if (process.env.POST_GROK_HOOK_BIN) return process.env.POST_GROK_HOOK_BIN;
  const installed = path.join(os.homedir(), ".local", "bin", "post");
  try {
    fs.accessSync(installed, fs.constants.X_OK);
    return installed;
  } catch {
    return "post";
  }
}

const LEGACY_CHANNEL_EPISODE = "legacy-channel-episode";
const LEGACY_WARNING = "Post compatibility warning: unreadable channel data from an older Post lacks channel identity. Per-message delivery is unknown; upgrade Post.";

function eventKey(event) {
  if (event.event === "unreadable" && event.reason === "channel") {
    // Presence-only episode, not a per-message acknowledgement.
    return event.channel === undefined ? LEGACY_CHANNEL_EPISODE : JSON.stringify(["unreadable", event.channel, event.id]);
  }
  if (event.event === "channel_message") return `channel:${event.channel}:${event.id}`;
  return `${event.event}:${event.room}:${event.id}`;
}

function safeName(value) {
  return typeof value === "string" && value.length <= NAME_MAX && ROOM_NAME.test(value);
}

// This field is identity-only, never rendered; accept Post's path-safe Unicode
// channel namespace rather than the narrower model-facing name alphabet.
function safeUnreadableChannel(value) {
  return safeUnreadableId(value) && Buffer.byteLength(value, "utf8") <= NAME_MAX &&
    value !== "." && value !== ".." && !/[\\/\\\\\u0080-\u009f]/.test(value);
}

function safeUnreadableId(value) {
  return (
    typeof value === "string" &&
    value.length > 0 &&
    value.length <= UNREADABLE_ID_MAX &&
    !CONTROL_CHARS.test(value)
  );
}

function formatBoundedList(items, remainderLabel) {
  const listed = items.slice(0, LIST_CAP);
  const text = listed.join(", ");
  if (listed.length === items.length) return text;
  return `${text}; +${items.length - listed.length} ${remainderLabel}`;
}

function channelSummary(channel) {
  const counts = new Map();
  for (const e of channel) counts.set(e.channel, (counts.get(e.channel) ?? 0) + 1);
  const entries = [...counts].map(([name, n]) => `#${name} (${n})`);
  return formatBoundedList(entries, "more");
}

function contextFor(events) {
  if (events.some((event) => eventKey(event) === LEGACY_CHANNEL_EPISODE)) {
    const ordinary = events.filter((event) => eventKey(event) !== LEGACY_CHANNEL_EPISODE);
    return (LEGACY_WARNING + (ordinary.length ? "\n" + contextFor(ordinary) : "")).slice(0, CONTEXT_MAX);
  }
  const mail = events.filter((e) => e.event === "mail");
  const channel = events.filter((e) => e.event === "channel_message");
  const unreadable = events.filter((e) => e.event === "unreadable");
  const room = mail[0]?.room ?? unreadable[0]?.room;
  const channelOnly = mail.length === 0 && unreadable.length === 0;
  const framing = [
    "Reading is optional. Inspection commands, run from the project directory: post inbox; post read <id>; post channels; post chat <channel> --peek.",
  ];

  function build({ includeIds, includeChannels, includeRoom }) {
    const lines = [
      channelOnly
        ? `[post] New channel message(s): ${includeChannels ? channelSummary(channel) : `${channel.length} item(s)`}.`
        : includeRoom && room
          ? `[post] Unread agent mail is waiting for room ${room} (resolved from this session's working directory).`
          : "[post] Unread agent mail is waiting for this session's mail room.",
    ];
    if (mail.length > 0) {
      lines.push(
        includeIds
          ? `Direct mail id(s): ${formatBoundedList(
              mail.map((e) => e.id),
              "more"
            )}.`
          : `Direct mail: ${mail.length} item(s).`
      );
    }
    if (channel.length > 0 && !channelOnly) {
      lines.push(
        includeChannels
          ? `New channel message(s): ${channelSummary(channel)}.`
          : `New channel message(s): ${channel.length} item(s).`
      );
    }
    if (unreadable.length > 0) {
      lines.push(`Unreadable mail: ${unreadable.length} item(s).`);
    }
    lines.push(...framing);
    return lines.join("\n");
  }

  let context = build({ includeIds: true, includeChannels: true, includeRoom: true });
  if (Buffer.byteLength(context, "utf8") <= CONTEXT_MAX) return context;
  context = build({ includeIds: false, includeChannels: false, includeRoom: false });
  if (Buffer.byteLength(context, "utf8") <= CONTEXT_MAX) return context;
  return framing.join("\n").slice(0, CONTEXT_MAX);
}

function isStringFields(event, fields) {
  return fields.every((field) => typeof event[field] === "string");
}

function validSnapshotEvent(event) {
  if (!event || typeof event !== "object" || Array.isArray(event)) return false;
  switch (event.event) {
    case "mail":
      return (
        isStringFields(event, ["room", "id", "from", "kind", "subject", "sent", "reason"]) &&
        event.reason === "mail" &&
        safeName(event.room) &&
        MAIL_ID.test(event.id)
      );
    case "channel_message":
      return (
        isStringFields(event, ["channel", "id", "from", "subject", "sent", "reason"]) &&
        (event.reason === "channel" || event.reason === "mention") &&
        safeName(event.channel) &&
        CHANNEL_ID.test(event.id)
      );
    case "unreadable":
      return (
        isStringFields(event, ["room", "id", "reason"]) &&
        (event.reason === "mail" || event.reason === "channel") &&
        (event.reason !== "channel" || event.channel === undefined || safeUnreadableChannel(event.channel)) &&
        safeName(event.room) &&
        safeUnreadableId(event.id)
      );
    default:
      return false;
  }
}

function readStdin() {
  try {
    return fs.readFileSync(0, "utf8");
  } catch {
    return "";
  }
}

function failDiagnostic() {
  return {
    hookSpecificOutput: {
      hookEventName: CANONICAL_EVENT,
      additionalContext:
        "[post] The automatic mail check failed; inbox state is UNKNOWN (not empty). " +
        "Manual check, from the project directory: post inbox",
    },
  };
}

function runPost(args, cwd, { participantId = null, clearParticipant = false, clearConversationKeys = false, deadline = null } = {}) {
  const env = { ...process.env };
  if (participantId) env.POST_PARTICIPANT = participantId;
  else if (clearParticipant || env.POST_PARTICIPANT === "") delete env.POST_PARTICIPANT;
  if (clearConversationKeys) {
    delete env.CLAUDE_CODE_SESSION_ID;
    delete env.CODEX_THREAD_ID;
    delete env.CODEX_SESSION_ID;
    delete env.POST_SENDER_ADDRESS;
  }
  return spawnSync(postBinary(), args, {
    cwd,
    encoding: "utf8",
    timeout: deadline === null ? 4000 : Math.max(1, Math.min(4000, deadline - Date.now())),
    env,
    stdio: ["ignore", "pipe", "ignore"],
  });
}

function versionFailure(result) {
  if (result?.error || result?.status !== 0) return VERSION_PROBE_FAILED;
  try {
    const value = JSON.parse(String(result.stdout ?? ""));
    if (!Array.isArray(value.capabilities)) return VERSION_PROBE_FAILED;
    return value.capabilities.includes("participants") ? null : PARTICIPANTS_MISSING;
  } catch {
    return VERSION_PROBE_FAILED;
  }
}

function boundParticipantId(result) {
  if (result?.error || result?.status !== 0) return null;
  try {
    const value = JSON.parse(String(result.stdout ?? ""));
    if (value?.ok !== true || value?.status !== "bound") return null;
    const participant = value?.participant;
    const id = participant?.id ?? value?.id;
    return safeIdentityPart(id) ? id : null;
  } catch {
    return null;
  }
}

function identityLine(result) {
  if (result?.error || result?.status !== 0) return null;
  try {
    const value = JSON.parse(String(result.stdout ?? ""));
    if (value?.ok !== true || value?.status !== "bound") return null;
    const participant = value?.participant;
    const id = participant?.id ?? value?.id;
    const lineage = participant?.lineage ?? value?.lineage;
    if (!safeIdentityPart(id) || !safeIdentityPart(lineage)) return null;
    const render = (name) => `[post] participant ${id}, continuing lineage ${name}; voices on request: post identity show ${shellQuote(name)} --voices`;
    if (Buffer.byteLength(render(lineage), "utf8") <= 256) return render(lineage);
    const prefix = `[post] participant ${id}, continuing lineage `;
    const suffix = "; voices on request: post identity show --help";
    const available = 256 - Buffer.byteLength(prefix + suffix, "utf8") - Buffer.byteLength("…", "utf8");
    if (available <= 0) return null;
    const chars = [...lineage];
    while (chars.length > 0 && Buffer.byteLength(chars.join(""), "utf8") > available) chars.pop();
    return `${prefix}${chars.join("")}…${suffix}`;
  } catch {
    return null;
  }
}

function bindingLine(id) {
  const line = safeIdentityPart(id)
    ? `[post] participant ${id}; prefix Post commands with POST_PARTICIPANT=${id}`
    : null;
  return line && Buffer.byteLength(line, "utf8") <= 256 ? line : null;
}

function shellQuote(value) {
  return `'${value.replaceAll("'", "'\\''")}'`;
}

function safeIdentityPart(value) {
  return (
    typeof value === "string" &&
    value.length > 0 &&
    Buffer.byteLength(value, "utf8") <= IDENTITY_PART_MAX &&
    !CONTROL_CHARS.test(value) &&
    !/[\u2028\u2029\r\n]/.test(value) &&
    value !== "." &&
    value !== ".." &&
    !/[\\/]/.test(value)
  );
}

function appendLine(context, line) {
  if (!line) return context;
  const merged = context ? `${context}\n${line}` : line;
  return Buffer.byteLength(merged, "utf8") <= MERGED_CONTEXT_MAX ? merged : context;
}

function setupPayload(context) {
  return {
    hookSpecificOutput: {
      hookEventName: CANONICAL_EVENT,
      additionalContext: context,
    },
  };
}

function setupParticipant(cwd, sessionId, deadline) {
  const explicit = typeof process.env.POST_PARTICIPANT === "string" && process.env.POST_PARTICIPANT.trim();
  const args = explicit
    ? ["participant", "bind", "--json"]
    : ["participant", "bind", "--harness", HARNESS, "--key", sessionId, "--json"];
  return boundParticipantId(runPost(args, cwd, { clearParticipant: !explicit, clearConversationKeys: true, deadline }));
}

function participantConflict(sessionId, explicit) {
  if (!explicit) return false;
  const digest = createHash("sha256").update(sessionId).digest("hex");
  return explicit !== `${HARNESS}-${digest.slice(0, 8)}` && explicit !== `${HARNESS}-${digest.slice(0, 12)}`;
}

function lifecycleWarning(cwd, participantId, command, deadline) {
  const result = runPost(["participant", command], cwd, { participantId, clearConversationKeys: true, deadline });
  return Boolean(result?.error || result?.status !== 0);
}

function deliverThenCommit(stateFile, payload, nextState) {
  if (!tryEmit(payload)) return;
  writeState(stateFile, nextState);
}

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

function main() {
  let input;
  try {
    input = JSON.parse(readStdin());
  } catch {
    return tryEmit({});
  }
  if (input === null || typeof input !== "object") return tryEmit({});
  const eventName = eventNameOf(input);
  if (!EVENTS.has(eventName)) return tryEmit({});
  if (isSubagent(input)) return tryEmit({});

  const sessionRaw = sessionIdOf(input);
  if (sessionRaw === null) return tryEmit({});
  const cwd = resolveCwd(input);
  if (cwd === null) return tryEmit({});
  const sessionId = sessionRaw.replace(/[^A-Za-z0-9._-]/g, "_");
  const stateFile = path.join(stateDir(), `session-${sessionId}.json`);
  const state = readState(stateFile);

  // Grok exposes only UserPromptSubmit; treat the first prompt as SessionStart
  // for participant setup and capability gating.
  const firstPrompt = !state.initialized || !state.participantId;
  const deadline = firstPrompt ? Date.now() + SESSION_DEADLINE_MS : null;
  const explicit = typeof process.env.POST_PARTICIPANT === "string" && process.env.POST_PARTICIPANT.trim();
  if (participantConflict(sessionRaw, explicit)) {
    tryEmit(setupPayload("[post] POST_PARTICIPANT conflicts with this hook session key; unset it to bind from the payload or use the matching participant id"));
    return;
  }
  const needsSetup = !state.initialized || !state.participantId || (explicit && explicit !== state.participantId);
  let participantId = state.participantId;
  let setupPerformed = false;
  if (needsSetup) {
    const versionError = versionFailure(runPost(["version", "--json"], cwd, { clearConversationKeys: true, deadline }));
    if (versionError) {
      tryEmit(setupPayload(versionError));
      return;
    }
    participantId = setupParticipant(cwd, sessionRaw, deadline);
    if (!participantId) {
      tryEmit(setupPayload(PARTICIPANT_SETUP_FAILED));
      return;
    }
    setupPerformed = true;
  }

  const touchFailed = participantId ? lifecycleWarning(cwd, participantId, "touch", deadline) : false;
  const result = runPost(["watch", "--snapshot"], cwd, { participantId, clearConversationKeys: true, deadline });

  if (result.error || result.status !== 0) {
    const nextState = {
      ...state,
      participantId,
      lifecycleWarned: state.lifecycleWarned || touchFailed,
      failStreak: state.failStreak + 1,
      initialized: state.initialized,
    };
    const payload = nextState.failStreak === 1 ? failDiagnostic() : {};
    const identity = firstPrompt ? identityLine(runPost(["participant", "show", "--json"], cwd, { participantId, clearConversationKeys: true, deadline })) : null;
    const context = appendLine(appendLine(appendLine(payload?.hookSpecificOutput?.additionalContext ?? "", setupPerformed ? bindingLine(participantId) : null), identity), !state.lifecycleWarned && touchFailed ? LIFECYCLE_WARNING : null);
    deliverThenCommit(stateFile, context ? setupPayload(context) : payload, nextState);
    return;
  }

  const events = [];
  let malformed = false;
  for (const line of String(result.stdout ?? "").split("\n")) {
    if (!line.trim()) continue;
    try {
      const event = JSON.parse(line);
      if (!validSnapshotEvent(event)) malformed = true;
      else events.push(event);
    } catch {
      malformed = true;
    }
  }
  if (malformed) {
    const nextState = {
      ...state,
      participantId,
      lifecycleWarned: state.lifecycleWarned || touchFailed,
      failStreak: state.failStreak + 1,
      initialized: state.initialized,
    };
    const payload = nextState.failStreak === 1 ? failDiagnostic() : {};
    const identity = firstPrompt ? identityLine(runPost(["participant", "show", "--json"], cwd, { participantId, clearConversationKeys: true, deadline })) : null;
    const context = appendLine(appendLine(appendLine(payload?.hookSpecificOutput?.additionalContext ?? "", setupPerformed ? bindingLine(participantId) : null), identity), !state.lifecycleWarned && touchFailed ? LIFECYCLE_WARNING : null);
    deliverThenCommit(stateFile, context ? setupPayload(context) : payload, nextState);
    return;
  }

  const seen = new Set(state.seen);
  const fresh = events.filter((event) => !seen.has(eventKey(event)));
  const nextState = {
    seen: [...new Set(events.map((event) => eventKey(event)))],
    failStreak: 0,
    initialized: true,
    participantId,
    lifecycleWarned: state.lifecycleWarned || touchFailed,
  };
  const identity = firstPrompt
    ? identityLine(runPost(["participant", "show", "--json"], cwd, { participantId, clearConversationKeys: true, deadline }))
    : null;
  const context = appendLine(appendLine(appendLine(fresh.length === 0 ? "" : contextFor(fresh), setupPerformed ? bindingLine(participantId) : null), identity), !state.lifecycleWarned && touchFailed ? LIFECYCLE_WARNING : null);
  const payload = context
    ? { hookSpecificOutput: { hookEventName: CANONICAL_EVENT, additionalContext: context } }
    : {};
  deliverThenCommit(stateFile, payload, nextState);
}

try {
  main();
} catch {
  tryEmit({});
}
process.exit(0);
