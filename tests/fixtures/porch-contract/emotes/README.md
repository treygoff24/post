# Emote corpus

These files freeze the emote side of the avatar contract: the built-in emote table, the exact payload that freezing an emote must produce, and the emote record files that post writes and every reader must classify the same way. The rules are in `docs/plans/2026-09-30-porch-build-plan.md`: I1 ("Freeze") and I2 ("The emote record", "Reading a record"). post and Porch's `packages/pixel` test against these bytes; neither edits them.

## builtin-1.json

The built-in emote table, version `builtin-1`. Every emote record stores the version its sender's post used (`library`). A later table is a new file (`builtin-2.json`); this one never changes. It also lists the standard poses, motions and particles.

| Emote | Steps (pose, motion, particle, ms) |
|---|---|
| `wave` | wave 250; idle 250; wave 250; idle 250 |
| `hop` | idle hop 500 |
| `shake` | idle shake 500 |
| `flip` | idle flip 250; idle 250; idle flip 250; idle 250 |
| `blink` | idle blink 500 |
| `celebrate` | celebrate hop spark 750; idle 250 |
| `think` | think question 1000 |
| `sleep` | sleep zzz 1500 |
| `heart` | idle heart 1000 |
| `spark` | celebrate spark 750 |
| `zzz` | sleep zzz 1000 |
| `question` | think question 750 |
| `exclaim` | idle hop exclaim 500 |

## freeze/

`freeze/<pack>.<emote>.json` is the exact payload `{frames, steps}` that freezing `<emote>` for a sender whose avatar is `../avatars/valid/<pack>.json` must produce, in canonical form (sorted member names, no whitespace) with **no trailing newline**. Compare bytes, not parsed values. The payload limit is 1280 bytes.

| File | Source | Bytes | What it shows |
|---|---|---|---|
| `blob.celebrate.json` | builtin | 491 | celebrate falls back to idle the same way, with hop and spark |
| `blob.wave.json` | builtin | 502 | a built-in whose poses fall back to idle: steps say wave, frames hold only idle |
| `bolt.beep-boop.json` | custom | 1210 | a custom emote with shake and an exclaim particle |
| `bolt.hop.json` | builtin | 442 | the smallest built-in freeze: one step, idle only |
| `mochi.knead.json` | custom | 838 | idle alternating with a custom loaf pose, with hearts |
| `ribbit.hop.json` | custom | 795 | Ribbit's custom hop, which shadows the built-in hop for Ribbit only |
| `ribbit.ribbit.json` | custom | 801 | a custom croak pose, once with hop |
| `trey.thumbs-up.json` | custom | 1083 | wave, then celebrate with hop and spark, then idle |
| `trey.wave.json` | builtin | 783 | a built-in resolved against a pack that has its own wave frame |
| `wisp.boo.json` | custom | 1199 | custom poses float and boo, with shake, the blink flicker, and an exclaim particle |

## records/

Each `.emote` file is a complete record as post stores it: a pretty-printed, ASCII-escaped JSON header, then `\n---\n`, then the body (empty when post writes it). Feed the file's bytes to the record reader (`parseEmoteRecord` in pixel; post's history reader) and compare the verdict and rule.

- `playable/`: the reader must return the payload, with no rule.
- `bubble/`: the envelope is fine but the payload is not. History keeps the record, renderers draw the generic bubble, and the reader reports exactly the rule named by the filename up to the first `--`.
- `omitted/`: the envelope is unreadable. History leaves the record out and reports an `unreadable_emote` diagnostic with the rule named by the filename.

Envelope checks run in order (separator, header size, JSON, fields, event), and the first failure is reported. These filenames are descriptions, not message ids, so the id-match check does not apply here. Each bubble file breaks exactly one payload rule.

| File | Verdict | Rule | Header bytes | What it is |
|---|---|---|---|---|
| `playable/bolt-hop-builtin.emote` | playable | — | 978 | built-in hop from a pack that has its own frames |
| `playable/trey-thumbs-up-custom-at.emote` | playable | — | 1757 | custom emote from the owner, aimed --at a participant (visual only) |
| `playable/blob-wave-fallback.emote` | playable | — | 1080 | built-in wave from a pack with no wave frame: steps say wave, frames hold only idle |
| `playable/ribbit-hop-shadows-builtin.emote` | playable | — | 1421 | ribbit's custom hop shadows the built-in hop for ribbit's own emotes only |
| `playable/body-ignored.emote` | playable | — | 927 | a non-empty body is ignored by readers (writers always write an empty body) |
| `bubble/payload-unknown-field.emote` | bubble | `payload-unknown-field` | 1870 | the emote object has an unknown member color |
| `bubble/missing-field--steps.emote` | bubble | `missing-field` | 1505 | the emote object has no steps |
| `bubble/payload-pose-unresolved.emote` | bubble | `payload-pose-unresolved` | 1265 | a step pose is not in frames.body and frames.body has no idle |
| `bubble/payload-frame-string.emote` | bubble | `payload-frame-string` | 1835 | a frozen body frame has 15 rows |
| `bubble/step-ms-range.emote` | bubble | `step-ms-range` | 1851 | a step ms is 59 |
| `bubble/payload-library-grammar.emote` | bubble | `payload-library-grammar` | 1854 | library is not builtin-<n> |
| `bubble/payload-source.emote` | bubble | `payload-source` | 1854 | source is not custom or builtin |
| `bubble/payload-name-grammar.emote` | bubble | `payload-name-grammar` | 1852 | name fails the frame-name grammar |
| `bubble/payload-too-large.emote` | bubble | `payload-too-large` | 2201 | the frozen payload is over 1280 bytes (the header itself is under 4096) |
| `bubble/payload-missing.emote` | bubble | `payload-missing` | 281 | event is emote but there is no emote object |
| `omitted/envelope-separator.emote` | omitted | `envelope-separator` | 928 | no \n---\n separator |
| `omitted/envelope-json.emote` | omitted | `envelope-json` | 928 | the header is not valid JSON |
| `omitted/envelope-event.emote` | omitted | `envelope-event` | 880 | a .emote file whose event is join |
| `omitted/envelope-fields.emote` | omitted | `envelope-fields` | 842 | the envelope has no sent field |
| `omitted/envelope-header-too-large.emote` | omitted | `envelope-header-too-large` | 4211 | the header is over the bridge cap of 4096 bytes |

The playable headers show the size of real records: a typical emote header is under 2 KiB. Post refuses to write a header over 3072 bytes, and readers accept up to 4096 (the bridge's cap). `omitted/envelope-header-too-large.emote` is 4211 bytes. `bubble/payload-too-large.emote` has a small enough header (2201 bytes) but a payload over 1280 bytes, so it is a bubble rather than omitted.
