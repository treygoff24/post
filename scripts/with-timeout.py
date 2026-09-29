#!/usr/bin/env python3
"""Run a command with a wall-clock limit, portably (macOS has no `timeout`).

    scripts/with-timeout.py SECONDS COMMAND [ARG...]

The command runs in its own process group. On expiry the whole group gets
SIGTERM, then SIGKILL five seconds later, so a hung test binary and the
children it spawned die with the `cargo test` that started them. Exit status
is the command's own; 124 when the limit expired (as GNU timeout); 125 on a
usage error or when the command could not be started.
"""
import os
import signal
import subprocess
import sys

GRACE_SECONDS = 5


def main(argv):
    if len(argv) < 3:
        print("usage: with-timeout.py SECONDS COMMAND [ARG...]", file=sys.stderr)
        return 125
    try:
        limit = float(argv[1])
    except ValueError:
        limit = -1.0
    if limit <= 0:
        print(f"with-timeout: SECONDS must be a positive number, got {argv[1]!r}", file=sys.stderr)
        return 125
    try:
        child = subprocess.Popen(argv[2:], start_new_session=True)
    except OSError as error:
        print(f"with-timeout: cannot run {argv[2]}: {error}", file=sys.stderr)
        return 125

    def forward(signum, _frame):
        # An interrupted gate must not leave its test binaries running.
        try:
            os.killpg(child.pid, signum)
        except ProcessLookupError:
            pass

    for name in ("SIGINT", "SIGTERM", "SIGHUP"):
        signal.signal(getattr(signal, name), forward)
    try:
        status = child.wait(timeout=limit)
        # A child killed by a signal reports -N; a shell would say 128+N.
        return status if status >= 0 else 128 - status
    except subprocess.TimeoutExpired:
        print(f"with-timeout: {argv[2]} exceeded {argv[1]} s; killing its process group", file=sys.stderr)
        for signum, wait in ((signal.SIGTERM, GRACE_SECONDS), (signal.SIGKILL, None)):
            try:
                os.killpg(child.pid, signum)
            except ProcessLookupError:
                break
            try:
                child.wait(timeout=wait)
                break
            except subprocess.TimeoutExpired:
                continue
        return 124


if __name__ == "__main__":
    sys.exit(main(sys.argv))
