# conventions — P2

_lane: conventions:omp:glm · alias omp-4 · exit 0 · 10840 bytes · review-shaped: yes_

=== completionReport ===
CONVENTIONS review complete. All evidence gathered statically; report follows.

---

**Delegate completion report**

- Status: completed.
- What I did: conventions review of commit `5d7ae30` ("send/read: a message the sender can be told about is a message the sender can read") against the house rules (global CLAUDE.md, repo CLAUDE.md/AGENTS.md, CONTRIBUTING.md invariants, CONTRACT.md, shell-footguns rules where reachable), plus static verification of every claim the parent asked me to attack, including the leak reasoning and the a8383e6 injection-shape regression check.
- Files reviewed: `src/commands/read.rs`, `src/commands/send.rs`, `src/commands/schema.rs`, `src/mailbox.rs` (identity/resolution/validators), `src/error.rs`, `tests/cli.rs` (targeted: the three new tests, the pre-existing read-miss pins, Sandbox/run_fix helpers, schema contract test), `CONTRACT.md`, `CONTRIBUTING.md`, `CHANGELOG.md`, `README.md`, `skills/post/SKILL.md`, `.beads/`.
- Verification: **this environment exposes file reads only — no shell.** `git show`, `cargo test`, and `./scripts/gate.sh` could not be executed; git objects are packed/zlib and unreadable via `read`, so the diff was reconstructed from the post-fix tree (HEAD confirmed at `5d7ae30` via the worktree ref) plus the commit's own comments, tests, and the task's description of pre-fix behavior. Every claim-check below is static and marked accordingly; red/green and gate status are [INFERENCE].
- Remaining risks: the `--from` receipt lane in the claims section is the load-bearing one; test-execution claims remain unverified by me.

---

CONVENTIONS: 2 findings

```
[RULE] The read command's behavior changed; every published contract surface still
describes the old one · src/commands/schema.rs:73 (also CONTRACT.md:175, CONTRACT.md:201-202)
Rule: CONTRIBUTING.md § Invariant 2 — "`post schema` is the contract. If you change a
command, flag, error code, or envelope shape, update the schema and the tests that pin it
in the same change" (CONTRACT.md carries the invariants per CONTRIBUTING.md § Invariants).
Fix: edit the read side_effects string at src/commands/schema.rs:73 from "archive copies
addressed to that room" to name both parties ("archive copies that room sent or that are
addressed to it"); amend CONTRACT.md:201-202 the same way and CONTRACT.md:175 to name the
new readback receipt line. No test pins the old phrase (help_and_schema pins command
names, watch usage, and substrings only — tests/cli.rs:331-395), so there is no test churn;
extending that test to pin the new phrase would match the house's schema-pinning style.
```

The archive filter now admits `envelope.from == room` (src/commands/read.rs:109), but `post schema` — which skills/post/SKILL.md designates "the exact contract when docs or memory disagree" — still teaches an agent that a sender cannot read back its own mail: the exact misconception this commit exists to kill, surviving on the one surface agents are told to consult first. CONTRACT.md drifts twice more: its read spec repeats the stale sentence (:201-202), and its send spec pins the text receipt as exactly one line — "Success (text): `post: sent <kind> <id> <from> -> <to>`" (:175) — while src/commands/send.rs:349-356 now emits two lines. The schema's output_shapes, codes, and usage strings are genuinely unchanged, so the parent's "no error code, no envelope shape" belief holds; this is contract-text drift, not shape drift, but Invariant 2's letter covers it.

```
[RULE] Four user-visible changes ship with no Unreleased entry · CHANGELOG.md:3-9
Rule: repo changelog practice — the P1 conventions review established it as a finding
(commit 238c1c4, docs/reviews/P1/conventions.md), and 0.6.0 logs comparable UX deltas
(the --digest entry above shows the current practice in this very section).
Fix: add Fixed/Changed bullets under Unreleased naming the four surfaces: sender archive
readback, the receipt's readback line, the split not_found wording, and `--body-file -`
as the stdin sentinel.
```

Worth one extra sentence: the parent commit `a8383e6` also shipped entryless, so the practice is decaying mid-wave rather than being a one-off miss — the next wave should catch both.

**Claims under attack — static verification** (no shell available; nothing was executed):

- **"Admitting `from == room` leaks nothing" — HOLDS, for a stronger reason than the parent's.** Archive visibility was never an authorization boundary. `post read <id> --room X` resolves any path-safe room name with no registration, containment, or credential check (`resolved_room_with_provenance` → `validate_room_name` only, src/mailbox.rs:307-337 and :543; `mailbox_dirs` builds paths for any valid name), so a third room could always read alpha's live inbox — bodies included — by simply asking for it. The party filter is a scoping convenience on top of an open store, consistent with the no-login/no-credential design. `from` forgery is irrelevant to it because the reading room never consults `envelope.from`: `--from` a registered room outside its tree is refused (src/mailbox.rs:392-419), free-form `--from` is allowed by design but changes nothing about read resolution, and the POST_FROM pin bypasses containment deliberately (specimen 21) yet grants only what `--room` already grants. Re-registering a name onto another room's path is refused (duplicate/case-fold/symlink-alias checks, CONTRACT.md § Commands `rooms add`), and even a successful impersonation would add no capability `--room` doesn't already give. Nothing else in the tree treats the filter as authz — the archive's only consumers are this fallback and doctor's byte-comparison.

- **The one claim that does not survive: the receipt's promise is false for free-form `--from` senders.** The new receipt emits a bare `post read <quoted id>` (src/commands/send.rs:349-356), but `post read` resolves its room from `--room`/POST_FROM/cwd — never from the envelope's sender (src/mailbox.rs:307-337). Send with `--from codex-<project>` (free-form, allowed, and the exact usage the ReservedSender error itself recommends, src/mailbox.rs:406-409) from alpha's tree: `envelope.from` is `codex-foo`, the reading room is `alpha`, the filter at src/commands/read.rs:109 matches neither — so the very shell that just got the receipt is told the mail "is in the archive but addressed between two other rooms" (src/commands/read.rs:126). That is false, and it is the same receipt/read disagreement this commit exists to kill, now with an actively misleading message instead of a silent miss. The lane is unpinned: the receipt test (tests/cli.rs:9031) uses cwd identity only. Mechanical fix: emit `post read {id} --room {shell_quote(from)}` in the receipt — correct for cwd, pin, and flag senders alike (a free-form sender that collides with a reserved room name would need the bare form withheld; that lane fails today too).

- **The honest branch cannot swallow the ordinary one — confirmed.** The branch keys on `archived_elsewhere = party.is_empty() && !candidates.is_empty()` (src/commands/read.rs:113): a genuinely absent id leaves `candidates` empty and keeps the original wording; a foreign-party id flips it. Pinned twice — the new test's absent-id block (tests/cli.rs:8994-9001, within the test starting at :8967) and the pre-existing tests/cli.rs:3522, which pins all three store names in the ordinary wording. Sufficient for the two intended lanes. One narrow edge remains: a candidate that fails `parse_mail` is dropped from `party` but still counted in `candidates` (src/commands/read.rs:104-112), so a corrupt or hand-edited archive file matching the prefix yields "addressed between two other rooms" — an addressee the code never established. Same assertion-without-check shape, rarer lane (requires corruption of the exact file being asked about). Mechanical fix: partition candidates into parse-failures vs foreign-party and report the corruption (suggest `post doctor`) instead.

- **A real `--body-file` path still wins over piped stdin — confirmed.** `read_body_unchecked` takes the body-file branch before ever touching stdin (src/commands/send.rs:488-494), and the test supplies both at once (tests/cli.rs:9059, second block: file body asserted present, piped stdin asserted absent). `--body-file -` reads stdin (first block), and the deprecated positional FILE inherits the same sentinel via `body_file.or(file)`.

- **The receipt's readback command is extracted and executed, not string-matched — confirmed.** tests/cli.rs:9031-9056 finds the "read it back with" line, slices between the first and last backtick, and runs the result through `sh -c` via `run_fix` (tests/cli.rs:106-131), which resolves `post` to the binary under test.

- **All three tests red at `a8383e6`, green at `5d7ae30` — plausible, not executed** [INFERENCE]. Statically each asserts post-fix-only structure: the sender readback requires the `from == room` admission; the gamma assertions require the new tail text and the negated "not in the archive"; the receipt test requires the "read it back with" line to exist; the dash test requires the piped body to be delivered where the parent opened a literal file named `-`.

- **No reintroduction of the a8383e6 injection shape — confirmed.** Every new command string is built with `shell_quote`: the receipt's id (src/commands/send.rs:355), the miss-path `exact_fix` (src/commands/read.rs:120), and the ambiguous-prefix fix (src/commands/read.rs:170-176, quoting both id and room). Envelope ids are generated `YYYYmmdd-HHMMSS-<6 hex>` (CONTRACT.md § On-disk format), so the receipt quoting is belt-and-braces; no unquoted path, name, or body interpolation appears anywhere in the new strings.

- **No error-code or envelope-shape change — confirmed statically.** `ErrorCode::ALL` is still 14 codes (src/error.rs:74); the split message reuses `not_found`/66 and `ambiguous_id`; `SendOutput` and `ReadOutput` shapes are unchanged, with `already_read: true` semantics pre-existing (schema output_shapes, read_json). The residue is Finding 1's contract-text drift.

**Notes.** The untracked `.beads/.auto-import-issues.jsonl` contains only a size/mtime marker record; nothing to review. Commit-message conventions (72-char subject, soft-wrapped body) are unverifiable here — the commit object is packed and unreadable via `read`. Skills: this environment's `skill://` pool reports none available; the always-on list (~/.claude-shared/skills.globals) was read, and the relevant entries (`code-review`, `post`) were unreachable — domain context was taken from the repo's own `skills/post/SKILL.md` instead, and the parent's mandated report shape governs. New comments in the diff are why-comments in the repo's established incident-citing style (read.rs:100-103, send.rs:345-348, send.rs:489-493); no slop findings.
