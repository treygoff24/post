"""Contract tests: the doorbell against the post it will actually run.

The samples come from `$POST_BIN contract samples --dir <tmp>`, the normalized
output the real commands produced in post's own suite, compiled into that
binary. POST_BIN defaults to the repository's release build. Only the surfaces
the doorbell reads are covered: `post watch --snapshot` events (event_metadata,
snapshot_events) and the `post rooms` listing (registered_rooms).

Each surface is tested exact, with an unknown extra field (accepted), with each
optional field it reads absent (handled), and with negative fixtures (a missing
required field, a wrong type, a malformed discriminator), which must be
rejected. Identity and routing fields -- event kind, id, reason, address, room,
channel -- stay strict: an event that cannot be identified is never keyed.
"""

import copy
import importlib.machinery
import importlib.util
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
SCRIPT = HERE / "post-doorbell"
spec = importlib.util.spec_from_loader("doorbell", importlib.machinery.SourceFileLoader("doorbell", str(SCRIPT)))
doorbell = importlib.util.module_from_spec(spec)
spec.loader.exec_module(doorbell)

POST_BIN = os.environ.get("POST_BIN") or str(HERE.parent / "target" / "release" / "post")

# A `post` that prints a prepared stdout file and exits with a prepared status.
STUB = """#!/bin/sh
cat "$CONTRACT_STDOUT"
exit "${CONTRACT_EXIT:-0}"
"""


def setUpModule():
    global SAMPLES, TMP
    TMP = tempfile.TemporaryDirectory(prefix="doorbell-contract-")
    out = Path(TMP.name) / "samples"
    proc = subprocess.run([POST_BIN, "contract", "samples", "--dir", str(out)],
                          capture_output=True, text=True, check=False)
    if proc.returncode != 0:
        raise RuntimeError(f"{POST_BIN} contract samples failed (build post or set POST_BIN): "
                           f"{proc.returncode} {proc.stderr.strip()}")
    SAMPLES = out


def tearDownModule():
    TMP.cleanup()


def events(name):
    lines = (SAMPLES / name).read_text().splitlines()
    return [json.loads(line) for line in lines if line.strip()]


def listing(name):
    return json.loads((SAMPLES / name).read_text())


def find(sample, **match):
    for event in sample:
        if all(event.get(key) == value for key, value in match.items()):
            return copy.deepcopy(event)
    raise AssertionError(f"no sample event matches {match}")


def address_of(event):
    return event.get("address", {}).get("kind")


class StubPost:
    """Put a `post` first on PATH that replays one prepared stdout."""

    def __init__(self, stdout, exit_code=0):
        self.dir = tempfile.TemporaryDirectory(prefix="doorbell-stub-")
        root = Path(self.dir.name)
        (root / "post").write_text(STUB)
        (root / "post").chmod(0o755)
        (root / "stdout").write_text(stdout)
        self.env = {
            "PATH": f"{root}:{os.environ['PATH']}",
            "CONTRACT_STDOUT": str(root / "stdout"),
            "CONTRACT_EXIT": str(exit_code),
        }
        self.saved = {}

    def __enter__(self):
        for key, value in self.env.items():
            self.saved[key] = os.environ.get(key)
            os.environ[key] = value
        return Path(self.dir.name) / "post"

    def __exit__(self, *exc):
        for key, value in self.saved.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value
        self.dir.cleanup()


class WatchEventsExact(unittest.TestCase):
    """Every real event variant keys to its exact delivery identity."""

    def test_each_variant_keys_to_its_identity(self):
        sample = events("watch-snapshot.jsonl")
        participant = next(e for e in sample if address_of(e) == "participant")["address"]["name"]
        expected = {
            "workspace": ("mail", "alpha"),
            "participant": ("mail", "participant:" + participant),
            "lineage": ("mail", "lineage:ember"),
        }
        seen = set()
        for event in sample:
            key, source, reason = doorbell.event_metadata(event)
            self.assertEqual(reason, event["reason"])
            if event["event"] == "channel_message":
                self.assertEqual(key, ("channel:" + event["channel"], event["channel"], event["id"]))
            elif event["event"] == "unreadable" and event["reason"] == "channel":
                self.assertEqual(key, ("channel:broken", "broken", event["id"]))
            else:
                self.assertEqual(key[2], event["id"])
                self.assertEqual(key[:2], expected[address_of(event)])
                self.assertEqual(source, "mail")
            seen.add((event["event"], event["reason"], address_of(event)))
        # The sample really carries every variant the doorbell distinguishes.
        for variant in [("mail", "mail", "workspace"), ("mail", "mail", "participant"),
                        ("mail", "mail", "lineage"), ("unreadable", "mail", "workspace"),
                        ("unreadable", "channel", "workspace"),
                        ("channel_message", "channel", "workspace"),
                        ("channel_message", "mention", "workspace")]:
            self.assertIn(variant, seen)

    def test_pending_and_remote_origin_events_key_like_any_mail(self):
        sample = events("watch-snapshot.jsonl")
        remote = find(sample, origin="remote")
        self.assertTrue(remote["pending"])
        self.assertEqual(doorbell.event_metadata(remote)[0], ("mail", "alpha", remote["id"]))

    def test_cursor_rereport_events_are_accepted(self):
        for event in events("watch-snapshot-cursor-unusable.jsonl"):
            self.assertIs(event["cursor_unusable"], True)
            doorbell.event_metadata(event)

    def test_snapshot_reads_the_whole_real_stream(self):
        text = (SAMPLES / "watch-snapshot.jsonl").read_text()
        with StubPost(text) as post:
            current = doorbell.snapshot_events([str(post), "watch"], os.getcwd(), {"all"})
        self.assertIsNotNone(current)
        sample = events("watch-snapshot.jsonl")
        # Every real event keys uniquely; nothing is dropped.
        self.assertEqual(len(current), len(sample))
        with StubPost(text) as post:
            mail_only = doorbell.snapshot_events([str(post), "watch"], os.getcwd(), {"mail"})
        self.assertEqual(set(mail_only.values()), {"mail"})

    def test_a_digest_line_is_not_an_event_the_doorbell_reads(self):
        # The doorbell never asks for --digest; if one arrived it must not be
        # keyed as a delivery, and the whole snapshot is then unknown.
        digest = events("watch-snapshot-digest.jsonl")[0]
        with self.assertRaises(ValueError):
            doorbell.event_metadata(digest)
        with StubPost(json.dumps(digest) + "\n") as post:
            self.assertIsNone(doorbell.snapshot_events([str(post), "watch"], os.getcwd(), {"all"}))


class WatchEventsTolerateAdditions(unittest.TestCase):
    def test_unknown_fields_are_accepted_everywhere(self):
        for event in events("watch-snapshot.jsonl") + events("watch-snapshot-cursor-unusable.jsonl"):
            before = doorbell.event_metadata(event)
            grown = copy.deepcopy(event)
            grown["zz_future"] = {"nested": [1, "two"]}
            grown["address"]["zz_future"] = True
            self.assertEqual(doorbell.event_metadata(grown), before)


class WatchEventsOptionalAbsent(unittest.TestCase):
    """The optional fields the doorbell reads: room, address, unreadable channel."""

    def test_workspace_event_without_room_keys_by_address(self):
        event = find(events("watch-snapshot.jsonl"), event="mail", room="alpha", origin="local")
        before = doorbell.event_metadata(event)
        del event["room"]
        self.assertEqual(doorbell.event_metadata(event), before)

    def test_workspace_event_without_address_keys_by_room(self):
        event = find(events("watch-snapshot.jsonl"), event="mail", room="alpha", origin="local")
        before = doorbell.event_metadata(event)
        del event["address"]
        self.assertEqual(doorbell.event_metadata(event), before)

    def test_typed_address_event_without_address_is_unidentifiable(self):
        sample = events("watch-snapshot.jsonl")
        event = next(e for e in sample if address_of(e) == "participant")
        del event["address"]
        with self.assertRaises(ValueError):
            doorbell.event_metadata(event)

    def test_unreadable_channel_without_channel_is_the_legacy_episode(self):
        event = find(events("watch-snapshot.jsonl"), event="unreadable", reason="channel")
        del event["channel"]
        key, source, _ = doorbell.event_metadata(event)
        self.assertEqual(key, doorbell.LEGACY_CHANNEL_EPISODE)
        self.assertEqual(source, "unreadable-channel")

    def test_fields_the_doorbell_never_reads_may_all_be_absent(self):
        for event in events("watch-snapshot.jsonl"):
            before = doorbell.event_metadata(event)
            for field in ("from", "from_participant", "from_lineage", "origin", "pending", "preview",
                          "reply_to_participant", "reply_to_shared", "sender_provenance", "sent",
                          "subject", "kind", "display_name", "pfp", "cursor_unusable"):
                event.pop(field, None)
            self.assertEqual(doorbell.event_metadata(event), before)


class WatchEventsRejectMalformed(unittest.TestCase):
    def assertRejected(self, event):
        with self.assertRaises((ValueError, TypeError, AttributeError)):
            doorbell.event_metadata(event)

    def sample(self, **match):
        return find(events("watch-snapshot.jsonl"), **match)

    def test_missing_required_fields(self):
        for field in ("event", "id", "reason"):
            for match in ({"event": "mail", "room": "alpha", "origin": "local"},
                          {"event": "channel_message", "reason": "mention"},
                          {"event": "unreadable", "reason": "mail"}):
                event = self.sample(**match)
                del event[field]
                with self.subTest(field=field, match=match):
                    self.assertRejected(event)
        event = self.sample(event="channel_message", reason="channel")
        del event["channel"]
        self.assertRejected(event)

    def test_wrong_types(self):
        cases = [("id", 20260101), ("reason", ["mail"]), ("event", None), ("room", 7),
                 ("channel", ["tax"])]
        for field, bad in cases:
            match = {"event": "channel_message", "reason": "channel"} if field == "channel" else {
                "event": "mail", "room": "alpha", "origin": "local"}
            event = self.sample(**match)
            event[field] = bad
            with self.subTest(field=field):
                self.assertRejected(event)
        event = self.sample(event="mail", room="alpha", origin="local")
        event["address"] = "workspace:alpha"
        self.assertRejected(event)
        event = self.sample(event="mail", room="alpha", origin="local")
        event["address"]["name"] = 1
        self.assertRejected(event)

    def test_malformed_discriminators(self):
        event = self.sample(event="mail", room="alpha", origin="local")
        event["event"] = "mail_v2"
        self.assertRejected(event)
        event = self.sample(event="mail", room="alpha", origin="local")
        event["reason"] = "urgent"
        self.assertRejected(event)
        event = self.sample(event="mail", room="alpha", origin="local")
        event["reason"] = "mention"  # mail can never be a mention
        self.assertRejected(event)
        event = self.sample(event="unreadable", reason="channel")
        event["reason"] = "mention"  # an unreadable body cannot mention anyone
        self.assertRejected(event)
        event = self.sample(event="mail", room="alpha", origin="local")
        event["address"]["kind"] = "room"
        self.assertRejected(event)

    def test_routing_fields_must_agree(self):
        event = self.sample(event="mail", room="alpha", origin="local")
        event["room"] = "beta"  # workspace address and room disagree
        self.assertRejected(event)
        typed = next(e for e in events("watch-snapshot.jsonl") if address_of(e) == "lineage")
        typed["room"] = "alpha"  # a typed address never comes with a room
        self.assertRejected(typed)
        event = self.sample(event="channel_message", reason="channel")
        event["channel"] = "../tax"
        self.assertRejected(event)

    def test_one_malformed_line_makes_the_snapshot_unknown_not_smaller(self):
        lines = (SAMPLES / "watch-snapshot.jsonl").read_text().splitlines()
        broken = json.loads(lines[0])
        broken["id"] = 5
        text = "\n".join([json.dumps(broken)] + lines[1:]) + "\n"
        with StubPost(text) as post:
            self.assertIsNone(doorbell.snapshot_events([str(post), "watch"], os.getcwd(), {"all"}))


class RoomsListing(unittest.TestCase):
    """`post rooms`: ok plus rooms[].name is all the doorbell reads."""

    def names(self, value, exit_code=0):
        with StubPost(json.dumps(value) + "\n", exit_code):
            return doorbell.registered_rooms(os.getcwd())

    def test_exact(self):
        value = listing("rooms.json")
        self.assertEqual(self.names(value), {room["name"] for room in value["rooms"]})
        self.assertIn("remote-room", self.names(value))

    def test_unknown_fields_are_accepted(self):
        value = listing("rooms.json")
        value["zz_future"] = 1
        for room in value["rooms"]:
            room["zz_future"] = {"x": 1}
        self.assertEqual(self.names(value), {room["name"] for room in listing("rooms.json")["rooms"]})

    def test_optional_fields_absent(self):
        value = listing("rooms.json")
        for room in value["rooms"]:
            room.pop("path")
            room.pop("blocked")
        del value["count"]
        self.assertEqual(self.names(value), {room["name"] for room in listing("rooms.json")["rooms"]})

    def test_not_ok_or_failed_listing_is_unchecked_not_empty(self):
        value = listing("rooms.json")
        value["ok"] = False
        self.assertIsNone(self.names(value))
        del value["ok"]
        self.assertIsNone(self.names(value))
        self.assertIsNone(self.names(listing("rooms.json"), exit_code=1))

    def test_missing_or_mistyped_room_name_never_registers_a_room(self):
        value = listing("rooms.json")
        del value["rooms"][0]["name"]
        names = self.names(value)
        # A nameless row cannot vouch for any room the doorbell was asked for.
        self.assertNotIn(listing("rooms.json")["rooms"][0]["name"], names)
        value = listing("rooms.json")
        value["rooms"][0]["name"] = 7
        self.assertNotIn(listing("rooms.json")["rooms"][0]["name"], self.names(value))

    def test_wrong_container_types_are_rejected(self):
        # Rejected, though not gracefully: registered_rooms documents an
        # unreadable listing as "unchecked" (None), but a non-object listing or
        # a non-object room row raises AttributeError out of the startup check.
        # Reported as a defect; the contract here is only that it never
        # yields a set of names.
        for bad in ([], {"ok": True, "rooms": "alpha"}, {"ok": True, "rooms": [["alpha"]]}):
            with self.subTest(bad=bad):
                try:
                    result = self.names(bad)
                except AttributeError:
                    continue
                self.assertIsNone(result)


if __name__ == "__main__":
    unittest.main()
