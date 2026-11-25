import { untrack } from "svelte";
import { artworkSrc, type Artwork } from "../cover.js";

/** Longest the previous artwork stands in for a new cover that core is still caching. */
export const ARTWORK_HOLD_MS = 2_000;

/** The page named this cover but core has no local image of it yet. Mock artwork is always ready. */
export function artworkAwaited(art: Artwork | null, mockMode: boolean): boolean {
  return art?.cover?.remoteUrl != null && artworkSrc(art, mockMode) === null;
}

/** A track whose cover is awaited, and the previous track's artwork standing in for it (null once it ended). */
interface Awaiting {
  readonly seed: string;
  readonly standIn: Artwork | null;
}

/**
 * The playing track's artwork as the shell shows it. Core reports a new track's cover by its
 * remote URL first and adds the cached image a moment later. Meanwhile the artwork of the track
 * just before stays, if it had an image, for at most ARTWORK_HOLD_MS from the track change, so
 * the player bar, the lyrics header and the ambient light switch once instead of passing through
 * the placeholder (or theme colours). Only the immediately previous track's art stands in: a
 * second track change before the first new cover arrived ends the stand-in. A track with no
 * cover at all shows the placeholder at once, and so does a cover still missing when the hold
 * runs out.
 *
 * Usage: create once during component initialisation (App) with getters, and read `shown`.
 */
export class HeldArtwork {
  readonly #current: () => Artwork | null;
  readonly #mockMode: () => boolean;
  /** The current track's artwork while it has an image; null otherwise. */
  #last = $state.raw<Artwork | null>(null);
  /** Set while the current track's cover is awaited; tracks are told apart by seed (the video id). */
  #awaiting = $state.raw<Awaiting | null>(null);

  readonly shown = $derived.by((): Artwork | null => {
    const art = this.#current();
    if (!art || !artworkAwaited(art, this.#mockMode())) return art;
    const awaiting = this.#awaiting;
    // Before the effect below has caught up with the change, the track just left is still `#last`.
    if (awaiting === null) return this.#last ?? art;
    return awaiting.seed === art.seed ? (awaiting.standIn ?? art) : art;
  });

  constructor(current: () => Artwork | null, mockMode: () => boolean) {
    this.#current = current;
    this.#mockMode = mockMode;
    $effect(() => {
      const art = this.#current();
      const awaited = artworkAwaited(art, this.#mockMode());
      untrack(() => {
        if (!awaited) {
          this.#awaiting = null;
          this.#last = art && artworkSrc(art, this.#mockMode()) !== null ? art : null;
          return;
        }
        // A fresh snapshot of the same track changes nothing; a new track takes over the
        // stand-in only from a track that had an image (not from one that was itself awaited).
        if (this.#awaiting?.seed === art!.seed) return;
        this.#awaiting = { seed: art!.seed, standIn: this.#awaiting === null ? this.#last : null };
        this.#last = null;
      });
    });
    // The hold counts from the track change: only a new stand-in starts it.
    $effect(() => {
      const awaiting = this.#awaiting;
      if (!awaiting?.standIn) return;
      const timer = setTimeout(() => {
        if (this.#awaiting === awaiting) this.#awaiting = { seed: awaiting.seed, standIn: null };
      }, ARTWORK_HOLD_MS);
      return () => clearTimeout(timer);
    });
  }
}
