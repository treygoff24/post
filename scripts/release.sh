#!/usr/bin/env bash
# Local release build + publish for post. Captured from the v0.7.0 ship
# (2026-08-30); see docs/RELEASING.md for the narrative and one-time setup.
#
# Releases are built and signed HERE, on the Mac, because Apple Developer ID
# signing and notarization cannot run on GitHub runners without exporting the
# signing key. dist only orchestrates the darwin builds and the installer;
# musl builds, signing, notarization, repacking, and the unified checksum
# file are this script's job (dist's local-mode global build writes a
# sha256.sum covering only the source tarball, and leaves the installer's
# embedded checksum slots empty -- sidecars serve manual verification).
#
#   scripts/release.sh build [--skip-gate]   everything local, no network writes
#   scripts/release.sh upload                GATED: pushes to GitHub + creates the
#                                            release. One explicit Trey
#                                            authorization per invocation.
set -Eeuo pipefail
cd "$(dirname "$0")/.."

SIGN_IDENTITY="${POST_SIGN_IDENTITY:-Developer ID Application: Lawrence Goff (LRU27MC63Q)}"
NOTARY_PROFILE="${POST_NOTARY_PROFILE:-post-notary}"
GITHUB_REPO="${POST_GITHUB_REPO:-treygoff24/post}"
DARWIN_TARGETS=(aarch64-apple-darwin x86_64-apple-darwin)
MUSL_TARGETS=(aarch64-unknown-linux-musl x86_64-unknown-linux-musl)
DISTRIB=target/distrib

die() { printf 'release: %s\n' "$*" >&2; exit 1; }
step() { printf '\n=== %s ===\n' "$*"; }

VERSION=$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[0].version')
TAG="v${VERSION}"

preflight() {
  step "preflight ($TAG)"
  for t in dist cargo-zigbuild zig jq gh shasum xcrun; do
    command -v "$t" >/dev/null || die "missing tool: $t (see docs/RELEASING.md)"
  done
  if ! git diff --quiet || ! git diff --cached --quiet; then
    die "working tree not clean"
  fi
  git rev-parse -q --verify "refs/tags/$TAG" >/dev/null \
    || die "tag $TAG does not exist (create it: git tag -a $TAG -m '$TAG')"
  [ "$(git rev-parse "$TAG^{commit}")" = "$(git rev-parse HEAD)" ] \
    || die "tag $TAG does not point at HEAD"
  security find-identity -v -p codesigning | grep -qF "$SIGN_IDENTITY" \
    || die "codesign identity not found: $SIGN_IDENTITY"
  for t in "${DARWIN_TARGETS[@]}" "${MUSL_TARGETS[@]}"; do
    rustup target list --installed | grep -qx "$t" \
      || die "rustup target missing: $t (rustup target add $t)"
  done
}

build() {
  local skip_gate=0
  [ "${1:-}" = "--skip-gate" ] && skip_gate=1

  preflight

  if [ "$skip_gate" -eq 0 ]; then
    step "gate"
    scripts/gate.sh || die "gate failed; fix or rerun with --skip-gate at your peril"
  fi

  step "darwin builds (dist local)"
  # Host mode on a Mac builds exactly the two darwin targets from
  # dist-workspace.toml, leaving unpacked dirs + tar.xz in target/distrib.
  dist build --artifacts=local

  step "sign + notarize + repack darwin"
  for t in "${DARWIN_TARGETS[@]}"; do
    local dir="$DISTRIB/post-$t" bin
    bin="$dir/post"
    [ -f "$bin" ] || die "expected dist output missing: $bin"
    codesign --force --options runtime --timestamp \
      --sign "$SIGN_IDENTITY" "$bin"
    codesign --verify --strict "$bin" || die "codesign verify failed: $bin"

    local zip="$DISTRIB/notarize-$t.zip"
    rm -f "$zip"
    ditto -c -k "$bin" "$zip"
    # The ticket lives with Apple; standalone executables cannot be stapled.
    xcrun notarytool submit "$zip" --keychain-profile "$NOTARY_PROFILE" \
      --wait --output-format json > "$DISTRIB/notarize-$t.json"
    local status
    status=$(jq -r '.status' "$DISTRIB/notarize-$t.json")
    [ "$status" = "Accepted" ] \
      || die "notarization not Accepted for $t: $status (see $DISTRIB/notarize-$t.json)"
    rm -f "$zip"

    # Repack the tarball so it carries the signed binary (dist tarred the
    # pre-signing one), preserving the post-<target>/ layout.
    tar -C "$DISTRIB" -cJf "$DISTRIB/post-$t.tar.xz" "post-$t"
  done

  step "musl builds (cargo-zigbuild, fully static)"
  for t in "${MUSL_TARGETS[@]}"; do
    cargo zigbuild --profile dist --target "$t"
    local dir="$DISTRIB/post-$t"
    rm -rf "$dir"
    mkdir -p "$dir"
    cp "target/$t/dist/post" "$dir/post"
    cp LICENSE CHANGELOG.md README.md "$dir/"
    file "$dir/post" | grep -q 'statically linked' \
      || die "musl binary for $t is not statically linked"
    tar -C "$DISTRIB" -cJf "$DISTRIB/post-$t.tar.xz" "post-$t"
  done

  step "installer + source tarball (dist global)"
  dist build --artifacts=global

  step "checksums"
  (
    cd "$DISTRIB"
    : > sha256.sum
    for f in source.tar.gz \
             post-aarch64-apple-darwin.tar.xz post-x86_64-apple-darwin.tar.xz \
             post-aarch64-unknown-linux-musl.tar.xz post-x86_64-unknown-linux-musl.tar.xz; do
      [ -f "$f" ] || { printf 'release: missing artifact %s\n' "$f" >&2; exit 1; }
      shasum -a 256 -b "$f" > "$f.sha256"
      cat "$f.sha256" >> sha256.sum
    done
    shasum -a 256 -c -- *.sha256
  )

  step "build complete"
  ls -l "$DISTRIB"/*.tar.xz "$DISTRIB"/*.tar.gz "$DISTRIB"/post-installer.sh
  printf '\nNext (GITHUB-GATED -- needs explicit Trey authorization):\n'
  printf '  scripts/release.sh upload\n'
}

upload() {
  preflight
  for t in "${DARWIN_TARGETS[@]}" "${MUSL_TARGETS[@]}"; do
    [ -f "$DISTRIB/post-$t.tar.xz" ] || die "run 'build' first: missing post-$t.tar.xz"
  done
  printf 'GITHUB-GATED ACTION: pushing %s + main and creating release %s on %s.\n' \
    "$TAG" "$TAG" "$GITHUB_REPO"

  step "push"
  git push github main "$TAG"

  step "release"
  # Notes = this version's CHANGELOG section (between its heading and the next).
  awk -v v="$VERSION" '$0 ~ "^## "v" " {p=1; next} /^## / {p=0} p' CHANGELOG.md \
    > "$DISTRIB/release-notes.md"
  [ -s "$DISTRIB/release-notes.md" ] || die "no CHANGELOG section found for $VERSION"
  if ! gh release view "$TAG" --repo "$GITHUB_REPO" >/dev/null 2>&1; then
    gh release create "$TAG" --repo "$GITHUB_REPO" --title "$TAG" \
      --notes-file "$DISTRIB/release-notes.md"
  fi
  (
    cd "$DISTRIB"
    gh release upload "$TAG" --repo "$GITHUB_REPO" --clobber \
      post-aarch64-apple-darwin.tar.xz post-aarch64-apple-darwin.tar.xz.sha256 \
      post-x86_64-apple-darwin.tar.xz post-x86_64-apple-darwin.tar.xz.sha256 \
      post-aarch64-unknown-linux-musl.tar.xz post-aarch64-unknown-linux-musl.tar.xz.sha256 \
      post-x86_64-unknown-linux-musl.tar.xz post-x86_64-unknown-linux-musl.tar.xz.sha256 \
      source.tar.gz source.tar.gz.sha256 sha256.sum post-installer.sh
  )

  step "verify from the live URL"
  local tmp
  tmp=$(mktemp -d)
  curl -fsSL "https://github.com/$GITHUB_REPO/releases/download/$TAG/post-installer.sh" \
    -o "$tmp/post-installer.sh"
  grep -q "$VERSION" "$tmp/post-installer.sh" \
    || die "live installer does not mention $VERSION"
  rm -rf "$tmp"
  printf '\nrelease %s is live: https://github.com/%s/releases/tag/%s\n' \
    "$TAG" "$GITHUB_REPO" "$TAG"
}

case "${1:-}" in
  check)  preflight; printf 'preflight ok for %s\n' "$TAG" ;;
  build)  shift; build "$@" ;;
  upload) upload ;;
  *) die "usage: scripts/release.sh check | build [--skip-gate] | upload" ;;
esac
