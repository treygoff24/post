#!/usr/bin/env node
// Codex hook adapter: injects metadata-only "new post mail" notifications into
// model context at SessionStart / UserPromptSubmit / root PostToolUse.
//
// Contract (see docs/ADAPTERS.md for the full adapter recipe):
// - runs `post watch --snapshot` from the hook cwd, so post resolves the
//   deepest registered room itself (read-only, envelope-only);
// - subagent PostToolUse events are suppressed;
// - PostToolUse scans are throttled to one per 30s via state-file mtime;
// - per-session dedupe keyed by session_id; SessionStart resets it;
// - a failed scan emits ONE generic diagnostic per failure streak — never a
//   fake empty inbox, never a per-event flood;
// - strictly fail-open: any internal error emits `{}` and exits 0;
// - injected context carries valid direct-mail ids and count-only summaries
//   for channel/unreadable mail — no subject, sender, body, or filename data;
// - listed ids/channel names are capped; the mail notice stays under 4 KiB,
//   and the optional participant line keeps merged context under 4352 bytes;
// - dedupe/fail-streak state commits only after a successful synchronous
//   stdout write of the final JSON payload (all bytes on fd 1).

// Test overrides (all optional):
//   POST_CODEX_HOOK_BIN         path to the post binary
//   POST_CODEX_HOOK_STATE_DIR   state directory (default <tmpdir>/post-codex-mail)
//   POST_CODEX_HOOK_THROTTLE_MS PostToolUse throttle (default 30000)

import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { randomBytes } from "node:crypto";
import { spawnSync } from "node:child_process";

const THROTTLE_MS = Number(process.env.POST_CODEX_HOOK_THROTTLE_MS ?? 30_000);
const EVENTS = new Set(["SessionStart", "UserPromptSubmit", "PostToolUse"]);
const LIST_CAP = 20;
const CONTEXT_MAX = 4096;
const MERGED_CONTEXT_MAX = CONTEXT_MAX + 256;
const NAME_MAX = 255;
const UNREADABLE_ID_MAX = 255; // filename-derived stem bound
const MAIL_ID = /^\d{8}-\d{6}-[0-9a-fA-F]{6}$/;
const CHANNEL_ID = /^\d{8}-\d{6}-\d{6}-[0-9a-fA-F]{6}$/;
const ROOM_NAME = /^[A-Za-z0-9._-]+$/;
const CONTROL_CHARS = /[\u0000-\u001f\u007f]/;
const REPAIR_LINE =
  "[post] installed post lacks the participants capability; repair: cargo build --release && install -m 0755 target/release/post ~/.local/bin/post";

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
  // Synchronous fd write: delivery success is known before any state commit.
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
  return process.env.POST_CODEX_HOOK_STATE_DIR || path.join(os.tmpdir(), "post-codex-mail");
}

function readState(file) {
  try {
    const parsed = JSON.parse(fs.readFileSync(file, "utf8"));
    return {
      seen: Array.isArray(parsed.seen) ? parsed.seen.filter((k) => typeof k === "string") : [],
      failStreak: Number.isInteger(parsed.failStreak) ? parsed.failStreak : 0,
    };
  } catch {
    return { seen: [], failStreak: 0 };
  }
}

// Atomic replace: exclusive unique temp in the destination dir, then rename.
// Never follows a planted predictable *.pid.tmp symlink.
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
  if (process.env.POST_CODEX_HOOK_BIN) return process.env.POST_CODEX_HOOK_BIN;
  const installed = path.join(os.homedir(), ".local", "bin", "post");
  try {
    fs.accessSync(installed, fs.constants.X_OK);
    return installed;
  } catch {
    return "post"; // PATH fallback
  }
}

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

// "#general (3), #ops (1)" — channel names are validated before acceptance.
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
    "Inspection commands, run from the project directory: post inbox; post read <id>; post channels; post chat <channel> --peek.",
  ];

  function build({ includeIds, includeChannels, includeRoom }) {
    const lines = [
      channelOnly
        ? `[post] New channel message(s): ${includeChannels ? channelSummary(channel) : `${channel.length} item(s)`}.`
        : includeRoom && room
          ? `[post] New mail is waiting for room ${room} (resolved from this session's working directory).`
          : "[post] New mail is waiting for this session's mail room.",
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
  // Omit overlong metadata rather than echoing unbounded strings.
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

function failDiagnostic(eventName) {
  return {
    hookSpecificOutput: {
      hookEventName: eventName,
      additionalContext:
        "[post] The automatic mail check failed; inbox state is UNKNOWN (not empty). " +
        "Check manually from the project directory with: post inbox",
    },
  };
}

function runPost(args, cwd) {
  return spawnSync(postBinary(), args, {
    cwd,
    encoding: "utf8",
    timeout: 4000,
    stdio: ["ignore", "pipe", "ignore"],
  });
}

function hasParticipantsCapability(result) {
  if (result?.error || result?.status !== 0) return false;
  try {
    const value = JSON.parse(String(result.stdout ?? ""));
    return Array.isArray(value.capabilities) && value.capabilities.includes("participants");
  } catch {
    return false;
  }
}

function identityLine(result) {
  if (result?.error || result?.status !== 0) return null;
  try {
    const value = JSON.parse(String(result.stdout ?? ""));
    if (value?.status !== "bound") return null;
    const participant = value?.participant;
    const id = participant?.id ?? value?.id;
    const lineage = participant?.lineage ?? value?.lineage;
    if (!safeIdentityPart(id) || !safeIdentityPart(lineage)) return null;
    return `[post] participant ${id}, continuing lineage ${lineage}; voices on request: post identity show ${lineage} --voices`;
  } catch {
    return null;
  }
}

function safeIdentityPart(value) {
  return (
    typeof value === "string" &&
    value.length > 0 &&
    Buffer.byteLength(value, "utf8") <= NAME_MAX &&
    !CONTROL_CHARS.test(value) &&
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

function repairPayload(eventName) {
  return {
    hookSpecificOutput: {
      hookEventName: eventName,
      additionalContext: REPAIR_LINE,
    },
  };
}

function deliverThenCommit(stateFile, payload, nextState) {
  if (!tryEmit(payload)) return;
  writeState(stateFile, nextState);
}

function main() {
  let input;
  try {
    input = JSON.parse(readStdin());
  } catch {
    tryEmit({});
    return;
  }
  if (input === null || typeof input !== "object") {
    tryEmit({});
    return;
  }
  const eventName = input.hook_event_name;
  if (!EVENTS.has(eventName)) {
    tryEmit({});
    return;
  }
  if (eventName === "PostToolUse" && isSubagent(input)) {
    tryEmit({});
    return;
  }

  if (typeof input.session_id !== "string" || input.session_id.trim() === "") {
    tryEmit({});
    return;
  }
  if (typeof input.cwd !== "string" || !path.isAbsolute(input.cwd)) {
    tryEmit({});
    return;
  }
  const sessionId = input.session_id.replace(/[^A-Za-z0-9._-]/g, "_");
  const stateFile = path.join(stateDir(), `session-${sessionId}.json`);

  if (eventName === "PostToolUse") {
    try {
      if (Date.now() - fs.statSync(stateFile).mtimeMs < THROTTLE_MS) {
        tryEmit({});
        return;
      }
    } catch {
      // no state yet: scan
    }
  }

  if (eventName === "SessionStart") {
    if (!hasParticipantsCapability(runPost(["version", "--json"], input.cwd))) {
      tryEmit(repairPayload(eventName));
      return;
    }
    runPost(["participant", "bind"], input.cwd);
  }
  const state = eventName === "SessionStart" ? { seen: [], failStreak: 0 } : readState(stateFile);

  const result = runPost(["watch", "--snapshot"], input.cwd);

  if (result.error || result.status !== 0) {
    const nextState = { ...state, failStreak: state.failStreak + 1 };
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
    const nextState = { ...state, failStreak: state.failStreak + 1 };
    const payload = nextState.failStreak === 1 ? failDiagnostic(eventName) : {};
    deliverThenCommit(stateFile, payload, nextState);
    return;
  }

  const seen = new Set(state.seen);
  const fresh = events.filter((event) => !seen.has(eventKey(event)));
  // Persist the exact current snapshot keys, not prior∪fresh sliced: a cap
  // below the backlog size would drop a still-unread key each run and re-ring
  // it forever. Consumed ids leave the snapshot and prune themselves.
  const nextState = {
    seen: [...new Set(events.map((event) => eventKey(event)))],
    failStreak: 0,
  };
  // Written after a successful emit even when nothing is new: the file's mtime
  // is the PostToolUse throttle clock.
  let identity = null;
  if (eventName === "SessionStart") {
    identity = identityLine(runPost(["participant", "show", "--json"], input.cwd));
  }
  const context = appendIdentity(fresh.length === 0 ? "" : contextFor(fresh), identity);
  const payload = context
    ? { hookSpecificOutput: { hookEventName: eventName, additionalContext: context } }
    : {};
  deliverThenCommit(stateFile, payload, nextState);
}

try {
  main();
} catch {
  tryEmit({});
}
process.exit(0);
