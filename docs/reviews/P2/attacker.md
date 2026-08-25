# attacker — P2 (omp-3)

=== completionReport ===
ATTACK: held (0) — all boundary transitions, shell quoting sinks, and archive authorization filters held against static adversarial analysis.

*Environment disclosure: Shell/PTY execution tools were unavailable in this delegate session; all findings and proofs are derived from static source inspection and verified control/data-flow tracing across `src/commands/read.rs`, `src/commands/send.rs`, `src/mailbox.rs`, and `tests/cli.rs`.*

### Strongest Attacks Tested

1. **Cross-room archive exfiltration via `--from` / `POST_FROM` spoofing (`src/commands/read.rs:90`)**: Attack died because `post read` evaluates `mail.envelope.to == room || mail.envelope.from == room` against the caller's resolved acting room; `post send` refuses unauthorized reserved room names outside their registered tree via `context.ensure_sender_allowed()`, and `POST_FROM` is a deliberate session-scoped declaration rendered in read provenance rather than an elevated credential.
2. **Command injection via `exact_fix` interpolation and send-receipt backticks (`src/commands/send.rs:315`, `src/commands/read.rs:104,127`)**: Attack died because every dynamic component (`id`, `room`, `prefix`, `path`, `subject`) is escaped with `crate::mailbox::shell_quote()` (POSIX single-quote encapsulation with escaped single quotes), eliminating shell metacharacter injection.
3. **Stdin hijacking / file-shadowing via `--body-file -` sentinel (`src/commands/send.rs:484`)**: Attack died because `path.as_os_str() != "-"` cleanly branches real paths to `read_body_file()` and only routes literal `-` to stdin; tests pin that an existing file path still takes precedence over piped stdin.

---

### Observations & Non-Breach Boundary Notes

- **Readback command mismatch on free-form `--from` aliases (`src/commands/send.rs:315`)**: When `post send --to <to> --from <alias>` is sent from room `alpha` with a free-form alias, the receipt emits `post read '<id>'`. Running that command from `alpha` infers `room = "alpha"`, which fails the archive filter (`to == beta`, `from == alias`), producing `not_found` ("addressed between two other rooms"). The caller must pass `post read '<id>' --room <alias>` to read back.
- **Contract/Schema Description Synchronization (`src/commands/schema.rs:59`, `CONTRACT.md:204`)**: `post read` documentation states archive fallback checks "archive copies addressed to that room", whereas commit `5d7ae30` expanded the filter to `to == room || from == room`.

---

## Delegate Completion Report

- **Status**: completed
- **What was done/found**: Executed red-team adversarial review of commit `5d7ae30`. Evaluated archive filter boundaries, honest error-branch reachability, `--body-file -` stdin sentinel parsing, command injection in `exact_fix` and send receipts, and schema/contract compliance. No exploitable vulnerabilities found.
- **Files reviewed**: `src/commands/read.rs`, `src/commands/send.rs`, `src/commands/chat.rs`, `src/commands/schema.rs`, `src/commands/doctor.rs`, `src/commands/rooms.rs`, `src/mailbox.rs`, `src/cli.rs`, `src/error.rs`, `tests/cli.rs`, `CONTRACT.md`, `CONTRIBUTING.md`, `docs/IDENTITY.md`.
- **Verification**: Complete static data-flow and boundary tracing across all modified and calling modules.
- **Remaining risks or follow-ups**: Consider updating the readback receipt formatter in `send.rs` to include `--room <sender>` when `sender != inferred_room` to ensure copy-paste execution succeeds when custom `--from` aliases are used.
