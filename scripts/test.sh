#!/usr/bin/env bash
# One bounded entry point: full (default), rust, node, bridge, or gate.
set -Eeuo pipefail
cd "$(dirname "$0")/.."
. scripts/test-env.sh
if [ "${POST_TEST_WRAPPED:-}" != 1 ]; then
  exec python3 scripts/test-run.py "$@"
fi
kind=${1:-full}
if [ $# -gt 0 ]; then shift; fi
case "$kind" in
  rust) exec cargo test --all-targets --all-features "$@" ;;
  gate) exec bash scripts/gate.sh "$@" ;;
  full|node|bridge) ;;
  *) echo "usage: scripts/test.sh [full|rust|node|bridge|gate] [cargo test options]" >&2; exit 64 ;;
esac
if [ $# -gt 0 ]; then echo "extra options are supported only for rust" >&2; exit 64; fi
if [ "$kind" = full ]; then
  python3 scripts/with-timeout.py "${GATE_TEST_TIMEOUT:-1200}" cargo test --all-targets --all-features
fi
. scripts/build-release-bin.sh
build_release_bin
if [ "$kind" != bridge ]; then
  node --test --test-concurrency="$POST_NODE_TEST_JOBS" skills/post/hooks/*.test.mjs
  node --test --test-concurrency="$POST_NODE_TEST_JOBS" launcher/*.test.mjs
fi
if [ "$kind" != node ]; then bridge/tests/run-all.sh; fi
