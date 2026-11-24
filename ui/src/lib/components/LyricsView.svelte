<script lang="ts">
  import type { LyricsResult, Track } from "@ytmcp/protocol";
  import { t } from "../i18n/index.svelte.js";
  import { activeLyricIndex, effectiveLineTimeMs } from "../lyrics.js";
  import type { TrackAccent } from "../theme/accent.js";
  import Cover from "./Cover.svelte";
  import { detectCjkLangHint, formatArtists, formatDurationMs } from "./format.js";
  import HeroWash from "./HeroWash.svelte";
  import Icon from "./Icon.svelte";

  interface Props {
    track: Track | null;
    lyrics: LyricsResult | null;
    loading: boolean;
    interpolatedPositionMs: number;
    canSeek: boolean;
    mockMode: boolean;
    /** Follow the active line only while playing and visible; otherwise the view stays put. */
    followActive: boolean;
    smoothScroll: boolean;
    accent: TrackAccent | null;
    onSeekMs: (positionMs: number) => void;
    onStepOffset: (deltaMs: number) => void;
    onResetOffset: () => void;
  }

  let {
    track,
    lyrics,
    loading,
    interpolatedPositionMs,
    canSeek,
    mockMode,
    followActive,
    smoothScroll,
    accent,
    onSeekMs,
    onStepOffset,
    onResetOffset,
  }: Props = $props();

  let listEl = $state<HTMLDivElement | null>(null);

  const activeIndex = $derived(activeLyricIndex(lyrics, interpolatedPositionMs));
  const synced = $derived(lyrics?.kind === "synced" && (lyrics.syncedLines?.length ?? 0) > 0);
  const heroArt = $derived(track ? [{ cover: track.cover, seed: track.videoId, label: track.title }] : []);
  const accentStyle = $derived(
    accent ? `--accent: ${accent.accent}; --accent-soft: ${accent.accentSoft};` : ""
  );
  const offsetFormatted = $derived(
    !lyrics || lyrics.userOffsetMs === 0
      ? "0 ms"
      : `${lyrics.userOffsetMs > 0 ? "+" : ""}${lyrics.userOffsetMs} ms`
  );

  // Scrolls only the list, never the page; the list is positioned, so offsetTop is measured from it.
  $effect(() => {
    const idx = activeIndex;
    if (!followActive || idx < 0 || !listEl) return;
    const el = listEl.querySelector<HTMLElement>(`[data-line-index="${idx}"]`);
    if (el && typeof listEl.scrollTo === "function") {
      const targetTop = Math.max(0, el.offsetTop - listEl.clientHeight / 2 + el.clientHeight / 2);
      listEl.scrollTo({ top: targetTop, behavior: smoothScroll ? "smooth" : "auto" });
    }
  });
</script>

<section class="flex flex-col gap-5" aria-labelledby="lyrics-heading" data-testid="view-lyrics" style={accentStyle}>
  <div class="page-hero flex flex-wrap items-end justify-between gap-4">
    <HeroWash art={heroArt} {mockMode} />
    <div class="flex items-center gap-4 min-w-0 flex-1">
      {#if track}
        <Cover cover={track.cover} seed={track.videoId} label={track.title} {mockMode} class="h-16 w-16 surface-rounded-md shadow-lg" />
      {/if}
      <div class="min-w-0 flex-1">
        <h1 id="lyrics-heading" class="type-xl font-bold tracking-tight m-0">{t.lyrics.title}</h1>
        {#if track}
          <div class="truncate type-sm mt-1" style="color: var(--text-secondary);" lang={detectCjkLangHint(track.title)}>
            <span class="font-medium" style="color: var(--text-primary);">{track.title}</span>
            <span aria-hidden="true"> · </span>
            <span>{formatArtists(track)}</span>
          </div>
        {/if}
      </div>
    </div>
    {#if lyrics}
      <div class="flex flex-wrap items-center gap-2">
        <span class="chip" data-testid="lyrics-kind-badge">{t.lyrics.kinds[lyrics.kind]}</span>
        {#if synced}
          <span class="chip tabular-nums">{t.lyrics.offsetLabel} {offsetFormatted}</span>
        {/if}
      </div>
    {/if}
  </div>

  {#if lyrics}
    <p class="type-xs m-0" style="color: var(--text-muted);" data-testid="lyrics-source">
      {mockMode ? t.lyrics.mockNotice : t.lyrics.sourceLabel.replace("{source}", lyrics.source)}
    </p>
  {/if}

  {#if !track}
    <div class="state-block">
      <Icon name="lyrics" class="w-9 h-9" style="color: var(--text-muted);" />
      <span>{t.lyrics.noTrack}</span>
    </div>
  {:else if loading && !lyrics}
    <div class="state-block" role="status" aria-busy="true">{t.states.loading}</div>
  {:else if !lyrics}
    <div class="state-block">{t.lyrics.kinds.unavailable}</div>
  {:else if synced}
    <div class="flex flex-wrap items-center justify-between gap-2">
      <span class="type-xs" style="color: var(--text-secondary);">{canSeek ? t.lyrics.syncedHint : ""}</span>
      <div class="flex flex-wrap items-center gap-1.5">
        <button type="button" class="btn-secondary type-xs" onclick={() => onStepOffset(-500)}>{t.lyrics.offsetEarlier}</button>
        <button type="button" class="btn-secondary type-xs" onclick={() => onStepOffset(500)}>{t.lyrics.offsetLater}</button>
        <button
          type="button"
          class="btn-ghost type-xs"
          disabled={lyrics.userOffsetMs === 0}
          onclick={onResetOffset}
        >
          {t.lyrics.offsetReset}
        </button>
      </div>
    </div>

    <!-- Plain text only (no {@html}); lines are seek buttons only for synced lyrics. -->
    <div bind:this={listEl} class="card relative p-4 sm:p-5 flex flex-col gap-1.5 max-h-[60vh] overflow-y-auto" data-testid="lyrics-synced-list">
      {#each lyrics.syncedLines ?? [] as line, idx (`${line.timeMs}::${idx}`)}
        {@const isActive = idx === activeIndex}
        {@const at = effectiveLineTimeMs(line, lyrics)}
        <button
          type="button"
          disabled={!canSeek}
          class="lyric-line"
          class:lyric-line-active={isActive}
          aria-current={isActive ? "true" : undefined}
          data-line-index={idx}
          onclick={() => onSeekMs(at)}
          data-testid="lyrics-seek-line"
        >
          <span class="w-14 shrink-0 type-xs tabular-nums" style="color: {isActive ? 'var(--accent)' : 'var(--text-muted)'};">
            {formatDurationMs(at)}
          </span>
          <span
            class="break-words type-base {isActive ? 'font-bold' : 'font-medium'}"
            lang={lyrics.language ?? undefined}
          >
            {line.text}
          </span>
        </button>
      {/each}
    </div>
  {:else if lyrics.kind === "plain" && lyrics.plainText}
    <p class="type-xs m-0" style="color: var(--text-secondary);">{t.lyrics.plainTextHint}</p>
    <div class="card p-5 sm:p-6 flex flex-col gap-2" data-testid="lyrics-plain-text" lang={lyrics.language ?? undefined}>
      {#each lyrics.plainText.split(/\r?\n/) as line, idx (`plain-${idx}`)}
        {#if line.trim() === ""}
          <div class="h-3" aria-hidden="true"></div>
        {:else}
          <p class="type-base m-0 leading-relaxed">{line}</p>
        {/if}
      {/each}
    </div>
  {:else}
    <div class="state-block" data-testid="lyrics-status-empty">
      {lyrics.error?.safeMessage ?? t.lyrics.kinds[lyrics.kind]}
    </div>
  {/if}
</section>
