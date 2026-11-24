<script lang="ts">
  import type { PlaylistSummary, QueryError } from "@ytmcp/protocol";
  import { untrack } from "svelte";
  import type { PageView } from "../catalog/page-state.js";
  import { t } from "../i18n/index.svelte.js";
  import Icon from "./Icon.svelte";
  import PageState from "./PageState.svelte";
  import TileGrid from "./TileGrid.svelte";
  import { pageScroller } from "./VirtualizedTrackList.svelte";

  interface Props {
    view: PageView;
    playlists: readonly PlaylistSummary[];
    hasMore: boolean;
    loadingMore: boolean;
    moreError: QueryError | null;
    mockMode: boolean;
    onOpenPlaylist: (playlistId: string) => void;
    onOpenLiked: () => void;
    onOpenRecent: () => void;
    /** Asked for the next page once the end comes within 1.5 screens; Retry asks again after a failure. */
    onLoadMore: () => void;
    onRetry: () => void;
    onOpenOfficialPlayer: () => void;
  }

  let {
    view,
    playlists,
    hasMore,
    loadingMore,
    moreError,
    mockMode,
    onOpenPlaylist,
    onOpenLiked,
    onOpenRecent,
    onLoadMore,
    onRetry,
    onOpenOfficialPlayer,
  }: Props = $props();

  const tiles = $derived(
    playlists.map((pl) => ({
      key: pl.playlistId,
      title: pl.title,
      subtitle: pl.subtitle ?? (pl.trackCount != null ? t.common.trackCount.replace("{count}", String(pl.trackCount)) : null),
      cover: pl.cover,
      testId: `playlist-card-${pl.playlistId}`,
      onOpen: () => onOpenPlaylist(pl.playlistId),
    }))
  );

  let tail = $state<HTMLElement | null>(null);

  /**
   * Asks for the next page while the end of the grid is still 1.5 screens below the visible
   * part of the page. Never after a failure: the tail's Retry is the only way to ask again.
   */
  function fetchAhead(): void {
    const el = tail;
    if (!el || !hasMore || loadingMore || moreError) return;
    const root = pageScroller(el);
    const isDocument = root === document.documentElement || root === document.scrollingElement;
    const viewport = isDocument || root.clientHeight <= 0 ? window.innerHeight : root.clientHeight;
    const visibleBottom = (isDocument ? 0 : root.getBoundingClientRect().top) + viewport;
    if (el.getBoundingClientRect().top - visibleBottom < viewport * 1.5) onLoadMore();
  }

  // Re-checked when a page arrives or the page or paging state changes (a short first page
  // never scrolls; a request made during a refresh is dropped by the store and asked again).
  $effect(() => {
    void [tail, playlists, hasMore, loadingMore, moreError, view];
    untrack(fetchAhead);
  });

  // And while the page scrolls or the window resizes.
  $effect(() => {
    const el = tail;
    if (!el) return;
    const root = pageScroller(el);
    const target: HTMLElement | Window = root === document.documentElement || root === document.scrollingElement ? window : root;
    target.addEventListener("scroll", fetchAhead, { passive: true });
    window.addEventListener("resize", fetchAhead, { passive: true });
    return () => {
      target.removeEventListener("scroll", fetchAhead);
      window.removeEventListener("resize", fetchAhead);
    };
  });
</script>

<!-- Library: Liked songs and Recently played first, then the playlists, on the browse ruler. -->
<section class="browse-page" aria-labelledby="library-heading" data-testid="view-library">
  <header class="browse-head page-hero">
    <h1 id="library-heading" class="browse-title">{t.library.title}</h1>
  </header>

  <div class="tile-grid">
    <button type="button" class="tile" onclick={onOpenLiked} data-testid="library-open-liked">
      <span
        class="h-12 w-12 rounded-lg shrink-0 grid place-items-center"
        style="background-color: var(--accent-soft); color: var(--accent);"
      >
        <Icon name="heart" class="w-6 h-6" />
      </span>
      <span class="min-w-0 flex-1">
        <span class="block truncate type-base font-medium">{t.nav.liked}</span>
        <span class="block truncate type-sm" style="color: var(--text-secondary);">
          {t.library.likedSubtitle}
        </span>
      </span>
      <Icon name="chevronRight" style="color: var(--text-muted);" />
    </button>

    <button type="button" class="tile" onclick={onOpenRecent} data-testid="library-open-recent">
      <span
        class="h-12 w-12 rounded-lg shrink-0 grid place-items-center"
        style="background-color: var(--bg-subtle); color: var(--text-primary);"
      >
        <Icon name="clock" class="w-6 h-6" />
      </span>
      <span class="min-w-0 flex-1">
        <span class="block truncate type-base font-medium">{t.nav.recent}</span>
        <span class="block truncate type-sm" style="color: var(--text-secondary);">
          {t.library.recentSubtitle}
        </span>
      </span>
      <Icon name="chevronRight" style="color: var(--text-muted);" />
    </button>
  </div>

  <PageState {view} name="library" {onRetry} {onOpenOfficialPlayer}>
    <div class="browse-block">
      <TileGrid heading={t.library.playlistsHeading} {tiles} {mockMode} />
      {#if hasMore || moreError}
        <!-- One fixed-height row while more can load, so its states never move the page. -->
        <div class="browse-tail" bind:this={tail} data-testid="library-tail">
          <span class="browse-tail-note" class:is-failed={!!moreError} role="status" aria-live="polite">
            {moreError ? t.library.loadMoreFailed : loadingMore ? t.common.loadingMore : ""}
          </span>
          {#if moreError}
            <button type="button" class="btn-ghost" onclick={onLoadMore} data-testid="library-retry">{t.states.retry}</button>
          {/if}
        </div>
      {/if}
    </div>
  </PageState>
</section>
