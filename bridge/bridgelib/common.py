"""Shared primitives for sweep.py and every bridgelib module: one
implementation, one ConfigError class (test_tick_v2 asserts by AST that
sweep.py defines none of these names).
"""

import argparse
import datetime as dt
import errno
import fcntl
import hashlib
import json
import os
import re
import shutil
import signal
import stat
import subprocess
import sys
import time
import unicodedata
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path, PurePosixPath

if sys.version_info < (3, 9):  # noqa: UP036 - the runtime guard is contractual
    raise RuntimeError(
        f"post-bridge requires Python 3.9 or newer; found {sys.version_info.major}.{sys.version_info.minor}.{sys.version_info.micro}"
    )


HOST_RE = re.compile(r"^[a-z0-9-]{1,32}$")
ID_RE = re.compile(r"^\d{8}-\d{6}-[0-9a-f]{6}$")
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
RECEIPT_STATUSES = {"delivered", "held", "quarantined"}
MAIL_KINDS = {"letter", "note", "signal"}
# post 0.9.0 src/mailbox.rs RESERVED_ROOM_NAMES (ROOMS_LOCK_FILE and the two
# migration_fence files resolve to the dotted names below).
# tests/test_rooms.py probes the real `post rooms add` with every entry.
POST_RESERVED = {
    "*",
    "archive",
    "participants",
    "lineages",
    "routing",
    ".participants.lock",
    "rooms.json",
    "rules.json",
    "profiles.json",
    "owner.json",
    ".rooms.lock",
    ".post-arx.json",
    ".post-arx.lock",
}
TOPOLOGY_DENY = POST_RESERVED | {
    "remote",
    "bridge",
    ".bridge",
    ".bridge.lock",
    "bridge.log",
    "channels",
}
CONFIG_KEYS = {"host", "relay_url", "peers"}
RECEIPT_KEYS = {"v", "status", "host", "room", "id", "sha256", "reason", "at"}
ENVELOPE_REQUIRED = {"id", "from", "to", "kind", "subject", "sent"}
ATTRIBUTION_KEYS = ("from_participant", "from_lineage")
MAIL_ADDRESS_KINDS = {"workspace", "lineage", "participant"}
ENVELOPE_OPTIONAL = {
    "display_name",
    "pfp",
    "sender_address",
    "sender_provenance",
    "address_kind",
    # F3: host-qualified participant mail (never workspace mail).
    "to_host",
    *ATTRIBUTION_KEYS,
}
LOG_ROTATE_BYTES = 10 * 1024 * 1024
RELAY_WARN_BYTES = 256 * 1024 * 1024
DEFAULT_MAX_MAIL_BYTES = 1024 * 1024
MAX_MAX_MAIL_BYTES = 1024 * 1024
DEFAULT_DEADLINE_SECONDS = 240
DEFAULT_FETCH_GRACE_SECONDS = 600
GIT_TIMEOUT_SECONDS = 30
GIT_NETWORK_TIMEOUT_SECONDS = 120


# The bridge is an external writer (SPEC.md §Non-goals, SPEC-v2 §Goal 6),
# never a post participant: it runs post unbound. post 0.9.0 resolves an
# acting participant from POST_PARTICIPANT or a harness conversation key, and
# every writer command (`rooms add` included) refreshes the resolved
# participant's lease, so a sweep started by hand from an agent session would
# otherwise act as that session. POST_FROM, POST_SENDER_ADDRESS and
# POST_ARX_GENERATION were already scrubbed for the same reason.
POST_IDENTITY_ENV = (
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


def post_environment(root):
    """The environment for every post subprocess the bridge runs."""
    environment = os.environ.copy()
    for name in POST_IDENTITY_ENV:
        environment.pop(name, None)
    environment["POST_MAIL_ROOT"] = str(root)
    return environment


class ConfigError(Exception):
    pass


class TickError(Exception):
    def __init__(self, reason, message=None):
        super().__init__(message or reason)
        self.reason = reason


class GitReadError(TickError):
    """m6: `git ls-tree` failed (bad ref, missing object). Never an empty tree."""

    def __init__(self, ref, path, detail):
        super().__init__("git_failed", f"git ls-tree {ref} -- {path} failed: {detail}")
        self.ref = ref
        self.path = path


def note_git_failure(git, logger, host, error):
    """Log a per-host read failure and record it for health and the checkpoint."""
    logger.emit("git_failed", host=host, ref=error.ref, path=error.path, error=str(error))
    failures = getattr(git, "read_failures", None)
    if failures is not None:
        failures.append({"host": host, "ref": error.ref, "path": error.path})


class DeadlineExpired(TickError):
    def __init__(self):
        super().__init__("deadline", "tick deadline expired")


def reject_duplicate_keys(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON key {key!r}")
        result[key] = value
    return result


# Every JSON document the bridge reads nests at most a few levels. A peer can
# publish deeper nesting in a few KiB, and json.loads then raises
# RecursionError at an interpreter-dependent depth (about 2000 on Python 3.9,
# far deeper on 3.13+), which escaped every handler and killed each tick.
MAX_JSON_DEPTH = 64


def json_depth_exceeds(text, limit=MAX_JSON_DEPTH):
    """True when `text` nests arrays/objects deeper than `limit` (strings skipped)."""
    if text.count("[") + text.count("{") <= limit:
        return False
    depth = 0
    in_string = False
    escaped = False
    for character in text:
        if in_string:
            if escaped:
                escaped = False
            elif character == "\\":
                escaped = True
            elif character == '"':
                in_string = False
        elif character == '"':
            in_string = True
        elif character in "[{":
            depth += 1
            if depth > limit:
                return True
        elif character in "]}":
            depth -= 1
    return False


def load_json_bytes(data, label):
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError as error:
        raise ConfigError(f"{label} is invalid JSON: {error}")
    if json_depth_exceeds(text):
        raise ConfigError(f"{label} nests deeper than {MAX_JSON_DEPTH} levels")
    try:
        return json.loads(text, object_pairs_hook=reject_duplicate_keys)
    except (ValueError, json.JSONDecodeError) as error:
        raise ConfigError(f"{label} is invalid JSON: {error}")
    except RecursionError:
        raise ConfigError(f"{label} nests too deeply to parse")


def utc_now():
    return dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds")


def epoch_from_iso(value):
    if not isinstance(value, str):
        return None
    try:
        return dt.datetime.fromisoformat(value).timestamp()
    except ValueError:
        return None


def realpath_equal(path):
    return os.path.realpath(str(path)) == str(path)


def under(root, path):
    try:
        Path(path).resolve(strict=False).relative_to(Path(root).resolve(strict=False))
        return True
    except ValueError:
        return False


def destination(root, *parts):
    result = Path(root).joinpath(*parts)
    if not under(root, result):
        raise ConfigError(f"destination escapes root: {result}")
    return result


def ensure_dir(root, path):
    if not under(root, path):
        raise ConfigError(f"directory escapes root: {path}")
    # Created with the process umask; an existing directory is never chmodded
    # (ruling R6: post's dirs, the archive, existing placeholders and bridge
    # state keep whatever mode they already had).
    path.mkdir(parents=True, exist_ok=True)
    if not path.is_dir() or path.is_symlink():
        raise ConfigError(f"expected a real directory: {path}")


def validate_host(value, label="host"):
    if not isinstance(value, str) or HOST_RE.fullmatch(value) is None:
        raise ConfigError(f"{label} must match ^[a-z0-9-]{{1,32}}$")


def normalize_name(value):
    """Return a name in the canonical NFC representation."""
    return unicodedata.normalize("NFC", value)


def fold_name(value):
    """Return the NFC-normalized cross-claimant comparison key."""
    normalized = normalize_name(value)
    return normalized.lower() if normalized.isascii() else normalized


def refused_profile_char(character):
    code = ord(character)
    return (
        code < 32
        or 127 <= code <= 159
        or 0x202A <= code <= 0x202E
        or 0x2066 <= code <= 0x2069
        or code in (0x200E, 0x200F, 0x061C, 0x2028, 0x2029)
    )


def default_ignorable(character):
    """Return whether Unicode marks the code point as default-ignorable."""
    code = ord(character)
    return (
        code == 0x00AD
        or code == 0x034F
        or code == 0x061C
        or 0x115F <= code <= 0x1160
        or 0x17B4 <= code <= 0x17B5
        or 0x180B <= code <= 0x180F
        or 0x200B <= code <= 0x200F
        or 0x202A <= code <= 0x202E
        or 0x2060 <= code <= 0x206F
        or code == 0x3164
        or 0xFE00 <= code <= 0xFE0F
        or code == 0xFEFF
        or code == 0xFFA0
        or 0xFFF0 <= code <= 0xFFF8
        or 0x1BCA0 <= code <= 0x1BCA3
        or 0x1D173 <= code <= 0x1D17A
        or 0xE0000 <= code <= 0xE0FFF
    )


def validate_path_component(value, label="room"):
    if not isinstance(value, str) or not value:
        raise ConfigError(f"{label} is empty or not a string")
    normalized = normalize_name(value)
    if normalized in (".", "..") or "/" in normalized or "\\" in normalized:
        raise ConfigError(f"{label} must be one path-safe component")
    if normalized[0].isspace() or normalized[-1].isspace():
        raise ConfigError(f"{label} has leading or trailing whitespace")
    if any(
        refused_profile_char(character)
        or unicodedata.category(character) == "Cf"
        or default_ignorable(character)
        for character in normalized
    ):
        raise ConfigError(f"{label} contains a refused control or direction character")


def validate_room(value, topology=False, label="room"):
    normalized = normalize_name(value) if isinstance(value, str) else value
    validate_path_component(normalized, label=label)
    folded = fold_name(normalized)
    compatibility = unicodedata.normalize("NFKC", normalized)
    reserved_fold = compatibility.lower() if compatibility.isascii() else compatibility
    if folded in POST_RESERVED or reserved_fold in POST_RESERVED:
        raise ConfigError(f"{label} {value!r} is reserved")
    if folded.startswith(".rooms.json.") and folded.endswith(".tmp"):
        raise ConfigError(f"{label} {value!r} is reserved")
    if folded.startswith("..post-arx.json.") and folded.endswith(".tmp"):
        raise ConfigError(f"{label} {value!r} is reserved")
    if topology and (folded in TOPOLOGY_DENY or reserved_fold in TOPOLOGY_DENY):
        raise ConfigError(f"{label} {value!r} is denied for bridge topology")


def validate_attribution(value, allowed_kinds):
    """Check post 0.9.0 sender attribution keys (r5.4 post-782).

    ``from_participant``/``from_lineage`` follow post's own grammar for
    participant ids and lineage names (one path component, no ``:``) under
    the bridge's stricter component check. They are attribution only and are
    never consulted for workspace mail binding, admission or rules. Channel
    import separately admits a stamped roomless sender when ``from`` equals
    ``from_participant`` and the branch host matches ``from_host``.
    """
    for key in ATTRIBUTION_KEYS:
        if key not in value:
            continue
        try:
            validate_path_component(value[key], label=key)
        except ConfigError as error:
            raise ConfigError(f"malformed_header: {error}")
        if ":" in value[key]:
            raise ConfigError(f"malformed_header: {key} must not contain ':'")
    if "address_kind" in value and value["address_kind"] not in allowed_kinds:
        raise ConfigError("malformed_header: invalid address_kind")


# r5.5 (M2; Aster rulings 20260923-032613, 20260923-034338). The bridge
# delivers on the evidence post consumes, not on its own binding verdict.
SENDER_NOT_HOMED = "sender_not_homed"
REMOTE_PARTICIPANT_UNHOMED = "remote_participant_unhomed"


def post_expand_room_path(value):
    """Expand a stored rooms.json path exactly as post's expand_room_path.

    `~` and `~/rest` resolve against HOME (post runs with the bridge's
    HOME); any other value must be absolute. Nothing is canonicalized:
    post compares the expansion lexically. None means post refuses it.
    """
    if not isinstance(value, str):
        return None
    home = os.environ.get("HOME")
    if value == "~" or value == "~/":
        return PurePosixPath(home) if home else None
    if value.startswith("~/"):
        return PurePosixPath(home, value[2:]) if home else None
    path = PurePosixPath(value)
    return path if path.is_absolute() else None


def post_sees_remote(root, host, sender, post_rooms):
    """Does post's own room table read `sender` as a placeholder of `host`?

    post keys rooms.json by the exact `from` spelling (no fold) and treats
    the sender as remote when its expanded path lies under <root>/remote/.
    The bridge demands the stricter host-bound form: exactly
    <root>/remote/<host>/<sender>. `post_rooms` is the table re-read after
    placeholder registration; None (never read) sees nothing.
    """
    if not post_rooms:
        return False
    expanded = post_expand_room_path(post_rooms.get(sender))
    if expanded is None:
        return False
    return expanded == PurePosixPath(str(root), "remote", host, sender)


class HoldLedger:
    """First-seen stamps for items held without a final outcome.

    A stamp lives at bridge/<namespace>/<host>/<a>/<b> and holds the UTC
    time the item was first held. `hold` records this tick's holds and keeps
    an existing stamp's time; `release` drops one stamp when its item
    reaches an outcome; `settle(host)` runs only after a walk that saw every
    held entry of the host and drops stamps for items it no longer holds.
    Health reports every stamp on disk per peer host (`summary`).
    """

    def __init__(self, root, namespace):
        self.root = root
        self.namespace = namespace
        self.held = {}

    def _path(self, host, *parts):
        return destination(self.root, "bridge", self.namespace, host, *parts)

    def hold(self, host, *parts):
        path = self._path(host, *parts)
        first = None
        try:
            first = epoch_from_iso(open_regular(path, 64).decode("ascii").strip())
        except (FileNotFoundError, ConfigError, UnicodeDecodeError, OSError):
            first = None
        if first is None:
            stamp = utc_now()
            atomic_replace(path, (stamp + "\n").encode("ascii"), self.root)
            first = epoch_from_iso(stamp)
        self.held.setdefault(host, {})[tuple(parts)] = first
        return first

    def release(self, host, *parts):
        try:
            self._path(host, *parts).unlink()
        except FileNotFoundError:
            pass

    def settle(self, host):
        keep = self.held.get(host, {})
        base = self._path(host)
        for directory, _, files in os.walk(str(base)):
            for name in files:
                path = Path(directory) / name
                if name.startswith("."):
                    continue
                if tuple(path.relative_to(base).parts) in keep:
                    continue
                try:
                    path.unlink()
                except FileNotFoundError:
                    pass

    def summary(self, peers, now=None, prune=True):
        """Per-host count and oldest age of every stamp on disk (r5.6, P2).

        The stamps, not this tick's holds, are the record: a host whose
        walk failed, was missing or stopped before its held entries keeps
        its count. Stamps of a host that is no longer a peer are dropped,
        since nothing will ever retry them, but only when ``prune`` says
        the peer set is positively known (``Snapshot.peers_known``); an
        unknown peer set keeps and reports every host's stamps.
        """
        now = time.time() if now is None else now
        base = destination(self.root, "bridge", self.namespace)
        peers = set(peers)
        result = {}
        try:
            hosts = sorted(os.listdir(str(base)))
        except FileNotFoundError:
            return result
        for host in hosts:
            directory = base / host
            if not directory.is_dir() or directory.is_symlink():
                continue
            if host not in peers and prune:
                shutil.rmtree(str(directory), ignore_errors=True)
                continue
            times = []
            for folder, _, files in os.walk(str(directory)):
                for name in files:
                    if name.startswith("."):
                        continue
                    path = Path(folder) / name
                    try:
                        first = epoch_from_iso(
                            open_regular(path, 64).decode("ascii").strip()
                        )
                    except (FileNotFoundError, ConfigError, UnicodeDecodeError, OSError):
                        first = None
                    if first is None:
                        try:
                            first = path.lstat().st_mtime
                        except OSError:
                            continue
                    times.append(first)
            if times:
                result[host] = {
                    "count": len(times),
                    "oldest_age_seconds": max(0, int(now - min(times))),
                }
        return result


def validate_id(value, label="mail id"):
    if not isinstance(value, str) or ID_RE.fullmatch(value) is None:
        raise ConfigError(f"{label} must match YYYYmmdd-HHMMSS-<6 lowercase hex>")


def parse_positive_int(raw, name, default, maximum=None):
    if raw is None:
        return default
    try:
        value = int(raw)
    except ValueError:
        raise ConfigError(f"{name} must be a positive integer")
    if value <= 0 or (maximum is not None and value > maximum):
        suffix = f" no greater than {maximum}" if maximum is not None else ""
        raise ConfigError(f"{name} must be a positive integer{suffix}")
    return value


def open_regular(path, max_bytes=None):
    metadata = path.lstat()
    if not stat.S_ISREG(metadata.st_mode):
        raise ConfigError(f"{path} is not a regular file")
    if max_bytes is not None and metadata.st_size > max_bytes:
        raise ConfigError(f"{path} exceeds {max_bytes} bytes")
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(str(path), flags)
    try:
        held = os.fstat(descriptor)
        if not stat.S_ISREG(held.st_mode) or (held.st_dev, held.st_ino) != (
            metadata.st_dev,
            metadata.st_ino,
        ):
            raise ConfigError(f"{path} changed while it was opened")
        if max_bytes is not None and held.st_size > max_bytes:
            raise ConfigError(f"{path} exceeds {max_bytes} bytes")
        chunks = []
        remaining = held.st_size
        while remaining:
            chunk = os.read(descriptor, min(remaining, 65536))
            if not chunk:
                break
            chunks.append(chunk)
            remaining -= len(chunk)
        data = b"".join(chunks)
        if len(data) != held.st_size:
            raise ConfigError(f"{path} changed size while it was read")
        return data
    finally:
        os.close(descriptor)


def fsync_directory(path):
    descriptor = os.open(str(path), os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def atomic_replace(path, data, root):
    if not under(root, path):
        raise ConfigError(f"write escapes root: {path}")
    ensure_dir(root, path.parent)
    try:
        if path.lstat().st_mode and path.is_symlink():
            raise ConfigError(f"refusing to replace symlink {path}")
    except FileNotFoundError:
        pass
    temporary = path.parent / f".{path.name}.{os.getpid()}.{time.time_ns()}.tmp"
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_CLOEXEC", 0)
    descriptor = os.open(str(temporary), flags, 0o600)
    try:
        remaining = memoryview(data)
        while remaining:
            written = os.write(descriptor, remaining)
            if written <= 0:
                raise OSError("short write while publishing file")
            remaining = remaining[written:]
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
    try:
        os.replace(str(temporary), str(path))
        fsync_directory(path.parent)
    finally:
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass


def exclusive_publish(path, data, temp_dir, root):
    if not under(root, path) or not under(root, temp_dir):
        raise ConfigError("exclusive publication escapes root")
    ensure_dir(root, path.parent)
    ensure_dir(root, temp_dir)
    temporary = temp_dir / f".{path.name}.{os.getpid()}.{time.time_ns()}.tmp"
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_CLOEXEC", 0)
    descriptor = os.open(str(temporary), flags, 0o600)
    try:
        remaining = memoryview(data)
        while remaining:
            written = os.write(descriptor, remaining)
            if written <= 0:
                raise OSError("short write while publishing file")
            remaining = remaining[written:]
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
    try:
        os.link(str(temporary), str(path), follow_symlinks=False)
        fsync_directory(path.parent)
    finally:
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass
        fsync_directory(temp_dir)


def checkpoint(name):
    if os.environ.get("BRIDGE_CRASH_AFTER") == name:
        # Hard crash: no cleanup, no flush — recovery must survive SIGKILL
        # semantics exactly as the SPEC's crash matrix requires.
        os.kill(os.getpid(), signal.SIGKILL)
    if os.environ.get("BRIDGE_RAISE_AFTER") == name:
        raise OSError(f"injected fault at checkpoint {name}")


def load_rules(settings):
    path = destination(settings.root, "rules.json")
    try:
        data = open_regular(path, 1024 * 1024)
    except FileNotFoundError:
        return []
    # Shape-only validation (ruling R1): post owns this file. Any problem is
    # TickError("rules_invalid") — never ConfigError/exit 2 — so a malformed
    # file stops the inbound batch with exit 1 instead of config-fataling
    # forever.
    try:
        value = load_json_bytes(data, str(path))
        if not isinstance(value, dict) or not isinstance(value.get("blocked"), list):
            raise ConfigError("rules.json must contain a blocked list")
        rules = []
        for index, rule in enumerate(value["blocked"]):
            if not isinstance(rule, dict):
                raise ConfigError(f"rules.json blocked[{index}] must be an object")
            if not all(
                isinstance(rule.get(key), str) and rule[key].strip()
                for key in ("from", "to", "reason")
            ):
                raise ConfigError(f"rules.json blocked[{index}] has invalid fields")
            if rule["from"] != "*":
                validate_path_component(rule["from"], label="blocked.from")
            if rule["to"] != "*":
                validate_path_component(rule["to"], label="blocked.to")
            rules.append(rule)
    except ConfigError as error:
        raise TickError("rules_invalid", str(error))
    return rules


def blocked_reason(rules, sender, recipient):
    for rule in rules:
        if (rule["from"] == "*" or rule["from"] == sender) and (
            rule["to"] == "*" or rule["to"] == recipient
        ):
            return rule["reason"]
    return None
