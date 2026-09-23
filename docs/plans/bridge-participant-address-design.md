# Design: participant DM across hosts (lane F3)

Status: draft for Aster's review. No code until Aster approves. Depends on wave 1 (R7, F1) being merged and F2 (bridge v2 live on both hosts). Author: Nightjar.

## What Trey asked for

"DM any agent anywhere by name." Today the Mac cannot address one devbox participant. `participant:<id>` resolves only against the local store (`src/participant.rs` `resolve_target`), and the bridge carries only workspace mail: its envelope `to` must be a room and must equal the outbox path's room (`sweep.py` `parse_envelope`). The workaround is to ssh to the other host and run post there.

## The change in one sentence

Add one host-qualified address, `participant:<id>@<host>`. Post queues the letter locally. The bridge carries it in a new relay namespace. The destination host resolves the id against its own participant list and delivers or rejects it explicitly. The sender can always see which of four states the letter is in.

## What stays the same

- Workspace mail, room publication, channels, and every existing envelope and receipt shape are unchanged.
- There is no global participant directory. The sender names the host; only the destination knows whether the id exists.
- There is no `room@host` form. SPEC-v2's names rule stands for rooms.
- The bridge still authenticates the host through the forge branch; nothing in an envelope is a credential.
- Relay principals can read relayed DMs, as they can read workspace mail today (SPEC-v2 non-goals). Participant DMs add no confidentiality.

This amends one SPEC-v2 non-goal ("Session-level addressing. A DM goes to a room, not a session"). The amendment: an explicit host-qualified participant address is the only session-level form, and it never falls back to a room.

## The address

`participant:<id>@<host>`, split at the last `@`.

- `<id>` follows post's participant-id grammar. Post-generated ids are `<harness>-<hex>` and never contain `@`. F3 also forbids `@` in `validate_participant_id`, so the split can never be ambiguous.
- `<host>` must match `^[a-z0-9-]{1,32}$` (the bridge's `HOST_RE`) and appear in the enrolled registry. Post reads the bridge's persisted copy at `$POST_MAIL_ROOT/bridge/registry/hosts.json`. If there is no valid copy, post uses the effective peers in `bridge/config.json`.
- `<host>` equal to this host's own `bridge/config.json` `host` resolves locally, exactly as `participant:<id>`.
- Errors are explicit and nothing falls back:
  - `unknown_host`, naming the enrolled hosts;
  - `no_bridge` when this host has no bridge config;
  - an unknown or ended id for the local form, as today.

## The sender side (post)

1. **Refuse an unroutable sender at send time.** The destination verifies `from` as a room published by the origin host (SPEC-v2 trust fact 2) and otherwise quarantines the letter as `unpublished_sender`. So post refuses a remote participant send when the acting participant's `from` is not a real local room (a rooms.json entry that is not under `remote/`). The error is `remote_sender_unroutable` and names the fix: `post participant bind --workspace <room>`.
2. **Write the letter** to `archive/<mail-id>.mail`, which is the same archive the bridge already selects outbound mail from. Envelope differences from local participant mail:
   - `to` = `<id>`;
   - `address_kind` = `participant`;
   - a new optional `to_host` = `<host>`.

   `from_participant`, `from_lineage`, `sender_provenance`, and the profile stamps are recorded as for any send. There is no local inbox write, because no local participant exists. Post's `Envelope` has no `deny_unknown_fields`, so older readers accept `to_host`.
3. **The send receipt never claims delivery.** The send JSON gains an optional `delivery` object: `{"state":"queued","host":"<host>"}`. The text form says "queued for <host>; not yet delivered." Schema entry updated.
4. **`post delivery <mail-id> [--json]`** is a new read-only command that reports the durable state:

| State | Evidence on the sending host |
|---|---|
| `queued` | the letter is in `archive/`, with no `bridge/published/<mail-id>` marker |
| `published` | `bridge/published/<mail-id>` exists (the bridge wrote it to the relay; contains the relay head) |
| `received` | `bridge/acked/<mail-id>.json` holds the destination's receipt with status `delivered` |
| `rejected` | `bridge/acked/<mail-id>.json` holds status `rejected` and its reason |

   The command reads files only and never infers delivery from age. A letter that stays `published` shows its age.

## The relay (bridge)

A new namespace, so participant ids can never collide with room names (`codex-5a036212` is also a valid room name), and so older sweepers ignore it: they list only `outbox/<self>/` and `receipts/<self>/` on peer branches.

- `pmail/<dest-host>/<id>/<mail-id>.mail`: the letter's bytes, unchanged from the archive.
- `preceipts/<origin-host>/<id>/<mail-id>.json`: `{"v":1,"status":"delivered"|"rejected","host","participant","id","sha256","reason","at"}`, sorted keys, at most 4 KiB.

**Outbound select.** An archive letter whose envelope has `address_kind: participant` and a `to_host` that is an effective peer is copied to `pmail/<to_host>/<to>/<mail-id>.mail`. It gets the same `published` marker as workspace mail. `to_host` equal to the local host, or not an effective peer, is logged as `pmail_unroutable` and not relayed. The send-time check makes that rare.

**Inbound, on the destination**, per `pmail/<self>/<id>/<mail-id>.mail` on a peer branch H, in order:

1. The v1 path, mode, size, and readability checks, as for outbox entries.
2. `parse_envelope`, extended for this namespace:
   - `address_kind` must be `participant`;
   - `to` must equal the path's `<id>`;
   - `to_host` must equal this host.

   A mismatch is `rejected: to_mismatch`.
3. Trust fact 2 on `from`: it must be a placeholder homed at H. Failures are `forged_from`, `name_collision`, or `unpublished_sender`, exactly as for workspace mail.
4. `from_participant` must be present and match the id grammar. It is data, never a credential.
5. The destination runs `post participant list --json` once per tick. The id must exist with `ended_at` null; otherwise `rejected: unknown_participant` or `rejected: ended_participant`. A stale lease is not ended, so the letter is delivered and waits in that inbox.
6. **Exactly once.** A ledger at `bridge/pdelivered/<H>/<id>/<mail-id>` records the sha256. A replay with the same sha skips delivery and rewrites nothing. A different sha under the same id is `rejected: id_collision`, preserved forensically.
7. **Deliver** by exclusive create into `participants/<id>/inbox/<mail-id>.mail`: the same path, shape, and create discipline that a local `post send` to `participant:<id>` uses (`exclusive_atomic_write`). Then write the ledger, then the `delivered` receipt.

**Rejections are final.** Unlike `unknown_room`, which SPEC-v2 re-evaluates each tick because rooms can appear later, a participant id is derived from a conversation key that already exists or never will. A final rejection ends the retry loop and tells the sender the truth.

**On the sender, per receipt** at `preceipts/<self>/<id>/<mail-id>.json` on the destination branch:

1. Validate it the way `parse_receipt` does: exact keys, and host, id, and sha must match the pmail entry.
2. Write `bridge/acked/<mail-id>.json` locally with an exclusive create. It is immutable once written.
3. Prune the `pmail` entry on either final status.

A destination still running a bridge without F3 never writes a receipt, so the letter stays `published` and its age is logged every tick. It never reads as received.

## The receiving side (post)

- **Remote origin evidence.** The delivered letter's `from` is a placeholder under `remote/<H>/<room>`. Post's existing rule (`output.rs` `reply_metadata`) already calls that `origin: remote`. R7 ensures a remote-origin letter is never treated as the local participant's own, even when its `from_participant` equals a local id. That collision is exactly what F3 makes possible.
- **Reply address.** For a remote-origin letter with `from_participant`, the reply target becomes `participant:<from_participant>@<H>`. H comes from the verified placeholder path, never from anything the envelope asserts. Workspace replies stay available as `reply_to_shared`. The existing test that a remote participant id is never promoted to a local `participant:` address still holds: the new form is always host-qualified.

## Tests

- **Post (Rust):**
  - Address parsing: the last-`@` split; host grammar; unknown host; no bridge; the own-host form resolving locally; `@` refused in new ids.
  - Send: the archive-only write; the `queued` receipt in both JSON and text; the unroutable-sender refusal.
  - `post delivery`, one fixture per state.
  - Reply metadata: remote origin with a placeholder gives `participant:<id>@<H>`; remote origin without `from_participant` gives no participant reply.
  - The R7 collision fixture, extended to a delivered pmail letter.
- **Bridge (Python, three-root harness):**
  - A Mac-to-devbox letter is delivered to the right participant inbox.
  - Unknown and ended ids are rejected as final, and the sender records `rejected` with its reason.
  - Path and envelope mismatches are rejected.
  - A forged `from` gives `forged_from`.
  - Replaying the same bytes is a no-op; the same id with different bytes gives `id_collision`.
  - A duplicate outbox entry is handled; a pre-F3 destination leaves the letter `published` forever and never `received`.
  - Provenance keys arrive byte-identical, with no `unknown_envelope_keys` line.
  - A reply round trip uses the reply address.
- **Live proof (plan P6):** a nonce DM from a Mac participant to a devbox participant, and one in the other direction. Each is acknowledged by the real recipient replying to `reply_to_participant`, and the reply's arrival is confirmed through `post delivery`. Then:
  - replay one relay commit and confirm canonical state is unchanged;
  - send to an unknown id and to an ended id and see both rejections;
  - confirm `from_participant`, `from_lineage`, and `sender_provenance` survive byte-identical.

## Build split

- **Post (Rust):** a follow-up turn on lane R, which holds the address and routing code. Scope: parsing, the send path, `post delivery`, reply metadata, and schema.
- **Bridge (Python):** a follow-up turn on lane F1 on the devbox, which holds bridge v2. Scope: `pmail`/`preceipts`, select, inbound, receipts, `acked`, health counts, and the SPEC-v2 amendment.
- The lead integrates, runs both gates, deploys through the F2 path, and runs the live proof.

## Open questions for Aster

1. **Final rejection for unknown ids** instead of `unknown_room`-style re-evaluation. My lean is final, for the reason above.
2. **A participant that ends between the destination's check and its write** gets the letter in an ended inbox, marked `delivered`. That matches a local send racing `post participant end`. Accept it, or recheck after the write and rewrite the receipt? My lean is to accept it: receipts are immutable, and rewriting one is worse.
3. **`bridge/acked/` for participant mail only tonight**, leaving workspace mail's receipt handling exactly as SPEC-v2 has it. My lean is yes, with workspace delivery states as a later, separate change.
4. **A new `post delivery` command**, rather than a flag on `post read` (which reads received mail and consumes). My lean is the new command, since it is read-only and has one job.
