<script lang="ts">
  import type { CoverRef, QueryError, Track } from "@ytm-glass/protocol";
  import type { Snippet } from "svelte";
  import type { PageView } from "../catalog/page-state.js";
  import { t } from "../i18n/index.svelte.js";
  import Cover from "./Cover.svelte";
  import HeroWash from "./HeroWash.svelte";
  import Icon from "./Icon.svelte";
  import PageState from "./PageState.svelte";
  import VirtualizedTrackList from "./VirtualizedTrackList.svelte";

  interface Props {
    /** Test id / page name, e.g. "playlist", "album", "liked". */
    name: string;
    title: string;
    subtitle?: string | null;
    cover?: CoverRef | null;
    coverSeed?: string;
    view: PageView;
    tracks: readonly Track[];
    hasMore: boolean;
    loadingMore: boolean;
    moreError: QueryError | null;
    currentVideoId: string | null;
    mockMode: boolean;
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
    view,
    tracks,
    hasMore,
    loadingMore,
    moreError,
    currentVideoId,
    mockMode,
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
</script>

<section class="flex flex-col gap-5" aria-labelledby={`${name}-heading`} data-testid={`view-${name}`}>
  <div class="page-hero flex flex-wrap items-end gap-5">
    <HeroWash art={heroArt} {mockMode} />
    {#if coverSeed}
      <Cover {cover} seed={coverSeed} label={title} {mockMode} class="h-36 w-36 surface-rounded-lg shadow-xl" />
    {/if}
    <div class="min-w-0 flex-1 flex flex-col gap-1.5">
      {#if onBack}
        <button type="button" class="btn-ghost self-start" onclick={onBack} data-testid={`${name}-back`}>
          <Icon name="arrowLeft" />
          <span>{t.app.back}</span>
        </button>
      {/if}
      <h1 id={`${name}-heading`} class="type-xl font-bold tracking-tight break-words">{title}</h1>
      {#if subtitle}
        <div class="type-sm" style="color: var(--text-secondary);">{subtitle}</div>
      {/if}
      {#if header}{@render header()}{/if}
    </div>
  </div>

  <PageState {view} {name} {onRetry} {onOpenOfficialPlayer}>
    <div class="type-xs tabular-nums" style="color: var(--text-secondary);">
      {t.common.loadedCount.replace("{count}", String(tracks.length))}
    </div>
    <VirtualizedTrackList
      {tracks}
      {currentVideoId}
      {mockMode}
      showHeader
      ariaLabel={title}
      onPlayTrack={(track, idx) => onPlayTrack(track, idx)}
      onTrackActions={(track, idx) => onTrackActions(track, idx)}
    />
    {#if moreError}
      <div role="alert" class="type-sm" style="color: var(--status-danger);">
        [{moreError.code}] {moreError.safeMessage}
      </div>
    {/if}
    {#if hasMore}
      <div class="flex justify-center">
        <button
          type="button"
          class="btn-secondary"
          disabled={loadingMore}
          onclick={onLoadMore}
          data-testid={`${name}-load-more`}
        >
          {loadingMore ? t.common.loadingMore : t.common.loadMore}
        </button>
      </div>
    {/if}
  </PageState>

  {#if footer}{@render footer()}{/if}
</section>
