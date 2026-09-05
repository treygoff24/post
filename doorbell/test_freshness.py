"""Faithful individual WatchEvent fixtures; no live Post operations."""
import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch
from test_doorbell import doorbell


def mail(ident="new", room="r"):
    return {"event": "mail", "room": room, "id": ident, "reason": "mail",
            "subject": "SECRET", "preview": "SECRET", "from": "SECRET"}


class Freshness(unittest.TestCase):
    def run_loop(self, snapshots, *, fail_prompt=False, events=None, prime=False, prior=None):
        notices, saved, calls = [], [], []
        watch = Mock()
        ticks, scans = iter(range(0, 10000, 10)), iter(snapshots)
        prompt_results = iter(fail_prompt) if isinstance(fail_prompt, list) else None
        events = [mail(), mail()] if events is None else events

        def run(argv, **kwargs):
            calls.append(argv)
            if argv[:3] == ["herdr", "agent", "wait"]:
                return subprocess.CompletedProcess(argv, 0)
            if argv[:3] == ["herdr", "agent", "prompt"]:
                notices.append(argv[-1])
                failed = next(prompt_results) if prompt_results is not None else fail_prompt
                return subprocess.CompletedProcess(argv, int(failed), "", "rejected")
            self.assertIn("--snapshot", argv)
            self.assertNotIn("--digest", argv)
            snapshot = next(scans)
            if snapshot is None:
                return subprocess.CompletedProcess(argv, 1, "", "failed")
            return subprocess.CompletedProcess(argv, 0, "\n".join(map(json.dumps, snapshot)), "")

        argv = ["post-doorbell", "--agent", "fake", "--quiet-ms", "0", "--wake-on", "all"]
        if not prime:
            argv.append("--ring-backlog")
        with patch.object(doorbell.sys, "argv", argv), \
             patch.object(doorbell.shutil, "which", return_value="stub"), \
             patch.object(doorbell, "find_agent", return_value={"cwd": "/isolated"}), \
             patch.object(doorbell, "load_marks", return_value=set() if prior is None else prior), \
             patch.object(doorbell, "save_marks", side_effect=lambda a, m: saved.append(set(m))), \
             patch.object(doorbell.subprocess, "Popen", return_value=watch), \
             patch.object(doorbell.subprocess, "run", side_effect=run), \
             patch.object(doorbell.time, "monotonic", side_effect=lambda: next(ticks)), \
             patch.object(doorbell.select, "select", side_effect=[([watch.stdout], [], [])] * len(events) + [KeyboardInterrupt]), \
             patch.object(doorbell.os, "read", side_effect=[(json.dumps(e) + "\n").encode() for e in events]):
            self.assertEqual(doorbell.main(), 0)
        return notices, saved, calls

    def test_consumed_during_wait_and_buffered_repeat_do_not_prompt(self):
        notices, marks, calls = self.run_loop([[], []])
        self.assertEqual(notices, [])
        self.assertEqual(marks, [set(), set()])
        self.assertEqual(sum("--snapshot" in c for c in calls), 2)

    def test_arrivals_during_wait_are_in_fresh_notice(self):
        notices, marks, _ = self.run_loop([[mail(), mail("second"), mail("third")]])
        self.assertEqual(len(notices), 1)
        self.assertIn("mail (3)", notices[0])
        self.assertNotIn("SECRET", notices[0])
        self.assertEqual(len(marks[0]), 3)

    def test_later_same_second_lower_sorting_mail_rings_in_both_rooms(self):
        first = mail("20260905-120000-ffffff")
        for room in ("r", "other room"):
            second = mail("20260905-120000-000000", room)
            notices, marks, _ = self.run_loop([[first], [first, second]], events=[first, second])
            self.assertEqual(len(notices), 2)
            self.assertTrue(all("mail (1)" in n for n in notices))
            self.assertEqual(len(marks[-1]), 2)

    def test_consumed_keys_are_pruned_after_success(self):
        notices, marks, _ = self.run_loop([[mail()], [mail("second")]], events=[mail(), mail("second")])
        self.assertEqual(len(notices), 2)
        self.assertEqual(marks[-1], {("mail", "r", "second")})

    def test_failed_scan_retains_eligibility(self):
        notices, marks, _ = self.run_loop([None, [mail()]])
        self.assertEqual(len(notices), 1)
        self.assertEqual(marks, [{("mail", "r", "new")}])

    def test_failed_prompt_retains_eligibility(self):
        notices, marks, _ = self.run_loop([[mail()], [mail()]], fail_prompt=True)
        self.assertEqual(len(notices), 2)
        self.assertEqual(marks, [])

    def test_malformed_startup_is_unknown_and_retries_without_new_trigger(self):
        notices, marks, _ = self.run_loop([[[]], [mail()]], prime=True, events=[{}])
        self.assertEqual(len(notices), 1)
        self.assertEqual(marks, [{("mail", "r", "new")}])

    def test_startup_primes_exact_backlog_but_not_lower_sorting_arrival(self):
        first, second = mail("z"), mail("a")
        notices, marks, _ = self.run_loop([[first], [first, second]], prime=True, events=[first, second])
        self.assertEqual(len(notices), 1)
        self.assertIn("mail (1)", notices[0])
        self.assertEqual(len(marks[-1]), 2)

    def test_maximum_names_and_unicode_channels_remain_eligible(self):
        for name in ("x" * 255, "team ops", "café"):
            event = {"event": "channel_message", "channel": name, "id": "channel-id",
                     "reason": "mention", "subject": "SECRET", "from": "SECRET"}
            notices, marks, _ = self.run_loop([[event]], events=[event, event])
            self.assertEqual(len(notices), 1)
            self.assertIn("(1)", notices[0])
            self.assertNotIn("SECRET", notices[0])
            self.assertLessEqual(len(notices[0]), 1500)
            self.assertEqual(marks, [{("channel:" + name, name, "channel-id")}])

    def test_unreadable_opaque_id_and_expected_warning_are_accepted(self):
        event = {"event": "unreadable", "room": "r", "id": "broken.name", "reason": "mail"}
        output = subprocess.CompletedProcess([], 0, json.dumps(event),
                                            'post: warning: unreadable mail "bad": "bad envelope"\n')
        with patch.object(doorbell.subprocess, "run", return_value=output):
            self.assertEqual(doorbell.snapshot_events(["post", "watch"], "/isolated", {"mail"}),
                             {("mail", "r", "broken.name"): "mail"})
        notices, _, _ = self.run_loop([[event]], events=[event, event])
        self.assertEqual(len(notices), 1)
        self.assertNotIn("broken.name", notices[0])

    def test_degraded_scan_and_malformed_metadata_are_unknown(self):
        for raw, stderr in (("[]", ""), (json.dumps(mail() | {"room": "x" * 256}), ""),
                            (json.dumps(mail() | {"id": []}), ""), ("", "channel scan failed")):
            with patch.object(doorbell.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, raw, stderr)):
                self.assertIsNone(doorbell.snapshot_events(["post", "watch"], "/isolated", {"mail"}))

    def test_unsafe_name_uses_placeholder_not_prompt_text(self):
        notice = doorbell.render({"channel:unsafe SECRET `instruction`": 1})
        self.assertNotIn("SECRET", notice)
        self.assertIn("[non-simple name] (1)", notice)

    def test_maximum_mail_room_and_unreadable_filename_are_opaque(self):
        event = {"event": "unreadable", "room": "x" * 255,
                 "id": "bad.name\nSECRET", "reason": "mail"}
        notices, marks, _ = self.run_loop([[event]], events=[event, event])
        self.assertEqual(len(notices), 1)
        self.assertNotIn("SECRET", notices[0])
        self.assertEqual(len(marks[0]), 1)

    def test_state_round_trip_and_legacy_or_malformed_state(self):
        with tempfile.TemporaryDirectory() as tmp, patch.object(doorbell, "state_path", return_value=str(Path(tmp) / "state.json")):
            path = Path(tmp) / "state.json"
            keys = {("mail", "room with spaces", "broken.name")}
            doorbell.save_marks("fake", keys)
            self.assertEqual(doorbell.load_marks("fake"), keys)
            for bad in ({"mail": "old-watermark"}, [], {"version": 2, "keys": [[1]]}):
                path.write_text(json.dumps(bad))
                self.assertEqual(doorbell.load_marks("fake"), set())

    def test_legacy_mixed_filter_cannot_become_silent(self):
        self.assertEqual(doorbell.parse_wake_on("mixed"), {"all"})

    def test_unreadable_channels_with_same_id_have_distinct_keys_and_prune(self):
        first = {"event": "unreadable", "room": "r", "id": "same.bad",
                 "reason": "channel", "channel": "first"}
        second = first | {"channel": "second"}
        notices, marks, _ = self.run_loop([[first, second], [second]], events=[first, mail()])
        self.assertEqual(len(notices), 1)
        self.assertIn("#first (1)", notices[0])
        self.assertIn("#second (1)", notices[0])
        self.assertNotIn("same.bad", notices[0])
        self.assertEqual(len(marks[0]), 2)
        self.assertEqual(marks[-1], {("channel:second", "second", "same.bad")})
        with tempfile.TemporaryDirectory() as tmp, patch.object(doorbell, "state_path", return_value=str(Path(tmp) / "keys.json")):
            doorbell.save_marks("fake", marks[0])
            self.assertEqual(doorbell.load_marks("fake"), marks[0])

    def test_legacy_channel_episode_warns_once_without_unique_ack(self):
        event = {"event": "unreadable", "room": "r", "id": "same.bad", "reason": "channel"}
        notices, marks, _ = self.run_loop([[event]] * 5, events=[event] * 5)
        self.assertEqual(len(notices), 1)
        self.assertIn("Per-message delivery is unknown", notices[0])
        self.assertEqual(marks, [{doorbell.LEGACY_CHANNEL_EPISODE}] * 5)
        self.assertTrue(all("same.bad" not in notice for notice in notices))
        with self.assertRaises(ValueError):
            doorbell.event_metadata(event | {"channel": "../unsafe"})

    def test_startup_does_not_prime_legacy_ids_or_an_unaccepted_warning(self):
        event = {"event": "unreadable", "room": "r", "id": "same.bad", "reason": "channel"}
        notices, marks, _ = self.run_loop([[event]] * 3, prime=True)
        self.assertEqual(len(notices), 1)
        self.assertEqual(marks[0], set())
        self.assertEqual(marks[-1], {doorbell.LEGACY_CHANNEL_EPISODE})
        notices, _, _ = self.run_loop([[event]] * 3, prime=True,
                                     prior={doorbell.LEGACY_CHANNEL_EPISODE})
        self.assertEqual(notices, [])

    def test_legacy_episode_clear_reappear_and_new_mail(self):
        event = {"event": "unreadable", "room": "r", "id": "same.bad", "reason": "channel"}
        notices, marks, _ = self.run_loop([[event], [event, mail()], [], [event]],
                                          events=[event, mail(), event, event])
        self.assertEqual(len(notices), 3)
        self.assertIn("compatibility warning", notices[0])
        self.assertIn("mail (1)", notices[1])
        self.assertNotIn("compatibility warning", notices[1])
        self.assertIn("compatibility warning", notices[2])
        self.assertEqual(marks[-2], set())

    def test_failed_warning_and_failed_scan_do_not_ack_or_clear_episode(self):
        event = {"event": "unreadable", "room": "r", "id": "same.bad", "reason": "channel"}
        notices, marks, _ = self.run_loop([[event], [event], None, [event]],
                                          events=[event] * 4, fail_prompt=[True, False])
        self.assertEqual(len(notices), 2)
        self.assertEqual(marks, [{doorbell.LEGACY_CHANNEL_EPISODE}] * 2)

    def test_warning_and_valid_mail_share_one_accepted_notice(self):
        event = {"event": "unreadable", "room": "r", "id": "same.bad", "reason": "channel"}
        notices, marks, _ = self.run_loop([[event, mail()]] * 2, events=[event, event])
        self.assertEqual(len(notices), 1)
        self.assertIn("compatibility warning", notices[0])
        self.assertIn("mail (1)", notices[0])
        self.assertEqual(marks[-1], {doorbell.LEGACY_CHANNEL_EPISODE, ("mail", "r", "new")})

    def test_restored_episode_checks_for_clear_even_with_ring_backlog(self):
        notices, marks, _ = self.run_loop([[]], events=[{}],
                                          prior={doorbell.LEGACY_CHANNEL_EPISODE})
        self.assertEqual(notices, [])
        self.assertEqual(marks, [set()])


if __name__ == "__main__":
    unittest.main()
