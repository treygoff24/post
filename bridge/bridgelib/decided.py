"""Durable "decided" state, so a full tick touches only ids it has not seen.

Before this, every full tick re-opened, parsed and re-judged every archive
letter and every local channel message no matter how old (post-9xf): about
1,000 letters and 8,700 channel files on the Mac, growing forever. The
settled majority is now recognised from directory listings alone.

Already-permanent markers are simply *listed once* instead of stat'ed per
letter: ``bridge/received``, ``bridge/published`` and ``bridge/delivered``
(one ``scandir`` each; delivered is ``<host>/<room>/<id>``).

Two verdicts had no marker, and get one here:

``decided/held/<id>``
    The local-held guard holds this letter and its record verified. A hold is
    permanent, and the marker only ever makes selection *skip* the letter, so
    a stale marker can never let a letter export. What the marker does skip
    is the guard's per-letter re-verification (record tampering), so the
    marker is trusted for ``BRIDGE_DECIDED_RECHECK_SECONDS`` (default 6 h,
    ``0`` re-verifies every tick), measured from its mtime; a re-verified
    letter gets its mtime refreshed. The guard's store-level checks (index
    shortfall, floors, sentinel) still run every tick and still block all
    export on a fault, so deleting any record is caught at once.

``decided/unrelayable/<id>``
    The letter cannot be relayed (malformed, oversize). The marker holds the
    file's size and mtime; while both match, selection does not re-read it,
    and the attention list keeps naming it until the file leaves ``archive/``.

Crash safety. Every marker is written *after* the work it records (the
guard's record and index line for a hold, the ``outbound_ignored`` line for
an unrelayable letter). A crash between the work and the marker leaves no
marker, so the next tick redoes the work, which is idempotent, and the
letter is never skipped undecided. A torn or missing marker means "redo".
The markers are deliberately not fsynced: losing one costs one re-judgement.

Channel messages use the same idea in ``bridge/chan-decided/<channel>/<id>``
(see ``channels.publish_channels``) and, for the messages this host has
already published, the committed relay tree.
"""

import json
import os
import stat
import time

from bridgelib.common import (
    ConfigError,
    destination,
    ensure_dir,
    load_json_bytes,
    open_regular,
)

DEFAULT_RECHECK_SECONDS = 21600
HELD = "held"
UNRELAYABLE = "unrelayable"


def parse_recheck_seconds(raw):
    """Seconds a decided marker is trusted; ``None``/garbage means the default."""
    if raw is None or raw == "":
        return DEFAULT_RECHECK_SECONDS
    try:
        value = int(raw)
    except ValueError:
        return DEFAULT_RECHECK_SECONDS
    return value if value >= 0 else DEFAULT_RECHECK_SECONDS


def recheck_seconds(settings):
    return getattr(settings, "decided_recheck_seconds", DEFAULT_RECHECK_SECONDS)


def names(path):
    """The entry names in one directory; a missing directory is empty."""
    try:
        return {entry.name for entry in os.scandir(str(path))}
    except (FileNotFoundError, NotADirectoryError):
        return set()


def names_with_mtime(path):
    result = {}
    try:
        for entry in os.scandir(str(path)):
            try:
                result[entry.name] = entry.stat(follow_symlinks=False).st_mtime
            except OSError:
                continue
    except (FileNotFoundError, NotADirectoryError):
        pass
    return result


def delivered_ids(root):
    """Every id in ``bridge/delivered/<host>/<room>/``, listed once."""
    found = set()
    base = destination(root, "bridge", "delivered")
    for host in names(base):
        for room in names(base / host):
            found |= names(base / host / room)
    return found


class ArchiveIndex:
    """One tick's view of which archive ids are already decided."""

    def __init__(self, settings):
        root = settings.root
        self.recheck = recheck_seconds(settings)
        self.now = time.time()
        self.received = names(destination(root, "bridge", "received"))
        self.published = names(destination(root, "bridge", "published"))
        self.delivered = delivered_ids(root)
        self.held = names_with_mtime(destination(root, "bridge", "decided", HELD))
        self.unrelayable = names(destination(root, "bridge", "decided", UNRELAYABLE))

    def marked(self, mail_id):
        """Received, delivered or published: settled for good."""
        return (
            mail_id in self.received
            or mail_id in self.delivered
            or mail_id in self.published
        )

    def held_trusted(self, mail_id):
        """A held letter whose marker is still inside its recheck window."""
        stamp = self.held.get(mail_id)
        return (
            stamp is not None
            and self.recheck > 0
            and 0 <= self.now - stamp < self.recheck
        )


def _touch(path, root, payload=b""):
    """Create or refresh a marker: no fsync (a lost marker means one redo)."""
    ensure_dir(root, path.parent)
    flags = os.O_WRONLY | os.O_CREAT | getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(str(path), flags, 0o600)
    try:
        if payload:
            os.ftruncate(descriptor, 0)
            os.write(descriptor, payload)
    finally:
        os.close(descriptor)
    os.utime(str(path), None)


def mark_held(settings, mail_id):
    _touch(destination(settings.root, "bridge", "decided", HELD, mail_id), settings.root)


def mark_channel(settings, channel, message_id):
    """A local channel message this host will not publish (imported from a
    peer, or authored under a name that is not a local room)."""
    try:
        _touch(
            destination(settings.root, "bridge", "chan-decided", channel, message_id),
            settings.root,
        )
    except (ConfigError, OSError):
        pass  # a lost marker costs one re-read next tick


def mark_unrelayable(settings, mail_id, reason, archive_stat):
    payload = json.dumps(
        {
            "reason": reason,
            "size": archive_stat.st_size,
            "mtime_ns": archive_stat.st_mtime_ns,
        },
        sort_keys=True,
    ).encode("utf-8")
    _touch(
        destination(settings.root, "bridge", "decided", UNRELAYABLE, mail_id),
        settings.root,
        payload,
    )


def unrelayable_record(settings, mail_id):
    """``{reason, size, mtime_ns}`` for a decided-unrelayable id, or ``None``
    when the marker is missing or damaged (which means: judge it again)."""
    path = destination(settings.root, "bridge", "decided", UNRELAYABLE, mail_id)
    try:
        value = load_json_bytes(open_regular(path, 4096), str(path))
    except (FileNotFoundError, ConfigError, OSError):
        return None
    if (
        isinstance(value, dict)
        and isinstance(value.get("reason"), str)
        and isinstance(value.get("size"), int)
        and isinstance(value.get("mtime_ns"), int)
    ):
        return value
    return None


def unrelayable_still_true(settings, mail_id, archive_path):
    """The decided-unrelayable letter, when its file is unchanged since the
    verdict; ``None`` when the verdict no longer holds (file changed or gone)."""
    record = unrelayable_record(settings, mail_id)
    if record is None:
        return None
    try:
        metadata = archive_path.lstat()
    except OSError:
        return None
    if not stat.S_ISREG(metadata.st_mode):
        return None
    if metadata.st_size != record["size"] or metadata.st_mtime_ns != record["mtime_ns"]:
        return None
    return record
