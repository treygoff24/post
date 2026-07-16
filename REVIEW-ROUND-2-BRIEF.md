# post — review round 2 (fresh adversarial pass after fix round 1)

You are reviewing the Rust crate in this directory at HEAD (commit "Terra fix
round 1"). A prior review round produced FIX-ROUND-1.md (in repo root); a fix
lane has since applied all 9 findings (B1, B2, M1–M7). Fix lanes introduce
regressions — that is why you exist. This is a fresh, adversarial, read-only
review. Trust nothing the fixer claimed.

## Your three jobs, in order

1. **Re-attack the laws in CONTRACT.md.** The structural laws (no-authority
   banner on every read; never edits existing rules.json; blocked routes are
   refused before any mail write; archive immutability) were verified CLEAN
   before the fix round. Verify they still hold at HEAD. Any regression here
   is a BLOCKER regardless of how small the diff looks.

2. **Confirm each FIX-ROUND-1 finding is actually fixed.** Read FIX-ROUND-1.md,
   then verify against the code — not the commit message — that each of B1, B2,
   M1, M2, M3, M4, M5, M6, M7 is genuinely resolved as specified, including the
   required test coverage. Report each as FIXED / PARTIAL / NOT FIXED with file
   and line evidence.

3. **Hunt new regressions.** The fix touched send.rs heavily (pipeline reorder:
   body → rules → exclusive-write → stdout → rename), lib.rs (exclusive-create
   helper, std-time id generation), cli.rs (control-char validation), read.rs,
   inbox.rs, rooms.rs, doctor.rs, error.rs. Look especially for: ordering bugs
   in the new send pipeline, error paths that leave partial state (temp files,
   archive-written-but-inbox-failed), id-regeneration retry loops, the new
   localtime_r time code (offset correctness, format byte-compatibility with
   `YYYYmmdd-HHMMSS` ids and `%Y-%m-%d %H:%M:%S %z` sent stamps), and
   sanitization that over-strips legitimate body content.

## Constraints

- On-disk format must stay byte-compatible with reference/post.py (Python
  original). Flag any divergence.
- CONTRACT.md is the spec. If code and CONTRACT.md disagree at HEAD, that is a
  finding.
- Severity anchors: BLOCKER = law regression or data loss; MAJOR = wrong
  behavior a real agent would hit; MINOR = everything else.

## Output

A single markdown report: laws verdict first, then the 9-finding fix
confirmation table, then new findings ordered by severity, each with
file:line evidence and a concrete failure scenario. No fixes — findings only.
