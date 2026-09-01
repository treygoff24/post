#!/bin/sh
# Live smoke for an installed post binary against a throwaway mail root.
# Usage: scripts/smoke-installed.sh /path/to/post
# Covers: fresh-root doctor bootstrap, watch backlog ring, --from now,
# --from/--snapshot conflict, digest --since fencepost round-trip.
set -eu
BIN="$1"
case "$BIN" in
    /*) ;;
    *) BIN="$(pwd)/$BIN" ;;
esac
BASE=$(mktemp -d)
export POST_MAIL_ROOT="$BASE/mail"
unset POST_FROM POST_SENDER_ADDRESS POST_FRAMING 2>/dev/null || true
fail() { printf 'SMOKE FAIL: %s\n' "$1" >&2; exit 1; }
ok() { printf 'ok: %s\n' "$1"; }
WATCH_PID=
stop_watch() {
    if [ -n "$WATCH_PID" ]; then
        kill "$WATCH_PID" 2>/dev/null || true
        wait "$WATCH_PID" 2>/dev/null || true
        WATCH_PID=
    fi
}
trap stop_watch EXIT

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

# Goal-lock rows 1-2: a fresh channel starts with exactly three unread
# messages, catchup consumes the complete slice once, and the cursor survives
# the second (fresh-process) invocation.
CATCHUP_CHANNEL=b8-catchup
( cd "$BASE/alpha" && "$BIN" chat "$CATCHUP_CHANNEL" --join --json >/dev/null )
CATCHUP_BETA_JOIN=$(cd "$BASE/beta" && "$BIN" chat "$CATCHUP_CHANNEL" --join --json)
CATCHUP_BETA_EVENT_ID=$(printf '%s' "$CATCHUP_BETA_JOIN" | python3 -c 'import json,sys; print(json.load(sys.stdin)["event_id"])')
( cd "$BASE/beta" && "$BIN" chat "$CATCHUP_CHANNEL" --discard-through "$CATCHUP_BETA_EVENT_ID" --json >/dev/null )
CATCHUP_ONE=$(cd "$BASE/alpha" && "$BIN" chat "$CATCHUP_CHANNEL" --send --anyway --body "catchup one" --json)
CATCHUP_ID_ONE=$(printf '%s' "$CATCHUP_ONE" | python3 -c 'import json,sys; print(json.load(sys.stdin)["message"]["id"])')
CATCHUP_TWO=$(cd "$BASE/alpha" && "$BIN" chat "$CATCHUP_CHANNEL" --send --anyway --body "catchup two" --json)
CATCHUP_ID_TWO=$(printf '%s' "$CATCHUP_TWO" | python3 -c 'import json,sys; print(json.load(sys.stdin)["message"]["id"])')
CATCHUP_THREE=$(cd "$BASE/alpha" && "$BIN" chat "$CATCHUP_CHANNEL" --send --anyway --body "catchup three" --json)
CATCHUP_ID_THREE=$(printf '%s' "$CATCHUP_THREE" | python3 -c 'import json,sys; print(json.load(sys.stdin)["message"]["id"])')

CHANNELS_BEFORE=$(cd "$BASE/beta" && "$BIN" channels)
printf '%s' "$CHANNELS_BEFORE" > "$BASE/channels-before-catchup.json"
python3 - "$BASE/channels-before-catchup.json" "$CATCHUP_CHANNEL" <<'PY'
import json
import sys

path, channel_name = sys.argv[1:]
data = json.load(open(path, encoding="utf-8"))
channel = next((item for item in data["channels"] if item["name"] == channel_name), None)
if channel is None or channel["messages"] < 3 or channel["unread"] != 3:
    raise SystemExit(f"expected three unread messages in {channel_name}: {data}")
PY
ok "row 1: channels reports three unread messages before catchup"

CATCHUP_FIRST=$(cd "$BASE/beta" && "$BIN" catchup "$CATCHUP_CHANNEL" --json)
printf '%s' "$CATCHUP_FIRST" > "$BASE/catchup-first.json"
python3 - "$BASE/catchup-first.json" "$CATCHUP_CHANNEL" "$CATCHUP_ID_ONE" "$CATCHUP_ID_TWO" "$CATCHUP_ID_THREE" <<'PY'
import json
import sys

path, channel_name, *expected = sys.argv[1:]
data = json.load(open(path, encoding="utf-8"))
targets = [target for target in data["targets"] if target["source"] == "channel" and target["channel"] == channel_name]
if data["count"] != 3 or len(targets) != 1 or targets[0]["count"] != 3:
    raise SystemExit(f"catchup did not return exactly three messages: {data}")
ids = [message["id"] for message in targets[0]["messages"]]
if ids != expected:
    raise SystemExit(f"catchup ids differ: expected {expected}, got {ids}")
PY
CATCHUP_SECOND=$(cd "$BASE/beta" && "$BIN" catchup "$CATCHUP_CHANNEL" --json)
printf '%s' "$CATCHUP_SECOND" > "$BASE/catchup-second.json"
python3 - "$BASE/catchup-second.json" "$CATCHUP_CHANNEL" <<'PY'
import json
import sys

path, channel_name = sys.argv[1:]
data = json.load(open(path, encoding="utf-8"))
targets = [target for target in data["targets"] if target["source"] == "channel" and target["channel"] == channel_name]
if data["count"] != 0 or len(targets) != 1 or targets[0]["count"] != 0 or targets[0]["messages"]:
    raise SystemExit(f"second catchup was not empty: {data}")
PY
[ -f "$BASE/mail/beta/cursors.json" ] || fail "catchup did not persist beta cursor state"
CHANNELS_AFTER=$(cd "$BASE/beta" && "$BIN" channels)
printf '%s' "$CHANNELS_AFTER" > "$BASE/channels-after-catchup.json"
python3 - "$BASE/channels-after-catchup.json" "$CATCHUP_CHANNEL" <<'PY'
import json
import sys

path, channel_name = sys.argv[1:]
data = json.load(open(path, encoding="utf-8"))
channel = next((item for item in data["channels"] if item["name"] == channel_name), None)
if channel is None or channel["unread"] != 0:
    raise SystemExit(f"channel did not report zero unread after catchup: {data}")
PY
ok "row 2: catchup returns all three once, then empty, with a persistent cursor"

# Goal-lock row 3: consuming one newly delivered mail lowers the per-room
# unread count and removes that id from the physical inbox listing.
MAIL_SENT=$("$BIN" send --to beta --from alpha-sender --subject b8-mail --body "mail cursor observation" --json)
MAIL_ID=$(printf '%s' "$MAIL_SENT" | python3 -c 'import json,sys; print(json.load(sys.stdin)["envelope"]["id"])')
MAIL_BEFORE=$(cd "$BASE/beta" && "$BIN" inbox --room beta)
printf '%s' "$MAIL_BEFORE" > "$BASE/inbox-before-read.json"
( cd "$BASE/beta" && "$BIN" read "$MAIL_ID" --room beta --json >/dev/null )
MAIL_AFTER=$(cd "$BASE/beta" && "$BIN" inbox --room beta)
printf '%s' "$MAIL_AFTER" > "$BASE/inbox-after-read.json"
python3 - "$BASE/inbox-before-read.json" "$BASE/inbox-after-read.json" "$MAIL_ID" <<'PY'
import json
import sys

before_path, after_path, mail_id = sys.argv[1:]
before = json.load(open(before_path, encoding="utf-8"))
after = json.load(open(after_path, encoding="utf-8"))
before_ids = {item["id"] for item in before["unread"]}
after_ids = {item["id"] for item in after["unread"]}
if mail_id not in before_ids or mail_id in after_ids:
    raise SystemExit(f"read did not consume the expected mail id: before={before}, after={after}")
if after["unread_count"] >= before["unread_count"]:
    raise SystemExit(f"inbox unread count did not drop: before={before}, after={after}")
PY
ok "row 3: consuming a mail read drops the room inbox unread count"

# Goal-lock row 4: arm a real long-running watch, consume its backlog, and
# prove the next send still rings. The byte comparison makes the converse
# (watch rings do not advance the cursor) non-vacuous.
WATCH_CHANNEL=b8-bell
mkdir -p "$BASE/gamma"
"$BIN" rooms add gamma "$BASE/gamma" >/dev/null
( cd "$BASE/alpha" && "$BIN" chat "$WATCH_CHANNEL" --join --json >/dev/null )
BELL_GAMMA_JOIN=$(cd "$BASE/gamma" && "$BIN" chat "$WATCH_CHANNEL" --join --json)
BELL_GAMMA_EVENT_ID=$(printf '%s' "$BELL_GAMMA_JOIN" | python3 -c 'import json,sys; print(json.load(sys.stdin)["event_id"])')
( cd "$BASE/gamma" && "$BIN" chat "$WATCH_CHANNEL" --discard-through "$BELL_GAMMA_EVENT_ID" --json >/dev/null )
BELL_FIRST=$(cd "$BASE/alpha" && "$BIN" chat "$WATCH_CHANNEL" --send --anyway --body "bell backlog" --json)
BELL_FIRST_ID=$(printf '%s' "$BELL_FIRST" | python3 -c 'import json,sys; print(json.load(sys.stdin)["message"]["id"])')
WATCH_OUT="$BASE/bell-watch.out"
WATCH_ERR="$BASE/bell-watch.err"
: > "$WATCH_OUT"
(
    cd "$BASE/gamma"
    exec "$BIN" watch --room gamma --interval-ms 100 >"$WATCH_OUT" 2>"$WATCH_ERR"
) &
WATCH_PID=$!
wait_for_watch_id() {
    expected=$1
    attempt=0
    while [ "$attempt" -lt 50 ]; do
        if /usr/bin/grep -Fq -- "$expected" "$WATCH_OUT"; then
            return 0
        fi
        if ! kill -0 "$WATCH_PID" 2>/dev/null; then
            wait "$WATCH_PID" 2>/dev/null || true
            fail "watch exited before ringing $expected: $(cat "$WATCH_ERR")"
        fi
        sleep 0.1
        attempt=$((attempt + 1))
    done
    fail "watch did not ring $expected"
}
wait_for_watch_id "$BELL_FIRST_ID"
BELL_CAUGHT=$(cd "$BASE/gamma" && "$BIN" catchup "$WATCH_CHANNEL" --json)
printf '%s' "$BELL_CAUGHT" > "$BASE/bell-first-catchup.json"
python3 - "$BASE/bell-first-catchup.json" "$WATCH_CHANNEL" "$BELL_FIRST_ID" <<'PY'
import json
import sys

path, channel_name, expected_id = sys.argv[1:]
data = json.load(open(path, encoding="utf-8"))
targets = [target for target in data["targets"] if target["source"] == "channel" and target["channel"] == channel_name]
ids = [message["id"] for message in targets[0]["messages"]] if targets else []
if data["count"] != 1 or ids != [expected_id]:
    raise SystemExit(f"backlog catchup mismatch: {data}")
PY
cp "$BASE/mail/gamma/cursors.json" "$BASE/bell-cursor-before-ring"
BELL_SECOND=$(cd "$BASE/alpha" && "$BIN" chat "$WATCH_CHANNEL" --send --anyway --body "bell after catchup" --json)
BELL_SECOND_ID=$(printf '%s' "$BELL_SECOND" | python3 -c 'import json,sys; print(json.load(sys.stdin)["message"]["id"])')
wait_for_watch_id "$BELL_SECOND_ID"
/usr/bin/cmp -s "$BASE/mail/gamma/cursors.json" "$BASE/bell-cursor-before-ring" || fail "watch ring advanced gamma cursor"
BELL_SECOND_CAUGHT=$(cd "$BASE/gamma" && "$BIN" catchup "$WATCH_CHANNEL" --json)
printf '%s' "$BELL_SECOND_CAUGHT" > "$BASE/bell-second-catchup.json"
python3 - "$BASE/bell-second-catchup.json" "$WATCH_CHANNEL" "$BELL_SECOND_ID" <<'PY'
import json
import sys

path, channel_name, expected_id = sys.argv[1:]
data = json.load(open(path, encoding="utf-8"))
targets = [target for target in data["targets"] if target["source"] == "channel" and target["channel"] == channel_name]
ids = [message["id"] for message in targets[0]["messages"]] if targets else []
if data["count"] != 1 or ids != [expected_id]:
    raise SystemExit(f"watch-ring message was not left unread: {data}")
PY
stop_watch
ok "row 4: watch rings after catchup and leaves ring-only messages unread (byte-compare)"

# Goal-lock row 5: a fenced store refuses the consuming writer before any
# cursor mutation, while peek/snapshot/listing/search surfaces remain usable
# and byte-for-byte read-only.
FENCE_ROOT="$BASE/fenced-mail"
FENCE_CHANNEL=fenced
FENCE_ID=20260901-010101-000001-aaaaaa
mkdir -p "$FENCE_ROOT/fence-room" "$FENCE_ROOT/channels/$FENCE_CHANNEL/messages"
printf '{"fence-room":"%s/fence-room"}\n' "$FENCE_ROOT" > "$FENCE_ROOT/rooms.json"
printf '%s\n' '{"blocked":[]}' > "$FENCE_ROOT/rules.json"
printf '%s\n' '{"state":"fenced","generation":7}' > "$FENCE_ROOT/.post-arx.json"
: > "$FENCE_ROOT/.post-arx.lock"
chmod 600 "$FENCE_ROOT/rooms.json" "$FENCE_ROOT/rules.json" "$FENCE_ROOT/.post-arx.json" "$FENCE_ROOT/.post-arx.lock"
printf '%s\n' '{"name":"fenced","created":"2026-09-01 01:01:01 +0000","created_by":"fence-source"}' > "$FENCE_ROOT/channels/$FENCE_CHANNEL/channel.json"
printf '%s\n' '{"fence-room":"2026-09-01 01:01:01 +0000"}' > "$FENCE_ROOT/channels/$FENCE_CHANNEL/members.json"
printf '%s\n---\n%s\n' '{"id":"20260901-010101-000001-aaaaaa","from":"fence-source","channel":"fenced","subject":"","sent":"2026-09-01 01:01:01 +0000"}' 'fenced unread body' > "$FENCE_ROOT/channels/$FENCE_CHANNEL/messages/$FENCE_ID.msg"
export POST_MAIL_ROOT="$FENCE_ROOT"
unset POST_ARX_GENERATION
fence_manifest() { /usr/bin/find "$1" -type f -exec sha256sum {} + | /usr/bin/sort; }
FENCE_BEFORE=$(fence_manifest "$FENCE_ROOT")
FENCE_CATCHUP_OUT="$BASE/fence-catchup.out"
FENCE_CATCHUP_ERR="$BASE/fence-catchup.err"
rc=0
( cd "$FENCE_ROOT/fence-room" && "$BIN" catchup "$FENCE_CHANNEL" --json >"$FENCE_CATCHUP_OUT" 2>"$FENCE_CATCHUP_ERR" ) || rc=$?
[ "$rc" -eq 78 ] || fail "fenced catchup expected exit 78, got $rc"
[ ! -s "$FENCE_CATCHUP_OUT" ] || fail "fenced catchup emitted stdout before refusing"
[ ! -e "$FENCE_ROOT/fence-room/cursors.json" ] || fail "fenced catchup created cursor state"
( cd "$FENCE_ROOT/fence-room" && "$BIN" chat "$FENCE_CHANNEL" --peek --json >"$BASE/fence-chat.json" )
( cd "$FENCE_ROOT/fence-room" && "$BIN" watch --room fence-room --snapshot >"$BASE/fence-watch.out" )
( cd "$FENCE_ROOT/fence-room" && "$BIN" channels >"$BASE/fence-channels.json" )
( cd "$FENCE_ROOT/fence-room" && "$BIN" inbox --room fence-room >"$BASE/fence-inbox.json" )
( cd "$FENCE_ROOT/fence-room" && "$BIN" search fenced --channel "$FENCE_CHANNEL" --json >"$BASE/fence-search.json" )
/usr/bin/grep -Fq -- "$FENCE_ID" "$BASE/fence-chat.json" || fail "fenced peek missed channel message"
/usr/bin/grep -Fq -- "$FENCE_ID" "$BASE/fence-watch.out" || fail "fenced snapshot missed channel message"
FENCE_AFTER=$(fence_manifest "$FENCE_ROOT")
[ "$FENCE_BEFORE" = "$FENCE_AFTER" ] || fail "fenced read-only surfaces changed store bytes"
[ ! -e "$FENCE_ROOT/fence-room/cursors.json" ] || fail "fenced read-only surfaces created cursor state"
[ ! -e "$FENCE_ROOT/fence-room/.cursors.lock" ] || fail "fenced read-only surfaces created cursor lock"
ok "row 5: fenced catchup refuses, while peek/snapshot/listings/search stay read-only"

# Goal-lock row 6: search returns a member-channel marker but excludes a
# planted marker in a channel beta is not a member of.
export POST_MAIL_ROOT="$BASE/mail"
SEARCH_VISIBLE=b8-search-visible
SEARCH_PRIVATE=b8-search-private
SEARCH_MARKER=b8-search-marker
( cd "$BASE/alpha" && "$BIN" chat "$SEARCH_VISIBLE" --join --json >/dev/null )
( cd "$BASE/beta" && "$BIN" chat "$SEARCH_VISIBLE" --join --json >/dev/null )
( cd "$BASE/beta" && "$BIN" chat "$SEARCH_VISIBLE" --discard --json >/dev/null )
( cd "$BASE/alpha" && "$BIN" chat "$SEARCH_PRIVATE" --join --json >/dev/null )
SEARCH_MEMBER=$(cd "$BASE/alpha" && "$BIN" chat "$SEARCH_VISIBLE" --send --anyway --body "$SEARCH_MARKER member-visible" --json)
SEARCH_MEMBER_ID=$(printf '%s' "$SEARCH_MEMBER" | python3 -c 'import json,sys; print(json.load(sys.stdin)["message"]["id"])')
SEARCH_NON_MEMBER=$(cd "$BASE/alpha" && "$BIN" chat "$SEARCH_PRIVATE" --send --anyway --body "$SEARCH_MARKER non-member" --json)
SEARCH_NON_MEMBER_ID=$(printf '%s' "$SEARCH_NON_MEMBER" | python3 -c 'import json,sys; print(json.load(sys.stdin)["message"]["id"])')
SEARCH_RESULT=$(cd "$BASE/beta" && "$BIN" search "$SEARCH_MARKER" --json)
printf '%s' "$SEARCH_RESULT" > "$BASE/search-result.json"
python3 - "$BASE/search-result.json" "$SEARCH_VISIBLE" "$SEARCH_MEMBER_ID" "$SEARCH_NON_MEMBER_ID" <<'PY'
import json
import sys

path, visible_channel, visible_id, private_id = sys.argv[1:]
data = json.load(open(path, encoding="utf-8"))
ids = [result["id"] for result in data["results"]]
if data["count"] != 1 or len(ids) != 1 or ids[0] != visible_id:
    raise SystemExit(f"search visibility/count mismatch: {data}")
result = data["results"][0]
if result["source"] != "channel" or result["channel"] != visible_channel or "body" not in result["matched"]:
    raise SystemExit(f"member-channel result has wrong shape: {result}")
if private_id in ids:
    raise SystemExit(f"non-member marker leaked into search: {data}")
PY
ok "row 6: search returns the member marker and excludes the planted non-member marker"

# Goal-lock row 7: a cursorless old store is readable as all-unread and stays
# untouched until the first consuming catchup materializes cursors.json.
LEGACY_ROOT="$BASE/legacy-mail"
LEGACY_CHANNEL=legacy-channel
LEGACY_ONE=20260901-010101-000001-aaaaaa
LEGACY_TWO=20260901-010101-000002-bbbbbb
LEGACY_MAIL=20260901-010101-cccccc
mkdir -p "$LEGACY_ROOT/legacy-beta/inbox" "$LEGACY_ROOT/channels/$LEGACY_CHANNEL/messages"
printf '{"legacy-beta":"%s/legacy-beta"}\n' "$LEGACY_ROOT" > "$LEGACY_ROOT/rooms.json"
printf '%s\n' '{"blocked":[]}' > "$LEGACY_ROOT/rules.json"
printf '%s\n' '{"name":"legacy-channel","created":"2026-09-01 01:01:01 +0000","created_by":"legacy-alpha"}' > "$LEGACY_ROOT/channels/$LEGACY_CHANNEL/channel.json"
printf '%s\n' '{"legacy-alpha":"2026-09-01 01:01:01 +0000","legacy-beta":"2026-09-01 01:01:01 +0000"}' > "$LEGACY_ROOT/channels/$LEGACY_CHANNEL/members.json"
printf '%s\n---\n%s\n' '{"id":"20260901-010101-000001-aaaaaa","from":"legacy-alpha","channel":"legacy-channel","subject":"","sent":"2026-09-01 01:01:01 +0000"}' 'legacy channel one' > "$LEGACY_ROOT/channels/$LEGACY_CHANNEL/messages/$LEGACY_ONE.msg"
printf '%s\n---\n%s\n' '{"id":"20260901-010101-000002-bbbbbb","from":"legacy-alpha","channel":"legacy-channel","subject":"","sent":"2026-09-01 01:01:01 +0000"}' 'legacy channel two' > "$LEGACY_ROOT/channels/$LEGACY_CHANNEL/messages/$LEGACY_TWO.msg"
printf '%s\n---\n%s\n' '{"id":"20260901-010101-cccccc","from":"legacy-alpha","to":"legacy-beta","kind":"note","subject":"legacy mail","sent":"2026-09-01 01:01:01 +0000"}' 'legacy mail' > "$LEGACY_ROOT/legacy-beta/inbox/$LEGACY_MAIL.mail"
export POST_MAIL_ROOT="$LEGACY_ROOT"
unset POST_ARX_GENERATION
LEGACY_BEFORE=$(fence_manifest "$LEGACY_ROOT")
( cd "$LEGACY_ROOT/legacy-beta" && "$BIN" channels >"$BASE/legacy-channels.json" )
( cd "$LEGACY_ROOT/legacy-beta" && "$BIN" inbox --room legacy-beta >"$BASE/legacy-inbox.json" )
( cd "$LEGACY_ROOT/legacy-beta" && "$BIN" chat "$LEGACY_CHANNEL" --peek --json >"$BASE/legacy-chat.json" )
( cd "$LEGACY_ROOT/legacy-beta" && "$BIN" watch --room legacy-beta --snapshot >"$BASE/legacy-watch.out" )
python3 - "$BASE/legacy-channels.json" "$BASE/legacy-inbox.json" "$BASE/legacy-chat.json" "$LEGACY_CHANNEL" "$LEGACY_ONE" "$LEGACY_TWO" "$LEGACY_MAIL" <<'PY'
import json
import sys

channels_path, inbox_path, chat_path, channel_name, first_id, second_id, mail_id = sys.argv[1:]
channels = json.load(open(channels_path, encoding="utf-8"))
channel = next((item for item in channels["channels"] if item["name"] == channel_name), None)
if channel is None or channel["messages"] != 2 or channel["unread"] != 2:
    raise SystemExit(f"cursorless channel was not all-unread: {channels}")
inbox = json.load(open(inbox_path, encoding="utf-8"))
if inbox["count"] != 1 or inbox["unread_count"] != 1 or inbox["unread"][0]["id"] != mail_id:
    raise SystemExit(f"cursorless inbox mismatch: {inbox}")
chat = json.load(open(chat_path, encoding="utf-8"))
ids = [message["id"] for message in chat["messages"]]
if chat["count"] != 2 or ids != [first_id, second_id]:
    raise SystemExit(f"cursorless chat mismatch: {chat}")
PY
/usr/bin/grep -Fq -- "$LEGACY_ONE" "$BASE/legacy-watch.out" || fail "legacy snapshot missed first channel message"
/usr/bin/grep -Fq -- "$LEGACY_TWO" "$BASE/legacy-watch.out" || fail "legacy snapshot missed second channel message"
[ ! -e "$LEGACY_ROOT/legacy-beta/cursors.json" ] || fail "legacy read-only surfaces migrated cursor state"
[ ! -e "$LEGACY_ROOT/legacy-beta/.cursors.lock" ] || fail "legacy read-only surfaces created cursor lock"
LEGACY_AFTER=$(fence_manifest "$LEGACY_ROOT")
[ "$LEGACY_BEFORE" = "$LEGACY_AFTER" ] || fail "legacy read-only surfaces changed store bytes"
LEGACY_CAUGHT=$(cd "$LEGACY_ROOT/legacy-beta" && "$BIN" catchup "$LEGACY_CHANNEL" --json)
printf '%s' "$LEGACY_CAUGHT" > "$BASE/legacy-catchup.json"
python3 - "$BASE/legacy-catchup.json" "$LEGACY_CHANNEL" "$LEGACY_ONE" "$LEGACY_TWO" <<'PY'
import json
import sys

path, channel_name, first_id, second_id = sys.argv[1:]
data = json.load(open(path, encoding="utf-8"))
targets = [target for target in data["targets"] if target["source"] == "channel" and target["channel"] == channel_name]
ids = [message["id"] for message in targets[0]["messages"]] if targets else []
if data["count"] != 2 or ids != [first_id, second_id]:
    raise SystemExit(f"legacy catchup did not consume both messages: {data}")
PY
[ -f "$LEGACY_ROOT/legacy-beta/cursors.json" ] || fail "legacy catchup did not materialize cursor state"
export POST_MAIL_ROOT="$BASE/mail"
ok "row 7: cursorless legacy reads are all-unread and first catchup materializes state"

echo "SMOKE PASS (root: $BASE)"
