#!/usr/bin/env python3
"""One crash-reconciling Post relay tick, implementing SPEC-v2 r5.2."""

import argparse
import datetime as dt
import errno
import fcntl
import hashlib
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import threading
import time
from contextlib import contextmanager
from dataclasses import dataclass
from dataclasses import replace as dataclass_replace
from pathlib import Path, PurePosixPath

# The installed launcher may execute this file from any cwd.  Put the package
# directory on sys.path before importing bridgelib.
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from bridgelib import (  # noqa: E402
    attention,
    bounce,
    channels,
    decided,
    localheld,
    logdedupe,
    pmail,
    rooms,
    tick,
)
from bridgelib.common import (  # noqa: E402
    blocked_reason,
    load_rules,
    DEFAULT_DEADLINE_SECONDS,
    DEFAULT_FETCH_GRACE_SECONDS,
    DEFAULT_MAX_MAIL_BYTES,
    ENVELOPE_OPTIONAL,
    ENVELOPE_REQUIRED,
    GIT_NETWORK_TIMEOUT_SECONDS,
    GIT_TIMEOUT_SECONDS,
    LOG_ROTATE_BYTES,
    MAIL_ADDRESS_KINDS,
    MAIL_KINDS,
    MAX_MAX_MAIL_BYTES,
    RECEIPT_KEYS,
    RECEIPT_STATUSES,
    RELAY_WARN_BYTES,
    REMOTE_PARTICIPANT_UNHOMED,
    SENDER_NOT_HOMED,
    SHA256_RE,
    ConfigError,
    DeadlineExpired,
    GitReadError,
    HoldLedger,
    TickError,
    atomic_replace,
    checkpoint,
    destination,
    ensure_dir,
    epoch_from_iso,
    exclusive_publish,
    fold_name,
    fsync_directory,
    load_json_bytes,
    note_git_failure,
    open_regular,
    post_environment,
    post_sees_remote,
    parse_positive_int,
    realpath_equal,
    under,
    utc_now,
    validate_host,
    validate_id,
    validate_attribution,
    validate_path_component,
    validate_room,
)
from bridgelib.snapshot import (  # noqa: E402
    FORGED_SELF,
    NAME_COLLISION,
    UNHOMED,
    UNPUBLISHED_SENDER,
    VERIFIED,
    binding_verdict,
)

# The post versions this bridge is written for: 0.9.0 up to, but not
# including, 0.10.0. Every 0.9.x is the same mail model (a patch release does
# not change a store or command contract), so a patch bump must not stop the
# bridge on the hosts it reaches; 0.10.0 is where a check is needed again.
POST_VERSION_RANGE = "post 0.9.0 up to, but not including, post 0.10.0"
# `post --version` may carry build metadata after the semver
# (`post 0.9.4 (build abc1234, ...)`); only the semver is judged. A patch
# number is a plain integer: no leading zeros, and no pre-release tag.
_POST_VERSION_RE = re.compile(r"post 0\.9\.(?:0|[1-9][0-9]*)(?: \(build [^()\n]*\))?")


def post_version_accepted(text):
    """True when ``post --version`` printed a version from 0.9.0 up to, but
    not including, 0.10.0, with or without a trailing ``(build ...)``."""
    return _POST_VERSION_RE.fullmatch(text.strip()) is not None


# Relay worktree namespaces the bridge owns; pmail/preceipts are F3.
RELAY_NAMESPACES = ("outbox", "receipts", "channels", "pmail", "preceipts")
RELAY_PATHS = (*RELAY_NAMESPACES, "rooms.json")


class Deadline:
    def __init__(self, seconds):
        self.ends = time.monotonic() + seconds

    def remaining(self):
        return self.ends - time.monotonic()

    def check(self):
        if self.remaining() <= 0:
            raise DeadlineExpired()

    def timeout(self, ceiling):
        self.check()
        return max(1, min(ceiling, int(self.remaining()) + 1))


class Logger:
    def __init__(self, root, echo=False):
        self.root = root
        # A tick logs to log.jsonl only: launchd and journald used to capture
        # a second copy of every line from stdout. The operator commands
        # (--init-held-sentinel, --seed-local-holds) pass echo=True because
        # their stdout is their result.
        self.echo = echo
        self.bridge = destination(root, "bridge")
        ensure_dir(root, self.bridge)
        self.path = destination(root, "bridge", "log.jsonl")
        self.conditions = None
        # What this tick saw, for health.json's attention list. Recorded
        # before the dedupe below, so a standing condition whose line was
        # suppressed is still reported.
        self.quarantines = []
        self.unrelayables = {}

    def track_conditions(self):
        # Full ticks only: a standing per-letter condition is logged when it
        # appears or changes (logdedupe). Never fails the tick.
        self.conditions = logdedupe.ConditionLog(
            destination(self.root, "bridge", "log-conditions.json"), self.root
        )

    def settle_conditions(self, prune):
        conditions, self.conditions = self.conditions, None
        if conditions is None:
            return {}
        cleared, standing = conditions.settle(prune)
        for action, host, room, mail_id, *path in cleared:
            fields = {"host": host, "room": room, "id": mail_id}
            fields["path"] = path[0] if path else None
            self.emit(
                "condition_cleared",
                condition=action,
                **{name: value for name, value in fields.items() if value is not None},
            )
        return standing

    def host_unread(self, host):
        # A peer this tick could not read (no branch) keeps its conditions.
        if self.conditions is not None:
            self.conditions.host_unread(host)

    def emit(self, action, **fields):
        if action in ("quarantined", "quarantined_path"):
            self.quarantines.append(dict(fields))
        elif action == "outbound_ignored" and isinstance(fields.get("id"), str):
            self.unrelayables[fields["id"]] = str(fields.get("reason", "unreadable"))
        # During a full tick, any action named in logdedupe.CONDITION_ACTIONS
        # is deduped, whichever module emits it (see that constant).
        if self.conditions is not None and not self.conditions.should_log(
            action, fields
        ):
            return
        record = {"ts": utc_now(), "action": action}
        record.update(fields)
        line = json.dumps(record, sort_keys=True, separators=(",", ":"))
        # A tick's one sink is log.jsonl; stderr carries what could not be
        # persisted below.
        if self.echo:
            print(line, flush=True)
        try:
            if self.path.exists() and self.path.stat().st_size >= LOG_ROTATE_BYTES:
                rotated = self.path.with_name("log.jsonl.1")
                if rotated.exists() or rotated.is_symlink():
                    rotated.unlink()
                os.replace(str(self.path), str(rotated))
            flags = (
                os.O_WRONLY | os.O_APPEND | os.O_CREAT | getattr(os, "O_NOFOLLOW", 0)
            )
            descriptor = os.open(str(self.path), flags, 0o600)
            try:
                os.write(descriptor, (line + "\n").encode("utf-8"))
                os.fsync(descriptor)
            finally:
                os.close(descriptor)
        except OSError as error:
            print(f"post-bridge: cannot persist log: {error}", file=sys.stderr)
            print(line, file=sys.stderr, flush=True)


@dataclass
class Settings:
    root: Path
    repo: Path
    host: str
    ssh_key: Path
    post_bin: str
    max_mail_bytes: int
    deadline_seconds: int
    fetch_grace_seconds: int
    # Health's interval_s (F3 capability guard); never fatal when unparseable.
    interval_seconds: "int | None" = None  # a string: Python 3.9 is supported
    # Seconds a decided marker is trusted before its letter is re-judged
    # (bridgelib/decided.py); 0 re-judges every tick.
    decided_recheck_seconds: int = decided.DEFAULT_RECHECK_SECONDS


@dataclass
class Config:
    host: str
    relay_url: str
    peers: dict
    channels: object = None


class Git:
    def __init__(self, settings, deadline):
        self.settings = settings
        self.deadline = deadline
        # m6: per-host read failures this tick (host, ref, path).
        self.read_failures = []
        self.environment = os.environ.copy()
        self.environment["GIT_SSH_COMMAND"] = (
            f"ssh -o ConnectTimeout=10 -o BatchMode=yes -i {shlex_quote(str(settings.ssh_key))} "
            "-o IdentitiesOnly=yes"
        )

    def run(self, args, check=True, network=False, text=True):
        timeout = self.deadline.timeout(
            GIT_NETWORK_TIMEOUT_SECONDS if network else GIT_TIMEOUT_SECONDS
        )
        result = subprocess.run(
            ["git", "-C", str(self.settings.repo)] + list(args),
            env=self.environment,
            capture_output=True,
            text=text,
            timeout=timeout,
            check=False,
        )
        if check and result.returncode != 0:
            stdout = result.stdout if text else result.stdout.decode("utf-8", "replace")
            stderr = result.stderr if text else result.stderr.decode("utf-8", "replace")
            raise TickError(
                "git_failed",
                "git {} failed: {}{}".format(" ".join(args), stdout, stderr),
            )
        return result

    def rev(self, ref):
        result = self.run(["rev-parse", "--verify", ref], check=False)
        return result.stdout.strip() if result.returncode == 0 else None

    def is_ancestor(self, older, newer):
        return (
            self.run(
                ["merge-base", "--is-ancestor", older, newer], check=False
            ).returncode
            == 0
        )

    def ls_tree(self, ref, prefix):
        result = self.run(
            ["ls-tree", "-r", "-l", "-z", ref, "--", prefix],
            check=False,
            text=False,
        )
        if result.returncode != 0:
            # m6: a nonzero ls-tree is a real failure (a missing path exits
            # 0 with no output); an empty list would silently skip the peer.
            raise GitReadError(
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
                size = None if fields[3] == b"-" else int(fields[3])
            except ValueError:
                continue
            try:
                path = raw_path.decode("utf-8")
            except UnicodeDecodeError:
                path = None
            entries.append(
                {
                    "mode": fields[0].decode("ascii", "replace"),
                    "type": fields[1].decode("ascii", "replace"),
                    "object": fields[2].decode("ascii", "replace"),
                    "size": size,
                    "path": path,
                    "raw_path": raw_path,
                }
            )
        return entries

    def show(self, ref, path):
        result = self.run(["show", f"{ref}:{path}"], check=False, text=False)
        return result.stdout if result.returncode == 0 else None

    def blob_sha256(self, oid):
        """``(sha256 hex, size)`` of a blob, streamed, never held; None on failure."""
        timeout = self.deadline.timeout(GIT_TIMEOUT_SECONDS)
        try:
            process = subprocess.Popen(
                ["git", "-C", str(self.settings.repo), "cat-file", "blob", oid],
                env=self.environment,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
            )
        except OSError:
            return None
        timer = threading.Timer(timeout, process.kill)
        timer.start()
        digest = hashlib.sha256()
        size = 0
        try:
            while True:
                chunk = process.stdout.read(65536)
                if not chunk:
                    break
                digest.update(chunk)
                size += len(chunk)
            returncode = process.wait()
        finally:
            timer.cancel()
            process.stdout.close()
        return (digest.hexdigest(), size) if returncode == 0 else None


def shlex_quote(value):
    return "'{}'".format(value.replace("'", "'\"'\"'"))


def load_settings():
    required = ("POST_MAIL_ROOT", "BRIDGE_REPO", "BRIDGE_HOST", "BRIDGE_SSH_KEY")
    missing = [name for name in required if not os.environ.get(name)]
    if missing:
        raise ConfigError("missing required environment: {}".format(", ".join(missing)))
    root = Path(os.environ["POST_MAIL_ROOT"])
    repo = Path(os.environ["BRIDGE_REPO"])
    ssh_key = Path(os.environ["BRIDGE_SSH_KEY"])
    for name, path in (
        ("POST_MAIL_ROOT", root),
        ("BRIDGE_REPO", repo),
        ("BRIDGE_SSH_KEY", ssh_key),
    ):
        if not path.is_absolute() or not realpath_equal(path):
            raise ConfigError(f"{name} must be absolute and canonical")
    if not root.is_dir():
        raise ConfigError("POST_MAIL_ROOT must be an existing directory")
    if not repo.is_dir() or not (repo / ".git").exists():
        raise ConfigError("BRIDGE_REPO must be an existing Git worktree")
    try:
        key_metadata = ssh_key.lstat()
    except FileNotFoundError:
        raise ConfigError("BRIDGE_SSH_KEY does not exist")
    if not stat.S_ISREG(key_metadata.st_mode) or ssh_key.is_symlink():
        raise ConfigError("BRIDGE_SSH_KEY must be a regular file")
    if stat.S_IMODE(key_metadata.st_mode) != 0o600:
        raise ConfigError("BRIDGE_SSH_KEY must have mode 0600")
    host = os.environ["BRIDGE_HOST"]
    validate_host(host, "BRIDGE_HOST")
    max_bytes = parse_positive_int(
        os.environ.get("BRIDGE_MAX_MAIL_BYTES"),
        "BRIDGE_MAX_MAIL_BYTES",
        DEFAULT_MAX_MAIL_BYTES,
        MAX_MAX_MAIL_BYTES,
    )
    deadline_raw = os.environ.get("BRIDGE_TICK_DEADLINE_SECONDS")
    deadline_seconds = parse_positive_int(
        deadline_raw,
        "BRIDGE_TICK_DEADLINE_SECONDS",
        DEFAULT_DEADLINE_SECONDS,
    )
    fetch_grace = parse_positive_int(
        os.environ.get("BRIDGE_FETCH_GRACE_SECONDS"),
        "BRIDGE_FETCH_GRACE_SECONDS",
        DEFAULT_FETCH_GRACE_SECONDS,
    )
    return Settings(
        root=root,
        repo=repo,
        host=host,
        ssh_key=ssh_key,
        post_bin=os.environ.get("POST_BIN", "post"),
        max_mail_bytes=max_bytes,
        deadline_seconds=deadline_seconds,
        fetch_grace_seconds=fetch_grace,
        interval_seconds=pmail.interval_seconds(os.environ.get("BRIDGE_INTERVAL_SECONDS")),
        decided_recheck_seconds=decided.parse_recheck_seconds(
            os.environ.get("BRIDGE_DECIDED_RECHECK_SECONDS")
        ),
    )


def load_config(settings):
    path = destination(settings.root, "bridge", "config.json")
    try:
        data = open_regular(path, 1024 * 1024)
    except (FileNotFoundError, ConfigError) as error:
        raise ConfigError(f"cannot read config.json: {error}")
    value = load_json_bytes(data, str(path))
    allowed_keys = {"host", "relay_url", "peers", "channels"}
    required_keys = {"host", "relay_url"}
    if (
        not isinstance(value, dict)
        or not required_keys.issubset(value)
        or not set(value).issubset(allowed_keys)
    ):
        raise ConfigError(
            "config.json must contain host and relay_url, with optional peers and channels"
        )
    validate_host(value["host"], "config.host")
    if value["host"] != settings.host:
        raise ConfigError("config.host must equal BRIDGE_HOST")
    if not isinstance(value["relay_url"], str) or not value["relay_url"]:
        raise ConfigError("config.relay_url must be a non-empty string")
    peers = value.get("peers", {})
    if not isinstance(peers, dict):
        raise ConfigError("config.peers must be an object")
    seen = {}
    normalized = {}
    for host, rooms in peers.items():
        validate_host(host, "peer host")
        if host == settings.host:
            raise ConfigError("config.peers must not include config.host")
        if not isinstance(rooms, list):
            raise ConfigError(f"rooms for peer {host} must be a list")
        normalized[host] = []
        for room in rooms:
            validate_room(room, topology=True, label="peer room")
            folded = room.lower() if room.isascii() else room
            if folded in seen:
                raise ConfigError(
                    f"peer room collision under ASCII case folding: {seen[folded]!r} and {room!r}"
                )
            seen[folded] = room
            normalized[host].append(room)
    # An absent key syncs every channel; an explicit null turns channel sync
    # off (SPEC-v2 §Channel config, r6.2).
    channel_config = channels.parse_channels_config(
        value.get("channels", {"mode": "all"})
    )
    return Config(value["host"], value["relay_url"], normalized, channel_config)


def validate_post_version(settings):
    path = settings.post_bin
    if not os.path.isfile(path) or not os.access(path, os.X_OK):
        raise ConfigError("POST_BIN must be an existing executable file")
    try:
        result = subprocess.run(
            [path, "--version"],
            env=post_environment(settings.root),
            capture_output=True,
            text=True,
            timeout=30,
            check=False,
        )
    except OSError as error:
        raise ConfigError(f"POST_BIN --version failed: {error}")
    if result.returncode != 0 or not post_version_accepted(result.stdout):
        raise ConfigError(
            f"POST_BIN --version must print {POST_VERSION_RANGE}, optionally "
            "followed by ' (build ...)'"
        )


def validate_repo(settings, config, git):
    branch = git.run(["symbolic-ref", "--quiet", "--short", "HEAD"]).stdout.strip()
    if branch != f"machines/{settings.host}":
        raise ConfigError(f"BRIDGE_REPO must have machines/{settings.host} checked out")
    fetch_url = git.run(["remote", "get-url", "origin"]).stdout.strip()
    push_url = git.run(["remote", "get-url", "--push", "origin"]).stdout.strip()
    if fetch_url != config.relay_url or push_url != config.relay_url:
        raise ConfigError("origin fetch and push URLs must equal config.relay_url")


def run_post_rooms(settings):
    env = post_environment(settings.root)
    result = subprocess.run(
        [settings.post_bin, "rooms", "--json"],
        env=env,
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )
    if result.returncode != 0:
        raise ConfigError(f"post rooms --json failed: {result.stdout}{result.stderr}")
    value = load_json_bytes(result.stdout.encode("utf-8"), "post rooms output")
    rooms = value.get("rooms") if isinstance(value, dict) else None
    if not isinstance(rooms, list):
        raise ConfigError("post rooms --json returned an unexpected shape")
    result_map = {}
    for room in rooms:
        if (
            not isinstance(room, dict)
            or not isinstance(room.get("name"), str)
            or not isinstance(room.get("path"), str)
        ):
            raise ConfigError("post rooms --json returned an invalid room entry")
        result_map[room["name"]] = room["path"]
    return result_map


def fence_present(settings):
    return (
        destination(settings.root, ".post-arx.json").exists()
        or destination(settings.root, ".post-arx.json").is_symlink()
    )


CHAN_SEEN_MAX_AGE_SECONDS = 30 * 24 * 60 * 60


def reap_chan_seen(settings, logger, now=None):
    """Drop channel dedup markers older than 30 days (SPEC-v2 r5.3).

    ``bridge/chan-seen/<category>/<host>/<sha256>`` is written for every
    ignored or quarantined peer entry and nothing else ever removes it, so
    a peer that keeps publishing junk paths would otherwise own an
    ever-growing slice of this node's inodes.
    """
    root = destination(settings.root, "bridge", "chan-seen")
    if not root.is_dir():
        return 0
    cutoff = (time.time() if now is None else now) - CHAN_SEEN_MAX_AGE_SECONDS
    removed = 0
    for marker in list(root.rglob("*")):
        if marker.is_symlink() or not marker.is_file():
            continue
        try:
            if marker.stat().st_mtime >= cutoff:
                continue
            marker.unlink()
        except OSError:
            continue
        removed += 1
    if removed:
        logger.emit("chan_seen_reaped", count=removed)
    return removed


def recover(settings, git, logger):
    git_dir = Path(git.run(["rev-parse", "--git-dir"]).stdout.strip())
    if not git_dir.is_absolute():
        git_dir = settings.repo / git_dir
    for name in ("rebase-merge", "rebase-apply", "MERGE_HEAD"):
        if (git_dir / name).exists():
            raise TickError(
                "foreign_state",
                f"{name} present in {git_dir}; not ours — inspect manually",
            )
    # bridge/.lock guarantees a single sweeper, so any git lock present at
    # recovery was left by a killed git child (subprocess or unit timeout)
    # and is stale by construction (ruling R4).
    for lock in (git_dir / "index.lock", git_dir / "packed-refs.lock"):
        if lock.exists() or lock.is_symlink():
            lock.unlink()
            logger.emit("git_lock_removed", path=str(lock))
    refs_root = git_dir / "refs"
    if refs_root.is_dir():
        for lock in refs_root.rglob("*.lock"):
            if lock.is_file() or lock.is_symlink():
                lock.unlink()
                logger.emit("git_lock_removed", path=str(lock))
    tmp_root = destination(settings.root, "bridge", "tmp")
    ensure_dir(settings.root, tmp_root)
    for item in list(tmp_root.iterdir()):
        if item.name.endswith(".tmp") and item.is_file() and not item.is_symlink():
            item.unlink()
            logger.emit("tmp_removed", path=str(item))
    bridge_root = destination(settings.root, "bridge")
    for item in bridge_root.rglob(".*.tmp"):
        if item.is_file() and not item.is_symlink():
            item.unlink()
            logger.emit("tmp_removed", path=str(item.relative_to(settings.root)))
    reap_chan_seen(settings, logger)
    for namespace in RELAY_NAMESPACES:
        ensure_dir(settings.repo, destination(settings.repo, namespace))
    for item in settings.repo.rglob(".*.tmp"):
        in_owned_namespace = (
            any(under(settings.repo / namespace, item) for namespace in RELAY_NAMESPACES)
            or (item.parent == settings.repo and item.name.startswith(".rooms.json."))
        )
        if in_owned_namespace and item.is_file() and not item.is_symlink():
            item.unlink()
            logger.emit("tmp_removed", path=str(item.relative_to(settings.repo)))
    allowed = {".git", "rooms.json", *RELAY_NAMESPACES}
    stray_root = destination(settings.root, "bridge", "stray")
    for item in list(settings.repo.iterdir()):
        if item.name in allowed:
            continue
        ensure_dir(settings.root, stray_root)
        target = stray_root / f"{int(time.time())}-{item.name}"
        try:
            os.replace(str(item), str(target))
        except OSError as error:
            if error.errno != errno.EXDEV:
                raise
            if item.is_dir() and not item.is_symlink():
                shutil.copytree(item, target, symlinks=True)
                shutil.rmtree(item)
            else:
                shutil.copy2(item, target, follow_symlinks=False)
                item.unlink()
        logger.emit("stray_moved", path=item.name, destination=str(target))
    # A stray the branch tracks (the relay's Initial README on machines/mac)
    # must leave the index as well, or every tick's commit keeps it and each
    # reinstall that restores it is moved again (post-aqw.12). Reading HEAD
    # also catches one an earlier tick already moved. commit_and_push commits
    # the whole index, so the staged removal lands in this tick's commit.
    tracked = git.run(["ls-tree", "-z", "--name-only", "HEAD"], check=False)
    if tracked.returncode == 0:
        for name in sorted(set(filter(None, tracked.stdout.split("\0"))) - allowed):
            untracked = git.run(
                ["rm", "-r", "--cached", "--quiet", "--ignore-unmatch", "--",
                 f":(literal){name}"],
                check=False,
            )
            if untracked.returncode != 0:
                # Retried next tick; never turns the tick into git_failed.
                logger.emit(
                    "stray_untrack_failed",
                    path=name,
                    error=(untracked.stderr or untracked.stdout).strip()[:500],
                )
                continue
            logger.emit("stray_untracked", path=name)
    # F3: a participant receipt that never reached a commit decided nothing;
    # the letter is re-evaluated (design §Crash states, rejection table).
    pmail.discard_uncommitted_receipts(settings, git, logger)


def fast_forward_state(settings, git, logger):
    local = git.rev("HEAD")
    remote_ref = f"origin/machines/{settings.host}"
    remote = git.rev(remote_ref)
    if remote is None:
        return local, None
    if local == remote or git.is_ancestor(remote, local):
        return local, remote
    if git.is_ancestor(local, remote):
        status = git.run(["status", "--porcelain"]).stdout
        if status:
            raise TickError(
                "branch_diverged", "remote advanced while the worktree is dirty"
            )
        git.run(["merge", "--ff-only", remote_ref])
        logger.emit(
            "remote_advanced", local=local, remote=remote
        )
        return git.rev("HEAD"), remote
    raise TickError("branch_diverged")


def refresh_remote_refs(git, logger):
    """Fetch machine refs plus the optional registry branch."""
    machines = git.run(
        [
            "fetch",
            "origin",
            "+refs/heads/machines/*:refs/remotes/origin/machines/*",
        ],
        check=False,
        network=True,
    )
    if machines.returncode != 0:
        return False
    registry = git.run(
        [
            "fetch",
            "origin",
            "+refs/heads/registry:refs/remotes/origin/registry",
        ],
        check=False,
        network=True,
    )
    if registry.returncode != 0:
        output = (registry.stdout + registry.stderr).lower()
        if "couldn't find remote ref" not in output:
            return False
        git.run(["update-ref", "-d", "refs/remotes/origin/registry"], check=False)
        logger.emit("registry_branch_missing")
    return True


def fetch_remote(settings, git, logger):
    """Refresh refs and fast-forward the single-writer local branch."""
    if not refresh_remote_refs(git, logger):
        return False
    fast_forward_state(settings, git, logger)
    return True


def pin_remote_oids(git):
    """Pin every fetched machine tip and the registry tip for this full tick."""
    result = git.run(
        [
            "for-each-ref",
            "--format=%(refname:strip=4) %(objectname)",
            "refs/remotes/origin/machines/*",
        ]
    )
    pinned = {}
    for line in result.stdout.splitlines():
        host, separator, oid = line.partition(" ")
        if separator and host and oid:
            pinned[f"machines/{host}"] = oid
    registry = git.rev("refs/remotes/origin/registry")
    if registry is not None:
        pinned["registry"] = registry
    return pinned


def parse_envelope(data, expected_id, expected_room, logger=None):
    separator = data.find(b"\n---\n")
    if separator < 0:
        raise ConfigError("malformed_header: missing envelope separator")
    if separator > 4096:
        raise ConfigError("malformed_header: header exceeds 4 KiB")
    try:
        value = load_json_bytes(data[:separator], "mail envelope")
    except ConfigError as error:
        raise ConfigError(f"malformed_header: {error}")
    if not isinstance(value, dict):
        raise ConfigError("malformed_header: envelope must be an object")
    missing = ENVELOPE_REQUIRED - set(value)
    if missing:
        raise ConfigError(
            "malformed_header: missing {}".format(",".join(sorted(missing)))
        )
    for key in ENVELOPE_REQUIRED:
        if not isinstance(value[key], str):
            raise ConfigError(f"malformed_header: {key} must be a string")
    for key in ENVELOPE_OPTIONAL:
        if key in value and not isinstance(value[key], str):
            raise ConfigError(f"malformed_header: {key} must be a string")
    validate_attribution(value, MAIL_ADDRESS_KINDS)
    validate_id(value["id"], "envelope id")
    validate_room(value["from"], label="envelope from")
    validate_room(value["to"], label="envelope to")
    if value["id"] != expected_id:
        raise ConfigError("id_mismatch")
    if value["to"] != expected_room:
        raise ConfigError("to_mismatch")
    if not workspace_addressed(value):
        # r5.4: lineage and participant addresses are host-local; `to` names
        # a lineage or participant, not a room, so this must never land in a
        # room inbox.
        raise ConfigError("unsupported_address_kind")
    if value["kind"] not in MAIL_KINDS:
        raise ConfigError("invalid_kind")
    if len(value["subject"].encode("utf-8")) > 1024:
        raise ConfigError("subject_too_large")
    try:
        dt.datetime.strptime(value["sent"], "%Y-%m-%d %H:%M:%S %z")
    except ValueError:
        raise ConfigError("invalid_sent")
    unknown = set(value) - ENVELOPE_REQUIRED - ENVELOPE_OPTIONAL
    if unknown and logger is not None:
        logger.emit("unknown_envelope_keys", id=expected_id, keys=sorted(unknown))
    return value


def receipt_bytes(status, host, room, mail_id, sha256, reason):
    value = {
        "v": 1,
        "status": status,
        "host": host,
        "room": room,
        "id": mail_id,
        "sha256": sha256,
        "reason": reason,
        "at": utc_now(),
    }
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode(
        "utf-8"
    )


def delivered_receipt(settings, host, room, mail_id, sha256):
    """This node's ``delivered`` receipt for exactly this letter, else ``None``.

    A delivered receipt is final: the sender retires the letter on seeing it,
    so nothing that changes later (a room removed, a name contested) may take
    it back. "Exactly this letter" is the (host, room, id, sha256) it names.
    """
    path = destination(settings.repo, "receipts", host, room, mail_id + ".json")
    try:
        value = load_json_bytes(open_regular(path, 4096), str(path))
    except (FileNotFoundError, ConfigError, OSError):
        return None
    if isinstance(value, dict) and all(
        (
            value.get("v") == 1,
            value.get("status") == "delivered",
            value.get("host") == host,
            value.get("room") == room,
            value.get("id") == mail_id,
            value.get("sha256") == sha256,
        )
    ):
        return value
    return None


def write_receipt(settings, host, room, mail_id, status, sha256, reason, logger):
    if fence_present(settings):
        raise TickError("fenced")
    if status != "delivered" and delivered_receipt(
        settings, host, room, mail_id, sha256
    ):
        # Never downgrade: process_inbound checks for a delivered letter
        # before any current routing condition, so this is the backstop.
        logger.emit(
            "receipt_downgrade_refused",
            status=status,
            host=host,
            room=room,
            id=mail_id,
            reason=reason,
        )
        return
    path = destination(settings.repo, "receipts", host, room, mail_id + ".json")
    payload = receipt_bytes(status, host, room, mail_id, sha256, reason)
    existing = None
    try:
        existing = open_regular(path, 4096)
    except FileNotFoundError:
        pass
    if existing is not None:
        try:
            value = load_json_bytes(existing, str(path))
        except ConfigError:
            value = None
        if isinstance(value, dict) and all(
            (
                value.get("v") == 1,
                value.get("status") == status,
                value.get("host") == host,
                value.get("room") == room,
                value.get("id") == mail_id,
                value.get("sha256") == sha256,
                value.get("reason") == reason,
            )
        ):
            return
    if existing != payload:
        atomic_replace(path, payload, settings.repo)
        logger.emit(
            "receipt", status=status, host=host, room=room, id=mail_id, reason=reason
        )


def forensic_copy(settings, host, room, mail_id, data, suffix, logger):
    if fence_present(settings):
        raise TickError("fenced")
    path = destination(
        settings.root,
        "bridge",
        "quarantine",
        host,
        room,
        mail_id + suffix,
    )
    try:
        exclusive_publish(
            path, data, destination(settings.root, "bridge", "tmp"), settings.root
        )
    except FileExistsError:
        try:
            if open_regular(path, settings.max_mail_bytes) != data:
                conflict = path.with_name(mail_id + f"-{host}-conflict.mail")
                exclusive_publish(
                    conflict,
                    data,
                    destination(settings.root, "bridge", "tmp"),
                    settings.root,
                )
        except (FileExistsError, ConfigError):
            pass
    logger.emit("forensic", host=host, room=room, id=mail_id, path=str(path))


def write_forensic_note(path, note, root):
    """Write a forensic note unless the same note is already there.

    A standing condition re-notes on every full tick; only ``at`` differs, so
    an otherwise identical note keeps its bytes and its first ``at``.
    """
    try:
        prior = load_json_bytes(open_regular(path, 64 * 1024), str(path))
    except (FileNotFoundError, ConfigError, OSError):
        prior = None
    if isinstance(prior, dict):
        unchanged = {key: value for key, value in prior.items() if key != "at"}
        if unchanged == {key: value for key, value in note.items() if key != "at"}:
            return
    atomic_replace(
        path, (json.dumps(note, sort_keys=True) + "\n").encode("utf-8"), root
    )


def forensic_note(settings, host, room, mail_id, reason, entry, logger):
    if fence_present(settings):
        raise TickError("fenced")
    path = destination(
        settings.root,
        "bridge",
        "quarantine",
        host,
        room,
        mail_id + ".json",
    )
    note = {
        "host": host,
        "room": room,
        "id": mail_id,
        "reason": reason,
        "path": entry["path"],
        "mode": entry["mode"],
        "size": entry["size"],
        "object": entry["object"],
        "at": utc_now(),
    }
    write_forensic_note(path, note, settings.root)
    logger.emit("forensic", host=host, room=room, id=mail_id, path=str(path))


def forensic_path_note(settings, host, entry, reason, logger):
    if fence_present(settings):
        raise TickError("fenced")
    fingerprint = hashlib.sha256(entry["path"].encode("utf-8")).hexdigest()
    path = destination(
        settings.root,
        "bridge",
        "quarantine",
        host,
        "_paths",
        fingerprint + ".json",
    )
    note = {
        "host": host,
        "reason": reason,
        "path": entry["path"],
        "mode": entry["mode"],
        "size": entry["size"],
        "object": entry["object"],
        "at": utc_now(),
    }
    write_forensic_note(path, note, settings.root)
    logger.emit("forensic", host=host, id=fingerprint, path=str(path))


def forensic_invalid_path_note(settings, host, entry, logger):
    if fence_present(settings):
        raise TickError("fenced")
    raw_path = entry["raw_path"]
    fingerprint = hashlib.sha256(raw_path).hexdigest()
    path = destination(
        settings.root,
        "bridge",
        "quarantine",
        host,
        "_paths",
        fingerprint + ".json",
    )
    note = {
        "host": host,
        "reason": "invalid_utf8",
        "path_sha256": fingerprint,
        "path_hex": raw_path.hex(),
        "mode": entry["mode"],
        "size": entry["size"],
        "object": entry["object"],
        "at": utc_now(),
    }
    write_forensic_note(path, note, settings.root)
    logger.emit("forensic", host=host, id=fingerprint, path=str(path))
    return fingerprint


def unread_descriptor_sha(host, entry):
    descriptor = f"{host}/{entry['path']}@{entry['object']}"
    return hashlib.sha256(descriptor.encode("utf-8")).hexdigest()


def quarantine(
    settings, host, room, mail_id, sha256, reason, data, logger, conflict=False
):
    if data is not None:
        suffix = f"-{host}-conflict.mail" if conflict else ".mail"
        forensic_copy(settings, host, room, mail_id, data, suffix, logger)
    write_receipt(settings, host, room, mail_id, "quarantined", sha256, reason, logger)
    logger.emit("quarantined", host=host, room=room, id=mail_id, reason=reason)


def local_collision(settings, host, room, mail_id, sha256):
    delivered_root = destination(settings.root, "bridge", "delivered")
    if delivered_root.exists():
        for other in delivered_root.glob(f"*/*/{mail_id}"):
            if other != destination(delivered_root, host, room, mail_id):
                return True
    ledger = destination(settings.root, "bridge", "delivered", host, room, mail_id)
    try:
        stored = open_regular(ledger, 65).decode("ascii").strip()
        if stored != sha256:
            return True
    except FileNotFoundError:
        pass
    except (ConfigError, UnicodeDecodeError):
        return True
    copies = [
        destination(settings.root, "archive", mail_id + ".mail"),
        destination(settings.root, room, "inbox", mail_id + ".mail"),
        destination(settings.root, room, "read", mail_id + ".mail"),
    ]
    for path in copies:
        try:
            if (
                hashlib.sha256(open_regular(path, settings.max_mail_bytes)).hexdigest()
                != sha256
            ):
                return True
        except FileNotFoundError:
            pass
        except ConfigError:
            return True
    reservation = destination(settings.root, "bridge", "received", mail_id)
    try:
        reserved = open_regular(reservation, 256).decode("ascii").strip()
    except FileNotFoundError:
        reserved = ""
    except (ConfigError, UnicodeDecodeError):
        return True
    # Durable collision reservation (SPEC r3.5 I3(c)/I6): present with a
    # different (host, room) or sha means another K owns this id; the same
    # triple means resume. An empty marker (pre-S2 test runs only) claims
    # nothing and is refreshed at the publish step.
    return bool(reserved) and reserved != f"{host}/{room} {sha256}"


def publish_marker(settings, path, content=b"", temp_subdir="tmp"):
    try:
        exclusive_publish(
            path,
            content,
            destination(settings.root, "bridge", temp_subdir),
            settings.root,
        )
    except FileExistsError:
        return False
    return True


def process_inbound(
    settings, config, git, real_rooms, placeholders, logger, snapshot=None, holds=None
):
    held = 0
    quarantined = 0
    if holds is None:
        holds = HoldLedger(settings.root, "held-not-homed")
    post_rooms = snapshot.post_rooms if snapshot is not None else None
    peer_hosts = snapshot.peers if snapshot is not None else sorted(config.peers)
    for host in peer_hosts:
        git.deadline.check()
        ref = (
            snapshot.oids.get(f"machines/{host}")
            if snapshot is not None
            else git.rev(f"origin/machines/{host}")
        )
        if ref is None:
            logger.host_unread(host)
            logger.emit("peer_branch_missing", host=host)
            continue
        try:
            entries = git.ls_tree(ref, f"outbox/{settings.host}/")
        except GitReadError as error:
            note_git_failure(git, logger, host, error)
            continue
        for entry in entries:
            git.deadline.check()
            if entry["path"] is None:
                fingerprint = forensic_invalid_path_note(
                    settings, host, entry, logger
                )
                logger.emit(
                    "quarantined_path",
                    host=host,
                    id=fingerprint,
                    reason="invalid_utf8",
                )
                continue
            path = PurePosixPath(entry["path"])
            parts = path.parts
            if len(parts) != 4 or parts[0] != "outbox" or parts[1] != settings.host:
                forensic_path_note(settings, host, entry, "invalid_path", logger)
                logger.emit(
                    "quarantined_path",
                    host=host,
                    path=entry["path"],
                    reason="invalid_path",
                )
                continue
            room = parts[2]
            filename = parts[3]
            try:
                validate_room(room, topology=True, label="outbox room")
                if not filename.endswith(".mail"):
                    raise ConfigError("outbox filename must end in .mail")
                mail_id = filename[:-5]
                validate_id(mail_id)
            except ConfigError as error:
                note_id = hashlib.sha256(entry["path"].encode("utf-8")).hexdigest()[:22]
                forensic_path_note(settings, host, entry, str(error), logger)
                logger.emit(
                    "quarantined_path",
                    host=host,
                    path=entry["path"],
                    id=note_id,
                    reason=str(error),
                )
                continue
            if entry["mode"] != "100644" or entry["type"] != "blob":
                reason = "non-regular-object"
                sha256 = unread_descriptor_sha(host, entry)
                forensic_note(settings, host, room, mail_id, reason, entry, logger)
                write_receipt(
                    settings,
                    host,
                    room,
                    mail_id,
                    "quarantined",
                    sha256,
                    reason,
                    logger,
                )
                logger.emit(
                    "quarantined", host=host, room=room, id=mail_id, reason=reason
                )
                quarantined += 1
                continue
            if entry["size"] is None or entry["size"] > settings.max_mail_bytes:
                reason = "oversize-unread"
                sha256 = unread_descriptor_sha(host, entry)
                forensic_note(settings, host, room, mail_id, reason, entry, logger)
                write_receipt(
                    settings,
                    host,
                    room,
                    mail_id,
                    "quarantined",
                    sha256,
                    reason,
                    logger,
                )
                logger.emit(
                    "quarantined", host=host, room=room, id=mail_id, reason=reason
                )
                quarantined += 1
                continue
            data = git.show(ref, entry["path"])
            if (
                data is None
                or len(data) != entry["size"]
                or len(data) > settings.max_mail_bytes
            ):
                reason = "unreadable-object"
                sha256 = unread_descriptor_sha(host, entry)
                forensic_note(settings, host, room, mail_id, reason, entry, logger)
                quarantine(
                    settings,
                    host,
                    room,
                    mail_id,
                    sha256,
                    reason,
                    None,
                    logger,
                )
                quarantined += 1
                continue
            sha256 = hashlib.sha256(data).hexdigest()
            ledger = destination(
                settings.root, "bridge", "delivered", host, room, mail_id
            )
            try:
                stored_sha = open_regular(ledger, 65).decode("ascii").strip()
            except (FileNotFoundError, ConfigError, UnicodeDecodeError):
                stored_sha = None
            # SPEC r3.5 I3½: a ledger hit with the matching sha replays — it
            # skips ONLY the route evaluation below; fence rechecks and state
            # repair still run (receipts move held→delivered, never back).
            replay = stored_sha == sha256
            # A letter this node already delivered is final, whether the
            # ledger or the receipt says so. The sender retires (or, for a
            # refusal, bounces) a letter on the strength of the receipt, so
            # the receipt is decided before anything that can change later:
            # the room still being registered, the sender's name still being
            # uncontested. Such a change never takes a delivery back.
            settled = replay or delivered_receipt(
                settings, host, room, mail_id, sha256
            ) is not None
            if settled and room not in real_rooms:
                # Nothing left to route to. A crash between the ledger and the
                # receipt leaves the receipt missing; this writes it.
                write_receipt(
                    settings, host, room, mail_id, "delivered", sha256, "", logger
                )
                continue
            if room not in real_rooms:
                quarantine(
                    settings,
                    host,
                    room,
                    mail_id,
                    sha256,
                    "unknown_room",
                    data,
                    logger,
                )
                quarantined += 1
                continue
            try:
                envelope = parse_envelope(data, mail_id, room, logger)
            except ConfigError as error:
                if settled:
                    write_receipt(
                        settings, host, room, mail_id, "delivered", sha256, "", logger
                    )
                    checkpoint("inbound-i1")
                    continue
                quarantine(
                    settings, host, room, mail_id, sha256, str(error), data, logger
                )
                quarantined += 1
                checkpoint("inbound-i1")
                continue
            checkpoint("inbound-i1")
            sender = envelope["from"]
            verdict = (
                binding_verdict(snapshot, host, sender)
                if snapshot is not None
                else (
                    FORGED_SELF
                    if sender in real_rooms
                    or (sender in placeholders and placeholders[sender] != host)
                    else UNHOMED
                    if sender not in placeholders
                    else "verified"
                )
            )
            if verdict in (FORGED_SELF, NAME_COLLISION, UNPUBLISHED_SENDER):
                if settled:
                    # Delivered before the sender's name became contested (or
                    # a room took it): the delivery stands, nothing repairs.
                    write_receipt(
                        settings, host, room, mail_id, "delivered", sha256, "", logger
                    )
                    checkpoint("inbound-i2")
                    continue
                # SPEC-v2 §What this changes in v1 mail: the reason is the
                # verdict. It never depends on whether a registry branch is
                # fetchable this tick (review D3).
                quarantine(
                    settings, host, room, mail_id, sha256, verdict, data, logger
                )
                quarantined += 1
                checkpoint("inbound-i2")
                continue
            # r5.5 (M2): deliver on the evidence post consumes. These checks
            # run only for a first delivery; a settled letter keeps its outcome.
            if not settled and verdict == VERIFIED and not post_sees_remote(
                settings.root, host, sender, post_rooms
            ):
                # post would not read this sender as remote: hold without a
                # receipt, so the bytes stay on the peer branch and every
                # full tick retries.
                holds.hold(host, room, mail_id)
                logger.emit(
                    "held",
                    host=host,
                    room=room,
                    id=mail_id,
                    sender=sender,
                    reason=SENDER_NOT_HOMED,
                )
                checkpoint("inbound-i2")
                continue
            if (
                not settled
                and verdict == UNHOMED
                and envelope.get("from_participant") is not None
            ):
                # No remote-origin evidence reaches post for an unhomed
                # sender, so a participant stamp would read as local.
                quarantine(
                    settings,
                    host,
                    room,
                    mail_id,
                    sha256,
                    REMOTE_PARTICIPANT_UNHOMED,
                    data,
                    logger,
                )
                quarantined += 1
                checkpoint("inbound-i2")
                continue
            if verdict == UNHOMED:
                logger.emit(
                    "from-unhomed", host=host, room=room, id=mail_id, sender=sender
                )
            checkpoint("inbound-i2")
            if local_collision(settings, host, room, mail_id, sha256):
                quarantine(
                    settings,
                    host,
                    room,
                    mail_id,
                    sha256,
                    "id-collision",
                    data,
                    logger,
                    conflict=True,
                )
                quarantined += 1
                checkpoint("inbound-i3")
                continue
            checkpoint("inbound-i3")
            if fence_present(settings):
                raise TickError("fenced")
            checkpoint("inbound-i4")
            if not settled:
                rules = load_rules(settings)
                reason = blocked_reason(rules, sender, room)
                if reason is not None:
                    write_receipt(
                        settings, host, room, mail_id, "held", sha256, reason, logger
                    )
                    logger.emit(
                        "held", host=host, room=room, id=mail_id, reason=reason
                    )
                    held += 1
                    checkpoint("inbound-i5")
                    continue
            checkpoint("inbound-i5")
            if fence_present(settings):
                raise TickError("fenced")
            reservation = destination(settings.root, "bridge", "received", mail_id)
            # First write of every delivery: the durable collision reservation
            # `<host>/<room> <sha>` consulted by local_collision at I3(c), and
            # still the bridge-origin exclusion for outbound selection.
            reservation_content = f"{host}/{room} {sha256}\n".encode("ascii")
            if not publish_marker(settings, reservation, reservation_content):
                try:
                    if open_regular(reservation, 32) == b"":
                        atomic_replace(
                            reservation, reservation_content, settings.root
                        )
                except (ConfigError, OSError):
                    pass
            checkpoint("inbound-received")
            room_path = destination(settings.root, room)
            ensure_dir(settings.root, destination(room_path, "inbox"))
            ensure_dir(settings.root, destination(room_path, "read"))
            inbox = destination(room_path, "inbox", mail_id + ".mail")
            read = destination(room_path, "read", mail_id + ".mail")
            archive = destination(settings.root, "archive", mail_id + ".mail")
            inbox_present = inbox.exists() or inbox.is_symlink()
            read_present = read.exists() or read.is_symlink()
            archive_present = archive.exists() or archive.is_symlink()
            if not inbox_present and not read_present and archive_present:
                logger.emit("archive-only", host=host, room=room, id=mail_id)
            if not inbox_present and not read_present and not archive_present:
                if fence_present(settings):
                    raise TickError("fenced")
                if replay:
                    logger.emit(
                        "ledger_only_repaired",
                        host=host,
                        room=room,
                        id=mail_id,
                    )
                try:
                    exclusive_publish(
                        inbox,
                        data,
                        destination(settings.root, "bridge", "tmp"),
                        settings.root,
                    )
                    logger.emit("inbox_delivered", host=host, room=room, id=mail_id)
                except FileExistsError:
                    if local_collision(settings, host, room, mail_id, sha256):
                        quarantine(
                            settings,
                            host,
                            room,
                            mail_id,
                            sha256,
                            "id-collision",
                            data,
                            logger,
                            conflict=True,
                        )
                        quarantined += 1
                        continue
            checkpoint("inbound-i6")
            if not archive_present:
                if fence_present(settings):
                    raise TickError("fenced")
                try:
                    exclusive_publish(
                        archive,
                        data,
                        destination(settings.root, "bridge", "tmp"),
                        settings.root,
                    )
                    logger.emit("archive_delivered", host=host, room=room, id=mail_id)
                except FileExistsError:
                    if (
                        hashlib.sha256(
                            open_regular(archive, settings.max_mail_bytes)
                        ).hexdigest()
                        != sha256
                    ):
                        quarantine(
                            settings,
                            host,
                            room,
                            mail_id,
                            sha256,
                            "id-collision",
                            data,
                            logger,
                            conflict=True,
                        )
                        quarantined += 1
                        continue
            checkpoint("inbound-i7")
            ledger = destination(
                settings.root, "bridge", "delivered", host, room, mail_id
            )
            publish_marker(settings, ledger, (sha256 + "\n").encode("ascii"))
            checkpoint("inbound-i8")
            write_receipt(
                settings, host, room, mail_id, "delivered", sha256, "", logger
            )
            logger.emit("delivered", host=host, room=room, id=mail_id, sha256=sha256)
            checkpoint("inbound-i9")
        # The host's outbox was walked in full: stamps for mail it no longer
        # holds (delivered, quarantined, withdrawn) are dropped.
        holds.settle(host)
    return held, quarantined


def mail_age_seconds(mail_id):
    try:
        moment = dt.datetime.strptime(mail_id[:15], "%Y%m%d-%H%M%S").replace(
            tzinfo=dt.timezone.utc
        )
        return max(0, int((dt.datetime.now(dt.timezone.utc) - moment).total_seconds()))
    except ValueError:
        return None


def marker_exists(settings, namespace, mail_id):
    # Presence-only checks for outbound selection and prune gating. The
    # `received/<id>` reservation's *content* is read only by local_collision.
    return destination(settings.root, "bridge", namespace, mail_id).exists()


def delivered_id_exists(settings, mail_id):
    root = destination(settings.root, "bridge", "delivered")
    return root.exists() and any(root.glob(f"*/*/{mail_id}"))


def select_outbound(
    settings, config, real_rooms, placeholders, logger, deadline, snapshot=None,
    typed=None, guard=None,
):
    """Choose archive letters for the workspace outbox.

    Typed letters (lineage, participant, or any ``to_host``) never enter
    ``outbox/``; each is logged once as ``outbound_typed_skipped``. When
    ``typed`` is a list, host-qualified participant letters are appended to
    it as ``(path, mail_id, data, envelope)`` for the pmail sender pass.

    r6.1: every workspace candidate passes the local-held guard before it
    is routed; a held letter is never selected. Without a ``guard`` the
    selection builds its own, so no caller can skip it. A tick that ends
    with a store-level guard fault selects no workspace letter at all.

    Decided ids (bridgelib/decided.py) are recognised from directory
    listings and never opened: received, delivered and published letters,
    held letters whose marker is inside its recheck window, and letters
    already judged unrelayable whose file has not changed. A letter is opened
    only when it is new, its verdict is stale, or its verdict cannot be
    permanent (no route yet).

    A letter whose envelope is invalid is flagged unrelayable only when its
    recipient routes to a peer. One that only ever reaches a local room
    (free-form ``from`` on a probe letter) never needed relaying, so it is
    left alone instead of being reported forever.
    """
    if guard is None:
        guard = local_held_guard(settings, real_rooms, snapshot, logger)
    selected = []
    unrelayable = []
    archive_root = destination(settings.root, "archive")
    if not archive_root.is_dir():
        checkpoint("outbound-o1-select")
        return selected, unrelayable
    index = decided.ArchiveIndex(settings)
    for path in sorted(archive_root.iterdir()):
        deadline.check()
        if path.suffix != ".mail":
            continue
        mail_id = path.stem
        try:
            validate_id(mail_id)
        except ConfigError as error:
            logger.emit("outbound_ignored", id=mail_id, reason=str(error))
            unrelayable.append(mail_id)
            continue
        if index.marked(mail_id):
            continue
        if index.held_trusted(mail_id):
            guard.carry_decided(mail_id)
            continue
        if mail_id in index.unrelayable:
            record = decided.unrelayable_still_true(settings, mail_id, path)
            if record is not None:
                logger.unrelayables[mail_id] = record["reason"]
                unrelayable.append(mail_id)
                continue
        if pmail.typed_skip_known(settings, mail_id):
            continue
        before = None
        try:
            before = path.lstat()
            data = open_regular(path, settings.max_mail_bytes)
            header = outbound_header(data)
        except (ConfigError, OSError) as error:
            # Only a verdict about the file's content is remembered. A read
            # that failed on I/O (permissions, a vanished file) is retried
            # every tick, because the file may be perfectly fine.
            flag_unrelayable(
                settings,
                logger,
                unrelayable,
                mail_id,
                str(error),
                before if isinstance(error, ConfigError) else None,
            )
            continue
        if not workspace_addressed(header):
            pmail.note_typed(settings, mail_id, header, logger)
            if typed is not None and pmail.is_pmail(header):
                typed.append((path, mail_id, data, header))
            continue
        try:
            envelope = parse_envelope(data, mail_id, header.get("to"), logger)
        except ConfigError as error:
            if routes_to_peer(header.get("to"), snapshot, placeholders):
                flag_unrelayable(
                    settings, logger, unrelayable, mail_id, str(error), before
                )
            continue
        if guard.held(mail_id, data, envelope):
            if guard.last_hold_valid:
                # The guard's record and index line are already written; the
                # marker only records that they verified.
                checkpoint("decided-d0-before-marker")
                decided.mark_held(settings, mail_id)
                checkpoint("decided-d1-marked")
            continue
        recipient = envelope["to"]
        sender = envelope["from"]
        if snapshot is not None:
            host = snapshot.route_for(recipient)
            if host is None:
                # r5.3 §Names fold: `contested` is keyed by the ASCII
                # case-fold; `recipient` is the envelope's display spelling.
                if snapshot.contested.get(fold_name(recipient)) is not None:
                    logger.emit(
                        "route_contested",
                        room=recipient,
                        id=mail_id,
                        age_seconds=mail_age_seconds(mail_id),
                    )
                continue
        else:
            host = placeholders.get(recipient)
        if host is None:
            continue
        if (
            sender in snapshot.placeholders
            if snapshot is not None
            else sender in placeholders
        ):
            continue
        selected.append((path, host, recipient, mail_id, data))
    faults = guard.store_faults()
    if faults:
        # r6.1.1: a store-level fault, even one raised mid-selection, means
        # no workspace letter is exported this tick. Typed letters are not
        # in `selected` and are unaffected.
        logger.emit("local_held_export_blocked", faults=faults, dropped=len(selected))
        selected = []
    checkpoint("outbound-o1-select")
    return selected, unrelayable[:20]


def routes_to_peer(recipient, snapshot, placeholders):
    """Whether ``recipient`` (an envelope ``to``, not yet validated) is a room
    some peer host owns, so a letter to it would leave this host."""
    if not isinstance(recipient, str):
        return False
    if snapshot is not None:
        return snapshot.route_for(recipient) is not None
    return placeholders.get(recipient) is not None


def flag_unrelayable(settings, logger, unrelayable, mail_id, reason, archive_stat):
    """Report a letter that cannot be relayed and remember the verdict.

    The marker is written after the log line, so a crash between them logs
    the letter again next tick and never leaves it undecided-and-silent.
    """
    logger.emit("outbound_ignored", id=mail_id, reason=reason)
    unrelayable.append(mail_id)
    if archive_stat is not None:
        try:
            decided.mark_unrelayable(settings, mail_id, reason, archive_stat)
        except (ConfigError, OSError):
            pass  # a lost marker costs one re-read next tick


def local_held_guard(settings, real_rooms, snapshot, logger):
    contested = frozenset(snapshot.contested) if snapshot is not None else None
    # The previous health carries what the store held, so a wiped store is
    # a fault rather than a fresh install (and stays one until restored).
    prior = read_health(settings).get("local_held")
    return localheld.Guard(settings, real_rooms, contested, logger, prior=prior)


def workspace_addressed(value):
    # F3-R2-B: workspace mail has address_kind absent or `workspace` AND no
    # `to_host`. A host-qualified letter is never room mail, whatever its kind.
    return value.get("address_kind", "workspace") == "workspace" and "to_host" not in value


def outbound_header(data):
    """An archive letter's envelope object, checked only enough to route it.

    r5.4: `send --to lineage:<n>` / `participant:<id>` archive with `to` set
    to the bare lineage name or participant id. Such mail is host-local (or,
    with `to_host`, pmail) and is skipped before routing, so a peer room of
    the same name never gets it.
    """
    separator = data.find(b"\n---\n")
    if separator < 0 or separator > 4096:
        raise ConfigError("malformed_header")
    value = load_json_bytes(data[:separator], "mail envelope")
    if not isinstance(value, dict) or not ENVELOPE_REQUIRED.issubset(value):
        raise ConfigError("malformed_header")
    return value


def copy_outbound(settings, selected, logger):
    for archive, host, room, mail_id, data in selected:
        target = destination(settings.repo, "outbox", host, room, mail_id + ".mail")
        try:
            existing = open_regular(target, settings.max_mail_bytes)
        except FileNotFoundError:
            existing = None
        if existing is not None:
            if existing != data:
                raise ConfigError(
                    f"outbox differs from immutable archive for {mail_id}"
                )
            continue
        # Who sent this letter, as of now: what a later bounce is routed by.
        bounce.record_origin(settings, mail_id, data)
        try:
            exclusive_publish(target, data, target.parent, settings.repo)
        except FileExistsError:
            if open_regular(target, settings.max_mail_bytes) != data:
                raise ConfigError(f"outbox collision for {mail_id}")
        logger.emit(
            "outbound_copied", host=host, room=room, id=mail_id, archive=str(archive)
        )
        checkpoint("outbound-o2-copy")


def parse_receipt(data, expected_host, expected_room, expected_id, expected_sha):
    value = load_json_bytes(data, "receipt")
    if not isinstance(value, dict) or set(value) != RECEIPT_KEYS:
        raise ConfigError("receipt has wrong keys")
    if value["v"] != 1 or value["status"] not in RECEIPT_STATUSES:
        raise ConfigError("receipt version or status is invalid")
    for key in ("host", "room", "id", "sha256", "reason", "at"):
        if not isinstance(value[key], str):
            raise ConfigError(f"receipt {key} must be a string")
    if (
        value["host"] != expected_host
        or value["room"] != expected_room
        or value["id"] != expected_id
    ):
        raise ConfigError("receipt path fields do not match")
    if SHA256_RE.fullmatch(value["sha256"]) is None or value["sha256"] != expected_sha:
        raise ConfigError("receipt sha256 does not match outbox")
    return value


def prune_outbox(settings, config, git, logger, snapshot=None, refusals=None):
    """Retire outbox entries the receiver has settled.

    A ``delivered`` receipt prunes the entry. A terminal refusal (bridgelib/
    bounce.py) tells the sending participant with a system letter and then
    retires the entry the same way; ``refusals`` collects the attention items
    for refusals that could not be bounced this tick.
    """
    root = destination(settings.repo, "outbox")
    if not root.exists():
        checkpoint("outbound-o5-prune")
        return 0
    real_rooms = snapshot.real_rooms if snapshot is not None else {}
    pruned = 0
    for path in sorted(root.glob("*/*/*.mail")):
        relative = path.relative_to(settings.repo).as_posix()
        parts = PurePosixPath(relative).parts
        if len(parts) != 4:
            continue
        host, room, filename = parts[1], parts[2], parts[3]
        if not filename.endswith(".mail"):
            logger.emit("outbox_invalid", path=relative)
            continue
        mail_id = filename[:-5]
        if not marker_exists(settings, "published", mail_id):
            continue
        try:
            validate_id(mail_id)
            data = open_regular(path, settings.max_mail_bytes)
        except (ConfigError, FileNotFoundError) as error:
            logger.emit("outbox_invalid", path=relative, reason=str(error))
            continue
        sha256 = hashlib.sha256(data).hexdigest()
        ref = (
            snapshot.oids.get(f"machines/{host}")
            if snapshot is not None
            else f"origin/machines/{host}"
        )
        if ref is None:
            logger.host_unread(host)
            continue
        receipt_path = f"receipts/{settings.host}/{room}/{mail_id}.json"
        try:
            listed = git.ls_tree(ref, receipt_path)
        except GitReadError as error:
            note_git_failure(git, logger, host, error)
            continue
        matches = [entry for entry in listed if entry["path"] == receipt_path]
        if len(matches) != 1:
            continue
        entry = matches[0]
        if (
            entry["mode"] != "100644"
            or entry["type"] != "blob"
            or entry["size"] is None
            or entry["size"] > 4096
        ):
            logger.emit(
                "receipt_ignored",
                host=host,
                room=room,
                id=mail_id,
                reason="invalid_mode_or_size",
            )
            continue
        try:
            receipt_data = git.show(ref, receipt_path)
            if (
                receipt_data is None
                or len(receipt_data) != entry["size"]
                or len(receipt_data) > 4096
            ):
                raise ConfigError("unreadable-object")
            receipt = parse_receipt(
                receipt_data, settings.host, room, mail_id, sha256
            )
        except ConfigError as error:
            logger.emit(
                "receipt_ignored", host=host, room=room, id=mail_id, reason=str(error)
            )
            continue
        if bounce.is_terminal(receipt, time.time()):
            try:
                bounce.notify(
                    settings, host, room, mail_id, data, receipt, logger, real_rooms
                )
            except (ConfigError, OSError) as error:
                logger.emit(
                    "bounce_failed", host=host, room=room, id=mail_id, reason=str(error)
                )
                if refusals is not None:
                    if isinstance(error, bounce.BounceConflict):
                        # A record on disk does not describe this letter:
                        # keep the entry, never retire on a guess.
                        refusals.append(
                            attention.refused_bounce_conflict(
                                mail_id, host, room, receipt["reason"], error
                            )
                        )
                    else:
                        refusals.append(
                            attention.refused_bounce_failed(
                                mail_id, host, room, receipt["reason"], error
                            )
                        )
                continue
            git.run(["rm", "--quiet", "--", relative])
            pruned += 1
            logger.emit(
                "outbox_bounced", host=host, room=room, id=mail_id,
                reason=receipt["reason"],
            )
            checkpoint("outbound-o6-bounced")
            continue
        if receipt["status"] != "delivered":
            logger.emit(
                "outbound_waiting",
                host=host,
                room=room,
                id=mail_id,
                status=receipt["status"],
                reason=receipt["reason"],
                age_seconds=mail_age_seconds(mail_id),
            )
            continue
        git.run(["rm", "--quiet", "--", relative])
        pruned += 1
        logger.emit("outbox_pruned", host=host, room=room, id=mail_id)
        checkpoint("outbound-o5-prune")
    checkpoint("outbound-o5-prune")
    return pruned



def queued_work(settings, git):
    remote_ref = f"origin/machines/{settings.host}"
    remote = git.rev(remote_ref)
    if remote is None:
        return True
    local_ahead = git.rev("HEAD") != remote
    commands = (
        ["diff", "--name-only", "-z", remote_ref, "HEAD", "--", *RELAY_PATHS],
        ["diff", "--name-only", "-z", "--", *RELAY_PATHS],
        ["diff", "--cached", "--name-only", "-z", "--", *RELAY_PATHS],
        ["ls-files", "--others", "--exclude-standard", "-z", "--", *RELAY_PATHS],
    )
    changed = set()
    for command in commands:
        result = git.run(command, check=False, text=False)
        if result.returncode != 0:
            return True
        for raw_path in result.stdout.split(b"\0"):
            if not raw_path:
                continue
            try:
                changed.add(raw_path.decode("utf-8"))
            except UnicodeDecodeError:
                return True
    # Ruling R3: unpushed receipts of EVERY status are queued work — held
    # included (the sender cannot see the hold until the receipt lands).
    return local_ahead or bool(changed)


def commit_and_push(settings, config, git, rooms, logger, has_queued_work):
    for namespace in RELAY_NAMESPACES:
        ensure_dir(settings.repo, destination(settings.repo, namespace))
    git.run(["add", "-A", "--", *RELAY_PATHS])
    checkpoint("outbound-o3-stage")
    changed = git.run(["diff", "--cached", "--quiet"], check=False).returncode != 0
    if changed:
        git.run(
            [
                "-c",
                "user.name=post-bridge",
                "-c",
                f"user.email=post-bridge@{settings.host}",
                "commit",
                "-m",
                f"post-bridge: {settings.host} tick {utc_now()}",
            ]
        )
        logger.emit("committed", head=git.rev("HEAD"))
        checkpoint("after-commit")
    local = git.rev("HEAD")
    remote_ref = f"origin/machines/{settings.host}"
    remote = git.rev(remote_ref)
    pushed = False
    if remote != local:
        result = git.run(
            ["push", "origin", f"HEAD:refs/heads/machines/{settings.host}"],
            check=False,
            network=True,
        )
        if result.returncode != 0:
            output = result.stdout + result.stderr
            refreshed_ok = refresh_remote_refs(git, logger)
            refreshed = git.rev(remote_ref) if refreshed_ok else remote
            if (
                "rejected" in output.lower()
                or "non-fast-forward" in output.lower()
                or (refreshed is not None and not git.is_ancestor(refreshed, local))
            ):
                raise TickError("branch_diverged", output)
            logger.emit("push_failed", queued_work=has_queued_work)
            if has_queued_work:
                raise TickError("push_failed", output)
            return False
        pushed = True
        # The crash checkpoint sits at the instant the push subprocess
        # returned success — before any derivation (finding 4): a hard kill
        # here leaves the marker absent and the next tick's step-0 derivation
        # reconstructs it from the fetched tree.
        checkpoint("after-push")
        derive_published_and_tidy(
            settings, config, git, rooms, logger, "HEAD", tidy=True
        )
        # F3-6: HEAD is exactly what the push just carried.
        pmail.derive_markers(settings, git, logger, "HEAD")
        checkpoint("after-derive")
        if git.rev(remote_ref) != local:
            refreshed_ok = refresh_remote_refs(git, logger)
            if not refreshed_ok:
                logger.emit("fetch", ok=False, reason="post_push_fetch_failed")
                git.run(
                    [
                        "update-ref",
                        f"refs/remotes/origin/machines/{settings.host}",
                        local,
                    ]
                )
            elif git.rev(remote_ref) != local:
                raise TickError("branch_diverged", "remote changed after successful push")
        logger.emit("pushed", head=local, ref=f"machines/{settings.host}")
    return pushed


def derive_published_and_tidy(settings, config, git, rooms, logger, ref, tidy):
    head = git.rev(ref)
    if head is None:
        return 0
    count = 0
    for entry in git.ls_tree(ref, "outbox/"):
        if entry["path"] is None:
            continue
        parts = PurePosixPath(entry["path"]).parts
        if len(parts) != 4 or parts[0] != "outbox" or not parts[3].endswith(".mail"):
            continue
        host, room, filename = parts[1], parts[2], parts[3]
        mail_id = filename[:-5]
        try:
            validate_id(mail_id)
        except ConfigError:
            continue
        marker = destination(settings.root, "bridge", "published", mail_id)
        created = publish_marker(settings, marker, (head + "\n").encode("ascii"))
        if created:
            logger.emit("published", host=host, room=room, id=mail_id, head=head)
            count += 1
        checkpoint("outbound-o4-published")
        if not tidy:
            continue
        if room in rooms:
            room_root = destination(settings.root, room)
            for directory_name in ("inbox", "read"):
                artifact = destination(room_root, directory_name, mail_id + ".mail")
                try:
                    metadata = artifact.lstat()
                except FileNotFoundError:
                    continue
                if stat.S_ISREG(metadata.st_mode) and under(settings.root, artifact):
                    artifact.unlink()
                    fsync_directory(artifact.parent)
                    logger.emit(
                        "placeholder_tidied",
                        room=room,
                        id=mail_id,
                        store=directory_name,
                    )
        checkpoint("outbound-o4-tidy")
    return count


def read_health(settings):
    path = destination(settings.root, "bridge", "health.json")
    try:
        value = load_json_bytes(open_regular(path, 64 * 1024), str(path))
        return value if isinstance(value, dict) else {}
    except (FileNotFoundError, ConfigError):
        return {}


@contextmanager
def health_lock(root, required):
    # Serializes health.json read-modify-writes across the tick holder and
    # busy probes (SPEC r3.5.1 Health). The installer creates the lock file;
    # the sweeper never does (no O_CREAT). On the fatal-config path a missing
    # lock means write health.json unlocked rather than create a file; on
    # every live path a missing lock is a misinstall and propagates.
    path = destination(root, "bridge", ".health.lock")
    try:
        descriptor = os.open(str(path), os.O_RDWR | getattr(os, "O_NOFOLLOW", 0))
    except FileNotFoundError:
        if required:
            raise
        descriptor = None
    try:
        if descriptor is not None:
            fcntl.flock(descriptor, fcntl.LOCK_EX)
        yield
    finally:
        if descriptor is not None:
            os.close(descriptor)


def bounded_git_failures(value):
    # m6: health carries at most 20 git read failures, each {host, ref, path}
    # with string fields; anything else from a prior file is dropped.
    if not isinstance(value, list):
        return []
    kept = []
    for item in value:
        if isinstance(item, dict) and all(
            isinstance(item.get(key), str) for key in ("host", "ref", "path")
        ):
            kept.append({key: item[key] for key in ("host", "ref", "path")})
    return kept[:20]


def bounded_hold_summary(value):
    # r5.5 (M2) / m7: {host: {count, oldest_age_seconds}} with integer
    # fields; anything else from a prior file is dropped.
    if not isinstance(value, dict):
        return {}
    kept = {}
    for host, item in value.items():
        if (
            isinstance(host, str)
            and isinstance(item, dict)
            and all(
                isinstance(item.get(key), int) and not isinstance(item.get(key), bool)
                for key in ("count", "oldest_age_seconds")
            )
        ):
            kept[host] = {
                "count": item["count"],
                "oldest_age_seconds": item["oldest_age_seconds"],
            }
    return kept


def write_health(
    settings,
    ok,
    reason,
    *,
    held=None,
    quarantined=None,
    outbound_unrelayable=None,
    fetch_ok=None,
    push_ok=None,
    room_stats=None,
    channel_stats=None,
    git_failed=None,
    sender_not_homed=None,
    pmail_stats=None,
    local_held=None,
    attention_items=None,
    quiet=False,
):
    # Every health.json write is a read-modify-write under .health.lock, and
    # every carried field comes only from the prior read taken under that
    # lock (SPEC r3.5.1 Health). None carries the prior value; fetch_ok /
    # push_ok True stamp last_fetch_ok / last_push_ok = now, anything else
    # carries the prior stamp. Fetch staleness is decided here, inside the
    # lock, from the locked prior, and never overrides a caller-supplied
    # unhealthy reason: every `ok and ...` test below is skipped once ok is
    # false. SPEC-v2 §Health removed `undeliverable`; the key is gone from
    # every writer.
    with health_lock(settings.root, required=True):
        prior = read_health(settings)
        now = utc_now()
        first_tick_at = prior.get("first_tick_at")
        if epoch_from_iso(first_tick_at) is None:
            first_tick_at = now
        last_fetch_ok = now if fetch_ok is True else prior.get("last_fetch_ok")
        last_push_ok = now if push_ok is True else prior.get("last_push_ok")
        if held is None:
            held = prior.get("held", 0)
        if quarantined is None:
            quarantined = prior.get("quarantined", 0)
        if outbound_unrelayable is None:
            outbound_unrelayable = prior.get("outbound_unrelayable", [])
        if room_stats is None:
            room_stats = prior.get(
                "rooms",
                {"published": 0, "collisions": [], "retired": [], "route_contested": 0},
            )
        if channel_stats is None:
            channel_stats = prior.get(
                "channels",
                {
                    "imported": 0,
                    "published": 0,
                    "quarantined": 0,
                    "unpublishable": 0,
                    "diverged": [],
                    "rewritten": [],
                },
            )
        if git_failed is None:
            git_failed = prior.get("git_failed", [])
        git_failed = bounded_git_failures(git_failed)
        if sender_not_homed is None:
            sender_not_homed = prior.get("sender_not_homed", {})
        sender_not_homed = bounded_hold_summary(sender_not_homed)
        if pmail_stats is None:
            pmail_stats = prior.get("pmail")
        pmail_stats = pmail.bounded_health(pmail_stats)
        if local_held is None:
            local_held = prior.get("local_held")
        local_held = localheld.bounded_health(local_held)
        if attention_items is None:
            attention_items = prior.get("attention")
        attention_items = attention.bounded(attention_items)
        # First, so a standing room collision cannot mask it.
        if ok and localheld.store_faulted(local_held):
            ok, reason = False, "local_held_store_fault"
        if ok and git_failed:
            ok, reason = False, "git_failed"
        if ok and room_stats.get("collisions"):
            ok, reason = False, "room_name_collision"
        if ok and channel_stats.get("diverged"):
            ok, reason = False, "channel_diverged"
        if ok and channel_stats.get("rewritten"):
            ok, reason = False, "relay_history_rewritten"
        if not isinstance(outbound_unrelayable, list):
            outbound_unrelayable = []
        outbound_unrelayable = [
            mail_id for mail_id in outbound_unrelayable if isinstance(mail_id, str)
        ][:20]
        if ok and fetch_ok is False:
            anchor = epoch_from_iso(last_fetch_ok) or epoch_from_iso(first_tick_at)
            if (
                anchor is None
                or time.time() - anchor > settings.fetch_grace_seconds
            ):
                ok, reason = False, "fetch_stale"
        stalled_since = None if ok else prior.get("stalled_since") or now
        value = {
            "ts": now,
            "ok": bool(ok),
            "reason": reason,
            "first_tick_at": first_tick_at,
            "last_fetch_ok": last_fetch_ok,
            "last_push_ok": last_push_ok,
            "stalled_since": stalled_since,
            "busy_streak": 0,
            "held": held,
            "quarantined": quarantined,
            "outbound_unrelayable": outbound_unrelayable,
            "rooms": room_stats,
            "channels": channel_stats,
            "git_failed": git_failed,
            "sender_not_homed": sender_not_homed,
            "pmail": pmail_stats,
            "local_held": local_held,
            # What is stuck and needs an agent or a human; `ok` above stays
            # a liveness flag (bridgelib/attention.py).
            "attention": attention_items,
            "quiet": bool(quiet),
            "quiet_streak": 0,
        }
        value.update(pmail.capability_fields(interval_of(settings), now))
        path = destination(settings.root, "bridge", "health.json")
        atomic_replace(
            path,
            (json.dumps(value, sort_keys=True) + "\n").encode("utf-8"),
            settings.root,
        )
        return value


def committed_channel_messages(git):
    """Relay paths of the channel messages already committed on this host's
    branch, or ``None`` when they cannot be listed (publication then compares
    every message with the worktree copy, as before). One ``ls-tree`` per
    tick replaces opening every local message to learn it was published."""
    try:
        entries = git.ls_tree("HEAD", "channels/")
    except GitReadError:
        return None
    return {
        entry["path"]
        for entry in entries
        if entry["path"] is not None
        and entry["type"] == "blob"
        and entry["mode"] == "100644"
    }


def build_attention(settings, logger, snapshot, refusals, prior, read_failures):
    """This full tick's attention list (bridgelib/attention.py).

    Everything here is recomputed from what the tick saw, so an item leaves
    the list on the first full tick after its cause is gone. The one carry is
    for inbound quarantines of a peer this tick could not read.
    """
    root = settings.root
    dead_letters = []
    try:
        listed = sorted(decided.names(bounce.dead_letter_dir(settings)))
    except ConfigError:
        listed = []
    for name in listed:
        if name.endswith(".mail"):
            dead_letters.append(
                attention.refused_dead_letter(
                    name[: -len(".mail")], bounce.dead_letter_dir(settings) / name
                )
            )
    unrelayable = []
    ids = sorted(logger.unrelayables)
    retired = destination(root, "bridge", "unrelayable-retired")
    for mail_id in ids[: attention.MAX_UNRELAYABLE]:
        unrelayable.append(
            attention.unrelayable(
                mail_id,
                logger.unrelayables[mail_id],
                destination(root, "archive", mail_id + ".mail"),
                retired,
            )
        )
    if len(ids) > attention.MAX_UNRELAYABLE:
        unrelayable.append(
            attention.unrelayable_overflow(len(ids) - attention.MAX_UNRELAYABLE)
        )
    inbound = []
    for entry in logger.quarantines:
        host, room, mail_id = entry.get("host"), entry.get("room"), entry.get("id")
        forensic = None
        if all(isinstance(value, str) for value in (host, room, mail_id)):
            forensic = destination(
                root, "bridge", "quarantine", host, room, mail_id + ".mail"
            )
        inbound.append(
            attention.quarantined_inbound(
                host, room if room is not None else "?", mail_id,
                str(entry.get("reason", "unknown")), forensic,
            )
        )
    if read_failures:
        inbound.extend(
            entry
            for entry in attention.bounded(prior.get("attention"))
            if entry["kind"] == "quarantined_inbound"
        )
    collisions = [
        attention.name_collision(collision, settings.host)
        for collision in rooms.rooms_health(snapshot).get("collisions", [])
    ]
    # Participant mail held for an archived record: the retry records are the
    # source, so a peer this tick could not read keeps its entries.
    archived = []
    waiting = pmail.RetryLedger(root).waiting(
        set(snapshot.peers), pmail.PARTICIPANT_ARCHIVED
    )
    for participant in sorted(waiting):
        archived.append(
            attention.archived_participant(
                participant,
                waiting[participant],
                destination(root, pmail.PARTICIPANTS_ARCHIVE_DIR, participant),
            )
        )
    return attention.assemble(
        [refusals, dead_letters, unrelayable, inbound, archived, collisions]
    )


def interval_of(settings):
    return getattr(settings, "interval_seconds", None)


def emit_unwritten_health(reason, write_error, **fields):
    record = {"ts": utc_now(), "action": reason, "health_unwritten": str(write_error)}
    record.update(fields)
    print(json.dumps(record, sort_keys=True, separators=(",", ":")), flush=True)


def write_busy_health(settings):
    # Busy probe: the whole read-modify-write happens under the health lock so
    # the streak and the carried-over fields come from one consistent read.
    with health_lock(settings.root, required=True):
        prior = read_health(settings)
        delay = os.environ.get("BRIDGE_TEST_HEALTH_DELAY_MS")
        if delay:
            time.sleep(float(delay) / 1000.0)
        prior_streak = prior.get("busy_streak", 0)
        busy_streak = prior_streak + 1 if isinstance(prior_streak, int) else 1
        unhealthy = busy_streak >= 5
        prior_ok = prior.get("ok", True)
        if not isinstance(prior_ok, bool):
            prior_ok = True
        prior_reason = prior.get("reason", "busy")
        if not isinstance(prior_reason, str):
            prior_reason = "busy"
        now = utc_now()
        first_tick_at = prior.get("first_tick_at")
        if epoch_from_iso(first_tick_at) is None:
            first_tick_at = now
        ok = False if unhealthy else prior_ok
        outbound_unrelayable = prior.get("outbound_unrelayable", [])
        if not isinstance(outbound_unrelayable, list):
            outbound_unrelayable = []
        local_held = localheld.bounded_health(prior.get("local_held"))
        reason = prior_reason
        if unhealthy:
            # Review 7b: a standing store fault stays the named reason; the
            # busy streak is reported by busy_streak itself.
            reason = (
                "local_held_store_fault"
                if localheld.store_faulted(local_held)
                else "busy_streak"
            )
        value = {
            "ts": now,
            "ok": ok,
            "reason": reason,
            "first_tick_at": first_tick_at,
            "last_fetch_ok": prior.get("last_fetch_ok"),
            "last_push_ok": prior.get("last_push_ok"),
            "stalled_since": None if ok else prior.get("stalled_since") or now,
            "busy_streak": busy_streak,
            "held": prior.get("held", 0),
            "quarantined": prior.get("quarantined", 0),
            "outbound_unrelayable": [
                mail_id for mail_id in outbound_unrelayable if isinstance(mail_id, str)
            ][:20],
            "rooms": prior.get(
                "rooms",
                {"published": 0, "collisions": [], "retired": [], "route_contested": 0},
            ),
            "channels": prior.get(
                "channels",
                {
                    "imported": 0,
                    "published": 0,
                    "quarantined": 0,
                    "unpublishable": 0,
                    "diverged": [],
                    "rewritten": [],
                },
            ),
            "git_failed": bounded_git_failures(prior.get("git_failed", [])),
            "sender_not_homed": bounded_hold_summary(prior.get("sender_not_homed", {})),
            "pmail": pmail.bounded_health(prior.get("pmail")),
            "local_held": local_held,
            "attention": attention.bounded(prior.get("attention")),
            "quiet": prior.get("quiet", False),
            "quiet_streak": prior.get("quiet_streak", 0),
        }
        value.update(pmail.carried_capability_fields(prior))
        path = destination(settings.root, "bridge", "health.json")
        atomic_replace(
            path,
            (json.dumps(value, sort_keys=True) + "\n").encode("utf-8"),
            settings.root,
        )
        return value


def write_quiet_health(settings):
    """Advance only the quiet tick fields while preserving prior health."""
    with health_lock(settings.root, required=True):
        value = read_health(settings)
        if value.get("ok") is not True:
            raise TickError("quiet_prior_unhealthy")
        streak = value.get("quiet_streak", 0)
        value["ts"] = utc_now()
        value["pmail"] = pmail.bounded_health(value.get("pmail"))
        value["local_held"] = localheld.bounded_health(value.get("local_held"))
        value["attention"] = attention.bounded(value.get("attention"))
        # A quiet tick is a tick: it re-states the capabilities, so a health
        # file last written by an older bridge never vouches for this one.
        # An unknown interval is omitted, so the prior tick's value must not
        # survive beside the fresh ticked_at (Grok, fix round 1).
        value.pop("interval_s", None)
        value.update(pmail.capability_fields(interval_of(settings), value["ts"]))
        value["quiet"] = True
        value["quiet_streak"] = streak + 1 if isinstance(streak, int) else 1
        atomic_replace(
            destination(settings.root, "bridge", "health.json"),
            (json.dumps(value, sort_keys=True) + "\n").encode("utf-8"),
            settings.root,
        )
        return value


def write_fatal_health(root, reason):
    # Exit-2 writer: no mkdir, no chmod, no log file (ruling R5a). It locks
    # only when bridge/.health.lock already exists, so the lock file itself
    # is never created on this path.
    with health_lock(root, required=False):
        path = destination(root, "bridge", "health.json")
        try:
            prior = load_json_bytes(open_regular(path, 64 * 1024), str(path))
        except (FileNotFoundError, ConfigError):
            prior = {}
        if not isinstance(prior, dict):
            prior = {}
        now = utc_now()
        first_tick_at = prior.get("first_tick_at")
        if epoch_from_iso(first_tick_at) is None:
            first_tick_at = now
        last_fetch_ok = prior.get("last_fetch_ok")
        if epoch_from_iso(last_fetch_ok) is None:
            last_fetch_ok = None
        last_push_ok = prior.get("last_push_ok")
        if epoch_from_iso(last_push_ok) is None:
            last_push_ok = None
        value = {
            "ts": now,
            "ok": False,
            "reason": reason,
            "first_tick_at": first_tick_at,
            "last_fetch_ok": last_fetch_ok,
            "last_push_ok": last_push_ok,
            "stalled_since": prior.get("stalled_since") or now,
            "busy_streak": 0,
            "held": 0,
            "quarantined": 0,
            "outbound_unrelayable": [],
            "rooms": prior.get(
                "rooms",
                {"published": 0, "collisions": [], "retired": [], "route_contested": 0},
            ),
            "channels": prior.get(
                "channels",
                {
                    "imported": 0,
                    "published": 0,
                    "quarantined": 0,
                    "unpublishable": 0,
                    "diverged": [],
                    "rewritten": [],
                },
            ),
            "git_failed": bounded_git_failures(prior.get("git_failed", [])),
            "sender_not_homed": bounded_hold_summary(prior.get("sender_not_homed", {})),
            "pmail": pmail.bounded_health(prior.get("pmail")),
            "local_held": localheld.bounded_health(prior.get("local_held")),
            "attention": attention.bounded(prior.get("attention")),
            "quiet": False,
            "quiet_streak": 0,
        }
        value.update(
            pmail.capability_fields(
                pmail.interval_seconds(os.environ.get("BRIDGE_INTERVAL_SECONDS")), now
            )
        )
        atomic_replace(
            path, (json.dumps(value, sort_keys=True) + "\n").encode(), root
        )


def relay_size(repo):
    total = 0
    git_dir = repo / ".git"
    if not git_dir.is_dir():
        return 0
    for path in git_dir.rglob("*"):
        try:
            if path.is_file() and not path.is_symlink():
                total += path.stat().st_size
        except OSError:
            pass
    return total


def acquire_tick_lock(settings):
    bridge = destination(settings.root, "bridge")
    ensure_dir(settings.root, bridge)
    path = destination(settings.root, "bridge", ".lock")
    anchor = destination(settings.root, "bridge", ".lock.anchor")
    flags = os.O_RDWR | os.O_CREAT | getattr(os, "O_NOFOLLOW", 0)
    initial = os.open(str(path), flags, 0o600)
    os.close(initial)
    try:
        os.link(str(path), str(anchor), follow_symlinks=False)
        fsync_directory(bridge)
    except FileExistsError:
        pass
    descriptor = os.open(
        str(anchor), os.O_RDWR | getattr(os, "O_NOFOLLOW", 0)
    )
    canonical = os.fstat(descriptor)
    if not stat.S_ISREG(canonical.st_mode):
        os.close(descriptor)
        raise ConfigError("bridge lock anchor must be a regular file")
    try:
        before = os.stat(str(path), follow_symlinks=False)
    except FileNotFoundError:
        before = None
    try:
        fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        os.close(descriptor)
        return None
    try:
        held = os.fstat(descriptor)
        current = os.stat(str(path), follow_symlinks=False)
    except FileNotFoundError:
        fcntl.flock(descriptor, fcntl.LOCK_UN)
        os.close(descriptor)
        return None
    held_identity = (held.st_dev, held.st_ino)
    current_identity = (current.st_dev, current.st_ino)
    before_identity = (
        None if before is None else (before.st_dev, before.st_ino)
    )
    if before_identity == held_identity and current_identity != held_identity:
        fcntl.flock(descriptor, fcntl.LOCK_UN)
        os.close(descriptor)
        return None
    if current_identity != held_identity:
        if not stat.S_ISREG(current.st_mode):
            fcntl.flock(descriptor, fcntl.LOCK_UN)
            os.close(descriptor)
            raise ConfigError("bridge lock path must be a regular file")
        path.unlink()
        os.link(str(anchor), str(path), follow_symlinks=False)
        fsync_directory(bridge)
    return descriptor


def ignored_branches(config, git, logger, peers=None):
    result = git.run(
        [
            "for-each-ref",
            "--format=%(refname:strip=4)",
            "refs/remotes/origin/machines/*",
        ]
    )
    for host in sorted(filter(None, result.stdout.splitlines())):
        effective = set(config.peers if peers is None else peers)
        if host not in effective and host != config.host:
            logger.emit("ignored_branch", host=host, reason="unlisted_host")


def execute(check_config=False):
    settings = load_settings()
    deadline = Deadline(settings.deadline_seconds)
    config = load_config(settings)
    git = Git(settings, deadline)
    validate_repo(settings, config, git)
    if check_config:
        validate_post_version(settings)
        print(json.dumps({"ok": True, "host": settings.host}, sort_keys=True))
        return 0
    logger = Logger(settings.root)
    lock_descriptor = acquire_tick_lock(settings)
    try:
        if lock_descriptor is None:
            # Busy probe: inside the try so a missing .health.lock surfaces
            # as internal_error instead of escaping to main() (finding 1).
            health = write_busy_health(settings)
            logger.emit("busy", busy_streak=health["busy_streak"], ok=health["ok"])
            return 0 if health["ok"] else 1
        prior_health = read_health(settings)
        # The start-of-tick probe is what a completed tick consumes, and what
        # step 17 persists: anything landing after it forces the next tick
        # full instead of being recorded as already seen.
        start_fingerprint, start_heads, quiet = tick.quiet_candidate(
            settings, git, prior_health
        )
        if quiet:
            # No log line: health.json's ts and quiet_streak are the
            # heartbeat, and a line per quiet tick was ~4,000 a day per host.
            write_quiet_health(settings)
            return 0
        if fence_present(settings):
            logger.emit("fenced", root=str(settings.root))
            write_health(settings, False, "fenced")
            return 1
        # Ruling R1: a malformed rules.json is TickError("rules_invalid"),
        # exit 1 — never config-fatal. Validate before any placeholder or
        # mailbox write; post's own CLI refuses to run against an invalid
        # file, so this also keeps that failure off the internal_error path.
        load_rules(settings)
        logger.track_conditions()
        recover(settings, git, logger)
        fetch_ok = fetch_remote(settings, git, logger)
        if fetch_ok:
            logger.emit("fetch", ok=True)
        else:
            logger.emit("fetch", ok=False, reason="git_fetch_failed")
        checkpoint("after-fetch")
        oids = pin_remote_oids(git)
        post_rooms = run_post_rooms(settings)
        # SPEC-v2 §Unpublished senders keys on the peer's own publication
        # history, not on the registry branch: snapshot.v2_peers is that set
        # and binding_verdict consumes it directly. §Registry's "none ever =>
        # config peers alone" governs peer selection only.
        snapshot = rooms.build_snapshot(
            settings,
            config,
            git,
            oids,
            post_rooms,
            logger,
        )
        ignored_branches(config, git, logger, snapshot.peers)
        # Step 8's archive check lives inside import_channels, which owns it
        # and reports through ChannelStats.completed and .rewritten. Running
        # it here too doubled a `git diff` per peer per tick and double-logged
        # relay_history_rewritten.
        if fence_present(settings):
            raise TickError("fenced")
        registered_rooms = rooms.ensure_placeholders(
            settings,
            config,
            snapshot,
            logger,
            deadline,
            lambda: fence_present(settings),
        )
        # r5.5 (M2): binding decisions read the table post itself will use.
        snapshot = dataclass_replace(snapshot, post_rooms=registered_rooms)
        checkpoint("after-placeholders")
        checkpoint("before-inbound")
        if fence_present(settings):
            raise TickError("fenced")
        mail_holds = HoldLedger(settings.root, "held-not-homed")
        held, quarantined = process_inbound(
            settings,
            config,
            git,
            snapshot.real_rooms,
            snapshot.placeholders,
            logger,
            snapshot=snapshot,
            holds=mail_holds,
        )
        checkpoint("before-channel-import")
        if fence_present(settings):
            raise TickError("fenced")
        imported = channels.import_channels(
            settings,
            config.channels,
            git,
            snapshot,
            logger,
            deadline,
            lambda: fence_present(settings),
        )
        for host in snapshot.peers:
            deadline.check()
            oid = snapshot.oids.get(f"machines/{host}")
            if (
                config.channels is not None
                and oid is not None
                and host in imported.completed
            ):
                channels.advance_tip(settings, host, oid)
        # F3: participant mail addressed to this host, per effective peer.
        checkpoint("before-pmail-import")
        if fence_present(settings):
            raise TickError("fenced")
        pmail_imported = pmail.import_pmail(
            settings, git, snapshot, logger, deadline, lambda: fence_present(settings)
        )
        fetched_ref = snapshot.oids.get(f"machines/{settings.host}")
        if fetched_ref is None:
            fetched_ref = git.rev("HEAD")
        derive_published_and_tidy(
            settings, config, git, registered_rooms, logger, fetched_ref, tidy=True
        )
        refusals = []
        prune_outbox(
            settings, config, git, logger, snapshot=snapshot, refusals=refusals
        )
        typed = []
        # r6.1: the local-held stamp runs inside selection, per letter,
        # before that letter can be routed.
        local_guard = local_held_guard(settings, snapshot.real_rooms, snapshot, logger)
        try:
            selected, outbound_unrelayable = select_outbound(
                settings,
                config,
                snapshot.real_rooms,
                snapshot.placeholders,
                logger,
                deadline,
                snapshot=snapshot,
                typed=typed,
                guard=local_guard,
            )
        finally:
            # Review 4, finding 3: selection is the only stamping, so the
            # sentinel is raised here, not only at write_health. A tick that
            # stamps and then fails (push_failed, branch_diverged, a fence,
            # the deadline) no longer leaves its new holds out of the floor.
            local_guard.settle_floor()
        copy_outbound(settings, selected, logger)
        pmail_sent = pmail.sender_pass(
            settings, git, snapshot, logger, typed, deadline,
            lambda: fence_present(settings),
        )
        rooms.write_rooms_json(settings, snapshot, logger)
        checkpoint("before-publish")
        if fence_present(settings):
            raise TickError("fenced")
        published = channels.publish_channels(
            settings,
            config.channels,
            snapshot,
            logger,
            deadline,
            tracked=committed_channel_messages(git),
        )
        imported.published += published.published
        imported.unpublishable += published.unpublishable
        for reason, count in published.reasons.items():
            imported.reasons[reason] = imported.reasons.get(reason, 0) + count
        has_queued_work = queued_work(settings, git)
        try:
            pushed = commit_and_push(
                settings, config, git, registered_rooms, logger, has_queued_work
            )
        except TickError as error:
            if error.reason == "push_failed":
                # A stage that was not pushed is not published (F3-6); the
                # sender's `post delivery` shows why it is still queued.
                pmail.note_unpushed(settings, logger, pmail.RELAY_PUSH_FAILED)
            raise
        pmail_health = pmail.health(settings, snapshot, pmail_imported, pmail_sent)
        pmail.log_awaiting(settings, logger, pmail_health["awaiting_receipt"])
        size = relay_size(settings.repo)
        if size > RELAY_WARN_BYTES:
            logger.emit("relay_large", bytes=size, threshold=RELAY_WARN_BYTES)
        # Exit code follows the written health: write_health decides fetch
        # staleness inside the lock and returns the final dict.
        health = write_health(
            settings,
            True,
            "ok",
            held=held,
            quarantined=quarantined,
            outbound_unrelayable=outbound_unrelayable,
            fetch_ok=fetch_ok,
            push_ok=pushed or git.rev("HEAD") == git.rev(fetched_ref),
            room_stats=rooms.rooms_health(snapshot),
            channel_stats=channels.channels_health(imported, settings),
            git_failed=git.read_failures,
            sender_not_homed=mail_holds.summary(
                snapshot.peers, prune=snapshot.peers_known
            ),
            pmail_stats=pmail_health,
            local_held=local_guard.health(),
            attention_items=build_attention(
                settings, logger, snapshot, refusals, prior_health, git.read_failures
            ),
            quiet=False,
        )
        # A tick that skipped a peer's read did not see every condition.
        # The per-letter lines are deduped; the count of standing conditions
        # per action keeps them visible on every full tick.
        standing = logger.settle_conditions(prune=not git.read_failures)
        logger.emit(
            "health", ok=health["ok"], reason=health["reason"], standing=standing
        )
        # m5/m6: the quiet fingerprint may only record a tick that actually
        # read every peer. A failed fetch leaves the pinned refs stale and a
        # failed ls-tree skipped a host, so neither checkpoints.
        if fetch_ok and not git.read_failures:
            tick.persist_completed(settings, start_fingerprint, start_heads)
        return 0 if health["ok"] else 1
    except TickError as error:
        logger.emit(error.reason, error=str(error))
        try:
            # This tick's git read failures, not the prior tick's list.
            write_health(
                settings, False, error.reason, git_failed=git.read_failures
            )
        except OSError as write_error:
            # Missing .health.lock on a live path is a misinstall; the record
            # goes to stdout rather than letting the handler's own failure
            # escape as a traceback (C1). The terminal record is the same
            # {action, health_unwritten, class} shape as the catch-all's:
            # the operational reason was already emitted above.
            emit_unwritten_health(
                "internal_error",
                write_error,
                **{"class": type(write_error).__name__},
            )
        return 1
    except ConfigError:
        # Config-fatal (topology collision, unsafe destination, …) exits 2
        # through main(); the R5b catch-all must not swallow it.
        raise
    except Exception as error:  # noqa: BLE001 - R5b mandates the catch-all
        # Ruling R5b: a traceback must never leave health.json stale. The
        # lock descriptor still closes via the finally below.
        logger.emit(
            "internal_error",
            **{"class": type(error).__name__},
            error=str(error),
        )
        try:
            write_health(
                settings, False, "internal_error", git_failed=git.read_failures
            )
        except OSError as write_error:
            emit_unwritten_health(
                "internal_error", write_error, **{"class": type(error).__name__}
            )
        return 1
    finally:
        # An aborted tick keeps what it saw without clearing anything.
        logger.settle_conditions(prune=False)
        if lock_descriptor is not None:
            os.close(lock_descriptor)


def last_contested(settings):
    """Fold-keyed names the last full tick found contested, or None.

    The seed does not run a tick, so "currently contested" is the room
    collisions the last full tick wrote to health (quiet ticks carry them).
    """
    rooms_value = read_health(settings).get("rooms")
    collisions = rooms_value.get("collisions") if isinstance(rooms_value, dict) else None
    if not isinstance(collisions, list):
        return None
    names = set()
    for item in collisions:
        if not isinstance(item, dict) or not isinstance(item.get("room"), str):
            return None
        names.add(fold_name(item["room"]))
    return frozenset(names)


def _seed_row(settings, real_rooms, row, manifest_copy, defer_index=False):
    """One manifest row -> (mail_id, outcome, reason); outcome in
    stamped / already_held / mismatch."""
    if row is None:
        return None, "mismatch", "row_malformed"
    mail_id = row.get("id")
    try:
        validate_id(mail_id)
    except ConfigError:
        return None, "mismatch", "id_invalid"
    if row.get("host") != settings.host:
        return mail_id, "mismatch", "host_mismatch"
    expected_sha = row.get("archive_sha256")
    room = row.get("room")
    copies = row.get("local_copies")
    if not (
        isinstance(expected_sha, str)
        and isinstance(room, str)
        and isinstance(copies, list)
        and all(isinstance(item, str) for item in copies)
    ):
        return mail_id, "mismatch", "row_malformed"
    try:
        data = open_regular(
            destination(settings.root, "archive", mail_id + ".mail"),
            settings.max_mail_bytes,
        )
    except (FileNotFoundError, ConfigError, OSError):
        return mail_id, "mismatch", "archive_unreadable"
    archive_sha256 = localheld.sha256_hex(data)
    if archive_sha256 != expected_sha:
        return mail_id, "mismatch", "digest_mismatch"
    try:
        header = outbound_header(data)
        if not workspace_addressed(header):
            raise ConfigError("not workspace mail")
        envelope = parse_envelope(data, mail_id, header.get("to"))
    except ConfigError:
        return mail_id, "mismatch", "envelope_invalid"
    to = envelope["to"]
    if to != room:
        return mail_id, "mismatch", "target_mismatch"
    if (
        marker_exists(settings, "received", mail_id)
        or delivered_id_exists(settings, mail_id)
        or marker_exists(settings, "published", mail_id)
    ):
        return mail_id, "mismatch", "marker_present"
    # A valid hold already binds these bytes and this target, so a re-seed
    # after a rename (the store-fault recovery) counts it instead of
    # failing on the registration that no longer exists.
    present, record = localheld.read_record(settings.root, mail_id)
    if present and localheld.record_matches(record, archive_sha256, to) is None:
        return mail_id, "already_held", None
    registered = localheld.real_room(real_rooms, to)
    if registered is None:
        # A placeholder's canonical inbox copy is not local delivery.
        return mail_id, "mismatch", "room_not_local"
    actual = localheld.mailbox_copies(settings.root, registered, mail_id, data)
    matched = [item for item in copies if item in actual]
    if not matched:
        return mail_id, "mismatch", "local_copy_mismatch"
    for _ in range(2):
        present, record = localheld.read_record(settings.root, mail_id)
        if present:
            if localheld.record_matches(record, archive_sha256, to) is None:
                return mail_id, "already_held", None
            return mail_id, "mismatch", "record_conflict"
        try:
            localheld.stamp(
                settings.root,
                mail_id,
                archive_sha256,
                to,
                localheld.SEEDED,
                matched + [manifest_copy],
                defer_index=defer_index,
            )
        except FileExistsError:
            continue
        return mail_id, "stamped", None
    return mail_id, "mismatch", "record_conflict"


def _seed_unknown_intent(settings, contested, manifest_copy, logger, defer_index=False):
    """Hold historical letters to a contested name whose intent is unknown:
    no local copy anywhere and no published marker (SPEC r6.1 §Seeding)."""
    stamped = []
    archive_root = destination(settings.root, "archive")
    if not archive_root.is_dir():
        return stamped
    for path in sorted(archive_root.iterdir()):
        mail_id = path.stem
        if path.suffix != ".mail":
            continue
        try:
            validate_id(mail_id)
        except ConfigError:
            continue
        if (
            marker_exists(settings, "received", mail_id)
            or delivered_id_exists(settings, mail_id)
            or marker_exists(settings, "published", mail_id)
        ):
            continue
        try:
            data = open_regular(path, settings.max_mail_bytes)
            header = outbound_header(data)
            if not workspace_addressed(header):
                continue
            envelope = parse_envelope(data, mail_id, header.get("to"))
        except (ConfigError, OSError):
            continue
        to = envelope["to"]
        if fold_name(to) not in contested:
            continue
        present, _ = localheld.read_record(settings.root, mail_id)
        if present:
            continue
        if localheld.mailbox_copies(settings.root, to, mail_id, data):
            continue
        try:
            localheld.stamp(
                settings.root,
                mail_id,
                localheld.sha256_hex(data),
                to,
                localheld.UNKNOWN_INTENT,
                [manifest_copy],
                defer_index=defer_index,
            )
        except FileExistsError:
            continue
        logger.emit(
            "local_held", id=mail_id, room=to, reason=localheld.UNKNOWN_INTENT
        )
        stamped.append(mail_id)
    return stamped


def _reset_local_held_floor(settings, indexed, manifests):
    """``--accept-lost-records``: set the carried floors to the store as it
    is now: the rebuilt index's id count and the manifest copies present.
    Only health's ``local_held.indexed`` and ``.manifests`` change."""
    with health_lock(settings.root, required=True):
        value = read_health(settings)
        if not value:
            return
        local_held = localheld.bounded_health(value.get("local_held"))
        local_held["indexed"] = indexed
        local_held["manifests"] = manifests
        value["local_held"] = local_held
        atomic_replace(
            destination(settings.root, "bridge", "health.json"),
            (json.dumps(value, sort_keys=True) + "\n").encode("utf-8"),
            settings.root,
        )


def seed_local_holds(manifest_arg, accept_lost_records=False):
    """``--seed-local-holds <manifest.jsonl>``: one-shot, under the tick lock.

    Rechecks every row against the archive and the local mailbox, stamps a
    ``seeded`` hold for each row that passes, keeps an immutable copy of the
    manifest (whose ids stay expected: a later missing hold is a held
    fault), and holds contested-name letters of unknown intent. The manifest
    file itself is only read. Exit 0 only when every row passed and the
    index was rebuilt; refused rows alone do not block the rebuild.
    """
    settings = load_settings()
    load_config(settings)
    logger = Logger(settings.root, echo=True)
    try:
        data = open_regular(Path(manifest_arg), localheld.MANIFEST_MAX_BYTES)
    except (FileNotFoundError, ConfigError, OSError) as error:
        logger.emit("local_held_seed", ok=False, error=f"manifest unreadable: {error}")
        return 1
    # r6.1.1: a manifest for another host (or an empty one) must not be
    # persisted: its copy would restore the store, clearing store_missing,
    # while expecting nothing here.
    rows = localheld.manifest_rows(data)
    if not any(value is not None and value.get("host") == settings.host for _, value in rows):
        logger.emit(
            "local_held_seed", ok=False, error=f"no manifest row names host {settings.host}"
        )
        return 1
    lock_descriptor = acquire_tick_lock(settings)
    if lock_descriptor is None:
        logger.emit("local_held_seed", ok=False, error="busy")
        return 1
    try:
        if fence_present(settings):
            logger.emit("local_held_seed", ok=False, error="fenced")
            return 1
        # Round 2, finding 3: an index the rebuild could not read or append
        # to fails the seed before anything is written.
        problem = localheld.index_problem(settings.root)
        if problem is not None:
            logger.emit(
                "local_held_seed", ok=False, error=problem, rows=len(rows), index_rebuilt=None
            )
            return 1
        # Review 3, finding 1: the floor is measured against the store as
        # it stood before this seed wrote anything. Review 4, finding 1:
        # only valid records count, never a bare file named like an id.
        records_before = localheld.valid_record_ids(settings.root)
        if records_before is None:
            # Review 3, finding 5: an unlistable record directory leaves the
            # store unknown; the seed writes nothing.
            logger.emit(
                "local_held_seed", ok=False, error="records_unreadable", rows=len(rows),
                index_rebuilt=None,
            )
            return 1
        index_before, _ = localheld.read_index(settings.root)
        prior = read_health(settings).get("local_held")
        sentinel = localheld.read_sentinel(settings.root)
        floor, manifests_floor = localheld.carried_floors(settings.root, prior, sentinel)
        # Round 2, finding 2: while the index is absent and a hold is known to
        # have existed, the seed writes no index unless it rebuilds, so a
        # seed that does not rebuild leaves the fault standing.
        defer = (
            localheld.missing_index_fault(settings.root, prior, (floor, manifests_floor))
            is not None
        )
        real_rooms, _ = rooms._split_rooms(settings, run_post_rooms(settings))
        manifest_copy = localheld.persist_manifest(settings.root, data)
        counts = {"stamped": 0, "already_held": 0, "mismatch": 0}
        row_stamped = set()
        for number, row in rows:
            mail_id, outcome, reason = _seed_row(
                settings, real_rooms, row, manifest_copy, defer_index=defer
            )
            counts[outcome] += 1
            if outcome == "mismatch":
                record = {"line": number, "id": mail_id, "reason": reason}
                logger.emit("local_held_seed_mismatch", **record)
            elif outcome == "stamped":
                row_stamped.add(mail_id)
                logger.emit("local_held", id=mail_id, reason=localheld.SEEDED)
        contested = last_contested(settings)
        # Surviving: records present before the seed, ids already indexed,
        # and the ids its manifest rows stamped (the operator's deliberate
        # input). Unknown-intent stamps never count.
        surviving = len(records_before | index_before | row_stamped)
        lost = floor is not None and surviving < floor
        # Review 3, finding 2: refused rows fail the seed but do not block
        # the rebuild; only an unmet or unknown floor does, and
        # --accept-lost-records overrides both.
        rebuild = contested is not None and (
            accept_lost_records or (floor is not None and not lost)
        )
        unknown = None
        index_rebuilt = None
        rebuild_error = None
        sentinel_error = None
        if rebuild:
            # Only a seed that rebuilds holds unknown-intent letters, so a
            # failed seed leaves no record that a later repair could count.
            unknown = _seed_unknown_intent(
                settings, contested, manifest_copy, logger, defer_index=defer
            )
            try:
                index_rebuilt = localheld.rebuild_index(settings.root)
            except (ConfigError, OSError) as error:
                # Damaged after the check above (a race with an operator).
                rebuild_error = str(error)
        if index_rebuilt is not None:
            rebuilt, _ = localheld.read_index(settings.root)
            manifests = localheld.manifest_count(settings.root, settings.host) or 0
            try:
                if accept_lost_records:
                    localheld.write_sentinel(settings.root, len(rebuilt), manifests)
                    logger.emit(
                        "local_held_lost_records_accepted",
                        floor=floor,
                        surviving=surviving,
                        indexed=len(rebuilt),
                        manifests_carried=manifests_floor,
                        manifests=manifests,
                        sentinel=sentinel[2] or sentinel[0],
                    )
                    _reset_local_held_floor(settings, len(rebuilt), manifests)
                else:
                    written = localheld.settle_sentinel(
                        settings.root,
                        sentinel,
                        max(floor, len(rebuilt)),
                        max(manifests_floor, manifests),
                        True,
                    )
                    if written is not None and sentinel[0] == "absent":
                        logger.emit("local_held_sentinel_created", **written)
            except (ConfigError, OSError) as error:
                sentinel_error = str(error)
        summary = {
            "ok": index_rebuilt is not None
            and counts["mismatch"] == 0
            and sentinel_error is None,
            "manifest_sha256": localheld.sha256_hex(data),
            "manifest_copy": manifest_copy,
            "rows": len(rows),
            "stamped": counts["stamped"],
            "already_held": counts["already_held"],
            "mismatched": counts["mismatch"],
            "unknown_intent": None if unknown is None else len(unknown),
            "contested": None if contested is None else sorted(contested),
            "floor": floor,
            "surviving": surviving,
            "index_rebuilt": index_rebuilt,
        }
        if contested is None:
            summary["error"] = "contested names unknown: no readable health rooms"
        elif sentinel[0] == "damaged" and not accept_lost_records:
            summary["error"] = localheld.SENTINEL_DAMAGED
        elif floor is None and not accept_lost_records:
            summary["error"] = localheld.FLOOR_UNKNOWN
        elif lost and not accept_lost_records:
            summary["error"] = "lost_records"
        elif rebuild and index_rebuilt is None:
            summary["error"] = "local-held index not rebuilt: " + (
                rebuild_error or "unreadable"
            )
        elif sentinel_error is not None:
            summary["error"] = "sentinel_unwritable: " + sentinel_error
        ok = summary["ok"]
        logger.emit("local_held_seed", **summary)
        return 0 if ok else 1
    finally:
        os.close(lock_descriptor)


# Review 4, finding 6: the lock wait covers a whole tick (the service's
# 240 s deadline, under TimeoutStartSec=5min), so install never fails busy
# behind a tick that is still inside its deadline.
INIT_SENTINEL_LOCK_ATTEMPTS = 300


def init_held_sentinel(attempts=INIT_SENTINEL_LOCK_ATTEMPTS, interval=1.0):
    """``--init-held-sentinel``: run by install.sh before the timer is
    enabled, so a store stamped by an older bridge gets its sentinel before
    any tick could see it wiped (review 3, finding 7: the deploy window).

    Under the tick lock (retried while a tick holds it), an absent sentinel
    is created by the same rule a full tick uses: the index's id count over
    a readable index with no carried store fault; null when holds are known
    to have existed but the index cannot be counted; nothing on a fresh
    install. A present sentinel, damaged or not, is left to the tick.
    """
    settings = load_settings()
    load_config(settings)
    logger = Logger(settings.root, echo=True)
    lock_descriptor = None
    for attempt in range(attempts):
        lock_descriptor = acquire_tick_lock(settings)
        if lock_descriptor is not None:
            break
        if attempt + 1 < attempts:
            time.sleep(interval)
    if lock_descriptor is None:
        logger.emit("local_held_sentinel_init", ok=False, error="busy")
        return 1
    try:
        root = settings.root
        sentinel = localheld.read_sentinel(root)
        if sentinel[0] != "absent":
            logger.emit("local_held_sentinel_init", ok=True, sentinel=sentinel[2] or sentinel[0])
            return 0
        prior = read_health(settings).get("local_held")
        floor, manifests_floor = localheld.carried_floors(root, prior, sentinel)
        ids, damaged = localheld.read_index(root)
        try:
            index_exists = os.path.lexists(str(destination(root, "bridge", localheld.INDEX)))
        except ConfigError:
            index_exists = True
        index_present = index_exists and not damaged
        manifests = max(manifests_floor, localheld.manifest_count(root, settings.host) or 0)
        indexed = None if floor is None else max(floor, len(ids))
        try:
            written = localheld.settle_sentinel(
                root, sentinel, indexed, manifests, index_present
            )
        except (ConfigError, OSError) as error:
            logger.emit("local_held_sentinel_init", ok=False, error=str(error))
            return 1
        if written is not None:
            logger.emit("local_held_sentinel_created", by="install", **written)
        logger.emit("local_held_sentinel_init", ok=True, sentinel=written)
        return 0
    finally:
        os.close(lock_descriptor)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check-config", action="store_true")
    parser.add_argument("--init-held-sentinel", action="store_true")
    parser.add_argument("--seed-local-holds", metavar="MANIFEST")
    parser.add_argument("--accept-lost-records", action="store_true")
    args = parser.parse_args()
    if args.accept_lost_records and args.seed_local_holds is None:
        parser.error("--accept-lost-records needs --seed-local-holds")
    if args.init_held_sentinel:
        try:
            return init_held_sentinel()
        except ConfigError as error:
            record = {"ts": utc_now(), "action": "config_error", "error": str(error)}
            print(json.dumps(record, sort_keys=True, separators=(",", ":")), flush=True)
            return 2
    if args.seed_local_holds is not None:
        try:
            return seed_local_holds(
                args.seed_local_holds, accept_lost_records=args.accept_lost_records
            )
        except ConfigError as error:
            record = {"ts": utc_now(), "action": "config_error", "error": str(error)}
            print(json.dumps(record, sort_keys=True, separators=(",", ":")), flush=True)
            return 2
    try:
        return execute(check_config=args.check_config)
    except ConfigError as error:
        record = {"ts": utc_now(), "action": "config_error", "error": str(error)}
        if args.check_config:
            print(json.dumps(record, sort_keys=True, separators=(",", ":")), flush=True)
            return 2
        # Ruling R5a: on exit 2 the only permitted write is
        # bridge/health.json, and only when POST_MAIL_ROOT itself validated
        # and bridge/ already exists — no Logger, no log.jsonl line, no
        # mkdir, no chmod. The JSON record always goes to stdout.
        emitted = False
        root_raw = os.environ.get("POST_MAIL_ROOT")
        required_present = all(
            os.environ.get(name)
            for name in (
                "POST_MAIL_ROOT",
                "BRIDGE_REPO",
                "BRIDGE_HOST",
                "BRIDGE_SSH_KEY",
            )
        )
        if root_raw and required_present:
            root = Path(root_raw)
            if root.is_absolute() and root.is_dir() and realpath_equal(root):
                bridge = root / "bridge"
                if bridge.is_dir() and not bridge.is_symlink():
                    try:
                        write_fatal_health(root, "config_error")
                        emitted = True
                    except (ConfigError, OSError):
                        pass
        print(json.dumps(record, sort_keys=True, separators=(",", ":")), flush=True)
        if not emitted:
            print(f"post-bridge: {error}", file=sys.stderr)
        return 2
    except subprocess.TimeoutExpired as error:
        print(
            json.dumps(
                {"ts": utc_now(), "action": "deadline", "error": str(error)},
                sort_keys=True,
                separators=(",", ":"),
            ),
            flush=True,
        )
        return 1


if __name__ == "__main__":
    sys.exit(main())
