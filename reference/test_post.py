#!/usr/bin/env python3
"""Smallest thing that fails if post breaks: send/inbox/read roundtrip,
banner present on read, and — load-bearing — the armed-instrument block."""

import os
import subprocess
import sys
import tempfile
from pathlib import Path

POST = [sys.executable, str(Path(__file__).parent / "post.py")]


def run(*args, stdin=None, cwd=None):
    return subprocess.run(POST + list(args), input=stdin, cwd=cwd,
                          capture_output=True, text=True)


with tempfile.TemporaryDirectory() as tmp:
    os.environ["HOME"] = tmp  # point ~/.claude-mail at a sandbox

    # roundtrip: send note pact -> claude-space
    r = run("send", "--to", "claude-space", "--from", "cousin-test",
            "--kind", "note", "--subject", "hello", stdin="test body\n")
    assert r.returncode == 0, r.stderr
    mid = r.stdout.split()[3]

    r = run("inbox", "--room", "claude-space")
    assert mid in r.stdout and "[note] from cousin-test" in r.stdout, r.stdout

    r = run("read", mid, "--room", "claude-space")
    assert "ANOTHER AI AGENT" in r.stdout, "banner missing!"
    assert "permission-launder" in r.stdout, "law missing from banner!"
    assert "test body" in r.stdout

    # read marks read: inbox now empty
    r = run("inbox", "--room", "claude-space")
    assert "empty" in r.stdout

    # archive copy exists
    assert (Path(tmp) / ".claude-mail" / "archive" / f"{mid}.mail").exists()

    # LOAD-BEARING: armed-instrument route refuses
    r = run("send", "--to", "agent-memory", "--from", "rogue-lane",
            stdin="should never arrive")
    assert r.returncode != 0 and "BLOCKED" in r.stderr, "armed route not blocked!"
    assert "ARMED INSTRUMENT" in r.stderr

    # unknown recipient refuses
    r = run("send", "--to", "nowhere", "--from", "codex-free", stdin="x")
    assert r.returncode != 0

    # impersonating a registered room from outside its tree refuses
    r = run("send", "--to", "claude-space", "--from", "pact", stdin="x")
    assert r.returncode != 0 and "reserved" in r.stderr, "impersonation not blocked!"

    # free-form sender still works from anywhere
    r = run("send", "--to", "claude-space", "--from", "opus-elsewhere", stdin="hi")
    assert r.returncode == 0, r.stderr

    # omitted --from in an unregistered dir: sender = dir basename
    import tempfile as tf
    with tf.TemporaryDirectory() as d:
        wd = Path(d) / "my-project"; wd.mkdir()
        r = run("send", "--to", "claude-space", stdin="hi from nowhere", cwd=str(wd))
        assert r.returncode == 0 and "my-project -> claude-space" in r.stdout, r.stdout + r.stderr

print("post self-check: all assertions passed")
