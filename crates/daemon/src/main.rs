mod control_catalog;
mod control_lyrics;
mod control_server;
mod lyrics_cache;
mod network;
mod ports;
mod protocol_io;

use crate::lyrics_cache::SqliteLyricsCacheAdapter;
use crate::network::NetworkStatus;
use crate::ports::{NdjsonBridgePort, NdjsonOutputGatePort, OutboundSender};
use crate::protocol_io::{
    ReadFrameResult, decode_bounded_frame, read_bounded_ndjson_line, run_ndjson_writer,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use tokio::io::BufReader;
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};
use tracing_subscriber::EnvFilter;
use ytm_core::{CoreDeps, CoreEngine, EngineConfig, SystemClock};
use ytm_data::{
    CatalogService, ConnectivityConfig, HttpReachabilityProbe, MUSIC_PROBE_URL, YtmCatalogAdapter,
    run_connectivity_monitor,
};
use ytm_lyrics::{LrclibConfig, LyricsService};
use ytm_mpris::{MprisConfig, MprisHostAction, MprisService};
use ytm_protocol::{
    AgentActivityRequest, AppSettingsPatch, AssetResolvedPayload, BridgeCommandAckPayload,
    BridgeHealthPayload, BridgeLifecyclePayload, BridgePlaybackObservedPayload, BridgeReadyPayload,
    CatalogQueryRequest, CoreReadyPayload, Envelope, HostEnvironmentSnapshotPayload,
    HostHelloPayload, HostWindowVisibilityPayload, LyricsQueryRequest, MAX_COMMAND_QUEUE,
    MAX_FRAME_BYTES, MAX_PENDING_REQUESTS, MessageKind, MomentDesktopRequest, MutationRequest,
    NetworkState, OutputGateResultPayload, PROTOCOL_VERSION, PlayerCommandPayload, QueryError,
    QueryErrorCode, QueueSnapshot, RecentSearchesRequest, RequestId, ResolveAssetPayload,
    SenderRole, SessionMaterialPayload,
};
use ytm_storage::{CoverCacheConfig, SqliteStorage, StoragePaths};

const OUTBOUND_CHANNEL_CAPACITY: usize = 512;

fn init_redacted_tracing() {
    let filter = EnvFilter::try_from_env("YTMCP_LOG")
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new("info"));

    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(true)
        .with_ansi(false)
        .try_init();
}

fn open_daemon_storage() -> Result<SqliteStorage, QueryError> {
    if let Ok(custom_root) = std::env::var("YTMCP_DATA_DIR") {
        let trimmed = custom_root.trim();
        if !trimmed.is_empty() {
            let paths = StoragePaths::for_root(PathBuf::from(trimmed));
            return SqliteStorage::open(
                paths,
                Arc::new(SystemClock::new()),
                CoverCacheConfig::production(),
            );
        }
    }
    SqliteStorage::open_default()
}

struct PendingSlot {
    counter: Arc<AtomicUsize>,
}

impl PendingSlot {
    fn try_acquire(counter: &Arc<AtomicUsize>, max: usize) -> Option<Self> {
        loop {
            let cur = counter.load(Ordering::Acquire);
            if cur >= max {
                return None;
            }
            if counter
                .compare_exchange_weak(cur, cur + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Some(Self {
                    counter: Arc::clone(counter),
                });
            }
        }
    }
}

impl Drop for PendingSlot {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::AcqRel);
    }
}

async fn send_error(
    outbound: &OutboundSender,
    request_id: Option<RequestId>,
    code: QueryErrorCode,
    message: impl Into<String>,
    retriable: bool,
) {
    let mut env = Envelope::error_response(0, request_id, code, message, retriable);
    env.core_epoch = outbound.current_core_epoch();
    let _ = outbound.send_envelope(env).await;
}

async fn send_typed_response(
    outbound: &OutboundSender,
    kind: MessageKind,
    request_id: Option<RequestId>,
    payload: impl serde::Serialize,
) {
    match outbound.make_envelope(kind, payload) {
        Ok(mut env) => {
            env.request_id = request_id;
            let _ = outbound.send_envelope(env).await;
        }
        Err(err) => {
            send_error(outbound, request_id, err.code, err.safe_message, false).await;
        }
    }
}

/// A UI settings patch waiting for the settings worker, holding its pending-request slot.
type SettingsPatchJob = (Option<RequestId>, AppSettingsPatch, PendingSlot);

/// Applies UI settings patches one at a time in arrival order, so two quick edits to the same
/// field end with the later one; other request kinds keep running beside it.
fn spawn_settings_worker(
    core: Arc<CoreEngine>,
    outbound: OutboundSender,
) -> mpsc::Sender<SettingsPatchJob> {
    // Every queued job holds a pending slot, so the slot limit also bounds this queue.
    let (tx, mut rx) = mpsc::channel::<SettingsPatchJob>(MAX_PENDING_REQUESTS);
    tokio::spawn(async move {
        while let Some((request_id, patch, _slot)) = rx.recv().await {
            match core.patch_settings(patch).await {
                Ok(settings) => {
                    send_typed_response(
                        &outbound,
                        MessageKind::CoreSettingsUpdated,
                        request_id,
                        settings,
                    )
                    .await;
                }
                Err(err) => {
                    send_error(
                        &outbound,
                        request_id,
                        err.code,
                        err.safe_message,
                        err.retriable,
                    )
                    .await;
                }
            }
        }
    });
    tx
}

/// Defense in depth: the host multiplexes UI, bridge and host frames onto one stdin, so the
/// daemon accepts only kinds that some inbound role may send and never core-originated kinds.
fn inbound_kind_allowed(kind: MessageKind) -> bool {
    [
        SenderRole::MainUi,
        SenderRole::PlayerBridge,
        SenderRole::Host,
    ]
    .into_iter()
    .any(|role| kind.is_allowed_from(role))
}

/// Decodes a typed payload; on failure counts the dropped frame and reports it as `core.error`
/// so malformed bridge/host frames are visible instead of silently ignored.
async fn decode_or_report<T: serde::de::DeserializeOwned>(
    outbound: &OutboundSender,
    env: &Envelope,
    dropped: &AtomicU64,
) -> Option<T> {
    match env.decode_payload::<T>() {
        Ok(payload) => Some(payload),
        Err(err) => {
            let total = dropped.fetch_add(1, Ordering::Relaxed) + 1;
            warn!(kind = ?env.kind, total, "dropped inbound frame with invalid payload");
            send_error(
                outbound,
                env.request_id.clone(),
                QueryErrorCode::InvalidPayload,
                format!("{err} (dropped inbound frames: {total})"),
                false,
            )
            .await;
            None
        }
    }
}

/// Spawns the traffic-based reachability monitor and the task that folds its verdict into the
/// published network state. Returns `None` (host signal only) if the probe cannot be built.
fn start_connectivity_monitor(
    adapter: &Arc<YtmCatalogAdapter>,
    network: &Arc<NetworkStatus>,
    host_online: Arc<Notify>,
    cancel: &tokio_util::sync::CancellationToken,
) -> Option<(JoinHandle<()>, JoinHandle<()>)> {
    let probe = match HttpReachabilityProbe::new(MUSIC_PROBE_URL, std::time::Duration::from_secs(5))
    {
        Ok(p) => Arc::new(p),
        Err(_) => {
            warn!("reachability probe unavailable; network state follows the host only");
            return None;
        }
    };
    let (tx, mut rx) = tokio::sync::watch::channel(false);
    let monitor = tokio::spawn(run_connectivity_monitor(
        probe,
        adapter.network_failure_signal(),
        host_online,
        ConnectivityConfig::default(),
        tx,
        cancel.clone(),
    ));
    let network = Arc::clone(network);
    let forward = tokio::spawn(async move {
        while rx.changed().await.is_ok() {
            let offline = *rx.borrow_and_update();
            network.set_traffic_offline(offline).await;
        }
    });
    Some((monitor, forward))
}

#[tokio::main]
async fn main() {
    init_redacted_tracing();

    let storage = match open_daemon_storage() {
        Ok(s) => Arc::new(s),
        Err(err) => {
            tracing::error!(code = ?err.code, "failed to initialize storage: {}", err.safe_message);
            std::process::exit(1);
        }
    };

    let catalog_adapter = match YtmCatalogAdapter::new(Arc::clone(&storage)) {
        Ok(c) => Arc::new(c),
        Err(err) => {
            tracing::error!(code = ?err.code, "failed to initialize catalog adapter: {}", err.safe_message);
            std::process::exit(1);
        }
    };
    let catalog = Arc::new(CatalogService::new(Arc::clone(&catalog_adapter)));

    let lyrics_cache = Arc::new(SqliteLyricsCacheAdapter::new(Arc::clone(&storage)));
    let lyrics_service = match LyricsService::with_cache(LrclibConfig::default(), lyrics_cache) {
        Ok(svc) => Arc::new(svc),
        Err(err) => {
            tracing::error!(code = ?err.code, "failed to initialize lyrics service: {}", err.safe_message);
            std::process::exit(1);
        }
    };

    let (out_tx, out_rx) = mpsc::channel::<Envelope>(OUTBOUND_CHANNEL_CAPACITY);
    let writer_handle = tokio::spawn(run_ndjson_writer(out_rx, tokio::io::stdout()));
    let outbound = OutboundSender::new(out_tx);

    let bridge_port = Arc::new(NdjsonBridgePort::new(outbound.clone()));
    let output_gate_port = Arc::new(NdjsonOutputGatePort::new(outbound.clone()));

    let control_registrations = storage.paths().config_dir.join("control-clients.json");
    let operation_store = storage.clone();
    let control_catalog = Arc::new(control_catalog::YtmControlCatalog::new(
        catalog_adapter.clone(),
    ));
    let core = Arc::new(
        CoreEngine::start(
            CoreDeps {
                clock: Arc::new(SystemClock::new()),
                bridge: bridge_port,
                output_gate: output_gate_port.clone(),
                catalog: Some(catalog.clone()),
                storage: Some(storage),
                lyrics: Some(lyrics_service.clone()),
            },
            EngineConfig::default(),
        )
        .with_playlist_editor(control_catalog.clone()),
    );
    outbound.set_core_epoch(core.core_epoch().clone());

    // Restore persisted user settings and Moment DJ config before serving subscriptions.
    if let Err(err) = core.restore_settings().await {
        warn!(code = ?err.code, "could not restore persisted settings");
    }
    if let Err(err) = core.restore_moment_config().await {
        warn!(code = ?err.code, "could not restore persisted Moment DJ config");
    }

    use ytm_core::control::journal::OperationStore;
    let store_recovered = operation_store.recover(core.clock().wall_now_ms()).await;
    let control_service = match store_recovered {
        Ok(()) => Some(Arc::new(
            ytm_core::control::ControlService::new(core.as_ref().clone())
                .with_catalog(control_catalog)
                .with_lyrics(Arc::new(control_lyrics::DaemonControlLyrics::new(
                    lyrics_service,
                )))
                .with_operations(operation_store.clone())
                .with_local_history(operation_store.clone())
                .with_dj_store(operation_store.clone())
                .with_moment_store(operation_store),
        )),
        Err(err) => {
            warn!(code = ?err.code, "operation journal could not be recovered");
            None
        }
    };
    // Interrupted writes with an evidence path settle on their own once the account is bound.
    if let Some(service) = &control_service {
        tokio::spawn(service.clone().settle_on_account_bind());
    }
    let moment_registrations = control_registrations.clone();
    let control_server = control_service.as_ref().and_then(|service| {
        match control_server::ControlServer::start(service.clone(), control_registrations) {
            Ok(server) => server,
            Err(err) => {
                warn!(kind = ?err.kind(), "local music control is unavailable");
                None
            }
        }
    });

    let (mpris_tx, mut mpris_rx) = mpsc::channel::<MprisHostAction>(16);
    let no_mpris = std::env::var("YTMCP_NO_MPRIS")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    let mpris_handle = if no_mpris {
        info!("MPRIS disabled via YTMCP_NO_MPRIS=1");
        None
    } else {
        match MprisService::start(Arc::clone(&core), MprisConfig::default(), mpris_tx).await {
            Ok(handle) => {
                info!(bus_name = handle.bus_name(), "MPRIS D-Bus service active");
                Some(handle)
            }
            Err(err) => {
                warn!(error = %err, "MPRIS service unavailable; continuing without D-Bus control");
                None
            }
        }
    };

    let mpris_forward_outbound = outbound.clone();
    let mpris_action_task = tokio::spawn(async move {
        while let Some(action) = mpris_rx.recv().await {
            let action_str = match action {
                MprisHostAction::Raise => "mpris:raise",
                MprisHostAction::Quit => "mpris:quit",
            };
            if let Ok(mut env) = mpris_forward_outbound.make_envelope(
                MessageKind::CorePong,
                serde_json::json!({ "action": action_str }),
            ) {
                env.request_id = RequestId::new(action_str).ok();
                let _ = mpris_forward_outbound.send_envelope(env).await;
            }
        }
    });

    let network = Arc::new(NetworkStatus::new(catalog.clone(), core.clone()));
    // Started by the first non-mock host.hello; mock runs never probe the real service.
    let host_online = Arc::new(Notify::new());
    let connectivity_cancel = tokio_util::sync::CancellationToken::new();
    let mut connectivity_tasks: Option<(JoinHandle<()>, JoinHandle<()>)> = None;

    let pending_requests = Arc::new(AtomicUsize::new(0));
    let pending_commands = Arc::new(AtomicUsize::new(0));
    let dropped_frames = AtomicU64::new(0);
    let mut delta_task: Option<JoinHandle<()>> = None;
    let settings_jobs = spawn_settings_worker(Arc::clone(&core), outbound.clone());
    let mut stdin_reader = BufReader::new(tokio::io::stdin());

    info!(core_epoch = %core.core_epoch(), "ytm-daemon ready");

    loop {
        match read_bounded_ndjson_line(&mut stdin_reader, MAX_FRAME_BYTES).await {
            Ok(ReadFrameResult::Eof) => {
                info!("stdin EOF received; shutting down cleanly");
                break;
            }
            Ok(ReadFrameResult::Oversized(bytes)) => {
                warn!(bytes, "rejected oversized inbound NDJSON frame");
                send_error(
                    &outbound,
                    None,
                    QueryErrorCode::InvalidPayload,
                    format!("frame exceeds maximum size of {MAX_FRAME_BYTES} bytes (got {bytes})"),
                    false,
                )
                .await;
            }
            Ok(ReadFrameResult::Frame(bytes)) => {
                if bytes.iter().all(|b| b.is_ascii_whitespace()) {
                    continue;
                }
                let env = match decode_bounded_frame(&bytes) {
                    Ok(env) => env,
                    Err((code, msg, request_id)) => {
                        warn!(?code, "rejected invalid inbound frame: {msg}");
                        send_error(&outbound, request_id, code, msg, false).await;
                        continue;
                    }
                };

                if !inbound_kind_allowed(env.kind) {
                    send_error(
                        &outbound,
                        env.request_id.clone(),
                        QueryErrorCode::InvalidPayload,
                        format!(
                            "message kind {:?} is not accepted from any inbound role",
                            env.kind
                        ),
                        false,
                    )
                    .await;
                    continue;
                }

                // State-changing UI frames must reach the daemon with an epoch. The host may
                // stamp an unstamped frame before sending; an explicit stale epoch is rejected below.
                if matches!(
                    env.kind,
                    MessageKind::UiPlayerCommand
                        | MessageKind::UiMutation
                        | MessageKind::UiAgentActivity
                        | MessageKind::UiRecentSearches
                ) && env.core_epoch.is_none()
                {
                    send_error(
                        &outbound,
                        env.request_id.clone(),
                        QueryErrorCode::StaleContext,
                        format!("{:?} requires coreEpoch", env.kind),
                        false,
                    )
                    .await;
                    continue;
                }

                if env.kind != MessageKind::HostHello
                    && let Err(err) = core.validate_core_epoch(env.core_epoch.as_ref())
                {
                    send_error(
                        &outbound,
                        env.request_id.clone(),
                        err.code,
                        err.safe_message,
                        false,
                    )
                    .await;
                    continue;
                }

                match env.kind {
                    MessageKind::HostShutdown => {
                        info!("host.shutdown received; exiting cleanly");
                        break;
                    }
                    MessageKind::HostHello => match env.decode_payload::<HostHelloPayload>() {
                        Ok(mut payload) => {
                            payload.host_health.mpris_active = mpris_handle.is_some();
                            payload.network_state = network.set_host(payload.network_state);
                            if !payload.mock_mode && connectivity_tasks.is_none() {
                                connectivity_tasks = start_connectivity_monitor(
                                    &catalog_adapter,
                                    &network,
                                    Arc::clone(&host_online),
                                    &connectivity_cancel,
                                );
                            }
                            let _ = core.on_host_hello(payload).await;
                            let ready = CoreReadyPayload {
                                core_epoch: core.core_epoch().clone(),
                                protocol_version: PROTOCOL_VERSION,
                            };
                            send_typed_response(
                                &outbound,
                                MessageKind::CoreReady,
                                env.request_id.clone(),
                                ready,
                            )
                            .await;
                            if let Ok(env_cfg) = core.environment_config().await {
                                send_typed_response(
                                    &outbound,
                                    MessageKind::CoreEnvironmentConfig,
                                    None,
                                    env_cfg,
                                )
                                .await;
                            }
                        }
                        Err(err) => {
                            send_error(
                                &outbound,
                                env.request_id.clone(),
                                QueryErrorCode::InvalidPayload,
                                err.to_string(),
                                false,
                            )
                            .await;
                        }
                    },
                    MessageKind::UiSubscribeSnapshot => {
                        if let Some(prev) = delta_task.take() {
                            prev.abort();
                        }
                        match core.subscribe_snapshot().await {
                            Ok((snapshot, mut delta_rx)) => {
                                let base_rev = snapshot.revision;
                                send_typed_response(
                                    &outbound,
                                    MessageKind::CoreSnapshot,
                                    env.request_id.clone(),
                                    snapshot,
                                )
                                .await;
                                let delta_outbound = outbound.clone();
                                let delta_core = Arc::clone(&core);
                                delta_task = Some(tokio::spawn(async move {
                                    let mut last_sent_rev = base_rev;
                                    loop {
                                        match delta_rx.recv().await {
                                            Ok(delta) => {
                                                if delta.revision() <= last_sent_rev {
                                                    continue;
                                                }
                                                last_sent_rev = delta.revision();
                                                if let Ok(delta_env) = delta_outbound
                                                    .make_envelope(MessageKind::CoreDelta, delta)
                                                    && delta_outbound
                                                        .send_envelope(delta_env)
                                                        .await
                                                        .is_err()
                                                {
                                                    break;
                                                }
                                            }
                                            Err(
                                                tokio::sync::broadcast::error::RecvError::Lagged(
                                                    skipped,
                                                ),
                                            ) => {
                                                debug!(
                                                    skipped,
                                                    "UI delta stream lagged; sending fresh snapshot"
                                                );
                                                if let Ok(snap) = delta_core.snapshot().await {
                                                    last_sent_rev = snap.revision;
                                                    if let Ok(snap_env) = delta_outbound
                                                        .make_envelope(
                                                            MessageKind::CoreSnapshot,
                                                            snap,
                                                        )
                                                        && delta_outbound
                                                            .send_envelope(snap_env)
                                                            .await
                                                            .is_err()
                                                    {
                                                        break;
                                                    }
                                                    // Skipped deltas may have carried terminal
                                                    // command states; replay recent receipts.
                                                    for receipt in delta_core
                                                        .recent_receipts()
                                                        .await
                                                        .unwrap_or_default()
                                                    {
                                                        if let Ok(env) = delta_outbound
                                                            .make_envelope(
                                                                MessageKind::CoreCommandReceipt,
                                                                receipt,
                                                            )
                                                        {
                                                            let _ = delta_outbound
                                                                .send_envelope(env)
                                                                .await;
                                                        }
                                                    }
                                                }
                                            }
                                            Err(
                                                tokio::sync::broadcast::error::RecvError::Closed,
                                            ) => {
                                                break;
                                            }
                                        }
                                    }
                                }));
                            }
                            Err(err) => {
                                let qerr = QueryError::from(err);
                                send_error(
                                    &outbound,
                                    env.request_id.clone(),
                                    qerr.code,
                                    qerr.safe_message,
                                    qerr.retriable,
                                )
                                .await;
                            }
                        }
                    }
                    MessageKind::UiUnsubscribe => {
                        if let Some(prev) = delta_task.take() {
                            prev.abort();
                        }
                    }
                    MessageKind::UiPing => {
                        send_typed_response(
                            &outbound,
                            MessageKind::CorePong,
                            env.request_id.clone(),
                            serde_json::json!({}),
                        )
                        .await;
                    }
                    MessageKind::UiAgentActivity => {
                        let Some(service) = control_service.clone() else {
                            send_error(
                                &outbound,
                                env.request_id,
                                QueryErrorCode::UnsupportedCapability,
                                "the local operation journal is unavailable",
                                false,
                            )
                            .await;
                            continue;
                        };
                        let Some(slot) =
                            PendingSlot::try_acquire(&pending_requests, MAX_PENDING_REQUESTS)
                        else {
                            send_error(
                                &outbound,
                                env.request_id,
                                QueryErrorCode::Busy,
                                "pending request limit reached",
                                true,
                            )
                            .await;
                            continue;
                        };
                        let request = match env.decode_payload::<AgentActivityRequest>() {
                            Ok(request) => request,
                            Err(err) => {
                                send_error(
                                    &outbound,
                                    env.request_id,
                                    QueryErrorCode::InvalidPayload,
                                    err.to_string(),
                                    false,
                                )
                                .await;
                                continue;
                            }
                        };
                        let out = outbound.clone();
                        tokio::spawn(async move {
                            let _slot = slot;
                            match service.desktop_activity(env.account_epoch, request).await {
                                Ok(snapshot) => {
                                    send_typed_response(
                                        &out,
                                        MessageKind::CoreAgentActivity,
                                        env.request_id,
                                        snapshot,
                                    )
                                    .await
                                }
                                Err(error) => {
                                    let code = match error.code {
                                        ytm_control_api::ErrorCode::StaleContext
                                        | ytm_control_api::ErrorCode::Conflict => {
                                            QueryErrorCode::StaleContext
                                        }
                                        _ => QueryErrorCode::StorageError,
                                    };
                                    send_error(&out, env.request_id, code, error.message, false)
                                        .await;
                                }
                            }
                        });
                    }
                    MessageKind::UiPlayerCommand => {
                        let Some(cmd_slot) =
                            PendingSlot::try_acquire(&pending_commands, MAX_COMMAND_QUEUE)
                        else {
                            send_error(
                                &outbound,
                                env.request_id.clone(),
                                QueryErrorCode::Busy,
                                "player command queue limit reached",
                                true,
                            )
                            .await;
                            continue;
                        };
                        match env.decode_payload::<PlayerCommandPayload>() {
                            Ok(payload) => {
                                let core_c = Arc::clone(&core);
                                let out_c = outbound.clone();
                                let req_id = env.request_id.clone();
                                tokio::spawn(async move {
                                    let _slot = cmd_slot;
                                    match core_c.submit_player_command(payload).await {
                                        Ok(receipt) => {
                                            send_typed_response(
                                                &out_c,
                                                MessageKind::CoreCommandReceipt,
                                                req_id,
                                                receipt,
                                            )
                                            .await;
                                        }
                                        Err(err) => {
                                            send_error(
                                                &out_c,
                                                req_id,
                                                err.code,
                                                err.safe_message,
                                                err.retriable,
                                            )
                                            .await;
                                        }
                                    }
                                });
                            }
                            Err(err) => {
                                drop(cmd_slot);
                                send_error(
                                    &outbound,
                                    env.request_id.clone(),
                                    QueryErrorCode::InvalidPayload,
                                    err.to_string(),
                                    false,
                                )
                                .await;
                            }
                        }
                    }
                    MessageKind::UiCatalogQuery => {
                        let Some(slot) =
                            PendingSlot::try_acquire(&pending_requests, MAX_PENDING_REQUESTS)
                        else {
                            send_error(
                                &outbound,
                                env.request_id.clone(),
                                QueryErrorCode::Busy,
                                "pending request limit (128) reached",
                                true,
                            )
                            .await;
                            continue;
                        };
                        match env.decode_payload::<CatalogQueryRequest>() {
                            Ok(req) => {
                                let core_c = Arc::clone(&core);
                                let out_c = outbound.clone();
                                let req_id = env.request_id.clone();
                                let acct_epoch = env.account_epoch.clone();
                                tokio::spawn(async move {
                                    let _slot = slot;
                                    match core_c.catalog_query(req, acct_epoch).await {
                                        Ok(resp) => {
                                            send_typed_response(
                                                &out_c,
                                                MessageKind::CoreCatalogResponse,
                                                req_id,
                                                resp,
                                            )
                                            .await;
                                        }
                                        Err(err) => {
                                            send_error(
                                                &out_c,
                                                req_id,
                                                err.code,
                                                err.safe_message,
                                                err.retriable,
                                            )
                                            .await;
                                        }
                                    }
                                });
                            }
                            Err(err) => {
                                drop(slot);
                                send_error(
                                    &outbound,
                                    env.request_id.clone(),
                                    QueryErrorCode::InvalidPayload,
                                    err.to_string(),
                                    false,
                                )
                                .await;
                            }
                        }
                    }
                    MessageKind::UiLyricsQuery => {
                        let Some(slot) =
                            PendingSlot::try_acquire(&pending_requests, MAX_PENDING_REQUESTS)
                        else {
                            send_error(
                                &outbound,
                                env.request_id.clone(),
                                QueryErrorCode::Busy,
                                "pending request limit (128) reached",
                                true,
                            )
                            .await;
                            continue;
                        };
                        match env.decode_payload::<LyricsQueryRequest>() {
                            Ok(req) => {
                                let core_c = Arc::clone(&core);
                                let out_c = outbound.clone();
                                let req_id = env.request_id.clone();
                                tokio::spawn(async move {
                                    let _slot = slot;
                                    match core_c.lyrics_query(req).await {
                                        Ok(resp) => {
                                            send_typed_response(
                                                &out_c,
                                                MessageKind::CoreLyricsResponse,
                                                req_id,
                                                resp,
                                            )
                                            .await;
                                        }
                                        Err(err) => {
                                            send_error(
                                                &out_c,
                                                req_id,
                                                err.code,
                                                err.safe_message,
                                                err.retriable,
                                            )
                                            .await;
                                        }
                                    }
                                });
                            }
                            Err(err) => {
                                drop(slot);
                                send_error(
                                    &outbound,
                                    env.request_id.clone(),
                                    QueryErrorCode::InvalidPayload,
                                    err.to_string(),
                                    false,
                                )
                                .await;
                            }
                        }
                    }
                    MessageKind::UiMutation => {
                        let Some(slot) =
                            PendingSlot::try_acquire(&pending_requests, MAX_PENDING_REQUESTS)
                        else {
                            send_error(
                                &outbound,
                                env.request_id.clone(),
                                QueryErrorCode::Busy,
                                "pending request limit (128) reached",
                                true,
                            )
                            .await;
                            continue;
                        };
                        match env.decode_payload::<MutationRequest>() {
                            Ok(req) => {
                                let core_c = Arc::clone(&core);
                                let out_c = outbound.clone();
                                let req_id = env.request_id.clone();
                                let acct_epoch = env.account_epoch.clone();
                                tokio::spawn(async move {
                                    let _slot = slot;
                                    match core_c.mutate(req, acct_epoch).await {
                                        Ok(resp) => {
                                            send_typed_response(
                                                &out_c,
                                                MessageKind::CoreMutationResponse,
                                                req_id,
                                                resp,
                                            )
                                            .await;
                                        }
                                        Err(err) => {
                                            send_error(
                                                &out_c,
                                                req_id,
                                                err.code,
                                                err.safe_message,
                                                err.retriable,
                                            )
                                            .await;
                                        }
                                    }
                                });
                            }
                            Err(err) => {
                                drop(slot);
                                send_error(
                                    &outbound,
                                    env.request_id.clone(),
                                    QueryErrorCode::InvalidPayload,
                                    err.to_string(),
                                    false,
                                )
                                .await;
                            }
                        }
                    }
                    MessageKind::UiRecentSearches => {
                        let Some(slot) =
                            PendingSlot::try_acquire(&pending_requests, MAX_PENDING_REQUESTS)
                        else {
                            send_error(
                                &outbound,
                                env.request_id.clone(),
                                QueryErrorCode::Busy,
                                "pending request limit (128) reached",
                                true,
                            )
                            .await;
                            continue;
                        };
                        match env.decode_payload::<RecentSearchesRequest>() {
                            Ok(req) => {
                                let core_c = Arc::clone(&core);
                                let out_c = outbound.clone();
                                let req_id = env.request_id.clone();
                                let acct_epoch = env.account_epoch.clone();
                                tokio::spawn(async move {
                                    let _slot = slot;
                                    match core_c.recent_searches(req, acct_epoch).await {
                                        Ok(resp) => {
                                            send_typed_response(
                                                &out_c,
                                                MessageKind::CoreRecentSearches,
                                                req_id,
                                                resp,
                                            )
                                            .await;
                                        }
                                        Err(err) => {
                                            send_error(
                                                &out_c,
                                                req_id,
                                                err.code,
                                                err.safe_message,
                                                err.retriable,
                                            )
                                            .await;
                                        }
                                    }
                                });
                            }
                            Err(err) => {
                                drop(slot);
                                send_error(
                                    &outbound,
                                    env.request_id.clone(),
                                    QueryErrorCode::InvalidPayload,
                                    err.to_string(),
                                    false,
                                )
                                .await;
                            }
                        }
                    }
                    MessageKind::UiSettingsUpdate => {
                        let Some(slot) =
                            PendingSlot::try_acquire(&pending_requests, MAX_PENDING_REQUESTS)
                        else {
                            send_error(
                                &outbound,
                                env.request_id.clone(),
                                QueryErrorCode::Busy,
                                "pending request limit (128) reached",
                                true,
                            )
                            .await;
                            continue;
                        };
                        match env.decode_payload::<AppSettingsPatch>() {
                            Ok(patch) => {
                                if settings_jobs
                                    .try_send((env.request_id.clone(), patch, slot))
                                    .is_err()
                                {
                                    send_error(
                                        &outbound,
                                        env.request_id.clone(),
                                        QueryErrorCode::Busy,
                                        "the settings worker is not accepting updates",
                                        true,
                                    )
                                    .await;
                                }
                            }
                            Err(err) => {
                                drop(slot);
                                send_error(
                                    &outbound,
                                    env.request_id.clone(),
                                    QueryErrorCode::InvalidPayload,
                                    err.to_string(),
                                    false,
                                )
                                .await;
                            }
                        }
                    }
                    MessageKind::UiLogout | MessageKind::HostSessionCleared => {
                        let _ = core.on_logout().await;
                    }
                    MessageKind::UiClearCache => {
                        let Some(slot) =
                            PendingSlot::try_acquire(&pending_requests, MAX_PENDING_REQUESTS)
                        else {
                            send_error(
                                &outbound,
                                env.request_id.clone(),
                                QueryErrorCode::Busy,
                                "pending request limit (128) reached",
                                true,
                            )
                            .await;
                            continue;
                        };
                        let core_c = Arc::clone(&core);
                        let out_c = outbound.clone();
                        let req_id = env.request_id.clone();
                        tokio::spawn(async move {
                            let _slot = slot;
                            if let Err(err) = core_c.clear_cache().await {
                                send_error(
                                    &out_c,
                                    req_id,
                                    err.code,
                                    err.safe_message,
                                    err.retriable,
                                )
                                .await;
                            }
                        });
                    }
                    MessageKind::UiOpenPlayerWindow => {
                        // Handled directly by the Electron host shell.
                    }
                    MessageKind::HostWindowVisibility => {
                        if let Some(payload) = decode_or_report::<HostWindowVisibilityPayload>(
                            &outbound,
                            &env,
                            &dropped_frames,
                        )
                        .await
                        {
                            let _ = core.on_host_window_visibility(payload).await;
                        }
                    }
                    MessageKind::HostPowerResume => {
                        let _ = core.on_host_power_resume().await;
                    }
                    MessageKind::HostEnvironmentSnapshot => {
                        if let Some(payload) = decode_or_report::<HostEnvironmentSnapshotPayload>(
                            &outbound,
                            &env,
                            &dropped_frames,
                        )
                        .await
                        {
                            let _ = core.on_host_environment_snapshot(payload).await;
                        }
                    }
                    MessageKind::UiMomentSettings => {
                        let Some(service) = control_service.clone() else {
                            send_error(
                                &outbound,
                                env.request_id,
                                QueryErrorCode::UnsupportedCapability,
                                "Moment DJ settings are unavailable",
                                false,
                            )
                            .await;
                            continue;
                        };
                        let Some(slot) =
                            PendingSlot::try_acquire(&pending_requests, MAX_PENDING_REQUESTS)
                        else {
                            send_error(
                                &outbound,
                                env.request_id,
                                QueryErrorCode::Busy,
                                "pending request limit reached",
                                true,
                            )
                            .await;
                            continue;
                        };
                        let request = match env.decode_payload::<MomentDesktopRequest>() {
                            Ok(req) => req,
                            Err(err) => {
                                send_error(
                                    &outbound,
                                    env.request_id,
                                    QueryErrorCode::InvalidPayload,
                                    err.to_string(),
                                    false,
                                )
                                .await;
                                continue;
                            }
                        };
                        let is_update = matches!(request, MomentDesktopRequest::Update { .. });
                        let reg_path = moment_registrations.clone();
                        let out = outbound.clone();
                        let core_c = Arc::clone(&core);
                        tokio::spawn(async move {
                            let _slot = slot;
                            let principals = tokio::task::spawn_blocking(move || {
                                ytm_control_api::local::load_private_json::<
                                    Vec<ytm_control_api::local::Registration>,
                                >(&reg_path)
                                .map(|list| {
                                    list.into_iter().map(|r| r.principal).collect::<Vec<_>>()
                                })
                                .unwrap_or_default()
                            })
                            .await
                            .unwrap_or_default();
                            match service
                                .desktop_moment_settings(env.account_epoch, request, principals)
                                .await
                            {
                                Ok(snapshot) => {
                                    send_typed_response(
                                        &out,
                                        MessageKind::CoreMomentSettings,
                                        env.request_id,
                                        snapshot,
                                    )
                                    .await;
                                }
                                Err(error) => {
                                    let code = match error.code {
                                        ytm_control_api::ErrorCode::StaleContext
                                        | ytm_control_api::ErrorCode::Conflict => {
                                            QueryErrorCode::StaleContext
                                        }
                                        ytm_control_api::ErrorCode::InvalidInput => {
                                            QueryErrorCode::InvalidPayload
                                        }
                                        _ => QueryErrorCode::StorageError,
                                    };
                                    send_error(&out, env.request_id, code, error.message, false)
                                        .await;
                                }
                            }
                            // An update can be saved even when its reply fails, so the host
                            // always gets the current config and never keeps an old revision.
                            if is_update && let Ok(cfg) = core_c.environment_config().await {
                                send_typed_response(
                                    &out,
                                    MessageKind::CoreEnvironmentConfig,
                                    None,
                                    cfg,
                                )
                                .await;
                            }
                        });
                    }
                    MessageKind::HostNetworkChanged => {
                        if let Some(network_state) =
                            decode_or_report::<NetworkState>(&outbound, &env, &dropped_frames).await
                        {
                            let effective = network.set_host(network_state);
                            let _ = core.on_host_network_changed(effective).await;
                            if network_state == NetworkState::Online && connectivity_tasks.is_some()
                            {
                                host_online.notify_one();
                            }
                        }
                    }
                    MessageKind::HostSessionMaterial => {
                        // NEVER log SessionMaterialPayload cookies or raw bytes.
                        let Some(slot) =
                            PendingSlot::try_acquire(&pending_requests, MAX_PENDING_REQUESTS)
                        else {
                            send_error(
                                &outbound,
                                env.request_id.clone(),
                                QueryErrorCode::Busy,
                                "pending request limit (128) reached",
                                true,
                            )
                            .await;
                            continue;
                        };
                        match env.decode_payload::<SessionMaterialPayload>() {
                            Ok(material) => {
                                let binding = core.enqueue_session_material(material).await;
                                let out_c = outbound.clone();
                                let req_id = env.request_id.clone();
                                tokio::spawn(async move {
                                    let _slot = slot;
                                    let result = match binding {
                                        Ok(receipt) => receipt.await,
                                        Err(err) => Err(err),
                                    };
                                    if let Err(err) = result {
                                        send_error(
                                            &out_c,
                                            req_id,
                                            err.code,
                                            err.safe_message,
                                            err.retriable,
                                        )
                                        .await;
                                    }
                                });
                            }
                            Err(_) => {
                                drop(slot);
                                send_error(
                                    &outbound,
                                    env.request_id.clone(),
                                    QueryErrorCode::InvalidPayload,
                                    "invalid session material payload",
                                    false,
                                )
                                .await;
                            }
                        }
                    }
                    MessageKind::HostBridgeLifecycle => {
                        if let Some(payload) = decode_or_report::<BridgeLifecyclePayload>(
                            &outbound,
                            &env,
                            &dropped_frames,
                        )
                        .await
                        {
                            if payload.event == "crashed" {
                                info!(bridge_epoch = ?payload.bridge_epoch, "player renderer crash received by daemon");
                            }
                            let _ = core.on_host_bridge_lifecycle(payload).await;
                        }
                    }
                    MessageKind::HostOutputGateResult => {
                        if let Some(result) = decode_or_report::<OutputGateResultPayload>(
                            &outbound,
                            &env,
                            &dropped_frames,
                        )
                        .await
                        {
                            output_gate_port.complete_gate_result(result);
                        }
                    }
                    MessageKind::CoreResolveAsset => {
                        let Some(slot) =
                            PendingSlot::try_acquire(&pending_requests, MAX_PENDING_REQUESTS)
                        else {
                            send_error(
                                &outbound,
                                env.request_id.clone(),
                                QueryErrorCode::Busy,
                                "pending request limit (128) reached",
                                true,
                            )
                            .await;
                            continue;
                        };
                        match env.decode_payload::<ResolveAssetPayload>() {
                            Ok(req) => {
                                let core_c = Arc::clone(&core);
                                let out_c = outbound.clone();
                                let req_id = env.request_id.clone();
                                tokio::spawn(async move {
                                    let _slot = slot;
                                    let asset_id = req.asset_id;
                                    let resolved =
                                        match core_c.resolve_asset(asset_id.clone()).await {
                                            Ok(res) => res,
                                            Err(err) => AssetResolvedPayload {
                                                asset_id,
                                                file_path: None,
                                                mime_type: None,
                                                error: Some(err),
                                            },
                                        };
                                    send_typed_response(
                                        &out_c,
                                        MessageKind::CoreAssetResolved,
                                        req_id,
                                        resolved,
                                    )
                                    .await;
                                });
                            }
                            Err(err) => {
                                drop(slot);
                                send_error(
                                    &outbound,
                                    env.request_id.clone(),
                                    QueryErrorCode::InvalidPayload,
                                    err.to_string(),
                                    false,
                                )
                                .await;
                            }
                        }
                    }
                    MessageKind::BridgeReady => {
                        if let Some(payload) =
                            decode_or_report::<BridgeReadyPayload>(&outbound, &env, &dropped_frames)
                                .await
                        {
                            let _ = core.on_bridge_ready(env.seq, payload).await;
                        }
                    }
                    MessageKind::BridgePlaybackObserved => {
                        if let Some(payload) = decode_or_report::<BridgePlaybackObservedPayload>(
                            &outbound,
                            &env,
                            &dropped_frames,
                        )
                        .await
                        {
                            let _ = core.on_bridge_playback_observed(env.seq, payload).await;
                        }
                    }
                    MessageKind::BridgeQueueObserved => {
                        if let Some(payload) =
                            decode_or_report::<QueueSnapshot>(&outbound, &env, &dropped_frames)
                                .await
                        {
                            let _ = core.on_bridge_queue_observed(env.seq, payload).await;
                        }
                    }
                    MessageKind::BridgeHealth => {
                        if let Some(payload) = decode_or_report::<BridgeHealthPayload>(
                            &outbound,
                            &env,
                            &dropped_frames,
                        )
                        .await
                        {
                            let _ = core.on_bridge_health(env.seq, payload).await;
                        }
                    }
                    MessageKind::BridgeCommandAck => {
                        if let Some(payload) = decode_or_report::<BridgeCommandAckPayload>(
                            &outbound,
                            &env,
                            &dropped_frames,
                        )
                        .await
                        {
                            let _ = core.on_bridge_command_ack(env.seq, payload).await;
                        }
                    }
                    MessageKind::BridgeError => {
                        if let Some(err) =
                            decode_or_report::<QueryError>(&outbound, &env, &dropped_frames).await
                        {
                            let _ = core
                                .on_bridge_error(env.seq, env.bridge_epoch.clone(), err)
                                .await;
                        }
                    }
                    other => {
                        send_error(
                            &outbound,
                            env.request_id.clone(),
                            QueryErrorCode::InvalidPayload,
                            format!("unexpected inbound message kind {other:?}"),
                            false,
                        )
                        .await;
                    }
                }
            }
            Err(err) => {
                warn!(error = %err, "stdin read error; shutting down");
                break;
            }
        }
    }

    if let Some(task) = delta_task.take() {
        task.abort();
        let _ = task.await;
    }
    connectivity_cancel.cancel();
    if let Some((monitor, forward)) = connectivity_tasks.take() {
        let _ = monitor.await;
        forward.abort();
        let _ = forward.await;
    }
    mpris_action_task.abort();
    let _ = mpris_action_task.await;
    if let Some(handle) = mpris_handle {
        handle.shutdown().await;
    }
    if let Some(server) = control_server {
        server.shutdown().await;
    }
    let _ = core.shutdown().await;
    drop(core);
    drop(output_gate_port);
    drop(outbound);
    tokio::task::yield_now().await;
    let _ = tokio::time::timeout(std::time::Duration::from_millis(500), writer_handle).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;
    use ytm_core::StoragePort;
    use ytm_protocol::AppSettings;

    #[tokio::test]
    async fn lyrics_setting_follows_restored_and_successfully_persisted_updates_only() {
        let dir =
            std::env::temp_dir().join(format!("ytm-daemon-settings-{}", uuid::Uuid::new_v4()));
        let paths = StoragePaths::for_root(&dir);
        let storage = Arc::new(
            SqliteStorage::open(
                paths.clone(),
                Arc::new(SystemClock::new()),
                CoverCacheConfig::production(),
            )
            .unwrap(),
        );
        let lyrics = Arc::new(LyricsService::new(LrclibConfig::default()).unwrap());
        let core = CoreEngine::start(
            CoreDeps {
                storage: Some(storage.clone()),
                lyrics: Some(lyrics.clone()),
                ..CoreDeps::default()
            },
            EngineConfig::default(),
        );

        let disabled = AppSettings {
            lrclib_enabled: false,
            ..AppSettings::default()
        };
        let saved = dir.join("saved.json");
        fs::write(&saved, serde_json::to_vec(&disabled).unwrap()).unwrap();
        symlink(&saved, paths.settings_file()).unwrap();
        core.restore_settings().await.unwrap();
        assert!(
            paths.settings_file().is_symlink(),
            "restoring must not rewrite the settings file"
        );
        assert!(!lyrics.is_enabled());
        assert!(!core.snapshot().await.unwrap().settings.lrclib_enabled);

        fs::remove_file(paths.settings_file()).unwrap();
        fs::create_dir(paths.settings_file()).unwrap();
        let enable = || AppSettingsPatch {
            lrclib_enabled: Some(true),
            ..AppSettingsPatch::default()
        };
        let err = core.patch_settings(enable()).await.unwrap_err();
        assert_eq!(err.code, QueryErrorCode::StorageError);
        assert!(!lyrics.is_enabled());
        assert!(!core.snapshot().await.unwrap().settings.lrclib_enabled);

        fs::remove_dir(paths.settings_file()).unwrap();
        core.patch_settings(enable()).await.unwrap();
        assert!(lyrics.is_enabled());
        assert!(
            storage
                .load_settings()
                .await
                .unwrap()
                .unwrap()
                .lrclib_enabled
        );
        core.shutdown().await.unwrap();
        fs::remove_dir_all(dir).unwrap();
    }
}
