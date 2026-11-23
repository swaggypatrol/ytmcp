use crate::clock::PlaybackClock;
use crate::commands::{
    ActiveCommand, CommandDeduplicator, EnqueuePlan, PausedSkipPhase, PausedSkipState,
    PendingOpPlan, PlanEvaluation, PlaybackFence, QueuedCommand, SEEK_TOLERANCE_MS, StopPhase,
    evaluate_active_command, is_coalescible, is_non_idempotent, is_terminal_status,
    resolve_toggle_op, seek_within_tolerance, validate_command_payload,
};
use crate::error::CoreError;
use crate::ports::{
    BridgePort, CachedCover, CatalogIdentity, CatalogPort, Clock, LastSession, LyricsPort,
    NoopBridgePort, NoopOutputGatePort, OutputGatePort, StoragePort, SystemClock,
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{OnceCell, broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use uuid::Uuid;
use ytm_protocol::{
    AccountEpoch, AccountScope, AppSettings, AppSnapshot, AssetId, AssetResolvedPayload, AuthState,
    AuthStatusInfo, BridgeCommandAckPayload, BridgeEpoch, BridgeHealthPayload,
    BridgeLifecyclePayload, BridgePlaybackObservedPayload, BridgeReadyPayload, CapabilitySet,
    CapabilitySupport, CatalogQueryRequest, CatalogResponsePayload, CommandId,
    CommandLifecycleStatus, CommandReceipt, CoreEpoch, CredentialRevision, EnqueuePosition,
    HostHealth, HostHelloPayload, HostWindowVisibilityPayload, LyricsKind, LyricsQueryRequest,
    LyricsResult, MAX_COMMAND_QUEUE, MutationRequest, MutationResult, NetworkState,
    OutputGateResultPayload, PROTOCOL_VERSION, PlaybackStatus, PlayerCommandArgs,
    PlayerCommandPayload, PlayerConnectionState, PlayerObservedState, PlayerOp, PlayerPendingState,
    QueryError, QueryErrorCode, QueueEntryId, QueueSnapshot, QueueSummary, SessionMaterialPayload,
    StateDelta, TrackInstanceId,
};

const MAX_QUEUE_PREVIEW_ENTRIES: usize = 100;
const MAX_ENQUEUE_PROOFS: usize = 64;
const MAX_DISPATCHED_COMMANDS: usize = 256;
const ACTOR_CHANNEL_CAPACITY: usize = 256;
const DELTA_BROADCAST_CAPACITY: usize = 512;

/// Observation deadline for basic player controls. 3 s was the first estimate; on the official
/// page next/previous needed up to ~8 s to load and report the new track, so this single value
/// is used by the daemon and the tests.
pub const DEFAULT_CONTROL_TIMEOUT_MS: u64 = 8_000;
const RESTORE_SEEK_TIMEOUT_MS: u64 = 1_500;

/// Configuration for `CoreEngine` timeouts and runtime mode.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub control_timeout_ms: u64,
    pub play_target_timeout_ms: u64,
    pub dispatch_timeout_ms: u64,
    pub tick_interval_ms: u64,
    pub mock_mode: bool,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            control_timeout_ms: DEFAULT_CONTROL_TIMEOUT_MS,
            play_target_timeout_ms: 15_000,
            dispatch_timeout_ms: 3_000,
            tick_interval_ms: 50,
            mock_mode: false,
        }
    }
}

/// Port dependencies injected into `CoreEngine` by `ytm-daemon` or test harnesses.
#[derive(Clone)]
pub struct CoreDeps {
    pub clock: Arc<dyn Clock>,
    pub bridge: Arc<dyn BridgePort>,
    pub output_gate: Arc<dyn OutputGatePort>,
    pub catalog: Option<Arc<dyn CatalogPort>>,
    pub storage: Option<Arc<dyn StoragePort>>,
    pub lyrics: Option<Arc<dyn LyricsPort>>,
}

impl Default for CoreDeps {
    fn default() -> Self {
        Self {
            clock: Arc::new(SystemClock::new()),
            bridge: Arc::new(NoopBridgePort),
            output_gate: Arc::new(NoopOutputGatePort),
            catalog: None,
            storage: None,
            lyrics: None,
        }
    }
}

/// Diagnostic counters exposed for verification and soak metrics.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CoreDiagnostics {
    pub dropped_stale_bridge_events: u64,
    pub dropped_non_monotonic_seq: u64,
    pub seq_gaps_detected: u64,
    pub snapshot_rebuilds_requested: u64,
    pub dropped_stale_account_responses: u64,
    pub bridge_dispatches: u64,
}

pub(crate) struct ControlRead {
    pub snapshot: AppSnapshot,
    pub queue: Option<QueueSnapshot>,
    pub captured_at_ms: u64,
    pub playback_generation: u64,
    pub settings_revision: u64,
    /// Monotonic time of the latest command from the player window, media keys or MPRIS.
    pub last_manual_command_ms: Option<u64>,
    pub events: Option<broadcast::Receiver<StateDelta>>,
}

enum CommandOrigin {
    Manual,
    Internal,
    External(PlaybackFence),
}

enum CoreMsg {
    SubscribeSnapshot {
        reply: oneshot::Sender<(AppSnapshot, broadcast::Receiver<StateDelta>)>,
    },
    ReadControl {
        subscribe: bool,
        reply: oneshot::Sender<ControlRead>,
    },
    GetSnapshot {
        reply: oneshot::Sender<AppSnapshot>,
    },
    GetQueueSnapshot {
        reply: oneshot::Sender<Option<QueueSnapshot>>,
    },
    GetCommandReceipt {
        command_id: CommandId,
        reply: oneshot::Sender<Option<CommandReceipt>>,
    },
    GetEnqueueProof {
        command_id: CommandId,
        reply: oneshot::Sender<Option<(QueueEntryId, u64)>>,
    },
    MayHaveReachedPage {
        command_id: CommandId,
        reply: oneshot::Sender<bool>,
    },
    GetDiagnostics {
        reply: oneshot::Sender<CoreDiagnostics>,
    },
    RecentReceipts {
        reply: oneshot::Sender<Vec<CommandReceipt>>,
    },
    SubmitPlayerCommand {
        payload: Box<PlayerCommandPayload>,
        fence: Option<Box<PlaybackFence>>,
        reply: oneshot::Sender<Result<CommandReceipt, QueryError>>,
    },
    HostHello {
        payload: Box<HostHelloPayload>,
    },
    HostNetworkChanged {
        network: NetworkState,
    },
    HostPowerResume,
    HostWindowVisibility {
        payload: HostWindowVisibilityPayload,
    },
    HostBridgeLifecycle {
        payload: BridgeLifecyclePayload,
    },
    SessionMaterial {
        payload: Box<SessionMaterialPayload>,
        reply: oneshot::Sender<Result<(), QueryError>>,
    },
    Logout {
        reply: oneshot::Sender<Result<(), QueryError>>,
    },
    AuthExpired {
        account_epoch: Option<AccountEpoch>,
        reason: Option<String>,
    },
    BridgeReady {
        seq: u64,
        payload: Box<BridgeReadyPayload>,
        reply: oneshot::Sender<bool>,
    },
    BridgePlaybackObserved {
        seq: u64,
        payload: Box<BridgePlaybackObservedPayload>,
        reply: oneshot::Sender<bool>,
    },
    BridgeQueueObserved {
        seq: u64,
        payload: Box<QueueSnapshot>,
        reply: oneshot::Sender<bool>,
    },
    BridgeHealth {
        seq: u64,
        payload: BridgeHealthPayload,
        reply: oneshot::Sender<bool>,
    },
    BridgeCommandAck {
        seq: u64,
        payload: BridgeCommandAckPayload,
        reply: oneshot::Sender<bool>,
    },
    BridgeError {
        seq: u64,
        bridge_epoch: Option<BridgeEpoch>,
        error: Box<QueryError>,
        reply: oneshot::Sender<bool>,
    },
    UpdateSettings {
        settings: Box<AppSettings>,
        reply: oneshot::Sender<Result<AppSettings, QueryError>>,
    },
    AcquireCatalogContext {
        require_auth: bool,
        reply: oneshot::Sender<Result<CatalogContext, QueryError>>,
    },
    AcquireLyricsContext {
        reply: oneshot::Sender<LyricsContext>,
    },
    Tick {
        reply: oneshot::Sender<()>,
    },
    InvalidateResource {
        resource_kind: String,
    },
    InvalidateControlResource {
        account_epoch: AccountEpoch,
        resource_kind: String,
        resource_id: String,
    },
    ControlOperationChanged {
        account_epoch: Option<AccountEpoch>,
        principal: String,
        operation_id: String,
    },
    Shutdown {
        reply: oneshot::Sender<()>,
    },
}

#[derive(Clone)]
struct CatalogContext {
    account_epoch: Option<AccountEpoch>,
    account_scope: Option<AccountScope>,
    identity_matched: bool,
    cancel: CancellationToken,
}

#[derive(Clone)]
struct LyricsContext {
    enabled: bool,
    current_video_id: Option<ytm_protocol::VideoId>,
    cancel: CancellationToken,
}

enum InternalEvent {
    OrphanGateReleased(String),
    OutputGateCompleted {
        dispatch_token: u64,
        command_id: CommandId,
        transaction_id: String,
        gated: bool,
        result: Result<OutputGateResultPayload, QueryError>,
        reply: Option<oneshot::Sender<Result<CommandReceipt, QueryError>>>,
        bridge_payload_after_gate: Option<Box<PlayerCommandPayload>>,
    },
    BridgeDispatchCompleted {
        dispatch_token: u64,
        command_id: CommandId,
        result: Result<(), QueryError>,
        /// The fence refused before the send future was first polled; nothing reached the page.
        not_sent: bool,
        reply: Option<oneshot::Sender<Result<CommandReceipt, QueryError>>>,
    },
    SessionMaterialApplied {
        cancel: CancellationToken,
        account_epoch: AccountEpoch,
        credential_revision: CredentialRevision,
        channel_label: Option<String>,
        same_account: bool,
        result: Result<CatalogIdentity, QueryError>,
        reply: oneshot::Sender<Result<(), QueryError>>,
    },
    LastSessionLoaded {
        session: Option<Box<LastSession>>,
    },
    CoverCached {
        remote_url: String,
        result: Result<CachedCover, QueryError>,
    },
}

/// Cloneable handle to the single-writer `CoreEngine` actor.
#[derive(Clone)]
pub struct CoreEngine {
    core_epoch: CoreEpoch,
    tx: mpsc::Sender<CoreMsg>,
    mutation_lock: Arc<tokio::sync::Mutex<()>>,
    settings_lock: Arc<tokio::sync::Mutex<()>>,
    deps: CoreDeps,
    clock: Arc<dyn Clock>,
    shutdown_token: CancellationToken,
}

impl CoreEngine {
    /// Spawns the single-writer `CoreEngine` actor on the current Tokio runtime.
    pub fn start(deps: CoreDeps, config: EngineConfig) -> Self {
        let core_epoch = CoreEpoch::new(format!("core-{}", Uuid::new_v4()))
            .expect("generated core epoch UUID is a valid ID");
        let (tx, rx) = mpsc::channel(ACTOR_CHANNEL_CAPACITY);
        let (internal_tx, internal_rx) = mpsc::channel(ACTOR_CHANNEL_CAPACITY);
        let (delta_tx, _) = broadcast::channel(DELTA_BROADCAST_CAPACITY);
        let shutdown_token = CancellationToken::new();

        let clock = deps.clock.clone();
        let actor = CoreActor::new(
            core_epoch.clone(),
            deps.clone(),
            config.clone(),
            delta_tx,
            internal_tx,
            shutdown_token.clone(),
        );

        tokio::spawn(actor.run(rx, internal_rx));

        Self {
            core_epoch,
            tx,
            mutation_lock: Arc::new(tokio::sync::Mutex::new(())),
            settings_lock: Arc::new(tokio::sync::Mutex::new(())),
            deps,
            clock,
            shutdown_token,
        }
    }

    pub fn core_epoch(&self) -> &CoreEpoch {
        &self.core_epoch
    }

    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.clock
    }

    /// Validates an incoming envelope's `coreEpoch` if present.
    pub fn validate_core_epoch(&self, epoch: Option<&CoreEpoch>) -> Result<(), QueryError> {
        if let Some(ep) = epoch
            && ep != &self.core_epoch
        {
            return Err(CoreError::StaleContext(format!(
                "coreEpoch mismatch: expected {}, got {}",
                self.core_epoch, ep
            ))
            .into());
        }
        Ok(())
    }

    /// Atomically captures `(AppSnapshot, broadcast::Receiver<StateDelta>)` inside the single-writer actor
    /// so `snapshot.revision == R` strictly precedes all `R + 1` deltas received on the channel.
    pub async fn subscribe_snapshot(
        &self,
    ) -> Result<(AppSnapshot, broadcast::Receiver<StateDelta>), CoreError> {
        let (reply, rx) = oneshot::channel();
        self.send_msg(CoreMsg::SubscribeSnapshot { reply }).await?;
        rx.await.map_err(|_| CoreError::EngineStopped)
    }

    pub(crate) async fn read_control(&self, subscribe: bool) -> Result<ControlRead, CoreError> {
        let (reply, rx) = oneshot::channel();
        self.send_msg(CoreMsg::ReadControl { subscribe, reply })
            .await?;
        rx.await.map_err(|_| CoreError::EngineStopped)
    }

    pub async fn snapshot(&self) -> Result<AppSnapshot, CoreError> {
        let (reply, rx) = oneshot::channel();
        self.send_msg(CoreMsg::GetSnapshot { reply }).await?;
        rx.await.map_err(|_| CoreError::EngineStopped)
    }

    pub async fn queue_snapshot(&self) -> Result<Option<QueueSnapshot>, CoreError> {
        let (reply, rx) = oneshot::channel();
        self.send_msg(CoreMsg::GetQueueSnapshot { reply }).await?;
        rx.await.map_err(|_| CoreError::EngineStopped)
    }

    pub async fn command_receipt(
        &self,
        command_id: CommandId,
    ) -> Result<Option<CommandReceipt>, CoreError> {
        let (reply, rx) = oneshot::channel();
        self.send_msg(CoreMsg::GetCommandReceipt { command_id, reply })
            .await?;
        rx.await.map_err(|_| CoreError::EngineStopped)
    }

    /// The occurrence and queue revision that proved an observed enqueue, read at completion.
    /// A later snapshot may already have moved on (for example a natural track change).
    pub async fn enqueue_proof(
        &self,
        command_id: CommandId,
    ) -> Result<Option<(QueueEntryId, u64)>, CoreError> {
        let (reply, rx) = oneshot::channel();
        self.send_msg(CoreMsg::GetEnqueueProof { command_id, reply })
            .await?;
        rx.await.map_err(|_| CoreError::EngineStopped)
    }

    /// Whether the command may have changed the page: it started a bridge dispatch and the
    /// bridge did not refuse it before acting. Admission refusals and bridge refusals did not.
    pub async fn may_have_reached_page(&self, command_id: CommandId) -> Result<bool, CoreError> {
        let (reply, rx) = oneshot::channel();
        self.send_msg(CoreMsg::MayHaveReachedPage { command_id, reply })
            .await?;
        rx.await.map_err(|_| CoreError::EngineStopped)
    }

    /// Command receipts updated in the last minute (bounded). A subscriber that lagged behind the
    /// delta stream replays these after its fresh snapshot so terminal states are not lost.
    pub async fn recent_receipts(&self) -> Result<Vec<CommandReceipt>, CoreError> {
        let (reply, rx) = oneshot::channel();
        self.send_msg(CoreMsg::RecentReceipts { reply }).await?;
        rx.await.map_err(|_| CoreError::EngineStopped)
    }

    pub async fn diagnostics(&self) -> Result<CoreDiagnostics, CoreError> {
        let (reply, rx) = oneshot::channel();
        self.send_msg(CoreMsg::GetDiagnostics { reply }).await?;
        rx.await.map_err(|_| CoreError::EngineStopped)
    }

    /// Submits a command from a trusted local input such as the UI or MPRIS.
    pub async fn submit_player_command(
        &self,
        payload: PlayerCommandPayload,
    ) -> Result<CommandReceipt, QueryError> {
        self.submit_player_command_with_fence(payload, None).await
    }

    /// Submits an authorized external command after its caller has recorded its intent.
    pub async fn submit_external_player_command(
        &self,
        payload: PlayerCommandPayload,
        fence: PlaybackFence,
    ) -> Result<CommandReceipt, QueryError> {
        self.submit_player_command_with_fence(payload, Some(Box::new(fence)))
            .await
    }

    async fn submit_player_command_with_fence(
        &self,
        payload: PlayerCommandPayload,
        fence: Option<Box<PlaybackFence>>,
    ) -> Result<CommandReceipt, QueryError> {
        let (reply, rx) = oneshot::channel();
        self.send_msg(CoreMsg::SubmitPlayerCommand {
            payload: Box::new(payload),
            fence,
            reply,
        })
        .await
        .map_err(QueryError::from)?;
        rx.await
            .map_err(|_| QueryError::from(CoreError::EngineStopped))?
    }

    pub async fn on_host_hello(&self, payload: HostHelloPayload) -> Result<(), CoreError> {
        self.send_msg(CoreMsg::HostHello {
            payload: Box::new(payload),
        })
        .await
    }

    pub async fn on_host_network_changed(&self, network: NetworkState) -> Result<(), CoreError> {
        self.send_msg(CoreMsg::HostNetworkChanged { network }).await
    }

    pub async fn on_host_power_resume(&self) -> Result<(), CoreError> {
        self.send_msg(CoreMsg::HostPowerResume).await
    }

    pub async fn on_host_window_visibility(
        &self,
        payload: HostWindowVisibilityPayload,
    ) -> Result<(), CoreError> {
        self.send_msg(CoreMsg::HostWindowVisibility { payload })
            .await
    }

    pub async fn on_host_bridge_lifecycle(
        &self,
        payload: BridgeLifecyclePayload,
    ) -> Result<(), CoreError> {
        self.send_msg(CoreMsg::HostBridgeLifecycle { payload })
            .await
    }

    /// Applies host-supplied session material without logging secrets.
    /// Account changes cancel in-flight private operations and clear account-scoped UI state;
    /// same-account credential rotation preserves UI state.
    pub async fn on_session_material(
        &self,
        payload: SessionMaterialPayload,
    ) -> Result<(), QueryError> {
        self.enqueue_session_material(payload).await?.await
    }

    /// Queue the material in host frame order. Waiting for the returned receipt performs no IO
    /// on the frame reader and can be delegated after the frame has entered the actor.
    pub async fn enqueue_session_material(
        &self,
        payload: SessionMaterialPayload,
    ) -> Result<impl Future<Output = Result<(), QueryError>> + use<>, QueryError> {
        let (reply, rx) = oneshot::channel();
        self.send_msg(CoreMsg::SessionMaterial {
            payload: Box::new(payload),
            reply,
        })
        .await
        .map_err(QueryError::from)?;
        Ok(async move {
            rx.await
                .map_err(|_| QueryError::from(CoreError::EngineStopped))?
        })
    }

    pub async fn on_logout(&self) -> Result<(), CoreError> {
        let (reply, rx) = oneshot::channel();
        self.send_msg(CoreMsg::Logout { reply }).await?;
        rx.await
            .map_err(|_| CoreError::EngineStopped)?
            .map_err(CoreError::from)
    }

    /// Reports that the catalog rejected the credentials of `account_epoch` (HTTP 401/403).
    /// Ignored when a newer session has already replaced that epoch.
    pub async fn on_auth_expired(
        &self,
        account_epoch: Option<AccountEpoch>,
        reason: Option<String>,
    ) -> Result<(), CoreError> {
        self.send_msg(CoreMsg::AuthExpired {
            account_epoch,
            reason,
        })
        .await
    }

    async fn note_catalog_error(&self, account_epoch: Option<AccountEpoch>, err: &QueryError) {
        if err.code == QueryErrorCode::AuthExpired {
            let _ = self
                .on_auth_expired(account_epoch, Some(err.safe_message.clone()))
                .await;
        }
    }

    /// Handles `bridge.ready`; returns `true` if accepted, `false` if dropped as stale/non-monotonic.
    pub async fn on_bridge_ready(
        &self,
        seq: u64,
        payload: BridgeReadyPayload,
    ) -> Result<bool, CoreError> {
        let (reply, rx) = oneshot::channel();
        self.send_msg(CoreMsg::BridgeReady {
            seq,
            payload: Box::new(payload),
            reply,
        })
        .await?;
        rx.await.map_err(|_| CoreError::EngineStopped)
    }

    /// Handles `bridge.playbackObserved`; returns `true` if accepted, `false` if dropped.
    pub async fn on_bridge_playback_observed(
        &self,
        seq: u64,
        payload: BridgePlaybackObservedPayload,
    ) -> Result<bool, CoreError> {
        let (reply, rx) = oneshot::channel();
        self.send_msg(CoreMsg::BridgePlaybackObserved {
            seq,
            payload: Box::new(payload),
            reply,
        })
        .await?;
        rx.await.map_err(|_| CoreError::EngineStopped)
    }

    /// Handles `bridge.queueObserved`; returns `true` if accepted, `false` if dropped.
    pub async fn on_bridge_queue_observed(
        &self,
        seq: u64,
        payload: QueueSnapshot,
    ) -> Result<bool, CoreError> {
        let (reply, rx) = oneshot::channel();
        self.send_msg(CoreMsg::BridgeQueueObserved {
            seq,
            payload: Box::new(payload),
            reply,
        })
        .await?;
        rx.await.map_err(|_| CoreError::EngineStopped)
    }

    pub async fn on_bridge_health(
        &self,
        seq: u64,
        payload: BridgeHealthPayload,
    ) -> Result<bool, CoreError> {
        let (reply, rx) = oneshot::channel();
        self.send_msg(CoreMsg::BridgeHealth {
            seq,
            payload,
            reply,
        })
        .await?;
        rx.await.map_err(|_| CoreError::EngineStopped)
    }

    pub async fn on_bridge_command_ack(
        &self,
        seq: u64,
        payload: BridgeCommandAckPayload,
    ) -> Result<bool, CoreError> {
        let (reply, rx) = oneshot::channel();
        self.send_msg(CoreMsg::BridgeCommandAck {
            seq,
            payload,
            reply,
        })
        .await?;
        rx.await.map_err(|_| CoreError::EngineStopped)
    }

    pub async fn on_bridge_error(
        &self,
        seq: u64,
        bridge_epoch: Option<BridgeEpoch>,
        error: QueryError,
    ) -> Result<bool, CoreError> {
        let (reply, rx) = oneshot::channel();
        self.send_msg(CoreMsg::BridgeError {
            seq,
            bridge_epoch,
            error: Box::new(error),
            reply,
        })
        .await?;
        rx.await.map_err(|_| CoreError::EngineStopped)
    }

    /// Persists settings, then applies them (publishing `settingsChanged`). Updates are
    /// serialized so disk and memory end in the same order; a save failure is returned and
    /// leaves the applied settings unchanged.
    pub async fn update_settings(&self, settings: AppSettings) -> Result<AppSettings, QueryError> {
        let _serialized = self.settings_lock.lock().await;
        if let Some(storage) = &self.deps.storage {
            storage.save_settings(settings.clone()).await?;
        }
        self.apply_settings(settings).await
    }

    pub(crate) async fn patch_control_settings(
        &self,
        expected: u64,
        patch: ytm_control_api::SettingsPatch,
        scope: Option<&AccountScope>,
        expected_epoch: Option<&AccountEpoch>,
    ) -> Result<(), QueryError> {
        let _serialized = self.settings_lock.lock().await;
        let state = self.read_control(false).await.map_err(QueryError::from)?;
        if state.settings_revision != expected
            || state.snapshot.auth_state.account_scope.as_ref() != scope
            || state.snapshot.account_epoch.as_ref() != expected_epoch
        {
            return Err(CoreError::StaleContext(
                "settings revision or active account changed".into(),
            )
            .into());
        }
        let settings = patch.apply(state.snapshot.settings);
        if let Some(storage) = &self.deps.storage {
            storage.save_settings(settings.clone()).await?;
        }
        self.apply_settings(settings).await.map(|_| ())
    }

    /// Applies already-persisted settings at startup, including on a read-only profile.
    pub async fn restore_settings(&self) -> Result<(), QueryError> {
        let _serialized = self.settings_lock.lock().await;
        if let Some(storage) = &self.deps.storage
            && let Some(settings) = storage.load_settings().await?
        {
            self.apply_settings(settings).await?;
        }
        Ok(())
    }

    async fn apply_settings(&self, settings: AppSettings) -> Result<AppSettings, QueryError> {
        let (reply, rx) = oneshot::channel();
        self.send_msg(CoreMsg::UpdateSettings {
            settings: Box::new(settings),
            reply,
        })
        .await
        .map_err(QueryError::from)?;
        rx.await
            .map_err(|_| QueryError::from(CoreError::EngineStopped))?
    }

    /// Executes a catalog query bound to the current `accountEpoch` and cancels/drops
    /// stale responses if `accountEpoch` changes while the query is in flight.
    pub async fn catalog_query(
        &self,
        request: CatalogQueryRequest,
        expected_account_epoch: Option<AccountEpoch>,
    ) -> Result<CatalogResponsePayload, QueryError> {
        let Some(catalog) = &self.deps.catalog else {
            return Err(CoreError::UnsupportedCapability(
                "catalog port is not attached".to_string(),
            )
            .into());
        };

        let ctx = self.acquire_catalog_context(false).await?;
        if let Some(expected) = expected_account_epoch.as_ref()
            && ctx.account_epoch.as_ref() != Some(expected)
        {
            return Err(CoreError::AccountMismatch(
                "request accountEpoch does not match active session".to_string(),
            )
            .into());
        }

        let response = tokio::select! {
            _ = ctx.cancel.cancelled() => {
                return Err(CoreError::AccountMismatch(
                    "catalog query cancelled by account state change".to_string(),
                ).into());
            }
            res = catalog.query(
                ctx.account_epoch.clone(),
                ctx.account_scope.clone(),
                request,
                ctx.cancel.clone(),
            ) => match res {
                Ok(response) => response,
                Err(err) => {
                    self.note_catalog_error(ctx.account_epoch.clone(), &err).await;
                    return Err(err);
                }
            },
        };

        let after_ctx = self.acquire_catalog_context(false).await?;
        if after_ctx.account_epoch != ctx.account_epoch {
            warn!("dropping stale catalog response after accountEpoch change");
            return Err(CoreError::AccountMismatch(
                "accountEpoch changed while catalog query was in flight".to_string(),
            )
            .into());
        }

        Ok(response)
    }

    /// Executes one user-confirmed account mutation at a time. Preconditions (confirmation, the
    /// request's `accountEpoch`, a verified player/catalog identity match) are checked before
    /// anything is sent. Once sent, an unknown result is `indeterminate` (never retried, never
    /// reported as an account error); a cheap read-back may then settle it.
    pub async fn mutate(
        &self,
        request: MutationRequest,
        expected_account_epoch: Option<AccountEpoch>,
    ) -> Result<MutationResult, QueryError> {
        if !request.confirmed_by_user {
            return Err(CoreError::InvalidPayload(
                "account mutation requires confirmedByUser=true".to_string(),
            )
            .into());
        }
        let Some(expected_epoch) = expected_account_epoch else {
            return Err(CoreError::InvalidPayload(
                "ui.mutation requires the envelope accountEpoch".to_string(),
            )
            .into());
        };
        let Some(catalog) = &self.deps.catalog else {
            return Err(CoreError::UnsupportedCapability(
                "catalog port is not attached".to_string(),
            )
            .into());
        };

        let _serialized = self.mutation_lock.lock().await;
        let ctx = self.acquire_catalog_context(true).await?;
        let (Some(account_epoch), Some(account_scope)) =
            (ctx.account_epoch.clone(), ctx.account_scope.clone())
        else {
            return Err(CoreError::AuthRequired("no active account session".to_string()).into());
        };
        if expected_epoch != account_epoch {
            return Err(CoreError::AccountMismatch(
                "mutation accountEpoch does not match active session".to_string(),
            )
            .into());
        }
        if !ctx.identity_matched {
            return Err(CoreError::AccountMismatch(
                "player and catalog are not verified as the same account; writes disabled"
                    .to_string(),
            )
            .into());
        }

        let kind = request.kind;
        let readback_target = match (kind, request.liked) {
            (ytm_protocol::MutationKind::SetLike, Some(liked)) => {
                Some((request.video_id.clone(), liked))
            }
            _ => None,
        };
        let sent = tokio::select! {
            _ = ctx.cancel.cancelled() => Ok(MutationResult {
                kind,
                outcome: ytm_protocol::MutationOutcome::Indeterminate,
                error: Some(CoreError::StaleContext(
                    "account state changed while the mutation was in flight".to_string(),
                ).into()),
            }),
            res = catalog.mutate(account_epoch.clone(), account_scope, request, ctx.cancel.clone()) => res,
        };
        let mut result = match sent {
            Ok(result) => result,
            Err(err) => {
                self.note_catalog_error(Some(account_epoch), &err).await;
                return Err(err);
            }
        };

        let same_account = self
            .acquire_catalog_context(false)
            .await
            .is_ok_and(|after| after.account_epoch.as_ref() == Some(&account_epoch));
        if result.outcome == ytm_protocol::MutationOutcome::Indeterminate
            && same_account
            && let Some((video_id, liked)) = readback_target
        {
            result.outcome = self
                .read_back_like(catalog.as_ref(), &account_epoch, &video_id, liked)
                .await;
        }
        if result.outcome == ytm_protocol::MutationOutcome::Confirmed && same_account {
            let resource = match kind {
                ytm_protocol::MutationKind::SetLike => "likedSongs",
                ytm_protocol::MutationKind::AddToPlaylist => "playlistTracks",
            };
            let _ = self
                .send_msg(CoreMsg::InvalidateResource {
                    resource_kind: resource.to_string(),
                })
                .await;
        }
        Ok(result)
    }

    /// Settles an indeterminate like/unlike from the first page of "Liked music" when that page
    /// shows the requested state: presence proves a like; absence proves an unlike only when
    /// that page is the whole list. Anything else stays indeterminate.
    async fn read_back_like(
        &self,
        catalog: &dyn CatalogPort,
        account_epoch: &AccountEpoch,
        video_id: &ytm_protocol::VideoId,
        liked: bool,
    ) -> ytm_protocol::MutationOutcome {
        use ytm_protocol::MutationOutcome::{Confirmed, Indeterminate};
        let Ok(ctx) = self.acquire_catalog_context(true).await else {
            return Indeterminate;
        };
        let request = CatalogQueryRequest {
            kind: ytm_protocol::CatalogQueryKind::LikedSongs,
            query: None,
            playlist_id: None,
            album_id: None,
            artist_id: None,
            cursor: None,
            limit: Some(ytm_protocol::MAX_PAGE_SIZE),
        };
        let page = match catalog
            .query(
                Some(account_epoch.clone()),
                ctx.account_scope,
                request,
                ctx.cancel,
            )
            .await
        {
            Ok(CatalogResponsePayload::LikedSongs(page))
                if page.freshness == ytm_protocol::Freshness::Fresh =>
            {
                page
            }
            _ => return Indeterminate,
        };
        // Only the requested end state counts. Seeing the opposite state may be read-after-write
        // lag, so it stays indeterminate rather than claiming the write was rejected.
        let present = page.items.iter().any(|t| &t.video_id == video_id);
        let whole_list = page.next_cursor.is_none();
        if (liked && present) || (!liked && !present && whole_list) {
            Confirmed
        } else {
            Indeterminate
        }
    }

    /// Queries lyrics for a track while dropping stale responses if the track changed in flight.
    pub async fn lyrics_query(
        &self,
        request: LyricsQueryRequest,
    ) -> Result<LyricsResult, QueryError> {
        self.lyrics_query_scoped(request, None).await
    }

    pub(crate) async fn lyrics_query_scoped(
        &self,
        request: LyricsQueryRequest,
        authorized_state: Option<&ControlRead>,
    ) -> Result<LyricsResult, QueryError> {
        let ctx = self.acquire_lyrics_context().await?;
        if !ctx.enabled {
            return Ok(LyricsResult {
                video_id: request.track.video_id,
                kind: LyricsKind::Disabled,
                source: "disabled".to_string(),
                language: None,
                match_basis: None,
                fetched_at_ms: self.clock.wall_now_ms(),
                file_offset_ms: 0,
                user_offset_ms: 0,
                synced_lines: Vec::new(),
                plain_text: None,
                error: None,
            });
        }
        let Some(lyrics) = &self.deps.lyrics else {
            return Err(CoreError::UnsupportedCapability(
                "lyrics port is not attached".to_string(),
            )
            .into());
        };

        let target_video_id = request.track.video_id.clone();
        let mut res = tokio::select! {
            _ = ctx.cancel.cancelled() => {
                return Err(CoreError::StaleContext(
                    "lyrics query cancelled due to track change".to_string(),
                ).into());
            }
            out = lyrics.query_lyrics(request, ctx.cancel.clone()) => out?,
        };
        // SPEC 7: LRCLIB first, then the official plain text from the signed-in catalog.
        if matches!(res.kind, LyricsKind::NotFound | LyricsKind::Unavailable)
            && authorized_state.is_none_or(|state| state.snapshot.account_epoch.is_some())
            && let Some(official) = self
                .official_lyrics(
                    &target_video_id,
                    &ctx.cancel,
                    authorized_state.and_then(|state| state.snapshot.account_epoch.as_ref()),
                )
                .await
        {
            res = LyricsResult {
                video_id: target_video_id.clone(),
                kind: LyricsKind::Plain,
                source: "youtube-music".to_string(),
                language: None,
                match_basis: Some(format!("youtube-music:videoId; {}", official.source)),
                fetched_at_ms: self.clock.wall_now_ms(),
                file_offset_ms: 0,
                user_offset_ms: res.user_offset_ms,
                synced_lines: Vec::new(),
                plain_text: Some(official.text),
                error: None,
            };
        }

        let after_ctx = self.acquire_lyrics_context().await?;
        if let Some(current_vid) = after_ctx.current_video_id
            && current_vid != target_video_id
        {
            return Err(CoreError::StaleContext(
                "track changed while lyrics query was in flight".to_string(),
            )
            .into());
        }

        Ok(res)
    }

    /// Official lyrics for the active account, `None` when unavailable for any reason (the
    /// LRCLIB result is then returned unchanged).
    async fn official_lyrics(
        &self,
        video_id: &ytm_protocol::VideoId,
        cancel: &CancellationToken,
        expected_account_epoch: Option<&AccountEpoch>,
    ) -> Option<crate::ports::OfficialLyrics> {
        match self
            .official_lyrics_checked(video_id, cancel, expected_account_epoch)
            .await
        {
            Ok(found) => found,
            Err(err) => {
                debug!(code = ?err.code, "official lyrics unavailable");
                None
            }
        }
    }

    pub(crate) async fn official_lyrics_checked(
        &self,
        video_id: &ytm_protocol::VideoId,
        cancel: &CancellationToken,
        expected_account_epoch: Option<&AccountEpoch>,
    ) -> Result<Option<crate::ports::OfficialLyrics>, QueryError> {
        let catalog = self.deps.catalog.as_ref().ok_or_else(|| {
            CoreError::UnsupportedCapability("catalog port is not attached".to_string())
        })?;
        let ctx = self.acquire_catalog_context(true).await?;
        let epoch = ctx.account_epoch.ok_or_else(|| {
            CoreError::AuthRequired("official lyrics require an authenticated account".to_string())
        })?;
        if expected_account_epoch.is_some_and(|expected| *expected != epoch) {
            return Err(
                CoreError::AccountMismatch("official lyrics account changed".to_string()).into(),
            );
        }
        let response = tokio::select! {
            _ = cancel.cancelled() => Err(CoreError::StaleContext("lyrics target changed".to_string()).into()),
            _ = ctx.cancel.cancelled() => Err(CoreError::AccountMismatch("official lyrics account changed".to_string()).into()),
            response = catalog.official_lyrics(epoch.clone(), video_id.clone(), ctx.cancel.clone()) => response,
        }?;
        let after = self.acquire_catalog_context(false).await?;
        if after.account_epoch.as_ref() != Some(&epoch) {
            return Err(
                CoreError::AccountMismatch("official lyrics account changed".to_string()).into(),
            );
        }
        Ok(response)
    }

    pub async fn clear_cache(&self) -> Result<(), QueryError> {
        let Some(storage) = &self.deps.storage else {
            return Err(CoreError::UnsupportedCapability(
                "storage port is not attached".to_string(),
            )
            .into());
        };
        let ctx = self.acquire_catalog_context(false).await?;
        // Reads that started before the clear must not write their results back after it.
        if let Some(catalog) = &self.deps.catalog {
            catalog.fence_cache_writes().await;
        }
        storage.clear_cache(ctx.account_scope.clone()).await?;
        if let Some(catalog) = &self.deps.catalog {
            catalog.clear_memory_cache(ctx.account_scope.as_ref())?;
        }
        // Cached pages and cover ids handed out earlier are gone; clients refetch.
        self.send_msg(CoreMsg::InvalidateResource {
            resource_kind: "catalog".to_string(),
        })
        .await
        .map_err(QueryError::from)
    }

    /// Publish an account-scoped entity change once its writer has verified the result.
    pub async fn invalidate_control_resource(
        &self,
        account_epoch: AccountEpoch,
        resource_kind: String,
        resource_id: String,
    ) -> Result<(), CoreError> {
        self.send_msg(CoreMsg::InvalidateControlResource {
            account_epoch,
            resource_kind,
            resource_id,
        })
        .await
    }

    pub(crate) async fn control_operation_changed(
        &self,
        account_epoch: Option<AccountEpoch>,
        key: &crate::control::journal::OperationKey,
    ) {
        let _ = self
            .send_msg(CoreMsg::ControlOperationChanged {
                account_epoch,
                principal: key.principal.clone(),
                operation_id: key.operation_id.clone(),
            })
            .await;
    }

    pub async fn resolve_asset(
        &self,
        asset_id: AssetId,
    ) -> Result<AssetResolvedPayload, QueryError> {
        let Some(storage) = &self.deps.storage else {
            return Err(CoreError::UnsupportedCapability(
                "storage port is not attached".to_string(),
            )
            .into());
        };
        storage.resolve_asset(asset_id).await
    }

    /// Drives deadline and TTL evaluation deterministically (useful in `MockClock` tests).
    pub async fn tick(&self) -> Result<(), CoreError> {
        let (reply, rx) = oneshot::channel();
        self.send_msg(CoreMsg::Tick { reply }).await?;
        rx.await.map_err(|_| CoreError::EngineStopped)
    }

    pub async fn shutdown(&self) -> Result<(), CoreError> {
        self.shutdown_token.cancel();
        let (reply, rx) = oneshot::channel();
        if self.send_msg(CoreMsg::Shutdown { reply }).await.is_ok() {
            let _ = rx.await;
        }
        Ok(())
    }

    async fn acquire_catalog_context(
        &self,
        require_auth: bool,
    ) -> Result<CatalogContext, QueryError> {
        let (reply, rx) = oneshot::channel();
        self.send_msg(CoreMsg::AcquireCatalogContext {
            require_auth,
            reply,
        })
        .await
        .map_err(QueryError::from)?;
        rx.await
            .map_err(|_| QueryError::from(CoreError::EngineStopped))?
    }

    async fn acquire_lyrics_context(&self) -> Result<LyricsContext, QueryError> {
        let (reply, rx) = oneshot::channel();
        self.send_msg(CoreMsg::AcquireLyricsContext { reply })
            .await
            .map_err(QueryError::from)?;
        rx.await
            .map_err(|_| QueryError::from(CoreError::EngineStopped))
    }

    async fn send_msg(&self, msg: CoreMsg) -> Result<(), CoreError> {
        match self.tx.try_send(msg) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(msg)) => {
                // Allow bounded backpressure wait up to a tiny margin, or return Busy if saturated.
                tokio::select! {
                    res = self.tx.send(msg) => res.map_err(|_| CoreError::EngineStopped),
                    _ = tokio::time::sleep(Duration::from_millis(50)) => {
                        Err(CoreError::Busy("core actor queue is saturated".to_string()))
                    }
                }
            }
            Err(mpsc::error::TrySendError::Closed(_)) => Err(CoreError::EngineStopped),
        }
    }
}

struct CoreActor {
    core_epoch: CoreEpoch,
    deps: CoreDeps,
    config: EngineConfig,
    delta_tx: broadcast::Sender<StateDelta>,
    internal_tx: mpsc::Sender<InternalEvent>,
    shutdown_token: CancellationToken,

    revision: u64,
    host_health: HostHealth,
    auth_state: AuthStatusInfo,
    player: PlayerObservedState,
    clock_model: PlaybackClock,
    capabilities: CapabilitySet,
    settings: AppSettings,
    network_state: NetworkState,

    active_bridge_epoch: Option<BridgeEpoch>,
    retired_bridge_epochs: HashSet<BridgeEpoch>,
    authorized_successor_epoch: Option<BridgeEpoch>,
    last_bridge_seq: Option<u64>,
    last_bridge_logged_in: Option<bool>,
    last_bridge_account_hint: Option<String>,
    catalog_account_hint: Option<String>,
    bridge_caps: CapabilitySet,
    /// The saved last track with the account scope it was recorded under.
    restore_candidate: Option<(AccountScope, crate::ports::LastPlayed)>,
    restore_attempted: bool,
    /// The active bridge epoch has reported playback from a healthy page at least once, so an
    /// empty page is a fact.
    bridge_playback_seen: bool,
    /// The active bridge's latest health report failed; nothing but a healthy report makes it
    /// Ready again.
    bridge_unhealthy: bool,
    restore_followup: Option<RestoreFollowup>,
    last_recorded_video: Option<ytm_protocol::VideoId>,
    /// The playback instance of the last history row; `None` after a launch restore or a new
    /// bridge epoch, whose first sighting of `last_recorded_video` continues that row.
    last_recorded_instance: Option<TrackInstanceId>,
    last_position_persist_ms: u64,
    now_playing_cover: Option<(String, Option<ytm_protocol::CoverRef>)>,

    full_queue: Option<QueueSnapshot>,
    queue_summary: QueueSummary,

    active_command: Option<ActiveCommand>,
    pending_queue: VecDeque<QueuedCommand>,
    playback_generation: u64,
    settings_revision: u64,
    last_manual_command_ms: Option<u64>,
    unreleased_gates: HashMap<String, Arc<OnceCell<OutputGateResultPayload>>>,
    last_receipt: Option<CommandReceipt>,
    dedup: CommandDeduplicator,
    /// Recent enqueue proofs (command, inserted occurrence, queue revision), newest last.
    enqueue_proofs: VecDeque<(CommandId, QueueEntryId, u64)>,
    /// Recent commands whose bridge dispatch started, newest last.
    dispatched_commands: VecDeque<CommandId>,
    /// Recent commands the bridge refused before the page acted (`dispatched: false` acks).
    page_refused_commands: VecDeque<CommandId>,
    dispatch_seq: u64,

    account_cancel: CancellationToken,
    lyrics_cancel: CancellationToken,
    pending_session_bind: Option<(AccountEpoch, CancellationToken)>,
    logout_cleanup_done: Option<CancellationToken>,
    diagnostics: CoreDiagnostics,
}

impl CoreActor {
    fn new(
        core_epoch: CoreEpoch,
        deps: CoreDeps,
        config: EngineConfig,
        delta_tx: broadcast::Sender<StateDelta>,
        internal_tx: mpsc::Sender<InternalEvent>,
        shutdown_token: CancellationToken,
    ) -> Self {
        let now_ms = deps.clock.now_ms();
        let clock_model = PlaybackClock::new(now_ms);
        let time_anchor = clock_model.snapshot_anchor(now_ms);
        Self {
            core_epoch,
            deps,
            config,
            delta_tx,
            internal_tx,
            shutdown_token,
            revision: 0,
            host_health: HostHealth {
                wayland_native: None,
                gpu_accelerated: None,
                sandbox_enabled: true,
                secret_store_backend: "uninitialized".to_string(),
                safe_storage_available: false,
                tray_available: false,
                mpris_active: false,
                core_restart_count: 0,
                last_error: None,
            },
            auth_state: AuthStatusInfo {
                state: AuthState::Unknown,
                account_epoch: None,
                account_scope: None,
                credential_revision: None,
                channel_label: None,
                player_catalog_matched: false,
                reason: None,
            },
            player: PlayerObservedState {
                connection: PlayerConnectionState::Starting,
                playback: PlaybackStatus::Idle,
                bridge_epoch: None,
                track_instance_id: None,
                current_track: None,
                current_queue_entry_id: None,
                playback_context: None,
                time_anchor,
                volume: 1.0,
                muted: false,
                shuffle: None,
                repeat: None,
                autoplay: None,
                codec: None,
                audio_mode: None,
                quality_label: None,
                drm_protected: None,
                stopped_confirmed: false,
            },
            clock_model,
            capabilities: CapabilitySet::default(),
            settings: AppSettings::default(),
            network_state: NetworkState::Unknown,
            active_bridge_epoch: None,
            retired_bridge_epochs: HashSet::new(),
            authorized_successor_epoch: None,
            last_bridge_seq: None,
            last_bridge_logged_in: None,
            last_bridge_account_hint: None,
            catalog_account_hint: None,
            bridge_caps: CapabilitySet::default(),
            restore_candidate: None,
            restore_attempted: false,
            bridge_playback_seen: false,
            bridge_unhealthy: false,
            restore_followup: None,
            last_recorded_video: None,
            last_recorded_instance: None,
            last_position_persist_ms: 0,
            now_playing_cover: None,
            full_queue: None,
            queue_summary: QueueSummary {
                queue_revision: 0,
                current_entry_id: None,
                loaded_count: 0,
                completeness: ytm_protocol::QueueCompleteness::Unknown,
                has_more: None,
                preview_entries: Vec::new(),
            },
            active_command: None,
            pending_queue: VecDeque::new(),
            playback_generation: 0,
            settings_revision: 0,
            last_manual_command_ms: None,
            unreleased_gates: HashMap::new(),
            last_receipt: None,
            dedup: CommandDeduplicator::new(),
            enqueue_proofs: VecDeque::new(),
            dispatched_commands: VecDeque::new(),
            page_refused_commands: VecDeque::new(),
            dispatch_seq: 0,
            account_cancel: CancellationToken::new(),
            lyrics_cancel: CancellationToken::new(),
            pending_session_bind: None,
            logout_cleanup_done: None,
            diagnostics: CoreDiagnostics::default(),
        }
    }

    async fn run(
        mut self,
        mut rx: mpsc::Receiver<CoreMsg>,
        mut internal_rx: mpsc::Receiver<InternalEvent>,
    ) {
        self.capabilities = self.compute_capabilities();
        if let Some(storage) = self.deps.storage.clone() {
            let internal_tx = self.internal_tx.clone();
            tokio::spawn(async move {
                let session = storage.last_session().await.unwrap_or_else(|err| {
                    warn!(code = ?err.code, "failed to load last session");
                    None
                });
                let _ = internal_tx
                    .send(InternalEvent::LastSessionLoaded {
                        session: session.map(Box::new),
                    })
                    .await;
            });
        }
        let interval_ms = self.config.tick_interval_ms.max(10);
        let mut ticker = tokio::time::interval(Duration::from_millis(interval_ms));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                biased;
                maybe_internal = internal_rx.recv() => {
                    match maybe_internal {
                        Some(ev) => self.handle_internal(ev),
                        None => break,
                    }
                }
                maybe_msg = rx.recv() => {
                    match maybe_msg {
                        Some(CoreMsg::Shutdown { reply }) => {
                            self.cancel_all_pending("core shutting down");
                            let _ = reply.send(());
                            break;
                        }
                        Some(msg) => self.handle_msg(msg),
                        None => break,
                    }
                }
                // Deadlines belong to the active command, and only playing positions need periodic writes.
                _ = ticker.tick(), if self.active_command.is_some() || self.player.playback == PlaybackStatus::Playing => {
                    self.check_timeouts();
                }
                _ = self.shutdown_token.cancelled() => {
                    self.cancel_all_pending("core cancelled");
                    break;
                }
            }
        }
        debug!(core_epoch = %self.core_epoch, "CoreActor exited");
    }

    fn current_pending_state(&self) -> PlayerPendingState {
        let mut desired_playback = None;
        let mut desired_position_ms = None;
        let mut desired_volume = None;

        if let Some(active) = &self.active_command {
            match &active.plan {
                PendingOpPlan::Play { .. } => desired_playback = Some(PlaybackStatus::Playing),
                PendingOpPlan::Pause { .. } | PendingOpPlan::Stop { .. } => {
                    desired_playback = Some(PlaybackStatus::Paused);
                }
                PendingOpPlan::Seek { position_ms, .. } => {
                    desired_position_ms = Some(*position_ms);
                }
                PendingOpPlan::SetVolume { volume } => {
                    desired_volume = Some(*volume);
                }
                _ => {}
            }
        }

        for queued in &self.pending_queue {
            match queued.payload.op {
                PlayerOp::Play => desired_playback = Some(PlaybackStatus::Playing),
                PlayerOp::Pause | PlayerOp::Stop => {
                    desired_playback = Some(PlaybackStatus::Paused);
                }
                PlayerOp::Seek => {
                    if let Some(args) = &queued.payload.args {
                        desired_position_ms = args.position_ms;
                    }
                }
                PlayerOp::SetVolume => {
                    if let Some(args) = &queued.payload.args {
                        desired_volume = args.volume;
                    }
                }
                _ => {}
            }
        }

        let count = u32::from(self.active_command.is_some())
            .saturating_add(u32::try_from(self.pending_queue.len()).unwrap_or(u32::MAX));
        let active_op = self.active_command.as_ref().map(|c| c.receipt.op);

        PlayerPendingState {
            pending_command_count: count,
            active_op,
            desired_playback,
            desired_position_ms,
            desired_volume,
            last_receipt: self.last_receipt.clone(),
        }
    }

    fn build_snapshot(&self) -> AppSnapshot {
        let now_ms = self.deps.clock.now_ms();
        let mut player = self.player.clone();
        player.time_anchor = self.clock_model.snapshot_anchor(now_ms);

        AppSnapshot {
            protocol_version: PROTOCOL_VERSION,
            core_epoch: self.core_epoch.clone(),
            account_epoch: self.auth_state.account_epoch.clone(),
            revision: self.revision,
            host_health: self.host_health.clone(),
            auth_state: self.auth_state.clone(),
            player,
            pending: self.current_pending_state(),
            queue_summary: self.queue_summary.clone(),
            capabilities: self.capabilities.clone(),
            settings: self.settings.clone(),
            network_state: self.network_state,
            mock_mode: self.config.mock_mode,
        }
    }

    fn next_revision(&mut self) -> u64 {
        self.revision = self.revision.saturating_add(1);
        self.revision
    }

    fn emit_delta(&self, delta: StateDelta) {
        let _ = self.delta_tx.send(delta);
    }

    fn emit_player_changed(&mut self) {
        let now_ms = self.deps.clock.now_ms();
        self.player.time_anchor = self.clock_model.snapshot_anchor(now_ms);
        let rev = self.next_revision();
        self.emit_delta(StateDelta::PlayerChanged {
            revision: rev,
            player: self.player.clone(),
            pending: self.current_pending_state(),
        });
    }

    fn emit_command_updated(&mut self, receipt: CommandReceipt) {
        self.last_receipt = Some(receipt.clone());
        let rev = self.next_revision();
        self.emit_delta(StateDelta::CommandUpdated {
            revision: rev,
            receipt,
        });
    }

    fn handle_msg(&mut self, msg: CoreMsg) {
        match msg {
            CoreMsg::SubscribeSnapshot { reply } => {
                let snap = self.build_snapshot();
                let rx = self.delta_tx.subscribe();
                let _ = reply.send((snap, rx));
            }
            CoreMsg::ReadControl { subscribe, reply } => {
                let _ = reply.send(ControlRead {
                    snapshot: self.build_snapshot(),
                    queue: self.full_queue.clone(),
                    captured_at_ms: self.deps.clock.wall_now_ms(),
                    playback_generation: self.playback_generation,
                    settings_revision: self.settings_revision,
                    last_manual_command_ms: self.last_manual_command_ms,
                    events: subscribe.then(|| self.delta_tx.subscribe()),
                });
            }
            CoreMsg::GetSnapshot { reply } => {
                let _ = reply.send(self.build_snapshot());
            }
            CoreMsg::GetQueueSnapshot { reply } => {
                let _ = reply.send(self.full_queue.clone());
            }
            CoreMsg::GetCommandReceipt { command_id, reply } => {
                let receipt = self.lookup_receipt(&command_id);
                let _ = reply.send(receipt);
            }
            CoreMsg::MayHaveReachedPage { command_id, reply } => {
                let _ = reply.send(
                    self.dispatched_commands.contains(&command_id)
                        && !self.page_refused_commands.contains(&command_id),
                );
            }
            CoreMsg::GetEnqueueProof { command_id, reply } => {
                let proof = self
                    .enqueue_proofs
                    .iter()
                    .find(|(id, _, _)| id == &command_id)
                    .map(|(_, entry, revision)| (entry.clone(), *revision));
                let _ = reply.send(proof);
            }
            CoreMsg::GetDiagnostics { reply } => {
                let _ = reply.send(self.diagnostics.clone());
            }
            CoreMsg::RecentReceipts { reply } => {
                let now_ms = self.deps.clock.now_ms();
                let _ = reply.send(self.dedup.recent(now_ms, 60_000, 64));
            }
            CoreMsg::SubmitPlayerCommand {
                payload,
                fence,
                reply,
            } => {
                let origin = fence.map_or(CommandOrigin::Manual, |fence| {
                    CommandOrigin::External(*fence)
                });
                self.handle_submit_command(*payload, false, origin, reply);
            }
            CoreMsg::HostHello { payload } => {
                let payload = *payload;
                self.host_health = payload.host_health;
                self.network_state = payload.network_state;
                self.config.mock_mode = payload.mock_mode;
                let rev1 = self.next_revision();
                self.emit_delta(StateDelta::HostHealthChanged {
                    revision: rev1,
                    host_health: self.host_health.clone(),
                });
                let rev2 = self.next_revision();
                self.emit_delta(StateDelta::NetworkChanged {
                    revision: rev2,
                    network_state: self.network_state,
                });
                self.refresh_capabilities();
            }
            CoreMsg::HostNetworkChanged { network } => {
                if self.network_state != network {
                    self.network_state = network;
                    let rev = self.next_revision();
                    self.emit_delta(StateDelta::NetworkChanged {
                        revision: rev,
                        network_state: network,
                    });
                }
            }
            CoreMsg::HostPowerResume => {
                let now_ms = self.deps.clock.now_ms();
                self.clock_model.on_power_resume(now_ms);
                if self.player.connection == PlayerConnectionState::Ready {
                    self.player.connection = PlayerConnectionState::Stale;
                }
                self.emit_player_changed();
                if let Some(bridge_epoch) = self.active_bridge_epoch.clone() {
                    self.trigger_snapshot_rebuild(bridge_epoch);
                }
            }
            CoreMsg::HostWindowVisibility { payload } => {
                debug!(
                    main_visible = payload.main_visible,
                    main_focused = payload.main_focused,
                    player_visible = payload.player_visible,
                    "host window visibility updated"
                );
            }
            CoreMsg::HostBridgeLifecycle { payload } => {
                self.handle_bridge_lifecycle(payload);
            }
            CoreMsg::SessionMaterial { payload, reply } => {
                self.handle_session_material(*payload, reply);
            }
            CoreMsg::Logout { reply } => {
                self.handle_logout(reply);
            }
            CoreMsg::AuthExpired {
                account_epoch,
                reason,
            } => {
                if account_epoch.is_none() || account_epoch == self.auth_state.account_epoch {
                    self.handle_auth_expired(reason);
                }
            }
            CoreMsg::BridgeReady {
                seq,
                payload,
                reply,
            } => {
                let accepted = self.handle_bridge_ready(seq, *payload);
                let _ = reply.send(accepted);
            }
            CoreMsg::BridgePlaybackObserved {
                seq,
                payload,
                reply,
            } => {
                let accepted = self.handle_bridge_playback_observed(seq, *payload);
                let _ = reply.send(accepted);
            }
            CoreMsg::BridgeQueueObserved {
                seq,
                payload,
                reply,
            } => {
                let accepted = self.handle_bridge_queue_observed(seq, *payload);
                let _ = reply.send(accepted);
            }
            CoreMsg::BridgeHealth {
                seq,
                payload,
                reply,
            } => {
                let accepted = self.handle_bridge_health(seq, payload);
                let _ = reply.send(accepted);
            }
            CoreMsg::BridgeCommandAck {
                seq,
                payload,
                reply,
            } => {
                let accepted = self.handle_bridge_command_ack(seq, payload);
                let _ = reply.send(accepted);
            }
            CoreMsg::BridgeError {
                seq,
                bridge_epoch,
                error,
                reply,
            } => {
                let accepted = self.handle_bridge_error(seq, bridge_epoch, *error);
                let _ = reply.send(accepted);
            }
            CoreMsg::UpdateSettings { settings, reply } => {
                let settings = *settings;
                if let Some(lyrics) = &self.deps.lyrics {
                    lyrics.set_enabled(settings.lrclib_enabled);
                }
                self.settings = settings.clone();
                self.settings_revision = self.settings_revision.saturating_add(1);
                let rev = self.next_revision();
                self.emit_delta(StateDelta::SettingsChanged {
                    revision: rev,
                    settings: settings.clone(),
                });
                self.refresh_capabilities();
                let _ = reply.send(Ok(settings));
            }
            CoreMsg::AcquireCatalogContext {
                require_auth,
                reply,
            } => {
                if require_auth && self.auth_state.state != AuthState::Authenticated {
                    let _ = reply.send(Err(CoreError::AuthRequired(
                        "operation requires authenticated session".to_string(),
                    )
                    .into()));
                    return;
                }
                let _ = reply.send(Ok(CatalogContext {
                    account_epoch: self.auth_state.account_epoch.clone(),
                    account_scope: self.auth_state.account_scope.clone(),
                    identity_matched: self.auth_state.player_catalog_matched,
                    cancel: self.account_cancel.clone(),
                }));
            }
            CoreMsg::AcquireLyricsContext { reply } => {
                let _ = reply.send(LyricsContext {
                    enabled: self.settings.lrclib_enabled,
                    current_video_id: self
                        .player
                        .current_track
                        .as_ref()
                        .map(|t| t.video_id.clone()),
                    cancel: self.lyrics_cancel.clone(),
                });
            }
            CoreMsg::Tick { reply } => {
                self.check_timeouts();
                let _ = reply.send(());
            }
            CoreMsg::InvalidateResource { resource_kind } => {
                if resource_kind == "catalog" {
                    // Cache cleared: the now-playing cover's URL mapping is gone, so cache it
                    // again or its assetId stops resolving until the track changes.
                    self.now_playing_cover = None;
                    if let Some(mut track) = self.player.current_track.take() {
                        self.apply_now_playing_cover(&mut track);
                        self.player.current_track = Some(track);
                    }
                }
                let rev = self.next_revision();
                self.emit_delta(StateDelta::ResourceInvalidated {
                    revision: rev,
                    resource_kind,
                    resource_id: None,
                });
            }
            CoreMsg::InvalidateControlResource {
                account_epoch,
                resource_kind,
                resource_id,
            } => {
                if Some(&account_epoch) == self.auth_state.account_epoch.as_ref() {
                    let revision = self.next_revision();
                    self.emit_delta(StateDelta::ResourceInvalidated {
                        revision,
                        resource_kind,
                        resource_id: Some(resource_id),
                    });
                }
            }
            CoreMsg::ControlOperationChanged {
                account_epoch,
                principal,
                operation_id,
            } => {
                if account_epoch == self.auth_state.account_epoch {
                    let revision = self.next_revision();
                    self.emit_delta(StateDelta::ControlOperationChanged {
                        revision,
                        principal,
                        operation_id,
                    });
                }
            }
            CoreMsg::Shutdown { .. } => {}
        }
    }

    fn lookup_receipt(&mut self, command_id: &CommandId) -> Option<CommandReceipt> {
        if let Some(active) = &self.active_command
            && &active.receipt.command_id == command_id
        {
            return Some(active.receipt.clone());
        }
        if let Some(queued) = self
            .pending_queue
            .iter()
            .find(|q| &q.receipt.command_id == command_id)
        {
            return Some(queued.receipt.clone());
        }
        let now_ms = self.deps.clock.now_ms();
        self.dedup.get(command_id, now_ms)
    }

    fn handle_submit_command(
        &mut self,
        payload: PlayerCommandPayload,
        keep_paused: bool,
        origin: CommandOrigin,
        reply: oneshot::Sender<Result<CommandReceipt, QueryError>>,
    ) {
        let now_ms = self.deps.clock.now_ms();
        let wall_ms = self.deps.clock.wall_now_ms();
        let (manual, playback_fence) = match origin {
            CommandOrigin::Manual => (true, None),
            CommandOrigin::Internal => (false, None),
            CommandOrigin::External(fence) => {
                if let Err(err) = self.validate_playback_fence(&fence, true) {
                    let _ = reply.send(Err(err));
                    return;
                }
                (false, Some(fence))
            }
        };
        if manual {
            self.last_manual_command_ms = Some(now_ms);
        }

        // 1. Bind the ID to the original request (even when a Toggle is later resolved).
        match self.dedup.check(&payload, now_ms) {
            Ok(None) => {}
            Ok(Some(receipt)) => {
                let _ = reply.send(Ok(receipt));
                return;
            }
            Err(err) => {
                let _ = reply.send(Err(err));
                return;
            }
        }

        // 2. Validate payload shape. Capability is checked by the bridge against the live page
        //    on every command (its refusal ack fails the receipt), not against a stale snapshot.
        if let Err(err) = validate_command_payload(&payload) {
            let receipt = CommandReceipt {
                command_id: payload.command_id.clone(),
                op: payload.op,
                status: CommandLifecycleStatus::Failed,
                accepted_at_ms: wall_ms,
                observed_at_ms: None,
                error: Some(err.clone()),
                deduplicated: false,
            };
            self.dedup.record(&payload, receipt.clone(), now_ms);
            self.emit_command_updated(receipt);
            let _ = reply.send(Err(err));
            return;
        }

        // 3. Validate precondition context (active bridge, stale trackInstanceId / queueRevision).
        if self.active_bridge_epoch.is_none()
            || matches!(
                self.player.connection,
                PlayerConnectionState::Disconnected | PlayerConnectionState::Incompatible
            )
        {
            let err = QueryError {
                code: QueryErrorCode::BridgeIncompatible,
                retriable: true,
                retry_after_ms: Some(500),
                safe_message: "player bridge is not ready".to_string(),
                request_id: None,
            };
            let receipt = CommandReceipt {
                command_id: payload.command_id.clone(),
                op: payload.op,
                status: CommandLifecycleStatus::Failed,
                accepted_at_ms: wall_ms,
                observed_at_ms: None,
                error: Some(err.clone()),
                deduplicated: false,
            };
            self.dedup.record(&payload, receipt.clone(), now_ms);
            self.emit_command_updated(receipt);
            let _ = reply.send(Err(err));
            return;
        }

        if let Err(err) = self.validate_command_context(&payload) {
            let receipt = CommandReceipt {
                command_id: payload.command_id.clone(),
                op: payload.op,
                status: CommandLifecycleStatus::Failed,
                accepted_at_ms: wall_ms,
                observed_at_ms: None,
                error: Some(err.clone()),
                deduplicated: false,
            };
            self.dedup.record(&payload, receipt.clone(), now_ms);
            self.emit_command_updated(receipt);
            let _ = reply.send(Err(err));
            return;
        }

        let took_over = manual
            && takes_over_external_playback(payload.op)
            && self.take_over_external_playback("local playback control took over");

        // 4. Coalesce rapid `seek` and `setVolume` in queue.
        if is_coalescible(payload.op) {
            self.coalesce_queued_commands(payload.op, now_ms, playback_fence.is_some());
        }

        // 5. Enforce bounded queue limit.
        let total_pending =
            usize::from(self.active_command.is_some()).saturating_add(self.pending_queue.len());
        if total_pending >= MAX_COMMAND_QUEUE {
            let _ = reply.send(Err(QueryError {
                code: QueryErrorCode::Busy,
                retriable: true,
                retry_after_ms: Some(100),
                safe_message: "player command queue limit reached".to_string(),
                request_id: None,
            }));
            return;
        }

        // 6. Accept and enqueue.
        let receipt = CommandReceipt {
            command_id: payload.command_id.clone(),
            op: payload.op,
            status: CommandLifecycleStatus::Accepted,
            accepted_at_ms: wall_ms,
            observed_at_ms: None,
            error: None,
            deduplicated: false,
        };
        self.dedup.record(&payload, receipt.clone(), now_ms);
        let command = QueuedCommand {
            receipt: receipt.clone(),
            payload,
            keep_paused,
            playback_fence,
        };
        if took_over && matches!(receipt.op, PlayerOp::Pause | PlayerOp::Stop) {
            self.pending_queue.push_front(command);
        } else {
            self.pending_queue.push_back(command);
        }
        self.emit_command_updated(receipt.clone());

        if self.active_command.is_none() {
            self.start_next_queued_command(Some(reply));
        } else {
            self.emit_player_changed();
            let supersedes_restore_seek = is_restore_play_intent(receipt.op);
            let _ = reply.send(Ok(receipt));
            if supersedes_restore_seek {
                self.evaluate_restore_seek();
            }
        }
    }

    fn validate_playback_fence(
        &self,
        fence: &PlaybackFence,
        check_resource: bool,
    ) -> Result<(), QueryError> {
        let authority_changed = fence.cancel.is_cancelled()
            || fence.deadline_ms <= self.deps.clock.now_ms()
            || fence.generation != self.playback_generation
            || Some(&fence.bridge_epoch) != self.active_bridge_epoch.as_ref()
            || fence.account_scope != self.auth_state.account_scope
            || fence.account_epoch != self.auth_state.account_epoch;
        let resource_changed = check_resource
            && (fence
                .track_instance_id
                .as_ref()
                .is_some_and(|id| Some(id) != self.player.track_instance_id.as_ref())
                || fence
                    .queue_revision
                    .is_some_and(|revision| revision != self.queue_summary.queue_revision));
        if authority_changed || resource_changed {
            return Err(external_playback_stale(
                "external playback permission or context changed",
            ));
        }
        Ok(())
    }

    fn validate_command_context(&self, payload: &PlayerCommandPayload) -> Result<(), QueryError> {
        let stale = |msg: &str| QueryError {
            code: QueryErrorCode::StaleContext,
            retriable: false,
            retry_after_ms: None,
            safe_message: msg.to_string(),
            request_id: None,
        };

        if let Some(args) = &payload.args {
            if let Some(req_tid) = &args.track_instance_id
                && self.player.track_instance_id.as_ref() != Some(req_tid)
            {
                return Err(stale("stale trackInstanceId"));
            }
            if let Some(req_rev) = args.queue_revision
                && req_rev != self.queue_summary.queue_revision
            {
                return Err(stale("stale queueRevision"));
            }
            // An append whose end position cannot be proven would only time out as unknown.
            if payload.op == PlayerOp::Enqueue
                && args.enqueue_position == Some(EnqueuePosition::Last)
                && !self
                    .full_queue
                    .as_ref()
                    .is_some_and(crate::commands::explicit_end_loaded)
            {
                return Err(stale(
                    "the end of this queue is not loaded, so an append cannot be verified",
                ));
            }
            if matches!(
                payload.op,
                PlayerOp::RemoveQueueEntry | PlayerOp::MoveQueueEntry | PlayerOp::ClearUpcoming
            ) && crate::commands::QueueEditPlan::capture(
                payload.op,
                args,
                self.full_queue.as_ref(),
                &self.player,
            )
            .is_none()
            {
                return Err(stale(
                    "queue edit requires an eligible occurrence in a complete bounded queue",
                ));
            }
        }
        Ok(())
    }

    /// A play with no track loaded can never be observed: right after start-up the official
    /// page has not restored its own queue yet, and its play control does nothing. Such a play
    /// fails at once (retriable) instead of timing out as indeterminate.
    fn check_something_to_play(&self, payload: &PlayerCommandPayload) -> Result<(), QueryError> {
        let is_play = match payload.op {
            PlayerOp::Play => true,
            PlayerOp::Toggle => resolve_toggle_op(self.player.playback) == PlayerOp::Play,
            _ => false,
        };
        if is_play && self.player.current_track.is_none() {
            return Err(QueryError {
                code: QueryErrorCode::UnavailableTrack,
                retriable: true,
                retry_after_ms: Some(1_000),
                safe_message: "no track is loaded in the player yet".to_string(),
                request_id: None,
            });
        }
        Ok(())
    }

    fn coalesce_queued_commands(&mut self, op: PlayerOp, now_ms: u64, external: bool) {
        let mut retained = VecDeque::with_capacity(self.pending_queue.len());
        let mut cancelled = Vec::new();
        while let Some(mut item) = self.pending_queue.pop_front() {
            if item.receipt.op == op && (!external || item.playback_fence.is_some()) {
                item.receipt.status = CommandLifecycleStatus::Cancelled;
                item.receipt.error = Some(QueryError {
                    code: QueryErrorCode::StaleContext,
                    retriable: false,
                    retry_after_ms: None,
                    safe_message: format!("coalesced by newer {op:?} command"),
                    request_id: None,
                });
                cancelled.push(item.receipt);
            } else {
                retained.push_back(item);
            }
        }
        self.pending_queue = retained;
        for receipt in cancelled {
            self.dedup.upsert(receipt.clone(), now_ms);
            self.emit_command_updated(receipt);
        }
    }

    fn start_next_queued_command(
        &mut self,
        reply: Option<oneshot::Sender<Result<CommandReceipt, QueryError>>>,
    ) {
        if self.active_command.is_some() {
            return;
        }
        let Some(queued) = self.pending_queue.pop_front() else {
            return;
        };
        let Some(bridge_epoch) = self.active_bridge_epoch.clone() else {
            let err = QueryError {
                code: QueryErrorCode::BridgeIncompatible,
                retriable: true,
                retry_after_ms: Some(500),
                safe_message: "no active bridgeEpoch".to_string(),
                request_id: None,
            };
            let mut r = queued.receipt;
            r.status = CommandLifecycleStatus::Failed;
            r.error = Some(err.clone());
            let now_ms = self.deps.clock.now_ms();
            self.dedup.upsert(r.clone(), now_ms);
            self.emit_command_updated(r);
            if let Some(tx) = reply {
                let _ = tx.send(Err(err));
            }
            return;
        };

        let now_ms = self.deps.clock.now_ms();
        // Re-check context at dispatch time (e.g. track changed while queued).
        if let Some(fence) = &queued.playback_fence
            && let Err(err) = self.validate_playback_fence(fence, true)
        {
            self.finish_interrupted(queued.receipt, false, err.clone());
            if let Some(tx) = reply {
                let _ = tx.send(Err(err));
            }
            self.start_next_queued_command(None);
            return;
        }
        if let Err(err) = self
            .validate_command_context(&queued.payload)
            .and_then(|()| self.check_something_to_play(&queued.payload))
        {
            let mut r = queued.receipt;
            r.status = CommandLifecycleStatus::Failed;
            r.error = Some(err.clone());
            self.dedup.upsert(r.clone(), now_ms);
            self.emit_command_updated(r);
            self.emit_player_changed();
            if let Some(tx) = reply {
                let _ = tx.send(Err(err));
            }
            self.start_next_queued_command(None);
            return;
        }

        let (plan, bridge_payload, needs_gate_tx) =
            self.build_plan(&queued.payload, queued.keep_paused);
        let timeout_ms = if matches!(
            queued.payload.op,
            PlayerOp::PlayTarget | PlayerOp::PlayCollection
        ) {
            self.config.play_target_timeout_ms
        } else {
            self.config.control_timeout_ms
        };

        let playback_fence = queued.playback_fence.map(|mut fence| {
            fence.cancel = fence.cancel.child_token();
            fence
        });
        let active = ActiveCommand {
            receipt: queued.receipt.clone(),
            plan,
            bound_bridge_epoch: bridge_epoch.clone(),
            successor_handoff_used: false,
            deadline_ms: now_ms.saturating_add(timeout_ms),
            dispatch_started: false,
            playback_fence,
        };
        self.active_command = Some(active);
        self.emit_player_changed();

        self.dispatch_seq = self.dispatch_seq.saturating_add(1);
        let dispatch_token = self.dispatch_seq;
        let command_id = queued.receipt.command_id;

        if let Some(tx_id) = needs_gate_tx {
            let gate = self.deps.output_gate.clone();
            let internal_tx = self.internal_tx.clone();
            let gate_timeout = Duration::from_millis(self.config.dispatch_timeout_ms);
            tokio::spawn(async move {
                let result = match tokio::time::timeout(
                    gate_timeout,
                    gate.set_output_gated(tx_id.clone(), true),
                )
                .await
                {
                    Ok(res) => res,
                    Err(_) => Err(QueryError {
                        code: QueryErrorCode::Timeout,
                        retriable: false,
                        retry_after_ms: None,
                        safe_message: "output gate engage timed out".to_string(),
                        request_id: None,
                    }),
                };
                let _ = internal_tx
                    .send(InternalEvent::OutputGateCompleted {
                        dispatch_token,
                        command_id,
                        transaction_id: tx_id,
                        gated: true,
                        result,
                        reply,
                        bridge_payload_after_gate: Some(Box::new(bridge_payload)),
                    })
                    .await;
            });
        } else {
            self.spawn_bridge_dispatch(
                dispatch_token,
                command_id,
                bridge_epoch,
                bridge_payload,
                reply,
            );
        }
    }

    fn build_plan(
        &self,
        payload: &PlayerCommandPayload,
        keep_paused: bool,
    ) -> (PendingOpPlan, PlayerCommandPayload, Option<String>) {
        let args = payload.args.clone();
        match payload.op {
            PlayerOp::Play => (
                PendingOpPlan::Play {
                    expected_track_instance_id: args.and_then(|a| a.track_instance_id),
                },
                payload.clone(),
                None,
            ),
            PlayerOp::Pause => (
                PendingOpPlan::Pause {
                    expected_track_instance_id: args.and_then(|a| a.track_instance_id),
                },
                payload.clone(),
                None,
            ),
            PlayerOp::Toggle => {
                // Resolve Toggle against latest observed playback state; never dispatch raw Toggle.
                let resolved_op = resolve_toggle_op(self.player.playback);
                let mut resolved_payload = payload.clone();
                resolved_payload.op = resolved_op;
                let plan = match resolved_op {
                    PlayerOp::Pause => PendingOpPlan::Pause {
                        expected_track_instance_id: self.player.track_instance_id.clone(),
                    },
                    _ => PendingOpPlan::Play {
                        expected_track_instance_id: self.player.track_instance_id.clone(),
                    },
                };
                (plan, resolved_payload, None)
            }
            PlayerOp::Stop => {
                let bound = args
                    .and_then(|a| a.track_instance_id)
                    .or_else(|| self.player.track_instance_id.clone());
                let already_paused = matches!(
                    self.player.playback,
                    PlaybackStatus::Paused | PlaybackStatus::Idle | PlaybackStatus::Ended
                );
                if already_paused {
                    let seek_payload = PlayerCommandPayload {
                        command_id: payload.command_id.clone(),
                        op: PlayerOp::Seek,
                        args: Some(PlayerCommandArgs {
                            track_instance_id: bound.clone(),
                            position_ms: Some(0),
                            volume: None,
                            muted: None,
                            target: None,
                            shuffle: None,
                            repeat: None,
                            autoplay: None,
                            enqueue_position: None,
                            queue_entry_id: None,
                            before_queue_entry_id: None,
                            queue_revision: None,
                        }),
                    };
                    (
                        PendingOpPlan::Stop {
                            bound_track_instance_id: bound,
                            phase: StopPhase::SeekPending,
                        },
                        seek_payload,
                        None,
                    )
                } else {
                    let pause_payload = PlayerCommandPayload {
                        command_id: payload.command_id.clone(),
                        op: PlayerOp::Pause,
                        args: Some(PlayerCommandArgs {
                            track_instance_id: bound.clone(),
                            position_ms: None,
                            volume: None,
                            muted: None,
                            target: None,
                            shuffle: None,
                            repeat: None,
                            autoplay: None,
                            enqueue_position: None,
                            queue_entry_id: None,
                            before_queue_entry_id: None,
                            queue_revision: None,
                        }),
                    };
                    (
                        PendingOpPlan::Stop {
                            bound_track_instance_id: bound,
                            phase: StopPhase::PausePending,
                        },
                        pause_payload,
                        None,
                    )
                }
            }
            PlayerOp::Seek => {
                let bound = args
                    .as_ref()
                    .and_then(|a| a.track_instance_id.clone())
                    .or_else(|| self.player.track_instance_id.clone());
                let pos = args.and_then(|a| a.position_ms).unwrap_or(0);
                (
                    PendingOpPlan::Seek {
                        bound_track_instance_id: bound,
                        position_ms: pos,
                    },
                    payload.clone(),
                    None,
                )
            }
            PlayerOp::SetVolume => {
                let vol = args.and_then(|a| a.volume).unwrap_or(self.player.volume);
                (
                    PendingOpPlan::SetVolume { volume: vol },
                    payload.clone(),
                    None,
                )
            }
            PlayerOp::SetMuted => {
                let muted = args.and_then(|a| a.muted).unwrap_or(!self.player.muted);
                (PendingOpPlan::SetMuted { muted }, payload.clone(), None)
            }
            PlayerOp::Next | PlayerOp::Previous => {
                let is_paused = matches!(
                    self.player.playback,
                    PlaybackStatus::Paused | PlaybackStatus::Idle | PlaybackStatus::Ended
                );
                let (paused_skip, gate_tx) = new_gate_transaction(is_paused);
                let plan = if payload.op == PlayerOp::Next {
                    PendingOpPlan::Next {
                        prev_track_instance_id: self.player.track_instance_id.clone(),
                        prev_queue_entry_id: self.player.current_queue_entry_id.clone(),
                        paused_skip,
                    }
                } else {
                    PendingOpPlan::Previous {
                        prev_track_instance_id: self.player.track_instance_id.clone(),
                        prev_queue_entry_id: self.player.current_queue_entry_id.clone(),
                        prev_position_ms: self
                            .clock_model
                            .estimated_position_ms(self.deps.clock.now_ms()),
                        paused_skip,
                    }
                };
                (plan, payload.clone(), gate_tx)
            }
            PlayerOp::PlayTarget | PlayerOp::PlayCollection => {
                let target = args
                    .and_then(|a| a.target)
                    .expect("validated playTarget has target");
                let (paused_skip, gate_tx) = new_gate_transaction(keep_paused);
                let restore_position_ms = self
                    .restore_followup
                    .as_ref()
                    .filter(|restore| restore.play_command == payload.command_id)
                    .map(|restore| restore.position_ms)
                    .filter(|&position_ms| position_ms > SEEK_TOLERANCE_MS);
                (
                    PendingOpPlan::PlayTarget {
                        target,
                        require_context: payload.op == PlayerOp::PlayCollection,
                        prev_track_instance_id: self.player.track_instance_id.clone(),
                        paused_skip,
                        restore_position_ms,
                    },
                    payload.clone(),
                    gate_tx,
                )
            }
            PlayerOp::SetShuffle => {
                let shuffle = args.and_then(|a| a.shuffle).unwrap_or(false);
                (PendingOpPlan::SetShuffle { shuffle }, payload.clone(), None)
            }
            PlayerOp::SetRepeat => {
                let repeat = args
                    .and_then(|a| a.repeat)
                    .unwrap_or(ytm_protocol::RepeatMode::Off);
                (PendingOpPlan::SetRepeat { repeat }, payload.clone(), None)
            }
            PlayerOp::SetAutoplay => {
                let enabled = args
                    .and_then(|args| args.autoplay)
                    .expect("validated setAutoplay has a target state");
                (
                    PendingOpPlan::SetAutoplay { enabled },
                    payload.clone(),
                    None,
                )
            }
            PlayerOp::Enqueue => {
                let args = args.expect("validated enqueue has args");
                let target = args.target.expect("validated enqueue has target");
                let position = args
                    .enqueue_position
                    .expect("validated enqueue has position");
                (
                    PendingOpPlan::Enqueue(EnqueuePlan::capture(
                        target,
                        position,
                        self.full_queue.as_ref(),
                        self.player.track_instance_id.clone(),
                    )),
                    payload.clone(),
                    None,
                )
            }
            PlayerOp::SelectQueueEntry => {
                let entry_id = args
                    .and_then(|a| a.queue_entry_id)
                    .expect("validated selectQueueEntry has queueEntryId");
                let video_id = self.full_queue.as_ref().and_then(|queue| {
                    queue
                        .entries
                        .iter()
                        .find(|entry| entry.entry_id == entry_id)
                        .map(|entry| entry.video_id.clone())
                });
                (
                    PendingOpPlan::SelectQueueEntry { entry_id, video_id },
                    payload.clone(),
                    None,
                )
            }
            PlayerOp::RemoveQueueEntry | PlayerOp::MoveQueueEntry | PlayerOp::ClearUpcoming => {
                let proof = crate::commands::QueueEditPlan::capture(
                    payload.op,
                    args.as_ref().expect("validated queue edit has args"),
                    self.full_queue.as_ref(),
                    &self.player,
                )
                .expect("validated queue edit has an occurrence proof");
                (PendingOpPlan::EditQueue(proof), payload.clone(), None)
            }
        }
    }

    fn spawn_bridge_dispatch(
        &mut self,
        dispatch_token: u64,
        command_id: CommandId,
        bridge_epoch: BridgeEpoch,
        payload: PlayerCommandPayload,
        reply: Option<oneshot::Sender<Result<CommandReceipt, QueryError>>>,
    ) {
        let fence = self.active_command.as_ref().and_then(|active| {
            active
                .playback_fence
                .as_ref()
                .map(|fence| (fence.clone(), active.dispatch_started))
        });
        if let Some((fence, already_dispatched)) = &fence
            && let Err(err) = self.validate_playback_fence(fence, !already_dispatched)
        {
            self.interrupt_active(err.clone());
            self.emit_player_changed();
            if let Some(reply) = reply {
                let _ = reply.send(Err(err));
            }
            self.start_next_queued_command(None);
            return;
        }
        self.diagnostics.bridge_dispatches = self.diagnostics.bridge_dispatches.saturating_add(1);
        if let Some(active) = self.active_command.as_mut()
            && active.receipt.command_id == command_id
        {
            active.dispatch_started = true;
        }
        if !self.dispatched_commands.contains(&command_id) {
            if self.dispatched_commands.len() >= MAX_DISPATCHED_COMMANDS {
                self.dispatched_commands.pop_front();
            }
            self.dispatched_commands.push_back(command_id.clone());
        }
        // An explicit request for audible playback releases gates left behind by failed silent
        // transitions; otherwise the host would stay muted forever (it unmutes only when no
        // gate transaction remains).
        let release_gates = if matches!(payload.op, PlayerOp::Play | PlayerOp::SelectQueueEntry)
            || (matches!(payload.op, PlayerOp::PlayTarget | PlayerOp::PlayCollection)
                && self
                    .active_command
                    .as_ref()
                    .is_some_and(|a| a.plan.paused_skip().is_none()))
        {
            self.unreleased_gates.clone()
        } else {
            HashMap::new()
        };
        let gate = self.deps.output_gate.clone();
        let bridge = self.deps.bridge.clone();
        let internal_tx = self.internal_tx.clone();
        let clock = self.deps.clock.clone();
        let is_restore_seek = matches!(
            self.active_command
                .as_ref()
                .and_then(|active| active.plan.paused_skip())
                .map(|skip| &skip.phase),
            Some(PausedSkipPhase::AwaitingRestoreSeek { .. })
        );
        let dispatch_timeout = Duration::from_millis(if is_restore_seek {
            self.config.dispatch_timeout_ms.min(RESTORE_SEEK_TIMEOUT_MS)
        } else {
            self.config.dispatch_timeout_ms
        });
        tokio::spawn(async move {
            for (tx_id, released) in release_gates {
                release_orphan_gate(&*gate, tx_id, &released, dispatch_timeout, &internal_tx).await;
            }
            let dispatch = async {
                match tokio::time::timeout(
                    dispatch_timeout,
                    bridge.send_player_command(bridge_epoch, payload),
                )
                .await
                {
                    Ok(res) => res,
                    Err(_) => Err(QueryError {
                        code: QueryErrorCode::Timeout,
                        retriable: false,
                        retry_after_ms: None,
                        safe_message: "bridge dispatch timed out".to_string(),
                        request_id: None,
                    }),
                }
            };
            let (result, not_sent) = if let Some((fence, _)) = fence {
                if fence.deadline_ms <= clock.now_ms() {
                    (
                        Err(external_playback_stale(
                            "external playback permission expired before send",
                        )),
                        true,
                    )
                } else if fence.cancel.is_cancelled() {
                    (
                        Err(external_playback_stale(
                            "external playback permission was revoked before send",
                        )),
                        true,
                    )
                } else {
                    // Once the send future has been polled, a later revocation may follow a write.
                    let result = tokio::select! {
                        biased;
                        _ = fence.cancel.cancelled() => Err(external_playback_stale("external playback permission was revoked during send")),
                        result = dispatch => result,
                    };
                    (result, false)
                }
            } else {
                (dispatch.await, false)
            };
            let _ = internal_tx
                .send(InternalEvent::BridgeDispatchCompleted {
                    dispatch_token,
                    command_id,
                    result,
                    not_sent,
                    reply,
                })
                .await;
        });
    }

    fn handle_internal(&mut self, ev: InternalEvent) {
        match ev {
            InternalEvent::OrphanGateReleased(transaction_id) => {
                self.unreleased_gates.remove(&transaction_id);
            }
            InternalEvent::OutputGateCompleted {
                dispatch_token,
                command_id,
                transaction_id,
                gated,
                result,
                reply,
                bridge_payload_after_gate,
            } => {
                let matches_active = self.active_command.as_ref().is_some_and(|a| {
                    a.receipt.command_id == command_id && dispatch_token == self.dispatch_seq
                });
                if !matches_active {
                    if gated {
                        if result.as_ref().is_ok_and(|r| r.applied) {
                            // The command ended before its page dispatch. Only the host gate ran.
                            let released = self
                                .unreleased_gates
                                .entry(transaction_id.clone())
                                .or_default()
                                .clone();
                            let gate = self.deps.output_gate.clone();
                            let tx = self.internal_tx.clone();
                            let timeout = Duration::from_millis(self.config.dispatch_timeout_ms);
                            tokio::spawn(async move {
                                release_orphan_gate(
                                    &*gate,
                                    transaction_id,
                                    &released,
                                    timeout,
                                    &tx,
                                )
                                .await;
                            });
                        } else if result.is_err() {
                            self.unreleased_gates.entry(transaction_id).or_default();
                        }
                    } else if result
                        .as_ref()
                        .is_ok_and(|r| r.applied && r.transaction_id == transaction_id)
                    {
                        self.unreleased_gates.remove(&transaction_id);
                    }
                    if let Some(tx) = reply {
                        let _ = tx.send(Err(QueryError {
                            code: QueryErrorCode::StaleContext,
                            retriable: false,
                            retry_after_ms: None,
                            safe_message: "command superseded before gate completed".to_string(),
                            request_id: None,
                        }));
                    }
                    return;
                }

                if gated {
                    match result {
                        Ok(gate_res) if gate_res.applied => {
                            if let Some(skip) = self
                                .active_command
                                .as_mut()
                                .and_then(|a| a.plan.paused_skip_mut())
                            {
                                skip.gate_engaged = true;
                            }
                            let bridge_epoch = self
                                .active_command
                                .as_ref()
                                .map(|a| a.bound_bridge_epoch.clone());
                            if let (Some(ep), Some(payload)) =
                                (bridge_epoch, bridge_payload_after_gate)
                            {
                                self.spawn_bridge_dispatch(
                                    dispatch_token,
                                    command_id,
                                    ep,
                                    *payload,
                                    reply,
                                );
                            }
                        }
                        Ok(gate_res) => {
                            // Gate refused/failed; do NOT dispatch Next/Previous and do NOT auto-clear unknown gate.
                            let err = QueryError {
                                code: QueryErrorCode::UnsupportedCapability,
                                retriable: false,
                                retry_after_ms: None,
                                safe_message: gate_res
                                    .error
                                    .unwrap_or_else(|| "output gate was not applied".to_string()),
                                request_id: None,
                            };
                            let receipt = self.finalize_active_command(
                                CommandLifecycleStatus::Failed,
                                Some(err.clone()),
                            );
                            if let Some(tx) = reply {
                                match receipt {
                                    Some(_) => {
                                        let _ = tx.send(Err(err));
                                    }
                                    None => {
                                        let _ = tx.send(Err(err));
                                    }
                                }
                            }
                        }
                        Err(err) => {
                            // An IPC timeout can hide a successful host mute. Wait for explicit playback.
                            self.unreleased_gates.entry(transaction_id).or_default();
                            self.finalize_active_command(
                                CommandLifecycleStatus::Failed,
                                Some(err.clone()),
                            );
                            if let Some(tx) = reply {
                                let _ = tx.send(Err(err));
                            }
                        }
                    }
                } else {
                    // Ungating completion after paused Next/Previous confirmed target track + Paused.
                    match result {
                        Ok(_) => {
                            self.unreleased_gates.remove(&transaction_id);
                            self.player.stopped_confirmed = false;
                            self.finalize_active_command(CommandLifecycleStatus::Observed, None);
                        }
                        Err(err) => {
                            self.finalize_active_command(CommandLifecycleStatus::Failed, Some(err));
                        }
                    }
                }
            }
            InternalEvent::BridgeDispatchCompleted {
                dispatch_token,
                command_id,
                result,
                not_sent,
                reply,
            } => {
                let matches_active = self.active_command.as_ref().is_some_and(|a| {
                    a.receipt.command_id == command_id && dispatch_token == self.dispatch_seq
                });
                if !matches_active {
                    if let Some(tx) = reply {
                        let _ = tx.send(Err(QueryError {
                            code: QueryErrorCode::StaleContext,
                            retriable: false,
                            retry_after_ms: None,
                            safe_message: "command cancelled during dispatch".to_string(),
                            request_id: None,
                        }));
                    }
                    return;
                }

                match result {
                    Ok(()) => {
                        if let Some(PausedSkipPhase::AwaitingRestoreSeek {
                            dispatch_pending, ..
                        }) = self
                            .active_command
                            .as_mut()
                            .and_then(|active| active.plan.paused_skip_mut())
                            .map(|skip| &mut skip.phase)
                        {
                            *dispatch_pending = false;
                            self.evaluate_restore_seek();
                            return;
                        }
                        let receipt = {
                            let active = self.active_command.as_mut().expect("checked above");
                            if active.receipt.status == CommandLifecycleStatus::Accepted {
                                active.receipt.status = CommandLifecycleStatus::Dispatched;
                            }
                            active.receipt.clone()
                        };
                        let now_ms = self.deps.clock.now_ms();
                        self.dedup.upsert(receipt.clone(), now_ms);
                        self.emit_command_updated(receipt.clone());
                        if let Some(tx) = reply {
                            let _ = tx.send(Ok(receipt));
                        }
                        // Re-evaluate in case an observation already arrived concurrently.
                        self.evaluate_current_active();
                    }
                    Err(err) => {
                        if self.abandon_restore_seek() {
                            return;
                        }
                        if not_sent && let Some(active) = self.active_command.as_mut() {
                            active.dispatch_started = false;
                        }
                        // Bridge dispatch failed; for paused Next/Previous, never auto-clear unknown gate.
                        if matches!(
                            err.code,
                            QueryErrorCode::StaleContext | QueryErrorCode::Timeout
                        ) && self
                            .active_command
                            .as_ref()
                            .is_some_and(|active| active.playback_fence.is_some())
                        {
                            self.interrupt_active(err.clone());
                            self.emit_player_changed();
                            self.start_next_queued_command(None);
                        } else {
                            self.finalize_active_command(
                                CommandLifecycleStatus::Failed,
                                Some(err.clone()),
                            );
                        }
                        if let Some(tx) = reply {
                            let _ = tx.send(Err(err));
                        }
                    }
                }
            }
            InternalEvent::CoverCached { remote_url, result } => {
                self.handle_cover_cached(remote_url, result);
            }
            InternalEvent::LastSessionLoaded { session } => {
                self.handle_last_session_loaded(session.map(|s| *s));
            }
            InternalEvent::SessionMaterialApplied {
                cancel,
                account_epoch,
                credential_revision,
                channel_label,
                same_account,
                result,
                reply,
            } => {
                if cancel.is_cancelled() {
                    let _ = reply.send(Err(QueryError {
                        code: QueryErrorCode::StaleContext,
                        retriable: false,
                        retry_after_ms: None,
                        safe_message: "superseded by newer session update".to_string(),
                        request_id: None,
                    }));
                    return;
                }
                self.pending_session_bind = None;
                match result {
                    Ok(identity) => {
                        self.apply_authenticated_session(
                            account_epoch,
                            identity,
                            credential_revision,
                            channel_label,
                            same_account,
                        );
                        let _ = reply.send(Ok(()));
                    }
                    Err(err) => {
                        if matches!(
                            err.code,
                            QueryErrorCode::AuthExpired | QueryErrorCode::AuthRequired
                        ) {
                            self.handle_auth_expired(Some(err.safe_message.clone()));
                        }
                        let _ = reply.send(Err(err));
                    }
                }
            }
        }
    }

    fn finalize_active_command(
        &mut self,
        status: CommandLifecycleStatus,
        error: Option<QueryError>,
    ) -> Option<CommandReceipt> {
        let mut active = self.active_command.take()?;
        self.remember_unreleased_gate(&active, status);
        let now_ms = self.deps.clock.now_ms();
        let wall_ms = self.deps.clock.wall_now_ms();
        active.receipt.status = status;
        active.receipt.error = error;
        if status == CommandLifecycleStatus::Observed {
            active.receipt.observed_at_ms = Some(wall_ms);
        }
        let receipt = active.receipt.clone();
        self.dedup.upsert(receipt.clone(), now_ms);
        self.emit_command_updated(receipt.clone());
        self.emit_player_changed();
        self.continue_restore(&receipt);
        self.start_next_queued_command(None);
        Some(receipt)
    }

    fn evaluate_current_active(&mut self) {
        if self.evaluate_restore_seek() {
            return;
        }
        let eval = {
            let Some(active) = &self.active_command else {
                return;
            };
            if active.receipt.status != CommandLifecycleStatus::Dispatched {
                return;
            }
            let now_ms = self.deps.clock.now_ms();
            let estimated_pos_ms = self.clock_model.estimated_position_ms(now_ms);
            evaluate_active_command(
                &active.plan,
                &self.player,
                &self.queue_summary,
                self.full_queue.as_ref(),
                estimated_pos_ms,
            )
        };

        match eval {
            PlanEvaluation::StillPending => {}
            PlanEvaluation::AdvanceStopToSeek => {
                let (command_id, bridge_epoch, bound_tid) = {
                    let active = self.active_command.as_mut().expect("checked above");
                    if let PendingOpPlan::Stop {
                        bound_track_instance_id,
                        phase,
                    } = &mut active.plan
                    {
                        *phase = StopPhase::SeekPending;
                        (
                            active.receipt.command_id.clone(),
                            active.bound_bridge_epoch.clone(),
                            bound_tid_clone(bound_track_instance_id),
                        )
                    } else {
                        return;
                    }
                };
                self.dispatch_seq = self.dispatch_seq.saturating_add(1);
                let token = self.dispatch_seq;
                let seek_payload = PlayerCommandPayload {
                    command_id: command_id.clone(),
                    op: PlayerOp::Seek,
                    args: Some(PlayerCommandArgs {
                        track_instance_id: bound_tid,
                        position_ms: Some(0),
                        volume: None,
                        muted: None,
                        target: None,
                        shuffle: None,
                        repeat: None,
                        autoplay: None,
                        enqueue_position: None,
                        queue_entry_id: None,
                        before_queue_entry_id: None,
                        queue_revision: None,
                    }),
                };
                self.spawn_bridge_dispatch(token, command_id, bridge_epoch, seek_payload, None);
            }
            PlanEvaluation::AdvancePausedSkipToPause {
                new_track_instance_id,
            } => {
                if !self.try_restore_seek(new_track_instance_id.as_ref()) {
                    self.pause_silent_transition(new_track_instance_id);
                }
            }
            PlanEvaluation::ReadyToComplete {
                sets_stopped_confirmed,
                clears_stopped_confirmed,
                ungate_transaction_id,
            } => {
                if let Some(tx_id) = ungate_transaction_id {
                    let command_id = {
                        let active = self.active_command.as_mut().expect("checked above");
                        if let Some(skip) = active.plan.paused_skip_mut() {
                            if skip.phase == PausedSkipPhase::ReadyToUngate {
                                return;
                            }
                            skip.phase = PausedSkipPhase::ReadyToUngate;
                        }
                        active.receipt.command_id.clone()
                    };
                    self.dispatch_seq = self.dispatch_seq.saturating_add(1);
                    let token = self.dispatch_seq;
                    let gate = self.deps.output_gate.clone();
                    let internal_tx = self.internal_tx.clone();
                    let gate_timeout = Duration::from_millis(self.config.dispatch_timeout_ms);
                    let released = self
                        .unreleased_gates
                        .entry(tx_id.clone())
                        .or_default()
                        .clone();
                    tokio::spawn(async move {
                        let result = release_gate(&*gate, &tx_id, &released, gate_timeout).await;
                        let _ = internal_tx
                            .send(InternalEvent::OutputGateCompleted {
                                dispatch_token: token,
                                command_id,
                                transaction_id: tx_id,
                                gated: false,
                                result,
                                reply: None,
                                bridge_payload_after_gate: None,
                            })
                            .await;
                    });
                } else {
                    if sets_stopped_confirmed {
                        self.player.stopped_confirmed = true;
                    } else if clears_stopped_confirmed {
                        self.player.stopped_confirmed = false;
                    }
                    self.record_enqueue_proof();
                    self.finalize_active_command(CommandLifecycleStatus::Observed, None);
                }
            }
            PlanEvaluation::Failed(err) => {
                self.finalize_active_command(CommandLifecycleStatus::Failed, Some(err));
            }
        }
    }

    fn record_enqueue_proof(&mut self) {
        let Some(ActiveCommand {
            plan: PendingOpPlan::Enqueue(Some(plan)),
            receipt,
            ..
        }) = &self.active_command
        else {
            return;
        };
        let Some(queue) = self.full_queue.as_ref() else {
            return;
        };
        if let Some(entry) = plan.observed_entry(queue, &self.player) {
            if self.enqueue_proofs.len() >= MAX_ENQUEUE_PROOFS {
                self.enqueue_proofs.pop_front();
            }
            self.enqueue_proofs.push_back((
                receipt.command_id.clone(),
                entry.entry_id.clone(),
                queue.queue_revision,
            ));
        }
    }

    fn try_restore_seek(&mut self, track_instance_id: Option<&TrackInstanceId>) -> bool {
        let Some(track_instance_id) = track_instance_id.cloned() else {
            return false;
        };
        let now_ms = self.deps.clock.now_ms();
        let Some(ActiveCommand {
            plan:
                PendingOpPlan::PlayTarget {
                    restore_position_ms: Some(position_ms),
                    paused_skip: Some(skip),
                    ..
                },
            receipt,
            bound_bridge_epoch,
            ..
        }) = self.active_command.as_mut()
        else {
            return false;
        };
        let position_ms = self
            .player
            .time_anchor
            .duration_ms
            .map_or(*position_ms, |duration| (*position_ms).min(duration));
        if seek_within_tolerance(
            self.clock_model.anchor().position_ms,
            position_ms,
            self.player.playback,
        ) || self
            .pending_queue
            .iter()
            .any(|queued| is_restore_play_intent(queued.receipt.op))
        {
            return false;
        }
        // A freshly paused YTM player may not apply seekTo. Seek whilst the target
        // is playing silently, then pause and release the same output transaction.
        let seek_id = internal_command_id("restore-seek");
        skip.phase = PausedSkipPhase::AwaitingRestoreSeek {
            command_id: seek_id.clone(),
            track_instance_id: track_instance_id.clone(),
            position_ms,
            deadline_ms: now_ms.saturating_add(RESTORE_SEEK_TIMEOUT_MS),
            dispatch_pending: true,
        };
        let command_id = receipt.command_id.clone();
        let bridge_epoch = bound_bridge_epoch.clone();
        let mut args = empty_args();
        args.track_instance_id = Some(track_instance_id);
        args.position_ms = Some(position_ms);
        self.dispatch_seq = self.dispatch_seq.saturating_add(1);
        self.spawn_bridge_dispatch(
            self.dispatch_seq,
            command_id,
            bridge_epoch,
            PlayerCommandPayload {
                command_id: seek_id,
                op: PlayerOp::Seek,
                args: Some(args),
            },
            None,
        );
        true
    }

    /// A restore seek is best-effort. Its observation, short deadline, or a queued
    /// play action continues the same silent transition to pause and ungate.
    fn evaluate_restore_seek(&mut self) -> bool {
        let Some(PausedSkipPhase::AwaitingRestoreSeek {
            track_instance_id,
            position_ms,
            deadline_ms,
            dispatch_pending,
            ..
        }) = self
            .active_command
            .as_ref()
            .and_then(|active| active.plan.paused_skip())
            .map(|skip| &skip.phase)
        else {
            return false;
        };
        // Finish dispatch first so the seek cannot reach the page after our pause.
        if *dispatch_pending {
            return true;
        }
        if self.player.track_instance_id.as_ref() != Some(track_instance_id) {
            self.finalize_active_command(
                CommandLifecycleStatus::Failed,
                Some(QueryError {
                    code: QueryErrorCode::StaleContext,
                    retriable: false,
                    retry_after_ms: None,
                    safe_message: "trackInstanceId changed during restore seek".into(),
                    request_id: None,
                }),
            );
        } else if seek_within_tolerance(
            self.clock_model.anchor().position_ms,
            *position_ms,
            self.player.playback,
        ) || self.deps.clock.now_ms() >= *deadline_ms
            || self
                .pending_queue
                .iter()
                .any(|queued| is_restore_play_intent(queued.receipt.op))
        {
            self.pause_silent_transition(Some(track_instance_id.clone()));
        }
        true
    }

    fn abandon_restore_seek(&mut self) -> bool {
        let Some(PausedSkipPhase::AwaitingRestoreSeek {
            deadline_ms,
            dispatch_pending,
            ..
        }) = self
            .active_command
            .as_mut()
            .and_then(|active| active.plan.paused_skip_mut())
            .map(|skip| &mut skip.phase)
        else {
            return false;
        };
        *deadline_ms = 0;
        *dispatch_pending = false;
        self.evaluate_restore_seek()
    }

    fn pause_silent_transition(&mut self, track_instance_id: Option<TrackInstanceId>) {
        let active = self
            .active_command
            .as_mut()
            .expect("active silent transition");
        let skip = active.plan.paused_skip_mut().expect("output gate required");
        skip.phase = PausedSkipPhase::AwaitingPausedConfirmation {
            new_track_instance_id: track_instance_id.clone(),
        };
        let command_id = active.receipt.command_id.clone();
        let bridge_epoch = active.bound_bridge_epoch.clone();
        let mut args = empty_args();
        args.track_instance_id = track_instance_id;
        self.dispatch_seq = self.dispatch_seq.saturating_add(1);
        self.spawn_bridge_dispatch(
            self.dispatch_seq,
            command_id.clone(),
            bridge_epoch,
            PlayerCommandPayload {
                command_id,
                op: PlayerOp::Pause,
                args: Some(args),
            },
            None,
        );
    }

    fn check_timeouts(&mut self) {
        let now_ms = self.deps.clock.now_ms();
        self.dedup.purge_expired(now_ms);
        if self.player.playback == PlaybackStatus::Playing {
            self.persist_position(false);
        }
        if let Some(fence) = self
            .active_command
            .as_ref()
            .and_then(|active| active.playback_fence.as_ref())
            && (fence.cancel.is_cancelled() || fence.deadline_ms <= now_ms)
        {
            self.interrupt_active(external_playback_stale(
                "external playback permission ended",
            ));
            self.emit_player_changed();
            self.start_next_queued_command(None);
            return;
        }
        self.evaluate_restore_seek();

        let Some(expired) = self.active_command.as_ref().filter(|active| {
            !is_terminal_status(active.receipt.status) && now_ms >= active.deadline_ms
        }) else {
            return;
        };
        let expired_id = expired.receipt.command_id.clone();

        // Before settling at Indeterminate, verify against the latest snapshot/observed state.
        self.evaluate_current_active();
        let Some(active) = self
            .active_command
            .as_ref()
            .filter(|active| active.receipt.command_id == expired_id)
        else {
            return;
        };
        let op = active.receipt.op;

        // If a paused Next/Previous timed out while gated and the new track started playing,
        // keep controlled pause best-effort and NEVER auto-clear an unknown gate.
        if let Some(active) = &self.active_command
            && active.plan.paused_skip().is_some()
            && matches!(
                self.player.playback,
                PlaybackStatus::Playing | PlaybackStatus::Buffering
            )
            && let Some(bridge_epoch) = self.active_bridge_epoch.clone()
        {
            let pause_cmd = PlayerCommandPayload {
                command_id: CommandId::new(format!("safety-pause-{}", Uuid::new_v4()))
                    .expect("valid uuid"),
                op: PlayerOp::Pause,
                args: Some(PlayerCommandArgs {
                    track_instance_id: self.player.track_instance_id.clone(),
                    position_ms: None,
                    volume: None,
                    muted: None,
                    target: None,
                    shuffle: None,
                    repeat: None,
                    autoplay: None,
                    enqueue_position: None,
                    queue_entry_id: None,
                    before_queue_entry_id: None,
                    queue_revision: None,
                }),
            };
            let bridge = self.deps.bridge.clone();
            tokio::spawn(async move {
                let _ = bridge.send_player_command(bridge_epoch, pause_cmd).await;
            });
        }

        // Never blindly retry non-idempotent commands; settle at Indeterminate on timeout.
        let message = if is_non_idempotent(op) {
            format!("non-idempotent command {op:?} timed out; settled as indeterminate")
        } else {
            format!("command {op:?} timed out waiting for player observation")
        };
        warn!(?op, "{}", message);
        let err = QueryError {
            code: QueryErrorCode::Timeout,
            retriable: false,
            retry_after_ms: None,
            safe_message: message,
            request_id: None,
        };
        self.finalize_active_command(CommandLifecycleStatus::Indeterminate, Some(err));
    }

    /// Records a gate that stayed engaged because its transaction ended without release.
    fn remember_unreleased_gate(&mut self, active: &ActiveCommand, status: CommandLifecycleStatus) {
        if status != CommandLifecycleStatus::Observed
            && let Some(skip) = active.plan.paused_skip()
            && skip.gate_engaged
        {
            self.unreleased_gates
                .entry(skip.transaction_id.clone())
                .or_default();
        }
    }

    /// Release the exclusive playback slot immediately. A granted frame may still be in
    /// transit, so the receipt stays indeterminate and cannot authorize an automatic retry.
    fn interrupt_active(&mut self, error: QueryError) {
        if let Some(active) = self.active_command.take() {
            if let Some(fence) = &active.playback_fence {
                fence.cancel.cancel();
            }
            let status = if active.dispatch_started {
                CommandLifecycleStatus::Indeterminate
            } else {
                CommandLifecycleStatus::Cancelled
            };
            self.remember_unreleased_gate(&active, status);
            self.finish_interrupted(active.receipt, active.dispatch_started, error);
        }
    }

    fn finish_interrupted(
        &mut self,
        mut receipt: CommandReceipt,
        dispatched: bool,
        error: QueryError,
    ) {
        receipt.status = if dispatched {
            CommandLifecycleStatus::Indeterminate
        } else {
            CommandLifecycleStatus::Cancelled
        };
        receipt.error = Some(error);
        self.dedup.upsert(receipt.clone(), self.deps.clock.now_ms());
        self.emit_command_updated(receipt);
    }

    fn take_over_external_playback(&mut self, reason: &str) -> bool {
        let released_active = self.invalidate_external_playback(reason);
        let revision = self.next_revision();
        self.emit_delta(StateDelta::ManualTakeover {
            revision,
            generation: self.playback_generation,
        });
        released_active
    }

    fn invalidate_external_playback(&mut self, reason: &str) -> bool {
        self.playback_generation += 1;
        let error = external_playback_stale(reason);
        let released_active = self
            .active_command
            .as_ref()
            .is_some_and(|active| active.playback_fence.is_some());
        if released_active {
            self.interrupt_active(error.clone());
        }
        let old_queue = std::mem::take(&mut self.pending_queue);
        let old_len = old_queue.len();
        for queued in old_queue {
            if queued.playback_fence.is_some() {
                self.finish_interrupted(queued.receipt, false, error.clone());
            } else {
                self.pending_queue.push_back(queued);
            }
        }
        if released_active || self.pending_queue.len() != old_len {
            self.emit_player_changed();
        }
        released_active
    }

    /// Ends every pending command. A command whose frame may already have reached the page
    /// becomes `indeterminate`; only commands that never left the core are `cancelled`.
    fn cancel_all_pending(&mut self, reason: &str) {
        let cancel_err = external_playback_stale(reason);
        self.interrupt_active(cancel_err.clone());
        while let Some(queued) = self.pending_queue.pop_front() {
            self.finish_interrupted(queued.receipt, false, cancel_err.clone());
        }
        self.restore_followup = None;
        self.emit_player_changed();
    }

    fn handle_bridge_lifecycle(&mut self, payload: BridgeLifecyclePayload) {
        match payload.event.as_str() {
            "successorAuthorized" | "playTargetAuthorized" => {
                if payload.is_music_origin == Some(false) {
                    self.mark_bridge_lost(&payload.event);
                    return;
                }
                if let Some(next_epoch) = payload.bridge_epoch {
                    self.authorized_successor_epoch = Some(next_epoch.clone());
                    if let Some(active) = self.active_command.as_mut()
                        && matches!(active.plan, PendingOpPlan::PlayTarget { .. })
                        && !active.successor_handoff_used
                    {
                        active.bound_bridge_epoch = next_epoch;
                        active.successor_handoff_used = true;
                    }
                }
            }
            "crashed" | "unloaded" | "disconnected" | "rebuilding" | "navigatedUnexpected" => {
                self.mark_bridge_lost(&payload.event);
            }
            // Any other page event that leaves the music origin (consent, sign-in, a blanked
            // page) means no bridge can answer until a new bridge.ready arrives. Mock mode has
            // no page (its player stays on about:blank).
            other if payload.is_music_origin == Some(false) && !self.config.mock_mode => {
                if self.player.connection != PlayerConnectionState::Disconnected {
                    self.mark_bridge_lost(other);
                }
            }
            other => {
                debug!(
                    event = other,
                    "ignoring informational bridge lifecycle event"
                );
            }
        }
    }

    /// The page document that was playing is gone (crash, navigation away, rebuild): nothing
    /// plays any more, so a last "playing" observation must not outlive it (MPRIS, UI).
    fn mark_bridge_lost(&mut self, event: &str) {
        let new_conn = if event == "rebuilding" {
            PlayerConnectionState::Recovering
        } else {
            PlayerConnectionState::Disconnected
        };
        self.invalidate_bridge(new_conn, &format!("bridge lifecycle event: {event}"));
    }

    fn clear_page_media_observations(&mut self) {
        self.player.playback_context = None;
        self.player.autoplay = None;
        self.player.codec = None;
        self.player.audio_mode = None;
        self.player.quality_label = None;
        self.player.drm_protected = None;
    }

    fn invalidate_bridge(&mut self, new_conn: PlayerConnectionState, reason: &str) {
        self.authorized_successor_epoch = None;
        if let Some(epoch) = self.active_bridge_epoch.take() {
            self.retired_bridge_epochs.insert(epoch);
        }
        self.last_bridge_seq = None;
        self.bridge_playback_seen = false;
        self.bridge_unhealthy = false;
        self.player.connection = new_conn;
        self.player.bridge_epoch = None;
        self.clear_page_media_observations();
        if matches!(
            self.player.playback,
            PlaybackStatus::Playing | PlaybackStatus::Buffering
        ) {
            self.player.playback = PlaybackStatus::Paused;
        }
        self.clock_model.on_connection_changed(
            new_conn,
            self.player.playback,
            self.deps.clock.now_ms(),
        );
        self.bridge_caps = CapabilitySet {
            play_pause: CapabilitySupport::Unsupported,
            seek: CapabilitySupport::Unsupported,
            next: CapabilitySupport::Unsupported,
            previous: CapabilitySupport::Unsupported,
            volume: CapabilitySupport::Unsupported,
            shuffle: CapabilitySupport::Unsupported,
            repeat: CapabilitySupport::Unsupported,
            queue_mirror: CapabilitySupport::Unsupported,
            queue_enqueue: CapabilitySupport::Unsupported,
            queue_reorder: CapabilitySupport::Unsupported,
            queue_remove: CapabilitySupport::Unsupported,
            queue_autoplay: None,
            queue_clear: None,
            ..CapabilitySet::default()
        };
        self.last_bridge_logged_in = None;
        self.last_bridge_account_hint = None;
        if self.auth_state.player_catalog_matched {
            self.recompute_player_catalog_matched();
            let revision = self.next_revision();
            self.emit_delta(StateDelta::AuthChanged {
                revision,
                auth_state: self.auth_state.clone(),
            });
        }
        self.refresh_capabilities();
        self.cancel_all_pending(reason);
    }

    fn handle_session_material(
        &mut self,
        payload: SessionMaterialPayload,
        reply: oneshot::Sender<Result<(), QueryError>>,
    ) {
        let same_account = self.auth_state.account_epoch.as_ref() == Some(&payload.account_epoch);

        if !same_account && self.auth_state.account_epoch.is_some() {
            self.invalidate_account_scoped_state("account changed");
        }

        if let Some(catalog) = self.deps.catalog.clone() {
            let internal_tx = self.internal_tx.clone();
            let account_epoch = payload.account_epoch.clone();
            let credential_revision = payload.credential_revision.clone();
            let channel_label = payload.channel_label.clone();
            let cancel = CancellationToken::new();
            if let Some((_, previous)) = self
                .pending_session_bind
                .replace((account_epoch.clone(), cancel.clone()))
            {
                previous.cancel();
            }
            let cleanup = self.logout_cleanup_done.clone();
            tokio::spawn(async move {
                if let Some(done) = cleanup {
                    done.cancelled().await;
                }
                let res = catalog
                    .apply_session_material(payload, cancel.clone())
                    .await;
                let _ = internal_tx
                    .send(InternalEvent::SessionMaterialApplied {
                        cancel,
                        account_epoch,
                        credential_revision,
                        channel_label,
                        same_account,
                        result: res,
                        reply,
                    })
                    .await;
            });
        } else {
            // No catalog attached (mock/tests): the session is unverified and scoped per epoch.
            let identity = CatalogIdentity {
                account_scope: AccountScope::new(format!("scope-{}", payload.account_epoch))
                    .unwrap_or_else(|_| AccountScope::new("scope-active").expect("literal valid")),
                account_hint: None,
            };
            self.apply_authenticated_session(
                payload.account_epoch,
                identity,
                payload.credential_revision,
                payload.channel_label,
                same_account,
            );
            let _ = reply.send(Ok(()));
        }
    }

    fn apply_authenticated_session(
        &mut self,
        account_epoch: AccountEpoch,
        identity: CatalogIdentity,
        credential_revision: CredentialRevision,
        channel_label: Option<String>,
        same_account: bool,
    ) {
        let scope_changed = self.auth_state.account_scope.as_ref() != Some(&identity.account_scope);
        if !same_account || scope_changed {
            self.invalidate_external_playback("account changed");
        }
        self.auth_state.state = AuthState::Authenticated;
        self.auth_state.account_epoch = Some(account_epoch);
        self.auth_state.account_scope = Some(identity.account_scope);
        self.auth_state.credential_revision = Some(credential_revision);
        self.auth_state.channel_label = channel_label;
        self.auth_state.reason = None;
        self.catalog_account_hint = identity.account_hint;
        self.recompute_player_catalog_matched();
        let bound_scope = self.auth_state.account_scope.clone();
        if let Some((_, dropped)) = self
            .restore_candidate
            .take_if(|(scope, _)| Some(&*scope) != bound_scope.as_ref())
            && self.last_recorded_instance.is_none()
            && self.last_recorded_video.as_ref() == Some(&dropped.track.video_id)
        {
            self.last_recorded_video = None;
        }

        let rev = self.next_revision();
        self.emit_delta(StateDelta::AuthChanged {
            revision: rev,
            auth_state: self.auth_state.clone(),
        });
        if !same_account || scope_changed {
            let rev_inv = self.next_revision();
            self.emit_delta(StateDelta::ResourceInvalidated {
                revision: rev_inv,
                resource_kind: "accountState".to_string(),
                resource_id: None,
            });
        }
        self.refresh_capabilities();
        self.maybe_start_restore();
    }

    /// Restores the last active account scope so cached data can be browsed (stale, unverified)
    /// before any session material arrives, e.g. on an offline start.
    fn handle_last_session_loaded(&mut self, session: Option<LastSession>) {
        let Some(session) = session else {
            return;
        };
        if self.auth_state.state == AuthState::SignedOut
            || self
                .auth_state
                .account_scope
                .as_ref()
                .is_some_and(|scope| scope != &session.account_scope)
        {
            return;
        }
        if self.auth_state.account_scope.is_none()
            && self.auth_state.state != AuthState::Authenticated
        {
            self.auth_state.account_scope = Some(session.account_scope.clone());
            let rev = self.next_revision();
            self.emit_delta(StateDelta::AuthChanged {
                revision: rev,
                auth_state: self.auth_state.clone(),
            });
            self.refresh_capabilities();
        }
        if let Some(last) = &session.last_played
            && self.last_recorded_video.is_none()
        {
            // The saved track is the newest history row; the page restoring it itself
            // continues that row instead of adding one.
            self.last_recorded_video = Some(last.track.video_id.clone());
        }
        self.restore_candidate = session
            .last_played
            .map(|last| (session.account_scope, last));
        self.maybe_start_restore();
    }

    /// SPEC default "restore the most recent track and position, paused": once the first bridge
    /// has reported playback with nothing loaded and the saved track's account is the bound
    /// one, play the last track behind the output gate so it settles at the saved position
    /// before pausing; `autoplayOnLaunch` adds a final explicit play.
    fn maybe_start_restore(&mut self) {
        if self.restore_attempted
            || self.active_bridge_epoch.is_none()
            || !self.bridge_playback_seen
        {
            return;
        }
        if self.player.current_track.is_some() {
            // The page loaded a track itself: keep it, and never restore over a later empty page.
            self.restore_attempted = true;
            self.restore_candidate = None;
            return;
        }
        if self.auth_state.state != AuthState::Authenticated
            || self.player.connection != PlayerConnectionState::Ready
            || self.active_command.is_some()
            || !self.pending_queue.is_empty()
        {
            return;
        }
        let Some((scope, last)) = self.restore_candidate.take() else {
            return;
        };
        if self.auth_state.account_scope.as_ref() != Some(&scope) {
            return;
        }
        self.restore_attempted = true;
        // The restored track is already the newest history row with its saved position: do not
        // record it again, and do not overwrite that position until the restore has settled.
        self.last_recorded_video = Some(last.track.video_id.clone());
        self.last_recorded_instance = None;
        self.last_position_persist_ms = self.deps.clock.now_ms();
        let target = last.playback_ref.unwrap_or(ytm_protocol::PlaybackRef {
            video_id: last.track.video_id.clone(),
            playlist_id: None,
            playlist_entry_id: None,
            browse_id: None,
            queue_entry_id: None,
            start_index: None,
        });
        let command_id = internal_command_id("restore-play");
        self.restore_followup = Some(RestoreFollowup {
            play_command: command_id.clone(),
            video_id: target.video_id.clone(),
            position_ms: last.position_ms,
            autoplay: self.settings.autoplay_on_launch,
        });
        let mut args = empty_args();
        args.target = Some(target);
        info!("restoring last played track paused");
        self.submit_internal(
            PlayerCommandPayload {
                command_id,
                op: PlayerOp::PlayTarget,
                args: Some(args),
            },
            true,
        );
    }

    /// Start configured autoplay once silent restoration has settled, unless the user
    /// already queued an action.
    fn continue_restore(&mut self, receipt: &CommandReceipt) {
        let Some(followup) = self.restore_followup.as_ref() else {
            return;
        };
        if receipt.command_id != followup.play_command {
            return;
        }
        let followup = self.restore_followup.take().expect("checked above");
        if receipt.status != CommandLifecycleStatus::Observed
            || self.player.current_track.as_ref().map(|t| &t.video_id) != Some(&followup.video_id)
        {
            warn!(status = ?receipt.status, "paused restore did not settle; leaving player as is");
            return;
        }
        if followup.autoplay && self.pending_queue.is_empty() {
            let mut args = empty_args();
            args.track_instance_id = self.player.track_instance_id.clone();
            self.submit_internal(
                PlayerCommandPayload {
                    command_id: internal_command_id("restore-autoplay"),
                    op: PlayerOp::Play,
                    args: Some(args),
                },
                false,
            );
        }
    }

    /// True while the launch restore's silent playTarget is still running; its transient
    /// play/pause near 0 must not replace the saved position.
    fn restore_play_active(&self) -> bool {
        self.restore_followup.as_ref().is_some_and(|f| {
            self.active_command
                .as_ref()
                .is_some_and(|a| a.receipt.command_id == f.play_command)
        })
    }

    fn submit_internal(&mut self, payload: PlayerCommandPayload, keep_paused: bool) {
        let (reply, _ignored) = oneshot::channel();
        self.handle_submit_command(payload, keep_paused, CommandOrigin::Internal, reply);
    }

    /// Appends the newly observed playback instance to the account's local history (bounded in
    /// storage). Another play of the same video is a new row; a changed queue entry of the same
    /// instance is not.
    fn record_track_change(&mut self, position_ms: u64) {
        let (Some(storage), Some(scope), Some(epoch), Some(track)) = (
            self.deps.storage.clone(),
            self.auth_state.account_scope.clone(),
            self.auth_state.account_epoch.clone(),
            self.player.current_track.clone(),
        ) else {
            return;
        };
        if self.auth_state.state != AuthState::Authenticated {
            return;
        }
        let instance = self.player.track_instance_id.clone();
        let same_play = match &self.last_recorded_instance {
            Some(last) => instance.as_ref() == Some(last),
            None => self.last_recorded_video.as_ref() == Some(&track.video_id),
        };
        self.last_recorded_instance = instance;
        if same_play {
            return;
        }
        self.last_recorded_video = Some(track.video_id.clone());
        self.last_position_persist_ms = self.deps.clock.now_ms();
        let playback_ref = match self.active_command.as_ref().map(|a| &a.plan) {
            Some(PendingOpPlan::PlayTarget {
                target,
                require_context,
                ..
            }) if target.video_id == track.video_id
                && (!require_context
                    || crate::commands::collection_context_matches(target, &self.player)) =>
            {
                Some(ytm_protocol::PlaybackRef {
                    queue_entry_id: None,
                    ..target.clone()
                })
            }
            _ => None,
        };
        let played = crate::ports::LastPlayed {
            track,
            playback_ref,
            position_ms,
        };
        tokio::spawn(async move {
            if let Err(err) = storage.record_playback(scope, epoch, played).await {
                debug!(code = ?err.code, "recent playback not recorded");
            }
        });
    }

    /// Persists the playing position of the recorded track every `POSITION_PERSIST_MS` and on
    /// pause, so the next launch can restore it.
    fn persist_position(&mut self, force: bool) {
        let now_ms = self.deps.clock.now_ms();
        if self.restore_play_active()
            || (!force
                && now_ms.saturating_sub(self.last_position_persist_ms) < POSITION_PERSIST_MS)
        {
            return;
        }
        let (Some(storage), Some(scope), Some(epoch), Some(video_id)) = (
            self.deps.storage.clone(),
            self.auth_state.account_scope.clone(),
            self.auth_state.account_epoch.clone(),
            self.last_recorded_video.clone(),
        ) else {
            return;
        };
        if self.player.current_track.as_ref().map(|t| &t.video_id) != Some(&video_id) {
            return;
        }
        self.last_position_persist_ms = now_ms;
        let position_ms = self.clock_model.estimated_position_ms(now_ms);
        tokio::spawn(async move {
            let _ = storage
                .update_playback_position(scope, epoch, video_id, position_ms)
                .await;
        });
    }

    fn clear_catalog_session(&mut self, expected_epoch: Option<AccountEpoch>) {
        if let Some((_, cancel)) = self.pending_session_bind.take_if(|(epoch, _)| {
            expected_epoch
                .as_ref()
                .is_none_or(|expected| expected == epoch)
        }) {
            cancel.cancel();
        }
        if let Some(catalog) = &self.deps.catalog
            && let Err(err) = catalog.clear_session_material(expected_epoch)
        {
            warn!(code = ?err.code, "failed to clear catalog session");
        }
    }

    fn handle_logout(&mut self, reply: oneshot::Sender<Result<(), QueryError>>) {
        // A bind can have registered its SQLite epoch before this actor receives the identity.
        let pending_epoch = self
            .pending_session_bind
            .as_ref()
            .map(|(epoch, _)| epoch.clone());
        self.clear_catalog_session(None);
        let prev_scope = self.auth_state.account_scope.clone();
        self.catalog_account_hint = None;
        self.restore_candidate = None;
        self.invalidate_account_scoped_state("user logged out");

        self.auth_state = AuthStatusInfo {
            state: AuthState::SignedOut,
            account_epoch: None,
            account_scope: None,
            credential_revision: None,
            channel_label: None,
            player_catalog_matched: false,
            reason: Some("signed out".to_string()),
        };
        let rev = self.next_revision();
        self.emit_delta(StateDelta::AuthChanged {
            revision: rev,
            auth_state: self.auth_state.clone(),
        });

        self.refresh_capabilities();

        if let Some(storage) = self.deps.storage.clone() {
            let done = CancellationToken::new();
            let previous = self.logout_cleanup_done.replace(done.clone());
            let completed = done.drop_guard();
            tokio::spawn(async move {
                let _completed = completed;
                if let Some(previous) = previous {
                    previous.cancelled().await;
                }
                let result = if prev_scope.is_some() || pending_epoch.is_some() {
                    storage.clear_account_data(prev_scope, pending_epoch).await
                } else {
                    Ok(())
                };
                if let Err(err) = &result {
                    warn!(code = ?err.code, "failed to clear account cache on logout");
                }
                let _ = reply.send(result);
            });
        } else {
            let _ = reply.send(Ok(()));
        }
        info!("session cleared and account-scoped operations cancelled");
    }

    fn handle_auth_expired(&mut self, reason: Option<String>) {
        self.clear_catalog_session(self.auth_state.account_epoch.clone());
        self.invalidate_account_scoped_state("session expired");

        // Keep the account scope: its cache stays browsable (stale) until the user signs in
        // again, and the same accountEpoch may be re-applied once credentials are refreshed.
        self.auth_state.state = AuthState::Expired;
        self.auth_state.account_epoch = None;
        self.auth_state.credential_revision = None;
        self.auth_state.player_catalog_matched = false;
        self.auth_state.reason = reason;

        let rev = self.next_revision();
        self.emit_delta(StateDelta::AuthChanged {
            revision: rev,
            auth_state: self.auth_state.clone(),
        });
        self.refresh_capabilities();
    }

    fn invalidate_account_scoped_state(&mut self, reason: &str) {
        self.account_cancel.cancel();
        self.account_cancel = CancellationToken::new();
        self.invalidate_external_playback(reason);

        let rev = self.next_revision();
        self.emit_delta(StateDelta::ResourceInvalidated {
            revision: rev,
            resource_kind: "accountState".to_string(),
            resource_id: None,
        });
        debug!(reason = reason, "invalidated account-scoped UI state");
    }

    /// Player and catalog are the same account only when both identity hints are present and
    /// equal; missing evidence is never treated as a match.
    fn recompute_player_catalog_matched(&mut self) {
        self.auth_state.player_catalog_matched = self.auth_state.state == AuthState::Authenticated
            && self.last_bridge_logged_in == Some(true)
            && matches!(
                (&self.last_bridge_account_hint, &self.catalog_account_hint),
                (Some(player), Some(catalog)) if player == catalog
            );
    }

    fn identity_conflict(&self) -> bool {
        matches!(
            (&self.last_bridge_account_hint, &self.catalog_account_hint),
            (Some(player), Some(catalog)) if player != catalog
        )
    }

    /// Merges capability owners: the bridge reports player/queue controls; catalog, lyrics and
    /// MPRIS bits come from the core's own state. Account writes need a verified identity match.
    fn compute_capabilities(&self) -> CapabilitySet {
        use CapabilitySupport::{Supported, Unsupported, Unverified};
        let mut caps = self.bridge_caps.clone();
        let has_catalog = self.deps.catalog.is_some();
        let authed = self.auth_state.state == AuthState::Authenticated;
        let read = if !has_catalog {
            Unsupported
        } else if authed {
            Supported
        } else if self.auth_state.account_scope.is_some() {
            Unverified
        } else {
            Unsupported
        };
        caps.search = read;
        caps.library_playlists = read;
        caps.playlist_tracks = read;
        caps.album_detail = read;
        caps.artist_detail = read;
        caps.liked_songs = read;
        caps.personalized_home = Unsupported;
        let write = if !has_catalog || !authed || self.identity_conflict() {
            Unsupported
        } else if self.auth_state.player_catalog_matched {
            Supported
        } else {
            Unverified
        };
        caps.like_mutation = write;
        caps.playlist_add_mutation = write;
        caps.lyrics_lrclib = if self.deps.lyrics.is_some() && self.settings.lrclib_enabled {
            Supported
        } else {
            Unsupported
        };
        caps.mpris = if self.host_health.mpris_active {
            Supported
        } else {
            Unsupported
        };
        caps
    }

    fn refresh_capabilities(&mut self) {
        let caps = self.compute_capabilities();
        if caps != self.capabilities {
            self.capabilities = caps;
            let rev = self.next_revision();
            self.emit_delta(StateDelta::CapabilitiesChanged {
                revision: rev,
                capabilities: self.capabilities.clone(),
            });
        }
    }

    fn check_bridge_epoch_and_seq(
        &mut self,
        incoming_epoch: &BridgeEpoch,
        seq: u64,
    ) -> BridgeCheck {
        if self.active_bridge_epoch.as_ref() != Some(incoming_epoch) {
            self.diagnostics.dropped_stale_bridge_events = self
                .diagnostics
                .dropped_stale_bridge_events
                .saturating_add(1);
            return BridgeCheck::Drop;
        }
        if let Some(last_seq) = self.last_bridge_seq {
            if seq <= last_seq {
                self.diagnostics.dropped_non_monotonic_seq =
                    self.diagnostics.dropped_non_monotonic_seq.saturating_add(1);
                return BridgeCheck::Drop;
            }
            if seq > last_seq.saturating_add(1) {
                self.diagnostics.seq_gaps_detected =
                    self.diagnostics.seq_gaps_detected.saturating_add(1);
                self.last_bridge_seq = Some(seq);
                return BridgeCheck::AcceptWithGap;
            }
        }
        self.last_bridge_seq = Some(seq);
        BridgeCheck::Accept
    }

    fn trigger_snapshot_rebuild(&mut self, bridge_epoch: BridgeEpoch) {
        self.diagnostics.snapshot_rebuilds_requested = self
            .diagnostics
            .snapshot_rebuilds_requested
            .saturating_add(1);
        let bridge = self.deps.bridge.clone();
        tokio::spawn(async move {
            let _ = bridge.request_snapshot(bridge_epoch).await;
        });
    }

    fn handle_bridge_ready(&mut self, seq: u64, payload: BridgeReadyPayload) -> bool {
        if self.retired_bridge_epochs.contains(&payload.bridge_epoch) {
            self.diagnostics.dropped_stale_bridge_events = self
                .diagnostics
                .dropped_stale_bridge_events
                .saturating_add(1);
            return false;
        }

        let same_epoch = self.active_bridge_epoch.as_ref() == Some(&payload.bridge_epoch);
        if same_epoch {
            match self.check_bridge_epoch_and_seq(&payload.bridge_epoch, seq) {
                BridgeCheck::Drop => return false,
                BridgeCheck::AcceptWithGap => {
                    self.trigger_snapshot_rebuild(payload.bridge_epoch.clone());
                }
                BridgeCheck::Accept => {}
            }
        } else {
            // Transition to a new bridgeEpoch.
            let was_authorized_successor =
                self.authorized_successor_epoch.take().as_ref() == Some(&payload.bridge_epoch);
            if let Some(old_epoch) = self
                .active_bridge_epoch
                .replace(payload.bridge_epoch.clone())
            {
                self.retired_bridge_epochs.insert(old_epoch);
            }
            self.last_bridge_seq = Some(seq);

            // Handle pending commands across epoch change.
            if was_authorized_successor {
                let keep_play_target = self
                    .active_command
                    .as_ref()
                    .is_some_and(|a| matches!(a.plan, PendingOpPlan::PlayTarget { .. }));
                if keep_play_target {
                    if let Some(active) = self.active_command.as_mut() {
                        active.bound_bridge_epoch = payload.bridge_epoch.clone();
                        active.successor_handoff_used = true;
                    }
                    // Cancel anything else queued behind PlayTarget; never auto-retry next/enqueue.
                    let now_ms = self.deps.clock.now_ms();
                    while let Some(mut queued) = self.pending_queue.pop_front() {
                        queued.receipt.status = CommandLifecycleStatus::Cancelled;
                        queued.receipt.error = Some(QueryError {
                            code: QueryErrorCode::StaleContext,
                            retriable: false,
                            retry_after_ms: None,
                            safe_message: "bridgeEpoch changed after playTarget navigation"
                                .to_string(),
                            request_id: None,
                        });
                        self.dedup.upsert(queued.receipt.clone(), now_ms);
                        self.emit_command_updated(queued.receipt);
                    }
                } else {
                    self.cancel_all_pending("unexpected bridgeEpoch transition");
                }
            } else {
                self.cancel_all_pending("bridgeEpoch rebuilt");
            }

            // Reset bridge-epoch-scoped clock, queue, and track identity.
            let now_ms = self.deps.clock.now_ms();
            self.clock_model.on_bridge_epoch_rebuild(now_ms);
            self.full_queue = None;
            self.queue_summary = QueueSummary {
                queue_revision: 0,
                current_entry_id: None,
                loaded_count: 0,
                completeness: ytm_protocol::QueueCompleteness::Unknown,
                has_more: None,
                preview_entries: Vec::new(),
            };
            self.player.bridge_epoch = Some(payload.bridge_epoch.clone());
            self.player.track_instance_id = None;
            self.player.current_queue_entry_id = None;
            self.bridge_playback_seen = false;
            self.bridge_unhealthy = false;
            self.last_recorded_instance = None;
            self.player.stopped_confirmed = false;
            self.clear_page_media_observations();
            let rev_q = self.next_revision();
            self.emit_delta(StateDelta::QueueReplaced {
                revision: rev_q,
                queue_summary: self.queue_summary.clone(),
            });
        }

        self.player.connection = if self.bridge_unhealthy {
            PlayerConnectionState::Stale
        } else {
            PlayerConnectionState::Ready
        };
        self.bridge_caps = payload.capabilities;
        self.last_bridge_logged_in = Some(payload.logged_in_observed);
        self.last_bridge_account_hint = payload.account_hint;
        self.recompute_player_catalog_matched();
        self.refresh_capabilities();
        let rev_auth = self.next_revision();
        self.emit_delta(StateDelta::AuthChanged {
            revision: rev_auth,
            auth_state: self.auth_state.clone(),
        });
        self.emit_player_changed();

        if !same_epoch {
            self.trigger_snapshot_rebuild(payload.bridge_epoch);
        }
        self.maybe_start_restore();
        true
    }

    fn handle_bridge_playback_observed(
        &mut self,
        seq: u64,
        payload: BridgePlaybackObservedPayload,
    ) -> bool {
        match self.check_bridge_epoch_and_seq(&payload.bridge_epoch, seq) {
            BridgeCheck::Drop => return false,
            BridgeCheck::AcceptWithGap => {
                self.trigger_snapshot_rebuild(payload.bridge_epoch.clone());
            }
            BridgeCheck::Accept => {}
        }
        // A failed page probe's observation says nothing about what the page loaded.
        self.bridge_playback_seen |= !self.bridge_unhealthy;

        let track_changed = self.player.track_instance_id != payload.track_instance_id
            || self.player.current_track.as_ref().map(|t| &t.video_id)
                != payload.current_track.as_ref().map(|t| &t.video_id)
            || self.player.current_queue_entry_id != payload.current_queue_entry_id;

        let was_playing = self.player.playback == PlaybackStatus::Playing;
        if track_changed {
            self.lyrics_cancel.cancel();
            self.lyrics_cancel = CancellationToken::new();
            self.player.stopped_confirmed = false;
        }
        if payload.playback == PlaybackStatus::Playing {
            self.player.stopped_confirmed = false;
        }

        if !self.bridge_unhealthy
            && (self.player.connection == PlayerConnectionState::Stale
                || self.player.connection == PlayerConnectionState::Recovering
                || self.player.connection == PlayerConnectionState::Starting)
        {
            self.player.connection = PlayerConnectionState::Ready;
        }

        let now_ms = self.deps.clock.now_ms();
        self.clock_model
            .update_from_observation(&payload, self.player.connection, now_ms);

        self.player.playback = payload.playback;
        self.player.bridge_epoch = Some(payload.bridge_epoch);
        self.player.track_instance_id = payload.track_instance_id;
        let mut current_track = payload.current_track;
        if let Some(track) = current_track.as_mut() {
            self.apply_now_playing_cover(track);
        }
        self.player.current_track = current_track;
        self.player.current_queue_entry_id = payload.current_queue_entry_id;
        // The bridge corroborates a context with the playing video itself; it does not need the
        // queue selection, which can lag behind a navigated collection.
        self.player.playback_context = payload.playback_context.filter(|_| {
            self.player.current_track.is_some() && self.player.track_instance_id.is_some()
        });
        self.player.time_anchor = self.clock_model.snapshot_anchor(now_ms);
        self.player.volume = payload.volume;
        self.player.muted = payload.muted;
        self.player.shuffle = payload.shuffle;
        self.player.repeat = payload.repeat;
        self.player.autoplay = payload.autoplay;
        self.player.codec = payload.codec;
        self.player.audio_mode = payload.audio_mode;
        self.player.quality_label = payload.quality_label;
        self.player.drm_protected = payload.drm_protected;

        if track_changed {
            self.record_track_change(payload.position_ms);
        } else if was_playing && self.player.playback == PlaybackStatus::Paused {
            let requested_pause = self.active_command.as_ref().is_some_and(|active| {
                matches!(
                    active.plan,
                    PendingOpPlan::Pause { .. } | PendingOpPlan::Stop { .. }
                ) || active.plan.paused_skip().is_some_and(|skip| {
                    matches!(
                        skip.phase,
                        PausedSkipPhase::AwaitingPausedConfirmation { .. }
                            | PausedSkipPhase::ReadyToUngate
                    )
                })
            });
            if !requested_pause {
                self.take_over_external_playback(
                    "the page paused outside the requested playback action",
                );
            }
            self.persist_position(true);
        }

        self.emit_player_changed();
        self.evaluate_current_active();
        self.maybe_start_restore();
        true
    }

    /// The page reports the playing track's cover as a remote URL. The core caches it once
    /// through storage and patches the resulting asset id and accent into every observation of
    /// that cover, so UI (`app://covers/<id>`) and MPRIS (`file://`) get a local image.
    fn apply_now_playing_cover(&mut self, track: &mut ytm_protocol::Track) {
        let Some(remote) = track.cover.as_ref().and_then(|c| c.remote_url.clone()) else {
            // A track without cover art leaves nothing to protect from eviction.
            if let Some(storage) = &self.deps.storage {
                storage.pin_cover(None);
            }
            return;
        };
        if let Some((url, cached)) = &self.now_playing_cover
            && *url == remote
        {
            if let Some(cover) = cached {
                track.cover = Some(cover.clone());
            }
            return;
        }
        // The previous track's cover is no longer playing; the new one is pinned once cached.
        if let Some(storage) = &self.deps.storage {
            storage.pin_cover(None);
        }
        self.now_playing_cover = Some((remote.clone(), None));
        if let Some(storage) = self.deps.storage.clone() {
            let internal_tx = self.internal_tx.clone();
            tokio::spawn(async move {
                let result = storage.cache_cover(remote.clone()).await;
                let _ = internal_tx
                    .send(InternalEvent::CoverCached {
                        remote_url: remote,
                        result,
                    })
                    .await;
            });
        }
    }

    fn handle_cover_cached(&mut self, remote_url: String, result: Result<CachedCover, QueryError>) {
        let cached = match result {
            Ok(cached) => cached,
            Err(err) => {
                debug!(code = ?err.code, "now-playing cover could not be cached");
                return;
            }
        };
        if self.now_playing_cover.as_ref().map(|(url, _)| url) != Some(&remote_url) {
            return;
        }
        if let Some(storage) = &self.deps.storage {
            storage.pin_cover(Some(cached.asset_id.clone()));
        }
        let cover = ytm_protocol::CoverRef {
            asset_id: Some(cached.asset_id),
            remote_url: Some(remote_url.clone()),
            accent_oklch: Some(cached.accent_oklch),
        };
        self.now_playing_cover = Some((remote_url.clone(), Some(cover.clone())));
        if let Some(track) = self.player.current_track.as_mut()
            && track.cover.as_ref().and_then(|c| c.remote_url.as_ref()) == Some(&remote_url)
        {
            track.cover = Some(cover);
            self.emit_player_changed();
        }
    }

    fn handle_bridge_queue_observed(&mut self, seq: u64, payload: QueueSnapshot) -> bool {
        let Some(ref epoch) = payload.bridge_epoch else {
            self.diagnostics.dropped_stale_bridge_events = self
                .diagnostics
                .dropped_stale_bridge_events
                .saturating_add(1);
            return false;
        };
        let epoch_owned = epoch.clone();
        match self.check_bridge_epoch_and_seq(&epoch_owned, seq) {
            BridgeCheck::Drop => return false,
            BridgeCheck::AcceptWithGap => {
                self.trigger_snapshot_rebuild(epoch_owned);
            }
            BridgeCheck::Accept => {}
        }

        let loaded_count = u32::try_from(payload.entries.len()).unwrap_or(u32::MAX);
        let preview_entries = payload
            .entries
            .iter()
            .take(MAX_QUEUE_PREVIEW_ENTRIES)
            .cloned()
            .collect();

        self.queue_summary = QueueSummary {
            queue_revision: payload.queue_revision,
            current_entry_id: payload.current_entry_id.clone(),
            loaded_count,
            completeness: payload.completeness,
            has_more: payload.has_more,
            preview_entries,
        };
        self.full_queue = Some(payload);

        let rev = self.next_revision();
        self.emit_delta(StateDelta::QueueReplaced {
            revision: rev,
            queue_summary: self.queue_summary.clone(),
        });
        self.evaluate_current_active();
        true
    }

    fn handle_bridge_health(&mut self, seq: u64, payload: BridgeHealthPayload) -> bool {
        match self.check_bridge_epoch_and_seq(&payload.bridge_epoch, seq) {
            BridgeCheck::Drop => return false,
            BridgeCheck::AcceptWithGap => {
                self.trigger_snapshot_rebuild(payload.bridge_epoch.clone());
            }
            BridgeCheck::Accept => {}
        }

        let now_ms = self.deps.clock.now_ms();
        self.bridge_unhealthy =
            !(payload.healthy && payload.video_element_present && payload.controls_bound);
        let next_conn = if self.bridge_unhealthy {
            PlayerConnectionState::Stale
        } else {
            PlayerConnectionState::Ready
        };
        if self.player.connection != next_conn {
            self.player.connection = next_conn;
            self.clock_model
                .on_connection_changed(next_conn, self.player.playback, now_ms);
            self.emit_player_changed();
        }
        true
    }

    fn handle_bridge_command_ack(&mut self, seq: u64, payload: BridgeCommandAckPayload) -> bool {
        let uninterrupted = match self.check_bridge_epoch_and_seq(&payload.bridge_epoch, seq) {
            BridgeCheck::Drop => return false,
            BridgeCheck::AcceptWithGap => {
                self.trigger_snapshot_rebuild(payload.bridge_epoch.clone());
                false
            }
            BridgeCheck::Accept => true,
        };

        if payload.dispatched
            && uninterrupted
            && let Some(active) = &mut self.active_command
            && active.receipt.command_id == payload.command_id
            && let PendingOpPlan::EditQueue(proof) = &mut active.plan
            && proof.acknowledge_no_op()
        {
            self.evaluate_current_active();
        }

        if let Some(active) = &self.active_command
            && active.receipt.command_id == payload.command_id
            && !payload.dispatched
        {
            if self.page_refused_commands.len() >= MAX_DISPATCHED_COMMANDS {
                self.page_refused_commands.pop_front();
            }
            self.page_refused_commands
                .push_back(payload.command_id.clone());
            let err = payload.error.unwrap_or_else(|| QueryError {
                code: QueryErrorCode::UnsupportedCapability,
                retriable: false,
                retry_after_ms: None,
                safe_message: "bridge refused to dispatch command".to_string(),
                request_id: None,
            });
            self.finalize_active_command(CommandLifecycleStatus::Failed, Some(err));
        } else if !payload.dispatched
            && self
                .active_command
                .as_ref()
                .and_then(|active| active.plan.paused_skip())
                .is_some_and(|skip| matches!(&skip.phase, PausedSkipPhase::AwaitingRestoreSeek { command_id, .. } if *command_id == payload.command_id))
        {
            self.abandon_restore_seek();
        }
        true
    }

    fn handle_bridge_error(
        &mut self,
        seq: u64,
        bridge_epoch: Option<BridgeEpoch>,
        error: QueryError,
    ) -> bool {
        if let Some(epoch) = bridge_epoch {
            match self.check_bridge_epoch_and_seq(&epoch, seq) {
                BridgeCheck::Drop => return false,
                BridgeCheck::AcceptWithGap => {
                    self.trigger_snapshot_rebuild(epoch);
                }
                BridgeCheck::Accept => {}
            }
        } else if self.active_bridge_epoch.is_some() {
            self.diagnostics.dropped_stale_bridge_events = self
                .diagnostics
                .dropped_stale_bridge_events
                .saturating_add(1);
            return false;
        }

        if error.code == QueryErrorCode::BridgeIncompatible {
            self.invalidate_bridge(PlayerConnectionState::Incompatible, &error.safe_message);
        }
        true
    }
}

enum BridgeCheck {
    Accept,
    AcceptWithGap,
    Drop,
}

async fn release_orphan_gate(
    gate: &dyn OutputGatePort,
    transaction_id: String,
    released: &OnceCell<OutputGateResultPayload>,
    timeout: Duration,
    tx: &mpsc::Sender<InternalEvent>,
) {
    if release_gate(gate, &transaction_id, released, timeout)
        .await
        .is_ok()
    {
        let _ = tx
            .send(InternalEvent::OrphanGateReleased(transaction_id))
            .await;
    }
}

// Share a confirmed release across concurrent callers; only a failed attempt can be retried.
async fn release_gate(
    gate: &dyn OutputGatePort,
    transaction_id: &str,
    released: &OnceCell<OutputGateResultPayload>,
    timeout: Duration,
) -> Result<OutputGateResultPayload, QueryError> {
    released
        .get_or_try_init(|| async {
            let release = gate.set_output_gated(transaction_id.to_owned(), false);
            let result =
                tokio::time::timeout(timeout, release)
                    .await
                    .map_err(|_| QueryError {
                        code: QueryErrorCode::Timeout,
                        retriable: false,
                        retry_after_ms: None,
                        safe_message: "output gate release timed out".to_string(),
                        request_id: None,
                    })??;
            if !result.applied || result.transaction_id != transaction_id {
                return Err(QueryError {
                    code: QueryErrorCode::BridgeIncompatible,
                    retriable: false,
                    retry_after_ms: None,
                    safe_message: result
                        .error
                        .unwrap_or_else(|| "output gate release was not applied".to_string()),
                    request_id: None,
                });
            }
            Ok(result)
        })
        .await
        .cloned()
}

fn external_playback_stale(message: &str) -> QueryError {
    QueryError {
        code: QueryErrorCode::StaleContext,
        retriable: false,
        retry_after_ms: None,
        safe_message: message.to_owned(),
        request_id: None,
    }
}

fn bound_tid_clone(opt: &Option<TrackInstanceId>) -> Option<TrackInstanceId> {
    opt.clone()
}

/// Interval for persisting the playing position of the current history entry.
const POSITION_PERSIST_MS: u64 = 15_000;

/// Remaining steps of a launch restore once its silent playTarget has been observed.
struct RestoreFollowup {
    play_command: CommandId,
    video_id: ytm_protocol::VideoId,
    position_ms: u64,
    autoplay: bool,
}

fn internal_command_id(prefix: &str) -> CommandId {
    CommandId::new(format!("{prefix}-{}", Uuid::new_v4())).expect("uuid id is valid")
}

fn is_restore_play_intent(op: PlayerOp) -> bool {
    matches!(
        op,
        PlayerOp::Play | PlayerOp::PlayTarget | PlayerOp::PlayCollection | PlayerOp::Toggle
    )
}

fn empty_args() -> PlayerCommandArgs {
    PlayerCommandArgs {
        track_instance_id: None,
        position_ms: None,
        volume: None,
        muted: None,
        target: None,
        shuffle: None,
        repeat: None,
        autoplay: None,
        enqueue_position: None,
        queue_entry_id: None,
        before_queue_entry_id: None,
        queue_revision: None,
    }
}

/// Creates an output-gate transaction when a transition must stay silent.
fn new_gate_transaction(needed: bool) -> (Option<PausedSkipState>, Option<String>) {
    if !needed {
        return (None, None);
    }
    let tx_id = format!("gate-{}", Uuid::new_v4());
    (
        Some(PausedSkipState {
            transaction_id: tx_id.clone(),
            gate_engaged: false,
            phase: PausedSkipPhase::AwaitingTrackChange,
        }),
        Some(tx_id),
    )
}

/// Manual volume, mute and repeat changes neither redirect playback nor touch the queue, so they
/// leave external playback and queue work running. Every other manual command takes over.
fn takes_over_external_playback(op: PlayerOp) -> bool {
    !matches!(
        op,
        PlayerOp::SetVolume | PlayerOp::SetMuted | PlayerOp::SetRepeat
    )
}
