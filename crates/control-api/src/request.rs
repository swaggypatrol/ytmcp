use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ytm_protocol::{AlbumId, ArtistId, PlaylistId, QueueEntryId, RepeatMode, VideoId};

use crate::{
    AccountState, AlbumPage, ArtistOverview, Capability, CapabilityArea, ContextSnapshot,
    ControlError, DiscoveryKind, DiscoveryPage, EventCursor, ExtendedMediaInput, ExtendedMediaPage,
    HomeView, LibraryPage, LyricsInput, LyricsOffsetAction, LyricsPage, OperationReceipt, Page,
    Playlist, PlaylistPrivacy, Preconditions, QueueProjection, Rating, SettingsPatch,
    SettingsSnapshot, Track, TrackView,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PageInput {
    #[schemars(length(min = 1, max = 2048))]
    pub cursor: Option<String>,
    #[schemars(range(min = 1, max = 100))]
    pub limit: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextInput {
    #[schemars(range(min = 0, max = 20))]
    pub preview_limit: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchInput {
    #[schemars(length(min = 1, max = 256))]
    pub query: String,
    pub page: Option<PageInput>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum TrackTarget {
    Video { video_id: VideoId },
    Context { context_token: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum PlayContextTarget {
    Video {
        video_id: VideoId,
    },
    Context {
        context_token: String,
    },
    Album {
        album_id: AlbumId,
        start_index: Option<u32>,
    },
    Playlist {
        playlist_id: PlaylistId,
        start_index: Option<u32>,
        #[schemars(length(min = 1, max = 512))]
        start_entry_id: Option<String>,
    },
    QueueEntry {
        entry_id: QueueEntryId,
        context_token: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum LibraryMembershipTarget {
    Song { id: VideoId },
    Playlist { id: PlaylistId },
    Album { id: AlbumId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum QueueMode {
    Next,
    Last,
}

/// The account change a client asks the user to make in the player.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AccountHandoffGoal {
    SignIn,
    Reauthenticate,
    SwitchAccount,
}

/// What follows an immediate interruption once the inserted tracks have played.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum InterruptResumePolicy {
    /// Replay the interrupted track from its saved position, then the original upcoming queue.
    ResumeInterrupted,
    /// Continue with the original upcoming queue; the interrupted track is not replayed.
    ContinueOriginalQueue,
    /// Start a new playback context from the inserted tracks; nothing is restored.
    StayOnInsertedContext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ResumeAction {
    ResumeUnsent,
    Reconcile,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum WriteAction {
    Play {
        target: PlayContextTarget,
    },
    Resume,
    Pause,
    Stop,
    Next,
    Previous,
    Seek {
        context_token: String,
        position_ms: u64,
    },
    Volume {
        #[schemars(range(min = 0, max = 1))]
        volume: f64,
    },
    Mute {
        muted: bool,
    },
    Shuffle {
        enabled: bool,
    },
    Repeat {
        mode: RepeatMode,
    },
    Autoplay {
        enabled: bool,
    },
    Enqueue {
        context_token: String,
        mode: QueueMode,
        #[schemars(length(min = 1, max = 50))]
        video_ids: Vec<VideoId>,
    },
    SelectQueueEntry {
        context_token: String,
        entry_id: QueueEntryId,
    },
    RemoveQueueEntry {
        context_token: String,
        entry_id: QueueEntryId,
    },
    MoveQueueEntry {
        context_token: String,
        entry_id: QueueEntryId,
        before_entry_id: Option<QueueEntryId>,
    },
    /// Remove the nearest loaded upcoming occurrences, at most 50; partial when more may follow.
    ClearUpcoming {
        context_token: String,
    },
    /// Clear the upcoming occurrences best effort, then insert in input order.
    ReplaceUpcoming {
        context_token: String,
        #[schemars(length(min = 1, max = 50))]
        video_ids: Vec<VideoId>,
    },
    /// Switch to the inserted tracks now and follow the explicit resume policy afterwards.
    InterruptWith {
        context_token: String,
        #[schemars(length(min = 1, max = 50))]
        video_ids: Vec<VideoId>,
        resume_policy: InterruptResumePolicy,
    },
    Rate {
        target: TrackTarget,
        rating: Rating,
    },
    SetArtistSubscription {
        artist_id: ArtistId,
        subscribed: bool,
    },
    SetLibraryMembership {
        target: LibraryMembershipTarget,
        saved: bool,
    },
    /// A struct variant: the edit carries its own `kind` tag inside `edit`.
    EditLocalHistory {
        edit: crate::LocalHistoryEdit,
    },
    CreatePrivatePlaylist {
        #[schemars(length(min = 1, max = 150))]
        title: String,
        #[schemars(length(max = 5000))]
        description: Option<String>,
    },
    AddPlaylistEntries {
        playlist_id: PlaylistId,
        created_by_operation_id: Option<String>,
        #[schemars(length(min = 1, max = 50))]
        video_ids: Vec<VideoId>,
    },
    UpdatePlaylist {
        playlist_id: PlaylistId,
        title: Option<String>,
        description: Option<String>,
    },
    SetPlaylistPrivacy {
        playlist_id: PlaylistId,
        privacy: PlaylistPrivacy,
    },
    RemovePlaylistEntries {
        playlist_id: PlaylistId,
        #[schemars(length(min = 1, max = 50))]
        entry_ids: Vec<String>,
    },
    MovePlaylistEntry {
        playlist_id: PlaylistId,
        entry_id: String,
        before_entry_id: Option<String>,
    },
    DeletePlaylist {
        playlist_id: PlaylistId,
    },
    UpdateSettings {
        expected_settings_revision: u64,
        patch: SettingsPatch,
    },
    SetLyricsOffset {
        target: TrackTarget,
        offset: LyricsOffsetAction,
    },
    SetDjFeedback {
        change: crate::DjFeedbackChange,
    },
    SetListeningIntent {
        change: crate::ListeningIntentChange,
    },
    /// Asks the user, without opening or focusing any window, to change the account.
    AccountHandoff {
        goal: AccountHandoffGoal,
    },
    Cancel {
        target_operation_id: String,
    },
    ResumeOperation {
        target_operation_id: String,
        expected_operation_revision: u64,
        action: ResumeAction,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WriteInput {
    #[schemars(length(min = 1, max = 128))]
    pub operation_id: String,
    pub preconditions: Preconditions,
    pub action: WriteAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlaylistInput {
    pub playlist_id: PlaylistId,
    pub page: Option<PageInput>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecommendationsInput {
    pub seed: TrackTarget,
    pub page: Option<PageInput>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationInput {
    #[schemars(length(min = 1, max = 128))]
    pub operation_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationReplayInput {
    #[schemars(length(min = 1, max = 128))]
    pub operation_id: String,
    #[schemars(length(min = 64, max = 64))]
    pub action_hash: String,
    pub token_was_missing: bool,
    pub explicit_preconditions: Option<Preconditions>,
}

/// Computes `(exact_action_sha256, tokenless_action_sha256)` for idempotency-replay validation
/// when bookkeeping fields (`contextToken` / `preconditions`) were omitted by the MCP caller.
pub fn compute_action_hashes(action: &WriteAction) -> (String, String) {
    let hash_value = |act: &WriteAction| -> String {
        let bytes = serde_json::to_vec(act).unwrap_or_default();
        hex::encode(Sha256::digest(bytes))
    };
    let exact = hash_value(action);
    let mut stripped = action.clone();
    match &mut stripped {
        WriteAction::Play {
            target: PlayContextTarget::QueueEntry { context_token, .. },
        }
        | WriteAction::Seek { context_token, .. }
        | WriteAction::Enqueue { context_token, .. }
        | WriteAction::SelectQueueEntry { context_token, .. }
        | WriteAction::RemoveQueueEntry { context_token, .. }
        | WriteAction::MoveQueueEntry { context_token, .. }
        | WriteAction::ClearUpcoming { context_token }
        | WriteAction::ReplaceUpcoming { context_token, .. }
        | WriteAction::InterruptWith { context_token, .. } => {
            context_token.clear();
        }
        _ => {}
    }
    let tokenless = hash_value(&stripped);
    (exact, tokenless)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AlbumInput {
    pub album_id: AlbumId,
    pub page: Option<PageInput>,
    pub include_membership: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArtistInput {
    pub artist_id: ArtistId,
    #[schemars(range(min = 1, max = 100))]
    pub limit: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum LibraryKind {
    Playlists,
    Liked,
    Songs,
    Albums,
    Artists,
    Subscriptions,
    Uploads,
    Podcasts,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LibraryInput {
    pub kind: LibraryKind,
    pub page: Option<PageInput>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LimitInput {
    #[schemars(range(min = 1, max = 100))]
    pub limit: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiscoveryInput {
    pub kind: DiscoveryKind,
    pub page: Option<PageInput>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MoodCategoryInput {
    #[schemars(length(min = 64, max = 64))]
    pub category_id: String,
    pub page: Option<PageInput>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchSuggestionsInput {
    #[schemars(length(min = 1, max = 256))]
    pub query: String,
    #[schemars(range(min = 1, max = 100))]
    pub limit: Option<u16>,
}

/// Application protocol over the local control socket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "method",
    content = "params",
    rename_all = "camelCase",
    deny_unknown_fields
)]
pub enum ControlRequest {
    GetCapabilities {
        area: Option<CapabilityArea>,
    },
    GetContext(ContextInput),
    GetAccount,
    GetPlayer {
        include: Option<crate::PlayerInclude>,
    },
    GetQueue(PageInput),
    Search(SearchInput),
    SearchEntities(crate::EntitySearchInput),
    GetPlaylist(PlaylistInput),
    GetShareLink(crate::ShareInput),
    Recommend(RecommendationsInput),
    GetOperation(OperationInput),
    ReplayOperation(OperationReplayInput),
    GetDjPreferences(crate::DjPreferencesInput),
    /// Returns the next prompt for a DJ agent, if any; polled by the MCP adapter while it waits.
    NextDjCue(crate::DjCueInput),
    GetSettings,
    GetAlbum(AlbumInput),
    GetArtist(ArtistInput),
    GetLibrary(LibraryInput),
    GetHistory(crate::HistoryInput),
    GetHome(LimitInput),
    GetTrack {
        target: TrackTarget,
    },
    GetDiscovery(DiscoveryInput),
    GetMoodCategory(MoodCategoryInput),
    GetSearchSuggestions(SearchSuggestionsInput),
    GetExtendedMedia(ExtendedMediaInput),
    GetLyrics(LyricsInput),
    Submit(WriteInput),
    Subscribe(ContextInput),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestFrame {
    pub control_version: u32,
    #[schemars(range(min = 1, max = 9007199254740991_u64))]
    pub request_id: u64,
    pub request: ControlRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "result",
    content = "value",
    rename_all = "camelCase",
    deny_unknown_fields
)]
pub enum ControlResponse {
    Capabilities(Vec<Capability>),
    Context(Box<ContextSnapshot>),
    Account(AccountState),
    Player(Box<crate::PlayerView>),
    Queue(QueueProjection),
    Search(Page<Track>),
    SearchEntities(Page<crate::SearchEntity>),
    SearchSuggestions(Page<String>),
    Playlist(Playlist),
    ShareLink(crate::ShareLink),
    Recommendations(RecommendationPage),
    Album(AlbumPage),
    Artist(ArtistOverview),
    Library(LibraryPage),
    History(Page<Track>),
    HistoryEntries(Page<crate::HistoryEntry>),
    LocalHistory(Box<crate::LocalHistoryView>),
    CombinedHistory(Box<crate::CombinedHistory>),
    Home(HomeView),
    Discovery(DiscoveryPage),
    ExtendedMedia(Box<ExtendedMediaPage>),
    Track(TrackView),
    Settings(SettingsSnapshot),
    Lyrics(LyricsPage),
    DjPreferences(Page<crate::DjPreference>),
    DjCue(Box<crate::DjCueReply>),
    Operation(OperationReceipt),
    Error(ControlError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecommendationPage {
    pub seed: VideoId,
    pub candidates: Page<Track>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "event",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ControlEvent {
    TrackChanged {
        cursor: EventCursor,
        video_id: Option<VideoId>,
    },
    QueueLow {
        cursor: EventCursor,
        queue_revision: u64,
    },
    QueueChanged {
        cursor: EventCursor,
        queue_revision: u64,
    },
    AuthChanged {
        cursor: EventCursor,
        account: AccountState,
    },
    ManualTakeover {
        cursor: EventCursor,
        generation: u64,
    },
    ResourceInvalidated {
        cursor: EventCursor,
        kind: String,
        id: Option<String>,
    },
    OperationChanged {
        cursor: EventCursor,
        operation_id: String,
    },
    EventGap {
        cursor: EventCursor,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ServerFrame {
    Response {
        request_id: u64,
        response: ControlResponse,
    },
    Event {
        event: ControlEvent,
    },
}

#[derive(JsonSchema)]
#[allow(dead_code)]
#[schemars(title = "Control V1")]
pub struct ControlSchemaRoot {
    request: RequestFrame,
    server: ServerFrame,
}
