# Lane A diagnosis: watch, doorbells, cursors

The cuts do not reduce to one defect. `post watch` is the common event source, but current delivery is split across native watch, the Python Herdr daemon, Claude Monitor, Codex timer adapter, and Cursor/Grok hook wrappers; each applies its own event validation, lifetime, selection, and delivery policy. Two shared code seams stand out: repeated full-store projections make wake cost grow with history, and the watch event schema is revalidated independently by consumers (one consumer rejects typed-address mail). Current main already mitigates cursor-corruption ambiguity and consumed-channel parsing; those symptoms are not silent in this checkout. The devbox Codex timer is active but its snapshot command fails repeatedly, so no delivered wake is evidenced.

## Per item

### `pc2_dc0e81eee6c776c3` — watch CPU scales with backlog
- **Symptom:** A long-running `post watch` accumulated high CPU as channel backlog and write activity grew.
- **Root cause:** **VERIFIED** scan work still scales with stored files. Every participant watch scan calls `unread_mail_snapshot` (`src/commands/watch.rs:1273-1274`; `src/eligibility.rs:50-60`), whose `visible_mail_snapshot` lists, reads receipts for, hashes, and parses every mail file before filtering consumed ids (`src/eligibility.rs:77-115,129-163`). Event wakes also enumerate all channel message filenames (`src/eligibility.rs:268-304,283-290`), even though they now skip parsing consumed bodies. The loop debounces queued notify events (`src/commands/watch.rs:993-995`) and uses cheap channel wake projections, so the old “busy spin / parse every channel body” theory is stale; complete scans still run at reconciliation (`src/commands/watch.rs:743-760`).
- **Fix class:** refactor.
- **Direction:** Keep a per-target index of eligible/unseen ids and update it from file events; bound/coalesce rescans, with periodic complete reconciliation retained for overflow, corruption, and missed events. First measure participant mail scan cost separately from directory enumeration.
- **Effort / risk:** L. Risk: an incremental index can miss rename, replacement, overflow, or cross-host writes; retain a complete correctness pass and test those paths before trusting it.

### `pc2_c10d1dc544b232b4` + `pc2_a2535b25ca4f1200` — cursors become unusable / backlog looks new
- **Symptom:** A cursor read failure resets the projection to “nothing seen,” so old eligible messages are emitted again.
- **Root cause:** **VERIFIED** `ParticipantCursors::load` maps unusable state to an empty cursor (`src/cursor_state.rs:32-64`); reads intentionally do not acquire `.cursors.lock` (`src/cursor_state.rs:222-260`), while writers replace the file atomically under that lock (`src/cursor_state.rs:327-359`). Metadata validation and content read are separate pathname operations (`src/cursor_state.rs:235-259`). **INFERRED trigger:** a transient metadata/read error, or a path replacement between those operations, best fits “valid on later inspection”; atomic temp+rename writes rule out the sweep’s write-in-place/partial-JSON theory for Post’s normal writer. A concurrent external in-place edit, unsafe symlink/hard-link replacement, or actual malformed/version-mismatched file remain possible; the specific historical trigger is not established.
- **Current status:** The original silent/ambiguous report is mitigated in current main: transient failures retry once (`src/cursor_state.rs:222-232`), warnings name path and reason (`src/cursor_state.rs:40-61`), and watch events/digests mark re-reported history (`src/commands/watch.rs:199-207,1267-1272,1406-1409,1888-1930`). An in-code test covers the marker (`src/commands/watch.rs:2101-2175`); this diagnosis did not run it or reproduce the old trigger.
- **Fix class:** behavior-change (for any further change); existing mitigation is already present.
- **Direction:** Keep fail-open but preserve typed failure reason through the scan and require operators/adapters to surface the marker. For a repeat, add safe file-descriptor-based validation/retry diagnostics and record the observed errno/metadata transition; do not rebuild/reset valid cursor state automatically.
- **Effort / risk:** S for better diagnostics, M for race-proof descriptor handling. Risk: changing fail-open to fail-closed can hide new mail; automatic repair can destroy read history.

### `pc2_0decc6f33fc0ebe4` — Python doorbell rejects participant-addressed mail
- **Symptom:** `post-doorbell` ignores participant-addressed mail events because the event has no `room`.
- **Root cause:** **VERIFIED** Rust deliberately omits `room` for typed addresses (`src/output.rs:907-910,922-930`; `src/commands/watch.rs:354-356`); `event_metadata` nevertheless requires `room` on every non-`channel_message` event (`doorbell/post-doorbell:122-143`, especially 127-131). It then uses `room` as the mail namespace. No participant-addressed mail can pass this validation.
- **Fix class:** band-aid.
- **Direction:** Derive/validate namespace from `address.kind/name` when `room` is absent; accept a legacy workspace `room` only when consistent with the address. Add participant/lineage mail cases to the consumer’s parser contract.
- **Effort / risk:** S. Risk: wrong dedupe namespace can collapse distinct address streams or make old and new producers double-ring; retain typed kind in the key.

### `pc2_aaf4e93c82e85591` — timer skips a focused Herdr pane
- **Symptom:** A valid timer tick does not prompt its named pane while that pane is focused.
- **Root cause:** **VERIFIED** the shared Codex monitor sends a Herdr prompt only for `idle`/`done` and `!focused` (`skills/post/hooks/codex-notify-monitor.mjs:380-402`); there is no installed-unit override for focused idle delivery. It intentionally avoids interrupting the pane.
- **Fix class:** behavior-change.
- **Direction:** Add an explicit opt-in `--wake-focused`/config policy passed through the installer, retaining the default focus guard and metadata-only notice. Do not infer that a focused pane is safe to interrupt.
- **Effort / risk:** S. Risk: an opted-in doorbell steals attention during active human use; installer/docs must make scope explicit.

### `pc2_709bd06fa9d0e14a` — installed devbox Codex timer has no observed delivery
- **Symptom:** The Astra timer runs, but no wake is recorded.
- **Root cause:** **VERIFIED** read-only systemd state shows `post-codex-doorbell@astra.timer` active/waiting, scheduled every five seconds; the oneshot service completes, while `astra.json` remains `{"seen":[]}` and logs repeatedly say `post-notify: snapshot failed; notification state is unknown`. The unit pins `POST_PARTICIPANT=codex-f20d25ab` and `jev-experiments`. The adapter collapses either a `post watch --snapshot` timeout or nonzero exit into that generic failure and discards stderr (`skills/post/hooks/codex-notify-monitor.mjs:303-313`), so the exact underlying CLI failure is **INFERRED / unresolved** from allowed evidence. This item uses the JS Codex monitor, not Python `event_metadata`; do not assume item `pc2_0de...` explains it. No live mail was sent to test delivery.
- **Fix class:** band-aid for observability; root cause requires one safe snapshot probe after selecting an isolated/non-sensitive test setup.
- **Direction:** Preserve post exit status, timeout-vs-exit, and a bounded stderr diagnostic in the service log; then validate a real idle delivery before calling it repaired. Keep event payloads and secrets out of logs.
- **Effort / risk:** S. Risk: logging raw stderr may expose local paths or event content; log only bounded, sanitized error detail.

### `pc2_d74a02555a639a2a` — Claude Monitor watch stops after harness expiry
- **Symptom:** Claude stops receiving idle wake notices after its Monitor-owned watch is reaped.
- **Root cause:** **VERIFIED** the documented design assigns process lifetime to Monitor and explicitly says Post cannot extend it; expiry is silent (`skills/post/references/post-mail-doorbell.md:46-60,71-79`). Activity-gated hooks only surface unread state on the next turn; they cannot wake an idle session (`skills/post/SKILL.md:162-175`; `skills/post/references/watch.md:97-125`). The reported 30-minute value is **INFERRED from the cut**, not independently established by repo code/docs, which deliberately call the cap unknown.
- **Fix class:** behavior-change (for a durable Claude idle wake); docs-only clarification is band-aid.
- **Direction:** Use a session-external supervisor with an explicit Claude-compatible wake sink if the harness exposes one; otherwise document Monitor as best-effort and keep the next-turn hook as recovery. Do not present a guessed lifetime as a Post guarantee.
- **Effort / risk:** M. Risk: duplicate active Monitors or external prompts can double-ring or wake the wrong session; bind one watcher to a stable session identity and make ownership/liveness observable.

### `pc2_d91fc6b1d3f10f13` — watch lacks mention/reason filter
- **Symptom:** Busy-channel doorbells emit every message and callers add their own `jq` filter.
- **Root cause:** **VERIFIED** `WatchArgs` has no reason selector (`src/cli.rs:822-872`); `WatchEvent` already assigns `mail`, `channel`, and `mention` (`src/output.rs:973-975,1131-1139`) and `watch` emits each eligible delivery without filtering (`src/commands/watch.rs:1888-1933`).
- **Fix class:** new-feature.
- **Direction:** Add repeatable `--reason mail|channel|mention` selection at the final delivery boundary, preserving unfiltered default, with digest reason filtering defined over group members before digesting.
- **Effort / risk:** S. Risk: filtering before the consumer sees unreadable events can hide uncertainty; unreadable channel events have only `channel`, never `mention`, so document that semantic limit.

## Shared-root-cause clusters

1. **Repeated full projections are the CPU multiplier** (`src/commands/watch.rs:717-760,1273-1274`; `src/eligibility.rs:77-115,268-304`). `post watch` is the common source, but channel body parsing is already skipped for consumed ids on event wakes. Remaining linear work is file enumeration plus participant mail receipt/digest/parse across the whole inbox on each relevant wake, with complete validation on a slow cadence. One indexed/incremental projection with a correctness reconciliation pass is the deeper fix for `pc2_dc0...`; changing poll interval or only adding a rescan floor cannot make large inbox snapshots constant-cost.

2. **One event schema, several independently coded consumers** (`src/output.rs:904-979`; `doorbell/post-doorbell:118-143`; `skills/post/hooks/codex-notify-monitor.mjs:149-199`; `skills/post/hooks/watch-notice.mjs:1-16`; `skills/post/hooks/claude-mail.mjs:22-29`). The `room` omission for participant/lineage addresses is intentional and documented, but the Python daemon assumes `room` for mail. Cursor-unusable markers also need a policy in every consumer: whether to ring a backlog-sized event, display degraded state, or leave the trigger pending. A single contract library/spec with shared positive/negative fixtures can retire compatibility cuts like `pc2_0de...` and prevent future drift; delivery itself should remain a thin harness-specific sink.

3. **Wake policy and process ownership are fragmented** (`skills/post/references/watch.md:97-149`; `skills/post/references/post-mail-doorbell.md:46-85`; `skills/post/hooks/codex-notify-monitor.mjs:380-402`; `doorbell/post-doorbell:278-294,380-435`). Activity hooks only annotate an active turn; Claude Monitor owns an expiring process; Codex uses a systemd timer/oneshot; Cursor/Grok rely on background task/monitor completion; the Python daemon owns a continuous watch and Herdr prompt. Focus gating and mention selection are consumer-local. A consolidated direction is one durable per-agent supervisor per host: consume canonical filtered watch events, keep cursor/schema validation and health/lifetime policy in one maintained component, and plug in a small sink per harness. Preserve explicit focus policy and fail-visible delivery; retain hooks as next-turn catch-up, not as idle wake. This is an architectural direction, not a claim that every harness exposes an interchangeable wake API.

## Recommended order and scope

1. **Repair observability and event compatibility first:** log why the devbox snapshot failed, fix typed-address parsing in the Python daemon, and verify end-to-end delivery with a controlled participant-address event. These are narrow and unlock trustworthy diagnosis.
2. **Settle wake policy:** document Claude Monitor as expiring/best-effort unless a supported durable Claude sink exists; add an opt-in focused-idle policy; add watch reason filtering. Keep per-harness sinks where necessary.
3. **Measure then reduce scan complexity:** instrument scan counts/time by projection (mail vs channels); introduce incremental file/id indexes and event coalescing while retaining periodic complete validation. This is the largest change and carries missed-event risk.
4. **Cursor reports:** preserve current warning/re-report marker. Reproduce the transient before changing persistence semantics; improve safe descriptor-based diagnostics if it recurs.

**Do not** claim the Astra timer is repaired from “active” or successful oneshot status: its logs show failed snapshots and no delivery. **Do not** change fail-open cursor behavior to silently fail closed or auto-reset cursor state; either can hide mail or erase read history. **Do not** remove the focused-pane guard globally: a wake can interrupt a user; make bypass explicit. **Do not** promise a 30-minute Claude limit from this repo: the cut reports it, while current docs say the Monitor cap is unknown and outside Post’s control. **Do not** merge all adapters into a universal injector; preserve harness-specific trust and wake APIs behind a shared event contract.

## Evidence boundary

Reviewed current source at `92de29aca9a300b166aec6fb0eb4502d31709739` and read the supplied sweep report as a lead. Read devbox systemd unit/timer state and logs without changing them; the Astra timer is active, the service repeatedly records snapshot failures, and no delivered event is recorded. No tests were run, no live mailbox snapshot or send was performed, and the historical cursor trigger plus exact cause of the Codex snapshot failure remain unverified.
