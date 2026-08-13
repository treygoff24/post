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

test("uninstall of a never-installed prefix is a total no-op, even at managed names", () => {
  const sb = sandbox();
  // Never installed: foreign files occupy every managed filename plus one.
  fs.mkdirSync(path.join(sb.prefix, "shims"), { recursive: true });
  const foreign = [
    "agent-session",
    "shims/claude",
    "shims/codex",
    "shims/cursor",
    "shims/grok",
    "precious",
  ].map((f) => path.join(sb.prefix, f));
  for (const f of foreign) fs.writeFileSync(f, "not yours\n");

  const un = run(sb, "--uninstall");
  assert.equal(un.status, 0);
  assert.match(un.stdout, /no install receipt/);
  for (const f of foreign) assert.ok(fs.existsSync(f), `${f} survives`);
  assert.ok(fs.existsSync(path.join(sb.prefix, "shims")), "shims dir survives");
  assert.ok(fs.existsSync(sb.prefix), "prefix survives");
});

test("uninstall preserves managed files tampered after install", () => {
  const sb = sandbox();
  assert.equal(run(sb).status, 0);
  const tampered = path.join(sb.prefix, "shims", "codex");
  fs.writeFileSync(tampered, "#!/bin/sh\nreplaced by someone else\n");

  const un = run(sb, "--uninstall");
  assert.equal(un.status, 0);
  assert.ok(fs.existsSync(tampered), "tampered file survives");
  assert.match(un.stdout, /leaving .*shims\/codex \(modified since install/);
  assert.ok(!fs.existsSync(path.join(sb.prefix, "agent-session")), "untampered file removed");
  assert.ok(fs.existsSync(sb.prefix), "prefix left (still holds the tampered file)");
});

test("uninstall with a receipt bound to another prefix deletes no files", () => {
  const sb = sandbox();
  assert.equal(run(sb).status, 0);
  // Simulate a moved/copied prefix: rebind the receipt to another path.
  const receipt = path.join(sb.prefix, ".install-receipt");
  const rebound = fs
    .readFileSync(receipt, "utf8")
    .replace(/^prefix .*$/m, "prefix /somewhere/else");
  fs.writeFileSync(receipt, rebound);

  const un = run(sb, "--uninstall");
  assert.equal(un.status, 0);
  assert.match(un.stdout, /receipt is bound to \/somewhere\/else/);
  assert.ok(fs.existsSync(path.join(sb.prefix, "agent-session")), "files survive");
});

test("never-installed uninstall preserves links even when they point at the prefix", () => {
  const sb = sandbox();
  // Never installed: foreign managed filenames AND a link aimed at our
  // would-be shim path (Sol's round-3 mutation).
  fs.mkdirSync(path.join(sb.prefix, "shims"), { recursive: true });
  fs.writeFileSync(path.join(sb.prefix, "shims", "codex"), "foreign\n");
  fs.mkdirSync(sb.links, { recursive: true });
  const link = path.join(sb.links, "codex");
  fs.symlinkSync(path.join(sb.prefix, "shims", "codex"), link);

  const un = run(sb, "--uninstall");
  assert.equal(un.status, 0);
  assert.match(un.stdout, /deleting nothing/);
  assert.ok(fs.lstatSync(link), "link survives without receipt proof");
  assert.ok(fs.existsSync(path.join(sb.prefix, "shims", "codex")), "file survives");
});

test("--check goes red when the receipt is missing or rebound", () => {
  const sb = sandbox();
  assert.equal(run(sb).status, 0);
  const receipt = path.join(sb.prefix, ".install-receipt");

  const rebound = fs
    .readFileSync(receipt, "utf8")
    .replace(/^prefix .*$/m, "prefix /somewhere/else");
  fs.writeFileSync(receipt, rebound);
  let check = run(sb, "--check");
  assert.equal(check.status, 1);
  assert.match(check.stdout, /check: receipt DRIFT bound to \/somewhere\/else/);

  fs.unlinkSync(receipt);
  check = run(sb, "--check");
  assert.equal(check.status, 1);
  assert.match(check.stdout, /check: receipt MISSING/);

  assert.equal(run(sb).status, 0); // re-install re-mints
  assert.equal(run(sb, "--check").status, 0);
});

test("--check flags receipt hash drift for a tampered file", () => {
  const sb = sandbox();
  assert.equal(run(sb).status, 0);
  fs.writeFileSync(path.join(sb.prefix, "shims", "grok"), "tampered\n");
  const check = run(sb, "--check");
  assert.equal(check.status, 1);
  assert.match(check.stdout, /check: receipt shims\/grok DRIFT/);
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
