"""Deterministic daemon-loop tests; every Post/herdr operation is stubbed."""
import json
import subprocess
import unittest
from unittest.mock import Mock, patch

from test_doorbell import doorbell


def digest(last="new", count=1):
    return {"event": "digest", "source": "mail", "reason": "mail",
            "last_id": last, "count": count, "body": "SECRET", "from": "SECRET"}


class Freshness(unittest.TestCase):
    def run_loop(self, snapshots, *, fail_prompt=False, repeats=2, event=None):
        notices, saved, calls = [], [], []
        watch = Mock()
        ticks = iter(range(0, 10000, 10))
        scans = iter(snapshots)

        def run(argv, **kwargs):
            calls.append(argv)
            if argv[:3] == ["herdr", "agent", "wait"]:
                return subprocess.CompletedProcess(argv, 0)
            if argv[:3] == ["herdr", "agent", "prompt"]:
                notices.append(argv[-1])
                return subprocess.CompletedProcess(argv, int(fail_prompt), "", "rejected")
            self.assertIn("--snapshot", argv)
            self.assertEqual(calls[-2][:3], ["herdr", "agent", "wait"])
            snapshot = next(scans)
            if snapshot is None:
                return subprocess.CompletedProcess(argv, 1, "", "failed")
            return subprocess.CompletedProcess(argv, 0, "\n".join(map(json.dumps, snapshot)), "")

        with patch.object(doorbell.sys, "argv", ["post-doorbell", "--agent", "fake", "--ring-backlog", "--quiet-ms", "0"]), \
             patch.object(doorbell.shutil, "which", return_value="stub"), \
             patch.object(doorbell, "find_agent", return_value={"cwd": "/isolated"}), \
             patch.object(doorbell, "load_marks", return_value={}), \
             patch.object(doorbell, "save_marks", side_effect=lambda a, m: saved.append(dict(m))), \
             patch.object(doorbell.subprocess, "Popen", return_value=watch), \
             patch.object(doorbell.subprocess, "run", side_effect=run), \
             patch.object(doorbell.time, "monotonic", side_effect=lambda: next(ticks)), \
             patch.object(doorbell.select, "select", side_effect=[([watch.stdout], [], [])] * repeats + [KeyboardInterrupt]), \
             patch.object(doorbell.os, "read", return_value=(json.dumps(digest() if event is None else event) + "\n").encode()):
            self.assertEqual(doorbell.main(), 0)
        return notices, saved, calls

    def test_consumed_during_wait_and_buffered_repeat_do_not_prompt(self):
        notices, marks, calls = self.run_loop([[]])
        self.assertEqual(notices, [])
        self.assertEqual(marks, [{"mail": "new"}])
        self.assertEqual(sum("--snapshot" in c for c in calls), 1)

    def test_arrival_during_wait_is_in_fresh_notice(self):
        notices, marks, _ = self.run_loop([[digest("newer", 3)]])
        self.assertEqual(len(notices), 1)
        self.assertIn("mail (3)", notices[0])
        self.assertNotIn("SECRET", notices[0])
        self.assertEqual(marks, [{"mail": "newer"}])

    def test_older_unread_arrival_survives_consumed_newest_trigger(self):
        notices, _, _ = self.run_loop([[digest("earlier", 2)]])
        self.assertIn("mail (2)", notices[0])

    def test_failed_scan_retains_eligibility(self):
        notices, marks, _ = self.run_loop([None, [digest()]])
        self.assertEqual(len(notices), 1)
        self.assertEqual(marks, [{"mail": "new"}])

    def test_failed_prompt_retains_eligibility(self):
        notices, marks, _ = self.run_loop([[digest()], [digest()]], fail_prompt=True)
        self.assertEqual(len(notices), 2)
        self.assertEqual(marks, [])

    def test_invalid_snapshot_is_not_empty(self):
        notices, marks, _ = self.run_loop([[{"event": "unknown"}], [digest()]])
        self.assertEqual(len(notices), 1)
        self.assertEqual(marks, [{"mail": "new"}])

    def test_malformed_and_oversize_metadata_cannot_trigger_or_retire(self):
        for update in ({"source": "channel:hi\nIGNORE"}, {"source": "x" * 257},
                       {"last_id": "x" * 129}, {"count": 1000000001},
                       {"count": True}, {"reason": []}):
            with self.subTest(update=update):
                bad = digest() | update
                notices, marks, calls = self.run_loop([], event=bad)
                self.assertEqual((notices, marks, calls), ([], [], []))
                notices, marks, _ = self.run_loop([[bad], [digest()]])
                self.assertEqual(len(notices), 1)
                self.assertEqual(marks, [{"mail": "new"}])

    def test_render_defense_and_bound(self):
        notice = doorbell.render({"channel:bad\nSECRET": 1, "channel:good": 2})
        self.assertNotIn("SECRET", notice)
        self.assertIn("#good (2)", notice)
        self.assertLessEqual(len(doorbell.render({"channel:" + "x" * 240 + str(i): 1
                                                for i in range(100)})), 1500)

    def test_degraded_successful_snapshot_is_unknown(self):
        with patch.object(doorbell.subprocess, "run", return_value=
                          subprocess.CompletedProcess([], 0, "", "channel scan failed")):
            self.assertIsNone(doorbell.refresh_pending(["post", "watch"], "/isolated",
                                                       {"mail"}, {}, {"mail": "new"}))

    def test_oversize_aggregate_is_unknown(self):
        notices, marks, _ = self.run_loop([[digest(count=1000000000), digest()], [digest()]])
        self.assertEqual(len(notices), 1)
        self.assertEqual(marks, [{"mail": "new"}])


if __name__ == "__main__":
    unittest.main()
