export type OverlayExit = "dismiss" | "dissolve";

/** Read the CSS exit clock after applying the leaving state. */
export function overlayExitDuration(surface: HTMLElement, fallback: number): number {
  const time = getComputedStyle(surface).getPropertyValue("--lg-overlay-exit-duration").trim();
  const value = parseFloat(time);
  return Number.isFinite(value) ? value * (time.endsWith("ms") ? 1 : 1000) : fallback;
}

/** Keep a closed menu or dialog visible for its exit, with an option to reopen the same surface. */
export class OverlayPresence {
  rendered = $state(false);
  #resume: (() => void) | undefined;
  #timer: ReturnType<typeof setTimeout> | undefined;
  #observer: MutationObserver | undefined;

  show(): void {
    clearTimeout(this.#timer);
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
    this.#timer = setTimeout(remove, duration + 34);
  }

  dispose(): void {
    clearTimeout(this.#timer);
    this.#observer?.disconnect();
  }
}
