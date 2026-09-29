# Proof: bridge protocol, Rust side (branch test-audit-i)

Each mutation was applied to production code, the named tests run through testrun, then the file restored from a byte copy (`git diff` on it empty).

## Test edits (commit bcccb61)

| Repair | Mutation | Result |
| --- | --- | --- |
| D2 replay test: exact routing receipt + readable letter | bridge.rs: skip `route_admitted_import_locked` when `*replay` | RED: `a_crash_after_d2_...` panics at the new receipt read (bridge_deliver.rs:813); also `a_failed_routing_receipt_...`, `a_replay_after_a_blocking_rule_...`. The old restricted-diff assertion alone stayed green. Restored, green 38/38. |
| Outbox absence folded into the remote-send test (typed_letters test deleted) | send.rs: `create_dir_all(root/outbox)` on a remote send | RED: only `a_remote_send_writes_only_the_archive_...` ("post never writes outbox/"). Restored, green 26/26. |
| Host-qualified no-fallback inbox path now `root/pact/inbox` | send.rs: write `root/pact/inbox/stray.mail` on a remote send | RED: the host-qualified test and others. Caveat: `assert_queued`'s whole-tree snapshot trips first, so the repaired final check is a redundant anchor, as the review said; it did not fail independently. |
| Removed row dropped, workspace-mail D deleted | none (deleted duplicates; stronger tests retained) | n/a |

Mutations caught 3/3 (one with the caveat).

## Product fixes (red first)

- S1 un-archive (commit e3563b5): `a_blocked_route_letter_leaves_an_archived_recipient_archived` (bridge_deliver.rs). On the unfixed code: outcome `rejected/blocked_route` and `replay:false` assertions passed, then RED at "a refused letter restored the archived recipient". After the fix (checks read the record via `participant::peek_locked`, revive last): green. bridge_deliver 39/39, participant_gc 17/17, participants 58/58, participant_auto_gc 10/10, routing 63/63.
- S2 (commit 334c761): `bridge_topology::tests::relay_status_names_why_the_config_is_unusable`. Unfixed: "no bridge config" assertion passed, RED when `bridge/config.json` is a directory (still "no bridge config"). Fixed: green 3/3 in the module.
- S3 (commit f69fb0c): comment move only; clippy --all-targets clean.

## Held / not done
Nothing held in this area. No tombstone-case regression for S1 was added (peek handles it; existing delete/replay tests still pass).
