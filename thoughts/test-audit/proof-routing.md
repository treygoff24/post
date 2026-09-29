# Proof: routing

Every mutation applied to production code, test run, then source restored byte for byte (cmp against backup).

| # | Repair | Mutation | Result |
|---|---|---|---|
| 1 | A5 crossed send (routing.rs) | `crossed_report` scans `visible_channel_with` (all history) instead of `unread_channel_with` (b's seen set) in src/channel.rs | red |
| 2 | B13 frozen sentences (cli.rs) | read.rs full/compact renderer stops pushing the `Sender evidence:` line | red |
| 3 | B15 address line (cli.rs) | read.rs address wording "(self-declared instance tag, opaque and non-routable)" -> "(declared)" | red |
| 4 | A23 absorbing B19 | send.rs receipt readback command unquoted (`post read <id>`) | red |
| 5 | A23 absorbing B19 | send.rs "sender is not a frozen recipient" sentence changed | red |
| 6 | A44 absorbing CT1 | inbox JSON `count` field emitted as `count + 1` | red |

Notes: mutation 1 first failed to compile (wrong path); retried with `crate::cursor_state::eligibility::visible_channel_with`. CT1's `count` fold does not discriminate file subtraction from per-id eligibility (as the review warned); the old divergent fixture was not restored.
Held: B14 text half (provenance framing contract, CONTRACT.md:1306-1314 vs read.rs), left unchanged.
Deleted dead code: `eligibility::visible_channel` and `archived_channel` (no callers).

## inbox_counts_are_eligible_ids_not_raw_files_minus_seen (restoration of the deleted counts.rs divergence case)
Fixture (real CLI plus plain filesystem edits, no seam): two letters to workspace:alpha, participant reads the first, the consumed file is removed from alpha/inbox (seen id without a file), then rewritten (failed-unlink duplicate). Asserts count and unread_count at each step.
Mutation: inbox list_bound computes count (and so unread_count) as raw .mail files in the inbox minus the participant's seen-set size for that address. Result: RED at the post-removal step (count 0, expected 1). Restored byte for byte (git diff on src empty); routing binary 63/63 green.
