<script lang="ts" module>
  import type { CoverRef } from "@ytmcp/protocol";

  /** One navigable album / artist / playlist tile (or a song to play again). */
  export interface Tile {
    readonly key: string;
    readonly title: string;
    readonly subtitle?: string | null;
    readonly cover?: CoverRef | null;
    /** Accessible name when the title alone does not say what choosing the tile does. */
    readonly label?: string;
    readonly testId: string;
    readonly onOpen: () => void;
  }
</script>

<script lang="ts">
  import Cover from "./Cover.svelte";
  import { detectCjkLangHint } from "./format.js";

  interface Props {
    heading?: string;
    tiles: readonly Tile[];
    mockMode: boolean;
    round?: boolean;
    /** Stop at three columns however wide the page gets (a short, fixed set of tiles). */
    maxThree?: boolean;
  }

  let { heading, tiles, mockMode, round = false, maxThree = false }: Props = $props();
</script>

<!-- Tiles in columns that follow the content area's width (two, three, then four). -->
{#if tiles.length > 0}
  <div class="browse-block">
    {#if heading}
      <div class="browse-block-head">
        <h2 class="type-lg font-semibold">{heading}</h2>
      </div>
    {/if}
    <div class="tile-grid" class:tile-grid-max-3={maxThree}>
      {#each tiles as tile (tile.key)}
        <button type="button" class="tile" aria-label={tile.label} onclick={tile.onOpen} data-testid={tile.testId}>
          <Cover
            cover={tile.cover}
            seed={tile.key}
            label={tile.title}
            {mockMode}
            class="h-12 w-12 {round ? 'rounded-full' : 'rounded-lg'}"
          />
          <span class="min-w-0 flex-1">
            <span class="block truncate type-base font-medium" lang={detectCjkLangHint(tile.title)}>{tile.title}</span>
            {#if tile.subtitle}
              <span class="block truncate type-sm" style="color: var(--text-secondary);" lang={detectCjkLangHint(tile.subtitle)}>
                {tile.subtitle}
              </span>
            {/if}
          </span>
        </button>
      {/each}
    </div>
  </div>
{/if}
