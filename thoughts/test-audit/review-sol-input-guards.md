# Independent layer review: input guards

Read-only source review of the ledger's 50 declarations at `b1e123c`. No tests, Cargo, builds, Pytest, or Node ran. The five fd-kind folds **stand**: the unit tests construct the same descriptor kinds and retained binary tests cover the shared `chat` read guard. The ledger's proposed marks remain **40 R, 2 F, 8 C, 0 D**. This is a proposed cutover, not a claim that the edited suite passes. Ledger line numbers for `tests/cli.rs` have drifted; use test names when implementing (for example, `inline_body_naming_an_existing_file_is_rejected_with_a_body_file_fix` is now at `tests/cli.rs:3892`, and `read_recognizes_a_channel_message_id_and_names_a_command_that_shows_it` is at `:12670`).

## Disagreements and corrections per test

There is **no mark change**. The five cases most at risk of losing a real-binary check are:

| Test (ledger -> review) | Evidence and keeper |
| --- | --- |
| `empty_regular_file_stdin_is_a_normal_read` (C -> C) | The binary receives `Stdio::from(File)` (`tests/stdin_guard.rs:127-136`); U2 creates and probes an empty `File` (`src/stdin_guard.rs:171-178`). `chat` has one `Clear` return, independent of kind (`src/commands/chat.rs:114-119`); G1 proves this call site runs and reads successfully with `/dev/null` (`tests/stdin_guard.rs:120-124`). Keep U2 + G1. |
| `pipe_at_eof_stdin_is_a_normal_read` (C -> C) | The binary receives a pipe after its writer is dropped (`tests/stdin_guard.rs:140-145`); U5 constructs that same EOF pipe (`src/stdin_guard.rs:213-218`). G1 owns the `Clear` command path. Keep U5 + G1. |
| `nonempty_regular_file_stdin_is_refused` (C -> C) | The binary receives a file opened on a nonempty body (`tests/stdin_guard.rs:189-199`); U3 proves `Queued` without consuming it (`src/stdin_guard.rs:183-195`). G4 proves the same call site rejects `Queued`, preserves read state, and executes the remedy using a real pipe (`tests/stdin_guard.rs:149-185`). Keep U3 + G4. |
| `socket_stdin_with_a_queued_byte_is_refused` (C -> C) | Both tests create a `UnixStream::pair`, queue a byte, and pass the read end as an fd (`tests/stdin_guard.rs:225-236`; `src/stdin_guard.rs:236-242`). The probe's socket verdict is U7; the guard's `Queued` mapping is G4. Keep U7 + G4. |
| `delayed_writer_inside_the_bound_is_refused` (C -> C) | Both use a pipe and a writer sleeping 50 ms (`tests/stdin_guard.rs:255-267`; `src/stdin_guard.rs:255-266`). U9 alone tests that `probe` waits for late data; G4 tests the binary's `Queued` mapping. The subprocess starts *after* the producer thread, so G9 can pass even if the producer writes before the child reaches `probe`; it is not a reliable independent wait assertion. Repair U9's timing margin before deleting G9. Keep U9 + G4. |

Each fold removes a descriptor-specific binary invocation, but none removes the sole binary anchor for its command call site. G1 and G4 both reach the default `chat` read call at `src/commands/chat.rs:105`; G8 reaches that same call with `--peek` (`tests/stdin_guard.rs:240-251`), and G11 reaches the two separate consuming-flag calls at `chat.rs:57,61` (`tests/stdin_guard.rs:336-363`). `post read` has a distinct guard at `src/commands/mod.rs:53-54` and `src/commands/read.rs:228-236`; `routing_read_with_piped_stdin_refuses_before_consuming_or_binding` exercises it through the binary with piped data and an ordinary-read control (`tests/routing.rs:2026-2075`). The ledger's `:2000` pointer is stale. **Existing gap:** `chat --message` has its own call at `chat.rs:64-66`; searches of `tests/stdin_guard.rs` and `tests/cli.rs` for `--message` found message-reading tests, but no piped-stdin refusal for this call site. Add one valid-message, queued-pipe integration anchor; none of the five folds supplies it today.

Further corrections without mark changes:

- `fully_consumed_regular_file_is_a_normal_read` remains R (`src/stdin_guard.rs:199-209`), but the ledger's claim that a subprocess *cannot* inherit an advanced offset is false. A parent can read a `File` before passing it through `Stdio::from`; the fd retains its offset. No current integration test does that, so U4 still owns the offset contract.
- `a_bare_argument_that_names_a_file_gets_the_body_file_remedy` remains C. S6 already tests absent path-shaped arguments and an existing `docs/plan.txt` remedy (`tests/surface.rs:514-619`); move S5's explicit zero-delivery assertion (`:494-498`) into S6's existing-file leg before removing S5. The production bare-argument predicate is lexical (`src/commands/send.rs:892-935`), while the separate `--body` existing-file check is at `send.rs:987-1014`, so K1 stays R.
- `consuming_flags_with_a_silent_open_pipe_are_input_ambiguous` remains C: G11 pins both flag call sites and G10 pins the shared ambiguous mapping (`tests/stdin_guard.rs:269-287,336-363,386-407`). Carry no extra assertion; both verdicts return before state mutation (`src/commands/chat.rs:114-157`). `the_refusal_names_the_ssh_case` remains C only after moving its `ssh -n` assertion (`tests/stdin_guard.rs:409-418`) into `assert_refused` (`:98-116`).
- Both `tests/closed_stderr.rs` cases remain F. They pass `--json` (`:61,83`), but the stated pre-effect banners are guarded by `!json_output` (`src/commands/chat.rs:2314-2320`; `src/commands/send.rs:176-188`). The closed pipe is therefore not necessarily written. Drop `--json` in the tested invocation. For each command, first run the same text-mode args with open stderr in a separate fixture or account for the extra delivered message; require the intended banner. Then run with closed stderr, require exit 0 and exactly one additional channel message or delivered mail. This makes replacing the crate shadow `eprintln!` (`src/lib.rs:7-16`) with std's macro produce the old panic before delivery. The actual mutation must be observed later.
- K14's entire named test is a read/channel-id diagnostic (`tests/cli.rs:12670`), not an input guard. Transfer ownership to area 2; do not delete it. K7 combines malformed-mail handling with per-recipient blocked-rule rendering (`tests/cli.rs`, test name `inbox_skips_malformed_mail_and_rooms_only_show_recipient_rules`); split only during the owner cutover, preserving both assertions. K10's bare `line != "FORGED-..."` checks are weak, but its escaped-newline and ESC checks bind; retain R and optionally use `!line.starts_with("FORGED")`.

All other R declarations in the ledger retain a distinct behavior, command wiring, or output surface. In particular, the 11 probe unit cases cover descriptor classification, one-byte consumption, file offset, and tty behavior (`src/stdin_guard.rs:159-311`); G6 retains the real shell heredoc, G10 the binary's ambiguous error, G12 the consuming flags' clear-input control, S2/S3 the actual commands' closed-stdout results, K9/K10 separate read and inbox renderers, and S9/K13 different help promises. No D mark has a same-failure keeper strong enough to justify deletion.

## Keeper per contract

| Contract | Keeper |
| --- | --- |
| Probe kind/verdict and byte/offset/tty invariants | U1-U11 (`src/stdin_guard.rs:159-311`), with U2/U5/U3/U7/U9 absorbing G2/G3/G5/G7/G9 respectively. |
| `chat` default read: clear, queued, ambiguous | G1, G4, G10 (`tests/stdin_guard.rs:119-185,269-287`). G6 retains real heredoc input. |
| `chat --peek`, `--ack`, `--discard-through` guard calls | G8; G11 with G12 as the successful clear-input control (`tests/stdin_guard.rs:239-251,335-384`). |
| `chat --message` guard call | New queued-pipe binary anchor required; currently none found. |
| `post read` guard call | `routing_read_with_piped_stdin_refuses_before_consuming_or_binding` (`tests/routing.rs:2026-2075`); its unbound-peek sibling starts at `:2083`. |
| Refusal guidance | G4/G10 via `assert_refused`, after moving G14's `ssh -n` assertion. |
| Closed stderr and closed stdout | Repaired E1/E2 (`tests/closed_stderr.rs:40-88`); S2/S3 (`tests/surface.rs:325-392`); `src/app.rs` unit tests own `finish_command_result`. S1 owns JSON stderr purity. |
| Body forms, size, subject and help | S4/S6/S7/S8/S9 (`tests/surface.rs:434-690`); K1-K6 and K13 (`tests/cli.rs`, named tests). K1 is the `--body` existing-file check, distinct from S6's bare positional path check. |
| Hostile text, corrupt mail/config, read diagnostics | K7-K12 (`tests/cli.rs`, named tests); K14 transfers to area 2. |

## Final edit list for this area

1. Add one `chat --message <valid id>` integration refusal with a queued pipe in `tests/stdin_guard.rs`; assert exit 2, `invalid_argument`, no slice output, and unchanged read state. This closes an existing call-site hole before pruning.
2. Make U9's delayed-producer unit assertion robust under scheduling load; keep its verdict and wait-detection purpose. Treat all 50 ms timing thresholds and U10's producer deadline as open questions below, not as an approved numeric rewrite.
3. Delete G2, G3, G5, G7, G9 only after the named unit and binary keepers are verified. G1/G4 stay as real-process anchors. Delete G13 after checking G10/G11; move G14's `ssh -n` assertion into `assert_refused`, then delete G14.
4. Move S5's zero-delivery assertion to S6's existing-file leg, then delete S5. Keep K1 for the separate `--body` path.
5. Repair E1/E2 with text-mode closed-stderr invocations and open-stderr banner controls. Keep S1's JSON purity test. Move K14's owner label to area 2 without deleting the test. Split K7 only if the area owner is moving its blocked-rule portion.
6. Optional production simplification: remove `BodySource.file` and replace `source.body_file.or(source.file)` with `source.body_file`; this is independent of the test deletions. No other production seam is unlocked by this area.

Retired declarations proposed: G2, G3, G5, G7, G9, G13, G14, S5. Retired files: none. This review made no implementation edits.

## Maintainer-decision items

None for the proposed test folds or `BodySource.file` removal: they retain the behaviors documented in `CONTRACT.md:291-303,586-592`. The contract explicitly says the `send` positional FILE form is gone (`:294-296`); the `BodySource.file` field has no live caller. Any proposal to change stdin refusal, body-source semantics, or the text/JSON stderr promise (`CONTRACT.md:283-286`) would require the maintainer's decision and a contract update. This review proposes no such change.

## Suspected bugs and production seams

- **No confirmed product bug.** The closed-stderr problem is a test defect: under JSON the cited banner sites do not execute (`tests/closed_stderr.rs:61,83`; `src/commands/chat.rs:2314-2320`; `src/commands/send.rs:176-188`). The library's ignore-write macro still implements the intended behavior (`src/lib.rs:7-16`). Runtime failure and repaired red-proof remain unverified.
- **`BodySource.file` is dead by caller search.** Exact searches `rg -n -F 'BodySource {' src tests` and `rg -n -F 'file:' src/commands/send.rs src/commands/chat.rs` found four production constructors (`src/commands/send.rs:28-34,315-321,630-636`; `src/commands/chat.rs:2273-2279`), all `file: None`; `rg -n -F '.file' src/commands/send.rs src/commands/chat.rs` found its only reader in `source.body_file.or(source.file)` (`send.rs:1018`). The `send.rs` tests call injected body closures rather than construct a non-None `BodySource.file`. This is internal dead code; removal does not remove a body source users can reach.
- **`probe` bound never varies in current callers.** Exact search `rg -n -F 'probe(' src tests` found only `chat.rs:116` and `read.rs:234` outside the unit module, both passing `READINESS_BOUND`. All unit calls pass that constant too (`src/stdin_guard.rs:153-155,188-190,205-207,225-227,239-241,248-250,262-264`). The `bound` parameter (`:44`) is therefore a currently unused variability seam, but keep it pending the timing decision. The fd argument varies in unit tests and must stay. `READINESS_BOUND` itself is used by both guards' error text (`chat.rs:150`; `read.rs:287`).
- **Other ledger nits:** A zero-size procfs regular file would be classified `Clear` by `st_size - offset` (`src/stdin_guard.rs:54-67`), but its runtime and practical reach are unverified. Short dotted prose can meet `looks_like_a_path`'s extension grammar (`src/commands/send.rs:892-908`); this is a recoverable, documented lexical rule, not a proven bug. `read.rs:228-295` and `chat.rs:114-157` duplicate the mapping; read's ambiguous error remains without a direct binary test.

## Open questions requiring a later test run

1. The four `< READINESS_BOUND / 2` assertions in U1/U2/U5/U11 are 50 ms ceilings (`src/stdin_guard.rs:165,178,218,308`). U9's writer also sleeps 50 ms inside a 100 ms bound (`:255-265`); U10 requires return before its 400 ms producer (`:270-282`). Determine tolerances with an actual loaded-host run and a mutation that removes the wait. Do not promote a guessed new timing number from this review.
2. Run the repaired E1/E2 with an open-stderr banner precondition and then close stderr; swap in std's `eprintln!` in a controlled later mutation to verify both tests go red for the intended reason.
3. Run the new `chat --message` queued-pipe anchor and the folded unit/binary keepers after implementation. A per-call-site mutation should remove only `chat.rs:65` and make that new anchor fail. Recheck `post read`'s ambiguous branch and flag-preserving fix separately; the existing read integration anchor covers queued input only.
4. Check whether `fixture()`'s `--join --backlog` already creates `cursors.json`; if so G12's nonempty cursor-file assertion is vacuous, though its unread-message consumption assertion still binds (`tests/stdin_guard.rs:26-45,365-383`). K8's mode-000 unreadable-mail fixture requires an unprivileged runner. K11's cwd-name leg may be redundant; neither uncertainty justifies a deletion here.
