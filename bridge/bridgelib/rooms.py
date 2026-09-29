"""Room registry, ownership, and topology primitives for post-bridge v2."""

import json
import os
import subprocess
from pathlib import Path
from typing import Dict, FrozenSet, List, Mapping, Optional, Tuple

from . import common
from .snapshot import Contest, LOCAL, Owner, Snapshot


REGISTRY_MAX_BYTES = 4 * 1024
ROOMS_MAX_BYTES = 64 * 1024
REGISTRY_KEYS = {"v", "hosts"}
ROOMS_KEYS = {"v", "host", "rooms"}


class _FoldedName(str):
    """A folded comparison key that retains its first-seen display spelling."""

    def __new__(cls, value):
        normalized = common.normalize_name(value)
        instance = super().__new__(cls, common.fold_name(normalized))
        instance.display = normalized
        return instance


def _display_name(value):
    return getattr(value, "display", str(value))


def _state_path(settings, *parts):
    return common.destination(settings.root, "bridge", *parts)


def _json_bytes(value):
    return (json.dumps(value, sort_keys=True) + "\n").encode("utf-8")


def _tree_blob(git, oid, path, maximum):
    if not oid:
        return None, None
    entries = [entry for entry in git.ls_tree(oid, path) if entry.get("path") == path]
    if len(entries) != 1:
        return None, None
    entry = entries[0]
    blob_oid = entry.get("object")
    if (
        entry.get("mode") != "100644"
        or entry.get("type") != "blob"
        or not isinstance(entry.get("size"), int)
        or entry["size"] > maximum
    ):
        return blob_oid, None
    data = git.show(oid, path)
    if data is None or len(data) != entry["size"] or len(data) > maximum:
        return blob_oid, None
    return blob_oid, data


def _read_persisted(path, maximum):
    try:
        return common.open_regular(path, maximum)
    except FileNotFoundError:
        return None


def _log_invalid_once(settings, marker, identity, logger, action, **fields):
    identity = identity or "missing"
    path = _state_path(settings, *marker)
    prior = _read_persisted(path, ROOMS_MAX_BYTES)
    seen = set()
    if prior is not None:
        try:
            decoded = common.load_json_bytes(prior, str(path))
        except common.ConfigError:
            decoded = None
        if isinstance(decoded, list) and all(
            isinstance(item, str) for item in decoded
        ):
            seen.update(decoded)
        else:
            legacy = prior.decode("ascii", "replace").strip()
            if legacy:
                seen.add(legacy)
    if identity in seen:
        return
    logger.emit(action, oid=None if identity == "missing" else identity, **fields)
    seen.add(identity)
    common.atomic_replace(path, _json_bytes(sorted(seen)), settings.root)


def _validate_registry(data):
    value = common.load_json_bytes(data, "hosts.json")
    if not isinstance(value, dict) or set(value) != REGISTRY_KEYS or value["v"] != 1:
        raise common.ConfigError("hosts.json must contain exactly v=1 and hosts")
    hosts = value["hosts"]
    if not isinstance(hosts, list) or len(hosts) > 64:
        raise common.ConfigError("hosts.json hosts must be a list of at most 64 entries")
    seen = set()
    for host in hosts:
        common.validate_host(host, "registry host")
        if host in seen:
            raise common.ConfigError(f"duplicate registry host {host!r}")
        seen.add(host)
    return list(hosts)


def read_registry(settings, git, oid, logger) -> Optional[List[str]]:
    """Read and persist the pinned registry, falling back to its last valid copy."""
    blob_oid, data = _tree_blob(git, oid, "hosts.json", REGISTRY_MAX_BYTES)
    try:
        if data is None:
            raise common.ConfigError("hosts.json is missing or not a bounded regular blob")
        hosts = _validate_registry(data)
    except common.ConfigError:
        _log_invalid_once(
            settings,
            ("registry", "invalid-oid"),
            blob_oid or oid,
            logger,
            "registry_invalid",
        )
        persisted = _read_persisted(
            _state_path(settings, "registry", "hosts.json"), REGISTRY_MAX_BYTES
        )
        if persisted is None:
            return None
        try:
            return _validate_registry(persisted)
        except common.ConfigError:
            return None
    common.atomic_replace(
        _state_path(settings, "registry", "hosts.json"),
        _json_bytes({"v": 1, "hosts": hosts}),
        settings.root,
    )
    return hosts


def effective_peers(self_host, registry_hosts, config_peers, logger) -> List[str]:
    """Return the sorted registry/config intersection, excluding the local host."""
    configured = set(config_peers)
    if registry_hosts is None:
        return sorted(configured - {self_host})
    registered = set(registry_hosts)
    for host in sorted(configured - registered - {self_host}):
        logger.emit("peer_unregistered", host=host)
    candidates = registered if not configured else registered & configured
    return sorted(candidates - {self_host})


def _validate_rooms(data, host):
    value = common.load_json_bytes(data, f"rooms.json for {host}")
    if not isinstance(value, dict) or set(value) != ROOMS_KEYS:
        raise common.ConfigError("rooms.json has unknown or missing keys")
    if value["v"] != 1 or value["host"] != host:
        raise common.ConfigError("rooms.json version or host binding is invalid")
    rooms = value["rooms"]
    if not isinstance(rooms, list) or len(rooms) > 1024:
        raise common.ConfigError("rooms.json rooms must be a list of at most 1024 entries")
    seen = set()
    result = []
    for room in rooms:
        common.validate_room(room, topology=True, label="published room")
        room = common.normalize_name(room)
        folded = common.fold_name(room)
        if folded in seen:
            raise common.ConfigError("rooms.json has a case-fold duplicate")
        seen.add(folded)
        result.append(room)
    return result


def read_peer_rooms(settings, git, host, oid, logger) -> FrozenSet[str]:
    """Read one peer's pinned rooms publication, retaining its last valid set."""
    try:
        blob_oid, data = _tree_blob(git, oid, "rooms.json", ROOMS_MAX_BYTES)
    except common.GitReadError as error:
        # m6: keep the last valid set; the failure is logged and in health.
        common.note_git_failure(git, logger, host, error)
        blob_oid, data = None, None
    try:
        if data is None:
            raise common.ConfigError("rooms.json is missing or not a bounded regular blob")
        rooms = _validate_rooms(data, host)
    except common.ConfigError:
        _log_invalid_once(
            settings,
            ("rooms", f"{host}.invalid-oid"),
            blob_oid or oid,
            logger,
            "rooms_invalid",
            host=host,
        )
        persisted = _read_persisted(
            _state_path(settings, "rooms", "peers", f"{host}.json"),
            ROOMS_MAX_BYTES,
        )
        if persisted is None:
            return frozenset()
        try:
            return frozenset(_validate_rooms(persisted, host))
        except common.ConfigError:
            return frozenset()
    common.atomic_replace(
        _state_path(settings, "rooms", "peers", f"{host}.json"),
        _json_bytes({"v": 1, "host": host, "rooms": sorted(rooms)}),
        settings.root,
    )
    return frozenset(rooms)


def _decode_owner(room, record, owners):
    common.validate_room(room, topology=True, label="owned room")
    if not isinstance(record, dict) or set(record) != {"host", "first_seen"}:
        raise common.ConfigError("owners.json entries require host and first_seen")
    host = record["host"]
    if host != LOCAL:
        common.validate_host(host, "owner host")
    if not isinstance(record["first_seen"], str):
        raise common.ConfigError("owner first_seen must be a string")
    folded = _FoldedName(room)
    if folded in owners:
        raise common.ConfigError("owners.json has a case-fold duplicate")
    return folded, Owner(host, record["first_seen"])


def _decode_owners(data, on_invalid=None):
    # A name a peer legally published before the grammar tightened is still
    # sitting in the ownership memory of every node that saw it. Refusing the
    # whole file would hand that peer a permanent denial of service, so an
    # entry that no longer decodes is dropped and reported; the rest of the
    # map is honoured. Structural damage to the file itself still raises.
    value = common.load_json_bytes(data, "owners.json")
    if not isinstance(value, dict):
        raise common.ConfigError("owners.json must be an object")
    owners = {}
    for index, (room, record) in enumerate(value.items()):
        try:
            folded, owner = _decode_owner(room, record, owners)
        except common.ConfigError as error:
            if on_invalid is not None:
                on_invalid(room if isinstance(room, str) else index, str(error))
            continue
        owners[folded] = owner
    return owners


def load_owners(settings, logger=None) -> Dict[str, Owner]:
    """Load the human-editable ownership memory, or an empty initial map."""
    path = _state_path(settings, "rooms", "owners.json")
    data = _read_persisted(path, ROOMS_MAX_BYTES)
    if data is None:
        return {}
    if logger is None:
        return _decode_owners(data)

    def report(entry, reason):
        logger.emit("owners_invalid", entry=entry, reason=reason)

    return _decode_owners(data, on_invalid=report)


def _persist_owners(settings, owners, filename="owners.json"):
    value = {
        _display_name(room): {"host": owner.host, "first_seen": owner.first_seen}
        for room, owner in sorted(owners.items())
    }
    common.atomic_replace(
        _state_path(settings, "rooms", filename),
        _json_bytes(value),
        settings.root,
    )


def update_owners(
    settings, claims, logger, owner_hosts=None
) -> Dict[str, Owner]:
    """Honor human edits, add first sightings, and atomically persist ownership."""
    current = load_owners(settings, logger)
    original = dict(current)
    shadow_path = _state_path(settings, "rooms", "owners.last.json")
    shadow_data = _read_persisted(shadow_path, ROOMS_MAX_BYTES)
    shadow = current if shadow_data is None else _decode_owners(shadow_data)
    for room, owner in sorted(shadow.items()):
        edited = current.get(room)
        if edited is None:
            logger.emit("owner_released", room=_display_name(room), host=owner.host)
        elif edited.host != owner.host:
            logger.emit(
                "owner_changed",
                room=_display_name(room),
                old_host=owner.host,
                host=edited.host,
            )
    if owner_hosts is not None:
        evicted = {}
        for room, owner in list(current.items()):
            if owner.host != LOCAL and owner.host not in owner_hosts:
                evicted.setdefault(owner.host, []).append(_display_name(room))
                del current[room]
        for host, names in sorted(evicted.items()):
            logger.emit("owner_evicted", host=host, names=sorted(names))
    for room in sorted(claims):
        if room in current:
            continue
        candidates = list(claims[room])
        if not candidates:
            continue
        claimant, oid = candidates[0]
        current[room] = Owner(claimant, "" if claimant == LOCAL else oid)
    if current != original:
        _persist_owners(settings, current)
    if shadow_data is None or current != shadow:
        _persist_owners(settings, current, "owners.last.json")
    return current


def _config_peers(config):
    if isinstance(config, Mapping):
        return config.get("peers", {}) or {}
    return getattr(config, "peers", {}) or {}


def _split_rooms(settings, post_rooms):
    remote_root = common.destination(settings.root, "remote")
    real = {}
    placeholders = {}
    for name, raw_path in post_rooms.items():
        name = common.normalize_name(name)
        canonical = os.path.realpath(os.path.expanduser(raw_path))
        if common.under(remote_root, canonical):
            try:
                parts = Path(canonical).relative_to(remote_root.resolve()).parts
            except ValueError:
                parts = ()
            placeholders[name] = (
                parts[0]
                if len(parts) == 2
                and parts[1] == name
                and common.HOST_RE.fullmatch(parts[0]) is not None
                else None
            )
        else:
            real[name] = canonical
    return real, placeholders


def build_snapshot(
    settings, config, git, oids, post_rooms, logger, denied_names=()
) -> Snapshot:
    """Build the immutable post-fetch topology and contest decision snapshot."""
    configured = _config_peers(config)
    registry = read_registry(settings, git, oids.get("registry"), logger)
    peers = effective_peers(settings.host, registry, configured, logger)
    published = {
        host: read_peer_rooms(settings, git, host, oids.get(f"machines/{host}"), logger)
        for host in peers
    }
    pins = {host: frozenset(configured.get(host, ())) for host in peers}
    real_rooms, placeholders = _split_rooms(settings, post_rooms)

    registered_by_fold = {
        common.fold_name(name): (name, raw_path) for name, raw_path in post_rooms.items()
    }
    for host, names in sorted(configured.items()):
        for room in sorted(names, key=common.fold_name):
            registered = registered_by_fold.get(common.fold_name(room))
            if registered is None:
                continue
            registered_name, registered_path = registered
            expected = common.destination(settings.root, "remote", host, room)
            actual = os.path.realpath(os.path.expanduser(registered_path))
            if actual == str(expected.resolve(strict=False)):
                continue
            _collision(settings, room, expected, registered_path, logger)
            raise common.ConfigError(
                f"peer room {room!r} collides with registered room {registered_name!r}"
            )

    claims = {}

    def claim(room, claimant, oid):
        records = claims.setdefault(_FoldedName(room), [])
        if all(existing != claimant for existing, _ in records):
            records.append((claimant, oid or ""))

    for room in sorted(real_rooms):
        claim(room, LOCAL, "")
    for host in peers:
        for room in sorted(published[host] | pins[host]):
            claim(room, host, oids.get(f"machines/{host}", ""))
    for room, host in sorted(placeholders.items()):
        if host is not None:
            claim(room, host, oids.get(f"machines/{host}", ""))

    # Eviction runs only against a topology we actually know. read_registry
    # returns None both for a node that has no registry and for one whose
    # pinned blob was unreadable with no valid persisted fallback; with no
    # config.peers either, the second case is not "every peer left", it is
    # "we cannot see who the peers are". Evicting there would wipe the squat
    # defense on one bad fetch, so owner_hosts stays None and update_owners
    # skips the loop.
    if registry is not None:
        owner_hosts = set(registry) - {settings.host}
    elif configured:
        owner_hosts = set(configured)
    else:
        owner_hosts = None
    owners = update_owners(
        settings, claims, logger, owner_hosts=owner_hosts
    )
    for room, owner in sorted(owners.items()):
        current_publishers = {
            host
            for host in peers
            if room in {common.fold_name(name) for name in published[host]}
        }
        if owner.host == LOCAL:
            if room not in {common.fold_name(name) for name in real_rooms}:
                claim(room, LOCAL, "")
        elif owner.host not in current_publishers:
            claim(room, owner.host, owner.first_seen)

    contested = {}
    routes = {}
    for room, records in sorted(claims.items()):
        claimants = tuple(sorted({host for host, _ in records}))
        if len(claimants) >= 2:
            owner = owners.get(room)
            contested[room] = Contest(owner.host if owner else None, claimants)
        elif claimants and claimants[0] != LOCAL and claimants[0] in peers:
            routes[room] = claimants[0]

    active = {
        (host, _FoldedName(room))
        for host in peers
        for room in published[host] | pins[host]
    }
    retired = frozenset(
        (host, room)
        for room, host in placeholders.items()
        if host is not None and (host, _FoldedName(room)) not in active
    )
    # ``denied_names`` is a room-level privacy list. The bridge passes none:
    # config.channels.deny names channels, and used to double as a room deny,
    # so denying a channel silently unpublished a same-named room and every
    # peer then logged that room as retired on every tick.
    denied = {
        common.fold_name(name) if isinstance(name, str) else name
        for name in denied_names
    }
    publishable = []
    for room in sorted(real_rooms):
        folded = common.fold_name(room)
        if folded in denied:
            continue
        try:
            common.validate_room(room, topology=True, label="local room")
        except common.ConfigError:
            continue
        publishable.append(common.normalize_name(room))
    v2_peers = frozenset(
        host
        for host in peers
        if _state_path(settings, "rooms", "peers", f"{host}.json").is_file()
    )
    pinned_oids = {
        key: value
        for key, value in oids.items()
        if value and (key == "registry" or key in {f"machines/{h}" for h in peers})
    }
    return Snapshot(
        self_host=settings.host,
        peers=tuple(peers),
        oids=pinned_oids,
        published=published,
        v2_peers=v2_peers,
        pins=pins,
        owners=owners,
        routes=routes,
        contested=contested,
        retired=retired,
        real_rooms=real_rooms,
        placeholders=placeholders,
        publishable=tuple(publishable),
        peers_known=owner_hosts is not None,
    )


def _post_rooms(settings):
    environment = common.post_environment(settings.root)
    result = subprocess.run(
        [settings.post_bin, "rooms", "--json"],
        env=environment,
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )
    if result.returncode != 0:
        raise common.ConfigError(f"post rooms --json failed: {result.stdout}{result.stderr}")
    value = common.load_json_bytes(result.stdout.encode("utf-8"), "post rooms output")
    rooms = value.get("rooms") if isinstance(value, dict) else None
    if not isinstance(rooms, list):
        raise common.ConfigError("post rooms --json returned an unexpected shape")
    result_map = {}
    for room in rooms:
        if not isinstance(room, dict) or not isinstance(room.get("name"), str) or not isinstance(room.get("path"), str):
            raise common.ConfigError("post rooms --json returned an invalid room entry")
        result_map[room["name"]] = room["path"]
    return result_map


def _collision(settings, room, expected, registered, logger):
    payload = {
        "room": room,
        "expected": str(expected),
        "registered": registered,
        "at": common.utc_now(),
    }
    common.atomic_replace(
        _state_path(settings, "collisions.json"), _json_bytes(payload), settings.root
    )
    logger.emit("collision", room=room, expected=str(expected), registered=registered)


def ensure_placeholders(
    settings, config, snapshot, logger, deadline, fence_present
) -> Dict[str, str]:
    """Create/register every routed placeholder and report retained retired ones.

    ``routes`` is bounded only by untrusted peer input, so this loop checks the
    tick deadline and the fence before every candidate as v1 did (SPEC-v2
    §Tick order: steps 8-16 check the deadline between candidates).
    """
    rules_path = common.destination(settings.root, "rules.json")
    if not rules_path.exists():
        try:
            common.exclusive_publish(
                rules_path,
                b'{"blocked":[]}\n',
                _state_path(settings, "tmp"),
                settings.root,
            )
        except FileExistsError:
            pass
    rooms = _post_rooms(settings)
    for host, folded_room in sorted(snapshot.retired):
        logger.emit("room_retired", host=host, room=_display_name(folded_room))
    for folded_room, host in sorted(snapshot.routes.items()):
        deadline.check()
        if fence_present():
            raise common.TickError("fenced")
        room = _display_name(folded_room)
        expected = common.destination(settings.root, "remote", host, room)
        if room in rooms:
            registered = os.path.realpath(os.path.expanduser(rooms[room]))
            if registered == str(expected.resolve(strict=False)):
                common.ensure_dir(settings.root, expected)
                continue
            if common.fold_name(room) in {
                common.fold_name(name) for name in snapshot.pins.get(host, frozenset())
            }:
                _collision(settings, room, expected, rooms[room], logger)
                raise common.ConfigError(f"room registration collision for {room!r}")
            logger.emit(
                "placeholder_conflict",
                host=host,
                room=room,
                expected=str(expected),
                registered=rooms[room],
            )
            continue
        # `rooms add` needs the placeholder directory to exist. The room's
        # inbox/read land in post's own tree, so they are created only after
        # post has accepted the name (M4: a refused name leaves nothing).
        common.ensure_dir(settings.root, expected)
        environment = common.post_environment(settings.root)
        result = subprocess.run(
            [settings.post_bin, "rooms", "add", "--", room, str(expected)],
            env=environment,
            capture_output=True,
            text=True,
            timeout=30,
            check=False,
        )
        if result.returncode != 0:
            if common.fold_name(room) in {
                common.fold_name(name) for name in snapshot.pins.get(host, frozenset())
            }:
                raise common.ConfigError(
                    f"post rooms add failed for {room!r}: {result.stdout}{result.stderr}"
                )
            current = _post_rooms(settings)
            registered = next(
                (
                    path
                    for name, path in current.items()
                    if common.fold_name(name) == common.fold_name(room)
                ),
                result.stdout + result.stderr,
            )
            logger.emit(
                "placeholder_conflict",
                host=host,
                room=room,
                expected=str(expected),
                registered=registered,
            )
            rooms.update(current)
            continue
        # <root>/<room>/{inbox,read} are post's own mailbox: `post send`
        # creates them the first time it writes there. Creating them here,
        # every tick for every peer room, left empty directories behind after
        # a rename or an owner release (post-6ep).
        logger.emit("room_registered", host=host, room=room, path=str(expected))
        rooms[room] = str(expected)
    return _post_rooms(settings)


def rooms_json_bytes(snapshot) -> bytes:
    """Serialize the deterministic local rooms publication."""
    selected = sorted(
        snapshot.publishable, key=lambda room: (common.fold_name(room), room)
    )[:1024]
    return _json_bytes(
        {"v": 1, "host": snapshot.self_host, "rooms": selected}
    )


def write_rooms_json(settings, snapshot, logger) -> bool:
    """Replace the worktree publication only when its deterministic bytes differ."""
    path = common.destination(settings.repo, "rooms.json")
    desired = rooms_json_bytes(snapshot)
    if len(snapshot.publishable) > 1024:
        logger.emit("rooms_publication_truncated", count=len(snapshot.publishable))
    try:
        current = common.open_regular(path, ROOMS_MAX_BYTES)
    except FileNotFoundError:
        current = None
    if current == desired:
        return False
    common.atomic_replace(path, desired, settings.repo)
    logger.emit("rooms_json_written", rooms=len(snapshot.publishable))
    return True


def rooms_health(snapshot) -> dict:
    """Return room health; route_contested is exactly len(contested)."""
    collisions = [
        {
            "room": _display_name(room),
            "owner": contest.owner,
            "claimants": list(contest.claimants),
        }
        for room, contest in sorted(snapshot.contested.items())
    ]
    retired = [
        {"host": host, "room": _display_name(room)}
        for host, room in sorted(snapshot.retired)
    ]
    return {
        "published": min(len(snapshot.publishable), 1024),
        "collisions": collisions,
        "retired": retired,
        "route_contested": len(snapshot.contested),
    }
