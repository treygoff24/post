// Pi extension: metadata-only Post notices, using the shared lifecycle core.
import path from "node:path";
import { randomUUID } from "node:crypto";
import { runMailHook, safeIdentityPart } from "./mail-hook-core.mjs";

const adapter = {
  harness: "pi",
  envPrefix: "POST_PI_HOOK",
  stateDirName: "post-pi-mail",
  text: { waiting: "Unread agent mail", manualCheck: "Check manually with: post inbox" },
  // Pi does not export an ambient Post conversation key to shell tools.
  lazyMint: false,
  bindingLine: (id) => safeIdentityPart(id)
    ? `[post] participant ${id}; prefix Post commands with POST_PARTICIPANT=${id}` : null,
  parse(input) {
    if (!new Set(["start", "prompt", "end"]).has(input.phase)) return null;
    if (typeof input.session !== "string" || !input.session.trim()) return null;
    if (typeof input.cwd !== "string" || !path.isAbsolute(input.cwd)) return null;
    return { event: input.phase, phase: input.phase, sessionRaw: input.session, cwd: input.cwd };
  },
  payload: (_event, context) => ({ hookSpecificOutput: { additionalContext: context } }),
};

export default function postMail(pi) {
  let active = null;
  let timer = null;
  let checking = false;
  const stop = () => {
    if (timer !== null) clearInterval(timer);
    timer = null;
    active = null;
  };

  function check(ctx, phase, wake = false) {
    if (checking || (phase !== "end" && !ctx.isIdle())) return;
    checking = true;
    let delivered = false;
    try {
      runMailHook(adapter, {
        input: { phase, session: ctx.sessionManager.getSessionId(), cwd: ctx.cwd },
        deliver(payload) {
          const content = payload?.hookSpecificOutput?.additionalContext;
          if (!content) return true;
          const receipt = randomUUID();
          // The public sendMessage API returns void, not an acceptance promise.
          // Append without starting a turn, then prove the exact message entered
          // this branch before the shared core commits dedupe/activation state.
          pi.sendMessage({ customType: "post-mail", content, display: true, details: { receipt } }, { triggerTurn: false });
          const accepted = ctx.sessionManager.getBranch().some((entry) =>
            entry.type === "custom_message" && entry.customType === "post-mail" &&
            entry.details?.receipt === receipt && entry.content === content);
          delivered ||= accepted;
          return accepted;
        },
      });
      if (wake && delivered && ctx.isIdle() && !ctx.hasPendingMessages()) {
        // Wake is a separate best-effort action. A failed model/provider start
        // cannot erase the already accepted, metadata-only notice from context.
        pi.sendMessage({ customType: "post-wake", content: "[post] Inbox notice available in session context.", display: false }, { triggerTurn: true });
      }
    } catch {
      // Host errors never break Pi. Failed acceptance keeps mail eligible.
    } finally {
      checking = false;
    }
  }

  pi.on("session_start", (_event, ctx) => {
    stop();
    active = ctx;
    check(ctx, "start");
    // Polling is explicit opt-in; the default has no idle model turns or timer.
    const interval = Number(process.env.POST_PI_IDLE_POLL_MS);
    if (Number.isFinite(interval) && interval >= 5000) {
      timer = setInterval(() => {
        if (active === ctx && ctx.isIdle() && !ctx.hasPendingMessages()) check(ctx, "prompt", true);
      }, interval);
      timer.unref?.();
    }
  });
  // before_agent_start is already marked busy by Pi. Input is the earlier
  // boundary where an idle session can synchronously accept custom context.
  pi.on("input", (_event, ctx) => check(ctx, "prompt"));
  pi.on("session_shutdown", (_event, ctx) => {
    stop();
    check(ctx, "end");
  });
}
