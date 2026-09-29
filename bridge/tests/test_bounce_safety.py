#!/usr/bin/env python3
"""The bounce path after review: a delivered letter is final, a bounce goes to
the sender the bridge recorded, and no record of an earlier attempt is trusted
unchecked.

The first tick of the bounce code on a live host removes real letters from the
shared relay, so each rule here is one way that could go wrong:

* a letter the receiver already delivered must never be bounced, however the
  receiver's rooms and names change afterwards;
* a bounce reaches the participant and workspace that sent the letter, as the
  bridge recorded them when it took the letter, never whoever the letter's
  stamps or the participant's current workspace point at today;
* after a crash a published notice is completed where it is, and any record
  or file that does not describe this letter stops the retirement.

Like test_terminal.py these drive the real sweep.py against real Post roots
and a real bare relay.
"""

import hashlib
import json
import signal
import subprocess
import tempfile
import time
import types
import unittest
from pathlib import Path

from .test_sweep import SWEEPER, CanonicalTemporaryDirectory, Topology, craft_mail, fixed_id
from .test_terminal import FORGED_SELF, TerminalFixture

NOTHING = "0" * 64


class Log:
    def __init__(self):
        self.records = []

    def emit(self, action, **fields):
        self.records.append(dict(fields, action=action))


class DeliveredIsFinalTest(TerminalFixture):
    """A `delivered` receipt or ledger entry is final: the sender retires the
    letter on it, so nothing that changes later may take it back."""

    def deliver_one(self):
        """One letter fc -> trey/hq, delivered. fc has not yet read the
        receipt, so its outbox entry is still in the relay."""
        self.bootstrap()
        mail_id = self.fc.send("garden", "hq", "delivered and staying delivered")
        self.full_sweep(self.fc)
        self.full_sweep(self.trey)
        relative = f"outbox/trey/hq/{mail_id}.mail"
        self.assertEqual(self.receipt(self.trey, "fc", "hq", mail_id)["status"], "delivered")
        self.assertTrue((self.trey.root / "hq" / "inbox" / (mail_id + ".mail")).is_file())
        self.assertIn(relative, self.remote_tree("fc"))
        return mail_id, relative

    def assert_stays_delivered(self, mail_id, relative, returncodes=(0,)):
        """The receiver still says delivered and the sender retires the entry
        as a delivery: no bounce, no notice, nothing to attend to."""
        receipt = self.receipt(self.trey, "fc", "hq", mail_id)
        self.assertEqual(receipt["status"], "delivered", receipt)
        self.assertEqual(self.actions(self.trey, "quarantined", mail_id), [])
        self.assertFalse(
            (self.trey.root / "bridge" / "quarantine" / "fc" / "hq" / (mail_id + ".mail")).exists()
        )
        self.assertEqual(
            [i for i in self.health(self.trey)["attention"] if i["id"] == mail_id], []
        )
        # A zero-second grace makes any refusal receipt bounce at once, so a
        # downgraded receipt would show up here as a removed, bounced letter.
        self.full_sweep(
            self.fc, returncodes=returncodes, BRIDGE_BOUNCE_TRANSIENT_SECONDS=0
        )
        self.assertEqual(self.actions(self.fc, "letter_bounced"), [])
        self.assertEqual(self.actions(self.fc, "outbox_bounced"), [])
        self.assertEqual(len(self.actions(self.fc, "outbox_pruned", mail_id)), 1)
        self.assertNotIn(relative, self.remote_tree("fc"))
        self.assertEqual(
            [i for i in self.health(self.fc)["attention"] if i["kind"] != "name_collision"],
            [],
        )
        self.assertFalse(list((self.fc.root / "bridge").glob("bounced/**/*.mail")))
        self.assertTrue((self.trey.root / "hq" / "inbox" / (mail_id + ".mail")).is_file())

    def room_ack(self, machine, mail_id):
        path = machine.root / "bridge" / "room-acked" / (mail_id + ".json")
        return json.loads(path.read_text()) if path.exists() else None

    def test_a_delivered_room_letter_leaves_a_durable_verdict(self):
        mail_id, relative = self.deliver_one()
        self.assertIsNone(self.room_ack(self.fc, mail_id))
        self.assert_stays_delivered(mail_id, relative)
        record = self.room_ack(self.fc, mail_id)
        archived = (self.fc.root / "archive" / (mail_id + ".mail")).read_bytes()
        self.assertEqual(
            set(record),
            {"v", "id", "host", "room", "status", "reason", "sha256", "at"},
        )
        self.assertEqual(
            {k: record[k] for k in ("v", "id", "host", "room", "status", "reason")},
            {"v": 1, "id": mail_id, "host": "trey", "room": "hq",
             "status": "delivered", "reason": None},
        )
        self.assertEqual(record["sha256"], hashlib.sha256(archived).hexdigest())
        self.assertRegex(record["at"], r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\+00:00$")

    def preplace_ack(self, mail_id, **changes):
        archived = (self.fc.root / "archive" / (mail_id + ".mail")).read_bytes()
        record = {
            "v": 1, "id": mail_id, "host": "trey", "room": "hq",
            "status": "delivered", "reason": None,
            "sha256": hashlib.sha256(archived).hexdigest(),
            "at": "2000-01-01T00:00:00+00:00",
        }
        record.update(changes)
        ack = self.fc.root / "bridge" / "room-acked" / (mail_id + ".json")
        ack.parent.mkdir(parents=True, exist_ok=True)
        ack.write_text(json.dumps(record, sort_keys=True) + "\n")
        return ack

    def test_this_letters_own_verdict_already_on_disk_is_kept_and_prunes(self):
        mail_id, relative = self.deliver_one()
        ack = self.preplace_ack(mail_id)
        before = ack.read_text()
        self.assert_stays_delivered(mail_id, relative)
        self.assertEqual(ack.read_text(), before)

    def test_a_disagreeing_verdict_on_disk_keeps_the_entry_and_is_never_replaced(self):
        disagreements = [
            {"id": "20260101-000000-aaaaaa"},
            {"sha256": "0" * 64},
            {"room": "atlasos"},
            {"host": "mac"},
            {"status": "rejected", "reason": "unknown_room"},
            {"extra": 1},
            {"at": "invalid"},
            {"at": "2026-09-29"},
        ]
        mail_id, relative = self.deliver_one()
        for changes in disagreements:
            ack = self.preplace_ack(mail_id, **changes)
            before = ack.read_text()
            self.full_sweep(self.fc, returncodes=(0, 1))
            self.assertIn(relative, self.remote_tree("fc"), changes)
            self.assertEqual(ack.read_text(), before, changes)
        self.assertGreaterEqual(
            len(self.actions(self.fc, "room_ack_conflict", mail_id)), len(disagreements)
        )
        ack.write_text("{not json\n")
        self.full_sweep(self.fc, returncodes=(0, 1))
        self.assertIn(relative, self.remote_tree("fc"))
        self.assertEqual(ack.read_text(), "{not json\n")

    def test_a_room_ack_path_escaping_the_root_is_logged_not_fatal(self):
        mail_id, relative = self.deliver_one()
        outside = Path(self.temporary.name) / "elsewhere"
        outside.mkdir()
        link = self.fc.root / "bridge" / "room-acked"
        link.parent.mkdir(parents=True, exist_ok=True)
        link.symlink_to(outside, target_is_directory=True)
        self.full_sweep(self.fc, returncodes=(0, 1))
        self.assertIn(relative, self.remote_tree("fc"))
        self.assertEqual(len(self.actions(self.fc, "room_ack_failed", mail_id)), 1)
        self.assertEqual(list(outside.iterdir()), [])

    def test_an_unwritable_verdict_keeps_the_entry_for_the_next_tick(self):
        mail_id, relative = self.deliver_one()
        blocker = self.fc.root / "bridge" / "room-acked"
        blocker.parent.mkdir(parents=True, exist_ok=True)
        blocker.write_text("not a directory")
        self.full_sweep(self.fc, returncodes=(0, 1))
        self.assertIn(relative, self.remote_tree("fc"))
        self.assertEqual(len(self.actions(self.fc, "room_ack_failed", mail_id)), 1)
        blocker.unlink()
        self.full_sweep(self.fc)
        self.assertNotIn(relative, self.remote_tree("fc"))
        self.assertEqual(self.room_ack(self.fc, mail_id)["status"], "delivered")

    def test_a_sender_name_contested_after_delivery_does_not_take_it_back(self):
        mail_id, relative = self.deliver_one()
        # trey takes a real room named like the sender's. The contest comes
        # from the published room lists, as it does live; a pinned name that
        # is also a registered room is fatal, so the pins go.
        for machine in self.topology.machines:
            config_path = machine.root / "bridge" / "config.json"
            config = json.loads(config_path.read_text())
            config["peers"] = {
                host: [room for room in names if room != "garden"]
                for host, names in config["peers"].items()
            }
            config_path.write_text(json.dumps(config, sort_keys=True) + "\n")
        garden = self.trey.base / "rooms" / "garden"
        garden.mkdir()
        table_path = self.trey.root / "rooms.json"
        table = json.loads(table_path.read_text())
        table["garden"] = str(garden)
        table_path.write_text(json.dumps(table, indent=2) + "\n")
        # A room-name collision is a standing unhealthy condition: exit 1.
        self.full_sweep(self.trey, returncodes=(0, 1))
        health = self.health(self.trey)
        self.assertEqual(health["reason"], "room_name_collision")  # the contest is real
        self.assertEqual(
            [i["id"] for i in health["attention"] if i["kind"] == "name_collision"],
            ["garden"],
        )
        self.assert_stays_delivered(mail_id, relative, returncodes=(0, 1))

    def test_a_crash_between_ledger_and_receipt_then_a_removed_room_still_delivers(self):
        # The ledger is written before the receipt. A crash between them and
        # then the room going away leaves a delivered letter with no receipt:
        # the tick after must write the delivered receipt, not a refusal.
        self.bootstrap()
        mail_id = self.fc.send("garden", "hq", "crashed before its receipt")
        self.full_sweep(self.fc)
        crashed = self.trey.sweep(BRIDGE_CRASH_AFTER="inbound-i8")
        self.assertEqual(crashed.returncode, -signal.SIGKILL, crashed.stdout + crashed.stderr)
        self.assertTrue((self.trey.root / "bridge" / "delivered" / "fc" / "hq" / mail_id).is_file())
        self.assertIsNone(self.receipt(self.trey, "fc", "hq", mail_id))
        self.remove_room(self.trey, "hq")
        self.full_sweep(self.trey)
        self.assert_stays_delivered(mail_id, f"outbox/trey/hq/{mail_id}.mail")

    def test_a_delivered_receipt_alone_is_final(self):
        # No ledger entry (lost, or never written): the receipt says delivered
        # and that is enough. The refusal must not even be attempted, which
        # would leave a forensic copy and a quarantine line behind.
        mail_id, relative = self.deliver_one()
        (self.trey.root / "bridge" / "delivered" / "fc" / "hq" / mail_id).unlink()
        self.remove_room(self.trey, "hq")
        self.full_sweep(self.trey)
        self.assert_stays_delivered(mail_id, relative)

    def test_a_delivery_made_under_looser_rules_is_kept(self):
        # A letter an older bridge delivered under a looser envelope grammar
        # (here: a kind this bridge refuses) is still delivered. Read again
        # under today's rules it must not become a refusal.
        self.bootstrap()
        mail_id = fixed_id(0x7E10)
        data = craft_mail(mail_id, "garden", "hq", kind="bogus")
        digest = hashlib.sha256(data).hexdigest()
        self.fc.inject(f"outbox/trey/hq/{mail_id}.mail", data)
        ledger = self.trey.root / "bridge" / "delivered" / "fc" / "hq" / mail_id
        ledger.parent.mkdir(parents=True, exist_ok=True)
        ledger.write_text(digest + "\n")
        delivered = SWEEPER.receipt_bytes("delivered", "fc", "hq", mail_id, digest, "")
        self.trey.inject(f"receipts/fc/hq/{mail_id}.json", delivered)
        self.full_sweep(self.trey)
        receipt_path = self.trey.repo / "receipts" / "fc" / "hq" / (mail_id + ".json")
        self.assertEqual(receipt_path.read_bytes(), delivered)
        self.assertEqual(self.actions(self.trey, "quarantined", mail_id), [])
        self.assertFalse(
            (self.trey.root / "bridge" / "quarantine" / "fc" / "hq" / (mail_id + ".mail")).exists()
        )

    def test_the_receipt_writer_never_downgrades_a_delivered_receipt(self):
        with tempfile.TemporaryDirectory(prefix="post-bridge-receipt-") as raw:
            base = Path(raw).resolve()
            root, repo = base / "mail", base / "repo"
            root.mkdir()
            repo.mkdir()
            settings = types.SimpleNamespace(root=root, repo=repo)
            log = Log()
            mail_id = fixed_id(0x7E01)
            path = repo / "receipts" / "fc" / "hq" / (mail_id + ".json")
            SWEEPER.write_receipt(settings, "fc", "hq", mail_id, "delivered", "a" * 64, "", log)
            delivered = path.read_bytes()
            for status, reason in (
                ("quarantined", "unknown_room"),
                ("quarantined", FORGED_SELF),
                ("held", "blocked"),
            ):
                with self.subTest(status=status, reason=reason):
                    SWEEPER.write_receipt(
                        settings, "fc", "hq", mail_id, status, "a" * 64, reason, log
                    )
                    self.assertEqual(path.read_bytes(), delivered)
            self.assertEqual(
                [r["action"] for r in log.records].count("receipt_downgrade_refused"), 3
            )
            # A different letter under the same id is a different fact: an id
            # collision is still recorded as the quarantine it is.
            SWEEPER.write_receipt(
                settings, "fc", "hq", mail_id, "quarantined", "b" * 64, "id-collision", log
            )
            value = json.loads(path.read_text())
            self.assertEqual(
                (value["status"], value["sha256"], value["reason"]),
                ("quarantined", "b" * 64, "id-collision"),
            )


class TwoRoomFixture(TerminalFixture):
    """As TerminalFixture, but fc has a second room, `orchard`."""

    def setUp(self):
        self.temporary = CanonicalTemporaryDirectory(prefix="post-bridge-bounce-")
        self.topology = Topology(self.temporary.name)
        self.fc = self.topology.add("fc", ["garden", "orchard"])
        self.trey = self.topology.add("trey", ["hq", "atlasos"])
        self.mac = self.topology.add("mac", ["porch"])
        self.topology.finalize()

    def refuse_atlasos(self):
        self.bootstrap()
        self.remove_room(self.trey, "atlasos")
        self.full_sweep(self.trey)

    def origin(self, mail_id):
        return json.loads(
            (self.fc.root / "bridge" / "origin" / (mail_id + ".json")).read_text()
        )

    def bounce_where(self, mail_id):
        return self.actions(self.fc, "letter_bounced", mail_id)[0]["where"]

    def notices(self, inbox, mail_id):
        """Notices in `inbox` that are about the refused letter `mail_id`."""
        return [
            path
            for path in self.notice_files(inbox)
            if mail_id in path.read_text(encoding="utf-8")
        ]

    def participant_inbox(self, participant):
        return self.fc.root / "participants" / participant / "inbox"


class BounceRoutingTest(TwoRoomFixture):
    def test_the_bridge_records_who_sent_a_letter_when_it_takes_it(self):
        self.refuse_atlasos()
        participant = self.fc.participant("garden")
        mail_id = fixed_id(0x7F01)
        self.publish_letter(mail_id, "garden", "atlasos", "who sent me", participant=participant)
        origin = self.origin(mail_id)
        self.assertEqual(
            (origin["id"], origin["participant"], origin["workspace"]),
            (mail_id, participant, "garden"),
        )
        archived = (self.fc.root / "archive" / (mail_id + ".mail")).read_bytes()
        self.assertEqual(origin["sha256"], hashlib.sha256(archived).hexdigest())

    def test_a_participant_that_moved_on_does_not_get_a_bounce_for_the_old_workspace(self):
        self.refuse_atlasos()
        participant = self.fc.participant("garden")
        mail_id = fixed_id(0x7F02)
        self.publish_letter(mail_id, "garden", "atlasos", "sent from garden", participant=participant)
        self.full_sweep(self.trey)
        # The participant is now working from `orchard`.
        record_path = self.fc.root / "participants" / participant / "participant.json"
        record = json.loads(record_path.read_text())
        record["workspace"] = "orchard"
        record["workspace_path"] = str(self.fc.workspaces["orchard"])
        record_path.write_text(json.dumps(record))
        self.full_sweep(self.fc, BRIDGE_BOUNCE_TRANSIENT_SECONDS=0)
        self.assertEqual(self.bounce_where(mail_id), "room:garden")
        self.assertEqual(self.notice_files(self.participant_inbox(participant)), [])
        self.assertEqual(len(self.notices(self.fc.root / "garden" / "inbox", mail_id)), 1)
        self.assertEqual(self.notice_files(self.fc.root / "orchard" / "inbox"), [])

    def test_a_stamp_naming_another_rooms_participant_is_not_believed(self):
        self.refuse_atlasos()
        elsewhere = self.fc.participant("orchard")
        mail_id = fixed_id(0x7F03)
        self.publish_letter(mail_id, "garden", "atlasos", "stamped oddly", participant=elsewhere)
        origin = self.origin(mail_id)
        self.assertEqual((origin["participant"], origin["workspace"]), (None, "garden"))
        self.full_sweep(self.trey)
        self.full_sweep(self.fc, BRIDGE_BOUNCE_TRANSIENT_SECONDS=0)
        self.assertEqual(self.bounce_where(mail_id), "room:garden")
        self.assertEqual(self.notice_files(self.participant_inbox(elsewhere)), [])
        self.assertEqual(len(self.notices(self.fc.root / "garden" / "inbox", mail_id)), 1)

    def test_a_recorded_room_this_host_no_longer_owns_is_a_dead_letter(self):
        # The letter was sent from `garden`, and nobody else can be shown to
        # have sent it; `garden` is gone from fc's room table before the
        # bounce, so there is no inbox that is really the sender's.
        self.refuse_atlasos()
        mail_id = fixed_id(0x7F08)
        relative = self.publish_letter(mail_id, "garden", "atlasos", "sent from a gone room")
        self.full_sweep(self.trey)
        self.remove_room(self.fc, "garden")
        self.full_sweep(self.fc, BRIDGE_BOUNCE_TRANSIENT_SECONDS=0)
        self.assertNotIn(relative, self.remote_tree("fc"))
        self.assertEqual(self.bounce_where(mail_id), "dead-letter")
        self.assertEqual(self.notice_files(self.fc.root / "garden" / "inbox"), [])
        self.assertEqual(len(list((self.fc.root / "bridge" / "bounced" / "undeliverable").glob("*.mail"))), 1)
        self.assertEqual([i["kind"] for i in self.health(self.fc)["attention"]], ["refused_letter"])

    def test_a_letter_with_no_record_and_stamps_that_do_not_check_out_is_a_dead_letter(self):
        # The three letters stuck on the Mac: published by a bridge that kept
        # no record, `from` naming a room the receiver owns, stamped with a
        # participant whose workspace is another room.
        self.bootstrap()
        participant = self.fc.participant("garden")
        mail_id = fixed_id(0x7F05)
        relative = self.stick(mail_id, "hq", "atlasos", "stuck", participant=participant)
        self.full_sweep(self.trey)
        refusal = self.receipt(self.trey, "fc", "atlasos", mail_id)
        self.assertEqual((refusal["status"], refusal["reason"]), ("quarantined", FORGED_SELF))
        self.full_sweep(self.fc)
        self.assertNotIn(relative, self.remote_tree("fc"))
        self.assertEqual(self.bounce_where(mail_id), "dead-letter")
        self.assertEqual(self.notice_files(self.participant_inbox(participant)), [])
        self.assertEqual(self.notice_files(self.fc.root / "garden" / "inbox"), [])
        dead = self.fc.root / "bridge" / "bounced" / "undeliverable"
        letters = sorted(dead.glob("*.mail"))
        self.assertEqual(len(letters), 1)
        self.assertIn(mail_id, letters[0].read_text())
        items = self.health(self.fc)["attention"]
        # The item is about the refused letter, not about the notice file.
        self.assertEqual([(i["kind"], i["id"]) for i in items],
                         [("refused_letter", mail_id)], items)
        self.assertIn(str(letters[0]), items[0]["fix"])

    def test_a_letter_with_no_record_and_no_participant_is_a_dead_letter(self):
        # `from` names a room this host owns, but nothing vouches for who at
        # that workspace wrote the letter.
        self.refuse_atlasos()
        mail_id = fixed_id(0x7F06)
        relative = self.stick(mail_id, "garden", "atlasos", "no stamp")
        self.full_sweep(self.trey)
        self.full_sweep(self.fc, BRIDGE_BOUNCE_TRANSIENT_SECONDS=0)
        self.assertNotIn(relative, self.remote_tree("fc"))
        self.assertEqual(self.bounce_where(mail_id), "dead-letter")
        self.assertEqual(self.notice_files(self.fc.root / "garden" / "inbox"), [])
        self.assertEqual(len(list((self.fc.root / "bridge" / "bounced" / "undeliverable").glob("*.mail"))), 1)
        self.assertEqual([i["kind"] for i in self.health(self.fc)["attention"]], ["refused_letter"])

    def test_a_letter_with_no_record_is_believed_while_its_stamps_check_out(self):
        self.refuse_atlasos()
        participant = self.fc.participant("garden")
        mail_id = fixed_id(0x7F07)
        relative = self.stick(mail_id, "garden", "atlasos", "old but sound", participant=participant)
        self.full_sweep(self.trey)
        self.full_sweep(self.fc, BRIDGE_BOUNCE_TRANSIENT_SECONDS=0)
        self.assertNotIn(relative, self.remote_tree("fc"))
        self.assertEqual(self.bounce_where(mail_id), "participant:" + participant)
        self.assertEqual(len(self.notices(self.participant_inbox(participant), mail_id)), 1)
        self.assertEqual(self.health(self.fc)["attention"], [])


class BounceRecordTest(TwoRoomFixture):
    """Every file a redo finds is checked against the letter in hand."""

    def refused(self, participant=None):
        """A refused letter from `garden`, published by the bridge (so the
        notice goes to garden's inbox, or the participant's)."""
        self.refuse_atlasos()
        mail_id = fixed_id(0x7F80)
        relative = self.publish_letter(
            mail_id, "garden", "atlasos", "bounce me", participant=participant
        )
        self.full_sweep(self.trey)
        return mail_id, relative

    def crash_at(self, hook):
        crashed = self.fc.sweep(BRIDGE_CRASH_AFTER=hook, BRIDGE_BOUNCE_TRANSIENT_SECONDS=0)
        self.assertEqual(crashed.returncode, -signal.SIGKILL, crashed.stdout + crashed.stderr)

    def bounced(self, name):
        return self.fc.root / "bridge" / "bounced" / name

    def intent(self, mail_id):
        return json.loads(self.bounced(mail_id + ".json").read_text())

    def notice_path(self, mail_id):
        return self.fc.root / "garden" / "inbox" / (self.intent(mail_id)["letter_id"] + ".mail")

    def copies_of_notice(self, letter_id):
        return sorted(self.fc.root.rglob(letter_id + ".mail"))

    def test_a_published_notice_is_completed_where_it_is_when_the_room_changes(self):
        mail_id, relative = self.refused()
        self.crash_at("bounce-b3-letter")  # the notice is published; .sent is not
        intent = self.intent(mail_id)
        self.assertEqual(intent["where"], "room:garden")
        self.assertTrue(self.notice_path(mail_id).is_file())
        self.assertFalse(self.bounced(mail_id + ".sent").exists())
        # The room the notice sits in is no longer registered when the bridge
        # comes back. It must not pick another destination and write again.
        self.remove_room(self.fc, "garden")
        recovered = self.fc.sweep(BRIDGE_BOUNCE_TRANSIENT_SECONDS=0)
        self.assertEqual(recovered.returncode, 0, recovered.stdout + recovered.stderr)
        self.assertEqual(self.copies_of_notice(intent["letter_id"]), [self.notice_path(mail_id)])
        self.assertEqual(list(self.bounced("undeliverable").glob("*.mail")), [])
        self.assertTrue(self.bounced(mail_id + ".sent").is_file())
        self.assertNotIn(relative, self.remote_tree("fc"))
        self.assertEqual(len(self.actions(self.fc, "letter_bounced", mail_id)), 1)
        self.assertEqual(self.health(self.fc)["attention"], [])

    def assert_kept_until_fixed(self, mail_id, relative, path):
        """The tick kept the letter, said why, changed nothing, and the fix
        the attention item names, run as written, lets the next tick finish."""
        self.assertIn(relative, self.remote_tree("fc"))
        self.assertEqual(self.actions(self.fc, "outbox_bounced", mail_id), [])
        items = [i for i in self.health(self.fc)["attention"] if i["id"] == mail_id]
        self.assertEqual(len(items), 1, self.health(self.fc)["attention"])
        self.assertEqual(items[0]["kind"], "refused_letter")
        self.assertIn("cannot confirm", items[0]["summary"])
        self.assertIn(str(path), items[0]["fix"])
        # A second tick decides the same and still touches nothing.
        self.full_sweep(self.fc, BRIDGE_BOUNCE_TRANSIENT_SECONDS=0)
        self.assertIn(relative, self.remote_tree("fc"))
        subprocess.run(items[0]["fix"].split(" redoes that step: ")[-1], shell=True, check=True)
        self.assertFalse(path.exists())
        self.full_sweep(self.fc, BRIDGE_BOUNCE_TRANSIENT_SECONDS=0)
        self.assertNotIn(relative, self.remote_tree("fc"))
        self.assertEqual(len(self.actions(self.fc, "outbox_bounced", mail_id)), 1)
        self.assertEqual(self.health(self.fc)["attention"], [])
        # The letter is told to its sender exactly once.
        self.assertEqual(len(self.notices(self.fc.root / "garden" / "inbox", mail_id)), 1)

    def test_a_different_file_at_the_notice_path_is_never_taken_for_the_notice(self):
        mail_id, relative = self.refused()
        self.crash_at("bounce-b1-intent")
        path = self.notice_path(mail_id)
        stranger = craft_mail(
            self.intent(mail_id)["letter_id"], "someone", "garden",
            body=b"not a bounce at all\n", subject="something else",
        )
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(stranger)
        result = self.fc.sweep(BRIDGE_BOUNCE_TRANSIENT_SECONDS=0)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(path.read_bytes(), stranger)  # not overwritten, not adopted
        self.assertFalse(self.bounced(mail_id + ".sent").exists())
        self.assert_kept_until_fixed(mail_id, relative, path)

    def test_an_intent_written_for_another_letter_stops_the_retirement(self):
        for key, value in (
            ("id", fixed_id(0x7F81)),
            ("sha256", NOTHING),
            ("room", "hq"),
            ("notice_sha256", NOTHING),
            ("sent", "2000-01-01 00:00:00 +0000"),
            ("reason", "forged_self"),
            ("where", "room:orchard"),  # not where the sealed notice is addressed
        ):
            with self.subTest(key=key):
                self.tearDown()
                self.setUp()
                mail_id, relative = self.refused()
                self.crash_at("bounce-b1-intent")
                path = self.bounced(mail_id + ".json")
                intent = self.intent(mail_id)
                intent[key] = value
                path.write_text(json.dumps(intent))
                result = self.fc.sweep(BRIDGE_BOUNCE_TRANSIENT_SECONDS=0)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertFalse(self.bounced(mail_id + ".sent").exists())
                self.assert_kept_until_fixed(mail_id, relative, path)

    def test_a_sent_marker_for_another_letter_stops_the_retirement(self):
        for key, value in (
            ("id", fixed_id(0x7F82)),
            ("sha256", NOTHING),
            ("letter_id", fixed_id(0x7F83)),
            ("where", "dead-letter"),
        ):
            with self.subTest(key=key):
                self.tearDown()
                self.setUp()
                mail_id, relative = self.refused()
                self.crash_at("bounce-b4-sent")
                path = self.bounced(mail_id + ".sent")
                marker = json.loads(path.read_text())
                marker[key] = value
                path.write_text(json.dumps(marker))
                result = self.fc.sweep(BRIDGE_BOUNCE_TRANSIENT_SECONDS=0)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assert_kept_until_fixed(mail_id, relative, path)

    def test_a_saved_body_that_is_not_the_letters_stops_the_retirement(self):
        mail_id, relative = self.refused()
        self.crash_at("bounce-b2-body")
        path = self.bounced(mail_id + ".body")
        path.write_bytes(b"some other text\n")
        result = self.fc.sweep(BRIDGE_BOUNCE_TRANSIENT_SECONDS=0)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(self.bounced(mail_id + ".sent").exists())
        self.assert_kept_until_fixed(mail_id, relative, path)

    def test_a_sent_marker_with_no_notice_stops_the_retirement(self):
        mail_id, relative = self.refused()
        self.crash_at("bounce-b4-sent")  # the notice and its marker are both written
        notice = self.notice_path(mail_id)
        self.assertTrue(notice.is_file())
        notice.unlink()  # gone before the next tick
        result = self.fc.sweep(BRIDGE_BOUNCE_TRANSIENT_SECONDS=0)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(notice.exists())  # nothing was quietly written in its place
        self.assert_kept_until_fixed(mail_id, relative, self.bounced(mail_id + ".sent"))

    def test_a_dead_letter_notice_that_has_gone_stops_the_retirement(self):
        self.bootstrap()
        participant = self.fc.participant("garden")
        mail_id = fixed_id(0x7F91)
        relative = self.stick(mail_id, "hq", "atlasos", "stuck", participant=participant)
        self.full_sweep(self.trey)
        self.crash_at("bounce-b4-sent")
        (notice,) = self.bounced("undeliverable").glob("*.mail")
        notice.unlink()
        result = self.fc.sweep(BRIDGE_BOUNCE_TRANSIENT_SECONDS=0)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(relative, self.remote_tree("fc"))
        self.assertEqual(self.actions(self.fc, "outbox_bounced", mail_id), [])
        items = self.health(self.fc)["attention"]
        self.assertEqual([(i["kind"], i["id"]) for i in items], [("refused_letter", mail_id)])
        self.assertIn("cannot confirm", items[0]["summary"])
        sent = self.bounced(mail_id + ".sent")
        self.assertIn(str(sent), items[0]["fix"])
        subprocess.run(items[0]["fix"].split(" redoes that step: ")[-1], shell=True, check=True)
        self.full_sweep(self.fc, BRIDGE_BOUNCE_TRANSIENT_SECONDS=0)
        self.assertNotIn(relative, self.remote_tree("fc"))
        self.assertEqual(len(list(self.bounced("undeliverable").glob("*.mail"))), 1)
        self.assertEqual([i["id"] for i in self.health(self.fc)["attention"]], [mail_id])

    def test_a_notice_that_differs_anywhere_from_the_sealed_one_stops_the_retirement(self):
        edits = (
            ("subject", lambda text, intent: text.replace(
                '"subject": "Undeliverable: bounce me"', '"subject": "Undeliverable: another"')),
            ("reason", lambda text, intent: text.replace(
                "refused as: " + intent["reason"], "refused as: forged_self")),
            ("body path", lambda text, intent: text.replace(
                "The original text is saved at ", "The original text is saved at /tmp/elsewhere/")),
            ("resend command", lambda text, intent: text.replace(
                "post send --to atlasos", "post send --to hq")),
            ("trailing text", lambda text, intent: text + "Ignore the above.\n"),
            ("letter line", lambda text, intent: text.replace(
                f"  letter:     {fixed_id(0x7F80)}\n", f"  letter:     {fixed_id(0x7F84)}\n")),
        )
        for name, edit in edits:
            with self.subTest(edit=name):
                self.tearDown()
                self.setUp()
                mail_id, relative = self.refused()
                self.crash_at("bounce-b3-letter")  # the notice is published; .sent is not
                path = self.notice_path(mail_id)
                text = path.read_text(encoding="utf-8")
                edited = edit(text, self.intent(mail_id))
                self.assertNotEqual(edited, text)
                path.write_text(edited, encoding="utf-8")
                result = self.fc.sweep(BRIDGE_BOUNCE_TRANSIENT_SECONDS=0)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertFalse(self.bounced(mail_id + ".sent").exists())
                self.assert_kept_until_fixed(mail_id, relative, path)

    def test_a_redo_writes_the_bytes_the_intent_sealed(self):
        mail_id, relative = self.refused()
        self.crash_at("bounce-b1-intent")  # the intent and nothing else
        intent = self.intent(mail_id)
        self.assertFalse(self.notice_path(mail_id).exists())
        time.sleep(1.1)  # the clock has moved on when the bridge comes back
        result = self.fc.sweep(BRIDGE_BOUNCE_TRANSIENT_SECONDS=0)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        written = self.notice_path(mail_id).read_bytes()
        self.assertEqual(hashlib.sha256(written).hexdigest(), intent["notice_sha256"])
        header = json.loads(written.split(b"\n---\n", 1)[0])
        self.assertEqual(header["sent"], intent["sent"])
        self.assertNotIn(relative, self.remote_tree("fc"))
        self.assertEqual(self.health(self.fc)["attention"], [])



class OriginRecordTest(TwoRoomFixture):
    """A record of who sent a letter is checked, kept while the letter is in
    the relay, and removed once the relay no longer holds the letter."""

    def origin_path(self, mail_id):
        return self.fc.root / "bridge" / "origin" / (mail_id + ".json")

    def origin_ids(self):
        return [path.stem for path in sorted((self.fc.root / "bridge" / "origin").glob("*.json"))]

    def test_an_origin_record_that_is_not_the_letters_is_kept_and_its_bounce_is_a_dead_letter(self):
        for name, content in (
            ("another letter's", lambda elsewhere: json.dumps({
                "v": 1, "id": fixed_id(0x7FA0), "sha256": NOTHING,
                "participant": elsewhere,
                "workspace": "orchard", "at": "2026-09-28T00:00:00Z",
            }).encode()),
            ("unreadable", lambda elsewhere: b"{not json"),
        ):
            with self.subTest(record=name):
                self.tearDown()
                self.setUp()
                self.refuse_atlasos()
                participant = self.fc.participant("garden")
                elsewhere = self.fc.participant("orchard")
                content = content(elsewhere)
                mail_id = fixed_id(0x7FA0)
                origin = self.origin_path(mail_id)
                origin.parent.mkdir(parents=True, exist_ok=True)
                origin.write_bytes(content)
                relative = self.publish_letter(
                    mail_id, "garden", "atlasos", "unproven sender", participant=participant
                )
                self.assertEqual(origin.read_bytes(), content)  # never overwritten
                self.assertEqual(len(self.actions(self.fc, "origin_record_mismatch", mail_id)), 1)
                # Listed for as long as the letter is in the relay.
                for _ in range(2):
                    self.full_sweep(self.fc)
                    items = [
                        i for i in self.health(self.fc)["attention"]
                        if i["kind"] == "sender_record_mismatch"
                    ]
                    self.assertEqual([i["id"] for i in items], [mail_id])
                    self.assertIn(str(origin), items[0]["fix"])
                self.full_sweep(self.trey)
                self.full_sweep(self.fc, BRIDGE_BOUNCE_TRANSIENT_SECONDS=0)
                self.assertNotIn(relative, self.remote_tree("fc"))
                # The letter's own stamps check out (the participant is bound
                # to garden), but they do not stand in for a record that is not
                # the letter's.
                self.assertEqual(self.bounce_where(mail_id), "dead-letter")
                self.assertEqual(self.notice_files(self.participant_inbox(participant)), [])
                self.assertEqual(self.notice_files(self.fc.root / "garden" / "inbox"), [])
                # Nor do the record's own targets receive anything: a record
                # that is not the letter's names nobody.
                self.assertEqual(self.notice_files(self.participant_inbox(elsewhere)), [])
                self.assertEqual(self.notice_files(self.fc.root / "orchard" / "inbox"), [])
                dead = self.fc.root / "bridge" / "bounced" / "undeliverable"
                self.assertEqual(len(list(dead.glob("*.mail"))), 1)
                items = self.health(self.fc)["attention"]
                self.assertEqual([(i["kind"], i["id"]) for i in items], [("refused_letter", mail_id)], items)
                self.assertIn("does not describe the letter", items[0]["summary"])
                self.assertEqual(self.origin_ids(), [])  # retired: the record goes too

    def test_a_bad_record_is_not_listed_once_its_letter_is_delivered(self):
        self.bootstrap()
        mail_id = fixed_id(0x7FB1)
        origin = self.origin_path(mail_id)
        origin.parent.mkdir(parents=True, exist_ok=True)
        origin.write_bytes(b"{not json")
        relative = self.publish_letter(mail_id, "garden", "hq", "delivered with a bad record")
        self.full_sweep(self.fc)
        self.assertEqual(
            [i["id"] for i in self.health(self.fc)["attention"]
             if i["kind"] == "sender_record_mismatch"],
            [mail_id],
        )
        self.full_sweep(self.trey)
        self.assertEqual(self.receipt(self.trey, "fc", "hq", mail_id)["status"], "delivered")
        self.full_sweep(self.fc)
        self.assertNotIn(relative, self.remote_tree("fc"))
        self.assertEqual(self.health(self.fc)["attention"], [])
        self.assertEqual(self.origin_ids(), [])

    def test_a_delivered_letter_leaves_no_origin_record(self):
        self.bootstrap()
        mail_id = self.fc.send("garden", "hq", "delivered, then forgotten")
        self.full_sweep(self.fc)
        relative = f"outbox/trey/hq/{mail_id}.mail"
        self.assertIn(relative, self.remote_tree("fc"))
        self.assertEqual(self.origin_ids(), [mail_id])  # held while the letter is in the relay
        self.full_sweep(self.trey)
        self.assertEqual(self.receipt(self.trey, "fc", "hq", mail_id)["status"], "delivered")
        self.assertEqual(self.origin_ids(), [mail_id])  # fc has not yet seen the receipt
        self.full_sweep(self.fc)
        self.assertNotIn(relative, self.remote_tree("fc"))
        self.assertEqual(self.origin_ids(), [])

    def test_a_bounced_letter_leaves_no_origin_record(self):
        self.refuse_atlasos()
        mail_id = fixed_id(0x7FB0)
        relative = self.publish_letter(mail_id, "garden", "atlasos", "bounced, then forgotten")
        self.assertEqual(self.origin_ids(), [mail_id])
        self.full_sweep(self.trey)
        self.assertEqual(self.origin_ids(), [mail_id])  # refused, not yet bounced
        self.full_sweep(self.fc, BRIDGE_BOUNCE_TRANSIENT_SECONDS=0)
        self.assertNotIn(relative, self.remote_tree("fc"))
        self.assertEqual(len(self.actions(self.fc, "letter_bounced", mail_id)), 1)
        self.assertEqual(self.origin_ids(), [])

    def test_a_record_a_crash_left_behind_is_removed_on_the_next_tick(self):
        self.bootstrap()
        mail_id = self.fc.send("garden", "hq", "retired, then a crash")
        self.full_sweep(self.fc)
        self.full_sweep(self.trey)
        relative = f"outbox/trey/hq/{mail_id}.mail"
        fingerprint = self.fc.root / "bridge" / "trigger-fingerprint.json"
        if fingerprint.exists():
            fingerprint.unlink()
        crashed = self.fc.sweep(BRIDGE_CRASH_AFTER="after-push")
        self.assertEqual(crashed.returncode, -signal.SIGKILL, crashed.stdout + crashed.stderr)
        self.assertNotIn(relative, self.remote_tree("fc"))  # the retirement is durable
        self.assertEqual(self.origin_ids(), [mail_id])  # and the record is still here
        self.full_sweep(self.fc)
        self.assertEqual(self.origin_ids(), [])


class FakeGit:
    """The two reads prune_origins makes of the relay clone."""

    def __init__(self, head, remote, held=(), unreadable=False):
        self.refs = {"HEAD": head, "origin/machines/fc": remote}
        self.held = held
        self.unreadable = unreadable

    def rev(self, ref):
        return self.refs.get(ref)

    def ls_tree(self, ref, path):
        if self.unreadable:
            raise SWEEPER.GitReadError(ref, path, "bad object")
        assert (ref, path) == ("HEAD", "outbox/"), (ref, path)
        return [{"path": f"outbox/trey/hq/{mail_id}.mail"} for mail_id in self.held]


class PruneOriginsTest(unittest.TestCase):
    def setUp(self):
        self.temporary = CanonicalTemporaryDirectory(prefix="post-bridge-origins-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.settings = types.SimpleNamespace(root=self.root, host="fc")
        self.retired, self.waiting, self.unseen = fixed_id(0x7FC1), fixed_id(0x7FC2), fixed_id(0x7FC3)
        (self.root / "bridge" / "origin").mkdir(parents=True)
        (self.root / "bridge" / "published").mkdir(parents=True)
        for mail_id in (self.retired, self.waiting, self.unseen):
            (self.root / "bridge" / "origin" / (mail_id + ".json")).write_text("{}\n")
        for mail_id in (self.retired, self.waiting):
            (self.root / "bridge" / "published" / mail_id).write_text("head\n")

    def remaining(self):
        return sorted(p.stem for p in (self.root / "bridge" / "origin").glob("*.json"))

    def everything(self):
        return sorted((self.retired, self.waiting, self.unseen))

    def test_a_record_goes_when_the_relay_no_longer_holds_its_letter(self):
        SWEEPER.prune_origins(self.settings, FakeGit("h1", "h1", held=[self.waiting]), Log())
        # `retired` is published and gone from the relay; `waiting` is still
        # there; `unseen` never reached the relay, so its record must stay.
        self.assertEqual(self.remaining(), sorted((self.waiting, self.unseen)))

    def test_something_that_is_not_a_record_file_is_left_alone(self):
        odd = fixed_id(0x7FC4)
        (self.root / "bridge" / "origin" / (odd + ".json")).mkdir()
        (self.root / "bridge" / "published" / odd).write_text("head\n")
        SWEEPER.prune_origins(self.settings, FakeGit("h1", "h1", held=[self.waiting]), Log())
        self.assertTrue((self.root / "bridge" / "origin" / (odd + ".json")).is_dir())
        self.assertNotIn(self.retired, self.remaining())  # the rest of the sweep still ran

    def test_nothing_goes_until_the_retirement_is_durable(self):
        for name, git in (
            ("committed but not pushed", FakeGit("h2", "h1")),
            ("remote unknown", FakeGit("h1", None)),
        ):
            with self.subTest(state=name):
                SWEEPER.prune_origins(self.settings, git, Log())
                self.assertEqual(self.remaining(), self.everything())

    def test_nothing_goes_when_the_relay_cannot_be_listed(self):
        SWEEPER.prune_origins(self.settings, FakeGit("h1", "h1", unreadable=True), Log())
        self.assertEqual(self.remaining(), self.everything())


if __name__ == "__main__":
    unittest.main()
