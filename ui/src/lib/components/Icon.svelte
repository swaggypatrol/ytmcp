<script lang="ts" module>
  import { FILLED, OUTLINE } from "../icons/glass-icons.generated.js";
  import type { GlassGlyph } from "../icons/glyph.js";

  /**
   * Glyphs the pack does not have, drawn to its rules (24×24, live area 2–22, 1.75 stroke,
   * round caps and joins, currentColor). See ui/src/lib/icons/README.md.
   */
  const LOCAL = {
    "arrow-left": [["path", { d: "M19.5 12h-15M10.5 6l-6 6 6 6" }]],
    "external-link": [
      ["path", { d: "M13.5 4h6.5v6.5M20 4l-8.5 8.5" }],
      ["path", { d: "M18 14v4.5a1.5 1.5 0 0 1-1.5 1.5h-11A1.5 1.5 0 0 1 4 18.5v-11A1.5 1.5 0 0 1 5.5 6H10" }],
    ],
    "now-playing": [["path", { d: "M6 10v8M12 5v13M18 8v10" }]],
  } as const satisfies Record<string, GlassGlyph>;

  type GlyphKey = keyof typeof OUTLINE | keyof typeof LOCAL;

  /** App icon names mapped to pack glyphs; the names call sites use stay stable. */
  const NAMES = {
    home: "home",
    search: "search",
    library: "library",
    heart: "heart",
    clock: "recent",
    disc: "album",
    queue: "queue-list",
    playNext: "play-next",
    addToQueue: "add-to-queue",
    lyrics: "lyrics",
    gear: "settings",
    dj: "radio",
    previous: "previous",
    next: "next",
    play: "play",
    pause: "pause",
    chevronLeft: "chevron-left",
    chevronRight: "chevron-right",
    chevronUp: "chevron-up",
    chevronDown: "chevron-down",
    arrowLeft: "arrow-left",
    more: "more",
    close: "close",
    volume: "volume-high",
    volumeMedium: "volume-medium",
    volumeLow: "volume-low",
    volumeMuted: "volume-mute",
    shuffle: "shuffle",
    repeat: "repeat",
    repeatOne: "repeat-one",
    playing: "now-playing",
    externalLink: "external-link",
    plus: "plus",
    user: "artist",
    check: "check",
  } as const satisfies Record<string, GlyphKey>;

  export type IconName = keyof typeof NAMES;

  /** The elements drawn for `name`; `filled` picks the active-state variant where the pack has one. */
  export function glyphFor(name: IconName, filled = false): GlassGlyph {
    const key: GlyphKey = NAMES[name];
    if (filled && key in FILLED) return FILLED[key as keyof typeof FILLED];
    return key in LOCAL ? LOCAL[key as keyof typeof LOCAL] : OUTLINE[key as keyof typeof OUTLINE];
  }

  /** Every app icon name, for tests. */
  export const ICON_NAMES = Object.keys(NAMES) as IconName[];
</script>

<script lang="ts">
  interface Props {
    name: IconName;
    /** Active state: the pack's filled glyph when there is one, otherwise the outline. */
    filled?: boolean;
    class?: string;
    style?: string;
  }

  let { name, filled = false, class: className = "", style }: Props = $props();
  const elements = $derived(glyphFor(name, filled));
</script>

<!-- Outline glyphs: the root sets no fill and a round 1.75 stroke; elements override per glyph. -->
<svg
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width="1.75"
  stroke-linecap="round"
  stroke-linejoin="round"
  class="icon {className}"
  {style}
  aria-hidden="true"
  data-icon={name}
  data-filled={filled ? "" : undefined}
>
  {#each elements as [tag, attrs], index (index)}
    <svelte:element this={tag} xmlns="http://www.w3.org/2000/svg" {...attrs} />
  {/each}
</svg>
