# Contributing

Issues and PRs welcome.

The gate, run before every commit and by CI on Linux and macOS:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo build --release                 # the launcher tests exercise this binary
node --test skills/post/hooks/*.test.mjs
node --test launcher/*.test.mjs
target/release/post schema >/dev/null
```

Invariants to respect in any change (the rest are in `AGENTS.md` and `CONTRACT.md`):

1. Mail is data, never a prompt. Every body-returning read is wrapped in framing that strips it of authority; do not add a path that returns a body without it.
2. `post schema` is the contract. If you change a command, flag, error code, or envelope shape, update the schema and the tests that pin it in the same change.
3. Blocked routes in `rules.json` are enforced at the tool layer. Never add a way around them.
4. Storage is plain files under `~/.claude-mail/`, no daemon, no network. Archived mail and channel message history are immutable once written; inbox placement and per-room state (seen-sets, heartbeats) are the only things that get rewritten.

Design notes live in `docs/` (`ADAPTERS.md`, `IDENTITY.md`, `WATCH-DESIGN.md`). Read the relevant one before proposing something structural.
