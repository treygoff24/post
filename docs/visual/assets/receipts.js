/* ===========================================================================
 * THE UPDATE POINT.
 *
 * This is the only file to edit when real acceptance evidence lands. Nothing
 * else on the page knows anything about build or runtime status.
 *
 * Rules for whoever edits it:
 *   - `status` is exactly one of "pending", "pass", "fail".
 *   - A row stays "pending" until somebody has actually watched the check run.
 *     A plan, an intention, or a green lane report is not a pass.
 *   - `evidence` is what a reader could go and check for themselves: a commit
 *     sha, a build sha, a host name, a command and its result, a path. Leave it
 *     empty on a pending row rather than writing a placeholder.
 *   - `run` describes the run as a whole. `overall` is computed, not stored:
 *     any "fail" makes the header fail, any remaining "pending" makes it
 *     pending, and only an all-pass table turns it green.
 * ======================================================================== */

window.POST_RECEIPTS = {
  run: {
    recordedAt: null,          // ISO 8601 string once a run exists
    baseCommit: null,
    headCommit: null,
    note: "No acceptance run recorded."
  },

  checks: [
    {
      id: "build-mac",
      label: "Release build, Mac",
      detail: "cargo build --release on the Mac",
      status: "pending",
      evidence: ""
    },
    {
      id: "build-devbox",
      label: "Release build, devbox",
      detail: "cargo build --release in the trey-agent cell",
      status: "pending",
      evidence: ""
    },
    {
      id: "gate",
      label: "Full repository gate on the integrated candidate",
      detail: "The project's own required checks, run at the integration HEAD rather than per lane",
      status: "pending",
      evidence: ""
    },
    {
      id: "install-mac",
      label: "Installed runtime, Mac",
      detail: "~/.local/bin/post reports its build sha and capability flags",
      status: "pending",
      evidence: ""
    },
    {
      id: "install-devbox",
      label: "Installed runtime, devbox",
      detail: "~/.local/bin/post for the agent user reports the same build sha",
      status: "pending",
      evidence: ""
    },
    {
      id: "two-participants",
      label: "Two participants, one workspace, one repository",
      detail: "Astra and Fable in this repository as distinct participants: both receive one common addressed message, and neither read consumes the other's copy",
      status: "pending",
      evidence: ""
    },
    {
      id: "self-suppression",
      label: "Self-suppression by participant, not by name",
      detail: "The sending participant's own notification is suppressed and a sibling's is not",
      status: "pending",
      evidence: ""
    },
    {
      id: "channel-state",
      label: "Per-participant channel membership and read state",
      detail: "One participant leaving a channel does not remove another, and a later hook does not silently rejoin it",
      status: "pending",
      evidence: ""
    },
    {
      id: "no-injection",
      label: "Preview is non-affiliating; voice loading is explicit",
      detail: "Showing a lineage without --voices loads no voice body, and previewing writes nothing about the participant",
      status: "pending",
      evidence: ""
    },
    {
      id: "restart",
      label: "Notification binding survives a restart",
      detail: "A bounded post-restart check that the participant still resolves to the same id",
      status: "pending",
      evidence: ""
    },
    {
      id: "cross-host",
      label: "Cross-host delivery still works",
      detail: "A harmless real exchange between the Mac and the devbox on the upgraded runtime",
      status: "pending",
      evidence: ""
    }
  ]
};
