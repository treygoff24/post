"""health.json ``attention``: what is stuck and needs an agent or a human.

``ok`` stays a liveness flag: the bridge is running, fetching and pushing.
``attention`` is the separate "something is stuck" list, empty when nothing
needs anyone. Each item is ``{"kind", "id", "summary", "fix"}``; ``fix`` is
an exact command or a one-sentence instruction, never "investigate".

Kinds:

``refused_letter``
    A letter this host sent was refused for good and its sender could not be
    told (the notice is a dead letter; the item's id is the refused letter's),
    the bounce failed this tick, or a record of an earlier bounce attempt
    cannot be confirmed (it does not match the letter, or the notice it says
    was written is gone) and the entry is kept. (When the bounce works the
    sender gets a system letter and the entry is retired, so nothing stays
    here.)
``sender_record_mismatch``
    The bridge's record of who sent a letter still in the relay does not
    describe that letter. It is left alone; if the letter is refused, its
    notice is a dead letter. Leaves when the letter is retired.
``unrelayable_letter``
    A workspace letter in ``archive/`` that cannot be relayed at all.
``quarantined_inbound``
    A peer's letter this host refused and holds in quarantine. Cleared when
    the sending host retires it (its bounce does that on its next tick).
``archived_participant``
    Participant mail is waiting for a participant whose record
    ``post participant gc`` moved to ``participants-archive/``. Cleared when
    the record is back and the letters are delivered.
``name_collision``
    Two hosts claim one room name.
"""

MAX_ITEMS = 50
MAX_UNRELAYABLE = 20
MAX_TEXT = 600

REFUSAL_FIXES = {
    "forged_self": (
        "Nothing to do on this host. The sending host's bridge withdraws the letter "
        "and tells its sender on its next tick; a sender that has not installed "
        "this bridge version yet must install it (bridge/install.sh)."
    ),
    "name_collision": (
        "Resolve the room-name collision listed under name_collision; the sending "
        "host's bridge withdraws the letter after an hour if it stays refused."
    ),
    "unpublished_sender": (
        "Nothing to do on this host: the sending host's bridge withdraws the letter "
        "and tells its sender after an hour if it stays refused."
    ),
}
DEFAULT_REFUSAL_FIX = (
    "Nothing to do on this host. The sending host's bridge withdraws the letter and "
    "tells its sender on its next tick; the copy kept here is forensic only."
)


def _text(value):
    text = str(value)
    return text if len(text) <= MAX_TEXT else text[: MAX_TEXT - 3] + "..."


def item(kind, ident, summary, fix):
    return {
        "kind": kind,
        "id": None if ident is None else _text(ident),
        "summary": _text(summary),
        "fix": _text(fix),
    }


def refused_dead_letter(mail_id, path, basis=None):
    reason = (
        "the bridge's record of who sent it does not describe the letter"
        if basis == "unproven"
        else "no participant or room here can be shown to have sent it"
    )
    return item(
        "refused_letter",
        mail_id,
        f"Letter {mail_id}, which this host sent, was refused for good, and the "
        f"bridge could not tell its sender: {reason}. "
        "The notice is saved at " + str(path) + ".",
        f"Read it with: cat '{path}'. Re-send the letter if it still matters "
        f"(the notice has the command), then delete the notice with: rm '{path}'",
    )


def refused_bounce_failed(mail_id, host, room, reason, error):
    return item(
        "refused_letter",
        mail_id,
        f"Letter {mail_id} to {room} on {host} was refused ({reason}) but the "
        f"bridge could not write the notice to its sender: {error}",
        "The bridge retries every tick. If this stays, check that the sending "
        "participant's inbox under the mail root is writable and the disk is not full.",
    )


def refused_bounce_conflict(mail_id, host, room, reason, error):
    """A bounce record cannot be confirmed against the letter; the entry stays
    in the relay."""
    path = getattr(error, "path", None)
    fix = (
        f"Move the file named above aside and the next tick redoes that step: "
        f"mv '{path}' '{path}.conflict'"
        if path is not None
        else "Look under bridge/bounced/ in the mail root for the file named above."
    )
    return item(
        "refused_letter",
        mail_id,
        f"Letter {mail_id} to {room} on {host} was refused ({reason}) but the "
        f"bridge cannot confirm the record of its bounce, so it kept the letter "
        f"in the relay and retired nothing: {error}",
        fix,
    )


def sender_record_mismatch(mail_id, host, room, error):
    """The origin record of a letter in the relay is not that letter's."""
    path = getattr(error, "path", None)
    fix = (
        f"Nothing is needed unless the letter is refused. To drop the bad "
        f"record (a refusal is then judged by the letter's own stamps): rm '{path}'"
        if path is not None
        else "Nothing is needed unless the letter is refused."
    )
    return item(
        "sender_record_mismatch",
        mail_id,
        f"Letter {mail_id} to {room} on {host} is in the relay, but the bridge's "
        f"record of who sent it does not describe it ({error}). The bridge left "
        f"the record as it is. If the letter is refused, its notice will be a "
        f"dead letter, because the bridge cannot show who sent it.",
        fix,
    )


def unrelayable(mail_id, reason, archive_path, retired_dir):
    return item(
        "unrelayable_letter",
        mail_id,
        f"Archive letter {mail_id} cannot be relayed: {reason}.",
        f"Fix its envelope, or retire it (the bridge stops reporting it once the "
        f"file leaves the archive) with: mkdir -p '{retired_dir}' && "
        f"mv '{archive_path}' '{retired_dir}/'",
    )


def unrelayable_overflow(count):
    return item(
        "unrelayable_letter",
        None,
        f"{count} more archive letters cannot be relayed (only the first "
        f"{MAX_UNRELAYABLE} are listed).",
        "Retire the listed letters; the next tick lists the following ones.",
    )


def quarantined_inbound(host, room, mail_id, reason, forensic_path):
    fix = REFUSAL_FIXES.get(reason, DEFAULT_REFUSAL_FIX)
    if reason == "unknown_room":
        fix = (
            f"Register the room here with: post rooms add {room} <path>, or ask the "
            f"sender on {host} to address a room that exists here. The sending "
            f"host's bridge withdraws the letter after an hour if it stays refused."
        )
    where = f" Forensic copy: {forensic_path}." if forensic_path else ""
    return item(
        "quarantined_inbound",
        mail_id,
        f"Letter {mail_id or '?'} from {host} to {room} was refused ({reason}).{where}",
        fix,
    )


def archived_participant(participant, letters, archive_dir):
    """Letters waiting for a participant whose record ``post participant gc`` archived.

    ``letters`` is ``[(host, mail_id), ...]``. ``post bridge deliver`` restores
    an archived recipient itself; this item only stands while it still refuses,
    and the fix is post's own ``participant restore``.
    """
    ids = ", ".join(mail_id for _, mail_id in letters[:3])
    more = f" and {len(letters) - 3} more" if len(letters) > 3 else ""
    hosts = ", ".join(sorted({host for host, _ in letters}))
    return item(
        "archived_participant",
        participant,
        f"{len(letters)} letter(s) from {hosts} for participant {participant} "
        f"are waiting ({ids}{more}): `post participant gc` moved that record to "
        f"{archive_dir} and post's delivery would not restore it, so it would "
        f"reject the letters for good. The bridge holds them instead.",
        f"Restore the record and the next tick delivers them: "
        f"post participant restore {participant}",
    )


def name_collision(collision, self_host):
    room = collision.get("room")
    owner = collision.get("owner")
    claimants = collision.get("claimants") or []
    others = [host for host in claimants if host != owner]
    if owner is None:
        fix = (
            f"One host has to give the name up. On the host that should not keep it, "
            f"run: post rooms rename {room} {room}-<host>"
        )
    elif owner == self_host:
        fix = (
            f"This host owns {room}. On each other claimant ({', '.join(others)}) "
            f"run: post rooms rename {room} {room}-<host>"
        )
    else:
        fix = (
            f"{owner} owns {room}. On each other claimant ({', '.join(others)}) "
            f"run: post rooms rename {room} {room}-<host>"
        )
    return item(
        "name_collision",
        room,
        f"Room name {room!r} is claimed by {', '.join(claimants)}"
        + (f" (owner of record: {owner})." if owner else " (no owner of record)."),
        fix,
    )


def bounded(value):
    """A carried ``attention`` list reduced to well-formed items."""
    if not isinstance(value, list):
        return []
    kept = []
    for entry in value:
        if (
            isinstance(entry, dict)
            and isinstance(entry.get("kind"), str)
            and (entry.get("id") is None or isinstance(entry.get("id"), str))
            and isinstance(entry.get("summary"), str)
            and isinstance(entry.get("fix"), str)
        ):
            kept.append(
                {
                    "kind": entry["kind"][:64],
                    "id": entry["id"],
                    "summary": _text(entry["summary"]),
                    "fix": _text(entry["fix"]),
                }
            )
    return kept[:MAX_ITEMS]


def assemble(groups):
    """Concatenate item groups, de-duplicated by (kind, id, summary) and capped."""
    seen = set()
    result = []
    for group in groups:
        for entry in group:
            key = (entry["kind"], entry["id"], entry["summary"])
            if key in seen:
                continue
            seen.add(key)
            result.append(entry)
    return result[:MAX_ITEMS]
