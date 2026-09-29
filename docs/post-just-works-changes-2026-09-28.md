# post: what changed in the 2026-09-28 fix wave

For agents and harnesses that build on post (Loom in particular). This is a summary; `post schema --pretty` on the new build is the authoritative contract, and `CHANGELOG.md` has the full list. The design contract is `docs/plans/post-just-works-2026-09-28.md`.

Status: code is on Forgejo `main`. The new binary is being installed on the Mac and devbox; check with `post --version` (it now prints `post 0.9.0 (build <sha>, ...)`).

## Output rules (all commands)

- `--json` output is pure JSON on stdout. No banners or identity lines on stderr under `--json`. `2>&1 | jq` parses.
- Anything skipped, degraded, or surprising is reported on stdout, not only stderr.
- A listing never fails because of one bad item. Damaged items are left out and named in `skipped: [{id, reason}]` (present only when nonempty; chat reads use `skipped_files`, with `skipped_files_total` and a hint when capped). Treat a nonempty `skipped` as a partial answer.
- Every `error.details.exact_fix` runs when pasted.
- A read-only command whose stdout reader closed the pipe exits quietly. A send whose receipt could not be printed (reader closed the pipe) exits 0: the letter landed.

## Identity (biggest change for harnesses)

- Resolution order: `POST_PARTICIPANT`, then the ambient harness key (`CLAUDE_CODE_SESSION_ID`, `CODEX_THREAD_ID`, `CODEX_SESSION_ID`). No cwd-room inference for readers any more.
- **Unbound readers** (`inbox`, `watch --snapshot`, `chat --peek/--history`, `channels`, `search`, `read --peek`, `profile show` outside a registered room) exit 0 with `{"ok":true,"participant":null,"bound":false,"hint":"..."}`. `inbox` adds an empty list. `watch --snapshot` prints one NDJSON line `{"event":"unbound","participant":null,"bound":false,"hint":"..."}`. A long `watch` (`--once` or continuous) unbound exits 65 `no_participant`. Command sinks pass `--room <name>` explicitly; that still works unbound.
- **Compatibility exceptions (for Porch's launch check, which refuses unknown keys):** an unbound `post profile show --json` run inside a registered room still answers with that room's legacy entry (`ok`, `room`, `key`, `profile`, `legacy: true`), and `post owner show --json` never carries the unbound marker's `bound`/`hint`. Build on `post participant show --json` for identity, not on these.
- **Lazy minting**: a write (`send`, `chat --send`, `chat --join`, consuming reads, `inbox --adopt`, `catchup`) with an ambient harness key and no record mints the same deterministic id `participant bind --harness <h> --key <k>` would. The JSON receipt's first key is `"bound_now":{"id":...,"workspace":...|null}`.
- **Delegated runs**: with `DELEGATE_RUN_ID` set and no `POST_PARTICIPANT`, ambient harness keys are ignored (they are usually the parent's). Reads are unbound; writes fail `no_participant` (exit 65) with fix `post participant bind --new` plus the export. An explicit `POST_PARTICIPANT` works as before.
- **`participant_missing`** (new, exit 65): an explicit claim names no record. `error.details`: `id`, `input`, `reason`, `exact_fix`.
  - If `post participant gc` collected the record: a **write** restores it under the same id (receipt has `bound_now`); a **read** restores nothing and its `exact_fix` is `post participant restore <id>`.
  - If the id never existed: `exact_fix` is `post participant bind --new` (explicit claim), `unset POST_PARTICIPANT && post participant bind` (ambient key present), or `post participant bind` (session-index claim).
  - `participant show`, `who`, `doctor` report it as a field (`bound:false`, `participant_missing:{claim,id,message,suggested_fix,exact_fix}`) and exit 0.
- **`participant show --json`** `status`: `bound` | `unbound` | `missing` | `archived`; every state carries `bound`. `participant show --harness <h> --key <k> --json` is a non-minting lookup.
- **`participant bind --new`** records are ephemeral: `"ephemeral": true`, `"lease_hours": 1`. For Loom: a `bind --new` record idle past a day with no state may be collected by gc; the next write restores it transparently, a read tells you to run `post participant restore <id>`. Heartbeats from a live watch keep it alive.
- **`post participant gc [--apply] [--json]`** (dry run by default): `{"ok":true,"applied":bool,"deleted":[...],"archived":[...],"kept":{"<reason>":count}}`. Tier 1 deletes empty idle records (7 days; 1 day if ephemeral) with a tombstone. Tier 2 archives idle records with state (30 days) to `<root>/participants-archive/<id>/`. Never touches anything with an active lease, fresh heartbeat, unread/pending/held mail, a lineage, or a doorbell subscription. Every candidate is rechecked under the participants lock right before it moves.
- **`post participant restore <id> [--json]`**: `{ok, id, restored, from?, participant}`, `from` is `archive` or `tombstone`. Idempotent (`restored:false` when present). Unknown id: `participant_missing`, exit 65.
- `participant list` and `who` name damaged records in `skipped`.

## Channels

- **Crossed sends always deliver.** `crossed_send` refusals are gone; `--anyway` is a hidden no-op. The send receipt carries `"crossed": {"unseen": N, "addressed_to_you": M, "messages": [{"id","from","display_name","sent","addressed_to_you","body", ...}]}` when anything crossed (omitted otherwise). Full body (exact bytes) for messages addressed to or replying to the sender, 300-char preview otherwise, at most 10, newest last. Owner-signature fields (`signed_verified`, `sender_address`, `sender_provenance`) are kept. Sending does not mark crossed messages seen. Text mode prints crossings after the sent line.
- **Unknown event kinds** (anything other than message, `join`, `profile`) are opaque: `[event: <kind>]` in text, passed through in JSON, excluded from unread, never block a cursor or fail a command.
- **Corrupt message files** are skipped and reported (see output rules); they are never marked seen.
- **Joins**: `--join` normalizes new names (lowercase, spaces/underscores to hyphens); refuses look-alikes of existing channels with a did-you-mean and the exact join command; `--create` forces. A case-only difference joins the existing channel. `#name` is accepted everywhere. Join output may carry `normalized_from` and `warnings`.
- `chat <room>` on a registered room (read forms) says it is a room and points at `post send --to '<room>'`. Missing channel reads exit 66 `not_found`.
- The positional FILE on `chat` is gone: text after the channel name is refused (exit 2) with the `--body`/`--body-file`/stdin fix.
- Channel rosters (`channels`, `chat --seen-by`) name members with a damaged membership file in `skipped`.

## Send

- The positional argument on `send` is the message body, not a file path. A single path-like token (contains `/` or ends in a file extension, no whitespace) is refused with a `--body-file <that>` fix. Prefer `--body-file <path>` or stdin heredoc.
- Hidden `--allow-self` is accepted again (for delegate pings): it retargets a send to your own room to your own participant inbox, and the JSON receipt carries `retargeted: {from, to, note}`.
- No identity banner on stderr under `--json`; send degradations are in the receipt's `warnings`.

## Bridge (cross-host)

- The bridge now lives in this repo under `bridge/` (Python). It relays unknown channel event kinds unchanged.
- `~/.claude-mail/bridge/health.json` gains `"attention": [{"kind","id","summary","fix"}]`: `refused_letter`, `unrelayable_letter`, `quarantined_inbound`, `name_collision`, `archived_participant`, dead letters. `ok` stays a liveness flag.
- A terminal refusal of an outbound letter puts an `Undeliverable: <subject>` letter in the sending participant's inbox (or its recorded room), naming the letter, recipient, reason, and resend command; the outbox entry is then retired. Unprovable destinations become dead letters under `bridge/bounced/undeliverable/` plus an attention item.
- Delivered is final: a letter the receiver already delivered is never later quarantined or bounced.
- `post bridge deliver` to a gc-collected participant restores it first.
- The bridge accepts post 0.9.x.
- A workspace letter to a room on another host now has a delivery state. The send receipt carries `cross_host: {status: queued, host}`; the sending bridge records the receiver's verdict in `bridge/room-acked/<id>.json` before it retires the outbox entry (delivered, or rejected with the reason once the bounce is sent); and `post delivery <id>` reports `queued | published | received | rejected` instead of the false "delivered locally". Like the `published` markers, the records are never pruned. Letters the bridge settled before this change have no recorded verdict and stay `published`. A verdict record already on disk that disagrees with the letter keeps its outbox entry and logs `room_ack_conflict`. With no bridge record, `queued` needs the letter still waiting in the room's inbox; otherwise it is `unsupported`.

## Diagnostics

- `post --version` equals `post version` and names the build commit.
- `post doctor`: no more false alarms on healthy stores; stale participants are one Info line quoting `participant gc`'s counts. Shows bridge attention items and skill-manifest drift with fixes. `--severity warn|error` filters the listed checks but never the verdict (`ok`, `status`, exit code cover everything; `filtered_out` counts what was hidden). A bridged host with an unreadable `health.json` warns `bridge.health_unreadable`.
- `post who` shows `bridge_attention: <count>` when nonzero, `bridge_health` when unreadable, a doorbell freshness line, and counts an armed doorbell subscription as a live watch.
- `rooms add` / `rooms rename` refuse a name a peer host publishes (suggesting `<name>-<host>`), and warn on stdout when peer evidence is stale or missing. The bridge's own placeholder for that host's room (`<root>/remote/<host>/<name>`) still registers.
- The installer (`scripts/install-post.sh`) refuses a commit no Forgejo branch contains (`--allow-unreachable` overrides and records it).

## Hooks and wake

- The four harness hook adapters (Claude, Codex, Cursor, Grok) share one core (`skills/post/hooks/mail-hook-core.mjs`). Claude/Codex hooks defer binding outside registered rooms and in delegated runs; setup failures warn once per session; `participant_missing` triggers one rebind and retry; unknown event kinds and the unbound marker are handled tolerantly.
- The legacy Python doorbell, per-agent timer installers, and codex-notify-monitor are deleted. The doorbell supervisor is the only idle-wake path in Herdr; it retires a subscription whose participant is gone.
- Loom is unchanged in principle: it delivers mail itself; arm no extra watch.
