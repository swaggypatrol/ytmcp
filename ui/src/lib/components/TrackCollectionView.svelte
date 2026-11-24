<script lang="ts">
  import type { CoverRef, QueryError, Track } from "@ytmcp/protocol";
  import type { Snippet } from "svelte";
  import type { PageView } from "../catalog/page-state.js";
  import { t } from "../i18n/index.svelte.js";
  import Cover from "./Cover.svelte";
  import { isPlayable } from "./format.js";
  import HeroWash from "./HeroWash.svelte";
  import Icon, { type IconName } from "./Icon.svelte";
  import PageState from "./PageState.svelte";
  import VirtualizedTrackList, { pageScroller, STRIP_PX, type TrackRowLike, type TrackRowPlayback } from "./VirtualizedTrackList.svelte";

  interface Props {
    /** Test id / page name, e.g. "playlist", "album", "liked". */
    name: string;
    title: string;
    subtitle?: string | null;
    cover?: CoverRef | null;
    coverSeed?: string;
    /** Glyph for the cover tile of a collection without artwork (Liked songs, Recently played). */
    icon?: IconName;
    /** Round cover tile (artists). */
    roundCover?: boolean;
    view: PageView;
    tracks: readonly Track[];
    hasMore: boolean;
    /** The collection's song total when a summary already names it (shown before every page is loaded). */
    total?: number | null;
    loadingMore: boolean;
    moreError: QueryError | null;
    currentVideoId: string | null;
    mockMode: boolean;
    /**
     * Shows Play (in the header and the condensed strip). Pass it only where `onPlayTrack` plays
     * with the collection as context, so Play really plays the collection.
     */
    playAll?: boolean;
    /** A heading over the list (e.g. an artist's "Top songs"); replaces the song count line. */
    listHeading?: string | null;
    like?: TrackRowLike;
    nowPlaying?: TrackRowPlayback;
    onBack?: () => void;
    onRetry: () => void;
    onOpenOfficialPlayer: () => void;
    onLoadMore: () => void;
    onPlayTrack: (track: Track, index: number) => void;
    onTrackActions: (track: Track, index: number) => void;
    /** Extra header content (artist links, notes, mock-only tools). */
    header?: Snippet;
    /** Sections rendered after the track list (e.g. an artist's albums). */
    footer?: Snippet;
  }

  let {
    name,
    title,
    subtitle = null,
    cover = null,
    coverSeed,
    icon,
    roundCover = false,
    view,
    tracks,
    hasMore,
    total = null,
    loadingMore,
    moreError,
    currentVideoId,
    mockMode,
    playAll = false,
    listHeading = null,
    like,
    nowPlaying,
    onBack,
    onRetry,
    onOpenOfficialPlayer,
    onLoadMore,
    onPlayTrack,
    onTrackActions,
    header,
    footer,
  }: Props = $props();

  // The collection's own cover, or failing that the artwork of its first tracks.
  const heroArt = $derived(
    coverSeed
      ? [{ cover, seed: coverSeed, label: title }]
      : tracks.slice(0, 3).map((t) => ({ cover: t.cover, seed: t.videoId, label: t.title }))
  );

  const ready = $derived(view.kind === "ready");
  const firstPlayable = $derived(tracks.findIndex((track) => isPlayable(track)));
  const canPlay = $derived(playAll && ready && firstPlayable >= 0);
  /** The song total: the known `total` while pages remain, the loaded count once every page is in; else nothing. */
  const countLine = $derived.by(() => {
    if (!ready || listHeading || tracks.length === 0) return null;
    const count = !hasMore ? tracks.length : typeof total === "number" && total > 0 ? Math.max(total, tracks.length) : null;
    if (count === null) return null;
    return count === 1 ? t.common.songCountOne : t.common.songCount.replace("{count}", count.toLocaleString());
  });

  function play() {
    const track = tracks[firstPlayable];
    if (canPlay && track) onPlayTrack(track, firstPlayable);
  }

  let heroEl = $state<HTMLElement | null>(null);
  let condensed = $state(false);

  // The strip shows once the header's title has scrolled up behind it.
  $effect(() => {
    const hero = heroEl;
    if (!hero) return;
    const root = pageScroller(hero);
    const target: EventTarget = root === document.documentElement || root === document.scrollingElement ? window : root;
    let frame = 0;
    const check = () => {
      frame = 0;
      const titleEl = hero.querySelector(".collection-title") ?? hero;
      const top = target === window ? 0 : root.getBoundingClientRect().top;
      condensed = titleEl.getBoundingClientRect().bottom < top + STRIP_PX;
    };
    const onScroll = () => {
      if (!frame) frame = typeof requestAnimationFrame === "function" ? requestAnimationFrame(check) : (check(), 0);
    };
    check();
    target.addEventListener("scroll", onScroll, { passive: true });
    return () => {
      target.removeEventListener("scroll", onScroll);
      if (frame && typeof cancelAnimationFrame === "function") cancelAnimationFrame(frame);
    };
  });
</script>

<section aria-labelledby={`${name}-heading`} data-testid={`view-${name}`}>
  <div class="collection-strip-anchor">
    <div
      class="collection-strip"
      class:is-shown={condensed}
      inert={!condensed}
      aria-hidden={!condensed}
      data-testid={`${name}-strip`}
    >
      {#if canPlay}
        <button
          type="button"
          class="btn-primary collection-strip-play"
          aria-label={t.track.playTitle.replace("{title}", title)}
          onclick={play}
          data-testid={`${name}-strip-play`}
        >
          <Icon name="play" filled />
        </button>
      {/if}
      <span class="collection-strip-title">{title}</span>
    </div>
  </div>

  <div class="page-hero collection-hero" bind:this={heroEl}>
    <HeroWash art={heroArt} {mockMode} />
    {#if coverSeed}
      <Cover {cover} seed={coverSeed} label={title} {mockMode} class="collection-tile{roundCover ? ' is-round' : ''}" />
    {:else if icon}
      <!-- Rose only for Liked songs: elsewhere the tile stays neutral. -->
      <div class="collection-tile collection-tile-icon" class:is-liked={icon === "heart"} aria-hidden="true"><Icon name={icon} filled /></div>
    {/if}
    <div class="collection-meta">
      {#if onBack}
        <button type="button" class="btn-ghost self-start" onclick={onBack} data-testid={`${name}-back`}>
          <Icon name="arrowLeft" />
          <span>{t.app.back}</span>
        </button>
      {/if}
      <h1 id={`${name}-heading`} class="collection-title">{title}</h1>
      {#if subtitle || countLine}
        <p class="collection-count" data-testid={`${name}-count`}>
          {[subtitle, countLine].filter(Boolean).join(" · ")}
        </p>
      {/if}
      {#if header}{@render header()}{/if}
    </div>
    {#if canPlay}
      <div class="collection-actions">
        <button type="button" class="btn-primary" onclick={play} data-testid={`${name}-play`}>
          <Icon name="play" filled />
          <span>{t.track.play}</span>
        </button>
      </div>
    {/if}
  </div>

  <PageState {view} {name} {onRetry} {onOpenOfficialPlayer}>
    {#if listHeading}
      <h2 class="list-section-title mb-4">{listHeading}</h2>
    {/if}
    <VirtualizedTrackList
      {tracks}
      {currentVideoId}
      {mockMode}
      {like}
      {nowPlaying}
      showHeader
      ariaLabel={title}
      {hasMore}
      {loadingMore}
      moreFailed={moreError !== null}
      {onLoadMore}
      onPlayTrack={(track, idx) => onPlayTrack(track, idx)}
      onTrackActions={(track, idx) => onTrackActions(track, idx)}
    />
  </PageState>

  {#if footer}<div class="mt-12">{@render footer()}</div>{/if}
</section>
