# Porch T2 implementation and verification

Trey approved this implementation on 2026-09-30. The governing brief is
`/tmp/porch-briefs/T2-post.md`; I1/I2 and the T2 acceptance rules are in
`/home/trey-agent/Code/porch/docs/plans/2026-09-30-porch-build-plan.md`.
This lane changes only the Delegate execution worktree, branch
`delegate/codex-20260930T221654Z_705144`. Nothing was installed, pushed, merged,
or removed from the source checkout or worktree.

## Result

Status: blocked acceptance; implementation is complete and committed as a review checkpoint. The final restored gate fails only the unchanged launcher suite (30/33 passed). T2's close condition requires a green full gate, so this lane is not marked accepted.
The unchanged launcher suite fails three tests with `no_participant` because
its child scripts request implicit bootstrap with `participant bind --json`.
Their production binding path and launcher files are outside this lane's edits.
The launcher and participant-bootstrap paths were also compared against starting HEAD with `git diff --exit-code`: no differences. Launcher files are not in T2's explicit ownership list. Blocker `post-5p1` records the required coordinator fix; `post-b7a` remains blocked by it.

Avatars have separate silent storage, duplicate-aware bounded validation,
canonical bytes, and read-time revalidation. Emotes freeze frames and steps,
use a separate suffix and history enumerator, and have no attention-bearing
fields or crossed receipt. The room snapshot now excludes opaque events.
The bridge preserves the suffix and separate reservations, and never publishes
an emote event. Schema, real-producer samples, capabilities, and agent command
reference are updated.

The bridge paging fixture exposed another reachable failure: an already
reserved later message advanced the cursor beyond entries deferred by the page
cap. The cursor now stays before deferred entries. The original gate imported
only one of six emotes; after the repair the publication red proof imported
all six before failing on the deliberately added event records.

## Verification

Final restored gate: **FAILED, exit 1**, solely `GATE FAIL: node launcher tests`. Format, Clippy, release build, schema, Rust, hooks and bridge passed. Rust: **869 passed, 0 failed, 2 existing ignored**. Hooks: **525/525 passed**. Launcher: **30/33 passed**. Bridge: **368 unittest cases (367 passed, 1 existing skip)** plus the shell installer suite passed; resource-runner tests **4/4 passed**. Toolchain: Cargo 1.98.1, Node v26.10.0, Python 3.14.7. Final log: `/tmp/porch-t2-gate-restored-final.log`.

All test commands used `./scripts/test.sh`; codec and live-loop unit filters
used its `rust --lib` form. No direct cargo, Node, or Python test runner was
launched outside the wrapper. `cargo fmt` formatted the source.

| Command | Result |
| --- | --- |
| `sha256sum -c SHA256SUMS` in `tests/fixtures/porch-contract` | 84/84 checksum entries; checksum manifest byte-identical to I0 (85 copied files total) |
| `./scripts/test.sh rust --lib record_corpus` | 1 test passed, all 20 record fixtures |
| `./scripts/test.sh rust --test porch_contract` | 7/7 passed after all mutations were restored; includes frozen playback, mixed-suffix pagination and search event/isolation assertions |
| `./scripts/test.sh rust --test schema_surface --test schema_truth` | 14/14 passed after the profile shape/schema repair |
| `./scripts/test.sh rust --lib porch_emotes` | 1 test passed, 3 real watch-loop modes |
| `POST_UPDATE_CONTRACT=1 ./scripts/test.sh rust --test contract_samples` | Producer samples generated; initial ordering assertion corrected afterward |
| Final `./scripts/test.sh gate` | Failed solely on launcher (3 failures); Rust 869 passed/2 ignored, hooks 525 passed, bridge 367 passed/1 skip plus installer and runner 4 passed |
| Initial `./scripts/test.sh gate` | Failed on new test setup, launcher, and paging failures; corrected lane failures as described below |
| Restored gate before schema/lint repair | Rust 805 passed/1 outdated shape assertion failed/2 ignored; Clippy caught integer-bound style; hooks 525 passed, bridge all suites passed; both lane failures corrected |
| Bridge publication mutant `./scripts/test.sh gate` | Expected bridge failure; hooks 525/525 passed; launcher 30/33 passed; Rust 704 passed, 1 old capability assertion failed, 2 existing ignored |

The copied corpus is never modified. Avatar results are 51/51, freeze fixtures
10/10 with byte equality, and record results 20/20 with exact verdict/rule.
The 84 checksum entries cover those files, built-ins, and READMEs; the manifest itself was compared byte for byte with I0.

## Regression matrix

The real CLI creates all stores under temporary `POST_MAIL_ROOT` values.
For snapshots and downstream consumers, each ordinary-message store is observed
before and after adding emotes. Consuming commands restore that same store's
cursor and messages between runs. Inputs include emotes alone, ordinary traffic
interleaved with emotes, planted mentions/visual targets, corrupt emote files,
compatibility `event=emote` `.msg` records, and healthy/missing/corrupt cursors.
Ordinary IDs, bodies, notices, and deliveries are preserved; generated send IDs
and timestamps are normalized. Channel total `.msg` history counts may include
the compatibility event; unread counts must be unchanged. Existing warnings
caused by broken cursors are compared against the no-emote baseline.

| Path | Primary proof/test name |
| --- | --- |
| Bound snapshot | `watch_snapshot_matrix_matches_ordinary_baseline_with_missing_and_corrupt_cursors` |
| Snapshot reason=channel | Same test, reason=channel row |
| Snapshot reason=mention | Same test, reason=mention row |
| Snapshot reason=mail | Same test, reason=mail row |
| Snapshot digest | Same test, digest row |
| Snapshot limits | Same test, limit=1 row |
| Unbound room snapshot | Same test, before/after room snapshot plus positive ordinary control |
| Once with live filesystem hints | `porch_emotes_do_not_complete_once_in_live_poll_or_reconciliation`, Events/60s row |
| Once with polling | Same test, TimedOut/zero row |
| Once with reconciliation | Same test, Events/zero row; ordinary record alone completes the loop |
| Resident command snapshots | Real `--room beta` producer snapshots in `porchSnapshots`, consumed by supervisor matrix |
| Supervisor subscribed | `Porch emotes cannot dispatch attention sinks`, subscribed preferences rows |
| Supervisor unsubscribed | Same suite, no ordinary channel subscriptions; ordinary mention control still rings |
| Supervisor muted | Same suite, muted channel rows; no dispatch |
| Herdr prompt sink | Same suite, herdr rows; counts and payloads compared |
| Resident execution sink | Same suite, resident rows; counts, argv and environment compared |
| Desktop notification sink | Same suite, desktop rows; counts and arguments compared |
| Claude hook | `Porch emotes never create hook or native-monitor notices`, claude adapter |
| Codex hook | Same suite, codex adapter |
| Cursor hook | Same suite, cursor adapter |
| Grok hook | Same suite, grok adapter |
| Watch-notice | Same suite, native notice before/after stdout equality |
| Unread counters | `unread_consumption_catchup_crossed_and_discard_match_baseline`, channels row |
| Peek | Same test, peek row |
| Consuming read | Same test, plain chat row |
| Catchup | Same test, catchup row |
| Crossed-send receipt | Same test, send row; ordinary unseen count preserved; emote-only sends have no crossed block |
| Discard-through | Same test, ordinary target row; `emote_retrieval_targets_duplicates_and_pagination` rejects emote targets |
| Seen-by | `emote_retrieval_targets_duplicates_and_pagination` rejects emote targets; existing `seen_by_lists_members_past_a_message_read_only` preserves ordinary behavior |
| Ack/reply target exclusion | Same retrieval test plus compatibility target checks in `corrupt_emotes_are_history_diagnostics_never_attention` |
| Corrupt-file history diagnostic | `corrupt_emotes_are_history_diagnostics_never_attention` |
| History/since/exact/order/duplicates/budgets | `emote_retrieval_targets_duplicates_and_pagination` and existing budget suites |
| Search optional event and suffix isolation | `emote_retrieval_targets_duplicates_and_pagination`, real compatibility event hit; `.emote` IDs excluded |
| Silent avatar set/clear, old profile rewrite | `avatar_silent_storage_and_revalidation` |
| Bridge export/import/paging/replay/reservations | `test_emotes_export_page_import_replay_and_reservations_are_silent` |
| Corrupt bridge envelope | `test_corrupt_emotes_are_quarantined_without_bridge_events` |
| Mixed-version file compatibility | Avatar sibling survives simulated old profiles rewrite; old `.msg` enumeration is preserved; compatibility event targets excluded. No older executable was run. |
| Writer classification | Existing `classifies_every_writer_and_read_only_variant`, extended with avatar/emote verbs |

Snapshot matrix: 3 cursor states x 6 modes x 2 traffic states, each with bound
and room observations. Hook matrix: 3 cursor states x 2 traffic states x 4
adapters plus watch-notice. Supervisor matrix: 3 cursor states x 2 traffic
states x 3 preferences x 3 sinks = 54 before/after comparisons. These do not
require paid agents or live external sinks.

## Deliberate red proofs

Each mutant ran in the foreground, failed on its intended observable assertion,
and restored the original source bytes in a `finally` block. All three fault
sites were checked afterward; no mutant remains in the implementation.

| Boundary deliberately broken | Command and red result | Evidence |
| --- | --- | --- |
| Attention enumeration: `message_files()` temporarily admitted `.emote` | `./scripts/test.sh rust --test porch_contract corrupt_emotes_are_history_diagnostics_never_attention`: 0 passed, 1 failed, exit 101 | Bound snapshot was no longer empty; `/tmp/porch-t2-red-attention.log` |
| Corruption handling: room scanner temporarily used merged history enumeration | `./scripts/test.sh rust --test porch_contract watch_snapshot_matrix_matches_ordinary_baseline_with_missing_and_corrupt_cursors`: 0 passed, 1 failed, exit 101 | Emote-only room emitted `event=unreadable` for the corrupt emote; `/tmp/porch-t2-red-corruption.log` |
| Bridge publication: publisher temporarily emitted events for every suffix | `./scripts/test.sh gate`: bridge channel suite 76 passed, 1 failed, exit 1 | All 6 emotes imported; event-directory bytes differed because of added emote events; `/tmp/porch-t2-red-bridge-gate.log` |

## Limits and integration

Green does not prove macOS behavior, real Herdr/resident/desktop delivery,
cross-host transport, or an actual mixed-version binary pair. The bridge uses
real temporary Git topologies; downstream consumer dispatches use the existing
transport stubs with real producer snapshot bytes. No live mailbox was used.
A separate existing watch test took about 600 seconds in the final gate:
`live_participant_watch_routes_bridge_arrival_and_emits_without_consuming`.
Its child remained alive while the fixture file was unrouted, then completed
at the ten-minute reconciliation interval. Prior runs finished it quickly.
That green test has no per-child exit deadline, so it does not establish the
filesystem-event timing its comment claims. Root cause is not diagnosed;
`post-vyi` records a coordinator-owned follow-up. No runtime routing changes
were made for this observation.

Avatars remain host-local, as I2 specifies. The required Opus/xhigh review is
for the coordinator; this lane did not perform it.

No intentional I1/I2 implementation deviations. The paging repair is necessary
to retain emotes under the existing bridge cap. Existing `.msg` reservations
retain their bare-ID keys for upgrade compatibility; emote keys carry `.emote`.
The existing CLI rejects `--since --limit`; the pagination proof uses its
supported byte-budget form and tests newest-N separately.

Coordinator next steps: inspect/cherry-pick this branch's commit, resolve launcher blocker `post-5p1` in its owning lane, run Opus/xhigh review,
then rerun the full gate at the integrated head before merging or installing.
No I1/I2 design questions remain open. The intermittent watch timing root cause remains a separate open investigation (`post-vyi`).

The pre-existing dirty `.beads/issues.jsonl` is excluded from the implementation
commit. It now also carries the lane task and follow-ups (`post-b7a`, `post-5p1`,
`post-vyi`); preserve/transfer their ledger state before retiring this worktree. No shared skill-pool sync, push, personal memory write, or worktree
cleanup ran: the launch authorizes this execution workspace only. Test logs
under `/tmp/porch-t2-*` are retained as evidence. No operator prose corrections
were received during this unattended run.

## Changed files

125 implementation/report files: 40 listed below, plus all 85 files in `tests/fixtures/porch-contract/` (including its checksum manifest).

```text
CONTRACT.md
README.md
bridge/SPEC-v2.md
bridge/bridgelib/channels.py
bridge/tests/test_channels.py
contract/samples/chat-emote.json
contract/samples/profile-avatar.json
contract/samples/profile-list.json
contract/samples/profile-show.json
contract/samples/version.json
docs/reviews/2026-09-30-porch-post-t2.md
skills/post/hooks/contract.test.mjs
skills/post/hooks/doorbell-supervisor.test.mjs
skills/post/references/commands.md
src/avatar.rs
src/channel.rs
src/cli.rs
src/commands/catchup.rs
src/commands/chat.rs
src/commands/contract.rs
src/commands/mod.rs
src/commands/profile.rs
src/commands/schema.rs
src/commands/search.rs
src/commands/version.rs
src/commands/watch.rs
src/cursor_state.rs
src/emote.rs
src/error.rs
src/lib.rs
src/migration_fence.rs
src/model.rs
src/output.rs
tests/common/mod.rs
tests/contract_samples.rs
tests/participants.rs
tests/porch-store.mjs
tests/porch_contract.rs
tests/schema_surface.rs
tests/schema_truth.rs
```
