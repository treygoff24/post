# Ledger: area 2, read, catchup, search, byte budget

Read-only lane. No tests, cargo, or builds were run (Trey's 2026-09-29 rule); everything below is from reading tests, owners, callers, and `git log`. Marks: R retain, F fix assertion, C consolidate, D delete.

Owners read: `src/commands/read.rs`, `catchup.rs`, `byte_budget.rs`, `search.rs`, `inbox.rs`, `src/cursor_state.rs` (plus `channel_state.rs` and `watch.rs` callers of it), `tests/common` sandbox helpers (`run_in_env`, `seed_fence_store`).

## Scope check against areas.md

- areas.md says 66 declarations; I count 68 integration (byte_budget.rs 20, catchup.rs 13, consuming.rs 4, search.rs 6, cli.rs 25) plus 27 unit tests (cursor_state.rs 14, read.rs 4, catchup.rs 3, byte_budget.rs 2, inbox.rs 2, search.rs 2) = 95 total.
- `src/commands/delivery.rs` (`post delivery`) is listed here and under area 15. It has no unit tests and none of my tests touch it; it belongs to area 15.
- `sender_label` / `sender_label_quoted` are tested in `output.rs` (area 13); `eligibility.rs` is `#[path]`-included by `cursor_state.rs` (area 1). Only the overlap is noted.

## Tally

95 declarations: R 67, F 14, C 6, D 8. Mostly R; the F/D findings cluster in three families:
1. Assertions about `banner-day`, a feature retired in `c948195` (2026-09-17); no production code reads or writes it, so "never stamps banner-day" cannot fail.
2. Room-level `<room>/cursors.json` and `.cursors.lock` absence checks. Since `4539250` ("Chat reads require a bound participant: drop the legacy room arms") cursors live only under `participants/<id>/`, so these paths are never written and the checks are vacuous.
3. Mail-consumption checks phrased as "the inbox file still exists". Canonical mail is immutable (read never unlinks it), so that cannot detect consumption or its absence. The real signal is `participants/<id>/cursors.json` `seen`.

Plus the dead legacy room cursor writer in `cursor_state.rs`, whose only callers are its own unit tests (see seams).

## A. `tests/byte_budget.rs` (20)

| # | Test, file:line | Mark | Contract, regression, evidence |
|---|---|---|---|
| B1 | `chat_budget_admits_only_a_complete_prefix_and_consumes_only_that_prefix` | R | 8000-byte whale under a 2000 cap: contiguous prefix, participant `seen == [ids[0]]`, omitted `first_id`. Fails if admission slices a message or consumes the omitted ones. |
| B2 | `channel_utf8_slices_reconstruct_the_body_and_never_consume` | R | Slices rejoin to the exact body on UTF-8 boundaries; no slice consumes. Fails on a mid-scalar cut. |
| B3 | `direct_mail_budget_slices_and_exact_ack_preserve_unrelated_unread_mail` | F | Ack half is real (`seen == [ids[1]]` after the ack). The claim that the omitted read and the slices did not consume is unobserved: `seen == [ids[1]]` holds whether or not an earlier read consumed something else only if it consumed nothing, but the `inbox.exists()` checks cannot fail (immutable mail). Repair: assert the cursor `seen` is absent or empty before the ack. |
| B4 | `channel_exact_ack_marks_only_its_target_even_for_a_rescued_old_mention` | R | Exact ack consumes only its target id, including an out-of-window rescued mention. |
| B5 | `catchup_uses_one_budget_across_targets_and_consumes_only_complete_prefixes` | R | One shared budget across mail and channel targets; channel `seen` check is real. Mail half is only a file-exists check (note: add a mail `seen` check). Trailing crossed-send `unseen == 2` belongs to area 3. |
| B6 | `pretty_catchup_admits_mail_and_channel_messages_at_their_exact_full_size` | F | Uncapped run uses `--max-bytes 100000` (6 digits in `byte_limit`), so the tight run has about 2 bytes of slack and is not exact. Repair: re-run with `--max-bytes exact.stdout.len()` and assert count is still 2 (and cap-1 omits). Slack size is an estimate (open question 1). |
| B7 | `budget_caps_json_pretty_and_text_after_utf8_and_escape_encoding` | F (provisional) | By my arithmetic the escaped body is about 512 bytes against caps of 2200-3200, so the caps look loose and may not bind on escape/UTF-8 overhead. Unmeasured. Repair: derive each cap from the measured full length, then assert cap admits and cap-1 omits. |
| B8 | `auto_text_peeks_and_consuming_reads_never_stamp_banner_day` | F | The `banner-day` assertions are vacuous (retired feature). Keep: budgeted peek and consuming text reads are quiet, and budgeted text consumption `seen == [id]`. Repair: drop the banner-day checks and rename. |
| B9 | `fenced_auto_text_stays_quiet_and_preserves_old_stamp` | F | Banner-day bytes/mtime, `dest/cursors.json`, `dest/.cursors.lock` are room-level or dead paths nothing touches. Keep the no-wall assertions. Repair: snapshot the mail root tree before and after (as the area 9 search check at cli.rs:7460-7490 does with `read_dir`) and assert equality. |
| B10 | `consuming_banner_state_uses_raw_room_identity_not_sanitized_display` | D | Its subject is banner-day room identity; banner-day has no production code (`c948195`, schema says no mode consults or stamps it). The only other assertion `contains("#identity")` is generic and is covered by the text-read tests. No contract remains. |
| B11 | `too_small_scaffolds_fail_on_stderr_without_stdout_or_consumption` | R | Cap below the minimum scaffold: stderr error, empty stdout, no channel cursor change (real). Mail `inbox.exists()` is vacuous (note only). |
| B12 | `budgeted_peek_keeps_mention_rescue_order_and_reports_omitted_mentions` | R | Rescue order and omitted-mention reporting under a budget. |
| B13 | `omission_continuations_measure_large_slice_scaffolds_for_chat_read_and_catchup` | R | Pins the fixed-point `minimum_progress_budget` for continuation caps. The six `any(start<10 && end>=10)` / `<100` assertions are always true once the body reconstructs (noise, not a defect). |
| B14 | `count_window_and_byte_omissions_remain_distinct_in_one_result` | R | Count-window omissions and byte omissions are reported separately. |
| B15 | `malformed_selected_messages_are_reported_not_hidden_by_a_budget` | R | Malformed selected messages appear in `skipped` despite a budget. |
| B16 | `no_flag_preserves_legacy_json_shape` | R | Without `--max-bytes` the JSON has no budget fields. |
| B17 | `complete_budgeted_direct_read_consumes_after_full_body_output` | F | Name claims consumption; the only check is `inbox.exists()`, which cannot fail. Repair: assert cursor `mail.workspace:beta.seen == [id]`. |
| B18 | `slice_overflow_eof_and_mode_conflicts_are_explicit` | R | Each negative would otherwise exit 0 or 66, so they bind on the intended error. |
| B19 | `failed_stdout_applies_no_budgeted_read_or_exact_ack_delta` | F | Chat and catchup checks are real (exit 75 `io_error`, no delta). The two mail budgeted-read and mail-ack checks are vacuous file-exists checks, plus a vacuous `banner-day` line. Repair: assert the mail cursor is unchanged; drop banner-day. |
| B20 | `strict_stdout_preserves_committed_delivery_and_registration_rules` | R | The `fc4b870` double-send regression: strict stdout must not undo a committed send. |

## B. `tests/catchup.rs` (13)

| # | Test, file:line | Mark | Contract, regression, evidence |
|---|---|---|---|
| C1 | `channel_catchup_returns_full_slice_then_fresh_invocation_is_empty` | R | Catchup returns the slice, consumes it, a second run is empty. |
| C2 | `all_reports_inspected_empty_targets` | R | `--all` lists inspected targets even when empty. |
| C3 | `fresh_text_catchup_reports_caught_up_without_creating_mailbox_dirs` | R | Text `post: caught up (0 unread)` with no mailbox dirs created. Keeper for the unit `empty_text_has_no_banner` if tightened to exact string equality. |
| C4 | `nonempty_text_catchup_has_no_banner_and_keeps_mail_kind` | R | No policy banner; mail kind retained. |
| C5 | `positional_channel_catchup_refuses_non_member_room` | R | Non-member room refused. |
| C6 | `all_skips_corrupt_unjoined_channel_and_delivers_mail` | R | Corrupt unjoined channel skipped with a warning, mail still delivered. |
| C7 | `all_reports_and_skips_a_joined_channels_malformed_message` | R | Keeper for the tolerant-scan `skipped` report on joined channels. |
| C8 | `malformed_channel_entry_is_skipped_and_reported_without_touching_the_file` | C | Same contract as C7. Absorb into C7 by carrying the positional `catchup tax` run as a second invocation. Its `beta/cursors.json` assertion is a vacuous room-level path. |
| C9 | `nonempty_catchup_to_dev_null_refuses_without_cursor` | F | Refusal is real; the no-cursor check reads `beta/cursors.json` (never written). Repair: check `participants/<id>/cursors.json` is unchanged, or that a follow-up catchup still returns the message, as `cli.rs channel_read_into_dev_null...` does for chat. |
| C10 | `malformed_mail_warns_and_valid_mail_is_seen_without_moving` | R | Malformed mail warned on stderr, valid mail delivered. "seen" is never asserted (file-exists only); add a cursor check as a note. |
| C11 | `fenced_catchup_refuses_before_cursor_or_room_mutation` | R | Bound fenced store: refused, no mutation. The area 9 matrix (cli.rs:7410) asserts only refused, empty stdout, no participant cursors.json for `--mail --json`; this test also covers channel targets. Partial overlap, both kept. |
| C12 | `matching_generation_allows_empty_catchup` | R | Matching generation admits an empty catchup. |
| C13 | `forged_section_markers_in_body_cannot_reach_column_zero` | R | Forged `#tax · ` markers in bodies are guttered; only the genuine section starts at column 0. |

## C. `tests/consuming.rs` (4)

| # | Test, file:line | Mark | Contract, regression, evidence |
|---|---|---|---|
| K1 | `direct_read_records_participant_seen_and_keeps_canonical_mail` | C | Same as cli.rs:679 (`read_never_unlinks_canonical_mail_and_records_exact_participant_seen_id`, X6). Absorb there and tighten X6 to exact `seen == [id]` equality. |
| K2 | `failed_emit_records_no_participant_seen_id` | R | Unbudgeted mail path with a proper cursor check: failed stdout, no seen id. |
| K3 | `bounded_channel_consumption_marks_only_emitted_page` | R | Only the emitted page is consumed. |
| K4 | `late_older_channel_id_remains_unread_and_seen_by_names_participants` | R | Keeper for seen-set semantics (late older ids stay unread) and `--seen-by`. |

## D. `tests/search.rs` (6), all R

| # | Test, file:line | Mark | Contract, regression, evidence |
|---|---|---|---|
| S1 | `search_mail_is_visible_history_for_recipient_and_sender_after_consumption` | R | Search sees mail after consumption, for both sides. |
| S2 | `search_channel_is_member_history_after_read_and_for_the_sender` | R | Channel history for members. |
| S3 | `search_imported_roomless_channel_sender_keeps_host_and_reply_address` | R | Imported roomless sender keeps host and reply address. |
| S4 | `search_literal_caps_and_sanitizes_previews` | R | Literal matching, result cap, preview sanitizing. No integration test covers case folding, the fullwidth-bracket replacement, or the 160 cap (unit tests cover the last). |
| S5 | `invalid_membership_closes_explicit_channel_without_opening_messages` | R | Invalid membership closes an explicit channel. |
| S6 | `search_succeeds_on_fenced_store_with_stale_generation` | R | Search is read-only and allowed under a fence. The area 9 matrix (cli.rs:7460-7490) also asserts search leaves the fenced root unchanged; different focus (stale generation vs unchanged root), keep both. |

## E. `tests/cli.rs` (25), all R

| # | Test, file:line | Mark | Contract, regression, evidence |
|---|---|---|---|
| X1 | `default_read_has_no_policy_prose_in_text_or_json` :392 | R | Default read has no policy prose. Weak point: assert `laws.is_empty()` rather than one law's absence. |
| X2 | `compact_framing_read_keeps_laws_schema_and_body_across_both_modes` :431 | R | Keeper for compact read text and JSON. Stale comment says "Default stays the full banner" while asserting no wall. Also the keeper for unit `compact_framing_keeps_the_law_and_header` (text half). |
| X3 | `compact_framing_chat_read_carries_laws_and_is_rejected_on_non_reads` :487 | R | Compact on chat read, rejected elsewhere. |
| X4 | `auto_peeks_stay_quiet_while_explicit_full_is_available` :586 | R | Only chat-level integration proof that `--framing full` shows the wall. Stateful sequence and comments are stale banner-day relics (cosmetic). |
| X5 | `read_ignores_legacy_read_collision_and_keeps_both_files_unchanged` :642 | R | Legacy `.read` collision ignored. |
| X6 | `read_never_unlinks_canonical_mail_and_records_exact_participant_seen_id` :679 | R (keeper) | Absorbs K1. |
| X7 | `id_prefixes_resolve_uniquely_and_ambiguity_lists_matches` :726 | R | Prefix resolution and ambiguity listing. |
| X8 | `read_serves_already_read_mail_by_prefix_instead_of_reporting_it_missing` :5856 | R | Already-read mail is served by prefix. |
| X9 | `read_of_a_wholly_unknown_prefix_names_the_participant_visibility_boundary` :5884 | R | Unknown prefix message names the visibility boundary. |
| X10 | `channel_read_into_dev_null_is_refused_and_discard_is_the_deliberate_form` :5943 | R | Consuming read to /dev/null refused; discard is the deliberate form. One vacuous `banner-day` line (drop). |
| X11 | `discard_through_advances_exactly_to_the_target_and_replays_as_a_no_op` :6026 | R | Exact advance and idempotent replay. |
| X12 | `discard_through_text_mode_summarizes_in_one_line` :6081 | R | One-line text summary. |
| X13 | `discard_through_accepts_an_unambiguous_prefix_and_refuses_an_ambiguous_one` :6102 | R | Prefix handling. |
| X14 | `discard_through_refuses_unknown_ids_and_ids_from_another_channel` :6125 | R | Unknown and foreign ids refused. |
| X15 | `discard_through_refuses_to_leap_over_an_unreadable_predecessor` :6163 | R (F-lite) | The refusal and the follow-up allowed ack are real; the `beta/cursors.json` absent check is a vacuous room-level path (drop it, or check the participant cursor). |
| X16 | `concurrent_acks_on_two_channels_from_two_processes_both_land` :6202 | R (keeper) | Real two-process flock race. Absorbs the lock-mode assertion from unit `concurrent_writers_union_the_whole_map`. |
| X17 | `discard_through_conflicts_with_the_flags_that_would_contradict_it` :6327 | R | Flag conflict matrix. |
| X18 | `default_catch_up_skips_older_unless_limit_zero` :8587 | R | Default catch-up window vs `--limit 0`. |
| X19 | `discard_receipt_counts_full_unread_past_catch_up` :9653 | R | Discard receipt counts the full unread set. |
| X20 | `plain_read_skips_and_reports_an_unreadable_file_then_emits_it_after_repair` :9683 | R | Skip, report, then emit after repair. |
| X21 | `send_delivers_and_reports_an_unreadable_unseen_file` :9758 | R | Send is not blocked by an unreadable unseen file. |
| X22 | `legacy_compact_env_selects_quiet_reads_without_restarting_agents` :12597 | R | `POST_FRAMING` legacy compact env. |
| X23 | `explicit_framing_flag_beats_post_framing_env` :12679 | R | Flag beats env. |
| X24 | `invalid_post_framing_env_warns_and_reads_as_auto` :12723 | R | Invalid env warns and falls back to auto. |
| X25 | `read_recognizes_a_channel_message_id_and_names_a_command_that_shows_it` :13071 | R | Channel id passed to `read` names the right command. |

## F. Unit tests

### `src/cursor_state.rs` (14)

| # | Test | Mark | Contract, regression, evidence |
|---|---|---|---|
| U1 | `legacy_room_cursor_write_refuses_a_room_mid_rename` | D | Exercises the dead module-level room writer (no non-test caller since `4539250`). No live contract; the live writer's refusal paths are untested (see U9 repair). |
| U2 | `missing_and_malformed_cursor_are_empty_without_writes` | R | `Snapshot::load` is live via `ChannelState::load` at watch.rs:547. Uses dead accessor `mail_has_seen` (swap when seams are removed). |
| U3 | `participant_cursor_lock_refuses_symlink_hardlink_and_fifo_without_mutation` | R | Live participant lock hardening. |
| U4 | `exact_v1_serialization_round_trips_mail_and_channels` | F | Pins the dead v1 room writer format. The live v2 exact bytes (CONTRACT.md ~216: version 2, sorted pretty JSON, trailing newline) are pinned nowhere. Repair: port to `ParticipantCursors::consume_*` and assert the v2 bytes. |
| U5 | `unread_channel_skipping_consumed_matches_the_complete_projection` | R | Only exact-equality proof of the fast projection (the watch.rs bench-like test compares lengths only). |
| U6 | `legacy_import_is_read_only_then_materialized_without_touching_legacy` | F | Read half is live; the materialize half exercises the dead writer. Repair: assert import through the live participant path, drop the room-writer half. |
| U7 | `legacy_v2_import_is_read_only_then_materialized_without_touching_legacy` | F | Same split as U6. |
| U8 | `legacy_v1_migration_error_refuses_write_without_losing_prior_channels` | D | Tests the dead writer's migration error; no live caller. |
| U9 | `symlinked_cursor_degrades_on_read_and_refuses_write` | F | Writer half pins dead code; the live `ParticipantCursors` writer's refusal on a symlinked or malformed cursors.json is untested. Repair: retarget the write half to the participant writer. |
| U10 | `late_channel_id_below_maximum_stays_unread` | D | Keeper is `late_older_channel_id_remains_unread_and_seen_by_names_participants` (K4), which runs the live path; this one runs the dead writer. |
| U11 | `participant_cursor_symlink_is_refused_on_read` | R | Live read refusal. |
| U12 | `participant_cursor_replaced_between_check_and_read_keeps_one_verdict` | R | TOCTOU; requires the `CURSOR_READ_HOOK` seam (`ff20812`), no alternative. |
| U13 | `bounded_participant_lock_times_out_names_the_lock_and_writes_nothing` | R | `consume_channel_within` has a non-test caller at chat.rs:2551. |
| U14 | `concurrent_writers_union_the_whole_map` | C | Tests the dead room writer. Absorbed by X16; carry the lock 0600 assertion there. |

### `src/commands/read.rs` (4)

| # | Test | Mark | Contract, regression, evidence |
|---|---|---|---|
| U15 | `stamped_profile_renders_in_from_room_line` | R (weak) | Only proof that the Full read header uses `sender_label`. |
| U16 | `absent_profile_from_room_line_is_byte_identical` | C | Absorb into U15 as a second row. |
| U17 | `compact_framing_keeps_the_law_and_header` | D | Keeper: X2 text half asserts the law and header. |
| U18 | `compact_framing_does_not_alter_the_body` | C | Absorb into U15 with an `ends_with(body)` assertion. |

### `src/commands/catchup.rs` (3)

| # | Test | Mark | Contract, regression, evidence |
|---|---|---|---|
| U19 | `empty_text_has_no_banner` | C | Absorb into C3, tightened to exact `post: caught up (0 unread)\n`. |
| U20 | `nonempty_text_has_no_policy_banner` | D | Synthetic count=1 with no messages; keeper is C4 which runs a real non-empty catchup. |
| U21 | `selected_delta_does_not_consume_messages_arriving_after_selection` | R | Late-arrival race cannot be interleaved from a CLI test. Depends on the `bind_test_actor` seam and shared 2026-01-01 seed (`0bc81fb`). |

### `src/commands/byte_budget.rs` (2)

| # | Test | Mark | Contract, regression, evidence |
|---|---|---|---|
| U22 | `json_array_prefix_matches_real_compact_and_pretty_layouts` | R | Oracle is `crate::output::json_len`; pins layout arithmetic that B6/B7 depend on. |
| U23 | `json_array_prefix_serializes_each_item_once_then_probes_in_constant_work` | F | The 100-iteration probe loop is vacuous: `JsonArrayPrefix` holds no items, so it cannot observe re-serialization. Keep only the constructor call-count assertion; repair: count serializations across probe calls with a counting item type, or delete the loop. |

### `src/commands/inbox.rs` (2)

| # | Test | Mark | Contract, regression, evidence |
|---|---|---|---|
| U24 | `inbox_line_sender_absent_profile_is_byte_identical` | D | Only calls `output::sender_label_quoted`; never touches inbox.rs code. Stronger proof: output.rs `text_line` dressed/bare tests and the `sender_label` tests (area 13, ~2338-2490). |
| U25 | `inbox_line_sender_renders_stamped_profile` | D | Same. Caveat: if area 13's tests do not cover the inbox line's call, only the wiring is unproven; no test here proves wiring either. |

### `src/commands/search.rs` (2)

| # | Test | Mark | Contract, regression, evidence |
|---|---|---|---|
| U26 | `literal_matching_is_unicode_case_insensitive` | F | Positive half is the only case-folding proof. Negative `!matches("prefix [a] suffix")` is vacuous: the pattern "Ä" has no metacharacters. Repair: drop it or use a literal with a metacharacter, and add an ASCII case check. |
| U27 | `preview_is_flattened_and_capped_by_scalars` | R | Only proof of the 160 cap and the ellipsis. The `[` to fullwidth bracket replacement is untested (add a row). |

## Keeper per contract

- Chat/mail budget admits only complete prefixes, consumes only those: B1, B5, B12, B14.
- UTF-8 slicing and continuation fixed point: B2, B13, B18, U22.
- Exact ack: B4 (channel), B3 after repair (mail).
- Failed stdout leaves no delta: B19 (chat, catchup; mail after repair), K2.
- Seen-set semantics and `--seen-by`: K4.
- Direct mail read records exact participant seen id: X6 (absorbs K1).
- Two-process cursor writes both land: X16.
- Participant cursor file hardening: U3, U11, U12, U13.
- Catchup tolerant scan and skipped report: C7 (absorbs C8).
- Catchup text framing (empty, non-empty, forged markers): C3, C4, C13.
- Compact framing: X2, X3; explicit full: X4; env vs flag: X22-X24.
- Discard-through: X11-X15, X17, X19.
- Search visibility and fence: S1-S6; preview and case folding: U26, U27.
- Fast projection equals the full projection: U5.

## Test-only seams unlocked

Dead legacy room cursor writer in `src/cursor_state.rs`, evidence from `rg`: no non-test caller remains after `4539250` (which removed the room-arm callers and the module-level `consume_channel_within`).
- Removable once U1, U4, U6-U10, U14 are retargeted or deleted: module-level `consume_channel` (~:632, `#[allow(dead_code)]`), `consume_channel_waiting`, `consume_channel_through` (~:671), `consume_inner`, `validate_channel_seen`, `lock_room_cursors`, `load_for_write`, `replace_state`, `serialize_state`.
- `Snapshot::{mail_has_seen, channel_has_seen, max_seen, channel_seen_count}` (all `#[allow(dead_code)]`).
- `ChannelState::has_seen` in `channel_state.rs` (`#[allow(dead_code)]`, used only by its own test; that test is area 3).
- Keep: `Snapshot::load` and `into_channels` (live via `ChannelState::load`, watch.rs:547).

Test-only seams that must stay: `CURSOR_READ_HOOK` (U12), `catchup.rs` line 1 `#[cfg(test)] use crate::channel;` (U21), `bind_test_actor` (area 5). `read.rs #[cfg(test)] fn render_text` is only removable if U15 moves to the CLI (X-level); `ReadProjection::legacy` is not test-only (non-test caller catchup.rs:814).

## Suspected product bugs and drift

- CONTRACT.md says a 50,000-id warning remains the operational threshold, but the participant writer `update_participant` (cursor_state.rs ~:739) never emits the `SEEN_SET_WARN` warning; only the dead `consume_inner` does. Either the warning is lost or the contract text is stale.
- Mail catchup reports unreadable pending mail only on stderr while channels report it in stdout `skipped` (nit, C10 vs C7).
- `search.rs` `preview()` duplicates `watch::sanitize_preview` (fullwidth bracket, 160 cap); drift risk, no shared test.
- Catchup Full and Compact headers (`render_framing`) are essentially untested (compact only asserts "catchup partial"); no test pins participant v2 cursors.json bytes; the live writer's refusal on a symlinked/malformed cursors.json is untested.

## Open questions (need a run)

1. How loose are the caps in B7, and is the slack in B6 really about 2 bytes?
2. Is the tree-snapshot repair for B9 stable (no timestamp or heartbeat files changing)?
3. Are room-level `<room>/cursors.json` and `.cursors.lock` never written by any path (static grep says yes; a run would confirm)?
4. Where does the stderr "warning" in C6 originate (probably `effective_channels`)?
