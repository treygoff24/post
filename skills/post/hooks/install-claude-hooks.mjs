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
import { fileURLToPath } from "node:url";

const SOURCE = path.join(path.dirname(fileURLToPath(import.meta.url)), "claude-mail.mjs");
// POST_CLAUDE_HOOK_INSTALL_DIR is a test override; live installs use the default.
const ADAPTER = path.join(
  process.env.POST_CLAUDE_HOOK_INSTALL_DIR || path.join(os.homedir(), ".claude", "hooks"),
  "post-claude-mail.mjs"
);
const EVENTS = ["SessionStart", "UserPromptSubmit", "PostToolUse"];

const requestedTarget = process.argv[2];
if (!requestedTarget) {
  console.error("usage: node install-claude-hooks.mjs <path-to-settings.json>");
  process.exit(2);
}
let target = requestedTarget;
try {
  fs.lstatSync(requestedTarget);
  target = fs.realpathSync(requestedTarget);
} catch (error) {
  if (error.code !== "ENOENT") throw error;
}

fs.mkdirSync(path.dirname(ADAPTER), { recursive: true });
const source = fs.readFileSync(SOURCE);
let adapterChanged = true;
try {
  adapterChanged = !source.equals(fs.readFileSync(ADAPTER));
} catch (error) {
  if (error.code !== "ENOENT") throw error;
}
if (adapterChanged) {
  const adapterTmp = `${ADAPTER}.${process.pid}.tmp`;
  fs.writeFileSync(adapterTmp, source, { mode: 0o755 });
  fs.renameSync(adapterTmp, ADAPTER);
}
const adapterModeChanged = (fs.statSync(ADAPTER).mode & 0o777) !== 0o755;
if (adapterModeChanged) fs.chmodSync(ADAPTER, 0o755);

let config = {};
if (fs.existsSync(target)) {
  config = JSON.parse(fs.readFileSync(target, "utf8"));
}
if (typeof config !== "object" || config === null || Array.isArray(config)) config = {};
if (typeof config.hooks !== "object" || config.hooks === null) config.hooks = {};

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
  configChanged || adapterChanged || adapterModeChanged
    ? [
        configChanged && "hooks updated",
        (adapterChanged || adapterModeChanged) && "adapter updated",
      ]
        .filter(Boolean)
        .join("\n")
    : "already registered; no changes"
);
