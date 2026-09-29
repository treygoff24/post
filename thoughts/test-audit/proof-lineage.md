# Proof: lineage

Baseline via testrun unit `post-area-lineage`: tests/lineage.rs 27/27 (was 29; L18 and L27 removed) after edits. Restored byte for byte (cmp against backup; `git diff` on src/lineage_store.rs empty).

| Edit | Mutation | Result |
| --- | --- | --- |
| L5 repaired (torn-tail stanza removed, renamed lineage_voice_content_rules_are_enforced) | src/lineage_store.rs `VOICE_MAX_BYTES` 4096 -> 4200 | RED: lineage_voice_content_rules_are_enforced (plus the terms test) |
| L18 folded into L19 (lineage_unaffiliated_settled_gap_retry_is_idempotent now: new, add, withdraw, re-add, leave, withdraw, retry) | :590 `checked_add(1)` -> `checked_add(0)` (gap count never increments) | RED: L19 among 6 failures |
| L27 removed, L28 kept | create(): hoist `validate_name` above `participant::lock` | STAYS GREEN. Does not bind |

## Finding: L28 cannot see create's own lock ordering

Sol's review said hoisting validation before the lock in `create` would fail L28. It does not. Every write command takes the participants lock in `participant::touch` (src/commands/mod.rs:188-190) before dispatch, so the spawned `identity new` blocks there, before `create` runs at all. L28 therefore proves "validation happens after the lock is obtained by the command", not "inside create". L27 had the same limit, so removing it lost nothing observable; the consolidation stands. Bind test for create's own ordering would need a unit test of `lineage_store::create` (not written; out of the approved edit list).

Not done: item 5 (optional dead_code attribute cleanup in src/lineage.rs) and held items 4 (read_history, U3, docs).
