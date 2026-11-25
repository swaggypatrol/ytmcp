<script lang="ts">
  import { untrack } from "svelte";
  import { getAmbient } from "../ambient/ambient.svelte.js";
  import type { Artwork } from "../cover.js";

  interface Props {
    /** The page's main cover(s); up to three are blended side by side. None: the playing track's cover lights the window instead. */
    art: readonly Artwork[];
    mockMode: boolean;
    /** Light the whole window rather than a band along the top (Now playing). */
    full?: boolean;
    /**
     * The page's own art is not known yet (its data is still loading): the light stays as it is
     * rather than falling back to the playing cover and changing again once the art arrives.
     */
    hold?: boolean;
  }

  let { art, mockMode, full = false, hold = false }: Props = $props();

  const ambient = getAmbient();
  const owner = {};
  // The light when this page opened, while the page it replaces still reports it: what a page
  // that opens on hold keeps showing (the old page withdraws its own report as it goes).
  const opening = untrack(() => ambient?.current ?? null);
  let reported = false;

  $effect(() => {
    if (hold) {
      // Keep this page's last report; a page with none yet keeps the light it opened under.
      if (!reported && opening) untrack(() => ambient?.report(owner, opening));
      return;
    }
    reported = true;
    ambient?.report(owner, { art, mockMode, full });
  });
  $effect(() => () => ambient?.withdraw(owner));
</script>

<!-- Renders nothing in the page: it reports the page's main cover to the shell's ambient light (L1). -->
