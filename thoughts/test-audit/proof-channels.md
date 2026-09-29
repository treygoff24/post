# Proof: channels (17 mutations, 17 caught, all restored byte for byte)

Each mutation was applied to production code, the named test went red, and the source was restored (cmp-verified).

- C1 archive missing channel code: tests/archive.rs missing-channel test red.
- C2a/C2b history grep case-fold and regex: history_grep_filters_case_insensitive_regex red.
- C3 catch-up larger than batch: chat.rs unit red.
- C4 non-member read fix text: chat.rs non_member unit red.
- C5a/C5b blocked route join alpha->beta and beta->alpha: channels_just_work blocked-route table red (table lives in channels_just_work.rs, not routing.rs, to stay out of the routing worker's file).
- C6 tolerant list skip entry: channel.rs list_channels_skips_strays test red.
- C7 stray positional retryable: channels_just_work stray-positional keeper red.
- C8a/C8b crossed send stderr and log room/channel: channels_just_work crossed keeper red.
- C9 cursor snapshot precedence over legacy channel-state: cursor_state.rs new test red.
- D-ownseen (chat unit) and D-ownseen-cli (seen_by_lists_members_past_a_message_read_only): red.
- D-mention (rescue of mentions in skipped range): catch_up_never_silently_skips_mentions_of_reader red.
- D-missing (NotFound code): chat_with_a_room_name_says_it_is_a_room_and_agrees_on_the_exit_code red.
- D-actor (address_kind stamp): participant_typed_targets_write_canonical_store_and_stamp_sender_fields red.

Notes: some mutations are synthetic. Not isolatable: the symlink row of the writer-refusal test (two guards). Deletion of visible_channel/archived_channel left to the routing worker.
