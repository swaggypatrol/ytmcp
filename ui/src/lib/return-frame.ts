import { getContext, setContext } from "svelte";

/** Where a track list was scrolled to, and which row had focus, when its page was left. */
export interface ListPosition {
  /** Key of the first row in view, and how far the list was scrolled past its top. */
  readonly anchorKey: string;
  readonly offsetPx: number;
  /** Raw scroll offset, used when the anchored row is gone. */
  readonly scrollTop: number;
  /** The focused row, only when focus was inside the list. */
  readonly focusKey: string | null;
  readonly focusIndex: number | null;
}

/**
 * Hands a track list's position from a page being left to the same page when it comes back
 * (lyrics opened and closed with the toggle). One slot: the page's mounted list registers how
 * to read its position, and the next list that mounts after `offer` claims it.
 *
 * Usage: the app calls `setReturnFrames(frames)`; a list calls `getReturnFrames()?.register(…)`
 * and `claim()` once on mount.
 */
export class ReturnFrames {
  #capture: (() => ListPosition | null) | null = null;
  #pending: { readonly position: ListPosition; readonly restored: () => void } | null = null;

  /** The most recently mounted list wins; the returned function unregisters it. */
  register(capture: () => ListPosition | null): () => void {
    this.#capture = capture;
    return () => {
      if (this.#capture === capture) this.#capture = null;
    };
  }

  capture(): ListPosition | null {
    return this.#capture?.() ?? null;
  }

  /** `restored` runs once the claiming list has applied the position (its data has loaded). */
  offer(position: ListPosition, restored: () => void): void {
    this.#pending = { position, restored };
  }

  claim(): { readonly position: ListPosition; readonly restored: () => void } | null {
    const pending = this.#pending;
    this.#pending = null;
    return pending;
  }

  clear(): void {
    this.#pending = null;
  }
}

/** Context key; tests pass `new Map([[RETURN_FRAMES_KEY, frames]])` as render context. */
export const RETURN_FRAMES_KEY = Symbol("return-frames");

export function setReturnFrames(frames: ReturnFrames): void {
  setContext(RETURN_FRAMES_KEY, frames);
}

export function getReturnFrames(): ReturnFrames | undefined {
  return getContext<ReturnFrames | undefined>(RETURN_FRAMES_KEY);
}
