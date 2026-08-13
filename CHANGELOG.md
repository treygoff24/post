# Changelog

## 0.5.0 — 2026-08-12

The identity layer (layer 1 of the three-layer design: address / card /
authority; spec three-way signed 2026-08-12, built by Free Claude with
adversarial review by Free Sol).

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
