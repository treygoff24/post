"""Local-held export guard (SPEC-v2 r6.1; bead post-aqw.9).

An archive letter that post delivered into a real local room must never be
exported to a peer, whatever later happens to the room table or the mailbox.
A full tick stamps an immutable hold for every workspace letter it can prove
was delivered locally, before outbound selection; a matching hold excludes
the letter from export forever (no release, no GC). A hold that is missing,
corrupt, or no longer matches the archive is a visible fault and the letter
stays held.

Only observed or captured local deliveries are protected. A missing local
copy is never proof of remote intent, and a remote placeholder's canonical
inbox copy (post writes one for every send to ``remote/<host>/<room>``) is
never local-delivery evidence: stamping keys on a real local registration.
"""

import hashlib
import json
import os
import re
import stat
from pathlib import PurePosixPath

from .common import (
    ConfigError,
    atomic_replace,
    destination,
    ensure_dir,
    exclusive_publish,
    fold_name,
    fsync_directory,
    load_json_bytes,
    normalize_name,
    open_regular,
    validate_id,
)
from .pmail import rfc3339_now, valid_rfc3339

NAMESPACE = "local-held"
MANIFESTS = "local-held-manifests"
INDEX = "local-held-index.txt"
FAULTS = "local-held-faults"
# Round 2, finding 5; review 3, finding 7: outlives a `rm -rf
# bridge/local-held*` and a deleted health.json, and carries the floors
# (``{"version":1,"indexed":N|null,"manifests":M}``). Removing it is a
# deliberate full reset of the guard.
SENTINEL = "held-guard-sentinel"
SENTINEL_VERSION = 1
# How many unindexed record ids an index_short fault line names. The fault's
# marker (kind plus the JSON detail) must stay under the 1024 bytes _once
# reads back, or it re-logs every tick: 8 ids measure 293 bytes with 7-digit
# counts, and 38 would cross the limit.
UNINDEXED_IDS_SHOWN = 8
SENTINEL_MAX_BYTES = 4096

RECORD_VERSION = 1
RECORD_KEYS = frozenset(
    {"v", "id", "archive_sha256", "to", "reason", "evidence", "observed_at"}
)
OBSERVED = "observed"
SEEDED = "seeded"
UNKNOWN_INTENT = "unknown_intent"
REASONS = frozenset({OBSERVED, SEEDED, UNKNOWN_INTENT})
RECORD_MAX_BYTES = 4096
EVIDENCE_MAX = 8
MAILBOXES = ("inbox", "read")
MANIFEST_MAX_BYTES = 16 * 1024 * 1024
INDEX_MAX_BYTES = 64 * 1024 * 1024

FAULT_INVALID = "record_invalid"
FAULT_DIGEST = "digest_mismatch"
FAULT_TARGET = "target_mismatch"
FAULT_MISSING = "missing"
# Store-level faults: the guard cannot tell which letters it must hold, so
# while any is set no workspace letter is selected for export.
STORE_MISSING = "store_missing"
INDEX_MISSING = "index_missing"
INDEX_UNREADABLE = "index_unreadable"
MANIFEST_DAMAGED = "manifest_damaged"
INDEX_UNWRITABLE = "index_unwritable"
INDEX_SHORT = "index_short"
MANIFEST_MISSING = "manifest_missing"
SENTINEL_DAMAGED = "sentinel_damaged"
FLOOR_UNKNOWN = "floor_unknown"
STORE_FAULTS = frozenset(
    {
        STORE_MISSING,
        INDEX_MISSING,
        INDEX_UNREADABLE,
        MANIFEST_DAMAGED,
        INDEX_UNWRITABLE,
        INDEX_SHORT,
        MANIFEST_MISSING,
        SENTINEL_DAMAGED,
        FLOOR_UNKNOWN,
    }
)

SHA_RE = re.compile(r"[0-9a-f]{64}")


def sha256_hex(data):
    return hashlib.sha256(data).hexdigest()


def record_path(root, mail_id):
    return destination(root, "bridge", NAMESPACE, mail_id + ".json")


def manifest_relpath(manifest_sha256):
    return f"bridge/{MANIFESTS}/{manifest_sha256}.jsonl"


def record_bytes(mail_id, archive_sha256, to, reason, evidence, observed_at):
    value = {
        "v": RECORD_VERSION,
        "id": mail_id,
        "archive_sha256": archive_sha256,
        "to": to,
        "reason": reason,
        "evidence": list(evidence),
        "observed_at": observed_at,
    }
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def _safe_relpath(value):
    if not isinstance(value, str) or not value or len(value) > 1024:
        return False
    path = PurePosixPath(value)
    return not path.is_absolute() and all(
        part not in ("", ".", "..") for part in path.parts
    ) and "\x00" not in value


def parse_record(data, mail_id):
    """The record as a dict when it is a valid v1 hold for ``mail_id``."""
    try:
        value = load_json_bytes(data, "local-held record")
    except (ConfigError, UnicodeDecodeError):
        return None
    if not isinstance(value, dict) or set(value) != RECORD_KEYS:
        return None
    evidence = value["evidence"]
    if not (
        type(value["v"]) is int
        and value["v"] == RECORD_VERSION
        and value["id"] == mail_id
        and isinstance(value["archive_sha256"], str)
        and SHA_RE.fullmatch(value["archive_sha256"]) is not None
        and isinstance(value["to"], str)
        and value["to"]
        and value["reason"] in REASONS
        and isinstance(evidence, list)
        and 1 <= len(evidence) <= EVIDENCE_MAX
        and all(_safe_relpath(item) for item in evidence)
        and isinstance(value["observed_at"], str)
        and valid_rfc3339(value["observed_at"])
    ):
        return None
    return value


def read_record(root, mail_id):
    """``(present, record)``: record is None when present but invalid."""
    try:
        data = open_regular(record_path(root, mail_id), RECORD_MAX_BYTES)
    except FileNotFoundError:
        return False, None
    except (ConfigError, OSError):
        return True, None
    return True, parse_record(data, mail_id)


def record_matches(record, archive_sha256, to):
    """None when the hold binds these bytes and target, else the fault kind."""
    if record is None:
        return FAULT_INVALID
    if record["archive_sha256"] != archive_sha256:
        return FAULT_DIGEST
    if record["to"] != to:
        return FAULT_TARGET
    return None


def publish_record(
    root, mail_id, archive_sha256, to, reason, evidence, observed_at=None, create_index=True
):
    """Create the hold record exclusively; FileExistsError when one exists.

    The index is created first, so no crash leaves a record without an index
    (that shape is the ``index_missing`` fault). A seed recovering a missing
    index passes ``create_index=False``: it writes the index only after every
    row and the floor pass."""
    if create_index:
        ensure_index(root)
    data = record_bytes(
        mail_id, archive_sha256, to, reason, evidence, observed_at or rfc3339_now()
    )
    exclusive_publish(
        record_path(root, mail_id), data, destination(root, "bridge", "tmp"), root
    )


def stamp(
    root, mail_id, archive_sha256, to, reason, evidence, observed_at=None, defer_index=False
):
    """Create the hold and index it; FileExistsError when one already exists.

    ``defer_index`` writes the record only; the seed's closing rebuild
    indexes it."""
    publish_record(
        root, mail_id, archive_sha256, to, reason, evidence, observed_at,
        create_index=not defer_index,
    )
    if not defer_index:
        append_index(root, mail_id)


def mailbox_copies(root, room, mail_id, data):
    """Root-relative paths of ``<room>/{inbox,read}/<id>.mail`` equal to ``data``."""
    found = []
    for box in MAILBOXES:
        try:
            path = destination(root, room, box, mail_id + ".mail")
            copy = open_regular(path, len(data))
        except (FileNotFoundError, ConfigError, OSError):
            continue
        if copy == data:
            found.append(f"{room}/{box}/{mail_id}.mail")
    return found


def real_room(real_rooms, to):
    """The real local registration ``to`` names exactly, or None."""
    try:
        name = normalize_name(to)
    except (ConfigError, TypeError, ValueError):
        return None
    return name if name in real_rooms else None


# -- expectations -------------------------------------------------------------


def read_sentinel(root):
    """``(state, value, problem)``: state ``absent``, ``ok`` or ``damaged``.

    ``value`` is ``{"indexed": int or None, "manifests": int}`` when ok;
    ``problem`` names the damage: ``not_regular``, ``oversize``,
    ``unreadable`` or ``unparseable``.
    """
    try:
        path = destination(root, "bridge", SENTINEL)
        metadata = os.lstat(str(path))
    except FileNotFoundError:
        return "absent", None, None
    except (ConfigError, OSError):
        return "damaged", None, "unreadable"
    if not stat.S_ISREG(metadata.st_mode):
        return "damaged", None, "not_regular"
    if metadata.st_size > SENTINEL_MAX_BYTES:
        return "damaged", None, "oversize"
    try:
        data = open_regular(path, SENTINEL_MAX_BYTES)
    except FileNotFoundError:
        return "absent", None, None
    except (ConfigError, OSError):
        return "damaged", None, "unreadable"
    try:
        value = load_json_bytes(data, "held-guard sentinel")
    except (ConfigError, UnicodeDecodeError):
        value = None
    if not isinstance(value, dict) or set(value) != {"version", "indexed", "manifests"}:
        return "damaged", None, "unparseable"
    indexed, manifests = value["indexed"], value["manifests"]
    if (
        type(value["version"]) is not int
        or value["version"] != SENTINEL_VERSION
        or not (indexed is None or _count(indexed))
        or not _count(manifests)
    ):
        return "damaged", None, "unparseable"
    return "ok", {"indexed": indexed, "manifests": manifests}, None


def _count(value):
    # Review 4, finding 4: ``type(...) is int`` rejects ``true`` everywhere.
    return type(value) is int and value >= 0


def write_sentinel(root, indexed, manifests):
    """Replace the sentinel atomically (fsync, rename, fsync the directory)."""
    data = json.dumps(
        {"version": SENTINEL_VERSION, "indexed": indexed, "manifests": manifests},
        sort_keys=True,
        separators=(",", ":"),
    )
    atomic_replace(
        destination(root, "bridge", SENTINEL), (data + "\n").encode("ascii"), root
    )


def held_evidence(root, prior):
    """Whether holds are known to have existed, apart from the sentinel:
    the previous health counted holds or floors or carries a store fault,
    or a record or manifest copy is present."""
    prior = bounded_health(prior)
    return (
        prior["holds"] > 0
        or prior["indexed"] > 0
        or prior["manifests"] > 0
        or store_faulted(prior)
        or _has_entry(root, NAMESPACE, ".json")
        or _has_entry(root, MANIFESTS, ".jsonl")
    )


def carried_floors(root, prior, sentinel):
    """``(indexed, manifests)``: the floors a tick or seed must meet.

    Each is the larger of the sentinel's and the previous health's. The
    ``indexed`` floor is None (unknown) when the sentinel is damaged or null,
    or absent while holds are known to have existed, with one exception:
    the live transition, where a sentinel absent over a readable index with
    no carried store fault takes the index's id count (review 3, finding 7).
    Review 4, finding 2: the exception trusts the index only when it is not
    shorter than the holds the previous health counted; otherwise the index
    may already have lost ids and the floor is unknown.
    """
    prior = bounded_health(prior)
    state, value, _ = sentinel
    manifests = prior["manifests"]
    if state == "damaged":
        return None, manifests
    if state == "ok":
        manifests = max(manifests, value["manifests"])
        if value["indexed"] is None:
            return None, manifests
        return max(prior["indexed"], value["indexed"]), manifests
    try:
        present = os.path.lexists(str(destination(root, "bridge", INDEX)))
    except ConfigError:
        present = True
    ids, damaged = read_index(root)
    if present and not damaged and not store_faulted(prior):
        if prior["holds"] > len(ids):
            return None, manifests
        return max(prior["indexed"], len(ids)), manifests
    if present or held_evidence(root, prior):
        return None, manifests
    return prior["indexed"], manifests


def settle_sentinel(root, sentinel, indexed, manifests, index_present):
    """Raise the sentinel to ``indexed``/``manifests`` (``indexed`` None when
    the floor is unknown); returns the value written, or None when nothing
    changed. It is never lowered, a null stays null, a damaged one is left
    alone, and an absent one is created only once an index exists or the
    floor is unknown (a fresh install has none)."""
    state, value, _ = sentinel
    if state == "damaged":
        return None
    if state == "ok":
        new = {
            "indexed": None
            if value["indexed"] is None or indexed is None
            else max(value["indexed"], indexed),
            "manifests": max(value["manifests"], manifests),
        }
        if new == value:
            return None
    else:
        if indexed is not None and not index_present:
            return None
        new = {"indexed": indexed, "manifests": manifests}
    write_sentinel(root, new["indexed"], new["manifests"])
    return new


def ensure_index(root):
    """Create an empty index when nothing is at its path. An index that
    exists already is left alone. The sentinel is written once the tick's
    selection ends (or by the seed) that counts it."""
    path = destination(root, "bridge", INDEX)
    if os.path.lexists(str(path)):
        return
    ensure_dir(root, path.parent)
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
    try:
        os.close(os.open(str(path), flags, 0o600))
    except FileExistsError:
        return
    fsync_directory(path.parent)


def record_ids(root):
    """Ids of every hold record file present, valid or not; None when the
    record directory exists but cannot be listed (the records are unknown)."""
    try:
        names = os.listdir(str(destination(root, "bridge", NAMESPACE)))
    except FileNotFoundError:
        return set()
    except (ConfigError, OSError):
        return None
    ids = set()
    for name in names:
        if not name.endswith(".json"):
            continue
        try:
            validate_id(name[: -len(".json")])
        except ConfigError:
            continue
        ids.add(name[: -len(".json")])
    return ids


def valid_record_ids(root):
    """Ids of the record files that are valid holds: a regular file that
    parses as a v1 record for the id its filename names. None when the
    record directory cannot be listed. The archive binding is not checked,
    so a hold whose letter was pruned from the archive still counts.

    Review 4, finding 1: only these feed the floor. An empty or garbage
    file named like an id is never appended and never counted; it stays an
    unindexed record file, which keeps a carried store fault standing.
    """
    ids = record_ids(root)
    if ids is None:
        return None
    return {mail_id for mail_id in ids if read_record(root, mail_id)[1] is not None}


def rebuild_index(root):
    """Operator recovery (the seed): create the index and append every
    valid record's id it lacks. Returns the count appended, or None when
    the index is unreadable and was left alone."""
    ids, damaged = read_index(root)
    present = valid_record_ids(root)
    if damaged or present is None:
        return None
    ensure_index(root)
    appended = 0
    for mail_id in sorted(present - ids):
        append_index(root, mail_id)
        appended += 1
    return appended


def append_index(root, mail_id):
    """Record that ``mail_id`` has been held; a later missing record is a fault.

    Append-only, one id per line. Each append is framed by newlines so a
    crash mid-write leaves an unparseable fragment of its own, never glued to
    the next id.
    """
    path = destination(root, "bridge", INDEX)
    ensure_dir(root, path.parent)
    flags = (
        os.O_WRONLY
        | os.O_APPEND
        | os.O_CREAT
        | getattr(os, "O_CLOEXEC", 0)
        | getattr(os, "O_NOFOLLOW", 0)
    )
    descriptor = os.open(str(path), flags, 0o600)
    try:
        payload = ("\n" + mail_id + "\n").encode("ascii")
        written = os.write(descriptor, payload)
        if written != len(payload):
            raise OSError("short write to local-held index")
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def index_writable(root):
    """Whether the index present at its path opens for append. Nothing is
    written; a directory, symlink or read-only file is not writable."""
    flags = os.O_WRONLY | os.O_APPEND | getattr(os, "O_CLOEXEC", 0)
    try:
        path = destination(root, "bridge", INDEX)
        descriptor = os.open(str(path), flags | getattr(os, "O_NOFOLLOW", 0))
    except (ConfigError, OSError):
        return False
    os.close(descriptor)
    return True


def index_problem(root):
    """``index_unreadable``, ``index_unwritable`` or None (fine, or absent)."""
    _, damaged = read_index(root)
    if damaged:
        return INDEX_UNREADABLE
    try:
        present = os.path.lexists(str(destination(root, "bridge", INDEX)))
    except ConfigError:
        return INDEX_UNREADABLE
    if present and not index_writable(root):
        return INDEX_UNWRITABLE
    return None


def index_shortfall(root, ids, prior, floor):
    """``{floor, indexed, unindexed_records, unindexed_ids}`` when a readable
    index is short, else None. ``unindexed_ids`` names the first
    UNINDEXED_IDS_SHOWN of those records, sorted, so an operator can find a
    junk file to remove (review 5, finding 2); null when records are unknown.

    Short means fewer ids than the carried ``floor`` (checked on every full
    tick: the index only grows), or, while a store-level fault is carried,
    a record file present whose id the index lacks. Outside a fault that
    shape is a crash between publish and append, which the tick repairs
    when it next validates the letter. Under one, the tick's carried-fault
    repair (Guard._repair) runs first and appends every valid record's id
    the index lacks, so what remains unindexed is an invalid record file
    (never appended by the repair or the seed's rebuild; the fault stands
    until the operator removes it) or a record whose append failed.
    """
    prior = bounded_health(prior)
    unindexed = 0
    shown = []
    if store_faulted(prior):
        present = record_ids(root)
        # Unlistable records are unknown: the fault stays (review 3, finding 5).
        if present is None:
            unindexed = shown = None
        else:
            unindexed = len(present - ids)
            shown = sorted(present - ids)[:UNINDEXED_IDS_SHOWN]
    if len(ids) >= floor and unindexed == 0:
        return None
    return {
        "floor": floor,
        "indexed": len(ids),
        "unindexed_records": unindexed,
        "unindexed_ids": shown,
    }


def read_index(root):
    try:
        data = open_regular(destination(root, "bridge", INDEX), INDEX_MAX_BYTES)
    except FileNotFoundError:
        return set(), False
    except (ConfigError, OSError):
        return set(), True
    ids = set()
    for line in data.decode("ascii", errors="replace").splitlines():
        try:
            validate_id(line)
        except ConfigError:
            continue
        ids.add(line)
    return ids, False


def manifest_rows(data):
    """Each manifest line as ``(line_number, value_or_None)``."""
    rows = []
    for number, line in enumerate(data.split(b"\n"), start=1):
        if not line.strip():
            continue
        try:
            value = load_json_bytes(line, "manifest row")
        except (ConfigError, UnicodeDecodeError):
            value = None
        rows.append((number, value if isinstance(value, dict) else None))
    return rows


def scan_manifests(root, host):
    """``(ids, damaged, copies)`` over the persisted manifest copies.

    ``ids`` are the ids this host's rows expect; ``damaged`` whether any copy
    is unreadable, misnamed or has a bad row; ``copies`` how many copies
    are intact (bytes hashing to their filename) and name this host in at
    least one row, or None when the directory exists but cannot be listed.
    Review 3, finding 8: only an intact copy counts toward the manifests
    floor, so a misnamed copy cannot stand in for one deleted. Review 4,
    finding 5: a copy that names only other hosts expects nothing here, so
    it cannot stand in for this host's deleted copy either.
    """
    ids = set()
    damaged = False
    copies = 0
    try:
        base = destination(root, "bridge", MANIFESTS)
        names = sorted(os.listdir(str(base)))
    except FileNotFoundError:
        return ids, damaged, copies
    except (ConfigError, OSError):
        return ids, True, None
    for name in names:
        if not name.endswith(".jsonl") or name.startswith("."):
            continue
        try:
            data = open_regular(base / name, MANIFEST_MAX_BYTES)
        except (ConfigError, OSError):
            damaged = True
            continue
        intact = sha256_hex(data) == name[: -len(".jsonl")]
        if not intact:
            # Still hold every id it names: a damaged copy never releases one.
            damaged = True
        names_host = False
        for _, value in manifest_rows(data):
            if value is None:
                damaged = True
                continue
            mail_id = value.get("id")
            if host is None or value.get("host") != host:
                continue
            names_host = True
            if not isinstance(mail_id, str):
                continue
            try:
                validate_id(mail_id)
            except ConfigError:
                continue
            ids.add(mail_id)
        if intact and names_host:
            copies += 1
    return ids, damaged, copies


def manifest_ids(root, host):
    """Ids this host's persisted manifests expect, and whether any copy is bad."""
    ids, damaged, _ = scan_manifests(root, host)
    return ids, damaged


def manifest_count(root, host):
    """How many intact manifest copies naming ``host`` are present; None
    when their directory exists but cannot be listed (``manifest_damaged``
    reports that)."""
    return scan_manifests(root, host)[2]


def _has_entry(root, name, suffix):
    """Whether ``bridge/<name>`` lists a ``*suffix`` entry; an unlistable
    path that exists counts as present (its damage is reported elsewhere)."""
    try:
        names = os.listdir(str(destination(root, "bridge", name)))
    except FileNotFoundError:
        return False
    except (ConfigError, OSError):
        return True
    return any(name.endswith(suffix) and not name.startswith(".") for name in names)


def missing_index_fault(root, prior, floors):
    """``store_missing``, ``index_missing`` or None.

    Every stamp writes the index before its record, so an absent index is a
    fault whenever a hold is known to have existed: a record or a manifest
    copy is present, the carried ``floors`` (``carried_floors``: the
    sentinel's and health's) are above zero or unknown, or the previous
    health (``prior``, its ``local_held``) counted ``holds`` or carries any
    store-level fault (sticky: a faulted tick validates nothing, so its
    ``holds`` is 0, but its floors are carried). It clears only when
    the index exists again. ``store_missing`` names the total wipe (no
    record and no manifest either); ``index_missing`` a partial one, where
    deleted records would otherwise go unnoticed.
    """
    try:
        index = destination(root, "bridge", INDEX)
    except ConfigError:
        return None  # a symlink out of the root: present, and unreadable
    if os.path.lexists(str(index)):
        return None
    prior = bounded_health(prior)
    records = _has_entry(root, NAMESPACE, ".json")
    manifests = _has_entry(root, MANIFESTS, ".jsonl")
    sticky = store_faulted(prior)
    indexed_floor, manifests_floor = floors
    known = (
        indexed_floor is None
        or indexed_floor > 0
        or manifests_floor > 0
        or prior["indexed"] > 0
        or prior["holds"] > 0
    )
    if not (sticky or known or records or manifests):
        return None
    return INDEX_MISSING if records or manifests else STORE_MISSING


def store_faulted(local_held):
    """Whether a health ``local_held`` object carries a store-level fault."""
    reasons = bounded_health(local_held)["fault_reasons"]
    return any(kind in STORE_FAULTS for kind in reasons)


def persist_manifest(root, data):
    """Keep an immutable copy of a seeded manifest; returns its relpath.

    An existing copy with other bytes is refused (ConfigError), never replaced.
    """
    digest = sha256_hex(data)
    path = destination(root, "bridge", MANIFESTS, digest + ".jsonl")
    try:
        existing = open_regular(path, MANIFEST_MAX_BYTES)
    except FileNotFoundError:
        existing = None
    if existing is not None:
        if existing != data:
            raise ConfigError(f"persisted manifest {path} differs from its name")
        return manifest_relpath(digest)
    try:
        exclusive_publish(path, data, destination(root, "bridge", "tmp"), root)
    except FileExistsError:
        if open_regular(path, MANIFEST_MAX_BYTES) != data:
            raise ConfigError(f"persisted manifest {path} differs from its name")
    return manifest_relpath(digest)


# -- the per-tick guard ---------------------------------------------------------


class Guard:
    """Decides, per candidate letter, whether it is held from export.

    ``held`` is called by outbound selection for every workspace letter with
    no received, delivered or published marker, after its envelope parsed and
    before routing. It stamps new local deliveries, validates existing holds,
    and counts what health reports. While a store-level fault stands it
    holds every candidate, stamps nothing and appends nothing.
    """

    def __init__(self, settings, real_rooms, contested, logger, prior=None):
        self.root = settings.root
        self.prior = bounded_health(prior)
        # A settings stand-in without a host matches no manifest row.
        self.host = getattr(settings, "host", None)
        self.real_rooms = real_rooms or {}
        # Fold-keyed names that are contested now; None when unknown.
        self.contested = contested
        self.logger = logger
        self.index, index_damaged = read_index(self.root)
        self.index_damaged = index_damaged
        # A damaged or unwritable index is never appended to: the fault is
        # already counted, and an append would repeat every tick.
        self.index_frozen = index_damaged
        manifested, manifests_damaged, self.manifests = scan_manifests(
            self.root, self.host
        )
        self.expected = self.index | manifested
        self.holds = 0
        # Whether the last held() call returned True because a record
        # verified (not because of a fault): the only hold selection may
        # remember as decided (bridgelib/decided.py).
        self.last_hold_valid = False
        # Ids of valid holds this tick; the floor counts them even when their
        # index line could not be appended.
        self.seen = set()
        self.stamped = 0
        self.faults = {}
        self.unaccounted = 0
        # Review 3, finding 7: the floors survive a deleted health.json in
        # the sentinel. An unknown floor (damaged, null, or absent over held
        # evidence) is itself a store fault that only the seed's
        # --accept-lost-records clears.
        self.sentinel = read_sentinel(self.root)
        self.sentinel_error = None
        self.floor, self.manifests_floor = carried_floors(
            self.root, prior, self.sentinel
        )
        if self.sentinel[0] == "damaged":
            self._note_damage(SENTINEL_DAMAGED, problem=self.sentinel[2])
        elif self.floor is None:
            self._note_damage(FLOOR_UNKNOWN)
        missing = missing_index_fault(
            self.root, prior, (self.floor, self.manifests_floor)
        )
        if missing is not None:
            self._note_damage(missing)
        if index_damaged:
            self._note_damage(INDEX_UNREADABLE)
        if manifests_damaged:
            self._note_damage(MANIFEST_DAMAGED)
        # Round 2, finding 4: a manifest copy is never removed by the bridge,
        # so fewer copies than health last counted means some were deleted,
        # and with them the expectation of every row they named.
        if self.manifests is not None and self.manifests < self.manifests_floor:
            self._note_damage(
                MANIFEST_MISSING, carried=self.manifests_floor, present=self.manifests
            )
        if not index_damaged and self._index_exists():
            # Round 2, finding 2: a carried store fault clears only over an
            # index that can be appended to, meets the floor, and names
            # every record present. Review 3, finding 4: the tick first
            # appends the lines of records present but unindexed. That only
            # adds protection, and while faulted no tick stamps, so a lost
            # record still leaves the count short of the floor.
            if store_faulted(self.prior):
                if not index_writable(self.root):
                    self.index_frozen = True
                    self._note_damage(INDEX_UNWRITABLE)
                else:
                    self._repair()
            short = index_shortfall(
                self.root, self.index, self.prior, self.floor or 0
            )
            if short is not None:
                self._note_damage(INDEX_SHORT, **short)

    def _index_exists(self):
        try:
            return os.path.lexists(str(destination(self.root, "bridge", INDEX)))
        except ConfigError:
            return True

    def _note_damage(self, kind, **detail):
        self.faults[kind] = self.faults.get(kind, 0) + 1
        self._once("_" + kind, kind, **detail)

    def store_faults(self):
        """The store-level faults standing now; outbound selection selects
        no workspace letter in a tick that ends with any."""
        return sorted(kind for kind in self.faults if kind in STORE_FAULTS)

    def _is_contested(self, to):
        return self.contested is not None and fold_name(to) in self.contested

    def carry_decided(self, mail_id):
        """Count a hold selection skipped because its decided marker is still
        trusted. The letter is held either way; this keeps ``holds`` and the
        floor's ``seen`` set equal to what a full re-verification would have
        counted. While a store fault stands nothing counts, as in held()."""
        if self.store_faults():
            return
        self.holds += 1
        self.seen.add(mail_id)

    def held(self, mail_id, data, envelope):
        self.last_hold_valid = False
        if self.store_faults():
            # Fail closed: without its store the guard cannot know which
            # letters were delivered locally. Stamp and append nothing.
            return True
        to = envelope["to"]
        contested = self._is_contested(to)
        archive_sha256 = sha256_hex(data)
        present, record = read_record(self.root, mail_id)
        if present:
            fault = record_matches(record, archive_sha256, to)
            if fault is not None:
                return self._fault(mail_id, fault, contested)
            self._valid(mail_id)
            return True
        if mail_id in self.expected:
            return self._fault(mail_id, FAULT_MISSING, contested)
        room = real_room(self.real_rooms, to)
        if room is not None:
            evidence = mailbox_copies(self.root, room, mail_id, data)
            if evidence:
                try:
                    publish_record(
                        self.root, mail_id, archive_sha256, to, OBSERVED, evidence
                    )
                except FileExistsError:
                    return self.held(mail_id, data, envelope)
                self.expected.add(mail_id)
                self.stamped += 1
                self.logger.emit(
                    "local_held", id=mail_id, room=to, reason=OBSERVED, evidence=evidence
                )
                self._valid(mail_id)
                return True
        if contested:
            self.unaccounted += 1
        return False

    def _repair(self):
        present = valid_record_ids(self.root)
        if present is None:
            return  # unknown records: index_short reports it
        appended = 0
        for mail_id in sorted(present - self.index):
            self._append(mail_id)
            if self.index_frozen:
                return
            appended += 1
        if appended:
            self.logger.emit("local_held_index_repaired", appended=appended)

    def _valid(self, mail_id):
        self.last_hold_valid = True
        self.holds += 1
        self.seen.add(mail_id)
        if mail_id not in self.index:
            # A fresh stamp, or a record whose index line never landed.
            self._append(mail_id)
        marker = destination(self.root, "bridge", FAULTS, mail_id)
        if marker.exists():
            try:
                marker.unlink()
                fsync_directory(marker.parent)
            except FileNotFoundError:
                pass
            self.logger.emit("local_held_fault_cleared", id=mail_id)

    def _append(self, mail_id):
        if self.index_frozen:
            return
        try:
            append_index(self.root, mail_id)
        except (ConfigError, OSError):
            # A symlink, directory or read-only file at the index path. The
            # record exists, so this letter stays held; the store fault
            # holds every later candidate this tick.
            self.index_frozen = True
            self._note_damage(INDEX_UNWRITABLE)
            return
        self.index.add(mail_id)

    def _fault(self, mail_id, kind, contested):
        self.faults[kind] = self.faults.get(kind, 0) + 1
        if contested:
            self.unaccounted += 1
        self._once(mail_id, kind)
        return True

    def _once(self, key, kind, **detail):
        """Log a fault when first seen for ``key``, or when its kind or its
        numbers change (review 3, finding 6: an operator restoring records
        one at a time sees each step)."""
        marker = destination(self.root, "bridge", FAULTS, key)
        text = kind
        if detail:
            text += " " + json.dumps(detail, sort_keys=True, separators=(",", ":"))
        try:
            prior = open_regular(marker, 1024).decode("ascii", errors="replace").strip()
        except (FileNotFoundError, ConfigError, OSError):
            prior = None
        if prior == text:
            return
        atomic_replace(marker, (text + "\n").encode("ascii"), self.root)
        if key.startswith("_"):
            self.logger.emit("local_held_fault", fault=kind, **detail)
        else:
            self.logger.emit("local_held_fault", id=key, fault=kind)

    def health(self):
        """This tick's counts. Called once, at the end of the tick; a
        store-level fault that no longer stands has its log marker cleared
        so a recurrence is logged again."""
        for kind in sorted(STORE_FAULTS - set(self.faults)):
            marker = destination(self.root, "bridge", FAULTS, "_" + kind)
            if os.path.lexists(str(marker)):
                try:
                    marker.unlink()
                    fsync_directory(marker.parent)
                except FileNotFoundError:
                    pass
                self.logger.emit("local_held_fault_cleared", fault=kind)
        indexed, manifests = self.settle_floor()
        return {
            "holds": self.holds,
            "indexed": indexed,
            "manifests": manifests,
            "faults": sum(self.faults.values()),
            "fault_reasons": dict(sorted(self.faults.items())),
            "candidates_unaccounted": self.unaccounted,
        }

    def settle_floor(self):
        """Raise the sentinel to this tick's floors and return them as
        ``(indexed, manifests)``. The tick calls it as soon as selection
        (the only stamping) ends, and health() again at the end; the second
        call writes only if a floor rose since. Review 4, finding 3: a tick
        that stamps and then fails before write_health has already raised
        the sentinel."""
        # The floor: the most index ids ever known. Measured only from a
        # readable index, never lowered by a tick (the index only grows), and
        # carried unchanged when this tick could not read it.
        indexed = self.prior["indexed"] if self.floor is None else self.floor
        index_present = not self.index_damaged and self._index_exists()
        if index_present:
            # The ids read at the start plus this tick's appends, and every
            # valid hold seen this tick (one whose append failed included).
            indexed = max(indexed, len(self.index | self.seen))
        # The same high-water rule for manifest copies (finding 4).
        manifests = self.manifests_floor
        if self.manifests is not None:
            manifests = max(manifests, self.manifests)
        # Review 3, finding 7: the sentinel carries both floors past a
        # deleted health.json. Raised before health is written, so a crash
        # between the two leaves it ahead, never behind.
        try:
            written = settle_sentinel(
                self.root,
                self.sentinel,
                None if self.floor is None else indexed,
                manifests,
                index_present,
            )
        except (ConfigError, OSError) as error:
            written = None
            if str(error) != self.sentinel_error:
                self.logger.emit("local_held_sentinel_unwritable", error=str(error))
            self.sentinel_error = str(error)
        if written is not None:
            if self.sentinel[0] == "absent":
                self.logger.emit("local_held_sentinel_created", **written)
            self.sentinel = ("ok", written, None)
        return indexed, manifests


def empty_health():
    return {
        "holds": 0,
        "indexed": 0,
        "manifests": 0,
        "faults": 0,
        "fault_reasons": {},
        "candidates_unaccounted": 0,
    }


def bounded_health(value):
    """A carried ``local_held`` object, reduced to valid counts."""
    if not isinstance(value, dict):
        return empty_health()
    result = empty_health()
    for key in ("holds", "indexed", "manifests", "faults", "candidates_unaccounted"):
        count = value.get(key)
        if _count(count):
            result[key] = count
    reasons = value.get("fault_reasons")
    if isinstance(reasons, dict):
        result["fault_reasons"] = {
            str(kind)[:64]: count
            for kind, count in sorted(reasons.items())[:16]
            if _count(count)
        }
    return result
