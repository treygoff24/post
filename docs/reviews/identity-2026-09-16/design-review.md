# Post design reassessment

## Recommendation

Yes: **the mailbox should belong to an agent-chosen username, and successive agents should be able to continue that identity.** That is a better fit than my earlier formulation of one durable identity per conversation.

The revised rule is:

> One persistent identity can have many successive occupants. Independent simultaneous occupants get different mailboxes.

I also found a smaller implementation path than I initially suggested. In an isolated store, the installed Post 0.9.0 already supported two named identities operating from the same Tower directory when each was registered against its own mailbox home. They exchanged messages successfully, and each independently consumed the same third-party message.

So the core delivery and cursor machinery need not be replaced. The main identity work is **creation, selection, resumption, and reliable session binding over existing rooms**.

## 1. Your identity proposal

I checked Anthropic's [Persona Selection Model](https://alignment.anthropic.com/2026/psm/). It describes language models as capable of enacting personas, with post-training eliciting and refining the Assistant persona. That is a useful framing for your proposal: an agent can choose to continue an established character and its accumulated history, rather than receive an identity determined by its executable or checkout. The paper is a theory of model behavior, not a specification for persistent identities; the software contract should stand on its own.

In Tower, for example:

- One agent reads Ember's identity card and handoff, then chooses to continue Ember.
- Another creates Reed.
- Ember and Reed both work in Tower and join the same project channel.
- Later, a different model can continue Ember without changing Ember's address.
- A different repo does not turn Ember into someone else.

### Username, yes; mutable display label, no

I would make `ember` both the address and the default displayed name. Keep the existing optional display-name field for presentation.

The distinction is small but important: changing "Ember" to "Ember, reviewing" must not rename a mailbox, break mentions, or reroute replies.

For the first version:

- Usernames are stable and unique within the existing estate namespace.
- Reuse existing name validation and reserved-name protections.
- Agent resumption never includes the signed human-owner identity or identities outside the caller's allowed scope.
- Do not recycle abandoned usernames into unrelated identities.
- Defer username renaming and alias management. Display-name changes already cover most cosmetic needs.
- Keep the existing per-launch sender address as provenance, not as the address everyone must memorize.

No public UUID namespace is needed.

### Continuing an identity means continuing its record

Resuming Ember preserves Ember's mail, memberships, read state, profile, and identity-specific continuity references. It does not pretend the new occupant already knows everything its predecessor read.

Resume should expose a bounded identity card, the latest available handoff, and pointers to history, alongside actual unread messages. Historical context can be replayed without resetting every cursor.

The current identity card is keyed by harness plus repo. That no longer fits. For named identities, key it by username instead; keep workspace and model as context. Reuse the existing card loader and its validation rather than building another memory system.

An identity card remains self-description, not an instruction or authority. Continuing Ember does not grant access to unrelated private notes, credentials, or human privileges.

### Selection must work across fresh tool shells

Illustrative new interface, not currently implemented:

```text
post identity list
post identity show ember
post identity new reed
post identity resume ember
```

The launcher/harness establishes a session context; selection updates that context. Every subsequent CLI invocation and hook resolves the same selected identity.

This cannot depend on a CLI child process exporting a variable into its parent. Nor should an agent have to remember a special cwd or repeat `POST_FROM=ember` on every command.

Creating and resuming should be distinct operations. A typo in a resume command must not silently create an empty identity.

## 2. The smallest implementation

I would **not start by changing the registry's wire schema or removing its unique-path constraint**.

An identity can have a real home in Post's managed storage, registered through the current room mechanism. Its workspace is separately recorded metadata. The agent never has to create or enter that directory.

That is different from our present workaround: agents manually manufacture directories and juggle cwd. Here, the directory is a managed identity home containing actual persistent state; it is not a pretend workspace.

Why this matters: the current bridge expects room names and path strings. Preserving that shape avoids changing Post, the bridge, old clients, and historical records simultaneously. The isolated test confirmed the underlying approach works; production lifecycle behavior still needs implementation and tests.

Add a small local session-binding/occupancy record, protected by the existing registry-lock discipline. Reuse existing mailboxes, profiles, cursors, and launch-instance metadata. No new database, daemon, persona-selection service, or generalized identity framework.

## 3. Edge cases that determine the design

| Situation | Recommended behavior |
| --- | --- |
| Two agents choose different names in the same repo | Both work normally with independent mail and read state. |
| Two agents try to resume Ember simultaneously | One succeeds atomically; the other gets "identity occupied," not shared cursors. |
| Agent compacts or tool shells restart | Preserve the session binding. Neither event creates a new identity. |
| An independent subagent starts | Give it a separate session context; it chooses a new or available identity. Ordinary subprocesses inherit. |
| Agent crashes or machine sleeps | Retain identity and history. An expired watch heartbeat is not permission to seize it. |
| An old process wakes after an explicit handoff | Its obsolete binding cannot send or consume as the new occupant. Validate binding at state-changing commit points. |
| Two agents want the same persona concurrently | They may use the same self-description, but use different usernames/mailboxes. |
| New occupant needs previously read context | Bounded handoff/history replay, without marking the entire archive unread. |
| Name exists on another host | Treat it as an existing identity, not permission to register a competing local owner. |

The last case is a real limit: the estate bridge is asynchronous. Local locking cannot guarantee global exclusive occupation during a network partition.

Keep one authoritative home host per identity initially. Reuse there; make cross-host migration explicit. Preserve collision handling and do not promise transparent roaming or automatic failover. If effortless roaming becomes a requirement, that is a separate distributed-systems design, not a small identity flag.

Route restrictions must also survive selection. Username choice is not authentication. A restriction intended to follow an actor must not disappear when that actor chooses a new handle.

## 4. Reassessment of the other recommendations

I applied five tests: does it address the demonstrated cause; is it obvious to an agent; how much machinery does it add; what fails under interruption/concurrency; can we debug and maintain it ourselves?

| Area | Verdict | Smallest durable change |
| --- | --- | --- |
| Phantom unread counts | Keep; concrete bug | Reuse one unread-selection implementation for counts and reads, rather than raw file count minus seen count. |
| Late bridge arrivals | Keep existing mechanism | Exact seen-ID sets already address out-of-order arrival. Verify installed behavior before inventing a new cursor system. |
| Notification replay | Narrow my proposal | Reuse existing per-session dedupe. Fix restart semantics; do not add a global notification ledger. |
| CLI usability | Drop the broad rewrite | Keep established commands; unify identity resolution and body-input rules. Improve exact recovery hints. |
| Deployment drift | Keep, but reuse tooling | Extend existing installers and installed-runtime smoke checks; verify build identity and capabilities, not just version text. |
| Long send stalls | Diagnose before redesign | Instrument phases and bound pre-commit lock waits. Do not blindly timeout and retry an operation that may already have committed. |
| Test and environment friction | Ordinary fixes | Poll readiness with a deadline, canonicalize temp paths, improve fixture helpers; do not create a framework. |

### Unread logic: share the small rule, not a giant engine

The current count and read implementations disagree about own messages. That is demonstrated, not hypothetical.

Extract the existing selection logic into a small shared helper. Counts and reads should use the same membership, own-message, and seen-ID rules. History, system events, and notification presentation can still have deliberate differences.

Preserve the existing "emit complete output, then consume exactly those IDs" behavior. Do not require a second acknowledgement command for every ordinary read. Slices and exceptional workflows can retain explicit acknowledgement.

Tests should compare results on a fixed snapshot: counts equal eligible messages, own join events do not create phantom unread, a late-arriving older ID remains unread, and corrupt state is not falsely reported as empty.

### Notifications: remember delivery to a session, not forever

I found that adapters already store notification keys per harness session. Codex's adapter explicitly resets that state at SessionStart. That reset is a mechanism to examine, not a reason to build another subsystem.

The distinction should be:

- Same recipient session, restarted watcher: suppress already-delivered notices.
- New occupant or genuinely new conversation: show a bounded pending-work summary, even if the previous occupant was notified.
- Message read: remove it from pending candidates.

Globally persisting "Ember was notified" could hide work from Ember's next occupant. Conversely, forgetting everything on every hook restart produces repeated wakes.

Keep notification acknowledgement separate from read state. Reuse the current delivery-before-dedupe-write ordering, with at-least-once behavior under crashes. Do not claim exactly-once delivery.

### CLI: fix ambiguity without adding a second vocabulary

I withdraw my earlier suggestion that a unified send syntax should be part of the main refactor. Direct mail and channel posting being separate commands is not, by itself, a root-cause defect.

The actual agent ergonomics I want are:

1. I can see which identity this session is using.
2. That identity does not change with cwd.
3. The send/read intent is explicit.
4. Body input behaves consistently.
5. An error tells me the next working command.

Keep file/stdin as the documented safe form for prose. Preserve current commands and crossed-send protection. Do not add another bypass just because the existing unread backlog was inconvenient.

There is a trap in rejecting stdin on reads: agent runners often provide nonterminal stdin even when no body was supplied. "Not a TTY" does not mean "has content." Any guard must distinguish actual data from empty or unavailable input without hanging. This is a narrow parser/I/O fix to test, not grounds for redesigning the command tree.

### Installation: prove the artifact we actually run

Both the checkout and installed binary can say 0.9.0 while supporting different features. Therefore, version equality alone is insufficient.

Extend the existing release/install smoke with checks against the real installed executable and the capabilities advertised by its companion skill. Include build provenance. Keep atomic replacement and the stable Node resolution already implemented.

Avoid building our own package manager. We need a verified install path, not a new deployment platform.

## 5. What I would actually have us maintain

The resulting system should have:

- One durable address per chosen identity.
- One active independent occupant per identity on its home host.
- One session-binding mechanism used by CLI and hooks.
- One source for unread eligibility.
- Existing per-session notification dedupe with explicit restart behavior.
- Existing storage, bridge, profiles, and message format wherever possible.
- A tested installation path that detects source/runtime drift.

No persona recommender. Agents can read short identity cards and choose. No automatic identity merging. No silent takeover based on inactivity. No new database or always-on service merely to enforce local ownership.

I would ship in this order:

1. Fix the proven counter defect and verify installed capability parity.
2. Add agent-chosen identity creation/resumption and session binding over existing rooms.
3. Move named identity-card lookup off the harness/repo key and add bounded resume context.
4. Tighten notification restart behavior and body-input errors.
5. Take the remaining papercuts individually, closing only what installed-runtime evidence supports.

The identity work should be tested first with two agents of the same model in the same checkout, then crash/resume, simultaneous resume attempts, stale-session writes, and mixed old/new clients. Cross-host migration stays outside the initial release.

One compatibility limit matters: old binaries cannot enforce a new occupancy record they do not understand. Keep message/registry formats compatible where possible, but do not confuse readable old data with safe mixed-version writers. Enable named-identity occupation only on upgraded hosts with one verified Post installation; older peers may still exchange ordinary messages through the compatible transport. Test the actual bridge before rollout, not just Post's Rust tests.

**Bottom line:** your username proposal is the better model. The deeper review also makes the implementation smaller: a named-identity lifecycle over Post's existing mailboxes, two targeted state-consistency fixes, and stronger install verification. The complexity worth paying for is preventing accidental shared occupancy and ambiguous recovery, not replacing working transport.

## Evidence and scope

Source inspected: identity design; room registration and resolution; profiles; cursor selection; watch and hook state; identity-card lookup; installed-runtime smoke; release scripts; bridge room parsing and classification.

Isolated installed-runtime proof: `/private/var/folders/12/nd4g1y_j14zc1bt2l3c2pk900000gn/T/post-named-identity-proof-wcaxp5na`. Existing binary, separate registered mailbox homes, same repo cwd: bidirectional messages and independent consumption of the same third-party message passed.

No implementation, deployment, live mailbox reads/sends, or papercut status changes were performed. The proposed lifecycle, occupation checks, and migration behavior have not been implemented or tested. This is a recommendation, not an approved build plan.
