<script lang="ts">
  import type { LyricsResult, Track } from "@ytmcp/protocol";
  import type { Artwork } from "../cover.js";
  import { t } from "../i18n/index.svelte.js";
  import { queryErrorDetail, queryErrorMessage } from "../i18n/query-errors.js";
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
    /** The shown playback position: the seek preview while one holds, as in the player bar. */
    interpolatedPositionMs: number;
    canSeek: boolean;
    mockMode: boolean;
    smoothScroll: boolean;
    /** The header's cover: App's held artwork, which keeps the previous one while a new cover is cached. Defaults to the track's own. */
    artwork?: Artwork | null;
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
    smoothScroll,
    artwork,
    accent,
    onSeekMs,
    onStepOffset,
    onResetOffset,
  }: Props = $props();

  let listEl = $state<HTMLDivElement | null>(null);
  /** The listener scrolled the lines by hand: stop following until asked or a new track. */
  let browsing = $state(false);

  const activeIndex = $derived(activeLyricIndex(lyrics, interpolatedPositionMs));
  const synced = $derived(lyrics?.kind === "synced" && (lyrics.syncedLines?.length ?? 0) > 0);
  const heroArt = $derived(track ? [{ cover: track.cover, seed: track.videoId, label: track.title }] : []);
  const headArt = $derived(artwork !== undefined ? artwork : (heroArt[0] ?? null));
  const accentStyle = $derived(
    accent ? `--accent: ${accent.accent}; --accent-soft: ${accent.accentSoft};` : ""
  );
  const offsetFormatted = $derived(
    !lyrics || lyrics.userOffsetMs === 0 ? null : `${lyrics.userOffsetMs > 0 ? "+" : ""}${lyrics.userOffsetMs} ms`
  );

  // A new track follows again; the next line of the same track does not. Derived, so a new
  // snapshot of the same track does not count as a change.
  const trackId = $derived(track?.videoId ?? null);
  $effect(() => {
    void trackId;
    browsing = false;
  });

  const SCROLL_KEYS = new Set(["ArrowUp", "ArrowDown", "PageUp", "PageDown", "Home", "End"]);

  /**
   * Only input that moves the list by hand pauses following (wheel, touch drag, scroll keys,
   * pressing the scrollbar), never `scroll` events, which the follow scroll itself emits.
   */
  function browse() {
    if (listEl && listEl.scrollHeight > listEl.clientHeight) browsing = true;
  }

  /**
   * Centres line `idx` in the list; scrolls only the list, never the page (offsetTop is measured
   * from it). `glide` animates the move unless motion is reduced; otherwise it is instant.
   */
  function scrollToLine(idx: number, glide: boolean) {
    const el = listEl?.querySelector<HTMLElement>(`[data-line-index="${idx}"]`);
    if (!listEl || !el || typeof listEl.scrollTo !== "function") return;
    const targetTop = Math.max(0, el.offsetTop - listEl.clientHeight / 2 + el.clientHeight / 2);
    listEl.scrollTo({ top: targetTop, behavior: glide && smoothScroll ? "smooth" : "auto" });
  }

  /** The list, the lines and the line it was last centred on; plain, so placing never re-runs the effect. */
  let placed: { list: HTMLElement; lines: unknown; index: number } | null = null;

  function place(idx: number, glide: boolean) {
    placed = listEl ? { list: listEl, lines: lyrics?.syncedLines ?? null, index: idx } : null;
    scrollToLine(idx, glide);
  }

  // The current line stays centred, playing or paused, unless the listener is browsing. Moving
  // on to a neighbouring line glides; anything else (opening the page, lyrics arriving, a seek,
  // coming back to a window that was hidden) is placed at once, so the list never sweeps
  // through the song.
  $effect(() => {
    const idx = activeIndex;
    const list = listEl;
    const lines = lyrics?.syncedLines ?? null;
    if (browsing || idx < 0 || !list) return;
    const same = placed !== null && placed.list === list && placed.lines === lines;
    if (same && placed!.index === idx) return;
    place(idx, same && Math.abs(placed!.index - idx) === 1);
  });

  /**
   * Ends browsing and glides back to the current line. Focus moves to that line, or the list,
   * before this button goes away.
   */
  function backToCurrent() {
    const line = activeIndex >= 0 ? listEl?.querySelector<HTMLElement>(`[data-line-index="${activeIndex}"]`) : null;
    ((canSeek && line) || listEl)?.focus({ preventScroll: true });
    if (activeIndex >= 0) place(activeIndex, true);
    browsing = false;
  }
</script>

<section class="lyrics-page" aria-labelledby="lyrics-heading" data-testid="view-lyrics" style={accentStyle}>
  <header class="lyrics-head page-hero">
    <HeroWash art={heroArt} {mockMode} />
    {#if track}
      <Cover cover={headArt?.cover} seed={headArt?.seed ?? track.videoId} label={headArt?.label ?? track.title} {mockMode} class="lyrics-head-cover" />
    {/if}
    <div class="min-w-0 flex-1">
      <h1 id="lyrics-heading" class="lyrics-title">{t.lyrics.title}</h1>
      {#if track}
        <p class="lyrics-track truncate" lang={detectCjkLangHint(track.title)}>
          <span class="lyrics-track-title">{track.title}</span>
          <span aria-hidden="true"> · </span>
          <span>{formatArtists(track)}</span>
        </p>
      {/if}
    </div>
  </header>

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
    <div class="lyrics-tools">
      <span>{canSeek ? t.lyrics.syncedHint : ""}</span>
      <div class="flex flex-wrap items-center gap-1.5">
        {#if offsetFormatted}
          <span class="tabular-nums" data-testid="lyrics-offset">{t.lyrics.offsetLabel} {offsetFormatted}</span>
        {/if}
        <button type="button" class="btn-ghost" onclick={() => onStepOffset(-500)}>{t.lyrics.offsetEarlier}</button>
        <button type="button" class="btn-ghost" onclick={() => onStepOffset(500)}>{t.lyrics.offsetLater}</button>
        <button type="button" class="btn-ghost" disabled={lyrics.userOffsetMs === 0} onclick={onResetOffset}>
          {t.lyrics.offsetReset}
        </button>
      </div>
    </div>

    <!-- Plain text only (no {@html}); lines are seek buttons only for synced lyrics. The
         "back" button sits outside the list so the lines stay positioned against the list. -->
    <div class="relative">
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div
        bind:this={listEl}
        class="lyrics-lines"
        tabindex="-1"
        data-testid="lyrics-synced-list"
        onwheel={(e) => {
          if (!e.ctrlKey && e.deltaY !== 0) browse();
        }}
        ontouchmove={browse}
        onkeydown={(e) => {
          if (SCROLL_KEYS.has(e.key)) browse();
        }}
        onpointerdown={(e) => {
          if (e.target === e.currentTarget) browse();
        }}
      >
        {#each lyrics.syncedLines ?? [] as line, idx (`${line.timeMs}::${idx}`)}
          {@const isActive = idx === activeIndex}
          {@const at = effectiveLineTimeMs(line, lyrics)}
          <button
            type="button"
            disabled={!canSeek}
            class="lyrics-line"
            class:is-current={isActive}
            aria-current={isActive ? "true" : undefined}
            data-line-index={idx}
            onclick={() => onSeekMs(at)}
            data-testid="lyrics-seek-line"
          >
            <span class="lyrics-line-time">{formatDurationMs(at)}</span>
            <span class="lyrics-line-text" lang={lyrics.language ?? undefined}>{line.text}</span>
          </button>
        {/each}
      </div>
      {#if browsing}
        <button
          type="button"
          class="btn-secondary lg-press type-xs absolute bottom-3 left-1/2 -translate-x-1/2 shadow-lg"
          onclick={backToCurrent}
          data-testid="lyrics-back-to-current"
        >
          {t.lyrics.backToCurrent}
        </button>
      {/if}
    </div>
  {:else if lyrics.kind === "plain" && lyrics.plainText}
    <p class="lyrics-note">{t.lyrics.plainTextHint}</p>
    <div class="lyrics-plain" data-testid="lyrics-plain-text" lang={lyrics.language ?? undefined}>
      {#each lyrics.plainText.split(/\r?\n/) as line, idx (`plain-${idx}`)}
        {#if line.trim() === ""}
          <div class="h-3" aria-hidden="true"></div>
        {:else}
          <p class="m-0">{line}</p>
        {/if}
      {/each}
    </div>
  {:else}
    <div class="state-block" title={queryErrorDetail(lyrics.error)} data-testid="lyrics-status-empty">
      {queryErrorMessage(lyrics.error?.code, t.lyrics.kinds[lyrics.kind])}
    </div>
  {/if}

  {#if lyrics}
    <p class="lyrics-note" data-testid="lyrics-source">
      {mockMode ? t.lyrics.mockNotice : t.lyrics.sourceLabel.replace("{source}", lyrics.source)}
    </p>
  {/if}
</section>
