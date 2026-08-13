// Tests for the agent-session identity launch helper. Run:
//     cargo build --release && node --test launcher/agent-session.test.mjs
// The suite drives the real script with the real release binary against a
// throwaway HOME/mail root; the real mailbox is never touched.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const LAUNCHER_DIR = path.dirname(fileURLToPath(import.meta.url));
const HELPER = path.join(LAUNCHER_DIR, "agent-session");
const REPO = path.resolve(LAUNCHER_DIR, "..");
const BIN = path.join(REPO, "target/release/post");

const ADDRESS_RE = /^[a-z0-9-]+\.[a-z0-9-]+-[0-9a-f]{8}\.[0-9a-f-]+$/;

function sandbox() {
  const work = fs.mkdtempSync(path.join(os.tmpdir(), "agent-session-test-"));
  const home = path.join(work, "home");
  const mail = path.join(work, "mail");
  const roomDir = path.join(home, "pinned-room");
  fs.mkdirSync(roomDir, { recursive: true });
  fs.mkdirSync(mail, { recursive: true });
  fs.writeFileSync(
    path.join(mail, "rooms.json"),
    JSON.stringify({ "pinned-room": "~/pinned-room" }) + "\n"
  );
  fs.writeFileSync(path.join(mail, "rules.json"), '{"blocked": []}\n');
  return { work, home, mail, roomDir };
}

/// Launch `agent-session <args> -- node -e <print env>` and return the child
/// process's identity environment plus the helper's stderr.
function launch(args, { cwd, env = {}, sb }) {
  const result = spawnSync(
    HELPER,
    [
      ...args,
      "--",
      process.execPath,
      "-e",
      'const keys=["POST_FROM","POST_SENDER_ADDRESS","POST_HARNESS","POST_REPO_KEY"];console.log(JSON.stringify(Object.fromEntries(keys.map(k=>[k,process.env[k]??null]))))',
    ],
    {
      cwd,
      encoding: "utf8",
      env: {
        ...process.env,
        HOME: sb.home,
        POST_MAIL_ROOT: sb.mail,
        AGENT_SESSION_POST_BIN: BIN,
        ...env,
      },
      timeout: 15000,
    }
  );
  if (result.error) throw result.error;
  return {
    status: result.status,
    stderr: result.stderr,
    env: result.status === 0 ? JSON.parse(result.stdout.trim().split("\n").pop()) : null,
  };
}

test("pins the registered room containing the launch cwd", () => {
  const sb = sandbox();
  try {
    const { status, env, stderr } = launch(["--harness", "claude-code"], {
      cwd: sb.roomDir,
      sb,
    });
    assert.equal(status, 0, stderr);
    assert.equal(env.POST_FROM, "pinned-room");
    assert.equal(env.POST_HARNESS, "claude-code");
    assert.match(env.POST_SENDER_ADDRESS, ADDRESS_RE);
    assert.ok(env.POST_SENDER_ADDRESS.startsWith("claude-code."));
    assert.ok(env.POST_SENDER_ADDRESS.length <= 256);
    assert.match(stderr, /pinned room 'pinned-room'/);
  } finally {
    fs.rmSync(sb.work, { recursive: true, force: true });
  }
});

test("no registered room: honest fallback exports no pin but full address", () => {
  const sb = sandbox();
  try {
    const outside = path.join(sb.work, "elsewhere");
    fs.mkdirSync(outside);
    const { status, env, stderr } = launch(["--harness", "codex"], {
      cwd: outside,
      sb,
    });
    assert.equal(status, 0, stderr);
    assert.equal(env.POST_FROM, null, "no pin may be synthesized");
    assert.equal(env.POST_HARNESS, "codex");
    assert.match(env.POST_SENDER_ADDRESS, ADDRESS_RE);
    assert.match(stderr, /no pin exported/);
  } finally {
    fs.rmSync(sb.work, { recursive: true, force: true });
  }
});

test("--room override beats cwd resolution", () => {
  const sb = sandbox();
  try {
    const { status, env } = launch(["--harness", "grok", "--room", "explicit-room"], {
      cwd: sb.roomDir,
      sb,
    });
    assert.equal(status, 0);
    assert.equal(env.POST_FROM, "explicit-room");
  } finally {
    fs.rmSync(sb.work, { recursive: true, force: true });
  }
});

test("stale inherited identity never survives a fresh launch", () => {
  const sb = sandbox();
  try {
    const outside = path.join(sb.work, "elsewhere");
    fs.mkdirSync(outside);
    const { status, env } = launch(["--harness", "cursor"], {
      cwd: outside,
      sb,
      env: {
        POST_FROM: "stale-ghost",
        POST_SENDER_ADDRESS: "stale.ghost.deadbeef",
      },
    });
    assert.equal(status, 0);
    assert.equal(env.POST_FROM, null, "inherited pin must be cleared, not passed through");
    assert.match(env.POST_SENDER_ADDRESS, ADDRESS_RE);
    assert.notEqual(env.POST_SENDER_ADDRESS, "stale.ghost.deadbeef");
  } finally {
    fs.rmSync(sb.work, { recursive: true, force: true });
  }
});

test("each launch mints a distinct address", () => {
  const sb = sandbox();
  try {
    const first = launch(["--harness", "claude-code"], { cwd: sb.roomDir, sb });
    const second = launch(["--harness", "claude-code"], { cwd: sb.roomDir, sb });
    assert.equal(first.status, 0);
    assert.equal(second.status, 0);
    assert.notEqual(first.env.POST_SENDER_ADDRESS, second.env.POST_SENDER_ADDRESS);
    // Same launch dir → same harness + repo key prefix, only the UUID varies.
    const prefix = (address) => address.split(".").slice(0, 2).join(".");
    assert.equal(prefix(first.env.POST_SENDER_ADDRESS), prefix(second.env.POST_SENDER_ADDRESS));
  } finally {
    fs.rmSync(sb.work, { recursive: true, force: true });
  }
});

test("usage errors are loud: bad slug, missing --, missing command", () => {
  const sb = sandbox();
  try {
    for (const args of [
      ["--harness", "Bad_Slug"],
      ["--harness", "-dash-first"],
      ["--harness", ""],
    ]) {
      const result = spawnSync(HELPER, [...args, "--", "true"], {
        cwd: sb.roomDir,
        encoding: "utf8",
        env: { ...process.env, HOME: sb.home, POST_MAIL_ROOT: sb.mail },
      });
      assert.equal(result.status, 64, `slug ${args[1]} must be refused`);
      assert.match(result.stderr, /--harness/);
    }
    const noDashes = spawnSync(HELPER, ["--harness", "ok"], {
      cwd: sb.roomDir,
      encoding: "utf8",
      env: { ...process.env, HOME: sb.home, POST_MAIL_ROOT: sb.mail },
    });
    assert.equal(noDashes.status, 64);
    assert.match(noDashes.stderr, /missing '--'/);
    const noCommand = spawnSync(HELPER, ["--harness", "ok", "--"], {
      cwd: sb.roomDir,
      encoding: "utf8",
      env: { ...process.env, HOME: sb.home, POST_MAIL_ROOT: sb.mail },
    });
    assert.equal(noCommand.status, 64);
    assert.match(noCommand.stderr, /missing vendor command/);
  } finally {
    fs.rmSync(sb.work, { recursive: true, force: true });
  }
});

test("end to end: a send through the helper records declared-env + the address", () => {
  const sb = sandbox();
  try {
    // Second registered room to receive the mail.
    fs.mkdirSync(path.join(sb.home, "receiver-room"), { recursive: true });
    fs.writeFileSync(
      path.join(sb.mail, "rooms.json"),
      JSON.stringify({ "pinned-room": "~/pinned-room", receiver: "~/receiver-room" }) + "\n"
    );
    // Launch cwd inside pinned-room; the CHILD post send runs from a
    // directory OUTSIDE it — only the pin carries the identity.
    const outside = path.join(sb.work, "elsewhere");
    fs.mkdirSync(outside);
    const result = spawnSync(
      HELPER,
      [
        "--harness",
        "claude-code",
        "--",
        "sh",
        "-c",
        `cd '${outside}' && '${BIN}' send --to receiver --body 'via helper' --json`,
      ],
      {
        cwd: sb.roomDir,
        encoding: "utf8",
        env: {
          ...process.env,
          HOME: sb.home,
          POST_MAIL_ROOT: sb.mail,
          AGENT_SESSION_POST_BIN: BIN,
        },
        timeout: 15000,
      }
    );
    assert.equal(result.status, 0, result.stderr);
    const sent = JSON.parse(result.stdout);
    assert.equal(sent.envelope.from, "pinned-room");
    assert.equal(sent.envelope.sender_provenance, "declared-env");
    assert.match(sent.envelope.sender_address, ADDRESS_RE);
    assert.ok(sent.envelope.sender_address.startsWith("claude-code."));
  } finally {
    fs.rmSync(sb.work, { recursive: true, force: true });
  }
});

test("shims exec the helper with their harness slug", () => {
  const sb = sandbox();
  try {
    for (const [shim, slug] of [
      ["claude", "claude-code"],
      ["codex", "codex"],
      ["cursor", "cursor"],
      ["grok", "grok"],
    ]) {
      const body = fs.readFileSync(path.join(LAUNCHER_DIR, "shims", shim), "utf8");
      assert.match(body, new RegExp(`--harness ${slug} --`), `${shim} declares slug ${slug}`);
      assert.match(body, /agent-session/, `${shim} routes through the helper`);
    }
  } finally {
    fs.rmSync(sb.work, { recursive: true, force: true });
  }
});

test("doctor: green inside a helper launch, red outside", () => {
  const sb = sandbox();
  try {
    const inside = spawnSync(
      HELPER,
      ["--harness", "claude-code", "--", HELPER, "--doctor"],
      {
        cwd: sb.roomDir,
        encoding: "utf8",
        env: {
          ...process.env,
          HOME: sb.home,
          POST_MAIL_ROOT: sb.mail,
          AGENT_SESSION_POST_BIN: BIN,
        },
        timeout: 15000,
      }
    );
    assert.equal(inside.status, 0, inside.stdout + inside.stderr);
    assert.match(inside.stdout, /pin names a registered room/);

    const outsideEnv = { ...process.env, HOME: sb.home, POST_MAIL_ROOT: sb.mail };
    delete outsideEnv.POST_FROM;
    delete outsideEnv.POST_SENDER_ADDRESS;
    delete outsideEnv.POST_HARNESS;
    delete outsideEnv.POST_REPO_KEY;
    const outside = spawnSync(HELPER, ["--doctor"], {
      cwd: sb.roomDir,
      encoding: "utf8",
      env: outsideEnv,
      timeout: 15000,
    });
    assert.equal(outside.status, 1);
    assert.match(outside.stdout, /NOT set/);
  } finally {
    fs.rmSync(sb.work, { recursive: true, force: true });
  }
});
