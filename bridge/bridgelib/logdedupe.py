"""Per-letter log dedupe: a standing condition is logged when it appears or
changes, not on every full tick (papercut pc2_7b180099776a256e).

A condition is keyed by (action, host, room, id) — ``path`` stands in for a
missing id — and its value is the line's remaining fields minus the ones that
move every tick (``age_seconds``). The last-logged state lives in one small
file under ``bridge/``. The dedupe is best effort by construction: an
unreadable or damaged state file means "log as before and rebuild it", and a
failed save only costs repeated lines next tick. It never fails a tick.
"""

import json

from bridgelib.common import ConfigError, atomic_replace, load_json_bytes, open_regular

# Per-letter emitters in sweep.py that repeat unchanged every full tick while
# the letter stands. pmail and channels already log their own once.
# The set is matched by action name inside Logger.emit, so it applies to
# every emitter that uses one of these names during a full tick, wherever it
# lives: pmail's parse already emits unknown_envelope_keys and shares its
# key space (same meaning, same mail ids). A new emitter that reuses a name
# here inherits the dedupe; pick a distinct name unless that is intended.
CONDITION_ACTIONS = frozenset(
    {
        "quarantined",
        "forensic",
        "quarantined_path",
        "held",
        "outbound_ignored",
        "outbound_waiting",
        "route_contested",
        "receipt_ignored",
        "unknown_envelope_keys",
    }
)
KEY_FIELDS = ("host", "room", "id")
VOLATILE_FIELDS = frozenset({"age_seconds"})
STATE_VERSION = 1
# The byte cap is the binding limit, and settle writes to the same cap _load
# reads. An entry serializes as its key (action, host, room and a 22-character
# id: about 60 bytes; a path key can reach several KiB) plus its value (a
# reason sentence or an absolute path: 30-300 bytes), so a typical entry is
# 150-400 bytes and 2000 of them (0.3-0.8 MB) can pass 512 KiB. settle
# therefore drops entries by serialized size until the file fits; the entry
# cap only bounds the work. A dropped entry costs one repeated line when it
# is next seen.
MAX_STATE_BYTES = 512 * 1024
MAX_ENTRIES = 2000


def condition_key(action, fields):
    parts = [action] + [fields.get(name) for name in KEY_FIELDS]
    if fields.get("id") is None:
        parts.append(fields.get("path"))
    return json.dumps(parts, separators=(",", ":"))


def valid_key(key):
    """A stored key is what condition_key writes, or the state is damaged."""
    # A key is itself JSON text that the file's own depth check skipped (it
    # is a string there): any failure to parse it, RecursionError included,
    # is damage.
    try:
        parts = json.loads(key)
    except (TypeError, ValueError, RecursionError):
        return False
    return (
        isinstance(parts, list)
        and len(parts) in (4, 5)
        # Checked first: an unhashable first part ([] or {}) would raise
        # TypeError at the membership test and fail every tick (review 6).
        and isinstance(parts[0], str)
        and parts[0] in CONDITION_ACTIONS
        and all(part is None or isinstance(part, str) for part in parts[1:])
        and condition_key(parts[0], dict(zip(KEY_FIELDS + ("path",), parts[1:])))
        == key
    )


def condition_value(fields):
    skip = set(KEY_FIELDS) | VOLATILE_FIELDS
    if fields.get("id") is None:
        skip.add("path")
    return {name: value for name, value in fields.items() if name not in skip}


def serialize_state(state):
    payload = {"v": STATE_VERSION, "conditions": state}
    return (json.dumps(payload, sort_keys=True) + "\n").encode("utf-8")


def fit_state(state, seen):
    """At most MAX_ENTRIES entries whose file fits in MAX_STATE_BYTES.

    The state has no ages, so "oldest" is approximated: this tick's
    conditions are kept before ones carried over from earlier ticks, each
    group in key order, and entries are dropped from the end of that order.
    """
    if len(state) <= MAX_ENTRIES and len(serialize_state(state)) <= MAX_STATE_BYTES:
        return state
    ordered = sorted(key for key in state if key in seen)
    ordered += sorted(key for key in state if key not in seen)
    budget = MAX_STATE_BYTES - len(serialize_state({}))
    kept = {}
    for key in ordered[:MAX_ENTRIES]:
        # json.dumps with its default separators: '<key>: <value>' joined
        # by ', ', so each entry costs its text plus two bytes.
        size = len(
            (json.dumps(key) + ": " + json.dumps(state[key], sort_keys=True)).encode(
                "utf-8"
            )
        ) + 2
        if size > budget:
            break
        budget -= size
        kept[key] = state[key]
    if len(serialize_state(kept)) > MAX_STATE_BYTES:  # arithmetic backstop
        return {}
    return kept


class ConditionLog:
    def __init__(self, path, root):
        self.path = path
        self.root = root
        self.prior = self._load()
        self.seen = {}
        self.unread_hosts = set()

    def host_unread(self, host):
        """This tick could not read ``host``: its conditions neither prune nor clear."""
        self.unread_hosts.add(host)

    def _load(self):
        try:
            value = load_json_bytes(
                open_regular(self.path, MAX_STATE_BYTES), "log dedupe state"
            )
        except (FileNotFoundError, ConfigError, OSError, ValueError):
            return {}
        # type() is int, not isinstance: True == 1, and a bool version is
        # damage (the guard's round-4 rule).
        if not isinstance(value, dict) or not (
            type(value.get("v")) is int and value["v"] == STATE_VERSION
        ):
            return {}
        conditions = value.get("conditions")
        if not isinstance(conditions, dict):
            return {}
        # Any entry of the wrong shape means the file is damaged as a whole:
        # log as before and rebuild, never trust part of it (settle unpacks
        # every key).
        for key, item in conditions.items():
            if not valid_key(key) or not isinstance(item, dict):
                return {}
        return conditions

    def should_log(self, action, fields):
        """Record the condition as standing; True when it is new or changed."""
        if action not in CONDITION_ACTIONS:
            return True
        try:
            key = condition_key(action, fields)
            value = condition_value(fields)
            # Round-trip so a comparison with the loaded state is like for like.
            value = json.loads(json.dumps(value, sort_keys=True))
        except (TypeError, ValueError):
            return True
        last = self.seen.get(key, self.prior.get(key))
        self.seen[key] = value
        return last != value

    def settle(self, prune):
        """Persist the state; return (cleared, standing counts per action).

        ``prune`` is true only for a tick that walked every letter source, so
        a condition missing from it has really cleared. Otherwise this tick's
        conditions are merged over the prior ones and nothing clears.
        """
        cleared = []
        if prune:
            state = dict(self.seen)
            for key, value in self.prior.items():
                if key not in state and json.loads(key)[1] in self.unread_hosts:
                    state[key] = value
            cleared = sorted(set(self.prior) - set(state))
        else:
            state = dict(self.prior)
            state.update(self.seen)
        state = fit_state(state, self.seen)
        if state != self.prior:
            try:
                atomic_replace(self.path, serialize_state(state), self.root)
            except (ConfigError, OSError, ValueError):
                pass
        standing = {}
        for key in self.seen:
            action = json.loads(key)[0]
            standing[action] = standing.get(action, 0) + 1
        return [json.loads(key) for key in cleared], standing
