# Ledger: area 17, skill hooks (node tests under skills/post/hooks)

Read-only lane. No tests, node, cargo or pytest were run (Trey, 2026-09-29). Everything is from reading tests, owners, callers, and `git log`. Line numbers are `/home/trey-agent/Code/post` (main). Marks: R retain, F fix assertion, C consolidate, D delete. I did not re-run or mutate anything, so "fails if" clauses are read from the owner code, not watched red.

Owners read: `mail-hook-core.mjs` (885 lines, in full), `claude-mail.mjs`, `codex-mail.mjs`, `cursor-mail.mjs`, `grok-mail.mjs`, `watch-notice.mjs`, `doorbell-supervisor.mjs` (2807 lines, the parts each test touches), `install-claude-hooks.mjs` (in full), the other three hook installers and `install-doorbell-supervisor.mjs` (diffed against the claude installer / read around the tests), `stable-node-path.mjs`. `envelope-canary.mjs` is a manual script (`cargo build --release && node ...`), not a test; it has no `test(` declaration and is out of the count.

## Scope check against areas.md

Every `*.test.mjs` under `skills/post/hooks` is assigned. Files: mail-hook-core, claude/codex/cursor/grok-mail, watch-notice, contract, activation-concurrency, stable-node-path, doorbell-supervisor, doorbell-supervisor-process, install-claude/codex/cursor/grok-hooks, install-doorbell-supervisor. Overlaps with other areas:
- `tests/streamlined.rs` 216-296 (real harness hooks notice, Rust, area CLI) overlaps `activation-concurrency.test.mjs` and `codex-mail.test.mjs:840` only in that all run a real release binary through a hook. Neither is redundant: the Rust test drives the CLI, these drive the adapter.
- The Rust watch/participant tests (`tests/watch*.rs`, `participant*.rs`) own the emitted snapshot shape; `contract.test.mjs` owns "three independent JS parsers accept what post prints". Different sides of one contract, so keep both.
- `contract.test.mjs` 533-634 (watch-notice rows) overlaps `watch-notice.test.mjs` 172-240 (malformed, future kinds, unbound). One set uses the real `post contract samples`, the other synthetic; noted under C.

## Tally

452 declarations counted by grep (a loop or table instantiating a test N times is one declaration). The ledger has 349 rows (some rows group consecutive declarations of one describe block). By row: R about 250, C about 95, F 2, D 1 (the identity-card row, which is 4 declarations, one per installer). Split rows: H50 is R for 6 table entries and C for 2; H26 is C only under the stated condition. Almost all R in substance: most tests were added with a bug or review finding (history notes in rows). The C count is one layout decision, not 95 independent ones: the four per-harness `*-mail.test.mjs` files repeat about 30 shared-core tests. Each C names its keeper.

## Structure note (the one big finding)

`mail-hook-core.test.mjs` already has a world builder (`makeWorld`, line ~100-180) and loops `ADAPTERS` (all four harnesses) for conflict, rebind, unbound, lazy, post-broken and deadline behaviour. The four `*-mail.test.mjs` files each build their own stub `post` again and repeat about 30 core behaviours (unreadable channels, malformed stdin, dedupe, throttle, failure streak, malformed snapshot, missing session, count-only, planted symlink, no prune, over-cap, consumed ids, closed stdout, old binary, bind, quoting, touch warning). The core owns all of that logic (`mail-hook-core.mjs` `runMailHook` 560, `deliverThenCommit` 705, `parseSnapshot` 325, `stateWrite`), so a mutation in the core turns up to four copies red for one cause. Recommended end state: keep the claude-mail copy of each shared row as the keeper (it is the only adapter that also has `Stop`/`SessionEnd` and the turn-mark observer), move the codex/cursor/grok copies into the `ADAPTERS` loop as one parameterized row each, and keep in the per-harness files only what is adapter-specific (event-name mapping, payload keys, text wording, `startsOnFirstPrompt`, subagent suppression rules).

Risk to state plainly: adapter wiring differences (`parse`, `payload`, `observe`, `startsOnFirstPrompt`) do reach the shared code, so a consolidation must keep one adapter-parameterized run of each row, not one adapter only. Marking C here assumes that.

## A. mail-hook-core.test.mjs (49 rows, all R; loops instantiate ×4 unless noted)

| # | Test, file:line | Mark | Contract, regression, evidence |
|---|---|---|---|
| A1 | `a future event value is skipped and the rest of the batch survives` :185 | R | `parseSnapshot` skips unknown `event` strings and keeps siblings (commit cb27a63 "tolerant watch parsing"). Fails if `validSnapshotEvent`/`eventKey` (202, 279) fails closed on an unknown kind. |
| A2 | `the unbound marker is recognised ...` :193 | R | The `{event:"unbound", bound:false}` line parses `unbound:true`, not event, not future, not malformed (bcf2556). Fails if the marker falls through to malformed. |
| A3 | `the bare bound:false object with no event is the marker too` :201 | R | Second printed shape. Fails if the marker check needs `event`. |
| A4 | `the marker beside mail leaves the mail and reports unbound` :209 | R | Mixed batch keeps mail. Fails if unbound short-circuits parse. |
| A5 | `a known event that fails validation stays malformed` :214 | R | Fail-closed for known kinds beside a future kind. |
| A6 | `non-JSON lines, arrays and objects with no event string are malformed` :221 | R | Shape rejection. |
| A7 | `a bound:false line that also names an event is not the marker` :227 | R | Marker precedence; a real event with `bound:false` must render. Same rule tested in the supervisor (D6) and real-post (F-contract 649/655), which is three copies of a three-copy parser; keep, the parsers are separate code. |
| A8 | `contextFor takes the harness wording` :233 | R | Adapter text passthrough. |
| A9 | `needs exit 65 and the typed code` :241 | R | `participantMissing` (527) requires both; fails on code-only or exit-only. Same rule is in the supervisor (typed answers). |
| A10-A13 | `the POST_PARTICIPANT conflict line prints once per session` x4: `first event warns...` :259, `another session warns for itself` :270, `a fresh session start (resume) warns again` :278, `a matching explicit id is not a conflict` :287 | R | Warn-once state keyed by session; reset by session start (skipped for adapters without a start). Fails if the warned flag is global or not reset. |
| A14-A20 | rebind: `watch fails participant_missing: rebind once, retry, deliver` :311, `stays missing is a failure, never no mail` :328, `a rebind that itself fails is a failure too` :340, `touch failing participant_missing rebinds without a lifecycle warning` :348, `one rebind per hook run across touch and watch` :360, `an ordinary nonzero exit is not rebound` :368, `SessionEnd for a record already gone is not a warning` :377 | R | One rebind budget per run (`runMailHook` 560). Each row binds a distinct branch of `participantMissing` handling. Fails on unbounded retry, swallowing the failure as "no mail", or rebinding on any nonzero. |
| A21-A24 | unbound marker/future kinds through the hook: `is no mail and no error` :391, `future kind still delivers known mail` :402, `only future kinds is quiet` :411, `malformed known event beside future kind fails closed` :419 | R | End-to-end (child process) versions of A1-A5 including exit 0 and `{}` output; keep since parse-level rows cannot see the output contract. |
| A25 | `an unregistered cwd is not minted at start and stays quiet until the CLI mints it` :436 | R | Lazy minting (3336294): no `participant bind` before a registered workspace. Fails if the hook mints on `SessionStart`. ×3 lazy adapters. |
| A26 | `a session minted while its first scan fails is no longer deferred` :470 | R | `deferReason` (673) clears once minted. |
| A27 | `a registered cwd is minted at start as before` :485 | R | Positive control for A25. |
| A28 | `a subdirectory of a registered room counts as registered` :493 | R | `workspaceRegistered` prefix logic. |
| A29 | `a sibling directory that only shares a name prefix is not inside the room` :502 | R | Path-boundary check; fails on `startsWith` without the separator. |
| A30 | `a delegate child is not minted even inside a registered room` :511 | R | `DELEGATE_RUN_ID` gate. Open question: env leakage (see Q1). |
| A31 | `a session already minted (a resume) is bound as usual even in an unregistered cwd` :519 | R | `probeMinted`. |
| A32 | `an unreadable rooms listing is not evidence: mint as usual` :527 | R | Fail-open on the rooms lookup. |
| A33 | `no answer from show (label) does not mint an unregistered cwd` :549 | R | table x N (timeout / bad exit / bad JSON); the "no answer is not an answer" fix in 3336294. |
| A34 | `no answer from show does not mint a delegate child either` :570 | R | Delegate gate ordering. |
| A35 | `the lookup failure record clears on an answer, so a later failure is reported again` :579 | R | Diagnostic-once-per-streak state. |
| A36 | `a lookup that recovers into a failing scan leaves no lookup failure recorded` :603 | R | Streak state reset. |
| A37 | `an explicit participant is never deferred` :613 | R | `POST_PARTICIPANT` bypass. |
| A38 | `a deferred PostToolUse inside the throttle window spawns nothing` :622 | R | Throttle wins over lookup. |
| A39 | `SessionEnd of a session that was never minted ends nothing` :631 | R | No `participant end` for an unminted session. |
| A40 | `a delegate child in an unregistered cwd is still minted and told its id` :645 | R | Non-lazy adapters keep eager behaviour (×1 non-lazy). |
| A41 | `a missing post binary is reported once per session ...` :663 | R | One diagnostic per failure streak. |
| A42 | `a post whose every command fails is reported once` :674 | R | Same. |
| A43 | `a bind that keeps failing is reported once, retried every turn, and clears when it works` :681 | R | Retry without repeat warning. |
| A44 | `a working setup after a failed one leaves nothing recorded` :700 | R | Clean state. |
| A45 | `a resumed session whose start hit a broken post clears the record on its next good turn` :710 | R | Resume path (lazy adapters with a start). |
| A46 | `a fresh session start reports a broken setup again` :725 | R | Reset by start. |
| A47 | `a release that hangs is cut off by the shared deadline, after delivery is recorded` :743 | R | `SESSION_DEADLINE_MS` 4.5 s incl. notice release; fails if release blocks output or runs unbounded. Timing based; wall-clock sensitive (Q3). |
| A48 | `a step that finds the shared budget already spent is not started` :759 | R | Budget check before each step. |
| A49 | `a prompt release still happens, after the ack` :772 | R | Order: write, then release. |

## B. claude-mail.test.mjs (40 rows). Keeper file for the shared-core rows.

| # | Test, file:line | Mark | Contract, regression, evidence |
|---|---|---|---|
| B1 | `unreadable channels use distinct current keys; legacy identity is not acknowledged` :118 | R | `eventKey` (202) for unreadable channel events; state keys differ per channel. Fails if two unreadable channels collapse to one key (a second one would never surface). Keeper for C rows in codex/cursor/grok. |
| B2 | `malformed stdin fails open to {}` :169 | R | Fail-open contract. |
| B3 | `unsupported hook events emit {}` :174 | R | Claude event mapping. |
| B4 | `channel-only snapshot names the channels, not a phantom mail room` :182 | R | Text wording in claude adapter. Codex lacks this row (codex wording differs?), cursor and grok repeat it: cursor/grok copies C. |
| B5 | `empty snapshot emits {}` :195 | R | Claude emits nothing when empty. Codex identical; cursor/grok emit the binding line, so their versions differ (kept). |
| B6 | `SessionStart surfaces the launch backlog with metadata only` :204 | R | Metadata-only (no subject/preview/from) at start. |
| B7 | `typed 12-hex participant, lineage, and workspace addresses render without poisoning valid siblings` :223 | R | Keeper for typed addresses (commit 32c9e6a). |
| B8 | `pending typed events render as pending rather than unread` :238 | R | Keeper. |
| B9 | `the snapshot runs from the hook's cwd with no --room pin` :247 | R | Keeper. |
| B10 | `missing, relative, or non-string cwd fails open without spawning post` :261 | R | Keeper. |
| B11 | `already-surfaced events dedupe to {} and new mail surfaces alone mid-turn` :272 | R | Dedupe state. Keeper. |
| B12 | `SessionStart resets dedupe state so a pending backlog surfaces again` :298 | R | Claude start semantics. |
| B13 | `subagent events (agent_id present) are suppressed without spawning post` :309 | R | Adapter-specific suppression rule. |
| B14 | `agent_type alone does NOT suppress` :322 | R | Adapter-specific negative (a --agent session is main-thread). Fails if suppression widens to `agent_type`. |
| B15 | `PostToolUse is throttled by state-file mtime` :336 | R | 30 s throttle, keeper. |
| B16 | `a failing post emits one diagnostic per streak, never a fake empty` :359 | R | Keeper. |
| B17 | `malformed or unknown nonempty snapshot output fails closed` :394 | R | Keeper. |
| B18 | `missing or empty session_id fails open without spawning post` :422 | R | Keeper. |
| B19 | `unreadable ids and channel metadata are count-only` :433 | R | Keeper. |
| B20 | `valid unreadable events stay count-only and never echo the id` :467 | R | Keeper. |
| B21 | `state write refuses a planted predictable legacy temp symlink` :491 | R | Exclusive random temp for state (security). Keeper. |
| B22 | `SessionStart does not prune arbitrary sibling state` :511 | R | Keeper. |
| B23 | `a huge distinct-channel backlog bounds the channel summary` :522 | R | Keeper. |
| B24 | `an over-cap backlog is delivered once, then identical snapshots stay silent` :563 | R | Keeper. |
| B25 | `a new arrival after an over-cap backlog still notifies with only the new id` :589 | R | Keeper. |
| B26 | `consumed ids drop from state while every still-unread id is kept` :606 | R | Keeper. |
| B27 | `a closed stdout leaves fresh events and failure eligibility intact` :637 | R | State committed only after a successful synchronous fd 1 write. Keeper. |
| B28 | `SessionStart with an old binary emits only the repair line` :700 | R | Capability probe; keeper. |
| B29 | `SessionStart binds before snapshot with the session cwd` :715 | R | Order. |
| B30 | `unaffiliated participant gets no identity text` :732 | R | Claude wording (cursor/grok differ). |
| B31 | `affiliated participant gets exactly one identity line` :738 | R | |
| B32 | `payload session key mints and reuses one participant across lifecycle events` :745 | R | Keeper. |
| B33 | `explicit POST_PARTICIPANT wins over payload bootstrap` :763 | R | Keeper. |
| B34 | `bind failure emits one setup diagnostic and leaves state retryable` :773 | R | Keeper. |
| B35 | `lineage names are shell-quoted in the voice command` :793 | R | Keeper (injection safety). |
| B36 | `long affiliated lineage omits a truncated executable command` :800 | R | Keeper. |
| B37 | `unsupported participant touch emits one bounded warning` :811 | R | Keeper. |
| B38 | `SessionEnd attempts participant end without scanning` :820 | R | Only claude/codex have SessionEnd; this is the sole copy. |
| B39 | `doorbell turn marks: busy at prompt, idle at Stop with its background work, gone at SessionEnd` :843 | R | New in 7c9266b/783dd45. `recordTurn` (claude-mail.mjs 60-87): the observer writes marks outside the live mail root (f131f42). Fails if the Stop mark is written for non-idle stops or the SessionEnd unlink is dropped. Sole proof of the writer side; the reader side is D-supervisor turn rows. |
| B40 | `doorbell turn marks: every SessionStart clears a leftover mark` :873 | R | Table x4 sources (startup/resume/clear/compact). A stale idle mark would make the doorbell ring a working pane. Fails if a source is left out of the clear. |

## C. codex-mail.test.mjs (43 rows)

Rows that duplicate the claude copy through the shared core and add no adapter-specific input are marked C, keeper = the same-named claude row in section B (parameterization suggested in the Structure note). The codex adapter has no `agent_id`-only suppression differences beyond the PostToolUse rows.

| # | Test, file:line | Mark | Contract, regression, evidence |
|---|---|---|---|
| C1 | `unreadable channels use distinct current keys ...` :122 | C | B1. |
| C2 | `malformed stdin fails open to {}` :173 | C | B2. |
| C3 | `unsupported hook events emit {}` :178 | R | Codex event set. |
| C4 | `empty snapshot emits {}` :186 | R | Codex wording (same as claude, but a different adapter text). Cheap; keep with C-file as the minimal adapter smoke. |
| C5 | `SessionStart surfaces the launch backlog with metadata only` :195 | R | Adapter smoke. |
| C6 | `typed 12-hex participant ...` :213 | C | B7. |
| C7 | `pending typed events render as pending` :228 | C | B8. |
| C8 | `the snapshot runs from the hook cwd with no room pin` :237 | C | B9. |
| C9 | `mail for the cwd-resolved room is accepted and named` :249 | R | Codex-only; names the room. Sole proof. |
| C10 | `missing, relative, or non-string cwd fails open ...` :260 | C | B10. |
| C11 | `already-surfaced events dedupe ...` :274 | C | B11. |
| C12 | `SessionStart resets dedupe state ...` :309 | R | Adapter start semantics (adapter smoke). |
| C13 | `subagent PostToolUse is suppressed without spawning post` :323 | R | Codex-specific input shapes. |
| C14 | `PostToolUse is throttled by state-file mtime` :337 | C | B15. |
| C15 | `a failing post emits one diagnostic per streak` :360 | C | B16. |
| C16 | `malformed or unknown nonempty snapshot output fails closed` :395 | C | B17. |
| C17 | `missing or empty session_id fails open` :421 | C | B18. |
| C18 | `unreadable ids and channel metadata are count-only` :432 | C | B19. |
| C19 | `valid unreadable events stay count-only ...` :466 | C | B20. |
| C20 | `state write refuses a planted predictable legacy temp symlink` :490 | C | B21. |
| C21 | `SessionStart does not prune arbitrary sibling state` :510 | C | B22. |
| C22 | `a huge direct backlog lists at most 20 ids plus an exact remainder` :521 | R | Sole proof of the 20-id direct cap and remainder; core behaviour with no claude/cursor/grok copy. Recommend moving it into the core loop (behaviour of `mail-hook-core.mjs`). |
| C23 | `a huge distinct-channel backlog bounds the channel summary` :550 | C | B23. |
| C24 | `an over-cap backlog is delivered once ...` :591 | C | B24. |
| C25 | `a new arrival after an over-cap backlog ...` :617 | C | B25. |
| C26 | `consumed ids drop from state ...` :634 | C | B26. |
| C27 | `a closed stdout leaves fresh events eligible` :662 | C | B27 (weaker: it lacks the failure-eligibility half the claude copy has). |
| C28 | `SessionStart with an old binary emits only the repair line` :703 | C | B28. |
| C29 | `version probe failures are distinguished from missing capabilities` :717 | R | Codex-only; `probeVersion` distinctions. Sole proof. |
| C30 | `SessionStart binds before snapshot ...` :726 | C | B29. |
| C31 | `unaffiliated participant gets no identity text` :743 | C | B30 (identical wording). |
| C32 | `affiliated participant gets exactly one identity line` :749 | C | B31. |
| C33 | `payload session key mints and reuses ...` :756 | R | Payload key name is per adapter (`payload`); keep as the adapter wiring smoke. |
| C34 | `explicit POST_PARTICIPANT wins ...` :771 | C | B33. |
| C35 | `conflicting explicit participant does not silently override the payload key` :781 | R | Codex-only conflict path; also covered by A10-A13 across adapters (overlap in behaviour), but this one binds the codex payload. |
| C36 | `bind failure emits one setup diagnostic ...` :790 | C | B34 (table of bind stdouts is richer here; fold the table into the keeper). |
| C37 | `lineage names are shell-quoted ...` :813 | C | B35. |
| C38 | `long affiliated lineage omits ...` :820 | C | B36. |
| C39 | `SessionStart still reads affiliation when snapshot fails` :831 | R | Codex-only ordering. Sole proof. |
| C40 | `release binary binds from payload keys and reuses the participant later` :840 | R | The only end-to-end run of a real release binary through an adapter; it builds via `scripts/cargo-release-bin.mjs`. Not redundant with `tests/streamlined.rs` 216-296 (that drives the harness through the CLI). Needs the built binary (Q2). |
| C41 | `unsupported participant touch emits one bounded warning` :924 | C | B37. |
| C42 | `normal events share one absolute deadline across touch and snapshot` :933 | R | Deadline; core loop A47/A48 covers only lazy adapters, this covers codex non-loop path. Sole proof for the touch+snapshot budget. |
| C43 | `an unthrottled PostToolUse shares the same absolute deadline` :945 | R | Same. |

## D. cursor-mail.test.mjs (42 rows)

| # | Test, file:line | Mark | Contract, regression, evidence |
|---|---|---|---|
| D1 | `unreadable channels use distinct current keys ...` :114 | C | B1. |
| D2 | `malformed stdin fails open to {}` :165 | C | B2. |
| D3 | `unsupported hook events emit {}` :170 | R | Cursor camelCase events. |
| D4 | `channel-only snapshot names the channels ...` :178 | C | B4 (wording is the same text through the core). |
| D5 | `empty snapshot emits the fresh binding line` :191 | R | Cursor's binding line (`bindingLine`); adapter-specific. |
| D6 | `SessionStart surfaces the launch backlog ...` :200 | R | Adapter smoke. |
| D7 | `typed 12-hex ...` :220 | C | B7. |
| D8 | `pending typed events ...` :235 | C | B8. |
| D9 | `the snapshot runs from the hook's cwd ...` :244 | C | B9. |
| D10 | `missing, relative, or non-string cwd ...` :258 | C | B10. |
| D11 | `already-surfaced events dedupe ...` :269 | C | B11. |
| D12 | `SessionStart resets dedupe state ...` :296 | R | Adapter start semantics. |
| D13 | `subagent events (subagent_id or agent_id) are suppressed ...` :307 | R | Cursor-specific keys. |
| D14 | `Claude PascalCase event names are not Cursor events and emit {}` :325 | R | Adapter isolation; fails if adapters share an event table. |
| D15 | `workspace_roots supplies cwd when hook cwd is missing` :338 | R | Cursor-only payload key. |
| D16 | `conversation_id is accepted as the session id` :356 | R | Cursor-only. |
| D17 | `is_background_agent and agent_type do not suppress` :365 | R | Cursor-specific negative. |
| D18 | `agent_type alone does NOT suppress` :380 | C | Subsumed by D17 (the same assertion for `agent_type`) and B14; keeper D17. |
| D19 | `PostToolUse is throttled ...` :394 | C | B15. |
| D20 | `a failing post emits one diagnostic per streak` :417 | C | B16. |
| D21 | `malformed or unknown nonempty snapshot output fails closed` :452 | C | B17. |
| D22 | `missing or empty session_id fails open` :480 | C | B18. |
| D23 | `unreadable ids and channel metadata are count-only` :491 | C | B19. |
| D24 | `valid unreadable events stay count-only` :525 | C | B20. |
| D25 | `state write refuses a planted predictable legacy temp symlink` :549 | C | B21. |
| D26 | `SessionStart does not prune arbitrary sibling state` :569 | C | B22. |
| D27 | `a huge distinct-channel backlog ...` :580 | C | B23. |
| D28 | `an over-cap backlog is delivered once ...` :621 | C | B24. |
| D29 | `a new arrival after an over-cap backlog ...` :647 | C | B25. |
| D30 | `consumed ids drop from state ...` :664 | C | B26. |
| D31 | `a closed stdout leaves fresh events and failure eligibility intact` :695 | C | B27. |
| D32 | `sessionStart with an old binary emits only the repair line` :758 | C | B28. |
| D33 | `sessionStart binds before snapshot ...` :767 | C | B29. |
| D34 | `unaffiliated participant gets a neutral binding line` :783 | R | Cursor wording. |
| D35 | `affiliated participant gets binding and identity lines` :790 | R | Cursor wording. |
| D36 | `setup retry emits the binding line again` :797 | R | Cursor binding-line retry (grok has the same; adapter-specific via `bindingLine`). |
| D37 | `payload session key mints and reuses ...` :807 | R | Cursor payload keys (adapter smoke). |
| D38 | `explicit POST_PARTICIPANT wins ...` :821 | C | B33. |
| D39 | `bind failure emits one setup diagnostic ...` :831 | C | B34. |
| D40 | `lineage names are shell-quoted ...` :846 | C | B35. |
| D41 | `long affiliated lineage omits ...` :853 | C | B36. |
| D42 | `unsupported participant touch emits one bounded warning` :865 | C | B37. |

## E. grok-mail.test.mjs (42 rows)

| # | Test, file:line | Mark | Contract, regression, evidence |
|---|---|---|---|
| E1 | `unreadable channels use distinct current keys ...` :113 | C | B1. |
| E2 | `malformed stdin fails open to {}` :164 | C | B2. |
| E3 | `unsupported hook events including SessionStart and PostToolUse emit {} without spawning post` :169 | R | Grok scans on the first prompt only; sole proof it does not scan on start or tool events. |
| E4 | `channel-only snapshot names the channels ...` :182 | C | B4. |
| E5 | `empty snapshot emits the fresh binding line` :195 | C | D5 (same adapter wording through `bindingLine`); keeper D5. |
| E6 | `SessionStart surfaces the launch backlog with metadata only` :204 | R | Grok has no SessionStart scan; the name is stale: the body drives a prompt event. Rename note; contract retained as first-prompt backlog. |
| E7 | `typed 12-hex ...` :223 | C | B7. |
| E8 | `pending typed events ...` :238 | C | B8. |
| E9 | `the snapshot runs from the hook's cwd ...` :247 | C | B9. |
| E10 | `missing, relative, or non-string cwd ...` :261 | C | B10. |
| E11 | `already-surfaced events dedupe ...` :272 | C | B11. |
| E12 | `a new session id surfaces the still-unread backlog again` :298 | R | Grok reset is by session id, not a start event. |
| E13 | `subagent events are suppressed ...` :309 | R | Grok key variants (`agentId`, `subagentId`). |
| E14 | `agent_type alone does NOT suppress` :322 | C | B14/D17. |
| E15 | `user_prompt_submit, hook_event_name, and GROK_HOOK_EVENT all scan` :336 | R | Grok event resolution. |
| E16 | `GROK_HOOK_EVENT is consulted when stdin omits the event name` :351 | R | Env event fallback; sole proof. |
| E17 | `workspaceRoot supplies cwd when cwd is missing` :370 | R | Grok-only key. |
| E18 | `a failing post emits one diagnostic per streak` :384 | C | B16. |
| E19 | `malformed or unknown nonempty snapshot output fails closed` :423 | C | B17. |
| E20 | `missing or empty session_id fails open` :451 | C | B18. |
| E21 | `unreadable ids and channel metadata are count-only` :462 | C | B19. |
| E22 | `valid unreadable events stay count-only` :496 | C | B20. |
| E23 | `state write refuses a planted predictable legacy temp symlink` :520 | C | B21. |
| E24 | `SessionStart does not prune arbitrary sibling state` :540 | C | B22. |
| E25 | `a huge distinct-channel backlog ...` :551 | C | B23. |
| E26 | `an over-cap backlog ...` :592 | C | B24. |
| E27 | `a new arrival after an over-cap backlog ...` :618 | C | B25. |
| E28 | `consumed ids drop from state ...` :635 | C | B26. |
| E29 | `a closed stdout leaves fresh events and failure eligibility intact` :666 | C | B27. |
| E30 | `first prompt with an old binary emits only the repair line` :728 | C | B28. |
| E31 | `capability mismatch stays retryable until the binary is upgraded` :736 | R | Grok/first-prompt retry; sole proof of retry-after-upgrade. |
| E32 | `long affiliated lineage omits ...` :761 | C | B36. |
| E33 | `first prompt binds before snapshot ...` :773 | C | B29. |
| E34 | `unaffiliated participant gets a neutral binding line` :790 | C | D34 (same wording path). |
| E35 | `affiliated participant gets binding and identity lines` :797 | C | D35. |
| E36 | `setup retry emits the binding line again` :804 | C | D36. |
| E37 | `legacy initialized state without a participant retries setup` :814 | R | Grok-only state migration. |
| E38 | `payload session key mints and reuses one participant across prompts` :830 | R | Adapter smoke for grok payload keys. |
| E39 | `explicit POST_PARTICIPANT wins ...` :844 | C | B33. |
| E40 | `bind failure emits one setup diagnostic ...` :854 | C | B34. |
| E41 | `lineage names are shell-quoted ...` :869 | C | B35. |
| E42 | `unsupported participant touch emits one bounded warning` :876 | C | B37. |

## F. watch-notice.test.mjs (19 rows) and contract.test.mjs (21 rows)

`watch-notice.mjs` (parseBatch at 205) is the third copy of the snapshot parser, so its own rows are not redundant with hook rows.

| # | Test, file:line | Mark | Contract, regression, evidence |
|---|---|---|---|
| F1 | `empty snapshot emits no stdout and exits 0` (watch-notice.test) :87 | R | |
| F2 | `mail and channel events render one metadata-only line` :94 | R | Metadata-only. |
| F3 | `channel-only snapshot names the channels ...` :112 | R | |
| F4 | `valid unreadable events stay count-only ...` :123 | R | |
| F5 | `hostile subject and from never reach stdout even on a valid event` :145 | R | Injection safety. |
| F6 | `unreadable channel identity is validated but never rendered; legacy is accepted` :154 | R | |
| F7 | `malformed or unknown nonempty snapshot output fails closed ...` :172 | R | Table. Overlaps the malformed row in contract F26; this is the synthetic set, contract the real-samples set. Keep both. |
| F8 | `a future event kind is skipped ...` :204 | C | Contract F27 (`future event kinds and the bound:false marker are skipped` for watch-notice, :588) proves the same via real post samples. Keeper F27. |
| F9 | `a batch of only future kinds is quiet, not UNKNOWN` :218 | C | Keeper F27. |
| F10 | `the unbound-reader marker is no mail and no error` :225 | C | Keeper F28 (contract :606 with the real printed line); the hand-written marker forms here (table) are the only proof of the bare `bound:false` shape for watch-notice: keep that row from this test when merging. |
| F11 | `scan failure emits UNKNOWN and exits 1` :241 | R | |
| F12 | `an over-cap backlog stays one bounded line` :249 | R | |
| F13 | `--snapshot and --once pass through; default watch is unpinned` :264 | R | Argument passthrough. |
| F14 | `repeated --room is passed through ...` :277 | R | |
| F15 | `--once and --snapshot cannot be combined` :285 | R | |
| F16 | `unknown flags and a valueless --room exit 2 ...` :293 | R | |
| F17 | `long-running mode still emits one line per flushed batch ...` :304 | R | |
| F18 | `typed participant and lineage mail wake native monitors without UNKNOWN` :315 | R | ×3 kinds. |
| F19 | `typed unreadable mail stays count-only and rejects conflicting aliases` :326 | R | |
| F20 | contract `exact samples render routes, ids, counts, and the sample identity` :332 | R | ×4 adapters; real samples from `post contract samples`, so drift in the printed shape fails. Needs the built binary (Q2). |
| F21 | `cursor-unusable snapshot (extra fields) renders as a normal snapshot` :356 | R | ×4. |
| F22 | `unknown fields at top level and in nested objects change nothing` :372 | R | ×4. |
| F23 | `optional snapshot fields removed are handled` :383 | R | ×4. |
| F24 | `optional version and participant fields removed are handled` :406 | R | ×4. |
| F25 | `malformed snapshot events are refused, never rendered` :425 | R | ×4. |
| F26 | `future event kinds and the bound:false marker are skipped, not refused` :439 | R | ×4. Overlaps A21-A24 (stub post) but this one uses real samples. |
| F27 | `the unbound line the real post prints is an empty inbox, not UNKNOWN` :456 | R | ×4. |
| F28 | `malformed version output fails the capability check` :463 | R | ×4. |
| F29 | `malformed participant bind output fails setup` :481 | R | ×4. |
| F30 | `malformed participant show output yields no identity line` :501 | R | ×4. |
| F31 | watch-notice `exact snapshot renders one metadata-only line` :534 | R | |
| F32 | `cursor-unusable snapshot renders as a normal snapshot` :550 | R | |
| F33 | `unknown fields change nothing` :564 | R | |
| F34 | `optional fields removed are handled` :570 | R | |
| F35 | `future event kinds and the bound:false marker are skipped` :588 | R | Keeper for F8/F9. |
| F36 | `the unbound line the real post prints is no mail and no error` :606 | R | Keeper for F10 (real line). |
| F37 | `any malformed snapshot event makes the batch UNKNOWN` :620 | R | |
| F38 | `the hook core reports unbound, with nothing skipped as a future kind` :649 | R | Real-sample bridge to the core parser. |
| F39 | `the supervisor reports unbound, ...` :655 | R | Real-sample bridge to the supervisor parser. Together with F38 these make supervisor test :307 redundant (below). |
| F40 | `a real snapshot with mail keeps its events and is not unbound` :661 | R | Both parsers. |

## G. Small files

| # | Test, file:line | Mark | Contract, regression, evidence |
|---|---|---|---|
| G1 | `overlapping hook events deliver exactly one activation notice` activation-concurrency.test.mjs :15 | R | ×4 harnesses. Real release binary; regression: two hook processes both delivered the one-time activation notice (c2feda0/a135b9a). Overlaps `tests/streamlined.rs` only in binary use. Needs the built binary (Q2). |
| G2 | `prefers a stable alias that points at the same binary` stable-node-path.test.mjs :28 | R | Upgrade-durable node path (2026-09-15, Homebrew Cellar path deleted). |
| G3 | `NEVER substitutes a same-named binary that is a different file` :34 | R | Fails if the alias comparison is by name. |
| G4 | `falls back to execPath when no candidate exists` :44 | R | |
| G5 | `skips a candidate that is not executable` :50 | R | |
| G6 | `returns the candidate unchanged when execPath already is it` :57 | R | |
| G7 | `returns execPath untouched when it cannot be resolved` :62 | R | |
| G8 | `honours candidate preference order` :67 | R | |
| G9 | `on this machine, the result is the running interpreter by another name` :77 | R | Machine-dependent by design (same file as running node); cannot go red on a host without aliases, which is why G3 exists. Fine. |
| G10 | `the built-in candidate list is absolute and covers the estate's package managers` :86 | R | Table check on the constant. |

## H. doorbell-supervisor.test.mjs (88 declarations, grouped) and doorbell-supervisor-process.test.mjs (15)

Supervisor is driven through injected `exec`/`watch`/`now`/`log`/`config` (constructor at `doorbell-supervisor.mjs:754`; production calls `new Supervisor({ paths })` at 2275 only, so the injections have no non-test caller). Rows that only differ by fixture are ×N.

| # | Test, file:line | Mark | Contract, regression, evidence |
|---|---|---|---|
| H1 | `future event kinds are skipped and the events beside them survive` :283 | R | Supervisor's own parser (401). |
| H2 | `a batch of only future kinds is an empty inbox, not a failure` :291 | R | |
| H3 | `the unbound marker is reported as unbound ...` :296 | R | Both printed shapes plus bound:false-with-event negative; sole proof of the hand-written shapes for this parser. |
| H4 | `the unbound line the REAL post prints is reported as unbound` :307 | C | Absorbed by contract.test.mjs :655 (same real line through `supervisorParseSnapshot`, same assertion). Both use `post contract samples`; keeper is the contract row, which also checks the mail-keeps-events variant. |
| H5 | `a known event that fails validation still fails ...` :312 | R | |
| H6 | `the real samples parse whole` :319 | R | |
| H7 | `an unknown optional field is fine` :326 | R | |
| H8 | `(label) fails the whole snapshot` :342 | R | Table x N of malformations, each a distinct `validSnapshotEvent` branch. |
| H9 | `a member of two channels subscribed to one ...` :349 | R | `selectEligible` mention vs ordinary. |
| H10 | `mail and unreadable are always kept; the scan passes no reason filter (B1)` :360 | F | Line 366 `args.slice(0, 5)` deepEqual is the whole array (5 entries) so the next line repeats it. Repair: delete :366 (or assert `--reason` absent explicitly). The remaining assertion is real: it binds "no `--reason` in the scan". |
| H11 | `pending -> routed and degraded -> healthy are different keys` :372 | R | Key carries state class; fails if key drops the class. |
| H12 | `names the participant, carries the self-check sentence, and never mail content` :385 | R | Metadata-only. |
| H13 | `pending, cursor-unusable, and unreadable each get their own wording` :396 | R | |
| H14 | `hostile channel names and ids become <?>` :410 | R | |
| H15 | `output over the cap is killed and reported oversize` :429 | R | |
| H16 | `a slow child times out` :435 | R | Timing. |
| H17 | `an exact digest match binds; a near-miss digest does not` :445 | R | |
| H18 | `discovery binds everything; only a stored enabled: false stays unarmed (E7)` :458 | R | Default-on rule (34ea9e6). |
| H19 | `two panes carrying one session are ambiguous ...` :477 | R | |
| H20 | `a selection never moves to a pane carrying another digest` :495 | R | |
| H21 | `a herdr list failure retires nothing` :507 | R | |
| H22 | `a participant list larger than the old 1 MiB cap still loads` :519 | R | |
| H23 | `a standing participant-list failure logs once, again after ten minutes, and its recovery` :550 | R | |
| H24 | `a participant with no prefs file, bound to a pane, is rung for direct mail` :592 | R | Default-on. |
| H25 | `after disable it is not rung, and a later subscribe does not re-enable it` :605 | R | |
| H26 | `subscribe on a fresh participant leaves it enabled and writes no enabled: false` :629 | C | Same contract as process.test :323 (real CLI, subscribe and select never persist `enabled:false`). Keeper: process :323 (real command). This one goes through `updatePrefs` in-process. Absorb only if the process test also asserts the ring, which it does not; leave R if in doubt. Marked C with that condition. |
| H27 | `enable still sets focused and desktop from its flags` :647 | R | Calls `updatePrefs` directly, so it proves the supervisor honours `focused`/`desktop`, not that `enable` writes them; the CLI half is process :301. Not a duplicate. |
| H28-H31 | `generations`: terminal change retires :670, session change retires :685, resumed session in new pane starts empty :694, identical generation revives with state after (label) :708 (table x N), one participant's state never suppresses another's :732 | R | Generation identity. |
| H32-H36 | `sinks and outcomes`: `notified never suppresses a later accepted prompt` :751, `another sink's state never suppresses this sink` :775, `accepted writes exactly the current eligible keys; a quiet scan prunes` :786, `a pending item that becomes routed rings again` :798, `a degraded item never suppresses its later healthy form` :811 | R | Per-sink state class. |
| H37-H41 | `recheck before the prompt`: busy at recheck :825, focused unless --focused :840, changed session retires :850, lookup error fails/retries/never retires :860, blocked prompt deferred; failed prompt never advances state :878 | R | |
| H42-H49 | `Claude turn marks` (7c9266b/783dd45, recent fix): idle-at-Stop-with-background rung :920, idle mark without background never overrides working :928, busy or unmarked not rung :939, last mark wins, recheck reads fresh :949, another session's mark not applied :967, focused Claude pane waits for --focused :974, non-Claude pane keeps Herdr's status :985, one deferred line per reason :996 | R | `turnFor` (1310), `ringGate` (265-286). Each binds a different clause of the narrowed override. Fails if the override widens to non-Claude panes (:985), to non-background idle (:928), or reads the mark from a different session (:967). |
| H50 | `(label) is failed, never accepted, and never advances state` :1041 | R for 6 rows, C for 2 | Table of 8. Rows 1-6 are distinct stages. Rows 7 and 8 (hand-written unbound marker :1037, bare bound:false :1038) are C: the real-line row (:1036) plus parse row H3 :296 already prove those shapes through the same `parseSnapshot`; the outcome stage `snapshot_unbound` is proven by the real row. Note the table's `typeof script === "function" ? script : script` (:1046) has identical branches: cosmetic, delete when editing. |
| H51 | `post's stderr text no longer decides anything: only the typed marker and code do` :1056 | R | Regression for the retired stderr-text matching. |
| H52 | `failures back off to the cap and mark broken, but never retire` :1064 | R | |
| H53-H58 | `typed answers`: participant_missing retires once :1089, warning line before the envelope :1099, exit 65 with another code or other code under exit 65 is ordinary :1106, `participant show` record-missing retires at recheck :1121, future kind beside mail rings :1137, only future kinds is a successful scan :1153 | R | Same `participantMissing` rule as A9 in a separate implementation. |
| H59-H61 | `lifetime`: expired lease still rings :1167, ended participant retires loudly once :1177, revived by a rebind :1190 | R | |
| H62-H72 | `scheduling`: change during a scan not lost :1214, one host-wide channels call :1233, archived channel keeps its targeted watch :1252, failing channels call retried once per 10 s :1260, startup scans each once, reconcile a minute later :1282, hints: participant-dir writes mark dirty; heartbeat writes do not :1303, prefs change seen with same mtime :1327, watcher error triggers full reconcile :1344, watch failing at creation backs off :1358, reconcile scans every armed subscription and reports gaps :1390, concurrency limit :1402 | R | Fake clock; deterministic. |
| H73-H74 | `channel subscriptions`: subscribe rings once for old unread ... :1436, unreadable under mention-only is a blind spot :1457 | R | |
| H75 | `health records versions, generations, outcomes, and never mail content` :1472 | R | |
| H76-H82 | `resident ring target`: room registration rings with no pane :1504, command gets only --reason, no content in argv/env :1523, reason priority :1537, exit 75 retries without ack :1552, other exit backs off :1577, timeout does not ack :1592, status lists armed state ... :1603 | R | |
| H83-H84 | `headless host`: missing herdr is zero panes :1627, herdr that exists but fails is a failure :1641 | R | |
| H85-H87 | `per-channel mute`: mute suppresses and unmute restores :1653, absent muted field leaves eligible :1676, mute beats subscribe for a resident :1690 | R | |
| H88 | the `describe`-level fixtures and the remaining declarations sum to 88; every declaration in :280-:1706 was read and none other than H4, H10, H26 (condition), and H50 rows 7-8 needed a mark other than R. | | |
| P1 | `simultaneous start: exactly one runs; the other exits with already running (pid N)` process :186 | R | Real flock (`supervisor.lock`); regression: two supervisors delivering. |
| P2 | `an abrupt crash then a restart: the kernel released the lock` :201 | R | |
| P3 | `a stale owner file naming a live, reused pid does not block a start` :213 | R | |
| P4 | `a lock held by someone else refuses the start and writes nothing` :223 | R | |
| P5 | `the lock helper dying stops all delivery and exits` :234 | F | The first half is real: exit code 70 and the stderr line. The tail (write mail, run `enable`, sleep 2.5 s, assert `host.prompts()` is `[]`) is vacuous: the supervisor has already exited (awaited via `exitWithin`), and nothing else can ring, so the empty-array assertion holds with or without the "stop all delivery" behaviour. Repair: delete the tail (the exit assertions carry the contract), or, to bind delivery, plant mail before the kill and assert no ring lands between the kill and the exit. |
| P6 | `an armed idle pane with waiting mail gets one v2 notice; health records accepted` :256 | R | End to end with real process. |
| P7-P15 | agent commands: `enable, disable, subscribe, and unsubscribe write versioned prefs` :301, `subscribe and select never persist enabled: false ...` :323, `an unbound or ended actor is refused` :341, `select accepts a pane carrying this conversation ...` :352, `status reports stale when the lock is held but the heartbeat is old` :366, `resident add, list, and remove store a room and argv` :376, `enable, subscribe, mute, and status target a room with no bound participant` :391, `a resident command rings for the room and status shows it` :435, `help exits 0; bad usage exits 2` :481 | R | Real CLI. |

## I. Hook installers

Four separate installer scripts (claude, codex, cursor, grok) hold near-identical code (stage, atomic rename, symlink-following write, preflight, ownership rules). Each has its own test file that repeats the same 9-10 rows; as the scripts are copies, each copy of a row can independently regress, so they are R. The cheaper structure would be one parameterized suite; that is a refactor recommendation, not a mark. The claude installer differs (exec-form registration, event set with Stop/SessionEnd, `$HOME` ownership, PreToolUse cleanup).

| # | Test (claude file lines; other files the same test at their lines) | Mark | Contract, regression, evidence |
|---|---|---|---|
| I1 | `refuses to run without an explicit target` claude :70 (codex :76, cursor :77, grok :77) | R | Usage exit 2, never guess a live config. |
| I2 | `-h and --help print usage ... before any validation` :76 (:82, :83, :83) | R | Regression 2026-07-31: `--help` wrote `./--help`. Uses a non-runnable bin to prove help precedes preflight. |
| I3 | `preflight refuses a stale binary that mints unroomed mailboxes, touching nothing` :89 | R | Preflight probes for the pre-0.2.0 junk-mailbox bug (2026-07-30). Stub `stale-post` mints a room dir. |
| I4 | `preflight refuses an unrunnable binary` :98 | R | |
| I5 | `creates a fresh settings file with lifecycle events and copies the adapter` :106 (codex :115, cursor :116, grok :116) | R | Event set: claude 5 events with exec form, no matcher; others 3 or 1. Fails if `Stop`/`SessionEnd` registration drops. |
| I6 | `installs the shared core beside the adapter, and the installed adapter runs on its own` :123 | R | The adapter imports `./mail-hook-core.mjs`; a missing core crashes it. This is the real proof that "installed copy is self-contained". |
| I7 | `a malformed settings file leaves neither the adapter nor the core behind` :141 | R | Parse before copy. |
| I8 | `a failure writing the settings leaves the installed files exactly as they were` :157 | R | Staged writes (3336294). Skipped as root. Fails if a rename happens before every stage succeeds. |
| I9 | `is idempotent and preserves unrelated hooks byte-identical` :185 | R | Claude only in this shape; the others use I10/I11. |
| I10 | `updates a stale registration in place instead of duplicating` :213 | R | Marker-based ownership, `$HOME` shell form. |
| I11 | `an unrelated hook that shares the adapter's basename survives the install` :255 | R | Ownership by path or marker, never by basename. |
| I12 | `an upgrade removes an owned registration from an event the adapter no longer uses` :277 | R | e7678cd; visited-events cleanup (owner 264-284). Fails if cleanup is limited to `EVENTS`. Second half asserts the rerun is a no-op. |
| I13 | `installer and installed adapter no longer reference identity-card.mjs` :246 (codex :337, cursor :315, grok :337) | D | x4. Contract: none in the product. The helper was removed (commits 22bbcbc to 49ce2f0); the only remaining `identity-card` strings in the repo outside `.beads` and `thoughts` are these four tests. It asserts absence of a string in source, which can fail only if someone deliberately reintroduces that name. Stronger remaining proof for the real risk (an adapter importing a helper the installer does not ship): I6 (`installed adapter runs on its own`) and, for codex, I19 (end-to-end release proof). Full evidence: repo-wide grep above; feature removal in `49ce2f0 fix: bind hooks to payload participants`. |
| I14 | `copies the adapter privately and writes through hook-config symlinks` codex :155 (cursor :154, grok :156) | R | ×3. Symlink survives; unchanged rerun does not touch mtime. |
| I15 | `normalizes and deduplicates only its own hooks` codex :185 (cursor :187, grok :186) | R | ×3. Shell-form ownership; a hook that merely echoes the name survives. |
| I16 | `refuses to replace a dangling hook-config symlink` codex :243 (:222, :244) | R | ×3. |
| I17 | `malformed target JSON fails before copying the adapter` codex :252 (:231, :253) | R | ×3 (claude I7). |
| I18 | `root array or null config normalizes to an object hooks map` codex :303 (:281, :303) | R | ×3. |
| I19 | `atomic writes refuse a planted predictable legacy temp symlink` codex :324 (:302, :324) | R | ×3. Exclusive random temp; claude's installer has the same code with no test (open question Q4). |
| I20 | `installed adapter copy runs end-to-end with the release CLI` codex :346 | R | Only end-to-end run of a copied adapter with the real binary and `rooms add`; needs the built binary (Q2). |
| I21 | `the emitted node path is upgrade-durable, not version-pinned` codex :403 | R | Regression 2026-09-15; asserts a fixed point of `stableNodePath`, not equality (avoids circularity). Only codex has it although cursor/grok installers also emit a node path (Q5). |

## J. install-doorbell-supervisor.test.mjs (26 declarations)

Fakes: a scripted service manager (`FAKE_SM`), herdr and post stubs, python3 for the lock helper. Test-only seam in the owner: `POST_DOORBELL_INSTALL_TEST_CRASH_AFTER` (install-doorbell-supervisor.mjs:255).

| # | Test, file:line | Mark | Contract, regression, evidence |
|---|---|---|---|
| J1 | `-h and --help print usage and exit 0; an unknown flag exits 2` :300 | R | |
| J2 | `installs, starts, and waits for a healthy supervisor; a rerun is idempotent` :316 | R | ×2 platforms. |
| J3 | `a restart waits for the old job to leave launchd before bootstrapping it again` :351 | R | 1d3272c. |
| J4 | `an old post-doorbell script is moved aside with its hash; the unit template stays` :372 | R | |
| J5 | `a kill between the rename and its receipt heals to moved, and restore moves it back` :394 | R | |
| J6 | `a lock already held by another supervisor refuses before writing anything` :420 | R | Real flock holder; python3. |
| J7 | `a host with no herdr at all installs a residents-only supervisor` :438 | R | Includes a precondition assertion that herdr is unreachable. |
| J8 | `a missing herdr or post binary refuses before writing anything` :458 | R | |
| J9 | `an unhealthy first tick (label) stops the supervisor and fails` :474 | R | ×3. |
| J10 | `an armed participant on an idle pane whose first scan has not finished holds up the start` :496 | R | f27bdd7. |
| J11 | `a first scan that fails after its pane went busy still fails the start` :509 | R | Review fix. |
| J12 | `an armed participant on (label) is a healthy start` :529 | R | ×3 (800e901). |
| J13 | `--dry-run writes nothing and calls no mutating manager command` :545 | R | |
| J14 | `settings come from the unit, its drop-ins, and the environment lines` :564 | R | |
| J15 | `macOS plists: agent, room, and channels` :585 | R | |
| J16 | `one timer: equivalent prefs, a healthy subscription, then only that timer is disabled` :601 | R | ×2 platforms. |
| J17 | `a room, pinned participant, or missing agent mismatch is not migrated (exit 3)` :630 | R | |
| J18 | `killed after (point), a rerun finishes without duplicating anything` :649 | R | ×2 (crash-after seam). |
| J19 | `says the timer is already off, never that it stays on` :677 | R | |
| J20 | `a resume does not disable the timer on a restarted supervisor's stale health` :698 | R | 52615a7; real SIGSTOP; timing-sensitive; the finally block resumes the process. |
| J21 | `default uninstall removes only the supervisor and prints the restoration command` :756 | R | |
| J22 | `--restore-legacy re-enables unchanged units, refuses an edited one, and moves the script back` :775 | R | |
| J23 | `uninstall with nothing installed is a clean no-op (platform)` :792 | R | ×2. |
| J24 | `macOS: restore bootstraps the recorded plist` :800 | R | |

## Keeper per contract

- Hook output/state contract (metadata-only, dedupe, throttle, failure streak, fail-open, fd write commit, temp-file safety): `mail-hook-core.test.mjs` for cross-adapter behaviour (A) plus `claude-mail.test.mjs` B1-B37 as the single per-behaviour copy.
- Adapter wiring (event names, payload keys, wording, first-prompt start): the per-harness files, adapter-specific rows only (C3-C5, C9, C12, C13, C29, C33, C35, C39, D3-D6, D12-D17, D34-D37, E3, E6, E12-E17, E31, E37, E38).
- Snapshot parser agreement across three copies: A1-A7 (core), H1-H8 (supervisor), F7-F10 (watch-notice) as unit; contract F20-F30 and F38-F40 as real-sample bridge.
- Activation delivery exactly once: G1 (real binary).
- Turn marks: writer B39/B40, reader H42-H49; no test binds the two through the same file (the writer and reader agree on `turnPath` only by convention; Q6).
- Installer safety: I-block rows, one copy per script.

## Test-only seams unlocked (with caller evidence)

- `Supervisor` constructor injection of `exec`, `watch`, `now`, `log`, `config`, `env` (`doorbell-supervisor.mjs:754`). Non-test caller: `new Supervisor({ paths })` at :2275 only. Defaults are the real ones, so this is ordinary dependency injection, low risk.
- `POST_DOORBELL_INSTALL_TEST_CRASH_AFTER` (`install-doorbell-supervisor.mjs:255`): exits 97 after a named migration state. Callers: only `install-doorbell-supervisor.test.mjs:653,681,706`. Reachable in production through the environment; a live env var with this name would kill an install midway. Low risk, guarded by exact name.
- `POST_CLAUDE_HOOK_INSTALL_DIR`, `POST_CODEX_HOOK_INSTALL_DIR` and the cursor/grok twins (comments in the installers say "test override"): callers are the tests only; production reads `~/.<harness>/hooks`. `POST_<HARNESS>_HOOK_BIN` is also a documented operator override (`claude-mail.mjs:40`), not test-only.
- `POST_DOORBELL_POST_BIN`/`_HERDR_BIN`: documented operator overrides, used by tests.
- `cargoReleaseBin` (`scripts/cargo-release-bin.mjs`): used by activation-concurrency, codex-mail and install-codex tests, not a production seam; note that it and `POST_BIN` in `contract.test.mjs:34` are two binary-selection mechanisms.

## Suspected product bugs

None confirmed. Two observations, both unproven without a run:
- `install-claude-hooks.mjs:57-58` versus the codex/cursor/grok installers: the claude installer has no test analogous to I19 (planted predictable temp symlink) even though `stageFile` (136-179) implements O_EXCL|O_NOFOLLOW; not a bug, a coverage gap (Q4).
- `install-claude-hooks.mjs:76-98`: the preflight timeout is 4000 ms; a slow `post watch --snapshot` on a cold cache would refuse the install with "could not run" and no hint that it timed out (`probe.error.message` is shown, so the reason is visible). Not a bug.

## Open questions (need a run or a decision)

- Q1. Whether ambient `POST_PARTICIPANT` / `DELEGATE_RUN_ID` leaks into the per-harness suites (A30 and the "delegate child" rows). The world builder deletes `unsetEnv` keys (mail-hook-core.test :159); the per-harness files' `run` I did not confirm delete them, and a session with `POST_PARTICIPANT` exported (this one) is exactly the environment the gate may run in. Needs a run under such an env to settle.
- Q2. Which tests need the built `target/release/post` at gate time: `contract.test.mjs` (`POST_BIN` default), `activation-concurrency` (G1), `codex-mail` C40, `install-codex` I20, and the real-sample rows F38-F40. A missing binary is an assertion failure ("release binary missing"), not a skip.
- Q3. Wall-clock tests (A47, A48, H16, J20, P5 sleep) under CPU load: I cannot judge flake rates without a run.
- Q4. Whether to add I19 for the claude installer (recommended: one mutation of `O_EXCL` red-proofs it) or accept the claude copy as covered by the codex copy's behaviour (they are separate code).
- Q5. `the emitted node path is upgrade-durable` (I21) exists for codex only; cursor and grok installers emit node paths too. Adding to the shared parameterization would cover them.
- Q6. No test ties the Claude turn-mark writer (`claude-mail.mjs` recordTurn) to the supervisor reader (`turnPath`/`readTurn` at :265-286); they agree by hard-coded directory and file naming. A mismatch in either would pass both suites. A mutation of the file name in one place would show whether any test goes red.
