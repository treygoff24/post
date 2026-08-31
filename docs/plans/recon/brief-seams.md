# Recon lane: code-seam map for Plan B (read-only wrt src)

Map the exact code paths Plan B (stateful read layer: cursors, catchup, unread
counts, search) will touch in this Rust repo. Goal-lock context:
`docs/plans/plan-b-goal-lock.md`. Output is evidence for the plan author — a
seam map with real `file:line` anchors, not opinions.

## Map these, each with file:line anchors

1. **Writer admission / arx fence** — where a command is admitted as a writer
   (generation check, `.post-arx.json` parse, flock acquisition), where the
   root flock is held and released, which commands take which path. The
   CONTRACT.md "On-disk format" section describes the rules; find the code
   that implements each clause.
2. **Consuming read paths** — plain `read` and plain `chat` (non-peek): where
   the "consumed" state change happens today; that is where cursor advance
   will hook.
3. **Watch ring path** — `src/commands/watch.rs`: the backlog-replay invariant
   (~lines 266–273 — quote the actual current lines), the ring-line emission
   with `[first..last]` fencepost ids (shipped 0.8.0), heartbeat + re-admission
   under fence.
4. **Listings** — `channels.rs`, `inbox.rs`: how message counts are computed
   today (full jsonl scan? metadata?), the JSON output construction, where an
   `unread` field would be added.
5. **Store primitives** — jsonl append/read utilities, message id format and
   ordering guarantees (is id ordering total? per-channel?), atomic-write
   helpers if any, `--since` id comparison semantics (exclusive? string
   compare?).
6. **schema.rs / doctor.rs** — where new commands and fields must be declared
   so `post schema` stays truthful; doctor checks that could see cursor files.
7. **Test infrastructure** — where integration tests live, how they isolate
   stores, existing fence tests, the watch/doorbell tests, and
   `scripts/smoke-installed.sh` structure.

## Danger list

End with a **danger list**: invariants that Plan B must not change, each with
the anchor and one sentence on how it could be broken accidentally (e.g. a
cursor write widening a flock hold, catchup consuming a watch cursor, listings
becoming writers).

## Output

One file: `docs/plans/recon/seams.md`.

## Rules

- Write boundary: `docs/plans/recon/seams.md` only. Do not modify src, tests,
  or docs. Do not commit. Never run tree-wide git state commands
  (stash/checkout/restore) — other lanes share this tree.
- Honest anchors only: cite `file:line` solely for lines you actually read;
  mark anything inferred-but-unverified as such.
