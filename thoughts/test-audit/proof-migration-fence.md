# Proof: migration-fence (branch test-audit-b)

Landed: T4 snapshot test repaired (full-tree byte comparison, real participant paths, pre-seeded aged heartbeat); T3 gained a `missing-state-and-lock` row (absorbs M12) with a legacy-send control, and its duplicate-key row now carries `generation: 7`; matrix M3 (moved to tests/catchup.rs participant cursor path), M12, M5b search branch and schema parse, M6 framing/banner, M8c malformed-generation rows and read, M13b ambiguous row removed; T7 read half removed; U4 plus `conservative_read_mode`/`read_only_must_not_mutate` deleted.

| Mutation (production) | Test that went red | Result |
| --- | --- | --- |
| `admit_generation`: rootful store, generation declared, no state -> admit | T3 `migration_fence_read_only_states_stay_available...` (missing-state-and-lock row, replaces M12) | red; restored |
| `classify_write`: Catchup no longer a writer | tests/catchup.rs `fenced_catchup_refuses_before_cursor_or_room_mutation` (replaces M3) | red |
| `classify_write`: Search a writer | matrix `migration_fence_cli_matrix...` (search admitted on fenced store) | red |
| state visitor: duplicate `state` key ignored | T3 duplicate row, first with the old fixture (no generation): GREEN, so it could not bind; repaired fixture adds `generation: 7`, then red | finding, fixed |
| `current_generation`: accept `0` | T7 `migration_fence_empty_and_malformed_generation...` | red |
| `mailbox_dirs` creates the inbox | `mailbox::tests::mailbox_dirs_names_paths_and_creates_nothing` (U4 keeper) red; CLI fence tests stay green, so the unit keeper is the binding one | red |
| watch snapshot refreshes heartbeats (mint case) | T4 repaired | red |
| watch snapshot refreshes an existing heartbeat only | T4 repaired, byte comparison of the pre-seeded heartbeat | red (bytes; mtime assertion not isolated) |

All sources restored byte for byte (cmp against the pre-mutation copy; `git diff` on watch.rs empty).
Not mutation-proved: M6 replacement (byte_budget owns framing/stamp), T7 read half removal (T2 covers rootless readers), M5b (search.rs keeper), T5/M10 timing. Held and untouched: U2 `transitions_are_locked_and_illegal_transitions_refuse`, `fence`, `activate`, `write_state_locked`, the `writes: bool` argument, watch.rs `is_enrolled()`.
