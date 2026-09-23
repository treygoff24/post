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
- **`lineage:` and `participant:` mail stays home.** Those addresses are
  host-local. The bridge never relays them: v2 skips them on the way out and
  quarantines any that arrive (`unsupported_address_kind`).
- **Channels stay home** unless `bridge/config.json` has a `channels` key:
  `{"mode":"all","deny":[...]}` or `{"mode":"allow","allow":[...]}`. Without
  the key, a host neither publishes nor imports channels. The planned v2
  allowlist on the Mac and the trey cell is `loom-build` only; the Mac's
  separate cell bridge syncs a few other channels over ssh. Only v2 relays
  channels; v1 has no channel support.
- **Channel wake files (v2).** Each imported channel message also writes one
  `bridge/events/<channel>/<id>.json` holding `host`, `channel`, `id`,
  `from`, `event`, and `mentions`, never subject or body. It is a hook point
  for waking an agent that is not running. Running sessions don't need it:
  their `post watch` already rings on the import. The bridge never deletes
  these files, so a consumer deletes what it has handled.
- **Replies.** Remote mail carries `reply_to_shared` (the sender's workspace)
  and no `reply_to_participant`. A remote sender is never treated as a local
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

## Name collisions: send from a unique workspace

The destination checks the sender's workspace name (`from`) against its own
rooms. When that name is also a real room on the destination, the mail is
quarantined and never delivered: v1 records `forged_from`;
v2 marks the name contested and records `name_collision`. The sender's own
send still reported `ok`, so nothing on the sending side shows the loss.
Several workspace names exist on both the Mac and the trey cell today, so a
send from one of those workspaces to the other host is lost this way.

The fix is a workspace name that exists only on your host:

```bash
post rooms add <unique-name> <dir>          # a directory no other room owns
post participant bind --workspace <unique-name>
```

Your sends then carry the new name as `from`, and replies come back to it. Two
conditions remain before replies can route back: under v1 the destination
needs the new name in its `peers` config; under v2 the destination learns it
from your host's published room list on the next tick, and quarantines mail
from a name it has not yet seen published (`unpublished_sender`), so wait one
tick before the first send. Under v2, mail addressed to a contested name
does not route anywhere until one side renames.

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
remedy. `post` has no `rooms remove`: renaming a room means registering a
new name, as above.

A tick can be killed anywhere; the next one reconciles from disk.
