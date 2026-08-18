#!/usr/bin/env node
// Install or remove a launchd "doorbell" job that wakes one explicitly named
// background Herdr agent about fresh direct post mail.
//
//   node install-codex-doorbell.mjs --room <room> --agent <unique-herdr-name> [--channel <name>]... [--interval-seconds <n>]
//   node install-codex-doorbell.mjs --uninstall --agent <unique-herdr-name>
//
// macOS only. Install copies the adjacent codex-notify-monitor.mjs to
// ~/.codex/hooks/post-codex-notify-monitor.mjs (0755), writes a per-agent
// LaunchAgent (dev.post.codex-doorbell.<agent>, configurable StartInterval,
// ProcessType Background) under ~/Library/LaunchAgents/, and loads it with
// launchctl. The job snapshots the exact room and wakes the exact Herdr agent
// only when it is unfocused and idle/done (see codex-notify-monitor.mjs).
// When POST_MAIL_ROOT is set at install time it must be absolute and is
// pinned verbatim into the plist EnvironmentVariables, so launchd runs the
// monitor against the same mail root the preflight saw; when unset the key
// is omitted and the monitor's default root applies.
// Uninstall boots the exact label out, then removes only that agent's plist,
// state file, and logs; the shared monitor copy is left in place.
//
// Everything is resolved and preflighted before anything is written: node is
// process.execPath; post, herdr, and launchctl come from the overrides below,
// else ~/.local/bin/post and ~/.local/bin/herdr, else PATH, and launchctl
// defaults to /bin/launchctl. A missing binary, a `post rooms` listing that
// lacks the exact requested room (or returns ok!==true / malformed members),
// a selected channel that is missing or does not include the room, a failed
// `post watch --room <room> --snapshot` probe, or a `herdr agent get <agent>`
// that does not return the requested agent aborts the install with nothing
// written. The probe commands are non-consuming: rooms/channels/snapshots
// never read bodies or advance cursors, and agent lookups change nothing.
// Note: `post watch --snapshot` exits 0 for an unregistered room, so
// registration is checked via `post rooms` first. `post channels` runs only
// when at least one --channel is selected.
//
// Test overrides (all optional):
//   POST_CODEX_DOORBELL_INSTALL_DIR    monitor install dir (default ~/.codex/hooks)
//   POST_CODEX_DOORBELL_POST_BIN       post executable
//   POST_CODEX_DOORBELL_HERDR_BIN      herdr executable
//   POST_CODEX_DOORBELL_LAUNCHCTL_BIN  launchctl executable
//   POST_CODEX_DOORBELL_HOME           replaces every "~" path derivation
//   POST_CODEX_DOORBELL_PLATFORM       test-only platform override (default process.platform)

import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { randomBytes } from "node:crypto";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const ROOM_NAME = /^[A-Za-z0-9._-]+$/;
const AGENT_NAME = /^[a-z][a-z0-9_-]{0,31}$/;
const NAME_MAX = 255;
// launchctl bootout failure modes that mean "nothing was loaded" — the only
// ones worth ignoring (a fresh install has no prior job).
const NOT_LOADED = /no such process|could not find service|not loaded|not-found/i;

function fail(message) {
  console.error(`install-codex-doorbell: ${message}`);
  process.exit(1);
}

function usageError(message) {
  console.error(
    `install-codex-doorbell: ${message}\n` +
      "usage: node install-codex-doorbell.mjs --room <room> --agent <unique-herdr-name> [--channel <name>]...\n" +
      "       [--interval-seconds <positive-integer>]\n" +
      "       node install-codex-doorbell.mjs --uninstall --agent <unique-herdr-name>"
  );
  process.exit(2);
}

function homeDir() {
  return process.env.POST_CODEX_DOORBELL_HOME || os.homedir();
}

function platform() {
  // Test-only override so hermetic stub suites can run on Linux CI.
  return process.env.POST_CODEX_DOORBELL_PLATFORM || process.platform;
}

function isExecutable(file) {
  try {
    fs.accessSync(file, fs.constants.X_OK);
    return true;
  } catch {
    return false;
  }
}

function resolveBin(override, preferred, name) {
  if (override) {
    if (!isExecutable(override)) {
      fail(`cannot execute the ${name} binary at ${override}; fix it or unset the override`);
    }
    return override;
  }
  if (isExecutable(preferred)) return preferred;
  for (const dir of String(process.env.PATH ?? "").split(":")) {
    if (!dir) continue;
    const candidate = path.join(dir, name);
    if (isExecutable(candidate)) return candidate;
  }
  fail(`could not find the ${name} executable; install it or point an override at it`);
}

function safeName(value) {
  return typeof value === "string" && value.length <= NAME_MAX && ROOM_NAME.test(value);
}

function parseArgs(argv) {
  const opts = {
    uninstall: false,
    room: null,
    agent: null,
    channels: [],
    intervalSeconds: null,
  };
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === "--uninstall") {
      if (opts.uninstall) usageError("duplicate --uninstall flag");
      opts.uninstall = true;
    } else if (arg === "--room") {
      if (opts.room !== null) usageError("duplicate --room flag");
      const value = argv[i + 1];
      if (value === undefined || value.startsWith("-")) usageError("--room requires a value");
      opts.room = value;
      i += 1;
    } else if (arg === "--agent") {
      if (opts.agent !== null) usageError("duplicate --agent flag");
      const value = argv[i + 1];
      if (value === undefined || value.startsWith("-")) usageError("--agent requires a value");
      opts.agent = value;
      i += 1;
    } else if (arg === "--channel") {
      const value = argv[i + 1];
      if (value === undefined || value.startsWith("-")) usageError("--channel requires a value");
      opts.channels.push(value);
      i += 1;
    } else if (arg === "--interval-seconds") {
      if (opts.intervalSeconds !== null) usageError("duplicate --interval-seconds flag");
      const value = argv[i + 1];
      if (value === undefined || value.startsWith("-")) {
        usageError("--interval-seconds requires a value");
      }
      if (!/^[1-9]\d*$/.test(value) || !Number.isSafeInteger(Number(value))) {
        usageError("--interval-seconds must be a positive integer");
      }
      opts.intervalSeconds = Number(value);
      i += 1;
    } else {
      usageError(`unknown argument: ${arg}`);
    }
  }
  return opts;
}

function validate(opts) {
  if (opts.uninstall) {
    if (opts.room !== null) usageError("--room is not valid with --uninstall");
    if (opts.channels.length > 0) usageError("--channel is not valid with --uninstall");
    if (opts.intervalSeconds !== null) {
      usageError("--interval-seconds is not valid with --uninstall");
    }
    if (opts.agent === null) usageError("--uninstall requires --agent <unique-herdr-name>");
  } else if (opts.room === null || opts.agent === null) {
    usageError("install requires --room <room> and --agent <unique-herdr-name>");
  }
  if (opts.intervalSeconds === null) opts.intervalSeconds = 5;
  if (opts.room !== null && !safeName(opts.room)) {
    usageError(`invalid room name: ${opts.room}`);
  }
  if (!AGENT_NAME.test(opts.agent)) {
    usageError(`invalid Herdr agent name: ${opts.agent}`);
  }
  const seen = new Set();
  const channels = [];
  for (const channel of opts.channels) {
    if (!safeName(channel)) usageError(`invalid channel name: ${channel}`);
    if (seen.has(channel)) continue;
    seen.add(channel);
    channels.push(channel);
  }
  opts.channels = channels;
}

function derivedPaths(agent) {
  const home = homeDir();
  return {
    home,
    monitorDir:
      process.env.POST_CODEX_DOORBELL_INSTALL_DIR || path.join(home, ".codex", "hooks"),
    plist: path.join(home, "Library", "LaunchAgents", `dev.post.codex-doorbell.${agent}.plist`),
    state: path.join(home, ".local", "state", "post-codex-doorbell", `${agent}.json`),
    log: path.join(home, "Library", "Logs", `post-codex-doorbell-${agent}.log`),
    errorLog: path.join(home, "Library", "Logs", `post-codex-doorbell-${agent}.error.log`),
  };
}

function isRoomRow(entry) {
  return (
    entry &&
    typeof entry === "object" &&
    !Array.isArray(entry) &&
    typeof entry.name === "string"
  );
}

function isChannelRow(entry) {
  return (
    entry &&
    typeof entry === "object" &&
    !Array.isArray(entry) &&
    typeof entry.name === "string" &&
    Array.isArray(entry.members) &&
    entry.members.every((member) => typeof member === "string")
  );
}

function preflight(postBin, herdrBin, room, agent, channels) {
  const rooms = spawnSync(postBin, ["rooms"], {
    encoding: "utf8",
    timeout: 4000,
    stdio: ["ignore", "pipe", "pipe"],
  });
  if (rooms.error || rooms.status !== 0) {
    fail(
      `preflight failed: \`post rooms\` ` +
        (rooms.error ? rooms.error.message : `exited ${rooms.status}`) +
        "; fix the post install, then re-run"
    );
  }
  let roomsParsed;
  try {
    roomsParsed = JSON.parse(String(rooms.stdout ?? ""));
  } catch {
    fail("preflight failed: `post rooms` printed malformed output; fix the post install, then re-run");
  }
  if (
    roomsParsed === null ||
    typeof roomsParsed !== "object" ||
    Array.isArray(roomsParsed) ||
    roomsParsed.ok !== true ||
    !Array.isArray(roomsParsed.rooms)
  ) {
    fail("preflight failed: `post rooms` printed an unexpected schema; fix the post install, then re-run");
  }
  if (!roomsParsed.rooms.every(isRoomRow)) {
    fail("preflight failed: `post rooms` printed malformed room entries; fix the post install, then re-run");
  }
  const registered = roomsParsed.rooms.some((entry) => entry.name === room);
  if (!registered) {
    fail(
      `preflight failed: room '${room}' is not registered in \`post rooms\`; ` +
        `register it first, then re-run`
    );
  }

  if (channels.length > 0) {
    const listed = spawnSync(postBin, ["channels"], {
      encoding: "utf8",
      timeout: 4000,
      stdio: ["ignore", "pipe", "pipe"],
    });
    if (listed.error || listed.status !== 0) {
      fail(
        `preflight failed: \`post channels\` ` +
          (listed.error ? listed.error.message : `exited ${listed.status}`) +
          "; fix the post install, then re-run"
      );
    }
    let channelsParsed;
    try {
      channelsParsed = JSON.parse(String(listed.stdout ?? ""));
    } catch {
      fail(
        "preflight failed: `post channels` printed malformed output; fix the post install, then re-run"
      );
    }
    if (
      channelsParsed === null ||
      typeof channelsParsed !== "object" ||
      Array.isArray(channelsParsed) ||
      channelsParsed.ok !== true ||
      !Array.isArray(channelsParsed.channels)
    ) {
      fail(
        "preflight failed: `post channels` printed an unexpected schema; fix the post install, then re-run"
      );
    }
    if (!channelsParsed.channels.every(isChannelRow)) {
      fail(
        "preflight failed: `post channels` printed malformed channel entries; fix the post install, then re-run"
      );
    }
    for (const channel of channels) {
      const entry = channelsParsed.channels.find((row) => row.name === channel);
      if (!entry) {
        fail(
          `preflight failed: channel '${channel}' is not listed in \`post channels\`; create/join it first, then re-run`
        );
      }
      if (!entry.members.includes(room)) {
        fail(
          `preflight failed: room '${room}' is not a member of channel '${channel}'; join it first, then re-run`
        );
      }
    }
  }

  const snapshot = spawnSync(postBin, ["watch", "--room", room, "--snapshot"], {
    encoding: "utf8",
    timeout: 4000,
    stdio: ["ignore", "pipe", "pipe"],
  });
  if (snapshot.error || snapshot.status !== 0) {
    fail(
      `preflight failed: \`post watch --room ${room} --snapshot\` ` +
        (snapshot.error ? snapshot.error.message : `exited ${snapshot.status}`) +
        "; fix the post install, then re-run"
    );
  }
  for (const line of String(snapshot.stdout ?? "").split("\n")) {
    if (!line.trim()) continue;
    try {
      const event = JSON.parse(line);
      if (event === null || typeof event !== "object" || Array.isArray(event)) {
        throw new Error("not an object");
      }
    } catch {
      fail(`preflight failed: \`post watch --room ${room} --snapshot\` printed malformed output; fix the post install, then re-run`);
    }
  }

  const info = spawnSync(herdrBin, ["agent", "get", agent], {
    encoding: "utf8",
    timeout: 4000,
    stdio: ["ignore", "pipe", "pipe"],
  });
  if (info.error || info.status !== 0) {
    fail(
      `preflight failed: \`herdr agent get ${agent}\` ` +
        (info.error ? info.error.message : `exited ${info.status}`) +
        "; fix the herdr install, then re-run"
    );
  }
  let parsed;
  try {
    parsed = JSON.parse(String(info.stdout ?? ""));
  } catch {
    fail(`preflight failed: \`herdr agent get ${agent}\` printed malformed output; fix the herdr install, then re-run`);
  }
  if (parsed?.result?.agent?.name !== agent) {
    fail(
      `preflight failed: \`herdr agent get ${agent}\` returned agent ` +
        `${JSON.stringify(parsed?.result?.agent?.name ?? "(none)")}; expected ${agent}`
    );
  }
}

function escapeXml(value) {
  return String(value)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&apos;");
}

function plistContent({
  agent,
  room,
  channels,
  intervalSeconds,
  nodeBin,
  monitor,
  postBin,
  herdrBin,
  mailRoot,
  paths,
}) {
  const xml = escapeXml;
  const envLines = [
    `    <key>HOME</key><string>${xml(paths.home)}</string>`,
    `    <key>POST_CODEX_NOTIFY_POST_BIN</key><string>${xml(postBin)}</string>`,
    `    <key>POST_CODEX_NOTIFY_HERDR_BIN</key><string>${xml(herdrBin)}</string>`,
    `    <key>POST_CODEX_NOTIFY_HERDR_AGENT</key><string>${xml(agent)}</string>`,
    `    <key>POST_CODEX_NOTIFY_STATE</key><string>${xml(paths.state)}</string>`,
  ];
  if (mailRoot !== undefined) {
    envLines.push(`    <key>POST_MAIL_ROOT</key><string>${xml(mailRoot)}</string>`);
  }
  if (channels.length > 0) {
    envLines.push(
      `    <key>POST_CODEX_NOTIFY_CHANNELS</key><string>${xml(channels.join(","))}</string>`
    );
  }
  return [
    '<?xml version="1.0" encoding="UTF-8"?>',
    '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">',
    '<plist version="1.0">',
    "<dict>",
    "  <key>Label</key>",
    `  <string>${xml(`dev.post.codex-doorbell.${agent}`)}</string>`,
    "  <key>ProgramArguments</key>",
    "  <array>",
    `    <string>${xml(nodeBin)}</string>`,
    `    <string>${xml(monitor)}</string>`,
    `    <string>${xml(room)}</string>`,
    "  </array>",
    "  <key>EnvironmentVariables</key>",
    "  <dict>",
    ...envLines,
    "  </dict>",
    "  <key>StartInterval</key>",
    `  <integer>${intervalSeconds}</integer>`,
    "  <key>RunAtLoad</key>",
    "  <true/>",
    "  <key>ProcessType</key>",
    "  <string>Background</string>",
    "  <key>StandardOutPath</key>",
    `  <string>${xml(paths.log)}</string>`,
    "  <key>StandardErrorPath</key>",
    `  <string>${xml(paths.errorLog)}</string>`,
    "</dict>",
    "</plist>",
    "",
  ].join("\n");
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
function writeFileAtomic(file, content, mode) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  let writeMode = mode;
  if (writeMode === undefined) {
    writeMode = 0o600;
    try {
      writeMode = fs.statSync(file).mode & 0o777;
    } catch {
      // New file: restrictive default.
    }
  }
  const dir = path.dirname(file);
  const tmp = path.join(
    dir,
    `.${path.basename(file)}.${process.pid}.${randomBytes(8).toString("hex")}.tmp`
  );
  let fd;
  try {
    const flags =
      fs.constants.O_WRONLY |
      fs.constants.O_CREAT |
      fs.constants.O_EXCL |
      (fs.constants.O_NOFOLLOW || 0);
    fd = fs.openSync(tmp, flags, writeMode);
    writeAllSync(fd, content);
    fs.closeSync(fd);
    fd = undefined;
    fs.renameSync(tmp, file);
  } catch (error) {
    if (fd !== undefined) {
      try {
        fs.closeSync(fd);
      } catch {
        // Best-effort.
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

function runLaunchctl(bin, args) {
  return spawnSync(bin, args, {
    encoding: "utf8",
    timeout: 10000,
    stdio: ["ignore", "pipe", "pipe"],
  });
}

function launchctlDetail(result) {
  return (
    result.error?.message ||
    String(result.stderr ?? "").trim() ||
    `exit ${result.status}`
  );
}

// Boot the exact label out; only a not-loaded failure is ignorable (fresh
// install, or an already-removed job).
function bootout(bin, domain, label) {
  const result = runLaunchctl(bin, ["bootout", `${domain}/${label}`]);
  if (result.error || result.status !== 0) {
    if (!NOT_LOADED.test(String(result.stderr ?? ""))) {
      fail(`launchctl bootout failed: ${launchctlDetail(result)}`);
    }
  }
}

function install(opts) {
  const nodeBin = process.execPath;
  // Any PRESENT value counts as set, including empty: the real Post binary
  // treats an explicitly empty POST_MAIL_ROOT as a relative path (rc 78), so
  // empty must refuse here rather than silently pass as unset.
  const mailRoot = process.env.POST_MAIL_ROOT;
  if (mailRoot !== undefined && !path.isAbsolute(mailRoot)) {
    fail(
      `POST_MAIL_ROOT must be an absolute path when set; got ${JSON.stringify(mailRoot)}. ` +
        "Fix it or unset it, then re-run"
    );
  }
  const postBin = resolveBin(
    process.env.POST_CODEX_DOORBELL_POST_BIN,
    path.join(homeDir(), ".local", "bin", "post"),
    "post"
  );
  const herdrBin = resolveBin(
    process.env.POST_CODEX_DOORBELL_HERDR_BIN,
    path.join(homeDir(), ".local", "bin", "herdr"),
    "herdr"
  );
  const launchctlBin = resolveBin(
    process.env.POST_CODEX_DOORBELL_LAUNCHCTL_BIN,
    "/bin/launchctl",
    "launchctl"
  );
  const paths = derivedPaths(opts.agent);
  // Preflight registration, optional channel membership, the exact room
  // snapshot, and the exact Herdr target before any write.
  preflight(postBin, herdrBin, opts.room, opts.agent, opts.channels);

  const source = fs.readFileSync(
    path.join(path.dirname(fileURLToPath(import.meta.url)), "codex-notify-monitor.mjs")
  );
  const monitor = path.join(paths.monitorDir, "post-codex-notify-monitor.mjs");
  let monitorChanged = true;
  try {
    monitorChanged = !source.equals(fs.readFileSync(monitor));
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
  }
  if (monitorChanged) writeFileAtomic(monitor, source, 0o755);
  const monitorModeChanged = (fs.statSync(monitor).mode & 0o777) !== 0o755;
  if (monitorModeChanged) fs.chmodSync(monitor, 0o755);

  const content = plistContent({
    agent: opts.agent,
    room: opts.room,
    channels: opts.channels,
    intervalSeconds: opts.intervalSeconds,
    nodeBin,
    monitor,
    postBin,
    herdrBin,
    mailRoot,
    paths,
  });
  let plistChanged = true;
  try {
    plistChanged = fs.readFileSync(paths.plist, "utf8") !== content;
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
  }
  if (plistChanged) writeFileAtomic(paths.plist, content);

  const domain = `gui/${process.getuid()}`;
  const label = `dev.post.codex-doorbell.${opts.agent}`;
  bootout(launchctlBin, domain, label);
  const loaded = runLaunchctl(launchctlBin, ["bootstrap", domain, paths.plist]);
  if (loaded.error || loaded.status !== 0) {
    fail(`launchctl bootstrap failed: ${launchctlDetail(loaded)}`);
  }

  console.log(
    [
      "doorbell installed",
      monitorChanged && "monitor updated",
      plistChanged && "launch agent updated",
    ]
      .filter(Boolean)
      .join("\n")
  );
}

function uninstall(opts) {
  const launchctlBin = resolveBin(
    process.env.POST_CODEX_DOORBELL_LAUNCHCTL_BIN,
    "/bin/launchctl",
    "launchctl"
  );
  const paths = derivedPaths(opts.agent);
  const domain = `gui/${process.getuid()}`;
  bootout(launchctlBin, domain, `dev.post.codex-doorbell.${opts.agent}`);
  for (const file of [paths.plist, paths.state, paths.log, paths.errorLog]) {
    try {
      fs.unlinkSync(file);
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
    }
  }
  console.log("doorbell uninstalled; shared monitor copy left in place");
}

if (platform() !== "darwin") {
  fail("macOS only: launchd LaunchAgents are the supported scheduling surface");
}
const opts = parseArgs(process.argv.slice(2));
validate(opts);
if (opts.uninstall) uninstall(opts);
else install(opts);
