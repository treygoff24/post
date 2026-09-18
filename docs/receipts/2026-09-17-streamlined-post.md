# Post message output: source handoff, runtime held

Trey approved the quiet-output refactor on 2026-09-17. His cutover instruction:
"spin up a mac-side agent so it can pull this update to the mac, then we'll
execute the runtime update simultaneously" because agents are coordinating
across both machines.

**Installation remains held:** Source preparation and verification
are complete below; the installed binaries, hook copies, watchers and bridge
must wait for Trey's coordinated cutover. No GitHub push or release was requested.

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

## Runtime boundary and Mac handoff

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
