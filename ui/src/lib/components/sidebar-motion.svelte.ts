import { holdPlainBlur } from "../glass/liquid-glass.js";

/** Name of the registered length the sidebar, the player dock and the page column move by (sidebar.css). */
export const SIDEBAR_EDGE_PROPERTY = "--sidebar-edge";
/** How long the labels take to leave before a reduced-motion collapse (--lg-duration-quick). */
const TEXT_EXIT_MS = 90;
/** The move's length when the --duration-slow token cannot be read. */
const SLOW_MS = 320;
/** The lens is back at most this long after the move should have ended. */
const LENS_RETURN_MS = 120;

/** --duration-slow in milliseconds, read from the document so a slowed token slows the hold too. */
function slowMs(): number {
  if (typeof document === "undefined") return SLOW_MS;
  const raw = getComputedStyle(document.documentElement).getPropertyValue("--duration-slow").trim();
  const value = parseFloat(raw);
  if (!Number.isFinite(value)) return SLOW_MS;
  return raw.endsWith("ms") ? value : raw.endsWith("s") ? value * 1000 : value;
}

/**
 * Collapse state of the sidebar and the bookkeeping of its transition.
 *
 * With motion, the layout switches in one step and the shell carries `moving` while the edge
 * glides (320 ms, interruptible: a second toggle reverses from where it is); both glass surfaces
 * hold the plain blur until the sidebar's own transition ends, then the lens comes back. With
 * reduced motion nothing moves: a collapse first fades the labels out, then the width switches;
 * an expansion switches the width and the labels fade in.
 */
export class SidebarMotion {
  collapsed = $state(false);
  moving = $state(false);
  /** Labels are on their way out ahead of a reduced-motion collapse. */
  #textLeaving = $state(false);
  #settleTimer: ReturnType<typeof setTimeout> | undefined;
  #exitTimer: ReturnType<typeof setTimeout> | undefined;

  /** True while the labels and shortcut hints are hidden or fading out. */
  get textHidden(): boolean {
    return this.collapsed || this.#textLeaving;
  }

  /** Flips the state; `reducedMotion` picks the cross-fade over the glide. */
  toggle(reducedMotion: boolean): void {
    if (reducedMotion) {
      this.#settle();
      if (this.#textLeaving) {
        // A second click while the labels leave: they come back and nothing collapses.
        clearTimeout(this.#exitTimer);
        this.#textLeaving = false;
      } else if (this.collapsed) {
        this.collapsed = false;
      } else {
        this.#textLeaving = true;
        this.#exitTimer = setTimeout(() => {
          this.#textLeaving = false;
          this.collapsed = true;
        }, TEXT_EXIT_MS);
      }
      return;
    }
    clearTimeout(this.#exitTimer);
    this.#textLeaving = false;
    if (!this.moving) {
      this.moving = true;
      holdPlainBlur(true);
    }
    clearTimeout(this.#settleTimer);
    // Ends a move whose transitionend never arrives (cancelled, or the panel not rendered).
    this.#settleTimer = setTimeout(() => this.#settle(), slowMs() + LENS_RETURN_MS);
    this.collapsed = !this.collapsed;
  }

  /** transitionend from the shell: the sidebar's own edge has arrived. */
  onTransitionEnd(event: TransitionEvent): void {
    if (event.propertyName !== SIDEBAR_EDGE_PROPERTY) return;
    if (!(event.target instanceof Element) || !event.target.classList.contains("app-sidebar")) return;
    this.#settle();
  }

  /** Stops timers and gives the lens back; for unmount as well. */
  dispose(): void {
    clearTimeout(this.#exitTimer);
    this.#settle();
  }

  #settle(): void {
    clearTimeout(this.#settleTimer);
    if (!this.moving) return;
    this.moving = false;
    holdPlainBlur(false);
  }
}
