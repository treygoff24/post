// Test helper (not installed): the environment a hook test starts its child from.
//
// The gate usually runs inside an agent session that is bound to post, and that
// session carries its identity in these variables. A test that spread
// process.env into a hook child would run the hook as that session (an explicit
// POST_PARTICIPANT, for one, makes the hook refuse to bind the payload's key),
// so its result would depend on who ran the gate. Tests start from this
// environment and set any identity they need explicitly.
export const SESSION_IDENTITY_ENV = [
  "POST_PARTICIPANT",
  "POST_HOST_PARTICIPANT",
  "POST_HARNESS",
  "POST_FROM",
  "POST_SENDER_ADDRESS",
  "POST_FRAMING",
  "CLAUDE_CODE_SESSION_ID",
  "CLAUDE_PID",
  "CODEX_THREAD_ID",
  "CODEX_SESSION_ID",
  "DELEGATE_RUN_ID",
];

export function withoutSessionIdentity(env = process.env) {
  const clean = { ...env };
  for (const key of SESSION_IDENTITY_ENV) delete clean[key];
  return clean;
}
