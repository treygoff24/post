# Lane 5 - papercut chunk 5 (43 cuts, 2026-09-18..2026-09-27)

Method: read all 43 cuts; checked current source (main at f9239eb, 2026-09-27), CHANGELOG, CONTRACT, skill docs, tests, `git log -S`, and open/closed beads. Ids below drop the `pc2_` prefix and use the first 6 hex chars. "Verified" = I read the code or commit; "inferred" = hypothesis from reading, not reproduced. Dates: cut timestamps are UTC, commit times are as `git log` prints them (mixed CDT/EDT), so I compared them explicitly.

Not about post (skip): `595568` (delegate's Devin lane never exits; delegate-agent tool). `9983998` and `562ba0`/`240401` are delegate/estate-sync/provisioning-side but touch post's interface or install, so they are kept below.

Summary table

| # | Cluster | Cuts | Status |
|---|---|---|---|
| A | Watch CPU spin / contention | dc0e81 1a1a75 b8bead | spin FIXED-VERIFIED; concurrency cost LIVE |
| B | Watch replays history as "N new" (cursor degrade + re-arm) | c10d1d ce8b4f a2535b b03c13 3d8903 ddf24f fed20f | mostly FIXED-VERIFIED; re-arm replay is by design; one residual race inferred |
| C | `post who` hang / scale / scope | f63a6c 32d2fe 59b438 4c7491 | FIXED in source (not yet installed); no bound on hangs |
| D | Profile / byline / sigil identity | cabd01 a9d7c8 4aead8 | FIXED-VERIFIED (sigil handoff remains a design limit) |
| E1 | Idle-wake / doorbell adapters | 709bd0 aaf4e9 321bbc 0decc6 6646d9 2e0af7 | FIXED-VERIFIED for supervisor; legacy python + installer probe LIVE |
| E2 | Watch delivery gaps (late inbox, room scan, no channel filter) | ded4e2 70bd95 fc1f49 | LIVE / UNCLEAR |
| F | Cross-repo contract drift | d00062 998399 8ecd1f 149ba6 | mixed: porch FIXED-VERIFIED; others LIVE |
| G | `--json` plus stderr banner (double-send) | e60725 59f29c | LIVE |
| H | Inconsistent failure posture per command | c81aa0 70bc26 4df31a 43a983 | LIVE (all four) |
| I | Hook conflict line injected every request | 8c6f5c | LIVE |
| J | Channel lifecycle and cross-host discoverability | df09d8 312305 | FIXED-VERIFIED (df09d8); 312305 likely fixed by default-on bridging, deployment unverified |
| K | Build / install / gate tooling | 562ba0 240401 7f1f84 | LIVE |

---

## A. Watch CPU spin and contention (dc0e81, 1a1a75, b8bead)

Symptom: `post watch --digest --interval-ms 5000` used 50-67% of a core on the devbox instead of sleeping; 8 concurrent `watch --snapshot` cost ~5x CPU each.

Status: spin FIXED-VERIFIED; concurrency LIVE (bead post-hg5, P3 open).

Root cause (verified for the spin): a Linux-only self-wake loop. A watch for a participant with no inbox dir yet anchors on `participants/<id>/`, where it writes `watch.heartbeat`; the heartbeat woke the watch, the scan read `cursors.json`, inotify reported the reads, the backend filtered them and returned the wake early, and the tick wrote another heartbeat. Fixed by fd16003 and 81c0f40 (2026-09-23; `NotifyWake::wait` keeps waiting to its deadline and ignores presence writes, `is_presence_write` in `src/commands/watch.rs`). The commit body reports a live devbox probe, red-proofed tests, and installation on both hosts 09-23 09:04Z (bead post-aqw.3 close reason). The cut's own correlates ("oldest watcher", "no --text") were red herrings, and its "never-consumed cursor rescanning backlog" guess was wrong; the loop was independent of backlog size. macOS never looped because FSEvents does not report reads.

Amplifier (verified in code, inferred as contributing): every scan of one participant calls `visible_addresses` -> `routing::received_addresses` -> `ReceivedIndex::read(context)` (`src/commands/inbox.rs:250`, `src/routing.rs:270`), which reads every store on the host, and every participant is itself a store. With 4-5k participants a single snapshot is O(host participants), so the 3.7 scans/sec spin cost 42% of a core in children, and N concurrent snapshots contend on the same file walks (hg5, and the older bead post-86k which named this scan in 2026-09-16). I did not measure hg5.

Fix shape: root-cause: stop scanning every store to answer "which addresses did this participant ever receive". Keep a per-participant routed-address index written under the participants lock at routing time (the fix already named in post-86k), so a snapshot reads O(own mail). Band-aid (already in place): supervisor staggers scans. Independent cheap guard: a per-poll scan-cost line (POST_WATCH_PROFILE exists, but is opt-in and stderr-only) or an automatic "watch loop exceeded N wakes/sec" self-diagnosis in `post doctor`/`who`.

## B. Watch reports old history as "N new" (c10d1d, ce8b4f, a2535b, b03c13, 3d8903, ddf24f, fed20f)

Symptom: `post watch --digest` emitted "#night-porch: 339 new" spanning hours of already-read messages, after a warning that `cursors.json` was invalid, or after re-arming the watch.

Status: mostly FIXED-VERIFIED; one residual race inferred; two behaviors are by design.

Root causes, separated:
1. Unusable cursor state fails open (all unread) and used to be silent. FIXED-VERIFIED: 94579be (2026-09-22 17:02 CDT) added `cursor_unusable` to every watch event and digest (`src/commands/watch.rs:1449,1603`; text says "re-reported ... cursor unusable", `watch.rs:301`), the warning now names participant, path and reason (`src/cursor_state.rs:58`), doctor reports `participant.<id>.cursors_unusable` (`src/commands/doctor.rs:482`), and contract samples pin it. Doctor deliberately does not auto-repair (comment at `doctor.rs:470-483`: discarding the file destroys the read record), which answers c10d1d's "offer to rebuild".
2. The intermittent "invalid" while the file is valid (a2535b, b03c13, fed20f). The cuts guessed non-atomic writes; that is wrong (verified): writers already use temp+rename (`src/mailbox.rs:836 atomic_replace`). The reader was the problem: it inspected the path and then read it (two path operations). A6 (ff20812, 2026-09-22 22:41 EDT) reads through one held descriptor and retries once on transient errors (`src/cursor_state.rs:236-318`); installed on both hosts 09-23 09:04Z, after the last such cut (fed20f, 04:33Z). Residual (inferred, not reproduced): `read_participant_cursor_once` treats `nlink != 1` as a deterministic invalid (`cursor_state.rs:298`). If a rename lands between `open` and `fstat`, the held inode has `nlink == 0`, which takes that same non-retried branch, so the exact window A6 closed for path races reappears at descriptor level. A tiny window, but every consuming read renames this file. Simple fix: treat `nlink == 0` as transient.
3. Replay on re-arm (ce8b4f, ddf24f, parts of c10d1d): by design. A watch never writes cursors; its "already told you" memory is process-local (`skills/post/references/watch.md`, end of Events), and the Monitor recipe tells agents to re-arm with the same command every 10-30 min ("starts by ringing everything still unread"). So any unread-but-rung message re-rings on every re-arm, and the digest phrase "N new" describes unread, not news. `--from now` exists (watch.rs:643) but is not in the Monitor recipe and also skips messages that arrive during the gap. ce8b4f is archived; its premise ("chat --limit 0 marked everything read") I could not verify. ddf24f ("workspace-level cursor") I could not reproduce from code (the participant cursor is what `unread_channel_skipping_consumed` uses); it predates join-from-now (0ku, 09-23), which removed the other big source of "flood" (a fresh join used to mark whole history unread), so UNCLEAR whether anything remains beyond causes 1-3.

Fix shape: root-cause for 3: make "seen by a doorbell" durable and cheap: have the doorbell (supervisor already does this for the pane) record the highest id it rang per participant, and have `watch` accept `--since-rung`/read it, so a re-arm rings only what the previous watch did not. Band-aid: put `--from now` in the Monitor recipe with an explicit "run `post inbox`/`chat --peek` after re-arm to catch the gap" step (hooks already report unread on the next turn), and call the digest word "unread" instead of "new". For 2: one-line nlink==0 retry.

## C. `post who` hangs and scope (f63a6c, 32d2fe, 59b438, 4c7491)

Symptom: `post who --json` took 47 s, 76.7 s, and 1m55s (77 s sys time) on the devbox, stalling Loom launches; `who --room atlas` printed ~1500 participants from every workspace.

Status: FIXED in source, not yet installed on devbox (STATE.md says both hosts run 20fc657; the fix is 857ab38 merged 2026-09-27 20:10 CDT). Bead post-gxz is still open; its "profile it and add a timing test" acceptance is done in the commit. The newest cuts (2026-09-26 22:20 CDT) predate the fix.

Root cause (verified by commit ec7f3e0 message and code): `who` recomputed `received_addresses` per participant and that scan opens every store on the host, including one per participant, so cost was quadratic in participants (102.6 s at 4,139 participants on a copy of the live store; now 0.12 s; tests/scaling.rs holds a 2,000-participant deadline test red-proofed against the old code). The participant count itself was inflated by test phantoms: bead post-276 records 2,907 of 4,820 as Loom vitest `bind --new` participants. Post never retires participants, so any large-N test harness poisons the host's every command (see A). `who --room` scoping was fixed by 94579be (participants filtered by workspace; verified `src/commands/who.rs:40-71,101-110`), after the 09-22 cut.

No-timeout/no-progress complaint (f63a6c): not addressed. A read-only listing that can take >60 s gives no signal that distinguishes hang from slow. Low priority once who is 0.12 s.

Fix shape: root-cause is already applied for who/doctor/channels; the remaining root cause is unbounded participant growth: implement post-276 (GC of never-used, lease-expired participants, reversible, by `doctor --fix` or `post participant gc`) and the per-participant routed-address index (see A) so single-participant commands are also not O(host). Also: install the 857ab38 build and re-measure on the devbox before closing gxz. Band-aid: callers like Loom keep their own 2 s bound (already done) and stop `bind --new` per test session.

## D. Profile, byline and sigil identity (cabd01, a9d7c8, 4aead8)

Symptom: three seats in one room all rendered as "Cairn"; a second participant in the same directory inherited another's display name; a continued lineage could not reuse its previous emoji.

Status: FIXED-VERIFIED for byline and inheritance; sigil handoff UNCHANGED BY DESIGN.

Root causes: (1) byline rendered the room profile instead of `from_lineage`; fixed 7c47a68 (2026-09-19 16:18 CDT; cut was 20:50Z = 15:50 CDT). (2) profiles were keyed by workspace, so everyone bound to a workspace shared one entry; fixed b58932a (2026-09-21 21:22 CDT) by keying `participant:<id>`; CHANGELOG "Profiles belong to one participant". Cut a9d7c8 is 09-22 02:05Z, so the fix was in the tree but the reporter's build likely was not (unverified). Remaining bead post-b6o (warn when a cwd-inferred bind shares a workspace across lineages) is open P3. (3) Sigil (4aead8): `validate_pfp` refuses a sigil held by any participant whose lease is active (`src/profile.rs:152-215`), and a continued lineage is a new participant, so it cannot take back its own sigil until the predecessor's lease lapses or the predecessor ends. The refusal message now names the holder and what frees it (`pfp_taken`), and `skills/post/references/identity.md:105` documents it; this is a documented limit, not a bug.

Fix shape: for sigil, either (root) let `participant continue` transfer the lineage's sigil (the presentation belongs to the lineage, and continuation is an explicit act), or (band-aid, already done) explain. Skip unless continuation is frequent.

## E1. Idle-wake and doorbell adapters (709bd0, aaf4e9, 321bbc, 0decc6, 6646d9, 2e0af7)

Symptom: Codex seat had only activity-gated hooks so channel mail never woke it; timer doorbells skipped the focused pane; the installed continuous doorbell rejected participant-addressed mail (`event_metadata requires room`); legacy python doorbell raises AttributeError on a non-object listing; installer snapshot probe only checks "each line is a JSON object".

Status: the design problems are FIXED-VERIFIED; two leftover defects LIVE.

Root cause: five separate wake paths (hooks, per-agent timers, codex-notify-monitor, legacy python doorbell, native watch notice) each with its own hand-copied parser of watch events, written before typed participant addresses existed. Fixed by consolidation: 93cba87 (2026-09-22 22:42 EDT, after the 17:53Z cuts) accepts participant/lineage addresses, and the per-host supervisor (`skills/post/hooks/doorbell-supervisor.mjs`, CHANGELOG "One doorbell supervisor per host", default-on per Trey's 2026-09-23 ruling) replaces the timers. Focused pane: `enable --focused` exists as an opt-in (`doorbell-supervisor.mjs:1022`) and SKILL.md documents it. Still live (verified in code): `doorbell/post-doorbell` `registered_rooms` does `listing.get(...)` on whatever JSON `post rooms` prints (`doorbell/post-doorbell:197-209`), and the codex/systemd installers still only assert that each snapshot line is a JSON object (`install-codex-doorbell.mjs:337-345`, `install-systemd-doorbell.mjs:387`). Bead post-pe2 (idle bell is workspace-aggregate, not participant-scoped) is open, and post-y2q (delete legacy poller and shim) is in progress since 09-06.

Fix shape: root-cause: delete the legacy python doorbell and the per-agent timer installers now that the supervisor is default-on (this removes 6646d9 and 2e0af7 outright), and have the one remaining parser consume `post contract samples` as its test fixture (Lane D already added consumer tests, 2c8b65c). No band-aid warranted for the python bug.

## E2. Watch delivery gaps: late inbox, same-room siblings, no channel filter (ded4e2, 70bd95, fc1f49)

Symptoms: `watch --channel` is rejected; a Monitor `post watch --from now` in a coordinator session never printed a message from a hosted agent that shares its room name while the hook did report it; in one of six runs the older of two watches on the same participant missed a direct message within 3 s.

Status: ded4e2 LIVE (doc gap); fc1f49 likely explained by an open bug (post-3uq, inferred); 70bd95 UNCLEAR.

- fc1f49 (inferred, strong fit): bead post-3uq. A participant with no `inbox/` yet is watched on its anchor dir non-recursively; when the first mail creates `inbox/`, no event reaches the anchor and dirs are only re-derived on the slow pass, up to `max(10 x interval, 30 s)` (`src/commands/watch.rs:729`, bead text). The probe used throwaway participants, so each first message hits exactly this window; "one run in six" fits variation in whether an earlier scan had already created the dir. Nothing is lost, only late. Not proven because the reporter has no logs for the miss.
- 70bd95: I found no same-room filtering in the participant scan: own-suppression is by participant id (`src/eligibility.rs:520 message_is_own`, `src/output.rs:317`). But the legacy room scan (used for unbound `--snapshot --room`, i.e. what the supervisor's headless-resident mode runs) drops any message whose `from` equals the watched room (`watch.rs:1943`), so a coordinator and hosted agent that share one room name would silently lose each other's channel posts there. That is the design inviting the mistake: room name is used as identity. Whether this cut hit that path is not established.
- ded4e2: `WatchArgs` has `--room`, `--own`, `--reason`, no channel selector (`src/cli.rs:953-1000`); channel choice lives in the doorbell adapter's `subscribe`. Nothing in `watch --help` says so.

Fix shape: 3uq: when a wake scan finds a target whose inbox did not exist at registration, re-derive dirs immediately (as the bead says); or simply always register the participant dir recursively. 70bd95: retire the room-name self-suppression in the room scan in favor of participant-id suppression, or require `--own` explicitly there (`--own` already exists for this). ded4e2: add a one-line `watch --help` note, or (small feature) a `--channel <name>` filter so a Monitor can ring on one channel without the supervisor.

## F. Cross-repo contract drift (d00062, 998399, 8ecd1f, 149ba6)

Symptoms: a post JSON addition (`key`/`legacy` on `profile show --json`) crashed Porch's exact-schema startup check on both hosts; delegate's completion ping passes `--allow-self`, which post no longer accepts; `post schema`'s watch shape omits fields watch emits; Porch's default owner marker (fox) differs from post's (bearded face 🧔, `src/mailbox.rs:1197`), so Porch's suggested `post owner init` (no `--marker`) leaves the store signing-disabled.

Status: d00062 FIXED-VERIFIED (Porch 39ca6df, plus post `scripts/install-smoke.sh` now launches Porch's startup sequence and requires all six checks, dbf1393 09-22 23:44 EDT, after the cut). 998399 LIVE (bead post-cq8 open; verified post removed `--allow-self` with the participant model, and CHANGELOG "the 0.5.0 --allow-self opt-in is retired"). 8ecd1f LIVE (verified: `src/commands/schema.rs:387-392` watch shape lists no `cursor_unusable`, `display_name`, `pfp`, `sender_address`, `sender_provenance`, though `WatchEvent` in `src/output.rs` carries them; schema is a hand-written string list, see `tests/schema_surface.rs` which spot-checks a few fields). 149ba6 LIVE, unverified on the Porch side (I only confirmed post's default).

Root cause: design invites it. Consumers (Porch, delegate, doorbell, Loom) each hard-code post's CLI/JSON, and post's own descriptions of that surface (`schema`, help text) are maintained by hand separately from the types that produce the output. Post removed a flag without an inventory of callers.

Fix shape: root-cause: generate the `post schema` shapes from the serde types (or test that every serialized field appears in the schema), and make `post contract samples` the contract Porch/delegate/doorbell test against in their own CI (the Lane D machinery exists; d00062 shows it works when run at install time). Process rule: a removed flag or new field needs a `docs/CONTRACT` note plus a run of install-smoke. Band-aid for cq8: pick option (a) in the bead (delegate sends to `participant:<caller>` when the room is the caller's own). For 149ba6: Porch passes `--marker` (or drops its own default).

## G. `--json` output is not safe to pipe (e60725, 59f29c)

Symptom: `post send --json 2>&1 | jq` and `post chat <ch> --send --json 2>&1 | jq` fail to parse because of a `post: sending as ...` stderr line; the parse error read like a failed send and a retry double-sent a note (cos-devbox 20260923-211941-dc6e90 and 212005-e7e22d).

Status: LIVE (verified: unconditional `eprintln!` at `src/commands/send.rs:168-176` and `src/commands/chat.rs:2352`).

Root cause: design plus agent habit. The contract keeps stdout for results and stderr for diagnostics, and errors under `--json` are JSON envelopes on stderr, so `2>&1` is a natural thing for agents to write. The banner is meant to name the acting identity "before the append-only write" (comment at `chat.rs:2348`), but nothing can act on it before the write: it is printed as a line and the command continues, so it prevents nothing, and the same facts are in the receipt (`envelope.from`, `sender_provenance`; chat `message.from`). Secondary root: no idempotency, so an ambiguous failure is answered by retry.

Fix shape: root-cause: under `--json`, print no banner (text mode keeps it), and keep the identity in the receipt. Optional: a `--dedupe-key`/client id on send so a retry of the same intent returns the original receipt. Band-aid: skill line "never `2>&1` with `--json`; on a parse failure, run `post inbox`/`chat --history` before retrying a send".

## H. Inconsistent failure posture per command (c81aa0, 70bc26, 4df31a, 43a983)

Symptoms and status (each LIVE, verified in source or bead):
- c81aa0: writing the activation notice to a closed stderr returns an I/O error, so `participant bind` exits nonzero after doing its work (`src/participant.rs:73-87`). The CHANGELOG says a closed stderr no longer panics, but this path deliberately propagates.
- 70bc26: `watch --snapshot` with an explicit `POST_PARTICIPANT` that has no record exits 0, empty stdout, only "participant: unbound" on stderr. Verified cause: `resolve()` returns `Resolved::Unbound` both for "no binding" and for "explicit id names no record" (`src/participant.rs:375-384`). Bead post-b18.
- 4df31a: `post read <id>` consumes mail and silently drops piped stdin; the A2 stdin guard exists only in chat (`src/stdin_guard.rs`, used from `chat.rs:159` only). Not re-verified by the reporter; I verified there is no other user of the guard.
- 43a983: `post channels` (a listing) calls `eligibility::unread_channel(...)?` per channel, so one corrupt message file anywhere in a joined channel fails the whole listing (`src/commands/channels.rs:36`; beads post-8pn describes the same fail-closed path for plain reads). Contract says consuming reads fail closed but non-consuming reads warn and skip.

Root cause: each command chose its own posture (fail closed, fail open, exit code, warning) at the call site; there is no shared rule such as "a read-only listing never fails on one bad item; a fail-open scan must say so in a machine-visible way; an explicit identity that does not exist is an error". Contract text states it per command.

Fix shape: root-cause: make the explicit-but-unknown participant a typed error in `resolve()` (keep ambient unbound as a value), pass a `skipped_unreadable` count through `channels` the way `inbox` already does (`src/commands/inbox.rs:66`), and generalize the stdin guard to every non-send command via one entry hook. Bands-aids: activation notice: warn-and-continue (the "keep the notice pending" behavior can stay, exit 0). Add a contract line for each posture so the four cases are testable.

## I. Hook conflict line injected on every request (8c6f5c)

Symptom: `[post] POST_PARTICIPANT conflicts with this hook session key; ...` was injected as model context 5,284 times across 51 Codex sessions.

Status: LIVE (verified in `skills/post/hooks/codex-mail.mjs:570-573`, and the same string in the claude, cursor and grok hooks). Each hook event that sees the conflict emits it and returns; the per-session state file has a `lifecycleWarned` flag for another warning but none for this one.

Root cause: the hook has no once-per-session memory for this condition; the environment cause (a POST_PARTICIPANT exported into a session that is bound to a different key, probably inherited from a parent shell or delegate lane) persists for the whole session, so every prompt and tool call repeats it. Also a guidance gap: the message tells the agent to unset the variable, but a Codex agent cannot change its parent's environment.

Fix shape: root-cause: warn once per session (state flag, like `lifecycleWarned`) and then stay silent, and record the conflict in `post doctor`/`who` so it is discoverable. Consider not exporting POST_PARTICIPANT to child seats at launch (the launcher/delegate side). Apply the same change to all four hooks (shared helper).

## J. Channel lifecycle and cross-host discoverability (df09d8, 312305)

- df09d8 (no delete verb; `chat '#name' --join` created a channel literally named "#name"): FIXED-VERIFIED. `post chat --archive/--unarchive` shipped 09-22 21:07 CDT (580ab81/c808283, after the 01:42Z cut; CHANGELOG, SKILL.md). The `#` case now fails closed unless a literal `#name` channel exists (`src/commands/chat.rs:17-53`, 94579be, 09-22 17:02 CDT). Deliberately not normalized because existing stores hold literal `#name` channels.
- 312305 (`post chat night-porch --join` on the Mac silently created a host-local channel while the real one lived on the devbox): status UNCLEAR. Root cause was a bridge default: channels only synced via an allowlist that contained only `loom-build`. Trey ruled 2026-09-24 that bridge v2 channel sync is on by default (post-xiy; docs a126fff 09-24 19:50 CDT, skill 15516fb 09-24 23:33 CDT), after this cut (09-25 00:22Z = 09-24 19:22 CDT). The code lives in claude-space's post-bridge, not this repo, so I could not verify it is deployed on both hosts. Even with sync on, a `--join` on a host that has not yet imported the remote channel creates a second channel with the same name, and merge/collision behavior is the bridge's, not post's (unverified).

Fix shape: for 312305 the root fix is applied (bridge default). Remaining small guard: `--join` of a name that is not local should say "created new local channel; no remote host advertises it / a remote host advertises it" when the bridge topology is readable (`bridge_topology.rs` already reads remote hosts). Verify the default is live on both bridges before closing.

## K. Build, install and gate tooling (562ba0, 240401, 7f1f84)

- 562ba0 (estate-sync install row hardcodes `target/release/post`; managed Cargo writes elsewhere) and 240401 (`scripts/install-post.sh` misses the shared cache without `CARGO_TARGET_DIR="$(estate-build-cache path)"`): LIVE. Verified `scripts/install-post.sh` honors `CARGO_TARGET_DIR` (line 127) but never consults `estate-build-cache`; the estate-sync row is outside this repo. Root cause: two installers each guess the artifact path; environment/tooling outside post. Fix: install through `cargo install --path --locked` (cargo owns artifact resolution, as 562ba0 suggests) or have install-post.sh call `estate-build-cache path` when present; the estate-sync row should call `install-post.sh` rather than copy a hardcoded path.
- 7f1f84 (gate.sh has no timeout around `cargo test`, a hanging test stalls the gate silently): LIVE, bead post-4th. Verified `scripts/gate.sh:22`. Fix: `timeout`/watchdog per step that names the stuck test binary (cargo `-- --nocapture` plus a `--test-threads` timeout). Band-aid is the whole fix here.
- Related but not in this chunk: the merge note for 857ab38 mentions a stale build-script manifest requiring `touch build.rs` ("filed separately").

---

## Cross-cutting observations

1. Participant bloat multiplies everything. Post stores every participant as a directory that other commands walk; there is no retirement (post-276 open). 4-5k participants (2.9k of them Loom test phantoms) turned `who` quadratic (C), made each watch scan O(host) through `ReceivedIndex::read` (A), and made the self-wake loop cost 42% of a core. Fixing one command's complexity does not fix the class; a per-participant routed-address index plus GC would.
2. Ledger status is not code status. The chunk has 25 open, 13 resolved, 5 archived. Five of the 25 open cuts are already fixed in source (1a1a75, fed20f, and the three `who` cuts f63a6c, 32d2fe, 59b438; 312305 is probably fixed by the default-on bridge), while several `resolved` doorbell cuts (for example 709bd0) were resolved by installing something or catching up by hand rather than by a post change, and 6646d9/2e0af7 remain live code. Deployment lags source: the who fix (857ab38) and the join-from-now/who work are on main but STATE.md shows both hosts still on 20fc657.
3. Silent degrade is the recurring failure shape. Cursor-unusable, unknown explicit participant, closed stderr, hook conflict, corrupt channel file, and `--json` plus banner are all cases where post picks a posture nobody chose centrally (H). The good fix so far, marking degraded output in a machine-visible field (`cursor_unusable`), should be the template.
4. Post's descriptions of itself are hand-maintained (`schema` strings, help text, contract, per-adapter parsers), and consumers hard-code shapes. Lane D's samples/consumer tests/install-smoke are the right direction; extend them to `schema` generation and to delegate/Loom.
5. Agent-guidance gaps that need no code: `2>&1` with `--json`; re-arm replay semantics and `--from now` in the Monitor recipe; never treat a manual catch-up as proof idle wake works (use `post-doorbell status` and a live self-probe); do not `bind --new` per test session.
6. Wrong hypotheses in cuts are common (write-in-place cursors, workspace-level cursor, same-room filtering). The papercut text records symptom well and cause poorly; the fixes that stuck came from measurement (POST_WATCH_PROFILE, a live probe) rather than from the cut's suggested prevention.
