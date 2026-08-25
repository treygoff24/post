# refuter — self-echo-union

_lane: refuter:codex:sol · alias codex-11 · exit 0 · 2349 bytes · review-shaped: yes_

=== completionReport ===
Status: completed  
What I found: two reachable issues, led by observer traffic suppression.  
Files reviewed: HEAD commit plus touched watch/schema/test code and relevant contracts, adapters, identity, and CLI resolution.  
Verification: `cargo fmt --check` and `git diff --check HEAD~1 HEAD` passed. Full `./scripts/gate.sh` was not run because safe mode forbids its build/test artifacts.  
Files changed: none; pre-existing `.beads` changes remain untouched.  
Remaining risk: no dynamic test execution.

VERDICT: refuted (2) — a monitor watching `alpha` and `beta` but owning neither silently loses every channel message sent by either room.

[SEV-2] Watched targets are incorrectly treated as watcher identities · src/commands/watch.rs:762  
Counterexample: A bridge invokes shipped `watch-notice.mjs --snapshot --room alpha --room beta`; alpha sends to a channel shared with beta. Explicit room resolution performs no cwd or `POST_FROM` ownership check (`src/mailbox.rs:313`), and the adapter deliberately passes repeated rooms through (`skills/post/hooks/watch-notice.mjs:225`).  
Observed: Alpha’s message is suppressed during both scans, so the observer emits nothing and receives no error. The commit’s claim that “watching a room already means claiming it” is false. (ran: no | derived)  
Fix: Separate rooms being scanned from identities owned by the watcher; suppress only an explicitly established self-identity set.

[SEV-2] Normative and public contracts still describe the old per-room rule · CONTRACT.md:349  
Counterexample: A consumer follows `CONTRACT.md:349`, `README.md:426`, or `skills/post/SKILL.md:201`, which promise only that a sender does not ring its own room’s watch, then watches alpha and beta expecting alpha’s message through beta.  
Observed: Runtime suppresses it across both rooms, contradicting those contract surfaces; only `post schema` documents the widened behavior. (ran: no | derived)  
Fix: If union suppression remains, update every normative/public contract and say “channel message” explicitly.

Attacks that held: the rewritten integration test still genuinely tests dedup—removing shared-ID dedup would produce two gamma events and fail. Snapshot and long-watch paths share the same union; snapshot-skipped unregistered rooms are excluded consistently with “scans nothing.”
