# Ledger: area 8, watch, doorbell, previews

Read-only lane. No tests, cargo, or builds were run; everything is from reading tests, owners, callers, and `git log`. Marks: R retain, F fix assertion, C consolidate, D delete.

Owners read: `src/commands/watch.rs` (production 1-2412, `mod tests` 2413-4533, `mod follow_tests` 4537-4720), `src/output.rs` (`WatchEvent`, `text_line`, unit tests 2295-2490), `src/commands/who.rs` (`read_doorbell`), `src/presence.rs`, `src/participant.rs` (`resolve`, `bind_test_actor`), `src/commands/send.rs` (`WATCH_NDJSON_WARNING`, `is_watch_event_line`). Git history read: profile walk order (`ccd465f`), filtered-wake and deadline fixes (`fd16003`, `81c0f40`, both red-proofed per their messages), the profile diagnostic (`2443a9a`), doorbell tests (`a93cbb3`). I did not re-run any red-proof.

## Scope check against areas.md

Assignment is right in substance; line numbers in areas.md are stale, so I mapped by test name and current line. Notes:
- `tests/cli.rs` `long_watch_scans_during_each_fence_episode_and_warns_once_per_episode` (7828), `long_watch_exits_when_generation_is_stale_or_state_disappears` (7933), `long_watch_retries_transiently_unparseable_same_generation_state` (8016) are listed here but are marked in the migration-fence ledger; not double-marked, not counted below.
- A long watch requires a bound participant, so a watch target with `participant: None` exists only in unbound `--snapshot` scans. Three unit loop tests and one heartbeat assertion build that shape (see F rows and seams).
- `tests/cli.rs` 8838 (`mentions_stamp_and_watch_reason_marks_at`), 4479 (`room_validation_rejects_controls_before_watch_diagnostics`), `tests/participants.rs` `porch_pre_bind_checks_keep_their_shape` were read for overlap only and belong to other areas.

## Tally

89 declarations: R 83, F 2, C 4, D 0. Almost everything is R: most tests came with a bug fix, and the doorbell and preview files pin user-visible bytes. No D: every candidate for deletion has either a distinct risk or an overlap I cannot fully prove without running.

## A. `tests/doorbell.rs` (7), all R

| Test, line | Mark | Contract, regression |
|---|---|---|
| `watch_snapshot_is_cursorless_then_channel_catchup_consumes_for_participant` :51 | R | Snapshot emits without writing cursors; a later catchup consumes. Fails if snapshot writes seen-sets or cursors. Overlaps `watch_snapshot_emits_direct_and_channel_events_without_consuming_anything` (cli 6877) on the no-consume half; the catchup half is unique. |
| `direct_mail_snapshot_never_consumes_or_moves_canonical_file` :98 | R | Direct mail file stays in place and unmoved after a snapshot. Partial overlap with cli 6877 (same "consumes nothing"); this one pins the canonical path, kept. |
| `restarted_live_watch_rings_again_without_advancing_participant_seen_state` :132 | R | Watch never writes seen state, so a restart re-rings. Fails if watch persists a seen-set. Sole owner of restart semantics. |
| `live_participant_watch_routes_bridge_arrival_and_emits_without_consuming` :177 | R | Live process, bound participant, bridge arrival routed and emitted, not consumed. Only end-to-end proof of the routing hook in a live watch. |
| `live_watch_isolates_same_address_corrupt_receipt_and_rings_healthy_sibling` :255 | R | A corrupt receipt for one address does not silence a healthy sibling. Fails if scan aborts on the first bad record. |
| `armed_watch_rings_again_after_catchup_consumes_the_previous_message` :313 | R | Consumption by catchup does not disarm the doorbell for the next arrival. |
| `watch_started_after_catchup_uses_unified_floor_without_replaying_backlog` :364 | R | A watch started after catchup does not replay consumed backlog (`ScanMode::Wake` skips consumed bodies). |

## B. `tests/watch_preview.rs` (9), all R

| Test, line | Mark | Contract, regression |
|---|---|---|
| `watch_text_line_includes_sanitized_preview_cap_at_80_chars` :10 | R | 80-char cap plus ellipsis. Also covered at unit level in `output.rs`; this row is the CLI byte contract. |
| `watch_text_line_sanitizes_control_characters_and_newlines` :57 | R | Controls dropped, whitespace flattened. |
| `watch_text_line_neutralizes_square_brackets_to_prevent_fencepost_forging` :95 | R | Brackets mapped to full-width so a body cannot forge `[first..last]`. Security relevant. Overlaps unit `full_width_bracket_lookalikes_in_a_body_cannot_forge_the_fencepost` and cli 6445 on the forging theme, from different layers and different bytes. |
| `watch_channel_message_includes_preview` :134 | R | Channel event carries the preview. |
| `watch_unreadable_message_has_no_preview` :165 | R | Unreadable mail rings with no body quoted. |
| `watch_ndjson_includes_preview_field` :186 | R | NDJSON `preview` field. |
| `watch_ndjson_omits_preview_field_when_none` :214 | R | Field absent, not null. |
| `watch_text_marks_pending_mail` :242 | R | Pending marker in text. |
| `watch_typed_snapshot_contract_matches_checked_in_ndjson_fixture` :269 | R | Typed NDJSON contract against `tests/fixtures/watch-snapshot-typed.ndjson`; a checked-in golden file for a wire contract, which the retention bar keeps. |

## C. `src/commands/watch.rs` unit tests (39)

| Test, line | Mark | Contract, regression |
|---|---|---|
| `reconciliation_reports_corruption_in_a_consumed_channel_message` :2520 | R | `ScanMode::Complete` reads consumed files and reports corruption; Wake would miss it. |
| `unusable_cursor_state_marks_the_events_and_the_digest_it_re_reports` :2579 | R | `cursor_unusable` marker on events and digest. |
| `participant_watch_heartbeat_refresh_renews_activity_lease` :2656 | R | Heartbeat refresh also renews the participant lease. |
| `touch_admitted_heartbeats_warns_once_per_failure_episode` :2687 | R | Warn once per failure episode. Uses the `_with` injection point (see seams). |
| `production_touch_wrapper_preserves_warning_episode_state` :2733 | R | The production wrapper keeps episode state across calls. Somewhat near :2687, but it is the only test through the real wrapper. |
| `watch_presence_is_live_before_backend_registration` :2783 | R | Presence is written before the notify backend registers, so `who` sees the watch early. |
| `participant_fast_scan_retries_routing_when_mail_becomes_parseable` :2820 | R | Routing retry after a transient parse failure. |
| `participant_fast_scan_defers_arrival_after_final_route` :2880 | R | Arrival after the last route is deferred, not dropped. |
| `participant_fast_scans_never_emit_pending_during_atomic_arrival_burst` :2965 | R | No false pending during an atomic arrival burst. Uses `participant_mail_snapshot_after_route_hooks`, which has one non-test caller (:1509), so not a seam. |
| `malformed_participant_channel_rings_once_then_repaired_file_delivers` :3051 | R | Ring once for a bad channel file, deliver after repair. |
| `unreadable_channel_fallback_delivers_remote_message_with_colliding_sender_id` :3187 | R | Identity fallback with colliding sender id. |
| `digest_groups_two_channels_and_mail_in_first_arrival_order` :3328 | R | Digest grouping and order. |
| `digest_sender_list_is_deduplicated_in_order_and_capped` :3361 | R | Sender list dedup and cap. |
| `digest_text_renders_sender_counts_and_singletons` :3387 | R | Text rendering. |
| `digest_text_distinguishes_lineages_that_share_one_workspace` :3409 | R | Group key includes lineage. |
| `digest_preview_is_capped_and_cannot_displace_the_true_fencepost_suffix` :3452 | R | Preview cannot displace the true `[first..last] --since` suffix. |
| `full_width_bracket_lookalikes_in_a_body_cannot_forge_the_fencepost` :3484 | R | Full-width brackets stay inert in the digest. |
| `snapshot_limit_is_applied_before_digest_grouping` :3504 | C | Same contract as cli `watch_snapshot_limit_digest_summarizes_only_admitted_events` (7014), which drives the real binary with a real limit. Fold any group-count assertion not already in 7014 into that row, then drop this. Medium confidence. |
| `empty_batch_produces_no_digest` :3520 | C | An empty snapshot also emits nothing at the CLI (`watch_snapshot_on_an_empty_mailbox_exits_zero_with_no_output` cli 6779). Drop after confirming 6779 runs with `--digest`; if it does not, add `--digest` to 6779 first. |
| `scan_batch_suppresses_declared_owned_rooms_but_not_merely_watched_ones` :3525 | R | `--own` suppression, owner of that contract. |
| `scan_batch_never_rings_for_the_rooms_own_messages` :3635 | C | Same suppression contract as :3525, weaker fixture. Fold its distinct row (own message, undeclared) into :3525 as a case. |
| `event_wake_drives_the_loop_and_once_exits_after_emitting` :3741 | F | Loop and `--once` behavior is worth keeping, but line 3812 asserts `room_dir/watch.heartbeat` exists for a `participant: None` target. Production long watches always have a participant, so this pins the dead else-arm of `touch_admitted_heartbeats_with` (watch.rs:1178). Repair: build the target with a participant and assert the participant heartbeat. |
| `slow_deadline_extends_fast_batch_instead_of_overwriting_it` :3819 | R | Deadline fix regression (`81c0f40`). Target shape is the unbound one, incidental to the contract (loop timing). |
| `continuous_events_for_one_room_cannot_starve_anothers_slow_scan` :3890 | R | Starvation regression. Same incidental target shape. |
| `notify_backend_rings_for_a_file_created_after_watch_starts` :3982 | R | Real notify backend end to end; the one test through inotify. |
| `a_wake_with_only_filtered_events_waits_out_its_deadline` :4041 | R | Filtered-wake fix (`fd16003`). |
| `past_its_deadline_the_wait_stops_consuming_filtered_events` :4065 | R | Same fix, other side of the deadline. |
| `own_presence_writes_do_not_wake_the_watch` :4090 | R | Self-wake loop guard. |
| `mail_arriving_after_filtered_events_still_wakes` :4120 | R | Real mail still wakes after filtered noise. |
| `inotify_reads_and_heartbeat_in_the_anchor_dir_are_not_a_wake` :4163 | R | Filter for reads and heartbeat in the anchor dir. |
| `reconcile_re_registers_a_replaced_watched_dir` :4190 | R | A replaced watched directory is re-registered. |
| `room_scan_profile_counts_mail_channels_and_channel_files` :4219 | R | `POST_WATCH_PROFILE` counters for the room scan. Documented (schema.rs:589, CHANGELOG, `skills/post/references/watch.md`). Uses `FORCED` and `profile_trace` cfg(test) items; see seams. |
| `participant_scan_profile_walks_files_after_every_timer` :4291 | R | Profile walk order (`ccd465f`). |
| bench, `#[ignore]` :4384 | R | Ignored bench (synthetic heavy stores, run with `--ignored --nocapture`); not part of the gate. Kept as tooling; see open questions. |
| `unchanged_record_rebuilds_nothing` :4574 | C | The unchanged-record arm is passed through by every slow-pass test, including `the_slow_pass_follows_a_rebind_then_stops_when_the_participant_ends` :4669. Fold as one assertion there. |
| `a_rebind_moves_the_targets_and_keeps_what_the_survivors_had_seen` :4585 | R | Rebind keeps per-address seen state. |
| `an_ended_participant_stops_the_watch_and_names_itself` :4621 | R | `ended_at` yields `watch_stopped`, asserted on "this watch is stopping". |
| `a_collected_record_stops_the_watch_and_names_the_participant` :4638 | R | Asserts message contains the id and "stopping". See open question 1: whether the "stopping" assertion is reachable on this path. |
| `the_slow_pass_follows_a_rebind_then_stops_when_the_participant_ends` :4669 | R | Whole slow-pass sequence through `run_watch_loop` with a scripted wake source; the loop-level owner. |

## D. `tests/cli.rs` (28 assigned)

| Test, line | Mark | Contract, regression |
|---|---|---|
| `watch_event_ndjson_warns_without_blocking_legitimate_forensics` :4273 | R | `WATCH_NDJSON_WARNING` fires on sending watch-event-looking text but does not block it. |
| `channel_watch_reports_backlog_live_events_omits_bodies_and_preserves_cursors` :5147 | R | Channel watch: backlog, live, no bodies, cursors untouched. |
| `participant_watch_targets_its_workspace_and_dedupes_channels_without_consuming` :5279 | R | Participant target expansion and channel dedupe. |
| `channel_watch_isolates_corrupt_channel_stores_and_still_rings_healthy_channels` :5405 | R | Corrupt channel isolation, cli level. |
| `watch_emits_backlog_then_live_arrivals_and_prints_sanitized_previews` :6391 | R | Live process, backlog then arrival. |
| `watch_digest_preview_cannot_forge_since_fencepost_or_change_floor` :6445 | R | Fencepost forgery through the real binary. |
| `watch_once_exits_zero_after_emitting_the_backlog` :6575 | R | `--once`. |
| `watch_from_now_suppresses_backlog_and_emits_post_start_mail` :6601 | R | `--from now` priming. |
| `watch_without_from_still_emits_the_startup_backlog` :6654 | R | Paired negative for the row above; different assertion (backlog present). |
| `watch_from_now_conflicts_with_snapshot_at_parse` :6677 | R | Parse conflict. |
| `watch_text_event_line_contains_full_message_id` :6695 | R | Full id in text line. |
| `watch_text_digest_suffix_runs_since_follow_up_for_exact_messages` :6708 | R | Digest suffix `--since 'id!'`. |
| `watch_snapshot_on_an_empty_mailbox_exits_zero_with_no_output` :6779 | R | Empty snapshot. Absorbs C row `empty_batch_produces_no_digest`. |
| `watch_snapshot_for_an_unregistered_room_creates_nothing_and_exits_zero` :6793 | R | No mailbox minted by a snapshot. |
| `watch_unreadable_channel_identity_survives_same_basename_and_multi_room` :6820 | R | Identity of unreadable channel files. |
| `watch_snapshot_emits_direct_and_channel_events_without_consuming_anything` :6877 | R | Snapshot read-only. Partially overlaps two doorbell tests (see A); kept. |
| `watch_snapshot_limit_emits_only_the_last_events_without_consuming_them` :6953 | R | `--limit`. |
| `watch_snapshot_limit_digest_summarizes_only_admitted_events` :7014 | R | Limit before digest. Absorbs C row :3504. |
| `watch_snapshot_conflicts_with_once` :7081 | R | Parse conflict. |
| `watch_limit_requires_snapshot_mode` :7088 | R | Parse rule. |
| `watch_snapshot_direct_scan_failure_is_a_nonzero_error_not_a_false_empty` :7101 | R | A failed direct-mail scan is an error, not empty output. |
| `watch_rings_for_malformed_mail_without_quoting_its_content` :7122 | R | No body quoting. |
| `watch_text_mode_escapes_control_characters_in_subjects` :7173 | R | Text escaping. |
| `unbound_long_watch_refuses_without_creating_an_unregistered_mailbox` :7215 | R | Refusal creates nothing. |
| `watch_text_mode_cannot_be_forged_by_crafted_filenames` :7239 | R | Filename forging. |
| `watch_text_mode_escapes_control_characters_in_from` :7284 | R | Text escaping of sender. |
| `watch_survives_the_mailbox_disappearing_and_rings_after_it_returns` :7325 | R | Resilience. |
| `snapshot_does_not_leave_a_live_heartbeat` :9323 | F | Contract (snapshot never touches heartbeats) is real. Assertions are stale: it checks `who.legacy_rooms[0].live_watch` and the legacy path `mail_root/alpha/watch.heartbeat`, which the participant-era watch does not write, so it would stay green if a snapshot minted a participant heartbeat. Repair: bind a participant, run the snapshot, assert `who` shows no live watch and no participant heartbeat file. |

The two `text_mode_escapes_control_characters` rows and the filename-forging row share the same sanitizer; each drives a different input field, so I kept them.

## E. `tests/routing.rs` (5) and `tests/participants.rs` (1), all R

| Test, line | Mark | Contract, regression |
|---|---|---|
| `watch_reason_filter_selects_each_reason_alone` routing :3426 | R | `--reason` selects each reason. |
| `watch_reason_filter_combines_and_leaves_the_default_unfiltered` :3461 | R | Combination and default. |
| `watch_reason_filter_applies_before_digest_grouping` :3502 | R | Filter before digest. |
| `watch_once_with_reason_emits_only_selected_events_and_ignores_filtered_batches` :3546 | R | Filter before the `--once` check. |
| `watch_profile_env_prints_one_stderr_line_per_target_scan_and_changes_nothing_else` :3597 | R | Profile is stderr only, stdout unchanged. |
| `participant_unbound_watch_snapshot_answers_with_one_marker_event` participants :3450 | R | Unbound snapshot answer. |

## Keeper per contract

- Snapshot is read-only, no cursors: cli 6877 plus doorbell :51 and :98.
- Wake vs Complete consumed-body handling: unit :2520, doorbell :364.
- Live watch never writes seen state: doorbell :132.
- Owned-room suppression (`--own`): unit :3525.
- Digest grouping, dedup, limit, forging: unit 3328-3484; limit and digest through the binary: cli 7014.
- Fencepost forgery and preview sanitizing: watch_preview :95 and cli 6445.
- Loop timing and filtered wakes: unit 3819, 3890, 4041, 4065, 4120.
- Identity follow: unit :4669, :4585, :4621.
- Profile diagnostic: unit :4219, :4291, routing :3597.
- Typed NDJSON wire contract: watch_preview :269.

## Test-only seams unlocked

1. `presence::touch_heartbeat` (presence.rs:34) and the `else` arm of `touch_admitted_heartbeats_with` (watch.rs:1178-1180). The only non-test caller is that arm, which runs only for `participant: None` targets, which a long watch never has (participant required). Test callers: presence.rs 314-452 unit tests and the unit tests above. Unlocked once F row :3741 is repaired and the presence.rs unit tests are checked. Confirm nothing else (bridge, who) reads the legacy `watch.heartbeat` first (see open question 2).
2. `#[cfg(test)] fn scan_batch` (watch.rs:2008-2028): three callers, all inside `mod tests` (3579, 3609, 3684). Production uses `scan_batch_measured`. Test-only wrapper; unlockable if the three tests call the measured fn directly. Not unlocked yet since C row :3635 and :3525 must land first.
3. `watch_profile_enabled` `FORCED` override and `profile_trace` cfg(test) module (watch.rs:73-97): used only by the two profile unit tests. The routing.rs profile test covers the env path end to end. Not unlocked: the unit tests count files walked, which the CLI test cannot see.

## Suspected product bugs

None confirmed. Possible: `src/commands/send.rs:1059` `is_watch_event_line` / `WATCH_NDJSON_WARNING` (:538) heuristics were only read, not exercised beyond cli 4273; I found no wrong behavior, only that a body that is valid NDJSON with a different `event` key is not warned. Unproven.

## Open questions (need a run)

1. `a_collected_record_stops_the_watch_and_names_the_participant` :4638 asserts `"stopping"`. `follow_identity` returns the raw `ParticipantMissing` error for that case (watch.rs:818), whose text (`error.rs:383`) has no "stopping". The assertion holds only if `resolve` returns `Resolved::Unbound` (test actor is bound by key, not by `POST_PARTICIPANT`) and `watch_stopped` fires at :834. That is plausible but unverified; if it takes the other branch the assertion would fail. It probably passes today, so it stays R. If the intended contract is the missing-claim error, the assertion is on the wrong branch.
2. Does anything besides `who` read the legacy `<room>/watch.heartbeat`? Needed before seam 1 is removed.
3. Is the `#[ignore]` bench at :4384 run by anyone? If not, it is a candidate for removal, outside test value.
4. Whether cli 6779 exercises `--digest` (decides C row `empty_batch_produces_no_digest`).
