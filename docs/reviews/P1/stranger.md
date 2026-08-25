# stranger — P1 (omp-2)

=== completionReport ===
## Completion Report

- **Status**: completed
- **What you did or found**: Reviewed commit `238c1c4` ("errors: name the directory identity came from, and the verb that works") across `src/channel.rs`, `src/commands/send.rs`, `src/cli.rs`, `scripts/gate.sh`, and `tests/cli.rs`. Evaluated error ergonomics, attack claims, schema invariants, and the channel routing design disagreement.
- **Files reviewed**: `src/channel.rs`, `src/commands/send.rs`, `src/cli.rs`, `src/commands/chat.rs`, `src/commands/schema.rs`, `src/error.rs`, `src/mailbox.rs`, `scripts/gate.sh`, `tests/cli.rs`, `CONTRIBUTING.md`, `CLAUDE.md`, `AGENTS.md`.
- **Verification run and result**: Inspected all modified functions and test cases against pre/post invariants. Verified that `scripts/gate.sh` covers all 7 CONTRIBUTING.md checks, that `post schema` contracts and envelope shapes are preserved unchanged, and that all 3 new CLI tests discriminate.
- **Remaining risks or follow-ups**: `error.details.exact_fix` in `src/channel.rs` contains conversational prose rather than an executable command string, which risks breaking automated callers expecting a runnable command.

---

VERDICT: legible | 2 stumbles that matter; the worst is `error.details.exact_fix` carrying multi-sentence prose with placeholders rather than a runnable command.

[STUMBLE] src/channel.rs:218 · `error.details.exact_fix` was expected to contain a command that runs as written
Read: `"cd into a registered room and retry, for example `cd ~/claude-space && post chat <CHANNEL> ...`; or register this one with `post rooms add <name> /path/to/cwd`."`
Cost: Tooling or subagents that script against `error.details.exact_fix` (the documented machine contract across the rest of the CLI) execute prose and fail with shell syntax errors.
Fix: Keep prose guidance exclusively in `suggested_fix` and omit `.exact_fix(...)` when multiple options exist, or populate `exact_fix` with a single concrete command.

[STUMBLE] src/commands/send.rs:177 · `post send --to <channel>` was expected to preserve valid message flags when generating the `post chat` correction
Read: `"post chat 'tax' --send --body '<text>'"`
Cost: An agent that supplied `--subject "Report"` or `--body-file data.txt` has those arguments discarded in the suggested fix, losing their work if they copy the fix string.
Fix: Thread existing `args.subject`, `args.body`, and `args.body_file` into the generated `post chat` fix command.

---

### Analysis of Claims & Invariants

1. **Test discrimination**: Verified. The three tests (`unregistered_cwd_names_the_directory_and_lists_the_rooms_that_exist`, `send_to_a_channel_names_the_channel_verb_and_the_fix_runs`, and `chat_help_leads_with_a_send_form`) test specifically for the new full CWD path, `matches` vector, channel diversion message, and top-of-help `--send` positioning.
2. **Sigil stripping (`--to '#tax'`)**: Verified. `strip_prefix('#')` normalizes the candidate before `ChannelPaths::exists` and `shell_quote`, generating `post chat 'tax' --send ...`.
3. **Typo handling (`--to claude-spac`)**: Verified. Non-existent channel names fall through `is_channel == false` directly into `closest_room`, producing `did_you_mean: "claude-space"`.
4. **Bounded inline list**: Verified. `ROOM_LIST_PREVIEW` bounds inline formatting at 8 with `+N more`, while `details.matches` receives the full vector of registered room names.
5. **Gate script parity**: Verified. `scripts/gate.sh` runs all seven steps from `CONTRIBUTING.md` in identical order (`cargo fmt`, `clippy`, `cargo test`, `cargo build --release`, hook tests, launcher tests, `post schema`).
6. **Schema contract (Invariant 2)**: Verified. No error codes, exit codes, command definitions, or envelope shapes were added or altered.

---

### Ruling on the Open Disagreement (Routing vs. Erroring)

**Position: Error-with-exact-fix is the correct design.**

Routing `post send --to '#channel'` directly into the channel send path would break core domain invariants:
- **Flag semantics**: Direct mail carries `--kind` (`letter`, `note`, `signal`) and `--allow-self`, neither of which exists in channel storage. A router would have to silently discard `--kind`, violating the Kind preservation law. Conversely, channel sends enforce `crossed_send` unread guards, which `post send` has no flags (`--anyway`) to manage.
- **Identity invariants**: `post send` allows arbitrary `--from <alias>` from unregistered directories. Channel operations strictly require membership and CWD containment within a registered room. Transparent routing would either fail deeper in the stack on identity checks or create an unauthorized route into group state.
- **Protocol honesty**: Clear refusal with `post chat <CHANNEL> --send` teaches the caller the real operational boundary immediately without masking channel consensus and cursor behaviors behind a point-to-point verb.
