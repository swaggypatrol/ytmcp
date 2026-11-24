import type {
  AudioQualityLevel,
  BridgeEpoch,
  BridgeHealthPayload,
  BridgePlaybackObservedPayload,
  CapabilitySet,
  Envelope,
  OfficialUpcomingStatus,
  QueueFilter,
  QueueSnapshot,
  RepeatMode,
} from "@ytmcp/protocol";

export const MUSIC_ORIGIN = "https://music.youtube.com";
export const BRIDGE_VERSION = "0.2.0";
export const PAGE_SHIM_CHANNEL = "ytmcp:page-shim:v2";

export function isPlaylistEntryId(value: unknown): value is string {
  return typeof value === "string" && value.length > 0 && value.length <= 256
    && !/\p{Cc}/u.test(value) && new TextEncoder().encode(value).length <= 256;
}

/** Fixed operations the main-world page shim accepts. Nothing else is reachable through it. */
export type PageShimOp =
  | "probe"
  | "play"
  | "pause"
  | "seek"
  | "next"
  | "previous"
  | "setVolume"
  | "setMuted"
  | "setShuffle"
  | "setRepeat"
  | "setAutoplay"
  | "setQueueFilter"
  | "navigate"
  | "selectQueueItem"
  | "removeQueueItem"
  | "moveQueueItem"
  | "clearUpcoming"
  | "enqueue";

/** Error codes the shim returns; the bridge maps them onto protocol QueryErrorCodes. */
/**
 * UNAVAILABLE, INVALID_ARGS, STALE and INFINITE_QUEUE are returned before the page changed anything.
 * NO_EFFECT follows clicks whose result was not the target, and PAGE_CALL_FAILED a throwing page call;
 * the page may have changed in both cases.
 */
export type PageShimErrorCode = "UNAVAILABLE" | "INVALID_ARGS" | "STALE" | "INFINITE_QUEUE" | "NO_EFFECT" | "PAGE_CALL_FAILED";

export interface PageTrack {
  readonly videoId: string;
  readonly title: string;
  readonly artists: readonly string[];
  readonly albumTitle: string | null;
  readonly durationMs: number | null;
  readonly coverUrl: string | null;
  readonly available: boolean;
}

/** A queue item plus the page-side occurrence key (videoId|playlistSetVideoId|queue item index). */
export interface PageQueueItem extends PageTrack {
  readonly key: string;
}

export interface PageQueue {
  /** Absolute index of `items[0]` in the page queue. */
  readonly start: number;
  readonly total: number;
  readonly items: readonly PageQueueItem[];
  readonly selectedIndex: number | null;
  /** Radio/automix queues never end; appending to them is rejected by YouTube Music. */
  readonly infinite: boolean;
  /** Autoplay suggestions follow the explicit queue. */
  readonly hasAutoplayTail: boolean;
  /** The page keeps a continuation, so more explicit entries may load after the loaded ones. */
  readonly continuation: boolean;
  /** What the page plays after the explicit list on its own; absent when its suggestions are unreadable. */
  readonly officialUpcoming?: PageOfficialUpcoming;
  /** The Up Next filters the page offers now; absent when none are offered. */
  readonly filters?: readonly QueueFilter[];
}

/** The page's autoplay suggestions after the explicit list. `tracks` is only filled for `observed`. */
export interface PageOfficialUpcoming {
  readonly status: OfficialUpcomingStatus;
  readonly tracks: readonly PageTrack[];
}

export interface PageControls {
  readonly playPause: boolean;
  readonly seek: boolean;
  readonly next: boolean;
  readonly previous: boolean;
  readonly volume: boolean;
  readonly shuffle: boolean;
  readonly repeat: boolean;
  readonly enqueue: boolean;
  readonly editQueue: boolean;
  /** The page would clear the upcoming entries now. */
  readonly clearUpcoming: boolean;
  readonly setAutoplay: boolean;
  /** At least one Up Next filter's chip can be clicked now. */
  readonly setQueueFilter: boolean;
  readonly navigate: boolean;
}

/**
 * Per-track player facts read in the page main world. Position and duration come from the
 * YouTube player API, which is per track; the media element shares one MSE timeline across
 * gapless tracks and is therefore not used for time.
 */
export interface PageSnapshot {
  readonly hasPlayerApi: boolean;
  /** Opaque per-load playback nonce; changes when a new playback instance starts. */
  readonly instanceKey: string | null;
  readonly videoId: string | null;
  readonly track: PageTrack | null;
  readonly positionMs: number | null;
  readonly durationMs: number | null;
  /** How much of the current track has loaded, 0…1, from the player API. */
  readonly loadedFraction: number | null;
  readonly playerState: number | null;
  readonly volume0To100: number | null;
  readonly muted: boolean | null;
  readonly playbackRate: number | null;
  readonly shuffle: boolean | null;
  readonly repeat: RepeatMode | null;
  readonly autoplay: boolean | null;
  readonly codec: string | null;
  /** Stream number of the playing audio stream, from the player's statistics. */
  readonly audioItag: number | null;
  /** The page's own label of that stream, from its player response for the playing video. */
  readonly audioQuality: AudioQualityLevel | null;
  /** That stream's average bit rate in kbit/s, from the same format. */
  readonly audioBitrateKbps: number | null;
  readonly qualityLabel: string | null;
  readonly audioMode: "song" | "video" | null;
  readonly playbackContext?: NonNullable<BridgePlaybackObservedPayload["playbackContext"]>;
  readonly loggedIn: boolean;
  readonly accountHint: string | null;
  readonly queue: PageQueue | null;
  readonly controls: PageControls;
}

export interface PageShimRequest {
  readonly channel: typeof PAGE_SHIM_CHANNEL;
  readonly direction: "to-page";
  readonly requestId: string;
  readonly op: PageShimOp;
  readonly args?: Record<string, unknown>;
}

export interface PageShimResponse {
  readonly channel: typeof PAGE_SHIM_CHANNEL;
  readonly direction: "from-page";
  readonly requestId: string;
  readonly ok: boolean;
  readonly data?: PageSnapshot;
  readonly error?: PageShimErrorCode;
  readonly message?: string;
}

export interface BridgeTransport {
  sendEnvelope(envelope: Envelope): unknown;
  onEnvelope(listener: (envelope: Envelope) => void): () => void;
}

export interface InstallBridgeOptions {
  readonly windowRef?: Window;
  readonly documentRef?: Document;
  /** Health heartbeat; SPEC 9.3 asks for 10-15 s. */
  readonly healthIntervalMs?: number;
  /** Minimum spacing of position samples driven by `timeupdate` while playing. */
  readonly sampleIntervalMs?: number;
  readonly mutationDebounceMs?: number;
  readonly shimTimeoutMs?: number;
  /** How long a capability-only change must hold, unchanged, before bridge.ready is re-sent. */
  readonly readyDebounceMs?: number;
}

export interface BridgeStateSnapshot {
  readonly capabilities: CapabilitySet;
  readonly playback: BridgePlaybackObservedPayload | null;
  readonly queue: QueueSnapshot | null;
  readonly health: BridgeHealthPayload | null;
}

export interface BridgeHandle {
  readonly installed: boolean;
  readonly bridgeEpoch: BridgeEpoch;
  readonly reason?: string;
  uninstall(): void;
  getStateSnapshot(): BridgeStateSnapshot | null;
}
