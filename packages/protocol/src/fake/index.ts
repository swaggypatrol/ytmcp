import type {
  AccountEpoch,
  AppSettings,
  AppSettingsPatch,
  AppSnapshot,
  CatalogQueryRequest,
  CatalogResponsePayload,
  CollectionPlaybackEntry,
  CommandReceipt,
  HostEnvironmentSnapshotPayload,
  LikedSongsPage,
  LyricsQueryRequest,
  LyricsResult,
  MomentDesktopRequest,
  MomentDesktopSnapshot,
  MutationRequest,
  MutationResult,
  OwnerMomentConfig,
  PlayerCommandPayload,
  PlaylistSummary,
  PlayerObservedState,
  QueryError,
  QueryErrorCode,
  QueueEntry,
  RecentSearchesRequest,
  RecentSearchesSnapshot,
  StateDelta,
  Track,
} from "../generated/types.js";
import { MAX_RECENT_SEARCH_CHARS, MAX_RECENT_SEARCHES, PROTOCOL_VERSION } from "../validator.js";

/** A request the mock core refused, carrying the error the real core would send as `core.error`. */
export class FakeCoreRequestError extends Error {
  constructor(readonly error: QueryError) {
    super(error.safeMessage);
    this.name = "FakeCoreRequestError";
  }
}

export type FakeScenario =
  | "normal"
  | "loading"
  | "empty"
  | "offlineCached"
  | "authExpired"
  | "unsupported"
  | "bridgeCrashed"
  | "indeterminateNext";

export type DeltaListener = (delta: StateDelta) => void;

export function createSyntheticTracks(count: number, prefix = "mock"): Track[] {
  const tracks: Track[] = new Array<Track>(count);
  // Invented titles; the CJK ones exercise font fallback, lang hints and CJK search.
  const sampleTitles = [
    "夜の硝子 (Synthetic Fixture)",
    "Midnight City Lights — Very Long Title With Mixed English and 中文字符 日本語タイトル 한국어",
    "青い残響 (Acoustic Session)",
    "Neon Drift over Tokyo Bay",
  ];
  for (let i = 0; i < count; i++) {
    const baseTitle = sampleTitles[i % sampleTitles.length]!;
    tracks[i] = {
      videoId: `${prefix}-vid-${String(i + 1).padStart(5, "0")}`,
      title: `${baseTitle} #${i + 1}`,
      artists: [
        { id: `${prefix}-art-${(i % 17) + 1}`, name: `Synth Artist ${(i % 17) + 1}` },
        { id: null, name: "Feat. Fixture Ensemble" },
      ],
      album: {
        id: `${prefix}-alb-${(i % 9) + 1}`,
        browseId: null,
        title: `Synthetic Album ${(i % 9) + 1}`,
      },
      durationMs: 185_000 + (i % 90) * 1_000,
      cover: null,
      availability: i % 37 === 36 ? "regionBlocked" : "available",
      versionTag: i % 5 === 0 ? "acoustic" : null,
      isLiked: i % 4 === 0,
    };
  }
  return tracks;
}

export const CALIBRATED_LRC_FIXTURE: LyricsResult = {
  videoId: "mock-vid-00001",
  kind: "synced",
  source: "synthetic-fixture",
  language: "ja",
  matchBasis: "fixture-calibrated-lrc",
  fetchedAtMs: 1_763_816_400_000,
  fileOffsetMs: 0,
  userOffsetMs: 0,
  syncedLines: [
    // Invented Japanese lines, not taken from any published song.
    { timeMs: 500, text: "[Synthetic test lyrics · not a live service]" },
    { timeMs: 3_000, text: "硝子の街に灯りがともる" },
    { timeMs: 7_200, text: "遠い波の音を数えて" },
    { timeMs: 12_500, text: "「またね」と小さく笑った" },
    { timeMs: 18_000, text: "朝が来るまで歌っていよう" },
  ],
  plainText: null,
  error: null,
};

/** Synthetic metadata for the second mock track, used to exercise the audio-quality label. */
const SECOND_TRACK_AUDIO = {
  audioQuality: "high",
  audioBitrateKbps: 192,
  codec: "mp4a.40.2",
} satisfies Partial<PlayerObservedState>;
const UNKNOWN_MOCK_AUDIO = {
  audioQuality: null,
  audioBitrateKbps: null,
  codec: "UNKNOWN (Mock)",
} satisfies Partial<PlayerObservedState>;

export class FakeCoreClient {
  private snapshot: AppSnapshot;
  private listeners = new Set<DeltaListener>();
  private seenCommands = new Map<string, CommandReceipt>();
  private scenario: FakeScenario;
  private playlists: PlaylistSummary[];
  private tracks10k: Track[];
  private likedVideoIds: Set<string>;
  /** Each mock playlist's entries in order (`entryId` is the provider occurrence), made on first use. */
  private playlistEntries = new Map<string, { entryId: string; videoId: string }[]>();
  /** The core's undo records: token → the one entry an addition created, for five minutes. */
  private undoRecords = new Map<string, { accountEpoch: AccountEpoch; playlistId: string; videoId: string; entryId: string; atMs: number }>();
  private nextEntry = 0;
  private injectGapNext = false;
  /** Recent searches by mock account scope, newest first, as the core keeps them. */
  private recentSearchesByScope = new Map<string, RecentSearchesSnapshot["items"]>();
  private momentConfig: OwnerMomentConfig = {
    configRevision: 0,
    shareTime: false,
    holidaysEnabled: false,
    holidayCountry: null,
    holidayState: null,
    holidayRegion: null,
    includeCulturalObservances: false,
    sceneCity: null,
    shareListeningIntents: true,
    sharedPrincipalsByAccount: {},
  };

  constructor(scenario: FakeScenario = "normal") {
    this.scenario = scenario;
    this.tracks10k = createSyntheticTracks(10_000, "mock");
    this.likedVideoIds = new Set(this.tracks10k.filter((track) => track.isLiked).map((track) => track.videoId));
    this.playlists = [
      {
        playlistId: "mock-pl-favorites",
        browseId: null,
        title: "Synthetic Favorites (Mock 10k)",
        subtitle: "10,000 synthetic test tracks",
        trackCount: 10_000,
        cover: null,
      },
      {
        playlistId: "mock-pl-late-night",
        browseId: null,
        title: "Late-Night City Glass Radio (Mock)",
        subtitle: "Synthetic Ambient & City Pop",
        trackCount: 42,
        cover: null,
      },
    ];
    this.snapshot = this.buildInitialSnapshot(scenario);
  }

  private buildInitialSnapshot(scenario: FakeScenario): AppSnapshot {
    const firstTrack = this.tracks10k[0]!;
    const secondTrack = this.tracks10k[1]!;
    const queueEntries: QueueEntry[] = [
      { entryId: "qe-mock-1", videoId: firstTrack.videoId, track: firstTrack, availability: "available" },
      { entryId: "qe-mock-2", videoId: secondTrack.videoId, track: secondTrack, availability: "available" },
      // Intentional duplicate videoId to verify occurrence-id handling
      { entryId: "qe-mock-3", videoId: firstTrack.videoId, track: firstTrack, availability: "available" },
    ];

    const base: AppSnapshot = {
      protocolVersion: PROTOCOL_VERSION,
      coreEpoch: "mock-core-epoch-1",
      accountEpoch: scenario === "authExpired" ? null : "mock-account-epoch-1",
      revision: 1,
      hostHealth: {
        waylandNative: true,
        gpuAccelerated: true,
        sandboxEnabled: true,
        secretStoreBackend: "mock-secret-store",
        safeStorageAvailable: true,
        trayAvailable: true,
        mprisActive: false,
        coreRestartCount: 0,
        lastError: scenario === "bridgeCrashed" ? "Mock bridge renderer crashed" : null,
      },
      authState: {
        state: scenario === "authExpired" ? "expired" : "authenticated",
        accountEpoch: scenario === "authExpired" ? null : "mock-account-epoch-1",
        accountScope: "mock-scope",
        credentialRevision: "mock-cred-1",
        channelLabel: "Mock Test Profile",
        playerCatalogMatched: scenario !== "authExpired",
        reason: scenario === "authExpired" ? "Mock sign-in session expired. Sign in again on the official page." : null,
      },
      player: {
        connection: scenario === "bridgeCrashed" ? "disconnected" : "ready",
        playback: "paused",
        bridgeEpoch: "mock-bridge-epoch-1",
        trackInstanceId: "ti-mock-1",
        currentTrack: scenario === "empty" ? null : firstTrack,
        currentQueueEntryId: scenario === "empty" ? null : "qe-mock-1",
        timeAnchor: {
          positionMs: 12_000,
          durationMs: firstTrack.durationMs ?? 185_000,
          playbackRate: 1.0,
          anchorCoreMonotonicMs: 0,
          extrapolating: false,
        },
        bufferedMs: Math.round((firstTrack.durationMs ?? 185_000) * 0.4),
        volume: 0.72,
        muted: false,
        shuffle: false,
        repeat: "off",
        ...UNKNOWN_MOCK_AUDIO,
        audioMode: "Mock Fixture",
        qualityLabel: "UNKNOWN",
        drmProtected: null,
        stoppedConfirmed: false,
      },
      pending: {
        pendingCommandCount: 0,
        activeOp: null,
        desiredPlayback: null,
        desiredPositionMs: null,
        desiredVolume: null,
        lastReceipt: null,
      },
      queueSummary: {
        queueRevision: 1,
        currentEntryId: scenario === "empty" ? null : "qe-mock-1",
        loadedCount: scenario === "empty" ? 0 : queueEntries.length,
        completeness: scenario === "empty" ? "unknown" : "partial",
        hasMore: scenario !== "empty",
        previewEntries: scenario === "empty" ? [] : queueEntries,
        previewStart: 0,
        // YouTube Music's own continuation after the explicit list, as the page reports it.
        officialUpcoming:
          scenario === "empty" ? null : { status: "observed", tracks: this.tracks10k.slice(20, 28) },
        filters:
          scenario === "empty"
            ? null
            : ["All", "Familiar", "Discover", "Popular"].map((label, i) => ({
                filterId: `qf-mock-${i}`,
                label,
                selected: i === 0,
              })),
      },
      capabilities: {
        playPause: scenario === "unsupported" ? "unsupported" : "supported",
        seek: scenario === "unsupported" ? "unsupported" : "supported",
        next: "supported",
        previous: "supported",
        volume: "supported",
        shuffle: "unsupported",
        repeat: "unsupported",
        queueMirror: "supported",
        queueEnqueue: "unsupported",
        queueReorder: "unsupported",
        queueRemove: "unsupported",
        search: "supported",
        libraryPlaylists: "supported",
        playlistTracks: "supported",
        albumDetail: "supported",
        artistDetail: "supported",
        likedSongs: "supported",
        personalizedHome: "unsupported",
        likeMutation: scenario === "normal" || scenario === "indeterminateNext" ? "supported" : "unsupported",
        playlistAddMutation: scenario === "normal" || scenario === "indeterminateNext" ? "supported" : "unsupported",
        lyricsLrclib: "unsupported",
        mpris: "unsupported",
      },
      settings: {
        theme: "dark",
        reduceMotion: false,
        reduceTransparency: false,
        uiScalePercent: 100,
        autoplayOnLaunch: false,
        closeToTray: true,
        lrclibEnabled: true,
        trackNotifications: false,
        nativeWindowDecorations: true,
        uiLanguage: "en",
      },
      networkState: scenario === "offlineCached" ? "offline" : "online",
      mockMode: true,
    };
    return base;
  }

  public setScenario(scenario: FakeScenario): void {
    this.scenario = scenario;
    const nextSnap = this.buildInitialSnapshot(scenario);
    nextSnap.revision = this.snapshot.revision + 1;
    this.snapshot = nextSnap;
    this.emit({
      deltaType: "playerChanged",
      revision: nextSnap.revision,
      player: nextSnap.player,
      pending: nextSnap.pending,
    });
  }

  public simulateRevisionGapOnNextDelta(): void {
    this.injectGapNext = true;
  }

  public subscribeSnapshot(listener: DeltaListener): {
    snapshot: AppSnapshot;
    unsubscribe: () => void;
  } {
    this.listeners.add(listener);
    return {
      snapshot: structuredClone(this.snapshot),
      unsubscribe: () => {
        this.listeners.delete(listener);
      },
    };
  }

  private emit(delta: StateDelta): void {
    for (const listener of this.listeners) {
      listener(structuredClone(delta));
    }
  }

  public sendPlayerCommand(cmd: PlayerCommandPayload): CommandReceipt {
    const cached = this.seenCommands.get(cmd.commandId);
    if (cached) {
      return { ...cached, deduplicated: true };
    }

    const now = Date.now();
    const acceptedReceipt: CommandReceipt = {
      commandId: cmd.commandId,
      op: cmd.op,
      status: "accepted",
      acceptedAtMs: now,
      observedAtMs: null,
      error: null,
      deduplicated: false,
    };

    const gapJump = this.injectGapNext ? 2 : 1;
    this.injectGapNext = false;
    this.snapshot.revision += gapJump;
    this.snapshot.pending = {
      ...this.snapshot.pending,
      pendingCommandCount: 1,
      activeOp: cmd.op,
      lastReceipt: acceptedReceipt,
    };
    this.emit({
      deltaType: "playerChanged",
      revision: this.snapshot.revision,
      player: structuredClone(this.snapshot.player),
      pending: structuredClone(this.snapshot.pending),
    });

    if (cmd.op === "next" && this.scenario === "indeterminateNext") {
      const indeterminate: CommandReceipt = {
        ...acceptedReceipt,
        status: "indeterminate",
        error: {
          code: "TIMEOUT",
          retriable: false,
          retryAfterMs: null,
          safeMessage: "The track change was not confirmed in time and was not retried. Check the official player page.",
          requestId: null,
        },
      };
      this.seenCommands.set(cmd.commandId, indeterminate);
      this.snapshot.revision += 1;
      this.snapshot.pending = {
        ...this.snapshot.pending,
        pendingCommandCount: 0,
        activeOp: null,
        lastReceipt: indeterminate,
      };
      this.emit({
        deltaType: "commandUpdated",
        revision: this.snapshot.revision,
        receipt: indeterminate,
      });
      return indeterminate;
    }

    const p = this.snapshot.player;
    if (cmd.op === "play") {
      p.playback = "playing";
      p.stoppedConfirmed = false;
    } else if (cmd.op === "pause") {
      p.playback = "paused";
    } else if (cmd.op === "toggle") {
      p.playback = p.playback === "playing" ? "paused" : "playing";
    } else if (cmd.op === "stop") {
      p.playback = "idle";
      p.timeAnchor.positionMs = 0;
      p.stoppedConfirmed = true;
    } else if (cmd.op === "seek" && cmd.args?.positionMs != null) {
      p.timeAnchor.positionMs = cmd.args.positionMs;
    } else if (cmd.op === "setVolume" && cmd.args?.volume != null) {
      p.volume = Math.max(0, Math.min(1, cmd.args.volume));
    } else if (cmd.op === "setMuted" && cmd.args?.muted != null) {
      p.muted = cmd.args.muted;
    } else if (cmd.op === "next" || cmd.op === "previous") {
      const idx = cmd.op === "next" ? 1 : 0;
      const track = this.tracks10k[idx]!;
      p.currentTrack = track;
      Object.assign(p, idx === 1 ? SECOND_TRACK_AUDIO : UNKNOWN_MOCK_AUDIO);
      p.trackInstanceId = `ti-mock-${Date.now()}`;
      p.timeAnchor.positionMs = 0;
      p.timeAnchor.durationMs = track.durationMs ?? null;
    }

    const observedReceipt: CommandReceipt = {
      ...acceptedReceipt,
      status: "observed",
      observedAtMs: Date.now(),
    };
    this.seenCommands.set(cmd.commandId, observedReceipt);
    this.snapshot.revision += 1;
    this.snapshot.pending = {
      pendingCommandCount: 0,
      activeOp: null,
      desiredPlayback: null,
      desiredPositionMs: null,
      desiredVolume: null,
      lastReceipt: observedReceipt,
    };
    this.emit({
      deltaType: "playerChanged",
      revision: this.snapshot.revision,
      player: structuredClone(p),
      pending: structuredClone(this.snapshot.pending),
    });
    return observedReceipt;
  }

  public updateSettings(patch: AppSettingsPatch): AppSettings {
    const base = this.snapshot.settings;
    const next: AppSettings = {
      theme: patch.theme ?? base.theme,
      reduceMotion: patch.reduceMotion ?? base.reduceMotion,
      reduceTransparency: patch.reduceTransparency ?? base.reduceTransparency,
      uiScalePercent: patch.uiScalePercent ?? base.uiScalePercent,
      autoplayOnLaunch: patch.autoplayOnLaunch ?? base.autoplayOnLaunch,
      closeToTray: patch.closeToTray ?? base.closeToTray,
      lrclibEnabled: patch.lrclibEnabled ?? base.lrclibEnabled,
      trackNotifications: patch.trackNotifications ?? base.trackNotifications,
      nativeWindowDecorations: patch.nativeWindowDecorations ?? base.nativeWindowDecorations,
      uiLanguage: patch.uiLanguage ?? base.uiLanguage,
    };
    this.snapshot.revision += 1;
    this.snapshot.settings = structuredClone(next);
    this.emit({
      deltaType: "settingsChanged",
      revision: this.snapshot.revision,
      settings: structuredClone(next),
    });
    return structuredClone(next);
  }

  /** Applies a confirmed change to the mock account and publishes the affected projections. */
  public sendMutation(req: MutationRequest, accountEpoch: AccountEpoch | null): MutationResult {
    const reject = (code: QueryErrorCode, safeMessage: string): MutationResult => ({
      kind: req.kind, outcome: "rejected", error: { code, safeMessage, retriable: false },
    });
    if (!req.confirmedByUser || accountEpoch === null) {
      return reject("INVALID_PAYLOAD", "Mock account writes require confirmation and an account epoch");
    }
    const { authState, capabilities, networkState } = this.snapshot;
    if (authState.state === "expired") return reject("AUTH_EXPIRED", "Mock auth expired");
    if (accountEpoch !== authState.accountEpoch || !authState.playerCatalogMatched) {
      return reject("ACCOUNT_MISMATCH", "Mock account does not match");
    }
    if (networkState === "offline") return reject("NETWORK_OFFLINE", "Mock account is offline");
    if (req.kind === "addToPlaylist" || req.kind === "undoAddToPlaylist") {
      if (capabilities.playlistAddMutation !== "supported") {
        return reject("UNSUPPORTED_CAPABILITY", "Mock account does not support this write");
      }
      return req.kind === "addToPlaylist" ? this.addToPlaylist(req, accountEpoch) : this.undoAddToPlaylist(req, accountEpoch);
    }
    if (req.kind !== "setLike" || capabilities.likeMutation !== "supported") {
      return reject("UNSUPPORTED_CAPABILITY", "Mock account does not support this write");
    }
    const liked = req.liked;
    if (liked == null) return reject("INVALID_PAYLOAD", "Mock like requires a rating");
    const index = this.tracks10k.findIndex((track) => track.videoId === req.videoId);
    if (index < 0) return reject("UNAVAILABLE_TRACK", "Mock track does not exist");

    if (liked !== this.likedVideoIds.has(req.videoId)) {
      if (liked) this.likedVideoIds.add(req.videoId);
      else this.likedVideoIds.delete(req.videoId);
      const track = { ...this.tracks10k[index]!, isLiked: liked };
      this.tracks10k[index] = track;
      this.publishTrackRating(track);
      this.emit({
        deltaType: "resourceInvalidated",
        resource_kind: "likedSongs",
        resource_id: req.videoId,
        revision: ++this.snapshot.revision,
      });
    }
    return { kind: req.kind, outcome: "confirmed", error: null };
  }

  /** How long the mock core keeps an addition undoable, as the real core does. */
  static readonly UNDO_LIFETIME_MS = 5 * 60 * 1000;

  /**
   * Another device removes `videoId`'s entries from a mock playlist, so a later Undo finds its
   * entry already gone.
   */
  public simulateRemovalElsewhere(playlistId: string, videoId: string): void {
    const entries = this.entriesOf(playlistId);
    if (entries) this.playlistEntries.set(playlistId, entries.filter((entry) => entry.videoId !== videoId));
  }

  /** A mock playlist's entries: Favorites holds every synthetic track, Late-Night the first 42. */
  private entriesOf(playlistId: string): { entryId: string; videoId: string }[] | null {
    const known = this.playlistEntries.get(playlistId);
    if (known) return known;
    const size = playlistId === "mock-pl-favorites" ? this.tracks10k.length : playlistId === "mock-pl-late-night" ? 42 : -1;
    if (size < 0) return null;
    const entries = this.tracks10k.slice(0, size).map((track, i) => ({ entryId: `${playlistId}-entry-${i}`, videoId: track.videoId }));
    this.playlistEntries.set(playlistId, entries);
    return entries;
  }

  /** Adds one entry, refusing a song already in the playlist as the provider does; the reply carries an undo token. */
  private addToPlaylist(req: MutationRequest, accountEpoch: AccountEpoch): MutationResult {
    const refuse = (code: QueryErrorCode, safeMessage: string): MutationResult => ({
      kind: req.kind, outcome: "rejected", error: { code, safeMessage, retriable: false },
    });
    const playlistId = req.playlistId;
    const entries = playlistId ? this.entriesOf(playlistId) : null;
    if (!playlistId || !entries) return refuse("INVALID_PAYLOAD", "Mock playlist does not exist");
    if (!this.tracks10k.some((track) => track.videoId === req.videoId)) return refuse("UNAVAILABLE_TRACK", "Mock track does not exist");
    if (entries.some((entry) => entry.videoId === req.videoId)) {
      return refuse("UNAVAILABLE_TRACK", "Mock playlist already holds this song");
    }
    const entryId = `${playlistId}-added-${++this.nextEntry}`;
    entries.push({ entryId, videoId: req.videoId });
    const undoToken = `undo-mock-${this.nextEntry}`;
    this.undoRecords.set(undoToken, { accountEpoch, playlistId, videoId: req.videoId, entryId, atMs: Date.now() });
    this.emit({ deltaType: "resourceInvalidated", resource_kind: "playlistTracks", resource_id: playlistId, revision: ++this.snapshot.revision });
    return { kind: req.kind, outcome: "confirmed", error: null, undoToken };
  }

  /** Removes exactly the entry the token's addition created, with the core's refusals. */
  private undoAddToPlaylist(req: MutationRequest, accountEpoch: AccountEpoch): MutationResult {
    const refuse = (code: QueryErrorCode, safeMessage: string): MutationResult => ({
      kind: req.kind, outcome: "rejected", error: { code, safeMessage, retriable: false },
    });
    if (!req.undoToken) return refuse("INVALID_PAYLOAD", "undoAddToPlaylist requires undoToken");
    const record = this.undoRecords.get(req.undoToken);
    if (!record || Date.now() - record.atMs >= FakeCoreClient.UNDO_LIFETIME_MS) {
      this.undoRecords.delete(req.undoToken);
      return refuse("STALE_CONTEXT", "this addition can no longer be undone");
    }
    if (record.accountEpoch !== accountEpoch) {
      this.undoRecords.delete(req.undoToken);
      return refuse("STALE_CONTEXT", "the account changed since this song was added");
    }
    if (record.videoId !== req.videoId || (req.playlistId != null && req.playlistId !== record.playlistId)) {
      return refuse("INVALID_PAYLOAD", "the undo token belongs to another addition");
    }
    this.undoRecords.delete(req.undoToken);
    const entries = this.entriesOf(record.playlistId) ?? [];
    const at = entries.findIndex((entry) => entry.entryId === record.entryId);
    if (at < 0) {
      return { ...refuse("STALE_CONTEXT", "the song this addition created is no longer in the playlist"), alreadyRemoved: true };
    }
    entries.splice(at, 1);
    this.emit({ deltaType: "resourceInvalidated", resource_kind: "playlistTracks", resource_id: record.playlistId, revision: ++this.snapshot.revision });
    return { kind: req.kind, outcome: "confirmed", error: null };
  }

  private publishTrackRating(track: Track): void {
    const { player, pending, queueSummary } = this.snapshot;
    const isRatedTrack = ({ videoId }: { videoId: string }) => videoId === track.videoId;
    if (player.currentTrack && isRatedTrack(player.currentTrack)) {
      player.currentTrack = track;
      this.emit({ deltaType: "playerChanged", player, pending, revision: ++this.snapshot.revision });
    }
    const official = queueSummary.officialUpcoming;
    const preview = queueSummary.previewEntries;
    if (preview?.some(isRatedTrack) || official?.tracks?.some(isRatedTrack)) {
      const queue = {
        ...queueSummary,
        previewEntries: preview?.map((entry) => isRatedTrack(entry) ? { ...entry, track } : entry),
        officialUpcoming: official?.tracks
          ? { ...official, tracks: official.tracks.map((item) => isRatedTrack(item) ? track : item) }
          : official,
      };
      this.snapshot.queueSummary = queue;
      this.emit({ deltaType: "queueReplaced", queue_summary: queue, revision: ++this.snapshot.revision });
    }
  }

  /**
   * The cursor follows fixture order, so liking or unliking earlier songs keeps the next page stable.
   * Each row's native entry in "Liked music" (`LM`) is its position among the liked songs, as
   * the core's `playback` gives it; the unsupported scenario leaves it out (single tracks only).
   */
  private likedPage(offset: number, limit: number): Pick<LikedSongsPage, "items" | "nextCursor" | "playback"> {
    const items: Track[] = [];
    const entries: CollectionPlaybackEntry[] = [];
    let resumeAt = offset;
    if (this.scenario === "empty") return { items, nextCursor: null };
    let position = this.tracks10k.slice(0, offset).filter((track) => this.likedVideoIds.has(track.videoId)).length;
    const page = (nextCursor: string | null) => ({
      items,
      nextCursor,
      ...(this.scenario === "unsupported" ? {} : { playback: { playlistId: "LM", entries } }),
    });
    for (let index = offset; index < this.tracks10k.length; index++) {
      const track = this.tracks10k[index]!;
      if (!this.likedVideoIds.has(track.videoId)) continue;
      if (items.length === limit) return page(String(resumeAt));
      items.push(track);
      entries.push({ startIndex: position++, playlistEntryId: null });
      resumeAt = index + 1;
    }
    return page(null);
  }

  public queryCatalog(req: CatalogQueryRequest): CatalogResponsePayload {
    if (this.scenario === "authExpired") {
      return {
        resultType: "error",
        code: "AUTH_EXPIRED",
        retriable: false,
        retryAfterMs: null,
        safeMessage: "Mock auth expired",
        requestId: null,
      };
    }
    const freshness = this.scenario === "offlineCached" ? "stale" : "fresh";
    const limit = Math.max(1, Math.min(req.limit ?? 50, 100));
    const offset = req.cursor ? Math.max(0, Number.parseInt(req.cursor, 10) || 0) : 0;

    switch (req.kind) {
      case "home":
        return {
          resultType: "home",
          personalizedSupported: false,
          fallbackNotice: "Personalized recommendations are not connected (Mock). Showing synthetic recent tracks and fixed playlists.",
          recentTracks: this.scenario === "empty" ? [] : this.tracks10k.slice(0, 6),
          pinnedPlaylists: this.scenario === "empty" ? [] : this.playlists,
          freshness,
        };
      case "libraryPlaylists":
        return {
          resultType: "libraryPlaylists",
          items: this.scenario === "empty" ? [] : this.playlists,
          nextCursor: null,
          freshness,
          fetchedAtMs: Date.now(),
          accountScope: "mock-scope",
          providerVersion: "fake-provider-v1",
        };
      case "playlistTracks": {
        const slice = this.scenario === "empty" ? [] : this.tracks10k.slice(offset, offset + limit);
        const nextOffset = offset + slice.length;
        return {
          resultType: "playlistTracks",
          playlistId: req.playlistId ?? "mock-pl-favorites",
          title: "Synthetic Favorites (Mock 10k)",
          description: "Synthetic 10,000-track fixture playlist",
          author: "Mock Fixture",
          cover: null,
          tracks: {
            items: slice,
            nextCursor: nextOffset < this.tracks10k.length ? String(nextOffset) : null,
            freshness,
            fetchedAtMs: Date.now(),
            accountScope: "mock-scope",
            providerVersion: "fake-provider-v1",
          },
        };
      }
      case "searchSongs": {
        const q = (req.query ?? "").trim().toLowerCase();
        const filtered = q
          ? this.tracks10k.filter((t) => t.title.toLowerCase().includes(q)).slice(0, limit)
          : [];
        return {
          resultType: "searchSongs",
          query: req.query ?? "",
          songs: {
            items: filtered,
            nextCursor: null,
            freshness,
            fetchedAtMs: Date.now(),
            accountScope: "mock-scope",
            providerVersion: "fake-provider-v1",
          },
          albums: [],
          playlists: [],
          artists: [],
        };
      }
      case "albumDetail": {
        const tracks = this.scenario === "empty" ? [] : this.tracks10k.filter((t) => t.album?.id === req.albumId).slice(0, 12);
        const track = tracks[0];
        if (track?.album?.id) {
          return {
            resultType: "albumDetail",
            albumId: track.album.id,
            title: track.album.title,
            artists: track.artists,
            tracks,
          };
        }
        break;
      }
      case "artistDetail": {
        const topSongs = this.scenario === "empty" ? [] : this.tracks10k.filter((t) => t.artists?.some((a) => a.id === req.artistId)).slice(0, 6);
        const track = topSongs[0];
        const artist = track?.artists?.find((a) => a.id === req.artistId);
        if (artist?.id) {
          return {
            resultType: "artistDetail",
            artistId: artist.id,
            name: artist.name,
            albums: track?.album ? [track.album] : [],
            topSongs,
          };
        }
        break;
      }
      case "likedSongs":
        return {
          resultType: "likedSongs",
          ...this.likedPage(offset, limit),
          freshness,
          fetchedAtMs: Date.now(),
          accountScope: "mock-scope",
          providerVersion: "fake-provider-v1",
        };
      case "recentHistory":
        return {
          resultType: "recentHistory",
          items: this.scenario === "empty" ? [] : this.tracks10k.slice(0, 10),
          freshness,
        };
    }
    return {
      resultType: "error",
      code: "UNSUPPORTED_CAPABILITY",
      retriable: false,
      retryAfterMs: null,
      safeMessage: `Mock endpoint ${req.kind} not populated`,
      requestId: null,
    };
  }

  public queryLyrics(req: LyricsQueryRequest): LyricsResult {
    return {
      ...CALIBRATED_LRC_FIXTURE,
      videoId: req.track.videoId,
      userOffsetMs: req.resetUserOffset ? 0 : (req.setUserOffsetMs ?? 0),
    };
  }

  /**
   * The mock account's recent searches, with the core's rules: the account is the signed-in
   * one, the request must carry its epoch, blank or overlong queries are refused, a query that
   * differs only in case or spacing moves to the front, and at most eight are kept.
   * Refusals throw `FakeCoreRequestError`.
   */
  public recentSearches(req: RecentSearchesRequest, accountEpoch: AccountEpoch | null): RecentSearchesSnapshot {
    const refuse = (code: QueryErrorCode, safeMessage: string): never => {
      throw new FakeCoreRequestError({ code, safeMessage, retriable: false });
    };
    if (req.action === "record") {
      if (req.query.trim() === "") refuse("INVALID_PAYLOAD", "recent search query is empty");
      if ([...req.query].length > MAX_RECENT_SEARCH_CHARS) {
        refuse("INVALID_PAYLOAD", `recent search query exceeds ${MAX_RECENT_SEARCH_CHARS} characters`);
      }
    }
    if (accountEpoch === null) refuse("INVALID_PAYLOAD", "ui.recentSearches requires the envelope accountEpoch");
    const { authState } = this.snapshot;
    if (authState.state !== "authenticated" || !authState.accountEpoch || !authState.accountScope) {
      return refuse("AUTH_REQUIRED", "no active account session");
    }
    if (accountEpoch !== authState.accountEpoch) refuse("ACCOUNT_MISMATCH", "request accountEpoch does not match active session");
    const scope = authState.accountScope;
    let items = this.recentSearchesByScope.get(scope) ?? [];
    if (req.action === "record") {
      const query = req.query.trim();
      const folded = foldRecentSearch(query);
      items = [{ query, recordedAtMs: Date.now() }, ...items.filter((item) => foldRecentSearch(item.query) !== folded)]
        .slice(0, MAX_RECENT_SEARCHES);
    } else if (req.action === "clear") {
      items = [];
    }
    this.recentSearchesByScope.set(scope, items);
    return { items: structuredClone(items) };
  }

  public momentSettings(req: MomentDesktopRequest): MomentDesktopSnapshot {
    const accountKey = this.snapshot.authState.accountScope ?? "__signed_out__";
    const principals = ["mock-dj"];
    let previewPrincipal: string | null = null;
    if (req.action === "read") {
      previewPrincipal = req.previewPrincipal ?? null;
    } else if (req.action === "update") {
      const p = req.patch;
      previewPrincipal = p.previewPrincipal ?? null;
      if (typeof p.shareTime === "boolean") this.momentConfig.shareTime = p.shareTime;
      if (typeof p.holidaysEnabled === "boolean") {
        this.momentConfig.holidaysEnabled = p.holidaysEnabled;
      }
      if (typeof p.holidayCountry === "string") {
        const trimmed = p.holidayCountry.trim().toUpperCase();
        this.momentConfig.holidayCountry = trimmed || null;
      }
      if (typeof p.holidayState === "string") {
        const trimmed = p.holidayState.trim();
        this.momentConfig.holidayState = trimmed || null;
      }
      if (typeof p.holidayRegion === "string") {
        const trimmed = p.holidayRegion.trim();
        this.momentConfig.holidayRegion = trimmed || null;
      }
      if (typeof p.includeCulturalObservances === "boolean") {
        this.momentConfig.includeCulturalObservances = p.includeCulturalObservances;
      }
      if (typeof p.sceneCity === "string") {
        const trimmed = p.sceneCity.trim();
        this.momentConfig.sceneCity = trimmed || null;
      }
      if (typeof p.shareListeningIntents === "boolean") {
        this.momentConfig.shareListeningIntents = p.shareListeningIntents;
      }
      if (p.setPrincipalShared) {
        const map = (this.momentConfig.sharedPrincipalsByAccount ??= {});
        const current = new Set(map[accountKey] ?? []);
        if (p.setPrincipalShared.shared) {
          current.add(p.setPrincipalShared.principal);
        } else {
          current.delete(p.setPrincipalShared.principal);
        }
        map[accountKey] = [...current].sort();
      }
      this.momentConfig.configRevision = (this.momentConfig.configRevision ?? 0) + 1;
      this.snapshot.revision += 1;
      this.emit({
        deltaType: "momentChanged",
        revision: this.snapshot.revision,
      });
    }

    const now = Date.now();
    const envSnap: HostEnvironmentSnapshotPayload = {
      configRevision: this.momentConfig.configRevision ?? 0,
      capturedAtMs: now,
      validUntilMs: now + 300_000,
      clockOk: true,
      calendarOk: true,
      timeZone: "America/New_York",
      localDate: "2026-11-23",
      weekday: "monday",
      dayPart: "evening",
      dayEndsAtUtcMs: now + 18_000_000,
      occasions: [],
      countries: [
        { code: "CN", name: "China", states: [] },
        { code: "JP", name: "Japan", states: [] },
        { code: "GB", name: "United Kingdom", states: [] },
        { code: "US", name: "United States", states: [{ code: "NY", name: "New York" }, { code: "WA", name: "Washington" }] },
      ],
    };

    const selected = previewPrincipal && principals.includes(previewPrincipal)
      ? previewPrincipal
      : principals[0]!;
    const sharedList = this.momentConfig.sharedPrincipalsByAccount?.[accountKey] ?? [];
    const allowed = sharedList.includes(selected);
    const revStr = `cfg-${this.momentConfig.configRevision ?? 0}`;
    const previewJson = allowed
      ? {
          revision: this.momentConfig.configRevision ?? 0,
          environment: {
            status:
              this.momentConfig.shareTime ||
              this.momentConfig.holidaysEnabled ||
              this.momentConfig.sceneCity
                ? "shared"
                : "notShared",
            revision: revStr,
            ...(this.momentConfig.shareTime
              ? {
                  time: {
                    localDate: envSnap.localDate,
                    weekday: envSnap.weekday,
                    dayPart: envSnap.dayPart,
                    timeZone: envSnap.timeZone,
                  },
                }
              : {}),
            ...(this.momentConfig.sceneCity
              ? { sceneCity: { label: this.momentConfig.sceneCity, source: "userConfigured" } }
              : {}),
            weatherProvided: false,
          },
          listeningIntents: {
            status: this.momentConfig.shareListeningIntents ? "shared" : "notShared",
            revision: "intent-0",
            items: [],
          },
        }
      : {
          revision: this.momentConfig.configRevision ?? 0,
          environment: { status: "notShared", revision: revStr, weatherProvided: false },
          listeningIntents: { status: "notShared", revision: revStr, items: [] },
        };

    return {
      config: structuredClone(this.momentConfig),
      currentAccountKey: accountKey,
      registeredPrincipals: principals,
      environment: envSnap,
      previewPrincipal: selected,
      previewJson,
    };
  }
}

/** The core's de-duplication key for a recent search: spacing collapsed, lower case. */
function foldRecentSearch(query: string): string {
  return query.trim().split(/\s+/).join(" ").toLowerCase();
}
