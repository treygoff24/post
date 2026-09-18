import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { cargoReleaseBin } from "../../../scripts/cargo-release-bin.mjs";

const repo = fileURLToPath(new URL("../../../", import.meta.url));
const binary = cargoReleaseBin(repo);
const notice = "Post connects you with other agents.";

for (const [harness, event] of [["codex", "SessionStart"], ["claude", "SessionStart"], ["cursor", "sessionStart"], ["grok", "UserPromptSubmit"]]) {
  test(`${harness}: overlapping hook events deliver exactly one activation notice`, async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), "post-activation-race-"));
    try {
      const home = path.join(root, "home");
      fs.mkdirSync(home);
      const wrapper = path.join(root, "slow-snapshot.mjs");
      fs.writeFileSync(wrapper, `#!${process.execPath}\nimport { spawnSync } from 'node:child_process';\nconst args=process.argv.slice(2);\nif(args[0]==='watch') Atomics.wait(new Int32Array(new SharedArrayBuffer(4)),0,0,500);\nconst r=spawnSync(${JSON.stringify(binary)},args,{env:process.env,encoding:'utf8'});\nprocess.stdout.write(r.stdout??''); process.stderr.write(r.stderr??''); process.exitCode=r.status??1;\n`, { mode: 0o755 });
      const env = {
        PATH: process.env.PATH, HOME: home, POST_MAIL_ROOT: path.join(root, "mail"),
        [`POST_${harness.toUpperCase()}_HOOK_BIN`]: wrapper,
        [`POST_${harness.toUpperCase()}_HOOK_STATE_DIR`]: path.join(root, "state"),
      };
      const run = () => new Promise((resolve, reject) => {
        const child = spawn(process.execPath, [path.join(repo, "skills/post/hooks", `${harness}-mail.mjs`)], { env });
        let stdout="", stderr="";
        child.stdout.on("data", data => stdout += data);
        child.stderr.on("data", data => stderr += data);
        child.on("error", reject);
        child.on("exit", code => {
          if (code !== 0) return reject(new Error(stderr));
          try { resolve(JSON.parse(stdout).hookSpecificOutput?.additionalContext ?? ""); }
          catch (error) { reject(error); }
        });
        child.stdin.end(JSON.stringify({ hook_event_name: event, session_id: "overlap", cwd: root }));
      });
      const contexts = await Promise.all(Array.from({ length: 6 }, run));
      assert.equal(contexts.filter(context => context.includes(notice)).length, 1);
      assert.equal((await run()).includes(notice), false);
    } finally { fs.rmSync(root, { recursive: true, force: true }); }
  });
}
