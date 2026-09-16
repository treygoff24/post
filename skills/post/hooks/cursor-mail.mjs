#!/usr/bin/env node
// Cursor CLI hook adapter: injects metadata-only "new post mail" notifications
// into model context at sessionStart / beforeSubmitPrompt / root postToolUse.
// The Cursor twin of claude-mail.mjs; adapter recipe and contract: docs/ADAPTERS.md.
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
// - sessionStart resets per-session dedupe so a resume still reminds.
//
// Shared invariants: envelope-metadata only, 30s postToolUse throttle, one
// diagnostic per failure streak, fail-open, always exit 0, commit state only
// after a successful synchronous stdout write, exclusive random-named state
// temps so a planted *.pid.tmp symlink cannot redirect the write.
//
// Test overrides (all optional):
//   POST_CURSOR_HOOK_BIN         path to the post binary
//   POST_CURSOR_HOOK_STATE_DIR   state directory (default <tmpdir>/post-cursor-mail)
//   POST_CURSOR_HOOK_THROTTLE_MS postToolUse throttle (default 30000)

import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { randomBytes } from "node:crypto";
import { spawnSync } from "node:child_process";

const THROTTLE_MS = Number(process.env.POST_CURSOR_HOOK_THROTTLE_MS ?? 30_000);
const EVENTS = new Set(["sessionStart", "beforeSubmitPrompt", "postToolUse"]);
const LIST_CAP = 20;
const CONTEXT_MAX = 4096;
const MERGED_CONTEXT_MAX = CONTEXT_MAX + 256;
const NAME_MAX = 255;
const UNREADABLE_ID_MAX = 255;
const MAIL_ID = /^\d{8}-\d{6}-[0-9a-fA-F]{6}$/;
const CHANNEL_ID = /^\d{8}-\d{6}-\d{6}-[0-9a-fA-F]{6}$/;
const ROOM_NAME = /^[A-Za-z0-9._-]+$/;
const CONTROL_CHARS = /[\u0000-\u001f\u007f]/;
const HARNESS = "cursor";
const VERSION_PROBE_FAILED =
  "[post] could not verify installed post capabilities (version query failed or timed out); repair: cd ~/Code/post && cargo build --release && install -m 0755 target/release/post ~/.local/bin/post";
const PARTICIPANTS_MISSING =
  "[post] installed post lacks the participants capability; repair: cd ~/Code/post && cargo build --release && install -m 0755 target/release/post ~/.local/bin/post";
const PARTICIPANT_SETUP_FAILED =
  "[post] participant setup failed; inbox state is UNKNOWN (not empty). Retry setup or run: post participant bind";

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
  return process.env.POST_CURSOR_HOOK_STATE_DIR || path.join(os.tmpdir(), "post-cursor-mail");
}

function readState(file) {
  try {
    const parsed = JSON.parse(fs.readFileSync(file, "utf8"));
    return {
      seen: Array.isArray(parsed.seen) ? parsed.seen.filter((k) => typeof k === "string") : [],
      failStreak: Number.isInteger(parsed.failStreak) ? parsed.failStreak : 0,
      participantId: typeof parsed.participantId === "string" ? parsed.participantId : null,
    };
  } catch {
    return { seen: [], failStreak: 0, participantId: null };
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
  if (process.env.POST_CURSOR_HOOK_BIN) return process.env.POST_CURSOR_HOOK_BIN;
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

function noticePayload(eventName, context) {
  return {
    additional_context: context,
    hookSpecificOutput: {
      hookEventName: eventName,
      additionalContext: context,
    },
  };
}

function failDiagnostic(eventName) {
  return noticePayload(
    eventName,
    "[post] The automatic mail check failed; inbox state is UNKNOWN (not empty). " +
      "Manual check, from the project directory: post inbox"
  );
}

function runPost(args, cwd, { participantId = null, clearParticipant = false, clearConversationKeys = false } = {}) {
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
    timeout: 4000,
    env,
    stdio: ["ignore", "pipe", "ignore"],
  });
}

function versionFailure(result) {
  if (result?.error || result?.status !== 0) return VERSION_PROBE_FAILED;
  try {
    const value = JSON.parse(String(result.stdout ?? ""));
    if (value?.ok !== true || !Array.isArray(value.capabilities)) return VERSION_PROBE_FAILED;
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
    return `[post] participant ${id}, continuing lineage ${lineage}; voices on request: post identity show ${shellQuote(lineage)} --voices; bootstrap: export POST_PARTICIPANT=${id}`;
  } catch {
    return null;
  }
}

function shellQuote(value) {
  return `'${value.replaceAll("'", "'\\''")}'`;
}

function safeIdentityPart(value) {
  return (
    typeof value === "string" &&
    value.length > 0 &&
    Buffer.byteLength(value, "utf8") <= NAME_MAX &&
    !CONTROL_CHARS.test(value) &&
    !/[\u2028\u2029\r\n]/.test(value) &&
    value !== "." &&
    value !== ".." &&
    !/[\\/]/.test(value)
  );
}

function appendIdentity(context, line) {
  if (!line || Buffer.byteLength(line, "utf8") > 256) return context;
  const merged = context ? `${context}\n${line}` : line;
  return Buffer.byteLength(merged, "utf8") <= MERGED_CONTEXT_MAX ? merged : context;
}

function setupPayload(eventName, context) {
  return {
    additional_context: context,
    hookSpecificOutput: {
      hookEventName: eventName,
      additionalContext: context,
    },
  };
}

function setupParticipant(cwd, sessionId) {
  const explicit = typeof process.env.POST_PARTICIPANT === "string" && process.env.POST_PARTICIPANT.trim();
  const args = explicit
    ? ["participant", "bind", "--json"]
    : ["participant", "bind", "--harness", HARNESS, "--key", sessionId, "--json"];
  return boundParticipantId(runPost(args, cwd, { clearParticipant: !explicit, clearConversationKeys: true }));
}

function deliverThenCommit(stateFile, payload, nextState) {
  if (!tryEmit(payload)) return;
  writeState(stateFile, nextState);
}

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

function main() {
  let input;
  try {
    input = JSON.parse(readStdin());
  } catch {
    return tryEmit({});
  }
  if (input === null || typeof input !== "object") return tryEmit({});
  const eventName = input.hook_event_name;
  if (!EVENTS.has(eventName)) return tryEmit({});
  if (isSubagent(input)) return tryEmit({});

  const sessionRaw = sessionIdOf(input);
  if (sessionRaw === null) return tryEmit({});
  const cwd = resolveCwd(input);
  if (cwd === null) return tryEmit({});
  const sessionId = sessionRaw.replace(/[^A-Za-z0-9._-]/g, "_");
  const stateFile = path.join(stateDir(), `session-${sessionId}.json`);

  const state = eventName === "sessionStart" ? { seen: [], failStreak: 0, participantId: null } : readState(stateFile);
  const explicit = typeof process.env.POST_PARTICIPANT === "string" && process.env.POST_PARTICIPANT.trim();
  const needsSetup = eventName === "sessionStart" || !state.participantId || (explicit && explicit !== state.participantId);
  let participantId = state.participantId;
  if (needsSetup) {
    const versionError = versionFailure(runPost(["version", "--json"], cwd, { clearConversationKeys: true }));
    if (versionError) {
      tryEmit(setupPayload(eventName, versionError));
      return;
    }
    participantId = setupParticipant(cwd, sessionRaw);
    if (!participantId) {
      tryEmit(setupPayload(eventName, PARTICIPANT_SETUP_FAILED));
      return;
    }
  }

  if (eventName === "postToolUse") {
    try {
      if (Date.now() - fs.statSync(stateFile).mtimeMs < THROTTLE_MS) return tryEmit({});
    } catch {
      // no state yet: scan
    }
  }

  const result = runPost(["watch", "--snapshot"], cwd, { participantId, clearConversationKeys: true });

  if (result.error || result.status !== 0) {
    const nextState = { ...state, participantId, failStreak: state.failStreak + 1 };
    const payload = nextState.failStreak === 1 ? failDiagnostic(eventName) : {};
    deliverThenCommit(stateFile, payload, nextState);
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
    const nextState = { ...state, participantId, failStreak: state.failStreak + 1 };
    const payload = nextState.failStreak === 1 ? failDiagnostic(eventName) : {};
    deliverThenCommit(stateFile, payload, nextState);
    return;
  }

  const seen = new Set(state.seen);
  const fresh = events.filter((event) => !seen.has(eventKey(event)));
  const nextState = {
    seen: [...new Set(events.map((event) => eventKey(event)))],
    failStreak: 0,
    participantId,
  };
  let identity = null;
  if (eventName === "sessionStart") {
    identity = identityLine(runPost(["participant", "show", "--json"], cwd, { participantId, clearConversationKeys: true }));
  }
  const context = appendIdentity(fresh.length === 0 ? "" : contextFor(fresh), identity);
  const payload = context ? noticePayload(eventName, context) : {};
  deliverThenCommit(stateFile, payload, nextState);
}

try {
  main();
} catch {
  tryEmit({});
}
process.exit(0);
