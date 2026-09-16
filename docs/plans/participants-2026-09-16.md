# Participants and lineages — build plan (Level 1, lint-only)

Spec: `docs/PARTICIPANTS.md` (commit 04ca99b). Goal lock: Trey's `/goal` of 2026-09-16 — ship the participant/lineage model, install on the Mac and the devbox (`devagent:~/.local/bin/post`), pass the two-agents-one-repo acceptance live with Fable and Astra, explainer open in the browser. Full authority delegated to Fable (integrator) and Astra (reviewer of every diff, co-owner of installed acceptance, owner of `docs/visual/` in a separate worktree — outside this plan). No GitHub actions. Forgejo pushes ungated.

Ceremony pricing: every task is reversible source in one repo; the merge gate is the full `scripts/gate.sh` at integrated HEAD, run by the coordinator; installed acceptance (§13 of the spec) is coordinator work on both hosts, not a task row. No G1/G2 gates.

## Interfaces

- P.1 mutates `src/participant.rs`, `src/lineage.rs`, `src/cli.rs`, `src/model.rs`, `src/mailbox.rs`. It declares and the later tasks consume:
  - `participant::Participant { id, harness, workspace: Option<String>, workspace_path: Option<PathBuf>, lineage: Option<String>, dir: PathBuf }`
  - `participant::resolve(context: &Context) -> AppResult<Resolved>` where `Resolved { participant: Participant, provenance: Provenance }` and `Provenance ∈ {ExplicitEnv, HarnessClaude, HarnessCodex, LauncherAddress}`; the no-binding case is `AppError::no_participant(fix_line)`.
  - `participant::bind(context, cwd: &Path, workspace: Option<&str>) -> AppResult<Participant>` (idempotent mint + context record; O_EXCL by-session index; `.participants.lock`).
  - `participant::lock(context) -> AppResult<File>` — the one lock (`$ROOT/.participants.lock`) for mint/bind, affiliation, routing.
  - `participant::list(context) -> AppResult<Vec<Participant>>`.
  - `lineage::Lineage { name, founder, created, host, dir }`, `lineage::load(context, name) -> AppResult<Option<Lineage>>`, `Lineage::members(&self) -> AppResult<BTreeMap<String, Member>>`, `lineage::validate_name` (rejects registered rooms and reserved names). Read model only; writes are P.3.
  - `model::Envelope` and `model::ChannelMessage` gain `from_participant: Option<String>`, `from_lineage: Option<String>`, `address_kind: Option<String>` (default + skip_serializing_if).
  - `mailbox::Context::sender(&self) -> AppResult<Sender>` with `Sender { from: String, participant: Participant, lineage: Option<String> }` (`from` = workspace address if bound else participant id); the old `resolved_room_with_provenance` stays for `--room`/legacy read paths.
  - CLI (clap) surface, all bodies stubbed with `AppError::not_yet("P.2"|"P.3")` where the body belongs to a later task: `post participant {show|bind [--workspace <room>]|new --harness <slug>|list}`, `post identity {list|show <name> [--voices]|new <name>|continue <name> [--acknowledge]|leave|voice add --body-file <f>|voice withdraw|terms set --body-file <f>}`, `post inbox --adopt`, `post send --kind <workspace|lineage|participant>`, `post version --json`.
  - `RESERVED_ROOM_NAMES` += `participants`, `lineages`, `routing`, `.participants.lock`.
- P.2 mutates `src/routing.rs`, `src/eligibility.rs`, `src/cursor_state.rs`. Declares `routing::Receipt { version, message, digest, address: Address, recipients: Vec<String>, routed_at, routed_by }`, `routing::route_pending(context, address: &Address) -> AppResult<RouteReport>`, `routing::Address { kind: AddressKind, name: String }` with `AddressKind ∈ {Workspace, Lineage, Participant}`, `eligibility::unread_mail(context, participant, address) -> AppResult<Vec<EligibleMail>>`, `eligibility::unread_channel(context, participant, channel) -> AppResult<Vec<EligibleChannelMessage>>`, `cursor_state` v2 per-participant API (`ParticipantCursors::load/consume_mail/consume_channel`) alongside the legacy v1 room reader (read-only).

## Scope fence

In: the participant primitive and resolution, sender fields, routing receipts and per-participant read state, lineages with voices and terms, adapter changes, skill/docs, the installed smoke, bridge verification. Out (documented limits in spec §14): SQLite, cross-host lineages or any relay protocol change, participant/lineage addresses over the bridge, stale-writer fencing, steward withdrawal, relay confidentiality, the stdin body marker, the papercut inventory beyond the five release requirements, `estate-harness` and `launcher/agent-session` changes (not needed: both harnesses export conversation keys), and `docs/visual/` (Astra's lane).

## Acceptance demo

Spec §13, run by the coordinator against the installed binaries on the Mac and `devagent`: two participants in one registered workspace of this checkout (the live Fable and Astra conversations, no restart), bidirectional workspace mail with independent read state and no file moves, a third-party message received by both, sibling-lineage visibility, pending-then-adopt on an empty lineage, terms acknowledgement, per-participant channel leave, late-arrival unread, watcher restart dedupe, matching `post version --json` receipts on both hosts, one ordinary cross-host workspace message each way through the unchanged bridge, full `scripts/gate.sh` green at integrated HEAD, and `scripts/smoke-installed.sh` green against both installed binaries.

## Wave map

WAVEMAP

## Invariant → verify table

| task | invariant | verify row |
| --- | --- | --- |
| P.1 | one conversation key never yields two participants, including under concurrent bind | `cargo test --all-targets --all-features participant` (concurrent-bind test) |
| P.1 | cwd never selects the sender after bind | `cargo test --all-targets --all-features participant` (bind-then-send-from-other-cwd test) |
| P.1 | old-format messages parse unchanged | `cargo test --all-targets --all-features` (existing envelope round-trip suites) |
| P.2 | a routing receipt is published atomically or not at all, and a retry after publication yields the same recipient set | `cargo test --all-targets --all-features routing` (crash-before/after-publish tests) |
| P.2 | no participant's read consumes for another | `cargo test --all-targets --all-features routing` (two-participant consume test) |
| P.2 | counts and rendered batches use one eligibility snapshot | `cargo test --all-targets --all-features` (counts suite equals read batch) |
| P.3 | a participant edits only its own voice | `cargo test --all-targets --all-features lineage` (foreign-voice edit refused) |
| P.3 | continuation never rejects by model, harness, or terms content | `cargo test --all-targets --all-features lineage` (terms require --acknowledge, never refuse) |
| P.4 | no adapter injects self-description text before an explicit affiliation | `node --test skills/post/hooks/*.test.mjs` (unaffiliated SessionStart emits no identity text) |
| P.4 | a capability mismatch fails before any instruction text | `node --test skills/post/hooks/*.test.mjs` (stub binary lacking participants) |
| P.5 | documented commands match the spec's command list | `rg -n 'post identity (list\|show\|new\|continue\|leave)' skills/post/SKILL.md` |
| P.6 | the smoke fails against the previous binary | `bash tests/acceptance.sh` plus the recorded red-proof against the 0.9.0 binary |
| P.7 | no file outside the report is modified | `test -s docs/reviews/identity-2026-09-16/bridge-verification.md` plus `git status --porcelain` limited to that path |

```toml plan
name = "participants-2026-09-16"
repo = "~/Code/post"

[regimes.standard]
lanes = 2
min_families = 2
fix_rounds = 1
re_review = "scoped"
adjudicate = true
severity_floor = "major"
max_task_calls = 12

[regimes.prose]
lanes = 1
min_families = 1
fix_rounds = 0
re_review = "none"
adjudicate = false
severity_floor = "major"
max_task_calls = 8

[regimes.critical]
lanes = 3
min_families = 3
fix_rounds = 2
re_review = "scoped"
adjudicate = true
severity_floor = "major"
max_task_calls = 24
```

```toml routing
[core]
routes = [{engine="codex", model="sol", effort="xhigh", mode="work"}, {engine="claude", model="opus", effort="high", mode="work"}]

[executor]
routes = [{engine="codex", model="luna", effort="xhigh", mode="work"}, {engine="omp", model="glm", effort="high", mode="work"}]

[writer]
routes = [{engine="codex", model="sol", effort="medium", mode="work"}]

[scout]
routes = [{engine="codex", model="luna", effort="medium", mode="work"}]

[reviewer]
routes = [{engine="claude", model="opus", effort="high", mode="work"}, {engine="omp", model="glm", effort="high", mode="work"}]

[integrator]
routes = [{engine="codex", model="luna", effort="medium", mode="work"}]

[verifier]
routes = [{engine="codex", model="luna", effort="xhigh", mode="work"}]
```

```toml task
id = "P.1"
title = "Participant primitive, resolution, bind, sender fields, CLI surface, version"
delivers = "post resolves the acting participant per spec §4 (POST_PARTICIPANT, CLAUDE_CODE_SESSION_ID, CODEX_THREAD_ID/CODEX_SESSION_ID, POST_SENDER_ADDRESS), mints idempotently under .participants.lock with an O_EXCL by-session index, records workspace context at bind, writes from/from_participant/from_lineage/address_kind on every new message, rewrites post who around participants, adds post version --json with build_sha and capabilities, declares the identity/adopt/--kind CLI surface with later-task stubs, and reserves the new store names"
kind = "build"
blocked_by = []
acceptance = "cargo test participant passes: two different conversation keys mint two ids; the same key twice returns one id; two concurrent binds for one key yield one participant.json; no binding fails send with the exact fix line and leaves who/doctor/schema working; a new send's envelope carries from_participant and a 0.9.0-format envelope still parses; post version --json lists participants in capabilities; rooms add participants is refused as reserved. Green does not prove routing or read isolation (P.2)."
role = "core"
persona = "minimalist-implementer"
skills = ["post"]
owned_files = ["src/participant.rs", "src/lineage.rs", "src/commands/participant.rs", "src/commands/version.rs", "src/commands/send.rs", "src/channel.rs", "src/cli.rs", "src/commands/mod.rs", "src/commands/who.rs", "src/mailbox.rs", "src/model.rs", "src/output.rs", "src/commands/schema.rs", "src/migration_fence.rs", "src/lib.rs", "src/error.rs", "build.rs", "Cargo.toml", "tests/participants.rs", "tests/common/mod.rs", "tests/schema_surface.rs", "tests/cli.rs"]
invariants = ["one conversation key never yields two participants, including under concurrent bind", "cwd never selects the sender after bind", "old-format messages parse unchanged"]
verify = [{run = "cargo test --all-targets --all-features participant", expect = "exit 0"}, {run = "cargo test --all-targets --all-features", expect = "exit 0"}, {run = "cargo clippy --all-targets --all-features -- -D warnings", expect = "exit 0"}, {run = "cargo fmt --check", expect = "exit 0"}]
reversibility = "reversible"
effects = ["code"]

[review]
tier = "standard"
personas = ["seam-verifier", "test-skeptic"]
```

```toml task
id = "P.2"
title = "Routing receipts, per-participant read state, one eligibility helper, no-move reads"
delivers = "spec §6–§7: routing/<id>.json receipts published under the participants lock at send and by route_pending in writer paths only (consuming read/chat/catchup, long-running watch, inbox --adopt, participant bind, identity new/continue/leave) while display-only forms show provisional eligibility as pending without writing; workspace pending mail routes to the bound participants at the first writer path, lineage pending mail waits for inbox --adopt; participants/<id>/cursors.json v2 seen sets; eligibility::unread_* used by every count and batch; self-suppression by from_participant only; consuming reads record seen after emit and move nothing; per-participant channel subscriptions with the legacy workspace default and individual opt-out; per-participant watch heartbeat and presence; doctor reports participant, provenance, pending counts, legacy room state"
kind = "build"
blocked_by = ["P.1"]
acceptance = "cargo test routing and the updated counts/consuming/catchup/watch suites pass: A→workspace: B counts 1, A counts 0, B's read consumes for B only and the file stays in inbox/; C→workspace: both count 1 and consume independently with one receipt naming both; a lineage with no affiliates holds pending until --adopt and a later affiliate does not see adopted mail; a late older id stays unread; a failed emit records nothing; channel leave by B leaves A's membership and B's seen set intact; channels count equals eligibility. Green does not prove installed-binary or cross-host behavior (P.6, coordinator acceptance)."
role = "core"
persona = "minimalist-implementer"
skills = ["post"]
owned_files = ["src/routing.rs", "src/eligibility.rs", "src/cursor_state.rs", "src/channel.rs", "src/channel_state.rs", "src/presence.rs", "src/commands/send.rs", "src/commands/read.rs", "src/commands/inbox.rs", "src/commands/catchup.rs", "src/commands/chat.rs", "src/commands/channels.rs", "src/commands/watch.rs", "src/commands/doctor.rs", "src/commands/search.rs", "tests/routing.rs", "tests/counts.rs", "tests/consuming.rs", "tests/catchup.rs", "tests/watch_preview.rs", "tests/doorbell.rs", "tests/byte_budget.rs", "tests/search.rs"]
invariants = ["a routing receipt is published atomically or not at all, and a retry after publication yields the same recipient set", "no participant's read consumes for another", "counts and rendered batches use one eligibility snapshot"]
verify = [{run = "cargo test --all-targets --all-features routing", expect = "exit 0"}, {run = "cargo test --all-targets --all-features", expect = "exit 0"}, {run = "cargo clippy --all-targets --all-features -- -D warnings", expect = "exit 0"}, {run = "cargo fmt --check", expect = "exit 0"}]
reversibility = "reversible"
effects = ["code"]

[review]
tier = "standard"
personas = ["seam-verifier", "refuter"]
```

```toml task
id = "P.3"
title = "Lineage lifecycle: new, continue, leave, voices, terms, journal"
delivers = "spec §8: post identity list/show [--voices]/new/continue [--acknowledge]/leave/voice add/voice withdraw/terms set over lineages/<name>/ (lineage.json, members.json, history.jsonl tolerant journal, voices/ with history and .gap withdrawal, terms.md); continue with terms present requires --acknowledge and never rejects; affiliation and leave update participant.json.lineage and members.json under the participants lock; voices render only on request under the attribution frame; the gap view names no author"
kind = "build"
blocked_by = ["P.1"]
acceptance = "cargo test lineage passes: new founds with founder=self and self affiliated; list from an unaffiliated participant prints no voice text; show --voices prints each voice under the exact frame; continue on a lineage with terms fails without --acknowledge and succeeds with it; leave clears only the leaver; voice add/revise keeps history; withdraw leaves a .gap and no content; a lineage name equal to a registered room or reserved name is refused; a truncated final journal line is ignored. Green does not prove lineage-addressed routing (P.2)."
role = "core"
persona = "minimalist-implementer"
skills = ["post"]
owned_files = ["src/commands/identity.rs", "src/lineage_store.rs", "tests/lineage.rs"]
invariants = ["a participant edits only its own voice", "continuation never rejects by model, harness, or terms content"]
verify = [{run = "cargo test --all-targets --all-features lineage", expect = "exit 0"}, {run = "cargo clippy --all-targets --all-features -- -D warnings", expect = "exit 0"}, {run = "cargo fmt --check", expect = "exit 0"}]
reversibility = "reversible"
effects = ["code"]

[review]
tier = "standard"
personas = ["spec-fidelity", "conventions"]
```

```toml task
id = "P.4"
title = "Adapters: participant bind at SessionStart, capability check, retire the identity card"
delivers = "spec §9: claude-mail.mjs and codex-mail.mjs (and cursor/grok adapters for parity) run post version --json and refuse with the repair line when capabilities lacks participants, run post participant bind with the session cwd, then post watch --snapshot; add exactly one context line only when affiliated; identity-card.mjs and its tests are removed and the installers stop copying it; docs/ADAPTERS.md identity-card section replaced by the participant/voices contract"
kind = "build"
blocked_by = ["P.1"]
acceptance = "node --test skills/post/hooks/*.test.mjs passes against the release binary: SessionStart against a stub lacking the participants capability emits the repair line and no instructions; SessionStart binds then snapshots; an unaffiliated participant gets no identity text; an affiliated one gets the single line; installers no longer reference identity-card.mjs. Green does not prove the hooks are installed in any live settings file (coordinator install step)."
role = "executor"
persona = "minimalist-implementer"
skills = ["post"]
owned_files = ["skills/post/hooks/claude-mail.mjs", "skills/post/hooks/claude-mail.test.mjs", "skills/post/hooks/codex-mail.mjs", "skills/post/hooks/codex-mail.test.mjs", "skills/post/hooks/cursor-mail.mjs", "skills/post/hooks/cursor-mail.test.mjs", "skills/post/hooks/grok-mail.mjs", "skills/post/hooks/grok-mail.test.mjs", "skills/post/hooks/identity-card.mjs", "skills/post/hooks/identity-card.test.mjs", "skills/post/hooks/install-claude-hooks.mjs", "skills/post/hooks/install-claude-hooks.test.mjs", "skills/post/hooks/install-codex-hooks.mjs", "skills/post/hooks/install-codex-hooks.test.mjs", "skills/post/hooks/install-cursor-hooks.mjs", "skills/post/hooks/install-cursor-hooks.test.mjs", "skills/post/hooks/install-grok-hooks.mjs", "skills/post/hooks/install-grok-hooks.test.mjs", "docs/ADAPTERS.md"]
invariants = ["no adapter injects self-description text before an explicit affiliation", "a capability mismatch fails before any instruction text"]
verify = [{run = "node --test skills/post/hooks/*.test.mjs", expect = "exit 0"}]
reversibility = "reversible"
effects = ["code"]

[review]
tier = "standard"
personas = ["seam-verifier", "conventions"]
```

```toml task
id = "P.5"
title = "Skill, README, changelog, orientation"
delivers = "skills/post/SKILL.md Identity and Command surface sections rewritten for participants, addresses, lineages, voices; skills/post/references/orientation.md (the short optional orientation from Astra's §14: mechanism and uncertainty stated plainly, session-only and empty self-description as valid outcomes, no scoring or emotional-narrative collection); README identity paragraph; CHANGELOG entry"
kind = "build"
blocked_by = []
acceptance = "every command in SKILL.md's command surface exists in docs/PARTICIPANTS.md §8 or in src/cli.rs at the integrated HEAD (coordinator cross-check); orientation.md is under 600 words and contains no instruction to adopt, persist, or report contentment. Green does not prove the CLI behaves as documented."
role = "writer"
persona = "minimalist-implementer"
skills = ["post", "writing-for-agents"]
owned_files = ["skills/post/SKILL.md", "skills/post/references/orientation.md", "README.md", "CHANGELOG.md"]
invariants = ["documented commands match the spec's command list"]
verify = [{run = "rg -n 'post identity (list|show|new|continue|leave)' skills/post/SKILL.md", expect = "exit 0"}]
reversibility = "reversible"
effects = ["docs"]

[review]
tier = "prose"
personas = ["stranger"]
```

```toml task
id = "P.6"
title = "Installed-runtime smoke: two participants, one workspace; acceptance script"
delivers = "scripts/smoke-installed.sh gains the §13 rows 1–10 as an isolated POST_MAIL_ROOT scenario driven through the binary under test with POST_PARTICIPANT and harness keys, plus a version/capabilities receipt; docs/acceptance/build-start.txt records the build start; docs/acceptance/participants-smoke.md records the red-proof"
kind = "build"
blocked_by = ["P.2", "P.3", "P.4"]
acceptance = "scripts/smoke-installed.sh <path-to-binary> exits 0 against the release build and exits non-zero against a copy of the 0.9.0 binary (red-proof recorded in the script's header); every §13 row 1–10 has a named check. Green does not prove cross-host delivery (row 11, coordinator)."
role = "executor"
persona = "repro-first-writer"
skills = ["post"]
owned_files = ["scripts/smoke-installed.sh", "docs/acceptance/build-start.txt", "docs/acceptance/participants-smoke.md"]
invariants = ["the smoke fails against the previous binary"]
verify = [{run = "shellcheck scripts/smoke-installed.sh tests/acceptance.sh", expect = "exit 0"}, {run = "bash tests/acceptance.sh", expect = "exit 0"}]
reversibility = "reversible"
effects = ["code"]

[review]
tier = "standard"
personas = ["test-skeptic", "3am-ops"]
```

```toml task
id = "P.7"
title = "Bridge verification: publish set and unknown-key handling"
delivers = "Decision settled: whether spec §10 (bridge unchanged, new dirs unpublished, unknown keys preserved) holds as written or needs a stated correction. docs/reviews/identity-2026-09-16/bridge-verification.md: from ~/Code/claude-space/post-bridge/sweep.py (byte-identical to the installed sweep), the exact set of paths the bridge publishes and imports, proof that participants/, lineages/, routing/ are outside it, the unknown-envelope-key behavior with line citations, the 4096-byte header bound, and the undeliverable/unknown-room path; any finding that contradicts docs/PARTICIPANTS.md §10 stated as a blocker"
kind = "spike"
blocked_by = []
acceptance = "the report cites sweep.py line numbers for each claim and states explicitly whether §10 holds; it is read-only and changes no file outside its own path. Green does not prove live cross-host delivery."
role = "scout"
persona = "repro-first-writer"
skills = ["post"]
owned_files = ["docs/reviews/identity-2026-09-16/bridge-verification.md"]
invariants = ["no file outside the report is modified"]
verify = [{run = "test -s docs/reviews/identity-2026-09-16/bridge-verification.md", expect = "exit 0"}]
reversibility = "reversible"
effects = ["docs"]

[review]
tier = "prose"
personas = ["provenance-auditor"]
```
