<script lang="ts">
  import type { AppSnapshot, CommandReceipt, PlayerCommandPayload, PlayerOp, RepeatMode } from "@ytmcp/protocol";
  import { commandGate, playPauseGate, type Gate } from "../capabilities.js";
  import { glass } from "../glass/liquid-glass.js";
  import { t } from "../i18n/index.svelte.js";
  import type { ClientPendingOverlay } from "../ipc/client.js";
  import { SeekControl } from "../seek-control.svelte.js";
  import { SEEK_KEY_STEP_MS, SEEK_PREVIEW_TIMEOUT_MS } from "../seek-preview.js";
  import { onDestroy } from "svelte";
  import Cover from "./Cover.svelte";
  import { detectCjkLangHint, formatArtists, formatDurationMs } from "./format.js";
  import Icon, { type IconName } from "./Icon.svelte";
  import QualityBadge from "./QualityBadge.svelte";
  import ScrubBar from "./ScrubBar.svelte";

  interface Props {
    snapshot: AppSnapshot | null;
    pendingOverlay: ClientPendingOverlay;
    interpolatedPositionMs: number;
    onCommand: (op: PlayerOp, args?: PlayerCommandPayload["args"]) => Promise<CommandReceipt | null>;
    onOpenNowPlaying: () => void;
    /** Opens lyrics, or closes it again when `lyricsOpen`. */
    onToggleLyrics: () => void;
    lyricsOpen?: boolean;
    /** Opens the Queue page; the button is left out without it. */
    onOpenQueue?: () => void;
    queueOpen?: boolean;
    /** The playing track's like state; the like button shows only while `likeGate` allows a write. */
    liked?: boolean | null;
    likeGate?: Gate;
    likePending?: boolean;
    onToggleLike?: () => void;
    /** The seek preview; App shares one instance. */
    seek?: SeekControl;
  }

  let {
    snapshot,
    pendingOverlay,
    interpolatedPositionMs,
    onCommand,
    onOpenNowPlaying,
    onToggleLyrics,
    lyricsOpen = false,
    onOpenQueue,
    queueOpen = false,
    liked = null,
    likeGate = { enabled: false, reason: null },
    likePending = false,
    onToggleLike,
    seek = new SeekControl(),
  }: Props = $props();

  const NEXT_REPEAT: Record<RepeatMode, RepeatMode> = { off: "all", all: "one", one: "off" };

  const player = $derived(snapshot?.player ?? null);
  const connection = $derived(player?.connection ?? null);
  const caps = $derived(snapshot?.capabilities);
  const track = $derived(player?.currentTrack ?? null);
  const durationMs = $derived(player?.timeAnchor.durationMs ?? track?.durationMs ?? 0);
  const displayPositionMs = $derived(seek.preview?.positionMs ?? interpolatedPositionMs);
  // While buffering the play button keeps the last settled state (playing unless it was
  // paused or stopped), so a short stall never flips its icon and label.
  let settledPlaying = true;
  const isPlaying = $derived.by(() => {
    const playback = player?.playback;
    if (playback === "buffering") return settledPlaying;
    settledPlaying = playback === "playing";
    return settledPlaying;
  });
  const activeOp = $derived(pendingOverlay.activeOp ?? snapshot?.pending.activeOp ?? null);
  const isPending = $derived(pendingOverlay.pendingCount > 0 || (snapshot?.pending.pendingCommandCount ?? 0) > 0);
  const VOLUME_DRAFT_TIMEOUT_MS = 2000;
  // An Up Next filter shows its wait on its own chip and never touches the current track, so it
  // stays quiet here unless another command is waiting with it.
  const filterOnly = (count: number, op: PlayerOp | null | undefined) => count === 0 || (count === 1 && op === "setQueueFilter");
  const onlyFilterPending = $derived(
    filterOnly(pendingOverlay.pendingCount, pendingOverlay.activeOp) &&
      filterOnly(snapshot?.pending.pendingCommandCount ?? 0, snapshot?.pending.activeOp)
  );
  // Sliders answer under the hand, so their own commands never show as "waiting". A discrete
  // command that is still unconfirmed after 600ms shows as a small breathing dot by the play button.
  const isWaiting = $derived(
    isPending && activeOp !== "setVolume" && activeOp !== "seek" && activeOp !== "setMuted" && !onlyFilterPending
  );
  let showWaitingDot = $state(false);
  $effect(() => {
    if (!isWaiting) {
      showWaitingDot = false;
      return;
    }
    const timer = setTimeout(() => {
      showWaitingDot = true;
    }, 600);
    return () => clearTimeout(timer);
  });

  /** Buffering shows only once it lasts 600 ms: a turning ring round the play button and a flowing bar. */
  const BUFFERING_DELAY_MS = 600;
  const isBuffering = $derived(player?.playback === "buffering");
  let showBuffering = $state(false);
  $effect(() => {
    if (!isBuffering) {
      showBuffering = false;
      return;
    }
    const timer = setTimeout(() => {
      showBuffering = true;
    }, BUFFERING_DELAY_MS);
    return () => clearTimeout(timer);
  });

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
  const shuffleLabel = $derived(
    player?.shuffle == null ? t.player.shuffleUnknown : player.shuffle ? t.player.shuffleOn : t.player.shuffleOff
  );
  const repeatLabel = $derived(t.player.repeat[player?.repeat ?? "unknown"]);
  const muteLabel = $derived(player?.muted ? t.player.unmute : t.player.mute);
  const volumeText = $derived(`${volumePercent}%`);
  const showLike = $derived(track !== null && likeGate.enabled && onToggleLike !== undefined);
  const trackTooltip = $derived(track ? `${track.title} — ${formatArtists(track)}` : t.nav["now-playing"]);

  const VOLUME_THROTTLE_MS = 60;
  let lastVolumeSentAt = 0;
  let pendingVolumeValue: number | null = null;
  let volumeThrottleTimer: ReturnType<typeof setTimeout> | null = null;

  function flushVolumeCommand(volume: number) {
    lastVolumeSentAt = Date.now();
    pendingVolumeValue = null;
    if (volumeThrottleTimer) {
      clearTimeout(volumeThrottleTimer);
      volumeThrottleTimer = null;
    }
    void onCommand("setVolume", { volume });
  }

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
    const target = percent / 100;
    volumeDraft = target;
    const now = Date.now();
    const elapsed = now - lastVolumeSentAt;
    if (elapsed >= VOLUME_THROTTLE_MS) {
      flushVolumeCommand(target);
      return;
    }
    pendingVolumeValue = target;
    if (!volumeThrottleTimer) {
      volumeThrottleTimer = setTimeout(() => {
        volumeThrottleTimer = null;
        if (pendingVolumeValue !== null) {
          flushVolumeCommand(pendingVolumeValue);
        }
      }, VOLUME_THROTTLE_MS - elapsed);
    }
  }

  function commitVolume() {
    volumeDragging = false;
    if (pendingVolumeValue !== null) {
      flushVolumeCommand(pendingVolumeValue);
    }
  }

  /** One mouse-wheel notch moves the volume by this many percent (a design value). */
  const WHEEL_NOTCH_PERCENT = 2;
  /** Pixels (deltaMode 0) and lines (deltaMode 1) that count as one notch; a page is one notch. */
  const WHEEL_NOTCH_PX = 100;
  const WHEEL_NOTCH_LINES = 3;
  // Sub-percent wheel movement not applied yet, so slow trackpad scrolls still add up.
  let wheelCarryPercent = 0;

  /**
   * Wheel over the volume control: up raises, down lowers, through the same draft and send
   * coalescing as dragging. Ctrl+wheel (zoom), mostly horizontal gestures and a disabled
   * control are left to the page.
   */
  function handleVolumeWheel(event: WheelEvent) {
    if (!volumeGate.enabled || event.ctrlKey || event.deltaY === 0) return;
    if (Math.abs(event.deltaX) >= Math.abs(event.deltaY)) return;
    event.preventDefault();
    const notches =
      event.deltaMode === 0
        ? event.deltaY / WHEEL_NOTCH_PX
        : event.deltaMode === 1
          ? event.deltaY / WHEEL_NOTCH_LINES
          : event.deltaY;
    const change = -notches * WHEEL_NOTCH_PERCENT;
    if (Math.sign(change) !== Math.sign(wheelCarryPercent)) wheelCarryPercent = 0;
    wheelCarryPercent += change;
    const steps = Math.trunc(wheelCarryPercent);
    if (steps === 0) return;
    wheelCarryPercent -= steps;
    const from = Math.round((volumeDraft ?? player?.volume ?? 0) * 100);
    const to = Math.min(100, Math.max(0, from + steps));
    if (to !== from) previewVolume(to);
  }

  /** One arrow press on the mute button moves the volume by this many percent. */
  const MUTE_KEY_STEP_PERCENT = 5;
  // Spoken after a keyboard step on the mute button, the one volume path left when the bar is cut.
  let volumeAnnouncement = $state("");

  /** ArrowUp/ArrowDown on the mute button step the volume, so the keyboard keeps it at any width. */
  function handleMuteKey(event: KeyboardEvent) {
    const direction = event.key === "ArrowUp" ? 1 : event.key === "ArrowDown" ? -1 : 0;
    if (direction === 0 || !volumeGate.enabled) return;
    event.preventDefault();
    const from = Math.round((volumeDraft ?? player?.volume ?? 0) * 100);
    const to = Math.min(100, Math.max(0, from + direction * MUTE_KEY_STEP_PERCENT));
    if (to !== from) previewVolume(to);
    volumeAnnouncement = `${t.player.volumeLabel} ${to}%`;
  }

  /** Registered by hand: a non-passive listener, so preventDefault keeps the page from scrolling. */
  function volumeWheel(node: HTMLElement) {
    node.addEventListener("wheel", handleVolumeWheel, { passive: false });
    return () => node.removeEventListener("wheel", handleVolumeWheel);
  }

  onDestroy(() => {
    if (volumeThrottleTimer) clearTimeout(volumeThrottleTimer);
    volumeThrottleTimer = null;
  });

  // Roll the preview back on track change, on any terminal receipt of its seek, or on timeout.
  $effect(() => seek.reconcile(player?.trackInstanceId, snapshot?.pending.lastReceipt));

  $effect(() => {
    if (seek.preview?.committedAtMs == null) return;
    const timer = setTimeout(
      () => seek.reconcile(player?.trackInstanceId, snapshot?.pending.lastReceipt),
      SEEK_PREVIEW_TIMEOUT_MS
    );
    return () => clearTimeout(timer);
  });
</script>

<!-- The dock holds the glass bar: track | transport and progress | quality, lyrics, queue, volume. -->
<div class="app-player-dock">
  <footer
    class="app-player-bar lg lg--bar surface-rounded-xl"
    aria-label={t.player.controlsLabel}
    data-testid="player-bar"
    {@attach glass}
  >
    <div class="player-now">
      <button
        type="button"
        class="player-track lg-press"
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
          class="player-cover"
        />
        <span class="player-track-text">
          <span
            class="player-track-title"
            lang={track ? detectCjkLangHint(track.title) : undefined}
            data-testid="player-track-title"
          >
            {track?.title ?? t.nowPlaying.noTrack}
          </span>
          {#if track}
            <span class="player-track-artist">{formatArtists(track)}</span>
          {/if}
        </span>
      </button>
      {#if showLike}
        <button
          type="button"
          class="btn-icon lg-press player-like"
          class:btn-icon-active={liked === true}
          aria-pressed={liked === true}
          aria-busy={likePending}
          aria-label={liked === true ? t.track.unlike : t.track.like}
          title={liked === true ? t.track.unlike : t.track.like}
          onclick={() => {
            if (!likePending) onToggleLike?.();
          }}
          data-testid="player-like"
        >
          <Icon name="heart" filled={liked === true} />
        </button>
      {/if}
    </div>

    <div class="player-centre">
      <div class="player-transport" role="group" aria-busy={showWaitingDot} data-testid="player-transport">
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
        <span class="player-play" class:is-buffering={showBuffering}>
          <button
            type="button"
            class="btn-icon btn-play-focal lg-press"
            disabled={!playGate.enabled}
            aria-label={isPlaying ? t.player.pause : t.player.play}
            title={playGate.reason ?? (isPlaying ? t.player.pause : t.player.play)}
            aria-busy={showBuffering || activeOp === "toggle" || activeOp === "play" || activeOp === "pause"}
            onclick={() => void onCommand("toggle")}
            data-testid="btn-play-pause"
          >
            <Icon name={isPlaying ? "pause" : "play"} filled />
          </button>
          <!-- Buffering past 600 ms: a ring turns round the button. -->
          <svg class="player-buffer-ring" viewBox="0 0 52 52" aria-hidden="true" data-testid="player-buffer-ring">
            <circle cx="26" cy="26" r="24.5" pathLength="100" />
          </svg>
          {#if showWaitingDot}
            <span class="wait-dot player-wait-dot" aria-hidden="true" data-testid="player-waiting-dot"></span>
          {/if}
        </span>
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

      <div class="player-progress">
        <span class="player-time player-pos" class:is-previewing={seek.preview !== null} data-testid="player-pos-text">
          {formatDurationMs(displayPositionMs)}
        </span>
        <ScrubBar
          class="player-seek"
          value={seekValue}
          max={seekMax}
          keyStep={SEEK_KEY_STEP_MS}
          buffered={player?.bufferedMs ?? null}
          busy={showBuffering}
          disabled={!seekGate.enabled || !track}
          label={t.player.seekLabel}
          valueText={`${formatDurationMs(displayPositionMs)} / ${formatDurationMs(durationMs)}`}
          title={seekGate.reason ?? t.player.seekLabel}
          oninput={(v) => seek.drag(v, durationMs, player?.trackInstanceId)}
          onchange={(v) =>
            void seek.commit(v, durationMs, player?.trackInstanceId, (positionMs, trackInstanceId) =>
              onCommand("seek", { positionMs, trackInstanceId })
            )}
          onrelease={() => seek.release()}
          testid="player-seek-slider"
        />
        <span class="player-time player-duration">{formatDurationMs(durationMs)}</span>
      </div>
    </div>

    <div class="player-tail">
      <QualityBadge quality={player?.audioQuality} codec={player?.codec} bitrateKbps={player?.audioBitrateKbps} class="player-quality" />
      <button
        type="button"
        class="btn-icon lg-press player-lyrics"
        class:btn-icon-active={lyricsOpen}
        onclick={onToggleLyrics}
        aria-label={t.nav.lyrics}
        aria-pressed={lyricsOpen}
        title={lyricsOpen ? t.nowPlaying.closeLyrics : t.nowPlaying.openLyrics}
        data-testid="btn-open-lyrics"
      >
        <Icon name="lyrics" />
      </button>
      {#if onOpenQueue}
        <button
          type="button"
          class="btn-icon lg-press"
          class:btn-icon-active={queueOpen}
          onclick={onOpenQueue}
          aria-label={t.player.queue}
          aria-current={queueOpen ? "page" : undefined}
          title={t.player.queue}
          data-testid="btn-open-queue"
        >
          <Icon name="queue" />
        </button>
      {/if}
      <div class="player-volume-area" data-testid="player-volume-area" {@attach volumeWheel}>
        <button
          type="button"
          class="btn-icon lg-press"
          onclick={() => void onCommand("setMuted", { muted: !(player?.muted ?? false) })}
          aria-label={muteLabel}
          aria-keyshortcuts="ArrowUp ArrowDown"
          title={volumeGate.reason ?? muteLabel}
          disabled={!volumeGate.enabled}
          onkeydown={handleMuteKey}
          data-testid="btn-mute"
        >
          <Icon name={volumeIcon} />
        </button>
        <span class="sr-only" role="status" data-testid="player-volume-status">{volumeAnnouncement}</span>
        <ScrubBar
          class="player-volume"
          value={volumePercent}
          max={100}
          disabled={!volumeGate.enabled}
          label={t.player.volumeLabel}
          valueText={volumeText}
          title={volumeGate.reason ?? t.player.volumeLabel}
          onpress={() => (volumeDragging = true)}
          oninput={previewVolume}
          onchange={commitVolume}
          onrelease={commitVolume}
          testid="player-volume-slider"
        />
      </div>
    </div>
  </footer>
</div>
