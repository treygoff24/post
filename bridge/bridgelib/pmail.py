"""Participant mail across hosts: the bridge half of lane F3.

Design: post repo ``docs/plans/bridge-participant-address-design.md`` rev 3.1
(SPEC-v2 §Participant mail). A letter to ``participant:<id>@<host>`` is an
archive letter with ``address_kind`` ``participant`` and a ``to_host``. It
never enters ``outbox/`` (the typed-letter exclusion in sweep.py); it travels
as ``pmail/<dest-host>/<id>/<mail-id>.mail`` on the sender's relay branch, is
admitted on the destination by ``post bridge deliver`` (post owns admission),
and comes back as ``preceipts/<origin-host>/<id>/<mail-id>.json``.

Destination rules the tests pin:

- A receipt committed at HEAD of this host's relay branch is final: the letter
  is never recomputed, post is never called again for it, and no second
  receipt is written. An uncommitted receipt left by a killed tick is
  discarded at recovery, because nothing was published.
- Only an exit-0 decision in the exact frozen schema, with every echoed field
  equal to what the bridge passed, ends a letter. Everything else is a retry
  (``interpret_deliver``); there is no generic nonzero handler.
- A delivered receipt's ``at`` is post's ``admitted_at``, so a receipt rebuilt
  after a crash is byte-identical.

Sender rules: ``published`` is written only from a pushed commit (the remote
tracking ref, or HEAD right after a successful push); ``acked`` is an
exclusive create and the first valid receipt wins; the pmail entry is pruned
only after ``acked`` exists. Receipts are tombstones and are never pruned.
"""

import datetime as dt
import hashlib
import json
import os
import re
import shutil
import subprocess
import tempfile
import time
from dataclasses import dataclass, field
from pathlib import PurePosixPath
from typing import Optional

from . import common
from .common import (
    ConfigError,
    GitReadError,
    TickError,
    atomic_replace,
    checkpoint,
    destination,
    ensure_dir,
    epoch_from_iso,
    exclusive_publish,
    load_json_bytes,
    note_git_failure,
    open_regular,
    utc_now,
)
from .snapshot import NAME_COLLISION, UNPUBLISHED_SENDER, binding_verdict

# Advertised in health.json. post's capability guard refuses a
# host-qualified participant send unless both are listed and ticked_at is
# fresh (design §Sender side 2, F3-R2-B).
CAPABILITIES = ("participant-mail-v1", "typed-outbound-exclusion", "roomless-channel-v1")
MAX_INTERVAL_SECONDS = 86400  # post's reader refuses a larger interval_s

DELIVER_SCHEMA = "post.bridge-deliver.v1"
DELIVER_OUTCOMES = frozenset({"delivered", "rejected", "retry"})
# Terminal rejection reasons (design §The relay). Each ends the letter.
TERMINAL_REASONS = frozenset(
    {
        "unknown_participant",
        "ended_participant",
        "blocked_route",
        "to_mismatch",
        "forged_from",
        "name_collision",
        "unpublished_sender",
        "id_collision",
        "malformed",
    }
)
# Retryable reasons from the bridge's handling of the post call.
POST_UNAVAILABLE = "post_unavailable"
POST_OUTPUT_MALFORMED = "post_output_malformed"
INVALID_INVOCATION = "invalid_invocation"
# Retryable: the relay entry could not be read this tick (Grok G2).
OBJECT_UNREADABLE = "object_unreadable"
# Retryable: `post participant gc` moved the addressee's record aside to
# <root>/participants-archive/<id>/. `post bridge deliver` restores an archived
# recipient itself; when it still answers unknown_participant for one, that
# answer is not the letter's fate, so the bridge holds the letter instead.
PARTICIPANT_ARCHIVED = "participant_archived"
PARTICIPANTS_DIR = "participants"
PARTICIPANTS_ARCHIVE_DIR = "participants-archive"

# What the destination can know about one relay entry (Grok G2).
ENTRY_LETTER = "letter"  # bytes read and hashed
ENTRY_DIGEST_ONLY = "digest_only"  # content hashed by streaming; never a letter
ENTRY_NO_CONTENT = "no_content"  # gitlink, tree, symlink: nothing to digest
ENTRY_UNREADABLE = "unreadable"  # a read failed; says nothing about the letter
REGULAR_MODES = ("100644", "100755")

PRECEIPT_KEYS = frozenset(
    {"v", "status", "origin", "host", "participant", "id", "sha256", "reason", "at"}
)
PRECEIPT_STATUSES = frozenset({"delivered", "rejected"})
PRECEIPT_MAX_BYTES = 4096
HEADER_MAX_BYTES = 4096
DETAIL_MAX_CHARS = 512
POST_CALL_TIMEOUT_SECONDS = 30
LOG_REPEAT_SECONDS = 3600

# Queued-state reasons the sender records in bridge/pmail-status/.
PEER_NOT_EFFECTIVE = "peer_not_effective"
SENDER_UNPUBLISHED = "sender_unpublished"
OVERSIZE = "oversize"
MALFORMED = "malformed"
RELAY_PUSH_FAILED = "relay_push_failed"
# A pmail-acked record exists but does not validate (GLM m1): the letter is
# neither acknowledged nor free to be pruned until a person looks.
ACKED_INVALID = "acked_invalid"

_RFC3339_RE = re.compile(
    r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d{1,9})?(Z|[+-]\d{2}:\d{2})$"
)
_REASON_RE = re.compile(r"^[a-z][a-z0-9_]{0,63}$")


class Rejected(Exception):
    """A terminal answer the bridge reaches from the letter's bytes and path."""

    def __init__(self, reason, detail):
        super().__init__(f"{reason}: {detail}")
        self.reason = reason
        self.detail = detail


# -- letters -----------------------------------------------------------------


def validate_participant(value, label="participant"):
    """post's participant-id grammar under the bridge's stricter component check."""
    common.validate_path_component(value, label=label)
    if ":" in value:
        raise ConfigError(f"{label} must not contain ':'")


def is_pmail(value):
    """A host-qualified participant letter: the only kind the pmail path takes."""
    return value.get("address_kind") == "participant" and "to_host" in value


def header_value(data):
    """The envelope object of a letter, or Rejected('malformed')."""
    separator = data.find(b"\n---\n")
    if separator < 0:
        raise Rejected(MALFORMED, "missing envelope separator")
    if separator > HEADER_MAX_BYTES:
        raise Rejected(MALFORMED, "header exceeds 4 KiB")
    try:
        value = load_json_bytes(data[:separator], "mail envelope")
    except ConfigError as error:
        raise Rejected(MALFORMED, str(error))
    if not isinstance(value, dict):
        raise Rejected(MALFORMED, "envelope must be an object")
    return value


def check_letter(value, mail_id, participant, to_host, logger=None):
    """The bridge's structural checks on a pmail envelope (design step 3).

    They read only the letter's bytes, its path and this host's identity, so
    they run on every attempt. Trust fact 2 (``from`` homed at the source
    host) is deliberately absent: it reads the placeholder table, which
    changes, so post runs it on first admission only (rev 3.1 correction 2).
    The bridge's own sender binding is likewise first-attempt only; see
    ``first_attempt_binding``.
    """
    missing = common.ENVELOPE_REQUIRED - set(value)
    if missing:
        raise Rejected(MALFORMED, "missing {}".format(",".join(sorted(missing))))
    for key in common.ENVELOPE_REQUIRED:
        if not isinstance(value[key], str):
            raise Rejected(MALFORMED, f"{key} must be a string")
    for key in common.ENVELOPE_OPTIONAL:
        if key in value and not isinstance(value[key], str):
            raise Rejected(MALFORMED, f"{key} must be a string")
    try:
        common.validate_attribution(value, common.MAIL_ADDRESS_KINDS)
        common.validate_id(value["id"], "envelope id")
        common.validate_room(value["from"], label="envelope from")
    except ConfigError as error:
        raise Rejected(MALFORMED, str(error))
    if value["id"] != mail_id:
        raise Rejected(MALFORMED, "envelope id does not equal the file name")
    if value.get("address_kind") != "participant":
        raise Rejected(MALFORMED, "address_kind must be participant")
    if value["to"] != participant:
        raise Rejected("to_mismatch", "envelope to does not equal the path participant")
    if value.get("to_host") != to_host:
        raise Rejected("to_mismatch", "envelope to_host is not this host")
    if value["kind"] not in common.MAIL_KINDS:
        raise Rejected(MALFORMED, "invalid kind")
    if len(value["subject"].encode("utf-8")) > 1024:
        raise Rejected(MALFORMED, "subject exceeds 1 KiB")
    try:
        dt.datetime.strptime(value["sent"], "%Y-%m-%d %H:%M:%S %z")
    except ValueError:
        raise Rejected(MALFORMED, "invalid sent")
    unknown = set(value) - common.ENVELOPE_REQUIRED - common.ENVELOPE_OPTIONAL
    if unknown and logger is not None:
        logger.emit("unknown_envelope_keys", id=mail_id, keys=sorted(unknown))
    return value


def admission_recorded(settings, participant, mail_id):
    """Whether post has an admission record for this letter.

    Post exposes no read-only query for it, so this checks that
    ``participants/<id>/imports/<mail-id>.json`` exists (lexists: a damaged
    record still counts) and never reads it. Coupled to post's layout
    (src/imports.rs); SPEC r6.0 names the coupling.
    """
    try:
        path = destination(
            settings.root, "participants", participant, "imports", mail_id + ".json"
        )
    except ConfigError:
        return True  # an unsafe path is post's to refuse, not ours to judge
    return os.path.lexists(str(path))


def first_attempt_binding(settings, snapshot, host, participant, mail_id, sender):
    """The destination bridge's sender binding for a participant letter
    (Grok G1; Aster's 9-reason ruling): ``name_collision`` or
    ``unpublished_sender``, or None to let post decide.

    First attempt only. Once post has admitted the letter, a later owner
    change must not reject a letter the recipient already has, so a letter
    with an admission record always goes to post, which replays it.
    """
    if admission_recorded(settings, participant, mail_id):
        return None
    verdict = binding_verdict(snapshot, host, sender)
    return verdict if verdict in (NAME_COLLISION, UNPUBLISHED_SENDER) else None


def git_blob_id(data):
    return hashlib.sha1(b"blob %d\0" % len(data) + data).hexdigest()


def _pmail_path(entry, dest_host):
    """``(participant, mail_id)`` for ``pmail/<dest_host>/<id>/<mail-id>.mail``, else None."""
    if entry["path"] is None:
        return None
    parts = PurePosixPath(entry["path"]).parts
    if len(parts) != 4 or parts[0] != "pmail" or parts[1] != dest_host:
        return None
    participant, filename = parts[2], parts[3]
    if not filename.endswith(".mail"):
        return None
    mail_id = filename[:-5]
    try:
        validate_participant(participant)
        common.validate_id(mail_id)
    except ConfigError:
        return None
    return participant, mail_id


# -- the import contract -----------------------------------------------------


@dataclass(frozen=True)
class Decision:
    outcome: str  # delivered | rejected | retry
    reason: Optional[str] = None  # noqa: UP045 - Python 3.9 dataclass runtime
    admitted_at: Optional[str] = None  # noqa: UP045
    detail: Optional[str] = None  # noqa: UP045
    replay: bool = False


def _retry(reason, detail):
    return Decision("retry", reason, None, (detail or "")[:DETAIL_MAX_CHARS])


def rfc3339_now():
    """Seconds-precision UTC with ``Z``: the shape of post's ``admitted_at``."""
    return dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def valid_rfc3339(value):
    if not isinstance(value, str) or _RFC3339_RE.fullmatch(value) is None:
        return False
    return epoch_from_iso(value.replace("Z", "+00:00")) is not None


# Listed keys and their types (design §The import command's contract).
_STR = (str,)
_OPT_STR = (str, type(None))
_CONTRACT_TYPES = {
    "ok": (bool,),
    "schema": _STR,
    "outcome": _STR,
    "reason": _OPT_STR,
    "participant": _STR,
    "mail_id": _STR,
    "source_host": _STR,
    "sha256": _STR,
    "admitted_at": _OPT_STR,
    "replay": (bool,),
    "detail": _OPT_STR,
}


def interpret_deliver(returncode, stdout, participant, mail_id, source_host, sha256):
    """Turn one ``post bridge deliver`` run into a Decision under the frozen contract.

    Only exit 0 carries a decision. ``delivered`` and ``rejected`` are
    accepted only in the exact schema with the echoed fields equal to the
    call; anything else is a retry, never a rejection.
    """
    if returncode != 0:
        reason = INVALID_INVOCATION if returncode == 2 else POST_UNAVAILABLE
        return _retry(reason, f"post exited {returncode}")
    if isinstance(stdout, str):
        stdout = stdout.encode("utf-8", "replace")
    text = stdout.strip()
    if not text:
        return _retry(POST_OUTPUT_MALFORMED, "empty stdout")
    try:
        value = load_json_bytes(text, "post bridge deliver output")
    except ConfigError as error:
        return _retry(POST_OUTPUT_MALFORMED, str(error))
    if not isinstance(value, dict):
        return _retry(POST_OUTPUT_MALFORMED, "output is not one JSON object")
    for key, types in _CONTRACT_TYPES.items():
        if key not in value:
            return _retry(POST_OUTPUT_MALFORMED, f"missing {key}")
        item = value[key]
        # bool is an int subclass; the listed types never admit int.
        if not isinstance(item, types) or (isinstance(item, bool) and bool not in types):
            return _retry(POST_OUTPUT_MALFORMED, f"{key} has the wrong type")
    if value["ok"] is not True or value["schema"] != DELIVER_SCHEMA:
        return _retry(POST_OUTPUT_MALFORMED, "wrong schema")
    if value["outcome"] not in DELIVER_OUTCOMES:
        return _retry(POST_OUTPUT_MALFORMED, "unknown outcome")
    if (
        value["participant"] != participant
        or value["mail_id"] != mail_id
        or value["source_host"] != source_host
    ):
        return _retry(POST_OUTPUT_MALFORMED, "echoed fields disagree with the call")
    detail = value["detail"]
    outcome = value["outcome"]
    if outcome == "retry":
        reason = value["reason"]
        if not isinstance(reason, str) or _REASON_RE.fullmatch(reason) is None:
            reason = POST_OUTPUT_MALFORMED
        return _retry(reason, detail)
    if value["sha256"] != sha256:
        return _retry(POST_OUTPUT_MALFORMED, "post digested different bytes")
    if outcome == "delivered":
        if value["reason"] is not None or not valid_rfc3339(value["admitted_at"]):
            return _retry(POST_OUTPUT_MALFORMED, "delivered without a valid admitted_at")
        return Decision("delivered", None, value["admitted_at"], detail, value["replay"])
    if value["reason"] not in TERMINAL_REASONS or value["admitted_at"] is not None:
        return _retry(POST_OUTPUT_MALFORMED, "rejected with an out-of-vocabulary reason")
    return Decision("rejected", value["reason"], None, detail, value["replay"])


def post_supports_deliver(settings):
    """Whether this host's post knows ``bridge deliver``.

    A pre-F3 post exits 2 (``unrecognized subcommand 'bridge'``), which the
    contract would also read as ``invalid_invocation``. Probing once per tick
    names the real cause in health and spares one doomed process per letter.
    """
    try:
        result = subprocess.run(
            [settings.post_bin, "bridge", "deliver", "--help"],
            env=common.post_environment(settings.root),
            capture_output=True,
            timeout=POST_CALL_TIMEOUT_SECONDS,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return False
    return result.returncode == 0


def archived_participant_dir(settings, participant):
    """The archive directory holding ``participant``'s record, or None.

    ``post participant gc`` (tier 2) moves a long-idle record whole from
    ``participants/<id>/`` to ``participants-archive/<id>/``. ``post bridge
    deliver`` restores such a recipient itself. This is the fallback for a
    post that still refuses one (``unknown_participant``): the bridge never
    writes inside post's participant store, so the letter waits and
    health.json's attention list carries ``post participant restore <id>``.
    """
    live = destination(settings.root, PARTICIPANTS_DIR, participant)
    if os.path.lexists(str(live)):
        return None
    archived = destination(settings.root, PARTICIPANTS_ARCHIVE_DIR, participant)
    if archived.is_dir() and not archived.is_symlink():
        return archived
    return None


def call_deliver(settings, deadline, participant, source_host, mail_id, sha256, data):
    tmp_root = destination(settings.root, "bridge", "tmp")
    ensure_dir(settings.root, tmp_root)
    # mkstemp: O_EXCL, mode 0600 - the private regular file post requires.
    descriptor, name = tempfile.mkstemp(
        prefix=f"pmail-{mail_id}.", suffix=".tmp", dir=str(tmp_root)
    )
    try:
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(data)
        arguments = [
            settings.post_bin,
            "bridge",
            "deliver",
            f"--participant={participant}",
            f"--source-host={source_host}",
            f"--mail-id={mail_id}",
            f"--sha256={sha256}",
            f"--file={name}",
            "--json",
        ]
        try:
            result = subprocess.run(
                arguments,
                env=common.post_environment(settings.root),
                capture_output=True,
                timeout=deadline.timeout(POST_CALL_TIMEOUT_SECONDS),
                check=False,
            )
        except subprocess.TimeoutExpired:
            return _retry(POST_UNAVAILABLE, "post bridge deliver timed out")
        except OSError as error:
            return _retry(POST_UNAVAILABLE, str(error))
        return interpret_deliver(
            result.returncode, result.stdout, participant, mail_id, source_host, sha256
        )
    finally:
        try:
            os.unlink(name)
        except FileNotFoundError:
            pass


# -- receipts ----------------------------------------------------------------


def preceipt_bytes(status, origin, host, participant, mail_id, sha256, reason, at):
    value = {
        "v": 1,
        "status": status,
        "origin": origin,
        "host": host,
        "participant": participant,
        "id": mail_id,
        "sha256": sha256,
        "reason": reason,
        "at": at,
    }
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode(
        "utf-8"
    )


def parse_preceipt(data, branch_host, origin, participant, mail_id, sha256):
    """Full sender-side validation (design §Sender acknowledgement, F3-6)."""
    if len(data) > PRECEIPT_MAX_BYTES:
        raise ConfigError("receipt exceeds 4 KiB")
    value = load_json_bytes(data, "participant receipt")
    if not isinstance(value, dict) or set(value) != PRECEIPT_KEYS:
        raise ConfigError("receipt has wrong keys")
    if type(value["v"]) is not int or value["v"] != 1:
        raise ConfigError("receipt version is not 1")
    if value["status"] not in PRECEIPT_STATUSES:
        raise ConfigError("receipt status is invalid")
    for key in ("origin", "host", "participant", "id", "sha256", "at"):
        if not isinstance(value[key], str):
            raise ConfigError(f"receipt {key} must be a string")
    if value["status"] == "delivered":
        if value["reason"] is not None:
            raise ConfigError("delivered receipt carries a reason")
    elif value["reason"] not in TERMINAL_REASONS:
        raise ConfigError("rejected receipt reason is out of vocabulary")
    if value["host"] != branch_host:
        raise ConfigError("receipt host is not its branch")
    if value["origin"] != origin:
        raise ConfigError("receipt origin is not this host")
    if value["participant"] != participant or value["id"] != mail_id:
        raise ConfigError("receipt path fields do not match")
    if common.SHA256_RE.fullmatch(value["sha256"]) is None or value["sha256"] != sha256:
        raise ConfigError("receipt sha256 does not match the letter")
    if not valid_rfc3339(value["at"]):
        raise ConfigError("receipt at is not RFC 3339")
    return value


def discard_uncommitted_receipts(settings, git, logger):
    """Recovery: a receipt that never reached a commit decided nothing.

    The design's rejection table re-evaluates a decision lost before its
    commit; a delivered one is rebuilt byte-identically from post's replay.
    """
    root = destination(settings.repo, "preceipts")
    if not root.is_dir():
        return 0
    if git.rev("HEAD") is None:
        committed = set()
    else:
        listed = git.run(
            ["ls-tree", "-r", "--name-only", "-z", "HEAD", "--", "preceipts/"],
            text=False,
        )
        committed = {
            raw.decode("utf-8", "replace") for raw in listed.stdout.split(b"\0") if raw
        }
    removed = 0
    for path in sorted(root.rglob("*")):
        if path.is_dir() and not path.is_symlink():
            continue
        relative = path.relative_to(settings.repo).as_posix()
        if relative in committed:
            continue
        git.run(["rm", "-q", "--cached", "--ignore-unmatch", "--", relative])
        path.unlink()
        removed += 1
        logger.emit("pmail_receipt_discarded", path=relative)
    return removed


# -- retry ledger (destination) ----------------------------------------------


class RetryLedger:
    """First-seen record of every letter the destination is still retrying.

    ``bridge/pmail-retry/<host>/<id>/<mail-id>.json`` holds
    ``{first_seen, reason, logged_at}``. A retry is logged when it first
    appears, when its reason changes, and at most hourly after that, never
    every tick. Health reads every record on disk (r5.6: the records, not the
    tick, are the source), and records of hosts that are no longer peers are
    dropped.
    """

    namespace = "pmail-retry"

    def __init__(self, root):
        self.root = root

    def _path(self, host, participant, mail_id):
        return destination(
            self.root, "bridge", self.namespace, host, participant, mail_id + ".json"
        )

    def _read(self, path):
        try:
            value = load_json_bytes(open_regular(path, 4096), str(path))
        except (FileNotFoundError, ConfigError, OSError):
            return None
        if not isinstance(value, dict):
            return None
        first = epoch_from_iso(value.get("first_seen"))
        if first is None:
            return None
        return value

    def note(self, host, participant, mail_id, reason, detail, logger, now=None):
        now = time.time() if now is None else now
        path = self._path(host, participant, mail_id)
        prior = self._read(path)
        stamp = utc_now()
        first_seen = prior["first_seen"] if prior else stamp
        logged_at = prior.get("logged_at") if prior else None
        logged = epoch_from_iso(logged_at) if isinstance(logged_at, str) else None
        emit = (
            prior is None
            or prior.get("reason") != reason
            or logged is None
            or now - logged >= LOG_REPEAT_SECONDS
        )
        if emit:
            logger.emit(
                "pmail_retry",
                host=host,
                participant=participant,
                id=mail_id,
                reason=reason,
                detail=detail,
            )
            logged_at = stamp
        value = {"first_seen": first_seen, "reason": reason, "logged_at": logged_at}
        if prior != value:
            atomic_replace(
                path,
                (json.dumps(value, sort_keys=True) + "\n").encode("utf-8"),
                self.root,
            )

    def release(self, host, participant, mail_id):
        try:
            self._path(host, participant, mail_id).unlink()
        except FileNotFoundError:
            pass

    def settle(self, host, keep):
        """After a full walk of ``host``: drop records of letters it no longer holds."""
        base = destination(self.root, "bridge", self.namespace, host)
        if not base.is_dir():
            return
        for path in list(base.rglob("*.json")):
            relative = path.relative_to(base).parts
            if len(relative) == 2 and (relative[0], relative[1][:-5]) in keep:
                continue
            try:
                path.unlink()
            except FileNotFoundError:
                pass

    def waiting(self, peers, reason):
        """``{participant: [(host, mail_id), ...]}`` for letters retrying for ``reason``.

        The records are the source (r5.6): a letter that was delivered, or
        that its sender withdrew, is released or settled away and drops out.
        """
        found = {}
        base = destination(self.root, "bridge", self.namespace)
        try:
            hosts = sorted(os.listdir(str(base)))
        except FileNotFoundError:
            return found
        for host in hosts:
            directory = base / host
            if host not in peers or not directory.is_dir() or directory.is_symlink():
                continue
            for path in sorted(directory.rglob("*.json")):
                parts = path.relative_to(directory).parts
                value = self._read(path)
                if len(parts) != 2 or value is None or value.get("reason") != reason:
                    continue
                found.setdefault(parts[0], []).append((host, parts[1][:-5]))
        return found

    def summary(self, peers, now=None, prune=True):
        """Per-host count and age, and per-reason counts; a non-peer's
        records are dropped only when ``prune`` (the peer set is known)."""
        now = time.time() if now is None else now
        base = destination(self.root, "bridge", self.namespace)
        peers = set(peers)
        per_host = {}
        reasons = {}
        try:
            hosts = sorted(os.listdir(str(base)))
        except FileNotFoundError:
            return per_host, reasons
        for host in hosts:
            directory = base / host
            if not directory.is_dir() or directory.is_symlink():
                continue
            if host not in peers and prune:
                shutil.rmtree(str(directory), ignore_errors=True)
                continue
            times = []
            for path in directory.rglob("*.json"):
                value = self._read(path)
                if value is None:
                    continue
                times.append(epoch_from_iso(value["first_seen"]))
                reason = value.get("reason")
                if isinstance(reason, str):
                    reasons[reason] = reasons.get(reason, 0) + 1
            if times:
                per_host[host] = {
                    "count": len(times),
                    "oldest_age_seconds": max(0, int(now - min(times))),
                }
        return per_host, reasons


# -- destination -------------------------------------------------------------


@dataclass
class ImportStats:
    delivered: int = 0
    rejected: int = 0
    retried: int = 0
    reasons: dict = field(default_factory=dict)
    ignored: int = 0


def _once(settings, namespace, key, logger, action, **fields):
    """Log ``action`` once per ``key``, persisted under bridge/<namespace>/."""
    digest = hashlib.sha256(key.encode("utf-8")).hexdigest()
    marker = destination(settings.root, "bridge", namespace, digest)
    try:
        exclusive_publish(
            marker, b"", destination(settings.root, "bridge", "tmp"), settings.root
        )
    except FileExistsError:
        return False
    logger.emit(action, **fields)
    return True


def _write_preceipt(settings, fence, host, participant, mail_id, payload, logger):
    if fence():
        raise TickError("fenced")
    path = destination(settings.repo, "preceipts", host, participant, mail_id + ".json")
    atomic_replace(path, payload, settings.repo)
    value = json.loads(payload)
    logger.emit(
        "pmail_receipt",
        host=host,
        participant=participant,
        id=mail_id,
        status=value["status"],
        reason=value["reason"],
    )
    checkpoint("pmail-receipt-written")


def classify_entry(settings, git, ref, entry):
    """``(kind, data, sha256, detail)`` for one relay entry (Grok G2).

    A terminal receipt must carry the digest of the letter's bytes, the one
    thing the sender can check. So a regular blob is always hashed: in full
    when it is a readable letter, by streaming when it is over this host's
    cap or not 100644 (still rejected, but with a digest the sender can
    validate). An entry with no content has no digest and gets no receipt;
    a failed read is retried, never answered.
    """
    if entry["type"] != "blob" or entry["mode"] not in REGULAR_MODES:
        return ENTRY_NO_CONTENT, None, None, f"{entry['type']} mode {entry['mode']}"
    if entry["size"] is None:
        return ENTRY_UNREADABLE, None, None, "blob size unknown"
    if entry["mode"] == "100644" and entry["size"] <= settings.max_mail_bytes:
        data = git.show(ref, entry["path"])
        if data is None or len(data) != entry["size"]:
            return ENTRY_UNREADABLE, None, None, "git show failed"
        return ENTRY_LETTER, data, hashlib.sha256(data).hexdigest(), None
    streamed = git.blob_sha256(entry["object"])
    if streamed is None or streamed[1] != entry["size"]:
        return ENTRY_UNREADABLE, None, None, "git cat-file failed"
    detail = "oversize" if entry["size"] > settings.max_mail_bytes else "not mode 100644"
    return ENTRY_DIGEST_ONLY, None, streamed[0], detail


def _check_committed_receipt(settings, git, ref, host, entry, participant, mail_id,
                             receipt_entry, logger):
    """A receipt already stands: note a later, different letter under its path."""
    kind, _, sha256, detail = classify_entry(settings, git, ref, entry)
    if kind in (ENTRY_NO_CONTENT, ENTRY_UNREADABLE):
        # The receipt stands either way; say that this entry went unchecked.
        _once(
            settings, "pmail-ignored", f"unchecked:{host}:{entry['path']}:{entry['object']}",
            logger, "pmail_entry_unchecked", host=host, participant=participant,
            id=mail_id, detail=detail,
        )
        return False
    receipt = git.show("HEAD", receipt_entry["path"])
    try:
        value = load_json_bytes(receipt or b"", "participant receipt")
        stood = value.get("sha256") if isinstance(value, dict) else None
    except ConfigError:
        stood = None
    if stood == sha256:
        return False
    path = destination(
        settings.root, "bridge", "pmail-receipt-conflict", host, participant,
        mail_id + ".json",
    )
    note = {
        "host": host,
        "participant": participant,
        "id": mail_id,
        "receipt_sha256": stood,
        "letter_sha256": sha256,
        "at": utc_now(),
    }
    try:
        exclusive_publish(
            path,
            (json.dumps(note, sort_keys=True) + "\n").encode("utf-8"),
            destination(settings.root, "bridge", "tmp"),
            settings.root,
        )
    except FileExistsError:
        return True
    logger.emit(
        "receipt_conflict",
        side="destination",
        host=host,
        participant=participant,
        id=mail_id,
    )
    return True


def import_pmail(settings, git, snapshot, logger, deadline, fence):
    """Deliver every ``pmail/<self>/`` letter on each effective peer's branch."""
    stats = ImportStats()
    ledger = RetryLedger(settings.root)
    supported = None
    for host in snapshot.peers:
        deadline.check()
        ref = snapshot.oids.get(f"machines/{host}")
        if ref is None:
            continue
        try:
            entries = git.ls_tree(ref, f"pmail/{settings.host}/")
        except GitReadError as error:
            note_git_failure(git, logger, host, error)
            continue
        if not entries:
            ledger.settle(host, set())
            continue
        try:
            committed = {
                item["path"]: item
                for item in (
                    git.ls_tree("HEAD", f"preceipts/{host}/")
                    if git.rev("HEAD") is not None
                    else []
                )
                if item["path"] is not None
            }
        except GitReadError as error:
            note_git_failure(git, logger, settings.host, error)
            continue
        waiting = set()
        for entry in entries:
            deadline.check()
            parsed = _pmail_path(entry, settings.host)
            if parsed is None:
                label = entry["path"] if entry["path"] is not None else entry["raw_path"].hex()
                _once(
                    settings, "pmail-ignored", f"path:{host}:{label}", logger,
                    "pmail_path_ignored", host=host, path=label,
                )
                continue
            participant, mail_id = parsed
            receipt_path = f"preceipts/{host}/{participant}/{mail_id}.json"
            # Step 1: a committed receipt is final, whatever changed since.
            if receipt_path in committed:
                _check_committed_receipt(
                    settings, git, ref, host, entry, participant, mail_id,
                    committed[receipt_path], logger,
                )
                ledger.release(host, participant, mail_id)
                continue
            # Steps 2-3: transport and envelope. A rejection is terminal and
            # always carries the content digest (Grok G2).
            kind, data, sha256, detail = classify_entry(settings, git, ref, entry)
            if kind == ENTRY_NO_CONTENT:
                stats.ignored += 1
                _once(
                    settings, "pmail-ignored",
                    f"entry:{host}:{entry['path']}:{entry['object']}", logger,
                    "pmail_entry_ignored", host=host, participant=participant,
                    id=mail_id, detail=detail,
                )
                continue
            if kind == ENTRY_UNREADABLE:
                decision = _retry(OBJECT_UNREADABLE, detail)
                ledger.note(
                    host, participant, mail_id, decision.reason, decision.detail, logger
                )
                waiting.add((participant, mail_id))
                stats.retried += 1
                stats.reasons[decision.reason] = stats.reasons.get(decision.reason, 0) + 1
                continue
            rejection = None
            if kind == ENTRY_DIGEST_ONLY:
                rejection = Rejected(MALFORMED, detail)
            else:
                try:
                    envelope = check_letter(
                        header_value(data), mail_id, participant, settings.host, logger
                    )
                except Rejected as error:
                    rejection = error
                else:
                    verdict = first_attempt_binding(
                        settings, snapshot, host, participant, mail_id, envelope["from"]
                    )
                    if verdict is not None:
                        rejection = Rejected(verdict, "sender binding")
            if rejection is not None:
                payload = preceipt_bytes(
                    "rejected", host, settings.host, participant, mail_id, sha256,
                    rejection.reason, rfc3339_now(),
                )
                _write_preceipt(settings, fence, host, participant, mail_id, payload, logger)
                ledger.release(host, participant, mail_id)
                stats.rejected += 1
                continue
            if fence():
                raise TickError("fenced")
            if supported is None:
                supported = post_supports_deliver(settings)
                if not supported:
                    _once(
                        settings, "pmail-ignored", f"post-unsupported:{settings.post_bin}",
                        logger, "pmail_post_unsupported", post_bin=settings.post_bin,
                    )
            if supported:
                decision = call_deliver(
                    settings, deadline, participant, host, mail_id, sha256, data
                )
            else:
                decision = _retry(POST_UNAVAILABLE, "post has no `bridge deliver`")
            if decision.outcome == "rejected" and decision.reason == "unknown_participant":
                # post answers that for an id with no live record, and for an
                # archived one only while it does not restore by itself.
                # Nothing was written; an archived record is not a dead one.
                archived = archived_participant_dir(settings, participant)
                if archived is not None:
                    decision = _retry(
                        PARTICIPANT_ARCHIVED,
                        f"participant record is archived at {archived}",
                    )
            if decision.outcome == "retry":
                ledger.note(
                    host, participant, mail_id, decision.reason, decision.detail, logger
                )
                waiting.add((participant, mail_id))
                stats.retried += 1
                stats.reasons[decision.reason] = stats.reasons.get(decision.reason, 0) + 1
                continue
            if decision.outcome == "delivered":
                # `at` is the admission record's admitted_at: a receipt rebuilt
                # after a crash is byte-identical to the one it replaces.
                payload = preceipt_bytes(
                    "delivered", host, settings.host, participant, mail_id, sha256,
                    None, decision.admitted_at,
                )
                stats.delivered += 1
            else:
                payload = preceipt_bytes(
                    "rejected", host, settings.host, participant, mail_id, sha256,
                    decision.reason, rfc3339_now(),
                )
                stats.rejected += 1
            _write_preceipt(settings, fence, host, participant, mail_id, payload, logger)
            ledger.release(host, participant, mail_id)
        ledger.settle(host, waiting)
    return stats


# -- sender ------------------------------------------------------------------


def _state_file(settings, namespace, mail_id):
    return destination(settings.root, "bridge", namespace, mail_id + ".json")


def _read_state(settings, namespace, mail_id, maximum=4096):
    try:
        return open_regular(_state_file(settings, namespace, mail_id), maximum)
    except FileNotFoundError:
        return None
    except ConfigError:
        return b""


def note_typed(settings, mail_id, value, logger):
    """The typed-letter outbound exclusion's once-per-id log (F3-R2-B).

    ``bridge/typed-skipped/<id>`` records the letter's path (``pmail`` or
    ``local``) so later ticks skip a local typed letter, or an acknowledged
    pmail letter, without reading it again.
    """
    kind = "pmail" if is_pmail(value) else "local"
    marker = destination(settings.root, "bridge", "typed-skipped", mail_id)
    try:
        exclusive_publish(
            marker,
            (kind + "\n").encode("ascii"),
            destination(settings.root, "bridge", "tmp"),
            settings.root,
        )
    except FileExistsError:
        return kind
    logger.emit(
        "outbound_typed_skipped",
        id=mail_id,
        address_kind=value.get("address_kind"),
        to_host=value.get("to_host"),
        path=kind,
    )
    return kind


def typed_skip_known(settings, mail_id):
    """True when select can skip this archive letter without reading it."""
    marker = destination(settings.root, "bridge", "typed-skipped", mail_id)
    try:
        kind = open_regular(marker, 16).strip()
    except (FileNotFoundError, ConfigError):
        return False
    if kind == b"local":
        return True
    return kind == b"pmail" and _state_file(settings, "pmail-acked", mail_id).exists()


def write_status(settings, mail_id, blocked_reason=None, last_error=None):
    """``bridge/pmail-status/<id>.json``: why a queued letter is still queued.

    Exactly ``{v, id, blocked_reason?, last_error?, at}`` (post delivery's
    reader, lane R d57d7d2); an absent reason is omitted, never null.
    """
    path = _state_file(settings, "pmail-status", mail_id)
    record = {"v": 1, "id": mail_id}
    if blocked_reason is not None:
        record["blocked_reason"] = blocked_reason
    if last_error is not None:
        record["last_error"] = last_error
    try:
        prior = load_json_bytes(open_regular(path, 4096), str(path))
    except (FileNotFoundError, ConfigError):
        prior = None
    if isinstance(prior, dict) and {k: v for k, v in prior.items() if k != "at"} == record:
        return False
    record["at"] = utc_now()
    atomic_replace(
        path, (json.dumps(record, sort_keys=True) + "\n").encode("utf-8"), settings.root
    )
    return True


def _clear_status(settings, mail_id):
    try:
        _state_file(settings, "pmail-status", mail_id).unlink()
    except FileNotFoundError:
        pass


def _worktree_letters(settings):
    """Every staged pmail letter in the relay worktree: {mail_id: (dest, id, path, data)}."""
    root = destination(settings.repo, "pmail")
    letters = {}
    if not root.is_dir():
        return letters
    for path in sorted(root.glob("*/*/*.mail")):
        if path.is_symlink() or not path.is_file():
            continue
        dest, participant, filename = path.relative_to(root).parts
        mail_id = filename[:-5]
        try:
            common.validate_host(dest)
            validate_participant(participant)
            common.validate_id(mail_id)
            data = open_regular(path, settings.max_mail_bytes)
        except (ConfigError, FileNotFoundError):
            continue
        letters[mail_id] = (dest, participant, path, data)
    return letters


def _peer_receipt(settings, git, snapshot, dest, participant, mail_id, sha256, logger):
    """The valid receipt for one letter on ``machines/<dest>``: (bytes, value) or None.

    A receipt is read only from the destination's own branch, so a receipt
    published on any other branch is never considered.
    """
    ref = snapshot.oids.get(f"machines/{dest}")
    if ref is None:
        return None
    receipt_path = f"preceipts/{settings.host}/{participant}/{mail_id}.json"
    try:
        listed = git.ls_tree(ref, receipt_path)
    except GitReadError as error:
        note_git_failure(git, logger, dest, error)
        return None
    matches = [entry for entry in listed if entry["path"] == receipt_path]
    if len(matches) != 1:
        return None
    entry = matches[0]
    try:
        if (
            entry["mode"] != "100644"
            or entry["type"] != "blob"
            or entry["size"] is None
            or entry["size"] > PRECEIPT_MAX_BYTES
        ):
            raise ConfigError("invalid mode or size")
        data = git.show(ref, receipt_path)
        if data is None or len(data) != entry["size"]:
            raise ConfigError("unreadable object")
        value = parse_preceipt(data, dest, settings.host, participant, mail_id, sha256)
    except ConfigError as error:
        _once(
            settings, "pmail-ignored", f"receipt:{dest}:{entry['object']}", logger,
            "pmail_receipt_ignored", host=dest, participant=participant, id=mail_id,
            reason=str(error),
        )
        return None
    return data, value


def _prune(settings, git, path, logger, dest, participant, mail_id):
    relative = path.relative_to(settings.repo).as_posix()
    tracked = git.run(
        ["ls-files", "--error-unmatch", "--", relative], check=False
    ).returncode == 0
    if tracked:
        git.run(["rm", "--quiet", "--", relative])
    else:
        path.unlink()
    logger.emit("pmail_pruned", host=dest, participant=participant, id=mail_id)
    checkpoint("pmail-s7-pruned")


def scan_conflicts(settings, git, snapshot, logger, deadline):
    """Report a receipt that differs from the ``acked`` record it follows.

    The first valid receipt wins and is never replaced (F3-6). The scan reads
    each effective destination's ``preceipts/<self>/`` listing every tick, so
    a later, different receipt is seen even after the pmail entry was pruned.
    A blob equal to the acked bytes costs no read.
    """
    acked_root = destination(settings.root, "bridge", "pmail-acked")
    if not acked_root.is_dir():
        return 0
    found = 0
    for dest in snapshot.peers:
        deadline.check()
        ref = snapshot.oids.get(f"machines/{dest}")
        if ref is None:
            continue
        try:
            entries = git.ls_tree(ref, f"preceipts/{settings.host}/")
        except GitReadError as error:
            note_git_failure(git, logger, dest, error)
            continue
        for entry in entries:
            if entry["path"] is None:
                continue
            parts = PurePosixPath(entry["path"]).parts
            if len(parts) != 4 or not parts[3].endswith(".json"):
                continue
            participant, mail_id = parts[2], parts[3][:-5]
            try:
                common.validate_id(mail_id)
            except ConfigError:
                continue
            acked = _read_state(settings, "pmail-acked", mail_id)
            if not acked or git_blob_id(acked) == entry["object"]:
                continue
            if _state_file(settings, "pmail-conflicts", mail_id).exists():
                continue
            try:
                first = load_json_bytes(acked, "acked record")
                if not isinstance(first, dict) or first.get("host") != dest:
                    continue
                data = git.show(ref, entry["path"])
                if data is None or len(data) > PRECEIPT_MAX_BYTES:
                    raise ConfigError("unreadable receipt")
                parse_preceipt(
                    data, dest, settings.host, participant, mail_id, first.get("sha256")
                )
            except ConfigError as error:
                _once(
                    settings, "pmail-ignored", f"receipt:{dest}:{entry['object']}", logger,
                    "pmail_receipt_ignored", host=dest, participant=participant,
                    id=mail_id, reason=str(error),
                )
                continue
            try:
                exclusive_publish(
                    _state_file(settings, "pmail-conflicts", mail_id),
                    data,
                    destination(settings.root, "bridge", "tmp"),
                    settings.root,
                )
            except FileExistsError:
                continue
            found += 1
            logger.emit(
                "receipt_conflict", side="sender", host=dest,
                participant=participant, id=mail_id,
            )
    return found


def derive_markers(settings, git, logger, ref):
    """``published`` only from a pushed commit whose pmail bytes equal the archive.

    ``ref`` is the remote tracking ref ``origin/machines/<self>``, or HEAD at
    the instant a push of it succeeded. Local HEAD is never a source here: a
    stage or commit that was not pushed is not published (F3-6).
    """
    commit = git.rev(ref)
    if commit is None:
        return 0
    try:
        entries = git.ls_tree(commit, "pmail/")
    except GitReadError as error:
        note_git_failure(git, logger, settings.host, error)
        return 0
    created = 0
    for entry in entries:
        if entry["path"] is None:
            continue
        parts = PurePosixPath(entry["path"]).parts
        if len(parts) != 4 or not parts[3].endswith(".mail"):
            continue
        dest, participant, mail_id = parts[1], parts[2], parts[3][:-5]
        try:
            common.validate_id(mail_id)
        except ConfigError:
            continue
        marker = _state_file(settings, "pmail-published", mail_id)
        if marker.exists() or _state_file(settings, "pmail-acked", mail_id).exists():
            continue
        archive = destination(settings.root, "archive", mail_id + ".mail")
        try:
            data = open_regular(archive, settings.max_mail_bytes)
        except (FileNotFoundError, ConfigError):
            continue
        if git_blob_id(data) != entry["object"]:
            continue
        # post delivery reads {v, id, host, sha256, commit, at} (lane R
        # d57d7d2); `participant` is an extra key it ignores.
        record = {
            "v": 1,
            "id": mail_id,
            "host": dest,
            "participant": participant,
            "sha256": hashlib.sha256(data).hexdigest(),
            "commit": commit,
            "at": utc_now(),
        }
        try:
            exclusive_publish(
                marker,
                (json.dumps(record, sort_keys=True) + "\n").encode("utf-8"),
                destination(settings.root, "bridge", "tmp"),
                settings.root,
            )
        except FileExistsError:
            continue
        _clear_status(settings, mail_id)
        created += 1
        logger.emit(
            "pmail_published", host=dest, participant=participant, id=mail_id, commit=commit
        )
        checkpoint("pmail-s4-published")
    return created


@dataclass
class SenderStats:
    staged: int = 0
    acked: int = 0
    pruned: int = 0


def _acked_valid(acked, dest, origin, participant, mail_id, sha256):
    if not acked:
        return False
    try:
        parse_preceipt(acked, dest, origin, participant, mail_id, sha256)
    except (ConfigError, UnicodeDecodeError):
        return False
    return True


def sender_pass(settings, git, snapshot, logger, typed, deadline, fence):
    """Acknowledge, prune, mark and select participant letters on the sending side.

    Receipts are consumed before markers are derived, so a receipt that
    outran the marker (a crash after the push) still reaches the terminal
    state; ``acked`` needs no marker.
    """
    stats = SenderStats()
    staged = _worktree_letters(settings)
    for mail_id, (dest, participant, path, data) in sorted(staged.items()):
        deadline.check()
        sha256 = hashlib.sha256(data).hexdigest()
        found = _peer_receipt(
            settings, git, snapshot, dest, participant, mail_id, sha256, logger
        )
        acked_path = _state_file(settings, "pmail-acked", mail_id)
        acked = _read_state(settings, "pmail-acked", mail_id)
        if acked is None:
            if found is None:
                continue
            if fence():
                raise TickError("fenced")
            receipt, value = found
            checkpoint("pmail-s5-receipt")
            try:
                exclusive_publish(
                    acked_path,
                    receipt,
                    destination(settings.root, "bridge", "tmp"),
                    settings.root,
                )
            except FileExistsError:
                acked = _read_state(settings, "pmail-acked", mail_id)
            else:
                acked = receipt
                found = None
                stats.acked += 1
                _clear_status(settings, mail_id)
                logger.emit(
                    "pmail_acked",
                    host=dest,
                    participant=participant,
                    id=mail_id,
                    status=value["status"],
                    reason=value["reason"],
                )
                checkpoint("pmail-s6-acked")
        if not _acked_valid(acked, dest, settings.host, participant, mail_id, sha256):
            # Corrupt or vanished: never pruned, never counted as acked.
            write_status(settings, mail_id, blocked_reason=ACKED_INVALID)
            digest = hashlib.sha256(acked or b"").hexdigest()
            _once(
                settings, "pmail-ignored", f"acked:{mail_id}:{digest}", logger,
                "pmail_acked_invalid", host=dest, participant=participant, id=mail_id,
            )
            continue
        if fence():
            raise TickError("fenced")
        _clear_status(settings, mail_id)
        _prune(settings, git, path, logger, dest, participant, mail_id)
        stats.pruned += 1
    scan_conflicts(settings, git, snapshot, logger, deadline)
    derive_markers(settings, git, logger, f"origin/machines/{settings.host}")
    for path, mail_id, data, value in typed:
        deadline.check()
        if (
            _state_file(settings, "pmail-acked", mail_id).exists()
            or _state_file(settings, "pmail-published", mail_id).exists()
            or mail_id in staged
        ):
            continue
        try:
            check_letter(value, mail_id, value.get("to"), value.get("to_host"))
            validate_participant(value["to"])
            common.validate_host(value["to_host"])
        except (Rejected, ConfigError):
            write_status(settings, mail_id, blocked_reason=MALFORMED)
            continue
        dest = value["to_host"]
        if dest == settings.host or dest not in snapshot.peers:
            write_status(settings, mail_id, blocked_reason=PEER_NOT_EFFECTIVE)
            continue
        if value["from"] not in snapshot.publishable:
            write_status(settings, mail_id, blocked_reason=SENDER_UNPUBLISHED)
            continue
        if len(data) > settings.max_mail_bytes:
            write_status(settings, mail_id, blocked_reason=OVERSIZE)
            continue
        if fence():
            raise TickError("fenced")
        target = destination(settings.repo, "pmail", dest, value["to"], mail_id + ".mail")
        try:
            exclusive_publish(target, data, target.parent, settings.repo)
        except FileExistsError:
            if open_regular(target, settings.max_mail_bytes) != data:
                raise ConfigError(f"pmail differs from immutable archive for {mail_id}")
        write_status(settings, mail_id)
        stats.staged += 1
        logger.emit(
            "pmail_staged", host=dest, participant=value["to"], id=mail_id,
            archive=str(path),
        )
        checkpoint("pmail-s2-staged")
    return stats


def note_unpushed(settings, logger, error):
    """A failed push: every staged, unpublished letter says why it is still queued."""
    for mail_id in sorted(_worktree_letters(settings)):
        if (
            _state_file(settings, "pmail-published", mail_id).exists()
            or _state_file(settings, "pmail-acked", mail_id).exists()
        ):
            continue
        write_status(settings, mail_id, last_error=error)


def awaiting_summary(settings, now=None):
    """Published letters with no ``acked`` record: {host: {count, oldest_age_seconds}}."""
    now = time.time() if now is None else now
    root = destination(settings.root, "bridge", "pmail-published")
    result = {}
    if not root.is_dir():
        return result
    for path in root.glob("*.json"):
        mail_id = path.name[:-5]
        if _state_file(settings, "pmail-acked", mail_id).exists():
            continue
        try:
            value = load_json_bytes(open_regular(path, 4096), str(path))
        except (ConfigError, FileNotFoundError):
            continue
        if not isinstance(value, dict) or not isinstance(value.get("host"), str):
            continue
        at = epoch_from_iso(value.get("at"))
        age = max(0, int(now - at)) if at is not None else 0
        item = result.setdefault(value["host"], {"count": 0, "oldest_age_seconds": 0})
        item["count"] += 1
        item["oldest_age_seconds"] = max(item["oldest_age_seconds"], age)
    return result


def log_awaiting(settings, logger, awaiting, now=None):
    """At most one ``pmail_awaiting_receipt`` line per destination per hour.

    A destination without F3 never answers, so its letters stay published;
    the count and oldest age are in health every tick, and the log says so
    once and then hourly, never every tick.
    """
    now = time.time() if now is None else now
    path = destination(settings.root, "bridge", "pmail-awaiting.json")
    try:
        prior = load_json_bytes(open_regular(path, 64 * 1024), str(path))
    except (FileNotFoundError, ConfigError):
        prior = {}
    if not isinstance(prior, dict):
        prior = {}
    state = {}
    for host, item in sorted(awaiting.items()):
        last = prior.get(host)
        logged = epoch_from_iso(last) if isinstance(last, str) else None
        if logged is None or now - logged >= LOG_REPEAT_SECONDS:
            logger.emit(
                "pmail_awaiting_receipt",
                host=host,
                count=item["count"],
                oldest_age_seconds=item["oldest_age_seconds"],
            )
            state[host] = utc_now()
        else:
            state[host] = last
    if state != prior:
        atomic_replace(
            path, (json.dumps(state, sort_keys=True) + "\n").encode("utf-8"), settings.root
        )


def blocked_summary(settings):
    root = destination(settings.root, "bridge", "pmail-status")
    result = {}
    if not root.is_dir():
        return result
    for path in root.glob("*.json"):
        try:
            value = load_json_bytes(open_regular(path, 4096), str(path))
        except (ConfigError, FileNotFoundError):
            continue
        if not isinstance(value, dict):
            continue
        reason = value.get("blocked_reason") or value.get("last_error")
        if isinstance(reason, str):
            result[reason] = result.get(reason, 0) + 1
    return result


def _count_files(settings, namespace):
    root = destination(settings.root, "bridge", namespace)
    if not root.is_dir():
        return 0
    return sum(1 for path in root.rglob("*.json") if path.is_file())


def health(settings, snapshot, imported, sender):
    """The ``pmail`` object of health.json, from records on disk."""
    retry, reasons = RetryLedger(settings.root).summary(
        snapshot.peers, prune=snapshot.peers_known
    )
    awaiting = awaiting_summary(settings)
    staged = sum(
        1
        for mail_id in _worktree_letters(settings)
        if not _state_file(settings, "pmail-published", mail_id).exists()
        and not _state_file(settings, "pmail-acked", mail_id).exists()
    )
    return {
        "delivered": imported.delivered,
        "rejected": imported.rejected,
        "retry": retry,
        "retry_reasons": reasons,
        "receipt_conflicts": _count_files(settings, "pmail-receipt-conflict")
        + _count_files(settings, "pmail-conflicts"),
        "ignored_entries": imported.ignored,
        "staged": staged,
        "awaiting_receipt": awaiting,
        "queued": blocked_summary(settings),
    }


def empty_health():
    return {
        "delivered": 0,
        "rejected": 0,
        "retry": {},
        "retry_reasons": {},
        "receipt_conflicts": 0,
        "ignored_entries": 0,
        "staged": 0,
        "awaiting_receipt": {},
        "queued": {},
    }


def _count_map(value):
    if not isinstance(value, dict):
        return {}
    return {
        key: item
        for key, item in value.items()
        if isinstance(key, str) and type(item) is int
    }


def _host_map(value):
    if not isinstance(value, dict):
        return {}
    kept = {}
    for host, item in value.items():
        if (
            isinstance(host, str)
            and isinstance(item, dict)
            and all(type(item.get(key)) is int for key in ("count", "oldest_age_seconds"))
        ):
            kept[host] = {
                "count": item["count"],
                "oldest_age_seconds": item["oldest_age_seconds"],
            }
    return kept


def bounded_health(value):
    """Carry a prior ``pmail`` object forward, dropping anything malformed."""
    result = empty_health()
    if not isinstance(value, dict):
        return result
    for key in ("delivered", "rejected", "receipt_conflicts", "ignored_entries", "staged"):
        if type(value.get(key)) is int:
            result[key] = value[key]
    for key in ("retry", "awaiting_receipt"):
        result[key] = _host_map(value.get(key))
    for key in ("retry_reasons", "queued"):
        result[key] = _count_map(value.get(key))
    return result


_TIMESPAN_UNITS = {
    "": 1, "s": 1, "sec": 1, "second": 1, "seconds": 1,
    "m": 60, "min": 60, "minute": 60, "minutes": 60,
    "h": 3600, "hr": 3600, "hour": 3600, "hours": 3600,
}
_TIMESPAN_RE = re.compile(r"(\d+)\s*([a-z]*)")


def interval_seconds(raw):
    """``BRIDGE_INTERVAL_SECONDS`` in seconds for health; never raises.

    install.sh renders the timer's ``--interval`` here, which is a systemd
    timespan (``15``, ``30s``, ``2min``, ``1h 30s``). Unset, unparseable or
    out of range is None, unknown: health then omits ``interval_s`` and post
    reads the bridge as unavailable rather than trusting an invented
    interval (GLM m2). Never a config error; the fatal-config writer uses
    this too.
    """
    if raw is None:
        return None
    text = raw.strip().lower()
    total = 0
    position = 0
    for match in _TIMESPAN_RE.finditer(text):
        if text[position:match.start()].strip():
            return None
        unit = _TIMESPAN_UNITS.get(match.group(2))
        if unit is None:
            return None
        total += int(match.group(1)) * unit
        position = match.end()
    if not text or text[position:].strip() or not 0 < total <= MAX_INTERVAL_SECONDS:
        return None
    return total


def carried_capability_fields(prior):
    """The previous health's capability fields, unchanged; absent stays absent.

    The busy writer runs while another process holds the tick lock, and that
    holder may be an older bridge (Grok G3): it must not vouch for a tick
    this process did not run.
    """
    return {
        key: prior[key]
        for key in ("capabilities", "ticked_at", "interval_s")
        if key in prior
    }


def capability_fields(interval, now):
    fields = {"capabilities": sorted(CAPABILITIES), "ticked_at": now}
    if interval is not None:
        fields["interval_s"] = interval
    return fields
