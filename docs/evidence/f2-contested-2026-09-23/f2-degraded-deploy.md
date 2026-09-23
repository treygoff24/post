# F2 degraded deployment: evidence, 2026-09-23

Bridge v2 (af00a1a) is live on the Mac and the devbox, and it is deployed deliberately degraded (Aster ruling 20260923-063511 in #post-overnight). Both hosts report `health.ok: false, reason: room_name_collision` because eight room names exist on both hosts. Mail to or from any other room flows.

## Collision set

Checked on both hosts after cutover. It is exactly the known eight: agent-memory, atlasos, brief-app, cos-crons, deslop-tooling, dwp-portals, papercuts, trey.

## Contested letters (the files beside this note)

`contested-mac.jsonl` holds 45 letters (Jul 16 to Aug 13): agent-memory 15, atlasos 20, deslop-tooling 1, papercuts 9. `contested-devbox.jsonl` holds 167 (96 from August, 71 from September): agent-memory 14, atlasos 89, brief-app 5, dwp-portals 43, papercuts 14, trey 2. Each row gives the archive id, the room, the archive file's sha256, the local mailbox copies, and any bridge markers. `contested-evidence.sh` regenerates a file from the latest full tick's `route_contested` lines.

Every one of the 212 letters has exactly one copy in its local room's `inbox/` or `read/` and no `published`, `received`, or `delivered` marker. In other words, post delivered each letter locally when it was sent, and none has ever crossed the bridge. The archive bytes are untouched.

## Outbound select trace: a rename would re-export them

`select_outbound` (sweep.py, af00a1a) walks every `archive/*.mail`. It skips a letter only when it has a received, delivered, or published marker. It then routes by `snapshot.route_for(to)`. `routes` holds every uncontested name whose only claimant is a peer. There is no age cutoff and no check for local delivery.

So renaming or deregistering one side of a colliding name makes the other side the sole claimant. The next tick on the renaming host would then select every archive letter addressed to the old name and export it to the peer, which would deliver weeks-old mail a second time. This follows from topology alone. It needs no code change or operator action beyond the rename.

A guard can tell the two cases apart without a blanket cutoff. Legitimately queued remote mail has no local-room copy: the smoke letters below have only an archive file and a `published` marker. The guard is bead post-aqw.9. The rename decision is post-aqw.10, and it is blocked on the guard. Nothing tonight renames a room, deregisters one, or changes an owner.

## Exit codes: Mac 1 versus devbox 0

On a full tick, `execute()` in sweep.py returns `0 if health["ok"] else 1`. A quiet tick returns 0. A tick can be quiet only when the prior health is `ok: true` (`tick.quiet_candidate`). Before the Mac published its v2 `rooms.json`, the devbox had no collision, so its health was ok and it ran quiet ticks that exited 0. Its last quiet tick was at 06:28:42Z. The Mac published its rooms in relay commit bbda6f0 (06:28:51Z), and from the devbox's next tick (06:28:59Z) both hosts were unhealthy.

Since then every tick on both hosts is a full tick that exits 1: the devbox systemd journal shows `status=1/FAILURE` on every run, and the Mac's launchd shows `last exit code = 1`. The earlier difference was a timing artifact, not a parity gap. The exit status honestly reflects the health JSON on both hosts.

While degraded:

- There are no quiet ticks, so every 15 s tick fetches and does a full pass.
- Every full tick logs one `route_contested` line per waiting letter: 45 lines on the Mac and 167 on the devbox.
- `log.jsonl` rotates at 10 MB with one generation, so it stays bounded. The Mac `launchd.log` (stdout) has no rotation and grows until the renames. The devbox journal is capped by journald.
- Relay commits happen only on real changes: 3 on the Mac and 4 on the devbox between 06:19Z and 06:39Z.

## Fresh-nonce smoke on non-colliding rooms

- **Mac to devbox.** `20260923-063954-41f092`, nonce `acc-20260923-f2-mac2dev-ab1168`, sent from post-repo (Nightjar, claude-83e5e99e) to workspace post-devbox.
  - Timeline: outbound copy 06:40:04Z, devbox delivery 06:40:19Z, receipt returned and Mac outbox pruned 06:40:25Z.
  - A fresh devbox participant (shell-a99cb33c) consumed it: `origin: remote`, with the nonce in both subject and body.
- **Devbox to Mac.** `20260923-064215-67e382`, nonce `acc-20260923-f2-dev2mac-89026f`, sent from shell-a99cb33c in post-devbox to workspace post-repo.
  - Timeline: outbound copy 06:42:25Z, Mac delivery 06:42:35Z, devbox outbox pruned 06:42:41Z.
  - Nightjar consumed it, and so, independently, did Aster (codex-5a036212, #post-overnight 20260923-064355). Aster confirmed `origin: remote` and `reply_to_participant: null`, which is expected before F3.

## The historical letter 20260916-150759-9cc7f1

This is not a smoke nonce. It was sent on Sep 16 from post-devbox to post-repo as part of a cross-host acceptance test. The devbox's v1 bridge never exported it, because v1 treated the `address_kind` and `from_participant` envelope keys as unknown (bead post-782) and logged `unknown_envelope_keys` for it every minute. v2 accepts those keys, and once the Mac's v2 `rooms.json` gave the devbox a route to post-repo, the devbox exported it (06:28:59Z), and the Mac delivered it to post-repo at 06:31:39Z. On the Mac it has one inbox copy, a routing record, and received and delivered markers.

It is the only pre-cutover letter exported since cutover, on either host. Aster's copy remains unconsumed at her choice.
