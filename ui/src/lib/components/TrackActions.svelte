<script lang="ts">
  import type { EnqueuePosition, PlaylistSummary, QueryError, Track } from "@ytmcp/protocol";
  import type { Gate } from "../capabilities.js";
  import type { PageView } from "../catalog/page-state.js";
  import { t } from "../i18n/index.svelte.js";
  import Cover from "./Cover.svelte";
  import { formatArtists, isPlayable } from "./format.js";
  import Icon, { type IconName } from "./Icon.svelte";
  import PageState from "./PageState.svelte";

  interface Props {
    track: Track;
    liked: boolean | null;
    mockMode?: boolean;
    enqueueGate: Gate;
    likeGate: Gate;
    playlistGate: Gate;
    canOpenAlbum: boolean;
    canOpenArtist: boolean;
    /** True while a write for this track is in flight. */
    pending: boolean;
    playlistsView: PageView;
    playlists: readonly PlaylistSummary[];
    playlistsHasMore?: boolean;
    playlistsLoadingMore?: boolean;
    playlistsMoreError?: QueryError | null;
    onPlay: () => void;
    onEnqueue: (position: EnqueuePosition) => void;
    onSetLike: (liked: boolean) => void;
    onRequestPlaylists: () => void;
    onLoadMorePlaylists?: () => void;
    onRetryPlaylists?: () => void;
    onAddToPlaylist: (playlist: PlaylistSummary) => void;
    onOpenAlbum: (albumId: string) => void;
    onOpenArtist: (artistId: string) => void;
  }

  let {
    track,
    liked,
    mockMode = false,
    enqueueGate,
    likeGate,
    playlistGate,
    canOpenAlbum,
    canOpenArtist,
    pending,
    playlistsView,
    playlists,
    playlistsHasMore = false,
    playlistsLoadingMore = false,
    playlistsMoreError = null,
    onPlay,
    onEnqueue,
    onSetLike,
    onRequestPlaylists,
    onLoadMorePlaylists,
    onRetryPlaylists,
    onAddToPlaylist,
    onOpenAlbum,
    onOpenArtist,
  }: Props = $props();

  let step = $state<"menu" | "pick" | "confirm">("menu");
  let chosen = $state<PlaylistSummary | null>(null);
  let playlistFilter = $state("");

  const albumId = $derived(track.album?.id ?? track.album?.browseId ?? null);
  const artistLinks = $derived((track.artists ?? []).filter((a) => a.id));
  const hasNavLinks = $derived((albumId && track.album && canOpenAlbum) || (canOpenArtist && artistLinks.length > 0));
  const filteredPlaylists = $derived(
    playlistFilter.trim()
      ? playlists.filter((pl) => pl.title.toLowerCase().includes(playlistFilter.trim().toLowerCase()))
      : playlists
  );
</script>

{#snippet action(
  icon: IconName,
  label: string,
  gate: { enabled: boolean; reason: string | null },
  onclick: () => void,
  testId: string,
  filled = false
)}
  <li>
    <button type="button" class="menu-item lg-press" disabled={!gate.enabled || pending} {onclick} data-testid={testId}>
      <Icon name={icon} {filled} />
      <span class="min-w-0 flex-1 flex flex-col gap-0.5">
        <span class="truncate">{label}</span>
        {#if gate.reason}
          <span class="type-xs" style="color: var(--text-muted);">{gate.reason}</span>
        {/if}
      </span>
    </button>
  </li>
{/snippet}

<div class="flex flex-col gap-3" data-testid="track-actions">
  <div class="flex items-center gap-3 pb-2 border-b" style="border-color: var(--border-subtle);">
    <Cover cover={track.cover} seed={track.videoId} label={track.title} {mockMode} class="h-11 w-11 rounded-lg" />
    <div class="min-w-0 flex-1">
      <div class="type-base font-semibold truncate">{track.title}</div>
      <div class="type-xs truncate" style="color: var(--text-secondary);">{formatArtists(track)}</div>
    </div>
  </div>

  {#if step === "menu"}
    <ul class="flex flex-col gap-1 m-0 p-0 list-none" aria-label={t.track.actions}>
      {@render action("play", t.track.play, { enabled: isPlayable(track), reason: null }, onPlay, "action-play")}
      {@render action("playNext", t.track.playNext, enqueueGate, () => onEnqueue("next"), "action-play-next")}
      {@render action("addToQueue", t.track.addToQueue, enqueueGate, () => onEnqueue("last"), "action-add-to-queue")}
      <li class="menu-divider" role="separator" aria-hidden="true"></li>
      {@render action(
        "heart",
        liked === true ? t.track.unlike : t.track.like,
        likeGate,
        () => onSetLike(liked !== true),
        "action-like",
        liked === true
      )}
      {@render action(
        "library",
        t.track.addToPlaylist,
        playlistGate,
        () => {
          playlistFilter = "";
          step = "pick";
          onRequestPlaylists();
        },
        "action-add-to-playlist"
      )}
      {#if hasNavLinks}
        <li class="menu-divider" role="separator" aria-hidden="true"></li>
      {/if}
      {#if albumId && track.album && canOpenAlbum}
        {@render action(
          "disc",
          t.track.goToAlbum.replace("{title}", track.album.title),
          { enabled: true, reason: null },
          () => onOpenAlbum(albumId),
          "action-go-album"
        )}
      {/if}
      {#if canOpenArtist}
        {#each artistLinks as artist (artist.id)}
          {@render action(
            "user",
            t.track.goToArtist.replace("{name}", artist.name),
            { enabled: true, reason: null },
            () => onOpenArtist(artist.id!),
            `action-go-artist-${artist.id}`
          )}
        {/each}
      {/if}
    </ul>
  {:else if step === "pick"}
    <div class="flex items-center justify-between gap-2">
      <h3 class="type-sm font-semibold m-0">{t.track.choosePlaylist}</h3>
      <button type="button" class="btn-ghost" onclick={() => (step = "menu")}>
        <Icon name="arrowLeft" />
        <span>{t.track.back}</span>
      </button>
    </div>
    {#if playlists.length > 6 || playlistsHasMore}
      <input
        type="search"
        value={playlistFilter}
        placeholder={t.track.filterPlaylists}
        aria-label={t.track.filterPlaylists}
        class="px-3 py-1.5 rounded-lg border type-sm"
        style="background-color: var(--bg-subtle); border-color: var(--border-strong); color: var(--text-primary);"
        oninput={(e) => (playlistFilter = e.currentTarget.value)}
      />
      {#if playlistsHasMore}
        <p class="type-xs m-0" style="color: var(--text-muted);" data-testid="playlist-picker-loaded-hint">
          {t.track.filterLoadedOnly}
        </p>
      {/if}
    {/if}
    <PageState view={playlistsView} name="playlist-picker" emptyText={t.track.noPlaylists} onRetry={onRetryPlaylists}>
      {#if filteredPlaylists.length === 0}
        <p class="type-sm m-0 py-3 text-center" style="color: var(--text-secondary);">{t.track.noMatchingPlaylists}</p>
      {:else}
        <ul class="flex flex-col gap-1 m-0 p-0 list-none max-h-72 overflow-y-auto">
          {#each filteredPlaylists as pl (pl.playlistId)}
            <li>
              <button
                type="button"
                class="menu-item lg-press justify-between"
                onclick={() => {
                  chosen = pl;
                  step = "confirm";
                }}
                data-testid={`pick-playlist-${pl.playlistId}`}
              >
                <span class="truncate">{pl.title}</span>
                {#if pl.trackCount != null}
                  <span class="type-xs tabular-nums shrink-0" style="color: var(--text-muted);">
                    {t.common.trackCount.replace("{count}", String(pl.trackCount))}
                  </span>
                {/if}
              </button>
            </li>
          {/each}
        </ul>
      {/if}
      {#if playlistsMoreError}
        <div
          class="flex items-center justify-between gap-2 px-3 py-2 rounded-lg type-xs"
          style="background-color: var(--bg-subtle); border: 1px solid var(--border-subtle); color: var(--status-danger);"
          data-testid="playlist-picker-more-error"
        >
          <span>[{playlistsMoreError.code}] {playlistsMoreError.safeMessage}</span>
          {#if playlistsHasMore && onLoadMorePlaylists}
            <button
              type="button"
              class="btn-secondary shrink-0"
              data-testid="playlist-picker-load-more"
              disabled={playlistsLoadingMore}
              onclick={onLoadMorePlaylists}
            >
              {playlistsLoadingMore ? t.common.loadingMore : t.states.retry}
            </button>
          {/if}
        </div>
      {:else if playlistsHasMore && onLoadMorePlaylists}
        <div class="pt-1">
          <button
            type="button"
            class="btn-secondary w-full"
            data-testid="playlist-picker-load-more"
            disabled={playlistsLoadingMore}
            onclick={onLoadMorePlaylists}
          >
            {playlistsLoadingMore ? t.common.loadingMore : t.common.loadMore}
          </button>
        </div>
      {/if}
    </PageState>
  {:else if chosen}
    <h3 class="type-sm font-semibold m-0">{t.track.confirmAddTitle}</h3>
    <p class="type-sm m-0">
      {t.track.confirmAdd.replace("{track}", track.title).replace("{playlist}", chosen.title)}
    </p>
    <div class="flex justify-end gap-2">
      <button type="button" class="btn-secondary" onclick={() => (step = "pick")}>{t.common.cancel}</button>
      <button
        type="button"
        class="btn-primary"
        disabled={pending || !playlistGate.enabled}
        onclick={() => onAddToPlaylist(chosen!)}
        data-testid="confirm-add-to-playlist"
      >
        {t.common.confirm}
      </button>
    </div>
  {/if}
</div>
