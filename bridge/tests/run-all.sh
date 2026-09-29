#!/usr/bin/env bash
# Run every bridge test suite from the repository root: each tests/test_*.py
# module through unittest and each tests/test_*.sh script, several at a time,
# one log per suite. Exits non-zero if any suite fails, printing that suite's
# log. Suites are found by glob, so a new test file is never silently skipped.
#
#   POST_BIN=/path/to/post bridge/tests/run-all.sh
#
# POST_BIN (default: `post` on PATH) must be the post the bridge pins (0.9.0).
# BRIDGE_TEST_JOBS (default 4) caps how many suites run at once. All mail
# roots the suites use are temporary; nothing here touches ~/.claude-mail or
# ~/post-relay.
set -Eeuo pipefail

here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
root=$(CDPATH='' cd -- "$here/../.." && pwd)
cd "$root"

POST_BIN=${POST_BIN:-$(command -v post || true)}
if [ -z "$POST_BIN" ]; then
  echo "bridge tests: no post binary; set POST_BIN" >&2
  exit 1
fi
export POST_BIN
jobs=${BRIDGE_TEST_JOBS:-4}

logs=$(mktemp -d)
trap 'rm -rf "$logs"' EXIT
export logs

run_suite() {
  local suite=$1 name
  name=$(basename "$suite")
  case "$suite" in
    *.py) python3 -m unittest "bridge.tests.${name%.py}" >"$logs/$name.log" 2>&1 ;;
    *.sh) bash "$suite" >"$logs/$name.log" 2>&1 ;;
  esac
  echo $? >"$logs/$name.rc"
}
export -f run_suite

# Largest first, so the long suites start together and the short ones fill in.
# shellcheck disable=SC2012
suites=$(ls -S "$here"/test_*.py "$here"/test_*.sh)
printf '%s\n' "$suites" | xargs -P "$jobs" -I{} bash -c 'run_suite "$1"' _ {}

failed=0
for suite in $suites; do
  name=$(basename "$suite")
  rc=$(cat "$logs/$name.rc" 2>/dev/null || echo missing)
  summary=$(grep -E '^(Ran [0-9]+ tests?|OK|FAILED)' "$logs/$name.log" | tr '\n' ' ' || true)
  if [ "$rc" = 0 ]; then
    printf 'PASS  %-22s %s\n' "$name" "$summary"
  else
    printf 'FAIL  %-22s rc=%s %s\n' "$name" "$rc" "$summary"
    failed=1
  fi
done
if [ "$failed" -ne 0 ]; then
  for suite in $suites; do
    name=$(basename "$suite")
    if [ "$(cat "$logs/$name.rc" 2>/dev/null || echo missing)" != 0 ]; then
      printf '\n=== %s ===\n' "$name" >&2
      cat "$logs/$name.log" >&2
    fi
  done
  echo "bridge tests FAILED" >&2
  exit 1
fi
echo "bridge tests passed"
