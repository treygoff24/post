# Adversarial plan review: Plan B, stateful read layer

Reviewer: fresh-context native (claude opus), 2026-08-31. Subject:
`docs/plans/plan-b-stateful-read-layer.md` at `aadf818`. Read against the goal
lock, the architecture, the seam map, and the 0.8.0 checkout. Every anchor below
was opened; nothing here is inferred from the recon docs alone.

## Verdict

**SHIP WITH FIXES.** The design is right and the recon under it is unusually
honest. The plan is not executable as written: wave 2 stalls. Four of the eight
`owned_files` lists omit files their task must edit, and three of those files are
edited by two wave-2 lanes at once. Every blocker below is fixed by editing the
plan document, not by redesigning anything.

The single most dangerous finding is #2, because it is the only one that ships
green. The rest go red and stop.

## Findings

### 1. Blocker: `tests/cli.rs` is a shared file for B3 and B6, and neither owns it

`tests/cli.rs` appears in `owned_files` for B1 (wave 1) and B7 (wave 3) only. Two
other tasks must edit it:

- **B3** rewires consumption from `channel-state.json` to `cursors.json`. Two
  existing tests read that file and hard-fail when it stops being written:
  `tests/cli.rs:4064-4067` does
  `fs::read(...join("beta").join("channel-state.json")).expect("read channel state")`
  then asserts `state["version"] == 2`, and `tests/cli.rs:5049-5053` does
  `fs::read_to_string(active.mail_root.join("dest/channel-state.json")).expect("advanced channel cursor").contains(...)`.
  Both panic on a store that no longer has the file. B3's verify row is
  `cargo test`, so B3 goes red in wave 2 with no owned file to fix.
- **B6** adds `catchup` and `search` to `schema.rs`.
  `tests/cli.rs:352-360` asserts
  `assert_eq!(command_names, expected_commands)` against a hardcoded
  twelve-name list. B6 owns `src/commands/schema.rs` but not `tests/cli.rs`.

Three more assertions go quietly vacuous rather than red, which is worse:
`tests/cli.rs:4002-4009` falls back to `b"{}"` on read failure and then asserts
`state.get("tax").is_none()`, which is trivially true once the file is gone;
`tests/cli.rs:6437-6444` is guarded by `if state_path.exists()`;
`tests/cli.rs:4935` and `tests/cli.rs:5383` assert the *absence* of
`channel-state.json` under a fence, which after the rewire proves nothing about
the file the fence now has to protect.

**Fix:** give B3 `tests/cli.rs`, move B7 to a later wave or split the file, and
add an explicit line to B3's `delivers`: retarget the five existing
`channel-state.json` assertions at `cursors.json`, including the two absence
assertions at 4935 and 5383. Add `tests/cli.rs` to B6's `owned_files` and name
the twelve-command list at 352-360 in its `delivers`.

### 2. Blocker: nothing owns `src/commands/watch.rs`, and the watch startup floor reads the old file

`src/commands/watch.rs:2` is `use crate::channel_state::ChannelState;`.
`src/commands/watch.rs:277` populates each `WatchTarget` with
`channel_seen: load_channel_seen(context, &room)`, and
`src/commands/watch.rs:960-966` implements that as `ChannelState::load(context, room)`.
That is the doorbell's startup backlog floor.

B1 keeps `channel_state.rs` as thin wrappers, so watch reads the new state
through the shim and behaves correctly. B3 then "deletes the wrapper shims"
(B3 `delivers`). The plan never says which of the seven public functions
(`load`, `has_seen`, `max_seen`, `into_channels`, `mark_seen`,
`mark_seen_through` at `src/channel_state.rs:92-168`, and
`stored_shape_is_valid` at `src/channel_state.rs:420`) are shims. Two outcomes,
both bad:

- B3 deletes `ChannelState::load`. `watch.rs` and `doctor.rs:382` stop
  compiling. B3 owns neither, and B7 asserts "src/commands/watch.rs is not
  modified by this or any Plan B task." B3 stalls.
- B3 leaves `load` alone. Watch's floor is now the frozen legacy
  `channel-state.json`, which B1's one-way materialization never writes again.
  A watch started after any catchup replays the entire channel backlog as
  rings. This contradicts architecture §3.3 bullet 3 and seams.md danger item 3.

**No verify row catches the second case.** B7's two doorbell directions both arm
the watch *before* the catchup (plan B7 `delivers`; architecture §7.2), so the
floor is loaded before cursor state moves. There is no "start a watch after a
catchup" test anywhere in the plan or the acceptance demo.

**Fix:** three parts. (a) Add a table to the Interfaces section listing each
`channel_state` public function as *shim, deleted in B3* or *survives*, with its
caller. `stored_shape_is_valid` must survive for `doctor.rs:382` and for the
`cursor_state.<room>.legacy` check the architecture wants at §6.3. (b) Add a
third doorbell direction to B7: catchup, then start a fresh watch, and assert it
does *not* replay the caught-up backlog. (c) Either give some task
`src/commands/watch.rs` or state in B7 that `load_channel_seen` reads the new
state through a surviving shim, which is what makes the no-modification claim
true.

### 3. Blocker: `src/output.rs` is owned by B4 alone and needed by B2 in the same wave

Every command's output struct lives in `src/output.rs`: `SendOutput:25`,
`ChatReadOutput:181`, `ChannelListItem:200`, `ChannelsOutput:212`,
`InboxOutput:470`, `ReadOutput:513`, and `OutputShapes:561-577`.
`tests/cli.rs:1-5` imports the typed shapes from `post::output`, and
`src/lib.rs:11` is `pub mod output` while every other module is private.

B2 delivers a `{ok, room, targets[], count}` catchup envelope and a
`tests/catchup.rs` suite. For that suite to deserialize the envelope the way
every existing test does, the struct must be `pub` in `post::output`. B2 does
not own `src/output.rs`; B4 does, in the same wave. B5 and B6 have the same
problem in later waves: B5 needs a search envelope, and B6 needs two new
`OutputShapes` fields at `src/output.rs:561-577`, which architecture §6.2 names
explicitly.

**Fix:** move the new output structs out of B4's exclusive claim. Cleanest is to
have B1 (wave 1, already touching `src/lib.rs`) pre-declare empty
`CatchupOutput`, `SearchOutput`, and the two `OutputShapes` fields, then let B2,
B4, B5, and B6 each own only their own struct. Failing that, add `src/output.rs`
to B2's list and move B4 to wave 3.

### 4. Blocker: `tests/common/mod.rs` extracts the wrong slice, and three wave-2 lanes need more of it

B1 extracts "the `Sandbox` helper (extracted unchanged from
`tests/cli.rs:19-225`)". That range is exactly `struct Sandbox` (19),
`impl Sandbox` (25), and `impl Drop` (212), which is correct as far as it goes.
Everything the new suites actually need to build a fixture is somewhere else:

`seed_fence_store:3255`, `seed_channel_fixture:3274`,
`fence_under_external_lock:3295`, `write_reference_mail:3339`,
`write_custom_mail:3351`, `register_alpha_beta:3367`, `register_room:3385`,
`join_channel:3390`, `write_channel_message:3395`, `write_bad_channel:3425`,
`post_command:3447`, `assert_success:3456`, `assert_migration_refused:3476`,
`from_stdout:3498`, `from_stderr:3508`.

`tests/catchup.rs` cannot register a room or join a channel; `tests/counts.rs`
cannot either; `tests/search.rs` needs channel and mail fixtures; `tests/consuming.rs`
needs the mail writers. Each would have to add to `tests/common/mod.rs`, which
only B1 owns, and B2, B3, and B4 are the same wave.

**Fix:** widen B1's extraction to `tests/cli.rs:19-225` plus `3255-3520`, and say
so in B1's `delivers` by function name so the lane cannot guess.

### 5. Blocker: `-D warnings` plus a shared test module is a deterministic clippy failure

Every task's verify list includes
`cargo clippy --all-targets --all-features -- -D warnings`. Rust compiles
`mod common;` separately into each integration-test crate, so every helper a
given suite does not call is `dead_code` in that crate, and `-D warnings` turns
that into an error. With six suites (`catchup`, `consuming`, `counts`, `search`,
`doorbell`, `schema_surface`) sharing fifteen-plus helpers, this fires on the
first suite that lands and again on every one after it. The fix lives in
`tests/common/mod.rs`, which only B1 owns.

**Fix:** B1's `delivers` states that `tests/common/mod.rs` opens with
`#![allow(dead_code)]`. One line, in wave 1, before three lanes trip on it.

### 6. Major: `channels.room` is specified two different ways in the same plan

Plan Interfaces line 46 and B4 `delivers`: "`channels` items gain `room` and
`unread: number|null`", which puts `room` on each item.
Architecture §4.1 and B6 `delivers` (via architecture §6.2): "`channels.room`,
`channels[].unread`", which puts `room` at the top level. `room` is a single
acting identity resolved from cwd (`channels --room` is rejected outright, exit
2, `tests/cli.rs:3725-3731`), so per-item is redundant on its face.

B4 builds one shape in wave 2; B6 advertises the other in wave 4; B6's own
`schema_surface` test ("every advertised field exists in real CLI output and
vice versa") then forces B6 to advertise whatever B4 shipped, and the contract
lands wrong rather than the run stopping.

**Fix:** pick one in the Interfaces section. Recommend top-level `room`,
per-item `unread`, and edit B4's `delivers` to match.

### 7. Major: goal-lock ruling 4 (watch body previews) has no home in the plan

Goal lock line 54 lists rulings 1 through 5 as settled, including "previews in".
`HANDOFF.md:60-61` is specific: "**Body previews in watch lines: yes** (Trey
ruled directly), capped ~80 chars, sanitized, untrusted-framed." That is a Trey
ruling, not a coordinator one. It does not exist in 0.8.0: `grep -n preview
src/commands/watch.rs` returns nothing.

The plan delivers nothing for it, and its scope fence forbids the only file it
could live in. The goal lock is itself inconsistent here, because §5 rules out
"any change to default `watch` behavior", and a preview on a ring line is one.
The plan resolved that conflict silently by dropping the ruling.

**Fix:** not a redesign. Add one line to the scope fence: previews are parked to
bead `post-<new>` because the lock's own scope fence excludes `watch.rs`, and
file the bead. A ruling that reaches neither the plan nor the ledger was a
decision made in secret.

### 8. Major: B6 is the contract task and gets the lightest review of the contract-bearing tasks

`references/review-regimes.md:11`: "Credentials, schema, process, or filesystem
effects derive critical." B6 amends `CONTRACT.md`, `src/commands/schema.rs`, and
`src/commands/doctor.rs`, and declares `effects = ["code", "docs"]` with no
`[review] tier` override. That derives **standard**: two lanes, two families,
`severity_floor = "major"`. B1 and B3 both carry `tier = "critical"`.

The goal lock singles out this exact diff: "Contract seam changes (CONTRACT.md
amendment, schema output) reviewed as their own diff, not folded into feature
commits" (goal lock §6). The plan under-prices the one thing the lock named.

**Fix:** add the schema effect to B6's `effects`, or set `[review] tier = "critical"`
with the goal-lock §6 citation as the reason. Same question applies to B2 and
B5, which each add a command and a JSON envelope.

### 9. Major: the framing rule is implemented two waves before it is decided

Goal lock §6, last bullet: "Open contract question riding along, **not
pre-decided**: CLI read-time framing (banner diet layer), resolved during the
contract pass." Architecture §3.2 labels its own rule "**Framing proposal (open
contract question, not a locked ruling)**". The plan's Interfaces section then
pins it as settled: "Both accept `--framing auto|full|compact` per architecture
§3.2/§5.1."

B2 (wave 2) and B5 (wave 3) implement it and freeze tests around it. B6 (wave 4)
"resolves" it. If the resolution differs, B6 owns neither `catchup.rs` nor
`search.rs`.

There is a live wrinkle the contract pass has to settle, and it is not written
down anywhere: existing `--framing auto` on text chat stamps banner-day state
(`src/cli.rs:425-429`), while architecture §2.4 requires the new read-only
surfaces to create no banner-day file. So `auto` would mean two different things
on two sets of commands under one enum (`FramingMode` at `src/cli.rs:424-435`,
which has exactly Auto, Full, Compact and no `none`).

**Fix:** either promote the framing rule to a locked ruling in the Interfaces
section now, with the "auto on catchup/search never stamps banner-day" sentence
spelled out, or add a checkpoint task before B2 that closes it.

### 10. Major: the shared-lock barrier test is deferred to a task that neither delivers nor can host it

B4's acceptance ends: "the shared-lock barrier test closes in B7." Architecture
§7.1 describes it as a test-only barrier hook "immediately after a listing
acquires its shared cursor snapshot", pausing a `channels` count while a writer
blocks on `.cursors.lock`. That hook has to live in product code, in
`cursor_state.rs` or `channels.rs`, and be exercised by an in-crate unit test:
`src/test_support.rs` is behind `#[cfg(test)]` at `src/lib.rs:14-15`, so an
integration test cannot reach it.

B7 owns only `tests/doorbell.rs` and `tests/cli.rs`. Its `delivers` describes a
two-process CLI concurrency test, which is a different and weaker instrument.
The barrier test is not in B7's acceptance, invariants, or verify rows. It falls
through the plan.

**Fix:** either assign the barrier hook and its unit test to B4 (which owns
`channels.rs`) and drop the deferral sentence, or add it to B7's `delivers` with
the owning file listed. Do not leave a deferral pointing at a task that has not
accepted it.

### 11. Major: B8's acceptance asserts a safety property the smoke script does not have

B8 acceptance: "The smoke never touches a live store (refuses to run without an
explicit throwaway root, **as it does today**)."

It does not do that today. `scripts/smoke-installed.sh:7-10` is
`BIN="$1"`, `BASE=$(mktemp -d)`, `export POST_MAIL_ROOT="$BASE/mail"`. The
script creates its own root unconditionally; the one positional argument is the
binary path, not a root. There is no refusal anywhere in the file. The property
happens to hold by construction, but a lane told the guard exists will not add
one, and B8's own invariant ("the smoke runs only against its throwaway root")
has no assertion behind it: its verify row is the script run itself, which is
the thing under test.

**Fix:** correct the sentence to describe the real mechanism (the script mints
its own root with `mktemp -d` and exports `POST_MAIL_ROOT` before any post
invocation), and add one assertion to the extended script that
`POST_MAIL_ROOT` is under `$BASE` before the new observations run.

### 12. Major: nothing in the plan tells an agent that `catchup` exists

`skills/post/SKILL.md:72` is `## Command surface`, and `tests/cli.rs:9689`
records that SKILL.md is "the authority" for the agent-facing surface.
`README.md` (35 KB) and `CHANGELOG.md` are both maintained per feature: the last
five feature commits touching CHANGELOG are `ae89653`, `52ec889`, `68f8df2`,
`def9279`, `8549036`.

No task in Plan B owns `skills/post/SKILL.md`, `README.md`, or `CHANGELOG.md`.
Two new commands and three new JSON fields ship invisible to exactly the
population the goal lock was written for ("An agent opening a busy store today
sees 621 messages and has no cheap way to know which ones it has read").

**Fix:** add the three files to B6's `owned_files` (its persona already carries
`writing-for-agents`), with an acceptance clause that SKILL.md's command surface
section lists `catchup` and `search`.

### 13. Minor: the plan violates a stated repo invariant on schema timing

`CONTRIBUTING.md:20`: "If you change a command, flag, error code, or envelope
shape, update the schema and the tests that pin it **in the same change**." The
plan deliberately lands `catchup` in wave 2 and its schema entry in wave 4. Two
waves of commits ship a binary whose `post schema` omits a real command, and
`scripts/gate.sh:51` only checks that schema exits 0, so nothing notices.

**Fix:** either move the schema entries for `catchup` and `search` into B2 and
B5 (leaving B6 the CONTRACT prose, doctor checks, and the cross-checking test),
or add one sentence to the plan's scope fence acknowledging the deviation and
why.

### 14. Minor: "watch.rs is not modified by any Plan B task" is an assertion with no verify row

B7 `acceptance` states it in prose. Nothing checks it.

**Fix:** add a verify row to B7:
`{run = "git diff --exit-code HEAD~N -- src/commands/watch.rs", expect = "exit 0"}`,
or the equivalent against the integration branch base.

### 15. Minor: `inbox --room <other>` leaves `unread_count` undefined

`src/commands/inbox.rs:8` is `context.resolved_mailbox_dirs(args.room)`, so
`inbox --room beta` from alpha's cwd lists beta's inbox, and
`tests/cli.rs:3758` pins that `--room` stays valid on inbox. Architecture §4.2
gives the formula ("count parseable inbox mail whose id is not in `mail.seen`")
without saying whose `mail.seen`. The `channels` case is pinned carefully by
contrast, because `channels --room` is rejected.

**Fix:** one clause in the Interfaces section: `inbox --room X` counts against
X's own cursor, or returns `null`.

### 16. Minor: recon evidence contradicts itself on one anchor

`seams.md:130` cites `src/commands/chat.rs:1907-1937` for the concurrent-arrival
callback boundary, which is right: `discard_consumes_exactly_the_rendered_batch_even_if_mail_arrives_before_the_callback`
is at `src/commands/chat.rs:1906-1939`. `seams.md:436` cites the same test as
`tests/cli.rs:1907-1937`, which is a different file. Harmless to a careful
reader, a wasted lane cycle for a literal one.

**Fix:** correct `seams.md:436`.

### 17. Minor: the legacy file never goes away and doctor keeps failing on it

Architecture §1.3 leaves `channel-state.json` in place "as rollback evidence"
forever. `src/commands/doctor.rs:386-392` raises `channel_state.<room>.invalid`
at **Error** severity for a malformed one, and that check is not scoped to
pre-migration stores. `src/commands/schema.rs:279` still documents the v1 to v2
rollback recipe (`.channel-state.v1.bak`); nothing analogous is specified for
v2 to `cursors.json`, and rolling back to a 0.8.0 binary after migration silently
loses every read mark recorded after the first catchup. B1 declares
`reversibility = "reversible"` and `effects = ["code"]`, which is true of the
code and not of the disk.

**Fix:** state the rollback story in B6's CONTRACT amendment (restore the 0.8.0
binary, accept re-reading everything consumed since migration), and decide
whether the legacy Error check should downgrade to Info once `cursors.json`
exists.

### 18. Minor: B5 has to copy a security boundary it does not own

B5's visibility filter reuses the archive party rule at
`src/commands/read.rs:104-121`. B5 does not own `read.rs`, so it will duplicate
it. That code carries a comment about a bug already fixed in it once: an
`is_ok_and` that discarded the parse error and let a corrupt archive entry fall
through to the wrong branch. B5's five-fixture matrix (own inbox/read, own-sent
archive, member channel, non-member channel, third-party archive) has no corrupt
archive entry, so the duplicate can reintroduce the same defect and pass.

**Fix:** add a sixth fixture to B5's acceptance, an unparseable archive entry,
and assert it is skipped rather than admitted. Or extract the party filter in B3
(which owns `read.rs`) and have B5 call it.

## Checked, no finding

- **`Cargo.toml` needs no edit.** No `[[test]]` blocks, no `autotests = false`,
  no new dependencies. `tests/*.rs` auto-discover as targets and
  `tests/common/mod.rs` correctly is not one. No lockfile collision between
  parallel lanes either, since no task adds a crate.
- **B2 and B5 share three files but not a wave.** Both claim
  `src/commands/mod.rs`, `src/cli.rs`, and `src/migration_fence.rs`; B5 is
  `blocked_by = ["B1", "B2"]`, so the writes serialize. Correct as authored.
- **`src/lib.rs` is real and B1 owns it.** 17 lines,
  `src/lib.rs:3` is `mod channel_state;`, so `mod cursor_state;` is a one-line
  addition in the file B1 already claims.
- **B1 can call the atomic primitives without editing `mailbox.rs`.**
  `atomic_replace` at `src/mailbox.rs:674` and `exclusive_move` at
  `src/mailbox.rs:817` are already `pub(crate)`.
- **B5 can call the channel primitives without editing `channel.rs`.**
  `message_files:1280`, `list_channels:1302`, `parse_channel_message:1082`,
  `is_canonical_channel_message_id:1214` are all `pub(crate)`.
- **B7's "retain `tests/cli.rs:4405-4468` unchanged" is accurate.** The test is
  `watch_snapshot_emits_direct_and_channel_events_without_consuming_anything`
  at 4404-4468; its final assertion (4460-4467) re-peeks through
  `chat --peek --json` rather than reading a state file, so it survives the
  cursor rewire untouched.
- **The late-arrival anchor is right.** `late_bridged_arrival_between_read_and_own_send_surfaces`
  runs `src/commands/chat.rs:1305-1356`; the next test starts at 1358.
- **The concurrent-writer seam anchor is right.**
  `concurrent_marks_on_two_channels_both_survive` at
  `src/channel_state.rs:831-859`, eight barrier-released threads, which is
  exactly the shape B1 is told to extend.
- **The fence classification anchor is right.** `classify_write` at
  `src/migration_fence.rs:484-515`. Note its final arm is `_ => false`, so
  adding a `Search` variant needs no edit at all and adding `Catchup` without
  the arm compiles silently as a read. B2's acceptance covers the refusal case,
  so this is closed, but it is worth B2 knowing the compiler will not help it.
- **The inbox malformed-mail anchor is right.** `src/commands/inbox.rs:7-35` is
  the loop that warns and skips without counting, matching architecture §4.2.
- **Additive JSON fields will not break existing tests.** Neither
  `ChannelListItem` (`src/output.rs:199-210`) nor `InboxOutput`
  (`src/output.rs:469-476`) uses `deny_unknown_fields`, so ruling 9 holds
  mechanically.
- **The gate command and its expectation match.** `scripts/gate.sh:57` prints
  `GATE PASS [cargo %s, node %s, python %s]`, so B8's substring expectation
  `"GATE PASS"` hits. `gate.sh:22` runs `cargo test --all-targets --all-features`,
  a superset of every task's `cargo test` row, and `gate.sh:14` does
  `cd "$(dirname "$0")/.."`, so it is cwd-independent from a lane worktree.
- **The parked `chat --limit` question has a real bead.** `post-xn0` is open and
  ready in `bd ready`, matching the plan's scope fence claim.
- **The wave map matches `blocked_by`.** B1; then B2/B3/B4; then B5 (B1,B2) and
  B7 (B2,B3,B4); then B6 (B2,B3,B4,B5); then B8 (B6,B7). Consistent as quoted.
- **No task is genuinely irreversible.** Nothing drops a column, renames an
  enum, or deletes history; the one durable new artifact is an additive file.
  See finding 17 for the rollback wrinkle, which is a documentation gap rather
  than a reversibility one.
- **`channels` has no `--room`, so the acting-room assumption holds.**
  `tests/cli.rs:3725-3731` pins that `channels --room alpha` exits 2 with
  `exact_fix = "post channels"`. Architecture §4.1's cwd-resolved identity is
  correct.
- **`FramingMode` has exactly the three values the plan uses.**
  `src/cli.rs:424-435`: Auto, Full, Compact. Architecture §3.2's "there is no
  `none`" is consistent with the existing enum rather than proposing a change.
- **The new doctor check names do not collide.** The existing check is
  `channel_state.<room>.invalid` (`src/commands/doctor.rs:386`); the proposed
  ones are `cursor_state.<room>.*` and `cursor_lock.<room>.*`.
- **`doctor --fix` really is create-only today.**
  `src/commands/doctor.rs:617-631` creates root, defaults, archive, and per-room
  inbox/read directories and nothing else, so B6's invariant is a preservation
  claim rather than a new one.
- **Acceptance No-Claim sentences are present and specific on all eight tasks.**
  They name the thing green does not prove and the task that closes it, which is
  the honest form. B4's is the only one that points at a task that does not
  accept the handoff (finding 10).

## What I could not verify

- **`plan-lint` itself.** No `bin/` directory ships with the installed
  `writing-plans` skill on this box (`~/.agents/skill-library/writing-plans`
  contains only `SKILL.md`, `lessons.md`, `personas/`, `references/`), so I
  could not re-run lint or enumerate the exact `effects` vocabulary. Finding 8
  rests on `references/review-regimes.md:11` prose ("Credentials, schema,
  process, or filesystem effects derive critical"), not on the parser's token
  list. Confirm the exact effect string before editing B6.
- **Whether wave-2 lanes share a `CARGO_TARGET_DIR`.** If they do, three
  concurrent `cargo test --all-targets` runs serialize behind cargo's build
  directory lock rather than corrupting each other, so this is a wall-clock
  question and not a correctness one. Worth checking before assuming wave 2 is
  three times faster than sequential.
