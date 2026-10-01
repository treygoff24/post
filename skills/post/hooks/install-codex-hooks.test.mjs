// Self-tests for install-codex-hooks.mjs. Run: node --test skills/post/hooks/*.test.mjs
// Hermetic: POST_CODEX_HOOK_INSTALL_DIR keeps the adapter copy inside the temp
// root and POST_CODEX_HOOK_BIN drives the preflight probe; no live config or
// home directory is touched. The adapter source always comes from this
// installer's own directory (the checked-in codex-mail.mjs).

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { execFileSync, spawnSync } from "node:child_process";

import { stableNodePath } from "./stable-node-path.mjs";
import { withoutSessionIdentity } from "./test-session-env.mjs";

// The child env never inherits the identity of the session running the tests.
const TEST_ENV = withoutSessionIdentity();

const DIR = path.dirname(fileURLToPath(import.meta.url));
// The installers pin a package-manager-stable alias for the running Node
// rather than the version-pinned process.execPath, so the expected command
// must be built the same way (see stable-node-path.mjs).
const NODE_BIN = stableNodePath();
const INSTALLER = path.join(DIR, "install-codex-hooks.mjs");
const SOURCE = path.join(DIR, "codex-mail.mjs");
const ROOT = fs.mkdtempSync(path.join(os.tmpdir(), "post-codex-install-test-"));
const INSTALL_DIR = path.join(ROOT, "hooks");
const ADAPTER = path.join(INSTALL_DIR, "post-codex-mail.mjs");
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
      ...TEST_ENV,
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

test("-h and --help print usage on stdout and exit 0 before any validation", () => {
  const target = freshTarget();
  // A binary that cannot run: if help reached the preflight, this would fail.
  const missing = path.join(ROOT, "help-no-such-binary");
  for (const [first, ...extra] of [["--help"], ["-h"], [target, "--help"]]) {
    const result = run(first, { bin: missing, extra });
    assert.equal(result.status, 0, `${[first, ...extra].join(" ")}: ${result.stderr}`);
    assert.match(result.stdout, /usage: node install-codex-hooks\.mjs <path-to-hooks\.json>/);
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
  fs.writeFileSync(realHooks, JSON.stringify({ hooks: { Stop: [{ hooks: [] }] } }));
  fs.symlinkSync(realHooks, symlinkHooks);

  const result = run(symlinkHooks);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(fs.lstatSync(symlinkHooks).isSymbolicLink(), true, "profile symlink must survive");

  const expectedCommand = `${JSON.stringify(NODE_BIN)} ${JSON.stringify(ADAPTER)}`;
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
  const expectedCommand = `${JSON.stringify(NODE_BIN)} ${JSON.stringify(ADAPTER)}`;
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
      ...TEST_ENV,
      POST_CODEX_HOOK_INSTALL_DIR: installDir,
      POST_CODEX_HOOK_BIN: GOOD_POST,
    },
  });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /not valid JSON/);
  assert.ok(!fs.existsSync(adapter), "adapter must not be copied on malformed JSON");
  assert.ok(!fs.existsSync(path.join(installDir, "mail-hook-core.mjs")), "core must not be copied on malformed JSON");
  assert.equal(fs.readFileSync(target, "utf8"), "{not-json");
});

// Everything is staged as temp files and renamed into place only when every
// write has succeeded. The config directory here allows reading but not
// creating a file, so the config write is the step that fails, after the core
// and adapter have been staged.
test("a failure writing the config leaves the installed files exactly as they were", { skip: process.getuid?.() === 0 && "root ignores directory permissions" }, () => {
  const dir = path.join(ROOT, "hooks-staged");
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(path.join(dir, "mail-hook-core.mjs"), "// old core\n", { mode: 0o644 });
  fs.writeFileSync(path.join(dir, "post-codex-mail.mjs"), "// old adapter\n", { mode: 0o755 });
  const before = fs.readdirSync(dir).sort();
  const configDir = path.join(ROOT, "config-readonly");
  fs.mkdirSync(configDir);
  const target = path.join(configDir, "hooks.json");
  fs.writeFileSync(target, JSON.stringify({ hooks: {} }));
  const env = { ...TEST_ENV, POST_CODEX_HOOK_INSTALL_DIR: dir, POST_CODEX_HOOK_BIN: GOOD_POST };
  fs.chmodSync(configDir, 0o500);
  try {
    const failed = spawnSync(process.execPath, [INSTALLER, target], { encoding: "utf8", env });
    assert.notEqual(failed.status, 0, "the install must fail");
    assert.equal(fs.readFileSync(path.join(dir, "mail-hook-core.mjs"), "utf8"), "// old core\n");
    assert.equal(fs.readFileSync(path.join(dir, "post-codex-mail.mjs"), "utf8"), "// old adapter\n");
    assert.deepEqual(fs.readdirSync(dir).sort(), before, "no staged temp files are left behind");
    assert.equal(fs.readFileSync(target, "utf8"), JSON.stringify({ hooks: {} }));
  } finally {
    fs.chmodSync(configDir, 0o700);
  }
  const rerun = spawnSync(process.execPath, [INSTALLER, target], { encoding: "utf8", env });
  assert.equal(rerun.status, 0, rerun.stderr);
  assert.deepEqual(fs.readFileSync(path.join(dir, "mail-hook-core.mjs")), fs.readFileSync(CORE_SOURCE));
  assert.ok(JSON.parse(fs.readFileSync(target, "utf8")).hooks.SessionStart);
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

test("installed adapter copy runs end-to-end with the release CLI", () => {
  const releaseBin = execFileSync(
    process.execPath,
    [path.join(DIR, "../../../scripts/cargo-release-bin.mjs")],
    { encoding: "utf8" }
  ).trim();
  assert.ok(fs.existsSync(releaseBin), `release binary missing: ${releaseBin}`);
  const runtimeRoot = fs.mkdtempSync(path.join(os.tmpdir(), "post-codex-installed-proof-"));
  try {
    const runtimeHome = path.join(runtimeRoot, "home");
    const runtimeInstallDir = path.join(runtimeHome, ".codex", "hooks");
    const target = path.join(runtimeRoot, "hooks.json");
    const installed = spawnSync(process.execPath, [INSTALLER, target], {
      encoding: "utf8",
      env: {
        ...TEST_ENV,
        POST_CODEX_HOOK_INSTALL_DIR: runtimeInstallDir,
        POST_CODEX_HOOK_BIN: releaseBin,
      },
    });
    assert.equal(installed.status, 0, installed.stderr);
    const installedAdapter = path.join(runtimeInstallDir, "post-codex-mail.mjs");
    assert.ok(fs.existsSync(installedAdapter));
    assert.ok(!fs.readFileSync(installedAdapter, "utf8").includes("identity-card.mjs"));
    const cwd = path.join(runtimeRoot, "workspace");
    const mailRoot = path.join(runtimeRoot, "mail");
    const stateDir = path.join(runtimeRoot, "state");
    fs.mkdirSync(cwd, { recursive: true });
    // Lazy minting: a session in a directory that is not a registered room is
    // not minted at start, so register the workspace the way an operator does.
    const registered = spawnSync(releaseBin, ["rooms", "add", "installed-proof", cwd], {
      encoding: "utf8",
      env: { PATH: process.env.PATH, HOME: runtimeHome, POST_MAIL_ROOT: mailRoot },
    });
    assert.equal(registered.status, 0, registered.stderr);
    assert.ok(fs.existsSync(path.join(runtimeInstallDir, "mail-hook-core.mjs")), "the installed adapter needs its core");
    const result = spawnSync(process.execPath, [installedAdapter], {
      input: JSON.stringify({ hook_event_name: "SessionStart", session_id: "installed-proof", cwd }),
      encoding: "utf8",
      env: {
        PATH: process.env.PATH,
        HOME: runtimeHome,
        POST_CODEX_HOOK_BIN: releaseBin,
        POST_CODEX_HOOK_STATE_DIR: stateDir,
        POST_MAIL_ROOT: mailRoot,
      },
    });
    assert.equal(result.status, 0, result.stderr);
    assert.match(JSON.parse(result.stdout).hookSpecificOutput.additionalContext, /Post connects you with other agents/);
    const participantDirs = fs.readdirSync(path.join(mailRoot, "participants"))
      .filter((entry) => entry.startsWith("codex-"));
    assert.equal(participantDirs.length, 1);
  } finally {
    fs.rmSync(runtimeRoot, { recursive: true, force: true });
  }
});

test("the emitted node path is upgrade-durable, not version-pinned", () => {
  // Regression guard for 2026-09-15. The installer used to bake process.execPath,
  // which on Homebrew is /opt/homebrew/Cellar/node/<version>/bin/node; a routine
  // `brew upgrade node` deleted that directory and every installed hook started
  // exiting 127 (command not found) at once, across Codex, Cursor and Grok.
  //
  // Asserting `emitted === stableNodePath()` would be circular — it would still
  // pass if the helper regressed to returning execPath. So assert the PROPERTY:
  // the emitted path must be a FIXED POINT of stableNodePath (i.e. no more
  // durable alias exists for that same binary), and must still resolve to the
  // interpreter actually running this test.
  const target = freshTarget();
  const result = run(target);
  assert.equal(result.status, 0, result.stderr);

  const config = JSON.parse(fs.readFileSync(target, "utf8"));
  const command = config.hooks.UserPromptSubmit[0].hooks[0].command;
  const emitted = JSON.parse(command.match(/^"(?:\\.|[^"\\])*"/)[0]);

  assert.equal(path.isAbsolute(emitted), true, "hook commands must pin an absolute node");
  assert.equal(
    stableNodePath(emitted),
    emitted,
    `installer emitted ${emitted}, but a more upgrade-durable alias exists for the same binary`
  );
  assert.equal(
    fs.realpathSync(emitted),
    fs.realpathSync(process.execPath),
    "the pinned path must be the same binary that is running these tests"
  );
});
