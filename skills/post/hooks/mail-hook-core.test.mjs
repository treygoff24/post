// Tests for mail-hook-core.mjs, run through all four harness adapters. Each
// harness keeps its own test file for its payload shape; this file holds the
// behaviour they share and the identity handling added by the 2026-09-28 fix
// wave (docs/plans/post-just-works-2026-09-28.md sections 1 and 3): warn-once
// conflict line, rebind on `participant_missing`, the unbound marker, lazy
// minting, and tolerant reading of watch events.
//
// Run: node --test skills/post/hooks/mail-hook-core.test.mjs
// Node stdlib only. A stub `post` is scripted per test through a control file;
// every run gets its own throwaway root and a cleared environment, so nothing
// here can reach the real mail store. POST_HOOK_TEST_DIR points the suite at
// another copy of the adapters (used once to prove the tests fail on old code).

import test, { describe } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { createHash } from "node:crypto";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

import { contextFor, parseSnapshot, participantMissing } from "./mail-hook-core.mjs";

const HOOKS = process.env.POST_HOOK_TEST_DIR || path.dirname(fileURLToPath(import.meta.url));
const ROOT = fs.mkdtempSync(path.join(os.tmpdir(), "post-hook-core-test-"));
test.after(() => fs.rmSync(ROOT, { recursive: true, force: true }));

const STUB = path.join(ROOT, "post-stub.mjs");
fs.writeFileSync(
  STUB,
  String.raw`#!/usr/bin/env node
import fs from "node:fs";
const args = process.argv.slice(2);
const key = args[0] === "participant" ? "participant " + args[1] : args[0];
fs.appendFileSync(process.env.STUB_CALLS, JSON.stringify({ args, key, participant: process.env.POST_PARTICIPANT ?? null, ambient: process.env.CLAUDE_CODE_SESSION_ID ?? null }) + "\n");
const control = JSON.parse(fs.readFileSync(process.env.STUB_CONTROL, "utf8"));
const list = control[key];
let step = {};
if (Array.isArray(list) && list.length > 0) {
  const counter = process.env.STUB_CALLS + ".count." + key.replace(/\W/g, "_");
  let n = 0;
  try { n = Number(fs.readFileSync(counter, "utf8")); } catch {}
  fs.writeFileSync(counter, String(n + 1));
  step = list[Math.min(n, list.length - 1)];
}
const json = (value) => JSON.stringify(value) + "\n";
let stdout = step.stdout;
if (stdout !== undefined && typeof stdout !== "string") stdout = json(stdout);
if (stdout === undefined) {
  if (key === "version") stdout = json({ ok: true, capabilities: ["participants"] });
  else if (key === "participant bind") stdout = json({ ok: true, status: "bound", id: process.env.POST_PARTICIPANT || "test-participant", participant: { id: process.env.POST_PARTICIPANT || "test-participant", lineage: null } });
  else if (key === "participant show") stdout = json({ ok: true, status: "unbound", bound: false });
  else if (key === "participant notice") stdout = json({ ok: true, notice: null });
  else stdout = "";
}
if (stdout) process.stdout.write(stdout);
if (step.stderr) process.stderr.write(step.stderr);
if (step.exit) process.exitCode = step.exit;
`
);
fs.chmodSync(STUB, 0o755);

const MISSING = {
  stderr: JSON.stringify({ ok: false, error: { code: "participant_missing", message: "participant claude-x does not exist", suggested_fix: "run: post participant bind --harness claude --key K" } }) + "\n",
  exit: 65,
};

const MAIL = {
  event: "mail",
  room: "proj",
  id: "20260730-010101-aaa111",
  from: "peer",
  kind: "note",
  subject: "SECRET-SUBJECT",
  sent: "2026-07-30 01:01:01 -0500",
  reason: "mail",
};
const FUTURE = { event: "future_kind", id: "x", note: "a kind a later post may add" };
const UNBOUND_MARKER = { ok: true, participant: null, bound: false, hint: "this session is not bound yet; nothing can be addressed to it" };
const jsonl = (...events) => events.map((event) => JSON.stringify(event)).join("\n") + (events.length ? "\n" : "");

// Per-harness payload shapes. `lazy` marks adapters that may leave a session
// unminted at start (an ambient session key lets the CLI mint it on a write).
const ADAPTERS = {
  claude: {
    script: "claude-mail.mjs",
    prefix: "POST_CLAUDE_HOOK",
    lazy: true,
    hasStart: true,
    input: (phase, session, cwd) => ({ hook_event_name: { start: "SessionStart", prompt: "UserPromptSubmit", tool: "PostToolUse", end: "SessionEnd" }[phase], session_id: session, cwd }),
  },
  codex: {
    script: "codex-mail.mjs",
    prefix: "POST_CODEX_HOOK",
    lazy: true,
    hasStart: true,
    input: (phase, session, cwd) => ({ hook_event_name: { start: "SessionStart", prompt: "UserPromptSubmit", tool: "PostToolUse" }[phase], session_id: session, cwd }),
  },
  cursor: {
    script: "cursor-mail.mjs",
    prefix: "POST_CURSOR_HOOK",
    lazy: false,
    hasStart: true,
    input: (phase, session, cwd) => ({ hook_event_name: { start: "sessionStart", prompt: "beforeSubmitPrompt", tool: "postToolUse" }[phase], session_id: session, cwd }),
  },
  grok: {
    script: "grok-mail.mjs",
    prefix: "POST_GROK_HOOK",
    lazy: false,
    hasStart: false, // the first prompt plays session start
    input: (_phase, session, cwd) => ({ hookEventName: "UserPromptSubmit", sessionId: session, cwd }),
  },
};

let worlds = 0;
function makeWorld(name, adapter) {
  const dir = path.join(ROOT, `w${worlds++}-${name}`);
  const world = {
    dir,
    cwd: fs.realpathSync(fs.mkdirSync(path.join(dir, "proj"), { recursive: true }) ?? dir),
    stateDir: path.join(dir, "state"),
    controlFile: path.join(dir, "control.json"),
    callsFile: path.join(dir, "calls.log"),
    control(value) {
      for (const file of fs.readdirSync(dir)) if (file.startsWith("calls.log.count.")) fs.rmSync(path.join(dir, file));
      fs.writeFileSync(world.controlFile, JSON.stringify(value));
    },
    reset() {
      fs.writeFileSync(world.callsFile, "");
      for (const file of fs.readdirSync(dir)) if (file.startsWith("calls.log.count.")) fs.rmSync(path.join(dir, file));
    },
    calls() {
      return fs.readFileSync(world.callsFile, "utf8").split("\n").filter(Boolean).map((line) => JSON.parse(line));
    },
    keys() {
      return world.calls().map((call) => call.key);
    },
    run(phase, session, { env: extra = {}, cwd = world.cwd, throttleMs = 0, unsetEnv = [] } = {}) {
      const env = {
        ...process.env,
        [`${adapter.prefix}_BIN`]: STUB,
        [`${adapter.prefix}_STATE_DIR`]: world.stateDir,
        [`${adapter.prefix}_THROTTLE_MS`]: String(throttleMs),
        STUB_CONTROL: world.controlFile,
        STUB_CALLS: world.callsFile,
        DELEGATE_RUN_ID: "",
        POST_PARTICIPANT: "",
        ...extra,
      };
      for (const key of unsetEnv) delete env[key];
      const result = spawnSync(process.execPath, [path.join(HOOKS, adapter.script)], {
        input: JSON.stringify(adapter.input(phase, session, cwd)),
        encoding: "utf8",
        env,
      });
      assert.equal(result.status, 0, `hook must always exit 0: ${result.stderr}`);
      return JSON.parse(result.stdout);
    },
    state(session) {
      return JSON.parse(fs.readFileSync(path.join(world.stateDir, `session-${session}.json`), "utf8"));
    },
  };
  world.control({});
  world.reset();
  return world;
}

const contextOf = (out) => out?.hookSpecificOutput?.additionalContext ?? "";
const derivedId = (harness, session) => `${harness}-${createHash("sha256").update(session).digest("hex").slice(0, 8)}`;

// ------------------------------------------------------------ pure parsing

describe("parseSnapshot", () => {
  test("a future event value is skipped and the rest of the batch survives", () => {
    const parsed = parseSnapshot(jsonl(FUTURE, MAIL, { ...FUTURE, event: "another" }));
    assert.equal(parsed.events.length, 1);
    assert.equal(parsed.events[0].id, MAIL.id);
    assert.equal(parsed.skipped, 2);
    assert.equal(parsed.malformed, 0);
  });

  test("the unbound marker is recognised, and is neither an event nor malformed", () => {
    const parsed = parseSnapshot(jsonl(UNBOUND_MARKER));
    assert.deepEqual({ events: parsed.events.length, unbound: parsed.unbound, malformed: parsed.malformed, nonempty: parsed.nonempty }, { events: 0, unbound: true, malformed: 0, nonempty: 1 });
  });

  test("a known event that fails validation stays malformed", () => {
    const parsed = parseSnapshot(jsonl({ ...MAIL, id: "forged" }, FUTURE));
    assert.equal(parsed.malformed, 1);
    assert.equal(parsed.skipped, 1);
    assert.equal(parsed.events.length, 0);
  });

  test("non-JSON lines, arrays and objects with no event string are malformed", () => {
    const parsed = parseSnapshot('not json\n[1]\n{"id":"x"}\n"str"\n');
    assert.equal(parsed.malformed, 4);
    assert.equal(parsed.skipped, 0);
  });

  test("a bound:false line that also names an event is not the marker", () => {
    const parsed = parseSnapshot(jsonl({ ...MAIL, bound: false }));
    assert.equal(parsed.unbound, false);
    assert.equal(parsed.events.length, 1);
  });

  test("contextFor takes the harness wording", () => {
    assert.match(contextFor([MAIL], "New mail"), /New mail is waiting for room proj/);
    assert.match(contextFor([MAIL]), /Unread agent mail is waiting for room proj/);
  });
});

describe("participantMissing", () => {
  const envelope = (code) => JSON.stringify({ ok: false, error: { code, message: "m" } });
  test("needs exit 65 and the typed code", () => {
    assert.equal(participantMissing({ status: 65, stderr: envelope("participant_missing") + "\n" }), true);
    assert.equal(participantMissing({ status: 65, stderr: "warning line\n" + envelope("participant_missing") + "\n" }), true);
    assert.equal(participantMissing({ status: 65, stderr: envelope("no_participant") }), false);
    assert.equal(participantMissing({ status: 78, stderr: envelope("participant_missing") }), false);
    assert.equal(participantMissing({ status: 0, stderr: envelope("participant_missing") }), false);
    assert.equal(participantMissing({ status: 65, stderr: "" }), false);
    assert.equal(participantMissing({ status: 65, stderr: "error: participant_missing: run post participant bind" }), true);
    assert.equal(participantMissing({ error: new Error("spawn"), status: null, stderr: "" }), false);
  });
});

// ------------------------------------------------------- conflict warn-once

for (const [name, adapter] of Object.entries(ADAPTERS)) {
  describe(`${name}: the POST_PARTICIPANT conflict line prints once per session`, () => {
    const conflicting = { POST_PARTICIPANT: "someone-else-00000000" };

    test("first event warns, later events stay silent, calls nothing", () => {
      const world = makeWorld("conflict", adapter);
      const first = world.run("start", "sess-a", { env: conflicting });
      assert.match(contextOf(first), /POST_PARTICIPANT conflicts with this hook session key/);
      for (const phase of adapter.hasStart ? ["prompt", "tool", "prompt"] : ["prompt", "prompt"]) {
        assert.deepEqual(world.run(phase, "sess-a", { env: conflicting }), {}, `${phase} must not repeat the warning`);
      }
      assert.deepEqual(world.calls(), [], "a conflicting session never reaches post");
      assert.equal(world.state("sess-a").conflictWarned, true);
    });

    test("another session warns for itself", () => {
      const world = makeWorld("conflict-two", adapter);
      assert.match(contextOf(world.run("start", "sess-a", { env: conflicting })), /conflicts/);
      assert.match(contextOf(world.run("start", "sess-b", { env: conflicting })), /conflicts/);
      assert.deepEqual(world.run("prompt", "sess-b", { env: conflicting }), {});
    });

    if (adapter.hasStart) {
      test("a fresh session start (resume) warns again", () => {
        const world = makeWorld("conflict-resume", adapter);
        assert.match(contextOf(world.run("start", "sess-a", { env: conflicting })), /conflicts/);
        assert.deepEqual(world.run("prompt", "sess-a", { env: conflicting }), {});
        assert.match(contextOf(world.run("start", "sess-a", { env: conflicting })), /conflicts/);
        assert.deepEqual(world.run("prompt", "sess-a", { env: conflicting }), {});
      });
    }

    test("a matching explicit id is not a conflict", () => {
      const world = makeWorld("no-conflict", adapter);
      const out = world.run("start", "sess-a", { env: { POST_PARTICIPANT: derivedId(name, "sess-a") } });
      assert.doesNotMatch(contextOf(out), /conflicts/);
      assert.ok(world.keys().includes("watch"));
    });
  });
}

// ---------------------------------------- participant_missing: rebind, retry

for (const [name, adapter] of Object.entries(ADAPTERS)) {
  describe(`${name}: a claimed participant that no longer exists`, () => {
    const bindCall = (session) => ["participant", "bind", "--harness", name, "--key", session, "--json"];

    function established(worldName, session) {
      const world = makeWorld(worldName, adapter);
      world.control({ watch: [{ stdout: "" }] });
      world.run("start", session);
      assert.ok(world.state(session).participantId, "setup: the session is bound");
      world.reset();
      return world;
    }

    test("watch fails participant_missing: rebind once, retry, deliver the mail", () => {
      const session = "sess-missing";
      const world = established("missing-watch", session);
      world.control({ watch: [MISSING, { stdout: jsonl(MAIL) }] });
      const out = world.run("prompt", session);
      assert.match(contextOf(out), /Direct mail id\(s\): 20260730-010101-aaa111/);
      const calls = world.calls();
      const watches = calls.filter((call) => call.key === "watch");
      assert.equal(watches.length, 2, "one failure, one retry");
      const binds = calls.filter((call) => call.key === "participant bind");
      assert.equal(binds.length, 1, "exactly one rebind");
      assert.deepEqual(binds[0].args, bindCall(session));
      assert.ok(calls.indexOf(binds[0]) > calls.indexOf(watches[0]) && calls.indexOf(binds[0]) < calls.indexOf(watches[1]), "bind sits between the failure and the retry");
      assert.ok(watches[1].participant, "the retry still names its participant");
      assert.equal(world.state(session).failStreak, 0);
    });

    test("a participant that stays missing is a failure, never 'no mail'", () => {
      const session = "sess-stays-missing";
      const world = established("missing-forever", session);
      world.control({ watch: [MISSING] });
      const out = world.run("prompt", session);
      assert.match(contextOf(out), /UNKNOWN \(not empty\)/, "the failure is reported, not read as an empty inbox");
      assert.equal(world.calls().filter((call) => call.key === "participant bind").length, 1, "one rebind per hook run, not a loop");
      assert.equal(world.calls().filter((call) => call.key === "watch").length, 2);
      // The second failure of the streak stays quiet, like every other failure.
      assert.deepEqual(world.run("prompt", session), {});
    });

    test("a rebind that itself fails is a failure too", () => {
      const session = "sess-rebind-fails";
      const world = established("rebind-fails", session);
      world.control({ watch: [MISSING], "participant bind": [{ stdout: { ok: false, status: "unbound" } }] });
      assert.match(contextOf(world.run("prompt", session)), /UNKNOWN \(not empty\)/);
      assert.equal(world.calls().filter((call) => call.key === "watch").length, 1, "no retry without a rebound participant");
    });

    test("participant touch failing participant_missing rebinds without a lifecycle warning", () => {
      const session = "sess-missing-touch";
      const world = established("missing-touch", session);
      world.control({ "participant touch": [MISSING, {}], watch: [{ stdout: "" }] });
      const out = world.run("prompt", session);
      assert.deepEqual(out, {}, "the touch retry succeeded, so there is nothing to warn about");
      const keys = world.keys();
      assert.deepEqual(keys.filter((key) => key === "participant touch").length, 2);
      assert.equal(keys.filter((key) => key === "participant bind").length, 1);
      assert.equal(keys.filter((key) => key === "watch").length, 1, "the scan ran once, after the record was back");
    });

    test("a record that stays missing costs one rebind per hook run, across touch and watch", () => {
      const session = "sess-missing-everywhere";
      const world = established("missing-everywhere", session);
      world.control({ "participant touch": [MISSING], watch: [MISSING] });
      assert.match(contextOf(world.run("prompt", session)), /UNKNOWN \(not empty\)/);
      assert.equal(world.calls().filter((call) => call.key === "participant bind").length, 1);
    });

    test("an ordinary nonzero exit is not rebound", () => {
      const session = "sess-plain-failure";
      const world = established("plain-failure", session);
      world.control({ watch: [{ exit: 78, stderr: JSON.stringify({ ok: false, error: { code: "config_invalid" } }) }] });
      assert.match(contextOf(world.run("prompt", session)), /UNKNOWN \(not empty\)/);
      assert.equal(world.calls().filter((call) => call.key === "participant bind").length, 0);
    });

    if (name === "claude") {
      test("SessionEnd for a record that is already gone is not a warning", () => {
        const session = "sess-end-missing";
        const world = established("end-missing", session);
        world.control({ "participant end": [MISSING] });
        assert.deepEqual(world.run("end", session), {});
      });
    }
  });
}

// ------------------------------------------------------- the unbound marker

for (const [name, adapter] of Object.entries(ADAPTERS)) {
  describe(`${name}: the unbound marker and future event kinds`, () => {
    test("the unbound marker is no mail and no error", () => {
      const world = makeWorld("unbound-marker", adapter);
      world.control({ watch: [{ stdout: jsonl(UNBOUND_MARKER) }] });
      // Cursor and Grok print the participant id after a fresh bind; that line
      // is the only thing a marker-only batch may produce.
      assert.doesNotMatch(contextOf(world.run("start", "sess-u")), /UNKNOWN|mail/i);
      assert.equal(world.state("sess-u").failStreak, 0);
      assert.deepEqual(world.run("prompt", "sess-u"), {});
      assert.equal(world.state("sess-u").failStreak, 0);
    });

    test("a batch with a future kind still delivers the known mail", () => {
      const world = makeWorld("future-batch", adapter);
      world.control({ watch: [{ stdout: jsonl(FUTURE, MAIL, { event: "bridge_attention", id: "y" }) }] });
      const out = world.run("start", "sess-f");
      assert.match(contextOf(out), /Direct mail id\(s\): 20260730-010101-aaa111/);
      assert.doesNotMatch(contextOf(out), /UNKNOWN/);
      assert.deepEqual(world.run("prompt", "sess-f"), {}, "the known mail is deduped, the future kind does not re-ring");
    });

    test("a batch of only future kinds is quiet, not a failure", () => {
      const world = makeWorld("future-only", adapter);
      world.control({ watch: [{ stdout: jsonl(FUTURE, { event: "another", id: "z" }) }] });
      assert.doesNotMatch(contextOf(world.run("start", "sess-g")), /UNKNOWN|mail/i);
      assert.equal(world.state("sess-g").failStreak, 0);
      assert.deepEqual(world.run("prompt", "sess-g"), {});
    });

    test("a malformed known event next to a future kind still fails closed", () => {
      const world = makeWorld("future-plus-forged", adapter);
      world.control({ watch: [{ stdout: jsonl(FUTURE, { ...MAIL, id: "forged" }) }] });
      assert.match(contextOf(world.run("start", "sess-h")), /UNKNOWN \(not empty\)/);
    });
  });
}

// ------------------------------------------------------------ lazy minting

const roomsListing = (...rooms) => ({ ok: true, count: rooms.length, rooms: rooms.map((room) => ({ name: path.basename(room), path: room, blocked: [] })) });
const BOUND = (id, lineage = null) => ({ ok: true, status: "bound", bound: true, id, participant: { id, lineage } });

for (const [name, adapter] of Object.entries(ADAPTERS).filter(([, a]) => a.lazy)) {
  describe(`${name}: lazy minting`, () => {
    const ID = `${name}-abcdef12`;

    test("an unregistered cwd is not minted at start and stays quiet until the CLI mints it", () => {
      const world = makeWorld("lazy-unregistered", adapter);
      world.control({ rooms: [{ stdout: roomsListing("/somewhere/else/entirely") }] });
      assert.deepEqual(world.run("start", "sess-l"), {});
      assert.deepEqual(world.keys(), ["version", "rooms", "participant show"]);
      const show = world.calls().find((call) => call.key === "participant show");
      assert.deepEqual(show.args, ["participant", "show", "--harness", name, "--key", "sess-l", "--json"]);
      assert.equal(show.participant, null, "the check names the key, never a participant");
      assert.equal(world.state("sess-l").deferred, true);
      assert.equal(world.state("sess-l").participantId, null);

      // Still unminted: each turn asks once, cheaply, and says nothing.
      world.reset();
      assert.deepEqual(world.run("prompt", "sess-l"), {});
      assert.deepEqual(world.keys(), ["participant show"]);

      // The agent's first write minted it. The next turn proceeds as usual.
      world.reset();
      world.control({ "participant show": [{ stdout: BOUND(ID) }], watch: [{ stdout: jsonl(MAIL) }] });
      const out = world.run("prompt", "sess-l");
      assert.match(contextOf(out), /Direct mail id\(s\): 20260730-010101-aaa111/);
      const keys = world.keys();
      assert.ok(!keys.includes("participant bind"), "the hook never mints a session the CLI already minted");
      assert.equal(world.calls().find((call) => call.key === "watch").participant, ID, "the scan runs as the minted participant");
      assert.equal(world.state("sess-l").participantId, ID);
      assert.equal(world.state("sess-l").deferred, false);

      // And from then on it is an ordinary session: no per-turn minted check.
      world.reset();
      world.control({ watch: [{ stdout: "" }] });
      assert.deepEqual(world.run("prompt", "sess-l"), {});
      assert.ok(!world.calls().some((call) => call.key === "participant show" && call.args.includes("--harness")));
    });

    test("a session minted while its first scan fails is no longer deferred", () => {
      const world = makeWorld("lazy-minted-then-fail", adapter);
      world.control({ rooms: [{ stdout: roomsListing("/elsewhere") }] });
      world.run("start", "sess-m");
      world.reset();
      world.control({ "participant show": [{ stdout: BOUND(ID) }], watch: [{ exit: 1 }] });
      assert.match(contextOf(world.run("prompt", "sess-m")), /UNKNOWN \(not empty\)/);
      assert.equal(world.state("sess-m").deferred, false);
      assert.equal(world.state("sess-m").participantId, ID);
      world.reset();
      world.control({ watch: [{ stdout: "" }] });
      world.run("prompt", "sess-m");
      assert.ok(!world.calls().some((call) => call.key === "participant show" && call.args.includes("--harness")), "no more minted-yet probes");
    });

    test("a registered cwd is minted at start as before", () => {
      const world = makeWorld("lazy-registered", adapter);
      world.control({ rooms: [{ stdout: roomsListing(world.cwd) }] });
      world.run("start", "sess-r");
      assert.ok(world.keys().includes("participant bind"));
      assert.equal(world.state("sess-r").deferred, false);
    });

    test("a subdirectory of a registered room counts as registered", () => {
      const world = makeWorld("lazy-subdir", adapter);
      const sub = path.join(world.cwd, "src", "deep");
      fs.mkdirSync(sub, { recursive: true });
      world.control({ rooms: [{ stdout: roomsListing(world.cwd) }] });
      world.run("start", "sess-s", { cwd: sub });
      assert.ok(world.keys().includes("participant bind"));
    });

    test("a sibling directory that only shares a name prefix is not inside the room", () => {
      const world = makeWorld("lazy-prefix", adapter);
      const sibling = `${world.cwd}-extra`;
      fs.mkdirSync(sibling, { recursive: true });
      world.control({ rooms: [{ stdout: roomsListing(world.cwd) }] });
      assert.deepEqual(world.run("start", "sess-p", { cwd: sibling }), {});
      assert.ok(!world.keys().includes("participant bind"));
    });

    test("a delegate child is not minted even inside a registered room", () => {
      const world = makeWorld("lazy-delegate", adapter);
      world.control({ rooms: [{ stdout: roomsListing(world.cwd) }] });
      assert.deepEqual(world.run("start", "sess-d", { env: { DELEGATE_RUN_ID: "run-123" } }), {});
      assert.ok(!world.keys().includes("participant bind"));
      assert.equal(world.state("sess-d").deferred, true);
    });

    test("a session already minted (a resume) is bound as usual even in an unregistered cwd", () => {
      const world = makeWorld("lazy-resume", adapter);
      world.control({ rooms: [{ stdout: roomsListing("/elsewhere") }], "participant show": [{ stdout: BOUND(ID) }] });
      world.run("start", "sess-x");
      assert.ok(world.keys().includes("participant bind"));
      assert.equal(world.state("sess-x").deferred, false);
    });

    test("an unreadable rooms listing is not evidence: mint as usual", () => {
      const world = makeWorld("lazy-rooms-broken", adapter);
      world.control({ rooms: [{ exit: 1 }] });
      world.run("start", "sess-b");
      assert.ok(world.keys().includes("participant bind"));
      const garbled = makeWorld("lazy-rooms-garbled", adapter);
      garbled.control({ rooms: [{ stdout: "not json\n" }] });
      garbled.run("start", "sess-b");
      assert.ok(garbled.keys().includes("participant bind"));
    });

    test("a show that cannot answer (an older post) falls back to minting", () => {
      const world = makeWorld("lazy-show-old", adapter);
      world.control({ rooms: [{ stdout: roomsListing("/elsewhere") }], "participant show": [{ exit: 2, stderr: "error: unexpected argument '--harness'\n" }] });
      world.run("start", "sess-o");
      assert.ok(world.keys().includes("participant bind"));
    });

    test("an explicit participant is never deferred", () => {
      const world = makeWorld("lazy-explicit", adapter);
      world.control({ rooms: [{ stdout: roomsListing("/elsewhere") }] });
      world.run("start", "sess-e", { env: { POST_PARTICIPANT: derivedId(name, "sess-e"), DELEGATE_RUN_ID: "run-9" } });
      const bind = world.calls().find((call) => call.key === "participant bind");
      assert.deepEqual(bind.args, ["participant", "bind", "--json"]);
      assert.ok(!world.keys().includes("rooms"));
    });

    test("a deferred PostToolUse inside the throttle window spawns nothing", () => {
      const world = makeWorld("lazy-throttle", adapter);
      world.control({ rooms: [{ stdout: roomsListing("/elsewhere") }] });
      world.run("start", "sess-t");
      world.reset();
      assert.deepEqual(world.run("tool", "sess-t", { throttleMs: 60_000 }), {});
      assert.deepEqual(world.calls(), []);
    });

    test("SessionEnd of a session that was never minted ends nothing", () => {
      if (name !== "claude") return;
      const world = makeWorld("lazy-end", adapter);
      world.control({ rooms: [{ stdout: roomsListing("/elsewhere") }] });
      world.run("start", "sess-z");
      world.reset();
      assert.deepEqual(world.run("end", "sess-z"), {});
      assert.deepEqual(world.calls(), []);
    });
  });
}

for (const [name, adapter] of Object.entries(ADAPTERS).filter(([, a]) => !a.lazy)) {
  describe(`${name}: no ambient session key, so no lazy minting`, () => {
    test("a delegate child in an unregistered cwd is still minted and told its id", () => {
      const world = makeWorld("eager", adapter);
      world.control({ rooms: [{ stdout: roomsListing("/elsewhere") }] });
      const out = world.run("start", "sess-c", { env: { DELEGATE_RUN_ID: "run-77" } });
      assert.ok(world.keys().includes("participant bind"));
      assert.ok(!world.keys().includes("rooms"), "the workspace check is skipped");
      assert.match(contextOf(out), /prefix Post commands with POST_PARTICIPANT=/);
    });
  });
}
