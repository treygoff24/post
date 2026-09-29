"""Bounce a terminally refused outbound letter back to its sender.

A receiver that will never deliver a letter writes a ``quarantined`` receipt.
Until this module the sender kept that letter in its relay outbox forever:
nothing told the agent who wrote it, and nothing retired the entry. Now the
sender's bridge, on seeing a terminal refusal receipt, writes one system
letter to the *sending participant's* inbox (or, when that participant is no
longer active, the inbox of the room it worked in; failing both, a
dead-letter file the attention list points at), records that it did, and only
then lets the caller retire the outbox entry.

Crash safety. Everything is idempotent under a hard kill between any two
steps, and no step is ever skipped:

1. ``bridge/bounced/<id>.json`` (the intent) is published first. It fixes the
   notice's letter id and its destination, so a redo writes the same file to
   the same place rather than a second notice.
2. ``bridge/bounced/<id>.body`` keeps the original body, so the re-send
   command in the notice names a file that exists.
3. The notice is published with an exclusive link (``FileExistsError`` means
   an earlier attempt already wrote it).
4. ``bridge/bounced/<id>.sent`` records that the notice exists, so a redo
   after step 3 does not write it again or log it twice. (post 0.9.0 never
   moves mail out of an inbox, so a notice the participant has read is still
   there for the exclusive link in step 3 to find.)

The caller retires the outbox entry (``git rm``) only after :func:`notify`
returns, and the receiver-side entry vanishing on the next fetch is the end
of the letter's life. The notice never enters ``archive/``: nothing exports
from a participant or room inbox, so a bounce can never travel to a peer.
"""

import datetime as dt
import json
import os
import secrets
import shlex
import time

from bridgelib import pmail
from bridgelib.common import (
    ConfigError,
    atomic_replace,
    checkpoint,
    destination,
    epoch_from_iso,
    exclusive_publish,
    load_json_bytes,
    open_regular,
    utc_now,
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


def _choose_where(settings, envelope, real_rooms):
    """Who can read the notice: the sending participant's inbox while that
    participant is active, else the inbox of the room it was working in, else
    the letter's own sending room, else (a participant that exists but has no
    room here) its own inbox, else a dead-letter file.

    A participant whose session ended or whose lease lapsed reads nothing
    until its conversation resumes, but any session in its workspace sees the
    workspace's inbox, so that is where the notice goes.

    Only a room this host really owns qualifies. A peer's room is a
    placeholder here, its inbox is nobody's to read, and a letter that names
    one as its sender is the very forgery being refused."""
    participant = envelope.get("from_participant")
    record = None
    if isinstance(participant, str):
        record = _participant_record(settings, participant)
    if record is not None and _participant_active(record, time.time()):
        return "participant:" + participant
    for room in (record.get("workspace") if record else None, envelope.get("from")):
        if isinstance(room, str) and room in real_rooms:
            return "room:" + room
    if record is not None:
        return "participant:" + participant
    return DEAD_LETTER


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


def _participant_exists(settings, participant):
    return _participant_record(settings, participant) is not None


def _still_valid(settings, where, real_rooms):
    if where.startswith("participant:"):
        return _participant_exists(settings, where.split(":", 1)[1])
    if where.startswith("room:"):
        room = where.split(":", 1)[1]
        return room in real_rooms
    return where == DEAD_LETTER


def _path_for(settings, where, letter_id):
    """The notice path an intent's ``where`` names."""
    if where.startswith("participant:"):
        participant = where.split(":", 1)[1]
        pmail.validate_participant(participant, "from_participant")
        return destination(
            settings.root, "participants", participant, "inbox", letter_id + ".mail"
        )
    if where.startswith("room:"):
        room = where.split(":", 1)[1]
        return destination(settings.root, room, "inbox", letter_id + ".mail")
    return dead_letter_dir(settings) / (letter_id + ".mail")


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


def render_notice(mail_id, host, room, receipt, envelope, body_path, letter_id, where):
    reason = receipt["reason"]
    original = str(envelope.get("subject", ""))
    subject = SUBJECT_PREFIX + original
    while len(subject.encode("utf-8")) > SUBJECT_MAX_BYTES:
        subject = subject[:-1]
    now = dt.datetime.now().astimezone()
    if where.startswith("participant:"):
        to = where.split(":", 1)[1]
        header = {"address_kind": "participant"}
    elif where.startswith("room:"):
        to = where.split(":", 1)[1]
        header = {"address_kind": "workspace"}
    else:
        to = str(envelope.get("from", SYSTEM_SENDER))
        header = {"address_kind": "workspace"}
    envelope_out = {
        "id": letter_id,
        "from": SYSTEM_SENDER,
        "to": to,
        "kind": "note",
        "subject": subject,
        "sent": now.strftime("%Y-%m-%d %H:%M:%S %z"),
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


def notify(settings, host, room, mail_id, data, receipt, logger, real_rooms):
    """Write the sender's notice for one refused outbox letter; idempotent.

    Returns a :class:`Bounce`. Raises ``ConfigError``/``OSError`` when the
    notice cannot be written; the caller then keeps the outbox entry and
    tries again next tick.
    """
    envelope, body = split_letter(data)
    intent_path = _bounced(settings, mail_id + ".json")
    sent_path = _bounced(settings, mail_id + ".sent")
    existing = _read_optional(intent_path, 4096)
    if existing is None:
        intent = {
            "v": 1,
            "id": mail_id,
            "letter_id": _new_letter_id(),
            "where": _choose_where(settings, envelope, real_rooms),
            "host": host,
            "room": room,
            "reason": receipt["reason"],
            "at": utc_now(),
        }
        _publish(settings, intent_path, _intent_bytes(intent))
        checkpoint("bounce-b1-intent")
    else:
        try:
            intent = load_json_bytes(existing, "bounce intent")
        except ConfigError:
            intent = None
        if not isinstance(intent, dict) or not all(
            isinstance(intent.get(key), str) for key in ("letter_id", "where")
        ):
            raise ConfigError(f"bounce intent for {mail_id} is unreadable")
    letter_id = intent["letter_id"]
    where = intent["where"]
    if not sent_path.exists() and not _still_valid(settings, where, real_rooms):
        # The participant or room the intent named is gone since the crash
        # that left it. The notice has not been written yet, so re-decide
        # rather than recreate a half participant directory.
        where = _choose_where(settings, envelope, real_rooms)
        intent = dict(intent, where=where)
        atomic_replace(intent_path, _intent_bytes(intent), settings.root)
    notice_path = _path_for(settings, where, letter_id)
    body_path = _bounced(settings, mail_id + ".body")
    _publish(settings, body_path, body)
    checkpoint("bounce-b2-body")
    if not sent_path.exists():
        notice = render_notice(
            mail_id, host, room, receipt, envelope, body_path, letter_id, where
        )
        _publish(settings, notice_path, notice)
        checkpoint("bounce-b3-letter")
        _publish(settings, sent_path, (utc_now() + "\n").encode("ascii"))
        logger.emit(
            "letter_bounced", id=mail_id, host=host, room=room,
            reason=receipt["reason"], where=where, notice=letter_id,
        )
        checkpoint("bounce-b4-sent")
    return Bounce(where, letter_id, notice_path)


def _intent_bytes(intent):
    return (json.dumps(intent, sort_keys=True) + "\n").encode("utf-8")
