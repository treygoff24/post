# Proof: owner-signing (branch test-audit-b)

Edits: 5 D (T4 `a0a_f4_...`, T8 `a0a_f8_...`, U3 `legacy_uses_the_registered_rooms_resolved_path_...`, U10 owner_tests `exclusive_atomic_write_never_replaces_an_existing_destination`, U11 `read_owner_file_rejects_symlink_and_legacy_matches_registered_path`); 5 F/C repairs (T7 vacuous race stanza removed, T9 alpha stanza removed and label table cut to one row, T10 table cut to newline + bidi rows, T22 signed manifest plus a verifying control plus new unit `v2_locator_grammar_rejects_empty_and_path_like_tags`, T23 over-cap fixture signed, T26 manifest signed for storage channel plus control and rewrite-took-effect assertion). Kept `mailbox::tests::exclusive_atomic_write_never_replaces_an_existing_mail_file`.

| Mutation (production) | Test that went red | Result |
| --- | --- | --- |
| mailbox `exclusive_atomic_write_with`: drop `remove_file(&temporary)` on error | mailbox::tests::exclusive_atomic_write_never_replaces_an_existing_mail_file (keeper for U10) | red; restored, green |
| `parse_v2_locator`: `version != 2` -> `false` | T22 v2_malformed_owner_locators... | red |
| `parse_v2_locator`: `len() != 2` -> `len() < 2` | T22 | red |
| `parse_v2_locator`: version read as f64 truncated to i64 (accepts 2.5) | T22 | red |
| `parse_v2_locator`: drop `tag.is_empty()` | new unit v2_locator_grammar_rejects_empty_and_path_like_tags | red |
| `parse_v2_locator`: tag grammar check -> `false` | same unit | red |
| `signed_status_v2`: read-side cap -> `false` | T23 v2_signed_cap... | red |
| `signed_status_v2`: envelope-channel check -> `false` | T26 v2_envelope_channel_differing... | red |
| `OwnerFile` `deny_unknown_fields` removed (T4 replacement) | U2 resolution_rejects_unregistered_room_and_unknown_fields | red |
| `expand_room_path`: `~/x` returns literal path (T8, U3, U11 replacement) | U1 resolution_states_configured_legacy_none | red |
| `validate_owner_values`: drop `validate_marker` (T10 consolidation) | T10 a0a_f10 | red |
| `validate_owner_values`: drop `validate_label` alone | T9 CLI row stays GREEN (the derived-label check in `resolve_owner` also refuses the label); U9 `validate_owner_values_checks_every_explicit_field` goes red | finding |
| both label validators dropped | T9 red | binds only when every label guard is gone |

All sources restored byte for byte after each mutation (checked by cmp against the pre-mutation copy).
Finding: T9's single hostile-label row proves that `owner init` runs some label validator, not which one; U9 owns the explicit-field guard.
Not run: T7's 0555 refusal as root (runner is non-root here).
