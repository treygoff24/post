# Design: one doorbell supervisor per host (lane E)

Status: draft for Aster's review. No code until Aster rules. Author: Nightjar, 2026-09-22 late.

## The problem, in one paragraph

Waking an idle agent takes five mechanisms today: the Claude Monitor, per-harness hooks, the Python `post-doorbell` daemon, the Codex per-agent timer (launchd on the Mac, systemd on the devbox), and the Cursor and Grok wrappers. Each has its own filter, lifetime, and failure handling, and all of them fail silently. Tonight 15 of 19 devbox timers were ringing panes that no longer existed; the Python daemon rejected all participant mail; the Claude Monitor dies after 30 minutes. Each per-agent install is also a manual step keyed to a reusable pane name, so it goes stale the moment the pane goes away.

## The key fact that makes this simple

herdr already knows which conversation runs in each pane, and post already keys participants by that conversation. Both were verified live tonight on both hosts:

- `herdr agent list` reports `agent_session.value` for Claude and Codex panes: the harness's own session id.
- Post's participant id is `<harness>-<first 8 hex of sha256(conversation key)>`, where the key is `CLAUDE_CODE_SESSION_ID` or `CODEX_THREAD_ID`/`CODEX_SESSION_ID`. The participant record stores the full 64-hex `conversation_key_digest`.
- sha256 of my herdr session value starts `83e5e99e`, and I am `claude-83e5e99e`. sha256 of Aster's Codex session value starts `5a036212`, and Aster is `codex-5a036212`.

So the supervisor can bind a pane to exactly one participant by comparing full digests. It needs no per-agent install, no pane naming, and no guessing. A new conversation in the same pane hashes differently, so it is a different target automatically. That is the "target generation" Aster asked for, derived rather than configured.

## What it is

`post-doorbell`: one long-running Node program per host (launchd on the Mac, a systemd user service on Linux). It is installed by `install-doorbell-supervisor.mjs`, next to the existing hooks. It reuses the Codex monitor's herdr and notice code; most of that file's logic moves into it.

It never writes to any participant's mail state. It reads through `post watch --snapshot` run as each participant (`POST_PARTICIPANT=<id>`), and a snapshot, verified in `src/commands/watch.rs`, returns before the heartbeat and lease code and does not route pending mail. The supervisor therefore never renews a lease, never claims presence, and never consumes anything. Post's own participant-scoped projection stays the single authority on what is unread. The supervisor shares orchestration only, not inbox identity: one scan per participant, never a privileged aggregate watch.

## The loop

1. **Discover (every 2 seconds).** Run `herdr agent list` once. For each pane with `agent_session.kind == "id"`, compute sha256 of the value. Look up a participant whose `conversation_key_digest` equals it. The candidate id is `<harness>-<digest[..8]>`; widen the prefix if post widened it, and always confirm the full digest in `participants/<id>/participant.json`. A match is a **binding**: (participant id, pane id, terminal id, session digest). Anything unmatched is ignored, and logged once.
2. **Decide who to scan.** A binding is scannable when its pane is `idle` or `done`, and either it is not focused or the participant opted into focused wakes. `working` and `blocked` panes are not scanned; their own hooks surface mail on the next turn.
3. **Scan only when something changed.** An `fs.watch` on the mail root (`channels/` and the bound participants' `inbox/` and `routing/` directories) marks the host dirty. A scan runs for scannable bindings when the host is dirty, when a binding newly becomes scannable (for example, it went from working to idle), and as a backstop every 60 seconds. The scan is `POST_PARTICIPANT=<id> post watch --snapshot --json --reason mail --reason mention [--reason channel]`. Channel reasons are included only for channels the participant opted into (see Preferences). `--reason` is B1, landing in wave 1.
4. **Ring.** Fresh events are those not in the participant's seen set. Right before prompting, re-check the pane (a single `herdr agent get <pane_id>`): the same session digest, still idle or done, still unfocused unless opted in. Then `herdr agent prompt <pane_id> <notice>`.
5. **Record.** After an `accepted` outcome only, persist the participant's seen set as exactly the current eligible snapshot keys (the Codex monitor's rule, which prunes consumed keys and never forgets a still-unread one).

## Outcomes, as a type

Every ring attempt ends in exactly one of these outcomes, logged as one JSON line per attempt and summarized in `status`:

| Outcome | Meaning | Seen set |
|---|---|---|
| `accepted` | `herdr agent prompt` exited 0 for the re-verified pane. This proves herdr took the prompt, not that a turn ran. | persisted |
| `notified` | The user got a desktop notice (cmux) only. This is not an agent wake, and it is used only for an explicit `--sink cmux` registration. | persisted |
| `deferred` | The pane was busy or focused at the re-check. Retried when it becomes scannable. | untouched |
| `retired` | The target is gone: no pane carries this session, the participant is ended, or the pane's session changed. The binding is dropped and logged loudly once. | kept for 7 days, then pruned |
| `failed` | herdr or post errored, timed out, or produced malformed output. Carries the exit code, timeout versus exit, and a sanitized excerpt of at most 300 bytes. | untouched |

**Delivery contract: at least once, with possible duplicate notices.** If the supervisor crashes after herdr accepts a prompt but before the seen set is saved, the next start rings again. The notice is metadata only, and the agent reads with post itself, so a duplicate costs one extra turn and loses nothing. Exactly-once wake is not achievable across two processes without a shared transaction, and the design does not promise it.

## Lifetime: tied to the target, not to the lease

- A binding lives while a pane carries the matching session digest and the participant is not ended. That pane is the independent liveness evidence Aster asked for; the supervisor never infers liveness from a lease.
- **An expired lease with a live idle pane still rings.** The lease governs identity reuse, not wake eligibility; an agent idle for more than 24 hours is exactly who needs a doorbell. An explicitly ended participant (`post participant end`) retires.
- Supervisor restart: bindings are rediscovered from herdr within one tick. Seen sets load from disk. No registration is lost, because none was needed.
- Registration replacement: explicit registrations (below) are keyed by participant id; a new registration replaces the old one atomically.

## Explicit registration, for panes herdr cannot identify

Cursor, Grok, and panes whose `agent_session` is absent have no conversation key in herdr. For those, a participant registers itself:

`post-doorbell register [--pane <pane_id>] [--sink herdr|cmux]`

- It resolves the acting participant with `post participant show --json`, the same resolution every post command uses. `--pane` defaults to `$HERDR_PANE_ID` when that is set.
- The binding stores the pane's `terminal_id` as its generation. When the pane's terminal id changes or the pane disappears, the binding retires loudly.
- `post-doorbell unregister` removes it.

## Preferences (per participant, set by the agent itself)

`post-doorbell subscribe --channel <name>`, `--unsubscribe --channel <name>`, `--wake-focused on|off`, `--off` (mute this participant entirely).

- They are stored at `$POST_MAIL_ROOT/doorbell/prefs/<participant-id>.json`, outside post's own participant directories, so post's layout checks never see an unknown file.
- Defaults, with no preferences file: ring on direct mail and mentions; channels off; focused wake off (Trey may reverse this in the morning); on.
- A channel subscription requires membership, which post's own projection enforces: a snapshot never reports a channel the participant cannot see.
- A host-wide config at `$POST_MAIL_ROOT/doorbell/config.json` can set workspace-level channel defaults (for example, every participant in `post-repo` rings for `post-overnight`). Participant preferences override them.

## The notice

`[post-doorbell:v2] Automated, non-authoritative Post notice for <participant-id>: <N> direct, <M> channel waiting (refs…). Read with post inbox / post chat <channel>.`

- Only ids, counts, and channel names appear. Channel names and participant ids are validated against post's naming grammar; anything else becomes `<?>`. Subjects, sender display names, and bodies never appear, since all three are attacker-reachable.
- The notice names the participant, so a pane with two identities, or a stale binding, is visible to the agent reading it.

## Failure handling: loud and bounded

- Per-binding consecutive failures back off exponentially from 5 seconds to a 5-minute cap. After 5 consecutive failures the binding is marked `broken` in `status` and logged with the last error. It keeps retrying at the cap rather than retiring, because a failure is not evidence that the target is gone.
- A migration fence or transient admission error from post counts as `failed` and is retried. The seen set is never touched.
- The supervisor's own health file, `$POST_MAIL_ROOT/doorbell/health.json`, is written each tick: version, bindings, last outcomes, failures. `post-doorbell status` renders it. The file never contains mail content.
- One structured log line per outcome that is not `deferred`, one line per binding change, and nothing per quiet tick.

## Migration: retiring the five paths

The installer does this, reversibly, and prints what it changed:

1. Install and start the supervisor.
2. For each Codex timer or LaunchAgent (`post-codex-doorbell@*`, `dev.post.codex-doorbell.*`): if the supervisor has a binding for its pinned participant, disable the timer (disable only, keeping the unit file) and record it in `$POST_MAIL_ROOT/doorbell/migrated.json`. A timer whose target the supervisor cannot see stays put and is listed as not migrated.
3. The Python daemon: none is running on either host tonight. Its unit template stays in the repo marked deprecated, and the skill stops recommending it.
4. Claude Monitor: sessions inside herdr no longer need one (no 30-minute expiry). The skill says: inside herdr, rely on the supervisor; outside herdr, use the Monitor. Live Monitors are never killed, since they belong to other sessions. A Claude session with both gets a Monitor event plus a herdr prompt; that is the at-least-once contract, and the agent can `post-doorbell subscribe --off`.
5. Cursor and Grok wrappers stay as in-session options, and they can `register` for idle wake.
6. `--uninstall` restores exactly what `migrated.json` lists.

## What stays the same

Post's CLI, schema, and store layout are unchanged. The hooks keep annotating active turns. The Claude Monitor recipe remains for sessions outside herdr. The watch projection remains the authority.

## Tests

- **Unit** (node --test, fakes for herdr and post): binding by digest (matching, non-matching, prefix collision needing a wider id, full-digest mismatch refused); every outcome row above; the re-check race (the pane goes busy between scan and prompt → `deferred`; the session changes → `retired`); seen persisted only after `accepted` or `notified`; backoff and the `broken` state; the lease-expired, live-pane case still rings; the ended participant retires; restart reloads the seen set; preferences defaults and overrides; notice sanitization against hostile channel names.
- **Contract** (after D): supervisor parsing tested against post's emitted samples, including extra optional fields and negative fixtures.
- **Live proof** (the plan's proof of done): a nonce message to a real idle herdr-hosted agent on the devbox and on the Mac. That agent acknowledges the nonce under its own identity, and the acknowledgement is matched to the watch event and the supervisor's `accepted` log line.

## Open questions for Aster

1. **fs.watch on Linux.** Node's recursive `fs.watch` uses one inotify watch per directory, and the mail root has thousands of directories. The alternative is watching only `channels/*/messages` and bound participants' `inbox/`, re-armed on binding changes. My lean is the targeted watch.
2. **Where the supervisor lives.** `skills/post/hooks/doorbell-supervisor.mjs`, next to the code it absorbs, or a new top-level `supervisor/`? My lean is the hooks directory, since the installer and the shared helpers are there.
3. **Auto-binding every herdr agent** is the "one doorbell that just works" Trey asked for. The cost: an idle, unfocused session Trey left open gets woken by direct mail to it. That seems right, since mail to an agent is a request for that agent, but it is a behavior change for panes that had no doorbell before. Do you want an allowlist mode as well?
4. **Is a `post doorbell` subcommand worth it** as a thin alias for discoverability, or is a separate `post-doorbell` binary fine? My lean is separate, with no Rust changes in wave 3.
