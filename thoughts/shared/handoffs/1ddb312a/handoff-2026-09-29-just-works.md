# Handoff: post just-works fix wave (coordinator session 1ddb312a)

## Trey's standing instructions (verbatim essentials)
- 2026-09-28: "make everything just work, flawlessly ... use sonnet medium subagents for all implementation/building, sol high for reviews, same sonnet subagents for fixes ... no more than 2 review fix rounds. if you feel like it needs more after 2, you review and fix and ship yourself, directly."
- Retire `--anyway` (crossed sends always deliver); move the bridge into the post repo. Both agreed.
- Late 2026-09-28 (he went to bed): "use /writing-for-agents to update the skill when you're done, fully ship this thing to both machines, update hooks etc. everywhere, and then run /done. ... push everything up to forgejo, even tho it's a work in progress ... use ur ssh devagent to pull it down over there for them [Loom agents]."
- Then: write a changelog-type doc and push it ASAP so the Loom agents can build around it.
- Gates: GitHub push/PR/tag/release gated (not authorized). Forgejo origin pushes and local commits ungated. Never amend/force/--no-verify; commit with explicit pathspecs. Tests never touch ~/.claude-mail.
- CPU: keep load under ~16. Post full gates via `/usr/bin/lockf -k /tmp/post-heavy-gate.lock nice -n 10 env CARGO_BUILD_JOBS=2 scripts/gate.sh`. Mender (delegate-agent session) uses /tmp/trey-heavy-gate.lock. Pause if load > 20.

## State right now
- main (Mac ~/Code/post) = integrate-just-works + skill commit 38d3340; pushed to Forgejo at 1fa4a7d (skill commit NOT yet pushed at time of writing).
- Devbox trey-agent ~/Code/post pulled to 1fa4a7d (ff). Binaries not yet installed anywhere.
- Integration worktree: /Users/treygoff/Code/post-integrate (branch integrate-just-works). From now on, merge directly into main in ~/Code/post.
- Lanes (native Sonnet subagents, resume with SendMessage by id):
  - hooks a52ad02158918da84 — done, 2 rounds, merged.
  - surface a0a2502ccb8cdf6c4 — done, 2 rounds + schema follow-up, merged.
  - identity ac5be5df5324a3ad7 — done, 2 rounds, merged.
  - channels a6b775151ba4d5181 — done, 2 rounds, merged.
  - bridge ae0fd9c6192532c37 — ROUND 2 FIXES IN PROGRESS (branch worktree-agent-ae0fd9c6192532c37). Items: .sent needs notice present; full-notice sha256; skip legacy migration; dead-letter attention id = refused letter id; validate existing origin record; prune origin records after retirement. When it reports: merge into main, then I own any further review (2 rounds used).
- scripts/gate.sh now runs bridge/tests/run-all.sh after schema (commit 744e25c).
- Last full gate: 4-lane integration passed (857 rust / 568 node / 34 launcher) before later round-2 merges; clippy+fmt clean on current tree. Need one full gate on final main (with bridge suite).
- Known flaky pre-existing test: tests/cli.rs long_watch_retries_transiently_unparseable_same_generation_state (heartbeat timing under load). File a papercut/bead; don't block.

## Remaining steps
1. Write + push changelog doc for Loom agents (docs/post-just-works-changes-2026-09-28.md), pull on devbox.
2. Merge bridge round-2 branch into main; my own review of its diff; full gate under post lock.
3. Deploy Mac: scripts/install-post.sh (installs ~/.local/bin/post; receipt ~/.local/share/post/install-receipt.json); hooks via skills/post/hooks/install-{claude,codex,cursor,grok}-hooks.mjs; doorbell supervisor via install-doorbell-supervisor.mjs; bridge by hand (copy bridge/sweep.py + bridge/bridgelib/*.py into temp dir under ~/.local/lib, BUILD file commit=<sha> dirty=no, 0755 dirs/0644 files, mv over ~/.local/lib/post-bridge, run ~/.local/bin/post-bridge-sweep --check-config with plist env); boot out launchd dev.post.codex-doorbell.locus if still present. `skill render` for served skill (Mac library symlinks to ~/Code/post/skills/post).
4. Deploy devbox: ssh devagent (trey-agent): pull, cargo build --release, install post to ~/.local/bin, bridge/install.sh with original args (--config at installed $POST_MAIL_ROOT/bridge/config.json, same --interval), hooks installers, supervisor. ssh devbox (trey, sudo): replace /usr/local/bin/post with new build, remove /usr/local/bin/post-doorbell. Deploy devbox bridge FIRST, then Mac (bridge lane advice). Check how devbox serves the skill.
5. participant gc on both hosts: dry run, then --apply. Verify the 3 stuck Mac outbox letters (brief-app → devbox) dead-letter with attention items and retire from relay.
6. Close beads (epic post-i80; post-11n, post-07b, post-box, post-9rj, post-2to, post-b18, post-8pn, post-6ep, post-cq8, post-4th) with reasons; update STATE.md; push Forgejo; release fleet claim (owner post-just-works-2026-09-28).
7. Post a note to the Loom agents (find their room via post rooms) that the new build is installed.
8. Run /done. Final message to Trey per unwatched-runs rules: outcome first, rulings surfaced.

## Rulings made (surface in final reply)
- Kept read-only behavior on a gc-collected explicit claim as an error with `post participant restore <id>` fix rather than a silent empty result (archived records hold state; empty would lie).
- Delegate children: with DELEGATE_RUN_ID and no POST_PARTICIPANT, ambient keys ignored (no identity by default).
- Bridge: 3 stuck letters will dead-letter (no origin record), not bounce to brief-app-mac.
- `--allow-self` retargets to sender's own inbox only (not room union); follow-up if delegate needs union.
- Native Sonnet lanes' effort couldn't be pinned to medium (custom agent def needed session restart).
