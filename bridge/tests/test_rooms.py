"""Contract tests for the v2 room registry and topology module."""

import json
import sys
import time
import unittest
from dataclasses import replace
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from bridgelib import common, rooms
from bridgelib.snapshot import (
    FORGED_SELF,
    LOCAL,
    NAME_COLLISION,
    UNHOMED,
    UNPUBLISHED_SENDER,
    VERIFIED,
    Contest,
    Owner,
    Snapshot,
    binding_verdict,
    empty_snapshot,
)
from .harness_v2 import TopologyV2
from .test_sweep import POST, SWEEPER, CanonicalTemporaryDirectory


class CaptureLogger:
    def __init__(self):
        self.records = []

    def emit(self, action, **fields):
        self.records.append({"action": action, **fields})

    def actions(self, action):
        return [record for record in self.records if record["action"] == action]


def deep_json(depth, prefix=b'{"x":', suffix=b"}"):
    """Object-rooted JSON nested `depth` arrays deep (2 bytes per level)."""
    return prefix + b"[" * depth + b"]" * depth + suffix


class JsonDepthTest(unittest.TestCase):
    """M3: peer JSON can nest deep enough to raise RecursionError in json.loads
    (about 2000 levels on Python 3.9, inside a 4 KiB header). The bound is
    enforced before parsing so the outcome does not depend on the interpreter."""

    def test_nesting_beyond_the_bound_is_a_config_error(self):
        with self.assertRaisesRegex(common.ConfigError, "nests deeper than 64"):
            common.load_json_bytes(deep_json(65), "fixture")
        # The root object plus 63 arrays is exactly 64 levels: accepted.
        self.assertIsInstance(common.load_json_bytes(deep_json(63), "fixture")["x"], list)

    def test_brackets_inside_strings_do_not_count(self):
        data = json.dumps({"s": "[" * 500 + '\\"{' * 100}).encode()
        self.assertEqual(len(common.load_json_bytes(data, "fixture")["s"]), 800)

    def test_recursion_error_from_the_parser_is_a_config_error(self):
        with (
            mock.patch.object(common.json, "loads", side_effect=RecursionError),
            self.assertRaisesRegex(common.ConfigError, "nests too deeply"),
        ):
            common.load_json_bytes(b'{"x": 1}', "fixture")


class PostReservedTest(unittest.TestCase):
    """M4: POST_RESERVED must stay a subset of what the pinned post refuses."""

    def test_real_post_refuses_every_reserved_name(self):
        import os
        import subprocess

        with CanonicalTemporaryDirectory(prefix="post-reserved-") as base:
            root = Path(base) / "mail"
            workspace = Path(base) / "ws"
            workspace.mkdir()
            environment = common.post_environment(root)
            for name in sorted(common.POST_RESERVED):
                with self.subTest(name=name):
                    result = subprocess.run(
                        [POST, "rooms", "add", "--", name, str(workspace)],
                        env=environment,
                        capture_output=True,
                        text=True,
                        timeout=30,
                        check=False,
                    )
                    self.assertNotEqual(result.returncode, 0, result.stdout)
                    self.assertIn("reserved", result.stdout + result.stderr)
            listed = subprocess.run(
                [POST, "rooms", "--json"],
                env=environment,
                capture_output=True,
                text=True,
                timeout=30,
                check=True,
            )
            self.assertEqual(json.loads(listed.stdout)["rooms"], [])
            self.assertTrue(os.path.isdir(workspace))


class RoomsTest(unittest.TestCase):
    def setUp(self):
        self.temporary = CanonicalTemporaryDirectory(prefix="post-bridge-rooms-")
        self.topology = TopologyV2(self.temporary.name)
        self.local = self.topology.add("self", ["local-room", "denied"])
        self.h1 = self.topology.add("h1", ["remote-one"])
        self.h2 = self.topology.add("h2", ["remote-two"])
        self.topology.finalize(include_peers=False)
        self.settings = SimpleNamespace(
            root=self.local.root,
            repo=self.local.repo,
            host="self",
            ssh_key=self.local.key,
            post_bin=POST,
            max_mail_bytes=1024 * 1024,
            deadline_seconds=30,
            fetch_grace_seconds=600,
        )
        self.logger = CaptureLogger()
        self.git = SWEEPER.Git(self.settings, SWEEPER.Deadline(60))

    def tearDown(self):
        self.temporary.cleanup()

    def test_bridgelib_has_one_module_identity(self):
        # assertIs(common, sys.modules["bridgelib.common"]) is true by
        # construction of the import above and cannot see the failure this
        # guard exists to prevent: a second module object registered under
        # a second key, e.g. post-bridge.bridgelib.common from a relative
        # import elsewhere in the package.
        self.assertEqual(
            [name for name in sys.modules if name.endswith("bridgelib.common")],
            ["bridgelib.common"],
        )

    def fetch(self):
        self.local.git(
            "fetch", "-q", "origin", "+refs/heads/*:refs/remotes/origin/*"
        )

    def post_rooms(self):
        value = json.loads(self.local.post("rooms", "--json").stdout)
        return {item["name"]: item["path"] for item in value["rooms"]}

    def push_registry(self, value=None, data=None, mode="100644"):
        oid = self.topology.push_registry(value=value, data=data, mode=mode)
        self.fetch()
        return oid

    def push_rooms(self, machine, value=None, data=None, mode="100644"):
        if data is None:
            data = (json.dumps(value, sort_keys=True) + "\n").encode("utf-8")
        oid = machine.push_rooms(data, mode=mode)
        self.fetch()
        return oid

    def persisted_rooms(self, host, names):
        path = self.local.root / "bridge" / "rooms" / "peers" / f"{host}.json"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(
            json.dumps({"v": 1, "host": host, "rooms": names}, sort_keys=True)
            + "\n",
            encoding="utf-8",
        )

    def config(self, peers):
        return SimpleNamespace(host="self", relay_url="fixture", peers=peers)

    def build(self, peers, post_rooms=None, denied_names=(), oids=None):
        return rooms.build_snapshot(
            self.settings,
            self.config(peers),
            self.git,
            {} if oids is None else oids,
            self.post_rooms() if post_rooms is None else post_rooms,
            self.logger,
            denied_names=denied_names,
        )

    def test_registry_contract_and_last_valid_fallback(self):
        valid = {"v": 1, "hosts": ["h2", "h1", "self"]}
        oid = self.push_registry(value=valid)
        self.assertEqual(
            rooms.read_registry(self.settings, self.git, oid, self.logger),
            valid["hosts"],
        )
        persisted = self.local.root / "bridge" / "registry" / "hosts.json"
        self.assertTrue(persisted.is_file())

        hostile = [
            (b'{"v":1,"hosts":[],"extra":true}\n', "100644"),
            (b'{"v":2,"hosts":[]}\n', "100644"),
            (
                (
                    json.dumps({"v": 1, "hosts": [f"h{i}" for i in range(65)]})
                    + "\n"
                ).encode(),
                "100644",
            ),
            (b'{"v":1,"hosts":["BAD"]}\n', "100644"),
            (b'{"v":1,"hosts":["h1","h1"]}\n', "100644"),
            (b'{"v":1,"hosts":[]}\n', "100755"),
            (b" " * 4097, "100644"),
        ]
        for data, mode in hostile:
            with self.subTest(data=data[:30], mode=mode):
                bad_oid = self.push_registry(data=data, mode=mode)
                before = len(self.logger.actions("registry_invalid"))
                self.assertEqual(
                    rooms.read_registry(self.settings, self.git, bad_oid, self.logger),
                    valid["hosts"],
                )
                self.assertEqual(
                    len(self.logger.actions("registry_invalid")), before + 1
                )
                rooms.read_registry(self.settings, self.git, bad_oid, self.logger)
                self.assertEqual(
                    len(self.logger.actions("registry_invalid")), before + 1
                )

        missing_oid = self.local.git("rev-parse", "HEAD").stdout.strip()
        self.assertEqual(
            rooms.read_registry(self.settings, self.git, missing_oid, self.logger),
            valid["hosts"],
        )
        other_root = Path(self.temporary.name) / "never"
        other_root.mkdir()
        never = SimpleNamespace(**vars(self.settings))
        never.root = other_root
        self.assertIsNone(rooms.read_registry(never, self.git, missing_oid, self.logger))

    def test_effective_peers_registry_restriction_and_warning(self):
        self.assertEqual(
            rooms.effective_peers(
                "self", None, {"self": [], "h2": [], "h1": []}, self.logger
            ),
            ["h1", "h2"],
        )
        self.assertEqual(
            rooms.effective_peers(
                "self",
                ["self", "h1", "h2"],
                {"h2": [], "ghost": []},
                self.logger,
            ),
            ["h2"],
        )
        self.assertEqual(
            self.logger.actions("peer_unregistered"),
            [{"action": "peer_unregistered", "host": "ghost"}],
        )
        self.assertEqual(
            rooms.effective_peers(
                "self", ["self", "h2", "h1"], {}, self.logger
            ),
            ["h1", "h2"],
        )

    def test_room_grammar_rejects_invisible_and_compatibility_names(self):
        refused = [
            " room",
            "room ",
            "room\u00a0",
            "h\u200bq",
            "\u200d",
            "\u3164",
            "hq\ufeff",
            "\uff21RCHIVE",
        ]
        for name in refused:
            with self.subTest(name=name), self.assertRaises(common.ConfigError):
                common.validate_room(name, topology=True)
        self.assertEqual(common.fold_name("e\u0301"), "é")
        with self.assertRaises(common.ConfigError):
            rooms._validate_rooms(
                json.dumps(
                    {"v": 1, "host": "h1", "rooms": ["é", "e\u0301"]}
                ).encode("utf-8"),
                "h1",
            )

    def test_channel_relay_name_verdicts_match_post_receipts(self):
        # Keep this table identical to post/tests/participants.rs::
        # channel_send_receipt_distinguishes_relay_state_and_reserved_names.
        cases = [
            ("☕️-chat", False),
            ("👩‍💻-devs", False),
            ("lobby ", False),
            ("ｂｒｉｄｇｅ", False),
            (".bridge.locK", False),
            (".ROOMS.JSON.é.TMP", True),
            ("bridge", False),
            ("archive", False),
            ("café", True),
            ("☕-chat", True),
            ("👩-devs", True),
            ("general", True),
            ("e\u0301", True),
        ]
        for name, relayable in cases:
            with self.subTest(name=name):
                try:
                    common.validate_room(name, topology=True)
                except common.ConfigError:
                    accepted = False
                else:
                    accepted = True
                self.assertEqual(accepted, relayable)

    def test_peer_rooms_contract_last_valid_and_v2_persistence(self):
        valid = {"v": 1, "host": "h1", "rooms": ["z", "a"]}
        persisted_path = self.local.root / "bridge" / "rooms" / "peers" / "h1.json"
        self.assertFalse(persisted_path.exists())
        oid = self.push_rooms(self.h1, value=valid)
        self.assertEqual(
            rooms.read_peer_rooms(self.settings, self.git, "h1", oid, self.logger),
            frozenset({"a", "z"}),
        )
        self.assertTrue(persisted_path.is_file())
        hostile = [
            (b" " * (64 * 1024 + 1), "100644"),
            (b'{"v":1,"host":"h1","rooms":[]}\n', "100755"),
            (b"not-json\n", "100644"),
            (b'{"v":1,"host":"h2","rooms":[]}\n', "100644"),
            (
                (
                    json.dumps(
                        {
                            "v": 1,
                            "host": "h1",
                            "rooms": [f"r{i}" for i in range(1025)],
                        }
                    )
                    + "\n"
                ).encode(),
                "100644",
            ),
            (
                (json.dumps({"v": 1, "host": "h1", "rooms": ["bad\nname"]}) + "\n").encode(),
                "100644",
            ),
            (
                (json.dumps({"v": 1, "host": "h1", "rooms": ["remote"]}) + "\n").encode(),
                "100644",
            ),
            (
                (json.dumps({"v": 1, "host": "h1", "rooms": ["Case", "case"]}) + "\n").encode(),
                "100644",
            ),
            # M3: 2000 levels in 4 KiB raised RecursionError on Python 3.9.
            (deep_json(2000, b'{"v":1,"host":"h1","rooms":', b"}\n"), "100644"),
        ]
        for data, mode in hostile:
            with self.subTest(data=data[:24], mode=mode):
                bad_oid = self.push_rooms(self.h1, data=data, mode=mode)
                before = len(self.logger.actions("rooms_invalid"))
                self.assertEqual(
                    rooms.read_peer_rooms(
                        self.settings, self.git, "h1", bad_oid, self.logger
                    ),
                    frozenset({"a", "z"}),
                )
                self.assertEqual(len(self.logger.actions("rooms_invalid")), before + 1)
                rooms.read_peer_rooms(
                    self.settings, self.git, "h1", bad_oid, self.logger
                )
                self.assertEqual(len(self.logger.actions("rooms_invalid")), before + 1)

        snapshot = self.build({"h1": []})
        self.assertIn("h1", snapshot.v2_peers)

    def test_rooms_invalid_is_once_for_every_seen_blob_oid(self):
        first = self.push_rooms(self.h1, data=b"not-json-one\n")
        second = self.push_rooms(self.h1, data=b"not-json-two\n")
        before = len(self.logger.actions("rooms_invalid"))
        rooms.read_peer_rooms(self.settings, self.git, "h1", first, self.logger)
        rooms.read_peer_rooms(self.settings, self.git, "h1", second, self.logger)
        rooms.read_peer_rooms(self.settings, self.git, "h1", first, self.logger)
        self.assertEqual(len(self.logger.actions("rooms_invalid")) - before, 2)

    def test_nonregular_rooms_publications_are_never_read(self):
        valid = {"v": 1, "host": "h1", "rooms": ["last-valid"]}
        valid_oid = self.push_rooms(self.h1, value=valid)
        self.assertEqual(
            rooms.read_peer_rooms(
                self.settings, self.git, "h1", valid_oid, self.logger
            ),
            frozenset({"last-valid"}),
        )
        data = json.dumps(
            {"v": 1, "host": "h1", "rooms": ["hostile"]}, sort_keys=True
        ).encode("utf-8")
        for mode in ("120000", "040000"):
            with self.subTest(mode=mode):
                oid = self.push_rooms(self.h1, data=data, mode=mode)
                self.assertEqual(
                    rooms.read_peer_rooms(
                        self.settings, self.git, "h1", oid, self.logger
                    ),
                    frozenset({"last-valid"}),
                )

    def test_build_snapshot_reads_publication_from_pinned_git_oid(self):
        self.persisted_rooms("h1", ["stale"])
        registry_oid = self.push_registry(
            value={"v": 1, "hosts": ["self", "h1"]}
        )
        peer_oid = self.push_rooms(
            self.h1,
            value={"v": 1, "host": "h1", "rooms": ["from-git"]},
        )
        snapshot = self.build(
            {}, oids={"registry": registry_oid, "machines/h1": peer_oid}
        )
        self.assertEqual(snapshot.published["h1"], frozenset({"from-git"}))
        self.assertEqual(snapshot.routes["from-git"], "h1")

    def test_peer_publication_namespace_allows_host_named_owners(self):
        owners_host = self.topology.add("owners", ["owners-room"])
        registry_oid = self.push_registry(
            value={"v": 1, "hosts": ["self", "owners"]}
        )
        peer_oid = self.push_rooms(
            owners_host,
            value={"v": 1, "host": "owners", "rooms": ["hq"]},
        )
        snapshot = self.build(
            {},
            oids={
                "registry": registry_oid,
                "machines/owners": peer_oid,
            },
        )
        self.assertEqual(snapshot.routes["hq"], "owners")
        self.assertIn("owners", snapshot.v2_peers)
        self.assertEqual(snapshot.owners["hq"].host, "owners")
        self.assertTrue(
            (self.local.root / "bridge/rooms/peers/owners.json").is_file()
        )

    def test_ownership_audit_uses_shadow_and_avoids_unchanged_writes(self):
        self.persisted_rooms("h1", ["x"])
        first = self.build({"h1": [], "h2": []})
        self.assertEqual(first.owners["x"].host, "h1")
        owners_path = self.local.root / "bridge" / "rooms" / "owners.json"
        shadow_path = self.local.root / "bridge" / "rooms" / "owners.last.json"
        self.assertTrue(shadow_path.is_file())

        edited = json.loads(owners_path.read_text())
        del edited["x"]
        owners_path.write_text(json.dumps(edited) + "\n", encoding="utf-8")
        self.build({"h1": [], "h2": []})
        self.assertEqual(len(self.logger.actions("owner_released")), 1)

        data = json.loads(owners_path.read_text())
        data["x"]["host"] = "h2"
        owners_path.write_text(json.dumps(data) + "\n", encoding="utf-8")
        changed = self.build({"h1": [], "h2": []})
        self.assertEqual(changed.owners["x"].host, "h2")
        self.assertEqual(len(self.logger.actions("owner_changed")), 1)

        mtimes = (owners_path.stat().st_mtime_ns, shadow_path.stat().st_mtime_ns)
        time.sleep(0.001)
        self.build({"h1": [], "h2": []})
        self.assertEqual(
            (owners_path.stat().st_mtime_ns, shadow_path.stat().st_mtime_ns),
            mtimes,
        )

    def test_legacy_invalid_owner_entry_is_dropped_not_fatal(self):
        legacy = "hq﻿"
        self.persisted_rooms("h1", ["keeper"])
        first = self.build({"h1": [], "h2": []})
        self.assertEqual(first.owners["keeper"].host, "h1")
        owners_path = self.local.root / "bridge" / "rooms" / "owners.json"
        shadow_path = self.local.root / "bridge" / "rooms" / "owners.last.json"
        record = {"host": "h2", "first_seen": "legacy"}
        for path in (owners_path, shadow_path):
            planted = json.loads(path.read_text())
            planted[legacy] = dict(record)
            path.write_text(json.dumps(planted) + "\n", encoding="utf-8")

        snapshot = self.build({"h1": [], "h2": []})
        self.assertEqual(snapshot.owners["keeper"].host, "h1")
        self.assertNotIn(legacy, snapshot.owners)
        self.assertNotIn(common.fold_name(legacy), snapshot.owners)
        invalid = self.logger.actions("owners_invalid")
        self.assertEqual(len(invalid), 1)
        self.assertEqual(invalid[0]["entry"], legacy)
        self.assertIn(
            "refused control or direction character", invalid[0]["reason"]
        )
        self.assertEqual(self.logger.actions("owner_released"), [])
        self.assertIn(legacy, json.loads(owners_path.read_text()))

        self.persisted_rooms("h2", ["fresh"])
        rewritten = self.build({"h1": [], "h2": []})
        self.assertEqual(rewritten.owners["fresh"].host, "h2")
        self.assertNotIn(legacy, json.loads(owners_path.read_text()))
        self.assertEqual(len(self.logger.actions("owners_invalid")), 2)

    def test_contest_ownership_routes_retired_and_placeholder_claim(self):
        self.persisted_rooms("h1", ["x"])
        first = self.build({"h1": [], "h2": []})
        self.assertEqual(first.routes["x"], "h1")
        self.persisted_rooms("h1", [])
        self.persisted_rooms("h2", ["x"])
        second = self.build({"h1": [], "h2": []})
        self.assertEqual(second.owners["x"].host, "h1")
        self.assertIn("x", second.contested)
        self.assertNotIn("x", second.routes)
        self.persisted_rooms("h2", [])
        self.persisted_rooms("h1", ["x"])
        returned = self.build({"h1": [], "h2": []})
        self.assertEqual(returned.routes["x"], "h1")

        placeholder_path = self.local.root / "remote" / "h2" / "remembered"
        placeholder_path.mkdir(parents=True)
        self.local.post("rooms", "add", "remembered", placeholder_path)
        self.persisted_rooms("h1", ["remembered"])
        remembered = self.build({"h1": [], "h2": []})
        self.assertIn("remembered", remembered.contested)
        self.assertIn(("h2", "remembered"), remembered.retired)
        health = rooms.rooms_health(remembered)
        collision = next(
            item for item in health["collisions"] if item["room"] == "remembered"
        )
        self.assertEqual(collision["claimants"], ["h1", "h2"])
        self.assertEqual(health["route_contested"], len(remembered.contested))

    def test_claimants_are_compared_by_ascii_fold(self):
        self.persisted_rooms("h1", ["Lumen"])
        local = self.build(
            {"h1": []}, post_rooms={"lumen": str(self.local.base / "rooms/lumen")}
        )
        self.assertIn("lumen", local.contested)
        self.assertNotIn("lumen", local.routes)
        before = self.post_rooms()
        self.assertEqual(
            rooms.ensure_placeholders(
                self.settings,
                self.config({}),
                local,
                self.logger,
                SWEEPER.Deadline(60),
                lambda: False,
            ),
            before,
        )
        self.assertEqual(self.logger.actions("room_registered"), [])

        self.persisted_rooms("h1", ["Case"])
        self.persisted_rooms("h2", ["case"])
        peers = self.build({"h1": [], "h2": []}, post_rooms={})
        self.assertIn("case", peers.contested)
        self.assertNotIn("case", peers.routes)
        collision = next(
            item for item in rooms.rooms_health(peers)["collisions"]
            if common.fold_name(item["room"]) == "case"
        )
        self.assertEqual(collision["room"], "Case")

    def test_eviction_waits_for_a_known_topology(self):
        registry_path = self.local.root / "bridge" / "registry" / "hosts.json"
        owners_path = self.local.root / "bridge" / "rooms" / "owners.json"
        self.persisted_rooms("h1", ["hq"])
        seeded = self.build({"h1": []})
        self.assertEqual(seeded.owners["hq"].host, "h1")

        hostile = self.push_registry(data=b'{"v":2,"hosts":[]}\n')
        unknown = self.build({}, oids={"registry": hostile})
        self.assertFalse(registry_path.exists())
        self.assertEqual(unknown.owners["hq"].host, "h1")
        self.assertEqual(self.logger.actions("owner_evicted"), [])

        valid = self.push_registry(value={"v": 1, "hosts": ["self", "h2"]})
        known = self.build({}, oids={"registry": valid})
        self.assertNotIn("hq", known.owners)
        self.assertEqual(
            self.logger.actions("owner_evicted")[-1],
            {"action": "owner_evicted", "host": "h1", "names": ["hq"]},
        )

        edited = json.loads(owners_path.read_text())
        edited["hq2"] = {"host": "h1", "first_seen": "old"}
        owners_path.write_text(json.dumps(edited) + "\n", encoding="utf-8")
        stale = self.push_registry(data=b'{"v":3,"hosts":[]}\n')
        fallback = self.build({}, oids={"registry": stale})
        self.assertTrue(registry_path.is_file())
        self.assertNotIn("hq2", fallback.owners)
        self.assertEqual(
            self.logger.actions("owner_evicted")[-1],
            {"action": "owner_evicted", "host": "h1", "names": ["hq2"]},
        )

    def test_local_and_peer_contest_owner_rule(self):
        self.persisted_rooms("h1", ["local-room"])
        local_wins = self.build({"h1": []})
        self.assertEqual(local_wins.contested["local-room"].owner, LOCAL)
        owners_path = self.local.root / "bridge" / "rooms" / "owners.json"
        data = json.loads(owners_path.read_text())
        data["local-room"] = {"host": "h1", "first_seen": "old"}
        owners_path.write_text(json.dumps(data) + "\n", encoding="utf-8")
        peer_wins = self.build({"h1": []})
        self.assertEqual(peer_wins.contested["local-room"].owner, "h1")

    def test_registry_eviction_releases_owner_but_peer_restriction_does_not(self):
        fc = self.topology.add("fc", ["fc-room"])
        bb = self.topology.add("bb", ["bb-room"])
        self.persisted_rooms("fc", ["hq"])
        squatted = self.build({"fc": []})
        self.assertEqual(squatted.owners["hq"].host, "fc")

        registry_oid = self.push_registry(
            value={"v": 1, "hosts": ["self", "bb"]}
        )
        bb_oid = self.push_rooms(
            bb, value={"v": 1, "host": "bb", "rooms": ["hq"]}
        )
        released = self.build(
            {}, oids={"registry": registry_oid, "machines/bb": bb_oid}
        )
        self.assertEqual(released.routes["hq"], "bb")
        self.assertEqual(released.owners["hq"].host, "bb")
        self.assertEqual(binding_verdict(released, "bb", "hq"), VERIFIED)
        self.assertEqual(
            self.logger.actions("owner_evicted")[-1]["names"], ["hq"]
        )

        registry_oid = self.push_registry(
            value={"v": 1, "hosts": ["self", "bb", "fc"]}
        )
        fc_oid = self.push_rooms(
            fc, value={"v": 1, "host": "fc", "rooms": ["held"]}
        )
        all_peers = self.build(
            {},
            oids={
                "registry": registry_oid,
                "machines/bb": bb_oid,
                "machines/fc": fc_oid,
            },
        )
        self.assertEqual(all_peers.owners["held"].host, "fc")
        restricted = self.build(
            {"bb": []},
            oids={"registry": registry_oid, "machines/bb": bb_oid},
        )
        self.assertEqual(restricted.owners["held"].host, "fc")

    def test_placeholder_lifecycle_pinned_and_derived_conflicts(self):
        base = empty_snapshot("self")
        rules_path = self.local.root / "rules.json"
        rules_path.unlink(missing_ok=True)
        routed = replace(
            base,
            peers=("h1",),
            pins={"h1": frozenset()},
            routes={"new-room": "h1"},
            retired=frozenset({("h2", "old-room")}),
        )
        result = rooms.ensure_placeholders(
            self.settings,
            self.config({}),
            routed,
            self.logger,
            SWEEPER.Deadline(60),
            lambda: False,
        )
        expected = self.local.root / "remote" / "h1" / "new-room"
        self.assertEqual(Path(result["new-room"]).resolve(), expected.resolve())
        self.assertTrue((self.local.root / "new-room" / "inbox").is_dir())
        self.assertTrue((self.local.root / "new-room" / "read").is_dir())
        self.assertEqual(json.loads(rules_path.read_text()), {"blocked": []})
        registered_before = len(self.logger.actions("room_registered"))
        rooms.ensure_placeholders(
            self.settings,
            self.config({}),
            routed,
            self.logger,
            SWEEPER.Deadline(60),
            lambda: False,
        )
        self.assertEqual(
            len(self.logger.actions("room_registered")), registered_before
        )
        self.assertEqual(len(self.logger.actions("room_retired")), 2)

        conflict_path = self.local.base / "rooms" / "conflict"
        conflict_path.mkdir()
        self.local.post("rooms", "add", "conflict", conflict_path)
        derived = replace(
            base, pins={"h1": frozenset()}, routes={"conflict": "h1"}
        )
        rooms.ensure_placeholders(
            self.settings,
            self.config({}),
            derived,
            self.logger,
            SWEEPER.Deadline(60),
            lambda: False,
        )
        self.assertEqual(len(self.logger.actions("placeholder_conflict")), 1)
        pinned = replace(
            base,
            pins={"h1": frozenset({"conflict"})},
            routes={"conflict": "h1"},
        )
        with self.assertRaises(common.ConfigError):
            rooms.ensure_placeholders(
                self.settings,
                self.config({}),
                pinned,
                self.logger,
                SWEEPER.Deadline(60),
                lambda: False,
            )
        self.assertTrue(
            (self.local.root / "bridge" / "collisions.json").is_file()
        )

    def test_placeholder_loop_honours_the_fence_between_candidates(self):
        """SPEC-v2 §Tick order step 9: the fence is checked per candidate.

        `routes` is bounded only by untrusted peer input (64 hosts x 1024
        names), so this is the one v2 loop whose length a peer sets. v1
        checked `fence_present` before every `post rooms add`; the v2 path
        checks it once before the whole call, so an operator who drops
        `.post-arx.json` mid-loop is ignored for the rest of it.
        """
        routed = replace(
            empty_snapshot("self"),
            peers=("h1",),
            pins={"h1": frozenset()},
            routes={"room-one": "h1", "room-two": "h1"},
        )
        fenced = {"value": False}
        original_run = rooms.subprocess.run

        def fence_after_first_add(command, **kwargs):
            result = original_run(command, **kwargs)
            if command[1:4] == ["rooms", "add", "--"]:
                fenced["value"] = True
            return result

        with mock.patch.object(
            rooms.subprocess, "run", side_effect=fence_after_first_add
        ):
            with self.assertRaises(common.TickError) as caught:
                rooms.ensure_placeholders(
                    self.settings,
                    self.config({}),
                    routed,
                    self.logger,
                    SWEEPER.Deadline(60),
                    lambda: fenced["value"],
                )

        self.assertEqual(caught.exception.reason, "fenced")
        self.assertTrue(fenced["value"], "no room was ever registered")
        registered = self.post_rooms()
        self.assertIn("room-one", registered)
        self.assertNotIn(
            "room-two", registered, "the bridge kept registering after the fence"
        )

    def test_placeholder_loop_stops_at_the_tick_deadline(self):
        """SPEC-v2 §Tick order: steps 8-16 check the deadline between candidates."""

        class CountingDeadline:
            def __init__(self, allowed):
                self.allowed = allowed
                self.calls = 0

            def check(self):
                self.calls += 1
                if self.calls > self.allowed:
                    raise common.DeadlineExpired()

        routed = replace(
            empty_snapshot("self"),
            peers=("h1",),
            pins={"h1": frozenset()},
            routes={f"room-{number}": "h1" for number in range(4)},
        )
        deadline = CountingDeadline(2)

        with self.assertRaises(common.DeadlineExpired):
            rooms.ensure_placeholders(
                self.settings,
                self.config({}),
                routed,
                self.logger,
                deadline,
                lambda: False,
            )

        self.assertEqual(deadline.calls, 3)
        registered = self.post_rooms()
        self.assertIn("room-0", registered)
        self.assertIn("room-1", registered)
        self.assertNotIn("room-2", registered)
        self.assertNotIn("room-3", registered)

    def test_build_snapshot_handles_derived_add_failure_and_pinned_collision(self):
        self.persisted_rooms("h1", ["racing"])
        derived = self.build({"h1": []})
        original_run = rooms.subprocess.run
        collided = False

        def register_before_add(command, **kwargs):
            nonlocal collided
            if command[1:4] == ["rooms", "add", "--"] and not collided:
                collided = True
                foreign = self.local.base / "rooms" / "racing-foreign"
                foreign.mkdir()
                original_run(
                    [POST, "rooms", "add", "--", "racing", str(foreign)],
                    env=kwargs["env"],
                    capture_output=True,
                    text=True,
                    timeout=30,
                    check=False,
                )
            return original_run(command, **kwargs)

        with mock.patch.object(rooms.subprocess, "run", side_effect=register_before_add):
            rooms.ensure_placeholders(
                self.settings,
                self.config({"h1": []}),
                derived,
                self.logger,
                SWEEPER.Deadline(60),
                lambda: False,
            )
        self.assertTrue(collided)
        self.assertEqual(len(self.logger.actions("placeholder_conflict")), 1)

        with self.assertRaises(common.ConfigError):
            self.build({"h1": ["local-room"]})
        collision = self.local.root / "bridge" / "collisions.json"
        self.assertTrue(collision.is_file())
        self.assertEqual(json.loads(collision.read_text())["room"], "local-room")

    def test_rooms_json_determinism_change_detection_and_filtering(self):
        placeholder = self.local.root / "remote" / "h1" / "remote-one"
        placeholder.mkdir(parents=True)
        self.local.post("rooms", "add", "remote-one", placeholder)
        snapshot = self.build({}, denied_names=("denied",))
        self.assertEqual(snapshot.publishable, ("local-room",))
        expected = b'{"host": "self", "rooms": ["local-room"], "v": 1}\n'
        self.assertEqual(rooms.rooms_json_bytes(snapshot), expected)
        second_build = self.build({}, denied_names=("denied",))
        self.assertEqual(
            rooms.rooms_json_bytes(snapshot), rooms.rooms_json_bytes(second_build)
        )
        self.assertTrue(rooms.write_rooms_json(self.settings, snapshot, self.logger))
        path = self.local.repo / "rooms.json"
        first_mtime = path.stat().st_mtime_ns
        time.sleep(0.001)
        self.assertFalse(rooms.write_rooms_json(self.settings, snapshot, self.logger))
        self.assertEqual(path.stat().st_mtime_ns, first_mtime)
        changed = replace(snapshot, publishable=("denied", "local-room"))
        self.assertTrue(rooms.write_rooms_json(self.settings, changed, self.logger))
        self.assertNotIn(
            "remote-one", json.loads(rooms.rooms_json_bytes(snapshot))["rooms"]
        )
        health = rooms.rooms_health(snapshot)
        self.assertEqual(health["published"], 1)

    def test_local_rooms_publication_is_capped_at_1024(self):
        names = tuple(reversed([f"room-{index:04d}" for index in range(1026)]))
        snapshot = replace(empty_snapshot("self"), publishable=names)
        publication = json.loads(rooms.rooms_json_bytes(snapshot))
        self.assertEqual(
            publication["rooms"], [f"room-{index:04d}" for index in range(1024)]
        )
        self.assertTrue(rooms.write_rooms_json(self.settings, snapshot, self.logger))
        self.assertEqual(
            self.logger.actions("rooms_publication_truncated"),
            [{"action": "rooms_publication_truncated", "count": 1026}],
        )

    def test_rooms_health_documents_route_contested_semantics(self):
        self.assertIn("len(contested)", rooms.rooms_health.__doc__)

    def test_binding_verdict_full_table(self):
        snapshot = Snapshot(
            self_host="self",
            peers=("h1", "h2"),
            oids={},
            published={
                "h1": frozenset({"owned", "collision"}),
                "h2": frozenset(),
            },
            v2_peers=frozenset({"h1", "h2"}),
            pins={"h1": frozenset(), "h2": frozenset()},
            owners={"collision": Owner("h1", "oid")},
            routes={"owned": "h1"},
            contested={"collision": Contest("h1", ("h1", "h2"))},
            retired=frozenset(),
            real_rooms={"local": "/local"},
            placeholders={"owned": "h1", "elsewhere": "h2"},
            publishable=("local",),
        )
        table = [
            (snapshot, "h1", "owned", VERIFIED),
            (replace(snapshot, v2_peers=frozenset()), "h1", "unknown", UNHOMED),
            (snapshot, "h1", "local", FORGED_SELF),
            (snapshot, "h1", "elsewhere", FORGED_SELF),
            (snapshot, "h2", "collision", NAME_COLLISION),
            (snapshot, "h1", "collision", VERIFIED),
            (
                replace(
                    snapshot,
                    owners={"hq": Owner("fc", "oid")},
                    contested={"hq": Contest("fc", (LOCAL, "fc"))},
                    real_rooms={"hq": "/local/hq"},
                ),
                "fc",
                "hq",
                NAME_COLLISION,
            ),
            (snapshot, "h2", "unknown", UNPUBLISHED_SENDER),
        ]
        for subject, host, sender, verdict in table:
            with self.subTest(host=host, sender=sender):
                self.assertEqual(binding_verdict(subject, host, sender), verdict)


if __name__ == "__main__":
    unittest.main()
