"""The doorbell must detect `watch --own` support, never assume it.

An older `post` rejects an unknown flag and the daemon dies on startup; the
installed post on this box was 0.6.0, which predates the flag, while the built
one has it. A doorbell that dies on a flag is worse than one that is noisy.
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

# Records the watch argv so the test can assert what was actually passed, and
# refuses --own when the fixture says this post predates it.
FAKE_POST = """#!/bin/sh
case "$1" in
  rooms) echo '{"ok":true,"rooms":[{"name":"alpha"}]}' ;;
  watch)
    case "$2" in
      --help) [ -n "$SUPPORTS_OWN" ] && echo "  --own <ROOM>  rooms this watcher is"; exit 0 ;;
    esac
    echo "$@" >> "$ARGV_LOG"
    for a in "$@"; do
      [ "$a" = "--snapshot" ] && exit 0
      if [ "$a" = "--own" ] && [ -z "$SUPPORTS_OWN" ]; then
        echo "error: unexpected argument '--own'" >&2; exit 2
      fi
    done
    exec sleep 30 ;;
esac
"""


def run(tmp, supports_own):
    binhome = Path(tmp) / "bin"
    binhome.mkdir(exist_ok=True)
    for name, body in (("herdr", FAKE_HERDR), ("post", FAKE_POST)):
        path = binhome / name
        path.write_text(body)
        path.chmod(0o755)
    log = Path(tmp) / "argv.log"
    env = dict(os.environ)
    env["PATH"] = f"{binhome}:{env['PATH']}"
    env["XDG_STATE_HOME"] = str(Path(tmp) / "state")
    env["FAKE_CWD"] = tmp
    env["ARGV_LOG"] = str(log)
    if supports_own:
        env["SUPPORTS_OWN"] = "1"
    else:
        env.pop("SUPPORTS_OWN", None)
    argv = [sys.executable, str(SCRIPT), "--agent", "fake", "--room", "alpha"]
    try:
        proc = subprocess.run(argv, env=env, capture_output=True, text=True, timeout=8)
        stderr = proc.stderr
    except subprocess.TimeoutExpired as exc:
        stderr = (exc.stderr or b"").decode()
    return stderr, log.read_text() if log.exists() else ""


class OwnSupportIsDetected(unittest.TestCase):
    def test_own_is_passed_when_post_supports_it(self):
        with tempfile.TemporaryDirectory() as tmp:
            stderr, argv = run(tmp, supports_own=True)
            watch_lines = [line for line in argv.splitlines() if "--snapshot" not in line]
            self.assertTrue(watch_lines, "the long watch must have been launched")
            self.assertIn("--own alpha", watch_lines[-1])
            self.assertNotIn("has no `watch --own`", stderr)

    def test_own_is_omitted_and_announced_when_post_lacks_it(self):
        with tempfile.TemporaryDirectory() as tmp:
            stderr, argv = run(tmp, supports_own=False)
            watch_lines = [line for line in argv.splitlines() if "--snapshot" not in line]
            self.assertTrue(watch_lines, "the long watch must have been launched")
            self.assertNotIn("--own", watch_lines[-1],
                             "passing --own to a post that lacks it kills the daemon")
            self.assertIn("has no `watch --own`", stderr,
                          "degrading silently would hide why the doorbell is noisy")


if __name__ == "__main__":
    unittest.main(verbosity=2)
