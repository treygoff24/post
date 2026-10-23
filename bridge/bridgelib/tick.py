"""Quiet-tick fingerprint and completed-full-tick checkpoint helpers."""

import json
import os
import stat
import time

from . import common


FINGERPRINT_FILE = "trigger-fingerprint.json"
REMOTE_HEADS_FILE = "remote-heads.json"

# Consecutive quiet ticks before one full tick is forced as a safety net.
QUIET_STREAK_MAX = 6
# Longest effective gap between ticks while the bridge stays quiet. The timer
# still fires every interval; ticks inside the gap exit before any git.
BACKOFF_CAP_SECONDS = 60
# A timer fires a little early or late; a tick that is almost due runs.
BACKOFF_SLACK_SECONDS = 2


def effective_interval(interval, streak):
    """Expected seconds until the next tick that does work.

    ``interval`` is the timer's period (None: unknown, no backoff). The gap
    doubles with each consecutive quiet tick up to BACKOFF_CAP_SECONDS and
    never goes below the timer's own period.
    """
    if interval is None:
        return None
    if not isinstance(streak, int) or streak < 1:
        return interval
    cap = max(interval, BACKOFF_CAP_SECONDS)
    return min(cap, interval * (2 ** (streak - 1)))


def _metadata(path):
    try:
        value = path.lstat()
    except FileNotFoundError:
        return None
    if stat.S_ISLNK(value.st_mode):
        raise OSError(f"fingerprint input is a symlink: {path}")
    return [value.st_mtime_ns, value.st_size]


def _directory_empty(path):
    try:
        return next(os.scandir(str(path)), None) is None
    except FileNotFoundError:
        return True


def _diverged_empty(settings):
    path = common.destination(settings.root, "bridge", "chan-diverged.json")
    try:
        value = common.load_json_bytes(common.open_regular(path, 1024 * 1024), str(path))
    except FileNotFoundError:
        return True
    return value == []


def _pending_empty(settings):
    return all(
        _directory_empty(common.destination(settings.root, "bridge", name))
        for name in ("chan-joins-pending", "chan-joins-held")
    ) and _diverged_empty(settings)


def local_paths(settings):
    """Metadata inputs of the fingerprint that need no git to read."""
    paths = {
        "config.json": common.destination(settings.root, "bridge", "config.json"),
        "rooms.json": common.destination(settings.root, "rooms.json"),
        "rules.json": common.destination(settings.root, "rules.json"),
        "archive": common.destination(settings.root, "archive"),
        "channels": common.destination(settings.root, "channels"),
    }
    channel_root = paths["channels"]
    if channel_root.is_dir() and not channel_root.is_symlink():
        for entry in sorted(os.scandir(str(channel_root)), key=lambda item: item.name):
            if entry.name == ".channels.lock":
                continue
            directory = common.destination(channel_root, entry.name)
            paths[f"channels/{entry.name}"] = directory
            paths[f"channels/{entry.name}/messages"] = common.destination(
                directory, "messages"
            )
    return paths


def probe(settings, git):
    """Return ``(fingerprint, advertised_heads)`` or ``(None, None)`` on failure."""
    try:
        result = git.run(
            [
                "ls-remote",
                "--heads",
                "origin",
                "refs/heads/machines/*",
                "refs/heads/registry",
            ],
            check=False,
            network=True,
        )
        if result.returncode != 0:
            return None, None
        heads = {}
        for line in result.stdout.splitlines():
            oid, separator, ref = line.partition("\t")
            if not separator or not oid or not ref.startswith("refs/heads/"):
                return None, None
            heads[ref] = oid

        paths = local_paths(settings)

        status = git.run(["status", "--porcelain"], check=False)
        if status.returncode != 0:
            return None, None
        # One process for the git dir and both tips; a nonzero exit (an
        # unborn HEAD, a missing remote-tracking ref) re-asks one at a time.
        remote_ref = f"origin/machines/{settings.host}"
        combined = git.run(
            ["rev-parse", "--git-dir", "HEAD", remote_ref], check=False
        )
        lines = combined.stdout.splitlines()
        if combined.returncode == 0 and len(lines) == 3:
            git_dir_text, head, remote = (line.strip() for line in lines)
            # The full tick that follows asks for both again.
            git.rev_cache.setdefault("HEAD", head)
            git.rev_cache.setdefault(remote_ref, remote)
        else:
            git_dir_result = git.run(["rev-parse", "--git-dir"], check=False)
            if git_dir_result.returncode != 0:
                return None, None
            git_dir_text = git_dir_result.stdout.strip()
            head = git.rev("HEAD")
            remote = git.rev(remote_ref)
        git_dir = common.destination(
            settings.repo, git_dir_text
        ) if not os.path.isabs(git_dir_text) else os.path.abspath(git_dir_text)
        git_dir_path = settings.repo.__class__(git_dir)
        if any(
            (git_dir_path / name).exists()
            for name in ("rebase-merge", "rebase-apply", "MERGE_HEAD")
        ):
            return None, None
        pending_empty = _pending_empty(settings)
        fingerprint = {
            "heads": heads,
            "fence": common.destination(settings.root, ".post-arx.json").exists(),
            "paths": {name: _metadata(path) for name, path in sorted(paths.items())},
            "pending_empty": pending_empty,
            "worktree_clean": status.stdout == "",
            "head_matches_remote": head is not None and head == remote,
        }
        return fingerprint, heads
    except (OSError, common.ConfigError, common.TickError, UnicodeError):
        return None, None


def read_fingerprint(settings):
    path = common.destination(settings.root, "bridge", FINGERPRINT_FILE)
    try:
        value = common.load_json_bytes(common.open_regular(path, 1024 * 1024), str(path))
    except (FileNotFoundError, common.ConfigError):
        return None
    return value if isinstance(value, dict) else None


def quiet_candidate(settings, git, prior_health):
    """Return ``(fingerprint, heads, quiet)`` for this tick's start-of-tick probe.

    The probe is returned whatever the verdict. A full tick consumes the world
    as of this probe and persists it at step 17 (SPEC-v2 §Tick order), so any
    change landing after this point correctly forces the next tick full; a
    fresh probe at step 17 would instead record state this tick never read.
    """
    fingerprint, heads = probe(settings, git)
    if fingerprint is None or prior_health.get("ok") is not True:
        return fingerprint, heads, False
    if "attention" not in prior_health:
        # Written by a bridge that predates the attention list: run one full
        # tick so an upgrade acts on standing conditions (a bounce) at once.
        return fingerprint, heads, False
    streak = prior_health.get("quiet_streak", 0)
    if not isinstance(streak, int) or streak >= QUIET_STREAK_MAX:
        return fingerprint, heads, False
    if not fingerprint["pending_empty"] or not fingerprint["worktree_clean"]:
        return fingerprint, heads, False
    if not fingerprint["head_matches_remote"] or fingerprint["fence"]:
        return fingerprint, heads, False
    return fingerprint, heads, fingerprint == read_fingerprint(settings)


def skip_tick(settings, prior_health, now=None):
    """True when this timer firing can exit before any git process.

    Only after a quiet tick has lengthened the gap: the prior health is a
    healthy quiet one, its ``interval_s`` says how long the bridge asked to be
    left alone, that time has not passed, and nothing the fingerprint reads
    locally (config, rooms, rules, archive, channels, pending joins, the
    fence) differs from the last completed full tick. Anything else, or any
    error reading, runs the tick.
    """
    if prior_health.get("ok") is not True or prior_health.get("quiet") is not True:
        return False
    streak = prior_health.get("quiet_streak")
    if not isinstance(streak, int) or not 0 < streak < QUIET_STREAK_MAX:
        return False
    # Health from a bridge that predates the attention list runs one full tick
    # (quiet_candidate's rule), and a letter still in retry is only retried by
    # a tick that runs: neither waits out a backoff gap.
    if "attention" not in prior_health:
        return False
    pmail_health = prior_health.get("pmail")
    if isinstance(pmail_health, dict) and pmail_health.get("retry"):
        return False
    base = settings.interval_seconds
    gap = effective_interval(base, streak)
    if gap is None or gap <= base:
        return False
    ticked = common.epoch_from_iso(prior_health.get("ts"))
    now = time.time() if now is None else now
    if ticked is None or not 0 <= now - ticked < gap - BACKOFF_SLACK_SECONDS:
        return False
    stored = read_fingerprint(settings)
    if not stored:
        return False
    try:
        paths = {name: _metadata(path) for name, path in sorted(local_paths(settings).items())}
        fence = common.destination(settings.root, ".post-arx.json").exists()
        pending_empty = _pending_empty(settings)
    except (OSError, common.ConfigError, UnicodeError):
        return False
    return (
        not fence
        and stored.get("paths") == paths
        and stored.get("pending_empty") is True
        and pending_empty
        and stored.get("fence") is False
    )


def persist_completed(settings, fingerprint, heads):
    """Persist both quiet-tick checkpoints after a completed full tick."""
    if fingerprint is None or heads is None:
        return False
    bridge = common.destination(settings.root, "bridge")
    common.atomic_replace(
        common.destination(bridge, FINGERPRINT_FILE),
        (json.dumps(fingerprint, sort_keys=True) + "\n").encode("utf-8"),
        settings.root,
    )
    common.atomic_replace(
        common.destination(bridge, REMOTE_HEADS_FILE),
        (json.dumps(heads, sort_keys=True) + "\n").encode("utf-8"),
        settings.root,
    )
    return True
