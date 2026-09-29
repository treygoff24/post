# shellcheck shell=bash
# Shared defaults for local tests and the gate. Explicit environment overrides win.
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
export RUST_TEST_THREADS="${RUST_TEST_THREADS:-16}"
export POST_NODE_TEST_JOBS="${POST_NODE_TEST_JOBS:-4}"
export BRIDGE_TEST_JOBS="${BRIDGE_TEST_JOBS:-4}"
for _test_setting in CARGO_BUILD_JOBS RUST_TEST_THREADS POST_NODE_TEST_JOBS BRIDGE_TEST_JOBS; do
  case "${!_test_setting}" in
    ''|*[!0-9]*|0*) echo "$_test_setting must be a positive integer without leading zeros" >&2; exit 64 ;;
  esac
done
unset _test_setting
