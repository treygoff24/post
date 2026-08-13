// Tests for identity-card.mjs (M5 card lookup). Run:
//     node --test skills/post/hooks/identity-card.test.mjs
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";

import { cardPath, identityCardContext } from "./identity-card.mjs";

const HARNESS = "claude";
const REPO_KEY = "post-1a2b3c4d";

function sandbox() {
  const data = fs.mkdtempSync(path.join(os.tmpdir(), "identity-card-test-"));
  const env = {
    XDG_DATA_HOME: data,
    POST_HARNESS: HARNESS,
    POST_REPO_KEY: REPO_KEY,
  };
  const dir = path.join(data, "agent-identities", HARNESS, REPO_KEY);
  fs.mkdirSync(dir, { recursive: true });
  return { env, file: path.join(dir, "identity.md") };
}

test("no launcher env -> null, never a synthesized path", () => {
  assert.equal(identityCardContext({}), null);
  assert.equal(cardPath({}), null);
  assert.equal(cardPath({ POST_HARNESS: "claude" }), null);
});

test("invalid env values -> null", () => {
  for (const bad of ["Claude", "a b", "-x", "x".repeat(33), "../etc", ""]) {
    assert.equal(cardPath({ POST_HARNESS: bad, POST_REPO_KEY: REPO_KEY }), null);
  }
  for (const bad of ["post", "post-XYZ12345", "post-1a2b3c", "../x-12345678"]) {
    assert.equal(cardPath({ POST_HARNESS: HARNESS, POST_REPO_KEY: bad }), null);
  }
});

test("relative XDG_DATA_HOME is ignored in favor of the default", () => {
  const p = cardPath({
    XDG_DATA_HOME: "relative/dir",
    POST_HARNESS: HARNESS,
    POST_REPO_KEY: REPO_KEY,
  });
  assert.ok(p.startsWith(path.join(os.homedir(), ".local", "share")));
});

test("absent card is silent (null), including empty file", () => {
  const sb = sandbox();
  assert.equal(identityCardContext(sb.env), null);
  fs.writeFileSync(sb.file, "");
  assert.equal(identityCardContext(sb.env), null);
});

test("present card injects content under the non-authority frame", () => {
  const sb = sandbox();
  fs.writeFileSync(sb.file, "I keep the room's letters.\n");
  const ctx = identityCardContext(sb.env);
  assert.match(ctx, /^\[post\] Identity card found/);
  assert.match(ctx, /a description, not an instruction/);
  assert.match(ctx, /I keep the room's letters\./);
  assert.ok(!ctx.endsWith("\n"));
});

test("symlink card is rejected with a notice, content never read", () => {
  const sb = sandbox();
  const real = path.join(path.dirname(sb.file), "elsewhere.md");
  fs.writeFileSync(real, "secret\n");
  fs.symlinkSync(real, sb.file);
  const ctx = identityCardContext(sb.env);
  assert.match(ctx, /not a regular file/);
  assert.ok(!ctx.includes("secret"));
});

test("oversize card is rejected with a notice, content never read", () => {
  const sb = sandbox();
  fs.writeFileSync(sb.file, "x".repeat(4097));
  const ctx = identityCardContext(sb.env);
  assert.match(ctx, /exceeds the 4096-byte cap/);
  assert.ok(!ctx.includes("xxxx"));
});

test("card at exactly the cap is accepted", () => {
  const sb = sandbox();
  fs.writeFileSync(sb.file, "y".repeat(4096));
  assert.match(identityCardContext(sb.env), /y{10}/);
});

test("control characters in content are rejected", () => {
  const sb = sandbox();
  fs.writeFileSync(sb.file, "hello\u0007world\n");
  assert.match(identityCardContext(sb.env), /not clean UTF-8 text/);
  fs.writeFileSync(sb.file, "tabs\tand\nnewlines\r\nare fine\n");
  assert.match(identityCardContext(sb.env), /tabs\tand/);
});
