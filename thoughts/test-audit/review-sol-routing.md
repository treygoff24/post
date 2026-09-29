# Routing layer review (read-only)

Scope: the 88 declarations marked in `ledger-routing.md`. I cross-checked the ledger against the decisive test assertions, production paths, caller searches, and the live contract. No tests, Cargo, builds, Python, or Node ran. Current `tests/cli.rs` line numbers are about 143 lines earlier than the ledger's references.

## Verdict and disagreements

The ledger's retain marks have distinct observable assertions or a different entry point; I found no further safe deletion. Accept its four F, two C, and one D marks, subject to the repairs and decisions below. In particular, the corrupt-receipt scenarios cover different errors and surfaces, and the `ReceivedIndex` batch walk is not covered by the single-reader tests.

| Test (current location) | Ledger → review | Evidence and correction |
|---|---|---|
| `routing_crossed_send_uses_the_participants_seen_eligibility_snapshot` (`tests/routing.rs:363`) | F → F | Its only final assertion is send success at :409. Sends always deliver (`src/channel.rs:938-953`). After B consumes A's message, parse B's JSON receipt and assert **absence** of `crossed` (and preferably `warnings` empty), with the consumed message actually observed in B's read result. `crossed_report` obtains B's unread items at `src/channel.rs:1310-1327`, and JSON omits `None` at `src/commands/chat.rs:2372-2405`. This would catch a regression that consults the room's rather than B's seen set. A post-change mutation is still needed to prove it binds. |
| `mail_read_renders_each_frozen_sentence_and_silence_for_unknown` (`tests/cli.rs:12125`) | F → F | `let _ = expected` and the default-read absence check at :12164-12165 leave the five frozen sentences unproved. Preserve the JSON pass-through check; add `read <id> --framing full` (and a compact case if both renderers need independent proof) and assert the exact `Sender evidence: {sentence}` for every known value, with absence for the unknown. Keep a separate default-auto quiet assertion. |
| `chat_renders_every_known_provenance_sentence_on_every_text_read` (`tests/cli.rs:12174`) | F → F, **contract decision** | The negative sentence checks at :12216-12223 describe current chat output; the bracketed lower-case address check at :12224-12229 never matches the only production wording in `src/commands/read.rs:1035`. JSON assertions at :12233-12251 do bind. Do not delete the text expectation on the theory that chat evidence is obsolete until the documented contract conflict below is decided. If quiet chat is approved, rename this test and assert the absence of a real `Sender evidence:` / `Sender address:` line, not a string no renderer emits. |
| `mail_read_renders_address_line_with_non_credential_wording` (`tests/cli.rs:12256`) | F → F | The current negative at :12275-12280 proves only quiet auto output. Add `--framing full` and assert the complete positive line including `self-declared instance tag, opaque and non-routable`; retain the auto negative if that mode is the intended contract. `src/commands/read.rs:1032-1038` is reachable for explicit framing. This can be folded into the preceding mail evidence test after carrying the exact address wording. |
| `send_receipt_offers_a_runnable_sender_history_readback_command` (`tests/cli.rs:12785`) | C → C | `tests/routing.rs:1263-1323` already extracts and executes the printed command for routed and pending mail. First add the two text receipt assertions and the quoted `post read '<id>'` shape from `tests/cli.rs:12796-12806` to its routed case. Also assert the readback contains `printed routed`; A23 currently checks `own: true` but not the body, while B19 checks `body == "hi"` at :12811-12814. Then remove B19. |
| `basic_inbox_unread_count_uses_participant_eligibility_not_file_subtraction` (`tests/counts.rs:7`) | C → C | `tests/routing.rs:2499-2541` proves participant-specific unread and canonical retention, but never checks the `count` field. Carry an exact `count` assertion after A's read into that keeper before removing `tests/counts.rs`. The current three-message fixture gives `count == unread_count == 2` after a read, so it does **not** discriminate file subtraction from per-id eligibility; the old divergent fixture would be needed if that is the actual intended regression. |
| `participant_round4_lineage_recipient_filtering_remains_pending_p2` (`tests/participants.rs:1504`) | D → D | Only asserts send success at :1527-1538. Its founder is a literal non-member and no affiliate is bound, so no recipient filtering is exercised. `tests/routing.rs:2818-2865` proves actual blocked-affiliate exclusion with a permitted recipient; :2949-2980 proves a no-affiliate lineage send stays receipt-less and later adoption works. The blocked rule in this test adds no independent assertion. |

All other ledger marks remain R. The ledger's B14 recommendation to drop the text half is conditional on the contract decision, not a ready edit.

## Keeper per contract

| Contract | Keeper(s) |
|---|---|
| Frozen recipient receipts, independent exact-id consumption, late older IDs, sender self-suppression | `routing_two_participants_consume_independently_and_canonical_file_stays_put` (A44), A6, A48, A49; direct self A19 |
| Corrupt/tampered receipt and unreadable walk isolation | A7-A9, A41, `an_unreadable_receipt_fails_only_the_walks_that_reach_it` (SB2), `a_corrupt_receipt_is_skipped_and_warned_about_once` (SB3) |
| Pending, unread, held, recovery | A15-A17, A3, A35-A37, A45, A50, A53; `blocked_unrouted_mail_is_held_never_pending` (SB4) owns the batch path |
| Sender history, own reads, text readback | A18-A23; A23 absorbs the receipt assertions from B19; A25-A27 own format-specific state wording |
| Reply origin and targets across projections | A1, A2, `routing_every_structured_message_projection_exposes_both_reply_targets` (A4), A24, B12 |
| Admission, blocked rules, lineage exclusions | B1, A12-A14, A38, C3, A51; A49 owns lifecycle filtering |
| Sender identity and provenance | B2-B11, B16; repaired B13/B15 own full-framing mail evidence and address; B14 owns channel JSON fields pending text-contract decision |
| Read stdin guard and display-only store preservation | A33-A34; A46 with A17, A28, A47 |
| `--allow-self` retargeting and help/schema exposure | S1-S7, with `tests/schema_truth.rs:617` for schema |
| `ReceivedIndex` batch walk order and failure ownership | SB1-SB3 |

## Final edit order for this area

1. Get the maintainer's ruling on the documented provenance contract below. Preserve the live read renderer while that is unresolved.
2. Repair A5 with a non-vacuous consumed-message precondition and absent `crossed` assertion; prove the assertion with a targeted mutation when tests are permitted.
3. Restore positive full/compact mail evidence and address assertions in B13/B15. Resolve B14 text assertions according to the contract ruling; retain its JSON assertions.
4. Move B19's two receipt sentences, quoted command shape, and body readback check into A23, then delete B19. Move CT1's `count` assertion into A44, then delete `tests/counts.rs` if no divergent fixture is retained. Delete C4 after confirming A51/A53 remain.
5. Only after explicit maintainer approval, remove the two unused strict wrapper functions in `src/eligibility.rs`; leave the `_with` implementations and their callers intact. No test-only production seam is unlocked by these test edits.

## Maintainer decisions and documented contract

- `CONTRACT.md:1306-1314` says **every known provenance sentence appears on every full-message text read, mail and channel, under every framing mode**, and a present address renders on both surfaces. `docs/IDENTITY.md:43-47` also describes read-banner evidence. Current `src/commands/read.rs:976-994` returns early for auto, and the channel text path in `src/commands/chat.rs` has no call to `output::provenance_sentence` (the only call is `src/commands/read.rs:1027`). `CONTRACT.md:40-47` separately says default reads are quiet and full/compact opt in. The contract is internally out of sync with current behavior. Decide whether chat/default reads should gain provenance lines or the ratified copy should be revised. Changing `CONTRACT.md` or deleting B14's text promise on that basis needs the maintainer's decision.
- Removing `eligibility::visible_channel` and `eligibility::archived_channel` deletes production code even though it appears behavior-preserving; the brief requires a maintainer decision for that deletion. The hidden `--anyway` compatibility flag is documented in `src/commands/schema.rs` and should not be removed as incidental cleanup.

## Bug and dead-code verdicts

- **Provenance test gap: confirmed.** Exact search `rg -n 'Sender evidence|Sender address|provenance_sentence|FROZEN_' src tests` finds the positive production lines at `src/commands/read.rs:1024-1037` and only negative test assertions at `tests/cli.rs:12165,12277`; B13 discards its expected sentences. The explicit full/compact path reaches those lines. `src/commands/read.rs:1177-1215` tests framing law/body, not evidence. This is an unproved live path, not dead product code.
- **Documented channel provenance mismatch: confirmed by source.** `rg -n 'provenance_sentence\s*\(' src` finds the definition and the mail-read call only. Whether to treat missing chat evidence as a product bug or intentional quieting requires the ruling above. No runtime result is claimed.
- **Unused wrappers: confirmed by caller search.** `rg -n 'fn visible_channel\b|fn archived_channel\b|visible_channel\s*\(|archived_channel\s*\(' src tests -g '*.rs'` returns only definitions at `src/eligibility.rs:465,507`; the strict wrappers delegate to `_with` at :470 and :512. The parallel `_with` search finds live calls from `src/commands/search.rs:48,65,76` and `src/eligibility.rs:496`. There is no caller or test-only seam keeping the wrappers alive.
- **No other product bug confirmed by this read-only pass.** A7's digest mismatch path and the receipt-index behavior have different purposes; no observed wrong output is established. The `held_for` warning difference is a possible reporting gap, not proof of a lost message.

## Open checks when runs are allowed

Run the repaired A5 against a deliberate seen-set regression; run explicit mail full/compact B13/B15 and a chat text probe after the contract ruling; check CT1 with a restored divergent fixture if the subtraction bug remains a concern. SB1/SB2 require a non-root runner for `chmod 000` to be meaningful. This review ran none of those checks under the launch prohibition. The Beads store was read-only, so no bead was created or closed.
