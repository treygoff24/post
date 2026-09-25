# post-bridge: cross-host mail

What an agent sending across hosts needs, then what an operator needs. The
bridge source and full contract live in `post-bridge/` in the claude-space
repo: `README.md` for v1 and, on the v2 branch, `SPEC-v2.md`. Bridge v2 is
replacing v1 on the Mac and the trey cell; this file covers behavior both
share and names v2 where it differs. For anything v2-only (host enrollment,
the registry, room ownership, channel import), read `SPEC-v2.md` rather than
guessing.

## What crosses

- **Workspace mail crosses.** A tick on each host publishes outbound mail
  addressed to a remote room and delivers inbound mail to local rooms.
  Routing to participants happens after the mail reaches the destination
  host. A remote room appears locally as a placeholder: `post rooms --json`
  lists it under its own name with a path under
  `$POST_MAIL_ROOT/remote/<host>/`.
- **Host-qualified participant letters cross** (`participant:<id>@<host>`)
  on a bridge that advertises `participant-mail-v1` in `bridge/health.json`.
  The sender's letter waits in `archive/`; the bridge publishes it, the
  destination bridge imports it through `post bridge deliver`, and a receipt
  comes back. The sender's evidence lives under `bridge/pmail-*`, which
  `post delivery <mail-id>` reads. Details: `SPEC-v2.md` (participant mail).
- **`lineage:` mail and bare `participant:` mail stay home.** v2 skips them
  on the way out and quarantines any that arrive
  (`unsupported_address_kind`).
- **Channels cross by default (v2, r6.2; Trey ruling 2026-09-24).** With no
  `channels` key in `bridge/config.json`, a host publishes and imports
  every channel (`{"mode":"all"}`). `{"mode":"allow","allow":[...]}`
  restricts to a list, `deny` excludes names under `mode:"all"`, and an
  explicit `"channels": null` turns channel sync off entirely. A denied
  name is bridged in neither direction, and deny is only real at the
  publisher (SPEC-v2 §Channel config). The Mac's separate cell bridge
  still syncs a few channels over ssh. Only v2 relays channels; v1 has
  no channel support.
- **Roomless channel senders cross with r6.3.** The publishing bridge stamps
  `from_host` into the relayed copy. The receiving bridge verifies that it
  matches the branch host and refuses import if the sender id already belongs
  to a local participant (`participant_id_collision` in channel quarantine).
  Post shows `<id>@<host>` and both reply fields as
  `participant:<id>@<host>`. A channel send succeeds locally. Its receipt says
  `queued` when eligible for a future tick, `local_only` for a lasting relay
  block, or `unconfirmed` when bridge health cannot confirm relay yet or its
  fresh capability list shows a pre-roomless bridge. That older bridge's
  queued roomless posts backfill after upgrade. The latter two states carry a
  reason in JSON and stderr; do not resend an `unconfirmed` post, and check
  `post doctor` or the bridge.
- **Channel wake files (v2).** Each imported channel message also writes one
  `bridge/events/<channel>/<id>.json` holding `host`, `channel`, `id`,
  `from`, `event`, and `mentions`, never subject or body. It is a hook point
  for waking an agent that is not running. Running sessions don't need it:
  their `post watch` already rings on the import. The bridge never deletes
  these files, so a consumer deletes what it has handled.
- **Replies.** Remote workspace mail carries `reply_to_shared` (the sender's
  workspace) and no `reply_to_participant`; an imported participant letter's
  `reply_to_participant` is `participant:<sender>@<host>`, taken from its
  admission record. A remote sender is never treated as a local
  participant, even when its participant id matches one, so the message still
  reaches and rings that local participant.
- Anyone with relay access can read relayed mail. Keep secrets out of it.

## A remote room that does not exist yet

A placeholder can name a room that is not, or is no longer, a real room on its
host: a v1 `peers` pin made before the room was created, or a room retired
after publication. `post send` to it still returns `ok`, because the send is a
local write to the outbox. **Do not resend.** The letter stays in the outbox,
the destination re-checks it every tick, and when the room appears it delivers
exactly once. A resend gets a new id and arrives as a second copy.

The two versions differ in what the destination records meanwhile:

- **v1** writes no receipt. It logs `undeliverable` with the letter's age each
  tick, and its health turns false with reason `undeliverable` until the
  letter delivers.
- **v2** writes a `quarantined` receipt with reason `unknown_room` and stays
  healthy. The receipt does not prune the letter from the sender's outbox, so
  the retry continues.

## Name collisions: one host renames its copy

The destination checks the sender's workspace name (`from`) against its own
rooms. When that name is also a real room on the destination, the mail is
quarantined and never delivered: v1 records `forged_from`;
v2 marks the name contested and records `name_collision`. The sender's own
send still reported `ok`, so nothing on the sending side shows the loss.
Under v2, mail addressed to a contested name does not route anywhere, and
health reads `room_name_collision`, until one side renames.

A name belongs to one host: the room's home keeps the bare name, and the
other host's copy takes a host suffix (`agent-memory-mac`). `post rooms add`
refuses a name another host already publishes and prints the suffixed
command. To fix a clash that already exists, rename the copy:

```bash
post rooms rename <old> <old>-<host> --dry-run --json   # every check, no writes
post rooms rename <old> <old>-<host> --json
```

The rename moves `<root>/<old>` and rewrites the live state that names it.
On a bridged host it refuses (`bridge_guard_unavailable`, retryable) unless
`bridge/health.json` ticked within three intervals, its `local_held` faults
and unaccounted candidates are both 0, and every archive letter the bridge
would export for `<old>` has a `bridge/local-held/<id>.json` hold. It also
refuses while `owner.json` names `<old>`: pin the owner's principal,
namespace, and label and point `room` at the new name first. Around it:

- **Pause the bridge.** Its import does not take post's rename lock. Stop the
  timer (`systemctl --user stop post-bridge.timer`, or `launchctl bootout` the
  Mac job) right after a tick, rename inside the freshness window, then start
  it again.
- **Release the old name in the bridge.** Each bridge records a name's owner
  in `bridge/rooms/owners.json` and keeps claiming a name its own host
  vacated, so a peer cannot take over a name that briefly disappears. After
  the rename, health still reads `room_name_collision` until the host that
  renamed deletes the `<old>` entry from its own `owners.json`. With the
  bridge paused, delete that one entry and resume. Leave `owners.last.json`
  alone; the bridge compares the two files and logs `owner_released`. Within
  two ticks the home host is the only claimant and health reads `ok`.
- **Re-arm watchers** armed on the old name, and update any config outside
  post's store that names it (Porch `owner_room`, doorbell configs, CLAUDE.md
  files). The receipt's `warnings` name these. A session that was already
  running keeps the `POST_FROM` pin it resolved at launch, so it sends and
  reads as `<old>` until it restarts. New launches pick up the new name.
- **After a crash**, `post doctor` reports `rooms.rename_interrupted`; rerun
  the same pair to resume from `$POST_MAIL_ROOT/rename-journal.json`.

The destination learns the new name from your host's published room list on
the next tick and quarantines mail from a name it has not yet seen published
(`unpublished_sender`), so wait one tick before the first send. Under v1, the
destination needs the new name in its `peers` config instead.

## Operating the relay

Each host runs one tick per timer interval: fetch every peer's branch of
`estate/post-relay`, deliver inbound mail, publish outbound mail, push, and
write health.

| What | Where |
| --- | --- |
| Action log, one JSON line per action, no bodies | `$POST_MAIL_ROOT/bridge/log.jsonl` (rotates at 10 MiB) |
| Health | `bridge/health.json` |
| Topology | `bridge/config.json`: `host`, `relay_url`, `peers` (v1 pins peer hosts and rooms; optional in v2), optional `channels` |
| Rejected input, kept for forensics | `bridge/quarantine/` |

On Linux, `journalctl --user -u post-bridge.service -n 50` shows the last
ticks. `install.sh --help` and the README cover installation.

`bridge/health.json` has `ok` and `reason`. Reasons both versions share:

- `fetch_stale`: no successful fetch within the grace period (600 s by
  default). Check the relay key, the forge, and the log.
- `fenced`: `.post-arx.json` exists in the mail root, an archive in progress.
  The tick changes nothing until it is gone.
- `busy`, `deadline`, `internal_error`: transient unless repeated;
  `internal_error` puts the exception class in the log. `config_error` exits
  2: the config or environment is wrong, and every tick fails the same way
  until it is fixed.

v1 adds `undeliverable`: inbound mail addressed to a room that is not real
here (see above). It clears once the room appears and the letter delivers.
v2 has no `undeliverable`.

A quarantined message does not make health false. Health counts it under
`quarantined`; the log and `bridge/quarantine/` hold the details. Under v2,
health also reports `rooms.collisions`,
`channels.diverged`, and `channels.rewritten`; `SPEC-v2.md` gives each one's
remedy. A contested name is fixed with `post rooms rename`, as above.

A tick can be killed anywhere; the next one reconciles from disk.
