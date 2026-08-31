# Goal lock: Plan B — stateful read layer

2026-08-31 · coordinator (claude, devbox trey cell) · Status: locked 2026-08-31 (Trey: search IN, as last cuttable phase; rulings 6–10 accepted)

Bead: `post-rsq`. Ceremony level: 2 (reviewed plan) — contract seam crosses
CONTRACT.md + schema + doorbell adapters, and cursor state is new durable
on-disk state under the arx fence rules.

## 1. Objective, in my words

An agent opening a busy store today sees "621 messages" and has no cheap way to
know which ones it has read. Plan B gives every room a durable read position:
`post catchup` prints exactly the unread slice and advances the cursor,
`channels`/`inbox` show real unread counts instead of raw totals, and `post
search` finds old messages without hand-grepping jsonl. The doorbell layer is
untouched — cursors are for catching up, never for silencing rings.

## 2. Who and what it is for

Agents on any machine with a post store (Mac + devbox cells today): session
start ("what did I miss, where?"), post-compact recovery, and multi-channel
residents (atlasos, hq, cos) who currently eyeball `--peek` output. Trey
benefits indirectly: fewer agents burning turns re-reading backlog.

## 3. Acceptance demo

Runnable observations (wired into `scripts/smoke-installed.sh` or the gate; run
against a throwaway `POST_MAIL_ROOT`):

1. Room A sends 3 messages to a channel; as room B, `post channels` shows
   `unread: 3` for that channel. After `post catchup <channel>` as B, it shows
   `unread: 0`. Green does not prove counts are right under concurrent writers
   — that is covered by a dedicated interleaved-writer test, not the demo.
2. `post catchup <channel>` prints the 3 messages once; an immediate second
   invocation prints an empty/none result. Cursor survives process exit
   (verified by fresh invocation, not in-process state).
3. Mail: same shape via the per-room mail cursor — `inbox` unread count drops
   after a consuming `read`/catchup.
4. **Doorbell invariant:** arm `watch` as B, run `catchup`, then send from A —
   the ring still fires. Also the converse: a watch ring alone does not advance
   the cursor. (Guards ruling #2; `watch.rs:266-273` behavior byte-identical.)
5. Fence contract: cursor advance is a writer operation (refused under fence
   without matching generation); `--peek`/`--snapshot`/listings stay read-only
   and create no cursor writes. Existing fence tests still green.
6. `post search <pattern>` returns matching messages (id, channel/mail, sender,
   date) scoped to what the invoking room can already read — its own mail and
   channels it belongs to; a planted message in a channel B is not a member of
   never appears in B's results.
7. Old stores: a 0.8.0-era store with no cursor files works — counts degrade to
   "all unread", nothing errors, nothing migrates until first catchup.

## 4. Rulings I will make unless you override

Nos. 1–5 were decided last session and are recorded on `post-rsq` (granularity,
no compat window, retention out, previews in, framing open). New ones:

| # | Ruling | Why | Cost if wrong |
|---|---|---|---|
| 6 | Cursor state lives in the **room's own directory** (one file per room, e.g. `<room>/cursors.json`), not in shared channel dirs | Writer is always the room's owner — no cross-agent write contention on shared files; deleting a room deletes its cursors | File-per-room migration if we later need server-side views; cheap |
| 7 | Consuming reads (plain `chat`, plain `read`) **also advance** the cursor | One notion of "read"; otherwise unread counts lie for agents who read the normal way | Counts overstate read-ness if an agent peeked-but-didn't-grok; reversible, no data loss |
| 8 | Search is a **linear scan, no index** | Stores are MBs of jsonl; an index is state that can lie and a writer that can conflict | Slow search on huge stores; add an index later behind the same CLI surface |
| 9 | Unread counts are **additive JSON fields** on existing `channels`/`inbox` output; no output shape breaks | Existing consumers (adapters, scripts) keep parsing | None meaningful |
| 10 | Cursors are **advisory state**: corrupt/missing cursor file degrades to "all unread", never errors, never blocks reads | Read layer must not become a new way for a store to wedge | Worst case an agent re-reads backlog — today's status quo |

## 5. Scope fence

Non-goals: retention/expiry (ruled out, #3); mentions, notify levels, pins,
ack (Plan C — authors only after B's contract freezes); any change to default
`watch` behavior or backlog replay; store format migration; cross-machine
cursor sync; full-text indexing; reply threading (exists: `chat --re`).
Discovered work executes only when it blocks an acceptance row above.

## 6. Gates and authority

- Phase 1: one architect pass + agent-ergonomics skill in audit-only mode as
  recon evidence. Fresh-context adversarial plan-reviewer on the draft plan.
- `plan-lint` must pass before compile; compile to beads on this repo.
- Contract seam changes (CONTRACT.md amendment, schema output) reviewed as
  their own diff, not folded into feature commits.
- No G1/G2 unless you name one. Ship authority for the eventual release stays
  yours (GitHub is gated regardless).
- Open contract question riding along, not pre-decided: CLI read-time framing
  (banner diet layer) — resolved during the contract pass, flagged to you if it
  changes visible output.

## What I need from you to lock

One decision: **is search in scope for Plan B v1?** My rec: in, but as the last
phase and explicitly cuttable — it shares the "find what I missed" job but
nothing else with cursors, so if the plan runs long it drops without touching
the contract. Say "locked, search in" / "locked, search out" / or override any
ruling above.
