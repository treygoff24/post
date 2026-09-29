#!/usr/bin/env bash
# One bounded entry point: full (default), rust, node, bridge, or gate.
set -Eeuo pipefail
cd "$(dirname "$0")/.."
if [ "${POST_TEST_WRAPPED:-}" != 1 ]; then
  exec python3 scripts/test-run.py "$@"
fi
. scripts/test-env.sh
kind=${1:-full}
if [ $# -gt 0 ]; then shift; fi
case "$kind" in
  rust)
    if [ $# -eq 0 ]; then set -- --all-targets; fi
    exec cargo test --all-features "$@" ;;
  gate) exec bash scripts/gate.sh "$@" ;;
  full|node|bridge) ;;
  *) echo "usage: scripts/test.sh [full|rust|node|bridge|gate] [cargo test options]" >&2; exit 64 ;;
esac
if [ $# -gt 0 ]; then echo "extra options are supported only for rust" >&2; exit 64; fi
if [ "$kind" = full ]; then
  python3 scripts/test_runner_test.py
  python3 scripts/with-timeout.py "${GATE_TEST_TIMEOUT:-1200}" cargo test --all-targets --all-features
fi
. scripts/build-release-bin.sh
build_release_bin
if [ "$kind" != bridge ]; then
  node scripts/test-node.mjs skills/post/hooks/*.test.mjs
  node scripts/test-node.mjs launcher/*.test.mjs
fi
if [ "$kind" != node ]; then bridge/tests/run-all.sh; fi
