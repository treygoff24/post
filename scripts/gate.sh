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
step "test";    cargo test --all-targets --all-features || err "cargo test"
step "release"; cargo build --release || err "cargo build --release"

# The launcher and hook suites exercise the release binary built above, so they
# run after it and not before.
if command -v node >/dev/null 2>&1; then
  step "node: hooks"
  node --test skills/post/hooks/*.test.mjs || err "node hooks tests"
  step "node: launcher"
  node --test launcher/*.test.mjs || err "node launcher tests"
else
  err "node not found; the hook and launcher suites are part of this gate"
fi

# The doorbell is a Python daemon that ships in this repo; its suite is the only
# thing standing between "the unit file installs" and "the doorbell rings", and a
# gate that skips it is exactly the six-of-seven gate this file's header warns
# about. It runs from its own directory because its tests resolve the daemon
# beside them.
if command -v python3 >/dev/null 2>&1; then
  step "python: doorbell"
  ( cd doorbell && python3 -m unittest discover -p 'test_*.py' ) || err "doorbell tests"
else
  err "python3 not found; the doorbell suite is part of this gate"
fi

# CONTRIBUTING invariant 2: post schema is the contract. A schema that cannot be
# emitted is a broken contract regardless of what the unit tests say.
step "schema"
if release_bin=$(node scripts/cargo-release-bin.mjs); then
  "$release_bin" schema >/dev/null || err "post schema"
else
  err "resolve release binary via cargo metadata"
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
