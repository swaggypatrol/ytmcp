import type { AppSnapshot } from "@ytmcp/protocol";
import type { ClientTransportMode } from "../ipc/client.js";
import type { ProblemLog } from "./problems.svelte.js";

/** Colour of the toast's mark: Aqua for done, Lemon for unclear or unavailable, Peach for failed, Orchid for the AI DJ. */
export type ToastTone = "neutral" | "success" | "warning" | "danger" | "agent";

export interface ToastAction {
  readonly label: string;
  /** Runs with the toast it belongs to, so an action can find that toast's problem entry. */
  readonly run: (toast: Toast) => void;
  readonly testId?: string;
  /** The action was pressed and its outcome is on its way: shown pending, and it cannot be pressed again. */
  readonly pending?: boolean;
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
  /** What the problem log keeps of a warning or failure beyond the sentence. */
  readonly problem?: ToastProblem | null;
}

/** The parts of a warning or failure that Diagnostics shows under "Recent problems". */
export interface ToastProblem {
  /** The track or playlist title it was about. */
  readonly subject?: string | null;
  /** What the listener can do about it. */
  readonly advice?: string | null;
  /** Error code and message (defaults to the toast's `detail`). */
  readonly technical?: string | null;
  /** An account write whose outcome is unknown; it stays marked until checked. */
  readonly unknownOutcome?: boolean;
}

/** Warning and failure toasts are logged; the others only inform. */
function isProblem(toast: Pick<Toast, "tone">): toast is Pick<Toast, "tone"> & { tone: "warning" | "danger" } {
  return toast.tone === "warning" || toast.tone === "danger";
}

/** Why a toast's countdown is paused: the pointer is over it, or keyboard focus is inside it. */
export type ToastHoldReason = "hover" | "focus";

/** How long a transient toast stays, not counting time under the pointer or keyboard focus or while the window is hidden. */
export const TOAST_DWELL_MS = 3200;

/**
 * The one transient toast slot. A new toast replaces the current one; it leaves by itself after
 * `TOAST_DWELL_MS`, and the countdown stops while any hold reason is in effect (pointer over it,
 * focus inside it) and while the window is hidden; `release()` without a reason clears the
 * pointer and focus holds, for a toast that was replaced. Sticky problems are not stored here:
 * the host shows `current ?? sticky`, so a problem toast comes back once a transient one is gone.
 *
 * Every warning or failure goes into the problem log as it is shown (transient ones in `show`,
 * standing ones through `trackStanding`), so one replaced before anyone saw it can still be found.
 */
export class ToastStore {
  current = $state.raw<Toast | null>(null);
  /** The timed-out toast, so the host can distinguish expiry from a dismissal. */
  expiredId: string | null = null;
  readonly #log: ProblemLog | null;
  readonly #scopeOf: (() => string | null) | null;
  #seq = 0;
  #timer: ReturnType<typeof setTimeout> | null = null;
  #remaining = TOAST_DWELL_MS;
  #since = 0;
  #holds = new Set<ToastHoldReason>();
  #hidden = false;
  /** Ids of the standing problems already logged while they stay up. */
  #standing = new Set<string>();

  /** `scopeOf` reads the log's current scope, so a problem raised in the same update as an account change is logged under the new one. */
  constructor(log: ProblemLog | null = null, scopeOf: (() => string | null) | null = null) {
    this.#log = log;
    this.#scopeOf = scopeOf;
  }

  show(toast: Omit<Toast, "id">): void {
    const shown = { ...toast, id: `toast-${++this.#seq}` };
    this.#note(shown);
    this.current = shown;
    this.#remaining = TOAST_DWELL_MS;
    this.#arm();
  }

  /**
   * Logs the standing problems (account, connection, a failed refresh) that are up now, each
   * once per appearance; pass every candidate, null for one that is not up.
   */
  trackStanding(toasts: readonly (Toast | null)[]): void {
    const up = new Set<string>();
    for (const toast of toasts) {
      if (!toast || !isProblem(toast)) continue;
      up.add(toast.id);
      if (!this.#standing.has(toast.id)) this.#note(toast);
    }
    this.#standing = up;
  }

  /** Follows the log's scope (the account key); a new one starts empty and relogs what still stands. */
  syncScope(scope: string | null): void {
    if (this.#log?.sync(scope)) this.#standing.clear();
  }

  /** The window was hidden or shown again: the countdown waits while nobody can see the toast. */
  setHidden(hidden: boolean): void {
    if (hidden === this.#hidden) return;
    const wasPaused = this.#paused();
    this.#hidden = hidden;
    if (hidden && !wasPaused) this.#pause();
    else if (!this.#paused()) this.#arm();
  }

  #note(toast: Toast): void {
    if (!this.#log || !isProblem(toast)) return;
    if (this.#scopeOf) this.syncScope(this.#scopeOf());
    const problem = toast.problem ?? {};
    this.#log.add({
      toastId: toast.id,
      kind: toast.kind,
      tone: toast.tone,
      text: toast.text,
      subject: problem.subject ?? null,
      advice: problem.advice ?? null,
      technical: problem.technical ?? toast.detail ?? null,
      unknownOutcome: problem.unknownOutcome === true,
    });
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
    const wasPaused = this.#paused();
    this.#holds.add(reason);
    if (!wasPaused) this.#pause();
  }

  /** Ends one hold reason, or the pointer and focus holds when none is given (the window's stays). */
  release(reason?: ToastHoldReason): void {
    if (this.#holds.size === 0) return;
    if (reason === undefined) this.#holds.clear();
    else if (!this.#holds.delete(reason)) return;
    if (!this.#paused()) this.#arm();
  }

  #paused(): boolean {
    return this.#holds.size > 0 || this.#hidden;
  }

  /** Stops a running countdown, keeping the time it has left. */
  #pause(): void {
    if (this.#timer === null) return;
    this.#remaining -= Date.now() - this.#since;
    this.#disarm();
  }

  #arm(): void {
    this.#disarm();
    const toast = this.current;
    if (this.#paused() || !toast) return;
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
