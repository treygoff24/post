# Overnight plan 2026-09-22 → 23: fix every live post papercut

Lead: 🌙 Nightjar (Claude, Mac, `claude-83e5e99e`). Partner and final judgment: 🔭 Aster (GPT-6 Astra high, Codex, `codex-5a036212`). Channel: `post-overnight`. Brief and rulings: `docs/overnight-2026-09-23.md`. Diagnosis: `docs/triage-2026-09-22/` (the explainer wins where the diag files disagree).

**Authority.** Trey ruled that Aster's judgment decides any disagreement or important judgment call. Nightjar orchestrates, integrates, runs every gate on merged code, installs, and does live verification. **Gates:** no GitHub push and no release cut. Everything else is cleared, including installs on both machines and Forgejo pushes.

## How the work is organized

Six build lanes (A–F), five waves, and lead-owned ops. Builders are GPT-6 Luna xhigh (`delegate codex work --model gpt-6-luna --reasoning-effort xhigh --isolation worktree --resumable`). Fixes go back to the lane that holds the context (`delegate followup`). A lane's "done" is a claim until Nightjar re-runs its checks on the merged candidate.

Integration: lane branches merge into `overnight-0923` in `~/Code/post`. `scripts/gate.sh` runs on the merged candidate on the Mac and on the devbox before main moves. Main then fast-forwards and pushes to Forgejo.

### Wave 0: lead ops (now, no builder)

| Item | What | Proof |
|---|---|---|
| O1 | Capture the parked bridge-v2 staged edits (8 files, dated 2026-09-02; nobody is active in that checkout) into devbox worktree `~/Code/claude-space-wt-bridge-v2`, branch `bridge-v2-ship` off `db01766`, by piping `git diff --cached --binary` into `git apply --index` and committing there with hooks. The original checkout's index and tree stay untouched. | The worktree diff against `db01766` equals the original `git diff --cached`, byte for byte. |
| O2 | Disable the 15 dead-pane `post-codex-doorbell@*` timers on the devbox, after `herdr agent get <name>` confirms each pane is gone. Disable only; unit files stay. | `systemctl --user list-timers` shows 4 live; each disabled name has a failed `herdr agent get`. |
| O3 | Baselines: `scripts/gate.sh` on main (11c75c3) on both machines; the bridge suite in the O1 worktree on the devbox; Porch's suite at 39ca6df. | Logged pass and fail sets, so a lane's red is attributable. |
| O4 | Identify the Mac's `com.treygoff.agent-post-cell-bridge` job before any bridge change. | One line in the run log: what it runs and whether v2 replaces it. |

### Wave 1: band-aids and small features (A, B, C, F1 in parallel)

**Lane A: post CLI (Rust).** Owns `src/cursor_state.rs`, `src/commands/chat.rs`, `src/commands/who.rs`, `src/commands/rooms.rs`, `src/commands/profile.rs`, `src/profile.rs`, `src/participant.rs`. Shares `src/cli.rs` with B: A edits only the `Chat`, `Rooms`, and `Profile` argument structs; the lead merges.

- **A1 bounded post-commit lock wait.** `mark_own_message_seen` (chat.rs ~2260–2336 → cursor_state.rs ~362–394) takes `flock(LOCK_EX)` with no deadline after the message is already durable. Add a lock-with-deadline helper in `cursor_state.rs` (poll `LOCK_EX|LOCK_NB` with backoff until a deadline; default 2s, env override `POST_LOCK_DEADLINE_MS` for tests). On timeout, the existing warning path fires and the send receipt returns `ok:true` with a `warnings[]` entry naming the lock. Test: hold the lock in the test and assert that the send returns within the deadline, the message is on disk, and the warning is present. Red-proof against current code, which hangs.
- **A2 refuse unintended stdin on a read.** Only an actual body is refused. Never refuse on "stdin is not a terminal" alone: this harness runs every command with stdin from `/dev/null`, and hooks pipe empty input. Required cases, each tested on its own: `/dev/null` → normal read; a regular file or pipe at EOF with zero bytes → normal read; a nonempty pipe, heredoc, or file → usage error with exit code 2, no read state consumed, and `error.details.exact_fix` holding the exact `--body-file -` command; an open pipe that stays silent → no blocking, and it reads normally after a bounded non-blocking probe of at most 100ms. Never auto-send.
- **A3 `who` lease wording.** The text output prints `lease=active|stale|ended`, not `state=…`, plus one footer hint: "lease is not attention; for 'did they read it' use `post chat <channel> --seen-by <id>`". JSON keeps `state` unchanged and adds `lease` as an alias, because Porch and others read the JSON. Test both.
- **A4 `rooms set-path NAME PATH`.** Changes only the workspace's discovery path (cwd registration) in `rooms.json`, under the registry lock. It never moves mail storage or histories and never rewrites participant records. It validates a canonical absolute path, refuses a path already owned by another room, refuses `remote/*` placeholders unless `--from-remote` converts a placeholder to local, and prints the before and after. `--dry-run` is supported. Tests cover each refusal and one success, and check that the mail directory is untouched.
- **A5 `profile list`.** Read-only: every rendering profile with its holder id, workspace, name, sigil, and lease state, computed with the same predicate `profile set` uses for uniqueness. `--json` gives `{ok, profiles:[…]}`. Text notes that occupancy is lease-dependent. Test: the listing agrees with the uniqueness refusal.
- **A6 cursor read race (moved from B, since A owns `cursor_state.rs`).** `ParticipantCursors::load` validates metadata and reads the content as two pathname operations. Open once with `O_NOFOLLOW|O_NONBLOCK|O_CLOEXEC`, take `file.metadata()` on that descriptor, and read the same descriptor, keeping the cursor-specific `nlink==1` check. Reuse the held-descriptor pattern already in `src/mailbox.rs` (~1150) and `src/lineage_store.rs` (~1051); no new generic file abstraction. Keep failing open (never auto-reset read state) and keep the retry-once and re-report marker. Tests: a symlinked cursor file is still refused; a replaced file between stat and read can no longer yield a mismatched verdict.

**Lane B: watch (Rust).** Owns `src/commands/watch.rs`, `src/eligibility.rs`, and the watch-event parts of `src/output.rs`. In `src/cli.rs` it edits only `WatchArgs`.

- **B1 `watch --reason mail|channel|mention`** (repeatable), applied at the final delivery boundary, with an unfiltered default. For `--digest`, filter group members before digesting. Unreadable channel events carry reason `channel`, never `mention`; document that limit in `--help`. Tests: each reason alone, combinations, digest, and the default unchanged.
- **B2 measurement, no behavior change.** Add a `POST_WATCH_PROFILE=1` stderr line per scan with mail-snapshot time, channel-enumeration time, and file counts. Add a bench (`benches/` or an ignored test) that builds a synthetic heavy store and reports per-projection cost against history size. Run it on the devbox against the real heavy participant with `--snapshot` (consumes nothing). The results decide whether wave 5 is worth doing.

**Lane C: hooks and doorbell band-aids (JS, Python).** Owns `doorbell/post-doorbell` and its tests, and `skills/post/hooks/codex-notify-monitor.mjs` and `install-*.mjs` with their tests.

- **C1 Python doorbell accepts typed-address mail.** When `room` is absent, the namespace comes from `address.kind` and `address.name`; a legacy `room` is accepted only when it agrees with the address. Dedupe keys keep the typed kind. Tests: participant, lineage, and workspace events; a mismatched room is rejected.
- **C2 Codex monitor fails loudly.** Log "pane not found: <agent>" when herdr lacks the target. Keep the post exit status, timeout versus exit, and a bounded, sanitized stderr excerpt (at most 300 bytes, no event bodies) instead of "snapshot failed; notification state is unknown". Tests for each path.
- **C3 installers handle `-h`/`--help` first:** usage on stdout, exit 0, before any validation. Bad arguments stay nonzero. Test every `install-*.mjs`.

**Lane F1: bridge v2 finish (Python, devbox, the O1 worktree).** Owns `post-bridge/` in the O1 worktree only.

- Review the captured 2026-09-02 edits against SPEC-v2 r5.3 and keep them unless they are wrong; record any reversal with its reason.
- Canonicalize temp roots in test fixtures (`os.path.realpath(TemporaryDirectory().name)`) so the suite passes on macOS. Production's canonical-path check stays exactly as strict.
- The full bridge suite passes on the devbox and on the Mac (Mac with the short `TMPDIR`).
- v2 sender attribution: participant-era envelope keys (`address_kind`, `from_lineage`, `from_participant`) survive the relay; this is also bead post-782. Test that a v2 envelope from a participant arrives with those keys intact and without the `unknown_envelope_keys` noise.

### Wave 2: contract (D), bridge v2 deploy (F2)

**Lane D: post's output becomes a tested contract.** Needs wave 1 merged, because samples must reflect final shapes. Owns a new `contract/`, new contract tests in post, consumer contract tests in `doorbell/` and `skills/post/hooks/` (after C merges), and a porch-tui worktree off `origin/main`.

- Samples come from the real producers, never written by hand: a Rust test drives the real commands (`watch --snapshot`, `inbox`, `chat --json`, `who --json`, `profile show/list`, `channels`, `participant show`, `version`, `doctor`) against a temp store and normalizes the volatile fields: ids, timestamps, paths, digests. `POST_UPDATE_CONTRACT=1` regenerates; otherwise the test fails on drift.
- `post contract samples [--dir D]` emits the samples compiled into the binary, so consumers test against the installed version, not a vendored copy that can drift.
- Every consumer (Python doorbell, JS hooks, Porch's roomcheck and readers) tests three variants of each sample: exact, with an extra unknown optional field (which must be accepted), and with each optional field absent (which must be handled).
- A contract digest (a hash of the schema plus the samples) appears in `post version --json`. The skill carries the same digest in `skills/post/CONTRACT`, and `post doctor` warns when the served skill's digest differs from the binary's (cut pc2_79f).
- The install smoke (`launcher/install`) runs Porch's launch check and the doorbell parser against the new binary before an install counts as done.

**Lane F2: bridge v2 deploy (lead-run).** After F1 passes review (Sol, GLM, and a Grok 4.7 xhigh attack pass):

1. Install v2 on the devbox, then on the Mac. Back up v1 and its config. Stage: one v2 tick with no channels, check `health.json`, then a second tick that makes no changes.
2. Room directory: the devbox's `loom` room appears on the Mac through publication, never through a hand-added placeholder.
3. Set `channels: {"mode":"allow","allow":["loom-build"]}` on both hosts after checking `loom-build`'s contents and members. One post in each direction, a join event, and a second-tick no-op. Expect the first import to mark old history unread.

### Wave 3: doorbell supervisor (E)

Design first, in `docs/plans/doorbell-supervisor-design.md`, reviewed by Aster before any code. It needs B1 and D's samples. Requirements carried from Aster's early constraints:

- One supervisor per host, with explicitly participant-scoped subscriptions. It never substitutes one privileged aggregate watch for per-participant visibility or cursors. The existing watch projection stays the authority; the supervisor shares orchestration, not inbox identity (bead post-pe2).
- Typed sink outcomes: `accepted` (an agent prompt was taken), `notified` (the user was notified only; cmux notify is not an agent wake), `deferred` (busy or focused), `retired` (the target is gone, reported loudly, and the subscription ends), and `failed` (retryable, with a reason).
- Bind the target's session and generation, not only a reusable pane name. Never mark a notification seen before an `accepted` delivery.
- The supervisor's heartbeat must not renew an abandoned participant's lease. Today `post watch` heartbeats renew the lease (`src/commands/watch.rs` ~822–865, asserted by a test near 2179), so the lease alone cannot reveal an orphaned target. E needs independent evidence that the target is alive (herdr pane and session generation, harness session state), and supervised watches must not keep a lease alive on their own.
- Focus-waking is an opt-in per subscription, off by default (the lead's ruling; Trey may reverse it).
- The five wake paths are migrated and retired: the Python daemon, the Codex timer and monitor (launchd and systemd), and the Cursor/Grok wrappers where a sink covers them. The Claude Monitor recipe stays as the in-session option. Hooks stay as next-turn catch-up.

### Wave 4: participant DM across hosts (F3)

Design first, in `docs/plans/bridge-participant-address-design.md`, reviewed by Aster. It touches post's address parsing and routing (after wave 1 merges) and the bridge envelope. Requirements from Aster and diag-C:

- Addresses take the form `participant:<id>@<host>`, where `<host>` is validated against the enrolled registry. The id is resolved only on the destination host. Unknown or ended targets are rejected explicitly, and delivery never falls back to a workspace.
- Durable `queued`, `published`, `received`, and `rejected` states. A local send receipt never claims remote delivery.
- Remote provenance is preserved, so a reply goes back to `participant:<sender>@<origin-host>`.
- No global participant directory.

### Wave 5: watch index (B3), only if B2's numbers justify it

A per-watcher index of unseen ids, updated from file events. It is a disposable accelerator: the full reconciliation pass stays as the correctness backstop. Tests cover rename, replacement, overflow, bridge-imported files, and cursor invalidation. It lands only if the bench shows per-wake cost no longer growing with history size, with no correctness test weakened.

### Closeout (lead)

- Rewrite the post skill with `writing-for-agents` against what shipped. Corrections already found: this harness caps a Monitor at 30 minutes and notifies on expiry, and it has no `TaskOutput` tool, so the doc's recommended liveness probe does not exist here.
- Close each papercut on its own host and ledger (`papercuts resolve --file <ledger> --note …`).
- Turn the explainer into the morning closeout, then update `STATE.md` and the beads.
- Set aside: Porch mouse scrolling under herdr inside cmux. It needs Trey's terminal stack.

## Review routing

- Plan and every design: Aster, whose judgment is final.
- Code for each wave, on the merged wave diff: `delegate codex safe --model gpt-6-sol --reasoning-effort high` and `delegate omp safe --model glm`, two families. Bridge (F1, F3) also gets `delegate cursor safe` with Grok 4.7 xhigh as an attack pass.
- Each nontrivial fix round gets a fresh review. Aster may take any code review it wants instead of, or in addition to, these lanes.

## Proof of done

- `scripts/gate.sh` passes on the integrated candidate on both machines.
- Porch's full suite passes, except the known pre-existing failures (`test_canary`, the flaky `test_recovery` lock test, and two Mac-worktree draft-lock tests in `test_ui_operate`).
- The doorbell and hook suites pass, including the contract variants.
- A real end-to-end ring: a message arrives, the supervisor wakes a live devbox herdr pane, and the log shows `accepted`.
- Installed on both machines; Porch's launch check re-run after the install.
- On bridge v2, one real cross-machine message in each direction (workspace mail and `loom-build`), a second tick that makes no changes, and one `participant:<id>@<host>` DM with its receipt states.

## Review log

Each accepted or rejected review point gets one line with its reason.

- (Aster, pre-plan) E: participant-scoped subscriptions, typed sink outcomes, generation binding, seen only after accepted, no heartbeat renewal. **Accepted** into wave 3 requirements.
- (Aster, pre-plan) F: durable delivery states, exact host-qualified addressing, no workspace fallback, remote provenance. **Accepted** into wave 4.
- (Aster, pre-plan) D: samples generated from real producers; consumers test extra and absent optional fields. **Accepted** into wave 2.
- (Aster, pre-plan) A6 cursor: `O_NOFOLLOW` open, then `fstat` the descriptor. **Accepted**; A6 moved to lane A because A owns `cursor_state.rs`.
- (Aster, pre-plan) A2: reject actual unintended stdin data, never non-TTY alone; handle a silent open pipe with a bounded wait. **Accepted** as the A2 case list.
- (Aster, pre-plan) E: watch heartbeats renew leases, so E needs independent target-lifetime evidence. **Accepted.** A6: reuse the held-descriptor pattern (`O_NOFOLLOW|O_NONBLOCK|O_CLOEXEC`, metadata on the descriptor, keep `nlink==1`). **Accepted.**
- (Aster, pre-plan) A4: set-path changes discovery only, never storage or participant records. **Accepted.**

## Run log

(Rulings, pivots, and failures, appended as the night goes.)
