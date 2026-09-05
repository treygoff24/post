import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const resolver = fileURLToPath(new URL("../scripts/cargo-release-bin.mjs", import.meta.url));

function checkResolution(useOverride) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "post-artifact-test-"));
  try {
    const crate = path.join(root, "crate");
    fs.mkdirSync(path.join(crate, ".cargo"), { recursive: true });
    fs.mkdirSync(path.join(crate, "src"));
    fs.writeFileSync(path.join(crate, "Cargo.toml"), '[package]\nname="post"\nversion="0.1.0"\nedition="2021"\n');
    fs.writeFileSync(path.join(crate, "src/main.rs"), "fn main() {}\n");
    const configured = path.join(root, "configured target");
    fs.writeFileSync(path.join(crate, ".cargo/config.toml"), `[build]\ntarget-dir=${JSON.stringify(configured)}\n`);
    const override = useOverride ? path.join(root, "explicit target") : undefined;
    const env = { ...process.env };
    delete env.CARGO_TARGET_DIR;
    if (override) env.CARGO_TARGET_DIR = override;
    const result = spawnSync(process.execPath, [resolver], { cwd: crate, env, encoding: "utf8" });
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stdout.trim(), path.join(override ?? configured, "release", "post"));
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
}

test("artifact resolution honors Cargo config target-dir", () => checkResolution(false));
test("artifact resolution honors explicit CARGO_TARGET_DIR", () => checkResolution(true));
