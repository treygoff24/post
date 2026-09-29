#!/usr/bin/env bash
set -euo pipefail

POST_VERSION_RANGE='post 0.9.0 up to, but not including, post 0.10.0'

# `post --version` prints a version from 0.9.0 up to, but not including,
# 0.10.0, optionally followed by build metadata: `post 0.9.4 (build abc1234,
# ...)`. A patch number is a plain integer (no leading zeros, no pre-release
# tag). Same rule as sweep.py's post_version_accepted, which --check-config
# applies again below.
post_version_accepted() {
  local text=$1 version_re='^post 0\.9\.(0|[1-9][0-9]*)( \(build [^()]*\))?$'
  case $text in
    *$'\n'*) return 1 ;;
  esac
  [[ $text =~ $version_re ]]
}

usage() {
  cat <<'EOF'
Usage: install.sh --repo-url <ssh-url> --host <name> --ssh-key <path> --config <path> [options]
       install.sh --uninstall [--clone-dir <path>]

Options:
  --repo-url <url>        Relay repository URL (required for installation)
  --clone-dir <path>      Relay clone directory (default: ~/post-relay)
  --host <name>           Relay host name; the branch is machines/<name> (required)
  --ssh-key <path>        relay ssh private key for this node (required)
  --config <path>         bridge topology config to install as
                          $POST_MAIL_ROOT/bridge/config.json (required;
                          refuses to overwrite a differing one)
  --post-bin <path>       Post executable (default: post resolved from PATH)
  --interval <seconds>    systemd interval (default: 15)
  --uninstall             Remove the user units, launcher, and installed package
  -h, --help              Show this help
EOF
}

die() {
  printf 'post-bridge install: %s\n' "$*" >&2
  exit 1
}

say() {
  printf 'post-bridge install: %s\n' "$*"
}

warn() {
  printf 'post-bridge install: warning: %s\n' "$*" >&2
}

# Set once this install has stopped post-bridge.timer, cleared once enable
# --now restarts it. Any exit in between (a die, or set -e on mv, mktemp,
# render_template or systemctl) says so on stderr (review 5, finding 1).
TIMER_STOPPED_BY_INSTALL=false
# Set when enable --now itself failed: the timer may then not be enabled at
# all, so the notice must not claim it is (review 6, item 3).
TIMER_ENABLE_FAILED=false
timer_stopped_notice() {
  if [ "$TIMER_ENABLE_FAILED" = true ]; then
    printf '%s\n' 'post-bridge install: post-bridge.timer is not running and may not be enabled: systemctl --user enable --now failed, so the next login or boot may not start it. Fix the error above, then re-run install.sh or run: systemctl --user enable --now post-bridge.timer' >&2
  elif [ "$TIMER_STOPPED_BY_INSTALL" = true ]; then
    printf '%s\n' 'post-bridge install: post-bridge.timer is stopped but still enabled: this install stopped it and did not restart it, so the next login or boot starts it again. Fix the error above, then re-run install.sh or run: systemctl --user start post-bridge.timer' >&2
  fi
}

require_value() {
  [ "$#" -ge 2 ] || die "missing value for $1"
}

systemd_escape() {
  # Environment= uses systemd's quoted-string escaping rules.
  local value=$1
  value=${value//\\/\\\\}
  value=${value//\"/\\\"}
  value=${value//%/%%}
  printf '%s' "$value"
}

render_template() {
  local template=$1 destination=$2
  local tmp mail_root bridge_repo bridge_host bridge_ssh_key post_bin interval
  tmp=$(mktemp "${destination}.tmp.XXXXXX")
  trap 'rm -f "$tmp"' RETURN
  mail_root=$(systemd_escape "$MAIL_ROOT")
  bridge_repo=$(systemd_escape "$CLONE_DIR")
  bridge_host=$(systemd_escape "$HOST")
  bridge_ssh_key=$(systemd_escape "$SSH_KEY")
  post_bin=$(systemd_escape "$POST_BIN")
  interval=$(systemd_escape "$INTERVAL")

  POST_BRIDGE_RENDER_MAIL_ROOT=$mail_root \
  POST_BRIDGE_RENDER_REPO=$bridge_repo \
  POST_BRIDGE_RENDER_HOST=$bridge_host \
  POST_BRIDGE_RENDER_SSH_KEY=$bridge_ssh_key \
  POST_BRIDGE_RENDER_POST_BIN=$post_bin \
  POST_BRIDGE_RENDER_INTERVAL=$interval \
  awk '
    function replace(token, value, position) {
      while ((position = index($0, token)) != 0) {
        $0 = substr($0, 1, position - 1) value substr($0, position + length(token))
      }
    }
    {
      replace("@POST_MAIL_ROOT@", ENVIRON["POST_BRIDGE_RENDER_MAIL_ROOT"])
      replace("@BRIDGE_REPO@", ENVIRON["POST_BRIDGE_RENDER_REPO"])
      replace("@BRIDGE_HOST@", ENVIRON["POST_BRIDGE_RENDER_HOST"])
      replace("@BRIDGE_SSH_KEY@", ENVIRON["POST_BRIDGE_RENDER_SSH_KEY"])
      replace("@POST_BIN@", ENVIRON["POST_BRIDGE_RENDER_POST_BIN"])
      replace("@INTERVAL@", ENVIRON["POST_BRIDGE_RENDER_INTERVAL"])
      print
    }' "$template" >"$tmp"

  if [ -f "$destination" ] && cmp -s "$tmp" "$destination"; then
    return
  fi
  mv -f "$tmp" "$destination"
}

systemctl_user_available() {
  command -v systemctl >/dev/null 2>&1 && systemctl --user show-environment >/dev/null 2>&1
}

uninstall() {
  local has_systemd=false
  if systemctl_user_available; then
    has_systemd=true
    systemctl --user disable --now post-bridge.timer >/dev/null 2>&1 || true
  else
    say 'systemctl --user is unavailable; skipping timer stop/disable'
  fi

  rm -f "$SERVICE_FILE" "$TIMER_FILE" "$LAUNCHER"
  if [ -n "$DATA_DIR" ] && [ "$DATA_DIR" != / ]; then
    rm -rf "$DATA_DIR"
  fi
  if [ "$has_systemd" = true ]; then
    systemctl --user daemon-reload
  fi
  say "removed user units, $LAUNCHER, and $DATA_DIR"
  say "left $CLONE_DIR and $MAIL_ROOT untouched"
}

REPO_URL=''
CONFIG_SRC=''
CLONE_DIR="$HOME/post-relay"
HOST=''
SSH_KEY=''
INTERVAL=15
UNINSTALL=false
MAIL_ROOT="${POST_MAIL_ROOT:-$HOME/.claude-mail}"
POST_BIN=${POST_BIN:-}

while [ "$#" -gt 0 ]; do
  case $1 in
    --repo-url)
      require_value "$@"
      REPO_URL=$2
      shift 2
      ;;
    --clone-dir)
      require_value "$@"
      CLONE_DIR=$2
      shift 2
      ;;
    --host)
      require_value "$@"
      HOST=$2
      shift 2
      ;;
    --ssh-key)
      require_value "$@"
      SSH_KEY=$2
      shift 2
      ;;
    --config)
      require_value "$@"
      CONFIG_SRC=$2
      shift 2
      ;;
    --interval)
      require_value "$@"
      INTERVAL=$2
      shift 2
      ;;
    --post-bin)
      require_value "$@"
      POST_BIN=$2
      shift 2
      ;;
    --uninstall)
      UNINSTALL=true
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *) die "unknown argument: $1" ;;
  esac
done



case $INTERVAL in
  '') die '--interval must not be empty' ;;
esac

case $CLONE_DIR in
  '') die '--clone-dir must not be empty' ;;
esac

case $MAIL_ROOT in
  *$'\n'*|*$'\r'*) die 'POST_MAIL_ROOT must not contain a newline' ;;
esac

for systemd_value in "$CLONE_DIR" "$HOST" "$SSH_KEY" "$INTERVAL" "$POST_BIN"; do
  case $systemd_value in
    *$'\n'*|*$'\r'*) die 'systemd environment values must not contain a newline' ;;
  esac
done

CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
SERVICE_FILE="$CONFIG_DIR/post-bridge.service"
TIMER_FILE="$CONFIG_DIR/post-bridge.timer"
DATA_DIR="$HOME/.local/lib/post-bridge"
LAUNCHER="$HOME/.local/bin/post-bridge-sweep"

if [ "$UNINSTALL" = true ]; then
  uninstall
  exit 0
fi

# A user service runs with systemd's minimal PATH. Resolve and validate Post
# now, then bake the absolute path into the unit.
if [ -z "$POST_BIN" ]; then
  POST_BIN=$(command -v post || true)
else
  case $POST_BIN in
    /*) ;;
    *) POST_BIN=$(command -v "$POST_BIN" || true) ;;
  esac
fi
[ -n "$POST_BIN" ] || die 'post is not on PATH; pass --post-bin <path>'
[ -f "$POST_BIN" ] && [ -x "$POST_BIN" ] || die "post binary is not an executable file: $POST_BIN"
POST_BIN=$(CDPATH='' cd -- "$(dirname -- "$POST_BIN")" && printf '%s/%s\n' "$PWD" "$(basename -- "$POST_BIN")")
POST_VERSION=$("$POST_BIN" --version 2>&1) || die "cannot run $POST_BIN --version"
post_version_accepted "$POST_VERSION" || die "post version must be $POST_VERSION_RANGE, optionally followed by ' (build ...)'; got $POST_VERSION"

[ -n "$REPO_URL" ] || die '--repo-url is required for installation'
[ -n "$SSH_KEY" ] || die '--ssh-key is required for installation'
[ -n "$CONFIG_SRC" ] || die '--config is required for installation'
[ -n "$HOST" ] || die '--host is required for installation (no hostname default)'
[ -f "$CONFIG_SRC" ] || die "--config does not exist: $CONFIG_SRC"
[ -f "$SSH_KEY" ] || die "--ssh-key does not exist: $SSH_KEY"
case $SSH_KEY in
  /*) ;;
  *) die '--ssh-key must be an absolute path' ;;
esac
case $HOST in
  *[!a-z0-9-]*) die '--host must match ^[a-z0-9-]{1,32}$' ;;
esac
[ "${#HOST}" -le 32 ] || die '--host must be at most 32 characters'

# Every git network call here and in the sweeper goes through the relay key
# only; the clone also carries it as core.sshCommand so an operator's manual
# git inside the clone cannot silently use a different identity.
SSH_COMMAND="ssh -o ConnectTimeout=10 -o BatchMode=yes -o IdentitiesOnly=yes -i $SSH_KEY"
export GIT_SSH_COMMAND="$SSH_COMMAND"

if [ -e "$CLONE_DIR" ]; then
  git -C "$CLONE_DIR" rev-parse --is-inside-work-tree >/dev/null 2>&1 || die "$CLONE_DIR exists but is not a Git working tree"
  [ "$(git -C "$CLONE_DIR" remote get-url origin 2>/dev/null || true)" = "$REPO_URL" ] || die "$CLONE_DIR is a foreign clone (origin does not match --repo-url)"
  [ -z "$(git -C "$CLONE_DIR" status --porcelain)" ] || die "$CLONE_DIR is dirty; refusing to overwrite it"
else
  mkdir -p "$(dirname "$CLONE_DIR")"
  git clone "$REPO_URL" "$CLONE_DIR"
fi
git -C "$CLONE_DIR" config core.sshCommand "$SSH_COMMAND"
git -C "$CLONE_DIR" config user.name post-bridge
git -C "$CLONE_DIR" config user.email "post-bridge@$HOST"

# The sweeper pushes HEAD to machines/<host>, so the clone must sit on that
# branch: track the forge's copy when it exists, otherwise start an orphan
# (the first sweep creates the remote branch). An existing local branch is
# only checked out, never reset -- it may hold unpushed outbox commits.
BRANCH="machines/$HOST"
# ls-remote exit 2 means the branch does not exist yet (first install on
# this host); anything else nonzero is an access failure with the relay key.
if git -C "$CLONE_DIR" ls-remote --exit-code origin "refs/heads/$BRANCH" >/dev/null 2>&1; then
  git -C "$CLONE_DIR" fetch -q origin "+refs/heads/$BRANCH:refs/remotes/origin/$BRANCH" || die "fetch of origin/$BRANCH with the relay key failed"
else
  rc=$?
  [ "$rc" -eq 2 ] || die "cannot reach origin with the relay key (ls-remote exit $rc)"
fi
CURRENT=$(git -C "$CLONE_DIR" symbolic-ref --quiet --short HEAD 2>/dev/null || true)
if [ "$CURRENT" != "$BRANCH" ]; then
  if git -C "$CLONE_DIR" show-ref --verify --quiet "refs/heads/$BRANCH"; then
    git -C "$CLONE_DIR" checkout -q "$BRANCH"
  elif git -C "$CLONE_DIR" show-ref --verify --quiet "refs/remotes/origin/$BRANCH"; then
    git -C "$CLONE_DIR" checkout -q -b "$BRANCH" "refs/remotes/origin/$BRANCH"
  else
    git -C "$CLONE_DIR" switch -q --orphan "$BRANCH"
    git -C "$CLONE_DIR" commit -q --allow-empty -m "bridge: init $BRANCH"
  fi
fi
[ "$(git -C "$CLONE_DIR" symbolic-ref --quiet --short HEAD)" = "$BRANCH" ] || die "clone is not on $BRANCH"

# The sweeper and unit templates ship next to this script (the source repo),
# never inside the relay clone, which holds only mail in transit.
SOURCE_DIR=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
SOURCE_SWEEP="$SOURCE_DIR/sweep.py"
SOURCE_BRIDGELIB="$SOURCE_DIR/bridgelib"
SOURCE_SERVICE="$SOURCE_DIR/services/post-bridge.service"
SOURCE_TIMER="$SOURCE_DIR/services/post-bridge.timer"
[ -f "$SOURCE_SWEEP" ] || die "missing sweeper next to installer: $SOURCE_SWEEP"
[ -d "$SOURCE_BRIDGELIB" ] || die "missing bridgelib next to installer: $SOURCE_BRIDGELIB"
[ -f "$SOURCE_SERVICE" ] || die "missing service template: $SOURCE_SERVICE"
[ -f "$SOURCE_TIMER" ] || die "missing timer template: $SOURCE_TIMER"

mkdir -p "$HOME/.local/bin" "$HOME/.local/lib" "$CONFIG_DIR"
package_tmp=$(mktemp -d "$HOME/.local/lib/.post-bridge.XXXXXX")
# errexit is live inside the trap: a failing rm must not skip the notice.
trap 'rm -rf "$package_tmp" || :; timer_stopped_notice' EXIT
cp -f "$SOURCE_SWEEP" "$package_tmp/sweep.py"
mkdir -p "$package_tmp/bridgelib"
for source_module in "$SOURCE_BRIDGELIB"/*.py; do
  [ -f "$source_module" ] || die "bridgelib contains no Python modules: $SOURCE_BRIDGELIB"
  cp -f "$source_module" "$package_tmp/bridgelib/"
done
# Record which post repo commit this package came from, next to the install
# ($DATA_DIR/BUILD), so "what is deployed" has an answer that is not a guess
# from file dates. No timestamp: an unchanged source reinstalls as a no-op.
# `dirty=yes` means bridge/ had uncommitted changes, so the commit alone does
# not reproduce the package; `unknown` means the source is not a Git checkout
# of this repo (a copied tree).
build_commit=unknown
build_dirty=unknown
if git -C "$SOURCE_DIR" ls-files --error-unmatch -- sweep.py >/dev/null 2>&1; then
  build_commit=$(git -C "$SOURCE_DIR" rev-parse HEAD 2>/dev/null) || build_commit=unknown
  if build_status=$(git -C "$SOURCE_DIR" status --porcelain -- . 2>/dev/null); then
    if [ -n "$build_status" ]; then
      build_dirty=yes
    else
      build_dirty=no
    fi
  fi
fi
printf 'commit=%s\ndirty=%s\n' "$build_commit" "$build_dirty" >"$package_tmp/BUILD"
chmod 0755 "$package_tmp" "$package_tmp/bridgelib"
chmod 0644 "$package_tmp/sweep.py" "$package_tmp/bridgelib"/*.py "$package_tmp/BUILD"
# Stop the timer before the package swap, so no tick of the new package runs
# before --init-held-sentinel below writes the local-held floors (review 4,
# finding 2). enable --now restarts it once the checks pass; a failure before
# that leaves it stopped but enabled (never disabled: review 5, finding 1),
# and the EXIT trap says so. A tick already running finishes under the tick
# lock, which --init-held-sentinel waits for. launchd is not managed here.
if systemctl_user_available && systemctl --user is-active --quiet post-bridge.timer; then
  systemctl --user stop post-bridge.timer || die "could not stop post-bridge.timer before the package swap"
  TIMER_STOPPED_BY_INSTALL=true
fi
if [ ! -d "$DATA_DIR" ] || ! diff -qr "$package_tmp" "$DATA_DIR" >/dev/null 2>&1; then
  old_package="$DATA_DIR.old.$$"
  rm -rf "$old_package"
  if [ -e "$DATA_DIR" ] || [ -L "$DATA_DIR" ]; then
    mv -f "$DATA_DIR" "$old_package"
  fi
  mv -f "$package_tmp" "$DATA_DIR"
  rm -rf "$old_package"
else
  rm -rf "$package_tmp"
fi
trap timer_stopped_notice EXIT
say "installed the bridge package from post repo commit $build_commit (dirty=$build_dirty), recorded in $DATA_DIR/BUILD"

launcher_tmp=$(mktemp "$HOME/.local/bin/.post-bridge-sweep.XXXXXX")
# macOS launchd captures stdout in $POST_MAIL_ROOT/bridge/launchd.log (the
# plist's StandardOutPath), which nothing rotates. launchd reopens that path at
# each spawn, so the launcher renames it past the log.jsonl cap and the next
# tick starts a fresh file. Elsewhere the file does not exist and this is inert.
{
  cat <<'SH'
#!/bin/sh
if [ -n "${POST_MAIL_ROOT:-}" ]; then
  stdout_log="$POST_MAIL_ROOT/bridge/launchd.log"
  if [ -f "$stdout_log" ] && [ ! -L "$stdout_log" ]; then
    size=$(wc -c <"$stdout_log" 2>/dev/null | tr -d ' ')
    case $size in '' | *[!0-9]*) size=0 ;; esac
    if [ "$size" -ge 10485760 ]; then
      mv -f "$stdout_log" "$stdout_log.1" 2>/dev/null || :
    fi
  fi
fi
SH
  printf 'exec python3 "%s/sweep.py" "$@"\n' "$DATA_DIR"
} >"$launcher_tmp"
chmod 0755 "$launcher_tmp"
if [ ! -f "$LAUNCHER" ] || ! cmp -s "$launcher_tmp" "$LAUNCHER"; then
  mv -f "$launcher_tmp" "$LAUNCHER"
else
  rm -f "$launcher_tmp"
fi

# Topology config: exclusive create; an identical file is fine, a differing
# one is refused because silently re-homing rooms is how mail gets lost.
CONFIG_DEST="$MAIL_ROOT/bridge/config.json"
mkdir -p "$MAIL_ROOT/bridge"
# The sweeper serializes health.json writes on this file but never creates
# it (exit 2 may write health.json only), so the installer owns its birth.
touch "$MAIL_ROOT/bridge/.health.lock"
if [ -e "$CONFIG_DEST" ]; then
  cmp -s "$CONFIG_SRC" "$CONFIG_DEST" || die "$CONFIG_DEST exists and differs from --config; remove it deliberately first"
else
  config_tmp=$(mktemp "$MAIL_ROOT/bridge/.config.json.XXXXXX")
  cp "$CONFIG_SRC" "$config_tmp"
  chmod 0644 "$config_tmp"
  if ! ln "$config_tmp" "$CONFIG_DEST" 2>/dev/null; then
    rm -f "$config_tmp"
    cmp -s "$CONFIG_SRC" "$CONFIG_DEST" || die "$CONFIG_DEST appeared concurrently and differs from --config"
  fi
  rm -f "$config_tmp"
fi

# No channels key means every channel syncs (SPEC-v2 r6.2), and a deny list
# only protects anything on the host that publishes the channel. Warn, not
# refuse: sync-all is a legitimate default, but a new host should normally
# start from config.template.json, which carries the estate deny list.
if ! python3 -c 'import json, sys; sys.exit(0 if "channels" in json.load(open(sys.argv[1])) else 1)' "$CONFIG_DEST" 2>/dev/null; then
  warn "$CONFIG_DEST has no \"channels\" key, so this host will publish and import every channel. Deny only works at the publisher: start from bridge/config.template.json to carry the estate deny list, or set \"channels\" deliberately."
fi

render_template "$SOURCE_SERVICE" "$SERVICE_FILE"
render_template "$SOURCE_TIMER" "$TIMER_FILE"

# The sweeper validates env + config + clone identity before the timer is
# allowed to run; a failing check leaves the units installed and the timer not
# started (stopped, if this install stopped it; see the EXIT trap).
env POST_MAIL_ROOT="$MAIL_ROOT" BRIDGE_REPO="$CLONE_DIR" BRIDGE_HOST="$HOST" \
  BRIDGE_SSH_KEY="$SSH_KEY" POST_BIN="$POST_BIN" \
  "$LAUNCHER" --check-config || die "sweep.py --check-config failed; the timer was not started"

# The local-held guard's floors live in bridge/held-guard-sentinel. A store
# stamped by an older bridge has none, so create it now, under the tick
# lock, before any new tick could meet that store wiped (SPEC-v2 r6.1.1).
env POST_MAIL_ROOT="$MAIL_ROOT" BRIDGE_REPO="$CLONE_DIR" BRIDGE_HOST="$HOST" \
  BRIDGE_SSH_KEY="$SSH_KEY" POST_BIN="$POST_BIN" \
  "$LAUNCHER" --init-held-sentinel || die "sweep.py --init-held-sentinel failed; the timer was not started"

if ! systemctl_user_available; then
  say 'systemctl --user is unavailable; units installed but timer was not enabled or started'
  exit 0
fi

systemctl --user daemon-reload || die "systemctl --user daemon-reload failed; the timer was not started"
if ! systemctl --user enable --now post-bridge.timer; then
  TIMER_ENABLE_FAILED=true
  die "systemctl --user enable --now post-bridge.timer failed"
fi
TIMER_STOPPED_BY_INSTALL=false
systemctl --user list-timers 'post-bridge*'
say 'timer enabled; the first tick runs on the timer, the service was not started'
