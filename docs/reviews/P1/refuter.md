# refuter — P1 (codex-8)

=== completionReport ===
Status: completed  
Reviewed: `238c1c4`, parent behavior, `CONTRIBUTING.md`, `CLAUDE.md`, relevant rules and touched files. No files changed.  
Verification: static comparison confirms the three tests discriminate against parent code, but execution and `gate.sh` were blocked by the read-only filesystem.  
Remaining risk: full gate status was not independently reproduced.

VERDICT: refuted (1) — unregistered-cwd errors populate `details.exact_fix` with English prose rather than an executable command.

[SEV-1] Runnable `exact_fix` contract is false · src/channel.rs:222  
Counterexample: From any unregistered cwd, run `post chat some-channel --peek`, then execute the returned `details.exact_fix` with `sh -c`.  
Observed: The field contains prose such as `cd into a registered room and retry, for example ...; or register this one ...`, including backticks, placeholders, and unquoted paths—not a command that runs verbatim as required by `src/commands/schema.rs:267`. The new test merely checks `exact_fix.is_some()` at `tests/cli.rs:1555`, so it passes without exercising the contract. (derived)  
Fix: Keep prose in `suggested_fix`; for cwd identity emit a shell-quoted `post rooms add <inferred-room> <cwd>` as `exact_fix`, and omit `exact_fix` for pinned identity unless a complete command can be produced. Exercise it with `Sandbox::run_fix`.

Position: Keep error-with-exact-fix rather than auto-routing. Routing would silently discard `--kind` or invent channel semantics for it; `--subject` maps, but `--kind` does not.
