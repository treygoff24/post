"""Small v2 extensions to the real-Post, real-Git sweep test harness."""

import json
import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from .test_sweep import Machine, Topology, run, run_input


class MachineV2(Machine):
    def write_config(self, include_peers=True, pins=None):
        config = {
            "host": self.host,
            "relay_url": str(self.topology.forge.resolve()),
        }
        if include_peers:
            config["peers"] = (
                pins
                if pins is not None
                else {
                    machine.host: sorted(machine.workspaces)
                    for machine in self.topology.machines
                    if machine.host != self.host
                }
            )
        bridge = self.root / "bridge"
        bridge.mkdir(parents=True, exist_ok=True)
        (bridge / "config.json").write_text(
            json.dumps(config, sort_keys=True) + "\n", encoding="utf-8"
        )
        (bridge / ".health.lock").touch()

    def push_rooms(self, data, mode="100644"):
        blob = run_input(
            ["git", "-C", self.repo, "hash-object", "-w", "--stdin"], data
        )
        object_id = blob
        if mode == "040000":
            object_id = run_input(
                ["git", "-C", self.repo, "mktree"],
                f"100644 blob {blob}\tpayload\n".encode("ascii"),
            )
        self.git(
            "update-index",
            "--add",
            "--cacheinfo",
            f"{mode},{object_id},rooms.json",
        )
        self.git("commit", "-q", "-m", "publish hostile rooms fixture")
        self.git("push", "-q", "origin", f"HEAD:refs/heads/machines/{self.host}")
        return self.git("rev-parse", "HEAD").stdout.strip()

    def bridge_bytes(self, *parts):
        return (self.root / "bridge" / Path(*parts)).read_bytes()

    def bridge_json(self, *parts):
        return json.loads(self.bridge_bytes(*parts))


class TopologyV2(Topology):
    def __init__(self, base):
        super().__init__(base)
        hook = self.forge / "hooks" / "pre-receive"
        hook.write_text(
            "#!/bin/sh\n"
            "set -eu\n"
            "actor=${BRIDGE_TEST_ACTOR:-}\n"
            "while read old new ref; do\n"
            "  case $ref in\n"
            "    refs/heads/registry) [ \"$actor\" = operator ] || { echo registry-protected >&2; exit 1; } ;;\n"
            "    refs/heads/machines/$actor) : ;;\n"
            "    *) echo \"protected branch: $actor cannot update $ref\" >&2; exit 1 ;;\n"
            "  esac\n"
            "done\n",
            encoding="utf-8",
        )
        hook.chmod(0o755)
        self.registry_operator = self.base / "registry-operator"
        run(["git", "init", "-q", "-b", "registry", self.registry_operator])
        run(
            [
                "git",
                "-C",
                self.registry_operator,
                "config",
                "user.name",
                "registry operator",
            ]
        )
        run(
            [
                "git",
                "-C",
                self.registry_operator,
                "config",
                "user.email",
                "operator@invalid",
            ]
        )
        run(
            [
                "git",
                "-C",
                self.registry_operator,
                "remote",
                "add",
                "origin",
                self.forge,
            ]
        )

    def add(self, host, rooms, mail_parent=None):
        environment = os.environ.copy()
        environment["BRIDGE_TEST_ACTOR"] = host
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
            env=environment,
        )
        machine = MachineV2(self, host, rooms, mail_parent=mail_parent)
        self.machines.append(machine)
        return machine

    def finalize(self, include_peers=True, pins=None):
        for machine in self.machines:
            machine.write_config(
                include_peers=include_peers,
                pins=None if pins is None else pins.get(machine.host),
            )

    def push_registry(self, value=None, data=None, mode="100644"):
        if data is None:
            data = (json.dumps(value, sort_keys=True) + "\n").encode("utf-8")
        blob = run_input(
            ["git", "-C", self.registry_operator, "hash-object", "-w", "--stdin"],
            data,
        )
        run(
            [
                "git",
                "-C",
                self.registry_operator,
                "update-index",
                "--add",
                "--cacheinfo",
                f"{mode},{blob},hosts.json",
            ]
        )
        run(
            [
                "git",
                "-C",
                self.registry_operator,
                "commit",
                "-q",
                "-m",
                "registry fixture",
            ]
        )
        environment = os.environ.copy()
        environment["BRIDGE_TEST_ACTOR"] = "operator"
        run(
            [
                "git",
                "-C",
                self.registry_operator,
                "push",
                "-q",
                "origin",
                "HEAD:refs/heads/registry",
            ],
            env=environment,
        )
        return run(
            ["git", "-C", self.registry_operator, "rev-parse", "HEAD"]
        ).stdout.strip()
