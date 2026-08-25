# conventions — P1 (omp-1)

=== completionReport ===
**Delegate completion report**

- Status: completed.
- What I did: conventions review of commit 238c1c4 ("errors: name the directory identity came from, and the verb that works") against the house rules (global CLAUDE.md, repo CLAUDE.md/AGENTS.md, CONTRIBUTING.md, CONTRACT.md, shell-footguns, always-on skills incl. `code-review` and `post`), plus static verification of the parent's claims.
- Files reviewed: `src/channel.rs`, `src/commands/send.rs`, `src/commands/schema.rs`, `src/cli.rs`, `src/error.rs`, `src/mailbox.rs`, `src/commands/profile.rs`, `tests/cli.rs` (targeted), `scripts/gate.sh`, `CHANGELOG.md`, `README.md`, `skills/post/SKILL.md`, `.beads/`.
- Verification: this environment exposes file reads only — no shell — so `git show`, `cargo test`, and `./scripts/gate.sh` could **not** be executed; git objects are zlib-compressed and unreadable via `read`, so the diff was reconstructed from the post-fix tree plus the parent's and papercuts' descriptions of pre-fix behavior. All claim-checks below are static and marked accordingly.
- Remaining risks: the three findings in the review (unquoted `exact_fix` interpolations is the load-bearing one); test-execution claims remain unverified by me.

---

CONVENTIONS: 3 findings

```
[RULE] exact_fix interpolates unquoted paths (and an unquoted pin room name) into a
command the house promises "runs as written" · src/channel.rs:189-193 (pin branch: src/channel.rs:179)
Rule: src/mailbox.rs:538 shell_quote — "Quote `value` for a POSIX shell so suggested/exact
fixes stay executable when names or bodies carry spaces, quotes, or other metacharacters";
pinned as law by tests/cli.rs:5460 crossed_send_exact_fix_shell_quotes_channel_metacharacters
("exact_fix runs verbatim through a shell, so an unquoted name is a command injection");
README.md § Commands: "error.details.exact_fix ... holds a command that runs as written".
Fix: wrap cwd, the example room's registered path, and the pin room name in
`crate::mailbox::shell_quote(...)` in the three fix strings at src/channel.rs:179,189,193
(`cd {quoted-path} && post chat ...`, `post rooms add <name> {quoted-cwd}`), and extend
tests/cli.rs:1523 beyond `exact_fix.is_some()` to pin the quoting with a spaced cwd,
mirroring tests/cli.rs:5460. Note the pin branch is also exposed: validate_component
permits spaces in pin room names (tests/cli.rs:7955).
```

The send-side channel fix got this right (`shell_quote(channel_candidate)`, src/commands/send.rs:174); the identity fix in the same commit did not. A cwd like `~/My Documents/room` yields a broken copy-paste; a cwd or room name carrying `;` or a backtick yields a fix that executes something else. The new test only asserts presence, so the gap ships untested.

```
[RULE] schema's chat usage string still buries the send forms last, undoing the fix on the
surface agents are told to consult · src/commands/schema.rs:53
Rule: CONTRIBUTING.md § Invariant 2 ("post schema is the contract... update the schema... in
the same change") plus the house practice of keeping schema usage byte-aligned with the CLI
(tests/cli.rs:362-364 pins watch.usage exactly; skills/post/SKILL.md: "Use `post schema
--pretty` as the exact contract when docs or memory disagree"). The commit reordered the
--help usage (src/cli.rs:126-142) and left the schema's mirror of it in the old reads-first
order.
Fix: reorder the usage literal at src/commands/schema.rs:53 to lead with the three --send
forms, matching src/cli.rs:127-141.
```

The papercut being fixed (pc2_9a7deeb69ac2a641: help scans as read-only) applies verbatim to `post schema` output — the machine-facing contract an agent consults when help is ambiguous — and that surface still reads reads-first.

```
[RULE] CHANGELOG Unreleased carries no entry for three user-visible error/help changes ·
CHANGELOG.md:3-9
Rule: repo changelog practice — the immediately preceding commit (d8c684c, --digest) added
its Unreleased entry in the same change, and 0.6.0 logs comparable UX deltas
("--discard-through text receipts now report how many additional messages...").
Fix: add one Changed bullet under Unreleased naming the three surfaces (identity error,
channel-reaches---to error, chat help ordering).
```

**Claims under attack — static verification** (no shell available; nothing was executed):

- *Tests discriminate*: plausible, but unverifiable by execution here. Statically, each of the three asserts post-fix-only structure: `unregistered_cwd...` (tests/cli.rs:1523) requires the full cwd in the message — the papercut-quoted pre-fix text carried only the basename; `send_to_a_channel...` (tests/cli.rs:1559) requires the literal "is a channel, not a room" (src/commands/send.rs:179); `chat_help_leads...` (tests/cli.rs:1628) requires `--send` within the first four usage lines (src/cli.rs:127-141). All three would fail against the pre-fix behavior as described [INFERENCE]. No other test pins the old text — the adjacent pinning tests (`unknown_room_has_a_did_you_mean...` tests/cli.rs:1497; `chat_acting_room_honors_registered_pin...` tests/cli.rs:8032) assert substrings that hold post-fix.
- *`--to '#tax'` strips the sigil*: confirmed — `strip_prefix('#')` at src/commands/send.rs:168; the fix uses the stripped name shell-quoted at 173-175; the test asserts the fix contains no `'#tax'` (tests/cli.rs:1595-1598).
- *Typo still takes did-you-mean*: confirmed — the channel branch requires `ChannelPaths::exists()` (src/commands/send.rs:169-171); `claude-spac` is no channel, so it falls through to `closest_room`; test asserts `did_you_mean == "claude-space"` (tests/cli.rs:1601-1622).
- *Bound at 8 with `+N more`, full set in `details.matches`*: confirmed in code — `ROOM_LIST_PREVIEW = 8` (src/channel.rs:27), suffix at src/channel.rs:198-205, `.matches(names)` carries the complete list (src/channel.rs:224). **Gap**: no test exercises >8 rooms, so the bound and suffix are unpinned behavior.
- *gate.sh runs all seven CONTRIBUTING checks*: confirmed by reading `scripts/gate.sh` (fmt, clippy, test, release build, node hooks, node launcher, schema — all seven, plus toolchain attribution). Its current exit status is **unverified** (not executed).
- *Invariant 2 — no error code or envelope shape change*: holds. `ErrorCode::ALL` is unchanged at 14 codes (src/error.rs:66-82); both new errors reuse `unknown_room`/exit 65; `matches`, `exact_fix`, `did_you_mean`, `input`, `reason` are pre-existing `ErrorDetails` fields (src/error.rs:6-35) already consumed by the ambiguous-id, crossed-send, and unknown-room paths; the schema pins codes, usage strings, and shapes — not message text. (Finding 2 is the mirror-alignment residue of this same invariant.)

**The open disagreement — I take your side.** Route-on-`#channel` is wrong for this CLI. `--kind` cannot map at all: CONTRACT.md law 5 makes kind structurally impossible in a channel, so a router must either drop the sender's explicit `--kind` (a lie) or error on it — and once it must error on some flags anyway, the honest uniform behavior is the verb-correcting error for every channel destination. Beyond `--kind`: `--allow-self` self-mail semantics, the blocked-route check placement, the `crossed_send` unread guard, and own-message seen-marking are send-path semantics a direct-mail invocation never opted into; silently acquiring them is a contract change with no flag expressing it. The only defensible router variant — route when no direct-only flags are present, error otherwise — makes the same destination behave differently depending on flags, which is worse for a contract CLI whose schema would then have to document the coercion. Error-with-exact-fix is the defensible position; 70% confidence is if anything low.

**Notes.** The untracked `.beads/.auto-import-issues.jsonl` contains only a size/mtime marker record; nothing to review. The commit subject (from the reflog) is 58 chars, within the 72-char rule; the body's soft-wrap compliance is unverifiable without git object access. Skills reviewed: the always-on `code-review` skill's two-axis process is superseded by the parent's mandated report shape; `post` skill used for domain context.
