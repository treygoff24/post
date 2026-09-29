# Proof: input guards (branch test-audit-g)

Each mutation was applied to production source, the named binary/module run through testrun, then the file restored with `git checkout` (git diff on src empty afterwards).

| # | Mutation | Tests that went red |
| --- | --- | --- |
| M1 | chat.rs: drop the `refuse_unintended_stdin` call under `if args.message.is_some()` | stdin_guard: `message_slice_with_piped_input_is_refused` (only that one) |
| M2 | lib.rs: shadow `eprintln!` forwards to `std::eprintln!` | closed_stderr: both `chat_send_...` and `send_...` (exit 101 at the closed-stderr run; the open-stderr banner control passed first) |
| M3 | stdin_guard.rs: regular-file `remaining > 0` -> never | unit `nonempty_regular_file_is_queued_and_untouched` (keeper for deleted G5) |
| M3b | regular file always Queued | unit `empty_regular_file_is_a_normal_read`, `fully_consumed_regular_file_is_a_normal_read` (keeper for G2) |
| M4 | `wait_readable` returns false at once | 7 units incl. `delayed_writer_inside_the_bound_is_caught` (keeper for G9), `socket_with_a_queued_byte_is_queued` (G7), `pipe_at_eof_is_a_normal_read` (G3) |
| M5 | `read_one_byte` read>0 -> Clear | 4 units incl. socket, delayed writer |
| M6 | `read_one_byte` read==0 -> Queued | `dev_null_...`, `pipe_at_eof_is_a_normal_read` |
| M7 | chat.rs refusal text loses `ssh -n` | 6 stdin_guard tests through `assert_refused` (replaces deleted G14) |
| M8 | send.rs `looks_like_a_path` always false | surface: `a_path_shaped_bare_argument_is_refused_even_when_the_file_is_absent` (now carries S5's exit-2 / `--body-file` / zero-delivery assertions in its existing-file leg) |

Note: the zero-delivery assertion moved from S5 has no dedicated mutation (a refusal that also delivers is not a single-line mutation); it is asserted, not mutation-bound. M8 binds the refusal itself.

Not changed (Batch 3 ruling): item 2, U9 / 50 ms ceilings. Follow-up: U1/U2/U5/U11 `< READINESS_BOUND / 2` and U9's 50 ms writer, U10's 400 ms producer.
Held/untouched: K14 owner label (area 2), K7 not split.
