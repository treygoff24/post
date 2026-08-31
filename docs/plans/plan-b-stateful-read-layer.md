# Plan B: stateful read layer for post

Goal lock: `docs/plans/plan-b-goal-lock.md` (locked 2026-08-31, search IN as
last cuttable phase). Architecture of record:
`docs/plans/recon/architecture.md`; seam evidence `docs/plans/recon/seams.md`;
ergonomics evidence `docs/plans/recon/ergonomics-audit.md`. Bead: `post-rsq`.

Design summary the tasks execute (details in the architecture doc):

- One unified `cursor_state` module persisting **exact seen-ID sets** (not
  watermarks) in `<room>/cursors.json` v1 under a hardened `.cursors.lock`,
  absorbing today's `channel_state.rs` with lazy read-only v0.8 import and
  one-way first-write materialization. Advisory degradation: malformed state
  reads as all-unread, never errors.
- `catchup` is a full-slice consuming **writer** (fence-admitted); `search`
  and the unread-count listings are **read-only** and create nothing.
- Cursor writes ride the existing `after_stdout` callback under the
  already-held root admission; the root-lock window does not widen.
- The doorbell is untouched: watch never reads or writes `cursors.json` after
  startup, and catchup never refreshes a running watcher's startup floor.

## Interfaces

Declared here so sibling lanes share exact names; the architecture doc carries
the full semantics.

- **Module `cursor_state`** (`src/cursor_state.rs`), replacing
  `channel_state`'s public seam: `Snapshot::load(room) -> Snapshot` (never
  errors; advisory degradation), `Snapshot::mail_has_seen(&str) -> bool`,
  `Snapshot::channel_has_seen(&str, &str) -> bool`, and one consumption entry
  `consume(room, Delta) -> Result<()>` where
  `Delta { mail_moves: Vec<MailMove>, channel_seen: Vec<(String, Vec<String>)> }`
  is fixed before stdout. Errors are the existing `AppError` types.
- **On-disk**: `<root>/<room>/cursors.json` v1
  `{"version":1,"mail":{"seen":[..]},"channels":{"<ch>":{"seen":[..]}}}`,
  sorted pretty + trailing newline, 0600, atomic replace via
  `mailbox::atomic_replace`; lock `<root>/<room>/.cursors.lock` 0600 with the
  root-fence-grade inode checks (`src/migration_fence.rs:200-264` pattern).
- **CLI**: `post catchup [<channel> | --mail | --all]` (no selector = `--all`);
  `post search <pattern> [--mail | --channel <ch>] [--limit 1..=1000]`
  (default 100, literal case-insensitive substring). Both accept
  `--framing auto|full|compact` per architecture §3.2/§5.1.
- **JSON additions**: catchup envelope `{ok, room, targets[], count}` and
  search envelope `{ok, framing, room, pattern, match, results[], count,
  limit, truncated}` exactly as architecture §3.2/§5.3; `channels` items gain
  `room` and `unread: number|null`; `inbox` gains `unread_count`. All
  additive; existing fields keep their meaning.
- **Read seam (permanent)**: `channel_state.rs` survives Plan B as the
  legacy-shape module and delegating read seam: `ChannelState::load` returns
  the unified snapshot's channel view (cursors.json when present, else the
  legacy baseline), and `stored_shape_is_valid` stays for legacy validation.
  Its direct callers — `src/commands/watch.rs:964` (startup floor),
  `src/channel.rs:609` (crossed-send guard), `src/commands/doctor.rs:382` —
  compile unchanged and read correct post-migration state with zero edits.
  Only the consumption-write wrappers (`mark_seen`, `mark_seen_through`) are
  transitional; B3 deletes them after rewiring chat/read.
- **output.rs ownership**: `post::output` is the crate's one public module
  (`src/lib.rs:11`) and typed envelopes must live there for the integration
  suites to deserialize them. Edits are wave-serialized: B2 adds the catchup
  envelope (wave 2), B4 the listing fields (wave 3), B5 the search envelope
  (wave 4), B6 the OutputShapes entries (wave 5). No two same-wave tasks own
  it.
- **Framing (ruling 11, locked)**: architecture §3.2's proposal is adopted
  for catchup and search only — `--framing auto|full|compact`, `auto` = one
  compact banner per non-empty invocation above all sections, structured
  framing in JSON, no `none`; existing read/chat framing untouched. B2 and
  B5 implement this text; B6 documents it.
- **Test scaffolding**: `tests/common/mod.rs` (with `#![allow(dead_code)]`)
  exports `Sandbox` (`tests/cli.rs:19-225`) and the shared helper block
  (`tests/cli.rs:3255-3520`); new integration suites
  live in `tests/catchup.rs`, `tests/consuming.rs`, `tests/counts.rs`,
  `tests/search.rs`, `tests/doorbell.rs`, `tests/schema_surface.rs`.

## Scope fence

Non-goals, restated from the locked goal: retention/expiry/compaction;
mentions, notify levels, pins, ack (Plan C); any change to default `watch`
behavior, backlog replay, or ring semantics (`src/commands/watch.rs:782-815`
stays byte-identical in behavior); store format migration beyond the additive
cursor files; cross-machine cursor sync; search indexing or regex (v1 is
literal; `chat --history --grep` already does one-channel regex); changing
existing `chat --limit` consumption semantics (recorded ruling — catchup is
the honest full-slice verb; the `--limit` skip-consumption question is parked
as its own bead outside Plan B); reply threading. Discovered work executes
only when it blocks a row in the acceptance demo.

## Acceptance demo

The seven runnable observations from the goal lock, proven by the extended
installed smoke (`scripts/smoke-installed.sh`, task B8) against a throwaway
`POST_MAIL_ROOT`, plus the canonical gate:

1. Three sends from A make `channels[].unread == 3` for B; 0 after catchup.
2. `catchup <channel>` prints the slice once; a fresh second invocation is
   empty; cursor survives process exit.
3. Mail: `inbox.unread_count` drops after consuming read/catchup.
4. Doorbell both directions: an armed watch still rings after B's catchup; a
   ring alone never advances the cursor (byte-compare `cursors.json`).
5. Fence: catchup refuses on a fenced store without the active generation;
   listings/search/snapshot succeed and leave the store byte-identical.
6. Search visibility: a planted non-member-channel marker never appears.
7. A 0.8-era store with no cursor files works read-only, reports all unread,
   creates nothing until first catchup.

Close condition: `scripts/gate.sh` prints `GATE PASS` and the extended smoke
passes against the release binary.

## Wave map

Derived from `blocked_by` (quoted for readers; `plan-lint --waves --check` is
the authority):

- Wave 1: B1
- Wave 2: B2, B3
- Wave 3: B4, B9
- Wave 4: B5, B7
- Wave 5: B6
- Wave 6: B8

## Invariant → verify table

| task | invariant | verify row |
|---|---|---|
| B1 | late ID below max stays unread; malformed state degrades to all-unread without error; atomic replace; writer refuses symlinked state; concurrent whole-map union survives | `cargo test cursor_state` |
| B2 | catchup delta fixed before stdout; empty catchup exits 0; fenced catchup refuses before any mutation | `cargo test --test catchup` |
| B3 | emit-then-consume ordering preserved; mail read-link commits before mail.seen mark; physical inbox stays authoritative on partial failure | `cargo test --test consuming` |
| B4 | listings create no room/lock/cursor/banner state; non-member unread is null never 0; existing fields unchanged | `cargo test --test counts` |
| B5 | membership/party filter applied before any message file opens; search never touches cursors.json; caps enforced | `cargo test --test search` |
| B6 | schema advertises exactly the real CLI surface; doctor --fix never creates/repairs/deletes cursor state | `cargo test --test schema_surface` |
| B7 | armed watch rings after catchup; ring never advances cursor; post-catchup watch does not replay caught-up backlog; two concurrent CLI writers lose no seen-set; 0.8 store degrades per row 7 | `cargo test --test doorbell` |
| B9 | preview emission never changes ring detection, floor, or admission; previews sanitized and capped | `cargo test --test watch_preview` |
| B8 | smoke runs only against its throwaway root | `bash scripts/smoke-installed.sh target/release/post` |

```toml plan
name = "plan-b-stateful-read-layer"
repo = "~/Code/post"

[regimes.prose]
lanes = 1
min_families = 1
fix_rounds = 0
re_review = "none"
adjudicate = false
severity_floor = "minor"
max_task_calls = 8

[regimes.standard]
lanes = 2
min_families = 2
fix_rounds = 1
re_review = "scoped"
adjudicate = true
severity_floor = "major"
max_task_calls = 12

[regimes.critical]
lanes = 3
min_families = 3
fix_rounds = 2
re_review = "scoped"
adjudicate = true
severity_floor = "blocker"
max_task_calls = 24
```

```toml routing
[executor]
routes = [{engine="codex", model="luna", effort="max", mode="work"},
          {engine="omp", model="glm", effort="high", mode="work"}]

[reviewer]
routes = [{engine="claude", model="opus", effort="high", mode="work"},
          {engine="codex", model="sol", effort="xhigh", mode="work"},
          {engine="cursor", model="grok-4.6-xhigh-fast", effort="high", mode="work"},
          {engine="omp", model="glm", effort="high", mode="work"}]

[integrator]
routes = [{engine="codex", model="luna", effort="medium", mode="work"}]

[adjudicator]
routes = [{engine="claude", model="opus", effort="high", mode="work"},
          {engine="codex", model="sol", effort="xhigh", mode="work"}]

[verifier]
routes = [{engine="codex", model="luna", effort="max", mode="work"},
          {engine="omp", model="glm", effort="high", mode="work"}]

[attacker]
routes = [{engine="grok", model="grok-4.6", effort="high", mode="work"},
          {engine="codex", model="sol", effort="xhigh", mode="work"}]
```

```toml task
id = "B1"
title = "Unified cursor_state module with v0.8 import and hardened locking"
delivers = "src/cursor_state.rs implementing architecture §1: cursors.json v1 exact seen-ID sets (mail + per-channel), advisory-degrading Snapshot::load, one consume(Delta) writer under a hardened .cursors.lock with atomic replace, lazy read-only channel-state.json import with one-way first-write materialization; channel_state.rs is reduced to the permanent legacy+read seam per Interfaces: ChannelState::load delegates to the unified snapshot, stored_shape_is_valid kept, and the consumption-write functions (mark_seen, mark_seen_through) become thin delegating wrappers over cursor_state::consume so every existing caller compiles unchanged (those two wrappers deleted in B3); the full shared test-helper slice extracted verbatim to tests/common/mod.rs (Sandbox at tests/cli.rs:19-225 AND the helper block at tests/cli.rs:3255-3520 — register_room, join_channel, seed_fence_store, assert_success, from_stdout, and the rest), opened with #![allow(dead_code)] so -D warnings survives suites that use a subset, with tests/cli.rs using it via mod common"
kind = "build"
blocked_by = []
acceptance = "cargo test cursor_state covers: missing and malformed state snapshot as empty without error or writes; exact v1 serialization round-trip; valid v0.8 import read-only then materialized on first consume with channel-state.json left untouched; late channel ID below the maximum stays unread; out-of-order mail marks only chosen IDs; symlinked cursors.json degrades on read and refuses on write; eight barrier-released writers all survive whole-map reload/union/replace (extend the seam at src/channel_state.rs:830-859). Full existing suite still green, plus one explicit delegation test: a store with a materialized cursors.json makes ChannelState::load return the unified seen view, not the legacy file's (this is what keeps the watch startup floor correct after migration). Green does not prove CLI-level fence behavior, doorbell interaction, or cross-process count correctness — those close in B2/B7."
role = "executor"
persona = "minimalist-implementer"
skills = ["rust-engineer"]
owned_files = ["src/cursor_state.rs", "src/channel_state.rs", "src/lib.rs", "tests/common/mod.rs", "tests/cli.rs"]
invariants = ["late ID below max stays unread", "malformed state degrades to all-unread without error", "state replace is atomic and refuses symlinks", "concurrent whole-map union loses no marks"]
verify = [{run = "cargo test cursor_state", expect = "exit 0"}, {run = "cargo test", expect = "exit 0"}, {run = "cargo clippy --all-targets --all-features -- -D warnings", expect = "exit 0"}, {run = "cargo fmt --check", expect = "exit 0"}]
reversibility = "reversible"
effects = ["code"]

[review]
tier = "critical"
personas = ["refuter", "test-skeptic", "guess-hunter"]
```

```toml task
id = "B2"
title = "post catchup command with fence writer admission"
delivers = "src/commands/catchup.rs implementing architecture §3: selectors <channel>|--mail|--all (default --all), membership required for a positional channel, full-slice fixed-delta consumption via cursor_state::consume in the after_stdout callback under the dispatcher-held admission, the {ok,room,targets[],count} JSON envelope and sectioned human output with the one-compact-banner framing rule, /dev/null refusal matching consuming chat; Catchup(_) => true added to the writer classification"
kind = "build"
blocked_by = ["B1"]
acceptance = "cargo test --test catchup covers: three-message catchup returns full IDs then a fresh invocation is empty and exits 0 with 'caught up'; --all reports inspected-but-empty targets with count 0; unparseable channel entry fails that target closed before stdout; unparseable mail warns and stays unread while valid mail moves; fenced store refuses catchup before stdout, lock creation, or cursor creation; matching generation succeeds. Green does not prove live-watch doorbell interaction (B7) or installed-binary behavior (B8)."
role = "executor"
persona = "minimalist-implementer"
skills = ["rust-engineer", "rust-agent-cli"]
owned_files = ["src/commands/catchup.rs", "src/commands/mod.rs", "src/cli.rs", "src/migration_fence.rs", "src/output.rs", "tests/catchup.rs"]
invariants = ["catchup delta is fixed before stdout", "empty catchup exits 0", "fenced catchup refuses before any mutation"]
verify = [{run = "cargo test --test catchup", expect = "exit 0"}, {run = "cargo test", expect = "exit 0"}, {run = "cargo clippy --all-targets --all-features -- -D warnings", expect = "exit 0"}, {run = "cargo fmt --check", expect = "exit 0"}]
reversibility = "reversible"
effects = ["code"]

[review]
personas = ["refuter", "spec-fidelity"]
```

```toml task
id = "B3"
title = "Rewire consuming reads onto cursor_state with mail-move ordering"
delivers = "Plain read and plain chat (plus --discard-through and --seen-by) consume through cursor_state::consume; B1's consumption-write wrappers (mark_seen, mark_seen_through) deleted, while the delegating read seam (ChannelState::load, stored_shape_is_valid) remains per Interfaces for watch.rs, channel.rs, and doctor.rs, none of which this task touches; direct-mail ordering per architecture §2.3: stdout, then read-link commit, then mail.seen mark, then atomic cursor replace, with every partial failure conservative (unmarked on link failure, marked on unlink-after-link failure, physical inbox authoritative when the cursor write fails)"
kind = "build"
blocked_by = ["B1"]
acceptance = "cargo test --test consuming covers: emit-then-consume preserved for chat (fixed batch marks only pre-render IDs, late arrival stays unread, extending src/commands/chat.rs:1305-1354); read moves mail then marks mail.seen; simulated link failure leaves the ID unmarked and mail in inbox; simulated cursor-write failure after a successful move leaves physical state authoritative and the next read not duplicated; --discard-through and --seen-by behave identically to 0.8.0 through the new module; the existing tests/cli.rs assertions that read channel-state.json directly (tests/cli.rs:4064-4067, 5049-5053, and the now-vacuous ones at 4002-4009, 4935, 5383, 6437-6444) are rewritten against cursors.json so none passes vacuously. Green does not prove concurrent cross-process behavior (B7). No-Claim: green does not prove unchanged behavior for chat --limit skip-consumption, which is deliberately out of scope."
role = "executor"
persona = "minimalist-implementer"
skills = ["rust-engineer"]
owned_files = ["src/commands/chat.rs", "src/commands/read.rs", "src/mailbox.rs", "tests/consuming.rs", "tests/cli.rs"]
invariants = ["emit-then-consume ordering preserved", "read-link commits before mail.seen mark", "physical inbox stays authoritative on partial failure"]
verify = [{run = "cargo test --test consuming", expect = "exit 0"}, {run = "cargo test", expect = "exit 0"}, {run = "cargo clippy --all-targets --all-features -- -D warnings", expect = "exit 0"}, {run = "cargo fmt --check", expect = "exit 0"}]
reversibility = "reversible"
effects = ["code"]
consumes = ["src/cursor_state.rs"]

[review]
tier = "critical"
personas = ["refuter", "test-skeptic", "seam-verifier"]
```

```toml task
id = "B4"
title = "Real unread counts in channels and inbox listings"
delivers = "channels items gain room and unread (number for joined channels, null for non-members or no acting room) computed per architecture §4.2 from one cursor snapshot and the existing single directory enumeration, parsing only unseen candidates; inbox gains unread_count over parseable inbox mail; both listings stay read-only (shared-lock snapshot when an existing .cursors.lock is present, creating nothing); existing messages/unread/count fields untouched"
kind = "build"
blocked_by = ["B1", "B2"]
acceptance = "cargo test --test counts covers: unread 3 then 0 around a consume; non-member channel and unregistered acting room yield null never 0; unreadable unseen channel file counts as one unhandled item while a seen malformed file is ignored; inbox unread_count excludes malformed mail per src/commands/inbox.rs:7-35; a listing on a store with no cursor files reports all unread and a before/after filesystem manifest is byte-identical. Includes the architecture §7.1 shared-lock barrier test, hosted here because this task owns the listing path and the src/cursor_state.rs test hook: a paused listing holds the shared lock, a cursor writer provably blocks, and after release the next count is exact. Green does not prove cross-process CLI concurrency — that closes in B7."
role = "executor"
persona = "minimalist-implementer"
skills = ["rust-engineer"]
owned_files = ["src/commands/channels.rs", "src/commands/inbox.rs", "src/output.rs", "src/channel.rs", "src/cursor_state.rs", "tests/counts.rs"]
invariants = ["listings create no room, lock, cursor, or banner state", "non-member unread is null never zero", "existing listing fields keep their meaning"]
verify = [{run = "cargo test --test counts", expect = "exit 0"}, {run = "cargo test", expect = "exit 0"}, {run = "cargo clippy --all-targets --all-features -- -D warnings", expect = "exit 0"}, {run = "cargo fmt --check", expect = "exit 0"}]
reversibility = "reversible"
effects = ["code"]
consumes = ["src/cursor_state.rs"]

[review]
personas = ["refuter", "conventions"]
```

```toml task
id = "B5"
title = "post search: literal, capped, visibility-filtered, cursorless"
delivers = "src/commands/search.rs implementing architecture §5: case-insensitive literal Unicode substring over body/subject/sender/ID across party-visible mail (inbox+read+party-filtered archive, deduped preferring inbox) and member channels only, membership checked before any message file opens, --mail/--channel selectors, default limit 100 hard cap 1000 with truncated via one-beyond-cap probe, deterministic newest-first ordering, sanitized 160-scalar previews, the §5.3 JSON envelope, read-only classification (Search(_) => false)"
kind = "build"
blocked_by = ["B1", "B2", "B4"]
acceptance = "cargo test --test search covers the five-fixture isolation matrix (own inbox/read, own-sent archive, member channel, non-member channel, third-party archive mail — only the first three appear); regex punctuation stays literal; caps and truncated behavior; invalid membership file fails that channel closed; no cursors.json read or write and no state files created (byte-identical manifest); fenced store search succeeds with a stale generation. Green does not prove search performance on large stores — linear scan is the accepted cost per goal-lock ruling 8."
role = "executor"
persona = "minimalist-implementer"
skills = ["rust-engineer", "rust-agent-cli"]
owned_files = ["src/commands/search.rs", "src/commands/mod.rs", "src/cli.rs", "src/migration_fence.rs", "src/output.rs", "tests/search.rs"]
invariants = ["membership and party filters apply before any message file opens", "search never reads or writes cursor state", "result caps are enforced"]
verify = [{run = "cargo test --test search", expect = "exit 0"}, {run = "cargo test", expect = "exit 0"}, {run = "cargo clippy --all-targets --all-features -- -D warnings", expect = "exit 0"}, {run = "cargo fmt --check", expect = "exit 0"}]
reversibility = "reversible"
effects = ["code"]

[review]
personas = ["attacker", "spec-fidelity"]
```

```toml task
id = "B6"
title = "Contract, schema, and doctor amendments for the read layer"
delivers = "CONTRACT.md amended per architecture §6.1 (cursor state in mutable-delivery-state law, cursors.json v1 + lock in on-disk format replacing the contradicted channel-state section, fence matrix rows for catchup/search, catchup/search command grammar and output shapes, channels/inbox field additions, one-banner framing rule for the two new surfaces, performance/security notes); schema.rs entries and OutputShapes for catchup and search plus the three new listing fields; doctor read-only checks cursor_state.<room>.invalid, cursor_lock.<room>.invalid, cursor_state.<room>.legacy with doctor --fix never touching cursor state"
kind = "build"
blocked_by = ["B2", "B3", "B4", "B5", "B9"]
acceptance = "cargo test --test schema_surface proves schema-vs-reality: every advertised catchup/search arg and output field exists in real CLI output and vice versa (a clap-only addition fails); doctor on a store with a planted malformed cursors.json warns cursor_state.<room>.invalid and --fix leaves it untouched; doctor on a legacy channel-state.json store emits the info check; the exact command-list assert at tests/cli.rs:352-360 is updated to the fourteen commands; README.md, CHANGELOG.md (Unreleased), and skills/post/SKILL.md document catchup, search, unread fields, and the watch preview lines so the new surface is visible to agents. CONTRACT.md changes land as their own commit, never folded into feature commits (goal-lock §6). Green does not prove the prose is complete — the B6 review reads CONTRACT.md against the architecture doc section by section."
role = "executor"
persona = "minimalist-implementer"
skills = ["rust-engineer", "writing-for-agents"]
owned_files = ["CONTRACT.md", "src/commands/schema.rs", "src/commands/doctor.rs", "tests/schema_surface.rs", "tests/cli.rs", "README.md", "CHANGELOG.md", "skills/post/SKILL.md"]
invariants = ["schema advertises exactly the real CLI surface", "doctor --fix never creates, repairs, or deletes cursor state"]
verify = [{run = "cargo test --test schema_surface", expect = "exit 0"}, {run = "cargo test", expect = "exit 0"}, {run = "cargo clippy --all-targets --all-features -- -D warnings", expect = "exit 0"}, {run = "cargo fmt --check", expect = "exit 0"}]
reversibility = "reversible"
effects = ["code", "docs"]
consumes = ["src/commands/catchup.rs", "src/commands/search.rs", "src/commands/chat.rs", "src/commands/channels.rs"]

[review]
tier = "critical"
personas = ["spec-fidelity", "refuter", "conventions"]
```

```toml task
id = "B7"
title = "Cross-cutting proofs: doorbell, fence matrix, concurrency, old stores"
delivers = "tests/doorbell.rs with the live-watch invariant in both directions (armed watch rings m2 after catchup consumed m1; a ring without any consuming read leaves cursors.json byte-identical or absent and a later catchup still returns the message); the fence matrix at tests/cli.rs:4884-5053 extended for catchup refusal and search/listing read-only admission; a third doorbell direction — a watch STARTED AFTER a catchup loads its startup floor from the unified state and does not replay the caught-up backlog (this binds the delegating read seam; it is the case both existing directions miss because they arm the watch first); a two-process CLI concurrency test (two consuming processes, different channels, one room, both seen-sets survive and the next listing shows exactly the one new message); 0.8-store fixtures (no state files, and valid channel-state.json baseline) proving acceptance row 7 end to end"
kind = "build"
blocked_by = ["B2", "B3", "B4", "B9"]
acceptance = "cargo test --test doorbell and the extended tests/cli.rs matrix pass, with each new assertion mutation-checked once: break the guarded behavior (e.g. make watch write the cursor, or drop the union) in a scratch copy and confirm the suite goes red before trusting green. The existing watch snapshot test tests/cli.rs:4405-4468 is retained unchanged. src/commands/watch.rs is not modified by this task or any Plan B task except B9's ring-text preview emission. Green does not prove multi-machine or long-uptime watch behavior."
role = "executor"
persona = "test-first-writer"
skills = ["rust-engineer"]
owned_files = ["tests/doorbell.rs", "tests/cli.rs"]
invariants = ["armed watch rings after catchup", "a ring never advances the cursor", "a watch started after catchup does not replay caught-up backlog", "two concurrent CLI writers lose no seen-set", "a 0.8 store works read-only until first catchup"]
verify = [{run = "cargo test --test doorbell", expect = "exit 0"}, {run = "cargo test --test cli", expect = "exit 0"}, {run = "cargo clippy --all-targets --all-features -- -D warnings", expect = "exit 0"}, {run = "cargo fmt --check", expect = "exit 0"}]
reversibility = "reversible"
effects = ["code"]
consumes = ["src/commands/catchup.rs", "src/commands/chat.rs", "src/commands/channels.rs"]

[review]
personas = ["test-skeptic", "refuter"]
```

```toml task
id = "B9"
title = "Watch ring-line body previews (goal-lock ruling 4)"
delivers = "Watch text ring lines and digest lines carry a sanitized single-line body preview capped at 80 Unicode scalar values (control chars stripped, newlines flattened, truncation marked), rendered as untrusted data within the existing line shape so the [first..last] fencepost ids and --since suffix survive unchanged; NDJSON watch events gain an additive preview field; ring detection, backlog floor, heartbeat, and admission logic in src/commands/watch.rs are untouched — this task edits emission formatting only"
kind = "build"
blocked_by = ["B1", "B3"]
acceptance = "cargo test --test watch_preview covers: preview present and capped on ring and digest lines; a body with ANSI escapes, newlines, and a 500-char run renders as one sanitized line; the fencepost --since suffix round-trips unchanged; NDJSON events carry the additive preview field and existing fields are byte-stable. The existing tests/cli.rs pins that ring lines carry no bodies are updated by this task to pin the new preview contract instead (they were updated for cursors.json by B3 first). Green does not prove the preview renders safely in every terminal — sanitization is the boundary, not terminal behavior."
role = "executor"
persona = "minimalist-implementer"
skills = ["rust-engineer", "rust-agent-cli"]
owned_files = ["src/commands/watch.rs", "tests/watch_preview.rs", "tests/cli.rs"]
invariants = ["preview emission never changes ring detection, floor, or admission behavior", "previews are sanitized and capped"]
verify = [{run = "cargo test --test watch_preview", expect = "exit 0"}, {run = "cargo test", expect = "exit 0"}, {run = "cargo clippy --all-targets --all-features -- -D warnings", expect = "exit 0"}, {run = "cargo fmt --check", expect = "exit 0"}]
reversibility = "reversible"
effects = ["code"]
consumes = ["tests/cli.rs"]

[review]
personas = ["refuter", "spec-fidelity"]
```

```toml task
id = "B8"
title = "Extended installed smoke and full-gate close"
delivers = "scripts/smoke-installed.sh extended after its two-room setup (scripts/smoke-installed.sh:45-57) with the seven goal-lock acceptance observations run against the release binary and a throwaway POST_MAIL_ROOT, including the byte-compare for row 4 and the planted non-member marker for row 6"
kind = "build"
blocked_by = ["B6", "B7"]
acceptance = "bash scripts/smoke-installed.sh target/release/post passes all seven observations against the freshly built release binary, and scripts/gate.sh prints GATE PASS. The smoke never touches a live store: it mints its own throwaway root via mktemp and exports POST_MAIL_ROOT itself (scripts/smoke-installed.sh:7-10), exactly as today. Green does not prove behavior on the Mac or under a different cargo toolchain — cross-platform verification is release procedure, not this task."
role = "executor"
persona = "minimalist-implementer"
skills = ["rust-engineer"]
owned_files = ["scripts/smoke-installed.sh"]
invariants = ["the smoke runs only against its throwaway root"]
verify = [{run = "cargo build --release", expect = "exit 0"}, {run = "bash scripts/smoke-installed.sh target/release/post", expect = "exit 0"}, {run = "bash scripts/gate.sh", expect = "GATE PASS"}]
reversibility = "reversible"
effects = ["code"]
consumes = ["tests/doorbell.rs", "src/commands/schema.rs"]

[review]
personas = ["conventions", "close-verifier"]
```
