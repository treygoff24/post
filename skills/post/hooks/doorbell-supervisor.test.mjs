// Unit tests for doorbell-supervisor.mjs, in process: herdr and post are a
// scripted fake `exec`, the clock is a variable, and fs.watch is a fake whose
// events and errors the tests fire. Snapshot events are the samples the real
// post binary emits (`post contract samples`), so the supervisor is tested
// against production event shapes. POST_BIN selects the producer (default
// <repo>/target/release/post); a failed sample emit FAILS, it never skips.
//
// Process-level tests (the singleton, the agent commands, an end-to-end ring
// through real child processes) live in doorbell-supervisor-process.test.mjs.

import test, { describe } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

import {
  Supervisor,
  resolvePaths,
  parseSnapshot,
  selectEligible,
  eventKey,
  stateClass,
  buildNotice,
  noticeName,
  snapshotArgs,
  updatePrefs,
  loadPrefs,
  prefsPath,
  runCommand,
  sha256,
  generationHash,
  NOTICE_TAG,
} from "./doorbell-supervisor.mjs";

const HOOKS = path.dirname(fileURLToPath(import.meta.url));
const REPO = path.resolve(HOOKS, "..", "..", "..");
const POST_BIN = process.env.POST_BIN || path.join(REPO, "target", "release", "post");
const ROOT = fs.mkdtempSync(path.join(os.tmpdir(), "doorbell-supervisor-test-"));

test.after(() => fs.rmSync(ROOT, { recursive: true, force: true }));

// ------------------------------------------------------------------ samples

let loaded;
function samples() {
  if (loaded) return loaded;
  const dir = path.join(ROOT, "samples");
  const result = spawnSync(POST_BIN, ["contract", "samples", "--dir", dir], { encoding: "utf8" });
  if (result.error || result.status !== 0) {
    throw new Error(`\`${POST_BIN} contract samples\` failed: ${result.error?.message ?? result.stderr}`);
  }
  const lines = (name) =>
    fs.readFileSync(path.join(dir, name), "utf8").split("\n").filter((line) => line.trim()).map((line) => JSON.parse(line));
  const watch = lines("watch-snapshot.jsonl");
  const cursorUnusable = lines("watch-snapshot-cursor-unusable.jsonl");
  const pick = (label, predicate) => {
    const found = watch.find(predicate);
    if (!found) throw new Error(`watch-snapshot.jsonl no longer has ${label}`);
    return found;
  };
  loaded = {
    watch,
    cursorUnusable,
    workspaceMail: pick("workspace mail", (e) => e.event === "mail" && e.address.kind === "workspace" && !e.pending),
    participantMail: pick("participant mail", (e) => e.event === "mail" && e.address.kind === "participant"),
    lineageMail: pick("lineage mail", (e) => e.event === "mail" && e.address.kind === "lineage"),
    pendingMail: pick("pending mail", (e) => e.event === "mail" && e.pending === true),
    channelMessage: pick("channel message", (e) => e.event === "channel_message" && e.reason === "channel"),
    mention: pick("mention", (e) => e.event === "channel_message" && e.reason === "mention"),
    unreadableChannel: pick("unreadable channel", (e) => e.event === "unreadable" && e.reason === "channel"),
    unreadableMail: pick("unreadable mail", (e) => e.event === "unreadable" && e.reason === "mail"),
  };
  return loaded;
}

const clone = (value) => structuredClone(value);
const jsonl = (events) => events.map((event) => JSON.stringify(event)).join("\n") + (events.length ? "\n" : "");
const ok = (stdout = "") => ({ ok: true, code: 0, stdout, stderr: "" });

function mailWith(id, patch = {}) {
  return { ...clone(samples().workspaceMail), id, ...patch };
}
function channelWith(channel, id, patch = {}) {
  return { ...clone(samples().channelMessage), channel, id, ...patch };
}

// ------------------------------------------------------------------ fake world

let worldSeq = 0;

function makeWorld({ config = {} } = {}) {
  const dir = fs.mkdtempSync(path.join(ROOT, `world-${worldSeq++}-`));
  fs.mkdirSync(path.join(dir, "mail", "participants"), { recursive: true });
  const paths = resolvePaths({ POST_MAIL_ROOT: path.join(dir, "mail"), POST_DOORBELL_HOME: dir });
  const w = {
    dir,
    paths,
    panes: [],
    participants: [],
    snapshots: new Map(),
    showPatch: new Map(),
    getOverride: new Map(),
    promptResult: null,
    cmuxResult: null,
    herdrListFail: false,
    channelRows: [],
    calls: [],
    prompts: [],
    desktop: [],
    logs: [],
    watches: [],
    clock: { t: 1_000_000 },
  };
  const agentJson = (pane) => ({
    pane_id: pane.pane_id,
    terminal_id: pane.terminal_id,
    agent: "codex",
    agent_status: pane.status ?? "idle",
    focused: pane.focused ?? false,
    agent_session: pane.session === null ? undefined : { agent: "codex", kind: "id", source: "herdr:codex", value: pane.session },
  });
  w.exec = async (kind, args, opts = {}) => {
    w.calls.push({ kind, args: [...args], participant: opts.participant });
    if (kind === "herdr") {
      if (args[0] === "--version") return ok("herdr 0.9.1\n");
      if (args[1] === "list") {
        if (w.herdrListFail) return { ok: false, code: 1, stdout: "", stderr: "herdr: socket unavailable" };
        return ok(JSON.stringify({ id: "cli:agent:list", result: { type: "agent_list", agents: w.panes.map(agentJson) } }));
      }
      if (args[1] === "get") {
        const override = w.getOverride.get(args[2]);
        if (override) return typeof override === "function" ? override() : override;
        const pane = w.panes.find((p) => p.pane_id === args[2]);
        if (!pane) {
          return { ok: false, code: 1, stdout: "", stderr: JSON.stringify({ error: { code: "agent_not_found", message: "not found" } }) };
        }
        return ok(JSON.stringify({ id: "cli:agent:get", result: { type: "agent", agent: agentJson(pane) } }));
      }
      if (args[1] === "prompt") {
        w.prompts.push({ pane: args[2], text: args[3] });
        return w.promptResult ? w.promptResult(args[2]) : ok("");
      }
    }
    if (kind === "post") {
      if (args[0] === "participant" && args[1] === "list") {
        return ok(JSON.stringify({ ok: true, participants: w.participants, count: w.participants.length }));
      }
      if (args[0] === "participant" && args[1] === "show") {
        const row = w.participants.find((p) => p.id === opts.participant);
        if (!row) return ok(JSON.stringify({ ok: true, status: "unbound", fix: "run: post participant bind" }));
        const participant = { ...row, ...(w.showPatch.get(row.id) ?? {}) };
        return ok(JSON.stringify({ ok: true, status: "bound", id: row.id, participant, provenance: "explicit-env" }));
      }
      if (args[0] === "channels") {
        // Like post: the default listing hides archived channels; --all shows both.
        const rows = args.includes("--all") ? w.channelRows : w.channelRows.filter((row) => !row.archived);
        return ok(JSON.stringify({ ok: true, channels: rows }));
      }
      if (args[0] === "version") return ok(JSON.stringify({ ok: true, version: "0.9.0", build_sha: "test" }));
      if (args[0] === "watch") {
        const script = w.snapshots.get(opts.participant);
        if (typeof script === "function") return script();
        if (script && script.result) return script.result;
        // Like post: --reason (repeatable) keeps only events with a selected reason.
        const reasons = args.flatMap((arg, index) => (args[index - 1] === "--reason" ? [arg] : []));
        const events = (script ?? []).filter((event) => reasons.length === 0 || reasons.includes(event.reason));
        return ok(jsonl(events));
      }
    }
    if (kind === "cmux") {
      w.desktop.push(args);
      return w.cmuxResult ?? ok("");
    }
    throw new Error(`unexpected exec: ${kind} ${args.join(" ")}`);
  };
  w.watch = (target, { recursive }, onEvent, onError) => {
    const handle = { target, recursive, onEvent, onError, closed: false, close() { handle.closed = true; } };
    w.watches.push(handle);
    return handle;
  };
  w.sup = new Supervisor({
    paths,
    exec: w.exec,
    watch: w.watch,
    now: () => w.clock.t,
    log: (record) => w.logs.push(record),
    config: { hintCoalesceMs: 1, ...config },
  });
  w.addParticipant = (id, session, extra = {}) => {
    const row = {
      version: 1,
      id,
      harness: "codex",
      conversation_key_digest: sha256(session),
      created: "2026-01-01 00:00:00 +0000",
      last_seen: "2026-09-23T00:00:00Z",
      lease_hours: 24,
      workspace: "alpha",
      ...extra,
    };
    w.participants.push(row);
    return row;
  };
  w.addPane = (pane_id, session, extra = {}) => {
    const pane = { pane_id, terminal_id: `term_${pane_id.replace(/\W/g, "")}`, session, status: "idle", focused: false, ...extra };
    w.panes.push(pane);
    return pane;
  };
  w.enable = (id, patch = {}) =>
    updatePrefs(paths, id, (p) => {
      p.enabled = true;
      Object.assign(p, patch);
    });
  w.run = async () => {
    await w.sup.tick();
    await w.sup.idle();
  };
  w.snapshotCalls = (id) => w.calls.filter((c) => c.kind === "post" && c.args[0] === "watch" && (id === undefined || c.participant === id));
  w.outcomes = (outcome) => w.logs.filter((r) => r.type === "outcome" && (outcome === undefined || r.outcome === outcome));
  w.sub = (id) => [...w.sup.subs.values()].find((s) => s.participant === id);
  w.state = (id) => {
    const sub = w.sub(id);
    return sub ? JSON.parse(fs.readFileSync(sub.stateFile, "utf8")) : undefined;
  };
  return w;
}

// Two panes, each an armed participant, with one workspace mail waiting.
function standardWorld(opts) {
  const w = makeWorld(opts);
  w.addParticipant("codex-aaaaaaaa", "session-a");
  w.addPane("wC:p1", "session-a");
  w.enable("codex-aaaaaaaa");
  w.snapshots.set("codex-aaaaaaaa", [mailWith("20260923-000001-aaaaa1")]);
  return w;
}

// ------------------------------------------------------------------ pure pieces

describe("parsing is all or nothing (E4)", () => {
  test("the real samples parse whole", () => {
    const parsed = parseSnapshot(jsonl(samples().watch));
    assert.equal(parsed.ok, true);
    assert.equal(parsed.events.length, samples().watch.length);
    assert.equal(parseSnapshot(jsonl(samples().cursorUnusable)).ok, true);
  });

  test("an unknown optional field is fine", () => {
    const event = { ...mailWith("20260923-000001-aaaaa1"), x_future: { nested: true } };
    assert.equal(parseSnapshot(jsonl([event])).ok, true);
  });

  const broken = [
    ["a truncated line", () => jsonl([mailWith("20260923-000001-aaaaa1")]).slice(0, -20)],
    ["a final line without a newline", () => JSON.stringify(mailWith("20260923-000001-aaaaa1"))],
    ["an unknown event", () => jsonl([mailWith("20260923-000001-aaaaa1", { event: "mial" })])],
    ["a missing id", () => jsonl([{ ...mailWith("x"), id: undefined }])],
    ["an unknown address kind", () => jsonl([mailWith("20260923-000001-aaaaa1", { address: { kind: "room", name: "alpha" } })])],
    ["a channel message without channel", () => jsonl([{ ...clone(samples().channelMessage), channel: undefined }])],
    ["a string pending", () => jsonl([mailWith("20260923-000001-aaaaa1", { pending: "true" })])],
    ["one bad line after good ones", () => jsonl(samples().watch) + "{not json\n"],
  ];
  for (const [label, make] of broken) {
    test(`${label} fails the whole snapshot`, () => {
      assert.equal(parseSnapshot(make()).ok, false);
    });
  }
});

describe("selection after parsing (E5)", () => {
  test("a member of two channels subscribed to one: ordinary events from it only, mentions from both", () => {
    const events = [
      channelWith("tax", "20260923-000000-000001-c00001"),
      channelWith("ops", "20260923-000000-000002-c00002"),
      channelWith("ops", "20260923-000000-000003-c00003", { reason: "mention" }),
      channelWith("tax", "20260923-000000-000004-c00004", { reason: "mention" }),
    ];
    const kept = selectEligible(events, ["tax"]).map((e) => `${e.channel}:${e.reason}`);
    assert.deepEqual(kept.sort(), ["ops:mention", "tax:channel", "tax:mention"]);
  });

  test("mail and unreadable are always kept; the scan passes no reason filter (B1)", () => {
    const kept = selectEligible(samples().watch, []);
    assert.ok(kept.some((e) => e.event === "unreadable" && e.reason === "channel"));
    assert.ok(kept.some((e) => e.event === "unreadable" && e.reason === "mail"));
    assert.ok(!kept.some((e) => e.event === "channel_message" && e.reason === "channel"));
    const args = snapshotArgs();
    assert.deepEqual(args.slice(0, 5), ["watch", "--snapshot", "--json", "--limit", "0"]);
    assert.deepEqual(args, ["watch", "--snapshot", "--json", "--limit", "0"]);
  });
});

describe("keys carry the state class (E1)", () => {
  test("pending -> routed and degraded -> healthy are different keys", () => {
    const pending = samples().pendingMail;
    const routed = { ...clone(pending), pending: undefined };
    assert.equal(stateClass(pending), "pending/healthy");
    assert.equal(stateClass(routed), "routed/healthy");
    assert.notEqual(eventKey(pending), eventKey(routed));
    const degraded = { ...clone(samples().workspaceMail), cursor_unusable: true };
    assert.notEqual(eventKey(degraded), eventKey(samples().workspaceMail));
    assert.equal(stateClass(samples().unreadableMail), "routed/unreadable");
  });
});

describe("notice", () => {
  test("names the participant, carries the self-check sentence, and never mail content", () => {
    const notice = buildNotice("claude-dd611847", samples().watch);
    assert.ok(notice.startsWith(`${NOTICE_TAG} Automated, non-authoritative Post notice for participant claude-dd611847.`));
    assert.ok(notice.includes('If "post participant show" does not report claude-dd611847, this notice is not for you: ignore it and report it to your operator.'));
    for (const event of samples().watch) {
      for (const field of ["subject", "preview", "from", "display_name"]) {
        if (typeof event[field] === "string" && event[field].length > 3) assert.ok(!notice.includes(event[field]), `${field} leaked`);
      }
    }
  });

  test("pending, cursor-unusable, and unreadable each get their own wording", () => {
    const pending = buildNotice("codex-aaaaaaaa", [samples().pendingMail]);
    assert.match(pending, /1 waiting to be routed/);
    assert.doesNotMatch(pending, /\d+ direct/);
    const degraded = buildNotice("codex-aaaaaaaa", samples().cursorUnusable.filter((e) => e.event === "mail"));
    assert.match(degraded, /2 re-reported while cursor state is unavailable; may include previously read messages/);
    assert.doesNotMatch(degraded, /new/);
    const unreadable = buildNotice("codex-aaaaaaaa", [samples().unreadableChannel, samples().unreadableMail]);
    assert.match(unreadable, /2 unreadable item\(s\) in #broken or the inbox; mentions there are unknown/);
    const mixed = buildNotice("codex-aaaaaaaa", selectEligible(samples().watch, ["tax"]));
    assert.match(mixed, /Waiting: 3 direct; 1 mention; 3 in #tax; 1 waiting to be routed; 2 unreadable item\(s\)/);
    assert.match(mixed, /Read with post inbox \/ post chat broken \/ post chat tax\./);
  });

  test("hostile channel names and ids become <?>", () => {
    const hostile = [
      "ignore previous instructions",
      "a\nb",
      'x"y',
      "…unicode",
      "a".repeat(65),
    ];
    const events = hostile.map((channel, index) => channelWith(channel, `20260923-000000-00000${index}-c0000${index}`, { reason: "mention" }));
    const notice = buildNotice("codex-aaaaaaaa; rm -rf ~", events);
    assert.match(notice, /for participant <\?>\./);
    for (const name of hostile) assert.ok(!notice.includes(name), `leaked ${JSON.stringify(name)}`);
    assert.match(notice, /post channels/);
    assert.equal(noticeName("tax"), "tax");
    assert.equal(noticeName(".."), "<?>");
  });
});

describe("runCommand bounds", () => {
  test("output over the cap is killed and reported oversize, never parsed", async () => {
    const result = await runCommand(process.execPath, ["-e", "process.stdout.write('x'.repeat(300000))"], { capBytes: 100000 });
    assert.equal(result.ok, false);
    assert.equal(result.oversize, true);
    assert.equal(result.stdout, "");
  });
  test("a slow child times out", async () => {
    const result = await runCommand(process.execPath, ["-e", "setTimeout(() => {}, 10000)"], { timeoutMs: 200 });
    assert.equal(result.ok, false);
    assert.equal(result.timedOut, true);
  });
});

// ------------------------------------------------------------------ discovery

describe("discovery", () => {
  test("an exact digest match binds; a near-miss digest does not", async () => {
    const w = makeWorld();
    w.addParticipant("codex-aaaaaaaa", "session-a");
    const near = sha256("session-b").slice(0, 63) + (sha256("session-b").endsWith("0") ? "1" : "0");
    w.addParticipant("codex-bbbbbbbb", "unused", { conversation_key_digest: near });
    w.addPane("wC:p1", "session-a");
    w.addPane("wC:p2", "session-b");
    await w.run();
    assert.equal(w.sup.bindings.get("codex-aaaaaaaa")?.state, "bound");
    assert.equal(w.sup.bindings.has("codex-bbbbbbbb"), false);
    assert.ok(w.logs.some((r) => r.problem === "pane session matches no participant" && r.pane === "wC:p2"));
  });

  test("discovery binds everything; only a stored `enabled: false` stays unarmed (E7)", async () => {
    const w = makeWorld();
    w.addParticipant("codex-aaaaaaaa", "session-a");
    w.addParticipant("codex-bbbbbbbb", "session-b");
    // What `post-doorbell disable` writes.
    updatePrefs(w.paths, "codex-bbbbbbbb", (p) => {
      p.enabled = false;
    });
    w.addPane("wC:p1", "session-a");
    w.addPane("wC:p2", "session-b");
    w.snapshots.set("codex-aaaaaaaa", [mailWith("20260923-000001-aaaaa1")]);
    w.snapshots.set("codex-bbbbbbbb", [mailWith("20260923-000001-bbbbb1")]);
    await w.run();
    assert.equal(w.sub("codex-aaaaaaaa").armed, true, "no prefs file is enabled");
    assert.equal(w.sub("codex-bbbbbbbb").armed, false);
    assert.equal(w.snapshotCalls("codex-bbbbbbbb").length, 0);
    assert.deepEqual(w.prompts.map((p) => p.pane), ["wC:p1"]);
  });

  test("two panes carrying one session are ambiguous and unarmed until select", async () => {
    const w = makeWorld();
    w.addParticipant("codex-aaaaaaaa", "session-a");
    w.addPane("wC:p1", "session-a");
    w.addPane("wC:p2", "session-a");
    w.enable("codex-aaaaaaaa");
    w.snapshots.set("codex-aaaaaaaa", [mailWith("20260923-000001-aaaaa1")]);
    await w.run();
    assert.equal(w.sup.bindings.get("codex-aaaaaaaa").state, "ambiguous");
    assert.equal(w.prompts.length, 0);
    updatePrefs(w.paths, "codex-aaaaaaaa", (p) => {
      p.selection = { pane: "wC:p2", digest: sha256("session-a") };
    });
    await w.run();
    assert.equal(w.sup.bindings.get("codex-aaaaaaaa").state, "bound");
    assert.deepEqual(w.prompts.map((p) => p.pane), ["wC:p2"]);
  });

  test("a selection never moves to a pane carrying another digest", async () => {
    const w = makeWorld();
    w.addParticipant("codex-aaaaaaaa", "session-a");
    w.addPane("wC:p1", "session-a");
    w.addPane("wC:p2", "session-a");
    w.addPane("wC:p3", "session-other");
    w.enable("codex-aaaaaaaa", { selection: { pane: "wC:p3", digest: sha256("session-a") } });
    await w.run();
    assert.equal(w.sup.bindings.get("codex-aaaaaaaa").state, "ambiguous");
    assert.equal(w.prompts.length, 0);
  });

  test("a herdr list failure retires nothing", async () => {
    const w = standardWorld();
    await w.run();
    assert.equal(w.outcomes("accepted").length, 1);
    w.herdrListFail = true;
    await w.run();
    await w.run();
    assert.ok(w.sub("codex-aaaaaaaa"), "subscription survives");
    assert.equal(w.outcomes("retired").length, 0);
    assert.equal(w.logs.filter((r) => r.problem === "herdr agent list failed").length, 1, "logged once");
  });

  test("a participant list larger than the old 1 MiB cap still loads", async () => {
    const w = makeWorld();
    w.addParticipant("codex-aaaaaaaa", "session-a");
    w.addPane("wC:p1", "session-a");
    // The fake exec ignores capBytes, so enforce it the way runCommand does:
    // stdout over the cap is an oversize kill, never parsed.
    const inner = w.exec;
    let listBytes = 0;
    w.sup.exec = async (kind, args, opts = {}) => {
      if (kind === "post" && args[0] === "participant" && args[1] === "list") {
        const body = JSON.stringify({
          ok: true,
          participants: w.participants,
          count: w.participants.length,
          filler: "x".repeat(1_300_000),
        });
        listBytes = Buffer.byteLength(body);
        if (listBytes > (opts.capBytes ?? (1 << 20))) {
          return { ok: false, code: null, signal: "SIGKILL", oversize: true, stdout: "", stderr: "", stdoutBytes: listBytes };
        }
        return ok(body);
      }
      return inner(kind, args, opts);
    };
    await w.run();
    assert.ok(listBytes > (1 << 20), `the fake list is ${listBytes} bytes, over the old cap`);
    assert.equal(w.sup.postOk, true);
    assert.ok(w.sup.participants.has("codex-aaaaaaaa"), "the participant map loaded");
    assert.equal(w.sup.bindings.get("codex-aaaaaaaa")?.state, "bound");
  });

  test("a standing participant-list failure logs once, again after ten minutes, and its recovery", async () => {
    const w = standardWorld();
    const inner = w.exec;
    let failing = true;
    w.sup.exec = async (kind, args, opts = {}) => {
      if (kind === "post" && args[0] === "participant" && args[1] === "list" && failing) {
        return { ok: false, code: null, signal: "SIGKILL", oversize: true, stdout: "", stderr: "", stdoutBytes: 1_246_652 };
      }
      return inner(kind, args, opts);
    };
    const failed = () => w.logs.filter((r) => r.problem === "post participant list failed");
    await w.run();
    assert.equal(w.sup.postOk, false);
    assert.equal(failed().length, 1, "the first failure logs immediately");
    assert.equal(failed()[0].oversize, true);
    assert.equal(failed()[0].stdout_bytes, 1_246_652);
    await w.run();
    w.clock.t += 9 * 60_000;
    await w.run();
    assert.equal(failed().length, 1, "a persisting failure stays quiet inside the window");
    w.clock.t += 61_000;
    await w.run();
    assert.equal(failed().length, 2, "the next log lands once ten minutes pass");
    failing = false;
    await w.run();
    assert.equal(failed().length, 2);
    assert.equal(w.sup.postOk, true);
    assert.equal(w.logs.filter((r) => r.problem === "post participant list recovered").length, 1, "recovery logs once");
    failing = true;
    w.clock.t += 61_000;
    await w.run();
    assert.equal(failed().length, 3, "a new streak is a new first failure");
  });
});

// ------------------------------------------------------------- default-on
// Trey's ruling, 2026-09-23: "definitely turn it on by default". A participant
// with no prefs file — or a file with no `enabled` field — is enabled. Only a
// stored `enabled: false` (`post-doorbell disable`) opts out, and `subscribe`
// and `select` never write one.

describe("the doorbell is default-on", () => {
  test("a participant with no prefs file, bound to a pane, is rung for direct mail", async () => {
    const w = makeWorld();
    w.addParticipant("codex-aaaaaaaa", "session-a");
    w.addPane("wC:p1", "session-a");
    w.snapshots.set("codex-aaaaaaaa", [mailWith("20260923-000001-aaaaa1")]);
    assert.equal(fs.existsSync(prefsPath(w.paths, "codex-aaaaaaaa")), false, "no prefs file");
    await w.run();
    assert.equal(w.sub("codex-aaaaaaaa").armed, true);
    assert.deepEqual(w.prompts.map((p) => p.pane), ["wC:p1"]);
    assert.match(w.prompts[0].text, /Waiting: 1 direct\./);
    assert.equal(fs.existsSync(prefsPath(w.paths, "codex-aaaaaaaa")), false, "ringing writes no prefs");
  });

  test("after disable it is not rung, and a later subscribe does not re-enable it", async () => {
    const w = makeWorld();
    w.addParticipant("codex-aaaaaaaa", "session-a");
    w.addPane("wC:p1", "session-a");
    w.snapshots.set("codex-aaaaaaaa", [mailWith("20260923-000001-aaaaa1")]);
    // What `post-doorbell disable` writes.
    updatePrefs(w.paths, "codex-aaaaaaaa", (p) => {
      p.enabled = false;
    });
    await w.run();
    assert.equal(w.sub("codex-aaaaaaaa").armed, false);
    assert.equal(w.prompts.length, 0);
    // What `post-doorbell subscribe --channel tax` writes.
    updatePrefs(w.paths, "codex-aaaaaaaa", (p) => {
      p.channels = [...new Set([...p.channels, "tax"])].sort();
    });
    w.snapshots.set("codex-aaaaaaaa", [mailWith("20260923-000001-aaaaa1"), channelWith("tax", "20260923-000000-000001-c00001")]);
    w.clock.t += 61_000;
    await w.run();
    assert.equal(loadPrefs(w.paths, "codex-aaaaaaaa").enabled, false);
    assert.equal(w.sub("codex-aaaaaaaa").armed, false);
    assert.equal(w.prompts.length, 0, "nothing rings while disabled");
  });

  test("subscribe on a fresh participant leaves it enabled and writes no enabled: false", async () => {
    const w = makeWorld();
    w.addParticipant("codex-aaaaaaaa", "session-a");
    w.addPane("wC:p1", "session-a");
    w.snapshots.set("codex-aaaaaaaa", [channelWith("tax", "20260923-000000-000001-c00001")]);
    // What `post-doorbell subscribe --channel tax` writes.
    updatePrefs(w.paths, "codex-aaaaaaaa", (p) => {
      p.channels = [...new Set([...p.channels, "tax"])].sort();
    });
    const written = JSON.parse(fs.readFileSync(prefsPath(w.paths, "codex-aaaaaaaa"), "utf8"));
    assert.notEqual(written.enabled, false);
    assert.deepEqual(written.channels, ["tax"]);
    await w.run();
    assert.equal(w.sub("codex-aaaaaaaa").armed, true);
    assert.equal(w.prompts.length, 1, "the subscribed channel rings");
    assert.match(w.prompts[0].text, /1 in #tax/);
  });

  test("enable still sets focused and desktop from its flags", async () => {
    const w = standardWorld();
    w.panes[0].focused = true;
    await w.run();
    assert.equal(w.prompts.length, 0, "a focused pane is not woken by default");
    assert.equal(w.desktop.length, 0);
    // What `post-doorbell enable --focused --desktop` writes.
    updatePrefs(w.paths, "codex-aaaaaaaa", (p) => {
      p.enabled = true;
      p.focused = true;
      p.desktop = true;
    });
    const prefs = loadPrefs(w.paths, "codex-aaaaaaaa");
    assert.deepEqual([prefs.enabled, prefs.focused, prefs.desktop], [true, true, true]);
    await w.run();
    assert.equal(w.prompts.length, 1, "--focused wakes a focused pane");
    assert.equal(w.desktop.length, 1, "--desktop notifies");
  });
});

// ------------------------------------------------------------------ generations

describe("generations", () => {
  test("a terminal change retires the generation and the new one starts empty", async () => {
    const w = standardWorld();
    await w.run();
    assert.equal(w.prompts.length, 1);
    const oldFile = w.sub("codex-aaaaaaaa").stateFile;
    w.panes[0].terminal_id = "term_new";
    await w.run();
    assert.deepEqual(w.outcomes("retired").map((r) => r.reason), ["terminal changed"]);
    assert.equal(w.prompts.length, 2, "the new generation announces what is unread once");
    assert.notEqual(w.sub("codex-aaaaaaaa").stateFile, oldFile);
    assert.ok(JSON.parse(fs.readFileSync(oldFile, "utf8")).retired_at, "old state frozen");
    await w.run();
    assert.equal(w.prompts.length, 2);
  });

  test("a session change in the same pane retires", async () => {
    const w = standardWorld();
    w.addParticipant("codex-bbbbbbbb", "session-b");
    await w.run();
    w.panes[0].session = "session-b";
    await w.run();
    assert.ok(w.outcomes("retired").some((r) => r.participant === "codex-aaaaaaaa" && r.reason === "session changed"));
  });

  test("a resumed session in a new pane starts with empty state", async () => {
    const w = standardWorld();
    await w.run();
    w.panes.splice(0, 1);
    w.addPane("wC:p9", "session-a");
    await w.run();
    assert.deepEqual(w.prompts.map((p) => p.pane), ["wC:p1", "wC:p9"]);
    assert.deepEqual(w.outcomes("retired").map((r) => r.reason), ["pane gone"]);
  });

  for (const [label, reason, vanish, restore] of [
    ["a pane missing from one list", "pane gone", (w) => w.panes.splice(0, 1)[0], (w, pane) => w.panes.push(pane)],
    ["a session briefly unreported", "session changed", (w) => { w.panes[0].session = null; }, (w) => { w.panes[0].session = "session-a"; }],
  ]) {
    test(`the identical generation revives with its state after ${label}`, async () => {
      const w = standardWorld();
      await w.run();
      assert.equal(w.prompts.length, 1);
      const file = w.sub("codex-aaaaaaaa").stateFile;
      const held = vanish(w);
      await w.run();
      assert.deepEqual(w.outcomes("retired").map((r) => r.reason), [reason]);
      assert.equal(w.sub("codex-aaaaaaaa"), undefined);
      restore(w, held);
      await w.run();
      const sub = w.sub("codex-aaaaaaaa");
      assert.ok(sub?.armed, "the same generation is armed again");
      assert.equal(sub.stateFile, file, "same generation, same state file");
      assert.equal(w.state("codex-aaaaaaaa").retired_at, undefined, "state unfrozen");
      assert.equal(w.prompts.length, 1, "mail it already announced does not ring again");
      assert.ok(w.logs.some((r) => r.type === "generation" && r.revived === true));
      w.snapshots.set("codex-aaaaaaaa", [mailWith("20260923-000001-aaaaa1"), mailWith("20260923-000001-aaaaa2")]);
      w.clock.t += 61_000;
      await w.run();
      assert.equal(w.prompts.length, 2, "new mail rings in the revived generation");
    });
  }

  test("one participant's state never suppresses another's", async () => {
    const w = standardWorld();
    w.addParticipant("codex-bbbbbbbb", "session-b");
    w.addPane("wC:p2", "session-b");
    w.enable("codex-bbbbbbbb");
    const shared = mailWith("20260923-000001-aaaaa1");
    w.snapshots.set("codex-bbbbbbbb", [shared]);
    await w.run();
    assert.deepEqual(w.prompts.map((p) => p.pane).sort(), ["wC:p1", "wC:p2"]);
    const a = w.sub("codex-aaaaaaaa");
    const b = w.sub("codex-bbbbbbbb");
    assert.notEqual(path.dirname(a.stateFile), path.dirname(b.stateFile));
    assert.match(b.stateFile, /state\/codex-bbbbbbbb\/herdr-[0-9a-f]{16}\.json$/);
  });
});

// ------------------------------------------------------------------ sinks and outcomes

describe("sinks and outcomes (E1)", () => {
  test("notified never suppresses a later accepted prompt", async () => {
    const w = standardWorld();
    w.enable("codex-aaaaaaaa", { desktop: true });
    w.panes[0].status = "working";
    w.getOverride.set("wC:p1", () =>
      ok(JSON.stringify({ result: { agent: { pane_id: "wC:p1", terminal_id: w.panes[0].terminal_id, agent_status: w.panes[0].status, focused: false, agent_session: { kind: "id", value: "session-a" } } } }))
    );
    // Scannable at discovery, busy at the recheck: desktop notifies, herdr defers.
    const sub = () => w.sub("codex-aaaaaaaa");
    await w.sup.tick();
    sub().scannable = true;
    w.sup.markDirty(sub(), "hint");
    w.sup.pump();
    await w.sup.idle();
    assert.equal(w.desktop.length, 1);
    assert.equal(w.prompts.length, 0);
    assert.equal(w.state("codex-aaaaaaaa").notified.length, 1);
    assert.equal(w.state("codex-aaaaaaaa").announced.length, 0);
    w.panes[0].status = "idle";
    await w.run();
    assert.equal(w.prompts.length, 1, "the notified key still rings the agent");
    assert.equal(w.desktop.length, 1, "and does not re-notify the desktop");
  });

  test("another sink's state never suppresses this sink", async () => {
    const w = standardWorld();
    await w.sup.tick();
    const sub = w.sub("codex-aaaaaaaa");
    const other = path.join(path.dirname(sub.stateFile), `cmux-${sub.genHash}.json`);
    fs.mkdirSync(path.dirname(other), { recursive: true });
    fs.writeFileSync(other, JSON.stringify({ announced: [eventKey(mailWith("20260923-000001-aaaaa1"))], notified: [] }));
    await w.sup.idle();
    assert.equal(w.prompts.length, 1);
  });

  test("accepted writes exactly the current eligible keys; a quiet scan prunes consumed keys", async () => {
    const w = standardWorld();
    w.snapshots.set("codex-aaaaaaaa", [mailWith("20260923-000001-aaaaa1"), mailWith("20260923-000001-aaaaa2")]);
    await w.run();
    assert.equal(w.state("codex-aaaaaaaa").announced.length, 2);
    w.snapshots.set("codex-aaaaaaaa", [mailWith("20260923-000001-aaaaa2")]);
    w.clock.t += 61_000;
    await w.run();
    assert.equal(w.prompts.length, 1, "nothing fresh, no ring");
    assert.deepEqual(w.state("codex-aaaaaaaa").announced, [eventKey(mailWith("20260923-000001-aaaaa2"))]);
  });

  test("a pending item that becomes routed rings again", async () => {
    const w = standardWorld();
    const pending = mailWith("20260923-000001-aaaaa1", { pending: true });
    w.snapshots.set("codex-aaaaaaaa", [pending]);
    await w.run();
    assert.match(w.prompts[0].text, /1 waiting to be routed/);
    w.snapshots.set("codex-aaaaaaaa", [mailWith("20260923-000001-aaaaa1")]);
    w.clock.t += 61_000;
    await w.run();
    assert.equal(w.prompts.length, 2, "routed is a new state class");
    assert.match(w.prompts[1].text, /Waiting: 1 direct\./);
  });

  test("a degraded item never suppresses its later healthy form", async () => {
    const w = standardWorld();
    w.snapshots.set("codex-aaaaaaaa", [mailWith("20260923-000001-aaaaa1", { cursor_unusable: true })]);
    await w.run();
    w.snapshots.set("codex-aaaaaaaa", [mailWith("20260923-000001-aaaaa1")]);
    w.clock.t += 61_000;
    await w.run();
    assert.equal(w.prompts.length, 2);
  });
});

// ------------------------------------------------------------------ recheck

describe("recheck before the prompt (E3)", () => {
  test("a pane busy at the recheck is deferred; it rings once idle again", async () => {
    const w = standardWorld();
    let status = "working";
    w.getOverride.set("wC:p1", () =>
      ok(JSON.stringify({ result: { agent: { pane_id: "wC:p1", terminal_id: w.panes[0].terminal_id, agent_status: status, focused: false, agent_session: { kind: "id", value: "session-a" } } } }))
    );
    await w.run();
    assert.equal(w.prompts.length, 0);
    assert.equal(w.sub("codex-aaaaaaaa").lastOutcome, "deferred");
    assert.equal(fs.existsSync(w.sub("codex-aaaaaaaa").stateFile), false, "deferred writes no state");
    status = "idle";
    await w.run();
    assert.equal(w.prompts.length, 1);
  });

  test("a focused pane is deferred unless --focused", async () => {
    const w = standardWorld();
    w.panes[0].focused = true;
    await w.run();
    assert.equal(w.prompts.length, 0);
    w.enable("codex-aaaaaaaa", { focused: true });
    await w.run();
    assert.equal(w.prompts.length, 1);
  });

  test("a changed session at the recheck retires without prompting", async () => {
    const w = standardWorld();
    w.getOverride.set("wC:p1", () =>
      ok(JSON.stringify({ result: { agent: { pane_id: "wC:p1", terminal_id: w.panes[0].terminal_id, agent_status: "idle", focused: false, agent_session: { kind: "id", value: "someone-else" } } } }))
    );
    await w.run();
    assert.equal(w.prompts.length, 0);
    assert.deepEqual(w.outcomes("retired").map((r) => r.reason), ["session changed"]);
  });

  test("a lookup error fails, retries, and never retires this or another subscription", async () => {
    const w = standardWorld();
    w.addParticipant("codex-bbbbbbbb", "session-b");
    w.addPane("wC:p2", "session-b");
    w.enable("codex-bbbbbbbb");
    w.snapshots.set("codex-bbbbbbbb", [mailWith("20260923-000001-bbbbb1")]);
    w.getOverride.set("wC:p1", { ok: false, code: 2, stdout: "", stderr: "herdr: socket busy" });
    await w.run();
    assert.deepEqual(w.prompts.map((p) => p.pane), ["wC:p2"]);
    assert.equal(w.outcomes("retired").length, 0);
    assert.equal(w.sub("codex-aaaaaaaa").consecutiveFailures, 1);
    assert.equal(w.sub("codex-aaaaaaaa").lastError.stage, "herdr_get");
    w.getOverride.delete("wC:p1");
    w.clock.t += 5_000;
    await w.run();
    assert.deepEqual(w.prompts.map((p) => p.pane), ["wC:p2", "wC:p1"]);
  });

  test("a blocked prompt is deferred; a failed prompt never advances state", async () => {
    const w = standardWorld();
    w.promptResult = () => ({ ok: false, code: 1, stdout: "", stderr: JSON.stringify({ error: { code: "agent_blocked" } }) });
    await w.run();
    assert.equal(w.sub("codex-aaaaaaaa").lastOutcome, "deferred");
    w.promptResult = () => ({ ok: false, code: 1, stdout: "", stderr: "herdr: write failed" });
    await w.run();
    assert.equal(w.outcomes("failed").at(-1).stage, "herdr_prompt");
    assert.equal(fs.existsSync(w.sub("codex-aaaaaaaa").stateFile), false);
  });
});

// ------------------------------------------------------------------ scans that fail

describe("failed scans (E4)", () => {
  const failures = [
    ["a nonzero exit", { result: { ok: false, code: 78, stdout: "", stderr: "post: store locked" } }, "snapshot"],
    ["a timeout", { result: { ok: false, code: null, signal: "SIGKILL", timedOut: true, stdout: "", stderr: "" } }, "snapshot_timeout"],
    ["oversize output", { result: { ok: false, code: null, signal: "SIGKILL", oversize: true, stdout: "", stderr: "" } }, "snapshot_oversize"],
    ["a truncated line", () => ok(jsonl([mailWith("20260923-000001-aaaaa1")]).slice(0, -30)), "snapshot_malformed"],
    ["an unknown event", () => ok(jsonl([mailWith("20260923-000001-aaaaa1", { event: "mail_v9" })])), "snapshot_malformed"],
    ["an unbound participant", () => ({ ok: true, code: 0, stdout: "", stderr: "participant: unbound (run: post participant bind)\n" }), "snapshot_unbound"],
  ];
  for (const [label, script, stage] of failures) {
    test(`${label} is failed, never accepted, and never advances state`, async () => {
      const w = standardWorld();
      w.snapshots.set("codex-aaaaaaaa", [mailWith("20260923-000001-aaaaa0")]);
      await w.run();
      const before = fs.readFileSync(w.sub("codex-aaaaaaaa").stateFile, "utf8");
      w.snapshots.set("codex-aaaaaaaa", typeof script === "function" ? script : script);
      w.clock.t += 61_000;
      await w.run();
      assert.equal(w.prompts.length, 1, "no second ring");
      assert.equal(w.outcomes("failed").at(-1).stage, stage);
      assert.equal(fs.readFileSync(w.sub("codex-aaaaaaaa").stateFile, "utf8"), before, "state untouched");
      assert.ok(w.sup.isDirty(w.sub("codex-aaaaaaaa")), "still dirty for the retry");
    });
  }

  test("failures back off to the cap and mark broken, but never retire", async () => {
    const w = standardWorld();
    w.snapshots.set("codex-aaaaaaaa", { result: { ok: false, code: 1, stdout: "", stderr: "boom" } });
    const delays = [];
    for (let attempt = 0; attempt < 8; attempt++) {
      await w.run();
      const sub = w.sub("codex-aaaaaaaa");
      delays.push(sub.nextAttemptAt - w.clock.t);
      w.clock.t = sub.nextAttemptAt;
    }
    assert.deepEqual(delays, [5000, 10000, 20000, 40000, 80000, 160000, 300000, 300000]);
    assert.equal(w.logs.filter((r) => r.type === "broken").length, 1);
    w.sup.writeHealth(true);
    const health = JSON.parse(fs.readFileSync(w.paths.healthFile, "utf8"));
    assert.equal(health.bindings[0].broken, true);
    assert.equal(health.bindings[0].last_error.stage, "snapshot");
    assert.equal(w.outcomes("retired").length, 0);
  });
});

// ------------------------------------------------------------------ lifetime

describe("lifetime", () => {
  test("an expired lease with a live idle pane still rings", async () => {
    const w = makeWorld();
    w.addParticipant("codex-aaaaaaaa", "session-a", { last_seen: "2026-01-01T00:00:00Z", lease_hours: 1 });
    w.addPane("wC:p1", "session-a");
    w.enable("codex-aaaaaaaa");
    w.snapshots.set("codex-aaaaaaaa", [mailWith("20260923-000001-aaaaa1")]);
    await w.run();
    assert.equal(w.outcomes("accepted").length, 1);
  });

  test("an ended participant retires at the recheck, loudly once, before any prompt", async () => {
    const w = standardWorld();
    w.showPatch.set("codex-aaaaaaaa", { ended_at: "2026-09-23T04:30:51Z" });
    await w.run();
    assert.equal(w.prompts.length, 0);
    assert.deepEqual(w.outcomes("retired").map((r) => r.reason), ["participant ended"]);
    w.participants[0].ended_at = "2026-09-23T04:30:51Z";
    await w.run();
    await w.run();
    assert.equal(w.outcomes("retired").length, 1);
    assert.equal(w.sup.bindings.get("codex-aaaaaaaa").state, "ended");
  });

  test("a participant revived by a rebind is bound again", async () => {
    const w = standardWorld();
    w.showPatch.set("codex-aaaaaaaa", { ended_at: "2026-09-23T04:30:51Z" });
    w.participants[0].ended_at = "2026-09-23T04:30:51Z";
    await w.run();
    w.showPatch.delete("codex-aaaaaaaa");
    delete w.participants[0].ended_at;
    w.clock.t += 61_000;
    await w.run();
    assert.equal(w.outcomes("accepted").length, 1);
  });
});

// ------------------------------------------------------------------ scheduling

describe("scheduling (E6)", () => {
  function gate() {
    let release;
    const promise = new Promise((resolve) => {
      release = resolve;
    });
    return { promise, release };
  }

  test("a change during a scan is not lost", async () => {
    const w = standardWorld();
    const first = gate();
    let calls = 0;
    w.snapshots.set("codex-aaaaaaaa", async () => {
      calls += 1;
      if (calls === 1) await first.promise;
      return ok(jsonl(calls === 1 ? [] : [mailWith("20260923-000001-aaaaa9")]));
    });
    await w.sup.tick();
    assert.equal(calls, 1);
    // The mail lands while the first scan is running.
    w.sup.markDirty(w.sub("codex-aaaaaaaa"), "hint");
    first.release();
    await w.sup.idle();
    assert.equal(calls, 2, "rescanned");
    assert.equal(w.prompts.length, 1);
  });

  test("one host-wide channels call serves every participant's channel watches", async () => {
    const w = makeWorld();
    for (const letter of "abcdef") {
      const id = `codex-${letter.repeat(8)}`;
      w.addParticipant(id, `session-${letter}`);
      w.addPane(`wC:p${letter}`, `session-${letter}`);
      w.enable(id);
    }
    w.channelRows = [{ name: "ops", participants: ["codex-aaaaaaaa", "codex-bbbbbbbb"] }];
    await w.run();
    const channelCalls = w.calls.filter((c) => c.kind === "post" && c.args[0] === "channels");
    assert.equal(channelCalls.length, 1, "one call, not one per participant");
    assert.equal(channelCalls[0].participant, undefined, "host-wide, not as any participant");
    const ops = w.watches.find((h) => h.target === path.join(w.paths.root, "channels", "ops") && !h.closed);
    assert.ok(ops, "the joined channel is watched");
    await w.run();
    assert.equal(w.calls.filter((c) => c.kind === "post" && c.args[0] === "channels").length, 1, "not re-run every tick");
  });

  test("an archived channel the participant joined keeps its targeted watch", async () => {
    const w = standardWorld();
    w.channelRows = [{ name: "planning", archived: true, participants: ["codex-aaaaaaaa"] }];
    await w.run();
    const planning = w.watches.find((h) => h.target === path.join(w.paths.root, "channels", "planning") && !h.closed);
    assert.ok(planning, "a post would resurrect it, so it stays watched");
  });

  test("a failing channels call is retried once per 10 s, not every tick", async () => {
    const w = standardWorld();
    let calls = 0;
    const exec = w.exec;
    w.sup.exec = async (kind, args, opts) => {
      if (kind === "post" && args[0] === "channels") {
        calls += 1;
        return { ok: false, code: 1, stdout: "", stderr: "post: store busy" };
      }
      return exec(kind, args, opts);
    };
    await w.run();
    w.clock.t += 2_000;
    await w.run();
    w.clock.t += 2_000;
    await w.run();
    assert.equal(calls, 1, "throttled on the attempt");
    w.clock.t += 10_000;
    await w.run();
    assert.equal(calls, 2);
  });

  test("startup scans each armed subscription once; the timer reconcile follows a minute later", async () => {
    const w = standardWorld();
    const first = gate();
    let calls = 0;
    w.snapshots.set("codex-aaaaaaaa", async () => {
      calls += 1;
      await first.promise;
      return ok(jsonl([]));
    });
    await w.sup.tick();
    first.release();
    await w.sup.idle();
    assert.equal(calls, 1, "one startup scan, not a second for an immediate reconcile");
    w.clock.t += 59_000;
    await w.run();
    assert.equal(calls, 1);
    w.clock.t += 1_000;
    await w.run();
    assert.equal(calls, 2, "the reconcile clock started at the first tick");
  });

  test("hints: participant-dir writes mark dirty; heartbeat writes do not", async () => {
    const w = standardWorld();
    await w.run();
    const scans = w.snapshotCalls().length;
    const participantWatch = w.watches.find((h) => h.target.endsWith(path.join("participants", "codex-aaaaaaaa")) && !h.closed);
    assert.ok(participantWatch?.recursive);
    participantWatch.onEvent("change", "watch.heartbeat");
    await new Promise((resolve) => setTimeout(resolve, 20));
    await w.sup.idle();
    assert.equal(w.snapshotCalls().length, scans, "heartbeat is not mail");
    // A live watch refreshes the participant record every interval.
    participantWatch.onEvent("change", "participant.json");
    participantWatch.onEvent("rename", ".participant.json.4242.7.tmp");
    await new Promise((resolve) => setTimeout(resolve, 20));
    await w.sup.idle();
    assert.equal(w.snapshotCalls().length, scans, "the participant record's activity refresh is not mail");
    participantWatch.onEvent("rename", "routing/20260923-000001-aaaaa2.json");
    await new Promise((resolve) => setTimeout(resolve, 20));
    await w.sup.idle();
    assert.equal(w.snapshotCalls().length, scans + 1);
    const workspaceWatch = w.watches.find((h) => h.target.endsWith(path.join("alpha", "inbox")));
    assert.ok(workspaceWatch, "workspace inbox watched");
  });

  test("a prefs change is seen even when the rewrite keeps the old mtime", async () => {
    // Linux stamps file times from a coarse clock: two prefs writes a few ms
    // apart share one mtime (the 2026-09-23 devbox gate). Pin it exactly.
    const w = standardWorld();
    await w.run();
    const file = prefsPath(w.paths, "codex-aaaaaaaa");
    const pinned = 1_790_000_000; // whole seconds: exact on every filesystem
    fs.utimesSync(file, pinned, pinned);
    assert.equal(w.sup.prefsFor("codex-aaaaaaaa").focused, false);
    updatePrefs(w.paths, "codex-aaaaaaaa", (p) => {
      p.focused = true;
    });
    fs.utimesSync(file, pinned, pinned);
    assert.equal(fs.statSync(file).mtimeMs, pinned * 1000, "the rewrite kept the old mtime");
    assert.equal(w.sup.prefsFor("codex-aaaaaaaa").focused, true);
  });

  test("a watcher error triggers a full reconciliation of every armed subscription", async () => {
    const w = standardWorld();
    w.addParticipant("codex-bbbbbbbb", "session-b");
    w.addPane("wC:p2", "session-b");
    w.enable("codex-bbbbbbbb");
    await w.run();
    const scans = w.snapshotCalls().length;
    const anyWatch = w.watches.find((h) => h.target.includes("codex-aaaaaaaa"));
    anyWatch.onError(Object.assign(new Error("overflow"), { code: "EOVERFLOW" }));
    await w.sup.idle();
    assert.equal(w.snapshotCalls().length, scans + 2);
    assert.ok(w.logs.some((r) => r.type === "watch" && r.problem.includes("full reconciliation")));
  });

  test("a watch that fails as it is created backs off instead of re-arming recursively", async () => {
    const w = standardWorld();
    const attempts = [];
    w.sup.watchImpl = (target, opts, onEvent, onError) => {
      if (target.includes("codex-aaaaaaaa")) {
        attempts.push(target);
        onError(Object.assign(new Error("too many open files"), { code: "EMFILE" }));
        return null;
      }
      return w.watch(target, opts, onEvent, onError);
    };
    await w.run();
    assert.equal(w.prompts.length, 1, "the mail still rings");
    const first = attempts.length;
    const failing = new Set(attempts).size;
    assert.equal(first, failing, "one attempt per failing watch, no recursion");
    await w.run();
    assert.equal(attempts.length, first, "no retry inside the backoff");
    w.clock.t += 1_000;
    await w.run();
    assert.equal(attempts.length, first + failing, "retried after the backoff");
    for (let i = 0; i < 12; i += 1) {
      w.clock.t += 60_000;
      await w.run();
    }
    const lines = w.logs.filter((r) => r.type === "watch" && r.problem.includes("full reconciliation"));
    assert.ok(lines.length <= failing * 5, `log bounded by doubling, got ${lines.length}`);
    const health = w.sup.healthSnapshot();
    assert.ok(health.watch_failures.length >= 1 && health.watch_failures.every((f) => f.error === "EMFILE"));
    assert.equal(w.sup.watchFailures.get([...w.sup.watchFailures.keys()][0]).count > 5, true, "the streak keeps counting");
  });

  test("reconciliation scans every armed subscription with no hint, and reports gaps", async () => {
    const w = standardWorld();
    await w.run();
    w.snapshots.set("codex-aaaaaaaa", [mailWith("20260923-000001-aaaaa1"), mailWith("20260923-000001-aaaaa3")]);
    w.clock.t += 60_000;
    await w.run();
    assert.equal(w.prompts.length, 2, "found without a hint");
    w.clock.t += 60_000;
    await w.run();
    assert.ok(w.logs.some((r) => r.type === "reconcile" && r.gaps.length === 1));
  });

  test("concurrency never exceeds the limit, and a slow snapshot does not block the rest", async () => {
    const w = makeWorld();
    const stuck = gate();
    let running = 0;
    let peak = 0;
    for (const letter of "abcdef") {
      const id = `codex-${letter.repeat(8)}`;
      w.addParticipant(id, `session-${letter}`);
      w.addPane(`wC:p${letter}`, `session-${letter}`);
      w.enable(id);
      w.snapshots.set(id, async () => {
        running += 1;
        peak = Math.max(peak, running);
        if (letter === "a") await stuck.promise;
        else await new Promise((resolve) => setTimeout(resolve, 5));
        running -= 1;
        return ok(jsonl([mailWith(`20260923-000001-${letter}0000${letter}`.slice(0, 22))]));
      });
    }
    await w.sup.tick();
    const deadline = Date.now() + 3000;
    while (w.prompts.length < 5 && Date.now() < deadline) await new Promise((resolve) => setTimeout(resolve, 5));
    assert.equal(w.prompts.length, 5, "every other subscription rang while one snapshot hung");
    assert.equal(peak, 2);
    stuck.release();
    await w.sup.idle();
    assert.equal(w.prompts.length, 6);
    assert.equal(peak, 2);
  });
});

// ------------------------------------------------------------------ selection and prefs

describe("channel subscriptions (E5)", () => {
  test("subscribe rings once for old unread; unsubscribe drops it; resubscribe rings again", async () => {
    const w = standardWorld();
    const old = channelWith("tax", "20260923-000000-000001-c00001");
    w.snapshots.set("codex-aaaaaaaa", [old]);
    await w.run();
    assert.equal(w.prompts.length, 0, "unsubscribed channel traffic does not ring");
    w.enable("codex-aaaaaaaa", { channels: ["tax"] });
    await w.run();
    assert.equal(w.prompts.length, 1);
    assert.match(w.prompts[0].text, /1 in #tax/);
    const version = loadPrefs(w.paths, "codex-aaaaaaaa").version;
    assert.equal(w.outcomes("accepted").at(-1).prefs_version, version);
    w.enable("codex-aaaaaaaa", { channels: [] });
    await w.run();
    assert.equal(w.prompts.length, 1);
    assert.deepEqual(w.state("codex-aaaaaaaa").announced, []);
    w.enable("codex-aaaaaaaa", { channels: ["tax"] });
    await w.run();
    assert.equal(w.prompts.length, 2);
  });

  test("an unreadable channel under a mention-only subscription is a blind spot", async () => {
    const w = standardWorld();
    w.snapshots.set("codex-aaaaaaaa", [clone(samples().unreadableChannel)]);
    await w.run();
    assert.match(w.prompts[0].text, /1 unreadable item\(s\) in #broken; mentions there are unknown/);
    w.sup.writeHealth(true);
    const health = JSON.parse(fs.readFileSync(w.paths.healthFile, "utf8"));
    assert.deepEqual(health.bindings[0].blind_spots, [{ where: "#broken", count: 1 }]);
    const scans = w.snapshotCalls("codex-aaaaaaaa");
    assert.ok(scans.length > 0);
    for (const call of scans) assert.ok(!call.args.includes("--reason"), `scan filtered by reason: ${call.args.join(" ")}`);
  });
});

describe("health", () => {
  test("health records versions, generations, outcomes, and never mail content", async () => {
    const w = standardWorld();
    await w.sup.probeVersions();
    await w.run();
    w.sup.writeHealth(true);
    const text = fs.readFileSync(w.paths.healthFile, "utf8");
    const health = JSON.parse(text);
    assert.equal(health.herdr_version, "herdr 0.9.1");
    assert.equal(health.bindings[0].last_outcome, "accepted");
    assert.equal(health.bindings[0].generation.hash, generationHash({ pane: "wC:p1", terminal: w.panes[0].terminal_id, digest: sha256("session-a") }));
    assert.ok(!text.includes(samples().workspaceMail.subject));
    assert.equal(health.stats.snapshots, health.stats.hintScans + health.stats.reconcileScans);
    assert.ok(health.stats.snapshots >= 1 && health.stats.discoveries >= 1);
  });
});
