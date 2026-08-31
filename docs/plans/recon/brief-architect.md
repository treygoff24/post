# Architect pass: Plan B — post stateful read layer

You are the sole architect for Plan B of the `post` CLI (Rust, this repo). Your
job: produce the architecture that a plan author will compile into tasks. You
make design decisions and anchor every one in the real code. **No source
changes — your only output is one file: `docs/plans/recon/architecture.md`.**

## Required reading (in this repo)

- `docs/plans/plan-b-goal-lock.md` — the locked goal, acceptance demo, and
  rulings 1–10. These are settled; design within them, flag (don't reopen)
  any you believe are wrong.
- `CONTRACT.md` — especially "On-disk format" (arx fence, flock rules, writer
  admission) and "Commands".
- `src/commands/` — watch.rs (ring path around lines 266–273 is a hard
  invariant), chat.rs, read.rs, channels.rs, inbox.rs, schema.rs, doctor.rs.
- `src/` store/room internals — find where jsonl is read/appended, where the
  root flock is held, where fence admission happens.

## Locked rulings (context, do not relitigate)

1. Per-room-per-channel cursors + one per-room mail cursor.
2. Catchup must NEVER suppress watch rings; rings never advance cursors.
3. Retention/expiry out of scope.
4. Body previews in watch lines already shipped in 0.8.0 — not your problem.
5. CLI read-time framing (banner diet) is an open contract question — propose
   an answer, marked as proposal.
6. Cursor state lives in the room's own directory (e.g. `<room>/cursors.json`).
7. Consuming reads (plain `chat`, plain `read`) also advance the cursor.
8. Search is a linear scan, no index.
9. Unread counts are additive JSON fields on existing output.
10. Cursors are advisory: corrupt/missing → "all unread", never an error.

## Deliverable: `docs/plans/recon/architecture.md` covering

1. **Cursor file format** — exact JSON shape, atomic write strategy, what the
   cursor value is (message id? byte offset? line count? — justify against
   how jsonl ids/ordering actually work in this store).
2. **Fence integration** — cursor advance is a writer operation: where writer
   admission happens today (file:line), how catchup and consuming-read cursor
   advance slot into the existing flock-hold and generation rules without
   widening lock hold time. What stays read-only (`--peek`, listings,
   `--snapshot`) and how unread-count computation stays cursor-READ-only.
3. **`post catchup` semantics** — args (`<channel>`, `--all`?, mail), output
   shape (human + `--json`), interaction with existing `chat --since`, empty
   result behavior, and how it composes with the ring-line fencepost ids that
   shipped in 0.8.0.
4. **Unread counts** — exact computation per listing call, cost analysis
   (channels × messages scanned per `post channels` invocation on a store the
   size of the live one, ~600+ msgs), and whether a cheap length/offset trick
   avoids full scans.
5. **`post search`** — flags, visibility scoping (own mail + member channels
   ONLY — treat cross-room leakage as a security bug), output shape, pattern
   semantics (substring vs regex), result caps.
6. **Contract amendments** — the CONTRACT.md amendment outline, schema.rs
   additions, doctor checks (if any), smoke-installed.sh additions.
7. **Test seams** — how the doorbell invariant (both directions), fence
   refusal, concurrent-writer count correctness, and old-store degradation
   (acceptance row 7) each get a real test; name existing test files/harnesses
   to extend.
8. **Risk list** — top 5 ways this design fails in the field, each with the
   mitigation that's actually in the design.

Anchor claims with `file:line`. Where you weigh two options, record the ruling
and the losing option in one line each.

## Rules

- Write boundary: `docs/plans/recon/architecture.md` only. Do not modify src,
  CONTRACT.md, or tests. Do not commit. Never run tree-wide git state commands
  (stash/checkout/restore) — other lanes share this tree.
- Honest-credit floor: every file:line you cite must be one you actually read.
  If you didn't verify a mechanism, say "unverified" next to it. An "I
  couldn't determine X" is a valid finding; a guessed anchor is not.
