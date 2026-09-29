# Layer review: install, gate, and launcher

Read-only review. All source and test citations below refer to `/home/trey-agent/Code/post` (the main checkout), not this audit worktree. No tests, Cargo, builds, Node, or Python were run.

## Disagreements with ledger marks

None of the 75 marks needs changing. In particular:

| Test | Ledger / review | Evidence and condition |
| --- | --- | --- |
| `a_truncated_backup_under_the_build_sha_name_is_not_trusted` (`tests/install_post.rs:403`) | C / C | Keep its truncated-file **row** when folding into `a_backup_of_other_bytes_with_the_same_build_sha_is_not_trusted` (`:426`); the two currently exercise the same untrusted primary-backup branch in `scripts/install-post.sh:298-310`. Preserve the diagnostic assertion at `:418-422`. The keeper's current other bytes (`:428-431`) are a different length, so make one folded row same-length altered bytes if size-only trust is a mutation of interest. |
| `a_smoke_exiting_zero_without_an_itemized_pass_installs_nothing` (`tests/install_post.rs:558`) | F / F | The `lying` fake emits only porch (`:57-58`), so the installer rejects missing setup at `scripts/install-post.sh:238-240` before reaching the result check at `:241-247`. The proposed complete six-record `lying` row reaches the intended guard. |
| `wrapper chain: shim -> re-entering wrapper -> shim terminates and runs the vendor once` (`launcher/agent-session.test.mjs:630`) | F / F | `:675` accepts every nonempty split, and the vendor sees only the final address (`:648-676`). Record the address in the intermediate wrapper before re-entry and compare it with the vendor's address. Removing `launcher/agent-session:313-317` should then change the address and fail the assertion. |
| `shims exec the helper with their harness slug` (`launcher/agent-session.test.mjs:339`) | D / D | The end-to-end shim test (`:818-850`) runs each shim and checks harness/address and vendor execution. A wrong slug or helper path reaches a visible failure there. Delete only the source grep; no production seam is unlocked by this D. |
| `tests/acceptance.sh` absolute-`cp` grep (`:9-12`) | R / R, weak | It guards the macOS `cp` PATH contract at low cost, but only for commands starting a line. There is no stronger cross-platform smoke test in this lane. Keep until a real alternative exists; do not count its green result as proof that every shell context avoids an absolute `cp`. |

The remaining R rows protect distinct installation outcomes, ownership/refusal paths, smoke result parsing, gate process handling, target-directory precedence, or launcher behavior. I found no additional safe layer deletion. A few assertions overlap, but the keeper in each pair reaches a different branch: for example, the already-owned backup (`tests/install_post.rs:444`) is distinct from the untrusted backup (`:426`); the first-install rollback (`:843`) is distinct from rollback with a live binary (`:818`); and installer-to-smoke `--expect-build` wiring (`:1147`) is distinct from smoke-side enforcement (`:724`).

## Keepers per contract

| Contract | Keeper(s) in main checkout |
| --- | --- |
| Backup naming, byte integrity, reuse, collision, first install, rollback | `tests/install_post.rs:341,403-440` (fold `:403` into `:426`), `:444,460,479,818,843,855` |
| Traceable origin reachability and receipts | `tests/install_post.rs:947,973,999,1014,1032,1067,1097,1127` |
| Build provenance, target directory, and skill verdicts | `tests/install_post.rs:365,384,874,897,1147`; `launcher/cargo-release-bin.test.mjs:128-129` |
| Smoke's six checks, skip policy, build id, version agreement, and Porch path | `tests/install_post.rs:506,519,537,558` (repair), `:573,724,751,773` |
| `who` speed rule in smoke; product scaling at host width | `tests/install_post.rs:697` for smoke timing; `tests/scaling.rs:191-216` for the 2,000-participant `post who` deadline and counts (`PARTICIPANTS = 2_000` at `:23`) |
| Gate success/failure and process-group timeout | `tests/gate_scripts.rs:140,191,210,234,243,260,294` |
| Launcher identity, lookup, validation, vendor resolution, and all four shims | `launcher/agent-session.test.mjs:112,131,150,164,186,202,214,249,297,357,398,448,469,513,531,568,630` (repair), `:699,745,785,818`; the `:818` test absorbs `:339` |
| Launcher install, check, and safe uninstall ownership | All ten `launcher/install.test.mjs` tests (`:37-194`); the no-receipt symlink case (`:133`) is distinct from no-receipt managed files (`:81`) |
| Portable `cp` in Plan B acceptance | `tests/acceptance.sh:9-12` (limited source guard) |

## Override/seam verdicts and caller search

I searched the exact three names with `rg -n 'AGENT_SESSION_POST_BIN|POST_LAUNCHER_PREFIX|POST_AGENT_SHIM_DIR'` in the main checkout's `README.md`, `CONTRACT.md`, `docs/`, `launcher/`, `scripts/`, `skills/`, and `tests/`, then with `rg -l` over `/home/trey-agent/Code` excluding `.git` and `target`. Outside copied Post checkouts, the only hit was a historical result in `papercuts/docs/fixes/2026-09-22/post-fix2.report.json:345`, not an operator caller. This is evidence of no checked-in non-test caller in that search scope, not proof that a live environment never sets an override.

| Override | Verdict and real boundary |
| --- | --- |
| `AGENT_SESSION_POST_BIN` | `launcher/agent-session:38-42` explicitly advertises this for “tests and unusual installs”; the checked-in calls are only `launcher/agent-session.test.mjs:48,370,538,551,595,614,661,687,726,842`. No README/docs/script consumer was found. It is a test-only production seam by checked-in usage, with a plausible but unverified operator use for a binary outside PATH. The normal contract is `post` on PATH (`agent-session:42,80-89,251-252`). The tests can put a real `post` symlink or failing fake named `post` in a sandbox PATH; the missing-binary row at `:531-542` needs a PATH with no `post` while retaining its required shell tools. This is more test refactoring than the other two overrides and affects an inline documented unusual-install option. **Do not remove without maintainer decision.** If retained as an intentional operator escape hatch, document that use explicitly and stop calling it test-only. |
| `POST_LAUNCHER_PREFIX` | `launcher/install:18` overrides a `HOME`-relative default; only `launcher/install.test.mjs:31` supplies it. README documents the default path at `README.md:496-501`, not this override. The tests already create a disposable root (`install.test.mjs:18-24`); setting `HOME` to that root and deriving `prefix = <HOME>/.local/libexec/post-launcher` reaches the same install/check/uninstall boundary. No checked-in operator caller found. Recommend removing this test-only seam after the test moves to `HOME`, subject to maintainer approval of production-code deletion. |
| `POST_AGENT_SHIM_DIR` | `launcher/install:19` similarly overrides `HOME/.local/agent-shims`; only `launcher/install.test.mjs:32` supplies it. The same sandbox `HOME` can derive `links = <HOME>/.local/agent-shims`; the two destinations remain independent directories. No checked-in operator caller found. Recommend removing with the prefix override, subject to the same maintainer decision. |

The prefix and shim-directory overrides could be intentional site-configuration features despite having no found callers. Their removal changes accepted environment behavior. Since the review brief requires a maintainer decision for any production deletion or documented-contract change, treat the three seam removals as **decision items**, not automatic cutover edits.

The ledger's `launcher/install.test.mjs` declaration citations `:166-310` do not match the main checkout: the ten declarations are at `:37,60,67,81,103,117,133,150,172,181`. Their described behaviors and marks match those declarations. The ledger's `tests/acceptance.sh:108,113` citations are likewise stale; that script is 15 lines in the main checkout (`:9,14`).

## Bugs and gate path

- **Confirmed from source: truncated help.** `scripts/install-post.sh:105` prints only source lines 2-78, while the header's exit-code 7 and unexpected-phase explanation are at `:79-81`. `--help` therefore omits them. Change the range to `2,81p` at minimum; a marker-delimited header would avoid a later repeat. No execution proof was possible in this lane.
- **Confirmed from source: Plan B acceptance hard-codes the release binary.** `tests/acceptance.sh:13-15` builds release then passes `target/release/post` to `smoke-installed.sh`; a configured target directory makes that path wrong. `scripts/cargo-release-bin.mjs:7-14` is the existing resolver. The acceptance script should use it, while checking that the resolved binary is the one just built if the estate wrapper can choose different target slots across invocations. The ledger's `:113` citation is stale; the main checkout has this at line 14.
- **Hook suite default mismatch confirmed.** `skills/post/hooks/contract.test.mjs:34`, `doorbell-supervisor.test.mjs:43`, and `doorbell-supervisor-process.test.mjs:20` use `<repo>/target/release/post` unless `POST_BIN` is set. Launcher tests instead call `cargoReleaseBin(REPO)` (`launcher/agent-session.test.mjs:13,18`). `scripts/gate.sh:39-48` resolves and exports `POST_BIN` before the hook suites, so the canonical script should not require the operator to set it. The smallest immediate gate fix is to replace CONTRIBUTING's copied command list and hard-coded schema path (`CONTRIBUTING.md:5-15`) with `./scripts/gate.sh`, which CI already runs (`.github/workflows/ci.yml:39`). For direct standalone hook tests, share `cargoReleaseBin` as the fallback in those three files; that is the small code fix and should be coordinated with area 17. Neither source fact explains a reported failure of **gate.sh itself** without `POST_BIN`; that observation needs an allowed run with the failing step captured. Also, if resolver failure occurs while an inherited `POST_BIN` is set, `gate.sh:39-43` records an error but leaves the inherited value available to later steps, so clear it or skip binary-dependent steps on that failure.
- **Docs path drift.** README install instructions (`README.md:39`) and capability-repair text (`skills/post/hooks/mail-hook-core.mjs:91-93`) also assume `target/release/post`. A change to these operator instructions is a documented-contract edit and needs the maintainer's decision.

## Real-smoke fixture size

The eight invocations are two each in `tests/install_post.rs:573,697,724`, and one each at `:751,773`. The smoke defaults to 2,000 participants (`scripts/install-smoke.sh:155-156`), seeds exactly that many (`:168-176`), checks `who` returns at least the chosen count (`:194-196`), and times it (`:179-203`). None of the eight test assertions pins **2,000**: the speed test's 2.5-second delay (`tests/install_post.rs:693-719`) tests threshold enforcement independent of width; the other tests target Porch, build id, or version behavior. `tests/scaling.rs:23,190-216` owns the actual wide-store performance and count contract. Set `POST_SMOKE_WHO_PARTICIPANTS=50` for all eight test invocations (including the two direct runners at `install_post.rs:573-592,791-803`), while leaving production smoke's 2,000 default intact. Fifty exercises all three workspace variants (`install-smoke.sh:167-173`) and the `who` count assertion, with no lost test contract. The ledger's proposed shrink is sound but must touch both direct runners as well as `run_real_smoke` (`install_post.rs:633-662`), which currently removes the override at `:651`. No timing or I/O savings were measured here.

## Final edit list, in implementation order

1. Repair the two F tests: make `lying` emit a complete six-check file with one `fail` and assert the specific refusal (`tests/install_post.rs:34-60,558-567`); log and compare wrapper and vendor addresses before/after re-entry (`launcher/agent-session.test.mjs:630-676`). Prove each by removing its production guard in an isolated run when tests are authorized.
2. Fold the truncated-backup case into the `:426` keeper, carrying its byte-preservation and diagnostic assertions; then delete the `:403` declaration. Delete the shim source-grep test at `launcher/agent-session.test.mjs:339`, retaining `:818`.
3. Shrink all eight real-smoke fixture invocations to 50 participants, keeping `scripts/install-smoke.sh`'s default unchanged. Repair the `--help` range and acceptance target-directory path as separate small bug changes.
4. Make `./scripts/gate.sh` the CONTRIBUTING gate; coordinate area 17's three hook test defaults with the shared resolver if standalone runs are supported. Fix the inherited-`POST_BIN` failure path in `gate.sh` if reproducing it confirms a meaningful false signal.
5. After maintainer decisions, either migrate launcher tests to sandbox `HOME`/`PATH` and remove the three overrides, or record the operator use and retain them deliberately.

## Maintainer decisions and run-only questions

- Decide whether the three launcher environment overrides are supported operator features before deleting their production branches. The `AGENT_SESSION_POST_BIN` script header is already a user-facing mention. No change to `CONTRACT.md` was identified, but README/CONTRIBUTING and hook repair instructions are operator-facing docs.
- Decide whether Plan B acceptance (`tests/acceptance.sh`) remains a supported separate gate; its smoke and source grep are absent from the canonical gate. If it remains, fix its target path.
- Under an authorized test window, confirm both F repairs go red for their intended mutation; verify the current `lying` row's actual stderr; run direct hook tests with the resolver and the final gate. To explain the observed `POST_BIN` dependence of gate.sh, capture whether release build, metadata resolver, and the binary path refer to the same artifact under the estate cargo wrapper. No such run was made here.
