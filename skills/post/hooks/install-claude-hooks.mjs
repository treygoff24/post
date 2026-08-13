#!/usr/bin/env node
// Idempotently register the claude-mail adapter in a Claude Code settings.json.
// Merges ONLY this adapter's entries; every unrelated hook is preserved.
//
//   node install-claude-hooks.mjs <path-to-settings.json>
//
// The target path is a required argument on purpose: this script never guesses
// at (or silently edits) a live config. Run it against the intended settings
// file (user-level ~/.claude/settings.json, or a profile variant) deliberately.
// Safe to re-run: an existing claude-mail entry is updated in place, never
// duplicated. The reviewed adapter is copied to ~/.claude/hooks/ and that
// private copy is what future Claude sessions execute, so later repo edits do
// not silently change live hook behavior.
//
// Registration uses the exec form (command + args array): no shell, exact
// argv, and Claude Code deduplicates identical command+args registrations
// across settings levels. Timeouts are in seconds in Claude Code.

import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { randomBytes } from "node:crypto";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const SOURCE = path.join(path.dirname(fileURLToPath(import.meta.url)), "claude-mail.mjs");
// POST_CLAUDE_HOOK_INSTALL_DIR is a test override; live installs use the default.
const ADAPTER = path.join(
  process.env.POST_CLAUDE_HOOK_INSTALL_DIR || path.join(os.homedir(), ".claude", "hooks"),
  "post-claude-mail.mjs"
);
const EVENTS = ["SessionStart", "UserPromptSubmit", "PostToolUse"];

const requestedTarget = process.argv[2];
// A flag-looking argv is a usage error, not a settings path: without this
// guard, `install-claude-hooks.mjs --help` silently writes a config file
// literally named ./--help (caught live, 2026-07-31).
if (!requestedTarget || requestedTarget.startsWith("-")) {
  console.error("usage: node install-claude-hooks.mjs <path-to-settings.json>");
  process.exit(2);
}

// Preflight: the adapter depends on post >= 0.2.0, where a snapshot from an
// unregistered cwd scans nothing and creates nothing. A stale installed binary
// reproduces the junk-mailbox bug on every hook fire from an unroomed project
// (caught live in review, 2026-07-30), so the precondition is enforced, not
// documented: probe the exact binary the adapter will resolve, from a temp
// unregistered cwd against an isolated mail root, and refuse to install if a
// mailbox appears or the probe fails.
function resolvedPostBinary() {
  if (process.env.POST_CLAUDE_HOOK_BIN) return process.env.POST_CLAUDE_HOOK_BIN;
  const installed = path.join(os.homedir(), ".local", "bin", "post");
  try {
    fs.accessSync(installed, fs.constants.X_OK);
    return installed;
  } catch {
    return "post";
  }
}
{
  const probeRoot = fs.mkdtempSync(path.join(os.tmpdir(), "post-claude-preflight-"));
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
try {
  fs.lstatSync(requestedTarget);
  target = fs.realpathSync(requestedTarget);
} catch (error) {
  if (error.code !== "ENOENT") throw error;
}

// Config is parsed BEFORE any file is copied: a malformed target must
// fail the install leaving neither the adapter nor the helper behind.
let config = {};
if (fs.existsSync(target)) {
  config = JSON.parse(fs.readFileSync(target, "utf8"));
}
if (typeof config !== "object" || config === null || Array.isArray(config)) config = {};
if (typeof config.hooks !== "object" || config.hooks === null) config.hooks = {};

fs.mkdirSync(path.dirname(ADAPTER), { recursive: true });

// Random-named exclusive temp, O_EXCL|O_NOFOLLOW: a planted predictable
// symlink can neither be followed nor clobber a victim file.
function writeFileAtomic(file, bytes, mode) {
  const tmp = path.join(
    path.dirname(file),
    `.${path.basename(file)}.${process.pid}.${randomBytes(8).toString("hex")}.tmp`
  );
  let fd;
  try {
    const flags =
      fs.constants.O_WRONLY |
      fs.constants.O_CREAT |
      fs.constants.O_EXCL |
      (fs.constants.O_NOFOLLOW || 0);
    fd = fs.openSync(tmp, flags, mode);
    // Full-write loop: writeSync may write fewer than bytes.length, and
    // renaming after a short write would atomically install a truncated file.
    const buf = Buffer.isBuffer(bytes) ? bytes : Buffer.from(bytes);
    let offset = 0;
    while (offset < buf.length) {
      const n = fs.writeSync(fd, buf, offset, buf.length - offset);
      if (n <= 0) throw new Error("short write");
      offset += n;
    }
    fs.closeSync(fd);
    fd = undefined;
    fs.renameSync(tmp, file);
  } catch (error) {
    if (fd !== undefined) {
      try {
        fs.closeSync(fd);
      } catch {
        // Best-effort close before unlink.
      }
    }
    try {
      fs.unlinkSync(tmp);
    } catch {
      // Only this process's temp; ignore if create failed first.
    }
    throw error;
  }
}

// Dependency-ordered: the helper the adapter imports lands first, the
// adapter that imports it last. A failed helper copy leaves the OLD
// runnable adapter in place; a helper-only partial is harmless.
// (The adapter statically imports ./identity-card.mjs since M5; an
// adapter-without-helper install crashes every hook fire.)
const HELPER = path.join(path.dirname(ADAPTER), "identity-card.mjs");
const helperSource = fs.readFileSync(
  path.join(path.dirname(fileURLToPath(import.meta.url)), "identity-card.mjs")
);
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

const canonicalHook = () => ({ type: "command", command: "node", args: [ADAPTER], timeout: 10 });

function isIntegrationHook(hook) {
  const names = ["claude-mail.mjs", "post-claude-mail.mjs"];
  if (Array.isArray(hook?.args)) {
    return hook.args.some(
      (arg) => typeof arg === "string" && names.includes(path.basename(arg))
    );
  }
  // Legacy shell-form registration: `node <path>`.
  const command = String(hook?.command ?? "");
  if (!command.startsWith("node ")) return false;
  let script = command.slice(5).trim();
  try {
    if (script.startsWith('"')) script = JSON.parse(script);
    else if (script.startsWith("'") && script.endsWith("'")) script = script.slice(1, -1);
  } catch {
    return false;
  }
  return names.includes(path.basename(script));
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
  const tmp = `${target}.${process.pid}.tmp`;
  fs.writeFileSync(tmp, `${JSON.stringify(config, null, 2)}\n`);
  fs.renameSync(tmp, target);
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
