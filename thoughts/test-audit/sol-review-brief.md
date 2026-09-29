# Sol review brief (read-only)

HARD RULE (Trey, after a CPU meltdown on the shared devbox): RUN NO TESTS, NO CARGO/RUSTC/BUILD, NO PYTEST, NO NODE. Reading files, rg, and git log/show only. Write only your review file.

Context: a test-value audit of the `post` Rust CLI in /home/trey-agent/Code/post-test-audit. The rules are in thoughts/test-audit/skill/SKILL.md and thoughts/test-audit/skill/CAMPAIGN.md (steps 3 and 4). Areas are defined in thoughts/test-audit/areas.md. A Claude Sonnet agent wrote the ledger named in your task. A finished, reviewed example: thoughts/test-audit/ledger-participant-gc.md with thoughts/test-audit/review-sol-participant-gc.md.

Your task (CAMPAIGN step 4 plus a correctness check of the ledger):
(a) Check every mark against the actual test and production code. Are the R marks really independent contracts, or do some duplicate a stronger test at another layer (a unit test vs a CLI integration test for the same behavior; tests in other areas' files)? Is each D backed by a named stronger remaining proof that really exercises the same failure? Is each C's absorbing keeper real? Is each F repair specified precisely enough to implement, and would it actually bind (fail when the guard is removed)?
(b) Name the keeper test for each contract in the area.
(c) Verify each suspected bug and each "dead production code / test-only seam" claim against the source: real or not, with file:line and the exact caller search you did.
(d) Flag any deletion that removes production code or changes a documented contract (CONTRACT.md, docs/) — those need the maintainer's decision, say so explicitly.
Calibrated: agreeing with the ledger is a fine result; do not invent deletions.

Output: write thoughts/test-audit/review-sol-<area-slug>.md with: disagreements per test (name, ledger mark, your mark, evidence file:line), keepers per contract, the FINAL edit list for this area in implementation order, maintainer-decision items, bug verdicts, open questions that need a test run.
