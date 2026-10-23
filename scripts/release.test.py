#!/usr/bin/env python3
"""Release preparation contract checks using local repositories and tiny archives."""
import argparse
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest import mock

sys.dont_write_bytecode = True

spec = importlib.util.spec_from_file_location('release', Path(__file__).with_name('release.py'))
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)
PROJECT_ROOT = Path(__file__).resolve().parents[1]


def git(root, *args):
    return subprocess.check_output(['git', '-C', str(root), *args], stderr=subprocess.DEVNULL)


class ReleaseTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='post-release-test-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.source = self.root / 'source'
        self.source.mkdir()
        git(self.source, 'init', '-q')
        git(self.source, 'config', 'user.name', 'Fixture')
        git(self.source, 'config', 'user.email', 'fixture@users.noreply.github.com')
        (self.source / 'README.md').write_text('public base\n')
        git(self.source, 'add', 'README.md')
        git(self.source, 'commit', '-qm', 'public base')
        git(self.source, 'branch', 'public-base')
        (self.source / 'scripts').mkdir()
        (self.source / 'scripts/release-public-paths.txt').write_text('README.md\nCargo.toml\nCHANGELOG.md\n')
        (self.source / 'Cargo.toml').write_text('[package]\nversion = "0.11.0"\n')
        (self.source / 'CHANGELOG.md').write_text('## 0.11.0 — 2026-10-23\n\nA public release.\n')
        (self.source / '.beads').mkdir()
        (self.source / '.beads/private.json').write_text('private fixture material\n')
        git(self.source, 'add', '.')
        git(self.source, '-c', 'user.email=private@example.test', 'commit', '-qm', 'private source')
        release.ROOT = self.source
        self.addCleanup(setattr, release, 'ROOT', Path(__file__).resolve().parents[1])
        self.destination = self.root / 'public'

    def export(self):
        release.prepare_public(argparse.Namespace(destination=str(self.destination), source_ref='HEAD',
            base_ref='public-base', public_name='Fixture', public_email='fixture@users.noreply.github.com'))
        release.ROOT = self.destination
        return self.destination / 'target/distrib'

    def assets(self):
        distrib = self.export()
        for name in release.ARCHIVES[1:]:
            with tarfile.open(distrib / name, 'w:xz') as archive:
                info = tarfile.TarInfo(name.removesuffix('.tar.xz') + '/post')
                info.mode = 0o755
                info.size = 4
                archive.addfile(info, io.BytesIO(b'post'))
            target = name.removeprefix('post-').removesuffix('.tar.xz')
            if target.endswith('apple-darwin'):
                (distrib / f'notarize-{target}.json').write_text(json.dumps(
                    {'id': f'fixture-{target}', 'status': 'Accepted'}))
            self.record_build(distrib, target)
        (distrib / 'post-installer.sh').write_text('case x in\n' + ''.join(
            f'        "{name}")\n            echo install\n            ;;\n'
            for name in release.ARCHIVES[1:]) + 'esac\n')
        return distrib

    def record_build(self, distrib, target):
        tools = self.root / 'signing-tools'
        tools.mkdir(exist_ok=True)
        signer = tools / 'codesign'
        signer.write_text('#!/bin/sh\n[ "$1" = --verify ] && [ "$2" = --strict ] || exit 2\n'
                          'cat "$3" > "$POST_TEST_CODESIGN_BINARY"\n'
                          'exit "${POST_TEST_CODESIGN_EXIT:-0}"\n')
        signer.chmod(0o755)
        env = {'PATH': f'{tools}{os.pathsep}{os.environ["PATH"]}',
               'POST_TEST_CODESIGN_BINARY': str(self.root / 'verified-binary')}
        with mock.patch.dict(os.environ, env), mock.patch.object(sys, 'platform', 'darwin'):
            release.record_build(argparse.Namespace(target=target, distrib=str(distrib)))

    def test_public_snapshot_has_no_private_history_or_omitted_files(self):
        distrib = self.export()
        self.assertFalse((self.destination / '.beads').exists())
        self.assertNotIn(b'private@example.test', git(self.destination, 'log', '--format=%ae'))
        self.assertEqual(git(self.source, 'rev-parse', 'public-base').strip(),
                         git(self.destination, 'rev-parse', 'HEAD^').strip())
        mapping = json.loads((distrib / 'source-map.json').read_text())
        self.assertIn('.beads/private.json', mapping['omitted'])
        self.assertEqual(mapping['source_commit'], git(self.source, 'rev-parse', 'HEAD').decode().strip())
        with tarfile.open(distrib / 'source.tar.gz') as archive:
            self.assertNotIn('post-0.11.0/.beads/private.json', archive.getnames())

    def test_public_snapshot_keeps_contract_build_inputs(self):
        (self.source / 'scripts/release-public-paths.txt').write_text(
            (PROJECT_ROOT / 'scripts/release-public-paths.txt').read_text())
        shutil.copytree(PROJECT_ROOT / 'contract', self.source / 'contract')
        git(self.source, 'add', 'scripts/release-public-paths.txt', 'contract')
        git(self.source, 'commit', '-qm', 'include contract samples')
        distrib = self.export()
        samples = sorted((self.source / 'contract').rglob('*'))
        self.assertTrue(any(path.is_file() for path in samples))
        with tarfile.open(distrib / 'source.tar.gz') as archive:
            names = set(archive.getnames())
        for path in samples:
            if path.is_file():
                relative = path.relative_to(self.source)
                self.assertEqual((self.destination / relative).read_bytes(), path.read_bytes())
                self.assertIn(f'post-0.11.0/{relative.as_posix()}', names)

    def test_existing_destination_and_non_noreply_identity_are_refused(self):
        self.destination.mkdir()
        args = argparse.Namespace(destination=str(self.destination), source_ref='HEAD',
            base_ref='public-base', public_name='Fixture', public_email='fixture@users.noreply.github.com')
        with self.assertRaisesRegex(ValueError, 'must not exist'):
            release.prepare_public(args)
        args.public_email = 'private@example.test'
        with self.assertRaisesRegex(ValueError, 'no-reply'):
            release.prepare_public(args)

    def test_assets_embed_all_hashes_and_reject_corruption(self):
        distrib = self.assets()
        args = argparse.Namespace(distrib=str(distrib))
        release.assets(args)
        manifest = json.loads((distrib / 'asset-manifest.json').read_text())
        self.assertEqual(len(manifest['assets']), 12)
        installer = (distrib / 'post-installer.sh').read_text()
        for name in release.ARCHIVES[1:]:
            self.assertIn(release.digest(distrib / name), installer)
        with (distrib / release.ARCHIVES[1]).open('ab') as file:
            file.write(b'corrupt')
        with self.assertRaisesRegex(ValueError, 'checksum mismatch'):
            release.verify(args)

    def test_installer_replaces_later_checksum_assignments(self):
        distrib = self.assets()
        path = distrib / 'post-installer.sh'
        path.write_text('case "$1" in\n' + ''.join(
            f'        "{name}")\n'
            f'            _arch="{name.removeprefix("post-").removesuffix(".tar.xz")}"\n'
            '            _zip_ext=".tar.xz"\n'
            '            _bins="post"\n'
            '            _checksum_style="sha256"\n'
            '            _checksum_value="stale"\n'
            '            ;;\n' for name in release.ARCHIVES[1:]) +
            'esac\nprintf "%s %s\\n" "$_checksum_style" "$_checksum_value"\n')
        release.assets(argparse.Namespace(distrib=str(distrib)))
        for name in release.ARCHIVES[1:]:
            result = subprocess.check_output(['bash', str(path), name], text=True).strip()
            self.assertEqual(result, f'sha256 {release.digest(distrib / name)}')

    def test_verify_refuses_a_second_checksum_pair_in_a_case(self):
        distrib = self.assets()
        args = argparse.Namespace(distrib=str(distrib))
        release.assets(args)
        path = distrib / 'post-installer.sh'
        path.write_text(path.read_text().replace('            ;;',
            '            _checksum_style="sha256"\n'
            '            _checksum_value="stale"\n            ;;', 1))
        manifest_path = distrib / 'asset-manifest.json'
        manifest = json.loads(manifest_path.read_text())
        manifest['assets']['post-installer.sh'] = release.digest(path)
        manifest_path.write_text(json.dumps(manifest))
        with self.assertRaisesRegex(ValueError, 'installer does not verify'):
            release.verify(args)

    def test_archive_requires_the_expected_target_binary_path(self):
        path = self.root / f'post-{release.TARGETS[0]}.tar.xz'
        with tarfile.open(path, 'w:xz') as archive:
            info = tarfile.TarInfo('different-target/post')
            info.mode = 0o755
            info.size = 4
            archive.addfile(info, io.BytesIO(b'post'))
        with self.assertRaisesRegex(ValueError, 'executable post|expected binary'):
            release.check_archive(path)

    def test_darwin_record_requires_accepted_notarization_and_signature(self):
        distrib = self.assets()
        target = release.TARGETS[0]
        path = distrib / f'notarize-{target}.json'
        path.unlink()
        with self.assertRaisesRegex((ValueError, OSError), 'notariz'):
            self.record_build(distrib, target)
        path.write_text(json.dumps({'status': 'Invalid'}))
        with self.assertRaisesRegex(ValueError, 'Accepted'):
            self.record_build(distrib, target)
        path.write_text(json.dumps({'status': 'Accepted'}))
        with mock.patch.dict(os.environ, {'POST_TEST_CODESIGN_EXIT': '1'}):
            with self.assertRaises(subprocess.CalledProcessError):
                self.record_build(distrib, target)
        self.record_build(distrib, target)
        self.assertEqual((self.root / 'verified-binary').read_bytes(), b'post')
        with mock.patch.object(sys, 'platform', 'linux'):
            with self.assertRaisesRegex(ValueError, 'Darwin signing host'):
                release.record_build(argparse.Namespace(target=target, distrib=str(distrib)))

    def test_verify_rechecks_build_and_notarization_receipts(self):
        distrib = self.assets()
        args = argparse.Namespace(distrib=str(distrib))
        release.assets(args)
        with mock.patch.object(sys, 'platform', 'linux'):
            release.verify(args)
        receipt_path = distrib / f'{release.ARCHIVES[1]}.build.json'
        original = json.loads(receipt_path.read_text())
        for key in ['target', 'binary_sha256']:
            with self.subTest(key=key):
                changed = dict(original, **{key: 'wrong'})
                receipt_path.write_text(json.dumps(changed))
                with self.assertRaisesRegex(ValueError, 'build receipt'):
                    release.verify(args)
        receipt_path.write_text(json.dumps(original))
        notary_path = distrib / f'notarize-{release.TARGETS[0]}.json'
        notary_path.write_text(json.dumps({'status': 'Invalid'}))
        with self.assertRaisesRegex(ValueError, 'Accepted|notariz'):
            release.verify(args)
        notary_path.write_text(json.dumps({'status': 'Accepted', 'id': 'another-submission'}))
        with self.assertRaisesRegex(ValueError, 'notarization receipt differs'):
            release.verify(args)

    def test_verify_refuses_a_new_head_after_assets_were_prepared(self):
        distrib = self.assets()
        args = argparse.Namespace(distrib=str(distrib))
        release.assets(args)
        (self.destination / 'README.md').write_text('changed after build\n')
        git(self.destination, 'add', 'README.md')
        git(self.destination, '-c', 'user.name=Fixture',
            '-c', 'user.email=fixture@users.noreply.github.com', 'commit', '-qm', 'later source')
        with self.assertRaisesRegex(ValueError, 'HEAD differs'):
            release.verify(args)

    def test_verify_refuses_wrong_tree_and_version_metadata(self):
        distrib = self.assets()
        args = argparse.Namespace(distrib=str(distrib))
        release.assets(args)
        path = distrib / 'asset-manifest.json'
        original = json.loads(path.read_text())
        for key, wrong in [('public_tree', '0' * 40), ('version', '999.0.0')]:
            with self.subTest(key=key):
                changed = json.loads(json.dumps(original))
                if key == 'version':
                    changed[key] = wrong
                else:
                    changed['source'][key] = wrong
                path.write_text(json.dumps(changed))
                with self.assertRaisesRegex(ValueError, 'prepared source|version'):
                    release.verify(args)
        path.write_text(json.dumps(original))

    def test_verify_binds_source_archive_to_prepared_source(self):
        distrib = self.assets()
        args = argparse.Namespace(distrib=str(distrib))
        release.assets(args)
        with tarfile.open(distrib / 'source.tar.gz', 'w:gz') as archive:
            info = tarfile.TarInfo('post-0.11.0/README.md')
            info.size = 5
            archive.addfile(info, io.BytesIO(b'wrong'))
        digest = release.digest(distrib / 'source.tar.gz')
        (distrib / 'source.tar.gz.sha256').write_text(f'{digest} *source.tar.gz\n')
        (distrib / 'sha256.sum').write_text(''.join(
            f'{release.digest(distrib / name)} *{name}\n' for name in release.ARCHIVES))
        path = distrib / 'asset-manifest.json'
        manifest = json.loads(path.read_text())
        for name in ['source.tar.gz', 'source.tar.gz.sha256', 'sha256.sum']:
            manifest['assets'][name] = release.digest(distrib / name)
        path.write_text(json.dumps(manifest))
        with self.assertRaisesRegex(ValueError, 'source.tar.gz differs'):
            release.verify(args)

    def test_untracked_hook_is_refused_but_build_outputs_are_allowed(self):
        distrib = self.assets()
        release.source_map(self.destination)
        (distrib / 'build.log').write_text('local build receipt\n')
        release.source_map(self.destination)
        hook = self.destination / 'skills/post/hooks/local.mjs'
        hook.parent.mkdir(parents=True)
        hook.write_text('export const local = true;\n')
        # Ignoring an extra hook does not stop build.rs from embedding it.
        (self.destination / '.git/info/exclude').write_text('skills/post/hooks/local.mjs\n')
        with self.assertRaisesRegex(ValueError, 'untracked'):
            release.source_map(self.destination)

    def test_installer_shell_preflight_keeps_prepared_source_clean(self):
        for name in ['release.py', 'release.sh']:
            shutil.copyfile(PROJECT_ROOT / 'scripts' / name, self.source / 'scripts' / name)
        with (self.source / 'scripts/release-public-paths.txt').open('a') as manifest:
            manifest.write('scripts/release.py\nscripts/release.sh\n')
        git(self.source, 'add', 'scripts')
        git(self.source, 'commit', '-qm', 'include release entrypoint')
        self.export()
        tools = self.root / 'tools'
        tools.mkdir()
        dist = tools / 'dist'
        dist.write_text('#!/bin/sh\nif [ "$1" = --version ]; then echo "cargo-dist 0.32.0"; '
                        'else echo "$*" >> "$POST_TEST_DIST_LOG"; fi\n')
        dist.chmod(0o755)
        log = self.root / 'dist.log'
        env = os.environ.copy()
        env.pop('PYTHONDONTWRITEBYTECODE', None)
        env.pop('PYTHONPYCACHEPREFIX', None)
        env.update(PATH=f'{tools}{os.pathsep}{env["PATH"]}', POST_TEST_DIST_LOG=str(log))
        result = subprocess.run(['bash', 'scripts/release.sh', 'installer'],
                                cwd=self.destination, env=env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('build --artifacts=global --tag v0.11.0', log.read_text())
        self.assertEqual(list(self.destination.rglob('__pycache__')), [])
        release.source_map(self.destination)

    def test_test_environment_does_not_write_import_cache(self):
        (self.root / 'probe.py').write_text('VALUE = 1\n')
        env = os.environ.copy()
        env.pop('PYTHONDONTWRITEBYTECODE', None)
        env.pop('PYTHONPYCACHEPREFIX', None)
        result = subprocess.run(['bash', '-c', 'source "$1"; python3 -c "import probe"',
                                 'test-env', str(PROJECT_ROOT / 'scripts/test-env.sh')],
                                cwd=self.root, env=env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((self.root / '__pycache__').exists())

    def test_release_flags_preserve_encoded_arguments_and_add_home_remap(self):
        home = str(self.root / 'builder home')
        remap = f'--remap-path-prefix={home}=/build'
        cases = [('--cfg\x1fname="two words"', 'ignored flags',
                  ['--cfg', 'name="two words"', remap]),
                 ('', 'ignored flags', [remap]),
                 (None, '-C opt-level=3', ['-C', 'opt-level=3', remap])]
        for encoded, flags, expected in cases:
            with self.subTest(encoded=encoded):
                env = os.environ.copy()
                env.update(HOME=home, RUSTFLAGS=flags)
                env.pop('CARGO_ENCODED_RUSTFLAGS', None)
                if encoded is not None:
                    env['CARGO_ENCODED_RUSTFLAGS'] = encoded
                result = subprocess.run(['bash', '-c',
                    'source "$1"; release_rustflags; printf %s "$CARGO_ENCODED_RUSTFLAGS"',
                    'release-flags', str(PROJECT_ROOT / 'scripts/release.sh')],
                    env=env, capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout.split('\x1f'), expected)

    def test_packaging_refuses_private_home_before_creating_output(self):
        home = str(self.root / 'builder home')
        binary = self.root / 'post'
        binary.write_bytes(b'prefix\0' + os.fsencode(home) + b'\0suffix')
        env = dict(os.environ, HOME=home)
        result = subprocess.run(['bash', '-c',
            'source "$1"; cd "$2"; package_binary x86_64-unknown-linux-musl "$3"',
            'release-privacy', str(PROJECT_ROOT / 'scripts/release.sh'),
            str(self.root), str(binary)], env=env, capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('builder home path', result.stderr)
        self.assertFalse((self.root / 'target').exists())

    def test_missing_installer_branch_is_refused(self):
        distrib = self.assets()
        (distrib / 'post-installer.sh').write_text('incomplete installer\n')
        with self.assertRaisesRegex(ValueError, 'archive branches'):
            release.assets(argparse.Namespace(distrib=str(distrib)))

    def test_unsafe_archive_and_wrong_source_are_refused(self):
        distrib = self.assets()
        receipt_path = distrib / f'{release.ARCHIVES[1]}.build.json'
        receipt = json.loads(receipt_path.read_text())
        receipt['source']['public_commit'] = '0' * 40
        receipt_path.write_text(json.dumps(receipt))
        with self.assertRaisesRegex(ValueError, 'build receipt differs'):
            release.assets(argparse.Namespace(distrib=str(distrib)))
        self.record_build(distrib, release.TARGETS[0])
        with tarfile.open(distrib / release.ARCHIVES[1], 'w:xz') as archive:
            info = tarfile.TarInfo('../post')
            info.mode = 0o755
            archive.addfile(info)
        with self.assertRaisesRegex(ValueError, 'unsafe archive'):
            release.assets(argparse.Namespace(distrib=str(distrib)))

    def test_retired_upload_does_not_invoke_tools(self):
        result = subprocess.run(['bash', str(Path(__file__).with_name('release.sh')), 'upload'],
                                capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('never publishes', result.stderr)


if __name__ == '__main__':
    unittest.main()
