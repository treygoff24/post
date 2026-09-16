# Seams map for the participants build (read-only recon, 2026-09-16, pre-change HEAD 04ca99b)

File:line references are against 04ca99b. Lanes: read the decisive code before editing; this map is navigation.

## Sender resolution (today)
- `Context::from_env` — `src/mailbox.rs:103-129` (HOME, `POST_MAIL_ROOT`). No instance concept.
- Precedence `--room`/`--from` > `POST_FROM` > cwd inference: `Context::resolved_room_with_provenance` `src/mailbox.rs:308-330`; `infer_from_cwd` `341-381` (deepest registered room containing cwd, else cwd basename → `InferredBasename`); `declared_env_pin` `438-464`; `declared_sender_address` `492-` (`POST_SENDER_ADDRESS`, ≤256 bytes, recorded verbatim, never used for routing).
- Channels resolve separately: `channel::acting_room` `src/channel.rs:157-247` — no `--from` override, requires the room to be registered (`UnknownRoom` exact-fix logic 190-246).
- `SenderProvenance` `src/model.rs:146-156`; set at `send.rs:112/122/136`, `channel.rs:162/1466/1487/1532/1555/1625`.
- `--from` vs pin conflict enforced in `send.rs::run_with_body_and_id` `src/commands/send.rs:76-370` (96-113).

## Store layout
- Room dir `root/<room>/{inbox,read,cursors.json,watch.heartbeat}`; `Context::mailbox_dirs` `src/mailbox.rs:413-436`. Global `archive/` gets a second copy of every sent mail (`send.rs:284,305-333`, `delivered_unarchived` asymmetry).
- Mail id `YYYYMMDD-HHMMSS-{6hex}` `new_mail_id` `mailbox.rs:957-961` (UTC); channel ids `local_timestamp_micros` `883-887`.
- Writes: `exclusive_atomic_write` `667-671` → `exclusive_atomic_write_with` `762-816` (tmp `create_new` 0600, `sync_all`, `hard_link`, `AlreadyExists` on id collision, retried 256×); `atomic_replace` `674-710` (rename) for mutable state files.
- `RESERVED_ROOM_NAMES` `mailbox.rs:64-73`, checked `547-550`.

## Direct-mail delivery
- `send.rs:172` requires `to` ∈ rooms; channel-name hint 172-222; `UnknownRoom` + Levenshtein `closest_room` (`mailbox.rs:964-971`) 224-241.

## Read state (`src/cursor_state.rs`)
- `STATE_VERSION=1` (15), `CURSORS_FILE` (13), lock (14); `cursor_path(context, room)` ~296-305 → `root/<room>/cursors.json`.
- `Snapshot::load` 52-67 (legacy fallback, warn on invalid); `mail_has_seen` 70-73; `channel_has_seen` 75-79; `max_seen` 86-91; `channel_seen_count` 93-95.
- Mutators `consume` 108-113, `consume_channel` 115-138 → `CursorAdvance{prior,cursor,advanced,marked}`, `consume_channel_through` 144-157, all via `consume_inner` 159- (locks, loads, applies `Delta{mail_moves, channel_seen}`, physical `exclusive_move` inbox→read, `replace_state`/`serialize_state` 400-427).
- Schema: `{"version":1,"mail":{"seen":[..]},"channels":{"<ch>":{"seen":[..]}}}` (BTreeSet).

## Eligibility predicates — five independent copies to unify
- `src/commands/chat.rs` `read_batch` 1507-1519 → `collect_batch` 1535-1604, `enum UnreadRule{AfterId, NotInSeen}` 1525-1529, membership 1552-1560, self-suppression `from == room` 1598. Best unification seed.
- `src/commands/inbox.rs:8-44` lists every file in inbox/ as unread; `unread_count` filters by `mail_has_seen` 40-44.
- `src/commands/read.rs` `is_committed_duplicate` 563-569; `resolve_already_read` ~574-.
- `src/commands/catchup.rs` mail skip ~810; channel skip 932 then `from == room` 936.
- `src/commands/watch.rs` `WatchTarget` 18-33; mail dedupe by path 751-772 (never consults cursors); channel 798-833 with `from == room || owned_rooms.contains(from)` 827 (`--own`).
- `src/commands/channels.rs:9-45` unread = `messages.saturating_sub(seen_count)` (35) — raw subtraction, excludes nothing; the known inconsistency.

## Channels (`src/channel.rs`)
- `ChannelPaths::new` 70-79 (`root/channels/<ch>/{messages,channel.json,members.json}`); `MemberMap = BTreeMap<String,String>` (35) room → join timestamp (348).
- `join` 255-359 (global `lock_channels` 122-129; blocked-rule check 283-298; event first, membership second 328-332).
- `send` 427-538 (`acting_room`; `NotAMember` 444-452; `crossed_send_check` 471 / 590-; `CrossedVerdict` 542-).
- Crossed-send inputs 590-596; `ChannelState::load(context, room)` per room; targeting 630-644.

## Presence and watch
- `src/presence.rs` `heartbeat_path` 22-24 (`root/<room>/watch.heartbeat`), `touch_heartbeat` 29-48, `read_presence` 99-123, `is_live` 173.
- `src/commands/watch.rs` `run` 235- ; targets keyed by room (256-273); `emitted_channel_ids` ~804-812; `target_dirs` 967-; `NotifyWake` 597-745, `PollWake` 585-596.

## CLI, dispatch, output, schema
- `Command` enum `src/cli.rs:77-108` (Send, Chat, Channels, Inbox, Read, Catchup, Search, Rooms, Profile, Owner, Schema, Doctor, Watch, Who); subcommand-of-subcommand precedent: `RoomsCommand`, `ProfileCommand`, `OwnerCommand`.
- Dispatch `src/commands/mod.rs:24-95` (`execute`), `migration_fence::classify_write` `mod.rs:26` / `migration_fence.rs:459-493` — explicit allowlist, extend for new writer commands.
- `src/output.rs` `*Output` structs (`WhoOutput` 420-424, `WhoRoom` 411-417), `ok: bool` first.
- `post schema` `src/commands/schema.rs` (laws incl. env vars 376-378, `fields(&[..])` 402); `tests/schema_surface.rs` pins substrings.

## Tests and gate
- `tests/common/mod.rs`: `Sandbox::new()` 23-64 (seeds rooms `claude-space, pact, agent-memory` + one blocked rule), `new_unseeded` 67-84, `run/run_in/run_in_env/run_with_stdin`, `post_command()` 452-459, helpers `register_alpha_beta`, `register_room`, `join_channel`, `write_channel_message`, `write_custom_mail`, `seed_channel_fixture`, `seed_fence_store`, `assert_success`, `assert_migration_refused`, `is_identity_notice` 499-502.
- `scripts/gate.sh`: fmt, clippy -D warnings, test, release build, node hook + launcher suites, doorbell unittest, `post schema`.
- `src/migration_fence.rs`: `.post-arx.json`/`.post-arx.lock` at root, `POST_ARX_GENERATION`, `admit()`, `read_only_must_not_mutate` 306-316, `classify_write` 459-493.

## Adapters, launcher, bridge, install
- `skills/post/hooks/claude-mail.mjs` `main()` 310-398: reads `hook_event_name`, `agent_id` (subagent suppression 320-323), `session_id`, `cwd`; runs `post watch --snapshot` with `cwd` 344-349; emits `{hookSpecificOutput:{hookEventName, additionalContext}}` (`writeAllSync` 55-63); state `<tmpdir>/post-claude-mail/session-<id>.json` written after emit 305-308; `identityCardContext()` only on SessionStart 332, merged via `withCard` (static import 42).
- `codex-mail.mjs`: same shape; `isSubagent()` 139-148 defensive OR; `session_id`+`cwd` 331-338. `docs/ADAPTERS.md:262-266` Codex fields.
- Installers: `install-claude-hooks.mjs` (settings.json argv[2]; SessionStart/UserPromptSubmit/PostToolUse 32,201-218; copies adapter + `identity-card.mjs` to `~/.claude/hooks/` 146-163; preflight 60-82); `install-codex-hooks.mjs` (`hooks.json`, `stableNodePath()` 41-42, `~/.codex/hooks/`, preflight 70-92).
- `identity-card.mjs`: `cardPath` 48-59, `CARD_MAX=4096`, `MERGED_CONTEXT_MAX=8448`, `FRAME` 43-46, `identityCardContext` 94-159 (O_NOFOLLOW fd read), `withCard` 78-89. Test shape in `identity-card.test.mjs` (sandbox 23-33).
- `launcher/agent-session`: room pin 322-347, `resolve_room_from_cwd` 80-120 (rc 0/3/4/5/6), `repo_key` 125-147, `mint_uuid` 152-187, address validation 354-359, chain re-entry 313-320, `resolve_vendor` 200-236, `--doctor` 238-267. `launcher/install` → `~/.local/libexec/post-launcher/`, shims in `~/.local/agent-shims/`, receipt-gated uninstall.
- `estate-harness` source `~/Code/linux-devbox/40-cells/user-env/estate-harness.py` (`scrub_and_prepare_env` 1769-1795, `prepare_launch` 2056-2153, `cmd_launch` 2166-2180) — independent of the launcher; unchanged this release.
- Bridge: installed `~/.local/bin/post-bridge-sweep` == `~/Code/claude-space/post-bridge/sweep.py` (sha256 a9d554fc…). `process_inbound` 1146-1205: `room not in real_rooms` → `undeliverable`, skipped and retried (1195-1201); `prune_outbox` ~1629 prunes only `delivered` receipts; `RECEIPT_STATUSES = {delivered, held, quarantined}` (31); `ENVELOPE_REQUIRED` (54); health flips `ok:false` on undeliverable (2160-2172) contrary to `skills/post/references/post-bridge.md:83-87`.
- Release/install: `docs/RELEASING.md` (Mac-only signing; estate upgrade 67-81), `scripts/release.sh`, `scripts/smoke-installed.sh`, `dist-workspace.toml` (`install-path = "~/.local/bin"`). Devbox agent binary that runs: `devagent:~/.local/bin/post` (root-owned `/usr/local/bin/post` behind it).
- `skills/post/SKILL.md` headings: Profiles 12, Signed owner 27, Laws 46, Identity 58, Estate-wide 75, Command surface 109, Direct mail 265, Channel workflow 288, Watch 329, Worked example 387, Doctor 446.
