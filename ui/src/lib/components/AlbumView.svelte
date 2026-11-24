<script lang="ts">
  import type { AlbumDetail, Track } from "@ytmcp/protocol";
  import { albumPlaybackContext } from "../catalog/catalog.svelte.js";
  import type { PageView } from "../catalog/page-state.js";
  import { t } from "../i18n/index.svelte.js";
  import TrackCollectionView from "./TrackCollectionView.svelte";
  import type { TrackRowLike, TrackRowPlayback } from "./VirtualizedTrackList.svelte";

  interface Props {
    albumId: string;
    album: AlbumDetail | null;
    view: PageView;
    tracks: readonly Track[];
    currentVideoId: string | null;
    mockMode: boolean;
    like?: TrackRowLike;
    nowPlaying?: TrackRowPlayback;
    onBack?: () => void;
    onRetry: () => void;
    onOpenOfficialPlayer: () => void;
    onOpenArtist: (artistId: string) => void;
    onPlayTrack: (track: Track, index: number) => void;
    onTrackActions: (track: Track, index: number) => void;
  }

  let {
    albumId,
    album,
    view,
    tracks,
    currentVideoId,
    mockMode,
    like,
    nowPlaying,
    onBack,
    onRetry,
    onOpenOfficialPlayer,
    onOpenArtist,
    onPlayTrack,
    onTrackActions,
  }: Props = $props();

  // Play plays the album only when its tracks map onto the album's own playlist.
  const playAll = $derived(albumPlaybackContext(album, tracks, 0) !== null);
</script>

<TrackCollectionView
  name="album"
  title={album?.title ?? t.album.title}
  cover={album?.cover}
  coverSeed={albumId}
  {view}
  {tracks}
  hasMore={false}
  loadingMore={false}
  moreError={null}
  {currentVideoId}
  {mockMode}
  {playAll}
  {like}
  {nowPlaying}
  {onBack}
  {onRetry}
  {onOpenOfficialPlayer}
  onLoadMore={() => {}}
  {onPlayTrack}
  {onTrackActions}
>
  {#snippet header()}
    {#if album}
      <div class="flex flex-wrap items-center gap-x-2 gap-y-1 type-sm" style="color: var(--text-secondary);">
        {#each album.artists ?? [] as artist, i (`${artist.id ?? artist.name}::${i}`)}
          {#if i > 0}<span aria-hidden="true">/</span>{/if}
          {#if artist.id}
            <button type="button" class="link" onclick={() => onOpenArtist(artist.id!)} data-testid="album-artist-link">
              {artist.name}
            </button>
          {:else}
            <span>{artist.name}</span>
          {/if}
        {/each}
        {#if album.year}<span class="tabular-nums">· {album.year}</span>{/if}
      </div>
    {/if}
  {/snippet}
</TrackCollectionView>
