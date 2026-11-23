<script lang="ts">
  import type { PlaylistSummary, QueryError } from "@ytm-glass/protocol";
  import type { PageView } from "../catalog/page-state.js";
  import { t } from "../i18n/index.svelte.js";
  import Icon from "./Icon.svelte";
  import PageState from "./PageState.svelte";
  import TileGrid from "./TileGrid.svelte";

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
</script>

<section class="flex flex-col gap-5" aria-labelledby="library-heading" data-testid="view-library">
  <h1 id="library-heading" class="type-xl font-bold tracking-tight">{t.library.title}</h1>

  <div class="grid grid-cols-1 sm:grid-cols-2 gap-3">
    <button type="button" class="tile" onclick={onOpenLiked} data-testid="library-open-liked">
      <span
        class="h-12 w-12 rounded-lg shrink-0 grid place-items-center"
        style="background-color: var(--accent-soft); color: var(--accent);"
      >
        <Icon name="heart" class="w-6 h-6" />
      </span>
      <span class="min-w-0 flex-1">
        <span class="block truncate type-base font-semibold">{t.nav.liked}</span>
        <span class="block truncate type-xs mt-0.5" style="color: var(--text-secondary);">
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
        <span class="block truncate type-base font-semibold">{t.nav.recent}</span>
        <span class="block truncate type-xs mt-0.5" style="color: var(--text-secondary);">
          {t.library.recentSubtitle}
        </span>
      </span>
      <Icon name="chevronRight" style="color: var(--text-muted);" />
    </button>
  </div>

  <PageState {view} name="library" {onRetry} {onOpenOfficialPlayer}>
    <TileGrid heading={t.library.playlistsHeading} {tiles} {mockMode} />
    {#if moreError}
      <div role="alert" class="type-sm" style="color: var(--status-danger);">
        [{moreError.code}] {moreError.safeMessage}
      </div>
    {/if}
    {#if hasMore}
      <div class="flex justify-center">
        <button type="button" class="btn-secondary" disabled={loadingMore} onclick={onLoadMore}>
          {loadingMore ? t.common.loadingMore : t.common.loadMore}
        </button>
      </div>
    {/if}
  </PageState>
</section>
