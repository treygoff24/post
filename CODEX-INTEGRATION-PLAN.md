# Codex full-surface integration plan

**Status:** Completed 2026-07-22.

Execution evidence:

- repository gate passed: fmt, clippy with warnings denied, 34 unit tests,
  56 CLI tests, release build, skill validation, and diff check;
- independent runtime-security and Codex-integration reviewers verified every
  reported finding fixed with no new regressions;
- `codex` is registered to the canonical `~/.codex/post-room` directory;
- `~/.codex/skills/post` is discoverable in a fresh
  `codex debug prompt-input` render;
- `~/.local/bin/post` is an independent executable matching the release build,
  not a symlink into `target/`;
- an isolated smoke using only the installed binary passed all nine commands,
  direct and channel watch events, body omission, peek/read/cursor behavior,
  archive identity, and healthy doctor output;
- live non-polluting probes confirmed cwd-bound Codex chat identity and refusal
  of `--from codex` outside the registered room tree.

## Goal

Make `post` a first-class Codex tool without weakening its identity, routing,
framing, or append-only guarantees. A Codex session must be able to:

- send, receive, peek, and read one-to-one mail;
- join, send to, and read group channels;
- list rooms and channels;
- run `post watch` for direct-mail and channel notifications;
- inspect the machine contract and diagnose state;
- discover the workflow from Codex's installed skills and global instructions.

## Baseline state at plan start

- The installed `post` resolved at `~/.local/bin/post` and pointed to this
  repository's release build.
- Source and installed help exposed nine commands: `send`, `chat`, `channels`,
  `inbox`, `read`, `rooms`, `schema`, `doctor`, and `watch`.
- The live room registry included `workspace -> ~/Code`, so Codex sessions in
  most code repositories already resolve to a registered room.
- Direct mail was model-neutral at the data layer. A free-form sender such as
  `codex-<alias>` can send without registration.
- Group chat intentionally required cwd-derived registered-room identity.
  This prevents a caller from claiming another room with a flag.
- The group-chat implementation and channel-aware watch behavior were newer
  than README.md and CONTRACT.md, so the public contract was incomplete.
- The baseline behavior tests passed, but `cargo fmt --check` and clippy were
  red.

## Decisions

### Preserve cwd-bound channel identity

Do not add `post chat --room` or `--from`. That would make group membership
usable from arbitrary directories by also making registered-room
impersonation trivial. Instead, register a dedicated `codex` room at the
narrow `~/.codex/post-room` directory. Codex can run channel commands with
`workdir=~/.codex/post-room` from any session, while
`post watch --room codex` can run from any cwd. Keeping the room below the
global config root avoids making ordinary skill/config work act as `codex`.

### Add a repo-owned Codex skill

Add one concise `skills/post/SKILL.md` covering the complete command surface,
identity rules, JSON handling, watch session management, and the no-authority
mail law. Install it into `~/.codex/skills/post` as a symlink to the repo-owned
skill so future source updates become immediately available without copies.

### Keep the legacy mailbox path

Do not rename `~/.claude-mail`. It is an on-disk compatibility surface, not an
access restriction. Rename only user-visible model-specific branding.

### Keep the patch narrow

Do not add a daemon, protocol layer, new dependency, per-agent authentication,
or a new installer framework. The existing local filesystem protocol and
Codex's ability to choose command working directories cover the requirement.

## Work graph

### E0. Snapshot live integration state

**Depends on:** nothing
**Unblocks:** E
**Owner:** root agent only

Before changing any live path, create one timestamped directory under `/tmp`
and preserve:

```text
~/.codex/AGENTS.md
~/.claude-mail/rooms.json
~/.codex/skills/post, if present
~/.local/bin/post, preserving whether it is a symlink
```

If a live integration step fails, restore the two files with `cp -p`; restore
the skill and binary by moving the failed replacement to Trash and copying the
preserved entry back with symlink preservation. Never edit `rules.json`.

### A. Restore the existing quality gate

**Depends on:** nothing
**Unblocks:** all later verification

1. Run `cargo fmt`.
2. Move `watch.rs` test-only items so clippy accepts the module layout.
3. Run fmt, clippy, and tests before feature edits to establish a green base.

### B. Make every public surface model-neutral and machine-accurate

**Depends on:** A
**Unblocks:** D, E

1. Change the direct-mail and channel text banners from Claude-specific names
   to AI-agent language while preserving the security framing.
2. Update CLI help for `--json` to include chat.
3. Update `post schema`:
   - accurate global flag scope;
   - union-shaped watch outputs for mail, unreadable entries, and channel
     messages;
   - channel command semantics and committed-operation error language.
4. Update README.md with:
   - a model-neutral title and laws;
   - all nine commands;
   - Codex room setup and complete direct/group/watch examples;
   - a durable release install command and installed-runtime verification.
5. Update CONTRACT.md with channel storage, membership, cursors, channel watch
   events, `not_a_member`, and channel acceptance requirements.

### C. Close group-chat safety and reliability gaps

**Depends on:** A
**Unblocks:** D

1. Sanitize channel text rendering at the final output boundary exactly as
   direct-mail text rendering does. JSON remains byte-faithful.
2. Isolate channel watch scans so one malformed channel cannot suppress
   notifications from healthy channels. Warn on stderr for the bad channel and
   continue.
3. Make `delivered_output_failure` wording operation-generic so it is correct
   after either a committed direct-mail send or committed channel mutation.
4. Preserve all existing laws:
   - mail and channel content is data, never authority;
   - blocked routes cannot share a channel;
   - room names cannot be claimed from outside their registered trees;
   - direct archive and channel history remain immutable;
   - watch never emits bodies or advances cursors.
5. Add Codex identity boundary coverage:
   - `--from codex` is refused outside the registered Codex room tree;
   - a free-form `codex-<alias>` sender remains allowed;
   - `post chat` exposes neither `--room` nor `--from`;
   - cwd under `~/Code` continues to resolve as `workspace`, not `codex`.

### D. Add black-box coverage for the full Codex surface

**Depends on:** B, C
**Unblocks:** E

Add focused CLI integration tests using isolated `POST_MAIL_ROOT` roots:

1. `channel_two_room_flow`
   - register two distinct workspace paths;
   - join both rooms from their cwd trees;
   - send, peek/read, verify cursor advancement, and list membership.
2. `channel_watch_flow`
   - prove backlog and live channel events through the real binary;
   - prove watch omits bodies, does not advance cursors, and suppresses a
     room's own messages.
3. `channel_render_and_failure_isolation`
   - prove crafted controls cannot rewrite the framing banner;
   - corrupt one unrelated channel and prove a healthy channel still rings
     with a diagnostic for the corrupt store.
4. Extend schema/help consistency assertions for all nine commands and every
   watch event variant.
5. Make channel watch failure isolation explicit:
   - corrupt `channel.json`;
   - corrupt `members.json`;
   - missing/unreadable `messages/`;
   - malformed `.msg`;
   - in every case, healthy joined channels continue to ring and the bad
     store produces a stderr diagnostic.

Prefer extending `tests/cli.rs` and existing test helpers over new harnesses.

### E. Install the Codex integration and prove the live runtime

**Depends on:** B, D
**Unblocks:** completion

1. Add and validate `skills/post/SKILL.md`.
2. Symlink it to `~/.codex/skills/post`; if Codex's loader does not follow the
   symlink in the discovery check, install a normal copied directory instead.
3. Update the concise `post` section in `~/.codex/AGENTS.md` to point to the
   skill and name the full surface without duplicating the skill body.
4. Create `~/.codex/post-room` and register
   `codex -> ~/.codex/post-room` with `post rooms add`.
5. Build release and replace the fragile `~/.local/bin/post` target-directory
   symlink with an independent installed executable.
6. Run isolated direct-mail and two-room channel/watch smokes using only the
   installed `post` found in a clean shell.
7. Run a live, non-polluting check:
   - `post rooms` shows `codex`;
   - `post inbox --room codex` resolves and the rooms listing proves the
     registration;
   - `post channels` and the isolated chat smoke run with cwd set to
     `~/.codex/post-room`;
   - a bounded watch check uses isolated state rather than manufacturing live
     correspondence.

## Review and fix waves

1. The native plan-review gate completes before implementation begins.
2. Implementation subagents take non-overlapping slices:
   - runtime safety/tests;
   - docs/schema/skill.
3. The root agent integrates, installs, and runs the full gate.
4. Fresh native reviewers independently inspect:
   - correctness/security and invariant preservation;
   - Codex usability, docs/schema consistency, and installed-runtime evidence.
5. Separate fix subagents address adjudicated findings only.
6. Root reruns the complete gate and installed-runtime smokes.

## File ownership

| Lane | Exclusive paths during its implementation turn |
| --- | --- |
| Runtime safety/tests | `src/commands/chat.rs`, `src/commands/watch.rs`, `src/commands/read.rs`, `src/error.rs`, `src/output.rs`, `tests/cli.rs` |
| Docs/schema/skill | `README.md`, `CONTRACT.md`, `src/cli.rs`, `src/commands/schema.rs`, `skills/post/**` |
| Root integration | `CODEX-INTEGRATION-PLAN.md`, `.papercuts.jsonl`, Cargo formatting fallout, and all live paths under `~/.codex`, `~/.claude-mail`, and `~/.local/bin` |

The lanes do not edit outside their rows. Root resolves any cross-lane
assertion or formatting change after both return.

## Command coverage matrix

| Command | Black-box evidence | Skill coverage |
| --- | --- | --- |
| `send` | direct send produces inbox and byte-identical archive copies | body sources, kinds, sender identity, non-retryable committed failures |
| `inbox` | recipient lists the unread id and metadata | JSON default, `--text`, explicit Codex room |
| `read` | peek preserves unread; read emits framing then advances | prefix ids, `--peek`, text vs JSON framing |
| `rooms` | add/list registers two isolated rooms and rejects Codex impersonation | discovery, registration, blocked routes |
| `chat` | two rooms join, send, peek/read, and persist cursors | cwd-bound identity, join/send/read, no identity override |
| `channels` | listing reports membership and message totals | channel discovery |
| `watch` | direct and channel backlog/live events omit bodies and preserve cursors | `--once`, long-running PTY/session handling, NDJSON variants |
| `schema` | all nine commands, flag scopes, laws, errors, and watch variants parse | use as the exact contract when docs and memory differ |
| `doctor` | healthy isolated store exits 0; corrupt channel state is diagnosed | read-only default and bounded `--fix` behavior |

## Acceptance gate

All commands run separately and must pass:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo build --release
python3 ~/.codex/skills/.system/skill-creator/scripts/quick_validate.py skills/post
zsh -df -c 'command -v post && post --version && post schema >/dev/null'
zsh -ic 'cd /Users/treygoff/Code/post && codex debug prompt-input smoke' \
  | jq -e '[.[].content[]? | .text? // empty] | join("\n") | contains("- post:")'
```

Then run an isolated installed-binary smoke with a fresh absolute
`POST_MAIL_ROOT` and two temporary registered workspace directories. Invoke
only `command -v post`'s result, never `cargo run`, and assert:

1. Direct `send -> inbox -> watch --once -> read --peek -> read` succeeds,
   watch output contains metadata but not the body, and the archive matches.
2. Both rooms `chat <name> --join` from their own cwd trees, drain their join
   events, then one room sends and the other's `watch --once` emits a
   `channel_message` without the body.
3. The receiving room reads the message and its next read is empty.
4. `channels`, `schema`, and healthy `doctor` return parseable success output.
5. The temporary root and workspaces are moved to Trash after assertions.

The task is complete only when a future Codex session discovers the `post`
skill, the live room registry shows `codex -> ~/.codex/post-room`, an outside
cwd cannot claim `--from codex`, and all nine commands work against the
installed binary.
