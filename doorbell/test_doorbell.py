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


class TypedAddressMailIsNotDropped(unittest.TestCase):
    """`post watch` omits `room` for a typed address.

    When the watched identity is a participant or a lineage, watch carries it in
    `address` instead: the `room` field appears only when watch is actually
    watching a workspace. A doorbell that insists on `room` therefore drops
    every participant and lineage message -- which is the whole of an agent's
    direct mail. These events are real `post watch --snapshot` lines.
    """

    def test_participant_mail_is_accepted_with_a_participant_namespace(self):
        key, source, reason = doorbell.event_metadata({
            "event": "mail",
            "address": {"kind": "participant", "name": "probe-6eb932de"},
            "id": "20260923-023848-33ad64",
            "reason": "mail",
        })
        self.assertEqual(source, "mail")
        self.assertEqual(reason, "mail")
        self.assertEqual(key, ("mail", "participant:probe-6eb932de", "20260923-023848-33ad64"))

    def test_lineage_mail_is_accepted_with_a_lineage_namespace(self):
        key, source, _ = doorbell.event_metadata({
            "event": "mail",
            "address": {"kind": "lineage", "name": "probe-lineage"},
            "id": "20260923-023848-f8bd81",
            "reason": "mail",
        })
        self.assertEqual(source, "mail")
        self.assertEqual(key, ("mail", "lineage:probe-lineage", "20260923-023848-f8bd81"))

    def test_workspace_mail_still_namespaces_by_room(self):
        key, source, _ = doorbell.event_metadata({
            "event": "mail",
            "address": {"kind": "workspace", "name": "alpha"},
            "room": "alpha",
            "id": "20260923-023849-5eb84d",
            "reason": "mail",
        })
        self.assertEqual(source, "mail")
        self.assertEqual(key, ("mail", "alpha", "20260923-023849-5eb84d"))

    def test_typed_unreadable_mail_takes_the_typed_namespace(self):
        key, source, _ = doorbell.event_metadata({
            "event": "unreadable",
            "address": {"kind": "participant", "name": "probe-6eb932de"},
            "id": "20260923-023848-33ad64",
            "reason": "mail",
        })
        self.assertEqual(source, "mail")
        self.assertEqual(key, ("mail", "participant:probe-6eb932de", "20260923-023848-33ad64"))

    def test_typed_unreadable_channel_mail_becomes_the_legacy_episode(self):
        # A channel message with no channel identity is the one compatibility
        # warning, whatever address carried it; a typed address must not turn
        # that event into a rejection that silently drops the warning.
        key, source, _ = doorbell.event_metadata({
            "event": "unreadable",
            "address": {"kind": "lineage", "name": "probe-lineage"},
            "id": "20260923-023848-f8bd81",
            "reason": "channel",
        })
        self.assertEqual(source, "unreadable-channel")
        self.assertEqual(key, doorbell.LEGACY_CHANNEL_EPISODE)

    def test_a_workspace_address_that_contradicts_room_is_rejected(self):
        for room in ("other", ""):
            with self.assertRaises(ValueError):
                doorbell.event_metadata({
                    "event": "mail",
                    "address": {"kind": "workspace", "name": "alpha"},
                    "room": room,
                    "id": "20260923-023849-5eb84d",
                    "reason": "mail",
                })

    def test_an_unusable_address_is_rejected_not_read_as_a_room(self):
        for address in (
            {"kind": "elsewhere", "name": "alpha"},
            {"kind": "participant"},
            {"kind": "participant", "name": "../escape"},
            None,
            "alpha",
        ):
            with self.assertRaises(ValueError):
                doorbell.event_metadata({
                    "event": "mail",
                    "address": address,
                    "id": "20260923-023849-5eb84d",
                    "reason": "mail",
                })

    def test_the_same_name_in_two_kinds_is_two_deliveries(self):
        def key(kind, name, room=None):
            event = {"event": "mail", "address": {"kind": kind, "name": name},
                     "id": "same", "reason": "mail"}
            if room is not None:
                event["room"] = room
            return doorbell.event_metadata(event)[0]

        keys = {key("workspace", "x", room="x"), key("participant", "x"), key("lineage", "x")}
        self.assertEqual(len(keys), 3)


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
