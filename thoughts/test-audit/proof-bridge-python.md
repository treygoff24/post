# Proof: Python bridge (test-audit-j)

Runner: `POST_BIN=$(command -v post) testrun post area-pybridge -- python3 -m unittest <module or dotted test>` (bridge/tests/run-all.sh uses unittest per module; one module at a time here). Baseline before edits: test_sweep 105/105 OK. Each mutation below edits Python production code under bridge/, runs the named tests, restores the file from a saved copy, and `git diff -- <file>` is empty afterwards.

## test_sweep.py
| Repair | Mutation | Result |
|---|---|---|
| push-then-die derives marker before receipt prune (F) | sweep.py: run `prune_outbox` before `derive_published_and_tidy` | RED (FAIL) |
| lineage/participant never relayed (F, positive control) | sweep.py select_outbound: `if not workspace_addressed(header)` -> `if True` (relay nothing) | RED (control fails; old negatives alone stay green) |
| peer room named like post storage (F, per-subtest log offset) | rooms.py `_log_invalid_once`: `if identity in seen` -> `if seen` (log only the first invalid ever) | RED for names lineages, routing (old cumulative-log assertion would stay green) |
| refused placeholder (F, placeholder_conflict) | rooms.py: drop the `placeholder_conflict` emit on the `rooms add` refusal | RED |
| inbound-received row absorbed into the inbound crash matrix (C) | sweep.py: move a `checkpoint("inbound-received")` before the reservation write | RED for both read_between rows |
| o5-prune archive hash at crash instant absorbed into prune crash test (C) | sweep.py prune: dirty the sender archive, `checkpoint("outbound-o5-prune")`, then restore it | RED (hook=outbound-o5-prune) |

## test_tick_v2.py
| Repair | Mutation | Result |
|---|---|---|
| enable_channels default carries a non-empty deny (absorbs the --check-config half) | channels.py parse: raise ConfigError when `deny` is non-empty | RED (channels round trip) |
| unknown-room test carries `health.ok` (absorbs test_sweep unknown-room) | sweep.py write_health: `"ok": bool(ok) and not quarantined` | RED |
| registry/local restriction (F, new record required) | sweep.py ignored_branches: suppress the log when `config.peers.get("trey") == []` (only the restricted phase) | RED (old cumulative check stays green by the phase-1 record) |
| archive check once per tick (F, exactly one) | channels.py: drop the `relay_history_rewritten` emit; and a second run that emits it twice | RED, RED (old `[1:] == []` passes with zero) |
| unhealthy forces full tick (F, settle then flip) | tick.py quiet_candidate: ignore prior `ok` | RED |
| fingerprint matrix (F, health.quiet false + fenced) | tick.py probe: `"fence": False` | RED at the fence row (old assertion stays green) |

## test_channels.py
| Repair | Mutation | Result |
|---|---|---|
| deadline stride 500 (F, raise_after=3 + short-walk control) | CHANNEL_DEADLINE_STRIDE 1000 | RED (1200-entry test) |
| same | STRIDE 1201 | RED |
| same | STRIDE 100 | RED (400-entry control raises) |

## test_localheld.py
| Repair | Mutation | Result |
|---|---|---|
| L16 folded into L45: index file stays absent after the refused seed | sweep.py seed_local_holds: call `localheld.rebuild_index` even though the seed refuses | RED |
| L16 folded into L45: `already_held == 1` | sweep.py seed: stop counting already_held rows | RED |
| L2 renamed to what it asserts (empty index, no append) | no mutation: a rename, ordering stays owned by the order-failure test | n/a |

## test_rooms.py
| Repair | Mutation | Result |
|---|---|---|
| symlink and tree rows added to the hostile-publication table (absorbs the nonregular-publications test) | rooms.py `_tree_blob`: drop the mode check, keep the type check | RED at mode 120000 and 040000 rows (13 failures across rooms tests) |
| same | rooms.py `_tree_blob`: drop mode and type checks | RED at both rows |
| owners audit compares inode and mtime with a 50 ms gap (F) | rooms.py update_owners: `if current != original` -> `if True` (rewrites owners.json every build) | RED (test_ownership_audit...; the legacy-owner test also goes red) |
| rooms-health docstring grep deleted (D) | none: the contested-count assertion in test_rooms_health... keeps `health["route_contested"]` | n/a |

## test_bounce_safety.py
| Repair | Mutation | Result |
|---|---|---|
| B15 folded into B30 row 1 (record claims participant `elsewhere` and workspace `orchard`; both inboxes must stay empty) | bounce.py `_read_origin`: stop comparing the record's sha256 to the letter's | RED (row "another letter's") |
| B24 folded into B26 as a sixth `letter line` row | bounce.py `_find_notice`: skip the sealed-notice sha comparison | RED on all six rows, including `letter line` |
| B6 (plain removed-room delivery) deleted: independent ledger and receipt guards stay in the two R tests | none run; the two guard tests are unchanged and were green | n/a |

## test_sweep.py: batch 4 restoration (author identity)
| Repair | Mutation | Result |
|---|---|---|
| `test_each_branch_reflog_contains_only_its_owner_commits` restored verbatim from 667921a~1 (only proof that trey and mac commits carry their own author) | sweep.py tick commit: `user.email=post-bridge@{settings.host}` -> `post-bridge@fc` | RED: ('trey', ['post-bridge@fc', 'post-bridge@fc', 'fixture@invalid']) |
| same | restore sweep.py byte for byte (git diff empty) | GREEN, 1 test |
