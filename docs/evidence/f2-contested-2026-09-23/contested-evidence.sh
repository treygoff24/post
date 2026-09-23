#!/usr/bin/env bash
# Emit one JSON line per archive letter the last full tick logged as
# route_contested: id, room, archive sha256, local mailbox copies, and
# whether a bridge published/received/delivered marker exists.
set -u
host=${1:?host label}
M="$HOME/.claude-mail"
log="$M/bridge/log.jsonl"
last=$(grep '"action":"health"' "$log" | tail -2 | head -1 | jq -r .ts)
grep '"action":"route_contested"' "$log" \
  | jq -c --arg last "$last" 'select(.ts > $last) | {id, room}' | sort -u \
  | while IFS= read -r line; do
      id=$(jq -r .id <<<"$line")
      room=$(jq -r .room <<<"$line")
      if command -v sha256sum >/dev/null; then
        sha=$(sha256sum "$M/archive/$id.mail" | cut -d' ' -f1)
      else
        sha=$(shasum -a 256 "$M/archive/$id.mail" | cut -d' ' -f1)
      fi
      locs=$(find "$M/$room" "$M/participants" -name "$id.mail" 2>/dev/null | sed "s#$M/##" | paste -sd, -)
      markers=$(find "$M/bridge/published" "$M/bridge/received" "$M/bridge/delivered" -name "$id*" 2>/dev/null | sed "s#$M/##" | paste -sd, -)
      jq -nc --arg host "$host" --arg id "$id" --arg room "$room" --arg sha "$sha" \
        --arg locs "$locs" --arg markers "$markers" \
        '{host:$host, id:$id, room:$room, archive_sha256:$sha,
          local_copies:($locs|split(",")|map(select(.!=""))),
          bridge_markers:($markers|split(",")|map(select(.!="")))}'
    done
