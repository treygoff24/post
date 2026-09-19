# Lineage byline TDD receipt

Issue: `post-jpw` / `issue.md`.

## Guarantee

When two affiliated participants send from one workspace, every text renderer
uses the lineage and participant stamped on each message, keeps the workspace
reply address as the final suffix, and does not borrow the workspace profile or
pfp. Messages without a lineage retain the legacy rendering. New lineage names
cannot imitate a registered workspace identifier.

## RED

- `cargo test --test participants participant_text_surfaces_prefer_stamped_lineage_to_shared_workspace_profile -- --nocapture`
  failed because Rowan and Fable both rendered as `Cairn (alpha)`.
- `cargo test --test participants participant_identity_new_rejects_lineage_that_imitates_a_workspace -- --nocapture`
  failed because `identity new "A l p h a"` succeeded beside workspace `alpha`.

Commit: `db6151c test: reproduce lineage byline misattribution`.

## GREEN

| Guarantee | Test | Result |
| --- | --- | --- |
| Chat, read, inbox, catchup, search, watch, and watch digest distinguish two lineages sharing a workspace | `tests/participants.rs::participant_text_surfaces_prefer_stamped_lineage_to_shared_workspace_profile` | PASS |
| Search and watch JSON retain `from_participant` and `from_lineage` | same integration test | PASS |
| Lineage/participant headers are sanitized and override the workspace profile | `src/output.rs::tests::sender_label_prefers_sanitized_lineage_and_participant_to_workspace_profile` | PASS |
| Digest text does not collapse same-workspace lineages | `src/commands/watch.rs::tests::digest_text_distinguishes_lineages_that_share_one_workspace` | PASS; the assertion went RED under a temporary raw-workspace mutation |
| Legacy profile and bare-address rendering stays byte-identical | existing `sender_label_*`, chat, read, inbox, and watch tests | PASS |
| Imitative lineage creation is refused | `tests/participants.rs::participant_identity_new_rejects_lineage_that_imitates_a_workspace` | PASS |

Focused checks and `cargo test --all-targets --all-features` passed. The
repository has no installed coverage runner (`cargo-llvm-cov` is unavailable),
so no numeric coverage percentage is claimed.

## Close verification

- The built release artifact rendered the original live Rowan message as
  `rowan [codex-0ea0d6a0] (atlas)` while retaining its participant reply target.
- `scripts/gate.sh` passed formatting, Clippy, all Rust tests, release build,
  297 hook tests, 32 of 34 launcher tests, 41 doorbell tests, and schema. Its
  two launcher failures were the known estate build-cache conflict: the Cargo
  shim overrides the temporary target directories those tests deliberately
  exercise. Running those two metadata-only tests with the real Cargo shim
  passed 2/2. Papercut: `pc2_43642b43e461c331`.
