"""Contract tests for post-doorbell.

These pin properties, not implementation: what the notice may never contain, and
the two ways a doorbell can fail silently (never ringing, or retiring a message
nobody was told about). Every one of them corresponds to a defect that actually
shipped in this file today.
"""

import importlib.util
import json
import subprocess
import sys
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("post-doorbell")
spec = importlib.util.spec_from_loader("doorbell", importlib.machinery.SourceFileLoader("doorbell", str(SCRIPT)))
doorbell = importlib.util.module_from_spec(spec)
spec.loader.exec_module(doorbell)


def run_cli(*args):
    proc = subprocess.run([sys.executable, str(SCRIPT), *args], capture_output=True, text=True, check=False)
    return proc.returncode, proc.stdout + proc.stderr


class NoticeCarriesNoContent(unittest.TestCase):
    """The wake exists to say mail arrived, never what it said.

    Content reaching a model through a door it did not open is how an agent ends
    up acting on instructions nobody vetted; the notice is that door.
    """

    def test_render_emits_only_channel_names_and_counts(self):
        notice = doorbell.render({"channel:machineroom-devbox": 3, "mail": 1})
        self.assertIn("#machineroom-devbox (3)", notice)
        self.assertIn("mail (1)", notice)

    def test_render_cannot_leak_a_subject_or_body_it_never_receives(self):
        # render's whole input is {source: count}; there is no parameter through
        # which a body could arrive. This test fails loudly if that ever changes.
        import inspect
        self.assertEqual(list(inspect.signature(doorbell.render).parameters), ["pending"])

    def test_notice_is_capped(self):
        notice = doorbell.render({f"channel:c{i}": i for i in range(500)})
        self.assertLessEqual(len(notice), doorbell.NOTICE_CAP)


class ADoorbellThatCannotRingIsRefused(unittest.TestCase):
    """The default shipped as `direct`, which post never emits, so it matched
    nothing and said nothing. Silence must be impossible to configure by typo."""

    def test_unknown_reason_is_rejected_not_ignored(self):
        code, out = run_cli("--agent", "nobody", "--wake-on", "direkt")
        self.assertNotEqual(code, 0)
        self.assertIn("unknown --wake-on value", out)

    def test_empty_selection_is_rejected(self):
        code, out = run_cli("--agent", "nobody", "--wake-on", ",")
        self.assertNotEqual(code, 0)
        self.assertIn("can never ring", out)

    def test_direct_is_accepted_as_an_alias_for_mail(self):
        # Rejecting the word everyone reaches for first would just reproduce the
        # original bug with an error message instead of silence.
        code, out = run_cli("--agent", "nobody", "--wake-on", "direct")
        self.assertNotIn("unknown --wake-on value", out)

    def test_the_default_wakes_on_direct_mail(self):
        # The original defect: the default named a reason post never emits, so
        # it matched nothing. The contract is behavioural -- whatever words the
        # default uses, parsing it must select the reason carried by direct mail.
        self.assertIn("mail", doorbell.parse_wake_on(doorbell.DEFAULT_WAKE_ON))

    def test_a_default_naming_only_unemitted_reasons_is_rejected(self):
        with self.assertRaises(ValueError):
            doorbell.parse_wake_on("nonsense")


class OpaqueIdentity(unittest.TestCase):
    def test_same_second_ids_are_distinct_without_ordering(self):
        keys = {doorbell.event_metadata({"event": "mail", "room": "r", "reason": "mail", "id": ident})[0]
                for ident in ("20260905-120000-ffffff", "20260905-120000-000000")}
        self.assertEqual(len(keys), 2)

    def test_same_id_in_two_rooms_is_distinct(self):
        keys = {doorbell.event_metadata({"event": "mail", "room": room, "reason": "mail", "id": "same"})[0]
                for room in ("a", "b")}
        self.assertEqual(len(keys), 2)


if __name__ == "__main__":
    unittest.main(verbosity=2)
