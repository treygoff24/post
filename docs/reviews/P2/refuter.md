# refuter — P2

_lane: refuter:codex:sol · alias codex-9 · exit 0 · 3784 bytes · review-shaped: yes_

=== completionReport ===
Status: completed  
What I found: 4 counterexamples, including two SEV-1 defects.  
Files reviewed: `src/commands/read.rs`, `src/commands/send.rs`, `src/commands/schema.rs`, `src/mailbox.rs`, `tests/cli.rs`, `CONTRIBUTING.md`, `CLAUDE.md`, `CONTRACT.md`, `README.md`, `docs/IDENTITY.md`. No files changed.  
Verification: `cargo fmt --check` and `git diff --check` passed. Shell worked, but Cargo tests were blocked because the read-only filesystem prevented creating `target/`. Parent-red/current-green behavior was therefore derived statically, not executed.  
Remaining risks: Full tests, Clippy, and release gate remain unexecuted.

VERDICT: refuted (4) — a supported send from room `alpha` using `--from alpha-worker` prints a `post read <id>` receipt that exits 66 when executed from the same room.

[SEV-1] the receipt command does not run for supported free-form senders · src/commands/send.rs:349  
Counterexample: From registered room `alpha`, run `post send --to beta --from alpha-worker --body hi`, then execute the emitted `post read '<id>'` without changing cwd.  
Observed: Read resolves room `alpha`, but the archive envelope has `from=alpha-worker` and `to=beta`; the new filter rejects it and returns not-found. The execution test covers only cwd-inferred `from=alpha`. (derived)  
Fix: Emit an executable command naming the effective archive reader, such as a shell-quoted `--room <envelope.from>`, and add an executed receipt test using free-form `--from`.

[SEV-1] `post schema` was not updated for changed command and flag semantics · src/commands/schema.rs:48  
Counterexample: A client reads `post schema`, which still describes `--body-file <path>`, says nonexistent body-file paths are usage errors, and says archive fallback covers copies addressed to the room; it then passes `--body-file -` or reads sender-authored mail.  
Observed: `-` now means stdin for both send and chat, while sender-side archive visibility is also new; commit `5d7ae30` makes no schema change or pinning-test change. This violates `CONTRIBUTING.md` invariant 2 even though error codes and JSON envelopes stayed unchanged. (derived)  
Fix: Update send/chat body-source descriptions and read/archive laws in `post schema`, then add pinning tests.

[SEV-3] `from == room` does not prove that the reading room authored the body · src/commands/read.rs:108  
Counterexample: Room `alpha` sends to `beta` using the currently unregistered free-form sender `courier`; later, a different workspace is registered as room `courier` and reads that archived id.  
Observed: Free-form `from` is accepted without ownership, room registration reserves no historical aliases, and the new string comparison gives the later `courier` room Alpha’s body. `POST_FROM` is likewise documented as a declaration rather than a credential. (derived)  
Fix: Persist the actual acting room separately from self-declared `from` and filter on that, or explicitly define archive filtering as convenience rather than a no-leak boundary.

[SEV-3] the honest miss branch still asserts an unproved archive relationship · src/commands/read.rs:113  
Counterexample: Put a matching `.mail` file in `archive/` with malformed envelope JSON or a filename/envelope-id mismatch, with no inbox/read match, then run `post read <prefix>`.  
Observed: `parse_mail(...).is_ok_and(...)` discards the parse failure, after which nonempty `candidates` sets `archived_elsewhere=true`; the command falsely says the message is “addressed between two other rooms” instead of reporting invalid mail state. The test covers only a valid foreign message and a completely absent id. (derived)  
Fix: Parse each archive candidate once and propagate parse errors before classifying valid candidates as local or elsewhere.
