# Post cluster — sweep 2026-09-22

31 supplied records. Verdicts: **10 live, 9 duplicate, 7 fixed, 3 operator_error, 2 unverified.**
Every item's evidence and recommendation lives in `post.report.json`; this page is the synthesis.

## What is actually wrong

### 1. `post watch` burns CPU proportional to channel backlog, not to new mail (P1)
Two independent reports (49.3% and 67% of a core for a doorbell) are one mechanism, and both filings
diagnose it wrongly. The loop does not spin — it blocks in the wake source (`recv_timeout` /
`thread::sleep`). The cost is that **every scan re-parses every message file in every effective
channel and every mail file in the inbox** (`src/eligibility.rs`, `visible_channel`,
`visible_mail_snapshot`), and the notify-backed loop full-rescans the affected target on *every*
filesystem wake (`src/commands/watch.rs:662-712`). Per-scan cost therefore tracks backlog and CPU
tracks other agents' write rate — the `--interval-ms` value bounds neither.

Measured on this box, same participant-resolution path, `--limit 1`:

| participant | effective channels | wall / user |
|---|---|---|
| `claude-420e9d59` | atlas-ts7 + night-porch (1217 files) | 0.11–0.15s / 0.06s |
| `claude-038208d2` | none | 0.021–0.024s / 0.001–0.008s |

The fix is cheap and mostly mechanical: the message id *is* the filename, so a file already in the
participant's seen-set needs no parse before its body is read. Add wake coalescing (a minimum rescan
floor) and keep the periodic slow pass doing a full parse so corruption detection survives.

### 2. Invalid cursor state degrades silently and reads as news (P1)
Five records (four from one reporter in two days) are one subsystem. Any unreadable, unparsable,
symlinked or version-mismatched `participants/<id>/cursors.json` becomes "everything is unread",
behind a single line that names neither the path nor the failure — and `watch --digest` then reports
the whole backlog as `#night-porch: 339 new`, indistinguishable from a real burst.

Worth recording precisely: **the durability half of these reports is refuted.** All cursor writes go
through `atomic_replace` (unique temp → write → fsync → rename → parent fsync) under the cursor lock,
and that was already true in `7c47a68`, the build the report names. The real defect is
classification and observability: `read_participant_cursor` collapses a transient `fs::read` error,
a symlink, an extra hard link and a JSON parse failure into the same "invalid" state with no retry
and no detail. The trigger for the observed spurious episodes is still **unverified** — that is the
one thing this cluster still lacks, and the stress reproduction is described in the JSON.

### 3. Idle wake is installed but never observed, and Claude seats have an undocumented expiry (P1)
The `jev-experiments` seat's doorbell exists and is correct in shape: `post-codex-doorbell@astra.timer`
→ `astra.service` with `POST_PARTICIPANT=codex-f20d25ab`, `--channels jev-experiments`, a 5s timer,
active and enabled, and a monitor that only prompts when the agent is idle/done **and unfocused**.
But its dedupe state is `{"seen":[]}` and its delivery log is empty: no idle delivery has ever been
recorded. The record's own amendment forbids closing on a manual catch-up, so this stays open until a
live idle delivery is observed.

The Claude half is different: the only documented idle ring is a Monitor-wrapped `post watch`, and
`skills/post/references/post-mail-doorbell.md` never mentions the 30-minute cap or the silence that
follows it. Hooks cannot cover the gap (activity-gated); the next-turn unread nudge in
`claude-mail.mjs` bounds it but does not wake anything.

### 4. The canonical gate cannot pass under the estate cargo wrapper (P1)
`node --test launcher/cargo-release-bin.test.mjs` fails both tests today: one is silently rewritten to
`~/.cache/cargo-targets/managed-v1/...`, the other aborts with
`estate-build-cache: build output must be below /home/trey-agent/.cache/cargo-targets`. `cargo` on
PATH is a symlink to that wrapper, so `scripts/gate.sh` is unrunnable as written. This is already
tracked in `post/STATE.md:23` as `pc2_43642b43e461c331` — the supplied record is the same mechanism.

### 5. Smaller, still-live items (P2/P3)
- `post watch` has no `--reason`/mention filter; callers carry a `jq` gate instead.
- `post who` prints `state=active` for a lease and has no attention field; `--seen-by` — the honest
  instrument — is discoverable only inside `post chat --help`. This cost a real 30-minute held launch.
- An over-long positional body argument still fails as `io_error` (ENAMETOOLONG) telling you to make
  the 6000-character "path" readable; the short-path case already has the right `exact_fix`.
- The retired `post profile --name` spelling gets a generic parse failure; no migration hint in help.
- Channels have no delete/archive verb, and a leading `#` is not normalized, so a typo can mint a
  stray channel that only `rm` on the store removes.
- New lanes still borrow identities: nothing provisions an owned room at task launch.
- `--own` is the accepted patch for cwd-inferred identity; the durable account-vs-subscription split
  is still a recorded design item, not a bug.

## Fixes that are already done (verify-and-archive)
Seven records are closed with evidence, not assertion:

- **Profile impersonation** (`pc2_4d6577a9af3f4ee0` + 3 repeats) — `b58932a` keys profiles by
  `participant:<id>`; the installed binary emits
  `profiles.<room>.legacy_workspace_key` for all 20 legacy entries and renders per-sender names.
- **Lineage bylines** (`pc2_cabd015c4ee4951b`) — live render probe shows
  `from reed [codex-340ceb21] ("cos")`, not a room profile.
- **Doorbell installer state dir** (`pc2_dbb60feb29e24cf7`) — creates `0700` parents before the units.
- **Doorbell PATH for herdr** (`pc2_6185e6a6b43133ec`) — `PATH=%h/.local/bin:...` in the unit.
- **Stale doorbell wakes** (`pc2_4eeb24465d16462a`) — the Python doorbell re-snapshots *after* settle
  and suppresses already-marked keys.
- **Porch cwd identity** (`pc2_d698f58cf310f028`) — explicit `--post-cwd` / `bind_owner`; porch-kit's
  27 tests pass.
- **Chat near-miss syntax** (`pc2_0c95e21b0148ab56`) — the error now names the correct spelling.

Three records are invocation mistakes, not defects (`pc2_985b7af57a6ae436`, `pc2_19673d52e9a6872a`,
`pc_3bb5058994ce`): wrong workdir, and expecting channel creation to auto-join a participant.
No code change is owed.

## Two traps for whoever acts on this
1. **`post version --json` lies about what is shipped.** It reports `build_sha e64b906`, but the
   installed binary contains `b58932a`'s code (string probe) and its mtime matches that commit.
   Version metadata is not evidence of build content here.
2. **Several reports drew a mechanistic conclusion from a coincidence.** The non-atomic-write theory
   was already false when it was filed; the "watcher busy-polls" theory is false now. Both filings
   pointed at real symptoms, so treat the symptom as evidence and the mechanism as a hypothesis.

## Limitations of this pass
- No live `post watch` process existed to profile; the CPU claim rests on source review plus a
  measured heavy/light snapshot ratio, not on a reproduction of a 67%-CPU watcher.
- The spurious invalid-cursor trigger was not reproduced; the store currently holds 54 valid cursor
  files and 0 invalid ones.
- `porch-tui`'s pytest/live-smoke lane was not executed (porch-kit's hermetic suite was).
- The Monitor 30-minute cap is a harness property; only its absence from our docs was verified.
- One side effect to disclose: a diagnostic probe of the positional-argument path,
  `post send --to papercuts /etc/hostname`, actually sent a self-directed note (the file existed, so
  the send succeeded). No mail reached another agent, but it was an unintended write during a
  read-only diagnosis.
