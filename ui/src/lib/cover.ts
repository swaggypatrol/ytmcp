import type { CoverRef } from "@ytm-glass/protocol";
import { createSyntheticCoverDataUri } from "../fixtures/synthetic.js";

const ASSET_ID_RE = /^[A-Za-z0-9_-]{1,128}$/;

/**
 * Local URL of a Rust-cached cover, or null when no asset exists yet.
 * `remoteUrl` is never used: the main UI makes no external requests.
 */
export function coverSrc(cover: CoverRef | null | undefined): string | null {
  const id = cover?.assetId;
  return id && ASSET_ID_RE.test(id) ? `app://covers/${id}` : null;
}

/** One piece of artwork to show: the cover reference plus the seed of its mock stand-in. */
export interface Artwork {
  readonly cover: CoverRef | null | undefined;
  /** Stable seed and label for the mock-mode synthetic artwork. */
  readonly seed: string;
  readonly label: string;
}

/** What an `<img>` may load for this artwork: the cached asset, or synthetic art in mock mode only. */
export function artworkSrc(art: Artwork, mockMode: boolean): string | null {
  return coverSrc(art.cover) ?? (mockMode ? createSyntheticCoverDataUri(art.seed, art.label) : null);
}
