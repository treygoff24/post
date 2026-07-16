# post — fix round 2 (adjudicated union of Sol xhigh + Cursor, review round 2)

Coordinator (Free Claude) adjudicated both lanes; Sol's severity anchors, with
one downgrade noted inline. Note for calibration: Cursor declared all four laws
CLEAN and all 9 round-1 findings FIXED; Sol failed three laws. Where they
diverge, the file:line evidence decides — and Sol's evidence held on every
divergence checked. Same rules as round 1: fix these, keep the gate green
(fmt, clippy -D warnings, tests, release build), update tests to assert NEW
behavior, update CONTRACT.md where a fix changes the spec. Do not commit.

## BLOCKERS

**R2-B1 — read's rename can clobber existing read/ mail (CONVERGENT: Sol B3 +
Cursor's sole blocker; law #4 "nothing deletes mail").**
`src/commands/read.rs:89-103` does `destination.exists()` then plain
`fs::rename` — POSIX rename replaces the destination. Fix: publish into
`read/` with the same exclusive no-replace discipline as mail writes
(`hard_link` source→dest, then remove source; on `AlreadyExists`, error
without touching either file). Test: pre-create `read/<id>.mail`, attempt the
read, assert both files survive and the command reports the collision.

**R2-B2 — framing banner can be suppressed/rewritten via envelope fields at
text render (Sol B1; law #1).**
`render_text` (`src/commands/read.rs:107-127`) interpolates `from`, `sent`,
`subject` raw; only the body is stripped. Clap validation protects only mail
sent through THIS binary's flags — Python-originated mail, hand-written .mail
files, and cwd-inferred sender names with control chars all reach render
unsanitized. An ANSI conceal in `from` hides the no-authority banner. Fix:
apply the same C0-strip (except \t) to from/sent/subject at text render time
— sanitize at the render boundary, not just the parse boundary. JSON mode
stays raw. Test: hand-write a .mail with ESC sequences in from and subject,
assert text read output contains no control bytes and the banner is intact.

## MAJOR

**R2-M1 — archive-first publication orphans mail and advises blind retry
(CONVERGENT: Sol M1 + both Cursor majors — one cluster, fix together).**
`src/commands/send.rs:111-135`: archive publishes before inbox. Two failure
arms, same seam: (a) inbox fails non-AlreadyExists (ENOSPC, EACCES) →
retryable io_error while the archive copy already exists; retry = new id =
orphan archive entry never delivered. (b) inbox AlreadyExists → `continue`
to a new id, leaving the just-written archive body behind under the old id —
same-id archive/inbox split-brain. Fix: publish INBOX FIRST (the deliverable
copy), then archive; an archive failure after inbox success is a delivered-
but-unarchived warning (non-retryable, named error, doctor can reconcile).
On inbox AlreadyExists, retry a fresh id BEFORE any archive write. Law #4
means no rollback-by-delete — so order the writes such that no failure arm
strands an orphan. Update CONTRACT.md's write-order clause to match. Tests:
force inbox failure post-archive ordering away (assert no orphan), and the
id-collision path (assert archive untouched until inbox commits).

**R2-M2 — exclusive_atomic_write reports failure after successful publish
(Sol M2; Cursor filed same bug MINOR — Sol anchors).**
`src/lib.rs:479-494`: `hard_link` lands the file, then temp-unlink or dir
`sync_all` failure returns Err → caller treats a committed write as failed,
retries, duplicates. Fix: once `hard_link` succeeds the write is COMMITTED —
downgrade subsequent temp-cleanup/sync failures to a non-retryable warning
(stderr note + success result), never an Err. Test via injected failure if
feasible; otherwise restructure so the commit point is explicit and unit-test
the result classification.

**R2-M3 — `post schema` advertises io_error as retryable:true exit 75,
contradicting the runtime's delivered-output path (Sol M3; round-1 M3 is only
half-done).**
`src/commands/schema.rs:88-103` + `CONTRACT.md:83-89` still describe only
retryable io_error; runtime emits exit 70 retryable:false after delivery.
An agent generated from the schema will resend delivered mail. Fix: schema
and CONTRACT.md gain the delivered-output-failure variant (exit 70,
retryable:false); keep plain io_error (75, retryable) for pre-commit
failures. Test: schema output includes both, matching error.rs.

**R2-M4 — inbox silently hides valid mail on I/O errors (Sol M4).**
`src/commands/inbox.rs:12-15` discards ALL parse_mail errors via `let Ok(...)`.
M6 wanted malformed-envelope skips; an EACCES/transient read error on a valid
file now vanishes silently — agent concludes inbox empty. Fix: distinguish
error classes — malformed envelope = skip (one stderr warning line, as M6
specified); I/O error opening/reading = stderr warning + nonzero-signal in
the JSON envelope (e.g. `"skipped_unreadable": n`), still exit 0 listing the
good ones. Test: chmod 000 a valid mail file, assert warning + count field.

## MINOR

**R2-N1 — non-ASCII envelope bytes diverge from the Python reference (Sol N1).**
Rust writes literal UTF-8; Python's json.dumps default escapes (`café`).
Spec requires byte-compatible .mail files. Fix: ASCII-escape non-ASCII when
serializing envelopes (match Python's ensure_ascii=True + indent=2 exactly).
Test: subject `café` byte-matches the Python-produced file.

**R2-N2 — timestamp tests check shape only (Sol M5-residual + Cursor).**
Add a fixture test: `format_local_timestamp(known_epoch)` under a pinned TZ
(e.g. `TZ=Asia/Kathmandu` and a negative-offset zone) equals exact expected
`YYYYmmdd-HHMMSS` and `%Y-%m-%d %H:%M:%S %z` strings.

**R2-N3 — M2 test debt: send-level id-regeneration is untested (Sol).**
Cover the send-loop retry path end-to-end, not just the helper: pre-create a
colliding inbox id, assert send retries to a fresh id with no clobber and no
orphan (dovetails with R2-M1's tests).

## Adjudication notes

- Sol's B2-residual (rules snapshot not literally last-op-before-write) is
  adjudicated MAJOR-fold-in, not blocker: fold into R2-M1's reorder — load
  rules and check the route immediately before the first inbox write, after
  payload construction. The TOCTOU window can't be zero without locking;
  minimize it and keep the round-1 test.
- Cursor's text-mode `\r`-strip finding: ACCEPTED behavior per CONTRACT
  (banner safety beats byte-fidelity in text mode; JSON mode is the
  byte-faithful surface). No change; CONTRACT.md may note it explicitly.
- Cluster hint: R2-B1, R2-M1, R2-M2 are one discipline ("a write is committed
  at hard_link; order writes so no failure arm strands or clobbers") — reason
  about them as one pipeline, like round 1's B2/M1/M3.
