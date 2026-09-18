# Post message output: deployed on devbox

Trey approved the quiet-output refactor on 2026-09-17. His cutover instruction:
"spin up a mac-side agent so it can pull this update to the mac, then we'll
execute the runtime update simultaneously" because agents are coordinating
across both machines.

**Hold lifted:** On 2026-09-17 at 21:38 CDT, Trey said all agents had moved to
the devbox and authorized: "full send just go ahead and do it". The active
devbox binary and existing Post hook scripts are now deployed at `c2feda0`.
No Mac deployment, GitHub publication or agent-session restart occurred.

## Changes

- One short activation notice per participant, delivered by direct CLI binding
  or the Claude, Codex, Cursor and Grok adapters. The persistent acknowledgment
  survives rebinds, resumes, a new day and lost hook caches.
- Default text reads use sender/time/id/reply headers and guttered bodies.
  Reply references, subjects, event labels and signature status are conditional.
  Provenance explanations and daily banners are gone from ordinary output.
- Default JSON retains canonical IDs, bodies and routing metadata, but omits
  `framing.laws`. Message files and bridge transport are unchanged.
- Channel notifications use `[post] #channel: N new`; routine inspection
  instructions no longer repeat. Operational errors remain visible.
- Existing sessions export `POST_FRAMING=compact`. That legacy environment
  value now selects quiet output without restarting those agents. Explicit
  `--framing full` and `--framing compact` still request diagnostic banners.

Channel text uses prefixes unique across the whole channel, including messages
outside the displayed page. Other reads retain full IDs when they lack a
complete reference namespace. Numeric timezone offsets remain visible.

The activation query/ack protocol records delivery after successful injection.
A process crash between delivery and acknowledgment can replay the notice;
failed output never silently consumes it. Queries remain read-only, and an
unbound acknowledgment refuses before initializing the mailbox.

## Verification

- RED: `1eb3329` added failing quiet-read and once-per-participant tests.
- GREEN: `c948195` implemented the presentation and activation changes.
- A targeted RED/GREEN test caught and fixed inherited compact-environment
  behavior, so existing sessions adopt quiet output at binary cutover.
- `7d44409` adds the unbound acknowledgment guard after a failing regression
  test. All five `tests/streamlined.rs` cases pass, including actual CLI-backed
  runs of all four hook adapters, failed stdout, resume and lost hook cache.
- Final canonical gate passed at `7d44409`: 544 Rust tests, 285 hook tests,
  34 launcher tests and 41 doorbell tests; fmt, clippy, release build and schema
  checks also passed. Local log: `/var/tmp/post-verified-gate.log`.
- A separate isolated CLI smoke exercised two participants, a channel send,
  a quiet read under the inherited compact environment, and a silent rebind.
  Output was inspected; no live mail was used.

Gate environment: native Cargo 1.97.1, Node v26.8.2, Python 3.13.15; build
artifacts stay under the existing managed Cargo cache. The estate Cargo wrapper
breaks the existing metadata-only target-directory tests by overriding their
fixtures. Native Cargo fixes that test environment without changing the
wrapper or those tests. Papercut: `pc2_0a98f361fa34caa4`.

The gate covers fmt, clippy, Rust tests, release build, hook and launcher tests,
Python doorbell tests and schema output. No numerical coverage measurement was
produced, and macOS execution is not verified here.

## Original runtime boundary (superseded by the deployment below)

The source is on Forgejo `origin/main` after closeout. The Mac agent should
inspect its checkout and pull that source without installing it. Coordinate
installation of the binary and the shipped adapter copies on both hosts only
after Trey resumes the cutover. Preserve active conversations, Post participant
IDs, watchers and bridge state; no store migration is part of this refactor.

The devbox installed binary remained at SHA-256
`7489f91889408f1a488ada01f2a9299020b30f6e9b8d9a219a414719faaa9acb`.
No live hook installer or process restart ran. The installed Codex, Claude and
Grok hook files were compared byte-for-byte with pre-refactor `7f3d1a2`; all
three still match that old source. The local Post skill is a symlink
into this repository, so its documentation reflects the source changes already;
that is not a binary/hook cutover.

Bead: `post-49f`. The pre-existing Beads JSONL/Dolt mismatch still causes the
CLI to refuse auto-export; no ledger import or overwrite was attempted. This
receipt carries the cross-host handoff independently of that local ledger issue.


## Devbox deployment

Deployment bead: `post-inq`. Source build `c2feda0` is installed at
`~/.local/bin/post`. SHA-256:
`1c8179a93ab18ee83d161bf84c331f6e0c7296b040f24fa8603d5d412a2cd3dd`.

The existing Codex, Claude and Grok adapters and Grok watch-notice helper now
match the source files byte-for-byte. Active Codex/Claude profile configs share
these adapter paths. Hook registrations were preserved. No Cursor Post hooks
were installed previously, so deployment did not add a new integration.

The first installation exposed overlapping hook calls issuing duplicate
activation notices. `a135b9a` reproduced the failure on all four source adapters
(4-6 notices for six concurrent events). `c2feda0` adds a PID-owned reservation
under the participant registry lock, with release on success/failure and
reclamation when the owner exits. Failed-output and lost-cache tests still pass.

Verification at the corrected build:

- Canonical gate passed: 545 Rust tests, 289 hook tests, 34 launcher tests and
  41 doorbell tests, plus fmt, clippy, release build and schema.
- Installed smoke passed with `POST_SMOKE_EXPECT_BUILD_SHA=c2feda0`.
- Six simultaneous events against each installed Codex/Claude/Grok adapter
  produced one notice; repeats and a fresh hook cache produced none.
- A non-consuming live `#astra` peek rendered quiet headers under the inherited
  compact environment. No message bodies were persisted in the receipt.
- All installed file hashes match the rollout manifest.

### Evidence and rollback

Local evidence: `/var/tmp/post-deployment-gate.log`,
`/var/tmp/post-runtime-final-smoke.log`, and
`/var/tmp/post-runtime-final-receipt.json`.
Both swaps used same-directory temporary files and atomic rename, preserving
loaded executables and existing sessions. Rollback files were retained at:

- `~/.local/state/post/rollbacks/20260918T024243Z-quiet-output/` (original runtime)
- `~/.local/state/post/rollbacks/20260918T025431Z-activation-claims/` (first quiet build)

Each directory has a manifest with target paths and before/after hashes. No
watcher, bridge or agent process was restarted. A pre-existing D-state Post
process was left alone on its original inode.

Pre-existing conditions, unchanged by deployment:

- `/usr/local/bin/post` is an older root-owned fallback. Noninteractive sudo
  requires a password; it was not replaced. Active agent PATHs and installed
  hooks resolve the updated user binary instead.
- Live doctor still reports five findings: missing workspace for
  `dwp-portals-phase1`, and missing inbox/read directories for `fable-devbox`
  and `warrant`. The before/after findings are identical; no mailbox repair
  was folded into this deployment.
- The Beads JSONL/Dolt auto-export mismatch remains as recorded above.
