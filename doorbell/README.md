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
- **A wake that gets lost.** Individual events use exact namespace-and-ID keys,
  not timestamp watermarks: a later direct-mail ID can sort below an earlier
  one. Keys are acknowledged only after a successful current snapshot and poke.
  Failed snapshots and rejected prompts remain pending for retry.
- **Stale mail after a busy turn.** The daemon rereads current unread events
  after the agent settles, then counts only unacknowledged eligible keys. A
  consumed trigger does not produce a notice. Consumed acknowledged keys are
  pruned after a successful cycle; a watcher-local seen set suppresses buffered
  duplicate triggers.
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
- **A backlog stampede.** Startup primes exact keys for existing unread mail.
  An unavailable initial snapshot is retried without crashing; backlog may ring
  once when the scan recovers. Legacy watermark state is ignored. State version
  2 persists only current acknowledged keys, never message content.

Names containing Unicode, spaces, or punctuation outside the simple notice
alphabet remain eligible but appear as `[non-simple name]`. Unreadable delivery
IDs are opaque and never rendered. Notices remain capped at 1,500 characters.
Current Post unreadable-channel events omit the channel name. Two such events
with the same room and filename-derived ID are indistinguishable; unique
delivery tracking for that case needs an upstream event field and remains open.

## Running it

Prerequisites: Python 3, `post`, and `herdr` must be available. The user unit
loads `post-doorbell` from PATH, preferring `~/.local/bin`; an existing
`/usr/local/bin/post-doorbell` remains a fallback. No administrator install is
required.

From this checkout:

    install -d "$HOME/.local/bin"
    install -m 755 post-doorbell "$HOME/.local/bin/post-doorbell"
    install -d -m 700 "$HOME/.config/systemd/user"
    install -m 644 post-doorbell@.service "$HOME/.config/systemd/user/post-doorbell@.service"
    systemctl --user daemon-reload

    systemctl --user enable --now post-doorbell@<agent-name>

Replace `<agent-name>` with the named target before running the last command.
Do not enable this service alongside another doorbell for the same agent.
For an existing instance, repeat the install and daemon-reload commands, then
run `systemctl --user restart post-doorbell@<agent-name>` for only your target.
Other running instances keep their loaded copy until their owners restart them.

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

The systemd user template sets PATH to `%h/.local/bin:/usr/local/bin:/usr/bin:/bin`;
it does not inherit your interactive shell setup. Install `post` and `herdr` in
one of those directories, or override PATH with a user-unit drop-in. Startup
refuses with an actionable error if either executable is missing.

    python -m unittest discover -p 'test_*.py'

The suite covers watch delivery, failure handling, and startup dependencies.
