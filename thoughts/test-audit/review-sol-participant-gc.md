# Independent layer review: participant GC, restore, automatic cleanup

Read-only source review of all 33 declarations in `ledger-participant-gc.md`. No tests or builds ran. Marks below are proposed for the area cutover, not verified pass results. The ledger's marks stand: **28 R, 3 F, 2 C, 0 D**. The C rows require carrying their unique assertions into named keepers before removal. Commit messages `a273af2`, `5f8a681`, and `a0a00cc` report earlier mutation checks; I did not reproduce them.

## Disagreements and corrections per test

There is **no mark disagreement** after comparing the declarations with source. These corrections affect the edit instructions or the strength of a claim:

| Test (ledger -> review) | Evidence and correction |
| --- | --- |
| `a_participant_that_woke_up_since_planning_is_left_alone` (C -> C) | `src/commands/participant_gc.rs:670-689` exercises a real touch. Carry it as a tier-1 **and tier-2** row in `state_that_arrives_after_planning_keeps_the_participant` (`:771-889`), and check `skipped == 1`, record present, no tombstone/archive. A mere deletion of U2 loses activity as an arrival kind. `still_collectable` also compares `last_seen` at `:515-520`; the row catches that path as well as classification. |
| `a_cached_answer_needs_a_stamp_that_still_matches` (R -> R) | The unit predicate assertion is low-level, but the unreadable-stamp fail-closed case at `src/commands/participant_gc.rs:913-919` is absent from the per-candidate test at `:926-971`. `cache_still_valid` is used in production at `:220`; retain its unique `None` and same-count/different-mtime rows. |
| `a_collected_record_is_revived_under_the_same_id` (C -> C) | Its `workspace_path` non-null assertion at `src/commands/participant_gc.rs:998-1021` must move to the restore CLI keeper. The archive/error portions at `:1027-1055` have stronger CLI proof at `tests/participant_gc.rs:902-947,1001-1043`. Remove U6 only after the I13 field repair. |
| `gc_never_collects_a_participant_that_has_mail` (R -> R) | The comment at `tests/participant_gc.rs:245-247` promises own-inbox, workspace-pending and held mail, but this declaration constructs one unread direct-letter case at `:257-298`. U3 owns the other arrival cases at `src/commands/participant_gc.rs:785-798`; do not credit I2 with them. The absence check is meaningful because `classify` reaches `mail_reason` for a stateful stale record (`src/commands/participant_gc.rs:291-297,348-370`). |
| `participant_restore_recreates_a_deleted_record_under_its_own_id` (F -> F) | `tests/participant_gc.rs:955-984` compares `workspace` and `ephemeral` without arranging meaningful non-default values and omits `workspace_path`. Set non-null workspace and absolute workspace path plus `ephemeral: true` **before** collection; assert the fixture values are present before GC, add `workspace_path` to the field loop, then retain the existing post-restore field and fresh `last_seen` checks. Tombstones and revival carry all three (`src/commands/participant_gc.rs:490-503`, `src/participant.rs:607-628`). |
| `doctor_reports_the_last_automatic_cleanup` (R -> R) | `tests/participant_auto_gc.rs:368-386` checks a later valid JSON line and a failure line. It does **not** check the malformed trailing-line fallback claimed in the ledger: `last_run_summary` uses reverse `find_map` at `src/commands/participant_auto_gc.rs:130-136`. The keeper is still distinct from `tests/reports.rs:580-645`, which checks prune counts. No extra row is required for this cutover. |
| `the_schema_documents_the_switch` (F -> F) | Whole-output `contains` at `tests/participant_auto_gc.rs:392-398` can pass on the wrong schema field. Parse the schema as JSON; require one `environment` string whose prefix is `POST_AUTO_GC:` and which states `=0` disables cleanup. The schema field is a string array (`src/output.rs:1948-1962`); the real declaration is at `src/commands/schema.rs:576-585`. Other schema tests do not pin this entry (`tests/schema_truth.rs:412-551`, `tests/schema_surface.rs:201-203`). This proves documentation presence, not that `enabled()` reads the same name; A4 proves that behavior. |
| `the_bookkeeping_files_are_invisible_to_rooms_and_doctor` (F -> F) | The current new-ID checks at `tests/participant_auto_gc.rs:403-430` can pass when a check points at `.auto-gc.*`; they never call `rooms`. Repair with a stable fixture: bind an active participant with `POST_AUTO_GC=0`, capture `rooms --json` and doctor check identities `(id, path, severity)`, age a stamp and bind **the same key** with cleanup enabled so stamp, lock and log are present, then compare room output and check identities and reject any doctor check path containing `.auto-gc`. Keep all participants active so the intentional last-run text in `participants.stale` cannot change the comparison (`src/commands/doctor.rs:585-609`). A missing `participants.stale` check should be asserted as a fixture condition. `rooms` reads `rooms.json` via `load_rooms` (`src/commands/rooms.rs:14-26`); doctor checks expose `path` (`src/output.rs:1977-1985`). |

All remaining R declarations were checked against the test and owner source and have separate failure modes. In particular, `gc_keeps_ids_stable_and_restores_archived_state` owns **bind** restoration and collision handling (`tests/participant_gc.rs:305-395`); I12 owns the explicit **restore command** plus idempotence (`:902-947`); I6/I7 own revival of a claimed actor before writes (`:491-675`). U1 tests interrupted mutation order (`src/commands/participant_gc.rs:609-666`), U3 tests post-plan arrivals (`:764-889`), and U5 tests a letter arriving between two candidates in one batch (`:926-971`). Their overlap with the CLI tests is in setup, not in the asserted failure point. I11 and I17 use a similar lock harness but protect distinct lock acquisitions in `revive_explicit_claim` and `restore` (`src/participant.rs:659-740`). A1-A7 protect separate scheduling, disable, lock, failure, and log-rotation branches (`src/commands/participant_auto_gc.rs:38-125`).

## Keeper per contract

| Contract | Primary keeper and distinct secondary boundary |
| --- | --- |
| Plan/apply parity, tier choice, tombstone/archive, second pass empty | `gc_dry_run_matches_apply_and_a_second_apply_finds_nothing` (I1, `tests/participant_gc.rs:184-243`). |
| Planned candidate becomes active or acquires state/mail/subscription/heartbeat/receipt | `state_that_arrives_after_planning_keeps_the_participant` (U3, `src/commands/participant_gc.rs:764-889`), absorbing U2's touch row. |
| Per-candidate workspace inbox change and cache safety | `a_candidate_is_checked_again_just_before_its_own_delete` (U5, `:926-971`); U4 (`:896-920`) keeps the unreadable-stamp and mtime-only predicate cases. |
| Safe state after an interrupted collection | `a_kill_at_any_step_leaves_the_participant_whole_or_put_away` (U1, `:609-666`). |
| Unread routed mail survives | `gc_never_collects_a_participant_that_has_mail` (I2, `tests/participant_gc.rs:249-299`); U3 owns mail arriving after planning. |
| Identity collision, same-id bind and archived bind restoration | `gc_keeps_ids_stable_and_restores_archived_state` (I3, `:305-395`). |
| Plan-time active lease, heartbeat, subscription and lineage | `gc_keeps_anything_still_alive` (I4, `:402-446`); U3 checks later arrivals. |
| Ephemeral day window | `ephemeral_records_are_collected_after_a_day` (I5, `:451-478`). |
| Deleted claim reader refusal and writer revival | `an_explicit_claim_on_a_deleted_record_gets_the_same_participant_back` (I6, `:491-598`). |
| Archived claim reader refusal and writer revival with state | `an_explicit_claim_on_an_archived_record_restores_it_whole` (I7, `:602-675`). |
| Never-held/unreadable claim mapping | `an_explicit_claim_nothing_can_restore_is_still_participant_missing` (I8, `:681-723`); I14 separately owns the explicit restore command's error and ID validation. |
| Migration fence before claim revival | `a_fenced_write_with_a_collected_claim_restores_nothing` (I9, `:730-779`); I15 separately owns `restore` and `gc --apply` command classification. |
| Send to an already collected recipient refuses without writes | `a_letter_sent_to_an_already_collected_participant_is_refused_and_writes_nothing` (I10, `:781-824`); `src/commands/send.rs:1441-1582` owns collection **during** send. |
| Explicit claim revival waits for participant lock | `reviving_a_claimed_record_waits_for_the_participants_lock` (I11, `:827-899`). |
| Explicit archive restore, state bytes, fresh life, idempotence | `participant_restore_brings_an_archived_record_back_whole_and_is_idempotent` (I12, `:902-947`). |
| Explicit tombstone restore, all non-default metadata, stable ID | `participant_restore_recreates_a_deleted_record_under_its_own_id` (I13, `:953-995`), repaired and absorbing U6's `workspace_path`. |
| Explicit restore missing/malformed/unreadable ID | `participant_restore_of_an_id_nothing_holds_is_participant_missing` (I14, `:1001-1043`). |
| Registry writers refused by migration fence | `participant_restore_and_gc_apply_are_refused_under_a_migration_fence` (I15, `:1050-1066`). |
| Restore ignores an unrelated stale acting claim | `participant_restore_runs_for_a_session_with_a_stale_claim` (I16, `:1072-1089`). |
| Explicit restore waits for participant lock | `participant_restore_waits_for_the_participants_lock` (I17, `:1096-1140`). |
| Auto-cleanup first, due, young, disabled, contended, failed, rotated | A1-A7 respectively (`tests/participant_auto_gc.rs:134-339`), one branch family each. |
| Doctor's last automatic run | `doctor_reports_the_last_automatic_cleanup` (A8, `:360-387`); `doctors_prune_numbers_are_the_ones_participant_gc_reports` (`tests/reports.rs:580-645`) owns prune count agreement. |
| Schema documents the disable switch | `the_schema_documents_the_switch` (A9, `tests/participant_auto_gc.rs:392-398`), repaired; `schema_truth.rs:412-551` owns GC/restore shape and usage. |
| Bookkeeping files do not appear as rooms or doctor findings | `the_bookkeeping_files_are_invisible_to_rooms_and_doctor` (A10, `tests/participant_auto_gc.rs:403-430`), repaired. |

## Final edit list for this area

1. Add touch as the eighth U3 arrival with both tiers and its existing no-collection/no-tombstone assertions; then remove U2.
2. Repair I13's fixture and assertions as above; then remove U6. Do not remove U6 first.
3. Repair A9 to inspect the parsed `environment` entry. Keep it in this file unless the schema area owner explicitly absorbs it.
4. Repair A10 with a same-participant baseline, a real `rooms --json` comparison, and doctor `(id, path, severity)` comparison. Do not compare whole doctor messages when the test intentionally changes the cleanup log.
5. Optional support-only cleanup: share I11/I17's lock harness locally. Do not make `tests/common` a cross-area edit solely for this lane; repeated date/tree helpers do not justify a behavior change.

Retired declarations: U2 and U6 only. Retired files: **none**. The area keeps both dedicated integration files and the unit module. No test-only production seam is unlocked by these edits.

## Suspected product bugs

- **S1 real by source, conditional on a later error; execution unverified.** `apply_plan` appends IDs only after `remove_index` (`src/commands/participant_gc.rs:451-487`). An error in a later candidate/batch makes it return `Err`, and `execute` propagates it without a partial report (`:104-129`). `append_log` then writes empty `deleted` and `archived` arrays for every error (`src/commands/participant_auto_gc.rs:90-109`). Earlier completed collections are omitted. An error after a directory move but before `remove_index` also omits that ID even though the record moved. A6 fails on its first tier-2 archive operation (`tests/participant_auto_gc.rs:272-303`), so its empty-array assertion does not probe this case. The stamp is written before the run (`src/commands/participant_auto_gc.rs:80-87`), delaying automatic retry. A product fix needs an outcome that preserves partial progress, then an owner-boundary regression with a successful first collection and a failing later one.
- **S2 real by source; exploitability depends on who can alter the store root.** `fs::read_to_string` and `fs::write` on `.auto-gc.stamp` follow a symlink (`src/commands/participant_auto_gc.rs:66-82`), whereas lock and log opens set `O_NOFOLLOW` (`:54-61,117-123`). Thus a planted stamp symlink can redirect the write on bind. No test covers this. Report as a product hardening follow-up; whether it is security-relevant depends on store-root write access. The current review did not execute a canary.

## Test-only production seams and callers

| Seam | Non-test callers? | Decision |
| --- | --- | --- |
| `Step` and `apply_plan(..., hook)` (`src/commands/participant_gc.rs:404-445,458-474`) | `execute` passes a no-op closure at `:108`; no production caller supplies a meaningful hook. U1/U5 and `src/commands/send.rs:1535-1541` use the steps. | Test-only instrumentation, still needed for deterministic timing/crash cases; not unlocked. |
| `#[cfg(test)] test_seed::{seed, seed_in, digest_of, days_ago}` (`src/commands/participant_gc.rs:527-594`) | None: the module is absent from non-test builds. U1/U3/U5 and `src/commands/send.rs:1442,1487` use it. | Keep while its owners remain. |
| `cache_still_valid` (`src/commands/participant_gc.rs:181-183`) | Yes, `Host::workspace_has_unrouted` at `:209-230`. | Production logic, not a test-only seam. |
| `plan`, `apply_plan`, `Plan::counts` / `Planned::id`, `Applied.skipped` (`src/commands/participant_gc.rs:58-84,104-129,234-244,440-487`) | Yes: GC `execute` uses plan/apply/skipped, and doctor uses plan/counts (`src/commands/doctor.rs:589-592`). `Planned::id` is also used by `Plan::ids` in production (`src/commands/participant_gc.rs:77-82`). | Keep. |
| Auto-GC stamp, lock, log, `POST_AUTO_GC` (`src/commands/participant_auto_gc.rs:34-40,53-125`) | Yes: bind invokes `maybe_run` (`src/commands/participant.rs:205,231`). | Real behavior/configuration, no test-only injection seam. |

## Open questions

- The repaired I13 fixture needs an actual run later to confirm that the chosen non-default `workspace`, absolute `workspace_path`, and `ephemeral` combination remains tier 1 and round-trips through the CLI. The source path supports it (`src/commands/participant_gc.rs:258-289,490-503`), but this run was forbidden to execute tests.
- The proposed A10 same-key fixture needs an actual run later to confirm doctor has no stale-participant check before or after the auto run. Source only emits that check when `total > 0` (`src/commands/doctor.rs:585-609`).
- S1 and S2 are source-confirmed paths without runtime reproduction in this review. The product owner must decide and validate fixes outside this read-only lane.
