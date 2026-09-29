# Whole-project test-value audit, 2026-09-29

Trey approved a campaign-mode run of the test-value-audit skill over the whole `post` repo. Sonnet agents wrote the ledgers and made the edits, Sol did the cross-family reviews, and an Opus session orchestrated. Everything merged to main in four batches:

| Batch | Areas |
|---|---|
| 1 | participant gc, lineage, message storage, owner signing, migration fence |
| 2 | routing, rooms, read/search, channels |
| 3 | participants, watch, doctor/who/reports, CLI surface/schema, input guards |
| 4 | install/gate/launcher, skill hooks, Rust bridge, Python bridge |

Every area took the same path: a read-only ledger marking each test keep, fix, consolidate or delete (`ledger-<area>.md`), then a Sol review that could overrule it (`review-sol-<area>.md`). Edits followed, with a mutation red-proof for every repair, consolidation and unproven deletion (`proof-<area>.md`). Each batch then passed the full gate on the merged candidate and a Sol preservation review of the merged diff (`review-sol-preservation-batch<N>.md`). The rulings made along the way are in `edit-brief.md`: Trey's keep rulings, and the product calls he delegated.

## Result

Diff from the pre-audit main to the batch-4 merge, excluding thoughts/:

| Category | Added | Removed | Net |
|---|---|---|---|
| Tests (tests/, bridge/tests/, *.test.mjs) | +2076 | -4962 | -2886 |
| Code (src/, bridge/, scripts/, hooks, launcher) | +564 | -1496 | -932 |
| Docs (*.md) | +23 | -26 | -3 |

The code row includes `#[cfg(test)]` blocks inside src/, so the real test share is larger than shown.

## Low-value categories removed

- Duplicates: CLI tests that restated unit tests, per-adapter replays of shared hook-core tests (about 90 in codex/cursor/grok), and near-identical integration probes of one guard.
- Source and docstring greps that a behavior test already owned.
- Vacuous assertions: legacy paths a participant-bound run never touches, wrong store paths, ranges never asserted non-empty, patterns identical to the body, and `--json` runs of text-only stderr paths.
- Dead production code kept alive only by tests: the room cursor writer chain, eligibility wrappers, `list_channels`, `ChannelState::has_seen`, the legacy room-heartbeat writer, `nearest_native_harness_in`, `output::InboxOutput`, `BodySource.file`, and the 50,000 seen-id warning, which Trey delegated and the lead dropped.

## Product fixes found by the audit

- A refused cross-host letter un-archived a collected recipient. Revival now runs after the refusal checks.
- An unreadable bridge config was reported as "no bridge config".
- `install-post.sh --help` truncated exit code 7.
- Two tests read the developer's live `~/.claude-mail`. They are now sandboxed.
- `post schema` omitted two emitted field groups.
- The gate resolved the release binary separately from the build. It now takes the path from the build's own artifact message.
- `gate_scripts` raced ETXTBSY.
- The README advertised a removed bridge health key.

## Kept on purpose

- The lineage journal reader, the migration-fence transitions test, and the mailbox commit hooks (Trey's rulings).
- The launcher env overrides (operator knobs).
- The crash-injection seams (`BRIDGE_CRASH_AFTER`, `POST_TEST_DELIVER_FAULT`).
- The published error codes `NotYet` and `CrossedSend`.
- The 50 ms stdin timing ceilings, left for measurement.

## Follow-ups (beads)

- Prove `bind` takes `.participants.lock`.
- Stdin timing ceilings.
- The `who` malformed-claim message.
- store_version literals and the unused error codes.
- Coverage gaps: gc during a live watch, and the first valid channel record across two hosts.
- The estate cargo wrapper's per-invocation target slot. This is estate tooling; the repo no longer depends on it.
