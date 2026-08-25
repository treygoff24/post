"""A doorbell whose watcher dies must say so and exit non-zero.

Silence is the failure mode this whole daemon exists to prevent: if `post watch`
dies, the daemon keeps running and looks healthy while ringing for nothing. The
non-zero exit is what makes systemd's Restart= clause fire.
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

# The watcher dies immediately with a distinctive code, as it would if `post`
# were upgraded out from under a running daemon or the store went away.
FAKE_POST = """#!/bin/sh
for a in "$@"; do [ "$a" = "--snapshot" ] && exit 0; done
exit 9
"""


class AWatcherThatDiesIsReported(unittest.TestCase):
    def test_watch_exit_is_logged_and_the_daemon_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            binhome = Path(tmp) / "bin"
            binhome.mkdir()
            for name, body in (("herdr", FAKE_HERDR), ("post", FAKE_POST)):
                path = binhome / name
                path.write_text(body)
                path.chmod(0o755)

            env = dict(os.environ)
            env["PATH"] = f"{binhome}:{env['PATH']}"
            env["XDG_STATE_HOME"] = str(Path(tmp) / "state")
            env["FAKE_CWD"] = tmp

            proc = subprocess.run(
                [sys.executable, str(SCRIPT), "--agent", "fake"],
                env=env, capture_output=True, text=True, timeout=30,
            )

            self.assertNotEqual(proc.returncode, 0,
                                "a dead watcher must exit non-zero so the supervisor restarts it")
            self.assertIn("`post watch` exited", proc.stderr,
                          "the journal must say why the doorbell stopped ringing")
            self.assertIn("rc=9", proc.stderr,
                          "the watcher's own exit code is the diagnostic; it must survive to the log")


if __name__ == "__main__":
    unittest.main(verbosity=2)
