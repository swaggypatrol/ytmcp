<script lang="ts">
  import type { CoverRef } from "@ytmcp/protocol";
  import { artworkSrc } from "../cover.js";

  interface Props {
    cover: CoverRef | null | undefined;
    /** Stable seed and label for the mock-mode synthetic artwork. */
    seed: string;
    label: string;
    mockMode: boolean;
    class?: string;
  }

  let { cover, seed, label, mockMode, class: className = "" }: Props = $props();

  let failedSrc = $state<string | null>(null);
  const src = $derived(artworkSrc({ cover, seed, label }, mockMode));
</script>

{#if src && src !== failedSrc}
  <img
    {src}
    alt=""
    class="object-cover shrink-0 {className}"
    loading="lazy"
    decoding="async"
    onerror={() => {
      failedSrc = src;
    }}
  />
{:else}
  <div
    class="cover-placeholder shrink-0 grid place-items-center {className}"
    aria-hidden="true"
    data-testid="cover-placeholder"
  >
    <svg viewBox="0 0 24 24" width="40%" height="40%" fill="none" stroke="currentColor" stroke-width="1.6">
      <path d="M9 18V6l10-2v12" stroke-linecap="round" stroke-linejoin="round" />
      <circle cx="6.5" cy="18" r="2.5" />
      <circle cx="16.5" cy="16" r="2.5" />
    </svg>
  </div>
{/if}
