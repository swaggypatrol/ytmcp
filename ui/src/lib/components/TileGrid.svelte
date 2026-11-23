<script lang="ts" module>
  import type { CoverRef } from "@ytmcp/protocol";

  /** One navigable album / artist / playlist tile. */
  export interface Tile {
    readonly key: string;
    readonly title: string;
    readonly subtitle?: string | null;
    readonly cover?: CoverRef | null;
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
  }

  let { heading, tiles, mockMode, round = false }: Props = $props();
</script>

{#if tiles.length > 0}
  <div class="flex flex-col gap-3">
    {#if heading}
      <h2 class="type-lg font-semibold">{heading}</h2>
    {/if}
    <div class="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-3">
      {#each tiles as tile (tile.key)}
        <button type="button" class="tile" onclick={tile.onOpen} data-testid={tile.testId}>
          <Cover
            cover={tile.cover}
            seed={tile.key}
            label={tile.title}
            {mockMode}
            class="h-14 w-14 {round ? 'rounded-full' : 'rounded-lg'}"
          />
          <span class="min-w-0 flex-1">
            <span class="block truncate type-base font-semibold" lang={detectCjkLangHint(tile.title)}>{tile.title}</span>
            {#if tile.subtitle}
              <span class="block truncate type-xs mt-0.5" style="color: var(--text-secondary);">{tile.subtitle}</span>
            {/if}
          </span>
        </button>
      {/each}
    </div>
  </div>
{/if}
