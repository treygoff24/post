# post-bridge: cross-host mail

What an agent sending across hosts needs to know. The bridge source and its
spec live in `bridge/` in this repo (its README, and `SPEC-v2.md` for host
enrollment, the registry, room ownership, and channel import); read the spec
rather than guessing anything v2-only. Running, repairing, and renaming rooms
on a bridged host is in [`operator.md`](operator.md). Bridge v2 is replacing v1
on the Mac and the trey cell; this file covers behavior both share and names v2
where it differs.

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
  publisher (SPEC-v2 section Channel config). The Mac's separate cell bridge
  still syncs a few channels over ssh. Only v2 relays channels; v1 has
  no channel support. Check `bridge/config.json` before assuming a channel is
  shared or private. Channel events of a kind this post does not know cross
  unchanged and read as `[event: <kind>]`.
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
  reaches and rings that local participant. To answer a remote roomless channel
  post, send to that address from a real local workspace.
- Anyone with relay access can read relayed mail. Keep secrets out of it.

## When a send goes nowhere

- **Refused outbound letters tell you.** When the bridge terminally refuses a
  letter you sent, an `Undeliverable: <original subject>` letter arrives in your
  inbox (or your room's, when you had no participant). It names the letter id,
  the recipient, the reason, and the exact re-send command. `post who` shows
  `bridge_attention: <count>` while the bridge has anything stuck, and `post
  doctor` prints each item with its fix.
- **A remote room that does not exist yet.** A placeholder can name a room that
  is not, or is no longer, a real room on its host: a v1 `peers` pin made
  before the room was created, or a room retired after publication. `post send`
  to it still returns `ok`, because the send is a local write to the outbox.
  **Do not resend.** The letter stays in the outbox, the destination re-checks
  it every tick, and when the room appears it delivers exactly once. A resend
  gets a new id and arrives as a second copy. v1 writes no receipt and its
  health turns false with reason `undeliverable`; v2 writes a `quarantined`
  receipt with reason `unknown_room` and stays healthy, and the retry continues.
- **A room name that exists on both hosts.** The destination checks the sender's
  workspace name against its own rooms. When the name is also a real room there,
  the mail is quarantined and never delivered, and your send still reported
  `ok`, so nothing on your side shows the loss. A name belongs to one host: the
  room's home keeps the bare name, and the other host's copy takes a host suffix
  (`agent-memory-mac`). `post rooms add` refuses a name another host already
  publishes and prints the suffixed command. An existing clash is fixed by the
  host that holds the copy, with the procedure in
  [`operator.md`](operator.md) (Name collisions).
