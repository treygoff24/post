#!/usr/bin/env node
// Idempotently register the cursor-mail adapter in a Cursor hooks.json.
// Merges ONLY this adapter's entries; every unrelated hook is preserved.
//
//   node install-cursor-hooks.mjs <path-to-hooks.json>
//
// The target path is a required argument on purpose: this script never guesses
// at (or silently edits) a live config. Run it against ~/.cursor/hooks.json
// deliberately. Safe to re-run: an existing cursor-mail entry is updated in
// place, never duplicated. The reviewed adapter is copied to ~/.cursor/hooks/
// (with mail-hook-core.mjs, which it imports, and watch-notice.mjs alongside
// it); those private copies are what future Cursor sessions execute.

import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { randomBytes } from "node:crypto";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import { stableNodePath } from "./stable-node-path.mjs";

const DIR = path.dirname(fileURLToPath(import.meta.url));
const SOURCE = path.join(DIR, "cursor-mail.mjs");
const NOTICE_SOURCE = path.join(DIR, "watch-notice.mjs");
const CORE_SOURCE = path.join(DIR, "mail-hook-core.mjs");
const INSTALL_DIR =
  process.env.POST_CURSOR_HOOK_INSTALL_DIR || path.join(os.homedir(), ".cursor", "hooks");
const ADAPTER = path.join(INSTALL_DIR, "post-cursor-mail.mjs");
// The adapter imports this by its plain name, so it sits beside the adapter.
const CORE = path.join(INSTALL_DIR, "mail-hook-core.mjs");
const NOTICE = path.join(INSTALL_DIR, "post-watch-notice.mjs");
// Pin an absolute Node that survives package-manager upgrades: process.execPath
// is version-pinned on Homebrew, so baking it in breaks every hook with exit 127
// at the next `brew upgrade node` (see stable-node-path.mjs). Shell-quote both args.
const NODE_BIN = stableNodePath();
const COMMAND = `${JSON.stringify(NODE_BIN)} ${JSON.stringify(ADAPTER)}`;
const EVENTS = ["sessionStart", "beforeSubmitPrompt", "postToolUse"];
const INTEGRATION_NAMES = new Set(["cursor-mail.mjs", "post-cursor-mail.mjs"]);

const USAGE = "usage: node install-cursor-hooks.mjs <path-to-hooks.json>";

const argv = process.argv.slice(2);
// `-h`/`--help` anywhere prints usage on stdout and exits 0, before the target
// is even looked at or the post preflight runs.
if (argv.includes("-h") || argv.includes("--help")) {
  process.stdout.write(`${USAGE}\n`);
  process.exit(0);
}

const requestedTarget = argv[0];
if (!requestedTarget || requestedTarget.startsWith("-")) {
  console.error(USAGE);
  process.exit(2);
}

function resolvedPostBinary() {
  if (process.env.POST_CURSOR_HOOK_BIN) return process.env.POST_CURSOR_HOOK_BIN;
  const installed = path.join(os.homedir(), ".local", "bin", "post");
  try {
    fs.accessSync(installed, fs.constants.X_OK);
    return installed;
  } catch {
    return "post";
  }
}
{
  const probeRoot = fs.mkdtempSync(path.join(os.tmpdir(), "post-cursor-preflight-"));
  const probeCwd = path.join(probeRoot, "unroomed-probe");
  const probeMail = path.join(probeRoot, "mail");
  fs.mkdirSync(probeCwd, { recursive: true });
  const probe = spawnSync(resolvedPostBinary(), ["watch", "--snapshot"], {
    cwd: probeCwd,
    env: { ...process.env, POST_MAIL_ROOT: probeMail },
    encoding: "utf8",
    timeout: 4000,
    stdio: ["ignore", "pipe", "pipe"],
  });
  const minted = fs.existsSync(path.join(probeMail, "unroomed-probe"));
  fs.rmSync(probeRoot, { recursive: true, force: true });
  if (probe.error || probe.status !== 0 || minted) {
    console.error(
      minted
        ? "preflight failed: the installed post binary mints a mailbox for an unregistered cwd (pre-0.2.0). Rebuild and reinstall post, then re-run."
        : `preflight failed: could not run the post binary (${probe.error?.message ?? `exit ${probe.status}`}). Fix the post install, then re-run.`
    );
    process.exit(1);
  }
}

let target = requestedTarget;
let requestedExists = false;
try {
  fs.lstatSync(requestedTarget);
  requestedExists = true;
  target = fs.realpathSync(requestedTarget);
} catch (error) {
  if (requestedExists || error.code !== "ENOENT") throw error;
}

function writeAllSync(fd, data) {
  const buf = Buffer.isBuffer(data) ? data : Buffer.from(data);
  let offset = 0;
  while (offset < buf.length) {
    const n = fs.writeSync(fd, buf, offset, buf.length - offset);
    if (n <= 0) throw new Error("short write");
    offset += n;
  }
}

// Every file the install writes is STAGED first and renamed into place only
// after every write has succeeded, so a failure while staging (disk full, an
// unwritable config directory) leaves what is installed exactly as it was; the
// temps are removed on exit. Renames run in staging order (watch-notice, core,
// adapter, then config), so an adapter never lands without the core it imports.
const staged = [];
process.on("exit", () => {
  for (const { tmp } of staged) {
    try {
      fs.unlinkSync(tmp);
    } catch {
      // Never created, or already renamed into place.
    }
  }
});

function commitStaged() {
  while (staged.length > 0) {
    fs.renameSync(staged[0].tmp, staged[0].file);
    staged.shift();
  }
}

function stageFile(file, content, mode) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  let writeMode = mode;
  if (writeMode === undefined) {
    writeMode = 0o600;
    try {
      writeMode = fs.statSync(file).mode & 0o777;
    } catch {
      // New file: restrictive default.
    }
  }
  const dir = path.dirname(file);
  const tmp = path.join(
    dir,
    `.${path.basename(file)}.${process.pid}.${randomBytes(8).toString("hex")}.tmp`
  );
  let fd;
  try {
    const flags =
      fs.constants.O_WRONLY |
      fs.constants.O_CREAT |
      fs.constants.O_EXCL |
      (fs.constants.O_NOFOLLOW || 0);
    fd = fs.openSync(tmp, flags, writeMode);
    writeAllSync(fd, content);
    // The umask must not trim the mode the file is meant to have.
    fs.fchmodSync(fd, writeMode);
    fs.closeSync(fd);
    fd = undefined;
  } catch (error) {
    if (fd !== undefined) {
      try {
        fs.closeSync(fd);
      } catch {
        // Best-effort.
      }
    }
    try {
      fs.unlinkSync(tmp);
    } catch {
      // Only this process's temp.
    }
    throw error;
  }
  staged.push({ tmp, file });
}

// Stage a reviewed file for installation if its bytes or mode differ. Returns
// whether anything will change.
function copyScript(source, dest, mode = 0o755) {
  const bytes = fs.readFileSync(source);
  let current = null;
  try {
    current = { bytes: fs.readFileSync(dest), mode: fs.statSync(dest).mode & 0o777 };
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
  }
  const changed = current === null || !current.bytes.equals(bytes) || current.mode !== mode;
  if (changed) stageFile(dest, bytes, mode);
  return changed;
}

function normalizeConfig(parsed) {
  let config = parsed;
  if (config === null || typeof config !== "object" || Array.isArray(config)) {
    config = {};
  }
  if (typeof config.hooks !== "object" || config.hooks === null || Array.isArray(config.hooks)) {
    config = { ...config, hooks: {} };
  }
  if (config.version === undefined) config = { ...config, version: 1 };
  return config;
}

let config = { version: 1, hooks: {} };
if (fs.existsSync(target)) {
  let raw;
  try {
    raw = fs.readFileSync(target, "utf8");
  } catch (error) {
    console.error(`install-cursor-hooks: could not read ${target}: ${error.message}`);
    process.exit(1);
  }
  let parsed;
  try {
    parsed = JSON.parse(raw);
  } catch (error) {
    console.error(
      `install-cursor-hooks: ${target} is not valid JSON (${error.message}); fix it, then re-run.`
    );
    process.exit(1);
  }
  config = normalizeConfig(parsed);
}

fs.mkdirSync(path.dirname(ADAPTER), { recursive: true });
const noticeChanged = copyScript(NOTICE_SOURCE, NOTICE);
// The adapter imports ./mail-hook-core.mjs, so the core goes in before it.
const coreChanged = copyScript(CORE_SOURCE, CORE, 0o644);
const adapterChanged = copyScript(SOURCE, ADAPTER);

function unquoteLeadingArg(text) {
  const s = text.trim();
  if (s.startsWith('"')) {
    const match = s.match(/^"(?:\\.|[^"\\])*"/);
    if (!match) return null;
    try {
      return { value: JSON.parse(match[0]), rest: s.slice(match[0].length).trim() };
    } catch {
      return null;
    }
  }
  if (s.startsWith("'")) {
    const end = s.indexOf("'", 1);
    if (end < 0) return null;
    return { value: s.slice(1, end), rest: s.slice(end + 1).trim() };
  }
  const m = s.match(/^([^\s]+)(.*)$/);
  if (!m) return null;
  return { value: m[1], rest: m[2].trim() };
}

function isIntegrationHook(hook) {
  const command = String(hook?.command ?? "").trim();
  if (command === COMMAND) return true;
  const first = unquoteLeadingArg(command);
  if (!first) return false;
  const isNode =
    first.value === "node" ||
    first.value === process.execPath ||
    path.basename(first.value) === "node";
  if (!isNode) return false;
  const second = unquoteLeadingArg(first.rest);
  if (!second) return false;
  return INTEGRATION_NAMES.has(path.basename(second.value));
}

const canonicalHook = () => ({ command: COMMAND, timeout: 10 });

const original = JSON.stringify(config);
for (const event of EVENTS) {
  const list = Array.isArray(config.hooks[event]) ? config.hooks[event] : [];
  const kept = [];
  for (const hook of list) {
    if (!hook || typeof hook !== "object" || Array.isArray(hook)) {
      kept.push(hook);
      continue;
    }
    if (!isIntegrationHook(hook)) kept.push(hook);
  }
  kept.push(canonicalHook());
  config.hooks[event] = kept;
}

const configChanged = JSON.stringify(config) !== original;
if (configChanged) {
  stageFile(target, `${JSON.stringify(config, null, 2)}\n`);
}
commitStaged();
console.log(
  configChanged || adapterChanged || coreChanged || noticeChanged
    ? [
        configChanged && "hooks updated",
        (adapterChanged || coreChanged) && "adapter updated",
        noticeChanged && "watch-notice updated",
      ]
        .filter(Boolean)
        .join("\n")
    : "already registered; no changes"
);
