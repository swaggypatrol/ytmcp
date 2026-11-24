<script lang="ts">
  import type { AppSnapshot, LyricsResult, Track } from "@ytmcp/protocol";
  import { tick, untrack } from "svelte";
  import type { Gate } from "../capabilities.js";
  import { t } from "../i18n/index.svelte.js";
  import { activeLyricIndex } from "../lyrics.js";
  import type { TrackAccent } from "../theme/accent.js";
  import { artworkSrc } from "../cover.js";
  import Cover from "./Cover.svelte";
  import { detectCjkLangHint, formatArtists } from "./format.js";
  import HeroWash from "./HeroWash.svelte";
  import Icon from "./Icon.svelte";
  import QualityBadge from "./QualityBadge.svelte";

  interface Props {
    snapshot: AppSnapshot | null;
    interpolatedPositionMs: number;
    liked: boolean | null;
    likeGate: Gate;
    likePending: boolean;
    canOpenAlbum: boolean;
    canOpenArtist: boolean;
    accent: TrackAccent | null;
    /** Reduced motion is on (setting or system); the track-change fade is skipped. */
    motionReduced?: boolean;
    /** Lyrics the app already holds for the current track; no synced lyrics, no preview. */
    lyrics?: LyricsResult | null;
    /** What the track plays from (a playlist or album name), only when it is really known. */
    playingFrom?: string | null;
    onToggleLike: () => void;
    onOpenActions: () => void;
    /** Opens the playlist chooser for the track; the button shows only while `playlistGate` allows adding. */
    playlistGate?: Gate;
    onAddToPlaylist?: () => void;
    onOpenAlbum: (albumId: string) => void;
    onOpenArtist: (artistId: string) => void;
    onOpenLyrics: () => void;
  }

  let {
    snapshot,
    interpolatedPositionMs,
    liked,
    likeGate,
    likePending,
    canOpenAlbum,
    canOpenArtist,
    accent,
    motionReduced = false,
    lyrics = null,
    playingFrom = null,
    onToggleLike,
    onOpenActions,
    playlistGate = { enabled: false, reason: null },
    onAddToPlaylist,
    onOpenAlbum,
    onOpenArtist,
    onOpenLyrics,
  }: Props = $props();

  const player = $derived(snapshot?.player ?? null);
  const track = $derived(player?.currentTrack ?? null);
  const albumId = $derived(track?.album?.id ?? track?.album?.browseId ?? null);

  interface PreviewLine {
    readonly index: number;
    readonly text: string;
    /** Lines from the current one: 0 is current, -1 the line before, 1 the next. */
    readonly distance: number;
  }

  /**
   * Synced lines round the current one, keyed by line. Each line sits in a one-row slot placed by
   * its distance (now-playing.css), so a new current line slides the column by one row and fades
   * the ends; nothing reflows. One hidden line past each visible end lets lines fade in and out.
   * Before the first line starts, the opening lines wait as "next".
   */
  const previewLines = $derived.by((): PreviewLine[] => {
    if (!track || lyrics?.kind !== "synced" || lyrics.videoId !== track.videoId) return [];
    const lines = lyrics.syncedLines ?? [];
    if (lines.length === 0) return [];
    const active = activeLyricIndex(lyrics, interpolatedPositionMs);
    const out: PreviewLine[] = [];
    for (let i = Math.max(0, active - 3); i <= Math.min(lines.length - 1, active + 4); i += 1) {
      out.push({ index: i, text: lines[i]!.text.trim() || "♪", distance: i - active });
    }
    return out;
  });

  const heroArt = $derived(track ? [{ cover: track.cover, seed: track.videoId, label: track.title }] : []);
  /** How many tracks Up next lists; below the ultrawide tier only the first three show. */
  const UP_NEXT_COUNT = 5;
  /** Longest wait for the new cover to decode before the fade starts anyway. */
  const REVEAL_WAIT_MS = 500;

  interface UpNextRow {
    readonly key: string;
    readonly track: Track;
    readonly suggestion: boolean;
  }

  const queue = $derived(snapshot?.queueSummary ?? null);
  const queueEntries = $derived(queue?.previewEntries ?? []);
  const queueIdx = $derived(
    queue?.currentEntryId ? queueEntries.findIndex((e) => e.entryId === queue.currentEntryId) : -1
  );
  const official = $derived(queue?.officialUpcoming ?? null);
  const selectedFilter = $derived(queue?.filters?.find((f) => f.selected)?.label ?? null);

  /**
   * The explicit tracks after the current occurrence; YouTube Music's own suggestions fill the
   * rest only when the preview reaches the end of the loaded list, so none jump ahead of queued
   * tracks.
   */
  const upNextRows = $derived.by((): UpNextRow[] => {
    if (!queue || queueIdx < 0) return [];
    const explicit = queueEntries
      .slice(queueIdx + 1, queueIdx + 1 + UP_NEXT_COUNT)
      .map((e) => ({ key: e.entryId, track: e.track, suggestion: false }));
    const reachesEnd = (queue.previewStart ?? 0) + queueEntries.length >= queue.loadedCount;
    if (!reachesEnd || explicit.length >= UP_NEXT_COUNT || official?.status !== "observed") return explicit;
    const fill = (official.tracks ?? [])
      .slice(0, UP_NEXT_COUNT - explicit.length)
      .map((track, i) => ({ key: `official:${track.videoId}:${i}`, track, suggestion: true }));
    return [...explicit, ...fill];
  });

  const upNextSource = $derived.by((): string | null => {
    if (queueIdx < 0) return null;
    switch (official?.status) {
      case "observed":
      case "loading":
        return selectedFilter ? `${t.nowPlaying.upNextAutoplay} · ${selectedFilter}` : t.nowPlaying.upNextAutoplay;
      case "off":
        return t.nowPlaying.upNextAutoplayOff;
      case "radio":
        return t.nowPlaying.upNextRadio;
      default:
        return null;
    }
  });

  // Track change fade: hide the cover and title until the new artwork decodes (or a short
  // wait passes), then fade them in. Not on first show and not under reduced motion.
  const trackInstance = $derived(player?.trackInstanceId ?? null);
  const mockMode = $derived(snapshot?.mockMode ?? false);
  const coverSrc = $derived(track ? artworkSrc({ cover: track.cover, seed: track.videoId, label: track.title }, mockMode) : null);
  /** The page reported artwork that core may still be caching (no local asset yet). */
  const coverExpected = $derived(coverSrc !== null || track?.cover?.remoteUrl != null);
  let reveal = $state<"idle" | "wait" | "in">("idle");
  let coverWrap = $state<HTMLElement | null>(null);
  let lastInstance: string | null | undefined;
  let revealSeq = 0;

  function showRevealed(seq: number) {
    if (seq === revealSeq && reveal === "wait") reveal = "in";
  }

  $effect(() => {
    const instance = trackInstance;
    const previous = lastInstance;
    lastInstance = instance;
    if (previous === undefined || instance === previous) return;
    const seq = ++revealSeq;
    if (instance === null || untrack(() => motionReduced)) {
      reveal = "idle";
      return;
    }
    reveal = "wait";
    const timer = setTimeout(() => showRevealed(seq), REVEAL_WAIT_MS);
    return () => clearTimeout(timer);
  });

  // While waiting: reveal once the cover image decodes, at once when there is no artwork, and
  // keep waiting (up to the cap) while expected artwork has no local image yet.
  $effect(() => {
    if (reveal !== "wait") return;
    const seq = revealSeq;
    if (coverSrc === null) {
      if (!coverExpected) showRevealed(seq);
      return;
    }
    void tick().then(() => {
      const img = untrack(() => coverWrap)?.querySelector("img");
      if (!img || typeof img.decode !== "function") showRevealed(seq);
      else img.decode().then(() => showRevealed(seq), () => showRevealed(seq));
    });
  });

  const accentStyle = $derived(
    accent ? `--accent: ${accent.accent}; --accent-soft: ${accent.accentSoft}; --text-on-accent: ${accent.onAccent};` : ""
  );
</script>

<!-- Cover, title and lyrics laid out by the content tier (see now-playing.css); no seek bar here,
     the player bar has it. -->
<section class="np" aria-labelledby="now-playing-heading" data-testid="view-now-playing" style={accentStyle}>
  <HeroWash art={heroArt} {mockMode} full />

  {#if !track}
    <div class="state-block" data-testid="now-playing-empty">
      <Icon name="disc" class="w-10 h-10" style="color: var(--text-muted);" />
      <h1 id="now-playing-heading" class="type-lg font-semibold m-0">{t.nowPlaying.noTrack}</h1>
      <p class="type-sm m-0" style="color: var(--text-secondary);">{t.nowPlaying.noTrackHint}</p>
    </div>
  {:else}
    <div class="np-stage" class:has-lyrics={previewLines.length > 0}>
      <div
        class="np-cover"
        class:np-wait={reveal === "wait"}
        class:np-reveal={reveal === "in"}
        bind:this={coverWrap}
        data-testid="np-cover-wrap"
      >
        <Cover cover={track.cover} seed={track.videoId} label={track.title} {mockMode} class="np-cover-art" />
      </div>

      <div class="np-info">
        <div class="np-head" class:np-wait={reveal === "wait"} class:np-reveal={reveal === "in"}>
          {#if playingFrom}
            <p class="np-from" data-testid="np-playing-from">{t.nowPlaying.playingFrom.replace("{context}", playingFrom)}</p>
          {/if}
          <h1 id="now-playing-heading" class="np-title" lang={detectCjkLangHint(track.title)} data-testid="now-playing-title">
            {track.title}
          </h1>
          <p class="np-sub">
            {#each track.artists ?? [] as artist, i (`${artist.id ?? artist.name}::${i}`)}
              {#if i > 0}<span aria-hidden="true">{" / "}</span>{/if}
              {#if artist.id && canOpenArtist}
                <button type="button" class="link" onclick={() => onOpenArtist(artist.id!)} data-testid="np-artist-link">
                  {artist.name}
                </button>
              {:else}
                <span>{artist.name}</span>
              {/if}
            {/each}
            {#if track.album}
              <span aria-hidden="true">{" · "}</span>
              {#if albumId && canOpenAlbum}
                <button type="button" class="link" onclick={() => onOpenAlbum(albumId)} data-testid="np-album-link">
                  {track.album.title}
                </button>
              {:else}
                <span>{track.album.title}</span>
              {/if}
            {/if}
          </p>
        </div>

        <div class="np-actions">
          {#if likeGate.enabled}
            <button
              type="button"
              class="btn-icon"
              class:btn-icon-active={liked === true}
              aria-pressed={liked === true}
              aria-busy={likePending}
              aria-label={liked === true ? t.track.unlike : t.track.like}
              title={liked === true ? t.track.unlike : t.track.like}
              onclick={() => {
                if (!likePending) onToggleLike();
              }}
              data-testid="np-like"
            >
              <Icon name="heart" filled={liked === true} />
            </button>
          {/if}
          {#if playlistGate.enabled && onAddToPlaylist}
            <button
              type="button"
              class="btn-icon"
              aria-label={t.track.addToPlaylist}
              title={t.track.addToPlaylist}
              onclick={onAddToPlaylist}
              data-testid="np-add-to-playlist"
            >
              <Icon name="plus" />
            </button>
          {/if}
          <button
            type="button"
            class="btn-icon"
            aria-label={t.track.actions}
            title={t.track.actions}
            onclick={onOpenActions}
            data-testid="np-actions"
          >
            <Icon name="more" />
          </button>
          <QualityBadge quality={player?.audioQuality} codec={player?.codec} bitrateKbps={player?.audioBitrateKbps} size="lg" class="np-quality" />
        </div>
      </div>

      {#if previewLines.length > 0}
        <!-- The whole preview opens the Lyrics page; the lines inside are its picture, not its name. -->
        <button
          type="button"
          class="np-lyrics"
          aria-label={t.nowPlaying.openLyrics}
          onclick={onOpenLyrics}
          lang={lyrics?.language ?? undefined}
          data-testid="np-lyrics-preview"
        >
          {#each previewLines as line (line.index)}
            <span
              class="np-line"
              class:is-current={line.distance === 0}
              class:is-near={Math.abs(line.distance) === 1}
              data-distance={line.distance}
              style="--np-d: {line.distance};"
              data-testid="np-lyrics-line"
            >{line.text}</span>
          {/each}
        </button>
      {/if}

      <aside class="np-up-next" aria-labelledby="np-up-next-heading" data-testid="np-up-next">
        <h2 id="np-up-next-heading" class="np-up-next-heading">{t.nowPlaying.upNext}</h2>
        {#if upNextRows.length > 0}
          <ol class="np-up-next-list">
            {#each upNextRows as row (row.key)}
              <li class="np-up-next-row" data-testid="np-up-next-row" data-suggestion={row.suggestion ? "true" : "false"}>
                <Cover cover={row.track.cover} seed={row.track.videoId} label={row.track.title} {mockMode} class="np-up-next-art" />
                <span class="min-w-0 flex-1">
                  <span class="block truncate np-up-next-title" lang={detectCjkLangHint(row.track.title)} title={row.track.title}>{row.track.title}</span>
                  <span class="block truncate np-up-next-artist">
                    {row.suggestion ? `${t.nowPlaying.upNextSuggestion} · ${formatArtists(row.track)}` : formatArtists(row.track)}
                  </span>
                </span>
              </li>
            {/each}
          </ol>
        {:else if queueIdx >= 0}
          <p class="np-up-next-note" data-testid="np-up-next-empty">{t.nowPlaying.upNextEmpty}</p>
        {/if}
        {#if upNextSource}
          <p class="np-up-next-note" data-testid="np-up-next-source">{upNextSource}</p>
        {/if}
      </aside>
    </div>
  {/if}
</section>
