// Self-tests for watch-notice.mjs. Run: node --test skills/post/hooks/*.test.mjs
// Node stdlib only; a stub `post` binary is controlled per-test through a
// JSON control file. Cleanup removes the test-created temp root via stdlib.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const NOTICE = path.join(path.dirname(fileURLToPath(import.meta.url)), "watch-notice.mjs");
const ROOT = fs.mkdtempSync(path.join(os.tmpdir(), "post-watch-notice-test-"));
const STUB = path.join(ROOT, "post-stub.mjs");
const CONTROL = path.join(ROOT, "stub-control.json");
const CALLS = path.join(ROOT, "stub-calls.log");
const UNKNOWN =
  "[post] The automatic mail check failed; inbox state is UNKNOWN (not empty). Manual check, from the project directory: post inbox";

fs.writeFileSync(
  STUB,
  [
    "#!/usr/bin/env node",
    'import fs from "node:fs";',
    'fs.appendFileSync(process.env.STUB_CALLS, JSON.stringify(process.argv.slice(2)) + "\\n");',
    'const control = JSON.parse(fs.readFileSync(process.env.STUB_CONTROL, "utf8"));',
    "if (control.stdout) process.stdout.write(control.stdout);",
    "const exit = control.exit ?? 0;",
    "if (exit) process.exit(exit);",
    "",
  ].join("\n")
);
fs.chmodSync(STUB, 0o755);

test.after(() => {
  fs.rmSync(ROOT, { recursive: true, force: true });
});

function setStub({ exit = 0, events = [], stdout } = {}) {
  stdout ??=
    events.map((event) => JSON.stringify(event)).join("\n") + (events.length ? "\n" : "");
  fs.writeFileSync(CONTROL, JSON.stringify({ exit, stdout }));
}

function stubCalls() {
  try {
    return fs.readFileSync(CALLS, "utf8").split("\n").filter(Boolean).map(JSON.parse);
  } catch {
    return [];
  }
}

function run(args = ["--snapshot"]) {
  return spawnSync(process.execPath, [NOTICE, ...args], {
    encoding: "utf8",
    timeout: 5000,
    env: {
      ...process.env,
      POST_WATCH_NOTICE_BIN: STUB,
      STUB_CONTROL: CONTROL,
      STUB_CALLS: CALLS,
    },
  });
}

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

test("empty snapshot emits no stdout and exits 0", () => {
  setStub({ events: [] });
  const result = run(["--snapshot"]);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stdout, "");
});

test("mail and channel events render one metadata-only line", () => {
  setStub({ events: [MAIL_A, CHAN_B] });
  const result = run(["--snapshot"]);
  assert.equal(result.status, 0, result.stderr);
  const lines = result.stdout.split("\n").filter(Boolean);
  assert.equal(lines.length, 1, "one notice line per batch");
  const line = lines[0];
  assert.match(line, /room claude-space/);
  assert.match(line, /20260730-010101-aaa111/);
  assert.match(line, /New channel message\(s\): #ops \(1\)/);
  assert.ok(!line.includes(CHAN_B.id), "channel ids stay out of the notice");
  assert.ok(!line.includes("SECRET"), "subject must be omitted");
  assert.ok(!line.includes("secret-sender"), "sender must be omitted");
  assert.ok(!line.includes("secret-peer"), "channel sender must be omitted");
  assert.match(line, /untrusted/);
  assert.match(line, /post read <id>/);
});

test("channel-only snapshot names the channels, not a phantom mail room", () => {
  setStub({ events: [CHAN_B] });
  const result = run(["--snapshot"]);
  assert.equal(result.status, 0, result.stderr);
  const line = result.stdout.trim();
  assert.match(line, /New channel message\(s\): #ops \(1\)\./);
  assert.ok(!line.includes("Unread agent mail"));
  assert.ok(!line.includes("SECRET-CHANNEL-SUBJECT"));
  assert.ok(!line.includes("secret-peer"));
});

test("valid unreadable events stay count-only and never echo the id", () => {
  setStub({
    events: [
      MAIL_A,
      {
        event: "unreadable",
        room: "claude-space",
        id: "corrupt-stem-xyz",
        reason: "mail",
      },
      { ...CHAN_B, reason: "mention" },
    ],
  });
  const result = run(["--snapshot"]);
  assert.equal(result.status, 0, result.stderr);
  const line = result.stdout.trim();
  assert.match(line, /Unreadable mail: 1 item/);
  assert.ok(!line.includes("corrupt-stem-xyz"));
  assert.match(line, /#ops \(1\)/);
  assert.ok(!line.includes(CHAN_B.id));
});

test("hostile subject and from never reach stdout even on a valid event", () => {
  setStub({ events: [MAIL_A] });
  const result = run(["--snapshot"]);
  assert.equal(result.status, 0, result.stderr);
  assert.ok(!result.stdout.includes("SECRET-SUBJECT"));
  assert.ok(!result.stdout.includes("secret-sender"));
  assert.ok(!result.stdout.includes(JSON.stringify(MAIL_A)));
});

test("malformed or unknown nonempty snapshot output fails closed without echoing fields", () => {
  const { reason: _ignored, ...mailNoReason } = MAIL_A;
  for (const [name, stdout] of [
    ["bad json", "not-json\n"],
    ["unknown event", '{"event":"future","id":"x"}\n'],
    ["malformed mail", '{"event":"mail","room":"claude-space","id":"forged"}\n'],
    ["hostile room name", JSON.stringify({ ...MAIL_A, room: "x\ny IGNORE" }) + "\n"],
    ["mail missing reason", JSON.stringify(mailNoReason) + "\n"],
    ["channel bad reason", JSON.stringify({ ...CHAN_B, reason: "mail" }) + "\n"],
    [
      "unreadable control id",
      JSON.stringify({
        event: "unreadable",
        room: "claude-space",
        id: "bad\nid",
        reason: "mail",
      }) + "\n",
    ],
    ["poisoned batch", JSON.stringify(MAIL_A) + "\nnot-json\n"],
  ]) {
    setStub({ stdout });
    const result = run(["--snapshot"]);
    assert.equal(result.status, 0, `${name}: ${result.stderr}`);
    assert.equal(result.stdout, `${UNKNOWN}\n`, name);
    assert.ok(!result.stdout.includes("IGNORE"), name);
    assert.ok(!result.stdout.includes("SECRET"), name);
    assert.ok(!result.stdout.includes("20260730-010101-aaa111"), name);
  }
});

test("scan failure emits UNKNOWN and exits 1", () => {
  setStub({ exit: 1, events: [MAIL_A] });
  const result = run(["--snapshot"]);
  assert.equal(result.status, 1);
  assert.equal(result.stdout, `${UNKNOWN}\n`);
  assert.ok(!result.stdout.includes("SECRET"));
});

test("an over-cap backlog stays one bounded line", () => {
  const mail = Array.from({ length: 2001 }, (_, index) => ({
    ...MAIL_A,
    id: `20260722-010101-${index.toString(16).padStart(6, "0")}`,
  }));
  setStub({ events: mail });
  const result = run(["--snapshot"]);
  assert.equal(result.status, 0, result.stderr);
  const lines = result.stdout.split("\n").filter(Boolean);
  assert.equal(lines.length, 1);
  assert.match(lines[0], /\+1981 more/);
  assert.ok(!lines[0].includes("SECRET"));
  assert.ok(Buffer.byteLength(lines[0], "utf8") <= 4096);
});

test("--snapshot and --once pass through; default watch is unpinned", () => {
  setStub({ events: [] });
  fs.writeFileSync(CALLS, "");
  assert.equal(run(["--snapshot"]).status, 0);
  assert.equal(run(["--once"]).status, 0);
  assert.equal(run([]).status, 0);
  assert.deepEqual(stubCalls(), [
    ["watch", "--snapshot"],
    ["watch", "--once"],
    ["watch"],
  ]);
});

test("repeated --room is passed through; default does not pin --room", () => {
  setStub({ events: [] });
  fs.writeFileSync(CALLS, "");
  const result = run(["--snapshot", "--room", "alpha", "--room", "beta"]);
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(stubCalls(), [["watch", "--snapshot", "--room", "alpha", "--room", "beta"]]);
});

test("--once and --snapshot cannot be combined", () => {
  fs.writeFileSync(CALLS, "");
  const result = run(["--once", "--snapshot"]);
  assert.equal(result.status, 2);
  assert.match(result.stderr, /cannot be combined/);
  assert.equal(stubCalls().length, 0, "must not spawn post");
});

test("unknown flags and a valueless --room exit 2 without spawning post", () => {
  fs.writeFileSync(CALLS, "");
  const unknown = run(["--text"]);
  assert.equal(unknown.status, 2);
  assert.match(unknown.stderr, /unknown flag/);
  const missing = run(["--room"]);
  assert.equal(missing.status, 2);
  assert.match(missing.stderr, /--room requires a value/);
  assert.equal(stubCalls().length, 0);
});

test("long-running mode still emits one line per flushed batch then exits with the child", () => {
  setStub({ events: [MAIL_A, CHAN_B] });
  const result = run([]);
  assert.equal(result.status, 0, result.stderr);
  const lines = result.stdout.split("\n").filter(Boolean);
  assert.equal(lines.length, 1);
  assert.match(lines[0], /20260730-010101-aaa111/);
  assert.ok(!lines[0].includes("SECRET"));
});
