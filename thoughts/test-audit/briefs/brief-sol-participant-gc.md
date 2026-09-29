# BRIEF: independent review of a test-audit ledger

**DO NOT RUN ANY TESTS. DO NOT RUN CARGO, RUSTC, OR ANY BUILD COMMAND. DO NOT RUN PYTEST OR NODE. THE MACHINE WAS JUST OVERLOADED. READ FILES AND USE `git log` / `git show` ONLY.**

PRE-APPROVED: this is a read-only review; the only file you write is thoughts/test-audit/review-sol-participant-gc.md. Do not edit any other file, do not commit.

Working directory: /home/trey-agent/Code/post-test-audit

## Context
Test-value audit of the `post` Rust CLI. Rules: thoughts/test-audit/skill/SKILL.md and thoughts/test-audit/skill/CAMPAIGN.md (steps 3 and 4). Read both first.
Ledger under review: thoughts/test-audit/ledger-participant-gc.md (area "participant gc and auto-cleanup", defined in thoughts/test-audit/areas.md). Written by a Claude Sonnet agent.

## Your task (CAMPAIGN step 4 layer pass, plus correctness check)
(a) Check every mark against the actual test and production code. Especially: are the 28 R marks really independent contracts, or do some duplicate a stronger test at another layer (unit test vs CLI integration test for the same gc behavior; tests in reports.rs / schema_truth.rs / participants.rs / cli.rs covering the same contract)? Are the 2 C and 3 F marks right, and is each F repair specified precisely enough to implement?
(b) Name the keeper test for each gc contract.
(c) Check the ledger's suspected bugs against source, real or not, with file:line: (1) append_log on a failed run logs `deleted: []` and loses partially-collected ids; (2) the stamp write follows symlinks while lock/log use O_NOFOLLOW.
(d) List test-only production seams and whether any have non-test callers.

Be calibrated: agreeing with the ledger is a fine result; do not invent deletions.

## Output
Write thoughts/test-audit/review-sol-participant-gc.md with: disagreements per test (test name, ledger mark, your mark, evidence file:line), keepers per contract, final edit list for this area, bug verdicts, open questions. Then reply with a short report.
