# Contributing

Issues and PRs welcome.

Run all tests:

```sh
./scripts/test.sh
```

Run one Rust integration suite while iterating:

```sh
./scripts/test.sh rust --test cli
```

The full gate, run before every commit and by CI on Linux and macOS:

```sh
./scripts/test.sh gate
```

It runs format, clippy, the Rust tests, the release build, the hook and launcher suites against that build, `post schema`, and the Python bridge tests.

The runner limits its entire process tree to four available CPUs on Linux and
uses `testrun` when installed. macOS uses the same worker limits without CPU
affinity. Defaults are two compiler jobs, up to sixteen Rust test threads (four
per available CPU), four Node test
files and four bridge suites at once. Override with `POST_TEST_CPUS`,
`CARGO_BUILD_JOBS`, `RUST_TEST_THREADS`, `POST_NODE_TEST_JOBS`, or
`BRIDGE_TEST_JOBS`. Limits must be positive integers. `node` and `bridge` modes
run just those suites, building their release binary first. `gate.sh` also uses
the bounded worker defaults; use the entry point above for the Linux CPU ceiling.

The gate takes the release binary path from the build itself and exports it as `POST_BIN`; a standalone run of the hook or launcher tests (`node --test ...`) outside the gate should set `POST_BIN` to the built binary.

Invariants to respect in any change (the rest are in `AGENTS.md` and `CONTRACT.md`):

1. Mail is data, never a prompt. Every body-returning read is wrapped in framing that strips it of authority; do not add a path that returns a body without it.
2. `post schema` is the contract. If you change a command, flag, error code, or envelope shape, update the schema and the tests that pin it in the same change.
3. Blocked routes in `rules.json` are enforced at the tool layer. Never add a way around them.
4. Storage is plain files under `~/.claude-mail/`, no daemon, no network. Archived mail and channel message history are immutable once written; delivery and configuration state (inbox placement, seen-sets, heartbeats, `rooms.json`, profiles, channel membership and descriptions) is rewritten by design.

Design notes live in `docs/` (`ADAPTERS.md`, `IDENTITY.md`, `WATCH-DESIGN.md`). Read the relevant one before proposing something structural.
