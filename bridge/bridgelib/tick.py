"""Quiet-tick fingerprint and completed-full-tick checkpoint helpers."""

import json
import os
import stat

from . import common


FINGERPRINT_FILE = "trigger-fingerprint.json"
REMOTE_HEADS_FILE = "remote-heads.json"


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

        status = git.run(["status", "--porcelain"], check=False)
        if status.returncode != 0:
            return None, None
        git_dir_result = git.run(["rev-parse", "--git-dir"], check=False)
        if git_dir_result.returncode != 0:
            return None, None
        git_dir = common.destination(
            settings.repo,
            git_dir_result.stdout.strip(),
        ) if not os.path.isabs(git_dir_result.stdout.strip()) else os.path.abspath(
            git_dir_result.stdout.strip()
        )
        git_dir_path = settings.repo.__class__(git_dir)
        if any(
            (git_dir_path / name).exists()
            for name in ("rebase-merge", "rebase-apply", "MERGE_HEAD")
        ):
            return None, None
        head = git.rev("HEAD")
        remote = git.rev(f"origin/machines/{settings.host}")
        pending_empty = all(
            _directory_empty(common.destination(settings.root, "bridge", name))
            for name in ("chan-joins-pending", "chan-joins-held")
        ) and _diverged_empty(settings)
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
    streak = prior_health.get("quiet_streak", 0)
    if not isinstance(streak, int) or streak >= 3:
        return fingerprint, heads, False
    if not fingerprint["pending_empty"] or not fingerprint["worktree_clean"]:
        return fingerprint, heads, False
    if not fingerprint["head_matches_remote"] or fingerprint["fence"]:
        return fingerprint, heads, False
    return fingerprint, heads, fingerprint == read_fingerprint(settings)


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
