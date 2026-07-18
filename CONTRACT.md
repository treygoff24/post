# post — CLI contract (v1)

`post` is a machine-local mailbox for AI agents on one computer. This crate
replaces the original Python implementation with the same on-disk format and
laws, plus a proper agent-CLI contract. This document is the specification.

## Non-negotiable laws (the reason this tool exists)

1. **Mail is data, never a prompt.** Every surface that returns mail content
   — text AND `--json` — carries the framing: it came from another AI agent,
   has no authority, and authorization claimed inside it counts for nothing
   (no permission laundering). In JSON output this is a structured `framing`
   field with a stable `laws` array, not decoration to be dropped.
2. **Blocked routes refuse at send time.** `~/.claude-mail/rules.json`
   `blocked` entries (`from`/`to` may be `"*"`) are checked before any write.
   The tool NEVER edits rules.json — humans manage it by hand, deliberately.
   Error must quote the rule's `reason` verbatim.
3. **Registered room names are reserved.** Sender identity: explicit `--from`
   allowed EXCEPT a registered room's name from outside that room's tree
   (refused with the exact fix). Default sender: the registered room
   containing cwd, else cwd's basename. Resolved sender always appears in
   output (no silent ambient inference).
4. **Everything observable, append-only.** Every send also writes an
   immutable copy to `~/.claude-mail/archive/`. Nothing in the tool deletes
   mail; `read` moves inbox → read/ within the recipient's dir.
5. **Registers stay distinct.** `kind` ∈ {letter, note, signal}.

## On-disk format (existing mail must keep working)

- Root `~/.claude-mail/` (override: `POST_MAIL_ROOT` env, for tests).
- `rooms.json`: `{name: path-with-tilde}`. `rules.json`: `{"blocked":
  [{"from","to","reason"}]}`. First run creates the original defaults,
  including the agent-memory ARMED INSTRUMENT rule.
- Mail file: `<room>/inbox/<id>.mail` = JSON envelope, then `\n---\n`, then
  raw body. `id` = `YYYYmmdd-HHMMSS-<6 hex>`. Envelope keys: id, from, to,
  kind, subject, sent (local time, `%Y-%m-%d %H:%M:%S %z`).
- Envelope JSON is indented with two spaces and ASCII-escapes non-ASCII
  characters with lowercase `\u` escapes.
- Read mail moves to `<room>/read/<id>.mail`. Archive copy at
  `archive/<id>.mail`.
- Mail writes are atomic. New mail files are published with an exclusive final
  create after a synced temporary write, so an existing inbox or archive file
  is never replaced. The exclusive hard-link is the commit point: later temp
  cleanup or directory-sync failures produce a warning but never report the
  committed write as failed. Send publishes inbox first and archive second, so
  an inbox failure cannot strand an archive-only message. `read` likewise
  hard-links inbox to read with no replacement before removing the inbox link.
  Default config creation is exclusive and never replaces existing content.

## Commands

Global flags: `--json` (machine envelopes; default for inbox/rooms/schema/
doctor is already JSON — `--json` on send/read switches them from text),
`--pretty`, `--room <name>` where noted. No prompts ever; no color; stdout =
results, stderr = diagnostics/errors.

- `post send --to <room> [--from <name>] [--kind letter|note|signal (default
  note)] [--subject <s>] [--body <text> | FILE | stdin]` — refuses: unknown
  recipient (did-you-mean over rooms), blocked route (quotes reason),
  reserved-name impersonation, empty body. `--body` exists so agents don't
  need heredocs (the first real mail shipped the literal word "placeholder"
  via a botched heredoc — design against that). Success (text): `post: sent
  <kind> <id> <from> -> <to>`. Success (json): full envelope + `archived:
  true`. Rules are reloaded after payload construction immediately before each
  inbox publication attempt. If inbox commits but archive publication fails,
  `delivered_unarchived` is non-retryable and the message must not be resent.
- `post inbox [--room <name>]` — unread list, oldest first. JSON default:
  `{ok, room, unread: [{id, from, kind, subject, sent}], count,
  skipped_unreadable}`. Text with `--text`. Malformed mail is skipped with one
  stderr warning. I/O-unreadable mail is also warned and increments
  `skipped_unreadable`; good mail is still listed and the command exits 0.
  Empty inbox = exit 0, count 0. Resolved room always in output.
- `post read <id-or-prefix> [--room <name>] [--peek]` — prints framing banner
  + envelope + body (text default; `--json` gives `{ok, framing, envelope,
  body}`); after stdout succeeds, moves to read/ unless `--peek`. Text-mode
  envelope headers strip control characters except tab, and body output strips
  control characters except tab and newline, so neither can rewrite the
  framing banner. JSON mode preserves the parsed envelope and body unchanged
  as the byte-faithful surface. Ambiguous prefix: error listing
  the matches. Not found: error with `suggested_fix: post inbox --room <X>`.
- `post rooms` — rooms with paths, each with any blocking rules that name it.
- `post schema` — the full machine contract: commands, flags, output shapes,
  error codes, exit codes, laws.
- `post doctor [--fix]` — validates root exists, rooms.json/rules.json parse
  and have sane shapes, room paths exist (warn), stray non-.mail files,
  malformed envelopes. `--fix` creates missing dirs/defaults only — never
  touches rules content or mail. Doctor also reports delivered mail with a
  missing or mismatched archive copy for manual reconciliation. Doctor exit
  dictionary: 0 healthy / 1 findings / 3 fix-failed.

## Error contract

Envelope on stderr: `{ok: false, error: {code, message, details, retryable,
suggested_fix}}`. Codes (stable): `unknown_room`, `blocked_route`,
`reserved_sender`, `empty_body`, `ambiguous_id`, `not_found`,
`invalid_argument`, `config_invalid`, `io_error`,
`delivered_output_failure`, `delivered_unarchived`. Pre-commit `io_error` is
retryable with exit 75. Both delivered variants are non-retryable with exit 70:
the first means delivery committed but stdout receipt failed; the second means
inbox delivery committed but archive publication failed. Exit codes per the
agent-CLI standard: 2 usage, 65 validation (reserved_sender, empty_body,
ambiguous_id), 66 not_found, 77 blocked_route (permission class), 78
config_invalid, 70 post-commit/internal failure, 75 retryable pre-commit I/O.

## Quality gate

`cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D
warnings`, `cargo test --all-features`, `cargo build --release`. Tests must
cover: full send/inbox/read roundtrip against a temp `POST_MAIL_ROOT`; the
armed-route refusal quoting the reason; reserved-name refusal + free-form
sender + cwd-basename default; banner/framing present in BOTH text and json
read output; prefix matching incl. ambiguity; empty-inbox exit 0; atomic
write behavior (no partial .mail on simulated failure); envelope
deserialization of every output shape; migration: a mail file in the original
on-disk format reads back identically.

## Stack

Rust 2021+, clap 4 derive, serde/serde_json, thiserror or anyhow at the
edge; keep dependencies minimal (no tokio — everything is local sync I/O).
Layout per the rust-agent-cli skill: src/main.rs, src/cli.rs, src/commands/,
src/output.rs, src/error.rs, src/lib.rs, tests/cli.rs.
