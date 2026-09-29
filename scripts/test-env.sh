# Shared defaults for local tests and the gate. Explicit environment overrides win.
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
export RUST_TEST_THREADS="${RUST_TEST_THREADS:-4}"
export POST_NODE_TEST_JOBS="${POST_NODE_TEST_JOBS:-2}"
export BRIDGE_TEST_JOBS="${BRIDGE_TEST_JOBS:-2}"
