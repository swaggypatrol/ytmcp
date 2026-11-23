<script lang="ts">
  import type { AlbumRef, ArtistRef, PlaylistSummary, QueryError, Track } from "@ytm-glass/protocol";
  import { untrack } from "svelte";
  import type { PageView } from "../catalog/page-state.js";
  import { t } from "../i18n/index.svelte.js";
  import Icon from "./Icon.svelte";
  import PageState from "./PageState.svelte";
  import TileGrid from "./TileGrid.svelte";
  import VirtualizedTrackList from "./VirtualizedTrackList.svelte";

  interface Props {
    /** The query of the current results (store key); empty when none. */
    query: string;
    view: PageView;
    songs: readonly Track[];
    albums: readonly AlbumRef[];
    artists: readonly ArtistRef[];
    playlists: readonly PlaylistSummary[];
    hasMore: boolean;
    loadingMore: boolean;
    moreError: QueryError | null;
    currentVideoId: string | null;
    mockMode: boolean;
    /** Search cannot be requested (unsupported or sign-in required): input off, state shown. */
    blocked: boolean;
    onSearch: (query: string) => void;
    onLoadMore: () => void;
    onPlayTrack: (track: Track) => void;
    onTrackActions: (track: Track) => void;
    onOpenAlbum: (albumId: string) => void;
    onOpenArtist: (artistId: string) => void;
    onOpenPlaylist: (playlistId: string) => void;
    onRetry: () => void;
    onOpenOfficialPlayer: () => void;
  }

  let {
    query,
    view,
    songs,
    albums,
    artists,
    playlists,
    hasMore,
    loadingMore,
    moreError,
    currentVideoId,
    mockMode,
    blocked,
    onSearch,
    onLoadMore,
    onPlayTrack,
    onTrackActions,
    onOpenAlbum,
    onOpenArtist,
    onOpenPlaylist,
    onRetry,
    onOpenOfficialPlayer,
  }: Props = $props();

  let inputValue = $state("");
  let isComposing = $state(false);
  let debounceTimer: ReturnType<typeof setTimeout> | null = null;
  let inputEl = $state<HTMLInputElement | null>(null);

  // Follow the store: a cleared search (e.g. account change) also clears the field.
  $effect(() => {
    const q = query;
    if (untrack(() => inputValue.trim()) !== q) inputValue = q;
  });

  $effect(() => () => {
    if (debounceTimer) clearTimeout(debounceTimer);
  });

  function scheduleSearch(next: string) {
    if (debounceTimer) clearTimeout(debounceTimer);
    debounceTimer = setTimeout(() => onSearch(next), 250);
  }

  function clearSearch() {
    if (debounceTimer) clearTimeout(debounceTimer);
    inputValue = "";
    onSearch("");
    inputEl?.focus();
  }

  const albumTiles = $derived(
    albums.flatMap((album) => {
      const id = album.id ?? album.browseId;
      return id
        ? [{ key: id, title: album.title, testId: `search-album-${id}`, onOpen: () => onOpenAlbum(id) }]
        : [];
    })
  );
  const artistTiles = $derived(
    artists.flatMap((artist) =>
      artist.id
        ? [{ key: artist.id, title: artist.name, testId: `search-artist-${artist.id}`, onOpen: () => onOpenArtist(artist.id!) }]
        : []
    )
  );
  const playlistTiles = $derived(
    playlists.map((pl) => ({
      key: pl.playlistId,
      title: pl.title,
      subtitle: pl.subtitle,
      cover: pl.cover,
      testId: `search-playlist-${pl.playlistId}`,
      onOpen: () => onOpenPlaylist(pl.playlistId),
    }))
  );
</script>

<section class="flex flex-col gap-5" aria-labelledby="search-heading" data-testid="view-search">
  <h1 id="search-heading" class="type-xl font-bold tracking-tight">{t.search.title}</h1>

  <form
    class="flex flex-wrap items-center gap-3"
    role="search"
    onsubmit={(e) => {
      e.preventDefault();
      if (!isComposing) {
        if (debounceTimer) clearTimeout(debounceTimer);
        onSearch(inputValue);
      }
    }}
  >
    <div class="search-field surface-rounded-md">
      <Icon name="search" style="color: var(--text-muted);" />
      <input
        bind:this={inputEl}
        id="ytm-search-input"
        type="search"
        value={inputValue}
        disabled={blocked}
        placeholder={t.search.placeholder}
        aria-label={t.search.inputLabel}
        class="type-sm"
        oncompositionstart={() => {
          isComposing = true;
        }}
        oncompositionend={(e) => {
          isComposing = false;
          inputValue = (e.currentTarget as HTMLInputElement).value;
          scheduleSearch(inputValue);
        }}
        oninput={(e) => {
          inputValue = (e.currentTarget as HTMLInputElement).value;
          if (!isComposing) scheduleSearch(inputValue);
        }}
        data-testid="search-input"
      />
      {#if inputValue && !blocked}
        <button
          type="button"
          class="btn-icon"
          aria-label={t.search.clear}
          title={t.search.clear}
          onclick={clearSearch}
          data-testid="search-clear-btn"
        >
          <Icon name="close" />
        </button>
      {/if}
    </div>
    {#if isComposing}
      <span class="chip type-xs">{t.search.imeComposingHint}</span>
    {/if}
  </form>

  {#if !query && !blocked}
    <div class="state-block">
      <Icon name="search" class="w-9 h-9" style="color: var(--text-muted);" />
      <p class="type-sm m-0" style="color: var(--text-primary);">{t.search.promptEmpty}</p>
      <p class="type-xs m-0" style="color: var(--text-muted);">{t.search.shortcutsTip}</p>
    </div>
  {:else}
    <PageState {view} name="search" emptyText={t.search.noResults} {onRetry} {onOpenOfficialPlayer}>
      <TileGrid heading={t.search.artists} tiles={artistTiles} {mockMode} round />
      <TileGrid heading={t.search.albums} tiles={albumTiles} {mockMode} />
      <TileGrid heading={t.search.playlists} tiles={playlistTiles} {mockMode} />

      {#if songs.length > 0}
        <div class="flex flex-col gap-3">
          <div class="flex items-baseline justify-between gap-3">
            <h2 class="type-lg font-semibold">{t.search.songs}</h2>
            <span class="type-xs tabular-nums" style="color: var(--text-secondary);">
              {t.search.resultCount.replace("{count}", String(songs.length))}
            </span>
          </div>
          <VirtualizedTrackList
            tracks={songs}
            {currentVideoId}
            {mockMode}
            showHeader
            ariaLabel={t.search.songs}
            onPlayTrack={(track) => onPlayTrack(track)}
            onTrackActions={(track) => onTrackActions(track)}
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
                data-testid="search-load-more"
              >
                {loadingMore ? t.common.loadingMore : t.common.loadMore}
              </button>
            </div>
          {/if}
        </div>
      {/if}
    </PageState>
  {/if}
</section>
