#!/usr/bin/env bash
# Build post from one commit and install it, leaving an install receipt.
#
#   scripts/install-post.sh [--dry-run] [--bin-dir DIR] [--served PATH]
#                           [--receipt PATH] [--repo DIR] <commit>
#
# 1. Check out <commit> in a temporary git worktree and run
#    `cargo build --release --locked` there (CARGO_TARGET_DIR is honored).
# 2. Run scripts/install-smoke.sh from that commit against the built binary,
#    before anything in the bin dir is touched: a failed smoke installs nothing.
# 3. Back up the current <bin-dir>/post to <bin-dir>/post-<old-build-sha>.bak,
#    unless that backup already exists.
# 4. Install atomically: `install` to <bin-dir>/post.new, then `mv` over post,
#    and confirm the installed bytes are the built bytes.
# 5. Verify the served skill path (default ~/.agents/skill-library/post)
#    against the skill manifest built into the binary, recording whether the
#    served root is a symlink or a rendered copy.
# 6. Write the receipt (default ~/.local/share/post/install-receipt.json):
#    commit, build sha, binary sha256, served-path kind, manifest verdict,
#    smoke verdict.
#
# Skill drift does not roll the install back: served prose is updated by
# syncing the served checkout, not by un-installing the binary. It is recorded
# in the receipt and the script exits 1 so it cannot pass unnoticed.
#
# --dry-run builds and smokes, verifies the manifest, and prints what it would
# do; it never backs up, installs, or writes a receipt.
#
# Exit: 0 installed and the served skill matches; 1 installed with skill drift;
# 2 usage; 3 build, smoke, or install failure (nothing installed unless the
# message says otherwise).
set -Eeuo pipefail

die() { printf 'install-post: %s\n' "$1" >&2; exit "${2:-3}"; }
note() { printf 'install-post: %s\n' "$*" >&2; }

dry_run=0
bin_dir="$HOME/.local/bin"
served="$HOME/.agents/skill-library/post"
receipt="$HOME/.local/share/post/install-receipt.json"
repo="$(cd "$(dirname "$0")/.." && pwd)"
commit=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --dry-run) dry_run=1 ;;
    --bin-dir) [ "$#" -ge 2 ] || die "--bin-dir needs a value" 2; bin_dir="$2"; shift ;;
    --served) [ "$#" -ge 2 ] || die "--served needs a value" 2; served="$2"; shift ;;
    --receipt) [ "$#" -ge 2 ] || die "--receipt needs a value" 2; receipt="$2"; shift ;;
    --repo) [ "$#" -ge 2 ] || die "--repo needs a value" 2; repo="$2"; shift ;;
    -h|--help) sed -n '2,31p' "$0"; exit 0 ;;
    -*) die "unknown option: $1" 2 ;;
    *) [ -z "$commit" ] || die "one commit only" 2; commit="$1" ;;
  esac
  shift
done
[ -n "$commit" ] || die "usage: install-post.sh [--dry-run] [--bin-dir DIR] [--served PATH] [--receipt PATH] [--repo DIR] <commit>" 2
for tool in git cargo python3 install; do
  command -v "$tool" >/dev/null 2>&1 || die "$tool not found"
done

full_sha=$(git -C "$repo" rev-parse --verify --quiet "${commit}^{commit}") || die "not a commit in $repo: $commit" 2
short_sha=$(git -C "$repo" rev-parse --short "$full_sha")

work=$(mktemp -d "${TMPDIR:-/tmp}/install-post.XXXXXX")
src="$work/src"
# shellcheck disable=SC2329 # invoked by the EXIT trap
cleanup() {
  if [ -d "$src" ]; then
    git -C "$repo" worktree remove --force "$src" >/dev/null 2>&1 || note "left temporary worktree $src"
  fi
  rm -rf "$work"
}
trap cleanup EXIT

note "building $short_sha in a temporary worktree"
git -C "$repo" worktree add --detach --quiet "$src" "$full_sha" || die "could not create a worktree for $short_sha"
( cd "$src" && cargo build --release --locked ) >&2 || die "cargo build --release --locked failed at $short_sha"
target_dir="${CARGO_TARGET_DIR:-$src/target}"
case "$target_dir" in /*) ;; *) target_dir="$src/$target_dir" ;; esac
built="$target_dir/release/post"
[ -x "$built" ] || die "no binary at $built"
# Copy it out: a shared CARGO_TARGET_DIR can be rebuilt under us.
cp "$built" "$work/post" || die "could not copy the built binary"
built="$work/post"

built_sha=$(python3 -c 'import json,sys; print(json.load(sys.stdin)["build_sha"])' < <("$built" version --json)) \
  || die "the built binary has no readable version --json"
[ "$built_sha" = "$short_sha" ] || die "built binary reports build $built_sha, expected $short_sha"
binary_sha256=$(python3 -c 'import hashlib,sys; print(hashlib.sha256(open(sys.argv[1],"rb").read()).hexdigest())' "$built")

smoke="$src/scripts/install-smoke.sh"
[ -x "$smoke" ] || die "$short_sha has no scripts/install-smoke.sh; it predates the install procedure"
note "running install-smoke against the built binary"
if "$smoke" "$built" >&2; then
  smoke_verdict=pass
else
  die "install-smoke failed for $short_sha; nothing was installed"
fi

# Served skill path against the manifest built into this binary. Exit 1 is
# drift; anything else nonzero means the path could not be checked.
manifest_json="$work/manifest.json"
set +e
"$built" contract skill-manifest --verify "$served" > "$manifest_json" 2> "$work/manifest.err"
manifest_rc=$?
set -e
case "$manifest_rc" in
  0|1)
    manifest_verdict=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["verdict"])' "$manifest_json")
    served_kind=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["kind"])' "$manifest_json")
    ;;
  *)
    manifest_verdict=unchecked
    served_kind=unreadable
    note "served skill path $served could not be checked: $(tr -d '\n' < "$work/manifest.err" | cut -c1-300)"
    ;;
esac
if [ "$manifest_verdict" = drift ]; then
  note "skill drift at $served: $(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print({k: d[k] for k in ("mismatched","missing","extra") if d[k]})' "$manifest_json")"
fi

target="$bin_dir/post"
old_sha=""
backup=""
if [ -e "$target" ]; then
  old_sha=$("$target" version --json 2>/dev/null | python3 -c 'import json,sys; print(json.load(sys.stdin).get("build_sha") or "")' 2>/dev/null || true)
  if [ -z "$old_sha" ] || [ "$old_sha" = unknown ]; then
    # An old post without version --json, or a build outside git: name the
    # backup by content instead.
    old_sha="sha256-$(python3 -c 'import hashlib,sys; print(hashlib.sha256(open(sys.argv[1],"rb").read()).hexdigest()[:12])' "$target")"
  fi
  backup="$bin_dir/post-$old_sha.bak"
fi

if [ "$dry_run" -eq 1 ]; then
  note "dry run: would install $short_sha (sha256 $binary_sha256) to $target"
  if [ -n "$backup" ]; then
    if [ -e "$backup" ]; then note "dry run: backup $backup already exists; would keep it"; else note "dry run: would back up the current post to $backup"; fi
  fi
  note "dry run: smoke $smoke_verdict; served $served ($served_kind) manifest $manifest_verdict; would write $receipt"
  exit 0
fi

mkdir -p "$bin_dir"
if [ -n "$backup" ]; then
  if [ -e "$backup" ]; then
    note "backup $backup already exists; keeping it"
  else
    cp -p "$target" "$backup" || die "could not back up $target to $backup; nothing was installed"
    note "backed up the current post to $backup"
  fi
fi
install -m 0755 "$built" "$target.new" || die "could not stage $target.new; nothing was installed"
mv -f "$target.new" "$target" || die "could not move $target.new over $target"
installed_sha256=$(python3 -c 'import hashlib,sys; print(hashlib.sha256(open(sys.argv[1],"rb").read()).hexdigest())' "$target")
[ "$installed_sha256" = "$binary_sha256" ] || die "installed bytes at $target differ from the build (installed, but verify it by hand)"
note "installed $short_sha to $target"

mkdir -p "$(dirname "$receipt")"
COMMIT="$full_sha" BUILD_SHA="$built_sha" BINARY_SHA256="$binary_sha256" BIN_PATH="$target" \
BACKUP="$backup" SERVED="$served" SERVED_KIND="$served_kind" MANIFEST_VERDICT="$manifest_verdict" \
SMOKE_VERDICT="$smoke_verdict" RECEIPT="$receipt" python3 - <<'PY'
import datetime, json, os
receipt = os.environ["RECEIPT"]
record = {
    "installed_at": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
    "commit": os.environ["COMMIT"],
    "build_sha": os.environ["BUILD_SHA"],
    "binary_sha256": os.environ["BINARY_SHA256"],
    "bin_path": os.environ["BIN_PATH"],
    "backup": os.environ["BACKUP"] or None,
    "served_path": os.environ["SERVED"],
    "served_kind": os.environ["SERVED_KIND"],
    "manifest_verdict": os.environ["MANIFEST_VERDICT"],
    "smoke_verdict": os.environ["SMOKE_VERDICT"],
}
tmp = receipt + ".tmp"
with open(tmp, "w") as handle:
    json.dump(record, handle, indent=2)
    handle.write("\n")
os.replace(tmp, receipt)
PY
note "receipt: $receipt"

[ "$manifest_verdict" = match ] || exit 1
exit 0
