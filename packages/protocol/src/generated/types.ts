/* eslint-disable */
/**
 * GENERATED FILE — DO NOT EDIT MANUALLY.
 * Regenerate via: just gen-protocol
 */

/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "AgentActivityRequest".
 */
export type AgentActivityRequest =
  | {
      action: "read";
    }
  | {
      action: "cancel";
      operationId: string;
      principal: string;
    }
  | {
      action: "clearDjPreference";
      preferenceId: string;
    };
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "AgentOperationPhase".
 */
export type AgentOperationPhase =
  "accepted" | "submitted" | "acknowledged" | "verified" | "unknown" | "partial" | "rejected" | "cancelled";
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "AgentPreferenceStance".
 */
export type AgentPreferenceStance = "prefer" | "avoid";
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "ThemeMode".
 */
export type ThemeMode = "dark" | "light" | "system";
/**
 * The language the UI is shown in. Values are the UI's BCP 47 locale tags.
 *
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "UiLanguage".
 */
export type UiLanguage = "en" | "zh-CN";
/**
 * Unique identifier for one authenticated account session epoch.
 *
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "AccountEpoch".
 */
export type AccountEpoch = string;
/**
 * Redacted local scope identifier isolating accounts in cache/state.
 *
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "AccountScope".
 */
export type AccountScope = string;
/**
 * Monotonic/opaque revision for credential material rotation.
 *
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "CredentialRevision".
 */
export type CredentialRevision = string;
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "AuthState".
 */
export type AuthState = "unknown" | "signedOut" | "signingIn" | "authenticated" | "expired" | "blocked";
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "CapabilitySupport".
 */
export type CapabilitySupport = "supported" | "unsupported" | "unverified";
/**
 * Unique identifier for one Rust core process instance.
 *
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "CoreEpoch".
 */
export type CoreEpoch = string;
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "NetworkState".
 */
export type NetworkState = "online" | "offline" | "unknown";
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "PlayerOp".
 */
export type PlayerOp =
  | "play"
  | "pause"
  | "toggle"
  | "stop"
  | "seek"
  | "setVolume"
  | "setMuted"
  | "next"
  | "previous"
  | "playTarget"
  | "playCollection"
  | "setShuffle"
  | "setRepeat"
  | "setAutoplay"
  | "enqueue"
  | "selectQueueEntry"
  | "removeQueueEntry"
  | "clearUpcoming"
  | "moveQueueEntry";
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "PlaybackStatus".
 */
export type PlaybackStatus = "idle" | "playing" | "paused" | "buffering" | "ended" | "error";
/**
 * Client-generated command deduplication identifier.
 *
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "CommandId".
 */
export type CommandId = string;
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "QueryErrorCode".
 */
export type QueryErrorCode =
  | "AUTH_REQUIRED"
  | "AUTH_EXPIRED"
  | "ACCOUNT_MISMATCH"
  | "RATE_LIMITED"
  | "NETWORK_OFFLINE"
  | "TIMEOUT"
  | "UNAVAILABLE_TRACK"
  | "UNSUPPORTED_CAPABILITY"
  | "BRIDGE_INCOMPATIBLE"
  | "STALE_CONTEXT"
  | "BUSY"
  | "INVALID_PAYLOAD"
  | "STORAGE_ERROR";
/**
 * Request/response correlation identifier.
 *
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "RequestId".
 */
export type RequestId = string;
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "CommandLifecycleStatus".
 */
export type CommandLifecycleStatus = "accepted" | "dispatched" | "observed" | "failed" | "indeterminate" | "cancelled";
/**
 * Unique identifier for one authorized page bridge handshake.
 *
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "BridgeEpoch".
 */
export type BridgeEpoch = string;
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "PlayerConnectionState".
 */
export type PlayerConnectionState = "starting" | "ready" | "stale" | "recovering" | "disconnected" | "incompatible";
/**
 * Occurrence-scoped queue entry identifier within a bridge epoch.
 *
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "QueueEntryId".
 */
export type QueueEntryId = string;
/**
 * Opaque YouTube Music browse endpoint identifier.
 *
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "BrowseId".
 */
export type BrowseId = string;
/**
 * Opaque YouTube Music album identifier.
 *
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "AlbumId".
 */
export type AlbumId = string;
/**
 * Opaque YouTube Music artist identifier.
 *
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "ArtistId".
 */
export type ArtistId = string;
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "TrackAvailability".
 */
export type TrackAvailability = "available" | "unavailable" | "regionBlocked" | "ageRestricted" | "unknown";
/**
 * Content-addressed local cover asset identifier generated by Rust storage.
 *
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "AssetId".
 */
export type AssetId = string;
/**
 * Opaque YouTube Music video identifier.
 *
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "VideoId".
 */
export type VideoId = string;
/**
 * Opaque YouTube Music playlist identifier.
 *
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "PlaylistId".
 */
export type PlaylistId = string;
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "RepeatMode".
 */
export type RepeatMode = "off" | "all" | "one";
/**
 * Unique playback instance identifier (distinguishes repeated plays).
 *
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "TrackInstanceId".
 */
export type TrackInstanceId = string;
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "QueueCompleteness".
 */
export type QueueCompleteness = "unknown" | "partial" | "complete";
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "CatalogQueryKind".
 */
export type CatalogQueryKind =
  | "home"
  | "searchSongs"
  | "libraryPlaylists"
  | "playlistTracks"
  | "albumDetail"
  | "artistDetail"
  | "likedSongs"
  | "recentHistory";
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "CatalogResponsePayload".
 */
export type CatalogResponsePayload =
  | (HomeFeed & {
      resultType: "home";
    })
  | (SearchResult & {
      resultType: "searchSongs";
    })
  | (PagedResult2 & {
      resultType: "libraryPlaylists";
    })
  | (PlaylistDetail & {
      resultType: "playlistTracks";
    })
  | (AlbumDetail & {
      resultType: "albumDetail";
    })
  | (ArtistDetail & {
      resultType: "artistDetail";
    })
  | (PagedResult & {
      resultType: "likedSongs";
    })
  | (RecentHistoryResult & {
      resultType: "recentHistory";
    })
  | (QueryError & {
      resultType: "error";
    });
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "Freshness".
 */
export type Freshness = "fresh" | "stale";
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "MessageKind".
 */
export type MessageKind =
  | "ui.subscribeSnapshot"
  | "ui.unsubscribe"
  | "ui.playerCommand"
  | "ui.catalogQuery"
  | "ui.lyricsQuery"
  | "ui.mutation"
  | "ui.agentActivity"
  | "ui.settingsUpdate"
  | "ui.openPlayerWindow"
  | "ui.logout"
  | "ui.clearCache"
  | "ui.ping"
  | "core.snapshot"
  | "core.delta"
  | "core.commandReceipt"
  | "core.catalogResponse"
  | "core.lyricsResponse"
  | "core.mutationResponse"
  | "core.agentActivity"
  | "core.settingsUpdated"
  | "core.error"
  | "core.pong"
  | "host.hello"
  | "core.ready"
  | "host.windowVisibility"
  | "host.powerResume"
  | "host.networkChanged"
  | "host.sessionMaterial"
  | "host.sessionCleared"
  | "host.bridgeLifecycle"
  | "core.outputGateControl"
  | "host.outputGateResult"
  | "core.resolveAsset"
  | "core.assetResolved"
  | "host.shutdown"
  | "bridge.ready"
  | "bridge.playbackObserved"
  | "bridge.queueObserved"
  | "bridge.health"
  | "bridge.commandAck"
  | "bridge.error"
  | "player.command"
  | "player.snapshotRequest";
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "LyricsKind".
 */
export type LyricsKind = "synced" | "plain" | "instrumental" | "notFound" | "unavailable" | "disabled";
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "MutationKind".
 */
export type MutationKind = "setLike" | "addToPlaylist";
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "MutationOutcome".
 */
export type MutationOutcome = "confirmed" | "rejected" | "indeterminate";
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "EnqueuePosition".
 */
export type EnqueuePosition = "next" | "last";
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "StateDelta".
 */
export type StateDelta =
  | {
      deltaType: "playerChanged";
      pending: PlayerPendingState;
      player: PlayerObservedState;
      revision: number;
    }
  | {
      deltaType: "queueReplaced";
      queue_summary: QueueSummary;
      revision: number;
    }
  | {
      auth_state: AuthStatusInfo;
      deltaType: "authChanged";
      revision: number;
    }
  | {
      capabilities: CapabilitySet;
      deltaType: "capabilitiesChanged";
      revision: number;
    }
  | {
      deltaType: "settingsChanged";
      revision: number;
      settings: AppSettings;
    }
  | {
      deltaType: "hostHealthChanged";
      host_health: HostHealth;
      revision: number;
    }
  | {
      deltaType: "networkChanged";
      network_state: NetworkState;
      revision: number;
    }
  | {
      deltaType: "commandUpdated";
      receipt: CommandReceipt;
      revision: number;
    }
  | {
      deltaType: "manualTakeover";
      generation: number;
      revision: number;
    }
  | {
      deltaType: "controlOperationChanged";
      operation_id: string;
      principal: string;
      revision: number;
    }
  | {
      deltaType: "resourceInvalidated";
      resource_id?: string | null;
      resource_kind: string;
      revision: number;
    };

/**
 * Root schema bundle exported for JSON Schema -> TypeScript generation.
 */
export interface ProtocolSchemaRoot {
  agentActivityRequest: AgentActivityRequest;
  agentActivitySnapshot: AgentActivitySnapshot;
  appSettings: AppSettings;
  appSettingsPatch: AppSettingsPatch;
  appSnapshot: AppSnapshot;
  assetResolved: AssetResolvedPayload;
  bridgeCommandAck: BridgeCommandAckPayload;
  bridgeHealth: BridgeHealthPayload;
  bridgeLifecycle: BridgeLifecyclePayload;
  bridgePlaybackObserved: BridgePlaybackObservedPayload;
  bridgeQueueObserved: QueueSnapshot;
  bridgeReady: BridgeReadyPayload;
  catalogQuery: CatalogQueryRequest;
  catalogResponse: CatalogResponsePayload;
  commandReceipt: CommandReceipt;
  coreReady: CoreReadyPayload;
  envelope: Envelope;
  hostHello: HostHelloPayload;
  hostWindowVisibility: HostWindowVisibilityPayload;
  lyricsQuery: LyricsQueryRequest;
  lyricsResult: LyricsResult;
  mutationRequest: MutationRequest;
  mutationResult: MutationResult;
  openPlayerWindow: OpenPlayerWindowPayload;
  outputGateControl: OutputGateControlPayload;
  outputGateResult: OutputGateResultPayload;
  playerCommand: PlayerCommandPayload;
  queryError: QueryError;
  resolveAsset: ResolveAssetPayload;
  sessionMaterial: SessionMaterialPayload;
  stateDelta: StateDelta;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "AgentActivitySnapshot".
 */
export interface AgentActivitySnapshot {
  operations: AgentOperationSummary[];
  /**
   * Newest first (bounded).
   */
  preferences?: AgentPreferenceSummary[];
  /**
   * All preferences of the account; more than `preferences` when only the newest are shown.
   */
  preferencesTotal?: number;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "AgentOperationSummary".
 */
export interface AgentOperationSummary {
  canCancel: boolean;
  operationId: string;
  phase: AgentOperationPhase;
  principal: string;
  resource?: string | null;
  totalItems: number;
  verifiedItems: number;
}
/**
 * A recorded DJ preference as the desktop shows it.
 *
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "AgentPreferenceSummary".
 */
export interface AgentPreferenceSummary {
  label?: string | null;
  preferenceId: string;
  recordedBy: string;
  stance: AgentPreferenceStance;
  subjectId: string;
  /**
   * `video` or `artist`.
   */
  subjectKind: string;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "AppSettings".
 */
export interface AppSettings {
  autoplayOnLaunch: boolean;
  closeToTray: boolean;
  lrclibEnabled: boolean;
  nativeWindowDecorations: boolean;
  reduceMotion: boolean;
  reduceTransparency: boolean;
  theme: ThemeMode;
  trackNotifications: boolean;
  /**
   * Settings saved before the UI language existed load as English.
   */
  uiLanguage?: UiLanguage & string;
  uiScalePercent: number;
}
/**
 * Partial settings update sent by the UI so concurrent edits to other fields are preserved.
 *
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "AppSettingsPatch".
 */
export interface AppSettingsPatch {
  autoplayOnLaunch?: boolean | null;
  closeToTray?: boolean | null;
  lrclibEnabled?: boolean | null;
  nativeWindowDecorations?: boolean | null;
  reduceMotion?: boolean | null;
  reduceTransparency?: boolean | null;
  theme?: ThemeMode | null;
  trackNotifications?: boolean | null;
  uiLanguage?: UiLanguage | null;
  uiScalePercent?: number | null;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "AppSnapshot".
 */
export interface AppSnapshot {
  accountEpoch?: AccountEpoch | null;
  authState: AuthStatusInfo;
  capabilities: CapabilitySet;
  coreEpoch: CoreEpoch;
  hostHealth: HostHealth;
  mockMode: boolean;
  networkState: NetworkState;
  pending: PlayerPendingState;
  player: PlayerObservedState;
  protocolVersion: number;
  queueSummary: QueueSummary;
  revision: number;
  settings: AppSettings;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "AuthStatusInfo".
 */
export interface AuthStatusInfo {
  accountEpoch?: AccountEpoch | null;
  accountScope?: AccountScope | null;
  channelLabel?: string | null;
  credentialRevision?: CredentialRevision | null;
  playerCatalogMatched: boolean;
  reason?: string | null;
  state: AuthState;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "CapabilitySet".
 */
export interface CapabilitySet {
  albumDetail: CapabilitySupport;
  artistDetail: CapabilitySupport;
  libraryPlaylists: CapabilitySupport;
  likeMutation: CapabilitySupport;
  likedSongs: CapabilitySupport;
  lyricsLrclib: CapabilitySupport;
  mpris: CapabilitySupport;
  next: CapabilitySupport;
  personalizedHome: CapabilitySupport;
  playPause: CapabilitySupport;
  playlistAddMutation: CapabilitySupport;
  playlistTracks: CapabilitySupport;
  previous: CapabilitySupport;
  queueAutoplay?: CapabilitySupport | null;
  /**
   * The page would clear the upcoming entries now (finite queue without a continuation).
   */
  queueClear?: CapabilitySupport | null;
  queueEnqueue: CapabilitySupport;
  queueMirror: CapabilitySupport;
  queueRemove: CapabilitySupport;
  queueReorder: CapabilitySupport;
  repeat: CapabilitySupport;
  search: CapabilitySupport;
  seek: CapabilitySupport;
  shuffle: CapabilitySupport;
  volume: CapabilitySupport;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "HostHealth".
 */
export interface HostHealth {
  coreRestartCount: number;
  gpuAccelerated?: boolean | null;
  lastError?: string | null;
  mprisActive: boolean;
  safeStorageAvailable: boolean;
  sandboxEnabled: boolean;
  secretStoreBackend: string;
  trayAvailable: boolean;
  waylandNative?: boolean | null;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "PlayerPendingState".
 */
export interface PlayerPendingState {
  activeOp?: PlayerOp | null;
  desiredPlayback?: PlaybackStatus | null;
  desiredPositionMs?: number | null;
  desiredVolume?: number | null;
  lastReceipt?: CommandReceipt | null;
  pendingCommandCount: number;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "CommandReceipt".
 */
export interface CommandReceipt {
  acceptedAtMs: number;
  commandId: CommandId;
  deduplicated: boolean;
  error?: QueryError | null;
  observedAtMs?: number | null;
  op: PlayerOp;
  status: CommandLifecycleStatus;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "QueryError".
 */
export interface QueryError {
  code: QueryErrorCode;
  requestId?: RequestId | null;
  retriable: boolean;
  retryAfterMs?: number | null;
  safeMessage: string;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "PlayerObservedState".
 */
export interface PlayerObservedState {
  audioMode?: string | null;
  autoplay?: boolean | null;
  bridgeEpoch?: BridgeEpoch | null;
  /**
   * How far the current track has loaded, as last reported by the page.
   */
  bufferedMs?: number | null;
  codec?: string | null;
  connection: PlayerConnectionState;
  currentQueueEntryId?: QueueEntryId | null;
  currentTrack?: Track | null;
  drmProtected?: boolean | null;
  muted: boolean;
  playback: PlaybackStatus;
  playbackContext?: PlaybackContext | null;
  qualityLabel?: string | null;
  repeat?: RepeatMode | null;
  shuffle?: boolean | null;
  stoppedConfirmed: boolean;
  timeAnchor: PlaybackTimeAnchor;
  trackInstanceId?: TrackInstanceId | null;
  volume: number;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "Track".
 */
export interface Track {
  album?: AlbumRef | null;
  artists?: ArtistRef[];
  availability: TrackAvailability;
  cover?: CoverRef | null;
  durationMs?: number | null;
  isLiked?: boolean | null;
  title: string;
  versionTag?: string | null;
  videoId: VideoId;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "AlbumRef".
 */
export interface AlbumRef {
  browseId?: BrowseId | null;
  id?: AlbumId | null;
  title: string;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "ArtistRef".
 */
export interface ArtistRef {
  id?: ArtistId | null;
  name: string;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "CoverRef".
 */
export interface CoverRef {
  accentOklch?: string | null;
  assetId?: AssetId | null;
  remoteUrl?: string | null;
}
/**
 * Playlist identity and index read from the official page's selected native queue item.
 *
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "PlaybackContext".
 */
export interface PlaybackContext {
  playlistEntryId?: string | null;
  playlistId: PlaylistId;
  startIndex: number;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "PlaybackTimeAnchor".
 */
export interface PlaybackTimeAnchor {
  anchorCoreMonotonicMs: number;
  durationMs?: number | null;
  extrapolating: boolean;
  playbackRate: number;
  positionMs: number;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "QueueSummary".
 */
export interface QueueSummary {
  completeness: QueueCompleteness;
  currentEntryId?: QueueEntryId | null;
  hasMore?: boolean | null;
  loadedCount: number;
  previewEntries?: QueueEntry[];
  queueRevision: number;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "QueueEntry".
 */
export interface QueueEntry {
  availability: TrackAvailability;
  entryId: QueueEntryId;
  track: Track;
  videoId: VideoId;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "AssetResolvedPayload".
 */
export interface AssetResolvedPayload {
  assetId: AssetId;
  error?: QueryError | null;
  filePath?: string | null;
  mimeType?: string | null;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "BridgeCommandAckPayload".
 */
export interface BridgeCommandAckPayload {
  bridgeEpoch: BridgeEpoch;
  commandId: CommandId;
  dispatched: boolean;
  error?: QueryError | null;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "BridgeHealthPayload".
 */
export interface BridgeHealthPayload {
  bridgeEpoch: BridgeEpoch;
  controlsBound: boolean;
  healthy: boolean;
  reason?: string | null;
  videoElementPresent: boolean;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "BridgeLifecyclePayload".
 */
export interface BridgeLifecyclePayload {
  bridgeEpoch?: BridgeEpoch | null;
  event: string;
  isMusicOrigin?: boolean | null;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "BridgePlaybackObservedPayload".
 */
export interface BridgePlaybackObservedPayload {
  audioMode?: string | null;
  autoplay?: boolean | null;
  bridgeEpoch: BridgeEpoch;
  /**
   * How far the current track has loaded, from its start, in whole seconds (as ms).
   */
  bufferedMs?: number | null;
  codec?: string | null;
  currentQueueEntryId?: QueueEntryId | null;
  currentTrack?: Track | null;
  drmProtected?: boolean | null;
  durationMs?: number | null;
  muted: boolean;
  playback: PlaybackStatus;
  playbackContext?: PlaybackContext | null;
  playbackRate: number;
  positionMs: number;
  qualityLabel?: string | null;
  repeat?: RepeatMode | null;
  sampleAgeMs: number;
  shuffle?: boolean | null;
  trackInstanceId?: TrackInstanceId | null;
  volume: number;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "QueueSnapshot".
 */
export interface QueueSnapshot {
  bridgeEpoch?: BridgeEpoch | null;
  completeness: QueueCompleteness;
  currentEntryId?: QueueEntryId | null;
  entries?: QueueEntry[];
  /**
   * The explicit, non-radio list is loaded from its first to its last entry. An autoplay
   * tail after it can still make `completeness` partial; the explicit end stays provable.
   */
  explicitComplete?: boolean | null;
  hasMore?: boolean | null;
  loadedRange?: QueueRange | null;
  /**
   * The page keeps a continuation for the explicit list, so more entries may load after the
   * last loaded one even when the loaded list looks complete. Official album queues keep one
   * after every entry has loaded, so it does not by itself mean that anything follows.
   */
  mayLoadMore?: boolean | null;
  queueRevision: number;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "QueueRange".
 */
export interface QueueRange {
  endExclusive: number;
  start: number;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "BridgeReadyPayload".
 */
export interface BridgeReadyPayload {
  accountHint?: string | null;
  bridgeEpoch: BridgeEpoch;
  bridgeVersion: string;
  capabilities: CapabilitySet;
  loggedInObserved: boolean;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "CatalogQueryRequest".
 */
export interface CatalogQueryRequest {
  albumId?: AlbumId | null;
  artistId?: ArtistId | null;
  cursor?: string | null;
  kind: CatalogQueryKind;
  limit?: number | null;
  playlistId?: PlaylistId | null;
  query?: string | null;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "HomeFeed".
 */
export interface HomeFeed {
  fallbackNotice?: string | null;
  freshness: Freshness;
  personalizedSupported: boolean;
  pinnedPlaylists?: PlaylistSummary[];
  recentTracks?: Track[];
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "PlaylistSummary".
 */
export interface PlaylistSummary {
  browseId?: BrowseId | null;
  cover?: CoverRef | null;
  playlistId: PlaylistId;
  subtitle?: string | null;
  title: string;
  trackCount?: number | null;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "SearchResult".
 */
export interface SearchResult {
  albums?: AlbumRef[];
  artists?: ArtistRef[];
  playlists?: PlaylistSummary[];
  query: string;
  songs: PagedResult;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "PagedResult".
 */
export interface PagedResult {
  accountScope: AccountScope;
  fetchedAtMs: number;
  freshness: Freshness;
  items: Track[];
  nextCursor?: string | null;
  providerVersion: string;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "PagedResult2".
 */
export interface PagedResult2 {
  accountScope: AccountScope;
  fetchedAtMs: number;
  freshness: Freshness;
  items: PlaylistSummary[];
  nextCursor?: string | null;
  providerVersion: string;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "PlaylistDetail".
 */
export interface PlaylistDetail {
  author?: string | null;
  cover?: CoverRef | null;
  description?: string | null;
  playlistId: PlaylistId;
  title: string;
  tracks: PagedResult;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "AlbumDetail".
 */
export interface AlbumDetail {
  albumId: AlbumId;
  artists?: ArtistRef[];
  /**
   * The album's provider audio playlist (`OLAK5uy_…`) used as playback context.
   * Present only when every track in `tracks` was matched to a playable album row;
   * otherwise `None`, and the UI plays single tracks.
   */
  audioPlaylistId?: PlaylistId | null;
  cover?: CoverRef | null;
  /**
   * Aligned 1:1 with `tracks`: the provider's 0-based watch index of each track in
   * `audio_playlist_id`. Non-empty only when `audio_playlist_id` is present.
   */
  playbackIndexes?: number[];
  title: string;
  tracks: Track[];
  year?: number | null;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "ArtistDetail".
 */
export interface ArtistDetail {
  albums?: AlbumRef[];
  artistId: ArtistId;
  cover?: CoverRef | null;
  description?: string | null;
  name: string;
  topSongs?: Track[];
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "RecentHistoryResult".
 */
export interface RecentHistoryResult {
  freshness: Freshness;
  items: Track[];
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "CoreReadyPayload".
 */
export interface CoreReadyPayload {
  coreEpoch: CoreEpoch;
  protocolVersion: number;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "Envelope".
 */
export interface Envelope {
  accountEpoch?: AccountEpoch | null;
  bridgeEpoch?: BridgeEpoch | null;
  coreEpoch?: CoreEpoch | null;
  kind: MessageKind;
  payload: unknown;
  protocolVersion: number;
  requestId?: RequestId | null;
  seq: number;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "HostHelloPayload".
 */
export interface HostHelloPayload {
  appVersion: string;
  chromiumVersion: string;
  electronVersion: string;
  hostHealth: HostHealth;
  mockMode: boolean;
  networkState: NetworkState;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "HostWindowVisibilityPayload".
 */
export interface HostWindowVisibilityPayload {
  mainFocused: boolean;
  mainVisible: boolean;
  playerVisible: boolean;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "LyricsQueryRequest".
 */
export interface LyricsQueryRequest {
  resetUserOffset: boolean;
  setUserOffsetMs?: number | null;
  track: LyricsMatchMetadata;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "LyricsMatchMetadata".
 */
export interface LyricsMatchMetadata {
  album?: string | null;
  artist: string;
  durationMs?: number | null;
  title: string;
  versionTag?: string | null;
  videoId: VideoId;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "LyricsResult".
 */
export interface LyricsResult {
  error?: QueryError | null;
  fetchedAtMs: number;
  fileOffsetMs: number;
  kind: LyricsKind;
  language?: string | null;
  matchBasis?: string | null;
  plainText?: string | null;
  source: string;
  syncedLines?: LyricsLine[];
  userOffsetMs: number;
  videoId: VideoId;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "LyricsLine".
 */
export interface LyricsLine {
  text: string;
  timeMs: number;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "MutationRequest".
 */
export interface MutationRequest {
  confirmedByUser: boolean;
  kind: MutationKind;
  liked?: boolean | null;
  playlistId?: PlaylistId | null;
  videoId: VideoId;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "MutationResult".
 */
export interface MutationResult {
  error?: QueryError | null;
  kind: MutationKind;
  outcome: MutationOutcome;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "OpenPlayerWindowPayload".
 */
export interface OpenPlayerWindowPayload {
  reason: string;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "OutputGateControlPayload".
 */
export interface OutputGateControlPayload {
  gated: boolean;
  transactionId: string;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "OutputGateResultPayload".
 */
export interface OutputGateResultPayload {
  applied: boolean;
  error?: string | null;
  transactionId: string;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "PlayerCommandPayload".
 */
export interface PlayerCommandPayload {
  args?: PlayerCommandArgs | null;
  commandId: CommandId;
  op: PlayerOp;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "PlayerCommandArgs".
 */
export interface PlayerCommandArgs {
  autoplay?: boolean | null;
  beforeQueueEntryId?: QueueEntryId | null;
  enqueuePosition?: EnqueuePosition | null;
  muted?: boolean | null;
  positionMs?: number | null;
  queueEntryId?: QueueEntryId | null;
  queueRevision?: number | null;
  repeat?: RepeatMode | null;
  shuffle?: boolean | null;
  target?: PlaybackRef | null;
  trackInstanceId?: TrackInstanceId | null;
  volume?: number | null;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "PlaybackRef".
 */
export interface PlaybackRef {
  browseId?: BrowseId | null;
  playlistEntryId?: string | null;
  playlistId?: PlaylistId | null;
  queueEntryId?: QueueEntryId | null;
  startIndex?: number | null;
  videoId: VideoId;
}
/**
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "ResolveAssetPayload".
 */
export interface ResolveAssetPayload {
  assetId: AssetId;
}
/**
 * Host -> Core secret session material. NEVER logged, serialized to disk, or forwarded to UI.
 *
 * This interface was referenced by `ProtocolSchemaRoot`'s JSON-Schema
 * via the `definition` "SessionMaterialPayload".
 */
export interface SessionMaterialPayload {
  accountEpoch: AccountEpoch;
  authUserIndex?: number | null;
  channelLabel?: string | null;
  cookiePairs: {
    [k: string]: string | undefined;
  };
  credentialRevision: CredentialRevision;
}
