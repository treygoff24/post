"""The per-tick topology snapshot (SPEC-v2 §Rooms, "The topology snapshot").

Built once per full tick, after the fetch, from pinned OIDs; immutable for
the rest of the tick. Every later step -- placeholder ensure, inbound
binding, outbound routing, channel import and publish -- reads this object
and never a ref or a config list.

`bridgelib.rooms` builds it; `bridgelib.channels` and `sweep.py` consume it.
"""

from dataclasses import dataclass, field
from typing import Dict, FrozenSet, Mapping, Optional, Tuple

from . import common

# Binding verdicts for a message whose `from` is `sender`, arriving on
# branch machines/<host> (SPEC-v2 §Contest detection, §Unpublished senders,
# and SPEC.md §Trust fact 2).
VERIFIED = "verified"  # placeholder homed at host: deliver
UNHOMED = "unhomed"  # legacy peer, name not published or pinned: deliver + from-unhomed log
FORGED_SELF = "forged_self"  # a real local room, or a placeholder homed elsewhere
NAME_COLLISION = "name_collision"  # contested name, host is not owner-of-record
UNPUBLISHED_SENDER = "unpublished_sender"  # v2 peer, name it never published

VERDICTS = frozenset(
    {VERIFIED, UNHOMED, FORGED_SELF, NAME_COLLISION, UNPUBLISHED_SENDER}
)

LOCAL = "local"  # owner-of-record token for a real local room


@dataclass(frozen=True)
class Owner:
    host: str  # a peer host token or LOCAL
    first_seen: str  # commit oid of machines/<host> at first sighting ("" for LOCAL)


@dataclass(frozen=True)
class Contest:
    owner: Optional[str]  # host token, LOCAL, or None when no owner-of-record exists
    claimants: Tuple[str, ...]  # sorted host tokens and/or LOCAL


@dataclass(frozen=True)
class Snapshot:
    self_host: str
    # Effective peers (registry minus self, intersected with local `peers`
    # when non-empty), sorted.
    peers: Tuple[str, ...]
    # Pinned commit oids: keys "machines/<H>" for every peer that has a
    # fetched branch, plus "registry" when present.
    oids: Mapping[str, str]
    # H -> validated published room names (last valid map when this tick's
    # blob was invalid; empty when never published).
    published: Mapping[str, FrozenSet[str]]
    # Hosts that have EVER published a valid rooms.json (persisted).
    v2_peers: FrozenSet[str]
    # H -> names pinned for H in the local config (legacy placeholders).
    pins: Mapping[str, FrozenSet[str]]
    # room -> owner-of-record after this tick's update (persisted memory).
    owners: Mapping[str, Owner]
    # room -> host for every uncontested name whose sole claimant is a peer:
    # the only map outbound selection may use.
    routes: Mapping[str, str]
    # room -> contest record for every contested name.
    contested: Mapping[str, Contest]
    # (H, room) pairs this node holds a placeholder for that no current
    # claimant publishes.
    retired: FrozenSet[Tuple[str, str]]
    # Real local rooms: name -> canonical path (from `post rooms --json`,
    # paths not under <root>/remote/).
    real_rooms: Mapping[str, str]
    # Registered placeholders: name -> host token (from the path
    # <root>/remote/<host>/<name>), or None when the path is malformed.
    placeholders: Mapping[str, Optional[str]]
    # Real local room names the bridge will publish in rooms.json (real
    # rooms minus denied channel names minus grammar failures), sorted.
    publishable: Tuple[str, ...] = field(default_factory=tuple)
    # r5.5 (M2): post's room table (exact name -> stored path) re-read after
    # placeholder registration. Delivery of a verified sender requires that
    # post itself reads it as remote (common.post_sees_remote). None means
    # the table was never read: nothing is seen, verified mail holds.
    # Optional, not `X | None`: dataclass annotations evaluate at runtime
    # and post-bridge still runs on Python 3.9.
    post_rooms: Optional[Mapping[str, str]] = None  # noqa: UP045
    # Whether `peers` is positively known: a registry was read, or the local
    # config names peers. False when read_registry returned None with an
    # empty config.peers, which is "cannot see who the peers are", not
    # "every peer left". Hold ledgers drop a non-peer's stamps only when
    # True (the same guard as update_owners' owner_hosts=None).
    peers_known: bool = True

    def is_contested_for(self, host: str, room: str) -> bool:
        contest = self.contested.get(common.fold_name(room)) or self.contested.get(room)
        return contest is not None and host in contest.claimants

    def owner_of(self, room: str) -> Optional[str]:
        owner = self.owners.get(common.fold_name(room)) or self.owners.get(room)
        return owner.host if owner is not None else None

    def route_for(self, room: str) -> Optional[str]:
        return self.routes.get(common.fold_name(room)) or self.routes.get(room)

    def is_v2_peer(self, host: str) -> bool:
        return host in self.v2_peers


def empty_snapshot(self_host: str) -> Snapshot:
    """A snapshot with no peers and no rooms: what a node sees before any
    registry or peer branch exists. Useful for tests and for the v1-only
    (no channels key, no registry) path."""
    return Snapshot(
        self_host=self_host,
        peers=(),
        oids={},
        published={},
        v2_peers=frozenset(),
        pins={},
        owners={},
        routes={},
        contested={},
        retired=frozenset(),
        real_rooms={},
        placeholders={},
        publishable=(),
    )


def binding_verdict(snapshot: Snapshot, host: str, sender: str) -> str:
    """Pure binding rule (SPEC-v2 §Contest detection item 3, §Unpublished
    senders, SPEC.md §Trust fact 2), for a message from `sender` arriving on
    machines/<host>. Returns one of VERDICTS."""
    folded = common.fold_name(sender)
    real_rooms = {common.fold_name(name) for name in snapshot.real_rooms}
    if folded in real_rooms:
        return (
            NAME_COLLISION
            if snapshot.is_contested_for(host, sender)
            else FORGED_SELF
        )
    if snapshot.is_contested_for(host, sender):
        return VERIFIED if snapshot.owner_of(sender) == host else NAME_COLLISION
    placeholders = {
        common.fold_name(name): host for name, host in snapshot.placeholders.items()
    }
    homed_at = placeholders.get(folded)
    if folded in placeholders:
        if homed_at == host:
            return VERIFIED
        return FORGED_SELF
    published = snapshot.published.get(host, frozenset())
    pinned = snapshot.pins.get(host, frozenset())
    if folded in {common.fold_name(name) for name in published | pinned}:
        return VERIFIED
    if snapshot.is_v2_peer(host):
        return UNPUBLISHED_SENDER
    return UNHOMED
