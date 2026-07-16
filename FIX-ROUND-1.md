# post — fix round 1 (adjudicated union of Sol xhigh + Cursor reviews)

Coordinator (Free Claude) adjudicated both lanes; Sol's severity anchors. All
four laws verified CLEAN by both — do not regress them. Fix these, keep the
gate green (fmt, clippy -D warnings, tests, release build), update tests to
assert the NEW behavior, and update CONTRACT.md where a fix changes the spec.
Do not commit — leave the tree for coordinator review.

## BLOCKERS

**B1 — rules.json can be clobbered by a create/replace race (Sol #1).**
`write_default_if_missing` does exists()-check then replacing `rename()`; a
human creating rules.json in between gets overwritten, and a dangling symlink
is treated as missing and replaced. Law #2 is "never edits rules.json" =
never changes existing content. Fix: create defaults ONLY via exclusive
create (O_EXCL / `create_new(true)`), never a replacing rename; treat an
existing file OR any symlink at that path as present (never replace). This
applies to first-run init AND doctor --fix. First-run bootstrap of a MISSING
file stays allowed — that's not an edit. Update the test that currently
asserts plain creation to assert exclusive-create + no-clobber.

**B2 — blocked-route check uses a stale rules snapshot (Sol #2 / Cursor implicit).**
Rules are read before the body (slow stdin/FIFO), so a rule added mid-read is
missed and mail delivers on a now-blocked route. Fix: read the body FIRST,
then load rules, then check the route, then write. Re-check must be the last
thing before the first filesystem write of mail. Add a test simulating
body-after-rule-add ordering (feed body via a controllable stream).

## MAJOR

**M1 — read marks mail read before stdout delivery (Sol #6 / Cursor #2, CONVERGENT).**
Rename inbox->read happens before the body is written to stdout; a broken
pipe loses the message permanently. Fix: write the full output to stdout
FIRST, and only rename to read/ after stdout succeeds. --peek still never
renames. Test: simulate stdout write failure, assert mail still in inbox.

**M2 — atomic write clobbers on id collision (Sol #7 / Cursor #4, CONVERGENT).**
Existence-check then replacing rename; two sends with the same id, or a
target created after the check, silently overwrite delivered/archived mail —
breaks archive immutability. Fix: final write via exclusive create
(create_new) for BOTH inbox and archive paths; on collision, regenerate the
id (bounded retries) and retry. Keep temp+fsync+rename only where the target
is guaranteed unique. Test: force an id collision, assert no overwrite.

**M3 — stdout failure reports a delivered send as retryable (Sol #8).**
After archive+inbox succeed, a stdout failure emits io_error/retryable:true
advising re-run -> duplicate mail. Fix: once delivery commits, stdout failure
is NON-retryable and must not advise re-running; exit reflects "delivered but
could not print receipt." Coordinate with M1's reorder. Test asserts
retryable:false in this path.

**M4 — terminal injection via --from / --subject / body (Sol #4 / Cursor #5).**
Control chars pass through into the raw text banner; a newline in --from
injects authoritative-looking header lines above the no-authority warning, and
a body of terminal escapes can erase the banner after printing. Fix: reject
control characters (including newlines, ESC, CR) in --from and --subject at
the clap parse boundary with a clear error. For body: in text read mode, the
banner must be robust — print body such that it cannot rewrite already-printed
lines (e.g. strip/escape C0 control chars except \n\t in text mode, or note
the limitation and sanitize). JSON mode already safe. Tests for each.

**M5 — message id derived from PATH `date`, enabling path traversal (Sol #5 / Cursor #6).**
Shelling to `date` and trusting its output; a hijacked `date` returns
`/abs|...` and PathBuf::join escapes the mailbox. Fix: DELETE the `date`
subprocess entirely — generate the timestamp with Rust std (SystemTime +
a local-offset formatter; keep the exact `YYYYmmdd-HHMMSS` id format and the
`%Y-%m-%d %H:%M:%S %z` envelope `sent` format). Removes an external dep, makes
it hermetic and faster. Also run validate_envelope/id-format on the WRITE path
as defense-in-depth. Test: id + sent formats byte-match the old format.

**M6 — one malformed .mail hard-fails the whole inbox (Cursor #3).**
A single garbage/truncated file makes `post inbox` return config_invalid and
list nothing — a shared-inbox DoS. Fix: inbox SKIPS unparseable entries
(optionally one stderr warning line each), lists the good ones, exits 0.
doctor is where malformed files get reported. Test: drop a garbage file
alongside good mail, assert good mail still lists.

**M7 — `post rooms` attaches from:* rules to every room (Cursor #1).**
Filter shows a rule if from==* OR to==*; the armed `from:* -> agent-memory`
rule thus marks claude-space and pact as blocked. Fix: match Python — a room
is marked only by rules where `to in ("*", room)`. A `from:*` rule attaches
to its recipient, not every sender. Test asserts only agent-memory shows the
armed block.

## Notes for the fixer
- These cluster on send.rs (ordering: body->rules->exclusive-write->stdout->
  rename) and lib.rs (exclusive-create helper, std-time id gen, component
  validation). Reason about them together; the reorder in B2/M1/M3 is one
  coherent pipeline change, not three.
- Preserve on-disk format compatibility with reference/post.py exactly.
- If a fix changes CONTRACT.md's stated behavior (e.g. doctor --fix wording,
  the "refuse before any write" clause re: scaffolding), update CONTRACT.md in
  the same pass and say so.
