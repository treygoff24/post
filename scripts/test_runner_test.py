"""Protect the resource ceiling and failure propagation across a real exec."""
import json
import os
from pathlib import Path
import subprocess
import shutil
import sys
import tempfile
import unittest

RUNNER = Path(__file__).with_name("test-run.py")


class RunnerTests(unittest.TestCase):
    def test_capped_exec_preserves_failure_with_and_without_testrun(self):
        for executable in ["testrun", "bash"]:
            with self.subTest(executable=executable), tempfile.TemporaryDirectory(prefix="post-runner-") as directory:
                fake = Path(directory) / executable
                fake.write_text(f"#!{sys.executable}\nimport json,os,sys\n"
                                "print(json.dumps({'wrapped': os.environ.get('POST_TEST_WRAPPED'),"
                                "'cpus': sorted(os.sched_getaffinity(0)) if hasattr(os, 'sched_getaffinity') else None,"
                                "'workers': os.environ.get('RUST_TEST_THREADS'),"
                                "'args': sys.argv[1:]}))\nsys.exit(23)\n")
                fake.chmod(0o755)
                env = dict(os.environ, PATH=directory, POST_TEST_CPUS="2")
                env.pop("RUST_TEST_THREADS", None)
                result = subprocess.run([sys.executable, str(RUNNER), "rust", "--test", "cli"], env=env, capture_output=True, text=True)
                self.assertEqual(result.returncode, 23, result.stderr)
                receipt = json.loads(result.stdout)
                self.assertEqual(receipt["wrapped"], "1")
                self.assertEqual(receipt["args"][-3:], ["rust", "--test", "cli"])
                if receipt["cpus"] is not None:
                    self.assertGreater(len(receipt["cpus"]), 0)
                    self.assertLessEqual(len(receipt["cpus"]), 2)
                    self.assertTrue(set(receipt["cpus"]) <= os.sched_getaffinity(0))
                    self.assertEqual(int(receipt["workers"]), min(16, len(receipt["cpus"]) * 4))

    def test_invalid_cpu_limit_refuses_before_launch(self):
        for value in ["0", "-1", "many", ""]:
            with self.subTest(value=value):
                env = dict(os.environ, POST_TEST_CPUS=value)
                result = subprocess.run([sys.executable, str(RUNNER)], env=env, capture_output=True, text=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("POST_TEST_CPUS must be a positive integer", result.stderr)

    def test_zero_and_malformed_worker_limits_cannot_launch_cargo(self):
        with tempfile.TemporaryDirectory(prefix="post-workers-") as directory:
            cargo = Path(directory) / "cargo"
            cargo.write_text("#!/bin/sh\necho unexpected-launch\nexit 23\n")
            cargo.chmod(0o755)
            for setting in ["CARGO_BUILD_JOBS", "RUST_TEST_THREADS", "POST_NODE_TEST_JOBS", "BRIDGE_TEST_JOBS"]:
                for value in ["0", "00", "-1", "many"]:
                    with self.subTest(setting=setting, value=value):
                        env = dict(os.environ, PATH=directory + ":/usr/bin:/bin", POST_TEST_WRAPPED="1")
                        env.pop("BASH_ENV", None)
                        env.pop("ENV", None)
                        env[setting] = value
                        result = subprocess.run([shutil.which("bash"), "scripts/test.sh", "rust"], env=env, capture_output=True, text=True)
                        self.assertEqual(result.returncode, 64, result.stderr)
                        self.assertIn(setting + " must be a positive integer", result.stderr)
                        self.assertNotIn("unexpected-launch", result.stdout)

    def test_node_runner_runs_every_file_and_preserves_failure(self):
        with tempfile.TemporaryDirectory(prefix="post-node-runner-") as directory:
            passing = Path(directory) / "passing.test.mjs"
            failing = Path(directory) / "failing.test.mjs"
            passing.write_text("import test from 'node:test'; test('passing marker', () => {});\n")
            failing.write_text("import test from 'node:test'; test('failure marker', () => { throw Error('intentional failure'); });\n")
            result = subprocess.run(["node", "scripts/test-node.mjs", str(passing), str(failing)], capture_output=True, text=True)
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertIn("passing marker", result.stdout)
            self.assertIn("failure marker", result.stdout)
            self.assertIn("# tests 2", result.stdout)


if __name__ == "__main__":
    unittest.main()
