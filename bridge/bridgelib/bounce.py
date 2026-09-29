"""Bounce a terminally refused outbound letter back to its sender.

A receiver that will never deliver a letter writes a ``quarantined`` receipt.
Until this module the sender kept that letter in its relay outbox forever:
nothing told the agent who wrote it, and nothing retired the entry. Now the
sender's bridge, on seeing a terminal refusal receipt, writes one system
letter to whoever sent the letter, records that it did, and only then lets
the caller retire the outbox entry.

Who sent it. The bridge records, when it first takes a letter from the
archive for the relay (:func:`record_origin`, ``bridge/origin/<id>.json``),
the sending participant it could verify at that moment and the workspace the
letter came from. The bounce goes to that recorded sender: the participant's
inbox while the participant is active and still bound to that workspace, else
the inbox of the workspace (any session there sees it), else a dead-letter
file the attention list points at. A letter published before this record
existed has nothing recorded, so the letter's own stamps are believed only
while the participant still names the workspace the letter says it came from;
otherwise the notice is a dead letter. A record that exists but does not
describe the letter is never overwritten and never believed: the bounce is a
dead letter (basis ``unproven``) and the caller lists the record. Nothing is
ever routed by a participant's *current* workspace alone: a rebound
participant would read someone else's bounce. A record is removed once the
letter's outbox entry is retired and that retirement is pushed
(:func:`remove_origin`, called by the sweep).

Crash safety. Everything is idempotent under a hard kill between any two
steps, and no step is ever skipped:

1. ``bridge/bounced/<id>.json`` (the intent) is published first. It names the
   letter (id and sha256), the notice's letter id and its destination, the
   time the notice will carry and the sha256 of the whole notice. A redo
   therefore writes the same bytes to the same place rather than a second
   notice, and a notice found later is judged against that digest.
2. ``bridge/bounced/<id>.body`` keeps the original body, so the re-send
   command in the notice names a file that exists.
3. The notice is published with an exclusive link. A notice already at the
   intent's path is completed, never re-routed and never duplicated, but only
   when every byte of it matches the intent's digest.
4. ``bridge/bounced/<id>.sent`` records that the notice exists, so a redo
   after step 3 does not write it again or log it twice. The marker is not
   proof on its own: it counts only while the notice is still at the intent's
   path. A marker with no notice raises :class:`BounceConflict`.

Every file found on a redo (intent, body, notice, sent marker) is checked
against the letter in hand. One that does not describe this letter raises
:class:`BounceConflict`: nothing is retired, the caller keeps the outbox entry
and lists an attention item. (post 0.9.0 never moves mail out of an inbox, so
a notice the participant has read is still there for the check to find.)

The caller retires the outbox entry (``git rm``) only after :func:`notify`
returns, and the receiver-side entry vanishing on the next fetch is the end
of the letter's life. The notice never enters ``archive/``: nothing exports
from a participant or room inbox, so a bounce can never travel to a peer.
"""

import datetime as dt
import hashlib
import json
import os
import secrets
import shlex
import stat
import time

from bridgelib import pmail
from bridgelib.common import (
    ConfigError,
    atomic_replace,
    checkpoint,
    destination,
    epoch_from_iso,
    exclusive_publish,
    fsync_directory,
    load_json_bytes,
    open_regular,
    utc_now,
    validate_id,
    validate_room,
)

SYSTEM_SENDER = "post-bridge"
SUBJECT_PREFIX = "Undeliverable: "
SUBJECT_MAX_BYTES = 200
# Quarantines the receiver can still clear by itself (it registers the room,
# the peer publishes the sender's name, a contest resolves). These wait this
# long, measured from the receipt's own first-written time, before bouncing.
TRANSIENT_REASONS = frozenset({"unknown_room", "unpublished_sender", "name_collision"})
DEFAULT_TRANSIENT_SECONDS = 3600

DEAD_LETTER = "dead-letter"

EXPLANATIONS = {
    "forged_self": (
        "the letter says it is from {sender!r}, but {host} already has a room "
        "of that name, so {host} will not believe the letter came from anywhere else"
    ),
    "name_collision": (
        "two hosts claim the room name {sender!r} and the sending host is not "
        "its owner of record"
    ),
    "unpublished_sender": (
        "{host} has never seen {sender!r} published by the sending host, so it "
        "cannot vouch for the name"
    ),
    "unknown_room": "{host} has no room named {room!r}",
    "remote_participant_unhomed": (
        "the letter carries a participant stamp but its sender name is not "
        "homed on the sending host"
    ),
    "id-collision": "{host} already holds a different letter with this id",
}


class BounceConflict(ConfigError):
    """A bounce record does not describe the letter being retired.

    ``path`` is the offending file. Nothing was retired or overwritten; moving
    that file aside lets the next tick redo the step it stood for.
    """

    def __init__(self, message, path=None):
        super().__init__(message)
        self.path = path


class Bounce:
    """What :func:`notify` did: ``where`` is ``participant:<id>``,
    ``room:<name>`` or ``dead-letter``; ``letter_id`` names the notice."""

    def __init__(self, where, letter_id, path):
        self.where = where
        self.letter_id = letter_id
        self.path = path


def transient_seconds():
    raw = os.environ.get("BRIDGE_BOUNCE_TRANSIENT_SECONDS")
    if raw is None:
        return DEFAULT_TRANSIENT_SECONDS
    try:
        value = int(raw)
    except ValueError:
        return DEFAULT_TRANSIENT_SECONDS
    return value if value >= 0 else DEFAULT_TRANSIENT_SECONDS


def is_terminal(receipt, now_epoch):
    """Whether ``receipt`` is a refusal the receiver will never turn into a
    delivery, so the sender should be told and the letter retired."""
    if receipt["status"] != "quarantined":
        return False
    if receipt["reason"] not in TRANSIENT_REASONS:
        return True
    written = epoch_from_iso(receipt.get("at"))
    return written is not None and now_epoch - written >= transient_seconds()


def split_letter(data):
    """``(envelope, body)`` of a mail file; an unreadable header gives ``({}, data)``."""
    separator = data.find(b"\n---\n")
    if separator < 0:
        return {}, data
    try:
        header = load_json_bytes(data[:separator], "outbox envelope")
    except ConfigError:
        header = None
    body = data[separator + len(b"\n---\n"):]
    return (header if isinstance(header, dict) else {}), body


def _bounced(settings, *parts):
    return destination(settings.root, "bridge", "bounced", *parts)


def _publish(settings, path, data):
    """Publish ``data`` at ``path`` exclusively; ``False`` when a file is there."""
    try:
        exclusive_publish(
            path, data, destination(settings.root, "bridge", "tmp"), settings.root
        )
    except FileExistsError:
        return False
    return True


def _read_optional(path, maximum):
    try:
        return open_regular(path, maximum)
    except FileNotFoundError:
        return None


def _read_found(path, maximum):
    """A file some earlier step left; one that is not an ordinary readable
    file cannot be vouched for, so it is a conflict rather than an I/O error."""
    try:
        return _read_optional(path, maximum)
    except ConfigError as error:
        raise BounceConflict(str(error), path)


def _origin_path(settings, mail_id):
    return destination(settings.root, "bridge", "origin", mail_id + ".json")


def record_origin(settings, mail_id, data, logger):
    """Remember who sent an outbound letter, as of the moment the bridge takes it.

    Called when the bridge first copies the letter toward the relay. It keeps
    the letter's sha256 (so a record can never be applied to another letter)
    and two facts about the sender: the ``from_participant`` stamp, but only
    when that participant exists here and is bound to the workspace the letter
    says it came from (a participant sends only from its own workspace, so a
    stamp that disagrees with its participant's record is not vouched for),
    and that workspace. First write wins; the record is never rewritten. A
    record that is already there and does not describe this letter is left as
    it is and logged; a bounce of the letter is then a dead letter
    (:func:`_choose_where`), and the sweep lists the record until the letter
    is retired (:func:`origin_conflict`).
    """
    envelope, _ = split_letter(data)
    workspace = envelope.get("from")
    stamp = envelope.get("from_participant")
    participant = None
    if isinstance(stamp, str):
        record = _participant_record(settings, stamp)
        if record is not None and record.get("workspace") == workspace:
            participant = stamp
    origin = {
        "v": 1,
        "id": mail_id,
        "sha256": hashlib.sha256(data).hexdigest(),
        "participant": participant,
        "workspace": workspace if isinstance(workspace, str) else None,
        "at": utc_now(),
    }
    if _publish(settings, _origin_path(settings, mail_id), _json_bytes(origin)):
        return
    try:
        _read_origin(settings, mail_id, origin["sha256"])
    except BounceConflict as error:
        logger.emit("origin_record_mismatch", id=mail_id, reason=str(error))


def _read_origin(settings, mail_id, sha256):
    """The recorded origin of this exact letter, or ``None`` when none was
    ever recorded. A record that is there but is not this letter's (unreadable,
    malformed, or written for another letter) raises :class:`BounceConflict`:
    it is never read as "no record", which would let the letter's own stamps
    stand in for a proof the bridge no longer has."""
    path = _origin_path(settings, mail_id)
    data = _read_found(path, 4096)
    if data is None:
        return None
    try:
        value = load_json_bytes(data, "letter origin")
    except ConfigError as error:
        raise BounceConflict(f"origin record for {mail_id} is unreadable: {error}", path)
    if not (
        isinstance(value, dict)
        and value.get("v") == 1
        and value.get("id") == mail_id
        and value.get("sha256") == sha256
        and (value.get("participant") is None or isinstance(value.get("participant"), str))
        and (value.get("workspace") is None or isinstance(value.get("workspace"), str))
    ):
        raise BounceConflict(
            f"origin record for {mail_id} does not describe this letter", path
        )
    return value


def origin_conflict(settings, mail_id, sha256):
    """The :class:`BounceConflict` for a letter whose origin record is not its
    own, else ``None`` (no record, or a good one). For the sweep's standing
    attention check; an I/O error is not a verdict and reads as ``None``."""
    try:
        _read_origin(settings, mail_id, sha256)
    except BounceConflict as error:
        return error
    except (ConfigError, OSError):
        return None
    return None


def remove_origin(settings, mail_id):
    """Delete a letter's origin record; ``True`` when one was removed. Only for
    a letter whose outbox entry is gone from the relay for good: nothing after
    that routes a bounce for it."""
    path = _origin_path(settings, mail_id)
    try:
        metadata = path.lstat()
    except FileNotFoundError:
        return False
    if not stat.S_ISREG(metadata.st_mode):
        return False
    path.unlink()
    fsync_directory(path.parent)
    return True


def _choose_where(settings, mail_id, sha256, envelope, real_rooms):
    """``(where, basis)``: who reads the notice, and what that rests on.

    ``basis`` is ``recorded`` when the bridge kept the sender at publish time
    (:func:`record_origin`) and ``legacy`` when it did not (a letter published
    before the record existed, including the three that were stuck when this
    landed).

    The sender is the recorded participant while it is active and still bound
    to the recorded workspace. Otherwise the recorded workspace's inbox, where
    any session in that workspace looks, provided this host really owns that
    room: a peer's room is a placeholder here, its inbox is nobody's to read,
    and a letter that names one as its sender is the very forgery being
    refused. A participant that is bound there but has neither an active
    lease nor a room to fall back on keeps the notice in its own inbox until
    its conversation resumes.

    A letter with no record is believed only as far as it can be checked now:
    its ``from_participant`` must still name the workspace in the letter's
    ``from``. Anything else is a dead letter, and so is a letter whose record
    is there but is not its own (``basis`` ``unproven``): the record is not
    overwritten and the letter's stamps are not believed in its place.
    """
    try:
        origin = _read_origin(settings, mail_id, sha256)
    except BounceConflict:
        return DEAD_LETTER, "unproven"
    if origin is not None:
        basis = "recorded"
        participant, workspace = origin["participant"], origin["workspace"]
    else:
        basis = "legacy"
        participant, workspace = envelope.get("from_participant"), envelope.get("from")
    record = None
    if isinstance(participant, str):
        record = _participant_record(settings, participant)
    bound = (
        record is not None
        and isinstance(workspace, str)
        and record.get("workspace") == workspace
    )
    if bound:
        if _participant_active(record, time.time()):
            return "participant:" + participant, basis
        if workspace in real_rooms:
            return "room:" + workspace, basis
        return "participant:" + participant, basis
    if basis == "recorded" and isinstance(workspace, str) and workspace in real_rooms:
        return "room:" + workspace, basis
    return DEAD_LETTER, basis


def _parse_rfc3339(value):
    # Python 3.9 (macOS's /usr/bin/python3) cannot read a trailing "Z".
    if isinstance(value, str) and value.endswith("Z"):
        value = value[:-1] + "+00:00"
    return epoch_from_iso(value)


def _participant_active(record, now_epoch):
    """post's own rule (participant.rs ``state``): not ended, and seen within
    its lease. A record with no readable ``last_seen`` is stale."""
    if record.get("ended_at"):
        return False
    last_seen = _parse_rfc3339(record.get("last_seen"))
    lease_hours = record.get("lease_hours", 24)
    if last_seen is None or not isinstance(lease_hours, int) or isinstance(lease_hours, bool):
        return False
    return now_epoch - last_seen <= lease_hours * 3600


def _participant_record(settings, participant):
    """The participant's record (``{}`` when it exists but is unreadable), or
    ``None`` when there is no such participant here."""
    try:
        pmail.validate_participant(participant, "from_participant")
        path = destination(settings.root, "participants", participant, "participant.json")
        data = _read_optional(path, 64 * 1024)
    except (ConfigError, OSError):
        return None
    if data is None:
        return None
    try:
        value = load_json_bytes(data, "participant record")
    except ConfigError:
        return {}
    return value if isinstance(value, dict) else {}


def _path_for(settings, where, letter_id):
    """The notice path an intent's ``where`` names."""
    validate_id(letter_id, "notice letter id")
    if where.startswith("participant:"):
        participant = where.split(":", 1)[1]
        pmail.validate_participant(participant, "from_participant")
        return destination(
            settings.root, "participants", participant, "inbox", letter_id + ".mail"
        )
    if where.startswith("room:"):
        room = where.split(":", 1)[1]
        validate_room(room, label="notice room")
        return destination(settings.root, room, "inbox", letter_id + ".mail")
    if where == DEAD_LETTER:
        return dead_letter_dir(settings) / (letter_id + ".mail")
    raise ConfigError(f"unknown notice destination {where!r}")


def dead_letter_dir(settings):
    return _bounced(settings, "undeliverable")


def _new_letter_id():
    moment = dt.datetime.now(dt.timezone.utc)
    return moment.strftime("%Y%m%d-%H%M%S-") + secrets.token_hex(3)


def _explain(reason, host, envelope):
    template = EXPLANATIONS.get(reason)
    if template is None:
        text = f"{host} refused it as {reason}"
    else:
        text = template.format(
            sender=envelope.get("from", "?"), host=host, room=envelope.get("to", "?")
        )
    return text[:1].upper() + text[1:]


def resend_command(envelope, body_path):
    """The exact command that re-sends the original text.

    The sender name is derived from the workspace the command runs in, so
    the command deliberately carries no ``--from``: a forged or renamed
    sender name is usually why the letter bounced.
    """
    parts = ["post", "send", "--to", str(envelope.get("to", "?"))]
    kind = envelope.get("kind")
    if isinstance(kind, str) and kind != "note":
        parts += ["--kind", kind]
    parts += ["--subject", str(envelope.get("subject", "")), "--body-file", str(body_path)]
    return " ".join(shlex.quote(part) for part in parts)


def _addressee(where, envelope):
    """``(to, header fields)`` of the notice a ``where`` names."""
    if where.startswith("participant:"):
        return where.split(":", 1)[1], {"address_kind": "participant"}
    if where.startswith("room:"):
        return where.split(":", 1)[1], {"address_kind": "workspace"}
    return str(envelope.get("from", SYSTEM_SENDER)), {"address_kind": "workspace"}


def _sent_stamp():
    return dt.datetime.now().astimezone().strftime("%Y-%m-%d %H:%M:%S %z")


def render_notice(mail_id, host, room, reason, envelope, body_path, letter_id, where, sent):
    """The notice's exact bytes. Every input is fixed by the intent and the
    letter (``sent`` included), so a redo renders the same bytes the first
    attempt did and the intent's ``notice_sha256`` can vouch for them."""
    original = str(envelope.get("subject", ""))
    subject = SUBJECT_PREFIX + original
    while len(subject.encode("utf-8")) > SUBJECT_MAX_BYTES:
        subject = subject[:-1]
    to, header = _addressee(where, envelope)
    envelope_out = {
        "id": letter_id,
        "from": SYSTEM_SENDER,
        "to": to,
        "kind": "note",
        "subject": subject,
        "sent": sent,
    }
    envelope_out.update(header)
    text = (
        f"Your letter was not delivered.\n"
        f"\n"
        f"  letter:     {mail_id}\n"
        f"  to:         {room} on {host}\n"
        f"  subject:    {original}\n"
        f"  refused as: {reason}\n"
        f"\n"
        f"{_explain(reason, host, envelope)}. Retrying cannot help, so the bridge has "
        f"taken the letter out of the relay instead of retrying forever.\n"
        f"\n"
        f"To send it again, run this from the workspace you send from. The sender "
        f"name comes from that workspace, so a renamed room fixes a forged-sender "
        f"refusal:\n"
        f"\n"
        f"  {resend_command(envelope, body_path)}\n"
        f"\n"
        f"The original text is saved at {body_path}.\n"
    )
    header_bytes = json.dumps(envelope_out, indent=2, ensure_ascii=True).encode("ascii")
    return header_bytes + b"\n---\n" + text.encode("utf-8")


NOTICE_MAX_BYTES = 64 * 1024


def _intent_notice(intent, envelope, body_path):
    """The notice an intent stands for, rendered from the intent and the letter."""
    return render_notice(
        intent["id"], intent["host"], intent["room"], intent["reason"], envelope,
        body_path, intent["letter_id"], intent["where"], intent["sent"],
    )


def _sealed(intent, envelope, body_path):
    """``intent`` with the sha256 of the notice it renders to (``notice_sha256``)."""
    digest = hashlib.sha256(_intent_notice(intent, envelope, body_path)).hexdigest()
    return dict(intent, notice_sha256=digest)


def notify(settings, host, room, mail_id, data, receipt, logger, real_rooms):
    """Write the sender's notice for one refused outbox letter; idempotent.

    Returns a :class:`Bounce`. Raises ``ConfigError``/``OSError`` when the
    notice cannot be written, and :class:`BounceConflict` (a ``ConfigError``)
    when a record from an earlier attempt does not describe this letter, or a
    marker says a notice was written and it is not there; the caller then
    keeps the outbox entry and tries again next tick.
    """
    envelope, body = split_letter(data)
    sha256 = hashlib.sha256(data).hexdigest()
    intent_path = _bounced(settings, mail_id + ".json")
    sent_path = _bounced(settings, mail_id + ".sent")
    body_path = _bounced(settings, mail_id + ".body")
    existing = _read_found(intent_path, 4096)
    decided_now = existing is None
    if decided_now:
        where, basis = _choose_where(settings, mail_id, sha256, envelope, real_rooms)
        intent = _sealed(
            {
                "v": 1,
                "id": mail_id,
                "sha256": sha256,
                "letter_id": _new_letter_id(),
                "where": where,
                "basis": basis,
                "host": host,
                "room": room,
                "reason": receipt["reason"],
                "sent": _sent_stamp(),
                "at": utc_now(),
            },
            envelope,
            body_path,
        )
        _publish(settings, intent_path, _json_bytes(intent))
        checkpoint("bounce-b1-intent")
    else:
        intent = _load_intent(existing, intent_path, mail_id, sha256, host, room)
        # The digest the intent carries must be the digest of the notice this
        # letter renders to: what a notice is later compared with is then
        # known to come from this letter.
        if _sealed(intent, envelope, body_path)["notice_sha256"] != intent["notice_sha256"]:
            raise BounceConflict(
                f"bounce intent for {mail_id} does not describe the notice "
                f"this letter renders to",
                intent_path,
            )
    where = intent["where"]
    sent = _read_sent(sent_path, mail_id, sha256, intent)
    # The notice at the intent's own path comes first: if it is there, the
    # bounce was already published, and completing it is the only correct
    # move whatever the world looks like now.
    notice_path, notice = _find_notice(settings, intent_path, intent, mail_id)
    if sent is not None and notice is None:
        # The marker only records that the notice was written; the notice is
        # the proof. Without it the letter would be retired with nobody told.
        raise BounceConflict(
            f"the sent marker for {mail_id} says its notice was written, but "
            f"there is no notice at {notice_path}",
            sent_path,
        )
    if sent is None and notice is None and not decided_now:
        # Nothing was published yet, so nothing pins the destination: choose
        # again from the world as it is now, rather than write into a
        # participant or room that has gone.
        fresh, basis = _choose_where(settings, mail_id, sha256, envelope, real_rooms)
        if fresh != where:
            where = fresh
            intent = _sealed(dict(intent, where=where, basis=basis), envelope, body_path)
            atomic_replace(intent_path, _json_bytes(intent), settings.root)
            notice_path, notice = _find_notice(settings, intent_path, intent, mail_id)
    if not _publish(settings, body_path, body):
        if _read_found(body_path, len(body) + 1) != body:
            raise BounceConflict(
                f"the saved body for {mail_id} is not the letter's body", body_path
            )
    checkpoint("bounce-b2-body")
    if sent is None:
        if notice is None:
            rendered = _intent_notice(intent, envelope, body_path)
            if not _publish(settings, notice_path, rendered):
                # A file appeared since the read above: it must be ours.
                _find_notice(settings, intent_path, intent, mail_id, required=True)
        checkpoint("bounce-b3-letter")
        marker = {
            "v": 1,
            "id": mail_id,
            "sha256": sha256,
            "letter_id": intent["letter_id"],
            "where": where,
            "at": utc_now(),
        }
        _publish(settings, sent_path, _json_bytes(marker))
        logger.emit(
            "letter_bounced", id=mail_id, host=host, room=room,
            reason=receipt["reason"], where=where, notice=intent["letter_id"],
        )
        checkpoint("bounce-b4-sent")
    return Bounce(where, intent["letter_id"], notice_path)


def _find_notice(settings, intent_path, intent, mail_id, required=False):
    """``(path, data)`` of the notice at the intent's destination; ``data`` is
    ``None`` when nothing is there. A file that is there must be the notice
    the intent sealed, byte for byte (:class:`BounceConflict` otherwise), so a
    file is never accepted on sight or on a few matching lines."""
    try:
        path = _path_for(settings, intent["where"], intent["letter_id"])
    except ConfigError as error:
        raise BounceConflict(f"bounce intent for {mail_id}: {error}", intent_path)
    data = _read_found(path, NOTICE_MAX_BYTES)
    if data is None and required:
        raise BounceConflict(f"the notice for {mail_id} could not be written", path)
    if data is not None and hashlib.sha256(data).hexdigest() != intent["notice_sha256"]:
        raise BounceConflict(
            f"the file at the notice path is not the notice for {mail_id}", path
        )
    return path, data


def _load_intent(existing, intent_path, mail_id, sha256, host, room):
    """The intent an earlier attempt left, if it describes this letter."""
    try:
        intent = load_json_bytes(existing, "bounce intent")
    except ConfigError as error:
        raise BounceConflict(
            f"bounce intent for {mail_id} is unreadable: {error}", intent_path
        )
    if (
        not isinstance(intent, dict)
        or intent.get("id") != mail_id
        or intent.get("sha256") != sha256
        or intent.get("host") != host
        or intent.get("room") != room
        or not all(
            isinstance(intent.get(key), str)
            for key in ("letter_id", "where", "reason", "sent", "notice_sha256")
        )
    ):
        raise BounceConflict(
            f"bounce intent for {mail_id} does not describe this letter", intent_path
        )
    return intent


def _read_sent(sent_path, mail_id, sha256, intent):
    """The sent marker when one is there and agrees with the letter and the
    intent; ``None`` when there is none yet."""
    data = _read_found(sent_path, 4096)
    if data is None:
        return None
    try:
        value = load_json_bytes(data, "bounce sent marker")
    except ConfigError as error:
        raise BounceConflict(
            f"sent marker for {mail_id} is unreadable: {error}", sent_path
        )
    if (
        not isinstance(value, dict)
        or value.get("v") != 1
        or value.get("id") != mail_id
        or value.get("sha256") != sha256
        or value.get("letter_id") != intent["letter_id"]
        or value.get("where") != intent["where"]
    ):
        raise BounceConflict(
            f"sent marker for {mail_id} does not describe this bounce", sent_path
        )
    return value


def dead_letter_intents(settings, notice_ids):
    """``{notice id: intent}`` for the dead-letter notices in ``notice_ids``,
    read from the intents that wrote them (the notice's own id is not the
    refused letter's)."""
    wanted = set(notice_ids)
    found = {}
    directory = _bounced(settings)
    try:
        names = sorted(
            entry.name for entry in os.scandir(str(directory))
            if entry.name.endswith(".json")
        )
    except (FileNotFoundError, NotADirectoryError):
        return found
    for name in names:
        if len(found) == len(wanted):
            break
        try:
            data = _read_optional(directory / name, 4096)
            intent = load_json_bytes(data or b"", "bounce intent")
        except (ConfigError, OSError):
            continue
        if (
            isinstance(intent, dict)
            and intent.get("where") == DEAD_LETTER
            and intent.get("letter_id") in wanted
            and intent.get("id") == name[: -len(".json")]
        ):
            found[intent["letter_id"]] = intent
    return found


def _json_bytes(value):
    return (json.dumps(value, sort_keys=True) + "\n").encode("utf-8")
