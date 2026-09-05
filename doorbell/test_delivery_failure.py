"""Integration test for the failure Pact flagged: a wake that cannot be delivered
must not retire the message. Uses fake `post` and `herdr` on PATH so the daemon
runs its real loop against a delivery that always fails.
"""

import json
import os
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("post-doorbell")

FAKE_HERDR = """#!/bin/sh
# list: one agent so startup succeeds. prompt: always fail, as a blocked agent does.
case "$1 $2" in
  "agent list") echo '{"result":{"agents":[{"name":"fake","cwd":"'"$FAKE_CWD"'","agent_status":"blocked"}]}}' ;;
  "agent prompt") echo "agent_blocked" >&2; exit 1 ;;
  "agent wait") exit 0 ;;
  *) exit 0 ;;
esac
"""

FAKE_POST = """#!/bin/sh
# snapshot: nothing unread at startup, so the watermark primes empty.
# watch: re-offer the same unread digest forever, which is what real `post watch`
# does while a message stays unread.
for a in "$@"; do
  if [ "$a" = "--snapshot" ]; then
    [ -f "$FAKE_CWD/started" ] || exit 0
    echo '{"event":"digest","source":"mail","count":1,"last_id":"20260825-170000-aaaaaa","reason":"mail"}'
    exit 0
  fi
done
touch "$FAKE_CWD/started"
while true; do
  echo '{"event":"digest","room":"r","source":"mail","count":1,"first_id":"20260825-170000-aaaaaa","last_id":"20260825-170000-aaaaaa","reason":"mail"}'
  sleep 1
done
"""


class FailedDeliveryDoesNotRetireTheMessage(unittest.TestCase):
    def test_watermark_is_not_persisted_when_the_wake_cannot_be_delivered(self):
        with tempfile.TemporaryDirectory() as tmp:
            binhome = Path(tmp) / "bin"
            binhome.mkdir()
            for name, body in (("herdr", FAKE_HERDR), ("post", FAKE_POST)):
                path = binhome / name
                path.write_text(body)
                path.chmod(0o755)

            state = Path(tmp) / "state"
            env = dict(os.environ)
            env["PATH"] = f"{binhome}:{env['PATH']}"
            env["XDG_STATE_HOME"] = str(state)
            env["FAKE_CWD"] = tmp

            proc = subprocess.Popen(
                [sys.executable, str(SCRIPT), "--agent", "fake", "--quiet-ms", "1000"],
                env=env, stderr=subprocess.PIPE, text=True,
            )
            time.sleep(7)
            proc.terminate()
            _, err = proc.communicate(timeout=10)

            self.assertIn("wake not delivered", err,
                          "a rejected prompt must be reported, not swallowed")
            marks_file = state / "post-doorbell" / "fake.json"
            marks = json.loads(marks_file.read_text()) if marks_file.exists() else {}
            self.assertNotEqual(marks.get("mail"), "20260825-170000-aaaaaa",
                                "an undelivered wake must not advance the watermark")
            # And it must keep trying: more than one failure in the window.
            self.assertGreaterEqual(err.count("wake not delivered"), 2,
                                    "the doorbell must re-attempt a wake it could not deliver")


if __name__ == "__main__":
    unittest.main(verbosity=2)
