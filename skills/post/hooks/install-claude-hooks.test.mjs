// Self-tests for install-claude-hooks.mjs. Run: node --test skills/post/hooks/*.test.mjs
// Hermetic: POST_CLAUDE_HOOK_INSTALL_DIR keeps the adapter copy inside the
// temp root; no live config or home directory is touched. Cleanup uses `trash`.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const INSTALLER = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  "install-claude-hooks.mjs"
);
const ROOT = fs.mkdtempSync(path.join(os.tmpdir(), "post-claude-install-test-"));
const INSTALL_DIR = path.join(ROOT, "hooks");
const ADAPTER = path.join(INSTALL_DIR, "post-claude-mail.mjs");

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
function freshSettings(content) {
  const file = path.join(ROOT, `settings-${counter++}.json`);
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
      POST_CLAUDE_HOOK_INSTALL_DIR: INSTALL_DIR,
      POST_CLAUDE_HOOK_BIN: bin,
    },
  });
}

test("refuses to run without an explicit target", () => {
  const result = run(undefined);
  assert.equal(result.status, 2);
  assert.match(result.stderr, /usage/);
});

test("preflight refuses a stale binary that mints unroomed mailboxes, touching nothing", () => {
  const target = freshSettings();
  const result = run(target, { bin: STALE_POST });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /mints a mailbox/);
  assert.ok(!fs.existsSync(target), "a failed preflight must not write the settings file");
});

test("preflight refuses an unrunnable binary", () => {
  const target = freshSettings();
  const result = run(target, { bin: path.join(ROOT, "no-such-binary") });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /could not run the post binary/);
  assert.ok(!fs.existsSync(target));
});

test("creates a fresh settings file with all three events and copies the adapter", () => {
  const target = freshSettings();
  const result = run(target);
  assert.equal(result.status, 0, result.stderr);
  const config = JSON.parse(fs.readFileSync(target, "utf8"));
  for (const event of ["SessionStart", "UserPromptSubmit", "PostToolUse"]) {
    const groups = config.hooks[event];
    assert.equal(groups.length, 1, event);
    assert.deepEqual(groups[0].hooks, [
      { type: "command", command: "node", args: [ADAPTER], timeout: 10 },
    ]);
    assert.ok(!("matcher" in groups[0]), `${event} must not carry a matcher`);
  }
  assert.ok(fs.existsSync(ADAPTER), "adapter copy must exist");
  assert.equal(fs.statSync(ADAPTER).mode & 0o777, 0o755);
});

test("is idempotent and preserves unrelated hooks byte-identical", () => {
  const unrelated = {
    hooks: {
      SessionStart: [
        {
          matcher: "startup",
          hooks: [{ type: "command", command: "python3 /x/hydrate.py", timeout: 30 }],
        },
      ],
      PreToolUse: [{ hooks: [{ type: "command", command: "guard.sh" }] }],
    },
    permissions: { allow: ["Bash(ls:*)"] },
  };
  const target = freshSettings(JSON.stringify(unrelated, null, 2));
  assert.equal(run(target).status, 0);
  const after = JSON.parse(fs.readFileSync(target, "utf8"));
  assert.deepEqual(after.hooks.SessionStart[0], unrelated.hooks.SessionStart[0]);
  assert.deepEqual(after.hooks.PreToolUse, unrelated.hooks.PreToolUse);
  assert.deepEqual(after.permissions, unrelated.permissions);
  assert.equal(after.hooks.SessionStart.length, 2);

  const once = fs.readFileSync(target, "utf8");
  const rerun = run(target);
  assert.equal(rerun.status, 0);
  assert.match(rerun.stdout, /already registered/);
  assert.equal(fs.readFileSync(target, "utf8"), once, "second run must not change bytes");
});

test("updates a stale registration in place instead of duplicating", () => {
  const stale = {
    hooks: {
      UserPromptSubmit: [
        {
          hooks: [
            { type: "command", command: "node /old/place/post-claude-mail.mjs", timeout: 5 },
            { type: "command", command: "other-tool" },
          ],
        },
      ],
    },
  };
  const target = freshSettings(JSON.stringify(stale));
  assert.equal(run(target).status, 0);
  const after = JSON.parse(fs.readFileSync(target, "utf8"));
  const flat = after.hooks.UserPromptSubmit.flatMap((g) => g.hooks);
  const ours = flat.filter(
    (h) => JSON.stringify(h).includes("post-claude-mail.mjs")
  );
  assert.equal(ours.length, 1, "exactly one registration after migrate");
  assert.deepEqual(ours[0].args, [ADAPTER]);
  assert.ok(flat.some((h) => h.command === "other-tool"), "sibling hook preserved");
});
