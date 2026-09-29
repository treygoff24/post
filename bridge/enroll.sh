#!/usr/bin/env bash
set -euo pipefail

API_REPO='/repos/estate/post-relay'
RELAY_URL=${POST_BRIDGE_RELAY_URL:-ssh://git@10.17.198.1:2222/estate/post-relay.git}
DRY_RUN=false
INIT_REGISTRY=false
MODE=enroll
HOST=''
TMP=''

usage() {
  cat <<'EOF'
Usage: enroll.sh --host <host> [--dry-run] [--init-registry]
       enroll.sh --verify <host>
EOF
}

die() {
  printf 'post-bridge enroll: %s\n' "$*" >&2
  exit 1
}

cleanup() {
  if [ -n "$TMP" ] && [ -d "$TMP" ]; then
    rm -rf "$TMP"
  fi
}
trap cleanup EXIT

require_value() {
  [ "$#" -ge 2 ] || die "missing value for $1"
}

while [ "$#" -gt 0 ]; do
  case $1 in
    --host)
      require_value "$@"
      HOST=$2
      shift 2
      ;;
    --verify)
      require_value "$@"
      MODE=verify
      HOST=$2
      shift 2
      ;;
    --dry-run)
      DRY_RUN=true
      shift
      ;;
    --init-registry)
      INIT_REGISTRY=true
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *) die "unknown argument: $1" ;;
  esac
done

[ -n "$HOST" ] || die '--host or --verify is required'
case $HOST in
  *[!a-z0-9-]*) die 'host must match ^[a-z0-9-]{1,32}$' ;;
esac
[ "${#HOST}" -le 32 ] || die 'host must be at most 32 characters'
[ "$MODE" = enroll ] || [ "$INIT_REGISTRY" = false ] || die '--init-registry is not valid with --verify'
command -v fj >/dev/null 2>&1 || die 'fj is required'
command -v jq >/dev/null 2>&1 || die 'jq is required'
command -v git >/dev/null 2>&1 || die 'git is required'

api_get() {
  local path=$1 body
  printf '+ fj GET %s\n' "$path" >&2
  body=$(fj GET "$path") || die "GET $path failed"
  printf '%s' "$body"
}

api_write() {
  local method=$1 path=$2 payload=$3 body
  if [ "$DRY_RUN" = true ]; then
    printf '+ fj %s %s body <<< %s\n' "$method" "$path" "$payload"
    return
  fi
  printf '+ fj %s %s\n' "$method" "$path" >&2
  body=$(printf '%s' "$payload" | fj "$method" "$path" body) || die "$method $path failed"
  if printf '%s' "$body" | jq -e 'type == "object" and has("message")' >/dev/null 2>&1; then
    die "$method $path failed: $body"
  fi
}

api_error() {
  printf '%s' "$1" | jq -e 'type == "object" and has("message")' >/dev/null 2>&1
}

api_scope_error() {
  printf '%s' "$1" | jq -e '
    type == "object" and (.message | type) == "string" and (.message | test("scope"))' \
    >/dev/null 2>&1
}

manual_user_step() {
  printf 'Manual step: create Forgejo user %s and install its relay SSH public key, then rerun enroll.sh.\n' "$HOST" >&2
}

USER_BODY=$(api_get "/users/$HOST")
if api_error "$USER_BODY"; then
  if api_scope_error "$USER_BODY"; then
    printf '%s\n' 'user check unavailable (token lacks read:user); relying on the branch-protection API to validate the username' >&2
  elif [ "$DRY_RUN" = false ]; then
    printf 'post-bridge enroll: GET /users/%s failed: %s\n' "$HOST" "$USER_BODY" >&2
    manual_user_step
    exit 1
  fi
  if [ "$DRY_RUN" = true ]; then
    printf '# user check returned: %s\n' "$USER_BODY"
  fi
else
  USER_LOGIN=$(printf '%s' "$USER_BODY" | jq -r '.login // empty')
  [ "$USER_LOGIN" = "$HOST" ] || die "GET /users/$HOST returned a different user"
fi

PROTECTIONS=$(api_get "$API_REPO/branch_protections")
if api_error "$PROTECTIONS"; then
  [ "$DRY_RUN" = true ] || die "cannot list branch protections: $PROTECTIONS"
  PROTECTIONS='[]'
fi
BRANCHES=$(api_get "$API_REPO/branches")
if api_error "$BRANCHES"; then
  [ "$DRY_RUN" = true ] || die "cannot list branches: $BRANCHES"
  BRANCHES='[]'
fi

protection_payload() {
  local rule=$1 users=$2
  jq -cn --arg rule "$rule" --argjson users "$users" '{rule_name:$rule,enable_push:true,enable_push_whitelist:true,push_whitelist_usernames:$users,push_whitelist_teams:[],push_whitelist_deploy_keys:false}'
}

verify_protection() {
  local rule=$1 users=$2 record
  record=$(printf '%s' "$PROTECTIONS" | jq -c --arg rule "$rule" '.[] | select(.rule_name == $rule)' | head -n 1)
  [ -n "$record" ] || return 1
  printf '%s' "$record" | jq -e --argjson users "$users" '
    .enable_push == true and
    .enable_push_whitelist == true and
    (.push_whitelist_usernames | sort) == ($users | sort) and
    (.push_whitelist_teams // []) == [] and
    .push_whitelist_deploy_keys == false' >/dev/null
}

HOST_RULE="machines/$HOST"
HOST_USERS=$(jq -cn --arg host "$HOST" '[$host]')
if ! verify_protection "$HOST_RULE" "$HOST_USERS"; then
  if printf '%s' "$PROTECTIONS" | jq -e --arg rule "$HOST_RULE" 'any(.[]; .rule_name == $rule)' >/dev/null; then
    die "$HOST_RULE protection exists but is not exact; refusing to loosen or replace it"
  fi
  [ "$MODE" = enroll ] || die "$HOST_RULE protection is missing"
  api_write POST "$API_REPO/branch_protections" "$(protection_payload "$HOST_RULE" "$HOST_USERS")"
fi

REGISTRY_USERS='["trey","mac"]'
if ! verify_protection registry "$REGISTRY_USERS"; then
  if printf '%s' "$PROTECTIONS" | jq -e 'any(.[]; .rule_name == "registry")' >/dev/null; then
    die 'registry protection exists but is not operator-only (trey, mac)'
  fi
  if [ "$INIT_REGISTRY" = true ]; then
    api_write POST "$API_REPO/branch_protections" "$(protection_payload registry "$REGISTRY_USERS")"
  elif [ "$DRY_RUN" = false ]; then
    die 'registry protection is missing; rerun with --init-registry'
  else
    printf '# registry protection is missing; --init-registry would create it\n'
  fi
fi

# Forgejo cannot express a dynamic "branch owner" in a wildcard rule. The
# catch-all is therefore only a deny-by-default backstop; exact per-host rules
# above grant each owner its branch.
if ! printf '%s' "$PROTECTIONS" | jq -e 'any(.[]; .rule_name == "machines/*" and .enable_push == true and .enable_push_whitelist == true and .push_whitelist_deploy_keys == false)' >/dev/null; then
  die 'machines/* owner-only catch-all protection is missing or unsafe'
fi

REGISTRY_EXISTS=false
if printf '%s' "$BRANCHES" | jq -e 'any(.[]; .name == "registry")' >/dev/null; then
  REGISTRY_EXISTS=true
fi

if [ "$MODE" = verify ]; then
  [ "$REGISTRY_EXISTS" = true ] || die 'registry branch is missing'
  TMP=$(mktemp -d)
  git clone -q --branch registry "$RELAY_URL" "$TMP/relay"
  jq -e --arg host "$HOST" '
    type == "object" and keys == ["hosts","v"] and .v == 1 and
    (.hosts | type) == "array" and (.hosts | index($host)) != null' \
    "$TMP/relay/hosts.json" >/dev/null || die "$HOST is not present in registry hosts.json"
  printf 'post-bridge enroll: %s enrollment verified\n' "$HOST"
  exit 0
fi

if [ "$DRY_RUN" = true ]; then
  if [ "$REGISTRY_EXISTS" = false ] && [ "$INIT_REGISTRY" = true ]; then
    printf '+ git clone --no-checkout %s <temp>/relay\n' "$RELAY_URL"
    printf '+ git -C <temp>/relay switch --orphan registry\n'
    printf '+ write hosts.json {"v":1,"hosts":[]}\n'
    printf '+ git -C <temp>/relay commit -m registry: init\n'
    printf '+ git -C <temp>/relay push origin HEAD:refs/heads/registry\n'
  fi
  printf '+ git clone --branch registry %s <temp>/relay\n' "$RELAY_URL"
  printf '+ add %s to hosts.json (sorted, unique)\n' "$HOST"
  printf '+ git -C <temp>/relay commit -m registry: enroll %s\n' "$HOST"
  printf '+ git -C <temp>/relay push origin HEAD:refs/heads/registry\n'
  printf "post chat machineroom-devbox --send --body 'Enrolled post-bridge host %s.'\n" "$HOST"
  exit 0
fi

if [ "$REGISTRY_EXISTS" = false ]; then
  [ "$INIT_REGISTRY" = true ] || die 'registry branch is missing; rerun with --init-registry'
  TMP=$(mktemp -d)
  git clone -q --no-checkout "$RELAY_URL" "$TMP/relay"
  git -C "$TMP/relay" switch -q --orphan registry
  git -C "$TMP/relay" rm -rf --ignore-unmatch . >/dev/null
  printf '%s\n' '{"v":1,"hosts":[]}' >"$TMP/relay/hosts.json"
  git -C "$TMP/relay" add -- hosts.json
  git -C "$TMP/relay" -c user.name=post-bridge-operator -c user.email=post-bridge-operator@estate.invalid commit -q -m 'registry: init'
  git -C "$TMP/relay" push -q origin HEAD:refs/heads/registry
  cleanup
  TMP=''
fi

TMP=$(mktemp -d)
git clone -q --branch registry "$RELAY_URL" "$TMP/relay"
HOSTS_FILE="$TMP/relay/hosts.json"
[ -f "$HOSTS_FILE" ] || die 'registry branch has no hosts.json'
UPDATED=$(jq -c --arg host "$HOST" '
  if type != "object" or keys != ["hosts","v"] or .v != 1 or (.hosts | type) != "array"
  then error("invalid hosts.json")
  else {v:1,hosts:((.hosts + [$host]) | unique | sort)} end' "$HOSTS_FILE") || die 'registry hosts.json is invalid'
printf '%s\n' "$UPDATED" >"$HOSTS_FILE"
git -C "$TMP/relay" add -- hosts.json
if git -C "$TMP/relay" diff --cached --quiet; then
  printf 'post-bridge enroll: %s is already enrolled\n' "$HOST"
else
  git -C "$TMP/relay" -c user.name=post-bridge-operator -c user.email=post-bridge-operator@estate.invalid commit -q -m "registry: enroll $HOST"
  git -C "$TMP/relay" push -q origin HEAD:refs/heads/registry
fi

printf "post chat machineroom-devbox --send --body 'Enrolled post-bridge host %s.'\n" "$HOST"
