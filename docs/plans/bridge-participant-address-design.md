# Design: participant DM across hosts (lane F3)

Status: revision 3.1, approved for implementation. Revision 1 (4aa7ef3) was reviewed by Aster, who kept the host-qualified target, the separate relay namespace, the archive-only queue, and the read-only delivery query, and required F3-1 through F3-8. Revision 2 (5ad1c1d) folded those in and moved admission into post. Aster approved revision 2's architecture with two blocking ordering corrections, F3-R2-A (write the admission record before the inbox bytes) and F3-R2-B (keep typed letters out of the workspace outbox path, and refuse the feature while the local bridge lacks that guard), plus five implementation constraints. Revision 3 adopts all seven as rulings. Aster then made two final corrections to revision 3, adopted here as revision 3.1: receipts are never pruned tonight, and a replay of an admitted letter skips the checks that read mutable room and policy state. The table at the end indexes every ruling. Depends on wave 1 (R7, F1) being merged and on F2 (bridge v2 live on both hosts). Author: Nightjar.

## What Trey asked for

"DM any agent anywhere by name." Today the Mac cannot address one devbox participant:

- `participant:<id>` resolves only against the local store (`src/participant.rs` `resolve_target`).
- The bridge carries only workspace mail: its envelope `to` must be a room and must equal the outbox path's room (`sweep.py` `parse_envelope`).

The workaround is to ssh to the other host and run post there.

## The change

Add one host-qualified address, `participant:<id>@<host>`.

1. Post queues the letter locally, once it has proven that this host's running bridge understands it.
2. The bridge carries its bytes in a new relay namespace.
3. On the destination, **post itself admits and delivers it** through a new bridge-only command. That command reuses post's participant lookup, route-block policy, migration fence, and locks, and records the verified origin before the letter becomes visible.
4. The bridge turns post's answer into a receipt.
5. The sender reads the letter's state: one of four normal states, or `unknown` when the evidence is corrupt.

Admission is post policy, so it runs in post. Python never carries a near-copy of it.

**What this promises:** idempotent canonical delivery and retry-safe acknowledgements. It does not promise exactly-once processing. Any step can run again after a crash, and every rerun converges to one canonical message and one observable receipt. `received` means the letter is persisted in the recipient's inbox. It never means the agent read it.

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
4. **Remote host.** `<host>` must be in the effective enrolled set: the bridge's validated persisted registry (`$POST_MAIL_ROOT/bridge/registry/hosts.json`: exact keys, `v == 1`, host grammar, unique entries), minus this host, intersected with the host keys of `bridge/config.json` `peers` when that map is non-empty. That is the same rule as bridge v2's `effective_peers` (`bridgelib/rooms.py`), except that the bridge falls back to config peers when no registry has ever been persisted, and post does not. **There is no fallback to config peers alone**, since that could resurrect a revoked or unenrolled host.
5. **Errors, before anything is written:**
   - `topology_unavailable` (retryable) when the registry copy is missing or invalid;
   - `unknown_host`, naming the enrolled hosts;
   - `no_bridge` when this host has no bridge config.

   A valid enrolled host that happens to be unreachable is not an error; `queued` is the honest outcome.

`@` is refused only when a new participant id is created (`participant_id` generation) and in the remote-address segment. Loading an existing record through `validate_participant_id` is unchanged.

## The sender side (post)

1. **Sender prerequisite (the bounded first slice).** The acting participant's `from` must be a real local room (a rooms.json entry not under `remote/`). Otherwise the send fails with `remote_sender_unroutable` and the fix `post participant bind --workspace <room>`. This slice does not support workspace-less senders. Publication restrictions (deny lists, ownership) are enforced by the bridge at select time; a letter that fails there stays `queued` with a visible reason (see Delivery states).
2. **Bridge capability guard (F3-R2-B).** Before writing anything, post proves that this host's running bridge keeps typed letters out of the workspace outbox path and can carry participant mail. It reads `$POST_MAIL_ROOT/bridge/health.json`, which the F3 bridge writes every tick with a `capabilities` array, `ticked_at`, and `interval_s`.
   - Both `typed-outbound-exclusion` and `participant-mail-v1` present, and `ticked_at` no older than three times `interval_s`: proceed.
   - A fresh health file that lacks either capability means the bridge is known to predate F3. The send is refused with `bridge_unsupported`, which is not retryable until the bridge is upgraded.
   - A health file that is missing, unreadable, malformed, or stale leaves the running bridge unknown. The send is refused with the retryable `bridge_status_unavailable`.

   In every refusal nothing reaches `archive/`. The reason: a pre-F3 exporter selects outbound mail from the shared archive, reads `to` as a room name, and ignores `to_host`. If the participant id also names a room published by another host, that exporter would deliver the letter to that workspace. Receiver-side namespace isolation cannot catch this, because the damage happens on the sending host.
3. **Write the letter** only to `archive/<mail-id>.mail`. Envelope:
   - `to` = `<id>`;
   - `address_kind` = `participant`;
   - a new optional `to_host`.

   Everything else (`from_participant`, `from_lineage`, `sender_provenance`, profile stamps) is recorded as usual. Post's `Envelope` has no `deny_unknown_fields`, so older readers tolerate `to_host`.
4. **The send receipt never claims delivery.** The send JSON gains an optional `delivery` object, `{"state":"queued","host":"<host>"}`. The text form says "queued for <host>; not yet delivered."

## Outbound selection on the sending bridge (F3-R2-B)

The bridge's workspace outbox path (`outbox/<host>/<room>/`) accepts an archive letter only when its `address_kind` is absent (legacy workspace mail) or `workspace`, and it carries no `to_host`. Participant letters, lineage letters, and any letter with `to_host` are never copied to `outbox/`. The bridge logs `outbound_typed_skipped` once per id, not every tick. This also closes a hazard that exists before F3: a local lineage letter whose lineage name equals a room published by another host would otherwise be relayed to that workspace. This part ships in wave 1 with bridge v2 (lane F1), before F3.

Host-qualified participant letters take only the `pmail/` path below.

## The relay

A new namespace, so participant ids cannot collide with room names, and so pre-F3 sweepers ignore it on the receiving side: they list only `outbox/<self>/` and `receipts/<self>/`.

- `pmail/<dest-host>/<id>/<mail-id>.mail`: the letter's archive bytes, unchanged.
- `preceipts/<origin-host>/<id>/<mail-id>.json`: exactly the keys `v`, `status`, `origin`, `host`, `participant`, `id`, `sha256`, `reason`, and `at`, sorted, at most 4 KiB.
  - `v` is 1; `status` is `delivered` or `rejected`.
  - `host` is the publisher, which must equal the branch's host; `origin` is the sending host.
  - `reason` is null for `delivered`, and for `rejected` one value from the vocabulary below.
  - `at` for a `delivered` receipt is the admission record's `admitted_at`, so a receipt rebuilt after a crash is byte-identical to the one it replaces. For a `rejected` receipt it is the time of the decision.

**Terminal rejection reasons** (each ends that letter; a new explicit send is the only retry):

- `unknown_participant`, `ended_participant` (conclusive lookups only, see F3-3);
- `blocked_route`;
- `to_mismatch`, `forged_from`, `name_collision`, `unpublished_sender`;
- `id_collision`, `malformed`.

**Retryable conditions** produce no receipt. The destination counts each in health with its age, and the letter stays `published`:

- from post's decision: `participant_unreadable`, `inventory_degraded`, `import_record_unreadable`, `digest_mismatch`, `fenced`, `topology_unavailable`, `io_error`;
- from the bridge's handling of the post call: `post_unavailable` (any nonzero exit, a timeout, or a missing binary), `post_output_malformed` (empty, unparseable, wrong schema, or fields that disagree with the call), and `invalid_invocation` (post refused the arguments, which means the bridge's own checks disagree with post's).

## Destination delivery (F3-1, F3-2, F3-3, F3-5, F3-R2-A)

**The bridge's part.** Per `pmail/<self>/<id>/<mail-id>.mail` on peer branch H:

1. If a receipt for this letter already exists at `HEAD` of this host's own relay branch, skip it. A committed receipt is never recomputed, whatever has changed since (see Crash states).
2. Run the transport checks: v1 path, mode, size, and readability.
3. Run `parse_envelope`, extended for this namespace:
   - `address_kind` must be `participant`;
   - `to` must equal the path's `<id>`;
   - `to_host` must equal this host.

A failure in 2–3 is a terminal rejection. These checks, together with the source-branch trust that chose branch H, read only the letter's bytes, its path, and the branch. They run on every attempt. **The bridge does not apply trust fact 2 to `from` for participant mail.** That check reads the placeholder table, which changes over time, so it runs in post and only when a letter is first admitted (post's step 4). If the bridge ran it before post, a replay of an already-admitted letter would fail after a room changed owner, and the recovery that the admission record exists for would never happen.

Then the bridge writes the bytes to a private temporary file and runs:

`post bridge deliver --participant <id> --source-host <H> --mail-id <mail-id> --sha256 <hex> --file <tmp> --json`

**Post's part.** `post bridge deliver` is a bridge-only writer. It runs under post's migration fence admission, like every other writer, and under the same lock that `post participant end` holds for that participant's record. The builder confirms and names that lock, and adds one only if none exists. Holding it makes admission and ending serialize. In order:

Post's checks come in two kinds. **Structural checks** read only the call's arguments, the letter's bytes, and this host's fixed identity; they run on every attempt (step 1). **Admission checks** read state that changes over time: the participant record, the placeholder table, and the route rules. They run only when a letter is admitted for the first time (step 4). A valid, matching admission record is the proof that those checks already passed. A missing, unreadable, or mismatching record never skips them.

1. **Structural checks: revalidate every argument and the envelope.** The intended caller is the bridge, but a malformed call must not bypass post's invariants.
   - Arguments: `--participant` passes `validate_participant_id`; `--source-host` matches `^[a-z0-9-]{1,32}$` and is not this host; `--mail-id` passes post's mail-id grammar; `--sha256` is 64 lowercase hex; `--file` is a regular file within post's size cap. A failure here is post's ordinary `invalid_argument` error (exit 2), which the bridge treats as `invalid_invocation`, a retry.
   - Digest: `--sha256` must equal the digest post computes from the file. A mismatch is retryable `digest_mismatch`, a local fault the sender did not cause.
   - Envelope, parsed with post's own mail parser: `id` equals `--mail-id`; `to` equals `--participant` (else `to_mismatch`); `address_kind` is `participant`; `to_host` equals this host's bridge `host` (else `to_mismatch`; an unreadable bridge config is retryable `topology_unavailable`); `from_participant` is present, passes `validate_participant_id`, and contains no `@`; `from` passes room-name grammar. Any other parse failure is terminal `malformed`.
2. **Look up the admission record** at `participants/<id>/imports/<mail-id>.json`: `{v, participant, mail_id, source_host, sha256, from_participant, admitted_at}`, exact keys, `v == 1`. It is the admission point, the idempotence ledger, and the frozen origin in one immutable file.
   - Valid, with the same `source_host` and `sha256`: this letter was already admitted. Go to step 5. The admission checks are not re-run, so a participant ended, a rule added, or a room that changed owner since then does not undo the admission.
   - Valid, with a different `source_host` or `sha256`: terminal `id_collision`. The record and any inbox file stay untouched.
   - Present but unreadable or invalid: retryable `import_record_unreadable`. It never becomes a terminal answer and never licenses a write.
   - Absent: step 3.
3. **No record: an existing inbox file is a collision.** If `participants/<id>/inbox/<mail-id>.mail` exists, the outcome is terminal `id_collision` and the file stays untouched, even when its bytes are identical. The inbox bytes do not carry `source_host`, so they cannot prove which host sent them, and post never invents an origin for an unrecorded file.
4. **Admission checks, then the record** (F3-2, F3-3). These run only here, for a letter with no admission record.

   First the sender: `from` must be a room registered as a placeholder homed under `remote/<source-host>/`, which is trust fact 2. If it is not, the outcome is terminal `forged_from`, with `detail` saying whether `from` names a local room or no placeholder of that host. An unreadable rooms registry is retryable `topology_unavailable`.

   Then the participant. The lookup must be exact: read `participants/<id>/participant.json` through post's validated loader, which distinguishes absent from unreadable.
   - Record conclusively absent: terminal `unknown_participant`.
   - Record valid with `ended_at` set: terminal `ended_participant`.
   - Record unreadable, corrupt, or any I/O error: retryable `participant_unreadable`.

   Then call the existing route-block policy (`ensure_route_allowed` in `src/commands/send.rs`, which resolves the recipient's workspace and applies the blocked-route rules, wildcards included) with the verified `from` room as the sender. Blocked: terminal `blocked_route`, with nothing written.

   All checks passed: write the admission record with `exclusive_atomic_write` (`src/mailbox.rs`), with `admitted_at` set once, now. **This write is the admission point.** Every later step and every retry treats the letter as admitted.
5. **Complete or verify the canonical inbox file.** With a matching admission record in hand:
   - The file is absent: create it with `exclusive_atomic_write` from the verified bytes of this call (their digest equals the record's `sha256`). This is also how a crash between steps 4 and 5 recovers. It cannot resurrect read mail: participant reads advance a cursor, and post never deletes a participant inbox file.
   - The file exists with identical bytes: nothing to write.
   - The file exists with different bytes: terminal `id_collision`, both files preserved.
6. **Report.** `delivered` only when a matching admission record and matching canonical bytes both exist. A metadata file alone is never enough.

The rejection is a per-send snapshot. A session can bind later, and bind logic can revive an ended participant, but a rejected letter is never resurrected: a later bind needs a new explicit send.

**The bridge's part, continued:**

- `delivered` or `rejected`: write the receipt in its relay worktree, commit, and push.
- An existing receipt at that path is never replaced. If a later conflicting letter arrives under the same path, it is logged as `receipt_conflict` and counted in health, and the first receipt stands.
- `retry`: no receipt, and the letter is considered again next tick.

## The import command's contract (frozen before the bridge builds against it)

`post bridge deliver ... --json` has one success exit and one output shape.

**Exit 0 means post reached a decision.** The decision is in stdout: exactly one JSON object with these keys and types.

| Key | Type | Value |
|---|---|---|
| `ok` | bool | `true` |
| `schema` | string | `post.bridge-deliver.v1` |
| `outcome` | string | `delivered`, `rejected`, or `retry` |
| `reason` | string or null | null for `delivered`; for `rejected`, a terminal reason; for `retry`, a retryable reason from post's list |
| `participant` | string | echoes `--participant` |
| `mail_id` | string | echoes `--mail-id` |
| `source_host` | string | echoes `--source-host` |
| `sha256` | string | the digest post computed |
| `admitted_at` | string or null | RFC 3339 UTC from the admission record; non-null exactly when `outcome` is `delivered` |
| `replay` | bool | `true` when the admission record already existed |
| `detail` | string or null | human text, at most 512 characters |

**Every other exit is a retry.** A usage error (2), a fence refusal, an I/O error, a crash, a timeout, or a binary too old to know the command all leave the letter `published` with a health count.

The bridge applies these rules and no generic nonzero handler:

- It accepts `delivered` only with exit 0, the exact schema, echoed fields equal to what it passed, a `sha256` equal to its own digest, and a parseable `admitted_at`.
- It accepts `rejected` only with exit 0, the exact schema, matching echoed fields, and a `reason` in the terminal vocabulary.
- Anything else is a retry: `post_output_malformed` when the exit was 0, `post_unavailable` or `invalid_invocation` when it was not.
- It ignores keys it does not know, so post can add fields without breaking it. It never ignores a listed key with the wrong type.

Post gives the command a `post schema` entry and a contract sample (lane D's `post contract samples`), and the bridge's tests run against that sample from the binary they deploy with.

## Crash states and recovery (F3-1, F3-R2-A)

**Delivery.** Destination writes: D1 admission record → D2 canonical inbox file → D3 receipt committed in the relay worktree → D4 pushed.

| Crash after | State left | Next tick converges by |
|---|---|---|
| nothing written | none | full delivery; admission is re-evaluated, because nothing durable was decided |
| D1 | record, no inbox file | step 2 matches → step 5 creates the file from the verified bytes → `delivered` → D3 with `at` = `admitted_at`, D4 |
| D2 | record and inbox file | step 2 matches → step 5 verifies the bytes → `delivered` (replay) → D3, D4 |
| D3 | local commit, not pushed | pushing it; the receipt at `HEAD` keeps the bridge from calling post again |
| D4 | complete | skipped; a forced replay of the relay commit is a no-op |

Between D1 and D2 a concurrent reader sees no inbox file. From the moment the file exists, its admission record already exists, so no reader ever sees an imported letter without its origin.

**Rejection.** A rejection has no destination-side record; its receipt is its only durable form.

| Crash after | State left | Next tick converges by |
|---|---|---|
| the decision, before the receipt is committed | none | re-evaluating; the answer may differ (a route rule removed, say), and that is correct, because nothing was published |
| R1 receipt committed, not pushed | local commit | pushing it as is; post is never called again for this letter |
| R2 pushed | complete | skipped |

**When a rejection becomes final:** at R1. From then on the bridge never recomputes the answer. A published rejection therefore never later becomes a delivery. The one exception is an operator recovery that discards an unpushed relay commit. That returns the letter to the "before the receipt" row, and the sender never saw the discarded decision.

**Sender.** Sender writes: S1 archive → S2 pmail committed → S3 pushed → S4 `published` marker → S5 receipt seen → S6 `acked` record → S7 pmail pruned and pushed.

| Crash after | State left | Next tick converges by |
|---|---|---|
| S1 | `queued` | select and publish |
| S2 | local commit, not pushed | pushing it (copy_outbound compares against the archive bytes) |
| S3 | pushed, no marker | seeing the entry on its own remote branch → S4 with that commit |
| S4 or S5 | `published` | validating the receipt → S6 |
| S6 | `acked`, pmail still present | S7 |

**A receipt may outrun the marker.** A valid receipt establishes the terminal state even when S4 never happened.

**Receipts are tombstones: no pruning tonight.** The destination keeps every receipt in `preceipts/`, rejections included, even after the sender prunes its pmail. That is what makes a rejection final. Suppose a rejected letter's receipt were pruned, and later someone replayed the same pmail bytes after the participant or the rules changed. The bridge would call post, post would admit the letter, and it would reach the inbox. The sender's first `acked` record would still say `rejected`, but the delivery would already have happened. With every receipt kept, step 1 skips that replay before post is called. Admission records in `participants/<id>/imports/` are never pruned either. Retention for both needs its own design later, one that fixes a replay horizon first; until then `preceipts/` grows by one small file per letter.

The crash-injection tests run every write above, and every point before and after a push, in both directions. Each rerun must converge to one canonical inbox message on the destination and one `acked` record on the sender.

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

A letter is in one of four normal states, or `unknown`:

| State | Evidence on the sending host |
|---|---|
| `queued` | the archive letter, with no valid marker and no valid receipt. It may carry `blocked_reason` and `last_error` from the bridge's `bridge/pmail-status/<mail-id>.json` (for example `peer_not_effective`, `sender_unpublished`, `topology_unavailable`, `relay_push_failed`). A transient failure never becomes `rejected`. |
| `published` | a valid marker with its remote commit, and no receipt. Shows the letter's age. |
| `received` | a valid `acked` record with status `delivered`. The letter is persisted in the recipient's inbox; this says nothing about whether the agent has read it. |
| `rejected` | a valid `acked` record with status `rejected` and its reason |
| `unknown` | corrupt evidence. Reports the file and the error, never a guess. |

**Letters to a destination without F3.** They stay `published`. The bridge reports the count and the oldest age in `health.json` and in status. It logs once when such a letter first appears and at most hourly afterwards, never every tick.

## The receiving side (post)

- **One origin lookup, used everywhere (F3-5).** Post gets a single function that answers where a letter came from: local, remote (with host and sender participant), or unavailable. A letter in `participants/<id>/inbox/` is remote when its admission record is valid and its `sha256` matches the inbox bytes. The answer is then `source_host` and `from_participant` from that record, whatever the placeholder table says later. An unreadable admission record makes the origin unavailable. R7's existing evidence (`sender_provenance`, a placeholder `from`) still applies to workspace mail.
- **Every comparison goes through it.** A letter is never "own" just because its `from_participant` equals the reader's id, since a remote host can mint the same id. The builder finds every place that compares `from_participant` (or the sender) against a local id: ownership filtering in inbox and read, sender exclusion in routing, text and JSON reply rendering, and watch output (`rg -n 'from_participant' src`). Each one calls the origin function, and each gets a test in which an imported letter's `from_participant` equals the reader's own id. Fixing only `reply_to_participant` while another raw comparison still hides the letter is the failure this rule prevents.
- **Reply address.** `reply_to_participant` for an imported letter is `participant:<from_participant>@<source_host>`, taken from the admission record. When the origin is unavailable, the participant reply is omitted. It is never re-derived from current topology. `from_participant` stays host-asserted data.

## Deployment order and the capability guard (F3-R2-B)

The runtime guard (Sender side, step 2) refuses the feature on a host whose bridge is known to predate F3. The deployment order makes sure it is never needed in practice:

1. **F2 done on both hosts.** Bridge v2, with the typed-letter exclusion, is the only outbound bridge. Prove it with a unit inventory (`systemctl --user list-units` on the devbox, `launchctl list` on the Mac) and a process inventory (`ps` for any v1 sweep), and record both in the F2 receipt.
2. **Deploy the F3 bridge on both hosts** through the F2 path. Confirm each host's `health.json` lists both `typed-outbound-exclusion` and `participant-mail-v1`, with a fresh `ticked_at`.
3. **Only then install the F3 post binary** on either host.
4. **Rollback runs in the reverse order.** Roll post back first. A bridge rollback to a pre-F3 version deletes `bridge/health.json` before starting the older bridge, so a stale F3 health file can never vouch for it.

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
- Send:
  - the archive-only write; the queued receipt in JSON and text; the unroutable-sender refusal;
  - the capability guard: a fresh health file without either capability gives `bridge_unsupported`; a missing, malformed, or stale one gives `bridge_status_unavailable`; neither writes to `archive/`.
- `post bridge deliver`:
  - every revalidation rule, each alone: participant and mail-id disagreement, `address_kind`, a foreign `to_host`, source-host grammar and own host, `from_participant` grammar and `@`, a `from` not homed at the source host, and a digest mismatch (retry, not rejection);
  - the contract: every decided outcome exits 0 with the schema; `admitted_at` is non-null exactly for `delivered`; the contract sample matches the real output;
  - admission-record first: fault injection after D1 and after D2, each rerun converging to `delivered` with the original `admitted_at`;
  - a replay of an admitted letter after its `from` placeholder is removed or re-homed, after the participant ends, and after a blocking rule is added: each still completes and reports `delivered`, with no admission check run;
  - a letter with no record whose `from` is not homed at the source host: `forged_from`, nothing written;
  - a concurrent reader, looping during an injected pause after D1 and again after D2, never sees the letter without remote origin or with a local-own reply;
  - a cross-host same-bytes retry (host B sends the bytes host A delivered): `id_collision`, and A's record and inbox file are unchanged;
  - an unrecorded inbox file with identical bytes: `id_collision`;
  - a matching record with a mismatched inbox file: `id_collision`, both preserved;
  - an unreadable admission record: retry, and nothing written;
  - an absent participant record is terminal, an unreadable one is retryable;
  - ended is terminal; ending after D1 still delivers;
  - a blocked route (with a wildcard rule and a workspace-less recipient) gets no admission record, no inbox bytes, and no success;
  - under a migration fence, nothing is written;
  - ending the participant concurrently with admission serializes.
- `post delivery`: each state; corrupt evidence gives `unknown`; an unknown id gives `not_found`; workspace mail gives `unsupported`; another participant's letter is not visible; a conflict.
- Origin: one test per call site in which an imported letter's `from_participant` equals the reader's own id (inbox and read ownership, routing exclusion, text and JSON reply, watch); the reply comes from the admission record; after the placeholder is removed, or its owner changes, the old letter keeps its host; a colliding local id is never own.

**Bridge (Python, three-root harness):**

- A round trip in each direction, then a reply round trip.
- The crash-injection matrices above, including the rejection rows: a decision lost before commit is re-evaluated; a committed rejection is pushed as is and post is not called again, even after the rejecting condition clears.
- The import contract: a nonzero exit, empty stdout, garbage, the wrong schema, a disagreeing echoed field, and an out-of-vocabulary reason are each a retry, never a rejection. Red-proof it against a generic "nonzero means rejected" handler.
- A delivered receipt rebuilt after a crash is byte-identical to the original.
- A rejection stays final: reject a letter, let the sender prune its pmail, change the participant and the rules so that admission would now pass, then replay the same pmail bytes. There is no inbox delivery, no admission record, no post call, and no second receipt.
- An admitted letter whose inbox write was lost (crash after D1), replayed after its `from` room changed owner: the bridge still calls post, and the letter is delivered.
- Pushes: a stage without a push is not `published`; a receipt that outran the marker still reaches the terminal state.
- Receipts: a receipt from the wrong branch, or with a wrong digest or an extra key, is rejected; a second, conflicting receipt is ignored and reported.
- Visible queued states: `blocked_reason` for a peer that is not effective and for an unpublished sender.
- Outbound exclusion (lands in wave 1): a local participant letter whose id names a room published by another host, a lineage letter whose name does the same, and any `to_host` letter never enter `outbox/` or a workspace inbox, and each is logged once.
- `health.json` carries `capabilities`, `ticked_at`, and `interval_s`.
- A destination without F3: the letter stays `published`, and logging is bounded.
- Provenance keys arrive byte-identical, with no `unknown_envelope_keys` line.

**Live proof (plan P6):**

- The deployment order above, with its unit and process inventories.
- A nonce DM from a Mac participant to a devbox participant, and one the other way. Each is acknowledged by the real recipient replying to `reply_to_participant`, and each is confirmed with `post delivery`.
- Replay a relay commit: canonical state is unchanged.
- An unknown id and an ended id are each rejected, and the rejections are visible to the sender.
- Provenance is byte-identical.

## Build split

- **Post (Rust):** a follow-up turn on lane R. It covers address resolution, the capability guard, the send path, `post bridge deliver` with its frozen contract and sample, `post delivery`, the single origin lookup and its call sites, reply metadata, and schema entries.
- **Bridge (Python):** a follow-up turn on lane F1 on the devbox. It covers `pmail` and `preceipts`, select with `pmail-status`, the deliver call under the contract's rules, receipts with stable timestamps, `acked`, markers written after a push, health counts and capabilities, and the SPEC-v2 amendment. The typed-letter outbound exclusion is already in F1's wave-1 scope.
- **Order:** R builds the import command first and the lead freezes its contract sample, so the bridge codes against the real binary. The lead then integrates, runs both gates, and deploys in the order above before the live proof.

## Aster's review, as folded in

| Item | Where |
|---|---|
| F3-1 crash states, idempotent canonical delivery, receipt immutability, cross-host id collisions | Destination delivery; Crash states |
| F3-2 route blocks and the fence via post's own seam | Destination delivery, post's step 4; `ensure_route_allowed` |
| F3-3 conclusive lookups only; per-send snapshot | Destination delivery, post's step 4; retryable list |
| F3-4 one topology source, fail visibly, the bounded sender slice | The address; Sender side 1 |
| F3-5 origin frozen at import | Admission record; Receiving side |
| F3-6 published means pushed; full receipt validation; first valid wins; evidence validation | Sender acknowledgement; `post delivery` |
| F3-7 `@` scope; local semantics unchanged; own host reuses the local resolver | The address |
| F3-8 visible queued reasons; bounded logging for destinations without F3 | Delivery states |
| Answers 1–4 (final with conditions; admission point; participant mail only; new command) | Terminal reasons; Destination step 4; `post delivery`; Sender side |
| F3-R2-A admission record before inbox bytes; unrecorded file is a collision; completion from a matching record; stable receipt time | Destination steps 2–6; Crash states; Relay `at` |
| F3-R2-B pre-F3 outbound hazard; exclusion; capability guard; deployment order | Sender side 2; Outbound selection; Deployment order |
| R2 constraint: the import endpoint revalidates arguments and envelope | Destination, post's step 1 |
| R2 constraint: honest vocabulary, four normal states plus `unknown` | The change; Delivery states |
| R2 constraint: a durable terminal receipt wins; rejection crash cases; when a rejection is final | Crash states, Rejection |
| R2 constraint: frozen JSON and exit-code contract; failures stay retryable | The import command's contract |
| R2 constraint: one origin lookup across every comparison | Receiving side |
| r3 correction 1: receipts are tombstones, no pruning; replay-after-rejection test | Crash states, "Receipts are tombstones"; bridge tests |
| r3 correction 2: structural checks every attempt, mutable checks on first admission only, on both sides | Destination delivery, the bridge's part and post's steps 1 and 4; tests |
