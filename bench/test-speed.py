"""Fixed workload and CPU accounting for the test speed climb (stdlib only)."""
import json
import re
import resource
import subprocess
import sys
import time

holdout = sys.argv[1:] == ["gate"]
command = ["bash", "scripts/test.sh", "gate" if holdout else "rust"]
if not holdout:
    command += ["--test", "cli"]
before = resource.getrusage(resource.RUSAGE_CHILDREN)
start = time.monotonic()
result = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
wall = time.monotonic() - start
after = resource.getrusage(resource.RUSAGE_CHILDREN)
print(result.stdout, end="")
counts = re.findall(r"test result: ok\. (\d+) passed", result.stdout)
if not holdout and (counts != ["224"] or result.returncode):
    sys.exit("workload changed or failed")
print(json.dumps({"wall_s": wall, "cpu_s": after.ru_utime + after.ru_stime - before.ru_utime - before.ru_stime,
                  "peak_rss_kb": after.ru_maxrss, "rust_passed": sum(map(int, counts))}))
sys.exit(result.returncode)
