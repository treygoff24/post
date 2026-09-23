// Consumer contract tests: every hook that parses post output is driven end
// to end against the samples the real post binary emits
// (`post contract samples --dir`), plus additive, optional-field, and
// malformed variants of them. Run: node --test skills/post/hooks/*.test.mjs
//
// POST_BIN selects the producer (default <repo>/target/release/post). A failed
// sample emit FAILS every test here; it never skips.
//
// Surfaces covered: `version --json`, `participant bind --json`,
// `participant show --json`, `watch --snapshot` (plain and cursor-unusable),
// `rooms`, `channels`. Not covered: `participant notice` (no sample; the stub
// echoes the snapshot text, as the neighbouring suites do, which never parses
// as an activation), `watch --digest` (no hook requests it), and herdr's
// `agent get` output (not post's).

import test, { describe } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawn, spawnSync } from "node:child_process";

const HOOKS = path.dirname(fileURLToPath(import.meta.url));
const REPO = path.resolve(HOOKS, "..", "..", "..");
const POST_BIN = process.env.POST_BIN || path.join(REPO, "target", "release", "post");
const ROOT = fs.mkdtempSync(path.join(os.tmpdir(), "post-contract-test-"));
const CWD = path.join(ROOT, "project");
fs.mkdirSync(CWD, { recursive: true });

test.after(() => {
  fs.rmSync(ROOT, { recursive: true, force: true });
});

// ---------------------------------------------------------------- samples

let loaded = null;
function samples() {
  if (loaded) return loaded;
  const dir = path.join(ROOT, "samples");
  const result = spawnSync(POST_BIN, ["contract", "samples", "--dir", dir], { encoding: "utf8" });
  if (result.error || result.status !== 0) {
    throw new Error(
      `\`${POST_BIN} contract samples --dir ${dir}\` failed ` +
        `(${result.error ? result.error.message : `exit ${result.status}`}): ${String(result.stderr ?? "").trim()}`
    );
  }
  const raw = (name) => fs.readFileSync(path.join(dir, name), "utf8");
  const lines = (name) => raw(name).split("\n").filter((line) => line.trim()).map((line) => JSON.parse(line));
  const S = {
    raw: {
      version: raw("version.json"),
      bind: raw("participant-bind.json"),
      show: raw("participant-show.json"),
      watch: raw("watch-snapshot.jsonl"),
      cursorUnusable: raw("watch-snapshot-cursor-unusable.jsonl"),
      rooms: raw("rooms.json"),
      channels: raw("channels.json"),
    },
    version: JSON.parse(raw("version.json")),
    bind: JSON.parse(raw("participant-bind.json")),
    show: JSON.parse(raw("participant-show.json")),
    watch: lines("watch-snapshot.jsonl"),
    cursorUnusable: lines("watch-snapshot-cursor-unusable.jsonl"),
    rooms: JSON.parse(raw("rooms.json")),
    channels: JSON.parse(raw("channels.json")),
  };
  const pick = (events, label, predicate) => {
    const found = events.find(predicate);
    if (!found) throw new Error(`sample watch-snapshot.jsonl no longer has ${label}`);
    return found;
  };
  const w = S.watch;
  S.ev = {
    lineageMail: pick(w, "a lineage mail", (e) => e.event === "mail" && e.address?.kind === "lineage"),
    participantMail: pick(w, "a participant mail", (e) => e.event === "mail" && e.address?.kind === "participant"),
    workspaceMail: pick(w, "a workspace mail", (e) => e.event === "mail" && e.address?.kind === "workspace" && !e.pending),
    pendingMail: pick(w, "a pending mail", (e) => e.event === "mail" && e.pending === true),
    channelMessage: pick(w, "a channel message", (e) => e.event === "channel_message" && e.reason === "channel"),
    unreadableChannel: pick(w, "an unreadable channel", (e) => e.event === "unreadable" && e.reason === "channel"),
    unreadableMail: pick(w, "an unreadable mail", (e) => e.event === "unreadable" && e.reason === "mail"),
  };
  loaded = S;
  return S;
}

const clone = (value) => structuredClone(value);
const jsonl = (events) => events.map((event) => JSON.stringify(event)).join("\n") + "\n";
const EXTRA = { x_contract_extra: { nested: [1, "two"], flag: true } };
const withExtra = (object) => ({ ...object, ...clone(EXTRA) });
const without = (object, ...keys) => {
  const copy = clone(object);
  for (const key of keys) delete copy[key];
  return copy;
};

// Every event and every nested object a snapshot consumer reads gains an
// unknown field.
function extendedEvents(events) {
  return events.map((event) => {
    const copy = withExtra(event);
    if (copy.address) copy.address = withExtra(copy.address);
    return copy;
  });
}

function extendedParticipant(value) {
  const copy = withExtra(value);
  if (copy.participant) copy.participant = withExtra(copy.participant);
  return JSON.stringify(copy);
}

// Snapshot defects every validating snapshot consumer must refuse. Each
// broken event carries a fresh id, so a rendered id proves it was accepted.
// `notReadBy` names consumers that never look at the broken field.
function brokenEvents() {
  const { ev } = samples();
  const mail = (n, patch) => ({ ...clone(ev.workspaceMail), id: `20260101-999999-bad0${String(n).padStart(2, "0")}`, ...patch });
  const channel = (n, patch) => ({ ...clone(ev.channelMessage), id: `20260101-000000-999999-bad0${String(n).padStart(2, "0")}`, ...patch });
  const unreadable = (patch) => ({ ...clone(ev.unreadableChannel), ...patch });
  return [
    { label: "mail id is a number", event: { ...mail(0), id: 20260101999999 } },
    { label: "mail missing subject", event: without(mail(1), "subject") },
    { label: "mail missing reason", event: without(mail(2), "reason") },
    { label: "mail from is a number", event: mail(3, { from: 42 }) },
    { label: "unknown event discriminator", event: mail(4, { event: "mial" }) },
    { label: "mail with reason mention", event: mail(5, { reason: "mention" }) },
    { label: "unknown address kind", event: mail(6, { address: { kind: "room", name: "alpha" } }) },
    { label: "address missing name", event: mail(7, { address: { kind: "workspace" } }) },
    { label: "malformed participant address", event: without(mail(8, { address: { kind: "participant", name: "Not A Participant" } }), "room") },
    { label: "room is a number", event: mail(9, { room: 7 }) },
    { label: "workspace room disagrees with address", event: mail(10, { room: "beta" }) },
    { label: "lineage address carries a room", event: mail(11, { address: { kind: "lineage", name: "ember" } }) },
    { label: "pending is a string", event: mail(12, { pending: "true" }), notReadBy: ["watch-notice", "monitor"] },
    { label: "channel is a number", event: channel(13, { channel: 5 }) },
    { label: "channel message missing channel", event: without(channel(14), "channel") },
    { label: "unknown channel reason", event: channel(15, { reason: "dm" }) },
    { label: "malformed channel id", event: channel(16, { id: "not-an-id-bad016" }) },
    { label: "unknown unreadable reason", event: unreadable({ reason: "other" }) },
    { label: "unreadable channel is a number", event: unreadable({ channel: 3 }) },
    { label: "unreadable missing id", event: without(unreadable(), "id") },
  ];
}

// ------------------------------------------------------------------ stubs

let runCounter = 0;
function runDir() {
  const dir = path.join(ROOT, `run-${runCounter++}`);
  fs.mkdirSync(dir, { recursive: true });
  return { dir, control: path.join(dir, "control.json"), calls: path.join(dir, "calls.jsonl"), state: path.join(dir, "state") };
}

function writeStub(tool, body) {
  const file = path.join(ROOT, `${tool}-stub.mjs`);
  fs.writeFileSync(
    file,
    [
      "#!/usr/bin/env node",
      'import fs from "node:fs";',
      "const args = process.argv.slice(2);",
      'const control = JSON.parse(fs.readFileSync(process.env.CONTRACT_CONTROL, "utf8"));',
      `fs.appendFileSync(process.env.CONTRACT_CALLS, JSON.stringify({ tool: ${JSON.stringify(tool)}, args, participant: process.env.POST_PARTICIPANT ?? null }) + "\\n");`,
      ...body,
      "",
    ].join("\n"),
    { mode: 0o755 }
  );
  return file;
}

// post: replies per command key from control.post; natural exit so large
// stdout is never truncated by process.exit().
const POST_STUB = writeStub("post", [
  'const key = args[0] === "participant" ? `participant ${args[1]}` : args[0];',
  'const reply = key === "participant notice" ? control.post.watch : control.post[key];',
  'if (typeof reply === "string") process.stdout.write(reply);',
  "const exit = control.exit?.[key] ?? 0;",
  "if (exit) process.exit(exit);",
]);
// herdr: the named agent exists, is idle, and is not focused.
const HERDR_STUB = writeStub("herdr", [
  'if (args[0] === "agent" && args[1] === "get") process.stdout.write(JSON.stringify({ result: { agent: { name: args[2], agent_status: "idle", focused: false } } }));',
]);
const SINK_STUB = writeStub("sink", []);

function readCalls(file) {
  try {
    return fs.readFileSync(file, "utf8").split("\n").filter(Boolean).map((line) => JSON.parse(line));
  } catch {
    return [];
  }
}

// Async spawn behind a small limiter: runs overlap, but never so many that a
// stub misses an adapter's 4 s post-call deadline under load.
const MAX_RUNS = 4;
let active = 0;
const waiting = [];
async function limited(fn) {
  if (active >= MAX_RUNS) await new Promise((resolve) => waiting.push(resolve));
  active += 1;
  try {
    return await fn();
  } finally {
    active -= 1;
    waiting.shift()?.();
  }
}

function runNode(script, args, { input = "", env }) {
  return limited(
    () =>
      new Promise((resolve, reject) => {
        const child = spawn(process.execPath, [script, ...args], { env, stdio: ["pipe", "pipe", "pipe"] });
        let stdout = "";
        let stderr = "";
        child.stdout.setEncoding("utf8").on("data", (chunk) => (stdout += chunk));
        child.stderr.setEncoding("utf8").on("data", (chunk) => (stderr += chunk));
        child.on("error", reject);
        child.on("close", (status) => resolve({ status, stdout, stderr }));
        child.stdin.end(input);
      })
  );
}

function stubEnv(run) {
  return { CONTRACT_CONTROL: run.control, CONTRACT_CALLS: run.calls, POST_PARTICIPANT: undefined };
}

// ---------------------------------------------------------- mail adapters

const MAIL_ADAPTERS = [
  { name: "claude", script: "claude-mail.mjs", prefix: "POST_CLAUDE_HOOK", start: { hook_event_name: "SessionStart" }, bindingLine: false },
  { name: "codex", script: "codex-mail.mjs", prefix: "POST_CODEX_HOOK", start: { hook_event_name: "SessionStart" }, bindingLine: false },
  { name: "cursor", script: "cursor-mail.mjs", prefix: "POST_CURSOR_HOOK", start: { hook_event_name: "sessionStart" }, bindingLine: true },
  // Grok's first prompt is its session start: version, bind, snapshot, show.
  { name: "grok", script: "grok-mail.mjs", prefix: "POST_GROK_HOOK", start: { hook_event_name: "UserPromptSubmit" }, bindingLine: true },
];
const UNKNOWN = /inbox state is UNKNOWN \(not empty\)/;

async function runMail(adapter, post = {}) {
  const S = samples();
  const run = runDir();
  fs.writeFileSync(
    run.control,
    JSON.stringify({
      post: {
        version: S.raw.version,
        "participant bind": S.raw.bind,
        "participant show": S.raw.show,
        watch: S.raw.watch,
        ...post,
      },
    })
  );
  const input = { ...adapter.start, session_id: `contract-${path.basename(run.dir)}`, cwd: CWD };
  const result = await runNode(path.join(HOOKS, adapter.script), [], {
    input: JSON.stringify(input),
    env: {
      ...process.env,
      ...stubEnv(run),
      [`${adapter.prefix}_BIN`]: POST_STUB,
      [`${adapter.prefix}_STATE_DIR`]: run.state,
      [`${adapter.prefix}_THROTTLE_MS`]: "0",
    },
  });
  assert.equal(result.status, 0, `${adapter.name} adapter must exit 0: ${result.stderr}`);
  const out = JSON.parse(result.stdout);
  const calls = readCalls(run.calls);
  return {
    out,
    context: out.hookSpecificOutput?.additionalContext ?? "",
    calls,
    watchCalls: calls.filter((call) => call.args[0] === "watch"),
  };
}

const baselines = new Map();
function mailBaseline(adapter) {
  if (!baselines.has(adapter.name)) baselines.set(adapter.name, runMail(adapter).then((run) => run.context));
  return baselines.get(adapter.name);
}

function identityPattern(id, lineage) {
  return `[post] participant ${id}, continuing lineage ${lineage};`;
}

for (const adapter of MAIL_ADAPTERS) {
  describe(`${adapter.name} mail adapter`, { concurrency: true }, () => {
    test("exact samples render routes, ids, counts, and the sample identity", async () => {
      const { ev, show, bind, watch } = samples();
      const { context, watchCalls } = await runMail(adapter);
      const unread = watch.filter((e) => e.event === "mail" && e.pending !== true).map((e) => e.id);
      const pending = watch.filter((e) => e.event === "mail" && e.pending === true).map((e) => e.id);
      const tax = watch.filter((e) => e.event === "channel_message" && e.channel === "tax").length;
      const unreadable = watch.filter((e) => e.event === "unreadable").length;
      assert.doesNotMatch(context, UNKNOWN);
      assert.match(context, new RegExp(`waiting for lineage ${ev.lineageMail.address.name}, direct to you, room alpha\\.`));
      assert.ok(context.includes(`Direct mail id(s): ${unread.join(", ")}.`), context);
      assert.ok(context.includes(`Pending mail id(s): ${pending.join(", ")}.`), context);
      assert.ok(context.includes(`New channel message(s): #tax: ${tax} new.`), context);
      assert.ok(context.includes(`Unreadable mail: ${unreadable} item(s).`), context);
      assert.ok(context.includes(identityPattern(show.participant.id, "ember")), context);
      const binding = `prefix Post commands with POST_PARTICIPANT=${bind.participant.id}`;
      assert.equal(context.includes(binding), adapter.bindingLine, context);
      // The bound id from `participant bind` is the identity the snapshot runs as.
      assert.equal(watchCalls.length, 1);
      assert.equal(watchCalls[0].participant, bind.participant.id);
      for (const leak of ["plain channel note", "a mention", "beta", "codex-37cdb648", ev.channelMessage.id]) {
        assert.ok(!context.includes(leak), `envelope field leaked: ${leak}`);
      }
    });

    test("cursor-unusable snapshot (extra fields) renders as a normal snapshot", async () => {
      const { raw, cursorUnusable } = samples();
      const { context } = await runMail(adapter, { watch: raw.cursorUnusable });
      const mail = cursorUnusable.filter((e) => e.event === "mail").map((e) => e.id);
      assert.doesNotMatch(context, UNKNOWN);
      assert.ok(context.includes(`Direct mail id(s): ${mail.join(", ")}.`), context);
      // Count from the sample: join-from-now leaves pre-join events out of it.
      const taxNew = cursorUnusable.filter((e) => e.event === "channel_message" && e.channel === "tax").length;
      assert.ok(taxNew > 0);
      assert.ok(context.includes(`#tax: ${taxNew} new`), context);
      // The sample must carry display fields, or this check proves nothing.
      assert.ok(cursorUnusable.some((e) => e.display_name === "Reader" && e.pfp === "📮"), "sample lacks display fields");
      assert.ok(!context.includes("Reader"), "display_name stays out");
      assert.ok(!context.includes("📮"), "pfp stays out");
    });

    test("unknown fields at top level and in nested objects change nothing", async () => {
      const S = samples();
      const { context } = await runMail(adapter, {
        version: JSON.stringify(withExtra(S.version)),
        "participant bind": extendedParticipant(S.bind),
        "participant show": extendedParticipant(S.show),
        watch: jsonl(extendedEvents(S.watch)),
      });
      assert.equal(context, await mailBaseline(adapter));
    });

    test("optional snapshot fields removed are handled", async () => {
      const { watch, ev } = samples();
      const [noRoom, noPending, legacy, legacyRoute] = await Promise.all([
        // room is a workspace alias; without it the address still routes.
        runMail(adapter, { watch: jsonl(watch.map((e) => without(e, "room"))) }),
        // pending absent: the remote mail is ordinary unread mail.
        runMail(adapter, { watch: jsonl(watch.map((e) => without(e, "pending"))) }),
        // channel absent on an unreadable channel event: the legacy warning.
        runMail(adapter, { watch: jsonl(watch.map((e) => (e === ev.unreadableChannel ? without(e, "channel") : e))) }),
        // address absent where room is present (pre-address post): room routing.
        runMail(adapter, { watch: jsonl(watch.map((e) => (e.address?.kind === "workspace" ? without(e, "address") : e))) }),
      ]);
      assert.equal(noRoom.context, await mailBaseline(adapter));
      assert.ok(!noPending.context.includes("Pending mail"), noPending.context);
      assert.ok(noPending.context.includes(ev.pendingMail.id), noPending.context);
      assert.match(legacy.context, /Per-message delivery is unknown/);
      assert.ok(legacy.context.includes("Unreadable mail: 1 item(s)."), legacy.context);
      assert.ok(legacy.context.includes(ev.workspaceMail.id), legacy.context);
      assert.doesNotMatch(legacyRoute.context, UNKNOWN);
      assert.ok(legacyRoute.context.includes("room alpha"), legacyRoute.context);
      assert.ok(legacyRoute.context.includes(ev.workspaceMail.id), legacyRoute.context);
    });

    test("optional version and participant fields removed are handled", async () => {
      const S = samples();
      const [version, bindNoParticipant, bindNoTopId, showNoLineage, showNoParticipant, showNoInnerId] = await Promise.all([
        runMail(adapter, { version: JSON.stringify(without(S.version, "ok", "build_sha", "store_version", "version")) }),
        runMail(adapter, { "participant bind": JSON.stringify(without(S.bind, "participant")) }),
        runMail(adapter, { "participant bind": JSON.stringify(without(S.bind, "id")) }),
        runMail(adapter, { "participant show": JSON.stringify({ ...S.show, participant: without(S.show.participant, "lineage") }) }),
        runMail(adapter, { "participant show": JSON.stringify(without(S.show, "participant")) }),
        runMail(adapter, { "participant show": JSON.stringify({ ...S.show, participant: without(S.show.participant, "id") }) }),
      ]);
      assert.equal(version.context, await mailBaseline(adapter));
      assert.equal(bindNoParticipant.watchCalls[0]?.participant, S.bind.id);
      assert.equal(bindNoTopId.watchCalls[0]?.participant, S.bind.participant.id);
      assert.ok(!showNoLineage.context.includes("continuing lineage"), showNoLineage.context);
      assert.ok(showNoLineage.context.includes(S.ev.workspaceMail.id), "the notice still renders");
      assert.ok(!showNoParticipant.context.includes("continuing lineage"), showNoParticipant.context);
      assert.ok(showNoInnerId.context.includes(identityPattern(S.show.id, "ember")), showNoInnerId.context);
    });

    test("malformed snapshot events are refused, never rendered", async () => {
      const S = samples();
      await Promise.all(
        brokenEvents().map(async ({ label, event }) => {
          const alone = await runMail(adapter, { watch: jsonl([event]) });
          assert.match(alone.context, UNKNOWN, `${label}: a snapshot of only this line must be UNKNOWN`);
          assert.ok(!alone.context.includes("bad0"), `${label}: broken id rendered`);
        })
      );
      // Mixed with the valid sample, broken lines drop and the rest renders as usual.
      const mixed = await runMail(adapter, { watch: jsonl([...S.watch, ...brokenEvents().map((b) => b.event)]) });
      assert.equal(mixed.context, await mailBaseline(adapter));
    });

    test("malformed version output fails the capability check", async () => {
      const S = samples();
      const probeFailed = /could not verify installed post capabilities/;
      const cases = [
        ["capabilities missing", JSON.stringify(without(S.version, "capabilities")), probeFailed],
        ["capabilities is a string", JSON.stringify({ ...S.version, capabilities: "participants" }), probeFailed],
        ["participants capability absent", JSON.stringify({ ...S.version, capabilities: S.version.capabilities.filter((c) => c !== "participants") }), /lacks the participants capability/],
        ["not JSON", "post 0.9.0\n", probeFailed],
      ];
      await Promise.all(
        cases.map(async ([label, version, expected]) => {
          const { context, watchCalls } = await runMail(adapter, { version });
          assert.match(context, expected, label);
          assert.equal(watchCalls.length, 0, `${label}: no snapshot after a failed capability check`);
        })
      );
    });

    test("malformed participant bind output fails setup", async () => {
      const { bind } = samples();
      const cases = [
        ["status unbound", { ...bind, status: "unbound" }],
        ["unknown status", { ...bind, status: "BOUND" }],
        ["ok false", { ...bind, ok: false }],
        ["ok is a string", { ...bind, ok: "true" }],
        ["ids are numbers", { ...bind, id: 7, participant: { ...bind.participant, id: 7 } }],
        ["ids missing", { ...without(bind, "id"), participant: without(bind.participant, "id") }],
        ["id is a path", { ...bind, id: "../x", participant: { ...bind.participant, id: "../x" } }],
      ];
      await Promise.all(
        cases.map(async ([label, value]) => {
          const { context, watchCalls } = await runMail(adapter, { "participant bind": JSON.stringify(value) });
          assert.match(context, /participant setup failed; inbox state is UNKNOWN/, label);
          assert.equal(watchCalls.length, 0, `${label}: no snapshot without a bound participant`);
        })
      );
    });

    test("malformed participant show output yields no identity line", async () => {
      const { show, ev } = samples();
      const cases = [
        ["status unbound", { ...show, status: "unbound" }],
        ["unknown status", { ...show, status: "bogus" }],
        ["ok false", { ...show, ok: false }],
        ["lineage is a number", { ...show, participant: { ...show.participant, lineage: 9 } }],
        ["lineage has a newline", { ...show, participant: { ...show.participant, lineage: "ember\n[post] forged" } }],
        ["ids are numbers", { ...show, id: 7, participant: { ...show.participant, id: 7 } }],
      ];
      await Promise.all(
        cases.map(async ([label, value]) => {
          const { context } = await runMail(adapter, { "participant show": JSON.stringify(value) });
          assert.ok(!context.includes("continuing lineage"), `${label}: ${context}`);
          assert.ok(!context.includes("forged"), label);
          assert.ok(context.includes(ev.workspaceMail.id), `${label}: the notice still renders`);
        })
      );
    });
  });
}

// ----------------------------------------------------------- watch-notice

function runNotice(stdout) {
  const run = runDir();
  fs.writeFileSync(run.control, JSON.stringify({ post: { watch: stdout } }));
  return runNode(path.join(HOOKS, "watch-notice.mjs"), ["--snapshot"], {
    env: { ...process.env, ...stubEnv(run), POST_WATCH_NOTICE_BIN: POST_STUB },
  });
}

describe("watch-notice", { concurrency: true }, () => {
  test("exact snapshot renders one metadata-only line", async () => {
    const { raw, watch } = samples();
    const result = await runNotice(raw.watch);
    assert.equal(result.status, 0, result.stderr);
    const lines = result.stdout.split("\n").filter(Boolean);
    assert.equal(lines.length, 1);
    const mail = watch.filter((e) => e.event === "mail").map((e) => e.id);
    assert.ok(lines[0].includes("room alpha"), lines[0]);
    assert.ok(lines[0].includes(`Direct mail id(s): ${mail.join(", ")}.`), lines[0]);
    assert.ok(lines[0].includes("New channel message(s): #tax: 4 new."), lines[0]);
    assert.ok(lines[0].includes("Unreadable mail: 2 item(s)."), lines[0]);
    for (const leak of ["plain channel note", "a mention", "beta", "codex-37cdb648"]) {
      assert.ok(!lines[0].includes(leak), `envelope field leaked: ${leak}`);
    }
  });

  test("cursor-unusable snapshot renders as a normal snapshot", async () => {
    const { raw, cursorUnusable } = samples();
    const result = await runNotice(raw.cursorUnusable);
    assert.equal(result.status, 0, result.stderr);
    for (const e of cursorUnusable.filter((x) => x.event === "mail")) assert.ok(result.stdout.includes(e.id));
    const taxNew = cursorUnusable.filter((e) => e.event === "channel_message" && e.channel === "tax").length;
    assert.ok(taxNew > 0);
    assert.ok(result.stdout.includes(`#tax: ${taxNew} new`), result.stdout);
    assert.ok(cursorUnusable.some((e) => e.display_name === "Reader" && e.pfp === "📮"), "sample lacks display fields");
    assert.ok(!result.stdout.includes("Reader"), "display_name stays out");
    assert.ok(!result.stdout.includes("📮"), "pfp stays out");
    assert.doesNotMatch(result.stdout, UNKNOWN);
  });

  test("unknown fields change nothing", async () => {
    const { raw, watch } = samples();
    const [extended, baseline] = await Promise.all([runNotice(jsonl(extendedEvents(watch))), runNotice(raw.watch)]);
    assert.equal(extended.stdout, baseline.stdout);
  });

  test("optional fields removed are handled", async () => {
    const { raw, watch, ev } = samples();
    const [baseline, noPending, noRoom, legacy, legacyRoute] = await Promise.all([
      runNotice(raw.watch),
      runNotice(jsonl(watch.map((e) => without(e, "pending")))),
      runNotice(jsonl(watch.map((e) => without(e, "room")))),
      runNotice(jsonl(watch.map((e) => (e === ev.unreadableChannel ? without(e, "channel") : e)))),
      runNotice(jsonl(watch.map((e) => (e.address?.kind === "workspace" ? without(e, "address") : e)))),
    ]);
    assert.equal(noPending.stdout, baseline.stdout);
    assert.equal(noRoom.status, 0);
    assert.doesNotMatch(noRoom.stdout, UNKNOWN);
    assert.ok(noRoom.stdout.includes(ev.workspaceMail.id));
    assert.match(legacy.stdout, /Per-message delivery is unknown/);
    assert.ok(legacy.stdout.includes("Unreadable mail: 1 item(s)."));
    assert.equal(legacyRoute.stdout, baseline.stdout);
  });

  test("any malformed snapshot event makes the batch UNKNOWN", async () => {
    const { watch } = samples();
    await Promise.all(
      brokenEvents()
        .filter(({ notReadBy = [] }) => !notReadBy.includes("watch-notice"))
        .map(async ({ label, event }) => {
          const result = await runNotice(jsonl([...watch, event]));
          assert.equal(result.status, 0, label);
          assert.match(result.stdout, UNKNOWN, label);
          assert.equal(result.stdout.split("\n").filter(Boolean).length, 1, label);
          assert.ok(!result.stdout.includes("bad0") && !result.stdout.includes("#tax"), `${label}: nothing rendered`);
        })
    );
  });
});

// ---------------------------------------------------- codex-notify-monitor

async function runMonitor(stdout) {
  const run = runDir();
  fs.writeFileSync(run.control, JSON.stringify({ post: { watch: stdout } }));
  const result = await runNode(path.join(HOOKS, "codex-notify-monitor.mjs"), ["alpha"], {
    env: {
      ...process.env,
      ...stubEnv(run),
      POST_CODEX_NOTIFY_POST_BIN: POST_STUB,
      POST_CODEX_NOTIFY_HERDR_BIN: HERDR_STUB,
      POST_CODEX_NOTIFY_CMUX_BIN: SINK_STUB,
      POST_CODEX_NOTIFY_HERDR_AGENT: "lane-bot",
      POST_CODEX_NOTIFY_CHANNELS: "tax,broken",
      POST_CODEX_NOTIFY_STATE: path.join(run.state, "seen.json"),
    },
  });
  const prompts = readCalls(run.calls).filter((call) => call.tool === "herdr" && call.args[1] === "prompt");
  return { ...result, prompt: prompts.at(-1)?.args[3] ?? null, prompts: prompts.length };
}

describe("codex-notify-monitor", { concurrency: true }, () => {
  test("exact snapshot rings with every typed route", async () => {
    const { raw, watch, ev } = samples();
    const result = await runMonitor(raw.watch);
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stderr, "");
    const direct = watch.filter((e) => e.reason === "mail").length;
    const channel = watch.length - direct;
    assert.ok(result.prompt.includes(`${direct} direct messages and ${channel} selected channel messages are waiting`), result.prompt);
    for (const ref of [
      `lineage:ember:${ev.lineageMail.id}`,
      `participant:${ev.participantMail.address.name}:${ev.participantMail.id}`,
      `alpha:${ev.workspaceMail.id}`,
      `alpha:${ev.pendingMail.id}`,
      `#tax:${ev.channelMessage.id}`,
      "unreadable delivery",
    ]) {
      assert.ok(result.prompt.includes(ref), `missing ${ref}: ${result.prompt}`);
    }
    assert.ok(!result.prompt.includes("plain channel note") && !result.prompt.includes("beta"));
  });

  test("cursor-unusable snapshot rings as a normal snapshot", async () => {
    const { raw, cursorUnusable } = samples();
    const result = await runMonitor(raw.cursorUnusable);
    assert.equal(result.status, 0, result.stderr);
    for (const e of cursorUnusable) assert.ok(result.prompt.includes(e.id), e.id);
  });

  test("unknown fields change nothing", async () => {
    const { raw, watch } = samples();
    const [extended, baseline] = await Promise.all([runMonitor(jsonl(extendedEvents(watch))), runMonitor(raw.watch)]);
    assert.equal(extended.status, 0, extended.stderr);
    assert.equal(extended.prompt, baseline.prompt);
  });

  test("optional fields removed are handled", async () => {
    const { raw, watch, ev } = samples();
    const baseline = (await runMonitor(raw.watch)).prompt;
    await Promise.all(
      [
        ["room removed", jsonl(watch.map((e) => without(e, "room")))],
        ["pending removed", jsonl(watch.map((e) => without(e, "pending")))],
        ["workspace address removed", jsonl(watch.map((e) => (e.address?.kind === "workspace" ? without(e, "address") : e)))],
      ].map(async ([label, stdout]) => {
        const result = await runMonitor(stdout);
        assert.equal(result.status, 0, `${label}: ${result.stderr}`);
        assert.equal(result.prompt, baseline, label);
      })
    );
    const legacy = await runMonitor(jsonl(watch.map((e) => (e === ev.unreadableChannel ? without(e, "channel") : e))));
    assert.equal(legacy.status, 0, legacy.stderr);
    assert.match(legacy.prompt, /^Post compatibility warning: .*Per-message delivery is unknown/);
  });

  test("any malformed snapshot event fails without ringing", async () => {
    const { watch } = samples();
    await Promise.all(
      brokenEvents()
        .filter(({ notReadBy = [] }) => !notReadBy.includes("monitor"))
        .map(async ({ label, event }) => {
          const result = await runMonitor(jsonl([...watch, event]));
          assert.equal(result.status, 1, label);
          assert.match(result.stderr, /snapshot output was malformed; notification state is unknown/, label);
          assert.equal(result.prompts, 0, `${label}: rang anyway`);
        })
    );
  });
});

// ------------------------------------------------------ doorbell installers

const INSTALLERS = [
  {
    name: "install-codex-doorbell",
    platform: "darwin",
    env: (bin) => ({ POST_CODEX_DOORBELL_LAUNCHCTL_BIN: bin }),
    artifact: (home) => path.join(home, "Library", "LaunchAgents", "dev.post.codex-doorbell.lane-bot.plist"),
    resolvesParticipant: false,
  },
  {
    name: "install-systemd-doorbell",
    platform: "linux",
    env: (bin) => ({ POST_CODEX_DOORBELL_SYSTEMCTL_BIN: bin }),
    artifact: (home) => path.join(home, ".config", "systemd", "user", "post-codex-doorbell@lane-bot.service"),
    resolvesParticipant: true,
  },
];

async function runInstaller(installer, post = {}, args = ["--room", "alpha", "--channel", "tax", "--channel", "broken"]) {
  const S = samples();
  const run = runDir();
  const home = path.join(run.dir, "home");
  fs.writeFileSync(
    run.control,
    JSON.stringify({
      post: { rooms: S.raw.rooms, channels: S.raw.channels, watch: S.raw.watch, "participant show": S.raw.show, ...post },
    })
  );
  const result = await runNode(path.join(HOOKS, `${installer.name}.mjs`), [...args, "--agent", "lane-bot"], {
    env: {
      ...process.env,
      ...stubEnv(run),
      ...installer.env(SINK_STUB),
      POST_CODEX_DOORBELL_PLATFORM: installer.platform,
      POST_CODEX_DOORBELL_HOME: home,
      POST_CODEX_DOORBELL_INSTALL_DIR: path.join(home, "hooks"),
      POST_CODEX_DOORBELL_POST_BIN: POST_STUB,
      POST_CODEX_DOORBELL_HERDR_BIN: HERDR_STUB,
      POST_MAIL_ROOT: undefined,
    },
  });
  const calls = readCalls(run.calls);
  const artifact = installer.artifact(home);
  return {
    ...result,
    calls,
    artifact: fs.existsSync(artifact) ? fs.readFileSync(artifact, "utf8") : null,
    serviceCalls: calls.filter((call) => call.tool === "sink").length,
  };
}

function assertRefused(result, pattern, label) {
  assert.equal(result.status, 1, `${label}: ${result.stderr}`);
  assert.match(result.stderr, pattern, label);
  assert.equal(result.artifact, null, `${label}: wrote the job anyway`);
  assert.equal(result.serviceCalls, 0, `${label}: touched the service manager`);
}

for (const installer of INSTALLERS) {
  describe(installer.name, { concurrency: true }, () => {
    test("exact samples install for a listed room and member channels", async () => {
      const { show } = samples();
      const result = await runInstaller(installer);
      assert.equal(result.status, 0, result.stderr);
      assert.ok(result.artifact, "job file written");
      const postCalls = result.calls.filter((call) => call.tool === "post").map((call) => call.args.join(" "));
      assert.ok(postCalls.includes("rooms") && postCalls.includes("channels"), postCalls.join("; "));
      assert.ok(postCalls.includes("watch --room alpha --snapshot"), postCalls.join("; "));
      assert.ok(result.artifact.includes("tax,broken"), "selected channels pinned");
      if (installer.resolvesParticipant) {
        assert.match(result.artifact, new RegExp(`^Environment=POST_PARTICIPANT=${show.id}$`, "m"));
        const watchCall = result.calls.find((call) => call.args[0] === "watch");
        assert.equal(watchCall.participant, show.id, "the probe runs as the resolved participant");
      }
    });

    test("routing reads room names and channel membership from the samples", async () => {
      const { show } = samples();
      // The systemd installer first requires the acting participant to be
      // bound to --room; bind the sample participant there so the rooms and
      // channels checks are the ones under test.
      const boundTo = (room) =>
        installer.resolvesParticipant ? { "participant show": JSON.stringify({ ...show, participant: { ...show.participant, workspace: room } }) } : {};
      const [unlisted, noChannel, nonMember] = await Promise.all([
        runInstaller(installer, boundTo("zeta"), ["--room", "zeta"]),
        runInstaller(installer, boundTo("alpha"), ["--room", "alpha", "--channel", "nope"]),
        runInstaller(installer, boundTo("beta"), ["--room", "beta", "--channel", "broken"]),
      ]);
      assertRefused(unlisted, /room 'zeta' is not registered/, "unlisted room");
      assertRefused(noChannel, /channel 'nope' is not listed/, "unlisted channel");
      assertRefused(nonMember, /room 'beta' is not a member of channel 'broken'/, "non-member channel");
      if (installer.resolvesParticipant) {
        assertRefused(await runInstaller(installer, {}, ["--room", "beta"]), /is not bound to room 'beta'/, "participant bound elsewhere");
      }
    });

    test("unknown fields at top level and in nested objects change nothing", async () => {
      const S = samples();
      const rooms = withExtra(S.rooms);
      rooms.rooms = rooms.rooms.map((row) => ({ ...withExtra(row), blocked: row.blocked.map(withExtra) }));
      const channels = withExtra(S.channels);
      channels.channels = channels.channels.map(withExtra);
      const result = await runInstaller(installer, {
        rooms: JSON.stringify(rooms),
        channels: JSON.stringify(channels),
        watch: jsonl(extendedEvents(S.watch)),
        "participant show": extendedParticipant(S.show),
      });
      assert.equal(result.status, 0, result.stderr);
      assert.ok(result.artifact);
    });

    test("optional fields removed are handled", async () => {
      const S = samples();
      const rooms = without(S.rooms, "count");
      rooms.rooms = rooms.rooms.map((row) => ({ name: row.name }));
      const channels = without(S.channels, "count", "participant", "pending", "archived_hidden");
      channels.channels = channels.channels.map((row) => ({ name: row.name, members: row.members }));
      const show = without(S.show, "provenance");
      show.participant = without(show.participant, "lineage", "lineage_since", "workspace_path");
      const [trimmed, empty] = await Promise.all([
        runInstaller(installer, {
          rooms: JSON.stringify(rooms),
          channels: JSON.stringify(channels),
          watch: S.raw.cursorUnusable,
          "participant show": JSON.stringify(show),
        }),
        runInstaller(installer, { watch: "" }),
      ]);
      assert.equal(trimmed.status, 0, trimmed.stderr);
      assert.ok(trimmed.artifact);
      assert.equal(empty.status, 0, `an empty snapshot is a valid probe: ${empty.stderr}`);
    });

    test("malformed rooms, channels, and snapshot output refuse the install", async () => {
      const { rooms, channels } = samples();
      const cases = [
        ["rooms not JSON", { rooms: "alpha\nbeta\n" }, /`post rooms` printed malformed output/],
        ["rooms ok false", { rooms: JSON.stringify({ ...rooms, ok: false }) }, /`post rooms` printed an unexpected schema/],
        ["rooms ok is a string", { rooms: JSON.stringify({ ...rooms, ok: "true" }) }, /`post rooms` printed an unexpected schema/],
        ["rooms is an object", { rooms: JSON.stringify({ ...rooms, rooms: { alpha: {} } }) }, /`post rooms` printed an unexpected schema/],
        ["room name is a number", { rooms: JSON.stringify({ ...rooms, rooms: [...rooms.rooms, { name: 5 }] }) }, /malformed room entries/],
        ["room name missing", { rooms: JSON.stringify({ ...rooms, rooms: [...rooms.rooms, { path: "/x" }] }) }, /malformed room entries/],
        ["channels ok missing", { channels: JSON.stringify(without(channels, "ok")) }, /`post channels` printed an unexpected schema/],
        ["channel members missing", { channels: JSON.stringify({ ...channels, channels: [without(channels.channels[0], "members")] }) }, /malformed channel entries/],
        ["channel member is a number", { channels: JSON.stringify({ ...channels, channels: [{ ...channels.channels[0], members: ["alpha", 3] }] }) }, /malformed channel entries/],
        ["channel name is a number", { channels: JSON.stringify({ ...channels, channels: [{ ...channels.channels[0], name: 1 }] }) }, /malformed channel entries/],
        ["snapshot line is an array", { watch: "[]\n" }, /--snapshot` printed malformed output/],
        ["snapshot line is a string", { watch: '"mail"\n' }, /--snapshot` printed malformed output/],
        ["snapshot line truncated", { watch: '{"event":"mail",\n' }, /--snapshot` printed malformed output/],
      ];
      await Promise.all(cases.map(async ([label, post, pattern]) => assertRefused(await runInstaller(installer, post), pattern, label)));
    });

    if (installer.resolvesParticipant) {
      test("malformed participant show output refuses the install", async () => {
        const { show } = samples();
        const unbound = /acting participant is unbound or malformed/;
        const cases = [
          ["not JSON", "participant: bound\n", /`post participant show` printed malformed output/],
          ["status unbound", JSON.stringify({ ...show, status: "unbound" }), unbound],
          ["unknown status", JSON.stringify({ ...show, status: "bogus" }), unbound],
          ["ok false", JSON.stringify({ ...show, ok: false }), unbound],
          ["id is a number", JSON.stringify({ ...show, id: 7 }), unbound],
          ["id is a path", JSON.stringify({ ...show, id: "../x", participant: { ...show.participant, id: "../x" } }), unbound],
          ["participant id disagrees", JSON.stringify({ ...show, participant: { ...show.participant, id: "codex-37cdb648" } }), unbound],
          ["participant missing", JSON.stringify(without(show, "participant")), unbound],
          ["workspace missing", JSON.stringify({ ...show, participant: without(show.participant, "workspace") }), /is not bound to room 'alpha'/],
        ];
        await Promise.all(
          cases.map(async ([label, value, pattern]) => assertRefused(await runInstaller(installer, { "participant show": value }), pattern, label))
        );
      });
    }
  });
}
