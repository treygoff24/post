// Resolve the Node binary that hook commands, plists, and units should invoke.
//
// process.execPath is absolute and correct today, but on a package-managed Node
// it is VERSION-PINNED: Homebrew reports /opt/homebrew/Cellar/node/<version>/bin/node
// rather than the /opt/homebrew/bin/node symlink it keeps pointing at the current
// release. Baking the Cellar path into an installed hook means the next
// `brew upgrade node` deletes that directory and every hook starts exiting 127
// (command not found) at once, silently.
//
// That is not hypothetical: on 2026-09-15 a 26.5.0_1 -> 26.8.2 upgrade broke the
// Codex (3 hooks), Cursor (3) and Grok (1) adapters plus the codex-doorbell
// launchd agent on the same machine within the same second.
//
// So: prefer a stable alias for the SAME binary, and only that. The realpath
// equality check is the whole safety argument — a machine with several Node
// installs (fnm, nvm, asdf, a distro package alongside Homebrew) has same-named
// binaries that are DIFFERENT versions, and silently redirecting a hook to one of
// those would be a worse bug than the one this fixes. When nothing matches, we
// return process.execPath and behave exactly as before.

import fs from "node:fs";
import os from "node:os";
import path from "node:path";

// Stable aliases a package manager keeps current, most-preferred first.
//
// mise earns an entry because its `installs/node/latest` is an ordinary symlink
// to the current version directory, so the realpath check below validates it
// exactly like Homebrew's. Its `shims/node` is deliberately NOT here: a shim
// resolves the version from whatever mise config applies at run time, so the
// binary a hook gets would depend on the directory it fires in.
export const STABLE_CANDIDATES = [
  "/opt/homebrew/bin/node", // Homebrew, Apple silicon
  "/usr/local/bin/node", // Homebrew on Intel, manual installs
  "/home/linuxbrew/.linuxbrew/bin/node", // Homebrew on Linux
  path.join(os.homedir(), ".local/share/mise/installs/node/latest/bin/node"), // mise
  "/usr/bin/node", // distro packages
];

/**
 * @param {string} [execPath] the running interpreter; defaults to process.execPath
 * @param {string[]} [candidates] stable aliases to consider, in preference order
 * @returns {string} an absolute path to the same binary, version-independent when possible
 */
export function stableNodePath(execPath = process.execPath, candidates = STABLE_CANDIDATES) {
  let realExec;
  try {
    realExec = fs.realpathSync(execPath);
  } catch {
    // Cannot resolve the interpreter we are running under: do not guess.
    return execPath;
  }
  for (const candidate of candidates) {
    if (candidate === execPath) return candidate;
    try {
      // Same file, reached by a name that survives upgrades?
      if (fs.realpathSync(candidate) !== realExec) continue;
      fs.accessSync(candidate, fs.constants.X_OK);
      return candidate;
    } catch {
      // Missing, unreadable, or not executable: try the next alias.
    }
  }
  return execPath;
}
