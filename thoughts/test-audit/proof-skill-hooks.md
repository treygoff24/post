# Proof: skill hooks (batch 4, worktree post-test-audit-b, branch test-audit-h)

Each mutation was applied to one production file, the named suite run through testrun with POST_BIN set to the release build, then the file restored byte for byte (git diff on it empty). No production edit landed.

| Repair or consolidation | Mutation | Result |
| --- | --- | --- |
| Core owner: direct backlog cap (was C22) | mail-hook-core.mjs LIST_CAP 20 -> 30 | RED, all four adapters |
| Core owner: repeated session start resets dedupe (was C12, D12) | drop `seen: []` from the start reset | RED for claude, codex, cursor (grok has no start event) |
| Core owner: version probe failure vs missing capability (was C29) | probe failure returns PARTICIPANTS_MISSING | RED (new test, plus three existing tests) |
| Core owner: affiliation read after failed snapshot (was C39) | fail() passes null instead of showIdentity() | RED, all four adapters (new test and the loop describe) |
| Cursor/Grok binding table (was D5, D34-36, E5, E34-36) | grok adapter `bindingLine: undefined` | RED, four table tests plus two neighbours |
| Bind-failure table moved into the claude keeper (from C36) | boundParticipantId always returns an id | RED (bind-failure test and two others) |
| same | remove only the `ok`/`status` guard, or only the id check | STAYS GREEN: each guard is covered by another (id "" is falsy downstream, `ok:false` fails first). The rows bind only against the whole check, not each guard. |
| Claude keeper for the ~75 replays: dedupe | `fresh = events` | RED, 6 tests |
| same: throttle | throttle check replaced by `false` | RED, 1 test |
| same: seen-set pruning | seen keeps prior keys | RED, 2 tests |
| Contract wiring: Codex wording (was C9 folded into C5, and contract envelope) | codex `waiting` set to "Unread agent mail" | RED in contract and in codex C5 |
| Contract wiring: Cursor dual output | drop `additional_context` from cursor payload | RED in contract |
| Grok positive agent_type row (was E14) | grok isSubagent counts agent_type | RED |
| watch-notice F17 takes F8's streaming assertion | unknown event kind refuses the batch | RED in the long-running test |
| contract F36 takes F10's bound:false-with-event case | marker check swallows any `bound:false` | RED |
| H10 (delete redundant assertion, keep whole-array compare) | snapshotArgs adds `--reason mail` | RED (whole-array assertion still binds) |
| P5 (vacuous tail removed) | supervisor's helper-died stderr text changed | RED on the retained stderr assertion |

Baselines after edits, with POST_BIN pinned: mail-hook-core 160/160, claude 40/40, codex 7/7, cursor 8/8, grok 10/10, contract 54/54, watch-notice 16/16, doorbell-supervisor 103/103, doorbell-supervisor-process 15/15, four installer suites green (242 tests across the 8-file batch).

Not mutation-proved: the E6 and E15 renames (titles only), the four identity-card greps (deleted, no assertion moved), C42/C43 and E31 (kept in place as temporary owners, unchanged).

Environment note: under the estate cargo wrapper, `cargo metadata` sometimes resolves a different target slot per invocation (seen: 5aa5... vs 2705...), so a standalone hook suite that resolves the binary itself can point at a slot with no release binary and fail with ENOENT (about a hundred false failures). Pinning POST_BIN removed it. gate.sh resolves once and exports POST_BIN, so it is unaffected.

## Turn-mark seam contract test (own commit)

Test: doorbell-supervisor.test.mjs, "the marks the real Claude hook writes are the marks the supervisor reads". It runs the real claude-mail.mjs for Stop (with background work) and UserPromptSubmit, then a real Supervisor scan, and computes no mark path.

| Mutation | Result |
| --- | --- |
| claude-mail.mjs writes `<digest>.mark` instead of `<digest>.json` | RED: only the new test fails (B39/B40 and H42-H49 stay green, which is the gap the review named) |
| claude-mail.mjs always records `background: false` | RED (new test) |
| claude-mail.mjs stops recording busy at UserPromptSubmit | RED (new test: a stale idle mark rings the working pane) |
| doorbell-supervisor.mjs turnPath reads `<digest>.mark` | RED (new test plus four hand-written-mark tests, 5 failures; the reader side was already bound) |
