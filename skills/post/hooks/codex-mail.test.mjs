// Self-tests for codex-mail.mjs. Run: node --test skills/post/hooks/*.test.mjs
// Node stdlib only; a stub `post` binary is controlled per-test through a
// JSON control file. Cleanup removes the test-created temp root via stdlib.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { execFileSync, spawnSync } from "node:child_process";

const ADAPTER = path.join(path.dirname(fileURLToPath(import.meta.url)), "codex-mail.mjs");
const ROOT = fs.mkdtempSync(path.join(os.tmpdir(), "post-codex-hook-test-"));
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
    'if (control.sleep_ms) Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, control.sleep_ms);',
    'let output = control.stdout ?? "";',
    'if (args[0] === "version") output = JSON.stringify(control.version ?? { ok: true, capabilities: ["participants"] }) + "\\n";',
    'else if (args[0] === "participant" && args[1] === "show") output = JSON.stringify(control.show ?? { ok: true, status: "unbound" }) + "\\n";',
    'else if (args[0] === "participant" && args[1] === "bind") output = control.bind_stdout ?? JSON.stringify({ ok: true, status: "bound", id: process.env.POST_PARTICIPANT || "test-participant", participant: { id: process.env.POST_PARTICIPANT || "test-participant", lineage: null } }) + "\\n";',
    'if (output) process.stdout.write(output);',
    // Natural exit when the control exit is 0: process.exit() would drop
    // stdout bytes still buffered for a pipe (over-cap snapshots exceed the
    // 64 KiB pipe buffer), truncating the snapshot mid-line.
    'const exit = args[0] === "participant" && args[1] === "bind" ? (control.bind_exit ?? 0) : args[0] === "participant" && args[1] === "touch" ? (control.touch_exit ?? 0) : args[0] === "participant" && args[1] === "end" ? (control.end_exit ?? 0) : args[0] === "watch" ? (control.exit ?? 0) : 0;',
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

function setStub({ exit = 0, events = [], stdout, version, show, bind_stdout, bind_exit, touch_exit, end_exit, sleep_ms } = {}) {
  stdout ??=
    events.map((event) => JSON.stringify(event)).join("\n") + (events.length ? "\n" : "");
  fs.writeFileSync(CONTROL, JSON.stringify({ exit, stdout, version, show, bind_stdout, bind_exit, touch_exit, end_exit, sleep_ms }));
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
function stubCallCount() {
  return allStubCalls().filter((call) => call.args[0] === "watch").length;
}

function run(input, { stateDir, throttleMs = 0, defaultCwd = true, env: extraEnv = {} } = {}) {
  const payload =
    defaultCwd && input && typeof input === "object" && !Array.isArray(input)
      ? { cwd: CWD, ...input }
      : input;
  const result = spawnSync(process.execPath, [ADAPTER], {
    input: typeof payload === "string" ? payload : JSON.stringify(payload),
    encoding: "utf8",
    env: {
      ...process.env,
      POST_CODEX_HOOK_BIN: STUB,
      POST_CODEX_HOOK_STATE_DIR: stateDir,
      POST_CODEX_HOOK_THROTTLE_MS: String(throttleMs),
      STUB_CONTROL: CONTROL,
      STUB_CALLS: CALLS,
      DELEGATE_RUN_ID: "", // a delegate child is not minted at start; these tests are not one
      ...extraEnv,
    },
  });
  assert.equal(result.status, 0, `adapter must always exit 0: ${result.stderr}`);
  return JSON.parse(result.stdout);
}

const MAIL_A = {
  event: "mail",
  room: "codex",
  id: "20260722-010101-aaa111",
  from: "secret-sender",
  kind: "note",
  subject: "SECRET-SUBJECT",
  sent: "2026-07-22 01:01:01 -0500",
  reason: "mail",
};

const CHAN_B = {
  event: "channel_message",
  channel: "ops",
  id: "20260722-020202-000002-bbb222",
  from: "secret-peer",
  subject: "SECRET-CHANNEL-SUBJECT",
  sent: "2026-07-22 02:02:02 -0500",
  reason: "channel",
};

test("unsupported hook events emit {}", () => {
  const out = run(
    { hook_event_name: "Stop", session_id: "s1" },
    { stateDir: freshStateDir() }
  );
  assert.deepEqual(out, {});
});

test("SessionStart surfaces the launch backlog with metadata only", () => {
  setStub({ events: [MAIL_A, CHAN_B] });
  const out = run(
    { hook_event_name: "SessionStart", session_id: "s-backlog" },
    { stateDir: freshStateDir() }
  );
  assert.equal(out.hookSpecificOutput.hookEventName, "SessionStart");
  const context = out.hookSpecificOutput.additionalContext;
  assert.match(context, /^\[post\] New mail is waiting for /, "Codex's own wording");
  assert.match(context, /20260722-010101-aaa111/);
  assert.match(context, /#ops: 1 new/);
  assert.ok(!context.includes("20260722-020202-000002-bbb222"));
  assert.doesNotMatch(context, /untrusted|carries no authority/);
  assert.doesNotMatch(context, /run from the project directory/);
  assert.ok(!context.includes("SECRET"), "subject must be omitted");
  assert.ok(!context.includes("secret-sender"), "sender must be omitted");
  assert.ok(!context.includes("secret-peer"), "channel sender must be omitted");
});

test("subagent PostToolUse is suppressed without spawning post", () => {
  setStub({ events: [MAIL_A] });
  const before = stubCallCount();
  for (const input of [
    { hook_event_name: "PostToolUse", session_id: "s-sub", is_subagent: true },
    { hook_event_name: "PostToolUse", session_id: "s-sub", agent_id: "child-1" },
    { hook_event_name: "PostToolUse", session_id: "s-sub", agent_type: "worker" },
  ]) {
    const out = run(input, { stateDir: freshStateDir(), throttleMs: 0 });
    assert.deepEqual(out, {});
  }
  assert.equal(stubCallCount(), before, "post must not run for subagent events");
});

function overCapMail(count = 2001) {
  return Array.from({ length: count }, (_, index) => ({
    ...MAIL_A,
    id: `20260722-010101-${index.toString(16).padStart(6, "0")}`,
    from: "secret-sender",
    subject: "SECRET-SUBJECT",
  }));
}

test("payload session key mints and reuses one participant across lifecycle events", () => {
  const stateDir = freshStateDir();
  fs.writeFileSync(CALLS, "");
  setStub({ events: [] });
  run({ hook_event_name: "SessionStart", session_id: "payload-key" }, { stateDir, env: { CODEX_THREAD_ID: "outer-key", CODEX_SESSION_ID: "outer-key" } });
  const first = allStubCalls();
  const participant = first.find((call) => call.args[0] === "participant" && call.args[1] === "bind");
  assert.deepEqual(participant.args.slice(2), ["--harness", "codex", "--key", "payload-key", "--json"]);
  const id = first.find((call) => call.args[0] === "watch").participant;
  assert.equal(id, "test-participant");
  setStub({ events: [] });
  run({ hook_event_name: "UserPromptSubmit", session_id: "payload-key" }, { stateDir, env: { CODEX_THREAD_ID: "different-native-key", CODEX_SESSION_ID: "different-native-key" } });
  assert.equal(allStubCalls().at(-1).participant, id);
});

test("release binary binds from payload keys and reuses the participant later", () => {
  const proofRoot = fs.mkdtempSync(path.join(os.tmpdir(), "post-codex-real-proof-"));
  try {
    const cwd = path.join(proofRoot, "session");
    const mailRoot = path.join(proofRoot, "mail");
    const stateDir = path.join(proofRoot, "state");
    fs.mkdirSync(cwd, { recursive: true });
    const repoRoot = path.resolve(path.dirname(ADAPTER), "../../..");
    const releaseBin = execFileSync(
      process.execPath,
      [path.join(repoRoot, "scripts", "cargo-release-bin.mjs")],
      { encoding: "utf8" }
    ).trim();
    assert.ok(fs.existsSync(releaseBin), `release binary missing: ${releaseBin}`);
    const callsPath = path.join(proofRoot, "post-calls.jsonl");
    const wrapper = path.join(proofRoot, "post-wrapper.mjs");
    fs.writeFileSync(
      wrapper,
      [
        "#!/usr/bin/env node",
        'import fs from "node:fs";',
        'import { spawnSync } from "node:child_process";',
        `const real = ${JSON.stringify(releaseBin)};`,
        `const calls = ${JSON.stringify(callsPath)};`,
        'const args = process.argv.slice(2);',
        'fs.appendFileSync(calls, JSON.stringify({ args, participant: process.env.POST_PARTICIPANT || null }) + "\\n");',
        'const result = spawnSync(real, args, { stdio: "inherit", env: process.env });',
        'if (result.error) { console.error(result.error); process.exit(1); }',
        'process.exit(result.status ?? 1);',
        "",
      ].join("\n"),
      { mode: 0o755 }
    );
    const cleanEnv = {
      PATH: process.env.PATH,
      HOME: process.env.HOME,
      POST_CODEX_HOOK_BIN: wrapper,
      POST_CODEX_HOOK_STATE_DIR: stateDir,
      POST_MAIL_ROOT: mailRoot,
    };
    // A session inside a registered room is minted at SessionStart; one outside
    // every room is deferred (lazy minting), which would leave nothing to count.
    execFileSync(releaseBin, ["rooms", "add", "session", cwd], { env: { PATH: process.env.PATH, HOME: process.env.HOME, POST_MAIL_ROOT: mailRoot }, encoding: "utf8" });
    const invoke = (payload, extraEnv = {}) => {
      const result = spawnSync(process.execPath, [ADAPTER], {
        input: JSON.stringify(payload),
        encoding: "utf8",
        env: { ...cleanEnv, ...extraEnv },
      });
      assert.equal(result.status, 0, result.stderr);
      return JSON.parse(result.stdout);
    };
    invoke({ hook_event_name: "SessionStart", session_id: "payload-alpha", cwd });
    const participantsDir = path.join(mailRoot, "participants");
    const participantIds = () => fs.readdirSync(participantsDir).filter((entry) => entry.startsWith("codex-"));
    const idsAfterAlpha = participantIds();
    assert.equal(idsAfterAlpha.length, 1);
    const alphaId = idsAfterAlpha[0];
    invoke(
      { hook_event_name: "SessionStart", session_id: "payload-beta", cwd },
      { CODEX_THREAD_ID: "inherited-outer", CODEX_SESSION_ID: "inherited-outer" }
    );
    assert.equal(participantIds().length, 2);
    invoke(
      { hook_event_name: "UserPromptSubmit", session_id: "payload-alpha", cwd },
      { CODEX_THREAD_ID: "another-native-key", CODEX_SESSION_ID: "another-native-key" }
    );
    invoke(
      { hook_event_name: "SessionStart", session_id: "payload-alpha", cwd },
      { POST_PARTICIPANT: alphaId }
    );
    assert.equal(participantIds().length, 2, "explicit participant must not mint a third record");
    const callLog = fs.readFileSync(callsPath, "utf8")
      .trim()
      .split("\n")
      .map((line) => JSON.parse(line));
    const alphaUses = callLog.filter((call) => call.participant === alphaId);
    assert.ok(alphaUses.some((call) => call.args[0] === "watch"));
    assert.ok(alphaUses.some((call) => call.args[0] === "participant" && call.args[1] === "show"));
  } finally {
    fs.rmSync(proofRoot, { recursive: true, force: true });
  }
});

test("normal events share one absolute deadline across touch and snapshot", () => {
  const stateDir = freshStateDir();
  setStub({ events: [] });
  run({ hook_event_name: "SessionStart", session_id: "deadline-session" }, { stateDir });
  setStub({ events: [], sleep_ms: 3000 });
  const started = Date.now();
  const out = run({ hook_event_name: "UserPromptSubmit", session_id: "deadline-session" }, { stateDir });
  const elapsed = Date.now() - started;
  assert.ok(elapsed < 5500, `event exceeded aggregate deadline: ${elapsed}ms`);
  assert.match(out.hookSpecificOutput.additionalContext, /UNKNOWN/);
});

test("an unthrottled PostToolUse shares the same absolute deadline", () => {
  const stateDir = freshStateDir();
  setStub({ events: [] });
  run({ hook_event_name: "SessionStart", session_id: "post-tool-deadline" }, { stateDir });
  setStub({ events: [], sleep_ms: 3000 });
  const started = Date.now();
  const out = run({ hook_event_name: "PostToolUse", session_id: "post-tool-deadline" }, { stateDir, throttleMs: 0 });
  const elapsed = Date.now() - started;
  assert.ok(elapsed < 5500, `PostToolUse exceeded aggregate deadline: ${elapsed}ms`);
  assert.match(out.hookSpecificOutput.additionalContext, /UNKNOWN/);
});
