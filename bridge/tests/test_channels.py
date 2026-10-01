"""Behavioral tests for the SPEC-v2 r5.1 channel state machine."""

import fcntl
import hashlib
import json
import os
import sys
import threading
import time
import unittest
from dataclasses import replace
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))

from bridgelib import channels, common, tick  # noqa: E402
from bridgelib.snapshot import Contest, Owner, empty_snapshot  # noqa: E402

from .harness_channels import (  # noqa: E402
    POST,
    Deadline,
    Git,
    Records,
    Topology,
    channel_id,
    channel_message,
    channel_record,
    snapshot_for,
)
from .test_sweep import PINNED_POST_VERSION, SWEEPER, CanonicalTemporaryDirectory

ALL = channels.ChannelsConfig("all", frozenset(), frozenset())


def write_peer_channel(machine, name, record, messages):
    root = machine.repo / "channels" / name
    (root / "messages").mkdir(parents=True, exist_ok=True)
    (root / "channel.json").write_text(
        json.dumps(record, sort_keys=True, indent=2) + "\n", encoding="utf-8"
    )
    for message_id, data in messages.items():
        (root / "messages" / (message_id + ".msg")).write_bytes(data)
    return machine.commit(f"publish {name}")


def local_snapshot(machine):
    return replace(
        empty_snapshot(machine.host),
        real_rooms={name: str(path) for name, path in machine.workspaces.items()},
    )


class ConfigTest(unittest.TestCase):
    def test_roomless_host_stamp_has_stable_bytes(self):
        message_id = channel_id(888)
        envelope = {
            "id": message_id,
            "from": "shell-fixed",
            "from_participant": "shell-fixed",
            "channel": "wire",
            "display_name": "Rémy",
            "subject": "hello 🚀",
            "sent": "2026-09-25 12:00:00 +0000",
        }
        original = json.dumps(envelope, separators=(",", ":")).encode() + b"\n---\nbody\nline"
        expected = (
            b'{\n  "channel": "wire",\n  "display_name": "R\\u00e9my",\n'
            b'  "from": "shell-fixed",\n'
            b'  "from_host": "trey",\n  "from_participant": "shell-fixed",\n'
            + f'  "id": "{message_id}",\n'.encode()
            + b'  "sent": "2026-09-25 12:00:00 +0000",\n'
            + b'  "subject": "hello \\ud83d\\ude80"\n}'
            + b"\n---\nbody\nline"
        )
        self.assertEqual(channels._stamp_roomless_host(original, envelope, "trey"), expected)

    def test_channels_and_tests_share_one_common_module_identity(self):
        self.assertEqual(
            [name for name in sys.modules if name.endswith("bridgelib.common")],
            ["bridgelib.common"],
        )

    def test_parse_channels_config_accepts_two_contract_shapes(self):
        # An explicit null is the opt-out; the absent-key default lives in
        # sweep.load_config (SPEC-v2 r6.2); test_tick_v2 covers absent and null.
        self.assertIsNone(channels.parse_channels_config(None))
        all_cfg = channels.parse_channels_config(
            {"mode": "all", "deny": ["devbox-build"]}
        )
        self.assertEqual(
            all_cfg,
            channels.ChannelsConfig("all", frozenset(), frozenset({"devbox-build"})),
        )
        allow_cfg = channels.parse_channels_config(
            {"mode": "allow", "allow": ["front-porch", "lamp"]}
        )
        self.assertEqual(allow_cfg.allow, frozenset({"front-porch", "lamp"}))
        self.assertTrue(channels.channel_allowed(allow_cfg, "lamp"))
        self.assertFalse(channels.channel_allowed(allow_cfg, "elsewhere"))

    def test_parse_channels_config_refuses_every_invalid_family(self):
        cases = (
            ([], "object or null"),
            ({}, "mode"),
            ({"mode": "sometimes"}, "'all' or 'allow'"),
            ({"mode": "allow"}, "required"),
            ({"mode": "all", "deny": "x"}, "must be a list"),
            ({"mode": "allow", "allow": "x"}, "must be a list"),
            ({"mode": "all", "extra": []}, "unknown keys"),
            ({"mode": "all", "deny": ["channels"]}, "denied"),
            ({"mode": "all", "deny": ["bad/name"]}, "path-safe"),
            ({"mode": "all", "deny": [1]}, "not a string"),
        )
        for value, phrase in cases:
            with (
                self.subTest(value=value),
                self.assertRaisesRegex(common.ConfigError, phrase),
            ):
                channels.parse_channels_config(value)


class ChannelIntegrationTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        version = subprocess_run([POST, "--version"])
        if not SWEEPER.post_version_accepted(version.stdout):
            raise RuntimeError(
                f"tests require {PINNED_POST_VERSION}, got {version.stdout!r}"
            )

    def setUp(self):
        self.temporary = CanonicalTemporaryDirectory(prefix="post-channels-")
        self.topology = Topology(self.temporary.name)
        self.alpha = self.topology.add("alpha", ["alice"])
        self.beta = self.topology.add("beta", ["bob"])
        self.log = Records()
        self.deadline = Deadline()

    def tearDown(self):
        self.temporary.cleanup()

    def publish(self, machine):
        stats = channels.publish_channels(
            machine.settings, ALL, local_snapshot(machine), self.log, self.deadline
        )
        machine.commit("bridge publishes channels")
        return stats

    def alpha_snapshot(
        self, *, legacy=False, placeholders=None, routes=None, post_rooms=None
    ):
        return snapshot_for(
            self.beta,
            ["alpha"],
            published={"alpha": frozenset({"alice"})},
            v2_peers=() if legacy else ("alpha",),
            placeholders=placeholders or {"alice": "alpha"},
            routes=routes or {"alice": "alpha"},
            post_rooms=post_rooms,
        )

    def import_alpha(self, snapshot=None, fence=lambda: False):
        snapshot = snapshot or self.alpha_snapshot()
        return channels.import_channels(
            self.beta.settings,
            ALL,
            Git(self.beta.repo),
            snapshot,
            self.log,
            self.deadline,
            fence,
        )

    def test_shim_copy_adoption_keeps_four_differences_divergent(self):
        name = "shim-negative"
        sender = "shell-fixed"
        variants = ("body", "subject", "extra_header", "other_reservation")
        remote = {}
        local = {}
        reservations = {}
        for index, variant in enumerate(variants):
            message_id = channel_id(900 + index)
            original = channel_message(
                message_id, sender, name, from_participant=sender
            )
            envelope = json.loads(original.split(b"\n---\n", 1)[0])
            stamped = channels._stamp_roomless_host(original, envelope, "alpha")
            remote[message_id] = stamped
            local[message_id] = {
                "body": channel_message(
                    message_id, sender, name, body="changed", from_participant=sender
                ),
                "subject": channel_message(
                    message_id, sender, name, subject="changed", from_participant=sender
                ),
                "extra_header": channel_message(
                    message_id, sender, name, extra_header="one", from_participant=sender
                ),
                "other_reservation": original,
            }[variant]
            reservations[message_id] = (
                f"gamma {hashlib.sha256(stamped).hexdigest()}"
                if variant == "other_reservation" else None
            )

        write_peer_channel(
            self.alpha, name, channel_record(name, "alice"), remote
        )
        peer_channel = self.alpha.repo / "channels" / name
        local_channel = self.beta.root / "channels" / name
        (local_channel / "messages").mkdir(parents=True)
        (local_channel / "channel.json").write_bytes(
            (peer_channel / "channel.json").read_bytes()
        )
        for index, variant in enumerate(variants):
            message_id = channel_id(900 + index)
            copy = local[message_id]
            (local_channel / "messages" / f"{message_id}.msg").write_bytes(copy)
            reservation = reservations[message_id]
            if reservation is not None:
                marker = (
                    self.beta.root / "bridge" / "chan-received" / name / message_id
                )
                marker.parent.mkdir(parents=True, exist_ok=True)
                marker.write_text(reservation + "\n")

        stats = self.import_alpha()
        diverged = {item["id"] for item in stats.diverged}
        for index, variant in enumerate(variants):
            message_id = channel_id(900 + index)
            with self.subTest(variant=variant):
                self.assertIn(message_id, diverged)
                self.assertEqual(
                    (local_channel / "messages" / f"{message_id}.msg").read_bytes(),
                    local[message_id],
                )
        self.assertEqual(len(diverged), len(variants))

    def test_backfill_200_messages_two_channels_real_post_and_cursor_invariance(self):
        self.beta.join("cursor-seed", "bob")
        self.beta.send("cursor-seed", "bob", "seed cursor")
        self.beta.post(
            "chat",
            "cursor-seed",
            "--limit",
            "1",
            cwd=self.beta.workspaces["bob"],
        )
        self.alpha.join("front-porch", "alice", "front")
        self.alpha.join("lamp", "alice", "lamp")
        for number in range(100):
            self.alpha.send("front-porch", "alice", f"front {number}")
            self.alpha.send("lamp", "alice", f"lamp {number}")
        cursors_before = {
            path.relative_to(self.beta.root): path.read_bytes()
            for path in self.beta.root.rglob("cursors.json")
        }
        published = self.publish(self.alpha)
        # The requested backfill fixture has no registered remote member yet;
        # its historical join is therefore pending and members.json stays {}.
        imported = self.import_alpha(replace(self.alpha_snapshot(), placeholders={}))
        self.assertEqual(published.published, 202)
        self.assertEqual(imported.imported, 202)
        for name in ("front-porch", "lamp"):
            source = self.alpha.root / "channels" / name
            target = self.beta.root / "channels" / name
            self.assertEqual((target / "members.json").read_bytes(), b"{}\n")
            source_files = sorted((source / "messages").glob("*.msg"))
            target_files = sorted((target / "messages").glob("*.msg"))
            self.assertEqual(
                [path.name for path in source_files],
                [path.name for path in target_files],
            )
            self.assertEqual(
                [path.read_bytes() for path in source_files],
                [path.read_bytes() for path in target_files],
            )
            self.beta.join(name, "bob")
            history = self.beta.post(
                "chat",
                name,
                "--history",
                "5",
                "--json",
                cwd=self.beta.workspaces["bob"],
            )
            self.assertEqual(len(json.loads(history.stdout)["messages"]), 5)
        self.assertEqual(
            cursors_before,
            {
                path.relative_to(self.beta.root): path.read_bytes()
                for path in self.beta.root.rglob("cursors.json")
            },
        )

    def test_round_trip_join_membership_and_second_import_noop(self):
        self.alpha.join("porch", "alice")
        self.publish(self.alpha)
        self.import_alpha()
        self.beta.join("porch", "bob")
        self.beta.send("porch", "bob", "hello alpha")
        beta_published = self.publish(self.beta)
        self.assertEqual(beta_published.published, 2)
        snapshot = snapshot_for(
            self.alpha,
            ["beta"],
            published={"beta": frozenset({"bob"})},
            v2_peers=("beta",),
            placeholders={"bob": "beta"},
            routes={"bob": "beta"},
        )
        first = channels.import_channels(
            self.alpha.settings,
            ALL,
            Git(self.alpha.repo),
            snapshot,
            self.log,
            self.deadline,
            lambda: False,
        )
        self.assertEqual(first.imported, 2)
        members_path = self.alpha.root / "channels" / "porch" / "members.json"
        members = json.loads(members_path.read_text())
        # beta's messages/ also holds alice's imported join, and glob order is
        # filesystem order: select bob's join by sender, not by position.
        joins = [
            json.loads(path.read_bytes().split(b"\n---\n", 1)[0])
            for path in (self.beta.root / "channels" / "porch" / "messages").glob(
                "*.msg"
            )
            if b'"event": "join"' in path.read_bytes()
        ]
        self.assertEqual(sorted(item["from"] for item in joins), ["alice", "bob"])
        join = next(item for item in joins if item["from"] == "bob")
        self.assertEqual(members["bob"], join["sent"])
        mtimes = {
            path: path.stat().st_mtime_ns
            for path in self.alpha.root.rglob("*")
            if path.is_file()
        }
        self.log.items.clear()
        second = channels.import_channels(
            self.alpha.settings,
            ALL,
            Git(self.alpha.repo),
            snapshot,
            self.log,
            self.deadline,
            lambda: False,
        )
        self.assertEqual(second.imported, 0)
        self.assertEqual(mtimes, {path: path.stat().st_mtime_ns for path in mtimes})
        self.assertEqual(self.log.items, [])

    def test_completed_host_tip_bounds_steady_state_message_reads(self):
        message_id = channel_id(9)
        write_peer_channel(
            self.alpha,
            "bounded",
            channel_record("bounded", "alice"),
            {message_id: channel_message(message_id, "alice", "bounded")},
        )
        snapshot = self.alpha_snapshot()
        first_git = Git(self.beta.repo)
        first = channels.import_channels(
            self.beta.settings,
            ALL,
            first_git,
            snapshot,
            self.log,
            self.deadline,
            lambda: False,
        )
        self.assertEqual(first.completed, frozenset({"alpha"}))
        channels.advance_tip(
            self.beta.settings, "alpha", snapshot.oids["machines/alpha"]
        )
        second_git = Git(self.beta.repo)

        second = channels.import_channels(
            self.beta.settings,
            ALL,
            second_git,
            snapshot,
            self.log,
            self.deadline,
            lambda: False,
        )

        self.assertEqual(second.imported, 0)
        message_shows = [
            call
            for call in second_git.calls
            if call and call[0] == "show" and "/messages/" in call[-1]
        ]
        self.assertEqual(message_shows, [])

    def tick_channels(self, snapshot):
        """Import as the sweeper does: advance the tip for completed hosts."""
        stats = self.import_alpha(snapshot)
        for host in sorted(stats.completed):
            channels.advance_tip(
                self.beta.settings, host, snapshot.oids[f"machines/{host}"]
            )
        return stats

    def test_unreadable_local_members_holds_the_tip_until_repair(self):
        # M1: a skipped channel must keep its host out of `completed`, or the
        # tip moves past messages the bounded delta will never list again.
        first, second = channel_id(720), channel_id(721)
        write_peer_channel(
            self.alpha,
            "fixed",
            channel_record("fixed", "alice"),
            {first: channel_message(first, "alice", "fixed")},
        )
        self.assertEqual(self.tick_channels(self.alpha_snapshot()).imported, 1)
        tip_path = self.beta.root / "bridge" / "chan-tip" / "alpha"
        tip = tip_path.read_bytes()
        members = self.beta.root / "channels" / "fixed" / "members.json"
        members.write_text("not json\n")
        write_peer_channel(
            self.alpha,
            "fixed",
            channel_record("fixed", "alice"),
            {second: channel_message(second, "alice", "fixed")},
        )

        held = self.tick_channels(self.alpha_snapshot())

        self.assertNotIn("alpha", held.completed)
        self.assertEqual(held.imported, 0)
        self.assertEqual(tip_path.read_bytes(), tip)
        message = self.beta.root / "channels" / "fixed" / "messages" / (second + ".msg")
        self.assertFalse(message.exists())

        members.write_text("{}\n")
        repaired = self.tick_channels(self.alpha_snapshot())
        self.assertEqual(repaired.imported, 1)
        self.assertIn("alpha", repaired.completed)
        self.assertTrue(message.is_file())
        self.assertEqual(self.tick_channels(self.alpha_snapshot()).imported, 0)

    def test_failed_channel_listing_is_logged_and_holds_the_tip(self):
        # m6: a nonzero `git ls-tree` of the peer's channels/ is a failure,
        # not an empty tree. The host is not completed, the tip stays put,
        # and the next tick imports the message exactly once.
        message_id = channel_id(730)
        write_peer_channel(
            self.alpha,
            "listed",
            channel_record("listed", "alice"),
            {message_id: channel_message(message_id, "alice", "listed")},
        )
        real_ls_tree = Git.ls_tree
        fired = []

        def failing_ls_tree(git, ref, prefix):
            if prefix == "channels/":
                fired.append(ref)
                raise common.GitReadError(ref, prefix, "fatal: simulated failure")
            return real_ls_tree(git, ref, prefix)

        with mock.patch.object(Git, "ls_tree", failing_ls_tree):
            held = self.tick_channels(self.alpha_snapshot())
        self.assertTrue(fired, "listing was never attempted")
        self.assertNotIn("alpha", held.completed)
        self.assertEqual(held.imported, 0)
        self.assertFalse((self.beta.root / "bridge" / "chan-tip" / "alpha").exists())
        failures = self.log.actions("git_failed")
        self.assertEqual(
            [(item["host"], item["ref"], item["path"]) for item in failures],
            [("alpha", fired[0], "channels/")],
        )

        repaired = self.tick_channels(self.alpha_snapshot())
        self.assertEqual(repaired.imported, 1)
        self.assertIn("alpha", repaired.completed)
        self.assertEqual(self.tick_channels(self.alpha_snapshot()).imported, 0)

    def page_until_walk_ends(self, snapshot, limit=10):
        """Tick with a small page until the walk's cursor clears."""
        cursor_path = self.beta.root / "bridge" / "chan-page" / "alpha"
        ticks = []
        for _ in range(limit):
            ticks.append(self.tick_channels(snapshot))
            if not cursor_path.exists():
                return ticks
        self.fail("paged walk never ended")

    def assert_early_skip_survives_paging(self, early_channel, early_id, stamp, repair,
                                          snapshot, repaired_snapshot):
        # `snapshot`/`repaired_snapshot` are factories: a snapshot pins the
        # peer oid when built, so it must be built after the fixture push.
        # r5.6 (P1): 60 later messages page at 20 per tick; the early entry
        # is skipped on page 1 and then sits behind the cursor.
        later = [channel_id(900 + number) for number in range(60)]
        write_peer_channel(
            self.alpha,
            "paged",
            channel_record("paged", "alice"),
            {message_id: channel_message(message_id, "alice", "paged")
             for message_id in later},
        )
        messages = self.beta.root / "channels"
        tip_path = self.beta.root / "bridge" / "chan-tip" / "alpha"
        early_path = messages / early_channel / "messages" / (early_id + ".msg")
        with mock.patch.object(channels, "CHANNEL_TREE_MAX", 20):
            walk = self.page_until_walk_ends(snapshot())
            self.assertGreater(len(walk), 2)  # precondition: it really paged
            self.assertFalse(early_path.exists())
            self.assertTrue(
                (messages / "paged" / "messages" / (later[-1] + ".msg")).is_file()
            )
            self.assertNotIn("alpha", walk[-1].completed)
            self.assertFalse(tip_path.exists())
            self.assertTrue(stamp.is_file(), "hold stamp lost by the paged walk")
            self.assertEqual(walk[-1].held.get("alpha", {}).get("count"), 1)
            # A second walk re-holds it and keeps the first-seen time.
            first_stamp = stamp.read_bytes()
            time.sleep(1.1)
            self.page_until_walk_ends(snapshot())
            self.assertEqual(stamp.read_bytes(), first_stamp)
            self.assertFalse(early_path.exists())

            repair()
            imported = sum(
                tick.imported for tick in self.page_until_walk_ends(repaired_snapshot())
            )
        self.assertEqual(imported, 1)
        self.assertTrue(early_path.is_file())
        self.assertTrue(tip_path.is_file())
        self.assertFalse(stamp.exists())
        self.assertEqual(
            [item["id"] for item in self.log.actions("chan_imported")].count(early_id),
            1,
        )

    def test_paged_walk_keeps_an_early_held_sender_until_repair(self):
        held_id = channel_id(880)
        write_peer_channel(
            self.alpha,
            "aaa-held",
            channel_record("aaa-held", "alice"),
            {held_id: channel_message(held_id, "dora", "aaa-held")},
        )
        homes = {"alice": "alpha", "dora": "alpha"}

        def snapshot(post_rooms):
            return snapshot_for(
                self.beta,
                ["alpha"],
                published={"alpha": frozenset(homes)},
                v2_peers=("alpha",),
                placeholders=homes,
                routes=homes,
                post_rooms=post_rooms,
            )

        self.assert_early_skip_survives_paging(
            "aaa-held",
            held_id,
            self.beta.root / "bridge" / "chan-held" / "alpha" / "aaa-held" / held_id,
            lambda: None,
            lambda: snapshot({"alice": str(self.beta.root / "remote" / "alpha" / "alice")}),
            lambda: snapshot(None),
        )

    def test_paged_walk_keeps_an_early_unreadable_members_skip_until_repair(self):
        early_id = channel_id(881)
        write_peer_channel(
            self.alpha,
            "aaa-members",
            channel_record("aaa-members", "alice"),
            {early_id: channel_message(early_id, "alice", "aaa-members")},
        )
        members = self.beta.root / "channels" / "aaa-members" / "members.json"
        members.parent.mkdir(parents=True)
        members.write_text("not json\n")
        self.assert_early_skip_survives_paging(
            "aaa-members",
            early_id,
            self.beta.root / "bridge" / "chan-held" / "alpha" / "aaa-members" / early_id,
            lambda: members.write_text("{}\n"),
            self.alpha_snapshot,
            self.alpha_snapshot,
        )

    def test_hold_summary_survives_a_failed_walk_and_drops_non_peers(self):
        # r5.6 (P2): the summary is the stamps on disk. A host whose listing
        # failed keeps its count and age; a non-peer's stamps are dropped.
        held_id = channel_id(882)
        write_peer_channel(
            self.alpha,
            "waiting",
            channel_record("waiting", "alice"),
            {held_id: channel_message(held_id, "alice", "waiting")},
        )
        ghost = self.beta.root / "bridge" / "chan-held" / "ghost" / "old" / held_id
        ghost.parent.mkdir(parents=True)
        ghost.write_text("2026-01-01T00:00:00+00:00\n")
        first = self.tick_channels(self.alpha_snapshot(post_rooms={}))
        self.assertEqual(first.held["alpha"]["count"], 1)
        self.assertNotIn("ghost", first.held)
        self.assertFalse(ghost.parent.parent.exists())

        real_ls_tree = Git.ls_tree

        def failing_ls_tree(git, ref, prefix):
            if prefix == "channels/":
                raise common.GitReadError(ref, prefix, "fatal: simulated failure")
            return real_ls_tree(git, ref, prefix)

        with mock.patch.object(Git, "ls_tree", failing_ls_tree):
            failed = self.tick_channels(self.alpha_snapshot(post_rooms={}))
        self.assertEqual(self.log.actions("git_failed")[-1]["host"], "alpha")
        self.assertIn("alpha", failed.held)
        self.assertEqual(failed.held["alpha"]["count"], 1)
        self.assertGreaterEqual(
            failed.held["alpha"]["oldest_age_seconds"],
            first.held["alpha"]["oldest_age_seconds"],
        )

    def test_hold_summary_keeps_stamps_when_peers_are_not_known(self):
        # Hardening after r5.6: with no registry and an empty config.peers the
        # peer set is unknown, not empty. Stamps survive and stay reported.
        ghost = self.beta.root / "bridge" / "chan-held" / "ghost" / "old" / channel_id(883)
        ghost.parent.mkdir(parents=True)
        ghost.write_text("2026-01-01T00:00:00+00:00\n")
        unknown = replace(self.alpha_snapshot(post_rooms={}), peers_known=False)
        stats = self.tick_channels(unknown)
        self.assertTrue(ghost.exists(), "chan-held stamp dropped (peers unknown)")
        self.assertEqual(stats.held["ghost"]["count"], 1)

    def test_first_sight_pages_a_large_tree_across_ticks(self):
        self.assertEqual(channels.CHANNEL_TREE_MAX, 50000)
        total = 60
        messages = {}
        for number in range(total):
            message_id = channel_id(700 + number)
            messages[message_id] = channel_message(message_id, "alice", "paged")
        write_peer_channel(
            self.alpha, "paged", channel_record("paged", "alice"), messages
        )
        snapshot = self.alpha_snapshot()
        tip_path = self.beta.root / "bridge" / "chan-tip" / "alpha"

        with mock.patch.object(channels, "CHANNEL_TREE_MAX", 50):
            first = self.tick_channels(snapshot)
            self.assertEqual(first.imported, 49)
            self.assertNotIn("alpha", first.completed)
            self.assertFalse(tip_path.exists())
            paged = self.log.actions("chan_tree_paged")
            self.assertEqual(len(paged), 1)
            self.assertEqual(paged[0]["host"], "alpha")
            self.assertEqual(paged[0]["processed"], 50)
            self.assertEqual(paged[0]["remaining"], 11)

            second = self.tick_channels(snapshot)

        self.assertEqual(second.imported, 11)
        self.assertIn("alpha", second.completed)
        self.assertTrue(tip_path.is_file())
        landed = sorted(
            item.name
            for item in (
                self.beta.root / "channels" / "paged" / "messages"
            ).glob("*.msg")
        )
        self.assertEqual(len(landed), total)
        self.assertEqual(self.log.actions("chan_tree_oversize"), [])

    def test_cursor_without_skipped_key_holds_the_walk(self):
        # Hardening after r5.6: a pre-r5.6 three-key cursor cannot say whether
        # an earlier page skipped something, so it reads as skipped=True. The
        # resumed walk then completes nothing and the tip stays; the next walk
        # starts at the old tip and completes. Reading it as False would let a
        # walk whose early pages held entries move the tip past them.
        messages = {
            channel_id(760 + number): channel_message(channel_id(760 + number), "alice", "legacy")
            for number in range(60)
        }
        write_peer_channel(self.alpha, "legacy", channel_record("legacy", "alice"), messages)
        snapshot = self.alpha_snapshot()
        cursor_path = self.beta.root / "bridge" / "chan-page" / "alpha"
        tip_path = self.beta.root / "bridge" / "chan-tip" / "alpha"
        with mock.patch.object(channels, "CHANNEL_TREE_MAX", 50):
            self.tick_channels(snapshot)
            cursor = json.loads(cursor_path.read_text())
            self.assertIs(cursor.pop("skipped"), False)  # precondition: a clean page
            cursor_path.write_text(json.dumps(cursor, sort_keys=True) + "\n")
            legacy = channels._read_page_cursor(
                self.beta.settings, "alpha", cursor["tip"], self.log
            )
            self.assertIsNotNone(legacy, "three-key cursor rejected as malformed")
            self.assertIs(legacy.skipped, True, "three-key cursor read as skipped=False")
            resumed = self.tick_channels(snapshot)
            self.assertFalse(cursor_path.exists())  # precondition: the walk ended
            self.assertNotIn("alpha", resumed.completed, "legacy cursor completed the walk")
            self.assertFalse(tip_path.exists())
            again = self.page_until_walk_ends(snapshot)
        self.assertIn("alpha", again[-1].completed)
        self.assertTrue(tip_path.is_file())
        landed = list((self.beta.root / "channels" / "legacy" / "messages").glob("*.msg"))
        self.assertEqual(len(landed), 60)

    def test_completed_excludes_a_host_whose_channel_was_skipped(self):
        """A recoverable skip must keep the host out of ``completed``.

        SPEC-v2 §Bounded import: the tick advances ``chan-tip/<H>`` for
        exactly the hosts in ``completed``, and bounded import afterwards
        only lists ``A`` rows in ``<tip>..<oid>``. A host whose record was
        rejected this tick therefore loses every message under that channel
        forever if it is reported as completed.
        """
        good_id = channel_id(31)
        broken_id = channel_id(32)
        write_peer_channel(
            self.alpha,
            "good",
            channel_record("good", "alice"),
            {good_id: channel_message(good_id, "alice", "good")},
        )
        write_peer_channel(
            self.alpha,
            "broken",
            {**channel_record("broken", "alice"), "name": "not-broken"},
            {broken_id: channel_message(broken_id, "alice", "broken")},
        )

        stats = self.import_alpha()

        imported_good = (
            self.beta.root / "channels" / "good" / "messages" / (good_id + ".msg")
        )
        imported_broken = (
            self.beta.root / "channels" / "broken" / "messages" / (broken_id + ".msg")
        )
        self.assertTrue(imported_good.is_file(), "the valid channel must still import")
        self.assertFalse(imported_broken.exists())
        self.assertEqual(
            [record["reason"] for record in self.log.actions("chan_record_invalid")],
            ["channel record name does not match path"],
        )
        self.assertNotIn(
            "alpha",
            stats.completed,
            "a host with a rejected record must not be reported completed",
        )

        write_peer_channel(
            self.alpha,
            "broken",
            channel_record("broken", "alice"),
            {broken_id: channel_message(broken_id, "alice", "broken")},
        )

        repaired = self.import_alpha()

        self.assertTrue(imported_broken.is_file())
        self.assertIn("alpha", repaired.completed)

    def test_hostile_junk_tree_stays_bounded_per_tick(self):
        self.alpha.write_many_relay_entries(100000)
        snapshot = self.alpha_snapshot()
        started = time.monotonic()

        stats = self.import_alpha(snapshot)

        self.assertLess(time.monotonic() - started, 60.0)
        self.assertEqual(stats.completed, frozenset())
        self.assertEqual(self.log.actions("chan_tree_oversize"), [])
        paged = self.log.actions("chan_tree_paged")
        self.assertEqual(len(paged), 1)
        self.assertEqual(paged[0]["processed"], channels.CHANNEL_TREE_MAX)
        self.assertEqual(paged[0]["remaining"], 100000 - channels.CHANNEL_TREE_MAX)
        self.assertEqual(len(self.log.actions("chan_ignored")), 1000)

    def test_paging_cursor_reaches_messages_sorted_after_a_junk_flood(self):
        ids = [channel_id(240 + number) for number in range(3)]
        write_peer_channel(
            self.alpha,
            "zulu",
            channel_record("zulu", "alice"),
            {
                message_id: channel_message(message_id, "alice", "zulu")
                for message_id in ids
            },
        )
        self.alpha.write_many_relay_entries(100000)
        snapshot = self.alpha_snapshot()
        cursor_path = self.beta.root / "bridge" / "chan-page" / "alpha"
        tip_path = self.beta.root / "bridge" / "chan-tip" / "alpha"

        first = self.tick_channels(snapshot)

        self.assertEqual(first.imported, 0)
        self.assertNotIn("alpha", first.completed)
        self.assertTrue(cursor_path.is_file())

        second = self.tick_channels(snapshot)

        self.assertEqual(second.imported, 0)
        self.assertNotIn("alpha", second.completed)
        self.assertTrue(cursor_path.is_file())

        third = self.tick_channels(snapshot)

        self.assertEqual(third.imported, 3)
        self.assertIn("alpha", third.completed)
        self.assertFalse(cursor_path.exists())
        self.assertTrue(tip_path.is_file())
        self.assertEqual(
            sorted(
                item.name
                for item in (
                    self.beta.root / "channels" / "zulu" / "messages"
                ).glob("*.msg")
            ),
            sorted(message_id + ".msg" for message_id in ids),
        )

    def test_paged_resume_imports_a_mid_sort_push_before_continuing(self):
        record = channel_record("paged", "alice")
        initial = [channel_id(500 + 10 * number) for number in range(60)]
        write_peer_channel(
            self.alpha,
            "paged",
            record,
            {message_id: channel_message(message_id, "alice", "paged")
             for message_id in initial},
        )
        landed = self.beta.root / "channels" / "paged" / "messages"
        cursor_path = self.beta.root / "bridge" / "chan-page" / "alpha"
        tip_path = self.beta.root / "bridge" / "chan-tip" / "alpha"

        with mock.patch.object(channels, "CHANNEL_TREE_MAX", 20):
            first = self.tick_channels(self.alpha_snapshot())
            self.assertEqual(first.imported, 19)
            self.assertNotIn("alpha", first.completed)
            self.assertTrue(cursor_path.is_file())

            before = [channel_id(505), channel_id(515)]
            after = [channel_id(9000)]
            write_peer_channel(
                self.alpha,
                "paged",
                record,
                {message_id: channel_message(message_id, "alice", "paged")
                 for message_id in before + after},
            )
            pushed = self.alpha_snapshot()
            resume = json.loads(cursor_path.read_text())["path"]
            for message_id in before:
                self.assertLess(
                    f"channels/paged/messages/{message_id}.msg", resume
                )
            self.assertGreater(
                f"channels/paged/messages/{after[0]}.msg", resume
            )

            second = self.tick_channels(pushed)

            for message_id in before:
                self.assertTrue(
                    (landed / (message_id + ".msg")).is_file(), message_id
                )
            self.assertNotIn("alpha", second.completed)

            stats, ticks = second, 2
            while "alpha" not in stats.completed and ticks < 8:
                stats = self.tick_channels(pushed)
                ticks += 1

        self.assertIn("alpha", stats.completed)
        self.assertEqual(
            len(list(landed.glob("*.msg"))),
            len(initial) + len(before) + len(after),
        )
        self.assertFalse(cursor_path.exists())
        self.assertEqual(tip_path.read_text().strip(), pushed.oids["machines/alpha"])

    def test_paged_resume_freezes_a_host_that_rewrites_history(self):
        ids = [channel_id(600 + number) for number in range(30)]
        write_peer_channel(
            self.alpha,
            "frozen",
            channel_record("frozen", "alice"),
            {message_id: channel_message(message_id, "alice", "frozen")
             for message_id in ids},
        )

        with mock.patch.object(channels, "CHANNEL_TREE_MAX", 20):
            first = self.tick_channels(self.alpha_snapshot())
            self.assertEqual(first.imported, 19)
            (
                self.alpha.repo
                / "channels"
                / "frozen"
                / "messages"
                / (ids[0] + ".msg")
            ).write_bytes(
                channel_message(ids[0], "alice", "frozen", body="rewritten")
            )
            self.alpha.commit("rewrite an already imported message")

            second = self.tick_channels(self.alpha_snapshot())

        self.assertEqual(second.rewritten, ["alpha"])
        self.assertEqual(second.imported, 0)
        self.assertNotIn("alpha", second.completed)
        self.assertTrue(
            (self.beta.root / "bridge" / "chan-rewritten" / "alpha").is_file()
        )

    def test_hostile_page_cursor_is_ignored_and_logged_once(self):
        unsafe = "channels/paged/messages/\u200b"
        oid = "b" * 40
        cases = {
            "symlink": None,
            "oversize": json.dumps(
                {"tip": "", "oid": oid, "path": "channels/x/messages/" + "a" * 8192}
            ).encode()
            + b"\n",
            "unsafe_component": json.dumps(
                {"tip": "", "oid": oid, "path": unsafe}
            ).encode()
            + b"\n",
            "bad_oid": json.dumps(
                {
                    "tip": "",
                    "oid": "not-a-git-object",
                    "path": "channels/paged/channel.json",
                }
            ).encode()
            + b"\n",
            "missing_oid": json.dumps({"tip": "", "path": unsafe}).encode() + b"\n",
            "not_json": b"{ this is not json\n",
        }
        for label, payload in cases.items():
            with self.subTest(cursor=label), CanonicalTemporaryDirectory(
                prefix="post-channel-page-"
            ) as directory:
                topology = Topology(directory)
                alpha = topology.add("alpha", ["alice"])
                beta = topology.add("beta", ["bob"])
                ids = [channel_id(250 + number) for number in range(10)]
                write_peer_channel(
                    alpha,
                    "paged",
                    channel_record("paged", "alice"),
                    {
                        message_id: channel_message(message_id, "alice", "paged")
                        for message_id in ids
                    },
                )
                snapshot = snapshot_for(
                    beta,
                    ["alpha"],
                    published={"alpha": frozenset({"alice"})},
                    v2_peers=("alpha",),
                    placeholders={"alice": "alpha"},
                    routes={"alice": "alpha"},
                )
                cursor_path = beta.root / "bridge" / "chan-page" / "alpha"
                cursor_path.parent.mkdir(parents=True, exist_ok=True)
                if payload is None:
                    cursor_path.symlink_to("/etc/hostname")
                else:
                    cursor_path.write_bytes(payload)
                log = Records()

                with mock.patch.object(channels, "CHANNEL_TREE_MAX", 5):
                    stats = channels.import_channels(
                        beta.settings,
                        ALL,
                        Git(beta.repo),
                        snapshot,
                        log,
                        Deadline(),
                        lambda: False,
                    )

                self.assertEqual(stats.imported, 4)
                self.assertNotIn("alpha", stats.completed)
                self.assertEqual(len(log.actions("chan_page_invalid")), 1)

    def test_chan_seen_markers_are_capped_per_category_and_host_each_tick(self):
        self.alpha.write_many_relay_entries(1200)

        self.import_alpha()

        self.assertEqual(len(self.log.actions("chan_ignored")), 1000)
        markers = list(
            (self.beta.root / "bridge" / "chan-seen" / "ignored" / "alpha").glob("*")
        )
        self.assertEqual(len(markers), 1000)

        self.import_alpha()

        self.assertEqual(len(self.log.actions("chan_ignored")), 1200)

    def test_entry_walk_checks_the_deadline_every_five_hundred_entries(self):
        self.alpha.write_many_relay_entries(1200)
        snapshot = self.alpha_snapshot()

        with self.assertRaises(common.DeadlineExpired):
            channels.import_channels(
                self.beta.settings,
                ALL,
                Git(self.beta.repo),
                snapshot,
                self.log,
                Deadline(raise_after=3),
                lambda: False,
            )

    def test_entry_walk_under_one_stride_does_not_check_the_deadline_per_entry(self):
        # Control for the test above: the same allowance survives a walk
        # shorter than one stride, so the 1200-entry raise comes from the
        # 500-entry checks and not from the per-host check alone.
        self.alpha.write_many_relay_entries(400)
        snapshot = self.alpha_snapshot()
        deadline = Deadline(raise_after=3)

        channels.import_channels(
            self.beta.settings,
            ALL,
            Git(self.beta.repo),
            snapshot,
            self.log,
            deadline,
            lambda: False,
        )

        self.assertLessEqual(deadline.calls, 3)

    def test_record_contract_rejects_all_invalid_families_once(self):
        base = channel_record("good", "alice")
        invalid = {
            "unknown": {**base, "name": "unknown", "extra": True},
            "large": {**base, "name": "large", "description": "x" * 5120},
            "mismatch": {**base, "name": "other"},
            "bad-creator": {**base, "name": "bad-creator", "created_by": "bad/name"},
            "bad-created": {**base, "name": "bad-created", "created": "tomorrow"},
        }
        for number, (name, record) in enumerate(invalid.items(), 1):
            message_id = channel_id(number)
            write_peer_channel(
                self.alpha,
                name,
                record,
                {message_id: channel_message(message_id, "alice", name)},
            )
        stats = self.import_alpha()
        self.assertEqual(stats.imported, 0)
        self.assertEqual(len(self.log.actions("chan_record_invalid")), 5)
        no_record = self.log.actions("chan_no_record")
        self.assertEqual(len(no_record), 5)
        self.assertEqual({item["host"] for item in no_record}, {"alpha"})
        for name in invalid:
            self.assertFalse((self.beta.root / "channels" / name).exists())

    def test_deeply_nested_channel_json_is_rejected_and_import_continues(self):
        # M3: a nested channel record or envelope used to raise
        # RecursionError out of import on Python 3.9.
        nest = b"[" * 1900 + b"]" * 1900
        record_root = self.alpha.repo / "channels" / "deep-record"
        (record_root / "messages").mkdir(parents=True)
        (record_root / "channel.json").write_bytes(
            b'{"name":"deep-record","created":"2026-09-02 17:22:00 +0000",'
            b'"created_by":"alice","description":' + nest + b"}"
        )
        self.assertLessEqual(
            len((record_root / "channel.json").read_bytes()), channels.CHANNEL_RECORD_MAX
        )
        record_message = channel_id(710)
        (record_root / "messages" / (record_message + ".msg")).write_bytes(
            channel_message(record_message, "alice", "deep-record")
        )
        deep_message = channel_id(711)
        good_message = channel_id(712)
        write_peer_channel(
            self.alpha,
            "deep-message",
            channel_record("deep-message", "alice"),
            {
                deep_message: b'{"x":' + nest + b"}\n---\nbody",
                good_message: channel_message(good_message, "alice", "deep-message"),
            },
        )

        stats = self.import_alpha()

        self.assertEqual(stats.imported, 1)
        self.assertEqual(
            [item["channel"] for item in self.log.actions("chan_record_invalid")],
            ["deep-record"],
        )
        self.assertIn("nests deeper", self.log.actions("chan_record_invalid")[0]["reason"])
        quarantined = self.log.actions("chan_quarantined")
        self.assertEqual([item["id"] for item in quarantined], [deep_message])
        self.assertIn("nests deeper", quarantined[0]["reason"])
        self.assertTrue(
            (
                self.beta.root / "channels" / "deep-message" / "messages"
                / (good_message + ".msg")
            ).is_file()
        )

    def test_peer_rejection_events_are_persistently_deduplicated(self):
        invalid_id = channel_id(8)
        write_peer_channel(
            self.alpha,
            "bad-record",
            {**channel_record("bad-record", "alice"), "extra": True},
            {invalid_id: channel_message(invalid_id, "alice", "bad-record")},
        )
        bad_message_id = channel_id(7)
        write_peer_channel(
            self.alpha,
            "bad-message",
            channel_record("bad-message", "alice"),
            {bad_message_id: b"not an envelope"},
        )
        self.alpha.write_relay(
            "channels/junk/unexpected.txt", b"junk", message="invalid channel path"
        )

        quarantined = []
        for _ in range(3):
            quarantined.append(self.import_alpha().quarantined)

        self.assertEqual(quarantined, [1, 0, 0])
        self.assertEqual(len(self.log.actions("chan_record_invalid")), 1)
        self.assertEqual(len(self.log.actions("chan_quarantined")), 1)
        self.assertEqual(len(self.log.actions("chan_ignored")), 1)

    def test_import_local_nonregular_files_skip_items_without_aborting(self):
        cases = ("symlink-message", "symlink-members", "fifo-message")
        for number, name in enumerate(cases, 130):
            message_id = channel_id(number)
            record = channel_record(name, "alice")
            write_peer_channel(
                self.alpha,
                name,
                record,
                {message_id: channel_message(message_id, "alice", name, event="join")},
            )
            local = self.beta.root / "channels" / name
            (local / "messages").mkdir(parents=True, exist_ok=True)
            if name == "symlink-message":
                (local / "messages" / (message_id + ".msg")).symlink_to("/etc/hostname")
            elif name == "fifo-message":
                os.mkfifo(str(local / "messages" / (message_id + ".msg")))
            else:
                (local / "channel.json").write_text(json.dumps(record) + "\n")
                (local / "members.json").symlink_to("/etc/hostname")

        stats = self.import_alpha()

        self.assertEqual(stats.imported, 0)
        unreadable = self.log.actions("chan_local_unreadable")
        self.assertEqual({item["channel"] for item in unreadable}, set(cases))
        self.assertEqual(len(unreadable), 3)

    def test_malformed_local_members_skips_the_channel_once_per_tick(self):
        name = "mal"
        record = channel_record(name, "alice")
        messages = {}
        for number in (200, 201, 202):
            message_id = channel_id(number)
            messages[message_id] = channel_message(
                message_id, "alice", name, event="join"
            )
        plain_id = channel_id(203)
        messages[plain_id] = channel_message(plain_id, "alice", name, body="plain")
        write_peer_channel(self.alpha, name, record, messages)
        local = self.beta.root / "channels" / name
        (local / "messages").mkdir(parents=True, exist_ok=True)
        (local / "channel.json").write_text(
            json.dumps(record) + "\n", encoding="utf-8"
        )
        (local / "members.json").write_text("{ this is not json", encoding="utf-8")

        stats = self.import_alpha()

        self.assertEqual(stats.imported, 0)
        self.assertEqual(
            sorted(item.name for item in (local / "messages").glob("*.msg")), []
        )
        unreadable = self.log.actions("chan_local_unreadable")
        self.assertEqual(len(unreadable), 1)
        self.assertEqual(unreadable[0]["channel"], name)

    def test_origin_adopts_once_and_never_changes_created_fields(self):
        message_id = channel_id(10)
        record = channel_record("origin", "alice", "from alpha")
        write_peer_channel(
            self.alpha,
            "origin",
            record,
            {message_id: channel_message(message_id, "alice", "origin")},
        )
        self.import_alpha()
        path = self.beta.root / "channels" / "origin" / "channel.json"
        local = json.loads(path.read_text())
        local["description"] = "local edit"
        path.write_text(json.dumps(local, indent=2) + "\n")
        self.import_alpha()
        after = json.loads(path.read_text())
        self.assertEqual(after["description"], "local edit")
        self.assertEqual(after["created"], record["created"])
        self.assertEqual(after["created_by"], record["created_by"])
        self.assertEqual(len(self.log.actions("chan_description_adopted")), 1)

    def test_record_only_peer_channel_is_created_and_joinable(self):
        write_peer_channel(
            self.alpha,
            "quiet",
            channel_record("quiet", "alice"),
            {},
        )

        stats = self.import_alpha()

        channel_root = self.beta.root / "channels" / "quiet"
        self.assertTrue((channel_root / "channel.json").is_file())
        self.assertEqual((channel_root / "members.json").read_bytes(), b"{}\n")
        joined = self.beta.post(
            "chat", "quiet", "--join", cwd=self.beta.workspaces["bob"], check=False
        )
        self.assertEqual(joined.returncode, 0, joined.stdout + joined.stderr)
        self.assertIn("alpha", stats.completed)

    def test_c6_partial_states_are_reconciled(self):
        variants = ("dir", "record", "members", "origin")
        for number, variant in enumerate(variants, 20):
            name = "partial-" + variant
            message_id = channel_id(number)
            record = channel_record(name, "alice")
            write_peer_channel(
                self.alpha,
                name,
                record,
                {message_id: channel_message(message_id, "alice", name)},
            )
            local = self.beta.root / "channels" / name
            (local / "messages").mkdir(parents=True)
            if variant in ("record", "members", "origin"):
                (local / "channel.json").write_text(json.dumps(record) + "\n")
            if variant in ("members", "origin"):
                (local / "members.json").write_text("{}\n")
            if variant == "origin":
                marker = self.beta.root / "bridge" / "chan-origin" / name
                marker.parent.mkdir(parents=True, exist_ok=True)
                marker.write_text("alpha\n")
        result = self.import_alpha()
        self.assertEqual(result.imported, 4)
        for variant in variants:
            root = self.beta.root / "channels" / ("partial-" + variant)
            self.assertTrue((root / "channel.json").is_file())
            self.assertTrue((root / "members.json").is_file())
            self.assertEqual(len(list(root.glob("channel.json"))), 1)

    def test_channel_lock_serializes_concurrent_local_join(self):
        message_id = channel_id(30)
        write_peer_channel(
            self.alpha,
            "locked",
            channel_record("locked", "alice"),
            {message_id: channel_message(message_id, "alice", "locked")},
        )
        lock_root = self.beta.root / "channels"
        lock_root.mkdir()
        lock = lock_root / ".channels.lock"
        descriptor = os.open(str(lock), os.O_RDWR | os.O_CREAT, 0o600)
        fcntl.flock(descriptor, fcntl.LOCK_EX)
        errors = []
        join_errors = []
        worker = threading.Thread(target=lambda: _capture(errors, self.import_alpha))
        worker.start()
        time.sleep(0.1)
        self.assertFalse((lock_root / "locked" / "channel.json").exists())
        joiner = threading.Thread(
            target=lambda: _capture(
                join_errors, lambda: self.beta.join("locked", "bob")
            )
        )
        joiner.start()
        time.sleep(0.1)
        fcntl.flock(descriptor, fcntl.LOCK_UN)
        os.close(descriptor)
        worker.join(5)
        joiner.join(5)
        self.assertFalse(worker.is_alive())
        self.assertFalse(joiner.is_alive())
        self.assertEqual(errors, [])
        self.assertEqual(join_errors, [])
        self.assertTrue((lock_root / "locked" / "channel.json").is_file())
        message_files = list((lock_root / "locked" / "messages").glob("*.msg"))
        self.assertEqual(len(message_files), 2)
        # post 0.9.0 records a join in the participant's own channels.json
        # (members.json is only the legacy workspace default), so membership
        # is read back through post's listing, the contract surface.
        listing = json.loads(
            self.beta.post("channels", cwd=self.beta.workspaces["bob"]).stdout
        )
        locked = [item for item in listing["channels"] if item["name"] == "locked"]
        self.assertEqual(len(locked), 1)
        self.assertIn(self.beta.participant("bob"), locked[0]["participants"])

    def test_event_schema_excludes_message_content(self):
        name = "event-schema"
        message_id = channel_id(40)
        write_peer_channel(
            self.alpha,
            name,
            channel_record(name, "alice"),
            {
                message_id: channel_message(
                    message_id, "alice", name, mentions=["bob"], body="secret body"
                )
            },
        )
        self.import_alpha()
        event_path = (
            self.beta.root / "bridge" / "events" / name / (message_id + ".json")
        )
        event = json.loads(event_path.read_text())
        self.assertEqual(
            set(event), {"host", "channel", "id", "from", "event", "mentions"}
        )
        self.assertNotIn("subject", event)
        self.assertNotIn("body", event)

    def test_sigkill_c5_matrix_reaches_one_fixed_point(self):
        for number, checkpoint in enumerate(
            ("channels-c5a", "channels-c5b", "channels-c5c"), 440
        ):
            with self.subTest(checkpoint=checkpoint), CanonicalTemporaryDirectory(
                prefix="post-channel-c5-"
            ) as directory:
                topology = Topology(directory)
                alpha = topology.add("alpha", ["alice"])
                beta = topology.add("beta", ["bob"])
                name = "crash-" + checkpoint[-1]
                message_id = channel_id(number)
                write_peer_channel(
                    alpha,
                    name,
                    channel_record(name, "alice"),
                    {message_id: channel_message(message_id, "alice", name)},
                )
                snapshot = snapshot_for(
                    beta,
                    ["alpha"],
                    published={"alpha": frozenset({"alice"})},
                    v2_peers=("alpha",),
                    placeholders={"alice": "alpha"},
                    routes={"alice": "alpha"},
                )
                pid = os.fork()
                if pid == 0:
                    os.environ["BRIDGE_CRASH_AFTER"] = checkpoint
                    channels.import_channels(
                        beta.settings,
                        ALL,
                        Git(beta.repo),
                        snapshot,
                        Records(),
                        Deadline(),
                        lambda: False,
                    )
                    os._exit(0)
                _, status = os.waitpid(pid, 0)
                self.assertTrue(os.WIFSIGNALED(status))
                self.assertEqual(os.WTERMSIG(status), 9)
                os.environ.pop("BRIDGE_CRASH_AFTER", None)
                channels.import_channels(
                    beta.settings,
                    ALL,
                    Git(beta.repo),
                    snapshot,
                    Records(),
                    Deadline(),
                    lambda: False,
                )
                event = beta.root / "bridge" / "events" / name / (message_id + ".json")
                message = beta.root / "channels" / name / "messages" / (message_id + ".msg")
                self.assertTrue(event.is_file())
                self.assertTrue(message.is_file())
                self.assertEqual(len(list(event.parent.glob(message_id + ".json"))), 1)

    def test_identical_second_host_cannot_steal_crashed_reservation(self):
        gamma = self.topology.add("gamma", ["gary"])
        message_id = channel_id(470)
        data = channel_message(message_id, "legacy", "shared")
        write_peer_channel(
            self.alpha,
            "shared",
            channel_record("shared", "legacy"),
            {message_id: data},
        )
        write_peer_channel(
            gamma,
            "shared",
            channel_record("shared", "legacy"),
            {message_id: data},
        )
        both = snapshot_for(
            self.beta,
            ["alpha", "gamma"],
            v2_peers=(),
            placeholders={},
            routes={},
        )
        pid = os.fork()
        if pid == 0:
            os.environ["BRIDGE_CRASH_AFTER"] = "channels-c5a"
            channels.import_channels(
                self.beta.settings, ALL, Git(self.beta.repo), both, Records(), Deadline(), lambda: False
            )
            os._exit(0)
        _, status = os.waitpid(pid, 0)
        self.assertEqual(os.WTERMSIG(status), 9)
        os.environ.pop("BRIDGE_CRASH_AFTER", None)
        gamma_only = replace(
            both,
            peers=("gamma",),
            oids={"machines/gamma": both.oids["machines/gamma"]},
        )
        gamma_stats = channels.import_channels(
            self.beta.settings, ALL, Git(self.beta.repo), gamma_only, Records(), Deadline(), lambda: False
        )
        reservation = self.beta.root / "bridge" / "chan-received" / "shared" / message_id
        message = self.beta.root / "channels" / "shared" / "messages" / (message_id + ".msg")
        self.assertTrue(reservation.read_text().startswith("alpha "))
        self.assertFalse(message.exists())
        self.assertEqual(gamma_stats.diverged, [])
        alpha_only = replace(
            both,
            peers=("alpha",),
            oids={"machines/alpha": both.oids["machines/alpha"]},
        )
        channels.import_channels(
            self.beta.settings, ALL, Git(self.beta.repo), alpha_only, Records(), Deadline(), lambda: False
        )
        self.assertEqual(message.read_bytes(), data)

    def test_deadline_mid_import_leaves_replayable_state(self):
        messages = {
            channel_id(480 + number): channel_message(
                channel_id(480 + number), "alice", "deadline"
            )
            for number in range(3)
        }
        write_peer_channel(
            self.alpha,
            "deadline",
            channel_record("deadline", "alice"),
            messages,
        )
        snapshot = self.alpha_snapshot()
        with self.assertRaises(common.DeadlineExpired):
            channels.import_channels(
                self.beta.settings,
                ALL,
                Git(self.beta.repo),
                snapshot,
                self.log,
                Deadline(raise_after=3),
                lambda: False,
            )
        landed = list((self.beta.root / "channels" / "deadline" / "messages").glob("*.msg"))
        self.assertEqual(len(landed), 1)
        for path in landed:
            self.assertTrue(
                (self.beta.root / "bridge" / "events" / "deadline" / (path.stem + ".json")).is_file()
            )
            self.assertTrue(
                (self.beta.root / "bridge" / "chan-received" / "deadline" / path.stem).is_file()
            )
        completed = channels.import_channels(
            self.beta.settings, ALL, Git(self.beta.repo), snapshot, self.log, Deadline(), lambda: False
        )
        self.assertEqual(completed.imported, 2)
        self.assertEqual(
            len(list((self.beta.root / "channels" / "deadline" / "messages").glob("*.msg"))),
            3,
        )

    def test_mail_root_replace_crash_leaves_no_channel_temp_files(self):
        message_id = channel_id(49)
        write_peer_channel(
            self.alpha,
            "staged",
            channel_record("staged", "alice"),
            {message_id: channel_message(message_id, "alice", "staged", event="join")},
        )
        pid = os.fork()
        if pid == 0:
            os.environ["BRIDGE_CRASH_AFTER"] = "channels-mail-stage"
            self.import_alpha()
            os._exit(0)
        _, status = os.waitpid(pid, 0)

        self.assertTrue(os.WIFSIGNALED(status))
        self.assertEqual(os.WTERMSIG(status), 9)
        channel_root = self.beta.root / "channels" / "staged"
        residue = [
            path
            for path in channel_root.rglob("*")
            if path.name.endswith((".tmp", ".stage"))
        ]
        self.assertEqual(residue, [])
        os.environ.pop("BRIDGE_CRASH_AFTER", None)
        self.import_alpha()
        members = json.loads((channel_root / "members.json").read_text())
        self.assertIn("alice", members)

    def test_divergence_persists_forensic_and_identical_is_noop(self):
        gamma = self.topology.add("gamma", ["gary"])
        same_id = channel_id(50)
        first = channel_message(same_id, "legacy-sender", "shared", body="first")
        write_peer_channel(
            self.alpha,
            "shared",
            channel_record("shared", "legacy-sender"),
            {same_id: first},
        )
        write_peer_channel(
            gamma, "shared", channel_record("shared", "legacy-sender"), {same_id: first}
        )
        snapshot = snapshot_for(
            self.beta,
            ["alpha", "gamma"],
            published={"alpha": frozenset({"alice"}), "gamma": frozenset({"gary"})},
            v2_peers=(),
            placeholders={"alice": "alpha", "gary": "gamma"},
            routes={"alice": "alpha", "gary": "gamma"},
        )
        first_stats = channels.import_channels(
            self.beta.settings,
            ALL,
            Git(self.beta.repo),
            snapshot,
            self.log,
            self.deadline,
            lambda: False,
        )
        self.assertEqual(first_stats.diverged, [])
        expected_sha = hashlib.sha256(first).hexdigest()
        self.assertEqual(
            (self.beta.root / "bridge" / "chan-received" / "shared" / same_id)
            .read_text()
            .strip(),
            f"alpha {expected_sha}",
        )
        self.assertEqual(
            len(
                list(
                    (self.beta.root / "channels" / "shared" / "messages").glob(
                        same_id + ".msg"
                    )
                )
            ),
            1,
        )
        self.assertFalse(
            (
                self.beta.root
                / "bridge"
                / "quarantine"
                / "channels"
                / "gamma"
                / "shared"
                / f"{same_id}-gamma-conflict.msg"
            ).exists()
        )
        self.assertFalse(
            (self.beta.root / "bridge" / "chan-diverged.json").exists()
        )
        self.assertEqual(self.log.actions("channel_diverged"), [])
        gamma_message = (
            gamma.repo / "channels" / "shared" / "messages" / (same_id + ".msg")
        )
        gamma_message.write_bytes(
            channel_message(same_id, "legacy-sender", "shared", body="different")
        )
        gamma.commit("colliding bytes")
        snapshot = snapshot_for(
            self.beta,
            ["alpha", "gamma"],
            published={"alpha": frozenset({"alice"}), "gamma": frozenset({"gary"})},
            v2_peers=(),
            placeholders={"alice": "alpha", "gary": "gamma"},
            routes={"alice": "alpha", "gary": "gamma"},
        )
        stats = channels.import_channels(
            self.beta.settings,
            ALL,
            Git(self.beta.repo),
            snapshot,
            self.log,
            self.deadline,
            lambda: False,
        )
        self.assertEqual(len(stats.diverged), 1)
        self.assertEqual(
            (
                self.beta.root / "channels" / "shared" / "messages" / (same_id + ".msg")
            ).read_bytes(),
            first,
        )
        self.assertTrue(
            (
                self.beta.root
                / "bridge"
                / "quarantine"
                / "channels"
                / "gamma"
                / "shared"
                / f"{same_id}-gamma-conflict.msg"
            ).is_file()
        )
        self.assertEqual(
            len(
                json.loads(
                    (self.beta.root / "bridge" / "chan-diverged.json").read_text()
                )
            ),
            1,
        )

    def test_reservations_are_scoped_by_channel(self):
        gamma = self.topology.add("gamma", ["gary"])
        same_id = channel_id(51)
        alpha_bytes = channel_message(same_id, "alice", "porch", body="alpha")
        gamma_bytes = channel_message(same_id, "gary", "lamp", body="gamma")
        write_peer_channel(
            self.alpha,
            "porch",
            channel_record("porch", "alice"),
            {same_id: alpha_bytes},
        )
        write_peer_channel(
            gamma,
            "lamp",
            channel_record("lamp", "gary"),
            {same_id: gamma_bytes},
        )
        snapshot = snapshot_for(
            self.beta,
            ["alpha", "gamma"],
            published={"alpha": frozenset({"alice"}), "gamma": frozenset({"gary"})},
            v2_peers=("alpha", "gamma"),
            placeholders={"alice": "alpha", "gary": "gamma"},
            routes={"alice": "alpha", "gary": "gamma"},
        )

        stats = channels.import_channels(
            self.beta.settings,
            ALL,
            Git(self.beta.repo),
            snapshot,
            self.log,
            self.deadline,
            lambda: False,
        )

        self.assertEqual(stats.imported, 2)
        self.assertEqual(stats.diverged, [])
        self.assertEqual(
            (self.beta.root / "channels" / "porch" / "messages" / (same_id + ".msg")).read_bytes(),
            alpha_bytes,
        )
        self.assertEqual(
            (self.beta.root / "channels" / "lamp" / "messages" / (same_id + ".msg")).read_bytes(),
            gamma_bytes,
        )

    def test_received_id_in_other_channel_does_not_block_publish(self):
        self.alpha.join("lamp", "alice")
        self.alpha.send("lamp", "alice", "ours")
        message_path = next(
            (self.alpha.root / "channels" / "lamp" / "messages").glob("*.msg")
        )
        message_id = message_path.stem
        reservation = self.alpha.root / "bridge" / "chan-received" / message_id
        reservation.parent.mkdir(parents=True)
        reservation.write_text("beta " + hashlib.sha256(message_path.read_bytes()).hexdigest() + "\n")

        stats = channels.publish_channels(
            self.alpha.settings,
            ALL,
            local_snapshot(self.alpha),
            self.log,
            self.deadline,
        )

        self.assertEqual(stats.published, 2)
        self.assertTrue(
            (self.alpha.repo / "channels" / "lamp" / "messages" / message_path.name).is_file()
        )
        self.assertEqual(self.log.actions("chan_self_conflict"), [])

    def test_archive_rewrite_freezes_tip_but_other_host_imports(self):
        gamma = self.topology.add("gamma", ["gary"])
        alpha_id, gamma_id = channel_id(60), channel_id(61)
        alpha_oid = write_peer_channel(
            self.alpha,
            "history",
            channel_record("history", "alice"),
            {alpha_id: channel_message(alpha_id, "alice", "history")},
        )
        write_peer_channel(
            gamma,
            "other",
            channel_record("other", "gary"),
            {gamma_id: channel_message(gamma_id, "gary", "other")},
        )
        channels.advance_tip(self.beta.settings, "alpha", alpha_oid)
        alpha_path = (
            self.alpha.repo / "channels" / "history" / "messages" / (alpha_id + ".msg")
        )
        alpha_path.write_bytes(
            channel_message(alpha_id, "alice", "history", body="modified")
        )
        self.alpha.commit("rewrite history")
        snapshot = snapshot_for(
            self.beta,
            ["alpha", "gamma"],
            published={"alpha": frozenset({"alice"}), "gamma": frozenset({"gary"})},
            v2_peers=("alpha", "gamma"),
            placeholders={"alice": "alpha", "gary": "gamma"},
            routes={"alice": "alpha", "gary": "gamma"},
        )
        stats = channels.import_channels(
            self.beta.settings,
            ALL,
            Git(self.beta.repo),
            snapshot,
            self.log,
            self.deadline,
            lambda: False,
        )
        self.assertEqual(stats.rewritten, ["alpha"])
        self.assertFalse((self.beta.root / "channels" / "history").exists())
        self.assertTrue(
            (
                self.beta.root / "channels" / "other" / "messages" / (gamma_id + ".msg")
            ).is_file()
        )
        self.assertEqual(
            (self.beta.root / "bridge" / "chan-tip" / "alpha").read_text().strip(),
            alpha_oid,
        )
        self.assertTrue(
            (self.beta.root / "bridge" / "chan-rewritten" / "alpha").is_file()
        )
        current_alpha = snapshot.oids["machines/alpha"]
        channels.advance_tip(self.beta.settings, "alpha", current_alpha)
        resumed = channels.import_channels(
            self.beta.settings,
            ALL,
            Git(self.beta.repo),
            snapshot,
            self.log,
            self.deadline,
            lambda: False,
        )
        self.assertIn("alpha", resumed.completed)
        self.assertTrue(
            (self.beta.root / "channels" / "history" / "messages" / (alpha_id + ".msg")).is_file()
        )

    def test_archive_check_allows_channel_record_description_update(self):
        message_id = channel_id(62)
        first_oid = write_peer_channel(
            self.alpha,
            "mutable-record",
            channel_record("mutable-record", "alice", "before"),
            {
                message_id: channel_message(
                    message_id, "alice", "mutable-record"
                )
            },
        )
        channels.advance_tip(self.beta.settings, "alpha", first_oid)
        record_path = (
            self.alpha.repo / "channels" / "mutable-record" / "channel.json"
        )
        record = json.loads(record_path.read_text())
        record["description"] = "after"
        record_path.write_text(json.dumps(record, sort_keys=True, indent=2) + "\n")
        updated_oid = self.alpha.commit("update channel description")
        self.beta.fetch()

        allowed = channels.check_archive(
            self.beta.settings,
            Git(self.beta.repo),
            "alpha",
            updated_oid,
            self.log,
        )

        self.assertTrue(allowed)
        self.assertEqual(self.log.actions("relay_history_rewritten"), [])
        self.assertFalse(
            (self.beta.root / "bridge" / "chan-rewritten" / "alpha").exists()
        )

    def test_channel_tip_is_validated_before_git(self):
        target = Path(self.temporary.name) / "git-option-output"
        tip = self.beta.root / "bridge" / "chan-tip" / "alpha"
        tip.parent.mkdir(parents=True, exist_ok=True)
        tip.write_text(f"--output={target}\n")
        snapshot = self.alpha_snapshot()

        with self.assertRaises(common.TickError) as raised:
            channels.check_archive(
                self.beta.settings,
                Git(self.beta.repo),
                "alpha",
                snapshot.oids["machines/alpha"],
                self.log,
            )

        self.assertEqual(raised.exception.reason, "channel_tip_invalid")
        self.assertFalse(target.exists())
        self.assertEqual(len(self.log.actions("channel_tip_invalid")), 1)

    def test_missing_tip_object_degrades_to_rewritten_host(self):
        tip = self.beta.root / "bridge" / "chan-tip" / "alpha"
        tip.parent.mkdir(parents=True, exist_ok=True)
        tip.write_text("d" * 40 + "\n")
        snapshot = self.alpha_snapshot()

        stats = self.import_alpha(snapshot)

        self.assertEqual(stats.rewritten, ["alpha"])
        self.assertEqual(stats.completed, frozenset())
        self.assertEqual(len(self.log.actions("relay_history_rewritten")), 1)

    def test_membership_block_held_then_admitted_and_pending_then_registered(self):
        self.beta.join("members", "bob")
        join_id = channel_id(70)
        pending_id = channel_id(71)
        write_peer_channel(
            self.alpha,
            "members",
            channel_record("members", "alice"),
            {
                join_id: channel_message(join_id, "alice", "members", event="join"),
                pending_id: channel_message(
                    pending_id, "legacy", "members", event="join"
                ),
            },
        )
        (self.beta.root / "rules.json").write_text(
            json.dumps(
                {"blocked": [{"from": "alice", "to": "bob", "reason": "not yet"}]}
            )
            + "\n"
        )
        legacy_snapshot = self.alpha_snapshot(legacy=True)
        self.import_alpha(legacy_snapshot)
        members = json.loads(
            (self.beta.root / "channels" / "members" / "members.json").read_text()
        )
        self.assertNotIn("alice", members)
        self.assertTrue(
            (
                self.beta.root / "bridge" / "chan-joins-held" / "members" / join_id
            ).is_file()
        )
        self.assertTrue(
            (
                self.beta.root
                / "bridge"
                / "chan-joins-pending"
                / "members"
                / pending_id
            ).is_file()
        )
        channels.advance_tip(
            self.beta.settings, "alpha", legacy_snapshot.oids["machines/alpha"]
        )
        (self.beta.root / "rules.json").write_text('{"blocked":[]}\n')
        registered = replace(
            legacy_snapshot,
            published={"alpha": frozenset({"alice", "legacy"})},
            v2_peers=frozenset({"alpha"}),
            placeholders={"alice": "alpha", "legacy": "alpha"},
        )
        self.import_alpha(registered)
        members = json.loads(
            (self.beta.root / "channels" / "members" / "members.json").read_text()
        )
        # The bridge indexes the two remote joins; bob joined locally as a
        # participant, which post 0.9.0 records in bob's channels.json, not in
        # members.json (the legacy workspace default). It stays a member.
        self.assertEqual(set(members), {"alice", "legacy"})
        listing = json.loads(
            self.beta.post("channels", cwd=self.beta.workspaces["bob"]).stdout
        )
        joined = [item for item in listing["channels"] if item["name"] == "members"]
        self.assertIn(self.beta.participant("bob"), joined[0]["participants"])
        self.assertFalse(
            (
                self.beta.root / "bridge" / "chan-joins-held" / "members" / join_id
            ).exists()
        )
        self.assertFalse(
            (
                self.beta.root
                / "bridge"
                / "chan-joins-pending"
                / "members"
                / pending_id
            ).exists()
        )
        for marker_dir in ("chan-joins-pending", "chan-joins-held"):
            self.assertTrue(
                tick._directory_empty(self.beta.root / "bridge" / marker_dir),
                marker_dir,
            )

    def test_unreadable_participant_state_holds_a_join_only_when_a_rule_could_apply(self):
        """C7 fails closed like post's own join admission.

        post 0.9.0 refuses a join when a participant record that might be a
        blocked member cannot be read. The bridge cannot tell whether an
        unreadable record is a member, so it holds a remote join whenever any
        rule could match a route touching the sender, and admits it when none
        could.
        """
        bob = self.beta.participant("bob")
        state = self.beta.root / "participants" / bob / "channels.json"
        state.write_text("{not json\n", encoding="utf-8")
        held_id = channel_id(80)
        write_peer_channel(
            self.alpha,
            "unreadable",
            channel_record("unreadable", "alice"),
            {held_id: channel_message(held_id, "alice", "unreadable", event="join")},
        )
        (self.beta.root / "rules.json").write_text(
            json.dumps(
                {"blocked": [{"from": "alice", "to": "carol", "reason": "elsewhere"}]}
            )
            + "\n"
        )
        self.import_alpha()
        members_path = self.beta.root / "channels" / "unreadable" / "members.json"
        self.assertNotIn("alice", json.loads(members_path.read_text()))
        held = (
            self.beta.root / "bridge" / "chan-joins-held" / "unreadable" / held_id
        )
        self.assertEqual(held.read_bytes(), b"participant_state_unreadable")

        (self.beta.root / "rules.json").write_text(
            json.dumps(
                {"blocked": [{"from": "dave", "to": "carol", "reason": "unrelated"}]}
            )
            + "\n"
        )
        self.import_alpha()
        self.assertIn("alice", json.loads(members_path.read_text()))
        self.assertFalse(held.exists())

    def test_unpublished_sender_quarantine_is_final_across_ticks(self):
        message_id = channel_id(600)
        write_peer_channel(
            self.alpha,
            "lag",
            channel_record("lag", "alice"),
            {
                message_id: channel_message(
                    message_id, "newroom", "lag", body="legit"
                )
            },
        )
        lagging = self.alpha_snapshot()

        first = self.import_alpha(lagging)

        self.assertEqual(first.imported, 0)
        self.assertEqual(first.quarantined, 1)
        self.assertEqual(
            [item["reason"] for item in self.log.actions("chan_quarantined")],
            ["unpublished_sender"],
        )
        forensic = (
            self.beta.root
            / "bridge"
            / "quarantine"
            / "channels"
            / "alpha"
            / "lag"
            / (message_id + ".msg")
        )
        self.assertTrue(forensic.is_file())
        # r5.3: the verdict is final, so the tick is free to advance the tip
        # past it exactly as it does for a host with nothing outstanding.
        channels.advance_tip(
            self.beta.settings, "alpha", lagging.oids["machines/alpha"]
        )
        registered = replace(
            lagging,
            published={"alpha": frozenset({"alice", "newroom"})},
            placeholders={"alice": "alpha", "newroom": "alpha"},
            routes={"alice": "alpha", "newroom": "alpha"},
        )

        second = self.import_alpha(registered)

        self.assertEqual(second.imported, 0)
        self.assertEqual(second.quarantined, 0)
        self.assertFalse(
            (
                self.beta.root
                / "channels"
                / "lag"
                / "messages"
                / (message_id + ".msg")
            ).exists()
        )
        self.assertEqual(len(self.log.actions("chan_quarantined")), 1)
        self.assertTrue(forensic.is_file())

    def test_chan_no_record_for_an_absent_record_names_the_host(self):
        name = "orphan"
        messages = self.alpha.repo / "channels" / name / "messages"
        messages.mkdir(parents=True)
        for number in (210, 211):
            message_id = channel_id(number)
            (messages / (message_id + ".msg")).write_bytes(
                channel_message(message_id, "alice", name)
            )
        self.alpha.commit("publish a channel with no record")

        stats = self.import_alpha()

        self.assertEqual(stats.imported, 0)
        events = self.log.actions("chan_no_record")
        self.assertEqual(len(events), 1)
        self.assertEqual(events[0]["host"], "alpha")
        self.assertEqual(events[0]["channel"], name)
        self.assertFalse((self.beta.root / "channels" / name).exists())

    def test_allow_mode_unlisted_channel_is_silent_on_the_import_side(self):
        allow = channels.ChannelsConfig(
            "allow", frozenset({"listed"}), frozenset()
        )
        listed_id, unlisted_id = channel_id(220), channel_id(221)
        write_peer_channel(
            self.alpha,
            "listed",
            channel_record("listed", "alice"),
            {listed_id: channel_message(listed_id, "alice", "listed")},
        )
        write_peer_channel(
            self.alpha,
            "unlisted",
            channel_record("unlisted", "alice"),
            {unlisted_id: channel_message(unlisted_id, "alice", "unlisted")},
        )

        stats = channels.import_channels(
            self.beta.settings,
            allow,
            Git(self.beta.repo),
            self.alpha_snapshot(),
            self.log,
            self.deadline,
            lambda: False,
        )

        self.assertEqual(stats.imported, 1)
        self.assertEqual(stats.quarantined, 0)
        self.assertTrue(
            (
                self.beta.root
                / "channels"
                / "listed"
                / "messages"
                / (listed_id + ".msg")
            ).is_file()
        )
        self.assertFalse((self.beta.root / "channels" / "unlisted").exists())
        self.assertFalse(
            (self.beta.root / "bridge" / "chan-received" / "unlisted").exists()
        )
        for action in ("chan_ignored", "chan_quarantined", "chan_no_record"):
            self.assertEqual(self.log.actions(action), [], action)

    def test_join_replay_survives_hostile_local_marker_directories(self):
        message_id = channel_id(230)
        write_peer_channel(
            self.alpha,
            "replay",
            channel_record("replay", "alice"),
            {message_id: channel_message(message_id, "alice", "replay")},
        )
        bridge = self.beta.root / "bridge"
        bridge.mkdir(parents=True, exist_ok=True)
        (bridge / "chan-joins-held").symlink_to("/etc")

        stats = self.import_alpha()

        self.assertEqual(stats.imported, 1)
        self.assertTrue(
            (
                self.beta.root
                / "channels"
                / "replay"
                / "messages"
                / (message_id + ".msg")
            ).is_file()
        )

    def test_fence_before_first_mutation_leaves_no_channel_state(self):
        message_id = channel_id(80)
        write_peer_channel(
            self.alpha,
            "fenced",
            channel_record("fenced", "alice"),
            {message_id: channel_message(message_id, "alice", "fenced")},
        )
        calls = 0

        def fence():
            nonlocal calls
            calls += 1
            return True

        with self.assertRaisesRegex(common.TickError, "fenced"):
            self.import_alpha(fence=fence)
        self.assertFalse((self.beta.root / "channels").exists())
        self.assertFalse((self.beta.root / "bridge" / "chan-received").exists())

    def test_counting_fence_binds_each_channel_mutation_boundary(self):
        cases = {
            3: (False, False, False, False, False, False),
            5: (True, True, False, False, False, False),
            7: (True, True, True, True, False, False),
            8: (True, True, True, True, True, False),
        }
        for trip, expected in cases.items():
            with self.subTest(trip=trip), CanonicalTemporaryDirectory(
                prefix="post-channel-fence-"
            ) as directory:
                topology = Topology(directory)
                alpha = topology.add("alpha", ["alice"])
                beta = topology.add("beta", ["bob"])
                message_id = channel_id(800 + trip)
                write_peer_channel(
                    alpha,
                    "fenced",
                    channel_record("fenced", "alice"),
                    {
                        message_id: channel_message(
                            message_id, "alice", "fenced", event="join"
                        )
                    },
                )
                snapshot = snapshot_for(
                    beta,
                    ["alpha"],
                    published={"alpha": frozenset({"alice"})},
                    v2_peers=("alpha",),
                    placeholders={"alice": "alpha"},
                    routes={"alice": "alpha"},
                )
                calls = 0

                def fence():
                    nonlocal calls
                    calls += 1
                    return calls == trip

                with self.assertRaisesRegex(common.TickError, "fenced"):
                    channels.import_channels(
                        beta.settings,
                        ALL,
                        Git(beta.repo),
                        snapshot,
                        Records(),
                        Deadline(),
                        fence,
                    )
                channel_root = beta.root / "channels" / "fenced"
                record = (channel_root / "channel.json").is_file()
                reservation = (
                    beta.root / "bridge" / "chan-received" / "fenced" / message_id
                ).is_file()
                event = (
                    beta.root / "bridge" / "events" / "fenced" / (message_id + ".json")
                ).is_file()
                message = (
                    channel_root / "messages" / (message_id + ".msg")
                ).is_file()
                members = {}
                if (channel_root / "members.json").is_file():
                    members = json.loads((channel_root / "members.json").read_text())
                description = (
                    beta.root / "bridge" / "chan-desc" / "fenced"
                ).is_file()
                self.assertEqual(
                    (
                        record,
                        reservation,
                        event,
                        message,
                        "alice" in members,
                        description,
                    ),
                    expected,
                )

    def test_denied_allow_binding_quarantines_and_legacy_unhomed(self):
        ids = {
            name: channel_id(90 + number)
            for number, name in enumerate(
                ("denied", "allowed", "forged", "collision", "unpublished", "legacy")
            )
        }
        for name, message_id in ids.items():
            sender = {
                "forged": "bob",
                "collision": "collider",
                "unpublished": "nobody",
                "legacy": "old-room",
            }.get(name, "alice")
            write_peer_channel(
                self.alpha,
                name,
                channel_record(name, "alice"),
                {message_id: channel_message(message_id, sender, name)},
            )
        cfg = channels.ChannelsConfig(
            "allow",
            frozenset({"allowed", "forged", "collision", "unpublished", "legacy"}),
            frozenset({"denied"}),
        )
        snapshot = self.alpha_snapshot(legacy=False)
        snapshot = replace(
            snapshot,
            contested={"collider": Contest(owner="beta", claimants=("alpha", "beta"))},
            owners={"collider": Owner("beta", "oid")},
        )
        stats = channels.import_channels(
            self.beta.settings,
            cfg,
            Git(self.beta.repo),
            snapshot,
            self.log,
            self.deadline,
            lambda: False,
        )
        self.assertEqual(stats.imported, 1)
        reasons = {item["reason"] for item in self.log.actions("chan_quarantined")}
        self.assertEqual(
            reasons, {"forged_self", "name_collision", "unpublished_sender"}
        )
        self.assertFalse((self.beta.root / "channels" / "denied").exists())
        legacy = replace(snapshot, v2_peers=frozenset(), contested={}, owners={})
        channels.import_channels(
            self.beta.settings,
            cfg,
            Git(self.beta.repo),
            legacy,
            self.log,
            self.deadline,
            lambda: False,
        )
        self.assertTrue(
            any(
                item.get("sender") == "old-room"
                for item in self.log.actions("from-unhomed")
            )
        )
        for name in ("forged", "collision", "unpublished"):
            message_id = ids[name]
            self.assertTrue(
                (
                    self.beta.root
                    / "bridge"
                    / "quarantine"
                    / "channels"
                    / "alpha"
                    / name
                    / (message_id + ".msg")
                ).is_file()
            )

    def test_publish_side_deny_and_allow_unlisted_are_silent(self):
        for name in ("allowed-local", "denied-local", "unlisted-local"):
            self.alpha.join(name, "alice")
            self.alpha.send(name, "alice", name)
        cfg = channels.ChannelsConfig(
            "allow", frozenset({"allowed-local"}), frozenset({"denied-local"})
        )

        stats = channels.publish_channels(
            self.alpha.settings,
            cfg,
            local_snapshot(self.alpha),
            self.log,
            self.deadline,
        )

        self.assertGreater(stats.published, 0)
        self.assertTrue((self.alpha.repo / "channels" / "allowed-local").is_dir())
        self.assertFalse((self.alpha.repo / "channels" / "denied-local").exists())
        self.assertFalse((self.alpha.repo / "channels" / "unlisted-local").exists())

    def test_import_envelope_refuses_each_c2_clause_and_accepts_unknown_key(self):
        name = "c2"
        root = self.alpha.repo / "channels" / name
        (root / "messages").mkdir(parents=True)
        (root / "channel.json").write_text(json.dumps(channel_record(name, "alice")) + "\n")
        base = {
            "from": "alice",
            "channel": name,
            "subject": "fixture",
            "sent": "2026-09-02 17:22:00 +0000",
        }
        cases = {}
        for offset, label in enumerate(
            ("missing", "channel", "id", "sent", "event", "mentions", "re"), 500
        ):
            message_id = channel_id(offset)
            envelope = {"id": message_id, **base}
            if label == "missing":
                envelope.pop("subject")
            elif label == "channel":
                envelope["channel"] = "other"
            elif label == "id":
                envelope["id"] = channel_id(999)
            elif label == "sent":
                envelope["sent"] = "tomorrow"
            elif label == "event":
                # A non-string kind is malformed; a string kind the bridge
                # does not know is opaque and imports (see the unknown-kind
                # tests below).
                envelope["event"] = 5
            elif label == "mentions":
                envelope["mentions"] = "bob"
            elif label == "re":
                envelope["re"] = "not-an-id"
            cases[message_id] = json.dumps(envelope).encode() + b"\n---\nbody"
        huge_id = channel_id(507)
        cases[huge_id] = b"{" + b"x" * 4097 + b"\n---\nbody"
        unknown_id = channel_id(508)
        cases[unknown_id] = channel_message(
            unknown_id, "alice", name, future_extension=True
        )
        for message_id, data in cases.items():
            (root / "messages" / (message_id + ".msg")).write_bytes(data)
        self.alpha.commit("publish C2 cases")

        stats = self.import_alpha()

        self.assertEqual(stats.imported, 1)
        self.assertEqual(stats.quarantined, 8)
        self.assertEqual(len(self.log.actions("chan_unknown_key")), 1)
        self.assertTrue(
            (self.beta.root / "channels" / name / "messages" / (unknown_id + ".msg")).is_file()
        )

    def test_unknown_event_kinds_import_unchanged(self):
        # post-11n: a channel event kind this bridge has never heard of is an
        # opaque system event. It imports byte-for-byte, so a newer post on
        # one host cannot wedge the channel for the rest.
        name = "kinds-in"
        root = self.alpha.repo / "channels" / name
        (root / "messages").mkdir(parents=True)
        (root / "channel.json").write_text(json.dumps(channel_record(name, "alice")) + "\n")
        wanted = {}
        for offset, event in enumerate(("leave", "poke", "topic-change", "join", "profile"), 700):
            message_id = channel_id(offset)
            wanted[message_id] = channel_message(message_id, "alice", name, event=event)
        plain_id = channel_id(710)
        wanted[plain_id] = channel_message(plain_id, "alice", name)
        for message_id, data in wanted.items():
            (root / "messages" / (message_id + ".msg")).write_bytes(data)
        self.alpha.commit("publish event kinds")

        stats = self.import_alpha()

        self.assertEqual(stats.quarantined, 0, self.log.actions("quarantined"))
        self.assertEqual(stats.imported, len(wanted))
        for message_id, data in wanted.items():
            imported = self.beta.root / "channels" / name / "messages" / (message_id + ".msg")
            self.assertEqual(imported.read_bytes(), data)

    def test_non_string_event_kind_is_still_quarantined(self):
        name = "kinds-bad"
        root = self.alpha.repo / "channels" / name
        (root / "messages").mkdir(parents=True)
        (root / "channel.json").write_text(json.dumps(channel_record(name, "alice")) + "\n")
        bad = {}
        for offset, event in enumerate((5, ["join"], {"k": "v"}, True), 720):
            message_id = channel_id(offset)
            bad[message_id] = channel_message(message_id, "alice", name, event=event)
        for message_id, data in bad.items():
            (root / "messages" / (message_id + ".msg")).write_bytes(data)
        self.alpha.commit("publish bad event kinds")

        stats = self.import_alpha()

        self.assertEqual(stats.imported, 0)
        self.assertEqual(stats.quarantined, len(bad))

    def test_unknown_event_kinds_publish_unchanged(self):
        name = "kinds-out"
        self.alpha.join(name, "alice")
        local = self.alpha.root / "channels" / name / "messages"
        wanted = {}
        for offset, event in enumerate(("leave", "poke", "topic-change"), 730):
            message_id = channel_id(offset)
            data = channel_message(message_id, "alice", name, event=event)
            (local / (message_id + ".msg")).write_bytes(data)
            wanted[message_id] = data

        stats = self.publish(self.alpha)

        self.assertEqual(stats.unpublishable, 0, self.log.actions("channel_unpublishable"))
        for message_id, data in wanted.items():
            relayed = self.alpha.repo / "channels" / name / "messages" / (message_id + ".msg")
            self.assertEqual(relayed.read_bytes(), data)

    def test_attribution_keys_are_known_and_validated_on_import(self):
        # SPEC-v2 r5.4 (post-782): channel envelopes carry the same
        # attribution keys; address_kind is always `channel`.
        name = "attributed"
        root = self.alpha.repo / "channels" / name
        (root / "messages").mkdir(parents=True)
        (root / "channel.json").write_text(json.dumps(channel_record(name, "alice")) + "\n")
        valid_id = channel_id(600)
        valid = channel_message(
            valid_id,
            "alice",
            name,
            from_participant="alpha-remote-1",
            from_lineage="fern",
            address_kind="channel",
        )
        invalid = {
            channel_id(601): {"address_kind": "workspace"},
            channel_id(602): {"from_participant": "a:b"},
            channel_id(603): {"from_lineage": ""},
            channel_id(604): {"from_participant": ["alice"]},
        }
        (root / "messages" / (valid_id + ".msg")).write_bytes(valid)
        for message_id, extra in invalid.items():
            (root / "messages" / (message_id + ".msg")).write_bytes(
                channel_message(message_id, "alice", name, **extra)
            )
        self.alpha.commit("publish attribution cases")

        stats = self.import_alpha()

        self.assertEqual(stats.imported, 1)
        self.assertEqual(stats.quarantined, len(invalid))
        self.assertEqual(self.log.actions("chan_unknown_key"), [])
        imported = self.beta.root / "channels" / name / "messages" / (valid_id + ".msg")
        self.assertEqual(imported.read_bytes(), valid)
        for message_id in invalid:
            self.assertFalse(
                (self.beta.root / "channels" / name / "messages" / (message_id + ".msg")).exists()
            )

    def test_unhomed_stamped_senders_are_quarantined_and_plain_ones_import(self):
        # r5.5 (M2): an unhomed sender carries no remote-origin evidence post
        # can read, so any from_participant stamp is quarantined, local id
        # or not. from_lineage is no stamp; a verified stamp imports.
        name = "legacy-ids"
        local_id = self.beta.participant("bob")
        colliding, innocent, verified, plain = (
            channel_id(700),
            channel_id(701),
            channel_id(702),
            channel_id(703),
        )
        write_peer_channel(
            self.alpha,
            name,
            channel_record(name, "alice"),
            {
                colliding: channel_message(
                    colliding, "old-room", name, from_participant=local_id
                ),
                innocent: channel_message(
                    innocent, "old-room", name, from_participant="alpha-remote-2"
                ),
                verified: channel_message(
                    verified, "alice", name, from_participant=local_id
                ),
                plain: channel_message(plain, "old-room", name, from_lineage="fern"),
            },
        )

        stats = self.import_alpha(self.alpha_snapshot(legacy=True))

        self.assertEqual(stats.imported, 2)
        self.assertEqual(
            sorted(
                (item["id"], item["reason"])
                for item in self.log.actions("chan_quarantined")
            ),
            [
                (colliding, "remote_participant_unhomed"),
                (innocent, "remote_participant_unhomed"),
            ],
        )
        messages = self.beta.root / "channels" / name / "messages"
        for message_id in (colliding, innocent):
            self.assertFalse((messages / (message_id + ".msg")).exists(), message_id)
        for message_id in (verified, plain):
            self.assertTrue((messages / (message_id + ".msg")).is_file(), message_id)
        self.assertEqual(
            [item["id"] for item in self.log.actions("from-unhomed")], [plain]
        )

    def test_roomless_import_refuses_a_local_participant_id_collision(self):
        local_id = self.beta.participant("bob")
        message_id = channel_id(704)
        write_peer_channel(
            self.alpha,
            "colliding-participant",
            channel_record("colliding-participant", "alice"),
            {
                message_id: channel_message(
                    message_id, local_id, "colliding-participant",
                    from_participant=local_id, from_host="alpha",
                )
            },
        )
        stats = self.import_alpha()
        self.assertEqual(stats.imported, 0)
        self.assertEqual(stats.reasons.get("participant_id_collision"), 1)
        self.assertFalse(
            (
                self.beta.root / "channels" / "colliding-participant"
                / "messages" / (message_id + ".msg")
            ).exists()
        )

    def test_roomless_import_refuses_local_room_and_placeholder_names(self):
        name = "colliding-room"
        record = channel_record(name, "alice")
        write_peer_channel(self.alpha, name, record, {})
        self.assertEqual(self.import_alpha().imported, 0)
        members_path = self.beta.root / "channels" / name / "members.json"
        before = members_path.read_bytes()
        messages = {
            channel_id(705): channel_message(
                channel_id(705), "BoB", name,
                event="join", from_participant="BoB", from_host="alpha",
            ),
            channel_id(706): channel_message(
                channel_id(706), "bob", name,
                event="join", from_participant="bob", from_host="alpha",
            ),
            channel_id(707): channel_message(
                channel_id(707), "OBSERVER", name,
                event="join", from_participant="OBSERVER", from_host="alpha",
            ),
        }
        write_peer_channel(self.alpha, name, record, messages)

        stats = self.import_alpha(
            self.alpha_snapshot(placeholders={"alice": "alpha", "observer": "alpha"})
        )

        self.assertEqual(stats.imported, 0)
        self.assertEqual(stats.reasons.get("roomless_sender_room_collision"), 3)
        self.assertEqual(members_path.read_bytes(), before)
        for message_id in messages:
            self.assertFalse(
                (self.beta.root / "channels" / name / "messages" / (message_id + ".msg")).exists()
            )

    def test_verified_sender_post_cannot_place_is_held_then_imports_once(self):
        # r5.5 (M2): alice verifies, but post's table has no placeholder for
        # her. The message is held: not imported, not marked seen, the tip
        # stays, and health counts it per host. Another channel imports in
        # the same tick. After registration the next tick imports it once.
        held_id, other_id = channel_id(740), channel_id(741)
        write_peer_channel(
            self.alpha,
            "waiting",
            channel_record("waiting", "alice"),
            {held_id: channel_message(held_id, "alice", "waiting")},
        )
        write_peer_channel(
            self.alpha,
            "flowing",
            channel_record("flowing", "carol"),
            {other_id: channel_message(other_id, "carol", "flowing")},
        )
        carol = {"alice": "alpha", "carol": "alpha"}
        registered = str(self.beta.root / "remote" / "alpha" / "carol")

        def snapshot(post_rooms):
            return snapshot_for(
                self.beta,
                ["alpha"],
                published={"alpha": frozenset({"alice", "carol"})},
                v2_peers=("alpha",),
                placeholders=carol,
                routes=carol,
                post_rooms=post_rooms,
            )

        seen_root = self.beta.root / "bridge" / "chan-seen"

        def seen_markers():
            return sorted(str(path) for path in seen_root.rglob("*") if path.is_file())

        seen_before = seen_markers()
        held = self.tick_channels(snapshot({"carol": registered}))
        self.assertEqual(held.imported, 1)
        self.assertNotIn("alpha", held.completed)
        self.assertFalse((self.beta.root / "bridge" / "chan-tip" / "alpha").exists())
        self.assertEqual(
            [item["id"] for item in self.log.actions("chan_held")], [held_id]
        )
        self.assertEqual(held.held["alpha"]["count"], 1)
        self.assertEqual(
            channels.channels_health(held, self.beta.settings)["held"]["alpha"]["count"],
            1,
        )
        messages = self.beta.root / "channels"
        self.assertFalse((messages / "waiting" / "messages" / (held_id + ".msg")).exists())
        self.assertTrue((messages / "flowing" / "messages" / (other_id + ".msg")).is_file())
        # Held, not decided: no seen marker (they are keyed by path+oid, so
        # compare the whole tree).
        self.assertEqual(seen_markers(), seen_before)
        again = self.tick_channels(snapshot({"carol": registered}))
        self.assertEqual((again.imported, again.held["alpha"]["count"]), (0, 1))

        repaired = self.tick_channels(snapshot(None))
        self.assertEqual(repaired.imported, 1)
        self.assertIn("alpha", repaired.completed)
        self.assertEqual(repaired.held, {})
        self.assertTrue((messages / "waiting" / "messages" / (held_id + ".msg")).is_file())
        self.assertFalse(any((self.beta.root / "bridge" / "chan-held").rglob(held_id)))
        self.assertEqual(self.tick_channels(snapshot(None)).imported, 0)

    def test_unrecorded_messages_are_counted_per_host(self):
        # m7: messages waiting for their channel record are held (tip kept)
        # and appear in channels health with a count and an age.
        message_id = channel_id(750)
        root = self.alpha.repo / "channels" / "norecord" / "messages"
        root.mkdir(parents=True)
        (root / (message_id + ".msg")).write_bytes(
            channel_message(message_id, "alice", "norecord")
        )
        self.alpha.commit("message without a record")
        stats = self.tick_channels(self.alpha_snapshot())
        self.assertNotIn("alpha", stats.completed)
        self.assertEqual(stats.held["alpha"]["count"], 1)
        self.assertGreaterEqual(stats.held["alpha"]["oldest_age_seconds"], 0)

    def test_same_channel_received_marker_blocks_self_publish(self):
        self.alpha.join("self-conflict", "alice")
        self.alpha.send("self-conflict", "alice", "ours")
        message_path = next(
            path
            for path in (self.alpha.root / "channels" / "self-conflict" / "messages").glob("*.msg")
            if b'"event": "join"' not in path.read_bytes()
        )
        marker = (
            self.alpha.root
            / "bridge"
            / "chan-received"
            / "self-conflict"
            / message_path.stem
        )
        marker.parent.mkdir(parents=True)
        marker.write_text("beta " + hashlib.sha256(message_path.read_bytes()).hexdigest() + "\n")

        channels.publish_channels(
            self.alpha.settings, ALL, local_snapshot(self.alpha), self.log, self.deadline
        )

        self.assertFalse(
            (self.alpha.repo / "channels" / "self-conflict" / "messages" / message_path.name).exists()
        )
        self.assertEqual(len(self.log.actions("chan_self_conflict")), 1)

    def test_peer_message_mode_and_size_quarantines_are_forensic(self):
        link_id, large_id = channel_id(520), channel_id(521)
        write_peer_channel(
            self.alpha, "hostile", channel_record("hostile", "alice"), {}
        )
        self.alpha.write_relay(
            f"channels/hostile/messages/{link_id}.msg",
            b"target",
            mode="symlink",
        )
        self.alpha.write_relay(
            f"channels/hostile/messages/{large_id}.msg",
            channel_message(large_id, "alice", "hostile", body="x" * 2000),
        )
        settings = self.beta.settings
        settings.max_mail_bytes = 1024

        stats = channels.import_channels(
            settings,
            ALL,
            Git(self.beta.repo),
            self.alpha_snapshot(),
            self.log,
            self.deadline,
            lambda: False,
        )

        self.assertEqual(stats.quarantined, 2)
        self.assertEqual(
            {item["reason"] for item in self.log.actions("chan_quarantined")},
            {"non-regular-object", "oversize-unread"},
        )

    def test_hostile_peer_record_mode_size_and_other_host_fallback(self):
        gamma = self.topology.add("gamma", ["gary"])
        bad_mode = channels._json_bytes(channel_record("mode-record", "alice"))
        self.alpha.write_relay("channels/mode-record/channel.json", bad_mode, mode=0o755)
        self.alpha.write_relay("channels/large-record/channel.json", b"x" * 5000)
        good_id = channel_id(530)
        write_peer_channel(
            gamma,
            "mode-record",
            channel_record("mode-record", "gary"),
            {good_id: channel_message(good_id, "gary", "mode-record")},
        )
        snapshot = snapshot_for(
            self.beta,
            ["alpha", "gamma"],
            published={"alpha": frozenset({"alice"}), "gamma": frozenset({"gary"})},
            v2_peers=("alpha", "gamma"),
            placeholders={"alice": "alpha", "gary": "gamma"},
            routes={"alice": "alpha", "gary": "gamma"},
        )

        stats = channels.import_channels(
            self.beta.settings, ALL, Git(self.beta.repo), snapshot, self.log, self.deadline, lambda: False
        )

        self.assertEqual(len(self.log.actions("chan_record_invalid")), 2)
        self.assertIn("gamma", stats.completed)
        self.assertTrue(
            (self.beta.root / "channels" / "mode-record" / "messages" / (good_id + ".msg")).is_file()
        )

    def test_channels_disabled_returns_empty_stats_without_writes(self):
        before_root = sorted(path.relative_to(self.beta.root) for path in self.beta.root.rglob("*"))
        before_repo = self.beta.git("status", "--porcelain").stdout

        imported = channels.import_channels(
            self.beta.settings,
            None,
            Git(self.beta.repo),
            self.alpha_snapshot(),
            self.log,
            self.deadline,
            lambda: False,
        )
        published = channels.publish_channels(
            self.beta.settings,
            None,
            local_snapshot(self.beta),
            self.log,
            self.deadline,
        )

        self.assertEqual(imported, channels.ChannelStats())
        self.assertEqual(published, channels.ChannelStats())
        self.assertEqual(
            before_root,
            sorted(path.relative_to(self.beta.root) for path in self.beta.root.rglob("*")),
        )
        self.assertEqual(before_repo, self.beta.git("status", "--porcelain").stdout)

    def test_publish_rejections_immutable_and_record_rewrite(self):
        root = self.alpha.root / "channels" / "publish"
        messages = root / "messages"
        messages.mkdir(parents=True)
        record = channel_record("publish", "alice")
        (root / "channel.json").write_text(json.dumps(record) + "\n")
        good_id, mismatch_id, bad_id, link_id, big_id = [
            channel_id(value) for value in range(110, 115)
        ]
        (messages / (good_id + ".msg")).write_bytes(
            channel_message(good_id, "alice", "publish")
        )
        (messages / (mismatch_id + ".msg")).write_bytes(
            channel_message(good_id, "alice", "publish")
        )
        (messages / (bad_id + ".msg")).write_bytes(b"not an envelope")
        (messages / (link_id + ".msg")).symlink_to(messages / (good_id + ".msg"))
        (messages / (big_id + ".msg")).write_bytes(
            channel_message(big_id, "alice", "publish", body="x" * 2000)
        )
        settings = self.alpha.settings
        settings.max_mail_bytes = 1024
        stats = channels.publish_channels(
            settings, ALL, local_snapshot(self.alpha), self.log, self.deadline
        )
        self.assertEqual(stats.published, 1)
        self.assertEqual(stats.unpublishable, 4)
        record_target = self.alpha.repo / "channels" / "publish" / "channel.json"
        self.assertEqual(record_target.read_bytes(), channels._json_bytes(record))
        record["description"] = "new"
        (root / "channel.json").write_text(json.dumps(record) + "\n")
        channels.publish_channels(
            settings, ALL, local_snapshot(self.alpha), self.log, self.deadline
        )
        self.assertEqual(json.loads(record_target.read_text())["description"], "new")
        target = (
            self.alpha.repo / "channels" / "publish" / "messages" / (good_id + ".msg")
        )
        target.write_bytes(b"changed")
        with self.assertRaisesRegex(common.ConfigError, "channel message immutable"):
            channels.publish_channels(
                settings, ALL, local_snapshot(self.alpha), self.log, self.deadline
            )

    def test_publish_retries_file_that_vanishes_after_directory_listing(self):
        self.alpha.join("vanish", "alice")
        self.alpha.send("vanish", "alice", "survives reset")
        channels.publish_channels(
            self.alpha.settings, ALL, local_snapshot(self.alpha), self.log, self.deadline
        )
        target = next(
            path
            for path in (self.alpha.repo / "channels" / "vanish" / "messages").glob("*.msg")
            if b'"event": "join"' not in path.read_bytes()
        )
        expected = target.read_bytes()
        real_open = common.open_regular
        removed = False

        def vanish(path, maximum=None):
            nonlocal removed
            if Path(path) == target and not removed:
                target.unlink()
                removed = True
            return real_open(path, maximum)

        with mock.patch.object(common, "open_regular", side_effect=vanish):
            stats = channels.publish_channels(
                self.alpha.settings,
                ALL,
                local_snapshot(self.alpha),
                self.log,
                self.deadline,
            )

        self.assertTrue(removed)
        self.assertEqual(target.read_bytes(), expected)
        self.assertEqual(stats.published, 1)

    def test_import_recreates_existing_file_that_vanishes_before_read(self):
        message_id = channel_id(540)
        data = channel_message(message_id, "alice", "import-vanish")
        write_peer_channel(
            self.alpha,
            "import-vanish",
            channel_record("import-vanish", "alice"),
            {message_id: data},
        )
        target = (
            self.beta.root
            / "channels"
            / "import-vanish"
            / "messages"
            / (message_id + ".msg")
        )
        target.parent.mkdir(parents=True)
        target.write_bytes(data)
        real_open = common.open_regular
        removed = False

        def vanish(path, maximum=None):
            nonlocal removed
            if Path(path) == target and not removed:
                target.unlink()
                removed = True
            return real_open(path, maximum)

        with mock.patch.object(common, "open_regular", side_effect=vanish):
            stats = self.import_alpha()

        self.assertTrue(removed)
        self.assertEqual(target.read_bytes(), data)
        self.assertEqual(stats.imported, 1)

    def test_health_reads_persisted_divergence_and_rewrite_markers(self):
        diverged = [{"channel": "x", "id": channel_id(120), "hosts": ["alpha", "beta"]}]
        path = self.beta.root / "bridge" / "chan-diverged.json"
        path.write_text(json.dumps(diverged) + "\n")
        rewrite = self.beta.root / "bridge" / "chan-rewritten" / "alpha"
        rewrite.parent.mkdir(parents=True)
        rewrite.write_text("oid\n")
        health = channels.channels_health(
            channels.ChannelStats(imported=2), self.beta.settings
        )
        self.assertEqual(health["imported"], 2)
        self.assertEqual(health["diverged"], diverged)
        self.assertEqual(health["rewritten"], ["alpha"])

    def test_malformed_divergence_state_is_a_tick_error(self):
        path = self.beta.root / "bridge" / "chan-diverged.json"
        path.write_bytes(b'[{"channel":')

        with self.assertRaises(common.TickError) as raised:
            channels.channels_health(channels.ChannelStats(), self.beta.settings)

        self.assertEqual(raised.exception.reason, "chan_diverged_invalid")
        self.assertEqual(path.read_bytes(), b'[{"channel":')


def _capture(errors, function):
    try:
        function()
    except Exception as error:
        errors.append(error)



class PorchEmoteIntegrationTest(unittest.TestCase):
    setUpClass = ChannelIntegrationTest.setUpClass
    setUp = ChannelIntegrationTest.setUp
    tearDown = ChannelIntegrationTest.tearDown
    publish = ChannelIntegrationTest.publish
    alpha_snapshot = ChannelIntegrationTest.alpha_snapshot
    import_alpha = ChannelIntegrationTest.import_alpha
    tick_channels = ChannelIntegrationTest.tick_channels
    page_until_walk_ends = ChannelIntegrationTest.page_until_walk_ends
    # Inherit the existing bridge harness, not its test methods.
    def test_emotes_export_page_import_replay_and_reservations_are_silent(self):
        name = "porch-wire"
        self.alpha.join(name, "alice")
        local = self.alpha.root / "channels" / name / "messages"
        ordinary = channel_id(910)
        original = channel_message(ordinary, "alice", name, body="@bob ordinary")
        (local / (ordinary + ".msg")).write_bytes(original)
        self.publish(self.alpha)
        baseline = self.import_alpha()
        self.assertGreater(baseline.imported, 0)
        event_dir = self.beta.root / "bridge" / "events" / name
        before_events = {p.name: p.read_bytes() for p in event_dir.iterdir()}
        emotes = {}
        # Same-id .msg/.emote imports must have distinct reservations.
        for index in range(6):
            message_id = ordinary if index == 0 else channel_id(910 + index)
            data = channel_message(message_id, "alice", name, event="emote", mentions=["bob"], body="@bob")
            emotes[message_id] = data
            (local / (message_id + ".emote")).write_bytes(data)
        self.publish(self.alpha)
        with mock.patch.object(channels, "CHANNEL_TREE_MAX", 2):
            walk = self.page_until_walk_ends(self.alpha_snapshot())
        self.assertGreater(len(walk), 1, "precondition: emotes exercised paging")
        self.assertEqual(sum(t.imported for t in walk), len(emotes))
        for message_id, data in emotes.items():
            landed = self.beta.root / "channels" / name / "messages" / (message_id + ".emote")
            self.assertEqual(landed.read_bytes(), data)
            self.assertTrue(channels._message_reservation_exists(self.beta.settings, f"channels/{name}/messages/{message_id}.emote"))
            self.assertIsNotNone(channels._read_reservation(self.beta.settings, name, message_id, ".emote"))
        self.assertEqual({p.name:p.read_bytes() for p in event_dir.iterdir()}, before_events)
        self.assertEqual(self.import_alpha().imported, 0)
        self.assertEqual({p.name:p.read_bytes() for p in event_dir.iterdir()}, before_events)
        # Even a malicious pending marker cannot replay .emote as a join.
        marker=self.beta.root / "bridge" / "chan-joins-pending" / name / list(emotes)[1]
        marker.parent.mkdir(parents=True,exist_ok=True); marker.write_text("pending")
        members = (self.beta.root / "channels" / name / "members.json").read_bytes()
        self.import_alpha()
        self.assertEqual((self.beta.root / "channels" / name / "members.json").read_bytes(), members)

    def test_corrupt_emotes_are_quarantined_without_bridge_events(self):
        name="porch-corrupt"
        root=self.alpha.repo / "channels" / name
        (root / "messages").mkdir(parents=True)
        (root / "channel.json").write_text(json.dumps(channel_record(name,"alice"))+"\n")
        (root / "messages" / (channel_id(950)+".emote")).write_bytes(b"broken @bob")
        (root / "messages" / (channel_id(951)+".emote")).write_bytes(channel_message(channel_id(951),"alice",name,event="join"))
        self.alpha.commit("publish corrupt silent records")
        stats=self.import_alpha()
        self.assertEqual(stats.imported,0)
        self.assertEqual(stats.quarantined,2)
        self.assertFalse((self.beta.root / "bridge" / "events" / name).exists())


def subprocess_run(args):
    import subprocess

    return subprocess.run(
        args, text=True, capture_output=True, check=False, stdin=subprocess.DEVNULL
    )


if __name__ == "__main__":
    unittest.main(verbosity=2)
