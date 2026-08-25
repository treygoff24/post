# P1 findings ledger

Every finding the panel raised, and what happened to it. A finding with no
recorded disposition is the one outcome the process cannot allow: it makes the
panel decoration. Added after Mac Fable's gate caught two findings I had neither
fixed nor argued with.

Statuses: **fixed** · **adjudicated-away** (rejected, with a reason) · **deferred** (filed, with an id).

| # | Lane | Finding | Disposition |
|---|---|---|---|
| 1 | conventions, refuter, stranger, test-skeptic (all four) | `details.exact_fix` carried multi-sentence prose with unquoted interpolated paths, into a field the README documents as "a command that runs as written" — a command injection, against a rule this repo had already pinned in `crossed_send_exact_fix_shell_quotes_channel_metacharacters` | **fixed** — a8383e6. Single shell-quoted command; prose moved to `suggested_fix`; `exact_fix` omitted on the POST_FROM branch where no complete command exists. Injection test watched red at 238c1c4. |
| 2 | conventions | `post schema`'s chat usage still led with the read forms, undoing the help fix on the surface SKILL.md names as authoritative when help is ambiguous | **fixed** — 201d44a. Reordered, plus a test pinning that help and schema both lead with a send form, because nothing tied the two renderings together. |
| 3 | conventions | CHANGELOG Unreleased carried no entry for three user-visible error/help changes | **fixed** — 201d44a. |
| 4 | conventions | `ROOM_LIST_PREVIEW` bound and `+N more` suffix were unpinned; the fixture has three rooms and the bound is eight | **fixed** — a8383e6, `many_rooms_are_summarized_inline_but_complete_in_matches`. Recorded honestly: this test passes at 238c1c4 too. The bound was untested, not broken. |
| 5 | test-skeptic | `exact_fix.is_some()` survives any non-empty string, including the prose in finding 1 | **fixed** — a8383e6. Now runs the command through the suite's `run_fix` and asserts the situation it described is resolved. |
| 6 | test-skeptic | `!fix.contains("'#tax'")` also passes for an unquoted `#tax`, which opens a shell comment and degrades the fix to a bare `post chat` | **fixed** — a8383e6. Pins the quoted form and the absence of the sigil. |
| 7 | test-skeptic | A test named `..._and_the_fix_runs` never ran the fix | **fixed** — a8383e6. |
| 8 | stranger | The channel correction discarded `--subject` and `--body-file` the caller had supplied | **fixed** — a8383e6. `--subject`, `--oversize` and the body source carry across; `--kind`, which channels have no equivalent for, is named in the prose rather than dropped. |
| 9 | all four | Open disagreement: should `post send --to '#channel'` route into the channel send path rather than error? | **adjudicated-away**, 4-0 against, and on better grounds than I argued. `send_fix_prefix` already records that `--kind` "always survives into the fix", and channels have no kind at all, so a router must either drop it or invent a meaning. atlasos, who proposed routing, conceded in channel. |
| 10 | conventions, test-skeptic | Two of four lanes had shell blocked inside safe isolation and could not run the suite; their reviews were static reconstructions, disclosed as such | **deferred** — panel-infrastructure, not a code defect. Same genus as delegate-agent ce295d7: what a lane can reach varies by engine and nothing declares it at launch. |
