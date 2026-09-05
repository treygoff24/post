"""Startup must explain missing tools without entering the watch loop."""

import os
import shlex
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).parent


class Dependencies(unittest.TestCase):
    def test_missing_tool_is_actionable_without_traceback(self):
        for missing in ("herdr", "post"):
            with self.subTest(missing=missing), tempfile.TemporaryDirectory() as tmp:
                other = Path(tmp) / ("post" if missing == "herdr" else "herdr")
                other.write_text("#!/bin/sh\nexit 0\n")
                other.chmod(0o755)
                result = subprocess.run(
                    [sys.executable, str(ROOT / "post-doorbell"), "--agent", "fake"],
                    env={**os.environ, "PATH": tmp}, capture_output=True, text=True, timeout=5,
                )
                self.assertEqual(result.returncode, 2)
                self.assertIn(f"required executable '{missing}' not found on PATH", result.stderr)
                self.assertIn("~/.local/bin", result.stderr)
                self.assertNotIn("Traceback", result.stderr)

    def test_service_path_includes_user_tools_without_shell_expansion(self):
        unit = (ROOT / "post-doorbell@.service").read_text()
        self.assertIn('Environment="PATH=%h/.local/bin:/usr/local/bin:/usr/bin:/bin"', unit)

    def test_service_prefers_user_binary_and_retains_system_fallback(self):
        unit = (ROOT / "post-doorbell@.service").read_text().splitlines()
        executable = next(line.removeprefix("ExecStart=") for line in unit
                          if line.startswith("ExecStart="))
        environment = next(line.removeprefix("Environment=") for line in unit
                           if line.startswith("Environment="))
        self.assertEqual(shlex.split(executable),
                         ["/usr/bin/env", "post-doorbell", "--agent", "%i"])
        for user_installed in (True, False):
            with self.subTest(user_installed=user_installed), tempfile.TemporaryDirectory() as tmp:
                home = Path(tmp)
                user_bin, system_bin = home / ".local/bin", home / "system-bin"
                user_bin.mkdir(parents=True)
                system_bin.mkdir()
                targets = [(system_bin, "system")]
                if user_installed:
                    targets.append((user_bin, "user"))
                for directory, label in targets:
                    stub = directory / "post-doorbell"
                    stub.write_text(f'#!/bin/sh\nprintf "%s\\n" "{label}" "$@"\n')
                    stub.chmod(0o755)
                path = shlex.split(environment)[0].removeprefix("PATH=")
                path = path.replace("%h", tmp).replace("/usr/local/bin", str(system_bin))
                result = subprocess.run(
                    shlex.split(executable.replace("%i", "fake")), env={"PATH": path},
                    capture_output=True, text=True, timeout=5, check=True,
                )
                self.assertEqual(result.stdout.splitlines(),
                                 ["user" if user_installed else "system", "--agent", "fake"])


if __name__ == "__main__":
    unittest.main()
