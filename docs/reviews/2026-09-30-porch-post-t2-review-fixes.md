# Porch T2 review fixes

This report covers the coordinator-authorized final review round in the isolated
Delegate worktree. The original implementation and attention matrix are in
`2026-09-30-porch-post-t2.md`. Main was unchanged at arrival, so no merge was
needed. The copied corpus is unchanged. No source-checkout edit, push, install,
or worktree lifecycle operation was performed.

## Changes

- Record headers use the avatar parser's recursive duplicate-aware JSON visitor;
  duplicates, including the nested duplicate from Porch's test, are envelope-json.
- Event type/value is checked before decoding the other typed fields. Null
  payloads mean payload-missing; built-in versions are positive with no leading zero.
- Doctor accepts readable emotes, including bubbles, and warns only on unreadable
  records. It no longer calls valid emotes stray files or suggests moving them.
- Bubble messages carry emote_rule in history and exact-slice JSON and are not
  reported as skipped. Output schemas and CONTRACT document the field.
- Acknowledgment/discard prefix selection precedes parsing unrelated records.
- Emote sends follow --json; text sends produce a text receipt. Stdin refusals
  name the emote command and preserve --at/output flags in the exact correction.
- Bridge tests use real post-produced emotes and include a roomless sender with
  a header near 3072 bytes, checking host stamping and readback through post.
- Bridge SPEC documents that older bridges do not backfill skipped emotes after
  upgrade. The schema checker exempts avatar/frozen-frame maps only at their
  exact documented paths. Launcher tests clear inherited DELEGATE_RUN_ID.

Findings 11 (Loom) and 14 (avatar error ordering) were skipped as instructed.
Finding 2 retains Rust's strict checks, following the coordinator's ruling.

## Canonical .emote envelope checks for Porch

Checks run in this order. The first failure is the reported rule. Payload
validation happens only after all envelope checks and does not omit the record.

| Field or input | Check | Rule |
| --- | --- | --- |
| Record bytes | First literal newline/three hyphens/newline separator must exist | envelope-separator |
| Header bytes before separator | Length must be at most 4096 bytes | envelope-header-too-large |
| Entire header, recursively including unknown fields | Valid UTF-8 JSON with no trailing JSON tokens; no repeated decoded object member names; serde_json 1.0.150 allows at most 127 nested array/object containers (including the root), refuses depth 128, non-finite/out-of-range numeric parsing and unpaired Unicode-surrogate escapes | envelope-json |
| Root | JSON object | envelope-fields |
| event | Present string exactly emote (missing, null, any other type/value refused before other field checks) | envelope-event |
| id, from, channel, sent | Required strings; absent or wrong type refused | envelope-fields |
| subject | Optional, defaults to empty string; if present must be string (null refused) | envelope-fields |
| from_participant, from_host, from_lineage, address_kind, display_name, pfp, re, sender_address, sender_provenance | Optional string or null; any other type refused | envelope-fields |
| mentions | Optional, defaults to empty list; if present must be array of strings (null refused) | envelope-fields |
| emote, signature_ref | Any JSON value accepted at envelope stage; absent stays absent and present null stays present | No envelope rule |
| Unknown fields | Ignored after recursive JSON validation | No envelope rule |
| id, from, channel, sent | Each must be nonempty after Rust str::trim | envelope-fields |
| channel | One normal path component; no control characters, slash or backslash; not dot or dot-dot | envelope-fields |
| id | Exactly 29 ASCII bytes: 8 digits, hyphen, 6 digits, hyphen, 6 digits, hyphen, 6 ASCII hex digits (uppercase hex accepted); no calendar/time validation | envelope-fields |
| display_name, pfp | If string, no control characters, U+202A..202E, U+2066..2069, U+200E/F, U+061C, U+2028/2029; ZWJ and VS16 accepted | envelope-fields |
| re | If string, same canonical ID grammar as id | envelope-fields |
| Each mentions entry | Same refused profile characters; one path-safe component via validate_room_name | envelope-fields |
| id against filename | Must equal file stem when decoder receives a stem | envelope-id-mismatch |

No additional read-time grammar is imposed on from/sent beyond their nonempty
trimmed strings, or on the optional origin/provenance strings. Sent is not parsed
as a date. Channel is not compared with its parent directory by emote::decode.
Planted mentions, re, signature_ref and subject are stripped after validation;
the body is ignored. Library grammar is builtin-[1-9][0-9]*; a missing or null
emote payload is a visible bubble with payload-missing, not an envelope failure.
The nonempty check uses Rust's Unicode White_Space trimming, not JavaScript's
String.trim: U+0009..000D, U+0020, U+0085, U+00A0, U+1680, U+2000..200A,
U+2028/2029, U+202F, U+205F, U+3000. U+FEFF is not trimmed. The nesting bound
was checked against Cargo.lock and the installed serde_json deserializer's
remaining_depth=128/check_recursion implementation.

The CLI's generated header cap remains 3072 bytes so bridge host stamping has
room under the 4096-byte read cap.

## Verification and red proofs

Status: completed. Final restored ./scripts/test.sh gate passed, exit 0:
Rust 880 passed, 0 failed, 2 existing ignored; hooks 525/525; launcher 33/33;
bridge 369 unittest cases (368 passed, 1 existing skip), shell installer passed,
and resource-runner tests 4/4. Format, Clippy, release and schema passed.
Toolchain: Cargo 1.98.1, Node v26.10.0, Python 3.14.7. Final evidence:
/tmp/porch-t2-review-gate-final.log. The original T2 gate blocker is resolved.

The final gate includes Porch integration tests 13/13, schema suites 15/15,
the record corpus unit test and the real once-loop regression.

Commands used the repo wrapper, which enters testrun and applies CPU caps. Final
invocation (Delegate's marker remains inherited):

```sh
env -u POST_PARTICIPANT -u POST_HARNESS -u POST_FROM -u POST_SENDER_ADDRESS \
  -u CLAUDE_CODE_SESSION_ID -u CLAUDE_PID -u CODEX_THREAD_ID -u CODEX_SESSION_ID \
  ./scripts/test.sh gate
```

The identical corpus passed 84/84 SHA256 entries again; its manifest was compared
byte for byte with Porch's. Corpus tests cover 51/51 avatar packs, 10/10 exact
freeze vectors, and 20/20 record verdicts. No corpus file was modified.

| Command | Observed result |
| --- | --- |
| rust --lib review_ | Before fixes: 3 existing matches passed, 4 codec regressions failed |
| rust --test porch_contract review_ | Before fixes: 4 intended CLI/fixture assertions failed |
| rust --test schema_truth review_avatar_maps | Before path fix: 0 passed, 1 failed |
| rust --lib --test porch_contract --test schema_truth --test schema_surface | 197 library tests passed, 1 unrelated ambient-identity precondition failed, 2 existing ignored; later suites not reached |
| Clean-environment rust --test porch_contract --test schema_truth --test schema_surface | 12 passed, 1 undersized cap fixture failed; later suites not reached |
| rust --test porch_contract review_emote_write_cap | Strengthened fixture: 1 passed |
| Focused mutation commands listed below | Each final mutant: 0 passed, 1 intended failure, exit 101; source restored |
| Bridge-stamping mutant gate | Exit 1: 815 Rust passed/1 schema assertion failed/2 ignored; hooks 525/525; launcher 33/33; bridge channel suite 77 passed/1 intended failure; older stamping tests also failed as expected |
| Final restored gate | PASS, exit 0: Rust 880 passed/2 ignored; hooks 525/525; launcher 33/33; bridge 368 passed/1 skip plus installer; resource runner 4/4 |

Focused mutation commands were:

```sh
./scripts/test.sh rust --test porch_contract review_emote_write_cap
./scripts/test.sh rust --test porch_contract review_ack_prefix
./scripts/test.sh rust --test porch_contract review_emote_stdin_refusal
./scripts/test.sh rust --test porch_contract review_doctor
./scripts/test.sh rust --test porch_contract review_bubble
./scripts/test.sh gate  # bridge host-stamping mutant, then restored source
```

All rust commands above were invoked as ./scripts/test.sh rust (with the
identity cleanup where described). No direct cargo/Node/Python test runner was
used outside the wrapper. cargo fmt formatted the source; git diff --check
passed. Logs are retained under /tmp/porch-t2-review-*.


| Proof | Deliberate fault / before-fix behavior | Observed result |
| --- | --- | --- |
| Duplicate member names | Original last-wins record parser | Codec test failed: duplicate record was accepted |
| Null payload | Original null handling | Codec test failed: type-mismatch instead of payload-missing |
| Event type | Original typed decode before event check | Codec test failed: envelope-fields instead of envelope-event |
| Built-in version | Original zero/leading-zero acceptance | Codec test failed: playable instead of payload-library-grammar |
| Doctor | Original non-.msg stray warning | Real CLI test failed on valid emote warning |
| Bubble diagnostics | Original skipped-files diagnostic without per-message rule | History test failed on missing emote_rule |
| Text send | Original unconditional JSON receipt | Real CLI test failed on JSON in text mode |
| Schema paths | Original bare-name map exemption | Test failed because unrelated body/head/emotes fields were hidden |
| Header write cap | Temporarily removed the 3072-byte refusal | Focused real CLI test failed, 0 passed/1 failed, exit 101; original bytes restored |
| Doctor corrupt warning | Temporarily suppressed warnings for unreadable emotes | Real doctor test failed on missing corrupt-record warning, exit 101; original bytes restored |
| Bridge host stamp | Temporarily bypassed the export host-stamping call | New real roomless fixture reached import and failed with 0 imports instead of at least 3; channel suite 77 passed/1 failed; gate exit 1; original bytes restored |
| Bubble skipped list | Temporarily re-added displayed bubbles to skipped_files | Real history test failed on the nonempty skipped list, exit 101; original bytes restored |
| Prefix selection | Temporarily moved prefix filtering after parsing | Real acknowledgment opened an unrelated file; 0 passed/1 failed, exit 101; original bytes restored |
| Stdin wording | Temporarily bypassed emote-specific refusal | Real stdin test failed on old read wording; 0 passed/1 failed, exit 101; original bytes restored |

The first proposed sparse-file prefix test stayed green under mutation because
allocation refusal became a tolerated read error. It was replaced with Linux
inotify open events on the target and an unrelated file. The final proof checks
a positive target-open control and absence of unrelated opens after the child
exits, with no timing or memory threshold. It then failed on the deliberate
ordering fault. No test-only production seam was added.

An initial all-library run passed 197 tests and failed one existing watch test's
clean-ambient-identity precondition (2 existing ignored). This resumed harness
exports a participant claim. Subsequent verification clears ambient participant,
harness and address variables at the command boundary while preserving the
present DELEGATE_RUN_ID, so the assigned launcher fix is actually exercised.
The pre-fix lane gate's three launcher failures are the corresponding red proof.
The bridge-stamping mutant gate also caught an
accidental emote_rule addition to the mail-slice schema. That unrelated field
was removed, and the chat-slice shape pin was updated before the final gate.

Initial regression observations:

- Codec review filter: four intended failures (duplicates accepted, wrong null
  rule, wrong non-string event rule, zero/leading-zero libraries accepted),
  plus three existing matching tests passed.
- CLI review filter: doctor flagged the valid emote as stray, the bubble had no
  message rule, and text mode emitted JSON. The first cap fixture was below the
  limit and was strengthened with accepted escaped profile metadata and a long
  lineage created through the CLI; this was a fixture correction, not evidence of a missing cap.
- Schema-path test: failed because an unrelated body map hid its undocumented
  field under the original bare-name exemption.

## What green does not prove

The original no-attention matrix remains part of the full gate. Real bridge
fixtures use temporary Git topologies and real post writes; they do not prove
cross-host deployment or an older bridge binary's behavior. The no-backfill
statement follows the older bridge's persisted-position algorithm; no old
executable was run. Live macOS, Herdr, resident and desktop delivery remain
unverified; hook/supervisor transport tests use stubs with real producer bytes.
The once-loop regression uses a simulated wake source; it does not establish
operating-system watch notification timing. The new acknowledgment
filesystem-open proof exercises actual Linux opens only and is Linux-specific. The separate
intermittent existing watch timing investigation remains open.

No intentional changes to I1/I2 beyond the coordinator-approved diagnostic and
rule corrections. No additional design questions remain. The coordinator owns integration and deployment: inspect the fix commit,
merge this branch (or cherry-pick the initial implementation and fixes in order),
then run the gate at the integrated head before installing. Main remained at
its starting commit throughout this run, so no merge/rebase was necessary. Preserve the pre-existing dirty Beads ledger,
including this review task and follow-ups, before retiring the worktree.

## Changed files

```text
CONTRACT.md
bridge/SPEC-v2.md
bridge/tests/test_channels.py
launcher/agent-session.test.mjs
src/avatar.rs
src/commands/chat.rs
src/commands/doctor.rs
src/commands/schema.rs
src/emote.rs
src/output.rs
tests/common/mod.rs
tests/porch_contract.rs
tests/schema_surface.rs
tests/schema_truth.rs
docs/reviews/2026-09-30-porch-post-t2-review-fixes.md
```

The dirty Beads ledger is excluded from the implementation commit. The earlier
implementation commit remains the base of this review-fix commit; integrate the
branch, or both commits in order, rather than cherry-picking only the fixes.
