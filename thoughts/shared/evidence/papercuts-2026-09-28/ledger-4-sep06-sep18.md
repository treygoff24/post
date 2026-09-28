# Lane 4: papercut chunk4 (43 cuts, 2026-09-06 .. 2026-09-18)

Reviewed against /Users/treygoff/Code/post at HEAD f9239eb (2026-09-27). Cargo version is still 0.9.0 with a large Unreleased section. Read-only pass. "Verified" means I read the cited code or commit. "Inferred" means it is a hypothesis.

Key date: the participant model landed 2026-09-16 (3d4875f, c1a456a). Profiles were re-keyed by participant 2026-09-21 (b58932a). Join-from-now and the doorbell supervisor landed 2026-09-22/23. About half of chunk4 was logged against pre-participant binaries (0.8/0.9.0), so "resolved" is usually true, but only because the architecture changed.

Not about post: none strictly. Three cuts are about post's neighbours, so the fix is not in post's Rust:
- pc2_d8111ef841a09e16: the `ccw` launcher and realm hook env.
- pc2_d698f58cf310f028: Porch's own tests.
- pc2_0a98f361fa34caa4: estate cargo cache wrapper, tooling.
I still cover them below where post's contract is the lever.

---

## Cluster 1: Sender/actor identity inferred from cwd (posted as the wrong agent)

Cuts: aa84740a, 5fdf76e1, e806839, 5321d204, 69dc2190 (five reports of the same thing; `pc2_` prefix omitted), 985b7af5, 282fbba3, 5daeb647, f1863156, ba5045a4 (related).

Symptom: `post chat --send` run from another agent's checkout posted as `chuck-lead` or `deslop-tooling`. The message landed in the partner's unread stream and could not be retracted. Later cuts (9/16) show the mirror image: a Tower cwd resolved the generic `workspace` room and failed `not_a_member`. Separately, `--from codex-effort` was accepted, but the reply address named a nonexistent room.

Status: FIXED-VERIFIED for the cwd-impersonation half. The live remainder is listed below.
- Verified: `docs/PARTICIPANTS.md` section 1 states the cause: one room/workspace concept was the sender, the filesystem key and the cursor key. `src/channel.rs:152-190` (`acting_room`) and `src/participant.rs:375-420` (`resolve`) now take the actor from `POST_PARTICIPANT` or the harness conversation-key binding, never from cwd. `src/commands/send.rs:111-175` refuses `--from` or a `POST_FROM` pin that disagrees with the bound participant. Every send prints `post: sending as '<room>' (bound participant <id>)` to stderr.
- Verified remainder, part 1 (LIVE): the workspace is captured from cwd only at first bind (`workspace_context`, `src/participant.rs:994-1036`) and then sticks. A plain rebind deliberately preserves it (`tests/participants.rs:1798`). A session first bound from a generic cwd keeps the wrong workspace, which is what happened in 282fbba3 and 5daeb647 (the 5daeb647 fix was `participant bind --workspace X` then `join`).
- Verified remainder, part 2 (LIVE): `not_a_member` (`src/commands/chat.rs:1664, 1754, 1827`; `catchup.rs:901`) only says "Join first". It never says "you are bound to workspace W; this channel's membership is under a different actor". Both misrouted cuts asked for exactly that hint.
- Verified remainder, part 3 (LIVE): there is no retract/unsend verb (rg for retract/unsend/delete message across cli/CONTRACT/skill finds nothing). A misattributed post is permanent.
- Verified remainder, part 4 (LIVE): open bead post-b6o (P3) covers warning when a cwd-inferred bind shares a workspace across lineages.
- ba5045a4 (unregistered `--from` sender): `--from` is now only an assertion, checked against the bound reply address (`send.rs:118-135`), so the "unregistered sender with dead reply room" path is gone. Inferred for the pre-participant binary, since I did not reproduce it.
- f1863156 (task closeout `unknown_room` in an unregistered repo): the participant model allows roomless participants (`participant.id` is the reply address, `channel.rs:172`). The 2026-09-25 roomless-channel work (8f8f8e3, post-ojh) extended this. So the old failure is probably gone. "Task launch should provision a project room" is a launcher (outside post) choice. UNCLEAR whether the launcher does it.
- 985b7af5 (git-push-then-post ran in the wrong worktree): that was user-side chaining. With participant binding it stops mattering unless the cwd matters for bind. This one is FIXED as a consequence.

Root cause: DESIGN. Identity was overloaded onto cwd/room (it was one concept doing three jobs, per PARTICIPANTS.md). It is fixed by the participant refactor. What survives is the design hazard that workspace is a sticky value derived once from ambient cwd.

Fix shape:
- Root fix (small): make `not_a_member`, `unknown_room` and `no_participant` name the acting participant, its bound workspace, and the exact rebind command (`post participant bind --workspace <room>`). Surface the workspace in the "sending as" stderr line, which is already there.
- Add `post participant bind --workspace` guidance to the `not_a_member` hint when `participant.workspace` differs from the cwd's room.
- Retract: either a narrowly scoped `post chat <ch> --retract <id>` (own messages only, an append-only tombstone; history stays honest), or accept that channels are append-only and drop the request. That is a decision, not a fix.
- Band-aid: implement post-b6o's warning. It is cheap and rare-path only.

---

## Cluster 2: Send-command grammar mistakes (positional prose, missing `--send`, delegate/post mismatch)

Cuts: 86356fc, ee98861 (agent-workflow tag), f612b25, c9af5a5, 8cb0041.

Symptom: `post chat CHANNEL "prose"` treats the string as a body file path; a long paragraph even hit ENAMETOOLONG. Resumed sessions omit `--send`. `delegate mail send` takes positional body while `post send` wants `--to` plus `--body`.

Status: mostly FIXED-VERIFIED on diagnostics. The grammar itself is still LIVE by design.
- Verified: ENAMETOOLONG/InvalidFilename now returns `invalid_argument` with "use `--body '<text>'` or stdin" and does not echo the payload (`src/commands/send.rs:902-916`, commit 94579be 2026-09-22). NotFound builds a runnable `--body` fix (`send.rs:875-895`). `parse_failure_fix` (`src/app.rs`) special-cases `[FILE] cannot be used with --body`. `--body/--body-file` imply `--send` on chat (`src/cli.rs:471`), so the specific 09-17 "requires --send/--body" complaint is narrowed.
- Verified LIVE: the deprecated positional `FILE` spelling still exists on both `send` and `chat` (`src/cli.rs:433`, `552`). It is the reason every one of these mistakes parses at all and then fails one layer down. It also collides with the shape agents guess from `delegate mail send` and `mail`-style CLIs.
- Skill examples were the other half (c9af5a5): SKILL.md was reorganised on 2026-09-22 (92de29a). I did not audit every example for `--body`.

Root cause: DESIGN plus CLI-surface inconsistency across the estate (post vs delegate). The positional slot is a footgun kept for compatibility.

Fix shape:
- Root: delete the positional body-file argument on `send` and `chat`. Let clap reject it with a targeted message ("body is `--body TEXT` or `--body-file PATH`"). It is documented as deprecated, and the stdin guard already exists, so the removal blast radius is small. Check first that no hook or installer still passes a positional path (rg the hooks and launcher).
- Band-aid: what shipped (better diagnostics). Also make the missing-`--send` case an exact_fix the way crossed-send does.
- Cross-CLI (8cb0041): fix in delegate (bead post-cq8 shows the same drift: delegate still passes the removed `--allow-self`). Not a post change.

---

## Cluster 3: Unread counter disagrees with the consuming read (phantom unread, `crossed_send` deadlock)

Cuts: 5ad74557, 21716da3, 3d100a49.

Symptom: `post channels` said `unread=3` (or `unread:1` on #tower-build) while a full consuming read returned nothing, so the CoS doorbell and agents kept re-polling. Separately, `post chat --body-file` was refused with `crossed_send` while 99 unseen messages waited, and one page of reading only cleared 24 of them.

Status: 5ad74557 and 21716da3 FIXED-VERIFIED. 3d100a49 is FIXED plus a docs gap.
- Verified root cause (v0.9.0): `channels` computed `unread = summary.messages.saturating_sub(seen_set_count)` (`git show v0.9.0:src/commands/channels.rs`, lines 30-40). That counts own messages, join events and any file the consuming read filters out. Reads used a different predicate ("id not in seen and from != self").
- Verified fix: `src/commands/channels.rs:38` now calls `eligibility::unread_channel(...)`, the same function the consuming chat read (`chat.rs:1774`), catchup (`catchup.rs:942`) and watch (`watch.rs:1546`) call. The predicate is `!own && !already_read && !history` (`src/eligibility.rs:355-369`), so the counter and the reader cannot drift. The join-from-now change (a3ee781) also stops the join event and pre-join backlog from counting.
- 3d100a49: `crossed_send` now bounces only when an unseen message is addressed to the sender (mention, reply to own message, owner room), and `--anyway` overrides (schema text in `src/commands/schema.rs:69`; `src/channel.rs:610-780`; refusal previews the targeted messages). The reporter asked for exactly `--anyway`-style relief and apparently did not know it existed or was not shown it. Checking whether the `crossed_send` exact_fix includes `--anyway` was not done. Inferred fixed.

Root cause: CODE BUG (two independent implementations of one predicate). Design lesson: any count shown to a user must come from the same function that selects what is read.

Fix shape: done. Optional hardening: a test asserting `channels.unread == messages returned by an immediate consuming read` across own/join/history/mention fixtures. I did not find an existing one. Band-aid: none needed.

---

## Cluster 4: Doorbell/watch replays and silent deaths

Cuts: e8d2bbfe (digest re-emits last read message on each watch start), 2d748264 (open; cos-doorbell replays 10 msgs on gateway restart), 268f7c03 (doorbell woke resident with `no_participant` JSON), d3e5b5ff (watch dies silently after participant rebind), d74a0255 (open; Monitor caps at 30 min).

Symptom: restarting a watch re-rings the still-unread backlog (two restarts = two full wakes for zero new work). A watch that dies leaves the seat believing its doorbell is up. A Monitor-wrapped watch expires and an idle Claude session misses everything until its next turn.

Status: mixed.
- Replay on restart: LIVE by documented design. Verified: `skills/post/references/watch.md` states "A watch's notification memory lives only in its process. After a restart, an id you consumed stays quiet and an unconsumed id may ring again." A long watch "replays everything still unread on start"; `--from now` opts out but also drops anything that arrived while no watch ran. `2d748264` is the CoS gateway (external), not post. The post-side cure is the supervisor's headless `resident add`, which acknowledges by exit code (CHANGELOG Unreleased). From a quick read the supervisor's own ack keys are in-memory (`doorbell/README.md`: "No lifetime event history accumulates in the daemon"). Inferred: a supervisor restart can still re-ring unread mail once. That is acceptable if the mail really is unread, and wrong only when it was read (see e8d2bbfe).
- e8d2bbfe ("already-read message re-emitted"): UNCLEAR. It was logged on the pre-participant Mac binary. Current watch uses participant seen-sets (`watch.rs:1546`), and I found no path that re-emits a consumed id, so I cannot confirm or refute it. Needs a repro on a throwaway `POST_MAIL_ROOT` (I did not, per the read-only constraint).
- Silent watch death (d3e5b5ff): UNCLEAR. Verified: the only fatal exits in the long loop are `watch_admission` errors (a migration-generation mismatch via `POST_ARX_GENERATION`, `src/migration_fence.rs:377-431`) and a failed stdout write. Per-target scan failures warn and continue (`watch.rs:1030+`). Nothing in the loop reacts to a participant workspace rebind: the loop holds a startup snapshot of `target.participant`. I could not derive an exit-1 at about 7 s from a rebind by reading. Hypotheses, all inferred: (a) the reporter's watch shared a mail root with an active migration generation; (b) a harness Monitor tore down the pipe (stdout failure is fatal) and the stderr line was not shown; (c) a rebind changed a file the wake source watches and the backend died (this degrades to polling and does not exit). Related open bug post-b18 (watch --snapshot with unknown explicit `POST_PARTICIPANT` exits 0 with empty stdout) is the same class: a doorbell that looks alive and is not. Best evidence: Kettle's run log (participant omp-5aaee752, night-porch 2026-09-17).
- 268f7c03: the automation shell had no binding. Post's supervisor now runs `post watch --snapshot` as the discovered participant (CHANGELOG, 92f568c "pin the acting participant in systemd doorbells"), and headless residents ring by room. Superseded for the post repo's supervisor. The cos-doorbell itself is external. UNCLEAR.
- Monitor 30 min (d74a0255): LIVE but documented and environmental. Verified: `references/post-mail-doorbell.md` explains the harness cap and says "when the expiry notice arrives, re-arm", and that outside Herdr there is no cap-free wake. Bead post-pe2 (P3, open) tracks that idle doorbells are workspace-aggregate/legacy. The structural gap remains for non-Herdr Claude sessions.

Root cause: mostly DESIGN plus ENVIRONMENT (harness Monitor cap). The doorbell notification state is process-local by design. A shared "notified but not consumed" state does not exist anywhere durable.

Fix shape:
- Root: persist doorbell "last rung" per participant and reason (`doorbell/` state file, the way the install receipt is), and let the supervisor and `watch --once` skip ids already rung for this generation. This would kill the restart-replay class for post-native consumers (CoS should adopt `resident add`).
- Silent deaths: give every fatal watch exit a named stderr line and a distinct exit code (fence, stdout closed, participant changed), and make a watch check its bound participant's workspace each slow pass. Then exit with a named "rebind" message or re-derive its targets. Fix post-b18 in the same change so no doorbell path exits 0 silently.
- Monitor cap: band-aid is the doc that exists. The real answer is the supervisor as the only wake path (extend it beyond Herdr panes to a Claude Code hook-level unread nudge). That belongs to post-pe2, not to a post CLI change.

---

## Cluster 5: Shared display profile stamped onto other participants

Cuts: db3a1de1 (open in ledger), 4d6577a9, ce416161 (sigil collision needed a list).

Symptom: Two participants bound to one workspace shared one display name, so a peer's channel messages rendered under "Fable". Kettle withdrew its name rather than label others' words. Picking a sigil was trial and error.

Status: FIXED-VERIFIED. Both open ledger flags are stale.
- Verified: b58932a (2026-09-21) "profiles: key by participant, never by workspace". `src/profile.rs:29` keys entries `participant:<id>`. `stamp_for` (`profile.rs:250`) reads only the participant's own entry. A bare workspace entry never stamps again. `doctor` reports legacy entries. CHANGELOG "Profiles belong to one participant". Independent black-box probe cited in the commit (13/13). `post profile list` shows holder, workspace, sigil and lease (`src/commands/profile.rs:243-320`), which answers ce416161.
- Remaining residue: pfp uniqueness counts active participants plus registered legacy rooms, so it can still collide and the list is advisory (lease-dependent, noted in the output).

Root cause: DESIGN (per-workspace profile keyed to the shared identity). Fixed at the root by participant-scoped storage. Fix shape: none. Suggest resolving db3a1de1 and 4d6577a9 in the ledger with b58932a.

---

## Cluster 6: Vocabulary invites reading a lease as attention ("active", "read")

Cut: b621671b (major).

Symptom: `post who` says a participant is `active`, meaning only that its lease has not expired. The reporter told a coordinator an auditor was "active, reading the diff" and held a launch for about 30 minutes, when that Codex seat had never received the request.

Status: PARTLY FIXED. The band-aid shipped; the vocabulary rename did not.
- Verified: 8d4d8a4 (A3). `post who --text` prints `lease=active|stale|ended` and a one-line hint: "lease is not attention; for 'did they read it' use `post chat <channel> --seen-by <message-id>`" (`src/commands/who.rs:12-14`). JSON keeps the `state` key with no alias ("strict consumers already broke on additive keys", `tests/cli.rs` A3 comment).
- Verified still LIVE: JSON `state: active` is unchanged, and `who` still prints no per-channel seen state. "unread/read" on messages still names a cursor position.

Root cause: DESIGN/vocabulary. The right word for a lease is "leased". The instrument (`who`) answers a question adjacent to the operator's.

Fix shape: root is rename at the vocabulary layer (JSON and text): `state` values `leased|expired|ended`, and messages `delivered`/`seen` rather than `read`. Do it as an additive JSON key first (`lease_state`) with a deprecation period, if strict consumers really are a concern. Second cheaper step: make `post who` print per-channel seen state for the acting participant's channels, or at least the hint once per JSON call (a `hint` field). The band-aid shipped is appropriate as a floor, and its skill wording is in SKILL.md.

---

## Cluster 7: Skill, docs and installed-runtime version skew

Cuts: 321967e3 (`--message` rejected), 6ba3b35f and bd235b2d (`--max-bytes` on chat/catchup rejected), 79f0dabc (served skill symlinks follow main; installed binary advances separately), 16185624 (dead `SPEC-v2.md` pointer), 48e22fc (hook installers bake a version-pinned node path), 996c4626 (doorbell installer rejects `--help`).

Symptom: An agent follows the skill, and the installed binary rejects the flag. The served skill tracked `~/Code/post` main while the binary lagged. Hooks broke with exit 127 after `brew upgrade node`.

Status: mostly FIXED-VERIFIED. One item is LIVE.
- Skill/binary skew: mitigated, not eliminated. Verified: `post contract skill-manifest [--verify <path>]` (758e389) and `scripts/install-post.sh` (2212ee2, 2026-09-22) build a commit, smoke it, install it, and verify the served skill against the binary manifest (`install-post.sh:186`). Verified: the skill symlinks (`~/.claude/skills/post` -> `~/.agents/skill-library/post`) still resolve to the repo's live tree (the library entry is not a rendered pinned copy; not fully traced), so the structural exposure (skill on main, binary on last install) remains. `--max-bytes` is no longer in SKILL.md on main (moved to `references/commands.md`).
- 48e22fc: FIXED-VERIFIED, c198399 (2026-09-15) and 55b2dda add `skills/post/hooks/stable-node-path.mjs`, used by `install-codex-hooks.mjs:20`.
- 996c4626: FIXED-VERIFIED, 947d3bc (2026-09-22) "installers: make --help a help flag".
- 16185624: LIVE, minor. Verified: `skills/post/references/post-bridge.md:5-8, 24, 34, 168` still points to `SPEC-v2.md` "on the v2 branch". `~/Code/claude-space/post-bridge/` on the Mac main contains only `SPEC.md` (r3.5.1, v1 ship record). The overnight plan listed a "stable SPEC-v2 link" (docs/overnight-2026-09-23.md:63), but the reference was not updated. The SPEC lives on branches (`bridge-v2-ship`, `post-bridge-v2`).

Root cause: DOCS/process. Skill content is served from a moving tree while the binary is version-pinned. Nothing in the agent's session tells it which post version it is running.

Fix shape:
- Root: make the skill self-verifying. Serve a rendered copy per install (or have `install-post.sh` write the manifest hash next to the binary), and add `post version` output of the skill manifest hash so a skill can say "requires post >= X" in front matter. The install gate in place already covers the deploy edge, so this is the last step for the runtime edge. Also add a doc/flag drift test (schema -> SKILL/references grep) in the repo gate so a flag mentioned in docs but not in `post schema` fails.
- Band-aid: for SPEC-v2, replace the path with a durable link or copy the spec into `docs/` and cite that.

---

## Cluster 8: Message-id addressing

Cuts: 17657282 (compact framing truncates ids; `--re` not_found), e69cc6e4 (`post read` cannot read a channel message id), 321967e3 (overlaps cluster 7: `--message`).

Symptom: A compact-framing id `20260918-0524` typed into `--re` returned `not_found`. `post read <channel-id>` suggests `post chat CH --history N` rather than reading it.

Status: 17657282 FIXED-VERIFIED. e69cc6e4 LIVE, minor.
- Verified: 94579be (2026-09-22). Truncated references now render with a U+2026 mark (`src/output.rs:87-110`); `--re`, `--seen-by`, `--message`, `--discard-through` strip it (`src/commands/chat.rs:63-72`), and `--re` resolves a unique prefix (`src/channel.rs:537, 885-923`).
- Verified LIVE: `post read <channel-msg-id>` names the channel but the fix it prints is `post chat <ch> --history <depth>` (`src/commands/read.rs:215, 976`), a position-based render, although `post chat <ch> --message <id>` exists (cursorless) and would render exactly that message. Search returns ids with no one-line way to open one.

Root cause: CODE/UX gap (the fix hint points at the weaker of two tools).

Fix shape: make the hint `post chat <ch> --message <id>` (exact, cursorless), or let `post read <channel-msg-id>` run it. Root: a `post show <id>` that resolves mail or channel ids uniformly (a small feature, reuses `resolve_message_id`).

---

## Cluster 9: Room lifecycle gaps (moving a project between hosts)

Cut: bfbf020 (moving brief-app Mac to devbox: placeholder to local room needed hand-editing rooms.json).

Symptom: `rooms add` reports duplicate-name only. There is no supported way to move a room's ownership between hosts.

Status: PARTLY FIXED, mostly LIVE.
- Verified: `rooms set-path` (path only) and `rooms rename` exist (`src/cli.rs:713-718`). `rooms add` against a remote placeholder now names the owning host and suggests a suffixed name (CHANGELOG, aqw.14). Neither converts a remote placeholder to a local room, nor releases the bridge owner entry. Open beads post-4hy (release old name without hand-editing `owners.json`) and post-6ep (owner release leaves empty mailboxes) describe the same missing verb.

Root cause: MISSING FEATURE. Ownership is a bridge-state concept but post has no verb for it.

Fix shape: one `post rooms transfer`/`release` command, which is the operation that post-4hy already specifies (refuse unless the bridge is stopped; atomic owners.json edit; audit line). Band-aid: the documented manual steps.

---

## Cluster 10: Output-boundary safety (stdout not writable)

Cut: 2929066c (major). `post` consumed unread messages and exited 0 when fd1 was a read-only regular file, because Rust's `StdoutRaw` maps EBADF to success.

Status: FIXED-VERIFIED. `src/app.rs:55-100` (`StrictStdout`, direct `libc::write`), commit 35d8c78. CHANGELOG "Unix result output now writes fd1 through a strict unbuffered syscall seam"; test at `tests/byte_budget.rs:1495` (read-only stdout sentinel).

Root cause: CODE BUG at the emit-then-commit boundary from a std-library edge case. Good fix at the root. Worth noting the neighbouring closed-stderr panic fix (CHANGELOG) is the same class.

---

## Cluster 11: Test and gate reliability (post repo's own tooling)

Cuts: 6dc6f330 (flaky `who_reports_live_watch_without_pids`), c2cfae1b (smoke assertions that pass without the state change), 0a98f361 (cargo cache wrapper breaks gate artifact tests; archived), d698f58 (Porch live tests rely on cwd identity, fail under participant binding).

Status:
- 6dc6f330: FIXED-VERIFIED. `tests/cli.rs:9066-9107` now uses `wait_for_live_watch` and a 40 x 50 ms poll instead of a fixed sleep.
- c2cfae1b: FIXED (participant smoke hardened, commits 886fa6a, ecd238b, 251e1c9; the lesson is process, not code).
- 0a98f361: environment/tooling outside post (estate cargo cache wrapper). The related open bead post-4th ("gate.sh has no timeout around cargo test") is the same family. UNCLEAR whether a native-cargo lane exists.
- d698f58: Porch is outside this repo. What post owns: `post contract samples` and the consumer tests via `POST_BIN` (CHANGELOG Added) are the fix shape. Inferred sufficient if Porch's tests read the samples.

Root cause: TEST DESIGN (timing sleeps; assertions weaker than the claimed state change). A recurring shape, and the shell-footguns rules already name it ("a new assertion proves nothing until you have watched it fail").

Fix shape: root is a gate rule. Any test that spawns a long-running child polls for its condition with a deadline, and any state-change assertion must seed distinct before/after states. Band-aid: fix each flaky test when it flakes, which is what happened.

---

## Cross-cutting observations

1. Timing artifact. Most of the loud cuts were logged in the ten days before the participant refactor and the profile re-keying. The 2026-09-16..23 architectural changes retired clusters 1, 3 and 5 wholesale. "Resolved" is mostly true, but the ledger flag was set by whoever noticed, not by proof. `db3a1de1` and `4d6577a9` are marked open/resolved though the fix (b58932a) landed three days later.
2. Agents keep hitting "the natural first instrument answers an adjacent question" (`who` vs `--seen-by`; `channels` unread vs reads; `post read` hint vs `--message`). The recurring fix shape is one predicate/one source of truth, and error hints that name the exact runnable command (`exact_fix`). Post already does the latter well (send body errors, ambiguous id).
3. Post's own docs/skill are a moving tree relative to the installed binary (cluster 7). The install gate exists now. A flag-drift test between `post schema` and skill/reference text would remove the remaining edge.
4. Several cuts are not fixable in the post binary: the Monitor cap (harness), `brew` node path (fixed), Porch/delegate/ccw/CoS integrations. Their common lever is a stable, sampled contract (`post contract samples`) plus consumer tests. That exists; adoption in the other repos is unverified.
5. Open beads that cover live items from this chunk: post-b6o, post-pe2, post-b18, post-4hy, post-6ep, post-cq8, post-4th. Nothing is filed for: retract verb, `not_a_member` bound-workspace hint, the `read` channel-id hint, the deprecated positional body slot, the stale SPEC-v2 pointer, the silent watch death on rebind.
6. Unverified: I did not run any post command or reproduce the watch-death (d3e5b5ff), the digest replay (e8d2bbfe), or the crossed_send exact_fix contents. Anything marked inferred or UNCLEAR above is from reading source and git history only.
