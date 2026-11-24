<script lang="ts">
  import { getAmbient } from "../ambient/ambient.svelte.js";
  import type { Artwork } from "../cover.js";

  interface Props {
    /** The page's main cover(s); up to three are blended side by side. None: the playing track's cover lights the window instead. */
    art: readonly Artwork[];
    mockMode: boolean;
    /** Light the whole window rather than a band along the top (Now playing). */
    full?: boolean;
  }

  let { art, mockMode, full = false }: Props = $props();

  const ambient = getAmbient();
  const owner = {};

  $effect(() => {
    ambient?.report(owner, { art, mockMode, full });
  });
  $effect(() => () => ambient?.withdraw(owner));
</script>

<!-- Renders nothing in the page: it reports the page's main cover to the shell's ambient light (L1). -->
