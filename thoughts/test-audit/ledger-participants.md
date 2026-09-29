# Test-value ledger: Participants (identity, bind, lifecycle, profile), area 5

Read-only pass. No tests, cargo, or builds were run; every negative was judged by tracing the production path. Marks: R keep, F fix (vacuous or wrong-reason), C collapse into a keeper, D delete (proof remains elsewhere).

## Scope check

Assignment is `tests/participants.rs` minus other areas' ranges (99-404 area 3; 866-1168 and 1426-1538 area 1; 1202-1222 and 1674-1719 area 4; 1983-2065 area 7; 3450-3485 area 8), plus unit tests in `src/presence.rs` (13), `src/participant.rs` (9), `src/profile.rs` (4), plus cli.rs tests. The cli.rs range in areas.md (13148-13179) has drifted after batch 1: the assigned tests now sit at cli.rs:3367, 3394 and 13011. cli.rs:3413 (unbound rooms listing) belongs to area 4 and was not marked here. Nothing else in the assignment was wrong.

## Tally (declarations: 48 integration + 3 cli + 26 unit = 77)

**R 58, F 8, C 8, D 3** (77 declarations).

## A. `src/presence.rs` unit tests

| Test | Mark | Contract / regression / note |
|---|---|---|
| `missing_heartbeat_is_not_live` :214 | D | Absent file reads dead. Proof remains in cli.rs `who_reports_live_watch_without_pids` (pre-watch not live) and `snapshot_does_not_leave_a_live_heartbeat`. No unique branch. |
| `fifo_heartbeat_does_not_hang_who` :227 | R | O_NONBLOCK read; regression = `who` hangs on a planted FIFO. |
| `symlink_heartbeat_reads_as_dead` :245 | R | O_NOFOLLOW read; symlink to a fresh stamp must not read live. |
| `oversized_heartbeat_reads_as_dead` :266 | F | `"9".repeat(129)` overflows u64, so the stamp parses to None and the test passes even with the 128-byte guard removed. Repair: a valid fresh stamp padded past 128 bytes, `format!("{now} 1000 {}", "x".repeat(128))`; assert dead and last_seen None. |
| `forged_giant_interval_does_not_pin_liveness` :281 | R | Interval clamp to 60000; a forged huge interval must not keep a stale stamp live. Absorbs stale row. |
| `fresh_heartbeat_is_live` :307 | R | Live positive plus mode 0600 on written file. |
| `stale_heartbeat_is_not_live` :324 | C | Same stamp-1 dead and last_seen "1" assertions as the giant-interval test; `ten_second_interval` covers the pure negative. Absorbed by `forged_giant_interval...`. |
| `ten_second_interval_stays_live_between_polls` :340 | R | Interval honoured (age <= 2*interval+2000ms). Edges are far from the real 22s edge but catch "interval ignored". |
| `future_stamp_is_never_live` :352 | R | Clock-skew forged stamp. |
| `heartbeat_write_does_not_follow_symlink` :361 | R | Writer O_NOFOLLOW. |
| `existing_heartbeat_perms_normalized_to_0600` :385 | R | Writer chmod of a pre-existing loose file. |
| `heartbeat_write_does_not_clobber_through_hard_link` :402 | R | nlink==1 guard. |
| `heartbeat_write_does_not_hang_or_write_fifo` :438 | R | Writer O_NONBLOCK on FIFO. |

Nit: two pairs of tests reuse `test_root` labels ("presence-fifo", "presence-symlink"); nanosecond nonce makes a clash unlikely.

## B. `src/participant.rs` unit tests (1746-1950)

| Test | Mark | Note |
|---|---|---|
| `created_offsets_convert_to_a_utc_id_watermark` | R | Offset-to-UTC conversion feeding the id watermark. |
| `address_inbox_is_a_pure_path_accessor` | D | Pure path join. Proof: participants.rs typed-targets test (~899-955) asserts files at `lineages/ember/inbox/<id>.mail` and `participants/<id>/inbox/<id>.mail`; workspace inbox is asserted by every send. Non-test caller send.rs:382. |
| `nearest_harness_ancestor_selects_nested_codex_child` | F | Calls test-only `nearest_native_harness_in`, a duplicate of the production walker; only `native_harness()` is shared. A regression in `nearest_native_harness_from` would not fail. Repair: point at `nearest_native_harness_from` with an info closure. |
| `claude_pid_marks_nearest_claude_ancestor_even_under_a_wrapper` | F | Same seam and repair. |
| `ambiguous_ancestor_list_never_guesses` | F | Same seam and repair. |
| `recognized_nearest_harness_survives_unavailable_higher_ancestor` | R | Missing higher ancestor info must not drop a recognized nearer harness. |
| `lifecycle_lease_boundary_missing_record_and_end_are_explicit` | R | +-1s boundary, missing record label, end. |
| `lifecycle_uses_each_records_own_lease` | R | Per-record lease, not the env default. |
| `list_active_requires_a_current_lease_record` | R | list_active has non-test callers (routing.rs:850,858; commands/profile.rs:129,266). |

## C. `src/profile.rs` unit tests

| Test | Mark | Note |
|---|---|---|
| `display_name_rules` :326 | R | Skeleton/NFKC imitation, owner reservation. |
| `pfp_rules` | R | Grapheme, bidi, ASCII, refusal text are unit-only. Uniqueness rows duplicate `profile_list_agrees_with_the_sigil_uniqueness_refusal` (harmless). |
| `drop_invalid_fields_catches_planted_pfp_preserved_by_set` | R (weak) | Gap: nothing checks `profile set --name` scrubs a planted pfp at the CLI boundary; that is a new integration row, not a change here. |
| `stamp_for_uses_only_the_participants_own_entry_and_drops_invalid_values` | R | Bad/imitation rows unique; fresh/solo rows duplicate `participant_profile_belongs...`. |

## D. `tests/participants.rs`

| Test :line | Mark | Contract / regression / note |
|---|---|---|
| `participant_two_keys_mint_distinct_ids_and_rebind_is_idempotent` :85 | R | Deterministic id from key; rebind same id. |
| `participant_bind_key_bootstrap_is_idempotent_and_prints_export` :405 | R | Bootstrap output and idempotence. |
| `participant_bind_new_uses_fresh_uuid_and_default_shell_harness` :439 | R | Ephemeral 1h lease is not asserted here. |
| `participant_explicit_bind_workspace_rebinds_existing_record` :453 | R | |
| `participant_native_harness_labels_ignore_post_harness_override` :471 | R | |
| `participant_concurrent_bind_converges_on_one_record` :513 | C | Ids are deterministic, so racing binds of one key converge without the lock; vacuous for the lock. Into `participant_round2_collision_race_preserves_both_keys` :2255 (24 concurrent binds); carry over a record-count == 2 assertion. |
| `participant_crash_after_record_before_index_reconverges` :550 | R | |
| `participant_dangling_index_re_mints_same_deterministic_record` :570 | C | Into :1828, whose tail asserts rebind gives same id and `inbox` then works. |
| `participant_digest_mismatch_extends_id_to_twelve_hex` :589 | R | |
| `participant_read_only_unbound_commands_create_nothing` :620 | R | Dir-inclusive `tree()` is stronger than `common::tree_snapshot`; do not swap. Table-merge candidate with 1241/2310. |
| `participant_delegate_child_without_a_claim_is_unbound_not_its_parent` :676 | R | |
| `participant_keyless_send_fails_with_bootstrap_fix` :781 | R | Keeper for :1759. |
| `participant_keyed_send_binds_lazily_and_reports_bound_now` :805 | R | |
| `participant_keyed_send_text_receipt_names_the_lazy_bind` :840 | R | Text branch of `annotate_bound_now`. |
| `participant_old_format_envelope_still_parses` :1169 | D (medium) | `Envelope` is the struct parsed by mailbox::parse_mail_text; old fixtures without from_participant/from_lineage/address_kind are read via CLI in :1241 (`read --json`), :2831, and cli.rs `old_mail_renders_unknown_origin_reply_metadata_without_an_evidence_line`. Storage back-compat contract, so a reviewer may keep it as a cheap R. |
| `participant_version_json_advertises_store_and_capabilities` :1185 | C | Capabilities/store_version also asserted at :1720 and schema_surface.rs:137. Into :1720; carry over build_sha non-empty (near-trivial, default "unknown"). |
| `participant_codex_conflict_is_an_error_not_a_guess` :1223 | R | |
| `participant_review_bound_and_unbound_read_only_forms_preserve_complete_tree` :1241 | R | Bound half overlaps :2831's 16-command list but fixtures differ. |
| `participant_round4_unbound_streams_keep_stdout_protocol_and_budget` :1338 | F (medium) | The cap equals the bound baseline size, probably larger than the ~430-byte marker, so the `--max-bytes` fallback (bare marker; `scaffold_too_large`, mod.rs:479-488) is likely never hit. "session not bound" appears in no test. Repair: cap below marker size (e.g. 200) expects bare `{"ok":true,"participant":null,"bound":false}`; tiny cap expects `scaffold_too_large`. Sizes need a run to confirm. |
| `participant_round4_bound_sender_assertions_refuse_disagreement` :1539 | R | Only test asserting "conflicts with bound participant" and "workspace pin" (send.rs:138,156; channel.rs:585). |
| `participant_round4_bound_matching_cwd_still_uses_binding_provenance` :1599 | C | Into :2207 (parameterize cwd in {beta, alpha}; assert from == "beta" and provenance participant-binding). |
| `participant_round4_native_keys_reject_empty_or_whitespace` :1643 | R | |
| `participant_review_version_is_pure_under_broken_or_ambiguous_identity` :1720 | R | Both rows pass through early `version` dispatch (mod.rs:36); one would do. Absorbs :1185. |
| `participant_review_no_key_diagnostic_names_real_bootstrap_sequence` :1759 | C | Same invocation as :781; carry over `exact_fix.is_none()` and message contains checks. |
| `participant_missing_explicit_claim_is_a_typed_error_with_the_fix` :1775 | R | |
| `participant_missing_dangling_session_index_is_loud_and_diagnosable` :1828 | R | Absorbs :570. |
| `a_missing_claim_is_missing_not_unbound_in_text_mode` :1877 | R | |
| `participant_review_existing_colon_and_reserved_rooms_load_but_doctor_reports_them` :1940 | F | `contains("reserved")` is satisfied by the colon message "reserved for typed addresses", so the `participants` reserved-store-name finding is unbound. Repair: assert check ids `config.room_name.participant:foo` and `config.room_name.participants` with messages "typed addresses" and "reserved by mailbox storage". Confirm wording with a run. |
| `participant_round2_explicit_bootstrap_ignores_inherited_identity_and_ambiguity` :2066 | R | |
| `participant_round2_resolution_errors_are_advisory_on_read_only_surfaces` :2122 | R | |
| `participant_round2_plain_rebind_preserves_existing_workspace` :2188 | C | Last step (rebind from beta cwd keeps workspace alpha and lineage) is the same branch as :2502. Into :2502. |
| `participant_round2_binding_provenance_wins_when_cwd_differs` :2207 | R | Keeper for :1599. |
| `participant_round2_collision_race_preserves_both_keys` :2255 | R | Digest-prefix precondition; fails without the lock. |
| `participant_round2_fully_unbound_read_only_forms_work_without_mutation` :2310 | R | Unique rows: chat --history/--since/--seen-by/--message, search --channel. `expected_error` label "marker" is misnamed (nit). |
| `participant_round2_workspace_less_actor_gets_rebind_fix_not_room_shadowing` :2454 | F | Contract drift: commit 13510aa changed the assertion from rebind-fix/no-shadowing to not_found, so it cannot detect shadowing; not_found for an absent channel is already asserted for bound actors in :1241/:2831. Repair: as the workspace-less actor run from the alpha cwd (`chat join`, then send) and assert message.from == actor id, not "alpha". |
| `participant_round2_unbound_annotation_keeps_ok_first` :2486 | R | |
| `participant_lifecycle_touch_end_and_bind_reactivation` :2502 | R | Absorbs :2188. |
| `participant_touch_preserves_recorded_lease_unless_env_explicitly_overrides_it` :2584 | R | Env-override row duplicates :2502's touch step. |
| `participant_lifecycle_missing_lease_is_stale_until_rebind` :2643 | R | |
| `participant_lifecycle_malformed_timestamps_are_skipped_and_doctor_names_them` :2677 | R | Letter and low-byte '/' rows. |
| `list_and_who_name_damaged_participant_records_on_stdout` :2721 | R | |
| `participant_lifecycle_central_writer_refresh_and_read_only_stability` :2831 | R | Final last_seen == 2020 assertion subsumed by per-command tree equality. |
| `participant_lifecycle_who_reports_active_stale_ended_and_crash_gap` :2963 | R | |
| `participant_lifecycle_validates_renewal_lease_but_end_preserves_it` :3036 | R | |
| `participant_profile_belongs_to_the_participant_not_the_workspace` :3099 | R | |
| `profile_list_agrees_with_the_sigil_uniqueness_refusal` :3271 | R | |
| `participant_show_by_key_is_a_non_minting_lookup` :3377 | R | Gap: `archived` status arm of show_key untested. |
| `porch_pre_bind_checks_keep_their_shape` :3486 | R | |

## E. `tests/cli.rs`

| Test :line | Mark | Note |
|---|---|---|
| `unregistered_cwd_read_only_chat_reports_unbound_without_creating_identity` :3367 | C | Into the extended :3394, plus :2310's `chat --peek --json` marker row and :1241's text row. |
| `hostile_unregistered_cwd_read_only_chat_creates_nothing_and_cannot_inject` :3394 | F | The `INJECTED` assertion is vacuous (no shell runs since it lost `run_fix` in 7264125); `!contains("exact_fix")` is vacuous in prose. Repair: add a plain-cwd row and assert the marker line is present (one line, names `post participant bind`, does not echo the dirname); drop the INJECTED and exact_fix asserts. |
| `profile_show_names_its_argument_a_participant` :13011 | R (weak) | Pins a help/schema positional name; red-proofed in 61a2228. Consider moving to area 13. |

## Keeper per contract

- Deterministic id, rebind idempotent, 12-hex collision: :85, :589, :2255 (with :513 record-count).
- Crash/dangling index recovery: :550, :1828 (with :570 tail).
- Resolution order, explicit claim, missing vs unbound: :676, :1223, :1775, :1877, :2066, :2122.
- Unbound reader marker / budget: :620, :1241, :2310, :2486, :1338 (repaired), cli :3394 (extended).
- Bound provenance and sender assertions: :1539, :2207 (with :1599 rows).
- Lifecycle (touch/end/lease/stale): :2502 (with :2188), :2584, :2643, :2963, :3036, unit lifecycle tests.
- Damaged records: :2677, :2721.
- Profile: unit `display_name_rules`, `pfp_rules`, `stamp_for...`; integration :3099, :3271.
- Presence heartbeat: the presence unit tests above; `forged_giant_interval` absorbs stale.
- Nearest native harness: three F tests repointed at `nearest_native_harness_from`.

## Test-only seams

Unlocked: `nearest_native_harness_in` (`#[cfg(test)]`, participant.rs:1321-1332). Only the three F tests call it; no non-test caller. Delete after repointing the tests at production `nearest_native_harness_from`.

Retained: `bind_test_actor`, `test_actor_id`, `TEST_ACTORS` thread_local (participant.rs:390, 1684-1734) and the `#[cfg(test)]` arm in `resolve`; used by about 30 unit tests in send.rs, channel.rs, chat.rs, watch.rs, cursor_state.rs, catchup.rs, channel_state.rs, output.rs. `Address::inbox` has non-test caller send.rs:382. `list_active` has non-test callers (see B).

## Suspected product bugs / nits (none confirmed by running)

- participant.rs:994 stale `#[allow(dead_code)] // routing seam consumed by P.2`; `list_active` is now called from routing.rs and commands/profile.rs.
- participant.rs:1490 `parse_rfc3339` and :1557 `parse_sent_timestamp` are near-duplicate parsers; `parse_rfc3339` has no unit test.
- profile.rs `drop_invalid_fields` re-implements `validate_pfp`'s shape checks rather than sharing them.
- Presence writer and reader are correct as read; no bugs found.

## Open questions (need a run)

1. Does :1338's bound baseline exceed the marker size (is the `--max-bytes` fallback ever exercised)?
2. Is :513 truly lock-vacuous (reasoned from deterministic ids, not run)?
3. Confirm :2454 cannot detect shadowing (history shows contract drift in 13510aa).
4. Confirm the doctor messages needed for the :1940 repair.
5. :2963 seeds test_participant("alpha") in cwd; confirm that does not mask the crash-gap row.
6. :2831 versus :1241 fixture difference (routing receipt vs unrouted mail) if a table merge is attempted.
7. areas.md cli.rs line range has drifted (13148-13179 is now 13011).

Provenance note: areas.md's "round2/round4/review prefixed = audit artifacts" smell is only partly true; many began as red-first "test: reproduce" commits (19ce7e1, 8898d54, cbccc84, d1984a1).
