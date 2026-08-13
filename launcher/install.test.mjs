// Tests for launcher/install ownership discipline. Run:
//     node --test launcher/install.test.mjs
// Drives the real installer against a throwaway prefix/link dir; proves
// uninstall deletes only files it owns, preserving foreign sentinels and
// replacement symlinks, and that --check flags vendor-link drift.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const LAUNCHER_DIR = path.dirname(fileURLToPath(import.meta.url));
const INSTALL = path.join(LAUNCHER_DIR, "install");
const VENDORS = ["claude", "codex", "cursor-agent", "grok"];

function sandbox() {
  const work = fs.mkdtempSync(path.join(os.tmpdir(), "install-test-"));
  return {
    prefix: path.join(work, "prefix"),
    links: path.join(work, "links"),
  };
}

function run(env, ...args) {
  return spawnSync(INSTALL, args, {
    encoding: "utf8",
    env: {
      ...process.env,
      POST_LAUNCHER_PREFIX: env.prefix,
      POST_AGENT_SHIM_DIR: env.links,
    },
  });
}

test("install then uninstall removes only owned files, empty dirs", () => {
  const sb = sandbox();
  assert.equal(run(sb).status, 0);
  assert.ok(fs.existsSync(path.join(sb.prefix, "agent-session")));

  // Plant foreign sentinels inside the prefix and link dir.
  const sentinel = path.join(sb.prefix, "KEEP-ME.txt");
  fs.writeFileSync(sentinel, "not yours\n");
  const foreignLink = path.join(sb.links, "other-tool");
  fs.symlinkSync("/usr/bin/true", foreignLink);

  const un = run(sb, "--uninstall");
  assert.equal(un.status, 0);
  assert.ok(fs.existsSync(sentinel), "foreign sentinel survives uninstall");
  assert.ok(fs.existsSync(sb.prefix), "non-empty prefix is left in place");
  assert.match(un.stdout, /leaving non-empty/);
  assert.ok(fs.lstatSync(foreignLink), "foreign symlink survives");
  for (const v of VENDORS) {
    assert.ok(!fs.existsSync(path.join(sb.links, v)), `${v} link removed`);
  }
  assert.ok(!fs.existsSync(path.join(sb.prefix, "shims")), "shims dir removed");
});

test("uninstall of clean install removes the whole prefix", () => {
  const sb = sandbox();
  assert.equal(run(sb).status, 0);
  assert.equal(run(sb, "--uninstall").status, 0);
  assert.ok(!fs.existsSync(sb.prefix), "empty prefix fully removed");
});

test("uninstall leaves replacement vendor symlinks it no longer owns", () => {
  const sb = sandbox();
  assert.equal(run(sb).status, 0);
  // Another tool replaced the claude link.
  const claudeLink = path.join(sb.links, "claude");
  fs.unlinkSync(claudeLink);
  fs.symlinkSync("/usr/bin/true", claudeLink);

  const un = run(sb, "--uninstall");
  assert.equal(un.status, 0);
  assert.equal(fs.readlinkSync(claudeLink), "/usr/bin/true");
  assert.match(un.stdout, /leaving .*claude \(points to/);
});

test("uninstall with a hostile prefix deletes nothing foreign", () => {
  const sb = sandbox();
  // Never installed: prefix dir full of someone else's files.
  fs.mkdirSync(sb.prefix, { recursive: true });
  const sentinel = path.join(sb.prefix, "precious");
  fs.writeFileSync(sentinel, "data\n");
  assert.equal(run(sb, "--uninstall").status, 0);
  assert.ok(fs.existsSync(sentinel), "hostile-prefix contents untouched");
});

test("--check flags vendor-link drift and passes when clean", () => {
  const sb = sandbox();
  assert.equal(run(sb).status, 0);
  const clean = run(sb, "--check");
  assert.equal(clean.status, 0);
  assert.match(clean.stdout, /check: link claude OK/);

  const claudeLink = path.join(sb.links, "claude");
  fs.unlinkSync(claudeLink);
  fs.symlinkSync("/usr/bin/true", claudeLink);
  const drifted = run(sb, "--check");
  assert.equal(drifted.status, 1);
  assert.match(drifted.stdout, /check: link claude DRIFT/);
});
