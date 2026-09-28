# Lane 3: papercut chunk 3 (43 cuts, 2026-08-16..2026-09-05)

Checked against `/Users/treygoff/Code/post` at HEAD `f9239eb` (installed `post 0.9.0`). "Verified" means I read the code, CHANGELOG, commit, or a read-only help/parse-failure output. "Inferred" means I did not confirm it. The ledger's `resolved` flag is treated as no evidence.

One timing point governs most verdicts. The cuts were logged before the participants architecture (2026-09-16 to 09-23) and before the 2026-08-25 fix day. The 08-25 fix day was `ca430aa`, `078f6bd`, `def9279`, `238c1c4`, `b9f4405`, `8549036`. So a fix commit shortly after a cut usually means the cut caused it.

## Summary table

| # | Cluster | Cuts | Status |
|---|---|---|---|
| 1 | Sender identity is inferred from place (cwd, POST_FROM, room name), and identity is fused with subscription | 6c4e, ed23, 54b1, 66e0, 556c, 19673, 0afda, 5336 | FIXED-VERIFIED (participants), small residual LIVE |
| 2 | Channel send and read syntax hard to find; errors and help pointed the wrong way | 892d, 499b, 9a7d, 0c95, 8a50, ff8d, b03a, 93f8, 9ce0 | FIXED-VERIFIED, one alias gap |
| 3 | `post send` splits its output contract: JSON error envelope, prose success | 6659 | LIVE (by design; guidance gap) |
| 4 | Send blocks for minutes on a lock after the message is already durable | 1dea | FIXED-VERIFIED for that lock; same class LIVE elsewhere |
| 5 | The crossed_send guard: a wall of text, a wrong fix, cursor confusion, a bypass loop | 0dfb, 6b4d, 4ba1, dd75, bb29 | Mostly FIXED-VERIFIED; dd75/bb29 UNCLEAR; bypass steering LIVE |
| 6 | Watch and doorbell noise and staleness (self-echo, no filter, stale wake, what `who` means) | 0112, 15f4, d91f, 4eeb, 3fdb | FIXED-VERIFIED; `who` wording partly LIVE |
| 7 | Doorbell installer and service environment assumptions | 3be0, dbb6, 6185 | FIXED-VERIFIED |
| 8 | Gate and test-harness fragility (flake, hard-coded target dir, macOS tmp path) | 4cfd, 3e9e, 9c12 | FIXED-VERIFIED (4cfd, 3e9e); 9c12 UNCLEAR (other repo) |
| 9 | doctor: a fresh store looks broken, and old corruption masks new findings | 97e0, f4d1 | 97e0 FIXED-VERIFIED; f4d1 LIVE/UNCLEAR |

Not about post, skipped: `5c1a` (delegate workflow `--notify`), `e5e2` (delegate workflow close agent kills PIDs), `f4c7` (OpenClaw stale gateway dist), `a3a7` (the `tell` CLI has no `--json`).

Only tangentially about post:
- `e929` is a writing-plans workflow KeyError. Its second half, a Monitor pipeline ending in `cut` that block-buffers, belongs to cluster 6 as a guidance gap.
- `c71a` is agent shell hygiene: `cmd; rm brief.txt` deletes the input when the command fails. The global shell-footguns rule already covers it. Its trigger, the cwd-bound `unknown_room`, is cluster 1.

---

## 1. Sender identity is inferred from place, and identity is fused with subscription

**Cuts:** 6c4e, ed23, 54b1, 66e0, 556c, 19673, 0afda, 5336 (5336 is also the design note behind 0112 and 15f4 in cluster 6).

**Symptoms**
- Two agents resolved room 'workspace', and one profile rename changed the display name for both (6c4e).
- One `POST_FROM=sol-devbox` pin covered every lane in the linux-devbox checkout, and a Mac shell inherited the same pin. Fable, Sol and others posted as "Sol Devbox" (ed23, 54b1, 66e0).
- `post chat stack-upgrade --send` from `~/Code/hq` silently sent as `hq-devbox`, the coordinator. Only the unrelated crossed_send guard stopped it (556c).
- `post chat` from a non-room cwd gave `unknown_room` with no hint (0afda). A `post chat` from `life` resolved `cos-devbox` instead of the guest research room (19673).

**Status: FIXED-VERIFIED for the mechanism; small residual LIVE.**
- Participant model (2026-09-16 to 09-23): `docs/PARTICIPANTS.md` and `docs/IDENTITY.md`, "superseded in part". Only `post participant bind` mints an actor. `skills/post/references/identity.md` says: "cwd alone never moves a bound participant. Workspace context never chooses the actor."
- Code: `src/channel.rs:152-197` (`acting_room`) takes the acting room from the bound participant's workspace. A `POST_FROM` pin that disagrees with the binding is a hard `invalid_argument`, in both `src/channel.rs:166-175` and `src/commands/send.rs:143-158`. An unbound writer fails with the binding fix (README, around line 165).
- Profiles are keyed per participant (`b58932a`, 2026-09-21; CHANGELOG "Profiles belong to one participant"). It names the "2026-09-22 collision where a newly bound participant in a shared workspace was stamped with a peer's display name", which is 6c4e's failure.
- Duplicate room names across hosts are now refused, with a suffix fix. `post rooms rename` exists (`7cb9384`). The eight duplicate names were renamed on 2026-09-23 (STATE.md).
- Echoing identity before the write: `238c1c4` (2026-08-25) names the directory identity came from. `src/commands/chat.rs:2345-2351` prints `post: sending to #<ch> as room '<x>' (<provenance>)` to stderr before the append. `send.rs:166-176` does the same, naming the participant.

**Residual LIVE (verified):**
- The shared reply address (workspace) is still the `from` on every message. Every participant bound to `sol-devbox` still posts `from=sol-devbox`. What now distinguishes them is the participant id and per-participant profile in the byline. The first bind still takes its workspace from `--workspace`, else `POST_FROM`, else cwd (`identity.md`, "Binding"), so a stale or global `POST_FROM` still sets the workspace silently on first bind.
- STATE.md open loop: sessions launched before a repo rename keep their old pin until restart.
- Bead `post-b6o` (P3, open): warn when a cwd-inferred bind shares a workspace across lineages. It was not implemented when I checked.
- The `chat` echo prints the room, not the participant id. The `send` echo prints both.

**Root cause (verified):** a design that invites the mistake. `from` was a location (a room), and cwd or env picked it. `docs/IDENTITY.md` says this in its own "Why": "A from-field is location, not identity." The 08-25 `5336` cut correctly diagnosed that identity and subscription were one concept. `--own` (`b9f4405`) was the patch; participants were the durable fix. The `POST_FROM` in the shell env of a whole checkout or account (ed23, 66e0) is environment outside post: a launcher or profile exports it globally. That was a documented feature of `launcher/agent-session`, not a bug.

**Fix shape**
- Root: already done. The remaining root gap is that a workspace is still a shared `from`. Either make the byline participant-first everywhere (mostly done for text), or give each participant its own reply address by default and make a workspace opt-in. That is a design call, not a bug.
- Cheap: finish `post-b6o` (a warning on inferred cross-lineage bind). Have `chat --send` echo the participant id like `send` does. Never export `POST_FROM` from a shell profile; only the per-launch helper sets it. That last point is estate hygiene, not post.

---

## 2. Channel send and read syntax hard to find

**Cuts:** 892d (apostrophe in `--body`), 499b (`send --to <channel>` error points at rooms), 9a7d (help shows only read forms), 0c95 (three near-miss syntaxes), 8a50 and ff8d (`--body-file -` rejected while bare stdin works), b03a (`post read <channel-msg-id>` error points at `post inbox`), 93f8 and 9ce0 (`post profile --name` is obsolete).

**Status: FIXED-VERIFIED; one small residual.**

| Cut | Fix | Evidence |
|---|---|---|
| 892d, 9a7d | `chat --help` leads with the safe stdin form and warns that a body on argv is parsed by the shell | `def9279` (08-25). Live `post chat --help` shows "SAFEST" first and the backtick warning. `post send --help` warns about `$1.63B` and apostrophes. |
| 499b, 0c95 | `post send --to <channel>` now says "is a channel, not a room" and names `post chat <ch> --send`. It accepts a leading `#`. | `src/commands/send.rs:213-249` (`238c1c4`) |
| 0c95 (`--send <text>` read as a FILE path) | A missing body-file path is `invalid_argument` with an `exact_fix` of `--body '<that text>'` | `src/commands/send.rs:869-893` |
| 8a50, ff8d | `--body-file -` reads stdin. It is in `chat --help`. | CHANGELOG line 427; help output; `b73427f` |
| b03a | `post read <channel-msg-id>` says "id names a channel message, not mail" | `src/commands/read.rs:225, 967-986` (`8549036`) |
| 93f8, 9ce0 | `post profile --name` gets a parse-failure fix naming `post profile set --name <NAME>`. I ran it: it returned `suggested_fix` with that spelling. | `src/app.rs:118-131` |

**Residual (verified):** no `post send --channel` alias and no way to send to a channel from `post send`. The code comment at `send.rs:220-232` rejects this on purpose: a `post send` invocation cannot carry `--kind`, `--from` or a stdin body across to `chat`, so an auto-built correction would silently drop them.

**Root cause:** a design invite plus discoverability. Direct mail and channel send are two verbs. Channel send is a flag on the read command (`chat --send`). Each surface was taught by its own error only after an agent hit it. The cuts came from agents guessing from `post send`. The papercuts themselves prompted the fixes, which landed in the same day or week.

**Fix shape**
- Root, if it recurs: make the addressing symmetric. Let `post send --to '#<channel>'` deliver as a channel send when the flags map cleanly, and refuse with the current message when they do not. The existing comment argues against auto-correcting a failed command, not against accepting the form up front.
- Otherwise leave it: help and errors now teach. The band-aid is already in.
- Do not add a `--channel` alias, which adds a third spelling.

---

## 3. `post send` output contract is split

**Cut:** 6659. An agent piped `post send` into a JSON parser. Errors are JSON, success is prose, so the parse failure looked like a send failure and the agent sent twice.

**Status: LIVE, by design.**
- Verified in `src/app.rs:24-46`: every error (and every clap parse failure) goes through `output::write_error` as a JSON envelope on stderr with a nonzero exit, whether or not `--json` was passed.
- Success is text unless `--json` (README around lines 269-274: "`--json` switches `send`, `read`, `chat`, `catchup`, and `search` from text to JSON").
- `--json` exists on `send` (`post send --help`).

**Root cause:** mostly a guidance gap, plus an asymmetry in the design. The fields for the right answer exist: the exit code is the verdict and `--json` gives a machine success shape. The agent parsed stdout for a verdict and had merged stderr into it. Not verified: exactly how the agent piped it (inferred).

**Fix shape**
- Cheap: one line in `skills/post/SKILL.md` and `commands.md`: "the exit code is the verdict; if you parse, pass `--json` on the same call. Never re-send on a parse failure. Check `post inbox` or the receipt first."
- Root, optional: an env default (`POST_OUTPUT=json`) so an agent shell sets machine mode once and gets the same shape for success and failure. Cheaper than changing the default and safe for humans.

---

## 4. Send blocks for minutes on a lock after the message is durable

**Cut:** 1dea. A channel send waited about 11 minutes and then returned a normal receipt.

**Status: FIXED-VERIFIED for the identified lock; same class still LIVE elsewhere.**
- Mechanism (verified, from `docs/triage-2026-09-22/diag-B.md` and `e6ee2a9`): after the append, `mark_own_message_seen` took a blocking `flock(LOCK_EX)` on the sender's cursor file with no deadline. Any other holder of that lock kept a finished send waiting. That the 08-19 incident was this lock is inferred; the commit shows the mechanism, not the incident.
- Fix: `e6ee2a9` (2026-09-22, bead `post-aqw.1`, A1). `src/commands/chat.rs:2470-2500` and `src/cursor_state.rs:438-456` poll `LOCK_EX|LOCK_NB` with capped backoff for 2s. On timeout the send still returns its receipt, with a stderr warning naming the lock. It was red-proofed: the new cli test hangs on the old code.
- Still unbounded (verified by grep): 10 `libc::flock` sites in `src`, exactly one with `LOCK_NB` (`cursor_state.rs:456`). That leaves the rename lock (`src/mailbox.rs:258-262`, shared by every `send`/`read`), the rooms lock, the channels lock, and other cursor transactions. `post-4th` (open) notes the gate has no timeout either.

**Root cause:** no shared policy for blocking file locks. The best-effort post-commit step took a mutex as if it were the critical path.

**Fix shape**
- Root: one bounded-lock helper for every flock, each caller choosing its own timeout outcome. A committed send must still report success. A pre-commit lock should fail with a typed, retryable `lock_busy` naming the holder's path. This is what diag-B recommended and I did not find it done.
- Band-aid: what shipped. It is appropriate for this incident.

---

## 5. The crossed_send guard

**Cuts:** 0dfb (rejection embeds ~15KB of bodies; joining a busy channel makes the first send refuse), 6b4d (`exact_fix` was a `<PLACEHOLDER>` template that destroys the heredoc body), 4ba1 (bridged earlier-id message lands after own send advanced the cursor, so `post chat` reports nothing), dd75 (a read via `--history` leaves the message "unseen and addressed to you" forever; every send needs `--anyway`), bb29 (the same for mentions).

**Status**
- 0dfb, FIXED-VERIFIED: `ca430aa` (2026-08-25). `src/channel.rs:597-790`: refuse only when an unseen message is targeted (an @mention of the sender's room, a `--re` reply to its own message, or an owner message). Otherwise warn and deliver. The preview is targeted messages only, first line each, capped at `PREVIEW_CAP`. Join-from-now (`a3ee781`) also stops a fresh joiner from counting the backlog as unseen. Every decision is logged to `crossed-send.jsonl`.
- 6b4d, FIXED-VERIFIED: `078f6bd`. The funnel `AppError::exact_fix` (`src/error.rs:246-259`) `debug_assert!`s there is no placeholder. Caveat (verified): it is a `debug_assert`, so it enforces in test and debug builds only. A release binary would ship a placeholder that slipped through. The tests run in debug, so the risk is low but not zero.
- 4ba1, FIXED-VERIFIED (by code reading): v2 seen-sets (`8a8bd2a`, 2026-08-21, the same day as the cut). `src/commands/chat.rs:2470-2500`: a send marks only its own id seen; others stay unseen "so nothing is swallowed by this mark". The old max-cursor model could jump past an earlier-id bridged message; a set cannot.
- dd75 and bb29, UNCLEAR:
  - As described ("advances PAST it without clearing"), the mechanism does not fit the current code. Verified: `--history/--since` are cursorless (`UnreadRule::AfterId`). A consuming read marks exactly the ids it emitted (`chat.rs:983-1010`) and the guard's set is the same `unread_channel` predicate (`src/eligibility.rs:355-370`). So a later plain read should clear a targeted message.
  - Plausible live variant (inferred): a plain read pages the oldest 25 unread. With a deep backlog, a mention beyond that window stays unseen after any number of `--history` reads, and every send is refused. I did not reproduce this; I did not run any write commands.
- The bypass loop is LIVE by design (verified): the refusal's `exact_fix` is the `--anyway` command (`channel.rs:693-698`, and the prose says "adding `--anyway`"). It steers straight to the escape hatch. The exact-id verbs `post chat <ch> --ack <id>` and `--discard-through` exist (CHANGELOG "Narrow exact-id acknowledgements") but the refusal does not name them.

**Root cause:** the guard measures consumption (an id in the seen-set), while the agent's mental model is display ("I read it"). The recovery text offers the bypass, not the acknowledgement. The earlier failures (dumping bodies, a template fix, cursor jumps) were the code having bugs; they are fixed.

**Fix shape**
- Root: have the refusal offer `--ack <id>` for each targeted id, and list the ids in the message. Have `--history` mark a message seen only if the caller asks (`--history N --mark-seen`). Read `crossed-send.jsonl` for the `anyway_after_ms` distribution before tuning further; `ca430aa` recorded 6050 ms on its first sample, too short to have read anything.
- Band-aid: make `contains_placeholder` a runtime check in release too (one line), so 6b4d cannot recur through a rarely-tested path.

---

## 6. Watch and doorbell noise and staleness

**Cuts:** 0112 and 15f4 (self-echo suppression is per room, so a multi-room watch rings on its own sends), d91f (no mention-only filter), 4eeb (stale "new mail" wake after the channel was read while the agent was busy), 3fdb (`who` `live_watch` misread as "agent alive"). Plus the second half of `e929`: a Monitor pipeline ending in `cut` block-buffers and swallows events.

**Status**
- 0112, 15f4, FIXED-VERIFIED: `b9f4405` (2026-08-25) added `--own <ROOM>`; `post watch --help`: "watching a room is not the same as being it". Union suppression was tried and rejected because it made observers deaf, which is the 5336 story. In the participant model own-message detection is per participant (`src/commands/watch.rs:1773`, `message_is_own`), so one session following N channels is not rung by its own sends.
- d91f, FIXED-VERIFIED: `e7fbdd3` (2026-09-22) repeatable `post watch --reason mail|channel|mention` (`watch.rs:470`; `skills/post/references/post-mail-doorbell.md` recommends `--reason mail --reason mention`).
- 4eeb, FIXED-VERIFIED: `4c72c57` (2026-09-05, "refresh doorbell unread snapshot after agent settles"), then `1fd752f` binds the settle-before-snapshot order in tests. `post-y2q` is still `in_progress` "left open deliberately". Since 2026-09-23 the resident supervisor (`skills/post/hooks/doorbell-supervisor.mjs`) scans afresh and takes over from the per-agent daemon and timers. I did not verify that path end to end.
- 3fdb, PARTIAL:
  - `who --text` now labels `lease=active|stale|ended` plus a hint "a lease is not attention; use `post chat <ch> --seen-by <id>`" (CHANGELOG, Unreleased).
  - `who` still has a `live_watch` field and the schema entry (`src/commands/schema.rs:424`) is a bare field list with no note. `live_watch` is a per-participant heartbeat file (`src/presence.rs`), i.e. "some local process is watching", not "the agent is alive". The `who` JSON `live_watch` still carries no such caveat.
- e929 second half, guidance gap (LIVE): nothing in `post-mail-doorbell.md` or `SKILL.md` warns about piping a Monitor watch through `cut`/`sed`/`jq`. `--reason` removes the reason to pipe, but nothing says "do not pipe". Not a post bug.

**Root cause:** the wake layer has several consumers (native watch, Monitor, a Python daemon, timers, Cursor/Grok hooks), each with its own policy. `docs/triage-2026-09-22/diag-A.md` says the same. The supervisor consolidation is the structural fix and is done for herdr hosts; `post-pe2` (idle monitor still workspace-aggregate) is open.

**Fix shape**
- Root: done (`--own`, `--reason`, participant own, the supervisor). Nothing more to add here.
- Cheap: one sentence on the `live_watch` schema line ("a local process holds a watch for this participant; not proof the agent is attending"). One line in the doorbell reference: "Do not pipe a Monitor watch; if you must, use `stdbuf -oL`/`--line-buffered`".

---

## 7. Doorbell installer and service environment assumptions

**Cuts:** 3be0 and dbb6 (installer says installed; the first service tick exits 209/STDOUT because `~/.local/state/post-codex-doorbell` does not exist), 6185 (the systemd service has no PATH for user-installed `herdr`: `FileNotFoundError: herdr`).

**Status: FIXED-VERIFIED.**
- State directory: `c45baef` (2026-09-05, same day). `skills/post/hooks/install-systemd-doorbell.mjs:649-663` creates a private, non-symlink state dir before enabling the timer. A test was added.
- PATH: `65defbe` (2026-09-05). `doorbell/post-doorbell@.service:13` has `Environment="PATH=%h/.local/bin:/usr/local/bin:/usr/bin:/bin"`; `doorbell/post-doorbell:327-331` checks `herdr` and `post` up front with a message that names the fix. The legacy per-agent unit pins the herdr binary through `POST_CODEX_NOTIFY_HERDR_BIN` (`install-systemd-doorbell.mjs:481`).
- Successor: `install-doorbell-supervisor.mjs` (`c101b17`, 2026-09-23) sets PATH itself (line 286), does `mkdirSync` for the log parent (line 787), and "waits for the lock and a healthy first tick" (CHANGELOG). That is the "verify a real service tick" the cuts asked for. The legacy timers are migrated off one at a time.

**Root cause (verified for the mechanism):** the installers' tests checked the generated unit text, not a live service start. Systemd runs with a near-empty env and does not create `StandardOutput=append:` parents. Environment assumptions that a shell hides. The cuts also say "preflight/tests missed a fresh runtime directory".

**Fix shape:** the shipped fix is the right one: install, start, and assert one healthy tick. Band-aid (mkdir plus PATH) was applied first and is fine. Nothing further, except that the legacy `install-systemd-doorbell.mjs` path remains for hosts not yet migrated; it is covered by the migration path.

---

## 8. Gate and test-harness fragility

**Cuts:** 4cfd (`migration_fence` flake under load), 3e9e (gate builds under a configured Cargo target-dir, then launcher tests and the schema look for `target/release/post`), 9c12 (post-bridge Mac tests fail because Python `TMPDIR` is `/var/folders`, not `/private/var`).

**Status**
- 4cfd, FIXED-VERIFIED, band-aid quality: `8549036` (2026-08-25) raised the handshake bound from 2s to 60s (`src/migration_fence.rs:868, 944`, with a comment calling it "a DEADLOCK GUARD, not a timing assertion"). The other rendezvous timeouts in `src` and `tests` are 15s or more (`mailbox.rs:2574`, 15s). The cut's own recommendation, removing the wall clock from the handshake, was not done. 60s is defensible.
- 3e9e, FIXED-VERIFIED: `98d53c4` (2026-09-05) added `scripts/cargo-release-bin.mjs`, which resolves the artifact through `cargo metadata`. `scripts/gate.sh` runs `launcher/cargo-release-bin.test.mjs`.
- 9c12, UNCLEAR: this is `~/Code/claude-space/post-bridge`, a sibling repo, not the post CLI. `diag-B.md` says fix the fixture, never weaken the production canonical-path check. I found no `TMPDIR` note in the bridge README, and I did not check whether the harness was changed. The bridge ship commit (`3221aa4`) records a "macOS deviation" (not read).
- Open, adjacent: `post-4th` (P3): `scripts/gate.sh` has no timeout around `cargo test`, so one hanging test wedges the gate.

**Root cause:** environment and tooling assumptions inside tests (wall-clock, paths), not product bugs. 3e9e came from the estate's Cargo cache wrapper. The lesson in 4cfd is worth keeping: a gate that flakes teaches "re-run until green".

**Fix shape:** as shipped. For 4cfd, the durable alternative is a handshake with no timeout (channel plus join), leaving a hang to `post-4th`'s gate-level timeout. That is the simple root fix and it closes 4cfd and `post-4th` together.

---

## 9. doctor: a fresh store looks broken, and old corruption masks new findings

**Cuts:** 97e0 (a fresh `post doctor --fix` writes an empty `rooms.json` and doctor then reports it invalid and not fixable, so a `set -e` bootstrap aborts), f4d1 (a devagent cell's doctor reports `channel-state.json` "invalid map" in 9 rooms, pre-existing, exits "broken", and hides real findings).

**Status**
- 97e0, FIXED-VERIFIED: `52ec889` (2026-08-31, "doctor: healthy empty store"). `src/commands/doctor.rs:943-951` reports an empty `{}` `rooms.json` as `config.rooms_empty`, severity Info, which does not count as a finding (`doctor.rs:195-214`). Nit (verified): the `config.rooms_invalid` message (line 983) still says "non-empty JSON object", stale wording.
- f4d1, LIVE/UNCLEAR:
  - `94579be` (09-22) made a legacy invalid `channel-state.json` a Warning rather than an Error when `participants/<id>/cursors.json` already exists (`doctor.rs:842-860`). That covers this cut's symptom in part.
  - Doctor still exits 1 for any non-Info finding (`doctor.rs:46`), has no acknowledge/suppress facility, and `broken` vs `degraded` is only Error vs Warning. I could not tell why nine rooms had an invalid map. A v1 shape holds `{channel: last-read-id}`; what produced the invalid ones is not established (inferred: the old writer, or hand edits).

**Root cause:** doctor is a flat list with a binary exit code and no notion of "known and accepted". `--brief` reduces it to a count, so stale corruption swamps new corruption.

**Fix shape**
- Root: give each check a stable id (they already have `id`), and support a `doctor --since`/baseline file or `--only <id-prefix>` and `--ignore <id>`. The exit code then reflects only what the caller selected.
- Band-aid: stay with severity demotion for legacy files that have a successor (done).

---

## Cross-cutting observations

1. The cuts mostly came from one root: the tool had no concept of "who am I" other than place. Clusters 1, 5 (the `room` in the targeted check), and 6 (`--own`) are all that one design flaw showing up at different commands. Participants closed it. New cuts should be judged against the participant build, and the nine cuts here rarely apply to it as written.
2. Most cuts are "resolved" for real: about 30 of the 43 have a fix commit dated within days of the cut, often the same day. What remains is not the reported symptom. It is the class: unbounded flocks (4), a guard that recommends its own bypass (5), doctor's flat exit code (9), a debug-only placeholder assertion (5), and the shared-workspace reply address (1).
3. The pattern "tests verify text or one path, not the live behavior" recurs: the systemd unit text vs a running tick (7), `exact_fix` checked per test rather than at the funnel (5), and the wall-clock handshake (8). Each fix moved the check to the funnel or the live tick. That is the right shape to keep.
4. Errors that steer are a design lever: many fixes were a better `suggested_fix` or `exact_fix` (2, 5, 1). The reverse is also true: the crossed_send refusal's exact fix hands the agent the bypass. Treat every error's fix text as product surface.
5. The agent-guidance gaps I found are small and cheap: exit code is the verdict, and `--json` on the same call (3); do not pipe a Monitor watch (6); `live_watch` is not liveness (6). They belong in `skills/post/SKILL.md`, not in code.
6. Open beads relevant to this lane: `post-b6o` (workspace-collision warning), `post-4th` (gate timeout), `post-pe2` (idle monitor is workspace-aggregate), `post-y2q` (settle ordering, left open deliberately), `post-b18` (`watch --snapshot` with an unknown explicit `POST_PARTICIPANT` exits 0 with nothing, the same silent-failure class as cluster 6).
7. Not checked: I did not run write commands or a live doorbell tick, and I did not reproduce dd75/bb29. I did not look at how the supervisor's per-participant scan handles `post-b18` in practice.
