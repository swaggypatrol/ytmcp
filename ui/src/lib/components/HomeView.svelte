<script lang="ts">
  import type { HomeFeed, Track } from "@ytmcp/protocol";
  import type { PageView } from "../catalog/page-state.js";
  import { t } from "../i18n/index.svelte.js";
  import CoverShelf from "./CoverShelf.svelte";
  import HeroWash from "./HeroWash.svelte";
  import PageState from "./PageState.svelte";
  import VirtualizedTrackList from "./VirtualizedTrackList.svelte";

  interface Props {
    view: PageView;
    homeFeed: HomeFeed | null;
    currentVideoId: string | null;
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
    mockMode,
    smoothScroll = true,
    onPlayTrack,
    onTrackActions,
    onOpenPlaylist,
    onOpenRecent,
    onRetry,
    onOpenOfficialPlayer,
  }: Props = $props();

  const recent = $derived(homeFeed?.recentTracks ?? []);
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

<section class="flex flex-col gap-6" aria-labelledby="home-heading" data-testid="view-home">
  <div class="page-hero flex flex-wrap items-center justify-between gap-3">
    <HeroWash art={heroArt} {mockMode} />
    <h1 id="home-heading" class="type-xl font-bold tracking-tight">{t.home.title}</h1>
    {#if homeFeed && !homeFeed.personalizedSupported}
      <!-- The core's notice text is English only; the chip uses the interface language. -->
      <span class="chip" title={homeFeed.fallbackNotice ?? undefined}>{t.home.personalizedUnsupported}</span>
    {/if}
  </div>

  <PageState {view} name="home" {onRetry} {onOpenOfficialPlayer}>
    <CoverShelf heading={t.home.pinnedPlaylists} {tiles} {mockMode} {smoothScroll} />

    {#if recent.length > 0}
      <div class="flex flex-col gap-3">
        <div class="flex items-center justify-between gap-3">
          <h2 class="type-lg font-semibold">{t.home.recentTracks}</h2>
          <button type="button" class="btn-ghost" onclick={onOpenRecent} data-testid="home-view-all-recent">
            {t.home.viewAllRecent}
          </button>
        </div>
        <VirtualizedTrackList
          tracks={recent}
          {currentVideoId}
          {mockMode}
          showHeader
          heightPx={Math.min(recent.length, 6) * 58 + 2}
          ariaLabel={t.home.recentTracks}
          onPlayTrack={(track) => onPlayTrack(track)}
          onTrackActions={(track) => onTrackActions(track)}
        />
      </div>
    {/if}
  </PageState>
</section>
