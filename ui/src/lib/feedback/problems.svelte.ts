/** Most problems kept; the oldest goes first, an unchecked unknown write last. */
export const PROBLEM_LOG_LIMIT = 20;

/** One failure or unclear outcome, kept so it can be found again after its toast has gone. */
export interface ProblemEntry {
  readonly id: string;
  /** The toast that reported it (`Toast.id`); a standing problem keeps the same one. */
  readonly toastId: string;
  readonly kind: string;
  readonly tone: "warning" | "danger";
  /** What happened, as the toast said it. */
  readonly text: string;
  /** What it was about: the track or playlist title, when there is one. */
  readonly subject: string | null;
  /** What the listener can do about it, when there is something. */
  readonly advice: string | null;
  /** The technical detail: error code and message, or the message alone. */
  readonly technical: string | null;
  readonly atMs: number;
  /** An account write whose outcome is unknown: stays marked until the listener has checked it. */
  readonly unknownOutcome: boolean;
  readonly checked: boolean;
}

export type ProblemInput = Omit<ProblemEntry, "id" | "atMs" | "checked">;

/**
 * The recent problems shown in Diagnostics: a bounded, UI-only log for one account (the catalog
 * account key). Another account or a sign-out starts it empty, so one account's problems never
 * show under another. A core restart keeps it: an unknown write is exactly what a restart can
 * leave behind, and it must stay until the listener has checked it.
 */
export class ProblemLog {
  /** Newest first. */
  entries = $state.raw<readonly ProblemEntry[]>([]);
  #scope: string | null = null;
  #seq = 0;

  /**
   * Starts an empty log when the account key changes; true if it did. Null (no state from the
   * core, as while it restarts) keeps the log as it is.
   */
  sync(scope: string | null): boolean {
    if (scope === null || scope === this.#scope) return false;
    this.#scope = scope;
    this.entries = [];
    return true;
  }

  /**
   * Adds an entry at the top. Past the limit the oldest entry goes, except that an unknown
   * write not yet checked outlives every other entry; only a log full of those drops the
   * oldest of them.
   */
  add(input: ProblemInput, atMs = Date.now()): ProblemEntry {
    const entry: ProblemEntry = { ...input, id: `problem-${++this.#seq}`, atMs, checked: false };
    let next = [entry, ...this.entries];
    while (next.length > PROBLEM_LOG_LIMIT) {
      let drop = next.length - 1;
      while (drop > 0 && next[drop]!.unknownOutcome && !next[drop]!.checked) drop -= 1;
      if (drop === 0) drop = next.length - 1;
      next = next.filter((_, i) => i !== drop);
    }
    this.entries = next;
    return entry;
  }

  /** The listener has checked an unknown write on the official page. */
  markChecked(id: string): void {
    this.entries = this.entries.map((entry) => (entry.id === id ? { ...entry, checked: true } : entry));
  }

  /** The newest entry a toast added, for "View diagnostics" from that toast. */
  entryForToast(toastId: string): ProblemEntry | null {
    return this.entries.find((entry) => entry.toastId === toastId) ?? null;
  }
}
