<script lang="ts">
  import type { HomeFeed, Track } from "@ytmcp/protocol";
  import type { PageView } from "../catalog/page-state.js";
  import { t } from "../i18n/index.svelte.js";
  import CoverShelf from "./CoverShelf.svelte";
  import HeroWash from "./HeroWash.svelte";
  import PageState from "./PageState.svelte";
  import VirtualizedTrackList, { type TrackRowLike, type TrackRowPlayback } from "./VirtualizedTrackList.svelte";

  interface Props {
    view: PageView;
    homeFeed: HomeFeed | null;
    currentVideoId: string | null;
    /** Like toggles for the recent rows; pass it only while likes can be written. */
    like?: TrackRowLike;
    /** The playing row's indicator. */
    nowPlaying?: TrackRowPlayback;
    mockMode: boolean;
    smoothScroll?: boolean;
    onPlayTrack: (track: Track) => void;
    onTrackActions: (track: Track) => void;
    onOpenPlaylist: (playlistId: string) => void;
    onOpenRecent: () => void;
    onRetry: () => void;
    onOpenOfficialPlayer: () => void;
  }

  let {
    view,
    homeFeed,
    currentVideoId,
    like,
    nowPlaying,
    mockMode,
    smoothScroll = true,
    onPlayTrack,
    onTrackActions,
    onOpenPlaylist,
    onOpenRecent,
    onRetry,
    onOpenOfficialPlayer,
  }: Props = $props();

  /** Home shows this many recent plays; "View all" opens the rest. */
  const RECENT_ON_HOME = 6;

  const recent = $derived((homeFeed?.recentTracks ?? []).slice(0, RECENT_ON_HOME));
  const heroArt = $derived(
    (homeFeed?.pinnedPlaylists ?? []).map((pl) => ({ cover: pl.cover, seed: pl.playlistId, label: pl.title }))
  );
  const tiles = $derived(
    (homeFeed?.pinnedPlaylists ?? []).map((pl) => ({
      key: pl.playlistId,
      title: pl.title,
      subtitle: pl.subtitle ?? (pl.trackCount != null ? t.common.trackCount.replace("{count}", String(pl.trackCount)) : null),
      cover: pl.cover,
      testId: `home-playlist-${pl.playlistId}`,
      onOpen: () => onOpenPlaylist(pl.playlistId),
    }))
  );
</script>

<!-- Home presents what exists: the library's playlists and the latest plays. -->
<section class="browse-page" aria-labelledby="home-heading" data-testid="view-home">
  <header class="browse-head page-hero">
    <HeroWash art={heroArt} {mockMode} />
    <h1 id="home-heading" class="browse-title">{t.home.title}</h1>
  </header>

  <PageState {view} name="home" {onRetry} {onOpenOfficialPlayer}>
    <CoverShelf heading={t.home.pinnedPlaylists} {tiles} {mockMode} {smoothScroll} />

    {#if recent.length > 0}
      <div class="browse-block">
        <div class="browse-block-head">
          <h2 class="type-lg font-semibold">{t.home.recentTracks}</h2>
          <button
            type="button"
            class="btn-ghost"
            aria-label={t.home.viewAllRecent}
            onclick={onOpenRecent}
            data-testid="home-view-all-recent"
          >
            {t.home.viewAll}
          </button>
        </div>
        <VirtualizedTrackList
          tracks={recent}
          {currentVideoId}
          {mockMode}
          showHeader
          ariaLabel={t.home.recentTracks}
          onPlayTrack={(track) => onPlayTrack(track)}
          onTrackActions={(track) => onTrackActions(track)}
          {like}
          {nowPlaying}
        />
      </div>
    {/if}
  </PageState>
</section>
