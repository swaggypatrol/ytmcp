use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{Notify, broadcast};
use tokio_util::sync::CancellationToken;
use ytm_core::{
    BoxFuture, BridgePort, CachedCover, CatalogIdentity, CatalogPort, Clock, CoreDeps, CoreEngine,
    DEDUP_TTL_MS, DEFAULT_CONTROL_TIMEOUT_MS, EngineConfig, LastPlayed, LastSession,
    MAX_DEDUP_ENTRIES, MockClock, OfficialLyrics, OutputGatePort, PlaybackClock, PlaybackFence,
    StoragePort, SystemClock,
};
use ytm_protocol::{
    AccountEpoch, AccountScope, ArtistRef, AuthState, BridgeCommandAckPayload, BridgeEpoch,
    BridgeLifecyclePayload, BridgePlaybackObservedPayload, BridgeReadyPayload, CapabilitySet,
    CapabilitySupport, CatalogQueryKind, CatalogQueryRequest, CatalogResponsePayload, CommandId,
    CommandLifecycleStatus, CredentialRevision, EnqueuePosition, Freshness, MessageKind,
    MutationKind, MutationOutcome, MutationRequest, MutationResult, NetworkState,
    OutputGateResultPayload, PlaybackRef, PlaybackStatus, PlayerCommandArgs, PlayerCommandPayload,
    PlayerConnectionState, PlayerOp, QueryError, QueryErrorCode, QueueCompleteness, QueueEntry,
    QueueEntryId, QueueRange, QueueSnapshot, RecentHistoryResult, SessionMaterialPayload,
    StateDelta, Track, TrackAvailability, TrackInstanceId, VideoId, decode_ndjson_str,
};

#[derive(Default)]
struct RecordingBridge {
    sent: Mutex<Vec<(BridgeEpoch, PlayerCommandPayload)>>,
    snapshot_requests: Mutex<Vec<BridgeEpoch>>,
    fail_next_dispatch: Mutex<Option<QueryError>>,
}

impl RecordingBridge {
    fn sent_ops(&self) -> Vec<PlayerOp> {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .map(|(_, c)| c.op)
            .collect()
    }

    fn sent_commands(&self) -> Vec<(BridgeEpoch, PlayerCommandPayload)> {
        self.sent.lock().unwrap().clone()
    }

    fn snapshot_requests(&self) -> Vec<BridgeEpoch> {
        self.snapshot_requests.lock().unwrap().clone()
    }
}

impl BridgePort for RecordingBridge {
    fn send_player_command(
        &self,
        bridge_epoch: BridgeEpoch,
        payload: PlayerCommandPayload,
    ) -> BoxFuture<'_, Result<(), QueryError>> {
        let fail = self.fail_next_dispatch.lock().unwrap().take();
        if fail.is_none() {
            self.sent.lock().unwrap().push((bridge_epoch, payload));
        }
        Box::pin(async move {
            if let Some(err) = fail {
                Err(err)
            } else {
                Ok(())
            }
        })
    }

    fn request_snapshot(&self, bridge_epoch: BridgeEpoch) -> BoxFuture<'_, Result<(), QueryError>> {
        self.snapshot_requests.lock().unwrap().push(bridge_epoch);
        Box::pin(async { Ok(()) })
    }
}

#[derive(Default)]
struct RecordingOutputGate {
    pending_engage: Mutex<Option<Arc<Notify>>>,
    pending_release: Mutex<Option<Arc<Notify>>>,
    started: Notify,
    reject_release: Mutex<bool>,
    calls: Mutex<Vec<(String, bool)>>,
    reject_engage: Mutex<bool>,
    error_on_engage: Mutex<bool>,
}

impl RecordingOutputGate {
    fn calls(&self) -> Vec<(String, bool)> {
        self.calls.lock().unwrap().clone()
    }
}

impl OutputGatePort for RecordingOutputGate {
    fn set_output_gated(
        &self,
        transaction_id: String,
        gated: bool,
    ) -> BoxFuture<'_, Result<OutputGateResultPayload, QueryError>> {
        self.calls
            .lock()
            .unwrap()
            .push((transaction_id.clone(), gated));
        let reject = if gated {
            *self.reject_engage.lock().unwrap()
        } else {
            *self.reject_release.lock().unwrap()
        };
        let hold = if gated {
            self.pending_engage.lock().unwrap().take()
        } else {
            self.pending_release.lock().unwrap().take()
        };
        let err = gated && *self.error_on_engage.lock().unwrap();
        Box::pin(async move {
            if let Some(hold) = hold {
                self.started.notify_one();
                hold.notified().await;
            }
            if err {
                Err(QueryError {
                    code: QueryErrorCode::Busy,
                    retriable: false,
                    retry_after_ms: None,
                    safe_message: "gate IPC failed".to_string(),
                    request_id: None,
                })
            } else {
                Ok(OutputGateResultPayload {
                    transaction_id,
                    applied: !reject,
                    error: if reject {
                        Some("gate rejected".to_string())
                    } else {
                        None
                    },
                })
            }
        })
    }
}

struct SlowCatalogPort {
    account_hint: Mutex<Option<String>>,
    official: Mutex<Option<OfficialLyrics>>,
    started: Arc<Notify>,
    proceed: Arc<Notify>,
    ignore_cancel: bool,
    seen_tokens: Mutex<Vec<CancellationToken>>,
}

impl CatalogPort for SlowCatalogPort {
    fn query(
        &self,
        _account_epoch: Option<AccountEpoch>,
        _account_scope: Option<AccountScope>,
        _request: CatalogQueryRequest,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<CatalogResponsePayload, QueryError>> {
        self.seen_tokens.lock().unwrap().push(cancel.clone());
        let started = self.started.clone();
        let proceed = self.proceed.clone();
        let ignore_cancel = self.ignore_cancel;
        Box::pin(async move {
            started.notify_one();
            if ignore_cancel {
                proceed.notified().await;
            } else {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        return Err(QueryError {
                            code: QueryErrorCode::AccountMismatch,
                            retriable: false,
                            retry_after_ms: None,
                            safe_message: "cancelled".to_string(),
                            request_id: None,
                        });
                    }
                    _ = proceed.notified() => {}
                }
            }
            Ok(CatalogResponsePayload::RecentHistory(RecentHistoryResult {
                items: Vec::new(),
                freshness: Freshness::Fresh,
            }))
        })
    }

    fn mutate(
        &self,
        _account_epoch: AccountEpoch,
        _account_scope: AccountScope,
        _request: MutationRequest,
        _cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<MutationResult, QueryError>> {
        Box::pin(async {
            Ok(MutationResult {
                kind: MutationKind::SetLike,
                outcome: MutationOutcome::Confirmed,
                error: None,
            })
        })
    }

    fn official_lyrics(
        &self,
        _account_epoch: AccountEpoch,
        _video_id: VideoId,
        _cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<Option<OfficialLyrics>, QueryError>> {
        let found = self.official.lock().unwrap().clone();
        Box::pin(async move { Ok(found) })
    }

    fn apply_session_material(
        &self,
        material: SessionMaterialPayload,
        _cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<CatalogIdentity, QueryError>> {
        Box::pin(async move {
            Ok(CatalogIdentity {
                account_scope: AccountScope::new(format!("scope-{}", material.account_epoch))
                    .unwrap(),
                account_hint: self.account_hint.lock().unwrap().clone(),
            })
        })
    }

    fn clear_session_material(
        &self,
        _account_epoch: Option<AccountEpoch>,
    ) -> Result<(), QueryError> {
        Ok(())
    }
}

fn supported_caps() -> CapabilitySet {
    CapabilitySet {
        play_pause: CapabilitySupport::Supported,
        seek: CapabilitySupport::Supported,
        next: CapabilitySupport::Supported,
        previous: CapabilitySupport::Supported,
        volume: CapabilitySupport::Supported,
        shuffle: CapabilitySupport::Supported,
        repeat: CapabilitySupport::Supported,
        queue_mirror: CapabilitySupport::Supported,
        queue_enqueue: CapabilitySupport::Supported,
        ..CapabilitySet::default()
    }
}

fn make_track(vid: &str, title: &str, duration_ms: Option<u64>) -> Track {
    Track {
        video_id: VideoId::new(vid).unwrap(),
        title: title.to_string(),
        artists: vec![ArtistRef {
            id: None,
            name: "Synthetic Artist".to_string(),
        }],
        album: None,
        duration_ms,
        cover: None,
        availability: TrackAvailability::Available,
        version_tag: None,
        is_liked: None,
    }
}

fn make_obs(
    epoch: &str,
    playback: PlaybackStatus,
    tid: &str,
    vid: &str,
    qe: &str,
    position_ms: u64,
    duration_ms: Option<u64>,
) -> BridgePlaybackObservedPayload {
    BridgePlaybackObservedPayload {
        bridge_epoch: BridgeEpoch::new(epoch).unwrap(),
        playback,
        track_instance_id: Some(TrackInstanceId::new(tid).unwrap()),
        current_track: Some(make_track(vid, "Track", duration_ms)),
        current_queue_entry_id: Some(QueueEntryId::new(qe).unwrap()),
        playback_context: None,
        position_ms,
        duration_ms,
        buffered_ms: None,
        playback_rate: 1.0,
        sample_age_ms: 5,
        volume: 0.8,
        muted: false,
        shuffle: Some(false),
        repeat: Some(ytm_protocol::RepeatMode::Off),
        autoplay: None,
        codec: None,
        audio_mode: None,
        quality_label: None,
        audio_itag: None,
        audio_quality: None,
        audio_bitrate_kbps: None,
        drm_protected: None,
    }
}

fn make_cmd(id: &str, op: PlayerOp, args: Option<PlayerCommandArgs>) -> PlayerCommandPayload {
    PlayerCommandPayload {
        command_id: CommandId::new(id).unwrap(),
        op,
        args,
    }
}

fn make_args() -> PlayerCommandArgs {
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
        queue_filter_id: None,
    }
}

async fn setup_engine() -> (
    CoreEngine,
    Arc<MockClock>,
    Arc<RecordingBridge>,
    Arc<RecordingOutputGate>,
) {
    let clock = Arc::new(MockClock::with_wall_offset(1_000, 1_700_000_000_000));
    let bridge = Arc::new(RecordingBridge::default());
    let output_gate = Arc::new(RecordingOutputGate::default());
    let deps = CoreDeps {
        clock: clock.clone(),
        bridge: bridge.clone(),
        output_gate: output_gate.clone(),
        catalog: None,
        storage: None,
        lyrics: None,
    };
    let config = EngineConfig {
        dispatch_timeout_ms: 1_000,
        tick_interval_ms: 20,
        mock_mode: true,
        ..EngineConfig::default()
    };
    let engine = CoreEngine::start(deps, config);
    (engine, clock, bridge, output_gate)
}

async fn init_ready_bridge(engine: &CoreEngine, epoch: &str, seq: u64) {
    let accepted = engine
        .on_bridge_ready(
            seq,
            BridgeReadyPayload {
                bridge_epoch: BridgeEpoch::new(epoch).unwrap(),
                bridge_version: "0.1.0".to_string(),
                capabilities: supported_caps(),
                account_hint: Some("TestChannel".to_string()),
                logged_in_observed: true,
            },
        )
        .await
        .unwrap();
    assert!(accepted);
}

async fn replay_fixture_ndjson(engine: &CoreEngine, ndjson: &str) {
    for line in ndjson.lines().filter(|l| !l.trim().is_empty()) {
        let env = decode_ndjson_str(line).expect("fixture line must satisfy protocol");
        match env.kind {
            MessageKind::BridgeReady => {
                let p: BridgeReadyPayload = env.decode_payload().unwrap();
                engine.on_bridge_ready(env.seq, p).await.unwrap();
            }
            MessageKind::BridgePlaybackObserved => {
                let p: BridgePlaybackObservedPayload = env.decode_payload().unwrap();
                engine
                    .on_bridge_playback_observed(env.seq, p)
                    .await
                    .unwrap();
            }
            MessageKind::BridgeQueueObserved => {
                let p: QueueSnapshot = env.decode_payload().unwrap();
                engine.on_bridge_queue_observed(env.seq, p).await.unwrap();
            }
            MessageKind::BridgeCommandAck => {
                let p: BridgeCommandAckPayload = env.decode_payload().unwrap();
                engine.on_bridge_command_ack(env.seq, p).await.unwrap();
            }
            MessageKind::UiPlayerCommand => {
                let p: PlayerCommandPayload = env.decode_payload().unwrap();
                engine.submit_player_command(p).await.unwrap();
            }
            other => panic!("unexpected fixture kind: {other:?}"),
        }
    }
}

#[tokio::test]
async fn fixture_playback_lifecycle_replays_cleanly() {
    let (engine, _clock, _bridge, _gate) = setup_engine().await;
    let ndjson = include_str!("../../../fixtures/player-events/playback_lifecycle.ndjson");
    replay_fixture_ndjson(&engine, ndjson).await;

    let snap = engine.snapshot().await.unwrap();
    assert_eq!(snap.player.connection, PlayerConnectionState::Ready);
    assert_eq!(snap.player.playback, PlaybackStatus::Paused);
    assert!(snap.player.stopped_confirmed);
    assert_eq!(snap.player.time_anchor.position_ms, 0);
    let stop_receipt = engine
        .command_receipt(CommandId::new("cmd-fx-stop").unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stop_receipt.status, CommandLifecycleStatus::Observed);
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn fixture_duplicate_video_queue_distinguishes_entries_by_queue_entry_id() {
    let (engine, _clock, _bridge, _gate) = setup_engine().await;
    let ndjson = include_str!("../../../fixtures/player-events/duplicate_video_queue.ndjson");
    replay_fixture_ndjson(&engine, ndjson).await;

    let snap = engine.snapshot().await.unwrap();
    assert_eq!(snap.queue_summary.loaded_count, 2);
    assert_eq!(
        snap.queue_summary.preview_entries[0].video_id,
        snap.queue_summary.preview_entries[1].video_id
    );
    assert_ne!(
        snap.queue_summary.preview_entries[0].entry_id,
        snap.queue_summary.preview_entries[1].entry_id
    );
    assert_eq!(
        snap.player.current_queue_entry_id,
        Some(QueueEntryId::new("qe-dup-2").unwrap())
    );
    let next_receipt = engine
        .command_receipt(CommandId::new("cmd-dup-next").unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(next_receipt.status, CommandLifecycleStatus::Observed);
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn fixture_stale_epoch_and_seq_guards() {
    let (engine, _clock, bridge, _gate) = setup_engine().await;
    let ndjson = include_str!("../../../fixtures/player-events/stale_epoch_and_seq.ndjson");
    replay_fixture_ndjson(&engine, ndjson).await;

    let snap = engine.snapshot().await.unwrap();
    assert_eq!(
        snap.player.bridge_epoch,
        Some(BridgeEpoch::new("bridge-ep-2").unwrap())
    );
    // Seq 1 from bridge-ep-2 was dropped as non-monotonic, while seq 5 had a gap -> accepted with rebuild request.
    assert_eq!(
        snap.player.track_instance_id,
        Some(TrackInstanceId::new("ti-ep-2").unwrap())
    );
    assert_eq!(snap.player.playback, PlaybackStatus::Playing);

    let diag = engine.diagnostics().await.unwrap();
    assert!(diag.dropped_stale_bridge_events >= 1);
    assert!(diag.dropped_non_monotonic_seq >= 1);
    assert!(diag.seq_gaps_detected >= 1);
    assert!(diag.snapshot_rebuilds_requested >= 1);
    assert!(!bridge.snapshot_requests().is_empty());
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn how_far_the_track_has_loaded_reaches_the_player_state() {
    let (engine, _clock, _bridge, _gate) = setup_engine().await;
    init_ready_bridge(&engine, "bridge-1", 1).await;
    let mut obs = make_obs(
        "bridge-1",
        PlaybackStatus::Playing,
        "ti-1",
        "vid-1",
        "qe-1",
        3_000,
        Some(240_000),
    );
    obs.buffered_ms = Some(68_000);
    engine.on_bridge_playback_observed(2, obs).await.unwrap();
    assert_eq!(
        engine.snapshot().await.unwrap().player.buffered_ms,
        Some(68_000)
    );

    // A page that stops reporting it clears it, rather than leaving a stale bar behind.
    let obs = make_obs(
        "bridge-1",
        PlaybackStatus::Playing,
        "ti-1",
        "vid-1",
        "qe-1",
        4_000,
        Some(240_000),
    );
    engine.on_bridge_playback_observed(3, obs).await.unwrap();
    assert_eq!(engine.snapshot().await.unwrap().player.buffered_ms, None);
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn command_lifecycle_accepted_dispatched_observed_and_ack_not_observed() {
    let (engine, _clock, _bridge, _gate) = setup_engine().await;
    init_ready_bridge(&engine, "bridge-1", 1).await;
    engine
        .on_bridge_playback_observed(
            2,
            make_obs(
                "bridge-1",
                PlaybackStatus::Paused,
                "ti-1",
                "vid-1",
                "qe-1",
                0,
                Some(200_000),
            ),
        )
        .await
        .unwrap();

    let (_snap, mut deltas) = engine.subscribe_snapshot().await.unwrap();

    let cmd = make_cmd("cmd-play-1", PlayerOp::Play, None);
    let receipt = engine.submit_player_command(cmd).await.unwrap();
    assert_eq!(receipt.status, CommandLifecycleStatus::Dispatched);

    // Bridge ack confirming dispatch MUST NOT transition observed playback or command status to Observed.
    engine
        .on_bridge_command_ack(
            3,
            BridgeCommandAckPayload {
                command_id: CommandId::new("cmd-play-1").unwrap(),
                bridge_epoch: BridgeEpoch::new("bridge-1").unwrap(),
                dispatched: true,
                error: None,
            },
        )
        .await
        .unwrap();

    let mid_snap = engine.snapshot().await.unwrap();
    assert_eq!(mid_snap.player.playback, PlaybackStatus::Paused);
    assert_eq!(mid_snap.pending.pending_command_count, 1);

    // Real observation settles command at Observed.
    engine
        .on_bridge_playback_observed(
            4,
            make_obs(
                "bridge-1",
                PlaybackStatus::Playing,
                "ti-1",
                "vid-1",
                "qe-1",
                500,
                Some(200_000),
            ),
        )
        .await
        .unwrap();

    let mut statuses = Vec::new();
    while let Ok(delta) = deltas.try_recv() {
        if let StateDelta::CommandUpdated { receipt, .. } = delta
            && receipt.command_id.as_str() == "cmd-play-1"
        {
            statuses.push(receipt.status);
        }
    }
    assert_eq!(
        statuses,
        vec![
            CommandLifecycleStatus::Accepted,
            CommandLifecycleStatus::Dispatched,
            CommandLifecycleStatus::Observed,
        ]
    );
    let end_snap = engine.snapshot().await.unwrap();
    assert_eq!(end_snap.pending.pending_command_count, 0);
    assert_eq!(end_snap.player.playback, PlaybackStatus::Playing);
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn paused_core_wakes_for_a_new_command_deadline() {
    let (engine, clock, _bridge, _gate) = setup_engine().await;
    init_ready_bridge(&engine, "bridge-1", 1).await;
    engine
        .on_bridge_playback_observed(
            2,
            make_obs(
                "bridge-1",
                PlaybackStatus::Paused,
                "ti-1",
                "vid-1",
                "qe-1",
                1_000,
                Some(200_000),
            ),
        )
        .await
        .unwrap();

    let command_id = "paused-timeout";
    let receipt = engine
        .submit_player_command(make_cmd(command_id, PlayerOp::Play, None))
        .await
        .unwrap();
    assert_eq!(receipt.status, CommandLifecycleStatus::Dispatched);
    clock.advance(DEFAULT_CONTROL_TIMEOUT_MS + 500);

    let receipt = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let receipt = engine
                .command_receipt(CommandId::new(command_id).unwrap())
                .await
                .unwrap()
                .unwrap();
            if receipt.status == CommandLifecycleStatus::Indeterminate {
                break receipt;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("automatic command deadline after an idle pause");
    assert_eq!(receipt.error.unwrap().code, QueryErrorCode::Timeout);
}

#[tokio::test]
async fn non_idempotent_timeout_checks_snapshot_and_settles_indeterminate_without_retry() {
    let (engine, clock, bridge, _gate) = setup_engine().await;
    init_ready_bridge(&engine, "bridge-1", 1).await;
    engine
        .on_bridge_playback_observed(
            2,
            make_obs(
                "bridge-1",
                PlaybackStatus::Playing,
                "ti-1",
                "vid-1",
                "qe-1",
                1_000,
                Some(200_000),
            ),
        )
        .await
        .unwrap();

    for (idx, op) in [
        PlayerOp::Next,
        PlayerOp::Previous,
        PlayerOp::Toggle,
        PlayerOp::Enqueue,
        PlayerOp::PlayTarget,
    ]
    .into_iter()
    .enumerate()
    {
        let mut args = make_args();
        if matches!(op, PlayerOp::Enqueue | PlayerOp::PlayTarget) {
            args.target = Some(PlaybackRef {
                video_id: VideoId::new("vid-x").unwrap(),
                playlist_id: None,
                playlist_entry_id: None,
                browse_id: None,
                queue_entry_id: None,
                start_index: None,
            });
            // Next needs no verified queue end, so the command reaches the timeout path.
            args.enqueue_position = Some(EnqueuePosition::Next);
        }
        let cmd_id = format!("cmd-timeout-{idx}");
        let cmd = make_cmd(&cmd_id, op, Some(args));
        engine.submit_player_command(cmd).await.unwrap();

        let advance_ms = if op == PlayerOp::PlayTarget {
            15_500
        } else {
            DEFAULT_CONTROL_TIMEOUT_MS + 500
        };
        clock.advance(advance_ms);
        engine.tick().await.unwrap();

        let receipt = engine
            .command_receipt(CommandId::new(&cmd_id).unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            receipt.status,
            CommandLifecycleStatus::Indeterminate,
            "op {op:?} must settle at Indeterminate on timeout"
        );
        assert_eq!(
            receipt.error.as_ref().map(|e| e.code),
            Some(QueryErrorCode::Timeout)
        );
    }

    // Exactly 5 dispatches (1 per command, zero blind retries).
    assert_eq!(bridge.sent_commands().len(), 5);
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn command_id_deduplication_and_bounds() {
    let (engine, clock, bridge, _gate) = setup_engine().await;
    init_ready_bridge(&engine, "bridge-1", 1).await;
    engine
        .on_bridge_playback_observed(
            2,
            make_obs(
                "bridge-1",
                PlaybackStatus::Paused,
                "ti-1",
                "vid-1",
                "qe-1",
                0,
                Some(200_000),
            ),
        )
        .await
        .unwrap();

    let first = engine
        .submit_player_command(make_cmd("cmd-dup", PlayerOp::Toggle, None))
        .await
        .unwrap();
    assert!(!first.deduplicated);
    // Toggle while Paused resolved to Play.
    assert_eq!(bridge.sent_ops(), vec![PlayerOp::Play]);

    let second = engine
        .submit_player_command(make_cmd("cmd-dup", PlayerOp::Toggle, None))
        .await
        .unwrap();
    assert!(second.deduplicated);
    assert_eq!(bridge.sent_ops().len(), 1);
    let conflict = engine
        .submit_player_command(make_cmd("cmd-dup", PlayerOp::Next, None))
        .await
        .unwrap_err();
    assert_eq!(conflict.code, QueryErrorCode::InvalidPayload);
    assert_eq!(bridge.sent_ops().len(), 1);

    // Complete first toggle, then advance past 10 min TTL -> same commandId can run again.
    engine
        .on_bridge_playback_observed(
            3,
            make_obs(
                "bridge-1",
                PlaybackStatus::Playing,
                "ti-1",
                "vid-1",
                "qe-1",
                500,
                Some(200_000),
            ),
        )
        .await
        .unwrap();

    clock.advance(DEDUP_TTL_MS + 1);
    let after_ttl = engine
        .submit_player_command(make_cmd("cmd-dup", PlayerOp::Toggle, None))
        .await
        .unwrap();
    assert!(!after_ttl.deduplicated);
    // Toggle while Playing resolved to Pause!
    assert_eq!(bridge.sent_ops(), vec![PlayerOp::Play, PlayerOp::Pause]);

    // Verify dedup cache bound of 256 entries.
    engine
        .on_bridge_playback_observed(
            4,
            make_obs(
                "bridge-1",
                PlaybackStatus::Paused,
                "ti-1",
                "vid-1",
                "qe-1",
                500,
                Some(200_000),
            ),
        )
        .await
        .unwrap();
    for (seq, i) in (5u64..).zip(0..(MAX_DEDUP_ENTRIES + 5)) {
        let cid = format!("cmd-fill-{i}");
        let mut args = make_args();
        args.volume = Some(0.5);
        engine
            .submit_player_command(make_cmd(&cid, PlayerOp::SetVolume, Some(args)))
            .await
            .unwrap();
        let mut obs = make_obs(
            "bridge-1",
            PlaybackStatus::Paused,
            "ti-1",
            "vid-1",
            "qe-1",
            500,
            Some(200_000),
        );
        obs.volume = 0.5;
        engine.on_bridge_playback_observed(seq, obs).await.unwrap();
    }
    // Oldest entry ("cmd-fill-0") should have been evicted.
    assert!(
        engine
            .command_receipt(CommandId::new("cmd-fill-0").unwrap())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        engine
            .command_receipt(CommandId::new(format!("cmd-fill-{}", MAX_DEDUP_ENTRIES + 4)).unwrap())
            .await
            .unwrap()
            .is_some()
    );
    engine.shutdown().await.unwrap();
}

fn playback_fence(clock: &MockClock, generation: u64) -> PlaybackFence {
    PlaybackFence {
        account_scope: None,
        account_epoch: None,
        bridge_epoch: BridgeEpoch::new("bridge-1").unwrap(),
        generation,
        track_instance_id: Some(TrackInstanceId::new("ti-1").unwrap()),
        queue_revision: None,
        deadline_ms: clock.now_ms() + 500,
        cancel: CancellationToken::new(),
    }
}

#[tokio::test]
async fn manual_pause_takes_the_playback_slot_without_waiting_for_an_external_observation() {
    let (engine, clock, bridge, _) = setup_engine().await;
    init_ready_bridge(&engine, "bridge-1", 1).await;
    engine
        .on_bridge_playback_observed(
            2,
            make_obs(
                "bridge-1",
                PlaybackStatus::Playing,
                "ti-1",
                "vid-1",
                "qe-1",
                0,
                Some(180_000),
            ),
        )
        .await
        .unwrap();
    let fence = playback_fence(&clock, 0);
    let mut sub = engine.subscribe_snapshot().await.unwrap().1;
    let one = engine
        .submit_external_player_command(make_cmd("auto-next", PlayerOp::Next, None), fence.clone())
        .await
        .unwrap();
    assert_eq!(one.status, CommandLifecycleStatus::Dispatched);
    let two = engine
        .submit_external_player_command(
            make_cmd("auto-next-2", PlayerOp::Next, None),
            fence.clone(),
        )
        .await
        .unwrap();
    assert_eq!(two.status, CommandLifecycleStatus::Accepted);

    let pause = engine
        .submit_player_command(make_cmd("local-pause", PlayerOp::Pause, None))
        .await
        .unwrap();
    assert_eq!(pause.status, CommandLifecycleStatus::Dispatched);
    assert_eq!(bridge.sent_ops(), [PlayerOp::Next, PlayerOp::Pause]);
    for (id, status) in [
        ("auto-next", CommandLifecycleStatus::Indeterminate),
        ("auto-next-2", CommandLifecycleStatus::Cancelled),
    ] {
        assert_eq!(
            engine
                .command_receipt(CommandId::new(id).unwrap())
                .await
                .unwrap()
                .unwrap()
                .status,
            status
        );
        let mut seen = false;
        while let Ok(delta) = sub.try_recv() {
            if matches!(delta, StateDelta::CommandUpdated { receipt, .. } if receipt.command_id.as_str() == id && receipt.status == status)
            {
                seen = true;
                break;
            }
        }
        assert!(seen);
    }
    assert!(
        !fence.cancel.is_cancelled(),
        "local playback takeover must not revoke the caller's other work"
    );
    let stale = engine
        .submit_external_player_command(make_cmd("obsolete-resume", PlayerOp::Play, None), fence)
        .await
        .unwrap_err();
    assert_eq!(stale.code, QueryErrorCode::StaleContext);
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn manual_pause_cancels_an_external_skip_waiting_for_the_output_gate() {
    let (engine, clock, bridge, gate) = setup_engine().await;
    init_ready_bridge(&engine, "bridge-1", 1).await;
    engine
        .on_bridge_playback_observed(
            2,
            make_obs(
                "bridge-1",
                PlaybackStatus::Paused,
                "ti-1",
                "vid-1",
                "qe-1",
                0,
                Some(180_000),
            ),
        )
        .await
        .unwrap();
    let hold = Arc::new(Notify::new());
    *gate.pending_engage.lock().unwrap() = Some(hold.clone());
    let external = engine.clone();
    let fence = playback_fence(&clock, 0);
    let next = tokio::spawn(async move {
        external
            .submit_external_player_command(make_cmd("waiting-gate", PlayerOp::Next, None), fence)
            .await
    });
    gate.started.notified().await;
    engine
        .submit_player_command(make_cmd("local-pause", PlayerOp::Pause, None))
        .await
        .unwrap();
    assert_eq!(bridge.sent_ops(), [PlayerOp::Pause]);
    hold.notify_one();
    assert_eq!(
        next.await.unwrap().unwrap_err().code,
        QueryErrorCode::StaleContext
    );
    settle(&engine).await;
    let result = engine
        .command_receipt(CommandId::new("waiting-gate").unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.status, CommandLifecycleStatus::Cancelled);
    let engaged = gate.calls()[0].0.clone();
    assert_eq!(gate.calls(), [(engaged.clone(), true), (engaged, false)]);
    assert_eq!(bridge.sent_ops(), [PlayerOp::Pause]);
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn dispatch_rechecks_external_grant_expiry_after_waiting_for_a_manual_command() {
    let (engine, clock, bridge, _) = setup_engine().await;
    init_ready_bridge(&engine, "bridge-1", 1).await;
    engine
        .on_bridge_playback_observed(
            2,
            make_obs(
                "bridge-1",
                PlaybackStatus::Playing,
                "ti-1",
                "vid-1",
                "qe-1",
                0,
                Some(180_000),
            ),
        )
        .await
        .unwrap();
    engine
        .submit_player_command(make_cmd("manual-pause", PlayerOp::Pause, None))
        .await
        .unwrap();
    let permit = playback_fence(&clock, 1);
    let external = engine
        .submit_external_player_command(make_cmd("expired-play", PlayerOp::Play, None), permit)
        .await
        .unwrap();
    assert_eq!(external.status, CommandLifecycleStatus::Accepted);
    clock.advance(501);
    engine
        .on_bridge_playback_observed(
            3,
            make_obs(
                "bridge-1",
                PlaybackStatus::Paused,
                "ti-1",
                "vid-1",
                "qe-1",
                100,
                Some(180_000),
            ),
        )
        .await
        .unwrap();
    settle(&engine).await;
    let result = engine
        .command_receipt(CommandId::new("expired-play").unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.status, CommandLifecycleStatus::Cancelled);
    assert_eq!(bridge.sent_ops(), [PlayerOp::Pause]);
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_unattributed_same_track_pause_invalidates_an_external_playback_plan() {
    let (engine, clock, bridge, _) = setup_engine().await;
    init_ready_bridge(&engine, "bridge-1", 1).await;
    engine
        .on_bridge_playback_observed(
            2,
            make_obs(
                "bridge-1",
                PlaybackStatus::Playing,
                "ti-1",
                "vid-1",
                "qe-1",
                0,
                Some(180_000),
            ),
        )
        .await
        .unwrap();
    let permit = playback_fence(&clock, 0);
    engine
        .submit_external_player_command(make_cmd("auto-next", PlayerOp::Next, None), permit.clone())
        .await
        .unwrap();
    engine
        .on_bridge_playback_observed(
            3,
            make_obs(
                "bridge-1",
                PlaybackStatus::Paused,
                "ti-1",
                "vid-1",
                "qe-1",
                100,
                Some(180_000),
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        engine
            .command_receipt(CommandId::new("auto-next").unwrap())
            .await
            .unwrap()
            .unwrap()
            .status,
        CommandLifecycleStatus::Indeterminate
    );
    let resume = engine
        .submit_external_player_command(make_cmd("auto-resume", PlayerOp::Play, None), permit)
        .await
        .unwrap_err();
    assert_eq!(resume.code, QueryErrorCode::StaleContext);
    assert_eq!(bridge.sent_ops(), [PlayerOp::Next]);
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn resource_invalidation_keeps_the_entity_and_epoch_of_the_verified_write() {
    let (engine, _, _, _) = setup_engine().await;
    let current = AccountEpoch::new("account-current").unwrap();
    engine
        .on_session_material(SessionMaterialPayload {
            account_epoch: current.clone(),
            credential_revision: CredentialRevision::new("cred-1").unwrap(),
            auth_user_index: None,
            cookie_pairs: BTreeMap::new(),
            channel_label: None,
        })
        .await
        .unwrap();
    let (_, mut events) = engine.subscribe_snapshot().await.unwrap();
    engine
        .invalidate_control_resource(
            AccountEpoch::new("account-old").unwrap(),
            "playlistTracks".into(),
            "old-playlist".into(),
        )
        .await
        .unwrap();
    engine
        .invalidate_control_resource(current, "playlistTracks".into(), "new-playlist".into())
        .await
        .unwrap();
    engine.snapshot().await.unwrap();
    let delta = events.try_recv().unwrap();
    assert!(matches!(delta, StateDelta::ResourceInvalidated {
        resource_kind, resource_id: Some(id), ..
    } if resource_kind == "playlistTracks" && id == "new-playlist"));
    assert!(events.try_recv().is_err());
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_refreshed_receipt_does_not_extend_the_ttl_of_later_ids() {
    let (engine, clock, bridge, _gate) = setup_engine().await;
    init_ready_bridge(&engine, "bridge-1", 1).await;
    engine
        .on_bridge_playback_observed(
            2,
            make_obs(
                "bridge-1",
                PlaybackStatus::Playing,
                "ti-1",
                "vid-1",
                "qe-1",
                0,
                Some(180_000),
            ),
        )
        .await
        .unwrap();
    engine
        .submit_player_command(make_cmd("pending-first", PlayerOp::Pause, None))
        .await
        .unwrap();
    let rejected = engine
        .submit_player_command(make_cmd("later-rejected", PlayerOp::Seek, None))
        .await
        .unwrap_err();
    assert_eq!(rejected.code, QueryErrorCode::InvalidPayload);
    clock.advance(500);
    engine
        .on_bridge_playback_observed(
            3,
            make_obs(
                "bridge-1",
                PlaybackStatus::Paused,
                "ti-1",
                "vid-1",
                "qe-1",
                0,
                Some(180_000),
            ),
        )
        .await
        .unwrap();
    clock.advance(DEDUP_TTL_MS - 499);
    assert!(
        engine
            .command_receipt(CommandId::new("later-rejected").unwrap())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        engine
            .command_receipt(CommandId::new("pending-first").unwrap())
            .await
            .unwrap()
            .is_some()
    );
    let mut seek = make_args();
    seek.position_ms = Some(15_000);
    let receipt = engine
        .submit_player_command(make_cmd("later-rejected", PlayerOp::Seek, Some(seek)))
        .await
        .unwrap();
    assert!(!receipt.deduplicated);
    assert_eq!(bridge.sent_ops(), vec![PlayerOp::Pause, PlayerOp::Seek]);
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn enqueue_proves_the_requested_position_and_retains_existing_occurrences() {
    // `Explicit` is a fully loaded explicit list followed by an autoplay tail.
    #[derive(Clone, Copy, Debug)]
    enum Loaded {
        Complete,
        Partial,
        Explicit,
    }
    let snapshot =
        |revision, current: &str, loaded: Loaded, items: &[(&str, &str)]| QueueSnapshot {
            bridge_epoch: Some(BridgeEpoch::new("bridge-1").unwrap()),
            queue_revision: revision,
            current_entry_id: Some(QueueEntryId::new(current).unwrap()),
            loaded_range: None,
            completeness: if matches!(loaded, Loaded::Complete) {
                QueueCompleteness::Complete
            } else {
                QueueCompleteness::Partial
            },
            has_more: Some(!matches!(loaded, Loaded::Complete)),
            explicit_complete: Some(!matches!(loaded, Loaded::Partial)),
            may_load_more: None,
            official_upcoming: None,
            filters: None,
            entries: items
                .iter()
                .map(|(entry_id, vid)| QueueEntry {
                    entry_id: QueueEntryId::new(*entry_id).unwrap(),
                    video_id: VideoId::new(*vid).unwrap(),
                    track: make_track(vid, vid, None),
                    availability: TrackAvailability::Available,
                })
                .collect(),
        };
    let initial = [
        ("qe-a", "songA"),
        ("qe-b", "songB"),
        ("qe-repeat", "repeat"),
    ];
    let next = [initial[0], ("qe-added", "repeat"), initial[1], initial[2]];
    let last = [initial[0], initial[1], initial[2], ("qe-added", "repeat")];
    let reordered = [initial[0], ("qe-added", "repeat"), initial[2], initial[1]];
    let cases = [
        (
            EnqueuePosition::Next,
            &last,
            Loaded::Complete,
            "qe-a",
            false,
        ),
        (EnqueuePosition::Next, &next, Loaded::Complete, "qe-a", true),
        (EnqueuePosition::Next, &next, Loaded::Partial, "qe-a", true),
        (
            EnqueuePosition::Next,
            &reordered,
            Loaded::Complete,
            "qe-a",
            false,
        ),
        (
            EnqueuePosition::Next,
            &next,
            Loaded::Complete,
            "qe-b",
            false,
        ),
        (
            EnqueuePosition::Last,
            &next,
            Loaded::Complete,
            "qe-a",
            false,
        ),
        (EnqueuePosition::Last, &last, Loaded::Complete, "qe-a", true),
        (EnqueuePosition::Last, &last, Loaded::Explicit, "qe-a", true),
        (
            EnqueuePosition::Last,
            &next,
            Loaded::Explicit,
            "qe-a",
            false,
        ),
    ];
    for (position, observed, loaded, current, verified) in cases {
        let complete = loaded;
        let (engine, _, bridge, _) = setup_engine().await;
        init_ready_bridge(&engine, "bridge-1", 1).await;
        engine
            .on_bridge_queue_observed(2, snapshot(1, "qe-a", complete, &initial))
            .await
            .unwrap();
        engine
            .on_bridge_playback_observed(
                3,
                make_obs(
                    "bridge-1",
                    PlaybackStatus::Playing,
                    "ti-1",
                    "songA",
                    "qe-a",
                    0,
                    Some(180_000),
                ),
            )
            .await
            .unwrap();
        let mut args = make_args();
        args.target = Some(PlaybackRef {
            video_id: VideoId::new("repeat").unwrap(),
            playlist_id: None,
            playlist_entry_id: None,
            browse_id: None,
            queue_entry_id: None,
            start_index: None,
        });
        args.enqueue_position = Some(position);
        args.queue_revision = Some(1);
        engine
            .submit_player_command(make_cmd("enqueue", PlayerOp::Enqueue, Some(args)))
            .await
            .unwrap();
        engine
            .on_bridge_queue_observed(4, snapshot(2, current, complete, observed))
            .await
            .unwrap();
        let receipt = engine
            .command_receipt(CommandId::new("enqueue").unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            receipt.status == CommandLifecycleStatus::Observed,
            verified,
            "{position:?}, loaded={complete:?}, {observed:?}, current={current}"
        );
        assert_eq!(bridge.sent_ops(), vec![PlayerOp::Enqueue]);
        engine.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn append_without_a_loaded_queue_end_is_refused_before_the_page_sees_it() {
    let (engine, _, bridge, _) = setup_engine().await;
    init_ready_bridge(&engine, "bridge-1", 1).await;
    let truncated = QueueSnapshot {
        bridge_epoch: Some(BridgeEpoch::new("bridge-1").unwrap()),
        queue_revision: 1,
        current_entry_id: Some(QueueEntryId::new("qe-a").unwrap()),
        loaded_range: Some(QueueRange {
            start: 0,
            end_exclusive: 1,
        }),
        completeness: QueueCompleteness::Partial,
        has_more: Some(true),
        explicit_complete: Some(false),
        may_load_more: None,
        official_upcoming: None,
        filters: None,
        entries: vec![QueueEntry {
            entry_id: QueueEntryId::new("qe-a").unwrap(),
            video_id: VideoId::new("songA").unwrap(),
            track: make_track("songA", "songA", None),
            availability: TrackAvailability::Available,
        }],
    };
    engine.on_bridge_queue_observed(2, truncated).await.unwrap();
    let mut args = make_args();
    args.target = Some(PlaybackRef {
        video_id: VideoId::new("added").unwrap(),
        playlist_id: None,
        playlist_entry_id: None,
        browse_id: None,
        queue_entry_id: None,
        start_index: None,
    });
    args.enqueue_position = Some(EnqueuePosition::Last);
    let refused = engine
        .submit_player_command(make_cmd("append", PlayerOp::Enqueue, Some(args)))
        .await;
    assert_eq!(refused.unwrap_err().code, QueryErrorCode::StaleContext);
    assert!(bridge.sent_ops().is_empty());
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn queue_edits_require_a_complete_queue_and_prove_the_exact_occurrence_order() {
    let current = ("qe-playing", "anchor");
    let first = ("qe-1", "same-video");
    let second = ("qe-2", "same-video");
    let last = ("qe-last", "last-video");
    let initial = [current, first, second, last];
    let snapshot = |revision, complete, items: &[(&str, &str)]| QueueSnapshot {
        bridge_epoch: Some(BridgeEpoch::new("bridge-1").unwrap()),
        queue_revision: revision,
        current_entry_id: Some(QueueEntryId::new(current.0).unwrap()),
        loaded_range: Some(QueueRange {
            start: 0,
            end_exclusive: items.len() as u32,
        }),
        completeness: if complete {
            QueueCompleteness::Complete
        } else {
            QueueCompleteness::Partial
        },
        has_more: Some(!complete),
        explicit_complete: None,
        may_load_more: None,
        official_upcoming: None,
        filters: None,
        entries: items
            .iter()
            .map(|(entry_id, video_id)| QueueEntry {
                entry_id: QueueEntryId::new(*entry_id).unwrap(),
                video_id: VideoId::new(*video_id).unwrap(),
                track: make_track(video_id, video_id, None),
                availability: TrackAvailability::Available,
            })
            .collect(),
    };
    let cases = [
        (
            PlayerOp::RemoveQueueEntry,
            first.0,
            None,
            true,
            &[current, second, last][..],
            true,
            true,
        ),
        (
            PlayerOp::RemoveQueueEntry,
            first.0,
            None,
            true,
            &[current, first, last],
            true,
            false,
        ),
        (
            PlayerOp::RemoveQueueEntry,
            current.0,
            None,
            true,
            &[],
            false,
            false,
        ),
        (
            PlayerOp::MoveQueueEntry,
            first.0,
            None,
            false,
            &[],
            false,
            false,
        ),
        (
            PlayerOp::MoveQueueEntry,
            first.0,
            None,
            true,
            &[current, second, last, first],
            true,
            true,
        ),
        (
            PlayerOp::MoveQueueEntry,
            last.0,
            Some(first.0),
            true,
            &[current, last, first, second],
            true,
            true,
        ),
        (
            PlayerOp::MoveQueueEntry,
            last.0,
            Some(first.0),
            true,
            &[current, second, last, first],
            true,
            false,
        ),
    ];
    for (op, entry, before, complete, observed, dispatches, verifies) in cases {
        let (engine, _, bridge, _) = setup_engine().await;
        init_ready_bridge(&engine, "bridge-1", 1).await;
        engine
            .on_bridge_queue_observed(2, snapshot(1, complete, &initial))
            .await
            .unwrap();
        engine
            .on_bridge_playback_observed(
                3,
                make_obs(
                    "bridge-1",
                    PlaybackStatus::Playing,
                    "ti-edit",
                    current.1,
                    current.0,
                    0,
                    None,
                ),
            )
            .await
            .unwrap();
        let mut args = make_args();
        args.queue_revision = Some(1);
        args.queue_entry_id = Some(QueueEntryId::new(entry).unwrap());
        args.before_queue_entry_id = before.map(|id| QueueEntryId::new(id).unwrap());
        let submitted = engine
            .submit_player_command(make_cmd("edit", op, Some(args)))
            .await;
        assert_eq!(submitted.is_ok(), dispatches);
        if dispatches {
            engine
                .on_bridge_queue_observed(4, snapshot(2, true, observed))
                .await
                .unwrap();
            assert_eq!(bridge.sent_ops(), vec![op]);
            let receipt = engine
                .command_receipt(CommandId::new("edit").unwrap())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                receipt.status == CommandLifecycleStatus::Observed,
                verifies,
                "{op:?}, {observed:?}"
            );
        } else {
            assert_eq!(submitted.unwrap_err().code, QueryErrorCode::StaleContext);
            assert!(bridge.sent_ops().is_empty());
        }
        engine.shutdown().await.unwrap();
    }
}

const CLEAR_QUEUE: [(&str, &str); 5] = [
    ("qe-past", "repeat"),
    ("qe-recent", "another"),
    ("qe-current", "repeat"),
    ("qe-upcoming", "repeat"),
    ("qe-tail", "tail"),
];

fn clear_snapshot(revision: u64, complete: bool, items: &[(&str, &str)]) -> QueueSnapshot {
    QueueSnapshot {
        bridge_epoch: Some(BridgeEpoch::new("bridge-1").unwrap()),
        queue_revision: revision,
        current_entry_id: Some(QueueEntryId::new("qe-current").unwrap()),
        loaded_range: Some(QueueRange {
            start: 0,
            end_exclusive: items.len() as u32,
        }),
        completeness: if complete {
            QueueCompleteness::Complete
        } else {
            QueueCompleteness::Partial
        },
        has_more: Some(!complete),
        explicit_complete: None,
        may_load_more: None,
        official_upcoming: None,
        filters: None,
        entries: items
            .iter()
            .map(|(entry, video)| QueueEntry {
                entry_id: QueueEntryId::new(*entry).unwrap(),
                video_id: VideoId::new(*video).unwrap(),
                track: make_track(video, video, None),
                availability: TrackAvailability::Available,
            })
            .collect(),
    }
}

async fn clear_harness(
    complete: bool,
    items: &[(&str, &str)],
) -> (CoreEngine, Arc<MockClock>, Arc<RecordingBridge>) {
    let (engine, clock, bridge, _) = setup_engine().await;
    init_ready_bridge(&engine, "bridge-1", 1).await;
    engine
        .on_bridge_queue_observed(2, clear_snapshot(7, complete, items))
        .await
        .unwrap();
    engine
        .on_bridge_playback_observed(
            3,
            make_obs(
                "bridge-1",
                PlaybackStatus::Playing,
                "ti-clear",
                "repeat",
                "qe-current",
                12_000,
                None,
            ),
        )
        .await
        .unwrap();
    (engine, clock, bridge)
}

fn clear_command(id: &str) -> PlayerCommandPayload {
    let mut args = make_args();
    args.track_instance_id = Some(TrackInstanceId::new("ti-clear").unwrap());
    args.queue_entry_id = Some(QueueEntryId::new("qe-current").unwrap());
    args.queue_revision = Some(7);
    make_cmd(id, PlayerOp::ClearUpcoming, Some(args))
}

async fn acknowledge_clear(engine: &CoreEngine, command_id: &CommandId, seq: u64) {
    assert!(
        engine
            .on_bridge_command_ack(
                seq,
                BridgeCommandAckPayload {
                    command_id: command_id.clone(),
                    bridge_epoch: BridgeEpoch::new("bridge-1").unwrap(),
                    dispatched: true,
                    error: None,
                },
            )
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn clear_upcoming_requires_readback_of_the_exact_played_prefix() {
    let (engine, _, bridge) = clear_harness(true, &CLEAR_QUEUE).await;
    let command = clear_command("clear-verify");
    let id = command.command_id.clone();
    assert_eq!(
        engine
            .submit_player_command(command.clone())
            .await
            .unwrap()
            .status,
        CommandLifecycleStatus::Dispatched
    );
    assert_eq!(
        bridge.sent_commands(),
        vec![(BridgeEpoch::new("bridge-1").unwrap(), command)]
    );
    acknowledge_clear(&engine, &id, 4).await;
    assert_eq!(
        engine
            .command_receipt(id.clone())
            .await
            .unwrap()
            .unwrap()
            .status,
        CommandLifecycleStatus::Dispatched
    );

    // The page removed one tail item, but the next occurrence is still queued.
    engine
        .on_bridge_queue_observed(5, clear_snapshot(8, true, &CLEAR_QUEUE[..4]))
        .await
        .unwrap();
    assert_eq!(
        engine
            .command_receipt(id.clone())
            .await
            .unwrap()
            .unwrap()
            .status,
        CommandLifecycleStatus::Dispatched
    );

    engine
        .on_bridge_queue_observed(6, clear_snapshot(9, true, &CLEAR_QUEUE[..3]))
        .await
        .unwrap();
    let receipt = engine.command_receipt(id).await.unwrap().unwrap();
    assert_eq!(receipt.status, CommandLifecycleStatus::Observed);
    assert!(receipt.observed_at_ms.is_some());
    let snapshot = engine.snapshot().await.unwrap();
    assert_eq!(snapshot.pending.pending_command_count, 0);
    assert_eq!(snapshot.player.playback, PlaybackStatus::Playing);
    assert_eq!(
        snapshot.player.track_instance_id.unwrap().as_str(),
        "ti-clear"
    );
    assert_eq!(
        snapshot.player.current_queue_entry_id.unwrap().as_str(),
        "qe-current"
    );
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn clear_upcoming_never_confirms_partial_or_wrong_prefix_after_a_positive_ack() {
    let wrong_prefix = [CLEAR_QUEUE[3], CLEAR_QUEUE[1], CLEAR_QUEUE[2]];
    let wrong_video = [("qe-past", "other"), CLEAR_QUEUE[1], CLEAR_QUEUE[2]];
    let cases = [
        ("left a tail entry", &CLEAR_QUEUE[..4], true),
        ("same video at the wrong occurrence", &wrong_prefix, true),
        (
            "wrong video under the preserved entry id",
            &wrong_video,
            true,
        ),
        (
            "matching prefix in a partial window",
            &CLEAR_QUEUE[..3],
            false,
        ),
    ];
    for (case, entries, complete) in cases {
        let (engine, clock, bridge) = clear_harness(true, &CLEAR_QUEUE).await;
        let command = clear_command("clear-unverified");
        let id = command.command_id.clone();
        engine.submit_player_command(command).await.unwrap();
        acknowledge_clear(&engine, &id, 4).await;
        engine
            .on_bridge_queue_observed(5, clear_snapshot(8, complete, entries))
            .await
            .unwrap();
        clock.advance(DEFAULT_CONTROL_TIMEOUT_MS + 500);
        engine.tick().await.unwrap();
        let receipt = engine.command_receipt(id).await.unwrap().unwrap();
        assert_eq!(
            receipt.status,
            CommandLifecycleStatus::Indeterminate,
            "{case}"
        );
        assert_eq!(
            receipt.error.unwrap().code,
            QueryErrorCode::Timeout,
            "{case}"
        );
        assert_eq!(bridge.sent_ops(), vec![PlayerOp::ClearUpcoming]);
        engine.shutdown().await.unwrap();
    }
}

/// A queue that may continue past its loaded entries is cleared best effort: the command is
/// observed once no loaded upcoming entry remains, even if more entries loaded after the current
/// one meanwhile, while the earlier occurrences must still sit where they were.
#[tokio::test]
async fn clear_upcoming_on_an_open_queue_confirms_once_no_loaded_upcoming_entry_remains() {
    let continuation = |revision, items: &[(&str, &str)]| QueueSnapshot {
        may_load_more: Some(true),
        official_upcoming: None,
        filters: None,
        ..clear_snapshot(revision, true, items)
    };
    let loaded_later = [
        CLEAR_QUEUE[0],
        CLEAR_QUEUE[1],
        CLEAR_QUEUE[2],
        ("qe-loaded-later", "later"),
    ];
    let moved_prefix = [CLEAR_QUEUE[1], CLEAR_QUEUE[0], CLEAR_QUEUE[2]];
    let cases = [
        (
            "continuation",
            continuation(7, &CLEAR_QUEUE),
            continuation(8, &loaded_later),
            true,
        ),
        (
            "truncated window",
            clear_snapshot(7, false, &CLEAR_QUEUE),
            clear_snapshot(8, false, &CLEAR_QUEUE[..3]),
            true,
        ),
        (
            "a loaded upcoming entry remains",
            continuation(7, &CLEAR_QUEUE),
            continuation(8, &CLEAR_QUEUE[..4]),
            false,
        ),
        (
            "an earlier occurrence moved",
            continuation(7, &CLEAR_QUEUE),
            continuation(8, &moved_prefix),
            false,
        ),
    ];
    for (case, before, after, verifies) in cases {
        let (engine, _, bridge) = clear_harness(true, &CLEAR_QUEUE).await;
        engine.on_bridge_queue_observed(4, before).await.unwrap();
        let mut command = clear_command("clear-open");
        command.args.as_mut().unwrap().queue_revision = Some(7);
        let id = command.command_id.clone();
        assert_eq!(
            engine.submit_player_command(command).await.unwrap().status,
            CommandLifecycleStatus::Dispatched,
            "{case}"
        );
        acknowledge_clear(&engine, &id, 5).await;
        engine.on_bridge_queue_observed(6, after).await.unwrap();
        let status = engine.command_receipt(id).await.unwrap().unwrap().status;
        assert_eq!(
            status == CommandLifecycleStatus::Observed,
            verifies,
            "{case}"
        );
        assert_eq!(bridge.sent_ops(), vec![PlayerOp::ClearUpcoming], "{case}");
        engine.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn clear_upcoming_rejects_stale_context_before_dispatch() {
    let (engine, _, bridge) = clear_harness(true, &CLEAR_QUEUE).await;
    engine
        .on_bridge_queue_observed(4, clear_snapshot(8, true, &CLEAR_QUEUE))
        .await
        .unwrap();
    let stale = engine
        .submit_player_command(clear_command("clear-old-revision"))
        .await
        .unwrap_err();
    assert_eq!(stale.code, QueryErrorCode::StaleContext);

    let mut wrong_track = clear_command("clear-old-track");
    let args = wrong_track.args.as_mut().unwrap();
    args.queue_revision = Some(8);
    args.track_instance_id = Some(TrackInstanceId::new("ti-old").unwrap());
    assert_eq!(
        engine
            .submit_player_command(wrong_track)
            .await
            .unwrap_err()
            .code,
        QueryErrorCode::StaleContext
    );
    assert!(bridge.sent_ops().is_empty());
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn clear_upcoming_no_op_waits_for_the_matching_page_ack() {
    let (engine, _, bridge) = clear_harness(true, &CLEAR_QUEUE[..3]).await;
    let command = clear_command("clear-empty");
    let id = command.command_id.clone();
    engine.submit_player_command(command).await.unwrap();
    assert_eq!(
        engine
            .command_receipt(id.clone())
            .await
            .unwrap()
            .unwrap()
            .status,
        CommandLifecycleStatus::Dispatched
    );

    acknowledge_clear(&engine, &CommandId::new("unrelated").unwrap(), 4).await;
    engine
        .on_bridge_queue_observed(5, clear_snapshot(7, true, &CLEAR_QUEUE[..3]))
        .await
        .unwrap();
    assert_eq!(
        engine
            .command_receipt(id.clone())
            .await
            .unwrap()
            .unwrap()
            .status,
        CommandLifecycleStatus::Dispatched
    );

    acknowledge_clear(&engine, &id, 6).await;
    let receipt = engine.command_receipt(id).await.unwrap().unwrap();
    assert_eq!(receipt.status, CommandLifecycleStatus::Observed);
    assert_eq!(
        engine
            .snapshot()
            .await
            .unwrap()
            .queue_summary
            .queue_revision,
        7
    );
    assert_eq!(bridge.sent_ops(), vec![PlayerOp::ClearUpcoming]);
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn clear_upcoming_no_op_honors_page_rejection_and_ack_sequence_gaps() {
    let (engine, _, bridge) = clear_harness(true, &CLEAR_QUEUE[..3]).await;
    let command = clear_command("clear-rejected-by-page");
    let id = command.command_id.clone();
    engine.submit_player_command(command).await.unwrap();
    engine
        .on_bridge_command_ack(
            4,
            BridgeCommandAckPayload {
                command_id: id.clone(),
                bridge_epoch: BridgeEpoch::new("bridge-1").unwrap(),
                dispatched: false,
                error: Some(QueryError {
                    code: QueryErrorCode::StaleContext,
                    retriable: false,
                    retry_after_ms: None,
                    safe_message: "Queue changed at the page".into(),
                    request_id: None,
                }),
            },
        )
        .await
        .unwrap();
    let rejected = engine.command_receipt(id).await.unwrap().unwrap();
    assert_eq!(rejected.status, CommandLifecycleStatus::Failed);
    assert_eq!(rejected.error.unwrap().code, QueryErrorCode::StaleContext);

    let command = clear_command("clear-after-gap");
    let id = command.command_id.clone();
    engine.submit_player_command(command).await.unwrap();
    acknowledge_clear(&engine, &id, 6).await;
    assert_eq!(
        engine
            .command_receipt(id.clone())
            .await
            .unwrap()
            .unwrap()
            .status,
        CommandLifecycleStatus::Dispatched
    );
    engine
        .on_bridge_queue_observed(7, clear_snapshot(8, true, &CLEAR_QUEUE[..3]))
        .await
        .unwrap();
    assert_eq!(
        engine.command_receipt(id).await.unwrap().unwrap().status,
        CommandLifecycleStatus::Observed
    );
    assert_eq!(
        bridge.sent_ops(),
        vec![PlayerOp::ClearUpcoming, PlayerOp::ClearUpcoming]
    );
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn seek_and_volume_coalesce_to_latest_and_reject_stale_track_instance() {
    let (engine, _clock, bridge, _gate) = setup_engine().await;
    init_ready_bridge(&engine, "bridge-1", 1).await;
    engine
        .on_bridge_playback_observed(
            2,
            make_obs(
                "bridge-1",
                PlaybackStatus::Playing,
                "ti-1",
                "vid-1",
                "qe-1",
                1_000,
                Some(200_000),
            ),
        )
        .await
        .unwrap();

    // Occupy the active command slot with a Play/Next so queued seeks coalesce.
    engine
        .submit_player_command(make_cmd("cmd-hold", PlayerOp::Next, None))
        .await
        .unwrap();

    for (id, pos) in [("s1", 10_000), ("s2", 20_000), ("s3", 30_000)] {
        let mut args = make_args();
        args.track_instance_id = Some(TrackInstanceId::new("ti-1").unwrap());
        args.position_ms = Some(pos);
        engine
            .submit_player_command(make_cmd(id, PlayerOp::Seek, Some(args)))
            .await
            .unwrap();
    }

    for (id, vol) in [("v1", 0.2), ("v2", 0.4), ("v3", 0.9)] {
        let mut args = make_args();
        args.volume = Some(vol);
        engine
            .submit_player_command(make_cmd(id, PlayerOp::SetVolume, Some(args)))
            .await
            .unwrap();
    }

    // s1, s2 and v1, v2 should be Cancelled; only s3 and v3 remain queued.
    assert_eq!(
        engine
            .command_receipt(CommandId::new("s1").unwrap())
            .await
            .unwrap()
            .unwrap()
            .status,
        CommandLifecycleStatus::Cancelled
    );
    assert_eq!(
        engine
            .command_receipt(CommandId::new("s2").unwrap())
            .await
            .unwrap()
            .unwrap()
            .status,
        CommandLifecycleStatus::Cancelled
    );
    assert_eq!(
        engine
            .command_receipt(CommandId::new("v1").unwrap())
            .await
            .unwrap()
            .unwrap()
            .status,
        CommandLifecycleStatus::Cancelled
    );
    assert_eq!(
        engine
            .command_receipt(CommandId::new("v2").unwrap())
            .await
            .unwrap()
            .unwrap()
            .status,
        CommandLifecycleStatus::Cancelled
    );

    // Now complete cmd-hold by transitioning to ti-2!
    // Because track changed to ti-2, queued s3 (bound to ti-1) MUST be rejected as StaleContext and NOT dispatched!
    engine
        .on_bridge_playback_observed(
            3,
            make_obs(
                "bridge-1",
                PlaybackStatus::Playing,
                "ti-2",
                "vid-2",
                "qe-2",
                0,
                Some(200_000),
            ),
        )
        .await
        .unwrap();

    let s3_receipt = engine
        .command_receipt(CommandId::new("s3").unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(s3_receipt.status, CommandLifecycleStatus::Failed);
    assert_eq!(
        s3_receipt.error.as_ref().map(|e| e.code),
        Some(QueryErrorCode::StaleContext)
    );
    // Only Next and SetVolume(0.9) were dispatched, never stale Seek!
    assert_eq!(bridge.sent_ops(), vec![PlayerOp::Next, PlayerOp::SetVolume]);

    // Submitting a seek bound to old ti-1 must fail immediately.
    let mut stale_args = make_args();
    stale_args.track_instance_id = Some(TrackInstanceId::new("ti-1").unwrap());
    stale_args.position_ms = Some(50_000);
    let err = engine
        .submit_player_command(make_cmd("s-stale", PlayerOp::Seek, Some(stale_args)))
        .await
        .unwrap_err();
    assert_eq!(err.code, QueryErrorCode::StaleContext);
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn stop_transaction_pause_then_seek_zero_and_abort_on_track_change() {
    let (engine, _clock, bridge, _gate) = setup_engine().await;
    init_ready_bridge(&engine, "bridge-1", 1).await;
    engine
        .on_bridge_playback_observed(
            2,
            make_obs(
                "bridge-1",
                PlaybackStatus::Playing,
                "ti-1",
                "vid-1",
                "qe-1",
                45_000,
                Some(200_000),
            ),
        )
        .await
        .unwrap();

    let mut args = make_args();
    args.track_instance_id = Some(TrackInstanceId::new("ti-1").unwrap());
    engine
        .submit_player_command(make_cmd("cmd-stop-1", PlayerOp::Stop, Some(args)))
        .await
        .unwrap();

    // Step 1: only Pause dispatched so far.
    assert_eq!(bridge.sent_ops(), vec![PlayerOp::Pause]);

    // Observe Paused still at 45_000 -> Step 2 dispatches Seek(0), stopped_confirmed still false.
    engine
        .on_bridge_playback_observed(
            3,
            make_obs(
                "bridge-1",
                PlaybackStatus::Paused,
                "ti-1",
                "vid-1",
                "qe-1",
                45_000,
                Some(200_000),
            ),
        )
        .await
        .unwrap();
    tokio::task::yield_now().await;
    assert_eq!(bridge.sent_ops(), vec![PlayerOp::Pause, PlayerOp::Seek]);
    assert!(!engine.snapshot().await.unwrap().player.stopped_confirmed);

    // Observe Paused at 0 -> Stop Observed & stopped_confirmed == true.
    engine
        .on_bridge_playback_observed(
            4,
            make_obs(
                "bridge-1",
                PlaybackStatus::Paused,
                "ti-1",
                "vid-1",
                "qe-1",
                0,
                Some(200_000),
            ),
        )
        .await
        .unwrap();

    let snap = engine.snapshot().await.unwrap();
    assert!(snap.player.stopped_confirmed);
    assert_eq!(snap.player.playback, PlaybackStatus::Paused);
    assert_eq!(
        engine
            .command_receipt(CommandId::new("cmd-stop-1").unwrap())
            .await
            .unwrap()
            .unwrap()
            .status,
        CommandLifecycleStatus::Observed
    );

    // Playing clears stopped_confirmed.
    engine
        .on_bridge_playback_observed(
            5,
            make_obs(
                "bridge-1",
                PlaybackStatus::Playing,
                "ti-1",
                "vid-1",
                "qe-1",
                100,
                Some(200_000),
            ),
        )
        .await
        .unwrap();
    assert!(!engine.snapshot().await.unwrap().player.stopped_confirmed);

    // Track change during Stop must abort without setting stopped_confirmed or seeking new track.
    let mut args2 = make_args();
    args2.track_instance_id = Some(TrackInstanceId::new("ti-1").unwrap());
    engine
        .submit_player_command(make_cmd("cmd-stop-2", PlayerOp::Stop, Some(args2)))
        .await
        .unwrap();
    let count_before = bridge.sent_ops().len();
    engine
        .on_bridge_playback_observed(
            6,
            make_obs(
                "bridge-1",
                PlaybackStatus::Paused,
                "ti-2",
                "vid-2",
                "qe-2",
                0,
                Some(200_000),
            ),
        )
        .await
        .unwrap();
    assert_eq!(bridge.sent_ops().len(), count_before);
    assert!(!engine.snapshot().await.unwrap().player.stopped_confirmed);
    assert_eq!(
        engine
            .command_receipt(CommandId::new("cmd-stop-2").unwrap())
            .await
            .unwrap()
            .unwrap()
            .status,
        CommandLifecycleStatus::Failed
    );
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn paused_next_uses_output_gate_never_reports_transient_playing_as_success() {
    let (engine, _clock, bridge, gate) = setup_engine().await;
    init_ready_bridge(&engine, "bridge-1", 1).await;
    engine
        .on_bridge_playback_observed(
            2,
            make_obs(
                "bridge-1",
                PlaybackStatus::Paused,
                "ti-1",
                "vid-1",
                "qe-1",
                12_000,
                Some(200_000),
            ),
        )
        .await
        .unwrap();

    engine
        .submit_player_command(make_cmd("cmd-pnext-1", PlayerOp::Next, None))
        .await
        .unwrap();

    let calls = gate.calls();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].1, "must gate output before Next dispatch");
    let tx_id = calls[0].0.clone();
    assert_eq!(bridge.sent_ops(), vec![PlayerOp::Next]);

    // Page switches to ti-2 and transiently reports Playing!
    engine
        .on_bridge_playback_observed(
            3,
            make_obs(
                "bridge-1",
                PlaybackStatus::Playing,
                "ti-2",
                "vid-2",
                "qe-2",
                0,
                Some(200_000),
            ),
        )
        .await
        .unwrap();
    tokio::task::yield_now().await;

    // MUST NOT report Observed yet and MUST NOT ungate yet; must dispatch Pause for ti-2.
    assert_eq!(
        engine
            .command_receipt(CommandId::new("cmd-pnext-1").unwrap())
            .await
            .unwrap()
            .unwrap()
            .status,
        CommandLifecycleStatus::Dispatched
    );
    assert_eq!(gate.calls().len(), 1);
    assert_eq!(bridge.sent_ops(), vec![PlayerOp::Next, PlayerOp::Pause]);

    // Page confirms Paused on ti-2 -> core ungates with SAME tx_id and settles Observed!
    engine
        .on_bridge_playback_observed(
            4,
            make_obs(
                "bridge-1",
                PlaybackStatus::Paused,
                "ti-2",
                "vid-2",
                "qe-2",
                0,
                Some(200_000),
            ),
        )
        .await
        .unwrap();
    tokio::task::yield_now().await;

    assert_eq!(gate.calls(), vec![(tx_id.clone(), true), (tx_id, false)]);
    assert_eq!(
        engine
            .command_receipt(CommandId::new("cmd-pnext-1").unwrap())
            .await
            .unwrap()
            .unwrap()
            .status,
        CommandLifecycleStatus::Observed
    );
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn paused_next_failure_does_not_auto_clear_unknown_gate() {
    let (engine, clock, bridge, gate) = setup_engine().await;
    init_ready_bridge(&engine, "bridge-1", 1).await;
    engine
        .on_bridge_playback_observed(
            2,
            make_obs(
                "bridge-1",
                PlaybackStatus::Paused,
                "ti-1",
                "vid-1",
                "qe-1",
                5_000,
                Some(200_000),
            ),
        )
        .await
        .unwrap();

    // Case 1: Gate error/unknown on engage -> Next is never dispatched, ungate is never called.
    *gate.error_on_engage.lock().unwrap() = true;
    let err = engine
        .submit_player_command(make_cmd("cmd-pnext-err", PlayerOp::Next, None))
        .await
        .unwrap_err();
    assert_eq!(err.code, QueryErrorCode::Busy);
    assert!(bridge.sent_ops().is_empty());
    assert_eq!(gate.calls().len(), 1);
    assert!(gate.calls()[0].1);

    // Case 2: Gate succeeds, Next dispatches, times out -> settles Indeterminate and NEVER auto-clears gate!
    *gate.error_on_engage.lock().unwrap() = false;
    engine
        .submit_player_command(make_cmd("cmd-pnext-timeout", PlayerOp::Next, None))
        .await
        .unwrap();
    clock.advance(DEFAULT_CONTROL_TIMEOUT_MS + 1_000);
    engine.tick().await.unwrap();
    assert_eq!(
        engine
            .command_receipt(CommandId::new("cmd-pnext-timeout").unwrap())
            .await
            .unwrap()
            .unwrap()
            .status,
        CommandLifecycleStatus::Indeterminate
    );
    // Still only `true` gate calls, zero automatic `false` ungate calls on failure/timeout!
    assert!(gate.calls().iter().all(|(_, gated)| *gated));

    // An explicit user Play releases the gate left engaged by the failed silent transition
    // before dispatching, otherwise the host would stay muted for good.
    let unknown_tx = gate.calls()[0].0.clone();
    let stuck_tx = gate.calls()[1].0.clone();
    engine
        .submit_player_command(make_cmd("cmd-play-after-stuck", PlayerOp::Play, None))
        .await
        .unwrap();
    assert!(gate.calls().contains(&(unknown_tx, false)));
    assert!(gate.calls().contains(&(stuck_tx, false)));
    assert_eq!(bridge.sent_ops().last(), Some(&PlayerOp::Play));
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn late_applied_gate_after_timeout_is_released_and_only_retried_if_unconfirmed() {
    for reject_release in [false, true] {
        let (engine, clock, bridge, gate) = setup_engine().await;
        init_ready_bridge(&engine, "bridge-1", 1).await;
        engine
            .on_bridge_playback_observed(
                2,
                make_obs(
                    "bridge-1",
                    PlaybackStatus::Paused,
                    "ti-1",
                    "vid-1",
                    "qe-1",
                    0,
                    Some(200_000),
                ),
            )
            .await
            .unwrap();
        let hold = Arc::new(Notify::new());
        *gate.pending_engage.lock().unwrap() = Some(hold.clone());
        let release = Arc::new(Notify::new());
        *gate.pending_release.lock().unwrap() = Some(release.clone());
        *gate.reject_release.lock().unwrap() = reject_release;
        let caller = engine.clone();
        let next = tokio::spawn(async move {
            caller
                .submit_player_command(make_cmd("late-next", PlayerOp::Next, None))
                .await
        });
        gate.started.notified().await;
        clock.advance(DEFAULT_CONTROL_TIMEOUT_MS + 1);
        engine.tick().await.unwrap();
        hold.notify_one();
        assert_eq!(
            next.await.unwrap().unwrap_err().code,
            QueryErrorCode::StaleContext
        );
        gate.started.notified().await;
        assert!(bridge.sent_ops().is_empty());
        let tx = gate.calls()[0].0.clone();
        let mut expected = vec![(tx.clone(), true), (tx.clone(), false)];
        assert_eq!(gate.calls(), expected);
        *gate.reject_release.lock().unwrap() = false;
        let caller = engine.clone();
        let play = tokio::spawn(async move {
            caller
                .submit_player_command(make_cmd("play-after-late", PlayerOp::Play, None))
                .await
        });
        settle(&engine).await;
        assert_eq!(
            gate.calls(),
            expected,
            "explicit Play must join the release in flight"
        );
        assert!(bridge.sent_ops().is_empty());
        release.notify_one();
        play.await.unwrap().unwrap();
        assert_eq!(bridge.sent_ops(), [PlayerOp::Play]);
        if reject_release {
            expected.push((tx, false));
        }
        assert_eq!(gate.calls(), expected);
        engine.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn explicit_play_recovers_unacknowledged_host_mute_after_gate_ipc_timeout() {
    for command_expired in [false, true] {
        let (engine, clock, bridge, gate) = setup_engine().await;
        init_ready_bridge(&engine, "bridge-1", 1).await;
        engine
            .on_bridge_playback_observed(
                2,
                make_obs(
                    "bridge-1",
                    PlaybackStatus::Paused,
                    "ti-1",
                    "vid-1",
                    "qe-1",
                    0,
                    Some(180_000),
                ),
            )
            .await
            .unwrap();
        // Simulate a host which muted the output but did not send back its acknowledgement.
        *gate.pending_engage.lock().unwrap() = Some(Arc::new(Notify::new()));
        let caller = engine.clone();
        let next = tokio::spawn(async move {
            caller
                .submit_player_command(make_cmd("missing-gate-ack", PlayerOp::Next, None))
                .await
        });
        gate.started.notified().await;
        if command_expired {
            clock.advance(DEFAULT_CONTROL_TIMEOUT_MS + 1);
            engine.tick().await.unwrap();
        }
        assert_eq!(
            next.await.unwrap().unwrap_err().code,
            if command_expired {
                QueryErrorCode::StaleContext
            } else {
                QueryErrorCode::Timeout
            }
        );
        assert!(bridge.sent_ops().is_empty());
        assert_eq!(gate.calls().len(), 1);
        let tx = gate.calls()[0].0.clone();
        engine
            .submit_player_command(make_cmd("play-after-ipc-timeout", PlayerOp::Play, None))
            .await
            .unwrap();
        assert_eq!(gate.calls(), [(tx.clone(), true), (tx, false)]);
        assert_eq!(bridge.sent_ops(), [PlayerOp::Play]);
        engine.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn collection_playback_requires_exact_playlist_and_occurrence_even_for_the_same_video() {
    let (engine, _clock, bridge, _gate) = setup_engine().await;
    init_ready_bridge(&engine, "bridge-1", 1).await;
    let playlist = ytm_protocol::PlaylistId::new("PL-exact-fixture").unwrap();
    let other = ytm_protocol::PlaylistId::new("PL-other-fixture").unwrap();
    let observation = |playlist_id, start_index, entry_id: Option<&str>| {
        let mut sample = make_obs(
            "bridge-1",
            PlaybackStatus::Playing,
            "ti-1",
            "vid-shared",
            "qe-1",
            0,
            Some(200_000),
        );
        sample.playback_context = Some(ytm_protocol::PlaybackContext {
            playlist_id,
            start_index,
            playlist_entry_id: entry_id.map(str::to_owned),
        });
        sample
    };
    engine
        .on_bridge_playback_observed(2, observation(other.clone(), 1, Some("set-first")))
        .await
        .unwrap();
    let target = PlaybackRef {
        video_id: VideoId::new("vid-shared").unwrap(),
        playlist_id: Some(playlist.clone()),
        playlist_entry_id: Some("set-first".into()),
        browse_id: None,
        queue_entry_id: None,
        start_index: Some(1),
    };
    let mut args = make_args();
    args.target = Some(target.clone());
    let receipt = engine
        .submit_player_command(make_cmd("collection", PlayerOp::PlayCollection, Some(args)))
        .await
        .unwrap();
    assert_eq!(receipt.status, CommandLifecycleStatus::Dispatched);
    assert_eq!(
        bridge.sent_commands()[0]
            .1
            .args
            .as_ref()
            .unwrap()
            .target
            .as_ref(),
        Some(&target)
    );

    for (sequence, sample) in [
        observation(other, 1, Some("set-first")),
        observation(playlist.clone(), 0, Some("set-first")),
        observation(playlist.clone(), 1, None),
        observation(playlist.clone(), 1, Some("set-second")),
    ]
    .into_iter()
    .enumerate()
    {
        engine
            .on_bridge_playback_observed(3 + sequence as u64, sample)
            .await
            .unwrap();
        assert_eq!(
            engine
                .command_receipt(receipt.command_id.clone())
                .await
                .unwrap()
                .unwrap()
                .status,
            CommandLifecycleStatus::Dispatched
        );
    }
    // The page's queue selection can lag behind the navigated album (K12): the bridge then
    // reports the context from the current endpoint and no current queue entry.
    let mut confirmed = observation(playlist, 1, Some("set-first"));
    confirmed.current_queue_entry_id = None;
    engine
        .on_bridge_playback_observed(7, confirmed)
        .await
        .unwrap();
    assert_eq!(
        engine
            .command_receipt(receipt.command_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        CommandLifecycleStatus::Observed
    );
    assert_eq!(bridge.sent_ops(), [PlayerOp::PlayCollection]);
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn play_target_authorized_successor_handoff_vs_unexpected_rebuild() {
    let (engine, _clock, bridge, _gate) = setup_engine().await;
    init_ready_bridge(&engine, "bridge-1", 1).await;
    engine
        .on_bridge_playback_observed(
            2,
            make_obs(
                "bridge-1",
                PlaybackStatus::Paused,
                "ti-1",
                "vid-1",
                "qe-1",
                0,
                Some(200_000),
            ),
        )
        .await
        .unwrap();

    let mut args = make_args();
    args.target = Some(PlaybackRef {
        video_id: VideoId::new("vid-target").unwrap(),
        playlist_id: None,
        playlist_entry_id: None,
        browse_id: None,
        queue_entry_id: None,
        start_index: None,
    });
    engine
        .submit_player_command(make_cmd("cmd-pt-1", PlayerOp::PlayTarget, Some(args)))
        .await
        .unwrap();

    // Host authorizes successor bridge-2 for PlayTarget navigation.
    engine
        .on_host_bridge_lifecycle(BridgeLifecyclePayload {
            event: "successorAuthorized".to_string(),
            bridge_epoch: Some(BridgeEpoch::new("bridge-2").unwrap()),
            is_music_origin: Some(true),
        })
        .await
        .unwrap();
    init_ready_bridge(&engine, "bridge-2", 1).await;

    // Late message from old bridge-1 MUST be dropped.
    let stale_accepted = engine
        .on_bridge_playback_observed(
            3,
            make_obs(
                "bridge-1",
                PlaybackStatus::Playing,
                "ti-old",
                "vid-target",
                "qe-old",
                0,
                Some(200_000),
            ),
        )
        .await
        .unwrap();
    assert!(!stale_accepted);

    // Authorized bridge-2 observation settles PlayTarget as Observed.
    engine
        .on_bridge_playback_observed(
            2,
            make_obs(
                "bridge-2",
                PlaybackStatus::Playing,
                "ti-new",
                "vid-target",
                "qe-new",
                0,
                Some(200_000),
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        engine
            .command_receipt(CommandId::new("cmd-pt-1").unwrap())
            .await
            .unwrap()
            .unwrap()
            .status,
        CommandLifecycleStatus::Observed
    );

    // Unexpected bridge rebuild (bridge-3 without authorization) ends the already dispatched Next
    // as indeterminate (it may have run) and does NOT re-run it.
    let dispatches_before = bridge.sent_ops().len();
    engine
        .submit_player_command(make_cmd("cmd-next-cancel", PlayerOp::Next, None))
        .await
        .unwrap();
    assert_eq!(bridge.sent_ops().len(), dispatches_before + 1);

    init_ready_bridge(&engine, "bridge-3", 1).await;
    assert_eq!(
        engine
            .command_receipt(CommandId::new("cmd-next-cancel").unwrap())
            .await
            .unwrap()
            .unwrap()
            .status,
        CommandLifecycleStatus::Indeterminate
    );
    assert_eq!(bridge.sent_ops().len(), dispatches_before + 1);
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn subscribe_snapshot_atomic_zero_lost_or_duplicate_revisions() {
    let (engine, _clock, _bridge, _gate) = setup_engine().await;
    init_ready_bridge(&engine, "bridge-1", 1).await;

    let writer_engine = engine.clone();
    let writer = tokio::spawn(async move {
        for seq in 2..=120u64 {
            writer_engine
                .on_bridge_playback_observed(
                    seq,
                    make_obs(
                        "bridge-1",
                        PlaybackStatus::Playing,
                        "ti-1",
                        "vid-1",
                        "qe-1",
                        seq * 100,
                        Some(200_000),
                    ),
                )
                .await
                .unwrap();
            if seq % 10 == 0 {
                tokio::task::yield_now().await;
            }
        }
    });

    // Subscribe mid-stream while writer is mutating.
    tokio::task::yield_now().await;
    let (snap, mut rx) = engine.subscribe_snapshot().await.unwrap();
    writer.await.unwrap();

    let mut expected_rev = snap.revision + 1;
    loop {
        match rx.try_recv() {
            Ok(delta) => {
                assert_eq!(
                    delta.revision(),
                    expected_rev,
                    "delta revisions must be strictly contiguous after snapshot.revision"
                );
                expected_rev += 1;
            }
            Err(broadcast::error::TryRecvError::Empty) => break,
            Err(err) => panic!("unexpected broadcast recv error: {err:?}"),
        }
    }
    let final_snap = engine.snapshot().await.unwrap();
    assert_eq!(expected_rev - 1, final_snap.revision);
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn account_epoch_guard_logout_and_credential_rotation() {
    let clock = Arc::new(MockClock::new(1_000));
    let bridge = Arc::new(RecordingBridge::default());
    let output_gate = Arc::new(RecordingOutputGate::default());
    let started = Arc::new(Notify::new());
    let proceed = Arc::new(Notify::new());
    let catalog = Arc::new(SlowCatalogPort {
        account_hint: Mutex::new(None),
        official: Mutex::new(None),
        started: started.clone(),
        proceed: proceed.clone(),
        ignore_cancel: true,
        seen_tokens: Mutex::new(Vec::new()),
    });
    let engine = CoreEngine::start(
        CoreDeps {
            clock: clock.clone(),
            bridge,
            output_gate,
            catalog: Some(catalog.clone()),
            storage: None,
            lyrics: None,
        },
        EngineConfig::default(),
    );

    let (_s0, mut rx) = engine.subscribe_snapshot().await.unwrap();

    // Login acct-1.
    engine
        .on_session_material(SessionMaterialPayload {
            account_epoch: AccountEpoch::new("acct-1").unwrap(),
            credential_revision: CredentialRevision::new("cred-1").unwrap(),
            cookie_pairs: BTreeMap::from([("SAPISID".to_string(), "secret-val".to_string())]),
            auth_user_index: Some(0),
            channel_label: Some("UserOne".to_string()),
        })
        .await
        .unwrap();

    // Rotation within SAME accountEpoch must NOT emit ResourceInvalidated.
    while rx.try_recv().is_ok() {}
    engine
        .on_session_material(SessionMaterialPayload {
            account_epoch: AccountEpoch::new("acct-1").unwrap(),
            credential_revision: CredentialRevision::new("cred-2").unwrap(),
            cookie_pairs: BTreeMap::from([("SAPISID".to_string(), "secret-val-2".to_string())]),
            auth_user_index: Some(0),
            channel_label: Some("UserOne".to_string()),
        })
        .await
        .unwrap();
    let mut saw_invalidated = false;
    while let Ok(d) = rx.try_recv() {
        if matches!(d, StateDelta::ResourceInvalidated { .. }) {
            saw_invalidated = true;
        }
    }
    assert!(!saw_invalidated);

    // Start in-flight catalog query for acct-1, then switch to acct-2 BEFORE slow catalog returns.
    let engine_q = engine.clone();
    let query_task = tokio::spawn(async move {
        engine_q
            .catalog_query(
                CatalogQueryRequest {
                    kind: CatalogQueryKind::RecentHistory,
                    query: None,
                    playlist_id: None,
                    album_id: None,
                    artist_id: None,
                    cursor: None,
                    limit: None,
                },
                Some(AccountEpoch::new("acct-1").unwrap()),
            )
            .await
    });

    started.notified().await;
    engine
        .on_session_material(SessionMaterialPayload {
            account_epoch: AccountEpoch::new("acct-2").unwrap(),
            credential_revision: CredentialRevision::new("cred-3").unwrap(),
            cookie_pairs: BTreeMap::from([("SAPISID".to_string(), "secret-val-3".to_string())]),
            auth_user_index: Some(1),
            channel_label: Some("UserTwo".to_string()),
        })
        .await
        .unwrap();
    // Unblock slow catalog port (which ignored cancel) -> CoreEngine cancels and drops stale acct-1 response!
    proceed.notify_one();
    let err = query_task.await.unwrap().unwrap_err();
    assert_eq!(err.code, QueryErrorCode::AccountMismatch);
    assert!(catalog.seen_tokens.lock().unwrap()[0].is_cancelled());

    // Passing stale expected_account_epoch is also rejected immediately.
    let stale_err = engine
        .catalog_query(
            CatalogQueryRequest {
                kind: CatalogQueryKind::RecentHistory,
                query: None,
                playlist_id: None,
                album_id: None,
                artist_id: None,
                cursor: None,
                limit: None,
            },
            Some(AccountEpoch::new("acct-1").unwrap()),
        )
        .await
        .unwrap_err();
    assert_eq!(stale_err.code, QueryErrorCode::AccountMismatch);

    // SystemClock sanity check (monotonic + wall clock).
    let sys_clock = SystemClock::new();
    let t0 = sys_clock.now_ms();
    let t1 = sys_clock.now_ms();
    assert!(t1 >= t0);
    assert!(sys_clock.wall_now_ms() >= t1);

    // Logout clears auth_state and invalidates account-scoped UI state.
    engine.on_logout().await.unwrap();
    let snap = engine.snapshot().await.unwrap();
    assert_eq!(snap.auth_state.state, AuthState::SignedOut);
    assert!(snap.account_epoch.is_none());
    assert!(!snap.auth_state.player_catalog_matched);
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn playback_clock_extrapolation_freezing_and_power_resume() {
    let mut clock_model = PlaybackClock::new(1_000);
    let obs = make_obs(
        "bridge-1",
        PlaybackStatus::Playing,
        "ti-1",
        "vid-1",
        "qe-1",
        10_000,
        Some(15_000),
    );
    clock_model.update_from_observation(&obs, PlayerConnectionState::Ready, 1_000);
    assert_eq!(clock_model.estimated_position_ms(3_000), 12_000);
    // Clamps to duration_ms.
    assert_eq!(clock_model.estimated_position_ms(10_000), 15_000);

    // Freezes when Buffering / Stale / Paused.
    clock_model.on_connection_changed(PlayerConnectionState::Stale, PlaybackStatus::Playing, 3_000);
    assert_eq!(clock_model.estimated_position_ms(9_000), 12_000);

    let (engine, mock_clock, bridge, _gate) = setup_engine().await;
    init_ready_bridge(&engine, "bridge-1", 1).await;
    engine
        .on_bridge_playback_observed(
            2,
            make_obs(
                "bridge-1",
                PlaybackStatus::Playing,
                "ti-1",
                "vid-1",
                "qe-1",
                20_000,
                Some(300_000),
            ),
        )
        .await
        .unwrap();

    // Offline network while Playing && Ready must NOT force Buffering or freeze progress.
    engine
        .on_host_network_changed(NetworkState::Offline)
        .await
        .unwrap();
    mock_clock.advance(5_000);
    let snap_offline = engine.snapshot().await.unwrap();
    assert_eq!(snap_offline.network_state, NetworkState::Offline);
    assert_eq!(snap_offline.player.playback, PlaybackStatus::Playing);
    assert_eq!(snap_offline.player.time_anchor.position_ms, 25_000);

    // Simulate 8-hour system suspend followed by power resume -> MUST NOT extrapolate 8 hours!
    let before_sleep = snap_offline.player.time_anchor.position_ms;
    mock_clock.advance(8 * 60 * 60 * 1_000);
    engine.on_host_power_resume().await.unwrap();
    let resumed = engine.snapshot().await.unwrap();
    assert_eq!(resumed.player.connection, PlayerConnectionState::Stale);
    assert!(!resumed.player.time_anchor.extrapolating);
    assert!(
        resumed.player.time_anchor.position_ms <= before_sleep,
        "suspend must not add sleep time to playback position"
    );
    assert!(!bridge.snapshot_requests().is_empty());
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn playback_after_a_failed_page_probe_stays_stale_until_health_recovers() {
    let (engine, _clock, _bridge, _gate) = setup_engine().await;
    init_ready_bridge(&engine, "bridge-1", 1).await;
    let health = |healthy: bool| ytm_protocol::BridgeHealthPayload {
        bridge_epoch: BridgeEpoch::new("bridge-1").unwrap(),
        healthy,
        video_element_present: true,
        controls_bound: healthy,
        reason: (!healthy).then(|| "Page shim did not answer".to_string()),
    };
    let obs = |position_ms| {
        make_obs(
            "bridge-1",
            PlaybackStatus::Playing,
            "ti-1",
            "vid-1",
            "qe-1",
            position_ms,
            Some(300_000),
        )
    };
    let connection = || async { engine.snapshot().await.unwrap().player.connection };

    // One bridge refresh: the failed probe's health, then its media-element playback.
    assert!(engine.on_bridge_health(2, health(false)).await.unwrap());
    engine
        .on_bridge_playback_observed(3, obs(1_000))
        .await
        .unwrap();
    assert_eq!(connection().await, PlayerConnectionState::Stale);
    // A same-epoch ready (a debounced capability re-send) does not hide it either.
    init_ready_bridge(&engine, "bridge-1", 4).await;
    assert_eq!(connection().await, PlayerConnectionState::Stale);

    assert!(engine.on_bridge_health(5, health(true)).await.unwrap());
    engine
        .on_bridge_playback_observed(6, obs(2_000))
        .await
        .unwrap();
    assert_eq!(connection().await, PlayerConnectionState::Ready);
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn duplicate_video_id_queue_entries_never_deduplicated_and_stale_revision_rejected() {
    let (engine, _clock, _bridge, _gate) = setup_engine().await;
    init_ready_bridge(&engine, "bridge-1", 1).await;

    let track = make_track("vid-same", "Same Track", Some(180_000));
    let q = QueueSnapshot {
        bridge_epoch: Some(BridgeEpoch::new("bridge-1").unwrap()),
        queue_revision: 7,
        current_entry_id: Some(QueueEntryId::new("qe-a").unwrap()),
        entries: vec![
            QueueEntry {
                entry_id: QueueEntryId::new("qe-a").unwrap(),
                video_id: VideoId::new("vid-same").unwrap(),
                track: track.clone(),
                availability: TrackAvailability::Available,
            },
            QueueEntry {
                entry_id: QueueEntryId::new("qe-b").unwrap(),
                video_id: VideoId::new("vid-same").unwrap(),
                track,
                availability: TrackAvailability::Available,
            },
        ],
        loaded_range: None,
        completeness: QueueCompleteness::Complete,
        has_more: Some(false),
        explicit_complete: None,
        may_load_more: None,
        official_upcoming: None,
        filters: None,
    };
    engine.on_bridge_queue_observed(2, q).await.unwrap();

    let snap = engine.snapshot().await.unwrap();
    assert_eq!(snap.queue_summary.loaded_count, 2);
    assert_eq!(snap.queue_summary.preview_entries.len(), 2);

    // Stale queueRevision rejected.
    let mut bad_args = make_args();
    bad_args.queue_entry_id = Some(QueueEntryId::new("qe-b").unwrap());
    bad_args.queue_revision = Some(6);
    let err = engine
        .submit_player_command(make_cmd(
            "cmd-sel-stale",
            PlayerOp::SelectQueueEntry,
            Some(bad_args),
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code, QueryErrorCode::StaleContext);

    // Valid queueRevision succeeds upon observation of qe-b.
    let mut ok_args = make_args();
    ok_args.queue_entry_id = Some(QueueEntryId::new("qe-b").unwrap());
    ok_args.queue_revision = Some(7);
    engine
        .submit_player_command(make_cmd(
            "cmd-sel-ok",
            PlayerOp::SelectQueueEntry,
            Some(ok_args),
        ))
        .await
        .unwrap();
    engine
        .on_bridge_playback_observed(
            3,
            make_obs(
                "bridge-1",
                PlaybackStatus::Playing,
                "ti-b",
                "vid-same",
                "qe-b",
                0,
                Some(180_000),
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        engine
            .command_receipt(CommandId::new("cmd-sel-ok").unwrap())
            .await
            .unwrap()
            .unwrap()
            .status,
        CommandLifecycleStatus::Observed
    );
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn select_queue_entry_is_observed_only_while_that_entry_video_plays() {
    let (engine, _clock, _bridge, _gate) = setup_engine().await;
    init_ready_bridge(&engine, "bridge-1", 1).await;
    let entry = |id: &str, vid: &str| QueueEntry {
        entry_id: QueueEntryId::new(id).unwrap(),
        video_id: VideoId::new(vid).unwrap(),
        track: make_track(vid, "Track", Some(180_000)),
        availability: TrackAvailability::Available,
    };
    let q = QueueSnapshot {
        bridge_epoch: Some(BridgeEpoch::new("bridge-1").unwrap()),
        queue_revision: 3,
        current_entry_id: Some(QueueEntryId::new("qe-a").unwrap()),
        entries: vec![entry("qe-a", "vid-a"), entry("qe-b", "vid-b")],
        loaded_range: None,
        completeness: QueueCompleteness::Complete,
        has_more: Some(false),
        explicit_complete: None,
        may_load_more: None,
        official_upcoming: None,
        filters: None,
    };
    engine.on_bridge_queue_observed(2, q).await.unwrap();
    let mut args = make_args();
    args.queue_entry_id = Some(QueueEntryId::new("qe-b").unwrap());
    args.queue_revision = Some(3);
    engine
        .submit_player_command(make_cmd("cmd-sel", PlayerOp::SelectQueueEntry, Some(args)))
        .await
        .unwrap();
    let status = || async {
        engine
            .command_receipt(CommandId::new("cmd-sel").unwrap())
            .await
            .unwrap()
            .unwrap()
            .status
    };

    // The page selects the requested row while the player still plays another video.
    let lagging = make_obs(
        "bridge-1",
        PlaybackStatus::Playing,
        "ti-a",
        "vid-a",
        "qe-b",
        0,
        Some(180_000),
    );
    engine
        .on_bridge_playback_observed(3, lagging)
        .await
        .unwrap();
    assert_ne!(status().await, CommandLifecycleStatus::Observed);

    let playing = make_obs(
        "bridge-1",
        PlaybackStatus::Playing,
        "ti-b",
        "vid-b",
        "qe-b",
        0,
        Some(180_000),
    );
    engine
        .on_bridge_playback_observed(4, playing)
        .await
        .unwrap();
    assert_eq!(status().await, CommandLifecycleStatus::Observed);
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn previous_completes_on_same_instance_restart_near_zero() {
    let (engine, clock, _bridge, _gate) = setup_engine().await;
    init_ready_bridge(&engine, "bridge-1", 1).await;
    let playing = |pos: u64| {
        make_obs(
            "bridge-1",
            PlaybackStatus::Playing,
            "ti-1",
            "vid-1",
            "qe-1",
            pos,
            Some(200_000),
        )
    };
    engine
        .on_bridge_playback_observed(2, playing(42_000))
        .await
        .unwrap();
    engine
        .submit_player_command(make_cmd("cmd-prev-restart", PlayerOp::Previous, None))
        .await
        .unwrap();
    clock.advance(300);
    // YTM restarts the same track instead of changing it: same instance, position back at ~0.
    engine
        .on_bridge_playback_observed(3, playing(400))
        .await
        .unwrap();
    let receipt = engine
        .command_receipt(CommandId::new("cmd-prev-restart").unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(receipt.status, CommandLifecycleStatus::Observed);
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn bridge_refusal_fails_the_command_with_its_error_code() {
    let (engine, _clock, bridge, _gate) = setup_engine().await;
    let mut caps = supported_caps();
    // A capability unsupported at bridge.ready may become usable later, so the core still
    // dispatches; the bridge re-probes the live page and refuses if it is still unavailable.
    caps.shuffle = CapabilitySupport::Unsupported;
    engine
        .on_bridge_ready(
            1,
            BridgeReadyPayload {
                bridge_epoch: BridgeEpoch::new("bridge-1").unwrap(),
                bridge_version: "0.1.0".to_string(),
                capabilities: caps,
                account_hint: None,
                logged_in_observed: true,
            },
        )
        .await
        .unwrap();
    let mut args = make_args();
    args.shuffle = Some(true);
    let receipt = engine
        .submit_player_command(make_cmd("cmd-shuffle", PlayerOp::SetShuffle, Some(args)))
        .await
        .unwrap();
    assert_eq!(receipt.status, CommandLifecycleStatus::Dispatched);
    assert_eq!(bridge.sent_ops(), vec![PlayerOp::SetShuffle]);
    engine
        .on_bridge_command_ack(
            2,
            BridgeCommandAckPayload {
                command_id: CommandId::new("cmd-shuffle").unwrap(),
                bridge_epoch: BridgeEpoch::new("bridge-1").unwrap(),
                dispatched: false,
                error: Some(QueryError {
                    code: QueryErrorCode::UnsupportedCapability,
                    retriable: false,
                    retry_after_ms: None,
                    safe_message: "Shuffle control unavailable".to_string(),
                    request_id: None,
                }),
            },
        )
        .await
        .unwrap();
    let receipt = engine
        .command_receipt(CommandId::new("cmd-shuffle").unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(receipt.status, CommandLifecycleStatus::Failed);
    assert_eq!(
        receipt.error.map(|e| e.code),
        Some(QueryErrorCode::UnsupportedCapability)
    );
    engine.shutdown().await.unwrap();
}

/// Catalog whose identity hint, mutation outcome and query answer are scripted per test.
struct ScriptedCatalog {
    account_hint: Option<String>,
    official: Mutex<Option<OfficialLyrics>>,
    mutate_outcome: Mutex<Result<MutationOutcome, QueryError>>,
    query_result: Mutex<Result<CatalogResponsePayload, QueryError>>,
    mutate_calls: Mutex<u32>,
}

impl ScriptedCatalog {
    fn new(account_hint: &str) -> Self {
        Self {
            account_hint: Some(account_hint.to_string()),
            official: Mutex::new(None),
            mutate_outcome: Mutex::new(Ok(MutationOutcome::Confirmed)),
            query_result: Mutex::new(Ok(CatalogResponsePayload::RecentHistory(
                RecentHistoryResult {
                    items: Vec::new(),
                    freshness: Freshness::Fresh,
                },
            ))),
            mutate_calls: Mutex::new(0),
        }
    }
}

impl CatalogPort for ScriptedCatalog {
    fn query(
        &self,
        _account_epoch: Option<AccountEpoch>,
        _account_scope: Option<AccountScope>,
        _request: CatalogQueryRequest,
        _cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<CatalogResponsePayload, QueryError>> {
        let res = self.query_result.lock().unwrap().clone();
        Box::pin(async move { res })
    }

    fn mutate(
        &self,
        _account_epoch: AccountEpoch,
        _account_scope: AccountScope,
        request: MutationRequest,
        _cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<MutationResult, QueryError>> {
        *self.mutate_calls.lock().unwrap() += 1;
        let outcome = self.mutate_outcome.lock().unwrap().clone();
        Box::pin(async move {
            outcome.map(|outcome| MutationResult {
                kind: request.kind,
                outcome,
                error: None,
            })
        })
    }

    fn official_lyrics(
        &self,
        _account_epoch: AccountEpoch,
        _video_id: VideoId,
        _cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<Option<OfficialLyrics>, QueryError>> {
        let found = self.official.lock().unwrap().clone();
        Box::pin(async move { Ok(found) })
    }

    fn apply_session_material(
        &self,
        _material: SessionMaterialPayload,
        _cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<CatalogIdentity, QueryError>> {
        let hint = self.account_hint.clone();
        Box::pin(async move {
            Ok(CatalogIdentity {
                account_scope: AccountScope::new("scope-stable").unwrap(),
                account_hint: hint,
            })
        })
    }

    fn clear_session_material(
        &self,
        _account_epoch: Option<AccountEpoch>,
    ) -> Result<(), QueryError> {
        Ok(())
    }
}

async fn engine_with_catalog(catalog: Arc<ScriptedCatalog>) -> CoreEngine {
    CoreEngine::start(
        CoreDeps {
            clock: Arc::new(MockClock::new(1_000)),
            bridge: Arc::new(RecordingBridge::default()),
            output_gate: Arc::new(RecordingOutputGate::default()),
            catalog: Some(catalog),
            storage: None,
            lyrics: None,
        },
        EngineConfig::default(),
    )
}

async fn announce_bridge(engine: &CoreEngine, seq: u64, account_hint: &str) {
    engine
        .on_bridge_ready(
            seq,
            BridgeReadyPayload {
                bridge_epoch: BridgeEpoch::new("bridge-1").unwrap(),
                bridge_version: "0.1.0".to_string(),
                capabilities: supported_caps(),
                account_hint: Some(account_hint.to_string()),
                logged_in_observed: true,
            },
        )
        .await
        .unwrap();
}

async fn sign_in(engine: &CoreEngine) {
    engine
        .on_session_material(SessionMaterialPayload {
            account_epoch: AccountEpoch::new("acct-1").unwrap(),
            credential_revision: CredentialRevision::new("cred-1").unwrap(),
            cookie_pairs: BTreeMap::from([("SAPISID".to_string(), "synthetic".to_string())]),
            auth_user_index: None,
            channel_label: None,
        })
        .await
        .unwrap();
}

fn like_request() -> MutationRequest {
    MutationRequest {
        kind: MutationKind::SetLike,
        video_id: VideoId::new("vid-like").unwrap(),
        liked: Some(true),
        playlist_id: None,
        confirmed_by_user: true,
    }
}

#[tokio::test]
async fn account_writes_require_epoch_and_matching_identity_hints() {
    let catalog = Arc::new(ScriptedCatalog::new("acct:aaaa"));
    let engine = engine_with_catalog(catalog.clone()).await;
    announce_bridge(&engine, 1, "acct:aaaa").await;
    sign_in(&engine).await;

    let snap = engine.snapshot().await.unwrap();
    assert!(snap.auth_state.player_catalog_matched);
    assert_eq!(
        snap.auth_state.account_scope.as_ref().unwrap().as_str(),
        "scope-stable"
    );
    assert_eq!(
        snap.capabilities.like_mutation,
        CapabilitySupport::Supported
    );
    assert_eq!(snap.capabilities.search, CapabilitySupport::Supported);
    // Bridge-owned bits survive the merge.
    assert_eq!(snap.capabilities.next, CapabilitySupport::Supported);

    let epoch = Some(AccountEpoch::new("acct-1").unwrap());
    let missing = engine.mutate(like_request(), None).await.unwrap_err();
    assert_eq!(missing.code, QueryErrorCode::InvalidPayload);
    let ok = engine.mutate(like_request(), epoch.clone()).await.unwrap();
    assert_eq!(ok.outcome, MutationOutcome::Confirmed);

    // The page now reports a different identity: writes are refused before anything is sent.
    announce_bridge(&engine, 2, "acct:bbbb").await;
    let snap = engine.snapshot().await.unwrap();
    assert!(!snap.auth_state.player_catalog_matched);
    assert_eq!(
        snap.capabilities.like_mutation,
        CapabilitySupport::Unsupported
    );
    let err = engine.mutate(like_request(), epoch).await.unwrap_err();
    assert_eq!(err.code, QueryErrorCode::AccountMismatch);
    assert_eq!(*catalog.mutate_calls.lock().unwrap(), 1);
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn missing_bridge_hint_is_unverified_not_matched() {
    let catalog = Arc::new(ScriptedCatalog::new("acct:aaaa"));
    let engine = engine_with_catalog(catalog).await;
    engine
        .on_bridge_ready(
            1,
            BridgeReadyPayload {
                bridge_epoch: BridgeEpoch::new("bridge-1").unwrap(),
                bridge_version: "0.1.0".to_string(),
                capabilities: supported_caps(),
                account_hint: None,
                logged_in_observed: true,
            },
        )
        .await
        .unwrap();
    sign_in(&engine).await;
    let snap = engine.snapshot().await.unwrap();
    assert!(!snap.auth_state.player_catalog_matched);
    assert_eq!(
        snap.capabilities.like_mutation,
        CapabilitySupport::Unverified
    );
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn indeterminate_like_is_settled_by_liked_music_readback() {
    let catalog = Arc::new(ScriptedCatalog::new("acct:aaaa"));
    *catalog.mutate_outcome.lock().unwrap() = Ok(MutationOutcome::Indeterminate);
    *catalog.query_result.lock().unwrap() = Ok(CatalogResponsePayload::LikedSongs(
        ytm_protocol::PagedResult {
            items: vec![make_track("vid-like", "Liked", None)],
            next_cursor: Some("more".to_string()),
            freshness: Freshness::Fresh,
            fetched_at_ms: 1,
            account_scope: AccountScope::new("scope-stable").unwrap(),
            provider_version: "test".to_string(),
        },
    ));
    let engine = engine_with_catalog(catalog.clone()).await;
    announce_bridge(&engine, 1, "acct:aaaa").await;
    sign_in(&engine).await;
    let epoch = Some(AccountEpoch::new("acct-1").unwrap());
    let res = engine.mutate(like_request(), epoch.clone()).await.unwrap();
    assert_eq!(res.outcome, MutationOutcome::Confirmed);

    // Absent even from the whole list may be read-after-write lag: it stays indeterminate
    // (never "rejected"), and the write is not retried.
    *catalog.query_result.lock().unwrap() = Ok(CatalogResponsePayload::LikedSongs(
        ytm_protocol::PagedResult {
            items: vec![make_track("vid-other", "Other", None)],
            next_cursor: None,
            freshness: Freshness::Fresh,
            fetched_at_ms: 1,
            account_scope: AccountScope::new("scope-stable").unwrap(),
            provider_version: "test".to_string(),
        },
    ));
    let res = engine.mutate(like_request(), epoch).await.unwrap();
    assert_eq!(res.outcome, MutationOutcome::Indeterminate);
    assert_eq!(*catalog.mutate_calls.lock().unwrap(), 2);
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn auth_expired_from_catalog_revokes_writes_and_recovers_on_new_material() {
    let catalog = Arc::new(ScriptedCatalog::new("acct:aaaa"));
    let engine = engine_with_catalog(catalog.clone()).await;
    announce_bridge(&engine, 1, "acct:aaaa").await;
    sign_in(&engine).await;

    *catalog.query_result.lock().unwrap() = Err(QueryError {
        code: QueryErrorCode::AuthExpired,
        retriable: false,
        retry_after_ms: None,
        safe_message: "HTTP 401".to_string(),
        request_id: None,
    });
    let err = engine
        .catalog_query(
            CatalogQueryRequest {
                kind: CatalogQueryKind::LibraryPlaylists,
                query: None,
                playlist_id: None,
                album_id: None,
                artist_id: None,
                cursor: None,
                limit: None,
            },
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, QueryErrorCode::AuthExpired);
    let snap = engine.snapshot().await.unwrap();
    assert_eq!(snap.auth_state.state, AuthState::Expired);
    assert_eq!(
        snap.capabilities.like_mutation,
        CapabilitySupport::Unsupported
    );
    // Cached data of the same account stays browsable as unverified.
    assert_eq!(
        snap.capabilities.library_playlists,
        CapabilitySupport::Unverified
    );
    assert!(snap.auth_state.account_scope.is_some());

    // Fresh credentials for the same epoch restore the session.
    sign_in(&engine).await;
    let snap = engine.snapshot().await.unwrap();
    assert_eq!(snap.auth_state.state, AuthState::Authenticated);
    assert_eq!(
        snap.capabilities.like_mutation,
        CapabilitySupport::Supported
    );
    engine.shutdown().await.unwrap();
}

/// Storage double recording the calls the core makes and answering `last_session`.
#[derive(Default)]
struct RecordingStorage {
    pending_session: Option<Arc<Notify>>,
    pending_logout: Option<Arc<Notify>>,
    fail_logout_cleanup: bool,
    fail_save_settings: bool,
    saved_settings: Mutex<Option<ytm_protocol::AppSettings>>,
    last_session: Mutex<Option<LastSession>>,
    cleared_scopes: Mutex<Vec<AccountScope>>,
    recorded: Mutex<Vec<(AccountScope, LastPlayed)>>,
    positions: Mutex<Vec<(VideoId, u64)>>,
    cover_requests: Mutex<u32>,
}

const SYNTHETIC_ASSET: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

impl StoragePort for RecordingStorage {
    fn load_settings(
        &self,
    ) -> BoxFuture<'_, Result<Option<ytm_protocol::AppSettings>, QueryError>> {
        Box::pin(async { Ok(None) })
    }
    fn save_settings(
        &self,
        settings: ytm_protocol::AppSettings,
    ) -> BoxFuture<'_, Result<(), QueryError>> {
        if self.fail_save_settings {
            return Box::pin(async {
                Err(QueryError {
                    code: QueryErrorCode::StorageError,
                    retriable: false,
                    retry_after_ms: None,
                    safe_message: "settings disk write failed".to_string(),
                    request_id: None,
                })
            });
        }
        *self.saved_settings.lock().unwrap() = Some(settings);
        Box::pin(async { Ok(()) })
    }
    fn clear_cache(&self, _scope: Option<AccountScope>) -> BoxFuture<'_, Result<(), QueryError>> {
        Box::pin(async { Ok(()) })
    }
    fn resolve_asset(
        &self,
        asset_id: ytm_protocol::AssetId,
    ) -> BoxFuture<'_, Result<ytm_protocol::AssetResolvedPayload, QueryError>> {
        Box::pin(async move {
            Ok(ytm_protocol::AssetResolvedPayload {
                asset_id,
                file_path: None,
                mime_type: None,
                error: None,
            })
        })
    }
    fn cache_cover(&self, _remote_url: String) -> BoxFuture<'_, Result<CachedCover, QueryError>> {
        *self.cover_requests.lock().unwrap() += 1;
        Box::pin(async {
            Ok(CachedCover {
                asset_id: ytm_protocol::AssetId::new(SYNTHETIC_ASSET).unwrap(),
                accent_oklch: "oklch(0.600 0.100 250.0)".to_string(),
            })
        })
    }
    fn clear_account_data(
        &self,
        scope: Option<AccountScope>,
        _pending_epoch: Option<AccountEpoch>,
    ) -> BoxFuture<'_, Result<(), QueryError>> {
        if let Some(scope) = scope {
            self.cleared_scopes.lock().unwrap().push(scope);
        }
        Box::pin(async {
            if let Some(hold) = &self.pending_logout {
                hold.notified().await;
            }
            if self.fail_logout_cleanup {
                return Err(QueryError {
                    code: QueryErrorCode::StorageError,
                    retriable: false,
                    retry_after_ms: None,
                    safe_message: "synthetic storage failure".to_owned(),
                    request_id: None,
                });
            }
            Ok(())
        })
    }
    fn last_session(&self) -> BoxFuture<'_, Result<Option<LastSession>, QueryError>> {
        let session = self.last_session.lock().unwrap().clone();
        Box::pin(async move {
            if let Some(hold) = &self.pending_session {
                hold.notified().await;
            }
            Ok(session)
        })
    }
    fn record_playback(
        &self,
        scope: AccountScope,
        _epoch: AccountEpoch,
        played: LastPlayed,
    ) -> BoxFuture<'_, Result<(), QueryError>> {
        self.recorded.lock().unwrap().push((scope, played));
        Box::pin(async { Ok(()) })
    }
    fn update_playback_position(
        &self,
        _scope: AccountScope,
        _epoch: AccountEpoch,
        video_id: VideoId,
        position_ms: u64,
    ) -> BoxFuture<'_, Result<(), QueryError>> {
        self.positions.lock().unwrap().push((video_id, position_ms));
        Box::pin(async { Ok(()) })
    }
}

struct StorageHarness {
    engine: CoreEngine,
    clock: Arc<MockClock>,
    bridge: Arc<RecordingBridge>,
    gate: Arc<RecordingOutputGate>,
    storage: Arc<RecordingStorage>,
}

async fn engine_with_storage(storage: RecordingStorage, settings_autoplay: bool) -> StorageHarness {
    let clock = Arc::new(MockClock::with_wall_offset(1_000, 1_700_000_000_000));
    let bridge = Arc::new(RecordingBridge::default());
    let gate = Arc::new(RecordingOutputGate::default());
    let storage = Arc::new(storage);
    let engine = CoreEngine::start(
        CoreDeps {
            clock: clock.clone(),
            bridge: bridge.clone(),
            output_gate: gate.clone(),
            catalog: None,
            storage: Some(storage.clone()),
            lyrics: None,
        },
        EngineConfig {
            dispatch_timeout_ms: 1_000,
            tick_interval_ms: 20,
            mock_mode: true,
            ..EngineConfig::default()
        },
    );
    if settings_autoplay {
        engine
            .patch_settings(ytm_protocol::AppSettingsPatch {
                autoplay_on_launch: Some(true),
                ..Default::default()
            })
            .await
            .unwrap();
    }
    // Let the actor finish loading the last session.
    engine.tick().await.unwrap();
    tokio::task::yield_now().await;
    engine.tick().await.unwrap();
    StorageHarness {
        engine,
        clock,
        bridge,
        gate,
        storage,
    }
}

async fn settle(engine: &CoreEngine) {
    for _ in 0..5 {
        tokio::task::yield_now().await;
        engine.tick().await.unwrap();
    }
}

async fn engine_with_saved_playback() -> StorageHarness {
    let storage = RecordingStorage::default();
    // `sign_in` without a catalog binds `scope-acct-1`, the account the track was saved under.
    *storage.last_session.lock().unwrap() = Some(LastSession {
        account_scope: AccountScope::new("scope-acct-1").unwrap(),
        last_played: Some(LastPlayed {
            track: make_track("vid-last", "Last", Some(200_000)),
            playback_ref: None,
            position_ms: 42_000,
        }),
    });
    engine_with_storage(storage, false).await
}

#[tokio::test]
async fn same_account_relogin_waits_for_logout_storage_cleanup_to_finish() {
    for fail_logout_cleanup in [false, true] {
        let hold = Arc::new(Notify::new());
        let engine = CoreEngine::start(
            CoreDeps {
                storage: Some(Arc::new(RecordingStorage {
                    pending_logout: Some(hold.clone()),
                    fail_logout_cleanup,
                    ..Default::default()
                })),
                catalog: Some(Arc::new(ScriptedCatalog::new("stable-identity"))),
                ..Default::default()
            },
            Default::default(),
        );
        sign_in(&engine).await;
        let mut logout = std::pin::pin!(engine.on_logout());
        tokio::select! {
            biased;
            result = &mut logout => panic!("logout completed before SQLite cleanup: {result:?}"),
            _ = std::future::ready(()) => {}
        }

        let epoch = AccountEpoch::new("new-epoch").unwrap();
        let binding = engine
            .enqueue_session_material(SessionMaterialPayload {
                account_epoch: epoch.clone(),
                credential_revision: CredentialRevision::new("new-rev").unwrap(),
                cookie_pairs: BTreeMap::new(),
                channel_label: None,
                auth_user_index: None,
            })
            .await
            .unwrap();
        settle(&engine).await;
        assert_eq!(
            engine.snapshot().await.unwrap().auth_state.state,
            AuthState::SignedOut
        );
        hold.notify_one();
        assert_eq!(logout.await.is_err(), fail_logout_cleanup);
        binding.await.unwrap();
        assert_eq!(
            engine.snapshot().await.unwrap().auth_state.account_epoch,
            Some(epoch)
        );
        engine.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn signed_out_ignores_late_saved_playback() {
    let hold = Arc::new(Notify::new());
    let storage = RecordingStorage {
        last_session: Mutex::new(Some(LastSession {
            account_scope: AccountScope::new("previous-scope").unwrap(),
            last_played: Some(LastPlayed {
                track: make_track("private-video", "Saved track", Some(150_000)),
                playback_ref: None,
                position_ms: 10_000,
            }),
        })),
        pending_session: Some(hold.clone()),
        ..RecordingStorage::default()
    };
    let h = engine_with_storage(storage, false).await;
    h.engine.on_logout().await.unwrap();
    hold.notify_one();
    settle(&h.engine).await;
    init_ready_bridge(&h.engine, "bridge-1", 1).await;
    settle(&h.engine).await;
    let snap = h.engine.snapshot().await.unwrap();
    assert_eq!(snap.auth_state.state, AuthState::SignedOut);
    assert!(snap.auth_state.account_scope.is_none());
    assert!(h.bridge.sent_ops().is_empty());
    assert!(h.gate.calls().is_empty());
    h.engine.shutdown().await.unwrap();
}

/// The first playback observation of a page that has not loaded any track.
fn empty_page_obs(epoch: &str) -> BridgePlaybackObservedPayload {
    BridgePlaybackObservedPayload {
        track_instance_id: None,
        current_track: None,
        current_queue_entry_id: None,
        ..make_obs(
            epoch,
            PlaybackStatus::Idle,
            "ti-unused",
            "vid-unused",
            "qe-unused",
            0,
            None,
        )
    }
}

async fn observe_empty_page(engine: &CoreEngine, seq: u64) {
    assert!(
        engine
            .on_bridge_playback_observed(seq, empty_page_obs("bridge-1"))
            .await
            .unwrap()
    );
    settle(engine).await;
}

async fn observe_restore(
    engine: &CoreEngine,
    seq: u64,
    playback: PlaybackStatus,
    position_ms: u64,
) {
    assert!(
        engine
            .on_bridge_playback_observed(
                seq,
                make_obs(
                    "bridge-1",
                    playback,
                    "ti-r",
                    "vid-last",
                    "qe-r",
                    position_ms,
                    Some(200_000),
                ),
            )
            .await
            .unwrap()
    );
    settle(engine).await;
}

#[tokio::test]
async fn launch_seeks_while_silent_then_observes_pause_before_ungating() {
    let h = engine_with_saved_playback().await;
    // The saved scope is offered for offline browsing before any session material.
    let snap = h.engine.snapshot().await.unwrap();
    assert_eq!(
        snap.auth_state.account_scope.as_ref().map(|s| s.as_str()),
        Some("scope-acct-1")
    );

    sign_in(&h.engine).await;
    init_ready_bridge(&h.engine, "bridge-1", 1).await;
    settle(&h.engine).await;
    observe_empty_page(&h.engine, 2).await;
    assert_eq!(h.bridge.sent_ops(), vec![PlayerOp::PlayTarget]);
    let gate_tx = h.gate.calls()[0].clone();
    assert!(gate_tx.1, "gate engaged before the page is asked to play");

    observe_restore(&h.engine, 3, PlaybackStatus::Paused, 0).await;
    assert_eq!(h.bridge.sent_ops(), vec![PlayerOp::PlayTarget]);
    observe_restore(&h.engine, 4, PlaybackStatus::Playing, 0).await;
    assert_eq!(
        h.bridge.sent_ops(),
        vec![PlayerOp::PlayTarget, PlayerOp::Seek]
    );
    let (_, seek) = h.bridge.sent_commands().last().cloned().unwrap();
    let args = seek.args.unwrap();
    assert_eq!(args.position_ms, Some(42_000));
    assert_eq!(args.track_instance_id.unwrap().as_str(), "ti-r");
    assert_eq!(h.gate.calls(), vec![gate_tx.clone()]);

    observe_restore(&h.engine, 5, PlaybackStatus::Playing, 42_200).await;
    assert_eq!(
        h.bridge.sent_ops(),
        vec![PlayerOp::PlayTarget, PlayerOp::Seek, PlayerOp::Pause]
    );
    assert_eq!(h.gate.calls(), vec![gate_tx.clone()]);
    observe_restore(&h.engine, 6, PlaybackStatus::Paused, 42_200).await;
    assert!(
        h.gate.calls().contains(&(gate_tx.0.clone(), false)),
        "gate released once paused"
    );
    let snap = h.engine.snapshot().await.unwrap();
    assert_eq!(snap.player.playback, PlaybackStatus::Paused);
    assert_eq!(snap.player.time_anchor.position_ms, 42_200);
    // The silent play/pause neither adds a history row nor overwrites the saved 42 s.
    assert!(h.storage.recorded.lock().unwrap().is_empty());
    assert!(h.storage.positions.lock().unwrap().is_empty());
    h.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn launch_restore_waits_for_the_first_playback_observation_of_the_page() {
    for page_restored_a_track in [false, true] {
        let h = engine_with_saved_playback().await;
        sign_in(&h.engine).await;
        init_ready_bridge(&h.engine, "bridge-1", 1).await;
        settle(&h.engine).await;
        // Ready alone does not say whether the official page restored a track itself.
        assert!(h.bridge.sent_ops().is_empty());
        if page_restored_a_track {
            observe_restore(&h.engine, 2, PlaybackStatus::Paused, 5_000).await;
            assert!(
                h.bridge.sent_ops().is_empty(),
                "the page's own track is kept"
            );
            // The page restored the saved track: it continues the saved history row.
            assert!(h.storage.recorded.lock().unwrap().is_empty());
            // A later reload that comes up empty is not a launch any more.
            init_ready_bridge(&h.engine, "bridge-2", 1).await;
            assert!(
                h.engine
                    .on_bridge_playback_observed(2, empty_page_obs("bridge-2"))
                    .await
                    .unwrap()
            );
            settle(&h.engine).await;
            assert!(
                h.bridge.sent_ops().is_empty(),
                "no restore over a reloaded page"
            );
        } else {
            observe_empty_page(&h.engine, 2).await;
            assert_eq!(h.bridge.sent_ops(), vec![PlayerOp::PlayTarget]);
        }
        h.engine.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn saved_playback_of_another_account_is_never_restored() {
    let storage = RecordingStorage::default();
    *storage.last_session.lock().unwrap() = Some(LastSession {
        account_scope: AccountScope::new("scope-other-account").unwrap(),
        last_played: Some(LastPlayed {
            track: make_track("vid-other", "Other account", Some(200_000)),
            playback_ref: None,
            position_ms: 42_000,
        }),
    });
    let h = engine_with_storage(storage, true).await;
    // The empty page is observed before the session binds; the bound account differs.
    init_ready_bridge(&h.engine, "bridge-1", 1).await;
    observe_empty_page(&h.engine, 2).await;
    assert!(
        h.bridge.sent_ops().is_empty(),
        "no restore before the account is bound"
    );
    sign_in(&h.engine).await;
    settle(&h.engine).await;
    assert_eq!(
        h.engine
            .snapshot()
            .await
            .unwrap()
            .auth_state
            .account_scope
            .map(|s| s.as_str().to_owned()),
        Some("scope-acct-1".to_owned())
    );
    observe_empty_page(&h.engine, 3).await;
    assert!(h.bridge.sent_ops().is_empty());
    assert!(h.gate.calls().is_empty());
    h.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn unobserved_restore_seek_yields_to_the_short_deadline_or_explicit_play() {
    for user_plays in [false, true] {
        let h = engine_with_saved_playback().await;
        sign_in(&h.engine).await;
        init_ready_bridge(&h.engine, "bridge-1", 1).await;
        settle(&h.engine).await;
        observe_empty_page(&h.engine, 2).await;
        observe_restore(&h.engine, 3, PlaybackStatus::Playing, 0).await;
        assert_eq!(
            h.bridge.sent_ops(),
            vec![PlayerOp::PlayTarget, PlayerOp::Seek]
        );

        if user_plays {
            h.engine
                .submit_player_command(make_cmd("explicit-play", PlayerOp::Play, None))
                .await
                .unwrap();
        } else {
            // The page never reported the requested position. The 8 s control
            // timeout has not elapsed, but restoration must already continue.
            h.clock.advance(2_000);
        }
        settle(&h.engine).await;
        assert_eq!(
            h.bridge.sent_ops(),
            vec![PlayerOp::PlayTarget, PlayerOp::Seek, PlayerOp::Pause]
        );
        assert_eq!(h.gate.calls().len(), 1);

        observe_restore(&h.engine, 4, PlaybackStatus::Paused, 800).await;
        assert_eq!(h.gate.calls().len(), 2);
        assert!(!h.gate.calls()[1].1);
        if user_plays {
            assert_eq!(h.bridge.sent_ops().last(), Some(&PlayerOp::Play));
            observe_restore(&h.engine, 5, PlaybackStatus::Playing, 900).await;
            let receipt = h
                .engine
                .command_receipt(CommandId::new("explicit-play").unwrap())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(receipt.status, CommandLifecycleStatus::Observed);
        } else {
            let snap = h.engine.snapshot().await.unwrap();
            assert_eq!(snap.player.playback, PlaybackStatus::Paused);
            assert_eq!(snap.player.time_anchor.position_ms, 800);
        }
        h.engine.shutdown().await.unwrap();
    }
}

/// A caller's playTarget or enqueue that reached the page before the launch restore could start
/// may still be loading there when it times out. The restore must not navigate on top of it.
#[tokio::test]
async fn an_outside_play_or_enqueue_before_the_launch_restore_replaces_the_restore() {
    for op in [PlayerOp::PlayTarget, PlayerOp::Enqueue] {
        let h = engine_with_saved_playback().await;
        sign_in(&h.engine).await;
        init_ready_bridge(&h.engine, "bridge-1", 1).await;
        settle(&h.engine).await;
        let mut args = make_args();
        args.target = Some(PlaybackRef {
            video_id: VideoId::new("vid-caller").unwrap(),
            playlist_id: None,
            playlist_entry_id: None,
            browse_id: None,
            queue_entry_id: None,
            start_index: None,
        });
        if op == PlayerOp::Enqueue {
            args.enqueue_position = Some(EnqueuePosition::Next);
        }
        h.engine
            .submit_player_command(make_cmd("caller-first", op, Some(args)))
            .await
            .unwrap();
        settle(&h.engine).await;
        observe_empty_page(&h.engine, 2).await;
        h.clock.advance(15_500);
        settle(&h.engine).await;
        let receipt = h
            .engine
            .command_receipt(CommandId::new("caller-first").unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(receipt.status, CommandLifecycleStatus::Indeterminate);
        observe_empty_page(&h.engine, 3).await;
        assert_eq!(h.bridge.sent_ops(), vec![op], "{op:?}: no restore after it");
        assert!(h.gate.calls().is_empty());
        h.engine.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn now_playing_cover_gets_local_asset_and_accent() {
    let h = engine_with_storage(RecordingStorage::default(), false).await;
    init_ready_bridge(&h.engine, "bridge-1", 1).await;
    let mut obs = make_obs(
        "bridge-1",
        PlaybackStatus::Playing,
        "ti-1",
        "vid-1",
        "qe-1",
        0,
        Some(200_000),
    );
    obs.current_track.as_mut().unwrap().cover = Some(ytm_protocol::CoverRef {
        asset_id: None,
        remote_url: Some("https://lh3.googleusercontent.com/art=w60-h60".to_string()),
        accent_oklch: None,
    });
    h.engine
        .on_bridge_playback_observed(2, obs.clone())
        .await
        .unwrap();
    settle(&h.engine).await;
    // Later observations carry only the remote URL again; the cached asset stays attached.
    obs.position_ms = 1_000;
    h.engine.on_bridge_playback_observed(3, obs).await.unwrap();
    let cover = h
        .engine
        .snapshot()
        .await
        .unwrap()
        .player
        .current_track
        .unwrap()
        .cover
        .unwrap();
    assert_eq!(cover.asset_id.unwrap().as_str(), SYNTHETIC_ASSET);
    assert_eq!(
        cover.accent_oklch.as_deref(),
        Some("oklch(0.600 0.100 250.0)")
    );
    assert_eq!(*h.storage.cover_requests.lock().unwrap(), 1);
    // Clearing the cache drops the URL mapping; the now-playing cover is cached again.
    h.engine.clear_cache().await.unwrap();
    settle(&h.engine).await;
    assert_eq!(*h.storage.cover_requests.lock().unwrap(), 2);
    h.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn track_changes_are_recorded_and_logout_clears_the_account_cache() {
    let h = engine_with_storage(RecordingStorage::default(), false).await;
    init_ready_bridge(&h.engine, "bridge-1", 1).await;
    h.engine
        .on_session_material(SessionMaterialPayload {
            account_epoch: AccountEpoch::new("acct-1").unwrap(),
            credential_revision: CredentialRevision::new("cred-1").unwrap(),
            cookie_pairs: BTreeMap::from([("SAPISID".to_string(), "synthetic".to_string())]),
            auth_user_index: None,
            channel_label: None,
        })
        .await
        .unwrap();
    for (seq, vid) in [(2, "vid-a"), (3, "vid-a"), (4, "vid-b")] {
        h.engine
            .on_bridge_playback_observed(
                seq,
                make_obs(
                    "bridge-1",
                    PlaybackStatus::Playing,
                    &format!("ti-{vid}"),
                    vid,
                    &format!("qe-{vid}"),
                    0,
                    Some(200_000),
                ),
            )
            .await
            .unwrap();
    }
    settle(&h.engine).await;
    let recorded: Vec<String> = h
        .storage
        .recorded
        .lock()
        .unwrap()
        .iter()
        .map(|(_, p)| p.track.video_id.as_str().to_string())
        .collect();
    assert_eq!(recorded, vec!["vid-a", "vid-b"]);

    h.engine.on_logout().await.unwrap();
    settle(&h.engine).await;
    assert_eq!(
        h.storage.cleared_scopes.lock().unwrap().as_slice(),
        &[AccountScope::new("scope-acct-1").unwrap()]
    );
    h.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn consecutive_plays_of_the_same_video_are_separate_history_rows() {
    let h = engine_with_storage(RecordingStorage::default(), false).await;
    sign_in(&h.engine).await;
    init_ready_bridge(&h.engine, "bridge-1", 1).await;
    // Queue A, A: the second occurrence is a new instance; a lagging selection flips the
    // entry of the second instance without a new play.
    for (seq, instance, entry) in [
        (2, "ti-1", "qe-1"),
        (3, "ti-2", "qe-2"),
        (4, "ti-2", "qe-1"),
    ] {
        h.engine
            .on_bridge_playback_observed(
                seq,
                make_obs(
                    "bridge-1",
                    PlaybackStatus::Playing,
                    instance,
                    "vid-a",
                    entry,
                    0,
                    Some(200_000),
                ),
            )
            .await
            .unwrap();
    }
    settle(&h.engine).await;
    let recorded: Vec<String> = h
        .storage
        .recorded
        .lock()
        .unwrap()
        .iter()
        .map(|(_, p)| p.track.video_id.as_str().to_string())
        .collect();
    assert_eq!(recorded, vec!["vid-a", "vid-a"]);
    h.engine.shutdown().await.unwrap();
}

struct NotFoundLyrics;

impl ytm_core::LyricsPort for NotFoundLyrics {
    fn query_lyrics(
        &self,
        request: ytm_protocol::LyricsQueryRequest,
        _cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<ytm_protocol::LyricsResult, QueryError>> {
        Box::pin(async move {
            Ok(ytm_protocol::LyricsResult {
                video_id: request.track.video_id,
                kind: ytm_protocol::LyricsKind::NotFound,
                source: "lrclib".to_string(),
                language: None,
                match_basis: None,
                fetched_at_ms: 1,
                file_offset_ms: 0,
                user_offset_ms: 0,
                synced_lines: Vec::new(),
                plain_text: None,
                error: None,
            })
        })
    }
}

#[tokio::test]
async fn lrclib_miss_falls_back_to_official_plain_lyrics() {
    let catalog = Arc::new(ScriptedCatalog::new("acct:aaaa"));
    *catalog.official.lock().unwrap() = Some(OfficialLyrics {
        text: "line one\nline two".to_string(),
        source: "Source: Synthetic".to_string(),
    });
    let engine = CoreEngine::start(
        CoreDeps {
            clock: Arc::new(MockClock::new(1_000)),
            bridge: Arc::new(RecordingBridge::default()),
            output_gate: Arc::new(RecordingOutputGate::default()),
            catalog: Some(catalog),
            storage: None,
            lyrics: Some(Arc::new(NotFoundLyrics)),
        },
        EngineConfig::default(),
    );
    sign_in(&engine).await;
    let res = engine
        .lyrics_query(ytm_protocol::LyricsQueryRequest {
            track: ytm_protocol::LyricsMatchMetadata {
                video_id: VideoId::new("vid-lyr").unwrap(),
                title: "Song".to_string(),
                artist: "Artist".to_string(),
                album: None,
                duration_ms: None,
                version_tag: None,
            },
            set_user_offset_ms: None,
            reset_user_offset: false,
        })
        .await
        .unwrap();
    assert_eq!(res.kind, ytm_protocol::LyricsKind::Plain);
    assert!(
        res.synced_lines.is_empty(),
        "plain lyrics are never seekable"
    );
    assert_eq!(res.plain_text.as_deref(), Some("line one\nline two"));
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn leaving_the_music_origin_or_a_renderer_crash_marks_the_bridge_lost_until_ready() {
    let engine = engine_with_catalog(Arc::new(ScriptedCatalog::new("acct:aaaa"))).await;
    let lifecycle = |event: &str, music: bool| BridgeLifecyclePayload {
        event: event.to_string(),
        bridge_epoch: None,
        is_music_origin: Some(music),
    };
    let connection = || async { engine.snapshot().await.unwrap().player.connection };

    announce_bridge(&engine, 1, "acct:aaaa").await;
    sign_in(&engine).await;
    // In-app SPA navigation on the music origin is informational.
    engine
        .on_host_bridge_lifecycle(lifecycle("did-navigate-in-page", true))
        .await
        .unwrap();
    assert_eq!(connection().await, PlayerConnectionState::Ready);

    // The player went to a consent / sign-in page: no bridge can answer commands there.
    engine
        .on_host_bridge_lifecycle(lifecycle("did-navigate", false))
        .await
        .unwrap();
    assert_eq!(connection().await, PlayerConnectionState::Disconnected);
    let disconnected = engine.snapshot().await.unwrap();
    assert!(!disconnected.auth_state.player_catalog_matched);
    assert_eq!(
        disconnected.capabilities.search,
        CapabilitySupport::Supported
    );
    assert_eq!(
        disconnected.capabilities.like_mutation,
        CapabilitySupport::Unverified
    );
    init_ready_bridge(&engine, "bridge-2", 1).await;
    assert_eq!(connection().await, PlayerConnectionState::Ready);

    // The host reports a renderer crash with the structured event name; the dead page no
    // longer counts as playing.
    engine
        .on_bridge_playback_observed(
            2,
            make_obs(
                "bridge-2",
                PlaybackStatus::Playing,
                "ti-1",
                "vid-1",
                "qe-1",
                1_000,
                Some(200_000),
            ),
        )
        .await
        .unwrap();
    let (_, mut deltas) = engine.subscribe_snapshot().await.unwrap();
    engine
        .on_host_bridge_lifecycle(lifecycle("crashed", false))
        .await
        .unwrap();
    let snap = engine.snapshot().await.unwrap();
    assert_eq!(snap.player.connection, PlayerConnectionState::Disconnected);
    assert_eq!(snap.player.playback, PlaybackStatus::Paused);
    assert_eq!(snap.capabilities.play_pause, CapabilitySupport::Unsupported);
    assert_eq!(snap.capabilities.seek, CapabilitySupport::Unsupported);
    assert_eq!(
        snap.capabilities.queue_mirror,
        CapabilitySupport::Unsupported
    );
    assert_eq!(
        snap.capabilities.library_playlists,
        CapabilitySupport::Supported
    );
    assert!(matches!(
        deltas.try_recv().unwrap(),
        StateDelta::CapabilitiesChanged { capabilities, .. } if capabilities == snap.capabilities
    ));
    assert!(
        !engine
            .on_bridge_ready(
                3,
                BridgeReadyPayload {
                    bridge_epoch: BridgeEpoch::new("bridge-2").unwrap(),
                    bridge_version: "0.1.0".to_string(),
                    capabilities: supported_caps(),
                    account_hint: None,
                    logged_in_observed: true,
                }
            )
            .await
            .unwrap()
    );
    assert_eq!(connection().await, PlayerConnectionState::Disconnected);
    init_ready_bridge(&engine, "bridge-3", 1).await;
    assert_eq!(connection().await, PlayerConnectionState::Ready);
    assert_eq!(
        engine.snapshot().await.unwrap().capabilities.play_pause,
        CapabilitySupport::Supported
    );
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn play_before_the_page_loaded_any_track_fails_at_once_instead_of_going_indeterminate() {
    let (engine, _clock, bridge, _gate) = setup_engine().await;
    init_ready_bridge(&engine, "bridge-1", 1).await;

    // Right after start-up the page has not restored its queue: nothing to play yet.
    let early = engine
        .submit_player_command(make_cmd("cmd-early", PlayerOp::Toggle, None))
        .await;
    let err = early.expect_err("rejected without dispatch");
    assert_eq!(err.code, QueryErrorCode::UnavailableTrack);
    assert!(err.retriable);
    let receipt = engine
        .command_receipt(CommandId::new("cmd-early").unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(receipt.status, CommandLifecycleStatus::Failed);
    assert!(bridge.sent_ops().is_empty());

    // Once the page reports its (paused) track, the next click plays it.
    engine
        .on_bridge_playback_observed(
            2,
            make_obs(
                "bridge-1",
                PlaybackStatus::Paused,
                "ti-1",
                "vid-1",
                "qe-1",
                0,
                Some(200_000),
            ),
        )
        .await
        .unwrap();
    engine
        .submit_player_command(make_cmd("cmd-later", PlayerOp::Toggle, None))
        .await
        .unwrap();
    assert_eq!(bridge.sent_ops(), vec![PlayerOp::Play]);
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn patch_settings_merges_fields_and_leaves_memory_unchanged_when_save_fails() {
    let storage = RecordingStorage::default();
    let h = engine_with_storage(storage, false).await;

    let first = h
        .engine
        .patch_settings(ytm_protocol::AppSettingsPatch {
            reduce_transparency: Some(true),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(first.reduce_transparency);
    assert_eq!(first.ui_language, ytm_protocol::UiLanguage::English);

    let second = h
        .engine
        .patch_settings(ytm_protocol::AppSettingsPatch {
            ui_language: Some(ytm_protocol::UiLanguage::SimplifiedChinese),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(second.reduce_transparency);
    assert_eq!(
        second.ui_language,
        ytm_protocol::UiLanguage::SimplifiedChinese
    );
    assert_eq!(
        h.storage.saved_settings.lock().unwrap().as_ref(),
        Some(&second)
    );
    h.engine.shutdown().await.unwrap();

    let failing = RecordingStorage {
        fail_save_settings: true,
        ..RecordingStorage::default()
    };
    let h_fail = engine_with_storage(failing, false).await;
    let err_fail = h_fail
        .engine
        .patch_settings(ytm_protocol::AppSettingsPatch {
            reduce_motion: Some(true),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(err_fail.code, QueryErrorCode::StorageError);
    // An empty patch changes nothing, so it never reaches the disk.
    let unchanged = h_fail
        .engine
        .patch_settings(ytm_protocol::AppSettingsPatch::default())
        .await
        .unwrap();
    assert!(!unchanged.reduce_motion);
    assert!(
        !h_fail
            .engine
            .snapshot()
            .await
            .unwrap()
            .settings
            .reduce_motion
    );
    h_fail.engine.shutdown().await.unwrap();
}

/// The page restores its own track paused while the bridge has not yet verified play/pause, so
/// launch autoplay waits; the control then turns supported. Returns the ops sent to the page,
/// with an optional manual Pause arriving during the wait.
async fn self_restore_while_play_pause_unverified(manual_pause: bool) -> Vec<PlayerOp> {
    let h = engine_with_storage(RecordingStorage::default(), true).await;
    sign_in(&h.engine).await;
    let ready = |seq, play_pause| {
        h.engine.on_bridge_ready(
            seq,
            BridgeReadyPayload {
                bridge_epoch: BridgeEpoch::new("bridge-1").unwrap(),
                bridge_version: "0.1.0".to_string(),
                capabilities: CapabilitySet {
                    play_pause,
                    ..supported_caps()
                },
                account_hint: Some("TestChannel".to_string()),
                logged_in_observed: true,
            },
        )
    };
    assert!(ready(1, CapabilitySupport::Unverified).await.unwrap());
    observe_restore(&h.engine, 2, PlaybackStatus::Paused, 12_000).await;
    assert!(h.bridge.sent_ops().is_empty(), "autoplay waits");
    let mut seq = 3;
    if manual_pause {
        h.engine
            .submit_player_command(make_cmd("user-pause", PlayerOp::Pause, None))
            .await
            .unwrap();
        settle(&h.engine).await;
        observe_restore(&h.engine, seq, PlaybackStatus::Paused, 12_000).await;
        seq += 1;
    }
    assert!(ready(seq, CapabilitySupport::Supported).await.unwrap());
    settle(&h.engine).await;
    observe_restore(&h.engine, seq + 1, PlaybackStatus::Paused, 12_000).await;
    let ops = h.bridge.sent_ops();
    h.engine.shutdown().await.unwrap();
    ops
}

#[tokio::test]
async fn a_failed_paused_restore_leaves_the_player_as_is_instead_of_autoplaying_later() {
    let storage = RecordingStorage::default();
    *storage.last_session.lock().unwrap() = Some(LastSession {
        account_scope: AccountScope::new("scope-acct-1").unwrap(),
        last_played: Some(LastPlayed {
            track: make_track("vid-last", "Last", Some(200_000)),
            playback_ref: None,
            position_ms: 42_000,
        }),
    });
    let h = engine_with_storage(storage, true).await;
    sign_in(&h.engine).await;
    init_ready_bridge(&h.engine, "bridge-1", 1).await;
    *h.bridge.fail_next_dispatch.lock().unwrap() = Some(QueryError {
        code: QueryErrorCode::Timeout,
        retriable: true,
        retry_after_ms: None,
        safe_message: "page did not take the command".to_string(),
        request_id: None,
    });
    observe_empty_page(&h.engine, 2).await;
    // The restore's PlayTarget was refused, so nothing reached the page.
    assert!(h.bridge.sent_ops().is_empty());
    assert!(h.bridge.fail_next_dispatch.lock().unwrap().is_none());
    // The page later shows the saved track paused on its own; it stays paused.
    observe_restore(&h.engine, 3, PlaybackStatus::Paused, 0).await;
    observe_restore(&h.engine, 4, PlaybackStatus::Paused, 0).await;
    assert!(
        !h.bridge.sent_ops().contains(&PlayerOp::Play),
        "{:?}",
        h.bridge.sent_ops()
    );
    h.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn launch_autoplay_honors_setting_when_page_self_restores_and_manual_pause_wins() {
    // 1. Page self-restores paused with autoplay_on_launch = false -> stays paused, no PlayTarget/Play.
    {
        let h = engine_with_storage(RecordingStorage::default(), false).await;
        sign_in(&h.engine).await;
        init_ready_bridge(&h.engine, "bridge-1", 1).await;
        observe_restore(&h.engine, 2, PlaybackStatus::Paused, 12_000).await;
        assert!(h.bridge.sent_ops().is_empty());
        h.engine.shutdown().await.unwrap();
    }

    // 2. Page self-restores paused with autoplay_on_launch = true -> keeps page track/queue, sends Play.
    {
        let h = engine_with_storage(RecordingStorage::default(), true).await;
        sign_in(&h.engine).await;
        init_ready_bridge(&h.engine, "bridge-1", 1).await;
        assert!(
            h.engine
                .on_bridge_queue_observed(
                    2,
                    QueueSnapshot {
                        bridge_epoch: Some(BridgeEpoch::new("bridge-1").unwrap()),
                        queue_revision: 3,
                        current_entry_id: Some(QueueEntryId::new("qe-r").unwrap()),
                        loaded_range: None,
                        completeness: ytm_protocol::QueueCompleteness::Complete,
                        has_more: Some(false),
                        explicit_complete: Some(true),
                        may_load_more: Some(false),
                        official_upcoming: None,
                        filters: None,
                        entries: vec![QueueEntry {
                            entry_id: QueueEntryId::new("qe-r").unwrap(),
                            video_id: VideoId::new("vid-last").unwrap(),
                            track: make_track("vid-last", "Last", Some(200_000)),
                            availability: TrackAvailability::Available,
                        }],
                    },
                )
                .await
                .unwrap()
        );
        observe_restore(&h.engine, 3, PlaybackStatus::Paused, 12_000).await;
        assert_eq!(h.bridge.sent_ops(), vec![PlayerOp::Play]);
        assert_eq!(
            h.engine
                .snapshot()
                .await
                .unwrap()
                .queue_summary
                .queue_revision,
            3
        );
        h.engine.shutdown().await.unwrap();
    }

    // 3. Manual Pause while launch autoplay still waits preempts it on a self-restored track.
    // Without the Pause the same start-up plays, so the wait really was pending.
    assert_eq!(
        self_restore_while_play_pause_unverified(false).await,
        vec![PlayerOp::Play]
    );
    let ops = self_restore_while_play_pause_unverified(true).await;
    assert!(ops.contains(&PlayerOp::Pause), "{ops:?}");
    assert!(!ops.contains(&PlayerOp::Play), "{ops:?}");

    // 4. Page empty at launch with saved track and autoplay_on_launch = true -> PlayTarget -> Seek -> Pause -> Play.
    {
        let storage = RecordingStorage::default();
        *storage.last_session.lock().unwrap() = Some(LastSession {
            account_scope: AccountScope::new("scope-acct-1").unwrap(),
            last_played: Some(LastPlayed {
                track: make_track("vid-last", "Last", Some(200_000)),
                playback_ref: None,
                position_ms: 42_000,
            }),
        });
        let h = engine_with_storage(storage, true).await;
        sign_in(&h.engine).await;
        init_ready_bridge(&h.engine, "bridge-1", 1).await;
        observe_empty_page(&h.engine, 2).await;
        assert_eq!(h.bridge.sent_ops(), vec![PlayerOp::PlayTarget]);
        observe_restore(&h.engine, 3, PlaybackStatus::Playing, 0).await;
        observe_restore(&h.engine, 4, PlaybackStatus::Playing, 42_000).await;
        observe_restore(&h.engine, 5, PlaybackStatus::Paused, 42_000).await;
        assert_eq!(
            h.bridge.sent_ops(),
            vec![
                PlayerOp::PlayTarget,
                PlayerOp::Seek,
                PlayerOp::Pause,
                PlayerOp::Play
            ]
        );
        h.engine.shutdown().await.unwrap();
    }

    // 5. Controlled daemon restart (core_restart_count > 0) must NOT re-trigger autoplayOnLaunch
    //    either on page self-restore or saved-track restore.
    {
        let h = engine_with_storage(RecordingStorage::default(), true).await;
        h.engine
            .on_host_hello(ytm_protocol::HostHelloPayload {
                app_version: "1.2.0".to_string(),
                electron_version: "45.4.1".to_string(),
                chromium_version: "136.0".to_string(),
                host_health: ytm_protocol::HostHealth {
                    wayland_native: Some(true),
                    gpu_accelerated: Some(true),
                    sandbox_enabled: true,
                    secret_store_backend: "kwallet".to_string(),
                    safe_storage_available: true,
                    tray_available: true,
                    mpris_active: true,
                    core_restart_count: 1,
                    last_error: Some("unexpected daemon exit".to_string()),
                },
                network_state: NetworkState::Online,
                mock_mode: false,
            })
            .await
            .unwrap();
        sign_in(&h.engine).await;
        init_ready_bridge(&h.engine, "bridge-1", 1).await;
        assert!(
            h.engine
                .on_bridge_queue_observed(
                    2,
                    QueueSnapshot {
                        bridge_epoch: Some(BridgeEpoch::new("bridge-1").unwrap()),
                        queue_revision: 1,
                        current_entry_id: Some(QueueEntryId::new("qe-r").unwrap()),
                        loaded_range: None,
                        completeness: ytm_protocol::QueueCompleteness::Complete,
                        has_more: Some(false),
                        explicit_complete: Some(true),
                        may_load_more: Some(false),
                        official_upcoming: None,
                        filters: None,
                        entries: vec![QueueEntry {
                            entry_id: QueueEntryId::new("qe-r").unwrap(),
                            video_id: VideoId::new("vid-last").unwrap(),
                            track: make_track("vid-last", "Last", Some(200_000)),
                            availability: TrackAvailability::Available,
                        }],
                    },
                )
                .await
                .unwrap()
        );
        observe_restore(&h.engine, 3, PlaybackStatus::Paused, 12_000).await;
        assert!(
            h.bridge.sent_ops().is_empty(),
            "daemon restart should keep paused track paused, got {:?}",
            h.bridge.sent_ops()
        );
        h.engine.shutdown().await.unwrap();
    }

    {
        let storage = RecordingStorage::default();
        *storage.last_session.lock().unwrap() = Some(LastSession {
            account_scope: AccountScope::new("scope-acct-1").unwrap(),
            last_played: Some(LastPlayed {
                track: make_track("vid-last", "Last", Some(200_000)),
                playback_ref: None,
                position_ms: 42_000,
            }),
        });
        let h = engine_with_storage(storage, true).await;
        h.engine
            .on_host_hello(ytm_protocol::HostHelloPayload {
                app_version: "1.2.0".to_string(),
                electron_version: "45.4.1".to_string(),
                chromium_version: "136.0".to_string(),
                host_health: ytm_protocol::HostHealth {
                    wayland_native: Some(true),
                    gpu_accelerated: Some(true),
                    sandbox_enabled: true,
                    secret_store_backend: "kwallet".to_string(),
                    safe_storage_available: true,
                    tray_available: true,
                    mpris_active: true,
                    core_restart_count: 1,
                    last_error: None,
                },
                network_state: NetworkState::Online,
                mock_mode: false,
            })
            .await
            .unwrap();
        sign_in(&h.engine).await;
        init_ready_bridge(&h.engine, "bridge-1", 1).await;
        observe_empty_page(&h.engine, 2).await;
        observe_restore(&h.engine, 3, PlaybackStatus::Playing, 0).await;
        observe_restore(&h.engine, 4, PlaybackStatus::Playing, 42_000).await;
        observe_restore(&h.engine, 5, PlaybackStatus::Paused, 42_000).await;
        assert_eq!(
            h.bridge.sent_ops(),
            vec![PlayerOp::PlayTarget, PlayerOp::Seek, PlayerOp::Pause],
            "daemon restart restore must stay paused without extra Play"
        );
        h.engine.shutdown().await.unwrap();
    }
}
