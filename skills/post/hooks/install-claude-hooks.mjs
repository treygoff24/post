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
// duplicated. The reviewed adapter and the shared mail-hook-core.mjs it imports
// are copied to ~/.claude/hooks/ and that private copy is what future Claude
// sessions execute, so later repo edits do not silently change live hook
// behavior.
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

const HERE = path.dirname(fileURLToPath(import.meta.url));
const SOURCE = path.join(HERE, "claude-mail.mjs");
const CORE_SOURCE = path.join(HERE, "mail-hook-core.mjs");
// POST_CLAUDE_HOOK_INSTALL_DIR is a test override; live installs use the default.
const ADAPTER = path.join(
  process.env.POST_CLAUDE_HOOK_INSTALL_DIR || path.join(os.homedir(), ".claude", "hooks"),
  "post-claude-mail.mjs"
);
// The adapter imports this by its plain name, so it sits beside the adapter.
const CORE = path.join(path.dirname(ADAPTER), "mail-hook-core.mjs");
// Stop carries only the doorbell turn mark (see claude-mail.mjs).
const EVENTS = ["SessionStart", "UserPromptSubmit", "PostToolUse", "Stop", "SessionEnd"];

const USAGE = "usage: node install-claude-hooks.mjs <path-to-settings.json>";

const argv = process.argv.slice(2);
// `-h`/`--help` anywhere prints usage on stdout and exits 0, before the target
// is even looked at or the post preflight runs.
if (argv.includes("-h") || argv.includes("--help")) {
  process.stdout.write(`${USAGE}\n`);
  process.exit(0);
}

const requestedTarget = argv[0];
// A flag-looking argv is a usage error, not a settings path: without this
// guard, `install-claude-hooks.mjs --help` silently writes a config file
// literally named ./--help (caught live, 2026-07-31).
if (!requestedTarget || requestedTarget.startsWith("-")) {
  console.error(USAGE);
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

// Every file the install writes is STAGED first and renamed into place only
// after every write has succeeded, so a failure while staging (disk full, an
// unwritable settings directory) leaves what is installed exactly as it was; the
// temps are removed on exit. Renames run in staging order (core, adapter, then
// settings), so an adapter never lands without the core it imports.
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

// Random-named exclusive temp, O_EXCL|O_NOFOLLOW: a planted predictable
// symlink can neither be followed nor clobber a victim file.
function stageFile(file, bytes, mode) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
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
    // The umask must not trim the mode the file is meant to have.
    fs.fchmodSync(fd, mode);
    fs.closeSync(fd);
    fd = undefined;
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
  staged.push({ tmp, file });
}

function commitStaged() {
  while (staged.length > 0) {
    fs.renameSync(staged[0].tmp, staged[0].file);
    staged.shift();
  }
}

// Stage a reviewed file for installation if its bytes or mode differ. Returns
// whether anything will change.
function installFile(from, to, mode) {
  const bytes = fs.readFileSync(from);
  let current = null;
  try {
    current = { bytes: fs.readFileSync(to), mode: fs.statSync(to).mode & 0o777 };
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
  }
  const changed = current === null || !current.bytes.equals(bytes) || current.mode !== mode;
  if (changed) stageFile(to, bytes, mode);
  return changed;
}

// The adapter imports ./mail-hook-core.mjs, so the core goes in first and the
// two always travel together.
const coreChanged = installFile(CORE_SOURCE, CORE, 0o644);
const adapterChanged = installFile(SOURCE, ADAPTER, 0o755);

const canonicalHook = () => ({ type: "command", command: "node", args: [ADAPTER], timeout: 10 });

// Ownership is by path or marker, never by basename: another tool's hook that
// happens to be named claude-mail.mjs must survive the install. A registration
// is ours when its script is the installed adapter path (a leading $HOME,
// ${HOME}, or ~ expanded, as the shell-form registrations wrote it), or an
// existing file carrying the header every version of the adapter has had.
const ADAPTER_MARKER = '// Claude Code hook adapter: injects metadata-only "new post mail" notifications';

function expandHome(script) {
  const home = os.homedir();
  for (const prefix of ["$HOME/", "${HOME}/", "~/"]) {
    if (script.startsWith(prefix)) return path.join(home, script.slice(prefix.length));
  }
  return script;
}

function isOurScript(script) {
  if (typeof script !== "string" || script === "") return false;
  const resolved = path.resolve(expandHome(script));
  if (resolved === path.resolve(ADAPTER)) return true;
  try {
    if (fs.realpathSync(resolved) === fs.realpathSync(ADAPTER)) return true;
  } catch {
    // Either side missing: fall through to the marker.
  }
  try {
    const head = fs.readFileSync(resolved, "utf8").split("\n", 3);
    return head.includes(ADAPTER_MARKER);
  } catch {
    return false;
  }
}

function isIntegrationHook(hook) {
  if (Array.isArray(hook?.args)) {
    return path.basename(String(hook.command ?? "")) === "node" && hook.args.length === 1 && isOurScript(hook.args[0]);
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
  return isOurScript(script);
}

const original = JSON.stringify(config);
// Cleanup visits every event in the file, not only EVENTS: an upgrade from a
// version that registered the adapter elsewhere (PreToolUse, on the first
// doorbell turn-mark commit) must not keep launching it there. Only owned
// registrations are removed; the adapter is re-added for EVENTS alone.
const visited = new Set([...Object.keys(config.hooks).filter((event) => Array.isArray(config.hooks[event])), ...EVENTS]);
for (const event of visited) {
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
  if (EVENTS.includes(event)) normalized.push({ hooks: [canonicalHook()] });
  else if (normalized.length === groups.length) continue; // nothing of ours here: untouched
  if (normalized.length === 0) delete config.hooks[event];
  else config.hooks[event] = normalized;
}

const configChanged = JSON.stringify(config) !== original;
if (configChanged) {
  // Keep an existing settings file's mode; a new one is created 0644.
  let configMode = 0o644;
  try {
    configMode = fs.statSync(target).mode & 0o777;
  } catch {
    // New file.
  }
  stageFile(target, `${JSON.stringify(config, null, 2)}\n`, configMode);
}
commitStaged();
console.log(
  configChanged || adapterChanged || coreChanged
    ? [
        configChanged && "hooks updated",
        (adapterChanged || coreChanged) && "adapter updated",
      ]
        .filter(Boolean)
        .join("\n")
    : "already registered; no changes"
);
