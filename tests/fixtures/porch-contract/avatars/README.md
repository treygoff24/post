# Avatar pack corpus (format 1)

These files freeze the avatar format that post stores and Porch renders. The rules are in the build plan, `docs/plans/2026-09-30-porch-build-plan.md`, under "I1. Avatar pack, format 1". post (Rust) and Porch's `packages/pixel` (TypeScript) both test against these exact bytes. No implementation lane edits them; a disagreement between a file and the plan goes to the coordinator.

## How to test against it

- Read each file as raw bytes. Do not open it as text, trim it, or re-encode it: several files test the byte-level rules (size, byte-order mark, invalid UTF-8, escapes).
- Run your validator and collect the **set of rule ids** it reports.
- Every file in `valid/` must report the empty set.
- Every file in `invalid/` must report exactly one rule: the filename up to the first `--` (or up to `.json` when there is none). `step-ms-range--2001.json` must report exactly `{step-ms-range}`. A file that fails for an extra or different rule is a failing test, even though the verdict "invalid" is right.
- For valid files, also check the canonical form (sorted member names, no whitespace, integers in plain decimal) against the sizes below. `limit-canonical-16384.json` must canonicalize to exactly 16384 bytes.
- Freeze fixtures for these packs live in `../emotes/freeze/` (see `../emotes/README.md`).

## Palette digits

A pixel is `.` (transparent) or one lowercase hex digit naming a palette index. The meanings are frozen in the plan (I3): 0 ink, 1 night, 2 steel, 3 pale, 4 blue, 5 cyan (Trey only), 6 green, 7 moss, 8 lemon, 9 orange, a red, b magenta, c violet, d pink, e tan, f brown. Accents 0, 1, 5, 8 and a are refused at render time (cyan is allowed only for the post owner, and the app decides who that is, never the file).

## valid/

The first six are hand-drawn characters, meant as the starter kit agents learn from. The rest sit exactly on a limit.

| File | Raw bytes | Canonical bytes | What it is |
|---|---|---|---|
| `trey.json` | 3411 | 2369 | Trey: bearded, cyan hoodie. Body idle, blink, talk, wave, think, celebrate; head idle, blink, talk. Custom emote thumbs-up. Accent 5 (cyan), allowed only because Porch knows Trey is the owner. |
| `bolt.json` | 3910 | 2729 | Bolt, a small robot with an antenna. Accent 4. Custom emote beep-boop. |
| `wisp.json` | 3456 | 2398 | Wisp, a ghost. Accent c. Custom emote boo. |
| `mochi.json` | 2859 | 1991 | Mochi, an orange cat. Accent 9. Custom emote knead. |
| `ribbit.json` | 2583 | 1770 | Ribbit, a frog. Accent 6. Custom emotes ribbit and hop; its hop shadows the built-in hop for Ribbit's own emotes only. |
| `blob.json` | 667 | 453 | Blob, a pink slime with only an idle frame and no custom emotes: the smallest real pack. Built-in emotes fall back to idle. |
| `limit-frames-and-steps.json` | 9176 | 6387 | 16 body frames, 8 head frames, a 24-character frame name, 16 steps, ms at 60 and 2000, total exactly 4000 |
| `limit-emote-count.json` | 2406 | 1513 | 16 emotes, one with a 24-character name |
| `limit-freeze-1280.json` | 2085 | 1412 | custom emote whose frozen payload is exactly 1280 bytes |
| `limit-canonical-16384.json` | 23149 | 16384 | canonical form is exactly 16384 bytes |
| `limit-raw-32768.json` | 32768 | 453 | raw input is exactly 32768 bytes (whitespace padding) |
| `json-escapes.json` | 672 | 453 | a frame name written with a JSON escape (\u0069dle decodes to idle) |
| `number-forms.json` | 794 | 527 | integers written as 1.0 and 2.5e2 are accepted by value |

## invalid/

Each file breaks exactly one rule. Only that rule may be reported.

| File | Rule it breaks | How |
|---|---|---|
| `emote-freeze-too-large.json` | `emote-freeze-too-large` | frozen payload of custom emote "chatter" is 1281 bytes |
| `canonical-too-large.json` | `canonical-too-large` | canonical form is 16385 bytes; every other rule holds |
| `input-too-large.json` | `input-too-large` | raw input is 32769 bytes; canonical form is small |
| `json-syntax.json` | `json-syntax` | trailing comma after a member |
| `json-syntax--bom.json` | `json-syntax` | leading byte-order mark |
| `json-syntax--invalid-utf8.json` | `json-syntax` | a byte 0xff inside a string (not UTF-8) |
| `duplicate-key.json` | `duplicate-key` | the top-level member accent appears twice |
| `duplicate-key--escaped.json` | `duplicate-key` | head has idle twice, once written as \u0069dle |
| `type-mismatch.json` | `type-mismatch` | the top level is an array |
| `type-mismatch--ms-string.json` | `type-mismatch` | a step ms is the string "250" |
| `null-value.json` | `null-value` | emotes is null (absent is allowed; null is not) |
| `missing-field.json` | `missing-field` | head is absent |
| `missing-field--idle.json` | `missing-field` | body has no idle frame |
| `unknown-field.json` | `unknown-field` | unknown top-level member name |
| `unknown-field--step.json` | `unknown-field` | unknown member color inside a step |
| `format-unsupported.json` | `format-unsupported` | format is 2 |
| `not-integer.json` | `not-integer` | a step ms is 250.5 |
| `accent-grammar.json` | `accent-grammar` | accent is uppercase D |
| `accent-grammar--two-chars.json` | `accent-grammar` | accent has two characters |
| `frame-name-grammar.json` | `frame-name-grammar` | body frame named Wave (uppercase) |
| `frame-name-grammar--too-long.json` | `frame-name-grammar` | head frame name has 25 characters |
| `emote-name-grammar.json` | `emote-name-grammar` | emote named "moon walk" (space) |
| `body-frame-count.json` | `body-frame-count` | 17 body frames |
| `head-frame-count.json` | `head-frame-count` | 9 head frames |
| `body-frame-size.json` | `body-frame-size` | body idle has 15 rows |
| `body-frame-size--width.json` | `body-frame-size` | one body row has 17 characters |
| `head-frame-size.json` | `head-frame-size` | one head row has 7 characters |
| `pixel-char.json` | `pixel-char` | uppercase D in a body row |
| `pixel-char--space.json` | `pixel-char` | a space in a head row |
| `emote-count.json` | `emote-count` | 17 emotes |
| `emote-step-count.json` | `emote-step-count` | an emote with 0 steps |
| `emote-step-count--17.json` | `emote-step-count` | an emote with 17 steps |
| `step-ms-range.json` | `step-ms-range` | a step ms is 59 |
| `step-ms-range--2001.json` | `step-ms-range` | a step ms is 2001 |
| `emote-duration.json` | `emote-duration` | steps total 4001 ms |
| `step-motion.json` | `step-motion` | motion spin is not in the set |
| `step-particle.json` | `step-particle` | particle star is not in the set |
| `emote-pose-unknown.json` | `emote-pose-unknown` | pose moonwalk is neither a body frame nor a standard pose |

Notes on the whole-pack limits:

- `input-too-large.json` is `blob.json`'s pack padded with whitespace to 32769 bytes, so a validator that only checks the canonical size would wrongly accept it. `valid/limit-raw-32768.json` is the same pack at exactly 32768 bytes.
- `emote-freeze-too-large.json` is `valid/limit-freeze-1280.json` with one step's `ms` changed from 60 to 100, so its custom emote freezes to 1281 bytes. The freeze is computed as in the plan (I1, "Freeze").
- `canonical-too-large.json` is built the same way as `valid/limit-canonical-16384.json` (padding emotes with long names) but canonicalizes to 16385 bytes, with every other rule still holding.
