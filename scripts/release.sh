#!/usr/bin/env bash
# Local release preparation only. Review the export and run the gate separately.
set -Eeuo pipefail
cd "$(dirname "$0")/.."

die() { printf 'release: %s\n' "$*" >&2; exit 1; }
version() { python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml", "rb"))["package"]["version"])'; }
prepared_source() {
  python3 -B - <<'PY'
import importlib.util
from pathlib import Path
spec = importlib.util.spec_from_file_location('release', 'scripts/release.py')
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)
release.source_map(Path.cwd())
PY
}
dist_check() {
  command -v dist >/dev/null || die 'cargo-dist 0.32.0 is required'
  [[ "$(dist --version)" == *'0.32.0'* ]] || die 'use the pinned cargo-dist 0.32.0'
}
release_rustflags() {
  CARGO_ENCODED_RUSTFLAGS=$(python3 - <<'PYFLAGS'
import os, sys
home = os.environ.get('HOME', '')
if not os.path.isabs(home) or home == '/' or '\x1f' in home:
    raise SystemExit('release: HOME must be a non-root absolute path')
if 'CARGO_ENCODED_RUSTFLAGS' in os.environ:
    encoded = os.environ['CARGO_ENCODED_RUSTFLAGS']
    flags = encoded.split('\x1f') if encoded else []
else:
    flags = os.environ.get('RUSTFLAGS', '').split()
flags.append(f'--remap-path-prefix={home}=/build')
sys.stdout.write('\x1f'.join(flags))
PYFLAGS
)
  export CARGO_ENCODED_RUSTFLAGS
}
check_binary_privacy() {
  python3 - "$1" "$HOME" <<'PYPRIVACY'
import os, pathlib, sys
if os.fsencode(sys.argv[2]) in pathlib.Path(sys.argv[1]).read_bytes():
    raise SystemExit('release: binary contains the builder home path; refusing to sign or archive')
PYPRIVACY
}
package_binary() {
  local target="$1" binary="$2" out="target/distrib/post-$1"
  check_binary_privacy "$binary"
  [ ! -e "$out" ] || die "$out exists; preserve or move it before rebuilding"
  mkdir -p "$out"
  cp "$binary" "$out/post"
  cp LICENSE CHANGELOG.md README.md "$out/"
  COPYFILE_DISABLE=1 tar -C target/distrib -cJf "target/distrib/post-$target.tar.xz" "post-$target"
}
build_linux() {
  [ "$(uname -s)" = Linux ] || die 'Linux release builds run on the Linux workhorse'
  prepared_source
  release_rustflags
  command -v cargo-zigbuild >/dev/null || die 'cargo-zigbuild is required'
  for target in aarch64-unknown-linux-musl x86_64-unknown-linux-musl; do
    local messages binary
    messages=$(mktemp "${TMPDIR:-/tmp}/post-release-build.XXXXXX")
    CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}" cargo zigbuild --locked --profile dist --target "$target" --message-format=json-render-diagnostics > "$messages"
    binary=$(python3 - "$messages" <<'PYBUILD'
import json, sys
binary = None
for line in open(sys.argv[1]):
    try:
        item = json.loads(line)
    except ValueError:
        continue
    if item.get('reason') == 'compiler-artifact' and item.get('target', {}).get('name') == 'post' and item.get('executable'):
        binary = item['executable']
if not binary:
    raise SystemExit('release build returned no post binary')
print(binary)
PYBUILD
)
    [ -x "$binary" ] || die "reported binary is missing: $binary"
    local binary_kind
    binary_kind=$(file "$binary")
    printf '%s\n' "$binary_kind" | grep -Eq 'statically linked|static-pie linked' || die "$target is not statically linked"
    case "$target" in
      aarch64-*) printf '%s\n' "$binary_kind" | grep -Eq 'ARM aarch64|aarch64' || die "$target has the wrong architecture" ;;
      x86_64-*) printf '%s\n' "$binary_kind" | grep -q 'x86-64' || die "$target has the wrong architecture" ;;
    esac
    package_binary "$target" "$binary"
    python3 scripts/release.py record-build "$target"
  done
}
build_darwin() {
  [ "$(uname -s)" = Darwin ] || die 'Darwin signing runs on macOS'
  prepared_source
  release_rustflags
  dist_check
  [ -n "${POST_SIGN_IDENTITY:-}" ] || die 'set POST_SIGN_IDENTITY to the authorized Developer ID identity'
  [ -n "${POST_NOTARY_PROFILE:-}" ] || die 'set POST_NOTARY_PROFILE to the authorized notarytool profile'
  for target in aarch64-apple-darwin x86_64-apple-darwin; do
    local out="target/distrib/post-$target"
    [ ! -e "$out" ] || die "$out exists; preserve or move it before rebuilding"
    dist build --artifacts=local --target "$target" --tag "v$(version)"
    local binary="$out/post" receipt="target/distrib/notarize-$target.json"
    /usr/bin/strip -S "$binary"
    check_binary_privacy "$binary"
    codesign --options runtime --timestamp --sign "$POST_SIGN_IDENTITY" "$binary"
    codesign --verify --strict "$binary"
    local zip
    zip=$(mktemp "${TMPDIR:-/tmp}/post-notarize.XXXXXX.zip")
    ditto -c -k "$binary" "$zip"
    xcrun notarytool submit "$zip" --keychain-profile "$POST_NOTARY_PROFILE" --wait --output-format json > "$receipt"
    python3 - "$receipt" <<'PY'
import json, sys
if json.load(open(sys.argv[1])).get('status') != 'Accepted':
    raise SystemExit('notarization not Accepted; inspect the saved receipt')
PY
    # Retain the notarization zip and receipt for review; standalone binaries cannot be stapled.
    COPYFILE_DISABLE=1 tar -C target/distrib -cJf "target/distrib/post-$target.tar.xz" "post-$target"
    python3 scripts/release.py record-build "$target"
  done
}
if [[ "${BASH_SOURCE[0]}" != "$0" ]]; then return 0; fi

case "${1:-}" in
  prepare-public|prepare-assets|verify) exec python3 scripts/release.py "$@" ;;
  build-linux) build_linux ;;
  build-darwin) build_darwin ;;
  installer)
    prepared_source
    dist_check
    dist build --artifacts=global --tag "v$(version)"
    git archive --format=tar.gz --prefix="post-$(version)/" -o target/distrib/source.tar.gz HEAD
    ;;
  build|upload|publish)
    die 'the old build/upload flow is retired; see docs/RELEASING.md. This script never publishes.' ;;
  *)
    printf '%s\n' 'Usage: scripts/release.sh prepare-public DEST --public-name NAME --public-email EMAIL' \
      '       scripts/release.sh build-linux | build-darwin | installer' \
      '       scripts/release.sh prepare-assets [DISTRIB] | verify [DISTRIB]'
    ;;
esac
