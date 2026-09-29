# Design: one doorbell supervisor per host (lane E)

Status: **approved for implementation** (Aster, GO on revision 2, with seven brief corrections folded in as revision 2.1). Revision 1 (2f2556e) was reviewed by Aster, who approved the architecture and required E1–E8; each is indexed at the end. The builder names its singleton lock mechanism in its report. Author: Nightjar.

## The problem

Waking an idle agent takes five mechanisms today: the Claude Monitor, per-harness hooks, the Python `post-doorbell` daemon, the Codex per-agent timer (launchd on the Mac, systemd on the devbox), and the Cursor and Grok wrappers. Each has its own filter, lifetime, and failure handling, and all of them fail silently. Tonight 15 of 19 devbox timers were ringing panes that no longer existed; the Python daemon dropped all participant mail; the Claude Monitor expires after 30 minutes.

## What was verified before this design

- **Discovery is exact.** `post participant list --json` returns every participant with its full 64-hex `conversation_key_digest`, `harness`, and `ended_at`. `herdr agent list` reports `agent_session.value` for Claude and Codex panes. sha256 of that value equals the participant's digest: mine matches `claude-83e5e99e`, Aster's matches `codex-5a036212`. The supervisor matches the full digest against post's list output: no id-prefix logic, and no reading of post's files.
- **herdr's sink capability** (herdr 0.9.1, both hosts):
  - `herdr agent get` and `herdr agent prompt` accept a pane id (`wC:p31`). They reject a terminal id (`agent_not_found`).
  - The socket API's `agent.prompt` takes only `target`, `text`, and `wait`. **There is no conditional prompt:** no expected session, revision, or terminal precondition.
  - A prompt to a `blocked` agent is rejected with `agent_blocked` before input is sent. The window between `get` and `prompt` cannot be closed from outside herdr. See "Ring."
- **Snapshots are side-effect free for this use.** `post watch --snapshot` returns before the heartbeat and lease code and does not route pending mail (`route_pending: !snapshot`). `post participant show` resolves without touching the lease (`participant::resolve`, no `touch`).
- **Digest mode is unusable here.** `--digest` folds unreadable events into ordinary groups (`digest_batch` keys on room, source, and pending only), which would break E4. The supervisor reads per-event JSON.

## What it is

`post-doorbell`: one long-running Node program per host (launchd on the Mac, a systemd user service on Linux), in `skills/post/hooks/doorbell-supervisor.mjs`, next to the herdr and notice helpers it absorbs from the Codex monitor. The same entry point carries the agent-facing commands (`enable`, `disable`, `select`, `subscribe`, `status`). The post skill is how agents discover it. There is no Rust forwarding command.

It never writes any participant's mail state. It reads through `POST_PARTICIPANT=<id> post watch --snapshot --json` and `post participant list --json`. It renews no lease, claims no presence, routes nothing, and consumes nothing. Post's participant-scoped projection stays the only authority on what a participant can see. The supervisor shares orchestration, not inbox identity: one scan per armed subscription, never an aggregate watch.

**Singleton.** A second instance (a second installer run, a manual start, a duplicate unit) must exit nonzero with `already running (pid N)`, so two supervisors can never double-ring. Node 26.9 has no `fs.flock` (Aster checked live), so the builder picks a proven mechanism and names it in the report. The recommendation is a kernel lock the operating system releases at process death: a small `python3` child that takes `fcntl.flock(LOCK_EX|LOCK_NB)` on `$POST_MAIL_ROOT/doorbell/supervisor.lock`, reports success, and holds the lock until its stdin closes. The supervisor does no work until the child sends an explicit lock-acquired handshake. If the child exits unexpectedly, the supervisor stops all delivery and exits immediately; otherwise a killed helper would release the lock while the old supervisor kept ringing, and a second one could start (Aster's acceptance condition). python3 is already a bridge dependency on both hosts. No dependency is added and PID reuse cannot matter. Required tests: simultaneous start; an abrupt parent crash, then restart; helper death with the parent alive, tested separately; and a stale owner with a reused PID. No owner file or receipt is written before ownership is established.

## Terms

- **Binding:** a herdr pane that carries a participant's conversation. Its **target generation** is (pane id, terminal id, session digest). Any change in any of the three is a different generation.
- **Subscription:** the delivery unit, keyed by (participant id, sink, target generation). All dedupe state belongs to a subscription, never to a bare participant (E1).
- **Armed:** a subscription is armed when it is enabled; only armed subscriptions scan and ring. Discovery binds everything it can see; activation is selective (E7).

## Discovery (every 2 seconds)

1. Run `herdr agent list`. If it fails, do nothing else this tick: a herdr error is not evidence that any target is gone, so nothing retires.
2. Refresh the participant list from `post participant list --json` when the participants directory changed, and at least every 60 seconds. Digests never change for an id, so this is a cache of post's own output.
3. For each pane whose `agent_session.kind` is `id`, compute sha256 of the value and find the participant with that exact `conversation_key_digest`. No match: not a target; logged once.
4. **Ambiguity.** If two panes carry the same session digest, the participant's bindings are `ambiguous` and unarmed, whatever the preferences say, until the agent runs `post-doorbell select --pane <pane_id>`. The supervisor never chooses one arbitrarily. A selection names a pane plus a session digest, so it cannot move to a pane carrying anything else.
5. **An explicit selection overrides discovery** for that participant. It lives only while its pane carries the same digest.

## Activation and preferences

Agent-facing commands resolve the acting participant with `post participant show --json`, the same resolution every post command uses:

- `post-doorbell enable [--focused]`: re-arm this participant's herdr subscription after a `disable` (it is armed by default). `--focused` opts into waking while the pane is focused; the default is off (the lead's ruling; Trey may reverse it).
- `post-doorbell disable`: disarm it.
- `post-doorbell subscribe --channel <name>` and `--unsubscribe --channel <name>`: ring for ordinary channel messages in that channel. Direct mail and mentions always ring when armed.
- `post-doorbell select --pane <pane_id>`: resolve ambiguity (above).
- `post-doorbell status [--json]`: this host's bindings, their armed or unarmed state, the last outcome, failures, and blind spots.

Preferences live at `$POST_MAIL_ROOT/doorbell/prefs/<participant-id>.json`, outside post's participant directories. Each has a monotonically increasing `version`, and every outcome log line records the preference version it used.

**Rollout tonight (Aster's E7 ruling).** The supervisor discovers every herdr agent, but it arms only these:

- the participants of the four live Codex timers it migrates (see Migration), with their old settings carried over; and
- this project's overnight participants (`post-repo`: Nightjar and Aster).

**Default changed 2026-09-23 (Trey's ruling: "definitely turn it on by default").** The rollout list above was launch-night scoping; the opt-in is gone. A participant with no prefs file, or a prefs file with no `enabled` field, is enabled and rings for direct mail and mentions once bound. `post-doorbell disable` is the opt-out: it persists `enabled: false`, which `subscribe` and `select` never overwrite and only `enable` reverses. `focused` and `desktop` stay opt-in flags on `enable`.

**Cursor and Grok are out of scope tonight (E2).** herdr gives them no conversation key, and a terminal id does not prove conversation lifetime: a shell can start a second harness conversation in the same terminal. They keep their existing in-session wrappers. The supervisor does not accept a terminal-only registration, and the design claims no generation protection for those targets.

## Scanning

**Hints plus reconciliation (E6).** File events are hints, never the authority.

- Targeted watches, re-armed whenever the armed set changes:
  - each armed participant's `participants/<id>/` (inbox, routing, cursors, channel membership);
  - its workspace's `<root>/<workspace>/inbox`;
  - `lineages/` for its lineage;
  - `channels/<name>/` for each channel it has joined;
  - `rooms.json`;
  - the doorbell `prefs/` and `config.json`.
- A hint marks the affected subscriptions dirty, and hints are coalesced over 250ms. A watcher error, overflow, directory replacement, or new relevant directory schedules a full reconciliation of every armed subscription and re-arms the hints.
- **Authoritative reconciliation every 60 seconds:** every armed subscription is scanned whether or not a hint fired. Gaps the hints knowingly leave to reconciliation are frozen routing receipts that keep an old address visible after a rebind, lineage mail routed under a different lineage's directory, membership and config changes made outside the watched paths, and bridge imports landing under renamed paths. The supervisor does not reimplement post's routing policy in JavaScript to chase perfect coverage.
- **Dirty generations.** Each subscription has a dirty counter. A scan records the counter value when it starts and clears dirtiness only if the counter is unchanged when it finishes, so a change that lands during a scan is never lost.
- **Fair, bounded concurrency.** At most 2 snapshots run at once (a tunable setting), drawn from a round-robin queue of dirty subscriptions. Each snapshot has a 20-second timeout. One slow participant cannot stall the rest.
- A subscription whose pane is `working` or `blocked`, or focused without `--focused`, is not scanned. Its hooks surface mail on the next turn. When the pane becomes scannable again, it is marked dirty.

**The scan command.** `POST_PARTICIPANT=<id> post watch --snapshot --json --limit 0`, with no `--reason` filter, then selection in JavaScript (below).

- `--limit 0` keeps every event, so the newest unread state is never dropped.
- Output over 32 MiB is killed and reported as `failed: snapshot_oversize`, never parsed partially.
- **B1 check, done at e7fbdd3:** B1 gives unreadable channel events reason `channel`, never `mention`. A mention-only filter would therefore hide an unreadable channel, so the supervisor never passes `--reason`. The cost is larger output for participants with big unsubscribed channel backlogs, which the 32 MiB cap bounds visibly.

**Parsing is all or nothing (E4).** A scan counts only if post exits 0 within the timeout, every line parses as JSON, and every event has the fields the contract requires for its variant: `event`, `address.kind`, `address.name`, `id`, and `channel` for channel variants. Anything else is `failed`: a nonzero exit, a timeout, a malformed or truncated line, an unknown `event` value, or oversize output. It is never partially accepted and never `accepted`. Unknown optional fields are fine; D's contract fixtures cover these cases.

**Selection after parsing (E5).** From the parsed events the supervisor keeps:

- every `mail` event (direct, workspace, and lineage mail that post projects for this participant);
- every `channel_message` with `reason: mention`, in any channel;
- ordinary `channel_message` events only from channels in the subscription's list;
- every `unreadable` event, which is reported as a blind spot, never counted as a message.

A participant in two channels who subscribes to one gets ordinary events from that one only, plus mentions from both. Changing preferences bumps the version and changes the next eligible set predictably. Subscribing to a channel that has old unread messages rings once for them, because they are unread and this subscription has not announced them. Unsubscribing drops them from eligibility without marking anything read.

## Ring

For each armed subscription with fresh eligible events (keys not yet in that subscription's `announced` set):

1. **Recheck, best effort (E3).** First the participant: `POST_PARTICIPANT=<id> post participant show --json` must report it bound with no `ended_at`. The 60-second list cache can never keep an ended participant armed; an ended participant retires here. Then the pane: `herdr agent get <pane_id>` immediately before prompting:
   - same terminal id and session digest, idle or done, unfocused unless `--focused`: prompt. A Claude pane Herdr reports `working` counts as idle only when its session's turn mark is `idle` with background work pending, because Claude keeps its title spinner, which Herdr reads, while background tasks run after the main turn ends (post-bt2). The Claude mail hook writes the mark: `busy` at UserPromptSubmit (which also fires when a background completion resumes the turn), `idle` at Stop with `background: true` when Stop's `background_tasks` is non-empty, deleted at every SessionStart and at SessionEnd. A plain idle mark never overrides Herdr, since Herdr goes idle on its own when nothing runs in the background, and the mark cannot see a parallel Stop hook that blocks the stop. The mark is read only by the session digest of a pane Herdr lists now, so a crashed session's leftover file is never consulted, and a resumed session clears it at SessionStart. Residual, accepted by the lead on 2026-09-29 after the second Sol review: the mark cannot tell whether a parallel Stop hook blocked the stop, so a Stop blocked while background work runs leaves an idle-with-background mark on a turn that is still working, and the doorbell rings that busy pane. This is routine, not rare, in /goal-style sessions, whose evaluator Stop hook blocks the stop until the goal holds, and in any session with a blocking Stop hook such as a reply-style check. The consequence is bounded: `herdr agent prompt` (herdr 0.9.1, `queue_agent_prompt` and `encode_api_submission_parts`) writes the notice text, inside bracketed-paste markers when the pane has paste mode on, then one unmodified Enter, and never Escape or Ctrl-C, and it refuses a blocked agent. Claude Code queues a message submitted while it works instead of interrupting the turn, and hands it to Claude once the running tool calls finish, within the same turn (code.claude.com/docs/en/interactive-mode, "Queue messages while Claude works"). The notice starts with `[post-doorbell:v2]`, so it queues as a message, not a `!` shell command or a slash command. The mail nudge therefore arrives mid-turn as extra input; it does not stop the work;
   - busy or focused: `deferred`;
   - a different terminal id or session digest, or the pane is gone: `retired` for this subscription;
   - a lookup error or unparseable reply: `failed`, retried; never a retirement, and never a retirement of other subscriptions.
2. **Prompt:** `herdr agent prompt <pane_id> <notice>`, with no `--wait`.
3. **The window is not closed.** herdr has no conditional prompt, so a pane can change between step 1 and step 2. The design narrows the window and does not claim to eliminate it. The notice carries the defense: it names the target participant and tells the recipient to compare it with its own binding.

**The notice:**

`[post-doorbell:v2] Automated, non-authoritative Post notice for participant <id>. If "post participant show" does not report <id>, this notice is not for you: ignore it and report it to your operator. Waiting: <N> direct, <M> mentions, <K> in #<channel>. Read with post inbox / post chat <channel>.`

Degraded states change the words rather than inflating counts (E4):

- An event carrying `cursor_unusable: true` (a boolean on the existing event, not a separate variant) reads "<N> re-reported while cursor state is unavailable; may include previously read messages", never "N new."
- `pending: true` mail reads "<N> waiting to be routed", counted separately from routed mail.
- `unreadable` events read "<N> unreadable item(s) in #<channel> or the inbox; mentions there are unknown."

Only participant ids, counts, and channel names appear. Ids and channel names are validated against post's naming grammar, and anything else becomes `<?>`. Subjects, display names, previews, and bodies never appear, since all of them are attacker-reachable.

## Outcomes and state (E1)

Each subscription keeps two independent sets under `$POST_MAIL_ROOT/doorbell/state/<participant>/<sink>-<generation-hash>.json`:

- `announced`: event keys included in an `accepted` agent prompt. A key is (kind, address kind, address name, source, id, **state class**), where the state class is `routed` or `pending`, and `healthy`, `degraded` (cursor unusable), or `unreadable`. When the same id moves from pending to routed, or from degraded to healthy, the key changes, so the actionable state rings again. Announcing a provisional or degraded item never suppresses a later actionable unread event with the same id (a required test);
- `notified`: keys included in a desktop notification. This set never suppresses an agent prompt.

| Outcome | Meaning | State written |
|---|---|---|
| `accepted` | herdr took the prompt for the rechecked pane. This does not prove a turn ran or a message was read. | `announced` becomes exactly the current eligible keys |
| `notified` | A desktop notification only (the cmux sink, kept as an explicit per-participant add-on). Not an agent wake. | `notified` only |
| `deferred` | Busy or focused at the recheck. | none |
| `retired` | The generation ended: a new terminal or session, the pane is gone, or the participant is ended. Logged loudly once. | the subscription's state is frozen and pruned after 7 days |
| `failed` | herdr or post errored, timed out, or returned malformed or oversize output. Carries the exit code, timeout versus exit, and at most 300 bytes of sanitized stderr. | none |

- Setting `announced` to exactly the current eligible keys prunes consumed keys and never forgets a key that is still unread. A wholly successful scan with no fresh eligible events also prunes `announced` down to the current eligible keys, even though no sink call is made. A `failed` or `deferred` outcome never advances either set.
- **State never transfers across generations or participants.** A resumed conversation in a new pane is a new generation with an empty `announced` set, so it gets one notice for what is currently unread. A retired generation's state is never read again.
- **Delivery contract:** at-least-once notification attempts while an armed, healthy subscription exists. It is not a guarantee of agent execution or of every message being read. A crash between herdr accepting a prompt and `announced` being saved rings again on restart: a duplicate metadata notice. Exactly-once is not promised.
- **Lease expiry.** A binding whose participant's lease expired but whose pane is live and idle still scans and rings, but only for what post actually projects for that participant. The supervisor never bypasses post routing, never adopts held or pending mail, and never renews the lease to make that true. An ended participant (`ended_at` set) retires.
- **Bounded retries.** Consecutive failures back off exponentially from 5 seconds to a 5-minute cap. After 5 in a row, the subscription is `broken` in `status`, with the last error, and keeps retrying at the cap. A failure is not evidence the target is gone, so it never retires on failures alone.
- **Migration fences.** A post admission error or fence counts as `failed` with its reason and retries like any other failure.

## Health and logs

- `$POST_MAIL_ROOT/doorbell/heartbeat.json` is a small file rewritten each tick: pid, start time, tick sequence, and time.
- `$POST_MAIL_ROOT/doorbell/health.json` is rewritten atomically only when something changes, and at least every 30 seconds. It holds the supervisor version; the herdr and post versions; and every binding with its generation, armed state, last outcome and time, last successful scan, consecutive failures, and blind spots (unreadable sources). It never contains mail content.
- `post-doorbell status` reports supervisor liveness separately from scan health. Liveness means whether the singleton lock is held and how old the heartbeat is. The status is `running`, `stale` (lock held, heartbeat older than 10 seconds), or `dead` (lock not held). A once-green health file from a dead supervisor never reads as healthy. A subscription that mentions nothing because its channel is unreadable says so; it never reports "no mentions."
- Logging: one JSON line per non-`deferred` outcome, one per binding or generation change, one per reconciliation that found hint gaps, and nothing per quiet tick.
- **Measured cost (E6, bounded per approval item 7).** A representative before/after workload on a fixture store, not an hour of unrelated live traffic, and not a gate to wait on. State the enabled-subscription count, the history size, and the event workload. Report CPU time and scan counts for the supervisor against the equivalent per-participant timers, with reconciliation cost reported separately. Name the limits of the comparison honestly.

## Migration (E8)

The installer is idempotent and records each step in `$POST_MAIL_ROOT/doorbell/install-receipt.json` as it goes, so an interrupted run resumes correctly.

1. **The name collision.** The devbox has the old Python script at `~/.local/bin/post-doorbell` and a disabled `post-doorbell@.service` template with no instances. The installer moves the script to `~/.local/bin/post-doorbell.legacy-<sha8>` and records its hash in the receipt. The unused unit template stays in place: deleting it buys nothing and widens what the installer owns. The repository copy of the Python daemon (fixed by C1) stays available for use outside herdr.
2. Install and start the supervisor, then confirm the singleton lock and a healthy first tick.
3. **Per old timer** (`post-codex-doorbell@*` on the devbox, `dev.post.codex-doorbell.*` on the Mac):
   - Read the effective settings from the unit and its environment: participant, channels, reasons, focus policy, and sink.
   - Create the equivalent armed subscription.
   - Wait until `status` shows it healthy: bound to a live generation, with at least one successful scan using the same channel and reason selection.
   - Only then, disable that one timer. Record the unit's hash and the state the installer left it in.
   - A timer whose target the supervisor cannot bind, or whose settings it cannot reproduce, stays on its old mechanism and is listed as not migrated.
4. **Cutover proof before the bulk.** Migrate one target per platform first and prove it with a nonce: a direct message to that idle agent, answered by that agent under its own participant id, matched to the supervisor's `accepted` line. Only then migrate the rest.
5. **Uninstall.** By default it stops and removes only the supervisor, leaves every legacy unit as it is, and prints the exact restoration command. `--restore-legacy` restores the recorded migration set: first it stops the supervisor, then it re-enables each recorded unit only if the unit file still has its recorded hash. (Hash and disabled state cannot distinguish "still disabled by the installer" from "disabled again on purpose," so restoration is an explicit choice, never a default.) The Python script is restored only if the file at the path is still the one the installer wrote.
6. **Claude Monitor:** the skill tells agents inside herdr to arm the supervisor, and to use the Monitor outside herdr. Live Monitors belong to other sessions and are never killed. A session with both gets both events, which is within the at-least-once contract.
7. **Out of scope:** Cursor and Grok wrappers (E2); hooks, which stay as next-turn catch-up.

**Installer tests:** failed startup (the lock is held, herdr is missing, post is missing, a first scan fails); interrupted migration (killed between creating a subscription and disabling a timer, then rerun); default uninstall leaving legacy units untouched; `--restore-legacy` with a unit edited after install (refused for that unit).

## Tests

Unit tests (`node --test`, with fakes for herdr and post; the fake post emits D's contract samples once D lands):

- **Discovery:** an exact digest match; no match; two panes carrying one session (ambiguous and unarmed, then resolved by `select`); `select` refused for a pane carrying a different digest; a herdr list failure (nothing retires).
- **Generations:** a terminal change or session change retires; a resumed session in a new pane starts empty state; one participant's state is never read by another.
- **E1 sinks:** `notified` never suppresses a later `accepted` prompt; switching sinks re-announces.
- **E3 recheck:** busy gives `deferred`; a changed session gives `retired`; a lookup error gives `failed` and never retires other subscriptions.
- **E4 parsing:** a nonzero exit, a timeout, a truncated line, an unknown event, and oversize output each give `failed`, never `accepted`; `cursor_unusable`, `pending`, and `unreadable` each produce their own notice wording; an unreadable channel under a mention filter shows as a blind spot.
- **E5 selection:** a member of two channels subscribed to one; subscribe, unsubscribe, and resubscribe with old unread messages.
- **E6 scheduling:** a change during a scan is not lost (the dirty counter); a watcher overflow triggers full reconciliation; concurrency never exceeds the limit; a slow snapshot does not block other subscriptions.
- **Lifetime:** lease expired with a live idle pane still rings; an ended participant retires; backoff and `broken`.
- **Notice sanitization:** hostile channel names and ids.

**Live proof:** the nonce cutover in Migration step 4, once on the devbox and once on the Mac.

## Aster's review, as folded in

| Item | Where |
|---|---|
| E1 subscription-keyed dedupe, sinks separated, explicit override, ambiguity, no state transfer | Terms; Discovery 4–5; Outcomes and state |
| E2 no terminal-only registration; Cursor and Grok keep their wrappers; list output, not prefix rules | Verified; Activation; Discovery 2–3 |
| E3 no conditional prompt exists; best-effort recheck; notice self-check; errors never retire | Verified; Ring |
| E4 degraded wording, pending kept separate, blind spots, all-or-nothing parsing, visible bounds | Scanning; Ring (notice); Health |
| E5 selection after parsing; two channels, one subscribed; preference changes | Scanning (Selection) |
| E6 hints plus 60s reconciliation, listed gaps, dirty generations, concurrency 2, fairness, measured cost | Scanning; Health |
| E7 discover all, arm selectively, no hidden opt-in | Activation (Rollout) |
| E8 carry settings, prove health, nonce first, safe uninstall, singleton, installer tests | Migration; What it is |
| Location, entry point, name collision | What it is; Migration 1 |
| Lease-expiry wording; the at-least-once guarantee restated | Outcomes and state |
| GO corrections: uninstall default, cursor wording, state-class keys and pruning, ended recheck, singleton, liveness, bounded benchmark | Migration 1 and 5; Ring; Outcomes and state; What it is; Health |
