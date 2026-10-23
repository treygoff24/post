import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { withoutSessionIdentity } from "./test-session-env.mjs";

const hooks = process.env.POST_HOOK_TEST_DIR || path.dirname(fileURLToPath(import.meta.url));
const root = fs.mkdtempSync(path.join(os.tmpdir(), "post-pi-test-"));
test.after(() => fs.rmSync(root, { recursive: true, force: true }));
const stub = path.join(root, "post.mjs");
fs.writeFileSync(stub, `#!/usr/bin/env node
import fs from 'node:fs';
const args = process.argv.slice(2);
if (process.env.POST_PI_TEST_CALLS) fs.appendFileSync(process.env.POST_PI_TEST_CALLS, args[0] + '\\n');
if (args[0] === 'version' && process.env.POST_PI_TEST_EXPIRE) fs.writeFileSync(process.env.POST_PI_TEST_EXPIRE, 'expired');
let result = { ok: true };
if (args[0] === 'version') result.capabilities = ['participants'];
if (args[1] === 'bind') { result.id = 'pi-12345678'; result.status = 'bound'; }
if (args[1] === 'notice') result.notice = null;
if (args[0] === 'watch') result = {event:'mail',room:'demo',id:process.env.POST_PI_TEST_ID || '20261021-010101-aaaaaa',from:'SECRET-SENDER',subject:'SECRET-SUBJECT',kind:'note',reason:'mail',sent:'now'};
process.stdout.write(JSON.stringify(result) + '\\n');
`);
fs.chmodSync(stub, 0o755);
const driver = path.join(root, "driver.mjs");
fs.writeFileSync(driver, `
import fs from 'node:fs';
import postMail from ${JSON.stringify(path.join(hooks, "pi-mail.mjs"))};
const handlers = {};
const branch = [];
const sent = [];
const timers = [];
let cleared = 0, idle = true, pending = false, accept = true;
globalThis.setInterval = (fn) => { const timer = {fn,unref(){}}; timers.push(timer); return timer; };
globalThis.clearInterval = () => { cleared++; };
const pi = {
 on(name, fn) { handlers[name] = fn; },
 sendMessage(message, options) {
   sent.push({message,options});
   if (accept) branch.push({type:'custom_message',...message});
 },
};
const ctx = {
 cwd: process.env.HOME,
 isIdle: () => idle,
 hasPendingMessages: () => pending,
 sessionManager: { getSessionId: () => 'test-session', getBranch: () => branch },
};
postMail(pi);
const invoke = (name) => handlers[name]({}, ctx);
const scenario = process.argv[2];
if (scenario === 'deadline') {
 // Model a slow version probe without making the test depend on machine load.
 // The production default must stop before binding once 4.5 seconds are spent.
 delete process.env.POST_PI_HOOK_DEADLINE_MS;
 const started = Date.now();
 Date.now = () => started + (fs.existsSync(process.env.POST_PI_TEST_EXPIRE) ? 4501 : 0);
 invoke('session_start');
 process.stdout.write(JSON.stringify({sent,calls:fs.readFileSync(process.env.POST_PI_TEST_CALLS, 'utf8').trim().split('\\n')}));
 process.exit(0);
}
if (scenario === 'reject') accept = false;
invoke('session_start');
const startup = sent.length;
if (scenario === 'reject') { accept = true; invoke('input'); }
const retried = sent.length;
invoke('input');
const deduped = sent.length;
if (scenario === 'poll') {
 // A new session-start resets snapshot dedupe; run its callback without a
 // startup acceptance so the poll has eligible mail to deliver.
 process.env.POST_PI_TEST_ID = '20261021-010102-bbbbbb';
 accept = false; invoke('session_start'); accept = true;
 idle = false; timers.at(-1).fn(); const busy = sent.length;
 idle = true; pending = true; timers.at(-1).fn(); const queued = sent.length;
 pending = false; timers.at(-1).fn(); const rang = sent.length;
 invoke('session_shutdown'); timers.at(-1).fn();
 process.stdout.write(JSON.stringify({startup,busy,queued,rang,after:sent.length,cleared,sent}));
} else {
 process.stdout.write(JSON.stringify({startup,retried,deduped,timers:timers.length,sent}));
}
`);

function run(scenario, poll = "") {
  const dir = fs.mkdtempSync(path.join(root, "case-"));
  const result = spawnSync(process.execPath, [driver, scenario], {
    encoding: "utf8", timeout: 60000,
    env: { ...withoutSessionIdentity(), HOME: dir, POST_MAIL_ROOT: path.join(dir, "mail"),
      // Lifecycle assertions exercise several fresh Node processes per notice;
      // their fixture budget is independent of the production deadline below.
      POST_PI_HOOK_DEADLINE_MS: "18000",
      POST_PI_TEST_EXPIRE: scenario === "deadline" ? path.join(dir, "expired") : "",
      POST_PI_TEST_CALLS: path.join(dir, "calls"),
      POST_PI_HOOK_BIN: stub, POST_PI_HOOK_STATE_DIR: path.join(dir, "state"), POST_PI_IDLE_POLL_MS: poll },
  });
  assert.equal(result.status, 0, result.stderr);
  return JSON.parse(result.stdout);
}

test("the default deadline stops setup and reports unknown mail without throwing", () => {
  const result = run("deadline");
  assert.deepEqual(result.calls, ["version"]);
  assert.equal(result.sent.length, 1);
  assert.match(result.sent[0].message.content, /participant setup failed; inbox state is UNKNOWN/);
  assert.equal(result.sent[0].options.triggerTurn, false);
});

test("session-start injects only validated metadata and a binding instruction", () => {
  const result = run("normal");
  assert.equal(result.startup, 1);
  const content = result.sent[0].message.content;
  assert.match(content, /20261021-010101-aaaaaa/);
  assert.match(content, /POST_PARTICIPANT=pi-12345678/);
  assert.doesNotMatch(content, /SECRET/);
  assert.equal(result.sent[0].options.triggerTurn, false);
});

test("unaccepted context stays eligible, accepted snapshot is deduped", () => {
  const result = run("reject");
  assert.equal(result.startup, 1);
  assert.equal(result.retried, 2);
  assert.equal(result.deduped, 2);
  assert.match(result.sent[1].message.content, /20261021-010101-aaaaaa/);
});

test("idle polling is opt-in; session startup does not start a model turn", () => {
  const result = run("normal");
  assert.equal(result.startup, 1);
  assert.equal(result.timers, 0);
  assert.ok(result.sent.every((item) => item.options.triggerTurn === false));
});

test("opt-in idle polling guards busy/pending sessions and cleans up on shutdown", () => {
  const result = run("poll", "5000");
  assert.equal(result.busy, 2); // two startup delivery attempts
  assert.equal(result.queued, result.busy);
  assert.equal(result.rang, result.queued + 2); // accepted notice, then fixed wake
  assert.equal(result.after, result.rang);
  assert.ok(result.cleared >= 2);
  assert.equal(result.sent.at(-1).options.triggerTurn, true);
  assert.equal(result.sent.at(-1).message.content, "[post] Inbox notice available in session context.");
});
