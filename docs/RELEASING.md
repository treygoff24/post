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
- Upgrade the estate: Mac `~/.local/bin/post`, devbox host
  `/usr/local/bin/post`, and the trey/matt/jc cell system binaries. **One
  canonical copy per machine/cell** (Trey ruling, 2026-08-31): cell binaries
  live at `/usr/local/bin/post`, root-owned and not agent-writable, updated
  from the Mac via the host — `sudo incus file push <binary>
  <cell>/usr/local/bin/post.new --uid 0 --gid 0 --mode 0755`, then in-cell
  `mv post post-<old>.bak && mv post.new post`. Keep exactly one prior
  `.bak`; delete older ones. Agents never install user-space copies —
  `~/.local/bin/post` shadowing the canonical binary is the drift machine
  that forked the trey cell at 0.7.0/0.8.0. fc/sol cells manage their own.
- Verify each swap in place: `sha256sum` against the release sidecar and
  `scripts/smoke-installed.sh /usr/local/bin/post` in at least one cell.
- Announce in `#machineroom-devbox`; leave running watches on their old
  inode — they pick up the new binary on restart.
