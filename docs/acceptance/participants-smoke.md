# Participants installed smoke acceptance

The smoke is authored before the integrated P.2 binary freeze. The current-main run used the release binary built from `a69f389` with `cargo build --release --locked` and `POST_SMOKE_EXPECT_CAPABILITIES=participants,lineages`. P.2 rows are real assertions and remain red; none are skipped or weakened.

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
| `LIFECYCLE-frozen` | red@a69f389 (needs P.2) | red-proof@0.9.0 | PENDING (integrated binary not yet frozen) |
| `UNBOUND-watch` | green@a69f389 | green@0.9.0 (read-only control) | PENDING (integrated binary not yet frozen) |
| `UNBOUND-version` | green@a69f389 | red-proof@0.9.0 | PENDING (integrated binary not yet frozen) |
| `UNBOUND-show` | green@a69f389 | red-proof@0.9.0 | PENDING (integrated binary not yet frozen) |
| `CHANNEL-own-leave` | red@a69f389 (needs P.2) | red-proof@0.9.0 | PENDING (integrated binary not yet frozen) |
| `IDENTITY-withdraw` | green@a69f389 | red-proof@0.9.0 | PENDING (integrated binary not yet frozen) |
| `IDENTITY-terms` | green@a69f389 | red-proof@0.9.0 | PENDING (integrated binary not yet frozen) |
| `OUTPUT-closed-pipe` | red@a69f389 (needs P.2) | red-proof@0.9.0 | PENDING (integrated binary not yet frozen) |

## P.2 field contract used

The P.2 lane's `tests/routing.rs`, `tests/fixtures/watch-snapshot-typed.ndjson`, and `src/commands/schema.rs` supplied these exact spellings: `pending`, `pending_by_address`, typed `address.kind` and `address.name`, receipt `recipients`, and `participants/<id>/cursors.json`. Only names and payload shapes were read from that lane; no implementation code was copied.

## Preserved 0.9.0 red proof

Command:

```sh
bash scripts/smoke-installed.sh /tmp/post-before-participants.64fZu4/post
```

Observed exit: `1`. The `UNBOUND-watch` control passes because 0.9.0 already emits valid snapshot NDJSON or nothing; the scenario still fails decisively on every participant, lineage, capability, per-participant cursor, and receipt surface.

Failing row output:

```text
FAIL P13-01: participant bind unavailable: {"ok":false,"error":{"code":"invalid_argument","message":"error: unrecognized subcommand 'participant'\n\nUsage: post [OPTIONS] <COMMAND>\n\nFor more information, try '--help'.","details":{"reason":"command-line parse failure"},"retryable":false,"suggested_fix":"Run `post --help` or `post schema` and retry with the documented syntax."}}
FAIL P13-02: command failed (65): post: sending as 'smoke' (identity inferred from cwd); pass --from <NAME> to send as someone else {"ok":false,"error":{"code":"unknown_room","message":"recipient room 'workspace:smoke' is unknown","details":{"input":"workspace:smoke","matches":["smoke"],"reason":"recipient is absent from rooms.json"},"retryable":false,"suggested_fix":"Run `post rooms` to list rooms or `post channels` to list channels, then retry with `post send --to <registered-room>` or `post chat <CHANNEL> --send`."}}
FAIL P13-03: command failed (65): post: sending as 'smoke' (identity inferred from cwd); pass --from <NAME> to send as someone else {"ok":false,"error":{"code":"unknown_room","message":"recipient room 'workspace:smoke' is unknown","details":{"input":"workspace:smoke","matches":["smoke"],"reason":"recipient is absent from rooms.json"},"retryable":false,"suggested_fix":"Run `post rooms` to list rooms or `post channels` to list channels, then retry with `post send --to <registered-room>` or `post chat <CHANNEL> --send`."}}
FAIL P13-04: command failed (2): {"ok":false,"error":{"code":"invalid_argument","message":"error: unrecognized subcommand 'identity'\n\nUsage: post [OPTIONS] <COMMAND>\n\nFor more information, try '--help'.","details":{"reason":"command-line parse failure"},"retryable":false,"suggested_fix":"Run `post --help` or `post schema` and retry with the documented syntax."}}
FAIL P13-05: command failed (2): {"ok":false,"error":{"code":"invalid_argument","message":"error: unrecognized subcommand 'identity'\n\nUsage: post [OPTIONS] <COMMAND>\n\nFor more information, try '--help'.","details":{"reason":"command-line parse failure"},"retryable":false,"suggested_fix":"Run `post --help` or `post schema` and retry with the documented syntax."}}
FAIL P13-06: command failed (2): {"ok":false,"error":{"code":"invalid_argument","message":"error: unrecognized subcommand 'identity'\n\nUsage: post [OPTIONS] <COMMAND>\n\nFor more information, try '--help'.","details":{"reason":"command-line parse failure"},"retryable":false,"suggested_fix":"Run `post --help` or `post schema` and retry with the documented syntax."}}
FAIL P13-07: B participant cursor missing before leave
FAIL P13-08: JSON assertion failed: .pending_by_address["workspace:smoke"] >= 1
FAIL P13-09: command failed (65): post: sending as 'smoke' (identity inferred from cwd); pass --from <NAME> to send as someone else {"ok":false,"error":{"code":"unknown_room","message":"recipient room 'workspace:smoke' is unknown","details":{"input":"workspace:smoke","matches":["smoke"],"reason":"recipient is absent from rooms.json"},"retryable":false,"suggested_fix":"Run `post rooms` to list rooms or `post channels` to list channels, then retry with `post send --to <registered-room>` or `post chat <CHANNEL> --send`."}}
FAIL P13-10: command failed (2): {"ok":false,"error":{"code":"invalid_argument","message":"error: unrecognized subcommand 'version'\n\nUsage: post [OPTIONS] <COMMAND>\n\nFor more information, try '--help'.","details":{"reason":"command-line parse failure"},"retryable":false,"suggested_fix":"Run `post --help` or `post schema` and retry with the documented syntax."}}
FAIL LIFECYCLE-touch: touch participant bind failed
FAIL LIFECYCLE-who: stale bind failed
FAIL LIFECYCLE-frozen: frozen recipient bind failed
FAIL UNBOUND-version: command failed (2): {"ok":false,"error":{"code":"invalid_argument","message":"error: unrecognized subcommand 'version'\n\nUsage: post [OPTIONS] <COMMAND>\n\nFor more information, try '--help'.","details":{"reason":"command-line parse failure"},"retryable":false,"suggested_fix":"Run `post --help` or `post schema` and retry with the documented syntax."}}
FAIL UNBOUND-show: command failed (2): {"ok":false,"error":{"code":"invalid_argument","message":"error: unrecognized subcommand 'participant'\n\nUsage: post [OPTIONS] <COMMAND>\n\nFor more information, try '--help'.","details":{"reason":"command-line parse failure"},"retryable":false,"suggested_fix":"Run `post --help` or `post schema` and retry with the documented syntax."}}
FAIL CHANNEL-own-leave: own-channel cursor missing
FAIL IDENTITY-withdraw: command failed (2): {"ok":false,"error":{"code":"invalid_argument","message":"error: unrecognized subcommand 'identity'\n\nUsage: post [OPTIONS] <COMMAND>\n\nFor more information, try '--help'.","details":{"reason":"command-line parse failure"},"retryable":false,"suggested_fix":"Run `post --help` or `post schema` and retry with the documented syntax."}}
FAIL IDENTITY-terms: terms founder bind failed
FAIL OUTPUT-closed-pipe: could not bind closed-pipe recipient
```
