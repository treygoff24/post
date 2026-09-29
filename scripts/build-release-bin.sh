# shellcheck shell=sh
# Sourced by scripts/gate.sh and tests/acceptance.sh (POSIX sh).
#
# Builds the release binary and exports POST_BIN as the path cargo itself reports
# for what it just built. A separate `cargo metadata` lookup can resolve a
# different target slot than the build used (the estate cargo wrapper picks a slot
# per invocation), so the path is taken from the build's own compiler-artifact
# message. Human diagnostics stay on stderr.
build_release_bin() {
    unset POST_BIN
    _brb_json=$(mktemp) || return 1
    if ! cargo build --release --message-format=json-render-diagnostics >"$_brb_json"; then
        rm -f "$_brb_json"
        return 1
    fi
    POST_BIN=$(node -e '
      const lines = require("fs").readFileSync(process.argv[1], "utf8").split("\n");
      let exe = "";
      for (const line of lines) {
        if (!line.startsWith("{")) continue;
        let m; try { m = JSON.parse(line); } catch { continue; }
        if (m.reason === "compiler-artifact" && m.target && m.target.name === "post"
            && (m.target.kind || []).includes("bin") && m.executable) exe = m.executable;
      }
      process.stdout.write(exe);
    ' "$_brb_json")
    rm -f "$_brb_json"
    if [ -z "$POST_BIN" ]; then
        echo "release build reported no executable for bin target post" >&2
        return 1
    fi
    if [ ! -x "$POST_BIN" ]; then
        echo "release build's executable is missing or not executable: $POST_BIN" >&2
        return 1
    fi
    export POST_BIN
}
