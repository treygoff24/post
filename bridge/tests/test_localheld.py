"""Local-held export guard (SPEC-v2 r6.1; bead post-aqw.9).

The three-root harness mirrors the live collision: ``atlasos`` is a real
room on trey and on mac, so the name is contested. trey's local letters to it
sit in trey's archive with a canonical mailbox copy and no bridge marker.
Removing trey's registration makes mac the sole claimant, which is the moment
an unguarded bridge exports those letters as duplicates.
"""

import hashlib
import json
import os
import subprocess
import sys
import time
import unittest
from pathlib import Path

from .test_sweep import (
    PINNED_POST_VERSION,
    POST,
    SWEEP,
    SWEEPER,
    CanonicalTemporaryDirectory,
    Topology,
    run,
)


def sha(data):
    return hashlib.sha256(data).hexdigest()


class LocalHeldTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        version = run([POST, "--version"], check=False)
        if version.returncode != 0 or not SWEEPER.post_version_accepted(version.stdout):
            raise RuntimeError(
                f"tests require {PINNED_POST_VERSION!r}; got {version.stdout.strip()!r}"
            )

    def build(self, contested=True):
        self.temporary = CanonicalTemporaryDirectory(prefix="post-bridge-held-")
        self.addCleanup(self.temporary.cleanup)
        self.topology = Topology(self.temporary.name)
        self.fc = self.topology.add("fc", ["garden"])
        self.trey = self.topology.add("trey", ["hq", "atlasos"])
        self.mac = self.topology.add(
            "mac", ["porch", "atlasos"] if contested else ["porch"]
        )
        self.topology.finalize()
        # The contest comes from published rooms.json, as it does live; a
        # config pin for a name that is also a registered room is config-fatal.
        for machine in self.topology.machines:
            config_path = machine.root / "bridge" / "config.json"
            config = json.loads(config_path.read_text())
            config["peers"] = {
                host: [room for room in names if room != "atlasos"]
                for host, names in config["peers"].items()
            }
            config_path.write_text(json.dumps(config, sort_keys=True) + "\n")
        for _ in range(2):
            for machine in self.topology.machines:
                self.sweep(machine, expect=None)

    # -- helpers -------------------------------------------------------------

    def sweep(self, machine, expect=0):
        """One full tick (the quiet fingerprint is dropped first)."""
        try:
            (machine.root / "bridge" / "trigger-fingerprint.json").unlink()
        except FileNotFoundError:
            pass
        result = machine.sweep()
        if expect is not None:
            self.assertEqual(
                result.returncode, expect, f"{machine.host}: {result.stdout}{result.stderr}"
            )
        return result

    def local_letter(self, body):
        """trey hq -> atlasos: post delivers it into trey's own atlasos room."""
        mail_id = self.trey.send("hq", "atlasos", body)
        archive = self.trey.root / "archive" / (mail_id + ".mail")
        copy = self.trey.root / "atlasos" / "inbox" / (mail_id + ".mail")
        self.assertEqual(copy.read_bytes(), archive.read_bytes())  # precondition
        return mail_id

    def record_path(self, mail_id):
        return self.trey.root / "bridge" / "local-held" / (mail_id + ".json")

    def record(self, mail_id):
        path = self.record_path(mail_id)
        return json.loads(path.read_bytes()) if path.exists() else None

    def health(self, machine):
        return json.loads((machine.root / "bridge" / "health.json").read_text())

    def exported(self, mail_id):
        """Whether trey staged or published this letter anywhere."""
        staged = list((self.trey.repo / "outbox").rglob(mail_id + ".mail"))
        published = (self.trey.root / "bridge" / "published" / mail_id).exists()
        pushed = run(
            ["git", "-C", self.topology.forge, "log", "--all", "--format=%H",
             "--", f"outbox/mac/atlasos/{mail_id}.mail"],
        ).stdout.strip()
        return bool(staged or published or pushed)

    def deregister_atlasos(self):
        """The rename decision on trey: atlasos leaves post's room table and
        its owner-of-record entry is released (owners.json is the bridge's
        human-edited ownership memory), so mac becomes the sole claimant."""
        table_path = self.trey.root / "rooms.json"
        table = json.loads(table_path.read_text())
        table.pop("atlasos")
        table_path.write_text(json.dumps(table, indent=2) + "\n")
        owners_path = self.trey.root / "bridge" / "rooms" / "owners.json"
        owners = json.loads(owners_path.read_text())
        self.assertEqual(owners.pop("atlasos")["host"], "local")  # precondition
        owners_path.write_text(json.dumps(owners, sort_keys=True) + "\n")

    def control_letter(self):
        """A fresh send to atlasos after deregistration: queued remote mail
        that must export in the same tick, proving the route is open."""
        rooms = {
            room["name"]: room["path"]
            for room in json.loads(self.trey.post("rooms", "--json").stdout)["rooms"]
        }
        self.assertIn("/remote/mac/atlasos", rooms.get("atlasos", ""))  # precondition
        return self.trey.send("hq", "atlasos", "control: queued remote")

    def actions(self, machine, name):
        return [record for record in machine.logs() if record["action"] == name]

    def seed(self, machine, manifest, *extra):
        return subprocess.run(
            [sys.executable, str(SWEEP), "--seed-local-holds", str(manifest), *extra],
            env=machine.env(),
            capture_output=True,
            text=True,
            timeout=60,
            check=False,
        )

    def init_sentinel(self, machine):
        """install.sh's one-shot: sweep.py --init-held-sentinel."""
        return subprocess.run(
            [sys.executable, str(SWEEP), "--init-held-sentinel"],
            env=machine.env(),
            capture_output=True,
            text=True,
            timeout=90,
            check=False,
        )

    def sentinel(self):
        path = self.trey.root / "bridge" / "held-guard-sentinel"
        return json.loads(path.read_text()) if path.exists() else None

    def as_2655dbe_store(self):
        """Reshape trey's store as 2655dbe left it: no sentinel, and health's
        local_held carrying only holds, faults, fault_reasons and
        candidates_unaccounted (no floors)."""
        bridge = self.trey.root / "bridge"
        (bridge / "held-guard-sentinel").unlink()
        path = bridge / "health.json"
        value = json.loads(path.read_text())
        holds = value["local_held"]["holds"]
        value["local_held"] = {"holds": holds, "faults": 0, "fault_reasons": {},
                               "candidates_unaccounted": 0}
        path.write_text(json.dumps(value) + "\n")

    def manifest_row(self, mail_id, **changes):
        archive = (self.trey.root / "archive" / (mail_id + ".mail")).read_bytes()
        row = {
            "host": "trey",
            "id": mail_id,
            "room": "atlasos",
            "archive_sha256": sha(archive),
            "local_copies": [f"atlasos/inbox/{mail_id}.mail"],
            "bridge_markers": [],
        }
        row.update(changes)
        return json.dumps(row, separators=(",", ":"))

    # -- record format -------------------------------------------------------

    def test_record_evidence_must_name_one_to_eight_paths(self):
        localheld = SWEEPER.localheld
        mail_id = "20260923-000000-abcdef"
        for count, valid in ((0, False), (1, True), (8, True), (9, False)):
            with self.subTest(count=count):
                data = localheld.record_bytes(
                    mail_id, "0" * 64, "atlasos", "observed",
                    [f"atlasos/inbox/{n}.mail" for n in range(count)],
                    "2026-09-23T00:00:00Z",
                )
                self.assertEqual(localheld.parse_record(data, mail_id) is not None, valid)

    def test_publish_record_creates_an_empty_index_without_appending(self):
        temporary = CanonicalTemporaryDirectory(prefix="post-bridge-held-unit-")
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        (root / "bridge").mkdir()
        mail_id = "20260923-000000-abcdef"
        SWEEPER.localheld.publish_record(
            root, mail_id, "0" * 64, "atlasos", "observed", ["atlasos/inbox/x.mail"]
        )
        self.assertTrue((root / "bridge" / "local-held" / (mail_id + ".json")).exists())
        # Present and still empty: the append is the caller's, so a crash
        # before it leaves a present index, which the next tick repairs. The
        # order (index before record) is bound by the order-failure test.
        self.assertEqual((root / "bridge" / "local-held-index.txt").read_bytes(), b"")

    @unittest.skipIf(os.geteuid() == 0, "root creates files in a 0500 directory")
    def test_a_record_is_never_published_when_the_index_cannot_be_created(self):
        # Review 7a (Grok): prove the order, not just the end state. bridge/
        # refuses new entries, so the index cannot be made,
        # while local-held/ and tmp/ would accept the record.
        temporary = CanonicalTemporaryDirectory(prefix="post-bridge-held-unit-")
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        bridge = root / "bridge"
        (bridge / "local-held").mkdir(parents=True)
        (bridge / "tmp").mkdir()
        bridge.chmod(0o500)
        self.addCleanup(bridge.chmod, 0o700)
        with self.assertRaises(OSError):
            SWEEPER.localheld.publish_record(
                root, "20260923-000000-abcdef", "0" * 64, "atlasos", "observed",
                ["atlasos/inbox/x.mail"],
            )
        self.assertEqual(sorted(os.listdir(bridge / "local-held")), [])
        self.assertEqual(sorted(os.listdir(bridge / "tmp")), [])
        self.assertFalse((bridge / "local-held-index.txt").exists())

    # -- stamping and the exclusion ------------------------------------------

    def test_hold_survives_deregistration_when_peer_becomes_sole_claimant(self):
        self.build()
        mail_id = self.local_letter("delivered locally")
        self.sweep(self.trey, expect=1)  # room_name_collision, stays visible
        self.assertEqual(self.health(self.trey)["reason"], "room_name_collision")
        record = self.record(mail_id)
        self.assertEqual(
            (record["reason"], record["to"], record["evidence"]),
            ("observed", "atlasos", [f"atlasos/inbox/{mail_id}.mail"]),
        )
        self.assertEqual(
            record["archive_sha256"],
            sha((self.trey.root / "archive" / (mail_id + ".mail")).read_bytes()),
        )
        self.deregister_atlasos()
        self.sweep(self.trey)  # registers the mac placeholder
        control = self.control_letter()
        self.sweep(self.trey)
        self.assertTrue(self.exported(control), "route to mac never opened")
        self.assertFalse(self.exported(mail_id), "held letter exported")
        self.assertEqual(self.health(self.trey)["local_held"]["holds"], 1)

    def test_hold_survives_mailbox_copy_and_room_directory_removal(self):
        self.build()
        copy_gone = self.local_letter("copy removed")
        room_gone = self.local_letter("room removed")
        self.sweep(self.trey, expect=1)
        self.assertIsNotNone(self.record(copy_gone))
        self.assertIsNotNone(self.record(room_gone))
        (self.trey.root / "atlasos" / "inbox" / (copy_gone + ".mail")).unlink()
        self.sweep(self.trey, expect=1)
        run(["rm", "-rf", "--", str(self.trey.root / "atlasos")])
        self.deregister_atlasos()
        self.sweep(self.trey)
        control = self.control_letter()
        self.sweep(self.trey)
        self.assertTrue(self.exported(control), "route to mac never opened")
        self.assertFalse(self.exported(copy_gone), "held letter exported (copy removed)")
        self.assertFalse(self.exported(room_gone), "held letter exported (room removed)")
        self.assertEqual(self.health(self.trey)["local_held"]["holds"], 2)

    def test_faulted_holds_are_visible_and_stay_held(self):
        self.build()
        corrupt = self.local_letter("corrupt record")
        altered = self.local_letter("altered archive")
        missing = self.local_letter("missing record")
        retargeted = self.local_letter("retargeted record")
        self.sweep(self.trey, expect=1)
        self.record_path(corrupt).write_text("{not json\n")
        value = self.record(retargeted)
        value["to"] = "elsewhere"
        self.record_path(retargeted).write_text(json.dumps(value) + "\n")
        archive = self.trey.root / "archive" / (altered + ".mail")
        archive.write_bytes(archive.read_bytes() + b"tampered\n")
        self.record_path(missing).unlink()
        self.deregister_atlasos()
        self.sweep(self.trey)
        control = self.control_letter()
        self.sweep(self.trey)
        self.assertTrue(self.exported(control), "route to mac never opened")
        for mail_id in (corrupt, altered, missing, retargeted):
            self.assertFalse(self.exported(mail_id), f"faulted hold exported: {mail_id}")
        faults = {
            record["id"]: record["fault"]
            for record in self.actions(self.trey, "local_held_fault")
        }
        self.assertEqual(
            faults,
            {
                corrupt: "record_invalid",
                altered: "digest_mismatch",
                missing: "missing",
                retargeted: "target_mismatch",
            },
        )
        # Logged once per id and kind, not every tick.
        self.assertEqual(len(self.actions(self.trey, "local_held_fault")), 4)
        local_held = self.health(self.trey)["local_held"]
        self.assertEqual(local_held["faults"], 4)
        self.assertEqual(
            local_held["fault_reasons"],
            {"digest_mismatch": 1, "missing": 1, "record_invalid": 1, "target_mismatch": 1},
        )
        self.assertEqual(self.record_path(corrupt).read_text(), "{not json\n")

    def test_duplicate_ticks_leave_the_record_byte_identical(self):
        self.build()
        mail_id = self.local_letter("stamped once")
        self.sweep(self.trey, expect=1)
        path = self.record_path(mail_id)
        first = (path.read_bytes(), path.stat().st_ino)
        for _ in range(2):
            self.sweep(self.trey, expect=1)
        self.assertEqual((path.read_bytes(), path.stat().st_ino), first)
        self.assertEqual(len(self.actions(self.trey, "local_held")), 1)
        local_held = self.health(self.trey)["local_held"]
        self.assertEqual((local_held["holds"], local_held["faults"]), (1, 0))

    def test_queued_remote_mail_still_exports(self):
        self.build()
        # post writes archive/ plus a canonical garden/inbox copy for a send
        # to the fc placeholder; that copy is not local delivery.
        mail_id = self.trey.send("hq", "garden", "queued for fc")
        canonical = self.trey.root / "garden" / "inbox" / (mail_id + ".mail")
        self.assertTrue(canonical.exists())  # precondition: post's real layout
        self.sweep(self.trey, expect=1)
        self.assertIsNone(self.record(mail_id))
        self.assertTrue((self.trey.root / "bridge" / "published" / mail_id).exists())
        self.sweep(self.fc, expect=1)  # fc sees the trey/mac contest too
        self.assertIn(mail_id, self.fc.inbox_ids("garden"))
        # A fresh install (no hold ever existed, no store) is not a wiped store.
        health = self.health(self.trey)
        self.assertEqual(health["local_held"]["fault_reasons"], {})
        self.assertEqual(health["reason"], "room_name_collision")
        self.assertEqual(self.actions(self.trey, "local_held_export_blocked"), [])

    # -- store-level faults ----------------------------------------------------

    def wipe_store(self):
        """rm -rf bridge/local-held* (the index may be a directory)."""
        bridge = self.trey.root / "bridge"
        for name in ("local-held", "local-held-manifests", "local-held-index.txt"):
            path = bridge / name
            if path.is_dir() and not path.is_symlink():
                for child in sorted(path.iterdir()):
                    child.unlink()
                path.rmdir()
            elif path.exists() or path.is_symlink():
                path.unlink()

    def assert_store_fault(self, kind):
        health = self.health(self.trey)
        self.assertEqual((health["ok"], health["reason"]), (False, "local_held_store_fault"))
        self.assertIn(kind, health["local_held"]["fault_reasons"])

    def test_wiped_store_is_a_sticky_fail_closed_fault(self):
        self.build()
        seeded = self.local_letter("seeded, then wiped")
        observed = self.local_letter("observed, then wiped")
        self.sweep(self.trey, expect=1)
        manifest = Path(self.temporary.name) / "manifest.jsonl"
        manifest.write_text(self.manifest_row(seeded) + "\n")
        self.assertEqual(self.seed(self.trey, manifest).returncode, 0)
        self.sweep(self.trey, expect=1)
        self.assertEqual(self.health(self.trey)["local_held"]["holds"], 2)  # precondition
        self.wipe_store()
        # Queued remote mail that a healthy guard exports on the next tick.
        control = self.trey.send("hq", "garden", "blocked while the store is gone")
        for _ in range(2):  # the second tick sees holds 0: the fault is sticky
            self.sweep(self.trey, expect=1)
            self.assert_store_fault("store_missing")
            for mail_id in (seeded, observed, control):
                self.assertFalse(self.exported(mail_id), mail_id)
            self.assertIsNone(self.record(observed))
            self.assertFalse((self.trey.root / "bridge" / "local-held-index.txt").exists())
        self.assertEqual(len(self.actions(self.trey, "local_held_export_blocked")), 2)
        self.assertEqual(  # the wipe took the manifest copies too (finding 4)
            [r.get("fault") for r in self.actions(self.trey, "local_held_fault")],
            ["store_missing", "manifest_missing"],
        )
        # Round 2, finding 2: the manifest restamps only the seeded hold; the
        # observed record is lost, so the plain seed refuses on the floor
        # and writes no index.
        refused = self.seed(self.trey, manifest)
        self.assertEqual(refused.returncode, 1, refused.stdout + refused.stderr)
        summary = json.loads(refused.stdout.splitlines()[-1])
        self.assertEqual(
            (summary["error"], summary["floor"], summary["surviving"], summary["index_rebuilt"]),
            ("lost_records", 2, 1, None),
        )
        self.assertFalse((self.trey.root / "bridge" / "local-held-index.txt").exists())
        self.sweep(self.trey, expect=1)
        self.assert_store_fault("index_missing")  # the manifest copy is back
        for mail_id in (seeded, observed, control):
            self.assertFalse(self.exported(mail_id), mail_id)
        # Recovery: accept the loss. atlasos is still local with the copy,
        # so the next tick observes the lost letter afresh.
        result = self.seed(self.trey, manifest, "--accept-lost-records")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        accepted = self.actions(self.trey, "local_held_lost_records_accepted")
        self.assertEqual(
            [(r["floor"], r["surviving"], r["indexed"]) for r in accepted], [(2, 1, 1)]
        )
        self.assertEqual(self.health(self.trey)["local_held"]["indexed"], 1)
        self.sweep(self.trey, expect=1)
        health = self.health(self.trey)
        self.assertEqual(health["reason"], "room_name_collision")
        self.assertEqual(health["local_held"]["fault_reasons"], {})
        self.assertEqual(health["local_held"]["holds"], 2)
        self.assertEqual(self.record(seeded)["reason"], "seeded")
        self.assertEqual(self.record(observed)["reason"], "observed")
        self.assertTrue(self.exported(control), "route to fc never reopened")
        for mail_id in (seeded, observed):
            self.assertFalse(self.exported(mail_id), mail_id)
        self.assertEqual(
            [r.get("fault") for r in self.actions(self.trey, "local_held_fault_cleared")],
            ["manifest_missing", "store_missing", "index_missing"],
        )
        self.assertEqual(self.health(self.trey)["local_held"]["indexed"], 2)
        self.deregister_atlasos()
        for _ in range(2):
            self.sweep(self.trey, expect=None)
            for mail_id in (seeded, observed):
                self.assertFalse(self.exported(mail_id), mail_id)

    def test_mailbox_copy_with_other_bytes_is_not_evidence(self):
        self.build()
        mail_id = self.local_letter("copy rewritten")
        copy = self.trey.root / "atlasos" / "inbox" / (mail_id + ".mail")
        copy.write_bytes(copy.read_bytes().replace(b"copy rewritten", b"copy REWRITTEN"))
        self.sweep(self.trey, expect=1)
        self.assertIsNone(self.record(mail_id))
        self.assertEqual(self.health(self.trey)["local_held"]["candidates_unaccounted"], 1)

    def test_new_local_letter_is_held_before_any_contest(self):
        self.build(contested=False)
        mail_id = self.local_letter("no contest yet")
        self.sweep(self.trey)
        self.assertEqual(self.health(self.trey)["rooms"]["collisions"], [])
        self.assertEqual(self.record(mail_id)["reason"], "observed")
        local_held = self.health(self.trey)["local_held"]
        self.assertEqual(
            (local_held["holds"], local_held["faults"], local_held["candidates_unaccounted"]),
            (1, 0, 0),
        )

    def test_stamp_never_overwrites_a_record(self):
        # Direct: a racing second stamp (a seed beside a tick) must lose.
        localheld = SWEEPER.localheld
        temporary = CanonicalTemporaryDirectory(prefix="post-bridge-held-unit-")
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        (root / "bridge" / "tmp").mkdir(parents=True)
        mail_id = "20260923-000000-deadbeef"
        localheld.stamp(root, mail_id, "a" * 64, "atlasos", "observed", ["atlasos/inbox/x.mail"])
        path = localheld.record_path(root, mail_id)
        first = (path.read_bytes(), path.stat().st_ino)
        with self.assertRaises(FileExistsError):
            localheld.stamp(root, mail_id, "b" * 64, "elsewhere", "seeded", [])
        self.assertEqual((path.read_bytes(), path.stat().st_ino), first)

    def test_partial_wipe_of_index_and_one_record_is_a_sticky_fault(self):
        # GLM 5.3 minor 2: a manifest copy survives, so the store is not
        # wholly gone, but the deleted observed record is in no manifest.
        self.build()
        lost = self.local_letter("observed, record deleted with the index")
        kept = self.local_letter("observed, record survives")
        self.sweep(self.trey, expect=1)
        manifest = Path(self.temporary.name) / "manifest.jsonl"
        manifest.write_text(self.manifest_row(kept) + "\n")
        self.assertEqual(self.seed(self.trey, manifest).returncode, 0)
        backup = self.record_path(lost).read_bytes()
        (self.trey.root / "bridge" / "local-held-index.txt").unlink()
        self.record_path(lost).unlink()
        self.deregister_atlasos()
        self.sweep(self.trey, expect=1)  # places remote/mac/atlasos; already faulted
        self.assert_store_fault("index_missing")
        self.assertFalse(self.exported(lost))
        control = self.control_letter()
        for _ in range(2):
            self.sweep(self.trey, expect=1)
            self.assert_store_fault("index_missing")
            for mail_id in (lost, kept, control):
                self.assertFalse(self.exported(mail_id), mail_id)
            self.assertIsNone(self.record(lost))
        # Recovery: restore the lost record from a backup, then re-seed; the
        # seed appends every surviving record's line and counts the kept
        # row as already held although atlasos is no longer local here.
        self.record_path(lost).write_bytes(backup)
        result = self.seed(self.trey, manifest)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        summary = json.loads(result.stdout.splitlines()[-1])
        self.assertEqual((summary["already_held"], summary["index_rebuilt"]), (1, 2))
        self.sweep(self.trey, expect=None)
        self.assertEqual(self.health(self.trey)["local_held"]["fault_reasons"], {})
        self.assertTrue(self.exported(control), "route to mac never reopened")
        for mail_id in (lost, kept):
            self.assertFalse(self.exported(mail_id), mail_id)

    def partial_wipe_without_backup(self):
        """GLM 5.3's scenario: the index and one observed record are deleted,
        a manifest copy survives, atlasos is renamed away, and no backup of
        the record exists. Returns (lost, kept, manifest, control)."""
        self.build()
        lost = self.local_letter("observed; record deleted, no backup")
        kept = self.local_letter("observed; record survives")
        self.sweep(self.trey, expect=1)
        manifest = Path(self.temporary.name) / "manifest.jsonl"
        manifest.write_text(self.manifest_row(kept) + "\n")
        self.assertEqual(self.seed(self.trey, manifest).returncode, 0)
        self.sweep(self.trey, expect=1)
        self.assertEqual(self.health(self.trey)["local_held"]["indexed"], 2)  # precondition
        (self.trey.root / "bridge" / "local-held-index.txt").unlink()
        self.record_path(lost).unlink()
        self.deregister_atlasos()
        self.sweep(self.trey, expect=1)
        self.assert_store_fault("index_missing")
        return lost, kept, manifest, self.control_letter()

    def test_unknown_intent_stamps_do_not_meet_the_floor(self):
        # Review 3, finding 1 (Fable): the seed measured surviving after
        # its own stamps, so one unknown-intent letter offset a lost
        # observed record and a plain re-seed cleared the fault. Fable's
        # letter is a queued remote send to a still-contested name; here
        # atlasos is still local on trey, so the letter's local copy is
        # removed instead. Both are what the unknown-intent pass stamps: a
        # letter to a contested name with no copy, marker or record.
        self.build()
        lost = self.local_letter("observed; record deleted, no backup")
        kept = self.local_letter("observed; record survives")
        self.sweep(self.trey, expect=1)
        manifest = Path(self.temporary.name) / "manifest.jsonl"
        manifest.write_text(self.manifest_row(kept) + "\n")
        self.assertEqual(self.seed(self.trey, manifest).returncode, 0)
        self.sweep(self.trey, expect=1)
        self.assertEqual(self.sentinel()["indexed"], 2)  # precondition
        (self.trey.root / "bridge" / "local-held-index.txt").unlink()
        self.record_path(lost).unlink()
        self.sweep(self.trey, expect=1)
        self.assert_store_fault("index_missing")
        queued = self.local_letter("no copy, no marker, no record")
        (self.trey.root / "atlasos" / "inbox" / (queued + ".mail")).unlink()
        self.assertIn(  # precondition: the name is contested
            "atlasos", [c["room"] for c in self.health(self.trey)["rooms"]["collisions"]]
        )
        result = self.seed(self.trey, manifest)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        summary = json.loads(result.stdout.splitlines()[-1])
        self.assertEqual(
            (summary["error"], summary["floor"], summary["surviving"],
             summary["unknown_intent"], summary["index_rebuilt"]),
            ("lost_records", 2, 1, None, None),
        )
        self.assertFalse((self.trey.root / "bridge" / "local-held-index.txt").exists())
        self.assertIsNone(self.record(queued))
        self.assert_nothing_exports(lost, kept, queued)
        self.assert_store_fault("index_missing")

    def test_a_restamped_seeded_record_meets_the_floor(self):
        # Review 3, finding 1: the ids the manifest rows stamp do count. A
        # lost seeded record re-stamped from the preserved manifest, with
        # nothing else lost, clears the fault.
        self.build()
        seeded = self.local_letter("seeded; record deleted, re-stamped")
        observed = self.local_letter("observed; record survives")
        self.sweep(self.trey, expect=1)
        manifest = Path(self.temporary.name) / "manifest.jsonl"
        manifest.write_text(self.manifest_row(seeded) + "\n")
        self.assertEqual(self.seed(self.trey, manifest).returncode, 0)
        self.sweep(self.trey, expect=1)
        (self.trey.root / "bridge" / "local-held-index.txt").unlink()
        self.record_path(seeded).unlink()
        self.sweep(self.trey, expect=1)
        self.assert_store_fault("index_missing")
        result = self.seed(self.trey, manifest)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        summary = json.loads(result.stdout.splitlines()[-1])
        self.assertEqual((summary["floor"], summary["surviving"], summary["stamped"]), (2, 2, 1))
        self.sweep(self.trey, expect=1)
        self.assertEqual(self.health(self.trey)["local_held"]["fault_reasons"], {})
        self.deregister_atlasos()
        self.sweep(self.trey, expect=0)
        control = self.control_letter()
        self.sweep(self.trey, expect=None)
        self.assertTrue(self.exported(control), "route to mac never reopened")
        for mail_id in (seeded, observed):
            self.assertFalse(self.exported(mail_id), mail_id)

    def test_an_empty_index_put_back_does_not_clear_the_fault(self):
        lost, kept, _, control = self.partial_wipe_without_backup()
        index = self.trey.root / "bridge" / "local-held-index.txt"
        index.write_bytes(b"")  # touch
        for _ in range(2):
            self.sweep(self.trey, expect=1)
            self.assert_store_fault("index_short")
            self.assertEqual(self.health(self.trey)["local_held"]["indexed"], 2)
            for mail_id in (lost, kept, control):
                self.assertFalse(self.exported(mail_id), mail_id)
        # The tick repaired the surviving record's line (review 3, finding
        # 4); the floor still knows the other hold is gone.
        self.assertEqual(index.read_bytes().split(), [kept.encode()])
        short = [r for r in self.actions(self.trey, "local_held_fault")
                 if r.get("fault") == "index_short"]
        self.assertEqual(
            [(r["floor"], r["indexed"], r["unindexed_records"]) for r in short], [(2, 1, 0)]
        )
        # A stale index naming every surviving record: coverage passes, and
        # only the floor still knows a hold is gone.
        index.write_text(kept + "\n")
        for _ in range(2):
            self.sweep(self.trey, expect=1)
            self.assert_store_fault("index_short")
            for mail_id in (lost, kept, control):
                self.assertFalse(self.exported(mail_id), mail_id)

    def test_index_short_is_logged_again_as_records_come_back(self):
        # Review 3, finding 6 (Fable): the numbers are logged per change, not
        # once per kind.
        self.build()
        first = self.local_letter("restored first")
        second = self.local_letter("restored second")
        kept = self.local_letter("never lost")
        self.sweep(self.trey, expect=1)
        backups = {m: self.record_path(m).read_bytes() for m in (first, second)}
        index = self.trey.root / "bridge" / "local-held-index.txt"
        index.unlink()
        for mail_id in backups:
            self.record_path(mail_id).unlink()
        self.deregister_atlasos()
        self.sweep(self.trey, expect=1)
        self.assert_store_fault("index_missing")
        control = self.control_letter()
        index.write_bytes(b"")
        for mail_id in (None, first):
            if mail_id is not None:
                self.record_path(mail_id).write_bytes(backups[mail_id])
            self.sweep(self.trey, expect=1)
            self.assert_store_fault("index_short")
            for sent in (first, second, kept, control):
                self.assertFalse(self.exported(sent), sent)
        short = [(r["floor"], r["indexed"], r["unindexed_records"])
                 for r in self.actions(self.trey, "local_held_fault")
                 if r.get("fault") == "index_short"]
        self.assertEqual(short, [(3, 1, 0), (3, 2, 0)])
        self.record_path(second).write_bytes(backups[second])
        self.sweep(self.trey, expect=None)
        self.assertEqual(self.health(self.trey)["local_held"]["fault_reasons"], {})
        self.assertTrue(self.exported(control), "route to mac never reopened")
        for sent in (first, second, kept):
            self.assertFalse(self.exported(sent), sent)

    def test_a_refused_row_does_not_block_the_rebuild(self):
        # Review 3, finding 2 (Fable): a manifest with a persistently
        # refused row could never rebuild a missing index. Refused rows now
        # fail the seed (exit 1) but the rebuild runs once the floor is met.
        self.build()
        good = self.local_letter("seeded")
        refused = self.local_letter("refused row, held as a missing fault")
        manifest = Path(self.temporary.name) / "manifest.jsonl"
        manifest.write_text(
            self.manifest_row(good) + "\n"
            + self.manifest_row(refused, local_copies=[f"atlasos/read/{refused}.mail"]) + "\n"
        )
        self.assertEqual(self.seed(self.trey, manifest).returncode, 1)
        self.sweep(self.trey, expect=1)
        self.assertEqual(  # precondition
            self.health(self.trey)["local_held"]["fault_reasons"], {"missing": 1}
        )
        (self.trey.root / "bridge" / "local-held-index.txt").unlink()
        self.sweep(self.trey, expect=1)
        self.assert_store_fault("index_missing")
        result = self.seed(self.trey, manifest)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        summary = json.loads(result.stdout.splitlines()[-1])
        self.assertEqual(
            (summary["mismatched"], summary["floor"], summary["surviving"],
             summary["index_rebuilt"], summary.get("error")),
            (1, 1, 1, 1, None),
        )
        self.assertEqual(
            [r["reason"] for r in self.actions(self.trey, "local_held_seed_mismatch")][-1],
            "local_copy_mismatch",
        )
        self.sweep(self.trey, expect=1)
        self.assertEqual(
            self.health(self.trey)["local_held"]["fault_reasons"], {"missing": 1}
        )
        self.deregister_atlasos()
        self.sweep(self.trey, expect=None)
        control = self.control_letter()
        self.sweep(self.trey, expect=None)
        self.assertTrue(self.exported(control), "route to mac never reopened")
        for mail_id in (good, refused):
            self.assertFalse(self.exported(mail_id), mail_id)

    def test_every_row_refused_with_a_lost_record_writes_no_index(self):
        # Review 3, finding 2: refused rows no longer block the rebuild, so
        # what keeps this index absent is the floor (the held record is
        # gone and no row restamps it).
        self.build()
        held = self.local_letter("held, then wiped, then badly re-seeded")
        self.sweep(self.trey, expect=1)
        self.wipe_store()
        self.deregister_atlasos()
        self.sweep(self.trey, expect=1)
        self.assert_store_fault("store_missing")
        control = self.control_letter()
        manifest = Path(self.temporary.name) / "bad.jsonl"
        manifest.write_text(self.manifest_row(held, archive_sha256="0" * 64) + "\n")
        result = self.seed(self.trey, manifest)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        summary = json.loads(result.stdout.splitlines()[-1])
        self.assertEqual(
            (summary["mismatched"], summary["error"], summary["floor"], summary["surviving"],
             summary["index_rebuilt"]),
            (1, "lost_records", 1, 0, None),
        )
        self.assertFalse((self.trey.root / "bridge" / "local-held-index.txt").exists())
        self.assertIsNone(self.record(held))
        for _ in range(2):
            self.sweep(self.trey, expect=1)
            self.assert_store_fault("index_missing")  # the manifest copy persisted
            for mail_id in (held, control):
                self.assertFalse(self.exported(mail_id), mail_id)

    def test_unreadable_index_is_never_appended_or_exported_past(self):
        self.build()
        mail_id = self.local_letter("held, index then damaged")
        self.sweep(self.trey, expect=1)
        index = self.trey.root / "bridge" / "local-held-index.txt"
        good = index.read_bytes()
        self.assertIn(mail_id.encode(), good)  # precondition
        target = Path(self.temporary.name) / "index-target.txt"

        def oversize():
            index.write_bytes(good)
            os.truncate(index, 64 * 1024 * 1024 + 1)  # sparse; Grok's nit

        def symlink():
            target.write_bytes(good)
            index.symlink_to(target)

        def directory():
            index.mkdir()

        for label, damage in (("oversize", oversize), ("symlink", symlink),
                              ("directory", directory)):
            with self.subTest(label):
                if index.is_dir() and not index.is_symlink():
                    index.rmdir()
                elif index.exists() or index.is_symlink():
                    index.unlink()
                damage()
                control = self.trey.send("hq", "garden", f"blocked: {label} index")
                before = (index.lstat().st_size, target.read_bytes() if target.exists() else None)
                for _ in range(2):
                    self.sweep(self.trey, expect=1)
                    self.assert_store_fault("index_unreadable")
                    self.assertFalse(self.exported(control), label)
                    self.assertFalse(self.exported(mail_id), label)
                after = (index.lstat().st_size, target.read_bytes() if target.exists() else None)
                self.assertEqual(after, before, f"{label}: index appended")
                self.assertEqual(self.actions(self.trey, "internal_error"), [])

    def test_a_busy_streak_does_not_mask_a_store_fault(self):
        # Review 7b (Grok): the fifth busy probe used to rename the reason to
        # busy_streak while the store fault still stood.
        import fcntl

        self.build()
        self.local_letter("held, then wiped")
        self.sweep(self.trey, expect=1)
        self.wipe_store()
        self.sweep(self.trey, expect=1)
        self.assert_store_fault("store_missing")
        lock_path = self.trey.root / "bridge" / ".lock"
        with lock_path.open("r+") as lock:
            fcntl.flock(lock.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
            results = [self.trey.sweep() for _ in range(5)]
        self.assertEqual([r.returncode for r in results], [1, 1, 1, 1, 1])
        health = self.health(self.trey)
        self.assertEqual(health["busy_streak"], 5)  # precondition: the streak tripped
        self.assert_store_fault("store_missing")

    def test_wipe_after_another_store_fault_is_still_store_missing(self):
        # Review finding 1 (Fable, Grok): a faulted tick validates nothing, so
        # its holds is 0; the floor must survive it.
        self.build()
        held = self.local_letter("held, then index damaged, then wiped")
        self.sweep(self.trey, expect=1)
        index = self.trey.root / "bridge" / "local-held-index.txt"
        self.assertIn(held.encode(), index.read_bytes())  # precondition
        index.unlink()
        index.mkdir()
        self.sweep(self.trey, expect=1)
        self.assert_store_fault("index_unreadable")
        # Floor readings are asserted last, so a red-proof against a floorless
        # build fails on the export path, not on a missing field.
        local_held = self.health(self.trey)["local_held"]
        floors = [(local_held["holds"], local_held.get("indexed"))]
        # The operator "resets": index, records and manifests all removed.
        self.wipe_store()
        self.deregister_atlasos()
        for _ in range(2):
            self.sweep(self.trey, expect=1)
            self.assert_store_fault("store_missing")
            self.assertFalse(self.exported(held))
            floors.append((0, self.health(self.trey)["local_held"].get("indexed")))
        # An ea363e3 host's health after its index_unreadable tick has no
        # floor and holds 0: the carried store fault alone is sticky.
        path = self.trey.root / "bridge" / "health.json"
        value = json.loads(path.read_text())
        value["local_held"] = {"holds": 0, "faults": 1, "candidates_unaccounted": 0,
                               "fault_reasons": {"index_unreadable": 1}}
        path.write_text(json.dumps(value) + "\n")
        self.sweep(self.trey, expect=1)
        self.assert_store_fault("store_missing")
        self.assertFalse(self.exported(held))
        self.assertEqual(floors, [(0, 1)] * 3)

    def test_floor_alone_marks_a_wipe(self):
        # A tick whose selection returned before validating anything (for
        # example no archive/) writes holds 0 and no fault; the floor is the
        # only evidence left.
        self.build()
        held = self.local_letter("held; then an early-return tick; then wiped")
        self.sweep(self.trey, expect=1)
        path = self.trey.root / "bridge" / "health.json"
        value = json.loads(path.read_text())
        value["local_held"].update(holds=0, faults=0, fault_reasons={})
        index = self.trey.root / "bridge" / "local-held-index.txt"
        self.assertIn(held.encode(), index.read_bytes())  # precondition
        path.write_text(json.dumps(value) + "\n")
        self.wipe_store()
        self.deregister_atlasos()
        for _ in range(2):
            self.sweep(self.trey, expect=1)
            self.assert_store_fault("store_missing")
            self.assertFalse(self.exported(held))

    def test_sentinel_marks_a_wipe_that_took_health_too(self):
        # Round 2, finding 5: with health.json deleted as well, the floor and
        # the carried fault are gone; the sentinel outside local-held* stays.
        self.build()
        sentinel = self.trey.root / "bridge" / "held-guard-sentinel"
        self.assertFalse(sentinel.exists())  # a fresh install has none
        self.assertEqual(self.health(self.trey)["local_held"]["fault_reasons"], {})
        held = self.local_letter("held; then store and health wiped")
        self.sweep(self.trey, expect=1)
        # Precondition: written at the end of the tick that made the index.
        self.assertEqual(self.sentinel(), {"version": 1, "indexed": 1, "manifests": 0})
        self.wipe_store()
        (self.trey.root / "bridge" / "health.json").unlink()
        self.deregister_atlasos()
        self.sweep(self.trey, expect=1)
        self.assert_store_fault("store_missing")
        control = self.control_letter()
        for _ in range(2):
            self.sweep(self.trey, expect=1)
            self.assert_store_fault("store_missing")
            for mail_id in (held, control):
                self.assertFalse(self.exported(mail_id), mail_id)
        self.assertTrue(sentinel.exists())

    def wipe_after_the_sentinel(self):
        """Grok's phase 2: holds exist and the sentinel carries their floor;
        then the store and health.json are wiped and one tick runs. Returns
        (lost, seeded, manifest): lost is observed-only."""
        self.build()
        lost = self.local_letter("observed only; wiped with health")
        seeded = self.local_letter("seeded; wiped with health")
        self.sweep(self.trey, expect=1)
        manifest = Path(self.temporary.name) / "manifest.jsonl"
        manifest.write_text(self.manifest_row(seeded) + "\n")
        self.assertEqual(self.seed(self.trey, manifest).returncode, 0)
        self.sweep(self.trey, expect=1)
        self.assertEqual(self.sentinel(), {"version": 1, "indexed": 2, "manifests": 1})
        self.wipe_store()
        (self.trey.root / "bridge" / "health.json").unlink()
        self.sweep(self.trey, expect=1)
        self.assert_store_fault("store_missing")
        return lost, seeded, manifest

    def assert_nothing_exports(self, *mail_ids):
        self.deregister_atlasos()
        self.sweep(self.trey, expect=1)
        control = self.control_letter()
        for _ in range(2):
            self.sweep(self.trey, expect=1)
            for mail_id in mail_ids + (control,):
                self.assertFalse(self.exported(mail_id), mail_id)

    def test_touched_index_after_a_health_wipe_meets_the_sentinel_floor(self):
        # Review 3, finding 7 (Grok, phase 2a): the floor died with
        # health.json, so a touched index cleared the carried fault.
        lost, seeded, _ = self.wipe_after_the_sentinel()
        (self.trey.root / "bridge" / "local-held-index.txt").touch()
        self.sweep(self.trey, expect=1)
        self.assert_store_fault("index_short")
        self.assertEqual(self.health(self.trey)["local_held"]["indexed"], 2)
        self.assert_nothing_exports(lost, seeded)
        self.assert_store_fault("index_short")

    def test_reseed_after_a_health_wipe_meets_the_sentinel_floor(self):
        # Review 3, finding 7 (Grok, phase 2b): a plain re-seed rebuilt the
        # index against a floor of 0 and cleared the fault.
        lost, seeded, manifest = self.wipe_after_the_sentinel()
        result = self.seed(self.trey, manifest)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        summary = json.loads(result.stdout.splitlines()[-1])
        self.assertEqual(
            (summary["error"], summary["floor"], summary["surviving"], summary["index_rebuilt"]),
            ("lost_records", 2, 1, None),
        )
        # The seeded record and its manifest copy are back, the index is not.
        self.assert_nothing_exports(lost, seeded)
        self.assert_store_fault("index_missing")

    def test_oversize_health_does_not_lower_the_sentinel_floor(self):
        """Regression guard (passes on ccbffa3): fails if the floors stop reading the sentinel when health.json reads as absent."""
        # Review 3, finding 7: health.json over its 64 KiB read cap reads as
        # absent; the sentinel still carries the floor.
        self.build()
        held = self.local_letter("held; then store wiped and health oversized")
        self.sweep(self.trey, expect=1)
        sentinel = self.trey.root / "bridge" / "held-guard-sentinel"
        self.assertTrue(sentinel.is_file())  # precondition, in any format
        self.wipe_store()
        path = self.trey.root / "bridge" / "health.json"
        value = json.loads(path.read_text())
        value["padding"] = "x" * (65 * 1024)
        path.write_text(json.dumps(value) + "\n")
        self.sweep(self.trey, expect=1)
        self.assert_store_fault("store_missing")
        self.assert_nothing_exports(held)
        self.assert_store_fault("store_missing")
        # Review 4, finding 6: the sentinel's shape is checked last, so the
        # test reaches its real assertions on a bridge whose sentinel is not
        # this JSON.
        self.assertEqual(self.sentinel()["indexed"], 1)

    def test_accept_over_a_directory_sentinel_deletes_nothing(self):
        """Regression guard (passes on ccbffa3): fails if --accept-lost-records removes a non-regular sentinel path."""
        # Review 4, finding 4: the documented sentinel_unwritable recovery.
        # The accept cannot replace a directory and must not delete it;
        # the operator removes it, one tick writes a null floor, and a
        # second accept clears the fault.
        self.build()
        held = self.local_letter("held under a directory sentinel")
        self.sweep(self.trey, expect=1)
        sentinel = self.trey.root / "bridge" / "held-guard-sentinel"
        sentinel.unlink()
        sentinel.mkdir()
        (sentinel / "operator-note").write_text("not ours to delete\n")
        self.sweep(self.trey, expect=1)
        self.assert_store_fault("sentinel_damaged")
        manifest = Path(self.temporary.name) / "manifest.jsonl"
        manifest.write_text(self.manifest_row(held) + "\n")
        result = self.seed(self.trey, manifest, "--accept-lost-records")
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        summary = json.loads(result.stdout.splitlines()[-1])
        self.assertTrue(summary["error"].startswith("sentinel_unwritable: "), summary)
        self.assertEqual((sentinel / "operator-note").read_text(), "not ours to delete\n")
        self.sweep(self.trey, expect=1)
        self.assert_store_fault("sentinel_damaged")
        (sentinel / "operator-note").unlink()
        sentinel.rmdir()
        self.sweep(self.trey, expect=1)
        self.assert_store_fault("floor_unknown")
        self.assertEqual(self.sentinel(), {"version": 1, "indexed": None, "manifests": 1})
        result = self.seed(self.trey, manifest, "--accept-lost-records")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.sentinel(), {"version": 1, "indexed": 1, "manifests": 1})
        self.sweep(self.trey, expect=1)
        self.assertEqual(self.health(self.trey)["local_held"]["fault_reasons"], {})
        self.assertFalse(self.exported(held))

    def test_a_tick_failing_after_selection_has_raised_the_sentinel(self):
        # Review 4, finding 3 (Fable): the sentinel was settled only in
        # write_health, so a tick that stamped and then failed (here a fence
        # appearing before publish) left its new hold out of the floor.
        import contextlib
        import io
        from unittest import mock

        self.build()
        self.local_letter("held by the first tick")
        self.sweep(self.trey, expect=1)
        self.assertEqual(self.sentinel()["indexed"], 1)  # precondition
        second = self.local_letter("stamped by a tick that then fails")
        fence = self.trey.root / ".post-arx.json"
        self.addCleanup(lambda: fence.unlink() if fence.exists() else None)

        def fence_before_publish(name):
            if name == "before-publish":
                fence.write_text("{}\n")

        (self.trey.root / "bridge" / "trigger-fingerprint.json").unlink()
        with mock.patch.dict(os.environ, self.trey.env(), clear=True), mock.patch.object(
            SWEEPER, "checkpoint", side_effect=fence_before_publish
        ), contextlib.redirect_stdout(io.StringIO()):
            self.assertNotEqual(SWEEPER.execute(), 0)
        self.assertTrue(fence.exists(), "the fence never fired")
        self.assertEqual(self.health(self.trey)["reason"], "fenced")
        self.assertIsNotNone(self.record(second))
        self.assertEqual(self.sentinel()["indexed"], 2)

    def test_a_damaged_sentinel_is_a_store_fault_until_accepted(self):
        # Review 3, finding 7: a sentinel that cannot be read as the floors
        # is sticky: without it a wipe of health would read as fresh.
        self.build()
        held = self.local_letter("held under a damaged sentinel")
        self.sweep(self.trey, expect=1)
        sentinel = self.trey.root / "bridge" / "held-guard-sentinel"
        good = sentinel.read_bytes()
        cases = (
            ("unparseable", lambda: sentinel.write_bytes(b"The local-held guard\n")),
            ("unparseable", lambda: sentinel.write_text('{"version":1,"indexed":-1,"manifests":0}')),
            # Review 4, finding 4: true == 1 in Python; the type is checked.
            ("unparseable", lambda: sentinel.write_text('{"version":true,"indexed":1,"manifests":0}')),
            ("oversize", lambda: sentinel.write_bytes(b" " * 5000)),
            ("not_regular", lambda: (sentinel.unlink(), sentinel.mkdir())),
        )
        for problem, damage in cases:
            with self.subTest(problem=problem):
                damage()
                self.sweep(self.trey, expect=1)
                self.assert_store_fault("sentinel_damaged")
                faults = [r for r in self.actions(self.trey, "local_held_fault")
                          if r.get("fault") == "sentinel_damaged"]
                self.assertEqual(faults[-1]["problem"], problem)
                if sentinel.is_dir():
                    sentinel.rmdir()
                sentinel.write_bytes(good)
        sentinel.write_bytes(b"garbage")
        (self.trey.root / "bridge" / "health.json").unlink()
        self.sweep(self.trey, expect=1)
        self.assert_store_fault("sentinel_damaged")
        self.assertFalse(self.exported(held))
        manifest = Path(self.temporary.name) / "manifest.jsonl"
        manifest.write_text(self.manifest_row(held) + "\n")
        result = self.seed(self.trey, manifest)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertEqual(json.loads(result.stdout.splitlines()[-1])["error"], "sentinel_damaged")
        self.sweep(self.trey, expect=1)
        self.assert_store_fault("sentinel_damaged")
        result = self.seed(self.trey, manifest, "--accept-lost-records")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.sentinel(), {"version": 1, "indexed": 1, "manifests": 1})
        accepted = self.actions(self.trey, "local_held_lost_records_accepted")
        self.assertEqual(accepted[-1]["sentinel"], "unparseable")
        self.sweep(self.trey, expect=1)
        self.assertEqual(self.health(self.trey)["local_held"]["fault_reasons"], {})
        self.deregister_atlasos()
        self.sweep(self.trey, expect=0)
        control = self.control_letter()
        self.sweep(self.trey, expect=None)
        self.assertTrue(self.exported(control), "route to mac never reopened")
        self.assertFalse(self.exported(held))

    def test_fresh_install_first_stamp_is_no_fault(self):
        # False-fault guard: the tick that stamps first writes the sentinel,
        # and the next tick finds the floor met.
        self.build()
        self.assertIsNone(self.sentinel())
        self.sweep(self.trey, expect=1)
        self.assertIsNone(self.sentinel())  # nothing held: none written
        self.local_letter("first hold on a fresh install")
        self.sweep(self.trey, expect=1)
        self.assertEqual(self.sentinel(), {"version": 1, "indexed": 1, "manifests": 0})
        self.sweep(self.trey, expect=1)
        self.assertEqual(self.health(self.trey)["local_held"]["fault_reasons"], {})
        self.assertEqual(self.actions(self.trey, "local_held_fault"), [])

    def test_fresh_install_crash_after_the_empty_index_is_no_fault(self):
        # False-fault guard: a crash between creating the index and
        # publishing the first record leaves an empty index, no record and
        # no sentinel.
        self.build()
        self.sweep(self.trey, expect=1)
        (self.trey.root / "bridge" / "local-held-index.txt").touch()
        self.sweep(self.trey, expect=1)
        self.assertEqual(self.health(self.trey)["local_held"]["fault_reasons"], {})
        self.assertEqual(self.sentinel(), {"version": 1, "indexed": 0, "manifests": 0})
        held = self.local_letter("the first record after the crash")
        self.sweep(self.trey, expect=1)
        self.assertEqual(self.health(self.trey)["local_held"]["fault_reasons"], {})
        self.assertEqual(self.sentinel()["indexed"], 1)
        self.assertEqual(self.actions(self.trey, "local_held_fault"), [])
        self.assertIsNotNone(self.record(held))

    def test_install_writes_the_sentinel_before_any_tick(self):
        # Review 3, finding 7: the deploy window. Installed over a 2655dbe
        # store, no full tick yet, then store and health are wiped.
        self.build()
        first = self.local_letter("held under 2655dbe, seeded")
        second = self.local_letter("held under 2655dbe, observed")
        manifest = Path(self.temporary.name) / "manifest.jsonl"
        manifest.write_text(self.manifest_row(first) + "\n")
        self.assertEqual(self.seed(self.trey, manifest).returncode, 0)
        self.sweep(self.trey, expect=1)
        self.as_2655dbe_store()
        result = self.init_sentinel(self.trey)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.sentinel(), {"version": 1, "indexed": 2, "manifests": 1})
        # Idempotent: a present sentinel is left alone.
        self.assertEqual(self.init_sentinel(self.trey).returncode, 0)
        self.assertEqual(self.sentinel(), {"version": 1, "indexed": 2, "manifests": 1})
        self.wipe_store()
        (self.trey.root / "bridge" / "health.json").unlink()
        self.sweep(self.trey, expect=1)
        self.assert_store_fault("store_missing")
        self.assert_nothing_exports(first, second)
        self.assert_store_fault("store_missing")

    def test_init_sentinel_waits_for_the_tick_lock_and_skips_a_fresh_install(self):
        import fcntl

        self.build()
        result = self.init_sentinel(self.trey)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIsNone(self.sentinel())  # a fresh install gets none
        (self.trey.root / "bridge" / "local-held-index.txt").write_text(
            "\n20260923-000000-abcdef\n"
        )
        lock = os.open(str(self.trey.root / "bridge" / ".lock"), os.O_RDWR)
        self.addCleanup(os.close, lock)
        fcntl.flock(lock, fcntl.LOCK_EX)
        process = subprocess.Popen(
            [sys.executable, str(SWEEP), "--init-held-sentinel"],
            env=self.trey.env(), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )
        time.sleep(2.5)
        self.assertIsNone(process.poll())  # still waiting on the lock
        self.assertIsNone(self.sentinel())
        fcntl.flock(lock, fcntl.LOCK_UN)
        out, err = process.communicate(timeout=30)
        self.assertEqual(process.returncode, 0, out + err)
        self.assertEqual(self.sentinel(), {"version": 1, "indexed": 1, "manifests": 0})

    def test_unknown_floor_is_a_store_fault_that_only_accept_clears(self):
        # Review 3, finding 7: a sentinel absent over held evidence with no
        # countable index (here a 2655dbe store whose index was deleted)
        # records the floor as null. A tick cannot clear it; the seed
        # rebuilds only with --accept-lost-records.
        self.build()
        held = self.local_letter("held under 2655dbe")
        self.sweep(self.trey, expect=1)
        self.as_2655dbe_store()
        (self.trey.root / "bridge" / "local-held-index.txt").unlink()
        self.sweep(self.trey, expect=1)
        self.assert_store_fault("floor_unknown")
        self.assert_store_fault("index_missing")
        self.assertEqual(self.sentinel(), {"version": 1, "indexed": None, "manifests": 0})
        # Health gone too: the null sentinel keeps the floor unknown.
        (self.trey.root / "bridge" / "health.json").unlink()
        self.sweep(self.trey, expect=1)
        self.assert_store_fault("floor_unknown")
        manifest = Path(self.temporary.name) / "manifest.jsonl"
        manifest.write_text(self.manifest_row(held) + "\n")
        result = self.seed(self.trey, manifest)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        summary = json.loads(result.stdout.splitlines()[-1])
        self.assertEqual(
            (summary["error"], summary["floor"], summary["index_rebuilt"]),
            ("floor_unknown", None, None),
        )
        self.assertFalse((self.trey.root / "bridge" / "local-held-index.txt").exists())
        self.sweep(self.trey, expect=1)
        self.assert_store_fault("floor_unknown")
        self.assertFalse(self.exported(held))
        result = self.seed(self.trey, manifest, "--accept-lost-records")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.sentinel(), {"version": 1, "indexed": 1, "manifests": 1})
        self.sweep(self.trey, expect=1)
        self.assertEqual(self.health(self.trey)["local_held"]["fault_reasons"], {})
        self.deregister_atlasos()
        self.sweep(self.trey, expect=0)
        control = self.control_letter()
        self.sweep(self.trey, expect=None)
        self.assertTrue(self.exported(control), "route to mac never reopened")
        self.assertFalse(self.exported(held))

    def test_index_made_a_directory_before_the_first_new_tick_then_wiped(self):
        # Review 3, finding 3 (folded into 7): over a 2655dbe store the
        # index is a directory at the first new-code tick; then everything
        # and health.json are wiped.
        self.build()
        held = self.local_letter("held under 2655dbe")
        self.sweep(self.trey, expect=1)
        self.as_2655dbe_store()
        index = self.trey.root / "bridge" / "local-held-index.txt"
        index.unlink()
        index.mkdir()
        self.sweep(self.trey, expect=1)
        self.assert_store_fault("index_unreadable")
        written = self.sentinel()
        self.wipe_store()
        (self.trey.root / "bridge" / "health.json").unlink()
        self.sweep(self.trey, expect=1)
        self.assert_store_fault("store_missing")
        self.assert_nothing_exports(held)
        self.assert_store_fault("floor_unknown")
        # The first new tick recorded the uncountable floor as null.
        self.assertEqual(written, {"version": 1, "indexed": None, "manifests": 0})

    def test_first_tick_over_a_2655dbe_store_changes_nothing(self):
        # Review 3, finding 8: the deploy, at the live shape. 879 held ids
        # (834 observed, 45 seeded), one manifest copy, records naming one
        # or two evidence paths, health's local_held carrying only holds,
        # faults, fault_reasons and candidates_unaccounted, and no sentinel.
        # Letters and records are written directly: stamping 879 through
        # post is slow, and the guard reads only the files.
        localheld = SWEEPER.localheld
        self.build()
        template_id = self.local_letter("the template letter")
        self.sweep(self.trey, expect=1)
        root = self.trey.root
        bridge = root / "bridge"
        template = (root / "archive" / (template_id + ".mail")).read_bytes()
        self.assertEqual(template.count(template_id.encode()), 1)  # precondition
        ids = [template_id] + [
            f"20260901-{n // 60 * 100 + n % 60:06d}-{n:06x}" for n in range(1, 879)
        ]
        seeded = ids[-45:]
        rows = []
        for n, mail_id in enumerate(ids):
            data = template.replace(template_id.encode(), mail_id.encode()) + (
                f"\n{n}\n".encode() if n else b""
            )
            (root / "archive" / (mail_id + ".mail")).write_bytes(data)
            if mail_id in seeded:
                rows.append(json.dumps({
                    "host": "trey", "id": mail_id, "room": "atlasos",
                    "archive_sha256": sha(data),
                    "local_copies": [f"atlasos/inbox/{mail_id}.mail"], "bridge_markers": [],
                }, separators=(",", ":")))
        manifest = ("\n".join(rows) + "\n").encode()
        copies = bridge / "local-held-manifests"
        copies.mkdir()
        (copies / (sha(manifest) + ".jsonl")).write_bytes(manifest)
        manifest_path = localheld.manifest_relpath(sha(manifest))
        for n, mail_id in enumerate(ids):
            data = (root / "archive" / (mail_id + ".mail")).read_bytes()
            mailbox = "read" if n % 3 == 0 else "inbox"
            evidence = [f"atlasos/{mailbox}/{mail_id}.mail"]
            (root / "atlasos" / mailbox).mkdir(parents=True, exist_ok=True)
            (root / "atlasos" / mailbox / (mail_id + ".mail")).write_bytes(data)
            if mail_id in seeded:
                reason, evidence = "seeded", evidence + [manifest_path]
            else:
                reason = "observed"
                if n % 5 == 0:
                    other = "inbox" if mailbox == "read" else "read"
                    (root / "atlasos" / other / (mail_id + ".mail")).write_bytes(data)
                    evidence.append(f"atlasos/{other}/{mail_id}.mail")
            self.record_path(mail_id).write_bytes(localheld.record_bytes(
                mail_id, sha(data), "atlasos", reason, evidence, "2026-09-01T00:00:00Z"
            ))
        index = "".join("\n" + mail_id + "\n" for mail_id in ids).encode()
        (bridge / "local-held-index.txt").write_bytes(index)
        self.as_2655dbe_store()
        health_path = bridge / "health.json"
        value = json.loads(health_path.read_text())
        value["local_held"]["holds"] = 879
        health_path.write_text(json.dumps(value) + "\n")
        records = {mail_id: self.record_path(mail_id).read_bytes() for mail_id in ids}
        faults_before = len(self.actions(self.trey, "local_held_fault"))
        stamps_before = len(self.actions(self.trey, "local_held"))
        self.sweep(self.trey, expect=1)
        health = self.health(self.trey)
        self.assertEqual(health["reason"], "room_name_collision")
        self.assertEqual(
            health["local_held"],
            {"holds": 879, "indexed": 879, "manifests": 1, "faults": 0, "fault_reasons": {},
             "candidates_unaccounted": 0},
        )
        self.assertEqual(self.actions(self.trey, "local_held_fault")[faults_before:], [])
        self.assertEqual(self.actions(self.trey, "local_held")[stamps_before:], [])
        self.assertEqual((bridge / "local-held-index.txt").read_bytes(), index)
        self.assertEqual(self.sentinel(), {"version": 1, "indexed": 879, "manifests": 1})
        self.assertEqual(
            self.actions(self.trey, "local_held_sentinel_created")[-1]["indexed"], 879
        )
        for mail_id in ids:
            self.assertEqual(self.record_path(mail_id).read_bytes(), records[mail_id])
        # And the second tick is the same, from the sentinel.
        self.sweep(self.trey, expect=1)
        self.assertEqual(self.health(self.trey)["local_held"]["fault_reasons"], {})
        self.assertEqual(self.health(self.trey)["local_held"]["holds"], 879)

    def short_2655dbe_store(self):
        """A 2655dbe store (health holds 2, no sentinel) whose index already
        lost one line and whose record for that id is gone. Returns the
        lost id."""
        self.build()
        lost = self.local_letter("held under 2655dbe; line and record lost")
        kept = self.local_letter("held under 2655dbe; intact")
        self.sweep(self.trey, expect=1)
        self.as_2655dbe_store()
        self.assertEqual(self.health(self.trey)["local_held"]["holds"], 2)  # precondition
        (self.trey.root / "bridge" / "local-held-index.txt").write_text("\n" + kept + "\n")
        self.record_path(lost).unlink()
        return lost

    def test_transition_over_an_index_shorter_than_the_holds_is_floor_unknown(self):
        # Review 4, finding 2 (Grok): the live-transition exception trusted
        # the index's id count, so an index already short at the first tick
        # set the floor to 1 and the lost letter exported.
        lost = self.short_2655dbe_store()
        self.sweep(self.trey, expect=1)
        self.assert_store_fault("floor_unknown")
        self.assertEqual(self.sentinel(), {"version": 1, "indexed": None, "manifests": 0})
        self.assert_nothing_exports(lost)
        self.assert_store_fault("floor_unknown")

    def test_init_over_an_index_shorter_than_the_holds_writes_a_null_floor(self):
        # Review 4, finding 2: install's --init-held-sentinel applies the
        # same rule as the tick.
        lost = self.short_2655dbe_store()
        result = self.init_sentinel(self.trey)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.sentinel(), {"version": 1, "indexed": None, "manifests": 0})
        self.sweep(self.trey, expect=1)
        self.assert_store_fault("floor_unknown")
        self.assert_nothing_exports(lost)

    @unittest.skipIf(os.geteuid() == 0, "root lists a 0000 directory")
    def test_unlistable_records_keep_the_fault_in_tick_and_seed(self):
        # Review 3, finding 5 (Fable): record_ids raised PermissionError out
        # of the tick (internal_error) and the seed (a traceback).
        self.build()
        held = self.local_letter("held; records then unlistable")
        self.sweep(self.trey, expect=1)
        index = self.trey.root / "bridge" / "local-held-index.txt"
        good = index.read_bytes()
        index.unlink()
        index.mkdir()
        self.sweep(self.trey, expect=1)  # carries index_unreadable
        index.rmdir()
        index.write_bytes(good)
        records = self.trey.root / "bridge" / "local-held"
        records.chmod(0)
        self.addCleanup(records.chmod, 0o700)
        self.deregister_atlasos()
        self.sweep(self.trey, expect=1)
        control = self.control_letter()
        for _ in range(2):
            self.sweep(self.trey, expect=1)
            self.assert_store_fault("index_short")
            for mail_id in (held, control):
                self.assertFalse(self.exported(mail_id), mail_id)
        self.assertEqual(self.actions(self.trey, "internal_error"), [])
        manifest = Path(self.temporary.name) / "manifest.jsonl"
        manifest.write_text(self.manifest_row(held) + "\n")
        result = self.seed(self.trey, manifest)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertEqual(result.stderr, "")
        summary = json.loads(result.stdout.splitlines()[-1])
        self.assertEqual((summary["action"], summary["error"]),
                         ("local_held_seed", "records_unreadable"))
        self.assertFalse((self.trey.root / "bridge" / "local-held-manifests").exists())
        records.chmod(0o700)
        self.sweep(self.trey, expect=None)
        self.assertEqual(self.health(self.trey)["local_held"]["fault_reasons"], {})
        self.assertTrue(self.exported(control), "route to mac never reopened")
        self.assertFalse(self.exported(held))

    @unittest.skipIf(os.geteuid() == 0, "root writes through a 0400 file")
    def test_unwritable_index_is_a_store_fault_not_a_crash(self):
        self.build()
        index = self.trey.root / "bridge" / "local-held-index.txt"
        self.assertFalse(index.exists())  # precondition: nothing held yet
        index.write_bytes(b"")
        index.chmod(0o400)
        self.addCleanup(lambda: index.chmod(0o600) if index.exists() else None)
        # The control sorts first, so it is selected before the stamp's
        # append fails: the fault must still drop it from this tick.
        control = self.trey.send("hq", "garden", "blocked: index unwritable")
        time.sleep(1.1)
        mail_id = self.local_letter("stamped, index unwritable")
        self.assertLess(control, mail_id)  # precondition: selection order
        # The first tick fails the append; the second (fault carried) probes
        # the index before clearing and finds the record's line missing.
        for reasons in ({"index_unwritable": 1},
                        {"index_unwritable": 1, "index_short": 1}):
            self.sweep(self.trey, expect=1)
            self.assert_store_fault("index_unwritable")
            self.assertEqual(self.health(self.trey)["local_held"]["fault_reasons"], reasons)
            self.assertEqual(self.record(mail_id)["reason"], "observed")
            self.assertFalse(self.exported(mail_id))
            self.assertFalse(self.exported(control))
            self.assertEqual(index.read_bytes(), b"")
        self.assertEqual(self.actions(self.trey, "internal_error"), [])
        index.chmod(0o600)
        # Writable again: the next tick appends the record's line itself and
        # clears, with no seed (review 3, finding 4).
        self.sweep(self.trey, expect=1)
        self.assertEqual(
            [r["appended"] for r in self.actions(self.trey, "local_held_index_repaired")], [1]
        )
        self.assertEqual(index.read_bytes().split(), [mail_id.encode()])
        health = self.health(self.trey)
        self.assertEqual((health["reason"], health["local_held"]["fault_reasons"]),
                         ("room_name_collision", {}))
        self.assertTrue(self.exported(control))
        self.assertFalse(self.exported(mail_id))

    @unittest.skipIf(os.geteuid() == 0, "root writes through a 0400 file")
    def test_repair_after_an_unwritable_blip_still_counts_a_lost_record(self):
        # The stamp whose append failed is counted in the floor, so deleting
        # its record during the fault leaves the repaired index short.
        self.build()
        index = self.trey.root / "bridge" / "local-held-index.txt"
        index.write_bytes(b"")
        index.chmod(0o400)
        self.addCleanup(lambda: index.chmod(0o600) if index.exists() else None)
        mail_id = self.local_letter("stamped during the blip, then deleted")
        self.sweep(self.trey, expect=1)
        self.assert_store_fault("index_unwritable")
        index.chmod(0o600)
        self.record_path(mail_id).unlink()
        self.deregister_atlasos()
        self.sweep(self.trey, expect=1)
        control = self.control_letter()
        for _ in range(2):
            self.sweep(self.trey, expect=1)
            self.assert_store_fault("index_short")
            for sent in (mail_id, control):
                self.assertFalse(self.exported(sent), sent)
        self.assertEqual(index.read_bytes(), b"")

    def plant_empty_record(self):
        """An empty file under local-held/ named like a record."""
        path = self.trey.root / "bridge" / "local-held" / "20260923-000000-abcdef.json"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(b"")
        return path

    def test_an_empty_planted_record_does_not_meet_the_floor(self):
        # Review 4, finding 1 (Grok): the repair appended every filename, so
        # an empty file named like an id stood in for the lost record.
        lost, kept, _, control = self.partial_wipe_without_backup()
        index = self.trey.root / "bridge" / "local-held-index.txt"
        index.touch()
        planted = self.plant_empty_record()
        for _ in range(2):
            self.sweep(self.trey, expect=1)
            self.assert_store_fault("index_short")
            for mail_id in (lost, control):
                self.assertFalse(self.exported(mail_id), mail_id)
        self.assertEqual(index.read_text().split(), [kept])
        self.assertTrue(planted.exists())
        # Review 5, finding 2: the fault line names the junk file by id.
        short = [r for r in self.actions(self.trey, "local_held_fault")
                 if r.get("fault") == "index_short"]
        self.assertEqual(short[-1]["unindexed_ids"], ["20260923-000000-abcdef"])

    def test_an_empty_planted_record_does_not_count_toward_a_reseed(self):
        # Review 4, finding 1: the seed's surviving count had the same hole.
        lost, _, manifest, control = self.partial_wipe_without_backup()
        self.plant_empty_record()
        result = self.seed(self.trey, manifest)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        summary = json.loads(result.stdout.splitlines()[-1])
        self.assertEqual(
            (summary["error"], summary["floor"], summary["surviving"],
             summary["already_held"], summary["index_rebuilt"]),
            ("lost_records", 2, 1, 1, None),
        )
        # Round 2, finding 2: a re-seed must not clear the fault by rebuilding
        # an index that no longer names the lost hold.
        self.assertFalse((self.trey.root / "bridge" / "local-held-index.txt").exists())
        self.assertIsNone(self.record(lost))
        for _ in range(2):
            self.sweep(self.trey, expect=1)
            self.assert_store_fault("index_missing")
            for mail_id in (lost, control):
                self.assertFalse(self.exported(mail_id), mail_id)

    def test_a_truncated_record_does_not_count_toward_the_floor(self):
        # Review 4, finding 1: a record file with a real id whose bytes do
        # not parse is never appended, and the carried fault stands.
        self.build()
        garbled = self.local_letter("held; record truncated")
        kept = self.local_letter("held; record intact")
        self.sweep(self.trey, expect=1)
        index = self.trey.root / "bridge" / "local-held-index.txt"
        index.unlink()
        self.sweep(self.trey, expect=1)
        self.assert_store_fault("index_missing")
        path = self.record_path(garbled)
        path.write_bytes(path.read_bytes()[:20])
        index.touch()
        self.sweep(self.trey, expect=1)
        self.assert_store_fault("index_short")
        self.assertEqual(index.read_text().split(), [kept])

    def test_a_record_whose_letter_was_pruned_still_counts(self):
        """Regression guard (passes on ccbffa3): fails if record validity starts requiring the archive binding."""
        # Review 4, finding 1: validity does not require the archive binding,
        # so a hold whose archived letter was pruned still meets the floor.
        self.build()
        pruned = self.local_letter("held; archive copy pruned")
        kept = self.local_letter("held; archive copy kept")
        self.sweep(self.trey, expect=1)
        index = self.trey.root / "bridge" / "local-held-index.txt"
        index.unlink()
        self.sweep(self.trey, expect=1)
        self.assert_store_fault("index_missing")
        (self.trey.root / "archive" / (pruned + ".mail")).unlink()
        index.touch()
        self.sweep(self.trey, expect=None)
        self.assertEqual(self.health(self.trey)["local_held"]["fault_reasons"], {})
        self.assertEqual(sorted(index.read_text().split()), sorted([pruned, kept]))

    # -- seeding ------------------------------------------------------------

    def test_seed_stamps_good_rows_and_refuses_a_digest_mismatch(self):
        self.build()
        good = self.local_letter("seed good")
        bad = self.local_letter("seed bad")
        nocopy = self.local_letter("seed wrong copy path")
        remote = self.trey.send("hq", "garden", "queued for fc, not local")
        # No tick has run since the sends, so no observed hold exists yet.
        self.assertIsNone(self.record(good))
        manifest = Path(self.temporary.name) / "manifest.jsonl"
        manifest.write_text(
            self.manifest_row(good) + "\n"
            + self.manifest_row(bad, archive_sha256="0" * 64) + "\n"
            + self.manifest_row(nocopy, local_copies=[f"atlasos/read/{nocopy}.mail"]) + "\n"
            + self.manifest_row(
                remote, room="garden", local_copies=[f"garden/inbox/{remote}.mail"]
            ) + "\n"
        )
        before = (manifest.read_bytes(), manifest.stat().st_mtime_ns)
        result = self.seed(self.trey, manifest)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        lines = [json.loads(line) for line in result.stdout.splitlines()]
        mismatches = [line for line in lines if line["action"] == "local_held_seed_mismatch"]
        self.assertEqual(
            [(line["id"], line["reason"]) for line in mismatches],
            [(bad, "digest_mismatch"), (nocopy, "local_copy_mismatch"),
             (remote, "room_not_local")],
        )
        summary = lines[-1]
        manifest_sha = sha(before[0])
        self.assertEqual(
            (summary["stamped"], summary["mismatched"], summary["manifest_sha256"]),
            (1, 3, manifest_sha),
        )
        copy = f"bridge/local-held-manifests/{manifest_sha}.jsonl"
        self.assertEqual((self.trey.root / copy).read_bytes(), before[0])
        self.assertEqual((manifest.read_bytes(), manifest.stat().st_mtime_ns), before)
        record = self.record(good)
        self.assertEqual(
            (record["reason"], record["evidence"]),
            ("seeded", [f"atlasos/inbox/{good}.mail", copy]),
        )
        for mail_id in (bad, nocopy, remote):
            self.assertIsNone(self.record(mail_id))
        # The manifest still expects every row it lists: the next tick holds
        # each refused row as a visible fault instead of stamping it afresh
        # or exporting it.
        self.sweep(self.trey, expect=1)
        for mail_id in (bad, nocopy, remote):
            self.assertIsNone(self.record(mail_id))
        self.assertFalse((self.trey.root / "bridge" / "published" / remote).exists())
        self.assertEqual(
            sorted((r["id"], r["fault"]) for r in self.actions(self.trey, "local_held_fault")),
            sorted([(bad, "missing"), (nocopy, "missing"), (remote, "missing")]),
        )

        clean = Path(self.temporary.name) / "clean.jsonl"
        clean.write_text(self.manifest_row(good) + "\n")
        again = self.seed(self.trey, clean)
        self.assertEqual(again.returncode, 0, again.stdout + again.stderr)
        self.assertEqual(json.loads(again.stdout.splitlines()[-1])["already_held"], 1)

    def test_seed_holds_contested_letters_of_unknown_intent(self):
        self.build()
        anchor = self.local_letter("the manifest's own row")
        mail_id = self.local_letter("copy lost before any tick")
        (self.trey.root / "atlasos" / "inbox" / (mail_id + ".mail")).unlink()
        self.sweep(self.trey, expect=1)
        self.assertIsNone(self.record(mail_id))
        self.assertEqual(self.health(self.trey)["local_held"]["candidates_unaccounted"], 1)
        manifest = Path(self.temporary.name) / "anchor.jsonl"
        manifest.write_text(self.manifest_row(anchor) + "\n")
        result = self.seed(self.trey, manifest)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        summary = json.loads(result.stdout.splitlines()[-1])
        self.assertEqual((summary["unknown_intent"], summary["contested"]), (1, ["atlasos"]))
        self.assertEqual(summary["already_held"], 1)
        self.assertEqual(self.record(mail_id)["reason"], "unknown_intent")
        self.sweep(self.trey, expect=1)
        self.assertEqual(self.health(self.trey)["local_held"]["candidates_unaccounted"], 0)

    def test_seed_refuses_a_manifest_without_this_host(self):
        self.build()
        mail_id = self.local_letter("unknown intent, never seeded here")
        (self.trey.root / "atlasos" / "inbox" / (mail_id + ".mail")).unlink()
        self.sweep(self.trey, expect=1)
        for label, text in (
            ("empty", ""),
            ("another host", self.manifest_row(mail_id, host="mac") + "\n"),
            ("unparseable", "{not json\n"),
        ):
            with self.subTest(label):
                manifest = Path(self.temporary.name) / "other.jsonl"
                manifest.write_text(text)
                result = self.seed(self.trey, manifest)
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                last = json.loads(result.stdout.splitlines()[-1])
                self.assertEqual(
                    (last["action"], last["error"]),
                    ("local_held_seed", "no manifest row names host trey"),
                )
                self.assertFalse(
                    (self.trey.root / "bridge" / "local-held-manifests").exists()
                )
                # Nothing ran: not even the unknown-intent pass.
                self.assertIsNone(self.record(mail_id))

    def test_seed_over_an_unreadable_or_unwritable_index_writes_nothing(self):
        # Round 2, finding 3: a typed summary and exit 1, never a traceback
        # and never a record written before the rebuild could fail.
        cases = [("directory", "index_unreadable")]
        if os.geteuid() != 0:  # root writes through a 0400 file
            cases.append(("read-only", "index_unwritable"))
        for label, error in cases:
            with self.subTest(label):
                self.build()
                mail_id = self.local_letter(f"seed over a {label} index")
                index = self.trey.root / "bridge" / "local-held-index.txt"
                self.assertFalse(index.exists())  # precondition: nothing held yet
                if label == "directory":
                    index.mkdir()
                else:
                    index.write_bytes(b"")
                    index.chmod(0o400)
                manifest = Path(self.temporary.name) / "manifest.jsonl"
                manifest.write_text(self.manifest_row(mail_id) + "\n")
                result = self.seed(self.trey, manifest)
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                self.assertEqual(result.stderr, "")
                summary = json.loads(result.stdout.splitlines()[-1])
                self.assertEqual(
                    (summary["action"], summary["ok"], summary["error"], summary["rows"]),
                    ("local_held_seed", False, error, 1),
                )
                self.assertIsNone(self.record(mail_id))
                self.assertFalse(
                    (self.trey.root / "bridge" / "local-held-manifests").exists()
                )
                if label == "read-only":
                    self.assertEqual(index.read_bytes(), b"")
                    index.chmod(0o600)

    def test_deleted_manifest_copies_are_a_store_fault(self):
        # Round 2, finding 4: a refused row's only expectation is its
        # manifest copy; deleting the copies must not release the letter.
        self.build()
        good = self.local_letter("seeded")
        refused = self.local_letter("refused row, expected only by the manifest")
        manifest = Path(self.temporary.name) / "manifest.jsonl"
        manifest.write_text(
            self.manifest_row(good) + "\n"
            + self.manifest_row(refused, local_copies=[f"atlasos/read/{refused}.mail"]) + "\n"
        )
        self.assertEqual(self.seed(self.trey, manifest).returncode, 1)  # the refused row
        self.sweep(self.trey, expect=1)
        local_held = self.health(self.trey)["local_held"]
        self.assertEqual(  # precondition: the refused row is a held fault
            (local_held["manifests"], local_held["fault_reasons"]), (1, {"missing": 1})
        )
        copies = self.trey.root / "bridge" / "local-held-manifests"
        for child in copies.iterdir():
            child.unlink()
        copies.rmdir()
        self.deregister_atlasos()
        self.sweep(self.trey, expect=1)
        control = self.control_letter()
        for _ in range(2):
            self.sweep(self.trey, expect=1)
            self.assert_store_fault("manifest_missing")
            self.assertEqual(self.health(self.trey)["local_held"]["manifests"], 1)
            for mail_id in (good, refused, control):
                self.assertFalse(self.exported(mail_id), mail_id)
        missing = [r for r in self.actions(self.trey, "local_held_fault")
                   if r.get("fault") == "manifest_missing"]
        self.assertEqual([(r["carried"], r["present"]) for r in missing], [(1, 0)])
        # Recovery: re-seed the same manifest. The refused row still fails
        # (exit 1), but its copy is back and the letter is expected again.
        self.assertEqual(self.seed(self.trey, manifest).returncode, 1)
        self.sweep(self.trey, expect=None)
        local_held = self.health(self.trey)["local_held"]
        self.assertEqual(local_held["fault_reasons"], {"missing": 1})
        self.assertTrue(self.exported(control), "route to mac never reopened")
        for mail_id in (good, refused):
            self.assertFalse(self.exported(mail_id), mail_id)
        # A lost manifest is accepted, not restored: --accept-lost-records
        # resets the carried count to the copies present. The refused row's
        # expectation goes with its manifest; that is what accepting means.
        other = Path(self.temporary.name) / "good-only.jsonl"
        other.write_text(self.manifest_row(good) + "\n")
        self.assertEqual(self.seed(self.trey, other).returncode, 0)
        self.sweep(self.trey, expect=None)
        self.assertEqual(self.health(self.trey)["local_held"]["manifests"], 2)
        for child in copies.iterdir():
            child.unlink()
        result = self.seed(self.trey, other, "--accept-lost-records")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.health(self.trey)["local_held"]["manifests"], 1)
        self.sweep(self.trey, expect=None)
        self.assertEqual(self.health(self.trey)["local_held"]["fault_reasons"], {})
        self.assertFalse(self.exported(good))

    def test_manifest_floor_counts_intact_copies_only(self):
        # Review 3, finding 8: the count is per intact copy. Deleting one of
        # two overlapping manifests is manifest_missing though no id was
        # lost, and a misnamed file cannot stand in for the deleted copy.
        self.build()
        mail_id = self.local_letter("named by both manifests")
        first = Path(self.temporary.name) / "first.jsonl"
        first.write_text(self.manifest_row(mail_id) + "\n")
        second = Path(self.temporary.name) / "second.jsonl"
        second.write_text(self.manifest_row(mail_id) + "\n\n")
        for manifest in (first, second):
            self.assertEqual(self.seed(self.trey, manifest).returncode, 0)
        self.sweep(self.trey, expect=1)
        self.assertEqual(self.health(self.trey)["local_held"]["manifests"], 2)
        copies = self.trey.root / "bridge" / "local-held-manifests"
        deleted = copies / (sha(second.read_bytes()) + ".jsonl")
        deleted.unlink()
        self.sweep(self.trey, expect=1)
        self.assert_store_fault("manifest_missing")
        # A misnamed copy of the deleted bytes is damage, and not a copy.
        (copies / ("0" * 64 + ".jsonl")).write_bytes(second.read_bytes())
        self.sweep(self.trey, expect=1)
        reasons = self.health(self.trey)["local_held"]["fault_reasons"]
        self.assertIn("manifest_damaged", reasons)
        self.assertIn("manifest_missing", reasons)
        missing = [(r["carried"], r["present"]) for r in self.actions(self.trey, "local_held_fault")
                   if r.get("fault") == "manifest_missing"]
        self.assertEqual(missing, [(2, 1)])
        self.deregister_atlasos()
        self.sweep(self.trey, expect=1)
        control = self.control_letter()
        self.sweep(self.trey, expect=1)
        self.assertFalse(self.exported(mail_id))
        self.assertFalse(self.exported(control))

    def foreign_copy(self, mail_id, note=""):
        """An intact manifest copy whose rows name only mac, persisted
        under its own sha256 as the seed would name it."""
        data = (self.manifest_row(mail_id, host="mac") + "\n" + note).encode()
        copies = self.trey.root / "bridge" / "local-held-manifests"
        copies.mkdir(parents=True, exist_ok=True)
        path = copies / (sha(data) + ".jsonl")
        path.write_bytes(data)
        return path

    def test_a_foreign_manifest_copy_does_not_stand_in_for_a_deleted_one(self):
        # Review 4, finding 5 (GLM): any intact copy counted, so a copy
        # naming only other hosts replaced this host's deleted copy and the
        # refused row's letter exported.
        self.build()
        good = self.local_letter("seeded")
        refused = self.local_letter("refused row, expected only by the manifest")
        manifest = Path(self.temporary.name) / "manifest.jsonl"
        manifest.write_text(
            self.manifest_row(good) + "\n"
            + self.manifest_row(refused, local_copies=[f"atlasos/read/{refused}.mail"]) + "\n"
        )
        self.assertEqual(self.seed(self.trey, manifest).returncode, 1)  # the refused row
        self.sweep(self.trey, expect=1)
        local_held = self.health(self.trey)["local_held"]
        self.assertEqual(  # precondition: the refused row is a held fault
            (local_held["manifests"], local_held["fault_reasons"]), (1, {"missing": 1})
        )
        (self.trey.root / "bridge" / "local-held-manifests"
         / (sha(manifest.read_bytes()) + ".jsonl")).unlink()
        self.foreign_copy(refused)
        self.deregister_atlasos()
        self.sweep(self.trey, expect=1)
        control = self.control_letter()
        for _ in range(2):
            self.sweep(self.trey, expect=1)
            self.assert_store_fault("manifest_missing")
            for mail_id in (good, refused, control):
                self.assertFalse(self.exported(mail_id), mail_id)

    def test_foreign_manifest_copies_do_not_raise_the_floor(self):
        # Review 4, finding 5: copies naming only other hosts expect nothing
        # here, so they neither count nor go missing.
        self.build()
        mail_id = self.local_letter("seeded")
        manifest = Path(self.temporary.name) / "manifest.jsonl"
        manifest.write_text(self.manifest_row(mail_id) + "\n")
        self.assertEqual(self.seed(self.trey, manifest).returncode, 0)
        self.sweep(self.trey, expect=1)
        self.assertEqual(self.health(self.trey)["local_held"]["manifests"], 1)  # precondition
        foreign = [self.foreign_copy(mail_id), self.foreign_copy(mail_id, "\n")]
        self.sweep(self.trey, expect=1)
        self.assertEqual(self.health(self.trey)["local_held"]["manifests"], 1)
        self.assertEqual(self.sentinel()["manifests"], 1)
        for path in foreign:
            path.unlink()
        self.sweep(self.trey, expect=1)
        local_held = self.health(self.trey)["local_held"]
        self.assertEqual((local_held["manifests"], local_held["fault_reasons"]), (1, {}))

    def test_seed_takes_the_tick_lock(self):
        import fcntl

        self.build()
        mail_id = self.local_letter("seed while a tick runs")
        manifest = Path(self.temporary.name) / "manifest.jsonl"
        manifest.write_text(self.manifest_row(mail_id) + "\n")
        anchor = self.trey.root / "bridge" / ".lock.anchor"
        descriptor = os.open(str(anchor), os.O_RDWR)
        try:
            fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
            busy = self.seed(self.trey, manifest)
        finally:
            os.close(descriptor)
        self.assertEqual(busy.returncode, 1, busy.stdout + busy.stderr)
        self.assertEqual(json.loads(busy.stdout.splitlines()[-1])["error"], "busy")
        self.assertIsNone(self.record(mail_id))
        self.assertFalse((self.trey.root / "bridge" / "local-held-manifests").exists())

    def test_seed_refuses_a_tampered_manifest_copy(self):
        self.build()
        mail_id = self.local_letter("seed over a tampered copy")
        manifest = Path(self.temporary.name) / "manifest.jsonl"
        data = (self.manifest_row(mail_id) + "\n").encode()
        manifest.write_bytes(data)
        copies = self.trey.root / "bridge" / "local-held-manifests"
        copies.mkdir(parents=True)
        tampered = copies / (sha(data) + ".jsonl")
        tampered.write_bytes(b"{}\n")
        result = self.seed(self.trey, manifest)
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        self.assertEqual(json.loads(result.stdout.splitlines()[-1])["action"], "config_error")
        self.assertIsNone(self.record(mail_id))
        self.assertEqual(tampered.read_bytes(), b"{}\n")
        # A damaged manifest copy is a store-level fault: nothing exports.
        control = self.trey.send("hq", "garden", "blocked by a damaged manifest")
        self.sweep(self.trey, expect=1)
        self.assert_store_fault("manifest_damaged")
        self.assertFalse(self.exported(control))
        self.assertIsNone(self.record(mail_id))


if __name__ == "__main__":
    unittest.main()
