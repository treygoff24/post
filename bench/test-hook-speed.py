"""Fixed process-level installer case: same behavior, fewer teardown waits."""
import json
import os
import re
import resource
import subprocess
import sys
import time

if hasattr(os, "sched_getaffinity"):
    os.sched_setaffinity(0, sorted(os.sched_getaffinity(0))[:4])
pattern = "default uninstall removes only the supervisor and prints the restoration command"
command = ["testrun", "post", "hook-measure", "--", "node", "--test", "--test-concurrency=4", "--test-reporter=tap", "--test-name-pattern=" + pattern, "skills/post/hooks/install-doorbell-supervisor.test.mjs"]
before = resource.getrusage(resource.RUSAGE_CHILDREN)
start = time.monotonic()
result = subprocess.run(command, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
after = resource.getrusage(resource.RUSAGE_CHILDREN)
print(result.stdout, end="")
passed = re.search(r"^# pass (\d+)$", result.stdout, re.M)
if result.returncode or not passed or passed[1] != "1":
    sys.exit("selected case failed or changed")
print(json.dumps({"wall_s": time.monotonic() - start, "cpu_s": after.ru_utime + after.ru_stime - before.ru_utime - before.ru_stime, "passed": int(passed[1])}))
