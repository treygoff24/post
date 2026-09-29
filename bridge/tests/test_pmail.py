"""F3 bridge side: participant mail across hosts (SPEC-v2 r6.0).

Design: bridge-participant-address-design rev 3.1, §Tests -> Bridge. The
three-root harness is test_sweep's (fc [garden], trey [hq, atlasos],
mac [porch]).

The F3 post under test (0612a18) has `post bridge deliver` but no sender
surface yet, so every sending-side letter here is written straight into the
sender's archive in the envelope shape the design fixes: `to` = the
participant id, `address_kind` = participant, and `to_host`. Everything after
the archive (select, pmail, deliver, receipts, acked) is the real bridge
calling the real post binary named by POST_BIN.
"""

import hashlib
import json
import os
import re
import signal
import subprocess
import sys
import textwrap
import time
import types
import unittest
from pathlib import Path

from .test_sweep import (
    PINNED_POST_VERSION,
    POST,
    SWEEPER,
    CanonicalTemporaryDirectory,
    Topology,
    craft_mail,
    fixed_id,
    post_env,
    run,
)

pmail = SWEEPER.pmail  # the bridgelib.pmail module the sweeper runs
HERE = Path(__file__).resolve().parent
FIXTURES = HERE / "fixtures"
PRE_F3_POST = os.environ.get(
    "PRE_F3_POST_BIN", str(Path.home() / ".local" / "bin" / "post")
)
PRE_F3_BRIDGE_COMMIT = "af00a1a"


def has_deliver(binary):
    try:
        result = subprocess.run(
            [binary, "bridge", "deliver", "--help"],
            capture_output=True,
            timeout=30,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return False
    return result.returncode == 0


POST_HAS_DELIVER = has_deliver(POST)
needs_deliver = unittest.skipUnless(
    POST_HAS_DELIVER,
    f"POST_BIN={POST} has no `post bridge deliver` (pre-F3 post): "
    "this test needs the F3 import command",
)


def sha(data):
    return hashlib.sha256(data).hexdigest()


def fake_post(directory, real):
    """A POST_BIN wrapper that logs deliver calls and can fake their answer.

    `deliver.log` gets one line per real deliver attempt (not the --help
    probe). When `fake.out` exists, a deliver call prints it and exits with
    the code in `fake.rc` instead of running post: the contract tests feed
    the bridge each malformed answer this way.
    """
    directory.mkdir(parents=True, exist_ok=True)
    script = directory / "post"
    script.write_text(
        textwrap.dedent(
            f"""\
            #!/bin/sh
            if [ "$1" = bridge ] && [ "$2" = deliver ]; then
              case " $* " in
                *" --help "*) printf 'fake help\\n'; exit 0 ;;
              esac
              printf '%s\\n' "$*" >> '{directory}/deliver.log'
              if [ -f '{directory}/fake.out' ]; then
                cat '{directory}/fake.out'
                exit "$(cat '{directory}/fake.rc')"
              fi
            fi
            exec '{real}' "$@"
            """
        ),
        encoding="utf-8",
    )
    script.chmod(0o755)
    return script


class PmailTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        version = run([POST, "--version"], check=False)
        if version.returncode != 0 or not SWEEPER.post_version_accepted(version.stdout):
            raise RuntimeError(
                f"tests require {PINNED_POST_VERSION!r}; got {version.stdout.strip()!r}"
            )

    def setUp(self):
        self.temporary = CanonicalTemporaryDirectory(prefix="post-bridge-f3-")
        self.base = Path(self.temporary.name)
        self.topology = Topology(self.temporary.name)
        self.fc = self.topology.add("fc", ["garden"])
        self.trey = self.topology.add("trey", ["hq", "atlasos"])
        self.mac = self.topology.add("mac", ["porch"])
        self.topology.finalize()
        self.sequence = 0x5000

    def tearDown(self):
        self.temporary.cleanup()

    # -- helpers -------------------------------------------------------------

    def sweep(self, machine, expect=0, full=True, **env):
        """One tick. `full` drops the quiet fingerprint so the tick does work."""
        if full:
            try:
                (machine.root / "bridge" / "trigger-fingerprint.json").unlink()
            except FileNotFoundError:
                pass
        result = machine.sweep(**env)
        if expect is not None:
            self.assertEqual(
                result.returncode, expect, f"{machine.host}: {result.stdout}{result.stderr}"
            )
        return result

    def bootstrap(self, rounds=2):
        for _ in range(rounds):
            for machine in self.topology.machines:
                self.sweep(machine)

    def letter(self, sender, room, dest, participant, body=None, **changes):
        """Write one host-qualified participant letter into `sender`'s archive."""
        self.sequence += 1
        mail_id = fixed_id(self.sequence)
        acting = room if room in sender.workspaces else min(sender.workspaces)
        fields = {
            "address_kind": "participant",
            "to_host": dest.host,
            "from_participant": sender.participant(acting),
        }
        fields.update(changes)
        data = craft_mail(
            mail_id,
            room,
            participant,
            body if body is not None else f"pmail {mail_id}".encode(),
            **fields,
        )
        archive = sender.root / "archive"
        archive.mkdir(parents=True, exist_ok=True)
        (archive / (mail_id + ".mail")).write_bytes(data)
        return mail_id, data

    def show(self, machine, ref, path):
        result = machine.git("show", f"{ref}:{path}", check=False)
        return result.stdout if result.returncode == 0 else None

    def tree(self, machine, ref, prefix):
        result = machine.git("ls-tree", "-r", "--name-only", ref, "--", prefix)
        return [line for line in result.stdout.splitlines() if line]

    def pmail_path(self, dest, participant, mail_id):
        return f"pmail/{dest.host}/{participant}/{mail_id}.mail"

    def preceipt_path(self, origin, participant, mail_id):
        return f"preceipts/{origin.host}/{participant}/{mail_id}.json"

    def receipt_bytes(self, dest, origin, participant, mail_id, ref="HEAD"):
        result = run(
            ["git", "-C", dest.repo, "show",
             f"{ref}:{self.preceipt_path(origin, participant, mail_id)}"],
            check=False,
        )
        return result.stdout.encode() if result.returncode == 0 else None

    def state(self, machine, namespace, mail_id):
        path = machine.root / "bridge" / namespace / (mail_id + ".json")
        return path.read_bytes() if path.exists() else None

    def inbox(self, machine, participant, mail_id):
        path = machine.root / "participants" / participant / "inbox" / (mail_id + ".mail")
        return path.read_bytes() if path.exists() else None

    def admission(self, machine, participant, mail_id):
        path = machine.root / "participants" / participant / "imports" / (mail_id + ".json")
        return json.loads(path.read_text()) if path.exists() else None

    def actions(self, machine, name):
        return [record for record in machine.logs() if record["action"] == name]

    def health(self, machine):
        return json.loads((machine.root / "bridge" / "health.json").read_text())

    def deliver_direct(self, machine, participant, source, mail_id, data):
        """One `post bridge deliver` call outside the bridge (D1/D2 states)."""
        letter = self.base / f"direct-{mail_id}.mail"
        letter.write_bytes(data)
        letter.chmod(0o600)
        result = run(
            [POST, "bridge", "deliver", "--participant", participant,
             "--source-host", source.host, "--mail-id", mail_id,
             "--sha256", sha(data), "--file", letter, "--json"],
            env=post_env(machine.root),
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return json.loads(result.stdout)

    def wrap_post(self, machine):
        directory = self.base / f"fake-post-{machine.host}"
        script = fake_post(directory, POST)
        return directory, str(script)

    def deliver_calls(self, directory):
        path = directory / "deliver.log"
        return path.read_text().splitlines() if path.exists() else []

    def assert_delivered(self, dest, origin, participant, mail_id, data):
        self.assertEqual(self.inbox(dest, participant, mail_id), data)
        record = self.admission(dest, participant, mail_id)
        self.assertEqual(
            (record["source_host"], record["sha256"]), (origin.host, sha(data))
        )
        receipt = self.receipt_bytes(dest, origin, participant, mail_id,
                                     ref=f"origin/machines/{dest.host}")
        self.assertIsNotNone(receipt, "receipt not pushed")
        value = json.loads(receipt)
        self.assertEqual(
            (value["status"], value["reason"], value["at"], value["sha256"]),
            ("delivered", None, record["admitted_at"], sha(data)),
        )
        return receipt

    def assert_acked(self, origin, dest, participant, mail_id, status="delivered"):
        acked = self.state(origin, "pmail-acked", mail_id)
        self.assertIsNotNone(acked, f"{mail_id} not acked on {origin.host}")
        self.assertEqual(json.loads(acked)["status"], status)
        self.assertEqual(
            acked, self.receipt_bytes(dest, origin, participant, mail_id,
                                      ref=f"origin/machines/{dest.host}"),
        )
        self.assertEqual(
            self.tree(origin, f"origin/machines/{origin.host}",
                      self.pmail_path(dest, participant, mail_id)),
            [],
            "pmail entry not pruned and pushed",
        )
        return acked

    # -- round trips ---------------------------------------------------------

    @needs_deliver
    def test_round_trip_each_direction_then_reply(self):
        self.bootstrap()
        fc_pid = self.fc.participant("garden")
        trey_pid = self.trey.participant("hq")

        forward, forward_bytes = self.letter(self.fc, "garden", self.trey, trey_pid)
        self.sweep(self.fc)
        pushed = self.show(self.fc, "origin/machines/fc",
                           self.pmail_path(self.trey, trey_pid, forward))
        self.assertEqual(pushed, forward_bytes.decode())
        marker = json.loads(self.state(self.fc, "pmail-published", forward))
        self.assertEqual(
            (marker["v"], marker["id"], marker["host"], marker["sha256"]),
            (1, forward, "trey", sha(forward_bytes)),
        )
        self.assertEqual(
            marker["commit"], self.fc.git("rev-parse", "origin/machines/fc").stdout.strip()
        )
        self.assertEqual(self.tree(self.fc, "HEAD", "outbox/"), [])
        self.sweep(self.trey)
        self.assert_delivered(self.trey, self.fc, trey_pid, forward, forward_bytes)
        self.sweep(self.fc)
        self.assert_acked(self.fc, self.trey, trey_pid, forward)

        backward, backward_bytes = self.letter(self.trey, "hq", self.fc, fc_pid)
        self.sweep(self.trey)
        self.sweep(self.fc)
        self.assert_delivered(self.fc, self.trey, fc_pid, backward, backward_bytes)
        self.sweep(self.trey)
        self.assert_acked(self.trey, self.fc, fc_pid, backward)

        # The reply address comes from the admission record, the way post's
        # reply_to_participant derives it: participant:<from_participant>@<host>.
        record = self.admission(self.fc, fc_pid, backward)
        reply_to = (record["from_participant"], record["source_host"])
        self.assertEqual(reply_to, (trey_pid, "trey"))
        reply, reply_bytes = self.letter(self.fc, "garden", self.trey, reply_to[0])
        self.sweep(self.fc)
        self.sweep(self.trey)
        self.assert_delivered(self.trey, self.fc, trey_pid, reply, reply_bytes)
        self.sweep(self.fc)
        self.assert_acked(self.fc, self.trey, trey_pid, reply)
        # No participant letter ever took the workspace path.
        for machine in self.topology.machines:
            self.assertEqual(self.tree(machine, "HEAD", "outbox/"), [])

    @needs_deliver
    def test_provenance_keys_arrive_byte_identical(self):
        self.bootstrap()
        garden = self.fc.workspaces["garden"]
        self.fc.post("identity", "new", "fern", cwd=garden)
        self.fc.post("profile", "set", "--name", "Fern", "--pfp", "🌿", cwd=garden)
        # A real post header carries the real provenance stamps; the fixture
        # re-addresses it into the pmail shape post's F3 send will write.
        seed = self.fc.send("garden", "garden", "provenance seed")
        header, _ = (self.fc.root / "archive" / (seed + ".mail")).read_bytes().split(
            b"\n---\n", 1
        )
        stamps = json.loads(header)
        for key in ("from_participant", "from_lineage", "display_name", "pfp",
                    "sender_provenance"):
            self.assertIn(key, stamps)
        trey_pid = self.trey.participant("hq")
        kept = {key: stamps[key] for key in stamps if key not in
                ("id", "from", "to", "kind", "subject", "sent", "address_kind")}
        mail_id, data = self.letter(self.fc, "garden", self.trey, trey_pid, **kept)
        self.sweep(self.fc)
        self.sweep(self.trey)
        self.assert_delivered(self.trey, self.fc, trey_pid, mail_id, data)
        delivered = json.loads(self.inbox(self.trey, trey_pid, mail_id).split(b"\n---\n")[0])
        for key, value in kept.items():
            self.assertEqual(delivered[key], value, key)
        for machine in (self.fc, self.trey):
            self.assertEqual(self.actions(machine, "unknown_envelope_keys"), [])

    # -- crash matrices ------------------------------------------------------

    @needs_deliver
    def test_destination_crash_matrix_converges(self):
        self.bootstrap()
        trey_pid = self.trey.participant("hq")
        directory, wrapper = self.wrap_post(self.trey)

        def staged():
            mail_id, data = self.letter(self.fc, "garden", self.trey, trey_pid)
            self.sweep(self.fc)
            return mail_id, data

        # Nothing written: the tick dies before the import pass.
        mail_id, data = staged()
        self.sweep(self.trey, expect=-signal.SIGKILL,
                   BRIDGE_CRASH_AFTER="before-pmail-import")
        self.assertIsNone(self.admission(self.trey, trey_pid, mail_id))
        self.sweep(self.trey)
        self.assert_delivered(self.trey, self.fc, trey_pid, mail_id, data)

        # D1: admission record only (inbox write lost).
        mail_id, data = staged()
        first = self.deliver_direct(self.trey, trey_pid, self.fc, mail_id, data)
        self.assertEqual(first["outcome"], "delivered")
        (self.trey.root / "participants" / trey_pid / "inbox" / (mail_id + ".mail")).unlink()
        self.sweep(self.trey)
        receipt = self.assert_delivered(self.trey, self.fc, trey_pid, mail_id, data)
        self.assertEqual(json.loads(receipt)["at"], first["admitted_at"])

        # D2: record and inbox file, no receipt.
        mail_id, data = staged()
        first = self.deliver_direct(self.trey, trey_pid, self.fc, mail_id, data)
        self.sweep(self.trey)
        receipt = self.assert_delivered(self.trey, self.fc, trey_pid, mail_id, data)
        self.assertEqual(json.loads(receipt)["at"], first["admitted_at"])

        # D3: receipt committed, not pushed -> pushed as is, post not called.
        mail_id, data = staged()
        self.sweep(self.trey, expect=-signal.SIGKILL, BRIDGE_CRASH_AFTER="after-commit")
        committed = self.receipt_bytes(self.trey, self.fc, trey_pid, mail_id)
        self.assertIsNotNone(committed)
        self.assertIsNone(self.receipt_bytes(self.trey, self.fc, trey_pid, mail_id,
                                             ref="origin/machines/trey"))
        before = len(self.deliver_calls(directory))
        self.sweep(self.trey, POST_BIN=wrapper)
        self.assertEqual(len(self.deliver_calls(directory)), before)
        self.assertEqual(
            self.assert_delivered(self.trey, self.fc, trey_pid, mail_id, data), committed
        )

        # D4: complete. Another tick, and a replayed relay commit, are no-ops.
        self.sweep(self.trey, POST_BIN=wrapper)
        self.assertEqual(len(self.deliver_calls(directory)), before)
        # Every letter converged to one inbox message and one acked record.
        self.sweep(self.fc)
        inbox = sorted((self.trey.root / "participants" / trey_pid / "inbox").iterdir())
        self.assertEqual(len(inbox), 4)
        self.assertEqual(
            len(list((self.fc.root / "bridge" / "pmail-acked").iterdir())), 4
        )

    @needs_deliver
    def test_rebuilt_delivered_receipt_is_byte_identical(self):
        self.bootstrap()
        trey_pid = self.trey.participant("hq")
        mail_id, data = self.letter(self.fc, "garden", self.trey, trey_pid)
        self.sweep(self.fc)
        # Killed after the receipt reached the worktree, before the commit.
        self.sweep(self.trey, expect=-signal.SIGKILL,
                   BRIDGE_CRASH_AFTER="pmail-receipt-written")
        path = self.trey.repo / self.preceipt_path(self.fc, trey_pid, mail_id)
        original = path.read_bytes()
        time.sleep(1.1)  # a rebuilt `at` from the clock would differ
        self.sweep(self.trey)
        discarded = self.actions(self.trey, "pmail_receipt_discarded")
        self.assertEqual(len(discarded), 1)
        rebuilt = self.assert_delivered(self.trey, self.fc, trey_pid, mail_id, data)
        self.assertEqual(rebuilt, original)

    @needs_deliver
    def test_sender_crash_matrix_converges(self):
        self.bootstrap()
        trey_pid = self.trey.participant("hq")
        points = [
            ("after-commit", "fc"),          # S2: committed, not pushed
            ("after-push", "fc"),            # S3: pushed, no marker
            ("pmail-s4-published", "fc"),    # S4
            ("pmail-s5-receipt", "fc2"),     # S5: receipt seen, no acked
            ("pmail-s6-acked", "fc2"),       # S6: acked, pmail present
            ("pmail-s7-pruned", "fc2"),      # S7: pruned, not committed
        ]
        for point, phase in points:
            with self.subTest(point=point):
                mail_id, data = self.letter(self.fc, "garden", self.trey, trey_pid)
                if phase == "fc":
                    self.sweep(self.fc, expect=-signal.SIGKILL, BRIDGE_CRASH_AFTER=point)
                    if point == "after-commit":
                        # A stage that was not pushed is not published.
                        self.assertIsNone(self.state(self.fc, "pmail-published", mail_id))
                    self.sweep(self.fc)
                    self.assertIsNotNone(self.state(self.fc, "pmail-published", mail_id))
                    self.sweep(self.trey)
                    self.sweep(self.fc)
                else:
                    self.sweep(self.fc)
                    self.sweep(self.trey)
                    self.sweep(self.fc, expect=-signal.SIGKILL, BRIDGE_CRASH_AFTER=point)
                    self.sweep(self.fc)
                self.sweep(self.fc)
                self.assert_delivered(self.trey, self.fc, trey_pid, mail_id, data)
                self.assert_acked(self.fc, self.trey, trey_pid, mail_id)

    @needs_deliver
    def test_rejection_rows(self):
        self.bootstrap()
        trey_pid = self.trey.participant("hq")
        rules = self.trey.root / "rules.json"
        block = json.dumps({"blocked": [{"from": "garden", "to": "hq", "reason": "t"}]})
        directory, wrapper = self.wrap_post(self.trey)

        # A decision lost before its commit is re-evaluated.
        lost, lost_bytes = self.letter(self.fc, "garden", self.trey, trey_pid)
        self.sweep(self.fc)
        rules.write_text(block + "\n")
        self.sweep(self.trey, expect=-signal.SIGKILL,
                   BRIDGE_CRASH_AFTER="pmail-receipt-written")
        uncommitted = (self.trey.repo / self.preceipt_path(self.fc, trey_pid, lost))
        self.assertEqual(json.loads(uncommitted.read_bytes())["reason"], "blocked_route")
        rules.write_text('{"blocked":[]}\n')
        self.sweep(self.trey)
        self.assert_delivered(self.trey, self.fc, trey_pid, lost, lost_bytes)

        # A committed rejection is pushed as is; post is never called again,
        # even after the rejecting condition clears.
        final, _ = self.letter(self.fc, "garden", self.trey, trey_pid)
        self.sweep(self.fc)
        rules.write_text(block + "\n")
        self.sweep(self.trey, expect=-signal.SIGKILL, BRIDGE_CRASH_AFTER="after-commit")
        committed = self.receipt_bytes(self.trey, self.fc, trey_pid, final)
        self.assertEqual(json.loads(committed)["reason"], "blocked_route")
        rules.write_text('{"blocked":[]}\n')
        self.sweep(self.trey, POST_BIN=wrapper)
        self.assertEqual(self.deliver_calls(directory), [])
        self.assertEqual(
            self.receipt_bytes(self.trey, self.fc, trey_pid, final,
                               ref="origin/machines/trey"),
            committed,
        )
        self.assertIsNone(self.inbox(self.trey, trey_pid, final))
        self.assertIsNone(self.admission(self.trey, trey_pid, final))
        self.sweep(self.fc)
        self.assert_acked(self.fc, self.trey, trey_pid, final, status="rejected")

    @needs_deliver
    def test_rejection_is_final_through_prune_and_replay(self):
        self.bootstrap()
        trey_pid = self.trey.participant("hq")
        rules = self.trey.root / "rules.json"
        rules.write_text(json.dumps(
            {"blocked": [{"from": "garden", "to": "hq", "reason": "t"}]}) + "\n")
        mail_id, data = self.letter(self.fc, "garden", self.trey, trey_pid)
        self.sweep(self.fc)
        self.sweep(self.trey)
        receipt = self.receipt_bytes(self.trey, self.fc, trey_pid, mail_id)
        self.assertEqual(json.loads(receipt)["reason"], "blocked_route")
        self.sweep(self.fc)
        self.assert_acked(self.fc, self.trey, trey_pid, mail_id, status="rejected")
        # Admission would pass now.
        rules.write_text('{"blocked":[]}\n')
        # Replay the same pmail bytes on the sender's branch.
        self.fc.inject(self.pmail_path(self.trey, trey_pid, mail_id), data)
        directory, wrapper = self.wrap_post(self.trey)
        self.sweep(self.trey, POST_BIN=wrapper)
        self.assertEqual(self.deliver_calls(directory), [])
        self.assertIsNone(self.inbox(self.trey, trey_pid, mail_id))
        self.assertIsNone(self.admission(self.trey, trey_pid, mail_id))
        self.assertEqual(
            self.receipt_bytes(self.trey, self.fc, trey_pid, mail_id,
                               ref="origin/machines/trey"),
            receipt,
        )
        self.assertEqual(
            self.tree(self.trey, "HEAD", f"preceipts/fc/{trey_pid}/"),
            [self.preceipt_path(self.fc, trey_pid, mail_id)],
        )

        # Different bytes under the same path: the first receipt stands and
        # the conflict is logged once and counted.
        self.fc.inject(self.pmail_path(self.trey, trey_pid, mail_id), data + b"changed")
        self.sweep(self.trey, POST_BIN=wrapper)
        self.sweep(self.trey, POST_BIN=wrapper)
        self.assertEqual(self.deliver_calls(directory), [])
        self.assertEqual(len(self.actions(self.trey, "receipt_conflict")), 1)
        self.assertEqual(self.health(self.trey)["pmail"]["receipt_conflicts"], 1)
        self.assertEqual(
            self.receipt_bytes(self.trey, self.fc, trey_pid, mail_id), receipt
        )

    @needs_deliver
    def test_admitted_letter_replays_after_from_changes_owner(self):
        self.bootstrap()
        trey_pid = self.trey.participant("hq")
        admitted, admitted_bytes = self.letter(self.fc, "garden", self.trey, trey_pid)
        control, _ = self.letter(self.fc, "garden", self.trey, trey_pid)
        self.sweep(self.fc)
        # Crash after D1: the record exists, the inbox write was lost.
        first = self.deliver_direct(self.trey, trey_pid, self.fc, admitted, admitted_bytes)
        self.assertEqual(first["outcome"], "delivered")
        (self.trey.root / "participants" / trey_pid / "inbox" / (admitted + ".mail")).unlink()
        # `garden` stops being homed at fc: fc no longer publishes it, mac
        # claims it, and trey's retained fc placeholder is gone (post's
        # `rooms set-path` refuses to touch a placeholder, so the fixture
        # edits the table the way test_sweep's lineage test does). fc keeps
        # its ownership pin, so without mac's claim trey's bridge re-registers
        # the fc placeholder; with it the name is contested (reported as
        # room_name_collision, exit 1) and has no placeholder at all, which
        # is exactly what post's trust fact 2 refuses for a fresh letter.
        self.fc.inject("rooms.json", (json.dumps(
            {"v": 1, "host": "fc", "rooms": []}, sort_keys=True) + "\n").encode())
        self.mac.inject("rooms.json", (json.dumps(
            {"v": 1, "host": "mac", "rooms": ["garden", "porch"]},
            sort_keys=True) + "\n").encode())
        table_path = self.trey.root / "rooms.json"
        table = json.loads(table_path.read_text())
        self.assertIn("/remote/fc/garden", table.pop("garden"))
        table_path.write_text(json.dumps(table, indent=2) + "\n")
        self.sweep(self.trey, expect=1)
        self.assertEqual(self.health(self.trey)["reason"], "room_name_collision")
        rooms = [room["name"] for room in
                 json.loads(self.trey.post("rooms", "--json").stdout)["rooms"]]
        self.assertNotIn("garden", rooms)
        # The admitted letter completes with no admission check re-run ...
        receipt = self.assert_delivered(self.trey, self.fc, trey_pid, admitted,
                                        admitted_bytes)
        self.assertEqual(json.loads(receipt)["at"], first["admitted_at"])
        # ... while a fresh letter in the same state is refused as forged.
        refused = json.loads(self.receipt_bytes(self.trey, self.fc, trey_pid, control))
        self.assertEqual((refused["status"], refused["reason"]), ("rejected", "forged_from"))

    @needs_deliver
    def test_sender_binding_rejects_first_attempts_only(self):
        # Grok G1: the destination bridge applies its own sender binding to a
        # participant letter on a first attempt. mac claims `garden` and
        # trey's owner memory names mac, but trey keeps the fc placeholder:
        # post alone would admit a fresh fc letter from garden.
        self.bootstrap()
        trey_pid = self.trey.participant("hq")
        admitted, admitted_bytes = self.letter(self.fc, "garden", self.trey, trey_pid)
        fresh, fresh_bytes = self.letter(self.fc, "garden", self.trey, trey_pid)
        self.sweep(self.fc)
        # admitted reached post's admission point before the owner change
        # (a crash after D1: the record exists, no receipt was written).
        first = self.deliver_direct(self.trey, trey_pid, self.fc, admitted, admitted_bytes)
        self.assertEqual(first["outcome"], "delivered")
        # A v2 peer's letter from a room it never published.
        ghost, ghost_bytes = self.letter(self.fc, "ghostroom", self.trey, trey_pid)
        self.fc.inject(self.pmail_path(self.trey, trey_pid, ghost), ghost_bytes)
        self.mac.inject("rooms.json", (json.dumps(
            {"v": 1, "host": "mac", "rooms": ["garden", "porch"]},
            sort_keys=True) + "\n").encode())
        owners_path = self.trey.root / "bridge" / "rooms" / "owners.json"
        owners = json.loads(owners_path.read_text())
        self.assertEqual(owners["garden"]["host"], "fc")  # precondition
        owners["garden"]["host"] = "mac"
        owners_path.write_text(json.dumps(owners, sort_keys=True) + "\n")
        directory, wrapper = self.wrap_post(self.trey)
        self.sweep(self.trey, expect=None, POST_BIN=wrapper)
        rooms = json.loads(self.trey.post("rooms", "--json").stdout)["rooms"]
        placeholder = [room["path"] for room in rooms if room["name"] == "garden"]
        self.assertEqual(len(placeholder), 1)  # precondition: placeholder kept
        self.assertIn("/remote/fc/garden", placeholder[0])
        self.assertEqual(self.health(self.trey)["reason"], "room_name_collision")
        for mail_id, data, reason in ((fresh, fresh_bytes, "name_collision"),
                                      (ghost, ghost_bytes, "unpublished_sender")):
            with self.subTest(reason):
                value = json.loads(self.receipt_bytes(self.trey, self.fc, trey_pid, mail_id))
                self.assertEqual((value["status"], value["reason"], value["sha256"]),
                                 ("rejected", reason, sha(data)))
                self.assertIsNone(self.admission(self.trey, trey_pid, mail_id))
        # The admitted letter went to post, which replayed it.
        calls = self.deliver_calls(directory)
        self.assertEqual(len(calls), 1, calls)
        self.assertIn(admitted, calls[0])
        receipt = self.assert_delivered(self.trey, self.fc, trey_pid, admitted,
                                        admitted_bytes)
        self.assertEqual(json.loads(receipt)["at"], first["admitted_at"])
        self.sweep(self.fc, expect=None)
        self.assert_acked(self.fc, self.trey, trey_pid, fresh, status="rejected")

    # -- the import contract -------------------------------------------------

    def test_interpret_deliver_contract(self):
        participant, mail_id, host = "p-1", fixed_id(1), "fc"
        digest = "a" * 64
        good = {
            "ok": True, "schema": pmail.DELIVER_SCHEMA, "outcome": "delivered",
            "reason": None, "participant": participant, "mail_id": mail_id,
            "source_host": host, "sha256": digest, "admitted_at": "2026-01-01T00:00:00Z",
            "replay": False, "detail": None,
        }
        rejected = dict(good, outcome="rejected", reason="unknown_participant",
                        admitted_at=None)

        def decide(returncode, value):
            stdout = value if isinstance(value, bytes) else json.dumps(value).encode()
            return pmail.interpret_deliver(returncode, stdout, participant, mail_id,
                                           host, digest)

        self.assertEqual(decide(0, good).outcome, "delivered")
        self.assertEqual(decide(0, good).admitted_at, "2026-01-01T00:00:00Z")
        self.assertEqual(
            (decide(0, rejected).outcome, decide(0, rejected).reason),
            ("rejected", "unknown_participant"),
        )
        self.assertEqual(decide(0, dict(good, extra_future_key=1)).outcome, "delivered")
        retries = {
            "nonzero exit claiming rejected": (1, rejected, pmail.POST_UNAVAILABLE),
            "usage error": (2, rejected, pmail.INVALID_INVOCATION),
            "killed": (-9, good, pmail.POST_UNAVAILABLE),
            "empty stdout": (0, b"", pmail.POST_OUTPUT_MALFORMED),
            "garbage": (0, b"not json {", pmail.POST_OUTPUT_MALFORMED),
            "two objects": (0, json.dumps(rejected).encode() * 2,
                            pmail.POST_OUTPUT_MALFORMED),
            "array": (0, b"[]", pmail.POST_OUTPUT_MALFORMED),
            "wrong schema": (0, dict(rejected, schema="post.bridge-deliver.v2"),
                             pmail.POST_OUTPUT_MALFORMED),
            "ok false": (0, dict(rejected, ok=False), pmail.POST_OUTPUT_MALFORMED),
            "echo participant": (0, dict(rejected, participant="p-2"),
                                 pmail.POST_OUTPUT_MALFORMED),
            "echo mail_id": (0, dict(rejected, mail_id=fixed_id(2)),
                             pmail.POST_OUTPUT_MALFORMED),
            "echo source_host": (0, dict(rejected, source_host="mac"),
                                 pmail.POST_OUTPUT_MALFORMED),
            "reason out of vocabulary": (0, dict(rejected, reason="made_up"),
                                         pmail.POST_OUTPUT_MALFORMED),
            "rejected with admitted_at": (0, dict(rejected, admitted_at=good["admitted_at"]),
                                          pmail.POST_OUTPUT_MALFORMED),
            "unknown outcome": (0, dict(rejected, outcome="maybe"),
                                pmail.POST_OUTPUT_MALFORMED),
            "missing key": (0, {k: v for k, v in rejected.items() if k != "replay"},
                            pmail.POST_OUTPUT_MALFORMED),
            "wrong type": (0, dict(rejected, replay="false"), pmail.POST_OUTPUT_MALFORMED),
            "int for bool": (0, dict(rejected, replay=0), pmail.POST_OUTPUT_MALFORMED),
            "delivered other digest": (0, dict(good, sha256="b" * 64),
                                       pmail.POST_OUTPUT_MALFORMED),
            "delivered bad admitted_at": (0, dict(good, admitted_at="yesterday"),
                                          pmail.POST_OUTPUT_MALFORMED),
            "delivered with reason": (0, dict(good, reason="malformed"),
                                      pmail.POST_OUTPUT_MALFORMED),
            "post retry": (0, dict(rejected, outcome="retry", reason="io_error"),
                           "io_error"),
            "post retry odd reason": (0, dict(rejected, outcome="retry", reason="Bad Reason!"),
                                      pmail.POST_OUTPUT_MALFORMED),
        }
        for label, (code, value, reason) in retries.items():
            with self.subTest(label):
                decision = decide(code, value)
                self.assertEqual((decision.outcome, decision.reason), ("retry", reason))

    def test_frozen_contract_samples_parse(self):
        expected = {
            "delivered": ("delivered", None),
            "rejected": ("rejected", "unknown_participant"),
            "retry": ("retry", "digest_mismatch"),
        }
        binary_samples = None
        if POST_HAS_DELIVER:
            result = run([POST, "contract", "samples", "--json"], check=False)
            if result.returncode == 0:
                binary_samples = json.loads(result.stdout)["samples"]
        for name, (outcome, reason) in expected.items():
            with self.subTest(name):
                data = (FIXTURES / f"bridge-deliver-{name}.json").read_bytes()
                if binary_samples is not None:
                    self.assertEqual(
                        binary_samples[f"bridge-deliver-{name}.json"].strip(),
                        data.decode().strip(),
                        "the binary's contract sample drifted from the frozen fixture",
                    )
                value = json.loads(data)
                decision = pmail.interpret_deliver(
                    0, data, value["participant"], value["mail_id"],
                    value["source_host"], value["sha256"],
                )
                self.assertEqual((decision.outcome, decision.reason), (outcome, reason))

    def test_malformed_post_answers_retry_never_reject(self):
        """Each bad answer through the real tick: no receipt, a retry record."""
        self.bootstrap()
        trey_pid = self.trey.participant("hq")
        mail_id, data = self.letter(self.fc, "garden", self.trey, trey_pid)
        self.sweep(self.fc)
        directory, wrapper = self.wrap_post(self.trey)
        echo = {
            "ok": True, "schema": pmail.DELIVER_SCHEMA, "outcome": "rejected",
            "reason": "unknown_participant", "participant": trey_pid,
            "mail_id": mail_id, "source_host": "fc", "sha256": sha(data),
            "admitted_at": None, "replay": False, "detail": None,
        }
        answers = [
            ("nonzero exit", 1, json.dumps(echo), pmail.POST_UNAVAILABLE),
            ("empty stdout", 0, "", pmail.POST_OUTPUT_MALFORMED),
            ("garbage", 0, "}{", pmail.POST_OUTPUT_MALFORMED),
            ("wrong schema", 0, json.dumps(dict(echo, schema="x")),
             pmail.POST_OUTPUT_MALFORMED),
            ("disagreeing echo", 0, json.dumps(dict(echo, participant="someone-else")),
             pmail.POST_OUTPUT_MALFORMED),
            ("out-of-vocabulary reason", 0, json.dumps(dict(echo, reason="nope")),
             pmail.POST_OUTPUT_MALFORMED),
        ]
        ledger = (self.trey.root / "bridge" / "pmail-retry" / "fc" / trey_pid
                  / (mail_id + ".json"))
        for label, code, stdout, reason in answers:
            with self.subTest(label):
                (directory / "fake.out").write_text(stdout)
                (directory / "fake.rc").write_text(str(code))
                self.sweep(self.trey, POST_BIN=wrapper)
                self.assertIsNone(self.receipt_bytes(self.trey, self.fc, trey_pid, mail_id))
                self.assertFalse(
                    (self.trey.repo / self.preceipt_path(self.fc, trey_pid, mail_id)).exists()
                )
                self.assertEqual(json.loads(ledger.read_text())["reason"], reason)
                health = self.health(self.trey)
                self.assertEqual(health["pmail"]["retry"]["fc"]["count"], 1)
                self.assertEqual(health["pmail"]["retry_reasons"], {reason: 1})
        self.assertEqual(len(self.deliver_calls(directory)), len(answers))
        self.assertIsNone(self.inbox(self.trey, trey_pid, mail_id))
        if POST_HAS_DELIVER:
            (directory / "fake.out").unlink()
            self.sweep(self.trey, POST_BIN=wrapper)
            self.assert_delivered(self.trey, self.fc, trey_pid, mail_id, data)
            self.assertFalse(ledger.exists())
            self.assertEqual(self.health(self.trey)["pmail"]["retry"], {})

    # -- sender acknowledgement ---------------------------------------------

    def test_stage_without_push_is_not_published(self):
        self.bootstrap()
        trey_pid = self.trey.participant("hq")
        mail_id, _ = self.letter(self.fc, "garden", self.trey, trey_pid)
        forge = self.topology.forge
        away = forge.with_name("forge-away.git")
        forge.rename(away)  # the relay is unreachable for this tick
        try:
            self.sweep(self.fc, expect=1)
        finally:
            away.rename(forge)
        self.assertEqual(self.health(self.fc)["reason"], "push_failed")
        self.assertIn(self.pmail_path(self.trey, trey_pid, mail_id),
                      self.tree(self.fc, "HEAD", "pmail/"))
        self.assertIsNone(self.state(self.fc, "pmail-published", mail_id))
        status = json.loads(self.state(self.fc, "pmail-status", mail_id))
        self.assertEqual(set(status), {"v", "id", "last_error", "at"})
        self.assertEqual(status["last_error"], "relay_push_failed")
        # A second tick whose push also fails: the commit is local HEAD, and
        # it is still not published.
        forge.rename(away)
        try:
            self.sweep(self.fc, expect=1)
        finally:
            away.rename(forge)
        self.assertIsNone(self.state(self.fc, "pmail-published", mail_id))
        self.sweep(self.fc)
        marker = json.loads(self.state(self.fc, "pmail-published", mail_id))
        self.assertEqual(
            marker["commit"], self.fc.git("rev-parse", "origin/machines/fc").stdout.strip()
        )
        self.assertIsNone(self.state(self.fc, "pmail-status", mail_id))

    @needs_deliver
    def test_receipt_that_outran_the_marker_reaches_terminal_state(self):
        self.bootstrap()
        trey_pid = self.trey.participant("hq")
        mail_id, _ = self.letter(self.fc, "garden", self.trey, trey_pid)
        self.sweep(self.fc, expect=-signal.SIGKILL, BRIDGE_CRASH_AFTER="after-push")
        self.assertIsNone(self.state(self.fc, "pmail-published", mail_id))
        self.sweep(self.trey)
        self.sweep(self.fc)
        self.assert_acked(self.fc, self.trey, trey_pid, mail_id)
        self.assertIsNone(self.state(self.fc, "pmail-published", mail_id))

    def test_receipt_validation_and_conflict(self):
        self.bootstrap()
        trey_pid = self.trey.participant("hq")
        mail_id, data = self.letter(self.fc, "garden", self.trey, trey_pid)
        self.sweep(self.fc)
        path = self.preceipt_path(self.fc, trey_pid, mail_id)

        def receipt(**changes):
            value = {"v": 1, "status": "delivered", "origin": "fc", "host": "trey",
                     "participant": trey_pid, "id": mail_id, "sha256": sha(data),
                     "reason": None, "at": "2026-09-23T00:00:00Z"}
            value.update(changes)
            return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()

        bad = [
            (self.mac, receipt(host="mac")),           # wrong branch
            (self.mac, receipt()),                      # right bytes, wrong branch
            (self.trey, receipt(sha256="0" * 64)),      # wrong digest
            (self.trey, receipt(extra=True)),           # extra key
            (self.trey, receipt(host="mac")),           # host is not its branch
            (self.trey, receipt(status="rejected", reason="bogus")),
        ]
        for machine, payload in bad:
            with self.subTest(payload=payload, branch=machine.host):
                machine.inject(path, payload)
                self.sweep(self.fc)
                self.assertIsNone(self.state(self.fc, "pmail-acked", mail_id))
                self.assertIn(self.pmail_path(self.trey, trey_pid, mail_id),
                              self.tree(self.fc, "HEAD", "pmail/"))
        good = receipt()
        self.trey.inject(path, good)
        self.sweep(self.fc)
        self.assertEqual(self.state(self.fc, "pmail-acked", mail_id), good)
        self.assertEqual(self.tree(self.fc, "HEAD", "pmail/"), [])
        # A second, different, valid receipt: ignored and reported once.
        second = receipt(status="rejected", reason="blocked_route")
        self.trey.inject(path, second)
        self.sweep(self.fc)
        self.sweep(self.fc)
        self.assertEqual(self.state(self.fc, "pmail-acked", mail_id), good)
        self.assertEqual(self.state(self.fc, "pmail-conflicts", mail_id), second)
        conflicts = [r for r in self.actions(self.fc, "receipt_conflict")
                     if r.get("side") == "sender"]
        self.assertEqual(len(conflicts), 1)
        self.assertEqual(self.health(self.fc)["pmail"]["receipt_conflicts"], 1)

    def test_corrupt_acked_record_blocks_instead_of_pruning(self):
        # GLM m1: a damaged pmail-acked is neither acked nor absent.
        self.bootstrap()
        trey_pid = self.trey.participant("hq")
        mail_id, data = self.letter(self.fc, "garden", self.trey, trey_pid)
        self.sweep(self.fc)
        entry = self.pmail_path(self.trey, trey_pid, mail_id)
        self.assertIn(entry, self.tree(self.fc, "HEAD", "pmail/"))  # precondition
        acked = self.fc.root / "bridge" / "pmail-acked" / (mail_id + ".json")
        acked.parent.mkdir(parents=True, exist_ok=True)
        for label, payload in (("empty", b""), ("not a receipt", b"{not json\n")):
            with self.subTest(label):
                acked.write_bytes(payload)
                self.sweep(self.fc)
                self.sweep(self.fc)
                self.assertIn(entry, self.tree(self.fc, "HEAD", "pmail/"))
                self.assertEqual(acked.read_bytes(), payload)
                status = json.loads(self.state(self.fc, "pmail-status", mail_id))
                self.assertEqual(status["blocked_reason"], "acked_invalid")
                self.assertEqual(
                    self.health(self.fc)["pmail"]["queued"].get("acked_invalid"), 1
                )
        self.assertEqual(len(self.actions(self.fc, "pmail_acked_invalid")), 2)
        self.assertEqual(self.actions(self.fc, "pmail_acked"), [])
        # Once a person repairs it, the letter is pruned and the status clears.
        good = (json.dumps(
            {"v": 1, "status": "delivered", "origin": "fc", "host": "trey",
             "participant": trey_pid, "id": mail_id, "sha256": sha(data),
             "reason": None, "at": "2026-09-23T00:00:00Z"},
            sort_keys=True, separators=(",", ":"),
        ) + "\n").encode()
        self.trey.inject(self.preceipt_path(self.fc, trey_pid, mail_id), good)
        acked.write_bytes(good)
        self.sweep(self.fc)
        self.assertEqual(self.state(self.fc, "pmail-acked", mail_id), good)
        self.assertEqual(self.tree(self.fc, "HEAD", "pmail/"), [])
        self.assertIsNone(self.state(self.fc, "pmail-status", mail_id))

    # -- transport: every terminal receipt carries the content digest --------

    def test_oversize_and_contentless_entries(self):
        # Grok G2: a destination cap below the sender's is an honest oversize.
        # The rejection carries the streamed content digest, so the sender
        # reaches acked. A gitlink or symlink has no content: no receipt.
        self.bootstrap()
        trey_pid = self.trey.participant("hq")
        big, data = self.letter(self.fc, "garden", self.trey, trey_pid, body=b"x" * 4096)
        self.sweep(self.fc)
        self.assertGreater(len(data), 2048)  # precondition: over trey's cap below
        gitlink = self.pmail_path(self.trey, trey_pid, fixed_id(0x6001))
        symlink = self.pmail_path(self.trey, trey_pid, fixed_id(0x6002))
        self.fc.inject_gitlink(gitlink)
        self.fc.inject(symlink, b"../../../archive/elsewhere.mail", mode="symlink")
        for _ in range(2):
            self.sweep(self.trey, BRIDGE_MAX_MAIL_BYTES=2048)
        receipt = json.loads(self.receipt_bytes(
            self.trey, self.fc, trey_pid, big, ref="origin/machines/trey"))
        self.assertEqual((receipt["status"], receipt["reason"], receipt["sha256"]),
                         ("rejected", "malformed", sha(data)))
        self.assertEqual(self.tree(self.trey, "HEAD", f"preceipts/fc/{trey_pid}/"),
                         [self.preceipt_path(self.fc, trey_pid, big)])
        ignored = self.actions(self.trey, "pmail_entry_ignored")
        self.assertEqual(sorted(r["id"] for r in ignored),
                         [fixed_id(0x6001), fixed_id(0x6002)])
        self.assertEqual(self.health(self.trey)["pmail"]["ignored_entries"], 2)
        self.assertEqual(self.health(self.trey)["pmail"]["retry"], {})
        self.sweep(self.fc)
        self.assert_acked(self.fc, self.trey, trey_pid, big, status="rejected")
        # A contentless entry later under a receipted path: the receipt
        # stands, and the skipped comparison is logged once (GLM M1).
        self.fc.inject_gitlink(self.pmail_path(self.trey, trey_pid, big))
        for _ in range(2):
            self.sweep(self.trey, BRIDGE_MAX_MAIL_BYTES=2048)
        unchecked = self.actions(self.trey, "pmail_entry_unchecked")
        self.assertEqual([r["id"] for r in unchecked], [big])

    @needs_deliver
    def test_unreadable_entry_retries_and_never_rejects(self):
        # Grok G2: a failed read says nothing about the letter.
        self.bootstrap()
        trey_pid = self.trey.participant("hq")
        mail_id, data = self.letter(self.fc, "garden", self.trey, trey_pid)
        self.sweep(self.fc)
        shim = self.base / "git-shim"
        shim.mkdir()
        flag = shim / "fail"
        flag.touch()
        real = subprocess.run(["sh", "-c", "command -v git"], capture_output=True,
                              text=True, check=True).stdout.strip()
        (shim / "git").write_text(textwrap.dedent(f"""\
            #!/bin/sh
            for arg in "$@"; do
              case "$arg" in
                *:pmail/*) [ -f '{flag}' ] && exit 128 ;;
              esac
            done
            exec '{real}' "$@"
            """))
        (shim / "git").chmod(0o755)
        path = f"{shim}:{os.environ['PATH']}"
        self.sweep(self.trey, PATH=path)
        self.sweep(self.trey, PATH=path)
        self.assertIsNone(self.receipt_bytes(self.trey, self.fc, trey_pid, mail_id))
        health = self.health(self.trey)["pmail"]
        self.assertEqual(health["retry_reasons"], {"object_unreadable": 1})
        self.assertEqual(health["rejected"], 0)
        flag.unlink()
        self.sweep(self.trey, PATH=path)
        self.assert_delivered(self.trey, self.fc, trey_pid, mail_id, data)
        self.assertEqual(self.health(self.trey)["pmail"]["retry"], {})

    @needs_deliver
    def test_letter_for_an_archived_participant_waits_and_is_flagged(self):
        # `post participant gc` (tier 2) moves a long-idle record whole to
        # <root>/participants-archive/<id>/. post cannot find such a
        # participant and rejects a letter for it for good, so the bridge holds
        # the letter, says so in health.json's attention list with the restore
        # command, writes nothing into the missing directory, and delivers
        # once the record is back.
        self.bootstrap()
        trey_pid = self.trey.participant("hq")
        live = self.trey.root / "participants" / trey_pid
        archived = self.trey.root / "participants-archive" / trey_pid
        archived.parent.mkdir()
        live.rename(archived)

        mail_id, data = self.letter(self.fc, "garden", self.trey, trey_pid)
        self.sweep(self.fc)
        self.sweep(self.trey)
        self.sweep(self.trey)

        self.assertIsNone(self.receipt_bytes(self.trey, self.fc, trey_pid, mail_id))
        self.assertFalse(os.path.lexists(live), "the bridge wrote into a missing record")
        self.assertEqual(
            self.actions(self.trey, "pmail_retry")[0]["reason"], "participant_archived"
        )
        pmail_health = self.health(self.trey)["pmail"]
        self.assertEqual(pmail_health["retry_reasons"], {"participant_archived": 1})
        self.assertEqual(pmail_health["rejected"], 0)
        items = [
            entry
            for entry in self.health(self.trey)["attention"]
            if entry["kind"] == "archived_participant"
        ]
        self.assertEqual([entry["id"] for entry in items], [trey_pid])
        self.assertIn(mail_id, items[0]["summary"])
        # The sender neither got a receipt nor a bounce: the letter is queued.
        self.sweep(self.fc)
        self.assertIsNone(self.state(self.fc, "pmail-acked", mail_id))
        self.assertEqual(
            self.tree(self.fc, "origin/machines/fc",
                      self.pmail_path(self.trey, trey_pid, mail_id)),
            [self.pmail_path(self.trey, trey_pid, mail_id)],
        )

        # The fix the item names is exact: run it as written.
        command = re.search(r"mv '[^']+' '[^']+'", items[0]["fix"])
        self.assertIsNotNone(command, items[0]["fix"])
        run(["sh", "-c", command.group(0)])
        self.assertTrue(live.is_dir())
        self.sweep(self.trey)
        self.assert_delivered(self.trey, self.fc, trey_pid, mail_id, data)
        self.assertEqual(
            [
                entry
                for entry in self.health(self.trey)["attention"]
                if entry["kind"] == "archived_participant"
            ],
            [],
        )
        self.sweep(self.fc)
        self.assert_acked(self.fc, self.trey, trey_pid, mail_id)

    # -- visible queued states and outbound exclusion ------------------------

    def test_blocked_reason_is_visible(self):
        self.bootstrap()
        trey_pid = self.trey.participant("hq")
        cases = {
            "peer_not_effective": self.letter(self.fc, "garden", self.trey, trey_pid,
                                              to_host="nowhere")[0],
            "peer_not_effective ": self.letter(self.fc, "garden", self.trey, trey_pid,
                                               to_host="fc")[0],
            "sender_unpublished": self.letter(self.fc, "ghostroom", self.trey,
                                              trey_pid)[0],
        }
        self.sweep(self.fc)
        self.sweep(self.fc)
        for reason, mail_id in cases.items():
            with self.subTest(reason):
                status = json.loads(self.state(self.fc, "pmail-status", mail_id))
                self.assertEqual(set(status), {"v", "id", "blocked_reason", "at"})
                self.assertEqual(status["blocked_reason"], reason.strip())
                self.assertIsNone(self.state(self.fc, "pmail-published", mail_id))
        self.assertEqual(self.tree(self.fc, "HEAD", "pmail/"), [])
        self.assertEqual(
            self.health(self.fc)["pmail"]["queued"],
            {"peer_not_effective": 2, "sender_unpublished": 1},
        )

    def test_typed_letters_never_enter_outbox_and_log_once(self):
        self.bootstrap()
        archive = self.fc.root / "archive"
        letters = {
            # A local participant letter whose id names trey's room.
            fixed_id(0x6001): craft_mail(fixed_id(0x6001), "garden", "hq",
                                         address_kind="participant"),
            # A lineage letter whose name is mac's room.
            fixed_id(0x6002): craft_mail(fixed_id(0x6002), "garden", "porch",
                                         address_kind="lineage"),
            # Workspace-shaped, but host-qualified.
            fixed_id(0x6003): craft_mail(fixed_id(0x6003), "garden", "hq",
                                         to_host="trey"),
            fixed_id(0x6004): craft_mail(fixed_id(0x6004), "garden", "hq",
                                         address_kind="workspace", to_host="trey"),
        }
        for mail_id, data in letters.items():
            (archive / (mail_id + ".mail")).write_bytes(data)
        for _ in range(3):
            self.sweep(self.fc)
            self.sweep(self.trey)
            self.sweep(self.mac)
        self.assertEqual(self.tree(self.fc, "origin/machines/fc", "outbox/"), [])
        for machine, room in ((self.trey, "hq"), (self.mac, "porch")):
            for mail_id in letters:
                self.assertFalse((machine.root / room / "inbox" / (mail_id + ".mail")).exists())
        skipped = self.actions(self.fc, "outbound_typed_skipped")
        self.assertEqual(sorted(r["id"] for r in skipped), sorted(letters))

    # -- health --------------------------------------------------------------

    def test_health_carries_capability_fields(self):
        self.sweep(self.fc)
        for label, raw, expected in (
            # Unknown is omitted, never an invented 60 (GLM m2).
            ("unset is unknown", None, None),
            ("seconds", "15", 15),
            ("timespan", "2min", 120),
            ("garbage never fatal", "soon", None),
            ("over a day is unknown", "90000", None),
        ):
            with self.subTest(label):
                env = {"BRIDGE_INTERVAL_SECONDS": raw}  # None unsets it
                self.sweep(self.fc, **env)
                health = self.health(self.fc)
                self.assertEqual(
                    health["capabilities"],
                    ["participant-mail-v1", "roomless-channel-v1", "typed-outbound-exclusion"],
                )
                self.assertEqual(health.get("interval_s"), expected)
                self.assertEqual("interval_s" in health, expected is not None)
                self.assertEqual(health["ticked_at"], health["ts"])
                self.assertTrue(pmail.valid_rfc3339(health["ticked_at"]), health["ticked_at"])
        # A quiet tick restates them with a fresh ticked_at.
        before = self.health(self.fc)["ticked_at"]
        time.sleep(1.1)
        result = self.sweep(self.fc, full=False)
        quiet = self.health(self.fc)
        self.assertTrue(quiet["quiet"], result.stdout)
        self.assertNotEqual(quiet["ticked_at"], before)
        self.assertIn("participant-mail-v1", quiet["capabilities"])
        # The busy probe and the fatal-config writer carry them too.
        # The busy probe runs beside another process's tick, which may be an
        # older bridge: it carries the prior fields unchanged (Grok G3).
        time.sleep(1.1)
        busy = SWEEPER.write_busy_health(types.SimpleNamespace(root=self.fc.root))
        for key in ("capabilities", "ticked_at", "interval_s"):
            self.assertEqual(busy[key], quiet[key], key)
        self.assertNotEqual(busy["ts"], busy["ticked_at"])
        # Written by a pre-F3 holder: absent stays absent.
        path = self.fc.root / "bridge" / "health.json"
        old = json.loads(path.read_text())
        for key in ("capabilities", "ticked_at", "interval_s"):
            old.pop(key)
        path.write_text(json.dumps(old) + "\n")
        busy = SWEEPER.write_busy_health(types.SimpleNamespace(root=self.fc.root))
        for key in ("capabilities", "ticked_at", "interval_s"):
            self.assertNotIn(key, busy)
        (self.fc.root / "bridge" / "config.json").write_text("{not json\n")
        self.sweep(self.fc, expect=2)
        fatal = self.health(self.fc)
        self.assertEqual(fatal["reason"], "config_error")
        self.assertEqual(sorted(fatal["capabilities"]), sorted(pmail.CAPABILITIES))
        self.assertEqual(fatal["ticked_at"], fatal["ts"])

    def test_quiet_tick_drops_an_unknown_interval(self):
        for _ in range(3):  # settle, so the next unforced tick is quiet
            self.sweep(self.fc)
        self.assertEqual(self.health(self.fc)["interval_s"], 60)  # precondition
        before = self.health(self.fc)["ticked_at"]
        time.sleep(1.1)
        result = self.sweep(self.fc, full=False, BRIDGE_INTERVAL_SECONDS=None)
        quiet = self.health(self.fc)
        self.assertTrue(quiet["quiet"], result.stdout)  # precondition: a quiet tick
        self.assertNotEqual(quiet["ticked_at"], before)
        self.assertNotIn("interval_s", quiet)

    # -- mixed versions ------------------------------------------------------

    def old_bridge(self):
        target = self.base / "pre-f3-bridge"
        target.mkdir()
        # The pre-F3 bridge (claude-space commit af00a1a: sweep.py and
        # bridgelib/) is vendored as a fixture so this repo needs no
        # claude-space history to run the mixed-version test.
        subprocess.run(
            ["tar", "-xzf", str(FIXTURES / "pre-f3-bridge.tar.gz"), "-C", str(target)],
            check=True,
        )
        return target / "post-bridge" / "sweep.py"

    def test_destination_without_f3_keeps_letter_published_with_bounded_logs(self):
        old = self.old_bridge()
        self.bootstrap()
        trey_pid = self.trey.participant("hq")
        mail_id, _ = self.letter(self.fc, "garden", self.trey, trey_pid)
        for _ in range(4):
            self.sweep(self.fc)
            result = run([sys.executable, str(old)], env=self.trey.env(), check=False,
                         timeout=60)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIsNotNone(self.state(self.fc, "pmail-published", mail_id))
        self.assertIsNone(self.state(self.fc, "pmail-acked", mail_id))
        self.assertIsNone(self.inbox(self.trey, trey_pid, mail_id))
        self.assertEqual(self.tree(self.trey, "origin/machines/trey", "preceipts/"), [])
        awaiting = self.health(self.fc)["pmail"]["awaiting_receipt"]
        self.assertEqual(list(awaiting), ["trey"])
        self.assertEqual(awaiting["trey"]["count"], 1)
        self.assertEqual(len(self.actions(self.fc, "pmail_awaiting_receipt")), 1)

    @unittest.skipUnless(
        Path(PRE_F3_POST).exists() and not has_deliver(PRE_F3_POST),
        f"PRE_F3_POST_BIN={PRE_F3_POST} is missing or already has `bridge deliver`",
    )
    def test_pre_f3_post_is_a_retry_with_bounded_logs(self):
        self.bootstrap()
        trey_pid = self.trey.participant("hq")
        mail_id, data = self.letter(self.fc, "garden", self.trey, trey_pid)
        self.sweep(self.fc)
        for _ in range(3):
            self.sweep(self.trey, POST_BIN=PRE_F3_POST)
        self.assertIsNone(self.receipt_bytes(self.trey, self.fc, trey_pid, mail_id))
        self.assertIsNone(self.admission(self.trey, trey_pid, mail_id))
        self.assertIsNone(self.inbox(self.trey, trey_pid, mail_id))
        health = self.health(self.trey)
        self.assertTrue(health["ok"], health)
        self.assertEqual(health["pmail"]["retry"]["fc"]["count"], 1)
        self.assertEqual(health["pmail"]["retry_reasons"], {"post_unavailable": 1})
        self.assertEqual(len(self.actions(self.trey, "pmail_retry")), 1)
        self.assertEqual(len(self.actions(self.trey, "pmail_post_unsupported")), 1)
        if POST_HAS_DELIVER:
            self.sweep(self.trey)
            self.assert_delivered(self.trey, self.fc, trey_pid, mail_id, data)
            self.assertEqual(self.health(self.trey)["pmail"]["retry"], {})

    def test_unknown_peer_set_keeps_hold_and_retry_stamps(self):
        # Hardening after r5.6: no registry branch (the harness has none) and
        # an empty config.peers is "cannot see who the peers are", not "every
        # peer left". Hold and retry stamps survive and stay in health; a
        # known peer set still drops a non-peer's stamps.
        self.bootstrap()
        bridge = self.trey.root / "bridge"
        stamp = "2026-01-01T00:00:00+00:00"
        record = json.dumps(
            {"first_seen": stamp, "reason": "post_unavailable", "logged_at": stamp}
        )

        def plant(host):
            held = bridge / "held-not-homed" / host / "room" / fixed_id(0x5F01)
            held.parent.mkdir(parents=True, exist_ok=True)
            held.write_text(stamp + "\n")
            retry = bridge / "pmail-retry" / host / "bridge-test-p" / (fixed_id(0x5F02) + ".json")
            retry.parent.mkdir(parents=True, exist_ok=True)
            retry.write_text(record)
            return held, retry

        held, retry = plant("fc")
        config_path = bridge / "config.json"
        config = json.loads(config_path.read_text())
        config["peers"] = {}
        config_path.write_text(json.dumps(config, sort_keys=True) + "\n")
        self.sweep(self.trey)
        self.assertTrue(held.exists(), "held-not-homed stamp dropped (peers unknown)")
        self.assertTrue(retry.exists(), "pmail-retry record dropped (peers unknown)")
        health = self.health(self.trey)
        self.assertEqual(health["sender_not_homed"]["fc"]["count"], 1)
        self.assertEqual(health["pmail"]["retry"]["fc"]["count"], 1)

        ghost_held, ghost_retry = plant("ghost")
        self.trey.write_config()
        self.sweep(self.trey)
        self.assertFalse(ghost_held.parent.parent.exists())
        self.assertFalse(ghost_retry.parent.parent.exists())
        self.assertNotIn("ghost", self.health(self.trey)["sender_not_homed"])


if __name__ == "__main__":
    unittest.main()
