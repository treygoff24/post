# post-bridge root-cause lane (read-only), 2026-09-28

## Which code is live

- Both hosts run byte-identical bridge code (md5 of `sweep.py` plus all 9 `bridgelib/*.py` match): `~/.local/lib/post-bridge/`, installed 2026-09-25 15:27. It equals **claude-space `origin/bridge-v2-ship` @ 6f57ce1** ("Merge post-ojh bridge: relay roomless channel posts (v2 r6.3)").
- `/Users/treygoff/Code/claude-space/post-bridge` (main) is the stale v1 from Aug 26. The worktree `/Users/treygoff/Code/claude-space/post-bridge-wt-channels/post-bridge` (0744fa0) is 5 commits behind the install; it differs in `channels.py`, `common.py`, and `pmail.py`. Read the install, or `git show 6f57ce1:post-bridge/...`.
- All line numbers below refer to `~/.local/lib/post-bridge/{sweep.py,bridgelib/*.py}`.
- Devbox: systemd user timer, `BRIDGE_HOST=trey`, 15 s interval. Mac: launchd `StartInterval 15`, `BRIDGE_HOST=mac`.

## Summary

| Item | Root cause (one line) | Status |
|---|---|---|
| 1a. 3 Mac→devbox letters stuck `forged_self` | Envelope `from: brief-app` names a room that is a **real local room on the devbox** (`~/Code/hq/tools/cos/brief-app`; hq is checked out on both hosts, so both registered `brief-app`). The anti-forgery rule refuses it correctly, and a refused letter has no terminal state on the sender side. | Verified |
| 1b. 3 devbox letters `outbound_unrelayable` | Local-only probe letters with free-form `--from lane/foo-123` etc. Outbound selection validates `from` **before** checking whether the recipient routes to a peer, so letters that never needed relaying are flagged forever. | Verified |
| 1c. health `ok:true` | `ok` is a liveness/safety flag. It ignores receiver quarantines, unrelayable letters, and sender-side `outbound_waiting` (which has no health field at all). No `post` command reads bridge state beyond freshness and capabilities. | Verified |
| 2. `room_retired {trey, cos}` every tick | The devbox's **channel** deny list contains `cos`, and denied channel names are also dropped from **room** publication. The Mac's `cos` placeholder therefore reads as "retired", and `room_retired` is not in the dedupe set. | Verified |
| 2. `route_contested` 35k, `quiet` 20k | route_contested (and Mac-side unknown_envelope_keys/outbound_waiting) is **historical**: all of it before 2026-09-23 08Z, zero since. `quiet` is one line per quiet tick by design. | Verified |
| 2. `outbound_typed_skipped` | **Not per tick.** It is once per letter via an exclusive marker, but every local participant letter costs a log line and a permanent marker file. | Verified (the premise was wrong) |
| 2. config `.bak-*` litter | No code writes them. They are hand backups made before editing the `peers` pin lists, and every pin is now redundant with v2 `rooms.json`. | Verified |
| 3. Channel count mismatch | Not lag and not the deny list. channels.py:2110-2112 **silently skips** local messages whose `from` is no longer a real local room. After the 9/23 room renames, all pre-rename posts by the renamed names (and devbox `trey`/`hq` posts) can never publish. All traffic since 9/25 converges. | Verified |

---

## Item 1a: the three `forged_self` letters

### Facts (verified)

- **Envelopes.** The devbox quarantine copies (`~/.claude-mail/bridge/quarantine/mac/{hq-devbox,cos-devbox}/<id>.mail`) all carry `"from": "brief-app"`, `sender_provenance: participant-binding`. 1abc11 and 1a1516 were sent 2026-09-17 ~21:00 CDT; 2f27c3 was sent 2026-09-23 18:12Z.
- **Devbox today.** `post rooms --json` lists `brief-app` → `/home/trey-agent/Code/hq/tools/cos/brief-app`, a **real local room**. `brief-app-mac` is a placeholder at `remote/mac/brief-app-mac`.
- **Mac today.** `brief-app` → `~/.claude-mail/remote/trey/brief-app` (placeholder homed at `trey`); `brief-app-mac` → `~/Code/hq/tools/cos/brief-app` (real). `bridge/rooms/owners.json`: `brief-app → {host: trey}`. The Mac's `rooms.json` mtime is 2026-09-23 14:36 local, the same minute as the devbox's `rooms.json`. That is the coordinated 9/23 rename recorded in bead post-4hy/post-6ep, and `brief-app` is one of the seven names the Mac vacated.
- **Binding rule.** snapshot.py:133-140: if the sender's folded name is a real local room, the verdict is `FORGED_SELF` (or `NAME_COLLISION` only when contested). sweep.py:1150-1172 quarantines on that verdict. The v1 fallback is sweep.py:1155-1157. The Mac log shows the receipt reason was `forged_from` (v1) on 2026-09-20. The devbox rewrote it to `forged_self` at 2026-09-23T19:33:27Z when v2 went live (receipt on `origin/machines/trey:receipts/mac/hq-devbox/20260918-015518-1abc11.json`).
- **Why it never ends.**
  - The receiver re-reads and re-judges every entry in `machines/mac:outbox/trey/` on every full tick (sweep.py:1019-1172); quarantine and receipt writes are idempotent (sweep.py:764-793, 798-825).
  - The sender prunes an outbox entry **only** on a `delivered` receipt; any other status logs `outbound_waiting` and `continue`s (sweep.py:1639-1649). There is no expiry, no bounce, and no notice to the sending room.
  - The log line is deduped to one (logdedupe.py:30). The only persistent traces are `log-conditions.json` and the `standing: {"outbound_waiting": 3}` field on every Mac `health` **log line** (sweep.py:2475-2478), never in `health.json` itself.
- **Why no config change can fix it.** `from` lives in immutable bytes, and `brief-app` now correctly belongs to the devbox on both hosts. Making these deliver would mean the devbox trusting `mac` for a name it owns, which is the forgery hole the rule exists to close.

### Root cause

Room names are host-local, yet the envelope `from` is used as a cross-host identity. hq is checked out on both machines, so any subdirectory registered as a room on both gets the same name on both. The receiver's anti-forgery check behaved correctly. The defect is that a **terminal refusal has no terminal state on the sender**: nothing converts a `quarantined` receipt into something the sending agent, `post doctor`, or `health.ok` can see.

(Inferred: the 9/17 letters predate v2, and v1 had no collision detection. By 9/23 v2 flagged `room_name_collision` and the rename followed about 24 minutes after the third letter.)

### Recoverable? Yes, out of band. Procedure (described only, not run)

The bytes are intact in three places: the Mac `~/.claude-mail/archive/<id>.mail`, the relay branch `machines/mac:outbox/trey/<room>/<id>.mail`, and the devbox forensic copy. (The Mac's placeholder inbox copy was tidied at publish, per `derive_published_and_tidy`.)

1. **Decide relevance.** The two hq-devbox notes ("sync hazard", "brief-app transfer landed", 9/17) are probably moot. The cos-devbox letter to Lumen (Daylight "top 3" layout field, 9/23) likely still matters. Trey or the recipient decides.
2. **Deliver by re-sending, not by moving the quarantine file.** From a Mac session in `brief-app-mac`: `post send <room>` with the original body and a one-line "re-send of <id>, refused as forged_self". The new id's sender `brief-app-mac` is published by mac and is a mac-homed placeholder on the devbox, so it binds VERIFIED. Do **not** hand-copy the forensic file into the devbox inbox: its `from: brief-app` resolves on the devbox to the devbox's own brief-app room, so replies would go to the wrong agent.
3. **Retire the stuck outbox entries.**
   - Pause the Mac bridge (boot out `com.treygoff.post-bridge`).
   - In `~/post-relay` on `machines/mac`, `git rm` the three `outbox/trey/{hq-devbox,cos-devbox}/<id>.mail` by name and commit.
   - Resume. `queued_work` sees HEAD ahead (sweep.py:1659-1685) and pushes.
   - This is safe because `bridge/published/<id>` stays (selection skips at sweep.py:1430), and `brief-app` is now a placeholder on the Mac (selection skips at sweep.py:1473-1478). Neither guard will let the letters re-export.
   - On the devbox's next full tick the entries vanish and the conditions clear (`condition_cleared`).
   - Harmless leftovers, since nothing GCs them: the devbox forensic copies, the three `receipts/mac/...` files on `machines/trey`, and the stale 9/16 `post-devbox/20260916-150729-7606cf` forensic copy (that letter was delivered 5 minutes after quarantine).

## Item 1b: devbox `outbound_unrelayable` (since Sep 1)

- The three letters are local probes: `from` values `lane/foo-123`, `lane/distress-probe`, `distress/distress` (`sender_provenance: declared-flag`), addressed to `hq-devbox` and `distress`, both real **devbox-local** rooms. They never needed the bridge.
- In `select_outbound`, sweep.py:1446-1451 calls `parse_envelope`. That validates `from` at sweep.py:723 → `validate_path_component`, which raises "must be one path-safe component" at common.py:306. This happens **before** the route lookup at sweep.py:1454-1472, which would have discarded them as "no peer route". Result: `outbound_ignored` plus the health `outbound_unrelayable` list.
- There is no skip marker for local letters (unlike `typed-skipped/`), so all three are re-read every full tick, forever.
- Post-side gap: `post send --from` accepted a `/` on Sep 1. I did not check current post behaviour.
- **Fix:** route first. Use `outbound_header`'s `to` to find a peer route, `continue` if there is none, and apply `parse_envelope` only to letters that will actually be exported. Then `outbound_unrelayable` can only name mail that is really stuck.

## Item 1c: why `ok:true`, and the observability contract

- `write_health` makes `ok` false only for a local_held store fault, `git_failed`, room collisions, channel diverged/rewritten, or stale fetch (sweep.py:1945-1967). `quarantined` and `outbound_unrelayable` are written as fields and never gate. The sender side has **no field** for peer-refused or waiting letters.
- On the `post` side:
  - `src/commands/doctor.rs` never reads the bridge (it has no bridge references at all).
  - `who.rs` does not read it either.
  - `delivery.rs` covers participant mail only; workspace letters return `state=unsupported` (schema.rs:165).
  - Only `send.rs` and `rooms.rs` read health.json, and only for freshness and capability gating (`bridge_topology.rs:171-228`).

### Proposed contract

1. **Keep `ok` as liveness/safety.** Add a bounded `attention` list (≤20) plus `attention_count` to `health.json`. Entries look like `{id, dir: in|out|chan, peer, room|channel, state, reason, since}`, built from data the tick already computes:
   - receiver quarantines;
   - sender receipts that are not `delivered` (from `prune_outbox`);
   - true unrelayables;
   - channel messages skipped at channels.py:2110;
   - `sender_not_homed` holds older than N minutes.

   Quiet ticks carry the list forward, as they already do for the other fields.
2. **Bounce on terminal refusal.** When the sender sees a terminal receipt (`quarantined`: forged_self / name_collision / unpublished_sender / unknown_room ...), write one system letter into the **sending room's** local inbox ("your letter X to Y@trey was refused: forged_self, sender name brief-app is a trey room; re-send as brief-app-mac"), then move the entry to a terminal `outbound-refused/<id>` marker and `git rm` it from the outbox. The agent who sent it learns through the mail tool itself. This is the root-cause fix for "permanently stuck and invisible".
3. **`post doctor`.**
   - Read `bridge/health.json`. Stale or missing on a host with `bridge/config.json` is an error.
   - Each `attention` item is a warning finding with an exact fix.
   - `post who` shows a one-line "bridge: N letters need attention".
4. **`post delivery <id>` for workspace mail.** Have the sender bridge write `bridge/outbound-status/<id>.json` in the same shape as `pmail-status`/`pmail-acked` (published → receipt), so `delivery.rs` reuses its reader instead of returning `unsupported`.
5. Fold in post-qr6: advertise live caps (`max_mail_bytes`, envelope cap) in `health.json`. Today pmail.py:1551-1555 advertises only capabilities, `ticked_at`, and `interval_s`.

## Item 2: log noise and litter

- **`room_retired {trey, cos}` (verified).**
  - The devbox `config.json` (mtime 2026-09-24 19:38 CDT = 9/25 00:38Z, matching "since 2026-09-25") has `channels.deny = [devbox-build, litigation-work, wade-overnight, cos, cos-urgent]`.
  - rooms.py:447-455 drops denied **channel** names from **publishable rooms**. That is by design (SPEC-v2 line 141), so the devbox's real room `cos` (`~/Code/cos`) left its `rooms.json`.
  - The Mac still holds the placeholder `remote/trey/cos`, so it lands in `retired` (rooms.py:437-446) and is emitted every full tick (rooms.py:547-548; SPEC-v2:214 says "once per tick"). `room_retired` is absent from `CONDITION_ACTIONS` (logdedupe.py:23-35).
  - Routing is unaffected: the placeholder still claims the name (rooms.py:395-397), so Mac mail to `cos` still routes. It is a false state plus about 1,300 lines a day.
  - **Fix:** (a) stop conflating the channel deny with room publication; if room-name privacy is wanted, give it its own `rooms.deny`. (b) Add `room_retired` to `CONDITION_ACTIONS`, keyed by `(host, room)`.
- **`route_contested` 35,486 (devbox) / 22,050 (Mac), `unknown_envelope_keys` 33,456, `outbound_waiting` 7,769 (Mac).** These are all in `log.jsonl.1`, all from before 2026-09-23 08Z (devbox) or from 9/20-9/23 (Mac). That was before log dedupe and before the 9/23 renames cleared the contests. Zero since.
  - Why the numbers mislead: rotation happens only at 10 MB (`LOG_ROTATE_BYTES`, sweep.py:153-157), so the `.1` file can hold days-old noise. (The devbox rotated at 22:16Z today, mid-probe.)
- **`quiet`:** one line per quiet tick (sweep.py:2295), about 4,000 a day per host. `health.json` already carries `ts`/`quiet_streak`, so this is a redundant heartbeat. **Fix:** don't log quiet ticks.
- **`outbound_typed_skipped`:** once per letter (pmail.py:915-939, exclusive marker `bridge/typed-skipped/<id>`), not per tick. `to_host: null` is the normal case for a **local** participant letter. Each one still adds a log line plus a permanent marker (Mac 20, devbox 282). **Fix:** local typed letters are not bridge business; skip them silently. Better, fold them into the single "decided" marker below.
- **Double logging:** every `emit` also `print`s to stdout (sweep.py:151), so `launchd.log` (Mac: 2.4 MB plus an 11 MB `.1`) and journald duplicate `log.jsonl`. Each line is also opened, fsynced, and closed individually (sweep.py:158-166). **Fix:** drop the stdout print (keep stderr for failures) and batch log writes per tick.
- **Config `.bak-*` (Mac ×5 plus `chan-tip-fc.bak-20260925`, plist `.bak`/`.retired` in LaunchAgents).** Grep finds no writer in the bridge, the installer, or post. They are hand backups before edits of `peers` pins (8/27, 9/12, 9/17, 9/23, 9/25).
  - Every Mac pin is already in the peer's published `rooms.json` (checked trey, fc, and sol: zero unpublished pins).
  - A pin is also a hazard: a pinned name registered elsewhere is config-fatal, exit 2 (rooms.py:562-566).
  - **Fix:** delete pins from both configs so `config.json` becomes static (host, relay_url, channels) and nobody edits it by hand again.
- **Stale Xcode-license lines in the Mac `launchd.err`:** last written 9/21, before the 9/23 plist change. The wrapper still `exec python3` under the launchd `PATH=/usr/bin:...`, which is the CLT shim. It works today but is fragile; pin the interpreter path in the plist or wrapper.

## Item 3: channel counts differ

Not lag and not the Mac deny list: none of build, astra, or atlas-ts7 is denied.

Across **all** channels:
- Mac-only: 826 messages (544 in denied channels, expected). Devbox-only: 314 (172 denied).
- **Every non-denied gap predates 2026-09-25**; zero divergence in new traffic.

Senders of the non-denied gaps:
- Mac-only: `atlasos` 189, `agent-memory` 61, `papercuts` 14, `dwp-portals` 10, `deslop-tooling` 8. Exactly the Mac names vacated in the 9/23 rename (now `-mac` real rooms; the bare names are placeholders homed at trey).
  - build: 51 = 37 `atlasos` + 14 `papercuts` (Aug 4-18).
- Devbox-only: `trey` 107 (Trey's Porch posts; the devbox renamed its `trey` room to `trey-devbox`, so `trey` is now a mac-homed placeholder there), `hq` 32 (`hq` is a mac placeholder on the devbox; the real one is `hq-devbox`), `sol-devbox` 2.
  - astra +6 and atlas-ts7 +1 are all `from: trey`.

Mechanism (verified): `publish_channels` does `if envelope["from"] not in snapshot.real_rooms: if not _roomless_sender(envelope): continue` (channels.py:2110-2112). There is no log, no `unpublishable` count, and no health signal. The Mac's first `chan_published` was 2026-09-25T00:40Z, after the rename, so these posts were never eligible. Even if published, the peer would judge them forged_self, because it owns those names now.

**Fix:**
1. Count the skip as `unpublishable` with reason `sender_not_local` and put it in `attention`.
2. Author channel posts either as a room the host owns or as a roomless participant (`from == from_participant`, the r6.3 path at channels.py:269-283). In particular, Porch on the devbox should not post as bare `trey`.
3. Leave the historical divergence alone (ids are immutable), or ship a one-off "rehome history" tool only if Trey wants the old posts mirrored.

## Item 4: open beads

- **post-qr6** (caps not advertised): confirmed. pmail.py:1551-1555 advertises no caps. Fold into the health contract (1c.5).
- **post-7df** (oversize pmail blob re-hashed every tick): consistent with pmail.py:706-709 → `classify_entry` per tick. Same class as the per-tick re-judging of quarantined workspace letters (sweep.py:1105-1172). **General fix:** memoize terminal verdicts by blob oid in a once-file, and skip re-judging when the oid is unchanged.
- **post-9xf** (O(n) per tick): confirmed and wider than filed. Per full tick:
  - `delivered_id_exists` globs `delivered/*/*/<id>` per archive letter (sweep.py:1386-1388).
  - Every local letter is re-opened, hashed, and checked against its local-held record (sweep.py:1415-1452, localheld.py:822-836).
  - `publish_channels` opens and parses **every** channel message before checking `chan-received` (channels.py:2089-2109).
  - Mac scale: 1,040 archive letters and 8,732 channel files. Full ticks are about 28% of ticks. Measured fetch→health: median 3 s, p90 5 s, max 803 s (Mac); devbox median 1 s, max 41 s.
  - **Fix:** one durable "decided" marker per archive id (`exported|local|typed|unrelayable|refused`) or a time-sortable high-water mark, so selection touches only new ids. In channels, check the `chan-received`/published state before `open_regular`.
- **post-4hy** (rename needs a hand edit of `owners.json`): confirmed as the upstream of items 1a and 3. A rename on a bridged host also needs to (a) report or bounce in-flight outbox letters and pending receipts from the old name, and (b) state that old channel posts under the old name stay host-local.
- **post-6ep** (empty `<root>/<old>/{inbox,read}` reappear): **creator identified.** rooms.py:557-560 `ensure_dir`s `<root>/<room>/inbox` and `read` for every routed placeholder whose registration matches. post keeps every room's mailbox at `<root>/<name>/` (post `src/channel.rs:44`, `src/cursor_state.rs:859`) and writes a canonical inbox copy for sends to a placeholder (localheld.py docstring; tidied by `derive_published_and_tidy`, sweep.py). After the rename each vacated name became a trey-homed placeholder, so these are that placeholder's real mailbox, not orphans. The `remote/<host>/<name>` path is only the workspace path. **Fix:** document it and add a doctor info line. Deleting the directories would be undone next tick.

## Item d: design traits that will keep producing papercuts

1. **Host-local names used as cross-host identity.** hq and other repos are checked out on both hosts, so whenever a subdirectory gets registered as a room on both, the brief-app collision recurs, followed by stuck mail and silently unpublishable channel history. Fix at registration: on a bridged host, `post rooms add` should default to a host-qualified name, or refuse a name any peer publishes (schema.rs:105 already refuses case-folded duplicates of *existing placeholders*, but not of a peer's name not yet placeheld).
2. **Terminal states have no terminal handling.** This covers quarantine, unrelayable, channel skip, and oversize pmail. Each is re-evaluated every tick, logged once, counted nowhere that gates health, and never bounced or retired.
3. **Unbounded append-only state with no GC:**
   - `local-held/` ("no release, no GC", localheld.py:1-13): Mac 879 records plus a 1,758-line index; devbox 398.
   - `typed-skipped/`, `published/`, `received/`, and `delivered/` markers.
   - `receipts/` on relay branches: 80 on trey, 72 on mac.
   - Quarantine forensic copies.

   Each also adds per-tick work (point 4).
4. **Per-tick O(all history) work.** See post-9xf above. It grows linearly with mail and channel volume forever.
5. **Log design.** A per-line fsync, a duplicate stdout stream, a single 10 MB `.1` rotation, and heartbeat lines for quiet ticks. Emitters outside `CONDITION_ACTIONS` (`room_retired`, and any future per-tick state) repeat every tick.
6. **Channel deny doubles as room privacy.** Denying a channel silently unpublishes a same-named room and creates false "retired" state on peers.
7. **Hand-edited bridge state as an operating procedure** (`owners.json` release, pins, `.bak` files). Every manual step is a future miss (post-4hy).

## Probe hygiene note

All reads. One slip: an `scp` of a sender-id list to the devbox's `/tmp/onlydev-lane.txt` ran once and was deleted by the next command (`rm -f`). Nothing else was written outside this scratchpad.
