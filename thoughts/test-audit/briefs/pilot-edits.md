# Pilot edits: participant gc, restore, auto-cleanup

Nothing here has been compiled or run (CPU rule). Mutations are for the later capped runner; each should turn the named test red, and the unmutated tree must be green first. Line numbers are pre-mutation, on branch test-audit.

## 1. Fold "woke since planning" into U3 (src/commands/participant_gc.rs)

- Changed: `state_that_arrives_after_planning_keeps_the_participant` gained an eighth arrival, "the session woke up and touched it" (`participant::touch`), flag `true` so it runs at both tiers (Delete and Archive), with the existing skipped == 1, record present, no tombstone, no archive assertions. Removed `a_participant_that_woke_up_since_planning_is_left_alone`.
- Catches: a candidate that becomes active between plan and apply being collected (the `last_seen` re-check in `still_collectable`), now at tier 2 as well.
- Mutation: src/commands/participant_gc.rs:515, delete the `current.last_seen != before.last_seen` clause (or `||` it to `false`). The touch rows should fail (other arrivals are caught by other clauses, so only touch goes red).

## 2. Repair I13 (tests/participant_gc.rs) and remove U6

- Changed: `participant_restore_recreates_a_deleted_record_under_its_own_id` now sets workspace "alpha", workspace_path "/projects/alpha", ephemeral true (plus lease 12, name Ember) before collection, asserts those fixture values are on disk pre-GC, and adds `workspace_path` to the post-restore field loop. Removed unit test `a_collected_record_is_revived_under_the_same_id` (its unique workspace_path assertion now lives here; archive and error cases are owned by I12/I14).
- Catches: a tombstone or revival dropping or defaulting workspace, workspace_path, or ephemeral.
- Mutation: src/commands/participant_gc.rs:499, replace `participant.workspace_path.clone()` with `None`. Also try :498 `participant.ephemeral` -> `false`.
- Run-time risk: with ephemeral true and 40 idle days it should still be tier 1 (window is one day); the review asked to confirm this on a real run.

## 3. Repair A9 (tests/participant_auto_gc.rs)

- Changed: `the_schema_documents_the_switch` parses the schema JSON, selects `environment` entries that start with `POST_AUTO_GC:`, requires exactly one, and requires it to contain "=0 disables the automatic participant cleanup".
- Catches: the switch documented under the wrong field, wrong name, or with wrong semantics (old whole-text grep passed on any mention anywhere).
- Mutation: src/commands/schema.rs:585, change `=0 disables` to `=1 disables`.

## 4. Repair A10 (tests/participant_auto_gc.rs)

- Changed: `the_bookkeeping_files_are_invisible_to_rooms_and_doctor` binds one active participant with `POST_AUTO_GC=0`, snapshots `rooms --json` and doctor `(id, path, severity)` for every check (asserting no `participants.stale` check as a fixture condition), then ages the stamp and re-binds the same key with cleanup on, asserts stamp/lock/log exist, and requires identical rooms JSON, identical check tuples, and no check path containing `.auto-gc`.
- Catches: rooms or doctor picking up the stamp, lock, or log as a room or finding; a new check appearing or an existing check's path/severity moving because of them.
- Mutation (synthetic, no natural seam): src/commands/doctor.rs:598, right after `last_run_summary`, push an extra `DoctorCheck` with a fixed id and `path` set to the `.auto-gc.log` path when that file exists. A second option: make `list` in src/commands/rooms.rs:14 include store-root dot entries.
- Run-time risk: confirm doctor has no `participants.stale` check in this fixture and that `rooms --json` output is stable across the two calls (no timestamps).

## Not done / notes

- Edit list item 5 (share I11/I17 lock harness) was optional and skipped.
- Unit-module removals may leave now-unused imports in src/commands/participant_gc.rs tests (`Path` is still used by `context_at`; `seed_in` still used). A compile will say.
- S1 and S2 product bugs untouched, as instructed.
