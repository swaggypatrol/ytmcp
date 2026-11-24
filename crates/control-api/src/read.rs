use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ytm_protocol::{
    AccountEpoch, AlbumId, ArtistId, AuthState, BridgeEpoch, BrowseId, CoreEpoch, Freshness,
    PlaybackStatus, PlaybackTimeAnchor, PlayerConnectionState, PlayerOp, PlaylistId,
    QueueCompleteness, QueueEntryId, RepeatMode, TrackAvailability, TrackInstanceId, VideoId,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Rating {
    Liked,
    Disliked,
    Neutral,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum PlaylistPrivacy {
    Private,
    Unlisted,
    Public,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Artist {
    pub id: Option<ArtistId>,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Album {
    pub id: Option<AlbumId>,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Track {
    pub video_id: VideoId,
    pub title: String,
    pub artists: Vec<Artist>,
    pub album: Option<Album>,
    pub duration_ms: Option<u64>,
    pub availability: TrackAvailability,
    pub version_tag: Option<String>,
    pub rating: Rating,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlaylistEntry {
    pub entry_id: Option<String>,
    pub position: u32,
    pub track: Track,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Playlist {
    pub playlist_id: PlaylistId,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub identity_confirmed: bool,
    pub title: String,
    pub description: Option<String>,
    pub privacy: Option<PlaylistPrivacy>,
    pub editable: Option<bool>,
    pub snapshot_fingerprint: Option<String>,
    pub entries: Page<PlaylistEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
    pub complete: bool,
    pub source: String,
    pub freshness: Freshness,
    pub captured_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueueOccurrence {
    pub entry_id: QueueEntryId,
    pub track: Track,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueueProjection {
    pub revision: u64,
    pub current_entry_id: Option<QueueEntryId>,
    pub entries: Vec<QueueOccurrence>,
    pub next_cursor: Option<String>,
    pub completeness: QueueCompleteness,
    pub has_more: Option<bool>,
    pub infinite: Option<bool>,
    pub loaded_start: Option<u32>,
    pub loaded_end_exclusive: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlayerState {
    pub connection: PlayerConnectionState,
    pub playback: PlaybackStatus,
    pub track_instance_id: Option<TrackInstanceId>,
    pub track: Option<Track>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playback_context: Option<ytm_protocol::PlaybackContext>,
    pub time_anchor: PlaybackTimeAnchor,
    pub volume: f64,
    pub muted: bool,
    pub shuffle: Option<bool>,
    pub repeat: Option<RepeatMode>,
    pub autoplay: Option<bool>,
    pub active_op: Option<PlayerOp>,
    pub pending_commands: u32,
    pub stopped_confirmed: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum PlayerInclude {
    State,
    Audio,
    #[default]
    Both,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AudioObservation {
    pub codec: Option<String>,
    /// Song or video variant selected in the official page.
    pub audio_mode: Option<String>,
    /// Opaque playback-quality label reported by the page, not a measured audio bit rate.
    pub quality_label: Option<String>,
    pub drm_protected: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PendingPlayback {
    pub active_op: Option<PlayerOp>,
    pub commands: u32,
    pub desired_playback: Option<PlaybackStatus>,
    pub desired_position_ms: Option<u64>,
    pub desired_volume: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlayerView {
    pub bridge_epoch: Option<BridgeEpoch>,
    pub freshness: Freshness,
    pub observed: Option<PlayerState>,
    pub pending: Option<PendingPlayback>,
    pub audio: Option<AudioObservation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountState {
    pub state: AuthState,
    pub account_epoch: Option<AccountEpoch>,
    pub player_catalog_matched: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum CapabilityArea {
    Catalog,
    Playback,
    Queue,
    Playlists,
    Library,
    Lyrics,
    Dj,
    Extensions,
    Account,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Capability {
    pub id: String,
    pub area: CapabilityArea,
    pub implemented: bool,
    pub verified_at_ms: Option<u64>,
    pub available_now: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventCursor {
    pub core_epoch: CoreEpoch,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextSnapshot {
    pub cursor: EventCursor,
    pub bridge_epoch: Option<BridgeEpoch>,
    pub captured_at_ms: u64,
    pub context_token: String,
    pub freshness: Freshness,
    pub player: PlayerState,
    pub account: AccountState,
    pub queue: QueueProjection,
    pub capabilities: Vec<Capability>,
    /// Owner-configured Moment DJ context projected for this client (`None` only in legacy fixtures).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moment: Option<crate::MomentView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AlbumPage {
    pub album: Album,
    pub artists: Vec<Artist>,
    pub year: Option<u16>,
    pub tracks: Vec<Track>,
    pub source: String,
    pub freshness: Option<Freshness>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub membership: Option<MembershipEvidence>,
    pub complete: bool,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArtistOverview {
    pub artist: Artist,
    pub description: Option<String>,
    pub top_songs: Vec<Track>,
    pub albums: Vec<Album>,
    pub source: String,
    pub freshness: Option<Freshness>,
    pub complete: bool,
    pub subscription: Option<MembershipEvidence>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MembershipEvidence {
    pub value: Option<bool>,
    pub observed_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LibraryAlbum {
    pub album: Album,
    pub artists: Vec<Artist>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LibraryArtist {
    pub artist: Artist,
    pub byline: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum HistoryMedia {
    Song { track: Box<Track> },
    PodcastEpisode { episode_id: BrowseId, title: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HistoryEntry {
    pub media: HistoryMedia,
    pub period: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlaylistSummary {
    pub playlist_id: PlaylistId,
    pub title: String,
    pub track_count: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    content = "page",
    rename_all = "camelCase",
    deny_unknown_fields
)]
pub enum LibraryPage {
    Playlists(Page<PlaylistSummary>),
    Liked(Page<Track>),
    Songs(Page<Track>),
    Albums(Page<LibraryAlbum>),
    Artists(Page<LibraryArtist>),
    Subscriptions(Page<LibraryArtist>),
    Uploads(Box<crate::ExtendedMediaPage>),
    Podcasts(Box<crate::ExtendedMediaPage>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HomeView {
    pub personalized: bool,
    pub freshness: Freshness,
    pub source: String,
    pub recent: Vec<Track>,
    pub playlists: Vec<PlaylistSummary>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum DiscoveryKind {
    Home,
    Explore,
    MoodsAndGenres,
    Charts,
    NewReleases,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum DiscoveryTarget {
    Track { video_id: VideoId },
    Playlist { playlist_id: PlaylistId },
    Album { album_id: AlbumId },
    Artist { artist_id: ArtistId },
    MoodCategory { category_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiscoveryItem {
    pub section: Option<String>,
    pub title: String,
    pub target: DiscoveryTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiscoveryPage {
    pub items: Page<DiscoveryItem>,
    pub personalized: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TrackView {
    pub track: Track,
    pub source: String,
    pub freshness: Freshness,
    pub rating_observed_at_ms: Option<u64>,
    pub membership: Option<MembershipEvidence>,
}
