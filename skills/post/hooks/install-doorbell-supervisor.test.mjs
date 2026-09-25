// Tests for install-doorbell-supervisor.mjs. Every path lives under a temp
// home (POST_DOORBELL_HOME, POST_MAIL_ROOT); launchctl, systemctl, herdr, and
// post are stub executables. The service-manager stubs really start the
// installed supervisor from the written plist or unit (and kill it on stop),
// so "healthy" is the supervisor's own lock, heartbeat, and health.json, not a
// canned answer. No real LaunchAgent, systemd unit, ~/.local/bin, or mail store
// is touched.

import test, { describe } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";

const HOOKS = path.dirname(fileURLToPath(import.meta.url));
const INSTALLER = path.join(HOOKS, "install-doorbell-supervisor.mjs");
const ROOT = fs.mkdtempSync(path.join(os.tmpdir(), "dbi-"));
const sha256 = (value) => createHash("sha256").update(value).digest("hex");

function killAll() {
  let dirs = [];
  try {
    dirs = fs.readdirSync(ROOT);
  } catch {
    return;
  }
  for (const dir of dirs) {
    let state;
    try {
      state = JSON.parse(fs.readFileSync(path.join(ROOT, dir, "sm-state.json"), "utf8"));
    } catch {
      continue;
    }
    for (const pid of Object.values(state.running ?? {})) {
      try {
        process.kill(pid, "SIGKILL");
      } catch {
        // Already gone.
      }
    }
  }
}
test.after(() => {
  killAll();
  fs.rmSync(ROOT, { recursive: true, force: true });
});

const FAKE_POST = `#!/usr/bin/env node
import fs from "node:fs";
const args = process.argv.slice(2);
const control = JSON.parse(fs.readFileSync(process.env.DOORBELL_FAKE_CONTROL, "utf8"));
const out = (value) => process.stdout.write(JSON.stringify(value) + "\\n");
if (args[0] === "version") out({ ok: true, version: "0.9.0", build_sha: "fake" });
else if (args[0] === "participant" && args[1] === "list") {
  if (control.postFails) { process.stderr.write("post: store unreadable\\n"); process.exit(1); }
  out({ ok: true, participants: control.participants, count: control.participants.length });
} else if (args[0] === "watch") {
  if (control.watchFails) { process.stderr.write("post: watch failed\\n"); process.exit(1); }
  if (control.watchHangs) Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 15000);
  if (control.watchFlipsPaneThenFails) {
    // The pane goes busy while this scan runs; discovery sees it before the scan fails.
    const next = { ...control, panes: control.panes.map((pane, i) => (i === 0 ? { ...pane, status: "working" } : pane)) };
    const tmp = process.env.DOORBELL_FAKE_CONTROL + ".flip.tmp";
    fs.writeFileSync(tmp, JSON.stringify(next));
    fs.renameSync(tmp, process.env.DOORBELL_FAKE_CONTROL);
    Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 5000);
    process.stderr.write("post: watch failed\\n");
    process.exit(1);
  }
} else if (args[0] === "channels") out({ ok: true, channels: [] });
else { process.stderr.write("fake post: unexpected " + args.join(" ") + "\\n"); process.exit(64); }
`;

const FAKE_HERDR = `#!/usr/bin/env node
import fs from "node:fs";
const args = process.argv.slice(2);
const control = JSON.parse(fs.readFileSync(process.env.DOORBELL_FAKE_CONTROL, "utf8"));
const agent = (pane) => ({ pane_id: pane.pane_id, name: pane.name, terminal_id: "term_" + pane.pane_id.replace(/\\W/g, ""), agent: "codex", agent_status: pane.status ?? "idle", focused: pane.focused ?? false, agent_session: { agent: "codex", kind: "id", source: "herdr:codex", value: pane.session } });
if (args[0] === "--version") process.stdout.write("herdr 0.9.1\\n");
else if (control.herdrFails) { process.stderr.write("herdr: server not running\\n"); process.exit(1); }
else if (args[1] === "list") process.stdout.write(JSON.stringify({ result: { agents: control.panes.map(agent) } }));
else if (args[1] === "get") {
  const pane = control.panes.find((p) => p.pane_id === args[2] || p.name === args[2]);
  if (!pane) { process.stderr.write(JSON.stringify({ error: { code: "agent_not_found" } })); process.exit(1); }
  process.stdout.write(JSON.stringify({ result: { agent: agent(pane) } }));
} else if (args[1] === "prompt") fs.appendFileSync(control.prompts, JSON.stringify({ pane: args[2], text: args[3] }) + "\\n");
else process.exit(64);
`;

// One stub serves both managers: argv[1] of the symlink name picks the dialect.
// State: sm-state.json { enabled: [...], disabled: [...], loaded: [...], running: {unit: pid} }.
const FAKE_MANAGER = `#!/usr/bin/env node
import fs from "node:fs";
import path from "node:path";
import { spawn } from "node:child_process";
const args = process.argv.slice(2);
const dir = process.env.FAKE_SM_DIR;
const stateFile = path.join(dir, "sm-state.json");
const state = JSON.parse(fs.readFileSync(stateFile, "utf8"));
fs.appendFileSync(path.join(dir, "sm-calls.jsonl"), JSON.stringify({ bin: path.basename(process.argv[1]), args }) + "\\n");
const save = () => fs.writeFileSync(stateFile, JSON.stringify(state));
const add = (list, value) => { if (!state[list].includes(value)) state[list].push(value); };
const drop = (list, value) => { state[list] = state[list].filter((item) => item !== value); };
const stop = (unit) => {
  const pid = state.running[unit];
  if (pid) { try { process.kill(pid, "SIGTERM"); } catch {} delete state.running[unit]; }
  const deadline = Date.now() + 5000;
  while (pid && Date.now() < deadline) { try { process.kill(pid, 0); } catch { break; } Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 50); }
};
const start = (unit, argv, env) => {
  stop(unit);
  const log = fs.openSync(path.join(dir, "service.log"), "a");
  const child = spawn(argv[0], argv.slice(1), { env: { ...env, DOORBELL_FAKE_CONTROL: process.env.DOORBELL_FAKE_CONTROL }, detached: true, stdio: ["ignore", log, log] });
  child.unref();
  state.running[unit] = child.pid;
};
const unq = (text) => text.startsWith('"') ? text.slice(1, -1).replace(/\\\\(.)/g, "$1") : text;
const xml = (text) => text.replace(/&lt;/g, "<").replace(/&gt;/g, ">").replace(/&quot;/g, '"').replace(/&apos;/g, "'").replace(/&amp;/g, "&");
if (path.basename(process.argv[1]) === "systemctl") {
  if (args[0] !== "--user") process.exit(64);
  const [verb, ...rest] = args.slice(1);
  const now = rest.includes("--now");
  const unit = rest.filter((arg) => arg !== "--now")[0];
  const unitFile = unit && path.join(process.env.FAKE_UNIT_DIR, unit);
  if (verb === "daemon-reload") {}
  else if (verb === "is-enabled") { process.stdout.write(state.enabled.includes(unit) ? "enabled\\n" : "disabled\\n"); process.exit(state.enabled.includes(unit) ? 0 : 1); }
  else if (verb === "enable") { add("enabled", unit); drop("disabled", unit); if (now) add("loaded", unit); }
  else if (verb === "disable") {
    if (!fs.existsSync(unitFile) && !state.enabled.includes(unit)) { process.stderr.write("Failed to disable unit: Unit file " + unit + " does not exist.\\n"); save(); process.exit(1); }
    drop("enabled", unit); add("disabled", unit); if (now) { drop("loaded", unit); stop(unit); }
  }
  else if (verb === "restart") {
    const text = fs.readFileSync(unitFile, "utf8");
    const exec = /^ExecStart=(.*)$/m.exec(text)[1].match(/"(?:[^"\\\\]|\\\\.)*"|\\S+/g).map(unq);
    const env = {};
    for (const match of text.matchAll(/^Environment=(.*)$/gm)) { const pair = unq(match[1]); env[pair.slice(0, pair.indexOf("="))] = pair.slice(pair.indexOf("=") + 1); }
    start(unit, exec, env);
    add("loaded", unit);
  } else process.exit(64);
} else {
  const [verb, target, file] = args;
  const label = (target ?? "").split("/").slice(2).join("/");
  // Real launchd finishes tearing a job down after bootout returns: until
  // then print still finds it and bootstrap fails with 5. FAKE_SM_SLOW_BOOTOUT_MS
  // holds that window open.
  state.tearing ??= {};
  for (const [name, until] of Object.entries(state.tearing)) if (Date.now() >= until) { drop("loaded", name); delete state.tearing[name]; }
  if (verb === "print") { save(); process.exit(state.loaded.includes(label) ? 0 : 113); }
  else if (verb === "enable") { add("enabled", label); drop("disabled", label); }
  else if (verb === "disable") { drop("enabled", label); add("disabled", label); }
  else if (verb === "bootout") {
    if (!state.loaded.includes(label) || state.tearing[label]) { process.stderr.write("Boot-out failed: 3: No such process\\n"); save(); process.exit(3); }
    stop(label);
    const slow = Number(process.env.FAKE_SM_SLOW_BOOTOUT_MS ?? 0);
    if (slow > 0) state.tearing[label] = Date.now() + slow;
    else drop("loaded", label);
  } else if (verb === "bootstrap") {
    const loadedLabel = path.basename(file, ".plist");
    if (state.disabled.includes(loadedLabel) || state.loaded.includes(loadedLabel)) { process.stderr.write("Bootstrap failed: 5: Input/output error\\n"); save(); process.exit(5); }
    const text = fs.readFileSync(file, "utf8");
    const plistLabel = /<key>Label<\\/key>\\s*<string>([^<]*)<\\/string>/.exec(text)[1];
    if (plistLabel === "dev.post.doorbell-supervisor") {
      const argv = [.../<key>ProgramArguments<\\/key>\\s*<array>([\\s\\S]*?)<\\/array>/.exec(text)[1].matchAll(/<string>([\\s\\S]*?)<\\/string>/g)].map((m) => xml(m[1]));
      const env = {};
      for (const m of /<key>EnvironmentVariables<\\/key>\\s*<dict>([\\s\\S]*?)<\\/dict>/.exec(text)[1].matchAll(/<key>([\\s\\S]*?)<\\/key><string>([\\s\\S]*?)<\\/string>/g)) env[xml(m[1])] = xml(m[2]);
      start(plistLabel, argv, env);
    }
    add("loaded", plistLabel);
  } else process.exit(64);
}
save();
`;

let seq = 0;
function makeHost(platform = "linux") {
  const dir = path.join(ROOT, `h${seq++}`);
  const bin = path.join(dir, "bin");
  const mail = path.join(dir, "mail");
  fs.mkdirSync(bin, { recursive: true });
  fs.mkdirSync(path.join(mail, "participants"), { recursive: true });
  fs.writeFileSync(path.join(bin, "post"), FAKE_POST, { mode: 0o755 });
  fs.writeFileSync(path.join(bin, "herdr"), FAKE_HERDR, { mode: 0o755 });
  fs.writeFileSync(path.join(bin, "systemctl"), FAKE_MANAGER, { mode: 0o755 });
  fs.writeFileSync(path.join(bin, "launchctl"), FAKE_MANAGER, { mode: 0o755 });
  fs.writeFileSync(path.join(dir, "sm-state.json"), JSON.stringify({ enabled: [], disabled: [], loaded: [], running: {} }));
  const unitDir = platform === "darwin" ? path.join(dir, "Library", "LaunchAgents") : path.join(dir, ".config", "systemd", "user");
  fs.mkdirSync(unitDir, { recursive: true });
  const host = {
    dir,
    mail,
    platform,
    unitDir,
    doorbell: path.join(mail, "doorbell"),
    controlFile: path.join(dir, "control.json"),
    control: { prompts: path.join(dir, "prompts.jsonl"), participants: [], panes: [] },
    env: {
      PATH: process.env.PATH,
      HOME: dir,
      POST_MAIL_ROOT: mail,
      POST_DOORBELL_HOME: dir,
      POST_DOORBELL_PLATFORM: platform,
      POST_DOORBELL_POST_BIN: path.join(bin, "post"),
      POST_DOORBELL_HERDR_BIN: path.join(bin, "herdr"),
      POST_DOORBELL_SYSTEMCTL_BIN: path.join(bin, "systemctl"),
      POST_DOORBELL_LAUNCHCTL_BIN: path.join(bin, "launchctl"),
      DOORBELL_FAKE_CONTROL: path.join(dir, "control.json"),
      FAKE_SM_DIR: dir,
      FAKE_UNIT_DIR: unitDir,
    },
  };
  host.save = () => fs.writeFileSync(host.controlFile, JSON.stringify(host.control));
  host.install = (args = [], extraEnv = {}) =>
    spawnSync(process.execPath, [INSTALLER, ...args], { encoding: "utf8", env: { ...host.env, ...extraEnv }, timeout: 60_000 });
  host.sm = () => JSON.parse(fs.readFileSync(path.join(dir, "sm-state.json"), "utf8"));
  host.smCalls = () => {
    try {
      return fs.readFileSync(path.join(dir, "sm-calls.jsonl"), "utf8").split("\n").filter(Boolean).map((line) => JSON.parse(line));
    } catch {
      return [];
    }
  };
  host.receipt = () => JSON.parse(fs.readFileSync(path.join(host.doorbell, "install-receipt.json"), "utf8"));
  host.prefs = (id) => JSON.parse(fs.readFileSync(path.join(host.doorbell, "prefs", `${id}.json`), "utf8"));
  host.alive = (pid) => {
    try {
      process.kill(pid, 0);
      return true;
    } catch {
      return false;
    }
  };
  // A bound agent: a participant and the herdr pane carrying its conversation.
  host.agent = (name, id, workspace = "alpha") => {
    const session = `session-${name}`;
    host.control.participants.push({ version: 1, id, harness: "codex", conversation_key_digest: sha256(session), created: "2026-01-01 00:00:00 +0000", lease_hours: 24, workspace });
    host.control.panes.push({ pane_id: `w1:p${host.control.panes.length + 1}`, name, session });
    host.save();
  };
  // An old per-agent timer, written the way install-systemd-doorbell.mjs and
  // install-codex-doorbell.mjs write them, and enabled in the fake manager.
  host.legacyTimer = (agent, { room = "alpha", channels = [], participant, extraEnv = [] } = {}) => {
    const sm = host.sm();
    if (platform === "linux") {
      const env = [
        `POST_CODEX_NOTIFY_HERDR_AGENT=${agent}`,
        ...(channels.length ? [`POST_CODEX_NOTIFY_CHANNELS=${channels.join(",")}`] : []),
        ...(participant ? [`POST_PARTICIPANT=${participant}`] : []),
        ...extraEnv,
      ];
      fs.writeFileSync(
        path.join(unitDir, `post-codex-doorbell@${agent}.service`),
        ["[Unit]", `Description=post doorbell for ${agent}`, "", "[Service]", "Type=oneshot", ...env.map((line) => `Environment="${line}"`), `ExecStart=/usr/bin/node /opt/post/codex-notify-monitor.mjs ${room}`, ""].join("\n")
      );
      fs.writeFileSync(path.join(unitDir, `post-codex-doorbell@${agent}.timer`), ["[Timer]", "OnActiveSec=10", "OnUnitActiveSec=10", "", "[Install]", "WantedBy=timers.target", ""].join("\n"));
      sm.enabled.push(`post-codex-doorbell@${agent}.timer`);
    } else {
      const env = { POST_CODEX_NOTIFY_HERDR_AGENT: agent, ...(channels.length ? { POST_CODEX_NOTIFY_CHANNELS: channels.join(",") } : {}) };
      fs.writeFileSync(
        path.join(unitDir, `dev.post.codex-doorbell.${agent}.plist`),
        [
          '<?xml version="1.0" encoding="UTF-8"?>',
          '<plist version="1.0">',
          "<dict>",
          `  <key>Label</key><string>dev.post.codex-doorbell.${agent}</string>`,
          "  <key>ProgramArguments</key>",
          "  <array>",
          "    <string>/opt/homebrew/bin/node</string>",
          "    <string>/opt/post/codex-notify-monitor.mjs</string>",
          `    <string>${room}</string>`,
          "  </array>",
          "  <key>EnvironmentVariables</key>",
          "  <dict>",
          ...Object.entries(env).map(([key, value]) => `    <key>${key}</key>\n    <string>${value}</string>`),
          "  </dict>",
          "  <key>StartInterval</key><integer>10</integer>",
          "</dict>",
          "</plist>",
          "",
        ].join("\n")
      );
      sm.loaded.push(`dev.post.codex-doorbell.${agent}`);
    }
    fs.writeFileSync(path.join(dir, "sm-state.json"), JSON.stringify(sm));
  };
  host.timerUnit = (agent) => (platform === "linux" ? `post-codex-doorbell@${agent}.timer` : `dev.post.codex-doorbell.${agent}`);
  host.timerOn = (agent) => (platform === "linux" ? host.sm().enabled.includes(host.timerUnit(agent)) : host.sm().loaded.includes(host.timerUnit(agent)));
  host.save();
  return host;
}

const supervisorUnit = (host) => (host.platform === "linux" ? "post-doorbell-supervisor.service" : "dev.post.doorbell-supervisor");
const serviceFile = (host) =>
  host.platform === "linux" ? path.join(host.unitDir, "post-doorbell-supervisor.service") : path.join(host.unitDir, "dev.post.doorbell-supervisor.plist");
const ok = (result) => assert.equal(result.status, 0, `exit ${result.status}\nstdout: ${result.stdout}\nstderr: ${result.stderr}`);

describe("usage", () => {
  test("-h and --help print usage and exit 0; an unknown flag exits 2", () => {
    const host = makeHost();
    for (const flag of ["-h", "--help"]) {
      const result = host.install([flag]);
      ok(result);
      assert.match(result.stdout, /^usage: node install-doorbell-supervisor\.mjs/);
    }
    const bad = host.install(["--frobnicate"]);
    assert.equal(bad.status, 2);
    assert.match(bad.stderr, /unknown argument: --frobnicate/);
    assert.equal(fs.existsSync(host.doorbell), false, "usage errors write nothing");
  });
});

for (const platform of ["linux", "darwin"]) {
  describe(`install (${platform})`, () => {
    test("installs, starts, and waits for a healthy supervisor; a rerun is idempotent", () => {
      const host = makeHost(platform);
      host.agent("ada", "ada");
      const result = host.install();
      ok(result);
      assert.match(result.stdout, /supervisor healthy \(pid \d+\); 1 binding\(s\) discovered, 1 armed/);
      const pid = host.sm().running[supervisorUnit(host)];
      assert.ok(pid && host.alive(pid), "the service manager is running the supervisor");
      const health = JSON.parse(fs.readFileSync(path.join(host.doorbell, "health.json"), "utf8"));
      assert.equal(health.pid, pid);
      assert.equal(health.herdr_ok, true);
      assert.equal(health.post_ok, true);
      const receipt = host.receipt();
      assert.equal(receipt.supervisor.state, "healthy");
      assert.equal(receipt.supervisor.service_file, serviceFile(host));
      const text = fs.readFileSync(serviceFile(host), "utf8");
      assert.match(text, platform === "linux" ? /Restart=always/ : /<key>KeepAlive<\/key>\s*<true\/>/);
      assert.ok(text.includes(host.mail), "the mail root is pinned into the service");
      const shim = fs.readFileSync(path.join(host.dir, ".local", "bin", "post-doorbell"), "utf8");
      assert.match(shim, /post-doorbell supervisor shim/);
      assert.equal(fs.statSync(path.join(host.dir, ".local", "share", "post-doorbell", "doorbell-supervisor.mjs")).mode & 0o777, 0o755);

      // The shim reaches the installed supervisor.
      const status = spawnSync(path.join(host.dir, ".local", "bin", "post-doorbell"), ["status", "--json"], { encoding: "utf8", env: host.env });
      assert.equal(status.status, 0, status.stderr);
      assert.equal(JSON.parse(status.stdout).liveness.state, "running");

      const again = host.install();
      ok(again);
      assert.doesNotMatch(again.stdout, /writing|updating/, "nothing rewritten on an unchanged rerun");
    });
  });
}

describe("install: launchd's teardown after bootout (macOS)", () => {
  test("a restart waits for the old job to leave launchd before bootstrapping it again", () => {
    const host = makeHost("darwin");
    host.agent("ada", "ada");
    ok(host.install());
    const first = host.sm().running[supervisorUnit(host)];
    const before = host.smCalls().length;
    const again = host.install([], { FAKE_SM_SLOW_BOOTOUT_MS: "1500" });
    ok(again);
    assert.match(again.stdout, /supervisor healthy \(pid \d+\)/);
    const pid = host.sm().running[supervisorUnit(host)];
    assert.ok(pid && pid !== first && host.alive(pid), "a new supervisor is running");
    const verbs = host.smCalls().slice(before).map((call) => call.args[0]);
    const bootout = verbs.indexOf("bootout");
    const bootstrap = verbs.lastIndexOf("bootstrap");
    assert.ok(bootout >= 0 && bootstrap > bootout, `bootout then bootstrap: ${verbs.join(" ")}`);
    assert.ok(verbs.slice(bootout + 1, bootstrap).includes("print"), "launchd is asked between bootout and bootstrap");
    assert.equal(host.receipt().supervisor.state, "healthy");
  });
});

describe("install: the name collision", () => {
  test("an old post-doorbell script is moved aside with its hash; the unit template stays", () => {
    const host = makeHost("linux");
    const binDir = path.join(host.dir, ".local", "bin");
    fs.mkdirSync(binDir, { recursive: true });
    const legacy = "#!/usr/bin/env python3\nprint('old doorbell daemon')\n";
    fs.writeFileSync(path.join(binDir, "post-doorbell"), legacy, { mode: 0o755 });
    const template = path.join(host.unitDir, "post-doorbell@.service");
    fs.writeFileSync(template, "[Service]\nExecStart=%h/.local/bin/post-doorbell %i\n");
    ok(host.install());
    const hash = sha256(legacy);
    const moved = path.join(binDir, `post-doorbell.legacy-${hash.slice(0, 8)}`);
    assert.equal(fs.readFileSync(moved, "utf8"), legacy);
    assert.deepEqual(
      { path: host.receipt().legacy_script.path, sha256: host.receipt().legacy_script.sha256, state: host.receipt().legacy_script.state },
      { path: path.join(binDir, "post-doorbell"), sha256: hash, state: "moved" }
    );
    assert.ok(fs.existsSync(template), "the Python unit template is kept");
    assert.match(fs.readFileSync(path.join(binDir, "post-doorbell"), "utf8"), /post-doorbell supervisor shim/);
  });
});

describe("install: an interrupted legacy move", () => {
  test("a kill between the rename and its receipt heals to moved, and restore moves it back", () => {
    const host = makeHost("linux");
    const binDir = path.join(host.dir, ".local", "bin");
    fs.mkdirSync(binDir, { recursive: true });
    const legacy = "#!/usr/bin/env python3\nprint('old doorbell daemon')\n";
    const shim = path.join(binDir, "post-doorbell");
    fs.writeFileSync(shim, legacy, { mode: 0o755 });
    ok(host.install());
    // What the kill leaves: the script already aside, the receipt still
    // "moving", and no shim written yet.
    const receipt = host.receipt();
    receipt.legacy_script.state = "moving";
    fs.writeFileSync(path.join(host.doorbell, "install-receipt.json"), JSON.stringify(receipt));
    fs.unlinkSync(shim);
    const again = host.install();
    ok(again);
    assert.match(again.stdout, /an interrupted move finished/);
    assert.equal(host.receipt().legacy_script.state, "moved");
    assert.match(fs.readFileSync(shim, "utf8"), /post-doorbell supervisor shim/);
    host.install(["--restore-legacy"]);
    assert.equal(fs.readFileSync(shim, "utf8"), legacy, "the old script is back");
    assert.equal(host.receipt().legacy_script.state, "restored");
  });
});

describe("install: failed starts", () => {
  test("a lock already held by another supervisor refuses before writing anything", async () => {
    const host = makeHost("linux");
    fs.mkdirSync(host.doorbell, { recursive: true });
    const { spawn } = await import("node:child_process");
    const holder = spawn("python3", ["-c", "import fcntl,os,sys\nfd=os.open(sys.argv[1],os.O_RDWR|os.O_CREAT,0o600)\nfcntl.flock(fd,fcntl.LOCK_EX)\nprint('held',flush=True)\nsys.stdin.read()", path.join(host.doorbell, "supervisor.lock")], { stdio: ["pipe", "pipe", "inherit"] });
    await new Promise((resolve) => holder.stdout.once("data", resolve));
    try {
      const result = spawnSync(process.execPath, [INSTALLER], { encoding: "utf8", env: host.env, timeout: 60_000 });
      assert.equal(result.status, 1);
      assert.match(result.stderr, /another doorbell supervisor holds the lock/);
      assert.equal(fs.existsSync(serviceFile(host)), false);
      assert.equal(fs.existsSync(path.join(host.dir, ".local", "bin", "post-doorbell")), false);
      assert.deepEqual(host.smCalls(), []);
    } finally {
      holder.kill("SIGKILL");
    }
  });

  test("a host with no herdr at all installs a residents-only supervisor", () => {
    const host = makeHost("linux");
    fs.rmSync(path.join(host.dir, "bin", "herdr"));
    delete host.env.POST_DOORBELL_HERDR_BIN;
    // A PATH that cannot find herdr but still has python3 (the lock) and node (the fakes).
    const pathBin = path.join(host.dir, "pathbin");
    fs.mkdirSync(pathBin);
    const python = spawnSync("sh", ["-c", "command -v python3"], { encoding: "utf8" }).stdout.trim();
    fs.symlinkSync(python, path.join(pathBin, "python3"));
    fs.symlinkSync(process.execPath, path.join(pathBin, "node"));
    host.env.PATH = `${pathBin}:/usr/bin:/bin`;
    assert.notEqual(spawnSync("sh", ["-c", "command -v herdr"], { encoding: "utf8", env: host.env }).status, 0, "herdr must be unreachable");
    const result = host.install();
    ok(result);
    const health = JSON.parse(fs.readFileSync(path.join(host.doorbell, "health.json"), "utf8"));
    assert.equal(health.herdr_absent, true);
    assert.equal(health.post_ok, true);
    assert.equal(host.receipt().supervisor.state, "healthy");
  });

  test("a missing herdr or post binary refuses before writing anything", () => {
    for (const key of ["POST_DOORBELL_HERDR_BIN", "POST_DOORBELL_POST_BIN"]) {
      const host = makeHost("linux");
      const result = host.install([], { [key]: path.join(host.dir, "nope") });
      assert.equal(result.status, 1);
      assert.match(result.stderr, /cannot execute the (herdr|post) binary/);
      assert.equal(fs.existsSync(serviceFile(host)), false);
      assert.deepEqual(host.smCalls(), []);
    }
  });

  for (const [label, fault, why] of [
    ["herdr failing", { herdrFails: true }, /herdr agent list is failing/],
    ["post failing", { postFails: true }, /post participant list is failing/],
    ["a failing first scan", { watchFails: true }, /the first scan for ada failed at snapshot\b/],
  ]) {
    test(`an unhealthy first tick (${label}) stops the supervisor and fails`, () => {
      const host = makeHost("linux");
      host.agent("ada", "ada");
      fs.mkdirSync(path.join(host.doorbell, "prefs"), { recursive: true });
      fs.writeFileSync(path.join(host.doorbell, "prefs", "ada.json"), JSON.stringify({ version: 1, enabled: true, channels: [] }));
      Object.assign(host.control, fault);
      host.save();
      const result = host.install(["--startup-timeout-seconds", "6"]);
      assert.equal(result.status, 1, result.stdout + result.stderr);
      assert.match(result.stderr, why);
      assert.match(result.stderr, /it was stopped again/);
      assert.equal(host.sm().running[supervisorUnit(host)], undefined, "the supervisor is no longer running");
      assert.equal(host.receipt().supervisor.state, "failed_start");
    });
  }
});

describe("install: a healthy start with participants already armed", () => {
  // A reinstall finds participants armed before it started. A first scan
  // that finds nothing new rings nobody and records no outcome, and a busy or
  // focused pane is not scanned until it goes idle; neither is a failed start
  // (the devbox reinstall, 2026-09-23).
  test("an armed participant on an idle pane whose first scan has not finished holds up the start", () => {
    const host = makeHost("linux");
    host.agent("ada", "ada");
    fs.mkdirSync(path.join(host.doorbell, "prefs"), { recursive: true });
    fs.writeFileSync(path.join(host.doorbell, "prefs", "ada.json"), JSON.stringify({ version: 1, enabled: true, channels: [] }));
    Object.assign(host.control, { watchHangs: true });
    host.save();
    const result = host.install(["--startup-timeout-seconds", "4"]);
    assert.equal(result.status, 1, result.stdout + result.stderr);
    assert.match(result.stderr, /the first scan for ada is still running/);
    assert.equal(host.receipt().supervisor.state, "failed_start");
  });

  test("a first scan that fails after its pane went busy still fails the start", () => {
    // Grok, fix round 3: the scan started while the pane was idle; the pane
    // turning busy must not let the start pass before the scan's result.
    const host = makeHost("linux");
    host.agent("ada", "ada");
    fs.mkdirSync(path.join(host.doorbell, "prefs"), { recursive: true });
    fs.writeFileSync(path.join(host.doorbell, "prefs", "ada.json"), JSON.stringify({ version: 1, enabled: true, channels: [] }));
    Object.assign(host.control, { watchFlipsPaneThenFails: true });
    host.save();
    const result = host.install(["--startup-timeout-seconds", "15"]);
    assert.equal(result.status, 1, result.stdout + result.stderr);
    assert.match(result.stderr, /the first scan for ada failed/);
    assert.equal(host.receipt().supervisor.state, "failed_start");
  });

  for (const [label, pane] of [
    ["an idle pane with no new mail", {}],
    ["a busy pane", { status: "working" }],
    ["a focused pane", { focused: true }],
  ]) {
    test(`an armed participant on ${label} is a healthy start`, () => {
      const host = makeHost("linux");
      host.agent("ada", "ada");
      Object.assign(host.control.panes[0], pane);
      fs.mkdirSync(path.join(host.doorbell, "prefs"), { recursive: true });
      fs.writeFileSync(path.join(host.doorbell, "prefs", "ada.json"), JSON.stringify({ version: 1, enabled: true, channels: [] }));
      host.save();
      const result = host.install(["--startup-timeout-seconds", "6"]);
      ok(result);
      assert.equal(host.receipt().supervisor.state, "healthy");
      assert.ok(host.sm().running[supervisorUnit(host)], "the supervisor is still running");
    });
  }
});

describe("install: dry run", () => {
  test("--dry-run writes nothing and calls no mutating manager command", () => {
    const host = makeHost("linux");
    host.agent("ada", "ada");
    host.legacyTimer("ada");
    const result = host.install(["--dry-run"]);
    ok(result);
    assert.match(result.stdout, /\[dry-run\] writing .*post-doorbell-supervisor\.service/);
    assert.match(result.stdout, /\[dry-run\] would run: systemctl --user restart post-doorbell-supervisor\.service/);
    const migrate = host.install(["--migrate", "ada", "--dry-run"]);
    ok(migrate);
    assert.match(migrate.stdout, /would wait for a healthy subscription, then disable post-codex-doorbell@ada\.timer/);
    assert.equal(fs.existsSync(host.doorbell), false);
    assert.equal(fs.existsSync(serviceFile(host)), false);
    assert.ok(host.timerOn("ada"));
    assert.deepEqual(host.smCalls().filter((call) => !call.args.includes("is-enabled") && call.args[0] !== "print"), []);
  });
});

describe("legacy discovery", () => {
  test("settings come from the unit, its drop-ins, and the environment lines", () => {
    const host = makeHost("linux");
    host.legacyTimer("ada", { channels: ["ops"], participant: "ada" });
    host.legacyTimer("bob", { room: "beta" });
    const dropIn = path.join(host.unitDir, "post-codex-doorbell@ada.service.d");
    fs.mkdirSync(dropIn);
    fs.writeFileSync(path.join(dropIn, "override.conf"), '[Service]\nEnvironment="POST_CODEX_NOTIFY_CHANNELS=ops,release"\n');
    host.legacyTimer("cy", { extraEnv: ["POST_MAIL_ROOT=/elsewhere"] });
    const result = host.install(["--list-legacy", "--json"]);
    ok(result);
    const timers = Object.fromEntries(JSON.parse(result.stdout).timers.map((timer) => [timer.agent, timer]));
    assert.deepEqual(timers.ada.settings.channels, ["ops", "release"], "the drop-in wins");
    assert.equal(timers.ada.settings.participant, "ada");
    assert.deepEqual(timers.ada.settings.rooms, ["alpha"]);
    assert.equal(timers.ada.enabled, true);
    assert.equal(timers.ada.migratable, true);
    assert.deepEqual(timers.bob.settings.rooms, ["beta"]);
    assert.equal(timers.cy.migratable, false);
    assert.match(timers.cy.problem, /mail root \/elsewhere/);
  });

  test("macOS plists: agent, room, and channels", () => {
    const host = makeHost("darwin");
    host.legacyTimer("ada", { channels: ["ops", "release"] });
    const result = host.install(["--list-legacy", "--json"]);
    ok(result);
    const [timer] = JSON.parse(result.stdout).timers;
    assert.equal(timer.unit, "dev.post.codex-doorbell.ada");
    assert.equal(timer.enabled, true);
    assert.deepEqual(timer.settings.rooms, ["alpha"]);
    assert.deepEqual(timer.settings.channels, ["ops", "release"]);
    assert.equal(timer.migratable, true);
  });
});

for (const platform of ["linux", "darwin"]) {
  describe(`migration (${platform})`, () => {
    test("one timer: equivalent prefs, a healthy subscription, then only that timer is disabled", () => {
      const host = makeHost(platform);
      host.agent("ada", "ada");
      host.agent("bob", "bob");
      host.legacyTimer("ada", { channels: ["ops"] });
      host.legacyTimer("bob");
      const result = host.install(["--migrate", "ada", "--health-timeout-seconds", "20"]);
      ok(result);
      assert.match(result.stdout, /migrated to the supervisor \(ada on w1:p1\)/);
      const prefs = host.prefs("ada");
      assert.equal(prefs.enabled, true);
      assert.equal(prefs.focused, false);
      assert.deepEqual(prefs.channels, ["ops"]);
      assert.equal(prefs.selection.pane, "w1:p1");
      assert.equal(host.timerOn("ada"), false, "ada's timer is off");
      assert.equal(host.timerOn("bob"), true, "bob's timer is untouched");
      const entry = host.receipt().migrations[host.timerUnit("ada")];
      assert.equal(entry.state, "migrated");
      assert.equal(entry.enabled_before, true);
      assert.equal(entry.participant, "ada");
      const health = JSON.parse(fs.readFileSync(path.join(host.doorbell, "health.json"), "utf8"));
      const binding = health.bindings.find((row) => row.participant === "ada");
      assert.equal(binding.armed, true);
      assert.ok(binding.scan_prefs_version >= entry.prefs_version);
    });
  });
}

describe("migration: refusals keep the timer", () => {
  test("a room, pinned participant, or missing agent mismatch is not migrated (exit 3)", () => {
    const host = makeHost("linux");
    host.agent("ada", "ada");
    host.agent("bob", "bob");
    host.legacyTimer("ada", { room: "beta" });
    host.legacyTimer("bob", { participant: "someone-else" });
    host.legacyTimer("cy");
    const result = host.install(["--migrate-all", "--health-timeout-seconds", "10"]);
    assert.equal(result.status, 3, result.stdout + result.stderr);
    assert.match(result.stdout, /post-codex-doorbell@ada\.timer: not migrated: the unit watches room beta, but ada is bound to alpha/);
    assert.match(result.stdout, /post-codex-doorbell@bob\.timer: not migrated: the unit pins participant someone-else, but the pane carries bob/);
    assert.match(result.stdout, /post-codex-doorbell@cy\.timer: not migrated: `herdr agent get cy` failed/);
    for (const agent of ["ada", "bob", "cy"]) assert.equal(host.timerOn(agent), true, `${agent}'s timer stays on`);
    assert.equal(fs.existsSync(path.join(host.doorbell, "prefs", "ada.json")), false, "no subscription for a refused timer");
  });
});

describe("migration: interrupted and resumed", () => {
  for (const point of ["subscription_created", "timer_disabled"]) {
    test(`killed after ${point}, a rerun finishes without duplicating anything`, () => {
      const host = makeHost("linux");
      host.agent("ada", "ada");
      host.legacyTimer("ada", { channels: ["ops"] });
      const first = host.install(["--migrate", "ada", "--health-timeout-seconds", "20"], { POST_DOORBELL_INSTALL_TEST_CRASH_AFTER: point });
      assert.equal(first.status, 97, first.stdout + first.stderr);
      const mid = host.receipt().migrations["post-codex-doorbell@ada.timer"];
      assert.equal(mid.state, "subscription_created");
      assert.equal(host.timerOn("ada"), point === "subscription_created", "the timer is disabled only after a healthy subscription");
      const version = host.prefs("ada").version;

      const second = host.install(["--migrate", "ada", "--health-timeout-seconds", "20"]);
      ok(second);
      assert.match(second.stdout, /supervisor already running and current/);
      const done = host.receipt().migrations["post-codex-doorbell@ada.timer"];
      assert.equal(done.state, "migrated");
      assert.equal(done.enabled_before, true, "the pre-migration state survives the resume");
      assert.equal(host.prefs("ada").version, version, "the unchanged subscription is not rewritten");
      assert.equal(host.timerOn("ada"), false);

      const third = host.install(["--migrate", "ada"]);
      ok(third);
      assert.match(third.stdout, /already migrated/);
    });
  }
});

describe("migration: an unhealthy resume after the disable", () => {
  test("says the timer is already off, never that it stays on", () => {
    const host = makeHost("linux");
    host.agent("ada", "ada");
    host.legacyTimer("ada", { channels: ["ops"] });
    const first = host.install(["--migrate", "ada", "--health-timeout-seconds", "20"], { POST_DOORBELL_INSTALL_TEST_CRASH_AFTER: "timer_disabled" });
    assert.equal(first.status, 97, first.stdout + first.stderr);
    assert.equal(host.timerOn("ada"), false);
    // Scans now fail, and a changed subscription needs a new scan to prove it.
    Object.assign(host.control, { watchFails: true });
    host.save();
    const prefsFile = path.join(host.doorbell, "prefs", "ada.json");
    fs.writeFileSync(prefsFile, JSON.stringify({ ...host.prefs("ada"), focused: true }));
    const second = host.install(["--migrate", "ada", "--health-timeout-seconds", "3"]);
    assert.notEqual(second.status, 0, second.stdout + second.stderr);
    assert.match(second.stdout, /The timer is already off/);
    assert.doesNotMatch(second.stdout, /The timer stays on/);
    assert.equal(host.timerOn("ada"), false);
  });
});

describe("migration: health from an earlier supervisor", () => {
  test("a resume does not disable the timer on a restarted supervisor's stale health", async () => {
    // GLM, fix round 3: after a crash and restart, health.json is the dead
    // process's until the new one writes. Freeze the running supervisor so it
    // cannot write, keep its heartbeat fresh, and plant health whose writer is
    // another pid but whose scan would otherwise satisfy the migration.
    const host = makeHost("linux");
    host.agent("ada", "ada");
    host.legacyTimer("ada", { channels: ["ops"] });
    const first = host.install(["--migrate", "ada", "--health-timeout-seconds", "20"], { POST_DOORBELL_INSTALL_TEST_CRASH_AFTER: "subscription_created" });
    assert.equal(first.status, 97, first.stdout + first.stderr);
    assert.equal(host.timerOn("ada"), true);
    const healthFile = path.join(host.doorbell, "health.json");
    const scanned = () => {
      try {
        const health = JSON.parse(fs.readFileSync(healthFile, "utf8"));
        return health.bindings.find((row) => row.participant === "ada" && row.last_success_scan_at && row.scan_prefs_version >= host.prefs("ada").version) ? health : null;
      } catch {
        return null;
      }
    };
    let health = null;
    for (let i = 0; i < 100 && !(health = scanned()); i++) await new Promise((resolve) => setTimeout(resolve, 100));
    assert.ok(health, "the supervisor scanned ada under the new subscription");
    const pid = host.sm().running[supervisorUnit(host)];
    process.kill(pid, "SIGSTOP");
    const heartbeatFile = path.join(host.doorbell, "heartbeat.json");
    try {
      // Stamped ahead so the frozen supervisor reads as running for the whole
      // rerun (liveness clamps a future heartbeat to age 0).
      fs.writeFileSync(heartbeatFile, JSON.stringify({ pid, started_at: health.started_at, seq: 0, time: new Date(Date.now() + 20_000).toISOString() }) + "\n");
      fs.writeFileSync(healthFile, JSON.stringify({ ...health, pid: pid + 100000 }));
      const second = host.install(["--migrate", "ada", "--health-timeout-seconds", "3"]);
      assert.equal(second.status, 3, second.stdout + second.stderr);
      assert.match(second.stdout, /supervisor already running and current/);
      assert.match(second.stdout, /no health from the running supervisor/);
      assert.equal(host.timerOn("ada"), true, "the timer stays on until this supervisor proves the subscription");
    } finally {
      process.kill(pid, "SIGCONT");
    }
  });
});

describe("uninstall and restore", () => {
  function migratedHost() {
    const host = makeHost("linux");
    const binDir = path.join(host.dir, ".local", "bin");
    fs.mkdirSync(binDir, { recursive: true });
    fs.writeFileSync(path.join(binDir, "post-doorbell"), "#!/usr/bin/env python3\n# old daemon\n", { mode: 0o755 });
    host.agent("ada", "ada");
    host.agent("bob", "bob");
    host.legacyTimer("ada");
    host.legacyTimer("bob");
    ok(host.install(["--migrate-all", "--health-timeout-seconds", "20"]));
    assert.equal(host.timerOn("ada"), false);
    assert.equal(host.timerOn("bob"), false);
    return host;
  }

  test("default uninstall removes only the supervisor and prints the restoration command", () => {
    const host = migratedHost();
    const pid = host.sm().running[supervisorUnit(host)];
    const before = host.smCalls().length;
    const result = host.install(["--uninstall"]);
    ok(result);
    assert.equal(host.alive(pid), false, "the supervisor stopped");
    assert.equal(fs.existsSync(serviceFile(host)), false);
    assert.equal(fs.existsSync(path.join(host.dir, ".local", "share", "post-doorbell", "doorbell-supervisor.mjs")), false);
    assert.equal(fs.existsSync(path.join(host.dir, ".local", "bin", "post-doorbell")), false, "our shim is removed");
    assert.equal(host.timerOn("ada"), false, "legacy units are left alone");
    assert.equal(host.timerOn("bob"), false);
    const calls = host.smCalls().slice(before);
    assert.equal(calls.some((call) => call.args.some((arg) => arg.includes("codex-doorbell"))), false, "no call touched a legacy unit");
    assert.match(result.stdout, /--restore-legacy/);
    assert.match(result.stdout, /systemctl --user enable --now post-codex-doorbell@ada\.timer/);
    assert.ok(fs.existsSync(path.join(host.doorbell, "install-receipt.json")), "the receipt is kept for a later restore");
  });

  test("--restore-legacy re-enables unchanged units, refuses an edited one, and moves the script back", () => {
    const host = migratedHost();
    fs.appendFileSync(path.join(host.unitDir, "post-codex-doorbell@bob.service"), "# edited after install\n");
    const result = host.install(["--restore-legacy"]);
    assert.equal(result.status, 1, result.stdout + result.stderr);
    assert.match(result.stdout, /post-codex-doorbell@ada\.timer: re-enabling/);
    assert.match(result.stdout, /post-codex-doorbell@bob\.timer: NOT restored: .*post-codex-doorbell@bob\.service changed after install/);
    assert.match(result.stderr, /1 item\(s\) were not restored/);
    assert.equal(host.timerOn("ada"), true);
    assert.equal(host.timerOn("bob"), false, "the edited unit stays off");
    assert.equal(host.receipt().migrations["post-codex-doorbell@ada.timer"].state, "restored");
    assert.equal(host.receipt().migrations["post-codex-doorbell@bob.timer"].state, "migrated");
    assert.equal(fs.readFileSync(path.join(host.dir, ".local", "bin", "post-doorbell"), "utf8"), "#!/usr/bin/env python3\n# old daemon\n");
    assert.equal(host.sm().running[supervisorUnit(host)], undefined);
  });

  for (const platform of ["linux", "darwin"]) {
    test(`uninstall with nothing installed is a clean no-op (${platform})`, () => {
      const host = makeHost(platform);
      const result = host.install(["--uninstall"]);
      ok(result);
      assert.match(result.stdout, /nothing to restore/);
    });
  }

  test("macOS: restore bootstraps the recorded plist", () => {
    const host = makeHost("darwin");
    host.agent("ada", "ada");
    host.legacyTimer("ada");
    ok(host.install(["--migrate", "ada", "--health-timeout-seconds", "20"]));
    assert.equal(host.timerOn("ada"), false);
    assert.ok(host.sm().disabled.includes("dev.post.codex-doorbell.ada"));
    ok(host.install(["--restore-legacy"]));
    assert.equal(host.timerOn("ada"), true);
    assert.ok(host.sm().enabled.includes("dev.post.codex-doorbell.ada"));
  });
});
