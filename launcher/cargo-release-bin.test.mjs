import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const resolver = fileURLToPath(new URL("../scripts/cargo-release-bin.mjs", import.meta.url));

// These tests ask Cargo a question — where does the target directory come
// from? — and only Cargo may answer it. On boxes where `cargo` on PATH is the
// estate build shim, that command is not Cargo: it injects
// `--config build.target-dir=<managed>` and refuses any target outside its
// managed root, so both results below are the shim's policy, not Cargo's
// precedence. Build a metadata-only lane around a Cargo that answers, and keep
// the production resolver untouched.
function metadataTargetDirectory(cargoBin, cwd, targetDir) {
  const env = { ...process.env };
  delete env.CARGO_TARGET_DIR;
  if (targetDir !== undefined) env.CARGO_TARGET_DIR = targetDir;
  const result = spawnSync(cargoBin, ["metadata", "--no-deps", "--format-version", "1"], {
    cwd,
    env,
    encoding: "utf8",
  });
  if (result.status !== 0) return null;
  try {
    const metadata = JSON.parse(result.stdout);
    return typeof metadata.target_directory === "string" ? metadata.target_directory : null;
  } catch {
    return null;
  }
}

function scratchCrate() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "post-cargo-probe-"));
  const crate = path.join(root, "crate");
  fs.mkdirSync(path.join(crate, "src"), { recursive: true });
  fs.writeFileSync(path.join(crate, "Cargo.toml"), '[package]\nname="probe"\nversion="0.1.0"\nedition="2021"\n');
  fs.writeFileSync(path.join(crate, "src/main.rs"), "fn main() {}\n");
  return { root, crate };
}

// A candidate qualifies only by behavior: it must report the target directory
// it was handed. The shim rewrites that value, and a wrapper that only pretends
// to be Cargo would fail the same probe.
function honorsTargetDirectory(candidate) {
  const { root, crate } = scratchCrate();
  try {
    const chosen = path.join(root, "chosen target");
    const reported = metadataTargetDirectory(candidate, crate, chosen);
    return reported === chosen;
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
}

function cargoCandidates() {
  const candidates = [];
  if (process.env.POST_REAL_CARGO) candidates.push(process.env.POST_REAL_CARGO);
  if (process.env.CARGO_HOME) candidates.push(path.join(process.env.CARGO_HOME, "bin", "cargo"));
  candidates.push(path.join(os.homedir(), ".cargo", "bin", "cargo"));
  for (const directory of (process.env.PATH ?? "").split(path.delimiter)) {
    if (directory) candidates.push(path.join(directory, "cargo"));
  }
  const seen = new Set();
  return candidates.filter((candidate) => {
    let key;
    try {
      key = fs.realpathSync(candidate);
    } catch {
      return false;
    }
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

let realCargoCache;
function realCargo() {
  if (realCargoCache === undefined) {
    realCargoCache = cargoCandidates().find(honorsTargetDirectory) ?? null;
  }
  return realCargoCache;
}

// The resolver execs bare `cargo`, so the lane is a PATH whose first entry is a
// directory holding this `cargo` and nothing else. Cargo's own resolution of
// rustup/toolchain shims is preserved because the link points at the candidate
// exactly as it was found.
function cargoLane(binDir) {
  fs.mkdirSync(binDir, { recursive: true });
  const link = path.join(binDir, "cargo");
  if (!fs.existsSync(link)) fs.symlinkSync(realCargo(), link);
  return binDir;
}

function checkResolution(useOverride) {
  const cargo = realCargo();
  assert.ok(
    cargo,
    "no Cargo that honors build.target-dir on PATH; set POST_REAL_CARGO to a metadata-only Cargo to run the target-directory precedence tests",
  );
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
    env.PATH = `${cargoLane(path.join(root, "bin"))}${path.delimiter}${env.PATH ?? ""}`;
    const result = spawnSync(process.execPath, [resolver], { cwd: crate, env, encoding: "utf8" });
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stdout.trim(), path.join(override ?? configured, "release", "post"));
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
}

test("artifact resolution honors Cargo config target-dir", () => checkResolution(false));
test("artifact resolution honors explicit CARGO_TARGET_DIR", () => checkResolution(true));
