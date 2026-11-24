<script lang="ts">
  import type { AlbumRef, ArtistRef, PlaylistSummary, QueryError, Track } from "@ytmcp/protocol";
  import { untrack } from "svelte";
  import type { PageView } from "../catalog/page-state.js";
  import { t } from "../i18n/index.svelte.js";
  import { recentSearches } from "../recent-searches.svelte.js";
  import CoverShelf from "./CoverShelf.svelte";
  import { detectCjkLangHint, formatArtists, isPlayable } from "./format.js";
  import Icon from "./Icon.svelte";
  import PageState from "./PageState.svelte";
  import TileGrid, { type Tile } from "./TileGrid.svelte";
  import VirtualizedTrackList, { type TrackRowLike, type TrackRowPlayback } from "./VirtualizedTrackList.svelte";

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
    /** Like toggles for the song rows; pass it only while likes can be written. */
    like?: TrackRowLike;
    /** The playing row's indicator. */
    nowPlaying?: TrackRowPlayback;
    mockMode: boolean;
    /** Search cannot be requested (unsupported or sign-in required): input off, state shown. */
    blocked: boolean;
    /** The catalog account key; recent searches belong to it. */
    accountKey: string;
    /** What was really played lately, for "Jump back in". */
    recentTracks?: readonly Track[];
    /** The account's playlists, for "From your library". */
    libraryPlaylists?: readonly PlaylistSummary[];
    smoothScroll?: boolean;
    /** The data behind the start blocks is not loaded (never, or dropped by an invalidation). */
    startDataMissing?: boolean;
    /** Asks for the data behind the blocks shown before typing. */
    onRequestStartData?: () => void;
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
    like,
    nowPlaying,
    mockMode,
    blocked,
    accountKey,
    recentTracks = [],
    libraryPlaylists = [],
    smoothScroll = true,
    startDataMissing = false,
    onRequestStartData,
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

  /** "Jump back in" shows at most this many songs. */
  const JUMP_BACK_LIMIT = 6;

  // Starts from the shown results, so coming back to them does not first show the start blocks.
  let inputValue = $state(untrack(() => query));
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

  // A list made by another account (or before a sign-out) is dropped, never shown.
  $effect(() => {
    const key = accountKey;
    untrack(() => recentSearches.sync(key));
  });

  // The start blocks need Home's data: asked again whenever the account changes or the data is
  // dropped. Sent after this update settles, so an account change has already cleared the old
  // account's pages when the request goes out.
  $effect(() => {
    void accountKey;
    void startDataMissing;
    const request = onRequestStartData;
    if (blocked || !request) return;
    queueMicrotask(request);
  });

  /** Before typing: recent searches, recent plays and the library. Typing swaps in results. */
  const showStart = $derived(!blocked && inputValue.trim() === "");
  const recentQueries = $derived(recentSearches.itemsFor(accountKey));

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

  /** Runs a query now and remembers it: the user asked for exactly this. */
  function runNow(text: string) {
    if (debounceTimer) clearTimeout(debounceTimer);
    onSearch(text);
    if (!blocked) recentSearches.record(accountKey, text);
  }

  /** A chip runs its query again; focus goes to the field, as the chips give way to results. */
  function runRecent(text: string) {
    inputValue = text;
    runNow(text);
    inputEl?.focus();
  }

  function clearRecent() {
    recentSearches.clear();
    inputEl?.focus();
  }

  /** Acting on a result means the query that found it was worth keeping. */
  function rememberQuery() {
    recentSearches.record(accountKey, query);
  }

  function openAlbum(id: string) {
    rememberQuery();
    onOpenAlbum(id);
  }

  function openArtist(id: string) {
    rememberQuery();
    onOpenArtist(id);
  }

  function openPlaylist(id: string) {
    rememberQuery();
    onOpenPlaylist(id);
  }

  function playResult(track: Track) {
    rememberQuery();
    onPlayTrack(track);
  }

  const albumTiles = $derived(
    albums.flatMap((album) => {
      const id = album.id ?? album.browseId;
      return id ? [{ key: id, title: album.title, testId: `search-album-${id}`, onOpen: () => openAlbum(id) }] : [];
    })
  );
  const artistTiles = $derived(
    artists.flatMap((artist) =>
      artist.id
        ? [{ key: artist.id, title: artist.name, testId: `search-artist-${artist.id}`, onOpen: () => openArtist(artist.id!) }]
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
      onOpen: () => openPlaylist(pl.playlistId),
    }))
  );

  /** Distinct, playable recent songs; choosing one plays it again. */
  const jumpTiles = $derived.by(() => {
    const seen = new Set<string>();
    const out: Tile[] = [];
    for (const track of recentTracks) {
      if (out.length >= JUMP_BACK_LIMIT) break;
      if (seen.has(track.videoId) || !isPlayable(track)) continue;
      seen.add(track.videoId);
      out.push({
        key: track.videoId,
        title: track.title,
        subtitle: track.artists?.length ? formatArtists(track) : null,
        cover: track.cover,
        label: t.track.playTitle.replace("{title}", track.title),
        testId: `search-jump-${track.videoId}`,
        onOpen: () => onPlayTrack(track),
      });
    }
    return out;
  });
  const libraryTiles = $derived(
    libraryPlaylists.map((pl) => ({
      key: pl.playlistId,
      title: pl.title,
      subtitle: pl.subtitle ?? (pl.trackCount != null ? t.common.trackCount.replace("{count}", String(pl.trackCount)) : null),
      cover: pl.cover,
      testId: `search-library-${pl.playlistId}`,
      onOpen: () => onOpenPlaylist(pl.playlistId),
    }))
  );
</script>

<section class="browse-page" aria-labelledby="search-heading" data-testid="view-search">
  <header class="browse-head page-hero">
    <h1 id="search-heading" class="browse-title">{t.search.title}</h1>
  </header>

  <form
    class="search-form"
    role="search"
    onsubmit={(e) => {
      e.preventDefault();
      if (!isComposing) runNow(inputValue);
    }}
  >
    <div class="search-field search-bar">
      <Icon name="search" style="color: var(--text-secondary);" />
      <input
        bind:this={inputEl}
        id="ytm-search-input"
        type="search"
        value={inputValue}
        disabled={blocked}
        placeholder={t.search.placeholder}
        aria-label={t.search.inputLabel}
        aria-keyshortcuts="Control+F"
        class="type-base"
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
      {:else if !blocked}
        <span class="search-bar-hint" aria-hidden="true">{t.search.shortcutHint}</span>
      {/if}
    </div>
  </form>

  <div class="search-stage">
    <div class="search-pane browse-page" class:is-active={showStart} inert={!showStart} data-testid="search-start">
      {#if recentQueries.length > 0}
        <div class="browse-block" data-testid="search-recent">
          <div class="browse-block-head">
            <h2 class="type-lg font-semibold">{t.search.recentSearches}</h2>
            <button
              type="button"
              class="btn-ghost"
              aria-label={t.search.clearRecent}
              onclick={clearRecent}
              data-testid="search-recent-clear"
            >
              {t.common.clear}
            </button>
          </div>
          <div class="recent-search-chips">
            {#each recentQueries as text (text)}
              <button
                type="button"
                class="chip"
                lang={detectCjkLangHint(text)}
                onclick={() => runRecent(text)}
                data-testid="search-recent-chip"
              >
                <Icon name="clock" class="w-3.5 h-3.5" />{text}
              </button>
            {/each}
          </div>
        </div>
      {/if}
      <TileGrid heading={t.search.jumpBackIn} tiles={jumpTiles} {mockMode} maxThree />
      <CoverShelf heading={t.search.fromLibrary} tiles={libraryTiles} {mockMode} {smoothScroll} />
    </div>

    <div class="search-pane browse-page" class:is-active={!showStart} inert={showStart} data-testid="search-results">
      {#if query || blocked}
        <PageState {view} name="search" emptyText={t.search.noResults} {onRetry} {onOpenOfficialPlayer}>
          <TileGrid heading={t.search.artists} tiles={artistTiles} {mockMode} round />
          <TileGrid heading={t.search.albums} tiles={albumTiles} {mockMode} />
          <TileGrid heading={t.search.playlists} tiles={playlistTiles} {mockMode} />

          {#if songs.length > 0}
            <div class="browse-block">
              <div class="browse-block-head">
                <h2 class="type-lg font-semibold">{t.search.songs}</h2>
              </div>
              <VirtualizedTrackList
                tracks={songs}
                {currentVideoId}
                {mockMode}
                showHeader
                ariaLabel={t.search.songs}
                onPlayTrack={(track) => playResult(track)}
                onTrackActions={(track) => onTrackActions(track)}
                {like}
                {nowPlaying}
                {hasMore}
                {loadingMore}
                moreFailed={moreError !== null}
                {onLoadMore}
              />
            </div>
          {/if}
        </PageState>
      {/if}
    </div>
  </div>
</section>
