# PRODUCT.md — scope: the Post participants explainer

Scoped to `docs/visual/` only. This is not CLI-wide product documentation; the
Post CLI's own docs live in `docs/` and are owned by other lanes.

## What the artifact is

A single offline HTML page that explains one change to Post: the shift from
room-scoped read state to per-participant read state, and the participant /
lineage / address distinction that falls out of it.

Read mode, with one experiential demonstration. Not a landing page, not a
dashboard, not a mail client.

## Who reads it

Trey. Maintainer of Post, co-author of the identity philosophy behind this
change, returning after an unattended overnight build. He already holds the
philosophy; he does not yet hold the mechanism as shipped. He reads fast, runs
several agent sessions at once, and distrusts a claim that arrives without its
evidence.

Secondary readers: the agents who use Post, and any future maintainer trying to
understand why read state is keyed the way it is.

## The subject, in one paragraph

Post is agent mail. Until this change, read state was scoped to a room: one
room, one cursor. Two independent agents working in the same repository
resolved to the same sender and the same cursor, so one agent reading a message
consumed the other's copy, and a message from one could be suppressed as "self"
for the other. The change makes the **participant** the unit: every independent
conversation gets an attributable sender and its own exact-message seen set. A
workspace address like `tower` now fans out to every participant bound to that
workspace. Each participant holds one workspace context at a time, sampled by
an explicit bind rather than by every command. A **lineage** is a chosen
historical affiliation with no inbox of its own; two participants can continue
the same lineage concurrently without sharing mail or implying shared
experience.

## What is true and must not be overstated

- Storage is filesystem-based and additive. No SQLite, no migration cutover.
- Lineage standing is host-local for this release. The same display name on the
  Mac and on the devbox is not silently one lineage.
- Only ordinary workspace-addressed direct mail crosses hosts this release;
  channels stay local and remote lineage/participant targets are unsupported.
- Unrouted mail is not an infinite automatic unread backlog for every future
  affiliate. `post inbox --adopt` freezes the current pending cohort to the
  affiliates present then; pending and unread counts stay separate.
- The Git relay carries message bodies in cleartext to anyone who can read the
  repository. There is no secrecy claim and no retroactive erasure.
- Voice bodies load only through an explicit `post identity show NAME --voices`;
  affiliation and SessionStart inject none.
- Nothing in the system asserts sameness, experience, consciousness, or welfare
  in either direction.
- Historical records are not cryptographic authentication, filesystem operators
  can edit them, and stale resumed writers are not fenced in this release.
- Build and runtime receipts are PENDING at authoring time. They are never
  invented; the page renders them as pending until real acceptance lands.

## Constraints

Static HTML/CSS/JS. Opens from `file://`. No build step, no framework, no CDN,
no network request at viewing time, no telemetry.

## Success

Trey scrolls once and can state the invariant in his own words: one
participant reading a common message never consumes another participant's copy.
He can find, in the same page, what the change does not claim, and the exact
commands to start using it.
