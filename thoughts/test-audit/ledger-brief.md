# Ledger lane brief (read-only)

You are a read-only ledger lane in a test-value audit of the `post` project. Work in /home/trey-agent/Code/post-test-audit (branch test-audit).

HARD RULES (Trey, 2026-09-29, after a CPU meltdown on the shared devbox):
- Run NO tests, NO cargo/rustc/build, NO pytest, NO node. Reading files, rg, and git log/show only. If a question needs a run to settle, put it under open questions.
- Do NOT edit source or tests. Do not commit. Your only write is your ledger file.

Read first, in full:
- thoughts/test-audit/skill/SKILL.md (value bar, junk patterns, retention bar, candidate evidence fields)
- thoughts/test-audit/skill/CAMPAIGN.md step 3 (the R/F/C/D ledger marks)
- thoughts/test-audit/areas.md — your area is named in your task. Use its file/fn assignment exactly; if it is wrong, say so rather than silently changing scope.
- thoughts/test-audit/ledger-participant-gc.md — a finished example ledger that an independent reviewer agreed with. Match its shape and rigor.
- AGENTS.md, CLAUDE.md, CONTRIBUTING.md.

Then for your area: read EVERY assigned test declaration in full (including table rows), the production owners (src modules, entry points, callers, callees), the tests/common helpers used, and git history for the tests and owner (`git log --follow` — many tests are bug regressions; the commit says why). Check overlap: does a test elsewhere (another area's file, a unit test vs a CLI test) already cover the same contract? List test-only production seams in the owner (#[cfg(test)] items, pub-for-tests fns, env hooks) and whether any NON-test caller uses them (grep callers precisely).

Write thoughts/test-audit/ledger-<area-slug>.md. One row per test declaration (split table rows only when they need different marks). Each row: test name and file:line; mark R/F/C/D; the contract it protects; the concrete regression that makes it fail; for C the absorbing keeper test; for D the stronger remaining proof or why no contract exists; for F exactly what is vacuous and the precise repair. Judge by assertions, not names; check that negatives fail for the intended reason. Then sections: keeper per contract, test-only seams unlocked (with caller evidence), suspected product bugs (file:line), open questions.

Be calibrated: "almost all R" is a fine result. Optimize for confidence, not deletion count. A D without full evidence is not a D; mark it R with a note.

Final reply: at most 10 lines — counts per mark, D/C/F items by name with a one-phrase reason each, seams unlocked, suspected bugs.
