# stranger — self-echo-union

_lane: stranger:omp:glm · alias omp-5 · exit 0 · 3767 bytes · review-shaped: yes_

=== completionReport ===
Now let me analyze the commit. Based on my investigation, here's my review:

## VERDICT: legible | 3 stumbles that matter

The commit fixes a legitimate issue where `post watch` watching multiple rooms would ring for its own sends, creating a false doorbell. The fix threads the union of watched rooms into `scan_batch` and suppresses when the sender is any room the watch covers. However, there are three significant issues:

### [STUMBLE] src/commands/watch.rs:798 · The suppression logic assumes "watching a room means being that room"
Read: `Ok(parsed) if watched_rooms.contains(&parsed.message.from) => {}`
Cost: This breaks legitimate observers that watch rooms they don't own (bridges, relays, monitoring processes). Such processes would suppress traffic between rooms they watch and could go deaf without error.
Fix: Add an explicit ownership control rather than assuming that every watched room belongs to the watcher.

### [STUMBLE] tests/cli.rs:2815-2870 · The rewritten test no longer validates the core dedup property
Read: The test now sends the shared message from a third room (gamma) instead of from one of the watched rooms (alpha), removing the test for whether dedup works when a watched room sends to a shared channel.
Cost: The test passes even if dedup is entirely broken for the "own voice" case it was supposed to validate.
Fix: Add a separate test that specifically validates dedup when a watched room sends to a shared channel.

### [STUMBLE] src/commands/schema.rs:95 · The schema description doesn't mention the union suppression behavior
Read: The watch command schema describes deduplication but doesn't state that suppression now applies to any room in the watch union.
Cost: The machine-readable contract is incomplete - callers can't know the behavior changed from "suppress own room only" to "suppress any watched room".
Fix: Update the schema to explicitly state: "suppresses messages from any room being watched".

## Questions (not defects):

1. **Snapshot vs long-watch union building**: The schema change mentions snapshot behavior, but the union is built from assembled targets in both paths. Rooms that are requested but skipped (unregistered under --snapshot) are excluded from the union. This seems correct - a requested-but-skipped room shouldn't count as "self" when it's not actually being watched.

2. **Union consistency**: The snapshot path and long-watch path build the union from the same `targets.iter().map(|t| t.room.clone()).collect()`, so they cannot disagree.

The core change is sound for the common case (agents watching their own identities), but the assumption that watching implies ownership creates a regression for legitimate multi-room observers. The test rewrite accidentally removed coverage of the dedup property for own-voice suppression, and the schema contract is incomplete.

## Completion Report

**Status**: completed

**What I found**: Reviewed commit 6cf6f40a047a4bd246f7983ffef9732c0b440e02 which fixes `post watch` self-echo by suppressing messages from any room in the watch union. The change is correct for the primary use case but has three issues: it breaks legitimate observers watching rooms they don't own, the rewritten test no longer validates the core dedup property, and the schema doesn't document the new behavior.

**Files reviewed**: 
- `src/commands/watch.rs` (core implementation)
- `src/commands/schema.rs` (schema documentation)
- `tests/cli.rs` (test coverage)

**Verification**: Analyzed the code paths and test logic to understand the change and its implications.

**Remaining risks**: 
- Multi-room observers (bridges, relays, monitors) may silently lose messages
- Test coverage gap for own-voice deduplication
- Incomplete machine contract in schema
