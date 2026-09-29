# Proof: watch (test-audit-e)

Each mutation was applied to production code, the named test run through testrun, then the source restored byte for byte (cmp against a saved copy) and re-run green.

| Repair / consolidation | Mutation | Result |
|---|---|---|
| `snapshot_does_not_leave_a_live_heartbeat` (F, shared with doctor W5) | `watch --snapshot` touches every target participant's heartbeat | red (participant row live / heartbeat exists) |
| same | `read_presence` absent-file branch returns `live_watch: true` (the missing-heartbeat-is-live regression) | red (`tests/cli.rs` who-row live_watch assertion). Done BEFORE deleting participants `missing_heartbeat_is_not_live`. |
| `event_wake_drives_the_loop_and_once_exits_after_emitting` (F) | remove `touch_participant_heartbeat` in `touch_admitted_heartbeats_with` | red |
| `a_vanished_record_with_no_claim_stops_the_watch_as_unbound` (F) | `Resolved::Unbound` arm in `follow_identity` returns `Ok(false)` | red |
| Deleted `scan_batch_never_rings_for_the_rooms_own_messages` (D/C) | drop `parsed.message.from == room` in the own-room filter | keeper `scan_batch_suppresses_declared_owned_rooms_but_not_merely_watched_ones` red |
| Retargeted presence writer tests (5) | remove O_NOFOLLOW from write open | `heartbeat_write_does_not_follow_symlink` red |
| same | nlink check disabled | `heartbeat_write_does_not_clobber_through_hard_link` red |
| same | remove `set_permissions(0o600)` | `existing_heartbeat_perms_normalized_to_0600` red |
| same | (reader) drop ELOOP arm | `symlink_heartbeat_reads_as_dead` red |

Not mutated: the FIFO write test. With O_NONBLOCK a write-open on a reader-less FIFO fails with ENXIO before the is_file check, so removing the is_file guard cannot go red; removing O_NONBLOCK would hang the test instead of failing it. It is kept as a no-hang check only.

Unproven: real `participant gc` collection stopping a live watch has no test in this area (the vanished-record test states honestly that it covers the unbound branch only).
Total: 9 mutations, 9 caught.
