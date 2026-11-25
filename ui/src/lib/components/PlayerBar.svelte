<script lang="ts">
  import type { AppSnapshot, CommandReceipt, PlayerCommandPayload, PlayerOp, RepeatMode } from "@ytmcp/protocol";
  import { commandGate, playPauseGate, type Gate } from "../capabilities.js";
  import { CommandIntent } from "../command-intent.svelte.js";
  import type { Artwork } from "../cover.js";
  import { glass } from "../glass/liquid-glass.js";
  import { t } from "../i18n/index.svelte.js";
  import type { ClientPendingOverlay } from "../ipc/client.js";
  import { SeekControl } from "../seek-control.svelte.js";
  import { SEEK_KEY_STEP_MS, SEEK_PREVIEW_TIMEOUT_MS } from "../seek-preview.js";
  import { VolumeWheel } from "../volume-wheel.js";
  import { onDestroy, untrack } from "svelte";
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
    /** The cover to show: App's held artwork, which keeps the previous one while a new cover is cached. Defaults to the track's own. */
    artwork?: Artwork | null;
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
    artwork,
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

  // What this bar's own presses asked for, shown at once and marked pending until the command
  // settles (CommandIntent). A press steps from what is shown, so quick presses build on each other.
  const lastReceipt = $derived(snapshot?.pending.lastReceipt ?? null);
  const ownIdle = $derived(pendingOverlay.pendingCount === 0);
  const intentFacts = () => ({ lastReceipt, ownIdle });
  const playIntent = new CommandIntent<boolean>(intentFacts);
  const shuffleIntent = new CommandIntent<boolean>(intentFacts);
  const repeatIntent = new CommandIntent<RepeatMode>(intentFacts);
  // A play or pause sent elsewhere (Space, media keys, another client) also shows as pending.
  const otherDesired = $derived(pendingOverlay.desiredPlayback ?? snapshot?.pending.desiredPlayback ?? null);
  const shownPlaying = $derived(playIntent.value ?? (otherDesired !== null ? otherDesired === "playing" : isPlaying));
  const playPending = $derived(playIntent.value !== null || otherDesired !== null);
  const shownShuffle = $derived(shuffleIntent.value ?? player?.shuffle ?? null);
  const shownRepeat = $derived(repeatIntent.value ?? player?.repeat ?? null);

  /** Asks for the opposite of what the button shows, as an explicit play or pause; App's Space shortcut calls it too. */
  export function pressPlay() {
    const play = !shownPlaying;
    void playIntent.request(play, () => onCommand(play ? "play" : "pause"));
  }

  function pressShuffle() {
    const shuffle = !(shownShuffle ?? false);
    void shuffleIntent.request(shuffle, () => onCommand("setShuffle", { shuffle }));
  }

  function pressRepeat() {
    const repeat = NEXT_REPEAT[shownRepeat ?? "off"];
    void repeatIntent.request(repeat, () => onCommand("setRepeat", { repeat }));
  }
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

  const playGate = $derived(playPauseGate(caps?.playPause, connection, player?.playback, track !== null, player?.preparing === true));
  const seekGate = $derived(commandGate(caps?.seek, connection));
  const nextGate = $derived(commandGate(caps?.next, connection));
  const prevGate = $derived(commandGate(caps?.previous, connection));
  const volumeGate = $derived(commandGate(caps?.volume, connection));
  const shuffleGate = $derived(commandGate(caps?.shuffle, connection));
  const repeatGate = $derived(commandGate(caps?.repeat, connection));

  // The volume under the hand (drag, wheel or keys), kept until the player reports it (or fails).
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
    shownShuffle == null ? t.player.shuffleUnknown : shownShuffle ? t.player.shuffleOn : t.player.shuffleOff
  );
  const repeatLabel = $derived(t.player.repeat[shownRepeat ?? "unknown"]);
  const muteLabel = $derived(player?.muted ? t.player.unmute : t.player.mute);
  const volumeText = $derived(`${volumePercent}%`);
  const showLike = $derived(track !== null && likeGate.enabled && onToggleLike !== undefined);
  const trackTooltip = $derived(track ? `${track.title} — ${formatArtists(track)}` : t.nav["now-playing"]);
  const coverArt = $derived(
    artwork !== undefined ? artwork : track ? { cover: track.cover, seed: track.videoId, label: track.title } : null
  );

  /** Volume sends are at least this far apart. */
  const VOLUME_THROTTLE_MS = 60;
  /** A new target waits for the previous volume command to settle, but never longer than this. */
  const VOLUME_SETTLE_WAIT_MS = 250;
  let lastVolumeSentAt = Number.NEGATIVE_INFINITY;
  // The newest target not sent yet. Held while a volume command is unsettled, so the core
  // never queues (and cancels) a stale one; it is always sent in the end.
  let heldVolume = $state<number | null>(null);
  let volumeSendTimer: ReturnType<typeof setTimeout> | null = null;
  // Counts sends, so only the newest send's failure restores the bar.
  let volumeSendSeq = 0;
  let lastVolumeCommandId: string | null = null;
  // A volume command (from this window or elsewhere) the player has not settled yet.
  const volumeUnsettled = $derived(
    pendingOverlay.desiredVolume != null || snapshot?.pending.desiredVolume != null
  );
  // A command that will not take effect as sent: it failed, its outcome is unknown, or the core
  // cancelled it (another client's command replaced it), so the player's own value stands.
  const failedReceipt = (receipt: CommandReceipt | null | undefined) =>
    receipt?.status === "failed" || receipt?.status === "indeterminate" || receipt?.status === "cancelled";

  /** The bar goes back to the player's own volume, unless a newer target is still on its way. */
  function restoreVolume() {
    if (heldVolume === null && !volumeDragging) volumeDraft = null;
  }

  /** Sends the held target once the throttle allows and the previous volume command has settled. */
  function pumpVolume() {
    if (volumeSendTimer) clearTimeout(volumeSendTimer);
    volumeSendTimer = null;
    const target = heldVolume;
    if (target === null) return;
    const since = Date.now() - lastVolumeSentAt;
    const wait = Math.max(VOLUME_THROTTLE_MS - since, volumeUnsettled ? VOLUME_SETTLE_WAIT_MS - since : 0);
    if (wait > 0) {
      volumeSendTimer = setTimeout(pumpVolume, wait);
      return;
    }
    heldVolume = null;
    lastVolumeSentAt = Date.now();
    const seq = ++volumeSendSeq;
    lastVolumeCommandId = null;
    void onCommand("setVolume", { volume: target }).then((receipt) => {
      if (seq !== volumeSendSeq) return;
      lastVolumeCommandId = receipt?.commandId ?? null;
      const settled = snapshot?.pending.lastReceipt;
      if (!receipt || failedReceipt(receipt) || (settled?.commandId === receipt.commandId && failedReceipt(settled))) {
        restoreVolume();
      }
    });
  }

  // A settled volume command lets the held target go.
  $effect(() => {
    if (!volumeUnsettled) untrack(pumpVolume);
  });

  // The draft stays until the player reports it with nothing of ours still on its way, so a
  // confirmation of an older target never pulls the bar back.
  $effect(() => {
    if (volumeDraft === null || volumeDragging) return;
    if (heldVolume === null && !volumeUnsettled && Math.abs((player?.volume ?? -1) - volumeDraft) < 0.005) {
      volumeDraft = null;
    }
  });

  // The backstop: a draft left alone this long goes. It depends on the draft only, so player
  // updates (every second while playing) never postpone it.
  $effect(() => {
    if (volumeDraft === null || volumeDragging) return;
    const timer = setTimeout(() => (volumeDraft = null), VOLUME_DRAFT_TIMEOUT_MS);
    return () => clearTimeout(timer);
  });

  // A failed, unknown or cancelled volume command restores the true value at once (App reports
  // failures); restoreVolume keeps the draft while a newer target is still held.
  $effect(() => {
    const receipt = snapshot?.pending.lastReceipt;
    if (!failedReceipt(receipt)) return;
    untrack(() => {
      if (receipt!.op === "setVolume" && receipt!.commandId === lastVolumeCommandId) restoreVolume();
      if (receipt!.op === "setMuted") unmuteSent = false;
    });
  });

  // Set while the unmute sent by a volume raise is on its way, so a run of notches sends one.
  let unmuteSent = false;
  $effect(() => {
    if (!player?.muted) unmuteSent = false;
  });

  /** Raising the volume while muted (even when already at 100) unmutes; the page's setVolume leaves mute alone. */
  function unmuteForRaise() {
    if (unmuteSent) return;
    unmuteSent = true;
    void onCommand("setMuted", { muted: false }).then((receipt) => {
      if (!receipt || failedReceipt(receipt)) unmuteSent = false;
    });
  }

  /** Shows a new volume at once and sends it through the throttle; dragging, wheel and keys share it. */
  function previewVolume(percent: number) {
    const target = percent / 100;
    const from = volumeDraft ?? player?.volume ?? 0;
    volumeDraft = target;
    heldVolume = target;
    pumpVolume();
    if (player?.muted && target > from) unmuteForRaise();
  }

  function commitVolume() {
    volumeDragging = false;
    pumpVolume();
  }

  const wheelSteps = new VolumeWheel();
  // The main window's zoom, which the host sets from the interface scale (window-manager.ts).
  const pageZoom = $derived(Math.min(1.5, Math.max(0.75, (snapshot?.settings.uiScalePercent ?? 100) / 100)));

  /**
   * Wheel over the volume control: up raises, down lowers, five points a notch (VolumeWheel),
   * through the same draft and send path as dragging. Ctrl+wheel (zoom), mostly horizontal
   * gestures and a disabled control are left to the page; at 0 or 100 the wheel is still taken.
   */
  function handleVolumeWheel(event: WheelEvent) {
    if (!volumeGate.enabled) return;
    const from = Math.round((volumeDraft ?? player?.volume ?? 0) * 100);
    const to = wheelSteps.next(event, from, pageZoom);
    if (to === null) return;
    event.preventDefault();
    if (to !== from) previewVolume(to);
    else if (to === 100 && event.deltaY < 0 && player?.muted) unmuteForRaise();
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
    else if (direction > 0 && player?.muted) unmuteForRaise();
    volumeAnnouncement = `${t.player.volumeLabel} ${to}%`;
  }

  /** Registered by hand: a non-passive listener, so preventDefault keeps the page from scrolling. */
  function volumeWheel(node: HTMLElement) {
    node.addEventListener("wheel", handleVolumeWheel, { passive: false });
    return () => node.removeEventListener("wheel", handleVolumeWheel);
  }

  onDestroy(() => {
    if (volumeSendTimer) clearTimeout(volumeSendTimer);
    volumeSendTimer = null;
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
          cover={coverArt?.cover}
          seed={coverArt?.seed ?? "idle"}
          label={coverArt?.label ?? "YTM"}
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
          data-pending={likePending || undefined}
          aria-label={liked === true ? t.track.unlike : t.track.like}
          title={liked === true ? t.track.unlike : t.track.like}
          onclick={() => onToggleLike?.()}
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
          class:btn-icon-active={shownShuffle === true}
          disabled={!shuffleGate.enabled || !player}
          aria-pressed={shownShuffle == null ? undefined : shownShuffle}
          aria-busy={activeOp === "setShuffle" || shuffleIntent.value !== null}
          data-pending={shuffleIntent.value !== null || undefined}
          aria-label={shuffleLabel}
          title={shuffleGate.reason ?? shuffleLabel}
          onclick={pressShuffle}
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
          data-pending={activeOp === "previous" || undefined}
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
            aria-label={shownPlaying ? t.player.pause : t.player.play}
            title={playGate.reason ?? (shownPlaying ? t.player.pause : t.player.play)}
            aria-busy={showBuffering || playPending || activeOp === "toggle" || activeOp === "play" || activeOp === "pause"}
            data-pending={playPending || undefined}
            onclick={pressPlay}
            data-testid="btn-play-pause"
          >
            <Icon name={shownPlaying ? "pause" : "play"} filled />
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
          data-pending={activeOp === "next" || undefined}
          onclick={() => void onCommand("next")}
          data-testid="btn-next"
        >
          <Icon name="next" filled />
        </button>
        <button
          type="button"
          class="btn-icon lg-press"
          class:btn-icon-active={shownRepeat != null && shownRepeat !== "off"}
          disabled={!repeatGate.enabled || !player}
          aria-label={repeatLabel}
          title={repeatGate.reason ?? repeatLabel}
          aria-busy={activeOp === "setRepeat" || repeatIntent.value !== null}
          data-pending={repeatIntent.value !== null || undefined}
          onclick={pressRepeat}
          data-testid="btn-repeat"
        >
          <Icon name={shownRepeat === "one" ? "repeatOne" : "repeat"} />
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
