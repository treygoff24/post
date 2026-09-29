# Proof: participant-gc

Baseline (unmutated, via testrun unit `post-area-gc`): tests/participant_gc.rs 17/17, tests/participant_auto_gc.rs 10/10, lib participant_gc 4/4. Each mutation restored byte for byte (cmp against a backup; `git diff` on schema.rs and doctor.rs empty).

| # | Mutation | Test expected red | Result |
| --- | --- | --- | --- |
| M1 | participant_gc.rs still_collectable: drop `current.last_seen != before.last_seen` clause | state_that_arrives_after_planning (touch row) | STAYS GREEN (4/4). Redundant guard, see below |
| M1b | classify: `is_active` -> `false` | same | STAYS GREEN (the `recent` age check still keeps it) |
| M1c | M1 + M1b + `age <= tier1_window` -> `false` (all three guards off) | same | RED: state_that_arrives_after_planning_keeps_the_participant. Touch row can fail, but only when all three guards are gone |
| M2 | :499 workspace_path -> None | participant_restore_recreates_a_deleted_record_under_its_own_id | RED (1 failed) |
| M3 | :498 ephemeral -> false | same, plus an_explicit_claim_on_a_deleted_record_gets_the_same_participant_back | RED (2 failed) |
| M4 | schema.rs:585 `=0 disables` -> `=1 disables` | the_schema_documents_the_switch | RED |
| M5 | doctor.rs detect_routing_receipts: synthetic check whose path is `.auto-gc.log` | the_bookkeeping_files_are_invisible_to_rooms_and_doctor | RED |

## Finding: the touch row cannot bind to the `last_seen` clause

The "woke since planning" regression is triple-guarded in the apply path: the `last_seen` comparison in still_collectable, `is_active` in classify, and the age window in classify (a touch makes last_seen fresh, so age <= window). Removing the clause (M1) or is_active (M1b) alone leaves the row green. The row is still a valid behavior test (M1c reds it), and it now runs at both tiers, but the `last_seen` clause is defense in depth that no test can isolate. Candidate for the maintainer: delete the clause, or keep as documented redundancy. Not changed here.

Run-time risks from pilot-edits.md, confirmed: ephemeral with 40 idle days is still tier 1 (I13 green); doctor emitted no participants.stale in the A10 fixture and rooms --json was stable across the two calls (A10 green).
