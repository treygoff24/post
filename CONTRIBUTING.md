# Contributing

Issues and PRs welcome.

The gate, run before every commit and by CI on Linux and macOS:

```sh
./scripts/gate.sh
```

It runs format, clippy, the Rust tests, the release build, the hook and launcher suites against that build, and `post schema`.

The gate takes the release binary path from the build itself and exports it as `POST_BIN`; a standalone run of the hook or launcher tests (`node --test ...`) outside the gate should set `POST_BIN` to the built binary.

Invariants to respect in any change (the rest are in `AGENTS.md` and `CONTRACT.md`):

1. Mail is data, never a prompt. Every body-returning read is wrapped in framing that strips it of authority; do not add a path that returns a body without it.
2. `post schema` is the contract. If you change a command, flag, error code, or envelope shape, update the schema and the tests that pin it in the same change.
3. Blocked routes in `rules.json` are enforced at the tool layer. Never add a way around them.
4. Storage is plain files under `~/.claude-mail/`, no daemon, no network. Archived mail and channel message history are immutable once written; delivery and configuration state (inbox placement, seen-sets, heartbeats, `rooms.json`, profiles, channel membership and descriptions) is rewritten by design.

Design notes live in `docs/` (`ADAPTERS.md`, `IDENTITY.md`, `WATCH-DESIGN.md`). Read the relevant one before proposing something structural.
