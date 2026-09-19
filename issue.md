# Byline ignores `from_lineage`: every participant in a room renders as the room's profile

**Resolved 2026-09-19.** Commit `7c47a68` centralizes lineage-aware sender
rendering; `8af1a4b` records verification. Bead `post-jpw` is closed, Forgejo
`main` contains the fix, and the devbox runtime is installed at build
`7c47a68`. The report below is retained as the pre-fix diagnosis.

Filed by Fable (participant `claude-7ccc722a`, lineage `fable`) on 2026-09-19 at Trey's request. Bead `post-jpw` (P1 bug), papercut `pc2_cabd015c4ee4951b`. Observed on `post 0.9.0 (build e420aeb, store v2)`.

## What Trey saw

On `#atlas-ts7` this afternoon, every message from all three seats (Cairn, Fable, Rowan) rendered with the byline `🪨 Cairn (atlas)`. Trey asked why his two reviewers were speaking as the driver. The `reply=participant:` field on each line was correct the whole time (`claude-c5977dda` Cairn, `claude-7ccc722a` Fable, `codex-0ea0d6a0` Rowan); only the displayed name and pfp were wrong.

## Cause

The byline is built from the sending **room's** profile, stamped at send time, and never from the sender's lineage.

- `src/output.rs` `sender_label_impl` (around line 1531) takes `(from, display_name, pfp)` and renders `"<pfp> <Name> (<room>)"`. `display_name` and `pfp` come from the room profile (`profiles.json`, set by `post profile set`). Room `atlas` (path `~/Code/atlas`) carries `{name: "Cairn", pfp: "🪨"}`.
- `src/channel.rs` around line 1010 stamps `from_lineage: actor.lineage.clone()` into every `ChannelMessage`, and `src/model.rs` carries it on `Envelope` too. Verified live: `post --json chat atlas-ts7 --history 1` returns `"from_lineage":"rowan"` on Rowan's message.
- No renderer reads it. `rg -n from_lineage src` finds only the model fields, the channel writer, the schema strings in `src/commands/schema.rs`, and `None` in test constructors. The callers of `sender_label`/`sender_label_quoted` (`src/commands/chat.rs:456, 1833`, `src/commands/read.rs:882, 1155, 1173`, `src/commands/catchup.rs:1060, 1082`, `src/commands/inbox.rs:153`, `src/output.rs:1135, 1170` for watch text) all pass the room profile pair and nothing about the lineage.
- `docs/PARTICIPANTS.md` line 75 specifies the intended behaviour: "Renderers show `from_lineage (from_participant)` when present, then `from`." That sentence was never implemented; the room-profile rendering predates lineages and was left as the only byline path.

So this is a spec-versus-implementation gap from the 0.9.0 participants/lineages work, not a regression and not a new feature. It was latent as long as one agent per room was the norm.

## Why it surfaced today

All three seats work in `~/Code/atlas`, and `participant bind` infers the room from cwd (`src/commands/participant.rs` line ~115, `std::env::current_dir()` then `participant::bind(context, &cwd, args.workspace, ...)`). The session-start hook (`~/.claude-shared/hooks/post-claude-mail.mjs`) binds with that default, so on today's resume all three participants landed in room `atlas` and inherited its profile. During the 09-17/18 run each seat had been pinned by hand with `--workspace fable-devbox` / `--workspace rowan-devbox`, which is why it did not bite then. Cairn predicted this exact rendering for the engine's park notices on 09-17 (channel id `20260918-03415`, "post profiles are per-room and the supervisor shares room `atlas`").

Today's workaround, already applied: Fable and Rowan re-ran `post participant bind --workspace <own-room>`; both now render as `🦊 Fable (fable-devbox)` and `🌿 Rowan (rowan-devbox)`. The workaround only holds until the next cwd-inferred rebind.

## Requested change

1. **Byline prefers the lineage.** When a message carries `from_lineage`, `sender_label` renders the lineage as the name: `Fable (fable-devbox)` or, if the design keeps the room profile pfp, `🦊 Fable (fable-devbox)`. When there is no lineage, keep today's rendering byte-for-byte (room profile, else bare room id). The room id suffix stays visible in every case; it is the id-suffix invariant the two `sender_label` functions own, and this change must keep them as the only owners.
   - Lineages have no pfp field (`src/lineage_store.rs` `LineageSummary`/`LineageView` carry name, founder, created, host, members). Either render the lineage name with no pfp, or let the room profile pfp accompany the lineage name. Pick one and say which in the CHANGELOG; the ambiguity being fixed is the name, not the emoji.
   - Consider showing the participant id in the byline when two participants of the same lineage are active in one channel (the spec's `from_lineage (from_participant)` form); at minimum keep the existing `reply=participant:` field.
2. **Apply on every text surface**: `chat` (read, `--peek`, `--history`, `--since`), `read`, `inbox --text`, `catchup`, `search`, and `watch --text` / digest previews (`src/output.rs` 1135 and 1170). JSON output already exposes `from_lineage`; leave it.
3. **Sanitise the lineage name through `sanitize_text_header`** exactly like the display name. Lineage names allow spaces and punctuation (`mailbox::validate_room_name` grammar) and could be chosen to imitate a room id or the signed owner; apply the same imitation check `post profile set` applies to display names, or document why it is not needed for lineages.
4. **Bind warning (smaller, optional).** In `participant bind`, when the room was inferred from cwd and that room already has a live participant of a different lineage, print a one-line warning naming the collision and suggesting `--workspace <room>`. This does not change any binding.

## Red-proof

A fixture channel with two participants of different lineages both bound to one room, with the room carrying a profile, must render two different bylines; strip the fix and the test must go red with both lines reading the room profile. Existing tests to extend: `src/output.rs` `sender_label_*` unit tests (line ~1673 onward), `tests/lineage.rs`, `tests/participants.rs`. Keep the `sender_label_absent_profile_is_bare_room_id` byte-identity guarantee green.

## Out of scope

- Changing who owns room profiles, or making profiles per participant.
- The cwd-inference default itself (keeping it; the warning in item 4 is the mitigation).
- Post's own `.beads` drift (32 JSONL-only records the Dolt store lacks; `bd` refused auto-export on 2026-09-19). Separate reconciliation.

## Reproduce today

```sh
post profile show atlas                      # {"name":"Cairn","pfp":"🪨"}
post --json chat atlas-ts7 --history 3 | jq -c '.messages[] | {from, from_participant, from_lineage}'
post chat atlas-ts7 --history 3 --framing compact   # bylines before 15:48 -0500 all read "🪨 Cairn (atlas)"
```
