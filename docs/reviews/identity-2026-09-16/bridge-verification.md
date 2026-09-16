# Bridge verification (2026-09-16)

## Provenance

The requested source and installed executable are byte-identical.  The
verification command was:

```text
sha256sum /Users/treygoff/Code/claude-space/post-bridge/sweep.py ~/.local/bin/post-bridge-sweep
a9d554fc2d8d4b63820c262260f9a53b579bb43d734f123128164f554f0a37d7  /Users/treygoff/Code/claude-space/post-bridge/sweep.py
a9d554fc2d8d4b63820c262260f9a53b579bb43d734f123128164f554f0a37d7  /Users/treygoff/.local/bin/post-bridge-sweep
```

## 1. Publish/import set

### Publish (local mail root -> relay)

* The only mail-root source enumerated for outbound publication is the
  immediate children of `<POST_MAIL_ROOT>/archive/`; only `*.mail` files with
  a valid id are considered (`sweep.py:1463-1479`).  The envelope's `to` must
  be a configured remote placeholder, and an envelope whose `from` is itself
  a placeholder is excluded (`sweep.py:1481-1500`).
* Each selected archive byte string is copied unchanged to the relay worktree
  as `outbox/<destination-host>/<room>/<id>.mail`
  (`sweep.py:1516-1536`).  The commit stages **only** the relay namespaces
  `outbox` and `receipts` (`sweep.py:1678-1682`).
* Receipts are also relay data (written on the receiver's branch) at
  `receipts/<sender-host>/<room>/<id>.json`; their payload is a newly
  serialized receipt, not an envelope (`sweep.py:876-906`).

There is no channel publication path.  `channels` occurs in `sweep.py` only
as a topology-deny-list name (`sweep.py:44-50`); no channel tree is selected,
copied, staged, or pushed.  Thus the exact transport is direct-mail outboxes
plus delivery receipts, not channels.

### Import (relay -> local mail root)

* For each configured peer, inbound mail is read from that peer's fetched
  branch under exactly `outbox/<this-host>/<room>/<id>.mail`
  (`sweep.py:1150-1156`, `sweep.py:1170-1188`), and blob bytes are obtained
  with `git show` (`sweep.py:1245-1268`).
* For an outbound item, the sender reads the receiver's fetched receipt at
  `receipts/<this-host>/<room>/<id>.json` (`sweep.py:1589-1623`).  Only a
  `delivered` receipt permits pruning; held/quarantined/no receipt leaves the
  outbox entry queued (`sweep.py:1629-1643`).
* A deliverable inbound blob is written to the destination room's
  `<room>/inbox/<id>.mail` and to the root `archive/<id>.mail`
  (`sweep.py:1352-1407`); those are local destination writes, not additional
  relay namespaces.

No bridge code enumerates or stages `participants/`, `lineages/`, `routing/`,
or `.participants.lock`; those paths are therefore outside this bridge's
publish/import set.  (The fixed relay namespace is enforced by the explicit
`git add -- outbox receipts` above.)

## 2. Unknown envelope keys and header bound

`parse_envelope` parses the header with the duplicate-key-rejecting JSON
loader, validates required/known fields, computes the set of unknown keys,
and only logs their names; it returns the parsed value without deleting keys
(`sweep.py:819-857`).  Crucially, delivery writes the original `data` bytes to
both inbox and archive (`sweep.py:1373-1380`, `sweep.py:1397-1407`).  Unknown
envelope keys (and their original JSON formatting/body bytes) are therefore
preserved as raw bytes, not re-serialized.

The header delimiter is the first `\n---\n`; a delimiter index greater than
4096 is rejected (`sweep.py:819-825`).  Consequently at most 4096 bytes before
that delimiter are accepted (index 4096 passes; 4097 fails).  Outbound parsing
applies the same check (`sweep.py:1505-1513`).

## 3. Unknown-room inbound behavior

After path/id validation, a room absent from `real_rooms` is logged as
`undeliverable`, counted, and skipped before `git show`; no local file or
receipt is written (`sweep.py:1181-1206`).  Because the sender-side prune
requires a matching `delivered` receipt, the upstream outbox entry remains in
the relay and is seen again on later ticks (`sweep.py:1580-1597`,
`sweep.py:1629-1643`).  The tick writes `health.json` with `ok = false`,
`reason = "undeliverable"`, and the observed count (`sweep.py:2164-2176`).

## 4. Receiving-host inbox mutation

Inbound delivery only creates `<room>/inbox/<id>.mail` when neither inbox nor
read nor archive already exists, using an exclusive publication; an existing
inbox is not moved or rewritten (`sweep.py:1358-1381`).  The only inbox
deletion in this file is a separate sender-side post-publication tidy for
outbound placeholder mail (`sweep.py:1765-1809`); it is not a receiving-side
routing operation.  No receiving-side move/rename of routed mail is present.

## 5. Meaning of `from`

The envelope `from` must satisfy the path-safe room grammar
(`sweep.py:833-841`).  On inbound, a `from` equal to a real local room or a
placeholder homed at another host is rejected as `forged_from`; any other
non-placeholder sender is accepted with a `from-unhomed` log and continues
through delivery/rule evaluation (`sweep.py:1277-1291`).

`from` does not appear in the outbox path: outbound selection uses it only to
exclude bridge-origin messages, while the target path is derived from
`to` (`sweep.py:1494-1500`, `sweep.py:1516-1519`).  Receipts likewise contain
host, room, id, hash, status, reason, and timestamp; they have no envelope
`from` field (`sweep.py:860-873`).

Therefore a participant id used as `from` behaves as a free-form/unhomed
sender if it passes the room grammar and does not collide with a local or
foreign placeholder: it is logged, delivered, and has no effect on outbox
layout or receipt keying.  An invalid/reserved id is quarantined by envelope
validation; a colliding registered-room/foreign-placeholder name is
`forged_from`.

## Discovered

* `sweep.py` itself does not add `participants`, `lineages`, `routing`, or
  `.participants.lock` to its topology deny list; this report relies on the
  upgraded `post` room-registration/reservation contract to reject those as
  room names.  Independently of that registration guard, the bridge's relay
  staging/import code names only `outbox` and `receipts`.

## Decision

§10 NEEDS CORRECTION: `sweep.py` bridges direct-mail outboxes and delivery
receipts only; it does **not** publish/import channels.  Replace “room
outboxes and channels only” with “direct-mail outboxes plus delivery receipts;
channels are not bridged.”
