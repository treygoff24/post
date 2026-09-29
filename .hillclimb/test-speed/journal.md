# Test speed and resource climb

## Brief
Trey asked for tests that run much faster without consuming the machine's CPU.
All test cases and assertions remain intact. No production behavior changes.

## Setup
- Target: warm wall time of all 224 tests in the largest integration binary, cli.
- Secondary metric: total descendant CPU seconds; must not increase materially.
- Resource ceiling: four allowed CPUs on Linux, inherited by all descendants;
  two compiler jobs. All workloads pass through testrun's shared test slice.
- Minimum meaningful effect: 10%. Shared-machine variance exceeded the original
  3% and then 5% thresholds; raising the threshold avoids keeping tiny wins.
- Harness: bench/test-speed.py, fixed commands, fixed test count. Approved locally
  under Trey's carte blanche for this work. Worker settings and Cargo profiles
  are the implementation being tuned, rather than locked measurement fixtures.
- Held-out: entire canonical gate, including Rust, release, Node hooks/launcher,
  schema, Python bridge. Same Rust test inventory and existing assertions.
- Scope: test execution, build profiles for tests, runner scripts and docs.
- Stop after testing bounded worker counts and one compiler-profile optimization,
  or after three rounds inside noise. No unbounded concurrency experiments.
- Starting implementation: f277aa1. Original product code unchanged.

## Measurement corrections
The estate Cargo pool is shared by worktrees. A helper path selects a *pool slot*,
not a permanent worktree identity. Initial attempts reused one slot and caused
alternating recompilation. Those measurements are invalid and are not evidence
for any keep decision. Filed a papercut. Corrected the harness to persist each
checkout's own child target below the managed path before restarting. The final
harness is identical in both checkouts and is locked. No previous number crosses
this change. Compilation after a source change is paid during warmup; cold costs
are separately reported and never claimed to disappear.

## Baseline and noise floor
Five unchanged interleaved pairs: medians 5.513s and 5.494s, paired change +0.81%,
95% CI [-8.69%, +2.05%], half-width 5.72%. Verdict WITHIN NOISE. This floor is below
our 10% minimum effect. Test count 224 in every run. CPU medians 4.730s and 4.710s.

## Rounds
Pending known-cost sanity check and worker-count measurements.

## Result
Pending full verification.

### Confirmed measurements
- Planted two-second delay: 5.453s -> 7.481s, +37.19%, 95% CI
  [+35.71%, +39.84%]. WORSE as expected; plant removed.
- Four to eight Rust workers: 5.420s -> 3.331s, -38.54%, CI
  [-39.45%, -36.75%]. CPU 4.571s -> 4.386s. Kept.
- Eight to sixteen: 3.433s -> 2.562s, -25.08%, CI
  [-27.54%, -21.59%]. CPU 4.488s -> 4.576s (+2%). Kept under the same
  four-CPU ceiling. Defaults scale down to four test threads per available CPU,
  up to sixteen. Explicit environment overrides remain supported.
- Test profile opt-level=1/debug=1: 2.687s -> 2.524s (-5.39%), CPU
  4.668s -> 3.797s. Below minimum effect and substantial recompilation cost;
  rejected. Cargo.toml restored byte for byte.
- Installer and lazy-fixture experiments: rejected; their separate journals
  contain the results. Existing test source and assertions are unchanged.

### Full workload and budget
The first complete gate cost 537s including compilation; 860 Rust tests,
513 hook tests, 33 launcher tests, and 366 Python bridge tests (one existing skip)
passed, along with bridge install checks, format, clippy, release and schema.
Repeating that workload twelve times would spend too many resources. Full-suite
confirmation therefore uses single observations, not a statistical confidence
claim; the smaller Rust experiments above use five interleaved pairs.

A precompiled comparison measured baseline 496.795s / 558.868 CPU-seconds and
candidate 393.906s / 612.718 CPU-seconds. Inspection found the candidate rebuilt
its test artifacts (6.59s) and release binary (23.94s) after staging, whereas the
baseline did not. This is a valid complete invocation, but an unfair comparison
of *warm* CPU cost. A candidate repeat with no code or index changes resolves it.

The full runner starts larger Node files first via Node's native run({files})
API (the CLI sorts filename arguments again). Node and bridge concurrency are
bounded at four, compiler jobs at two. Test-runner checks prove inherited CPU
limits, smaller-machine worker scaling, refusal of zero/unlimited worker counts,
complete Node file execution, and failure exit propagation. Removing CPU affinity,
zero guards, and Node failure propagation each produced the intended failing
check; all mutations restored. Node 22 and 26 checks pass. ShellCheck passes.

## Final result
The unchanged warm candidate repeat passed the entire gate in 346.097s with
541.034 CPU-seconds, versus baseline 496.795s and 558.868 CPU-seconds:
30.33% less wall time and 3.19% less CPU time. These are single full-workload
observations on a shared machine, not confidence intervals. Both measured
invocations had the same four-CPU ceiling. Same inventory: 860 Rust, 513 hooks,
33 launcher, 366 bridge tests with the same one skip, plus bridge install checks.
The candidate passed the full gate twice. The largest Rust suite improved about
53% in the repeated experiments without increasing its total CPU work.

Kept: bounded entry point, two compiler jobs, up to sixteen Rust test threads,
four Node files and bridge suites, and larger Node files starting early. No
existing test, assertion, deadline, production behavior or build profile changed.
Cold compilation remains real work and is excluded from warm speed claims.
Mac CPU affinity and a full Mac gate were not verified. Node 22 API/runner checks
and Node 26 full gate passed. Scaling default workers incorrectly to sixteen on
a two-CPU child also produced the intended failure; restored and four runner
checks green. Measurements and rejected experiments are archived alongside this
journal. Future reproduction needs warmups again after a commit/source change.
