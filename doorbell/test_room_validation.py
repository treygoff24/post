"""A doorbell pointed at a room `post` does not know must refuse to start.

`post watch --room typo` warns once and then watches an empty mailbox forever.
Inside a daemon that warning is a journal line nobody reads, and the doorbell is
silently deaf -- the exact failure this program exists to delete.
"""

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("post-doorbell")

FAKE_HERDR = """#!/bin/sh
case "$1 $2" in
  "agent list") echo '{"result":{"agents":[{"name":"fake","cwd":"'"$FAKE_CWD"'","agent_status":"idle"}]}}' ;;
  *) exit 0 ;;
esac
"""

FAKE_POST_KNOWN = """#!/bin/sh
case "$1" in
  rooms) echo '{"ok":true,"rooms":[{"name":"alpha"},{"name":"beta"}]}' ;;
  watch) for a in "$@"; do [ "$a" = "--snapshot" ] && exit 0; done; exec sleep 30 ;;
esac
"""

# A listing that cannot be parsed must NOT read as "no rooms exist".
FAKE_POST_BROKEN = """#!/bin/sh
case "$1" in
  rooms) echo 'not json at all' ;;
  watch) for a in "$@"; do [ "$a" = "--snapshot" ] && exit 0; done; exec sleep 30 ;;
esac
"""


def run_doorbell(tmp, post_body, rooms, timeout=20):
    binhome = Path(tmp) / "bin"
    binhome.mkdir(exist_ok=True)
    for name, body in (("herdr", FAKE_HERDR), ("post", post_body)):
        path = binhome / name
        path.write_text(body)
        path.chmod(0o755)
    env = dict(os.environ)
    env["PATH"] = f"{binhome}:{env['PATH']}"
    env["XDG_STATE_HOME"] = str(Path(tmp) / "state")
    env["FAKE_CWD"] = tmp
    argv = [sys.executable, str(SCRIPT), "--agent", "fake"]
    for room in rooms:
        argv += ["--room", room]
    return subprocess.run(argv, env=env, capture_output=True, text=True, timeout=timeout)


class AnUnknownRoomIsRefused(unittest.TestCase):
    def test_an_unregistered_room_stops_the_daemon_before_it_watches_nothing(self):
        with tempfile.TemporaryDirectory() as tmp:
            proc = run_doorbell(tmp, FAKE_POST_KNOWN, ["alpha", "typo"])
            self.assertEqual(proc.returncode, 2,
                             "an unknown room must stop startup, not warn and continue")
            # Name the offender exactly: a message that just says "some room is
            # wrong" leaves the operator diffing their own command line.
            self.assertIn("unregistered room(s) ['typo']", proc.stderr)

    def test_an_unreadable_listing_is_unchecked_not_proof_of_absence(self):
        with tempfile.TemporaryDirectory() as tmp:
            try:
                proc = run_doorbell(tmp, FAKE_POST_BROKEN, ["alpha"], timeout=8)
                started = "without room validation" in proc.stderr
            except subprocess.TimeoutExpired as exc:
                started = "without room validation" in (exc.stderr or b"").decode()
            self.assertTrue(started,
                            "an unreadable rooms listing must degrade to unchecked, never to refusal")


if __name__ == "__main__":
    unittest.main(verbosity=2)
