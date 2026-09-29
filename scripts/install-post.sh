#!/usr/bin/env bash
# Build post from one commit and install it, leaving an install receipt.
#
#   scripts/install-post.sh [--dry-run] [--allow-unreachable] [--bin-dir DIR]
#                           [--served PATH] [--receipt PATH] [--repo DIR] <commit>
#
# 0. Refuse a <commit> that no branch on origin contains. Every build installed
#    on a host must be traceable to a branch other hosts can fetch: an install
#    of an unpushed or abandoned-branch commit leaves a running binary whose
#    build id names a commit that exists on no remote (the devbox ran one for
#    days). The script runs `git fetch --prune origin`, then asks
#    `git branch -r --contains`; the branches found are recorded in the
#    receipt. The fetch must succeed: a stale clone still holds the
#    remote-tracking ref of a branch that was deleted on origin, so judging by
#    what was fetched last time can call an abandoned commit reachable. When
#    the fetch fails (or there is no origin) the script refuses and says so.
#    --allow-unreachable installs anyway, for the rare deliberate case, and
#    the receipt records reachable=false (origin was asked and no branch holds
#    the commit) or reachable="unverified" (origin could not be asked).
# 1. Refuse a <bin-dir>/post that is a symlink or not a regular file: the
#    install would silently replace the link with a file.
# 2. Check out <commit> in a temporary git worktree and run
#    `cargo build --release --locked` there (CARGO_TARGET_DIR is honored).
# 3. Run scripts/install-smoke.sh from that commit against the built binary,
#    before anything in the bin dir is touched: a failed smoke installs nothing.
#    The smoke is told the commit (--expect-build), so it fails a binary whose
#    build id is not that commit, and it times `post who` against a store as
#    wide as the live hosts.
#    Every check must run and pass. The results must name each of the six
#    checks (setup, version, build_id, samples, who_speed, porch) exactly once
#    and no other; a smoke that exits 0 with one missing, repeated, or unknown
#    installs nothing. Only porch may be skipped, and only when the operator
#    allowed it (POST_SMOKE_ALLOW_SKIP=porch, passed through to the smoke); the
#    receipt then says pass_with_skips, never pass.
# 4. Back up the current <bin-dir>/post to <bin-dir>/post-<old-build-sha>.bak.
#    The copy goes to a temporary name, its sha256 is checked against the live
#    file, and only then is it renamed into place. An existing backup of that
#    name is kept only when its sha256 equals the live file's; otherwise the
#    backup is written to post-<old-build-sha>-<sha256[:12]>.bak, and if that
#    name also holds other bytes the script stops before touching post.
# 5. Install atomically: `install` to <bin-dir>/post.new, `mv` over post, and
#    confirm the installed bytes are the built bytes. If they are not, the
#    verified backup is restored over post (a first install removes the bad
#    file) and the script exits 5.
# 6. Verify the served skill path (default ~/.agents/skill-library/post)
#    against the skill manifest built into the binary, recording whether the
#    served root is a symlink or a rendered copy.
# 7. Write the receipt (default ~/.local/share/post/install-receipt.json):
#    commit, build sha, the origin branches that contain it, binary sha256,
#    backup path and sha256, served-path kind, manifest verdict, smoke verdict
#    and per-check smoke results.
#
# Skill drift does not roll the install back: served prose is updated by
# syncing the served checkout, not by un-installing the binary. It is recorded
# in the receipt and the exit code says so, so it cannot pass unnoticed.
#
# --dry-run builds and smokes, verifies the manifest, and prints what it would
# do; it never backs up, installs, or writes a receipt. It exits with the code
# the real install would (0, 1, or 4 for the served-skill verdict; 3 for a
# refusal before install), but a dry run writes nothing whatever its code.
#
# Exit codes:
#   0  installed; served skill matches (manifest verdict match).
#   1  installed; served skill drift (a changed, missing, or extra file).
#   2  usage.
#   3  nothing installed; post is untouched: refused target, a commit no origin
#      branch contains or origin that could not be fetched, build or smoke
#      failure, unusable backup, or a failed backup or staging step.
#   4  installed; served skill not verified: the path could not be checked
#      (verdict unchecked), or a rendered copy's fenced file differs (verdict
#      unverified).
#   5  install failed verification and was rolled back: post holds the
#      pre-install bytes again, or is absent again after a first install.
#   6  install failed and the rollback failed: post may hold bad bytes.
#      Restore it by hand from the backup the message names.
#   7  installed and verified, but the receipt could not be written.
# An unexpected failure exits with the code of the phase it happened in:
# 3 before post is replaced, 6 while it is being replaced, 7 after.
set -Eeuo pipefail

# The code an unexpected failure exits with; each phase moves it forward.
fail_code=3
die() { printf 'install-post: %s\n' "$1" >&2; exit "${2:-3}"; }
note() { printf 'install-post: %s\n' "$*" >&2; }
trap 'die "unexpected failure at line $LINENO (exit code $fail_code; see the header)" "$fail_code"' ERR

dry_run=0
allow_unreachable=0
bin_dir="$HOME/.local/bin"
served="$HOME/.agents/skill-library/post"
receipt="$HOME/.local/share/post/install-receipt.json"
repo="$(cd "$(dirname "$0")/.." && pwd)"
commit=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --dry-run) dry_run=1 ;;
    --allow-unreachable) allow_unreachable=1 ;;
    --bin-dir) [ "$#" -ge 2 ] || die "--bin-dir needs a value" 2; bin_dir="$2"; shift ;;
    --served) [ "$#" -ge 2 ] || die "--served needs a value" 2; served="$2"; shift ;;
    --receipt) [ "$#" -ge 2 ] || die "--receipt needs a value" 2; receipt="$2"; shift ;;
    --repo) [ "$#" -ge 2 ] || die "--repo needs a value" 2; repo="$2"; shift ;;
    -h|--help) sed -n '2,78p' "$0"; exit 0 ;;
    -*) die "unknown option: $1" 2 ;;
    *) [ -z "$commit" ] || die "one commit only" 2; commit="$1" ;;
  esac
  shift
done
[ -n "$commit" ] || die "usage: install-post.sh [--dry-run] [--allow-unreachable] [--bin-dir DIR] [--served PATH] [--receipt PATH] [--repo DIR] <commit>" 2
for tool in git cargo python3 install; do
  command -v "$tool" >/dev/null 2>&1 || die "$tool not found"
done

sha256_of() { python3 -c 'import hashlib,sys; print(hashlib.sha256(open(sys.argv[1],"rb").read()).hexdigest())' "$1"; }

target="$bin_dir/post"
# Refuse before any write, and before the build: install plus mv would
# replace a symlink with a regular file and leave its target stale.
refuse_unusable_target() {
  if [ -L "$target" ]; then
    die "$target is a symlink to $(readlink "$target"); refusing to replace a link with a file. Install to the link's target with --bin-dir, or remove the link first. Nothing was installed."
  fi
  if [ -e "$target" ] && [ ! -f "$target" ]; then
    die "$target exists and is not a regular file; nothing was installed"
  fi
}
refuse_unusable_target

full_sha=$(git -C "$repo" rev-parse --verify --quiet "${commit}^{commit}") || die "not a commit in $repo: $commit" 2
short_sha=$(git -C "$repo" rev-parse --short "$full_sha")

# Traceability: refuse a commit no branch on origin contains, before the
# (slow) build. Fetch with --prune first so a branch pushed since the last
# fetch counts and a branch deleted since does not. A fetch that fails proves
# nothing either way (the cached refs may name a branch origin has dropped), so
# it is a refusal, not a fallback. reachable is true, false (origin says no
# branch holds it), or unverified (origin could not be asked).
origin_branches=""
reachable=true
refuse_unreachable_commit() {
  local why="" verdict=false
  if ! git -C "$repo" remote get-url origin >/dev/null 2>&1; then
    why="this checkout has no origin remote to check it against"
    verdict=unverified
  elif ! GIT_TERMINAL_PROMPT=0 git -C "$repo" fetch --quiet --prune origin >&2; then
    why="could not fetch origin, so whether a branch there still holds it is unknown (the branches fetched earlier may since have been deleted)"
    verdict=unverified
  else
    # `git branch -r --contains` lists every remote's branches; only origin's
    # count, and origin/HEAD is a pointer, not a branch.
    origin_branches=$(git -C "$repo" branch -r --contains "$full_sha" --format='%(refname)' 2>/dev/null |
      awk 'index($0, "refs/remotes/origin/") == 1 { name = substr($0, 21); if (name != "HEAD") print name }' | paste -sd, -) || origin_branches=""
    [ -n "$origin_branches" ] || why="no branch on origin contains it"
  fi
  [ -n "$why" ] || return 0
  if [ "$allow_unreachable" -eq 1 ]; then
    reachable=$verdict
    note "WARNING: $short_sha is not traceable to a branch ($why); installing anyway because of --allow-unreachable, and the receipt records reachable=$verdict"
    return 0
  fi
  die "refusing to install $short_sha: $why. Every installed build must be traceable to a branch other hosts can fetch. Push the branch that holds it (git push origin <branch>; a Forgejo push needs no authorization) and rerun, or pass --allow-unreachable for a deliberate exception. Nothing was installed."
}
refuse_unreachable_commit

work=$(mktemp -d "${TMPDIR:-/tmp}/install-post.XXXXXX")
src="$work/src"
backup_tmp=""
# shellcheck disable=SC2329 # invoked by the EXIT trap
cleanup() {
  if [ -d "$src" ]; then
    git -C "$repo" worktree remove --force "$src" >/dev/null 2>&1 || note "left temporary worktree $src"
  fi
  if [ -n "$backup_tmp" ]; then rm -f "$backup_tmp"; fi
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
binary_sha256=$(sha256_of "$built")

smoke="$src/scripts/install-smoke.sh"
[ -x "$smoke" ] || die "$short_sha has no scripts/install-smoke.sh; it predates the install procedure"
smoke_results="$work/smoke-results.jsonl"
note "running install-smoke against the built binary"
"$smoke" --results "$smoke_results" --expect-build "$short_sha" "$built" >&2 || die "install-smoke failed for $short_sha; nothing was installed"
# The verdict comes from the per-check results, not the exit code alone: pass
# only when every check ran and passed; pass_with_skips when a skip was
# allowed. A smoke that reported nothing, or reported a failure yet exited 0,
# is not trusted.
smoke_verdict=$(python3 - "$smoke_results" <<'PY'
import json, sys
try:
    with open(sys.argv[1]) as handle:
        checks = [json.loads(line) for line in handle if line.strip()]
except (OSError, ValueError) as error:
    sys.exit(f"unreadable smoke results: {error}")
if not checks:
    sys.exit("the smoke reported no checks")
# The six checks install-smoke.sh runs. Each must report exactly once; an
# unknown id is refused; only porch may be skipped (and only when allowed).
expected = ["setup", "version", "build_id", "samples", "who_speed", "porch"]
skippable = {"porch"}
ids = [check.get("check") for check in checks]
for check_id in ids:
    if check_id not in expected:
        sys.exit(f"the smoke reported an unknown check {check_id!r}")
for check_id in expected:
    if ids.count(check_id) != 1:
        sys.exit(f"check {check_id!r} reported {ids.count(check_id)} times, expected exactly once")
for check in checks:
    result = check.get("result")
    if result == "pass":
        continue
    if result == "skipped" and check.get("allowed") is True and check["check"] in skippable:
        continue
    sys.exit(f"check {check.get('check')!r} reported {result!r} but the smoke exited 0")
print("pass_with_skips" if any(check["result"] == "skipped" for check in checks) else "pass")
PY
) || die "install-smoke for $short_sha exited 0 without an itemized pass; nothing was installed"
[ "$smoke_verdict" = pass ] || note "smoke verdict $smoke_verdict: an allowed check did not run"

# Served skill path against the manifest built into this binary. Exit 1 is
# drift or unverified; anything else nonzero means the path could not be
# checked.
manifest_json="$work/manifest.json"
manifest_rc=0
"$built" contract skill-manifest --verify "$served" > "$manifest_json" 2> "$work/manifest.err" || manifest_rc=$?
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
case "$manifest_verdict" in
  match) outcome_code=0 ;;
  drift)
    outcome_code=1
    note "skill drift at $served: $(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print({k: d[k] for k in ("mismatched","missing","extra") if d[k]})' "$manifest_json")"
    ;;
  unverified)
    outcome_code=4
    note "served copy at $served has rendered fenced files this binary cannot verify: $(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["rendered_unverified"])' "$manifest_json")"
    ;;
  *) outcome_code=4 ;;
esac

# The backup name: post-<old-build-sha>.bak when that name is free or already
# holds exactly the live bytes; else a content-addressed name. Sets
# backup/backup_keep, or stops before any write.
old_sha=""
backup=""
backup_keep=0
live_sha256=""
choose_backup() {
  live_sha256=$(sha256_of "$target")
  old_sha=$("$target" version --json 2>/dev/null | python3 -c 'import json,sys; print(json.load(sys.stdin).get("build_sha") or "")' 2>/dev/null || true)
  if [ -z "$old_sha" ] || [ "$old_sha" = unknown ]; then
    # An old post without version --json, or a build outside git.
    old_sha="sha256-${live_sha256:0:12}"
  fi
  local candidate
  for candidate in "$bin_dir/post-$old_sha.bak" "$bin_dir/post-$old_sha-${live_sha256:0:12}.bak"; do
    if [ ! -e "$candidate" ] && [ ! -L "$candidate" ]; then
      backup="$candidate"
      return 0
    fi
    if [ -f "$candidate" ] && [ ! -L "$candidate" ] && [ "$(sha256_of "$candidate")" = "$live_sha256" ]; then
      backup="$candidate"
      backup_keep=1
      return 0
    fi
    note "existing $candidate is not a copy of the live post (sha256 differs); not trusting it as the backup"
  done
  die "no usable backup name for $target: $bin_dir/post-$old_sha.bak and $bin_dir/post-$old_sha-${live_sha256:0:12}.bak both hold other bytes. Nothing was installed."
}
if [ -e "$target" ]; then
  choose_backup
fi

if [ "$dry_run" -eq 1 ]; then
  note "dry run: would install $short_sha (sha256 $binary_sha256) to $target"
  if [ -n "$backup" ]; then
    if [ "$backup_keep" -eq 1 ]; then note "dry run: backup $backup already holds the live post; would keep it"; else note "dry run: would back up the current post (sha256 $live_sha256) to $backup"; fi
  else
    note "dry run: no current post at $target; nothing to back up"
  fi
  note "dry run: smoke $smoke_verdict; served $served ($served_kind) manifest $manifest_verdict; would write $receipt; would exit $outcome_code"
  exit "$outcome_code"
fi

mkdir -p "$bin_dir" || die "could not create $bin_dir; nothing was installed"
refuse_unusable_target
if [ -n "$backup" ]; then
  if [ "$backup_keep" -eq 1 ]; then
    note "backup $backup already holds the live post; keeping it"
  else
    backup_tmp="$bin_dir/.post-backup.$$.tmp"
    cp -p "$target" "$backup_tmp" || die "could not copy $target to $backup_tmp; nothing was installed"
    [ "$(sha256_of "$backup_tmp")" = "$live_sha256" ] \
      || die "the backup copy of $target does not match it (the live file changed, or the copy is short); nothing was installed"
    mv -f "$backup_tmp" "$backup" || die "could not rename the backup to $backup; nothing was installed"
    backup_tmp=""
    note "backed up the current post to $backup"
  fi
  [ "$(sha256_of "$backup")" = "$live_sha256" ] || die "backup $backup does not hold the live post; nothing was installed"
fi

install -m 0755 "$built" "$target.new" || { rm -f "$target.new"; die "could not stage $target.new; nothing was installed"; }
fail_code=6
if ! mv -f "$target.new" "$target"; then
  rm -f "$target.new"
  [ -z "$live_sha256" ] || [ "$(sha256_of "$target")" = "$live_sha256" ] || die "mv over $target failed and $target no longer holds the pre-install bytes; restore it from $backup" 6
  die "could not move $target.new over $target; nothing was installed"
fi
installed_sha256=$(sha256_of "$target")
if [ "$installed_sha256" != "$binary_sha256" ]; then
  note "installed bytes at $target differ from the build (sha256 $installed_sha256, expected $binary_sha256); rolling back"
  if [ -n "$backup" ]; then
    restore="$target.restore"
    if cp -p "$backup" "$restore" && [ "$(sha256_of "$restore")" = "$live_sha256" ] &&
      mv -f "$restore" "$target" && [ "$(sha256_of "$target")" = "$live_sha256" ]; then
      die "rolled back: $target holds the pre-install post again (restored from $backup); nothing was installed" 5
    fi
    rm -f "$restore"
    die "ROLLBACK FAILED: $target may hold bad bytes. Restore it by hand: cp -p $backup $target (sha256 $live_sha256)" 6
  fi
  if rm -f "$target" && [ ! -e "$target" ]; then
    die "rolled back: removed the bad $target; there was no post before this install" 5
  fi
  die "ROLLBACK FAILED: could not remove the bad $target; remove it by hand" 6
fi
fail_code=7
note "installed $short_sha to $target"

backup_sha256=""
[ -z "$backup" ] || backup_sha256="$live_sha256"
write_receipt() {
  mkdir -p "$(dirname "$receipt")" || return 1
  COMMIT="$full_sha" BUILD_SHA="$built_sha" BINARY_SHA256="$binary_sha256" BIN_PATH="$target" \
  REACHABLE="$reachable" ORIGIN_BRANCHES="$origin_branches" \
  BACKUP="$backup" BACKUP_SHA256="$backup_sha256" SERVED="$served" SERVED_KIND="$served_kind" \
  MANIFEST_VERDICT="$manifest_verdict" SMOKE_VERDICT="$smoke_verdict" SMOKE_RESULTS="$smoke_results" \
  RECEIPT="$receipt" python3 - <<'PY'
import datetime, json, os
receipt = os.environ["RECEIPT"]
with open(os.environ["SMOKE_RESULTS"]) as handle:
    smoke_checks = [json.loads(line) for line in handle if line.strip()]
record = {
    "installed_at": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
    "commit": os.environ["COMMIT"],
    "build_sha": os.environ["BUILD_SHA"],
    "reachable": {"true": True, "false": False}.get(os.environ["REACHABLE"], os.environ["REACHABLE"]),
    "origin_branches": [name for name in os.environ["ORIGIN_BRANCHES"].split(",") if name],
    "binary_sha256": os.environ["BINARY_SHA256"],
    "bin_path": os.environ["BIN_PATH"],
    "backup": os.environ["BACKUP"] or None,
    "backup_sha256": os.environ["BACKUP_SHA256"] or None,
    "served_path": os.environ["SERVED"],
    "served_kind": os.environ["SERVED_KIND"],
    "manifest_verdict": os.environ["MANIFEST_VERDICT"],
    "smoke_verdict": os.environ["SMOKE_VERDICT"],
    "smoke_checks": smoke_checks,
}
tmp = receipt + ".tmp"
with open(tmp, "w") as handle:
    json.dump(record, handle, indent=2)
    handle.write("\n")
os.replace(tmp, receipt)
PY
}
write_receipt || die "installed and verified $short_sha at $target, but could not write the receipt $receipt (manifest $manifest_verdict, smoke $smoke_verdict)" 7
note "receipt: $receipt"

exit "$outcome_code"
