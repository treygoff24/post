// Self-tests for install-claude-hooks.mjs. Run: node --test skills/post/hooks/*.test.mjs
// Hermetic: POST_CLAUDE_HOOK_INSTALL_DIR keeps the adapter copy inside the
// temp root; no live config or home directory is touched.

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
const CORE_SOURCE = path.join(path.dirname(INSTALLER), "mail-hook-core.mjs");
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
function freshSettings(content) {
  const file = path.join(ROOT, `settings-${counter++}.json`);
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

test("-h and --help print usage on stdout and exit 0 before any validation", () => {
  const target = freshSettings();
  // A binary that cannot run: if help reached the preflight, this would fail.
  const missing = path.join(ROOT, "help-no-such-binary");
  for (const [first, ...extra] of [["--help"], ["-h"], [target, "--help"]]) {
    const result = run(first, { bin: missing, extra });
    assert.equal(result.status, 0, `${[first, ...extra].join(" ")}: ${result.stderr}`);
    assert.match(result.stdout, /usage: node install-claude-hooks\.mjs <path-to-settings\.json>/);
    assert.equal(result.stderr, "");
  }
  assert.ok(!fs.existsSync(target), "help must not write the settings file");
});

test("preflight refuses a stale binary that mints unroomed mailboxes, touching nothing", () => {
  const target = freshSettings();
  const result = run(target, { bin: STALE_POST });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /mints a mailbox/);
  assert.ok(!fs.existsSync(target), "a failed preflight must not write the settings file");
  assert.ok(!fs.existsSync(CORE), "a failed preflight must not copy the shared core");
});

test("preflight refuses an unrunnable binary", () => {
  const target = freshSettings();
  const result = run(target, { bin: path.join(ROOT, "no-such-binary") });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /could not run the post binary/);
  assert.ok(!fs.existsSync(target));
});

test("creates a fresh settings file with lifecycle events and copies the adapter", () => {
  const target = freshSettings();
  const result = run(target);
  assert.equal(result.status, 0, result.stderr);
  const config = JSON.parse(fs.readFileSync(target, "utf8"));
  for (const event of ["SessionStart", "UserPromptSubmit", "PostToolUse", "SessionEnd"]) {
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

test("installs the shared core beside the adapter, and the installed adapter runs on its own", () => {
  const target = freshSettings();
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

test("a malformed settings file leaves neither the adapter nor the core behind", () => {
  const dir = path.join(ROOT, "hooks-malformed");
  const target = freshSettings("{not-json");
  const result = spawnSync(process.execPath, [INSTALLER, target], {
    encoding: "utf8",
    env: { ...process.env, POST_CLAUDE_HOOK_INSTALL_DIR: dir, POST_CLAUDE_HOOK_BIN: GOOD_POST },
  });
  assert.notEqual(result.status, 0);
  assert.ok(!fs.existsSync(path.join(dir, "post-claude-mail.mjs")));
  assert.ok(!fs.existsSync(path.join(dir, "mail-hook-core.mjs")));
});

// Everything is staged as temp files and renamed into place only when every
// write has succeeded. The settings directory here allows reading but not
// creating a file, so the settings write is the step that fails, after the core
// and adapter have been staged.
test("a failure writing the settings leaves the installed files exactly as they were", { skip: process.getuid?.() === 0 && "root ignores directory permissions" }, () => {
  const dir = path.join(ROOT, "hooks-staged");
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(path.join(dir, "mail-hook-core.mjs"), "// old core\n", { mode: 0o644 });
  fs.writeFileSync(path.join(dir, "post-claude-mail.mjs"), "// old adapter\n", { mode: 0o755 });
  const before = fs.readdirSync(dir).sort();
  const settingsDir = path.join(ROOT, "settings-readonly");
  fs.mkdirSync(settingsDir);
  const target = path.join(settingsDir, "settings.json");
  fs.writeFileSync(target, JSON.stringify({ hooks: {} }));
  const env = { ...process.env, POST_CLAUDE_HOOK_INSTALL_DIR: dir, POST_CLAUDE_HOOK_BIN: GOOD_POST };
  fs.chmodSync(settingsDir, 0o500);
  try {
    const failed = spawnSync(process.execPath, [INSTALLER, target], { encoding: "utf8", env });
    assert.notEqual(failed.status, 0, "the install must fail");
    assert.equal(fs.readFileSync(path.join(dir, "mail-hook-core.mjs"), "utf8"), "// old core\n");
    assert.equal(fs.readFileSync(path.join(dir, "post-claude-mail.mjs"), "utf8"), "// old adapter\n");
    assert.deepEqual(fs.readdirSync(dir).sort(), before, "no staged temp files are left behind");
    assert.equal(fs.readFileSync(target, "utf8"), JSON.stringify({ hooks: {} }));
  } finally {
    fs.chmodSync(settingsDir, 0o700);
  }
  const rerun = spawnSync(process.execPath, [INSTALLER, target], { encoding: "utf8", env });
  assert.equal(rerun.status, 0, rerun.stderr);
  assert.deepEqual(fs.readFileSync(path.join(dir, "mail-hook-core.mjs")), fs.readFileSync(CORE_SOURCE));
  assert.ok(JSON.parse(fs.readFileSync(target, "utf8")).hooks.SessionStart);
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

test("installer and installed adapter no longer reference identity-card.mjs", () => {
  const target = freshSettings();
  const result = run(target);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(fs.existsSync(path.join(INSTALL_DIR, "identity-card.mjs")), false);
  assert.ok(!fs.readFileSync(path.join(INSTALL_DIR, path.basename(ADAPTER)), "utf8").includes("identity-card.mjs"));
  assert.ok(!fs.readFileSync(INSTALLER, "utf8").includes("identity-card.mjs"));
});
