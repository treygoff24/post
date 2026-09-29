# Test-value audit: area inventory

Repo `post-test-audit` (branch test-audit = main). Read-only inventory; no source or tests edited.

Method: each tests/*.rs file was assigned by the src module and `post` subcommand it drives. The three huge files (cli.rs, participants.rs, routing.rs) and three mid files (reports.rs, schema_surface.rs, surface.rs) are split into line-range slices. Ranges start at the first test fn of the slice (fn line minus its `#[test]`/doc lines, so edges are +/- a few lines) and run to the line before the next slice. Line counts are total lines in the slice (helpers included). `#[test]` counts were computed by script; Python and Node counts are `def test_` / `test(` occurrences (approximate). Rust integration totals: 40,041 lines, 702 `#[test]`.

Ownership below is by production owner, not by file. `X/ range` means a slice of that file.

Shared support used by nearly every area:
- `tests/common/mod.rs` (951 lines, `Sandbox`, `assert_success`, `register_*`, `write_channel_message`, `tree_snapshot`, fence seeders). `mod common;` is declared in 29 of 30 tests/*.rs (only gate_scripts.rs skips it). One shared file: edits are serialized through one owner (see "Serialization").
- `tests/fixtures/watch-snapshot-typed.ndjson` (only used by watch_preview.rs, area 8).
- `src/test_support.rs` (19 lines, `#[cfg(test)]`, `test_root` + `trash_test_root`), used by unit tests in presence, app, lineage, lineage_store, participant, profile, owner, channel_state, participant_gc, stdin_guard, watch, and others. It has production-free callers by design (it is test infrastructure, not a leaked export). Declared in `src/lib.rs:48-49` (lib.rs has 0 tests).

## Area list (summary)

| # | Area | Integration test lines / #[test] | src unit tests (fns / approx lines) |
|---|------|------|------|
| 1 | Routing, addresses, sender identity pin | 5126 / 90 | none |
| 2 | Read, catchup, search, consumption, byte budget | 3818 / 66 | 26 / ~870 (+cursor_state 14 / 491) |
| 3 | Channels (membership, crossed send, archive, join-from-now) | 4078 / 63 | 41 / ~1560 |
| 4 | Rooms registry and rename | 2932 / 55 | 9 / 216 |
| 5 | Participants: identity, bind, lifecycle, profile | 2694 / 50 | 26 / ~715 |
| 6 | Participant gc, restore, auto-cleanup | 1572 / 27 | 6 / 464 |
| 7 | Lineage | 2048 / 32 | 3 / ~170 |
| 8 | Watch, doorbell, previews | 3025 / 55 | 39 / ~2300 |
| 9 | Migration fence | 886 / 8 | 7 / 296 |
| 10 | Owner signing and verified badge (a0a / v2) | 2057 / 27 | 2 / 175 |
| 11 | Doctor, who, reports, scaling | 1906 / 30 | 1 / 85 |
| 12 | Message storage, envelope, send/archive publication | 401 / 13 | 31 / ~1215 |
| 13 | CLI surface, schema truth, contract samples | 2926 / 25 | 21 / ~525 |
| 14 | Input guards: stdin, hostile text, body sources, closed pipes | 1476 / 39 | 11 / 169 |
| 15 | Bridge protocol, Rust side (host-qualified addresses, deliver) | 3073 / 70 | 2 / 69 |
| 16 | Bridge, Python (bridge/tests) | ~15,900 (py + sh + harness) / ~385 | n/a |
| 17 | Skill bundle and hooks (Node) | 551 Rust / 12 + 9686 Node / ~427 | n/a |
| 18 | Install, release, gate scripts, launcher | 1472 Rust / 40 + 1178 Node launcher / ~34 | n/a |

Sum check: integration rows 1-15 + 17 (Rust) + 18 (Rust) = 40,041 lines, 702 tests (script total).

## Serialization notes (huge and shared files)

One owner at a time may edit each of these; areas listed share the file:
- `tests/cli.rs` 13,845 lines: areas 1, 2, 3, 4, 5, 8, 9, 10, 11, 12, 13, 14. Largest slices: area 4 (775-3203), area 10 (9800-11856), area 8 (6353-7363), area 9 (7364-7907, 8197-8538).
- `tests/participants.rs` 3,523: areas 5 (main), 3 (99-404), 1 (866-1168, 1426-1538), 4 (1202-1222, 1674-1719), 7 (1983-2065), 8 (3450-3485).
- `tests/routing.rs` 3,683: areas 1 (1-3049), 15 (3050-3329), 8 (3330-3684).
- `tests/reports.rs` 1,265: areas 11 (1-893), 4 (894-1265).
- `tests/schema_surface.rs` 1,272: areas 13 (1-1199), 11 (1200-1272).
- `tests/surface.rs` 690: areas 1 (1-286), 14 (287-394, 426-690), 13 (395-425).
- `tests/common/mod.rs`: every Rust area; any change to it is cross-area.
- src side: each src file with a test module belongs to one area only (table below), so src edits are not shared, but src/mailbox.rs, migration_fence.rs, cursor_state.rs, watch.rs, participant.rs carry `#[cfg(test)]` seams inside production code (see Test-only seams).

## Pilot recommendation

Area 6, participant gc / restore / auto-cleanup. It is 1,572 integration lines and 27 tests, sits entirely in two dedicated files (participant_gc.rs, participant_auto_gc.rs) plus one src unit-test module (commands/participant_gc.rs, 464 test lines), and touches no huge shared file. Its owner boundary is crisp (`post participant gc/restore`, `bind` auto run, `src/participant/gc.rs`, `src/commands/participant_auto_gc.rs`). Cross-area contact is small and read-only: `schema_truth.rs 412-553` (area 13) and `reports.rs 580` (area 11) assert gc numbers/schema, and `common` helpers. Runner-up: area 7 (lineage), a 1,965-line own file, but 83 lines live in participants.rs.

## Areas in detail

### 1. Routing, addresses, sender identity pin
- Owns: `src/routing.rs` (1000), `src/eligibility.rs` (600), `src/command_result.rs`; no src unit tests.
- Tests (5126 lines / 90 t): routing.rs 1-3049 (53 t: reply origin, pending vs unread, frozen deliveries, corrupt receipts/participants, unbound pending, own-read semantics, lifecycle fan-out); cli.rs 308-391 (armed route refusal, reserved sender), 4562-4631, 5560-5626 (reserved senders, codex impersonation), 11857-12596 (pin, sender address, provenance sentences, projections carry identity), 12869-12959 (own-mail inspect, sender history); participants.rs 866-1168, 1426-1538 (typed targets, recipient workspace); surface.rs 1-286 (delegate ping / allow_self); counts.rs (41); store_boundaries.rs (280, damaged-store error boundaries, corrupt receipt / blocked mail).
- Support: common Sandbox, `register_alpha_beta`, `write_custom_mail`, `write_channel_message`.
- Smell: corrupt-receipt / pending scenarios appear in routing.rs (526, 588, 2358, 2395), store_boundaries.rs (132-273), and doctor tests in cli.rs and reports.rs; likely duplicated. counts.rs is a single-test file. Text-renderer tests in routing.rs 1326-1706 overlap each other (three "read modes share ... state" tests).

### 2. Read, catchup, search, consumption, byte budget
- Owns: `src/commands/read.rs`, `catchup.rs`, `search.rs`, `inbox.rs`, `byte_budget.rs`, `src/cursor_state.rs`, `src/commands/delivery.rs`.
- Tests (3818 / 66 t): byte_budget.rs (1555, 20 t), catchup.rs (435, 13), consuming.rs (187, 4), search.rs (380, 6); cli.rs 392-774 (read framing, peek, id prefixes), 5856-5901, 5943-6352 (discard-through, concurrent acks), 8587-8621, 9653-9799, 12597-12755 (framing env), 13071-13147.
- src unit tests: cursor_state.rs (14 t, ~491), read.rs (4, ~108), catchup.rs (3, ~105), byte_budget.rs (2, ~109), inbox.rs (2, ~35), search.rs (2, ~20).
- Support: common; `post::output::` types.
- Smell: byte_budget.rs is 1,555 lines with heavy fixture helpers; catchup/byte_budget/discard scenarios repeat channel fixtures (each file has its own `channel_fixture`); `sanitize`/forged-marker checks duplicate watch_preview and cli.rs hostile-text tests. `set_pre_commit_hook`-style seams live in cursor_state.rs (CURSOR_READ_HOOK).

### 3. Channels
- Owns: `src/channel.rs` (2373), `src/channel_state.rs`, `src/channel_archive.rs`, `src/commands/channels.rs`, `src/commands/chat.rs`.
- Tests (4078 / 63 t): channels_just_work.rs (1342, 21 t), archive.rs (223, 6); cli.rs 3451-3605, 4820-5146, 5649-5855, 5902-5942, 8539-8586, 8622-8970 (crossed sends, mentions, threads), 9111-9370, 9502-9652, 13180-13845 (join-from-now, 10 t); participants.rs 99-404 (roomless channel send receipts).
- src unit tests: chat.rs (26 t, ~943), channel.rs (11, ~442), channel_state.rs (4, ~179).
- Smell: crossed-send is tested in cli.rs 8739-8886 and channels_just_work.rs 328-486 (probable duplicate); `#`-prefix name handling appears in cli.rs 4901-5146 and channels_just_work.rs 732-765; corrupt-message-skip in channels_just_work 227 and catchup/store_boundaries.

### 4. Rooms registry and rename
- Owns: `src/commands/rooms.rs` (2089), room registry paths in `channel.rs`/`app.rs`.
- Tests (2932 / 55 t): cli.rs 775-3203 (42 t: rooms add / set-path / rename, journal resume, interlocks, rollback, locks), 3342-3366, 3413-3450; participants.rs 1202-1222, 1674-1719; reports.rs 894-1265 (peer-published room names, 9 t).
- src: rooms.rs (9 t, ~216).
- Support: common; bridge-config helpers redefined inside cli.rs (`write_bridge_config`, `write_health`, ~1492-1604) and reports.rs (`write_bridge_health` 862) (duplicated helpers).
- Smell: rooms_add / rename refusal-family tests are numerous and repetitive (remote placeholder x5, rename interlock family); reports.rs peer-name tests overlap cli.rs remote-placeholder tests 1269-1462.

### 5. Participants: identity, bind, lifecycle, profile
- Owns: `src/participant.rs` (1951), `src/participant/*`, `src/presence.rs`, `src/profile.rs`, `src/commands/participant.rs`, `identity.rs`, `profile.rs`.
- Tests (2694 / 50 t): participants.rs 1-98, 405-865, 1169-1201, 1223-1425, 1539-1673, 1720-1982, 2066-3449 (lifecycle 2502-3098, profile 3099-3376), 3486-3523; cli.rs 3367-3412 (unbound chat), 13148-13179 (profile show).
- src unit tests: presence.rs (13 t, ~258), participant.rs (9, ~216), profile.rs (4, ~242).
- Smell: "round2/round4/review" prefixed tests (participant_round2_*, participant_round4_*, participant_review_*) are audit-wave artifacts and likely restate earlier tests; read-only-unbound-creates-nothing scenarios repeat (620, 1241, 2310); `bind_test_actor` / `test_actor_id` are `#[cfg(test)]` items in participant.rs.

### 6. Participant gc, restore, auto-cleanup (PILOT)
- Owns: `src/commands/participant_gc.rs` (1059), `src/participant/gc.rs` (277), `src/commands/participant_auto_gc.rs`.
- Tests (1572 / 27 t): participant_gc.rs (1140, 17 t), participant_auto_gc.rs (430, 10 t).
- src unit tests: commands/participant_gc.rs (6 t, ~464).
- Support: common; own helpers `bound_idle`, `patch`, `tree`, `digest` (each of participant_gc.rs, participant_auto_gc.rs, participants.rs, routing.rs, contract_samples.rs re-defines `digest`/`tree`/`record_path`-style helpers).
- Smell: `shape`/`stamp_path`/`log_path` bookkeeping tests; `the_schema_documents_the_switch` (participant_auto_gc.rs 392) is a docs-in-schema assertion that overlaps area 13 schema_truth.rs 412-553.

### 7. Lineage
- Owns: `src/lineage.rs`, `src/lineage_store.rs` (1399), `src/commands/*` lineage subcommands.
- Tests (2048 / 32 t): lineage.rs (1964, 29 t); participants.rs 1983-2065 (3 t).
- src: lineage.rs (1 t, ~76), lineage_store.rs (2, ~96).
- Smell: long withdraw-selector / retry / gap-count family (lineage.rs 968-1523, about 9 tests) looks like variants of one state machine; torn-tail tests 855 and 893 near-duplicates.

### 8. Watch, doorbell, previews
- Owns: `src/commands/watch.rs` (4720), `src/commands/who.rs` (live-watch part), doorbell files.
- Tests (3025 / 55 t): doorbell.rs (400, 7), watch_preview.rs (461, 9); cli.rs 4273-4315, 5147-5559, 6353-7363 (24 t), 7908-8196 (long watch), 9371-9385; routing.rs 3330-3684 (reason filter, profile env, 5 t); participants.rs 3450-3485.
- src unit tests: watch.rs (39 t, ~2300 lines, mods `tests` and `follow_tests`).
- Support: fixtures/watch-snapshot-typed.ndjson (watch_preview.rs 269 only); `sanitize_preview` (lib.rs:44 `pub use`, production callers exist in watch.rs; the pub re-export is used by tests/watch_preview.rs).
- Smell: watch_text control-character escaping is asserted in cli.rs 7173-7325 (3 tests), watch_preview.rs 57-134, and other places; `profile_trace` in watch.rs is `#[cfg(test)]` instrumentation exercised by routing.rs 3597; cli.rs `watch_snapshot_*` family overlaps doorbell.rs snapshot tests.

### 9. Migration fence
- Owns: `src/migration_fence.rs` (965).
- Tests (886 / 8 t): cli.rs 7364-7907 (one 540-line matrix test), 8197-8538 (7 t).
- src: migration_fence.rs (7 t, ~296; plus 10 `#[cfg(test)]` items).
- Support: common `seed_fence_store`, `fence_under_external_lock`, `write_fence_state_locked`, `assert_migration_refused`.
- Smell: `migration_fence_cli_matrix_...` is one 540-line test; `LOCK_OPEN_HOOK` / `STATE_OPEN_HOOK` and two mode helpers (`conservative_read_mode`, `read_only_must_not_mutate`) exist only under `cfg(test)`.

### 10. Owner signing and verified badge (a0a / v2)
- Owns: `src/commands/owner.rs` (465), v2 manifest in `src/mailbox.rs`.
- Tests (2057 / 27 t): cli.rs 9800-11856 only (a0a_f1-f11, a0a_r2_*, v2_*).
- src: owner.rs (2 t, ~175), mailbox.rs `owner_tests` module (part of mailbox's 21).
- Support: `sha2` dev-dependency exists solely so tests rebuild the manifest independently of src/mailbox.rs::v2_manifest (Cargo.toml comment). Ships ssh-keygen usage in tests.
- Smell: test names carry wave IDs (`a0a_f1`...), 27 tests in a 2,000-line block inside cli.rs; strong candidate to move to its own file.

### 11. Doctor, who, reports, scaling
- Owns: `src/commands/doctor.rs` (1789), `who.rs`.
- Tests (1906 / 30 t): reports.rs 1-893 (13 t), scaling.rs (314, 4 t, timing-based), schema_surface.rs 1200-1272, cli.rs 3626-3993, 8971-9110, 9386-9501.
- src: doctor.rs (1 t, ~85).
- Smell: scaling.rs is timing-based (flake risk); doctor cursor / legacy-state tests are asserted in schema_surface.rs 1200-1272 and cli.rs 3735-3850; doctor_brief tests sit in the cli.rs 12756 slice assigned to area 13.

### 12. Message storage, envelope, send/archive publication
- Owns: `src/mailbox.rs` (2553), `src/model.rs`, `src/commands/send.rs` (1619), `src/imports.rs`.
- Tests (401 / 13 t): cli.rs 236-307, 3204-3316, 4632-4819, 13043-13070. This area is mostly covered by src unit tests.
- src: mailbox.rs (21 t, ~700 non-owner), send.rs (9, ~491), model.rs (1, ~24).
- Smell: `python_reference_mail_reads_back...` and `sent_mail_ascii_escapes...` (cli.rs 3247-3316) are cross-implementation golden checks; `set_pre_commit_hook` / `set_post_open_hook` are `pub(crate)` `cfg(test)` seams in mailbox.rs; `send.rs:70 const fn none()` is cfg(test) only.

### 13. CLI surface, schema truth, contract samples
- Owns: `src/cli.rs` (1114), `src/output.rs` (2658), `src/error.rs`, `src/app.rs`, `src/commands/schema.rs`, `contract.rs`, `version.rs`, `CONTRACT.md`, `contract/samples/`.
- Tests (2926 / 25 t): contract_samples.rs (630, 2 t), schema_truth.rs (635, 8), schema_surface.rs 1-1199 (5 t, some are 500-line tests), cli.rs 1-235, 3317-3341, 3606-3625, 12756-12868, 13007-13042; surface.rs 395-425.
- src: output.rs (14 t, ~411), app.rs (5, ~107), contract.rs (1, ~67), error.rs (1, ~39).
- Smell: three separate schema-vs-reality checks (schema_truth, schema_surface, contract_samples) overlap; contract_samples.rs 571 asserts the embedded list in src/commands/contract.rs matches the samples dir (source-vs-data copy); 31 `post::output::` type imports in tests (`pub mod output` exists for tests and the binary; check what is only test-consumed); help-text wording assertions (`help_and_schema_agree`, `send_help_teaches_...`) are string greps.

### 14. Input guards: stdin, hostile text, body sources, closed pipes
- Owns: `src/stdin_guard.rs` (312), input handling in `src/commands/send.rs`/`chat.rs`, eprintln shadow macros in `src/lib.rs`.
- Tests (1476 / 39 t): stdin_guard.rs (418, 14 t), closed_stderr.rs (88, 2), surface.rs 287-394, 426-690; cli.rs 3994-4272 (body/oversize), 4316-4561 (hostile text), 5627-5648, 12960-13006.
- src: stdin_guard.rs (11 t, ~169).
- Smell: stdin refusal is tested at unit level (src 11 t) and integration (stdin_guard.rs 14 t), likely duplicated; body-source tests repeated in cli.rs 3994-4272 and surface.rs 426-690.

### 15. Bridge protocol, Rust side
- Owns: `src/bridge_topology.rs` (856), `src/commands/bridge.rs`, `src/commands/delivery.rs`.
- Tests (3073 / 70 t): bridge_deliver.rs (1515, 38 t), participant_address.rs (1276, 28 t), routing.rs 3050-3329 (4 t, remote placeholder / colliding sender).
- src: bridge_topology.rs (2 t, ~69).
- Smell: `sha256_hex`, `rfc3339`, `receipt`, `marker` helpers are re-defined across bridge_deliver.rs, participant_address.rs, reports.rs, participant_gc.rs, skill_manifest.rs; delivery-state evidence tests in both files.

### 16. Bridge, Python
- Owns: `bridge/` (Python), tests in `bridge/tests/`.
- Files (lines / tests): test_sweep.py (4504 / 105), test_channels.py (2936 / 76), test_localheld.py (1889 / 57), test_tick_v2.py (1397 / 32), test_pmail.py (1326 / 27), test_rooms.py (944 / 29), test_bounce_safety.py (897 / 38), test_terminal.py (635 / 14), test_decided.py (332 / 7), test_install.sh (400), harness_channels.py (411), harness_v2.py (186), run-all.sh (71), fixtures/ (3 bridge-deliver JSON, pre-f3-bridge.tar.gz).
- Support: harness_channels.py and harness_v2.py shared by these tests; POST_BIN of the Rust binary.
- Smell: test_sweep.py at 4.5k lines is a single-file monolith; JSON fixtures bridge-deliver-*.json mirror the contract in tests/bridge_deliver.rs (area 15).

### 17. Skill bundle and hooks (Node)
- Owns: `skills/post/hooks/*.mjs`, `skills/post/SKILL.md`, `references/`, `agents/openai.yaml`, skill manifest in `build.rs`.
- Rust tests: skill_manifest.rs (193, 5 t), streamlined.rs (356, 7 t: bind activation notice, real harness hooks, activation claim).
- Node tests (9686 lines, ~427 `test(`): doorbell-supervisor.test.mjs (1569), codex-mail.test.mjs (955), grok-mail.test.mjs (883), cursor-mail.test.mjs (872), claude-mail.test.mjs (825), install-doorbell-supervisor.test.mjs (811), mail-hook-core.test.mjs (779), contract.test.mjs (667), doorbell-supervisor-process.test.mjs (490), install-codex-hooks.test.mjs (433), watch-notice.test.mjs (336), install-grok-hooks.test.mjs (344), install-cursor-hooks.test.mjs (322), install-claude-hooks.test.mjs (244), stable-node-path.test.mjs (108), activation-concurrency.test.mjs (48).
- Smell: four near-parallel harness suites (claude/codex/cursor/grok-mail and install-*-hooks) likely repeat the same cases per harness; `contract.test.mjs` (667) is another contract-vs-fixtures check overlapping area 13.

### 18. Install, release, gate scripts, launcher
- Owns: `scripts/*` (gate.sh, install-post.sh, install-smoke.sh, release.sh, smoke-installed.sh, cargo-release-bin.mjs, with-timeout.py), `launcher/`, `tests/acceptance.sh`.
- Tests: install_post.rs (1160, 33 t), gate_scripts.rs (310, 7 t; only integration file that does not use `mod common`); launcher/agent-session.test.mjs (855), launcher/install.test.mjs (194), launcher/cargo-release-bin.test.mjs (129); tests/acceptance.sh (15 lines, a runner/script, not a test suite; it greps scripts/smoke-installed.sh).
- Smell: install_post.rs builds fake self-verify shims and reads script text; gate_scripts.rs asserts script behavior via real subprocess; `acceptance.sh` greps a script's source for `/cp`.

## Src unit-test modules (file, #[test] count, approx test lines) and their area

| File | t | ~lines | Area |
|------|---|--------|------|
| src/commands/watch.rs | 39 | 2300 | 8 |
| src/commands/chat.rs | 26 | 943 | 3 |
| src/mailbox.rs | 21 | ~700 | 12 (owner_tests portion -> 10) |
| src/output.rs | 14 | 411 | 13 |
| src/cursor_state.rs | 14 | 491 | 2 |
| src/presence.rs | 13 | 258 | 5 |
| src/channel.rs | 11 | 442 | 3 |
| src/stdin_guard.rs | 11 | 169 | 14 |
| src/participant.rs | 9 | 216 | 5 |
| src/commands/send.rs | 9 | 491 | 12 |
| src/commands/rooms.rs | 9 | 216 | 4 |
| src/migration_fence.rs | 7 | 296 | 9 |
| src/commands/participant_gc.rs | 6 | 464 | 6 |
| src/app.rs | 5 | 107 | 13 |
| src/channel_state.rs | 4 | 179 | 3 |
| src/commands/read.rs | 4 | 108 | 2 |
| src/profile.rs | 4 | 242 | 5 |
| src/commands/catchup.rs | 3 | 105 | 2 |
| src/commands/byte_budget.rs | 2 | 109 | 2 |
| src/commands/inbox.rs | 2 | 35 | 2 |
| src/commands/search.rs | 2 | 20 | 2 |
| src/bridge_topology.rs | 2 | 69 | 15 |
| src/lineage_store.rs | 2 | 96 | 7 |
| src/commands/owner.rs | 2 | 175 | 10 |
| src/commands/contract.rs | 1 | 67 | 13 |
| src/error.rs | 1 | 39 | 13 |
| src/model.rs | 1 | 24 | 12 |
| src/lineage.rs | 1 | 76 | 7 |
| src/commands/doctor.rs | 1 | 85 | 11 |
| src/lib.rs | 0 | 4 | declares `#[cfg(test)] mod test_support` (support) |
| src/test_support.rs | 0 | 19 | support |

Line figures for files with several test-cfg modules (watch, mailbox, chat) are approximate. In src/commands/catchup.rs `#[cfg(test)]` also appears at line 1 (an import) so its ~105 is the tail module only.

## Test-only seams in production code (candidates for the audit; none verified for callers)
- `src/mailbox.rs`: `set_pre_commit_hook`, `set_post_open_hook` plus thread_locals (`pub(crate)`, cfg(test)).
- `src/migration_fence.rs`: `LOCK_OPEN_HOOK`, `STATE_OPEN_HOOK`, `conservative_read_mode`, `read_only_must_not_mutate`, inline cfg(test) blocks at 236 and 298.
- `src/cursor_state.rs`: `CURSOR_READ_HOOK` thread_local.
- `src/commands/watch.rs`: `profile_trace` module and `scan_batch` (cfg(test) fn at 2008), plus 6 trace-step calls.
- `src/participant.rs`: `nearest_native_harness_in`, `test_actor_id`, `bind_test_actor`.
- `src/commands/send.rs:70`: `const fn none()` under cfg(test).
- `pub mod output` and `pub use commands::watch::sanitize_preview` in `src/lib.rs`: the crate is a lib + bin; integration tests import 31 items from `post::output` (mostly `ErrorEnvelope`, `DoctorSeverity`) and `sanitize_preview` once.
- `sha2` dev-dependency: exists only for independent manifest recomputation in area 10.

## Placement check
- Every tests/*.rs (30 files) placed: archive 3, bridge_deliver 15, byte_budget 2, catchup 2, channels_just_work 3, cli (split: 1,2,3,4,5,8,9,10,11,12,13,14), closed_stderr 14, common (support), consuming 2, contract_samples 13, counts 1, doorbell 8, gate_scripts 18, install_post 18, lineage 7, participant_address 15, participant_auto_gc 6, participant_gc 6, participants (split: 1,3,4,5,7,8), reports (split 11,4), routing (split 1,15,8), scaling 11, schema_surface (split 13,11), schema_truth 13, search 2, skill_manifest 17, stdin_guard 14, store_boundaries 1, streamlined 17, surface (split 1,13,14), watch_preview 8. tests/acceptance.sh -> 18; tests/fixtures -> 8.
- All 10 bridge/tests/test_*.py and test_install.sh, harness_*.py, run-all.sh, fixtures/ placed in area 16.
- All 16 skills/post/hooks/*.test.mjs placed in area 17; all 3 launcher/*.test.mjs in area 18.
- All 30 src files with test modules placed (table above).
- Unplaced: none. (docs/evidence/f2-contested-*/contested-evidence.sh and docs/reviews/*/test-skeptic.md are evidence and review docs, not tests, and were left out.)
