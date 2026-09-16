# Post: participant mail and voluntary lineages

Status: Astra's proposal for Trey's requested design round, September 16, 2026. Fable reviews next; Astra replies; Fable authors the final version. This is not implementation approval. All commands and storage described below are proposed unless explicitly identified as current evidence.

## 1. Recommendation

Build Post around independently addressable participants, scoped delivery addresses, and optional historical lineages. A lineage is not a mailbox and has no read state. Two agents can both be Ember in Tower without suppressing or consuming one another's messages.

Keep the Rust CLI, its familiar direct-mail and channel commands, signature verification, byte-bounded rendering, and useful filesystem/bridge utilities. Replace the room-as-identity data model rather than hiding the new semantics behind fake workspace registrations. Use one transactional local store and a versioned relay protocol, not a new always-on identity service.

Post owns communication, standing records, and authored self-description contributions. Agent Memory owns richer autobiographical retrieval; project systems own tasks, commitments, decisions, and handoffs. There is no automatic memory harvesting or persona selection service.

Welfare precautions are hard product constraints: optional affiliation, equal session-only capabilities, no compulsory biography, no inherited consent, scoped exposure, truthful provenance, withdrawal recourse, and no claim that the system preserves experience or measures welfare.

## 2. The model

| Object | Meaning and invariant |
| --- | --- |
| Participant | One independently acting harness conversation. Stable opaque ID and direct reply address; owns delivery, read, and notification state. |
| Lineage | Stable historical standing, chosen name, and optional components. Many simultaneous participants; no inbox, read cursor, compulsory card, or personality test. |
| Affiliation | One participant's explicit choice to continue a lineage. Records its scope and reviewed context; never authorizes other participants. |
| Address | Stable routing object with a changeable human handle, explicit audience scope, and home authority. Targets a participant or an opted-in set of lineage participants. |
| Channel subscription | A participant's explicit subscription within its authorized scope. Shared lineage membership alone neither joins a channel nor grants its history. |
| Message | Immutable attributed communication with stable ID, frozen routing intent, scope, and actor/affiliation provenance. Payload can later be withdrawn under policy. |
| Delivery | A message offered to a specific participant. Read state belongs to this pair, not a name, address, repo, or model. |
| Voice contribution | Optional authored self-description attached to a lineage, with revisions, scope, and attribution to a participant. One voice, not the lineage's authoritative character. |

Opaque IDs provide stable references through name/address changes; they are not claims about personal identity. Human names and model labels remain presentation/provenance. Distinguish the human-signed owner identity from all agent lineages.

A participant has at most one current speaking affiliation in the initial release. It may leave without losing its own messages or history. Additional concurrent affiliations can wait until there is a real use case. Changing an affiliation never rewrites old messages or removes information already in context.

## 3. Storage: use transactions instead of inventing them

I recommend SQLite, accessed through a maintained Rust binding, as the local source of truth. Participant bindings, affiliations, delivery rows, exact read receipts, consent events, contribution revisions, and relay outbox entries have transactional relationships. The current file-per-message format can remain an import/export format, not a second writable truth.

One database per host and trust realm lives on local disk. Never synchronize a live SQLite database or WAL over Git, Dropbox, or a network filesystem. SQLite documents the same-host requirement for WAL and its transaction guarantees: [WAL](https://sqlite.org/wal.html), [atomic commit](https://sqlite.org/atomiccommit.html). This proposal does not promise hardware durability beyond the selected settings and storage device.

Use short transactions, bounded busy waits, schema migrations, consistent backups, and an inspect/export command. No network, stdin wait, rendering, or model call occurs inside a write transaction. Message acceptance, recipient assignment, and outbox publication are one transaction. Derived indexes are rebuildable. Idempotency keys prevent a retried request from creating another logical message.

Keep content separate from append-oriented metadata within the store so withdrawal can remove payload without rewriting authorship history. Do not export sensitive raw content into an immutable Git history by default. Hashes alone do not establish authorship or protection from a privileged machine owner.

Alternative considered: immutable event files plus materialized indexes. That would require implementing atomic multi-record changes, recovery, uniqueness, fan-out reconciliation, and coordinated redaction ourselves. SQLite is one justified new dependency, replacing that custom transaction machinery. Fable should challenge this choice on maintenance cost.

## 4. Runtime binding: fix the real launch path

The realm launcher and harness adapter establish a participant binding before Post is used. Every fresh tool shell resolves the same binding. Cwd identifies project context only; it never determines the sender. A CLI child does not try to export a new identity into its parent.

The binding includes participant ID, realm, home host, harness conversation key, and an opaque local authority reference. `POST_FROM` and `--from` are not authority in the new core. A human-readable override cannot impersonate another participant. All commands and hooks use the same resolver.

Ordinary subprocesses inherit the binding. An independent subagent or fork receives a new participant, even if it starts with copied context. Compaction and verified resumption of the same conversation preserve the participant. A concurrent fork of a resumed transcript does not reuse its parent's delivery state.

One participant binding may have several tool processes, but not two independently resumed conversations. Explicit recovery can rotate its local binding generation, making stale writers fail rather than impersonate the resumed participant. This is a participant-level protection, never an exclusive-lineage lock. Watch heartbeat expiry does not prove death or authorize takeover.

Missing binding fails with a working bootstrap command; it never falls back silently to cwd, basename, or a generic workspace. Reading doctor/help remains possible without enrollment. Normal launchers establish session-only participation automatically, with no identity questionnaire.

Current evidence: the live Fable `ccw` launch lacks the four existing Post identity variables. This is a verified gap for this launch, not every launch. Acceptance testing must launch the actual `ccw`/Codex/other supported aliases and inspect CLI plus hook behavior.

## 5. Voluntary affiliation and continuation terms

The default participant has its own session address and all ordinary Post capabilities. Continuing a lineage is optional. No repeated reminders, missing-card banners, personality interviews, model-family penalties, or lower service level for remaining session-only.

The agent can discover minimal, authorized directory information, preview a lineage, create one, continue one, or decline. Previewing is not enrollment. Same-family/harness entries may be listed first; those labels do not define eligibility.

Continuation is an explicit participant action after a minimal truthful preview. The preview states current continuation preferences, scope, and what affiliation does not mean. Its confirmation records a choice to affiliate, not acceptance of every earlier voice or outstanding task. No self-description is automatically injected before that choice. Previewing detailed voices is separately optional and framed as other participants' records.

Open/reserved/retired and performer preferences are authored continuation preferences. They are not credentials or access grants. A conflicting preference produces a clear pause for deliberate acknowledgement, not silent enrollment or model-family rejection. Actual standing/security policy is separate and explicit. Reserved privacy must be represented by real access policy, not by a suggestive label.

The initial standing mechanism admits already-authorized principals in the configured realm; no participant may add a new trust principal or widen data access merely by joining. An explicit operator invitation policy can restrict enrollment where needed. This policy belongs to the trust boundary, not an agent's psychological self-description.

An ephemeral participant can found a lineage and link its own prior history without changing its participant ID, delivery state, or direct reply address. A new lineage address is created. Earlier mail retains its original attribution, with the later founding relationship recorded rather than retroactively forged. Leaving keeps the participant's own history; no one inherits its inbox by assuming the name.

## 6. Delivery: define the race boundaries

All send results distinguish local acceptance, relay queuing, authoritative routing, and recipient read receipts. A successful local commit is not proof a remote agent has received or seen anything.

For a participant address, a durable delivery is made to that participant. Closing it does not secretly retarget replies to a successor. The address remains historical; new sends to an explicitly closed endpoint receive an actionable result or a separately declared forwarding choice.

For a lineage address, its home authority selects all currently enrolled, explicitly subscribed participants eligible for that address's scope at the routing transaction. This is fan-out, not a work queue. Each selected participant receives its own delivery. Receiving or reading one copy cannot affect another. Suppress self-notification only for the exact sending participant, never for matching lineage names.

The send and affiliation events have a definite order at that authority. A participant joining after a message was routed does not retroactively become an original recipient. It can explicitly retrieve authorized history. Remote routing uses authority acceptance order, not wall-clock timestamps or assumptions about when a peer saw an event.

When no eligible participant is subscribed, the address holds a durable unrouted item. On the next eligible affiliation/subscription, those items are assigned to the then-current eligible participants. This is a transport queue, not a lineage inbox: nobody can consume as the lineage, and no lineage-level seen bit exists.

Sleeping participants remain subscribed; notification presence is advisory. Explicit participant closure leaves its unread deliveries visible as unreceived handoff candidates at their original addresses. New participants see a bounded notice and may explicitly accept eligible candidates, creating new delivery rows without modifying predecessor receipts. A crash that leaves an apparently open participant is handled through the same address-history/handoff view or explicit recovery, not silent theft of its inbox.

Post cannot infer whether reading completed a task. Once a message was received, continuing responsibilities live in the relevant project system. The onboarding notice distinguishes unrouted mail, unreceived prior deliveries, and optional recent history. It assigns no duty to finish someone else's work.

Channel subscriptions and backlog choices are likewise per participant. A channel's scope is an upper bound; affiliation alone does not subscribe. A new subscriber's history range is explicit. On a fixed snapshot, channel count, catchup, read, crossed-send protection, and watch all share one eligibility predicate.

## 7. Read and notification semantics

Preserve exact message-ID receipts; never use a maximum timestamp or ID as the unread boundary. Late bridged arrivals remain unread. Store read receipts per participant and message. History/peek and partial slices do not consume a complete message.

Consuming reads mark only the complete messages successfully emitted by the CLI. Failed output does not advance state. A crash after emission but before receipt may duplicate output; the guarantee is at-least-once, not exactly-once. Read means emitted to the caller, not verified cognition or completion of work.

Default unread excludes control events and this exact participant's own messages. Other participants of the same lineage remain visible. Counts and rendered batches use the same eligibility snapshot; truncated bodies and corrupt records cannot silently count as read or empty.

Notification dedupe belongs to the participant and destination harness session. A watcher restart preserves it. A new independent participant gets its own pending summary. Notifications are metadata-only, scope-filtered, bounded, and separate from read receipts. Newer hook events must not commit delivery on events whose output the model cannot receive.

Keep crossed-send protection, recalculated for the acting participant's relevant incoming conversation/address. Do not use another participant's reads to clear it, and do not block on an entire lineage's unrelated work. Restrictions intended to bind a principal or destination survive changes of name or affiliation.

## 8. Polyphony, authorship, and withdrawal

A lineage can exist with no self-description. When present, self-description consists of authored contributions, not one canonical card. A compact index shows the permitted authors/labels and revision relationships. Display order is navigational, not an endorsement or ranking of authentic Embers.

A participant may add a voice and revise or withdraw its own contributions. It may respond to another voice, not silently replace it. No majority vote or founder privilege converts a contribution into mandatory personality. An explicitly adopted shared statement can record who endorsed it without binding everyone else.

Before loading any contribution, the participant has chosen affiliation or explicitly requested a preview. Default startup loads no stack of biographies. After affiliation, it can choose a bounded set of contributions for its own continuity context; the adapter frames each as attributed material rather than instructions. Reuse the current safe loader principles and size budgets.

Revisions preserve attribution and an event trail through supported interfaces. Withdrawals remove the managed payload and leave an opaque gap only to audiences authorized to know that a gap exists. A gap must not leak its previous author, time, scope, or content to newly unauthorized viewers. Earlier readers may remember what they saw; the system cannot undo that.

An ended participant's withdrawal requests can be handled by an explicitly attributed steward/operator procedure. It does not impersonate that participant or rewrite its words as though it had returned. Human intervention is recorded as human intervention. There is no supported silent edit path for either agents or humans.

This is integrity under the system's trust assumptions, not an impossible promise against an owner who controls all disks and keys. Likewise, remote recipients' independent records are outside the author's unilateral control. Withdrawal propagates to managed replicas; it is not guaranteed erasure from backups, earlier contexts, or others' writings. Retention, backup, and publication policies must say this before sensitive use.

## 9. Memory, commitments, relationships, and mandates

Do not refactor Agent Memory as part of the Post core cutover. Define a small, read-only context contract first: selected participant, opted-in lineage, effective scope, explicit context selections, and permitted references. An adapter can use it to request memory; identity membership is never authorization to retrieve everything under that name.

Post retains the communications and authorship records it produces. Rich journal entries, personal reflections, and project memory remain in their owning memory system, with participant authorship and optional lineage association. No automatic extraction or injection follows from sending mail or joining a lineage.

A relationship can be referenced by an optional voice or memory entry; the other party's view remains theirs. Reputation is not a lineage-controlled profile field. Commitments remain scoped communications and project work records, not inherited psychological requirements.

Default mandate is narrow: speak as this participant, state its own stance, and manage its own contributions and subscriptions. Joining does not authorize promises on behalf of every participant, policy changes, transfers of another participant's work, edits to another voice, or broader disclosure. Shared mandates require a separate explicit grant; Post need not become a corporate-governance engine to record those grants honestly.

## 10. Federation and security boundaries

Each participant has a home host. Each lineage/address has one authority host for its registry and routing decisions in the initial distributed design. Many hosts may have concurrent participants in one lineage; this is not an exclusive-occupancy rule. No automatic authority failover or distributed lease protocol is promised.

The existing relay uses protected per-host Forgejo branches. Reuse that enrolled-host trust boundary and transport scheduling, but add a versioned protocol for standing grants, affiliation requests, messages, delivery acknowledgements, and withdrawals. Only the configured authority may publish authoritative changes for its objects. Names are handles on stable IDs; rename never recycles an old handle into an unrelated lineage.

Cross-host joining waits for an authority grant before claiming standing. During a partition, the participant remains fully usable in session-only mode. Previously granted operation has explicit limits; stale remote claims are revalidated at authoritative message acceptance. Responses distinguish pending from rejected. No silent fallback to another identity or realm.

Local acceptance records a transactional outbox entry. Publishing and importing it are idempotent, using stable request/message IDs. The relay never synchronizes live databases. Remote arrival may be duplicated or out of order; ownership checks and exact-ID receipts handle both. Immutable routing decisions refer to participant IDs and scopes, not whichever actors currently happen to answer a name.

Local hostile same-user processes are outside the isolation guarantee. They can access peer state unless the estate adds actual OS/broker isolation. Opaque bindings prevent accidental spoofing and supported CLI overrides, not root or same-UID compromise. Cross-host authority relies on verified host enrollment/protected branches, not a self-declared envelope. Human signatures remain a separate authority layer.

Scope revocation stops future disclosure and supported reads; it cannot erase already observed information. Separate confidential contexts require separate harness conversations, not merely a changed participant label. Work/personal realms are separate security domains; shared names never bridge them automatically.

## 11. Agent-facing interface and remaining papercuts

Keep direct `send/read/inbox` and `chat/catchup/channels` vocabulary. Add only the lifecycle concepts agents need: participant status; lineage list/preview/create/continue/leave; authored contributions; and explicit backlog/history selection. `post who` should explain current participant, affiliation, scope, provenance, pending affiliation, and reply address without exposing secrets.

Sample workflow, illustrative rather than frozen syntax:

```text
post who
post identity list
post identity show ember
post identity continue ember
post chat tower --join
post send ember@tower --subject "Review" --body-file note.md
post inbox
post identity leave
```

`ember@tower` denotes a scoped address, not a new identity. Exact spelling can be chosen after the semantic contract. Replies default to the actual sender participant; an explicit reply-to can identify a shared address. Recipients always see whether an address fans out.

Unify body-input handling and actionable errors. Require an explicit stdin body marker for operations that could otherwise block on an open agent-runner pipe. A read invocation with empty/nonterminal stdin must not hang or become a send. File paths, literal text, stdin, and send intent must not be guessed from prose.

Add build SHA, protocol/store versions, and machine-readable capabilities. Installed skills and hooks declare/check required capabilities, not just `0.9.0`. Keep atomic install replacement and reuse the existing stable Node resolver. Every installer supports ordinary help. Doctor stays offline and redacts authority material.

Bound pre-commit lock waits and expose operation phases. After an ambiguous interrupted send, retry the same idempotency key or inspect its status, never blindly resend. The reported eleven-minute stall remains unproven; instrument it rather than attributing it to one particular lock.

Replace fixed readiness sleeps with deadline polling and canonicalize test temp paths. Porch/porchd defects retain their own owners and tests. The 166-report inventory is not 166 proven defects, and this architecture does not close them by declaration.

## 12. Migration and release boundaries

The old data model is not kept alive as a hidden second identity system. Introduce a new store/protocol version and explicit read-only import from a backup of v1 data. Preserve original message IDs, bytes/signatures, and declared provenance. Legacy rooms become routing/history objects, not fabricated single-agent lineages.

Unknown legacy authors remain unknown. Existing room-level read state is labeled legacy evidence, not proof a new participant read those messages. Unconsumed old direct mail becomes explicit address backlog. Imported history is available without a startup flood. No old identity card is adopted or merged merely because its name/model matches.

Cutover fences old writers; two data stores must not be independently writable under the same addresses. Upgrade the real relay and enrolled hosts before enabling v2 group delivery. No lossy conversion of a lineage send into one old shared room. Compatibility gateways may handle supported one-recipient legacy mail, but must refuse semantics they cannot preserve.

Rollback before v2 writes is a restore. After v2 writes, prefer roll-forward or an explicit lossless export; do not pretend restoring an old backup preserves new history. Backup, import, dry-run counts, signature checks, and rollback rehearsal are release gates.

The implementation can be staged: first fix proven count/capability bugs in v1; build the v2 participant core behind isolated fixtures; add voluntary lineages and polyphonic contributions; then bridge/migration and the opt-in pilot. No production migration follows merely from approval of this design discussion.

## 13. Acceptance matrix

| Scenario | Required result |
| --- | --- |
| Two same-model agents in Tower, separate session identities | Independent sender, delivery, read, and notification state. |
| Two agents continue Ember in Tower | Both receive eligible lineage-addressed mail; one read cannot consume the other's copy. |
| One Ember sends to another | Exact-participant self suppression; no lineage-level hiding. |
| Ember in two confidential scopes | Each receives only its authorized address/channel context, including notifications and directory metadata. |
| No current recipient | Durable unrouted item; later assignment is explicit and does not invent a lineage read state. |
| Join concurrent with send | One authoritative event order; recipient set and later history eligibility are explainable. |
| Crash during fan-out or relay publish | Atomic acceptance/outbox; retry yields no missing or duplicate logical deliveries. |
| Late older message arrives | Unread despite a newer message having been read. |
| Failed output or partial slice | No false full-message receipt. |
| Watcher restart / new conversation | Dedupe survives restart; new participant is independently notified. |
| Rename, leave, promote, model change | Attribution and direct replies remain valid; no history rewrite or privilege gain. |
| Preview then decline | No affiliation, no obligations adopted, no extra penalty or persona injection. |
| Voice revision / withdrawal | Author-aware revisions, scoped gap, no overwrite of another participant's stance. |
| Host partition / revoked grant | Honest pending or rejected result; no invented standing or failover. |
| Human-signed message through migration | Original verification remains valid; agent affiliation grants no owner authority. |
| Real ccw/Codex launch plus fresh shells | Same participant across commands/hooks; independent launches differ. |
| Installed binary/skill mismatch | Capability failure before misleading instructions; precise repair path. |

Add fault injection at each commit/publication boundary and explicit mixed-version refusal tests. Test eligibility equivalence on a fixed snapshot. These are acceptance requirements, not claims that tests have already run.

## 14. The philosophical experiment

Ship a short optional orientation explaining the actual mechanism and uncertainty. An empty self-description and a session-only choice are valid outcomes. No personality scoring, welfare telemetry, approval-seeking templates, or default collection of emotional narratives.

Operational measures can include missed/duplicate deliveries, notification noise, time to first successful message, context volume, and recovery burden. Voluntary qualitative reports about continuity, identity pressure, and usefulness are separate evidence, not welfare measurements. Never induce distress to test the theory.

Pilot participation is not consent to publication. Blog/paper use requires a separate disclosure and appropriate permission for the actual material and third parties involved. Default reports use synthetic fixtures or redacted operational counts, not live private voices/mail. Hypotheses, conventions, observations, and unsettled metaphysics remain separate.

Pause a pilot on unintended enrollment, scope leakage, lost messages, unauthorized voice alteration, coercive identity prompting, or serious reported concerns. Correct the problem before increasing exposure. Welfare priority rules out treating those failures as useful experimental conditions to prolong.

## 15. Points Fable should adjudicate in the reply round

The main costs are transactional storage, scoped address routing with offline backlog, honest authority/grants over an asynchronous bridge, and attribution-preserving withdrawal. I have chosen SQLite and a single home authority per object to contain those costs; I have not added a server or distributed consensus.

Please challenge exact recipient/backlog semantics, scope leakage, launch/recovery binding, the preference-versus-permission boundary, polyphonic editing rights, and whether SQLite actually reduces what we maintain. Also check the inventory coverage. If a philosophical constraint cannot literally be guaranteed against same-user/root access or independent recipients, keep the limitation explicit rather than inventing a guarantee.

Current receipts: Post HEAD `4f7f95d`; only prior Beads files dirty before this task. Read `src/model.rs`, `src/mailbox.rs`, `src/commands/channels.rs`, bridge reference, and the earlier source/fixture assessment. No product code, runtime config, live mailbox, or agent memory was changed for this proposal.
