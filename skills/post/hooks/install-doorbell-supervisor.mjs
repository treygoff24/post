#!/usr/bin/env node
// Install, migrate to, and remove the per-host doorbell supervisor
// (doorbell-supervisor.mjs). Design: docs/plans/doorbell-supervisor-design.md,
// section "Migration (E8)".
//
//   node install-doorbell-supervisor.mjs [--dry-run]              install or refresh the supervisor
//   node install-doorbell-supervisor.mjs --list-legacy [--json]   the old per-agent timers and their settings
//   node install-doorbell-supervisor.mjs --migrate <agent> [--dry-run]
//   node install-doorbell-supervisor.mjs --migrate-all [--dry-run]
//   node install-doorbell-supervisor.mjs --uninstall [--dry-run]
//   node install-doorbell-supervisor.mjs --restore-legacy [--dry-run]
//
// Install copies doorbell-supervisor.mjs to ~/.local/share/post-doorbell/,
// writes the `post-doorbell` shim in ~/.local/bin, writes a launchd
// LaunchAgent (macOS, dev.post.doorbell-supervisor) or a systemd user service
// (Linux, post-doorbell-supervisor.service), starts it, and waits for the
// singleton lock and a healthy first tick (herdr and post both answered). An
// unhealthy start is stopped again and reported. A file already at
// ~/.local/bin/post-doorbell that is not this shim (the old Python daemon) is
// moved to post-doorbell.legacy-<sha8> with its hash in the receipt; the
// unused post-doorbell@.service template is left alone.
//
// Migration is one old timer at a time (post-codex-doorbell@<agent> on
// Linux, dev.post.codex-doorbell.<agent> on macOS): read its effective
// settings from the unit files, bind its herdr agent to a participant by exact
// session digest, write the equivalent prefs (enabled, the same channels,
// unfocused only, and a pane selection pinning the timer's pane), wait until
// the supervisor's health shows that subscription armed on that pane with a
// successful scan under those prefs, and only then disable that one timer. A
// timer the supervisor cannot bind or reproduce stays on its old mechanism.
// Every step is recorded in $POST_MAIL_ROOT/doorbell/install-receipt.json as it
// happens, so an interrupted run resumes where it stopped.
//
// Uninstall stops and removes only the supervisor (service, shim, installed
// copy) and prints the exact restoration commands; legacy units stay as they
// are. --restore-legacy stops the supervisor, then re-enables each timer the
// installer disabled whose unit files still have their recorded hashes, and
// moves the Python script back if the path still holds this installer's shim.
//
// Test and operator overrides (all optional):
//   POST_DOORBELL_HOME             replaces every "~" path derivation
//   POST_MAIL_ROOT                 the mail root (absolute); pinned into the service when set
//   POST_DOORBELL_INSTALL_DIR      where the supervisor copy goes
//   POST_DOORBELL_POST_BIN, POST_DOORBELL_HERDR_BIN, POST_DOORBELL_PYTHON_BIN
//   POST_DOORBELL_LAUNCHCTL_BIN, POST_DOORBELL_SYSTEMCTL_BIN
//   POST_DOORBELL_PLATFORM         darwin or linux (default process.platform)

import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import { stableNodePath } from "./stable-node-path.mjs";
import { liveness, resolvePaths, updatePrefs, loadPrefs, sha256, writeFileAtomic, stderrExcerpt } from "./doorbell-supervisor.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const SUPERVISOR_SOURCE = path.join(HERE, "doorbell-supervisor.mjs");
const LABEL = "dev.post.doorbell-supervisor";
const UNIT = "post-doorbell-supervisor.service";
const SHIM_MARKER = "# post-doorbell supervisor shim (install-doorbell-supervisor.mjs)";
const AGENT_NAME = /^[a-z][a-z0-9_-]{0,31}$/;
const CHANNEL_NAME = /^[A-Za-z0-9._-]{1,255}$/;
const NOT_LOADED = /no such process|could not find service|not loaded|not-found|not found|does not exist/i;

class Fail extends Error {
  constructor(message, code = 1) {
    super(message);
    this.code = code;
  }
}

function usageText() {
  return [
    "usage: node install-doorbell-supervisor.mjs [--dry-run] [--startup-timeout-seconds <n>]",
    "       node install-doorbell-supervisor.mjs --list-legacy [--json]",
    "       node install-doorbell-supervisor.mjs --migrate <agent> [--dry-run] [--health-timeout-seconds <n>]",
    "       node install-doorbell-supervisor.mjs --migrate-all [--dry-run] [--health-timeout-seconds <n>]",
    "       node install-doorbell-supervisor.mjs --uninstall [--dry-run]",
    "       node install-doorbell-supervisor.mjs --restore-legacy [--dry-run]",
  ].join("\n");
}

function parseArgs(argv) {
  const opts = { mode: "install", agents: [], dryRun: false, json: false, startupTimeout: 30, healthTimeout: 120 };
  const setMode = (mode) => {
    if (opts.mode !== "install" && opts.mode !== mode) throw new Fail(`--${mode} conflicts with --${opts.mode}`, 2);
    opts.mode = mode;
  };
  for (let index = 0; index < argv.length; index++) {
    const arg = argv[index];
    const value = () => {
      const next = argv[index + 1];
      if (next === undefined || next.startsWith("-")) throw new Fail(`${arg} requires a value`, 2);
      index += 1;
      return next;
    };
    const seconds = () => {
      const raw = value();
      if (!/^[1-9]\d{0,5}$/.test(raw)) throw new Fail(`${arg} must be a positive integer`, 2);
      return Number(raw);
    };
    if (arg === "--dry-run") opts.dryRun = true;
    else if (arg === "--json") opts.json = true;
    else if (arg === "--list-legacy") setMode("list-legacy");
    else if (arg === "--uninstall") setMode("uninstall");
    else if (arg === "--restore-legacy") setMode("restore-legacy");
    else if (arg === "--migrate-all") setMode("migrate-all");
    else if (arg === "--migrate") {
      setMode("migrate");
      const agent = value();
      if (!AGENT_NAME.test(agent)) throw new Fail(`invalid agent name: ${JSON.stringify(agent)}`, 2);
      opts.agents.push(agent);
    } else if (arg === "--startup-timeout-seconds") opts.startupTimeout = seconds();
    else if (arg === "--health-timeout-seconds") opts.healthTimeout = seconds();
    else throw new Fail(`unknown argument: ${arg}`, 2);
  }
  if (opts.json && opts.mode !== "list-legacy") throw new Fail("--json is only valid with --list-legacy", 2);
  return opts;
}

// ------------------------------------------------------------------ context

function isExecutable(file) {
  try {
    fs.accessSync(file, fs.constants.X_OK);
    return fs.statSync(file).isFile();
  } catch {
    return false;
  }
}

function resolveBin(override, preferred, name) {
  if (override) {
    if (!isExecutable(override)) throw new Fail(`cannot execute the ${name} binary at ${override}; fix it or unset the override`);
    return override;
  }
  if (preferred && isExecutable(preferred)) return preferred;
  for (const dir of String(process.env.PATH ?? "").split(":")) {
    if (!dir) continue;
    const candidate = path.join(dir, name);
    if (isExecutable(candidate)) return candidate;
  }
  throw new Fail(`could not find the ${name} executable; install it or point an override at it`);
}

function run(bin, args, { env, timeout = 15_000 } = {}) {
  return spawnSync(bin, args, { encoding: "utf8", env: env ?? process.env, timeout, stdio: ["ignore", "pipe", "pipe"] });
}

function detail(result) {
  return result.error?.message || stderrExcerpt(result.stderr) || `exit ${result.status}`;
}

function fileHash(file) {
  try {
    return sha256(fs.readFileSync(file));
  } catch {
    return null;
  }
}

function buildContext(opts) {
  const platform = process.env.POST_DOORBELL_PLATFORM || process.platform;
  if (platform !== "darwin" && platform !== "linux") throw new Fail(`unsupported platform ${platform}: launchd (macOS) and systemd (Linux) only`);
  const home = process.env.POST_DOORBELL_HOME || os.homedir();
  const rootPinned = process.env.POST_MAIL_ROOT;
  if (rootPinned !== undefined && !path.isAbsolute(rootPinned)) {
    throw new Fail(`POST_MAIL_ROOT must be an absolute path when set; got ${JSON.stringify(rootPinned)}`);
  }
  const installDir = process.env.POST_DOORBELL_INSTALL_DIR || path.join(home, ".local", "share", "post-doorbell");
  const binDir = path.join(home, ".local", "bin");
  const ctx = {
    opts,
    platform,
    home,
    rootPinned,
    dryRun: opts.dryRun,
    installDir,
    supervisorPath: path.join(installDir, "doorbell-supervisor.mjs"),
    binDir,
    shimPath: path.join(binDir, "post-doorbell"),
    uid: typeof process.getuid === "function" ? process.getuid() : 0,
    lines: [],
  };
  const paths = resolvePaths({ ...process.env, POST_DOORBELL_HOME: home });
  ctx.root = paths.root;
  ctx.doorbell = paths.doorbell;
  ctx.receiptFile = path.join(paths.doorbell, "install-receipt.json");
  if (platform === "darwin") {
    ctx.serviceFile = path.join(home, "Library", "LaunchAgents", `${LABEL}.plist`);
    ctx.logFile = path.join(home, "Library", "Logs", "post-doorbell-supervisor.log");
    ctx.legacyDir = path.join(home, "Library", "LaunchAgents");
  } else {
    ctx.serviceFile = path.join(home, ".config", "systemd", "user", UNIT);
    ctx.logFile = path.join(home, ".local", "state", "post-doorbell", "supervisor.log");
    ctx.legacyDir = path.join(home, ".config", "systemd", "user");
  }
  ctx.paths = paths;
  return ctx;
}

// Binaries for commands that touch the host. List and uninstall need only the
// service manager; install and migrate need everything.
function resolveTools(ctx, { full }) {
  ctx.managerBin =
    ctx.platform === "darwin"
      ? resolveBin(process.env.POST_DOORBELL_LAUNCHCTL_BIN, "/bin/launchctl", "launchctl")
      : resolveBin(process.env.POST_DOORBELL_SYSTEMCTL_BIN, null, "systemctl");
  ctx.pythonBin = resolveBin(process.env.POST_DOORBELL_PYTHON_BIN, null, "python3");
  ctx.paths = resolvePaths({ ...process.env, POST_DOORBELL_HOME: ctx.home, POST_DOORBELL_PYTHON_BIN: ctx.pythonBin });
  if (!full) return;
  ctx.nodeBin = stableNodePath();
  ctx.postBin = resolveBin(process.env.POST_DOORBELL_POST_BIN, path.join(ctx.home, ".local", "bin", "post"), "post");
  ctx.herdrBin = resolveBin(process.env.POST_DOORBELL_HERDR_BIN, path.join(ctx.home, ".local", "bin", "herdr"), "herdr");
  ctx.postEnv = { ...process.env, POST_MAIL_ROOT: ctx.root };
  delete ctx.postEnv.POST_PARTICIPANT;
}

function say(ctx, line) {
  process.stdout.write(`${ctx.dryRun ? "[dry-run] " : ""}${line}\n`);
}

// ------------------------------------------------------------------ receipt

function loadReceipt(ctx) {
  try {
    const parsed = JSON.parse(fs.readFileSync(ctx.receiptFile, "utf8"));
    if (parsed && typeof parsed === "object") {
      parsed.migrations ??= {};
      return parsed;
    }
  } catch (error) {
    if (error.code !== "ENOENT") throw new Fail(`cannot read ${ctx.receiptFile}: ${error.message}; fix or move it, then re-run`);
  }
  return { version: 1, platform: ctx.platform, created: new Date().toISOString(), supervisor: null, legacy_script: null, migrations: {} };
}

function saveReceipt(ctx, receipt) {
  if (ctx.dryRun) return;
  receipt.updated = new Date().toISOString();
  writeFileAtomic(ctx.receiptFile, `${JSON.stringify(receipt, null, 2)}\n`);
}

// Test seam for the resume guarantee: die right after a named migration state
// is durably recorded, as a kill or power loss would.
function crashPoint(state) {
  if (process.env.POST_DOORBELL_INSTALL_TEST_CRASH_AFTER === state) {
    process.stderr.write(`install-doorbell-supervisor: test crash after ${state}\n`);
    process.exit(97);
  }
}

// ------------------------------------------------------------------ service files

function escapeXml(value) {
  return String(value).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;").replace(/'/g, "&apos;");
}

function unescapeXml(value) {
  return String(value)
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&quot;/g, '"')
    .replace(/&apos;/g, "'")
    .replace(/&amp;/g, "&");
}

// systemd expands %-specifiers in ExecStart= and Environment=; %% is a literal %.
function unitQuote(value) {
  const text = String(value).replace(/%/g, "%%");
  if (!/[\s"'\\#;]/.test(text)) return text;
  return `"${text.replace(/([\\"])/g, "\\$1")}"`;
}

function serviceEnv(ctx) {
  const env = [
    ["HOME", ctx.home],
    ["PATH", [path.dirname(ctx.nodeBin), path.join(ctx.home, ".local", "bin"), "/usr/local/bin", "/usr/bin", "/bin"].join(":")],
    ["POST_DOORBELL_POST_BIN", ctx.postBin],
    ["POST_DOORBELL_HERDR_BIN", ctx.herdrBin],
    ["POST_DOORBELL_PYTHON_BIN", ctx.pythonBin],
  ];
  if (process.env.POST_DOORBELL_HOME) env.push(["POST_DOORBELL_HOME", ctx.home]);
  if (ctx.rootPinned !== undefined) env.push(["POST_MAIL_ROOT", ctx.rootPinned]);
  return env;
}

function plistContent(ctx) {
  const x = escapeXml;
  return [
    '<?xml version="1.0" encoding="UTF-8"?>',
    '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">',
    '<plist version="1.0">',
    "<dict>",
    "  <key>Label</key>",
    `  <string>${LABEL}</string>`,
    "  <key>ProgramArguments</key>",
    "  <array>",
    `    <string>${x(ctx.nodeBin)}</string>`,
    `    <string>${x(ctx.supervisorPath)}</string>`,
    "    <string>run</string>",
    "  </array>",
    "  <key>EnvironmentVariables</key>",
    "  <dict>",
    ...serviceEnv(ctx).map(([key, value]) => `    <key>${x(key)}</key><string>${x(value)}</string>`),
    "  </dict>",
    "  <key>RunAtLoad</key>",
    "  <true/>",
    "  <key>KeepAlive</key>",
    "  <true/>",
    "  <key>ThrottleInterval</key>",
    "  <integer>10</integer>",
    "  <key>StandardOutPath</key>",
    `  <string>${x(ctx.logFile)}</string>`,
    "  <key>StandardErrorPath</key>",
    `  <string>${x(ctx.logFile)}</string>`,
    "</dict>",
    "</plist>",
    "",
  ].join("\n");
}

function unitContent(ctx) {
  return [
    "[Unit]",
    "Description=Post doorbell supervisor (one per host)",
    "",
    "[Service]",
    "Type=simple",
    `ExecStart=${unitQuote(ctx.nodeBin)} ${unitQuote(ctx.supervisorPath)} run`,
    ...serviceEnv(ctx).map(([key, value]) => `Environment=${unitQuote(`${key}=${value}`)}`),
    "Restart=always",
    "RestartSec=10",
    `StandardOutput=${unitQuote(`append:${ctx.logFile}`)}`,
    `StandardError=${unitQuote(`append:${ctx.logFile}`)}`,
    "",
    "[Install]",
    "WantedBy=default.target",
    "",
  ].join("\n");
}

function shimContent(ctx) {
  const q = (value) => `'${String(value).replace(/'/g, "'\\''")}'`;
  const lines = ["#!/bin/sh", SHIM_MARKER];
  // Defaults only: an agent's own environment still wins.
  if (ctx.rootPinned !== undefined) lines.push(`: "\${POST_MAIL_ROOT:=${ctx.rootPinned.replace(/["\\$`]/g, "\\$&")}}"`, "export POST_MAIL_ROOT");
  if (process.env.POST_DOORBELL_HOME) lines.push(`: "\${POST_DOORBELL_HOME:=${ctx.home.replace(/["\\$`]/g, "\\$&")}}"`, "export POST_DOORBELL_HOME");
  lines.push(`exec ${q(ctx.nodeBin)} ${q(ctx.supervisorPath)} "$@"`, "");
  return lines.join("\n");
}

// ------------------------------------------------------------------ service manager

function managerCall(ctx, args, { mutating = true, allowNotLoaded = false } = {}) {
  if (mutating && ctx.dryRun) {
    say(ctx, `would run: ${path.basename(ctx.managerBin)} ${args.join(" ")}`);
    return { status: 0, stdout: "", stderr: "" };
  }
  const result = run(ctx.managerBin, args);
  if ((result.error || result.status !== 0) && !(allowNotLoaded && NOT_LOADED.test(`${result.stderr}${result.stdout}`))) {
    throw new Fail(`${path.basename(ctx.managerBin)} ${args.join(" ")} failed: ${detail(result)}`);
  }
  return result;
}

function domain(ctx) {
  return `gui/${ctx.uid}`;
}

function startSupervisor(ctx) {
  if (ctx.platform === "darwin") {
    managerCall(ctx, ["bootout", `${domain(ctx)}/${LABEL}`], { allowNotLoaded: true });
    managerCall(ctx, ["enable", `${domain(ctx)}/${LABEL}`]);
    managerCall(ctx, ["bootstrap", domain(ctx), ctx.serviceFile]);
  } else {
    managerCall(ctx, ["--user", "daemon-reload"]);
    managerCall(ctx, ["--user", "enable", UNIT]);
    managerCall(ctx, ["--user", "restart", UNIT]);
  }
}

function stopSupervisor(ctx) {
  if (ctx.platform === "darwin") managerCall(ctx, ["bootout", `${domain(ctx)}/${LABEL}`], { allowNotLoaded: true });
  else managerCall(ctx, ["--user", "disable", "--now", UNIT], { allowNotLoaded: true });
}

// ------------------------------------------------------------------ legacy timers

function tokenize(text) {
  const tokens = [];
  let current = "";
  let quote = null;
  let started = false;
  for (let index = 0; index < text.length; index++) {
    const char = text[index];
    if (quote) {
      if (char === "\\" && quote === '"' && index + 1 < text.length) {
        current += text[++index];
      } else if (char === quote) quote = null;
      else current += char;
    } else if (char === '"' || char === "'") {
      quote = char;
      started = true;
    } else if (/\s/.test(char)) {
      if (started) tokens.push(current);
      current = "";
      started = false;
    } else {
      current += char;
      started = true;
    }
  }
  if (quote) return null;
  if (started) tokens.push(current);
  return tokens;
}

// The keys a systemd unit (plus drop-ins, in order) sets that matter here.
function parseSystemdFiles(files) {
  const env = {};
  let execStart = null;
  let environmentFile = false;
  for (const file of files) {
    let text;
    try {
      text = fs.readFileSync(file, "utf8");
    } catch {
      continue;
    }
    for (const raw of text.split("\n")) {
      const line = raw.trim();
      if (!line || line.startsWith("#") || line.startsWith(";") || line.startsWith("[")) continue;
      const eq = line.indexOf("=");
      if (eq < 0) continue;
      const key = line.slice(0, eq).trim();
      const value = line.slice(eq + 1).trim();
      if (key === "Environment") {
        if (value === "") {
          for (const name of Object.keys(env)) delete env[name];
          continue;
        }
        const tokens = tokenize(value);
        if (tokens === null) return { error: `unparseable Environment= in ${file}` };
        for (const token of tokens) {
          const split = token.indexOf("=");
          if (split > 0) env[token.slice(0, split)] = token.slice(split + 1);
        }
      } else if (key === "EnvironmentFile") environmentFile = true;
      else if (key === "ExecStart") {
        if (value === "") execStart = null;
        else {
          const tokens = tokenize(value.replace(/^[-@:+!]+/, ""));
          if (tokens === null) return { error: `unparseable ExecStart= in ${file}` };
          execStart = tokens;
        }
      }
    }
  }
  return { env, execStart, environmentFile };
}

function parsePlist(file) {
  let text;
  try {
    text = fs.readFileSync(file, "utf8");
  } catch (error) {
    return { error: `cannot read ${file}: ${error.code}` };
  }
  const envBlock = /<key>EnvironmentVariables<\/key>\s*<dict>([\s\S]*?)<\/dict>/.exec(text);
  const argsBlock = /<key>ProgramArguments<\/key>\s*<array>([\s\S]*?)<\/array>/.exec(text);
  if (!argsBlock) return { error: `no ProgramArguments in ${file}` };
  const env = {};
  if (envBlock) {
    for (const match of envBlock[1].matchAll(/<key>([\s\S]*?)<\/key>\s*<string>([\s\S]*?)<\/string>/g)) {
      env[unescapeXml(match[1])] = unescapeXml(match[2]);
    }
  }
  const execStart = [...argsBlock[1].matchAll(/<string>([\s\S]*?)<\/string>/g)].map((match) => unescapeXml(match[1]));
  return { env, execStart, environmentFile: false };
}

// Every old timer on this host with the settings its unit files give it.
function legacyTimers(ctx) {
  let entries;
  try {
    entries = fs.readdirSync(ctx.legacyDir);
  } catch {
    return [];
  }
  const timers = [];
  for (const name of entries.sort()) {
    let agent;
    let files;
    let parsed;
    let unit;
    if (ctx.platform === "darwin") {
      const match = /^dev\.post\.codex-doorbell\.([a-z][a-z0-9_-]{0,31})\.plist$/.exec(name);
      if (!match) continue;
      agent = match[1];
      unit = `dev.post.codex-doorbell.${agent}`;
      files = [path.join(ctx.legacyDir, name)];
      parsed = parsePlist(files[0]);
    } else {
      const match = /^post-codex-doorbell@([a-z][a-z0-9_-]{0,31})\.service$/.exec(name);
      if (!match) continue;
      agent = match[1];
      unit = `post-codex-doorbell@${agent}.timer`;
      const service = path.join(ctx.legacyDir, name);
      const dropIns = [];
      for (const dir of [path.join(ctx.legacyDir, "post-codex-doorbell@.service.d"), path.join(ctx.legacyDir, `${name}.d`)]) {
        try {
          for (const conf of fs.readdirSync(dir).filter((f) => f.endsWith(".conf")).sort()) dropIns.push(path.join(dir, conf));
        } catch {
          // No drop-ins.
        }
      }
      files = [service, path.join(ctx.legacyDir, `post-codex-doorbell@${agent}.timer`), ...dropIns];
      parsed = parseSystemdFiles([service, ...dropIns]);
    }
    const timer = { agent, unit, files, hashes: Object.fromEntries(files.map((file) => [file, fileHash(file)])) };
    if (parsed.error) {
      timer.problem = parsed.error;
      timers.push(timer);
      continue;
    }
    const env = parsed.env;
    const monitorIndex = (parsed.execStart ?? []).findIndex((arg) => arg.endsWith("codex-notify-monitor.mjs"));
    const rooms = monitorIndex >= 0 ? parsed.execStart.slice(monitorIndex + 1) : [];
    const channels = String(env.POST_CODEX_NOTIFY_CHANNELS ?? "")
      .split(",")
      .map((part) => part.trim())
      .filter(Boolean);
    timer.settings = {
      herdr_agent: env.POST_CODEX_NOTIFY_HERDR_AGENT || null,
      rooms,
      channels,
      participant: env.POST_PARTICIPANT || null,
      mail_root: env.POST_MAIL_ROOT || null,
      sink: env.POST_CODEX_NOTIFY_HERDR_AGENT ? "herdr" : "cmux",
      focused: false,
      reasons: channels.length ? ["mail", `channel:${channels.join(",")}`] : ["mail"],
    };
    if (parsed.environmentFile) timer.problem = "the unit reads an EnvironmentFile, whose settings this installer cannot see";
    else if (timer.settings.sink !== "herdr") timer.problem = "desktop-only (cmux) timer: no herdr target to bind";
    else if (timer.settings.herdr_agent !== agent) timer.problem = "the unit's herdr agent differs from its name";
    else if (rooms.length !== 1) timer.problem = `the unit watches ${rooms.length} rooms; the supervisor follows one participant`;
    else if (channels.some((channel) => !CHANNEL_NAME.test(channel))) timer.problem = "a channel name outside the doorbell alphabet";
    else if (timer.settings.mail_root && path.resolve(timer.settings.mail_root) !== path.resolve(ctx.root)) {
      timer.problem = `the unit uses mail root ${timer.settings.mail_root}, not ${ctx.root}`;
    }
    timers.push(timer);
  }
  return timers;
}

function legacyEnabled(ctx, timer) {
  if (ctx.platform === "darwin") {
    const result = run(ctx.managerBin, ["print", `${domain(ctx)}/${timer.unit}`]);
    return result.status === 0;
  }
  const result = run(ctx.managerBin, ["--user", "is-enabled", timer.unit]);
  return String(result.stdout ?? "").trim() === "enabled";
}

function disableLegacy(ctx, timer) {
  if (ctx.platform === "darwin") {
    managerCall(ctx, ["bootout", `${domain(ctx)}/${timer.unit}`], { allowNotLoaded: true });
    managerCall(ctx, ["disable", `${domain(ctx)}/${timer.unit}`]);
  } else {
    managerCall(ctx, ["--user", "disable", "--now", timer.unit]);
  }
}

function restoreCommands(ctx, entry) {
  if (ctx.platform === "darwin") {
    return [`launchctl enable ${domain(ctx)}/${entry.unit}`, `launchctl bootstrap ${domain(ctx)} ${entry.files[0]}`];
  }
  return [`systemctl --user enable --now ${entry.unit}`];
}

function enableLegacy(ctx, entry) {
  if (ctx.platform === "darwin") {
    managerCall(ctx, ["enable", `${domain(ctx)}/${entry.unit}`]);
    managerCall(ctx, ["bootstrap", domain(ctx), entry.files[0]]);
  } else {
    managerCall(ctx, ["--user", "enable", "--now", entry.unit]);
  }
}

// ------------------------------------------------------------------ health

function readJson(file) {
  try {
    return JSON.parse(fs.readFileSync(file, "utf8"));
  } catch {
    return undefined;
  }
}

function logTail(ctx) {
  try {
    const text = fs.readFileSync(ctx.logFile, "utf8");
    return stderrExcerpt(text.slice(-600));
  } catch {
    return "";
  }
}

function sleepMs(ms) {
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
}

// Healthy first tick: the singleton lock is held, the heartbeat is fresh, and
// a health file written by this start shows herdr and post both answering.
function supervisorHealth(ctx, since) {
  const live = liveness(ctx.paths);
  if (live.state !== "running") return { ok: false, why: `supervisor ${live.state} (lock ${live.lock})` };
  const health = readJson(path.join(ctx.doorbell, "health.json"));
  if (!health || Date.parse(health.started_at) < since - 1000) return { ok: false, why: "no health from this start yet" };
  if (health.herdr_ok !== true) return { ok: false, why: "herdr agent list is failing" };
  if (health.post_ok !== true) return { ok: false, why: "post participant list is failing" };
  if (!health.last_discovery_at) return { ok: false, why: "no discovery yet" };
  // Every subscription armed at start must have finished its first scan, and
  // a first scan that failed is a failed start.
  for (const binding of health.bindings ?? []) {
    if (!binding.armed) continue;
    if (!binding.last_success_scan_at && binding.consecutive_failures > 0) {
      return { ok: false, fatal: true, why: `the first scan for ${binding.participant} failed at ${binding.last_error?.stage ?? "an unknown stage"}` };
    }
    if (!binding.last_outcome) return { ok: false, why: `the first scan for ${binding.participant} has not finished` };
  }
  return { ok: true, health };
}

function waitFor(check, timeoutSeconds) {
  const deadline = Date.now() + timeoutSeconds * 1000;
  let last;
  for (;;) {
    last = check();
    if (last.ok || last.fatal || Date.now() >= deadline) return last;
    sleepMs(250);
  }
}

// ------------------------------------------------------------------ install

function preflight(ctx, receipt) {
  const version = run(ctx.postBin, ["version", "--json"], { env: ctx.postEnv });
  if (version.error || version.status !== 0) throw new Fail(`preflight failed: \`post version --json\`: ${detail(version)}`);
  const herdr = run(ctx.herdrBin, ["--version"]);
  if (herdr.error || herdr.status !== 0) throw new Fail(`preflight failed: \`herdr --version\`: ${detail(herdr)}`);
  const python = run(ctx.pythonBin, ["-c", "import fcntl"]);
  if (python.error || python.status !== 0) throw new Fail(`preflight failed: ${ctx.pythonBin} cannot import fcntl: ${detail(python)}`);
  const live = liveness(ctx.paths);
  if (live.lock === "held" && !receipt.supervisor?.service_file) {
    throw new Fail(`another doorbell supervisor holds the lock (pid ${live.pid ?? "unknown"}) and this installer did not start it; stop it first, then re-run`);
  }
}

// The name collision: the old Python daemon at ~/.local/bin/post-doorbell.
function moveLegacyScript(ctx, receipt) {
  // A kill between the rename and the "moved" write leaves the receipt at
  // "moving" with the script already aside. Heal it when the aside copy still
  // has the recorded hash and the original path is empty or holds our shim,
  // so --restore-legacy can move it back.
  const prior = receipt.legacy_script;
  if (prior?.state === "moving" && prior.path === ctx.shimPath) {
    const asideHash = (() => {
      try {
        return sha256(fs.readFileSync(prior.moved_to));
      } catch {
        try {
          return sha256(fs.readlinkSync(prior.moved_to));
        } catch {
          return null;
        }
      }
    })();
    let occupant = null;
    try {
      occupant = fs.readFileSync(ctx.shimPath, "utf8");
    } catch {
      occupant = fs.existsSync(ctx.shimPath) ? "" : null;
    }
    if (asideHash === prior.sha256 && (occupant === null || occupant.includes(SHIM_MARKER))) {
      say(ctx, `${prior.moved_to}: an interrupted move finished; recording it as moved`);
      prior.state = "moved";
      saveReceipt(ctx, receipt);
      return;
    }
  }
  let stat;
  try {
    stat = fs.lstatSync(ctx.shimPath);
  } catch (error) {
    if (error.code === "ENOENT") return;
    throw error;
  }
  let content = null;
  try {
    content = fs.readFileSync(ctx.shimPath);
  } catch {
    content = null;
  }
  if (content && content.toString("utf8").includes(SHIM_MARKER)) return;
  if (!stat.isFile() && !stat.isSymbolicLink()) throw new Fail(`${ctx.shimPath} is not a file; move it aside, then re-run`);
  const hash = content ? sha256(content) : sha256(fs.readlinkSync(ctx.shimPath));
  const movedTo = `${ctx.shimPath}.legacy-${hash.slice(0, 8)}`;
  say(ctx, `moving the existing ${ctx.shimPath} aside to ${movedTo}`);
  receipt.legacy_script = { path: ctx.shimPath, sha256: hash, moved_to: movedTo, state: "moving", at: new Date().toISOString() };
  saveReceipt(ctx, receipt);
  if (!ctx.dryRun) {
    if (fs.existsSync(movedTo)) throw new Fail(`${movedTo} already exists; resolve it by hand, then re-run`);
    fs.renameSync(ctx.shimPath, movedTo);
  }
  receipt.legacy_script.state = "moved";
  saveReceipt(ctx, receipt);
}

function writeIfChanged(ctx, file, content, mode) {
  let current = null;
  try {
    current = fs.readFileSync(file, "utf8");
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
  }
  if (current === content) {
    if (!ctx.dryRun && (fs.statSync(file).mode & 0o777) !== mode) fs.chmodSync(file, mode);
    return false;
  }
  say(ctx, `${current === null ? "writing" : "updating"} ${file}`);
  if (!ctx.dryRun) {
    writeFileAtomic(file, content, mode);
    fs.chmodSync(file, mode);
  }
  return true;
}

function install(ctx, receipt) {
  preflight(ctx, receipt);
  moveLegacyScript(ctx, receipt);
  const source = fs.readFileSync(SUPERVISOR_SOURCE, "utf8");
  writeIfChanged(ctx, ctx.supervisorPath, source, 0o755);
  writeIfChanged(ctx, ctx.shimPath, shimContent(ctx), 0o755);
  if (!ctx.dryRun) fs.mkdirSync(path.dirname(ctx.logFile), { recursive: true, mode: 0o700 });
  writeIfChanged(ctx, ctx.serviceFile, ctx.platform === "darwin" ? plistContent(ctx) : unitContent(ctx), 0o644);
  receipt.supervisor = {
    ...(receipt.supervisor ?? {}),
    service: ctx.platform === "darwin" ? LABEL : UNIT,
    service_file: ctx.serviceFile,
    supervisor_path: ctx.supervisorPath,
    supervisor_sha256: sha256(source),
    shim_path: ctx.shimPath,
    shim_sha256: sha256(shimContent(ctx)),
    log_file: ctx.logFile,
    state: "starting",
    started_at: new Date().toISOString(),
  };
  saveReceipt(ctx, receipt);
  const since = Date.now();
  startSupervisor(ctx);
  if (ctx.dryRun) {
    say(ctx, "would wait for the singleton lock and a healthy first tick");
    return;
  }
  const result = waitFor(() => supervisorHealth(ctx, since), ctx.opts.startupTimeout);
  if (!result.ok) {
    stopSupervisor(ctx);
    receipt.supervisor.state = "failed_start";
    receipt.supervisor.failure = result.why;
    saveReceipt(ctx, receipt);
    const tail = logTail(ctx);
    throw new Fail(`the supervisor did not become healthy: ${result.why}; it was stopped again.${tail ? ` Log tail: ${tail}` : ""}`);
  }
  receipt.supervisor.state = "healthy";
  delete receipt.supervisor.failure;
  saveReceipt(ctx, receipt);
  say(ctx, `supervisor healthy (pid ${result.health.pid}); ${result.health.bindings.length} binding(s) discovered, ${result.health.bindings.filter((b) => b.armed).length} armed`);
}

// ------------------------------------------------------------------ migrate

function participantList(ctx) {
  const result = run(ctx.postBin, ["participant", "list", "--json"], { env: ctx.postEnv });
  if (result.error || result.status !== 0) throw new Fail(`\`post participant list --json\` failed: ${detail(result)}`);
  try {
    const parsed = JSON.parse(result.stdout);
    if (Array.isArray(parsed?.participants)) return parsed.participants;
  } catch {
    // Reported below.
  }
  throw new Fail("`post participant list --json` printed malformed output");
}

// The timer's herdr agent, by name, to a participant by exact session digest.
function bindTimer(ctx, timer, participants) {
  const got = run(ctx.herdrBin, ["agent", "get", timer.settings.herdr_agent]);
  if (got.error || got.status !== 0) return { problem: `\`herdr agent get ${timer.settings.herdr_agent}\` failed: ${detail(got)}` };
  let agent;
  try {
    agent = JSON.parse(got.stdout)?.result?.agent;
  } catch {
    agent = undefined;
  }
  const session = agent?.agent_session;
  if (agent?.name !== undefined && agent.name !== timer.settings.herdr_agent) {
    return { problem: `herdr answered for ${agent.name}, not ${timer.settings.herdr_agent}` };
  }
  if (typeof agent?.pane_id !== "string" || session?.kind !== "id" || typeof session.value !== "string") {
    return { problem: `herdr agent ${timer.settings.herdr_agent} carries no identifiable conversation` };
  }
  const digest = sha256(session.value);
  const matches = participants.filter((row) => row.conversation_key_digest === digest);
  if (matches.length !== 1) return { problem: `herdr agent ${timer.settings.herdr_agent}'s conversation matches ${matches.length} participants` };
  const participant = matches[0];
  if (participant.ended_at) return { problem: `participant ${participant.id} has ended` };
  if (timer.settings.participant && timer.settings.participant !== participant.id) {
    return { problem: `the unit pins participant ${timer.settings.participant}, but the pane carries ${participant.id}` };
  }
  if (participant.workspace !== timer.settings.rooms[0]) {
    return { problem: `the unit watches room ${timer.settings.rooms[0]}, but ${participant.id} is bound to ${participant.workspace ?? "no workspace"}` };
  }
  return { participant: participant.id, pane: agent.pane_id, digest };
}

function subscriptionHealth(ctx, entry) {
  const live = liveness(ctx.paths);
  if (live.state !== "running") return { ok: false, why: `supervisor ${live.state}` };
  const health = readJson(path.join(ctx.doorbell, "health.json"));
  const binding = health?.bindings?.find((row) => row.participant === entry.participant);
  if (!binding) return { ok: false, why: `the supervisor does not see ${entry.participant} on any pane` };
  if (!binding.armed) return { ok: false, why: `${entry.participant} is ${binding.state} and unarmed` };
  if (binding.generation?.pane !== entry.pane) return { ok: false, why: `${entry.participant} is bound to ${binding.generation?.pane}, not ${entry.pane}` };
  const same = [...(binding.scan_channels ?? [])].sort().join(",") === [...entry.settings.channels].sort().join(",");
  if (!binding.last_success_scan_at || binding.scan_prefs_version < entry.prefs_version || !same) {
    const error = binding.last_error ? `; last error ${binding.last_error.stage}` : "";
    return { ok: false, why: `no successful scan under prefs version ${entry.prefs_version} yet${error}` };
  }
  return { ok: true };
}

function migrateOne(ctx, receipt, timer, participants) {
  const key = timer.unit;
  let entry = receipt.migrations[key];
  if (entry?.state === "migrated" || entry?.state === "restored") {
    say(ctx, `${key}: already ${entry.state}`);
    return entry.state === "migrated";
  }
  if (timer.problem) {
    say(ctx, `${key}: not migrated: ${timer.problem}; it stays on its timer`);
    return false;
  }
  if (entry && Object.entries(entry.hashes).some(([file, hash]) => fileHash(file) !== hash)) {
    say(ctx, `${key}: not migrated: its unit files changed since the plan was recorded; review them, then remove its receipt entry to re-plan`);
    return false;
  }
  const bound = bindTimer(ctx, timer, participants);
  if (bound.problem) {
    say(ctx, `${key}: not migrated: ${bound.problem}; it stays on its timer`);
    return false;
  }
  const enabledBefore = entry?.enabled_before ?? legacyEnabled(ctx, timer);
  entry = {
    ...(entry ?? {}),
    unit: key,
    agent: timer.agent,
    files: timer.files,
    hashes: entry?.hashes ?? timer.hashes,
    settings: timer.settings,
    participant: bound.participant,
    pane: bound.pane,
    enabled_before: enabledBefore,
    state: entry?.state ?? "planned",
  };
  if (entry.state === "planned") entry.planned_at = new Date().toISOString();
  receipt.migrations[key] = entry;
  saveReceipt(ctx, receipt);

  // The equivalent subscription: enabled, the same channels, unfocused only,
  // and the timer's own pane selected. Rewritten only when it differs.
  const current = loadPrefs(ctx.paths, bound.participant);
  const wanted = { enabled: true, focused: false, channels: [...timer.settings.channels].sort(), selection: { pane: bound.pane, digest: bound.digest } };
  const matches =
    current.enabled && !current.focused && current.channels.join(",") === wanted.channels.join(",") &&
    current.selection?.pane === wanted.selection.pane && current.selection?.digest === wanted.selection.digest;
  let prefsVersion = current.version;
  if (!matches) {
    say(ctx, `${key}: arming ${bound.participant} on ${bound.pane} with channels [${wanted.channels.join(", ")}]`);
    if (!ctx.dryRun) {
      prefsVersion = updatePrefs(ctx.paths, bound.participant, (prefs) => {
        Object.assign(prefs, wanted, { desktop: prefs.desktop });
        prefs.source = { migrated_from: key };
      }).version;
    }
  }
  entry.prefs_version = prefsVersion;
  entry.state = "subscription_created";
  saveReceipt(ctx, receipt);
  crashPoint("subscription_created");
  if (ctx.dryRun) {
    say(ctx, `${key}: would wait for a healthy subscription, then disable ${key}`);
    return true;
  }

  const healthy = waitFor(() => subscriptionHealth(ctx, entry), ctx.opts.healthTimeout);
  if (!healthy.ok) {
    // A resume after a kill that followed the disable: the timer is already
    // off, so saying it "stays on" would promise coverage that is not there.
    if (enabledBefore && !legacyEnabled(ctx, timer)) {
      say(ctx, `${key}: not migrated yet: ${healthy.why}. The timer is already off (an earlier run disabled it), so only the supervisor covers it, and that is not proven healthy. Re-run --migrate ${timer.agent} to finish, or --restore-legacy to turn the timer back on.`);
    } else {
      say(ctx, `${key}: not migrated yet: ${healthy.why}. The timer stays on (both may ring; at-least-once). Re-run --migrate ${timer.agent} to finish.`);
    }
    return false;
  }
  if (enabledBefore) disableLegacy(ctx, timer);
  crashPoint("timer_disabled");
  entry.state = "migrated";
  entry.migrated_at = new Date().toISOString();
  entry.left_state = enabledBefore ? "disabled by this installer" : "already disabled; untouched";
  entry.hashes = Object.fromEntries(timer.files.map((file) => [file, fileHash(file)]));
  saveReceipt(ctx, receipt);
  say(ctx, `${key}: migrated to the supervisor (${bound.participant} on ${bound.pane}); timer ${enabledBefore ? "disabled" : "was already off"}`);
  return true;
}

// ------------------------------------------------------------------ uninstall

function removeSupervisor(ctx, receipt) {
  stopSupervisor(ctx);
  const removals = [[ctx.serviceFile, null]];
  if (receipt.supervisor?.shim_sha256) removals.push([ctx.shimPath, receipt.supervisor.shim_sha256]);
  if (receipt.supervisor?.supervisor_sha256) removals.push([ctx.supervisorPath, receipt.supervisor.supervisor_sha256]);
  for (const [file, hash] of removals) {
    const actual = fileHash(file);
    if (actual === null) continue;
    if (hash && actual !== hash) {
      say(ctx, `leaving ${file}: it changed since install`);
      continue;
    }
    say(ctx, `removing ${file}`);
    if (!ctx.dryRun) fs.unlinkSync(file);
  }
  if (ctx.platform === "linux") managerCall(ctx, ["--user", "daemon-reload"]);
  if (receipt.supervisor) {
    receipt.supervisor.state = "uninstalled";
    receipt.supervisor.uninstalled_at = new Date().toISOString();
    saveReceipt(ctx, receipt);
  }
}

function restorationText(ctx, receipt) {
  const lines = [];
  const migrated = Object.values(receipt.migrations).filter((entry) => entry.state === "migrated" && entry.enabled_before);
  if (migrated.length === 0 && receipt.legacy_script?.state !== "moved") return "nothing to restore: no legacy timer was disabled and no script was moved";
  lines.push(`restore everything this installer changed: node ${path.join(HERE, "install-doorbell-supervisor.mjs")} --restore-legacy`);
  for (const entry of migrated) for (const command of restoreCommands(ctx, entry)) lines.push(`  or by hand: ${command}`);
  if (receipt.legacy_script?.state === "moved") lines.push(`  and: mv ${receipt.legacy_script.moved_to} ${receipt.legacy_script.path}`);
  return lines.join("\n");
}

function restoreLegacy(ctx, receipt) {
  removeSupervisor(ctx, receipt);
  let refused = 0;
  for (const entry of Object.values(receipt.migrations)) {
    if (entry.state !== "migrated") continue;
    if (!entry.enabled_before) {
      say(ctx, `${entry.unit}: was already disabled before migration; left as is`);
      continue;
    }
    const changed = Object.entries(entry.hashes).filter(([file, hash]) => fileHash(file) !== hash).map(([file]) => file);
    if (changed.length) {
      refused += 1;
      say(ctx, `${entry.unit}: NOT restored: ${changed.join(", ")} changed after install. Review, then: ${restoreCommands(ctx, entry).join(" && ")}`);
      continue;
    }
    say(ctx, `${entry.unit}: re-enabling`);
    enableLegacy(ctx, entry);
    entry.state = "restored";
    entry.restored_at = new Date().toISOString();
    saveReceipt(ctx, receipt);
  }
  const legacy = receipt.legacy_script;
  if (legacy?.state === "moved") {
    const occupant = fileHash(legacy.path);
    const ours = receipt.supervisor?.shim_sha256;
    if (occupant !== null && occupant !== ours) {
      refused += 1;
      say(ctx, `${legacy.path}: NOT restored: the path holds a file this installer did not write`);
    } else if (fileHash(legacy.moved_to) !== legacy.sha256) {
      refused += 1;
      say(ctx, `${legacy.moved_to}: NOT restored: it no longer matches the recorded hash`);
    } else {
      say(ctx, `moving ${legacy.moved_to} back to ${legacy.path}`);
      if (!ctx.dryRun) {
        if (occupant !== null) fs.unlinkSync(legacy.path);
        fs.renameSync(legacy.moved_to, legacy.path);
      }
      legacy.state = "restored";
      saveReceipt(ctx, receipt);
    }
  }
  if (refused) throw new Fail(`${refused} item(s) were not restored; see above`);
}

// ------------------------------------------------------------------ main

function listLegacy(ctx, receipt) {
  const timers = legacyTimers(ctx).map((timer) => ({
    unit: timer.unit,
    agent: timer.agent,
    enabled: legacyEnabled(ctx, timer),
    settings: timer.settings ?? null,
    migratable: !timer.problem,
    problem: timer.problem ?? null,
    receipt_state: receipt.migrations[timer.unit]?.state ?? null,
  }));
  if (ctx.opts.json) {
    process.stdout.write(`${JSON.stringify({ ok: true, platform: ctx.platform, timers }, null, 2)}\n`);
    return;
  }
  if (!timers.length) say(ctx, "no legacy doorbell timers on this host");
  for (const timer of timers) {
    const settings = timer.settings ? `room ${timer.settings.rooms.join(",")}; channels [${timer.settings.channels.join(", ")}]` : "unreadable settings";
    say(ctx, `${timer.unit}: ${timer.enabled ? "enabled" : "disabled"}; ${settings}; ${timer.migratable ? "migratable" : `not migratable: ${timer.problem}`}${timer.receipt_state ? `; receipt: ${timer.receipt_state}` : ""}`);
  }
}

function main(argv) {
  if (argv.includes("-h") || argv.includes("--help")) {
    process.stdout.write(`${usageText()}\n`);
    return 0;
  }
  const opts = parseArgs(argv);
  const ctx = buildContext(opts);
  const receipt = loadReceipt(ctx);
  if (opts.mode === "list-legacy") {
    resolveTools(ctx, { full: false });
    listLegacy(ctx, receipt);
    return 0;
  }
  if (opts.mode === "uninstall") {
    resolveTools(ctx, { full: false });
    removeSupervisor(ctx, receipt);
    say(ctx, "supervisor stopped and removed; legacy units untouched; prefs, state, and this receipt kept");
    process.stdout.write(`${restorationText(ctx, receipt)}\n`);
    return 0;
  }
  if (opts.mode === "restore-legacy") {
    resolveTools(ctx, { full: false });
    restoreLegacy(ctx, receipt);
    say(ctx, "legacy doorbells restored");
    return 0;
  }
  resolveTools(ctx, { full: true });
  if (opts.mode === "install") {
    install(ctx, receipt);
    return 0;
  }
  // Migration needs a healthy supervisor first; an idempotent install
  // restarts it with the current code.
  const live = liveness(ctx.paths);
  if (!(live.state === "running" && receipt.supervisor?.state === "healthy" && fileHash(ctx.supervisorPath) === sha256(fs.readFileSync(SUPERVISOR_SOURCE)))) {
    install(ctx, receipt);
  } else {
    say(ctx, `supervisor already running and current (pid ${live.pid})`);
  }
  const timers = legacyTimers(ctx);
  const selected = opts.mode === "migrate-all" ? timers : opts.agents.map((agent) => timers.find((timer) => timer.agent === agent) ?? { agent, unit: agent, problem: "no such legacy timer on this host" });
  if (selected.length === 0) {
    say(ctx, "no legacy doorbell timers to migrate");
    return 0;
  }
  const participants = participantList(ctx);
  let incomplete = 0;
  for (const timer of selected) {
    if (!migrateOne(ctx, receipt, timer, participants)) incomplete += 1;
  }
  if (incomplete) {
    say(ctx, `${incomplete} timer(s) not migrated; they keep ringing on their old mechanism`);
    return 3;
  }
  return 0;
}

try {
  process.exitCode = main(process.argv.slice(2));
} catch (error) {
  if (error instanceof Fail) {
    process.stderr.write(`install-doorbell-supervisor: ${error.message}\n`);
    if (error.code === 2) process.stderr.write(`${usageText()}\n`);
    process.exitCode = error.code;
  } else {
    throw error;
  }
}
