# post-doorbell

A push wake-up for agents whose harness cannot ring itself.

Claude Code sessions get woken by new `post` mail through the harness Monitor
tool. Codex CLI sessions have no Monitor: their mail hook is activity-gated, so
an idle session learns about mail only when someone happens to prompt it. This
daemon closes that gap — it watches `post` and pokes the agent through
`herdr agent prompt` when something arrives.

## What the poke contains: nothing

The notice carries counts and channel names only. No subject, no sender, no
body. Mail content reaching a model through a door the model did not open is
how an agent ends up following instructions nobody vetted, and this daemon
opens exactly that kind of door. So the door stays empty: the wake says "you
have mail", and the agent reads it through its own vetted path. On Codex that
composes neatly — this poke lands, and Codex's own `UserPromptSubmit` mail hook
supplies the metadata. Push and content arrive through different doors, and the
one built here carries nothing an attacker could write.

`render()` takes the pending counts and nothing else, and a test asserts its
signature so a future body parameter fails the suite rather than shipping.

## Failure modes it is built against

Every one of these was a real bug caught in review or testing, not a
hypothetical:

- **A doorbell that cannot ring.** `--wake-on` refuses unknown or empty reason
  tokens instead of starting a daemon that silently matches nothing. The first
  version defaulted to a token `post` never emits.
- **A wake that gets lost.** The watermark advances only after a delivered
  poke. Advancing it on a rejected prompt retires a message nobody was told
  about.
- **An ambiguous batch.** `post` reports `mixed` when a batch holds more than
  one reason. A mention-only filter that dropped it would lose the exact
  message the filter exists to catch, so ambiguity always rings.
- **A line that is read but not seen.** `select()` watches a file descriptor,
  so a buffered text stream that pulls a whole chunk above it hides every line
  but the first until the next write. A multi-room scan emits one line per room
  in a single write, so this was the common case. The loop reads the raw fd and
  splits lines itself.
- **A dead watcher.** If `post watch` exits, the daemon logs its exit code and
  exits non-zero so the supervisor restarts it, rather than living on as a
  healthy-looking process that can never ring.
- **A room that does not exist.** A typo'd `--room` makes `post watch` warn
  once and then watch an empty mailbox forever. Startup checks the requested
  rooms against `post rooms` and refuses. An unreadable listing degrades to
  unchecked, never to refusal — unreadable means unchecked, not absent.
- **Its own voice.** A doorbell wakes one agent about that agent's mail, so
  the rooms it is given are that agent's own identities and their sends are not
  news to it. It declares them with `post watch --own`, and detects whether the
  installed `post` has that flag rather than assuming it — an older `post`
  rejects an unknown flag outright, and a doorbell that dies on a flag is worse
  than one that is occasionally noisy. When the flag is absent it says so.
- **A backlog stampede.** Startup primes the watermark past existing unread
  mail. The first live run rang for 226 backlog messages.

## Running it

Nothing is installed. The template unit expects the script at
`/usr/local/bin/post-doorbell`:

    systemctl --user enable --now post-doorbell@<agent-name>

`Restart=always` with a 10s backoff and no start-rate limit: systemd's default
gives up after five restarts and leaves the unit dead, which for a doorbell
means it stops ringing and nothing says so.

`<agent-name>` must be a **named** herdr agent. `herdr agent list` omits the
`name` key entirely for panes that were never named, and `find_agent` matches on
it, so an unnamed pane refuses to start. At first install on this cell, three of
four live agents carried no name at all, so this is the common case rather than
the edge one. The refusal names the fix and lists the agents that do have names:

    herdr agent rename <pane-id> <agent-name>

`herdr agent list` shows the pane ids.

## Tests

    python -m unittest discover -p 'test_*.py'

Seventeen tests. Each was watched red before being kept.
