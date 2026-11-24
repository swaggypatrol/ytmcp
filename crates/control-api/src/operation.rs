use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ytm_protocol::{
    AccountEpoch, AccountScope, ArtistId, BridgeEpoch, PlayerOp, PlaylistId, QueueEntryId, VideoId,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum OperationPhase {
    Accepted,
    Submitted,
    Acknowledged,
    Verified,
    Partial,
    Unknown,
    Rejected,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ItemPhase {
    NotStarted,
    Submitted,
    Acknowledged,
    Verified,
    Unknown,
    Rejected,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RetryDisposition {
    SafeReadRetry,
    QueryOperation,
    ReconcileFirst,
    NewPlanRequired,
    ReauthRequired,
    NotRetryable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    AuthRequired,
    PermissionDenied,
    NotEditable,
    CapabilityUnavailable,
    StaleContext,
    ContextExpired,
    ContextChanged,
    Conflict,
    PartialQueue,
    RateLimited,
    IdempotencyConflict,
    ResultUnknown,
    InvalidInput,
    IncompatibleVersion,
    Busy,
    Unavailable,
    StorageError,
    // A daily account-change limit of the client's tier, or the per-call edit limit, would be
    // exceeded. Nothing was sent; `retryAfterMs` says when the rolling window has room again.
    // (A plain comment: a doc comment would split the generated string union.)
    LimitReached,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ControlError {
    pub code: ErrorCode,
    pub retry_disposition: RetryDisposition,
    pub message: String,
    pub retry_after_ms: Option<u64>,
}

impl ControlError {
    pub fn new(
        code: ErrorCode,
        retry_disposition: RetryDisposition,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code,
            retry_disposition,
            message: message.into(),
            retry_after_ms: None,
        }
    }
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(formatter)
    }
}

impl std::error::Error for ControlError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Preconditions {
    pub expected_account_epoch: Option<AccountEpoch>,
    pub expected_bridge_epoch: Option<BridgeEpoch>,
    pub expected_queue_revision: Option<u64>,
    pub expected_entity_revision: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum OperationResource {
    Player {
        op: PlayerOp,
    },
    Queue,
    Playlist {
        id: PlaylistId,
    },
    Track {
        id: VideoId,
    },
    Artist {
        id: ArtistId,
        subscribed: Option<bool>,
    },
    LibraryMembership {
        target: crate::LibraryMembershipTarget,
        saved: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        album_playlist_id: Option<PlaylistId>,
    },
    LocalHistory {
        removed_count: Option<u32>,
    },
    Settings,
    LyricsOffset {
        video_id: VideoId,
        offset_ms: i64,
    },
    Operation {
        id: String,
    },
    DjPreference {
        id: String,
        cleared: bool,
    },
    ListeningIntent {
        intent_key: String,
        intent_id: String,
        cleared: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expires_at_ms: Option<u64>,
        moment_revision: u64,
    },
    /// A request for the user to sign in, renew or switch the account in the player.
    AccountHandoff {
        goal: crate::AccountHandoffGoal,
        state: AccountHandoffState,
    },
    /// Saved resume plan of an immediate interruption, updated as playback progresses.
    Interruption {
        policy: crate::InterruptResumePolicy,
        interrupted_entry_id: QueueEntryId,
        interrupted_video_id: VideoId,
        /// Playback position captured right before the switch, when the policy resumes it.
        resume_position_ms: Option<u64>,
        /// The queued replay occurrence of the interrupted track, once it is verified.
        resume_entry_id: Option<QueueEntryId>,
        resume: InterruptResumeState,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AccountHandoffState {
    /// The account already satisfies the goal; nothing was asked of the user.
    NotNeeded,
    /// Shown in the player's activity view; the user signs in there. No window is opened.
    AwaitingUser,
    /// The requested account change was observed in the player.
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum InterruptResumeState {
    /// The policy does not restore anything.
    NotApplicable,
    /// The inserted tracks are playing; the interrupted track has not been resumed yet.
    Pending,
    /// The replay occurrence started and the saved position was observed.
    Resumed,
    /// Manual control, a divergent queue, cancellation, expiry or a restart ended the plan.
    Abandoned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum VerificationLevel {
    Observed,
    ReadBack,
    Persisted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Verification {
    pub level: VerificationLevel,
    pub observed_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ItemResult {
    pub input_index: u32,
    pub phase: ItemPhase,
    pub resource_entry_id: Option<String>,
    pub error: Option<ErrorCode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationReceipt {
    pub operation_id: String,
    pub account_scope: Option<AccountScope>,
    pub revision: u64,
    pub phase: OperationPhase,
    pub resource: Option<OperationResource>,
    pub verification: Option<Verification>,
    pub item_results: Vec<ItemResult>,
    pub scheduling_stopped: bool,
    pub settled: bool,
    pub retry_disposition: RetryDisposition,
    pub error: Option<ControlError>,
    /// What a removal took away, recorded before anything is sent, so it can be restored.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<RemovedItem>,
}

/// One entry a removal took (or, while unsettled, may have taken) from the account or device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum RemovedItem {
    /// A playlist occurrence and its zero-based position in the playlist before the removal.
    PlaylistEntry {
        playlist_id: PlaylistId,
        entry_id: String,
        video_id: VideoId,
        position: u32,
    },
    /// A local playback history entry and when it was recorded.
    LocalHistoryEntry {
        entry_id: String,
        video_id: VideoId,
        recorded_at_ms: u64,
    },
}

/// Deployment grant actions. These values are loaded by trusted local ingress, never tool input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GrantAction {
    /// Device settings and lyrics offsets.
    Preferences,
    Dj,
    Playback,
    Queue,
    Rating,
    ArtistSubscription,
    LibraryMembership,
    LocalHistory,
    CreatePrivatePlaylist,
    AddPlaylistEntries,
    EditPlaylist,
    ChangePlaylistPrivacy,
    DeletePlaylist,
    AccountHandoff,
}
