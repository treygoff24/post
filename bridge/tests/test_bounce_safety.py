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

    def test_a_room_removed_after_delivery_does_not_take_the_delivery_back(self):
        mail_id, relative = self.deliver_one()
        self.remove_room(self.trey, "hq")
        self.full_sweep(self.trey)
        self.assert_stays_delivered(mail_id, relative)

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

    def test_an_origin_recorded_for_another_letter_is_ignored(self):
        self.refuse_atlasos()
        participant = self.fc.participant("garden")
        elsewhere = self.fc.participant("orchard")
        mail_id = fixed_id(0x7F04)
        self.publish_letter(mail_id, "garden", "atlasos", "mismatched", participant=participant)
        self.full_sweep(self.trey)
        path = self.fc.root / "bridge" / "origin" / (mail_id + ".json")
        record = json.loads(path.read_text())
        record.update(sha256=NOTHING, participant=elsewhere, workspace="orchard")
        path.write_text(json.dumps(record))
        self.full_sweep(self.fc, BRIDGE_BOUNCE_TRANSIENT_SECONDS=0)
        # Not the record's participant and workspace; the letter's own stamps
        # (which still check out) decide, as for a letter with no record.
        self.assertEqual(self.bounce_where(mail_id), "participant:" + participant)
        self.assertEqual(self.notice_files(self.participant_inbox(elsewhere)), [])

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
        self.assertEqual([(i["kind"], i["id"]) for i in items],
                         [("refused_letter", letters[0].stem)], items)
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
        self.assertIn("does not match", items[0]["summary"])
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
        path.write_bytes(stranger)
        result = self.fc.sweep(BRIDGE_BOUNCE_TRANSIENT_SECONDS=0)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(path.read_bytes(), stranger)  # not overwritten, not adopted
        self.assertFalse(self.bounced(mail_id + ".sent").exists())
        self.assert_kept_until_fixed(mail_id, relative, path)

    def test_an_intent_written_for_another_letter_stops_the_retirement(self):
        for key, value in (("id", fixed_id(0x7F81)), ("sha256", NOTHING), ("room", "hq")):
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

    def test_a_notice_that_names_another_letter_stops_the_retirement(self):
        mail_id, relative = self.refused()
        self.crash_at("bounce-b3-letter")
        path = self.notice_path(mail_id)
        text = path.read_text(encoding="utf-8")
        self.assertIn(f"  letter:     {mail_id}\n", text)
        path.write_text(
            text.replace(f"  letter:     {mail_id}\n", f"  letter:     {fixed_id(0x7F84)}\n"),
            encoding="utf-8",
        )
        result = self.fc.sweep(BRIDGE_BOUNCE_TRANSIENT_SECONDS=0)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(self.bounced(mail_id + ".sent").exists())
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


if __name__ == "__main__":
    unittest.main()
