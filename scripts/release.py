#!/usr/bin/env python3
"""Prepare a reviewed public source snapshot and checksum-verified release assets.

All operations are local. This helper never pushes, uploads, tags or publishes.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys
import tarfile
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[1]
TARGETS = ('aarch64-apple-darwin', 'x86_64-apple-darwin',
           'aarch64-unknown-linux-musl', 'x86_64-unknown-linux-musl')
ARCHIVES = ['source.tar.gz', *(f'post-{target}.tar.xz' for target in TARGETS)]


def git(root, *args, **kwargs):
    return subprocess.check_output(['git', '-C', str(root), *args], **kwargs)


def digest(path):
    with path.open('rb') as source:
        return hashlib.file_digest(source, 'sha256').hexdigest()


def version(root):
    return tomllib.loads((root / 'Cargo.toml').read_text())['package']['version']


def source_map(root):
    path = Path(git(root, 'rev-parse', '--git-path', 'post-release-source.json').decode().strip())
    if not path.is_absolute():
        path = root / path
    if not path.exists():
        path = root / 'target/distrib/source-map.json'
    data = json.loads(path.read_text())
    if git(root, 'rev-parse', 'HEAD').decode().strip() != data['public_commit']:
        raise ValueError('HEAD differs from the prepared public snapshot')
    if git(root, 'rev-parse', 'HEAD^{tree}').decode().strip() != data['public_tree']:
        raise ValueError('tree differs from the prepared public snapshot')
    if version(root) != data.get('version'):
        raise ValueError('version differs from the prepared public snapshot')
    if git(root, 'status', '--porcelain', '--untracked-files=no').strip():
        raise ValueError('prepared public source has tracked changes')
    # Build.rs embeds files discovered under skills/, including ignored files.
    # Only Cargo's output directory may contain untracked build products.
    untracked = git(root, 'ls-files', '--others', '--directory', '-z').split(b'\0')
    if any(path and not path.startswith(b'target/') for path in untracked):
        raise ValueError('prepared public source has untracked inputs outside target/')
    return data


def source_archive_digest(root):
    archive = git(root, 'archive', '--format=tar.gz',
                  f'--prefix=post-{version(root)}/', 'HEAD')
    return hashlib.sha256(archive).hexdigest()


def prepare_public(args):
    source = ROOT
    commit = git(source, 'rev-parse', f'{args.source_ref}^{{commit}}').decode().strip()
    base = git(source, 'rev-parse', f'{args.base_ref}^{{commit}}').decode().strip()
    if not re.fullmatch(r'[^\s@]+@users\.noreply\.github\.com', args.public_email):
        raise ValueError('--public-email must be a GitHub no-reply address')
    destination = Path(args.destination).expanduser().absolute()
    if destination.exists():
        raise ValueError('destination must not exist; existing files are never replaced')
    manifest = git(source, 'show', f'{commit}:scripts/release-public-paths.txt').decode()
    allowed = [line.strip() for line in manifest.splitlines()
               if line.strip() and not line.lstrip().startswith('#')]
    entries = git(source, 'ls-tree', '-rz', '--full-tree', commit).split(b'\0')
    selected, omitted = [], []
    for entry in filter(None, entries):
        meta, name = entry.split(b'\t', 1)
        name = name.decode()
        if any(name == item or (item.endswith('/') and name.startswith(item)) for item in allowed):
            mode = meta.split()[0]
            if mode not in (b'100644', b'100755'):
                raise ValueError(f'public export needs review of non-regular entry: {name}')
            selected.append(name)
        else:
            omitted.append(name)
    if not selected:
        raise ValueError('empty public export')
    destination.mkdir(parents=True)
    subprocess.run(['git', 'init', '-q', str(destination)], check=True)
    # Fetch only public ancestry. Private source objects are never copied to this repository.
    subprocess.run(['git', '-C', str(destination), 'fetch', '-q', str(source), base], check=True)
    subprocess.run(['git', '-C', str(destination), 'checkout', '-q', '-b', 'main', 'FETCH_HEAD'], check=True)
    # Start with an empty index/tree, so files removed from the public manifest disappear too.
    subprocess.run(['git', '-C', str(destination), 'rm', '-qr', '--ignore-unmatch', '.'], check=True)
    for name in selected:
        path = destination / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(git(source, 'show', f'{commit}:{name}'))
        mode = git(source, 'ls-tree', commit, '--', name).split()[0]
        path.chmod(0o755 if mode == b'100755' else 0o644)
    subprocess.run(['git', '-C', str(destination), 'add', '--', *selected], check=True)
    env = os.environ.copy()
    env.update(GIT_AUTHOR_NAME=args.public_name, GIT_COMMITTER_NAME=args.public_name,
               GIT_AUTHOR_EMAIL=args.public_email, GIT_COMMITTER_EMAIL=args.public_email)
    subprocess.run(['git', '-C', str(destination), 'commit', '-q', '-m', f'Release Post {version(destination)}'], check=True, env=env)
    mapping = {'source_commit': commit, 'public_base': base,
               'public_commit': git(destination, 'rev-parse', 'HEAD').decode().strip(),
               'public_tree': git(destination, 'rev-parse', 'HEAD^{tree}').decode().strip(),
               'version': version(destination),
               'included': selected, 'omitted': omitted}
    (destination / '.git/post-release-source.json').write_text(json.dumps(mapping, indent=2) + '\n')
    distrib = destination / 'target/distrib'
    distrib.mkdir(parents=True)
    subprocess.run(['git', '-C', str(destination), 'archive', '--format=tar.gz',
                    f'--prefix=post-{version(destination)}/', '-o', str(distrib / 'source.tar.gz'), 'HEAD'], check=True)
    (distrib / 'source-map.json').write_text(json.dumps(mapping, indent=2) + '\n')
    print(f'Prepared {destination}; review source-map.json, especially omitted paths, then scan and gate the export.')


def check_archive(path):
    with tarfile.open(path) as archive:
        entries = archive.getmembers()
        for entry in entries:
            relative = PurePosixPath(entry.name)
            if relative.is_absolute() or '..' in relative.parts or not (entry.isfile() or entry.isdir()):
                raise ValueError(f'unsafe archive entry in {path.name}: {entry.name}')
        if path.name != 'source.tar.gz':
            expected = f'{path.name.removesuffix(".tar.xz")}/post'
            binaries = [entry for entry in entries if PurePosixPath(entry.name).name == 'post']
            if (len(binaries) != 1 or binaries[0].name != expected
                    or not binaries[0].isfile() or not binaries[0].mode & 0o111):
                raise ValueError(f'{path.name} must contain one executable post at {expected}')
            with archive.extractfile(binaries[0]) as binary:
                return hashlib.file_digest(binary, 'sha256').hexdigest()


def notarization(distrib, target):
    path = distrib / f'notarize-{target}.json'
    receipt = json.loads(path.read_text())
    if receipt.get('status') != 'Accepted':
        raise ValueError(f'{path.name} must have status Accepted')
    return {'file': path.name, 'sha256': digest(path), 'status': 'Accepted'}


def verify_darwin_signature(archive_path):
    if sys.platform != 'darwin':
        raise ValueError('Darwin build receipts must be recorded on the Darwin signing host')
    with tempfile.TemporaryDirectory(prefix='post-signature-') as directory:
        binary = Path(directory) / 'post'
        with tarfile.open(archive_path) as archive:
            source = archive.extractfile(f'{archive_path.name.removesuffix(".tar.xz")}/post')
            with source, binary.open('wb') as destination:
                while chunk := source.read(1024 * 1024):
                    destination.write(chunk)
        binary.chmod(0o755)
        subprocess.run(['codesign', '--verify', '--strict', str(binary)], check=True)


def verify_build_receipt(distrib, name, mapping, archive_hash, binary_hash):
    target = name.removeprefix('post-').removesuffix('.tar.xz')
    receipt = json.loads((distrib / f'{name}.build.json').read_text())
    expected = {'target': target, 'source': mapping,
                'archive_sha256': archive_hash, 'binary_sha256': binary_hash}
    if any(receipt.get(key) != value for key, value in expected.items()):
        raise ValueError(f'{name} build receipt differs from the prepared source, target or archive')
    if target.endswith('apple-darwin') and receipt.get('notarization') != notarization(distrib, target):
        raise ValueError(f'{name} notarization receipt differs from the recorded build')


def installer_case(installer, name):
    pattern = (rf'^[ \t]*"{re.escape(name)}"\)[ \t]*\n'
               r'(?P<body>.*?)(?P<end>^[ \t]*;;[ \t]*$)')
    matches = list(re.finditer(pattern, installer, re.M | re.S))
    if len(matches) != 1:
        raise ValueError(f'installer has {len(matches)} archive branches for {name}; expected one')
    return matches[0]


def installer_checksum(installer, name, checksum):
    case = installer_case(installer, name)
    body = re.sub(r'^[ \t]*_checksum_(?:style|value)=.*(?:\n|$)', '', case['body'], flags=re.M)
    indent = re.match(r'[ \t]*', case['end'])[0]
    body += f'{indent}_checksum_style="sha256"\n{indent}_checksum_value="{checksum}"\n'
    return installer[:case.start('body')] + body + installer[case.end('body'):]


def verify_installer_checksum(installer, name, checksum):
    body = installer_case(installer, name)['body']
    assignments = re.findall(r'^[ \t]*_checksum_(?:style|value)=.*$', body, re.M)
    tail = (rf'[ \t]*_checksum_style="sha256"\n'
            rf'[ \t]*_checksum_value="{re.escape(checksum)}"\n?\Z')
    if len(assignments) != 2 or not re.search(tail, body):
        raise ValueError(f'installer does not verify {name} with exactly one final checksum pair')


def record_build(args):
    if args.target not in TARGETS:
        raise ValueError('unsupported release target')
    distrib = Path(args.distrib).resolve()
    name = f'post-{args.target}.tar.xz'
    binary_hash = check_archive(distrib / name)
    receipt = {'target': args.target, 'archive_sha256': digest(distrib / name),
               'binary_sha256': binary_hash, 'source': source_map(ROOT)}
    if args.target.endswith('apple-darwin'):
        receipt['notarization'] = notarization(distrib, args.target)
        verify_darwin_signature(distrib / name)
    (distrib / f'{name}.build.json').write_text(json.dumps(receipt, indent=2) + '\n')


def assets(args):
    root, distrib = ROOT, Path(args.distrib).resolve()
    mapping = source_map(root)
    hashes = {}
    for name in ARCHIVES:
        binary_hash = check_archive(distrib / name)
        hashes[name] = digest(distrib / name)
        if name != 'source.tar.gz':
            verify_build_receipt(distrib, name, mapping, hashes[name], binary_hash)
    if source_archive_digest(root) != hashes['source.tar.gz']:
        raise ValueError('source.tar.gz differs from the reviewed public snapshot')
    installer = (distrib / 'post-installer.sh').read_text()
    for name in ARCHIVES[1:]:
        installer = installer_checksum(installer, name, hashes[name])
    for name in ARCHIVES:
        (distrib / f'{name}.sha256').write_text(f'{hashes[name]} *{name}\n')
    (distrib / 'sha256.sum').write_text(''.join(f'{hashes[name]} *{name}\n' for name in ARCHIVES))
    (distrib / 'post-installer.sh').write_text(installer)
    notes = re.search(rf'^## {re.escape(version(root))}[^\n]*\n(.*?)(?=^## |\Z)',
                      (root / 'CHANGELOG.md').read_text(), re.M | re.S)
    if not notes or not notes[1].strip():
        raise ValueError('CHANGELOG needs a nonempty section for this release')
    (distrib / 'release-notes.md').write_text(notes[1].strip() + '\n')
    names = ARCHIVES + [f'{name}.sha256' for name in ARCHIVES] + ['sha256.sum', 'post-installer.sh']
    manifest = {'source': mapping, 'version': version(root),
                'assets': {name: digest(distrib / name) for name in names}}
    (distrib / 'asset-manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
    verify(args)


def verify(args):
    distrib = Path(args.distrib).resolve()
    mapping = source_map(ROOT)
    manifest = json.loads((distrib / 'asset-manifest.json').read_text())
    if manifest['source'] != mapping:
        raise ValueError('asset manifest differs from the prepared source')
    if manifest['version'] != version(ROOT):
        raise ValueError('asset manifest version differs from the prepared source')
    if source_archive_digest(ROOT) != digest(distrib / 'source.tar.gz'):
        raise ValueError('source.tar.gz differs from the reviewed public snapshot')
    names = ARCHIVES + [f'{name}.sha256' for name in ARCHIVES] + ['sha256.sum', 'post-installer.sh']
    if set(manifest['assets']) != set(names):
        raise ValueError('asset manifest must name exactly the twelve release assets')
    installer = (distrib / 'post-installer.sh').read_text()
    for name, expected in manifest['assets'].items():
        if digest(distrib / name) != expected:
            raise ValueError(f'asset checksum mismatch: {name}')
    aggregate = ''.join(f'{digest(distrib / name)} *{name}\n' for name in ARCHIVES)
    if (distrib / 'sha256.sum').read_text() != aggregate:
        raise ValueError('aggregate checksum file mismatch')
    for name in ARCHIVES:
        binary_hash = check_archive(distrib / name)
        expected = digest(distrib / name)
        if (distrib / f'{name}.sha256').read_text() != f'{expected} *{name}\n':
            raise ValueError(f'checksum sidecar mismatch: {name}')
        if name != 'source.tar.gz':
            verify_build_receipt(distrib, name, mapping, expected, binary_hash)
            verify_installer_checksum(installer, name, expected)
    print('Verified all twelve assets and installer archive checksums; nothing published.')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='command', required=True)
    public = sub.add_parser('prepare-public')
    public.add_argument('destination')
    public.add_argument('--source-ref', default='HEAD')
    public.add_argument('--base-ref', default='refs/remotes/github/main')
    public.add_argument('--public-name', required=True)
    public.add_argument('--public-email', required=True)
    public.set_defaults(run=prepare_public)
    record = sub.add_parser('record-build')
    record.add_argument('target', choices=TARGETS)
    record.add_argument('distrib', nargs='?', default=str(ROOT / 'target/distrib'))
    record.set_defaults(run=record_build)
    for command, run in [('prepare-assets', assets), ('verify', verify)]:
        child = sub.add_parser(command)
        child.add_argument('distrib', nargs='?', default=str(ROOT / 'target/distrib'))
        child.set_defaults(run=run)
    args = parser.parse_args()
    try:
        args.run(args)
    except (OSError, ValueError, subprocess.CalledProcessError, tarfile.TarError) as error:
        parser.exit(1, f'release: {error}\n')


if __name__ == '__main__':
    main()
