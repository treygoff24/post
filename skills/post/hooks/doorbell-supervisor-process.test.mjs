// Process-level tests for doorbell-supervisor.mjs: the singleton lock, the
// agent-facing commands, and one end-to-end ring through real child
// processes. herdr and post are control-file-driven stub executables; every
// path lives under a temp root (POST_MAIL_ROOT, POST_DOORBELL_HOME), so no
// live mail store, herdr pane, or service manager is touched. Snapshot events
// come from the release binary's `post contract samples` (POST_BIN).

import test, { describe } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawn, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";

const HOOKS = path.dirname(fileURLToPath(import.meta.url));
const REPO = path.resolve(HOOKS, "..", "..", "..");
const SUPERVISOR = path.join(HOOKS, "doorbell-supervisor.mjs");
const POST_BIN = process.env.POST_BIN || path.join(REPO, "target", "release", "post");
// Short: macOS caps socket paths, and python's lock path shows up in errors.
const ROOT = fs.mkdtempSync(path.join(os.tmpdir(), "dbsp-"));
const sha256 = (value) => createHash("sha256").update(value).digest("hex");

const children = new Set();
test.after(() => {
  for (const child of children) {
    try {
      child.kill("SIGKILL");
    } catch {
      // Already gone.
    }
  }
  fs.rmSync(ROOT, { recursive: true, force: true });
});

function sampleMail() {
  const dir = path.join(ROOT, "samples");
  if (!fs.existsSync(path.join(dir, "watch-snapshot.jsonl"))) {
    const result = spawnSync(POST_BIN, ["contract", "samples", "--dir", dir], { encoding: "utf8" });
    if (result.error || result.status !== 0) throw new Error(`post contract samples failed: ${result.error?.message ?? result.stderr}`);
  }
  return fs
    .readFileSync(path.join(dir, "watch-snapshot.jsonl"), "utf8")
    .split("\n")
    .filter(Boolean)
    .map((line) => JSON.parse(line))
    .find((event) => event.event === "mail" && event.address.kind === "workspace" && !event.pending);
}

const FAKE_POST = `#!/usr/bin/env node
import fs from "node:fs";
const args = process.argv.slice(2);
const control = JSON.parse(fs.readFileSync(process.env.DOORBELL_FAKE_CONTROL, "utf8"));
fs.appendFileSync(control.calls, JSON.stringify({ bin: "post", args, participant: process.env.POST_PARTICIPANT ?? null, cwd: process.cwd() }) + "\\n");
const out = (value) => process.stdout.write(typeof value === "string" ? value : JSON.stringify(value) + "\\n");
const find = (id) => control.participants.find((row) => row.id === id);
if (args[0] === "participant" && args[1] === "list") out({ ok: true, participants: control.participants, count: control.participants.length });
else if (args[0] === "participant" && args[1] === "show") {
  const row = find(process.env.POST_PARTICIPANT ?? control.actor);
  out(row ? { ok: true, status: "bound", id: row.id, participant: row } : { ok: true, status: "unbound" });
} else if (args[0] === "watch") {
  const events = control.snapshots?.[process.env.POST_PARTICIPANT] ?? [];
  out(events.map((event) => JSON.stringify(event) + "\\n").join(""));
} else if (args[0] === "channels") out({ ok: true, channels: [] });
else if (args[0] === "version") out({ ok: true, version: "0.9.0", build_sha: "fake" });
else { process.stderr.write("fake post: unexpected " + args.join(" ") + "\\n"); process.exit(64); }
`;

const FAKE_HERDR = `#!/usr/bin/env node
import fs from "node:fs";
const args = process.argv.slice(2);
const control = JSON.parse(fs.readFileSync(process.env.DOORBELL_FAKE_CONTROL, "utf8"));
fs.appendFileSync(control.calls, JSON.stringify({ bin: "herdr", args }) + "\\n");
const agent = (pane) => ({ pane_id: pane.pane_id, terminal_id: pane.terminal_id, agent: "codex", agent_status: pane.status ?? "idle", focused: pane.focused ?? false, agent_session: { agent: "codex", kind: "id", source: "herdr:codex", value: pane.session } });
if (args[0] === "--version") process.stdout.write("herdr 0.9.1\\n");
else if (args[1] === "list") process.stdout.write(JSON.stringify({ result: { agents: control.panes.map(agent) } }));
else if (args[1] === "get") {
  const pane = control.panes.find((p) => p.pane_id === args[2]);
  if (!pane) { process.stderr.write(JSON.stringify({ error: { code: "agent_not_found" } })); process.exit(1); }
  process.stdout.write(JSON.stringify({ result: { agent: agent(pane) } }));
} else if (args[1] === "prompt") fs.appendFileSync(control.prompts, JSON.stringify({ pane: args[2], text: args[3] }) + "\\n");
else process.exit(64);
`;

let seq = 0;
function makeHost() {
  const dir = path.join(ROOT, `h${seq++}`);
  const bin = path.join(dir, "bin");
  const mail = path.join(dir, "mail");
  fs.mkdirSync(bin, { recursive: true });
  fs.mkdirSync(path.join(mail, "participants"), { recursive: true });
  fs.writeFileSync(path.join(bin, "post"), FAKE_POST, { mode: 0o755 });
  fs.writeFileSync(path.join(bin, "herdr"), FAKE_HERDR, { mode: 0o755 });
  const host = {
    dir,
    mail,
    doorbell: path.join(mail, "doorbell"),
    controlFile: path.join(dir, "control.json"),
    control: { calls: path.join(dir, "calls.jsonl"), prompts: path.join(dir, "prompts.jsonl"), participants: [], panes: [], snapshots: {}, actor: null },
  };
  host.env = {
    PATH: process.env.PATH,
    HOME: dir,
    POST_MAIL_ROOT: mail,
    POST_DOORBELL_HOME: dir,
    POST_DOORBELL_POST_BIN: path.join(bin, "post"),
    POST_DOORBELL_HERDR_BIN: path.join(bin, "herdr"),
    DOORBELL_FAKE_CONTROL: host.controlFile,
  };
  host.save = () => fs.writeFileSync(host.controlFile, JSON.stringify(host.control));
  host.participant = (id, session, extra = {}) => {
    const row = { version: 1, id, harness: "codex", conversation_key_digest: sha256(session), created: "2026-01-01 00:00:00 +0000", lease_hours: 24, workspace: "alpha", ...extra };
    host.control.participants.push(row);
    host.save();
    return row;
  };
  host.pane = (pane_id, session, extra = {}) => {
    host.control.panes.push({ pane_id, terminal_id: `term_${pane_id.replace(/\W/g, "")}`, session, status: "idle", ...extra });
    host.save();
  };
  host.cli = (args, extraEnv = {}) => spawnSync(process.execPath, [SUPERVISOR, ...args], { encoding: "utf8", env: { ...host.env, ...extraEnv }, timeout: 20_000 });
  host.start = () => {
    const child = spawn(process.execPath, [SUPERVISOR, "run"], { env: host.env, stdio: ["ignore", "pipe", "pipe"] });
    children.add(child);
    child.out = "";
    child.err = "";
    child.stdout.on("data", (chunk) => (child.out += chunk));
    child.stderr.on("data", (chunk) => (child.err += chunk));
    child.exited = new Promise((resolve) => child.on("exit", (code, signal) => resolve({ code, signal })));
    return child;
  };
  host.heartbeat = () => {
    try {
      return JSON.parse(fs.readFileSync(path.join(host.doorbell, "heartbeat.json"), "utf8"));
    } catch {
      return undefined;
    }
  };
  host.prompts = () => {
    try {
      return fs.readFileSync(host.control.prompts, "utf8").split("\n").filter(Boolean).map((line) => JSON.parse(line));
    } catch {
      return [];
    }
  };
  host.calls = () => {
    try {
      return fs.readFileSync(host.control.calls, "utf8").split("\n").filter(Boolean).map((line) => JSON.parse(line));
    } catch {
      return [];
    }
  };
  host.save();
  return host;
}

// Bounded: a broken singleton must fail a test, never hang the gate.
function exitWithin(promise, ms = 8000) {
  return Promise.race([promise, new Promise((resolve) => setTimeout(() => resolve({ code: `still running after ${ms}ms` }), ms).unref())]);
}

async function until(predicate, label, timeoutMs = 8000) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const value = predicate();
    if (value) return value;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${label}`);
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
}

// A foreign holder of the lock: the same flock the supervisor uses.
function holdLock(lockFile) {
  fs.mkdirSync(path.dirname(lockFile), { recursive: true });
  const holder = spawn("python3", ["-c", "import fcntl,os,sys\nfd=os.open(sys.argv[1],os.O_RDWR|os.O_CREAT,0o600)\nfcntl.flock(fd,fcntl.LOCK_EX)\nprint('held',flush=True)\nsys.stdin.read()", lockFile], { stdio: ["pipe", "pipe", "inherit"] });
  children.add(holder);
  return new Promise((resolve) => holder.stdout.once("data", () => resolve(holder)));
}

// ------------------------------------------------------------------ singleton

describe("singleton", () => {
  test("simultaneous start: exactly one runs; the other exits with already running (pid N)", async () => {
    const host = makeHost();
    const a = host.start();
    const b = host.start();
    const first = await exitWithin(Promise.race([a.exited.then((r) => ({ who: a, other: b, ...r })), b.exited.then((r) => ({ who: b, other: a, ...r }))]));
    assert.ok(first.who, `neither instance exited: ${first.code}`);
    assert.equal(first.code, 75, first.who.err);
    assert.match(first.who.err, new RegExp(`already running \\(pid ${first.other.pid}\\)`));
    const beat = await until(() => host.heartbeat()?.pid === first.other.pid && host.heartbeat(), "winner heartbeat");
    assert.equal(beat.pid, first.other.pid, "the loser wrote no owner file");
    assert.equal(fs.readFileSync(path.join(host.doorbell, "supervisor.lock"), "utf8").trim(), String(first.other.pid));
    first.other.kill("SIGTERM");
    assert.equal((await exitWithin(first.other.exited)).code, 0);
  });

  test("an abrupt crash then a restart: the kernel released the lock", async () => {
    const host = makeHost();
    const a = host.start();
    await until(() => host.heartbeat()?.pid === a.pid, "first heartbeat");
    a.kill("SIGKILL");
    await exitWithin(a.exited);
    const b = host.start();
    await until(() => host.heartbeat()?.pid === b.pid, "restarted heartbeat");
    b.kill("SIGTERM");
    assert.equal((await exitWithin(b.exited)).code, 0);
  });

  test("a stale owner file naming a live, reused pid does not block a start", async () => {
    const host = makeHost();
    fs.mkdirSync(host.doorbell, { recursive: true });
    fs.writeFileSync(path.join(host.doorbell, "supervisor.lock"), `${process.pid}\n`);
    const a = host.start();
    await until(() => host.heartbeat()?.pid === a.pid, "heartbeat");
    a.kill("SIGTERM");
    await exitWithin(a.exited);
  });

  test("a lock held by someone else refuses the start and writes nothing", async () => {
    const host = makeHost();
    const holder = await holdLock(path.join(host.doorbell, "supervisor.lock"));
    const a = host.start();
    const result = await exitWithin(a.exited);
    assert.equal(result.code, 75);
    assert.match(a.err, /already running/);
    assert.equal(host.heartbeat(), undefined);
    holder.kill("SIGKILL");
  });

  test("the lock helper dying stops all delivery and exits", async () => {
    const host = makeHost();
    host.participant("codex-aaaaaaaa", "session-a");
    host.pane("wC:p1", "session-a");
    const a = host.start();
    const beat = await until(() => host.heartbeat()?.lock_helper_pid && host.heartbeat(), "heartbeat with helper pid");
    process.kill(beat.lock_helper_pid, "SIGKILL");
    const result = await exitWithin(a.exited, 5000);
    assert.equal(result.code, 70);
    assert.match(a.err, /lock helper died; stopping all delivery/);
    // Mail and an armed pane appear after the exit: nothing rings.
    host.control.snapshots["codex-aaaaaaaa"] = [{ ...sampleMail(), id: "20260923-000001-dead01" }];
    host.save();
    host.cli(["enable"], { POST_PARTICIPANT: "codex-aaaaaaaa" });
    await new Promise((resolve) => setTimeout(resolve, 2500));
    assert.deepEqual(host.prompts(), []);
  });
});

// ------------------------------------------------------------------ end to end

describe("end to end", () => {
  test("an armed idle pane with waiting mail gets one v2 notice; health records accepted", async () => {
    const host = makeHost();
    host.participant("codex-aaaaaaaa", "session-a");
    host.participant("codex-bbbbbbbb", "session-b");
    host.pane("wC:p1", "session-a");
    host.pane("wC:p2", "session-b");
    host.control.snapshots["codex-aaaaaaaa"] = [{ ...sampleMail(), id: "20260923-000001-e2e001" }];
    host.control.snapshots["codex-bbbbbbbb"] = [{ ...sampleMail(), id: "20260923-000001-e2e002" }];
    host.save();
    assert.equal(host.cli(["enable"], { POST_PARTICIPANT: "codex-aaaaaaaa" }).status, 0);
    const a = host.start();
    const prompts = await until(() => host.prompts().length >= 1 && host.prompts(), "a prompt");
    assert.equal(prompts[0].pane, "wC:p1");
    assert.match(prompts[0].text, /^\[post-doorbell:v2\] Automated, non-authoritative Post notice for participant codex-aaaaaaaa\. .*Waiting: 1 direct\./);
    const health = await until(() => {
      try {
        const parsed = JSON.parse(fs.readFileSync(path.join(host.doorbell, "health.json"), "utf8"));
        return parsed.bindings?.find((b) => b.participant === "codex-aaaaaaaa")?.last_outcome === "accepted" && parsed;
      } catch {
        return false;
      }
    }, "health accepted");
    assert.equal(health.bindings.find((b) => b.participant === "codex-bbbbbbbb").armed, false, "unarmed participant never rings");
    const snapshotCalls = host.calls().filter((c) => c.bin === "post" && c.args[0] === "watch");
    assert.ok(snapshotCalls.every((c) => c.participant === "codex-aaaaaaaa"));
    assert.ok(snapshotCalls.every((c) => c.cwd === fs.realpathSync(host.doorbell)), "post runs from a neutral cwd");
    const status = host.cli(["status", "--json"]);
    assert.equal(JSON.parse(status.stdout).liveness.state, "running");
    await new Promise((resolve) => setTimeout(resolve, 2500));
    assert.equal(host.prompts().length, 1, "no second ring for the same mail");
    a.kill("SIGTERM");
    await exitWithin(a.exited);
    const after = JSON.parse(host.cli(["status", "--json"]).stdout);
    assert.equal(after.liveness.state, "dead");
    assert.equal(after.health_current, false);
    assert.match(host.cli(["status"]).stdout, /from a supervisor that is dead; it is not current/);
  });
});

// ------------------------------------------------------------------ commands

describe("agent commands", () => {
  const prefs = (host, id) => JSON.parse(fs.readFileSync(path.join(host.doorbell, "prefs", `${id}.json`), "utf8"));

  test("enable, disable, subscribe, and unsubscribe write versioned prefs for the acting participant", () => {
    const host = makeHost();
    host.participant("codex-aaaaaaaa", "session-a");
    host.control.actor = "codex-aaaaaaaa";
    host.save();
    let result = host.cli(["enable", "--focused"]);
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /enabled for codex-aaaaaaaa \(also while focused\); prefs version 1\. Supervisor dead\./);
    assert.deepEqual([prefs(host, "codex-aaaaaaaa").enabled, prefs(host, "codex-aaaaaaaa").focused], [true, true]);
    assert.equal(host.cli(["subscribe", "--channel", "tax", "--channel", "ops"]).status, 0);
    assert.deepEqual(prefs(host, "codex-aaaaaaaa").channels, ["ops", "tax"]);
    assert.equal(host.cli(["subscribe", "--unsubscribe", "--channel", "ops"]).status, 0);
    assert.equal(host.cli(["unsubscribe", "--channel", "tax"]).status, 0);
    assert.deepEqual(prefs(host, "codex-aaaaaaaa").channels, []);
    assert.equal(host.cli(["disable"]).status, 0);
    assert.equal(prefs(host, "codex-aaaaaaaa").enabled, false);
    assert.equal(prefs(host, "codex-aaaaaaaa").version, 5);
  });

  test("an unbound or ended actor is refused", () => {
    const host = makeHost();
    let result = host.cli(["enable"]);
    assert.equal(result.status, 1);
    assert.match(result.stderr, /no bound post participant/);
    host.participant("codex-aaaaaaaa", "session-a", { ended_at: "2026-09-23T00:00:00Z" });
    result = host.cli(["enable"], { POST_PARTICIPANT: "codex-aaaaaaaa" });
    assert.equal(result.status, 1);
    assert.match(result.stderr, /has ended/);
  });

  test("select accepts a pane carrying this conversation and refuses one carrying another", () => {
    const host = makeHost();
    host.participant("codex-aaaaaaaa", "session-a");
    host.pane("wC:p1", "session-a");
    host.pane("wC:p2", "session-other");
    let result = host.cli(["select", "--pane", "wC:p2"], { POST_PARTICIPANT: "codex-aaaaaaaa" });
    assert.equal(result.status, 1);
    assert.match(result.stderr, /carries a different conversation/);
    assert.equal(fs.existsSync(path.join(host.doorbell, "prefs", "codex-aaaaaaaa.json")), false);
    result = host.cli(["select", "--pane", "wC:p1"], { POST_PARTICIPANT: "codex-aaaaaaaa" });
    assert.equal(result.status, 0, result.stderr);
    assert.deepEqual(prefs(host, "codex-aaaaaaaa").selection, { pane: "wC:p1", digest: sha256("session-a") });
  });

  test("status reports stale when the lock is held but the heartbeat is old", async () => {
    const host = makeHost();
    const holder = await holdLock(path.join(host.doorbell, "supervisor.lock"));
    fs.writeFileSync(path.join(host.doorbell, "heartbeat.json"), JSON.stringify({ pid: 1, time: new Date(Date.now() - 60_000).toISOString() }));
    const report = JSON.parse(host.cli(["status", "--json"]).stdout);
    assert.equal(report.liveness.state, "stale");
    assert.equal(report.liveness.lock, "held");
    holder.kill("SIGKILL");
  });

  test("help exits 0; bad usage exits 2", () => {
    const host = makeHost();
    const help = host.cli(["--help"]);
    assert.equal(help.status, 0);
    assert.match(help.stdout, /usage: post-doorbell run/);
    assert.equal(host.cli(["subscribe"]).status, 2);
    assert.equal(host.cli(["enable", "--pane", "x"]).status, 2);
    assert.equal(host.cli(["subscribe", "--channel", "a b"]).status, 2);
  });
});
