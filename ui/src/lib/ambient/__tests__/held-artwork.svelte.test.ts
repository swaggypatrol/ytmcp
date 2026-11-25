import { flushSync } from "svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Artwork } from "../../cover.js";
import { ARTWORK_HOLD_MS, HeldArtwork } from "../held-artwork.svelte.js";

/** A track's artwork: `cached` when core already has its image, else only the page's URL. */
const art = (id: string, cached: boolean): Artwork => ({
  cover: { assetId: cached ? `asset-${id}` : null, remoteUrl: `https://example.com/${id}.jpg` },
  seed: id,
  label: id,
});

describe("HeldArtwork", () => {
  let stop: (() => void) | null = null;
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    stop?.();
    stop = null;
    vi.useRealTimers();
  });

  /** Mounts a HeldArtwork over a settable current artwork. */
  function held(first: Artwork | null) {
    let current = $state.raw<Artwork | null>(first);
    let value!: HeldArtwork;
    stop = $effect.root(() => {
      value = new HeldArtwork(() => current, () => false);
    });
    flushSync();
    return {
      shown: () => value.shown?.seed ?? null,
      set(next: Artwork | null) {
        current = next;
        flushSync();
      },
      wait(ms: number) {
        vi.advanceTimersByTime(ms);
        flushSync();
      },
    };
  }

  it("keeps the previous cover for at most the hold time while the new one is cached", () => {
    const h = held(art("A", true));
    h.set(art("B", false));
    expect(h.shown()).toBe("A");
    // A fresh snapshot of the same track does not restart the hold.
    h.wait(ARTWORK_HOLD_MS / 2);
    h.set(art("B", false));
    h.wait(ARTWORK_HOLD_MS / 2 - 1);
    expect(h.shown()).toBe("A");
    h.wait(1);
    expect(h.shown()).toBe("B");
    // The cover arrives: the track's own.
    h.set(art("B", true));
    expect(h.shown()).toBe("B");
  });

  it("ends the stand-in when the track changes again before the new cover arrived", () => {
    const h = held(art("A", true));
    h.set(art("B", false));
    h.wait(1_500);
    expect(h.shown()).toBe("A");
    // Skipped again: A's cover never stands in for C, at once and not after the hold either.
    h.set(art("C", false));
    expect(h.shown()).toBe("C");
    h.wait(1_500);
    expect(h.shown()).toBe("C");
    // C's cover lands, then D's is awaited: C (the track just before) stands in again.
    h.set(art("C", true));
    h.set(art("D", false));
    expect(h.shown()).toBe("C");
    h.wait(ARTWORK_HOLD_MS);
    expect(h.shown()).toBe("D");
  });

  it("shows a track with no cover at all at once", () => {
    const h = held(art("A", true));
    h.set({ cover: null, seed: "B", label: "B" });
    expect(h.shown()).toBe("B");
    h.set(art("C", false));
    expect(h.shown()).toBe("C");
  });
});
