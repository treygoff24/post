#!/usr/bin/env node
// post-doorbell: one long-running doorbell supervisor per host, plus the
// agent-facing commands that arm it. Design: docs/plans/doorbell-supervisor-design.md
// (approved; its E1-E8 items are cited inline where they bind the code).
//
//   post-doorbell run                        the supervisor (launchd / systemd user service)
//   post-doorbell enable [--focused] [--desktop]
//   post-doorbell disable
//   post-doorbell subscribe --channel <name> [--unsubscribe]
//   post-doorbell unsubscribe --channel <name>
//   post-doorbell select --pane <pane_id>
//   post-doorbell status [--json]
//
// What it does. Every 2 seconds it lists herdr panes and matches sha256 of each
// pane's agent_session value against the exact conversation_key_digest in
// `post participant list --json`. A matched pane is a binding; its target
// generation is (pane id, terminal id, session digest). A subscription is
// (participant, sink, generation) and owns all dedupe state (E1). Only armed
// subscriptions (the participant ran `post-doorbell enable`) scan and ring (E7).
//
// It never writes a participant's mail state. It reads through
// `POST_PARTICIPANT=<id> post watch --snapshot --json --limit 0 --reason ...`,
// which returns before post's heartbeat, lease, and routing code; it renews no
// lease and consumes nothing. File-system events are hints that mark
// subscriptions dirty; every armed subscription is rescanned every 60 seconds
// regardless (E6). A snapshot is accepted whole or not at all (E4), selection
// happens after parsing (E5), and a ring rechecks the participant and pane
// first (E3). herdr has no conditional prompt, so the window between the
// recheck and the prompt is narrowed, not closed; the notice tells the
// recipient to compare the named participant with its own binding.
//
// Singleton: a python3 child takes fcntl.flock(LOCK_EX|LOCK_NB) on
// $POST_MAIL_ROOT/doorbell/supervisor.lock, prints LOCKED, and holds the lock
// until its stdin closes. The kernel releases the lock when that child dies,
// so PID reuse cannot matter; if the child dies while the supervisor lives,
// the supervisor stops all delivery and exits. Node has no fs.flock.
//
// The herdr lookup, stderr excerpt, and atomic-write helpers are absorbed
// from codex-notify-monitor.mjs, which the per-agent timers still run.
//
// Environment (all optional): POST_MAIL_ROOT; POST_DOORBELL_HOME (replaces
// "~"); POST_DOORBELL_POST_BIN, POST_DOORBELL_HERDR_BIN,
// POST_DOORBELL_CMUX_BIN, POST_DOORBELL_PYTHON_BIN.

import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { createHash, randomBytes } from "node:crypto";
import { spawn, spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

export const SUPERVISOR_VERSION = "1.0.0";
export const NOTICE_TAG = "[post-doorbell:v2]";
export const SINK_HERDR = "herdr";

export const DEFAULTS = Object.freeze({
  discoveryMs: 2000,
  participantRefreshMs: 60_000,
  reconcileMs: 60_000,
  hintCoalesceMs: 250,
  concurrency: 2,
  snapshotTimeoutMs: 20_000,
  snapshotCapBytes: 32 * 1024 * 1024,
  commandTimeoutMs: 10_000,
  backoffBaseMs: 5_000,
  backoffCapMs: 300_000,
  brokenAfter: 5,
  healthMaxAgeMs: 30_000,
  staleHeartbeatMs: 10_000,
  retiredPruneMs: 7 * 24 * 3600 * 1000,
  membershipRefreshMs: 10 * 60 * 1000,
  lockAcquireRetryMs: 1_500,
});

// Tunables an operator may override in $POST_MAIL_ROOT/doorbell/config.json.
const TUNABLE = new Set(["concurrency", "snapshotTimeoutMs", "reconcileMs"]);

const HERDR_STATUSES = new Set(["idle", "done", "working", "blocked", "unknown"]);
const EVENT_KINDS = new Set(["mail", "channel_message", "unreadable"]);
const ADDRESS_KINDS = new Set(["workspace", "participant", "lineage"]);
// The notice alphabet is deliberately narrower than post's naming grammar
// (which admits any path-safe component, spaces included): a name in a
// model-facing notice must not be able to carry a sentence.
const NOTICE_NAME = /^[A-Za-z0-9._-]{1,64}$/;
// Channel names an agent may subscribe to: the name alphabet every doorbell
// installer has used.
const CHANNEL_NAME = /^[A-Za-z0-9._-]{1,255}$/;
const PANE_ID = /^[A-Za-z0-9:_.-]{1,64}$/;
const STATE_ID = /^[A-Za-z0-9._-]{1,128}$/;
const STDERR_EXCERPT_CAP = 300;
const NOTICE_CHANNEL_CAP = 10;

// --------------------------------------------------------------- small helpers

export function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

function writeAllSync(fd, data) {
  const buf = Buffer.isBuffer(data) ? data : Buffer.from(data);
  let offset = 0;
  while (offset < buf.length) {
    const n = fs.writeSync(fd, buf, offset, buf.length - offset);
    if (n <= 0) throw new Error("short write");
    offset += n;
  }
}

// Atomic replace: exclusive unique temp in the destination dir, then rename.
export function writeFileAtomic(file, content, mode = 0o600) {
  fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
  const tmp = path.join(
    path.dirname(file),
    `.${path.basename(file)}.${process.pid}.${randomBytes(8).toString("hex")}.tmp`
  );
  let fd;
  try {
    fd = fs.openSync(
      tmp,
      fs.constants.O_WRONLY | fs.constants.O_CREAT | fs.constants.O_EXCL | (fs.constants.O_NOFOLLOW || 0),
      mode
    );
    writeAllSync(fd, content);
    fs.closeSync(fd);
    fd = undefined;
    fs.renameSync(tmp, file);
  } catch (error) {
    if (fd !== undefined) {
      try {
        fs.closeSync(fd);
      } catch {
        // Best-effort close before unlink.
      }
    }
    try {
      fs.unlinkSync(tmp);
    } catch {
      // Only this process's temp.
    }
    throw error;
  }
}

function readJson(file) {
  try {
    return JSON.parse(fs.readFileSync(file, "utf8"));
  } catch {
    return undefined;
  }
}

// Another program's stderr headed for a log line: bound it and strip control
// characters so it cannot forge a line. Stdout, where subjects and previews
// live, is never excerpted.
export function stderrExcerpt(raw) {
  const oneLine = String(raw ?? "").replace(/[\u0000-\u001f\u007f-\u009f\u2028\u2029]+/g, " ").trim();
  let bytes = 0;
  let excerpt = "";
  for (const character of oneLine) {
    const size = Buffer.byteLength(character, "utf8");
    if (bytes + size > STDERR_EXCERPT_CAP) break;
    bytes += size;
    excerpt += character;
  }
  return excerpt;
}

function executable(override, preferred, fallback) {
  if (override) return override;
  try {
    fs.accessSync(preferred, fs.constants.X_OK);
    return preferred;
  } catch {
    return fallback;
  }
}

export function noticeName(value) {
  return typeof value === "string" && NOTICE_NAME.test(value) && value !== "." && value !== ".."
    ? value
    : "<?>";
}

export function validChannelName(value) {
  return typeof value === "string" && CHANNEL_NAME.test(value) && value !== "." && value !== "..";
}

function validStateId(value) {
  return typeof value === "string" && STATE_ID.test(value) && value !== "." && value !== "..";
}

function herdrErrorCode(stderr) {
  try {
    const code = JSON.parse(String(stderr ?? ""))?.error?.code;
    return typeof code === "string" ? code : undefined;
  } catch {
    return undefined;
  }
}

// ----------------------------------------------------------------------- paths

export function resolvePaths(env = process.env) {
  const home = env.POST_DOORBELL_HOME || os.homedir();
  const root = env.POST_MAIL_ROOT || path.join(home, ".claude-mail");
  const doorbell = path.join(root, "doorbell");
  return {
    home,
    root,
    doorbell,
    prefsDir: path.join(doorbell, "prefs"),
    stateDir: path.join(doorbell, "state"),
    lockFile: path.join(doorbell, "supervisor.lock"),
    heartbeatFile: path.join(doorbell, "heartbeat.json"),
    healthFile: path.join(doorbell, "health.json"),
    configFile: path.join(doorbell, "config.json"),
    postBin: executable(env.POST_DOORBELL_POST_BIN, path.join(home, ".local", "bin", "post"), "post"),
    herdrBin: executable(env.POST_DOORBELL_HERDR_BIN, path.join(home, ".local", "bin", "herdr"), "herdr"),
    cmuxBin: executable(env.POST_DOORBELL_CMUX_BIN, "/Applications/cmux.app/Contents/Resources/bin/cmux", "cmux"),
    pythonBin: env.POST_DOORBELL_PYTHON_BIN || "python3",
  };
}

// ------------------------------------------------------------------- execution

// Run one child with a timeout and a stdout byte cap. Over the cap the child
// is killed and the output is reported as oversize, never parsed partially.
export function runCommand(bin, args, { env, cwd, timeoutMs = DEFAULTS.commandTimeoutMs, capBytes = 1 << 20, children } = {}) {
  return new Promise((resolve) => {
    const started = Date.now();
    let child;
    try {
      child = spawn(bin, args, { env, cwd, stdio: ["ignore", "pipe", "pipe"] });
    } catch (error) {
      resolve({ ok: false, spawnError: String(error.code ?? error.message), stdout: "", stderr: "", durationMs: 0 });
      return;
    }
    children?.add(child);
    const out = [];
    let outBytes = 0;
    const err = [];
    let errBytes = 0;
    let oversize = false;
    let timedOut = false;
    let spawnError;
    const timer = setTimeout(() => {
      timedOut = true;
      child.kill("SIGKILL");
    }, timeoutMs);
    child.stdout.on("data", (chunk) => {
      if (oversize) return;
      outBytes += chunk.length;
      if (outBytes > capBytes) {
        oversize = true;
        child.kill("SIGKILL");
        return;
      }
      out.push(chunk);
    });
    child.stderr.on("data", (chunk) => {
      if (errBytes < 65536) {
        err.push(chunk);
        errBytes += chunk.length;
      }
    });
    child.on("error", (error) => {
      spawnError = String(error.code ?? error.message);
    });
    child.on("close", (code, signal) => {
      clearTimeout(timer);
      children?.delete(child);
      const result = {
        code,
        signal,
        timedOut,
        oversize,
        spawnError,
        stdout: oversize ? "" : Buffer.concat(out).toString("utf8"),
        stderr: Buffer.concat(err).toString("utf8"),
        durationMs: Date.now() - started,
      };
      result.ok = !spawnError && !timedOut && !oversize && code === 0;
      resolve(result);
    });
  });
}

function failureDetail(result) {
  const detail = {};
  if (result.spawnError) detail.spawn_error = result.spawnError;
  if (typeof result.code === "number") detail.exit_code = result.code;
  if (result.signal && !result.timedOut && !result.oversize) detail.signal = result.signal;
  if (result.timedOut) detail.timed_out = true;
  if (result.oversize) detail.oversize = true;
  const excerpt = stderrExcerpt(result.stderr);
  if (excerpt) detail.stderr = excerpt;
  return detail;
}

// ------------------------------------------------------- snapshot parsing (E4)

function validEvent(event) {
  if (event === null || typeof event !== "object" || Array.isArray(event)) return "not an object";
  if (!EVENT_KINDS.has(event.event)) return "unknown event";
  const address = event.address;
  if (address === null || typeof address !== "object" || Array.isArray(address)) return "missing address";
  if (!ADDRESS_KINDS.has(address.kind)) return "unknown address kind";
  if (typeof address.name !== "string" || address.name === "") return "missing address name";
  if (typeof event.id !== "string" || event.id === "") return "missing id";
  if (event.pending !== undefined && typeof event.pending !== "boolean") return "pending is not boolean";
  if (event.cursor_unusable !== undefined && typeof event.cursor_unusable !== "boolean") {
    return "cursor_unusable is not boolean";
  }
  if (event.event === "mail") {
    if (event.reason !== "mail") return "mail reason";
  } else if (event.event === "channel_message") {
    if (event.reason !== "channel" && event.reason !== "mention") return "channel reason";
    if (typeof event.channel !== "string" || event.channel === "") return "missing channel";
  } else {
    if (event.reason !== "mail" && event.reason !== "channel") return "unreadable reason";
    if (event.reason === "channel" && (typeof event.channel !== "string" || event.channel === "")) {
      return "missing channel";
    }
  }
  return null;
}

// All or nothing: one bad line fails the whole snapshot. Post terminates every
// line, so non-empty output that does not end in a newline is truncated.
export function parseSnapshot(stdout) {
  const text = String(stdout ?? "");
  if (text !== "" && !text.endsWith("\n")) return { ok: false, reason: "truncated output" };
  const events = [];
  for (const line of text.split("\n")) {
    if (line.trim() === "") continue;
    let event;
    try {
      event = JSON.parse(line);
    } catch {
      return { ok: false, reason: "malformed line" };
    }
    const problem = validEvent(event);
    if (problem) return { ok: false, reason: problem };
    events.push(event);
  }
  return { ok: true, events };
}

// ------------------------------------------------------ selection and keys (E5)

export function eventSource(event) {
  if (event.event === "mail" || (event.event === "unreadable" && event.reason === "mail")) return "mail";
  return `channel:${event.channel}`;
}

export function stateClass(event) {
  const routing = event.pending === true ? "pending" : "routed";
  const health =
    event.event === "unreadable" ? "unreadable" : event.cursor_unusable === true ? "degraded" : "healthy";
  return `${routing}/${health}`;
}

// (kind, address kind, address name, source, id, state class): the same id
// moving pending -> routed or degraded -> healthy is a new key and rings again.
export function eventKey(event) {
  return JSON.stringify([
    event.event,
    event.address.kind,
    event.address.name,
    eventSource(event),
    event.id,
    stateClass(event),
  ]);
}

export function selectEligible(events, subscribedChannels) {
  const subscribed = subscribedChannels instanceof Set ? subscribedChannels : new Set(subscribedChannels ?? []);
  return events.filter((event) => {
    if (event.event === "mail" || event.event === "unreadable") return true;
    if (event.reason === "mention") return true;
    return subscribed.has(event.channel);
  });
}

export function blindSpots(eligible) {
  const spots = new Map();
  for (const event of eligible) {
    if (event.event !== "unreadable") continue;
    const where = event.reason === "mail" ? "inbox" : `#${noticeName(event.channel)}`;
    spots.set(where, (spots.get(where) ?? 0) + 1);
  }
  return [...spots].map(([where, count]) => ({ where, count }));
}

// Waiting scan reasons. `post watch --reason mention` drops unreadable channel
// events (src/cli.rs: an unreadable channel message always has reason
// `channel`), so every scan also asks for `channel` and filters here; an
// unreadable channel is never silently invisible (design, B1 check).
export const SCAN_REASONS = Object.freeze(["mail", "mention", "channel"]);

export function snapshotArgs() {
  const args = ["watch", "--snapshot", "--json", "--limit", "0"];
  for (const reason of SCAN_REASONS) args.push("--reason", reason);
  return args;
}

// ------------------------------------------------------------------ notice

function plural(count, one, many) {
  return `${count} ${count === 1 ? one : many}`;
}

export function buildNotice(participantId, eligible) {
  const id = noticeName(participantId);
  let direct = 0;
  let pending = 0;
  let mentions = 0;
  let degraded = 0;
  const perChannel = new Map();
  const readChannels = new Set();
  let unreadable = 0;
  const unreadableWhere = [];
  for (const event of eligible) {
    if (event.event === "unreadable") {
      unreadable += 1;
      const where = event.reason === "mail" ? "the inbox" : `#${noticeName(event.channel)}`;
      if (!unreadableWhere.includes(where)) unreadableWhere.push(where);
      if (event.reason === "channel") readChannels.add(noticeName(event.channel));
      continue;
    }
    if (event.event === "channel_message") readChannels.add(noticeName(event.channel));
    if (event.pending === true) {
      pending += 1;
    } else if (event.cursor_unusable === true) {
      degraded += 1;
    } else if (event.event === "mail") {
      direct += 1;
    } else if (event.reason === "mention") {
      mentions += 1;
    } else {
      const name = noticeName(event.channel);
      perChannel.set(name, (perChannel.get(name) ?? 0) + 1);
    }
  }
  const parts = [];
  if (direct) parts.push(`${direct} direct`);
  if (mentions) parts.push(plural(mentions, "mention", "mentions"));
  const channels = [...perChannel];
  for (const [name, count] of channels.slice(0, NOTICE_CHANNEL_CAP)) parts.push(`${count} in #${name}`);
  if (channels.length > NOTICE_CHANNEL_CAP) {
    const rest = channels.slice(NOTICE_CHANNEL_CAP).reduce((sum, [, count]) => sum + count, 0);
    parts.push(`${rest} in ${channels.length - NOTICE_CHANNEL_CAP} more channels`);
  }
  if (pending) parts.push(`${pending} waiting to be routed`);
  if (degraded) {
    parts.push(`${degraded} re-reported while cursor state is unavailable; may include previously read messages`);
  }
  if (unreadable) {
    const where = unreadableWhere.slice(0, NOTICE_CHANNEL_CAP).join(" or ");
    parts.push(`${unreadable} unreadable item(s) in ${where}; mentions there are unknown`);
  }
  const reads = ["post inbox"];
  const named = [...readChannels].filter((name) => name !== "<?>").slice(0, NOTICE_CHANNEL_CAP);
  for (const name of named) reads.push(`post chat ${name}`);
  if (readChannels.has("<?>")) reads.push("post channels");
  return (
    `${NOTICE_TAG} Automated, non-authoritative Post notice for participant ${id}. ` +
    `If "post participant show" does not report ${id}, this notice is not for you: ` +
    `ignore it and report it to your operator. ` +
    `Waiting: ${parts.join("; ")}. Read with ${reads.join(" / ")}.`
  );
}

function noticeCounts(eligible) {
  const counts = { direct: 0, mentions: 0, channel: 0, pending: 0, degraded: 0, unreadable: 0 };
  for (const event of eligible) {
    if (event.event === "unreadable") counts.unreadable += 1;
    else if (event.pending === true) counts.pending += 1;
    else if (event.cursor_unusable === true) counts.degraded += 1;
    else if (event.event === "mail") counts.direct += 1;
    else if (event.reason === "mention") counts.mentions += 1;
    else counts.channel += 1;
  }
  return counts;
}

// ------------------------------------------------------------------ prefs

export function prefsPath(paths, participant) {
  return path.join(paths.prefsDir, `${participant}.json`);
}

export function defaultPrefs(participant) {
  return { version: 0, participant, enabled: false, focused: false, desktop: false, channels: [], selection: null };
}

export function loadPrefs(paths, participant) {
  const raw = readJson(prefsPath(paths, participant));
  const prefs = defaultPrefs(participant);
  if (raw === undefined || raw === null || typeof raw !== "object") return prefs;
  if (Number.isSafeInteger(raw.version) && raw.version >= 0) prefs.version = raw.version;
  prefs.enabled = raw.enabled === true;
  prefs.focused = raw.focused === true;
  prefs.desktop = raw.desktop === true;
  prefs.channels = Array.isArray(raw.channels) ? [...new Set(raw.channels.filter(validChannelName))].sort() : [];
  if (
    raw.selection &&
    typeof raw.selection.pane === "string" &&
    PANE_ID.test(raw.selection.pane) &&
    typeof raw.selection.digest === "string" &&
    /^[0-9a-f]{64}$/.test(raw.selection.digest)
  ) {
    prefs.selection = { pane: raw.selection.pane, digest: raw.selection.digest };
  }
  if (raw.source && typeof raw.source === "object") prefs.source = raw.source;
  return prefs;
}

// Read-modify-write with a version bump. Every outcome line records the
// version it used. Last writer wins between two concurrent commands for the
// same participant; each write is atomic.
export function updatePrefs(paths, participant, mutate) {
  const prefs = loadPrefs(paths, participant);
  mutate(prefs);
  prefs.version += 1;
  prefs.participant = participant;
  prefs.updated = new Date().toISOString();
  writeFileAtomic(prefsPath(paths, participant), `${JSON.stringify(prefs, null, 2)}\n`);
  return prefs;
}

// ------------------------------------------------------------------ state

function stateFileFor(paths, participant, sink, genHash) {
  return path.join(paths.stateDir, participant, `${sink}-${genHash}.json`);
}

export function generationHash(generation) {
  return sha256(JSON.stringify([generation.pane, generation.terminal, generation.digest])).slice(0, 16);
}

// ------------------------------------------------------------------ watches

function defaultWatch(target, { recursive }, onEvent, onError) {
  let watcher;
  try {
    watcher = fs.watch(target, { recursive, persistent: false }, onEvent);
  } catch (error) {
    if (error.code === "ENOENT" || error.code === "ENOTDIR") return null;
    onError(error);
    return null;
  }
  watcher.on("error", onError);
  return { close: () => watcher.close() };
}

// Writes that are not mail: live `post watch` heartbeats, activation notices,
// locks, and temps (the rename that follows a temp is its own event).
function ignoredHintFile(filename) {
  if (!filename) return false;
  const base = path.basename(String(filename));
  return (
    base === "watch.heartbeat" ||
    base.startsWith("watch.heartbeat") ||
    base === "activation-notice" ||
    base.endsWith(".lock") ||
    (base.startsWith(".") && base.endsWith(".tmp"))
  );
}

// ------------------------------------------------------------------ supervisor

export class Supervisor {
  constructor({ paths, exec, watch, now, log, config = {}, env = process.env } = {}) {
    this.paths = paths;
    this.config = { ...DEFAULTS, ...config };
    this.now = now ?? (() => Date.now());
    this.log = log ?? ((record) => process.stdout.write(`${JSON.stringify(record)}\n`));
    this.watchImpl = watch ?? defaultWatch;
    this.env = env;
    this.children = new Set();
    this.exec = exec ?? ((kind, args, opts = {}) => this.defaultExec(kind, args, opts));
    this.startedAt = new Date(this.now()).toISOString();
    this.halted = false;
    this.participants = new Map();
    this.byDigest = new Map();
    this.participantsLoaded = false;
    this.participantsFetchedAt = -Infinity;
    this.participantsDirMtime = undefined;
    this.bindings = new Map();
    this.subs = new Map();
    this.retired = [];
    this.retiredKeys = new Map();
    this.participantsListSeq = 0;
    this.knownRootDirs = null;
    this.prefsCache = new Map();
    this.membership = new Map();
    this.logged = new Set();
    this.inflight = 0;
    this.rrCursor = null;
    this.watches = new Map();
    this.pendingHints = new Set();
    this.hintTimer = null;
    this.lastReconcileAt = -Infinity;
    this.reconcileRound = null;
    this.herdrOk = null;
    this.postOk = null;
    this.lastDiscoveryAt = null;
    this.lastHealthJson = null;
    this.lastHealthWriteAt = -Infinity;
    this.seq = 0;
    this.versions = { herdr: null, post: null };
    this.stats = { snapshots: 0, reconcileScans: 0, hintScans: 0, discoveries: 0 };
    this.idleWaiters = [];
    this.lastPruneAt = -Infinity;
  }

  defaultExec(kind, args, { participant, timeoutMs, capBytes } = {}) {
    const bin = kind === "post" ? this.paths.postBin : kind === "herdr" ? this.paths.herdrBin : this.paths.cmuxBin;
    const env = { ...this.env, POST_MAIL_ROOT: this.paths.root };
    delete env.POST_PARTICIPANT;
    if (participant !== undefined) env.POST_PARTICIPANT = participant;
    return runCommand(bin, args, {
      env,
      // A neutral cwd: post must never fall back to a cwd room.
      cwd: this.paths.doorbell,
      timeoutMs: timeoutMs ?? this.config.commandTimeoutMs,
      capBytes,
      children: this.children,
    });
  }

  logOnce(key, record) {
    if (this.logged.has(key)) return;
    this.logged.add(key);
    this.emit(record);
  }

  emit(record) {
    this.log({ ts: new Date(this.now()).toISOString(), ...record });
  }

  // Stop all delivery at once: used when the singleton lock is lost.
  halt(reason) {
    if (this.halted) return;
    this.halted = true;
    this.emit({ type: "halt", reason });
    for (const child of this.children) child.kill("SIGKILL");
    this.closeWatches();
    if (this.hintTimer) clearTimeout(this.hintTimer);
  }

  loadConfig() {
    const raw = readJson(this.paths.configFile);
    if (!raw || typeof raw !== "object") return;
    for (const [key, value] of Object.entries(raw)) {
      if (TUNABLE.has(key) && Number.isSafeInteger(value) && value > 0) this.config[key] = value;
    }
  }

  // Cached by file mtime, so a prefs change is seen by the next discovery
  // tick even when no watch fires.
  prefsFor(participant) {
    let mtime = null;
    try {
      mtime = fs.statSync(prefsPath(this.paths, participant)).mtimeMs;
    } catch {
      mtime = null;
    }
    const cached = this.prefsCache.get(participant);
    if (cached && cached.mtime === mtime) return cached.prefs;
    const prefs = loadPrefs(this.paths, participant);
    this.prefsCache.set(participant, { prefs, mtime });
    return prefs;
  }

  reloadPrefs(participant) {
    this.prefsCache.delete(participant);
    return this.prefsFor(participant);
  }

  async probeVersions() {
    const herdr = await this.exec("herdr", ["--version"], { timeoutMs: 5000 });
    if (herdr.ok) this.versions.herdr = stderrExcerpt(herdr.stdout);
    const post = await this.exec("post", ["version", "--json"], { timeoutMs: 5000 });
    if (post.ok) {
      try {
        const parsed = JSON.parse(post.stdout);
        this.versions.post = `${parsed.version} (${parsed.build_sha})`;
      } catch {
        // Health reports null.
      }
    }
  }

  // ---------------------------------------------------------- participants

  async maybeRefreshParticipants() {
    let mtime;
    try {
      mtime = fs.statSync(path.join(this.paths.root, "participants")).mtimeMs;
    } catch {
      mtime = null;
    }
    const due =
      !this.participantsLoaded ||
      mtime !== this.participantsDirMtime ||
      this.now() - this.participantsFetchedAt >= this.config.participantRefreshMs;
    if (!due) return;
    const result = await this.exec("post", ["participant", "list", "--json"], {});
    let parsed;
    if (result.ok) {
      try {
        parsed = JSON.parse(result.stdout);
      } catch {
        parsed = undefined;
      }
    }
    if (!parsed || parsed.ok !== true || !Array.isArray(parsed.participants)) {
      this.postOk = false;
      this.logOnce("post-list-failed", { type: "discovery", problem: "post participant list failed", ...failureDetail(result) });
      return;
    }
    this.logged.delete("post-list-failed");
    this.postOk = true;
    const participants = new Map();
    const byDigest = new Map();
    for (const row of parsed.participants) {
      if (!row || typeof row.id !== "string" || typeof row.conversation_key_digest !== "string") continue;
      if (!validStateId(row.id)) {
        this.logOnce(`bad-id:${row.id}`, { type: "discovery", problem: "participant id outside the doorbell alphabet", participant: noticeName(row.id) });
        continue;
      }
      participants.set(row.id, row);
      const list = byDigest.get(row.conversation_key_digest) ?? [];
      list.push(row);
      byDigest.set(row.conversation_key_digest, list);
    }
    this.participants = participants;
    this.byDigest = byDigest;
    this.participantsLoaded = true;
    this.participantsListSeq += 1;
    this.participantsFetchedAt = this.now();
    this.participantsDirMtime = mtime;
  }

  // ---------------------------------------------------------- discovery

  async discover() {
    if (this.halted) return;
    this.stats.discoveries += 1;
    const listed = await this.exec("herdr", ["agent", "list"], {});
    let agents;
    if (listed.ok) {
      try {
        agents = JSON.parse(listed.stdout)?.result?.agents;
      } catch {
        agents = undefined;
      }
    }
    if (!Array.isArray(agents)) {
      // A herdr error is not evidence that any target is gone: nothing retires.
      this.herdrOk = false;
      this.logOnce("herdr-list-failed", { type: "discovery", problem: "herdr agent list failed", ...failureDetail(listed) });
      return;
    }
    this.logged.delete("herdr-list-failed");
    this.herdrOk = true;
    await this.maybeRefreshParticipants();
    if (!this.participantsLoaded) return;
    this.lastDiscoveryAt = this.now();

    const panesByDigest = new Map();
    for (const agent of agents) {
      const session = agent?.agent_session;
      if (
        typeof agent?.pane_id !== "string" ||
        !PANE_ID.test(agent.pane_id) ||
        typeof agent.terminal_id !== "string" ||
        session?.kind !== "id" ||
        typeof session.value !== "string" ||
        session.value === ""
      ) {
        continue;
      }
      const digest = sha256(session.value);
      const pane = {
        pane: agent.pane_id,
        terminal: agent.terminal_id,
        digest,
        status: agent.agent_status,
        focused: agent.focused === true,
      };
      const list = panesByDigest.get(digest) ?? [];
      list.push(pane);
      panesByDigest.set(digest, list);
    }

    const bindings = new Map();
    const seen = new Set();
    for (const [digest, panes] of panesByDigest) {
      const matches = this.byDigest.get(digest) ?? [];
      if (matches.length === 0) {
        this.logOnce(`unmatched:${digest}`, { type: "discovery", problem: "pane session matches no participant", pane: panes[0].pane });
        continue;
      }
      if (matches.length > 1) {
        this.logOnce(`collision:${digest}`, {
          type: "discovery",
          problem: "one session digest matches several participants; not a target",
          participants: matches.map((row) => row.id),
        });
        continue;
      }
      const participant = matches[0];
      const prefs = this.prefsFor(participant.id);
      let chosen = null;
      let state;
      if (prefs.selection && prefs.selection.digest === digest) {
        chosen = panes.find((pane) => pane.pane === prefs.selection.pane) ?? null;
      }
      if (chosen) state = "bound";
      else if (panes.length === 1) {
        chosen = panes[0];
        state = "bound";
      } else state = "ambiguous";
      if (participant.ended_at) state = "ended";
      const binding = { participant: participant.id, state, panes, chosen, digest, selected: Boolean(chosen && prefs.selection?.pane === chosen.pane) };
      bindings.set(participant.id, binding);
      const previous = this.bindings.get(participant.id);
      if (!previous || previous.state !== state) {
        this.emit({ type: "binding", participant: participant.id, state, panes: panes.map((pane) => pane.pane) });
      }
      if (!chosen || state === "ended") continue;
      const generation = { pane: chosen.pane, terminal: chosen.terminal, digest };
      const genHash = generationHash(generation);
      const key = `${participant.id}|${SINK_HERDR}|${genHash}`;
      seen.add(key);
      let sub = this.subs.get(key);
      if (!sub && this.retiredKeys.has(key)) {
        // A retired generation stays retired. A participant-level retirement
        // lifts only once a participant list fetched after it shows the
        // participant live again (a rebind clears ended_at).
        const entry = this.retiredKeys.get(key);
        if (!entry.participantLevel || this.participantsListSeq <= entry.listSeq) continue;
        this.retiredKeys.delete(key);
      }
      if (!sub) {
        sub = this.createSub(participant.id, SINK_HERDR, generation, genHash);
        this.emit({ type: "generation", participant: participant.id, sink: SINK_HERDR, generation: genHash, pane: chosen.pane });
      }
      sub.paneStatus = chosen.status;
      sub.focused = chosen.focused;
      if (sub.prefsVersionSeen !== prefs.version) {
        if (sub.prefsVersionSeen !== undefined && sub.armed) this.markDirty(sub, "prefs");
        sub.prefsVersionSeen = prefs.version;
      }
      const armed = prefs.enabled && state === "bound";
      if (armed !== sub.armed) {
        sub.armed = armed;
        this.emit({ type: "arm", participant: participant.id, generation: genHash, armed, prefs_version: prefs.version });
        if (armed) this.markDirty(sub, "armed");
      }
      const scannable = ["idle", "done"].includes(chosen.status) && (!chosen.focused || prefs.focused);
      if (scannable && !sub.scannable && sub.armed) this.markDirty(sub, "scannable");
      sub.scannable = scannable;
    }

    for (const [key, sub] of this.subs) {
      if (seen.has(key)) continue;
      const binding = bindings.get(sub.participant);
      const stillCarried = binding?.state === "ambiguous" && binding.panes.some(
        (pane) => pane.pane === sub.generation.pane && pane.terminal === sub.generation.terminal
      );
      if (stillCarried) {
        if (sub.armed) {
          sub.armed = false;
          this.emit({ type: "arm", participant: sub.participant, generation: sub.genHash, armed: false, reason: "ambiguous" });
        }
        continue;
      }
      let reason;
      const carrier = agents.find((agent) => agent?.pane_id === sub.generation.pane);
      const carrierSession = carrier?.agent_session;
      const carrierDigest = carrierSession?.kind === "id" && typeof carrierSession.value === "string" ? sha256(carrierSession.value) : null;
      if (binding?.state === "ended") reason = "participant ended";
      else if (!this.participants.has(sub.participant)) reason = "participant gone";
      else if (!carrier) reason = "pane gone";
      else if (carrier.terminal_id !== sub.generation.terminal) reason = "terminal changed";
      else if (carrierDigest !== sub.generation.digest) reason = "session changed";
      else reason = "selection moved";
      this.retire(sub, reason);
    }
    this.bindings = bindings;
    this.rearmWatches();
    this.pump();
  }

  createSub(participant, sink, generation, genHash) {
    const sub = {
      key: `${participant}|${sink}|${genHash}`,
      participant,
      sink,
      generation,
      genHash,
      stateFile: stateFileFor(this.paths, participant, sink, genHash),
      dirtyGen: 0,
      cleanGen: 0,
      hinted: false,
      reconcileRound: null,
      inFlight: false,
      armed: false,
      scannable: false,
      paneStatus: null,
      focused: false,
      consecutiveFailures: 0,
      nextAttemptAt: 0,
      lastOutcome: null,
      lastOutcomeAt: null,
      lastSuccessAt: null,
      lastError: null,
      blindSpots: [],
      scanPrefsVersion: null,
      scanChannels: null,
      knownChannels: new Set(),
      retired: false,
      createdAt: this.now(),
    };
    // A retired generation's state is never read again: a reappearing
    // generation starts empty (at-least-once, one notice for what is unread).
    const existing = readJson(sub.stateFile);
    if (existing?.retired_at) {
      try {
        fs.unlinkSync(sub.stateFile);
      } catch {
        // Recreated on first save.
      }
    }
    this.subs.set(sub.key, sub);
    return sub;
  }

  retire(sub, reason) {
    if (sub.retired) return;
    sub.retired = true;
    sub.armed = false;
    this.subs.delete(sub.key);
    const participantLevel = reason === "participant ended" || reason === "participant gone";
    this.retiredKeys.set(sub.key, { participantLevel, listSeq: this.participantsListSeq });
    // The 60-second list cache must not keep an ended participant bound.
    if (participantLevel) this.participantsFetchedAt = -Infinity;
    this.emit({ type: "outcome", outcome: "retired", participant: sub.participant, sink: sub.sink, generation: sub.genHash, pane: sub.generation.pane, reason });
    const state = readJson(sub.stateFile);
    if (state) {
      try {
        writeFileAtomic(sub.stateFile, `${JSON.stringify({ ...state, retired_at: new Date(this.now()).toISOString(), retired_reason: reason })}\n`);
      } catch {
        // Frozen state is best effort; it is never read again either way.
      }
    }
    this.retired.unshift({ participant: sub.participant, generation: sub.genHash, pane: sub.generation.pane, reason, at: new Date(this.now()).toISOString() });
    this.retired.length = Math.min(this.retired.length, 20);
  }

  // ---------------------------------------------------------- dirty and hints

  markDirty(sub, reason) {
    sub.dirtyGen += 1;
    if (reason === "hint") sub.hinted = true;
    if (reason === "reconcile" && this.reconcileRound) sub.reconcileRound = this.reconcileRound.id;
  }

  isDirty(sub) {
    return sub.dirtyGen !== sub.cleanGen;
  }

  armedSubs() {
    return [...this.subs.values()].filter((sub) => sub.armed && !sub.retired);
  }

  hint(participants) {
    for (const participant of participants) this.pendingHints.add(participant);
    if (this.hintTimer || this.halted) return;
    this.hintTimer = setTimeout(() => this.flushHints(), this.config.hintCoalesceMs);
    this.hintTimer.unref?.();
  }

  // Coalesced hints: "*" marks every armed subscription, "#reconcile" runs a
  // full reconciliation, anything else is a participant id.
  flushHints() {
    if (this.hintTimer) clearTimeout(this.hintTimer);
    this.hintTimer = null;
    const pending = this.pendingHints;
    this.pendingHints = new Set();
    if (pending.has("#reconcile")) {
      this.reconcile("structure");
      return;
    }
    const all = pending.has("*");
    for (const sub of this.armedSubs()) {
      if (all || pending.has(sub.participant)) this.markDirty(sub, "hint");
    }
    this.pump();
  }

  membershipDirty(participant) {
    const entry = this.membership.get(participant);
    if (entry) entry.stale = true;
  }

  // Joined channels for targeted channel watches, from post's own output.
  async refreshMembership(participant) {
    const entry = this.membership.get(participant) ?? { channels: new Set(), at: -Infinity, stale: true, running: false };
    this.membership.set(participant, entry);
    if (entry.running) return;
    const due = entry.stale || this.now() - entry.at >= this.config.membershipRefreshMs;
    if (!due || this.now() - entry.at < 10_000) return;
    entry.running = true;
    try {
      const result = await this.exec("post", ["channels", "--json"], { participant });
      if (!result.ok) return;
      let parsed;
      try {
        parsed = JSON.parse(result.stdout);
      } catch {
        return;
      }
      if (!Array.isArray(parsed?.channels)) return;
      const channels = new Set();
      for (const row of parsed.channels) {
        if (typeof row?.name !== "string") continue;
        if (Array.isArray(row.participants) && row.participants.includes(participant)) channels.add(row.name);
      }
      const changed = channels.size !== entry.channels.size || [...channels].some((name) => !entry.channels.has(name));
      entry.channels = channels;
      entry.at = this.now();
      entry.stale = false;
      if (changed) this.rearmWatches();
    } finally {
      entry.running = false;
    }
  }

  desiredWatches() {
    const specs = new Map();
    const add = (target, recursive, kind, participant) => {
      const key = `${recursive ? "R" : "N"}:${target}`;
      const spec = specs.get(key) ?? { target, recursive, kind, participants: new Set() };
      if (participant) spec.participants.add(participant);
      specs.set(key, spec);
    };
    add(this.paths.root, false, "root");
    add(this.paths.prefsDir, false, "prefs");
    add(this.paths.doorbell, false, "doorbell");
    for (const sub of this.armedSubs()) {
      const row = this.participants.get(sub.participant);
      add(path.join(this.paths.root, "participants", sub.participant), true, "participant", sub.participant);
      if (typeof row?.workspace === "string" && validStateId(row.workspace)) {
        add(path.join(this.paths.root, row.workspace, "inbox"), false, "workspace", sub.participant);
      }
      if (typeof row?.lineage === "string" && validStateId(row.lineage)) {
        add(path.join(this.paths.root, "lineages", row.lineage), true, "lineage", sub.participant);
      }
      const channels = new Set([
        ...(this.membership.get(sub.participant)?.channels ?? []),
        ...sub.knownChannels,
        ...this.prefsFor(sub.participant).channels,
      ]);
      for (const channel of channels) {
        if (validStateId(channel)) add(path.join(this.paths.root, "channels", channel), true, "channel", sub.participant);
      }
    }
    return specs;
  }

  rearmWatches(force = false) {
    if (this.halted) return;
    const desired = this.desiredWatches();
    for (const [key, entry] of this.watches) {
      if (force || !desired.has(key)) {
        entry.handle?.close();
        this.watches.delete(key);
      }
    }
    for (const [key, spec] of desired) {
      const existing = this.watches.get(key);
      if (existing) {
        existing.spec = spec;
        if (existing.handle) continue;
      }
      const entry = { spec, handle: null };
      entry.handle = this.watchImpl(
        spec.target,
        { recursive: spec.recursive },
        (eventType, filename) => this.onWatchEvent(entry, eventType, filename),
        (error) => this.onWatchError(entry, error)
      );
      this.watches.set(key, entry);
    }
    if (this.knownRootDirs === null) this.knownRootDirs = this.listRootDirs();
    for (const sub of this.armedSubs()) void this.refreshMembership(sub.participant).catch(() => {});
  }

  // A new or vanished top-level directory (a room, channels/, lineages/) is a
  // new relevant directory; appends and lock files at the root are not.
  rootDirChanged(name) {
    if (!name || ignoredHintFile(name) || name === "doorbell") return false;
    if (this.knownRootDirs === null) this.knownRootDirs = this.listRootDirs();
    let isDir = false;
    try {
      isDir = fs.statSync(path.join(this.paths.root, name)).isDirectory();
    } catch {
      isDir = false;
    }
    const known = this.knownRootDirs.has(name);
    if (isDir === known) return false;
    if (isDir) this.knownRootDirs.add(name);
    else this.knownRootDirs.delete(name);
    return true;
  }

  listRootDirs() {
    try {
      return new Set(fs.readdirSync(this.paths.root, { withFileTypes: true }).filter((entry) => entry.isDirectory()).map((entry) => entry.name));
    } catch {
      return new Set();
    }
  }

  rearmSoon() {
    if (this.rearmTimer || this.halted) return;
    this.rearmTimer = setTimeout(() => {
      this.rearmTimer = null;
      for (const sub of this.armedSubs()) void this.refreshMembership(sub.participant).catch(() => {});
    }, 1000);
    this.rearmTimer.unref?.();
  }

  closeWatches() {
    for (const entry of this.watches.values()) entry.handle?.close();
    this.watches.clear();
  }

  onWatchEvent(entry, eventType, filename) {
    if (this.halted) return;
    const { spec } = entry;
    const name = filename ? String(filename) : "";
    switch (spec.kind) {
      case "root":
        if (name === "rooms.json") this.hint(["*"]);
        else if (eventType === "rename" && this.rootDirChanged(name)) this.hint(["#reconcile"]);
        return;
      case "prefs": {
        const participant = name.endsWith(".json") ? name.slice(0, -5) : "";
        if (participant && validStateId(participant)) {
          this.reloadPrefs(participant);
          this.hint([participant]);
        }
        return;
      }
      case "doorbell":
        if (name === "config.json") {
          this.loadConfig();
          this.hint(["#reconcile"]);
        }
        return;
      default:
        if (ignoredHintFile(name)) return;
        if (spec.kind === "participant" && path.basename(name) === "channels.json") {
          for (const participant of spec.participants) this.membershipDirty(participant);
          this.rearmSoon();
        }
        this.hint([...spec.participants]);
    }
  }

  // A watcher error, overflow, or a watched directory being replaced: the
  // hints are no longer trustworthy, so reconcile everything and re-arm.
  onWatchError(entry, error) {
    if (this.halted) return;
    this.emit({ type: "watch", problem: "watcher error; full reconciliation", target: entry.spec.kind, error: String(error?.code ?? error?.message ?? error) });
    entry.handle?.close();
    entry.handle = null;
    this.reconcile("watch-error", true);
  }

  // Every armed subscription is scanned whether or not a hint fired.
  reconcile(reason = "timer", rearmAll = false) {
    if (this.halted) return;
    if (this.reconcileRound?.gaps.length) {
      this.emit({ type: "reconcile", problem: "hint gaps found by reconciliation", round: this.reconcileRound.id, gaps: this.reconcileRound.gaps });
    }
    this.reconcileRound = { id: (this.reconcileRound?.id ?? 0) + 1, reason, gaps: [] };
    this.lastReconcileAt = this.now();
    for (const sub of this.armedSubs()) this.markDirty(sub, "reconcile");
    this.rearmWatches(rearmAll);
    this.pump();
  }

  // ---------------------------------------------------------- scheduling

  nextReady() {
    const keys = [...this.subs.keys()];
    if (keys.length === 0) return null;
    let start = this.rrCursor === null ? 0 : keys.indexOf(this.rrCursor) + 1;
    if (start < 0) start = 0;
    const now = this.now();
    for (let offset = 0; offset < keys.length; offset++) {
      const sub = this.subs.get(keys[(start + offset) % keys.length]);
      if (sub.armed && sub.scannable && !sub.inFlight && this.isDirty(sub) && now >= sub.nextAttemptAt) {
        this.rrCursor = sub.key;
        return sub;
      }
    }
    return null;
  }

  pump() {
    if (this.halted) return;
    while (this.inflight < this.config.concurrency) {
      const sub = this.nextReady();
      if (!sub) break;
      this.inflight += 1;
      sub.inFlight = true;
      this.scan(sub)
        .catch((error) => this.recordFailure(sub, "internal", { error: String(error?.stack ?? error).slice(0, 300) }))
        .finally(() => {
          sub.inFlight = false;
          this.inflight -= 1;
          this.pump();
          if (this.inflight === 0) this.resolveIdle();
        });
    }
    if (this.inflight === 0) this.resolveIdle();
  }

  resolveIdle() {
    const waiters = this.idleWaiters;
    this.idleWaiters = [];
    for (const resolve of waiters) resolve();
  }

  // Resolves when no scan is in flight and nothing ready remains.
  idle() {
    if (this.inflight === 0 && !this.nextReadyPeek()) return Promise.resolve();
    return new Promise((resolve) => this.idleWaiters.push(resolve)).then(() => this.idle());
  }

  nextReadyPeek() {
    const cursor = this.rrCursor;
    const sub = this.nextReady();
    this.rrCursor = cursor;
    return sub;
  }

  // ---------------------------------------------------------- scan and ring

  async scan(sub) {
    const startGen = sub.dirtyGen;
    const hinted = sub.hinted;
    const round = sub.reconcileRound;
    const prefs = this.prefsFor(sub.participant);
    this.stats.snapshots += 1;
    if (hinted) this.stats.hintScans += 1;
    else this.stats.reconcileScans += 1;
    const result = await this.exec("post", snapshotArgs(), {
      participant: sub.participant,
      timeoutMs: this.config.snapshotTimeoutMs,
      capBytes: this.config.snapshotCapBytes,
    });
    if (this.halted || sub.retired) return;
    if (!result.ok) {
      const stage = result.oversize ? "snapshot_oversize" : result.timedOut ? "snapshot_timeout" : "snapshot";
      this.recordFailure(sub, stage, failureDetail(result));
      return;
    }
    if (/participant: unbound/.test(String(result.stderr ?? ""))) {
      this.recordFailure(sub, "snapshot_unbound", failureDetail(result));
      return;
    }
    const parsed = parseSnapshot(result.stdout);
    if (!parsed.ok) {
      this.recordFailure(sub, "snapshot_malformed", { detail: parsed.reason });
      return;
    }
    for (const event of parsed.events) {
      if (event.event !== "mail" && typeof event.channel === "string") sub.knownChannels.add(event.channel);
    }
    const eligible = selectEligible(parsed.events, prefs.channels);
    const keys = [...new Set(eligible.map(eventKey))];
    const state = this.loadSubState(sub);
    const announced = new Set(state.announced);
    const notified = new Set(state.notified);
    const freshAgent = eligible.filter((event) => !announced.has(eventKey(event)));
    const freshDesktop = prefs.desktop ? eligible.filter((event) => !notified.has(eventKey(event))) : [];
    sub.blindSpots = blindSpots(eligible);
    sub.scanPrefsVersion = prefs.version;
    sub.scanChannels = [...prefs.channels];

    if (freshAgent.length > 0 && round !== null && !hinted && this.reconcileRound?.id === round) {
      this.reconcileRound.gaps.push({ participant: sub.participant, generation: sub.genHash, fresh: freshAgent.length });
    }

    let next = { announced: [...announced].filter((key) => keys.includes(key)), notified: [...notified].filter((key) => keys.includes(key)) };
    let failed = null;
    let deferred = false;
    if (freshAgent.length > 0 || freshDesktop.length > 0) {
      const check = await this.recheckParticipant(sub);
      if (this.halted || sub.retired) return;
      if (check.retired) {
        this.retire(sub, check.retired);
        return;
      }
      if (check.failed) {
        this.recordFailure(sub, "participant_recheck", check.failed);
        return;
      }
      if (freshAgent.length > 0) {
        const ring = await this.ring(sub, prefs, eligible);
        if (this.halted) return;
        if (ring.outcome === "retired") {
          this.retire(sub, ring.reason);
          return;
        }
        if (ring.outcome === "failed") failed = ring;
        else if (ring.outcome === "deferred") deferred = true;
        else next.announced = keys;
        this.outcome(sub, ring.outcome, prefs, eligible, ring);
      }
      if (freshDesktop.length > 0) {
        const desk = await this.desktopNotify(sub, eligible);
        if (this.halted) return;
        if (desk.ok) {
          next.notified = keys;
          this.outcome(sub, "notified", prefs, eligible, {});
        } else {
          failed = failed ?? { outcome: "failed", stage: "desktop", detail: desk.detail };
          this.outcome(sub, "failed", prefs, eligible, { stage: "desktop", detail: desk.detail });
        }
      }
    }

    if (failed) {
      // A failed outcome never advances either set; the scan stays dirty.
      this.recordFailure(sub, failed.stage, failed.detail, { logged: true });
      return;
    }
    this.saveSubState(sub, state, next);
    sub.consecutiveFailures = 0;
    sub.nextAttemptAt = 0;
    sub.lastError = null;
    sub.lastSuccessAt = new Date(this.now()).toISOString();
    if (deferred) {
      // Busy or focused at the recheck: the next discovery tick that sees the
      // pane scannable marks it dirty again.
      sub.scannable = false;
    }
    // Dirty generations: clean only if nothing marked the subscription while
    // this scan ran; a change that lands mid-scan is never lost.
    if (sub.dirtyGen === startGen) {
      sub.cleanGen = startGen;
      sub.hinted = false;
      sub.reconcileRound = null;
    }
  }

  loadSubState(sub) {
    const raw = readJson(sub.stateFile);
    if (!raw || raw.retired_at) return { announced: [], notified: [] };
    return {
      announced: Array.isArray(raw.announced) ? raw.announced.filter((key) => typeof key === "string") : [],
      notified: Array.isArray(raw.notified) ? raw.notified.filter((key) => typeof key === "string") : [],
    };
  }

  saveSubState(sub, previous, next) {
    const same = (a, b) => a.length === b.length && a.every((key, index) => key === b[index]);
    const exists = fs.existsSync(sub.stateFile);
    if (exists && same(previous.announced, next.announced) && same(previous.notified, next.notified)) return;
    if (!exists && next.announced.length === 0 && next.notified.length === 0) return;
    const record = {
      participant: sub.participant,
      sink: sub.sink,
      generation: { pane: sub.generation.pane, terminal: sub.generation.terminal, digest: sub.generation.digest },
      announced: next.announced,
      notified: next.notified,
      updated: new Date(this.now()).toISOString(),
    };
    try {
      writeFileAtomic(sub.stateFile, `${JSON.stringify(record)}\n`);
    } catch (error) {
      this.emit({ type: "state", problem: "could not save dedupe state; mail may ring again", participant: sub.participant, error: String(error.code ?? error.message) });
    }
  }

  async recheckParticipant(sub) {
    const result = await this.exec("post", ["participant", "show", "--json"], { participant: sub.participant });
    if (!result.ok) return { failed: failureDetail(result) };
    let parsed;
    try {
      parsed = JSON.parse(result.stdout);
    } catch {
      return { failed: { detail: "participant show printed malformed output" } };
    }
    if (parsed?.ok !== true) return { failed: { detail: "participant show not ok" } };
    if (parsed.status === "unbound") {
      if (parsed.participant_error) return { failed: { detail: stderrExcerpt(parsed.participant_error) } };
      return { retired: "participant gone" };
    }
    if (parsed.status !== "bound" || parsed.id !== sub.participant || parsed.participant?.id !== sub.participant) {
      return { failed: { detail: "participant show reported another participant" } };
    }
    if (parsed.participant.ended_at) return { retired: "participant ended" };
    return { ok: true };
  }

  async ring(sub, prefs, eligible) {
    const got = await this.exec("herdr", ["agent", "get", sub.generation.pane], {});
    if (this.halted) return { outcome: "failed", stage: "halted", detail: {} };
    if (!got.ok) {
      if (!got.timedOut && !got.spawnError && herdrErrorCode(got.stderr) === "agent_not_found") {
        return { outcome: "retired", reason: "pane gone" };
      }
      return { outcome: "failed", stage: "herdr_get", detail: failureDetail(got) };
    }
    let agent;
    try {
      agent = JSON.parse(got.stdout)?.result?.agent;
    } catch {
      agent = undefined;
    }
    if (
      !agent ||
      agent.pane_id !== sub.generation.pane ||
      typeof agent.terminal_id !== "string" ||
      !HERDR_STATUSES.has(agent.agent_status) ||
      typeof agent.focused !== "boolean"
    ) {
      return { outcome: "failed", stage: "herdr_get", detail: { detail: "herdr agent state was malformed" } };
    }
    if (agent.terminal_id !== sub.generation.terminal) return { outcome: "retired", reason: "terminal changed" };
    const session = agent.agent_session;
    const digest = session?.kind === "id" && typeof session.value === "string" ? sha256(session.value) : null;
    if (digest !== sub.generation.digest) return { outcome: "retired", reason: "session changed" };
    if (!["idle", "done"].includes(agent.agent_status) || (agent.focused && !prefs.focused)) {
      return { outcome: "deferred", reason: agent.focused ? "focused" : agent.agent_status };
    }
    if (this.halted) return { outcome: "failed", stage: "halted", detail: {} };
    const notice = buildNotice(sub.participant, eligible);
    const prompted = await this.exec("herdr", ["agent", "prompt", sub.generation.pane, notice], {});
    if (prompted.ok) return { outcome: "accepted" };
    const code = herdrErrorCode(prompted.stderr);
    if (code === "agent_blocked") return { outcome: "deferred", reason: "blocked" };
    if (code === "agent_not_found") return { outcome: "retired", reason: "pane gone" };
    return { outcome: "failed", stage: "herdr_prompt", detail: failureDetail(prompted) };
  }

  async desktopNotify(sub, eligible) {
    const counts = noticeCounts(eligible);
    const body = `Post for ${noticeName(sub.participant)}: ${counts.direct} direct, ${counts.mentions} mentions, ${counts.channel} channel, ${counts.pending} to be routed.`;
    const result = await this.exec("cmux", ["notify", "--title", "Post", "--body", body], {});
    return result.ok ? { ok: true } : { ok: false, detail: failureDetail(result) };
  }

  outcome(sub, outcome, prefs, eligible, extra) {
    sub.lastOutcome = outcome;
    sub.lastOutcomeAt = new Date(this.now()).toISOString();
    if (outcome === "deferred") return;
    const record = {
      type: "outcome",
      outcome,
      participant: sub.participant,
      sink: sub.sink,
      generation: sub.genHash,
      pane: sub.generation.pane,
      prefs_version: prefs.version,
      counts: noticeCounts(eligible),
    };
    if (extra?.stage) record.stage = extra.stage;
    if (extra?.detail) Object.assign(record, extra.detail);
    this.emit(record);
  }

  recordFailure(sub, stage, detail, { logged = false } = {}) {
    sub.consecutiveFailures += 1;
    const delay = Math.min(this.config.backoffBaseMs * 2 ** (sub.consecutiveFailures - 1), this.config.backoffCapMs);
    sub.nextAttemptAt = this.now() + delay;
    sub.lastError = { stage, ...detail, at: new Date(this.now()).toISOString() };
    sub.lastOutcome = "failed";
    sub.lastOutcomeAt = sub.lastError.at;
    if (!logged) {
      this.emit({ type: "outcome", outcome: "failed", participant: sub.participant, sink: sub.sink, generation: sub.genHash, stage, prefs_version: this.prefsFor(sub.participant).version, ...detail });
    }
    if (sub.consecutiveFailures === this.config.brokenAfter) {
      this.emit({ type: "broken", participant: sub.participant, generation: sub.genHash, stage, retry_ms: delay });
    }
  }

  // ---------------------------------------------------------- health

  healthSnapshot() {
    const bindings = [];
    const subsByParticipant = new Map();
    for (const sub of this.subs.values()) subsByParticipant.set(sub.participant, sub);
    for (const binding of this.bindings.values()) {
      const sub = binding.chosen ? subsByParticipant.get(binding.participant) : undefined;
      const prefs = this.prefsFor(binding.participant);
      bindings.push({
        participant: binding.participant,
        state: binding.state,
        panes: binding.panes.map((pane) => pane.pane),
        armed: Boolean(sub?.armed),
        enabled: prefs.enabled,
        focused_ok: prefs.focused,
        channels: prefs.channels,
        prefs_version: prefs.version,
        generation: sub ? { hash: sub.genHash, pane: sub.generation.pane, terminal: sub.generation.terminal } : null,
        pane_status: sub?.paneStatus ?? null,
        last_outcome: sub?.lastOutcome ?? null,
        last_outcome_at: sub?.lastOutcomeAt ?? null,
        last_success_scan_at: sub?.lastSuccessAt ?? null,
        scan_prefs_version: sub?.scanPrefsVersion ?? null,
        scan_channels: sub?.scanChannels ?? null,
        scan_reasons: SCAN_REASONS,
        consecutive_failures: sub?.consecutiveFailures ?? 0,
        broken: (sub?.consecutiveFailures ?? 0) >= this.config.brokenAfter,
        last_error: sub?.lastError ?? null,
        blind_spots: sub?.blindSpots ?? [],
      });
    }
    bindings.sort((a, b) => a.participant.localeCompare(b.participant));
    return {
      supervisor_version: SUPERVISOR_VERSION,
      pid: process.pid,
      started_at: this.startedAt,
      herdr_version: this.versions.herdr,
      post_version: this.versions.post,
      herdr_ok: this.herdrOk,
      post_ok: this.postOk,
      last_discovery_at: this.lastDiscoveryAt === null ? null : new Date(this.lastDiscoveryAt).toISOString(),
      concurrency: this.config.concurrency,
      // Cumulative since start: snapshots taken, split by what caused them.
      stats: { ...this.stats },
      bindings,
      retired_recent: this.retired,
    };
  }

  writeHealth(force = false) {
    const snapshot = this.healthSnapshot();
    const json = JSON.stringify(snapshot);
    if (!force && json === this.lastHealthJson && this.now() - this.lastHealthWriteAt < this.config.healthMaxAgeMs) return;
    this.lastHealthJson = json;
    this.lastHealthWriteAt = this.now();
    writeFileAtomic(this.paths.healthFile, `${JSON.stringify({ ...snapshot, updated_at: new Date(this.now()).toISOString() }, null, 2)}\n`);
  }

  writeHeartbeat(extra = {}) {
    this.seq += 1;
    writeFileAtomic(
      this.paths.heartbeatFile,
      `${JSON.stringify({ pid: process.pid, started_at: this.startedAt, seq: this.seq, time: new Date(this.now()).toISOString(), ...extra })}\n`
    );
  }

  // Retired state is pruned after 7 days; so is state no live subscription owns.
  pruneState() {
    this.lastPruneAt = this.now();
    let dirs;
    try {
      dirs = fs.readdirSync(this.paths.stateDir);
    } catch {
      return;
    }
    const live = new Set([...this.subs.values()].map((sub) => sub.stateFile));
    for (const dir of dirs) {
      const full = path.join(this.paths.stateDir, dir);
      let files;
      try {
        files = fs.readdirSync(full);
      } catch {
        continue;
      }
      for (const file of files) {
        const target = path.join(full, file);
        if (live.has(target)) continue;
        try {
          const age = this.now() - fs.statSync(target).mtimeMs;
          if (age > this.config.retiredPruneMs) fs.unlinkSync(target);
        } catch {
          // Pruning is housekeeping.
        }
      }
    }
  }

  // One supervisor tick: discovery, the reconciliation clock, health.
  async tick() {
    if (this.halted) return;
    await this.discover();
    if (this.halted) return;
    if (this.now() - this.lastReconcileAt >= this.config.reconcileMs) this.reconcile("timer");
    if (this.lastDiscoveryAt !== null && this.now() - this.lastPruneAt >= 3600_000) this.pruneState();
    this.pump();
  }
}

// ------------------------------------------------------------------ singleton

const LOCK_SCRIPT = `
import fcntl, os, sys
fd = os.open(sys.argv[1], os.O_RDWR | os.O_CREAT, 0o600)
try:
    fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
except BlockingIOError:
    try:
        owner = os.pread(fd, 32, 0).decode("ascii", "replace").strip()
    except Exception:
        owner = ""
    sys.stdout.write("BUSY " + owner + "\\n")
    sys.stdout.flush()
    sys.exit(3)
os.ftruncate(fd, 0)
os.pwrite(fd, (sys.argv[2] + "\\n").encode("ascii"), 0)
sys.stdout.write("LOCKED %d\\n" % os.getpid())
sys.stdout.flush()
try:
    while sys.stdin.buffer.read(65536):
        pass
except Exception:
    pass
`;

const PROBE_SCRIPT = `
import fcntl, os, sys
try:
    fd = os.open(sys.argv[1], os.O_RDONLY)
except FileNotFoundError:
    sys.exit(0)
try:
    fcntl.flock(fd, fcntl.LOCK_SH | fcntl.LOCK_NB)
except BlockingIOError:
    sys.exit(1)
sys.exit(0)
`;

function tryLockOnce(paths) {
  return new Promise((resolve) => {
    let child;
    try {
      child = spawn(paths.pythonBin, ["-c", LOCK_SCRIPT, paths.lockFile, String(process.pid)], {
        stdio: ["pipe", "pipe", "pipe"],
      });
    } catch (error) {
      resolve({ error: `cannot start ${paths.pythonBin}: ${error.message}` });
      return;
    }
    let buffer = "";
    let stderr = "";
    let settled = false;
    const settle = (value) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolve(value);
    };
    const timer = setTimeout(() => {
      child.kill("SIGKILL");
      settle({ error: "lock helper did not answer" });
    }, 5000);
    child.stdout.on("data", (chunk) => {
      buffer += chunk.toString("utf8");
      const newline = buffer.indexOf("\n");
      if (newline < 0) return;
      const line = buffer.slice(0, newline);
      const locked = /^LOCKED (\d+)$/.exec(line);
      if (locked) settle({ locked: true, child, helperPid: Number(locked[1]) });
      else if (line.startsWith("BUSY")) settle({ busy: true, owner: line.slice(4).trim() });
      else settle({ error: `lock helper said ${JSON.stringify(stderrExcerpt(line))}` });
    });
    child.stderr.on("data", (chunk) => {
      stderr += chunk.toString("utf8");
    });
    child.on("error", (error) => settle({ error: `cannot start ${paths.pythonBin}: ${error.code ?? error.message}` }));
    child.on("exit", (code) => settle({ error: `lock helper exited ${code}: ${stderrExcerpt(stderr)}` }));
  });
}

// Take the singleton lock or report who holds it. A short retry window lets a
// crashed predecessor's helper notice its closed stdin and exit, and lets a
// concurrent `status` probe finish. Nothing is written before LOCKED.
export async function acquireSingleton(paths, { retryMs = DEFAULTS.lockAcquireRetryMs } = {}) {
  fs.mkdirSync(paths.doorbell, { recursive: true, mode: 0o700 });
  const deadline = Date.now() + retryMs;
  for (;;) {
    const attempt = await tryLockOnce(paths);
    if (attempt.locked) return attempt;
    if (attempt.error) return attempt;
    if (Date.now() >= deadline) return attempt;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
}

// Liveness authority for `status`: is the lock held right now.
export function probeLock(paths) {
  if (!fs.existsSync(paths.lockFile)) return "free";
  const result = spawnSync(paths.pythonBin, ["-c", PROBE_SCRIPT, paths.lockFile], { timeout: 5000 });
  if (result.error) return "unknown";
  if (result.status === 1) return "held";
  if (result.status === 0) return "free";
  return "unknown";
}

// ------------------------------------------------------------------ run

async function runSupervisor(paths) {
  const lock = await acquireSingleton(paths);
  if (!lock.locked) {
    if (lock.busy) {
      const owner = /^\d+$/.test(lock.owner) ? lock.owner : "unknown";
      process.stderr.write(`post-doorbell: already running (pid ${owner})\n`);
      return 75;
    }
    process.stderr.write(`post-doorbell: could not take the singleton lock: ${lock.error}\n`);
    return 71;
  }
  const supervisor = new Supervisor({ paths });
  let stopping = false;
  let exitCode = 0;
  let loopTimer;
  const finish = (code) => {
    if (stopping) return;
    stopping = true;
    exitCode = code;
    clearTimeout(loopTimer);
    supervisor.halt(code === 0 ? "stopped" : "lock lost");
    try {
      lock.child.stdin.end();
    } catch {
      // Helper already gone.
    }
    // Exit now: nothing may ring after this point.
    process.exit(exitCode);
  };
  lock.child.on("exit", () => {
    if (stopping) return;
    process.stderr.write("post-doorbell: singleton lock helper died; stopping all delivery\n");
    finish(70);
  });
  lock.child.stdout.resume();
  lock.child.stderr.resume();
  process.on("SIGTERM", () => finish(0));
  process.on("SIGINT", () => finish(0));

  supervisor.loadConfig();
  supervisor.emit({ type: "start", pid: process.pid, lock_helper_pid: lock.helperPid, version: SUPERVISOR_VERSION });
  supervisor.writeHeartbeat({ lock_helper_pid: lock.helperPid });
  await supervisor.probeVersions();
  const loop = async () => {
    if (stopping) return;
    try {
      await supervisor.tick();
      supervisor.writeHeartbeat({ lock_helper_pid: lock.helperPid });
      supervisor.writeHealth();
    } catch (error) {
      supervisor.emit({ type: "tick", problem: "tick failed", error: String(error?.stack ?? error).slice(0, 500) });
    }
    loopTimer = setTimeout(loop, supervisor.config.discoveryMs);
  };
  await loop();
  return new Promise(() => {});
}

// ------------------------------------------------------------------ commands

function usageText() {
  return [
    "usage: post-doorbell run",
    "       post-doorbell enable [--focused] [--desktop]",
    "       post-doorbell disable",
    "       post-doorbell subscribe --channel <name> [--unsubscribe]",
    "       post-doorbell unsubscribe --channel <name>",
    "       post-doorbell select --pane <pane_id>",
    "       post-doorbell status [--json]",
  ].join("\n");
}

class UsageError extends Error {}
class CommandError extends Error {}

function parseCommandArgs(argv) {
  const opts = { command: null, focused: false, desktop: false, unsubscribe: false, channels: [], pane: null, json: false };
  for (let index = 0; index < argv.length; index++) {
    const arg = argv[index];
    const value = () => {
      const next = argv[index + 1];
      if (next === undefined || next.startsWith("-")) throw new UsageError(`${arg} requires a value`);
      index += 1;
      return next;
    };
    if (arg === "--focused") opts.focused = true;
    else if (arg === "--desktop") opts.desktop = true;
    else if (arg === "--unsubscribe") opts.unsubscribe = true;
    else if (arg === "--json") opts.json = true;
    else if (arg === "--channel") opts.channels.push(value());
    else if (arg === "--pane") opts.pane = value();
    else if (!arg.startsWith("-") && opts.command === null) opts.command = arg;
    else throw new UsageError(`unknown argument: ${arg}`);
  }
  if (opts.command === null && opts.unsubscribe) opts.command = "subscribe";
  if (opts.command === "unsubscribe") {
    opts.command = "subscribe";
    opts.unsubscribe = true;
  }
  const allowed = {
    run: [],
    enable: ["focused", "desktop"],
    disable: [],
    subscribe: ["channels", "unsubscribe"],
    select: ["pane"],
    status: ["json"],
  };
  if (!(opts.command in allowed)) throw new UsageError(opts.command ? `unknown command: ${opts.command}` : "missing command");
  const used = [];
  if (opts.focused) used.push("focused");
  if (opts.desktop) used.push("desktop");
  if (opts.unsubscribe) used.push("unsubscribe");
  if (opts.channels.length) used.push("channels");
  if (opts.pane !== null) used.push("pane");
  if (opts.json) used.push("json");
  for (const flag of used) {
    if (!allowed[opts.command].includes(flag)) throw new UsageError(`--${flag === "channels" ? "channel" : flag} is not valid with ${opts.command}`);
  }
  if (opts.command === "subscribe" && opts.channels.length === 0) throw new UsageError("subscribe requires --channel <name>");
  for (const channel of opts.channels) {
    if (!validChannelName(channel)) throw new UsageError(`invalid channel name: ${JSON.stringify(channel)}`);
  }
  if (opts.command === "select") {
    if (opts.pane === null) throw new UsageError("select requires --pane <pane_id>");
    if (!PANE_ID.test(opts.pane)) throw new UsageError(`invalid pane id: ${JSON.stringify(opts.pane)}`);
  }
  return opts;
}

// The acting participant, resolved the way every post command resolves it.
function resolveActor(paths) {
  const env = { ...process.env, POST_MAIL_ROOT: paths.root };
  const result = spawnSync(paths.postBin, ["participant", "show", "--json"], { encoding: "utf8", env, timeout: 10_000 });
  if (result.error || result.status !== 0) {
    throw new CommandError(`\`post participant show --json\` failed: ${stderrExcerpt(result.stderr) || result.error?.message || `exit ${result.status}`}`);
  }
  let parsed;
  try {
    parsed = JSON.parse(result.stdout);
  } catch {
    throw new CommandError("`post participant show --json` printed malformed output");
  }
  if (parsed?.ok !== true || parsed.status !== "bound" || typeof parsed.id !== "string" || parsed.participant?.id !== parsed.id) {
    throw new CommandError("no bound post participant here; run `post participant bind` first, or set POST_PARTICIPANT");
  }
  if (!validStateId(parsed.id)) throw new CommandError(`participant id ${JSON.stringify(parsed.id)} is outside the doorbell alphabet`);
  if (parsed.participant.ended_at) throw new CommandError(`participant ${parsed.id} has ended; bind again first`);
  return parsed.participant;
}

export function liveness(paths, now = Date.now()) {
  const lock = probeLock(paths);
  const heartbeat = readJson(paths.heartbeatFile);
  const time = heartbeat?.time ? Date.parse(heartbeat.time) : NaN;
  const age = Number.isFinite(time) ? Math.max(0, now - time) : null;
  let state;
  if (lock === "held") state = age !== null && age <= DEFAULTS.staleHeartbeatMs ? "running" : "stale";
  else if (lock === "free") state = "dead";
  else state = "unknown";
  return { state, lock, heartbeat_age_ms: age, pid: heartbeat?.pid ?? null };
}

function statusReport(paths) {
  const live = liveness(paths);
  const health = readJson(paths.healthFile) ?? null;
  return { ok: true, liveness: live, health_current: live.state === "running", health };
}

function printStatus(report) {
  const lines = [];
  const live = report.liveness;
  const age = live.heartbeat_age_ms === null ? "no heartbeat" : `heartbeat ${Math.round(live.heartbeat_age_ms / 1000)}s ago`;
  lines.push(`supervisor: ${live.state} (lock ${live.lock}, ${age}${live.pid ? `, pid ${live.pid}` : ""})`);
  const health = report.health;
  if (!health) {
    lines.push("health: none recorded");
  } else {
    if (!report.health_current) lines.push(`health below is from a supervisor that is ${live.state}; it is not current`);
    lines.push(`herdr: ${health.herdr_ok === false ? "FAILING" : "ok"} ${health.herdr_version ?? ""}; post: ${health.post_ok === false ? "FAILING" : "ok"} ${health.post_version ?? ""}`);
    if (!health.bindings?.length) lines.push("bindings: none on this host");
    for (const binding of health.bindings ?? []) {
      const armed = binding.armed ? "armed" : binding.state === "ambiguous" ? "unarmed (ambiguous: run post-doorbell select --pane <id>)" : binding.state === "ended" ? "unarmed (ended)" : "unarmed";
      let line = `  ${binding.participant} ${binding.panes.join(",")} ${armed}`;
      if (binding.pane_status) line += ` pane ${binding.pane_status}`;
      if (binding.last_outcome) line += `; last ${binding.last_outcome} at ${binding.last_outcome_at}`;
      if (binding.broken) line += `; BROKEN after ${binding.consecutive_failures} failures`;
      else if (binding.consecutive_failures) line += `; ${binding.consecutive_failures} failure(s)`;
      if (binding.last_error) line += ` (${binding.last_error.stage})`;
      if (binding.channels?.length) line += `; channels ${binding.channels.map((c) => `#${c}`).join(" ")}`;
      for (const spot of binding.blind_spots ?? []) line += `; blind spot: ${spot.count} unreadable in ${spot.where}, mentions there unknown`;
      lines.push(line);
    }
    for (const retired of health.retired_recent ?? []) lines.push(`  retired ${retired.participant} ${retired.pane} (${retired.reason}) at ${retired.at}`);
  }
  process.stdout.write(`${lines.join("\n")}\n`);
}

function runCommandLine(opts, paths) {
  if (opts.command === "status") {
    const report = statusReport(paths);
    if (opts.json) process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);
    else printStatus(report);
    return 0;
  }
  const actor = resolveActor(paths);
  const id = actor.id;
  let prefs;
  if (opts.command === "enable") {
    prefs = updatePrefs(paths, id, (p) => {
      p.enabled = true;
      p.focused = opts.focused;
      p.desktop = opts.desktop;
    });
  } else if (opts.command === "disable") {
    prefs = updatePrefs(paths, id, (p) => {
      p.enabled = false;
    });
  } else if (opts.command === "subscribe") {
    prefs = updatePrefs(paths, id, (p) => {
      const set = new Set(p.channels);
      for (const channel of opts.channels) {
        if (opts.unsubscribe) set.delete(channel);
        else set.add(channel);
      }
      p.channels = [...set].sort();
    });
  } else if (opts.command === "select") {
    const env = { ...process.env };
    const result = spawnSync(paths.herdrBin, ["agent", "get", opts.pane], { encoding: "utf8", env, timeout: 10_000 });
    if (result.error || result.status !== 0) {
      throw new CommandError(`\`herdr agent get ${opts.pane}\` failed: ${stderrExcerpt(result.stderr) || result.error?.message || `exit ${result.status}`}`);
    }
    let agent;
    try {
      agent = JSON.parse(result.stdout)?.result?.agent;
    } catch {
      agent = undefined;
    }
    const session = agent?.agent_session;
    const digest = session?.kind === "id" && typeof session.value === "string" ? sha256(session.value) : null;
    if (agent?.pane_id !== opts.pane || digest === null) {
      throw new CommandError(`pane ${opts.pane} carries no conversation herdr can identify; not selected`);
    }
    if (digest !== actor.conversation_key_digest) {
      throw new CommandError(`pane ${opts.pane} carries a different conversation than participant ${id}; not selected`);
    }
    prefs = updatePrefs(paths, id, (p) => {
      p.selection = { pane: opts.pane, digest };
    });
  }
  const live = liveness(paths);
  const summary = {
    enable: `enabled for ${id}${prefs.focused ? " (also while focused)" : ""}${prefs.desktop ? " with desktop notifications" : ""}`,
    disable: `disabled for ${id}`,
    subscribe: `channels for ${id}: ${prefs.channels.length ? prefs.channels.map((c) => `#${c}`).join(" ") : "none"} (direct mail and mentions always ring)`,
    select: `selected pane ${opts.pane} for ${id}`,
  }[opts.command];
  process.stdout.write(`post-doorbell: ${summary}; prefs version ${prefs.version}. Supervisor ${live.state}.\n`);
  if (live.state !== "running") {
    process.stdout.write("post-doorbell: no running supervisor picks this up until one is installed or restarted.\n");
  }
  return 0;
}

export async function main(argv = process.argv.slice(2)) {
  if (argv.includes("-h") || argv.includes("--help")) {
    process.stdout.write(`${usageText()}\n`);
    return 0;
  }
  let opts;
  try {
    opts = parseCommandArgs(argv);
  } catch (error) {
    if (error instanceof UsageError) {
      process.stderr.write(`post-doorbell: ${error.message}\n${usageText()}\n`);
      return 2;
    }
    throw error;
  }
  const paths = resolvePaths();
  if (!path.isAbsolute(paths.root)) {
    process.stderr.write(`post-doorbell: POST_MAIL_ROOT must be absolute; got ${JSON.stringify(paths.root)}\n`);
    return 2;
  }
  if (opts.command === "run") return runSupervisor(paths);
  try {
    return runCommandLine(opts, paths);
  } catch (error) {
    if (error instanceof CommandError) {
      process.stderr.write(`post-doorbell: ${error.message}\n`);
      return 1;
    }
    throw error;
  }
}

const invokedDirectly = (() => {
  try {
    return process.argv[1] && fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url));
  } catch {
    return false;
  }
})();

if (invokedDirectly) {
  main().then(
    (code) => {
      if (typeof code === "number") process.exitCode = code;
    },
    (error) => {
      process.stderr.write(`post-doorbell: ${error?.stack ?? error}\n`);
      process.exitCode = 1;
    }
  );
}
