# post-bridge

This directory is the bridge's home. Its history before the move into the post
repo lives in `claude-space` (`post-bridge/`, up to commit 6f57ce1), together
with the reviews, candidates and research that were left behind.

`post-bridge` carries direct Post mail between pinned hosts through one
single-writer Git branch per host. It does not change Post, discover rooms, read
`main`, or treat envelope identity fields as credentials. The forge-authenticated
branch identifies the sending host; the receiver still validates the room
binding, destination, envelope, local rules, and exact bytes.

The relay is a queue. Every tick runs the same fixed order — fetch peer
branches, finish inbound delivery, derive publication markers from the fetched
copy of its own branch, prune marker-gated delivered items, select and copy new
outbound mail from the archive, make one commit carrying receipts, prunes, and
new mail and push it, then derive markers again from the pushed `HEAD` (even if
a follow-up fetch fails) and tidy placeholder copies. This is the authoritative
§Outbound order of SPEC r3.5, and the code runs it in exactly that sequence.
Crashes reconcile from disk on the next tick.

## Install

```sh
bridge/install.sh --repo-url ssh://git@<forge>:2222/estate/post-relay.git \
  --host <host> --ssh-key /abs/path/to/relay_key --config /abs/path/to/config.json \
  [--post-bin /abs/path/to/post] [--interval 15]
```

Start `--config` from `config.template.json`: fill in `host` and `relay_url`
(the relay clone's `origin` URL), and keep its `channels` deny list. A deny
only protects a channel on the host that publishes it, so every host carries
the same list. The installer warns when a config has no `channels` key.

The installer requires Python 3.9 or newer and Post 0.9.0 (`post --version`
may add a trailing `(build ...)` annotation; the semver stays pinned). It resolves
Post at install time, installs `sweep.py` and `bridgelib/` from this
directory under `~/.local/lib/post-bridge/` together with a `BUILD` file that
names the post repo commit it was installed from (`commit=<sha>`, and
`dirty=yes` when `bridge/` had uncommitted changes), writes `~/.local/bin/post-bridge-sweep`, clones the
relay with the relay key baked into `core.sshCommand`, and exclusively creates
`$POST_MAIL_ROOT/bridge/config.json`. It runs `--check-config` before enabling
the systemd timer and never starts the service directly. The Linux interval is
15 seconds by default; the macOS launchd default remains 60 seconds. Run
`install.sh --uninstall` to remove the package, launcher, and units without
touching the mail root or relay clone.

The systemd service carries `BRIDGE_INTERVAL_SECONDS` (rendered from
`--interval`) and the timer sets `AccuracySec=1s`. `install.sh` has no launchd
path: a macOS host's plist is built by hand, and it must set
`BRIDGE_INTERVAL_SECONDS` to the job's `StartInterval` under
`EnvironmentVariables`, for example:

```xml
<key>StartInterval</key><integer>60</integer>
<key>EnvironmentVariables</key>
<dict>
  <key>BRIDGE_INTERVAL_SECONDS</key><string>60</string>
</dict>
```

Without it, health omits `interval_s`, and post refuses participant sends
from that host with `bridge_status_unavailable` (SPEC-v2 r6.0 Health).

If the plist sets `StandardOutPath`, point it at
`$POST_MAIL_ROOT/bridge/launchd.log`. The launcher renames that file to
`launchd.log.1` once it reaches 10 MiB, before it runs the sweep, and launchd
opens a fresh file at the next spawn. Keep stdout captured rather than sending
it to `/dev/null`: `config_error` on exit 2, `deadline`, and an unwritable
health record reach stdout only.

## Enroll

Run `bridge/enroll.sh --host <host> [--dry-run] [--init-registry]` as a
Forgejo operator, or `bridge/enroll.sh --verify <host>` to check an existing
enrollment. The script checks the Forgejo user when its token has `read:user`;
otherwise it relies on the exact branch-protection API to validate the username.
It checks registry and wildcard protections, then adds the host to the sorted
registry and prints the `post chat machineroom-devbox --send` announcement for
the operator; it does not send the message. `--dry-run` executes GET requests
only and prints every write it would perform.

## Run

`post-bridge-sweep --check-config` validates the installed environment and
`post-bridge-sweep` runs one tick. Normal operation uses the installed timer and
the service keeps `ExecStart=%h/.local/bin/post-bridge-sweep`.

## Required environment

All four variables are required. Path values must be absolute, canonical paths.

- `POST_MAIL_ROOT`: existing Post root.
- `BRIDGE_REPO`: dedicated clone with `machines/<BRIDGE_HOST>` checked out.
- `BRIDGE_HOST`: `^[a-z0-9-]{1,32}$`; it must equal `config.host`.
- `BRIDGE_SSH_KEY`: regular file with mode `0600`.

Optional variables:

- `POST_BIN`: Post executable, default `post`.
- `BRIDGE_MAX_MAIL_BYTES`: maximum relay object size, default and hard maximum
  1,048,576 bytes. This deliberately narrows Post's unbounded `--oversize`
  option.
- `BRIDGE_TICK_DEADLINE_SECONDS`: tick deadline, default 240 seconds.
- `BRIDGE_FETCH_GRACE_SECONDS`: seconds since the last successful fetch before
  a partition becomes unhealthy, default 600. Before the first successful
  fetch, the grace period starts at `first_tick_at`.
- `BRIDGE_DECIDED_RECHECK_SECONDS`: how long a "decided" marker is trusted
  before the letter is verified again, default 21600 (6 hours), `0` verifies
  every letter every tick. See Decided markers below.
- `BRIDGE_BOUNCE_TRANSIENT_SECONDS`: how long a refusal the receiver could still
  clear by itself (`unknown_room`, `unpublished_sender`, `name_collision`)
  waits before the sender's bridge treats it as terminal, default 3600.
- `BRIDGE_CRASH_AFTER`, `BRIDGE_RAISE_AFTER`, `BRIDGE_TEST_HEALTH_DELAY_MS`:
  test-only hooks, described under Crash hooks below.

Every Git command receives:

```text
GIT_SSH_COMMAND="ssh -o ConnectTimeout=10 -o BatchMode=yes -i <key> -o IdentitiesOnly=yes"
```

Fetch and push are also bounded by subprocess timeouts. A tick acquires
`$POST_MAIL_ROOT/bridge/.lock` without waiting. A stable hard-link anchor keeps
that inode discoverable if `.lock` is unlinked and recreated while held. A
second tick reports `busy`.

## Configuration

`$POST_MAIL_ROOT/bridge/config.json` is strict JSON. Duplicate or unknown keys
are fatal.

```json
{
  "host": "fc",
  "relay_url": "forge:estate/post-relay.git",
  "peers": {
    "trey": ["hq", "atlasos", "post-devbox"],
    "mac": ["claude-space", "porch"]
  },
  "channels": {
    "mode": "all",
    "deny": ["devbox-build", "litigation-work", "wade-overnight", "cos", "cos-urgent"]
  }
}
```

`peers` is optional. `channels` is optional too, and a config without it syncs
every channel (SPEC-v2 r6.2); `"channels": null` turns channel sync off.

`relay_url` must equal both `origin` URLs. Peer hosts and rooms are pinned;
rooms must be unique under ASCII case folding. Post's reserved names plus
`remote`, `bridge`, `.bridge`, `.bridge.lock`, `bridge.log`, and `channels` are
denied. A peer room may not collide with a real local room.

The sweeper reads `post rooms --json` before registration. It creates each
workspace at `remote/<host>/<room>` and calls `post rooms add` only when the
name is absent. It does not create `<root>/<placeholder>/inbox` or `read`: post
makes a room's mailbox directories the first time it writes there, and creating
them for every peer room on every tick left empty directories behind after a
rename or release. (A `post doctor` that still lists `room.<peer>.inbox_missing`
or `read_missing` as errors predates that change.) An existing registration at
that exact real path is a no-op. A different registration writes
`bridge/collisions.json` and exits 2.

## Files and branches

The checked-out branch is `machines/<host>`. Its tree root contains:

```text
outbox/<destination-host>/<room>/<id>.mail
receipts/<sender-host>/<room>/<id>.json
```

Peer branches are never checked out. Mail and receipts are read with
`git ls-tree` and `git show`; only `100644` blobs are admitted. The sweeper
stages exactly `git add -A -- outbox receipts`, makes at most one commit per
tick as `post-bridge <post-bridge@host>`, and pushes
`HEAD:refs/heads/machines/<host>`. It never rebases or forces. A rejected or
non-fast-forward push is `branch_diverged` and requires a human.

Local bridge state is under `$POST_MAIL_ROOT/bridge/`:

- `delivered/<host>/<room>/<id>`: SHA-256 delivery ledger.
- `received/<id>`: durable collision reservation holding `<host>/<room> <sha>`
  (exclusive-created before any delivery byte moves) that doubles as the
  bridge-origin exclusion marker for outbound selection.
- `published/<id>`: derived after the outbox object is confirmed on the remote.
- `decided/held/<id>`, `decided/unrelayable/<id>`, `chan-decided/<channel>/<id>`:
  decided markers (see Decided markers).
- `bounced/`: the bounce of a terminally refused letter (see Bounce):
  `<id>.json` (intent), `<id>.body` (original text), `<id>.sent`, and
  `undeliverable/` (notices with nobody to read them).
- `quarantine/`: rejected bytes or metadata retained for local forensics.
- `tmp/`: same-filesystem publication temporaries, cleared during recovery.
- `stray/`: unexpected relay-worktree files moved aside during recovery.
- `health.json`, `log.jsonl`, `log-conditions.json`, `.lock`, `.lock.anchor`,
  `.health.lock`, `collisions.json`: operations state.

Directories the bridge creates (`bridge/**`, placeholder `inbox/`/`read/`) get
the process umask; the bridge never chmods a directory it did not create in
that call, so post's rooms, the archive, and pre-existing placeholders keep
their modes. The bridge does not create a placeholder's `inbox/` or `read/`
at all; post does, when a letter first passes through them.

The sender's `archive/` is read-only to the bridge. Placeholder `inbox/` and
`read/` copies are routing artifacts and are removed only after push success.

An inbound `(host, room, id, sha256)` is replay-safe when its exact delivery
ledger has the same SHA-256; such a replay skips only the route evaluation —
fence rechecks and state repair still run. A ledger-only mailbox (a human
deleted every copy) is republished from the candidate and logged as
`ledger_only_repaired`; an inbox without its archive gets just the archive
repaired, logged as `archive_delivered`. Without a ledger,
matching local inbox, read, or archive bytes may also be repaired as a partial
delivery from that host. A ledger for the same room and id under a different
host is always an id collision, even when the bytes match. Any differing local
or ledger hash is also an id collision. Collisions are quarantined and never
replace existing mail or ledgers.

## Validation, holds, and receipts

The bridge checks size before reading a relay object, requires a lowercase Post
mail id and a four-component outbox path, rejects non-regular modes, and opens
local files with `O_NOFOLLOW`. It parses only the JSON bytes before the first
`\n---\n`; the body is never parsed, logged, or rewritten. Headers are limited
to 4 KiB. Required envelope fields, kind, subject length, timestamp, id, and
destination are checked. Forward-compatible unknown envelope keys are admitted
and logged.

Sender binding has three outcomes:

- A sender that is a real room on the receiver, or a placeholder homed on a
  different host, is quarantined as forged.
- A placeholder homed on the arrival host is host-verified.
- Any other grammar-valid sender is delivered and logged as `from-unhomed`.

`rules.json` is post's file; the bridge validates shape only, immediately
before the mutation batch. A missing file means no rules; the bridge
materializes the empty default when the pinned Post CLI needs the file for room
discovery. Rule destinations are grammar-checked, not required to be real local
rooms, so placeholder-targeted rules are valid but cannot match inbound
delivery. A matching rule writes a `held` receipt containing its reason
verbatim and leaves mail untouched. Removing the rule lets the next tick
replace that receipt with `delivered`. A malformed file never makes the node
config-fatal: it stops the inbound batch before any write with health reason
`rules_invalid` and exit 1. A hold is successful policy enforcement; held *mail*
does not make health false, though an unpushed held *receipt* is queued work.

For a deliverable item, `received/<id>` is written first as an exclusive
create, content `<host>/<room> <sha256>` — the winner of a same-(room, id)
race is fixed the moment the reservation lands and survives any crash. The
collision check reads it: a different host, room, or sha is an id collision;
the same triple resumes the interrupted delivery. Peer hosts are scanned in
sorted order, so arrival arbitration is deterministic. Outbound selection
continues to consult the marker by presence alone.

The bridge cannot hash bytes that it refuses to read. For an oversized object
or a non-regular tree entry, the quarantine receipt uses reason
`oversize-unread` or `non-regular-object`. Its SHA-256 is the hash of the UTF-8
descriptor `<host>/<outbox-path>@<object-id>` from `git ls-tree`. These receipts
are quarantined, never delivered, so the sender does not prune on them.
If `git show` cannot read a mail object, or its bytes no longer match the size
reported by `ls-tree`, the same descriptor hash is used with reason
`unreadable-object`; the entry is quarantined without stopping other mail. An
unreadable receipt is logged as `receipt_ignored`. Invalid UTF-8 peer paths have
no addressable receipt path, so they produce a SHA-256-keyed local forensic note
and `quarantined_path` log only.

Receipts have this exact shape:

```json
{
  "v": 1,
  "status": "delivered",
  "host": "fc",
  "room": "hq",
  "id": "20260823-050000-abc123",
  "sha256": "<64 lowercase hex>",
  "reason": "",
  "at": "2026-08-23T05:00:00+00:00"
}
```

Statuses:

- `delivered`: inbox/archive/ledger reconciliation completed. A matching sender
  may prune.
- `held`: a local blocking rule refused the write. The sender keeps the item.
- `quarantined`: hostile input, forged binding, invalid metadata, or an id/byte
  collision. The sender keeps the item.

Receipt paths, modes, size, keys, values, and SHA-256 are hostile input on the
sender. Only a valid `delivered` receipt prunes, except for the bounce below.

### Bounce

A `quarantined` receipt the receiver will never turn into a delivery
(`forged_self`, `remote_participant_unhomed`, an id collision, malformed
input) is *terminal*.
One the receiver could still clear itself (`unknown_room`,
`unpublished_sender`, `name_collision`) becomes terminal once it is older than
`BRIDGE_BOUNCE_TRANSIENT_SECONDS`, measured from the receipt's own first-written
time. On a terminal receipt the sender's bridge:

1. writes a system letter (`from: post-bridge`, subject `Undeliverable:
   <original subject>`) into the sending participant's inbox while that
   participant is active. A participant whose session ended or whose lease
   lapsed reads nothing until its conversation resumes, so the notice goes to
   the inbox of the room it worked in instead (any session in that workspace
   sees it), then to the letter's own sending room when that is a room on this
   host, then to the inactive participant's own inbox, and with no such
   participant at all to `bridge/bounced/undeliverable/` plus an `attention`
   item. The body names the
   letter id, recipient, the reason in words, and the exact `post send
   --body-file` command that re-sends the original text (kept in
   `bridge/bounced/<id>.body`). It reads with `post inbox` and `post read`.
2. records that it did, then retires the outbox entry with `git rm`.

Every step is idempotent under a hard kill (see the `bounce-*` crash hooks), so
a crash never sends two notices or none. The notice never enters `archive/`,
so it cannot itself travel to a peer.

### Decided markers

A full tick used to re-open and re-judge every archive letter and every local
channel message on every tick. Now only ids the bridge has not settled are
opened; the rest are recognised from directory listings.

- Received, delivered and published letters are already settled and are listed
  once per tick from `bridge/received`, `bridge/delivered` and
  `bridge/published`.
- `decided/held/<id>`: the local-held guard holds the letter and its record
  verified. The marker is trusted for `BRIDGE_DECIDED_RECHECK_SECONDS`, then the
  letter is verified again and the marker refreshed. The guard's store-level
  checks (index shortfall, floors, sentinel) still run every tick, so deleting a
  record is still caught at once.
- `decided/unrelayable/<id>`: the letter cannot be relayed. It is re-read only
  when its size or mtime changes. Only a verdict about the file's content is
  remembered; a read that failed on I/O is retried every tick.
- `chan-decided/<channel>/<id>`: a local channel message this host will not
  publish (imported from a peer, or authored under a name that is not a local
  room). Messages already committed on this host's branch are found with one
  `git ls-tree` per tick and skipped without being opened.

A marker is written only after the work it records, so a crash between them
redoes the work (idempotently) and never skips a letter undecided. A torn or
missing marker means "judge it again".

### Participant mail for an archived record

`post participant gc` moves a long-idle participant record whole from
`<root>/participants/<id>/` to `<root>/participants-archive/<id>/`; the
session's next `post participant bind` moves it back. While it is archived,
post cannot find the participant and `post bridge deliver` rejects a letter for
it for good (`unknown_participant`). So before calling deliver the bridge checks
for that state and, instead of asking, holds the letter: a `pmail_retry` with
reason `participant_archived` (counted in `pmail.retry_reasons`), no receipt,
nothing written into the missing directory, and one `archived_participant`
attention item per participant naming the letters and the fix, which is the
same move post's own restore makes:
`mv <root>/participants-archive/<id> <root>/participants/<id>`. The next full
tick after the record is back delivers the letters and the item leaves the
list. No post command restores by id (`bind` restores by the session's
conversation key, which the bridge does not have), and the bridge does not
write inside post's participant store, so this is the attention path, not an
automatic restore.

## Logs

Each action is one compact JSON line in `bridge/log.jsonl`. A tick does not
copy its log lines to stdout (under launchd that only grew a second, unrotated
file); the operator commands (`--seed-local-holds`, `--init-held-sentinel`) do,
because their stdout is their result. A quiet tick (nothing changed) writes no
`health` line. The log rotates at 10 MiB to `log.jsonl.1`; one prior file is
kept. No action contains a mail body.

A standing per-letter condition (`quarantined`, `forensic`,
`quarantined_path`, `held`, `outbound_ignored`, `outbound_waiting`,
`route_contested`, `receipt_ignored`, `unknown_envelope_keys`) is logged when
it first appears and when its fields change, not on every full tick. The last
logged state is `bridge/log-conditions.json`; a damaged file is rebuilt and the
tick logs as it would without it. A tick that read every peer emits one
`condition_cleared` line (`condition`, `host`, `room`, `id`) for each condition
that went away. Every full tick's `health` line carries `standing`, the count
of current conditions per action, so a quiet log still shows them.
`held` in `standing` and in `condition_cleared` is the inbound hold (a peer's
letter waiting, for example on `sender_not_homed` or a blocking rule), not the
local-held export guard; the guard's own `local_held*` lines are never deduped.

Configuration and setup actions:

- `config_error`: missing, malformed, mismatched, or unsafe configuration.
- `collision`: a peer name is already registered elsewhere.
- `room_registered`: one placeholder registration was added.
- `fenced`: `.post-arx.json` is present; mail and forge state are untouched.
- `busy`: another tick owns `bridge/.lock`.

Recovery and Git actions:

- `foreign_state`, `tmp_removed`, `stray_moved`, `git_lock_removed`: minimal
  dirty-state recovery. A rebase/merge in progress is foreign state — the tick
  stops untouched; stale git locks left by killed git children are unlinked.
  `remote_advanced` logs a fast-forward onto a forge branch that moved ahead.
- `fetch`: fetch success or failure.
- `ignored_branch`, `peer_branch_missing`: unlisted or absent peer branch.
- `committed`, `pushed`: the tick's one commit and explicit branch push.
- `git_failed`, `push_failed`, `branch_diverged`, `deadline`: terminal transport
  failures.
- `internal_error`: an unexpected exception; health records it and the tick
  exits 1.
- `relay_large`: clone storage exceeds 256 MiB.

Inbound actions:

- `unknown_envelope_keys`: admitted forward-compatible header keys.
- `from-unhomed`: grammar-valid free-form sender.
- `ledger_only_repaired`: a replayed delivery restored missing mailbox copies.
- `receipt`: receipt creation or status change.
- `held`: blocked route.
- `forensic`, `quarantined`, `quarantined_path`: rejected input and evidence.
- `undeliverable`: mail targets a room that is not real locally; includes age.

Outbound actions:

- `outbound_ignored`: local archive item is not relayable. The tick records each
  id once, and health retains at most 20 ids in `outbound_unrelayable`.
- `outbound_copied`: archive bytes entered the local branch outbox.
- `outbox_invalid`: malformed local outbox state.
- `receipt_ignored`: hostile or mismatched peer receipt.
- `outbound_waiting`: `held` or `quarantined` receipt, with status and age.
- `outbox_pruned`: matching delivery receipt removed the outbox entry.
- `published`: a marker derived from the remote branch tree.
- `placeholder_tidied`: published placeholder inbox/read artifact removed.
- `health`: final tick health.

## Health and exit status

`bridge/health.json` contains `ts`, `ok`, `reason`, `first_tick_at`,
`last_fetch_ok`, `last_push_ok`, `stalled_since`, `busy_streak`, `held`,
`quarantined`, `undeliverable`, `outbound_unrelayable`, and `attention`. `ok`
is liveness (the bridge is running, fetching and pushing); `attention` is the
separate list of things that are stuck: items `{kind, id, summary, fix}`,
where `fix` is an exact command or a one-sentence instruction. Kinds:
`refused_letter` (a terminal refusal whose sender could not be told, or whose
bounce failed), `unrelayable_letter`, `quarantined_inbound` (a peer's letter
this host refused; it clears when the sender's bounce retires it),
`archived_participant` (participant mail waiting for a participant whose record
`post participant gc` moved to `participants-archive/<id>/`; see below), and
`name_collision`. The list is recomputed each full tick, so an item leaves it
on the first tick after its cause is gone, and it is empty when nothing needs
anyone. Five consecutive
busy ticks are unhealthy; earlier busy ticks preserve any standing unhealthy
reason. A stale fetch, fence, divergence, push failure with queued work, or
undeliverable item is unhealthy. Queued work includes outbound mail, receipts
not yet on the remote **of any status — `held` included** (the sender cannot
see a hold until its receipt lands), dirty relay state, and local-ahead
commits. Held mail itself is reported by count only.

Permanently unrelayable local archive mail (over cap, unparsable envelope) is a
*terminal local rejection*: logged with id and reason at first sight, listed in
`outbound_unrelayable` (≤20 ids), counted, and healthy by design — nothing the
bridge can do later will move it, so it is not stalled work. Any other
uncaught exception inside the tick writes health `ok:false`
reason:`internal_error` with the exception class in the log and exits 1; a
traceback never leaves `health.json` stale. If that health write itself fails
(a deleted `.health.lock`, for example), the tick still exits 1 and emits one
final JSON record on stdout — `{"action": "internal_error",
"health_unwritten": "<message>", "class": "<exception class>", "ts": …}` —
from both the operational-error and catch-all handlers. On exit 2,
`bridge/health.json` is the only permitted write, and only when
`POST_MAIL_ROOT` validated and `bridge/` already existed. Every `health.json`
write — busy probes included — is a read-modify-write serialized under
`flock(bridge/.health.lock)`: all carried fields come from the read taken
under that lock, never from state gathered before acquiring it. The installer
creates that file; the sweeper opens it without `O_CREAT` — on the
fatal-config path a missing lock means `health.json` is written unlocked
rather than creating a file, and on any live path a missing lock propagates as
`internal_error` (misinstall).

After a successful push, the sweeper skips the post-push fetch when the local
tracking ref already equals `HEAD`. A foreign commit that lands after that push
therefore raises `branch_diverged` on the next tick rather than the current one.
This one-tick delay is safe because the pushed mail's markers are already
derived before the tick ends.

Host tokens and room names occupy separate namespaces and may share a
spelling (a peer host `sol` and a local room `sol` are unrelated). Placeholder
mailbox directories are always keyed by room name under the root —
`<root>/<room>/inbox` and `<root>/<room>/read` — while the registered
workspace remains `<root>/remote/<host>/<room>`.

`--check-config` validates the required environment, strict config, origin
URLs, and checked-out clone identity without creating logs, health, or bridge
state.

- Exit 0: healthy, or a busy lock before the fifth consecutive busy tick.
- Exit 1: unhealthy operational state. Retry ordinary outages; do not automate
  recovery from `branch_diverged`.
- Exit 2: fatal environment, topology, registration, repository, or local
  configuration error.
- A `BRIDGE_CRASH_AFTER` crash surfaces as SIGKILL (shell status 137);
  production units must not set the test hooks.

## Crash hooks

Three test-only hooks exist; all are inert unless set, and the test suite
scrubs them from every environment it builds. Production units never set them.

- `BRIDGE_CRASH_AFTER=<boundary>`: SIGKILLs the process at the named boundary —
  a hard crash with no cleanup, flush, or exit handling, exactly the semantics
  crash recovery must survive.
- `BRIDGE_RAISE_AFTER=<boundary>`: raises an injected `OSError` at the named
  boundary, driving the `internal_error` path.
- `BRIDGE_TEST_HEALTH_DELAY_MS=<ms>`: delays each busy-probe health write so a
  test can interleave two probes deterministically under the health lock.

Boundaries:

- Setup: `after-placeholders`.
- Inbound: `inbound-i1`, `inbound-i2`, `inbound-i3`, `inbound-i4`,
  `inbound-i5`, `inbound-received`, `inbound-i6`, `inbound-i7`, `inbound-i8`,
  `inbound-i9`.
- Outbound: `outbound-o1-select`, `outbound-o2-copy`, `outbound-o3-stage`,
  `after-commit`, `after-push` (the instant the push subprocess returns
  success, before any derivation), `after-derive` (after markers are derived
  from the pushed HEAD), `outbound-o4-published`, `outbound-o4-tidy`,
  `outbound-o5-prune`, `outbound-o6-bounced` (the notice is written and
  recorded; the outbox entry is not yet retired).
- Bounce: `bounce-b1-intent`, `bounce-b2-body`, `bounce-b3-letter`,
  `bounce-b4-sent`.
- Decided markers: `decided-d0-before-marker` (the hold's record is written,
  its marker is not), `decided-d1-marked`.

The tests exercise every hook against a real Post binary, including a real
`post read` between inbound ticks and prune recovery after commit and push.

## Fence and v2

The bridge does not open or create `.post-arx.lock`. Any
`.post-arx.json` marker fences the whole tick before placeholder, mailbox, or
forge mutation. The health stamp and bridge log still record the refusal.
Generation-aware admission for an enrolled Post store is v2.

Relay compaction is also v2. Today, pruned bodies remain in Git history. The
branch owner will eventually recreate `machines/<host>` from its current tree
as an orphan commit, with peers coordinated and branch protection deliberately
lowered and restored. No v1 process rebases, force-pushes, or rewrites history.

## Test

From the repository root:

```sh
POST_BIN=/abs/path/to/post bridge/tests/run-all.sh
```

`run-all.sh` runs every `bridge/tests/test_*.py` module (through
`python3 -m unittest`) and every `test_*.sh` script, four at a time
(`BRIDGE_TEST_JOBS` changes that), and exits non-zero with the failing
suite's log if any fail. One module alone: `POST_BIN=... python3 -m unittest
bridge.tests.test_sweep`.

The suites refuse to run unless `post --version` is `post 0.9.0`, optionally
followed by a ` (build ...)` annotation as a release build prints. All mail
roots are temporary and initialized by `post doctor --fix`; the suite never
touches the user's real Post root or relay repo.
