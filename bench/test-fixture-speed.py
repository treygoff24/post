"""Four existing bridge helper cases, with their test inventory fixed."""
import json
import os
import re
import resource
import subprocess
import sys
import time

if hasattr(os, "sched_getaffinity"):
    os.sched_setaffinity(0, sorted(os.sched_getaffinity(0))[:4])
tests = ["test_atomic_publish_retries_short_writes", "test_deadline_is_checked_between_inbound_and_outbound_candidates", "test_tick_lock_replacement_during_acquisition_is_busy", "test_internal_error_after_a_git_read_failure_reports_it"]
command = ["testrun", "post", "fixture-measure", "--", "python3", "-m", "unittest"]
command += ["bridge.tests.test_sweep.SweeperTest." + name for name in tests]
before = resource.getrusage(resource.RUSAGE_CHILDREN)
start = time.monotonic()
result = subprocess.run(command, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
after = resource.getrusage(resource.RUSAGE_CHILDREN)
print(result.stdout, end="")
if result.returncode or not re.search(r"Ran 4 tests", result.stdout):
    sys.exit("selected cases failed or changed")
print(json.dumps({"wall_s": time.monotonic() - start, "cpu_s": after.ru_utime + after.ru_stime - before.ru_utime - before.ru_stime, "passed": 4}))
