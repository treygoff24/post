#!/usr/bin/env python3
"""End-to-end integration tests for the SPEC-v2 sweep tick."""

import ast
import hashlib
import json
import os
import shutil
import signal
import stat
import sys
import time
import unittest
from pathlib import Path
from unittest import mock

# r5.3 §One package identity: bootstrap the package path the same way
# test_channels.py and test_rooms.py do, before any bridgelib import.
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from .harness_channels import channel_id, channel_message, channel_record  # noqa: E402
from .harness_v2 import TopologyV2  # noqa: E402
from .test_sweep import (  # noqa: E402
    POST,
    SWEEP,
    SWEEPER,
    CanonicalTemporaryDirectory,
    craft_mail,
    fixed_id,
    run,
)
from bridgelib.snapshot import UNPUBLISHED_SENDER  # noqa: E402

GIT = shutil.which("git") or "git"


class TickV2Test(unittest.TestCase):
    def setUp(self):
        self.temporary = CanonicalTemporaryDirectory(prefix="post-bridge-v2-tick-")
        self.topology = TopologyV2(self.temporary.name)
        self.fc = self.topology.add("fc", ["garden"])
        self.trey = self.topology.add("trey", ["hq"])
        self.mac = self.topology.add("mac", ["porch"])
        self.topology.push_registry({"v": 1, "hosts": ["fc", "mac", "trey"]})
        self.topology.finalize(include_peers=False)

    def tearDown(self):
        self.temporary.cleanup()

    def enable_channels(self, machine, value="__default__"):
        path = machine.root / "bridge" / "config.json"
        config = json.loads(path.read_text(encoding="utf-8"))
        config["channels"] = {"mode": "all", "deny": []} if value == "__default__" else value
        path.write_text(json.dumps(config, sort_keys=True) + "\n", encoding="utf-8")

    def full_rounds(self, count=2, machines=None):
        machines = machines or (self.fc, self.trey, self.mac)
        for _ in range(count):
            for machine in machines:
                result = machine.sweep()
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_sweep_defines_no_common_exports_and_accepts_v2_config(self):
        common_path = SWEEP.parent / "bridgelib" / "common.py"
        common_tree = ast.parse(common_path.read_text(encoding="utf-8"))
        common_exports = {
            node.name
            for node in common_tree.body
            if isinstance(node, (ast.FunctionDef, ast.ClassDef))
        }
        sweep_tree = ast.parse(SWEEP.read_text(encoding="utf-8"))
        sweep_definitions = {
            node.name
            for node in sweep_tree.body
            if isinstance(node, (ast.FunctionDef, ast.ClassDef))
        }
        self.assertFalse(common_exports & sweep_definitions)

        config = json.loads(
            (self.fc.root / "bridge" / "config.json").read_text(encoding="utf-8")
        )
        config["channels"] = {"mode": "all", "deny": ["devbox-build"]}
        (self.fc.root / "bridge" / "config.json").write_text(
            json.dumps(config) + "\n", encoding="utf-8"
        )
        result = run(
            [sys.executable, SWEEP, "--check-config"],
            env=self.fc.env(),
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_sweep_keeps_no_v1_room_shadows(self):
        """Review finding 6: v1's ensure_placeholders/room_maps are unreachable.

        `grep -n 'def ensure_placeholders' sweep.py` lands a maintainer in v1
        logic inside the file that owns the tick, and that copy carried the
        per-candidate fence checks the live path was missing. A fence or
        deadline patch applied there ships nothing and tests green.
        """
        tree = ast.parse(SWEEP.read_text(encoding="utf-8"))
        definitions = {
            node.name
            for node in tree.body
            if isinstance(node, (ast.FunctionDef, ast.ClassDef))
        }
        self.assertNotIn("ensure_placeholders", definitions)
        self.assertNotIn("room_maps", definitions)
        # enforce_pinned_collisions (verify-rooms R5) re-implements by display
        # name the pinned-collision check build_snapshot now runs first, over
        # a strict superset of `post rooms` and with folding. Every case it
        # could catch is caught earlier; the real-tick exit-2 binding is
        # test_sweep's test_registration_collision_is_fatal_and_persisted.
        self.assertNotIn("enforce_pinned_collisions", definitions)

    def test_check_config_refuses_wrong_post_version(self):
        fake = Path(self.temporary.name) / "wrong-post"
        fake.write_text("#!/bin/sh\nprintf '%s\\n' 'post 9.9.9'\n", encoding="utf-8")
        fake.chmod(fake.stat().st_mode | stat.S_IXUSR)
        result = run(
            [sys.executable, SWEEP, "--check-config"],
            env=self.fc.env(POST_BIN=fake),
            check=False,
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("post 0.9.0", result.stdout)

    def check_config_with_version_line(self, line):
        fake = Path(self.temporary.name) / "annotated-post"
        fake.write_text(f"#!/bin/sh\nprintf '%s\\n' '{line}'\n", encoding="utf-8")
        fake.chmod(fake.stat().st_mode | stat.S_IXUSR)
        return run(
            [sys.executable, SWEEP, "--check-config"],
            env=self.fc.env(POST_BIN=fake),
            check=False,
        )

    def test_check_config_accepts_a_build_annotated_post_version(self):
        # `post --version` gains build metadata after the semver; the bridge
        # pins the semver, not the annotation, or a rebuilt post would stop
        # every tick on the host it was installed to.
        for line in (
            "post 0.9.0 (build abc1234, 2026-09-28)",
            "post 0.9.0 (build abc1234-dirty)",
        ):
            result = self.check_config_with_version_line(line)
            self.assertEqual(result.returncode, 0, line + result.stdout + result.stderr)

    def test_check_config_refuses_other_semvers_and_malformed_annotations(self):
        for line in (
            "post 0.9.1",
            "post 0.10.0 (build abc1234)",
            "post 0.9.0 build abc1234",
            "post 0.9.0 (build abc) trailing",
            "post 0.9.0-rc1",
            "post 0.9.0 (build (nested))",
        ):
            result = self.check_config_with_version_line(line)
            self.assertEqual(result.returncode, 2, line + result.stdout + result.stderr)
            self.assertIn("post 0.9.0", result.stdout)

    def test_check_config_refuses_a_missing_or_non_executable_post_bin(self):
        """SPEC-v2 §Onboarding: POST_BIN must exist and be executable.

        The clause was written for the trey-cell incident — a POST_BIN path
        that no longer existed — and only its third half (the pinned version
        string) was bound.
        """
        missing = Path(self.temporary.name) / "no-such-post"
        self.assertFalse(missing.exists())
        absent = run(
            [sys.executable, SWEEP, "--check-config"],
            env=self.fc.env(POST_BIN=missing),
            check=False,
        )
        self.assertEqual(absent.returncode, 2, absent.stdout + absent.stderr)
        self.assertIn("POST_BIN must be an existing executable file", absent.stdout)

        unreadable = Path(self.temporary.name) / "not-executable-post"
        unreadable.write_text(
            "#!/bin/sh\nprintf '%s\\n' 'post 0.9.0'\n", encoding="utf-8"
        )
        unreadable.chmod(0o644)
        self.assertFalse(os.access(unreadable, os.X_OK))
        blocked = run(
            [sys.executable, SWEEP, "--check-config"],
            env=self.fc.env(POST_BIN=unreadable),
            check=False,
        )
        self.assertEqual(blocked.returncode, 2, blocked.stdout + blocked.stderr)
        self.assertIn("POST_BIN must be an existing executable file", blocked.stdout)

    def test_registry_rooms_enable_unpinned_bare_name_round_trip(self):
        self.full_rounds()

        for machine, expected in (
            (self.fc, ["garden"]),
            (self.trey, ["hq"]),
            (self.mac, ["porch"]),
        ):
            published = json.loads((machine.repo / "rooms.json").read_text())
            self.assertEqual(published["rooms"], expected)

        outbound = self.fc.send("garden", "hq", "unpinned bare-name dm")
        self.assertEqual(self.fc.sweep().returncode, 0)
        delivered = self.trey.sweep()
        self.assertEqual(delivered.returncode, 0, delivered.stdout + delivered.stderr)
        self.assertIn(outbound, self.trey.inbox_ids("hq"))
        self.assertEqual(self.fc.sweep().returncode, 0)
        self.assertFalse(list((self.fc.repo / "outbox").rglob(outbound + ".mail")))

        reply = self.trey.send("hq", "garden", "reply")
        self.assertEqual(self.trey.sweep().returncode, 0)
        received = self.fc.sweep()
        self.assertEqual(received.returncode, 0, received.stdout + received.stderr)
        self.assertIn(reply, self.fc.inbox_ids("garden"))

    def test_channels_round_trip_while_third_machine_is_off(self):
        # Since r6.2 the absent key already means sync-all; enable_channels is
        # kept explicit here so the test reads as the contract it guards.
        self.enable_channels(self.fc)
        self.enable_channels(self.trey)
        self.enable_channels(self.mac, value=None)
        for _ in range(2):
            for machine in (self.fc, self.trey):
                result = machine.sweep()
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

        self.trey.post("chat", "front-porch", "--join", cwd=self.trey.workspaces["hq"])
        self.trey.post(
            "chat",
            "front-porch",
            "--send",
            "--body",
            "from trey",
            "--anyway",
            cwd=self.trey.workspaces["hq"],
        )
        self.assertEqual(self.trey.sweep().returncode, 0)
        imported = self.fc.sweep()
        self.assertEqual(imported.returncode, 0, imported.stdout + imported.stderr)
        self.fc.post("chat", "front-porch", "--join", cwd=self.fc.workspaces["garden"])
        self.fc.post(
            "chat",
            "front-porch",
            "--send",
            "--body",
            "from fc",
            "--anyway",
            cwd=self.fc.workspaces["garden"],
        )
        self.assertEqual(self.fc.sweep().returncode, 0)
        received = self.trey.sweep()
        self.assertEqual(received.returncode, 0, received.stdout + received.stderr)
        history = self.trey.post(
            "chat", "front-porch", "--history", "20", cwd=self.trey.workspaces["hq"]
        )
        self.assertIn("from fc", history.stdout)
        members = json.loads(
            (self.trey.root / "channels" / "front-porch" / "members.json").read_text()
        )
        self.assertIn("garden", members)

        mac_tick = self.mac.sweep()
        self.assertEqual(mac_tick.returncode, 0, mac_tick.stdout + mac_tick.stderr)
        self.assertTrue((self.mac.repo / "rooms.json").is_file())
        self.assertFalse(any((self.mac.repo / "channels").rglob("*.msg")))
        # The repo is the publish side. "imports nothing" is a claim
        # about the mail root, and only this assertion binds it.
        self.assertFalse((self.mac.root / "channels").exists())

    def test_roomless_channel_post_reaches_peer_and_can_receive_reply(self):
        self.enable_channels(self.mac)
        self.enable_channels(self.trey)
        self.full_rounds(2, machines=(self.mac, self.trey))
        bound = run(
            [POST, "participant", "bind", "--new", "--harness", "bridge-test"],
            cwd=self.mac.base,
            env=self.mac.env(),
        )
        sender = next(
            line.removeprefix("export POST_PARTICIPANT=")
            for line in bound.stdout.splitlines()
            if line.startswith("export POST_PARTICIPANT=")
        )
        sender_env = self.mac.env(POST_PARTICIPANT=sender)
        run([POST, "profile", "set", "--name", "Rook"], cwd=self.mac.base, env=sender_env)
        run([POST, "chat", "cross-host", "--join"], cwd=self.mac.base, env=sender_env)
        sent = run(
            [POST, "chat", "cross-host", "--send", "--body", "roomless hello", "--json"],
            cwd=self.mac.base,
            env=sender_env,
        )
        receipt = json.loads(sent.stdout)
        self.assertEqual(receipt["cross_host"]["status"], "queued")
        self.assertEqual(receipt["message"]["from"], sender)
        self.assertEqual(self.mac.sweep().returncode, 0)
        imported = self.trey.sweep()
        self.assertEqual(imported.returncode, 0, imported.stdout + imported.stderr)
        self.assertEqual(self.trey.bridge_json("health.json")["channels"]["unpublishable"], 0)
        for kind in ("chan-joins-pending", "chan-joins-held"):
            markers = self.trey.root / "bridge" / kind
            self.assertFalse(markers.exists() and any(markers.iterdir()), kind)
        members_path = self.trey.root / "channels" / "cross-host" / "members.json"
        self.assertNotIn(sender, json.loads(members_path.read_text()))
        message_dir = members_path.parent / "messages"
        join_id = next(
            envelope["id"]
            for path in message_dir.glob("*.msg")
            for envelope in [json.loads(path.read_bytes().split(b"\n---\n", 1)[0])]
            if envelope.get("event") == "join" and envelope["from"] == sender
        )
        stale = self.trey.root / "bridge" / "chan-joins-pending" / "cross-host" / join_id
        stale.parent.mkdir(parents=True, exist_ok=True)
        stale.write_bytes(b"unregistered sender")
        replay = self.trey.sweep()
        self.assertEqual(replay.returncode, 0, replay.stdout + replay.stderr)
        self.assertFalse(stale.exists())
        self.trey.post("chat", "cross-host", "--join", "--backlog", cwd=self.trey.workspaces["hq"])
        read = self.trey.post(
            "chat", "cross-host", "--history", "20", "--json",
            cwd=self.trey.workspaces["hq"],
        )
        messages = json.loads(read.stdout)["messages"]
        received = next(item for item in messages if item["id"] == receipt["message"]["id"])
        self.assertEqual(received["from_host"], "mac")
        self.assertEqual(received["origin"], "remote")
        self.assertEqual(received["display_name"], "Rook")
        self.assertEqual(received["reply_to_participant"], f"participant:{sender}@mac")
        self.assertEqual(received["reply_to_shared"], f"participant:{sender}@mac")
        catchup = self.trey.post(
            "catchup", "cross-host", "--json", cwd=self.trey.workspaces["hq"]
        )
        caught = next(
            item
            for target in json.loads(catchup.stdout)["targets"]
            for item in target["messages"]
            if item["id"] == receipt["message"]["id"]
        )
        self.assertEqual(caught["reply_to_participant"], f"participant:{sender}@mac")
        self.assertEqual(caught["reply_to_shared"], f"participant:{sender}@mac")
        text_read = self.trey.post(
            "chat", "cross-host", "--history", "20",
            cwd=self.trey.workspaces["hq"],
        )
        self.assertIn(f"{sender}@mac", text_read.stdout)
        self.trey.post(
            "send", "--to", f"participant:{sender}@mac", "--body", "reply works",
            cwd=self.trey.workspaces["hq"],
        )
        self.assertEqual(self.trey.sweep().returncode, 0)
        for kind in ("chan-joins-pending", "chan-joins-held"):
            markers = self.trey.root / "bridge" / kind
            self.assertFalse(markers.exists() and any(markers.iterdir()), kind)
        self.assertNotIn(sender, json.loads(members_path.read_text()))
        self.assertEqual(self.mac.sweep().returncode, 0)
        inbox = run([POST, "inbox", "--json"], cwd=self.mac.base, env=sender_env)
        unread = json.loads(inbox.stdout)["unread"]
        self.assertEqual(len(unread), 1)
        reply = run([POST, "read", unread[0]["id"], "--json"], cwd=self.mac.base, env=sender_env)
        self.assertIn("reply works", reply.stdout)

    def test_roomless_post_publishes_after_participant_binds_workspace(self):
        self.enable_channels(self.trey)
        self.full_rounds(2, machines=(self.trey,))
        bound = run(
            [POST, "participant", "bind", "--new", "--harness", "bridge-test"],
            cwd=self.trey.base,
            env=self.trey.env(),
        )
        sender = next(
            line.removeprefix("export POST_PARTICIPANT=")
            for line in bound.stdout.splitlines()
            if line.startswith("export POST_PARTICIPANT=")
        )
        sender_env = self.trey.env(POST_PARTICIPANT=sender)
        run([POST, "chat", "rebound-history", "--join"], cwd=self.trey.base, env=sender_env)
        sent = run(
            [POST, "chat", "rebound-history", "--body", "before rebind", "--json"],
            cwd=self.trey.base,
            env=sender_env,
        )
        message_id = json.loads(sent.stdout)["message"]["id"]
        rebound = run(
            [POST, "participant", "bind", "--workspace", "hq", "--json"],
            cwd=self.trey.workspaces["hq"],
            env=sender_env,
        )
        self.assertEqual(json.loads(rebound.stdout)["participant"]["workspace"], "hq")

        published = self.trey.sweep()
        self.assertEqual(published.returncode, 0, published.stdout + published.stderr)
        path = self.trey.repo / "channels" / "rebound-history" / "messages" / (message_id + ".msg")
        self.assertTrue(path.is_file())
        envelope = json.loads(path.read_bytes().split(b"\n---\n", 1)[0])
        self.assertEqual(envelope["from"], sender)
        self.assertEqual(envelope["from_host"], "trey")

    def test_stamped_roomless_import_adopts_unstamped_shim_copy(self):
        self.enable_channels(self.trey)
        self.enable_channels(self.mac)
        self.full_rounds(2, machines=(self.trey, self.mac))
        bound = run(
            [POST, "participant", "bind", "--new", "--harness", "bridge-test"],
            cwd=self.trey.base,
            env=self.trey.env(),
        )
        sender = next(
            line.removeprefix("export POST_PARTICIPANT=")
            for line in bound.stdout.splitlines()
            if line.startswith("export POST_PARTICIPANT=")
        )
        sender_env = self.trey.env(POST_PARTICIPANT=sender)
        run([POST, "chat", "shim-copy", "--join"], cwd=self.trey.base, env=sender_env)
        sent = run(
            [POST, "chat", "shim-copy", "--body", "copied by shim", "--json"],
            cwd=self.trey.base,
            env=sender_env,
        )
        message_id = json.loads(sent.stdout)["message"]["id"]
        source_channel = self.trey.root / "channels" / "shim-copy"
        local_bytes = (source_channel / "messages" / (message_id + ".msg")).read_bytes()
        receiver_channel = self.mac.root / "channels" / "shim-copy"
        (receiver_channel / "messages").mkdir(parents=True)
        (receiver_channel / "channel.json").write_bytes((source_channel / "channel.json").read_bytes())
        local_copy = receiver_channel / "messages" / (message_id + ".msg")
        local_copy.write_bytes(local_bytes)

        self.assertEqual(self.trey.sweep().returncode, 0)
        stamped = (self.trey.repo / "channels" / "shim-copy" / "messages" / (message_id + ".msg")).read_bytes()
        self.assertNotEqual(stamped, local_bytes)
        imported = self.mac.sweep()

        self.assertEqual(imported.returncode, 0, imported.stdout + imported.stderr)
        health = self.mac.bridge_json("health.json")
        self.assertTrue(health["ok"], health)
        self.assertEqual(health["channels"]["diverged"], [])
        self.assertEqual(local_copy.read_bytes(), local_bytes)
        reservation = self.mac.root / "bridge" / "chan-received" / "shim-copy" / message_id
        self.assertEqual(
            reservation.read_text().strip(),
            f"trey {hashlib.sha256(stamped).hexdigest()}",
        )
        tip = self.mac.root / "bridge" / "chan-tip" / "trey"
        self.assertTrue(tip.is_file())
        tip.unlink()
        replay = self.mac.sweep()
        self.assertEqual(replay.returncode, 0, replay.stdout + replay.stderr)
        self.assertEqual(local_copy.read_bytes(), local_bytes)
        self.assertEqual(self.mac.bridge_json("health.json")["channels"]["diverged"], [])

    def test_absent_channels_key_defaults_to_sync_all(self):
        # SPEC-v2 r6.2 (Trey ruling 2026-09-24): no "channels" key in
        # config.json means {"mode": "all"} — a fresh host syncs every
        # channel without configuration.
        self.trey.post("chat", "front-porch", "--join", cwd=self.trey.workspaces["hq"])
        self.trey.post(
            "chat",
            "front-porch",
            "--send",
            "--body",
            "from trey",
            "--anyway",
            cwd=self.trey.workspaces["hq"],
        )
        self.assertEqual(self.trey.sweep().returncode, 0)
        imported = self.fc.sweep()
        self.assertEqual(imported.returncode, 0, imported.stdout + imported.stderr)
        self.fc.post("chat", "front-porch", "--join", cwd=self.fc.workspaces["garden"])
        history = self.fc.post(
            "chat", "front-porch", "--history", "20", cwd=self.fc.workspaces["garden"]
        )
        self.assertIn("from trey", history.stdout)

    def test_null_channels_key_opts_out(self):
        # The pre-r6 escape hatch: an explicit null keeps channel sync off.
        self.enable_channels(self.fc, value=None)
        self.trey.post("chat", "front-porch", "--join", cwd=self.trey.workspaces["hq"])
        self.trey.post(
            "chat",
            "front-porch",
            "--send",
            "--body",
            "from trey",
            "--anyway",
            cwd=self.trey.workspaces["hq"],
        )
        self.assertEqual(self.trey.sweep().returncode, 0)
        imported = self.fc.sweep()
        self.assertEqual(imported.returncode, 0, imported.stdout + imported.stderr)
        self.assertFalse((self.fc.root / "channels" / "front-porch").exists())
        self.assertFalse(any((self.fc.repo / "channels").rglob("*.msg")))

    def test_unknown_room_receipt_flips_to_one_delivery_when_room_appears(self):
        self.full_rounds()
        mail_id = fixed_id(930)
        self.fc.inject(
            f"outbox/trey/nowhere/{mail_id}.mail",
            craft_mail(mail_id, "garden", "nowhere"),
        )
        first = self.trey.sweep()
        self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
        receipt_path = (
            self.trey.repo / "receipts" / "fc" / "nowhere" / (mail_id + ".json")
        )
        self.assertEqual(json.loads(receipt_path.read_text())["reason"], "unknown_room")
        workspace = self.trey.base / "rooms" / "nowhere"
        workspace.mkdir()
        self.trey.post("rooms", "add", "nowhere", workspace)
        second = self.trey.sweep()
        self.assertEqual(second.returncode, 0, second.stdout + second.stderr)
        self.assertEqual(json.loads(receipt_path.read_text())["status"], "delivered")
        copies = [
            path
            for path in (
                self.trey.root / "nowhere" / "inbox" / (mail_id + ".mail"),
                self.trey.root / "nowhere" / "read" / (mail_id + ".mail"),
            )
            if path.exists()
        ]
        self.assertEqual(len(copies), 1)

    def test_contested_route_does_not_strand_existing_outbox(self):
        self.full_rounds()
        mail_id = self.fc.send("garden", "hq", "queued before contest")
        self.assertEqual(self.fc.sweep().returncode, 0)
        self.mac.push_rooms(
            (json.dumps({"v": 1, "host": "mac", "rooms": ["hq", "porch"]}) + "\n").encode()
        )
        contested = self.fc.sweep()
        self.assertEqual(contested.returncode, 1, contested.stdout + contested.stderr)
        self.assertTrue(
            (self.fc.root / "bridge" / "published" / mail_id).is_file()
        )
        delivered = self.trey.sweep()
        self.assertEqual(delivered.returncode, 1, delivered.stdout + delivered.stderr)
        pruned = self.fc.sweep()
        self.assertEqual(pruned.returncode, 1, pruned.stdout + pruned.stderr)
        self.assertFalse(list((self.fc.repo / "outbox").rglob(mail_id + ".mail")))

        waiting = self.fc.send("garden", "hq", "new while contested")
        blocked = self.fc.sweep()
        self.assertEqual(blocked.returncode, 1, blocked.stdout + blocked.stderr)
        self.assertFalse(list((self.fc.repo / "outbox").rglob(waiting + ".mail")))
        self.assertTrue(
            any(
                record["action"] == "route_contested" and record.get("id") == waiting
                for record in self.fc.logs()
            )
        )

    def test_route_contested_is_logged_for_a_name_whose_fold_differs(self):
        """r5.3 §Names fold: `contested` is fold-keyed, `to` is a display name.

        The guard sits inside `if host is None:` and the branch continues
        either way, so the mail is held regardless; what is lost is the
        route_contested diagnostic, for exactly the names — the ones whose
        spelling differs from their fold — an operator most needs to see.
        """
        self.full_rounds()
        self.trey.push_rooms(
            (
                json.dumps({"v": 1, "host": "trey", "rooms": ["Case", "hq"]}) + "\n"
            ).encode()
        )
        claimed = self.fc.sweep()
        self.assertEqual(claimed.returncode, 0, claimed.stdout + claimed.stderr)
        self.assertTrue(
            (self.fc.root / "remote" / "trey" / "Case").is_dir(),
            "the uncontested name must register a placeholder first",
        )

        mail_id = self.fc.send("garden", "Case", "queued before the contest")
        self.mac.push_rooms(
            (
                json.dumps({"v": 1, "host": "mac", "rooms": ["case", "porch"]}) + "\n"
            ).encode()
        )

        contested = self.fc.sweep()
        self.assertEqual(contested.returncode, 1, contested.stdout + contested.stderr)
        self.assertFalse(list((self.fc.repo / "outbox").rglob(mail_id + ".mail")))
        self.assertTrue(
            any(
                record["action"] == "route_contested"
                and record.get("id") == mail_id
                and record.get("room") == "Case"
                for record in self.fc.logs()
            ),
            "route_contested was not logged for the contested display name",
        )

    def test_registry_enrollment_removal_and_local_restriction(self):
        self.full_rounds()
        self.topology.push_registry({"v": 1, "hosts": ["fc", "trey"]})
        self.mac.push_rooms(
            (json.dumps({"v": 1, "host": "mac", "rooms": ["hq", "porch"]}) + "\n").encode()
        )
        removed = self.fc.sweep()
        self.assertEqual(removed.returncode, 0, removed.stdout + removed.stderr)
        self.assertTrue(
            any(
                record["action"] == "ignored_branch"
                and record.get("host") == "mac"
                for record in self.fc.logs()
            )
        )

        self.topology.push_registry({"v": 1, "hosts": ["fc", "mac", "trey"]})
        enrolled = self.fc.sweep()
        self.assertEqual(enrolled.returncode, 1, enrolled.stdout + enrolled.stderr)
        self.assertEqual(
            json.loads((self.fc.root / "bridge" / "health.json").read_text())["reason"],
            "room_name_collision",
        )

        self.mac.push_rooms(
            (json.dumps({"v": 1, "host": "mac", "rooms": ["porch"]}) + "\n").encode()
        )
        config_path = self.fc.root / "bridge" / "config.json"
        config = json.loads(config_path.read_text())
        config["peers"] = {"trey": []}
        config_path.write_text(json.dumps(config) + "\n", encoding="utf-8")
        restricted = self.fc.sweep()
        self.assertEqual(restricted.returncode, 0, restricted.stdout + restricted.stderr)
        current = [record for record in self.fc.logs() if record["action"] == "ignored_branch"]
        self.assertEqual(current[-1]["host"], "mac")

    def test_post_fetch_snapshot_sees_room_and_mail_from_same_tip(self):
        self.full_rounds()
        workspace = self.mac.base / "rooms" / "nova"
        workspace.mkdir()
        self.mac.post("rooms", "add", "nova", workspace)
        mail_id = fixed_id(940)
        rooms_payload = {"v": 1, "host": "mac", "rooms": ["nova", "porch"]}
        (self.mac.repo / "rooms.json").write_text(
            json.dumps(rooms_payload, sort_keys=True) + "\n", encoding="utf-8"
        )
        target = self.mac.repo / "outbox" / "trey" / "hq" / (mail_id + ".mail")
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(craft_mail(mail_id, "nova", "hq"))
        self.mac.git("add", "--", "rooms.json", "outbox")
        self.mac.git("commit", "-q", "-m", "publish nova and its first mail")
        self.mac.git("push", "-q", "origin", "HEAD:refs/heads/machines/mac")

        result = self.trey.sweep()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue((self.trey.root / "remote" / "mac" / "nova").is_dir())
        self.assertIn(mail_id, self.trey.inbox_ids("hq"))

    def test_killed_post_fetch_tick_does_not_advance_checkpoint(self):
        first = self.fc.sweep()
        self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
        checkpoint_path = self.fc.root / "bridge" / "trigger-fingerprint.json"
        before = checkpoint_path.read_bytes()
        self.topology.push_registry({"v": 1, "hosts": ["trey", "mac", "fc"]})
        killed = self.fc.sweep(BRIDGE_CRASH_AFTER="after-fetch")
        self.assertEqual(killed.returncode, -signal.SIGKILL)
        self.assertEqual(checkpoint_path.read_bytes(), before)
        fetches = sum(record["action"] == "fetch" for record in self.fc.logs())
        recovered = self.fc.sweep()
        self.assertEqual(recovered.returncode, 0, recovered.stdout + recovered.stderr)
        self.assertEqual(
            sum(record["action"] == "fetch" for record in self.fc.logs()),
            fetches + 1,
        )

    def peer_record_bytes(self, channel, author, **overrides):
        record = {**channel_record(channel, author), **overrides}
        return json.dumps(record, sort_keys=True).encode("utf-8") + b"\n"

    def test_tip_freezes_for_an_invalid_record_and_delivers_after_repair(self):
        """SPEC-v2 §Bounded import: the tick advances exactly the completed tips.

        A channel skipped for a recoverable reason (`chan_record_invalid`)
        must freeze `bridge/chan-tip/<H>`; otherwise the repaired record
        arrives as an `M` row and the message under it is never listed
        again.
        """
        for machine in (self.fc, self.trey):
            self.enable_channels(machine)
        self.full_rounds()
        tip_path = self.fc.root / "bridge" / "chan-tip" / "trey"
        self.assertTrue(tip_path.is_file())
        frozen_at = tip_path.read_text(encoding="utf-8").strip()

        message_id = channel_id(41)
        self.trey.inject(
            "channels/lamp/channel.json",
            self.peer_record_bytes("lamp", "hq", name="not-lamp"),
        )
        self.trey.inject(
            f"channels/lamp/messages/{message_id}.msg",
            channel_message(message_id, "hq", "lamp"),
        )

        result = self.fc.sweep()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        imported = (
            self.fc.root / "channels" / "lamp" / "messages" / f"{message_id}.msg"
        )
        self.assertFalse(imported.exists())
        self.assertEqual(
            tip_path.read_text(encoding="utf-8").strip(),
            frozen_at,
            "chan-tip advanced past a channel this tick never imported",
        )

        self.trey.inject(
            "channels/lamp/channel.json", self.peer_record_bytes("lamp", "hq")
        )
        repaired = self.fc.sweep()
        self.assertEqual(repaired.returncode, 0, repaired.stdout + repaired.stderr)
        self.assertTrue(
            imported.is_file(), "the repaired channel never delivered its message"
        )
        self.assertEqual(
            tip_path.read_text(encoding="utf-8").strip(),
            self.trey.git("rev-parse", "HEAD").stdout.strip(),
        )

    def test_tip_is_frozen_for_a_host_that_rewrote_channel_history(self):
        """SPEC-v2 §Tests "rewrite: … tip frozen", through a real sweep."""
        for machine in (self.fc, self.trey):
            self.enable_channels(machine)
        self.full_rounds()
        message_id = channel_id(42)
        self.trey.inject(
            "channels/lamp/channel.json", self.peer_record_bytes("lamp", "hq")
        )
        self.trey.inject(
            f"channels/lamp/messages/{message_id}.msg",
            channel_message(message_id, "hq", "lamp"),
        )
        result = self.fc.sweep()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        imported = (
            self.fc.root / "channels" / "lamp" / "messages" / f"{message_id}.msg"
        )
        self.assertTrue(imported.is_file())
        tip_path = self.fc.root / "bridge" / "chan-tip" / "trey"
        frozen_at = tip_path.read_text(encoding="utf-8").strip()
        self.assertEqual(
            frozen_at, self.trey.git("rev-parse", "HEAD").stdout.strip()
        )

        (self.trey.repo / "channels" / "lamp" / "messages" / f"{message_id}.msg").unlink()
        self.trey.git("add", "-A", "--", "channels")
        self.trey.git("commit", "-q", "-m", "rewrite published channel history")
        self.trey.git("push", "-q", "origin", "HEAD:refs/heads/machines/trey")
        rewritten_head = self.trey.git("rev-parse", "HEAD").stdout.strip()

        after = self.fc.sweep()
        self.assertEqual(after.returncode, 1, after.stdout + after.stderr)
        health = json.loads(
            (self.fc.root / "bridge" / "health.json").read_text(encoding="utf-8")
        )
        self.assertFalse(health["ok"])
        self.assertIn("trey", health["channels"]["rewritten"])
        self.assertNotEqual(rewritten_head, frozen_at)
        self.assertEqual(
            tip_path.read_text(encoding="utf-8").strip(),
            frozen_at,
            "chan-tip advanced for a host that rewrote channel history",
        )

    def test_archive_check_runs_once_per_tick_per_host(self):
        """Review finding 4: sweep and import_channels both checked the archive.

        `check_archive` issues a `git diff --name-status <tip>..<oid>` per
        peer, so running it twice doubles the cost of the one operation the
        15 s timer and the quiet tick exist to avoid, and it emits
        `relay_history_rewritten` twice for a rewritten peer.
        """
        for machine in (self.fc, self.trey):
            self.enable_channels(machine)
        self.full_rounds()
        message_id = channel_id(43)
        self.trey.inject(
            "channels/lamp/channel.json", self.peer_record_bytes("lamp", "hq")
        )
        self.trey.inject(
            f"channels/lamp/messages/{message_id}.msg",
            channel_message(message_id, "hq", "lamp"),
        )
        self.assertEqual(self.fc.sweep().returncode, 0)
        (self.trey.repo / "channels" / "lamp" / "messages" / f"{message_id}.msg").unlink()
        self.trey.git("add", "-A", "--", "channels")
        self.trey.git("commit", "-q", "-m", "rewrite published channel history")
        self.trey.git("push", "-q", "origin", "HEAD:refs/heads/machines/trey")

        for expected_tick in ("first", "steady state"):
            with self.subTest(tick=expected_tick):
                seen = len(self.fc.logs())
                self.assertEqual(self.fc.sweep().returncode, 1)
                records = self.fc.logs()[seen:]
                self.assertEqual(
                    [
                        record
                        for record in records
                        if record["action"] == "relay_history_rewritten"
                        and record["host"] == "trey"
                    ][1:],
                    [],
                    "relay_history_rewritten logged more than once this tick",
                )

    def test_ten_idle_ticks_create_no_commits(self):
        self.full_rounds()
        heads = {
            machine.host: machine.git("rev-parse", "HEAD").stdout.strip()
            for machine in (self.fc, self.trey, self.mac)
        }
        for _ in range(10):
            for machine in (self.fc, self.trey, self.mac):
                result = machine.sweep()
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(
            {
                machine.host: machine.git("rev-parse", "HEAD").stdout.strip()
                for machine in (self.fc, self.trey, self.mac)
            },
            heads,
        )

    def test_peer_push_during_a_tick_is_not_swallowed_by_the_checkpoint(self):
        """SPEC-v2 §Latency: the checkpoint means "consumed as of this tick".

        The tick pins peer OIDs at step 6 and imports from those OIDs. If
        step 17 takes a fresh probe, anything a peer pushed after the fetch
        is recorded as though this tick had processed it, and the next three
        ticks go quiet without fetching — ~60 s on a 15 s timer whose whole
        purpose is <= ~30 s worst case.
        """
        self.full_rounds(3)
        mail_id = fixed_id(777)
        planted = {"done": False}

        def plant(name):
            if name == "before-publish" and not planted["done"]:
                planted["done"] = True
                self.trey.inject(
                    f"outbox/fc/garden/{mail_id}.mail",
                    craft_mail(mail_id, "hq", "garden"),
                )

        (self.fc.root / "bridge" / "trigger-fingerprint.json").unlink()
        with mock.patch.dict(os.environ, self.fc.env(), clear=True), mock.patch.object(
            SWEEPER, "checkpoint", side_effect=plant
        ):
            self.assertEqual(SWEEPER.execute(), 0)
        self.assertTrue(planted["done"], "plant never fired")

        inbox = self.fc.root / "garden" / "inbox" / f"{mail_id}.mail"
        self.assertFalse(
            inbox.exists(), "the racing tick pinned OIDs before the push"
        )
        fingerprint = json.loads(
            (self.fc.root / "bridge" / "trigger-fingerprint.json").read_text(
                encoding="utf-8"
            )
        )
        raced_head = self.trey.git("rev-parse", "HEAD").stdout.strip()
        self.assertNotEqual(
            fingerprint["heads"].get("refs/heads/machines/trey"),
            raced_head,
            "the checkpoint recorded a trey head this tick never fetched",
        )

        fetches = sum(record["action"] == "fetch" for record in self.fc.logs())
        result = self.fc.sweep()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        health = json.loads(
            (self.fc.root / "bridge" / "health.json").read_text(encoding="utf-8")
        )
        self.assertFalse(
            health["quiet"], "the tick after a raced peer push must be full"
        )
        self.assertEqual(
            sum(record["action"] == "fetch" for record in self.fc.logs()),
            fetches + 1,
        )
        self.assertTrue(inbox.is_file(), "the raced letter was never delivered")

    def test_fence_before_each_integration_batch_preserves_checkpoint(self):
        self.full_rounds()
        checkpoint_path = self.fc.root / "bridge" / "trigger-fingerprint.json"
        for stage in ("before-inbound", "before-channel-import", "before-publish"):
            with self.subTest(stage=stage):
                before_checkpoint = checkpoint_path.read_bytes()
                before_head = self.fc.git("rev-parse", "HEAD").stdout.strip()

                def plant(name):
                    if name == stage:
                        (self.fc.root / ".post-arx.json").write_text(
                            '{"state":"fenced","generation":1}\n', encoding="utf-8"
                        )

                config_path = self.fc.root / "bridge" / "config.json"
                config_path.write_bytes(config_path.read_bytes())
                with mock.patch.dict(os.environ, self.fc.env(), clear=True), mock.patch.object(
                    SWEEPER, "checkpoint", side_effect=plant
                ):
                    result = SWEEPER.execute()
                self.assertEqual(result, 1)
                self.assertEqual(checkpoint_path.read_bytes(), before_checkpoint)
                self.assertEqual(
                    self.fc.git("rev-parse", "HEAD").stdout.strip(), before_head
                )
                (self.fc.root / ".post-arx.json").unlink()

    def test_unhealthy_or_failed_quiet_probe_forces_full_tick(self):
        first = self.fc.sweep()
        self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
        health_path = self.fc.root / "bridge" / "health.json"
        health = json.loads(health_path.read_text())
        health["ok"] = False
        health["reason"] = "test-unhealthy"
        health_path.write_text(json.dumps(health) + "\n", encoding="utf-8")
        fetches = sum(record["action"] == "fetch" for record in self.fc.logs())
        forced = self.fc.sweep()
        self.assertEqual(forced.returncode, 0, forced.stdout + forced.stderr)
        self.assertEqual(
            sum(record["action"] == "fetch" for record in self.fc.logs()),
            fetches + 1,
        )

        offline = self.topology.forge.with_name("forge.quiet-probe-offline")
        self.topology.forge.rename(offline)
        try:
            fetches = sum(record["action"] == "fetch" for record in self.fc.logs())
            failed_probe = self.fc.sweep()
            self.assertEqual(
                failed_probe.returncode, 0, failed_probe.stdout + failed_probe.stderr
            )
            self.assertEqual(
                sum(record["action"] == "fetch" for record in self.fc.logs()),
                fetches + 1,
            )
            self.assertFalse(json.loads(health_path.read_text())["quiet"])
        finally:
            offline.rename(self.topology.forge)

    def test_failed_fetch_does_not_checkpoint_the_quiet_fingerprint(self):
        # m5: a full tick whose fetch failed read stale pinned refs. If it
        # checkpointed the ls-remote heads it probed, the next tick would
        # see a matching fingerprint and go quiet over undelivered mail.
        self.full_rounds(3)
        mail_id = fixed_id(0x5501)
        mail = craft_mail(mail_id, "hq", "garden", body=b"behind a failed fetch")
        self.trey.inject(f"outbox/fc/garden/{mail_id}.mail", mail)
        wrapper_dir = self.fc.base / "failing-fetch-wrapper"
        wrapper_dir.mkdir()
        fired = wrapper_dir / "fired"
        wrapper = wrapper_dir / "git"
        wrapper.write_text(
            "#!/bin/sh\n"
            "for arg do\n"
            '  if [ "$arg" = fetch ]; then\n'
            f"    : > {str(fired)!r}\n"
            "    echo 'fatal: simulated network failure' >&2\n"
            "    exit 128\n"
            "  fi\n"
            "done\n"
            f"exec {GIT!r} \"$@\"\n",
            encoding="utf-8",
        )
        wrapper.chmod(0o755)
        health_path = self.fc.root / "bridge" / "health.json"
        delivered = [
            self.fc.root / "garden" / box / (mail_id + ".mail")
            for box in ("inbox", "read")
        ]

        failed = self.fc.sweep(PATH=f"{wrapper_dir}{os.pathsep}{os.environ['PATH']}")
        self.assertEqual(failed.returncode, 0, failed.stdout + failed.stderr)
        self.assertTrue(fired.exists(), "wrapper never fired")
        health = json.loads(health_path.read_text())
        # Preconditions: a full tick, still healthy inside the fetch grace,
        # so nothing but the checkpoint can force the next tick full.
        self.assertEqual((health["ok"], health["quiet"]), (True, False))
        self.assertFalse(any(path.exists() for path in delivered))

        fetches = sum(record["action"] == "fetch" for record in self.fc.logs())
        after = self.fc.sweep()
        self.assertEqual(after.returncode, 0, after.stdout + after.stderr)
        self.assertFalse(json.loads(health_path.read_text())["quiet"])
        self.assertEqual(
            sum(record["action"] == "fetch" for record in self.fc.logs()), fetches + 1
        )
        self.assertEqual(
            [path.read_bytes() for path in delivered if path.exists()], [mail]
        )

    def test_held_channel_message_imports_once_after_repair_while_others_flow(self):
        # r5.5 (M2), Aster: fc and mac both claim `fern`; fc is owner of
        # record, so fc's `fern` verifies, but the contested name has no
        # placeholder and post would not read it as remote. The channel
        # message is held while fc's other channel and mac's mail flow in
        # the same ticks; mac withdrawing `fern` repairs it.
        self.enable_channels(self.trey)
        self.full_rounds()

        def publish(machine, names):
            machine.inject(
                "rooms.json",
                (
                    json.dumps(
                        {"v": 1, "host": machine.host, "rooms": sorted(names)},
                        sort_keys=True,
                    )
                    + "\n"
                ).encode(),
            )

        def channel(machine, name, message_id, sender):
            machine.inject(
                f"channels/{name}/channel.json",
                (json.dumps(channel_record(name, "garden"), sort_keys=True) + "\n").encode(),
            )
            machine.inject(
                f"channels/{name}/messages/{message_id}.msg",
                channel_message(message_id, sender, name),
            )

        def imported(name, message_id):
            return self.trey.root / "channels" / name / "messages" / (message_id + ".msg")

        def health():
            return json.loads((self.trey.root / "bridge" / "health.json").read_text())

        publish(self.fc, ["fern", "garden"])
        publish(self.mac, ["fern", "porch"])
        held_id, flowing_id = channel_id(860), channel_id(861)
        channel(self.fc, "fern-talk", held_id, "fern")
        channel(self.fc, "garden-talk", flowing_id, "garden")
        first_mail_id = fixed_id(0x8601)
        first_mail = craft_mail(first_mail_id, "porch", "hq")
        self.mac.inject(f"outbox/trey/hq/{first_mail_id}.mail", first_mail)

        contested = self.trey.sweep()
        self.assertEqual(contested.returncode, 1, contested.stdout + contested.stderr)
        owners = json.loads(
            (self.trey.root / "bridge" / "rooms" / "owners.json").read_text()
        )
        self.assertEqual(owners["fern"]["host"], "fc")  # precondition
        self.assertFalse(imported("fern-talk", held_id).exists())
        self.assertTrue(imported("garden-talk", flowing_id).is_file())
        self.assertTrue(
            (self.trey.root / "hq" / "inbox" / (first_mail_id + ".mail")).is_file()
        )
        self.assertEqual(health()["channels"]["held"]["fc"]["count"], 1)
        self.assertIn(
            held_id,
            [r.get("id") for r in self.trey.logs() if r["action"] == "chan_held"],
        )

        publish(self.mac, ["porch"])
        second_mail_id = fixed_id(0x8602)
        second_mail = craft_mail(second_mail_id, "porch", "hq")
        self.mac.inject(f"outbox/trey/hq/{second_mail_id}.mail", second_mail)
        repaired = self.trey.sweep()
        self.assertEqual(repaired.returncode, 0, repaired.stdout + repaired.stderr)
        self.assertTrue(imported("fern-talk", held_id).is_file())
        self.assertTrue(
            (self.trey.root / "hq" / "inbox" / (second_mail_id + ".mail")).is_file()
        )
        self.assertEqual(health()["channels"]["held"], {})
        self.full_rounds(1, machines=(self.trey,))
        self.assertEqual(
            [
                r.get("id")
                for r in self.trey.logs()
                if r["action"] == "chan_imported"
            ].count(held_id),
            1,
        )

    def test_health_schema_and_channel_divergence(self):
        self.enable_channels(self.fc)
        self.full_rounds()
        channel = "fault-line"
        message_id = "20260902-180000-000001-abcdef"
        record_mac = {
            "name": channel,
            "created": "2026-09-02 18:00:00 +0000",
            "created_by": "porch",
        }
        record_trey = dict(record_mac, created_by="hq")

        def message(sender, body):
            envelope = {
                "id": message_id,
                "from": sender,
                "channel": channel,
                "subject": "",
                "sent": "2026-09-02 18:00:00 +0000",
            }
            return json.dumps(envelope, sort_keys=True).encode() + b"\n---\n" + body

        for machine, record, sender, body in (
            (self.mac, record_mac, "porch", b"mac bytes"),
            (self.trey, record_trey, "hq", b"trey bytes"),
        ):
            machine.inject(
                f"channels/{channel}/channel.json",
                (json.dumps(record, sort_keys=True) + "\n").encode(),
            )
            machine.inject(
                f"channels/{channel}/messages/{message_id}.msg",
                message(sender, body),
            )
        result = self.fc.sweep()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        health = json.loads((self.fc.root / "bridge" / "health.json").read_text())
        self.assertEqual(health["reason"], "channel_diverged")
        self.assertFalse(health["quiet"])
        self.assertEqual(
            set(health),
            {
                "ts",
                "ok",
                "reason",
                "first_tick_at",
                "last_fetch_ok",
                "last_push_ok",
                "stalled_since",
                "busy_streak",
                "held",
                "quarantined",
                "outbound_unrelayable",
                "rooms",
                "channels",
                "git_failed",
                "sender_not_homed",
                # F3 (SPEC-v2 r6.0): participant-mail counts and the
                # capability fields post's send guard reads.
                "pmail",
                "capabilities",
                "ticked_at",
                "interval_s",
                # r6.1: the local-held export guard's counts.
                "local_held",
                "quiet",
                "quiet_streak",
                # Task 3: what needs a person, separate from `ok`.
                "attention",
            },
        )
        self.assertTrue(health["channels"]["diverged"])

    def test_quiet_ticks_skip_fetch_preserve_health_and_force_fourth_full(self):
        first = self.fc.sweep()
        self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
        # The first tick registers placeholders, writes rooms.json and pushes,
        # so the world moved after the start-of-tick probe it checkpoints.
        # Exactly one more full tick settles that, and then quiet is
        # permitted; this is the shape SPEC-v2 §Latency asks for, with the
        # checkpoint meaning "consumed as of the start of this tick".
        settling = self.fc.sweep()
        self.assertEqual(settling.returncode, 0, settling.stdout + settling.stderr)
        before = json.loads(
            (self.fc.root / "bridge" / "health.json").read_text(encoding="utf-8")
        )
        self.assertFalse(
            before["quiet"], "the tick after a pushing tick must be full"
        )
        fetches = sum(record["action"] == "fetch" for record in self.fc.logs())
        time.sleep(1.05)

        for streak in (1, 2, 3):
            result = self.fc.sweep()
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            health = json.loads(
                (self.fc.root / "bridge" / "health.json").read_text(encoding="utf-8")
            )
            self.assertTrue(health["quiet"])
            self.assertEqual(health["quiet_streak"], streak)
            self.assertTrue(health["ok"])
        self.assertGreater(health["ts"], before["ts"])
        self.assertEqual(
            sum(record["action"] == "fetch" for record in self.fc.logs()), fetches
        )

        fourth = self.fc.sweep()
        self.assertEqual(fourth.returncode, 0, fourth.stdout + fourth.stderr)
        health = json.loads(
            (self.fc.root / "bridge" / "health.json").read_text(encoding="utf-8")
        )
        self.assertFalse(health["quiet"])
        self.assertEqual(health["quiet_streak"], 0)
        self.assertEqual(
            sum(record["action"] == "fetch" for record in self.fc.logs()), fetches + 1
        )


class TickV2QuietMatrixTest(unittest.TestCase):
    """One row per SPEC-v2 §Latency fingerprint input, perturbed alone."""

    def setUp(self):
        self.temporary = CanonicalTemporaryDirectory(prefix="post-bridge-v2-quiet-")
        self.topology = TopologyV2(self.temporary.name)
        self.fc = self.topology.add("fc", ["garden"])
        self.trey = self.topology.add("trey", ["hq"])
        self.topology.push_registry({"v": 1, "hosts": ["fc", "trey"]})
        self.topology.finalize(include_peers=False)
        for machine in (self.fc, self.trey):
            path = machine.root / "bridge" / "config.json"
            config = json.loads(path.read_text(encoding="utf-8"))
            config["channels"] = {"mode": "all", "deny": []}
            path.write_text(json.dumps(config, sort_keys=True) + "\n", encoding="utf-8")
        for _ in range(2):
            for machine in (self.fc, self.trey):
                result = machine.sweep()
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.fc.post("chat", "probe", "--join", cwd=self.fc.workspaces["garden"])
        self.pending = (
            self.fc.root / "bridge" / "chan-joins-pending" / "probe" / "quiet-matrix"
        )
        self.fence = self.fc.root / ".post-arx.json"
        self.stray = self.fc.repo / "quiet-matrix-stray"

    def tearDown(self):
        self.temporary.cleanup()

    def health(self):
        return json.loads(
            (self.fc.root / "bridge" / "health.json").read_text(encoding="utf-8")
        )

    def settle(self):
        """Sweep until a tick goes quiet: the checkpoint now matches the world."""
        for _ in range(10):
            result = self.fc.sweep()
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            if self.health().get("quiet"):
                return
        self.fail("fc never reached a quiet tick")

    def rows(self):
        channel = self.fc.root / "channels" / "probe"
        for path in (
            self.fc.root / "bridge" / "config.json",
            self.fc.root / "rooms.json",
            self.fc.root / "rules.json",
            self.fc.root / "archive",
            self.fc.root / "channels",
            channel,
            channel / "messages",
        ):
            self.assertTrue(path.exists(), f"fingerprint input missing: {path}")

        def touch(path):
            return lambda: os.utime(path)

        def push_peer_head():
            self.trey.inject(
                f"outbox/mac/porch/{fixed_id(880)}.mail",
                craft_mail(fixed_id(880), "hq", "porch"),
            )

        def raise_fence():
            self.fence.write_text(
                '{"state":"fenced","generation":1}\n', encoding="utf-8"
            )

        def hold_a_join():
            self.pending.parent.mkdir(parents=True, exist_ok=True)
            self.pending.write_bytes(b"")

        def drop_the_join():
            self.pending.unlink()
            self.pending.parent.rmdir()

        def dirty_worktree():
            self.stray.write_text("untracked\n", encoding="utf-8")

        def move_head_off_remote():
            self.fc.git("commit", "-q", "--allow-empty", "-m", "local only")

        # (label, perturb, undo, expects a fetch)
        return (
            ("heads", push_peer_head, None, True),
            ("fence", raise_fence, lambda: self.fence.unlink(), False),
            ("config.json", touch(self.fc.root / "bridge" / "config.json"), None, True),
            ("rooms.json", touch(self.fc.root / "rooms.json"), None, True),
            ("rules.json", touch(self.fc.root / "rules.json"), None, True),
            ("archive", touch(self.fc.root / "archive"), None, True),
            ("channels", touch(self.fc.root / "channels"), None, True),
            ("channels/probe", touch(channel), None, True),
            ("channels/probe/messages", touch(channel / "messages"), None, True),
            # The undo removes the channel directory too: _directory_empty
            # scans bridge/chan-joins-pending itself, so an emptied
            # subdirectory keeps the node out of quiet forever.
            ("pending_empty", hold_a_join, drop_the_join, True),
            ("worktree_clean", dirty_worktree, None, True),
            ("head_matches_remote", move_head_off_remote, None, True),
        )

    def test_each_fingerprint_input_alone_forces_a_full_tick(self):
        for label, perturb, undo, expect_fetch in self.rows():
            with self.subTest(input=label):
                self.settle()
                seen = len(self.fc.logs())
                perturb()
                result = self.fc.sweep()
                self.assertIn(
                    result.returncode, (0, 1), result.stdout + result.stderr
                )
                actions = [record["action"] for record in self.fc.logs()[seen:]]
                self.assertNotIn(
                    "quiet",
                    actions,
                    f"changing {label} alone left the next tick quiet",
                )
                if expect_fetch:
                    self.assertIn("fetch", actions, label)
                if undo is not None:
                    undo()


class TickV2NoRegistryTest(unittest.TestCase):
    """A forge with no `registry` branch: config.peers is the only topology."""

    def setUp(self):
        self.temporary = CanonicalTemporaryDirectory(prefix="post-bridge-v2-noreg-")
        self.topology = TopologyV2(self.temporary.name)
        self.fc = self.topology.add("fc", ["garden"])
        self.trey = self.topology.add("trey", ["hq"])
        self.topology.finalize(include_peers=True)

    def tearDown(self):
        self.temporary.cleanup()

    def test_unpublished_sender_survives_a_missing_registry_branch(self):
        """SPEC-v2 §Unpublished senders keys on the peer's own publication.

        "for a peer that has **ever** published a valid rooms.json (a v2
        peer), a `from` that is neither in its published set nor pinned is
        quarantined unpublished_sender". The predicate is the peer's own
        history; nothing in it mentions the registry branch. Clearing
        v2_peers when `registry` is absent makes the quarantine remotely
        disableable — refresh_remote_refs drops the local registry ref
        whenever the remote answers "couldn't find remote ref", so deleting
        the branch downgrades every node carrying a peers restriction to v1
        free-form senders.
        """
        registry = run(
            ["git", "-C", self.topology.forge, "rev-parse", "--verify",
             "refs/heads/registry"],
            check=False,
        )
        self.assertNotEqual(registry.returncode, 0, "fixture must have no registry")

        for _ in range(2):
            for machine in (self.fc, self.trey):
                result = machine.sweep()
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(
            json.loads((self.fc.repo / "rooms.json").read_text())["rooms"],
            ["garden"],
            "fc must have published, so it is a v2 peer by publication history",
        )

        mail_id = fixed_id(931)
        self.fc.inject(
            f"outbox/trey/hq/{mail_id}.mail", craft_mail(mail_id, "impostor", "hq")
        )
        result = self.trey.sweep()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

        self.assertNotIn(mail_id, self.trey.inbox_ids("hq"))
        receipt = json.loads(
            (self.trey.repo / "receipts" / "fc" / "hq" / f"{mail_id}.json").read_text()
        )
        self.assertEqual(receipt["status"], "quarantined")
        self.assertEqual(receipt["reason"], UNPUBLISHED_SENDER)


if __name__ == "__main__":
    unittest.main()
