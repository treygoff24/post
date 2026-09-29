#!/usr/bin/env python3
"""Terminal refusals, the attention list, and the tick's log and stdout diet.

Before these behaviours a letter the receiver refused for good sat in the
sender's relay outbox forever, in the sender's *and* the receiver's
health.json as nothing at all, and the sending agent was never told. The
tests here drive the real sweep.py against real Post roots and a real bare
relay, in the style of test_sweep.py:

* the bounce: a terminal refusal receipt writes a system letter the sending
  participant can read with `post inbox`/`post read`, with the exact re-send
  command, and retires the outbox entry, idempotently and across crashes;
* health.json `attention`, while `ok` stays liveness;
* routing before validating `from`, so local-only letters are not flagged;
* the denied-channel-name / room separation, `room_retired` dedupe, no quiet
  log line and no duplicate stdout copy of log lines.
"""

import json
import os
import shlex
import signal
import subprocess
import unittest
from pathlib import Path

from .test_sweep import (
    POST,
    SWEEPER,
    CanonicalTemporaryDirectory,
    Topology,
    bind_participant,
    craft_mail,
    fixed_id,
)

FORGED_SELF = SWEEPER.FORGED_SELF
FORGED_BODY = b"forged body text\n"


class TerminalFixture(unittest.TestCase):
    """Three hosts, as SweeperTest: fc (garden), trey (hq, atlasos), mac (porch)."""

    @classmethod
    def setUpClass(cls):
        version = subprocess.run(
            [POST, "--version"], capture_output=True, text=True, check=False
        )
        if version.returncode != 0 or not version.stdout.startswith("post 0.9.0"):
            raise RuntimeError(f"tests require post 0.9.0; got {version.stdout!r}")

    def setUp(self):
        self.temporary = CanonicalTemporaryDirectory(prefix="post-bridge-terminal-")
        self.topology = Topology(self.temporary.name)
        self.fc = self.topology.add("fc", ["garden"])
        self.trey = self.topology.add("trey", ["hq", "atlasos"])
        self.mac = self.topology.add("mac", ["porch"])
        self.topology.finalize()

    def tearDown(self):
        self.temporary.cleanup()

    def bootstrap(self, rounds=2, machines=None):
        for _ in range(rounds):
            for machine in self.topology.machines if machines is None else machines:
                result = machine.sweep()
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def full_sweep(self, machine, **env):
        """One full tick: the quiet fingerprint is dropped first."""
        fingerprint = machine.root / "bridge" / "trigger-fingerprint.json"
        if fingerprint.exists():
            fingerprint.unlink()
        result = machine.sweep(**env)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(self.health(machine)["quiet"])
        return result

    def health(self, machine):
        return json.loads(
            (machine.root / "bridge" / "health.json").read_text(encoding="utf-8")
        )

    def actions(self, machine, action, mail_id=None):
        return [
            record
            for record in machine.logs()
            if record["action"] == action
            and (mail_id is None or record.get("id") == mail_id)
        ]

    def remote_tree(self, host):
        """Paths on the forge's copy of `machines/<host>`: what a peer sees."""
        result = subprocess.run(
            [
                "git",
                "--git-dir",
                str(self.topology.forge),
                "ls-tree",
                "-r",
                "--name-only",
                f"refs/heads/machines/{host}",
            ],
            capture_output=True,
            text=True,
            check=True,
        )
        return result.stdout.splitlines()

    def receipt(self, machine, sender_host, room, mail_id):
        path = machine.repo / "receipts" / sender_host / room / (mail_id + ".json")
        return json.loads(path.read_text()) if path.exists() else None

    def stick(self, mail_id, sender, recipient, subject, participant=None, host="trey"):
        """A letter fc's outbox holds for `host`, as a peer's bridge left it."""
        fields = {"subject": subject}
        if participant is not None:
            fields["from_participant"] = participant
        relative = f"outbox/{host}/{recipient}/{mail_id}.mail"
        self.fc.inject(
            relative, craft_mail(mail_id, sender, recipient, body=FORGED_BODY, **fields)
        )
        return relative

    def unread(self, machine, room):
        """What the room's participant sees. post resolves the participant
        from the working directory, so this runs in the room's workspace.
        Bridge-delivered mail is `pending` until reader activity routes it;
        `participant touch` is that activity (a live session's bind or watch
        does the same), as in Machine.inbox_ids."""
        machine.post("participant", "touch", as_room=room)
        result = machine.post("inbox", "--json", cwd=machine.workspaces[room])
        return json.loads(result.stdout)["unread"]

    def undeliverable(self, machine, room, subject=None):
        return [
            item
            for item in self.unread(machine, room)
            if item["subject"].startswith("Undeliverable: ")
            and (subject is None or item["subject"] == "Undeliverable: " + subject)
        ]

    def notice_files(self, directory, subject=None):
        """The Undeliverable letters in one inbox directory, read or not."""
        found = []
        for path in sorted(Path(directory).glob("*.mail")):
            header = json.loads(path.read_bytes().split(b"\n---\n", 1)[0])
            if header["subject"].startswith("Undeliverable: ") and (
                subject is None or header["subject"] == "Undeliverable: " + subject
            ):
                found.append(path)
        return found

    def shown(self, machine, room, mail_id):
        result = machine.post("read", mail_id, "--json", cwd=machine.workspaces[room])
        return json.loads(result.stdout)


class BounceTest(TerminalFixture):
    def test_a_refusal_tells_the_sending_participant_and_retires_the_entry(self):
        # The three live stuck Mac->devbox letters are this shape: a workspace
        # letter whose `from` names a room the receiver already owns
        # (forged_self), stamped with the sending participant.
        self.bootstrap()
        participant = self.fc.participant("garden")
        mail_id = fixed_id(0x7A01)
        relative = self.stick(
            mail_id, "hq", "atlasos", "forged hello", participant=participant
        )
        self.full_sweep(self.trey)
        refusal = self.receipt(self.trey, "fc", "atlasos", mail_id)
        self.assertEqual((refusal["status"], refusal["reason"]), ("quarantined", FORGED_SELF))
        self.assertIn(relative, self.remote_tree("fc"))

        # The receiver lists it, with a fix, and stays alive (ok is liveness).
        health = self.health(self.trey)
        self.assertTrue(health["ok"], health)
        standing = [i for i in health["attention"] if i["id"] == mail_id]
        self.assertEqual(len(standing), 1, health["attention"])
        self.assertEqual(set(standing[0]), {"kind", "id", "summary", "fix"})
        self.assertEqual(standing[0]["kind"], "quarantined_inbound")
        self.assertIn("bridge", standing[0]["fix"])

        # The first tick on the sender after it learns of the refusal.
        self.full_sweep(self.fc)
        self.assertNotIn(relative, self.remote_tree("fc"))
        self.assertEqual(self.actions(self.fc, "letter_bounced", mail_id)[0]["where"],
                         "participant:" + participant)
        self.assertEqual(len(self.actions(self.fc, "outbox_bounced", mail_id)), 1)
        self.assertEqual(self.health(self.fc)["attention"], [])

        # The notice reads with the ordinary commands.
        notices = self.undeliverable(self.fc, "garden", "forged hello")
        self.assertEqual(len(notices), 1, self.unread(self.fc, "garden"))
        notice = self.shown(self.fc, "garden", notices[0]["id"])
        self.assertEqual(notice["envelope"]["subject"], "Undeliverable: forged hello")
        self.assertEqual(notice["envelope"]["from"], "post-bridge")
        text = notice["body"]
        for expected in (mail_id, "atlasos", "trey", FORGED_SELF):
            self.assertIn(expected, text)
        commands = [
            line.strip() for line in text.splitlines() if line.strip().startswith("post send ")
        ]
        self.assertEqual(len(commands), 1, text)

        # Every later tick, and a crash-free rerun, changes nothing.
        inbox = self.fc.root / "participants" / participant / "inbox"
        for _ in range(3):
            self.full_sweep(self.fc)
        self.assertEqual(len(self.notice_files(inbox)), 1)
        self.assertEqual(len(self.actions(self.fc, "letter_bounced", mail_id)), 1)
        self.assertEqual(len(self.actions(self.fc, "outbox_bounced", mail_id)), 1)

        # The receiver's list clears once the sender withdrew the letter.
        self.full_sweep(self.trey)
        self.assertEqual(self.health(self.trey)["attention"], [])

        # The command in the notice is exact: pasted as written from the
        # workspace it re-sends the original text, and it is delivered.
        tokens = shlex.split(commands[0])
        self.assertEqual(tokens[:2], ["post", "send"])
        resent = self.fc.post(*tokens[1:], "--json", cwd=self.fc.workspaces["garden"])
        new_id = json.loads(resent.stdout)["envelope"]["id"]
        self.assertNotEqual(new_id, mail_id)
        self.bootstrap(rounds=2, machines=[self.fc, self.trey])
        self.assertIn(new_id, self.trey.inbox_ids("atlasos"))
        self.assertEqual(self.trey.read("atlasos", new_id)["body"], FORGED_BODY.decode())
        self.assertNotIn(mail_id, self.trey.inbox_ids("atlasos"))

    def test_the_notice_falls_back_to_the_sending_room_inbox(self):
        # No participant stamp: the sending room's own inbox is next.
        self.bootstrap()
        mail_id = fixed_id(0x7A02)
        self.stick(mail_id, "garden", "nowhere", "no such room")
        self.full_sweep(self.trey)
        refusal = self.receipt(self.trey, "fc", "nowhere", mail_id)
        self.assertEqual((refusal["status"], refusal["reason"]), ("quarantined", "unknown_room"))
        self.full_sweep(self.fc, BRIDGE_BOUNCE_TRANSIENT_SECONDS=0)
        self.assertEqual(
            self.actions(self.fc, "letter_bounced", mail_id)[0]["where"], "room:garden"
        )
        self.assertIn(
            "Undeliverable: no such room",
            [item["subject"] for item in self.unread(self.fc, "garden")],
        )
        self.assertEqual(self.health(self.fc)["attention"], [])

    def test_an_inactive_participants_notice_goes_to_its_workspace_inbox(self):
        # The three live stuck letters were sent by sessions that ended days
        # ago, and their `from` names a peer's room, which is a placeholder
        # here. Nobody reads an inactive participant's inbox, so the notice
        # goes where the next session in that workspace looks.
        self.bootstrap()
        participant = self.fc.participant("garden")
        own_inbox = self.fc.root / "participants" / participant / "inbox"
        workspace_inbox = self.fc.root / "garden" / "inbox"
        record_path = self.fc.root / "participants" / participant / "participant.json"

        ended, lapsed = fixed_id(0x7A05), fixed_id(0x7A06)
        self.stick(ended, "hq", "atlasos", "sent by an ended session", participant=participant)
        self.full_sweep(self.trey)
        self.fc.post("participant", "end", as_room="garden")
        self.assertIn("ended_at", json.loads(record_path.read_text()))
        self.full_sweep(self.fc)

        self.assertEqual(
            self.actions(self.fc, "letter_bounced", ended)[0]["where"], "room:garden"
        )
        self.assertEqual(self.notice_files(own_inbox), [])
        self.assertEqual(len(self.notice_files(workspace_inbox)), 1)
        # A new session in that workspace finds it with the ordinary commands.
        # (The ended participant itself sees no mail until its conversation
        # resumes, which is why the notice is not left in its own inbox.)
        self.fc.participants["garden"] = bind_participant(
            POST, self.fc.root, self.fc.workspaces["garden"]
        )
        notices = self.undeliverable(self.fc, "garden", "sent by an ended session")
        self.assertEqual(len(notices), 1, self.unread(self.fc, "garden"))
        self.assertIn(ended, self.shown(self.fc, "garden", notices[0]["id"])["body"])

        # A lease that ran out without an explicit end is inactive too: turn
        # the ended record into one that was simply last seen weeks ago.
        self.stick(lapsed, "hq", "atlasos", "sent by a lapsed session", participant=participant)
        self.full_sweep(self.trey)
        record = json.loads(record_path.read_text())
        record.pop("ended_at", None)
        record["last_seen"] = "2026-09-01T00:00:00Z"
        record_path.write_text(json.dumps(record))
        self.full_sweep(self.fc)
        self.assertEqual(
            self.actions(self.fc, "letter_bounced", lapsed)[0]["where"], "room:garden"
        )
        self.assertEqual(self.notice_files(own_inbox), [])
        self.assertEqual(self.health(self.fc)["attention"], [])

    def test_a_notice_with_nobody_to_read_it_is_a_dead_letter_the_health_names(self):
        # `from` is neither a room here nor a participant: the notice is kept
        # where the operator can find it, and health points at it.
        self.bootstrap()
        mail_id = fixed_id(0x7A03)
        relative = self.stick(mail_id, "ghost", "hq", "from nobody")
        self.full_sweep(self.trey)
        refusal = self.receipt(self.trey, "fc", "hq", mail_id)
        self.assertEqual(refusal["status"], "quarantined", refusal)
        self.full_sweep(self.fc, BRIDGE_BOUNCE_TRANSIENT_SECONDS=0)
        self.assertNotIn(relative, self.remote_tree("fc"))
        dead = self.fc.root / "bridge" / "bounced" / "undeliverable"
        letters = sorted(dead.glob("*.mail"))
        self.assertEqual(len(letters), 1)
        self.assertIn(mail_id, letters[0].read_text())
        items = self.health(self.fc)["attention"]
        self.assertEqual([i["kind"] for i in items], ["refused_letter"], items)
        self.assertIn(str(letters[0]), items[0]["fix"])
        # It stays listed until the operator deletes the file; the fix is exact.
        self.full_sweep(self.fc)
        self.assertEqual(len(self.health(self.fc)["attention"]), 1)
        subprocess.run(items[0]["fix"].split(" then delete the notice with: ")[-1], shell=True, check=True)
        self.full_sweep(self.fc)
        self.assertEqual(self.health(self.fc)["attention"], [])

    def test_a_refusal_the_receiver_may_still_clear_waits_before_it_bounces(self):
        # unknown_room clears when the receiver registers the room, so the
        # sender does not give up on it at once.
        self.bootstrap()
        mail_id = fixed_id(0x7A04)
        relative = self.stick(mail_id, "garden", "nowhere", "wait for me")
        self.full_sweep(self.trey)
        self.full_sweep(self.fc)
        self.assertIn(relative, self.remote_tree("fc"))
        self.assertEqual(self.actions(self.fc, "letter_bounced"), [])
        self.assertEqual(self.undeliverable(self.fc, "garden"), [])
        # An hour of standing refusal (here: a zero-second grace) ends the wait.
        self.full_sweep(self.fc, BRIDGE_BOUNCE_TRANSIENT_SECONDS=0)
        self.assertNotIn(relative, self.remote_tree("fc"))
        self.assertEqual(len(self.actions(self.fc, "letter_bounced", mail_id)), 1)

    def test_a_crash_at_any_bounce_step_never_skips_or_doubles_the_notice(self):
        self.bootstrap()
        participant = self.fc.participant("garden")
        hooks = (
            "bounce-b1-intent",
            "bounce-b2-body",
            "bounce-b3-letter",
            "bounce-b4-sent",
            "outbound-o6-bounced",
        )
        for index, hook in enumerate(hooks):
            with self.subTest(hook=hook):
                mail_id = fixed_id(0x7B00 + index)
                subject = f"crash at {hook}"
                relative = self.stick(
                    mail_id, "hq", "atlasos", subject, participant=participant
                )
                self.full_sweep(self.trey)
                crashed = self.fc.sweep(BRIDGE_CRASH_AFTER=hook)
                self.assertEqual(
                    crashed.returncode, -signal.SIGKILL, crashed.stdout + crashed.stderr
                )
                self.assertIn(relative, self.remote_tree("fc"))  # nothing pushed yet
                recovered = self.fc.sweep()
                self.assertEqual(recovered.returncode, 0, recovered.stdout + recovered.stderr)
                self.assertNotIn(relative, self.remote_tree("fc"))
                inbox = self.fc.root / "participants" / participant / "inbox"
                self.assertEqual(len(self.notice_files(inbox, subject)), 1, hook)
                intent = json.loads(
                    (self.fc.root / "bridge" / "bounced" / (mail_id + ".json")).read_text()
                )
                notice_path = (
                    self.fc.root / "participants" / participant / "inbox"
                    / (intent["letter_id"] + ".mail")
                )
                self.assertTrue(notice_path.is_file(), hook)
                self.full_sweep(self.fc)
                self.assertEqual(len(self.notice_files(inbox, subject)), 1, hook)
                self.assertEqual(self.health(self.fc)["attention"], [], hook)
                # ...and it reads with the ordinary command.
                body = self.shown(self.fc, "garden", intent["letter_id"])["body"]
                self.assertIn(mail_id, body)

    def test_a_health_file_from_before_attention_forces_a_full_tick(self):
        # The stuck letters exist before this bridge is deployed and nothing
        # about them changes, so the first tick after the deploy would go
        # quiet and never look. A prior health without `attention` cannot
        # vouch for the new bridge.
        self.bootstrap()
        for _ in range(8):
            result = self.fc.sweep()
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            if self.health(self.fc)["quiet"]:
                break
        else:
            self.fail("fc never reached a quiet tick")
        path = self.fc.root / "bridge" / "health.json"
        value = json.loads(path.read_text())
        value.pop("attention", None)
        path.write_text(json.dumps(value) + "\n")
        result = self.fc.sweep()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        health = self.health(self.fc)
        self.assertFalse(health["quiet"])
        self.assertEqual(health["attention"], [])


class AttentionTest(TerminalFixture):
    def test_a_standing_quarantine_stays_on_the_list_through_quiet_ticks(self):
        self.bootstrap()
        mail_id = fixed_id(0x7C01)
        self.stick(mail_id, "hq", "atlasos", "still stuck")
        self.full_sweep(self.trey)
        for _ in range(8):
            result = self.trey.sweep()
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            if self.health(self.trey)["quiet"]:
                break
        else:
            self.fail("trey never reached a quiet tick")
        health = self.health(self.trey)
        self.assertTrue(health["ok"])
        self.assertEqual([i["id"] for i in health["attention"]], [mail_id])

    def test_an_unrelayable_archive_letter_is_listed_with_an_exact_fix(self):
        self.bootstrap()
        archive = self.fc.root / "archive"
        archive.mkdir(exist_ok=True)
        mail_id = fixed_id(0x7C02)
        path = archive / (mail_id + ".mail")
        # An invalid kind to a peer's room: the bridge can never relay it.
        path.write_bytes(craft_mail(mail_id, "garden", "hq", kind="bogus"))
        self.full_sweep(self.fc)
        health = self.health(self.fc)
        self.assertTrue(health["ok"], health)
        items = [i for i in health["attention"] if i["kind"] == "unrelayable_letter"]
        self.assertEqual([i["id"] for i in items], [mail_id], health["attention"])
        self.assertIn("invalid_kind", items[0]["summary"])
        # The fix works as written and the item goes with the file.
        subprocess.run(items[0]["fix"].split("leaves the archive) with: ")[1], shell=True, check=True)
        self.assertFalse(path.exists())
        self.full_sweep(self.fc)
        self.assertEqual(self.health(self.fc)["attention"], [])


class CollisionAttentionTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        TerminalFixture.setUpClass()

    def test_a_room_name_collision_is_listed_with_the_rename_command(self):
        with CanonicalTemporaryDirectory(prefix="post-bridge-collision-") as temporary:
            topology = Topology(temporary)
            topology.add("fc", ["garden"])
            trey = topology.add("trey", ["hq", "atlasos"])
            topology.add("mac", ["porch", "atlasos"])
            topology.finalize()
            # The contest comes from published rooms.json, as it does live; a
            # config pin for a name that is also a registered room is fatal.
            for machine in topology.machines:
                config_path = machine.root / "bridge" / "config.json"
                config = json.loads(config_path.read_text())
                config["peers"] = {
                    host: [room for room in names if room != "atlasos"]
                    for host, names in config["peers"].items()
                }
                config_path.write_text(json.dumps(config, sort_keys=True) + "\n")
            for _ in range(2):
                for machine in topology.machines:
                    machine.sweep()
            health = json.loads((trey.root / "bridge" / "health.json").read_text())
            items = [i for i in health["attention"] if i["kind"] == "name_collision"]
            self.assertEqual([i["id"] for i in items], ["atlasos"], health["attention"])
            self.assertIn("post rooms rename atlasos", items[0]["fix"])


class RoutingBeforeValidationTest(TerminalFixture):
    def test_a_local_only_letter_with_a_free_form_sender_is_not_flagged(self):
        # A probe letter to a room on this host never needed relaying, so its
        # free-form `from` is nobody's business. The same letter addressed to
        # a peer's room is still flagged: it would leave this host.
        self.bootstrap()
        archive = self.fc.root / "archive"
        archive.mkdir(exist_ok=True)
        local_id, peer_id = fixed_id(0x7D01), fixed_id(0x7D02)
        (archive / (local_id + ".mail")).write_bytes(
            craft_mail(local_id, "probe/free form", "garden")
        )
        (archive / (peer_id + ".mail")).write_bytes(
            craft_mail(peer_id, "probe/free form", "hq")
        )
        for _ in range(2):
            self.full_sweep(self.fc)
        self.assertEqual(self.actions(self.fc, "outbound_ignored", local_id), [])
        self.assertEqual(len(self.actions(self.fc, "outbound_ignored", peer_id)), 1)
        listed = [i["id"] for i in self.health(self.fc)["attention"]]
        self.assertEqual(listed, [peer_id])


class LogDietTest(TerminalFixture):
    def publish_rooms(self, machine, names):
        machine.inject(
            "rooms.json",
            (
                json.dumps({"v": 1, "host": machine.host, "rooms": sorted(names)}, sort_keys=True)
                + "\n"
            ).encode(),
        )

    def set_channels(self, machine, deny):
        path = machine.root / "bridge" / "config.json"
        config = json.loads(path.read_text())
        config["channels"] = {"mode": "all", "deny": deny}
        path.write_text(json.dumps(config, sort_keys=True) + "\n")

    def test_a_denied_channel_name_does_not_unpublish_a_same_named_room(self):
        # config.channels.deny names channels. It used to double as a room
        # deny list, so denying the channel `hq` silently unpublished trey's
        # room `hq`, and every peer then logged that room as retired.
        self.bootstrap()
        published = json.loads(
            subprocess.run(
                ["git", "--git-dir", str(self.topology.forge), "show",
                 "refs/heads/machines/trey:rooms.json"],
                capture_output=True, text=True, check=True,
            ).stdout
        )
        self.assertIn("hq", published["rooms"])  # precondition
        self.set_channels(self.trey, ["hq"])
        self.full_sweep(self.trey)
        published = json.loads(
            subprocess.run(
                ["git", "--git-dir", str(self.topology.forge), "show",
                 "refs/heads/machines/trey:rooms.json"],
                capture_output=True, text=True, check=True,
            ).stdout
        )
        self.assertEqual(sorted(published["rooms"]), ["atlasos", "hq"])
        self.full_sweep(self.fc)
        self.assertEqual(
            [r for r in self.fc.logs() if r["action"] == "room_retired"], []
        )

    def test_room_retired_is_logged_once_while_the_room_stays_retired(self):
        # Derived (published, not pinned) rooms are the ones that retire.
        path = self.fc.root / "bridge" / "config.json"
        config = json.loads(path.read_text())
        config["peers"] = {host: [] for host in config["peers"]}
        path.write_text(json.dumps(config, sort_keys=True) + "\n")
        self.bootstrap()
        self.publish_rooms(self.trey, ["hq"])  # atlasos leaves trey's table
        for _ in range(3):
            self.full_sweep(self.fc)
        retired = [
            r for r in self.fc.logs()
            if r["action"] == "room_retired" and r.get("room") == "atlasos"
        ]
        self.assertEqual(len(retired), 1, retired)

    def test_a_quiet_tick_logs_nothing_and_no_tick_copies_its_log_to_stdout(self):
        first = self.fc.sweep()
        self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
        self.assertTrue(self.fc.logs())  # the tick logged
        self.assertEqual(first.stdout, "")  # log.jsonl is the one sink
        for _ in range(8):
            result = self.fc.sweep()
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual(result.stdout, "")
            if self.health(self.fc)["quiet"]:
                break
        else:
            self.fail("fc never reached a quiet tick")
        lines = (self.fc.root / "bridge" / "log.jsonl").read_text().splitlines()
        for _ in range(2):
            result = self.fc.sweep()
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual(result.stdout, "")
        self.assertTrue(self.health(self.fc)["quiet"])
        self.assertEqual((self.fc.root / "bridge" / "log.jsonl").read_text().splitlines(), lines)
        self.assertNotIn("quiet", [r["action"] for r in self.fc.logs()])


if __name__ == "__main__":
    unittest.main()
