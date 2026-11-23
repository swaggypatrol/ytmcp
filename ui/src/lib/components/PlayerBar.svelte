<script lang="ts">
  import type { AppSnapshot, CommandReceipt, PlayerCommandPayload, PlayerOp, RepeatMode } from "@ytmcp/protocol";
  import { commandGate, playPauseGate } from "../capabilities.js";
  import { glass } from "../glass/liquid-glass.js";
  import { t } from "../i18n/index.svelte.js";
  import type { ClientPendingOverlay } from "../ipc/client.js";
  import {
    commitSeekPreview,
    isTerminalCommandStatus,
    reconcileSeekPreview,
    SEEK_PREVIEW_TIMEOUT_MS,
    startSeekPreview,
    type SeekPreview,
  } from "../seek-preview.js";
  import Cover from "./Cover.svelte";
  import { detectCjkLangHint, formatArtists, formatDurationMs } from "./format.js";
  import Icon, { type IconName } from "./Icon.svelte";
  import ScrubBar from "./ScrubBar.svelte";

  interface Props {
    snapshot: AppSnapshot | null;
    pendingOverlay: ClientPendingOverlay;
    interpolatedPositionMs: number;
    onCommand: (op: PlayerOp, args?: PlayerCommandPayload["args"]) => Promise<CommandReceipt | null>;
    onOpenNowPlaying: () => void;
    onOpenLyrics: () => void;
    onOpenDiagnostics: () => void;
  }

  let {
    snapshot,
    pendingOverlay,
    interpolatedPositionMs,
    onCommand,
    onOpenNowPlaying,
    onOpenLyrics,
    onOpenDiagnostics,
  }: Props = $props();

  const NEXT_REPEAT: Record<RepeatMode, RepeatMode> = { off: "all", all: "one", one: "off" };

  let seekPreview = $state<SeekPreview | null>(null);
  let seekSequence = 0;

  const player = $derived(snapshot?.player ?? null);
  const connection = $derived(player?.connection ?? null);
  const caps = $derived(snapshot?.capabilities);
  const track = $derived(player?.currentTrack ?? null);
  const durationMs = $derived(player?.timeAnchor.durationMs ?? track?.durationMs ?? 0);
  const displayPositionMs = $derived(seekPreview?.positionMs ?? interpolatedPositionMs);
  const isPlaying = $derived(player?.playback === "playing");
  const activeOp = $derived(pendingOverlay.activeOp ?? snapshot?.pending.activeOp ?? null);
  const isPending = $derived(pendingOverlay.pendingCount > 0 || (snapshot?.pending.pendingCommandCount ?? 0) > 0);
  const VOLUME_DRAFT_TIMEOUT_MS = 2000;
  // Sliders answer under the hand, so their own commands never show as "waiting". A discrete
  // command that is still unconfirmed after a moment shows as a quiet pulse on the status chip.
  const isWaiting = $derived(isPending && activeOp !== "setVolume" && activeOp !== "seek" && activeOp !== "setMuted");

  const playGate = $derived(playPauseGate(caps?.playPause, connection, player?.playback, track !== null));
  const seekGate = $derived(commandGate(caps?.seek, connection));
  const nextGate = $derived(commandGate(caps?.next, connection));
  const prevGate = $derived(commandGate(caps?.previous, connection));
  const volumeGate = $derived(commandGate(caps?.volume, connection));
  const shuffleGate = $derived(commandGate(caps?.shuffle, connection));
  const repeatGate = $derived(commandGate(caps?.repeat, connection));

  // The value under the hand while dragging, kept until the player reports it (or gives up).
  let volumeDraft = $state<number | null>(null);
  let volumeDragging = $state(false);
  const volumePercent = $derived(Math.round((volumeDraft ?? player?.volume ?? 0) * 100));
  const volumeIcon = $derived<IconName>(
    player?.muted || volumePercent === 0
      ? "volumeMuted"
      : volumePercent < 34
        ? "volumeLow"
        : volumePercent < 67
          ? "volumeMedium"
          : "volume"
  );
  const seekMax = $derived(Math.max(durationMs, 1));
  const seekValue = $derived(Math.min(displayPositionMs, seekMax));
  const playbackLabel = $derived(t.player.playbackStates[player?.playback ?? "idle"]);
  const shuffleLabel = $derived(
    player?.shuffle == null ? t.player.shuffleUnknown : player.shuffle ? t.player.shuffleOn : t.player.shuffleOff
  );
  const repeatLabel = $derived(t.player.repeat[player?.repeat ?? "unknown"]);
  const trackTooltip = $derived(track ? `${track.title} — ${formatArtists(track)}` : t.nav["now-playing"]);

  $effect(() => {
    if (volumeDraft === null || volumeDragging) return;
    if (Math.abs((player?.volume ?? -1) - volumeDraft) < 0.005) {
      volumeDraft = null;
      return;
    }
    const timer = setTimeout(() => (volumeDraft = null), VOLUME_DRAFT_TIMEOUT_MS);
    return () => clearTimeout(timer);
  });

  function previewVolume(percent: number) {
    volumeDraft = percent / 100;
    void onCommand("setVolume", { volume: percent / 100 });
  }

  // Roll the preview back on track change, on any terminal receipt of its seek, or on timeout.
  $effect(() => {
    const next = reconcileSeekPreview(seekPreview, player?.trackInstanceId, snapshot?.pending.lastReceipt, Date.now());
    if (next !== seekPreview) seekPreview = next;
  });

  $effect(() => {
    const committedAt = seekPreview?.committedAtMs;
    if (committedAt == null) return;
    const timer = setTimeout(() => {
      seekPreview = reconcileSeekPreview(seekPreview, player?.trackInstanceId, snapshot?.pending.lastReceipt, Date.now());
    }, SEEK_PREVIEW_TIMEOUT_MS);
    return () => clearTimeout(timer);
  });

  async function commitSeek(targetMs: number) {
    const sequence = ++seekSequence;
    const trackInstanceId = player?.trackInstanceId ?? null;
    seekPreview = commitSeekPreview(startSeekPreview(targetMs, durationMs, trackInstanceId), Date.now());
    const receipt = await onCommand("seek", { positionMs: seekPreview.positionMs, trackInstanceId });
    if (sequence !== seekSequence || !seekPreview || seekPreview.trackInstanceId !== trackInstanceId) return;
    if (!receipt || isTerminalCommandStatus(receipt.status)) {
      seekPreview = null;
      return;
    }
    seekPreview = reconcileSeekPreview(
      commitSeekPreview(seekPreview, Date.now(), receipt.commandId),
      player?.trackInstanceId,
      snapshot?.pending.lastReceipt,
      Date.now()
    );
  }
</script>

<!-- The dock holds the glass bar. -->
<div class="app-player-dock">
  <footer
    class="app-player-bar lg lg--bar surface-rounded-xl"
    aria-label={t.player.controlsLabel}
    data-testid="player-bar"
    {@attach glass}
  >
    <div class="player-now min-w-0">
      <button
        type="button"
        class="lg-press flex items-center gap-3 min-w-0 max-w-full text-left cursor-pointer rounded-xl p-1 pr-3 -m-1"
        style="--lg-press-scale: 0.975;"
        onclick={onOpenNowPlaying}
        aria-label={t.nav["now-playing"]}
        title={trackTooltip}
      >
        <Cover
          cover={track?.cover}
          seed={track?.videoId ?? "idle"}
          label={track?.title ?? "YTM"}
          mockMode={snapshot?.mockMode ?? false}
          class="h-12 w-12 rounded-lg"
        />
        <div class="min-w-0">
          <div
            class="truncate type-sm font-semibold"
            lang={track ? detectCjkLangHint(track.title) : undefined}
            data-testid="player-track-title"
          >
            {track?.title ?? t.nowPlaying.noTrack}
          </div>
          <div class="truncate type-xs" style="color: var(--text-secondary);">
            {#if track}<span class="player-status-inline">{`${playbackLabel} · `}</span>{/if}{track ? formatArtists(track) : playbackLabel}
          </div>
        </div>
      </button>
    </div>

    <div class="min-w-0 flex flex-col items-center gap-1">
      <!-- Transport stays centred; status chips take what is left on its right. -->
      <div class="player-transport-row w-full">
        <div></div>
        <div class="flex items-center gap-1.5">
          <button
            type="button"
            class="btn-icon lg-press"
            class:btn-icon-active={player?.shuffle === true}
            disabled={!shuffleGate.enabled || !player}
            aria-pressed={player?.shuffle == null ? undefined : player.shuffle}
            aria-busy={activeOp === "setShuffle"}
            aria-label={shuffleLabel}
            title={shuffleGate.reason ?? shuffleLabel}
            onclick={() => void onCommand("setShuffle", { shuffle: !(player?.shuffle ?? false) })}
            data-testid="btn-shuffle"
          >
            <Icon name="shuffle" />
          </button>
          <button
            type="button"
            class="btn-icon lg-press"
            disabled={!prevGate.enabled}
            aria-label={t.player.previous}
            title={prevGate.reason ?? t.player.previous}
            aria-busy={activeOp === "previous"}
            onclick={() => void onCommand("previous")}
            data-testid="btn-prev"
          >
            <Icon name="previous" filled />
          </button>
          <button
            type="button"
            class="btn-icon btn-icon-lg btn-play-focal lg-press"
            disabled={!playGate.enabled}
            aria-label={isPlaying ? t.player.pause : t.player.play}
            title={playGate.reason ?? (isPlaying ? t.player.pause : t.player.play)}
            aria-busy={activeOp === "toggle" || activeOp === "play" || activeOp === "pause"}
            onclick={() => void onCommand("toggle")}
            data-testid="btn-play-pause"
          >
            <Icon name={isPlaying ? "pause" : "play"} filled />
          </button>
          <button
            type="button"
            class="btn-icon lg-press"
            disabled={!nextGate.enabled}
            aria-label={t.player.next}
            title={nextGate.reason ?? t.player.next}
            aria-busy={activeOp === "next"}
            onclick={() => void onCommand("next")}
            data-testid="btn-next"
          >
            <Icon name="next" filled />
          </button>
          <button
            type="button"
            class="btn-icon lg-press"
            class:btn-icon-active={player?.repeat != null && player.repeat !== "off"}
            disabled={!repeatGate.enabled || !player}
            aria-label={repeatLabel}
            title={repeatGate.reason ?? repeatLabel}
            aria-busy={activeOp === "setRepeat"}
            onclick={() => void onCommand("setRepeat", { repeat: NEXT_REPEAT[player?.repeat ?? "off"] })}
            data-testid="btn-repeat"
          >
            <Icon name={player?.repeat === "one" ? "repeatOne" : "repeat"} />
          </button>
        </div>
        <div class="flex items-center gap-1.5 min-w-0 overflow-hidden whitespace-nowrap">
          <span
            class="player-status chip tabular-nums"
            class:is-waiting={isWaiting}
            aria-busy={isWaiting}
            data-testid="player-observed-status"
          >{playbackLabel}</span>
        </div>
      </div>

      <div class="w-full max-w-xl flex items-center gap-2.5">
        <span
          class="player-pos w-11 text-right type-xs tabular-nums"
          class:is-previewing={seekPreview !== null}
          data-testid="player-pos-text"
        >
          {formatDurationMs(displayPositionMs)}
        </span>
        <ScrubBar
          class="flex-1"
          value={seekValue}
          max={seekMax}
          buffered={player?.bufferedMs ?? null}
          disabled={!seekGate.enabled || !track}
          label={t.player.seekLabel}
          valueText={`${formatDurationMs(displayPositionMs)} / ${formatDurationMs(durationMs)}`}
          title={seekGate.reason ?? undefined}
          oninput={(v) => {
            seekSequence += 1;
            seekPreview = startSeekPreview(v, durationMs, player?.trackInstanceId);
          }}
          onchange={(v) => void commitSeek(v)}
          testid="player-seek-slider"
        />
        <span class="w-11 type-xs tabular-nums" style="color: var(--text-secondary);">{formatDurationMs(durationMs)}</span>
      </div>
    </div>

    <div class="player-tail flex items-center gap-2">
      <button
        type="button"
        class="btn-secondary btn-mute lg-press type-xs tabular-nums"
        onclick={() => void onCommand("setMuted", { muted: !(player?.muted ?? false) })}
        aria-label={player?.muted ? t.player.unmute : t.player.mute}
        title={volumeGate.reason ?? (player?.muted ? t.player.unmute : t.player.mute)}
        disabled={!volumeGate.enabled}
        data-testid="btn-mute"
      >
        <Icon name={volumeIcon} />
        <span>{player?.muted ? t.player.muted : `${volumePercent}%`}</span>
      </button>
      <ScrubBar
        class="player-volume"
        value={volumePercent}
        max={100}
        disabled={!volumeGate.enabled}
        label={t.player.volumeLabel}
        valueText={`${volumePercent}%`}
        title={volumeGate.reason ?? undefined}
        onpress={() => (volumeDragging = true)}
        oninput={previewVolume}
        onchange={() => (volumeDragging = false)}
        onrelease={() => (volumeDragging = false)}
        testid="player-volume-slider"
      />
      <button
        type="button"
        class="btn-icon lg-press"
        onclick={onOpenLyrics}
        aria-label={t.nav.lyrics}
        title={t.nowPlaying.openLyrics}
        data-testid="btn-open-lyrics"
      >
        <Icon name="lyrics" />
      </button>
      <button
        type="button"
        class="player-diagnostics btn-secondary lg-press type-xs"
        onclick={onOpenDiagnostics}
        data-testid="btn-open-diagnostics"
      >
        {t.player.diagnosticsButton}
      </button>
    </div>
  </footer>
</div>
