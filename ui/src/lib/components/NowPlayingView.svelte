<script lang="ts">
  import type { AppSnapshot } from "@ytm-glass/protocol";
  import type { Gate } from "../capabilities.js";
  import { t } from "../i18n/index.svelte.js";
  import type { TrackAccent } from "../theme/accent.js";
  import Cover from "./Cover.svelte";
  import { detectCjkLangHint, formatDurationMs } from "./format.js";
  import HeroWash from "./HeroWash.svelte";
  import Icon from "./Icon.svelte";

  interface Props {
    snapshot: AppSnapshot | null;
    interpolatedPositionMs: number;
    liked: boolean | null;
    likeGate: Gate;
    likePending: boolean;
    canSeek?: boolean;
    canOpenAlbum: boolean;
    canOpenArtist: boolean;
    accent: TrackAccent | null;
    /** Decorative ambient motion allowed (playing, visible, motion not reduced). */
    decorativeMotion: boolean;
    onSeekMs?: (positionMs: number) => void;
    onToggleLike: () => void;
    onOpenActions: () => void;
    onOpenAlbum: (albumId: string) => void;
    onOpenArtist: (artistId: string) => void;
    onOpenLyrics: () => void;
    onOpenQueue: () => void;
  }

  let {
    snapshot,
    interpolatedPositionMs,
    liked,
    likeGate,
    likePending,
    canSeek = false,
    canOpenAlbum,
    canOpenArtist,
    accent,
    decorativeMotion,
    onSeekMs,
    onToggleLike,
    onOpenActions,
    onOpenAlbum,
    onOpenArtist,
    onOpenLyrics,
    onOpenQueue,
  }: Props = $props();

  const player = $derived(snapshot?.player ?? null);
  const track = $derived(player?.currentTrack ?? null);
  const durationMs = $derived(player?.timeAnchor.durationMs ?? track?.durationMs ?? null);
  const seekMax = $derived(Math.max(durationMs ?? 0, 1));
  const clampedPos = $derived(Math.min(interpolatedPositionMs, seekMax));
  const progressPct = $derived(durationMs && durationMs > 0 ? Math.min(100, (clampedPos / durationMs) * 100) : 0);
  const albumId = $derived(track?.album?.id ?? track?.album?.browseId ?? null);
  const drm = $derived(player?.drmProtected == null ? t.common.unknown : player.drmProtected ? t.common.yes : t.common.no);
  const hasAnyMediaFact = $derived(
    player?.codec != null || player?.audioMode != null || player?.qualityLabel != null || player?.drmProtected != null
  );
  const heroArt = $derived(track ? [{ cover: track.cover, seed: track.videoId, label: track.title }] : []);
  const accentStyle = $derived(
    accent ? `--accent: ${accent.accent}; --accent-soft: ${accent.accentSoft}; --text-on-accent: ${accent.onAccent};` : ""
  );
</script>

<section
  class="flex flex-col gap-6"
  aria-labelledby="now-playing-heading"
  data-testid="view-now-playing"
  style={accentStyle}
>
  <div class="page-hero flex flex-wrap items-center justify-between gap-3">
    <HeroWash art={heroArt} mockMode={snapshot?.mockMode ?? false} />
    <h1 id="now-playing-heading" class="type-xl font-bold tracking-tight m-0">{t.nowPlaying.title}</h1>
    {#if track}
      <div class="flex flex-wrap items-center gap-2">
        <span class="chip chip-accent">
          {t.nowPlaying.observedStatus}{t.common.colon}{t.player.playbackStates[player?.playback ?? "idle"]}
        </span>
        {#if snapshot?.pending.pendingCommandCount}
          <span class="chip" style="color: var(--status-warning);">{t.nowPlaying.pendingStatus}</span>
        {/if}
      </div>
    {/if}
  </div>

  {#if !track}
    <div class="state-block" data-testid="now-playing-empty">
      <Icon name="disc" class="w-10 h-10" style="color: var(--text-muted);" />
      <div class="type-lg font-semibold">{t.nowPlaying.noTrack}</div>
      <p class="type-sm m-0" style="color: var(--text-secondary);">{t.nowPlaying.noTrackHint}</p>
    </div>
  {:else}
    <div class="card p-6 sm:p-7 flex flex-col md:flex-row items-start md:items-center gap-7">
      <div class="relative shrink-0">
        <div class="np-ambient" class:np-ambient-on={decorativeMotion} aria-hidden="true" data-testid="np-ambient"></div>
        <Cover
          cover={track.cover}
          seed={track.videoId}
          label={track.title}
          mockMode={snapshot?.mockMode ?? false}
          class="relative h-48 w-48 sm:h-56 sm:w-56 surface-rounded-lg shadow-2xl"
        />
      </div>

      <div class="min-w-0 flex-1 w-full flex flex-col gap-3.5">
        <div class="flex flex-col gap-1.5">
          <h2 class="type-xl font-bold break-words m-0" lang={detectCjkLangHint(track.title)} data-testid="now-playing-title">
            {track.title}
          </h2>

          <div class="flex flex-wrap items-center gap-x-2 gap-y-1 type-base" style="color: var(--text-secondary);">
            {#each track.artists ?? [] as artist, i (`${artist.id ?? artist.name}::${i}`)}
              {#if i > 0}<span aria-hidden="true">/</span>{/if}
              {#if artist.id && canOpenArtist}
                <button type="button" class="link" onclick={() => onOpenArtist(artist.id!)} data-testid="np-artist-link">
                  {artist.name}
                </button>
              {:else}
                <span>{artist.name}</span>
              {/if}
            {/each}
            {#if track.album}
              <span aria-hidden="true">·</span>
              {#if albumId && canOpenAlbum}
                <button type="button" class="link" onclick={() => onOpenAlbum(albumId)} data-testid="np-album-link">
                  {track.album.title}
                </button>
              {:else}
                <span>{track.album.title}</span>
              {/if}
            {/if}
          </div>
        </div>

        <div class="flex flex-col gap-1.5 max-w-xl">
          {#if onSeekMs}
            <input
              type="range"
              min="0"
              max={seekMax}
              value={clampedPos}
              disabled={!canSeek || !durationMs}
              aria-label={t.player.seekLabel}
              aria-valuetext={`${formatDurationMs(interpolatedPositionMs)} / ${formatDurationMs(durationMs)}`}
              class="range w-full"
              style="--range-fill: {progressPct}%;"
              onchange={(e) => onSeekMs(Number(e.currentTarget.value))}
              data-testid="np-seek-slider"
            />
          {/if}
          <div class="flex items-center justify-between type-xs tabular-nums" style="color: var(--text-secondary);">
            <span>{formatDurationMs(interpolatedPositionMs)}</span>
            <span>{formatDurationMs(durationMs)}</span>
          </div>
        </div>

        {#if hasAnyMediaFact}
          <div class="flex flex-wrap gap-2 type-xs" style="color: var(--text-secondary);">
            {#if player?.codec}<span class="chip">{t.nowPlaying.codecLabel}{t.common.colon}{player.codec}</span>{/if}
            {#if player?.audioMode}<span class="chip">{t.nowPlaying.audioModeLabel}{t.common.colon}{player.audioMode}</span>{/if}
            {#if player?.qualityLabel}<span class="chip">{t.nowPlaying.qualityLabel}{t.common.colon}{player.qualityLabel}</span>{/if}
            {#if player?.drmProtected != null}<span class="chip">{t.nowPlaying.drmLabel}{t.common.colon}{drm}</span>{/if}
          </div>
        {:else}
          <div class="type-xs" style="color: var(--text-muted);">
            {t.nowPlaying.mediaDetailsPending}
          </div>
        {/if}

        <div class="flex flex-wrap gap-2.5 pt-1">
          <button
            type="button"
            class="btn-secondary"
            class:btn-icon-active={liked === true}
            aria-pressed={liked === true}
            disabled={!likeGate.enabled || likePending}
            aria-busy={likePending}
            title={likeGate.reason ?? undefined}
            onclick={onToggleLike}
            data-testid="np-like"
          >
            <Icon name="heart" filled={liked === true} />
            <span>{liked === true ? t.track.unlike : t.track.like}</span>
          </button>
          <button type="button" class="btn-secondary" onclick={onOpenActions} data-testid="np-actions">
            <Icon name="more" />
            <span>{t.track.actions}</span>
          </button>
          <button type="button" class="btn-primary" onclick={onOpenLyrics}>
            <Icon name="lyrics" />
            <span>{t.nowPlaying.openLyrics}</span>
          </button>
          <button type="button" class="btn-secondary" onclick={onOpenQueue}>
            <Icon name="queue" />
            <span>{t.nowPlaying.openQueue}</span>
          </button>
        </div>
        {#if likeGate.reason}
          <p class="type-xs m-0" style="color: var(--text-muted);">{t.track.like}{t.common.colon}{likeGate.reason}</p>
        {/if}
      </div>
    </div>
  {/if}
</section>
