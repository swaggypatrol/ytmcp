use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tempfile::TempDir;
use tokio::{sync::mpsc, time::timeout};
use ytm_control_api::{
    AccountHandoffGoal, AccountHandoffState, ContextInput, ControlError, ControlRequest,
    ControlResponse, DjChattiness, DjCue, DjCueInput, DjCueKind, DjCueReason, DjFeedbackChange,
    DjFeedbackTarget, DjInteraction, DjPreference, DjPreferenceStance, DjPreferenceSubject,
    DjPreferencesInput, DjTrackOutcome, ErrorCode, GrantAction, HistoryEntry, HistoryInput,
    HistoryMedia, HistorySource, InterruptResumePolicy, InterruptResumeState, ItemPhase,
    LibraryMembershipTarget, LocalHistoryEdit, LocalHistoryRange, LocalHistoryView,
    LyricsOffsetAction, OperationInput, OperationPhase, OperationReceipt, OperationResource, Page,
    PageInput, Playlist, PlaylistPrivacy, Preconditions, Rating, RecommendationPage, RemovedItem,
    RetryDisposition, TrackTarget, WriteAction, WriteInput,
};
use ytm_core::control::catalog::{
    CatalogReadContext, CatalogWriteError, ControlCatalog, project_track,
};
use ytm_core::control::dj_store::DjStore;
use ytm_core::control::journal::OperationStore;
use ytm_core::control::lyrics::ControlLyrics;
use ytm_core::control::{ControlService, DispatchAuthority, DispatchGrant, ReadGrant};
use ytm_core::ports::Clock;
use ytm_core::ports::StoragePort;
use ytm_core::{
    BoxFuture, BridgePort, CancellationToken, CatalogIdentity, CatalogPort, CoreDeps, CoreEngine,
    EngineConfig, MockClock, OfficialLyrics,
};
use ytm_protocol::{
    AccountEpoch, AccountScope, AgentActivityRequest, ArtistId, ArtistRef, BridgeEpoch,
    BridgePlaybackObservedPayload, BridgeReadyPayload, CapabilitySet, CapabilitySupport,
    CatalogQueryRequest, CatalogResponsePayload, CommandId, CredentialRevision, EnqueuePosition,
    Freshness, HomeFeed, LyricsMatchMetadata, LyricsResult, MutationRequest, MutationResult,
    PlaybackStatus, PlayerCommandPayload, PlayerOp, PlaylistId, QueryError, QueueCompleteness,
    QueueEntry, QueueEntryId, QueueRange, QueueSnapshot, SessionMaterialPayload, StateDelta, Track,
    TrackAvailability, TrackInstanceId, UiLanguage, VideoId,
};
use ytm_storage::{
    AccountCacheContext, CoverCacheConfig, RecentPlaybackRecord, SqliteStorage, StoragePaths,
};

const ACCOUNT: &str = "fixture-dj-account";
const EPOCH: &str = "fixture-dj-epoch";
const BRIDGE: &str = "fixture-dj-bridge";
const HINT: &str = "fixture-dj-identity";
const SEED: &str = "fixture-seed";
const FIRST: &str = "fixture-first";
const SECOND: &str = "fixture-second";
const THIRD: &str = "fixture-third";
const FOURTH: &str = "fixture-fourth";
const EXCLUDED: &str = "fixture-excluded";
const WAIT: Duration = Duration::from_secs(3);

fn track(id: &str, duration_ms: Option<u64>) -> Track {
    Track {
        video_id: VideoId::new(id).unwrap(),
        title: id.into(),
        artists: vec![],
        album: None,
        duration_ms,
        cover: None,
        availability: TrackAvailability::Available,
        version_tag: None,
        is_liked: None,
    }
}

struct Catalog;

impl CatalogPort for Catalog {
    fn query(
        &self,
        _epoch: Option<AccountEpoch>,
        _scope: Option<AccountScope>,
        _request: CatalogQueryRequest,
        _cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<CatalogResponsePayload, QueryError>> {
        Box::pin(async { panic!("DJ must use the scoped radio catalog") })
    }

    fn mutate(
        &self,
        _epoch: AccountEpoch,
        _scope: AccountScope,
        _request: MutationRequest,
        _cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<MutationResult, QueryError>> {
        Box::pin(async { panic!("queue refill must use the shared player bridge") })
    }

    fn apply_session_material(
        &self,
        _material: SessionMaterialPayload,
        _cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<CatalogIdentity, QueryError>> {
        Box::pin(async {
            Ok(CatalogIdentity {
                account_scope: AccountScope::new(ACCOUNT).unwrap(),
                account_hint: Some(HINT.into()),
            })
        })
    }

    fn official_lyrics(
        &self,
        _epoch: AccountEpoch,
        _video: VideoId,
        _cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<Option<OfficialLyrics>, QueryError>> {
        Box::pin(async { panic!("DJ should not request lyrics") })
    }

    fn clear_session_material(&self, _epoch: Option<AccountEpoch>) -> Result<(), QueryError> {
        Ok(())
    }
}

/// Ratings the fixture provider accepted; only the daily-limit test rates.
static PROVIDER_RATINGS: AtomicUsize = AtomicUsize::new(0);

impl ControlCatalog for Catalog {
    fn set_rating(
        &self,
        _context: CatalogReadContext,
        _video: VideoId,
        _rating: Rating,
    ) -> BoxFuture<'_, Result<(), CatalogWriteError>> {
        PROVIDER_RATINGS.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }

    fn youtube_history(
        &self,
        context: CatalogReadContext,
        _page: PageInput,
    ) -> BoxFuture<'_, Result<Page<HistoryEntry>, QueryError>> {
        Box::pin(async move {
            assert_eq!(context.account_scope.as_str(), ACCOUNT);
            Ok(Page {
                items: vec![HistoryEntry {
                    media: HistoryMedia::Song {
                        track: Box::new(project_track(&track(SEED, Some(20_000)))),
                    },
                    period: "Today".into(),
                }],
                next_cursor: None,
                complete: true,
                source: "fixture-youtube-history".into(),
                freshness: Freshness::Fresh,
                captured_at_ms: 1_000,
            })
        })
    }

    fn fresh_playlist(
        &self,
        _context: CatalogReadContext,
        _playlist: PlaylistId,
        _page: PageInput,
    ) -> BoxFuture<'_, Result<Playlist, QueryError>> {
        Box::pin(async { panic!("a queue session should not read playlists") })
    }

    fn recommend_song_radio(
        &self,
        context: CatalogReadContext,
        seed: VideoId,
        page: PageInput,
    ) -> BoxFuture<'_, Result<RecommendationPage, QueryError>> {
        Box::pin(async move {
            assert_eq!(context.account_scope.as_str(), ACCOUNT);
            assert_eq!(context.account_epoch.as_str(), EPOCH);
            assert!(page.cursor.is_none());
            let durations = [
                (seed.as_str(), Some(20_000)),
                (EXCLUDED, Some(20_000)),
                ("fixture-too-long", Some(90_000)),
                ("fixture-unknown-duration", None),
                (FIRST, Some(20_000)),
                (FIRST, Some(30_000)),
                (SECOND, Some(25_000)),
                (THIRD, Some(30_000)),
                (FOURTH, Some(35_000)),
            ];
            let items = durations
                .into_iter()
                .map(|(id, duration)| project_track(&track(id, duration)))
                .collect();
            Ok(RecommendationPage {
                seed,
                candidates: Page {
                    items,
                    next_cursor: None,
                    complete: true,
                    source: "fixture-song-radio".into(),
                    freshness: Freshness::Fresh,
                    captured_at_ms: 1_000,
                },
            })
        })
    }
}

struct Bridge(mpsc::Sender<PlayerCommandPayload>);

impl BridgePort for Bridge {
    fn send_player_command(
        &self,
        epoch: BridgeEpoch,
        command: PlayerCommandPayload,
    ) -> BoxFuture<'_, Result<(), QueryError>> {
        Box::pin(async move {
            assert_eq!(epoch.as_str(), BRIDGE);
            self.0.send(command).await.unwrap();
            Ok(())
        })
    }

    fn request_snapshot(&self, _epoch: BridgeEpoch) -> BoxFuture<'_, Result<(), QueryError>> {
        Box::pin(async { Ok(()) })
    }
}

struct Grant {
    queue: bool,
}

struct HistoryGrant;

impl DispatchAuthority for HistoryGrant {
    fn authorize(&self, action: GrantAction) -> BoxFuture<'_, Result<DispatchGrant, ControlError>> {
        Box::pin(async move {
            assert_eq!(action, GrantAction::LocalHistory);
            Ok(DispatchGrant {
                deadline_wall_ms: u64::MAX,
                editable_playlists: vec![],
            })
        })
    }
}

fn history_request(source: HistorySource, cursor: Option<String>, limit: u16) -> ControlRequest {
    ControlRequest::GetHistory(HistoryInput {
        source: Some(source),
        page: Some(PageInput {
            cursor,
            limit: Some(limit),
        }),
        limit: None,
        local_cursor: None,
        youtube_cursor: None,
        range: None,
    })
}

async fn local_history(
    fixture: &Harness,
    cursor: Option<String>,
    limit: u16,
) -> Box<LocalHistoryView> {
    let response = fixture
        .service
        .execute(
            &fixture.read,
            history_request(HistorySource::Local, cursor, limit),
        )
        .await;
    let ControlResponse::LocalHistory(history) = response else {
        panic!("expected local history: {response:?}");
    };
    history
}

#[tokio::test]
async fn local_history_preserves_occurrences_and_edits_only_the_observed_revision() {
    let fixture = Harness::new().await;
    let storage = open_storage(&fixture.root, fixture.clock.clone());
    let scope = AccountScope::new(ACCOUNT).unwrap();
    let epoch = AccountEpoch::new(EPOCH).unwrap();
    let other_scope = AccountScope::new("other-fixture-account").unwrap();
    let other_epoch = AccountEpoch::new("other-fixture-epoch").unwrap();
    let record = |time, id| RecentPlaybackRecord {
        track: track(id, Some(40_000)),
        playback_ref: None,
        position_ms: 1_000,
        duration_ms: Some(40_000),
        recorded_at_ms: time,
        source_context: None,
    };
    for (time, id) in [(10, FIRST), (20, SEED), (30, SEED)] {
        storage
            .record_recent_playback(&scope, &epoch, record(time, id))
            .await
            .unwrap();
    }
    storage
        .record_recent_playback(&other_scope, &other_epoch, record(20, SEED))
        .await
        .unwrap();
    let cached_home = HomeFeed {
        personalized_supported: false,
        fallback_notice: None,
        recent_tracks: vec![track(SEED, Some(40_000))],
        pinned_playlists: vec![],
        freshness: Freshness::Fresh,
    };
    storage
        .put_home_feed(
            &AccountCacheContext {
                account_scope: scope.clone(),
                account_epoch: epoch.clone(),
                locale: "fixture".into(),
                provider_version: "fixture".into(),
            },
            &cached_home,
            None,
        )
        .await
        .unwrap();

    let first = local_history(&fixture, None, 1).await;
    assert_eq!(first.retention_limit, 200);
    let second = local_history(&fixture, first.entries.next_cursor.clone(), 1).await;
    let newest = &first.entries.items[0];
    let duplicate = &second.entries.items[0];
    assert_eq!(newest.track.video_id, duplicate.track.video_id);
    assert_ne!(newest.entry_id, duplicate.entry_id);
    assert_eq!((newest.recorded_at_ms, duplicate.recorded_at_ms), (30, 20));

    let combined = fixture
        .service
        .execute(
            &fixture.read,
            history_request(HistorySource::Combined, None, 3),
        )
        .await;
    let ControlResponse::CombinedHistory(combined) = combined else {
        panic!("expected two sourced histories");
    };
    assert_eq!(combined.local.entries.items.len(), 3);
    assert_eq!(combined.youtube.unwrap().items.len(), 1);
    let outside_local_source = fixture
        .service
        .execute(
            &fixture.read,
            ControlRequest::GetHistory(HistoryInput {
                source: Some(HistorySource::Combined),
                page: None,
                limit: None,
                local_cursor: None,
                youtube_cursor: None,
                range: Some(LocalHistoryRange {
                    from_ms: Some(20),
                    until_ms: Some(30),
                }),
            }),
        )
        .await;
    assert!(
        matches!(outside_local_source, ControlResponse::Error(error) if error.code == ErrorCode::InvalidInput)
    );
    let foreign = fixture
        .service
        .execute(
            &ReadGrant::new("fixture-dj-client".into(), Some(other_scope.clone())),
            history_request(HistorySource::Local, None, 3),
        )
        .await;
    assert!(
        matches!(foreign, ControlResponse::Error(error) if error.code == ErrorCode::PermissionDenied)
    );

    let args = |id: &str, revision: String, action: LocalHistoryEdit| WriteInput {
        operation_id: id.to_owned(),
        preconditions: Preconditions {
            expected_account_epoch: Some(epoch.clone()),
            expected_bridge_epoch: None,
            expected_queue_revision: None,
            expected_entity_revision: Some(revision),
        },
        action: WriteAction::EditLocalHistory { edit: action },
    };
    let remove = LocalHistoryEdit::Remove {
        entry_ids: vec![duplicate.entry_id.clone()],
    };
    let unsupported = fixture
        .service
        .submit(
            fixture.read.clone(),
            fixture.write.clone(),
            args("no-history-grant", first.revision.clone(), remove.clone()),
        )
        .await
        .unwrap_err();
    assert_eq!(unsupported.code, ErrorCode::PermissionDenied);
    storage
        .record_recent_playback(&scope, &epoch, record(40, SECOND))
        .await
        .unwrap();
    let old_page = fixture
        .service
        .execute(
            &fixture.read,
            history_request(HistorySource::Local, first.entries.next_cursor.clone(), 1),
        )
        .await;
    assert!(
        matches!(old_page, ControlResponse::Error(error) if error.code == ErrorCode::StaleContext)
    );
    let grant = Arc::new(HistoryGrant);
    fixture
        .service
        .submit(
            fixture.read.clone(),
            grant.clone(),
            args("stale-history", first.revision, remove.clone()),
        )
        .await
        .unwrap();
    let stale = fixture.finished("stale-history").await;
    assert_eq!(stale.phase, OperationPhase::Rejected);
    assert_eq!(stale.error.unwrap().code, ErrorCode::StaleContext);
    assert!(stale.removed.is_empty(), "a refused edit removed nothing");
    assert!(
        storage
            .get_home_feed(&scope, "fixture")
            .await
            .unwrap()
            .is_some()
    );

    let fresh = local_history(&fixture, None, 10).await;
    assert_eq!(fresh.entries.items.len(), 4);
    let exact = args("remove-history", fresh.revision, remove);
    fixture
        .service
        .submit(fixture.read.clone(), grant.clone(), exact.clone())
        .await
        .unwrap();
    let removed = fixture.finished("remove-history").await;
    assert_eq!(
        removed.resource,
        Some(OperationResource::LocalHistory {
            removed_count: Some(1)
        })
    );
    assert_eq!(
        removed.item_results[0].resource_entry_id.as_ref(),
        Some(&duplicate.entry_id)
    );
    let ledger = |entry_id: &String, recorded_at_ms| RemovedItem::LocalHistoryEntry {
        entry_id: entry_id.clone(),
        video_id: VideoId::new(SEED).unwrap(),
        recorded_at_ms,
    };
    assert_eq!(removed.removed, [ledger(&duplicate.entry_id, 20)]);
    assert!(
        storage
            .get_home_feed(&scope, "fixture")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        fixture
            .service
            .submit(fixture.read.clone(), grant.clone(), exact)
            .await
            .unwrap(),
        removed
    );

    let current = local_history(&fixture, None, 10).await;
    assert!(
        current
            .entries
            .items
            .iter()
            .any(|item| item.entry_id == newest.entry_id)
    );
    assert_eq!(current.entries.items.len(), 3);
    fixture
        .service
        .submit(
            fixture.read.clone(),
            grant,
            args(
                "clear-history",
                current.revision,
                LocalHistoryEdit::Clear {
                    range: Some(LocalHistoryRange {
                        from_ms: Some(30),
                        until_ms: Some(40),
                    }),
                },
            ),
        )
        .await
        .unwrap();
    let cleared = fixture.finished("clear-history").await;
    assert_eq!(
        cleared.resource,
        Some(OperationResource::LocalHistory {
            removed_count: Some(1)
        })
    );
    assert_eq!(cleared.removed, [ledger(&newest.entry_id, 30)]);
    let remaining = storage.list_recent_playback(&scope, 10).await.unwrap();
    assert_eq!(
        remaining
            .iter()
            .map(|row| row.recorded_at_ms)
            .collect::<Vec<_>>(),
        [40, 10]
    );
    assert_eq!(
        storage
            .list_recent_playback(&other_scope, 10)
            .await
            .unwrap()
            .len(),
        1
    );
    fixture.engine.shutdown().await.unwrap();
}

impl DispatchAuthority for Grant {
    fn authorize(&self, action: GrantAction) -> BoxFuture<'_, Result<DispatchGrant, ControlError>> {
        Box::pin(async move {
            if action != GrantAction::Dj && !(self.queue && action == GrantAction::Queue) {
                return Err(ControlError::new(
                    ErrorCode::PermissionDenied,
                    RetryDisposition::NotRetryable,
                    "fixture grant lacks this action",
                ));
            }
            Ok(DispatchGrant {
                deadline_wall_ms: u64::MAX,
                editable_playlists: vec![],
            })
        })
    }
}

struct Harness {
    root: TempDir,
    clock: Arc<MockClock>,
    engine: CoreEngine,
    service: Arc<ControlService>,
    read: ReadGrant,
    write: Arc<Grant>,
    commands: mpsc::Receiver<PlayerCommandPayload>,
    seq: u64,
    queue: QueueSnapshot,
}

impl Harness {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let clock = Arc::new(MockClock::new(1_000));
        let catalog = Arc::new(Catalog);
        let (sender, commands) = mpsc::channel(8);
        let engine = CoreEngine::start(
            CoreDeps {
                clock: clock.clone(),
                bridge: Arc::new(Bridge(sender)),
                catalog: Some(catalog.clone()),
                ..Default::default()
            },
            EngineConfig::default(),
        );
        assert!(
            engine
                .on_bridge_ready(
                    1,
                    BridgeReadyPayload {
                        bridge_epoch: BridgeEpoch::new(BRIDGE).unwrap(),
                        bridge_version: "0.1.0".into(),
                        capabilities: CapabilitySet {
                            play_pause: CapabilitySupport::Supported,
                            queue_mirror: CapabilitySupport::Supported,
                            queue_enqueue: CapabilitySupport::Supported,
                            ..Default::default()
                        },
                        account_hint: Some(HINT.into()),
                        logged_in_observed: true,
                    }
                )
                .await
                .unwrap()
        );
        engine
            .on_session_material(SessionMaterialPayload {
                account_epoch: AccountEpoch::new(EPOCH).unwrap(),
                credential_revision: CredentialRevision::new("fixture-dj-credentials").unwrap(),
                cookie_pairs: Default::default(),
                auth_user_index: None,
                channel_label: None,
            })
            .await
            .unwrap();
        assert!(
            engine
                .snapshot()
                .await
                .unwrap()
                .auth_state
                .player_catalog_matched
        );
        let storage = open_storage(&root, clock.clone());
        let service = Arc::new(
            ControlService::new(engine.clone())
                .with_catalog(catalog)
                .with_operations(storage.clone())
                .with_local_history(storage.clone())
                .with_dj_store(storage),
        );
        let read = ReadGrant::new(
            "fixture-dj-client".into(),
            Some(AccountScope::new(ACCOUNT).unwrap()),
        );
        let initial_track = track(SEED, Some(20_000));
        let mut harness = Self {
            root,
            clock,
            engine,
            service,
            read,
            write: Arc::new(Grant { queue: true }),
            commands,
            seq: 1,
            queue: QueueSnapshot {
                bridge_epoch: Some(BridgeEpoch::new(BRIDGE).unwrap()),
                queue_revision: 0,
                current_entry_id: Some(QueueEntryId::new("fixture-entry-0").unwrap()),
                loaded_range: None,
                completeness: QueueCompleteness::Complete,
                has_more: Some(false),
                explicit_complete: None,
                may_load_more: None,
                entries: vec![QueueEntry {
                    entry_id: QueueEntryId::new("fixture-entry-0").unwrap(),
                    video_id: initial_track.video_id.clone(),
                    track: initial_track,
                    availability: TrackAvailability::Available,
                }],
            },
        };
        harness.publish_queue().await;
        harness.observe_playback(0, PlaybackStatus::Playing).await;
        harness
    }

    async fn publish_queue(&mut self) {
        self.seq += 1;
        self.queue.queue_revision += 1;
        assert!(
            self.engine
                .on_bridge_queue_observed(self.seq, self.queue.clone())
                .await
                .unwrap()
        );
    }

    async fn observe_playback(&mut self, index: usize, playback: PlaybackStatus) {
        self.observe_at(index, playback, 0).await;
    }

    /// Observes the queue entry at `index` as current, as its own playback instance.
    async fn observe_at(&mut self, index: usize, playback: PlaybackStatus, position_ms: u64) {
        let entry = &self.queue.entries[index];
        self.seq += 1;
        assert!(
            self.engine
                .on_bridge_playback_observed(
                    self.seq,
                    BridgePlaybackObservedPayload {
                        bridge_epoch: BridgeEpoch::new(BRIDGE).unwrap(),
                        playback,
                        track_instance_id: Some(
                            TrackInstanceId::new(format!("fixture-instance-{index}")).unwrap()
                        ),
                        current_track: Some(entry.track.clone()),
                        current_queue_entry_id: Some(entry.entry_id.clone()),
                        playback_context: None,
                        position_ms,
                        duration_ms: entry.track.duration_ms,
                        buffered_ms: None,
                        playback_rate: 1.0,
                        sample_age_ms: 0,
                        volume: 0.5,
                        muted: false,
                        shuffle: Some(false),
                        repeat: None,
                        autoplay: None,
                        codec: None,
                        audio_mode: None,
                        quality_label: None,
                        drm_protected: None,
                    }
                )
                .await
                .unwrap()
        );
    }

    async fn submit(&self, id: &str, action: WriteAction) -> OperationReceipt {
        self.service
            .submit(
                self.read.clone(),
                self.write.clone(),
                WriteInput {
                    operation_id: id.into(),
                    preconditions: Preconditions {
                        expected_account_epoch: Some(AccountEpoch::new(EPOCH).unwrap()),
                        expected_bridge_epoch: None,
                        expected_queue_revision: None,
                        expected_entity_revision: None,
                    },
                    action,
                },
            )
            .await
            .unwrap()
    }

    async fn finished(&self, id: &str) -> OperationReceipt {
        timeout(WAIT, async {
            loop {
                let receipt = operation(&self.service, &self.read, id).await;
                if receipt.scheduling_stopped {
                    return receipt;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("operation did not reach a result")
    }
}

fn open_storage(root: &TempDir, clock: Arc<MockClock>) -> Arc<SqliteStorage> {
    Arc::new(
        SqliteStorage::open(
            StoragePaths::for_root(root.path()),
            clock,
            CoverCacheConfig::production(),
        )
        .unwrap(),
    )
}

async fn operation(service: &ControlService, read: &ReadGrant, id: &str) -> OperationReceipt {
    let result = service
        .execute(
            read,
            ControlRequest::GetOperation(OperationInput {
                operation_id: id.into(),
            }),
        )
        .await;
    let ControlResponse::Operation(receipt) = result else {
        panic!("operation missing: {result:?}")
    };
    receipt
}

fn video_ids(ids: &[&str]) -> Vec<VideoId> {
    ids.iter().map(|id| VideoId::new(*id).unwrap()).collect()
}

struct QueuePlayback;

impl DispatchAuthority for QueuePlayback {
    fn authorize(&self, action: GrantAction) -> BoxFuture<'_, Result<DispatchGrant, ControlError>> {
        Box::pin(async move {
            if !matches!(action, GrantAction::Queue | GrantAction::Playback) {
                return Err(ControlError::new(
                    ErrorCode::PermissionDenied,
                    RetryDisposition::NotRetryable,
                    "fixture grant lacks this action",
                ));
            }
            Ok(DispatchGrant {
                deadline_wall_ms: u64::MAX,
                editable_playlists: vec![],
            })
        })
    }
}

impl Harness {
    async fn queue_context(&self) -> String {
        let ControlResponse::Context(context) = self
            .service
            .execute(
                &self.read,
                ControlRequest::GetContext(ContextInput {
                    preview_limit: Some(0),
                }),
            )
            .await
        else {
            panic!("an observed playback context is required")
        };
        context.context_token
    }

    async fn submit_queue(&self, id: &str, action: WriteAction) -> OperationReceipt {
        self.service
            .submit(
                self.read.clone(),
                Arc::new(QueuePlayback),
                WriteInput {
                    operation_id: id.into(),
                    preconditions: Preconditions {
                        expected_account_epoch: Some(AccountEpoch::new(EPOCH).unwrap()),
                        expected_bridge_epoch: Some(BridgeEpoch::new(BRIDGE).unwrap()),
                        expected_queue_revision: Some(self.queue.queue_revision),
                        expected_entity_revision: None,
                    },
                    action,
                },
            )
            .await
            .unwrap()
    }

    fn add_entry(&mut self, index: usize, video: &str) -> QueueEntryId {
        let entry_id = QueueEntryId::new(format!("fixture-added-{video}-{}", self.seq)).unwrap();
        self.queue.entries.insert(
            index,
            QueueEntry {
                entry_id: entry_id.clone(),
                video_id: VideoId::new(video).unwrap(),
                track: track(video, Some(20_000)),
                availability: TrackAvailability::Available,
            },
        );
        entry_id
    }

    fn position_of(&self, entry: &QueueEntryId) -> usize {
        self.queue
            .entries
            .iter()
            .position(|candidate| &candidate.entry_id == entry)
            .unwrap()
    }

    /// Answers one insert-next command like the page: the track lands after the current entry.
    async fn insert_next(&mut self, expected: &str) -> QueueEntryId {
        let command = timeout(WAIT, self.commands.recv()).await.unwrap().unwrap();
        assert_eq!(command.op, PlayerOp::Enqueue);
        let args = command.args.unwrap();
        assert_eq!(args.enqueue_position, Some(EnqueuePosition::Next));
        assert_eq!(args.queue_revision, Some(self.queue.queue_revision));
        assert_eq!(args.target.unwrap().video_id.as_str(), expected);
        let current = self.queue.current_entry_id.clone().unwrap();
        let index = self.position_of(&current) + 1;
        let entry = self.add_entry(index, expected);
        self.publish_queue().await;
        entry
    }

    async fn select(&mut self, expected: &QueueEntryId) {
        let command = timeout(WAIT, self.commands.recv()).await.unwrap().unwrap();
        assert_eq!(command.op, PlayerOp::SelectQueueEntry);
        assert_eq!(
            command.args.unwrap().queue_entry_id.as_ref(),
            Some(expected)
        );
        self.play_entry(expected).await;
    }

    async fn play_entry(&mut self, entry: &QueueEntryId) {
        self.queue.current_entry_id = Some(entry.clone());
        self.publish_queue().await;
        let index = self.position_of(entry);
        self.observe_at(index, PlaybackStatus::Playing, 0).await;
    }

    async fn receipt_where(
        &self,
        id: &str,
        done: impl Fn(&OperationReceipt) -> bool,
    ) -> OperationReceipt {
        timeout(WAIT, async {
            loop {
                let receipt = operation(&self.service, &self.read, id).await;
                if done(&receipt) {
                    return receipt;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("operation did not reach the expected state")
    }

    fn videos(&self) -> Vec<String> {
        self.queue
            .entries
            .iter()
            .map(|entry| entry.video_id.as_str().to_owned())
            .collect()
    }
}

fn resume_state(receipt: &OperationReceipt) -> InterruptResumeState {
    match &receipt.resource {
        Some(OperationResource::Interruption { resume, .. }) => *resume,
        other => panic!("expected an interruption resource, got {other:?}"),
    }
}

/// Interrupting at 7 s plays the inserted tracks now, keeps their order, and replays the
/// interrupted track from 7 s before the original upcoming entry.
#[tokio::test]
async fn interruption_plays_now_and_resumes_the_interrupted_track_where_it_stopped() {
    let mut fixture = Harness::new().await;
    fixture.add_entry(1, THIRD);
    fixture.publish_queue().await;
    fixture.observe_at(0, PlaybackStatus::Playing, 7_000).await;
    let context_token = fixture.queue_context().await;
    fixture
        .submit_queue(
            "interrupt",
            WriteAction::InterruptWith {
                context_token,
                video_ids: video_ids(&[FIRST, SECOND]),
                resume_policy: InterruptResumePolicy::ResumeInterrupted,
            },
        )
        .await;

    let first = fixture.insert_next(FIRST).await;
    fixture.select(&first).await;
    let replay = fixture.insert_next(SEED).await;
    let second = fixture.insert_next(SECOND).await;
    assert_eq!(fixture.videos(), [SEED, FIRST, SECOND, SEED, THIRD]);
    let waiting = fixture
        .receipt_where("interrupt", |receipt| {
            receipt.phase == OperationPhase::Acknowledged
        })
        .await;
    assert_eq!(
        waiting.resource,
        Some(OperationResource::Interruption {
            policy: InterruptResumePolicy::ResumeInterrupted,
            interrupted_entry_id: QueueEntryId::new("fixture-entry-0").unwrap(),
            interrupted_video_id: VideoId::new(SEED).unwrap(),
            resume_position_ms: Some(7_000),
            resume_entry_id: Some(replay.clone()),
            resume: InterruptResumeState::Pending,
        })
    );
    assert_eq!(
        waiting
            .item_results
            .iter()
            .map(|item| (item.phase, item.resource_entry_id.clone()))
            .collect::<Vec<_>>(),
        [
            (ItemPhase::Verified, Some(first.as_str().to_owned())),
            (ItemPhase::Verified, Some(second.as_str().to_owned())),
        ]
    );

    // The inserted tracks end naturally; the replay occurrence starts and is sought.
    fixture.play_entry(&second).await;
    fixture.play_entry(&replay).await;
    let seek = timeout(WAIT, fixture.commands.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(seek.op, PlayerOp::Seek);
    assert_eq!(seek.args.unwrap().position_ms, Some(7_000));
    let index = fixture.position_of(&replay);
    fixture
        .observe_at(index, PlaybackStatus::Playing, 7_000)
        .await;
    let resumed = fixture.finished("interrupt").await;
    assert_eq!(resumed.phase, OperationPhase::Verified);
    assert_eq!(resume_state(&resumed), InterruptResumeState::Resumed);
    fixture.engine.shutdown().await.unwrap();
}

/// A manual pause while the inserted tracks play ends the resume plan without a later seek.
#[tokio::test]
async fn manual_control_abandons_a_pending_resume() {
    let mut fixture = Harness::new().await;
    fixture.observe_at(0, PlaybackStatus::Playing, 4_000).await;
    let context_token = fixture.queue_context().await;
    fixture
        .submit_queue(
            "interrupt-manual",
            WriteAction::InterruptWith {
                context_token,
                video_ids: video_ids(&[FIRST]),
                resume_policy: InterruptResumePolicy::ResumeInterrupted,
            },
        )
        .await;
    let first = fixture.insert_next(FIRST).await;
    fixture.select(&first).await;
    fixture.insert_next(SEED).await;
    fixture
        .receipt_where("interrupt-manual", |receipt| {
            receipt.phase == OperationPhase::Acknowledged
        })
        .await;
    fixture
        .engine
        .submit_player_command(PlayerCommandPayload {
            command_id: CommandId::new("fixture-ui-pause").unwrap(),
            op: PlayerOp::Pause,
            args: None,
        })
        .await
        .unwrap();
    let abandoned = fixture.finished("interrupt-manual").await;
    assert_eq!(abandoned.phase, OperationPhase::Partial);
    assert!(abandoned.settled);
    assert_eq!(resume_state(&abandoned), InterruptResumeState::Abandoned);
    let manual = timeout(WAIT, fixture.commands.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(manual.op, PlayerOp::Pause);
    assert!(fixture.commands.try_recv().is_err());
    fixture.engine.shutdown().await.unwrap();
}

/// Restoring needs a known explicit end; a radio window is refused before any page action.
#[tokio::test]
async fn resume_policies_refuse_an_open_radio_queue_before_writing() {
    let mut fixture = Harness::new().await;
    fixture.queue.completeness = QueueCompleteness::Partial;
    fixture.queue.has_more = Some(true);
    fixture.queue.explicit_complete = Some(false);
    fixture.publish_queue().await;
    fixture.observe_playback(0, PlaybackStatus::Playing).await;
    let context_token = fixture.queue_context().await;
    fixture
        .submit_queue(
            "interrupt-radio",
            WriteAction::InterruptWith {
                context_token,
                video_ids: video_ids(&[FIRST]),
                resume_policy: InterruptResumePolicy::ContinueOriginalQueue,
            },
        )
        .await;
    let refused = fixture.finished("interrupt-radio").await;
    assert_eq!(refused.phase, OperationPhase::Rejected);
    assert_eq!(refused.error.unwrap().code, ErrorCode::PartialQueue);
    assert!(fixture.commands.try_recv().is_err());
    fixture.engine.shutdown().await.unwrap();
}

/// Replacing upcoming entries clears exactly the entries after the current one, then inserts the
/// new tracks in input order.
#[tokio::test]
async fn replacing_upcoming_entries_clears_then_inserts_in_order() {
    let mut fixture = Harness::new().await;
    fixture.add_entry(1, THIRD);
    fixture.add_entry(2, FOURTH);
    // Page queue edits need the loaded window that the real bridge always reports.
    fixture.queue.loaded_range = Some(QueueRange {
        start: 0,
        end_exclusive: 3,
    });
    fixture.publish_queue().await;
    fixture.observe_playback(0, PlaybackStatus::Playing).await;
    let context_token = fixture.queue_context().await;
    fixture
        .submit_queue(
            "replace",
            WriteAction::ReplaceUpcoming {
                context_token,
                video_ids: video_ids(&[FIRST, SECOND]),
            },
        )
        .await;
    let clear = timeout(WAIT, fixture.commands.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(clear.op, PlayerOp::ClearUpcoming);
    fixture.queue.entries.truncate(1);
    fixture.queue.loaded_range = Some(QueueRange {
        start: 0,
        end_exclusive: 1,
    });
    fixture.publish_queue().await;
    fixture.insert_next(SECOND).await;
    fixture.insert_next(FIRST).await;
    let replaced = fixture.finished("replace").await;
    assert_eq!(replaced.phase, OperationPhase::Verified);
    assert_eq!(fixture.videos(), [SEED, FIRST, SECOND]);
    fixture.engine.shutdown().await.unwrap();
}

/// An entry that arrives between the clear and the first insert stops the replacement: the
/// tracks would otherwise land in front of it and the result would read as replaced.
#[tokio::test]
async fn replacing_upcoming_entries_stops_when_the_cleared_queue_gains_an_entry() {
    let mut fixture = Harness::new().await;
    fixture.add_entry(1, THIRD);
    fixture.queue.loaded_range = Some(QueueRange {
        start: 0,
        end_exclusive: 2,
    });
    fixture.publish_queue().await;
    fixture.observe_playback(0, PlaybackStatus::Playing).await;
    let context_token = fixture.queue_context().await;
    fixture
        .submit_queue(
            "replace-raced",
            WriteAction::ReplaceUpcoming {
                context_token,
                video_ids: video_ids(&[FIRST]),
            },
        )
        .await;
    let clear = timeout(WAIT, fixture.commands.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(clear.op, PlayerOp::ClearUpcoming);
    // The cleared queue and the arrival reach the engine before the operation reads again.
    fixture.queue.entries.truncate(1);
    fixture.queue.loaded_range = Some(QueueRange {
        start: 0,
        end_exclusive: 1,
    });
    fixture.queue.queue_revision += 1;
    let cleared = fixture.queue.clone();
    fixture.add_entry(1, EXCLUDED);
    fixture.queue.loaded_range = Some(QueueRange {
        start: 0,
        end_exclusive: 2,
    });
    fixture.queue.queue_revision += 1;
    let arrived = fixture.queue.clone();
    let (first, second) = tokio::join!(
        fixture
            .engine
            .on_bridge_queue_observed(fixture.seq + 1, cleared),
        fixture
            .engine
            .on_bridge_queue_observed(fixture.seq + 2, arrived),
    );
    assert!(first.unwrap() && second.unwrap());
    fixture.seq += 2;
    let stopped = fixture.finished("replace-raced").await;
    assert_eq!(stopped.phase, OperationPhase::Partial);
    assert!(stopped.settled);
    assert!(
        stopped
            .item_results
            .iter()
            .all(|item| item.phase == ItemPhase::NotStarted)
    );
    assert!(fixture.commands.try_recv().is_err());
    fixture.engine.shutdown().await.unwrap();
}

impl Harness {
    /// Two upcoming entries after the current one, in a fully loaded window.
    async fn with_upcoming(shape: impl FnOnce(&mut QueueSnapshot)) -> Self {
        let mut fixture = Self::new().await;
        fixture.add_entry(1, THIRD);
        fixture.add_entry(2, FOURTH);
        fixture.queue.loaded_range = Some(QueueRange {
            start: 0,
            end_exclusive: 3,
        });
        shape(&mut fixture.queue);
        fixture.publish_queue().await;
        fixture.observe_playback(0, PlaybackStatus::Playing).await;
        fixture
    }

    /// Answers a clear like the page: every loaded entry after the current one is removed.
    async fn clear_loaded(&mut self) {
        let clear = timeout(WAIT, self.commands.recv()).await.unwrap().unwrap();
        assert_eq!(clear.op, PlayerOp::ClearUpcoming);
        self.queue.entries.truncate(1);
        self.queue.loaded_range = Some(QueueRange {
            start: 0,
            end_exclusive: 1,
        });
        self.publish_queue().await;
    }
}

fn continuing(queue: &mut QueueSnapshot) {
    queue.may_load_more = Some(true);
}

/// A finite queue, also one followed by autoplay suggestions, is cleared and verified; one whose
/// page keeps a continuation is cleared as far as it is loaded and reported as partly done.
#[tokio::test]
async fn clearing_upcoming_entries_is_partial_when_more_may_load() {
    let finite: fn(&mut QueueSnapshot) = |_| {};
    let autoplay_tail: fn(&mut QueueSnapshot) = |queue| {
        queue.completeness = QueueCompleteness::Partial;
        queue.has_more = Some(true);
        queue.explicit_complete = Some(true);
    };
    for (case, shape, phase) in [
        ("finite", finite, OperationPhase::Verified),
        ("autoplay tail", autoplay_tail, OperationPhase::Verified),
        (
            "continuation",
            continuing as fn(&mut QueueSnapshot),
            OperationPhase::Partial,
        ),
    ] {
        let mut fixture = Harness::with_upcoming(shape).await;
        let context_token = fixture.queue_context().await;
        fixture
            .submit_queue("clear", WriteAction::ClearUpcoming { context_token })
            .await;
        fixture.clear_loaded().await;
        let cleared = fixture.finished("clear").await;
        assert_eq!(cleared.phase, phase, "{case}");
        assert_eq!(fixture.videos(), [SEED]);
        if phase == OperationPhase::Partial {
            assert!(cleared.settled);
            let error = cleared.error.unwrap();
            assert_eq!(error.code, ErrorCode::PartialQueue);
            assert!(
                error.message.contains("more may still follow"),
                "{}",
                error.message
            );
        }
        fixture.engine.shutdown().await.unwrap();
    }
}

/// A finite queue with more upcoming entries than one clear removes loses the nearest 50 and
/// the result says more still follow.
#[tokio::test]
async fn clearing_more_than_fifty_upcoming_entries_removes_the_nearest_and_ends_partial() {
    let mut fixture = Harness::new().await;
    for index in 1..=51 {
        fixture.add_entry(index, &format!("fixture-long-{index}"));
    }
    fixture.queue.loaded_range = Some(QueueRange {
        start: 0,
        end_exclusive: 52,
    });
    fixture.publish_queue().await;
    fixture.observe_playback(0, PlaybackStatus::Playing).await;
    let kept = fixture.queue.entries[51].entry_id.clone();
    let context_token = fixture.queue_context().await;
    fixture
        .submit_queue("clear-long", WriteAction::ClearUpcoming { context_token })
        .await;
    let clear = timeout(WAIT, fixture.commands.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(clear.op, PlayerOp::ClearUpcoming);
    fixture.queue.entries.drain(1..51);
    fixture.queue.loaded_range = Some(QueueRange {
        start: 0,
        end_exclusive: 2,
    });
    fixture.publish_queue().await;
    let cleared = fixture.finished("clear-long").await;
    assert_eq!(cleared.phase, OperationPhase::Partial);
    assert_eq!(cleared.error.unwrap().code, ErrorCode::PartialQueue);
    assert_eq!(fixture.queue.entries[1].entry_id, kept);
    fixture.engine.shutdown().await.unwrap();
}

/// On a queue that may continue, the replacement still inserts every track in order after the
/// best-effort clear, and the whole result is partial.
#[tokio::test]
async fn replacing_upcoming_entries_on_a_continuing_queue_inserts_and_ends_partial() {
    let mut fixture = Harness::with_upcoming(continuing).await;
    let context_token = fixture.queue_context().await;
    fixture
        .submit_queue(
            "replace-open",
            WriteAction::ReplaceUpcoming {
                context_token,
                video_ids: video_ids(&[FIRST, SECOND]),
            },
        )
        .await;
    fixture.clear_loaded().await;
    fixture.insert_next(SECOND).await;
    fixture.insert_next(FIRST).await;
    let replaced = fixture.finished("replace-open").await;
    assert_eq!(replaced.phase, OperationPhase::Partial);
    assert!(replaced.settled);
    assert_eq!(replaced.error.unwrap().code, ErrorCode::PartialQueue);
    assert!(
        replaced
            .item_results
            .iter()
            .all(|item| item.phase == ItemPhase::Verified)
    );
    assert_eq!(fixture.videos(), [SEED, FIRST, SECOND]);
    fixture.engine.shutdown().await.unwrap();
}

/// Holds the first playback authorization until the test releases it, so playback can change
/// between an operation's checks and its dispatch.
#[derive(Default)]
struct HeldPlayback {
    held: std::sync::atomic::AtomicBool,
    reached: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

impl DispatchAuthority for HeldPlayback {
    fn authorize(&self, action: GrantAction) -> BoxFuture<'_, Result<DispatchGrant, ControlError>> {
        Box::pin(async move {
            if action == GrantAction::Playback
                && !self.held.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                self.reached.notify_one();
                self.release.notified().await;
            }
            QueuePlayback.authorize(action).await
        })
    }
}

/// Staying on an interruption is fenced to the occurrence it was planned against: when that
/// track ends on its own first, the new context is not started over the next one.
#[tokio::test]
async fn staying_on_an_interruption_leaves_a_track_that_changed_meanwhile() {
    let mut fixture = Harness::new().await;
    fixture.add_entry(1, THIRD);
    fixture.publish_queue().await;
    fixture.observe_at(0, PlaybackStatus::Playing, 4_000).await;
    let context_token = fixture.queue_context().await;
    let gate = Arc::new(HeldPlayback::default());
    fixture
        .service
        .submit(
            fixture.read.clone(),
            gate.clone(),
            WriteInput {
                operation_id: "stay".into(),
                preconditions: Preconditions {
                    expected_account_epoch: Some(AccountEpoch::new(EPOCH).unwrap()),
                    expected_bridge_epoch: Some(BridgeEpoch::new(BRIDGE).unwrap()),
                    expected_queue_revision: Some(fixture.queue.queue_revision),
                    expected_entity_revision: None,
                },
                action: WriteAction::InterruptWith {
                    context_token,
                    video_ids: video_ids(&[FIRST]),
                    resume_policy: InterruptResumePolicy::StayOnInsertedContext,
                },
            },
        )
        .await
        .unwrap();
    timeout(WAIT, gate.reached.notified()).await.unwrap();
    fixture.play_from(1, 0).await;
    gate.release.notify_one();
    let refused = fixture.finished("stay").await;
    assert_eq!(refused.phase, OperationPhase::Rejected);
    assert!(refused.settled);
    assert!(fixture.commands.try_recv().is_err());
    fixture.engine.shutdown().await.unwrap();
}

/// With nothing changed meanwhile, the fenced new context is admitted and verified.
#[tokio::test]
async fn staying_on_an_interruption_plays_the_new_context() {
    let mut fixture = Harness::new().await;
    fixture.observe_at(0, PlaybackStatus::Playing, 4_000).await;
    let context_token = fixture.queue_context().await;
    fixture
        .submit_queue(
            "stay-plays",
            WriteAction::InterruptWith {
                context_token,
                video_ids: video_ids(&[FIRST]),
                resume_policy: InterruptResumePolicy::StayOnInsertedContext,
            },
        )
        .await;
    let play = timeout(WAIT, fixture.commands.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(play.op, PlayerOp::PlayTarget);
    let entry = fixture.add_entry(1, FIRST);
    fixture.play_entry(&entry).await;
    let played = fixture.finished("stay-plays").await;
    assert_eq!(played.phase, OperationPhase::Verified);
    fixture.engine.shutdown().await.unwrap();
}

/// A session-scoped avoid removes the song from later refill candidates and is shown to the
/// selector; a persistent entry without the preferences grant is refused.
#[tokio::test]
async fn dj_preferences_need_only_the_dj_grant_and_reach_the_next_prompt() {
    let mut fixture = Harness::new().await;
    fixture.write = Arc::new(Grant { queue: false });
    fixture
        .submit(
            "feedback-avoid",
            WriteAction::SetDjFeedback {
                change: DjFeedbackChange::Record {
                    target: DjFeedbackTarget::Video {
                        video_id: VideoId::new(SECOND).unwrap(),
                    },
                    stance: DjPreferenceStance::Avoid,
                    note: Some("  too loud tonight  ".into()),
                },
            },
        )
        .await;
    assert_eq!(
        fixture.finished("feedback-avoid").await.phase,
        OperationPhase::Verified
    );
    let preferences = |fixture: &Harness| {
        let service = fixture.service.clone();
        let read = fixture.read.clone();
        async move {
            let ControlResponse::DjPreferences(page) = service
                .execute(
                    &read,
                    ControlRequest::GetDjPreferences(DjPreferencesInput { page: None }),
                )
                .await
            else {
                panic!("the preferences should be readable")
            };
            page.items
        }
    };
    let recorded = preferences(&fixture).await;
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].note.as_deref(), Some("too loud tonight"));
    assert_eq!(recorded[0].recorded_by, "fixture-dj-client");

    fixture.long_queue(&[FIRST, SECOND, THIRD, FOURTH]).await;
    fixture.clock.advance(200_000);
    fixture
        .observe_at(0, PlaybackStatus::Playing, 200_000)
        .await;
    let cue = fixture.cue(DjCueInput::default()).await.unwrap();
    assert_eq!(cue.preferences, recorded);
    assert!(!cue.preferences_unchanged);

    // The next prompt leaves the unchanged tastes out.
    let cue = fixture.next_ending(1).await;
    assert_eq!(cue.kind, DjCueKind::TrackEnding);
    assert!(cue.preferences.is_empty() && cue.preferences_unchanged);

    fixture
        .submit(
            "feedback-clear",
            WriteAction::SetDjFeedback {
                change: DjFeedbackChange::Clear {
                    preference_id: recorded[0].preference_id.clone(),
                },
            },
        )
        .await;
    assert_eq!(
        fixture.finished("feedback-clear").await.phase,
        OperationPhase::Verified
    );
    assert!(preferences(&fixture).await.is_empty());
    // After the change the prompt carries the (now empty) set again.
    let cue = fixture.next_ending(2).await;
    assert!(cue.preferences.is_empty() && !cue.preferences_unchanged);
    fixture.engine.shutdown().await.unwrap();
}

/// A preference is named from a track the player already knows (current track, queue, local
/// history); an unknown subject stays unnamed. The desktop activity shows the newest entries
/// and reports how many exist.
#[tokio::test]
async fn dj_preferences_are_named_from_known_tracks_and_activity_counts_all_of_them() {
    let mut fixture = Harness::new().await;
    fixture.write = Arc::new(Grant { queue: false });
    let storage = open_storage(&fixture.root, fixture.clock.clone());
    let scope = AccountScope::new(ACCOUNT).unwrap();
    let epoch = AccountEpoch::new(EPOCH).unwrap();
    let artist = ArtistId::new("fixture-artist").unwrap();
    let mut current = track(SEED, Some(20_000));
    current.title = "Synthetic Seed Song".into();
    current.artists = vec![ArtistRef {
        id: Some(artist.clone()),
        name: "Synthetic Artist".into(),
    }];
    fixture.queue.entries[0].track = current;
    let mut queued = track(SECOND, Some(20_000));
    queued.title = "Synthetic Queued Song".into();
    fixture.queue.entries.push(QueueEntry {
        entry_id: QueueEntryId::new("fixture-entry-1").unwrap(),
        video_id: queued.video_id.clone(),
        track: queued,
        availability: TrackAvailability::Available,
    });
    fixture.publish_queue().await;
    fixture.observe_playback(0, PlaybackStatus::Playing).await;
    let mut remembered = track(FIRST, Some(20_000));
    remembered.title = "Synthetic History Song".into();
    storage
        .record_recent_playback(
            &scope,
            &epoch,
            RecentPlaybackRecord {
                track: remembered,
                playback_ref: None,
                position_ms: 1_000,
                duration_ms: Some(20_000),
                recorded_at_ms: 10,
                source_context: None,
            },
        )
        .await
        .unwrap();

    let video = |id: &str| DjFeedbackTarget::Video {
        video_id: VideoId::new(id).unwrap(),
    };
    for (id, target) in [
        ("label-current", video(SEED)),
        ("label-queued", video(SECOND)),
        ("label-history", video(FIRST)),
        ("label-unknown", video(EXCLUDED)),
        (
            "label-artist",
            DjFeedbackTarget::Artist {
                artist_id: artist.clone(),
            },
        ),
        (
            "label-unknown-artist",
            DjFeedbackTarget::Artist {
                artist_id: ArtistId::new("fixture-other-artist").unwrap(),
            },
        ),
    ] {
        fixture
            .submit(
                id,
                WriteAction::SetDjFeedback {
                    change: DjFeedbackChange::Record {
                        target,
                        stance: DjPreferenceStance::Prefer,
                        note: None,
                    },
                },
            )
            .await;
        assert_eq!(fixture.finished(id).await.phase, OperationPhase::Verified);
    }
    let activity = fixture
        .service
        .desktop_activity(Some(epoch.clone()), AgentActivityRequest::Read)
        .await
        .unwrap();
    let mut labels: Vec<_> = activity
        .preferences
        .iter()
        .map(|entry| (entry.subject_id.clone(), entry.label.clone()))
        .collect();
    labels.sort();
    let named = |id: &str, label: Option<&str>| (id.to_owned(), label.map(str::to_owned));
    assert_eq!(
        labels,
        [
            named("fixture-artist", Some("Synthetic Artist")),
            named(EXCLUDED, None),
            named(FIRST, Some("Synthetic History Song")),
            named("fixture-other-artist", None),
            named(SECOND, Some("Synthetic Queued Song")),
            named(SEED, Some("Synthetic Seed Song")),
        ]
    );
    assert_eq!(activity.preferences_total, 6);

    for index in 0..50 {
        storage
            .put_preference(
                scope.clone(),
                DjPreference {
                    preference_id: format!("fixture-bulk-{index}"),
                    subject: DjPreferenceSubject::Video {
                        video_id: VideoId::new(format!("fixture-bulk-video-{index}")).unwrap(),
                        title: None,
                    },
                    stance: DjPreferenceStance::Avoid,
                    note: None,
                    recorded_by: "fixture-dj-client".into(),
                    recorded_at_ms: u64::MAX / 2,
                },
                500,
            )
            .await
            .unwrap();
    }
    let activity = fixture
        .service
        .desktop_activity(Some(epoch), AgentActivityRequest::Read)
        .await
        .unwrap();
    assert_eq!(activity.preferences.len(), 50);
    assert_eq!(activity.preferences_total, 56);
    fixture.engine.shutdown().await.unwrap();
}

/// Account handoffs and their cancellation, as a client with only that grant would hold them.
struct HandoffGrant;

impl DispatchAuthority for HandoffGrant {
    fn authorize(&self, action: GrantAction) -> BoxFuture<'_, Result<DispatchGrant, ControlError>> {
        Box::pin(async move {
            if action != GrantAction::AccountHandoff {
                return Err(ControlError::new(
                    ErrorCode::PermissionDenied,
                    RetryDisposition::NotRetryable,
                    "fixture grant lacks this action",
                ));
            }
            Ok(DispatchGrant {
                deadline_wall_ms: u64::MAX,
                editable_playlists: vec![],
            })
        })
    }
}

/// A signed-in account needs no renewal; a switch waits for the user without any page action
/// and can be cancelled.
#[tokio::test]
async fn account_handoff_waits_for_the_user_without_touching_the_page() {
    let mut fixture = Harness::new().await;
    let submit = |id: &str, action: WriteAction| {
        fixture.service.submit(
            fixture.read.clone(),
            Arc::new(HandoffGrant),
            WriteInput {
                operation_id: id.into(),
                preconditions: Preconditions {
                    expected_account_epoch: Some(AccountEpoch::new(EPOCH).unwrap()),
                    expected_bridge_epoch: None,
                    expected_queue_revision: None,
                    expected_entity_revision: None,
                },
                action,
            },
        )
    };
    submit(
        "renew",
        WriteAction::AccountHandoff {
            goal: AccountHandoffGoal::Reauthenticate,
        },
    )
    .await
    .unwrap();
    let renew = fixture.finished("renew").await;
    assert_eq!(renew.phase, OperationPhase::Verified);
    assert_eq!(
        renew.resource,
        Some(OperationResource::AccountHandoff {
            goal: AccountHandoffGoal::Reauthenticate,
            state: AccountHandoffState::NotNeeded,
        })
    );

    submit(
        "switch",
        WriteAction::AccountHandoff {
            goal: AccountHandoffGoal::SwitchAccount,
        },
    )
    .await
    .unwrap();
    let waiting = fixture
        .receipt_where("switch", |receipt| {
            receipt.phase == OperationPhase::Acknowledged
        })
        .await;
    assert_eq!(
        waiting.resource,
        Some(OperationResource::AccountHandoff {
            goal: AccountHandoffGoal::SwitchAccount,
            state: AccountHandoffState::AwaitingUser,
        })
    );
    submit(
        "switch-cancel",
        WriteAction::Cancel {
            target_operation_id: "switch".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        fixture.finished("switch").await.phase,
        OperationPhase::Cancelled
    );
    assert!(fixture.commands.try_recv().is_err());
    fixture.engine.shutdown().await.unwrap();
}

/// An append whose queue end is not loaded is refused at admission, so it settles as rejected
/// rather than as a write that might have reached the page.
#[tokio::test]
async fn an_unprovable_append_is_rejected_not_unknown() {
    let mut fixture = Harness::new().await;
    fixture.queue.completeness = QueueCompleteness::Partial;
    fixture.queue.has_more = Some(true);
    fixture.queue.explicit_complete = Some(false);
    fixture.publish_queue().await;
    fixture.observe_playback(0, PlaybackStatus::Playing).await;
    let context_token = fixture.queue_context().await;
    fixture
        .submit_queue(
            "append-open-end",
            WriteAction::Enqueue {
                context_token,
                mode: ytm_control_api::QueueMode::Last,
                video_ids: video_ids(&[FIRST]),
            },
        )
        .await;
    let refused = fixture.finished("append-open-end").await;
    assert_eq!(refused.phase, OperationPhase::Rejected);
    assert!(refused.settled);
    assert!(fixture.commands.try_recv().is_err());
    fixture.engine.shutdown().await.unwrap();
}

/// A second handoff from the same client while one waits is refused, so waiting requests cannot
/// pile up, and the waiting one does not hold a write slot.
#[tokio::test]
async fn one_account_handoff_waits_per_client() {
    let fixture = Harness::new().await;
    let submit = |id: &str| {
        fixture.service.submit(
            fixture.read.clone(),
            Arc::new(HandoffGrant),
            WriteInput {
                operation_id: id.into(),
                preconditions: Preconditions {
                    expected_account_epoch: Some(AccountEpoch::new(EPOCH).unwrap()),
                    expected_bridge_epoch: None,
                    expected_queue_revision: None,
                    expected_entity_revision: None,
                },
                action: WriteAction::AccountHandoff {
                    goal: AccountHandoffGoal::SwitchAccount,
                },
            },
        )
    };
    submit("switch-first").await.unwrap();
    fixture
        .receipt_where("switch-first", |receipt| {
            receipt.phase == OperationPhase::Acknowledged
        })
        .await;
    submit("switch-second").await.unwrap();
    let second = fixture.finished("switch-second").await;
    assert_eq!(second.phase, OperationPhase::Rejected);
    assert_eq!(second.error.unwrap().code, ErrorCode::Conflict);
    fixture.engine.shutdown().await.unwrap();
}

/// After its runner leaves, a session no longer blocks a new one, and a pause based on a
/// revision from before the last resume is refused.
#[tokio::test]
async fn a_bridge_refusal_before_any_page_action_is_rejected() {
    let mut fixture = Harness::new().await;
    fixture
        .submit_queue("autoplay-refused", WriteAction::Autoplay { enabled: false })
        .await;
    let command = timeout(WAIT, fixture.commands.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(command.op, PlayerOp::SetAutoplay);
    fixture.seq += 1;
    fixture
        .engine
        .on_bridge_command_ack(
            fixture.seq,
            ytm_protocol::BridgeCommandAckPayload {
                command_id: command.command_id,
                bridge_epoch: BridgeEpoch::new(BRIDGE).unwrap(),
                dispatched: false,
                error: Some(QueryError {
                    code: ytm_protocol::QueryErrorCode::UnsupportedCapability,
                    retriable: false,
                    retry_after_ms: None,
                    safe_message: "queue autoplay unavailable".into(),
                    request_id: None,
                }),
            },
        )
        .await
        .unwrap();
    let refused = fixture.finished("autoplay-refused").await;
    assert_eq!(refused.phase, OperationPhase::Rejected);
    assert!(refused.settled);
    fixture.engine.shutdown().await.unwrap();
}

/// The official page keeps autoplay on by default, so the queue reads partial with an autoplay
/// tail; a DJ session still refills when the explicit list's end is loaded.
impl Harness {
    async fn cue(&self, input: DjCueInput) -> Option<DjCue> {
        match self
            .service
            .execute(&self.read, ControlRequest::NextDjCue(input))
            .await
        {
            ControlResponse::DjCue(reply) => reply.cue,
            other => panic!("unexpected cue reply: {other:?}"),
        }
    }

    /// Replaces the queue with four-minute tracks and plays the first.
    async fn long_queue(&mut self, ids: &[&str]) {
        self.queue.entries = ids
            .iter()
            .enumerate()
            .map(|(index, id)| QueueEntry {
                entry_id: QueueEntryId::new(format!("fixture-long-{index}")).unwrap(),
                video_id: VideoId::new(*id).unwrap(),
                track: track(id, Some(240_000)),
                availability: TrackAvailability::Available,
            })
            .collect();
        self.queue.current_entry_id = Some(self.queue.entries[0].entry_id.clone());
        self.publish_queue().await;
        self.observe_at(0, PlaybackStatus::Playing, 0).await;
    }

    /// Plays a four-minute queue entry from the start, waiting along the way, until its
    /// track-ending prompt arrives.
    async fn next_ending(&mut self, index: usize) -> DjCue {
        self.clock.advance(1_000);
        self.play_from(index, 0).await;
        for position in [100_000, 200_000] {
            self.clock.advance(100_000);
            self.observe_at(index, PlaybackStatus::Playing, position)
                .await;
            if let Some(cue) = self.cue(DjCueInput::default()).await {
                return cue;
            }
        }
        panic!("expected the track-ending prompt")
    }

    async fn play_from(&mut self, index: usize, position_ms: u64) {
        self.queue.current_entry_id = Some(self.queue.entries[index].entry_id.clone());
        self.publish_queue().await;
        self.observe_at(index, PlaybackStatus::Playing, position_ms)
            .await;
    }
}

/// The agent gets one prompt shortly before each track ends, with the context as of delivery,
/// and the next prompt reports how the previous tracks ended.
#[tokio::test]
async fn a_waiting_agent_is_prompted_once_before_each_track_ends() {
    let mut fixture = Harness::new().await;
    fixture.long_queue(&[FIRST, SECOND, THIRD, FOURTH]).await;
    // Check-ins are off here; their timing is random and tested on its own.
    let quiet = DjCueInput {
        chattiness: Some(DjChattiness::Off),
        ..Default::default()
    };
    assert!(fixture.cue(quiet).await.is_none());

    fixture.clock.advance(150_000);
    fixture
        .observe_at(0, PlaybackStatus::Playing, 150_000)
        .await;
    assert!(fixture.cue(DjCueInput::default()).await.is_none());

    // Three tracks are queued, so the prompt comes 45 s before the end.
    fixture.clock.advance(50_000);
    fixture
        .observe_at(0, PlaybackStatus::Playing, 200_000)
        .await;
    let cue = fixture.cue(DjCueInput::default()).await.unwrap();
    assert_eq!(cue.kind, DjCueKind::TrackEnding);
    assert!(
        cue.prompt
            .contains(&format!("\"{FIRST}\" ends in about 40 seconds"))
    );
    assert!(cue.prompt.contains("3 tracks are queued after it"));
    assert!(
        cue.prompt
            .contains("call ytm_wait_dj_prompt again right away")
    );
    assert_eq!(cue.now_playing.as_ref().unwrap().track.title, FIRST);
    assert_eq!(cue.now_playing.unwrap().remaining_ms, Some(40_000));
    assert_eq!(cue.up_next.len(), 3);
    assert!(cue.recent.is_empty());
    // Delivered once to this client; another client still gets it.
    assert!(fixture.cue(DjCueInput::default()).await.is_none());
    let other = ReadGrant::new(
        "fixture-other-agent".into(),
        Some(AccountScope::new(ACCOUNT).unwrap()),
    );
    let ControlResponse::DjCue(reply) = fixture
        .service
        .execute(&other, ControlRequest::NextDjCue(DjCueInput::default()))
        .await
    else {
        panic!("expected a cue reply");
    };
    assert_eq!(reply.cue.unwrap().seq, cue.seq);

    // The first track finishes; the listener skips the second after a minute.
    fixture.clock.advance(39_000);
    fixture
        .observe_at(0, PlaybackStatus::Playing, 239_000)
        .await;
    assert!(fixture.cue(DjCueInput::default()).await.is_none());
    fixture.clock.advance(1_000);
    fixture.play_from(1, 0).await;
    assert!(fixture.cue(DjCueInput::default()).await.is_none());
    fixture.clock.advance(60_000);
    fixture.observe_at(1, PlaybackStatus::Playing, 60_000).await;
    assert!(fixture.cue(DjCueInput::default()).await.is_none());
    fixture
        .engine
        .submit_player_command(PlayerCommandPayload {
            command_id: CommandId::new("fixture-listener-next").unwrap(),
            op: PlayerOp::Next,
            args: None,
        })
        .await
        .unwrap();
    fixture.clock.advance(1_000);
    fixture.play_from(2, 0).await;
    assert!(fixture.cue(DjCueInput::default()).await.is_none());

    // One track left after the third, so its prompt comes 75 s early.
    fixture.clock.advance(170_000);
    fixture
        .observe_at(2, PlaybackStatus::Playing, 170_000)
        .await;
    let cue = fixture.cue(DjCueInput::default()).await.unwrap();
    assert_eq!(cue.kind, DjCueKind::TrackEnding);
    assert_eq!(cue.reasons, vec![DjCueReason::LittleQueued]);
    assert!(cue.prompt.contains("one track is queued after it"));
    let outcomes: Vec<(String, DjTrackOutcome)> = cue
        .recent
        .iter()
        .map(|ended| (ended.track.video_id.as_str().to_owned(), ended.outcome))
        .collect();
    assert_eq!(
        outcomes,
        vec![
            (SECOND.to_owned(), DjTrackOutcome::SkippedByListener),
            (FIRST.to_owned(), DjTrackOutcome::Finished),
        ]
    );
    assert_eq!(
        cue.listener.last_interaction,
        Some(DjInteraction::PlayerControl)
    );
    fixture.engine.shutdown().await.unwrap();
}

/// A check-in comes during long stretches of playback without the listener, at the latest at
/// the cap, never while paused, and the agent's report of a conversation postpones it.
#[tokio::test]
async fn check_ins_follow_the_heartbeat_and_reset_on_conversation() {
    let mut fixture = Harness::new().await;
    let long = (0..6)
        .map(|index| format!("fixture-long-track-{index}"))
        .collect::<Vec<_>>();
    let ids: Vec<&str> = long.iter().map(String::as_str).collect();
    fixture.long_queue(&ids).await;
    // Hour-long tracks keep track-ending prompts out of the way.
    for entry in &mut fixture.queue.entries {
        entry.track.duration_ms = Some(3_600_000);
    }
    fixture.publish_queue().await;
    fixture.observe_at(0, PlaybackStatus::Playing, 0).await;
    assert!(
        fixture
            .cue(DjCueInput {
                chattiness: Some(DjChattiness::Normal),
                ..Default::default()
            })
            .await
            .is_none()
    );

    let mut position = 0;
    let mut first = None;
    for _ in 0..60 {
        fixture.clock.advance(15_000);
        position += 15_000;
        fixture
            .observe_at(0, PlaybackStatus::Playing, position)
            .await;
        if let Some(cue) = fixture.cue(DjCueInput::default()).await {
            first = Some((position, cue));
            break;
        }
    }
    let (at, cue) = first.expect("a check-in within the 15-minute cap");
    assert!((240_000..=900_000).contains(&at), "{at}");
    assert_eq!(cue.kind, DjCueKind::CheckIn);
    assert!(cue.reasons.contains(&DjCueReason::LongTrack));
    assert!(cue.prompt.contains("check in"));
    assert!(
        cue.prompt
            .contains("call ytm_wait_dj_prompt again right away")
    );
    // A check-in counts as unanswered from the moment it fires.
    assert_eq!(cue.listener.unanswered_check_ins, 1);

    // Paused for an hour: nothing accumulates.
    fixture
        .observe_at(0, PlaybackStatus::Paused, position)
        .await;
    for _ in 0..240 {
        fixture.clock.advance(15_000);
        assert!(fixture.cue(DjCueInput::default()).await.is_none());
    }

    // After the listener talks to the agent, the next check-in waits the minimum gap again.
    fixture
        .observe_at(0, PlaybackStatus::Playing, position)
        .await;
    assert!(
        fixture
            .cue(DjCueInput {
                user_spoke: true,
                ..Default::default()
            })
            .await
            .is_none()
    );
    for _ in 0..15 {
        fixture.clock.advance(15_000);
        position += 15_000;
        fixture
            .observe_at(0, PlaybackStatus::Playing, position)
            .await;
        assert!(fixture.cue(DjCueInput::default()).await.is_none());
    }
    let ControlResponse::DjCue(reply) = fixture
        .service
        .execute(
            &fixture.read,
            ControlRequest::NextDjCue(DjCueInput {
                chattiness: Some(DjChattiness::Off),
                ..Default::default()
            }),
        )
        .await
    else {
        panic!("expected a cue reply");
    };
    assert_eq!(reply.chattiness, DjChattiness::Off);
    for _ in 0..120 {
        fixture.clock.advance(15_000);
        position += 15_000;
        fixture
            .observe_at(0, PlaybackStatus::Playing, position)
            .await;
        assert!(fixture.cue(DjCueInput::default()).await.is_none());
    }
    fixture.engine.shutdown().await.unwrap();
}

/// With YouTube Music autoplay on, the suggestions after a fully loaded list do not count as
/// queued music: the last explicit track gets the longer lead and says nothing is queued.
#[tokio::test]
async fn an_autoplay_tail_does_not_count_as_queued_music() {
    let mut fixture = Harness::new().await;
    fixture.long_queue(&[FIRST, SECOND]).await;
    fixture.queue.completeness = QueueCompleteness::Partial;
    fixture.queue.has_more = Some(true);
    fixture.queue.explicit_complete = Some(true);
    fixture.play_from(1, 0).await;
    let quiet = DjCueInput {
        chattiness: Some(DjChattiness::Off),
        ..Default::default()
    };
    assert!(fixture.cue(quiet).await.is_none());
    fixture.clock.advance(170_000);
    fixture
        .observe_at(1, PlaybackStatus::Playing, 170_000)
        .await;
    let cue = fixture.cue(DjCueInput::default()).await.unwrap();
    assert_eq!(cue.kind, DjCueKind::TrackEnding);
    assert_eq!(cue.reasons, vec![DjCueReason::LittleQueued]);
    assert!(
        cue.prompt.contains("nothing is queued after it"),
        "{}",
        cue.prompt
    );
    assert!(cue.up_next.is_empty() && !cue.more_queued);
    fixture.engine.shutdown().await.unwrap();
}

/// A track that played while no agent waited is not reported when the DJ comes back on, since
/// nobody saw how it ended.
#[tokio::test]
async fn tracks_played_while_the_dj_was_off_are_not_reported() {
    let mut fixture = Harness::new().await;
    fixture.long_queue(&[FIRST, SECOND, THIRD, FOURTH]).await;
    let quiet = || DjCueInput {
        chattiness: Some(DjChattiness::Off),
        ..Default::default()
    };
    assert!(fixture.cue(quiet()).await.is_none());
    fixture.clock.advance(30_000);
    fixture.observe_at(0, PlaybackStatus::Playing, 30_000).await;
    assert!(fixture.cue(quiet()).await.is_none());

    // The agent stops waiting; the first track plays out and the second starts.
    fixture.clock.advance(400_000);
    fixture.play_from(1, 0).await;
    fixture.clock.advance(150_000);
    fixture
        .observe_at(1, PlaybackStatus::Playing, 150_000)
        .await;
    assert!(fixture.cue(quiet()).await.is_none());

    fixture.clock.advance(50_000);
    fixture
        .observe_at(1, PlaybackStatus::Playing, 200_000)
        .await;
    let cue = fixture.cue(quiet()).await.unwrap();
    assert_eq!(cue.kind, DjCueKind::TrackEnding);
    assert!(cue.recent.is_empty(), "{:?}", cue.recent);
    fixture.engine.shutdown().await.unwrap();
}

/// Saves lyrics offsets in memory; this suite never reads lyrics.
#[derive(Default)]
struct Offsets(std::sync::Mutex<i64>);

impl ControlLyrics for Offsets {
    fn read_lrclib(
        &self,
        _track: LyricsMatchMetadata,
        _cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<LyricsResult, QueryError>> {
        Box::pin(async { panic!("an offset write does not read lyrics") })
    }

    fn user_offset(&self, _video_id: VideoId) -> BoxFuture<'_, Result<i64, QueryError>> {
        Box::pin(async { Ok(*self.0.lock().unwrap()) })
    }

    fn change_offset(
        &self,
        _video_id: VideoId,
        action: LyricsOffsetAction,
    ) -> BoxFuture<'_, Result<i64, QueryError>> {
        Box::pin(async move {
            let mut offset = self.0.lock().unwrap();
            *offset = match action {
                LyricsOffsetAction::Set { offset_ms } => offset_ms,
                LyricsOffsetAction::Reset => 0,
            };
            Ok(*offset)
        })
    }
}

struct PreferencesGrant;

impl DispatchAuthority for PreferencesGrant {
    fn authorize(&self, action: GrantAction) -> BoxFuture<'_, Result<DispatchGrant, ControlError>> {
        Box::pin(async move {
            assert_eq!(action, GrantAction::Preferences);
            Ok(DispatchGrant {
                deadline_wall_ms: u64::MAX,
                editable_playlists: vec![],
            })
        })
    }
}

/// The desktop lyrics view refreshes its offset only on a change notice, so a saved control
/// write must publish one for the video.
#[tokio::test]
async fn a_saved_lyrics_offset_notifies_the_desktop() {
    let mut fixture = Harness::new().await;
    fixture.service = Arc::new(
        ControlService::new(fixture.engine.clone())
            .with_operations(open_storage(&fixture.root, fixture.clock.clone()))
            .with_lyrics(Arc::new(Offsets::default())),
    );
    let (_, mut deltas) = fixture.engine.subscribe_snapshot().await.unwrap();
    fixture
        .service
        .submit(
            fixture.read.clone(),
            Arc::new(PreferencesGrant),
            WriteInput {
                operation_id: "lyrics-offset".into(),
                preconditions: Preconditions {
                    expected_account_epoch: Some(AccountEpoch::new(EPOCH).unwrap()),
                    expected_bridge_epoch: None,
                    expected_queue_revision: None,
                    expected_entity_revision: None,
                },
                action: WriteAction::SetLyricsOffset {
                    target: TrackTarget::Video {
                        video_id: VideoId::new(SEED).unwrap(),
                    },
                    offset: LyricsOffsetAction::Set { offset_ms: 250 },
                },
            },
        )
        .await
        .unwrap();
    assert_eq!(
        fixture.finished("lyrics-offset").await.phase,
        OperationPhase::Verified
    );
    // A snapshot round trip orders every earlier engine message before the drain.
    fixture.engine.snapshot().await.unwrap();
    let mut notices = Vec::new();
    while let Ok(delta) = deltas.try_recv() {
        if let StateDelta::ResourceInvalidated {
            resource_kind,
            resource_id,
            ..
        } = delta
        {
            notices.push((resource_kind, resource_id));
        }
    }
    assert_eq!(
        notices,
        [("lyricsOffset".to_owned(), Some(SEED.to_owned()))]
    );
    fixture.engine.shutdown().await.unwrap();
}

/// Grants the listed actions, as a client registered with those tiers would hold them.
struct Actions(&'static [GrantAction]);

impl DispatchAuthority for Actions {
    fn authorize(&self, action: GrantAction) -> BoxFuture<'_, Result<DispatchGrant, ControlError>> {
        Box::pin(async move {
            if !self.0.contains(&action) {
                return Err(ControlError::new(
                    ErrorCode::PermissionDenied,
                    RetryDisposition::NotRetryable,
                    "fixture grant lacks this action",
                ));
            }
            Ok(DispatchGrant {
                deadline_wall_ms: u64::MAX,
                editable_playlists: vec![PlaylistId::new("PL_fixture_edit").unwrap()],
            })
        })
    }
}

const ACCOUNT_TIERS: &[GrantAction] = &[
    GrantAction::Rating,
    GrantAction::LibraryMembership,
    GrantAction::ArtistSubscription,
    GrantAction::CreatePrivatePlaylist,
    GrantAction::AddPlaylistEntries,
    GrantAction::EditPlaylist,
    GrantAction::ChangePlaylistPrivacy,
    GrantAction::DeletePlaylist,
    GrantAction::LocalHistory,
];

fn account_write(id: &str, action: WriteAction) -> WriteInput {
    WriteInput {
        operation_id: id.into(),
        preconditions: Preconditions {
            expected_account_epoch: Some(AccountEpoch::new(EPOCH).unwrap()),
            expected_bridge_epoch: None,
            expected_queue_revision: None,
            expected_entity_revision: None,
        },
        action,
    }
}

fn like(id: &str) -> WriteInput {
    account_write(
        id,
        WriteAction::Rate {
            target: TrackTarget::Video {
                video_id: VideoId::new(FIRST).unwrap(),
            },
            rating: Rating::Liked,
        },
    )
}

#[tokio::test]
async fn a_runaway_record_loop_changes_at_most_the_daily_limit_even_after_a_restart() {
    let fixture = Harness::new().await;
    let authority = Arc::new(Actions(ACCOUNT_TIERS));
    let mut accepted = Vec::new();
    // A burst that does not wait: in-flight writes hold their place in the budget.
    for index in 0..200 {
        let id = format!("burst-{index}");
        match fixture
            .service
            .submit(fixture.read.clone(), authority.clone(), like(&id))
            .await
        {
            Ok(_) => accepted.push(id),
            Err(error) => assert!(
                matches!(error.code, ErrorCode::Busy | ErrorCode::LimitReached),
                "{error:?}"
            ),
        }
    }
    for id in &accepted {
        fixture.finished(id).await;
    }
    let mut refused = None;
    for index in 0..200 {
        let id = format!("loop-{index}");
        match fixture
            .service
            .submit(fixture.read.clone(), authority.clone(), like(&id))
            .await
        {
            Ok(_) => {
                fixture.finished(&id).await;
                accepted.push(id);
            }
            Err(error) => {
                assert_eq!(error.code, ErrorCode::LimitReached, "{error:?}");
                refused.get_or_insert(error);
            }
        }
    }
    assert_eq!(accepted.len(), 100);
    assert_eq!(PROVIDER_RATINGS.load(Ordering::SeqCst), 100);
    let refused = refused.expect("the limit refuses the 101st change");
    assert_eq!(refused.retry_disposition, RetryDisposition::NotRetryable);
    assert!(
        refused
            .retry_after_ms
            .is_some_and(|wait| wait <= 24 * 60 * 60 * 1000)
    );
    assert!(
        refused.message.contains("record tier allows 100"),
        "{refused:?}"
    );
    // A replayed operation answers with its original receipt instead of being refused.
    fixture
        .service
        .submit(fixture.read.clone(), authority.clone(), like(&accepted[0]))
        .await
        .unwrap();

    // The count lives in the journal and covers every client of the account: a fresh service
    // over reopened, recovered storage still refuses another client.
    let storage = open_storage(&fixture.root, fixture.clock.clone());
    storage.recover(fixture.clock.wall_now_ms()).await.unwrap();
    let reopened = ControlService::new(fixture.engine.clone())
        .with_catalog(Arc::new(Catalog))
        .with_operations(storage);
    let other_client = ReadGrant::new(
        "fixture-other-client".into(),
        Some(AccountScope::new(ACCOUNT).unwrap()),
    );
    let error = Arc::new(reopened)
        .submit(other_client, authority, like("after-restart"))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::LimitReached);
    assert_eq!(PROVIDER_RATINGS.load(Ordering::SeqCst), 100);
    fixture.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn one_edit_call_may_change_at_most_ten_items() {
    let fixture = Harness::new().await;
    let removal = WriteAction::RemovePlaylistEntries {
        playlist_id: PlaylistId::new("PL_fixture_edit").unwrap(),
        entry_ids: (0..11).map(|index| format!("entry-{index}")).collect(),
    };
    // The fixture catalog panics on any playlist read, so a refusal here sent nothing.
    let error = fixture
        .service
        .submit(
            fixture.read.clone(),
            Arc::new(Actions(ACCOUNT_TIERS)),
            account_write("eleven-removals", removal),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::LimitReached);
    assert!(error.message.contains("at most 10"), "{error:?}");
    let missing = fixture
        .service
        .execute(
            &fixture.read,
            ControlRequest::GetOperation(OperationInput {
                operation_id: "eleven-removals".into(),
            }),
        )
        .await;
    assert!(matches!(missing, ControlResponse::Error(_)), "{missing:?}");
    fixture.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn the_play_tier_alone_refuses_every_account_write() {
    let fixture = Harness::new().await;
    let play = Arc::new(Actions(&[
        GrantAction::Dj,
        GrantAction::Playback,
        GrantAction::Queue,
    ]));
    let playlist = || PlaylistId::new("PL_fixture_edit").unwrap();
    let video = || VideoId::new(FIRST).unwrap();
    let writes = [
        WriteAction::Rate {
            target: TrackTarget::Video { video_id: video() },
            rating: Rating::Liked,
        },
        WriteAction::SetLibraryMembership {
            target: LibraryMembershipTarget::Song { id: video() },
            saved: true,
        },
        WriteAction::SetArtistSubscription {
            artist_id: ArtistId::new("UC_fixture_artist").unwrap(),
            subscribed: true,
        },
        WriteAction::CreatePrivatePlaylist {
            title: "Fixture".into(),
            description: None,
        },
        WriteAction::AddPlaylistEntries {
            playlist_id: playlist(),
            created_by_operation_id: Some("fixture-create".into()),
            video_ids: vec![video()],
        },
        WriteAction::AddPlaylistEntries {
            playlist_id: playlist(),
            created_by_operation_id: None,
            video_ids: vec![video()],
        },
        WriteAction::RemovePlaylistEntries {
            playlist_id: playlist(),
            entry_ids: vec!["entry-0".into()],
        },
        WriteAction::MovePlaylistEntry {
            playlist_id: playlist(),
            entry_id: "entry-0".into(),
            before_entry_id: None,
        },
        WriteAction::UpdatePlaylist {
            playlist_id: playlist(),
            title: Some("Renamed".into()),
            description: None,
        },
        WriteAction::DeletePlaylist {
            playlist_id: playlist(),
        },
        WriteAction::SetPlaylistPrivacy {
            playlist_id: playlist(),
            privacy: PlaylistPrivacy::Public,
        },
        WriteAction::EditLocalHistory {
            edit: LocalHistoryEdit::Clear { range: None },
        },
    ];
    for (index, action) in writes.into_iter().enumerate() {
        let id = format!("play-only-{index}");
        let error = fixture
            .service
            .submit(
                fixture.read.clone(),
                play.clone(),
                account_write(&id, action.clone()),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::PermissionDenied, "{action:?}");
        let journaled = fixture
            .service
            .execute(
                &fixture.read,
                ControlRequest::GetOperation(OperationInput { operation_id: id }),
            )
            .await;
        assert!(matches!(journaled, ControlResponse::Error(_)), "{action:?}");
    }
    fixture.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn ui_settings_patch_preserves_concurrent_mcp_edit_and_persists_both() {
    let root = tempfile::tempdir().unwrap();
    let clock = Arc::new(MockClock::new(1_000));
    let storage = open_storage(&root, clock.clone());
    let (sender, _commands) = mpsc::channel(8);
    let engine = CoreEngine::start(
        CoreDeps {
            clock: clock.clone(),
            bridge: Arc::new(Bridge(sender)),
            catalog: Some(Arc::new(Catalog)),
            storage: Some(storage.clone()),
            ..Default::default()
        },
        EngineConfig::default(),
    );
    assert!(
        engine
            .on_bridge_ready(
                1,
                BridgeReadyPayload {
                    bridge_epoch: BridgeEpoch::new(BRIDGE).unwrap(),
                    bridge_version: "1.0.0".into(),
                    capabilities: CapabilitySet::default(),
                    account_hint: Some(HINT.into()),
                    logged_in_observed: true,
                }
            )
            .await
            .unwrap()
    );
    engine
        .on_session_material(SessionMaterialPayload {
            account_epoch: AccountEpoch::new(EPOCH).unwrap(),
            credential_revision: CredentialRevision::new("fixture-settings-cred").unwrap(),
            cookie_pairs: Default::default(),
            auth_user_index: None,
            channel_label: None,
        })
        .await
        .unwrap();
    let service = Arc::new(ControlService::new(engine.clone()).with_operations(storage.clone()));
    let read = ReadGrant::new(
        "fixture-agent".to_string(),
        Some(AccountScope::new(ACCOUNT).unwrap()),
    );

    // UI reads S0 before MCP writes A1.
    let s0 = engine.snapshot().await.unwrap().settings;
    assert!(!s0.reduce_transparency);
    assert_eq!(s0.ui_language, UiLanguage::English);

    let ControlResponse::Settings(before) =
        service.execute(&read, ControlRequest::GetSettings).await
    else {
        panic!("expected settings snapshot");
    };

    // MCP changes field A (reduceTransparency = true).
    let receipt = service
        .submit(
            read.clone(),
            Arc::new(PreferencesGrant),
            WriteInput {
                operation_id: "mcp-set-transparency".into(),
                preconditions: Preconditions {
                    expected_account_epoch: Some(AccountEpoch::new(EPOCH).unwrap()),
                    expected_bridge_epoch: None,
                    expected_queue_revision: None,
                    expected_entity_revision: None,
                },
                action: WriteAction::UpdateSettings {
                    expected_settings_revision: before.settings_revision,
                    patch: ytm_control_api::SettingsPatch {
                        theme: None,
                        reduce_motion: None,
                        reduce_transparency: Some(true),
                        ui_scale_percent: None,
                        autoplay_on_launch: None,
                        close_to_tray: None,
                        lrclib_enabled: None,
                        track_notifications: None,
                        ui_language: None,
                    },
                },
            },
        )
        .await
        .unwrap();
    assert_eq!(receipt.operation_id, "mcp-set-transparency");
    timeout(WAIT, async {
        loop {
            if let ControlResponse::Operation(op) = service
                .execute(
                    &read,
                    ControlRequest::GetOperation(OperationInput {
                        operation_id: "mcp-set-transparency".into(),
                    }),
                )
                .await
                && op.settled
            {
                assert_eq!(op.phase, OperationPhase::Verified);
                return;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();

    // UI still held S0 and only changes field B (uiLanguage = zh-CN).
    let after_ui = engine
        .patch_settings(ytm_protocol::AppSettingsPatch {
            ui_language: Some(UiLanguage::SimplifiedChinese),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(after_ui.reduce_transparency);
    assert_eq!(after_ui.ui_language, UiLanguage::SimplifiedChinese);

    let snap = engine.snapshot().await.unwrap().settings;
    assert!(snap.reduce_transparency);
    assert_eq!(snap.ui_language, UiLanguage::SimplifiedChinese);

    let persisted = storage.load_settings().await.unwrap().unwrap();
    assert!(persisted.reduce_transparency);
    assert_eq!(persisted.ui_language, UiLanguage::SimplifiedChinese);

    // Same-field conflict: UI intent wins after MCP edit.
    let override_ui = engine
        .patch_settings(ytm_protocol::AppSettingsPatch {
            reduce_transparency: Some(false),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(!override_ui.reduce_transparency);
    assert_eq!(override_ui.ui_language, UiLanguage::SimplifiedChinese);
    engine.shutdown().await.unwrap();
}
