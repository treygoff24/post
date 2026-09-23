#!/usr/bin/env bash
# Product integration smoke for one post binary, before it is installed.
#
#   scripts/install-smoke.sh [--results FILE] <post-bin>
#
# Runs the consumers that break when post's output changes against <post-bin>
# and a throwaway store (a temporary POST_MAIL_ROOT and HOME; nothing real is
# read or written):
#   - the binary answers version --json and emits its contract samples;
#   - the doorbell's own parsers read a real `post rooms` listing and a real
#     `post watch --snapshot` holding one delivered mail;
#   - the doorbell's contract suite passes against the binary's samples;
#   - Porch's launch check accepts the binary (PORCH_PYTHON selects the
#     interpreter that has porch3).
# Every consumer calls `post` by name, so <post-bin> is put first on PATH.
#
# A check that cannot run is skipped, and a skip is a failure unless the
# operator allows it by id: POST_SMOKE_ALLOW_SKIP=porch (comma-separated; porch
# is the only check that can skip, when no porch3 is installed). An allowed
# skip ends in PASS_WITH_SKIPS, never PASS.
#
# --results FILE writes one JSON object per check, one per line, to FILE
# (truncated first): {"check": <id>, "result": "pass"|"fail"|"skipped",
# "detail": <text>} plus "allowed": true|false on a skip. Check ids: setup,
# version, samples, doorbell_parsers, doorbell_contract, porch.
# install-post.sh reads this file for the receipt.
#
# Exit 0 when every check passed or skipped with permission, 1 when any
# failed or skipped without permission, 2 on usage.
# launcher/install does not call this: the launcher stays independent of Porch.
set -Eeuo pipefail

usage="usage: install-smoke.sh [--results FILE] <post-bin>"
results=""
if [ "${1:-}" = --results ]; then
  [ "$#" -ge 2 ] || { echo "$usage" >&2; exit 2; }
  results="$2"
  shift 2
fi
[ "$#" -eq 1 ] || { echo "$usage" >&2; exit 2; }
if [ -n "$results" ]; then
  : > "$results" || { echo "install-smoke: cannot write results file: $results" >&2; exit 2; }
fi
case "$1" in /*) bin="$1" ;; *) bin="$PWD/$1" ;; esac
[ -x "$bin" ] && [ -f "$bin" ] || { echo "install-smoke: not an executable file: $bin" >&2; exit 2; }
repo="$(cd "$(dirname "$0")/.." && pwd)"

# macOS TMPDIR ends in "/"; trim it so no path here has a doubled slash, which
# Porch normalizes and post does not, and the porch check would then fail.
tmp_root="${TMPDIR:-/tmp}"
work=$(mktemp -d "${tmp_root%/}/post-install-smoke.XXXXXX")
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin" "$work/home" "$work/smoke" "$work/sender"
ln -s "$bin" "$work/bin/post"
HOME_REAL="$HOME"
export PATH="$work/bin:$PATH" HOME="$work/home" POST_MAIL_ROOT="$work/mail"
# Nothing from the caller's session may choose an identity here.
unset POST_PARTICIPANT POST_FROM POST_SENDER_ADDRESS POST_HARNESS POST_ARX_GENERATION \
  CLAUDE_CODE_SESSION_ID CLAUDE_PID CODEX_THREAD_ID CODEX_SESSION_ID POST_FRAMING POST_WATCH_PROFILE

failed=0
skipped=""
# record <id> <result> <detail> [allowed]: one JSON line in the results file.
record() {
  [ -n "$results" ] || return 0
  python3 -c '
import json, sys
entry = {"check": sys.argv[1], "result": sys.argv[2], "detail": sys.argv[3]}
if sys.argv[2] == "skipped":
    entry["allowed"] = sys.argv[4] == "1"
print(json.dumps(entry))
' "$1" "$2" "$3" "${4:-0}" >> "$results"
}
# pass|fail <id> <label> [detail]
pass() { printf 'smoke: %-28s ok\n' "$2"; record "$1" pass ""; }
fail() { printf 'smoke: %-28s FAIL %s\n' "$2" "${3:-}" >&2; record "$1" fail "${3:-}"; failed=1; }
# skip <id> <label> <reason>: fails the smoke unless the id is allowed.
skip() {
  case ",${POST_SMOKE_ALLOW_SKIP:-}," in
    *",$1,"*)
      printf 'smoke: %-28s skipped (allowed by POST_SMOKE_ALLOW_SKIP): %s\n' "$2" "$3"
      record "$1" skipped "$3" 1
      skipped="${skipped:+$skipped, }$1"
      ;;
    *)
      printf 'smoke: %-28s FAIL skipped: %s (set POST_SMOKE_ALLOW_SKIP=%s to accept the skip)\n' "$2" "$3" "$1" >&2
      record "$1" skipped "$3" 0
      failed=1
      ;;
  esac
}

# A store with two rooms, a bound reader, and one mail delivered to it.
setup() {
  post rooms >/dev/null &&
    post rooms add smoke "$work/smoke" >/dev/null &&
    post rooms add sender "$work/sender" >/dev/null &&
    reader=$(cd "$work/smoke" && post participant bind --workspace smoke --harness claude --key install-smoke-reader --json |
      python3 -c 'import json,sys; print(json.load(sys.stdin)["participant"]["id"])') &&
    sender=$(cd "$work/sender" && post participant bind --workspace sender --harness codex --key install-smoke-sender --json |
      python3 -c 'import json,sys; print(json.load(sys.stdin)["participant"]["id"])') &&
    (cd "$work/sender" && POST_PARTICIPANT="$sender" post send --to smoke --subject smoke --body "install smoke" --json >/dev/null) </dev/null
}
if setup >"$work/setup.log" 2>&1; then
  pass setup "store setup"
else
  fail setup "store setup" "$(tail -3 "$work/setup.log")"
  echo "install-smoke: FAILED for $bin" >&2
  exit 1
fi
export SMOKE_READER="$reader"

if post version --json | python3 -c '
import json, sys
v = json.load(sys.stdin)
assert v["ok"] is True and isinstance(v["build_sha"], str) and "participants" in v["capabilities"], v
' >"$work/version.log" 2>&1; then pass version "version --json"; else fail version "version --json" "$(tail -2 "$work/version.log")"; fi

if post contract samples --dir "$work/samples" >/dev/null 2>"$work/samples.log" && [ -s "$work/samples/watch-snapshot.jsonl" ]; then
  pass samples "contract samples"
else
  fail samples "contract samples" "$(tail -2 "$work/samples.log")"
fi

# The doorbell's real parsers against the real store: the room listing it
# validates --room against, and the snapshot it keys wakes from.
if (cd "$work/smoke" && POST_PARTICIPANT="$reader" python3 - "$repo/doorbell/post-doorbell" <<'PY'
import importlib.machinery, importlib.util, os, sys
loader = importlib.machinery.SourceFileLoader("doorbell", sys.argv[1])
spec = importlib.util.spec_from_loader("doorbell", loader)
doorbell = importlib.util.module_from_spec(spec)
loader.exec_module(doorbell)
rooms = doorbell.registered_rooms(os.getcwd())
assert rooms is not None and {"smoke", "sender"} <= rooms, f"rooms listing: {rooms!r}"
current = doorbell.snapshot_events(["post", "watch"], os.getcwd(), {"mail"})
assert current is not None, "snapshot unreadable"
assert list(current.values()) == ["mail"], f"expected one delivered mail, got {current!r}"
(key,) = current
assert key[0] == "mail" and key[1] == "smoke", key
PY
) >"$work/doorbell.log" 2>&1; then
  pass doorbell_parsers "doorbell parsers (live)"
else
  fail doorbell_parsers "doorbell parsers (live)" "$(tail -3 "$work/doorbell.log")"
fi

if (cd "$repo/doorbell" && POST_BIN="$bin" python3 -m unittest test_contract) >"$work/doorbell-contract.log" 2>&1; then
  pass doorbell_contract "doorbell contract suite"
else
  fail doorbell_contract "doorbell contract suite" "$(tail -3 "$work/doorbell-contract.log")"
fi

# Porch's launch check: the sequence porch3.app.main runs before the TUI
# starts (acting room, owner crosscheck, human binding, acting room again),
# against a room whose owner was initialized with Porch's default marker.
# Porch finds post by name on PATH, so it runs <post-bin>. A soft-failed owner
# crosscheck (signing disabled) is a failure here: in this store the owner
# record is fresh and must agree.
porch_python="${PORCH_PYTHON:-$HOME_REAL/.local/share/uv/tools/porch3/bin/python}"
if [ ! -x "$porch_python" ] || ! "$porch_python" -c 'import porch3.app' >/dev/null 2>&1; then
  skip porch "porch launch check" "no porch3 at $porch_python; set PORCH_PYTHON"
elif (mkdir -p "$work/porch" && cd "$work/porch" &&
  post rooms add porch "$work/porch" --json >/dev/null &&
  post owner init --room porch --marker 🦊 --json >/dev/null &&
  "$porch_python" - porch "$work/porch" <<'PY'
import os, sys
from porch3.config import build_config
from porch3.participant import bind_owner
from porch3.roomcheck import RoomInvariantError, apply_owner_crosscheck, assert_acting_room
config = build_config(owner_room=sys.argv[1], owner_room_dir=sys.argv[2], mail_root=os.environ["POST_MAIL_ROOT"])
try:
    assert_acting_room(config)
    config = apply_owner_crosscheck(config)
    config = bind_owner(config)
    assert_acting_room(config)
except RoomInvariantError as error:
    sys.exit(f"room invariant: {error}")
if config.signing_disabled:
    sys.exit(f"signing disabled: {config.signing_disabled_reason}")
assert config.post_participant and config.post_participant.startswith("porch-"), config.post_participant
PY
) </dev/null >"$work/porch.log" 2>&1; then
  pass porch "porch launch check"
else
  fail porch "porch launch check" "$(tail -3 "$work/porch.log")"
fi

if [ "$failed" -eq 0 ] && [ -n "$skipped" ]; then
  echo "install-smoke: PASS_WITH_SKIPS (skipped: $skipped) $("$bin" version)"
elif [ "$failed" -eq 0 ]; then
  echo "install-smoke: PASS $("$bin" version)"
else
  echo "install-smoke: FAILED for $bin" >&2
  exit 1
fi
