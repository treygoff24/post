# Proof: CLI surface edit (branch test-audit-f)

Each mutation was applied to production code, the named test run through testrun, then the file restored to its pre-mutation bytes (the harness rewrote the saved bytes; for files carrying uncommitted edits the diff was compared by grepping for the mutation text, none remained; committed-clean files showed an empty git diff).

## Deleted tests: the named keeper goes red where the deleted test could not

| Mutation | Keeper | Result |
| --- | --- | --- |
| app.rs `finish_command_result`: registration branch disabled (`if false && ...`) | unit `app::` (AP2 still present) | AP3 RED, AP2 stayed GREEN (4 passed, 1 failed): AP2 could not bind, as the review said |
| same | byte_budget `strict_stdout_preserves_committed_delivery_and_registration_rules` | RED (byte_budget.rs:1469) |
| same | surface `a_send_that_landed_exits_zero_when_its_receipt_cannot_be_written` | RED (surface.rs:339) |
| output.rs `sender_label_impl` bare fallback decorated (`{rendered_from}!{participant}`) | cli `old_mail_renders_unknown_origin_reply_metadata_without_an_evidence_line` | RED (OU4 keeper) |
| output.rs ChannelMessage `display_name` serialized when absent | contract_samples `contract_samples_match_the_real_producers` | RED (OU11 keeper) |

## Consolidations: the carried assertion goes red

| Mutation | Test | Result |
| --- | --- | --- |
| output.rs lineage not sanitized (OU7 row moved into OU6) | `sender_label_sanitizes_control_characters` | RED |
| output.rs participant id not sanitized (OU7 row) | same | RED |
| output.rs empty registry treated as fail-closed (OU12 `{}` control moved into OU13) | `a_colliding_import_never_turns_local_when_the_registry_goes_away` | RED |
| output.rs missing registry fails open | same | RED |
| output.rs unparseable registry fails open | same | RED |
| schema.rs "consumes only emitted ids" reworded (CL6 phrases into S2a) | `schema_states_canonical_cursor_history_and_bound_watch_truth` | RED |
| schema.rs "never mutates channel seen-sets" reworded (CL6) | same | RED |
| schema.rs POST_FRAMING "falls back to auto ..." reworded (CL6) | same | RED |
| schema.rs stale "never advances channel cursors" reintroduced (CL6 negatives) | same | RED |
| schema.rs "deduplicates channel messages" reworded (CL2 watch wording) | same | RED |
| mod.rs human-only refusal stops naming the flag (CL7 into CL8) | `global_json_before_any_human_only_flag_is_refused` | RED |
| cli.rs chat help drops the `post send --to` cross-reference (CL4 into CL9) | `help_and_schema_agree_that_chat_leads_with_sending` | RED |
| schema.rs doctor shape loses `severity_filter` (ST4 deleted; S2d keeper) | S2a/S2d test | RED |
| schema.rs chat_send shape loses `crossed` (ST4 deleted) | same | RED |
| schema.rs read_budget shape loses `already_read` (S2b dropped) | same | RED |
| inbox.rs JSON renames `held` (S3f dropped; CS1 keeper) | `contract_samples_match_the_real_producers` | RED |
| output.rs ChannelsOutput renames `archived_hidden` (S3e keys dropped; CS1 keeper) | same | RED |
| schema.rs chat usage loses every `--max-bytes` (S4a dropped; S4b keeper) | `schema_matches_budget_slice_and_exact_ack_surfaces` | RED |
| first attempt: chat usage loses one of two `--max-bytes` mentions | same | stayed GREEN: not a real mutation (the option set is unchanged); replaced by the row above |

## F repairs

| Mutation | Test | Result |
| --- | --- | --- |
| schema.rs chat usage drops `--discard` but keeps `--discard-through` | `every_option_a_command_helps_with_is_in_its_schema_usage` (ST8, exact tokens) | RED |
| schema.rs restore line loses "participant_missing error, exit 65" (whole participant shape still has both words via PARTICIPANT_MISSING) | `participant_gc_and_restore_are_documented_and_live` (ST7) | RED |
| schema.rs search shape loses nested `matched` | `schema_matches_catchup_and_search_help_and_json` (S3d whole-document check) | RED |
| new test `the_option_lexers_keep_prefix_related_options_apart` | fixture for `--discard` vs `--discard-through` | passes, and ST8 red above proves the lexer binds |

## Findings

- The whole-word live-document check (S3c/S3d/S4c) exposed two real schema documentation holes, fixed additively in src/commands/schema.rs: `chat_slice` `message` (id, from, subject, sent) and catchup's mail `messages[]` envelope (id, from, to, kind, subject, sent, origin, reply_to_*, address{kind,name}). Before the schema edit the repaired tests failed on exactly those words.
- The plain-live-key-set assertions (exact top-level key equality for catchup/search) remain in S3c/S3d; only the substring documented-field checks were replaced.
- S2e (exact read_json envelope line) is retained, per the review, until a populated live read_json document is checked.
- `InboxOutput` deleted; tests decode through the new test-local `common::InboxView`, which also decodes participant/pending/pending_by_address/held.
