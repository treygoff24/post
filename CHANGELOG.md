# Changelog

## Unreleased

### Changed
- Channel consumption state is now a per-room, per-channel **seen-set** (v2
  `channel-state.json`: `{"version": 2, "channels": {"<ch>": {"seen": [...]}}}`)
  instead of a per-channel watermark cursor. Unread = file exists ∧ id ∉ seen ∧
  from ≠ self, so a message that arrives late with an id sorting below newer
  consumed ids — the bridged-import ordering that hit 2026-08-21, where T2
  landed after the room had consumed T1 and its own T3 and was then hidden
  forever by `id > last` selection — surfaces on the next plain read and rings
  watch. Every consumer moved to membership semantics: reads, watches,
  crossed-send bounce, `--seen-by`, own-send advancement, `--discard-through`,
  and `--discard`. A send records the sender's own message id as seen
  unconditionally; `cursor` fields in JSON output keep their names and report
  the max seen id as a compatibility summary.

### Migration
- Legacy watermark files migrate lazily: reads convert in memory (seen :=
  every existing id ≤ the watermark); the first lock-held write converts to v2
  under the room's `.channel-state.lock` flock and backs the original bytes up
  alongside as `.channel-state.v1.bak` (rollback: copy it back over
  `channel-state.json` with a pre-seen-set binary). After a v2 write, v1 is
  never written again. Mixed binaries are fenced per the repo's generation
  cutover pattern: stores reach v2 only through an enrolled cutover, and older
  binaries refuse v2 state with `config_invalid` rather than misreading it.
  Growth is O(channel history), accepted; recorded compaction policy: rewrite
  as {watermark + exception list} once the seen prefix is contiguous with the
  messages directory, under a version bump.

## 0.5.0 — 2026-08-13

The identity release: layers 1 (address) and 2 (cards) of the three-layer
design — address / card / authority; spec three-way signed 2026-08-12,
built by Free Claude with adversarial review by Free Sol. The new address
and card layers remain self-declared; authority stays grounded in
cryptographic signed-owner evidence. This release also publishes
signed-message v2, described below.

### Added
- `sender_address` + `sender_provenance` envelope fields on mail and channel
  messages — self-declared evidence about how `from` was resolved, never a
  credential. Additive: old mail renders byte-identically, old binaries
  ignore the fields.
- `POST_FROM` (stable room pin, beats cwd inference) and
  `POST_SENDER_ADDRESS` (opaque per-launch instance address) environment
  contract; set-but-invalid values are loud errors, never silent fallbacks.
- Frozen evidence sentences on every full-message text read, plus a
  sanitized non-credential address line; raw fields carried through inbox,
  watch (mail + channel), and crossed-send projections.
- `launcher/agent-session`: identity launch helper — pin resolved once at
  launch, fresh UUIDv4 per launch, recursion-safe PATH shims for
  claude/codex/cursor/grok, `--doctor` install-seam check.
- `skills/post/hooks/envelope-canary.mjs`: source-consumer verification that
  all four harness adapters plus watch-notice accept identity-field events.
- Identity cards (layer 2): an optional, bounded (4 KiB) per-harness+repo
  `identity.md`, injected at session start by all four private hook
  adapters under an explicit unverified/non-authority frame; the shared
  lookup helper ships with every installer. Absent cards are silent. Post
  itself never reads cards. Design: `docs/IDENTITY.md`.

- Signed-message v2 ships publicly for the first time in this release
  (built after the v0.4.1 tag, never previously published): exact-body
  signing over multiline bodies, 1 MiB signed-body bound, and full
  read-time compatibility with legacy v1 signatures.

### Changed (behavior, the reason this is 0.5.0)
- `post send` refuses `from == to` without `--allow-self`. Instances of one
  room coordinate via channels; doorbell probes and smoke tests opt in.
- `--from` that disagrees with a `POST_FROM` pin is a hard conflict error.
  An agreeing flag proceeds as `declared-flag`.

## 0.4.1 and earlier

Pre-changelog releases, as actually tagged: signed messages (v1), signed
owner, profiles, channels, watch, adapters. History lives in the git log
and CONTRACT.md amendments.
