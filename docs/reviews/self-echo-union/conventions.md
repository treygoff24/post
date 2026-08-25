# conventions — self-echo-union

_lane: conventions:codex:luna · alias codex-10 · exit 0 · 1539 bytes · review-shaped: yes_

=== completionReport ===
CONVENTIONS: clean | 2 findings

[RULE] Union suppression silences legitimate observers that select rooms they do not own · `src/commands/watch.rs:208-211,762`  
Rule: `CONTRACT.md § Commands`  
`watch-notice` forwards arbitrary repeated `--room` values (`skills/post/hooks/watch-notice.mjs:221-226`), and the contract presents watch as a notifier for any harness monitor. A process in room `gamma` watching `alpha` and `beta` will now suppress every channel message sent by either room, even though `gamma` is the observer.  
Fix: derive self identity from `POST_FROM`/cwd (or add an explicit acting-room option) and suppress only that identity; keep selected targets for scanning and deduplication.

[SLOP] The commit message overstates verification · `commit message:7`  
Rule: `~/.claude-shared/CLAUDE.md § Web research`  
The parent implementation already deduplicated a third-party `gamma` message via `emitted_channel_ids`, so the rewritten dedup assertion would pass before this commit. Only the own-send assertion is demonstrably red. The “five self-rings in one hour” measurement is also unsupported by repository evidence.  
Fix: state only the verified regression, or attach reproducible evidence for both claims.

No additional defects found: dedup coverage is real; snapshot and long-watch share one union; skipped snapshot targets are consistently excluded; the schema sentence is accurate. `git diff --check HEAD^ HEAD` passed. The full gate was not run per the read-only/no-build-test instruction.
