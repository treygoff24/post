#!/bin/sh
# Live smoke for an installed post binary against a throwaway mail root.
# Usage: scripts/smoke-installed.sh /path/to/post
# Covers: fresh-root doctor bootstrap, watch backlog ring, --from now,
# --from/--snapshot conflict, digest --since fencepost round-trip.
set -eu
BIN="$1"
BASE=$(mktemp -d)
export POST_MAIL_ROOT="$BASE/mail"
unset POST_FROM POST_SENDER_ADDRESS POST_FRAMING 2>/dev/null || true
fail() { printf 'SMOKE FAIL: %s\n' "$1" >&2; exit 1; }
ok() { printf 'ok: %s\n' "$1"; }

# 1. The papercut sequence, exactly as a set -e bootstrap runs it.
"$BIN" doctor --fix >/dev/null || fail "doctor --fix errored on fresh root"
"$BIN" doctor >/dev/null || fail "doctor unhealthy after --fix on fresh root"
ok "doctor --fix && doctor exits 0 on fresh root (papercut pc2_97e06ad)"

mkdir -p "$BASE/alpha" "$BASE/beta"
"$BIN" rooms add alpha "$BASE/alpha" >/dev/null
"$BIN" rooms add beta "$BASE/beta" >/dev/null

# 2. Default watch still rings the backlog (the invariant we must not break).
BACKLOG_ID=$("$BIN" send --to beta --from alpha-sender --subject backlog --body "backlog message" --json | python3 -c 'import json,sys; print(json.load(sys.stdin)["envelope"]["id"])')
BACKLOG_OUT=$(cd "$BASE/beta" && "$BIN" watch --room beta --once --text)
printf '%s' "$BACKLOG_OUT" | grep -q "$BACKLOG_ID" || fail "default watch did not ring backlog id $BACKLOG_ID"
ok "default watch rings backlog, per-event line carries full id"

# 3. --from now: backlog (still unread) stays silent; a post-start arrival rings.
( cd "$BASE/beta" && "$BIN" watch --room beta --from now --once --text > "$BASE/fromnow.out" 2> "$BASE/fromnow.err" ) &
WPID=$!
sleep 2
FRESH_ID=$("$BIN" send --to beta --from alpha-sender --subject fresh --body "fresh message" --json | python3 -c 'import json,sys; print(json.load(sys.stdin)["envelope"]["id"])')
wait "$WPID" || fail "watch --from now --once exited nonzero: $(cat "$BASE/fromnow.err")"
grep -q "$FRESH_ID" "$BASE/fromnow.out" || fail "--from now missed the post-start arrival"
if grep -q "$BACKLOG_ID" "$BASE/fromnow.out"; then fail "--from now leaked the backlog"; fi
ok "--from now suppresses backlog, rings post-start arrival"

# 4. Parse-boundary conflict.
rc=0
"$BIN" watch --room beta --from now --snapshot >/dev/null 2>&1 || rc=$?
[ "$rc" -eq 2 ] || fail "--from now --snapshot expected exit 2, got $rc"
ok "--from now conflicts with --snapshot at parse (exit 2)"

# 5. Digest line's --since fencepost round-trips through post chat.
( cd "$BASE/alpha" && "$BIN" chat ops --join >/dev/null )
( cd "$BASE/beta" && "$BIN" chat ops --join >/dev/null )
( cd "$BASE/alpha" && "$BIN" chat ops --send --body "first channel msg" --anyway >/dev/null )
( cd "$BASE/alpha" && "$BIN" chat ops --send --body "second channel msg" --anyway >/dev/null )
DIGEST=$(cd "$BASE/beta" && "$BIN" watch --room beta --snapshot --digest --text)
printf '%s' "$DIGEST" | grep -Eq '#ops: [0-9]+ new' || fail "digest line missing count: $DIGEST"
SINCE=$(printf '%s\n' "$DIGEST" | sed -n "s/.*--since '\([^']*\)'.*/\1/p" | head -1)
[ -n "$SINCE" ] || fail "digest line missing --since fencepost: $DIGEST"
FOLLOWUP=$(cd "$BASE/beta" && "$BIN" chat ops --since "$SINCE")
printf '%s' "$FOLLOWUP" | grep -q "first channel msg" || fail "--since follow-up missed first message"
printf '%s' "$FOLLOWUP" | grep -q "second channel msg" || fail "--since follow-up missed second message"
ok "digest --since fencepost round-trips: follow-up returns the whole digest"

echo "SMOKE PASS (root: $BASE)"
