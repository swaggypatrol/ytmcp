<script lang="ts">
  import { untrack } from "svelte";
  import { ambientSources, type AmbientStore } from "../ambient/ambient.svelte.js";
  import type { Artwork } from "../cover.js";

  interface Props {
    store: AmbientStore;
    /** The playing track's cover: the light when the page reports none. */
    playing: Artwork | null;
    mockMode: boolean;
    /** Playing, visible and motion allowed: the light breathes; otherwise it holds still. */
    breathing: boolean;
    reducedMotion: boolean;
    /** The page's scroll container; the light dims, but never moves, as it scrolls. */
    scroller: HTMLElement | null;
  }

  let { store, playing, mockMode, breathing, reducedMotion, scroller }: Props = $props();

  const CROSS_FADE_MS = 900;
  const REDUCED_FADE_MS = 140;
  /** Old layers kept under a new one while it fades in (an interrupted fade keeps its own). */
  const MAX_LAYERS = 3;
  /** Scroll distance over which the light dims from 0.85 to 0.5 of its strength. */
  const FADE_DISTANCE_PX = 320;
  const FADE_FLOOR = 0.5 / 0.85;

  let failed = $state.raw<ReadonlySet<string>>(new Set());
  const full = $derived(store.current?.full ?? false);
  const sources = $derived(ambientSources(store.current, playing, mockMode, failed));
  // Keyed on the resolved sources, so a re-reported but identical page does not cross-fade.
  const key = $derived(`${full ? "full" : "band"}|${sources.join("\n")}`);

  interface Layer {
    readonly id: number;
    readonly key: string;
    readonly sources: readonly string[];
    readonly full: boolean;
    readonly leaving: boolean;
  }
  let layers = $state.raw<Layer[]>([]);
  let layerSeq = 0;
  let pruneTimer: ReturnType<typeof setTimeout> | null = null;
  let rootEl = $state<HTMLDivElement | null>(null);

  // A change adds a layer on top that fades in; the ones below fade out under it and are
  // dropped once the fade is over. A change mid-fade simply starts from where things are.
  $effect(() => {
    const next = { key, sources, full };
    untrack(() => {
      const top = layers.at(-1);
      if (top && !top.leaving && top.key === next.key) return;
      const below = layers.slice(-(MAX_LAYERS - 1)).map((layer) => (layer.leaving ? layer : { ...layer, leaving: true }));
      layers = [...below, { id: ++layerSeq, ...next, leaving: false }];
      if (pruneTimer !== null) clearTimeout(pruneTimer);
      pruneTimer = setTimeout(
        () => {
          pruneTimer = null;
          layers = layers.filter((layer) => !layer.leaving);
        },
        (reducedMotion ? REDUCED_FADE_MS : CROSS_FADE_MS) + 60
      );
    });
  });

  $effect(() => () => {
    if (pruneTimer !== null) clearTimeout(pruneTimer);
  });

  // Scroll dims the light; written straight to the element, one frame at a time.
  $effect(() => {
    const el = scroller;
    const target = rootEl;
    if (!el || !target) return;
    let frame = 0;
    const apply = () => {
      frame = 0;
      const progress = Math.min(1, Math.max(0, el.scrollTop / FADE_DISTANCE_PX));
      target.style.setProperty("--ambient-scroll", String(1 - (1 - FADE_FLOOR) * progress));
    };
    const onScroll = () => {
      if (frame === 0) frame = requestAnimationFrame(apply);
    };
    apply();
    el.addEventListener("scroll", onScroll, { passive: true });
    return () => {
      el.removeEventListener("scroll", onScroll);
      if (frame !== 0) cancelAnimationFrame(frame);
    };
  });

  function markFailed(src: string) {
    if (!failed.has(src)) failed = new Set([...failed, src]);
  }
</script>

<!-- L1: fixed to the window, outside the scroll container and outside the page's transition
     snapshot. Breathing (outer) and the cross-fade (layers) are on separate elements. -->
<div
  bind:this={rootEl}
  class="ambient"
  aria-hidden="true"
  data-testid="ambient-light"
  data-full={full || undefined}
  data-motion={reducedMotion ? "reduced" : breathing ? "breathing" : "still"}
>
  <div class="ambient-breath">
    {#each layers as layer (layer.id)}
      <div class="ambient-layer" class:is-full={layer.full} data-leaving={layer.leaving || undefined}>
        <div class="ambient-art">{#each layer.sources as src (src)}<img {src} alt="" decoding="async" onerror={() => markFailed(src)} />{/each}</div>
      </div>
    {/each}
  </div>
</div>
