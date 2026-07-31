#!/usr/bin/env node
// Idempotently register the codex-mail adapter in a Codex hooks.json.
// Merges ONLY this adapter's entries; every unrelated hook is preserved.
//
//   node install-codex-hooks.mjs <path-to-hooks.json>
//
// The target path is a required argument on purpose: this script never guesses
// at (or silently edits) a live config. Run it against ~/.codex/hooks.json
// deliberately. Safe to re-run: an existing codex-mail entry is updated in
// place, never duplicated. The reviewed adapter is copied to ~/.codex/hooks/
// and that private copy is what future Codex sessions execute.

import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const SOURCE = path.join(os.homedir(), ".codex", "skills", "post", "hooks", "codex-mail.mjs");
const ADAPTER = path.join(os.homedir(), ".codex", "hooks", "post-codex-mail.mjs");
const COMMAND = `node ${JSON.stringify(ADAPTER)}`;
const EVENTS = ["SessionStart", "UserPromptSubmit", "PostToolUse"];

const requestedTarget = process.argv[2];
// Flag-looking argv = usage error, not a target path (see Claude twin).
if (!requestedTarget || requestedTarget.startsWith("-")) {
  console.error("usage: node install-codex-hooks.mjs <path-to-hooks.json>");
  process.exit(2);
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

let config = { hooks: {} };
if (fs.existsSync(target)) {
  config = JSON.parse(fs.readFileSync(target, "utf8"));
}
if (typeof config.hooks !== "object" || config.hooks === null) config.hooks = {};

const canonicalHook = () => ({ type: "command", command: COMMAND, timeout: 5 });
function isIntegrationHook(hook) {
  const command = String(hook?.command ?? "");
  if (!command.startsWith("node ")) return false;
  let script = command.slice(5).trim();
  try {
    if (script.startsWith('"')) script = JSON.parse(script);
    else if (script.startsWith("'") && script.endsWith("'")) script = script.slice(1, -1);
  } catch {
    return false;
  }
  return ["codex-mail.mjs", "post-codex-mail.mjs"].includes(path.basename(script));
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
