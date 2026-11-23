<script lang="ts">
  import { t } from "../i18n/index.svelte.js";
  import Cover from "./Cover.svelte";
  import { detectCjkLangHint } from "./format.js";
  import Icon from "./Icon.svelte";
  import type { Tile } from "./TileGrid.svelte";

  interface Props {
    heading: string;
    tiles: readonly Tile[];
    mockMode: boolean;
    smoothScroll?: boolean;
  }

  let { heading, tiles, mockMode, smoothScroll = true }: Props = $props();

  let scroller = $state<HTMLDivElement | null>(null);
  let canPrev = $state(false);
  let canNext = $state(false);

  function updateScrollBounds() {
    if (!scroller) {
      canPrev = false;
      canNext = false;
      return;
    }
    canPrev = scroller.scrollLeft > 4;
    canNext = scroller.scrollLeft + scroller.clientWidth < scroller.scrollWidth - 4;
  }

  $effect(() => {
    void tiles.length;
    const el = scroller;
    if (!el) return;
    updateScrollBounds();
    const ro = typeof ResizeObserver !== "undefined" ? new ResizeObserver(updateScrollBounds) : null;
    ro?.observe(el);
    return () => ro?.disconnect();
  });

  function page(direction: 1 | -1) {
    scroller?.scrollBy({
      left: direction * scroller.clientWidth * 0.6,
      behavior: smoothScroll ? "smooth" : "auto",
    });
  }
</script>

<!-- A horizontal row of covers. The row bleeds to the window edge, so covers slide under the
     sidebar while its first card rests on the page column. -->
{#if tiles.length > 0}
  <div class="flex flex-col gap-3">
    <div class="flex items-center justify-between gap-3">
      <h2 class="type-lg font-semibold">{heading}</h2>
      {#if canPrev || canNext}
        <div class="flex gap-1">
          <button
            type="button"
            class="btn-icon"
            disabled={!canPrev}
            aria-label={t.common.shelfPrevious}
            onclick={() => page(-1)}
          >
            <Icon name="chevronLeft" />
          </button>
          <button
            type="button"
            class="btn-icon"
            disabled={!canNext}
            aria-label={t.common.shelfNext}
            onclick={() => page(1)}
          >
            <Icon name="chevronRight" />
          </button>
        </div>
      {/if}
    </div>
    <div bind:this={scroller} class="shelf bleed" onscroll={updateScrollBounds} data-testid="cover-shelf">
      {#each tiles as tile (tile.key)}
        <button type="button" class="shelf-card" title={tile.title} onclick={tile.onOpen} data-testid={tile.testId}>
          <Cover cover={tile.cover} seed={tile.key} label={tile.title} {mockMode} class="shelf-cover surface-rounded-lg" />
          <span class="min-w-0">
            <span class="block truncate type-sm font-semibold" lang={detectCjkLangHint(tile.title)}>{tile.title}</span>
            {#if tile.subtitle}
              <span class="block truncate type-xs mt-0.5" style="color: var(--text-secondary);">{tile.subtitle}</span>
            {/if}
          </span>
        </button>
      {/each}
    </div>
  </div>
{/if}
