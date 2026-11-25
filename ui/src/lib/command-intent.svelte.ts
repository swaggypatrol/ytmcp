import type { CommandReceipt } from "@ytmcp/protocol";
import { untrack } from "svelte";
import { isTerminalCommandStatus } from "./seek-preview.js";

/** Longest a press is shown when its command never visibly settles (a receipt that was missed). */
export const COMMAND_INTENT_TIMEOUT_MS = 15_000;

/** What decides that a press has settled. */
export interface CommandIntentFacts {
  readonly lastReceipt: CommandReceipt | null | undefined;
  /** No command sent from this window is still waiting for its outcome. */
  readonly ownIdle: boolean;
}

/**
 * The value a discrete player control (play/pause, shuffle, repeat) was last asked for. The
 * control shows it at once, marked pending, until that command settles. The page's observation
 * reaches the snapshot before the command's receipt, so letting go on the receipt hands over to
 * the observed value without a flip. A newer press replaces an older one, whose late receipt is
 * then ignored; a failure or an unknown outcome lets go at once, and App reports it like any
 * failed command. The observed value stays the only settled truth.
 *
 * Usage: create during component initialisation with a getter of the facts, read `value`
 * (null when nothing is pending), and call `request(value, send)` on a press.
 */
export class CommandIntent<T> {
  value = $state.raw<T | null>(null);
  #sequence = 0;
  #commandId: string | null = null;
  readonly #facts: () => CommandIntentFacts;

  constructor(facts: () => CommandIntentFacts) {
    this.#facts = facts;
    $effect(() => {
      const current = this.#facts();
      untrack(() => this.#settle(current));
    });
    $effect(() => {
      if (this.value === null) return;
      const timer = setTimeout(() => this.#release(), COMMAND_INTENT_TIMEOUT_MS);
      return () => clearTimeout(timer);
    });
  }

  async request(value: T, send: () => Promise<CommandReceipt | null>): Promise<void> {
    const sequence = ++this.#sequence;
    this.value = value;
    this.#commandId = null;
    const receipt = await send();
    if (sequence !== this.#sequence) return;
    if (!receipt || isTerminalCommandStatus(receipt.status)) {
      this.#release();
      return;
    }
    this.#commandId = receipt.commandId;
    // The receipt may have settled before the reply came back.
    this.#settle(untrack(this.#facts));
  }

  #settle({ lastReceipt, ownIdle }: CommandIntentFacts): void {
    if (this.value === null || this.#commandId === null) return;
    const settled = lastReceipt?.commandId === this.#commandId && isTerminalCommandStatus(lastReceipt.status);
    if (settled || ownIdle) this.#release();
  }

  #release(): void {
    this.#sequence += 1;
    this.#commandId = null;
    this.value = null;
  }
}
