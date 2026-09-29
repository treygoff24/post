"""Real-Post and real-Git helpers for the channel bridge tests."""

import json
import os
import shutil
import subprocess
from dataclasses import replace
from pathlib import Path

from bridgelib import common
from bridgelib.snapshot import empty_snapshot

from .test_sweep import PARTICIPANT_ENV, acting_room, bind_participant

POST = os.environ.get("POST_BIN", "/usr/local/bin/post")


def run(args, *, cwd=None, env=None, check=True, input_text=None, timeout=120):
    # Without input_text, stdin is /dev/null: an inherited open, silent pipe
    # makes the pre-F3 post refuse reads with input_ambiguous.
    result = subprocess.run(
        [str(arg) for arg in args],
        cwd=str(cwd) if cwd is not None else None,
        env=env,
        input=input_text,
        stdin=subprocess.DEVNULL if input_text is None else None,
        text=True,
        capture_output=True,
        check=False,
        timeout=timeout,
    )
    if check and result.returncode != 0:
        raise AssertionError(
            "command failed ({}):\nstdout:\n{}\nstderr:\n{}".format(
                " ".join(str(arg) for arg in args), result.stdout, result.stderr
            )
        )
    return result


def post_env(root):
    env = os.environ.copy()
    for name in (
        "POST_FROM",
        "POST_SENDER_ADDRESS",
        "POST_ARX_GENERATION",
        "BRIDGE_CRASH_AFTER",
        "BRIDGE_RAISE_AFTER",
    ) + PARTICIPANT_ENV:
        env.pop(name, None)
    env["POST_MAIL_ROOT"] = str(root)
    return env


class Records:
    def __init__(self):
        self.items = []

    def emit(self, action, **fields):
        self.items.append({"action": action, **fields})

    def actions(self, action):
        return [item for item in self.items if item["action"] == action]


class Deadline:
    def __init__(self, raise_after=None):
        self.calls = 0
        self.raise_after = raise_after

    def check(self):
        self.calls += 1
        if self.raise_after is not None and self.calls > self.raise_after:
            from bridgelib.common import DeadlineExpired

            raise DeadlineExpired()


class Git:
    def __init__(self, repo):
        self.repo = Path(repo)
        self.calls = []
        self.read_failures = []

    def run(self, args, check=True, network=False, text=True):
        self.calls.append(tuple(args))
        del network
        result = subprocess.run(
            ["git", "-C", str(self.repo)] + list(args),
            capture_output=True,
            text=text,
            check=False,
            timeout=30,
        )
        if check and result.returncode != 0:
            stdout = result.stdout if text else result.stdout.decode("utf-8", "replace")
            stderr = result.stderr if text else result.stderr.decode("utf-8", "replace")
            raise AssertionError(stdout + stderr)
        return result

    def rev(self, ref):
        result = self.run(["rev-parse", "--verify", ref], check=False)
        return result.stdout.strip() if result.returncode == 0 else None

    def ls_tree(self, ref, prefix):
        result = self.run(
            ["ls-tree", "-r", "-l", "-z", ref, "--", prefix],
            check=False,
            text=False,
        )
        if result.returncode != 0:
            # Mirrors sweep.Git.ls_tree (m6).
            raise common.GitReadError(
                ref, prefix, result.stderr.decode("utf-8", "replace").strip()
            )
        entries = []
        for record in result.stdout.split(b"\0"):
            if not record:
                continue
            metadata, separator, raw_path = record.partition(b"\t")
            if not separator:
                continue
            fields = metadata.split()
            if len(fields) != 4:
                continue
            try:
                path = raw_path.decode("utf-8")
            except UnicodeDecodeError:
                path = None
            entries.append(
                {
                    "mode": fields[0].decode("ascii"),
                    "type": fields[1].decode("ascii"),
                    "object": fields[2].decode("ascii"),
                    "size": None if fields[3] == b"-" else int(fields[3]),
                    "path": path,
                    "raw_path": raw_path,
                }
            )
        return entries

    def show(self, ref, path):
        result = self.run(["show", f"{ref}:{path}"], check=False, text=False)
        return result.stdout if result.returncode == 0 else None


class Machine:
    def __init__(self, topology, host, rooms):
        self.topology = topology
        self.host = host
        self.base = topology.base / host
        self.root = self.base / "mail"
        self.repo = self.base / "relay"
        self.workspaces = {}
        self.participants = {}
        self.base.mkdir(parents=True)
        self.post("doctor", "--fix", check=False)
        for room in rooms:
            workspace = self.base / "rooms" / room
            workspace.mkdir(parents=True)
            self.post("rooms", "add", "--", room, workspace)
            self.workspaces[room] = workspace
        self.post("doctor", "--fix", check=False)
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
        self.git("config", "user.name", f"fixture {host}")
        self.git("config", "user.email", f"{host}@invalid")
        (self.root / "bridge").mkdir(parents=True, exist_ok=True)
        (self.root / "rules.json").write_text('{"blocked":[]}\n', encoding="utf-8")

    @property
    def settings(self):
        return type(
            "Settings",
            (),
            {
                "root": self.root,
                "repo": self.repo,
                "host": self.host,
                "max_mail_bytes": 1024 * 1024,
            },
        )()

    def participant(self, room):
        """This machine's one bound participant for `room`, minted on first use."""
        if room not in self.participants:
            self.participants[room] = bind_participant(
                POST, self.root, self.workspaces[room]
            )
        return self.participants[room]

    def post(self, *args, cwd=None, check=True):
        env = post_env(self.root)
        room = acting_room(self.workspaces, args, cwd)
        if room is not None:
            env["POST_PARTICIPANT"] = self.participant(room)
        return run(
            [POST] + [str(arg) for arg in args],
            cwd=cwd,
            env=env,
            check=check,
        )

    def join(self, channel, room, description=None):
        args = ["chat", channel, "--join"]
        if description is not None:
            args.extend(("--description", description))
        return self.post(*args, cwd=self.workspaces[room])

    def send(self, channel, room, body):
        return self.post(
            "chat",
            channel,
            "--send",
            "--body",
            body,
            "--anyway",
            "--json",
            cwd=self.workspaces[room],
        )

    def git(self, *args, check=True):
        return run(["git", "-C", self.repo] + list(args), check=check)

    def commit(self, message="fixture channel state"):
        self.git("add", "-A", "--", "channels")
        changed = self.git("diff", "--cached", "--quiet", check=False).returncode != 0
        if changed:
            self.git("commit", "-q", "-m", message)
            self.git("push", "-q", "origin", f"HEAD:refs/heads/machines/{self.host}")
        return self.git("rev-parse", "HEAD").stdout.strip()

    def fetch(self):
        self.git(
            "fetch",
            "-q",
            "origin",
            "+refs/heads/machines/*:refs/remotes/origin/machines/*",
        )

    def write_relay(self, relative, data, *, mode=None, message="hostile fixture"):
        path = self.repo / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        if mode == "symlink":
            path.symlink_to(data.decode("utf-8"))
        else:
            path.write_bytes(data)
            if mode is not None:
                path.chmod(mode)
        self.git("add", "-A", "--", relative)
        self.git("commit", "-q", "-m", message)
        self.git("push", "-q", "origin", f"HEAD:refs/heads/machines/{self.host}")
        return self.git("rev-parse", "HEAD").stdout.strip()

    def write_many_relay_entries(self, count, prefix="channels/junk"):
        blob = subprocess.run(
            ["git", "-C", str(self.repo), "hash-object", "-w", "--stdin"],
            input=b"x",
            capture_output=True,
            check=True,
        ).stdout.decode("ascii").strip()
        rows = "".join(
            f"100644 {blob}\t{prefix}/{number}.txt\n" for number in range(count)
        )
        result = subprocess.run(
            ["git", "-C", str(self.repo), "update-index", "--add", "--index-info"],
            input=rows.encode("utf-8"),
            capture_output=True,
            check=False,
        )
        if result.returncode != 0:
            raise AssertionError(result.stderr.decode("utf-8", "replace"))
        self.git("commit", "-q", "-m", f"publish {count} entries")
        self.git("push", "-q", "origin", f"HEAD:refs/heads/machines/{self.host}")
        return self.git("rev-parse", "HEAD").stdout.strip()


class Topology:
    def __init__(self, base):
        self.base = Path(base)
        self.forge = self.base / "forge.git"
        self.seed = self.base / "seed"
        self.machines = {}
        run(["git", "init", "--bare", "-q", "-b", "main", self.forge])
        run(["git", "init", "-q", "-b", "main", self.seed])
        run(["git", "-C", self.seed, "config", "user.name", "fixture"])
        run(["git", "-C", self.seed, "config", "user.email", "fixture@invalid"])
        run(["git", "-C", self.seed, "commit", "-q", "--allow-empty", "-m", "seed"])
        run(["git", "-C", self.seed, "remote", "add", "origin", self.forge])

    def add(self, host, rooms):
        run(
            [
                "git",
                "-C",
                self.seed,
                "push",
                "-q",
                "origin",
                f"HEAD:refs/heads/machines/{host}",
            ]
        )
        machine = Machine(self, host, rooms)
        self.machines[host] = machine
        return machine


def snapshot_for(
    machine,
    peers,
    *,
    published=None,
    v2_peers=None,
    routes=None,
    placeholders=None,
    contested=None,
    owners=None,
    post_rooms=None,
):
    """A tick snapshot for `machine`.

    `post_rooms` models post's room table after placeholder registration
    (r5.5, M2). By default every fixture placeholder registered where the
    bridge puts it, <root>/remote/<host>/<name>, as ensure_placeholders
    does; pass a table to model a failed or missing registration.
    """
    machine.fetch()
    oids = {
        f"machines/{host}": machine.git(
            "rev-parse", f"origin/machines/{host}"
        ).stdout.strip()
        for host in peers
    }
    return replace(
        empty_snapshot(machine.host),
        peers=tuple(peers),
        oids=oids,
        published=published or {},
        v2_peers=frozenset(v2_peers or ()),
        routes=routes or {},
        real_rooms={name: str(path) for name, path in machine.workspaces.items()},
        placeholders=placeholders or {},
        contested=contested or {},
        owners=owners or {},
        post_rooms=(
            post_rooms
            if post_rooms is not None
            else {
                name: str(machine.root / "remote" / host / name)
                for name, host in (placeholders or {}).items()
                if host
            }
        ),
    )


def channel_id(sequence):
    return f"20260902-172200-{sequence:06d}-{sequence:06x}"


def channel_record(name, created_by, description="fixture"):
    return {
        "name": name,
        "created": "2026-09-02 17:22:00 +0000",
        "created_by": created_by,
        "description": description,
    }


def channel_message(
    message_id, sender, channel, *, event=None, mentions=None, body="body", **extra
):
    envelope = {
        "id": message_id,
        "from": sender,
        "channel": channel,
        "subject": "fixture",
        "sent": "2026-09-02 17:22:00 +0000",
    }
    if event is not None:
        envelope["event"] = event
    if mentions is not None:
        envelope["mentions"] = mentions
    envelope.update(extra)
    return (
        json.dumps(envelope, sort_keys=True, indent=2).encode()
        + b"\n---\n"
        + body.encode()
    )


def copy_local_channels_to_repo(machine):
    source = machine.root / "channels"
    destination = machine.repo / "channels"
    if destination.exists():
        shutil.rmtree(destination)
    shutil.copytree(
        source, destination, ignore=shutil.ignore_patterns(".channels.lock")
    )
