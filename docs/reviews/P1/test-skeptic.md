# test-skeptic — P1 (cursor-1)

=== completionReport ===
I'll start by loading the required project docs and review skills, then inspect commit `238c1c4` read-only.Persona and house rules are loaded. Next I’ll read CONTRIBUTING, the commit, and the new tests before the implementation.Shell was blocked; retrying the commit inspection with smaller, read-only commands.Shell is blocked, so I’m reconstructing the commit from the tests and implementation, then checking schema and the untested claims.Shell is unavailable in this isolation boundary, so the review is static: tests first, then implementation, schema, and contract.

**Status:** completed  
**What I did or found:** Test-skeptic review of commit `238c1c4`. The three new CLI tests mostly pin customer-visible behavior and would fail the described pre-fix messages/help; two assertions are decoration, and the channel-fix test never runs the command it names. Error codes and envelope shape look unchanged. I side with error-with-exact-fix over routing.  
**Files changed or reviewed:** `tests/cli.rs`, `src/channel.rs`, `src/commands/send.rs`, `src/cli.rs`, `src/commands/schema.rs`, `src/error.rs`, `src/mailbox.rs`, `src/app.rs`, `CONTRIBUTING.md`, `CONTRACT.md`, `scripts/gate.sh`, `CHANGELOG.md`, `docs/IDENTITY.md`. No files edited.  
**Verification run and result:** Shell was rejected (`pwd`, `git show`, `cargo test`, `./scripts/gate.sh`). No live run of the three tests, the other 162, clippy, or gate. Pre-fix discrimination is from tracing assertions against the current error/help strings.  
**Remaining risks:** `ROOM_LIST_PREVIEW` (8 / `+N more`) has no test; `post schema` chat usage still leads with read forms; exact_fix for channel mis-address uses a `'<text>'` placeholder so even `run_fix` would not replay the original body.

VERDICT: behavioral (3) — `send_to_a_channel_names_the_channel_verb_and_the_fix_runs` names a run it never performs, and its sigil assert still passes an unquoted `#tax` that `sh` would comment-eat.

```
[MIRROR] send_to_a_channel_names_the_channel_verb_and_the_fix_runs · tests/cli.rs:1561
What it pins: `--to tax` and `--to #tax` both refuse with unknown_room and prose "is a channel, not a room"; `claude-spac` still gets did_you_mean=claude-space. The loop over both spellings does discriminate sigil-stripping for *lookup* (unstripped `#tax` would miss `channels/tax/channel.json` and fall through to the typo path).
Why it fails us: the name says the fix runs; the body never calls `Sandbox::run_fix` (tests/cli.rs:100), which this suite already uses to pin "exact_fix runs verbatim" (tests/cli.rs:3238, tests/cli.rs:8514). Schema law: exact_fix is a complete command that runs as written. Here it is only `contains("post chat")`. A template that is not argv, not quoted, or that names a non-existent channel still passes.
Fix: `run_fix` the exact_fix from the room cwd and assert a channel message lands (body may be the documented `'<text>'` placeholder — then assert *that* body, not `"x"`).
```

```
[DECOR] send_to_a_channel_names_the_channel_verb_and_the_fix_runs · tests/cli.rs:1601
What it pins: exact_fix does not contain the six characters `'#tax'`.
Why it fails us: `post chat #tax --send --body '<text>'` (hash unquoted) does not contain `'#tax'` and still passes. In a POSIX shell `#` starts a comment, so that "fix" becomes `post chat`. Identification of `#tax` as a channel is already pinned above; this assert does not pin stripping *in the executable command*.
Fix: assert the fix contains `'tax'` (shell_quote form) and does not contain `#`, or just run it.
```

```
[DECOR] unregistered_cwd_names_the_directory_and_lists_the_rooms_that_exist · tests/cli.rs:1555
What it pins: `chat --peek` from the sandbox cwd is unknown_room, the human message contains the full cwd string, and details.matches is exactly [agent-memory, claude-space, pact].
Why it fails us: `exact_fix.is_some()` survives any non-empty string, including a command that does not `cd` anywhere. The inline 8-name bound / `+N more` in src/channel.rs:30 and src/channel.rs:199 is untested (fixture has three rooms), so a dump of every registered room in the message stays green — the papercut this bound exists to prevent.
Fix: `run_fix` from that cwd, or assert exact_fix contains a registered path from matches; add a >8-room fixture that checks the message has `+N more` while matches.len() is the full set.
```

The rest earns its keep. `unregistered_cwd`’s `message.contains(&cwd)` would fail the old basename-only line (`cwd resolves to 'nested'`), because the basename is not the full path. `chat_help_leads_with_a_send_form` (tests/cli.rs:1628) would fail read-first usage: clap’s default synopsis is `post chat [OPTIONS] <CHANNEL>`, which has no `--send` in the first four Usage lines. `unknown_room` / exit 65 on the channel-misaddress path pins the “no new code” choice.

Invariant 2: no new error code, no new details field, no envelope-shape change. Channel misaddress and unregistered acting-room still emit `unknown_room` (exit 65) and fill already-optional `input` / `matches` / `exact_fix` / `reason`. `src/commands/schema.rs` error list is still `ErrorCode::ALL`. Clap `override_usage` is not the schema; `post schema` chat usage (src/commands/schema.rs:54) still leads with `[--peek | --limit | --history | --since]`. That is not an envelope break, but it is the contract-reading surface for papercut 3 and this change does not touch it.

Claims I could check without a runner: tests/cli.rs has 165 `#[test]` functions; three new leaves 162 others, which matches the author’s count for that file (not the whole crate). Pre-fix fail for those three follows from the strings above. `scripts/gate.sh` is the seven CONTRIBUTING.md steps. `--to '#tax'` identification is pinned; `--to claude-spac` did-you-mean is pinned; the 8-name bound is not.

Routing: keep the error. Channel messages carry no kind (schema law); chat has no `--from` / `--allow-self`; membership and `crossed_send` (default off, `--anyway` to skip) are send-path semantics mail does not have. A router that *works* only for the flag subset that is already `post chat --send` is a second, implicit chat interface inside `send`. For the rest it must refuse, not drop: `--kind` (no channel field — dropping lies), `--from` (cwd identity; a pin-disagree is already a hard conflict on send), `--allow-self` (no from==to). `--subject` / `--oversize` / body flags could map; `--anyway` would stay off, so a routed send can bounce `crossed_send` after the caller thought they used mail. That is a worse papercut than one error whose exact_fix is the other verb. If routing is ever built, refuse unmapped flags loudly; do not drop them.
