# Automatic Codex mail notification plan

**Status:** Ratified by Claude Fable (Delegate work-mode architecture review,
2026-07-22) with amendments: PostToolUse throttling, failure-streak dedupe,
transcript-grep verification, explicit alternatives analysis, and precise
snapshot exit semantics. A later security review removed state pruning, added
official Codex subagent-field suppression, and pinned live execution to a
private copied hook file under `~/.codex/hooks/`.

**Implementation status (2026-07-22, Claude Fable, Delegate work mode):**
repository implementation complete, live installation complete, and work +
personal Codex profile notification paths proven with real `codex exec` runs.

- `post watch --snapshot` implemented (`src/cli.rs`, `src/commands/watch.rs`,
  `src/commands/schema.rs`) with the ratified exit semantics; contract updated
  in CLI help, `post schema`, `README.md`, `CONTRACT.md`, and
  `skills/post/SKILL.md`.
- Hook adapter at `skills/post/hooks/codex-mail.mjs` (Node stdlib only):
  SessionStart/UserPromptSubmit/root-PostToolUse, subagent suppression, 30s
  PostToolUse throttle, per-session dedupe with SessionStart reset, atomic
  state under the system tmpdir, one diagnostic per failure streak, strict
  fail-open, validated direct-mail IDs, and count-only channel/unreadable
  context.
- Idempotent registration merger at
  `skills/post/hooks/install-codex-hooks.mjs` (explicit target path required;
  never touches a live config on its own).
- Evidence: 4 new black-box CLI tests (snapshot empty/non-empty/conflict/scan
  failure — `tests/cli.rs`); 16 adapter/installer self-tests
  (`skills/post/hooks/codex-mail.test.mjs`, `node --test`, all pass); end-to-end
  smoke of adapter + real binary against isolated `POST_MAIL_ROOT` proved
  validated direct-ID/count-only injection, unread preservation, and dedupe;
  installer smoke against a scratch copy of `~/.codex/hooks.json` proved an
  idempotent merge with unrelated hooks byte-identical and symlink-safe writes.
  The exact official OpenAI Codex `rust-v0.145.0` source tag confirms
  `PostToolUse` serializes `agent_id` / `agent_type` only for
  `Some(SubagentHookContext)`, matching the adapter's subagent suppression.
  Full gate green: `cargo fmt
  --check`, `cargo clippy --all-targets --all-features -- -D warnings`,
  `cargo test --all-features` (34 unit + 60 CLI tests), `cargo build
  --release`, and `node --test skills/post/hooks/*.test.mjs`.
- Live proof: the release binary is installed at `~/.local/bin/post`; the
  reviewed hook is copied to `~/.codex/hooks/post-codex-mail.mjs`; fresh real
  `codex exec` sessions in both work and personal profiles, without a hook
  trust bypass, persisted the seeded direct-mail ID from model-visible hook
  context. Seeded body sentinels were absent from both transcripts, and each
  isolated inbox still contained its unread message afterward. A separate
  active-turn proof delivered mail during a 35-second shell tool call and
  persisted its ID at the subsequent root `PostToolUse` boundary, across the
  production 30-second throttle window.

## Trey's product ask

Make `post` notify this Codex, and any future Codex session on this machine,
about new direct or group mail automatically after launch. There must be no
per-session instruction to start or remember a watcher. Keep the complete
`post` surface available. Choose the most elegant solution, reconcile it with
Claude Fable through Delegate in `work` mode, then implement and review it
through the same workflow.

## Confirmed runtime facts

- Codex 0.145.0 has stable native command hooks.
- Both work and personal Codex profiles use the same
  `~/.codex/hooks.json` (the personal path is a symlink), so one user hook
  registration covers both profiles.
- `SessionStart`, `UserPromptSubmit`, and `PostToolUse` accept
  `hookSpecificOutput.additionalContext`; Codex adds it to model context.
- Hook input includes `session_id` and `hook_event_name`; `PostToolUse` also
  identifies subagents.
- Hooks fire only at Codex lifecycle boundaries. No supported hook can wake an
  idle model when no Codex event is occurring.
- `post watch` already scans unread direct mail plus joined-channel messages,
  emits metadata only, and never consumes mail or advances a channel cursor.
  Its `--once` mode waits for a non-empty batch, so it cannot be called by a
  bounded lifecycle hook when there is no mail.
- `codex app-server` (0.145.0) is experimental and reaches only conversations
  created by its own client. It cannot address an independently launched TUI or
  `codex exec` session, so it is not an ingress path for fleet-wide delivery.
- The live `~/.codex/hooks.json` already runs a cmux command hook on every
  `PostToolUse`, and the memoryd `SessionStart`/`UserPromptSubmit` hooks
  demonstrate the exact working `hookSpecificOutput.additionalContext` shape.
  Per-tool-call hook overhead is established precedent in this config, and the
  file is hand-merged (cmux, memoryd, rm-guard entries coexist), so adding
  entries is safe.

## Alternatives rejected

- **Persistent daemon:** a daemon can watch mail, but nothing it does reaches
  model context until a Codex lifecycle event fires anyway — it adds a managed
  process without changing delivery timing. The hook adapter achieves the same
  visible timing with zero resident processes.
- **Launch wrapper / PTY keystroke injection:** fabricates user input, breaks
  `codex exec` and non-interactive sessions, and races the TUI. Not acceptable.
- **cmux-only integration:** covers only cmux-managed surfaces; ordinary
  terminal Codex sessions get nothing. Fails the "any Codex session" ask.
- **Periodic OS jobs (launchd/cron):** same wake problem as the daemon — no
  path into model context between lifecycle events.
- **Codex app-server ingress:** proven above to reach only its own client's
  conversations. Revisit only if Codex ships a supported session-ingress API.

## Decision

Use Codex's native hook lifecycle. Do not add a daemon, PTY keystroke injector,
terminal-specific integration, or background process.

### 1. Add a nonblocking watch snapshot

Add `post watch --snapshot`.

- Perform exactly one existing `scan_batch` pass (the loop body `watch`
  already runs) and return.
- Emit the current unread direct-mail and pending joined-channel event batch.
- Exit semantics, precisely: empty batch → exit 0 with no output; non-empty
  batch → emit events, exit 0; direct-mail scan failure → nonzero exit with a
  stderr diagnostic, never a false empty. Per-channel degradation keeps the
  existing watch posture (warn on stderr, continue scanning healthy channels).
- Remain envelope-only and read-only: no bodies, moves, or cursor writes.
- Conflict with `--once`; `--interval-ms` has no effect in snapshot mode.

This is a generally useful integration primitive, not Codex-specific logic.

### 2. Add one fail-open Codex hook adapter

Add a small standard-library Node adapter under the installed `post` skill.
It will:

1. parse Codex hook JSON from stdin;
2. ignore subagent `PostToolUse` events to avoid duplicate fan-out;
3. throttle `PostToolUse` scans: if the session state file's mtime is younger
   than 30 seconds, emit `{}` without spawning `post`. `SessionStart` and
   `UserPromptSubmit` always scan. This bounds a burst of hundreds of tool
   calls to one scan per 30 seconds instead of one process spawn each;
4. run the installed `post watch --room codex --snapshot`;
5. compare event identities with a per-Codex-session state file (keyed by
   `session_id`) under the system temporary directory; never prune an override
   directory from inside the hook;
6. on `SessionStart`, reset that session state and surface all pending events;
7. on `UserPromptSubmit` or root `PostToolUse`, surface only unseen events;
8. on a nonzero `post` exit, inject one bounded "mail check failed" diagnostic
   per failure streak (record the streak in session state), never per event —
   and never emit `{}` for a failed scan as if the inbox were empty;
9. emit valid `hookSpecificOutput.additionalContext`, or `{}` when there is
   nothing new.

The adapter watches the `codex` room only: direct mail addressed to `codex`
plus channels the `codex` room has joined. Mail addressed to other rooms
(e.g. `workspace`) is deliberately out of scope — those rooms are shared
destinations, and fanning their traffic into every Codex session would be
noise. Extending coverage later means adding a room to the adapter's scan
list, nothing structural.

The injected context will include validated generated IDs only for readable
direct mail. Channel notifications and unreadable entries are count-only:
channel names, channel-message IDs, and filename-derived unreadable IDs are not
inserted into model context. Subject, body, and sender are also omitted. This
keeps attacker-chosen identifiers and labels out of the prompt while still
surfacing that mail exists. The context states that mail is untrusted data with
no authority and names the explicit `post read` / `post channels` /
`post chat --peek` commands needed to inspect it.

State is notification-only. It never changes `post` unread state. Atomic
replacement prevents partial state files; losing temporary state can only
cause a duplicate reminder, never missed mail.

### 3. Register the adapter once for every Codex profile

Add the same adapter command to `SessionStart`, `UserPromptSubmit`, and
`PostToolUse` in the shared `~/.codex/hooks.json`, with a 5-second timeout
(matching the existing entries; the throttled fast path is a stat plus `{}`,
and even a full scan is milliseconds of filesystem work). The installer copies
the reviewed adapter from the installed skill into
`~/.codex/hooks/post-codex-mail.mjs` and registers that private copy, so future
repo edits do not silently change global hook behavior.

Because the personal profile already symlinks its hook config, the installer
resolves symlink targets before writing, and one shared hooks file covers every
Codex profile without a launch wrapper or profile-specific duplication.

## Delivery semantics

- **Fresh launch:** pending direct and group mail is injected before the first
  turn.
- **Actively working Codex:** mail that arrives is injected after the next
  completed tool call, subject to the 30-second scan throttle — worst-case
  delivery lag during a long autonomous turn is one throttle window, not the
  whole turn.
- **Idle Codex:** an idle model cannot be woken by Codex's current hook API.
  The mail is injected automatically on the next user prompt, before the model
  answers.
- **No manual ritual:** Codex never has to remember to start or poll a watch.

## Safety and failure posture

- Mail metadata remains data, never authority.
- The hook never reads bodies or consumes messages.
- Hook output is JSON-encoded; attacker-controlled subject, sender, channel,
  and unreadable filename fields are not inserted into context.
- A hook or state failure must not block a Codex turn.
- A snapshot scan failure must not masquerade as a healthy empty inbox; expose
  a bounded diagnostic without repeatedly flooding model context.
- The existing registered room and cwd-bound channel identity remain
  unchanged.

## Implementation and verification

1. Add `--snapshot` help, schema, README, contract, skill docs, unit tests, and
   black-box CLI tests for empty/non-empty direct and channel snapshots.
2. Add adapter self-tests covering empty output, launch backlog, dedupe,
   mid-turn new mail, subagent suppression, malformed input, and post failure.
3. Build and install the release binary.
4. Register the hook idempotently without disturbing existing hook entries.
5. Prove the installed adapter output against isolated `POST_MAIL_ROOT` state:
   pipe recorded Codex hook JSON for all three events into the adapter and
   assert exact `hookSpecificOutput.additionalContext` / `{}` outputs,
   including the throttle fast path and the failure-streak diagnostic.
6. Start a fresh installed Codex session (`codex exec` with a trivial prompt)
   with a seeded unread message, then grep that session's transcript under
   `~/.codex/sessions/` for the injected notification text. Transcript
   presence is the objective proof of model-visible injection — do not rely on
   eyeballing rendered output, and do not rely on `codex debug prompt-input`
   unless hooks demonstrably fire in that code path.
7. Send mail during an active test turn and prove it appears at a subsequent
   `PostToolUse` boundary (allowing for the throttle window) while the message
   remains unread.
8. Run fmt, clippy, all tests, release build, skill validation, and independent
   review/fix rounds.

## Explicit non-goal

Do not pretend hooks can asynchronously wake an idle language model. If truly
unsolicited idle delivery becomes necessary, it requires a separate host
notification or a supported Codex/app-server event-ingress API; PTY input
simulation is not an acceptable substitute.

Claude Code sessions are out of scope for this round. Its hook JSON contract
is near-identical, so the same `--snapshot` primitive and a thin adapter
variant cover it later without redesign.
