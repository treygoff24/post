# Proof: read-search (area 2)

Baseline (unmutated, unit `post-area-read`): tests/byte_budget.rs 20/20 before edits, then 19/19 after (B10 deleted); tests/catchup.rs 12/12 (13 before, C8 folded into C7), tests/consuming.rs 3/3, tests/search.rs 6/6, lib cursor_state 13, commands::read 1, commands::search 4, commands::byte_budget 2. Each mutation was applied by a script that asserts the pattern occurs exactly once, runs the named tests, and restores the file from a byte copy (`cmp` verified; `git diff` on the mutated file shows only the intended edits).

| # | Mutation | Test expected red | Result |
| --- | --- | --- | --- |
| M1 | catchup.rs after-stdout: drop `consume_mail` | byte_budget B5, catchup C10, C7 | RED (3) |
| M2 | read.rs direct read: drop the after-stdout mail consume | byte_budget B17; cli X6 | RED both |
| M3 | read.rs exact ack: consume before stdout | byte_budget B19 (mail ack) | RED |
| M3b | read.rs direct read: consume before stdout | byte_budget B19 (mail read) | RED |
| M3c | read.rs exact ack: drop the consume | byte_budget B3 | RED |
| M4 | byte_budget.rs admit_prefix_measured: `<=` -> `<` | B6 (exact full size), B7 (three modes) | RED (both) |
| M5 | ParticipantCursors::load Missing arm writes a lock file | fenced_auto_text_stays_quiet_and_writes_nothing (B9) | RED |
| M6 | catchup empty text gets a trailing space | catchup C3 | RED |
| M7 | catchup nonempty text prints `AI AGENT CATCHUP` | catchup C4 | RED |
| M8 | catchup null-sink branch consumes the channel before refusing | catchup C9 | RED |
| M9 | collect_channel scans strict instead of tolerant | catchup C7 | RED |
| M9b | positional channel branch clears `skipped` | C7 (positional half) | RED |
| M10 | Framing::default carries LAW_DATA | cli X1 | RED |
| M11 | compact read text loses the law / header | cli X2 | RED both |
| M12 | full read header uses raw `from` / two-space header / body altered | read.rs consolidated unit | RED (3) |
| M13 | JsonArrayPrefix::new serializes each item twice | byte_budget U23 | RED |
| M14 | serialize_participant_cursor drops the trailing newline | U4 (v2 exact bytes) | RED |
| M15 | Snapshot::load Missing arm skips the legacy import | legacy v1 and v2 units | RED (2) |
| M16a | update_participant: Invalid state becomes default | participant writer refusal unit (malformed row) | RED |
| M16b | update_participant: skip destination guard and Invalid refusal | same unit (symlink row) | RED |
| M17 | update_participant Missing arm inherits legacy-style seen ids | v2 "does not inherit" unit | RED |
| M18 | cursor lock created 0666 and the 0600 restore disabled | cli X16 | RED |
| M19 | consume_channel_through writes the target before the refusal | cli X15 (participant cursor untouched) | RED |
| M20 | search fold_case removed | U26 (both units) | RED |
| M20b | search literal pattern drops '.' | metacharacter unit | RED |
| M21 | preview: `[` not defanged | U27 | RED |
| M21b | preview: cap `<=` -> `<` | U27 exact-cap row | RED |
| M22 | ParticipantCursors::channel_has_seen treats ids below the max as seen | consuming K4 and the paged read test | RED (2) |
| M23 | output::sender_label_quoted drops the profile | output.rs watch line tests (replace the deleted inbox units) | RED (2) |

## Findings and limits

- The symlink row of the participant-writer refusal test cannot be isolated: `ensure_cursor_destination_safe` and the Invalid-state refusal in `update_participant` both refuse a symlink, so each alone leaves the row green. M16b removes both.
- X15's participant-cursor assertion is redundant with the exit-code and error-code assertions before it in every realistic regression (a failing closure never writes). M19 is a synthetic partial write and only that assertion catches it.
- M12 body-row and M17 are synthetic mutations for the exact regression they name (body altered in render; participant write inherits legacy ids); they show the assertion can bind, not that production has that fault.
- Deleted tests and their stronger proof: U19 -> C3 (M6), U20 -> C4 (M7), U17 -> X2 (M11), U24/U25 -> output.rs watch line tests (M23), U10 -> K4 (M22), U14 lock mode -> X16 (M18), K1 -> X6 (M2), C8 -> C7 (M9/M9b). U1, U8, U14 exercised only the deleted room writer.
- Nothing failed on the baseline.
