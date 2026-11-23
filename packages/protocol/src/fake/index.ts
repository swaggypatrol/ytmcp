import type {
  AppSettings,
  AppSnapshot,
  CatalogQueryRequest,
  CatalogResponsePayload,
  CommandReceipt,
  LyricsQueryRequest,
  LyricsResult,
  PlayerCommandPayload,
  PlaylistSummary,
  QueueEntry,
  StateDelta,
  Track,
} from "../generated/types.js";
import { PROTOCOL_VERSION } from "../validator.js";

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

export class FakeCoreClient {
  private snapshot: AppSnapshot;
  private listeners = new Set<DeltaListener>();
  private seenCommands = new Map<string, CommandReceipt>();
  private scenario: FakeScenario;
  private playlists: PlaylistSummary[];
  private tracks10k: Track[];
  private injectGapNext = false;

  constructor(scenario: FakeScenario = "normal") {
    this.scenario = scenario;
    this.tracks10k = createSyntheticTracks(10_000, "mock");
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
        volume: 0.72,
        muted: false,
        shuffle: false,
        repeat: "off",
        codec: "UNKNOWN (Mock)",
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
        likeMutation: "unsupported",
        playlistAddMutation: "unsupported",
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

  public updateSettings(settings: AppSettings): void {
    this.snapshot.revision += 1;
    this.snapshot.settings = structuredClone(settings);
    this.emit({
      deltaType: "settingsChanged",
      revision: this.snapshot.revision,
      settings: structuredClone(settings),
    });
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
    const limit = Math.min(req.limit ?? 50, 100);
    const offset = req.cursor ? Number.parseInt(req.cursor, 10) || 0 : 0;

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
          items: this.tracks10k.slice(0, 25),
          nextCursor: null,
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
}
