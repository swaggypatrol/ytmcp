import type {
  BridgeCommandAckPayload,
  BridgeEpoch,
  BridgeHealthPayload,
  BridgePlaybackObservedPayload,
  BridgeReadyPayload,
  CapabilitySet,
  CapabilitySupport,
  CommandId,
  Envelope,
  MessageKind,
  PlaybackStatus,
  PlayerCommandPayload,
  QueryErrorCode,
  QueueSnapshot,
  TrackInstanceId,
} from "@ytmcp/protocol";
import { DomObserver } from "./dom-observer.js";
import {
  PROTOCOL_VERSION,
  buildQueryError,
  isPlainObject,
  normalizePlaybackRate,
  normalizeVolume,
  sanitizeTrack,
  toUint64Ms,
  validateInboundBridgeEnvelope,
} from "./guard.js";
import { QueueMapper } from "./queue.js";
import {
  BRIDGE_VERSION,
  MUSIC_ORIGIN,
  PAGE_SHIM_CHANNEL,
  type BridgeHandle,
  type BridgeStateSnapshot,
  type BridgeTransport,
  type InstallBridgeOptions,
  type PageShimErrorCode,
  type PageShimOp,
  type PageShimResponse,
  type PageSnapshot,
} from "./types.js";

export { BRIDGE_VERSION, MUSIC_ORIGIN, PAGE_SHIM_CHANNEL } from "./types.js";
export type { BridgeHandle, BridgeTransport, InstallBridgeOptions } from "./types.js";

const MAX_COMMAND_ACK_CACHE = 256;
/** Match the core's command ID lifetime so an expired ID can be used again on this page. */
const COMMAND_ACK_TTL_MS = 10 * 60 * 1_000;
const DEFAULT_HEALTH_INTERVAL_MS = 12_000;
const DEFAULT_SAMPLE_INTERVAL_MS = 1_000;
const DEFAULT_MUTATION_DEBOUNCE_MS = 100;
const DEFAULT_SHIM_TIMEOUT_MS = 1_500;
/** Capability-only changes must hold this long before bridge.ready is re-sent. */
const DEFAULT_READY_DEBOUNCE_MS = 500;
/** enqueue waits for the page queue fetch; navigate waits for the target to load. */
const SLOW_SHIM_TIMEOUT_MS = 8_000;
/** Re-observe after a dispatched command so its effect is reported promptly. */
const FOLLOW_UP_DELAYS_MS = [300, 1_000];

/** Capabilities owned by Rust adapters, not by the page bridge. */
const OUTSIDE_BRIDGE = {
  search: "unverified",
  libraryPlaylists: "unverified",
  playlistTracks: "unverified",
  albumDetail: "unverified",
  artistDetail: "unverified",
  likedSongs: "unverified",
  personalizedHome: "unverified",
  likeMutation: "unverified",
  playlistAddMutation: "unverified",
  lyricsLrclib: "unverified",
  mpris: "unverified",
} as const satisfies Partial<CapabilitySet>;

export function isTrustedMusicTopFrame(win: Window | undefined): boolean {
  if (!win) return false;
  try {
    return win === win.top && win.location?.origin === MUSIC_ORIGIN;
  } catch {
    return false;
  }
}

/** Capabilities that the live page actually offers right now; nothing is assumed. */
export function capabilitiesFrom(page: PageSnapshot | null): CapabilitySet {
  const c = page?.hasPlayerApi ? page.controls : null;
  const s = (v: boolean | undefined): CapabilitySupport => (v ? "supported" : "unsupported");
  return {
    playPause: s(c?.playPause),
    seek: s(c?.seek),
    next: s(c?.next),
    previous: s(c?.previous),
    volume: s(c?.volume),
    shuffle: s(c?.shuffle),
    repeat: s(c?.repeat),
    queueMirror: s(page?.queue !== null && page?.queue !== undefined),
    queueEnqueue: s(c?.enqueue),
    queueReorder: s(c?.editQueue),
    queueRemove: s(c?.editQueue),
    queueAutoplay: s(c?.setAutoplay),
    queueFilter: s(c?.setQueueFilter),
    queueClear: s(c?.clearUpcoming),
    ...OUTSIDE_BRIDGE,
  };
}

function statusFromPlayerState(state: number | null, hasVideo: boolean): PlaybackStatus {
  switch (state) {
    case 1:
      return "playing";
    case 2:
    case 5:
      return "paused";
    case 3:
      return "buffering";
    case 0:
      return "ended";
    default:
      return hasVideo ? "paused" : "idle";
  }
}

function statusFromMedia(media: HTMLMediaElement | null): PlaybackStatus {
  if (!media) return "idle";
  if (media.ended) return "ended";
  if (!media.paused) return media.readyState > 0 && media.readyState < 3 ? "buffering" : "playing";
  return media.currentSrc !== "" || media.src !== "" ? "paused" : "idle";
}

function mapShimError(code: PageShimErrorCode | undefined): QueryErrorCode {
  if (code === "STALE") return "STALE_CONTEXT";
  if (code === "INVALID_ARGS") return "INVALID_PAYLOAD";
  return "UNSUPPORTED_CAPABILITY";
}

class BridgeController {
  private readonly epoch: BridgeEpoch;
  private readonly transport: BridgeTransport;
  private readonly win: Window;
  private readonly dom: DomObserver;
  private readonly queue: QueueMapper;
  private readonly healthIntervalMs: number;
  private readonly sampleIntervalMs: number;
  private readonly shimTimeoutMs: number;
  private readonly readyDebounceMs: number;

  private destroyed = false;
  private seq = 0;
  private page: PageSnapshot | null = null;
  private shimAnswered = false;
  private capabilities: CapabilitySet = capabilitiesFrom(null);
  /** The last bridge.ready actually sent; the core and UI see these capabilities. */
  private sentReady: BridgeReadyPayload | null = null;
  private readyTimer: ReturnType<typeof setTimeout> | null = null;
  /** Capabilities (as JSON) the running readyTimer is waiting on. */
  private pendingCapabilities: string | null = null;
  private health: BridgeHealthPayload | null = null;
  private playback: BridgePlaybackObservedPayload | null = null;
  private queueSnapshot: QueueSnapshot | null = null;

  private instanceCounter = 0;
  private instanceKey: string | null = null;
  private instanceId: TrackInstanceId | null = null;
  private lastStatus: PlaybackStatus = "idle";
  private lastSampleAt = Number.NEGATIVE_INFINITY;

  private refreshing = false;
  private pendingRefresh: { force: boolean; health: boolean } | null = null;

  private shimCounter = 0;
  private readonly pendingShim = new Map<string, { resolve: (r: PageShimResponse | null) => void; timer: ReturnType<typeof setTimeout> }>();
  /**
   * Dispatch results keyed by commandId plus the dispatched op and args. The core reuses one
   * commandId for the phases of a composite command (stop: pause then seek; skip while
   * paused: next/previous then pause), so a later phase must run while an identical
   * retransmit must not. `null` means the page was asked but did not answer.
   */
  private readonly acks = new Map<string, {
    result: Promise<BridgeCommandAckPayload | null>;
    expiresAt: number;
  }>();
  private readonly timers = new Set<ReturnType<typeof setTimeout>>();
  private readonly teardowns: Array<() => void> = [];

  public constructor(transport: BridgeTransport, epoch: BridgeEpoch, win: Window, doc: Document, options: InstallBridgeOptions) {
    this.transport = transport;
    this.epoch = epoch;
    this.win = win;
    this.healthIntervalMs = options.healthIntervalMs ?? DEFAULT_HEALTH_INTERVAL_MS;
    this.sampleIntervalMs = options.sampleIntervalMs ?? DEFAULT_SAMPLE_INTERVAL_MS;
    this.shimTimeoutMs = options.shimTimeoutMs ?? DEFAULT_SHIM_TIMEOUT_MS;
    this.readyDebounceMs = options.readyDebounceMs ?? DEFAULT_READY_DEBOUNCE_MS;
    this.queue = new QueueMapper(epoch);
    this.dom = new DomObserver(
      win,
      doc,
      {
        isActive: () => this.active(),
        onMediaEvent: (type) => this.onMediaEvent(type),
        onDomChanged: () => this.requestRefresh(false, false),
      },
      options.mutationDebounceMs ?? DEFAULT_MUTATION_DEBOUNCE_MS
    );
  }

  private active(): boolean {
    return !this.destroyed && isTrustedMusicTopFrame(this.win);
  }

  public start(): void {
    this.teardowns.push(this.transport.onEnvelope((env) => void this.onInbound(env)));

    const onMessage = (event: MessageEvent): void => {
      if (!this.active() || event.source !== this.win || event.origin !== MUSIC_ORIGIN) return;
      const data = event.data;
      if (!isPlainObject(data) || data.channel !== PAGE_SHIM_CHANNEL || data.direction !== "from-page") return;
      if (typeof data.requestId !== "string") return;
      const pending = this.pendingShim.get(data.requestId);
      if (!pending) return;
      clearTimeout(pending.timer);
      this.pendingShim.delete(data.requestId);
      pending.resolve(data as unknown as PageShimResponse);
    };
    this.win.addEventListener("message", onMessage);
    this.teardowns.push(() => this.win.removeEventListener("message", onMessage));

    const onNavigate = (): void => {
      if (!this.active()) return;
      this.dom.rebind();
      this.requestRefresh(true, false);
    };
    for (const evt of ["yt-navigate-finish", "popstate"] as const) {
      this.win.addEventListener(evt, onNavigate);
      this.teardowns.push(() => this.win.removeEventListener(evt, onNavigate));
    }

    this.dom.rebind();
    if (this.healthIntervalMs > 0) {
      const timer = setInterval(() => this.requestRefresh(false, true), this.healthIntervalMs);
      this.teardowns.push(() => clearInterval(timer));
    }
    this.requestRefresh(true, true);
  }

  public uninstall(): void {
    if (this.destroyed) return;
    this.destroyed = true;
    this.dom.dispose();
    for (const p of this.pendingShim.values()) {
      clearTimeout(p.timer);
      p.resolve(null);
    }
    this.pendingShim.clear();
    for (const t of this.timers) clearTimeout(t);
    this.timers.clear();
    this.cancelReadyTimer();
    while (this.teardowns.length > 0) {
      try {
        this.teardowns.pop()?.();
      } catch {
        // best-effort cleanup
      }
    }
  }

  public state(): BridgeStateSnapshot | null {
    if (this.destroyed) return null;
    return { capabilities: this.capabilities, playback: this.playback, queue: this.queueSnapshot, health: this.health };
  }

  private emit(kind: MessageKind, payload: unknown, requestId: string | null = null): void {
    if (!this.active()) return;
    this.seq += 1;
    const envelope: Envelope = {
      protocolVersion: PROTOCOL_VERSION,
      kind,
      bridgeEpoch: this.epoch,
      coreEpoch: null,
      accountEpoch: null,
      requestId,
      seq: this.seq,
      payload,
    };
    try {
      void Promise.resolve(this.transport.sendEnvelope(envelope)).catch(() => {});
    } catch {
      // The host rejects invalid frames; nothing to recover here.
    }
  }

  private onMediaEvent(type: string): void {
    if (type === "timeupdate") {
      // Media fires timeupdate ~4 Hz; position corrections at ~1 Hz are enough (SPEC 9.3).
      if (Date.now() - this.lastSampleAt < this.sampleIntervalMs) return;
    }
    this.requestRefresh(false, false);
  }

  /** Coalesces refreshes so at most one probe is in flight. */
  private requestRefresh(force: boolean, health: boolean): void {
    if (!this.active()) return;
    if (this.refreshing) {
      this.pendingRefresh = {
        force: force || (this.pendingRefresh?.force ?? false),
        health: health || (this.pendingRefresh?.health ?? false),
      };
      return;
    }
    this.refreshing = true;
    void this.refresh(force, health).finally(() => {
      this.refreshing = false;
      const next = this.pendingRefresh;
      this.pendingRefresh = null;
      if (next) this.requestRefresh(next.force, next.health);
    });
  }

  private async refresh(force: boolean, healthTick: boolean): Promise<void> {
    this.lastSampleAt = Date.now();
    const res = await this.callShim("probe");
    if (!this.active()) return;
    this.shimAnswered = Boolean(res?.ok && res.data);
    this.page = res?.ok && res.data ? res.data : null;
    this.capabilities = capabilitiesFrom(this.page);
    const { snapshot, changed } = this.queue.update(this.page?.queue ?? null);
    this.queueSnapshot = snapshot;
    this.updateReady(force);

    this.emitHealth(force || healthTick);

    if (force || changed) this.emit("bridge.queueObserved", snapshot);

    this.emitPlayback();
  }

  private currentReady(): BridgeReadyPayload {
    return {
      bridgeEpoch: this.epoch,
      bridgeVersion: BRIDGE_VERSION,
      capabilities: this.capabilities,
      loggedInObserved: this.page?.loggedIn ?? false,
      accountHint: this.page?.accountHint ?? null,
    };
  }

  /**
   * Re-sends bridge.ready when what it reports changes, so the core and the UI's disabled
   * states follow the live page. Account changes go out at once; capability-only changes
   * are debounced: a new capability set must hold for readyDebounceMs (each further change
   * restarts the wait), so a control that blinks out during a page transition sends nothing.
   */
  private updateReady(force: boolean): void {
    const ready = this.currentReady();
    const sent = this.sentReady;
    const accountChanged =
      sent === null || sent.loggedInObserved !== ready.loggedInObserved || sent.accountHint !== ready.accountHint;
    const capabilities = JSON.stringify(ready.capabilities);
    if (force || accountChanged) {
      this.sendReady(ready);
    } else if (capabilities === JSON.stringify(sent.capabilities)) {
      this.cancelReadyTimer();
    } else if (capabilities !== this.pendingCapabilities) {
      this.cancelReadyTimer();
      this.pendingCapabilities = capabilities;
      this.readyTimer = setTimeout(() => {
        this.readyTimer = null;
        this.pendingCapabilities = null;
        if (!this.active()) return;
        const latest = this.currentReady();
        if (JSON.stringify(latest) !== JSON.stringify(this.sentReady)) this.sendReady(latest);
      }, this.readyDebounceMs);
    }
  }

  private sendReady(ready: BridgeReadyPayload): void {
    this.cancelReadyTimer();
    this.sentReady = ready;
    this.emit("bridge.ready", ready);
  }

  private cancelReadyTimer(): void {
    if (this.readyTimer !== null) {
      clearTimeout(this.readyTimer);
      this.readyTimer = null;
    }
    this.pendingCapabilities = null;
  }

  private emitHealth(always: boolean): void {
    const media = this.dom.getMediaElement();
    const videoElementPresent = media !== null;
    const controlsBound = this.capabilities.playPause === "supported";
    const healthy = this.shimAnswered && videoElementPresent && controlsBound;
    const reason = healthy
      ? null
      : !this.shimAnswered
        ? "Page shim did not answer"
        : !videoElementPresent
          ? "HTMLMediaElement not present"
          : "YouTube player API unavailable";
    const next: BridgeHealthPayload = { bridgeEpoch: this.epoch, healthy, videoElementPresent, controlsBound, reason };
    const changed = JSON.stringify(next) !== JSON.stringify(this.health);
    this.health = next;
    if (always || changed) this.emit("bridge.health", next);
  }

  private emitPlayback(): void {
    const media = this.dom.getMediaElement();
    const api = this.page?.hasPlayerApi ? this.page : null;
    const mediaError = Boolean(media?.error);
    const status: PlaybackStatus = mediaError
      ? "error"
      : api
        ? statusFromPlayerState(api.playerState, api.videoId !== null)
        : statusFromMedia(media);

    // Per-track time from the player API. The media element spans one MSE timeline across
    // gapless tracks, so it is only a fallback when the player API is missing entirely.
    const positionMs = api ? api.positionMs ?? 0 : toUint64Ms((media?.currentTime ?? 0) * 1000) ?? 0;
    const durationMs = api
      ? api.durationMs
      : media && Number.isFinite(media.duration) && media.duration > 0
        ? toUint64Ms(media.duration * 1000)
        : null;

    // How far the track has loaded, per track from the player API, in whole seconds so that a
    // loading track does not send an observation for every chunk.
    const loaded = api?.loadedFraction ?? null;
    const bufferedMs = durationMs !== null && loaded !== null && loaded >= 0
      ? Math.floor((Math.min(1, loaded) * durationMs) / 1000) * 1000
      : null;

    const currentEntryId = this.queueSnapshot?.currentEntryId ?? null;
    const videoId = api?.videoId ?? null;
    const trackInstanceId = this.resolveInstance(videoId, api?.instanceKey ?? currentEntryId, status);
    const t = api?.track ?? null;
    const currentTrack = t
      ? sanitizeTrack({ ...t, durationMs: durationMs ?? t.durationMs, availability: t.available ? "available" : "unavailable" })
      : null;

    const payload: BridgePlaybackObservedPayload = {
      bridgeEpoch: this.epoch,
      playback: status,
      positionMs: durationMs !== null ? Math.min(positionMs, durationMs) : positionMs,
      durationMs,
      bufferedMs,
      playbackRate: normalizePlaybackRate(api?.playbackRate ?? media?.playbackRate),
      sampleAgeMs: 0,
      volume:
        api?.volume0To100 != null
          ? normalizeVolume(api.volume0To100, "percent")
          : normalizeVolume(media?.volume ?? 1, "unit"),
      muted: api?.muted ?? Boolean(media?.muted),
      shuffle: api?.shuffle ?? null,
      repeat: api?.repeat ?? null,
      autoplay: this.page?.autoplay ?? null,
      trackInstanceId,
      currentQueueEntryId: currentEntryId,
      currentTrack,
      playbackContext: api?.playbackContext ?? null,
      codec: api?.codec ?? null,
      audioMode: api?.audioMode ?? null,
      qualityLabel: api?.qualityLabel ?? null,
      audioItag: api?.audioItag ?? null,
      audioQuality: api?.audioQuality ?? null,
      audioBitrateKbps: api?.audioBitrateKbps ?? null,
      drmProtected: null,
    };
    this.playback = payload;
    this.emit("bridge.playbackObserved", payload);
  }

  /**
   * A new playback instance starts when the page loads a different video or a new load of
   * the same one (the player's per-load nonce changes), or when playback resumes after
   * `ended`. Seeking within a track, including YouTube Music's "previous restarts the
   * track", keeps the instance so the core can see the position jump back.
   */
  private resolveInstance(videoId: string | null, loadKey: string | null, status: PlaybackStatus): TrackInstanceId | null {
    if (!videoId) {
      this.instanceKey = null;
      this.instanceId = null;
      this.lastStatus = status;
      return null;
    }
    const key = `${videoId}|${loadKey ?? ""}`;
    const replay = this.lastStatus === "ended" && (status === "playing" || status === "buffering");
    if (key !== this.instanceKey || replay || this.instanceId === null) {
      this.instanceCounter += 1;
      this.instanceId = `ti:${this.epoch}:${this.instanceCounter}`;
      this.instanceKey = key;
    }
    this.lastStatus = status;
    return this.instanceId;
  }

  private callShim(op: PageShimOp, args?: Record<string, unknown>, timeoutMs = this.shimTimeoutMs): Promise<PageShimResponse | null> {
    if (!this.active()) return Promise.resolve(null);
    this.shimCounter += 1;
    const requestId = `shim:${this.epoch}:${this.shimCounter}`;
    return new Promise((resolve) => {
      const timer = setTimeout(() => {
        this.pendingShim.delete(requestId);
        resolve(null);
      }, timeoutMs);
      this.pendingShim.set(requestId, { resolve, timer });
      this.win.postMessage({ channel: PAGE_SHIM_CHANNEL, direction: "to-page", requestId, op, args }, MUSIC_ORIGIN);
    });
  }

  private async onInbound(raw: unknown): Promise<void> {
    if (!this.active()) return;
    const v = validateInboundBridgeEnvelope(raw, this.epoch);
    if (!v.ok) {
      this.emit("bridge.error", buildQueryError(v.code, v.message, v.requestId), v.requestId);
      return;
    }
    if (v.kind === "player.snapshotRequest") {
      this.requestRefresh(true, true);
      return;
    }
    const requestId = v.envelope.requestId ?? null;
    const key = JSON.stringify([v.command.commandId, v.command.op, v.command.args ?? null]);
    const cached = this.acks.get(key);
    const now = Date.now();
    let result = cached && now < cached.expiresAt ? cached.result : undefined;
    const first = result === undefined;
    if (result === undefined) {
      this.acks.delete(key);
      if (this.acks.size >= MAX_COMMAND_ACK_CACHE) {
        const oldest = this.acks.keys().next().value;
        if (oldest !== undefined) this.acks.delete(oldest);
      }
      result = this.execute(v.command);
      this.acks.set(key, { result, expiresAt: now + COMMAND_ACK_TTL_MS });
    }
    const ack = await result;
    if (this.destroyed) return;
    // No answer from the page: the op may still run, so the outcome is unknown. Send no ack
    // and let the core settle it from observations or its own timeout, never as "failed".
    if (ack) this.emit("bridge.commandAck", ack, requestId);
    if (first && (ack === null || ack.dispatched)) this.followUp();
  }

  private followUp(): void {
    // An applied seek may emit no media event, especially while paused. Probe the
    // page clock immediately instead of waiting for a renderer timer or claiming
    // the requested position was observed.
    this.requestRefresh(false, false);
    for (const delay of FOLLOW_UP_DELAYS_MS) {
      const timer = setTimeout(() => {
        this.timers.delete(timer);
        this.requestRefresh(false, false);
      }, delay);
      this.timers.add(timer);
    }
  }

  private fail(commandId: CommandId, code: QueryErrorCode, message: string): BridgeCommandAckPayload {
    return { bridgeEpoch: this.epoch, commandId, dispatched: false, error: buildQueryError(code, message) };
  }

  private async shimCommand(
    commandId: CommandId,
    op: PageShimOp,
    args: Record<string, unknown>,
    timeoutMs?: number
  ): Promise<BridgeCommandAckPayload | null> {
    const res = await this.callShim(op, args, timeoutMs);
    if (!res) return null;
    // After a throwing page call or clicks that missed the target the page may have changed;
    // report it dispatched so the core settles it from what it observes, not as a refusal.
    if (!res.ok && (res.error === "PAGE_CALL_FAILED" || res.error === "NO_EFFECT")) {
      return { bridgeEpoch: this.epoch, commandId, dispatched: true, error: null };
    }
    if (!res.ok) return this.fail(commandId, mapShimError(res.error), res.message ?? `${op} failed`);
    return { bridgeEpoch: this.epoch, commandId, dispatched: true, error: null };
  }

  private async execute(cmd: PlayerCommandPayload): Promise<BridgeCommandAckPayload | null> {
    const args = cmd.args ?? {};
    const id = cmd.commandId;
    const caps = this.capabilities;
    const need = (cap: CapabilitySupport, what: string): BridgeCommandAckPayload | null =>
      cap === "supported" ? null : this.fail(id, "UNSUPPORTED_CAPABILITY", `${what} is ${cap} on this page`);

    if (args.trackInstanceId && this.instanceId && args.trackInstanceId !== this.instanceId) {
      return this.fail(id, "STALE_CONTEXT", "trackInstanceId does not match active playback instance");
    }

    switch (cmd.op) {
      case "play":
        return need(caps.playPause, "play") ?? this.shimCommand(id, "play", {});
      case "pause":
        return need(caps.playPause, "pause") ?? this.shimCommand(id, "pause", {});
      case "toggle": {
        const playing = this.playback?.playback === "playing" || this.playback?.playback === "buffering";
        return need(caps.playPause, "toggle") ?? this.shimCommand(id, playing ? "pause" : "play", {});
      }
      case "seek": {
        if (args.positionMs == null) return this.fail(id, "INVALID_PAYLOAD", "seek requires positionMs");
        const duration = this.page?.durationMs ?? null;
        const targetMs = duration !== null ? Math.min(args.positionMs, duration) : args.positionMs;
        return need(caps.seek, "seek") ?? this.shimCommand(id, "seek", { positionSec: targetMs / 1000 });
      }
      case "next":
        return need(caps.next, "next") ?? this.shimCommand(id, "next", {});
      case "previous":
        return need(caps.previous, "previous") ?? this.shimCommand(id, "previous", {});
      case "setVolume":
        if (args.volume == null) return this.fail(id, "INVALID_PAYLOAD", "setVolume requires volume in [0.0, 1.0]");
        return need(caps.volume, "volume") ?? this.shimCommand(id, "setVolume", { volume01: args.volume });
      case "setMuted":
        if (typeof args.muted !== "boolean") return this.fail(id, "INVALID_PAYLOAD", "setMuted requires boolean muted");
        return need(caps.volume, "mute") ?? this.shimCommand(id, "setMuted", { muted: args.muted });
      case "setShuffle":
        if (typeof args.shuffle !== "boolean") return this.fail(id, "INVALID_PAYLOAD", "setShuffle requires boolean shuffle");
        return need(caps.shuffle, "shuffle") ?? this.shimCommand(id, "setShuffle", { shuffle: args.shuffle });
      case "setRepeat":
        if (!args.repeat) return this.fail(id, "INVALID_PAYLOAD", "setRepeat requires repeat");
        return need(caps.repeat, "repeat") ?? this.shimCommand(id, "setRepeat", { repeat: args.repeat });
      case "setAutoplay":
        if (typeof args.autoplay !== "boolean") return this.fail(id, "INVALID_PAYLOAD", "setAutoplay requires boolean autoplay");
        return need(caps.queueAutoplay ?? "unsupported", "queue autoplay") ?? this.shimCommand(id, "setAutoplay", { autoplay: args.autoplay });
      case "setQueueFilter":
        if (!args.queueFilterId) return this.fail(id, "INVALID_PAYLOAD", "setQueueFilter requires queueFilterId");
        return need(caps.queueFilter ?? "unsupported", "Up Next filter") ?? this.shimCommand(id, "setQueueFilter", { filterId: args.queueFilterId });
      case "playTarget":
      case "playCollection": {
        const target = args.target;
        const requireContext = cmd.op === "playCollection";
        if (!target || (requireContext && (!target.playlistId || target.startIndex == null))) {
          return this.fail(id, "INVALID_PAYLOAD", requireContext ? "playCollection requires videoId, playlistId and startIndex" : "playTarget requires target");
        }
        if (!this.page?.controls.navigate) return this.fail(id, "UNSUPPORTED_CAPABILITY", "in-page navigation unavailable");
        return this.shimCommand(
          id,
          "navigate",
          {
            videoId: target.videoId,
            ...(target.playlistId ? { playlistId: target.playlistId } : {}),
            ...(target.startIndex != null ? { index: target.startIndex } : {}),
            ...(requireContext ? { requireContext: true } : {}),
            ...(requireContext && target.playlistEntryId != null ? { playlistEntryId: target.playlistEntryId } : {}),
          },
          SLOW_SHIM_TIMEOUT_MS
        );
      }
      case "selectQueueEntry": {
        if (!args.queueEntryId) return this.fail(id, "INVALID_PAYLOAD", "selectQueueEntry requires queueEntryId");
        if (args.queueRevision != null && args.queueRevision !== this.queue.currentRevision) {
          return this.fail(id, "STALE_CONTEXT", "queueRevision is stale relative to current queue snapshot");
        }
        const located = this.queue.locate(args.queueEntryId);
        if (!located) return this.fail(id, "STALE_CONTEXT", "queueEntryId not found in current bridgeEpoch queue");
        return need(caps.queueMirror, "queue") ?? this.shimCommand(id, "selectQueueItem", located);
      }
      case "removeQueueEntry":
      case "moveQueueEntry":
      case "clearUpcoming": {
        const clearing = cmd.op === "clearUpcoming";
        if (!args.queueEntryId || args.queueRevision == null || (clearing && !args.trackInstanceId)) {
          const required = clearing ? "current occurrence, queueRevision and trackInstanceId" : "occurrence and queueRevision";
          return this.fail(id, "INVALID_PAYLOAD", `queue edit requires ${required}`);
        }
        if (args.queueRevision !== this.queue.currentRevision) {
          return this.fail(id, "STALE_CONTEXT", "queueRevision is stale relative to current queue snapshot");
        }
        const moving = cmd.op === "moveQueueEntry";
        const unsupported = clearing
          ? need(caps.queueClear ?? "unsupported", "clearing upcoming entries")
          : need(moving ? caps.queueReorder : caps.queueRemove, "exact queue edit");
        if (unsupported) return unsupported;
        const target = this.queue.locate(args.queueEntryId);
        const before = moving && args.beforeQueueEntryId ? this.queue.locate(args.beforeQueueEntryId) : null;
        const page = this.page;
        if (!target || (moving && args.beforeQueueEntryId && !before) || !page?.queue || !page.instanceKey
          || (clearing && (args.queueEntryId !== this.queueSnapshot?.currentEntryId || args.trackInstanceId !== this.instanceId))) {
          return this.fail(id, "STALE_CONTEXT", "queue target, anchor or playback instance is stale");
        }
        const op = clearing ? "clearUpcoming" : moving ? "moveQueueItem" : "removeQueueItem";
        return this.shimCommand(id, op, {
          index: target.index,
          beforeIndex: before?.index ?? null,
          expectedKeys: page.queue.items.map((item) => item.key),
          expectedSelectedIndex: page.queue.selectedIndex,
          expectedInstanceKey: page.instanceKey,
        });
      }
      case "enqueue": {
        const target = args.target;
        const position = args.enqueuePosition;
        if (!target || (position !== "next" && position !== "last")) {
          return this.fail(id, "INVALID_PAYLOAD", "enqueue requires target and enqueuePosition");
        }
        return (
          need(caps.queueEnqueue, "enqueue") ??
          this.shimCommand(id, "enqueue", { videoId: target.videoId, position }, SLOW_SHIM_TIMEOUT_MS)
        );
      }
      case "stop":
        // Stop is a core transaction (pause, then seek to zero); the page has no stop of its own.
        return this.fail(id, "UNSUPPORTED_CAPABILITY", "stop is composed by the core from pause and seek");
    }
    return this.fail(id, "INVALID_PAYLOAD", "unknown op");
  }
}

const ACTIVE_BRIDGE_SYMBOL = Symbol.for("ytmcpActiveBridge");

/** Tears down the bridge installed on `win`, including one installed by an earlier injection. */
export function uninstallBridge(win: Window | undefined = typeof window !== "undefined" ? window : undefined): void {
  if (!win) return;
  const host = win as unknown as Record<symbol, (() => void) | undefined>;
  host[ACTIVE_BRIDGE_SYMBOL]?.();
  delete host[ACTIVE_BRIDGE_SYMBOL];
}

/**
 * Installs the page bridge on the music.youtube.com top frame. Idempotent per window, even
 * across separate injections of this bundle: the previous instance is torn down first so
 * one command never runs twice. The page shim (`page-shim.ts`) must be installed in the
 * page main world; the bridge itself touches only the DOM, `postMessage` and the
 * transport, so it can run in an isolated world.
 */
export function installBridge(
  transport: BridgeTransport,
  bridgeEpoch: BridgeEpoch,
  options: InstallBridgeOptions = {}
): BridgeHandle {
  const win = options.windowRef ?? (typeof window !== "undefined" ? window : undefined);
  const doc = options.documentRef ?? win?.document;
  uninstallBridge(win);
  const refused = (reason: string, epoch: BridgeEpoch): BridgeHandle => ({
    installed: false,
    bridgeEpoch: epoch,
    reason,
    uninstall: () => {},
    getStateSnapshot: () => null,
  });
  if (!win || !doc || !isTrustedMusicTopFrame(win)) return refused("UNTRUSTED_ORIGIN_OR_SUBFRAME", bridgeEpoch);
  if (typeof bridgeEpoch !== "string" || bridgeEpoch.trim() === "") return refused("INVALID_BRIDGE_EPOCH", "");

  const controller = new BridgeController(transport, bridgeEpoch, win, doc, options);
  const host = win as unknown as Record<symbol, (() => void) | undefined>;
  const uninstall = (): void => {
    if (host[ACTIVE_BRIDGE_SYMBOL] === uninstall) delete host[ACTIVE_BRIDGE_SYMBOL];
    controller.uninstall();
  };
  host[ACTIVE_BRIDGE_SYMBOL] = uninstall;
  controller.start();
  return { installed: true, bridgeEpoch, uninstall, getStateSnapshot: () => controller.state() };
}
