# Participants installed smoke acceptance

The smoke is authored before the integrated P.2 binary freeze. The current-main run used the release binary built from `a69f389` with `cargo build --release --locked`, `POST_SMOKE_EXPECT_CAPABILITIES=participants,lineages`, and `POST_SMOKE_EXPECT_STORE_VERSION=1`. P.2 rows are real assertions and remain red; none are skipped or weakened. The 12 rewritten legacy checks execute and report independently even when participant rows fail. The default remains the strict four-capability, store-version-2 contract; `POST_SMOKE_EXPECT_BUILD_SHA` additionally pins the frozen run and fails on mismatch, while every run rejects `build_sha: "unknown"`.

The frozen-release column will be filled only after the coordinator supplies the integrated binary.

| Named check | Current main | Preserved 0.9.0 | Release build |
| --- | --- | --- | --- |
| `P13-01` | green@a69f389 | red-proof@0.9.0 | PENDING (integrated binary not yet frozen) |
| `P13-02` | red@a69f389 (needs P.2) | red-proof@0.9.0 | PENDING (integrated binary not yet frozen) |
| `P13-03` | red@a69f389 (needs P.2) | red-proof@0.9.0 | PENDING (integrated binary not yet frozen) |
| `P13-04` | red@a69f389 (needs P.2) | red-proof@0.9.0 | PENDING (integrated binary not yet frozen) |
| `P13-05` | red@a69f389 (needs P.2) | red-proof@0.9.0 | PENDING (integrated binary not yet frozen) |
| `P13-06` | green@a69f389 | red-proof@0.9.0 | PENDING (integrated binary not yet frozen) |
| `P13-07` | red@a69f389 (needs P.2) | red-proof@0.9.0 | PENDING (integrated binary not yet frozen) |
| `P13-08` | red@a69f389 (needs P.2) | red-proof@0.9.0 | PENDING (integrated binary not yet frozen) |
| `P13-09` | red@a69f389 (needs P.2) | red-proof@0.9.0 | PENDING (integrated binary not yet frozen) |
| `P13-10` | green@a69f389 | red-proof@0.9.0 | PENDING (integrated binary not yet frozen) |
| `LIFECYCLE-touch` | green@a69f389 | red-proof@0.9.0 | PENDING (integrated binary not yet frozen) |
| `LIFECYCLE-who` | green@a69f389 | red-proof@0.9.0 | PENDING (integrated binary not yet frozen) |
| `LIFECYCLE-fanout` | red@a69f389 (needs P.2) | red-proof@0.9.0 | PENDING (integrated binary not yet frozen) |
| `LIFECYCLE-frozen` | red@a69f389 (needs P.2) | red-proof@0.9.0 | PENDING (integrated binary not yet frozen) |
| `UNBOUND-watch` | red@a69f389 (needs P.2 typed address) | red-proof@0.9.0 | PENDING (integrated binary not yet frozen) |
| `UNBOUND-version` | green@a69f389 | red-proof@0.9.0 | PENDING (integrated binary not yet frozen) |
| `UNBOUND-show` | green@a69f389 | red-proof@0.9.0 | PENDING (integrated binary not yet frozen) |
| `CHANNEL-own-leave` | red@a69f389 (needs P.2) | red-proof@0.9.0 | PENDING (integrated binary not yet frozen) |
| `IDENTITY-withdraw` | green@a69f389 | red-proof@0.9.0 | PENDING (integrated binary not yet frozen) |
| `IDENTITY-terms` | green@a69f389 | red-proof@0.9.0 | PENDING (integrated binary not yet frozen) |
| `OUTPUT-closed-pipe` | red@a69f389 (needs P.2) | red-proof@0.9.0 | PENDING (integrated binary not yet frozen) |

| Named legacy check | Current main | Preserved 0.9.0 | Release build |
| --- | --- | --- | --- |
| `LEGACY-01` | green@a69f389 | green@0.9.0 (doctor control) | PENDING (integrated binary not yet frozen) |
| `LEGACY-02` | green@a69f389 | red-proof@0.9.0 | PENDING (integrated binary not yet frozen) |
| `LEGACY-03` | green@a69f389 | red-proof@0.9.0 | PENDING (integrated binary not yet frozen) |
| `LEGACY-04` | green@a69f389 | red-proof@0.9.0 | PENDING (integrated binary not yet frozen) |
| `LEGACY-05` | green@a69f389 | red-proof@0.9.0 | PENDING (integrated binary not yet frozen) |
| `LEGACY-06` | green@a69f389 | red-proof@0.9.0 | PENDING (integrated binary not yet frozen) |
| `LEGACY-07` | red@a69f389 (needs P.2) | red-proof@0.9.0 | PENDING (integrated binary not yet frozen) |
| `LEGACY-08` | green@a69f389 | red-proof@0.9.0 | PENDING (integrated binary not yet frozen) |
| `LEGACY-09` | red@a69f389 (needs P.2) | red-proof@0.9.0 | PENDING (integrated binary not yet frozen) |
| `LEGACY-10` | green@a69f389 | red-proof@0.9.0 | PENDING (integrated binary not yet frozen) |
| `LEGACY-11` | green@a69f389 | red-proof@0.9.0 | PENDING (integrated binary not yet frozen) |
| `LEGACY-12` | red@a69f389 (needs P.2) | red-proof@0.9.0 | PENDING (integrated binary not yet frozen) |

## P.2 field contract used

The P.2 lane's `tests/routing.rs`, `tests/fixtures/watch-snapshot-typed.ndjson`, and `src/commands/schema.rs` supplied these exact spellings: `pending`, `pending_by_address`, typed `address.kind` and `address.name`, receipt `recipients`, `already_read`, and the `participants/<id>/cursors.json` v2 `mail[address].seen` shape. Only names and payload shapes were read from that lane; no implementation code was copied.

## Preserved 0.9.0 red proof

Command:

```sh
bash scripts/smoke-installed.sh /tmp/post-before-participants.64fZu4/post
```

Observed exit: `1`. `LEGACY-01` is the sole green control. Every participant row and every legacy row that requires participant state fails, while all rows still execute and report by name.

Failing row output:

```text
FAIL P13-01: A participant bind unavailable: {"ok":false,"error":{"code":"invalid_argument","message":"error: unrecognized subcommand 'participant'\n\nUsage: post [OPTIONS] <COMMAND>\n\nFor more information, try '--help'.","details":{"reason":"command-line parse failure"},"retryable":false,"suggested_fix":"Run `post --help` or `post schema` and retry with the documented syntax."}}
FAIL P13-02: command failed (65): stderr=[post: sending as 'smoke' (identity inferred from cwd); pass --from <NAME> to send as someone else {"ok":false,"error":{"code":"unknown_room","message":"recipient room 'workspace:smoke' is unknown","details":{"input":"workspace:smoke","matches":["smoke"],"reason":"recipient is absent from rooms.json"},"retryable":false,"suggested_fix":"Run `post rooms` to list rooms or `post channels` to list channels, then retry with `post send --to <registered-room>` or `post chat <CHANNEL> --send`."}} ] stdout=[]
FAIL P13-03: command failed (65): stderr=[post: sending as 'smoke' (identity inferred from cwd); pass --from <NAME> to send as someone else {"ok":false,"error":{"code":"unknown_room","message":"recipient room 'workspace:smoke' is unknown","details":{"input":"workspace:smoke","matches":["smoke"],"reason":"recipient is absent from rooms.json"},"retryable":false,"suggested_fix":"Run `post rooms` to list rooms or `post channels` to list channels, then retry with `post send --to <registered-room>` or `post chat <CHANNEL> --send`."}} ] stdout=[]
FAIL P13-04: row04 A bind failed
FAIL P13-05: row05 A bind failed
FAIL P13-06: row06 founder bind failed
FAIL P13-07: B participant cursor missing before leave
FAIL P13-08: JSON assertion failed: .pending_by_address["workspace:smoke"] == 1 and ([.unread[]?.id] | index($id)) == null; actual={"ok":true,"room":"smoke","unread":[{"id":"20990916-030200-aaa111","from":"smoke","kind":"note","subject":"older","sent":"2026-09-16 03:02:00 -0500"}],"count":1,"skipped_unreadable":0,"unread_count":1}
FAIL P13-09: command failed (65): stderr=[post: sending as 'smoke' (identity inferred from cwd); pass --from <NAME> to send as someone else {"ok":false,"error":{"code":"unknown_room","message":"recipient room 'workspace:smoke' is unknown","details":{"input":"workspace:smoke","matches":["smoke"],"reason":"recipient is absent from rooms.json"},"retryable":false,"suggested_fix":"Run `post rooms` to list rooms or `post channels` to list channels, then retry with `post send --to <registered-room>` or `post chat <CHANNEL> --send`."}} ] stdout=[]
FAIL P13-10: command failed (2): stderr=[{"ok":false,"error":{"code":"invalid_argument","message":"error: unrecognized subcommand 'version'\n\nUsage: post [OPTIONS] <COMMAND>\n\nFor more information, try '--help'.","details":{"reason":"command-line parse failure"},"retryable":false,"suggested_fix":"Run `post --help` or `post schema` and retry with the documented syntax."}} ] stdout=[]
FAIL LIFECYCLE-touch: touch participant bind failed
FAIL LIFECYCLE-who: stale bind failed
FAIL LIFECYCLE-fanout: active fan-out bind failed
FAIL LIFECYCLE-frozen: frozen recipient bind failed
FAIL UNBOUND-watch: unbound snapshot was not strict typed NDJSON
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

covered-ids: S1 S2 S3 S4 S5 S6 F1 F2 F3 F4 F5 F6 F7 F8 F9 F10 F11 F12 F13 F14 F15 F16 F17 F18 F19 F20
acknowledged-rulings: 20260916-124930-783613
