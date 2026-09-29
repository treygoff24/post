// Self-tests for install-cursor-hooks.mjs. Run: node --test skills/post/hooks/*.test.mjs
// Hermetic: POST_CURSOR_HOOK_INSTALL_DIR keeps the adapter copy inside the temp
// root and POST_CURSOR_HOOK_BIN drives the preflight probe; no live config or
// home directory is touched. The adapter source always comes from this
// installer's own directory (the checked-in cursor-mail.mjs).

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

import { stableNodePath } from "./stable-node-path.mjs";

const DIR = path.dirname(fileURLToPath(import.meta.url));
// The installers pin a package-manager-stable alias for the running Node
// rather than the version-pinned process.execPath, so the expected command
// must be built the same way (see stable-node-path.mjs).
const NODE_BIN = stableNodePath();
const INSTALLER = path.join(DIR, "install-cursor-hooks.mjs");
const SOURCE = path.join(DIR, "cursor-mail.mjs");
const ROOT = fs.mkdtempSync(path.join(os.tmpdir(), "post-cursor-install-test-"));
const INSTALL_DIR = path.join(ROOT, "hooks");
const ADAPTER = path.join(INSTALL_DIR, "post-cursor-mail.mjs");
const NOTICE = path.join(INSTALL_DIR, "post-watch-notice.mjs");
const CORE_SOURCE = path.join(DIR, "mail-hook-core.mjs");
const CORE = path.join(INSTALL_DIR, "mail-hook-core.mjs");

// Preflight stubs: a fixed post that mints nothing, and a stale one that
// reproduces the pre-0.2.0 junk-mailbox bug.
const GOOD_POST = path.join(ROOT, "good-post.mjs");
fs.writeFileSync(GOOD_POST, "#!/usr/bin/env node\nprocess.exit(0);\n", { mode: 0o755 });
const STALE_POST = path.join(ROOT, "stale-post.mjs");
fs.writeFileSync(
  STALE_POST,
  [
    "#!/usr/bin/env node",
    'import fs from "node:fs";',
    'import path from "node:path";',
    'const room = path.basename(process.cwd());',
    'fs.mkdirSync(path.join(process.env.POST_MAIL_ROOT, room, "inbox"), { recursive: true });',
    "process.exit(0);",
    "",
  ].join("\n"),
  { mode: 0o755 }
);

test.after(() => {
  // ROOT is a uniquely named temp dir this test created; plain stdlib
  // removal is the portable cleanup, no external binary involved.
  fs.rmSync(ROOT, { recursive: true, force: true });
});

let counter = 0;
function freshTarget(content) {
  const file = path.join(ROOT, `hooks-${counter++}.json`);
  if (content !== undefined) fs.writeFileSync(file, content);
  return file;
}

function run(target, { bin = GOOD_POST, extra = [] } = {}) {
  const args = [INSTALLER];
  if (target !== undefined) args.push(target);
  args.push(...extra);
  return spawnSync(process.execPath, args, {
    encoding: "utf8",
    env: {
      ...process.env,
      POST_CURSOR_HOOK_INSTALL_DIR: INSTALL_DIR,
      POST_CURSOR_HOOK_BIN: bin,
    },
  });
}

test("refuses to run without an explicit target", () => {
  const result = run(undefined);
  assert.equal(result.status, 2);
  assert.match(result.stderr, /usage/);
});

test("-h and --help print usage on stdout and exit 0 before any validation", () => {
  const target = freshTarget();
  // A binary that cannot run: if help reached the preflight, this would fail.
  const missing = path.join(ROOT, "help-no-such-binary");
  for (const [first, ...extra] of [["--help"], ["-h"], [target, "--help"]]) {
    const result = run(first, { bin: missing, extra });
    assert.equal(result.status, 0, `${[first, ...extra].join(" ")}: ${result.stderr}`);
    assert.match(result.stdout, /usage: node install-cursor-hooks\.mjs <path-to-hooks\.json>/);
    assert.equal(result.stderr, "");
  }
  assert.ok(!fs.existsSync(target), "help must not write the hooks file");
});

test("preflight refuses a stale binary that mints unroomed mailboxes, touching nothing", () => {
  const target = freshTarget();
  const result = run(target, { bin: STALE_POST });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /mints a mailbox/);
  assert.ok(!fs.existsSync(target), "a failed preflight must not write the hooks file");
  assert.ok(!fs.existsSync(ADAPTER), "a failed preflight must not copy the adapter");
  assert.ok(!fs.existsSync(CORE), "a failed preflight must not copy the shared core");
});

test("preflight refuses an unrunnable binary", () => {
  const target = freshTarget();
  const result = run(target, { bin: path.join(ROOT, "no-such-binary") });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /could not run the post binary/);
  assert.ok(!fs.existsSync(target));
  assert.ok(!fs.existsSync(ADAPTER));
  assert.ok(!fs.existsSync(CORE));
});

test("creates a fresh hooks file with all three events and copies the adapter", () => {
  const target = freshTarget();
  const result = run(target);
  assert.equal(result.status, 0, result.stderr);
  const expectedCommand = `${JSON.stringify(NODE_BIN)} ${JSON.stringify(ADAPTER)}`;
  const config = JSON.parse(fs.readFileSync(target, "utf8"));
  assert.equal(config.version, 1);
  for (const event of ["sessionStart", "beforeSubmitPrompt", "postToolUse"]) {
    assert.deepEqual(config.hooks[event], [{ command: expectedCommand, timeout: 10 }], event);
  }
  assert.deepEqual(
    fs.readFileSync(ADAPTER, "utf8"),
    fs.readFileSync(SOURCE, "utf8"),
    "adapter copy must match the installer's own source"
  );
  assert.equal(fs.statSync(ADAPTER).mode & 0o777, 0o755);
  assert.ok(fs.existsSync(NOTICE), "watch-notice copy must exist");
  assert.equal(fs.statSync(NOTICE).mode & 0o777, 0o755);
});

test("installs the shared core beside the adapter, and the installed adapter runs on its own", () => {
  const target = freshTarget();
  assert.equal(run(target).status, 0);
  assert.deepEqual(fs.readFileSync(CORE), fs.readFileSync(CORE_SOURCE));
  assert.equal(fs.statSync(CORE).mode & 0o777, 0o644);
  // The adapter imports ./mail-hook-core.mjs; a missing core would crash it
  // before it printed anything.
  const ran = spawnSync(process.execPath, [ADAPTER], { input: "{}", encoding: "utf8" });
  assert.equal(ran.status, 0, ran.stderr);
  assert.equal(ran.stdout, "{}");
  // A stale or damaged core is replaced on re-run.
  fs.writeFileSync(CORE, "// stale\n");
  const rerun = run(target);
  assert.equal(rerun.status, 0, rerun.stderr);
  assert.match(rerun.stdout, /adapter updated/);
  assert.deepEqual(fs.readFileSync(CORE), fs.readFileSync(CORE_SOURCE));
});

test("copies the adapter privately and writes through hook-config symlinks", () => {
  const realHooks = path.join(ROOT, "real-hooks.json");
  const symlinkHooks = path.join(ROOT, "profile-hooks.json");
  fs.writeFileSync(
    realHooks,
    JSON.stringify({ version: 1, hooks: { sessionEnd: [{ command: "audit.sh sessionEnd" }] } })
  );
  fs.symlinkSync(realHooks, symlinkHooks);

  const result = run(symlinkHooks);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(fs.lstatSync(symlinkHooks).isSymbolicLink(), true, "profile symlink must survive");

  const expectedCommand = `${JSON.stringify(NODE_BIN)} ${JSON.stringify(ADAPTER)}`;
  const config = JSON.parse(fs.readFileSync(realHooks, "utf8"));
  assert.deepEqual(config.hooks.sessionEnd, [{ command: "audit.sh sessionEnd" }]);
  for (const event of ["sessionStart", "beforeSubmitPrompt", "postToolUse"]) {
    assert.equal(config.hooks[event][0].command, expectedCommand);
  }
  const hooksBefore = fs.readFileSync(realHooks);
  const adapterBefore = fs.readFileSync(ADAPTER);
  const hooksMtime = fs.statSync(realHooks, { bigint: true }).mtimeNs;
  const adapterMtime = fs.statSync(ADAPTER, { bigint: true }).mtimeNs;

  const again = run(realHooks);
  assert.equal(again.status, 0, again.stderr);
  assert.match(again.stdout, /already registered/);
  assert.deepEqual(fs.readFileSync(realHooks), hooksBefore);
  assert.deepEqual(fs.readFileSync(ADAPTER), adapterBefore);
  assert.equal(fs.statSync(realHooks, { bigint: true }).mtimeNs, hooksMtime);
  assert.equal(fs.statSync(ADAPTER, { bigint: true }).mtimeNs, adapterMtime);
});

test("normalizes and deduplicates only its own hooks", () => {
  const target = freshTarget();
  const unrelated = { command: "/Users/example/.cursor/hooks/audit.sh sessionStart" };
  const expectedCommand = `${JSON.stringify(NODE_BIN)} ${JSON.stringify(ADAPTER)}`;
  fs.writeFileSync(
    target,
    JSON.stringify({
      version: 1,
      hooks: {
        sessionStart: [
          { command: "node /old/cursor-mail.mjs", timeout: 99 },
          unrelated,
          { command: "node /other/post-cursor-mail.mjs", timeout: 1 },
          { command: expectedCommand, timeout: 1 },
          { command: "echo cursor-mail.mjs", timeout: 1 },
        ],
      },
    })
  );

  const result = run(target);
  assert.equal(result.status, 0, result.stderr);

  const config = JSON.parse(fs.readFileSync(target, "utf8"));
  const hooks = config.hooks.sessionStart;
  const installed = hooks.filter(
    (hook) =>
      String(hook.command ?? "").includes("cursor-mail.mjs") &&
      !String(hook.command ?? "").startsWith("echo ")
  );
  assert.deepEqual(installed, [{ command: expectedCommand, timeout: 10 }]);
  assert.ok(hooks.some((hook) => hook.command === unrelated.command), "unrelated hook survives");
  assert.ok(hooks.some((hook) => hook.command === "echo cursor-mail.mjs"));
});

test("refuses to replace a dangling hook-config symlink", () => {
  const target = path.join(ROOT, "dangling-hooks.json");
  fs.symlinkSync(path.join(ROOT, "missing-hooks.json"), target);

  const result = run(target);
  assert.notEqual(result.status, 0);
  assert.equal(fs.lstatSync(target).isSymbolicLink(), true);
});

test("malformed target JSON fails before copying the adapter", () => {
  const target = freshTarget("{not-json");
  const installDir = path.join(ROOT, "hooks-malformed-json");
  const adapter = path.join(installDir, "post-cursor-mail.mjs");
  const result = spawnSync(process.execPath, [INSTALLER, target], {
    encoding: "utf8",
    env: {
      ...process.env,
      POST_CURSOR_HOOK_INSTALL_DIR: installDir,
      POST_CURSOR_HOOK_BIN: GOOD_POST,
    },
  });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /not valid JSON/);
  assert.ok(!fs.existsSync(adapter), "adapter must not be copied on malformed JSON");
  assert.ok(!fs.existsSync(path.join(installDir, "mail-hook-core.mjs")), "core must not be copied on malformed JSON");
  assert.equal(fs.readFileSync(target, "utf8"), "{not-json");
});

test("root array or null config normalizes to an object hooks map", () => {
  for (const [label, content] of [
    ["array", "[]"],
    ["null", "null"],
    ["scalar", "42"],
    ["hooks array", JSON.stringify({ hooks: [] })],
  ]) {
    const target = freshTarget(content);
    const result = run(target);
    assert.equal(result.status, 0, `${label}: ${result.stderr}`);
    const config = JSON.parse(fs.readFileSync(target, "utf8"));
    assert.equal(typeof config.hooks, "object");
    assert.ok(!Array.isArray(config.hooks), label);
    assert.ok(Array.isArray(config.hooks.sessionStart), label);
    assert.equal(
      config.hooks.sessionStart[0].command,
      `${JSON.stringify(NODE_BIN)} ${JSON.stringify(ADAPTER)}`
    );
  }
});

test("atomic writes refuse a planted predictable legacy temp symlink", () => {
  const target = freshTarget(JSON.stringify({ hooks: {} }));
  fs.mkdirSync(INSTALL_DIR, { recursive: true });
  const victim = path.join(INSTALL_DIR, "victim-secret.mjs");
  fs.writeFileSync(victim, "keep-me\n");
  fs.symlinkSync(victim, `${ADAPTER}.${process.pid}.tmp`);

  const result = run(target);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(fs.readFileSync(victim, "utf8"), "keep-me\n");
  assert.deepEqual(fs.readFileSync(ADAPTER, "utf8"), fs.readFileSync(SOURCE, "utf8"));
});

test("installer and installed adapter no longer reference identity-card.mjs", () => {
  const target = freshTarget();
  const result = run(target);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(fs.existsSync(path.join(INSTALL_DIR, "identity-card.mjs")), false);
  assert.ok(!fs.readFileSync(path.join(INSTALL_DIR, path.basename(ADAPTER)), "utf8").includes("identity-card.mjs"));
  assert.ok(!fs.readFileSync(INSTALLER, "utf8").includes("identity-card.mjs"));
});
