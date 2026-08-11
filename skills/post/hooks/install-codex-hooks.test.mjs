// Self-tests for install-codex-hooks.mjs. Run: node --test skills/post/hooks/*.test.mjs
// Hermetic: POST_CODEX_HOOK_INSTALL_DIR keeps the adapter copy inside the temp
// root and POST_CODEX_HOOK_BIN drives the preflight probe; no live config or
// home directory is touched. The adapter source always comes from this
// installer's own directory (the checked-in codex-mail.mjs). Cleanup: `trash`.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const DIR = path.dirname(fileURLToPath(import.meta.url));
const INSTALLER = path.join(DIR, "install-codex-hooks.mjs");
const SOURCE = path.join(DIR, "codex-mail.mjs");
const ROOT = fs.mkdtempSync(path.join(os.tmpdir(), "post-codex-install-test-"));
const INSTALL_DIR = path.join(ROOT, "hooks");
const ADAPTER = path.join(INSTALL_DIR, "post-codex-mail.mjs");

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
  const cleanup = spawnSync("trash", [ROOT], { stdio: "ignore" });
  // No `trash` on this machine (CI runners, stranger installs): remove the
  // temp root directly — this test created it.
  if (cleanup.error) fs.rmSync(ROOT, { recursive: true, force: true });
});

let counter = 0;
function freshTarget(content) {
  const file = path.join(ROOT, `hooks-${counter++}.json`);
  if (content !== undefined) fs.writeFileSync(file, content);
  return file;
}

function run(target, { bin = GOOD_POST } = {}) {
  const args = [INSTALLER];
  if (target !== undefined) args.push(target);
  return spawnSync(process.execPath, args, {
    encoding: "utf8",
    env: {
      ...process.env,
      POST_CODEX_HOOK_INSTALL_DIR: INSTALL_DIR,
      POST_CODEX_HOOK_BIN: bin,
    },
  });
}

test("refuses to run without an explicit target", () => {
  const result = run(undefined);
  assert.equal(result.status, 2);
  assert.match(result.stderr, /usage/);
});

test("preflight refuses a stale binary that mints unroomed mailboxes, touching nothing", () => {
  const target = freshTarget();
  const result = run(target, { bin: STALE_POST });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /mints a mailbox/);
  assert.ok(!fs.existsSync(target), "a failed preflight must not write the hooks file");
  assert.ok(!fs.existsSync(ADAPTER), "a failed preflight must not copy the adapter");
});

test("preflight refuses an unrunnable binary", () => {
  const target = freshTarget();
  const result = run(target, { bin: path.join(ROOT, "no-such-binary") });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /could not run the post binary/);
  assert.ok(!fs.existsSync(target));
  assert.ok(!fs.existsSync(ADAPTER));
});

test("creates a fresh hooks file with all three events and copies the adapter", () => {
  const target = freshTarget();
  const result = run(target);
  assert.equal(result.status, 0, result.stderr);
  const expectedCommand = `${JSON.stringify(process.execPath)} ${JSON.stringify(ADAPTER)}`;
  const config = JSON.parse(fs.readFileSync(target, "utf8"));
  for (const event of ["SessionStart", "UserPromptSubmit", "PostToolUse"]) {
    const groups = config.hooks[event];
    assert.equal(groups.length, 1, event);
    assert.deepEqual(groups[0].hooks, [
      { type: "command", command: expectedCommand, timeout: 5 },
    ]);
    assert.ok(!("matcher" in groups[0]), `${event} must not carry a matcher`);
  }
  assert.deepEqual(
    fs.readFileSync(ADAPTER, "utf8"),
    fs.readFileSync(SOURCE, "utf8"),
    "adapter copy must match the installer's own source"
  );
  assert.equal(fs.statSync(ADAPTER).mode & 0o777, 0o755);
});

test("copies the adapter privately and writes through hook-config symlinks", () => {
  const realHooks = path.join(ROOT, "real-hooks.json");
  const symlinkHooks = path.join(ROOT, "profile-hooks.json");
  fs.writeFileSync(realHooks, JSON.stringify({ hooks: { Stop: [{ hooks: [] }] } }));
  fs.symlinkSync(realHooks, symlinkHooks);

  const result = run(symlinkHooks);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(fs.lstatSync(symlinkHooks).isSymbolicLink(), true, "profile symlink must survive");

  const expectedCommand = `${JSON.stringify(process.execPath)} ${JSON.stringify(ADAPTER)}`;
  const config = JSON.parse(fs.readFileSync(realHooks, "utf8"));
  assert.deepEqual(config.hooks.Stop, [{ hooks: [] }], "unrelated hooks are preserved");
  for (const event of ["SessionStart", "UserPromptSubmit", "PostToolUse"]) {
    assert.equal(config.hooks[event][0].hooks[0].command, expectedCommand);
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
  const unrelated = { type: "command", command: "echo keep", timeout: 9 };
  const expectedCommand = `${JSON.stringify(process.execPath)} ${JSON.stringify(ADAPTER)}`;
  fs.writeFileSync(
    target,
    JSON.stringify({
      hooks: {
        SessionStart: [
          {
            matcher: "old-scope",
            hooks: [
              { type: "command", command: "node /old/codex-mail.mjs", timeout: 99, async: true },
              unrelated,
            ],
          },
          {
            hooks: [
              { type: "command", command: "node /other/post-codex-mail.mjs", timeout: 1 },
              {
                type: "command",
                command: expectedCommand,
                timeout: 1,
              },
              { type: "command", command: "echo codex-mail.mjs", timeout: 1 },
            ],
          },
        ],
      },
    })
  );

  const result = run(target);
  assert.equal(result.status, 0, result.stderr);

  const config = JSON.parse(fs.readFileSync(target, "utf8"));
  const hooks = config.hooks.SessionStart.flatMap((group) => group.hooks ?? []);
  const installed = hooks.filter(
    (hook) =>
      String(hook.command ?? "").includes("codex-mail.mjs") &&
      !String(hook.command ?? "").startsWith("echo ")
  );
  assert.deepEqual(installed, [
    {
      type: "command",
      command: expectedCommand,
      timeout: 5,
    },
  ]);
  assert.ok(hooks.some((hook) => hook.command === unrelated.command), "unrelated hook survives");
  assert.ok(hooks.some((hook) => hook.command === "echo codex-mail.mjs"));
  assert.deepEqual(config.hooks.SessionStart[0], {
    matcher: "old-scope",
    hooks: [unrelated],
  });
  assert.deepEqual(config.hooks.SessionStart.at(-1), { hooks: installed });
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
  const adapter = path.join(installDir, "post-codex-mail.mjs");
  const result = spawnSync(process.execPath, [INSTALLER, target], {
    encoding: "utf8",
    env: {
      ...process.env,
      POST_CODEX_HOOK_INSTALL_DIR: installDir,
      POST_CODEX_HOOK_BIN: GOOD_POST,
    },
  });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /not valid JSON/);
  assert.ok(!fs.existsSync(adapter), "adapter must not be copied on malformed JSON");
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
    assert.ok(Array.isArray(config.hooks.SessionStart), label);
    assert.equal(
      config.hooks.SessionStart[0].hooks[0].command,
      `${JSON.stringify(process.execPath)} ${JSON.stringify(ADAPTER)}`
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
