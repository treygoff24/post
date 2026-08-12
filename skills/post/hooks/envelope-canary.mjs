#!/usr/bin/env node
// Adapter envelope canary (identity M2) — SOURCE-consumer verification.
//
// Scope, stated honestly: this proves all five SOURCE consumers (the four
// harness adapter scripts — Claude Code, Codex, Cursor, Grok — plus
// watch-notice) accept real Post events carrying the identity fields, under
// SYNTHETIC hook invocation against a locally built binary. It does NOT
// verify installed hook registrations in any live harness, and Grok's live
// doorbell cell stays an explicit gap until grok returns (Trey's two-way
// instruction while grok is down).
//
// What it does: in a throwaway HOME/mail root (the real mailbox is never
// touched), sends one direct mail and one @mention channel message under
// POST_FROM/POST_SENDER_ADDRESS with target/release/post, then asserts —
// by parsing the watch-snapshot NDJSON, never by grepping — the exact event
// classes, and drives each consumer to its NORMAL notice output.
//
// Ambient-identity immunity is a required receipt, not an option: this
// script POISONS ITS OWN environment with an ambient pin/address at startup,
// so every run is the poisoned run. Setup, watcher, adapter, and
// watch-notice processes get explicitly cleaned environments; only the three
// deliberate sender commands carry the declared identity. Any ambient value
// reaching an envelope fails the run.
//
//     cargo build --release && node skills/post/hooks/envelope-canary.mjs
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const HOOKS = path.dirname(fileURLToPath(import.meta.url));
const REPO = path.resolve(HOOKS, "../../..");
const BIN = path.join(REPO, "target/release/post");

// Deliberate self-poisoning: prove immunity on every run.
process.env.POST_FROM = "ambient-ghost";
process.env.POST_SENDER_ADDRESS = "ambient.ghost.deadbeef";

const WORK = fs.mkdtempSync(path.join(os.tmpdir(), "post-m2-canary-"));
const SANDBOX = path.join(WORK, "home");
const MAIL = path.join(WORK, "mail");
const WATCHER_DIR = path.join(SANDBOX, "watcher-room");
fs.mkdirSync(WATCHER_DIR, { recursive: true });
fs.mkdirSync(path.join(SANDBOX, "sender-room"), { recursive: true });

const ADDRESS = "claude-code.m2.0123abcd";
const AMBIENT_ADDRESS = "ambient.ghost.deadbeef";
let failures = 0;

function receipt(ok, label, detail = "") {
  if (ok) {
    console.log(`receipt: ${label}`);
  } else {
    failures += 1;
    console.error(`FAIL: ${label}${detail ? ` — ${detail}` : ""}`);
  }
}

/// Environment with BOTH identity variables removed, then `extra` applied.
/// Every process in this canary goes through here, so nothing ever inherits
/// the (deliberately poisoned) ambient identity by accident.
function cleanEnv(extra = {}) {
  const env = { ...process.env, HOME: SANDBOX, POST_MAIL_ROOT: MAIL };
  delete env.POST_FROM;
  delete env.POST_SENDER_ADDRESS;
  return { ...env, ...extra };
}

function run(cmd, args, { cwd = WORK, env = cleanEnv(), input } = {}) {
  const result = spawnSync(cmd, args, {
    cwd,
    env,
    encoding: "utf8",
    input,
    timeout: 15000,
  });
  if (result.error) throw result.error;
  return result;
}

function post(args, opts = {}) {
  const result = run(BIN, args, opts);
  if (result.status !== 0) {
    throw new Error(`post ${args.join(" ")} failed rc=${result.status}: ${result.stderr}`);
  }
  return result;
}

// ---- setup (clean env: watcher-side and registry operations) ----
post(["rooms", "add", "watcher", "~/watcher-room"]);
post(["rooms", "add", "sender", "~/sender-room"]);
post(["chat", "m2canary", "--join"], { cwd: WATCHER_DIR });

// ---- the three deliberate sender commands (declared identity, cwd OUTSIDE
// the sender room tree — only the pin makes this identity possible) ----
const senderEnv = cleanEnv({ POST_FROM: "sender", POST_SENDER_ADDRESS: ADDRESS });
post(["chat", "m2canary", "--join"], { env: senderEnv });
const mailSend = post(
  ["send", "--to", "watcher", "--body", "m2 canary mail", "--json"],
  { env: senderEnv }
);
const mailId = JSON.parse(mailSend.stdout).envelope.id;
const chatSend = post(
  ["chat", "m2canary", "--send", "--body", "m2 canary message @watcher", "--json"],
  { env: senderEnv }
);
const chatId = JSON.parse(chatSend.stdout).message.id;

// ---- receipt 0: exact event classes in the parsed snapshot ----
const snapshot = post(["watch", "--snapshot"], { cwd: WATCHER_DIR });
const events = snapshot.stdout
  .split("\n")
  .filter((line) => line.trim())
  .map((line) => JSON.parse(line));

const mailEvents = events.filter((e) => e.event === "mail");
receipt(
  mailEvents.length === 1 &&
    mailEvents[0].id === mailId &&
    mailEvents[0].from === "sender" &&
    mailEvents[0].sender_address === ADDRESS &&
    mailEvents[0].sender_provenance === "declared-env",
  "direct-mail event: from sender, both identity fields, exact id",
  JSON.stringify(mailEvents)
);
const mention = events.find((e) => e.event === "channel_message" && e.id === chatId);
receipt(
  mention !== undefined &&
    mention.from === "sender" &&
    mention.reason === "mention" &&
    mention.sender_address === ADDRESS &&
    mention.sender_provenance === "declared-env",
  "ordinary channel message: mention reason, both identity fields, exact id",
  JSON.stringify(mention)
);
receipt(
  events.every((e) => e.from !== "ambient-ghost") &&
    events.every((e) => e.sender_address !== AMBIENT_ADDRESS),
  "poisoned ambient identity reached no envelope"
);

// ---- adapters: each must produce its NORMAL notice types ----
function adapterContext(stdout, label) {
  let parsed;
  try {
    parsed = JSON.parse(stdout);
  } catch {
    receipt(false, `${label}: output is JSON`, stdout.slice(0, 200));
    return "";
  }
  // All four adapters nest the notice under hookSpecificOutput; Cursor also
  // emits a native field, but the nested one is common to all.
  const context = parsed?.hookSpecificOutput?.additionalContext;
  if (typeof context !== "string" || context === "") {
    receipt(false, `${label}: emitted a notice (not the empty/diagnostic path)`, stdout.slice(0, 200));
    return "";
  }
  return context;
}

function assertNotice(context, label) {
  if (context === "") return; // already failed above
  receipt(
    context.includes(`Direct mail id(s): ${mailId}`) || context.includes("Direct mail: 1 item(s)"),
    `${label}: direct-mail notice present with the canary mail`
  );
  receipt(
    /channel message\(s\)/i.test(context) && context.includes("#m2canary (2)"),
    `${label}: channel notice counts both m2canary messages`,
    context
  );
  receipt(
    !context.includes("Manual check") && !context.includes("could not"),
    `${label}: no failure diagnostic`
  );
}

const claudeOut = run("node", [path.join(HOOKS, "claude-mail.mjs")], {
  env: cleanEnv({ POST_CLAUDE_HOOK_BIN: BIN, POST_CLAUDE_HOOK_STATE_DIR: path.join(WORK, "st-claude") }),
  input: JSON.stringify({ hook_event_name: "SessionStart", session_id: "m2", cwd: WATCHER_DIR }),
});
assertNotice(adapterContext(claudeOut.stdout, "claude"), "claude");

const codexOut = run("node", [path.join(HOOKS, "codex-mail.mjs")], {
  env: cleanEnv({ POST_CODEX_HOOK_BIN: BIN, POST_CODEX_HOOK_STATE_DIR: path.join(WORK, "st-codex") }),
  input: JSON.stringify({ hook_event_name: "SessionStart", session_id: "m2", cwd: WATCHER_DIR }),
});
assertNotice(adapterContext(codexOut.stdout, "codex"), "codex");

const cursorOut = run("node", [path.join(HOOKS, "cursor-mail.mjs")], {
  env: cleanEnv({ POST_CURSOR_HOOK_BIN: BIN, POST_CURSOR_HOOK_STATE_DIR: path.join(WORK, "st-cursor") }),
  input: JSON.stringify({ hook_event_name: "sessionStart", session_id: "m2", cwd: WATCHER_DIR }),
});
assertNotice(adapterContext(cursorOut.stdout, "cursor"), "cursor");

const grokOut = run("node", [path.join(HOOKS, "grok-mail.mjs")], {
  env: cleanEnv({ POST_GROK_HOOK_BIN: BIN, POST_GROK_HOOK_STATE_DIR: path.join(WORK, "st-grok") }),
  input: JSON.stringify({ hookEventName: "UserPromptSubmit", sessionId: "m2", cwd: WATCHER_DIR }),
});
assertNotice(adapterContext(grokOut.stdout, "grok"), "grok");

// ---- watch-notice (Monitor lane): text lines for both event classes ----
const notice = run("node", [path.join(HOOKS, "watch-notice.mjs"), "--snapshot"], {
  cwd: WATCHER_DIR,
  env: cleanEnv({ POST_WATCH_NOTICE_BIN: BIN }),
});
// watch-notice renders the same summary-notice format as the adapters (not
// raw per-event lines): assert both notice types with exact values.
const noticeText = `${notice.stdout}${notice.stderr}`;
receipt(
  noticeText.includes(`Direct mail id(s): ${mailId}`),
  "watch-notice: direct-mail notice with the exact canary mail id",
  noticeText
);
receipt(
  noticeText.includes("#m2canary (2)"),
  "watch-notice: channel notice counts both m2canary messages",
  noticeText
);

fs.rmSync(WORK, { recursive: true, force: true });
if (failures > 0) {
  console.error(`M2 CANARY FAIL: ${failures} receipt(s) failed`);
  process.exit(1);
}
console.log(
  "M2 CANARY PASS: all five SOURCE consumers accepted real identity-field events under synthetic hook invocation (installed registrations and Grok's live doorbell remain unverified)"
);
