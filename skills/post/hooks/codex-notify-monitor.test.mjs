import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const MONITOR = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  "codex-notify-monitor.mjs"
);
const ROOT = fs.mkdtempSync(path.join(os.tmpdir(), "post-codex-notify-test-"));
const POST = path.join(ROOT, "post-stub.mjs");
const CMUX = path.join(ROOT, "cmux-stub.mjs");
const CONTROL = path.join(ROOT, "control.json");
const POST_CALLS = path.join(ROOT, "post-calls.jsonl");
const CMUX_CALLS = path.join(ROOT, "cmux-calls.jsonl");

for (const [file, calls, kind] of [
  [POST, POST_CALLS, "post"],
  [CMUX, CMUX_CALLS, "cmux"],
]) {
  fs.writeFileSync(
    file,
    [
      "#!/usr/bin/env node",
      'import fs from "node:fs";',
      `fs.appendFileSync(${JSON.stringify(calls)}, JSON.stringify(process.argv.slice(2)) + "\\n");`,
      `const control = JSON.parse(fs.readFileSync(${JSON.stringify(CONTROL)}, "utf8"));`,
      `if (control.${kind}Stdout) process.stdout.write(control.${kind}Stdout);`,
      `process.exit(control.${kind}Exit ?? 0);`,
      "",
    ].join("\n")
  );
  fs.chmodSync(file, 0o755);
}

test.after(() => {
  spawnSync("trash", [ROOT], { stdio: "ignore" });
});

function setControl(control = {}) {
  fs.writeFileSync(CONTROL, JSON.stringify(control));
}

function calls(file) {
  try {
    return fs
      .readFileSync(file, "utf8")
      .split("\n")
      .filter(Boolean)
      .map((line) => JSON.parse(line));
  } catch {
    return [];
  }
}

function run({ state = path.join(ROOT, "seen.json"), rooms = ["sol", "codex"] } = {}) {
  return spawnSync(process.execPath, [MONITOR, ...rooms], {
    encoding: "utf8",
    env: {
      ...process.env,
      POST_CODEX_NOTIFY_POST_BIN: POST,
      POST_CODEX_NOTIFY_CMUX_BIN: CMUX,
      POST_CODEX_NOTIFY_STATE: state,
    },
  });
}

const MAIL = {
  event: "mail",
  room: "sol",
  id: "20260804-220000-abc123",
  from: "untrusted-sender",
  kind: "note",
  subject: "UNTRUSTED SUBJECT",
  sent: "2026-08-04 22:00:00 -0400",
};
const CHANNEL = {
  event: "channel_message",
  channel: "commons",
  id: "20260804-220001-000001-def456",
  from: "untrusted-sender",
  subject: "UNTRUSTED CHANNEL SUBJECT",
  sent: "2026-08-04 22:00:01 -0400",
};

test("one snapshot rings once for fresh direct mail and ignores channels", () => {
  const state = path.join(ROOT, "fresh.json");
  setControl({
    postStdout: `${JSON.stringify(MAIL)}\n${JSON.stringify(CHANNEL)}\n`,
  });
  const result = run({ state });
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(calls(POST_CALLS).at(-1), [
    "watch",
    "--room",
    "sol",
    "--room",
    "codex",
    "--snapshot",
  ]);
  assert.deepEqual(calls(CMUX_CALLS).at(-1), [
    "notify",
    "--title",
    "Post for Codex",
    "--body",
    "1 direct message is waiting.",
  ]);
  assert.ok(!calls(CMUX_CALLS).at(-1).join(" ").includes("UNTRUSTED"));
});

test("persisted ids suppress repeat notifications across runs", () => {
  const state = path.join(ROOT, "dedupe.json");
  setControl({ postStdout: `${JSON.stringify(MAIL)}\n` });
  assert.equal(run({ state }).status, 0);
  const before = calls(CMUX_CALLS).length;
  assert.equal(run({ state }).status, 0);
  assert.equal(calls(CMUX_CALLS).length, before);
});

test("corrupt state re-rings instead of silently losing mail", () => {
  const state = path.join(ROOT, "corrupt.json");
  fs.writeFileSync(state, "not-json");
  setControl({ postStdout: `${JSON.stringify(MAIL)}\n` });
  const before = calls(CMUX_CALLS).length;
  assert.equal(run({ state }).status, 0);
  assert.equal(calls(CMUX_CALLS).length, before + 1);
});

test("a failed notification is not recorded as delivered", () => {
  const state = path.join(ROOT, "cmux-fail.json");
  setControl({ postStdout: `${JSON.stringify(MAIL)}\n`, cmuxExit: 1 });
  assert.notEqual(run({ state }).status, 0);
  setControl({ postStdout: `${JSON.stringify(MAIL)}\n` });
  const before = calls(CMUX_CALLS).length;
  assert.equal(run({ state }).status, 0);
  assert.equal(calls(CMUX_CALLS).length, before + 1);
});

test("malformed snapshot output fails without notifying", () => {
  const state = path.join(ROOT, "malformed.json");
  setControl({ postStdout: "not-json\n" });
  const before = calls(CMUX_CALLS).length;
  assert.notEqual(run({ state }).status, 0);
  assert.equal(calls(CMUX_CALLS).length, before);
});

test("room names are validated before running post", () => {
  setControl({});
  const before = calls(POST_CALLS).length;
  assert.notEqual(run({ rooms: ["../sol"] }).status, 0);
  assert.equal(calls(POST_CALLS).length, before);
});
