// Self-tests for stable-node-path.mjs. Run: node --test skills/post/hooks/*.test.mjs
// Hermetic: every fixture lives in a temp root; the one test that touches the
// real interpreter only asserts an invariant that holds on any machine.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { stableNodePath, STABLE_CANDIDATES } from "./stable-node-path.mjs";

const ROOT = fs.mkdtempSync(path.join(os.tmpdir(), "stable-node-path-test-"));
process.on("exit", () => fs.rmSync(ROOT, { recursive: true, force: true }));

function fixture(name) {
  const dir = path.join(ROOT, name);
  const cellar = path.join(dir, "Cellar", "node", "1.2.3", "bin");
  const stable = path.join(dir, "bin");
  fs.mkdirSync(cellar, { recursive: true });
  fs.mkdirSync(stable, { recursive: true });
  const real = path.join(cellar, "node");
  fs.writeFileSync(real, "#!/bin/sh\nexit 0\n");
  fs.chmodSync(real, 0o755);
  return { dir, real, alias: path.join(stable, "node") };
}

test("prefers a stable alias that points at the same binary", () => {
  const { real, alias } = fixture("same");
  fs.symlinkSync(real, alias);
  assert.equal(stableNodePath(real, [alias]), alias);
});

test("NEVER substitutes a same-named binary that is a different file", () => {
  // The safety property: fnm/nvm/asdf machines have several real `node`
  // binaries of different versions. Redirecting a hook to the wrong one would
  // be worse than the version-pinning bug this helper exists to fix.
  const { real, alias } = fixture("different");
  fs.writeFileSync(alias, "#!/bin/sh\nexit 1\n");
  fs.chmodSync(alias, 0o755);
  assert.equal(stableNodePath(real, [alias]), real);
});

test("falls back to execPath when no candidate exists", () => {
  const { real, alias } = fixture("absent");
  assert.equal(fs.existsSync(alias), false);
  assert.equal(stableNodePath(real, [alias]), real);
});

test("skips a candidate that is not executable", () => {
  const { real, alias } = fixture("noexec");
  fs.symlinkSync(real, alias);
  fs.chmodSync(real, 0o644);
  assert.equal(stableNodePath(real, [alias]), real);
});

test("returns the candidate unchanged when execPath already is it", () => {
  const { alias } = fixture("identity");
  assert.equal(stableNodePath(alias, [alias]), alias);
});

test("returns execPath untouched when it cannot be resolved", () => {
  const missing = path.join(ROOT, "nope", "node");
  assert.equal(stableNodePath(missing, STABLE_CANDIDATES), missing);
});

test("honours candidate preference order", () => {
  const { real } = fixture("order");
  const first = path.join(ROOT, "order", "first-node");
  const second = path.join(ROOT, "order", "second-node");
  fs.symlinkSync(real, first);
  fs.symlinkSync(real, second);
  assert.equal(stableNodePath(real, [first, second]), first);
  assert.equal(stableNodePath(real, [second, first]), second);
});

test("on this machine, the result is the running interpreter by another name", () => {
  // Machine-independent invariant: whatever we pick, it must be the SAME
  // binary that is executing this test, and it must be runnable.
  const resolved = stableNodePath();
  assert.equal(fs.realpathSync(resolved), fs.realpathSync(process.execPath));
  fs.accessSync(resolved, fs.constants.X_OK);
  assert.equal(path.isAbsolute(resolved), true);
});
