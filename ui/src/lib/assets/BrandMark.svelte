<script lang="ts">
  interface Props {
    /** Whole turns the ring has made; each increment turns it once more. */
    turns?: number;
    /** The star is at the peak of its twinkle. */
    twinkle?: boolean;
  }

  let { turns = 0, twinkle = false }: Props = $props();
</script>

<!-- The app icon's drawing (tile, ring with a gap, disc, play triangle, star) with its parts
     addressable, so the ring and the star can move. Colours are the icon's own (sidebar.css). -->
<svg class="brand-mark-art" class:is-twinkle={twinkle} viewBox="0 0 64 64" aria-hidden="true" focusable="false">
  <rect class="brand-mark-tile" width="64" height="64" rx="15" />
  <circle
    class="brand-mark-ring"
    cx="31"
    cy="33"
    r="20"
    fill="none"
    stroke-width="2.4"
    stroke-linecap="round"
    stroke-dasharray="100 25.7"
    style:transform="rotate({turns * 360}deg)"
  />
  <circle class="brand-mark-disc" cx="31" cy="33" r="13.5" />
  <path class="brand-mark-play" d="M27.6 27.2v11.6l9.8-5.8z" stroke-width="1.6" stroke-linejoin="round" />
  <path
    class="brand-mark-star"
    d="M47 11.5c.5 3 1.6 4.4 4.8 5c-3.2.6-4.3 2-4.8 5c-.5-3-1.6-4.4-4.8-5c3.2-.6 4.3-2 4.8-5z"
  />
</svg>
