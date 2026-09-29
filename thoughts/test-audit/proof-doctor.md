# Proof: doctor / who edit (branch test-audit-f)

Each mutation was applied to production code, the named test run through testrun, then the file restored (git diff on it empty afterwards, confirmed by the harness each time).

| Mutation | Test | Result |
| --- | --- | --- |
| D1 doctor.rs: channel_state.<room>.invalid severity always Warning | schema_surface doctor_reports_legacy_state... | RED at the new no-cursor Error assertion (schema_surface.rs:1256) |
| D1b same severity always Error | same | RED at the existing cursor-present Warning assertion (:1283) |
| D2 apply_fixes creates `<room>/read` for registered rooms | cli doctor_is_read_only_without_fix... | RED at the carried `read/` absence assertion (cli.rs:3567) |
| D2b apply_fixes creates `<room>/inbox` | same | RED at the inbox assertion (cli.rs:3566) |
| D3 apply_fixes writes invalid default rooms.json ("[]") | same | RED, but at the earlier fix-report assertion (cli.rs:3534), not the new plain-doctor one |
| D3b apply_fixes writes invalid default rules.json ("[") | same | RED at :3534 as D3 |
| W1 watch.rs: touch_participant_heartbeat(participant, 1000) instead of interval_ms | cli who_reports_live_for_ten_second_interval_watch | RED at the new stamp-interval assertion (cli.rs:9189) |
| C1 chat.rs seen_by marks the queried id seen for the caller | cli seen_by_lists_members_past_a_message_read_only | RED at the final unread==1 assertion (cli.rs:8817) |
| G1 chat.rs filter_grep drops case_insensitive | cli history_grep_filters_case_insensitive_regex | RED (cli.rs:8850) |
| G2 chat.rs filter_grep escapes the pattern (literal substring) | same | RED (cli.rs:8850) |

Result: 9 of 9 mutations caught by the test the repair names (D3/D3b are extra probes, see below).

Did not bind independently: the plain `doctor` run added right after the first fresh-root `--fix` (fold of `doctor_fix_then_doctor_is_healthy_on_a_fresh_root`). `--fix` builds its report by re-running detection on the fixed disk (doctor.rs `run`), so any defect a `--fix` leaves on disk is already caught by the `--fix` report assertions; no credible mutation makes the plain run fail while the fix report passes. The assertion is kept because CONTRACT.md documents the two-command bootstrap, but it is redundant with the fix-report assertions today.

Ten-second-interval test: the artificial `1 10000` stale-stamp stanza was removed as the review allows; the no-presence-after-exit case is not asserted here any more.

`snapshot_does_not_leave_a_live_heartbeat` belongs to the watch lane and was not touched.
