#!/bin/sh
# Adapter envelope canary (identity M2): sends real mail + channel messages
# under POST_FROM/POST_SENDER_ADDRESS with the locally built release binary,
# then drives all four harness adapters (Claude Code, Codex, Cursor, Grok)
# plus watch-notice against the resulting store. PASS = the watch snapshot
# carries both identity fields, every consumer emits its normal notice, and
# none takes its malformed/diagnostic path. Run from anywhere:
#     sh skills/post/hooks/envelope-canary.sh
# Requires: cargo build --release beforehand; node on PATH. Everything runs
# in a throwaway mktemp HOME/mail root — the real mailbox is never touched.
set -eu
HOOKS_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO=$(CDPATH= cd -- "$HOOKS_DIR/../../.." && pwd)
BIN="$REPO/target/release/post"
HOOKS="$HOOKS_DIR"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
SANDBOX="$WORK/home"
MAIL="$WORK/mail"
mkdir -p "$SANDBOX/watcher-room" "$SANDBOX/sender-room"
run_post() { HOME="$SANDBOX" POST_MAIL_ROOT="$MAIL" "$BIN" "$@"; }

run_post rooms add watcher "~/watcher-room" >/dev/null
run_post rooms add sender "~/sender-room" >/dev/null

# Watcher joins a channel; sender (via pin+address, cwd deliberately OUTSIDE
# its tree) mails the watcher and posts to the channel.
( cd "$SANDBOX/watcher-room" && run_post chat m2canary --join >/dev/null 2>&1 )
( cd "$WORK" && env POST_FROM=sender POST_SENDER_ADDRESS=claude-code.m2.0123abcd \
    HOME="$SANDBOX" POST_MAIL_ROOT="$MAIL" "$BIN" chat m2canary --join >/dev/null 2>&1 )
( cd "$WORK" && env POST_FROM=sender POST_SENDER_ADDRESS=claude-code.m2.0123abcd \
    HOME="$SANDBOX" POST_MAIL_ROOT="$MAIL" "$BIN" send --to watcher --body "m2 canary mail" >/dev/null 2>&1 )
( cd "$WORK" && env POST_FROM=sender POST_SENDER_ADDRESS=claude-code.m2.0123abcd \
    HOME="$SANDBOX" POST_MAIL_ROOT="$MAIL" "$BIN" chat m2canary --send --body "m2 canary message @watcher" >/dev/null 2>&1 )

# Receipt 0: the raw snapshot really carries the new fields.
SNAP=$(cd "$SANDBOX/watcher-room" && run_post watch --snapshot)
echo "$SNAP" | grep -q '"sender_address":"claude-code.m2.0123abcd"' || { echo "FAIL: snapshot lacks address"; exit 1; }
echo "$SNAP" | grep -q '"sender_provenance":"declared-env"' || { echo "FAIL: snapshot lacks provenance"; exit 1; }
echo "receipt 0: watch snapshot carries both identity fields"

fail=0
# Claude adapter
OUT=$(printf '{"hook_event_name":"SessionStart","session_id":"m2-canary","cwd":"%s"}' "$SANDBOX/watcher-room" \
  | env POST_CLAUDE_HOOK_BIN="$BIN" POST_CLAUDE_HOOK_STATE_DIR="$WORK/state-claude" \
        HOME="$SANDBOX" POST_MAIL_ROOT="$MAIL" node "$HOOKS/claude-mail.mjs")
echo "$OUT" | grep -q "Unread agent mail" && echo "$OUT" | grep -q "m2canary" \
  && echo "receipt 1: claude adapter notices mail+channel" \
  || { echo "FAIL claude: $OUT"; fail=1; }
echo "$OUT" | grep -qi "manual check\|could not" && { echo "FAIL claude diagnostic path: $OUT"; fail=1; } || true

# Codex adapter
OUT=$(printf '{"hook_event_name":"SessionStart","session_id":"m2-canary","cwd":"%s"}' "$SANDBOX/watcher-room" \
  | env POST_CODEX_HOOK_BIN="$BIN" POST_CODEX_HOOK_STATE_DIR="$WORK/state-codex" \
        HOME="$SANDBOX" POST_MAIL_ROOT="$MAIL" node "$HOOKS/codex-mail.mjs")
echo "$OUT" | grep -q "m2canary" && echo "receipt 2: codex adapter notices channel" \
  || { echo "FAIL codex: $OUT"; fail=1; }

# Cursor adapter (camelCase event names)
OUT=$(printf '{"hook_event_name":"sessionStart","session_id":"m2-canary","cwd":"%s"}' "$SANDBOX/watcher-room" \
  | env POST_CURSOR_HOOK_BIN="$BIN" POST_CURSOR_HOOK_STATE_DIR="$WORK/state-cursor" \
        HOME="$SANDBOX" POST_MAIL_ROOT="$MAIL" node "$HOOKS/cursor-mail.mjs")
echo "$OUT" | grep -q "m2canary" && echo "receipt 3: cursor adapter notices channel" \
  || { echo "FAIL cursor: $OUT"; fail=1; }

# Grok adapter
OUT=$(printf '{"hookEventName":"UserPromptSubmit","sessionId":"m2-canary","cwd":"%s"}' "$SANDBOX/watcher-room" \
  | env POST_GROK_HOOK_BIN="$BIN" POST_GROK_HOOK_STATE_DIR="$WORK/state-grok" \
        HOME="$SANDBOX" POST_MAIL_ROOT="$MAIL" node "$HOOKS/grok-mail.mjs")
echo "$OUT" | grep -q "m2canary" && echo "receipt 4: grok adapter notices channel" \
  || { echo "FAIL grok: $OUT"; fail=1; }

# watch-notice (Monitor lane): snapshot mode
OUT=$(cd "$SANDBOX/watcher-room" && env POST_WATCH_NOTICE_BIN="$BIN" HOME="$SANDBOX" POST_MAIL_ROOT="$MAIL" \
  node "$HOOKS/watch-notice.mjs" --snapshot 2>&1) || true
echo "$OUT" | grep -q "m2canary" && echo "receipt 5: watch-notice renders channel line" \
  || { echo "FAIL watch-notice: $OUT"; fail=1; }

[ "$fail" -eq 0 ] && echo "M2 CANARY PASS: all five consumers accept new envelopes" || { echo "M2 CANARY FAIL"; exit 1; }
