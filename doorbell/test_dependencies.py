"""Startup must explain missing tools without entering the watch loop."""

import os
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


if __name__ == "__main__":
    unittest.main()
