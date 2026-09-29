# Edit lane brief

You implement the approved test-audit edits for your assigned areas of the `post` Rust CLI. Your worktree and areas are named in your task.

## Read first
- thoughts/test-audit/skill/SKILL.md (value bar, junk patterns, validation). Never weaken an assertion. Every repaired assertion must be able to fail for the regression it names.
- For each assigned area, both thoughts/test-audit/ledger-<slug>.md and thoughts/test-audit/review-sol-<slug>.md. The Sol review's FINAL edit list is the spec. Where review and ledger disagree, the review wins.

## Held for the maintainer (do NOT do)
- Deleting `read_history` and the lineage journal reader test (`lineage_journal_ignores_only_a_malformed_final_line`), or changing docs/PARTICIPANTS.md.
- Deleting migration-fence `fence`/`activate`/`write_state_locked` or the test `transitions_are_locked_and_illegal_transitions_refuse`.
- Removing `set_pre_commit_hook` or `set_post_open_hook`.
Leave these in place and list them as held.

## Running tests (hard rules; the box had a CPU meltdown today)
- Bare cargo test is DENIED by a hook. Run every test through testrun, one at a time, e.g.
  `testrun post area-<slug> -- cargo test -j 8 --test <binary> -- --test-threads=8`
  or, for unit tests, `testrun post area-<slug> -- cargo test -j 8 --lib <module_path> -- --test-threads=8`.
- Run only the test binaries and modules your edits touch. Never the full suite. The lead runs the full gate at the end.
- `cargo build`/`cargo check`/`cargo clippy --all-targets -- -D warnings` also run through testrun (`testrun post check-<slug> -- ...`).

## Proof
For every F repair, every C consolidation, and every D whose "stronger remaining proof" claim hasn't been demonstrated, make one deliberate mutation of the production code that the repaired or keeper test must catch, and watch it go red. Then restore the source byte for byte (`git diff` on that file must be empty afterwards) and watch it go green. Record each mutation and result in thoughts/test-audit/proof-<slug>.md. A test that stays green under its mutation is a finding: fix the test, or report that it can't bind.
If a retained test fails on the baseline, don't delete it. Report it as a possible product bug.

## Commit
Commit in your worktree, one commit per area, with explicit pathspecs (`git commit -- <paths>`); check `git diff --cached --stat` first. No -a/-A, no amend, no --no-verify. Subject ≤72 chars. The body gives the verification that ran, with counts (tests run, mutations caught), and the net LOC. Don't commit thoughts/. Don't push, and don't merge.

## Reply
At most 12 lines: per area, what landed (counts deleted/consolidated/repaired), mutation proofs caught/total, net test and production LOC, held items, and anything that wouldn't bind or failed on baseline.

## Maintainer rulings (Trey, 2026-09-29)
- Lineage journal reader (`read_history`, its test, the docs/PARTICIPANTS.md "tolerant reader" line): KEEP for now. Revisit at the next audit if nothing reads the journal.
- Migration fence `fence`/`activate`/`write_state_locked` and `transitions_are_locked_and_illegal_transitions_refuse`: KEEP, as the in-repo reference for the transition rules.

## Batch 2 rulings (lead, 2026-09-29)
- The Sol review's final edit list is the spec, including its reversals (channels: fast unit tests in chat.rs stay R; do not fold into CLI).
- DELETE (lead decision, dead code with no non-test callers): `eligibility::visible_channel` and `archived_channel` (src/eligibility.rs ~465, ~507); the dead room-level cursor writer chain in src/cursor_state.rs (`consume_channel`, `consume_channel_through`, `consume_inner` and helpers only they use), retargeting any F-repaired tests to the live participant writer first. Historical docs under docs/plans/ are left as they are.
- RESOLVED (Trey ruled/delegated 2026-09-29): (a) Provenance: code is right; CONTRACT.md:1306-1314 was edited to say full/compact only; read.rs unchanged. (b) The 50,000 seen-id warning: DROPPED (Trey delegated the call; the lead ruled drop). `SEEN_SET_WARN` was deleted and CONTRACT.md:240-243 edited, in a separate commit.
- Rooms: the reserved-names repair loops over RESERVED_ROOM_NAMES in a mailbox-local test AND keeps independent CLI anchors for `participants`, `.rename.lock` and the rename journal, per the rooms review. Don't edit CONTRACT.md.

## Batch 3 rulings (lead, 2026-09-29)
- Input guards: implement review-sol-input-guards.md items 1, 3, 4, 5 and 6 (including removing the dead `BodySource.file` field). Item 2 (timing robustness): make NO timing change this batch; leave the 50 ms ceilings as they are and list them as a follow-up. Don't split K7.
- Read code and cite lines from your own worktree, which starts from main after batch 2 is merged. Review line numbers were taken on the pre-batch-2 tree, so re-locate by test name.
- Participants: implement the review-sol-participants.md final edit list as written, and also remove the stale `#[allow(dead_code)]` annotation on `list_active` (participant.rs ~994).
- Cross-area: `snapshot_does_not_leave_a_live_heartbeat` is the keeper for deleting `missing_heartbeat_is_not_live` (participants) AND is F in watch and doctor. Watch owns its repair. The repaired version must still fail if a missing heartbeat file reads as live; prove that with a mutation before deleting the participants unit test.
- Watch: implement the review-sol-watch.md final edit list. DELETE `presence::touch_heartbeat` and the legacy else-arm in `touch_admitted_heartbeats_with`. The lead approves this because the caller trace shows it's unreachable from production, and Trey delegated product calls. Keep the legacy reader, its fixtures and who.legacy_rooms. Don't edit CONTRACT.md or docs. Keep the cfg(test) scan_batch wrapper.
- Doctor: implement the review-sol-doctor.md final edit list, with two changes. Item 2's `snapshot_does_not_leave_a_live_heartbeat` repair belongs to the watch edit (one repair, not two). The channels lane is already merged, so the doctor edit fixes the chat `seen-by` read-only assertion and the history-grep case-insensitive/regex assertion itself, as that review specifies.
- CLI surface: implement the review-sol-cli-surface.md final edit list. Item 1 (sandbox the two tests that inherit the live mail root) lands as its own commit, FIRST. Also DELETE the unused `output::InboxOutput` struct (lead ruling: the only Rust consumers are copies of post itself, found by searching ~/Code). Move its test decodes to `InboxOutputV2` or `serde_json::Value` without dropping any field assertion. KEEP the error codes `NotYet`/`CrossedSend` and the store_version literals unchanged; they're follow-ups, not part of this audit.

## Batch 4 rulings (lead, 2026-09-29)
- Install: implement review-sol-install.md items 1 to 4. Item 3's `--help` range fix and acceptance target-path fix are each their own small commit. Item 4: make `./scripts/gate.sh` the gate named in CONTRIBUTING.md, and point the three hook test defaults (contract.test.mjs, doorbell-supervisor.test.mjs, doorbell-supervisor-process.test.mjs) at the shared cargoReleaseBin resolver the launcher tests use. Item 5: KEEP the three launcher env overrides as supported operator knobs; don't sandbox-migrate the launcher tests.
- Install (cont.): leave README.md:39 and the mail-hook-core.mjs repair text unchanged, since `target/release/post` is correct for a default cargo setup. Shrink the real-smoke fixtures to 50 participants via POST_SMOKE_WHO_PARTICIPANTS in the tests only.
- Bridge (Rust): implement the review-sol-bridge-rust.md final edit list. ALSO, as separate commits (lead ruling; Trey delegated product calls): (a) the refused-letter-un-archives bug (S1). Write the regression first in tests/bridge_deliver.rs: a letter to a gc-archived recipient on a blocked route is rejected with blocked_route AND the recipient stays archived. Watch it go red on current code, then move the revive after the refusal checks, and watch it go green. If the rule checks genuinely need the live record, stop and report instead of forcing it. (b) S2: channel_relay_status reports an unreadable or nonregular config with an accurate reason, not "no bridge config". This needs a test that fails first. (c) S3: move the misplaced bridge_health doc comment.
- Skill hooks: implement the review-sol-skill-hooks.md final edit list as written, including the one turn-mark seam contract test (writer hook to supervisor reader). Red-prove it by breaking the writer's mark path or format once. No production edits. Coordinate with install item 4: the three hook test defaults move to the cargoReleaseBin resolver. Whoever owns both areas does it once.
- Bridge (Python): implement the review-sol-bridge-python.md final edit list, items 1 to 4. Item 4, the README fix, is its own commit. The name-grep test is D, per the lead's ruling under Trey's delegation. Run Python tests only through testrun, one test file at a time (e.g. `testrun post area-pybridge -- python3 -m pytest -q bridge/tests/test_sweep.py -k <filter>`; use the repo's documented Python runner if it differs), never the whole bridge suite and never with -n or xdist.
