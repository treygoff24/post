// Self-tests for install-systemd-doorbell.mjs. Run:
// node --test skills/post/hooks/install-systemd-doorbell.test.mjs
// Hermetic: POST_CODEX_DOORBELL_HOME and POST_CODEX_DOORBELL_INSTALL_DIR keep
// every derived path inside the temp root, and post/herdr/systemctl are
// control-driven stubs. No systemd user manager, live config, mail, or cursor
// is ever touched.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const DIR = path.dirname(fileURLToPath(import.meta.url));
const INSTALLER = path.join(DIR, "install-systemd-doorbell.mjs");
const MONITOR_SOURCE = path.join(DIR, "codex-notify-monitor.mjs");
const ROOT = fs.mkdtempSync(path.join(os.tmpdir(), "post-systemd-doorbell-test-"));
const INSTALL_DIR = path.join(ROOT, "hooks");
const CONTROL = path.join(ROOT, "control.json");
const POST = path.join(ROOT, "post-stub.mjs");
const HERDR = path.join(ROOT, "herdr-stub.mjs");
const SYSTEMCTL = path.join(ROOT, "systemctl-stub.mjs");
const POST_CALLS = path.join(ROOT, "post-calls.jsonl");
const HERDR_CALLS = path.join(ROOT, "herdr-calls.jsonl");
const SYSTEMCTL_CALLS = path.join(ROOT, "systemctl-calls.jsonl");

fs.writeFileSync(
  POST,
  [
    "#!/usr/bin/env node",
    'import fs from "node:fs";',
    "const args = process.argv.slice(2);",
    `fs.appendFileSync(${JSON.stringify(POST_CALLS)}, JSON.stringify(args) + "\\n");`,
    `const control = JSON.parse(fs.readFileSync(${JSON.stringify(CONTROL)}, "utf8"));`,
    'if (args[0] === "rooms") {',
    "  if (control.roomsStdout) process.stdout.write(control.roomsStdout);",
    "  if (control.roomsStderr) process.stderr.write(control.roomsStderr);",
    "  process.exit(control.roomsExit ?? 0);",
    "}",
    'if (args[0] === "channels") {',
    "  if (control.channelsStdout) process.stdout.write(control.channelsStdout);",
    "  if (control.channelsStderr) process.stderr.write(control.channelsStderr);",
    "  process.exit(control.channelsExit ?? 0);",
    "}",
    "if (control.postStdout) process.stdout.write(control.postStdout);",
    "if (control.postStderr) process.stderr.write(control.postStderr);",
    "process.exit(control.postExit ?? 0);",
    "",
  ].join("\n"),
  { mode: 0o755 }
);
fs.writeFileSync(
  HERDR,
  [
    "#!/usr/bin/env node",
    'import fs from "node:fs";',
    "const args = process.argv.slice(2);",
    `fs.appendFileSync(${JSON.stringify(HERDR_CALLS)}, JSON.stringify(args) + "\\n");`,
    `const control = JSON.parse(fs.readFileSync(${JSON.stringify(CONTROL)}, "utf8"));`,
    "if (control.herdrGetStdout) process.stdout.write(control.herdrGetStdout);",
    "if (control.herdrGetStderr) process.stderr.write(control.herdrGetStderr);",
    "process.exit(control.herdrGetExit ?? 0);",
    "",
  ].join("\n"),
  { mode: 0o755 }
);
fs.writeFileSync(
  SYSTEMCTL,
  [
    "#!/usr/bin/env node",
    'import fs from "node:fs";',
    "const args = process.argv.slice(2);",
    `fs.appendFileSync(${JSON.stringify(SYSTEMCTL_CALLS)}, JSON.stringify(args) + "\\n");`,
    `const control = JSON.parse(fs.readFileSync(${JSON.stringify(CONTROL)}, "utf8"));`,
    "const action = args[1] === \"daemon-reload\" ? \"DaemonReload\" : args[1] === \"enable\" ? \"Enable\" : \"Disable\";",
    "if (control[`systemctl${action}Stderr`]) process.stderr.write(control[`systemctl${action}Stderr`]);",
    "process.exit(control[`systemctl${action}Exit`] ?? 0);",
    "",
  ].join("\n"),
  { mode: 0o755 }
);

test.after(() => {
  fs.rmSync(ROOT, { recursive: true, force: true });
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

const AGENT = "lane-bot";
const HERDR_OK = JSON.stringify({
  result: { agent: { name: AGENT, agent_status: "idle", focused: false } },
});
const ROOMS_OK = JSON.stringify({
  ok: true,
  rooms: [{ name: "ops", path: "/tmp/ops", blocked: [] }],
  count: 1,
});
const CHANNELS_OK = JSON.stringify({
  ok: true,
  channels: [
    { name: "build", members: ["ops"] },
    { name: "ops", members: ["ops"] },
  ],
  count: 2,
});
const OK = { herdrGetStdout: HERDR_OK, roomsStdout: ROOMS_OK };
const OK_CHANNELS = { ...OK, channelsStdout: CHANNELS_OK };

function run(args, control = OK, env = {}) {
  setControl(control);
  return spawnSync(process.execPath, [INSTALLER, ...args], {
    encoding: "utf8",
    env: {
      ...process.env,
      POST_CODEX_DOORBELL_PLATFORM: "linux",
      POST_CODEX_DOORBELL_HOME: path.join(ROOT, "home"),
      POST_CODEX_DOORBELL_INSTALL_DIR: INSTALL_DIR,
      POST_CODEX_DOORBELL_POST_BIN: POST,
      POST_CODEX_DOORBELL_HERDR_BIN: HERDR,
      POST_CODEX_DOORBELL_SYSTEMCTL_BIN: SYSTEMCTL,
      POST_MAIL_ROOT: undefined,
      ...env,
    },
  });
}

function homeFor(name) {
  return path.join(ROOT, `home-${name}`);
}

function unitPath(home, suffix, agent = AGENT) {
  return path.join(home, ".config", "systemd", "user", `post-codex-doorbell@${agent}.${suffix}`);
}

function statePath(home, agent = AGENT) {
  return path.join(home, ".local", "state", "post-codex-doorbell", `${agent}.json`);
}

function logPath(home, agent = AGENT) {
  return path.join(home, ".local", "state", "post-codex-doorbell", `${agent}.log`);
}

function errorLogPath(home, agent = AGENT) {
  return path.join(home, ".local", "state", "post-codex-doorbell", `${agent}.error.log`);
}

function monitorPath(installDir = INSTALL_DIR) {
  return path.join(installDir, "post-codex-notify-monitor.mjs");
}

function assertNoArtifacts(home, installDir = INSTALL_DIR) {
  assert.ok(!fs.existsSync(unitPath(home, "service")));
  assert.ok(!fs.existsSync(unitPath(home, "timer")));
  assert.ok(!fs.existsSync(path.join(home, ".config", "systemd", "user")));
  assert.ok(!fs.existsSync(monitorPath(installDir)));
}

test("refuses non-Linux before resolution, preflight, or writes", () => {
  const home = homeFor("non-linux");
  const before = calls(POST_CALLS).length;
  const result = run(["--room", "ops", "--agent", AGENT], OK, {
    POST_CODEX_DOORBELL_PLATFORM: "darwin",
    POST_CODEX_DOORBELL_HOME: home,
  });
  assert.equal(result.status, 1, result.stderr);
  assert.match(result.stderr, /Linux only/);
  assert.equal(calls(POST_CALLS).length, before);
  assertNoArtifacts(home);
});

test("usage validation mirrors the launchd CLI", () => {
  const cases = [
    { args: [], match: /requires --room/ },
    { args: ["--room", "ops"], match: /requires --room .*--agent/ },
    { args: ["--agent", AGENT], match: /requires --room/ },
    { args: ["--uninstall"], match: /requires --agent/ },
    { args: ["--room", "ops", "--agent", AGENT, "--bogus"], match: /unknown argument/ },
    { args: ["--room", "ops", "--room", "sol", "--agent", AGENT], match: /duplicate --room/ },
    { args: ["--room", "bad room!", "--agent", AGENT], match: /invalid room name/ },
    { args: ["--room", "ops", "--agent", "1bad"], match: /invalid Herdr agent name/ },
    { args: ["--room", "ops", "--agent", AGENT, "--interval-seconds", "0"], match: /positive integer/ },
    { args: ["--uninstall", "--agent", AGENT, "--channel", "build"], match: /not valid with --uninstall/ },
  ];
  for (const c of cases) {
    const result = run(c.args);
    assert.equal(result.status, 2, `${c.args.join(" ")} -> ${result.stderr}`);
    assert.match(result.stderr, c.match);
  }
});

test("each preflight refusal writes nothing", () => {
  const failures = [
    { control: { ...OK, roomsExit: 1 }, match: /preflight failed: `post rooms`/ },
    { control: { ...OK, roomsStdout: "not-json\n" }, match: /rooms.*malformed output/ },
    { control: { ...OK, roomsStdout: JSON.stringify({ ok: true, count: 0 }) }, match: /unexpected schema/ },
    {
      control: { ...OK, roomsStdout: JSON.stringify({ ok: true, rooms: [{ name: "other" }] }) },
      match: /room 'ops' is not registered/,
    },
    {
      control: { ...OK, roomsStdout: JSON.stringify({ ok: true, rooms: [{ path: "/tmp/ops" }] }) },
      match: /malformed room entries/,
    },
    {
      control: {
        ...OK_CHANNELS,
        channelsStdout: JSON.stringify({ ok: true, channels: [{ name: "build", members: "ops" }] }),
      },
      args: ["--room", "ops", "--agent", AGENT, "--channel", "build"],
      match: /malformed channel entries/,
    },
    {
      control: {
        ...OK,
        channelsStdout: JSON.stringify({ ok: true, channels: [{ name: "other", members: ["ops"] }] }),
      },
      args: ["--room", "ops", "--agent", AGENT, "--channel", "build"],
      match: /channel 'build' is not listed/,
    },
    {
      control: { ...OK_CHANNELS, channelsExit: 1 },
      args: ["--room", "ops", "--agent", AGENT, "--channel", "build"],
      match: /preflight failed: `post channels`/,
    },
    {
      control: {
        ...OK,
        channelsStdout: JSON.stringify({ ok: true, channels: [{ name: "build", members: ["sol"] }] }),
      },
      args: ["--room", "ops", "--agent", AGENT, "--channel", "build"],
      match: /not a member of channel 'build'/,
    },
    { control: { ...OK, postExit: 1 }, match: /post watch/ },
    { control: { ...OK, postStdout: "not-json\n" }, match: /watch.*malformed output/ },
    { control: { ...OK, herdrGetExit: 1 }, match: /herdr agent get/ },
    { control: { ...OK, herdrGetStdout: "garbage" }, match: /herdr agent get.*malformed output/ },
    {
      control: { ...OK, herdrGetStdout: JSON.stringify({ result: { agent: { agent_status: "idle" } } }) },
      match: /lane-bot.*must be a named herdr agent/,
    },
    {
      control: { ...OK, herdrGetStdout: JSON.stringify({ result: { agent: { name: "someone-else" } } }) },
      match: /returned agent/,
    },
  ];
  for (const [index, failing] of failures.entries()) {
    const home = homeFor(`preflight-${index}`);
    const installDir = path.join(ROOT, `hooks-preflight-${index}`);
    const result = run(
      failing.args ?? ["--room", "ops", "--agent", AGENT],
      failing.control,
      { POST_CODEX_DOORBELL_HOME: home, POST_CODEX_DOORBELL_INSTALL_DIR: installDir }
    );
    assert.equal(result.status, 1, `case ${index} -> ${result.stderr}`);
    assert.match(result.stderr, failing.match);
    assertNoArtifacts(home, installDir);
  }
});

test("binary resolution and invalid POST_MAIL_ROOT refuse before preflight", () => {
  for (const [name, env, match] of [
    ["missing-post", { POST_CODEX_DOORBELL_POST_BIN: path.join(ROOT, "missing-post") }, /cannot execute the post binary/],
    ["missing-systemctl", { POST_CODEX_DOORBELL_SYSTEMCTL_BIN: path.join(ROOT, "missing-systemctl") }, /cannot execute the systemctl binary/],
    ["relative-root", { POST_MAIL_ROOT: "relative/root" }, /POST_MAIL_ROOT must be an absolute path/],
    ["empty-root", { POST_MAIL_ROOT: "" }, /POST_MAIL_ROOT must be an absolute path/],
  ]) {
    const home = homeFor(name);
    const postBefore = calls(POST_CALLS).length;
    const result = run(["--room", "ops", "--agent", AGENT], OK, {
      POST_CODEX_DOORBELL_HOME: home,
      POST_CODEX_DOORBELL_INSTALL_DIR: path.join(ROOT, `hooks-${name}`),
      ...env,
    });
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, match);
    assert.equal(calls(POST_CALLS).length, postBefore);
    assertNoArtifacts(home, path.join(ROOT, `hooks-${name}`));
  }
});

test("successful install writes executable monitor and 0644 service and timer", () => {
  const home = homeFor("install");
  const result = run(
    ["--room", "ops", "--agent", AGENT, "--channel", "build", "--channel", "ops", "--channel", "build", "--interval-seconds", "30"],
    OK_CHANNELS,
    { POST_CODEX_DOORBELL_HOME: home }
  );
  assert.equal(result.status, 0, result.stderr);

  const service = unitPath(home, "service");
  const timer = unitPath(home, "timer");
  const monitor = monitorPath();
  assert.deepEqual(fs.readFileSync(monitor), fs.readFileSync(MONITOR_SOURCE));
  assert.equal(fs.statSync(monitor).mode & 0o777, 0o755);
  assert.equal(fs.statSync(service).mode & 0o777, 0o644);
  assert.equal(fs.statSync(timer).mode & 0o777, 0o644);

  const serviceText = fs.readFileSync(service, "utf8");
  const timerText = fs.readFileSync(timer, "utf8");
  assert.match(serviceText, /^Type=oneshot$/m);
  assert.match(serviceText, new RegExp(`^ExecStart=${escapeRegExp(process.execPath)} `, "m"));
  assert.match(serviceText, new RegExp(`^Environment=HOME=${escapeRegExp(home)}$`, "m"));
  assert.match(serviceText, new RegExp(`^Environment=POST_CODEX_NOTIFY_POST_BIN=${escapeRegExp(POST)}$`, "m"));
  assert.match(serviceText, new RegExp(`^Environment=POST_CODEX_NOTIFY_HERDR_BIN=${escapeRegExp(HERDR)}$`, "m"));
  assert.match(serviceText, new RegExp(`^Environment=POST_CODEX_NOTIFY_HERDR_AGENT=${AGENT}$`, "m"));
  assert.match(serviceText, new RegExp(`^Environment=POST_CODEX_NOTIFY_STATE=${escapeRegExp(statePath(home))}$`, "m"));
  assert.match(serviceText, /^Environment=POST_CODEX_NOTIFY_CHANNELS=build,ops$/m);
  assert.match(serviceText, new RegExp(`^StandardOutput=append:${escapeRegExp(logPath(home))}$`, "m"));
  assert.match(serviceText, new RegExp(`^StandardError=append:${escapeRegExp(errorLogPath(home))}$`, "m"));
  assert.match(timerText, /^OnUnitActiveSec=30s$/m);
  assert.match(timerText, /^Unit=post-codex-doorbell@lane-bot\.service$/m);
  assert.deepEqual(calls(SYSTEMCTL_CALLS).slice(-2), [
    ["--user", "daemon-reload"],
    ["--user", "enable", "--now", "post-codex-doorbell@lane-bot.timer"],
  ]);
});

test("POST_MAIL_ROOT is pinned verbatim when absolute and omitted when unset", () => {
  const pinnedHome = homeFor("mail-root");
  const mailRoot = "/custom/mail&root";
  const pinned = run(["--room", "ops", "--agent", AGENT], OK, {
    POST_CODEX_DOORBELL_HOME: pinnedHome,
    POST_MAIL_ROOT: mailRoot,
  });
  assert.equal(pinned.status, 0, pinned.stderr);
  assert.match(
    fs.readFileSync(unitPath(pinnedHome, "service"), "utf8"),
    new RegExp(`^Environment=POST_MAIL_ROOT=${escapeRegExp(mailRoot)}$`, "m")
  );

  const unsetHome = homeFor("mail-root-unset");
  const unset = run(["--room", "ops", "--agent", AGENT], OK, {
    POST_CODEX_DOORBELL_HOME: unsetHome,
  });
  assert.equal(unset.status, 0, unset.stderr);
  assert.ok(!fs.readFileSync(unitPath(unsetHome, "service"), "utf8").includes("POST_MAIL_ROOT"));
  assert.match(fs.readFileSync(unitPath(unsetHome, "timer"), "utf8"), /^OnUnitActiveSec=5s$/m);
});

test("uninstall disables the exact timer and removes only the target files", () => {
  const home = homeFor("uninstall");
  const seeded = run(["--room", "ops", "--agent", AGENT], OK, {
    POST_CODEX_DOORBELL_HOME: home,
  });
  assert.equal(seeded.status, 0, seeded.stderr);
  fs.mkdirSync(path.dirname(statePath(home)), { recursive: true });
  fs.writeFileSync(statePath(home), "{}");
  fs.writeFileSync(logPath(home), "tick\n");
  fs.writeFileSync(errorLogPath(home), "err\n");

  const other = "other";
  fs.writeFileSync(unitPath(home, "service", other), "other service\n");
  fs.writeFileSync(unitPath(home, "timer", other), "other timer\n");
  fs.writeFileSync(statePath(home, other), "{}");
  fs.writeFileSync(logPath(home, other), "other log\n");

  const result = run(["--uninstall", "--agent", AGENT], {}, {
    POST_CODEX_DOORBELL_HOME: home,
  });
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /doorbell uninstalled/);
  for (const file of [unitPath(home, "service"), unitPath(home, "timer"), statePath(home), logPath(home), errorLogPath(home)]) {
    assert.ok(!fs.existsSync(file), `removed ${file}`);
  }
  for (const file of [unitPath(home, "service", other), unitPath(home, "timer", other), statePath(home, other), logPath(home, other)]) {
    assert.ok(fs.existsSync(file), `preserved ${file}`);
  }
  assert.ok(fs.existsSync(monitorPath()), "shared monitor survives");
  assert.deepEqual(calls(SYSTEMCTL_CALLS).at(-2), [
    "--user",
    "disable",
    "--now",
    "post-codex-doorbell@lane-bot.timer",
  ]);
  assert.deepEqual(calls(SYSTEMCTL_CALLS).at(-1), ["--user", "daemon-reload"]);
});

test("systemctl lifecycle failures are reported after files are materialized", () => {
  const daemonHome = homeFor("daemon-fail");
  const daemon = run(["--room", "ops", "--agent", AGENT], { ...OK, systemctlDaemonReloadExit: 1, systemctlDaemonReloadStderr: "reload failed" }, {
    POST_CODEX_DOORBELL_HOME: daemonHome,
  });
  assert.equal(daemon.status, 1);
  assert.match(daemon.stderr, /systemctl --user daemon-reload failed/);
  assert.ok(fs.existsSync(unitPath(daemonHome, "service")));

  const enableHome = homeFor("enable-fail");
  const enable = run(["--room", "ops", "--agent", AGENT], { ...OK, systemctlEnableExit: 1, systemctlEnableStderr: "enable failed" }, {
    POST_CODEX_DOORBELL_HOME: enableHome,
  });
  assert.equal(enable.status, 1);
  assert.match(enable.stderr, /systemctl --user enable --now failed/);
  assert.ok(fs.existsSync(unitPath(enableHome, "timer")));
});

function escapeRegExp(value) {
  return String(value).replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}
