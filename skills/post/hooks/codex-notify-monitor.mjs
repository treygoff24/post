#!/usr/bin/env node
// One launchd tick: snapshot configured rooms, ring cmux once for fresh direct
// mail, persist ids, exit. This never reads bodies, consumes mail, advances a
// channel cursor, keeps a watch child alive, or injects terminal input.

import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";

const ROOM_NAME = /^[A-Za-z0-9._-]+$/;
const MAIL_ID = /^\d{8}-\d{6}-[0-9a-fA-F]{6}$/;
const SEEN_CAP = 2000; // ponytail: enough unread ids for one user; bounds retry state

function executable(override, preferred, fallback) {
  if (override) return override;
  try {
    fs.accessSync(preferred, fs.constants.X_OK);
    return preferred;
  } catch {
    return fallback;
  }
}

function readSeen(file) {
  try {
    const parsed = JSON.parse(fs.readFileSync(file, "utf8"));
    return Array.isArray(parsed.seen)
      ? parsed.seen.filter((value) => typeof value === "string")
      : [];
  } catch {
    return [];
  }
}

function writeSeen(file, seen) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const tmp = `${file}.${process.pid}.tmp`;
  fs.writeFileSync(tmp, JSON.stringify({ seen: seen.slice(-SEEN_CAP) }));
  fs.renameSync(tmp, file);
}

function fail(message) {
  process.stderr.write(`post-notify: ${message}\n`);
  process.exitCode = 1;
}

const rooms = [...new Set(process.argv.slice(2))];
if (rooms.length === 0 || rooms.some((room) => !ROOM_NAME.test(room))) {
  fail("pass one or more valid room names");
} else {
  const post = executable(
    process.env.POST_CODEX_NOTIFY_POST_BIN,
    path.join(os.homedir(), ".local", "bin", "post"),
    "post"
  );
  const cmux = executable(
    process.env.POST_CODEX_NOTIFY_CMUX_BIN,
    "/Applications/cmux.app/Contents/Resources/bin/cmux",
    "cmux"
  );
  const stateFile =
    process.env.POST_CODEX_NOTIFY_STATE ||
    path.join(os.homedir(), ".local", "state", "codex-post-notify", "seen.json");
  const args = ["watch"];
  for (const room of rooms) args.push("--room", room);
  args.push("--snapshot");
  const snapshot = spawnSync(post, args, {
    encoding: "utf8",
    timeout: 4000,
    stdio: ["ignore", "pipe", "ignore"],
  });

  if (snapshot.error || snapshot.status !== 0) {
    fail("snapshot failed; notification state is unknown");
  } else {
    const allowedRooms = new Set(rooms);
    const mail = [];
    let malformed = false;
    for (const line of String(snapshot.stdout ?? "").split("\n")) {
      if (!line.trim()) continue;
      try {
        const event = JSON.parse(line);
        if (event?.event === "mail") {
          if (
            typeof event.room !== "string" ||
            !allowedRooms.has(event.room) ||
            typeof event.id !== "string" ||
            !MAIL_ID.test(event.id)
          ) {
            malformed = true;
          } else {
            mail.push(event);
          }
        } else if (!["channel_message", "unreadable"].includes(event?.event)) {
          malformed = true;
        }
      } catch {
        malformed = true;
      }
    }

    if (malformed) {
      fail("snapshot output was malformed; notification state is unknown");
    } else {
      const seen = new Set(readSeen(stateFile));
      const fresh = [];
      for (const event of mail) {
        const key = `${event.room}:${event.id}`;
        if (!seen.has(key)) fresh.push(event);
        seen.add(key);
      }
      if (fresh.length > 0) {
        const count = fresh.length;
        const notification = spawnSync(
          cmux,
          [
            "notify",
            "--title",
            "Post for Codex",
            "--body",
            `${count} direct message${count === 1 ? " is" : "s are"} waiting.`,
          ],
          { encoding: "utf8", timeout: 4000, stdio: "ignore" }
        );
        if (notification.error || notification.status !== 0) {
          fail("cmux notification failed; mail remains eligible for a later ring");
        } else {
          try {
            writeSeen(stateFile, [...seen]);
          } catch {
            fail("could not save dedupe state; mail may ring again");
          }
        }
      }
    }
  }
}
