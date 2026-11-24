import type { AppSnapshot } from "@ytmcp/protocol";
import type { ClientTransportMode } from "../ipc/client.js";

/** Colour of the toast's mark: Aqua for done, Lemon for unclear or unavailable, Peach for failed, Orchid for the AI DJ. */
export type ToastTone = "neutral" | "success" | "warning" | "danger" | "agent";

export interface ToastAction {
  readonly label: string;
  readonly run: () => void;
  readonly testId?: string;
}

/** One floating message (layer L4). It never enters the layout flow. */
export interface Toast {
  /** Stable for a sticky problem, so re-renders do not replay its entrance. */
  readonly id: string;
  /** What it reports; also its test id (`toast-<kind>`). */
  readonly kind: string;
  readonly tone: ToastTone;
  /** A short past-tense sentence. */
  readonly text: string;
  /** Technical detail for the tooltip; the full record is in Diagnostics. */
  readonly detail?: string | null;
  /** At most one action. */
  readonly action?: ToastAction | null;
}

/** Why a toast's countdown is paused: the pointer is over it, or keyboard focus is inside it. */
export type ToastHoldReason = "hover" | "focus";

/** How long a transient toast stays, not counting time under the pointer or keyboard focus. */
export const TOAST_DWELL_MS = 3200;

/**
 * The one transient toast slot. A new toast replaces the current one; it leaves by itself after
 * `TOAST_DWELL_MS`, and the countdown stops while any hold reason is in effect (pointer over it,
 * focus inside it); `release()` without a reason clears them all, for a toast that was replaced.
 * Sticky problems are not stored here: the host shows `current ?? sticky`, so a problem toast
 * comes back once a transient one is gone.
 */
export class ToastStore {
  current = $state.raw<Toast | null>(null);
  /** The timed-out toast, so the host can distinguish expiry from a dismissal. */
  expiredId: string | null = null;
  #seq = 0;
  #timer: ReturnType<typeof setTimeout> | null = null;
  #remaining = TOAST_DWELL_MS;
  #since = 0;
  #holds = new Set<ToastHoldReason>();

  show(toast: Omit<Toast, "id">): void {
    this.current = { ...toast, id: `toast-${++this.#seq}` };
    this.#remaining = TOAST_DWELL_MS;
    this.#arm();
  }

  /** Removes the current toast, or only the given one if it is still current. */
  dismiss(id?: string): void {
    if (!this.current || (id !== undefined && this.current.id !== id)) return;
    this.#disarm();
    this.current = null;
  }

  /** Pauses the countdown for one reason; it resumes only when no reason is left. */
  hold(reason: ToastHoldReason): void {
    if (this.#holds.has(reason)) return;
    const wasHeld = this.#holds.size > 0;
    this.#holds.add(reason);
    if (!wasHeld && this.#timer !== null) {
      this.#remaining -= Date.now() - this.#since;
      this.#disarm();
    }
  }

  /** Ends one hold reason, or all of them when none is given. */
  release(reason?: ToastHoldReason): void {
    if (this.#holds.size === 0) return;
    if (reason === undefined) this.#holds.clear();
    else if (!this.#holds.delete(reason)) return;
    if (this.#holds.size === 0) this.#arm();
  }

  #arm(): void {
    this.#disarm();
    const toast = this.current;
    if (this.#holds.size > 0 || !toast) return;
    this.#since = Date.now();
    this.#timer = setTimeout(() => {
      this.expiredId = toast.id;
      this.dismiss(toast.id);
    }, Math.max(0, this.#remaining));
  }

  #disarm(): void {
    if (this.#timer !== null) clearTimeout(this.#timer);
    this.#timer = null;
  }
}

/** Account and connection problems that hold a sticky toast until they are resolved. */
export type ProblemKind =
  | "unconnected"
  | "connection-error"
  | "offline"
  | "signed-out"
  | "auth-expired"
  | "auth-blocked"
  | "bridge-disconnected"
  | "unsupported";

export interface ProblemInput {
  readonly mode: ClientTransportMode;
  readonly connectionError: string | null;
  readonly snapshot: AppSnapshot | null;
}

/**
 * The one problem to show when several are true, most fundamental first: no core, then no
 * network, then the account, then the player page, then missing basic controls.
 */
export function currentProblem({ mode, connectionError, snapshot }: ProblemInput): ProblemKind | null {
  if (mode === "unconnected") return "unconnected";
  if (mode === "preload" && connectionError !== null) return "connection-error";
  if (!snapshot) return null;
  if (snapshot.networkState === "offline") return "offline";
  switch (snapshot.authState.state) {
    case "signedOut":
      return "signed-out";
    case "expired":
      return "auth-expired";
    case "blocked":
      return "auth-blocked";
    default:
      break;
  }
  const connection = snapshot.player.connection;
  if (connection === "disconnected" || connection === "incompatible" || connection === "recovering") {
    return "bridge-disconnected";
  }
  if (snapshot.capabilities.playPause === "unsupported" || snapshot.capabilities.seek === "unsupported") {
    return "unsupported";
  }
  return null;
}
