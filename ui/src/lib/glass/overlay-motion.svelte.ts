export type OverlayExit = "dismiss" | "dissolve";

/** Read the CSS exit clock after applying the leaving state. */
export function overlayExitDuration(surface: HTMLElement, fallback: number): number {
  const time = getComputedStyle(surface).getPropertyValue("--lg-overlay-exit-duration").trim();
  const value = parseFloat(time);
  return Number.isFinite(value) ? value * (time.endsWith("ms") ? 1 : 1000) : fallback;
}

/** How long past its clock an exit may wait for its transitions (a hidden page does not run them). */
const EXIT_LIMIT_MS = 1000;

/**
 * Calls `done` once the exit the leaving state started inside `region` has run: when every finite
 * transition there has settled, so the removal lands on the frame after the last visible change,
 * even when a busy main thread started the exit late. Without running transitions (or without Web
 * Animations) it waits `clockMs`. Returns a cancel function; once cancelled, `done` never runs.
 */
export function afterExit(region: Element, clockMs: number, done: () => void): () => void {
  let live = true;
  const finish = () => {
    if (!live) return;
    live = false;
    clearTimeout(timer);
    done();
  };
  const running =
    typeof region.getAnimations === "function"
      ? region.getAnimations({ subtree: true }).filter((animation) => {
          const end = animation.effect?.getComputedTiming().endTime;
          return typeof end === "number" && Number.isFinite(end) && animation.playState !== "finished";
        })
      : [];
  const timer = setTimeout(finish, running.length ? clockMs + EXIT_LIMIT_MS : clockMs);
  if (running.length) void Promise.allSettled(running.map((animation) => animation.finished)).then(finish);
  return () => {
    live = false;
    clearTimeout(timer);
  };
}

/** Keep a closed menu or dialog visible for its exit, with an option to reopen the same surface. */
export class OverlayPresence {
  rendered = $state(false);
  #resume: (() => void) | undefined;
  #cancelExit: (() => void) | undefined;
  #observer: MutationObserver | undefined;

  /** True while a hide waits for its exit to run (plain, so a teardown reads the current value). */
  get exiting(): boolean {
    return this.#cancelExit !== undefined;
  }

  show(): void {
    this.#cancelExit?.();
    this.#cancelExit = undefined;
    this.#observer?.disconnect();
    this.#resume?.();
    this.#resume = undefined;
    this.rendered = true;
  }

  hide(region: HTMLElement | null, surface = region): void {
    if (!region || !surface || this.#resume) return;
    region.inert = true;
    region.setAttribute("aria-hidden", "true");
    region.dataset.leaving = "dismiss";
    surface.dataset.leaving = "dismiss";
    const ids = new Map<Element, string>();
    const stripIds = () => {
      for (const node of [region, ...region.querySelectorAll("[data-testid]")]) {
        const id = node.getAttribute("data-testid");
        if (id === null) continue;
        ids.set(node, id);
        node.removeAttribute("data-testid");
      }
    };
    stripIds();
    this.#resume = () => {
      region.inert = false;
      region.removeAttribute("aria-hidden");
      delete region.dataset.leaving;
      delete surface.dataset.leaving;
      for (const [node, id] of ids) {
        if (region.contains(node) && !node.hasAttribute("data-testid")) node.setAttribute("data-testid", id);
      }
    };
    const duration = overlayExitDuration(surface, 0);
    const remove = () => {
      this.#observer?.disconnect();
      this.#cancelExit = undefined;
      this.#resume = undefined;
      this.rendered = false;
    };
    if (!duration) {
      remove();
      return;
    }
    // A playlist request can finish during the dissolve and add new controls to the retained DOM.
    this.#observer = new MutationObserver(stripIds);
    this.#observer.observe(region, { subtree: true, childList: true, attributes: true, attributeFilter: ["data-testid"] });
    this.#cancelExit = afterExit(region, duration, remove);
  }

  dispose(): void {
    this.#cancelExit?.();
    this.#observer?.disconnect();
  }
}
