#!/usr/bin/env python3
"""End-to-end tests for the ratified post-bridge sweeper.

The public seam is the sweep.py process. Every fixture uses a throwaway Post
root initialized by the real Post CLI and a local bare Git repository.
"""

import hashlib
import importlib.util
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import types
import unittest
from pathlib import Path
from unittest import mock

PINNED_POST_VERSION = "post 0.9.0 up to, but not including, post 0.10.0"
HERE = Path(__file__).resolve().parent
SWEEP = HERE.parent / "sweep.py"
POST = os.environ.get("POST_BIN", "post")
POST_PATH = shutil.which(POST) or POST
GIT_PATH = shutil.which("git") or "git"


def load_sweeper_module():
    spec = importlib.util.spec_from_file_location("post_bridge_sweep_test", SWEEP)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot import {SWEEP}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


SWEEPER = load_sweeper_module()

# load_sweeper_module puts the package directory on sys.path.
from bridgelib.common import REMOTE_PARTICIPANT_UNHOMED, SENDER_NOT_HOMED
from bridgelib.snapshot import FORGED_SELF  # noqa: E402


class CanonicalTemporaryDirectory(tempfile.TemporaryDirectory):
    """A TemporaryDirectory whose `name` is its canonical path.

    macOS puts the default temp root under /var, a symlink to /private/var.
    Production compares canonical paths (placeholder homes, remote/ roots,
    the mail root), so a fixture handing it an uncanonical root fails there
    for a reason Linux never sees. Canonicalizing here keeps production's
    check exactly as strict.
    """

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.name = os.path.realpath(self.name)


def run(args, cwd=None, env=None, check=True, timeout=120):
    # stdin from /dev/null: an inherited open, silent pipe (a background
    # runner's) makes the pre-F3 post refuse reads with input_ambiguous.
    result = subprocess.run(
        [str(arg) for arg in args],
        cwd=str(cwd) if cwd is not None else None,
        env=env,
        stdin=subprocess.DEVNULL,
        text=True,
        capture_output=True,
        timeout=timeout,
        check=False,
    )
    if check and result.returncode != 0:
        raise AssertionError(
            "command failed ({}):\nstdout:\n{}\nstderr:\n{}".format(
                " ".join(str(arg) for arg in args), result.stdout, result.stderr
            )
        )
    return result


def run_input(args, payload, env=None):
    result = subprocess.run(
        [str(arg) for arg in args],
        env=env,
        input=payload,
        capture_output=True,
        timeout=120,
        check=False,
    )
    if result.returncode != 0:
        raise AssertionError(
            "command failed ({}):\nstdout:\n{}\nstderr:\n{}".format(
                " ".join(str(arg) for arg in args),
                result.stdout.decode("utf-8", "replace"),
                result.stderr.decode("utf-8", "replace"),
            )
        )
    return result.stdout.decode("ascii").strip()


def install_loose_peer_tree(topology, observer, host, files):
    git_dir = topology.forge
    created = set()
    object_ids = {}
    tree = {}
    for path, data in files.items():
        object_id = run_input(
            ["git", "--git-dir", git_dir, "hash-object", "-w", "--stdin"], data
        )
        created.add(object_id)
        object_ids[path] = object_id
        node = tree
        parts = Path(path).parts
        for part in parts[:-1]:
            node = node.setdefault(part, {})
        node[parts[-1]] = object_id

    def write_tree(node):
        records = []
        for name in sorted(node):
            value = node[name]
            if isinstance(value, dict):
                object_id = write_tree(value)
                records.append(f"040000 tree {object_id}\t{name}\n")
            else:
                records.append(f"100644 blob {value}\t{name}\n")
        object_id = run_input(
            ["git", "--git-dir", git_dir, "mktree"], "".join(records).encode()
        )
        created.add(object_id)
        return object_id

    root_tree = write_tree(tree)
    parent = run(
        ["git", "--git-dir", git_dir, "rev-parse", f"refs/heads/machines/{host}"]
    ).stdout.strip()
    identity = os.environ.copy()
    identity.update(
        {
            "GIT_AUTHOR_NAME": "fixture",
            "GIT_AUTHOR_EMAIL": "fixture@invalid",
            "GIT_COMMITTER_NAME": "fixture",
            "GIT_COMMITTER_EMAIL": "fixture@invalid",
        }
    )
    commit = run_input(
        ["git", "--git-dir", git_dir, "commit-tree", root_tree, "-p", parent],
        b"loose peer tree\n",
        env=identity,
    )
    created.add(commit)
    run(
        [
            "git",
            "--git-dir",
            git_dir,
            "update-ref",
            f"refs/heads/machines/{host}",
            commit,
            parent,
        ]
    )
    for object_id in created:
        source = git_dir / "objects" / object_id[:2] / object_id[2:]
        target = observer.repo / ".git" / "objects" / object_id[:2] / object_id[2:]
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, target)
    observer.git("update-ref", f"refs/remotes/origin/machines/{host}", commit)
    return object_ids


# post 0.9.0 resolves the acting participant from these. A test run launched
# from an agent session inherits them, and the sweeper runs under systemd or
# launchd with none of them, so every fixture scrubs them and binds its own
# participants explicitly.
PARTICIPANT_ENV = (
    "POST_PARTICIPANT",
    "POST_PARTICIPANT_LEASE_HOURS",
    "POST_HARNESS",
    "POST_NOTICE_MANAGED",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_PID",
    "CODEX_THREAD_ID",
    "CODEX_SESSION_ID",
)
# Commands that never act as a participant in these fixtures: room
# registration and doctor are store-level, `participant` manages itself.
STORE_LEVEL_COMMANDS = {"rooms", "doctor", "participant", "schema", "version"}
# post creates <room>/{inbox,read} on first write, and the bridge no longer
# pre-creates them for every peer room (papercut post-6ep). A doctor that
# still lists that absence as an error is not a bridge fault; the fix wave
# drops the check, and this filter then matches nothing.
LAZY_MAILBOX_CHECK = re.compile(r"room\..+\.(inbox|read)_missing")


def post_env(root):
    env = os.environ.copy()
    for name in (
        "POST_FROM",
        "POST_SENDER_ADDRESS",
        "POST_ARX_GENERATION",
        "BRIDGE_CRASH_AFTER",
        "BRIDGE_RAISE_AFTER",
        "BRIDGE_TEST_HEALTH_DELAY_MS",
    ) + PARTICIPANT_ENV:
        env.pop(name, None)
    env["POST_MAIL_ROOT"] = str(root)
    return env


def bind_participant(post, root, workspace):
    """Mint one participant for `workspace` the way a fresh session would.

    post 0.9.0 refuses writer commands (send, chat --send/--join, consuming
    reads) without a bound participant. `bind --new` under the workspace cwd
    records the workspace as the participant's reply address; the printed
    `export POST_PARTICIPANT=<id>` line is the contract (`post schema` laws).
    """
    result = run(
        [post, "participant", "bind", "--new", "--harness", "bridge-test"],
        cwd=workspace,
        env=post_env(root),
    )
    prefix = "export POST_PARTICIPANT="
    lines = [line for line in result.stdout.splitlines() if line.startswith(prefix)]
    if len(lines) != 1:
        raise AssertionError(f"unexpected bind output: {result.stdout!r}")
    return lines[0][len(prefix):]


def acting_room(workspaces, args, cwd):
    """The room a fixture command acts as: its cwd workspace, else `--room`."""
    if args and str(args[0]) in STORE_LEVEL_COMMANDS:
        return None
    if cwd is not None:
        resolved = Path(cwd).resolve()
        for room, workspace in workspaces.items():
            if Path(workspace).resolve() == resolved:
                return room
        return None
    arguments = [str(arg) for arg in args]
    if "--room" in arguments:
        index = arguments.index("--room")
        if index + 1 < len(arguments) and arguments[index + 1] in workspaces:
            return arguments[index + 1]
    return None


def directory_hash(path):
    digest = hashlib.sha256()
    for item in sorted(path.rglob("*")):
        relative = item.relative_to(path).as_posix().encode("utf-8")
        digest.update(relative + b"\0")
        if item.is_file() and not item.is_symlink():
            digest.update(item.read_bytes())
        elif item.is_symlink():
            digest.update(b"link\0" + os.readlink(str(item)).encode("utf-8"))
    return digest.hexdigest()


def craft_mail(mail_id, sender, recipient, body=b"crafted body", **changes):
    envelope = {
        "id": mail_id,
        "from": sender,
        "to": recipient,
        "kind": "letter",
        "subject": "fixture",
        "sent": "2026-08-23 05:00:00 +0000",
        "sender_provenance": "declared-flag",
    }
    envelope.update(changes)
    return (
        json.dumps(envelope, sort_keys=True, indent=2).encode("utf-8")
        + b"\n---\n"
        + body
    )


def fixed_id(sequence):
    return f"20260823-05{sequence % 10000:04d}-{sequence:06x}"


# About 400 KB: past any json recursion limit, inside the 512 KiB state cap.
DEEP_KEY = "[" * 200000 + "]" * 200000


class Machine:
    def __init__(self, topology, host, rooms, mail_parent=None):
        self.topology = topology
        self.host = host
        self.base = topology.base / host
        self.root = (
            Path(mail_parent) if mail_parent is not None else self.base
        ) / "mail"
        self.repo = self.base / "relay"
        self.key = self.base / "relay-key"
        self.workspaces = {}
        self.participants = {}
        self.base.mkdir(parents=True)
        self.key.write_text("test-only key\n", encoding="utf-8")
        self.key.chmod(0o600)

        doctor = self.post("doctor", "--fix", check=False)
        if doctor.returncode not in (0, 1):
            raise AssertionError(doctor.stdout + doctor.stderr)
        for room in rooms:
            workspace = self.base / "rooms" / room
            workspace.mkdir(parents=True)
            self.post("rooms", "add", room, workspace)
            self.workspaces[room] = workspace
        doctor = self.post("doctor", "--fix", check=False)
        if doctor.returncode not in (0, 1):
            raise AssertionError(doctor.stdout + doctor.stderr)

        run(
            [
                "git",
                "clone",
                "-q",
                "--branch",
                f"machines/{host}",
                topology.forge,
                self.repo,
            ]
        )
        self.git("config", "user.name", f"post-bridge {host}")
        self.git("config", "user.email", f"{host}@post-bridge.invalid")

    def git(self, *args, check=True, env=None):
        command_env = os.environ.copy()
        command_env["BRIDGE_TEST_ACTOR"] = self.host
        if env:
            command_env.update(env)
        return run(
            ["git", "-C", self.repo] + list(args),
            env=command_env,
            check=check,
        )

    def write_config(self):
        peers = {
            machine.host: sorted(machine.workspaces)
            for machine in self.topology.machines
            if machine.host != self.host
        }
        config = {
            "host": self.host,
            "relay_url": str(self.topology.forge.resolve()),
            "peers": peers,
        }
        bridge = self.root / "bridge"
        bridge.mkdir(parents=True, exist_ok=True)
        (bridge / "config.json").write_text(
            json.dumps(config, sort_keys=True) + "\n", encoding="utf-8"
        )
        (bridge / ".health.lock").touch()

    def env(self, **updates):
        env = post_env(self.root)
        env.update(
            {
                "BRIDGE_REPO": str(self.repo.resolve()),
                "BRIDGE_HOST": self.host,
                "BRIDGE_SSH_KEY": str(self.key.resolve()),
                "POST_BIN": POST,
                "BRIDGE_TEST_ACTOR": self.host,
                "BRIDGE_TICK_DEADLINE_SECONDS": "30",
                # Services always set it (install.sh renders the timer's
                # interval; macOS launchd runs at 60 s).
                "BRIDGE_INTERVAL_SECONDS": "60",
                # Decided markers (bridgelib/decided.py) let a tick skip a
                # letter it has already judged. The guard, hold and health
                # tests below assert per-tick re-judgement, which is what a
                # recheck window of 0 means; the decided-marker tests set the
                # real default explicitly (BRIDGE_DECIDED_RECHECK_SECONDS=None).
                "BRIDGE_DECIDED_RECHECK_SECONDS": "0",
            }
        )
        for key, value in updates.items():
            if value is None:
                env.pop(key, None)  # None means unset
            else:
                env[key] = str(value)
        return env

    def sweep(self, check=False, **updates):
        result = run(
            [sys.executable, SWEEP],
            env=self.env(**updates),
            check=False,
            timeout=60,
        )
        if check and result.returncode != 0:
            raise AssertionError(
                f"sweep {self.host} failed ({result.returncode}):\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
            )
        return result

    def participant(self, room):
        """This machine's one bound participant for `room`, minted on first use."""
        if room not in self.participants:
            self.participants[room] = bind_participant(
                POST, self.root, self.workspaces[room]
            )
        return self.participants[room]

    def post(self, *args, cwd=None, check=True, as_room=None):
        env = post_env(self.root)
        room = as_room if as_room is not None else acting_room(
            self.workspaces, args, cwd
        )
        if room is not None:
            env["POST_PARTICIPANT"] = self.participant(room)
        return run(
            [POST] + [str(arg) for arg in args],
            cwd=cwd,
            env=env,
            check=check,
        )

    def doctor_errors(self):
        """Ids of `post doctor` error checks, minus the lazy mailbox ones."""
        doctor = self.post("doctor", "--json", check=False)
        try:
            report = json.loads(doctor.stdout)
        except ValueError:
            raise AssertionError(doctor.stdout + doctor.stderr)
        errors = [
            check["id"]
            for check in report["checks"]
            if check["severity"] == "error"
            and not LAZY_MAILBOX_CHECK.fullmatch(check["id"])
        ]
        if not errors and doctor.returncode not in (0, 1):
            raise AssertionError(doctor.stdout + doctor.stderr)
        return errors

    def send(self, sender, recipient, body, allow_self=False):
        # post 0.9.0 dropped --allow-self: workspace fan-out suppresses only
        # the sending participant, so a send to one's own room is ordinary.
        arguments = ["send", "--to", recipient, "--body", body, "--json"]
        if allow_self:
            arguments.extend(("--kind", "note"))
        result = self.post(*arguments, cwd=self.workspaces[sender])
        return json.loads(result.stdout)["envelope"]["id"]

    def inbox_ids(self, room):
        # Bridge delivery lands receiptless, so post lists it as `pending`
        # until reader activity routes it. `participant touch` is that
        # activity (a live session's bind or watch does the same), after
        # which the room's participant sees it as unread.
        self.post("participant", "touch", as_room=room)
        result = self.post("inbox", "--room", room, "--json")
        return [item["id"] for item in json.loads(result.stdout)["unread"]]

    def read(self, room, mail_id):
        result = self.post("read", mail_id, "--room", room, "--json")
        return json.loads(result.stdout)

    def logs(self):
        path = self.root / "bridge" / "log.jsonl"
        return (
            []
            if not path.exists()
            else [json.loads(line) for line in path.read_text().splitlines()]
        )

    def inject(self, relative, data, mode=None):
        path = self.repo / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        if mode == "symlink":
            path.symlink_to(data.decode("utf-8"))
        else:
            path.write_bytes(data)
            if mode is not None:
                path.chmod(mode)
        self.git("add", "-A", "--", relative)
        self.git("commit", "-q", "-m", f"inject {relative}")
        self.git(
            "push",
            "-q",
            "origin",
            f"HEAD:refs/heads/machines/{self.host}",
        )

    def inject_gitlink(self, relative):
        commit = self.git("rev-parse", "HEAD").stdout.strip()
        self.git("update-index", "--add", "--cacheinfo", f"160000,{commit},{relative}")
        self.git("commit", "-q", "-m", f"inject gitlink {relative}")
        self.git(
            "push",
            "-q",
            "origin",
            f"HEAD:refs/heads/machines/{self.host}",
        )


class Topology:
    def __init__(self, base):
        self.base = Path(base)
        self.forge = self.base / "forge.git"
        self.seed = self.base / "seed"
        self.machines = []
        run(["git", "init", "--bare", "-q", "-b", "main", self.forge])
        run(
            [
                "git",
                "--git-dir",
                self.forge,
                "config",
                "core.logAllRefUpdates",
                "always",
            ]
        )
        run(
            [
                "git",
                "--git-dir",
                self.forge,
                "config",
                "receive.denyNonFastForwards",
                "true",
            ]
        )
        hooks = self.forge / "hooks"
        hook = hooks / "pre-receive"
        hook.write_text(
            "#!/bin/sh\n"
            "set -eu\n"
            "actor=${BRIDGE_TEST_ACTOR:-}\n"
            "while read old new ref; do\n"
            '  [ "$ref" = "refs/heads/machines/$actor" ] || {\n'
            '    echo "protected branch: $actor cannot update $ref" >&2; exit 1; }\n'
            "done\n",
            encoding="utf-8",
        )
        hook.chmod(0o755)
        run(["git", "init", "-q", "-b", "main", self.seed])
        run(["git", "-C", self.seed, "config", "user.name", "fixture"])
        run(["git", "-C", self.seed, "config", "user.email", "fixture@invalid"])
        run(["git", "-C", self.seed, "commit", "-q", "--allow-empty", "-m", "seed"])
        run(["git", "-C", self.seed, "remote", "add", "origin", self.forge])

    def add(self, host, rooms, mail_parent=None):
        env = os.environ.copy()
        env["BRIDGE_TEST_ACTOR"] = host
        run(
            [
                "git",
                "-C",
                self.seed,
                "push",
                "-q",
                "origin",
                f"HEAD:refs/heads/machines/{host}",
            ],
            env=env,
        )
        machine = Machine(self, host, rooms, mail_parent=mail_parent)
        self.machines.append(machine)
        return machine

    def finalize(self):
        for machine in self.machines:
            machine.write_config()

    def refs(self):
        return run(
            ["git", "--git-dir", self.forge, "for-each-ref", "--format=%(refname) %(objectname)"]
        ).stdout


class SweeperTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        version = run([POST, "--version"], check=False)
        if version.returncode != 0 or not SWEEPER.post_version_accepted(version.stdout):
            raise RuntimeError(
                f"tests require {PINNED_POST_VERSION!r}; got stdout={version.stdout.strip()!r} stderr={version.stderr.strip()!r}"
            )

    def setUp(self):
        self.temporary = CanonicalTemporaryDirectory(prefix="post-bridge-r31-")
        self.topology = Topology(self.temporary.name)
        self.fc = self.topology.add("fc", ["garden"])
        self.trey = self.topology.add("trey", ["hq", "atlasos"])
        self.mac = self.topology.add("mac", ["porch"])
        self.topology.finalize()

    def tearDown(self):
        self.temporary.cleanup()

    def bootstrap(self, rounds=2, machines=None):
        """Sweep `machines` (default: all) `rounds` times.

        A machine left out never publishes rooms.json, so it stays a legacy
        peer under SPEC-v2 §Unpublished senders and keeps v1's free-form
        `from-unhomed` senders. Fixtures written before any peer published
        pass the sending peer's exclusion explicitly.
        """
        for _ in range(rounds):
            for machine in self.topology.machines if machines is None else machines:
                result = machine.sweep()
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_atomic_publish_retries_short_writes(self):
        root = Path(self.temporary.name) / "short-write"
        root.mkdir()
        real_write = os.write

        def short_write(descriptor, data):
            return real_write(descriptor, data[:3])

        replaced = root / "replace" / "payload"
        published = root / "publish" / "payload"
        payload = b"complete payload despite short writes"
        with mock.patch.object(SWEEPER.os, "write", side_effect=short_write):
            SWEEPER.atomic_replace(replaced, payload, root)
            SWEEPER.exclusive_publish(published, payload, root / "tmp", root)
        self.assertEqual(replaced.read_bytes(), payload)
        self.assertEqual(published.read_bytes(), payload)

    def test_deadline_is_checked_between_inbound_and_outbound_candidates(self):
        class StopAfter:
            def __init__(self, limit):
                self.limit = limit
                self.calls = 0

            def check(self):
                self.calls += 1
                if self.calls == self.limit:
                    raise SWEEPER.DeadlineExpired()

        class Records:
            def __init__(self):
                self.items = []

            def emit(self, action, **fields):
                self.items.append((action, fields))

        inbound_root = Path(self.temporary.name) / "deadline-inbound"
        inbound_root.mkdir()
        inbound_deadline = StopAfter(3)
        entry = {
            "mode": "100644",
            "type": "blob",
            "object": "1" * 40,
            "size": 1,
            "path": "bad",
        }
        git = types.SimpleNamespace(
            deadline=inbound_deadline,
            rev=lambda ref: "head",
            ls_tree=lambda ref, prefix: [entry, entry],
        )
        settings = types.SimpleNamespace(
            root=inbound_root, host="trey", max_mail_bytes=1024
        )
        records = Records()
        with self.assertRaises(SWEEPER.DeadlineExpired):
            SWEEPER.process_inbound(
                settings,
                types.SimpleNamespace(peers={"fc": []}),
                git,
                {},
                {},
                records,
            )
        self.assertEqual(inbound_deadline.calls, 3)
        self.assertEqual([item[0] for item in records.items].count("quarantined_path"), 1)

        outbound_root = Path(self.temporary.name) / "deadline-outbound"
        archive = outbound_root / "archive"
        archive.mkdir(parents=True)
        for sequence in (800, 801):
            mail_id = fixed_id(sequence)
            (archive / (mail_id + ".mail")).write_bytes(
                craft_mail(mail_id, "outside", "hq")
            )
        outbound_deadline = StopAfter(2)
        with self.assertRaises(SWEEPER.DeadlineExpired):
            SWEEPER.select_outbound(
                types.SimpleNamespace(root=outbound_root, max_mail_bytes=1024),
                types.SimpleNamespace(peers={}),
                {},
                {"hq": "trey"},
                Records(),
                outbound_deadline,
            )
        self.assertEqual(outbound_deadline.calls, 2)

    def test_tick_lock_replacement_during_acquisition_is_busy(self):
        root = Path(self.temporary.name) / "lock-replacement"
        bridge = root / "bridge"
        bridge.mkdir(parents=True)
        lock_path = bridge / ".lock"
        lock_path.touch()
        settings = types.SimpleNamespace(root=root)
        real_flock = SWEEPER.fcntl.flock

        def replace_after_flock(descriptor, operation):
            result = real_flock(descriptor, operation)
            if operation & SWEEPER.fcntl.LOCK_EX:
                lock_path.unlink()
                lock_path.touch()
            return result

        with mock.patch.object(
            SWEEPER.fcntl, "flock", side_effect=replace_after_flock
        ):
            descriptor = SWEEPER.acquire_tick_lock(settings)
        self.assertIsNone(descriptor)

    def test_replaced_lock_path_stays_busy_across_processes(self):
        self.bootstrap()
        mail_id = self.fc.send("garden", "hq", "wait for canonical lock")
        expected = (self.fc.root / "archive" / (mail_id + ".mail")).read_bytes()
        self.assertEqual(self.fc.sweep().returncode, 0)
        lock_path = self.trey.root / "bridge" / ".lock"
        head_before = self.trey.git("rev-parse", "HEAD").stdout.strip()
        helper = subprocess.Popen(
            [
                sys.executable,
                "-c",
                (
                    "import fcntl, sys, time\n"
                    "lock = open(sys.argv[1], 'r+b', buffering=0)\n"
                    "fcntl.flock(lock.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)\n"
                    "print('locked', flush=True)\n"
                    "time.sleep(60)\n"
                ),
                str(lock_path),
            ],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        helper_stdout = helper.stdout
        helper_stderr = helper.stderr
        self.assertIsNotNone(helper_stdout)
        self.assertIsNotNone(helper_stderr)
        try:
            self.assertEqual(helper_stdout.readline().strip(), "locked")
            lock_path.unlink()
            lock_path.touch(mode=0o600)

            logged = len(self.trey.logs())
            busy = self.trey.sweep()

            self.assertEqual(busy.returncode, 0, busy.stdout + busy.stderr)
            health = json.loads(
                (self.trey.root / "bridge" / "health.json").read_text()
            )
            self.assertTrue(health["ok"])
            self.assertEqual(health["busy_streak"], 1)
            self.assertIn(
                "busy", [record["action"] for record in self.trey.logs()[logged:]]
            )
            self.assertNotIn(mail_id, self.trey.inbox_ids("hq"))
            self.assertFalse(
                (
                    self.trey.root
                    / "bridge"
                    / "delivered"
                    / "fc"
                    / "hq"
                    / mail_id
                ).exists()
            )
            self.assertEqual(
                self.trey.git("rev-parse", "HEAD").stdout.strip(), head_before
            )
        finally:
            helper.terminate()
            try:
                helper.wait(timeout=5)
            except subprocess.TimeoutExpired:
                helper.kill()
                helper.wait(timeout=5)
            helper_stdout.close()
            helper_stderr.close()

        delivered = self.trey.sweep()
        self.assertEqual(delivered.returncode, 0, delivered.stdout + delivered.stderr)
        self.assert_delivery_invariant(self.trey, "hq", mail_id, expected)

    def test_missing_required_environment_refuses_without_writes(self):
        before = directory_hash(self.fc.root)
        env = self.fc.env()
        del env["BRIDGE_SSH_KEY"]
        result = run([sys.executable, SWEEP], env=env, check=False)
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        self.assertEqual(directory_hash(self.fc.root), before)

    def test_fence_prevents_mailbox_and_forge_mutation(self):
        fence = self.fc.root / ".post-arx.json"
        fence.write_text('{"state":"fenced","generation":1}\n', encoding="utf-8")
        rooms_before = (self.fc.root / "rooms.json").read_bytes()
        head_before = self.fc.git("rev-parse", "HEAD").stdout.strip()
        result = self.fc.sweep()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertEqual((self.fc.root / "rooms.json").read_bytes(), rooms_before)
        self.assertEqual(self.fc.git("rev-parse", "HEAD").stdout.strip(), head_before)
        self.assertFalse((self.fc.root / ".post-arx.lock").exists())
        for host, rooms in (("trey", ("hq", "atlasos")), ("mac", ("porch",))):
            for room in rooms:
                self.assertFalse((self.fc.root / "remote" / host / room).exists())
        self.assertIn("fenced", [record["action"] for record in self.fc.logs()])
        health = json.loads((self.fc.root / "bridge" / "health.json").read_text())
        self.assertFalse(health["ok"])
        self.assertEqual(health["reason"], "fenced")

    def test_fence_added_between_ticks_stops_queued_mail(self):
        self.bootstrap()
        mail_id = self.fc.send("garden", "hq", "queued before fence")
        archive = self.fc.root / "archive" / (mail_id + ".mail")
        expected = archive.read_bytes()
        head_before = self.fc.git("rev-parse", "HEAD").stdout.strip()
        (self.fc.root / ".post-arx.json").write_text(
            '{"state":"fenced","generation":1}\n', encoding="utf-8"
        )
        result = self.fc.sweep()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertEqual(archive.read_bytes(), expected)
        self.assertEqual(self.fc.git("rev-parse", "HEAD").stdout.strip(), head_before)
        self.assertFalse(
            (self.fc.repo / "outbox" / "trey" / "hq" / (mail_id + ".mail")).exists()
        )

    def test_fatal_health_preserves_first_tick_timestamp(self):
        self.bootstrap()
        health_path = self.fc.root / "bridge" / "health.json"
        first_tick_at = json.loads(health_path.read_text())["first_tick_at"]
        config_path = self.fc.root / "bridge" / "config.json"
        config = json.loads(config_path.read_text())
        config["host"] = "wrong"
        config_path.write_text(json.dumps(config) + "\n", encoding="utf-8")
        result = self.fc.sweep()
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        health = json.loads(health_path.read_text())
        self.assertEqual(health["first_tick_at"], first_tick_at)
        self.assertEqual(health["reason"], "config_error")

    def test_check_config_validates_all_inputs_without_writing(self):
        before = directory_hash(self.fc.root)
        valid = run(
            [sys.executable, SWEEP, "--check-config"],
            env=self.fc.env(),
            check=False,
        )
        self.assertEqual(valid.returncode, 0, valid.stdout + valid.stderr)
        self.assertEqual(json.loads(valid.stdout), {"host": "fc", "ok": True})
        self.assertEqual(directory_hash(self.fc.root), before)

        missing_env = self.fc.env()
        del missing_env["BRIDGE_SSH_KEY"]
        missing_before = directory_hash(self.fc.root)
        missing = run(
            [sys.executable, SWEEP, "--check-config"],
            env=missing_env,
            check=False,
        )
        self.assertEqual(missing.returncode, 2, missing.stdout + missing.stderr)
        self.assertEqual(directory_hash(self.fc.root), missing_before)

        config_path = self.fc.root / "bridge" / "config.json"
        original = config_path.read_text()
        config = json.loads(original)
        config["host"] = "wrong"
        config_path.write_text(json.dumps(config) + "\n", encoding="utf-8")
        config_before = directory_hash(self.fc.root)
        invalid_config = run(
            [sys.executable, SWEEP, "--check-config"],
            env=self.fc.env(),
            check=False,
        )
        self.assertEqual(
            invalid_config.returncode,
            2,
            invalid_config.stdout + invalid_config.stderr,
        )
        self.assertEqual(directory_hash(self.fc.root), config_before)
        config_path.write_text(original, encoding="utf-8")

        self.fc.git("checkout", "-q", "-b", "wrong-check-config-branch")
        clone_before = directory_hash(self.fc.root)
        invalid_clone = run(
            [sys.executable, SWEEP, "--check-config"],
            env=self.fc.env(),
            check=False,
        )
        self.assertEqual(
            invalid_clone.returncode,
            2,
            invalid_clone.stdout + invalid_clone.stderr,
        )
        self.assertEqual(directory_hash(self.fc.root), clone_before)
        self.assertFalse((self.fc.root / "bridge" / "health.json").exists())

    def test_mail_size_configuration_above_one_mib_is_refused(self):
        before = self.topology.refs()
        result = self.fc.sweep(BRIDGE_MAX_MAIL_BYTES=str(1024 * 1024 + 1))
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        self.assertEqual(self.topology.refs(), before)

    def test_round_trips_all_directions_preserve_archive_bytes(self):
        self.bootstrap()
        routes = (
            (self.fc, "garden", self.trey, "hq", "fc to trey"),
            (self.trey, "hq", self.mac, "porch", "trey to mac"),
            (self.mac, "porch", self.fc, "garden", "mac to fc"),
        )
        for sender, sender_room, receiver, receiver_room, body in routes:
            mail_id = sender.send(sender_room, receiver_room, body)
            archive = sender.root / "archive" / (mail_id + ".mail")
            before = archive.read_bytes()
            sent_sweep = sender.sweep()
            self.assertEqual(
                sent_sweep.returncode, 0, sent_sweep.stdout + sent_sweep.stderr
            )
            received_sweep = receiver.sweep()
            self.assertEqual(
                received_sweep.returncode,
                0,
                received_sweep.stdout + received_sweep.stderr,
            )
            self.assertEqual(receiver.inbox_ids(receiver_room), [mail_id])
            delivered = receiver.root / receiver_room / "inbox" / (mail_id + ".mail")
            self.assertEqual(delivered.read_bytes(), before)
            self.assertEqual(
                (receiver.root / "archive" / (mail_id + ".mail")).read_bytes(), before
            )
            self.assertEqual(receiver.read(receiver_room, mail_id)["body"], body)
            prune_sweep = sender.sweep()
            self.assertEqual(
                prune_sweep.returncode, 0, prune_sweep.stdout + prune_sweep.stderr
            )
            settle_sweep = sender.sweep()
            self.assertEqual(
                settle_sweep.returncode, 0, settle_sweep.stdout + settle_sweep.stderr
            )
            self.assertFalse(
                (
                    sender.repo
                    / "outbox"
                    / receiver.host
                    / receiver_room
                    / (mail_id + ".mail")
                ).exists()
            )
            self.assertEqual(archive.read_bytes(), before)

        for machine in self.topology.machines:
            self.assertEqual(machine.doctor_errors(), [])

    def assert_delivery_invariant(self, machine, room, mail_id, expected):
        inbox = machine.root / room / "inbox" / (mail_id + ".mail")
        read = machine.root / room / "read" / (mail_id + ".mail")
        copies = [path for path in (inbox, read) if path.exists()]
        self.assertEqual(len(copies), 1, (inbox, read))
        self.assertEqual(copies[0].read_bytes(), expected)
        self.assertEqual(
            (machine.root / "archive" / (mail_id + ".mail")).read_bytes(), expected
        )

    def test_inbound_reconciles_every_crash_boundary_exactly_once(self):
        self.bootstrap()
        hooks = ("inbound-received",) + tuple(
            f"inbound-i{index}" for index in range(1, 10)
        ) + (
            "after-commit",
            "after-push",
        )
        for read_between in (False, True):
            for hook in hooks:
                with self.subTest(hook=hook, read_between=read_between):
                    mail_id = self.fc.send(
                        "garden", "hq", f"inbound {hook} read={read_between}"
                    )
                    expected = (
                        self.fc.root / "archive" / (mail_id + ".mail")
                    ).read_bytes()
                    archive_before = directory_hash(self.fc.root / "archive")
                    publish = self.fc.sweep()
                    self.assertEqual(
                        publish.returncode, 0, publish.stdout + publish.stderr
                    )
                    crashed = self.trey.sweep(BRIDGE_CRASH_AFTER=hook)
                    self.assertEqual(
                        crashed.returncode,
                        -signal.SIGKILL,
                        crashed.stdout + crashed.stderr,
                    )
                    if hook == "inbound-received":
                        # The reservation is written and nothing else is.
                        self.assertTrue(
                            (self.trey.root / "bridge" / "received" / mail_id).is_file()
                        )
                        self.assertFalse(
                            (
                                self.trey.root / "hq" / "inbox" / (mail_id + ".mail")
                            ).exists()
                        )
                        self.assertFalse(
                            (
                                self.trey.root / "archive" / (mail_id + ".mail")
                            ).exists()
                        )
                        self.assertFalse(
                            (
                                self.trey.root
                                / "bridge"
                                / "delivered"
                                / "fc"
                                / "hq"
                                / mail_id
                            ).exists()
                        )
                    consumed = False
                    if read_between:
                        read = self.trey.post(
                            "read", mail_id, "--room", "hq", "--json", check=False
                        )
                        consumed = read.returncode == 0
                    recovered = self.trey.sweep()
                    self.assertEqual(
                        recovered.returncode, 0, recovered.stdout + recovered.stderr
                    )
                    self.assert_delivery_invariant(self.trey, "hq", mail_id, expected)
                    sha256 = hashlib.sha256(expected).hexdigest()
                    ledger = (
                        self.trey.root
                        / "bridge"
                        / "delivered"
                        / "fc"
                        / "hq"
                        / mail_id
                    )
                    self.assertEqual(ledger.read_text().strip(), sha256)
                    remote_receipt = run(
                        [
                            "git",
                            "--git-dir",
                            self.topology.forge,
                            "show",
                            f"machines/trey:receipts/fc/hq/{mail_id}.json",
                        ]
                    )
                    self.assertEqual(json.loads(remote_receipt.stdout)["sha256"], sha256)
                    self.assertEqual(self.trey.doctor_errors(), [])
                    if not consumed:
                        self.trey.read("hq", mail_id)
                    self.assertEqual(
                        directory_hash(self.fc.root / "archive"), archive_before
                    )
                    settle = self.fc.sweep()
                    self.assertEqual(
                        settle.returncode, 0, settle.stdout + settle.stderr
                    )
                    self.assertFalse(
                        (
                            self.fc.repo
                            / "outbox"
                            / "trey"
                            / "hq"
                            / (mail_id + ".mail")
                        ).exists()
                    )

    def test_outbound_reconciles_every_crash_boundary_and_prune(self):
        self.bootstrap()
        hooks = (
            "outbound-o1-select",
            "outbound-o2-copy",
            "outbound-o3-stage",
            "after-commit",
            "after-push",
            "after-derive",
            "outbound-o4-published",
            "outbound-o4-tidy",
        )
        for hook in hooks:
            with self.subTest(hook=hook):
                mail_id = self.fc.send("garden", "hq", f"outbound {hook}")
                archive_hash = directory_hash(self.fc.root / "archive")
                crashed = self.fc.sweep(BRIDGE_CRASH_AFTER=hook)
                self.assertEqual(
                    crashed.returncode,
                    -signal.SIGKILL,
                    crashed.stdout + crashed.stderr,
                )
                recovered = self.fc.sweep()
                self.assertEqual(
                    recovered.returncode, 0, recovered.stdout + recovered.stderr
                )
                remote_paths = self.fc.git(
                    "ls-tree", "-r", "--name-only", "origin/machines/fc"
                ).stdout.splitlines()
                self.assertEqual(
                    [
                        path
                        for path in remote_paths
                        if path.endswith("/" + mail_id + ".mail")
                    ],
                    [f"outbox/trey/hq/{mail_id}.mail"],
                )
                delivered = self.trey.sweep()
                self.assertEqual(
                    delivered.returncode, 0, delivered.stdout + delivered.stderr
                )
                expected = (self.fc.root / "archive" / (mail_id + ".mail")).read_bytes()
                self.assert_delivery_invariant(self.trey, "hq", mail_id, expected)
                self.assertEqual(self.trey.doctor_errors(), [])
                self.trey.read("hq", mail_id)
                self.assertEqual(directory_hash(self.fc.root / "archive"), archive_hash)
                prune_crash = self.fc.sweep(BRIDGE_CRASH_AFTER="outbound-o5-prune")
                self.assertEqual(
                    prune_crash.returncode,
                    -signal.SIGKILL,
                    prune_crash.stdout + prune_crash.stderr,
                )
                retry = self.fc.sweep()
                self.assertEqual(retry.returncode, 0, retry.stdout + retry.stderr)
                self.assertEqual(directory_hash(self.fc.root / "archive"), archive_hash)

    def test_push_then_die_derives_marker_before_receipt_prune(self):
        self.bootstrap()
        baseline = int(self.fc.git("rev-list", "--count", "HEAD").stdout)
        mail_id = self.fc.send("garden", "hq", "push then die")
        marker = self.fc.root / "bridge" / "published" / mail_id
        crashed = self.fc.sweep(BRIDGE_CRASH_AFTER="after-push")
        self.assertEqual(
            crashed.returncode, -signal.SIGKILL, crashed.stdout + crashed.stderr
        )
        # SIGKILL at the instant the push subprocess returned success: the
        # outbox entry is on the remote and no marker exists yet.
        self.assertFalse(marker.exists())
        self.assertIn(
            f"outbox/trey/hq/{mail_id}.mail",
            self.fc.git(
                "ls-tree", "-r", "--name-only", "origin/machines/fc"
            ).stdout,
        )

        # The receiver settles the letter, so the next fc tick finds a receipt
        # for a letter whose marker was never written. Prune only retires an
        # entry that has a marker, so the same tick must derive the marker
        # from the fetched tree first and then prune.
        delivered = self.trey.sweep()
        self.assertEqual(
            delivered.returncode, 0, delivered.stdout + delivered.stderr
        )
        published_before = len(
            [r for r in self.fc.logs() if r["action"] == "published"]
        )
        derive_tick = self.fc.sweep()
        self.assertEqual(
            derive_tick.returncode, 0, derive_tick.stdout + derive_tick.stderr
        )
        self.assertTrue(marker.is_file())
        self.assertEqual(
            len([r for r in self.fc.logs() if r["action"] == "published"]),
            published_before + 1,
        )
        self.assertFalse(
            (self.fc.repo / "outbox" / "trey" / "hq" / (mail_id + ".mail")).exists()
        )
        self.assertNotIn(
            f"outbox/trey/hq/{mail_id}.mail",
            self.fc.git(
                "ls-tree", "-r", "--name-only", "origin/machines/fc"
            ).stdout,
        )

        # A later letter is relayed and published in one tick and never
        # rewrites the archive bytes; still exactly one outbox entry per mail.
        second_id = self.fc.send("garden", "hq", "second mail after crash")
        archive_before = directory_hash(self.fc.root / "archive")
        pushed = self.fc.sweep()
        self.assertEqual(pushed.returncode, 0, pushed.stdout + pushed.stderr)
        self.assertTrue(
            (self.fc.root / "bridge" / "published" / second_id).is_file()
        )
        delivered = self.trey.sweep()
        self.assertEqual(
            delivered.returncode, 0, delivered.stdout + delivered.stderr
        )
        settled = self.fc.sweep()
        self.assertEqual(settled.returncode, 0, settled.stdout + settled.stderr)
        outbox_paths = [
            path
            for path in self.fc.git(
                "ls-tree", "-r", "--name-only", "origin/machines/fc"
            ).stdout.splitlines()
            if path.startswith("outbox/")
        ]
        self.assertEqual(outbox_paths, [])
        self.assertEqual(directory_hash(self.fc.root / "archive"), archive_before)
        self.assertEqual(
            int(self.fc.git("rev-list", "--count", "HEAD").stdout), baseline + 4
        )

    def test_successful_push_derives_marker_when_post_push_fetch_fails(self):
        self.bootstrap()
        mail_id = self.fc.send("garden", "hq", "derive from pushed head")
        wrapper_dir = self.fc.base / "git-wrapper"
        wrapper_dir.mkdir()
        failed_after_push = wrapper_dir / "failed-after-push"
        wrapper = wrapper_dir / "git"
        wrapper.write_text(
            "#!/bin/sh\n"
            "set -eu\n"
            "is_push=0\n"
            "is_fetch=0\n"
            "for arg do\n"
            '  [ "$arg" = push ] && is_push=1\n'
            '  [ "$arg" = fetch ] && is_fetch=1\n'
            "done\n"
            f'if [ "$is_fetch" = 1 ] && [ -f {str(failed_after_push)!r} ]; then\n'
            "  echo post-push-fetch-blocked >&2\n"
            "  exit 1\n"
            "fi\n"
            'if [ "$is_push" = 1 ]; then\n'
            f"  {GIT_PATH!r} \"$@\"\n"
            "  status=$?\n"
            '  if [ "$status" = 0 ]; then\n'
            f"    {GIT_PATH!r} -C {str(self.fc.repo)!r} update-ref -d refs/remotes/origin/machines/fc\n"
            f"    : > {str(failed_after_push)!r}\n"
            "  fi\n"
            '  exit "$status"\n'
            "fi\n"
            f"exec {GIT_PATH!r} \"$@\"\n",
            encoding="utf-8",
        )
        wrapper.chmod(0o755)
        result = self.fc.sweep(PATH=f"{wrapper_dir}{os.pathsep}{os.environ['PATH']}")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue((self.fc.root / "bridge" / "published" / mail_id).is_file())
        self.assertEqual(
            self.fc.git("rev-parse", "HEAD").stdout,
            self.fc.git("rev-parse", "origin/machines/fc").stdout,
        )
        self.assertTrue(
            any(
                record["action"] == "fetch"
                and record.get("reason") == "post_push_fetch_failed"
                for record in self.fc.logs()
            )
        )

    def test_successful_push_marks_before_post_push_remote_advance(self):
        self.bootstrap()
        mail_id = self.fc.send("garden", "hq", "remote advances after push")
        relative = f"outbox/trey/hq/{mail_id}.mail"
        rival = self.fc.base / "post-push-rival"
        run(
            [
                "git",
                "clone",
                "-q",
                "--branch",
                "machines/fc",
                self.topology.forge,
                rival,
            ]
        )
        run(["git", "-C", rival, "config", "user.name", "fc rival"])
        run(["git", "-C", rival, "config", "user.email", "fc@post-bridge.invalid"])
        wrapper_dir = self.fc.base / "post-push-advance-wrapper"
        wrapper_dir.mkdir()
        advanced = wrapper_dir / "advanced"
        wrapper = wrapper_dir / "git"
        wrapper.write_text(
            "#!/bin/sh\n"
            "set -eu\n"
            "is_push=0\n"
            "for arg do\n"
            '  [ "$arg" = push ] && is_push=1\n'
            "done\n"
            f"{GIT_PATH!r} \"$@\"\n"
            "status=$?\n"
            f'if [ "$status" = 0 ] && [ "$is_push" = 1 ] && [ ! -f {str(advanced)!r} ]; then\n'
            f"  {GIT_PATH!r} -C {str(rival)!r} fetch -q origin machines/fc\n"
            f"  {GIT_PATH!r} -C {str(rival)!r} reset -q --hard origin/machines/fc\n"
            f"  {GIT_PATH!r} -C {str(rival)!r} rm -q -- {relative!r}\n"
            f"  {GIT_PATH!r} -C {str(rival)!r} commit -q -m 'foreign advance removes outbox'\n"
            f"  {GIT_PATH!r} -C {str(rival)!r} push -q origin HEAD:refs/heads/machines/fc\n"
            f"  {GIT_PATH!r} -C {str(self.fc.repo)!r} update-ref -d refs/remotes/origin/machines/fc\n"
            f"  : > {str(advanced)!r}\n"
            "fi\n"
            'exit "$status"\n',
            encoding="utf-8",
        )
        wrapper.chmod(0o755)

        raced = self.fc.sweep(PATH=f"{wrapper_dir}{os.pathsep}{os.environ['PATH']}")
        self.assertEqual(raced.returncode, 1, raced.stdout + raced.stderr)
        health = json.loads((self.fc.root / "bridge" / "health.json").read_text())
        self.assertEqual(health["reason"], "branch_diverged")
        marker = self.fc.root / "bridge" / "published" / mail_id
        self.assertTrue(marker.is_file())
        # Derive+tidy ran inside commit_and_push BEFORE the post-push
        # divergence check, so the placeholder routing artifact is gone even
        # though the tick ended branch_diverged.
        self.assertFalse(
            (self.fc.root / "hq" / "inbox" / (mail_id + ".mail")).exists()
        )

        self.fc.git(
            "fetch",
            "origin",
            "+refs/heads/machines/*:refs/remotes/origin/machines/*",
        )
        self.fc.git("merge", "--ff-only", "origin/machines/fc")
        resolved_head = self.fc.git("rev-parse", "HEAD").stdout.strip()
        recovered = self.fc.sweep()
        self.assertEqual(recovered.returncode, 0, recovered.stdout + recovered.stderr)
        self.assertEqual(self.fc.git("rev-parse", "HEAD").stdout.strip(), resolved_head)
        self.assertNotIn(
            relative,
            self.fc.git(
                "ls-tree", "-r", "--name-only", "origin/machines/fc"
            ).stdout.splitlines(),
        )

    # The identity variables a post subprocess must never inherit. Kept
    # independent of bridgelib.common's scrub tuple so that dropping a name
    # there is caught here.
    SCRUBBED_IDENTITY = (
        "POST_FROM",
        "POST_SENDER_ADDRESS",
        "POST_ARX_GENERATION",
        "POST_PARTICIPANT",
        "POST_PARTICIPANT_LEASE_HOURS",
        "POST_HARNESS",
        "POST_NOTICE_MANAGED",
        "CLAUDE_CODE_SESSION_ID",
        "CLAUDE_PID",
        "CODEX_THREAD_ID",
        "CODEX_SESSION_ID",
    )

    def test_sweeper_runs_post_as_no_participant(self):
        """The bridge is an external writer, never a post participant.

        A sweep started by hand from an agent session inherits that
        session's identity variables. post 0.9.0 refreshes the resolved
        participant's lease on every writer command, `rooms add` included,
        so an unscrubbed sweep would act as the session. A POST_BIN wrapper
        records which scrub-list names each post call inherited.
        """
        participant = self.trey.participant("hq")
        record = self.trey.root / "participants" / participant / "participant.json"
        before = record.read_bytes()
        probe = Path(self.temporary.name) / "post-env-probe"
        probe.mkdir()
        wrapper = probe / "post"
        checks = "".join(
            f'[ "${{{name}+set}}" = set ] && echo {name} >> "$LOG/leaked"\n'
            for name in self.SCRUBBED_IDENTITY
        )
        wrapper.write_text(
            "#!/bin/sh\n"
            f'LOG="{probe}"\n'
            'echo "$*" >> "$LOG/calls"\n'
            + checks
            + f'exec "{POST}" "$@"\n'
        )
        wrapper.chmod(0o755)
        inherited = {name: f"session-{name.lower()}" for name in self.SCRUBBED_IDENTITY}
        inherited["POST_PARTICIPANT"] = participant
        # last_seen has one-second resolution; make a refresh observable.
        time.sleep(1.1)
        result = self.trey.sweep(POST_BIN=wrapper, **inherited)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        checked = run(
            [sys.executable, SWEEP, "--check-config"],
            env=self.trey.env(POST_BIN=wrapper, **inherited),
            check=False,
            timeout=60,
        )
        self.assertEqual(checked.returncode, 0, checked.stdout + checked.stderr)
        calls = (probe / "calls").read_text().splitlines()
        # The precondition that makes this meaningful: every post call site
        # ran, the writer `rooms add` included.
        self.assertIn("--version", calls)
        self.assertIn("rooms --json", calls)
        self.assertTrue(any(call.startswith("rooms add") for call in calls), calls)
        leaked_path = probe / "leaked"
        leaked = set(leaked_path.read_text().split()) if leaked_path.exists() else set()
        for name in self.SCRUBBED_IDENTITY:
            with self.subTest(name=name):
                self.assertNotIn(name, leaked)
        self.assertEqual(record.read_bytes(), before)

    def test_placeholder_read_copy_is_tidied_only_after_push(self):
        self.bootstrap()
        mail_id = self.fc.send("garden", "hq", "drained placeholder")
        # post 0.9.0 never moves mail out of the canonical inbox, so read/
        # copies exist only in stores read by an earlier post. Stage one the
        # way those versions made it: the inbox copy renamed into read/.
        inbox_copy = self.fc.root / "hq" / "inbox" / (mail_id + ".mail")
        read_copy = self.fc.root / "hq" / "read" / (mail_id + ".mail")
        read_copy.parent.mkdir(parents=True, exist_ok=True)
        os.replace(inbox_copy, read_copy)
        self.assertTrue(read_copy.exists())
        crashed = self.fc.sweep(BRIDGE_CRASH_AFTER="after-commit")
        self.assertEqual(crashed.returncode, -signal.SIGKILL)
        self.assertTrue(read_copy.exists())
        recovered = self.fc.sweep()
        self.assertEqual(recovered.returncode, 0, recovered.stdout + recovered.stderr)
        self.assertFalse(read_copy.exists())

    def test_participant_attribution_keys_arrive_intact_without_unknown_key_log(self):
        # SPEC-v2 r5.4 (post-782): post 0.9.0 stamps from_participant,
        # from_lineage and address_kind; they are known keys, carried
        # byte-for-byte and never reported as unknown.
        self.bootstrap()
        self.fc.post("identity", "new", "fern", cwd=self.fc.workspaces["garden"])
        mail_id = self.fc.send("garden", "hq", "attributed")
        before = (self.fc.root / "archive" / (mail_id + ".mail")).read_bytes()
        header = json.loads(before.split(b"\n---\n", 1)[0])
        self.assertEqual(header["from_participant"], self.fc.participant("garden"))
        self.assertEqual(header["from_lineage"], "fern")
        self.assertEqual(header["address_kind"], "workspace")
        for machine in (self.fc, self.trey):
            result = machine.sweep()
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assert_delivery_invariant(self.trey, "hq", mail_id, before)
        self.assertEqual(self.trey.inbox_ids("hq"), [mail_id])
        for machine in (self.fc, self.trey):
            self.assertNotIn(
                "unknown_envelope_keys",
                [record["action"] for record in machine.logs()],
                machine.host,
            )

    def test_lineage_and_participant_mail_is_never_relayed(self):
        self.bootstrap()
        garden = self.fc.workspaces["garden"]
        self.fc.post("identity", "new", "fern", cwd=garden)
        sent = []
        for target in ("lineage:fern", "participant:" + self.fc.participant("garden")):
            result = self.fc.post(
                "send", "--to", target, "--body", target, "--json", cwd=garden
            )
            sent.append(json.loads(result.stdout)["envelope"]["id"])
        # A lineage whose name a peer room later shares: post refuses that
        # collision when it registers the placeholder, so the fixture edits a
        # real send to mac's room into the shape post archives lineage mail
        # in (`to` = the bare name, address_kind lineage).
        shaped_id = self.fc.send("garden", "porch", "lineage-shaped")
        archive = self.fc.root / "archive" / (shaped_id + ".mail")
        header, body = archive.read_bytes().split(b"\n---\n", 1)
        value = json.loads(header)
        self.assertEqual(value["address_kind"], "workspace")
        value["address_kind"] = "lineage"
        archive.unlink()
        archive.write_bytes(json.dumps(value, indent=2).encode() + b"\n---\n" + body)
        # Positive control in the same tick: a workspace-addressed letter is
        # relayed, so the empty outbox below is selection refusing the typed
        # letters and not selection relaying nothing at all.
        plain_id = self.fc.send("garden", "hq", "plain workspace letter")

        result = self.fc.sweep()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue(
            (self.fc.repo / "outbox" / "trey" / "hq" / (plain_id + ".mail")).is_file()
        )
        self.assertTrue((self.fc.root / "bridge" / "published" / plain_id).is_file())
        for mail_id in sent + [shaped_id]:
            self.assertEqual(
                list((self.fc.repo / "outbox").rglob(mail_id + ".mail")), [], mail_id
            )
            self.assertFalse(
                (self.fc.root / "bridge" / "published" / mail_id).exists(), mail_id
            )
        self.assertEqual(
            [r for r in self.fc.logs() if r["action"] == "outbound_ignored"], []
        )

    def test_attribution_keys_are_validated_and_non_workspace_mail_quarantined(self):
        self.bootstrap()
        cases = {
            fixed_id(30): ("unsupported_address_kind", {"address_kind": "lineage"}),
            fixed_id(31): ("unsupported_address_kind", {"address_kind": "participant"}),
            fixed_id(32): ("malformed_header", {"address_kind": "channel"}),
            fixed_id(33): ("malformed_header", {"from_participant": "a:b"}),
            fixed_id(34): ("malformed_header", {"from_lineage": ".."}),
            fixed_id(35): ("malformed_header", {"from_participant": 7}),
            fixed_id(36): ("malformed_header", {"from_lineage": "fern\u202e"}),
        }
        for mail_id, (_, changes) in cases.items():
            self.fc.inject(
                f"outbox/trey/hq/{mail_id}.mail",
                craft_mail(mail_id, "garden", "hq", **changes),
            )
        valid_id = fixed_id(37)
        valid = craft_mail(
            valid_id,
            "garden",
            "hq",
            from_participant="fc-remote-1",
            from_lineage="fern",
            address_kind="workspace",
        )
        self.fc.inject(f"outbox/trey/hq/{valid_id}.mail", valid)
        result = self.trey.sweep()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        for mail_id, (reason, _) in cases.items():
            receipt = json.loads(
                (
                    self.trey.repo / "receipts" / "fc" / "hq" / (mail_id + ".json")
                ).read_text()
            )
            self.assertEqual(receipt["status"], "quarantined", mail_id)
            self.assertTrue(receipt["reason"].startswith(reason), receipt)
            self.assertFalse(
                (self.trey.root / "hq" / "inbox" / (mail_id + ".mail")).exists()
            )
        self.assert_delivery_invariant(self.trey, "hq", valid_id, valid)
        self.assertNotIn(
            "unknown_envelope_keys", [record["action"] for record in self.trey.logs()]
        )

    def test_remote_participant_id_equal_to_a_local_one_arrives_as_remote(self):
        # Identity collision (post-782 / R7): a verified remote sender whose
        # from_participant equals a local participant id. The bridge delivers
        # it byte-for-byte; post must still see it as remote. The evidence
        # post reads (src/output.rs reply_metadata) is `from` registered
        # under <root>/remote/<host>/; the bridge never rewrites
        # sender_provenance.
        self.bootstrap()
        local_id = self.trey.participant("hq")
        mail_id = fixed_id(40)
        mail = craft_mail(
            mail_id,
            "garden",
            "hq",
            from_participant=local_id,
            address_kind="workspace",
            sender_provenance="participant-binding",
        )
        self.fc.inject(f"outbox/trey/hq/{mail_id}.mail", mail)
        result = self.trey.sweep()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assert_delivery_invariant(self.trey, "hq", mail_id, mail)
        rooms = {
            room["name"]: room["path"]
            for room in json.loads(self.trey.post("rooms", "--json").stdout)["rooms"]
        }
        self.assertEqual(rooms["garden"], str(self.trey.root / "remote" / "fc" / "garden"))
        shown = json.loads(
            self.trey.post(
                "read", mail_id, "--room", "hq", "--peek", "--json", as_room="hq"
            ).stdout
        )
        self.assertEqual(shown["envelope"]["from_participant"], local_id)
        self.assertEqual(shown["envelope"]["origin"], "remote")
        self.assertIsNone(shown["envelope"]["reply_to_participant"])
        # r5.5 (M2): a verified stamp whose placeholder post can see delivers.
        self.assertNotIn(SENDER_NOT_HOMED, json.dumps(self.trey.logs()))

    def publish_rooms(self, machine, names):
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

    def receipt(self, machine, host, room, mail_id):
        path = machine.repo / "receipts" / host / room / (mail_id + ".json")
        return json.loads(path.read_text()) if path.exists() else None

    def not_homed_holds(self, machine):
        return [
            record
            for record in machine.logs()
            if record["action"] == "held" and record.get("reason") == SENDER_NOT_HOMED
        ]

    def test_unhomed_stamped_sender_is_quarantined_whatever_the_local_ids(self):
        # r5.5 (M2): an unhomed sender carries no remote-origin evidence post
        # can read, so any from_participant stamp is quarantined, whether or
        # not the id exists locally. Unstamped unhomed mail keeps v1.
        # mac never sweeps, so it stays a legacy peer and its free-form
        # senders are unhomed.
        self.bootstrap(machines=(self.fc, self.trey))
        stamped_id, plain_id = fixed_id(42), fixed_id(43)
        stamped = craft_mail(
            stamped_id, "codex-free", "hq", from_participant="mac-remote-1"
        )
        plain = craft_mail(
            plain_id, "codex-free", "hq", from_lineage="fern", address_kind="workspace"
        )
        self.mac.inject(f"outbox/trey/hq/{stamped_id}.mail", stamped)
        self.mac.inject(f"outbox/trey/hq/{plain_id}.mail", plain)
        result = self.trey.sweep()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        receipt_path = (
            self.trey.repo / "receipts" / "mac" / "hq" / (stamped_id + ".json")
        )
        receipt = json.loads(receipt_path.read_text())
        self.assertEqual(
            (receipt["status"], receipt["reason"]),
            ("quarantined", REMOTE_PARTICIPANT_UNHOMED),
        )
        self.assertFalse(
            (self.trey.root / "hq" / "inbox" / (stamped_id + ".mail")).exists()
        )
        # from_lineage is not a participant stamp: v1 delivery.
        self.assert_delivery_invariant(self.trey, "hq", plain_id, plain)
        self.assertEqual(
            [
                record["id"]
                for record in self.trey.logs()
                if record["action"] == "from-unhomed"
            ],
            [plain_id],
        )

        # A local participant appearing afterwards changes nothing, and a
        # stamp naming a real local id is quarantined the same way.
        before = receipt_path.read_bytes()
        local_id = self.trey.participant("hq")
        colliding_id = fixed_id(41)
        colliding = craft_mail(
            colliding_id, "codex-free", "hq", from_participant=local_id
        )
        self.mac.inject(f"outbox/trey/hq/{colliding_id}.mail", colliding)
        again = self.trey.sweep()
        self.assertEqual(again.returncode, 0, again.stdout + again.stderr)
        self.assertEqual(receipt_path.read_bytes(), before)
        self.assertFalse(
            (self.trey.root / "hq" / "inbox" / (stamped_id + ".mail")).exists()
        )
        colliding_receipt = self.receipt(self.trey, "mac", "hq", colliding_id)
        self.assertEqual(
            (colliding_receipt["status"], colliding_receipt["reason"]),
            ("quarantined", REMOTE_PARTICIPANT_UNHOMED),
        )

    def test_contested_owner_without_placeholder_holds_until_repair(self):
        # r5.5 (M2): fc and mac claim `fern` in the same tick. fc is the
        # owner-of-record, so fc's `fern` verifies, but a contested name
        # gets no placeholder and post would not read `fern` as remote.
        self.bootstrap()
        self.publish_rooms(self.fc, ["fern", "garden"])
        self.publish_rooms(self.mac, ["fern", "porch"])
        mail_id = fixed_id(0x7701)
        mail = craft_mail(mail_id, "fern", "hq", body=b"owner of record")
        self.fc.inject(f"outbox/trey/hq/{mail_id}.mail", mail)

        contested = self.trey.sweep()
        self.assertEqual(contested.returncode, 1, contested.stdout + contested.stderr)
        owners = json.loads(
            (self.trey.root / "bridge" / "rooms" / "owners.json").read_text()
        )
        self.assertEqual(owners["fern"]["host"], "fc")  # precondition
        registered = json.loads(self.trey.post("rooms", "--json").stdout)["rooms"]
        self.assertNotIn("fern", [room["name"] for room in registered])
        self.assertEqual([r["id"] for r in self.not_homed_holds(self.trey)], [mail_id])
        self.assertIsNone(self.receipt(self.trey, "fc", "hq", mail_id))
        self.assertFalse((self.trey.root / "hq" / "inbox" / (mail_id + ".mail")).exists())
        self.assertTrue((self.fc.repo / "outbox" / "trey" / "hq" / (mail_id + ".mail")).exists())
        health = json.loads((self.trey.root / "bridge" / "health.json").read_text())
        self.assertEqual(health["sender_not_homed"]["fc"]["count"], 1)

        self.publish_rooms(self.mac, ["porch"])
        repaired = self.trey.sweep()
        self.assertEqual(repaired.returncode, 0, repaired.stdout + repaired.stderr)
        self.assert_delivery_invariant(self.trey, "hq", mail_id, mail)
        health = json.loads((self.trey.root / "bridge" / "health.json").read_text())
        self.assertEqual(health["sender_not_homed"], {})
        self.assertFalse(
            (self.trey.root / "bridge" / "held-not-homed" / "fc" / "hq" / mail_id).exists()
        )
        # A replay tick (fc has not pruned) leaves exactly one copy.
        settled = self.trey.sweep()
        self.assertEqual(settled.returncode, 0, settled.stdout + settled.stderr)
        self.assert_delivery_invariant(self.trey, "hq", mail_id, mail)

    def test_verified_name_post_refuses_is_held_and_counted(self):
        # r5.5 (M2): a local lineage makes post refuse `rooms add fern`
        # (placeholder_conflict, not fatal). fc's `fern` still verifies from
        # its rooms.json, but post has no placeholder for it: held.
        self.bootstrap()
        self.trey.post("identity", "new", "fern", cwd=self.trey.workspaces["hq"])
        self.publish_rooms(self.fc, ["fern", "garden"])
        mail_id = fixed_id(0x7702)
        mail = craft_mail(mail_id, "fern", "hq")
        self.fc.inject(f"outbox/trey/hq/{mail_id}.mail", mail)
        first = self.trey.sweep()
        self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
        self.assertIn(
            "placeholder_conflict", [r["action"] for r in self.trey.logs()]
        )
        stamp = self.trey.root / "bridge" / "held-not-homed" / "fc" / "hq" / mail_id
        first_seen = stamp.read_bytes()
        health = json.loads((self.trey.root / "bridge" / "health.json").read_text())
        self.assertEqual(health["sender_not_homed"]["fc"]["count"], 1)
        self.assertIsInstance(health["sender_not_homed"]["fc"]["oldest_age_seconds"], int)
        # A new peer push forces the next tick full; independent mail
        # delivers past the hold, which is retried with its stamp kept.
        other_id = fixed_id(0x7712)
        other = craft_mail(other_id, "garden", "hq")
        self.fc.inject(f"outbox/trey/hq/{other_id}.mail", other)
        second = self.trey.sweep()
        self.assertEqual(second.returncode, 0, second.stdout + second.stderr)
        self.assertFalse(json.loads((self.trey.root / "bridge" / "health.json").read_text())["quiet"])
        self.assert_delivery_invariant(self.trey, "hq", other_id, other)
        # The standing hold is logged once (papercut pc2_7b180099776a256e);
        # health still counts it on the retry tick.
        self.assertEqual(len(self.not_homed_holds(self.trey)), 1)
        retried = json.loads((self.trey.root / "bridge" / "health.json").read_text())
        self.assertEqual(retried["sender_not_homed"]["fc"]["count"], 1)
        self.assertEqual(stamp.read_bytes(), first_seen)
        self.assertIsNone(self.receipt(self.trey, "fc", "hq", mail_id))
        self.assertFalse((self.trey.root / "hq" / "inbox" / (mail_id + ".mail")).exists())

    def failing_ls_tree_wrapper(self, name, oid, also=""):
        # A git on PATH that fails every ls-tree naming `oid` (exit 128, as a
        # corrupt object would) and runs `also` when it does.
        wrapper_dir = self.trey.base / name
        wrapper_dir.mkdir()
        fired = wrapper_dir / "fired"
        wrapper = wrapper_dir / "git"
        wrapper.write_text(
            "#!/bin/sh\n"
            "is_target=0\n"
            "has_ref=0\n"
            "for arg do\n"
            '  [ "$arg" = ls-tree ] && is_target=1\n'
            f'  [ "$arg" = {oid!r} ] && has_ref=1\n'
            "done\n"
            'if [ "$is_target$has_ref" = 11 ]; then\n'
            f"  : > {str(fired)!r}\n"
            f"  {also}\n"
            "  exit 128\n"
            "fi\n"
            f"exec {GIT_PATH!r} \"$@\"\n",
            encoding="utf-8",
        )
        wrapper.chmod(0o755)
        return wrapper_dir, fired

    def assert_dying_tick_reports_its_git_failures(self, reason, also="", **env):
        # A tick that records a git read failure for fc and then dies must
        # write this tick's git_failed, not carry the prior tick's list.
        self.bootstrap()
        self.fc.send("garden", "hq", "fc has a branch to list")
        self.assertEqual(self.fc.sweep().returncode, 0)
        clean = self.trey.sweep()
        self.assertEqual(clean.returncode, 0, clean.stdout + clean.stderr)
        health_path = self.trey.root / "bridge" / "health.json"
        self.assertEqual(json.loads(health_path.read_text())["git_failed"], [])
        # A new fc head forces a full tick; the wrapper fails its listing.
        self.fc.send("garden", "hq", "second letter")
        self.assertEqual(self.fc.sweep().returncode, 0)
        fc_oid = run(
            [GIT_PATH, "--git-dir", str(self.topology.forge), "rev-parse",
             "refs/heads/machines/fc"]
        ).stdout.strip()
        wrapper_dir, fired = self.failing_ls_tree_wrapper(
            f"dying-{reason}-wrapper", fc_oid, also
        )
        result = self.trey.sweep(
            PATH=f"{wrapper_dir}{os.pathsep}{os.environ['PATH']}", **env
        )
        self.assertTrue(fired.exists(), "wrapper never fired")
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertNotIn("Traceback", result.stderr)
        health = json.loads(health_path.read_text())
        self.assertEqual(health["reason"], reason)
        self.assertIn(
            "fc",
            [item["host"] for item in health["git_failed"]],
            "the dying tick's git failure was lost",
        )

    def test_tick_error_after_a_git_read_failure_reports_it(self):
        fence = self.trey.root / ".post-arx.json"
        self.assert_dying_tick_reports_its_git_failures(
            "fenced",
            also=f"printf '%s\\n' '{{\"state\":\"fenced\",\"generation\":1}}' > {str(fence)!r}",
        )

    def test_internal_error_after_a_git_read_failure_reports_it(self):
        self.assert_dying_tick_reports_its_git_failures(
            "internal_error", BRIDGE_RAISE_AFTER="outbound-o1-select"
        )

    def test_hold_count_survives_a_failed_listing_of_its_host(self):
        # r5.6 (P2): health's sender_not_homed is the stamps on disk. A tick
        # whose listing of fc fails keeps fc's count and oldest age.
        self.bootstrap()
        self.trey.post("identity", "new", "fern", cwd=self.trey.workspaces["hq"])
        self.publish_rooms(self.fc, ["fern", "garden"])
        mail_id = fixed_id(0x7720)
        self.fc.inject(f"outbox/trey/hq/{mail_id}.mail", craft_mail(mail_id, "fern", "hq"))
        held = self.trey.sweep()
        self.assertEqual(held.returncode, 0, held.stdout + held.stderr)
        health_path = self.trey.root / "bridge" / "health.json"
        before = json.loads(health_path.read_text())["sender_not_homed"]["fc"]
        self.assertEqual(before["count"], 1)

        other_id = fixed_id(0x7721)
        self.fc.inject(
            f"outbox/trey/hq/{other_id}.mail", craft_mail(other_id, "garden", "hq")
        )  # a new head forces a full tick
        fc_oid = run(
            [GIT_PATH, "--git-dir", str(self.topology.forge), "rev-parse",
             "refs/heads/machines/fc"]
        ).stdout.strip()
        wrapper_dir, fired = self.failing_ls_tree_wrapper("hold-listing-wrapper", fc_oid)
        time.sleep(1.1)
        failed = self.trey.sweep(PATH=f"{wrapper_dir}{os.pathsep}{os.environ['PATH']}")
        self.assertTrue(fired.exists(), "wrapper never fired")
        self.assertEqual(failed.returncode, 1, failed.stdout + failed.stderr)
        health = json.loads(health_path.read_text())
        self.assertEqual(health["reason"], "git_failed")
        self.assertIn("fc", health["sender_not_homed"], "failed listing dropped fc's holds")
        self.assertEqual(health["sender_not_homed"]["fc"]["count"], 1)
        self.assertGreater(
            health["sender_not_homed"]["fc"]["oldest_age_seconds"],
            before["oldest_age_seconds"],
        )

    def test_case_variant_of_a_blocked_published_name_is_not_delivered(self):
        # Aster: fc publishes `garden`, a rule blocks `garden`, and mail
        # arrives `from: Garden`. The binding verdict folds case and the
        # rule does not, so a verdict-keyed bridge delivered it past the
        # block with a `from` post cannot place.
        self.bootstrap()
        (self.trey.root / "rules.json").write_text(
            json.dumps(
                {"blocked": [{"from": "garden", "to": "*", "reason": "muted"}]}
            )
            + "\n"
        )
        variant_id, exact_id = fixed_id(0x7703), fixed_id(0x7704)
        variant = craft_mail(variant_id, "Garden", "hq")
        exact = craft_mail(exact_id, "garden", "hq")
        self.fc.inject(f"outbox/trey/hq/{variant_id}.mail", variant)
        self.fc.inject(f"outbox/trey/hq/{exact_id}.mail", exact)
        result = self.trey.sweep()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        for mail_id in (variant_id, exact_id):
            for box in ("inbox", "read"):
                self.assertFalse(
                    (self.trey.root / "hq" / box / (mail_id + ".mail")).exists(),
                    (mail_id, box),
                )
        self.assertEqual(self.receipt(self.trey, "fc", "hq", exact_id)["reason"], "muted")
        self.assertIsNone(self.receipt(self.trey, "fc", "hq", variant_id))
        self.assertEqual(
            [r["id"] for r in self.not_homed_holds(self.trey)], [variant_id]
        )

    def test_replay_keeps_a_delivery_after_its_placeholder_goes(self):
        # r5.5 (M2): the new checks run only for a first delivery. A letter
        # the ledger records as delivered keeps that outcome on replay even
        # when post no longer has the sender's placeholder.
        # `fern` is derived (published, not pinned): a refused re-add is
        # placeholder_conflict, not config-fatal as it is for pinned names.
        self.bootstrap()
        self.publish_rooms(self.fc, ["fern", "garden"])
        mail_id = fixed_id(0x7705)
        mail = craft_mail(mail_id, "fern", "hq")
        self.fc.inject(f"outbox/trey/hq/{mail_id}.mail", mail)
        delivered = self.trey.sweep()
        self.assertEqual(delivered.returncode, 0, delivered.stdout + delivered.stderr)
        self.assert_delivery_invariant(self.trey, "hq", mail_id, mail)
        receipt_path = self.trey.repo / "receipts" / "fc" / "hq" / (mail_id + ".json")
        receipt = receipt_path.read_bytes()
        # Unregister `fern` and make post refuse it back: a lineage.
        rooms_path = self.trey.root / "rooms.json"
        table = json.loads(rooms_path.read_text())
        del table["fern"]
        rooms_path.write_text(json.dumps(table, indent=2) + "\n")
        self.trey.post("identity", "new", "fern", cwd=self.trey.workspaces["hq"])
        self.assertTrue(
            (self.fc.repo / "outbox" / "trey" / "hq" / (mail_id + ".mail")).exists()
        )  # precondition: fc has not pruned, so the tick replays it
        replay = self.trey.sweep()
        self.assertEqual(replay.returncode, 0, replay.stdout + replay.stderr)
        self.assertFalse(json.loads((self.trey.root / "bridge" / "health.json").read_text())["quiet"])
        registered = json.loads(self.trey.post("rooms", "--json").stdout)["rooms"]
        self.assertNotIn("fern", [room["name"] for room in registered])
        self.assertEqual(receipt_path.read_bytes(), receipt)
        self.assert_delivery_invariant(self.trey, "hq", mail_id, mail)
        self.assertEqual(self.not_homed_holds(self.trey), [])

    def condition_lines(self, machine, action, mail_id):
        return [
            record
            for record in machine.logs()
            if record["action"] == action and record.get("id") == mail_id
        ]

    def full_sweep(self, machine):
        # Drop the quiet fingerprint so an unchanged world still gets a full
        # tick, the case the per-letter log dedupe exists for.
        fingerprint = machine.root / "bridge" / "trigger-fingerprint.json"
        if fingerprint.exists():
            fingerprint.unlink()
        result = machine.sweep()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(
            json.loads((machine.root / "bridge" / "health.json").read_text())["quiet"]
        )
        return result

    def test_standing_quarantine_logs_once_until_reason_changes(self):
        # Papercut pc2_7b180099776a256e: a letter that stays quarantined was
        # re-logged (quarantined + forensic) on every full tick.
        self.bootstrap(machines=(self.fc, self.trey))
        mail_id = fixed_id(0x7801)
        relative = f"outbox/trey/hq/{mail_id}.mail"
        self.fc.inject(relative, craft_mail(mail_id, "hq", "hq"))
        for _ in range(3):
            self.full_sweep(self.trey)
        quarantined = self.condition_lines(self.trey, "quarantined", mail_id)
        self.assertEqual([r["reason"] for r in quarantined], [FORGED_SELF])
        # Every full tick's health line still counts the standing condition.
        healths = [r for r in self.trey.logs() if r["action"] == "health"][-3:]
        for health in healths:
            self.assertEqual(health["standing"].get("quarantined"), 1)
            self.assertEqual(health["standing"].get("forensic"), 1)
        self.assertEqual(len(self.condition_lines(self.trey, "forensic", mail_id)), 1)
        # The third tick logged nothing of it (log.jsonl is the only sink).
        logged = len(self.trey.logs())
        self.full_sweep(self.trey)
        self.assertGreater(len(self.trey.logs()), logged)  # it did log its health
        self.assertNotIn(
            mail_id, json.dumps(self.trey.logs()[logged:]), "the standing condition re-logged"
        )
        # A changed reason is a new condition and logs again, once.
        self.fc.inject(relative, b"not an envelope")
        for _ in range(2):
            self.full_sweep(self.trey)
        quarantined = self.condition_lines(self.trey, "quarantined", mail_id)
        self.assertEqual(len(quarantined), 2)
        self.assertEqual(quarantined[0]["reason"], FORGED_SELF)
        self.assertNotEqual(quarantined[1]["reason"], FORGED_SELF)
        # The letter leaving the peer outbox clears the condition, once.
        self.fc.git("rm", "-q", "--", relative)
        self.fc.git("commit", "-q", "-m", "withdraw")
        self.fc.git("push", "-q", "origin", "HEAD:refs/heads/machines/fc")
        for _ in range(2):
            self.full_sweep(self.trey)
        cleared = [
            (r["condition"], r.get("host"), r.get("room"))
            for r in self.condition_lines(self.trey, "condition_cleared", mail_id)
        ]
        self.assertEqual(
            sorted(cleared),
            [("forensic", "fc", "hq"), ("quarantined", "fc", "hq")],
        )
        final = [r for r in self.trey.logs() if r["action"] == "health"][-1]
        self.assertNotIn("quarantined", final["standing"])

    def test_damaged_log_dedupe_state_logs_and_never_fails_the_tick(self):
        self.bootstrap(machines=(self.fc, self.trey))
        mail_id = fixed_id(0x7802)
        self.fc.inject(f"outbox/trey/hq/{mail_id}.mail", craft_mail(mail_id, "hq", "hq"))
        self.full_sweep(self.trey)
        state = self.trey.root / "bridge" / "log-conditions.json"
        self.assertIn(mail_id, state.read_text())
        # Garbage: the tick logs as before and rebuilds the file.
        state.write_bytes(b"\x00{not json")
        self.full_sweep(self.trey)
        self.assertEqual(len(self.condition_lines(self.trey, "quarantined", mail_id)), 2)
        self.assertIn(mail_id, json.dumps(json.loads(state.read_text())))
        self.full_sweep(self.trey)
        self.assertEqual(len(self.condition_lines(self.trey, "quarantined", mail_id)), 2)
        # Unreadable and unwritable (a directory): every tick logs, none fails.
        state.unlink()
        state.mkdir()
        for expected in (3, 4):
            self.full_sweep(self.trey)
            self.assertEqual(
                len(self.condition_lines(self.trey, "quarantined", mail_id)), expected
            )

    def test_malformed_or_oversize_log_dedupe_state_is_rebuilt(self):
        # A state file that parses but holds a key of the wrong shape, or
        # that is past the read cap, is damaged: log as before, rebuild.
        self.bootstrap(machines=(self.fc, self.trey))
        mail_id = fixed_id(0x7804)
        self.fc.inject(f"outbox/trey/hq/{mail_id}.mail", craft_mail(mail_id, "hq", "hq"))
        self.full_sweep(self.trey)
        state = self.trey.root / "bridge" / "log-conditions.json"
        good = json.loads(state.read_text())
        damaged = [
            {"x": {}},  # not a JSON list
            {'["quarantined","fc"]': {}},  # too few items
            {'["quarantined","fc","hq",7]': {}},  # a non-string part
            # An unhashable action part raised TypeError past valid_key.
            {'[[],null,null,null]': {}},
            {'[{},null,null,null]': {}},
            # Nested past the recursion limit: the file's depth check skips
            # key strings, so parsing the key itself must count as damage.
            {DEEP_KEY: {}},
        ]
        # Precondition: this interpreter really raises on the deep key (the
        # limit is the C stack on 3.14, about 1000 levels on older Pythons).
        with self.assertRaises(RecursionError):
            json.loads(DEEP_KEY)
        expected = 1
        for bad in damaged:
            conditions = dict(good["conditions"])
            conditions.update(bad)
            state.write_text(json.dumps({"v": 1, "conditions": conditions}))
            self.full_sweep(self.trey)
            expected += 1
            self.assertEqual(
                len(self.condition_lines(self.trey, "quarantined", mail_id)), expected
            )
            rebuilt = json.loads(state.read_text())["conditions"]
            self.assertFalse(set(bad) & set(rebuilt))
        # A bool version is not version 1, although True == 1.
        state.write_text(json.dumps({"v": True, "conditions": good["conditions"]}))
        self.full_sweep(self.trey)
        expected += 1
        self.assertEqual(
            len(self.condition_lines(self.trey, "quarantined", mail_id)), expected
        )
        self.assertIs(type(json.loads(state.read_text())["v"]), int)
        # Oversize: valid JSON past the read cap.
        conditions = dict(good["conditions"])
        conditions['["held","fc","hq","padding"]'] = {"reason": "x" * (600 * 1024)}
        state.write_text(json.dumps({"v": 1, "conditions": conditions}))
        self.full_sweep(self.trey)
        self.assertEqual(
            len(self.condition_lines(self.trey, "quarantined", mail_id)), expected + 1
        )
        self.assertLess(state.stat().st_size, 512 * 1024)
        self.full_sweep(self.trey)
        self.assertEqual(
            len(self.condition_lines(self.trey, "quarantined", mail_id)), expected + 1
        )

    def test_missing_peer_branch_keeps_its_conditions(self):
        # Review fix3 nit 7: a peer whose branch is missing this tick was not
        # read, so its standing conditions must not clear (and re-log later).
        self.bootstrap(machines=(self.fc, self.trey))
        mail_id = fixed_id(0x7805)
        self.fc.inject(f"outbox/trey/hq/{mail_id}.mail", craft_mail(mail_id, "hq", "hq"))
        self.full_sweep(self.trey)
        self.fc.git("push", "-q", "origin", ":refs/heads/machines/fc")
        self.trey.git("update-ref", "-d", "refs/remotes/origin/machines/fc")
        logged = len(self.trey.logs())
        self.full_sweep(self.trey)
        self.assertIn(
            "peer_branch_missing",
            [record["action"] for record in self.trey.logs()[logged:]],
        )
        self.assertEqual(self.condition_lines(self.trey, "condition_cleared", mail_id), [])
        self.fc.git("push", "-q", "origin", "HEAD:refs/heads/machines/fc")
        self.full_sweep(self.trey)
        self.assertEqual(len(self.condition_lines(self.trey, "quarantined", mail_id)), 1)
        self.assertEqual(self.condition_lines(self.trey, "condition_cleared", mail_id), [])

    def test_standing_forensic_note_is_not_rewritten(self):
        # A non-regular outbox entry is re-noted every full tick; the note
        # keeps its bytes and its inode while nothing about it changes.
        self.bootstrap(machines=(self.fc, self.trey))
        mail_id = fixed_id(0x7803)
        self.fc.inject(
            f"outbox/trey/hq/{mail_id}.mail", b"/etc/passwd", mode="symlink"
        )
        self.full_sweep(self.trey)
        note = self.trey.root / "bridge" / "quarantine" / "fc" / "hq" / (mail_id + ".json")
        before = note.read_bytes()
        identity = (note.stat().st_ino, note.stat().st_mtime_ns)
        self.assertEqual(json.loads(before)["reason"], "non-regular-object")
        time.sleep(1.1)  # a rewrite would carry a new `at` second
        self.full_sweep(self.trey)
        self.assertEqual(note.read_bytes(), before)
        self.assertEqual((note.stat().st_ino, note.stat().st_mtime_ns), identity)

    def test_forged_and_free_form_senders_follow_binding_rule(self):
        # mac never sweeps, so it never publishes rooms.json and stays a
        # legacy peer; fc publishes and is therefore a v2 peer. SPEC-v2
        # §Unpublished senders keys on exactly that difference.
        self.bootstrap(machines=(self.fc, self.trey))
        forged_id = fixed_id(1)
        forged = craft_mail(forged_id, "hq", "hq")
        relative = f"outbox/trey/hq/{forged_id}.mail"
        self.fc.inject(relative, forged)
        result = self.trey.sweep()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        receipt = json.loads(
            (
                self.trey.repo / "receipts" / "fc" / "hq" / (forged_id + ".json")
            ).read_text()
        )
        self.assertEqual(receipt["status"], "quarantined")
        # SPEC-v2 §What this changes in v1 mail names the verdict
        # forged_self unconditionally; the reason must not depend on whether
        # a registry branch happens to be fetchable (review D3).
        self.assertEqual(receipt["reason"], FORGED_SELF)
        self.assertTrue((self.fc.repo / relative).exists())
        self.assertFalse(
            (self.trey.root / "hq" / "inbox" / (forged_id + ".mail")).exists()
        )
        self.assertTrue(
            any((self.trey.root / "bridge" / "quarantine").rglob(forged_id + ".mail"))
        )

        wrong_home_id = fixed_id(3)
        wrong_home = craft_mail(wrong_home_id, "porch", "hq")
        self.fc.inject(f"outbox/trey/hq/{wrong_home_id}.mail", wrong_home)
        wrong_home_result = self.trey.sweep()
        self.assertEqual(
            wrong_home_result.returncode,
            0,
            wrong_home_result.stdout + wrong_home_result.stderr,
        )
        wrong_home_receipt = json.loads(
            (
                self.trey.repo / "receipts" / "fc" / "hq" / (wrong_home_id + ".json")
            ).read_text()
        )
        self.assertEqual(wrong_home_receipt["reason"], FORGED_SELF)

        # A v2 peer speaking a name it never published is quarantined; the
        # same name from a legacy peer keeps v1's from-unhomed delivery.
        unpublished_id = fixed_id(5)
        unpublished = craft_mail(unpublished_id, "codex-free", "hq")
        self.fc.inject(f"outbox/trey/hq/{unpublished_id}.mail", unpublished)
        blocked = self.trey.sweep()
        self.assertEqual(blocked.returncode, 0, blocked.stdout + blocked.stderr)
        unpublished_receipt = json.loads(
            (
                self.trey.repo / "receipts" / "fc" / "hq" / (unpublished_id + ".json")
            ).read_text()
        )
        self.assertEqual(unpublished_receipt["status"], "quarantined")
        self.assertEqual(unpublished_receipt["reason"], "unpublished_sender")
        self.assertFalse(
            (self.trey.root / "hq" / "inbox" / (unpublished_id + ".mail")).exists()
        )

        free_id = fixed_id(2)
        free = craft_mail(free_id, "codex-free", "hq")
        self.mac.inject(f"outbox/trey/hq/{free_id}.mail", free)
        delivered = self.trey.sweep()
        self.assertEqual(delivered.returncode, 0, delivered.stdout + delivered.stderr)
        self.assert_delivery_invariant(self.trey, "hq", free_id, free)
        self.assertIn("from-unhomed", [record["action"] for record in self.trey.logs()])

        future_id = fixed_id(4)
        future = craft_mail(future_id, "garden", "hq", future_field="preserved")
        self.fc.inject(f"outbox/trey/hq/{future_id}.mail", future)
        future_result = self.trey.sweep()
        self.assertEqual(
            future_result.returncode, 0, future_result.stdout + future_result.stderr
        )
        self.assert_delivery_invariant(self.trey, "hq", future_id, future)
        self.assertIn(
            "unknown_envelope_keys",
            [record["action"] for record in self.trey.logs()],
        )

    def test_hostile_inputs_are_quarantined_without_delivery(self):
        self.bootstrap()
        cases = []
        wrong_to_id = fixed_id(10)
        cases.append((wrong_to_id, craft_mail(wrong_to_id, "garden", "porch"), None))
        stem_id = fixed_id(11)
        cases.append((stem_id, craft_mail(fixed_id(99), "garden", "hq"), None))
        bad_kind_id = fixed_id(12)
        cases.append(
            (bad_kind_id, craft_mail(bad_kind_id, "garden", "hq", kind="memo"), None)
        )
        bad_sent_id = fixed_id(13)
        cases.append(
            (
                bad_sent_id,
                craft_mail(bad_sent_id, "garden", "hq", sent="yesterday"),
                None,
            )
        )
        subject_id = fixed_id(14)
        cases.append(
            (
                subject_id,
                craft_mail(subject_id, "garden", "hq", subject="x" * 2048),
                None,
            )
        )
        duplicate_id = fixed_id(15)
        duplicate = (
            f'{{"id":"{duplicate_id}","id":"{duplicate_id}","from":"garden",'
            '"to":"hq","kind":"letter","subject":"x",'
            '"sent":"2026-08-23 05:00:00 +0000"}\n---\nx'
        ).encode()
        cases.append((duplicate_id, duplicate, None))
        missing_separator_id = fixed_id(20)
        cases.append((missing_separator_id, b'{"not":"mail"}', None))
        large_header_id = fixed_id(21)
        large_header = craft_mail(
            large_header_id, "garden", "hq", padding="x" * 4096
        )
        cases.append((large_header_id, large_header, None))
        list_header_id = fixed_id(22)
        cases.append((list_header_id, b"[]\n---\nbody", None))
        for mail_id, data, mode in cases:
            self.fc.inject(f"outbox/trey/hq/{mail_id}.mail", data, mode=mode)

        executable_id = fixed_id(16)
        self.fc.inject(
            f"outbox/trey/hq/{executable_id}.mail",
            craft_mail(executable_id, "garden", "hq"),
            mode=0o755,
        )
        symlink_id = fixed_id(17)
        self.fc.inject(
            f"outbox/trey/hq/{symlink_id}.mail", b"/etc/passwd", mode="symlink"
        )
        gitlink_id = fixed_id(23)
        self.fc.inject_gitlink(f"outbox/trey/hq/{gitlink_id}.mail")
        traversal_id = fixed_id(18)
        self.fc.inject(
            f"outbox/trey/hq\\escape/{traversal_id}.mail",
            craft_mail(traversal_id, "garden", "hq"),
        )
        oversize_id = fixed_id(19)
        self.fc.inject(
            f"outbox/trey/hq/{oversize_id}.mail",
            craft_mail(oversize_id, "garden", "hq", body=b"z" * 2048),
        )
        result = self.trey.sweep(BRIDGE_MAX_MAIL_BYTES="1024")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        expected_ids = {item[0] for item in cases} | {
            executable_id,
            symlink_id,
            gitlink_id,
            oversize_id,
        }
        for mail_id in expected_ids:
            receipt = self.trey.repo / "receipts" / "fc" / "hq" / (mail_id + ".json")
            self.assertTrue(receipt.exists(), mail_id)
            self.assertEqual(json.loads(receipt.read_text())["status"], "quarantined")
            self.assertFalse(
                (self.trey.root / "hq" / "inbox" / (mail_id + ".mail")).exists()
            )
            self.assertFalse(
                (self.trey.root / "archive" / (mail_id + ".mail")).exists()
            )
            self.assertFalse(
                (
                    self.trey.root
                    / "bridge"
                    / "delivered"
                    / "fc"
                    / "hq"
                    / mail_id
                ).exists()
            )
        self.assertTrue(
            any(record["action"] == "quarantined_path" for record in self.trey.logs())
        )
        forensic_root = self.trey.root / "bridge" / "quarantine" / "fc" / "hq"
        self.assertTrue((forensic_root / (symlink_id + ".json")).exists())
        self.assertTrue((forensic_root / (oversize_id + ".json")).exists())

        for mail_id, reason in (
            (symlink_id, "non-regular-object"),
            (gitlink_id, "non-regular-object"),
            (oversize_id, "oversize-unread"),
        ):
            relative = f"outbox/trey/hq/{mail_id}.mail"
            fields = self.fc.git("ls-tree", "HEAD", "--", relative).stdout.split()
            descriptor = f"fc/{relative}@{fields[2]}".encode()
            receipt = json.loads(
                (
                    self.trey.repo / "receipts" / "fc" / "hq" / (mail_id + ".json")
                ).read_text()
            )
            self.assertEqual(receipt["reason"], reason)
            self.assertEqual(receipt["sha256"], hashlib.sha256(descriptor).hexdigest())

        sender = self.fc.sweep()
        self.assertEqual(sender.returncode, 0, sender.stdout + sender.stderr)
        self.assertTrue(
            (self.fc.repo / "outbox" / "trey" / "hq" / (oversize_id + ".mail")).exists()
        )

    def test_peer_room_named_like_post_storage_is_denied(self):
        # M4: post 0.9.0 keeps participants, lineages and routing state at
        # the mail root; a placeholder by that name would write into them.
        self.bootstrap()
        self.trey.participant("hq")
        participants = self.trey.root / "participants"
        before = sorted(path.name for path in participants.iterdir())
        for name in ("participants", "lineages", "routing"):
            with self.subTest(name=name):
                self.fc.inject(
                    "rooms.json",
                    (
                        json.dumps(
                            {"v": 1, "host": "fc", "rooms": ["garden", name]},
                            sort_keys=True,
                        )
                        + "\n"
                    ).encode(),
                )
                logged = len(self.trey.logs())
                result = self.trey.sweep()
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn(
                    "rooms_invalid",
                    [record["action"] for record in self.trey.logs()[logged:]],
                )
                self.assertFalse((self.trey.root / "remote" / "fc" / name).exists())
                for child in ("inbox", "read"):
                    self.assertFalse((self.trey.root / name / child).exists())
                registered = json.loads(self.trey.post("rooms", "--json").stdout)
                self.assertNotIn(name, [room["name"] for room in registered["rooms"]])
        self.assertEqual(sorted(path.name for path in participants.iterdir()), before)

    def test_refused_placeholder_leaves_no_inbox_in_posts_tree(self):
        # M4: inbox/read are created only after `post rooms add` succeeds.
        # A local lineage makes post refuse a name the bridge allows.
        self.bootstrap()
        self.trey.post("identity", "new", "fern", cwd=self.trey.workspaces["hq"])
        self.fc.inject(
            "rooms.json",
            (
                json.dumps(
                    {"v": 1, "host": "fc", "rooms": ["fern", "garden"]}, sort_keys=True
                )
                + "\n"
            ).encode(),
        )
        logged = len(self.trey.logs())
        result = self.trey.sweep()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        # The bridge did attempt `post rooms add fern` and post refused it.
        conflicts = [
            record
            for record in self.trey.logs()[logged:]
            if record["action"] == "placeholder_conflict"
        ]
        self.assertEqual([record.get("room") for record in conflicts], ["fern"])
        registered = json.loads(self.trey.post("rooms", "--json").stdout)
        self.assertNotIn("fern", [room["name"] for room in registered["rooms"]])
        for child in ("inbox", "read"):
            self.assertFalse((self.trey.root / "fern" / child).exists(), child)

    def test_deeply_nested_header_is_quarantined_and_the_tick_survives(self):
        # M3: 2000 levels fit the 4 KiB header cap and raised RecursionError
        # in json.loads on Python 3.9, killing every tick while the entry
        # stayed on the peer branch.
        self.bootstrap()
        nested_id = fixed_id(60)
        nested = b'{"x":' + b"[" * 2000 + b"]" * 2000 + b"}\n---\nbody"
        self.assertLess(nested.find(b"\n---\n"), 4096)
        self.fc.inject(f"outbox/trey/hq/{nested_id}.mail", nested)
        normal_id = fixed_id(61)
        normal = craft_mail(normal_id, "garden", "hq")
        self.fc.inject(f"outbox/trey/hq/{normal_id}.mail", normal)
        result = self.trey.sweep()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        receipt = json.loads(
            (
                self.trey.repo / "receipts" / "fc" / "hq" / (nested_id + ".json")
            ).read_text()
        )
        self.assertEqual(receipt["status"], "quarantined")
        self.assertTrue(receipt["reason"].startswith("malformed_header"), receipt)
        self.assertIn("nests deeper", receipt["reason"])
        self.assert_delivery_invariant(self.trey, "hq", normal_id, normal)

    def test_unreadable_peer_mail_is_quarantined_and_other_mail_delivers(self):
        self.bootstrap()
        bad_id = fixed_id(600)
        good_id = fixed_id(601)
        bad_path = f"outbox/trey/hq/{bad_id}.mail"
        good_path = f"outbox/trey/hq/{good_id}.mail"
        # fc has published rooms.json; its sender is one of its published
        # names so the unreadable-object rule is what is under test.
        good_data = craft_mail(good_id, "garden", "hq", body=b"still delivered")
        object_ids = install_loose_peer_tree(
            self.topology,
            self.trey,
            "fc",
            {
                bad_path: craft_mail(bad_id, "garden", "hq", body=b"vanishes"),
                good_path: good_data,
            },
        )
        bad_object = (
            self.trey.repo
            / ".git"
            / "objects"
            / object_ids[bad_path][:2]
            / object_ids[bad_path][2:]
        )
        wrapper_dir = self.trey.base / "unreadable-mail-wrapper"
        wrapper_dir.mkdir()
        removed = wrapper_dir / "removed"
        wrapper = wrapper_dir / "git"
        wrapper.write_text(
            "#!/bin/sh\n"
            "set -eu\n"
            "is_target=0\n"
            "has_path=0\n"
            "for arg do\n"
            '  [ "$arg" = ls-tree ] && is_target=1\n'
            '  [ "$arg" = outbox/trey/ ] && has_path=1\n'
            "done\n"
            f'if [ "$is_target" = 1 ] && [ "$has_path" = 1 ] && [ ! -f {str(removed)!r} ]; then\n'
            f"  {GIT_PATH!r} \"$@\"\n"
            "  status=$?\n"
            f"  rm -f -- {str(bad_object)!r}\n"
            f"  : > {str(removed)!r}\n"
            '  exit "$status"\n'
            "fi\n"
            f"exec {GIT_PATH!r} \"$@\"\n",
            encoding="utf-8",
        )
        wrapper.chmod(0o755)

        result = self.trey.sweep(
            PATH=f"{wrapper_dir}{os.pathsep}{os.environ['PATH']}"
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        # The wrapper must have fired, or the test proves nothing.
        self.assertTrue(removed.exists())
        self.assertFalse(bad_object.exists())
        self.assert_delivery_invariant(self.trey, "hq", good_id, good_data)
        receipt = json.loads(
            (
                self.trey.repo / "receipts" / "fc" / "hq" / (bad_id + ".json")
            ).read_text()
        )
        descriptor = f"fc/{bad_path}@{object_ids[bad_path]}".encode()
        self.assertEqual(
            (receipt["status"], receipt["reason"], receipt["sha256"]),
            (
                "quarantined",
                "unreadable-object",
                hashlib.sha256(descriptor).hexdigest(),
            ),
        )
        self.assertTrue(
            (
                self.trey.root
                / "bridge"
                / "quarantine"
                / "fc"
                / "hq"
                / (bad_id + ".json")
            ).is_file()
        )

    def test_failed_peer_listing_is_logged_unhealthy_and_not_checkpointed(self):
        # m6: a nonzero `git ls-tree` on one peer's outbox is a git failure,
        # not an empty outbox. It is logged with host and ref, surfaces in
        # health, keeps the tick from checkpointing, and the other peer's
        # mail still delivers. The next plain tick runs full and imports.
        self.bootstrap()
        fc_id, mac_id = fixed_id(0x6601), fixed_id(0x6602)
        fc_mail = craft_mail(fc_id, "garden", "hq", body=b"listing failed once")
        mac_mail = craft_mail(mac_id, "porch", "hq", body=b"independent peer")
        self.fc.inject(f"outbox/trey/hq/{fc_id}.mail", fc_mail)
        self.mac.inject(f"outbox/trey/hq/{mac_id}.mail", mac_mail)
        fc_oid = run(
            [GIT_PATH, "--git-dir", str(self.topology.forge), "rev-parse",
             "refs/heads/machines/fc"]
        ).stdout.strip()
        bridge = self.trey.root / "bridge"
        checkpoint = bridge / "trigger-fingerprint.json"
        before = checkpoint.read_bytes() if checkpoint.exists() else None

        wrapper_dir = self.trey.base / "failing-listing-wrapper"
        wrapper_dir.mkdir()
        fired = wrapper_dir / "fired"
        wrapper = wrapper_dir / "git"
        wrapper.write_text(
            "#!/bin/sh\n"
            "is_target=0\n"
            "has_ref=0\n"
            "has_path=0\n"
            "for arg do\n"
            '  [ "$arg" = ls-tree ] && is_target=1\n'
            f'  [ "$arg" = {fc_oid!r} ] && has_ref=1\n'
            '  [ "$arg" = outbox/trey/ ] && has_path=1\n'
            "done\n"
            'if [ "$is_target$has_ref$has_path" = 111 ]; then\n'
            f"  : > {str(fired)!r}\n"
            "  echo 'fatal: simulated object store failure' >&2\n"
            "  exit 128\n"
            "fi\n"
            f"exec {GIT_PATH!r} \"$@\"\n",
            encoding="utf-8",
        )
        wrapper.chmod(0o755)

        failed = self.trey.sweep(
            PATH=f"{wrapper_dir}{os.pathsep}{os.environ['PATH']}"
        )
        self.assertTrue(fired.exists(), "wrapper never fired")
        self.assertEqual(failed.returncode, 1, failed.stdout + failed.stderr)
        git_failed = [
            event for event in self.trey.logs() if event.get("action") == "git_failed"
        ]
        self.assertEqual(
            [(event["host"], event["ref"], event["path"]) for event in git_failed],
            [("fc", fc_oid, "outbox/trey/")],
        )
        health = json.loads((bridge / "health.json").read_text())
        self.assertEqual((health["ok"], health["reason"]), (False, "git_failed"))
        self.assertEqual(
            health["git_failed"],
            [{"host": "fc", "ref": fc_oid, "path": "outbox/trey/"}],
        )
        self.assertEqual(
            checkpoint.read_bytes() if checkpoint.exists() else None, before
        )
        self.assert_delivery_invariant(self.trey, "hq", mac_id, mac_mail)
        fc_inbox = self.trey.root / "hq" / "inbox" / (fc_id + ".mail")
        self.assertFalse(fc_inbox.exists())
        self.assertFalse(
            (self.trey.repo / "receipts" / "fc" / "hq" / (fc_id + ".json")).exists()
        )

        repaired = self.trey.sweep()
        self.assertEqual(repaired.returncode, 0, repaired.stdout + repaired.stderr)
        health = json.loads((bridge / "health.json").read_text())
        self.assertFalse(health["quiet"])
        self.assertEqual((health["ok"], health["git_failed"]), (True, []))
        self.assert_delivery_invariant(self.trey, "hq", fc_id, fc_mail)
        self.assert_delivery_invariant(self.trey, "hq", mac_id, mac_mail)

    def test_invalid_utf8_peer_path_gets_hash_keyed_forensic_note(self):
        self.bootstrap()
        relative = b"outbox/trey/hq/invalid-\xff.mail"
        # Built in the index, never as a file: APFS refuses the name. The
        # worktree never holds it, so it stays out of the next `add -A`.
        blob = subprocess.run(
            ["git", "-C", str(self.fc.repo), "hash-object", "-w", "--stdin"],
            input=b"invalid path fixture",
            capture_output=True,
            check=True,
        ).stdout.strip()
        subprocess.run(
            ["git", "-C", str(self.fc.repo), "update-index", "--index-info"],
            input=b"100644 " + blob + b"\t" + relative + b"\n",
            capture_output=True,
            check=True,
        )
        self.fc.git("commit", "-q", "-m", "inject invalid utf8 path")
        self.fc.git("push", "-q", "origin", "HEAD:refs/heads/machines/fc")
        listed = subprocess.run(
            ["git", "-C", str(self.fc.repo), "ls-tree", "-r", "-z", "--name-only",
             "HEAD", "--", "outbox/trey/hq/"],
            capture_output=True,
            check=True,
        ).stdout.split(b"\0")
        self.assertIn(relative, listed)

        result = self.trey.sweep()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        fingerprint = hashlib.sha256(relative).hexdigest()
        self.assertTrue(
            any(
                record["action"] == "quarantined_path"
                and record.get("reason") == "invalid_utf8"
                and record.get("id") == fingerprint
                for record in self.trey.logs()
            )
        )
        note_path = (
            self.trey.root
            / "bridge"
            / "quarantine"
            / "fc"
            / "_paths"
            / (fingerprint + ".json")
        )
        note = json.loads(note_path.read_text())
        self.assertEqual(note["reason"], "invalid_utf8")
        self.assertEqual(note["path_sha256"], fingerprint)
        self.assertFalse((self.trey.repo / "receipts" / "fc").exists())

    def test_blocked_route_is_held_then_delivered_after_rule_removal(self):
        self.bootstrap()
        mail_id = self.fc.send("garden", "hq", "held route")
        self.assertEqual(self.fc.sweep().returncode, 0)
        reason = "operator hold"
        (self.trey.root / "rules.json").write_text(
            json.dumps({"blocked": [{"from": "garden", "to": "hq", "reason": reason}]})
            + "\n",
            encoding="utf-8",
        )
        held = self.trey.sweep()
        self.assertEqual(held.returncode, 0, held.stdout + held.stderr)
        receipt_path = self.trey.repo / "receipts" / "fc" / "hq" / (mail_id + ".json")
        receipt = json.loads(receipt_path.read_text())
        self.assertEqual((receipt["status"], receipt["reason"]), ("held", reason))
        health = json.loads((self.trey.root / "bridge" / "health.json").read_text())
        self.assertTrue(health["ok"])
        self.assertEqual(health["reason"], "ok")
        self.assertEqual(health["held"], 1)
        self.assertFalse(
            (self.trey.root / "hq" / "inbox" / (mail_id + ".mail")).exists()
        )
        (self.trey.root / "rules.json").write_text('{"blocked":[]}\n', encoding="utf-8")
        delivered = self.trey.sweep()
        self.assertEqual(delivered.returncode, 0, delivered.stdout + delivered.stderr)
        self.assertEqual(json.loads(receipt_path.read_text())["status"], "delivered")

    def test_placeholder_rule_is_inert_while_real_room_rule_holds(self):
        self.bootstrap()
        rules_path = self.trey.root / "rules.json"
        rules_path.write_text(
            json.dumps(
                {
                    "blocked": [
                        {"from": "*", "to": "garden", "reason": "remote only"}
                    ]
                }
            )
            + "\n",
            encoding="utf-8",
        )
        delivered_id = self.fc.send("garden", "hq", "placeholder rule is inert")
        self.assertEqual(self.fc.sweep().returncode, 0)
        delivered = self.trey.sweep()
        self.assertEqual(delivered.returncode, 0, delivered.stdout + delivered.stderr)
        self.assertIn(delivered_id, self.trey.inbox_ids("hq"))

        held_id = self.fc.send("garden", "hq", "real room rule holds")
        self.assertEqual(self.fc.sweep().returncode, 0)
        rules_path.write_text(
            json.dumps(
                {
                    "blocked": [
                        {"from": "*", "to": "garden", "reason": "remote only"},
                        {"from": "garden", "to": "hq", "reason": "real hold"},
                    ]
                }
            )
            + "\n",
            encoding="utf-8",
        )
        held = self.trey.sweep()
        self.assertEqual(held.returncode, 0, held.stdout + held.stderr)
        receipt = json.loads(
            (
                self.trey.repo / "receipts" / "fc" / "hq" / (held_id + ".json")
            ).read_text()
        )
        self.assertEqual((receipt["status"], receipt["reason"]), ("held", "real hold"))

    def test_rules_allow_reserved_post_names_and_still_match_real_rooms(self):
        rules_path = self.trey.root / "rules.json"
        rules_path.write_text(
            json.dumps(
                {
                    "blocked": [
                        {"from": "*", "to": "archive", "reason": "reserved"},
                        {"from": "archive", "to": "hq", "reason": "reserved"},
                        {"from": "garden", "to": "hq", "reason": "real hold"},
                    ]
                }
            )
            + "\n",
            encoding="utf-8",
        )

        rules = SWEEPER.load_rules(types.SimpleNamespace(root=self.trey.root))

        self.assertEqual(len(rules), 3)
        self.assertEqual(SWEEPER.blocked_reason(rules, "archive", "hq"), "reserved")
        self.assertEqual(SWEEPER.blocked_reason(rules, "garden", "hq"), "real hold")

    def test_rules_file_can_vanish_after_placeholder_setup(self):
        self.bootstrap()
        mail_id = self.fc.send("garden", "hq", "no rules file")
        expected = (self.fc.root / "archive" / (mail_id + ".mail")).read_bytes()
        self.assertEqual(self.fc.sweep().returncode, 0)
        rules_path = self.trey.root / "rules.json"

        def delete_rules_after_placeholders(name):
            if name == "after-placeholders":
                rules_path.unlink()

        with mock.patch.dict(os.environ, self.trey.env(), clear=True), mock.patch.object(
            SWEEPER, "checkpoint", side_effect=delete_rules_after_placeholders
        ):
            returncode = SWEEPER.execute()

        self.assertEqual(returncode, 0)
        self.assertFalse(rules_path.exists())
        self.assert_delivery_invariant(self.trey, "hq", mail_id, expected)

    def test_registration_collision_is_fatal_and_persisted(self):
        wrong = self.fc.base / "wrong-hq"
        wrong.mkdir()
        self.fc.post("rooms", "add", "hq", wrong)
        result = self.fc.sweep()
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        self.assertTrue((self.fc.root / "bridge" / "collisions.json").exists())

    def test_placeholder_registration_is_idempotent(self):
        log = self.fc.base / "post-wrapper.log"
        wrapper = self.fc.base / "post-wrapper"
        wrapper.write_text(
            "#!/usr/bin/env python3\n"
            "import os, sys\n"
            f"with open({str(log)!r}, 'a', encoding='utf-8') as stream:\n"
            "    stream.write(' '.join(sys.argv[1:]) + '\\n')\n"
            f"os.execv({POST_PATH!r}, [{POST_PATH!r}] + sys.argv[1:])\n",
            encoding="utf-8",
        )
        wrapper.chmod(0o755)
        first = self.fc.sweep(POST_BIN=wrapper)
        self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
        for placeholder in ("hq", "atlasos", "porch"):
            # post creates <root>/<room>/{inbox,read} on first use; the bridge
            # no longer makes empty ones for every peer room (post-6ep).
            self.assertFalse((self.fc.root / placeholder / "inbox").exists())
            self.assertFalse((self.fc.root / placeholder / "read").exists())
        rooms_before = (self.fc.root / "rooms.json").read_bytes()
        second = self.fc.sweep(POST_BIN=wrapper)
        self.assertEqual(second.returncode, 0, second.stdout + second.stderr)
        self.assertEqual((self.fc.root / "rooms.json").read_bytes(), rooms_before)
        additions = [
            line
            for line in log.read_text().splitlines()
            if line.startswith("rooms add ")
        ]
        self.assertEqual(len(additions), 3, additions)

    def test_leading_hyphen_topology_room_registers_as_positional_data(self):
        config_path = self.fc.root / "bridge" / "config.json"
        config = json.loads(config_path.read_text())
        config["peers"]["trey"].append("-q")
        config_path.write_text(json.dumps(config) + "\n", encoding="utf-8")
        result = self.fc.sweep()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        rooms = json.loads(self.fc.post("rooms", "--json").stdout)["rooms"]
        registered = {room["name"]: room["path"] for room in rooms}
        self.assertEqual(
            Path(registered["-q"]).resolve(),
            (self.fc.root / "remote" / "trey" / "-q").resolve(),
        )

    def test_tick_commit_has_explicit_identity_without_git_configuration(self):
        self.bootstrap()
        self.fc.git("config", "--unset", "user.name", check=False)
        self.fc.git("config", "--unset", "user.email", check=False)
        empty_home = self.fc.base / "empty-home"
        empty_home.mkdir()
        mail_id = self.fc.send("garden", "hq", "explicit commit identity")
        env = self.fc.env(HOME=empty_home)
        for name in (
            "GIT_AUTHOR_NAME",
            "GIT_AUTHOR_EMAIL",
            "GIT_COMMITTER_NAME",
            "GIT_COMMITTER_EMAIL",
        ):
            env.pop(name, None)
        result = run([sys.executable, SWEEP], env=env, check=False, timeout=60)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(
            f"outbox/trey/hq/{mail_id}.mail",
            self.fc.git("ls-tree", "-r", "--name-only", "HEAD").stdout,
        )
        identity = self.fc.git("show", "-s", "--format=%an <%ae>", "HEAD")
        self.assertEqual(identity.stdout.strip(), "post-bridge <post-bridge@fc>")

    def test_each_branch_reflog_contains_only_its_owner_commits(self):
        self.bootstrap()
        for machine, sender, recipient in (
            (self.fc, "garden", "hq"),
            (self.trey, "hq", "porch"),
            (self.mac, "porch", "garden"),
        ):
            machine.send(sender, recipient, "ownership")
            self.assertEqual(machine.sweep().returncode, 0)
        for host in ("fc", "trey", "mac"):
            reflog_commits = run(
                [
                    "git",
                    "--git-dir",
                    self.topology.forge,
                    "reflog",
                    "show",
                    f"refs/heads/machines/{host}",
                    "--format=%H",
                ]
            ).stdout.splitlines()
            authors = [
                run(
                    [
                        "git",
                        "--git-dir",
                        self.topology.forge,
                        "show",
                        "-s",
                        "--format=%ae",
                        commit,
                    ]
                ).stdout.strip()
                for commit in reflog_commits
            ]
            self.assertTrue(authors)
            self.assertTrue(
                all(
                    author == "fixture@invalid"
                    or author == f"post-bridge@{host}"
                    for author in authors
                ),
                (host, authors),
            )


    def test_duplicate_config_keys_and_loose_key_mode_are_fatal(self):
        config_path = self.fc.root / "bridge" / "config.json"
        valid = config_path.read_text()
        duplicate = valid.replace('"host": "fc"', '"host": "fc", "host": "fc"')
        config_path.write_text(duplicate, encoding="utf-8")
        result = self.fc.sweep()
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        config_path.write_text(valid, encoding="utf-8")
        self.fc.key.chmod(0o644)
        result = self.fc.sweep()
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)

    def test_prune_crashes_after_commit_and_push_recover(self):
        self.bootstrap()
        for hook in ("outbound-o5-prune", "after-commit", "after-push"):
            with self.subTest(hook=hook):
                mail_id = self.fc.send("garden", "hq", f"prune {hook}")
                archive_hash = directory_hash(self.fc.root / "archive")
                self.assertEqual(self.fc.sweep().returncode, 0)
                self.assertEqual(self.trey.sweep().returncode, 0)
                crashed = self.fc.sweep(BRIDGE_CRASH_AFTER=hook)
                self.assertEqual(crashed.returncode, -signal.SIGKILL, crashed.stdout + crashed.stderr)
                # The archive is untouched at the instant of the crash, not
                # only after recovery.
                self.assertEqual(directory_hash(self.fc.root / "archive"), archive_hash)
                recovered = self.fc.sweep()
                self.assertEqual(
                    recovered.returncode, 0, recovered.stdout + recovered.stderr
                )
                self.assertFalse(
                    (
                        self.fc.repo / "outbox" / "trey" / "hq" / (mail_id + ".mail")
                    ).exists()
                )
                self.assertEqual(directory_hash(self.fc.root / "archive"), archive_hash)

    def test_all_marked_archives_are_skipped_before_open(self):
        self.bootstrap()
        cases = []
        for marker_name in ("received", "published", "delivered"):
            mail_id = self.fc.send(
                "garden", "hq", f"{marker_name} archive is inert"
            )
            archive = self.fc.root / "archive" / (mail_id + ".mail")
            if marker_name == "delivered":
                marker = (
                    self.fc.root
                    / "bridge"
                    / "delivered"
                    / "trey"
                    / "hq"
                    / mail_id
                )
            else:
                marker = self.fc.root / "bridge" / marker_name / mail_id
            marker.parent.mkdir(parents=True, exist_ok=True)
            marker.write_text("marked\n", encoding="ascii")
            archive.chmod(0)
            cases.append((mail_id, archive))
        try:
            result = self.fc.sweep()
        finally:
            for _, archive in cases:
                archive.chmod(0o600)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        ignored_ids = {
            record.get("id")
            for record in self.fc.logs()
            if record["action"] == "outbound_ignored"
        }
        for mail_id, _ in cases:
            self.assertFalse(
                (
                    self.fc.repo
                    / "outbox"
                    / "trey"
                    / "hq"
                    / (mail_id + ".mail")
                ).exists()
            )
            self.assertNotIn(mail_id, ignored_ids)

    def test_unrelayable_local_archive_is_visible_but_not_unhealthy(self):
        self.bootstrap()
        body_file = self.fc.base / "oversize-body"
        body_file.write_bytes(b"x" * (1024 * 1024 + 1))
        sent = self.fc.post(
            "send",
            "--to",
            "hq",
            "--body-file",
            body_file,
            "--oversize",
            "--json",
            cwd=self.fc.workspaces["garden"],
        )
        oversize_id = json.loads(sent.stdout)["envelope"]["id"]
        malformed_ids = []
        for sequence in range(700, 721):
            mail_id = fixed_id(sequence)
            malformed_ids.append(mail_id)
            (self.fc.root / "archive" / (mail_id + ".mail")).write_bytes(b"not mail")

        result = self.fc.sweep()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        health = json.loads((self.fc.root / "bridge" / "health.json").read_text())
        self.assertTrue(health["ok"])
        self.assertEqual(health["reason"], "ok")
        self.assertEqual(len(health["outbound_unrelayable"]), 20)
        self.assertTrue(
            set(health["outbound_unrelayable"]).issubset(
                {oversize_id, *malformed_ids}
            )
        )
        ignored = [
            record
            for record in self.fc.logs()
            if record["action"] == "outbound_ignored"
            and record.get("id") in {oversize_id, *malformed_ids}
        ]
        self.assertEqual(len(ignored), 22)
        self.assertTrue(all(record.get("reason") for record in ignored))

    def test_inbound_completes_before_an_outbound_push_failure(self):
        self.bootstrap()
        mail_id = self.fc.send("garden", "hq", "arrives before receipt push fails")
        expected = (self.fc.root / "archive" / (mail_id + ".mail")).read_bytes()
        self.assertEqual(self.fc.sweep().returncode, 0)
        self.trey.git(
            "fetch",
            "origin",
            "+refs/heads/machines/*:refs/remotes/origin/machines/*",
        )
        offline = self.topology.forge.with_name("forge.receipt-push-offline")
        self.topology.forge.rename(offline)
        try:
            result = self.trey.sweep()
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            self.assert_delivery_invariant(self.trey, "hq", mail_id, expected)
            ledger = self.trey.root / "bridge" / "delivered" / "fc" / "hq" / mail_id
            self.assertTrue(ledger.is_file())
            health = json.loads((self.trey.root / "bridge" / "health.json").read_text())
            self.assertFalse(health["ok"])
            self.assertEqual(health["reason"], "push_failed")
        finally:
            offline.rename(self.topology.forge)

    def test_unpushed_held_receipt_push_failure_is_queued_work(self):
        self.bootstrap()
        mail_id = self.fc.send("garden", "hq", "held while forge is down")
        self.assertEqual(self.fc.sweep().returncode, 0)
        self.trey.git(
            "fetch",
            "origin",
            "+refs/heads/machines/*:refs/remotes/origin/machines/*",
        )
        reason = "policy hold stalls until its receipt lands"
        (self.trey.root / "rules.json").write_text(
            json.dumps(
                {"blocked": [{"from": "garden", "to": "hq", "reason": reason}]}
            )
            + "\n",
            encoding="utf-8",
        )
        offline = self.topology.forge.with_name("forge.held-push-offline")
        self.topology.forge.rename(offline)
        try:
            result = self.trey.sweep()
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            health = json.loads(
                (self.trey.root / "bridge" / "health.json").read_text()
            )
            self.assertFalse(health["ok"])
            self.assertEqual(health["reason"], "push_failed")
            receipt = json.loads(
                (
                    self.trey.repo / "receipts" / "fc" / "hq" / (mail_id + ".json")
                ).read_text()
            )
            self.assertEqual((receipt["status"], receipt["reason"]), ("held", reason))
        finally:
            offline.rename(self.topology.forge)

    def test_malformed_rules_stop_batch_before_any_write(self):
        self.bootstrap()
        mail_id = self.fc.send("garden", "hq", "malformed rules")
        self.assertEqual(self.fc.sweep().returncode, 0)
        rules_path = self.trey.root / "rules.json"
        head_before = self.trey.git("rev-parse", "HEAD").stdout.strip()
        for label, payload in (
            ("invalid-json", '{"blocked": [}\n'),
            ("blocked-not-a-list", '{"blocked": {"from": "garden"}}\n'),
        ):
            with self.subTest(case=label):
                rules_path.write_text(payload, encoding="utf-8")
                result = self.trey.sweep()
                self.assertEqual(
                    result.returncode, 1, result.stdout + result.stderr
                )
                health = json.loads(
                    (self.trey.root / "bridge" / "health.json").read_text()
                )
                self.assertFalse(health["ok"])
                self.assertEqual(health["reason"], "rules_invalid")
                self.assertFalse(
                    (self.trey.root / "hq" / "inbox" / (mail_id + ".mail")).exists()
                )
                self.assertFalse(
                    (self.trey.root / "archive" / (mail_id + ".mail")).exists()
                )
                self.assertFalse(
                    (
                        self.trey.root
                        / "bridge"
                        / "delivered"
                        / "fc"
                        / "hq"
                        / mail_id
                    ).exists()
                )
                self.assertFalse(
                    (self.trey.root / "bridge" / "received" / mail_id).exists()
                )
                self.assertFalse(
                    (
                        self.trey.repo
                        / "receipts"
                        / "fc"
                        / "hq"
                        / (mail_id + ".json")
                    ).exists()
                )
                self.assertEqual(
                    self.trey.git("rev-parse", "HEAD").stdout.strip(), head_before
                )

    def test_rule_added_after_delivery_does_not_undeliver(self):
        self.bootstrap()
        mail_id = self.fc.send("garden", "hq", "delivered then rule")
        self.fc.sweep()
        delivered = self.trey.sweep()
        self.assertEqual(delivered.returncode, 0, delivered.stdout + delivered.stderr)
        # hq's participant is live when the letter lands (inbox_ids routes it,
        # as a running session's watch would), so post freezes its routing
        # receipt before the rule exists. A letter nobody routed before the
        # rule appeared is held by post itself; that is post's policy for any
        # unrouted mail, not a bridge undelivery.
        self.assertIn(mail_id, self.trey.inbox_ids("hq"))
        reason = "too late to matter"
        (self.trey.root / "rules.json").write_text(
            json.dumps(
                {"blocked": [{"from": "garden", "to": "hq", "reason": reason}]}
            )
            + "\n",
            encoding="utf-8",
        )
        replayed = self.trey.sweep()
        self.assertEqual(replayed.returncode, 0, replayed.stdout + replayed.stderr)
        receipt = json.loads(
            (
                self.trey.repo / "receipts" / "fc" / "hq" / (mail_id + ".json")
            ).read_text()
        )
        self.assertEqual(receipt["status"], "delivered")
        self.assertIn(mail_id, self.trey.inbox_ids("hq"))
        self.fc.sweep()
        settle = self.fc.sweep()
        self.assertEqual(settle.returncode, 0, settle.stdout + settle.stderr)
        self.assertFalse(
            (self.fc.repo / "outbox" / "trey" / "hq" / (mail_id + ".mail")).exists()
        )

    def test_ledger_replay_repairs_missing_copies_despite_block(self):
        self.bootstrap()
        for sequence, state in ((710, "ledger-only"), (711, "ledger-inbox")):
            with self.subTest(state=state):
                mail_id = fixed_id(sequence)
                data = craft_mail(
                    mail_id, "garden", "hq", body=state.encode("utf-8")
                )
                self.fc.inject(f"outbox/trey/hq/{mail_id}.mail", data)
                sha = hashlib.sha256(data).hexdigest()
                inbox = self.trey.root / "hq" / "inbox" / (mail_id + ".mail")
                read = self.trey.root / "hq" / "read" / (mail_id + ".mail")
                archive = self.trey.root / "archive" / (mail_id + ".mail")
                ledger = (
                    self.trey.root / "bridge" / "delivered" / "fc" / "hq" / mail_id
                )
                for path in (inbox, read, archive, ledger):
                    path.parent.mkdir(parents=True, exist_ok=True)
                if state == "ledger-inbox":
                    inbox.write_bytes(data)
                ledger.write_text(sha + "\n", encoding="ascii")
                (self.trey.root / "rules.json").write_text(
                    json.dumps(
                        {
                            "blocked": [
                                {
                                    "from": "garden",
                                    "to": "hq",
                                    "reason": "blocked but already delivered",
                                }
                            ]
                        }
                    )
                    + "\n",
                    encoding="utf-8",
                )
                result = self.trey.sweep()
                self.assertEqual(
                    result.returncode, 0, result.stdout + result.stderr
                )
                self.assertEqual(archive.read_bytes(), data)
                self.assertEqual(sum(path.exists() for path in (inbox, read)), 1)
                self.assertEqual(ledger.read_text().strip(), sha)
                receipt = json.loads(
                    (
                        self.trey.repo
                        / "receipts"
                        / "fc"
                        / "hq"
                        / (mail_id + ".json")
                    ).read_text()
                )
                self.assertEqual(receipt["status"], "delivered")
                if state == "ledger-only":
                    repaired = {
                        record.get("id")
                        for record in self.trey.logs()
                        if record["action"] == "ledger_only_repaired"
                    }
                    self.assertIn(mail_id, repaired)
                self.assertEqual(self.trey.doctor_errors(), [])
                (self.trey.root / "rules.json").write_text(
                    '{"blocked":[]}\n', encoding="utf-8"
                )

    def test_fence_during_ledger_replay_stops_before_receipt_write(self):
        self.bootstrap()
        mail_id = self.fc.send("garden", "hq", "replay meets fence")
        self.fc.sweep()
        first = self.trey.sweep()
        self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
        receipt_path = self.trey.repo / "receipts" / "fc" / "hq" / (mail_id + ".json")
        receipt_before = receipt_path.read_bytes()
        inbox = self.trey.root / "hq" / "inbox" / (mail_id + ".mail")
        inbox_before = inbox.read_bytes()
        head_before = self.trey.git("rev-parse", "HEAD").stdout.strip()

        def plant_fence_at_boundary(name):
            if name == "inbound-i4":
                (self.trey.root / ".post-arx.json").write_text(
                    '{"state":"fenced","generation":1}\n', encoding="utf-8"
                )

        os.utime(self.trey.root / "bridge" / "config.json", None)
        with mock.patch.dict(
            os.environ, self.trey.env(), clear=True
        ), mock.patch.object(
            SWEEPER, "checkpoint", side_effect=plant_fence_at_boundary
        ):
            returncode = SWEEPER.execute()
        self.assertEqual(returncode, 1)
        health = json.loads(
            (self.trey.root / "bridge" / "health.json").read_text()
        )
        self.assertEqual(health["reason"], "fenced")
        self.assertEqual(receipt_path.read_bytes(), receipt_before)
        self.assertEqual(inbox.read_bytes(), inbox_before)
        self.assertEqual(
            self.trey.git("rev-parse", "HEAD").stdout.strip(), head_before
        )
        fence = self.trey.root / ".post-arx.json"
        fence.unlink()

    def test_reservation_marker_arbitrates_crash_window(self):
        self.bootstrap()
        mail_id = fixed_id(700)
        original = craft_mail(mail_id, "porch", "hq", body=b"mac original")
        rival = craft_mail(mail_id, "garden", "hq", body=b"fc rival bytes")
        self.mac.inject(f"outbox/trey/hq/{mail_id}.mail", original)
        crashed = self.trey.sweep(BRIDGE_CRASH_AFTER="inbound-received")
        self.assertEqual(crashed.returncode, -signal.SIGKILL)
        reservation = self.trey.root / "bridge" / "received" / mail_id
        sha_original = hashlib.sha256(original).hexdigest()
        self.assertEqual(
            reservation.read_text().strip(), f"mac/hq {sha_original}"
        )
        # fc sorts before mac: the colliding host is evaluated first on retry.
        self.fc.inject(f"outbox/trey/hq/{mail_id}.mail", rival)
        result = self.trey.sweep()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        receipts = {
            host: json.loads(
                (
                    self.trey.repo / "receipts" / host / "hq" / (mail_id + ".json")
                ).read_text()
            )
            for host in ("fc", "mac")
        }
        self.assertEqual(receipts["mac"]["status"], "delivered")
        self.assertEqual(
            (receipts["fc"]["status"], receipts["fc"]["reason"]),
            ("quarantined", "id-collision"),
        )
        self.assert_delivery_invariant(self.trey, "hq", mail_id, original)
        ledger = self.trey.root / "bridge" / "delivered" / "mac" / "hq" / mail_id
        self.assertEqual(ledger.read_text().strip(), sha_original)
        self.assertEqual(self.trey.doctor_errors(), [])

    def test_concurrent_busy_probes_increment_streak_exactly(self):
        import fcntl
        import time as time_module

        self.bootstrap()
        lock_path = self.fc.root / "bridge" / ".lock"
        with lock_path.open("r+") as lock:
            fcntl.flock(lock.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
            probe_env = self.fc.env(BRIDGE_TEST_HEALTH_DELAY_MS="800")
            first = subprocess.Popen(
                [sys.executable, SWEEP],
                env=probe_env,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
            time_module.sleep(0.3)
            second = subprocess.Popen(
                [sys.executable, SWEEP],
                env=probe_env,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
            first_out = first.communicate(timeout=60)
            second_out = second.communicate(timeout=60)
        self.assertEqual(first.returncode, 0, first_out)
        self.assertEqual(second.returncode, 0, second_out)
        health = json.loads((self.fc.root / "bridge" / "health.json").read_text())
        self.assertEqual(health["busy_streak"], 2)

    def test_stale_git_lock_removed_at_recovery(self):
        self.bootstrap()
        mail_id = self.fc.send("garden", "hq", "stale index lock")
        self.fc.sweep()
        git_dir = self.trey.repo / ".git"
        (git_dir / "index.lock").write_bytes(b"")
        refs_lock = git_dir / "refs" / "heads" / "machines" / "trey.lock"
        refs_lock.write_bytes(b"")
        result = self.trey.sweep()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse((git_dir / "index.lock").exists())
        self.assertFalse(refs_lock.exists())
        actions = [record["action"] for record in self.trey.logs()]
        self.assertEqual(actions.count("git_lock_removed"), 2)
        self.assertIn(mail_id, self.trey.inbox_ids("hq"))

    def test_exit2_writes_only_bridge_health(self):
        self.bootstrap()
        config_path = self.fc.root / "bridge" / "config.json"
        valid = config_path.read_text()
        duplicate = valid.replace('"host": "fc"', '"host": "fc", "host": "fc"')
        config_path.write_text(duplicate, encoding="utf-8")

        def snapshot():
            entries = {}
            for item in sorted(self.fc.root.rglob("*")):
                relative = str(item.relative_to(self.fc.root))
                if item.is_file() and not item.is_symlink():
                    metadata = item.stat()
                    entries[relative] = (
                        metadata.st_mtime_ns,
                        metadata.st_mode,
                    )
                else:
                    entries[relative] = None
            return entries

        lock = self.fc.root / "bridge" / ".health.lock"
        lock.unlink()
        before = snapshot()
        result = self.fc.sweep()
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        after = snapshot()
        self.assertEqual(sorted(after), sorted(before))
        changed = {name for name in after if after[name] != before[name]}
        self.assertEqual(changed, {"bridge/health.json"})
        self.assertFalse(lock.exists())
        record = json.loads(result.stdout.strip().splitlines()[-1])
        self.assertEqual(record["action"], "config_error")

    def test_uncaught_exception_records_internal_error(self):
        self.bootstrap()
        self.fc.send("garden", "hq", "internal error probe")
        self.fc.sweep()
        result = self.trey.sweep(BRIDGE_RAISE_AFTER="inbound-i6")
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertNotIn("Traceback", result.stderr)
        health = json.loads((self.trey.root / "bridge" / "health.json").read_text())
        self.assertFalse(health["ok"])
        self.assertEqual(health["reason"], "internal_error")
        actions = [
            (record["action"], record.get("class")) for record in self.trey.logs()
        ]
        self.assertIn(("internal_error", "OSError"), actions)

    def test_missing_health_lock_busy_probe_reports_internal_error(self):
        import fcntl

        self.bootstrap()
        lock_path = self.fc.root / "bridge" / ".health.lock"
        lock_path.unlink()
        with (self.fc.root / "bridge" / ".lock").open("r+") as held:
            fcntl.flock(held.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
            result = self.fc.sweep()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertNotIn("Traceback", result.stderr)
        record = json.loads(result.stdout.strip().splitlines()[-1])
        self.assertEqual(record["action"], "internal_error")
        self.assertIn("health_unwritten", record)
        actions = [
            (record["action"], record.get("class")) for record in self.fc.logs()
        ]
        self.assertIn(("internal_error", "FileNotFoundError"), actions)
        # The sweeper never recreates the installer-owned lock file.
        self.assertFalse(lock_path.exists())

    def test_missing_health_lock_live_tick_reports_internal_error(self):
        self.bootstrap()
        self.fc.send("garden", "hq", "missing lock live tick")
        lock_path = self.fc.root / "bridge" / ".health.lock"
        lock_path.unlink()
        result = self.fc.sweep()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertNotIn("Traceback", result.stderr)
        record = json.loads(result.stdout.strip().splitlines()[-1])
        self.assertEqual(record["action"], "internal_error")
        self.assertIn("health_unwritten", record)
        actions = [
            (record["action"], record.get("class")) for record in self.fc.logs()
        ]
        self.assertIn(("internal_error", "FileNotFoundError"), actions)
        self.assertFalse(lock_path.exists())

    def test_missing_health_lock_tick_error_reports_internal_error(self):
        # C1, TickError branch: an operational failure (foreign_state) whose
        # health write then fails must emit the same terminal
        # {action, health_unwritten, class} record as the catch-all.
        self.bootstrap()
        lock_path = self.fc.root / "bridge" / ".health.lock"
        lock_path.unlink()
        marker = self.fc.repo / ".git" / "rebase-apply"
        marker.mkdir()
        try:
            result = self.fc.sweep()
        finally:
            shutil.rmtree(marker, ignore_errors=True)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertNotIn("Traceback", result.stderr)
        record = json.loads(result.stdout.strip().splitlines()[-1])
        self.assertEqual(record["action"], "internal_error")
        self.assertEqual(record["class"], "FileNotFoundError")
        self.assertIn("health_unwritten", record)
        # The operational event was still logged before the terminal record.
        self.assertIn("foreign_state", [r["action"] for r in self.fc.logs()])
        self.assertFalse(lock_path.exists())

    def test_health_write_carries_state_read_after_lock(self):
        import fcntl
        import time as time_module

        self.bootstrap()
        health_path = self.fc.root / "bridge" / "health.json"
        self.assertEqual(self.fc.sweep().returncode, 0)
        sentinel = "sentinel-carried-under-lock"
        baseline = len(self.fc.logs())

        def plant_sentinel():
            value = json.loads(health_path.read_text(encoding="utf-8"))
            value["last_fetch_ok"] = sentinel
            health_path.write_text(
                json.dumps(value, sort_keys=True) + "\n", encoding="utf-8"
            )

        # A quiet offline tick refreshes neither last_fetch_ok nor any count,
        # so the writer must carry them from its locked read. Hold
        # .health.lock so the tick blocks at that write, swap last_fetch_ok
        # while it waits, then release: the written health must carry the
        # sentinel, proving the carried value was read after the lock was
        # taken, not from state gathered before it.
        offline = self.topology.forge.with_name("forge.carry-lock-offline")
        self.topology.forge.rename(offline)
        try:
            with (self.fc.root / "bridge" / ".health.lock").open("r+") as held:
                fcntl.flock(held.fileno(), fcntl.LOCK_EX)
                tick = subprocess.Popen(
                    [sys.executable, SWEEP],
                    env=self.fc.env(),
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                )
                try:
                    deadline = time_module.monotonic() + 30
                    while time_module.monotonic() < deadline:
                        records = self.fc.logs()
                        if (
                            len(records) > baseline
                            and records[-1]["action"] == "fetch"
                            and records[-1].get("ok") is False
                        ):
                            break
                        if tick.poll() is not None:
                            break
                        time_module.sleep(0.05)
                    plant_sentinel()
                finally:
                    fcntl.flock(held.fileno(), fcntl.LOCK_UN)
            out = tick.communicate(timeout=60)
            self.assertEqual(tick.returncode, 0, out)
            final = json.loads(health_path.read_text(encoding="utf-8"))
            self.assertEqual(final["last_fetch_ok"], sentinel)
            # The fresh node anchor keeps the partition inside grace: the
            # carried stale stamp does not by itself make health false.
            self.assertTrue(final["ok"])
        finally:
            offline.rename(self.topology.forge)

    def test_pre_existing_inbox_keeps_mode_across_tick(self):
        self.bootstrap()
        mail_id = self.fc.send("garden", "hq", "mode preservation")
        self.fc.sweep()
        inbox_dir = self.trey.root / "hq" / "inbox"
        # post creates a room's mailbox only when it writes mail there, so a
        # pre-existing inbox is made here.
        inbox_dir.mkdir(parents=True, exist_ok=True)
        inbox_dir.chmod(0o750)
        archive_dir = self.trey.root / "archive"
        archive_dir.chmod(0o751)
        result = self.trey.sweep()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(inbox_dir.stat().st_mode & 0o777, 0o750)
        self.assertEqual(archive_dir.stat().st_mode & 0o777, 0o751)
        self.assertIn(mail_id, self.trey.inbox_ids("hq"))

    def test_busy_lock_exits_zero_and_fifth_busy_is_unhealthy(self):
        self.bootstrap()
        lock_path = self.fc.root / "bridge" / ".lock"
        with lock_path.open("r+") as lock:
            import fcntl

            fcntl.flock(lock.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
            results = [self.fc.sweep() for _ in range(5)]
        self.assertEqual([result.returncode for result in results[:4]], [0, 0, 0, 0])
        self.assertEqual(results[4].returncode, 1)
        health = json.loads((self.fc.root / "bridge" / "health.json").read_text())
        self.assertEqual(health["reason"], "busy_streak")

    def test_busy_tick_preserves_existing_unhealthy_reason(self):
        self.bootstrap()
        fence = self.fc.root / ".post-arx.json"
        fence.write_text('{"state":"fenced","generation":1}\n', encoding="utf-8")
        fenced = self.fc.sweep()
        self.assertEqual(fenced.returncode, 1, fenced.stdout + fenced.stderr)
        before = json.loads((self.fc.root / "bridge" / "health.json").read_text())
        fence.unlink()
        lock_path = self.fc.root / "bridge" / ".lock"
        with lock_path.open("r+") as lock:
            import fcntl

            fcntl.flock(lock.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
            busy = self.fc.sweep()
        self.assertEqual(busy.returncode, 1, busy.stdout + busy.stderr)
        after = json.loads((self.fc.root / "bridge" / "health.json").read_text())
        self.assertFalse(after["ok"])
        self.assertEqual(after["reason"], "fenced")
        self.assertEqual(after["stalled_since"], before["stalled_since"])
        self.assertEqual(after["busy_streak"], before["busy_streak"] + 1)

    def test_wrong_origin_or_branch_is_fatal(self):
        config_path = self.fc.root / "bridge" / "config.json"
        config = json.loads(config_path.read_text())
        config["relay_url"] = str(self.fc.base / "wrong.git")
        config_path.write_text(json.dumps(config), encoding="utf-8")
        origin = self.fc.git("remote", "get-url", "origin").stdout.strip()
        result = self.fc.sweep()
        self.assertEqual(result.returncode, 2)
        config["relay_url"] = origin
        config_path.write_text(json.dumps(config), encoding="utf-8")
        self.fc.git("checkout", "-q", "-b", "wrong-branch")
        result = self.fc.sweep()
        self.assertEqual(result.returncode, 2)

    def test_reconcile_repairs_every_local_half_state(self):
        self.bootstrap()
        states = (
            "inbox-only",
            "read-only",
            "archive-only",
            "ledger-only",
            "ledger-archive",
            "ledger-inbox",
            "all-without-ledger",
        )
        for sequence, state_name in enumerate(states, 100):
            with self.subTest(state=state_name):
                mail_id = fixed_id(sequence)
                data = craft_mail(
                    mail_id, "garden", "hq", body=state_name.encode("utf-8")
                )
                self.fc.inject(f"outbox/trey/hq/{mail_id}.mail", data)
                inbox = self.trey.root / "hq" / "inbox" / (mail_id + ".mail")
                read = self.trey.root / "hq" / "read" / (mail_id + ".mail")
                archive = self.trey.root / "archive" / (mail_id + ".mail")
                ledger = self.trey.root / "bridge" / "delivered" / "fc" / "hq" / mail_id
                sha = hashlib.sha256(data).hexdigest()
                for path in (inbox, read, archive, ledger):
                    path.parent.mkdir(parents=True, exist_ok=True)
                if state_name in ("inbox-only", "ledger-inbox", "all-without-ledger"):
                    inbox.write_bytes(data)
                if state_name == "read-only":
                    read.write_bytes(data)
                if state_name in (
                    "archive-only",
                    "ledger-archive",
                    "all-without-ledger",
                ):
                    archive.write_bytes(data)
                if state_name in ("ledger-only", "ledger-archive", "ledger-inbox"):
                    ledger.write_text(sha + "\n", encoding="ascii")
                result = self.trey.sweep()
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(archive.read_bytes(), data)
                self.assertEqual(ledger.read_text().strip(), sha)
                if state_name in ("archive-only", "ledger-archive"):
                    self.assertFalse(inbox.exists())
                    self.assertFalse(read.exists())
                    archive_only_ids = {
                        record.get("id")
                        for record in self.trey.logs()
                        if record["action"] == "archive-only"
                    }
                    self.assertIn(mail_id, archive_only_ids)
                else:
                    self.assertEqual(sum(path.exists() for path in (inbox, read)), 1)
                self.assertEqual(self.trey.doctor_errors(), [])

    def test_same_id_from_another_host_or_room_is_quarantined(self):
        self.bootstrap()
        shared_id = fixed_id(200)
        first = craft_mail(shared_id, "garden", "hq", body=b"from fc")
        second = craft_mail(shared_id, "porch", "hq", body=b"from mac")
        self.fc.inject(f"outbox/trey/hq/{shared_id}.mail", first)
        self.mac.inject(f"outbox/trey/hq/{shared_id}.mail", second)
        result = self.trey.sweep()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        receipts = {
            host: json.loads(
                (
                    self.trey.repo / "receipts" / host / "hq" / (shared_id + ".json")
                ).read_text()
            )
            for host in ("fc", "mac")
        }
        self.assertEqual(receipts["fc"]["status"], "delivered")
        self.assertEqual(
            (receipts["mac"]["status"], receipts["mac"]["reason"]),
            ("quarantined", "id-collision"),
        )

        room_id = fixed_id(201)
        to_hq = craft_mail(room_id, "garden", "hq", body=b"hq")
        to_atlas = craft_mail(room_id, "garden", "atlasos", body=b"atlas")
        self.fc.inject(f"outbox/trey/hq/{room_id}.mail", to_hq)
        self.fc.inject(f"outbox/trey/atlasos/{room_id}.mail", to_atlas)
        result = self.trey.sweep()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        room_receipts = {
            room: json.loads(
                (
                    self.trey.repo / "receipts" / "fc" / room / (room_id + ".json")
                ).read_text()
            )
            for room in ("hq", "atlasos")
        }
        self.assertEqual(room_receipts["atlasos"]["status"], "delivered")
        self.assertEqual(
            (room_receipts["hq"]["status"], room_receipts["hq"]["reason"]),
            ("quarantined", "id-collision"),
        )

    def test_cross_host_id_collision_rejects_identical_and_different_bytes(self):
        # The identical-bytes half needs one envelope accepted from two
        # different hosts, which only a free-form sender allows, so both
        # sending peers stay legacy (SPEC-v2 §Unpublished senders).
        self.bootstrap(machines=(self.trey,))
        for sequence, identical in ((210, True), (211, False)):
            with self.subTest(identical=identical):
                mail_id = fixed_id(sequence)
                first = craft_mail(mail_id, "outside", "hq", body=b"first host")
                second = (
                    first
                    if identical
                    else craft_mail(mail_id, "outside", "hq", body=b"second host")
                )
                self.fc.inject(f"outbox/trey/hq/{mail_id}.mail", first)
                first_tick = self.trey.sweep()
                self.assertEqual(
                    first_tick.returncode, 0, first_tick.stdout + first_tick.stderr
                )
                ledger = self.trey.root / "bridge" / "delivered" / "fc" / "hq" / mail_id
                store_before = {
                    "ledger": ledger.read_bytes(),
                    "archive": (
                        self.trey.root / "archive" / (mail_id + ".mail")
                    ).read_bytes(),
                }
                self.mac.inject(f"outbox/trey/hq/{mail_id}.mail", second)
                second_tick = self.trey.sweep()
                self.assertEqual(
                    second_tick.returncode, 0, second_tick.stdout + second_tick.stderr
                )
                receipt = json.loads(
                    (
                        self.trey.repo
                        / "receipts"
                        / "mac"
                        / "hq"
                        / (mail_id + ".json")
                    ).read_text()
                )
                self.assertEqual(
                    (receipt["status"], receipt["reason"]),
                    ("quarantined", "id-collision"),
                )
                self.assertEqual(ledger.read_bytes(), store_before["ledger"])
                self.assertEqual(
                    (self.trey.root / "archive" / (mail_id + ".mail")).read_bytes(),
                    store_before["archive"],
                )

    def test_local_origin_id_with_different_peer_bytes_is_quarantined(self):
        self.bootstrap()
        mail_id = self.fc.send("garden", "garden", "local origin", allow_self=True)
        self.assertFalse(
            (
                self.fc.root / "bridge" / "delivered" / "mac" / "garden" / mail_id
            ).exists()
        )
        local_archive = self.fc.root / "archive" / (mail_id + ".mail")
        expected = local_archive.read_bytes()
        # mac has published rooms.json, so its sender must be a name it
        # published for binding to pass and the id-collision rule to be the
        # thing under test (SPEC-v2 §Unpublished senders).
        hostile = craft_mail(mail_id, "porch", "garden", body=b"different peer bytes")
        self.mac.inject(f"outbox/fc/garden/{mail_id}.mail", hostile)
        result = self.fc.sweep()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        receipt = json.loads(
            (
                self.fc.repo / "receipts" / "mac" / "garden" / (mail_id + ".json")
            ).read_text()
        )
        self.assertEqual(
            (receipt["status"], receipt["reason"]),
            ("quarantined", "id-collision"),
        )
        self.assertEqual(local_archive.read_bytes(), expected)

    def test_unread_object_to_an_unknown_room_keeps_its_own_reason(self):
        """SPEC-v2 §What this changes in v1 mail: ordering of the v1 checks.

        The unknown_room receipt is written "after the v1 mode, size, and
        readability checks, so an unread object keeps its descriptor sha and
        its own reason". Both existing unknown_room tests use a well-formed,
        in-size letter, so both would still pass if the room check were
        hoisted above the mode and size checks.
        """
        self.bootstrap()
        oversize_id = fixed_id(640)
        symlink_id = fixed_id(641)
        self.fc.inject(
            f"outbox/trey/nowhere/{oversize_id}.mail",
            craft_mail(oversize_id, "garden", "nowhere", body=b"z" * 2048),
        )
        self.fc.inject(
            f"outbox/trey/nowhere/{symlink_id}.mail", b"/etc/passwd", mode="symlink"
        )

        result = self.trey.sweep(BRIDGE_MAX_MAIL_BYTES="1024")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

        for mail_id, reason in (
            (oversize_id, "oversize-unread"),
            (symlink_id, "non-regular-object"),
        ):
            with self.subTest(reason=reason):
                relative = f"outbox/trey/nowhere/{mail_id}.mail"
                fields = self.fc.git("ls-tree", "HEAD", "--", relative).stdout.split()
                descriptor = f"fc/{relative}@{fields[2]}".encode()
                receipt = json.loads(
                    (
                        self.trey.repo
                        / "receipts"
                        / "fc"
                        / "nowhere"
                        / (mail_id + ".json")
                    ).read_text()
                )
                self.assertEqual(receipt["status"], "quarantined")
                self.assertEqual(receipt["reason"], reason)
                self.assertEqual(
                    receipt["sha256"], hashlib.sha256(descriptor).hexdigest()
                )

    def test_hostile_receipts_never_prune_sender_outbox(self):
        self.bootstrap()
        attacks = ("wrong-sha", "wrong-host", "symlink", "malformed-json")
        for sequence, attack in enumerate(attacks, 300):
            with self.subTest(attack=attack):
                mail_id = self.fc.send("garden", "hq", attack)
                self.assertEqual(self.fc.sweep().returncode, 0)
                outbox = self.fc.repo / "outbox" / "trey" / "hq" / (mail_id + ".mail")
                sha = hashlib.sha256(outbox.read_bytes()).hexdigest()
                value = {
                    "v": 1,
                    "status": "delivered",
                    "host": "fc",
                    "room": "hq",
                    "id": mail_id,
                    "sha256": sha,
                    "reason": "",
                    "at": "2026-08-23T05:00:00+00:00",
                }
                if attack == "wrong-sha":
                    value["sha256"] = "0" * 64
                elif attack == "wrong-host":
                    value["host"] = "mac"
                path = f"receipts/fc/hq/{mail_id}.json"
                if attack == "symlink":
                    self.trey.inject(path, b"/etc/passwd", mode="symlink")
                elif attack == "malformed-json":
                    self.trey.inject(path, b"{not-json\n")
                else:
                    self.trey.inject(path, (json.dumps(value) + "\n").encode("utf-8"))
                result = self.fc.sweep()
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertTrue(outbox.exists())
                self.assertTrue(
                    any(
                        record["action"] == "receipt_ignored"
                        and record.get("id") == mail_id
                        for record in self.fc.logs()
                    )
                )

    def test_receipt_informational_at_still_prunes(self):
        self.bootstrap()
        mail_id = self.fc.send("garden", "hq", "opaque receipt timestamp")
        self.assertEqual(self.fc.sweep().returncode, 0)
        self.assertEqual(self.trey.sweep().returncode, 0)
        relative = f"receipts/fc/hq/{mail_id}.json"
        receipt = json.loads((self.trey.repo / relative).read_text())
        self.assertEqual(receipt["status"], "delivered")
        # SPEC: `at` is informational and never parsed — any string admits
        # the receipt for pruning.
        receipt["at"] = "whenever"
        self.trey.inject(relative, (json.dumps(receipt) + "\n").encode("utf-8"))
        result = self.fc.sweep()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(
            (self.fc.repo / "outbox" / "trey" / "hq" / (mail_id + ".mail")).exists()
        )

    def test_unreadable_receipt_is_ignored_without_aborting_tick(self):
        self.bootstrap()
        mail_id = self.fc.send("garden", "hq", "unreadable receipt")
        self.assertEqual(self.fc.sweep().returncode, 0)
        outbox = self.fc.repo / "outbox" / "trey" / "hq" / (mail_id + ".mail")
        sha256 = hashlib.sha256(outbox.read_bytes()).hexdigest()
        receipt_path = f"receipts/fc/hq/{mail_id}.json"
        receipt = {
            "v": 1,
            "status": "delivered",
            "host": "fc",
            "room": "hq",
            "id": mail_id,
            "sha256": sha256,
            "reason": "",
            "at": "2026-08-23T05:00:00+00:00",
        }
        object_ids = install_loose_peer_tree(
            self.topology,
            self.fc,
            "trey",
            {receipt_path: (json.dumps(receipt) + "\n").encode()},
        )
        receipt_object = (
            self.fc.repo
            / ".git"
            / "objects"
            / object_ids[receipt_path][:2]
            / object_ids[receipt_path][2:]
        )
        wrapper_dir = self.fc.base / "unreadable-receipt-wrapper"
        wrapper_dir.mkdir()
        removed = wrapper_dir / "removed"
        wrapper = wrapper_dir / "git"
        wrapper.write_text(
            "#!/bin/sh\n"
            "set -eu\n"
            "is_target=0\n"
            "has_path=0\n"
            "for arg do\n"
            '  [ "$arg" = ls-tree ] && is_target=1\n'
            f'  [ "$arg" = {receipt_path!r} ] && has_path=1\n'
            "done\n"
            f'if [ "$is_target" = 1 ] && [ "$has_path" = 1 ] && [ ! -f {str(removed)!r} ]; then\n'
            f"  {GIT_PATH!r} \"$@\"\n"
            "  status=$?\n"
            f"  rm -f -- {str(receipt_object)!r}\n"
            f"  : > {str(removed)!r}\n"
            '  exit "$status"\n'
            "fi\n"
            f"exec {GIT_PATH!r} \"$@\"\n",
            encoding="utf-8",
        )
        wrapper.chmod(0o755)

        result = self.fc.sweep(
            PATH=f"{wrapper_dir}{os.pathsep}{os.environ['PATH']}"
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        # The wrapper must have fired, or the test proves nothing.
        self.assertTrue(removed.exists())
        self.assertFalse(receipt_object.exists())
        self.assertTrue(outbox.exists())
        self.assertTrue(
            any(
                record["action"] == "receipt_ignored"
                and record.get("id") == mail_id
                and record.get("reason") == "unreadable-object"
                for record in self.fc.logs()
            )
        )

    def test_fetch_grace_marks_pending_inbound_outage_stale_after_grace(self):
        self.bootstrap()
        self.fc.send("garden", "hq", "pending beyond unreachable forge")
        self.assertEqual(self.fc.sweep().returncode, 0)
        offline = self.topology.forge.with_name("forge.offline")
        self.topology.forge.rename(offline)
        try:
            within = self.trey.sweep(BRIDGE_FETCH_GRACE_SECONDS="600")
            self.assertEqual(within.returncode, 0, within.stdout + within.stderr)
            health_path = self.trey.root / "bridge" / "health.json"
            health = json.loads(health_path.read_text())
            health["last_fetch_ok"] = "2000-01-01T00:00:00+00:00"
            health_path.write_text(json.dumps(health) + "\n", encoding="utf-8")
            stale = self.trey.sweep(BRIDGE_FETCH_GRACE_SECONDS="1")
            self.assertEqual(stale.returncode, 1, stale.stdout + stale.stderr)
            self.assertEqual(
                json.loads(health_path.read_text())["reason"], "fetch_stale"
            )
        finally:
            offline.rename(self.topology.forge)

    def test_fetch_grace_uses_first_tick_for_never_fetched_idle_node(self):
        (self.trey.repo / "rooms.json").write_text(
            json.dumps(
                {"v": 1, "host": "trey", "rooms": ["atlasos", "hq"]},
                sort_keys=True,
            )
            + "\n",
            encoding="utf-8",
        )
        self.trey.git("add", "--", "rooms.json")
        self.trey.git("commit", "-q", "-m", "seed v2 rooms publication")
        self.trey.git(
            "push", "-q", "origin", "HEAD:refs/heads/machines/trey"
        )
        self.trey.git(
            "update-ref",
            "refs/remotes/origin/machines/trey",
            self.trey.git("rev-parse", "HEAD").stdout.strip(),
        )
        offline = self.topology.forge.with_name("forge.never-fetched-offline")
        self.topology.forge.rename(offline)
        try:
            fresh = self.trey.sweep(BRIDGE_FETCH_GRACE_SECONDS="600")
            self.assertEqual(fresh.returncode, 0, fresh.stdout + fresh.stderr)
            health_path = self.trey.root / "bridge" / "health.json"
            health = json.loads(health_path.read_text())
            self.assertTrue(health["ok"])
            self.assertEqual(health["reason"], "ok")
            self.assertIsNone(health["last_fetch_ok"])
            self.assertIsInstance(health["first_tick_at"], str)

            health["first_tick_at"] = "2000-01-01T00:00:00+00:00"
            health_path.write_text(json.dumps(health) + "\n", encoding="utf-8")
            stale = self.trey.sweep(BRIDGE_FETCH_GRACE_SECONDS="1")
            self.assertEqual(stale.returncode, 1, stale.stdout + stale.stderr)
            self.assertEqual(
                json.loads(health_path.read_text())["reason"], "fetch_stale"
            )
        finally:
            offline.rename(self.topology.forge)

    def test_never_fetched_node_with_an_unreachable_forge_reports_push_failed(self):
        """The r5.3 rooms.json publication is queued work on the first tick.

        SPEC-v2 r5.3 §Health: "Unpushed channel files and an unpushed
        rooms.json are queued work exactly like unpushed outbox entries."
        Every v2 node therefore generates a publication on its very first
        tick, so a fresh install whose forge is unreachable is unhealthy for
        push_failed rather than taking the §Health fetch-grace path a v1 node
        with no mail would have taken. That behavior change was what the
        `test_fetch_grace_uses_first_tick_for_never_fetched_idle_node` fixture
        was seeded to step around, and nothing covered it (review G6).
        """
        offline = self.topology.forge.with_name("forge.g6-offline")
        self.topology.forge.rename(offline)
        try:
            first = self.trey.sweep(BRIDGE_FETCH_GRACE_SECONDS="600")
            self.assertEqual(first.returncode, 1, first.stdout + first.stderr)
            health_path = self.trey.root / "bridge" / "health.json"
            health = json.loads(health_path.read_text())
            self.assertFalse(health["ok"])
            self.assertEqual(health["reason"], "push_failed")
            self.assertIsNone(
                health["last_fetch_ok"], "the node has never fetched"
            )
            self.assertTrue((self.trey.repo / "rooms.json").is_file())
            failures = [
                record
                for record in self.trey.logs()
                if record["action"] == "push_failed"
            ]
            self.assertTrue(failures)
            self.assertTrue(
                any(record.get("queued_work") is True for record in failures),
                f"the unpushed rooms.json publication is the queued work: {failures}",
            )
        finally:
            offline.rename(self.topology.forge)

        recovered = self.trey.sweep(BRIDGE_FETCH_GRACE_SECONDS="600")
        self.assertEqual(recovered.returncode, 0, recovered.stdout + recovered.stderr)
        health = json.loads(
            (self.trey.root / "bridge" / "health.json").read_text()
        )
        self.assertTrue(health["ok"])
        self.assertEqual(health["reason"], "ok")
        self.assertEqual(
            json.loads(
                self.trey.git(
                    "show", "origin/machines/trey:rooms.json"
                ).stdout
            )["rooms"],
            ["atlasos", "hq"],
        )

    def test_config_error_preserves_fetch_grace_anchor(self):
        self.bootstrap()
        health_path = self.trey.root / "bridge" / "health.json"
        health = json.loads(health_path.read_text())
        recent_fetch = health["last_fetch_ok"]
        recent_push = health["last_push_ok"]
        health["first_tick_at"] = "2000-01-01T00:00:00+00:00"
        health_path.write_text(json.dumps(health) + "\n", encoding="utf-8")

        config_path = self.trey.root / "bridge" / "config.json"
        valid_config = config_path.read_text(encoding="utf-8")
        config_path.write_text("not json\n", encoding="utf-8")
        invalid = self.trey.sweep()
        self.assertEqual(invalid.returncode, 2, invalid.stdout + invalid.stderr)
        fatal_health = json.loads(health_path.read_text())
        self.assertEqual(fatal_health["last_fetch_ok"], recent_fetch)
        self.assertEqual(fatal_health["last_push_ok"], recent_push)
        config_path.write_text(valid_config, encoding="utf-8")

        offline = self.topology.forge.with_name("forge.config-recovery-offline")
        self.topology.forge.rename(offline)
        try:
            recovered = self.trey.sweep(BRIDGE_FETCH_GRACE_SECONDS="600")
            self.assertEqual(
                recovered.returncode, 0, recovered.stdout + recovered.stderr
            )
            recovered_health = json.loads(health_path.read_text())
            self.assertTrue(recovered_health["ok"])
            self.assertEqual(recovered_health["reason"], "ok")
        finally:
            offline.rename(self.topology.forge)

    def test_foreign_git_state_stops_recovery_untouched(self):
        self.bootstrap()
        mail_id = fixed_id(400)
        data = craft_mail(mail_id, "garden", "hq")
        outbox = self.fc.repo / "outbox" / "trey" / "hq" / (mail_id + ".mail")
        outbox.parent.mkdir(parents=True, exist_ok=True)
        outbox.write_bytes(data)
        git_dir = self.fc.repo / ".git"
        head_before = self.fc.git("rev-parse", "HEAD").stdout.strip()
        for marker in ("rebase-merge", "rebase-apply", "MERGE_HEAD"):
            with self.subTest(marker=marker):
                path = git_dir / marker
                if marker == "rebase-merge":
                    path.mkdir()
                    (path / "head-name").write_text(
                        "refs/heads/machines/fc\n", encoding="ascii"
                    )
                elif marker == "rebase-apply":
                    # `git rebase --apply` / `git am` state is a directory.
                    path.mkdir()
                    (path / "head").write_text(
                        head_before + "\n", encoding="ascii"
                    )
                else:
                    path.write_text(head_before + "\n", encoding="ascii")
                try:
                    result = self.fc.sweep()
                    self.assertEqual(
                        result.returncode, 1, result.stdout + result.stderr
                    )
                    health = json.loads(
                        (self.fc.root / "bridge" / "health.json").read_text()
                    )
                    self.assertEqual(health["reason"], "foreign_state")
                    self.assertTrue(path.exists())
                    self.assertEqual(
                        self.fc.git("rev-parse", "HEAD").stdout.strip(), head_before
                    )
                    remote_paths = self.fc.git(
                        "ls-tree", "-r", "--name-only", "origin/machines/fc"
                    ).stdout
                    self.assertNotIn(
                        f"outbox/trey/hq/{mail_id}.mail", remote_paths
                    )
                finally:
                    if marker in ("rebase-merge", "rebase-apply"):
                        shutil.rmtree(path, ignore_errors=True)
                    elif path.exists():
                        path.unlink()

    def test_recovery_moves_stray_directory_without_following_symlinks(self):
        self.bootstrap()
        outside = self.fc.base / "outside-repo"
        outside.mkdir()
        (outside / "secret").write_text("must not be copied\n", encoding="utf-8")
        stray = self.fc.repo / "unexpected-directory"
        stray.mkdir()
        (stray / "outside-link").symlink_to(outside, target_is_directory=True)
        result = self.fc.sweep()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        moved = list(
            (self.fc.root / "bridge" / "stray").glob("*-unexpected-directory")
        )
        self.assertEqual(len(moved), 1, moved)
        copied_link = moved[0] / "outside-link"
        self.assertTrue(copied_link.is_symlink())
        self.assertEqual(Path(os.readlink(str(copied_link))), outside)

    def test_tracked_stray_converges_off_the_branch(self):
        # post-aqw.12: machines/mac tracked the relay's Initial README. The
        # stray pass moved the file but left it in the index, so the branch
        # kept it and every reinstall that restored it was moved again.
        self.bootstrap()
        self.fc.inject("README.md", b"relay initial readme\n")
        self.fc.inject("NOTES.md", b"tracked, then moved by hand\n")
        (self.fc.repo / "NOTES.md").unlink()  # an earlier tick moved it
        (self.fc.repo / "scratch.txt").write_text("untracked\n", encoding="utf-8")
        stray_root = self.fc.root / "bridge" / "stray"
        first = self.fc.sweep()
        self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
        head_names = self.fc.git("ls-tree", "--name-only", "HEAD").stdout.split()
        remote_names = self.fc.git(
            "ls-tree", "--name-only", "origin/machines/fc"
        ).stdout.split()
        for names in (head_names, remote_names):
            self.assertNotIn("README.md", names)
            self.assertNotIn("NOTES.md", names)
        self.assertEqual(self.fc.git("status", "--porcelain").stdout, "")
        self.assertEqual(len(list(stray_root.glob("*-README.md"))), 1)
        self.assertEqual(len(list(stray_root.glob("*-scratch.txt"))), 1)
        moved = [r for r in self.fc.logs() if r["action"] == "stray_moved"]
        self.assertEqual(sorted(r["path"] for r in moved), ["README.md", "scratch.txt"])
        second = self.fc.sweep()
        self.assertEqual(second.returncode, 0, second.stdout + second.stderr)
        self.assertEqual(
            len([r for r in self.fc.logs() if r["action"] == "stray_moved"]), 2
        )
        self.assertEqual(len(list(stray_root.glob("*-README.md"))), 1)
        self.assertEqual(self.fc.git("status", "--porcelain").stdout, "")

    def test_stray_untrack_failure_is_logged_not_fatal(self):
        # Review fix3 nit 6: a failing `git rm --cached` logs and the tick
        # goes on; the next tick converges.
        self.bootstrap()
        self.fc.inject("README.md", b"relay initial readme\n")
        wrapper_dir = self.fc.base / "failing-rm-wrapper"
        wrapper_dir.mkdir()
        fired = wrapper_dir / "fired"
        (wrapper_dir / "git").write_text(
            "#!/bin/sh\n"
            "for arg do\n"
            '  if [ "$arg" = rm ]; then\n'
            f"    : > {str(fired)!r}\n"
            "    echo 'fatal: simulated index failure' >&2\n"
            "    exit 128\n"
            "  fi\n"
            "done\n"
            f"exec {GIT_PATH!r} \"$@\"\n",
            encoding="utf-8",
        )
        (wrapper_dir / "git").chmod(0o755)
        logged = len(self.fc.logs())
        failed = self.fc.sweep(PATH=f"{wrapper_dir}{os.pathsep}{os.environ['PATH']}")
        self.assertTrue(fired.exists(), "wrapper never fired")
        self.assertEqual(failed.returncode, 0, failed.stdout + failed.stderr)
        records = self.fc.logs()[logged:]
        actions = [r["action"] for r in records]
        self.assertNotIn("git_failed", actions)
        failures = [r for r in records if r["action"] == "stray_untrack_failed"]
        self.assertEqual([r["path"] for r in failures], ["README.md"])
        self.assertIn("simulated index failure", failures[0]["error"])
        self.assertIn("README.md", self.fc.git("ls-tree", "--name-only", "HEAD").stdout.split())
        converged = self.fc.sweep()
        self.assertEqual(converged.returncode, 0, converged.stdout + converged.stderr)
        self.assertNotIn("README.md", self.fc.git("ls-tree", "--name-only", "HEAD").stdout.split())

    def test_diverged_branch_stops_without_rebase_or_force(self):
        self.bootstrap()
        self.fc.send("garden", "hq", "local ahead")
        crashed = self.fc.sweep(BRIDGE_CRASH_AFTER="after-commit")
        self.assertEqual(crashed.returncode, -signal.SIGKILL)
        rival = self.fc.base / "rival"
        run(
            [
                "git",
                "clone",
                "-q",
                "--branch",
                "machines/fc",
                self.topology.forge,
                rival,
            ]
        )
        run(["git", "-C", rival, "config", "user.name", "fc rival"])
        run(["git", "-C", rival, "config", "user.email", "fc@post-bridge.invalid"])
        marker = rival / "receipts" / "fixture" / "room" / "marker.json"
        marker.parent.mkdir(parents=True)
        marker.write_text("{}\n", encoding="utf-8")
        run(["git", "-C", rival, "add", "receipts"])
        run(["git", "-C", rival, "commit", "-q", "-m", "rival"])
        env = os.environ.copy()
        env["BRIDGE_TEST_ACTOR"] = "fc"
        run(
            ["git", "-C", rival, "push", "-q", "origin", "HEAD:refs/heads/machines/fc"],
            env=env,
        )
        result = self.fc.sweep()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        health = json.loads((self.fc.root / "bridge" / "health.json").read_text())
        self.assertEqual(health["reason"], "branch_diverged")
        self.assertFalse((self.fc.repo / ".git" / "rebase-merge").exists())

    def test_cross_filesystem_mail_root_round_trip(self):
        shm = Path("/dev/shm")
        if not shm.is_dir() or not os.access(str(shm), os.W_OK):
            self.skipTest("/dev/shm is unavailable; EXDEV fixture cannot be built")
        with CanonicalTemporaryDirectory(
            prefix="post-bridge-shm-", dir=str(shm)
        ) as shm_temp:
            topology = Topology(Path(self.temporary.name) / "exdev")
            fc = topology.add("fc", ["garden"], mail_parent=Path(shm_temp) / "fc")
            trey = topology.add("trey", ["hq"])
            mac = topology.add("mac", ["porch"])
            topology.finalize()
            self.assertNotEqual(fc.root.stat().st_dev, fc.repo.stat().st_dev)
            for _ in range(2):
                for machine in (fc, trey, mac):
                    result = machine.sweep()
                    self.assertEqual(
                        result.returncode, 0, result.stdout + result.stderr
                    )
            mail_id = fc.send("garden", "hq", "cross filesystem")
            expected = (fc.root / "archive" / (mail_id + ".mail")).read_bytes()
            self.assertEqual(fc.sweep().returncode, 0)
            delivered = trey.sweep()
            self.assertEqual(
                delivered.returncode, 0, delivered.stdout + delivered.stderr
            )
            self.assert_delivery_invariant(trey, "hq", mail_id, expected)

    def test_bridge_received_archive_is_not_relayed_after_room_rehome(self):
        self.bootstrap()
        mail_id = fixed_id(500)
        # fc has published rooms.json; the guard under test is the rehome
        # relay guard, not sender binding, so use one of fc's published names.
        data = craft_mail(mail_id, "garden", "hq", body=b"rehome guard")
        self.fc.inject(f"outbox/trey/hq/{mail_id}.mail", data)
        self.assertEqual(self.trey.sweep().returncode, 0)
        self.assertEqual(self.fc.sweep().returncode, 0)
        self.assertTrue((self.trey.root / "bridge" / "received" / mail_id).exists())
        envelope = json.loads(data.split(b"\n---\n", 1)[0])
        self.assertEqual(envelope["from"], "garden")
        ledger = self.trey.root / "bridge" / "delivered" / "fc" / "hq" / mail_id
        ledger.unlink()
        self.assertFalse(SWEEPER.delivered_id_exists(types.SimpleNamespace(root=self.trey.root), mail_id))
        rooms_path = self.trey.root / "rooms.json"
        rooms = json.loads(rooms_path.read_text())
        del rooms["hq"]
        rooms_path.write_text(json.dumps(rooms) + "\n", encoding="utf-8")
        config_path = self.trey.root / "bridge" / "config.json"
        config = json.loads(config_path.read_text())
        config["peers"]["fc"].append("hq")
        config_path.write_text(json.dumps(config) + "\n", encoding="utf-8")
        result = self.trey.sweep()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        health = json.loads((self.trey.root / "bridge" / "health.json").read_text())
        self.assertEqual(health["reason"], "room_name_collision")
        self.assertFalse(
            (self.trey.repo / "outbox" / "fc" / "hq" / (mail_id + ".mail")).exists()
        )

    def test_recovery_reaps_channel_dedup_markers_older_than_thirty_days(self):
        self.bootstrap()
        seen = self.fc.root / "bridge" / "chan-seen" / "ignored" / "trey"
        seen.mkdir(parents=True)
        stale = seen / ("a" * 64)
        fresh = seen / ("b" * 64)
        stale.write_bytes(b"")
        fresh.write_bytes(b"")
        aged = time.time() - 31 * 24 * 60 * 60
        os.utime(str(stale), (aged, aged))
        self.fc.send("garden", "hq", "keep this tick full")

        result = self.fc.sweep()

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(stale.exists())
        self.assertTrue(fresh.is_file())
        reaped = [
            record for record in self.fc.logs() if record["action"] == "chan_seen_reaped"
        ]
        self.assertEqual(len(reaped), 1)
        self.assertEqual(reaped[0]["count"], 1)



class LogDedupeStateTest(unittest.TestCase):
    """bridgelib.logdedupe without a tick: the state file's own bounds."""

    def setUp(self):
        from bridgelib import logdedupe

        self.logdedupe = logdedupe
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(os.path.realpath(self.directory.name))
        self.path = self.root / "bridge" / "log-conditions.json"
        self.path.parent.mkdir()

    def tearDown(self):
        self.directory.cleanup()

    def test_state_at_the_entry_cap_with_long_paths_still_loads(self):
        # Review fix3 finding 2: 2000 entries can serialize past the 512 KiB
        # read cap; such a file read as damaged every tick and the flood
        # returned. settle now fits the file under the cap.
        ld = self.logdedupe
        fields = [
            {"host": "mac", "room": "hq", "id": f"20260923-12{i:04d}-{i:06x}",
             "path": "/Users/trey/.claude-mail/bridge/quarantine/" + "d" * 400}
            for i in range(ld.MAX_ENTRIES)
        ]
        log = ld.ConditionLog(self.path, self.root)
        for item in fields:
            self.assertTrue(log.should_log("forensic", item))
        log.settle(prune=True)
        self.assertLessEqual(self.path.stat().st_size, ld.MAX_STATE_BYTES)
        again = ld.ConditionLog(self.path, self.root)
        self.assertGreater(len(again.prior), 0)
        repeated = [item for item in fields if again.should_log("forensic", item)]
        # Only what did not fit repeats; the rest stays quiet.
        self.assertEqual(len(repeated), ld.MAX_ENTRIES - len(again.prior))
        again.settle(prune=True)
        self.assertLessEqual(self.path.stat().st_size, ld.MAX_STATE_BYTES)
        self.assertGreater(len(ld.ConditionLog(self.path, self.root).prior), 0)


if __name__ == "__main__":
    unittest.main(verbosity=2)
