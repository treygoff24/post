# Proof: rooms

Each mutation is applied to production code, the named test run, then the source restored byte for byte (cmp against backup).
An earlier run of the reserved-name mutations left one mutant in place by mistake (a piped run killed the restore); it was found by `git diff`, restored, and every reserved-name mutation was rerun clean. The results below are from the clean reruns.

| # | Repair | Mutation | Result |
|---|---|---|---|
| 1 | I8 mailbox unit + CLI | `validate_new_room_name` stops consulting RESERVED_ROOM_NAMES | unit red, CLI red |
| 2 | I8 anchor `participants` | entry renamed in the constant | unit GREEN (as predicted), CLI anchor red |
| 3 | I8 anchor `.rename.lock` | RENAME_LOCK_FILE entry renamed | unit GREEN, CLI anchor red |
| 4 | I8 anchor `rename-journal.json` | RENAME_JOURNAL_FILE entry renamed | unit GREEN, CLI anchor red |
| 5 | I20 reserved rename row | rename skips `validate_new_room_name` | red |
| 6 | I5 table, both rows | `normalize_path` collapses `..` past symlinks | red |
| 7 | I5 row 2 alone (rows temporarily swapped) | same | red on the `a` fixture (`duplicate_workspace` at `.../a`), so the second fixture binds independently |
| 8 | I10 exact-case row | duplicate check ignores exact-case matches | red |
| 9 | I10 absorbing I15 | add-site set-path hint reworded | red |
| 10 | U7 removal (I12/I13/I20) | bridge-host fallback dropped in `suffixed_room_candidate` | 3 CLI tests red |
| 11 | U8 removal (I12) | `is_free` ignored | I12 red |
| 12 | I16 | `plan_routing_receipts` skipped in rename | red |

Not independently proven: the two assertions added to I16 (`unread_count == 1`, no "corrupt routing receipt" warning). Mutation 12 is caught by an earlier I16 assertion first, and I found no small mutation that trips only the new ones.
I29 (deleted): fixture confirmed to build no `bridge/`; I16 and I36 stay green.
Not done: optional I38 warning sharpening. Done: optional `learned_host_suffix` `#[cfg(test)]` wrapper removed (U2-U6 use `learned_host_suffixes(...).first()`).
Left for the send lane: I43 was renamed to `send_to_a_mistyped_room_has_a_did_you_mean_and_a_discovery_hint`, ownership not moved. I44 comment fixed.
