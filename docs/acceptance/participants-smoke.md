# Participants installed smoke acceptance

The smoke is authored before the integrated P.2 and P.3 binary exists. The authoring run uses the release binary built from `a69f389` with `POST_SMOKE_EXPECT_CAPABILITIES=participants,lineages` and `POST_SMOKE_EXPECT_STORE_VERSION=1`. P.2 assertions stay live and red on that binary. All 12 legacy rows execute and report by name.

The default contract remains the four capabilities `participants,lineages,routing-receipts,cursors-v2` and store version 2. `POST_SMOKE_EXPECT_BUILD_SHA` can pin a frozen run. Every run rejects `build_sha: "unknown"`. The release column stays `PENDING` until the coordinator supplies the integrated binary.

| Named check | Authoring build | Preserved 0.9.0 | Release build |
| --- | --- | --- | --- |
| `P13-01` | green@a69f389 | red-proof@0.9.0 | PENDING |
| `P13-02` | red@a69f389 | red-proof@0.9.0 | PENDING |
| `P13-03` | red@a69f389 | red-proof@0.9.0 | PENDING |
| `P13-04` | red@a69f389 | red-proof@0.9.0 | PENDING |
| `P13-05` | red@a69f389 | red-proof@0.9.0 | PENDING |
| `P13-06` | green@a69f389 | red-proof@0.9.0 | PENDING |
| `P13-07` | red@a69f389 | red-proof@0.9.0 | PENDING |
| `P13-08` | red@a69f389 | red-proof@0.9.0 | PENDING |
| `P13-09-durable-read-suppression` | red@a69f389 | red-proof@0.9.0 | PENDING |
| `P13-09-unread-restart-rering` | red@a69f389 | red-proof@0.9.0 | PENDING |
| `P13-10` | green@a69f389 | red-proof@0.9.0 | PENDING |
| `LIFECYCLE-touch` | red@a69f389 (ruled lease semantics) | red-proof@0.9.0 | PENDING |
| `LIFECYCLE-who` | green@a69f389 | red-proof@0.9.0 | PENDING |
| `LIFECYCLE-fanout` | red@a69f389 | red-proof@0.9.0 | PENDING |
| `LIFECYCLE-frozen` | red@a69f389 | red-proof@0.9.0 | PENDING |
| `UNBOUND-watch` | red@a69f389 | red-proof@0.9.0 | PENDING |
| `UNBOUND-version` | green@a69f389 | red-proof@0.9.0 | PENDING |
| `UNBOUND-show` | green@a69f389 | red-proof@0.9.0 | PENDING |
| `CHANNEL-own-leave` | red@a69f389 | red-proof@0.9.0 | PENDING |
| `IDENTITY-withdraw` | green@a69f389 | red-proof@0.9.0 | PENDING |
| `IDENTITY-terms` | green@a69f389 | red-proof@0.9.0 | PENDING |
| `OUTPUT-closed-pipe` | red@a69f389 | red-proof@0.9.0 | PENDING |

| Named legacy check | Authoring build | Preserved 0.9.0 | Release build |
| --- | --- | --- | --- |
| `LEGACY-01` | green@a69f389 | green@0.9.0 (doctor control) | PENDING |
| `LEGACY-02` | green@a69f389 | red-proof@0.9.0 | PENDING |
| `LEGACY-03` | green@a69f389 | red-proof@0.9.0 | PENDING |
| `LEGACY-04` | green@a69f389 | red-proof@0.9.0 | PENDING |
| `LEGACY-05` | green@a69f389 | red-proof@0.9.0 | PENDING |
| `LEGACY-06` | green@a69f389 | red-proof@0.9.0 | PENDING |
| `LEGACY-07` | red@a69f389 | red-proof@0.9.0 | PENDING |
| `LEGACY-08` | green@a69f389 | red-proof@0.9.0 | PENDING |
| `LEGACY-09` | red@a69f389 | red-proof@0.9.0 | PENDING |
| `LEGACY-10` | green@a69f389 | red-proof@0.9.0 | PENDING |
| `LEGACY-11` | green@a69f389 | red-proof@0.9.0 | PENDING |
| `LEGACY-12` | red@a69f389 | red-proof@0.9.0 | PENDING |

## Watch guarantees

Core `post watch` does not consume mail. Its notification seen-state is process-local, not durable watcher state.

`P13-09-durable-read-suppression` consumes A's message with `read`, sends a fresh canary, and restarts A's streaming watcher. The canary must ring and the consumed id must not. A restarted snapshot supplies a second durable-cursor check. B was independently eligible when routing froze the recipient set, so B's unread copy still rings. This is not a claim that a participant created after routing can join the frozen cohort. `P13-09-unread-restart-rering` leaves A's message unread and starts a second watcher process. The same id must ring again; a watcher that persists notification state across processes fails this row.

Adapter notification dedupe is separate. `/tmp/post-papercuts-2026-09-15/installed-adapter-acceptance.py` is run once per installed adapter during acceptance. Its `adapter-strict-rehearsal-87df.json` receipt records `typed_12hex_metadata_and_dedupe` green for the installed Claude adapter, including the assertion `second event dedupes the first direct notice`. Those harness receipts, not this smoke, prove per-participant adapter dedupe across hook invocations.

No durable watcher-notification store is required or tested.

## Assertion and process controls

`P13-02` checks the sender's own read returns the exact id with `own:true`, then scans every sender mail seen-set and proves none contains that id. Sender history is inspection, not consumption.

`P13-03` checks B's consuming read returns the exact id, B's workspace seen-set contains it, and B's next inbox no longer lists it as unread. The frozen receipt check follows those assertions.

`P13-08` plants valid receipts for the newer and late older fixtures. The older id must appear in `.unread`, with `unread_count == 1`, before it is consumed. This exercises routed unread state rather than the pending path.

`LIFECYCLE-touch` seeds lease 7 twice. With `POST_PARTICIPANT_LEASE_HOURS` unset, touch must preserve 7. With the env set to 3, touch must reapply 3. `LIFECYCLE-frozen` seeds `last_seen` and `ended_at` to a fixed past instant, performs a consuming read after `participant end`, proves the exact id enters the ended actor's workspace cursor and leaves its unread inbox, then proves `ended_at` is unchanged and `who` still reports `ended`. The row permits the consuming read to refresh `last_seen`.

Every spawned watch and every snapshot is bounded, including the conflicting `LEGACY-04` invocation. Background subshells `exec` the binary, timeout cleanup walks descendants with `pgrep -P` before TERM and KILL, and the closed-pipe row takes the writer exit status from `wait_for_pid`. The closed-pipe proof still requires at least one stdout byte, the specific `io_error` for `write stdout` with an EPIPE reason, an unchanged cursor, and a successful regular-file retry that consumes the same id. INT, TERM, and EXIT run the same section cleanup, and a section exit of 130 or 143 stops the top-level script before the other section starts. Cleanup accepts any exact root returned by this run's `mktemp`, including roots below `~/.delegate/run-scratch`, and reports the root as removed.

Named `FAIL SETUP` checks reject a missing binary, jq, Python, mktemp, or pgrep before either scenario runs. Each scenario also checks `mktemp` and writability before deriving `POST_MAIL_ROOT`; a failed temp allocation exits nonzero without using an empty path.

Every legacy failure path sets a specific reason. Failed jq assertions include the actual JSON value.

## P.2 diagnostic run

The pinned P.2 build at `87df4ca` produced 28 green rows and six red rows. Five reds are the expected P.3-only rows: `P13-04`, `P13-05`, `P13-06`, `IDENTITY-withdraw`, and `IDENTITY-terms`. The sixth is spec amended by ruling (R28); red until the P.2 fix lands: with the lease env unset, `87df4ca` rewrites seeded lease 7 to default 24. The row is not weakened. All 12 legacy rows pass, as do A1, A2, both watcher-restart rows, routed late-older unread, and the ended-participant consuming read.

The earlier `LIFECYCLE-frozen` green was timing-dependent because the byte comparison only passed when the consuming read refreshed `last_seen` within the same second. B3 supersedes that evidence: the row now seeds fixed past timestamps, checks `ended_at` rather than whole-file bytes, and positively proves consumption in the ended actor's workspace cursor. The current `87df4ca` replay is green under that deterministic assertion.

Transcript: green `P13-01 P13-02 P13-03 P13-07 P13-08 P13-09-durable-read-suppression P13-09-unread-restart-rering P13-10 LIFECYCLE-who LIFECYCLE-fanout LIFECYCLE-frozen UNBOUND-watch UNBOUND-version UNBOUND-show CHANNEL-own-leave OUTPUT-closed-pipe LEGACY-01 LEGACY-02 LEGACY-03 LEGACY-04 LEGACY-05 LEGACY-06 LEGACY-07 LEGACY-08 LEGACY-09 LEGACY-10 LEGACY-11 LEGACY-12`; red `P13-04 P13-05 P13-06 LIFECYCLE-touch IDENTITY-withdraw IDENTITY-terms`. Both scenario sections ran.

## Authoring transcript

The authoring run used this command and exited 1.

```sh
POST_SMOKE_EXPECT_CAPABILITIES=participants,lineages \
POST_SMOKE_EXPECT_STORE_VERSION=1 \
bash scripts/smoke-installed.sh target/release/post
```

Transcript: green `P13-01 P13-06 P13-10 LIFECYCLE-who UNBOUND-version UNBOUND-show IDENTITY-withdraw IDENTITY-terms LEGACY-01 LEGACY-02 LEGACY-03 LEGACY-04 LEGACY-05 LEGACY-06 LEGACY-08 LEGACY-10 LEGACY-11`; red `P13-02 P13-03 P13-04 P13-05 P13-07 P13-08 P13-09-durable-read-suppression P13-09-unread-restart-rering LIFECYCLE-touch LIFECYCLE-fanout LIFECYCLE-frozen UNBOUND-watch CHANNEL-own-leave OUTPUT-closed-pipe LEGACY-07 LEGACY-09 LEGACY-12`. Both scenario sections ran.

## Preserved 0.9.0 red proof

The preserved run used this command and exited 1. `LEGACY-01` is the sole green control; every participant row and `LEGACY-02` through `LEGACY-12` are red, and all rows report by name.

```sh
bash scripts/smoke-installed.sh /tmp/post-before-participants.64fZu4/post
```

Transcript: green `LEGACY-01`; red `P13-01 P13-02 P13-03 P13-04 P13-05 P13-06 P13-07 P13-08 P13-09-durable-read-suppression P13-09-unread-restart-rering P13-10 LIFECYCLE-touch LIFECYCLE-who LIFECYCLE-fanout LIFECYCLE-frozen UNBOUND-watch UNBOUND-version UNBOUND-show CHANNEL-own-leave IDENTITY-withdraw IDENTITY-terms OUTPUT-closed-pipe LEGACY-02 LEGACY-03 LEGACY-04 LEGACY-05 LEGACY-06 LEGACY-07 LEGACY-08 LEGACY-09 LEGACY-10 LEGACY-11 LEGACY-12`. Both scenario sections ran.

Raw failing row output:

```text
FAIL P13-01: A participant bind unavailable: {"ok":false,"error":{"code":"invalid_argument","message":"error: unrecognized subcommand 'participant'\n\nUsage: post [OPTIONS] <COMMAND>\n\nFor more information, try '--help'.","details":{"reason":"command-line parse failure"},"retryable":false,"suggested_fix":"Run `post --help` or `post schema` and retry with the documented syntax."}}
FAIL P13-02: command failed (65): stderr=[post: sending as 'smoke' (identity inferred from cwd); pass --from <NAME> to send as someone else {"ok":false,"error":{"code":"unknown_room","message":"recipient room 'workspace:smoke' is unknown","details":{"input":"workspace:smoke","matches":["smoke"],"reason":"recipient is absent from rooms.json"},"retryable":false,"suggested_fix":"Run `post rooms` to list rooms or `post channels` to list channels, then retry with `post send --to <registered-room>` or `post chat <CHANNEL> --send`."}} ] stdout=[]
FAIL P13-03: command failed (65): stderr=[post: sending as 'smoke' (identity inferred from cwd); pass --from <NAME> to send as someone else {"ok":false,"error":{"code":"unknown_room","message":"recipient room 'workspace:smoke' is unknown","details":{"input":"workspace:smoke","matches":["smoke"],"reason":"recipient is absent from rooms.json"},"retryable":false,"suggested_fix":"Run `post rooms` to list rooms or `post channels` to list channels, then retry with `post send --to <registered-room>` or `post chat <CHANNEL> --send`."}} ] stdout=[]
FAIL P13-04: row04 A bind failed
FAIL P13-05: row05 A bind failed
FAIL P13-06: row06 founder bind failed
FAIL P13-07: B participant cursor missing before leave
FAIL P13-08: JSON assertion failed: .pending_by_address["workspace:smoke"] == 0 and ([.unread[]?.id] | index($id)) != null and .unread_count == 1 and .count == 1; actual={"ok":true,"room":"smoke","unread":[{"id":"20990916-030200-aaa111","from":"smoke","kind":"note","subject":"older","sent":"2026-09-16 03:02:00 -0500"}],"count":1,"skipped_unreadable":0,"unread_count":1}
FAIL P13-09-durable-read-suppression: command failed (65): stderr=[post: sending as 'smoke' (identity inferred from cwd); pass --from <NAME> to send as someone else {"ok":false,"error":{"code":"unknown_room","message":"recipient room 'workspace:smoke' is unknown","details":{"input":"workspace:smoke","matches":["smoke"],"reason":"recipient is absent from rooms.json"},"retryable":false,"suggested_fix":"Run `post rooms` to list rooms or `post channels` to list channels, then retry with `post send --to <registered-room>` or `post chat <CHANNEL> --send`."}} ] stdout=[]
FAIL P13-09-unread-restart-rering: command failed (65): stderr=[post: sending as 'smoke' (identity inferred from cwd); pass --from <NAME> to send as someone else {"ok":false,"error":{"code":"unknown_room","message":"recipient room 'workspace:smoke' is unknown","details":{"input":"workspace:smoke","matches":["smoke"],"reason":"recipient is absent from rooms.json"},"retryable":false,"suggested_fix":"Run `post rooms` to list rooms or `post channels` to list channels, then retry with `post send --to <registered-room>` or `post chat <CHANNEL> --send`."}} ] stdout=[]
FAIL P13-10: command failed (2): stderr=[{"ok":false,"error":{"code":"invalid_argument","message":"error: unrecognized subcommand 'version'\n\nUsage: post [OPTIONS] <COMMAND>\n\nFor more information, try '--help'.","details":{"reason":"command-line parse failure"},"retryable":false,"suggested_fix":"Run `post --help` or `post schema` and retry with the documented syntax."}} ] stdout=[]
FAIL LIFECYCLE-touch: touch participant bind failed
FAIL LIFECYCLE-who: stale bind failed
FAIL LIFECYCLE-fanout: active fan-out bind failed
FAIL LIFECYCLE-frozen: frozen recipient bind failed
FAIL UNBOUND-watch: unbound plain snapshot omitted the participant bind diagnostic
FAIL UNBOUND-version: command failed (2): stderr=[{"ok":false,"error":{"code":"invalid_argument","message":"error: unrecognized subcommand 'version'\n\nUsage: post [OPTIONS] <COMMAND>\n\nFor more information, try '--help'.","details":{"reason":"command-line parse failure"},"retryable":false,"suggested_fix":"Run `post --help` or `post schema` and retry with the documented syntax."}} ] stdout=[]
FAIL UNBOUND-show: command failed (2): stderr=[{"ok":false,"error":{"code":"invalid_argument","message":"error: unrecognized subcommand 'participant'\n\nUsage: post [OPTIONS] <COMMAND>\n\nFor more information, try '--help'.","details":{"reason":"command-line parse failure"},"retryable":false,"suggested_fix":"Run `post --help` or `post schema` and retry with the documented syntax."}} ] stdout=[]
FAIL CHANNEL-own-leave: own-channel cursor missing
FAIL IDENTITY-withdraw: withdraw actor bind failed
FAIL IDENTITY-terms: terms founder bind failed
FAIL OUTPUT-closed-pipe: could not bind closed-pipe recipient
FAIL LEGACY-02: legacy scenario setup failed: alpha participant bind failed: {"ok":false,"error":{"code":"invalid_argument","message":"error: unrecognized subcommand 'participant'\n\nUsage: post [OPTIONS] <COMMAND>\n\nFor more information, try '--help'.","details":{"reason":"command-line parse failure"},"retryable":false,"suggested_fix":"Run `post --help` or `post schema` and retry with the documented syntax."}}
FAIL LEGACY-03: legacy scenario setup failed: alpha participant bind failed: {"ok":false,"error":{"code":"invalid_argument","message":"error: unrecognized subcommand 'participant'\n\nUsage: post [OPTIONS] <COMMAND>\n\nFor more information, try '--help'.","details":{"reason":"command-line parse failure"},"retryable":false,"suggested_fix":"Run `post --help` or `post schema` and retry with the documented syntax."}}
FAIL LEGACY-04: legacy scenario setup failed: alpha participant bind failed: {"ok":false,"error":{"code":"invalid_argument","message":"error: unrecognized subcommand 'participant'\n\nUsage: post [OPTIONS] <COMMAND>\n\nFor more information, try '--help'.","details":{"reason":"command-line parse failure"},"retryable":false,"suggested_fix":"Run `post --help` or `post schema` and retry with the documented syntax."}}
FAIL LEGACY-05: legacy scenario setup failed: alpha participant bind failed: {"ok":false,"error":{"code":"invalid_argument","message":"error: unrecognized subcommand 'participant'\n\nUsage: post [OPTIONS] <COMMAND>\n\nFor more information, try '--help'.","details":{"reason":"command-line parse failure"},"retryable":false,"suggested_fix":"Run `post --help` or `post schema` and retry with the documented syntax."}}
FAIL LEGACY-06: legacy scenario setup failed: alpha participant bind failed: {"ok":false,"error":{"code":"invalid_argument","message":"error: unrecognized subcommand 'participant'\n\nUsage: post [OPTIONS] <COMMAND>\n\nFor more information, try '--help'.","details":{"reason":"command-line parse failure"},"retryable":false,"suggested_fix":"Run `post --help` or `post schema` and retry with the documented syntax."}}
FAIL LEGACY-07: legacy scenario setup failed: alpha participant bind failed: {"ok":false,"error":{"code":"invalid_argument","message":"error: unrecognized subcommand 'participant'\n\nUsage: post [OPTIONS] <COMMAND>\n\nFor more information, try '--help'.","details":{"reason":"command-line parse failure"},"retryable":false,"suggested_fix":"Run `post --help` or `post schema` and retry with the documented syntax."}}
FAIL LEGACY-08: legacy scenario setup failed: alpha participant bind failed: {"ok":false,"error":{"code":"invalid_argument","message":"error: unrecognized subcommand 'participant'\n\nUsage: post [OPTIONS] <COMMAND>\n\nFor more information, try '--help'.","details":{"reason":"command-line parse failure"},"retryable":false,"suggested_fix":"Run `post --help` or `post schema` and retry with the documented syntax."}}
FAIL LEGACY-09: legacy scenario setup failed: alpha participant bind failed: {"ok":false,"error":{"code":"invalid_argument","message":"error: unrecognized subcommand 'participant'\n\nUsage: post [OPTIONS] <COMMAND>\n\nFor more information, try '--help'.","details":{"reason":"command-line parse failure"},"retryable":false,"suggested_fix":"Run `post --help` or `post schema` and retry with the documented syntax."}}
FAIL LEGACY-10: legacy scenario setup failed: alpha participant bind failed: {"ok":false,"error":{"code":"invalid_argument","message":"error: unrecognized subcommand 'participant'\n\nUsage: post [OPTIONS] <COMMAND>\n\nFor more information, try '--help'.","details":{"reason":"command-line parse failure"},"retryable":false,"suggested_fix":"Run `post --help` or `post schema` and retry with the documented syntax."}}
FAIL LEGACY-11: legacy scenario setup failed: alpha participant bind failed: {"ok":false,"error":{"code":"invalid_argument","message":"error: unrecognized subcommand 'participant'\n\nUsage: post [OPTIONS] <COMMAND>\n\nFor more information, try '--help'.","details":{"reason":"command-line parse failure"},"retryable":false,"suggested_fix":"Run `post --help` or `post schema` and retry with the documented syntax."}}
FAIL LEGACY-12: legacy scenario setup failed: alpha participant bind failed: {"ok":false,"error":{"code":"invalid_argument","message":"error: unrecognized subcommand 'participant'\n\nUsage: post [OPTIONS] <COMMAND>\n\nFor more information, try '--help'.","details":{"reason":"command-line parse failure"},"retryable":false,"suggested_fix":"Run `post --help` or `post schema` and retry with the documented syntax."}}
```

## Timeout red proof

Two proxies delegated every command to `87df4ca` except `watch --once`, which slept for 300 seconds. The exec proxy replaced itself with the sleeper; the child proxy waited on the sleeper as a child. Both bounded smokes reported the expected failed rows, exited 1, and left no tagged sleeper process:

```text
exec stub:
FAIL P13-09-durable-read-suppression: A watch --once failed or timed out
FAIL P13-09-unread-restart-rering: unread watcher invocation 1 failed or timed out
FAIL LEGACY-02: default watch --once failed or timed out:
FAIL LEGACY-03: watch --from now --once failed or timed out:
exec-sleeper-pids-after-fail=

child stub:
FAIL P13-09-durable-read-suppression: A watch --once failed or timed out
FAIL P13-09-unread-restart-rering: unread watcher invocation 1 failed or timed out
child-sleeper-pids-after-fail=
```

## P.2 field contract used

The P.2 lane's `tests/routing.rs`, `tests/fixtures/watch-snapshot-typed.ndjson`, and `src/commands/schema.rs` supply these spellings: `pending`, `pending_by_address`, typed `address.kind` and `address.name`, receipt `recipients`, `already_read`, and the `participants/<id>/cursors.json` v2 `mail[address].seen` shape. Only names and payload shapes were read from that lane; no implementation code was copied.

covered-ids: S1 S2 S3 S4 S5 S6 F1 F2 F3 F4 F5 F6 F7 F8 F9 F10 F11 F12 F13 F14 F15 F16 F17 F18 F19 F20 A1 A2 A3 N1 N2 N3 N4 N5 N6 N7 N8 N9 B3 B4 B5 B6 B7 B8 B9 B10
acknowledged-rulings: 20260916-124930-783613 P.6-mail-round3-1 R28
