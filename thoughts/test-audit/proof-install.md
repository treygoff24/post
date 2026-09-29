# Proof: install (batch 4, worktree post-test-audit-b, branch test-audit-h)

Each mutation was applied to production code, the named test run through testrun, then the file restored byte for byte (git diff empty).

| Repair | Mutation | Result |
| --- | --- | --- |
| a_smoke_exiting_zero_without_an_itemized_pass_installs_nothing (F, lying row now complete six records, one fail; stderr asserted) | scripts/install-post.sh result loop: `if result == "pass"` replaced by `if True` | RED (the lying row installed). Restored, green. |
| a_backup_of_other_bytes_with_the_same_build_sha_is_not_trusted (C, truncated + different build + same-length rows) | backup trust check compares file size instead of sha256 | RED (the same-length row is the only one a size check trusts). Restored, green. |
| same test | backup trust check replaced by `true` (exists-only) | RED. Restored, green. |
| wrapper chain test (F, wrapper logs address, compared with vendor's) | launcher/agent-session: re-entry branch condition replaced by `false` (re-mints) | RED on "re-entry keeps the minted address". Restored, green. |

Baseline after edits: install_post 32/32, agent-session.test.mjs 21/21.
Not mutation-proved: the `silent` row's stderr assertion, the shim-grep deletion (D, covered by the end-to-end shim test), 50-participant shrink.

## gate_scripts.rs: "Text file busy" flake (own commit)

Symptom (from the full gate): `run gate.sh: Os { code: 26, kind: ExecutableFileBusy }` in the hung-step and failing-step tests, green on rerun.

Cause found in the harness: `run_gate` built `Command::new("bash")` with PATH set to `<shims>:/usr/bin:/bin`. Rust searches the child's PATH, so the program it execs is the stand-in `shims/bash` that `shim_dir` wrote a moment earlier. With eight test threads forking in parallel, another thread's forked child can still hold that file's write descriptor between fork and its own exec, and exec of a file open for writing fails with ETXTBSY.

Fix: `run_gate` now execs the real bash by absolute path (`real_tool("bash")`) and passes gate.sh as its argument, so the test thread never execs a freshly written file. The shims are still exec'd, but by gate.sh several milliseconds later inside the child, well after writing. A temp-name-and-rename fix would not have worked: rename does not change which inode is open for writing in a forked child.

Verification: gate_scripts 7/7 three times in a row through testrun. This is not a red proof. The race needs an unlucky fork inside a microsecond window, so I could not reproduce the failure on the old code on demand and did not fake one; the claim rests on the error text (ETXTBSY from the `run gate.sh` spawn) and on the PATH-search behaviour above.
