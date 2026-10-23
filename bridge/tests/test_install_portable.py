"""Portable installer integration tests: only temporary homes and stub managers."""
import json
import importlib.util
import os
from pathlib import Path
import plistlib
import shlex
import subprocess
import sys
import tempfile
import unittest

SOURCE = Path(__file__).resolve().parents[1]


def load_sweeper():
    spec = importlib.util.spec_from_file_location('portable_install_sweep', SOURCE / 'sweep.py')
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class BridgeCompatibility(unittest.TestCase):
    def test_supported_versions_overlap_across_upgrade(self):
        sweep = load_sweeper()
        for version in ('0.10.0', '0.10.12', '0.11.0', '0.11.12'):
            for annotation in ('', ' (build abc1234, 2026-10-23)'):
                with self.subTest(version=version, annotation=annotation):
                    self.assertTrue(sweep.post_version_accepted('post ' + version + annotation))
        for version in ('0.9.9', '0.12.0', '1.0.0', '0.11.01', '0.11.0-rc1',
                        '0.11.0+build', '0.11.0 (build nested())', '0.11.0 trailing'):
            with self.subTest(version=version):
                self.assertFalse(sweep.post_version_accepted('post ' + version))

    def test_supported_intervals_produce_health_seconds(self):
        interval_seconds = load_sweeper().pmail.interval_seconds
        for text, seconds in (('15', 15), ('30s', 30), ('2min', 120),
                              ('1h 30s', 3630), ('24h', 86400)):
            with self.subTest(interval=text):
                self.assertEqual(interval_seconds(text), seconds)


class PortableInstall(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name).resolve()
        self.home = self.root / 'home with space & quote\''
        self.home.mkdir()
        self.bin = self.root / 'bin'
        self.bin.mkdir()
        self.mail = self.root / 'mail'
        self.mail.mkdir()
        self.log = self.root / 'manager.log'
        self.state = self.root / 'loaded'
        self.env = dict(os.environ, HOME=str(self.home), POST_MAIL_ROOT=str(self.mail),
                        POST_PARTICIPANT='', PATH=str(self.bin) + ':' + os.environ['PATH'],
                        XDG_CONFIG_HOME=str(self.root / 'config'),
                        MANAGER_LOG=str(self.log), MANAGER_STATE=str(self.state))
        self.env.pop('POST_BIN', None)
        self.script('uname', 'printf "Darwin\\n"\n')
        self.script('post', 'printf "post 0.10.0\\n"\n')
        self.script('systemctl', 'echo "unexpected systemctl" >&2; exit 91\n')
        self.script('launchctl', '''printf '%s\\n' "$*" >>"$MANAGER_LOG"
case $1 in
 print) test -f "$MANAGER_STATE" ;;
 bootout) rm -f "$MANAGER_STATE" ;;
 bootstrap) touch "$MANAGER_STATE" ;;
 enable) exit 0 ;;
 *) exit 92 ;;
esac
''')
        self.relay = self.root / 'relay.git'
        self.git('init', '--bare', '-q', str(self.relay))
        seed = self.root / 'seed'
        self.git('init', '-q', '-b', 'main', str(seed))
        self.git('-C', str(seed), '-c', 'user.name=fixture', '-c',
                 'user.email=fixture@example.invalid', 'commit', '-q', '--allow-empty', '-m', 'seed')
        self.git('-C', str(seed), 'push', '-q', str(self.relay), 'main')
        self.git('--git-dir=' + str(self.relay), 'symbolic-ref', 'HEAD', 'refs/heads/main')
        self.key = self.home / "relay key's file"
        self.key.write_text('fixture')
        self.key.chmod(0o600)
        self.config = self.root / 'topology.json'
        self.config.write_text(json.dumps(dict(host='node-a', relay_url=str(self.relay), channels=None)))
        self.clone = self.root / 'clone'
        self.args = ['--repo-url', str(self.relay), '--host', 'node-a', '--ssh-key', str(self.key),
                     '--config', str(self.config), '--clone-dir', str(self.clone)]
        self.plist = self.home / 'Library/LaunchAgents/org.post.bridge.plist'

    def script(self, name, body):
        p = self.bin / name
        p.write_text('#!/bin/sh\n' + body)
        p.chmod(0o755)

    def git(self, *args):
        subprocess.run(['git', *args], env=self.env, check=True, capture_output=True)

    def install(self, *args, ok=True):
        result = subprocess.run(['bash', str(SOURCE / 'install.sh'), *args], env=self.env,
                                text=True, capture_output=True)
        if ok:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        return result

    def test_launchd_install_reinstall_uninstall(self):
        self.install(*self.args)
        job = plistlib.loads(self.plist.read_bytes())
        self.assertEqual(job['Label'], 'org.post.bridge')
        self.assertEqual(job['StartInterval'], 60)
        self.assertEqual(job['EnvironmentVariables']['BRIDGE_INTERVAL_SECONDS'], '60')
        self.assertEqual(job['EnvironmentVariables']['BRIDGE_REPO'], str(self.clone))
        self.assertEqual(job['EnvironmentVariables']['POST_MAIL_ROOT'], str(self.mail))
        self.assertEqual(job['StandardOutPath'], str(self.mail / 'bridge/launchd.log'))
        self.assertEqual(job['StandardErrorPath'], job['StandardOutPath'])
        self.assertTrue(self.state.exists())
        ssh = subprocess.check_output(['git', '-C', str(self.clone), 'config', 'core.sshCommand'],
                                      env=self.env, text=True)
        self.assertEqual(shlex.split(ssh)[-1], str(self.key))
        self.assertFalse((self.root / 'config/systemd/user/post-bridge.service').exists())
        self.install(*self.args, '--interval', '27')
        self.assertEqual(plistlib.loads(self.plist.read_bytes())['StartInterval'], 27)
        events = self.log.read_text().splitlines()
        self.assertTrue(any(x.startswith('bootout gui/') for x in events), events)
        self.assertTrue(any(x.startswith('enable gui/') for x in events), events)
        self.install('--uninstall', '--clone-dir', str(self.clone))
        self.assertFalse(self.plist.exists())
        self.assertFalse(self.state.exists())
        self.assertFalse((self.home / '.local/lib/post-bridge').exists())
        self.assertTrue((self.clone / '.git').exists())
        self.assertEqual((self.mail / 'bridge/config.json').read_bytes(), self.config.read_bytes())

    def test_legacy_collision_refuses_before_changes(self):
        self.plist.parent.mkdir(parents=True)
        legacy = self.plist.parent / 'example.legacy.plist'
        legacy.write_bytes(plistlib.dumps(dict(Label='example.legacy',
            ProgramArguments=[str(self.home / '.local/bin/post-bridge-sweep')])))
        before = legacy.read_bytes()
        for args in (self.args, ['--uninstall']):
            result = self.install(*args, ok=False)
            self.assertIn('migrate it explicitly', result.stderr)
            self.assertIn(str(legacy), result.stderr)
        self.assertEqual(legacy.read_bytes(), before)
        self.assertFalse(self.clone.exists())
        self.assertFalse(self.log.exists())

    def test_interval_rejects_non_integer(self):
        for interval in ('0', '-1', '1min', '1.5', '01', '86401'):
            with self.subTest(interval=interval):
                result = self.install(*self.args, '--interval', interval, ok=False)
                self.assertIn('positive integer', result.stderr)
        self.assertFalse(self.clone.exists())

    def test_linux_interval_validation_before_changes(self):
        self.script('uname', 'printf "Linux\\n"\n')
        for interval in ('1000ms', '1.5s', '1day', '25h', '0', '-1', 'infinity'):
            with self.subTest(interval=interval):
                result = self.install(*self.args, '--interval', interval, ok=False)
                self.assertIn('supported interval', result.stderr)
                self.assertFalse(self.clone.exists())
                self.assertFalse((self.home / '.local').exists())
                self.assertFalse((self.mail / 'bridge').exists())
                self.assertFalse(self.log.exists())


if __name__ == '__main__':
    unittest.main()
