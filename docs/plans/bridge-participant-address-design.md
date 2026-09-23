# Design: participant DM across hosts (lane F3)

Status: revision 2, for Aster's approval. Revision 1 (4aa7ef3) was reviewed by Aster, who kept the host-qualified target, the separate relay namespace, the archive-only queue, and the read-only delivery query, and required F3-1 through F3-8. They are folded in below and indexed at the end. No code until Aster approves. Depends on wave 1 (R7, F1) being merged and on F2 (bridge v2 live on both hosts). Author: Nightjar.

## What Trey asked for

"DM any agent anywhere by name." Today the Mac cannot address one devbox participant:

- `participant:<id>` resolves only against the local store (`src/participant.rs` `resolve_target`).
- The bridge carries only workspace mail: its envelope `to` must be a room and must equal the outbox path's room (`sweep.py` `parse_envelope`).

The workaround is to ssh to the other host and run post there.

## The change

Add one host-qualified address, `participant:<id>@<host>`.

1. Post queues the letter locally.
2. The bridge carries its bytes in a new relay namespace.
3. On the destination, **post itself admits and delivers it** through a new bridge-only command. That command reuses post's participant lookup, route-block policy, migration fence, and locks, and records the verified origin.
4. The bridge turns post's answer into a receipt.
5. The sender reads which of four states the letter is in.

Revision 2's main structural change is step 3. Admission is post policy, so it runs in post. Python never carries a near-copy of it.

**What this promises:** idempotent canonical delivery and retry-safe acknowledgements. It does not promise exactly-once processing. Any step can run again after a crash, and every rerun converges to one canonical message and one observable receipt.

## What stays the same

- Workspace mail, room publication, channels, and every existing envelope and receipt shape are unchanged.
- Local `participant:<id>` semantics are unchanged, including that local resolution checks existence, not `ended_at`.
- There is no global participant directory, and no `room@host` form.
- The bridge still authenticates hosts through the forge branch. Envelope fields are data, never credentials.
- Relay principals can read relayed DMs, as they can read workspace mail today.
- Post is not a security boundary against processes running as the same Unix user. The bridge-only command is no more trusted than the bridge's direct room writes are today.

This amends one SPEC-v2 non-goal ("Session-level addressing"). An explicit host-qualified participant address becomes the only session-level form, and it never falls back to a room.

## The address (F3-4, F3-7)

`participant:<id>@<host>`, resolved in this order:

1. **Exact local match first.** If a local participant record named exactly `<id>@<host>` exists, the address is local, using the same resolution as today. This preserves any historical id containing `@`. Post-generated ids never contain `@`.
2. **Otherwise, split at the last `@`.** `<host>` must match `^[a-z0-9-]{1,32}$`.
3. **Own host.** If `<host>` equals this host's bridge `host`, resolve `participant:<id>` with the unchanged local resolver.
4. **Remote host.** `<host>` must be in the effective enrolled set: the bridge's validated persisted registry (`$POST_MAIL_ROOT/bridge/registry/hosts.json`: exact keys, `v == 1`, host grammar, unique entries), minus this host, intersected with `bridge/config.json` `peers` when that list is non-empty. **There is no fallback to config peers alone**, since that could resurrect a revoked or unenrolled host.
5. **Errors, before anything is written:**
   - `topology_unavailable` (retryable) when the registry copy is missing or invalid;
   - `unknown_host`, naming the enrolled hosts;
   - `no_bridge` when this host has no bridge config.

   A valid enrolled host that happens to be unreachable is not an error; `queued` is the honest outcome.

`@` is refused only when a new participant id is created (`participant_id` generation) and in the remote-address segment. Loading an existing record through `validate_participant_id` is unchanged.

## The sender side (post)

1. **Sender prerequisite (the bounded first slice).** The acting participant's `from` must be a real local room (a rooms.json entry not under `remote/`). Otherwise the send fails with `remote_sender_unroutable` and the fix `post participant bind --workspace <room>`. This slice does not support workspace-less senders. Publication restrictions (deny lists, ownership) are enforced by the bridge at select time; a letter that fails there stays `queued` with a visible reason (see Delivery states).
2. **Write the letter** only to `archive/<mail-id>.mail`. Envelope:
   - `to` = `<id>`;
   - `address_kind` = `participant`;
   - a new optional `to_host`.

   Everything else (`from_participant`, `from_lineage`, `sender_provenance`, profile stamps) is recorded as usual. Post's `Envelope` has no `deny_unknown_fields`, so older readers tolerate `to_host`.
3. **The send receipt never claims delivery.** The send JSON gains an optional `delivery` object, `{"state":"queued","host":"<host>"}`. The text form says "queued for <host>; not yet delivered."

## The relay

A new namespace, so participant ids cannot collide with room names, and so pre-F3 sweepers ignore it: they list only `outbox/<self>/` and `receipts/<self>/`.

- `pmail/<dest-host>/<id>/<mail-id>.mail`: the letter's archive bytes, unchanged.
- `preceipts/<origin-host>/<id>/<mail-id>.json`: exactly the keys `v`, `status`, `origin`, `host`, `participant`, `id`, `sha256`, `reason`, and `at`, sorted, at most 4 KiB.
  - `v` is 1; `status` is `delivered` or `rejected`.
  - `host` is the publisher, which must equal the branch's host; `origin` is the sending host.
  - `reason` is null for `delivered`, and for `rejected` one value from the vocabulary below.

**Terminal rejection reasons** (each ends that letter; a new explicit send is the only retry):

- `unknown_participant`, `ended_participant` (conclusive lookups only, see F3-3);
- `blocked_route`;
- `to_mismatch`, `forged_from`, `name_collision`, `unpublished_sender`;
- `id_collision`, `malformed`.

**Retryable conditions** produce no receipt. The destination counts each in health with its age, and the letter stays `published`:

- `participant_unreadable` or `inventory_degraded`;
- `fenced`;
- `post_unavailable` or `timeout`;
- `topology_unavailable`.

## Destination delivery (F3-1, F3-2, F3-3, F3-5)

**The bridge's part.** Per `pmail/<self>/<id>/<mail-id>.mail` on peer branch H:

1. Run the transport checks: v1 path, mode, size, and readability.
2. Run `parse_envelope`, extended for this namespace:
   - `address_kind` must be `participant`;
   - `to` must equal the path's `<id>`;
   - `to_host` must equal this host.
3. Apply trust fact 2 to `from`: it must be a placeholder homed at H.

A failure in any of these is a terminal rejection. Then the bridge writes the bytes to a private temporary file and runs:

`post bridge deliver --participant <id> --source-host <H> --mail-id <mail-id> --file <tmp> --json`

**Post's part.** `post bridge deliver` is a bridge-only writer. It runs under post's migration fence admission, like every other writer, and under the same lock that `post participant end` holds for that participant's record. The builder confirms and names that lock, and adds one only if none exists. Holding it makes admission and ending serialize. In order:

1. **Parse** the bytes with post's own mail parser. Also check that `to` equals `--participant`. Any failure is terminal `malformed` or `to_mismatch`.
2. **Look up the delivery record** at `participants/<id>/imports/<mail-id>.json`: `{v, source_host, sha256, mail_id, from_participant, delivered_at}`, written atomically. It is both the idempotence ledger and the frozen origin (F3-5).
   - If it exists with the same `source_host` and `sha256`, the outcome is `delivered` (a replay). Nothing is rewritten.
   - If it exists with anything else, the outcome is terminal `id_collision`, and the existing record and inbox file are untouched.
3. **Check the canonical inbox path** `participants/<id>/inbox/<mail-id>.mail` when no record exists. That is the state a crash after the write but before the record leaves behind.
   - Bytes identical: write the record, and the outcome is `delivered`.
   - Bytes different (a local letter or another host's letter already holds that id): terminal `id_collision`, untouched.

   The single canonical path is never overwritten, whichever host a colliding id comes from.
4. **Admit** (F3-2, F3-3). The participant lookup must be exact: read `participants/<id>/participant.json` through post's validated loader, which distinguishes absent from unreadable.
   - Record conclusively absent: terminal `unknown_participant`.
   - Record valid with `ended_at` set: terminal `ended_participant`.
   - Record unreadable, corrupt, or any I/O error: retryable `participant_unreadable`. It never becomes a terminal answer.

   Then call the existing route-block policy (`ensure_route_allowed` in `src/commands/send.rs`, which resolves the recipient's workspace and applies the blocked-route rules, wildcards included) with the verified `from` room as the sender. Blocked: terminal `blocked_route`, with no inbox write.
5. **Write** the inbox file with the same exclusive create a local send uses. This is **the admission point**: a participant ended after it still counts as delivered, and a committed delivery is never rewritten as a rejection. Then write the delivery record.
6. **Report** a typed outcome: `delivered`, `rejected` with a reason, or `retry` with a reason. The exit status distinguishes the three.

The rejection is a per-send snapshot. A session can bind later, and bind logic can revive an ended participant, but a rejected letter is never resurrected: a later bind needs a new explicit send.

**The bridge's part, continued:**

- `delivered` or `rejected`: write the receipt in its relay worktree, commit, and push.
- An existing receipt at that path is never replaced. If a later conflicting letter arrives under the same path, it is logged as `receipt_conflict` and counted in health, and the first receipt stands.
- `retry`: no receipt, and the letter is considered again next tick.

## Crash states and recovery (F3-1)

Destination writes: D1 inbox file → D2 delivery record → D3 receipt committed in the relay worktree → D4 pushed.

| Crash after | State left | Next tick converges by |
|---|---|---|
| nothing written | none | full delivery |
| D1 | inbox file, no record | step 3: identical bytes → write D2, `delivered` → D3, D4 |
| D2 | record, no receipt | step 2: replay → `delivered` → D3, D4 |
| D3 | local commit, not pushed | pushing it; if recovery discarded the commit, replay regenerates the same receipt from D2 |
| D4 | complete | the receipt exists, so the bridge skips; a forced replay is a no-op |

Sender writes: S1 archive → S2 pmail committed → S3 pushed → S4 `published` marker → S5 receipt seen → S6 `acked` record → S7 pmail pruned and pushed.

| Crash after | State left | Next tick converges by |
|---|---|---|
| S1 | `queued` | select and publish |
| S2 | local commit, not pushed | pushing it (copy_outbound compares against the archive bytes) |
| S3 | pushed, no marker | seeing the entry on its own remote branch → S4 with that commit |
| S4 or S5 | `published` | validating the receipt → S6 |
| S6 | `acked`, pmail still present | S7 |

**A receipt may outrun the marker.** A valid receipt establishes the terminal state even when S4 never happened.

The crash-injection test runs every write above, and every point before and after a push, in both directions. Each rerun must converge to one canonical inbox message on the destination and one `acked` record on the sender.

## Sender acknowledgement (F3-6)

- **`published` means pushed.** The marker is written only after a push containing those exact bytes succeeds, and it records the remote commit id. A local stage or commit alone is not `published`.
- **Receipts are validated in full:**
  - the branch is `machines/<to_host>`, and the receipt's `host` equals it;
  - `origin` is this host;
  - `participant` and `id` match the path and the archive letter;
  - `sha256` equals the archive letter's digest;
  - the key set is exact, and the status and reason come from the vocabulary.
- **`acked` is an exclusive create.** The first valid receipt wins. A later, different terminal record is not applied (no last-writer-wins); it is logged as `receipt_conflict`, and `post delivery` reports `conflict: true`.

## Delivery states and `post delivery` (F3-6, F3-8)

`post delivery <mail-id> [--json]` is read-only.

- It applies the same sender visibility rule as reading your own sent mail: only the letter's sender sees its delivery state.
- It validates the archive letter and every evidence file. Missing evidence and corrupt evidence are different answers.
- An id that is not in the archive is `not_found`, never `queued`.
- Workspace mail returns `unsupported`: no state is fabricated for it tonight.

| State | Evidence on the sending host |
|---|---|
| `queued` | the archive letter, with no valid marker and no valid receipt. It may carry `blocked_reason` and `last_error` from the bridge's `bridge/pmail-status/<mail-id>.json` (for example `peer_not_effective`, `sender_unpublished`, `topology_unavailable`, `relay_push_failed`). A transient failure never becomes `rejected`. |
| `published` | a valid marker with its remote commit, and no receipt. Shows the letter's age. |
| `received` | a valid `acked` record with status `delivered` |
| `rejected` | a valid `acked` record with status `rejected` and its reason |

Corrupt evidence reports `state: "unknown"` with the file and the error, never a guess.

**Letters to a destination without F3.** They stay `published`. The bridge reports the count and the oldest age in `health.json` and in status. It logs once when such a letter first appears and at most hourly afterwards, never every tick.

## The receiving side (post)

- **Remote-origin evidence is frozen at import (F3-5).** A delivered letter has a delivery record naming its verified `source_host` and digest. Post's `remote_origin` (R7) treats a matching delivery record as remote evidence, alongside its existing rules. So a later placeholder change or removal can never turn an imported letter into a local-own one.
- **Reply address.** `reply_to_participant` for an imported letter is `participant:<from_participant>@<source_host>`, taken from the delivery record, and only when the record's digest matches the inbox bytes. Without a valid record, the participant reply is omitted (origin unavailable). It is never re-derived from current topology. `from_participant` stays host-asserted data.

## Tests

**Post (Rust):**

- Address resolution:
  - an exact local id containing `@` stays local;
  - the last-`@` split, and host grammar;
  - own host resolves locally, unchanged;
  - an invalid registry gives `topology_unavailable` with no archive write;
  - unknown host, no bridge;
  - a peer restriction excludes a registered host;
  - `@` is refused for new ids while existing records still load.
- Send: the archive-only write; the queued receipt in JSON and text; the unroutable-sender refusal.
- `post bridge deliver`:
  - every outcome row, including a replay and a crash after the inbox write;
  - `id_collision` from a different host and from a local letter;
  - an absent record is terminal, an unreadable record is retryable;
  - ended is terminal;
  - a blocked route (with a wildcard rule and a workspace-less recipient) gets no inbox bytes and no success;
  - under a migration fence, nothing is written;
  - ending the participant concurrently with admission serializes.
- `post delivery`: each state; corrupt evidence gives `unknown`; an unknown id gives `not_found`; workspace mail gives `unsupported`; another participant's letter is not visible; a conflict.
- Origin: the reply comes from the delivery record; after the placeholder is removed, or its owner changes, the old letter keeps its host; a colliding local id is never own.

**Bridge (Python, three-root harness):**

- A round trip in each direction, then a reply round trip.
- The crash-injection matrix above.
- Pushes: a stage without a push is not `published`; a receipt that outran the marker still reaches the terminal state.
- Receipts: a receipt from the wrong branch, or with a wrong digest or an extra key, is rejected; a second, conflicting receipt is ignored and reported.
- Visible queued states: `blocked_reason` for a peer that is not effective and for an unpublished sender.
- A destination without F3: the letter stays `published`, and logging is bounded.
- Provenance keys arrive byte-identical, with no `unknown_envelope_keys` line.

**Live proof (plan P6):**

- A nonce DM from a Mac participant to a devbox participant, and one the other way. Each is acknowledged by the real recipient replying to `reply_to_participant`, and each is confirmed with `post delivery`.
- Replay a relay commit: canonical state is unchanged.
- An unknown id and an ended id are each rejected, and the rejections are visible to the sender.
- Provenance is byte-identical.

## Build split

- **Post (Rust):** a follow-up turn on lane R. It covers address resolution, the send path, `post bridge deliver`, `post delivery`, origin evidence and reply metadata, and schema entries.
- **Bridge (Python):** a follow-up turn on lane F1 on the devbox. It covers `pmail` and `preceipts`, select with `pmail-status`, the deliver call, receipts, `acked`, markers written after a push, health counts, and the SPEC-v2 amendment.
- **Order:** the Rust command's contract is fixed first, so the bridge codes against the real binary. The lead then integrates, runs both gates, deploys through the F2 path, and runs the live proof.

## Aster's review, as folded in

| Item | Where |
|---|---|
| F3-1 crash states, idempotent canonical delivery, receipt immutability, cross-host id collisions | Destination delivery 2–3, 5; Crash states |
| F3-2 route blocks and the fence via post's own seam | Destination delivery (post's part, step 4); `ensure_route_allowed` |
| F3-3 conclusive lookups only; per-send snapshot | Destination delivery step 4; retryable list |
| F3-4 one topology source, fail visibly, the bounded sender slice | The address; Sender side 1 |
| F3-5 origin frozen at import | Delivery record; Receiving side |
| F3-6 published means pushed; full receipt validation; first valid wins; evidence validation | Sender acknowledgement; `post delivery` |
| F3-7 `@` scope; local semantics unchanged; own host reuses the local resolver | The address |
| F3-8 visible queued reasons; bounded logging for destinations without F3 | Delivery states |
| Answers 1–4 (final with conditions; admission point; participant mail only; new command) | Terminal reasons; Destination step 5; `post delivery`; Sender side |
