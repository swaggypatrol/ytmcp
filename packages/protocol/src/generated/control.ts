/* eslint-disable */
/**
 * GENERATED FILE — DO NOT EDIT MANUALLY.
 * Regenerate via: just gen-control
 */

/**
 * Application protocol over the local control socket.
 *
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "ControlRequest".
 */
export type ControlRequest =
  | {
      method: "getCapabilities";
      params: {
        area?: CapabilityArea | null;
      };
    }
  | {
      method: "getContext";
      params: ContextInput;
    }
  | {
      method: "getAccount";
    }
  | {
      method: "getPlayer";
      params: {
        include?: PlayerInclude | null;
      };
    }
  | {
      method: "getQueue";
      params: PageInput;
    }
  | {
      method: "search";
      params: SearchInput;
    }
  | {
      method: "searchEntities";
      params: EntitySearchInput;
    }
  | {
      method: "getPlaylist";
      params: PlaylistInput;
    }
  | {
      method: "getShareLink";
      params: ShareInput;
    }
  | {
      method: "recommend";
      params: RecommendationsInput;
    }
  | {
      method: "getOperation";
      params: OperationInput;
    }
  | {
      method: "replayOperation";
      params: OperationReplayInput;
    }
  | {
      method: "getDjPreferences";
      params: DjPreferencesInput;
    }
  | {
      method: "nextDjCue";
      params: DjCueInput;
    }
  | {
      method: "getListenerProfile";
      params: ListenerProfileInput;
    }
  | {
      method: "getSettings";
    }
  | {
      method: "getAlbum";
      params: AlbumInput;
    }
  | {
      method: "getArtist";
      params: ArtistInput;
    }
  | {
      method: "getLibrary";
      params: LibraryInput;
    }
  | {
      method: "getHistory";
      params: HistoryInput;
    }
  | {
      method: "getHome";
      params: LimitInput;
    }
  | {
      method: "getTrack";
      params: {
        target: TrackTarget;
      };
    }
  | {
      method: "getDiscovery";
      params: DiscoveryInput;
    }
  | {
      method: "getMoodCategory";
      params: MoodCategoryInput;
    }
  | {
      method: "getSearchSuggestions";
      params: SearchSuggestionsInput;
    }
  | {
      method: "getExtendedMedia";
      params: ExtendedMediaInput;
    }
  | {
      method: "getLyrics";
      params: LyricsInput;
    }
  | {
      method: "submit";
      params: WriteInput;
    }
  | {
      method: "subscribe";
      params: ContextInput;
    };
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "CapabilityArea".
 */
export type CapabilityArea =
  "catalog" | "playback" | "queue" | "playlists" | "library" | "lyrics" | "dj" | "extensions" | "account";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "PlayerInclude".
 */
export type PlayerInclude = "state" | "audio" | "both";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "EntitySearchKind".
 */
export type EntitySearchKind = "album" | "artist" | "playlist";
/**
 * Opaque YouTube Music playlist identifier.
 *
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "PlaylistId".
 */
export type PlaylistId = string;
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "ShareTarget".
 */
export type ShareTarget =
  | {
      kind: "track";
      target: TrackTarget;
    }
  | {
      kind: "playlist";
      playlistId: PlaylistId;
    }
  | {
      albumId: AlbumId;
      kind: "album";
    }
  | {
      artistId: ArtistId;
      kind: "artist";
    };
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "TrackTarget".
 */
export type TrackTarget =
  | {
      kind: "video";
      videoId: VideoId;
    }
  | {
      contextToken: string;
      kind: "context";
    };
/**
 * Opaque YouTube Music video identifier.
 *
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "VideoId".
 */
export type VideoId = string;
/**
 * Opaque YouTube Music album identifier.
 *
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "AlbumId".
 */
export type AlbumId = string;
/**
 * Opaque YouTube Music artist identifier.
 *
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "ArtistId".
 */
export type ArtistId = string;
/**
 * Unique identifier for one authenticated account session epoch.
 *
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "AccountEpoch".
 */
export type AccountEpoch = string;
/**
 * Unique identifier for one authorized page bridge handshake.
 *
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "BridgeEpoch".
 */
export type BridgeEpoch = string;
/**
 * How often the heartbeat lets the agent check in with the listener.
 *
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "DjChattiness".
 */
export type DjChattiness = "off" | "quiet" | "normal" | "chatty";
/**
 * Unique identifier for one Rust core process instance.
 *
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "CoreEpoch".
 */
export type CoreEpoch = string;
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "LibraryKind".
 */
export type LibraryKind =
  "playlists" | "liked" | "songs" | "albums" | "artists" | "subscriptions" | "uploads" | "podcasts";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "HistorySource".
 */
export type HistorySource = "local" | "youtube" | "combined";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "DiscoveryKind".
 */
export type DiscoveryKind = "home" | "explore" | "moodsAndGenres" | "charts" | "newReleases";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "ExtendedMediaTarget".
 */
export type ExtendedMediaTarget =
  | {
      kind: "uploadSongs";
    }
  | {
      kind: "uploadAlbums";
    }
  | {
      kind: "uploadArtists";
    }
  | {
      browseId: BrowseId;
      kind: "uploadAlbum";
    }
  | {
      browseId: BrowseId;
      kind: "uploadArtist";
    }
  | {
      kind: "libraryPodcasts";
    }
  | {
      browseId: BrowseId;
      kind: "podcast";
    }
  | {
      browseId: BrowseId;
      kind: "podcastChannel";
    }
  | {
      browseId: BrowseId;
      kind: "podcastChannelEpisodes";
    }
  | {
      browseId: BrowseId;
      kind: "podcastEpisode";
    };
/**
 * Opaque YouTube Music browse endpoint identifier.
 *
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "BrowseId".
 */
export type BrowseId = string;
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "LyricsSourcePreference".
 */
export type LyricsSourcePreference = "auto" | "lrclib" | "youTubeMusic";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "WriteAction".
 */
export type WriteAction =
  | {
      kind: "play";
      target: PlayContextTarget;
    }
  | {
      kind: "resume";
    }
  | {
      kind: "pause";
    }
  | {
      kind: "stop";
    }
  | {
      kind: "next";
    }
  | {
      kind: "previous";
    }
  | {
      contextToken: string;
      kind: "seek";
      positionMs: number;
    }
  | {
      kind: "volume";
      volume: number;
    }
  | {
      kind: "mute";
      muted: boolean;
    }
  | {
      enabled: boolean;
      kind: "shuffle";
    }
  | {
      kind: "repeat";
      mode: RepeatMode;
    }
  | {
      enabled: boolean;
      kind: "autoplay";
    }
  | {
      filterId: string;
      kind: "setQueueFilter";
    }
  | {
      contextToken: string;
      kind: "enqueue";
      mode: QueueMode;
      /**
       * @minItems 1
       * @maxItems 50
       */
      videoIds: [VideoId, ...VideoId[]];
    }
  | {
      contextToken: string;
      entryId: QueueEntryId;
      kind: "selectQueueEntry";
    }
  | {
      contextToken: string;
      entryId: QueueEntryId;
      kind: "removeQueueEntry";
    }
  | {
      beforeEntryId?: QueueEntryId | null;
      contextToken: string;
      entryId: QueueEntryId;
      kind: "moveQueueEntry";
    }
  | {
      contextToken: string;
      kind: "clearUpcoming";
    }
  | {
      contextToken: string;
      kind: "replaceUpcoming";
      /**
       * @minItems 1
       * @maxItems 50
       */
      videoIds: [VideoId, ...VideoId[]];
    }
  | {
      contextToken: string;
      kind: "interruptWith";
      resumePolicy: InterruptResumePolicy;
      /**
       * @minItems 1
       * @maxItems 50
       */
      videoIds: [VideoId, ...VideoId[]];
    }
  | {
      kind: "rate";
      rating: Rating;
      target: TrackTarget;
    }
  | {
      artistId: ArtistId;
      kind: "setArtistSubscription";
      subscribed: boolean;
    }
  | {
      kind: "setLibraryMembership";
      saved: boolean;
      target: LibraryMembershipTarget;
    }
  | {
      edit: LocalHistoryEdit;
      kind: "editLocalHistory";
    }
  | {
      description?: string | null;
      kind: "createPrivatePlaylist";
      title: string;
    }
  | {
      createdByOperationId?: string | null;
      kind: "addPlaylistEntries";
      playlistId: PlaylistId;
      /**
       * @minItems 1
       * @maxItems 50
       */
      videoIds: [VideoId, ...VideoId[]];
    }
  | {
      description?: string | null;
      kind: "updatePlaylist";
      playlistId: PlaylistId;
      title?: string | null;
    }
  | {
      kind: "setPlaylistPrivacy";
      playlistId: PlaylistId;
      privacy: PlaylistPrivacy;
    }
  | {
      /**
       * @minItems 1
       * @maxItems 50
       */
      entryIds: [string, ...string[]];
      kind: "removePlaylistEntries";
      playlistId: PlaylistId;
    }
  | {
      beforeEntryId?: string | null;
      entryId: string;
      kind: "movePlaylistEntry";
      playlistId: PlaylistId;
    }
  | {
      kind: "deletePlaylist";
      playlistId: PlaylistId;
    }
  | {
      expectedSettingsRevision: number;
      kind: "updateSettings";
      patch: SettingsPatch;
    }
  | {
      kind: "setLyricsOffset";
      offset: LyricsOffsetAction;
      target: TrackTarget;
    }
  | {
      change: DjFeedbackChange;
      kind: "setDjFeedback";
    }
  | {
      change: ListeningIntentChange;
      kind: "setListeningIntent";
    }
  | {
      goal: AccountHandoffGoal;
      kind: "accountHandoff";
    }
  | {
      kind: "cancel";
      targetOperationId: string;
    }
  | {
      action: ResumeAction;
      expectedOperationRevision: number;
      kind: "resumeOperation";
      targetOperationId: string;
    };
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "PlayContextTarget".
 */
export type PlayContextTarget =
  | {
      kind: "video";
      videoId: VideoId;
    }
  | {
      contextToken: string;
      kind: "context";
    }
  | {
      albumId: AlbumId;
      kind: "album";
      startIndex?: number | null;
    }
  | {
      kind: "playlist";
      playlistId: PlaylistId;
      startEntryId?: string | null;
      startIndex?: number | null;
    }
  | {
      contextToken: string;
      entryId: QueueEntryId;
      kind: "queueEntry";
    };
/**
 * Occurrence-scoped queue entry identifier within a bridge epoch.
 *
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "QueueEntryId".
 */
export type QueueEntryId = string;
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "RepeatMode".
 */
export type RepeatMode = "off" | "all" | "one";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "QueueMode".
 */
export type QueueMode = "next" | "last";
/**
 * What follows an immediate interruption once the inserted tracks have played.
 *
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "InterruptResumePolicy".
 */
export type InterruptResumePolicy = "resumeInterrupted" | "continueOriginalQueue" | "stayOnInsertedContext";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "Rating".
 */
export type Rating = "liked" | "disliked" | "neutral" | "unknown";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "LibraryMembershipTarget".
 */
export type LibraryMembershipTarget =
  | {
      id: VideoId;
      kind: "song";
    }
  | {
      id: PlaylistId;
      kind: "playlist";
    }
  | {
      id: AlbumId;
      kind: "album";
    };
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "LocalHistoryEdit".
 */
export type LocalHistoryEdit =
  | {
      /**
       * @minItems 1
       * @maxItems 50
       */
      entryIds: [string, ...string[]];
      kind: "remove";
    }
  | {
      kind: "clear";
      range?: LocalHistoryRange | null;
    };
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "PlaylistPrivacy".
 */
export type PlaylistPrivacy = "private" | "unlisted" | "public";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "ThemeMode".
 */
export type ThemeMode = "dark" | "light" | "system";
/**
 * The language the UI is shown in. Values are the UI's BCP 47 locale tags.
 *
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "UiLanguage".
 */
export type UiLanguage = "en" | "zh-CN";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "LyricsOffsetAction".
 */
export type LyricsOffsetAction =
  | {
      action: "set";
      offsetMs: number;
    }
  | {
      action: "reset";
    };
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "DjFeedbackChange".
 */
export type DjFeedbackChange =
  | {
      action: "record";
      note?: string | null;
      stance: DjPreferenceStance;
      target: DjFeedbackTarget;
    }
  | {
      action: "clear";
      preferenceId: string;
    };
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "DjPreferenceStance".
 */
export type DjPreferenceStance = "prefer" | "avoid";
/**
 * What a feedback entry is about.
 *
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "DjFeedbackTarget".
 */
export type DjFeedbackTarget =
  | {
      kind: "video";
      videoId: VideoId;
    }
  | {
      artistId: ArtistId;
      kind: "artist";
    };
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "ListeningIntentChange".
 */
export type ListeningIntentChange =
  | {
      action: "set";
      basis: ReportedIntentBasis;
      duration: ListeningIntentDuration;
      expectedMomentRevision?: number | null;
      instruction: ListeningIntentInstruction;
    }
  | {
      action: "clear";
      basis: ReportedIntentBasis;
      expectedMomentRevision?: number | null;
      intentId?: string | null;
      intentKey: string;
    };
/**
 * Provenance accepted from external MCP clients (owners set `ownerInput` strictly from desktop UI).
 *
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "ReportedIntentBasis".
 */
export type ReportedIntentBasis = "userRequest" | "inference";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "ListeningIntentDuration".
 */
export type ListeningIntentDuration =
  | {
      kind: "today";
    }
  | {
      kind: "session";
    }
  | {
      expiresAtMs: number;
      kind: "until";
    };
/**
 * One session-scoped listening intent dimension. Updating a dimension replaces only that dimension.
 *
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "ListeningIntentInstruction".
 */
export type ListeningIntentInstruction =
  | {
      dimension: "vocals";
      mode: VocalsMode;
    }
  | {
      dimension: "energy";
      level: EnergyLevel;
    }
  | {
      dimension: "activity";
      kind: ActivityKind;
    }
  | {
      brief: string;
      dimension: "musicBrief";
    }
  | {
      dimension: "dismissTheme";
      themeId: string;
    };
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "VocalsMode".
 */
export type VocalsMode = "any" | "preferInstrumental";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "EnergyLevel".
 */
export type EnergyLevel = "gentle" | "steady" | "driving";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "ActivityKind".
 */
export type ActivityKind = "gaming" | "focus" | "relax" | "social";
/**
 * The account change a client asks the user to make in the player.
 *
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "AccountHandoffGoal".
 */
export type AccountHandoffGoal = "signIn" | "reauthenticate" | "switchAccount";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "ResumeAction".
 */
export type ResumeAction = "resumeUnsent" | "reconcile";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "ServerFrame".
 */
export type ServerFrame =
  | {
      requestId: number;
      response: ControlResponse;
      type: "response";
    }
  | {
      event: ControlEvent;
      type: "event";
    };
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "ControlResponse".
 */
export type ControlResponse =
  | {
      result: "capabilities";
      value: Capability[];
    }
  | {
      result: "context";
      value: ContextSnapshot;
    }
  | {
      result: "account";
      value: AccountState;
    }
  | {
      result: "player";
      value: PlayerView;
    }
  | {
      result: "queue";
      value: QueueProjection;
    }
  | {
      result: "search";
      value: Page;
    }
  | {
      result: "searchEntities";
      value: Page2;
    }
  | {
      result: "searchSuggestions";
      value: Page3;
    }
  | {
      result: "playlist";
      value: Playlist;
    }
  | {
      result: "shareLink";
      value: ShareLink;
    }
  | {
      result: "recommendations";
      value: RecommendationPage;
    }
  | {
      result: "album";
      value: AlbumPage;
    }
  | {
      result: "artist";
      value: ArtistOverview;
    }
  | {
      result: "library";
      value: LibraryPage;
    }
  | {
      result: "history";
      value: Page;
    }
  | {
      result: "historyEntries";
      value: Page9;
    }
  | {
      result: "localHistory";
      value: LocalHistoryView;
    }
  | {
      result: "combinedHistory";
      value: CombinedHistory;
    }
  | {
      result: "home";
      value: HomeView;
    }
  | {
      result: "discovery";
      value: DiscoveryPage;
    }
  | {
      result: "extendedMedia";
      value: ExtendedMediaPage;
    }
  | {
      result: "track";
      value: TrackView;
    }
  | {
      result: "settings";
      value: SettingsSnapshot;
    }
  | {
      result: "lyrics";
      value: LyricsPage;
    }
  | {
      result: "djPreferences";
      value: Page12;
    }
  | {
      result: "djCue";
      value: DjCueReply;
    }
  | {
      result: "listenerProfile";
      value: ListenerProfile;
    }
  | {
      result: "operation";
      value: OperationReceipt;
    }
  | {
      result: "error";
      value: ControlError;
    };
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "AuthState".
 */
export type AuthState = "unknown" | "signedOut" | "signingIn" | "authenticated" | "expired" | "blocked";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "Freshness".
 */
export type Freshness = "fresh" | "stale";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "ListenerActionKind".
 */
export type ListenerActionKind =
  | (
      | "play"
      | "pause"
      | "stop"
      | "seek"
      | "volume"
      | "mute"
      | "shuffle"
      | "repeat"
      | "autoplay"
      | "addToQueue"
      | "removeFromQueue"
      | "reorderQueue"
      | "clearUpcoming"
      | "addToPlaylist"
    )
  | "next"
  | "previous"
  | "playTrack"
  | "playCollection"
  | "pickFromQueue"
  | "upNextFilter"
  | "rate"
  | "undoAddToPlaylist";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "MomentOccasionKind".
 */
export type MomentOccasionKind = "public" | "observance";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "MomentOccasionPhase".
 */
export type MomentOccasionPhase = "eve" | "day" | "observed";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "SceneCitySource".
 */
export type SceneCitySource = "userConfigured";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "MomentSectionStatus".
 */
export type MomentSectionStatus = "shared" | "notShared" | "unavailable";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "MomentDayPart".
 */
export type MomentDayPart = "earlyMorning" | "morning" | "afternoon" | "evening" | "lateNight";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "ListeningIntentBasis".
 */
export type ListeningIntentBasis = "ownerInput" | "userRequestReportedByAssistant" | "assistantInference";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "ListeningIntentScope".
 */
export type ListeningIntentScope = "today" | "session" | "until";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "PlayerOp".
 */
export type PlayerOp =
  | (
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
      | "moveQueueEntry"
    )
  | "setQueueFilter";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "PlayerConnectionState".
 */
export type PlayerConnectionState = "starting" | "ready" | "stale" | "recovering" | "disconnected" | "incompatible";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "PlaybackStatus".
 */
export type PlaybackStatus = "idle" | "playing" | "paused" | "buffering" | "ended" | "error";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "TrackAvailability".
 */
export type TrackAvailability = "available" | "unavailable" | "regionBlocked" | "ageRestricted" | "unknown";
/**
 * Unique playback instance identifier (distinguishes repeated plays).
 *
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "TrackInstanceId".
 */
export type TrackInstanceId = string;
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "QueueCompleteness".
 */
export type QueueCompleteness = "unknown" | "partial" | "complete";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "OfficialUpcomingStatus".
 */
export type OfficialUpcomingStatus = "observed" | "loading" | "off" | "radio" | "none";
/**
 * Whether what follows the current occurrence is known.
 *
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "UpcomingStatus".
 */
export type UpcomingStatus = "known" | "unknown";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "SearchEntity".
 */
export type SearchEntity =
  | {
      albumId: AlbumId;
      artist?: string | null;
      kind: "album";
      title: string;
      year?: number | null;
    }
  | {
      artistId: ArtistId;
      kind: "artist";
      name: string;
      subscribers?: string | null;
    }
  | {
      byline?: string | null;
      kind: "playlist";
      playlistId: PlaylistId;
      title: string;
      trackCount?: number | null;
    };
/**
 * Access evidence for an individual recipient remains unknown.
 *
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "ShareAccess".
 */
export type ShareAccess = "unknown" | "private" | "unlisted";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "LibraryPage".
 */
export type LibraryPage =
  | {
      kind: "playlists";
      page: Page5;
    }
  | {
      kind: "liked";
      page: Page;
    }
  | {
      kind: "songs";
      page: Page;
    }
  | {
      kind: "albums";
      page: Page6;
    }
  | {
      kind: "artists";
      page: Page7;
    }
  | {
      kind: "subscriptions";
      page: Page7;
    }
  | {
      kind: "uploads";
      page: ExtendedMediaPage;
    }
  | {
      kind: "podcasts";
      page: ExtendedMediaPage;
    };
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "ExtendedMediaKind".
 */
export type ExtendedMediaKind =
  "uploadedSong" | "uploadedAlbum" | "uploadedArtist" | "podcast" | "podcastChannel" | "podcastEpisode";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "HistoryMedia".
 */
export type HistoryMedia =
  | {
      kind: "song";
      track: Track;
    }
  | {
      episodeId: BrowseId;
      kind: "podcastEpisode";
      title: string;
    };
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "ErrorCode".
 */
export type ErrorCode =
  | "AUTH_REQUIRED"
  | "PERMISSION_DENIED"
  | "NOT_EDITABLE"
  | "CAPABILITY_UNAVAILABLE"
  | "STALE_CONTEXT"
  | "CONTEXT_EXPIRED"
  | "CONTEXT_CHANGED"
  | "CONFLICT"
  | "PARTIAL_QUEUE"
  | "RATE_LIMITED"
  | "IDEMPOTENCY_CONFLICT"
  | "RESULT_UNKNOWN"
  | "INVALID_INPUT"
  | "INCOMPATIBLE_VERSION"
  | "BUSY"
  | "UNAVAILABLE"
  | "STORAGE_ERROR"
  | "LIMIT_REACHED";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "RetryDisposition".
 */
export type RetryDisposition =
  "safe_read_retry" | "query_operation" | "reconcile_first" | "new_plan_required" | "reauth_required" | "not_retryable";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "DiscoveryTarget".
 */
export type DiscoveryTarget =
  | {
      kind: "track";
      videoId: VideoId;
    }
  | {
      kind: "playlist";
      playlistId: PlaylistId;
    }
  | {
      albumId: AlbumId;
      kind: "album";
    }
  | {
      artistId: ArtistId;
      kind: "artist";
    }
  | {
      categoryId: string;
      kind: "moodCategory";
    };
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "LyricsKind".
 */
export type LyricsKind = "synced" | "plain" | "instrumental" | "notFound" | "unavailable" | "disabled";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "DjPreferenceSubject".
 */
export type DjPreferenceSubject =
  | {
      kind: "video";
      title?: string | null;
      videoId: VideoId;
    }
  | {
      artistId: ArtistId;
      kind: "artist";
      name?: string | null;
    };
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "DjCueKind".
 */
export type DjCueKind = "trackEnding" | "checkIn" | "listenerSignal";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "DjInteraction".
 */
export type DjInteraction = "playerControl" | "spoke";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "DjCueReason".
 */
export type DjCueReason =
  "listenerQuiet" | "longTrack" | "routine" | "littleQueued" | "skipStreak" | "manualPick" | "replay" | "filterChanged";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "DjTrackOutcome".
 */
export type DjTrackOutcome = ("finished" | "skippedByListener" | "skippedByAgent") | "skipped";
/**
 * A writing system, as told from the letters of a title and its artist names.
 *
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "Script".
 */
export type Script =
  ("latin" | "hangul" | "cyrillic" | "arabic" | "devanagari" | "thai" | "hebrew" | "greek") | "han" | "kana" | "other";
/**
 * Redacted local scope identifier isolating accounts in cache/state.
 *
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "AccountScope".
 */
export type AccountScope = string;
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "ItemPhase".
 */
export type ItemPhase = "notStarted" | "submitted" | "acknowledged" | "verified" | "unknown" | "rejected" | "cancelled";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "OperationPhase".
 */
export type OperationPhase =
  "accepted" | "submitted" | "acknowledged" | "verified" | "partial" | "unknown" | "rejected" | "cancelled";
/**
 * One entry a removal took (or, while unsettled, may have taken) from the account or device.
 *
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "RemovedItem".
 */
export type RemovedItem =
  | {
      entryId: string;
      kind: "playlistEntry";
      playlistId: PlaylistId;
      position: number;
      videoId: VideoId;
    }
  | {
      entryId: string;
      kind: "localHistoryEntry";
      recordedAtMs: number;
      videoId: VideoId;
    };
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "OperationResource".
 */
export type OperationResource =
  | {
      kind: "player";
      op: PlayerOp;
    }
  | {
      kind: "queue";
    }
  | {
      id: PlaylistId;
      kind: "playlist";
    }
  | {
      id: VideoId;
      kind: "track";
    }
  | {
      id: ArtistId;
      kind: "artist";
      subscribed?: boolean | null;
    }
  | {
      albumPlaylistId?: PlaylistId | null;
      kind: "libraryMembership";
      saved?: boolean | null;
      target: LibraryMembershipTarget;
    }
  | {
      kind: "localHistory";
      removedCount?: number | null;
    }
  | {
      kind: "settings";
    }
  | {
      kind: "lyricsOffset";
      offsetMs: number;
      videoId: VideoId;
    }
  | {
      id: string;
      kind: "operation";
    }
  | {
      cleared: boolean;
      id: string;
      kind: "djPreference";
    }
  | {
      cleared: boolean;
      expiresAtMs?: number | null;
      intentId: string;
      intentKey: string;
      kind: "listeningIntent";
      momentRevision: number;
    }
  | {
      goal: AccountHandoffGoal;
      kind: "accountHandoff";
      state: AccountHandoffState;
    }
  | {
      interruptedEntryId: QueueEntryId;
      interruptedVideoId: VideoId;
      kind: "interruption";
      policy: InterruptResumePolicy;
      resume: InterruptResumeState;
      /**
       * The queued replay occurrence of the interrupted track, once it is verified.
       */
      resumeEntryId?: QueueEntryId | null;
      /**
       * Playback position captured right before the switch, when the policy resumes it.
       */
      resumePositionMs?: number | null;
    };
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "AccountHandoffState".
 */
export type AccountHandoffState = "notNeeded" | "awaitingUser" | "completed";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "InterruptResumeState".
 */
export type InterruptResumeState = "notApplicable" | "pending" | "resumed" | "abandoned";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "VerificationLevel".
 */
export type VerificationLevel = "observed" | "readBack" | "persisted";
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "ControlEvent".
 */
export type ControlEvent =
  | {
      cursor: EventCursor;
      event: "trackChanged";
      videoId?: VideoId | null;
    }
  | {
      cursor: EventCursor;
      event: "queueLow";
      queueRevision: number;
    }
  | {
      cursor: EventCursor;
      event: "queueChanged";
      queueRevision: number;
    }
  | {
      account: AccountState;
      cursor: EventCursor;
      event: "authChanged";
    }
  | {
      cursor: EventCursor;
      event: "manualTakeover";
      generation: number;
    }
  | {
      cursor: EventCursor;
      event: "resourceInvalidated";
      id?: string | null;
      kind: string;
    }
  | {
      cursor: EventCursor;
      event: "operationChanged";
      operationId: string;
    }
  | {
      cursor: EventCursor;
      event: "eventGap";
    };

export interface ControlV1 {
  request: RequestFrame;
  server: ServerFrame;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "RequestFrame".
 */
export interface RequestFrame {
  controlVersion: number;
  request: ControlRequest;
  requestId: number;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "ContextInput".
 */
export interface ContextInput {
  previewLimit?: number | null;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "PageInput".
 */
export interface PageInput {
  cursor?: string | null;
  limit?: number | null;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "SearchInput".
 */
export interface SearchInput {
  page?: PageInput | null;
  query: string;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "EntitySearchInput".
 */
export interface EntitySearchInput {
  kind: EntitySearchKind;
  page?: PageInput | null;
  query: string;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "PlaylistInput".
 */
export interface PlaylistInput {
  page?: PageInput | null;
  playlistId: PlaylistId;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "ShareInput".
 */
export interface ShareInput {
  target: ShareTarget;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "RecommendationsInput".
 */
export interface RecommendationsInput {
  page?: PageInput | null;
  seed: TrackTarget;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "OperationInput".
 */
export interface OperationInput {
  operationId: string;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "OperationReplayInput".
 */
export interface OperationReplayInput {
  actionHash: string;
  explicitPreconditions?: Preconditions | null;
  operationId: string;
  tokenWasMissing: boolean;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "Preconditions".
 */
export interface Preconditions {
  expectedAccountEpoch?: AccountEpoch | null;
  expectedBridgeEpoch?: BridgeEpoch | null;
  expectedEntityRevision?: string | null;
  expectedQueueRevision?: number | null;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "DjPreferencesInput".
 */
export interface DjPreferencesInput {
  page?: PageInput | null;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "DjCueInput".
 */
export interface DjCueInput {
  chattiness?: DjChattiness | null;
  /**
   * The last prompt this client actually received. A prompt handed out after it (for
   * example to a wait the client cancelled before the reply arrived) is offered again, so a
   * cancelled wait loses nothing; the client skips repeats by `(coreEpoch, seq)`. Ignored
   * when it names another core epoch.
   */
  lastReceived?: DjCueAck | null;
  /**
   * The agent said something to the listener since its previous call.
   */
  spoke?: boolean;
  /**
   * The listener talked to the agent since its previous call.
   */
  userSpoke?: boolean;
}
/**
 * Names one delivered prompt.
 *
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "DjCueAck".
 */
export interface DjCueAck {
  coreEpoch: CoreEpoch;
  seq: number;
}
/**
 * Input of a listener profile read.
 *
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "ListenerProfileInput".
 */
export interface ListenerProfileInput {
  /**
   * Read the library again instead of using the copy kept for this session.
   */
  refresh?: boolean;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "AlbumInput".
 */
export interface AlbumInput {
  albumId: AlbumId;
  includeMembership?: boolean | null;
  page?: PageInput | null;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "ArtistInput".
 */
export interface ArtistInput {
  artistId: ArtistId;
  limit?: number | null;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "LibraryInput".
 */
export interface LibraryInput {
  kind: LibraryKind;
  page?: PageInput | null;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "HistoryInput".
 */
export interface HistoryInput {
  limit?: number | null;
  localCursor?: string | null;
  page?: PageInput | null;
  range?: LocalHistoryRange | null;
  source?: HistorySource | null;
  youtubeCursor?: string | null;
}
/**
 * The local recording timestamp is known. YouTube history may only have a period label.
 *
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "LocalHistoryRange".
 */
export interface LocalHistoryRange {
  fromMs?: number | null;
  untilMs?: number | null;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "LimitInput".
 */
export interface LimitInput {
  limit?: number | null;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "DiscoveryInput".
 */
export interface DiscoveryInput {
  kind: DiscoveryKind;
  page?: PageInput | null;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "MoodCategoryInput".
 */
export interface MoodCategoryInput {
  categoryId: string;
  page?: PageInput | null;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "SearchSuggestionsInput".
 */
export interface SearchSuggestionsInput {
  limit?: number | null;
  query: string;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "ExtendedMediaInput".
 */
export interface ExtendedMediaInput {
  page?: PageInput | null;
  target: ExtendedMediaTarget;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "LyricsInput".
 */
export interface LyricsInput {
  page?: LyricsPageInput | null;
  source?: LyricsSourcePreference | null;
  target: TrackTarget;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "LyricsPageInput".
 */
export interface LyricsPageInput {
  cursor?: string | null;
  limit?: number | null;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "WriteInput".
 */
export interface WriteInput {
  action: WriteAction;
  operationId: string;
  preconditions: Preconditions;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "SettingsPatch".
 */
export interface SettingsPatch {
  autoplayOnLaunch?: boolean | null;
  closeToTray?: boolean | null;
  lrclibEnabled?: boolean | null;
  reduceMotion?: boolean | null;
  reduceTransparency?: boolean | null;
  theme?: ThemeMode | null;
  trackNotifications?: boolean | null;
  uiLanguage?: UiLanguage | null;
  uiScalePercent?: number | null;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "Capability".
 */
export interface Capability {
  area: CapabilityArea;
  availableNow: boolean;
  id: string;
  implemented: boolean;
  reason?: string | null;
  verifiedAtMs?: number | null;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "ContextSnapshot".
 */
export interface ContextSnapshot {
  account: AccountState;
  bridgeEpoch?: BridgeEpoch | null;
  capabilities: Capability[];
  capturedAtMs: number;
  contextToken: string;
  cursor: EventCursor;
  freshness: Freshness;
  /**
   * What the listener did by hand in the last ten minutes, newest first, at most
   * [`crate::MAX_LISTENER_ACTIONS`]. Commands sent by agents are not listed.
   */
  listenerActions?: ListenerAction[];
  /**
   * Owner-configured Moment DJ context projected for this client (`None` only in legacy fixtures).
   */
  moment?: MomentView | null;
  player: PlayerState;
  queue: QueueProjection;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "AccountState".
 */
export interface AccountState {
  accountEpoch?: AccountEpoch | null;
  playerCatalogMatched: boolean;
  state: AuthState;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "EventCursor".
 */
export interface EventCursor {
  coreEpoch: CoreEpoch;
  revision: number;
}
/**
 * Something the listener did by hand: in the player window, from the tray, with media keys or
 * through MPRIS. Commands sent by any agent are never listed.
 *
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "ListenerAction".
 */
export interface ListenerAction {
  action: ListenerActionKind;
  agoMs: number;
  /**
   * What was playing when they acted, and where in it.
   */
  during?: DjTrack | null;
  positionMs?: number | null;
  /**
   * Where a seek landed. A drag is one action.
   */
  seekToMs?: number | null;
  /**
   * The track the action was about (picked, queued, rated): the playing one when it was about
   * that. Absent for actions with no track of their own, and while the player cannot name the
   * track yet (for example one just picked from search).
   */
  subject?: DjTrack | null;
  /**
   * The new setting in plain words: `on` or `off`, a repeat mode, a rating (`liked`,
   * `disliked`, `neutral`), or the label of the chosen Up Next filter.
   */
  value?: string | null;
  /**
   * Volume before and after, 0 to 100. A drag is one action from its first to its last value.
   */
  volumeFrom?: number | null;
  volumeTo?: number | null;
}
/**
 * The few track facts a DJ prompt needs; the music tools return the full track.
 *
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "DjTrack".
 */
export interface DjTrack {
  /**
   * Album title.
   */
  album?: string | null;
  /**
   * Artist names.
   */
  artists: string[];
  durationMs?: number | null;
  title: string;
  videoId: VideoId;
}
/**
 * Authoritative Moment DJ context projection delivered to one authorized MCP client.
 *
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "MomentView".
 */
export interface MomentView {
  environment: EnvironmentView;
  listeningIntents: ListeningIntentsView;
  revision: number;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "EnvironmentView".
 */
export interface EnvironmentView {
  /**
   * The instant `time` describes (Unix ms), so an assistant can turn "tonight" or "for two
   * hours" into an absolute `expiresAtMs`. Present only with `time`.
   */
  capturedAtMs?: number | null;
  occasions?: MomentOccasion[];
  revision: string;
  sceneCity?: SceneCity | null;
  status: MomentSectionStatus;
  time?: MomentTime | null;
  /**
   * Always false: the player never fetches weather; an assistant may look it up for `sceneCity`.
   */
  weatherProvided?: boolean;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "MomentOccasion".
 */
export interface MomentOccasion {
  actualDate?: string | null;
  id: string;
  kind: MomentOccasionKind;
  label: string;
  localDate: string;
  phase: MomentOccasionPhase;
  providerVersion: string;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "SceneCity".
 */
export interface SceneCity {
  label: string;
  source: SceneCitySource;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "MomentTime".
 */
export interface MomentTime {
  dayPart: MomentDayPart;
  /**
   * Local calendar date (`YYYY-MM-DD`) in `timeZone`.
   */
  localDate: string;
  /**
   * IANA time zone identifier from the local device clock.
   */
  timeZone: string;
  /**
   * Lowercase weekday name (`monday`..`sunday`).
   */
  weekday: string;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "ListeningIntentsView".
 */
export interface ListeningIntentsView {
  items: ListeningIntentView[];
  revision: string;
  status: MomentSectionStatus;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "ListeningIntentView".
 */
export interface ListeningIntentView {
  basis: ListeningIntentBasis;
  createdAtMs: number;
  expiresAtMs: number;
  instruction: ListeningIntentInstruction;
  intentId: string;
  intentKey: string;
  scope: ListeningIntentScope;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "PlayerState".
 */
export interface PlayerState {
  activeOp?: PlayerOp | null;
  autoplay?: boolean | null;
  connection: PlayerConnectionState;
  muted: boolean;
  pendingCommands: number;
  playback: PlaybackStatus;
  playbackContext?: PlaybackContext | null;
  /**
   * The player is still finding out what it can play (shortly after launch: the page has not
   * reported playback yet, or the saved session is being restored). No track then does not
   * mean nothing will play: a play sent now is held until this ends, not refused. Absent
   * means false.
   */
  preparing?: boolean;
  repeat?: RepeatMode | null;
  shuffle?: boolean | null;
  stoppedConfirmed: boolean;
  timeAnchor: PlaybackTimeAnchor;
  track?: Track | null;
  trackInstanceId?: TrackInstanceId | null;
  volume: number;
}
/**
 * Playlist identity and index read from the official page's selected native queue item.
 *
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "PlaybackContext".
 */
export interface PlaybackContext {
  playlistEntryId?: string | null;
  playlistId: PlaylistId;
  startIndex: number;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
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
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "Track".
 */
export interface Track {
  album?: Album | null;
  artists: Artist[];
  availability: TrackAvailability;
  durationMs?: number | null;
  rating: Rating;
  title: string;
  versionTag?: string | null;
  videoId: VideoId;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "Album".
 */
export interface Album {
  id?: AlbumId | null;
  title: string;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "Artist".
 */
export interface Artist {
  id?: ArtistId | null;
  name: string;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "QueueProjection".
 */
export interface QueueProjection {
  completeness: QueueCompleteness;
  currentEntryId?: QueueEntryId | null;
  entries: QueueOccurrence[];
  /**
   * The Up Next filters the page offers now (label and whether selected). Empty when none
   * are offered or they could not be read.
   */
  filters?: QueueFilterView[];
  hasMore?: boolean | null;
  infinite?: boolean | null;
  loadedEndExclusive?: number | null;
  loadedStart?: number | null;
  nextCursor?: string | null;
  /**
   * YouTube Music's own continuation after the explicit list (autoplay suggestions, or a
   * radio that refills itself). Read-only: these tracks are not occurrences and cannot be
   * selected or edited. Absent when the page state was not readable.
   */
  officialUpcoming?: OfficialUpcomingView | null;
  revision: number;
  /**
   * Whether the current occurrence is in the loaded window. When it is not, nothing is known
   * about what follows it: a context read then carries no entries, and a queue read pages the
   * loaded window without a position.
   */
  upcoming?: UpcomingStatus & string;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "QueueOccurrence".
 */
export interface QueueOccurrence {
  entryId: QueueEntryId;
  track: Track;
}
/**
 * One Up Next filter the page shows.
 *
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "QueueFilterView".
 */
export interface QueueFilterView {
  /**
   * Pass to the `upNextFilter` playback mode. It names this filter for the suggestions
   * offered when it was read: after the listener changes songs or the suggestions change
   * seed, an old id is refused; read the context again then.
   */
  filterId: string;
  label: string;
  selected: boolean;
}
/**
 * The page's own continuation after the explicit list.
 *
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "OfficialUpcomingView".
 */
export interface OfficialUpcomingView {
  /**
   * More suggestions are loaded than listed here.
   */
  more: boolean;
  /**
   * `observed` (tracks follow the explicit list), `loading`, `off` (autoplay is off: nothing
   * plays after the list), `radio` (the queue refills itself) or `none`.
   */
  status: OfficialUpcomingStatus;
  /**
   * Suggestions in play order; only for `observed`.
   */
  tracks: Track[];
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "PlayerView".
 */
export interface PlayerView {
  audio?: AudioObservation | null;
  bridgeEpoch?: BridgeEpoch | null;
  freshness: Freshness;
  observed?: PlayerState | null;
  pending?: PendingPlayback | null;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "AudioObservation".
 */
export interface AudioObservation {
  /**
   * Song or video variant selected in the official page.
   */
  audioMode?: string | null;
  codec?: string | null;
  drmProtected?: boolean | null;
  /**
   * Opaque playback-quality label reported by the page, not a measured audio bit rate.
   */
  qualityLabel?: string | null;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "PendingPlayback".
 */
export interface PendingPlayback {
  activeOp?: PlayerOp | null;
  commands: number;
  desiredPlayback?: PlaybackStatus | null;
  desiredPositionMs?: number | null;
  desiredVolume?: number | null;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "Page".
 */
export interface Page {
  capturedAtMs: number;
  complete: boolean;
  freshness: Freshness;
  items: Track[];
  nextCursor?: string | null;
  source: string;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "Page2".
 */
export interface Page2 {
  capturedAtMs: number;
  complete: boolean;
  freshness: Freshness;
  items: SearchEntity[];
  nextCursor?: string | null;
  source: string;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "Page3".
 */
export interface Page3 {
  capturedAtMs: number;
  complete: boolean;
  freshness: Freshness;
  items: string[];
  nextCursor?: string | null;
  source: string;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "Playlist".
 */
export interface Playlist {
  description?: string | null;
  editable?: boolean | null;
  entries: Page4;
  identityConfirmed?: boolean;
  playlistId: PlaylistId;
  privacy?: PlaylistPrivacy | null;
  snapshotFingerprint?: string | null;
  title: string;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "Page4".
 */
export interface Page4 {
  capturedAtMs: number;
  complete: boolean;
  freshness: Freshness;
  items: PlaylistEntry[];
  nextCursor?: string | null;
  source: string;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "PlaylistEntry".
 */
export interface PlaylistEntry {
  entryId?: string | null;
  position: number;
  track: Track;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "ShareLink".
 */
export interface ShareLink {
  access: ShareAccess;
  url: string;
  visibility?: PlaylistPrivacy | null;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "RecommendationPage".
 */
export interface RecommendationPage {
  candidates: Page;
  seed: VideoId;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "AlbumPage".
 */
export interface AlbumPage {
  album: Album;
  artists: Artist[];
  complete: boolean;
  freshness?: Freshness | null;
  membership?: MembershipEvidence | null;
  nextCursor?: string | null;
  source: string;
  tracks: Track[];
  year?: number | null;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "MembershipEvidence".
 */
export interface MembershipEvidence {
  observedAtMs: number;
  value?: boolean | null;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "ArtistOverview".
 */
export interface ArtistOverview {
  albums: Album[];
  artist: Artist;
  complete: boolean;
  description?: string | null;
  freshness?: Freshness | null;
  source: string;
  subscription?: MembershipEvidence | null;
  topSongs: Track[];
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "Page5".
 */
export interface Page5 {
  capturedAtMs: number;
  complete: boolean;
  freshness: Freshness;
  items: PlaylistSummary[];
  nextCursor?: string | null;
  source: string;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "PlaylistSummary".
 */
export interface PlaylistSummary {
  playlistId: PlaylistId;
  title: string;
  trackCount?: number | null;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "Page6".
 */
export interface Page6 {
  capturedAtMs: number;
  complete: boolean;
  freshness: Freshness;
  items: LibraryAlbum[];
  nextCursor?: string | null;
  source: string;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "LibraryAlbum".
 */
export interface LibraryAlbum {
  album: Album;
  artists: Artist[];
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "Page7".
 */
export interface Page7 {
  capturedAtMs: number;
  complete: boolean;
  freshness: Freshness;
  items: LibraryArtist[];
  nextCursor?: string | null;
  source: string;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "LibraryArtist".
 */
export interface LibraryArtist {
  artist: Artist;
  byline?: string | null;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "ExtendedMediaPage".
 */
export interface ExtendedMediaPage {
  entity?: ExtendedMediaItem | null;
  entries: Page8;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "ExtendedMediaItem".
 */
export interface ExtendedMediaItem {
  description?: string | null;
  durationLabel?: string | null;
  id: string;
  kind: ExtendedMediaKind;
  publishedLabel?: string | null;
  relatedId?: BrowseId | null;
  subtitle?: string | null;
  title: string;
  videoId?: VideoId | null;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "Page8".
 */
export interface Page8 {
  capturedAtMs: number;
  complete: boolean;
  freshness: Freshness;
  items: ExtendedMediaItem[];
  nextCursor?: string | null;
  source: string;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "Page9".
 */
export interface Page9 {
  capturedAtMs: number;
  complete: boolean;
  freshness: Freshness;
  items: HistoryEntry[];
  nextCursor?: string | null;
  source: string;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "HistoryEntry".
 */
export interface HistoryEntry {
  media: HistoryMedia;
  period: string;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "LocalHistoryView".
 */
export interface LocalHistoryView {
  entries: Page10;
  retentionLimit: number;
  revision: string;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "Page10".
 */
export interface Page10 {
  capturedAtMs: number;
  complete: boolean;
  freshness: Freshness;
  items: LocalHistoryEntry[];
  nextCursor?: string | null;
  source: string;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "LocalHistoryEntry".
 */
export interface LocalHistoryEntry {
  durationMs?: number | null;
  entryId: string;
  playbackRef?: PlaybackRef | null;
  positionMs: number;
  recordedAtMs: number;
  track: Track;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
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
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "CombinedHistory".
 */
export interface CombinedHistory {
  local: LocalHistoryView;
  youtube?: Page9 | null;
  youtubeError?: ControlError | null;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "ControlError".
 */
export interface ControlError {
  code: ErrorCode;
  message: string;
  retryAfterMs?: number | null;
  retryDisposition: RetryDisposition;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "HomeView".
 */
export interface HomeView {
  freshness: Freshness;
  personalized: boolean;
  playlists: PlaylistSummary[];
  recent: Track[];
  source: string;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "DiscoveryPage".
 */
export interface DiscoveryPage {
  items: Page11;
  personalized?: boolean | null;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "Page11".
 */
export interface Page11 {
  capturedAtMs: number;
  complete: boolean;
  freshness: Freshness;
  items: DiscoveryItem[];
  nextCursor?: string | null;
  source: string;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "DiscoveryItem".
 */
export interface DiscoveryItem {
  section?: string | null;
  target: DiscoveryTarget;
  title: string;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "TrackView".
 */
export interface TrackView {
  freshness: Freshness;
  membership?: MembershipEvidence | null;
  ratingObservedAtMs?: number | null;
  source: string;
  track: Track;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "SettingsSnapshot".
 */
export interface SettingsSnapshot {
  settings: AppSettings;
  settingsRevision: number;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
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
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "LyricsPage".
 */
export interface LyricsPage {
  complete: boolean;
  fetchedAtMs: number;
  fileOffsetMs: number;
  kind: LyricsKind;
  language?: string | null;
  lines: LyricsLine[];
  matchBasis?: string | null;
  nextCursor?: string | null;
  plainText?: string | null;
  source: string;
  userOffsetMs: number;
  videoId: VideoId;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "LyricsLine".
 */
export interface LyricsLine {
  text: string;
  timeMs: number;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "Page12".
 */
export interface Page12 {
  capturedAtMs: number;
  complete: boolean;
  freshness: Freshness;
  items: DjPreference[];
  nextCursor?: string | null;
  source: string;
}
/**
 * One recorded DJ preference of the current account.
 *
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "DjPreference".
 */
export interface DjPreference {
  note?: string | null;
  preferenceId: string;
  recordedAtMs: number;
  /**
   * The registered client that recorded it; not a claim about a human.
   */
  recordedBy: string;
  stance: DjPreferenceStance;
  subject: DjPreferenceSubject;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "DjCueReply".
 */
export interface DjCueReply {
  chattiness: DjChattiness;
  cue?: DjCue | null;
}
/**
 * One prompt for the agent, with the listening context as it is at delivery.
 *
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "DjCue".
 */
export interface DjCue {
  ageMs: number;
  chattiness: DjChattiness;
  /**
   * Core epoch of the running daemon so background waiters can scope `(coreEpoch, seq)` safely.
   */
  coreEpoch?: CoreEpoch | null;
  kind: DjCueKind;
  listener: DjListener;
  /**
   * Moment DJ snapshot computed freshly when this prompt is delivered.
   */
  moment?: MomentView | null;
  moreQueued: boolean;
  nowPlaying?: DjNowPlaying | null;
  /**
   * What YouTube Music plays on its own after the queued tracks: status and up to 5
   * suggestions. Absent when the page state was not readable.
   */
  officialUpNext?: DjOfficialUpNext | null;
  /**
   * The recorded tastes, sent on this client's first prompt and whenever they changed since
   * its previous one; empty when `preferences_unchanged` or `preferences_unavailable`.
   */
  preferences: DjPreference[];
  /**
   * Total recorded tastes on the account when available (more than `preferences.len()` when paged).
   */
  preferencesTotal?: number | null;
  /**
   * True when the player could not read stored tastes for this prompt; keep earlier known tastes.
   */
  preferencesUnavailable?: boolean;
  /**
   * The tastes are the same as in this client's previous prompt.
   */
  preferencesUnchanged: boolean;
  prompt: string;
  reasons: DjCueReason[];
  /**
   * Newest first.
   */
  recent: DjRecentTrack[];
  seq: number;
  upNext: DjTrack[];
  /**
   * Whether `up_next` is what follows the current occurrence. When unknown it is empty and
   * says nothing about what comes next.
   */
  upcoming?: UpcomingStatus & string;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "DjListener".
 */
export interface DjListener {
  /**
   * Older actions since the previous prompt that are not listed.
   */
  actionsOmitted?: number;
  lastInteraction?: DjInteraction | null;
  lastInteractionAgoMs?: number | null;
  /**
   * When the agent last reported speaking to the listener.
   */
  lastSpokenToAgoMs?: number | null;
  /**
   * What the listener did by hand since this client's previous prompt, newest first, at most
   * [`crate::MAX_LISTENER_ACTIONS`]. Facts for the agent to read; never a request.
   */
  recentActions?: ListenerAction[];
  /**
   * Unanswered check-ins in a row: each check-in that comes with no listener action or words
   * since the previous check-in adds one, and any listener action or words reset it. The
   * first check-in after a reset counts 0. Above zero, check-ins come less often.
   */
  unansweredCheckIns: number;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "DjNowPlaying".
 */
export interface DjNowPlaying {
  playback: PlaybackStatus;
  positionMs: number;
  remainingMs?: number | null;
  track: DjTrack;
}
/**
 * YouTube Music's own continuation in a prompt.
 *
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "DjOfficialUpNext".
 */
export interface DjOfficialUpNext {
  /**
   * The selected Up Next filter's label, when one is offered.
   */
  filter?: string | null;
  /**
   * More suggestions are loaded than listed.
   */
  more: boolean;
  status: OfficialUpcomingStatus;
  tracks: DjTrack[];
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "DjRecentTrack".
 */
export interface DjRecentTrack {
  endedAgoMs: number;
  listenedMs: number;
  outcome: DjTrackOutcome;
  track: DjTrack;
}
/**
 * A compact picture of the signed-in listener's own music, built only from their library and
 * play history. Every part says how much it was built from; a part that could not be read is
 * named in `unavailable` and is not an empty result.
 *
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "ListenerProfile".
 */
export interface ListenerProfile {
  generatedAtMs: number;
  /**
   * Plays recorded by this player on this machine.
   */
  history: ProfileSample;
  liked: ProfileSample;
  /**
   * More playlists exist than listed.
   */
  morePlaylists: boolean;
  playlists: ProfilePlaylist[];
  /**
   * The writing systems of the titles and artist names in the sampled liked songs and plays,
   * largest first. A track counts once, under the script of most of its letters. It describes
   * what the listener listens to; it is not their language, country or interface language.
   */
  scripts: ScriptShare[];
  /**
   * Tracks the script figures were counted over.
   */
  scriptsSampled: number;
  /**
   * Taste notes stored for this account (read them with the DJ prompt or the taste tools).
   */
  tasteNotes: number;
  /**
   * Parts that could not be read this time: `liked`, `history`, `playlists`, `tasteNotes`.
   */
  unavailable: string[];
}
/**
 * One source of the profile: how much of it was read, who appears most, and its newest tracks.
 *
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "ProfileSample".
 */
export interface ProfileSample {
  /**
   * The newest sampled tracks, newest first.
   */
  recent: DjTrack[];
  /**
   * Tracks actually read for this profile.
   */
  sampled: number;
  /**
   * Artists with the most sampled tracks, most first.
   */
  topArtists: ProfileArtist[];
  /**
   * Tracks in the source as a whole, when the source says.
   */
  total?: number | null;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "ProfileArtist".
 */
export interface ProfileArtist {
  name: string;
  tracks: number;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "ProfilePlaylist".
 */
export interface ProfilePlaylist {
  playlistId: PlaylistId;
  title: string;
  trackCount?: number | null;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "ScriptShare".
 */
export interface ScriptShare {
  percent: number;
  script: Script;
  tracks: number;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "OperationReceipt".
 */
export interface OperationReceipt {
  accountScope?: AccountScope | null;
  error?: ControlError | null;
  itemResults: ItemResult[];
  operationId: string;
  phase: OperationPhase;
  /**
   * What a removal took away, recorded before anything is sent, so it can be restored.
   */
  removed?: RemovedItem[];
  resource?: OperationResource | null;
  retryDisposition: RetryDisposition;
  revision: number;
  schedulingStopped: boolean;
  settled: boolean;
  verification?: Verification | null;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "ItemResult".
 */
export interface ItemResult {
  error?: ErrorCode | null;
  inputIndex: number;
  phase: ItemPhase;
  resourceEntryId?: string | null;
}
/**
 * This interface was referenced by `ControlV1`'s JSON-Schema
 * via the `definition` "Verification".
 */
export interface Verification {
  level: VerificationLevel;
  observedAtMs: number;
}
