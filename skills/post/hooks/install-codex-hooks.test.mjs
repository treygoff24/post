// Self-tests for install-codex-hooks.mjs. Run: node --test skills/post/hooks/*.test.mjs

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const INSTALLER = path.join(path.dirname(fileURLToPath(import.meta.url)), "install-codex-hooks.mjs");
const ROOT = fs.mkdtempSync(path.join(os.tmpdir(), "post-codex-install-test-"));

test.after(() => {
  spawnSync("trash", [ROOT], { stdio: "ignore" });
});

test("installer copies adapter privately and writes through hook-config symlinks", () => {
  const home = path.join(ROOT, "home");
  const sourceDir = path.join(home, ".codex", "skills", "post", "hooks");
  fs.mkdirSync(sourceDir, { recursive: true });
  fs.writeFileSync(path.join(sourceDir, "codex-mail.mjs"), "# source adapter\n");

  const realHooks = path.join(ROOT, "real-hooks.json");
  const symlinkHooks = path.join(ROOT, "profile-hooks.json");
  fs.writeFileSync(realHooks, JSON.stringify({ hooks: { Stop: [{ hooks: [] }] } }));
  fs.symlinkSync(realHooks, symlinkHooks);

  const result = spawnSync(process.execPath, [INSTALLER, symlinkHooks], {
    encoding: "utf8",
    env: { ...process.env, HOME: home },
  });
  assert.equal(result.status, 0, result.stderr);
  assert.equal(fs.lstatSync(symlinkHooks).isSymbolicLink(), true, "profile symlink must survive");

  const copied = path.join(home, ".codex", "hooks", "post-codex-mail.mjs");
  assert.equal(fs.readFileSync(copied, "utf8"), "# source adapter\n");
  const config = JSON.parse(fs.readFileSync(realHooks, "utf8"));
  assert.deepEqual(config.hooks.Stop, [{ hooks: [] }], "unrelated hooks are preserved");
  for (const event of ["SessionStart", "UserPromptSubmit", "PostToolUse"]) {
    assert.equal(
      config.hooks[event][0].hooks[0].command,
      `node ${JSON.stringify(copied)}`
    );
  }
  const hooksBefore = fs.readFileSync(realHooks);
  const adapterBefore = fs.readFileSync(copied);
  const hooksMtime = fs.statSync(realHooks, { bigint: true }).mtimeNs;
  const adapterMtime = fs.statSync(copied, { bigint: true }).mtimeNs;

  const again = spawnSync(process.execPath, [INSTALLER, realHooks], {
    encoding: "utf8",
    env: { ...process.env, HOME: home },
  });
  assert.equal(again.status, 0, again.stderr);
  assert.match(again.stdout, /already registered/);
  assert.deepEqual(fs.readFileSync(realHooks), hooksBefore);
  assert.deepEqual(fs.readFileSync(copied), adapterBefore);
  assert.equal(fs.statSync(realHooks, { bigint: true }).mtimeNs, hooksMtime);
  assert.equal(fs.statSync(copied, { bigint: true }).mtimeNs, adapterMtime);
});

test("installer normalizes and deduplicates only its own hooks", () => {
  const home = path.join(ROOT, "dedupe-home");
  const sourceDir = path.join(home, ".codex", "skills", "post", "hooks");
  fs.mkdirSync(sourceDir, { recursive: true });
  fs.writeFileSync(path.join(sourceDir, "codex-mail.mjs"), "# adapter\n");
  const target = path.join(ROOT, "dedupe-hooks.json");
  const unrelated = { type: "command", command: "echo keep", timeout: 9 };
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
              { type: "command", command: "echo codex-mail.mjs", timeout: 1 },
            ],
          },
        ],
      },
    })
  );

  const result = spawnSync(process.execPath, [INSTALLER, target], {
    encoding: "utf8",
    env: { ...process.env, HOME: home },
  });
  assert.equal(result.status, 0, result.stderr);

  const config = JSON.parse(fs.readFileSync(target, "utf8"));
  const hooks = config.hooks.SessionStart.flatMap((group) => group.hooks ?? []);
  const installed = hooks.filter(
    (hook) =>
      String(hook.command ?? "").startsWith("node ") &&
      String(hook.command ?? "").includes("codex-mail.mjs")
  );
  assert.deepEqual(installed, [
    {
      type: "command",
      command: `node ${JSON.stringify(path.join(home, ".codex", "hooks", "post-codex-mail.mjs"))}`,
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

test("installer refuses to replace a dangling hook-config symlink", () => {
  const home = path.join(ROOT, "dangling-home");
  const sourceDir = path.join(home, ".codex", "skills", "post", "hooks");
  fs.mkdirSync(sourceDir, { recursive: true });
  fs.writeFileSync(path.join(sourceDir, "codex-mail.mjs"), "# adapter\n");
  const target = path.join(ROOT, "dangling-hooks.json");
  fs.symlinkSync(path.join(ROOT, "missing-hooks.json"), target);

  const result = spawnSync(process.execPath, [INSTALLER, target], {
    encoding: "utf8",
    env: { ...process.env, HOME: home },
  });
  assert.notEqual(result.status, 0);
  assert.equal(fs.lstatSync(target).isSymbolicLink(), true);
});
