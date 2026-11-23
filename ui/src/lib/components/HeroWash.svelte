<script lang="ts">
  import { artworkSrc, type Artwork } from "../cover.js";

  interface Props {
    /** Up to three pieces are blended side by side; none gives a wash in the theme's colours. */
    art: readonly Artwork[];
    mockMode: boolean;
  }

  let { art, mockMode }: Props = $props();

  const failed = $state<string[]>([]);
  const sources = $derived(
    art
      .slice(0, 3)
      .map((item) => artworkSrc(item, mockMode))
      .filter((src): src is string => src !== null && !failed.includes(src))
  );
</script>

<!-- Decorative colour wash behind a `.page-hero` header; bleeds to the window edge under the sidebar. -->
<div class="hero-wash" aria-hidden="true" data-testid="hero-wash">
  <div class="hero-wash-art">{#each sources as src, i (i)}<img {src} alt="" decoding="async" onerror={() => failed.push(src)} />{/each}</div>
</div>
