#!/usr/bin/env bash
set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)
# install.sh reads POST_BIN when --post-bin is absent. A gate that exports it
# for the Python suites must not redirect this script's fake post.
unset POST_BIN
# macOS mktemp hands back a /var symlink path; the sweeper requires canonical roots.
TMP=$(CDPATH='' cd -- "$(mktemp -d)" && pwd -P)
trap 'rm -rf "$TMP"' EXIT

HOME_DIR="$TMP/home"
MAIL_ROOT="$TMP/mailroot"
XDG_CONFIG="$TMP/config"
REMOTE="$TMP/relay.git"
SEED="$TMP/seed"
CLONE="$TMP/clone"
BIN="$TMP/bin"
mkdir -p "$HOME_DIR" "$MAIL_ROOT" "$BIN"

cat >"$BIN/post" <<'SH'
#!/bin/sh
if [ "${1:-}" = --version ]; then
  printf '%s\n' 'post 0.9.0'
  exit 0
fi
printf '%s\n' 'fake post: unsupported command' >&2
exit 1
SH
cat >"$BIN/post-old" <<'SH'
#!/bin/sh
printf '%s\n' 'post 0.6.0'
SH
cat >"$BIN/systemctl" <<'SH'
#!/bin/sh
printf '%s\n' "$*" >>"$POST_BRIDGE_SYSTEMCTL_LOG"
case "$*" in
  *'enable --now post-bridge.timer'*)
    if [ -n "${FAKE_SYSTEMCTL_FAIL_ENABLE:-}" ]; then exit 1; fi
    ;;
  *list-timers*) printf '%s\n' 'NEXT LEFT LAST PASSED UNIT ACTIVATES' ;;
  *'stop post-bridge.timer'*)
    # Which package was installed when the timer stopped (review 4, finding 2).
    if grep -Fq OLD-PACKAGE-MARKER "$HOME/.local/lib/post-bridge/sweep.py" 2>/dev/null; then
      printf '%s\n' 'package-at-stop: old' >>"$POST_BRIDGE_SYSTEMCTL_LOG"
    else
      printf '%s\n' 'package-at-stop: new' >>"$POST_BRIDGE_SYSTEMCTL_LOG"
    fi
    ;;
esac
SH
cat >"$BIN/fj" <<'SH'
#!/bin/sh
case ${2:-} in
  /users/*) printf '%s\n' "$FAKE_FJ_USER_BODY" ;;
  /repos/estate/post-relay/branch_protections)
    printf '%s\n' '[{"rule_name":"registry","enable_push":true,"enable_push_whitelist":true,"push_whitelist_usernames":["trey","mac"],"push_whitelist_teams":[],"push_whitelist_deploy_keys":false},{"rule_name":"machines/*","enable_push":true,"enable_push_whitelist":true,"push_whitelist_usernames":[],"push_whitelist_teams":[],"push_whitelist_deploy_keys":false}]'
    ;;
  /repos/estate/post-relay/branches) printf '%s\n' '[{"name":"registry"}]' ;;
  *) printf '%s\n' '{"message":"unexpected fake fj request"}' ;;
esac
SH
chmod 0755 "$BIN/post" "$BIN/post-old" "$BIN/systemctl" "$BIN/fj"

git init -q -b main "$SEED"
git -C "$SEED" config user.name fixture
git -C "$SEED" config user.email fixture@example.invalid
printf '%s\n' seed >"$SEED/README"
git -C "$SEED" add -- README
git -C "$SEED" commit -qm seed
git init --bare -q "$REMOTE"
git -C "$SEED" remote add origin "$REMOTE"
git -C "$SEED" push -q origin main
git --git-dir="$REMOTE" symbolic-ref HEAD refs/heads/main

KEY="$HOME_DIR/relay-key"
printf '%s\n' test-key >"$KEY"
chmod 0600 "$KEY"
CONFIG="$TMP/config.json"
printf '{"host":"cell-a","relay_url":"%s","peers":{}}\n' "$REMOTE" >"$CONFIG"

COMMON_ENV=(
  "HOME=$HOME_DIR"
  "XDG_CONFIG_HOME=$XDG_CONFIG"
  "POST_MAIL_ROOT=$MAIL_ROOT"
  "PATH=$BIN:$PATH"
  "POST_BRIDGE_SYSTEMCTL_LOG=$TMP/systemctl.log"
)

SCOPE_ERROR='{"message":"token does not have at least one of required scope(s): [read:user]"}'
env "${COMMON_ENV[@]}" FAKE_FJ_USER_BODY="$SCOPE_ERROR" \
  "$ROOT/bridge/enroll.sh" --dry-run --host x \
  >"$TMP/enroll-scope.out" 2>"$TMP/enroll-scope.err"
grep -Fq 'user check unavailable (token lacks read:user); relying on the branch-protection API to validate the username' "$TMP/enroll-scope.err"
grep -Fq '# user check returned:' "$TMP/enroll-scope.out"
grep -Fq '+ fj POST /repos/estate/post-relay/branch_protections body <<<' "$TMP/enroll-scope.out"
grep -Fq '+ add x to hosts.json (sorted, unique)' "$TMP/enroll-scope.out"

if env "${COMMON_ENV[@]}" FAKE_FJ_USER_BODY='{"message":"user not found"}' \
  "$ROOT/bridge/enroll.sh" --host x \
  >"$TMP/enroll-404.out" 2>"$TMP/enroll-404.err"; then
  printf '%s\n' 'missing Forgejo user was accepted' >&2
  exit 1
fi
grep -Fq 'Manual step: create Forgejo user x and install its relay SSH public key, then rerun enroll.sh.' "$TMP/enroll-404.err"
INSTALL_ARGS=(
  --repo-url "$REMOTE"
  --clone-dir "$CLONE"
  --host cell-a
  --ssh-key "$KEY"
  --config "$CONFIG"
  --interval 23
)

if env "${COMMON_ENV[@]}" "$ROOT/bridge/install.sh" "${INSTALL_ARGS[@]}" --post-bin "$BIN/post-old" >"$TMP/old.out" 2>"$TMP/old.err"; then
  printf '%s\n' 'old Post version was accepted' >&2
  exit 1
fi
grep -Fq "post version must be post 0.9.0, optionally followed by ' (build ...)'; got post 0.6.0" "$TMP/old.err"
[ ! -e "$CLONE" ] || { printf '%s\n' 'version refusal cloned the relay' >&2; exit 1; }
# A build-annotated version is the same semver and is accepted; another semver
# or a malformed annotation is not. Each variant gets its own fake post.
for variant in 'post 0.9.0 (build abc1234, 2026-09-28)' 'post 0.9.0 (build abc1234-dirty)'; do
  printf '#!/bin/sh\nprintf "%%s\\n" "%s"\n' "$variant" >"$BIN/post-annotated"
  chmod 0755 "$BIN/post-annotated"
  env "${COMMON_ENV[@]}" "$ROOT/bridge/install.sh" --repo-url "$REMOTE" --clone-dir "$TMP/clone-annotated" --host cell-a \
    --ssh-key "$KEY" --config "$CONFIG" --post-bin "$BIN/post-annotated" >"$TMP/annotated.out" 2>"$TMP/annotated.err" \
    || { printf 'annotated version %s was refused: %s\n' "$variant" "$(cat "$TMP/annotated.err")" >&2; exit 1; }
  env "${COMMON_ENV[@]}" "$ROOT/bridge/install.sh" --uninstall --clone-dir "$TMP/clone-annotated" >/dev/null
  rm -rf "$TMP/clone-annotated" "$MAIL_ROOT/bridge/config.json"
done
for variant in 'post 0.9.1' 'post 0.10.0 (build abc1234)' 'post 0.9.0 build abc1234' 'post 0.9.0 (build abc) trailing' 'post 0.9.0-rc1' 'post 0.9.0 (build (nested))'; do
  printf '#!/bin/sh\nprintf "%%s\\n" "%s"\n' "$variant" >"$BIN/post-annotated"
  chmod 0755 "$BIN/post-annotated"
  if env "${COMMON_ENV[@]}" "$ROOT/bridge/install.sh" --repo-url "$REMOTE" --clone-dir "$TMP/clone-annotated" --host cell-a \
    --ssh-key "$KEY" --config "$CONFIG" --post-bin "$BIN/post-annotated" >/dev/null 2>&1; then
    printf 'version %s was accepted\n' "$variant" >&2
    exit 1
  fi
  [ ! -e "$TMP/clone-annotated" ] || { printf 'version refusal cloned the relay: %s\n' "$variant" >&2; exit 1; }
done
rm -f "$BIN/post-annotated"

env "${COMMON_ENV[@]}" "$ROOT/bridge/install.sh" "${INSTALL_ARGS[@]}" 2> >(tee "$TMP/first.err" >&2)
# A config with no channels key syncs every channel; the installer says so
# and points at the template that carries the estate deny list.
grep -Fq 'has no "channels" key, so this host will publish and import every channel' "$TMP/first.err" || { printf '%s\n' 'no-channels config gave no warning' >&2; exit 1; }
grep -Fq 'bridge/config.template.json' "$TMP/first.err" || { printf '%s\n' 'no-channels warning does not name the template' >&2; exit 1; }

DATA="$HOME_DIR/.local/lib/post-bridge"
LAUNCHER="$HOME_DIR/.local/bin/post-bridge-sweep"
SERVICE="$XDG_CONFIG/systemd/user/post-bridge.service"
TIMER="$XDG_CONFIG/systemd/user/post-bridge.timer"
[ -f "$DATA/sweep.py" ] || { printf '%s\n' 'installed sweep.py is missing' >&2; exit 1; }
[ -d "$DATA/bridgelib" ] || { printf '%s\n' 'installed bridgelib package is missing' >&2; exit 1; }
cmp -s "$ROOT/bridge/sweep.py" "$DATA/sweep.py"
for module in "$ROOT"/bridge/bridgelib/*.py; do
  cmp -s "$module" "$DATA/bridgelib/$(basename -- "$module")"
done
# The install records the post repo commit it came from, next to the package.
[ -f "$DATA/BUILD" ] || { printf '%s\n' 'installed BUILD file is missing' >&2; exit 1; }
grep -Fqx "commit=$(git -C "$ROOT" rev-parse HEAD)" "$DATA/BUILD" || { printf 'BUILD does not name the source commit: %s\n' "$(cat "$DATA/BUILD")" >&2; exit 1; }
[ -x "$LAUNCHER" ] || { printf '%s\n' 'launcher is missing or not executable' >&2; exit 1; }
grep -Fqx "exec python3 \"$DATA/sweep.py\" \"\$@\"" "$LAUNCHER"
grep -Fqx "Environment=\"POST_BIN=$BIN/post\"" "$SERVICE"
grep -Fqx 'Environment="BRIDGE_TICK_DEADLINE_SECONDS=240"' "$SERVICE"
grep -Fqx 'Environment="BRIDGE_INTERVAL_SECONDS=23"' "$SERVICE"
grep -Fqx 'TimeoutStartSec=5min' "$SERVICE"
grep -Fqx 'OnUnitActiveSec=23' "$TIMER"
grep -Fqx 'AccuracySec=1s' "$TIMER"
grep -Fq -- 'enable --now post-bridge.timer' "$TMP/systemctl.log"
if grep -Fq -- 'start post-bridge.service' "$TMP/systemctl.log"; then
  printf '%s\n' 'installer started the service directly' >&2
  exit 1
fi

SENTINEL="$MAIL_ROOT/bridge/held-guard-sentinel"
[ ! -e "$SENTINEL" ] || { printf '%s\n' 'fresh install wrote a held-guard sentinel' >&2; exit 1; }

# A store stamped by an older bridge (an index, no sentinel) gets its
# sentinel at install, before any tick (review 3, finding 7).
printf '\n%s\n\n%s\n' 20260923-000000-aaaaaa 20260923-000001-bbbbbb >"$MAIL_ROOT/bridge/local-held-index.txt"
env "${COMMON_ENV[@]}" "$ROOT/bridge/install.sh" \
  --repo-url "$REMOTE" --clone-dir "$CLONE" --host cell-a \
  --ssh-key "$KEY" --config "$CONFIG"
grep -Fqx 'OnUnitActiveSec=15' "$TIMER"
[ "$(cat "$SENTINEL")" = '{"indexed":2,"manifests":0,"version":1}' ] || { printf 'sentinel after install over an index: %s\n' "$(cat "$SENTINEL" 2>&1)" >&2; exit 1; }

# Reinstallation preserves unpushed work and an identical config.
git -C "$CLONE" -c user.name=fixture -c user.email=fixture@example.invalid \
  commit -q --allow-empty -m 'local unpushed outbox commit'
LOCAL_HEAD=$(git -C "$CLONE" rev-parse HEAD)
env "${COMMON_ENV[@]}" "$ROOT/bridge/install.sh" "${INSTALL_ARGS[@]}"
[ "$(git -C "$CLONE" rev-parse HEAD)" = "$LOCAL_HEAD" ] || { printf '%s\n' 'reinstall reset a local commit' >&2; exit 1; }

# A differing config is refused and left untouched.
CHANGED_CONFIG="$TMP/config-changed.json"
printf '{"host":"cell-a","relay_url":"%s","peers":{"cell-b":["hq"]}}\n' "$REMOTE" >"$CHANGED_CONFIG"
if env "${COMMON_ENV[@]}" "$ROOT/bridge/install.sh" \
  --repo-url "$REMOTE" --clone-dir "$CLONE" --host cell-a \
  --ssh-key "$KEY" --config "$CHANGED_CONFIG" >"$TMP/different.out" 2>"$TMP/different.err"; then
  printf '%s\n' 'differing config was accepted' >&2
  exit 1
fi
cmp -s "$CONFIG" "$MAIL_ROOT/bridge/config.json"

# --host remains required and refusal happens before cloning.
if env "${COMMON_ENV[@]}" "$ROOT/bridge/install.sh" \
  --repo-url "$REMOTE" --clone-dir "$TMP/no-host-clone" \
  --ssh-key "$KEY" --config "$CONFIG" >"$TMP/nohost.out" 2>"$TMP/nohost.err"; then
  printf '%s\n' 'missing --host was accepted' >&2
  exit 1
fi
[ ! -e "$TMP/no-host-clone" ] || { printf '%s\n' 'missing --host cloned the relay' >&2; exit 1; }

# A failed real launcher config check never enables the timer.
mkdir -p "$TMP/failbin"
cat >"$TMP/failbin/python3" <<'SH'
#!/bin/sh
exit 1
SH
chmod 0755 "$TMP/failbin/python3"
: >"$TMP/systemctl.log"
if env "${COMMON_ENV[@]}" PATH="$TMP/failbin:$BIN:$PATH" \
  "$ROOT/bridge/install.sh" "${INSTALL_ARGS[@]}" >"$TMP/check.out" 2>"$TMP/check.err"; then
  printf '%s\n' 'failed --check-config was ignored' >&2
  exit 1
fi
if grep -Fq -- 'enable --now post-bridge.timer' "$TMP/systemctl.log"; then
  printf '%s\n' 'timer was enabled after failed --check-config' >&2
  exit 1
fi
grep -Fqx -- '--user stop post-bridge.timer' "$TMP/systemctl.log" || { printf '%s\n' 'timer was not stopped before a failed --check-config' >&2; exit 1; }
# Review 5, finding 1: the failure says the timer is stopped, not disabled.
grep -Fq 'sweep.py --check-config failed; the timer was not started' "$TMP/check.err" || { printf '%s\n' 'failed --check-config message missing' >&2; exit 1; }
grep -Fq 'post-bridge.timer is stopped but still enabled' "$TMP/check.err" || { printf '%s\n' 'failed install did not report the stopped timer' >&2; exit 1; }
grep -Fq 'systemctl --user start post-bridge.timer' "$TMP/check.err" || { printf '%s\n' 'failed install gave no recovery command' >&2; exit 1; }
if grep -Fq -- 'disable' "$TMP/systemctl.log"; then
  printf '%s\n' 'failed install disabled the timer' >&2
  exit 1
fi

# Review 6, item 3: when enable --now itself fails, the timer may not be
# enabled, so the notice must not say it is.
if env "${COMMON_ENV[@]}" FAKE_SYSTEMCTL_FAIL_ENABLE=1 \
  "$ROOT/bridge/install.sh" "${INSTALL_ARGS[@]}" >"$TMP/enable.out" 2>"$TMP/enable.err"; then
  printf '%s\n' 'failed enable --now was ignored' >&2
  exit 1
fi
grep -Fq 'post-bridge.timer is not running and may not be enabled' "$TMP/enable.err" || { printf '%s\n' 'failed enable --now did not say the timer may not be enabled' >&2; exit 1; }
if grep -Fq 'still enabled' "$TMP/enable.err"; then
  printf '%s\n' 'failed enable --now claimed the timer is still enabled' >&2
  exit 1
fi

# An existing remote machine branch is tracked rather than replaced.
git -C "$SEED" push -q origin main:refs/heads/machines/cell-b
REMOTE_B=$(git --git-dir="$REMOTE" rev-parse refs/heads/machines/cell-b)
CLONE_B="$TMP/clone-b"
MAIL_ROOT_B="$TMP/mailroot-b"
CONFIG_B="$TMP/config-b.json"
mkdir -p "$MAIL_ROOT_B"
printf '{"host":"cell-b","relay_url":"%s","peers":{}}\n' "$REMOTE" >"$CONFIG_B"
env "${COMMON_ENV[@]}" POST_MAIL_ROOT="$MAIL_ROOT_B" \
  "$ROOT/bridge/install.sh" --repo-url "$REMOTE" --clone-dir "$CLONE_B" \
  --host cell-b --ssh-key "$KEY" --config "$CONFIG_B"
[ "$(git -C "$CLONE_B" symbolic-ref --quiet --short HEAD)" = machines/cell-b ] || { printf '%s\n' 'existing machine branch was not checked out' >&2; exit 1; }
[ "$(git -C "$CLONE_B" rev-parse HEAD)" = "$REMOTE_B" ] || { printf '%s\n' 'existing machine branch was not tracked exactly' >&2; exit 1; }

if env -i HOME="$HOME_DIR" PATH="/usr/bin:/bin" "$LAUNCHER" --check-config >"$TMP/launcher.out" 2>"$TMP/launcher.err"; then
  printf '%s\n' 'launcher without required environment unexpectedly succeeded' >&2
  exit 1
else
  rc=$?
fi
[ "$rc" -eq 2 ] || { printf 'launcher returned %s, expected 2\n' "$rc" >&2; exit 1; }
grep -Fq 'missing required environment' "$TMP/launcher.out"

# The launcher bounds launchd's StandardOutPath at the log.jsonl cap. launchd
# holds the file open for the tick it spawns (simulated with >>), so that tick
# writes into the rotated copy and the next spawn opens a fresh launchd.log.
mkdir -p "$TMP/stubbin"
cat >"$TMP/stubbin/python3" <<'SH'
#!/bin/sh
printf 'stub %s\n' "$*"
SH
chmod 0755 "$TMP/stubbin/python3"
STDOUT_LOG="$MAIL_ROOT/bridge/launchd.log"
CAP=10485760
run_launcher() {
  env -i HOME="$HOME_DIR" PATH="$TMP/stubbin:/usr/bin:/bin" "$@" "$LAUNCHER" --tick >>"$STDOUT_LOG"
}
{ head -c "$((CAP - 2))" /dev/zero; printf '\n'; } >"$STDOUT_LOG"
run_launcher POST_MAIL_ROOT="$MAIL_ROOT"
[ ! -e "$STDOUT_LOG.1" ] || { printf '%s\n' 'launchd.log rotated below the cap' >&2; exit 1; }
tail -n 1 "$STDOUT_LOG" | grep -Fqx 'stub '"$DATA"'/sweep.py --tick'
[ "$(wc -c <"$STDOUT_LOG")" -ge "$CAP" ] || { printf '%s\n' 'launchd.log did not reach the cap' >&2; exit 1; }
printf '%s\n' previous >"$STDOUT_LOG.1"
SIZE_AT_CAP=$(wc -c <"$STDOUT_LOG")
run_launcher POST_MAIL_ROOT="$MAIL_ROOT"
[ ! -e "$STDOUT_LOG" ] || { printf '%s\n' 'launchd.log was not rotated at the cap' >&2; exit 1; }
[ "$(head -c 1 "$STDOUT_LOG.1" | od -An -tx1 | tr -d ' ')" = 00 ] || { printf '%s\n' 'launchd.log.1 was not replaced by the rotated log' >&2; exit 1; }
[ "$(wc -c <"$STDOUT_LOG.1")" -gt "$SIZE_AT_CAP" ] || { printf '%s\n' 'the rotating tick did not write to the rotated file' >&2; exit 1; }
# Without POST_MAIL_ROOT, or with a symlinked log, nothing moves.
head -c "$CAP" /dev/zero >"$STDOUT_LOG"
run_launcher
[ -f "$STDOUT_LOG" ] || { printf '%s\n' 'launcher rotated without POST_MAIL_ROOT' >&2; exit 1; }
mv -f "$STDOUT_LOG" "$TMP/big.log"
ln -s "$TMP/big.log" "$STDOUT_LOG"
run_launcher POST_MAIL_ROOT="$MAIL_ROOT"
[ -L "$STDOUT_LOG" ] || { printf '%s\n' 'launcher rotated a symlinked launchd.log' >&2; exit 1; }
rm -f "$STDOUT_LOG" "$STDOUT_LOG.1" "$TMP/big.log"

printf '%s\n' stale >"$DATA/bridgelib/renamed_module.py"
env "${COMMON_ENV[@]}" "$ROOT/bridge/install.sh" "${INSTALL_ARGS[@]}"
[ ! -e "$DATA/bridgelib/renamed_module.py" ] || { printf '%s\n' 'stale package module survived reinstall' >&2; exit 1; }

# Under systemd the timer stops before the package swap and restarts after
# --init-held-sentinel, so no tick of the new package precedes the sentinel.
printf '%s\n' '# OLD-PACKAGE-MARKER' >>"$DATA/sweep.py"
: >"$TMP/systemctl.log"
env "${COMMON_ENV[@]}" "$ROOT/bridge/install.sh" "${INSTALL_ARGS[@]}" 2>"$TMP/restart.err"
if grep -Fq 'post-bridge.timer is stopped' "$TMP/restart.err"; then
  printf '%s\n' 'a successful install reported the timer stopped' >&2
  exit 1
fi
! grep -Fq OLD-PACKAGE-MARKER "$DATA/sweep.py" || { printf '%s\n' 'reinstall did not swap the package' >&2; exit 1; }
STOP_ORDER=$(grep -E -e '^--user stop post-bridge.timer$' -e '^package-at-stop: ' -e '^--user enable --now post-bridge.timer$' "$TMP/systemctl.log" | tr '\n' '|')
[ "$STOP_ORDER" = '--user stop post-bridge.timer|package-at-stop: old|--user enable --now post-bridge.timer|' ] || { printf 'timer stop order: %s\n' "$STOP_ORDER" >&2; exit 1; }

env "${COMMON_ENV[@]}" "$ROOT/bridge/install.sh" --uninstall --clone-dir "$CLONE"
[ ! -e "$DATA" ] || { printf '%s\n' 'package remains after uninstall' >&2; exit 1; }
[ ! -e "$LAUNCHER" ] || { printf '%s\n' 'launcher remains after uninstall' >&2; exit 1; }
[ ! -e "$SERVICE" ] || { printf '%s\n' 'service remains after uninstall' >&2; exit 1; }
[ ! -e "$TIMER" ] || { printf '%s\n' 'timer remains after uninstall' >&2; exit 1; }
[ -d "$CLONE/.git" ] || { printf '%s\n' 'uninstall removed the clone' >&2; exit 1; }
cmp -s "$CONFIG" "$MAIL_ROOT/bridge/config.json" || { printf '%s\n' 'uninstall changed the mail root' >&2; exit 1; }

# A config that names channels installs without the warning, and the
# template itself carries the estate deny list.
python3 - "$ROOT/bridge/config.template.json" <<'PY2'
import json, sys
cfg = json.load(open(sys.argv[1]))
assert cfg["channels"]["mode"] == "all", cfg
assert {"devbox-build", "litigation-work", "wade-overnight", "cos", "cos-urgent"} <= set(cfg["channels"]["deny"]), cfg
PY2
CH_HOME="$TMP/home-ch"
CH_MAIL="$TMP/mailroot-ch"
mkdir -p "$CH_HOME" "$CH_MAIL"
CH_CONFIG="$TMP/config-channels.json"
printf '{"host":"cell-a","relay_url":"%s","channels":{"mode":"all","deny":["devbox-build"]}}\n' "$REMOTE" >"$CH_CONFIG"
env HOME="$CH_HOME" XDG_CONFIG_HOME="$TMP/config-ch" POST_MAIL_ROOT="$CH_MAIL" PATH="$BIN:$PATH" \
  POST_BRIDGE_SYSTEMCTL_LOG="$TMP/systemctl-ch.log" \
  "$ROOT/bridge/install.sh" --repo-url "$REMOTE" --clone-dir "$TMP/clone-ch" --host cell-a \
  --ssh-key "$KEY" --config "$CH_CONFIG" >"$TMP/ch.out" 2>"$TMP/ch.err"
if grep -Fq 'has no "channels" key' "$TMP/ch.err"; then
  printf '%s\n' 'config with channels still warned' >&2
  exit 1
fi

# BUILD is deterministic and honest about what it came from. A source repo
# with a known commit, a dirty edit, and a copied tree (no Git) each install
# under their own HOME.
build_install() {
  local name=$1 source=$2
  local home="$TMP/home-build-$name"
  mkdir -p "$home" "$TMP/mail-build-$name"
  env HOME="$home" XDG_CONFIG_HOME="$TMP/config-build-$name" POST_MAIL_ROOT="$TMP/mail-build-$name" \
    PATH="$BIN:$PATH" POST_BRIDGE_SYSTEMCTL_LOG="$TMP/systemctl-build-$name.log" \
    "$source/install.sh" --repo-url "$REMOTE" --clone-dir "$TMP/clone-build-$name" --host cell-a \
    --ssh-key "$KEY" --config "$CONFIG" >"$TMP/build-$name.out" 2>"$TMP/build-$name.err"
  BUILD_FILE="$home/.local/lib/post-bridge/BUILD"
}
SRC_REPO="$TMP/src-repo"
mkdir -p "$SRC_REPO"
cp -R "$ROOT/bridge" "$SRC_REPO/bridge"
git init -q -b main "$SRC_REPO"
git -C "$SRC_REPO" add -- bridge
git -C "$SRC_REPO" -c user.name=fixture -c user.email=fixture@example.invalid commit -qm 'bridge source'
SRC_COMMIT=$(git -C "$SRC_REPO" rev-parse HEAD)
build_install clean "$SRC_REPO/bridge"
[ "$(cat "$BUILD_FILE")" = "$(printf 'commit=%s\ndirty=no' "$SRC_COMMIT")" ] || { printf 'clean BUILD: %s\n' "$(cat "$BUILD_FILE")" >&2; exit 1; }
# Same source, same bytes: the record carries no timestamp.
FIRST_BUILD=$(cat "$BUILD_FILE")
sleep 1
build_install clean "$SRC_REPO/bridge"
[ "$(cat "$BUILD_FILE")" = "$FIRST_BUILD" ] || { printf '%s\n' 'BUILD changed with no source change' >&2; exit 1; }
printf '\n# local edit\n' >>"$SRC_REPO/bridge/sweep.py"
build_install dirty "$SRC_REPO/bridge"
[ "$(cat "$BUILD_FILE")" = "$(printf 'commit=%s\ndirty=yes' "$SRC_COMMIT")" ] || { printf 'dirty BUILD: %s\n' "$(cat "$BUILD_FILE")" >&2; exit 1; }
# A copy of the tree that sits inside some other Git checkout (untracked
# there) is not "from" that checkout's commit.
COPIED="$SRC_REPO/vendored"
mkdir -p "$COPIED"
cp -R "$ROOT/bridge" "$COPIED/bridge"
build_install copied "$COPIED/bridge"
[ "$(cat "$BUILD_FILE")" = "$(printf 'commit=unknown\ndirty=unknown')" ] || { printf 'copied-tree BUILD: %s\n' "$(cat "$BUILD_FILE")" >&2; exit 1; }

printf '%s\n' 'test_install: PASS'
