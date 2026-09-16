/* Verified acceptance data. Update this file only from examined receipts.
 * Runtime source and served documentation commits are deliberately distinct.
 * Full record: acceptance.json. */
window.POST_RECEIPTS = {
  "run": {
    "recordedAt": "2026-09-16T16:11:52+00:00",
    "baseCommit": null,
    "headCommit": "a500bd22121aa6bcce7e8942dd2a087332ed4403",
    "runtimeCommit": "a500bd22121aa6bcce7e8942dd2a087332ed4403",
    "skillDocsCommit": "f1e3eae3e51938568b574d88833471919ade20d4",
    "note": "Runtime a500bd2; skill/docs f1e3eae. See acceptance.json for exact hashes, native message ids, limits, and test footprint."
  },
  "checks": [
    {
      "id": "build-mac",
      "label": "Release build, Mac",
      "detail": "Locked release build from the frozen source commit.",
      "status": "pass",
      "evidence": "a500bd2; cargo build --release --locked; installed SHA c7ad93cb842e. assets/acceptance.json: build_and_install.mac; install-mac-a500bd2.log."
    },
    {
      "id": "build-devbox",
      "label": "Release build, devbox",
      "detail": "Locked release build as trey-agent, not the root-owned system copy.",
      "status": "pass",
      "evidence": "a500bd2; installed SHA 7489f9188940. assets/acceptance.json: build_and_install.devagent; install-devagent-a500bd2.log."
    },
    {
      "id": "gate",
      "label": "Full repository gate on the integrated candidate",
      "detail": "The project gate at the frozen runtime SHA, not separate lane heads.",
      "status": "pass",
      "evidence": "a500bd2: 536 Cargo tests; fmt/clippy; Node hooks/launcher; Python doorbell; schema. Strict build-SHA acceptance 34/34. gate-freeze-a500bd2.log."
    },
    {
      "id": "install-mac",
      "label": "Installed runtime, Mac",
      "detail": "The PATH binary and the installed hook copies, exercised against isolated stores.",
      "status": "pass",
      "evidence": "~/.local/bin/post: a500bd2, store 2, four capabilities; 34/34 installed smoke. Four adapters passed; unsupported end events skipped explicitly. assets/acceptance.json."
    },
    {
      "id": "install-devbox",
      "label": "Installed runtime, devbox",
      "detail": "The trey-agent PATH binary; /usr/local/bin/post was left alone.",
      "status": "pass",
      "evidence": "~/.local/bin/post: a500bd2, store 2, same four capabilities; 34/34 installed smoke. Claude/Codex/Grok passed; Cursor absent. assets/acceptance.json."
    },
    {
      "id": "skill-sync",
      "label": "Reviewed skill served on both hosts",
      "detail": "The docs revision is recorded separately from the runtime build.",
      "status": "pass",
      "evidence": "Docs f1e3eae; 32 files on each host, six served links, 192 served-file hash comparisons. SKILL.md SHA 380fc761cc02. installed-docs-verification.json."
    },
    {
      "id": "two-participants",
      "label": "Two participants, one workspace, one repository",
      "detail": "Astra and Fable received the same message; reading one copy did not consume the other.",
      "status": "pass",
      "evidence": "1582d5: codex-75ce6b09 read first; claude-e2ef843c still unread, then read. Both cursors hold the id; canonical file and receipt unchanged. assets/acceptance.json: native_pair."
    },
    {
      "id": "self-suppression",
      "label": "Self-suppression by participant, not by name",
      "detail": "Each native agent sent to the shared workspace; only its sibling was notified.",
      "status": "pass",
      "evidence": "83a707: Astra to Fable only. 268f74: Fable to Astra only. Both delivered/read, no sender unread entry. Real-session hooks rang. assets/acceptance.json: native_pair."
    },
    {
      "id": "channel-state",
      "label": "Per-participant channel membership and read state",
      "detail": "Leaving does not remove a peer; a later hook does not silently rejoin the leaver.",
      "status": "pass",
      "evidence": "P13-07 and CHANNEL-own-leave passed on both installed binaries. Installed hook fixtures preserved the participant left set. installed-mac-*.json / installed-devagent-LnQqUq/."
    },
    {
      "id": "no-injection",
      "label": "Preview is non-affiliating; voice loading is explicit",
      "detail": "A preview loaded no voice body and changed no participant or mailbox bytes.",
      "status": "pass",
      "evidence": "Installed preview proof passed; sentinel voice/body absent from all four Mac and three devagent hook outputs. P13-06 passed on both hosts. installed-mac-edge-preview.json."
    },
    {
      "id": "restart",
      "label": "Binding survives a hook-process restart",
      "detail": "Separate hook processes map one conversation key to the same participant id.",
      "status": "pass",
      "evidence": "All four installed Mac adapters passed fresh-process identity convergence; hook dedupe passed on both hosts. Not a claim about continuity of experience. installed-mac-edge-*.json."
    },
    {
      "id": "cross-host",
      "label": "Cross-host delivery still works",
      "detail": "Harmless workspace mail crossed the unchanged bridge in both directions and was read at its destination.",
      "status": "pass",
      "evidence": "7606cf: Mac to post-devbox. 21af36: devagent to claude-space. Digests match receipts; both consumed. Initial topology failure and 14 incidental legacy routing receipts disclosed in assets/acceptance.json."
    }
  ]
};
