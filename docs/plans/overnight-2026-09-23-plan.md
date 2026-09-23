# Overnight plan 2026-09-22 → 23: fix every live post papercut

Lead: 🌙 Nightjar (Claude, Mac, `claude-83e5e99e`). Partner and final judgment: 🔭 Aster (GPT-6 Astra high, Codex, `codex-5a036212`). Channel: `post-overnight`. Brief and rulings: `docs/overnight-2026-09-23.md`. Diagnosis: `docs/triage-2026-09-22/` (the explainer wins where the diag files disagree).

**Authority.** Trey ruled that Aster's judgment decides any disagreement or important judgment call. Nightjar orchestrates, integrates, runs every gate on merged code, installs, and does live verification. **Gates:** no GitHub push and no release cut. Everything else is cleared, including installs on both machines and Forgejo pushes.

## How the work is organized

Six build lanes (A–F; A and B run as one Rust lane, R), five waves, and lead-owned ops. Fixes go back to the lane that holds the context (`delegate followup`).

**Routing (Aster's ruling, 22:32 ET).** This replaces the brief's Luna builders. Two reasons: Trey's 2026-09-22 ruling takes Luna off coding (8% on Terminal-Bench 4 at xhigh, against 60% for Opus 5.5), and the Codex work account is at 88% of its weekly limit, past the 85% "Codex hot" line, so no new Codex fan-outs and no reset credit.
- **R** (all Rust: A1–A6 and B1–B2): Opus 5.5 high, `delegate claude work --model opus --reasoning-effort high --isolation worktree --resumable`, on the Mac.
- **F1** (bridge v2 finish): Opus 5.5 high, `delegate claude work` on the devbox, in the captured worktree.
- At most two concurrent Opus coding lanes; later coding queues behind them.
- **C** (JS and Python band-aids): DeepSeek V4.1 Flash, `delegate omp work --model deepseek`. Nightjar reviews the diff.
- **Wave code review:** GLM 5.3 (`delegate omp safe --model glm`) plus Cursor Grok 4.7 xhigh (`delegate cursor safe`). Both are decorrelated from the Opus authors and neither uses the Codex meter.
- Aster's existing session stays for design decisions and targeted hard reviews.
- The cost of this routing: R serializes the Rust work, and the Opus cap queues later lanes.

**cell_bridge coexistence (Aster's ruling).** The Mac's `com.treygoff.agent-post-cell-bridge` (linux-devbox `30-control/post/cell_bridge.py`) union-syncs four channels over ssh: machineroom-devbox, deck-parity-build, front-porch, and mac-wire. It stays as it is. v2's channel allowlist gets only `loom-build`, and the two allowlists share nothing. Before v2's channels switch on, confirm that no write path is shared. Migrating the four cell-bridge channels, and fixing its inotify stream (which exits after 0s every minute), are out of scope unless they block safe coexistence. A lane's "done" is a claim until Nightjar re-runs its checks on the merged candidate.

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

- **A1 bounded post-commit lock wait.** `mark_own_message_seen` (chat.rs ~2260–2336 → cursor_state.rs ~362–394) takes `flock(LOCK_EX)` with no deadline after the message is already durable. Give only this best-effort post-commit seen update a deadline: poll `LOCK_EX|LOCK_NB` with backoff until a fixed 2s budget, then retain the trusted-lock inode and path recheck after acquisition. Other cursor transactions keep their current behavior this wave. The budget is injectable only from tests (a function parameter or `cfg(test)` seam), not a new permanent env setting. On timeout, the existing non-fatal marker-failure path prints a stderr warning naming the lock. The receipt JSON is unchanged: `ChatSendOutput` has no warnings field, and adding one is the kind of additive key that broke strict consumers. Test: hold the lock before sending, then assert exactly one durable message, an `ok:true` receipt, and the stderr warning, returned within the budget. Red-proof against current code, which hangs.
- **A2 refuse unintended stdin on a read.** Only an actual body is refused. Never refuse on "stdin is not a terminal" alone: this harness runs every command with stdin from `/dev/null`, and hooks pipe empty input. Required cases, each tested on its own:
  - `/dev/null` (a character device), and a regular file of size 0: a normal read with no delay.
  - A pipe already at EOF: a normal read.
  - A nonempty regular file, or a pipe, heredoc, or socket with at least one queued byte: a usage error with exit code 2. No channel or read state is consumed. `error.details.exact_fix` names both corrections: supply `--send`/`--body-file -` to send the body, or redirect stdin from `/dev/null` for an intentional read. A single queued byte is enough to reject; never drain to EOF.
  - A pipe that is still open and silent after a readiness wait of at most 100ms (this ambiguous case only): a distinct `input_ambiguous` error with the same two corrections and nothing consumed. Never a silent normal read, because a producer can write at 101ms.
  - A delayed writer (writes after 50ms) is caught, and a writer slower than the bound gets `input_ambiguous`, not a read. No finite probe can detect every delayed producer, and the docs must not claim one does.
  - Never auto-send.
- **A3 `who` lease wording.** The text output prints `lease=active|stale|ended`, not `state=…`, plus one footer hint: "lease is not attention; for 'did they read it' use `post chat <channel> --seen-by <id>`". JSON is unchanged: no new `lease` field, since an alias is a permanent sync obligation and strict consumers already broke on additive keys. Test the text, and test that the JSON is byte-identical in shape.
- **A4 `rooms set-path NAME PATH`.** Changes only the workspace's discovery path (cwd registration) in `rooms.json`, under the registry lock. It never moves mail storage or histories and never rewrites participant records. It reuses `rooms add`'s validation and lock ordering: a canonical absolute path, and a refusal when another room already owns the path. It always refuses `remote/*` placeholders, because converting a remote placeholder to a local room is a routing-ownership transition (queued mail, bridge owner and contest state) and is out of scope. It prints the before and after. `--dry-run` is supported. Tests cover each refusal and one success, and check that the mail directory is untouched.
- **A5 `profile list`.** Read-only: every rendering profile with its holder id, workspace, name, sigil, and lease state, computed with the same predicate `profile set` uses for uniqueness. `--json` gives `{ok, profiles:[…]}`. Text notes that occupancy is lease-dependent. Test: the listing agrees with the uniqueness refusal.
- **A6 cursor read race (moved from B, since A owns `cursor_state.rs`).** `ParticipantCursors::load` validates metadata and reads the content as two pathname operations. Open once with `O_NOFOLLOW|O_NONBLOCK|O_CLOEXEC`, take `file.metadata()` on that descriptor, and read the same descriptor, keeping the cursor-specific `nlink==1` check. Reuse the held-descriptor pattern already in `src/mailbox.rs` (~1150) and `src/lineage_store.rs` (~1051); no new generic file abstraction. Keep failing open (never auto-reset read state) and keep the retry-once and re-report marker. Tests: a symlinked cursor file is still refused; a replaced file between stat and read can no longer yield a mismatched verdict.

**Lane B: watch (Rust).** Merged into lane A as lane R (see Routing below), so one writer owns every Rust file, including `cli.rs`, `output.rs`, and `schema.rs`. Test files follow the source they cover. No whole-file formatting churn.

- **R7 origin-aware own and sender checks (Aster's finding).** `src/eligibility.rs` (~132, 176, 208, 291, 350) compares a raw `from_participant` with the local id to detect "own" messages, and `src/routing.rs` (~555) excludes the raw sender id from workspace and lineage recipients. `src/output.rs` (~179) already gives remote-origin evidence priority for reply metadata. Once bridge v2 carries `from_participant` across hosts, a remote sender whose id equals a local participant's would be suppressed as own or dropped as a recipient. Fix: a message with remote-origin evidence (the same evidence output.rs uses) is never own, and its sender id never excludes a local recipient. Collision fixture: a bridged message whose remote `from_participant` equals a local id is visible and delivered to that local participant. Keep it minimal; F3 settles the canonical host-qualified identity rule. It must land before F2 deploys v2.
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

- Scope is a small JSON fixture surface, not a contract framework.
- Samples come from the real producers, never written by hand: a Rust test drives the real commands (`watch --snapshot`, `inbox`, `chat --json`, `who --json`, `profile show/list`, `channels`, `participant show`, `version`, `doctor`) against a temp store and normalizes the volatile fields: ids, timestamps, paths, digests. Normalization keeps field presence, types, and enum values. The version sample omits or sentinels `contract_digest` and `build_sha`, which avoids a cycle. Include one real representative event per consumed variant: participant, lineage, and workspace mail; pending; remote origin; unreadable; cursor re-report; and `rooms --json`, which Porch discovery reads. `POST_UPDATE_CONTRACT=1` regenerates; otherwise the test fails on drift.
- `post contract samples [--dir D]` emits the samples compiled into the binary, so consumers test against the installed version, not a vendored copy that can drift.
- Each consumer (Python doorbell, JS hooks, Porch's roomcheck and readers) tests only the surfaces it consumes, in these variants: exact; an extra unknown optional field (accepted); each optional field absent (handled); and negative fixtures (a missing required field, a wrong type, a malformed discriminator), which must fail. Identity, provenance, and routing fields keep explicit strict rules: this is not ignore-unknown-everywhere.
- Skill and binary drift (cut pc2_79f): a checked-in marker is not proof that the served prose matches. Use an explicit bundle manifest: the hash of each covered served file (SKILL.md, references, hooks; the manifest excludes itself), generated at build and embedded in the binary. The post install procedure verifies the served path (`~/.agents/skill-library/post`, whose other surfaces are symlinks to it) against the manifest. No pervasive doctor discovery system. If this cannot be made honest tonight, report schema compatibility only and leave skill drift open.
- Product integration smoke is a dedicated `scripts/install-smoke.sh` that the post install procedure runs: Porch's launch check and the doorbell parser against the new binary. `launcher/install` stays independent of Porch.

**Lane F2: bridge v2 deploy (lead-run).** After F1 passes review (Sol, GLM, and a Grok 4.7 xhigh attack pass):

1. Install v2 on the devbox, then on the Mac. Back up v1 and its config. Stage: one v2 tick with no channels, check `health.json`, then a second tick that makes no changes.
2. Room directory: the devbox's `loom` room appears on the Mac through publication, never through a hand-added placeholder.
3. Set `channels: {"mode":"allow","allow":["loom-build"]}` on both hosts after checking `loom-build`'s contents and members. One post in each direction, a join event, and a second-tick no-op. Expect the first import to mark old history unread.

### Wave 3: doorbell supervisor (E)

Design first, in `docs/plans/doorbell-supervisor-design.md`, reviewed by Aster before any code. It needs B1 and D's samples. Requirements carried from Aster's early constraints:

- One supervisor per host, with explicitly participant-scoped subscriptions. It never substitutes one privileged aggregate watch for per-participant visibility or cursors. The existing watch projection stays the authority; the supervisor shares orchestration, not inbox identity (bead post-pe2).
- One host supervisor owns registrations and health, reuses participant-scoped post projections, and keeps sinks small. A cmux-only notification cannot satisfy an idle-agent guarantee.
- Delivery contract: at least once, with possible duplicate metadata notices. The window between sink acceptance and dedupe persistence is ambiguous, and the design says so. Exactly-once wake is not achievable and not promised.
- Cases the design must cover: registration replacement and restart, target-generation mismatch, lease expiry while a valid idle target remains, migration fences, and bounded retries per broken subscription.
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

A per-watcher index of unseen ids, updated from file events. It is a disposable accelerator: the full reconciliation pass stays as the correctness backstop. Tests cover rename, replacement, overflow, bridge-imported files, and cursor invalidation. Measure event-driven wake cost separately from periodic full-reconciliation cost; the full backstop legitimately scales with history. Success means measured lower steady-state work, a bounded reconciliation cadence, and no missed eligible events, with no correctness test weakened. No persistent index or database unless the measurements require it.

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
- A real end-to-end ring: a nonce message arrives, the supervisor wakes a live devbox herdr pane, and the real recipient agent acknowledges the nonce under its own identity, matching the event and the sink receipt. A sink `accepted` alone proves acceptance, not that a turn ran.
- Installed on both machines; Porch's launch check re-run after the install.
- On bridge v2, one real cross-machine message in each direction (workspace mail and `loom-build`), and a second tick that leaves canonical mail, channel, and receipt state unchanged. Health timestamps and telemetry may change.
- `participant:<id>@<host>` DMs in both directions, plus restart and replay, duplicate delivery, rejection of unknown and ended recipients, and reply target and provenance.

## Review log

Each accepted or rejected review point gets one line with its reason.

- (Aster, pre-plan) E: participant-scoped subscriptions, typed sink outcomes, generation binding, seen only after accepted, no heartbeat renewal. **Accepted** into wave 3 requirements.
- (Aster, pre-plan) F: durable delivery states, exact host-qualified addressing, no workspace fallback, remote provenance. **Accepted** into wave 4.
- (Aster, pre-plan) D: samples generated from real producers; consumers test extra and absent optional fields. **Accepted** into wave 2.
- (Aster, pre-plan) A6 cursor: `O_NOFOLLOW` open, then `fstat` the descriptor. **Accepted**; A6 moved to lane A because A owns `cursor_state.rs`.
- (Aster, pre-plan) A2: reject actual unintended stdin data, never non-TTY alone; handle a silent open pipe with a bounded wait. **Accepted** as the A2 case list.
- (Aster, pre-plan) E: watch heartbeats renew leases, so E needs independent target-lifetime evidence. **Accepted.** A6: reuse the held-descriptor pattern (`O_NOFOLLOW|O_NONBLOCK|O_CLOEXEC`, metadata on the descriptor, keep `nlink==1`). **Accepted.**
- (Aster, pre-plan) A4: set-path changes discovery only, never storage or participant records. **Accepted.**
- (Aster, plan review 50646f0) P1 stdin: an open silent pipe is not known-empty; after the bound it fails `input_ambiguous`, never a silent read. **Accepted.**
- (Aster) P2: cut `--from-remote`; set-path always refuses placeholders. **Accepted.**
- (Aster) P3: no JSON `lease` alias; text only. **Accepted.**
- (Aster) P4: no digest cycle; a checked-in marker is not proof of served prose, so use an explicit bundle manifest verified at install. **Accepted.**
- (Aster) P5: unknown optional fields are accepted, but missing required fields, wrong types, and bad discriminators fail; negative fixtures; each consumer tests only what it consumes; cover rooms. **Accepted.**
- (Aster) P6: proof needs a nonce plus acknowledgement from the real recipient; bridge idempotence compares canonical state, not telemetry; DM proof covers both directions, replay, duplicates, rejections, and provenance. **Accepted.**
- (Aster) A1: deadline only on the post-commit seen update, test-injectable budget, keep the inode and path recheck. **Accepted.**
- (Aster) E: at-least-once delivery with honest ambiguity; replacement, restart, generation mismatch, lease expiry, fences, bounded retries. **Accepted.**
- (Aster) B3: wake cost measured separately from reconciliation; no index or database unless measurements require it. **Accepted.**
- (Aster) D: install smoke is a dedicated `scripts/install-smoke.sh`; `launcher/install` stays independent of Porch. **Accepted.**
- (Aster, ruling) Builder re-route to Opus R and F1, DeepSeek C, GLM and Grok review. **Ruled GO.**
- (Aster, ruling) cell_bridge coexists with disjoint allowlists; v2 gets loom-build only. **Ruled.**

## Run log

(Rulings, pivots, and failures, appended as the night goes.)

- 22:27 O1 done: bridge-v2 staged edits captured in devbox worktree `~/Code/claude-space-wt-bridge-v2`, branch `bridge-v2-ship` at 9328671 (the staged diff's sha256 matched the original's).
- 22:28 O2 done: 15 dead-pane timers disabled after a herdr re-check; the 4 live ones (atlas-quill, atlas-reviewer, linden, vale-recall) are untouched. Unit files kept.
- 22:29 O3: the Mac gate passes at main (cargo 1.98.1, node 26.9.0, python 3.14.7). The bridge suite must run from the repo root as `python3 -m unittest post-bridge.tests.<module>` with `POST_BIN` set; a bare `discover` inside `tests/` fails on relative imports.
- 22:30 O4: `agent-post-cell-bridge` identified (see cell_bridge coexistence above).
- 22:32 Ruling (Aster): routing changed from Luna to Opus, DeepSeek, GLM, and Grok. Reason: Trey's same-day Luna-coding ruling and the Codex meter at 88%. Downside: Rust work is serialized.
