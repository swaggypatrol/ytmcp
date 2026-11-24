<script lang="ts">
  /**
   * A framed mark reading LOW, STD or HIGH for the playing audio stream, like an "HD" mark. The
   * tier is the official page's own label for that stream; nothing renders while the page has not
   * said. It is a mark, not a button: not focusable, no hover fill. Usage:
   * `<QualityBadge quality={player.audioQuality} codec={player.codec} bitrateKbps={player.audioBitrateKbps} />`.
   */
  import { codecFamily, type AudioQuality } from "../audio-quality.js";
  import { t } from "../i18n/index.svelte.js";

  interface Props {
    quality: AudioQuality | null | undefined;
    codec?: string | null;
    bitrateKbps?: number | null;
    size?: "sm" | "lg";
    class?: string;
  }

  let { quality, codec = null, bitrateKbps = null, size = "sm", class: className = "" }: Props = $props();

  const family = $derived(codecFamily(codec));
  const name = $derived(quality ? t.player.quality[quality] : "");
  const mark = $derived(quality ? t.player.quality.marks[quality] : "");
  const title = $derived(
    [name, family, bitrateKbps ? t.player.quality.kbps.replace("{n}", String(bitrateKbps)) : null].filter(Boolean).join(" · ")
  );
</script>

{#if quality}
  <span
    class="quality-badge {className}"
    class:quality-badge-lg={size === "lg"}
    class:is-high={quality === "high"}
    role="img"
    aria-label={`${t.player.quality.label}${t.common.colon}${title}`}
    {title}
    data-testid="quality-badge"
    data-quality={quality}
  >
    <span aria-hidden="true">{mark}</span>
  </span>
{/if}
