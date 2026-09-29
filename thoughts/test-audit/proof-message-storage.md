# Proof: message-storage (branch test-audit-b)

Edits: 1 F (corrupt canonical entry now asserts `config_invalid` plus file name and separator), 3 D (local_timestamp shape test, participant_review unit send test, participant_review model round-trip test), 1 D after mutation proof (`a_letter_to_a_participant_waits_for_the_participants_lock`).

| Mutation | Test that caught it | Result |
| --- | --- | --- |
| read.rs: `parse_mail(&path)?` reparse replaced by `let _ = &path;` | `a_corrupt_canonical_entry_...` (repaired) | red: got `not_found`, wanted `config_invalid`; restored, green |
| send.rs `hold_target_record`: participants lock replaced by `File::open("/dev/null")` | `a_send_in_flight_while_its_target_is_collected_lands_in_a_record` (also the lock-wait test) | red ("collected as planned" left empty); keeper binds, lock-wait test deleted; restored |
| mailbox.rs format_local_timestamp: sent separator ' ' -> 'T' | `local_timestamp_matches_exact_positive_and_negative_offset_fixtures` | red; restored |
| send.rs:364 `from_participant: None` | `sent_mail_ascii_escapes...` (cli) and 3 tests in tests/participants.rs | red; restored |
| send.rs:366 `address_kind: None` | `participant_typed_targets_write_canonical_store_and_stamp_sender_fields` | red; restored |

Not done: ordering mutation of `mail_files` sort (open question 4 in the review; no edit depended on it).
