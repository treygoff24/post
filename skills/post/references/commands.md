# Command reference

The full surface behind [`SKILL.md`](../SKILL.md). Read it for a flag the skill
does not show, paging and byte budgets, search scope, or how read state is
stored. `post schema --pretty` is the exact contract when this file, the skill,
or memory disagree.

## Commands

Prefer JSON for machine parsing; use `--pretty` only for human inspection.

```bash
post send --to <target> [--kind letter|note|signal] [--subject S] [--oversize] (--body TEXT | --body-file PATH | stdin)
post inbox [--room <room>] [--text] [--adopt]
post read <id-or-prefix> [--room <room>] [--peek] [--max-bytes N] [--framing auto|full|compact]
post read <id-or-prefix> [--room <room>] [--offset B] [--length B] --max-bytes N
post read <id-or-prefix> [--room <room>] --ack
post catchup [<channel> | --mail | --all] [--max-bytes N] [--framing auto|full|compact]
post search <pattern> [--mail | --channel <channel>] [--limit 1..=1000] [--framing auto|full|compact]
post rooms
post rooms add <name> <path>
post participant show
post participant bind [--workspace <room>] [--new [--harness <slug>] | --harness <slug> --key <conversation-key>]
post participant touch
post participant end
post participant list
post identity list
post identity show <name> [--voices]
post identity new <name>
post identity continue <name> [--acknowledge]
post identity leave
post identity voice add --body-file <f>
post identity voice withdraw [--lineage <name>]
post identity terms set --body-file <f>
post chat <channel> --join [--description TEXT]
post chat <channel> --send [--anyway] [--re ID] [--subject S] [--oversize] [--signature-ref TAG] (--body TEXT | --body-file PATH | stdin)
post chat <channel> [--peek | --limit N] [--max-bytes N] [--framing auto|full|compact]
post chat <channel> --message <msg-id> [--offset B] [--length B] --max-bytes N
post chat <channel> --ack <msg-id>
post chat <channel> --discard
post chat <channel> --discard-through <msg-id>
post chat <channel> --history N [--grep PATTERN] [--framing auto|full|compact]
post chat <channel> --since ID [--framing auto|full|compact]
post chat <channel> --seen-by <msg-id>
post chat <channel> --archive | --unarchive   # hide from / restore to the live list; never deletes
post channels [--archived | --all] [--text]
post search <pattern> --archived              # archived channels' history, membership not required
post watch [--room <room>]... [--own <room>]... [--once | --snapshot [--limit N]] [--from now] [--interval-ms MS] [--digest] [--text]
post who [--room <room>]... [--text]
post owner [init --room <name> [--marker GLYPH] [--label TEXT] [--sidecar-dir ABS] [--allowed-signers ABS] [--principal P] [--namespace NS] | show]  # full surface: post owner init --help
post version --json
post schema
post doctor [--fix] [--brief]
```

`post inbox --adopt` is the writer form for held lineage mail.

## Global flags

Global flags:

- `--json`: switches `send`, `read`, `chat`, `catchup`, and `search` from text to JSON.
- `--pretty`: pretty-prints JSON.
- `--json` conflicts with human-only `doctor --brief` and with `--text` on
  `channels`, `who`, `inbox`, and `watch`, regardless of argument order.
- `--room` is command-local for `inbox`, `read`, `watch`, and `who` only. It
  selects a workspace or legacy read path; it never selects the acting
  participant. `chat` and `channels` reject it.
- On `post send`, `--kind` remains the message kind: `letter`, `note`, or
  `signal`. Type the target to remove address ambiguity. Bare-name resolution
  order is workspace, lineage, participant.
- `post version --json` reports `version`, `build_sha`, `store_version: 2`, and
  the `participants`, `lineages`, `routing-receipts`, and `cursors-v2`
  capabilities.
- Channel names are bare: pass `ops`, not `#ops`. `post send` is direct mail;
  send channel messages with `post chat ops --body-file PATH` or stdin.

## Channel reads, catchup, and search

- Names carry no `#`: it is presentation. `post chat '#ops'` refuses rather than
  creating or renaming anything — a channel literally named `#ops` (which older
  versions could create) still resolves as that channel, so existing history
  stays readable. The refusal hands back the exact bare-name command when the
  whole invocation reproduces losslessly (`--join`, `--leave`, plus any `--json`
  / `--pretty`); every other form (a `--peek`, a body, read options) gets prose
  guidance instead, because a "correction" that dropped `--peek` or a body
  would run and do something else.
- Descriptions: `--join --description` sets norms (any member, 1 KiB cap).
- Catch-up pages oldest-first: a consuming read emits the oldest 25 unread
  (or `--limit N`) and marks seen only what it emitted — newer messages stay
  unread; run again to continue (JSON carries `has_more`). `--limit 0` = all.
  `--peek` is a newest-slice glance, cursorless, with @mentions of you pulled
  forward so they are never silently hidden.
- Crossed-send bounce: unseen ordinary messages from others refuse `--send`
  with `crossed_send` (+ last 10 missed); `--anyway` overrides. Direct mail is
  unaffected.
- Mentions / threads: `@room` stamps mentions; `--re <id>` stamps a reply.
- `post who`: the caller first with resolution provenance, then every participant
  with lifecycle state (`no lease record` is the legacy label for a stale row
  without `last_seen`), `last_seen`, lineage, workspace, live watch, and separate
  `unread` and `pending` maps. `--room <room>` scopes the participant rows to
  that room (bound participants only); omit it to list the whole host. Legacy
  heartbeat rows stay under `legacy_rooms`; PIDs never appear.
- `--seen-by <id>`: which members' seen-sets contain that message (read-only).
- `--discard-through <id>`: ack exactly through one message (full id or a prefix
  unique in that channel) — the targeted alternative to `--discard`, which
  marks the whole currently-existing unread batch seen. Refuses to leap over a
  message that will not parse, and is safe to retry: a target whose range is
  already seen returns `advanced: false` with nothing changed.
- `--history N --grep PAT`: case-insensitive regex filter.
- Without `--max-bytes`, `post catchup` consumes the complete unread slice. No selector means
  `--all`; `--mail` selects direct mail and a channel argument requires
  membership. JSON is `{ok, room, targets[], count}`. Positional channel reads
  fail closed on an unloadable message; `--all` warns and skips an unloadable
  never-joined channel, while a broken joined channel remains a zero-count
  target. A non-empty catchup redirected to `/dev/null` is refused.
- `post search <pattern>` is read-only and cursorless. It searches party-visible
  mail and joined channels by literal case-insensitive Unicode substring over
  body, subject, sender, and id. `--mail` and `--channel` narrow scope;
  `--limit` defaults to 100 and caps at 1000. Results are newest first with
  sanitized 160-scalar previews and `matched` fields; mail results include
  `kind`, channel results include `channel` and no `kind`.
- Catchup and search accept `--framing auto|full|compact`. On non-empty text,
  `auto` is quiet; explicit `full`/`compact` request recurring banners. JSON
  retains source/authority and omits `laws` in auto. Legacy
  `POST_FRAMING=compact` also selects quiet output for existing sessions.

## Byte-bounded reads and slices

- `--max-bytes N` is opt-in on full-body `read`, `chat`, and `catchup`. It
  caps final stdout bytes, including UTF-8, JSON escaping, pretty whitespace,
  framing, omission metadata, and newline. No flag means the old behavior and
  shape. Budgeted output contains only complete bodies and stops at the first
  message that does not fit; only complete emitted ids are consumed after
  stdout succeeds. A too-small scaffold is `invalid_argument` on stderr with
  zero stdout and no read-state mutation.
- On Unix, Post uses a strict fd1 writer for result output. An invalid or
  read-only inherited stdout cannot count as success; no after-stdout cursor update,
  catchup delta, or exact ack runs. Budgeted JSON serializes each message once
  and reuses exact compact/pretty prefix sizes.
- Budgeted chat `auto` is quiet, just like unbudgeted reads. No read consults
  or stamps banner-day. Omission
  continuations advertise a measured stable cap
  covering the exact stored envelope at its widest later offsets plus the
  body's costliest encoded UTF-8 scalar. The chain remains runnable across
  decimal/scalar boundaries; the cap may exceed the original `byte_limit`.
- Chat applies bytes after count/history/mention-rescue selection. `skipped`
  remains the count-window remainder; `omitted` is the byte remainder and
  reports omitted mention count. Catchup uses one budget across its existing
  mail-then-channel target order, with explicit top-level/per-target remainder.
- Slice an omitted channel body with `post chat <channel> --message <id>
  --offset B [--length B] --max-bytes N --json`; slice direct mail with `post
  read <id> [--room <room>] --offset B [--length B] --max-bytes N --json`.
  Offsets address parsed-body UTF-8 bytes. JSON uses `body_slice`, `range`,
  `total_body_bytes`, and `next_offset`, never partial `body`. Non-boundary
  starts and overflow fail; every successful non-EOF partial slice progresses.
  Empty/EOF slices may finish without a next offset. Slices never consume,
  even when full/final.
- Channel slice signature status is verified against the complete stored body
  (`verification_scope: stored_full_body`), not the slice. After reviewing,
  use `post chat <channel> --ack <id>` or `post read <id> [--room <room>]
  --ack`; exact ack mutates only that id after stdout succeeds. Do not use
  `--discard-through` for one isolated slice: it deliberately marks the whole
  earlier unseen range.

## Inbox, pending, and read state

Inbox JSON keeps pending counts separate from unread counts:
`{ok, participant, room, unread, count, skipped_unreadable, unread_count,
pending, pending_by_address, held}`. Iterate
`(.unread // [])[]` rather than guessing `items` or `messages`.

Receipt-less mail already in an address inbox appears in JSON `post inbox`
only through `pending` and `pending_by_address` counts, never as a pending id;
`inbox --text` marks the count, while `watch --snapshot` lists each
provisionally eligible id with `pending: true`. For eligible workspace or
participant mail, a bound consuming `post read <id>` publishes the frozen
receipt and consumes that id, and an admitted long watch routes a new arrival
on its next scan. Held lineage mail stays held until `post inbox --adopt`;
neither read nor long watch adopts it.

`--peek` preserves unread state. A non-peek `read` records only the complete
message it emitted as seen, and only after stdout succeeds; the message file
does not move.

`not_a_member` means the participant is not an effective member. A plain read
records only the page it emits as seen — the oldest 25 unread by default, or
the oldest `--limit N` (`--limit 0` shows all) — after stdout succeeds. If
newer messages remain, repeat the read to page forward; `--peek` keeps its
newest-slice glance and never mutates that state. `watch` never mutates it
either. Unified state is stored per participant in
`participants/<id>/cursors.json` v2 as sorted exact mail and channel seen-id
sets. Missing or malformed cursors degrade reads to all eligible messages
unread and doctor reports the issue without repairing it. Legacy room cursor
state remains read-only and is labelled as legacy by doctor.
If a fail-closed join names an unreadable participant's `participant.json` or
`channels.json`, restore or repair that file from a backup, then retry. A
re-bind can recreate only a missing deterministic participant record; it does
not repair either malformed file. Never delete the record or directory as a
repair.
Late ids below newer consumed ids still surface unread. In text mode, chat body
lines are prefixed with `  | ` so body content cannot imitate a header or trust
marker; direct `post read` remains the deliberately unguttered single-message
surface.

`post channels` JSON adds `room` and `unread` to each channel item. `room` is
the participant's workspace context or `null`. `unread` is the exact unseen
eligible count when a participant is bound and effectively joined, whether by
explicit join or workspace default; it is `null` when unbound or not a member.
A session-only participant gets the same count after joining explicitly. The
existing `messages` field remains the raw message-file count.
A participant's own messages are excluded from unread selection even if their
best-effort seen-state update is absent. Writes warn when one channel reaches
50,000 seen ids; watermark compaction is unsafe until a durable
arrival-sequence fence can distinguish later backfills.
A bound participant's own channel sends do not ring its watch; Post compares
`from_participant` with the caller. `--own <room>` remains only for legacy
unbound snapshots and is ignored by a bound participant.
