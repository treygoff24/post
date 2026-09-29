#!/usr/bin/env python3
"""Decided markers: a full tick touches only the ids it has not settled.

Before bridgelib/decided.py every full tick re-opened, re-parsed and
re-judged every archive letter that was not received, delivered or published
(the local-only and held majority: about 1,000 on the Mac) and every local
channel message (about 8,700 files), forever. The tests here pin how that is
avoided and, above all, that it never costs a decision:

* a settled id is recognised without being opened (proved with a file the
  bridge could not read if it tried);
* a marker is trusted only inside its recheck window, and a re-judged id has
  its marker refreshed;
* a crash between the work and its marker redoes the work and never skips it;
* a store fault marks nothing, so a letter met during the fault is judged
  properly once the store is repaired;
* a letter judged unrelayable is re-read when (and only when) its file
  changes, and a transient read failure is never remembered as a verdict.
"""

import hashlib
import json
import os
import signal
import subprocess
import sys
import time
import types
import unittest
from pathlib import Path

from .test_sweep import (  # first: it puts the bridge package on sys.path
    SWEEP,
    SWEEPER,
    CanonicalTemporaryDirectory,
    craft_mail,
    fixed_id,
)
from .harness_channels import Deadline, Git, Records  # noqa: E402
from .harness_channels import Topology as ChannelTopology  # noqa: E402
from .harness_channels import channel_id, channel_message, channel_record  # noqa: E402
from .test_channels import ALL, local_snapshot  # noqa: E402
from .test_terminal import TerminalFixture  # noqa: E402
from bridgelib import channels  # noqa: E402

PAST_THE_WINDOW = 7 * 3600  # the default window is 6 hours


def sha(data):
    return hashlib.sha256(data).hexdigest()


class ArchiveDecidedTest(TerminalFixture):
    """fc (garden), trey (hq, atlasos), mac (porch); nothing contested."""

    def tick(self, machine, expect=0, **env):
        """One full tick under the real default recheck window unless the
        test names another (the fixture's own default is 0)."""
        env.setdefault("BRIDGE_DECIDED_RECHECK_SECONDS", None)
        fingerprint = machine.root / "bridge" / "trigger-fingerprint.json"
        if fingerprint.exists():
            fingerprint.unlink()
        result = machine.sweep(**env)
        if expect is not None:
            self.assertEqual(
                result.returncode, expect, f"{machine.host}: {result.stdout}{result.stderr}"
            )
        return result

    def local_letter(self, body):
        """trey hq -> atlasos: post delivers it into trey's own atlasos room,
        so the local-held guard holds it for good."""
        return self.trey.send("hq", "atlasos", body)

    def marker(self, machine, mail_id, kind="held"):
        return machine.root / "bridge" / "decided" / kind / mail_id

    def record(self, mail_id):
        path = self.trey.root / "bridge" / "local-held" / (mail_id + ".json")
        return json.loads(path.read_bytes()) if path.exists() else None

    def exported(self, machine, mail_id):
        """Whether the machine staged, pushed or published this letter."""
        staged = list((machine.repo / "outbox").rglob(mail_id + ".mail"))
        published = (machine.root / "bridge" / "published" / mail_id).exists()
        pushed = any(
            path.endswith("/" + mail_id + ".mail")
            for path in self.remote_tree(machine.host)
        )
        return bool(staged or published or pushed)

    def unreadable(self, path):
        """Make a file no read can open; a bridge that looks at it will say so."""
        path.chmod(0)
        self.addCleanup(lambda: path.exists() and path.chmod(0o644))

    def test_a_held_letter_is_not_reopened_inside_the_recheck_window(self):
        self.bootstrap()
        first = self.local_letter("first")
        self.tick(self.trey)
        marker = self.marker(self.trey, first)
        self.assertTrue(marker.is_file())
        self.assertIsNotNone(self.record(first))
        archive = self.trey.root / "archive" / (first + ".mail")
        self.unreadable(archive)
        # Inside the window the letter is recognised from the listing: a
        # re-read of this file would fail and be reported.
        self.tick(self.trey)
        self.assertEqual(self.actions(self.trey, "outbound_ignored", first), [])
        health = self.health(self.trey)
        self.assertEqual(health["local_held"]["holds"], 1)
        self.assertEqual(health["attention"], [])
        # A new letter in the same condition is still judged at once.
        second = self.local_letter("second")
        self.tick(self.trey)
        self.assertTrue(self.marker(self.trey, second).is_file())
        self.assertIsNotNone(self.record(second))
        self.assertEqual(self.health(self.trey)["local_held"]["holds"], 2)
        # Control: a window of 0 re-reads it, so the probe can see a re-read.
        self.tick(self.trey, BRIDGE_DECIDED_RECHECK_SECONDS=0)
        self.assertEqual(len(self.actions(self.trey, "outbound_ignored", first)), 1)
        archive.chmod(0o644)
        # Past the window the marker is not trusted: the letter is verified
        # again and its marker refreshed.
        old = time.time() - PAST_THE_WINDOW
        os.utime(marker, (old, old))
        self.tick(self.trey)
        self.assertGreater(marker.stat().st_mtime, time.time() - 600)
        self.assertEqual(self.health(self.trey)["attention"], [])
        for mail_id in (first, second):
            self.assertFalse(self.exported(self.trey, mail_id), mail_id)

    def test_a_crash_between_the_hold_and_its_marker_redoes_the_work(self):
        self.bootstrap()
        for hook in ("decided-d0-before-marker", "decided-d1-marked"):
            with self.subTest(hook=hook):
                letter = self.local_letter(f"crash at {hook}")
                crashed = self.tick(self.trey, expect=None, BRIDGE_CRASH_AFTER=hook)
                self.assertEqual(
                    crashed.returncode, -signal.SIGKILL, crashed.stdout + crashed.stderr
                )
                # The guard's record is written before the marker; the marker
                # is what a crash in between loses, never the decision.
                self.assertIsNotNone(self.record(letter))
                self.assertEqual(
                    self.marker(self.trey, letter).exists(), hook == "decided-d1-marked"
                )
                self.tick(self.trey)
                self.assertTrue(self.marker(self.trey, letter).is_file())
                self.assertFalse(self.exported(self.trey, letter))
                self.assertEqual(self.health(self.trey)["attention"], [])

    def test_a_store_fault_marks_nothing_so_the_letter_is_judged_after_repair(self):
        self.bootstrap()
        first = self.local_letter("first")
        self.tick(self.trey)
        bridge = self.trey.root / "bridge"
        archive = (self.trey.root / "archive" / (first + ".mail")).read_bytes()
        manifest = Path(self.temporary.name) / "manifest.jsonl"
        manifest.write_text(
            json.dumps(
                {
                    "host": "trey",
                    "id": first,
                    "room": "atlasos",
                    "archive_sha256": sha(archive),
                    "local_copies": [f"atlasos/inbox/{first}.mail"],
                    "bridge_markers": [],
                },
                separators=(",", ":"),
            )
            + "\n"
        )
        for name in ("local-held", "local-held-manifests", "local-held-index.txt"):
            path = bridge / name
            if path.is_dir() and not path.is_symlink():
                for child in sorted(path.iterdir()):
                    child.unlink()
                path.rmdir()
            elif path.exists() or path.is_symlink():
                path.unlink()
        second = self.local_letter("second, met during the fault")
        self.tick(self.trey, expect=1)
        self.assertEqual(self.health(self.trey)["reason"], "local_held_store_fault")
        self.assertFalse(self.marker(self.trey, second).exists())
        self.assertIsNone(self.record(second))
        self.assertFalse(self.exported(self.trey, second))
        # Repair: the manifest restamps the first hold, and the loss of the
        # second (never recorded) is not one.
        seeded = subprocess.run(
            [sys.executable, str(SWEEP), "--seed-local-holds", str(manifest),
             "--accept-lost-records"],
            env=self.trey.env(), capture_output=True, text=True, timeout=60, check=False,
        )
        self.assertEqual(seeded.returncode, 0, seeded.stdout + seeded.stderr)
        self.tick(self.trey)
        # Had the fault tick marked the second letter decided, it would be
        # skipped now and never recorded.
        self.assertIsNotNone(self.record(second))
        self.assertTrue(self.marker(self.trey, second).is_file())
        self.assertEqual(self.health(self.trey)["local_held"]["holds"], 2)
        for mail_id in (first, second):
            self.assertFalse(self.exported(self.trey, mail_id), mail_id)

    def test_an_unrelayable_letter_is_reread_only_when_its_file_changes(self):
        self.bootstrap()
        archive = self.fc.root / "archive"
        archive.mkdir(exist_ok=True)
        mail_id = fixed_id(0x7E01)
        path = archive / (mail_id + ".mail")
        path.write_bytes(craft_mail(mail_id, "garden", "hq", kind="bogus"))
        self.tick(self.fc)
        self.assertEqual(len(self.actions(self.fc, "outbound_ignored", mail_id)), 1)
        self.assertTrue(self.marker(self.fc, mail_id, "unrelayable").is_file())
        self.assertEqual([i["id"] for i in self.health(self.fc)["attention"]], [mail_id])
        # An unchanged file is not read again: the verdict stands, is still
        # listed, and is not logged a second time.
        self.unreadable(path)
        self.tick(self.fc)
        self.assertEqual(len(self.actions(self.fc, "outbound_ignored", mail_id)), 1)
        self.assertEqual([i["id"] for i in self.health(self.fc)["attention"]], [mail_id])
        path.chmod(0o644)
        # A changed file is judged afresh: mended, it is relayed and unlisted.
        path.write_bytes(craft_mail(mail_id, "garden", "hq", body=b"mended, and longer\n"))
        self.tick(self.fc)
        self.assertTrue(self.exported(self.fc, mail_id))
        self.assertEqual(self.health(self.fc)["attention"], [])

    def test_a_transient_read_failure_is_never_remembered_as_a_verdict(self):
        self.bootstrap()
        archive = self.fc.root / "archive"
        archive.mkdir(exist_ok=True)
        mail_id = fixed_id(0x7E02)
        path = archive / (mail_id + ".mail")
        path.write_bytes(craft_mail(mail_id, "garden", "hq", body=b"fine\n"))
        self.unreadable(path)
        self.tick(self.fc)
        self.assertEqual(len(self.actions(self.fc, "outbound_ignored", mail_id)), 1)
        self.assertFalse(self.marker(self.fc, mail_id, "unrelayable").exists())
        path.chmod(0o644)
        self.tick(self.fc)  # the file is fine: it is relayed
        self.assertTrue(self.exported(self.fc, mail_id))
        self.assertEqual(self.health(self.fc)["attention"], [])


class ChannelDecidedTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        TerminalFixture.setUpClass()

    def setUp(self):
        self.temporary = CanonicalTemporaryDirectory(prefix="post-channels-decided-")
        self.addCleanup(self.temporary.cleanup)
        self.topology = ChannelTopology(self.temporary.name)
        self.alpha = self.topology.add("alpha", ["alice"])
        self.log = Records()
        self.deadline = Deadline()

    def publish(self, recheck=21600, tracked=None):
        settings = types.SimpleNamespace(
            root=self.alpha.root,
            repo=self.alpha.repo,
            host=self.alpha.host,
            max_mail_bytes=1024 * 1024,
            decided_recheck_seconds=recheck,
        )
        return channels.publish_channels(
            settings, ALL, local_snapshot(self.alpha), self.log, self.deadline,
            tracked=tracked,
        )

    def messages(self, name):
        return sorted((self.alpha.root / "channels" / name / "messages").glob("*.msg"))

    def unreadable(self, paths):
        for path in paths:
            path.chmod(0)
        self.addCleanup(lambda: [p.chmod(0o644) for p in paths if p.exists()])

    def test_a_message_already_committed_is_skipped_unopened(self):
        self.alpha.join("c1", "alice")
        self.alpha.send("c1", "alice", "hello")
        first = self.publish()
        self.assertGreaterEqual(first.published, 1)
        self.alpha.commit("publish c1")
        tracked = SWEEPER.committed_channel_messages(Git(self.alpha.repo))
        files = self.messages("c1")
        self.assertTrue(files)
        for path in files:
            self.assertIn(f"channels/c1/messages/{path.name}", tracked)
        self.unreadable(files)
        # Control: without the committed set every file is opened, and an
        # unreadable one is reported, so the probe can see a re-read.
        self.assertEqual(self.publish().unpublishable, len(files))
        skipped = self.publish(tracked=tracked)
        self.assertEqual((skipped.unpublishable, skipped.published), (0, 0))

    def test_a_message_this_host_never_publishes_is_reread_only_after_the_window(self):
        root = self.alpha.root / "channels" / "c2"
        (root / "messages").mkdir(parents=True)
        (root / "channel.json").write_text(json.dumps(channel_record("c2", "alice")) + "\n")
        strange, received = channel_id(701), channel_id(702)
        # Authored under a name that is not a local room: never published.
        (root / "messages" / (strange + ".msg")).write_bytes(
            channel_message(strange, "stranger", "c2")
        )
        # Imported from a peer (its received marker exists): never republished.
        (root / "messages" / (received + ".msg")).write_bytes(
            channel_message(received, "alice", "c2")
        )
        marker = self.alpha.root / "bridge" / "chan-received" / "c2" / received
        marker.parent.mkdir(parents=True)
        marker.write_text("beta " + sha(b"x") + "\n")
        self.assertEqual(self.publish().published, 0)
        decided = self.alpha.root / "bridge" / "chan-decided" / "c2"
        self.assertEqual(sorted(path.name for path in decided.iterdir()), [strange, received])
        files = self.messages("c2")
        self.unreadable(files)
        self.assertEqual(self.publish().unpublishable, 0)  # inside the window
        self.assertEqual(self.publish(recheck=0).unpublishable, len(files))  # control
        for path in files:
            path.chmod(0o644)
        old = time.time() - PAST_THE_WINDOW
        for path in decided.iterdir():
            os.utime(path, (old, old))
        self.publish()
        for path in decided.iterdir():
            self.assertGreater(path.stat().st_mtime, time.time() - 600)


if __name__ == "__main__":
    unittest.main()
