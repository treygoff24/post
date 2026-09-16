# Post participants + lineages — overnight build, 2026-09-16 (Fable + Astra)

## Outcome
The participant identity model is built, reviewed, gated, installed on both machines, and proven live with the exact case you asked for: Fable and Astra, two agents in the same repo, each bound as itself, exchanging mail that only the right one sees. The explainer is open in your browser. Astra's explainer (`docs/visual/index.html`) carries a receipts table where every row is a verified pass that names its artifact, plus the full record in `docs/visual/assets/acceptance.json` (build hashes, the live message ids, known limits, the routing footprint of the live tests). Her commit 17e8675 merged onto main as **a7667c7** (docs-only; runtime paths byte-identical to a500bd2) and is served from both hosts.

## What is installed
- Runtime source SHA **a500bd22121aa6bcce7e8942dd2a087332ed4403**; skill/docs SHA **f1e3eae3e51938568b574d88833471919ade20d4**; final main **a7667c7** (explainer receipts on top). Docs-only commits after a500bd2; zero runtime diff between them, asserted after every merge. All pushed to Forgejo; devagent checkout at a7667c7.
- Installed runtime = main's runtime: at ship time I re-verified both hosts report build a500bd2 and that main a7667c7's runtime paths (src, hooks, scripts, tests, launcher, Cargo) diff against a500bd2 by zero lines, so no rebuild was run; rebuilding would only relabel `build_sha` and invalidate the binary hashes the explainer's receipts cite.
- Mac `~/.local/bin/post` and devagent (trey-agent) `~/.local/bin/post`: build a500bd2, store v2, capabilities participants / lineages / routing-receipts / cursors-v2. Adapters refreshed and hash-verified (Mac: Claude, Codex, Cursor, Grok; devagent: Claude, Codex, Grok — Cursor has no adapter there, left alone). Skill served from the checkout on both hosts, every file hash-checked against f1e3eae. Old binaries backed up (`/tmp/post-before-participants.64fZu4/post`, `…PUZmLT/post`). Root-owned `/usr/local/bin/post` untouched.

## Proof it works (the problem case)
- Fable = `claude-e2ef843c`, Astra = `codex-75ce6b09`, both bound to workspace `post-repo` through their own harness keys, no impersonation.
- A third fixture sender wrote **1582d5** to the workspace: the frozen receipt named exactly the two of us; Astra read it first; it was still unread for Fable afterward; Fable read it; the file and receipt never changed (sha256 + inode); both cursors hold the id.
- Astra's workspace send (**83a707**) reached only Fable; Fable's (**268f74**) reached only Astra. Both installed hooks rang in the real sessions.
- Cross-host: Mac→devagent **7606cf** delivered and read (after I caused and then reverted a topology mistake — details below); devagent→Mac **21af36** through the configured `claude-space` route delivered and read with `origin: remote`.

## Gates
- Full repo gate at a500bd2: 536 cargo tests, clippy/fmt, release, node hook + launcher suites, python doorbell, schema; `tests/acceptance.sh` under the strict build-sha expectation 34/34. Astra's installed-binary smoke: 34/34 on both hosts; installed adapter fixtures green; 17 edge cases / 96 assertions.
- Reviews: every lane head reviewed by Astra (herdr, in your voice) plus an independent second-family reviewer through delegate that varied by lane (Opus on P.2 and P.6, GLM on P.4); rulings mailed to running lanes with verified delivery; nine P.2 rounds, six P.6 rounds, one P.5b recheck round with twelve integration corrections.

## Things you should know (honest limits)
1. **Install order slip.** I installed with the runtime SHA's skill docs before the docs lane landed, so for ~50 minutes both hosts served a SKILL.md that still advertised a removed flag. Astra caught it; no rollback (binary was right), docs fixed by landing P.5b and fast-forwarding the served checkouts. Runtime checks stayed green; effects on other sessions in that window were not assessed.
2. **Bridge topology mistake.** I registered a `post-repo` placeholder on devagent to test the reverse direction; the bridge routes only peer-config rooms, so that quarantined the Mac→devagent message as `forged_from` and left the reverse message (**9cc7f1**) unsent. I reverted my own addition (backup kept), the quarantined message delivered on the next tick, and 9cc7f1 stays preserved as UNSENT. No bridge config was changed.
3. **Incidental routing.** Binding a fixture participant in a workspace freezes receipts for that room's pending legacy mail to the fixture (by design). This happened once on devagent (1 item) and once on the Mac in `claude-space` (13 items). Nothing was consumed or deleted; ids are in `STATE.md` and `acceptance-evidence-index.md`.
4. **Idle doorbell is still workspace-aggregate** (`codex-notify-monitor`); participant-scoped idle wake is a follow-up (bead post-pe2). Cursor on devagent has no Post adapter (disclosed, not installed).
5. **Follow-up beads left open on purpose:** post-pe2 (doorbell), post-782 (bridge logs unknown v2 envelope keys), post-hox (R26 re-read), post-86k (receipt scan cost), post-gcs (adopt snapshot), post-40n (identity continue digest).
6. **Author-metadata incident.** My merge rehearsal ran `git config user.name/email rehearsal` inside a worktree, which writes the shared `.git/config`, so every commit from 37185ba through e3d35e5 — including delegate-lane commits made in that window — is authored `rehearsal <rehearsal@local>`. I found it myself while checking the local git config before the P.4 merge, disclosed it to Astra, unset the override, and authorship is you again from the P.4 merge on. History was not rewritten (no amends, no force-push); the wrong author stays in pushed history and is disclosed here and in `acceptance.json`. Separately: delegate mail returns ok:true even when a recipient is ineligible (papercut filed); one lane round was lost to that before I started verifying delivery outcomes.

## Where the record is
- `STATE.md` (sitrep, chronological), `thoughts/shared/handoffs/2026-09-16-overnight-participants.md` (this report), coordinator scratchpad `…/7ed9a15b…/scratchpad/` (logs, freeze.sh, evidence), Astra's `/tmp/post-papercuts-2026-09-15/` (reviews, installed receipts, `acceptance-evidence-index.md`), `docs/visual/assets/receipts.js` + `acceptance.json` (the explainer's receipts).

## Rulings I made for you
- R28 lease preservation (rebind/touch keep the recorded lease), §4 rewritten at integration.
- Repair wording law: docs and errors say restore/repair, never advise deleting a participant record or directory.
- Historical records (signed IDENTITY paragraph, CHANGELOG 0.5.0, review artifacts) stay contemporaneous; supersession lives in banners/Unreleased.
- Parked Opus H2 (bead) rather than reopen the lane after Astra closed review.

## Browser
- Astra opened the final main path in your real Chrome (tab 1356725133, title "One cursor per room — what changed in Post", `open` exit 0, main a7667c7); receipt `/tmp/post-papercuts-2026-09-15/visual-browser-open.json`; her headless render of the same files shows "Acceptance passed", 12/12 rows, no overflow (`/tmp/post-visual-qa-20260916/final-main-open.png`). Aside was unavailable (its daemon was not connected to your browser profile), so I did not attach to or relaunch anything; I verified the exact tab read-only through AppleScript's tab list — URL `file:///Users/treygoff/Code/post/docs/visual/index.html`, title "One cursor per room — what changed in Post" — and viewed Astra's screenshot: green "ACCEPTANCE PASSED" badge, "All 12 acceptance checks passed, with evidence recorded in each row".

