# Proof: participants (test-audit-e)

| Item | Mutation | Result |
|---|---|---|
| `oversized_heartbeat_reads_as_dead` (F) | remove the `metadata.len() > MAX_HEARTBEAT_BYTES` rejection | red |
| `stale_heartbeat_is_not_live` (C into giant-interval) | remove the interval range filter in `parse_heartbeat` | `forged_giant_interval_does_not_pin_liveness` red |
| `missing_heartbeat_is_not_live` (D) | absent-file branch reads live | red in `snapshot_does_not_leave_a_live_heartbeat` (see proof-watch.md) |
| 3 nearest-harness tests (F) | walker returns None after first non-harness | codex + claude tests red |
| same | force every command to "codex" | claude-pid, ambiguous, recognized-nearest red |
| same | drop claude_pid | claude-pid test red |
| `participant_round4_unbound_streams...` (F) | disable the bare-marker fallback | red |
| same | disable the too-small refusal | red |
| `..._existing_colon_and_reserved_rooms...` (F) | disable the reserved-name check | red |
| same | disable the colon check | red |
| `..._workspace_less_actor...` (F) | `sender` falls back to a cwd room name | red |
| version build SHA carry (C) | `build_sha()` returns "" | `participant_review_version_is_pure...` red |
| no-key diagnostic carry (C) | drop the export text from the message | `participant_keyless_send...` red |
| same | add an exact_fix to the no-key error | red |
| matching-cwd carry (C) | provenance downgraded when cwd ends in beta | `participant_round2_binding_provenance_wins_when_cwd_differs` red |
| unregistered-cwd chat + hostile (C+F) | text marker echoes the cwd | red |
| same | text marker empty | red |
| plain-rebind (C) | every rebind resets workspace from cwd | `participant_lifecycle_touch_end_and_bind_reactivation` red |
| dangling-index (C) | vacant slot mints a pid-suffixed id | `participant_missing_dangling_session_index...` red |
| concurrent-bind record count (C into collision race) | bind writes a stray extra record | `participant_round2_collision_race_preserves_both_keys` red |
| `address_inbox_is_a_pure_path_accessor` (D) | lineage inbox path changed | `participant_typed_targets_write_canonical_store...` red |
| same | participant inbox path changed | 2 tests red |
| `participant_old_format_envelope_still_parses` (D) | parse path rejects mail without from_participant | `old_mail_renders_unknown_origin...` red |
| same, first attempt | drop `#[serde(default)]` on `from_participant` | stayed GREEN: serde defaults a missing `Option` field regardless, so this cannot bind. Replaced by the parse-path mutation above. |

Total: 22 mutations that can bind, 22 caught, plus the serde-attribute one that cannot bind.
Unproven: the `.participants.lock` acquisition in `bind` has no test that holds the lock while starting a bind; the collision race can catch an unlucky interleaving but a green run does not prove the lock. Left as reported by the review.
