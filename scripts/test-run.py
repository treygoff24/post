"""Cap this process tree on Linux, then enter the estate test slice if installed."""
import os
import shutil
import sys


def main():
    try:
        cores = int(os.environ.get("POST_TEST_CPUS", "4"))
        if cores < 1:
            raise ValueError
    except ValueError:
        sys.exit("POST_TEST_CPUS must be a positive integer")
    if hasattr(os, "sched_getaffinity"):
        allowed = sorted(os.sched_getaffinity(0))
        os.sched_setaffinity(0, allowed[:cores])
        available = min(cores, len(allowed))
    else:
        available = min(cores, os.cpu_count() or 1)
    # Waiting on subprocesses benefits from more test threads than CPUs.
    # Scale the measured four-CPU default down on smaller machines.
    os.environ.setdefault("RUST_TEST_THREADS", str(min(16, available * 4)))
    os.environ["POST_TEST_WRAPPED"] = "1"
    command = ["bash", "scripts/test.sh", *sys.argv[1:]]
    if shutil.which("testrun"):
        command = ["testrun", "post", "gate-tests", "--", *command]
    os.execvp(command[0], command)


if __name__ == "__main__":
    main()
