// Self-tests for install-codex-doorbell.mjs. Run: node --test skills/post/hooks/*.test.mjs
// Hermetic: POST_CODEX_DOORBELL_HOME and POST_CODEX_DOORBELL_INSTALL_DIR keep
// every derived path inside the temp root, and post/herdr/launchctl are
// control-driven stubs. No launchd job, live config, mail, or cursor is ever
// touched.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const DIR = path.dirname(fileURLToPath(import.meta.url));
const INSTALLER = path.join(DIR, "install-codex-doorbell.mjs");
const MONITOR_SOURCE = path.join(DIR, "codex-notify-monitor.mjs");
const ROOT = fs.mkdtempSync(path.join(os.tmpdir(), "post-codex-doorbell-test-"));
const INSTALL_DIR = path.join(ROOT, "hooks");
const CONTROL = path.join(ROOT, "control.json");
const POST = path.join(ROOT, "post-stub.mjs");
const HERDR = path.join(ROOT, "herdr-stub.mjs");
const LAUNCHCTL = path.join(ROOT, "launchctl-stub.mjs");
const POST_CALLS = path.join(ROOT, "post-calls.jsonl");
const HERDR_CALLS = path.join(ROOT, "herdr-calls.jsonl");
const LAUNCHCTL_CALLS = path.join(ROOT, "launchctl-calls.jsonl");

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
    'if (control.herdrGetStdout) process.stdout.write(control.herdrGetStdout);',
    'if (control.herdrGetStderr) process.stderr.write(control.herdrGetStderr);',
    "process.exit(control.herdrGetExit ?? 0);",
    "",
  ].join("\n"),
  { mode: 0o755 }
);
fs.writeFileSync(
  LAUNCHCTL,
  [
    "#!/usr/bin/env node",
    'import fs from "node:fs";',
    "const args = process.argv.slice(2);",
    `fs.appendFileSync(${JSON.stringify(LAUNCHCTL_CALLS)}, JSON.stringify(args) + "\\n");`,
    `const control = JSON.parse(fs.readFileSync(${JSON.stringify(CONTROL)}, "utf8"));`,
    'const sub = args[0] === "bootout" ? "Bootout" : "Bootstrap";',
    'if (control[`launchctl${sub}Stderr`]) process.stderr.write(control[`launchctl${sub}Stderr`]);',
    "process.exit(control[`launchctl${sub}Exit`] ?? 0);",
    "",
  ].join("\n"),
  { mode: 0o755 }
);

test.after(() => {
  // ROOT is a uniquely named temp dir this test created; plain stdlib
  // removal is the portable cleanup, no external binary involved.
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
    {
      name: "build",
      created: "2026-08-04 22:00:00 -0400",
      created_by: "ops",
      members: ["ops"],
      messages: 0,
    },
    {
      name: "ops",
      created: "2026-08-04 22:00:00 -0400",
      created_by: "ops",
      members: ["ops"],
      messages: 0,
    },
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
      POST_CODEX_DOORBELL_PLATFORM: "darwin",
      POST_CODEX_DOORBELL_HOME: path.join(ROOT, "home"),
      POST_CODEX_DOORBELL_INSTALL_DIR: INSTALL_DIR,
      POST_CODEX_DOORBELL_POST_BIN: POST,
      POST_CODEX_DOORBELL_HERDR_BIN: HERDR,
      POST_CODEX_DOORBELL_LAUNCHCTL_BIN: LAUNCHCTL,
      // An ambient POST_MAIL_ROOT must never leak into tests; explicit
      // overrides come through `env`.
      POST_MAIL_ROOT: undefined,
      ...env,
    },
  });
}

function homeFor(name) {
  return path.join(ROOT, `home-${name}`);
}

function plistPath(home, agent = AGENT) {
  return path.join(home, "Library", "LaunchAgents", `dev.post.codex-doorbell.${agent}.plist`);
}

// The installer XML-escapes every plist value; the test mirrors that for
// assertions against derived, possibly escapable, paths.
function escapeXml(value) {
  return String(value)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&apos;");
}

test("usage errors reject unknown, missing, duplicate, and invalid arguments", () => {
  const cases = [
    { args: [], match: /requires --room/ },
    { args: ["--room", "ops"], match: /requires --room .*--agent/ },
    { args: ["--agent", AGENT], match: /requires --room/ },
    { args: ["--uninstall"], match: /requires --agent/ },
    { args: ["--room", "ops", "--agent", AGENT, "--bogus"], match: /unknown argument/ },
    { args: ["--room", "ops", "--agent", AGENT, "stray"], match: /unknown argument/ },
    { args: ["--room", "ops", "--room", "sol", "--agent", AGENT], match: /duplicate --room/ },
    { args: ["--room", "ops", "--agent", AGENT, "--agent", "x"], match: /duplicate --agent/ },
    {
      args: ["--room", "ops", "--agent", AGENT, "--uninstall", "--uninstall"],
      match: /duplicate --uninstall/,
    },
    { args: ["--room", "bad room!", "--agent", AGENT], match: /invalid room name/ },
    { args: ["--room", "ok-room", "--agent", "1bad-start"], match: /invalid Herdr agent name/ },
    { args: ["--room", "ok-room", "--agent", "x".repeat(33)], match: /invalid Herdr agent name/ },
    {
      args: ["--uninstall", "--agent", AGENT, "--room", "ops"],
      match: /--room is not valid with --uninstall/,
    },
    {
      args: ["--uninstall", "--agent", AGENT, "--channel", "build"],
      match: /--channel is not valid with --uninstall/,
    },
    {
      args: ["--uninstall", "--agent", AGENT, "--interval-seconds", "30"],
      match: /--interval-seconds is not valid with --uninstall/,
    },
    {
      args: ["--room", "ops", "--agent", AGENT, "--channel", "bad channel!"],
      match: /invalid channel name/,
    },
    {
      args: ["--room", "ops", "--agent", AGENT, "--interval-seconds"],
      match: /--interval-seconds requires a value/,
    },
    {
      args: [
        "--room",
        "ops",
        "--agent",
        AGENT,
        "--interval-seconds",
        "30",
        "--interval-seconds",
        "60",
      ],
      match: /duplicate --interval-seconds/,
    },
    {
      args: ["--room", "ops", "--agent", AGENT, "--interval-seconds", "0"],
      match: /positive integer/,
    },
    {
      args: ["--room", "ops", "--agent", AGENT, "--interval-seconds", "1.5"],
      match: /positive integer/,
    },
    { args: ["--room", "ops", "--agent", AGENT, "--"], match: /unknown argument/ },
  ];
  for (const c of cases) {
    const result = run(c.args);
    assert.equal(result.status, 2, `${c.args.join(" ")} -> ${result.stderr}`);
    assert.match(result.stderr, c.match);
  }
});

test("an explicit interval is written to StartInterval", () => {
  const home = homeFor("interval");
  const result = run(
    ["--room", "ops", "--agent", AGENT, "--interval-seconds", "30"],
    OK,
    { POST_CODEX_DOORBELL_HOME: home }
  );
  assert.equal(result.status, 0, result.stderr);
  const plist = fs.readFileSync(plistPath(home), "utf8");
  assert.ok(plist.includes("<integer>30</integer>"), "explicit StartInterval");
  assert.ok(!plist.includes("<integer>5</integer>"), "default interval must be replaced");
});

test("installs a machine-independent launch agent with the exact per-agent state", () => {
  const home = path.join(homeFor("amp"), "lantern&house");
  const result = run(["--room", "ops", "--agent", AGENT], OK, {
    POST_CODEX_DOORBELL_HOME: home,
  });
  assert.equal(result.status, 0, result.stderr);

  const plist = fs.readFileSync(plistPath(home), "utf8");
  const monitor = path.join(INSTALL_DIR, "post-codex-notify-monitor.mjs");

  assert.deepEqual(
    fs.readFileSync(monitor, "utf8"),
    fs.readFileSync(MONITOR_SOURCE, "utf8"),
    "monitor copy must match the installer's own source"
  );
  assert.equal(fs.statSync(monitor).mode & 0o777, 0o755);

  // XML-escaped values: the raw ampersand must never appear in the plist.
  assert.ok(plist.includes(escapeXml(home)), "home must be XML-escaped in the plist");
  assert.ok(!plist.includes(home), "raw unescaped home path must not appear");

  // Exact per-agent state and binaries.
  const state = path.join(home, ".local", "state", "post-codex-doorbell", `${AGENT}.json`);
  const log = path.join(home, "Library", "Logs", `post-codex-doorbell-${AGENT}.log`);
  const errorLog = path.join(home, "Library", "Logs", `post-codex-doorbell-${AGENT}.error.log`);
  assert.ok(plist.includes("<key>POST_CODEX_NOTIFY_STATE</key>"));
  assert.ok(plist.includes(`<string>${escapeXml(state)}</string>`));
  assert.ok(plist.includes("<key>POST_CODEX_NOTIFY_POST_BIN</key>"));
  assert.ok(plist.includes(`<string>${escapeXml(POST)}</string>`));
  assert.ok(plist.includes("<key>POST_CODEX_NOTIFY_HERDR_BIN</key>"));
  assert.ok(plist.includes(`<string>${escapeXml(HERDR)}</string>`));
  assert.ok(plist.includes("<key>POST_CODEX_NOTIFY_HERDR_AGENT</key>"));
  assert.ok(plist.includes(`<string>${AGENT}</string>`));
  assert.ok(plist.includes("<key>HOME</key>"));
  assert.ok(plist.includes(`<string>${escapeXml(home)}</string>`));
  assert.ok(
    !plist.includes("POST_CODEX_NOTIFY_CHANNELS"),
    "channels key is omitted when none were requested"
  );

  // Launchd shape.
  assert.ok(plist.includes(`<string>${escapeXml(`dev.post.codex-doorbell.${AGENT}`)}</string>`), "label");
  assert.ok(plist.includes(`<string>${escapeXml(process.execPath)}</string>`), "absolute node");
  assert.ok(plist.includes(`<string>${escapeXml(monitor)}</string>`), "installed monitor");
  assert.ok(plist.includes("<string>ops</string>"), "room");
  assert.ok(plist.includes("<integer>5</integer>"), "StartInterval");
  assert.ok(plist.includes("<true/>"), "RunAtLoad");
  assert.ok(plist.includes("<string>Background</string>"), "ProcessType");
  assert.ok(plist.includes(`<string>${escapeXml(log)}</string>`));
  assert.ok(plist.includes(`<string>${escapeXml(errorLog)}</string>`));

  // No Trey- or machine-specific literals beyond the ambient node binary.
  const stripped = plist.replaceAll(escapeXml(process.execPath), "");
  for (const literal of ["trey", "treygoff", "sol", "cmux", "/Users/"]) {
    assert.ok(!stripped.includes(literal), `plist must not contain ${literal}`);
  }
});

test("repeatable --channel wires a deduped POST_CODEX_NOTIFY_CHANNELS plist value", () => {
  const home = homeFor("channels");
  const result = run(
    ["--room", "ops", "--agent", AGENT, "--channel", "build", "--channel", "ops", "--channel", "build"],
    OK_CHANNELS,
    { POST_CODEX_DOORBELL_HOME: home }
  );
  assert.equal(result.status, 0, result.stderr);
  const plist = fs.readFileSync(plistPath(home), "utf8");
  assert.ok(plist.includes("<key>POST_CODEX_NOTIFY_CHANNELS</key>"));
  assert.ok(plist.includes("<string>build,ops</string>"));
  assert.deepEqual(
    calls(POST_CALLS).filter((a) => a[0] === "channels").at(-1),
    ["channels"]
  );

  const again = run(
    ["--room", "ops", "--agent", AGENT, "--channel", "build", "--channel", "ops"],
    OK_CHANNELS,
    { POST_CODEX_DOORBELL_HOME: home }
  );
  assert.equal(again.status, 0, again.stderr);
  assert.equal(fs.readFileSync(plistPath(home), "utf8"), plist, "re-run stays byte-stable");
});

test("direct-only install never calls post channels", () => {
  const home = homeFor("no-channels-call");
  const postBefore = calls(POST_CALLS).length;
  const result = run(["--room", "ops", "--agent", AGENT], OK, {
    POST_CODEX_DOORBELL_HOME: home,
  });
  assert.equal(result.status, 0, result.stderr);
  const newCalls = calls(POST_CALLS).slice(postBefore);
  assert.ok(newCalls.every((args) => args[0] !== "channels"));
  assert.deepEqual(
    newCalls.map((args) => args[0]),
    ["rooms", "watch"]
  );
});

test("a valid-but-unregistered room writes nothing", () => {
  const home = homeFor("unregistered");
  const installDir = path.join(ROOT, "hooks-unregistered");
  const monitor = path.join(installDir, "post-codex-notify-monitor.mjs");
  const postBefore = calls(POST_CALLS).length;
  const herdrBefore = calls(HERDR_CALLS).length;
  const result = run(
    ["--room", "ops", "--agent", AGENT],
    {
      ...OK,
      roomsStdout: JSON.stringify({
        ok: true,
        rooms: [{ name: "other", path: "/tmp/other", blocked: [] }],
        count: 1,
      }),
    },
    {
      POST_CODEX_DOORBELL_HOME: home,
      POST_CODEX_DOORBELL_INSTALL_DIR: installDir,
    }
  );
  assert.equal(result.status, 1, result.stderr);
  assert.match(result.stderr, /not registered/);
  assert.ok(!fs.existsSync(plistPath(home)));
  assert.ok(!fs.existsSync(path.join(home, "Library", "LaunchAgents")));
  assert.ok(!fs.existsSync(monitor));
  const newPostCalls = calls(POST_CALLS).slice(postBefore);
  assert.deepEqual(newPostCalls, [["rooms"]]);
  assert.equal(calls(HERDR_CALLS).length, herdrBefore);
});

test("preflight or resolution failures write nothing", () => {
  const failures = [
    { control: { ...OK, roomsExit: 1 }, match: /preflight failed: `post rooms`/ },
    {
      control: { ...OK, roomsStdout: "not-json\n" },
      match: /preflight failed: `post rooms` printed malformed output/,
    },
    {
      control: { ...OK, roomsStdout: JSON.stringify({ ok: true, count: 0 }) },
      match: /unexpected schema/,
    },
    {
      control: {
        ...OK,
        roomsStdout: JSON.stringify({
          ok: false,
          rooms: [{ name: "ops", path: "/tmp/ops", blocked: [] }],
          count: 1,
        }),
      },
      match: /unexpected schema/,
    },
    {
      control: {
        ...OK,
        roomsStdout: JSON.stringify({
          ok: true,
          rooms: [{ path: "/tmp/ops" }],
          count: 1,
        }),
      },
      match: /malformed room entries/,
    },
    {
      control: {
        ...OK_CHANNELS,
        channelsStdout: JSON.stringify({
          ok: true,
          channels: [
            {
              name: "build",
              created: "t",
              created_by: "ops",
              members: "ops",
              messages: 0,
            },
          ],
          count: 1,
        }),
      },
      args: ["--room", "ops", "--agent", AGENT, "--channel", "build"],
      match: /malformed channel entries/,
    },
    {
      control: {
        ...OK,
        channelsStdout: JSON.stringify({
          ok: true,
          channels: [
            {
              name: "other",
              created: "t",
              created_by: "ops",
              members: ["ops"],
              messages: 0,
            },
          ],
          count: 1,
        }),
      },
      args: ["--room", "ops", "--agent", AGENT, "--channel", "build"],
      match: /channel 'build' is not listed/,
    },
    {
      control: {
        ...OK,
        channelsStdout: JSON.stringify({
          ok: true,
          channels: [
            {
              name: "build",
              created: "t",
              created_by: "ops",
              members: ["sol"],
              messages: 0,
            },
          ],
          count: 1,
        }),
      },
      args: ["--room", "ops", "--agent", AGENT, "--channel", "build"],
      match: /not a member of channel 'build'/,
    },
    { control: { ...OK, postExit: 1 }, match: /preflight failed/ },
    {
      control: { ...OK, postStdout: "not-json\n" },
      match: /preflight failed: .*malformed output/,
    },
    { control: { ...OK, herdrGetExit: 1 }, match: /preflight failed/ },
    { control: { ...OK, herdrGetStdout: "garbage" }, match: /preflight failed: .*malformed output/ },
    {
      control: {
        ...OK,
        herdrGetStdout: JSON.stringify({ result: { agent: { name: "someone-else" } } }),
      },
      match: /returned agent/,
    },
  ];
  for (const [index, failing] of failures.entries()) {
    const home = homeFor(`fail-${index}`);
    const installDir = path.join(ROOT, `hooks-fail-${index}`);
    const monitor = path.join(installDir, "post-codex-notify-monitor.mjs");
    const result = run(
      failing.args ?? ["--room", "ops", "--agent", AGENT],
      failing.control,
      {
        POST_CODEX_DOORBELL_HOME: home,
        POST_CODEX_DOORBELL_INSTALL_DIR: installDir,
      }
    );
    assert.equal(result.status, 1, `case ${index} -> ${result.stderr}`);
    assert.match(result.stderr, failing.match);
    assert.ok(!fs.existsSync(plistPath(home)), `case ${index} must not write the plist`);
    assert.ok(
      !fs.existsSync(path.join(home, "Library", "LaunchAgents")),
      `case ${index} must not create the LaunchAgents directory`
    );
    assert.ok(!fs.existsSync(monitor), `case ${index} must not touch the monitor`);
  }

  const home = homeFor("resolve");
  const installDir = path.join(ROOT, "hooks-resolve");
  const monitor = path.join(installDir, "post-codex-notify-monitor.mjs");
  const result = run(
    ["--room", "ops", "--agent", AGENT],
    OK,
    {
      POST_CODEX_DOORBELL_HOME: home,
      POST_CODEX_DOORBELL_INSTALL_DIR: installDir,
      POST_CODEX_DOORBELL_POST_BIN: path.join(ROOT, "missing-post"),
    }
  );
  assert.equal(result.status, 1);
  assert.match(result.stderr, /cannot execute the post binary/);
  assert.ok(!fs.existsSync(plistPath(home)));
  assert.ok(!fs.existsSync(monitor));
});

test("atomic writes refuse a planted predictable legacy temp symlink", () => {
  const home = homeFor("symlink-temp");
  const installDir = path.join(ROOT, "hooks-symlink-temp");
  fs.mkdirSync(installDir, { recursive: true });
  const monitor = path.join(installDir, "post-codex-notify-monitor.mjs");
  const victim = path.join(installDir, "victim-secret.mjs");
  fs.writeFileSync(victim, "keep-me\n");
  fs.symlinkSync(victim, `${monitor}.${process.pid}.tmp`);

  const result = run(["--room", "ops", "--agent", AGENT], OK, {
    POST_CODEX_DOORBELL_HOME: home,
    POST_CODEX_DOORBELL_INSTALL_DIR: installDir,
  });
  assert.equal(result.status, 0, result.stderr);
  assert.equal(fs.readFileSync(victim, "utf8"), "keep-me\n");
  assert.deepEqual(
    fs.readFileSync(monitor, "utf8"),
    fs.readFileSync(MONITOR_SOURCE, "utf8")
  );
});

test("re-run is idempotent, byte-stable, and tolerates a not-loaded bootout", () => {
  const home = homeFor("rerun");
  const first = run(["--room", "ops", "--agent", AGENT], OK, {
    POST_CODEX_DOORBELL_HOME: home,
  });
  assert.equal(first.status, 0, first.stderr);

  const plist = plistPath(home);
  const monitor = path.join(INSTALL_DIR, "post-codex-notify-monitor.mjs");
  const plistOnce = fs.readFileSync(plist);
  const monitorOnce = fs.readFileSync(monitor);
  const plistMtime = fs.statSync(plist, { bigint: true }).mtimeNs;
  const monitorMtime = fs.statSync(monitor, { bigint: true }).mtimeNs;

  const second = run(
    ["--room", "ops", "--agent", AGENT],
    {
      ...OK,
      launchctlBootoutStderr: "Boot-out failed: 3: No such process",
      launchctlBootoutExit: 1,
    },
    { POST_CODEX_DOORBELL_HOME: home }
  );
  assert.equal(second.status, 0, second.stderr);
  assert.deepEqual(fs.readFileSync(plist), plistOnce, "plist bytes must not change on re-run");
  assert.deepEqual(
    fs.readFileSync(monitor),
    monitorOnce,
    "monitor bytes must not change on re-run"
  );
  assert.equal(fs.statSync(plist, { bigint: true }).mtimeNs, plistMtime);
  assert.equal(fs.statSync(monitor, { bigint: true }).mtimeNs, monitorMtime);

  const uid = String(process.getuid());
  const label = `dev.post.codex-doorbell.${AGENT}`;
  assert.deepEqual(calls(LAUNCHCTL_CALLS).filter((a) => a[0] === "bootout").at(-1), [
    "bootout",
    `gui/${uid}/${label}`,
  ]);
  assert.deepEqual(calls(LAUNCHCTL_CALLS).filter((a) => a[0] === "bootstrap").at(-1), [
    "bootstrap",
    `gui/${uid}`,
    plist,
  ]);
  assert.deepEqual(calls(POST_CALLS).at(-2), ["rooms"]);
  assert.deepEqual(calls(POST_CALLS).at(-1), ["watch", "--room", "ops", "--snapshot"]);
  assert.deepEqual(calls(HERDR_CALLS).at(-1), ["agent", "get", AGENT]);
});

test("a failed launchctl bootstrap is a failure", () => {
  const home = homeFor("bootstrap-fail");
  const result = run(
    ["--room", "ops", "--agent", AGENT],
    { ...OK, launchctlBootstrapExit: 1, launchctlBootstrapStderr: "Bootstrap failed: 5: Input/output error" },
    { POST_CODEX_DOORBELL_HOME: home }
  );
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /launchctl bootstrap failed/);
  assert.ok(
    fs.existsSync(plistPath(home)),
    "the plist must exist for bootstrap; a failed load leaves the files in place"
  );
});

test("a non-not-loaded bootout failure is a failure", () => {
  const home = homeFor("bootout-fail");
  const result = run(
    ["--room", "ops", "--agent", AGENT],
    { ...OK, launchctlBootoutExit: 1, launchctlBootoutStderr: "Boot-out failed: 5: Input/output error" },
    { POST_CODEX_DOORBELL_HOME: home }
  );
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /launchctl bootout failed/);
});

test("a custom absolute POST_MAIL_ROOT is pinned verbatim into the plist", () => {
  const home = homeFor("mail-root");
  const mailRoot = "/custom/mail&root";
  const result = run(["--room", "ops", "--agent", AGENT], OK, {
    POST_CODEX_DOORBELL_HOME: home,
    POST_MAIL_ROOT: mailRoot,
  });
  assert.equal(result.status, 0, result.stderr);

  const plist = fs.readFileSync(plistPath(home), "utf8");
  assert.ok(plist.includes("<key>POST_MAIL_ROOT</key>"), "mail root key must be pinned");
  assert.ok(
    plist.includes(`<string>${escapeXml(mailRoot)}</string>`),
    "the exact value must be persisted"
  );
  assert.ok(!plist.includes(mailRoot), "raw unescaped mail root must not appear");
});

test("a relative POST_MAIL_ROOT refuses install with nothing written", () => {
  const home = homeFor("mail-root-relative");
  const installDir = path.join(ROOT, "hooks-mail-root-relative");
  const monitor = path.join(installDir, "post-codex-notify-monitor.mjs");
  const postBefore = calls(POST_CALLS).length;
  const result = run(["--room", "ops", "--agent", AGENT], OK, {
    POST_CODEX_DOORBELL_HOME: home,
    POST_CODEX_DOORBELL_INSTALL_DIR: installDir,
    POST_MAIL_ROOT: "relative/mail/root",
  });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /POST_MAIL_ROOT must be an absolute path/);
  assert.ok(!fs.existsSync(plistPath(home)));
  assert.ok(!fs.existsSync(path.join(home, "Library", "LaunchAgents")));
  assert.ok(!fs.existsSync(monitor));
  assert.equal(
    calls(POST_CALLS).length,
    postBefore,
    "refusal happens before any preflight command"
  );
});

test("an explicitly empty POST_MAIL_ROOT refuses install with nothing written", () => {
  const home = homeFor("mail-root-empty");
  const installDir = path.join(ROOT, "hooks-mail-root-empty");
  const monitor = path.join(installDir, "post-codex-notify-monitor.mjs");
  const postBefore = calls(POST_CALLS).length;
  const result = run(["--room", "ops", "--agent", AGENT], OK, {
    POST_CODEX_DOORBELL_HOME: home,
    POST_CODEX_DOORBELL_INSTALL_DIR: installDir,
    POST_MAIL_ROOT: "",
  });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /POST_MAIL_ROOT must be an absolute path/);
  assert.ok(!fs.existsSync(plistPath(home)));
  assert.ok(!fs.existsSync(monitor));
  assert.equal(
    calls(POST_CALLS).length,
    postBefore,
    "refusal happens before any preflight command"
  );
});

test("an unset POST_MAIL_ROOT is omitted from the plist", () => {
  const home = homeFor("mail-root-unset");
  const result = run(["--room", "ops", "--agent", AGENT], OK, {
    POST_CODEX_DOORBELL_HOME: home,
  });
  assert.equal(result.status, 0, result.stderr);
  assert.ok(
    !fs.readFileSync(plistPath(home), "utf8").includes("POST_MAIL_ROOT"),
    "the key is omitted when POST_MAIL_ROOT is unset"
  );
});

test("uninstall removes only the exact agent files and never touches post or herdr", () => {
  const home = homeFor("uninstall");
  const seeded = run(["--room", "ops", "--agent", AGENT], OK, {
    POST_CODEX_DOORBELL_HOME: home,
  });
  assert.equal(seeded.status, 0, seeded.stderr);

  const plist = plistPath(home);
  const state = path.join(home, ".local", "state", "post-codex-doorbell", `${AGENT}.json`);
  const log = path.join(home, "Library", "Logs", `post-codex-doorbell-${AGENT}.log`);
  const errorLog = path.join(home, "Library", "Logs", `post-codex-doorbell-${AGENT}.error.log`);
  const monitor = path.join(INSTALL_DIR, "post-codex-notify-monitor.mjs");
  // Seed the files the launchd job would have produced, plus another agent's
  // files that must survive the scrub.
  fs.mkdirSync(path.dirname(state), { recursive: true });
  fs.writeFileSync(state, JSON.stringify({ seen: ["20260804-220000-abc123"] }));
  fs.mkdirSync(path.dirname(log), { recursive: true });
  fs.writeFileSync(log, "tick\n");
  fs.writeFileSync(errorLog, "err\n");
  const otherPlist = path.join(home, "Library", "LaunchAgents", "dev.post.codex-doorbell.other.plist");
  fs.writeFileSync(otherPlist, '<plist version="1.0"><dict/></plist>\n');
  const otherState = path.join(home, ".local", "state", "post-codex-doorbell", "other.json");
  fs.mkdirSync(path.dirname(otherState), { recursive: true });
  fs.writeFileSync(otherState, "{}");
  const otherLog = path.join(home, "Library", "Logs", "post-codex-doorbell-other.log");
  fs.writeFileSync(otherLog, "tick\n");

  const postBefore = calls(POST_CALLS).length;
  const herdrBefore = calls(HERDR_CALLS).length;
  const result = run(["--uninstall", "--agent", AGENT], {}, {
    POST_CODEX_DOORBELL_HOME: home,
  });
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /doorbell uninstalled/);

  assert.ok(!fs.existsSync(plist), "exact agent plist removed");
  assert.ok(!fs.existsSync(state), "per-agent state removed");
  assert.ok(!fs.existsSync(log), "exact agent log removed");
  assert.ok(!fs.existsSync(errorLog), "exact agent error log removed");
  assert.ok(fs.existsSync(otherPlist), "another agent's plist survives");
  assert.ok(fs.existsSync(otherState), "another agent's state survives");
  assert.ok(fs.existsSync(otherLog), "another agent's log survives");
  assert.ok(fs.existsSync(monitor), "shared monitor copy is left in place");

  assert.equal(calls(POST_CALLS).length, postBefore, "post must not run during uninstall");
  assert.equal(calls(HERDR_CALLS).length, herdrBefore, "herdr must not run during uninstall");
  const uid = String(process.getuid());
  assert.deepEqual(calls(LAUNCHCTL_CALLS).filter((a) => a[0] === "bootout").at(-1), [
    "bootout",
    `gui/${uid}/dev.post.codex-doorbell.${AGENT}`,
  ]);
});
