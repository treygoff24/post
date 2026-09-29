#!/usr/bin/env sh
# Plan B acceptance: the goal lock's close condition (docs/plans/plan-b-goal-lock.md).
# The seven runnable observations live in scripts/smoke-installed.sh (extended by
# task B8); the canonical gate is scripts/gate.sh. Green here means: release
# binary builds, all seven acceptance rows pass against a throwaway store the
# smoke mints itself, and the full gate passes.
set -eu
cd "$(dirname "$0")/.."
if grep -nE '^[[:space:]]*/[^[:space:]]*/cp[[:space:]]' scripts/smoke-installed.sh >&2; then
    printf '%s\n' 'acceptance: smoke helper must resolve cp from PATH' >&2
    exit 1
fi
cargo build --release
bash scripts/smoke-installed.sh "$(node scripts/cargo-release-bin.mjs)"
bash scripts/gate.sh
