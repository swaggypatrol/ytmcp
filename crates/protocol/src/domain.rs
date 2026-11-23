use crate::ids::{
    AccountEpoch, AccountScope, AlbumId, ArtistId, AssetId, BridgeEpoch, BrowseId, CommandId,
    CoreEpoch, CredentialRevision, PlaylistId, QueueEntryId, RequestId, TrackInstanceId, VideoId,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AuthState {
    Unknown,
    SignedOut,
    SigningIn,
    Authenticated,
    Expired,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum PlayerConnectionState {
    Starting,
    Ready,
    Stale,
    Recovering,
    Disconnected,
    Incompatible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum PlaybackStatus {
    Idle,
    Playing,
    Paused,
    Buffering,
    Ended,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum NetworkState {
    Online,
    Offline,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum CommandLifecycleStatus {
    Accepted,
    Dispatched,
    Observed,
    Failed,
    Indeterminate,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum MutationOutcome {
    Confirmed,
    Rejected,
    Indeterminate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum QueueCompleteness {
    Unknown,
    Partial,
    Complete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum CapabilitySupport {
    Supported,
    Unsupported,
    Unverified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum RepeatMode {
    Off,
    All,
    One,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum TrackAvailability {
    Available,
    Unavailable,
    RegionBlocked,
    AgeRestricted,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Freshness {
    Fresh,
    Stale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum LyricsKind {
    Synced,
    Plain,
    Instrumental,
    NotFound,
    Unavailable,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ThemeMode {
    Dark,
    Light,
    System,
}

/// The language the UI is shown in. Values are the UI's BCP 47 locale tags.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum UiLanguage {
    #[default]
    #[serde(rename = "en")]
    English,
    #[serde(rename = "zh-CN")]
    SimplifiedChinese,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum QueryErrorCode {
    AuthRequired,
    AuthExpired,
    AccountMismatch,
    RateLimited,
    NetworkOffline,
    Timeout,
    UnavailableTrack,
    UnsupportedCapability,
    BridgeIncompatible,
    StaleContext,
    Busy,
    InvalidPayload,
    StorageError,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueryError {
    pub code: QueryErrorCode,
    pub retriable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
    pub safe_message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<RequestId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ArtistRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<ArtistId>,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AlbumRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<AlbumId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browse_id: Option<BrowseId>,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CoverRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_id: Option<AssetId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent_oklch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    pub video_id: VideoId,
    pub title: String,
    #[serde(default)]
    pub artists: Vec<ArtistRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album: Option<AlbumRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<CoverRef>,
    pub availability: TrackAvailability,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_liked: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackRef {
    pub video_id: VideoId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playlist_id: Option<PlaylistId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playlist_entry_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browse_id: Option<BrowseId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_entry_id: Option<QueueEntryId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_index: Option<u32>,
}

/// Playlist identity and index read from the official page's selected native queue item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackContext {
    pub playlist_id: PlaylistId,
    pub start_index: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playlist_entry_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackTimeAnchor {
    pub position_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    pub playback_rate: f64,
    pub anchor_core_monotonic_ms: u64,
    pub extrapolating: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlayerObservedState {
    pub connection: PlayerConnectionState,
    pub playback: PlaybackStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge_epoch: Option<BridgeEpoch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_instance_id: Option<TrackInstanceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_track: Option<Track>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_queue_entry_id: Option<QueueEntryId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playback_context: Option<PlaybackContext>,
    pub time_anchor: PlaybackTimeAnchor,
    pub volume: f64,
    pub muted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shuffle: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat: Option<RepeatMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autoplay: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codec: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drm_protected: Option<bool>,
    pub stopped_confirmed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlayerPendingState {
    pub pending_command_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_op: Option<PlayerOp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desired_playback: Option<PlaybackStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desired_position_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desired_volume: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_receipt: Option<CommandReceipt>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueueEntry {
    pub entry_id: QueueEntryId,
    pub video_id: VideoId,
    pub track: Track,
    pub availability: TrackAvailability,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueueRange {
    pub start: u32,
    pub end_exclusive: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueueSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge_epoch: Option<BridgeEpoch>,
    pub queue_revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_entry_id: Option<QueueEntryId>,
    #[serde(default)]
    pub entries: Vec<QueueEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loaded_range: Option<QueueRange>,
    pub completeness: QueueCompleteness,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_more: Option<bool>,
    /// The explicit, non-radio list is loaded from its first to its last entry. An autoplay
    /// tail after it can still make `completeness` partial; the explicit end stays provable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explicit_complete: Option<bool>,
    /// The page keeps a continuation for the explicit list, so more entries may load after the
    /// last loaded one even when the loaded list looks complete. Official album queues keep one
    /// after every entry has loaded, so it does not by itself mean that anything follows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub may_load_more: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueueSummary {
    pub queue_revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_entry_id: Option<QueueEntryId>,
    pub loaded_count: u32,
    pub completeness: QueueCompleteness,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_more: Option<bool>,
    #[serde(default)]
    pub preview_entries: Vec<QueueEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CapabilitySet {
    pub play_pause: CapabilitySupport,
    pub seek: CapabilitySupport,
    pub next: CapabilitySupport,
    pub previous: CapabilitySupport,
    pub volume: CapabilitySupport,
    pub shuffle: CapabilitySupport,
    pub repeat: CapabilitySupport,
    pub queue_mirror: CapabilitySupport,
    pub queue_enqueue: CapabilitySupport,
    pub queue_reorder: CapabilitySupport,
    pub queue_remove: CapabilitySupport,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_autoplay: Option<CapabilitySupport>,
    /// The page would clear the upcoming entries now (finite queue without a continuation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_clear: Option<CapabilitySupport>,
    pub search: CapabilitySupport,
    pub library_playlists: CapabilitySupport,
    pub playlist_tracks: CapabilitySupport,
    pub album_detail: CapabilitySupport,
    pub artist_detail: CapabilitySupport,
    pub liked_songs: CapabilitySupport,
    pub personalized_home: CapabilitySupport,
    pub like_mutation: CapabilitySupport,
    pub playlist_add_mutation: CapabilitySupport,
    pub lyrics_lrclib: CapabilitySupport,
    pub mpris: CapabilitySupport,
}

impl Default for CapabilitySet {
    fn default() -> Self {
        Self {
            play_pause: CapabilitySupport::Unverified,
            seek: CapabilitySupport::Unverified,
            next: CapabilitySupport::Unverified,
            previous: CapabilitySupport::Unverified,
            volume: CapabilitySupport::Unverified,
            shuffle: CapabilitySupport::Unverified,
            repeat: CapabilitySupport::Unverified,
            queue_mirror: CapabilitySupport::Unverified,
            queue_enqueue: CapabilitySupport::Unverified,
            queue_reorder: CapabilitySupport::Unverified,
            queue_remove: CapabilitySupport::Unverified,
            queue_autoplay: None,
            queue_clear: None,
            search: CapabilitySupport::Unverified,
            library_playlists: CapabilitySupport::Unverified,
            playlist_tracks: CapabilitySupport::Unverified,
            album_detail: CapabilitySupport::Unverified,
            artist_detail: CapabilitySupport::Unverified,
            liked_songs: CapabilitySupport::Unverified,
            personalized_home: CapabilitySupport::Unverified,
            like_mutation: CapabilitySupport::Unverified,
            playlist_add_mutation: CapabilitySupport::Unverified,
            lyrics_lrclib: CapabilitySupport::Unverified,
            mpris: CapabilitySupport::Unverified,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HostHealth {
    pub wayland_native: Option<bool>,
    pub gpu_accelerated: Option<bool>,
    pub sandbox_enabled: bool,
    pub secret_store_backend: String,
    pub safe_storage_available: bool,
    pub tray_available: bool,
    pub mpris_active: bool,
    pub core_restart_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    pub theme: ThemeMode,
    pub reduce_motion: bool,
    pub reduce_transparency: bool,
    pub ui_scale_percent: u16,
    pub autoplay_on_launch: bool,
    pub close_to_tray: bool,
    pub lrclib_enabled: bool,
    pub track_notifications: bool,
    pub native_window_decorations: bool,
    /// Settings saved before the UI language existed load as English.
    #[serde(default)]
    pub ui_language: UiLanguage,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            theme: ThemeMode::Dark,
            reduce_motion: false,
            reduce_transparency: false,
            ui_scale_percent: 100,
            autoplay_on_launch: false,
            close_to_tray: true,
            lrclib_enabled: true,
            track_notifications: false,
            native_window_decorations: true,
            ui_language: UiLanguage::English,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthStatusInfo {
    pub state: AuthState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_epoch: Option<AccountEpoch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_scope: Option<AccountScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_revision: Option<CredentialRevision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_label: Option<String>,
    pub player_catalog_matched: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AppSnapshot {
    pub protocol_version: u32,
    pub core_epoch: CoreEpoch,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_epoch: Option<AccountEpoch>,
    pub revision: u64,
    pub host_health: HostHealth,
    pub auth_state: AuthStatusInfo,
    pub player: PlayerObservedState,
    pub pending: PlayerPendingState,
    pub queue_summary: QueueSummary,
    pub capabilities: CapabilitySet,
    pub settings: AppSettings,
    pub network_state: NetworkState,
    pub mock_mode: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum PlayerOp {
    Play,
    Pause,
    Toggle,
    Stop,
    Seek,
    SetVolume,
    SetMuted,
    Next,
    Previous,
    PlayTarget,
    PlayCollection,
    SetShuffle,
    SetRepeat,
    SetAutoplay,
    Enqueue,
    SelectQueueEntry,
    RemoveQueueEntry,
    ClearUpcoming,
    MoveQueueEntry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum EnqueuePosition {
    Next,
    Last,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlayerCommandArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_instance_id: Option<TrackInstanceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub muted: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<PlaybackRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shuffle: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat: Option<RepeatMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autoplay: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enqueue_position: Option<EnqueuePosition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_entry_id: Option<QueueEntryId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_queue_entry_id: Option<QueueEntryId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_revision: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlayerCommandPayload {
    pub command_id: CommandId,
    pub op: PlayerOp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<PlayerCommandArgs>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CommandReceipt {
    pub command_id: CommandId,
    pub op: PlayerOp,
    pub status: CommandLifecycleStatus,
    pub accepted_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<QueryError>,
    pub deduplicated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "action",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum AgentActivityRequest {
    Read,
    Cancel {
        #[schemars(length(min = 1, max = 64))]
        principal: String,
        #[schemars(length(min = 1, max = 128))]
        operation_id: String,
    },
    /// The user removes a recorded DJ preference from the desktop.
    ClearDjPreference {
        #[schemars(length(min = 1, max = 64))]
        preference_id: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AgentOperationPhase {
    Accepted,
    Submitted,
    Acknowledged,
    Verified,
    Unknown,
    Partial,
    Rejected,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentOperationSummary {
    pub principal: String,
    pub operation_id: String,
    pub phase: AgentOperationPhase,
    pub resource: Option<String>,
    pub verified_items: usize,
    pub total_items: usize,
    pub can_cancel: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AgentPreferenceStance {
    Prefer,
    Avoid,
}

/// A recorded DJ preference as the desktop shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentPreferenceSummary {
    pub preference_id: String,
    /// `video` or `artist`.
    pub subject_kind: String,
    pub subject_id: String,
    pub label: Option<String>,
    pub stance: AgentPreferenceStance,
    pub recorded_by: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentActivitySnapshot {
    pub operations: Vec<AgentOperationSummary>,
    /// Newest first (bounded).
    #[serde(default)]
    pub preferences: Vec<AgentPreferenceSummary>,
    /// All preferences of the account; more than `preferences` when only the newest are shown.
    #[serde(default)]
    pub preferences_total: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "deltaType", rename_all = "camelCase")]
#[allow(clippy::large_enum_variant)]
pub enum StateDelta {
    PlayerChanged {
        revision: u64,
        player: PlayerObservedState,
        pending: PlayerPendingState,
    },
    QueueReplaced {
        revision: u64,
        queue_summary: QueueSummary,
    },
    AuthChanged {
        revision: u64,
        auth_state: AuthStatusInfo,
    },
    CapabilitiesChanged {
        revision: u64,
        capabilities: CapabilitySet,
    },
    SettingsChanged {
        revision: u64,
        settings: AppSettings,
    },
    HostHealthChanged {
        revision: u64,
        host_health: HostHealth,
    },
    NetworkChanged {
        revision: u64,
        network_state: NetworkState,
    },
    CommandUpdated {
        revision: u64,
        receipt: CommandReceipt,
    },
    ManualTakeover {
        revision: u64,
        generation: u64,
    },
    ControlOperationChanged {
        revision: u64,
        principal: String,
        operation_id: String,
    },
    ResourceInvalidated {
        revision: u64,
        resource_kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resource_id: Option<String>,
    },
}

impl StateDelta {
    pub fn revision(&self) -> u64 {
        match self {
            Self::PlayerChanged { revision, .. }
            | Self::QueueReplaced { revision, .. }
            | Self::AuthChanged { revision, .. }
            | Self::CapabilitiesChanged { revision, .. }
            | Self::SettingsChanged { revision, .. }
            | Self::HostHealthChanged { revision, .. }
            | Self::NetworkChanged { revision, .. }
            | Self::CommandUpdated { revision, .. }
            | Self::ManualTakeover { revision, .. }
            | Self::ControlOperationChanged { revision, .. }
            | Self::ResourceInvalidated { revision, .. } => *revision,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PagedResult<T> {
    pub items: Vec<T>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub freshness: Freshness,
    pub fetched_at_ms: u64,
    pub account_scope: AccountScope,
    pub provider_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistSummary {
    pub playlist_id: PlaylistId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browse_id: Option<BrowseId>,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<CoverRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistDetail {
    pub playlist_id: PlaylistId,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<CoverRef>,
    pub tracks: PagedResult<Track>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AlbumDetail {
    pub album_id: AlbumId,
    pub title: String,
    #[serde(default)]
    pub artists: Vec<ArtistRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<CoverRef>,
    pub tracks: Vec<Track>,
    /// The album's provider audio playlist (`OLAK5uy_…`) used as playback context.
    /// Present only when every track in `tracks` was matched to a playable album row;
    /// otherwise `None`, and the UI plays single tracks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_playlist_id: Option<PlaylistId>,
    /// Aligned 1:1 with `tracks`: the provider's 0-based watch index of each track in
    /// `audio_playlist_id`. Non-empty only when `audio_playlist_id` is present.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub playback_indexes: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ArtistDetail {
    pub artist_id: ArtistId,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<CoverRef>,
    #[serde(default)]
    pub top_songs: Vec<Track>,
    #[serde(default)]
    pub albums: Vec<AlbumRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    pub query: String,
    pub songs: PagedResult<Track>,
    #[serde(default)]
    pub albums: Vec<AlbumRef>,
    #[serde(default)]
    pub playlists: Vec<PlaylistSummary>,
    #[serde(default)]
    pub artists: Vec<ArtistRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HomeFeed {
    pub personalized_supported: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_notice: Option<String>,
    #[serde(default)]
    pub recent_tracks: Vec<Track>,
    #[serde(default)]
    pub pinned_playlists: Vec<PlaylistSummary>,
    pub freshness: Freshness,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CatalogQueryRequest {
    pub kind: CatalogQueryKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playlist_id: Option<PlaylistId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_id: Option<AlbumId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist_id: Option<ArtistId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum CatalogQueryKind {
    Home,
    SearchSongs,
    LibraryPlaylists,
    PlaylistTracks,
    AlbumDetail,
    ArtistDetail,
    LikedSongs,
    RecentHistory,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RecentHistoryResult {
    pub items: Vec<Track>,
    pub freshness: Freshness,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "resultType", rename_all = "camelCase")]
pub enum CatalogResponsePayload {
    Home(HomeFeed),
    SearchSongs(SearchResult),
    LibraryPlaylists(PagedResult<PlaylistSummary>),
    PlaylistTracks(PlaylistDetail),
    AlbumDetail(AlbumDetail),
    ArtistDetail(ArtistDetail),
    LikedSongs(PagedResult<Track>),
    RecentHistory(RecentHistoryResult),
    Error(QueryError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LyricsLine {
    pub time_ms: u64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LyricsMatchMetadata {
    pub video_id: VideoId,
    pub title: String,
    pub artist: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_tag: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LyricsResult {
    pub video_id: VideoId,
    pub kind: LyricsKind,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_basis: Option<String>,
    pub fetched_at_ms: u64,
    pub file_offset_ms: i64,
    pub user_offset_ms: i64,
    #[serde(default)]
    pub synced_lines: Vec<LyricsLine>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plain_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<QueryError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LyricsQueryRequest {
    pub track: LyricsMatchMetadata,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_user_offset_ms: Option<i64>,
    pub reset_user_offset: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum MutationKind {
    SetLike,
    AddToPlaylist,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MutationRequest {
    pub kind: MutationKind,
    pub video_id: VideoId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub liked: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playlist_id: Option<PlaylistId>,
    pub confirmed_by_user: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MutationResult {
    pub kind: MutationKind,
    pub outcome: MutationOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<QueryError>,
}

/// Host -> Core secret session material. NEVER logged, serialized to disk, or forwarded to UI.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionMaterialPayload {
    pub account_epoch: AccountEpoch,
    pub credential_revision: CredentialRevision,
    pub cookie_pairs: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_user_index: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_label: Option<String>,
}

impl fmt::Debug for SessionMaterialPayload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionMaterialPayload")
            .field("account_epoch", &self.account_epoch)
            .field("credential_revision", &self.credential_revision)
            .field("cookie_count", &self.cookie_pairs.len())
            .field("redacted", &true)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HostHelloPayload {
    pub app_version: String,
    pub electron_version: String,
    pub chromium_version: String,
    pub host_health: HostHealth,
    pub network_state: NetworkState,
    pub mock_mode: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CoreReadyPayload {
    pub core_epoch: CoreEpoch,
    pub protocol_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HostWindowVisibilityPayload {
    pub main_visible: bool,
    pub main_focused: bool,
    pub player_visible: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BridgeReadyPayload {
    pub bridge_epoch: BridgeEpoch,
    pub bridge_version: String,
    pub capabilities: CapabilitySet,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_hint: Option<String>,
    pub logged_in_observed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BridgePlaybackObservedPayload {
    pub bridge_epoch: BridgeEpoch,
    pub playback: PlaybackStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_instance_id: Option<TrackInstanceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_track: Option<Track>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_queue_entry_id: Option<QueueEntryId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playback_context: Option<PlaybackContext>,
    pub position_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    pub playback_rate: f64,
    pub sample_age_ms: u64,
    pub volume: f64,
    pub muted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shuffle: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat: Option<RepeatMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autoplay: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codec: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drm_protected: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BridgeHealthPayload {
    pub bridge_epoch: BridgeEpoch,
    pub healthy: bool,
    pub video_element_present: bool,
    pub controls_bound: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BridgeCommandAckPayload {
    pub command_id: CommandId,
    pub bridge_epoch: BridgeEpoch,
    pub dispatched: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<QueryError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OutputGateControlPayload {
    pub transaction_id: String,
    pub gated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OutputGateResultPayload {
    pub transaction_id: String,
    pub applied: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResolveAssetPayload {
    pub asset_id: AssetId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssetResolvedPayload {
    pub asset_id: AssetId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<QueryError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OpenPlayerWindowPayload {
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BridgeLifecyclePayload {
    pub event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge_epoch: Option<BridgeEpoch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_music_origin: Option<bool>,
}
