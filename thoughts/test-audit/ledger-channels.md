# Ledger: area 3, channels (membership, crossed send, archive, join-from-now)

Read-only lane. No tests, cargo, or builds were run; everything is from reading tests, owners, callers, and `git log`. Marks: R retain, F fix assertion, C consolidate, D delete.

Owners read: `src/channel.rs` (production and tests), `src/channel_state.rs` (production to ChannelState, tests 810-988), `src/commands/chat.rs` (run, read, join, leave, send, archive toggle, crossed rendering, tests 2561-3503; `discard`/`discard_through` bodies and the render functions only skimmed), `src/commands/channels.rs` (first 80 lines). `src/channel_archive.rs` (204 lines) was NOT read; archive rows are judged from the assertions and `archive.rs` history only.

## Scope check against areas.md

Assignment matches: 21 + 6 + 35 cli + 2 participants + 26 + 11 + 4 = 105 declarations. Notes:
- cli :5815 (`inline_body_and_body_file_are_exclusive_alternatives`) exercises `send`, not chat: belongs to area 12 or 14.
- cli :9265 (`exact_fix_carries_a_body_full_of_angle_brackets...`) is a direct `send` to a participant: area 12.
- cli :8622 (`chat_body_markers_stay_behind_the_gutter`) and chat.rs `signed_status_*` overlap the owner-signing ledger (retained there too).
- cli :8886 watch half belongs to area 8.

## Tally

105 declarations: R 84, F 5, C 13, D 3. Almost all R: the join-from-now block, the channels_just_work file and the archive file were each written against a named bug. The C rows are real duplicates (crossed send, hash names, mentions); the D rows are unit tests whose contract a CLI test already binds.

## A. `tests/channels_just_work.rs` (21), all R

| Test :line | Mark | Contract / regression |
|---|---|---|
| `a_future_event_kind_passes_through_every_reader_and_a_send` :119 | R | Unknown event kinds are opaque to every reader and a send. Fails if a reader parses kind strictly. |
| `a_corrupt_message_file_is_skipped_and_reported_by_every_reader` :227 | R | Tolerant scans skip and report `skipped_files` on every reader. Fails on any reader turning a bad file into a hard error. |
| `a_crossed_send_delivers_and_reports_what_crossed_without_marking_it_read` :328 | R | Crossed-send keeper: delivered, `unseen`, `addressed_to_you`, 300-char preview, crossed messages stay unread, `crossed-send.jsonl` line. |
| `a_clean_send_carries_no_crossed_block` :390 | R | No crossed block when nothing crossed. |
| `an_addressed_crossed_body_keeps_its_trailing_whitespace` :410 | R | Addressed bodies are carried verbatim, not trimmed. |
| `crossed_text_receipt_lists_addressed_messages_first_and_in_full` :435 | R | Text receipt order and fullness. |
| `a_reply_to_your_message_counts_as_addressed_and_the_preview_is_capped_at_ten` :486 | R | `re` counts as addressed; `CROSSED_MESSAGE_CAP` 10. |
| `anyway_is_a_hidden_no_op_and_no_hint_still_names_it` :549 | R | Hidden `--anyway` still accepted, never suggested. Help-text grep is a mild string check. |
| `a_new_channel_name_is_normalized_and_odd_characters_are_refused_with_the_fix` :586 | R | Name normalization and refusal with fix. |
| `creating_a_channel_next_to_a_near_duplicate_asks_first` :624 | R | Look-alike detection with `--create`. |
| `an_existing_odd_channel_name_stays_reachable_by_its_exact_name` :692 | R | Legacy odd names remain addressable. |
| `a_leading_hash_is_accepted_wherever_a_channel_name_is` :732 | R | `#name` across join, send, peek, search, catchup. Overlaps cli :4901 and :5043 lightly; different verbs. |
| `a_channel_literally_named_with_a_hash_keeps_working` :765 | R | Legacy `#name` directory. Same fixture as the legacy half of cli :4901, which adds the "`legacy` does not resolve" negative. Not worth merging. |
| `chat_with_a_room_name_says_it_is_a_room_and_agrees_on_the_exit_code` :785 | R | Room name in chat gives the room-hint error and one exit code across consume/peek. |
| `chat_json_prints_nothing_on_stderr` :826 | R | JSON mode stderr silence. |
| `a_stray_positional_after_the_channel_is_refused_and_names_the_body_forms` :871 | R | Keeper for the stray-positional contract. |
| `a_join_never_splits_a_channel_that_differs_only_by_letter_case` :928 | R | Case-safe join. |
| `concurrent_joins_of_look_alike_names_create_one_channel` :991 | R | Only proof that `plan_join` runs inside `lock_channels`. Race test, retain. |
| `a_bounded_read_caps_its_skipped_list_and_never_loses_a_message_to_it` :1066 | R | `--max-bytes` bounded skipped list, no message lost. |
| `a_bounded_catchup_caps_its_skipped_list_and_never_loses_a_message_to_it` :1177 | R | Same for catchup. |
| `a_roster_names_the_member_it_left_out_for_an_invalid_membership_file` :1254 | R | Roster reports the invalid membership file. |

## B. `tests/archive.rs` (6)

| Test :line | Mark | Contract / regression |
|---|---|---|
| `archive_hides_from_listing_keeps_history_and_unarchive_restores` :47 | R | Sidecar-only archive, listing flags, idempotence, log grows `archive, unarchive`. |
| `a_new_post_resurrects_but_a_join_does_not` :101 | R | Post un-archives without a sidecar write; join does not. |
| `an_empty_channel_stays_archived_until_its_first_post` :134 | R | Empty channel edge. |
| `a_non_member_may_archive_and_search_archived_history` :148 | R | Non-member archive plus `search --archived`, membership rule returns after unarchive. |
| `archive_refuses_a_missing_channel_and_creates_nothing` :195 | F (minor) | Asserts only `!success`; any error passes, including a usage error. Repair: assert error code `not_found` (exit 66) and the channel-not-found message; keep the no-directory check. |
| `a_corrupt_archive_file_fails_open_and_doctor_reports_it` :207 | R | Fail-open plus doctor code `channel.bent.archive_invalid`. |

## C. `tests/cli.rs` (35)

| Test :line | Mark | Contract / regression |
|---|---|---|
| `send_to_a_channel_names_the_channel_verb_and_never_publishes_a_command` :3451 | R | 7 cases, store byte-identical. |
| `channel_two_room_flow_lists_participants_and_advances_each_seen_set` :4820 | R | Keeper for peek, consume and empty reads, per-participant seen sets. |
| `chat_accepts_a_leading_hash_as_the_rendered_channel_name` :4901 | R | Overlaps cjw :732/:765 but uniquely asserts `legacy` does not resolve. |
| `chat_subject_only_read_is_refused_without_a_command_or_a_store_change` :4958 | R | Refusal leaves store unchanged. |
| `chat_hash_name_forms_behave_exactly_like_the_bare_name` :5043 | R | Options loop, `--leave`, space normalization through `#`. Unique. |
| `chat_send_returns_its_receipt_when_the_own_seen_lock_held` :5649 | R | Regression: own-seen lock contention must not lose the receipt. |
| `chat_send_with_inline_text_in_the_old_file_slot_is_refused_naming_the_body_forms` :5738 | C | Into cjw :871. Carry the `!retryable` assertion over. |
| `chat_body_flags_imply_send_without_the_verb` :5773 | R | Not checked against other files for overlap; kept. |
| `inline_body_and_body_file_are_exclusive_alternatives` :5815 | R | Misassigned to this area (send). |
| `room_flag_on_channel_commands_names_the_cwd_bound_invocation` :5902 | R | Error text for `--room` on channel commands. |
| `channel_description_set_by_member_and_listed` :8539 | R | Description round trip. |
| `chat_body_markers_stay_behind_the_gutter` :8622 | R | Cross-ref owner-signing ledger. |
| `a_crossed_send_always_delivers_and_reports_what_crossed` :8739 | C | Into cjw :328 (and :549 for `--anyway`). Absorb: json-mode stderr contains no "unseen". |
| `every_crossed_send_is_recorded` :8843 | C | Into cjw :328. Its negative ("refused"/"anyway" outcomes absent) is vacuous: `log_crossed_event` hardcodes `"delivered_crossed"`. Absorb the `room` and `channel` field assertions. |
| `mentions_stamp_and_watch_reason_marks_at` :8886 | R | CLI wiring of mention stamping; watch half is area 8. |
| `threads_lite_stamps_re_and_renders_marker` :8926 | R | `re` stamp and text marker. |
| `seen_by_lists_members_past_a_message_read_only` :9111 | R | Also proves `mark_own_message_seen` (sender listed without reading). |
| `history_grep_filters_case_insensitive_regex` :9147 | F | Pattern "BETA two" equals the stored casing, so `case_insensitive(true)` is never exercised, and no regex metacharacter is used; a literal case-sensitive substring filter passes. Repair: grep "beta TWO" and add an alternation such as `alpha\|gamma` (unescaped in the actual arg) with the expected set. |
| `catch_up_never_silently_skips_mentions_of_reader` :9196 | R | Peek rescue of mentions in the skipped range. |
| `description_over_1kib_is_refused` :9243 | R | Size limit. |
| `exact_fix_carries_a_body_full_of_angle_brackets_without_tripping_the_guard` :9265 | R | Misfiled: direct send, area 12. |
| `crossed_send_text_receipt_shell_quotes_channel_metacharacters` :9299 | R | Runs the fix through a shell. |
| `history_survives_hand_written_non_ascii_re_without_panic` :9502 | R | Keeper for malformed `re` (absorbs unit `malformed_re_is_refused_at_parse`). |
| `mention_prefix_pairs_stamp_longest_only` :9576 | C | Into unit `extract_mentions_takes_longest_registered_name_per_at`; :8886 proves the CLI wiring. |
| `mention_boundary_is_unicode_alphanumeric` :9605 | C | Into the same unit test (boundary rows). |
| `join_from_now_fresh_joiner_sees_no_backlog_then_exactly_new_mail` :13347 | R | Core watermark. |
| `join_from_now_legacy_membership_starts_at_created_and_keeps_old_members_mail` :13443 | R | Legacy floor. |
| `join_from_now_mentions_obey_the_watermark` :13472 | R | Mentions and ring obey floor. |
| `join_from_now_backlog_flag_restores_the_whole_backlog_as_unread` :13505 | R | `BACKLOG_MEMBERSHIP_START`. |
| `join_from_now_backlog_from_an_explicit_member_says_it_changed_nothing` :13547 | R | Message truthfulness. |
| `join_from_now_rejoin_after_leave_treats_the_gap_as_history` :13599 | R | Rejoin. |
| `join_from_now_history_stays_reachable_by_peek_history_and_search` :13643 | R | Floor is not a visibility limit. |
| `join_from_now_old_format_channel_file_loads_and_falls_back_to_created` :13714 | R | Old file format; lightly adjacent to unit `unparseable_created...`, different input. |
| `join_from_now_explicit_join_by_a_legacy_member_keeps_its_floor` :13749 | R | Explicit join keeps floor. |
| `join_from_now_corrupt_history_file_does_not_ring` :13801 | R | No ring from a corrupt file. |

## D. `tests/participants.rs` (2), both R

`roomless_channel_send_reports_when_it_stays_local` :99 (cross_host receipt states); `channel_send_receipt_distinguishes_relay_state_and_reserved_names` :207 (relay state plus the cross-language name-verdict table shared with Python `test_rooms.py`).

## E. `src/channel.rs` units (11)

| Test :line | Mark | Contract / regression |
|---|---|---|
| `microsecond_ids_sort_chronologically_and_validate` :1961 | R (weak) | Only direct proof of the micro-timestamp and id shape; message-storage ledger points at it. Unused `test_context`/`bind_test_actor` setup. |
| `control_characters_in_channel_field_are_refused_at_parse` :1993 | R | Integration overlap not checked; kept. |
| `second_resolution_mail_id_is_refused_for_channel_messages` :2021 | R | Same. |
| `join_records_event_then_membership_and_send_roundtrips` :2047 | R | Drives `write_message` directly with a hand-written `members.json`. Looks covered by integration join/send/history but I did not confirm every assert; not a D. |
| `send_stamps_profile_as_of_send_time_and_rename_does_not_retcon` :2111 | C | Into `participants.rs` profile-stamping tests (~3120-3260). Must add a rename step (first stored message keeps the old name); integration proves send-time stamping only via unnamed, named, cleared. Medium confidence. |
| `blocked_route_bars_shared_membership_in_both_directions` :2180 | F | Re-implements the predicate inline (`members.keys().any(..rule.matches_route..)`) and never calls `join_resolved` (channel.rs:733); passes with production broken. Repair: move to routing.rs beside :2269 as a table over both rule directions on a real join, asserting exit 77, code `blocked_route`, no writes (routing.rs covers only one direction). |
| `list_channels_skips_strays_and_counts_messages` :2204 | F | Calls the test-only strict `list_channels` wrapper, so the tolerant production path (`list_channels_with(.., Scan::Tolerant)`) is unproven for stray directories. Repair: call the tolerant fn or add a stray dir to an integration `channels` test; then delete the wrapper. |
| `embedded_at_in_registered_name_does_not_double_stamp` :2244 | R | |
| `extract_mentions_takes_longest_registered_name_per_at` :2264 | R | Mention keeper. |
| `malformed_re_is_refused_at_parse` :2319 | C | Into cli :9502 (keeper). |
| `participant_review_unit_channel_send_stamps_actor_fields` :2345 | D | Asserts only `is_string` and `address_kind == "channel"`. Stronger proof: `participants.rs:975-977` asserts `from_participant == sender_id`, `from_lineage`, `address_kind` on a real send. |

## F. `src/channel_state.rs` units (4)

| Test :line | Mark | Contract / regression |
|---|---|---|
| `unparseable_created_falls_back_to_the_backlog_floor` :820 | R | Unique (43d2318). |
| `a_join_writes_its_start_first_and_a_leave_writes_channels_first` :855 | R | `write_order` is used by production `mutate` (channel_state.rs:680), so not a seam. |
| `load_prefers_materialized_cursor_over_legacy_channel_state` :893 | C | Into `cursor_state.rs` legacy-import tests (area 2), adding a stanza where legacy and `cursors.json` differ. Unlocks deleting `ChannelState::has_seen`. |
| `participant_leave_is_individual_and_preserves_cursor_state` :925 | R | Only proof of individual leave for legacy workspace membership (routing.rs:2865 covers explicit-join only). |

## G. `src/commands/chat.rs` units (26)

| Test :line | Mark | Contract / regression |
|---|---|---|
| `batch_reads_all_then_only_new_after_advance` :2692 | C | Into cli :4820 (consume then only new). |
| `unadvanced_cursor_reshows_batch` :2717 | D | Peek reshow: cli :4820; refused `--discard` leaves batch unread: cli :5943 (area 2); failed emit records no seen id: `consuming.rs:71`. |
| `non_member_read_is_refused_with_join_fix` :2730 | F (minor) | Asserts only code `not_a_member`. Repair: also assert the `post chat 'tax' --join` fix. No other proof for chat reads (catchup.rs:136 is catchup only; routing.rs:2914 is a send). |
| `missing_channel_is_not_found` :2739 | D | cjw :785 (consume `chat gamma` gives 66) and `participants.rs:2476` (peek `not_found` plus fix) both go through `require_channel`. |
| `send_marks_own_message_seen_so_it_never_reshows` :2747 | C | Into cli :9111; the empty-read half is guaranteed by `from != self` and proved by unit :3435. |
| `send_marks_own_id_even_when_others_are_unread` :2771 | C | Into cjw :328 (crossed messages stay unread) plus cli :9111. |
| `late_bridged_arrival_between_read_and_own_send_surfaces` :2798 | R | Own send then a late lower id; sibling cases (`consuming.rs:147`, cursor_state unit) lack the own send. |
| `collect_batch_since_filters_and_ignores_cursor` :2844 | R | CLI `--since` reaches `collect_batch_scanned` via cli :6764 (watch digest copyable command) but nothing at CLI proves "ignores cursor". |
| `peek_catch_up_trims_to_newest_and_reports_older_slice` :2877 | R | Compare cli `default_catch_up_skips_older...` (area 2); kept. |
| `catch_up_larger_than_batch_is_a_plain_read` :2894 | R | Suspected trivial (empty batch) but not verified; not a D. |
| `limit_zero_means_unlimited` :2908 | C | Into cli ~8586-8620 (`--limit 0`: count 30, skipped 0 vs 25/5). |
| `catch_up_rescues_mentions_from_skipped_range` :2922 | C | Into cli :9196. |
| `auto_reads_are_quiet_without_daily_state` :2943 | R | Uses test-only `render_text` wrapper; overlaps area 2 framing tests (cli 12597-12755), not compared line by line. |
| `compact_framing_never_stamps_banner_day` :2982 | R | Same. |
| `explicit_full_always_walls_and_never_stamps_banner_day` :3030 | R | Same. |
| `signed_status_detects_and_fails_safely` :3058 | R | Retained overlap with owner-signing ledger. |
| `signed_status_multiline_body_never_reaches_verification` :3148 | R | Same. |
| `signed_age_minutes_math` :3230 | R | `signed_age_minutes` has production callers (mailbox.rs:1746, 1822). Lives in chat.rs but tests mailbox.rs; misplaced. |
| `join_events_render_with_label_and_messages_in_id_order` :3239 | R | |
| `stamped_profile_renders_name_and_id_in_chat_line` :3290 | R | |
| `absent_profile_chat_line_is_byte_identical` :3319 | R | |
| `unreadable_message_is_skipped_reported_and_never_consumed` :3341 | R | |
| `unreadable_at_or_below_cursor_is_ignored_on_fail_closed_read` :3382 | R | |
| `discard_consumes_exactly_the_rendered_batch_even_if_mail_arrives_before_the_callback` :3397 | R | Needs the `after_stdout` callback. |
| `own_message_absent_from_seen_never_surfaces_to_its_sender` :3435 | R | Unique. |
| `discard_through_replay_reports_marked_not_advanced_from_cursor_to_cursor` :3470 | R | Exact late-replay accounting; cli :6100 is only a one-line summary. |

## Keeper per contract

- Crossed send: cjw :328 (absorbs cli :8739, :8843), :549 for `--anyway`.
- Stray positional: cjw :871 (absorbs cli :5738).
- Hash names: cjw :732 and cli :5043 (verbs and options), cli :4901 (legacy negative).
- Mentions: unit `extract_mentions_takes_longest...` (absorbs cli :9576, :9605); cli :8886 for wiring.
- Malformed `re`: cli :9502.
- Consume/peek/seen sets: cli :4820. Own-message seen: cli :9111 plus unit :3435.
- Join-from-now: the cli 13347-13801 block plus channel_state units.
- Archive: archive.rs.

## Test-only seams unlocked

- `channel::list_channels` (`channel.rs` ~1848, `#[allow(dead_code)]`): sole caller is the unit test at :2236 (the watch.rs:2168 hit is a comment). Its `Scan::Strict` branch in `list_channels_with` is reached only through it (channels.rs and search.rs pass Tolerant). Deletable once :2204 is repaired to use the tolerant path.
- `ChannelState::has_seen` (`channel_state.rs:794-799`, `#[allow(dead_code)]`): only callers are the unit test at :919-920. Deletable after the C move of :893. `ChannelState::load`/`into_channels` stay (`watch.rs:2322`).
- `chat.rs:1811` `#[cfg(test)] fn render_text`: callers only in unit tests (2948-3324). Keeping those tests keeps it; it is not unlocked.
- Dead flexibility: `mark_own_message_seen`'s `lock_budget` parameter; every caller (production `send` and tests) passes `OWN_SEEN_LOCK_BUDGET`.
- Not unlockable: `ParticipantChannels::join` (`#[cfg(test)]`) is used by cursor_state.rs and watch.rs tests.
- Stale attributes: `#[allow(dead_code)]` on `ParticipantChannels::leave` (channel_state.rs:172, called by chat.rs `leave`) and on `channel_state_path` (channel.rs:64, callers doctor.rs:821, cursor_state.rs:962).

## Suspected product bugs

None confirmed. Nits: the stale `allow(dead_code)` attributes above; `log_crossed_event` writes a hand-formatted JSON line with a hardcoded outcome, so the log's "outcome" field carries no information.

## Open questions

- Did any unit in E/G duplicate integration asserts more fully than I found (`join_records_event...` :2047, `catch_up_larger_than_batch...` :2894, `control_characters...` :1993, `second_resolution...` :2021)? These need a line-by-line comparison; I marked them R.
- `src/channel_archive.rs` was not read; archive.rs rows rest on assertions and history only.
- Whether the framing units (:2943, :2982, :3030) duplicate area 2 cli 12597-12755 needs a side-by-side read; a run is not required.
- Repairs proposed for the F rows (history_grep, blocked_route table, list_channels) should be red-proofed by mutation when execution is allowed again.

## Support and duplication notes

`write_raw_message` and `write_corrupt_message` in cjw partly duplicate `tests/common` helpers; `jfn_*` helpers in cli.rs are join-from-now local; the crossed-send fixtures in cjw :328 and cli :8739/:8843 are copies.
