"""Every line of one write must be seen, not just the first.

`select()` watches a file descriptor. A buffered text stream reads a whole chunk
off the fd into Python's own buffer, so lines 2..n of a single write become
invisible to select and wait for the NEXT write to wake the loop. A multi-room
scan emits one digest line per room in exactly one write, so this is the common
case, not a corner. Verified standalone: two lines in one write, select then
times out while holding line 2.
"""

import os
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("post-doorbell")

FAKE_HERDR = """#!/bin/sh
case "$1 $2" in
  "agent list") echo '{"result":{"agents":[{"name":"fake","cwd":"'"$FAKE_CWD"'","agent_status":"idle"}]}}' ;;
  "agent prompt") shift 2; echo "$@" >> "$NOTICE_LOG"; exit 0 ;;
  "agent wait") exit 0 ;;
  *) exit 0 ;;
esac
"""

# Two digest lines, two different rooms, ONE write -- then a long silence, so
# nothing else can wake the loop and rescue the second line.
FAKE_POST = """#!/bin/sh
case "$1" in
  rooms) echo '{"ok":true,"rooms":[{"name":"alpha"},{"name":"beta"}]}' ;;
  watch)
    case "$2" in --help) exit 0 ;; esac
    for a in "$@"; do [ "$a" = "--snapshot" ] && exit 0; done
    printf '%s\\n%s\\n' \\
      '{"event":"digest","source":"channel:alpha","count":1,"last_id":"20260825-180000-aaaaaa","reason":"mail"}' \\
      '{"event":"digest","source":"channel:beta","count":1,"last_id":"20260825-180000-bbbbbb","reason":"mail"}'
    exec sleep 30 ;;
esac
"""


class EveryLineOfOneWriteIsSeen(unittest.TestCase):
    def test_a_multi_room_batch_wakes_for_all_of_its_rooms(self):
        with tempfile.TemporaryDirectory() as tmp:
            binhome = Path(tmp) / "bin"
            binhome.mkdir()
            for name, body in (("herdr", FAKE_HERDR), ("post", FAKE_POST)):
                path = binhome / name
                path.write_text(body)
                path.chmod(0o755)
            notices = Path(tmp) / "notices.log"
            env = dict(os.environ)
            env["PATH"] = f"{binhome}:{env['PATH']}"
            env["XDG_STATE_HOME"] = str(Path(tmp) / "state")
            env["FAKE_CWD"] = tmp
            env["NOTICE_LOG"] = str(notices)

            # stderr to a file, not a pipe: the fake post's `sleep` grandchild
            # inherits the pipe and would keep communicate() blocked after the
            # daemon itself is gone.
            errlog = Path(tmp) / "stderr.log"
            with open(errlog, "w") as err:
                proc = subprocess.Popen(
                    [sys.executable, str(SCRIPT), "--agent", "fake",
                     "--room", "alpha", "--room", "beta", "--quiet-ms", "1500"],
                    env=env, stderr=err, text=True,
                )
                time.sleep(6)
                proc.terminate()
                try:
                    proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    proc.kill()

            text = notices.read_text() if notices.exists() else ""
            self.assertIn("alpha", text, "the first line of the batch must wake the agent")
            self.assertIn("beta", text,
                          "the second line of the SAME write must not sit invisible "
                          "in a buffer select cannot see")


if __name__ == "__main__":
    unittest.main(verbosity=2)
