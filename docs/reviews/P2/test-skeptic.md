# test-skeptic — P2

_lane: test-skeptic:cursor: · alias cursor-2 · exit 0 · 7159 bytes · review-shaped: yes_

=== completionReport ===
I'll load the review and workspace skills first, then reproduce the parent-commit defects and inspect the tests in `5d7ae30` before judging them.Skills loaded: test-skeptic persona, review-agent (read-only, no invented findings), CONTRIBUTING invariant 2. Next I’ll reproduce the parent defects and read the new tests before the implementation.Shell was rejected on the first attempt; I’ll retry with a narrower git show and then fall back to static review if it stays blocked.Git and shell are blocked here, so I’ll reconstruct the commit from the tests and implementation, then check schema, identity, and the archive-visibility claim statically.**Status:** completed  
**What I did or found:** Static test-skeptic review of `5d7ae30`. The three new tests pin customer-visible behavior, not mock-shaped internals. `from == room` does not leak third-room mail at the CLI; schema/CONTRACT still describe the pre-fix filter.  
**Files reviewed:** `tests/cli.rs` (new tests + helpers), `src/commands/read.rs`, `src/commands/send.rs`, `src/commands/chat.rs`, `src/commands/schema.rs`, `src/mailbox.rs`, `src/cli.rs`, `src/error.rs`, `CONTRIBUTING.md`, `CONTRACT.md`, `docs/IDENTITY.md`, `README.md`, `CHANGELOG.md`. No files edited.  
**Verification:** Shell was rejected (`git show`, `cargo test`, `./scripts/gate.sh`). Git objects unreadable. All discrimination claims are static, marked below.  
**Remaining risks:** `--from` free-form send still makes the new receipt/`post read` lie to the sending cwd; schema/CONTRACT still say “addressed to that room”.

VERDICT: behavioral — no theater; worst residue is schema/CONTRACT still publishing the pre-fix archive filter, which these tests never pin.

The three new tests earn their keep. They pin: sender can read a just-sent archive id and see the body; a third room is refused without the old “not in the archive” lie; a truly absent id still uses that wording; the text receipt’s backticked command is extracted and executed until the body comes back; `--body-file -` takes piped stdin; a real `--body-file` path still wins over stdin.

```
[static] a_sender_can_read_back_its_own_archived_mail_and_a_stranger_still_cannot · tests/cli.rs:8972
What it pins: JSON `archived: true` plus cwd-alpha `post read <id>` returning the body; beta still succeeds; gamma exits 66/`not_found` with “addressed between two other rooms” and without “not in the archive”; a synthetic absent id still contains “not in the archive”.
Why the mutations hold: parent filter `to == room` only → alpha’s read dies on the old tail (red). Serving the body from archive, sender `read/`, or any other store still passes. Opening archive to every room fails the gamma half. Routing the honest tail onto a missing id fails the last assert. The positive phrase is copy-locked; the load-bearing half is the negative plus the absent-id split.
```

```
[static] the_send_receipt_names_a_readback_command_that_runs · tests/cli.rs:9033
What it pins: a stdout line containing “read it back with”, a command taken from between backticks, `Sandbox::run_fix` (tests/cli.rs:100), and the body “hi”.
Why the mutations hold: parent receipt has no such line → `expect` fires. A string that looks like `post read` but does not run fails `run_fix`. A correct receipt that used `--room` or a quoted id still passes if `sh -c` can execute it. This is not `contains("post read")`.
```

```
[static] body_file_dash_reads_stdin_and_a_real_path_still_wins · tests/cli.rs:9059
What it pins: `--body-file -` plus piped stdin delivers that pipe; `--body-file <real>` plus a different pipe delivers the file and not the pipe.
Why the mutations hold: parent `read_body_file("-")` is NotFound → `from_stdout` never sees a send. Concatenating file+stdin or letting any stdin steal the file branch fails the second half. The second half would already have been green on `a8383e6`; it is the complementary pin that the new sentinel does not eat a real path. Chat uses the same `send::read_body` (src/commands/chat.rs:960) and is untested; same function, not a second implementation.
```

**Leak claim (attacked, static).** Admitting `from == room` does not let acting-room gamma read alpha→beta mail. The filter runs *after* room resolution (`--room` > `POST_FROM` > cwd). `--room` already lets any caller act as any room; archive was never an authz boundary against that. Claiming a *registered* `from` at send is `ensure_sender_allowed` (cwd tree) except `POST_FROM`, which bypasses the tree *and* is the acting room on read (`resolved_room`, src/mailbox.rs:301–327) — that is declaring you are that room, not forging past it. `rooms add` cannot overwrite a name or rebind a path. Free-form `--from` is forgeable; that writes a new envelope, it does not reveal two other rooms’ mail. Hand-edited `archive/*.mail` is FS write; the reader who can plant `from` could already read the bytes. The author’s “sender authored the body” is the wrong reason; the right one is “party string after an unauthenticated room pick.” CONTRACT.md:202, schema.rs:74 and :264, and the new “this room may not read it” copy do treat the filter as a visibility boundary — and still describe only `to == room`.

**Honest vs ordinary miss.** Sufficient for full ids: present-but-not-party vs nonexistent. `archived_elsewhere = party.is_empty() && !candidates.is_empty()` (read.rs:113) keeps a missing id on the old tail. Not pinned: prefix-only, unparseable archive files (parse failure drops them from `party` and can falsely take the “two other rooms” branch), or send with `--from` ≠ acting room.

**`--from` hole the tests do not own.** Default send sets `from` to the cwd room, so `post read <id>` from that cwd works. `cd alpha && post send --to beta --from alice` stamps `from=alice`; the new receipt still says ``post read <id>``; alpha’s read then hits the honest branch and is told the file is “addressed between two other rooms” — a new lie, same class as papercut 1. `shell_quote` on the generated id does not fix that.

**exact_fix injection.** Not reintroduced. Receipt is human stdout, not `details.exact_fix`: `post read {shell_quote(id)}` (send.rs:349–356). Id is `YYYYmmdd-HHMMSS-` + 6 hex (mailbox.rs:957–961). Read miss path: `post inbox --room {shell_quote(room)}` (read.rs:120). No unquoted path interpolation of the a8383e6 kind.

**Invariant 2.** Author’s belief holds for codes and JSON shape: still `not_found`/66; `ErrorCode::ALL` length 14; send JSON still `ok, envelope, archived`; `already_read` unchanged; `details.reason` is the existing optional string. Text send stdout gained a second line (CONTRACT.md:177 still quotes only the first). Read *behavior* changed; schema.rs:74, schema.rs:264, and CONTRACT.md:202 still say “archive copies addressed to that room.” No schema pin was added (`help_and_schema_keep_command_contract_visible` does not touch read.side_effects). That is a contract lie, not test theater.

**Red-then-green at `a8383e6` → `5d7ae30`.** Not executed. Static: all three tests depend on strings/control flow that only exist post-fix (sender archive admit, receipt line, `-` short-circuit before `read_body_file`).
