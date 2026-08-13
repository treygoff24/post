#!/usr/bin/env node
// Idempotently register the codex-mail adapter in a Codex hooks.json.
// Merges ONLY this adapter's entries; every unrelated hook is preserved.
//
//   node install-codex-hooks.mjs <path-to-hooks.json>
//
// The target path is a required argument on purpose: this script never guesses
// at (or silently edits) a live config. Run it against ~/.codex/hooks.json
// deliberately. Safe to re-run: an existing codex-mail entry is updated in
// place, never duplicated. The reviewed adapter is sourced from this
// installer's own directory and copied to ~/.codex/hooks/; that private copy
// is what future Codex sessions execute.

import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { randomBytes } from "node:crypto";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

// Source from this installer's own directory so the reviewed adapter follows
// the installed skill rather than a guessed location (~/.codex/skills).
// POST_CODEX_HOOK_INSTALL_DIR is a test override; live installs use the
// default.
const SOURCE = path.join(path.dirname(fileURLToPath(import.meta.url)), "codex-mail.mjs");
// The adapter statically imports ./identity-card.mjs (M5); the private
// install must carry it alongside or every installed hook fire crashes
// with ERR_MODULE_NOT_FOUND.
const HELPER_SOURCE = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  "identity-card.mjs"
);
const ADAPTER = path.join(
  process.env.POST_CODEX_HOOK_INSTALL_DIR || path.join(os.homedir(), ".codex", "hooks"),
  "post-codex-mail.mjs"
);
// Pin the absolute Node that is running this installer; shell-quote both args.
const COMMAND = `${JSON.stringify(process.execPath)} ${JSON.stringify(ADAPTER)}`;
const EVENTS = ["SessionStart", "UserPromptSubmit", "PostToolUse"];
const INTEGRATION_NAMES = new Set(["codex-mail.mjs", "post-codex-mail.mjs"]);

const requestedTarget = process.argv[2];
// Flag-looking argv = usage error, not a target path (see Claude twin).
if (!requestedTarget || requestedTarget.startsWith("-")) {
  console.error("usage: node install-codex-hooks.mjs <path-to-hooks.json>");
  process.exit(2);
}

// Preflight: the adapter depends on post >= 0.2.0, where a snapshot from an
// unregistered cwd scans nothing and creates nothing. A stale installed binary
// reproduces the junk-mailbox bug on every hook fire from an unroomed project
// (caught live in review, 2026-07-30), so the precondition is enforced, not
// documented: probe the exact binary the adapter would resolve, from a temp
// unregistered cwd against an isolated mail root, and refuse to install if a
// mailbox appears or the probe fails — before touching target or adapter.
function resolvedPostBinary() {
  if (process.env.POST_CODEX_HOOK_BIN) return process.env.POST_CODEX_HOOK_BIN;
  const installed = path.join(os.homedir(), ".local", "bin", "post");
  try {
    fs.accessSync(installed, fs.constants.X_OK);
    return installed;
  } catch {
    return "post";
  }
}
{
  const probeRoot = fs.mkdtempSync(path.join(os.tmpdir(), "post-codex-preflight-"));
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

function writeFileAtomic(file, content, mode) {
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
    fs.closeSync(fd);
    fd = undefined;
    fs.renameSync(tmp, file);
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
}

function normalizeConfig(parsed) {
  // Match the Claude twin: non-object roots become a fresh object; a non-object
  // hooks value becomes {}.
  let config = parsed;
  if (config === null || typeof config !== "object" || Array.isArray(config)) {
    config = {};
  }
  if (typeof config.hooks !== "object" || config.hooks === null || Array.isArray(config.hooks)) {
    config = { ...config, hooks: {} };
  }
  return config;
}

// Validate/normalize the target JSON BEFORE copying the adapter so a malformed
// config cannot leave a partial install.
let config = { hooks: {} };
if (fs.existsSync(target)) {
  let raw;
  try {
    raw = fs.readFileSync(target, "utf8");
  } catch (error) {
    console.error(`install-codex-hooks: could not read ${target}: ${error.message}`);
    process.exit(1);
  }
  let parsed;
  try {
    parsed = JSON.parse(raw);
  } catch (error) {
    console.error(
      `install-codex-hooks: ${target} is not valid JSON (${error.message}); fix it, then re-run.`
    );
    process.exit(1);
  }
  config = normalizeConfig(parsed);
} else {
  config = { hooks: {} };
}

fs.mkdirSync(path.dirname(ADAPTER), { recursive: true });
// Dependency-ordered: the helper the adapter imports lands first, the
// adapter that imports it last. A failed helper copy leaves the OLD
// runnable adapter in place; a helper-only partial is harmless.
const HELPER = path.join(path.dirname(ADAPTER), "identity-card.mjs");
const helperSource = fs.readFileSync(HELPER_SOURCE);
let helperChanged = true;
try {
  helperChanged = !helperSource.equals(fs.readFileSync(HELPER));
} catch (error) {
  if (error.code !== "ENOENT") throw error;
}
if (helperChanged) {
  writeFileAtomic(HELPER, helperSource, 0o644);
}

const source = fs.readFileSync(SOURCE);
let adapterChanged = true;
try {
  adapterChanged = !source.equals(fs.readFileSync(ADAPTER));
} catch (error) {
  if (error.code !== "ENOENT") throw error;
}
if (adapterChanged) {
  writeFileAtomic(ADAPTER, source, 0o755);
}
const adapterModeChanged = (fs.statSync(ADAPTER).mode & 0o777) !== 0o755;
if (adapterModeChanged) fs.chmodSync(ADAPTER, 0o755);

const canonicalHook = () => ({ type: "command", command: COMMAND, timeout: 5 });

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
  if (!second || second.rest !== "") return false;
  return INTEGRATION_NAMES.has(path.basename(second.value));
}

const original = JSON.stringify(config);
for (const event of EVENTS) {
  const groups = Array.isArray(config.hooks[event]) ? config.hooks[event] : [];
  const normalized = [];
  for (const group of groups) {
    if (!group || typeof group !== "object" || !Array.isArray(group.hooks)) {
      normalized.push(group);
      continue;
    }
    const hasIntegration = group.hooks.some(isIntegrationHook);
    if (!hasIntegration) normalized.push(group);
    else {
      const hooks = group.hooks.filter((hook) => !isIntegrationHook(hook));
      if (hooks.length > 0) normalized.push({ ...group, hooks });
    }
  }
  normalized.push({ hooks: [canonicalHook()] });
  config.hooks[event] = normalized;
}

const configChanged = JSON.stringify(config) !== original;
if (configChanged) {
  writeFileAtomic(target, `${JSON.stringify(config, null, 2)}\n`);
}
console.log(
  configChanged || adapterChanged || adapterModeChanged || helperChanged
    ? [
        configChanged && "hooks updated",
        (adapterChanged || adapterModeChanged) && "adapter updated",
        helperChanged && "identity-card helper updated",
      ]
        .filter(Boolean)
        .join("\n")
    : "already registered; no changes"
);
