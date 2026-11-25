<script module lang="ts">
  /** Songs shown before the tile groups; the rest are behind "All songs". */
  export const SONGS_PREVIEW_COUNT = 6;

  /**
   * The search (account and query) whose results were last switched to the full songs list, and
   * where the grouped view was scrolled to then. Kept outside the component so the page left for
   * lyrics and shown again comes back to the same list.
   */
  let allSongsFor: { readonly key: string; readonly groupedScrollTop: number } | null = null;
</script>

<script lang="ts">
  import type { AlbumRef, ArtistRef, PlaylistSummary, QueryError, Track } from "@ytmcp/protocol";
  import { tick, untrack } from "svelte";
  import type { PageView } from "../catalog/page-state.js";
  import { t } from "../i18n/index.svelte.js";
  import CoverShelf from "./CoverShelf.svelte";
  import { detectCjkLangHint, formatArtists, isPlayable } from "./format.js";
  import Icon from "./Icon.svelte";
  import PageState from "./PageState.svelte";
  import TileGrid, { type Tile } from "./TileGrid.svelte";
  import VirtualizedTrackList, { pageScroller, type TrackRowLike, type TrackRowPlayback } from "./VirtualizedTrackList.svelte";

  interface Props {
    /** The query of the current results (store key); empty when none. */
    query: string;
    /** The results shown are an earlier query's, kept on screen until this query's reply arrives. */
    replacing?: boolean;
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
    /** The catalog account key; a new one starts a new search and asks for the start data again. */
    accountKey: string;
    /** The account's recent searches, newest first (the core's list). */
    recentQueries?: readonly string[];
    /** Records a query the listener actually ran: submitted, picked from the list, or acted on. */
    onRememberQuery?: (query: string) => void;
    onClearRecent?: () => void;
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
    replacing = false,
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
    recentQueries = [],
    onRememberQuery,
    onClearRecent,
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
    if (!blocked) onRememberQuery?.(text);
  }

  /** A chip runs its query again; focus goes to the field, as the chips give way to results. */
  function runRecent(text: string) {
    inputValue = text;
    runNow(text);
    inputEl?.focus();
  }

  function clearRecent() {
    onClearRecent?.();
    inputEl?.focus();
  }

  /** Acting on a result means the query that found it was worth keeping. */
  function rememberQuery() {
    onRememberQuery?.(query);
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

  /** The full songs list exists only when there are more songs than the short block shows. */
  const hasAllSongs = $derived(songs.length > SONGS_PREVIEW_COUNT || hasMore);
  const searchKey = $derived(`${accountKey}\n${query}`);
  let allSongsOpen = $state(untrack(() => allSongsFor?.key === searchKey));
  /** Shown instead of the grouped results; a new search (or one with fewer songs) is grouped again. */
  const showAllSongs = $derived(allSongsOpen && allSongsFor?.key === searchKey && hasAllSongs);
  let resultsEl = $state<HTMLElement | null>(null);

  // A different search starts on the grouped view.
  $effect(() => {
    const key = searchKey;
    if (allSongsFor && allSongsFor.key !== key) {
      allSongsFor = null;
      allSongsOpen = false;
    }
  });

  /** Opens the full list at the top of the results, remembering where the grouped view was. */
  async function openAllSongs() {
    const root = resultsEl ? pageScroller(resultsEl) : null;
    allSongsFor = { key: searchKey, groupedScrollTop: root?.scrollTop ?? 0 };
    allSongsOpen = true;
    await tick();
    if (root) root.scrollTop = 0;
    resultsEl?.querySelector<HTMLElement>('[data-testid="search-songs-back"]')?.focus({ preventScroll: true });
  }

  /** Back to the grouped results where they were left, with focus on the control that left them. */
  async function closeAllSongs() {
    const scrollTop = allSongsFor?.groupedScrollTop ?? 0;
    allSongsFor = null;
    allSongsOpen = false;
    await tick();
    const root = resultsEl ? pageScroller(resultsEl) : null;
    if (root) root.scrollTop = scrollTop;
    resultsEl?.querySelector<HTMLElement>('[data-testid="search-all-songs"]')?.focus({ preventScroll: true });
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

  /**
   * The chips on screen. The list comes from the core and can arrive after the blocks below it
   * are already showing; a list that would then push them down waits until the start blocks
   * are out of view (typing, or leaving the page). Emptying (another account, Clear), a change
   * to chips already shown, and a list that moves nothing are shown at once.
   */
  let shownQueries = $state.raw<readonly string[]>(untrack(() => recentQueries));
  $effect(() => {
    const next = recentQueries;
    const visible = showStart;
    const belowShown = jumpTiles.length > 0 || libraryTiles.length > 0;
    untrack(() => {
      if (!visible || !belowShown || next.length === 0 || shownQueries.length > 0) shownQueries = next;
    });
  });
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
      {#if shownQueries.length > 0}
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
            {#each shownQueries as text (text)}
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

    <div
      class="search-pane browse-page"
      class:is-active={!showStart}
      class:is-replacing={replacing}
      inert={showStart}
      aria-busy={replacing || undefined}
      bind:this={resultsEl}
      data-testid="search-results"
    >
      {#if query || blocked}
        <PageState {view} name="search" emptyText={t.search.noResults} {onRetry} {onOpenOfficialPlayer}>
          {#if showAllSongs}
            <!-- The full list pages itself as its end comes near, so nothing is placed below it. -->
            <div class="browse-block" data-testid="search-all-songs-list">
              <div class="browse-block-head">
                <h2 class="type-lg font-semibold">{t.search.songs}</h2>
                <button type="button" class="btn-ghost" onclick={() => void closeAllSongs()} data-testid="search-songs-back">
                  <Icon name="chevronLeft" />{t.search.backToResults}
                </button>
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
          {:else}
            <!-- Songs first, a short block that never pages, so the groups below stay reachable. -->
            {#if songs.length > 0}
              <div class="browse-block" data-testid="search-songs">
                <div class="browse-block-head">
                  <h2 class="type-lg font-semibold">{t.search.songs}</h2>
                </div>
                <VirtualizedTrackList
                  tracks={songs.slice(0, SONGS_PREVIEW_COUNT)}
                  {currentVideoId}
                  {mockMode}
                  showHeader
                  ariaLabel={t.search.songs}
                  onPlayTrack={(track) => playResult(track)}
                  onTrackActions={(track) => onTrackActions(track)}
                  {like}
                  {nowPlaying}
                />
              </div>
            {/if}
            <TileGrid heading={t.search.artists} tiles={artistTiles} {mockMode} round />
            <TileGrid heading={t.search.albums} tiles={albumTiles} {mockMode} />
            <TileGrid heading={t.search.playlists} tiles={playlistTiles} {mockMode} />
            {#if hasAllSongs}
              <div class="browse-block">
                <button type="button" class="btn-ghost self-start" onclick={() => void openAllSongs()} data-testid="search-all-songs">
                  {t.search.allSongs}<Icon name="chevronRight" />
                </button>
              </div>
            {/if}
          {/if}
        </PageState>
      {/if}
    </div>
  </div>
</section>

<style>
  /* Earlier results waiting for the new query's: dimmed only once the wait lasts (600 ms, as
     the player's buffering cue), so a quick reply just replaces them. */
  .search-pane.is-active.is-replacing {
    opacity: 0.6;
    transition-delay: 600ms, 0s, 0s;
  }
</style>
