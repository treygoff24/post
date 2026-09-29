# Independent layer review: owner signing and verified badge

Read-only review of the 41 declarations in `ledger-owner-signing.md` against the tests, production callers, and `CONTRACT.md:1203-1265`. No tests, Cargo, builds, pytest, or Node ran. Proposed marks stand: **30 R, 5 F, 1 C, 5 D**. These are source-based edit recommendations, not passing-test claims. No file retires and no production seam is unlocked.

## Disagreements and corrections per test

**No mark disagreement.** The following corrections make the edits and remaining proof precise:

| Test (ledger mark -> review mark) | Evidence and correction |
| --- | --- |
| `a0a_f4_malformed_owner_fails_closed_badge_paths_transport_unaffected` (D -> D) | Its unknown-field owner file reaches `read_owner_file` (`src/mailbox.rs:1310-1360`); U2 checks that parse failure (`:2177-2191`). T11 checks the same badge/transport command matrix with truncated JSON (`tests/cli.rs:10567-10745`). Keep T11 and U2 before deleting T4 (`:10044-10105`). This loses only the second malformed-file fixture, not an independently guarded branch. |
| `a0a_f7_owner_init_create_only_and_failed_install_recovery` (F -> F) | Remove only the pre-planted “Adversarial commit race” stanza (`tests/cli.rs:10336-10345`): `owner init` sees an existing destination in `src/commands/owner.rs:51-62` and never reaches its commit at `:82`. U13 plants after the temporary file is written (`src/commands/owner.rs:344-381`). Preserve T7's mode, retry, refusal, and failed-install assertions. The 0555 permission row may behave differently as root; runtime unverified. |
| `a0a_f8_raw_tilde_registry_derives_absolute_sidecar` (D -> D) | U1 asserts the exact configured absolute sidecar from `~/.mara-room` (`src/mailbox.rs:2124-2162`), through the same `load_owner`/`resolve_owner_file` path as `owner show` (`src/mailbox.rs:1235-1252,1412-1473`). T8 (`tests/cli.rs:10355-10380`) adds a CLI environment wiring check, but other CLI tests use home-relative registered rooms; it is not a separate owner contract. |
| `a0a_f9_immutable_room_id_renders_under_every_label_and_hostile_labels_rejected` (F -> F) | Delete the alpha send/history stanza (`tests/cli.rs:10429-10440`): the earlier verified Mara message already satisfies its `contains` assertion (`:10393-10412`). Keep the independent `Oracle (mara)` verified render (`:10442-10469`). Retain one hostile-label CLI row to prove `init` invokes validation; U8 owns the predicate's three cases (`src/mailbox.rs:2340-2357`). |
| `a0a_f10_hostile_markers_rejected_and_wire_stays_unambiguous` (C -> C) | Keep the test, its newline parser row, one bidi validator/writes-nothing row, and the custom whale marker's verified and wrong-prefix assertions (`tests/cli.rs:10475-10565`). Remove the remaining duplicate refusal rows only after U7 remains (`src/mailbox.rs:2316-2338`). This is a table consolidation, not a whole-test deletion. |
| `v2_malformed_owner_locators_fail_loudly_and_non_owner_locators_are_inert` (F -> F) | Sign a valid manifest for tag `20260812T210800Z`, channel `malformed`, body `body` before the table (`tests/cli.rs:11489-11533`). Then the unknown-version, extra-key, and float-version rows can turn verified if their respective parser guard is removed. Missing-tag, null, and non-object still prove loud failure versus unsigned; the non-owner rows prove inertness. Add direct `parse_v2_locator` unit rows in `src/mailbox.rs` for empty tag and `../escape`, with a valid-locator control: no valid sidecar can make those malformed path names reach a matching ordinary manifest. The parser is private at `src/mailbox.rs:1621-1644`; no production export is needed. |
| `v2_signed_cap_enforced_at_send_and_read_while_unsigned_oversize_is_unchanged` (F -> F) | Before writing the over-cap stored fixture, `v2_sign(&sandbox, "20260812T211200Z", "cap", &over)` (`tests/cli.rs:11574-11647`). Without it, removal of the read cap still fails on missing sidecar. With it, the read cap at `src/mailbox.rs:1717-1719` is the decisive guard; the existing exactly-at-cap positive case remains. |
| `v2_envelope_channel_differing_from_storage_directory_fails` (F -> F) | Sign the manifest for **storage** channel `bind-b`, then rewrite only the stored envelope to `bind-a` (`tests/cli.rs:11763-11800`). `signed_status_v2` reconstructs from `storage_channel` (`src/mailbox.rs:1738-1743`), so the current `bind-a` signature also fails manifest equality and does not bind the envelope check at `:1720-1724`. Inspect the rewritten JSON in the fixture, or assert the replacement changed bytes, before expecting the negative badge. |
| `exclusive_atomic_write_never_replaces_an_existing_destination` (D -> D) | The same production primitive is tested with a mail destination plus no-temp-leak assertion at `src/mailbox.rs:1877-1897`; U13 handles the owner-specific mid-commit race. The fresh-write half of this owner test (`:2440-2445`) is also exercised by real owner init T7. No owner-only failure is lost. |
| `read_owner_file_rejects_symlink_and_legacy_matches_registered_path` (D -> D) | The body (`src/mailbox.rs:2451-2475`) never creates a symlink. U4 actually rejects one (`:2214-2225`); U1 owns legacy resolution (`:2124-2175`); U6 owns explicit `/srv/...` values (`:2256-2313`). Its supplied `RoomMap` is read from disk, so it cannot prove use of a registry different from disk. `load_owner_with_rooms` remains production-used by profile and doctor (`src/commands/profile.rs:106`, `src/commands/doctor.rs:320-325`). |

All other R declarations have distinct asserted failure points or user-visible boundaries. In particular T12's real signature protects against a multiline v1 wire inheriting a badge (`tests/cli.rs:10746-10808`), beyond the no-crypto unit at `src/commands/chat.rs:3148`; T14's owner-addressed crossed receipt uses a different address branch from the mention case in `tests/channels_just_work.rs:406`; and T18 reads v1 and v2 as separate messages in one CLI flow (`tests/cli.rs:11288-11338`). The v2 manifest negatives T19-T21 each bind a different field or exact format (`:11340-11488`), despite sharing the comparison at `src/mailbox.rs:1740-1745`.

## Keeper per contract

| Contract | Keeper |
| --- | --- |
| Configured, legacy, absent resolution and derived defaults | U1 `resolution_states_configured_legacy_none`; U2 rejects unregistered/unknown fields; U6 and U9 own explicit values and validation. |
| Owner file safety | U4 symlink, U5 size bound, T16 bounded FIFO refusal, U12 held-descriptor swap. |
| Owner init creation, mode, idempotence, recovery and race | T7 `a0a_f7_owner_init_create_only_and_failed_install_recovery`; U13 deterministic commit race; U14 observable `missing` and `note`. |
| Marker and label rules, CLI wiring | U7 marker predicate, U8 label predicate; T10's two refusal rows and T9's one label row cover `owner init`; T10 keeps custom-marker verification. |
| Bad owner failure matrix versus transport | T11 `a0a_f11_command_matrix_rows_and_crossed_send_draft_preserved`; U2 owns unknown-field rejection. |
| Legacy output and feature absent behavior | T2 `a0a_f2_legacy_fallback_resolves_trey_byte_identical`; T3 `a0a_f3_feature_absent_signed_looking_text_unbadged`. |
| Imitation and doctor collision | T5 `a0a_f5_imitation_tracks_configured_owner_and_doctor_flags_collisions`; profile predicate units at `src/profile.rs:329-363`. |
| V1 real signing, non-owner no badge, bytes/replay, one-line rule | T1, T6, T12 respectively; T9 owns configured label render under a non-default label. |
| Crossed receipt status, bytes, malformed-owner warning | T13 status tri-state, T14 owner-addressed body bytes, T11 warning; mention-addressed twin is `tests/channels_just_work.rs:406`. |
| Doctor keygen probe and FIFO no-hang | T15 `a0a_r2_doctor_keygen_probe_never_executes_ssh_keygen`; T16 `a0a_r2_fifo_owner_json_fails_fast_not_hung`. |
| V2 body inertness, slices, coexistence, manifest fields, fidelity | T17, T18, T19-T21, T27 respectively. |
| V2 locator grammar, cap, CLI flag, v1 no-fallback, envelope binding | T22 after repair plus direct parser rows; T23 after repair; T24; T25; T26 after repair. |

## Final edit list, in implementation order

1. Keep the named owner and sibling keepers. Delete T4 and T8 from `tests/cli.rs`, and U3, U10, U11 from `src/mailbox.rs`. U3 is `legacy_uses_the_registered_rooms_resolved_path_not_the_raw_string`; U10/U11 are the two unit declarations named above. No production code deletion follows.
2. Repair T7 and T9 by removing their vacuous stanzas; trim T9's hostile-label table to one CLI wiring row. Consolidate T10's refusal table to newline plus one validator row, keeping the real-signature half.
3. Repair T22 with a signed valid-tag sidecar and add direct empty/path-like tag parser assertions; repair T23 with a signed over-cap fixture; repair T26 so the manifest matches storage while the envelope differs, and assert the fixture rewrite occurred.
4. Run focused keepers and deliberate guard-removal/red checks **later in an authorized test lane**. This review ran none. Keep the v2 manifest's independent fixture format; do not replace it with the production formatter.

Retired files: **none**. No support hook or production helper becomes dead. Do not fold unrelated Cargo dependency cleanup or the 2,000-line test-block move into this area cutover.

## Maintainer-decision items

The edits above remove or change tests only; they remove **no production code** and alter **no documented contract**. The owner trust anchor, v1 compatibility, and v2 rules are documented in `CONTRACT.md:1203-1265`; changing any of those behaviors or deleting their production path requires the maintainer's decision. In particular, removing `set_pre_commit_hook` or `set_post_open_hook` would discard the only deterministic race proof (U13 and U12) and is **not** recommended. A separate proposal to change `Cargo.toml` dev dependencies is outside this review and needs its own build verification.

## Suspected bugs and test-only seam verdicts

No product bug was confirmed by this source review. The T22/T23/T26 negatives are **test gaps**, not evidence that production accepts malformed locators, oversized bodies, or mismatched channels: `parse_v2_locator` rejects them at `src/mailbox.rs:1621-1644`, and `signed_status_v2` rejects the cap/envelope at `:1717-1724`. A dangling `owner.json` symlink reaches `read_owner_file` because `symlink_metadata` reports the symlink itself (`src/mailbox.rs:1245-1251`); its no-follow open rejects it (`:1310-1330`), but there is no dedicated dangling-symlink test.

Exact caller search: `rg -n 'set_pre_commit_hook|set_post_open_hook|exclusive_atomic_write|load_owner_with_rooms|legacy_owner|resolve_owner_file|read_owner_file|validate_owner_values|check_owner_parses|SIGNED_V2_BODY_MAX' src --glob '*.rs'`. `set_pre_commit_hook` is used only by U13 (`src/commands/owner.rs:295,356,360,381`); `set_post_open_hook` only by U12 (`src/mailbox.rs:2502,2516,2530,2550`). Both are test-only hooks but remain justified. `exclusive_atomic_write` has real send, channel, bridge, and owner callers (`src/commands/send.rs:392,403,697`, `src/channel.rs:1593`, `src/commands/bridge.rs:433,447`, `src/commands/owner.rs:82`). `load_owner_with_rooms`, `read_owner_file`, `validate_owner_values`, `resolve_owner_file`, `check_owner_parses`, and `SIGNED_V2_BODY_MAX` also have the non-test callers listed in that search; deleting their duplicate tests does not make them dead. `legacy_owner` is reached from production `load_owner_with_rooms` (`src/mailbox.rs:1256`). The after-commit closure in `exclusive_atomic_write_with` is used by production `exclusive_atomic_write` (`:813-833`), not retained solely for a test.

## Open questions requiring an authorized run

- Verify T7's 0555 sidecar refusal on the actual runner identity; root can bypass mode bits. Source review cannot establish that CI never uses root.
- Run the repaired T22, T23, T26 fixtures, then remove each intended parser/cap/channel guard temporarily and confirm the corresponding keeper goes red; restore byte-for-byte. The current vacuity and proposed binding are source-derived.
- After D deletion, run the named keepers and owner CLI slice. No baseline or post-edit result was produced in this read-only lane.

## Cross-area conflict

**Keep `mailbox::tests::exclusive_atomic_write_never_replaces_an_existing_mail_file`** (`src/mailbox.rs:1877-1897`); delete `owner_tests::exclusive_atomic_write_never_replaces_an_existing_destination` (`:2424-2446`). This confirms the owner review's D mark and supersedes the message-storage ledger's proposed C deletion of the mailbox test (`thoughts/test-audit/ledger-message-storage.md:42`). Do not delete both.

The function under test is the shared `exclusive_atomic_write` primitive (`src/mailbox.rs:813-818,908-953`), called by mail publication, channel publication, bridge delivery, and owner init. The mailbox test covers `AlreadyExists`, preserved destination bytes, **and removal of the failed attempt's temporary file** by listing the directory. The owner test covers the first two plus fresh creation, but misses temporary-file cleanup. Fresh owner creation and the owner-specific refusal/retry path remain at the real `owner init` boundary in T7 (`tests/cli.rs:10238-10345`), while U13 plants a destination specifically between temporary write and commit (`src/commands/owner.rs:344-381`). Thus the mailbox test is the stronger direct keeper for the shared primitive; T7 and U13 retain the distinct owner boundary. The message-storage ledger points to a 256-collision send test for leaked temps, but retaining the direct directory assertion makes that failure easier to isolate and costs no extra declaration beyond the one keeper.
