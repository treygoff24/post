# Releasing Post

Prepare and verify a public source snapshot before publishing. The release
helpers build and check local artifacts; they do not push commits, create a
GitHub release or upload files. Publishing is a separate maintainer decision.

A release consists of four binary archives (macOS and Linux, each arm64 and
x86_64), their checksums, a source archive, and an installer that verifies the
checksums of the final archives. Keep the mapping from reviewed source to
those binaries with the release evidence.

## Prepare the source

Commit the intended changes and finish the version and changelog updates.
The crate version, lockfile, bridge compatibility window and release notes
must agree. Read the bridge's [upgrade order](../bridge/README.md#compatibility-and-upgrades)
when changing the Post version.

The examples use a remote named `github` for the public repository. Check
`git remote -v`; if that remote is absent, add it with
`git remote add github https://github.com/treygoff24/post.git`. If your public
remote already has another name, substitute it in both the fetch command
and the baseline ref below.

Refresh the public baseline, then create a new public checkout. Substitute
an unused absolute destination and a public author identity:

```sh
git fetch github main
scripts/release.sh prepare-public /absolute/path/to/public-post \
  --source-ref HEAD --base-ref refs/remotes/github/main \
  --public-name 'Public author' --public-email '12345+author@users.noreply.github.com'
```

`scripts/release-public-paths.txt` is the export manifest. Review it and the
resulting tree: include required source, tests, documentation, hooks and build
files; exclude local ledgers, operational records and private helpers. Inspect
commit and tag metadata as well as file contents. A clean source scan does
not establish that history or author metadata is suitable for publication.

Run the integrated gate on that exact public source revision, on the build
workhorse. Fix findings in the source and prepare a new snapshot before
building if anything changes. Retain gate output and source mapping outside
the public package.

## Build on the appropriate machines

Use the prepared public checkout for both platforms. Builds require Python
3.11 or newer, the locked Rust toolchain/dependencies, cargo-dist 0.32.0 and the tools named by
the helpers. Linux builds use cargo-zigbuild and Zig for the static musl
targets. macOS builds require explicit `POST_SIGN_IDENTITY` and
`POST_NOTARY_PROFILE` environment variables for the release; keep their credentials in the trusted signing
machine's keychain.

On Linux:

```sh
scripts/release.sh build-linux
```

On the signing Mac, from the same reviewed source revision:

```sh
scripts/release.sh build-darwin
```

macOS archives must contain the signed binaries, with hardened runtime and
successful notarization. Signing changes bytes: calculate final checksums
only after signing and repacking. Keep notarization responses and verify
signatures locally. Linux archives must contain binaries for the stated
architecture with the expected static linkage.

The build helpers remap the builder's home path to `/build` and refuse a
binary that still contains the original path.

## Assemble and verify

Collect all four final archives in the release distribution directory. Keep
the source identity with each build; do not combine outputs from different
source trees or versions. Each binary archive has a matching
`post-<target>.tar.xz.build.json` receipt; transfer those receipts with the
archives. Include `notarize-<target>.json` for each macOS archive, with its
`Accepted` response. Darwin build receipts are written on the signing Mac
after signature verification. Preserve `target/distrib/source-map.json` too
when assembling on a different host. Asset preparation checks the source,
target, archive and binary hashes, and the notarization receipts. Then run:

```sh
scripts/release.sh installer
scripts/release.sh prepare-assets target/distrib
scripts/release.sh verify target/distrib
```

The installer step uses pinned cargo-dist 0.32.0 and regenerates the source
archive without compiling. Asset preparation embeds the final binary archive
checksums and writes `asset-manifest.json` for the twelve upload files,
`release-notes.md` and `source-map.json`. The snapshot also retains its source
mapping in `.git/post-release-source.json`; review the included and omitted
paths. Keep mapping and build evidence outside the upload set unless you
have reviewed them for publication. Verification must reject a damaged archive and a
missing or mismatched installer checksum. Checksums establish byte integrity;
the recorded source mapping and build evidence establish what was built.

Inspect archive entries before extracting. Test clean installation and an
upgrade in disposable locations on supported platforms, then run
`scripts/smoke-installed.sh /path/to/post` against the installed binary. The
smoke uses a temporary mail root. Verify the packaged skill manifest and
hook files, not just the binary version. Bridge tests need both a clean
installation and an old/new overlap in the documented upgrade order.

Check the complete artifact set and release notes before requesting
publication approval. The old `build` and `upload` shortcuts are disabled;
there is no automatic publish step.

## Publish after approval

Push only the reviewed public source and tag, using the repository's normal
release authorization. Keep private development history out of that push.
Create a draft GitHub release, upload the complete verified artifact set,
then download it again into a fresh directory and verify every checksum.
Do not overwrite artifacts belonging to an already-published release.

Publish the draft only after all files, notes and source references agree.
Verify the public release's installer and all download links again. Retain
the local verification evidence and source mapping; document any platform or
runtime check that could not be completed instead of treating a build as an
installation test.

## Update running installations

Update the bridge first on each host, then the Post binary, then the hook
adapters from the same source revision. Run `post doctor --brief`, check the
bridge health and test delivery both ways. Restart owned long-running
processes deliberately; replacing a binary does not change a running
process's code. Keep rollback copies until the new installation is verified.
