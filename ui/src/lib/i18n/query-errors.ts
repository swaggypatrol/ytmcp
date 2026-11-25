import type { PlayerOp, QueryError, QueryErrorCode } from "@ytmcp/protocol";
import { t } from "./index.svelte.js";

/** Use the UI's own words. Callers supply a fallback suited to the task that failed. */
export function queryErrorMessage(code: QueryErrorCode | null | undefined, fallback: string): string {
  switch (code) {
    case "AUTH_REQUIRED":
    case "AUTH_EXPIRED":
      return t.states.authExpiredDescription;
    case "ACCOUNT_MISMATCH":
      return t.states.accountMismatchDescription;
    case "NETWORK_OFFLINE":
      return t.states.offlineRetry;
    case "TIMEOUT":
      return t.states.requestTimeout;
    case "BUSY":
    case "RATE_LIMITED":
      return t.states.tryAgainSoon;
    case "UNSUPPORTED_CAPABILITY":
      return t.gates.unsupported;
    case "UNAVAILABLE_TRACK":
      return t.availability.unavailable;
    default:
      return fallback;
  }
}

/**
 * The words for a failed player command (`op` null when the failed request is not known). A play,
 * or a toggle that meant play, failing because no track could be loaded is not about an
 * unavailable song: it says there is nothing to play yet.
 */
export function commandFailureMessage(op: PlayerOp | null | undefined, code: QueryErrorCode | null | undefined): string {
  if ((op === "play" || op === "toggle") && code === "UNAVAILABLE_TRACK") return t.player.noTrackToPlay;
  return op ? t.toast.commandFailed[op] : t.toast.commandFailedGeneric;
}

/** Core detail is available in tooltips and Diagnostics. */
export function queryErrorDetail(error: QueryError | null | undefined): string | undefined {
  return error ? `${error.code}${t.common.colon}${error.safeMessage}` : undefined;
}
