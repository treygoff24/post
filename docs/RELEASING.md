# Releasing post

Releases are built, signed, and notarized **locally on the Mac** — Apple
Developer ID signing cannot run on GitHub runners without exporting the
signing key, so there is deliberately no CI release pipeline. dist
(cargo-dist 0.32, config in `dist-workspace.toml`) orchestrates the darwin
builds, the shell installer, and the source tarball; `scripts/release.sh`
does everything else. Captured from the v0.7.0 ship (2026-08-30).

## The flow

```sh
# 1. CHANGELOG has a "## X.Y.Z — date" section; Cargo.toml version bumped;
#    everything committed.
git tag -a vX.Y.Z -m vX.Y.Z

# 2. Build everything locally (runs scripts/gate.sh first):
scripts/release.sh build

# 3. GITHUB-GATED — needs an explicit Trey authorization, every time:
scripts/release.sh upload
```

`build` produces in `target/distrib/`: four tarballs (darwin arm64/x86_64
signed with hardened runtime + timestamp and notarized; musl arm64/x86_64
fully static via cargo-zigbuild), per-artifact `.sha256` sidecars, a unified
`sha256.sum`, `post-installer.sh`, and `source.tar.gz`. It touches nothing
off-machine except the notarization submission to Apple.

`upload` pushes `main` + the tag to the `github` remote, creates the release
with the version's CHANGELOG section as notes, uploads all twelve assets,
and verifies the installer from the live URL.

## One-time setup

- **Signing**: a `Developer ID Application` certificate in the login
  keychain. `security find-identity -v -p codesigning` must list it; the
  identity string is pinned in `release.sh` (override:
  `POST_SIGN_IDENTITY`).
- **Notarization**: `xcrun notarytool store-credentials post-notary
  --apple-id <apple-id> --team-id LRU27MC63Q` with an app-specific password.
  Profile name override: `POST_NOTARY_PROFILE`.
- **Tools**: `brew install cargo-dist cargo-zigbuild zig`; `rustup target
  add x86_64-apple-darwin aarch64-unknown-linux-musl
  x86_64-unknown-linux-musl`. `gh` authenticated for the repo.

## Why the script does what dist won't

- dist tars the darwin binaries **before** we sign, so the script re-signs
  the unpacked `target/distrib/post-<target>/post` and repacks the tarball.
- Standalone executables cannot be stapled; the notarization ticket lives
  with Apple, and Gatekeeper fetches it online. `Accepted` status is the
  proof, saved as `notarize-<target>.json`.
- dist cannot cross-build musl from macOS; `cargo zigbuild --profile dist`
  produces fully static binaries (pure-Rust dep tree — the script asserts
  `statically linked`).
- dist's local-mode `--artifacts=global` writes a `sha256.sum` covering only
  the source tarball, and leaves the installer's embedded checksum slots
  empty (no runtime verification — the sidecars serve manual checks). The
  script regenerates `sha256.sum` over every artifact.

## After the release

- Smoke the installed binary: `scripts/smoke-installed.sh /path/to/post`
  runs a live end-to-end pass (doctor bootstrap, watch semantics, digest
  fencepost) against a throwaway mail root — safe to run anywhere.
- **Where post lives is free; who can update it is not** (Trey ruling,
  2026-09-29, superseding the 2026-08-31 one-root-owned-copy rule): any
  install location is fine so long as an agent on the devbox, without sudo,
  can install and update post.
- Mac and the trey cell: `scripts/install-post.sh <full-sha>` on each host
  installs to `~/.local/bin/post`. It builds the commit in a temporary
  worktree, refuses a commit no Forgejo branch contains or a build whose
  skill manifest disagrees with its source, smokes the binary, keeps a
  `.bak`, and writes a receipt to `~/.local/share/post/install-receipt.json`.
  `post --version` names the build and `post doctor` lists other post
  binaries on PATH, which is what now guards against the drift that forked
  the trey cell at 0.7.0/0.8.0. The trey cell's `/usr/local/bin/post` is a
  stale Aug 31 build, shadowed on PATH; the bridge unit hardcodes
  `~/.local/bin/post`.
- Reinstall the bridge on the same commit: `bridge/install.sh` on the
  devbox (arguments in `bridge/README.md`, values from the running
  `post-bridge` unit's environment); on the Mac, copy `bridge/sweep.py`,
  `bridge/bridgelib/*.py` and a `BUILD` file (`commit=<sha>`, `dirty=no`)
  into a temp dir, swap it over `~/.local/lib/post-bridge`, and run
  `post-bridge-sweep --check-config` with the LaunchAgent's environment.
- The other cells (matt/jc/fc/sol) still run the root-owned
  `/usr/local/bin/post`, updated from the Mac via the host (`sudo incus
  file push <binary> <cell>/usr/local/bin/post.new --uid 0 --gid 0 --mode
  0755`, then in-cell `mv post post-<old>.bak && mv post.new post`). That
  layout does not yet meet the no-sudo rule; moving them is open work. In
  fc/sol, `~/.local/bin/post` is a symlink to the canonical copy (their
  systemd units and hooks hardcode that path).
- Verify each install: `scripts/smoke-installed.sh <path-to-post>` on at
  least one devbox cell, and `sha256sum` against the release sidecar for
  release binaries.
- Announce in `#machineroom-devbox`; leave running watches on their old
  inode — they pick up the new binary on restart.
