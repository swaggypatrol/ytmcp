<script lang="ts">
  import type { ArtistDetail, Track } from "@ytmcp/protocol";
  import type { PageView } from "../catalog/page-state.js";
  import { t } from "../i18n/index.svelte.js";
  import TileGrid from "./TileGrid.svelte";
  import TrackCollectionView from "./TrackCollectionView.svelte";
  import type { TrackRowLike, TrackRowPlayback } from "./VirtualizedTrackList.svelte";

  interface Props {
    artistId: string;
    artist: ArtistDetail | null;
    /** The artist's name as the page it was opened from named it, shown until its own page loads. */
    knownName?: string | null;
    view: PageView;
    tracks: readonly Track[];
    currentVideoId: string | null;
    mockMode: boolean;
    like?: TrackRowLike;
    nowPlaying?: TrackRowPlayback;
    onBack?: () => void;
    onRetry: () => void;
    onOpenOfficialPlayer: () => void;
    onOpenAlbum: (albumId: string) => void;
    onPlayTrack: (track: Track, index: number) => void;
    onTrackActions: (track: Track, index: number) => void;
  }

  let {
    artistId,
    knownName = null,
    artist,
    view,
    tracks,
    currentVideoId,
    mockMode,
    like,
    nowPlaying,
    onBack,
    onRetry,
    onOpenOfficialPlayer,
    onOpenAlbum,
    onPlayTrack,
    onTrackActions,
  }: Props = $props();

  const albumTiles = $derived(
    (artist?.albums ?? []).flatMap((album) => {
      const id = album.id ?? album.browseId;
      return id ? [{ key: id, title: album.title, testId: `artist-album-${id}`, onOpen: () => onOpenAlbum(id) }] : [];
    })
  );
</script>

<!-- No Play: an artist's top songs play one at a time (there is no artist playback context). -->
<TrackCollectionView
  name="artist"
  title={artist?.name ?? knownName ?? t.artist.title}
  cover={artist?.cover}
  coverSeed={artistId}
  roundCover
  listHeading={t.artist.topSongs}
  {view}
  {tracks}
  hasMore={false}
  loadingMore={false}
  moreError={null}
  {currentVideoId}
  {mockMode}
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
    {#if artist?.description}
      <p class="type-sm m-0 line-clamp-3" style="color: var(--text-secondary);">{artist.description}</p>
    {/if}
  {/snippet}
  {#snippet footer()}
    {#if view.kind === "ready"}
      <TileGrid heading={t.artist.albums} tiles={albumTiles} {mockMode} />
    {/if}
  {/snippet}
</TrackCollectionView>
