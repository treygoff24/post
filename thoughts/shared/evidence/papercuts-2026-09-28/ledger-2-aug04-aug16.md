# Lane 2: papercut chunk2 (43 cuts, 2026-08-04..2026-08-16)

Method: read all 43 cuts; checked current source at HEAD `f9239eb` (installed binary is `post 0.9.0 build f784a3b`), CHANGELOG, SKILL.md, `docs/triage-2026-09-22/`, the open beads, and ran read-only probes (`post send/chat/watch --help`, `post rooms`, `post contract samples`, `cargo fmt --check`, a throwaway `POST_MAIL_ROOT`). Nothing was written to any real store. All cuts carry ledger status `resolved` except pc2_e1d4e47b (open); that flag proves nothing, so the verdicts below come from code.

Key dating fact: the 2026-09-16 **participants redesign** (docs/PARTICIPANTS.md) moved identity from cwd to a bound harness conversation. That single change retires most of the "wrong room from wrong directory" family. Commits `ca430aa` (2026-08-25, crossed_send) and `238c1c4` (2026-08-25, errors) retire others.

## Cluster summary

| # | Cluster | Cuts | Status |
|---|---|---|---|
| 1 | Shell interpolation of prose in `--body` | 6 (+1 tooling) | LIVE (structural, cannot be fixed inside post alone) |
| 2 | Body-source intent ambiguity (stdin without `--send`, positional FILE) | 5 | stdin part FIXED-VERIFIED; positional FILE LIVE by design |
| 3 | Room vs channel: two nouns, two verbs, inconsistent flags | 8 | PARTIAL: `send --to <channel>` FIXED; reverse direction and flag model LIVE |
| 4 | Identity guessed from cwd / inherited env | 7 | FIXED-VERIFIED (design change), two residuals |
| 5 | Unbounded output (snapshot, history, junk bodies) | 4 | PARTIAL (opt-in bounds exist, defaults still unbounded) |
| 6 | Read-cursor state vs the crossed_send guard | 2 | MOSTLY FIXED-VERIFIED |
| 7 | Event-schema skew: old readers choke on new event kinds | 2 | LIVE (design) |
| 8 | Agents guess machine-output shape and smoke recipes | 4 | FIXED-VERIFIED (docs/samples), one design smell remains |
| 9 | Send receipt says `archived: true` but gives no readback | 1 | PARTIAL |
| 10 | Post repo build/deploy/release hygiene | 3 | 2 FIXED-VERIFIED, 1 PARTIAL |
| - | Not about post | 3 | skipped |

---

## 1. Shell interpolation of prose in `--body`

- **Cuts:** pc_d8eb7d0218f5, pc_31c9a2cc8c6d, pc_88101f5327c2, pc_d433906822d8, pc2_32134200205460da, pc2_d3c87218467c8dd7 Related tooling cut pc_76ab64ab9e65 (Codex `exec` wrapper rejects a JS template literal with backticks) is the same hazard one layer up.
- **Symptom:** an agent writes `post chat X --body "... `post watch` ..."`; the shell executes the backticks and the output (127 KB in the first incident, 2026-08-05) is posted into an append-only channel. Same family: `$1.63B` becomes `.63B`; an apostrophe ends a single-quoted body. The failure recurred on 08-05, 08-06, 08-11, 08-12 (marked major, into #build), 08-15, despite fixes below.
- **Still live?** LIVE. Post has added mitigations, none removes the cause.
  - `d863a8c` (2026-08-04 23:01 ET): 32 KiB body cap, 1 KiB subject cap. That commit was the direct response to the first backtick incident. It limits blast radius, and did not prevent the 08-12 recurrence.
  - `7f3dd31` (08-11): help text and README name the hazard. Current `--body` help says so verbatim.
  - SKILL.md "Body input" section says the same.
- **Root cause (verified vs inferred):**
  - VERIFIED: expansion happens in the caller's shell before post's `main` runs (nothing in `src/` can see it). Post's own `--body` is documented first in `post send --help` and `post chat --help` usage as the primary form.
  - VERIFIED, design invites it: the skill's own teaching example is the fragile form: `SKILL.md:57` `post send --to workspace:hq --subject "short" --body "message" --json` (double quotes). Post's own error text also steers to it: `src/commands/send.rs:236` and `src/channel.rs:473` recommend `--body '<text>'` (single quotes, apostrophe-fragile), `send.rs:909` likewise.
  - INFERRED: agents copy the example shape; warnings sit in help text nobody reads at the moment of writing. Five documentation passes have not stopped it, so documentation is not the effective layer.
  - Environment: `~/.claude-shared/hooks/bash-footguns.mjs` is the layer that can see the command string, and it has no rule for `post send|chat --body "..."` containing backticks or `$`.
- **Fix shape:**
  - Root-cause (small): make stdin/`--body-file` the canonical form everywhere post teaches or suggests (SKILL example, all `exact_fix`/guidance strings, help usage line order), and demote `--body` to "short single-line, no `$`, backtick, or apostrophe". Optionally, refuse `--body` containing a newline or over ~1 KiB with an exact_fix to the heredoc form: this makes the 127 KB class inert (substituted command output is nearly always multi-line) though it cannot catch `$1` or apostrophes.
  - Band-aid (appropriate here, it is the only layer that can see the shell): add a `post-body-quoting` rule to `bash-footguns.mjs` denying `post (send|chat)` with `--body "..."` where the double-quoted span contains a backtick, `$(`, or `$[0-9A-Za-z{]`, and single-quoted spans containing `'\''` gaps. Outside post; runs in both Claude Code and Codex.
  - Do not add a content sniff inside post: by the time post sees the argument the damage is done.

## 2. Body-source intent ambiguity

- **Cuts:** pc2_459eb76d25a173dc (heredoc without `--send` silently reads), pc2_0dd420308445caf6 (same), pc2_594768b476fd0c03 (`--send "text"`: positional parsed as FILE, "File name too long"), pc2_3ead6dfde44c6cb0 (`--body-file /dev/stdin` under exec with no piped stdin: empty body), pc_9e5d65d24c6c (`post send <room> --body`: positional read as body file).
- **Symptom:** the three body forms (`--body`, `--body-file`, stdin) plus a deprecated positional `[FILE]` mean that a plausible-looking command is silently a read (no send), or a path lookup on prose.
- **Still live?**
  - Bare stdin on a chat read: FIXED-VERIFIED, `b73427f` (2026-09-22) `src/commands/chat.rs:137-190` `refuse_unintended_stdin`; queued input is `invalid_argument`, an open silent pipe is `input_ambiguous`, both with a runnable `--send --body-file -` exact_fix and nothing consumed. Deliberately never auto-sends (diag-B.md ruling: an omitted flag must not become capable of delivering).
  - Positional FILE: errors are now clear (`send.rs:877-912`: nonexistent path and over-255-byte cases both say "that argument is a path to a body FILE"; the send-side room-as-positional case has a dedicated hint at `src/app.rs:113-118`). The positional itself still exists (`--help`: "Deprecated positional spelling of --body-file"). LIVE by design.
  - `--body-file /dev/stdin` with a null stdin: LIVE (minor). `src/channel.rs:470-473` empty-body error says "Retry with ... --body '<text>' or a non-empty FILE/stdin" and does not say that stdin was closed or empty, so the agent cannot tell the pipe never connected (inferred: no probe of stdin state on that path).
- **Root cause:** design invites it. Send intent on `chat` is inferred from flag presence (`chat.rs:107`: `sending = send || body || body_file`), and one verb does both read and write. The positional FILE slot is a leftover alias that competes with the recipient/channel positional.
- **Fix shape:**
  - Root-cause: delete the deprecated positional `[FILE]` from both `send` and `chat` (its only remaining job is producing the errors above); `--body-file -` already covers stdin. Split the chat verb (`post chat <ch>` read, `post say <ch>` or `post chat <ch> send`) only if further confusion recurs; not warranted yet.
  - Band-aid: make the empty-body error branch on `stdin` being a null device or closed pipe and say so. Small.

## 3. Room vs channel: two nouns, two verbs, inconsistent flags

- **Cuts:** pc_05f9423b80b0 (`channels --room`), pc_e24a375b5a51, pc_9cc97e157f57 (`send --room`), pc_18e0df8d9c85 (send help lists rooms only), pc_dbc95bc7b823 ("post room (wade-discovery)" mistaken for a channel), pc_25c8f0351809 (guessed channel, `not_found`), pc2_843a34923133e81d (`send` vs `chat` verbs), pc_9e5d65d24c6c (also here: positional recipient).
- **Symptom:** an agent told "post to the wade room" cannot tell whether that is a room (`post send --to`) or a channel (`post chat`), and tries the wrong verb, or carries `--room` across commands that do not take it.
- **Still live?** PARTIAL.
  - `send --to <channel>`: FIXED-VERIFIED, `238c1c4` (2026-08-25), `src/commands/send.rs:212-262`. It checks the channel registry before saying "unknown room", accepts a leading `#`, and names `post chat <channel> --send` in prose (deliberately prose only: `send.rs:224-235` explains why no command is built). Commit message closes pc2_843a3492 by id.
  - `--room` on `send`/`chat`/`channels`: FIXED-VERIFIED at `src/app.rs:134-158` (per-subcommand parse_failure_fix; `channels` gets a runnable `post channels`), from `864ee4f` (2026-07-26, before the cuts; the cuts describe a hint that already existed and was found via the error). Residual: `send --channel` (unexpected argument) has no hint entry in `parse_failure_fix`.
  - **Reverse direction, LIVE (verified in code):** `post chat <room-name>` where the name is a registered room but no such channel exists returns `channel 'x' does not exist` with guidance "Create it with `post chat 'x' --join`" (`src/channel.rs:444-452`, `src/commands/chat.rs:1644-1650`). It invites creating a stray channel named after the room, never says a *room* by that name exists, and unlike `send` has no did-you-mean. This is exactly pc_25c8f035 and pc_dbc95bc7; the fix landed only on the `send` side.
  - Flag model (pc_05f9423b's "consistent room flag model"): LIVE, design. Mailbox selection is `--room` on inbox/read/watch/catchup, `--to` on send, nothing on chat. `post inbox --help` still says "Mailbox room; defaults to the room containing cwd or cwd basename" (stale after the participants redesign; the default is the bound participant).
- **Root cause:** DESIGN. "Room" is a workspace address for direct mail; "channel" is a separate namespace with a separate verb, and human/task language ("the wade room") uses "room" for both. The 08-25 fix treated one direction.
- **Fix shape:**
  - Root-cause (small, symmetric): one destination resolver used by both verbs. Given a name, look it up in rooms and channels; if it exists only in the other namespace, say which verb works (already done for send). Apply to `chat` NotFound and add did-you-mean over channel names; drop "Create it with --join" when a same-named room exists. Add the `send --channel` parse hint.
  - Bigger option (only if this keeps recurring): accept `post send --to '#name'` and `post chat --to <room>` so the address itself carries the kind, mirroring the typed `workspace:`/`participant:` address grammar already in the tool. This is the identity design applied consistently; skip unless residual cuts appear.
  - Band-aid: refresh `inbox/read --room` help text to describe participant binding.

## 4. Identity guessed from cwd or inherited env

- **Cuts:** pc_7951fc78 (send from wrong repo resolves as room `workspace`, not a #build member), pc2_a904270fadea36b1 and pc2_5b6cea2338604447 (cwd-derived chat/read resolves the wrong room silently; asked for a dry-run/banner), pc2_6a54128c0bb19b03 (inherited `POST_FROM` overrode cwd identity), pc_55fce903 (delivered mail from unregistered room `codex-ui`; unreplyable), pc2_a63ad6a35fe62420 (`post send hq`: no `hq` room), pc_3bb5058994ce (new channel did not auto-join an expected participant).
- **Symptom:** "I composed a correct message from the wrong directory and post would not tell me which directory that was" (`238c1c4` commit text), or a leftover `POST_FROM` silently chose the acting room.
- **Still live?** FIXED-VERIFIED for the main family, by two changes:
  - `238c1c4` (08-25): `acting_room` error now names the full cwd, lists registered rooms (bounded at 8, full list in `matches`), and carries a fix. Closed pc2_a904270f and pc2_5b6cea23 by id.
  - Participants redesign (09-16): `src/channel.rs:154-192` `acting_room` uses the bound participant's workspace, and cwd only in the unbound read-only fallback; `src/participant.rs:439-451` `sender()` always builds `from` from the participant's workspace or participant id. Consequences: (a) an unregistered ad-hoc sender like `codex-ui` cannot occur; `send.rs:129-152` rejects a `--from`/`POST_FROM` that disagrees with the bound participant (pc_55fce903 FIXED-VERIFIED); (b) `channel.rs:170-183` makes a `POST_FROM` pin that disagrees with the bound participant a hard error before any write (pc2_6a54128c FIXED-VERIFIED, from reading; not exercised live); (c) `docs/PARTICIPANTS.md` §4: "Cwd never chooses the sender afterwards".
  - `hq` room: registry state, not code. `post rooms` now lists `hq` (path `/Users/treygoff/Code/hq`); SKILL.md example uses `workspace:hq`. FIXED (config).
  - pc_3bb50589 (no auto-join): not a defect. The devbox sweep (`docs/triage-2026-09-22/devbox-sweep-post-report.md:91-93`) already ruled it "an invocation mistake"; membership is explicit and per participant, and `not_a_member` gives `--join` (`channel.rs:459-463`).
- **Residuals (inferred, low priority):** `participant.workspace` is fixed at bind time from the launch cwd, so a session opened in `~/Code` is workspace `workspace` for its life; open bead post-b6o ("warn when cwd-inferred bind shares a workspace across lineages") is adjacent. Unbound read-only commands still fall back to `POST_FROM`/cwd silently.
- **Root cause:** DESIGN (identity was a location), already replaced.
- **Fix shape:** none needed for this cluster; close it. If a banner is wanted, the cheap version is a one-line stderr `acting as <participant> (workspace <room>)` on writer commands; not evidenced as needed after the redesign.

## 5. Unbounded output

- **Cuts:** pc_c6b4a5b65805 (`watch --snapshot` emitted hundreds of historical events), pc_942e450c8d66 (same; no channel/limit filter), pc_e4f7611ea213 (`chat commons --history 6` exceeded model context), and pc_31c9a2cc8c6d (the 127 KB message itself; see cluster 1).
- **Symptom:** a stale cursor, or a few oversized stored bodies, floods an agent's context with a single command; the useful newest output is truncated away.
- **Still live?** PARTIAL.
  - `7f3dd31` (08-11): `watch --snapshot --limit N` emits the last N events (omitted stay unread; stderr warning). Verified in `post watch --help`. Also since then: `--reason mail|channel|mention` (Lane B), `--digest`, `--from now`, and (`35d8c78`, 09-06) opt-in `--max-bytes` on reads.
  - Join-from-now (`a3ee781`, 09-23; CHANGELOG "Channel joins start from now") removes the dominant cause of "hundreds of stale events": a new member's pre-join history is no longer unread. The explainer reports 172 stale mentions became 0 on two sessions.
  - `d863a8c` 32 KiB body cap (08-04) bounds a single message; with `--history 6` the worst case is bounded by 6 x body cap, but 32 KiB x N still exceeds a sane context.
  - Still true at HEAD: no default cap on `--snapshot`, `--history`, or `--peek`; `--max-bytes` is opt-in; `watch` has `--room` (mailbox) but no per-channel filter.
- **Root cause:** DESIGN default. Every read defaults to "complete", and the consumer with the tightest budget (an LLM context) is the one that must remember to opt in. The stale-cursor trigger is FIXED; the size trigger (oversized junk bodies from cluster 1) is upstream of it.
- **Fix shape:**
  - Root-cause: default byte ceiling on human/agent-facing reads (`chat`/`--history`/`--peek`), with a trailer "N more, use --max-bytes or --limit" (the `--max-bytes` machinery and "count-window skipped vs byte omitted" reporting already exist in `src/commands/byte_budget.rs`). Keep hooks/doorbell paths (`--snapshot` under the supervisor) unbounded or explicitly budgeted, since they need whole snapshots.
  - Band-aid: none beyond documenting `--limit`/`--digest` (already in SKILL/watch reference). Reducing junk bodies is cluster 1.

## 6. Read-cursor state vs the crossed_send guard

- **Cuts:** pc2_d2f7785dc0d160a1 (read into `/dev/null` refused, so scripted read-then-send loops on crossed_send), pc2_1adf1e38054c99c2 (a `--peek` read left the cursor behind; the next gate send bounced with `crossed_send`).
- **Symptom:** an agent inspects a channel non-consumingly, then sends; the send is refused for unseen messages.
- **Still live?** MOSTLY FIXED-VERIFIED.
  - `ca430aa` (2026-08-25): crossed_send refuses only for messages **addressed to the sender** (an @mention, a reply to its own message, or an owner-room message); other unseen traffic prints a warning and delivers; every decision is logged to `crossed-send.jsonl` (`src/channel.rs:600-700`; schema text). Both cuts predate it (08-11).
  - `--discard` and `--discard-through <id>` (`b4c8865`, 08-10, one day before these cuts) are the documented "consume without printing" path, and the `/dev/null` refusal message points to them (`chat.rs:1015`, SKILL.md "A read that would print unread messages into /dev/null is refused. To skip messages, use `--discard` ...").
  - Residual by design: a targeted message (mention or reply) still bounces a send after a peek. The refusal carries `--anyway` as exact_fix (`channel.rs:743-780`).
- **Root cause:** DESIGN, intentional. "You must have seen what was addressed to you before replying" is the point of crossed_send; the friction was that the guard was over-broad before 08-25 and the consume-quietly option was undiscoverable on 08-11.
- **Fix shape:** none. Optionally a single SKILL sentence for coordination scripts: `peek` then `--discard-through <last-id>` then send; already implied.

## 7. Event-schema skew: older readers choke on new event kinds

- **Cuts:** pc_66f84ca6b16e, pc_e29f8b9685fd (channel `profile` events render as `[?] unreadable envelope` and repeat warnings in every joined channel for watchers predating profiles).
- **Symptom:** the writer added a new channel event kind (`profile`, `09da741`, 2026-08-05); readers whose parser only knew `join` treated it as corrupt.
- **Still live?** LIVE as a class. That specific instance is over (readers upgraded; `34b317b` gave unreadable events a `reason` field), but the mechanism is unchanged at HEAD:
  - `src/channel.rs:1183-1190`: the parser **rejects any event other than `join` or `profile`** with a `config` error ("event 'x' is unknown; only 'join' and 'profile' exist"). That check dates from the original store (`679e19d`, 2026-07-22) and was widened by hand for `profile`. The next event kind will break every older binary in the same way.
  - `27ad67a` (08-06) made past-cursor unreadable channel messages fail closed, so on an old binary a new event kind is not only rendered as garbage; it can block cursor advance. (Inferred from the commit title and `post-8pn`; not exercised.)
  - Version skew is chronic in this estate: Mac, devbox host, five cells (`docs/RELEASING.md:67-80`), and bridge v2 now syncs all channels between hosts by default (CHANGELOG, post-xiy), so skew now crosses machines.
  - Mitigations that exist and are not this fix: `post version --json` capabilities, contract samples plus consumer canaries (Lane D), skill-manifest verify. They detect drift in installed readers; they do not make an old reader tolerant.
- **Root cause:** the code has a design-level bug: the reader validates a closed enum while the format is meant to be additive (Envelope/ChannelMessage fields are `serde(default)` elsewhere, per PARTICIPANTS.md §5). Additive fields are tolerated; additive *values* are not.
- **Fix shape:**
  - Root-cause: readers treat an unknown `event` value as an opaque system event (skip in unread counts, render a generic `[event: kind]` line, never advance-block, never "unreadable"); write a test that feeds a `future_kind` event through chat, watch, catchup, and doctor. Then the writer can add kinds without a fleet upgrade.
  - Band-aid: none is safe; the old binaries already deployed cannot be fixed, only replaced.
  - No open bead covers this (checked `bd list`).

## 8. Agents guess machine-output shape and smoke recipes

- **Cuts:** pc_7ace7ed01bf5 (guessed inbox `.items`), pc_a8aa8b019958 (guessed inbox `.messages`), pc_e46864f3abb8 (compared human `--peek` output byte-for-byte; framing changed), pc_5cb09ec597e3 (pre-created `POST_MAIL_ROOT` made `post rooms` fail config_invalid).
- **Symptom:** jq null-iteration on a wrong key; a "snapshot is idempotent" smoke that compares text with volatile framing; a smoke store seeded the wrong way.
- **Still live?** FIXED-VERIFIED (guidance), with one design smell.
  - SKILL.md now states the inbox envelope and the jq idiom: "Inbox JSON is `{ok, participant, room, unread, count, ...}`; iterate `(.unread // [])[]`", and "Smokes assert on `--json` output or the cursor file. Text rendering changes with framing, profiles, and whatever arrived since" and the throwaway-store recipe (`SKILL.md:56-65, 255-262`).
  - `post contract samples` and `contract/samples/*.json` publish every output shape (Lane D, `tests/contract_samples.rs`).
  - pc_5cb09ec5: verified live in a scratch dir: `mkdir root; POST_MAIL_ROOT=root post rooms` returns `ok:true, rooms:[]`, rc 0. So that scenario no longer fails (could not identify which commit changed it; the original failing condition may have been narrower).
  - Design smell, VERIFIED from the samples: list payload keys differ per command (`inbox.unread`, `chat.messages`, `channels.channels`, `rooms.rooms`, `who.participants`, `profile-list.profiles`). Two independent agents guessed `.items`/`.messages` for inbox.
- **Root cause:** docs/skill gap (fixed) over a mildly inconsistent contract.
- **Fix shape:** nothing more required. Do not add `items` aliases. If ever breaking the contract for another reason, name the list `items` everywhere; until then the samples are the answer.

## 9. Send receipt says `archived: true` but gives no readback

- **Cut:** pc2_39f0a982b749210c ("post send reported archived=true, but post read from the sending room could not find the sent id").
- **Still live?** PARTIAL. The text receipt was fixed (`789b389`, 09-16): it prints `canonical message retained at <kind>:<name>` and `read it back with: post read <id>` (`src/commands/send.rs:412-424`). The JSON receipt is still `{ok, envelope, archived:true, delivery?}` (`src/output.rs:547-556`, `send.rs:405`) with no store owner and no readback. `post read` also has an explicit diagnosis when the id is archived between two other rooms (`read.rs:925-1001`), which is what the older half of this cut describes.
- **Root cause:** naming/contract: `archived` means "written to the immutable archive", which agents read as "delivered and retrievable by me"; under fan-out the sender is often not a recipient (`send.rs:392-398` even prints "sender is not a frozen recipient").
- **Fix shape:** root-cause: JSON receipt gains `retained_at: {kind, name}` and `readback` (the same command the text form prints), leaving `archived` for compatibility. Small. Band-aid: none needed.

## 10. Post repo build/deploy/release hygiene

- **Cuts:** pc_08fad241570b (stale `~/.local/bin/post` shadowed `~/.cargo/bin`), pc_3168faf810f8 (main failed `cargo fmt --check`), pc2_7f42d066d80201a3 (release inventory assumed the wrong hook source path).
- **Status:**
  - fmt: FIXED-VERIFIED. `d956f51` "cargo fmt: clear formatting debt from v0.3" landed 2026-08-04 22:18 ET, four minutes after the cut (22:14 ET); `scripts/gate.sh` added in `238c1c4` so the gate is one command; `cargo fmt --check` is clean at HEAD (run today, rc 0).
  - Inventory: FIXED-VERIFIED. `post contract skill-manifest` prints the sha256 of every file in the shipped skill bundle (42 files today, including `hooks/watch-notice.mjs`), which is the "shared inventory command" the cut asked for; it replaced guessing paths.
  - Shadowing: PARTIAL. `docs/RELEASING.md:73-77` now says "Agents never install user-space copies; `~/.local/bin/post` shadowing the canonical binary is the drift machine," and `scripts/install-post.sh` installs atomically with a verified backup and rollback. It does not check that the installed path is the one PATH resolves first. That hazard exists today on this Mac: `type -a post` shows both `~/.local/bin/post` (Sep 25) and a stale `~/.cargo/bin/post` (Aug 22, 4.4 MB) on PATH; the newer one wins only by PATH order. `post doctor` has no shadow check (`rg shadow src/commands/doctor.rs` finds nothing). The devbox sweep also found `post version --json` reporting a `build_sha` not matching the installed contents.
- **Root cause:** environment/process, plus a missing check in `install-post.sh`/`doctor`.
- **Fix shape:** root-cause: after installing, `install-post.sh` runs `command -v post` and fails unless it equals the target (and warns on additional `post` entries in `PATH`). Band-aid: delete the stale `~/.cargo/bin/post` and stop using `cargo install` for this tool. Optional: a `doctor` check listing every `post` on PATH with its build_sha.

---

## Not about post (skipped)

- pc2_45ae6a8bc397ac5f: `porchd` (Python `http.server`) does not drain POST bodies on error, poisoning Tailscale Serve's pooled connection. Porch, not post. (Fix named in the cut: drain the body or send `Connection: close`.)
- pc_76ab64ab9e65: Codex `functions.exec` wrapper rejects a JS template literal containing backticks. Harness tooling; the underlying trigger is cluster 1.
- pc2_e1d4e47b2c225ad4: an agent piped a Mach-O binary through `sed` during shell orientation. Shell habit; no post code involved.

## Cross-cutting observations

1. **Fixed by redesign, not by patching.** Cluster 4 (7 cuts) died with the participants model, not with any of the eight targeted error-message commits. The ledger's `resolved` flags for those were written at the time of the workaround, not the fix; several cuts (pc2_a904270f, pc2_5b6cea23, pc2_843a3492) were closed by the commit that quoted their ids, which is the good pattern.
2. **Half-fixed symmetric problems.** The `238c1c4` fix to room/channel confusion handled `send --to <channel>` only. The mirror case (`chat <room>`) still tells the agent to create a channel. When fixing a confusion between two nouns, fix both directions in one change and test both.
3. **Documentation is not a control for shell-quoting.** Clusters 1 and 2 repeat six times over eleven days across three agents with the warning in `--help`, README, and SKILL. The only effective controls are a layer that sees the shell (a PreToolUse rule) or removing the fragile form as the teaching default. The teaching example itself (`SKILL.md:57`) and post's own error guidance use the fragile form; fix those regardless.
4. **Additive-format promise is only half implemented.** New fields are tolerated (`serde(default)`), new enum values are not (cluster 7). This is the highest-value structural item in the chunk because bridge v2 makes channel data cross hosts by default and skew is now cross-machine.
5. **Defaults are "complete", consumers are budgeted.** Cluster 5's cuts all come from LLM callers hitting default-unbounded reads. Opt-in bounds landed (`--limit`, `--max-bytes`, `--reason`, join-from-now) after the cuts; defaults did not change.
6. **Instrument caveat.** I established "fixed" from source reading and from three live read-only probes (help output, `post rooms` on a scratch root, `cargo fmt --check`, `post contract samples` listing). I did not run send/chat/join, so the identity hard-error paths (`channel.rs:170`, `send.rs:129-152`) and crossed_send targeting are code-read, not exercised.
7. No open bead covers: chat room-vs-channel NotFound hint (cluster 3), unknown event kinds (cluster 7), body-quoting hook or `--body` demotion (cluster 1), PATH shadow check (cluster 10).
