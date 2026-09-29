// Self-tests for grok-mail.mjs. Run: node --test skills/post/hooks/*.test.mjs
// Node stdlib only; a stub `post` binary is controlled per-test through a
// JSON control file. Cleanup removes the test-created temp root via stdlib.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const ADAPTER = path.join(path.dirname(fileURLToPath(import.meta.url)), "grok-mail.mjs");
const ROOT = fs.mkdtempSync(path.join(os.tmpdir(), "post-grok-hook-test-"));
const CWD = path.join(ROOT, "some-project");
fs.mkdirSync(CWD, { recursive: true });

const STUB = path.join(ROOT, "post-stub.mjs");
const CONTROL = path.join(ROOT, "stub-control.json");
const CALLS = path.join(ROOT, "stub-calls.log");
fs.writeFileSync(
  STUB,
  [
    "#!/usr/bin/env node",
    'import fs from "node:fs";',
    'fs.appendFileSync(process.env.STUB_CALLS, JSON.stringify({ cwd: process.cwd(), args: process.argv.slice(2), participant: process.env.POST_PARTICIPANT || null }) + "\\n");',
    'const control = JSON.parse(fs.readFileSync(process.env.STUB_CONTROL, "utf8"));',
    'const args = process.argv.slice(2);',
    'let output = control.stdout ?? "";',
    'if (args[0] === "version") output = JSON.stringify(control.version ?? { ok: true, capabilities: ["participants"] }) + "\\n";',
    'else if (args[0] === "participant" && args[1] === "show") output = JSON.stringify(control.show ?? { ok: true, status: "unbound" }) + "\\n";',
    'else if (args[0] === "participant" && args[1] === "bind") output = control.bind_stdout ?? JSON.stringify({ ok: true, status: "bound", id: process.env.POST_PARTICIPANT || "test-participant", participant: { id: process.env.POST_PARTICIPANT || "test-participant", lineage: null } }) + "\\n";',
    'if (output) process.stdout.write(output);',
    // Natural exit when the control exit is 0: process.exit() would drop
    // stdout bytes still buffered for a pipe (over-cap snapshots exceed the
    // 64 KiB pipe buffer), truncating the snapshot mid-line.
    "const exit = args[0] === \"participant\" && args[1] === \"bind\" ? (control.bind_exit ?? 0) : args[0] === \"participant\" && args[1] === \"touch\" ? (control.touch_exit ?? 0) : args[0] === \"participant\" && args[1] === \"end\" ? (control.end_exit ?? 0) : args[0] === \"watch\" ? (control.exit ?? 0) : 0;",
    "if (exit) process.exit(exit);",
    "",
  ].join("\n")
);
fs.chmodSync(STUB, 0o755);

test.after(() => {
  // ROOT is a uniquely named temp dir this test created; plain stdlib
  // removal is the portable cleanup, no external binary involved.
  fs.rmSync(ROOT, { recursive: true, force: true });
});

let stateDirCounter = 0;
function freshStateDir() {
  const dir = path.join(ROOT, `state-${stateDirCounter++}`);
  fs.mkdirSync(dir, { recursive: true });
  return dir;
}

function setStub({ exit = 0, events = [], stdout, version, show, bind_stdout, bind_exit, touch_exit, end_exit } = {}) {
  stdout ??=
    events.map((event) => JSON.stringify(event)).join("\n") + (events.length ? "\n" : "");
  fs.writeFileSync(CONTROL, JSON.stringify({ exit, stdout, version, show, bind_stdout, bind_exit, touch_exit, end_exit }));
}

function allStubCalls() {
  try {
    return fs.readFileSync(CALLS, "utf8").split("\n").filter(Boolean).map((line) => {
      const call = JSON.parse(line);
      if (call.args[1] === "notice" && call.args[2] === "--claim") {
        assert.match(call.args[3], /^[1-9][0-9]*$/);
        call.args[3] = "<adapter-pid>";
      }
      return call;
    });
  } catch {
    return [];
  }
}
function stubCalls() {
  return allStubCalls().filter((call) => call.args[0] === "watch").map((call) => call.cwd);
}

function run(input, { stateDir, env: extraEnv = {} } = {}) {
  const result = spawnSync(process.execPath, [ADAPTER], {
    input: typeof input === "string" ? input : JSON.stringify(input),
    encoding: "utf8",
    env: {
      ...process.env,
      POST_GROK_HOOK_BIN: STUB,
      POST_GROK_HOOK_STATE_DIR: stateDir,
      STUB_CONTROL: CONTROL,
      STUB_CALLS: CALLS,
      ...extraEnv,
    },
  });
  assert.equal(result.status, 0, `adapter must always exit 0: ${result.stderr}`);
  return JSON.parse(result.stdout);
}

const BASE = { cwd: CWD };
const MAIL_A = {
  event: "mail",
  room: "claude-space",
  id: "20260730-010101-aaa111",
  from: "secret-sender",
  kind: "note",
  subject: "SECRET-SUBJECT",
  sent: "2026-07-30 01:01:01 -0500",
  reason: "mail",
};

const CHAN_B = {
  event: "channel_message",
  channel: "ops",
  id: "20260730-020202-000002-bbb222",
  from: "secret-peer",
  subject: "SECRET-CHANNEL-SUBJECT",
  sent: "2026-07-30 02:02:02 -0500",
  reason: "channel",
};

test("unsupported hook events including SessionStart and PostToolUse emit {} without spawning post", () => {
  setStub({ events: [MAIL_A] });
  const before = stubCalls().length;
  for (const eventName of ["Stop", "SessionStart", "PostToolUse", "session_start", "post_tool_use"]) {
    const out = run(
      { ...BASE, hookEventName: eventName, sessionId: "s1" },
      { stateDir: freshStateDir() }
    );
    assert.deepEqual(out, {}, eventName);
  }
  assert.equal(stubCalls().length, before, "ignored events must not spawn post");
});

test("the first prompt surfaces the launch backlog with metadata only", () => {
  setStub({ events: [MAIL_A, CHAN_B] });
  const out = run(
    { ...BASE, hookEventName: "UserPromptSubmit", session_id: "s-backlog" },
    { stateDir: freshStateDir() }
  );
  assert.equal(out.hookSpecificOutput.hookEventName, "UserPromptSubmit");
  const context = out.hookSpecificOutput.additionalContext;
  assert.match(context, /room claude-space/);
  assert.match(context, /20260730-010101-aaa111/);
  assert.match(context, /#ops: 1 new/);
  assert.ok(!context.includes("20260730-020202-000002-bbb222"));
  assert.doesNotMatch(context, /untrusted|carries no authority/);
  assert.doesNotMatch(context, /post read <id>/);
  assert.ok(!context.includes("SECRET"), "subject must be omitted");
  assert.ok(!context.includes("secret-sender"), "sender must be omitted");
  assert.ok(!context.includes("secret-peer"), "channel sender must be omitted");
});

test("subagent events are suppressed without spawning post", () => {
  setStub({ events: [MAIL_A] });
  const before = stubCalls().length;
  for (const extra of [{ agent_id: "child-1" }, { agentId: "child-1" }, { subagentId: "child-1" }]) {
    const out = run(
      { ...BASE, hookEventName: "UserPromptSubmit", sessionId: "s-sub", ...extra },
      { stateDir: freshStateDir() }
    );
    assert.deepEqual(out, {}, JSON.stringify(extra));
  }
  assert.equal(stubCalls().length, before, "post must not run for subagent events");
});

test("agent_type alone does not suppress (a main-thread agent session gets mail)", () => {
  setStub({ events: [MAIL_A] });
  const out = run(
    { ...BASE, hookEventName: "UserPromptSubmit", sessionId: "s-agent-type", agent_type: "reviewer" },
    { stateDir: freshStateDir() }
  );
  assert.match(out.hookSpecificOutput.additionalContext, /20260730-010101-aaa111/);
});

test("user_prompt_submit and hook_event_name both scan", () => {
  setStub({ events: [MAIL_A] });
  const snake = run(
    { ...BASE, hookEventName: "user_prompt_submit", sessionId: "s-snake" },
    { stateDir: freshStateDir() }
  );
  assert.match(snake.hookSpecificOutput.additionalContext, /20260730-010101-aaa111/);

  const alias = run(
    { cwd: CWD, hook_event_name: "UserPromptSubmit", session_id: "s-alias" },
    { stateDir: freshStateDir() }
  );
  assert.match(alias.hookSpecificOutput.additionalContext, /20260730-010101-aaa111/);
});

test("GROK_HOOK_EVENT is consulted when stdin omits the event name", () => {
  setStub({ events: [MAIL_A] });
  const result = spawnSync(process.execPath, [ADAPTER], {
    input: JSON.stringify({ sessionId: "s-env", cwd: CWD }),
    encoding: "utf8",
    env: {
      ...process.env,
      POST_GROK_HOOK_BIN: STUB,
      POST_GROK_HOOK_STATE_DIR: freshStateDir(),
      GROK_HOOK_EVENT: "user_prompt_submit",
      STUB_CONTROL: CONTROL,
      STUB_CALLS: CALLS,
    },
  });
  assert.equal(result.status, 0, result.stderr);
  const out = JSON.parse(result.stdout);
  assert.match(out.hookSpecificOutput.additionalContext, /20260730-010101-aaa111/);
});

test("workspaceRoot supplies cwd when cwd is missing", () => {
  setStub({ events: [MAIL_A] });
  fs.writeFileSync(CALLS, "");
  const out = run(
    { hookEventName: "UserPromptSubmit", sessionId: "s-ws", workspaceRoot: CWD },
    { stateDir: freshStateDir() }
  );
  assert.match(out.hookSpecificOutput.additionalContext, /20260730-010101-aaa111/);
  assert.deepEqual(
    stubCalls().map((p) => fs.realpathSync(p)),
    [fs.realpathSync(CWD)]
  );
});

function overCapMail(count = 2001) {
  return Array.from({ length: count }, (_, index) => ({
    ...MAIL_A,
    id: `20260722-010101-${index.toString(16).padStart(6, "0")}`,
    from: "secret-sender",
    subject: "SECRET-SUBJECT",
  }));
}

test("capability mismatch stays retryable until the binary is upgraded", () => {
  const stateDir = freshStateDir();
  setStub({ version: { ok: true, version: "0.9.0", capabilities: [] }, events: [] });
  const first = run({ ...BASE, hookEventName: "UserPromptSubmit", sessionId: "cap-once" }, { stateDir });
  assert.match(first.hookSpecificOutput.additionalContext, /lacks the participants capability/);
  // The mismatch is recorded so it prints once per session; no participant is held.
  const recorded = JSON.parse(fs.readFileSync(path.join(stateDir, "session-cap-once.json"), "utf8"));
  assert.equal(recorded.participantId, null);
  assert.equal(recorded.setupWarned, true);
  assert.deepEqual(allStubCalls().at(-1).args, ["version", "--json"]);
  assert.deepEqual(run({ ...BASE, hookEventName: "UserPromptSubmit", sessionId: "cap-once" }, { stateDir }), {}, "the same mismatch is not reported again");
  setStub({ events: [MAIL_A] });
  const second = run({ ...BASE, hookEventName: "UserPromptSubmit", sessionId: "cap-once" }, { stateDir });
  assert.match(second.hookSpecificOutput.additionalContext, /20260730-010101-aaa111/);
  assert.equal(JSON.parse(fs.readFileSync(path.join(stateDir, "session-cap-once.json"), "utf8")).setupWarned, false);
  assert.deepEqual(allStubCalls().slice(-6).map((call) => call.args), [
    ["version", "--json"],
    ["participant", "bind", "--harness", "grok", "--key", "cap-once", "--json"],
    ["participant", "notice", "--claim", "<adapter-pid>", "--json"],
    ["participant", "touch"],
    ["watch", "--snapshot"],
    ["participant", "show", "--json"],
  ]);
});

test("legacy initialized state without a participant retries setup", () => {
  const stateDir = freshStateDir();
  const stateFile = path.join(stateDir, "session-legacy-state.json");
  fs.writeFileSync(stateFile, JSON.stringify({ initialized: true, participantId: null, seen: [] }));
  setStub({ events: [], bind_stdout: JSON.stringify({ ok: true, status: "bound", id: "grok-legacy123", participant: { id: "grok-legacy123", lineage: null } }) });
  run({ ...BASE, hookEventName: "UserPromptSubmit", sessionId: "legacy-state" }, { stateDir });
  assert.deepEqual(allStubCalls().slice(-6).map((call) => call.args), [
    ["version", "--json"],
    ["participant", "bind", "--harness", "grok", "--key", "legacy-state", "--json"],
    ["participant", "notice", "--claim", "<adapter-pid>", "--json"],
    ["participant", "touch"],
    ["watch", "--snapshot"],
    ["participant", "show", "--json"],
  ]);
});

test("payload session key mints and reuses one participant across prompts", () => {
  const stateDir = freshStateDir();
  fs.writeFileSync(CALLS, "");
  setStub({ events: [] });
  run({ ...BASE, hookEventName: "UserPromptSubmit", sessionId: "payload-key" }, { stateDir, env: { CODEX_THREAD_ID: "outer-key" } });
  const first = allStubCalls();
  const participant = first.find((call) => call.args[0] === "participant" && call.args[1] === "bind");
  assert.deepEqual(participant.args.slice(2), ["--harness", "grok", "--key", "payload-key", "--json"]);
  const id = first.find((call) => call.args[0] === "watch").participant;
  setStub({ events: [] });
  run({ ...BASE, hookEventName: "UserPromptSubmit", sessionId: "payload-key" }, { stateDir, env: { CODEX_THREAD_ID: "different-native-key" } });
  assert.equal(allStubCalls().at(-1).participant, id);
});
