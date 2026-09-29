#!/usr/bin/env bash
# Canonical gate for post. Run before calling anything done.
#
# The steps are CONTRIBUTING.md's list, which until now lived only as prose: seven
# commands a human was expected to remember and type. "I ran the checks" then meant
# whatever each contributor recalled, and the failure mode is silent -- a gate that
# asks six of seven questions reads exactly like one that asked all seven. The same
# gap cost delegate-agent a shipped-unformatted commit on 2026-08-25.
#
# The verdict line prints the toolchain that produced it, because a green gate on
# one machine and a red one on another is otherwise an unattributable argument
# (shellcheck 0.10 vs 0.11 and gitleaks 8.16 vs 8.30 each cost us a round trip).
set -Eeuo pipefail
cd "$(dirname "$0")/.."

fail=0
err() { printf 'GATE FAIL: %s\n' "$*" >&2; fail=1; }
step() { printf '\n=== %s ===\n' "$*"; }

step "fmt";     cargo fmt --check || err "cargo fmt --check (run: cargo fmt)"
step "clippy";  cargo clippy --all-targets --all-features -- -D warnings || err "clippy"
# A hung test (a watcher that never exits, a lock that is never released) used
# to hold the gate, the shared machine lock, and every session waiting on it
# forever. The whole suite takes about two minutes on an idle machine; the
# default limit is ten times that. GATE_TEST_TIMEOUT (seconds) overrides it.
# macOS has no `timeout`, so scripts/with-timeout.py does the job, killing the
# test binaries' process group with the cargo that started them.
test_timeout="${GATE_TEST_TIMEOUT:-1200}"
step "test";    python3 scripts/with-timeout.py "$test_timeout" cargo test --all-targets --all-features \
  || err "cargo test (failed, or exceeded the ${test_timeout} s limit: GATE_TEST_TIMEOUT)"
step "release"; cargo build --release || err "cargo build --release"

# The launcher and hook suites exercise the release binary built above, so they
# run after it and not before.
#
# The contract suite (skills/post/hooks/contract.test.mjs) takes its samples from
# `post contract samples` of the binary under test, so it is pointed at exactly
# this release build rather than a guessed path.
if release_bin=$(node scripts/cargo-release-bin.mjs); then
  export POST_BIN="$release_bin"
else
  err "resolve release binary via cargo metadata"
fi
if command -v node >/dev/null 2>&1; then
  step "node: hooks"
  node --test skills/post/hooks/*.test.mjs || err "node hooks tests"
  step "node: launcher"
  node --test launcher/*.test.mjs || err "node launcher tests"
else
  err "node not found; the hook and launcher suites are part of this gate"
fi

# CONTRIBUTING invariant 2: post schema is the contract. A schema that cannot be
# emitted is a broken contract regardless of what the unit tests say.
step "schema"
if [ -n "${POST_BIN:-}" ]; then
  "$POST_BIN" schema >/dev/null || err "post schema"
else
  err "no release binary to emit the schema"
fi

rust_ver=$(cargo --version 2>/dev/null | awk '{print $2}')
node_ver=$(node --version 2>/dev/null || echo "absent")
py_ver=$(python3 --version 2>/dev/null | awk '{print $2}' || echo "absent")
if [ "$fail" -eq 0 ]; then
  printf '\nGATE PASS [cargo %s, node %s, python %s]\n' "$rust_ver" "$node_ver" "$py_ver"
else
  printf '\nGATE FAILED [cargo %s, node %s, python %s]\n' "$rust_ver" "$node_ver" "$py_ver" >&2
  exit 1
fi
