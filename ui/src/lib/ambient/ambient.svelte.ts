import { getContext, setContext, untrack } from "svelte";
import { artworkSrc, type Artwork } from "../cover.js";

/** What a page says about the ambient light: its main cover(s), and whether it lights the whole window. */
export interface AmbientReport {
  readonly art: readonly Artwork[];
  readonly mockMode: boolean;
  /** Light the whole window (Now playing) rather than a band along the top. */
  readonly full: boolean;
}

/** At most this many covers are blended side by side. */
export const MAX_AMBIENT_ART = 3;

/**
 * The page side of the ambient light (layer L1). Pages report their main cover(s) through
 * `HeroWash`; the shell's ambient layer reads `current`. The most recently registered reporter
 * wins, and withdrawing removes only the caller's own report, so it does not matter whether an
 * old page unmounts before or after the new one mounts.
 *
 * Usage: the app calls `setAmbient(store)`; a reporter calls `getAmbient()?.report(owner, …)`
 * whenever its art changes and `withdraw(owner)` on unmount.
 */
export class AmbientStore {
  #reports = $state.raw<readonly { readonly owner: object; readonly report: AmbientReport }[]>([]);

  /** The report the light follows, or null when the page reports nothing. */
  readonly current = $derived(this.#reports.at(-1)?.report ?? null);

  /** Adds or replaces the owner's report; a new owner goes on top. Safe to call from an effect. */
  report(owner: object, report: AmbientReport): void {
    const reports = untrack(() => this.#reports);
    const index = reports.findIndex((entry) => entry.owner === owner);
    this.#reports =
      index === -1 ? [...reports, { owner, report }] : reports.map((entry, i) => (i === index ? { owner, report } : entry));
  }

  withdraw(owner: object): void {
    const reports = untrack(() => this.#reports);
    if (reports.some((entry) => entry.owner === owner)) {
      this.#reports = reports.filter((entry) => entry.owner !== owner);
    }
  }
}

/**
 * Image sources for the light, by priority: the page's main cover(s), else the playing track's
 * cover. An empty result means the theme's own colours. Covers that failed to load are skipped,
 * so a broken or uncached cover falls through to the next source.
 */
export function ambientSources(
  report: AmbientReport | null,
  playing: Artwork | null,
  mockMode: boolean,
  failed: ReadonlySet<string>
): string[] {
  const usable = (art: readonly Artwork[], mock: boolean) => {
    const sources = new Set<string>();
    // Albums can repeat across rows. Only distinct images take a slot in the blend.
    for (const item of art) {
      const src = artworkSrc(item, mock);
      if (src) sources.add(src);
      if (sources.size === MAX_AMBIENT_ART) break;
    }
    return [...sources].filter((src) => !failed.has(src));
  };
  const page = report ? usable(report.art, report.mockMode) : [];
  if (page.length > 0) return page;
  return playing ? usable([playing], mockMode) : [];
}

const AMBIENT_KEY = Symbol("ambient");

export function setAmbient(store: AmbientStore): void {
  setContext(AMBIENT_KEY, store);
}

/** Undefined outside the app shell (a page rendered on its own reports nowhere). */
export function getAmbient(): AmbientStore | undefined {
  return getContext<AmbientStore | undefined>(AMBIENT_KEY);
}
