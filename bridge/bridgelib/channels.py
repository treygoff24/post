"""Estate-wide Post channel import and publication (SPEC-v2 r5.1).

Peer trees are hostile Git objects.  Local channel files are hostile local
inputs.  The tick owns fetch/commit/push; this module owns only the channel
state machine and its durable bridge markers.
"""

import datetime as dt
import fcntl
import hashlib
import json
import os
import re
import stat
import time
from contextlib import contextmanager
from dataclasses import dataclass, field
from pathlib import Path, PurePosixPath
from typing import Dict, FrozenSet, List, Optional, Set

from . import common, decided, pmail
from .snapshot import (
    FORGED_SELF,
    LOCAL,
    NAME_COLLISION,
    UNHOMED,
    UNPUBLISHED_SENDER,
    VERIFIED,
    binding_verdict,
)

CHANNEL_ID_RE = re.compile(r"^\d{8}-\d{6}-\d{6}-[0-9a-f]{6}$")
CHANNEL_CONFIG_KEYS = frozenset({"mode", "allow", "deny"})
CHANNEL_RECORD_REQUIRED = frozenset({"name", "created", "created_by"})
CHANNEL_RECORD_OPTIONAL = frozenset({"description"})
CHANNEL_ENVELOPE_REQUIRED = frozenset({"id", "from", "channel", "subject", "sent"})
CHANNEL_ENVELOPE_OPTIONAL = frozenset(
    {
        "event",
        "display_name",
        "pfp",
        "re",
        "mentions",
        "signature_ref",
        "sender_address",
        "sender_provenance",
        "address_kind",
        "from_host",
        *common.ATTRIBUTION_KEYS,
    }
)
CHANNEL_ADDRESS_KINDS = frozenset({"channel"})
CHANNEL_RECORD_MAX = 4096
CHANNEL_DESCRIPTION_MAX = 1024
CHANNEL_HEADER_MAX = 4096
CHANNEL_TREE_MAX = 50000
CHANNEL_SEEN_MAX = 1000
CHANNEL_PAGE_MAX = 4096
CHANNEL_DEADLINE_STRIDE = 500
CHANNEL_TIP_RE = re.compile(r"^[0-9a-f]{40,64}$")


@dataclass(frozen=True)
class ChannelsConfig:
    mode: str
    allow: FrozenSet[str] = frozenset()
    deny: FrozenSet[str] = frozenset()


@dataclass
class ChannelStats:
    imported: int = 0
    published: int = 0
    quarantined: int = 0
    unpublishable: int = 0
    diverged: List[dict] = field(default_factory=list)
    rewritten: List[str] = field(default_factory=list)
    reasons: Dict[str, int] = field(default_factory=dict)
    completed: FrozenSet[str] = frozenset()
    # m7: host -> {count, oldest_age_seconds} of messages this tick left
    # unimported and unmarked (held, or waiting for a channel record).
    held: dict = field(default_factory=dict)

    def count_reason(self, reason: str) -> None:
        self.reasons[reason] = self.reasons.get(reason, 0) + 1


def parse_channels_config(value) -> Optional[ChannelsConfig]:
    """Validate config.json's optional ``channels`` entry.

    ``None`` (an explicit ``null``) means channel sync off. An absent key never
    reaches here as None: ``sweep.load_config`` supplies ``{"mode": "all"}``
    (SPEC-v2 §Channel config, r6.2).
    """
    if value is None:
        return None
    if not isinstance(value, dict):
        raise common.ConfigError("config.channels must be an object or null")
    unknown = set(value) - CHANNEL_CONFIG_KEYS
    if unknown:
        raise common.ConfigError(
            "config.channels has unknown keys: {}".format(", ".join(sorted(unknown)))
        )
    mode = value.get("mode")
    if mode not in ("all", "allow"):
        raise common.ConfigError("config.channels.mode must be 'all' or 'allow'")
    if mode == "allow" and "allow" not in value:
        raise common.ConfigError("config.channels.allow is required in allow mode")

    normalized = {}
    for key in ("allow", "deny"):
        raw = value.get(key, [])
        if not isinstance(raw, list):
            raise common.ConfigError(f"config.channels.{key} must be a list")
        names = []
        for index, name in enumerate(raw):
            common.validate_room(
                name,
                topology=True,
                label=f"config.channels.{key}[{index}]",
            )
            names.append(name)
        normalized[key] = frozenset(names)
    return ChannelsConfig(mode, normalized["allow"], normalized["deny"])


def channel_allowed(cfg, name) -> bool:
    """Return whether ``name`` is enabled in both bridge directions."""
    if cfg is None or name in cfg.deny:
        return False
    return cfg.mode == "all" or name in cfg.allow


def _json_bytes(value) -> bytes:
    return (json.dumps(value, sort_keys=True, indent=2) + "\n").encode("utf-8")


def _marker_bytes(value: str) -> bytes:
    return (value + "\n").encode("utf-8")


def _parse_time(value: str, reason: str) -> None:
    try:
        dt.datetime.strptime(value, "%Y-%m-%d %H:%M:%S %z")
    except ValueError:
        raise common.ConfigError(reason)


def _validate_channel_id(value, label="channel id") -> None:
    if not isinstance(value, str) or CHANNEL_ID_RE.fullmatch(value) is None:
        raise common.ConfigError(
            f"{label} must match YYYYmmdd-HHMMSS-ffffff-<6 lowercase hex>"
        )


def _parse_record(data: bytes, expected_name: str, label: str) -> dict:
    if len(data) > CHANNEL_RECORD_MAX:
        raise common.ConfigError("channel record exceeds 4 KiB")
    value = common.load_json_bytes(data, label)
    if not isinstance(value, dict):
        raise common.ConfigError("channel record must be an object")
    keys = set(value)
    if not CHANNEL_RECORD_REQUIRED.issubset(keys):
        missing = ",".join(sorted(CHANNEL_RECORD_REQUIRED - keys))
        raise common.ConfigError(f"channel record missing {missing}")
    if not keys.issubset(CHANNEL_RECORD_REQUIRED | CHANNEL_RECORD_OPTIONAL):
        unknown = ",".join(
            sorted(keys - CHANNEL_RECORD_REQUIRED - CHANNEL_RECORD_OPTIONAL)
        )
        raise common.ConfigError(f"channel record has unknown keys: {unknown}")
    for key in CHANNEL_RECORD_REQUIRED:
        if not isinstance(value[key], str):
            raise common.ConfigError(f"channel record {key} must be a string")
    if value["name"] != expected_name:
        raise common.ConfigError("channel record name does not match path")
    common.validate_room(value["name"], topology=True, label="channel name")
    common.validate_room(value["created_by"], label="channel created_by")
    _parse_time(value["created"], "channel record created is invalid")
    if "description" in value:
        description = value["description"]
        if not isinstance(description, str):
            raise common.ConfigError("channel record description must be a string")
        if len(description.encode("utf-8")) > CHANNEL_DESCRIPTION_MAX:
            raise common.ConfigError("channel record description exceeds 1 KiB")
        if any(
            ord(character) < 32 or 127 <= ord(character) <= 159
            for character in description
        ):
            raise common.ConfigError(
                "channel record description contains a control character"
            )
    return value


def _parse_envelope(
    data: bytes,
    expected_name: str,
    expected_id: str,
    logger,
    unknown_seen: Set[str],
) -> dict:
    separator = data.find(b"\n---\n")
    if separator < 0:
        raise common.ConfigError("malformed_header: missing envelope separator")
    if separator > CHANNEL_HEADER_MAX:
        raise common.ConfigError("malformed_header: header exceeds 4 KiB")
    try:
        value = common.load_json_bytes(data[:separator], "channel envelope")
    except common.ConfigError as error:
        raise common.ConfigError(f"malformed_header: {error}")
    if not isinstance(value, dict):
        raise common.ConfigError("malformed_header: envelope must be an object")
    missing = CHANNEL_ENVELOPE_REQUIRED - set(value)
    if missing:
        raise common.ConfigError(
            "malformed_header: missing {}".format(",".join(sorted(missing)))
        )
    for key in CHANNEL_ENVELOPE_REQUIRED:
        if not isinstance(value[key], str):
            raise common.ConfigError(f"malformed_header: {key} must be a string")
    _validate_channel_id(value["id"], "envelope id")
    common.validate_room(value["from"], label="envelope from")
    common.validate_room(value["channel"], topology=True, label="envelope channel")
    if value["id"] != expected_id:
        raise common.ConfigError("id_mismatch")
    if value["channel"] != expected_name:
        raise common.ConfigError("channel_mismatch")
    _parse_time(value["sent"], "invalid_sent")

    for key in (
        "display_name",
        "pfp",
        "sender_address",
        "sender_provenance",
        "address_kind",
        "from_host",
        *common.ATTRIBUTION_KEYS,
    ):
        if key in value and not isinstance(value[key], str):
            raise common.ConfigError(f"malformed_header: {key} must be a string")
    common.validate_attribution(value, CHANNEL_ADDRESS_KINDS)
    if "from_host" in value and common.HOST_RE.fullmatch(value["from_host"]) is None:
        raise common.ConfigError("malformed_header: invalid from_host")
    # An event kind is any string. `join` is the one kind the bridge acts on
    # (membership, in _membership_locked); every other kind, `profile` and
    # kinds a newer post invents alike, is an opaque system event that
    # imports and relays byte-for-byte. Refusing an unknown kind here used to
    # quarantine it and wedge the channel for every member (post-11n).
    if "event" in value and not isinstance(value["event"], str):
        raise common.ConfigError("malformed_header: event must be a string")
    if "re" in value:
        _validate_channel_id(value["re"], "envelope re")
    if "mentions" in value:
        mentions = value["mentions"]
        if not isinstance(mentions, list):
            raise common.ConfigError("malformed_header: mentions must be a list")
        for mention in mentions:
            common.validate_room(mention, label="envelope mention")

    unknown = set(value) - CHANNEL_ENVELOPE_REQUIRED - CHANNEL_ENVELOPE_OPTIONAL
    for key in sorted(unknown - unknown_seen):
        logger.emit("chan_unknown_key", key=key)
        unknown_seen.add(key)
    return value


def _read_optional(path: Path, maximum: int) -> Optional[bytes]:
    try:
        return common.open_regular(path, maximum)
    except FileNotFoundError:
        return None


def _roomless_sender(envelope: dict) -> bool:
    sender = envelope.get("from_participant")
    return isinstance(sender, str) and sender == envelope["from"]


def _stamp_roomless_host(data: bytes, envelope: dict, host: str) -> bytes:
    """Put authenticated branch origin in the relayed copy, preserving the body."""
    if envelope.get("from_host") not in (None, host):
        raise common.ConfigError("roomless sender has a different from_host")
    _, body = data.split(b"\n---\n", 1)
    stamped = {**envelope, "from_host": host}
    header = json.dumps(stamped, sort_keys=True, indent=2, ensure_ascii=True).encode()
    if len(header) > CHANNEL_HEADER_MAX:
        raise common.ConfigError("stamped channel header exceeds 4 KiB")
    return header + b"\n---\n" + body


def _same_unstamped_roomless_copy(
    existing: bytes, incoming: bytes, envelope: dict,
    name: str, message_id: str, logger, unknown_seen,
    reservation: Optional[str], expected_reservation: str,
) -> bool:
    """Recognize a verbatim shim copy of this stamped roomless message."""
    if (
        reservation not in (None, expected_reservation)
        or not _roomless_sender(envelope)
        or "from_host" not in envelope
    ):
        return False
    try:
        local = _parse_envelope(existing, name, message_id, logger, unknown_seen)
    except common.ConfigError:
        return False
    if "from_host" in local:
        return False
    _, local_body = existing.split(b"\n---\n", 1)
    _, incoming_body = incoming.split(b"\n---\n", 1)
    unstamped = {key: value for key, value in envelope.items() if key != "from_host"}
    return local == unstamped and local_body == incoming_body


def _publish_once(path: Path, data: bytes, temp_dir: Path, root: Path) -> bool:
    try:
        common.exclusive_publish(path, data, temp_dir, root)
        return True
    except FileExistsError:
        return False


def _replace_mail_file(settings, path: Path, data: bytes) -> None:
    """Stage a replace under bridge/tmp, never beside a Post-owned file."""
    temp = common.destination(settings.root, "bridge", "tmp")
    common.ensure_dir(settings.root, path.parent)
    common.ensure_dir(settings.root, temp)
    staged = common.destination(
        temp, f".{path.name}.{os.getpid()}.{time.time_ns()}.stage"
    )
    try:
        common.exclusive_publish(staged, data, temp, settings.root)
        common.checkpoint("channels-mail-stage")
        os.replace(str(staged), str(path))
        common.fsync_directory(path.parent)
    finally:
        try:
            staged.unlink()
        except FileNotFoundError:
            pass


def _fence(fence_present) -> None:
    if fence_present():
        raise common.TickError("fenced")


@dataclass(frozen=True)
class ArchiveCheck:
    allowed: bool
    additions: Optional[FrozenSet[str]] = None
    full_walk: bool = False
    tip: Optional[str] = None

    def __bool__(self):
        return self.allowed


@dataclass(frozen=True)
class PageState:
    """What one page of a peer's channel tree consumed and left behind."""

    processed: int = 0
    remaining: int = 0
    cursor: Optional[str] = None
    tip: Optional[str] = None
    oid: Optional[str] = None
    # r5.6 (P1): the walk resumed behind a cursor, so entries before it were
    # not looked at this tick.
    resumed: bool = False


@dataclass(frozen=True)
class PageCursor:
    """Where a paged walk stopped, and the tree it stopped in."""

    tip: str
    oid: str
    path: str
    # r5.6 (P1): an earlier page of this walk held or skipped a recoverable
    # entry that is now behind the cursor. The walk must not complete.
    skipped: bool = False


def _once(emitted, host, action) -> bool:
    """True the first time ``action`` is reported for ``host`` this tick.

    ``check_archive`` runs once per host per tick, but the caller may hand in
    a per-tick set so a second call can never double-log a per-host verdict.
    """
    if emitted is None:
        return True
    key = (host, action)
    if key in emitted:
        return False
    emitted.add(key)
    return True


def _mark_rewritten(settings, logger, host, marker, tip, rows, emitted=None):
    payload = _marker_bytes(tip)
    if _read_optional(marker, 256) != payload:
        common.atomic_replace(marker, payload, settings.root)
    if _once(emitted, host, "relay_history_rewritten"):
        logger.emit("relay_history_rewritten", host=host, rows=rows[:20])


def check_archive(settings, git, host, oid, logger, emitted=None) -> ArchiveCheck:
    """Reject a peer tip whose channel tree changes existing archive data.

    The tick calls this exactly once per peer, from ``import_channels``, which
    owns the check and reports its verdict through ``completed`` and
    ``rewritten``.
    """
    tip_path = common.destination(settings.root, "bridge", "chan-tip", host)
    marker = common.destination(settings.root, "bridge", "chan-rewritten", host)
    try:
        tip = common.open_regular(tip_path, 256).decode("ascii").strip()
    except FileNotFoundError:
        return ArchiveCheck(True, full_walk=True)
    except (common.ConfigError, UnicodeDecodeError):
        raise common.TickError("channel_tip_invalid", f"invalid channel tip for {host}")
    if CHANNEL_TIP_RE.fullmatch(tip) is None:
        logger.emit("channel_tip_invalid", host=host)
        raise common.TickError("channel_tip_invalid", f"invalid channel tip for {host}")

    frozen_at = _read_optional(marker, 256)
    if frozen_at is not None:
        try:
            frozen_tip = frozen_at.decode("ascii").strip()
        except UnicodeDecodeError:
            frozen_tip = ""
        if frozen_tip == tip:
            if _once(emitted, host, "relay_history_rewritten"):
                logger.emit("relay_history_rewritten", host=host, rows=[])
            return ArchiveCheck(False)
        _unlink_marker(marker)
        return ArchiveCheck(True, full_walk=True, tip=tip)

    return archive_delta(settings, git, host, tip, oid, logger, emitted)


def archive_delta(settings, git, host, base, oid, logger, emitted=None) -> ArchiveCheck:
    """The A-only (plus ``channel.json`` M) rule, from ``base`` to ``oid``.

    Used both for the ordinary ``chan-tip`` delta and, on a paged walk that
    resumes into a newer tree, for ``chan-page`` cursor's OID: either way a
    peer that changes existing archive data freezes here.
    """
    marker = common.destination(settings.root, "bridge", "chan-rewritten", host)
    result = git.run(
        ["diff", "--name-status", base, oid, "--", "channels/"],
        check=False,
    )
    if result.returncode != 0:
        _mark_rewritten(
            settings, logger, host, marker, base, ["git_diff_failed"], emitted
        )
        return ArchiveCheck(False)
    rows = [row for row in result.stdout.splitlines() if row]

    def permitted(row):
        fields = row.split("\t")
        if fields[0] == "A":
            return True
        if fields[0] != "M" or len(fields) != 2:
            return False
        parts = PurePosixPath(fields[1]).parts
        return (
            len(parts) == 3
            and parts[0] == "channels"
            and parts[2] == "channel.json"
        )

    bad = [row for row in rows if not permitted(row)]
    if bad:
        _mark_rewritten(settings, logger, host, marker, base, bad, emitted)
        return ArchiveCheck(False)
    additions = frozenset(
        fields[1]
        for row in rows
        for fields in (row.split("\t"),)
        if fields[0] == "A" and len(fields) == 2
    )
    return ArchiveCheck(True, additions=additions, tip=base)


def advance_tip(settings, host, oid):
    """Persist ``oid`` after the caller completes all channel work for host."""
    path = common.destination(settings.root, "bridge", "chan-tip", host)
    common.atomic_replace(path, _marker_bytes(oid), settings.root)


def _seen_once(settings, category, host, path, oid, fence_present, budget) -> bool:
    """Persisted once-per-(host, path, oid) dedup, bounded per tick.

    ``budget`` caps new markers at ``CHANNEL_SEEN_MAX`` per (category, host)
    per tick: beyond it the item is neither logged nor marked, so a peer
    cannot spend this node's inodes at the rate it publishes junk paths.
    """
    identity = hashlib.sha256(
        (str(path) + "\0" + str(oid)).encode("utf-8", "surrogateescape")
    ).hexdigest()
    marker = common.destination(
        settings.root, "bridge", "chan-seen", category, host, identity
    )
    if marker.exists() or marker.is_symlink():
        return False
    if budget is not None:
        key = (category, host)
        spent = budget.get(key, 0)
        if spent >= CHANNEL_SEEN_MAX:
            return False
        budget[key] = spent + 1
    _fence(fence_present)
    return _publish_once(
        marker,
        b"",
        common.destination(settings.root, "bridge", "tmp"),
        settings.root,
    )


def _log_quarantine(
    settings,
    stats,
    logger,
    host,
    name,
    message_id,
    reason,
    path,
    oid,
    fence_present,
    budget,
):
    if not _seen_once(
        settings, "quarantined", host, path, oid, fence_present, budget
    ):
        return False
    stats.quarantined += 1
    stats.count_reason(reason)
    fields = {"host": host, "channel": name, "id": message_id, "reason": reason}
    fields["path"] = path
    logger.emit("chan_quarantined", **fields)
    return True


def _log_ignored(settings, logger, host, path, oid, reason, fence_present, budget):
    if _seen_once(settings, "ignored", host, path, oid, fence_present, budget):
        fields = {"host": host, "reason": reason}
        if path is not None:
            fields["path"] = path
        logger.emit("chan_ignored", **fields)


def _forensic(
    settings,
    host: str,
    name: str,
    filename: str,
    data: bytes,
    fence_present,
) -> None:
    _fence(fence_present)
    path = common.destination(
        settings.root, "bridge", "quarantine", "channels", host, name, filename
    )
    _publish_once(
        path,
        data,
        common.destination(settings.root, "bridge", "tmp"),
        settings.root,
    )


def _load_diverged(settings) -> List[dict]:
    path = common.destination(settings.root, "bridge", "chan-diverged.json")
    try:
        value = common.load_json_bytes(
            common.open_regular(path, 1024 * 1024), str(path)
        )
    except FileNotFoundError:
        return []
    except (common.ConfigError, OSError) as error:
        raise common.TickError("chan_diverged_invalid", str(error))
    if not isinstance(value, list):
        raise common.TickError("chan_diverged_invalid")
    result = []
    for item in value:
        if not (
            isinstance(item, dict)
            and set(item) == {"channel", "id", "hosts"}
            and isinstance(item["channel"], str)
            and isinstance(item["id"], str)
            and isinstance(item["hosts"], list)
            and all(isinstance(host, str) for host in item["hosts"])
        ):
            raise common.TickError("chan_diverged_invalid")
        result.append(item)
    return result


def _record_divergence(
    settings,
    stats: ChannelStats,
    logger,
    host: str,
    name: str,
    message_id: str,
    data: bytes,
    other_host: str,
    fence_present,
) -> None:
    _forensic(
        settings,
        host,
        name,
        f"{message_id}-{host}-conflict.msg",
        data,
        fence_present,
    )
    _fence(fence_present)
    hosts = sorted({host, other_host})
    item = {"channel": name, "id": message_id, "hosts": hosts}
    persisted = _load_diverged(settings)
    key = (name, message_id, tuple(hosts))
    if key not in {
        (row["channel"], row["id"], tuple(row["hosts"])) for row in persisted
    }:
        persisted.append(item)
        persisted.sort(key=lambda row: (row["channel"], row["id"], row["hosts"]))
        common.atomic_replace(
            common.destination(settings.root, "bridge", "chan-diverged.json"),
            _json_bytes(persisted),
            settings.root,
        )
    if item not in stats.diverged:
        stats.diverged.append(item)
    stats.count_reason("channel_diverged")
    logger.emit("channel_diverged", **item)


@contextmanager
def _channel_lock(settings, fence_present):
    _fence(fence_present)
    channels_root = common.destination(settings.root, "channels")
    common.ensure_dir(settings.root, channels_root)
    path = common.destination(settings.root, "channels", ".channels.lock")
    flags = (
        os.O_RDWR
        | os.O_CREAT
        | getattr(os, "O_CLOEXEC", 0)
        | getattr(os, "O_NOFOLLOW", 0)
    )
    descriptor = os.open(str(path), flags, 0o600)
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode):
            raise common.ConfigError("channels lock must be a regular file")
        fcntl.flock(descriptor, fcntl.LOCK_EX)
        yield
    finally:
        os.close(descriptor)


def _origin(snapshot, created_by: str) -> str:
    if created_by in snapshot.real_rooms:
        return LOCAL
    return snapshot.route_for(created_by) or LOCAL


def _load_local_record(settings, name: str) -> Optional[dict]:
    path = common.destination(settings.root, "channels", name, "channel.json")
    data = _read_optional(path, CHANNEL_RECORD_MAX)
    if data is None:
        return None
    return _parse_record(data, name, str(path))


def _ensure_channel_locked(
    settings,
    name: str,
    peer_record: dict,
    snapshot,
    logger,
    fence_present,
) -> Optional[dict]:
    """C6: independently reconcile every channel-state component."""
    _fence(fence_present)
    channel_root = common.destination(settings.root, "channels", name)
    messages = common.destination(channel_root, "messages")
    common.ensure_dir(settings.root, messages)
    record_path = common.destination(channel_root, "channel.json")
    if not (record_path.exists() or record_path.is_symlink()):
        _publish_once(
            record_path,
            _json_bytes(peer_record),
            common.destination(settings.root, "bridge", "tmp"),
            settings.root,
        )
    try:
        local_record = _load_local_record(settings, name)
    except common.ConfigError:
        raise
    if local_record is None:
        raise common.ConfigError("local channel record is missing")

    members_path = common.destination(channel_root, "members.json")
    if not (members_path.exists() or members_path.is_symlink()):
        _publish_once(
            members_path,
            b"{}\n",
            common.destination(settings.root, "bridge", "tmp"),
            settings.root,
        )
    origin_path = common.destination(settings.root, "bridge", "chan-origin", name)
    computed = _origin(snapshot, local_record["created_by"])
    desired = _marker_bytes(computed)
    existing = _read_optional(origin_path, 256)
    if existing is None:
        _publish_once(
            origin_path,
            desired,
            common.destination(settings.root, "bridge", "tmp"),
            settings.root,
        )
    elif existing != desired:
        common.atomic_replace(origin_path, desired, settings.root)
        logger.emit(
            "chan_origin_changed",
            channel=name,
            old=existing.decode("utf-8", "replace").strip(),
            new=computed,
        )
    return local_record


def _read_members(settings, name: str) -> dict:
    path = common.destination(settings.root, "channels", name, "members.json")
    data = common.open_regular(path, 1024 * 1024)
    value = common.load_json_bytes(data, str(path))
    if not isinstance(value, dict) or not all(
        isinstance(member, str) and isinstance(sent, str)
        for member, sent in value.items()
    ):
        raise common.ConfigError("channel_members_invalid")
    return value


def _participant_members(settings, name: str) -> tuple[list[str], bool]:
    """Addresses and ids of local participants explicitly joined to ``name``.

    Mirrors post 0.9.0 ``participants_for_join_validation``: every directory
    under ``<root>/participants`` except ``by-session`` holds
    ``participant.json`` and, once it has joined anything, ``channels.json``
    (``{"version": 1, "joined": [...], "left": [...]}``). A participant whose
    ``joined`` names the channel is a member under both its reply address
    (workspace, else its id) and its id. A participant that is neither joined
    nor left falls back to ``members.json``, which the caller already reads.
    Returns ``(names, unreadable)``; ``unreadable`` is true when any record
    could not be read, so the caller can fail closed.
    """
    root = common.destination(settings.root, "participants")
    try:
        entries = sorted(os.scandir(root), key=lambda entry: entry.name)
    except FileNotFoundError:
        return [], False
    except OSError:
        return [], True
    names: list[str] = []
    unreadable = False
    for entry in entries:
        if entry.name == "by-session" or not entry.is_dir(follow_symlinks=False):
            continue
        directory = Path(entry.path)
        try:
            record = common.load_json_bytes(
                common.open_regular(directory / "participant.json", 64 * 1024),
                entry.path,
            )
            state_path = directory / "channels.json"
            if not (state_path.exists() or state_path.is_symlink()):
                continue
            state = common.load_json_bytes(
                common.open_regular(state_path, 1024 * 1024), str(state_path)
            )
        except (common.ConfigError, OSError):
            unreadable = True
            continue
        joined = state.get("joined") if isinstance(state, dict) else None
        if (
            not isinstance(record, dict)
            or not isinstance(joined, list)
            or not all(isinstance(item, str) for item in joined)
        ):
            unreadable = True
            continue
        if name not in joined:
            continue
        identifier = record.get("id")
        workspace = record.get("workspace")
        if isinstance(workspace, str) and workspace:
            names.append(workspace)
        if isinstance(identifier, str) and identifier:
            names.append(identifier)
    return names, unreadable


def _rules_touch(rules, sender: str) -> bool:
    """True when some blocked rule could match a route to or from ``sender``."""
    return any(
        rule["from"] in ("*", sender) or rule["to"] in ("*", sender) for rule in rules
    )


def _local_members_readable(settings, name: str, logger, skipped: Set[str]) -> bool:
    """A malformed local members.json skips the channel, logged once (r5.3)."""
    try:
        path = common.destination(settings.root, "channels", name, "members.json")
        if not (path.exists() or path.is_symlink()):
            return True
        _read_members(settings, name)
    except (common.ConfigError, OSError) as error:
        if name not in skipped:
            skipped.add(name)
            logger.emit(
                "chan_local_unreadable", channel=name, id="", reason=str(error)
            )
        return False
    return True


def _unlink_marker(path: Path) -> None:
    try:
        path.unlink()
        common.fsync_directory(path.parent)
    except FileNotFoundError:
        pass


def _unlink_join_marker(path: Path) -> None:
    """Remove a join marker and the channel directory it was alone in.

    ``tick.probe`` folds the emptiness of ``bridge/chan-joins-{pending,held}``
    into ``pending_empty``, and it scans only the top level: an abandoned
    per-channel directory would keep quiet ticks off forever.
    """
    _unlink_marker(path)
    try:
        path.parent.rmdir()
        common.fsync_directory(path.parent.parent)
    except OSError:
        pass


def _membership_locked(
    settings,
    snapshot,
    name: str,
    message_id: str,
    envelope: dict,
    fence_present,
) -> None:
    if envelope.get("event") != "join":
        return
    sender = envelope["from"]
    pending = common.destination(
        settings.root, "bridge", "chan-joins-pending", name, message_id
    )
    held = common.destination(
        settings.root, "bridge", "chan-joins-held", name, message_id
    )
    if _roomless_sender(envelope) and "from_host" in envelope:
        # Participant membership lives in channels.json, not members.json.
        # Also retire markers made by a receiver running the earlier code.
        _fence(fence_present)
        _unlink_join_marker(pending)
        _unlink_join_marker(held)
        return
    registered = sender in snapshot.real_rooms or sender in snapshot.placeholders
    if not registered:
        _fence(fence_present)
        _publish_once(
            pending,
            b"unregistered sender",
            common.destination(settings.root, "bridge", "tmp"),
            settings.root,
        )
        return

    members = _read_members(settings, name)
    if sender in members:
        _fence(fence_present)
        _unlink_join_marker(pending)
        _unlink_join_marker(held)
        return
    rules = common.load_rules(settings)
    reason = None
    # post 0.9.0 records a local join in participants/<id>/channels.json, not
    # in members.json (now only the legacy workspace default), and its own
    # join admission checks both. Checking members.json alone would admit a
    # remote join that a local participant member's block rule forbids.
    local_members, unreadable = _participant_members(settings, name)
    candidates = list(members) + local_members
    for member in candidates:
        reason = common.blocked_reason(rules, sender, member) or common.blocked_reason(
            rules, member, sender
        )
        if reason is not None:
            break
    if reason is None and unreadable and _rules_touch(rules, sender):
        # An unreadable participant record might be a member a rule blocks;
        # post refuses the join in that case, so the bridge holds it.
        reason = "participant_state_unreadable"
    if reason is not None:
        _fence(fence_present)
        _publish_once(
            held,
            reason.encode("utf-8"),
            common.destination(settings.root, "bridge", "tmp"),
            settings.root,
        )
        return

    _fence(fence_present)
    members[sender] = envelope["sent"]
    _replace_mail_file(
        settings,
        common.destination(settings.root, "channels", name, "members.json"),
        _json_bytes(members),
    )
    _unlink_join_marker(pending)
    _unlink_join_marker(held)


def _adopt_description_locked(
    settings,
    snapshot,
    host: str,
    name: str,
    peer_record: dict,
    record_oid: str,
    logger,
    fence_present,
) -> None:
    local_record = _load_local_record(settings, name)
    if local_record is None or _origin(snapshot, local_record["created_by"]) != host:
        return
    marker = common.destination(settings.root, "bridge", "chan-desc", name)
    desired_marker = _marker_bytes(record_oid)
    if _read_optional(marker, 256) == desired_marker:
        return
    _fence(fence_present)
    adopted = {
        "name": local_record["name"],
        "created": local_record["created"],
        "created_by": local_record["created_by"],
    }
    if "description" in peer_record:
        adopted["description"] = peer_record["description"]
    record_path = common.destination(settings.root, "channels", name, "channel.json")
    payload = _json_bytes(adopted)
    if common.open_regular(record_path, CHANNEL_RECORD_MAX) != payload:
        _replace_mail_file(settings, record_path, payload)
    common.atomic_replace(marker, desired_marker, settings.root)
    logger.emit("chan_description_adopted", host=host, channel=name, oid=record_oid)


def _read_reservation(settings, name: str, message_id: str) -> Optional[str]:
    path = common.destination(
        settings.root, "bridge", "chan-received", name, message_id
    )
    data = _read_optional(path, 512)
    if data is None:
        return None
    try:
        return data.decode("ascii").strip()
    except UnicodeDecodeError:
        return "invalid"


def _reservation_parts(value: Optional[str]):
    if value is None:
        return None, None
    host, separator, sha256 = value.partition(" ")
    if not separator:
        return host, None
    return host, sha256


def _event_bytes(host: str, name: str, message_id: str, envelope: dict) -> bytes:
    return _json_bytes(
        {
            "host": host,
            "channel": name,
            "id": message_id,
            "from": envelope["from"],
            "event": envelope.get("event"),
            "mentions": envelope.get("mentions", []),
        }
    )


def _publish_message_batch(
    settings,
    host: str,
    name: str,
    message_id: str,
    data: bytes,
    sha256: str,
    envelope: dict,
    fence_present,
) -> bool:
    """C5(a-c), one fence-delimited mutation batch."""
    _fence(fence_present)
    temp = common.destination(settings.root, "bridge", "tmp")
    reservation = common.destination(
        settings.root, "bridge", "chan-received", name, message_id
    )
    expected = f"{host} {sha256}"
    reserved = _read_reservation(settings, name, message_id)
    if reserved is None:
        _publish_once(reservation, _marker_bytes(expected), temp, settings.root)
        reserved = _read_reservation(settings, name, message_id)
    if reserved != expected:
        return False
    common.checkpoint("channels-c5a")
    _fence(fence_present)
    event = common.destination(
        settings.root, "bridge", "events", name, message_id + ".json"
    )
    _publish_once(
        event, _event_bytes(host, name, message_id, envelope), temp, settings.root
    )
    common.checkpoint("channels-c5b")
    _fence(fence_present)
    message = common.destination(
        settings.root, "channels", name, "messages", message_id + ".msg"
    )
    created = _publish_once(message, data, temp, settings.root)
    common.checkpoint("channels-c5c")
    return created


def _is_record_path(raw_path) -> bool:
    if not raw_path:
        return False
    parts = PurePosixPath(raw_path).parts
    return len(parts) == 3 and parts[0] == "channels" and parts[2] == "channel.json"


def _valid_cursor_path(value) -> bool:
    if not isinstance(value, str) or not value or len(value) > CHANNEL_PAGE_MAX:
        return False
    for component in value.split("/"):
        try:
            common.validate_path_component(component, label="channel tree path")
        except common.ConfigError:
            return False
    return True


def _page_cursor_path(settings, host: str) -> Path:
    return common.destination(settings.root, "bridge", "chan-page", host)


def _read_page_cursor(settings, host: str, tip, logger) -> Optional[PageCursor]:
    """Resume point for a paged walk; hostile or stale input restarts it.

    Same local-input discipline as ``chan-tip``: a regular file under the
    size bound holding ``{"tip", "oid", "path"}`` and, since r5.6, a boolean
    ``skipped`` (absent reads as True), with an OID shaped like a tip and a
    path made of path-safe components. Anything else is treated as
    absent and logged once, and a cursor written against a different tip is
    stale — a hand-advanced tip restarts the walk.
    """
    try:
        path = _page_cursor_path(settings, host)
        data = common.open_regular(path, CHANNEL_PAGE_MAX)
    except FileNotFoundError:
        return None
    except (common.ConfigError, OSError) as error:
        logger.emit("chan_page_invalid", host=host, reason=str(error))
        return None
    try:
        value = common.load_json_bytes(data, str(path))
    except (common.ConfigError, UnicodeDecodeError) as error:
        logger.emit("chan_page_invalid", host=host, reason=str(error))
        return None
    if not (
        isinstance(value, dict)
        and set(value) - {"skipped"} == {"tip", "oid", "path"}
        and isinstance(value.get("skipped", False), bool)
        and isinstance(value["tip"], str)
        and isinstance(value["oid"], str)
        and CHANNEL_TIP_RE.fullmatch(value["oid"]) is not None
        and _valid_cursor_path(value["path"])
    ):
        logger.emit("chan_page_invalid", host=host, reason="malformed page cursor")
        return None
    if value["tip"] != (tip or ""):
        return None
    # A pre-r5.6 cursor has no "skipped" key and cannot say whether an
    # earlier page held something, so it reads as skipped: the walk ends
    # without completing the host and the next walk re-lists from the tip.
    return PageCursor(
        value["tip"], value["oid"], value["path"], value.get("skipped", True)
    )


def _write_page_cursor(
    settings, host: str, tip, oid: str, cursor: str, skipped: bool = False
) -> None:
    # A hostile cursor path has already been reported by the read that runs
    # first on this same host and tick, so failing to persist is silent: the
    # walk simply restarts next tick instead of resuming.
    try:
        common.atomic_replace(
            _page_cursor_path(settings, host),
            _json_bytes(
                {"tip": tip or "", "oid": oid, "path": cursor, "skipped": skipped}
            ),
            settings.root,
        )
    except (common.ConfigError, OSError):
        pass


def _clear_page_cursor(settings, host: str) -> None:
    try:
        _unlink_marker(_page_cursor_path(settings, host))
    except (common.ConfigError, OSError):
        pass


def _advanced_cursor(current, cursor, key):
    """Cursors only ever move forward, and only over representable paths."""
    if not _valid_cursor_path(key):
        return current
    if cursor is not None and key <= cursor:
        return current
    return key


def _message_reservation_exists(settings, raw_path) -> bool:
    """Cheap "already handled" test used as the paging cursor."""
    if raw_path is None:
        return False
    parts = PurePosixPath(raw_path).parts
    if not (
        len(parts) == 4
        and parts[0] == "channels"
        and parts[2] == "messages"
        and parts[3].endswith(".msg")
    ):
        return False
    name, message_id = parts[1], parts[3][:-4]
    try:
        common.validate_room(name, topology=True, label="channel name")
        _validate_channel_id(message_id)
        marker = common.destination(
            settings.root, "bridge", "chan-received", name, message_id
        )
    except common.ConfigError:
        return False
    return marker.exists()


def _peer_records_and_messages(
    settings,
    git,
    oid,
    host,
    cfg,
    stats,
    logger,
    deadline,
    fence_present,
    budget,
    archive=None,
    cursor=None,
    forced=frozenset(),
):
    """Walk one peer's channel tree, at most ``CHANNEL_TREE_MAX`` entries.

    The cap is a per-tick page, not a refusal (SPEC-v2 r5.3). When the
    selected set is larger than one page the walk resumes after
    ``bridge/chan-page/<host>``, skips entries whose reservation marker
    already exists, processes a page of the rest in sorted order, and
    reports how many entries remain so the caller can withhold the host
    from ``completed``, leave its tip where it is, and persist the new
    cursor. Two kinds of entry are visited on every page whatever the
    cursor says: a ``channel.json``, because messages in a later page need
    their record, and anything in ``forced`` — the additions a peer made
    since the cursor's OID, which are new and therefore not behind it. The
    caller clears the cursor and completes the host when nothing remains.
    """
    records = {}
    messages = {}
    entries = git.ls_tree(oid, "channels/")
    selected = entries
    if archive is not None and not archive.full_walk:
        selected = [
            entry
            for entry in entries
            if entry.get("path") in archive.additions
            or (entry.get("path") or "").endswith("/channel.json")
        ]
    tip = archive.tip if archive is not None else None
    paging = len(selected) > CHANNEL_TREE_MAX
    resume = cursor.path if (paging and cursor is not None) else None
    if paging:
        selected = sorted(selected, key=lambda entry: entry.get("path") or "")
    processed = 0
    remaining = 0
    advanced = None
    deferred_forced = 0
    for position, entry in enumerate(selected):
        if position % CHANNEL_DEADLINE_STRIDE == 0:
            deadline.check()
        raw_path = entry.get("path")
        if paging:
            key = raw_path or ""
            new_since_cursor = key in forced
            behind = resume is not None and key <= resume
            if behind and not (new_since_cursor or _is_record_path(raw_path)):
                continue
            if _message_reservation_exists(settings, raw_path):
                advanced = _advanced_cursor(advanced, resume, key)
                continue
            if processed >= CHANNEL_TREE_MAX:
                if new_since_cursor:
                    deferred_forced += 1
                if new_since_cursor or not behind:
                    remaining += 1
                continue
            processed += 1
            advanced = _advanced_cursor(advanced, resume, key)
        if raw_path is None:
            _log_ignored(
                settings,
                logger,
                host,
                entry.get("raw_path", b"").hex(),
                entry.get("object"),
                "invalid_utf8",
                fence_present,
                budget,
            )
            continue
        parts = PurePosixPath(raw_path).parts
        if len(parts) == 3 and parts[0] == "channels" and parts[2] == "channel.json":
            name = parts[1]
            try:
                common.validate_room(name, topology=True, label="channel name")
            except common.ConfigError as error:
                _log_ignored(
                    settings, logger, host, raw_path, entry.get("object"), str(error),
                    fence_present, budget
                )
                continue
            if channel_allowed(cfg, name):
                records[name] = entry
            continue
        if (
            len(parts) == 4
            and parts[0] == "channels"
            and parts[2] == "messages"
            and parts[3].endswith(".msg")
        ):
            name = parts[1]
            message_id = parts[3][:-4]
            try:
                common.validate_room(name, topology=True, label="channel name")
                _validate_channel_id(message_id)
            except common.ConfigError as error:
                _log_ignored(
                    settings, logger, host, raw_path, entry.get("object"), str(error),
                    fence_present, budget
                )
                continue
            if not channel_allowed(cfg, name):
                continue
            if entry.get("mode") != "100644" or entry.get("type") != "blob":
                _log_quarantine(
                    settings,
                    stats,
                    logger,
                    host,
                    name,
                    message_id,
                    "non-regular-object",
                    raw_path,
                    entry.get("object"),
                    fence_present,
                    budget,
                )
                continue
            size = entry.get("size")
            if size is None or size > settings.max_mail_bytes:
                _log_quarantine(
                    settings, stats, logger, host, name, message_id, "oversize-unread", raw_path,
                    entry.get("object"), fence_present, budget
                )
                continue
            messages.setdefault(name, []).append((message_id, entry))
            continue
        _log_ignored(
            settings, logger, host, raw_path, entry.get("object"), "invalid_path",
            fence_present, budget
        )
    # Holding the old OID keeps the same delta in play next tick, so a
    # forced addition squeezed out by the page cap is still exempt then.
    walked = cursor.oid if (cursor is not None and deferred_forced) else oid
    return records, messages, PageState(
        processed,
        remaining,
        advanced if advanced is not None else resume,
        tip,
        walked,
        resume is not None,
    )


def _read_peer_record(
    settings, git, oid, host, name, entry, logger, fence_present, budget
):
    blob_oid = entry.get("object")
    if (
        entry.get("mode") != "100644"
        or entry.get("type") != "blob"
        or entry.get("size") is None
        or entry["size"] > CHANNEL_RECORD_MAX
    ):
        reason = "invalid mode or size"
    else:
        data = git.show(oid, entry["path"])
        if data is None or len(data) != entry["size"]:
            reason = "unreadable object"
        else:
            try:
                return _parse_record(data, name, f"{host}:{entry['path']}")
            except common.ConfigError as error:
                reason = str(error)
    if _seen_once(
        settings,
        "record-invalid",
        host,
        entry.get("path"),
        blob_oid,
        fence_present,
        budget,
    ):
        logger.emit(
            "chan_record_invalid", host=host, channel=name, oid=blob_oid, reason=reason
        )
    return None


def _outstanding_joins(settings) -> Set[tuple]:
    outstanding = set()
    for kind in ("chan-joins-pending", "chan-joins-held"):
        try:
            root = common.destination(settings.root, "bridge", kind)
            channel_dirs = list(os.scandir(str(root)))
        except (common.ConfigError, OSError):
            continue
        for channel_dir in channel_dirs:
            if not channel_dir.is_dir(follow_symlinks=False):
                continue
            try:
                markers = list(os.scandir(channel_dir.path))
            except OSError:
                continue
            for marker in markers:
                if marker.is_file(follow_symlinks=False):
                    outstanding.add((channel_dir.name, marker.name))
    return outstanding


def _replay_outstanding_joins(
    settings, cfg, snapshot, logger, deadline, fence_present
) -> None:
    """C7 replay from local markers, with no git and no host attribution.

    A held join is retried every full tick and a pending one every tick; the
    delta import will not show the message again once the tip has advanced
    past it, so the retry reads the already-imported local message instead.
    """
    outstanding = _outstanding_joins(settings)
    if not outstanding:
        return
    unknown_seen = set()
    skipped_channels = set()
    skipped_messages = set()
    try:
        with _channel_lock(settings, fence_present):
            for name, message_id in sorted(outstanding):
                deadline.check()
                try:
                    common.validate_room(name, topology=True, label="channel name")
                    _validate_channel_id(message_id)
                    if not channel_allowed(cfg, name):
                        continue
                    if not _local_members_readable(
                        settings, name, logger, skipped_channels
                    ):
                        continue
                    data = _read_optional(
                        common.destination(
                            settings.root,
                            "channels",
                            name,
                            "messages",
                            message_id + ".msg",
                        ),
                        settings.max_mail_bytes,
                    )
                    if data is None:
                        continue
                    envelope = _parse_envelope(
                        data, name, message_id, logger, unknown_seen
                    )
                    _membership_locked(
                        settings, snapshot, name, message_id, envelope, fence_present
                    )
                except (common.ConfigError, OSError) as error:
                    key = (name, message_id)
                    if key not in skipped_messages:
                        skipped_messages.add(key)
                        logger.emit(
                            "chan_local_unreadable",
                            channel=name,
                            id=message_id,
                            reason=str(error),
                        )
    except (common.ConfigError, OSError) as error:
        # Only the lock itself can land here: every per-marker failure is
        # already handled inside the loop.
        logger.emit("chan_local_unreadable", channel="", id="", reason=str(error))


def import_channels(
    settings, cfg, git, snapshot, logger, deadline, fence_present
) -> ChannelStats:
    """Import and reconcile peer channel archives at snapshot-pinned OIDs.

    The first imported valid record fixes ``created_by`` and therefore the
    channel origin on this node. This function performs the archive check and
    returns completed hosts in ``ChannelStats.completed``; the tick advances
    exactly those hosts with :func:`advance_tip`.

    A snapshot-dependent quarantine (``unpublished_sender``,
    ``name_collision``) is final rather than retried: a peer's tick publishes
    ``rooms.json`` and its messages in one commit, so a sender is either
    published or not when its message arrives, and a contest is resolved by
    humans and the message re-posted.

    A host is completed only when nothing recoverable was skipped for it this
    tick. Every ``continue`` that abandons a channel or a message for a reason
    that can resolve later — ``chan_no_record``, ``chan_record_invalid``,
    ``chan_local_unreadable``, divergence, a reservation held by another host,
    a channel that could not be created locally — clears the host. Terminal
    quarantines are complete: the forensic copy and the ``chan-seen`` marker
    are the receipt, and re-listing the message would change nothing.
    """
    stats = ChannelStats()
    if cfg is None:
        return stats
    unknown_seen = set()
    local_unreadable = set()
    unreadable_channels = set()
    seen_budget = {}
    holds = common.HoldLedger(settings.root, "chan-held")

    def hold_channel(host, name, rows):
        for message_id, _ in rows:
            holds.hold(host, name, message_id)
    completed = set()
    emitted = set()
    for host in sorted(snapshot.peers):
        deadline.check()
        oid = snapshot.oids.get(f"machines/{host}")
        if oid is None:
            logger.emit("peer_branch_missing", host=host)
            continue
        _fence(fence_present)
        archive = check_archive(settings, git, host, oid, logger, emitted)
        if not archive:
            if (common.destination(settings.root, "bridge", "chan-rewritten", host)).exists():
                stats.rewritten.append(host)
            continue
        cursor = _read_page_cursor(settings, host, archive.tip, logger)
        forced = frozenset()
        if cursor is not None and cursor.oid != oid:
            # The walk is resuming into a newer tree. Everything the peer
            # added since the cursor's OID is new, so it is not behind the
            # cursor however its path sorts — and the same archive rule
            # applies to that span as to a chan-tip delta.
            resumed = archive_delta(
                settings, git, host, cursor.oid, oid, logger, emitted
            )
            if not resumed:
                if (common.destination(settings.root, "bridge", "chan-rewritten", host)).exists():
                    stats.rewritten.append(host)
                continue
            forced = resumed.additions or frozenset()
        try:
            listing = _peer_records_and_messages(
                settings,
                git,
                oid,
                host,
                cfg,
                stats,
                logger,
                deadline,
                fence_present,
                seen_budget,
                archive,
                cursor,
                forced,
            )
        except common.GitReadError as error:
            # m6: the host is not completed, so its tip stays put.
            common.note_git_failure(git, logger, host, error)
            continue
        records, messages, page = listing
        skipped = False
        for name in sorted(set(records) | set(messages)):
            record_entry = records.get(name)
            if record_entry is None:
                logger.emit("chan_no_record", host=host, channel=name)
                hold_channel(host, name, messages.get(name, ()))
                skipped = True
                continue
            peer_record = _read_peer_record(
                settings,
                git,
                oid,
                host,
                name,
                record_entry,
                logger,
                fence_present,
                seen_budget,
            )
            if peer_record is None:
                logger.emit(
                    "chan_no_record", host=host, channel=name, reason="invalid"
                )
                hold_channel(host, name, messages.get(name, ()))
                skipped = True
                continue
            if not _local_members_readable(
                settings, name, logger, unreadable_channels
            ):
                # M1: the channel's messages were not imported; keep the tip
                # so the bounded delta lists them again after repair.
                hold_channel(host, name, messages.get(name, ()))
                skipped = True
                continue
            if not messages.get(name):
                try:
                    with _channel_lock(settings, fence_present):
                        _ensure_channel_locked(
                            settings,
                            name,
                            peer_record,
                            snapshot,
                            logger,
                            fence_present,
                        )
                        _adopt_description_locked(
                            settings,
                            snapshot,
                            host,
                            name,
                            peer_record,
                            record_entry["object"],
                            logger,
                            fence_present,
                        )
                except (common.ConfigError, OSError) as error:
                    logger.emit(
                        "chan_local_unreadable",
                        channel=name,
                        id="",
                        reason=str(error),
                    )
                    skipped = True
                continue
            for message_id, entry in sorted(messages[name], key=lambda row: row[0]):
                deadline.check()
                data = git.show(oid, entry["path"])
                if (
                    data is None
                    or len(data) != entry["size"]
                    or len(data) > settings.max_mail_bytes
                ):
                    _log_quarantine(
                        settings, stats, logger, host, name, message_id, "unreadable-object",
                        entry["path"], entry.get("object"), fence_present, seen_budget
                    )
                    continue
                sha256 = hashlib.sha256(data).hexdigest()
                try:
                    envelope = _parse_envelope(
                        data, name, message_id, logger, unknown_seen
                    )
                except common.ConfigError as error:
                    _forensic(
                        settings,
                        host,
                        name,
                        message_id + ".msg",
                        data,
                        fence_present,
                    )
                    _log_quarantine(
                        settings, stats, logger, host, name, message_id, str(error),
                        entry["path"], entry.get("object"), fence_present, seen_budget
                    )
                    continue
                sender = envelope["from"]
                roomless = _roomless_sender(envelope) and "from_host" in envelope
                if roomless:
                    try:
                        pmail.validate_participant(sender, "roomless sender")
                        if "@" in sender or envelope.get("from_host") != host:
                            raise common.ConfigError("roomless_origin_invalid")
                        if any(
                            common.fold_name(name) == common.fold_name(sender)
                            for name in snapshot.published.get(host, frozenset())
                            | snapshot.pins.get(host, frozenset())
                        ):
                            raise common.ConfigError("roomless_sender_is_room")
                        if any(
                            common.fold_name(name) == common.fold_name(sender)
                            for name in snapshot.real_rooms.keys() | snapshot.placeholders.keys()
                        ):
                            raise common.ConfigError("roomless_sender_room_collision")
                        local_id = common.destination(
                            settings.root, "participants", sender
                        )
                        if local_id.exists() or local_id.is_symlink():
                            raise common.ConfigError("participant_id_collision")
                    except common.ConfigError as error:
                        _forensic(
                            settings, host, name, message_id + ".msg", data, fence_present
                        )
                        _log_quarantine(
                            settings, stats, logger, host, name, message_id,
                            str(error), entry["path"], entry.get("object"),
                            fence_present, seen_budget,
                        )
                        continue
                elif "from_host" in envelope:
                    _forensic(
                        settings, host, name, message_id + ".msg", data, fence_present
                    )
                    _log_quarantine(
                        settings, stats, logger, host, name, message_id,
                        "unexpected_from_host", entry["path"], entry.get("object"),
                        fence_present, seen_budget,
                    )
                    continue
                verdict = VERIFIED if roomless else binding_verdict(snapshot, host, sender)
                if verdict in (FORGED_SELF, NAME_COLLISION, UNPUBLISHED_SENDER):
                    _forensic(
                        settings,
                        host,
                        name,
                        message_id + ".msg",
                        data,
                        fence_present,
                    )
                    _log_quarantine(
                        settings, stats, logger, host, name, message_id, verdict,
                        entry["path"], entry.get("object"), fence_present, seen_budget
                    )
                    continue
                # r5.5 (M2): the new checks run only for a first import. A
                # message already imported with these bytes keeps its outcome.
                try:
                    already = (
                        _read_optional(
                            common.destination(
                                settings.root,
                                "channels",
                                name,
                                "messages",
                                message_id + ".msg",
                            ),
                            settings.max_mail_bytes,
                        )
                        == data
                    )
                except (common.ConfigError, OSError):
                    already = False
                if (
                    not already
                    and verdict == VERIFIED
                    and not roomless
                    and not common.post_sees_remote(
                        settings.root, host, sender, snapshot.post_rooms
                    )
                ):
                    # Held, never marked seen; the tip stays so the bounded
                    # delta lists it again next full tick.
                    holds.hold(host, name, message_id)
                    logger.emit(
                        "chan_held",
                        host=host,
                        channel=name,
                        id=message_id,
                        sender=sender,
                        reason=common.SENDER_NOT_HOMED,
                    )
                    skipped = True
                    continue
                if (
                    not already
                    and verdict == UNHOMED
                    and envelope.get("from_participant") is not None
                ):
                    _forensic(
                        settings,
                        host,
                        name,
                        message_id + ".msg",
                        data,
                        fence_present,
                    )
                    _log_quarantine(
                        settings, stats, logger, host, name, message_id,
                        common.REMOTE_PARTICIPANT_UNHOMED,
                        entry["path"], entry.get("object"), fence_present, seen_budget
                    )
                    continue
                if verdict == UNHOMED:
                    logger.emit(
                        "from-unhomed",
                        host=host,
                        channel=name,
                        id=message_id,
                        sender=sender,
                    )

                try:
                    message_path = common.destination(
                        settings.root,
                        "channels",
                        name,
                        "messages",
                        message_id + ".msg",
                    )
                    reservation = _read_reservation(settings, name, message_id)
                    existing = _read_optional(message_path, settings.max_mail_bytes)
                except (common.ConfigError, OSError) as error:
                    key = (name, message_id)
                    if key not in local_unreadable:
                        logger.emit(
                            "chan_local_unreadable",
                            channel=name,
                            id=message_id,
                            reason=str(error),
                        )
                        local_unreadable.add(key)
                    skipped = True
                    continue
                expected_reservation = f"{host} {sha256}"
                reserved_host, reserved_sha = _reservation_parts(reservation)
                same_relay_message = reserved_sha == sha256
                same_shim_copy = (
                    existing is not None
                    and _same_unstamped_roomless_copy(
                        existing, data, envelope, name, message_id, logger, unknown_seen,
                        reservation, expected_reservation,
                    )
                )
                if (
                    reservation is not None
                    and reservation != expected_reservation
                    and not same_relay_message
                ):
                    other_host = reserved_host or LOCAL
                    _record_divergence(
                        settings,
                        stats,
                        logger,
                        host,
                        name,
                        message_id,
                        data,
                        other_host,
                        fence_present,
                    )
                    skipped = True
                    continue
                if (
                    reservation is not None
                    and reservation != expected_reservation
                    and same_relay_message
                    and existing is None
                ):
                    # An identical second-host replay must not steal a
                    # reservation whose winning host crashed before C5(c).
                    skipped = True
                    continue
                if existing is not None and existing != data and not same_shim_copy:
                    other_host = reservation.split(" ", 1)[0] if reservation else LOCAL
                    _record_divergence(
                        settings,
                        stats,
                        logger,
                        host,
                        name,
                        message_id,
                        data,
                        other_host,
                        fence_present,
                    )
                    skipped = True
                    continue

                try:
                    with _channel_lock(settings, fence_present):
                        local_record = _ensure_channel_locked(
                            settings,
                            name,
                            peer_record,
                            snapshot,
                            logger,
                            fence_present,
                        )
                        if local_record is None:
                            skipped = True
                            continue
                        existing = _read_optional(message_path, settings.max_mail_bytes)
                        current_reservation = _read_reservation(settings, name, message_id)
                        same_shim_copy = (
                            existing is not None
                            and _same_unstamped_roomless_copy(
                                existing, data, envelope, name, message_id, logger, unknown_seen,
                                current_reservation, expected_reservation,
                            )
                        )
                        if existing is not None and existing != data and not same_shim_copy:
                            _record_divergence(
                                settings,
                                stats,
                                logger,
                                host,
                                name,
                                message_id,
                                data,
                                reservation.split(" ", 1)[0] if reservation else LOCAL,
                                fence_present,
                            )
                            skipped = True
                            continue
                        if same_shim_copy and current_reservation is None:
                            _fence(fence_present)
                            reservation_path = common.destination(
                                settings.root, "bridge", "chan-received", name, message_id
                            )
                            _publish_once(
                                reservation_path,
                                _marker_bytes(expected_reservation),
                                common.destination(settings.root, "bridge", "tmp"),
                                settings.root,
                            )
                            if _read_reservation(settings, name, message_id) != expected_reservation:
                                _record_divergence(
                                    settings, stats, logger, host, name, message_id,
                                    data, LOCAL, fence_present,
                                )
                                skipped = True
                                continue
                        if existing is None:
                            created = _publish_message_batch(
                                settings,
                                host,
                                name,
                                message_id,
                                data,
                                sha256,
                                envelope,
                                fence_present,
                            )
                            if (
                                not created
                                and _read_reservation(settings, name, message_id)
                                != expected_reservation
                            ):
                                current = _read_reservation(settings, name, message_id)
                                other_host = (
                                    current.split(" ", 1)[0] if current else LOCAL
                                )
                                _record_divergence(
                                    settings,
                                    stats,
                                    logger,
                                    host,
                                    name,
                                    message_id,
                                    data,
                                    other_host,
                                    fence_present,
                                )
                                skipped = True
                                continue
                            if not created:
                                appeared = common.open_regular(
                                    message_path, settings.max_mail_bytes
                                )
                                if appeared != data:
                                    reserved_host, _ = _reservation_parts(
                                        _read_reservation(settings, name, message_id)
                                    )
                                    _record_divergence(
                                        settings,
                                        stats,
                                        logger,
                                        host,
                                        name,
                                        message_id,
                                        data,
                                        reserved_host or LOCAL,
                                        fence_present,
                                    )
                                    skipped = True
                                    continue
                            if created:
                                stats.imported += 1
                                logger.emit(
                                    "chan_imported", host=host, channel=name, id=message_id
                                )
                        _membership_locked(
                            settings,
                            snapshot,
                            name,
                            message_id,
                            envelope,
                            fence_present,
                        )
                        _adopt_description_locked(
                            settings,
                            snapshot,
                            host,
                            name,
                            peer_record,
                            record_entry["object"],
                            logger,
                            fence_present,
                        )
                except (common.ConfigError, OSError) as error:
                    key = (name, message_id)
                    if key not in local_unreadable:
                        logger.emit(
                            "chan_local_unreadable",
                            channel=name,
                            id=message_id,
                            reason=str(error),
                        )
                        local_unreadable.add(key)
                    skipped = True
                    continue
                # Imported (or already present with these bytes): any stamp
                # an earlier page or tick left for it is done.
                holds.release(host, name, message_id)
        # r5.6 (P1): a skip on any page of a paged walk holds the whole walk.
        # The cursor steps past held entries, so the bit rides with it and
        # the last page completes the host only when no page skipped; the
        # tip then stays and the next walk lists the held entries again.
        walk_skipped = skipped or (
            page.resumed and cursor is not None and cursor.skipped
        )
        if page.remaining:
            # Persisted only now: everything this page reached has been
            # attempted, so a deadline or fence mid-host replays the page
            # instead of stepping over it.
            logger.emit(
                "chan_tree_paged",
                host=host,
                processed=page.processed,
                remaining=page.remaining,
            )
            if page.cursor is not None:
                _write_page_cursor(
                    settings, host, page.tip, page.oid, page.cursor, walk_skipped
                )
        else:
            _clear_page_cursor(settings, host)
            if not walk_skipped:
                completed.add(host)
                holds.settle(host)
            elif not page.resumed:
                # Every selected entry was looked at this tick, so this
                # tick's holds are the host's whole held set. A resumed last
                # page saw only part of it and keeps every stamp.
                holds.settle(host)
    _replay_outstanding_joins(
        settings, cfg, snapshot, logger, deadline, fence_present
    )
    stats.completed = frozenset(completed)
    stats.held = holds.summary(snapshot.peers, prune=snapshot.peers_known)
    return stats


def _unpublishable(stats, logger, name, message_id, reason):
    stats.unpublishable += 1
    stats.count_reason(reason)
    fields = {"channel": name, "reason": reason}
    if message_id is not None:
        fields["id"] = message_id
    logger.emit("channel_unpublishable", **fields)


def _local_failure_reason(error: Exception) -> str:
    text = str(error)
    if "exceeds" in text:
        return "oversize"
    if "regular file" in text or "symlink" in text:
        return "non_regular"
    return text


def publish_channels(
    settings, cfg, snapshot, logger, deadline, tracked=None
) -> ChannelStats:
    """Publish validated locally-authored channel messages to the worktree.

    Only ids this host has not settled are opened (bridgelib/decided.py):
    ``tracked`` is the set of relay paths already committed on this host's
    branch (``channels/<name>/messages/<id>.msg``), and a message in it is
    published for good, so it is skipped unopened. The messages that stay
    local for good (imported from a peer, or authored under a name that is
    not a local room) leave a ``bridge/chan-decided/<name>/<id>`` marker
    after their first judgement and are re-judged only after the recheck
    window. Byte-comparison against the worktree copy therefore guards the
    one tick between copying a message and committing it; committed history
    is immutable in Git itself.
    """
    stats = ChannelStats()
    if cfg is None:
        return stats
    recheck = decided.recheck_seconds(settings)
    now = time.time()
    channels_root = common.destination(settings.root, "channels")
    try:
        local_channels = sorted(
            os.scandir(str(channels_root)), key=lambda item: item.name
        )
    except FileNotFoundError:
        return stats
    unknown_seen = set()
    for channel_entry in local_channels:
        name = channel_entry.name
        if name == ".channels.lock":
            continue
        try:
            common.validate_room(name, topology=True, label="channel name")
        except common.ConfigError as error:
            _unpublishable(stats, logger, name, None, str(error))
            continue
        if not channel_allowed(cfg, name):
            continue
        if not channel_entry.is_dir(follow_symlinks=False):
            _unpublishable(stats, logger, name, None, "non_regular_channel")
            continue
        record_path = Path(channel_entry.path) / "channel.json"
        try:
            record = _parse_record(
                common.open_regular(record_path, CHANNEL_RECORD_MAX),
                name,
                str(record_path),
            )
        except (FileNotFoundError, common.ConfigError, OSError) as error:
            _unpublishable(stats, logger, name, None, _local_failure_reason(error))
            continue

        destination_messages = common.destination(
            settings.repo, "channels", name, "messages"
        )
        common.ensure_dir(settings.repo, destination_messages)
        # One worktree listing per channel.  Individual byte comparisons open
        # only candidates present in this snapshot of the directory.
        try:
            worktree_names = {
                entry.name for entry in os.scandir(str(destination_messages))
            }
        except FileNotFoundError:
            worktree_names = set()
        local_messages = Path(channel_entry.path) / "messages"
        settled = decided.names_with_mtime(
            common.destination(settings.root, "bridge", "chan-decided", name)
        )
        try:
            metadata = local_messages.lstat()
            if not stat.S_ISDIR(metadata.st_mode) or local_messages.is_symlink():
                raise common.ConfigError("messages is not a real directory")
            candidates = sorted(
                os.scandir(str(local_messages)), key=lambda item: item.name
            )
        except FileNotFoundError:
            candidates = []
        except (common.ConfigError, OSError) as error:
            _unpublishable(stats, logger, name, None, _local_failure_reason(error))
            continue
        for entry in candidates:
            deadline.check()
            if not entry.name.endswith(".msg"):
                continue
            message_id = entry.name[:-4]
            if tracked is not None and (
                f"channels/{name}/messages/{entry.name}" in tracked
            ):
                continue
            stamp = settled.get(message_id)
            if stamp is not None and recheck > 0 and 0 <= now - stamp < recheck:
                continue
            try:
                _validate_channel_id(message_id)
                data = common.open_regular(Path(entry.path), settings.max_mail_bytes)
                envelope = _parse_envelope(data, name, message_id, logger, unknown_seen)
            except (common.ConfigError, OSError) as error:
                _unpublishable(
                    stats, logger, name, message_id, _local_failure_reason(error)
                )
                continue
            received = common.destination(
                settings.root, "bridge", "chan-received", name, message_id
            )
            if received.exists() or received.is_symlink():
                if envelope["from"] in snapshot.real_rooms:
                    logger.emit("chan_self_conflict", channel=name, id=message_id)
                decided.mark_channel(settings, name, message_id)
                continue
            if envelope["from"] not in snapshot.real_rooms:
                if not _roomless_sender(envelope):
                    decided.mark_channel(settings, name, message_id)
                    continue
                try:
                    pmail.validate_participant(envelope["from"], "roomless sender")
                    if "@" in envelope["from"]:
                        raise common.ConfigError("roomless sender must not contain '@'")
                    participant_path = common.destination(
                        settings.root, "participants", envelope["from"], "participant.json"
                    )
                    participant = common.load_json_bytes(
                        common.open_regular(participant_path, 64 * 1024), str(participant_path)
                    )
                    if (
                        not isinstance(participant, dict)
                        or participant.get("id") != envelope["from"]
                    ):
                        raise common.ConfigError(
                            "roomless sender has no matching local participant record"
                        )
                    data = _stamp_roomless_host(data, envelope, settings.host)
                    if len(data) > settings.max_mail_bytes:
                        raise common.ConfigError("stamped message exceeds bridge mail limit")
                except (common.ConfigError, FileNotFoundError, OSError) as error:
                    _unpublishable(stats, logger, name, message_id, _local_failure_reason(error))
                    continue
            target = common.destination(
                settings.repo, "channels", name, "messages", entry.name
            )
            present = entry.name in worktree_names
            if present:
                try:
                    existing = common.open_regular(target, settings.max_mail_bytes)
                except FileNotFoundError:
                    present = False
                except (common.ConfigError, OSError) as error:
                    raise common.ConfigError(
                        f"channel message immutable {name}/{message_id}: {error}"
                    )
                if present:
                    if existing != data:
                        raise common.ConfigError(
                            f"channel message immutable {name}/{message_id} differs"
                        )
                    continue
            try:
                common.exclusive_publish(target, data, target.parent, settings.repo)
            except FileExistsError:
                if common.open_regular(target, settings.max_mail_bytes) != data:
                    raise common.ConfigError(
                        f"channel message immutable {name}/{message_id} differs"
                    )
                continue
            stats.published += 1
            logger.emit("chan_published", channel=name, id=message_id)

        record_target = common.destination(
            settings.repo, "channels", name, "channel.json"
        )
        payload = _json_bytes(record)
        try:
            existing_record = common.open_regular(record_target, CHANNEL_RECORD_MAX)
        except FileNotFoundError:
            existing_record = None
        if existing_record != payload:
            common.atomic_replace(record_target, payload, settings.repo)
    return stats


def channels_health(stats, settings) -> dict:
    """Return the health.json ``channels`` fragment, including durable faults."""
    diverged = _load_diverged(settings)
    for item in stats.diverged:
        if item not in diverged:
            diverged.append(item)
    diverged.sort(key=lambda item: (item["channel"], item["id"], item["hosts"]))
    rewritten = set(stats.rewritten)
    marker_root = common.destination(settings.root, "bridge", "chan-rewritten")
    try:
        for entry in os.scandir(str(marker_root)):
            if entry.is_file(follow_symlinks=False):
                rewritten.add(entry.name)
    except FileNotFoundError:
        pass
    return {
        "imported": stats.imported,
        "published": stats.published,
        "quarantined": stats.quarantined,
        "unpublishable": stats.unpublishable,
        "diverged": diverged,
        "rewritten": sorted(rewritten),
        "held": dict(stats.held),
    }
