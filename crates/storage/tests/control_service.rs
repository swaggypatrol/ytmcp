use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tempfile::TempDir;
use tokio::{sync::mpsc, time::timeout};
use ytm_control_api::{
    AccountHandoffGoal, AccountHandoffState, ContextInput, ControlError, ControlRequest,
    ControlResponse, DjChattiness, DjCue, DjCueAck, DjCueInput, DjCueKind, DjCueReason,
    DjFeedbackChange, DjFeedbackTarget, DjInteraction, DjPreference, DjPreferenceStance,
    DjPreferenceSubject, DjPreferencesInput, DjTrackOutcome, ErrorCode, GrantAction, HistoryEntry,
    HistoryInput, HistoryMedia, HistorySource, InterruptResumePolicy, InterruptResumeState,
    ItemPhase, LibraryMembershipTarget, LocalHistoryEdit, LocalHistoryRange, LocalHistoryView,
    LyricsOffsetAction, OperationInput, OperationPhase, OperationReceipt, OperationResource, Page,
    PageInput, Playlist, PlaylistPrivacy, Preconditions, QueueFilterView, Rating,
    RecommendationPage, RemovedItem, RetryDisposition, TrackTarget, WriteAction, WriteInput,
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
    CatalogQueryRequest, CatalogResponsePayload, CommandId, CoreEpoch, CredentialRevision,
    EnqueuePosition, Freshness, HomeFeed, LyricsMatchMetadata, LyricsResult, MutationRequest,
    MutationResult, OfficialUpcoming, OfficialUpcomingStatus, PlaybackStatus, PlayerCommandPayload,
    PlayerOp, PlaylistId, QueryError, QueueCompleteness, QueueEntry, QueueEntryId, QueueFilter,
    QueueRange, QueueSnapshot, SessionMaterialPayload, StateDelta, Track, TrackAvailability,
    TrackInstanceId, UiLanguage, VideoId,
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

/// Ratings of the first fixture song the provider accepted; only the daily-limit test rates it.
static PROVIDER_RATINGS: AtomicUsize = AtomicUsize::new(0);

impl ControlCatalog for Catalog {
    fn set_rating(
        &self,
        _context: CatalogReadContext,
        video: VideoId,
        _rating: Rating,
    ) -> BoxFuture<'_, Result<(), CatalogWriteError>> {
        // Other tests may rate other songs; the daily-limit test rates only this one.
        if video.as_str() == FIRST {
            PROVIDER_RATINGS.fetch_add(1, Ordering::SeqCst);
        }
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

/// A provider of two accounts' libraries for listener profile reads. The account scope follows
/// the signed-in epoch, so a test can switch accounts; liked pages are counted and can fail or
/// never answer.
#[derive(Default)]
struct ProfileCatalog {
    liked_reads: AtomicUsize,
    liked_fails: std::sync::atomic::AtomicBool,
    liked_hangs: std::sync::atomic::AtomicBool,
}

const PROFILE_OTHER_EPOCH: &str = "fixture-profile-other-epoch";
const PROFILE_OTHER_ACCOUNT: &str = "fixture-profile-other-account";

fn profile_scope(epoch: &str) -> AccountScope {
    AccountScope::new(if epoch == PROFILE_OTHER_EPOCH {
        PROFILE_OTHER_ACCOUNT
    } else {
        ACCOUNT
    })
    .unwrap()
}

fn named_track(id: &str, title: &str, artist: &str) -> Track {
    Track {
        title: title.into(),
        artists: vec![ArtistRef {
            id: None,
            name: artist.into(),
        }],
        ..track(id, Some(30_000))
    }
}

/// 230 liked songs: every fifth in Hangul by one artist, the rest Latin by sixteen artists.
fn fixture_liked(index: usize) -> Track {
    if index.is_multiple_of(5) {
        named_track(&format!("fixture-liked-{index}"), "봄날", "방탄")
    } else {
        named_track(
            &format!("fixture-liked-{index}"),
            "Song",
            &format!("Artist {}", index % 20),
        )
    }
}

fn paged<T>(
    items: Vec<T>,
    next_cursor: Option<String>,
    scope: &AccountScope,
) -> ytm_protocol::PagedResult<T> {
    ytm_protocol::PagedResult {
        items,
        next_cursor,
        freshness: Freshness::Fresh,
        fetched_at_ms: 1_000,
        account_scope: scope.clone(),
        provider_version: "fixture".into(),
    }
}

impl CatalogPort for ProfileCatalog {
    fn query(
        &self,
        epoch: Option<AccountEpoch>,
        _scope: Option<AccountScope>,
        request: CatalogQueryRequest,
        _cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<CatalogResponsePayload, QueryError>> {
        Box::pin(async move {
            let scope = profile_scope(epoch.as_ref().unwrap().as_str());
            let other = scope.as_str() == PROFILE_OTHER_ACCOUNT;
            let offset: usize = request
                .cursor
                .as_deref()
                .map_or(0, |cursor| cursor.parse().unwrap());
            let limit = request.limit.unwrap() as usize;
            let window = |len: usize| {
                let end = (offset + limit).min(len);
                (offset..end, (end < len).then(|| end.to_string()))
            };
            match request.kind {
                ytm_protocol::CatalogQueryKind::LikedSongs => {
                    self.liked_reads.fetch_add(1, Ordering::SeqCst);
                    if self.liked_hangs.load(Ordering::SeqCst) {
                        std::future::pending::<()>().await;
                    }
                    if self.liked_fails.load(Ordering::SeqCst) {
                        return Err(QueryError {
                            code: ytm_protocol::QueryErrorCode::Timeout,
                            retriable: true,
                            retry_after_ms: None,
                            safe_message: "fixture liked read timed out".into(),
                            request_id: None,
                        });
                    }
                    let (range, next) = window(if other { 3 } else { 230 });
                    let items = range
                        .map(|index| {
                            if other {
                                named_track(&format!("fixture-other-{index}"), "Кино", "Группа")
                            } else {
                                fixture_liked(index)
                            }
                        })
                        .collect();
                    Ok(CatalogResponsePayload::LikedSongs(
                        paged(items, next, &scope).into(),
                    ))
                }
                ytm_protocol::CatalogQueryKind::LibraryPlaylists => {
                    let (range, next) = window(if other { 2 } else { 35 });
                    let items = range
                        .map(|index| ytm_protocol::PlaylistSummary {
                            playlist_id: PlaylistId::new(format!("PL_fixture_profile_{index}"))
                                .unwrap(),
                            browse_id: None,
                            title: format!("Fixture playlist {index}"),
                            subtitle: None,
                            track_count: Some(index as u32),
                            cover: None,
                        })
                        .collect();
                    Ok(CatalogResponsePayload::LibraryPlaylists(paged(
                        items, next, &scope,
                    )))
                }
                kind => panic!("a listener profile should not read {kind:?}"),
            }
        })
    }

    fn mutate(
        &self,
        _epoch: AccountEpoch,
        _scope: AccountScope,
        _request: MutationRequest,
        _cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<MutationResult, QueryError>> {
        Box::pin(async { panic!("a listener profile never writes") })
    }

    fn apply_session_material(
        &self,
        material: SessionMaterialPayload,
        _cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<CatalogIdentity, QueryError>> {
        Box::pin(async move {
            Ok(CatalogIdentity {
                account_scope: profile_scope(material.account_epoch.as_str()),
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
        Box::pin(async { panic!("a listener profile should not request lyrics") })
    }

    fn clear_session_material(&self, _epoch: Option<AccountEpoch>) -> Result<(), QueryError> {
        Ok(())
    }
}

impl ControlCatalog for ProfileCatalog {
    fn fresh_playlist(
        &self,
        _context: CatalogReadContext,
        _playlist: PlaylistId,
        _page: PageInput,
    ) -> BoxFuture<'_, Result<Playlist, QueryError>> {
        Box::pin(async { panic!("a listener profile should not page a playlist") })
    }

    fn recommend_song_radio(
        &self,
        _context: CatalogReadContext,
        _seed: VideoId,
        _page: PageInput,
    ) -> BoxFuture<'_, Result<RecommendationPage, QueryError>> {
        Box::pin(async { panic!("a listener profile should not ask for recommendations") })
    }
}

struct ProfileFixture {
    _root: TempDir,
    engine: CoreEngine,
    catalog: Arc<ProfileCatalog>,
    storage: Arc<SqliteStorage>,
    service: ControlService,
}

impl ProfileFixture {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let clock = Arc::new(MockClock::new(1_000));
        let catalog = Arc::new(ProfileCatalog::default());
        let (sender, _commands) = mpsc::channel(8);
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
                        bridge_version: "1.0.0".into(),
                        capabilities: CapabilitySet::default(),
                        account_hint: Some(HINT.into()),
                        logged_in_observed: true,
                    }
                )
                .await
                .unwrap()
        );
        let storage = open_storage(&root, clock);
        let service = ControlService::new(engine.clone())
            .with_catalog(catalog.clone())
            .with_local_history(storage.clone())
            .with_dj_store(storage.clone())
            .with_profile_part_deadline(Duration::from_millis(300));
        let fixture = Self {
            _root: root,
            engine,
            catalog,
            storage,
            service,
        };
        fixture.sign_in(EPOCH).await;
        fixture
    }

    async fn sign_in(&self, epoch: &str) {
        self.engine
            .on_session_material(SessionMaterialPayload {
                account_epoch: AccountEpoch::new(epoch).unwrap(),
                credential_revision: CredentialRevision::new(format!("{epoch}-credentials"))
                    .unwrap(),
                cookie_pairs: Default::default(),
                auth_user_index: None,
                channel_label: None,
            })
            .await
            .unwrap();
        let auth = self.engine.snapshot().await.unwrap().auth_state;
        assert_eq!(auth.account_scope, Some(profile_scope(epoch)));
    }

    async fn profile(&self, scope: &str, refresh: bool) -> ytm_control_api::ListenerProfile {
        let response = self.profile_response(scope, refresh).await;
        let ControlResponse::ListenerProfile(profile) = response else {
            panic!("expected a listener profile: {response:?}");
        };
        *profile
    }

    async fn profile_response(&self, scope: &str, refresh: bool) -> ControlResponse {
        let grant = ReadGrant::new(
            "fixture-profile-client".into(),
            Some(AccountScope::new(scope).unwrap()),
        );
        let read = self.service.execute(
            &grant,
            ControlRequest::GetListenerProfile(ytm_control_api::ListenerProfileInput { refresh }),
        );
        timeout(WAIT, read)
            .await
            .expect("the listener profile answers in time")
    }

    fn liked_reads(&self) -> usize {
        self.catalog.liked_reads.load(Ordering::SeqCst)
    }
}

/// Records local plays for the fixture account: a kana song twice around a liked Latin song.
async fn record_profile_history(storage: &SqliteStorage) {
    let scope = AccountScope::new(ACCOUNT).unwrap();
    let epoch = AccountEpoch::new(EPOCH).unwrap();
    let kana = named_track("fixture-kana", "夜に駆ける", "ヨル");
    for (time, track) in [(10, kana.clone()), (20, fixture_liked(1)), (30, kana)] {
        storage
            .record_recent_playback(
                &scope,
                &epoch,
                RecentPlaybackRecord {
                    track,
                    playback_ref: None,
                    position_ms: 1_000,
                    duration_ms: Some(30_000),
                    recorded_at_ms: time,
                    source_context: None,
                },
            )
            .await
            .unwrap();
    }
    storage
        .put_preference(
            scope,
            DjPreference {
                preference_id: "fixture-profile-taste".into(),
                subject: DjPreferenceSubject::Video {
                    video_id: VideoId::new("fixture-kana").unwrap(),
                    title: None,
                },
                stance: DjPreferenceStance::Prefer,
                note: None,
                recorded_by: "fixture-profile-client".into(),
                recorded_at_ms: 40,
            },
            500,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn the_listener_profile_samples_liked_history_and_playlists_within_their_caps() {
    use ytm_control_api::Script;
    let fixture = ProfileFixture::new().await;
    record_profile_history(&fixture.storage).await;

    let profile = fixture.profile(ACCOUNT, false).await;
    assert!(profile.unavailable.is_empty(), "{:?}", profile.unavailable);
    // Two pages of a hundred, of 230: the sample is capped and the total is not claimed.
    assert_eq!(fixture.liked_reads(), 2);
    assert_eq!((profile.liked.sampled, profile.liked.total), (200, None));
    let liked_recent: Vec<_> = profile
        .liked
        .recent
        .iter()
        .map(|track| track.video_id.as_str().to_owned())
        .collect();
    assert_eq!(
        liked_recent,
        (0..12)
            .map(|index| format!("fixture-liked-{index}"))
            .collect::<Vec<_>>()
    );
    let artists = &profile.liked.top_artists;
    assert_eq!(artists.len(), 15);
    assert_eq!((artists[0].name.as_str(), artists[0].tracks), ("방탄", 40));
    assert_eq!(
        (artists[1].name.as_str(), artists[1].tracks),
        ("Artist 1", 10)
    );
    assert_eq!(artists[14].name, "Artist 17");

    // Three plays, two of the same song: one recent entry, both plays count for its artist.
    assert_eq!(
        (profile.history.sampled, profile.history.total),
        (3, Some(3))
    );
    let history_recent: Vec<_> = profile
        .history
        .recent
        .iter()
        .map(|track| track.video_id.as_str())
        .collect();
    assert_eq!(history_recent, ["fixture-kana", "fixture-liked-1"]);
    assert_eq!(
        (
            profile.history.top_artists[0].name.as_str(),
            profile.history.top_artists[0].tracks
        ),
        ("ヨル", 2)
    );

    assert_eq!(profile.playlists.len(), 30);
    assert!(profile.more_playlists);
    assert_eq!(profile.playlists[29].title, "Fixture playlist 29");
    assert_eq!(profile.taste_notes, 1);

    // 200 liked plus one kana song; the liked song that was also played counts once.
    assert_eq!(profile.scripts_sampled, 201);
    let scripts: Vec<_> = profile
        .scripts
        .iter()
        .map(|share| (share.script, share.tracks, share.percent))
        .collect();
    assert_eq!(
        scripts,
        [
            (Script::Latin, 160, 80),
            (Script::Hangul, 40, 20),
            (Script::Kana, 1, 0)
        ]
    );

    // The copy is kept for this account session; refresh reads the library again.
    assert_eq!(fixture.profile(ACCOUNT, false).await, profile);
    assert_eq!(fixture.liked_reads(), 2);
    fixture.profile(ACCOUNT, true).await;
    assert_eq!(fixture.liked_reads(), 4);
    fixture.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_failing_liked_read_is_named_unavailable_and_the_rest_is_kept() {
    let fixture = ProfileFixture::new().await;
    record_profile_history(&fixture.storage).await;
    fixture.catalog.liked_fails.store(true, Ordering::SeqCst);

    let profile = fixture.profile(ACCOUNT, false).await;
    assert_eq!(profile.unavailable, ["liked"]);
    assert_eq!((profile.liked.sampled, profile.liked.total), (0, None));
    assert!(profile.liked.recent.is_empty() && profile.liked.top_artists.is_empty());
    assert_eq!((profile.history.sampled, profile.playlists.len()), (3, 30));
    assert_eq!(profile.taste_notes, 1);
    // Scripts are counted over what was read: the two played songs.
    assert_eq!(profile.scripts_sampled, 2);

    // A failed part is not kept, so the next read tries the liked songs again.
    fixture.catalog.liked_fails.store(false, Ordering::SeqCst);
    let profile = fixture.profile(ACCOUNT, false).await;
    assert!(profile.unavailable.is_empty());
    assert_eq!(profile.liked.sampled, 200);
    fixture.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn another_account_never_sees_the_kept_listener_profile() {
    let fixture = ProfileFixture::new().await;
    let first = fixture.profile(ACCOUNT, false).await;
    assert_eq!(first.liked.sampled, 200);
    assert_eq!(fixture.liked_reads(), 2);
    // The same account signed in again is a new session, as after a logout: no kept copy.
    fixture.sign_in("fixture-profile-epoch-renewed").await;
    fixture.profile(ACCOUNT, false).await;
    assert_eq!(fixture.liked_reads(), 4);

    fixture.sign_in(PROFILE_OTHER_EPOCH).await;
    let refused = fixture.profile_response(ACCOUNT, false).await;
    assert!(
        matches!(refused, ControlResponse::Error(error) if error.code == ErrorCode::PermissionDenied)
    );
    let other = fixture.profile(PROFILE_OTHER_ACCOUNT, false).await;
    assert_eq!(fixture.liked_reads(), 5);
    assert_eq!((other.liked.sampled, other.liked.total), (3, Some(3)));
    assert_eq!(other.liked.top_artists[0].name, "Группа");
    assert_eq!((other.playlists.len(), other.more_playlists), (2, false));
    assert_eq!(other.history.sampled, 0);
    assert_eq!(other.scripts[0].script, ytm_control_api::Script::Cyrillic);

    // Back on the first account, the other account's copy is not served either.
    fixture.sign_in("fixture-profile-epoch-again").await;
    let again = fixture.profile(ACCOUNT, false).await;
    assert_eq!(fixture.liked_reads(), 7);
    assert_eq!(again.liked.sampled, 200);
    fixture.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn local_history_edits_show_in_the_next_listener_profile_without_a_refresh() {
    use ytm_core::control::history::LocalHistoryStore;
    let fixture = ProfileFixture::new().await;
    record_profile_history(&fixture.storage).await;
    let before = fixture.profile(ACCOUNT, false).await;
    assert_eq!(before.history.total, Some(3));
    assert_eq!(before.scripts_sampled, 201);

    let scope = AccountScope::new(ACCOUNT).unwrap();
    let revision = fixture
        .storage
        .snapshot(scope.clone())
        .await
        .unwrap()
        .revision;
    fixture
        .storage
        .edit(
            scope,
            AccountEpoch::new(EPOCH).unwrap(),
            revision,
            LocalHistoryEdit::Clear { range: None },
        )
        .await
        .unwrap();

    // The cleared plays are gone at once; the liked songs still come from the kept copy.
    let after = fixture.profile(ACCOUNT, false).await;
    assert_eq!(fixture.liked_reads(), 2);
    assert_eq!((after.history.sampled, after.history.total), (0, Some(0)));
    assert!(after.history.recent.is_empty() && after.history.top_artists.is_empty());
    assert_eq!(after.scripts_sampled, 200);
    assert!(
        after
            .scripts
            .iter()
            .all(|share| share.script != ytm_control_api::Script::Kana)
    );
    assert_eq!(after.liked, before.liked);
    fixture.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_liked_read_past_its_deadline_is_unavailable_and_the_rest_arrives() {
    let fixture = ProfileFixture::new().await;
    record_profile_history(&fixture.storage).await;
    fixture.catalog.liked_hangs.store(true, Ordering::SeqCst);

    let profile = fixture.profile(ACCOUNT, false).await;
    assert_eq!(profile.unavailable, ["liked"]);
    assert_eq!((profile.history.sampled, profile.playlists.len()), (3, 30));
    assert_eq!(profile.taste_notes, 1);
    fixture.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_expired_sign_in_still_serves_the_local_parts_of_the_listener_profile() {
    let fixture = ProfileFixture::new().await;
    record_profile_history(&fixture.storage).await;
    fixture.profile(ACCOUNT, false).await;
    fixture
        .engine
        .on_auth_expired(Some(AccountEpoch::new(EPOCH).unwrap()), None)
        .await
        .unwrap();

    // The kept catalog parts belong to the expired session and are not served.
    let profile = fixture.profile(ACCOUNT, false).await;
    assert_eq!(profile.unavailable, ["liked", "playlists"]);
    assert_eq!(fixture.liked_reads(), 2);
    assert_eq!((profile.liked.sampled, profile.playlists.len()), (0, 0));
    assert_eq!(
        (profile.history.sampled, profile.history.total),
        (3, Some(3))
    );
    assert_eq!(profile.taste_notes, 1);
    assert_eq!(profile.scripts_sampled, 2);
    fixture.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_failed_refresh_leaves_no_older_liked_copy_behind() {
    let fixture = ProfileFixture::new().await;
    fixture.profile(ACCOUNT, false).await;
    assert_eq!(fixture.liked_reads(), 2);

    fixture.catalog.liked_fails.store(true, Ordering::SeqCst);
    let refreshed = fixture.profile(ACCOUNT, true).await;
    assert_eq!(refreshed.unavailable, ["liked"]);
    let failed_reads = fixture.liked_reads();

    // The next ordinary read pages the liked songs again instead of the copy from before.
    fixture.catalog.liked_fails.store(false, Ordering::SeqCst);
    let profile = fixture.profile(ACCOUNT, false).await;
    assert!(profile.unavailable.is_empty());
    assert_eq!(fixture.liked_reads(), failed_reads + 2);
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
                official_upcoming: None,
                filters: None,
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
                        audio_itag: None,
                        audio_quality: None,
                        audio_bitrate_kbps: None,
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

impl Harness {
    /// Offers the fixture filters (`All`, with `Familiar` selected) after two explicit tracks.
    async fn offer_filters(&mut self) {
        self.long_queue(&[FIRST, SECOND]).await;
        self.queue.filters = Some(filters());
        self.publish_queue().await;
    }

    async fn select_filter(&self, id: &str, label: &str) -> OperationReceipt {
        self.submit_queue(
            id,
            WriteAction::SetQueueFilter {
                filter_id: format!("fixture-filter-{label}"),
            },
        )
        .await
    }
}

/// Selecting an Up Next filter is observed once a snapshot shows it selected, even when the
/// explicit queue revision stays the same; a skip meanwhile does not settle it either way.
#[tokio::test]
async fn an_up_next_filter_is_verified_when_the_page_shows_it_selected() {
    let mut fixture = Harness::new().await;
    fixture.offer_filters().await;
    fixture.select_filter("filter-all", "All").await;
    let command = timeout(WAIT, fixture.commands.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(command.op, PlayerOp::SetQueueFilter);
    let args = command.args.unwrap();
    assert_eq!(args.queue_filter_id.as_deref(), Some("fixture-filter-All"));
    assert_eq!((args.queue_revision, args.track_instance_id), (None, None));

    // The listener skips: neither a failure nor the selection.
    fixture.observe_at(1, PlaybackStatus::Playing, 0).await;
    let pending = fixture
        .engine
        .command_receipt(command.command_id.clone())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        pending.status,
        ytm_protocol::CommandLifecycleStatus::Accepted
            | ytm_protocol::CommandLifecycleStatus::Dispatched
    ));

    for filter in fixture.queue.filters.iter_mut().flatten() {
        filter.selected = filter.label == "All";
    }
    fixture.seq += 1;
    assert!(
        fixture
            .engine
            .on_bridge_queue_observed(fixture.seq, fixture.queue.clone())
            .await
            .unwrap()
    );
    let selected = fixture.finished("filter-all").await;
    assert_eq!(selected.phase, OperationPhase::Verified);
    assert_eq!(
        selected.resource,
        Some(OperationResource::Player {
            op: PlayerOp::SetQueueFilter
        })
    );
    fixture.engine.shutdown().await.unwrap();
}

/// A filter the current queue does not offer is refused, and the selected one is already
/// observed; neither reaches the page.
#[tokio::test]
async fn unknown_or_selected_up_next_filters_are_not_sent() {
    let mut fixture = Harness::new().await;
    fixture.offer_filters().await;
    fixture.select_filter("filter-unknown", "Workout").await;
    let unknown = fixture.finished("filter-unknown").await;
    assert_eq!(unknown.phase, OperationPhase::Rejected);
    assert_eq!(unknown.error.unwrap().code, ErrorCode::StaleContext);

    fixture.select_filter("filter-same", "Familiar").await;
    let same = fixture.finished("filter-same").await;
    assert_eq!(same.phase, OperationPhase::Verified);
    assert!(fixture.commands.try_recv().is_err());
    fixture.engine.shutdown().await.unwrap();
}

/// The page refusing to click a filter is a rejection: nothing changed.
#[tokio::test]
async fn a_refused_up_next_filter_is_rejected() {
    let mut fixture = Harness::new().await;
    fixture.offer_filters().await;
    fixture.select_filter("filter-refused", "All").await;
    let command = timeout(WAIT, fixture.commands.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(command.op, PlayerOp::SetQueueFilter);
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
                    safe_message: "the Up Next filter is not on the page".into(),
                    request_id: None,
                }),
            },
        )
        .await
        .unwrap();
    let refused = fixture.finished("filter-refused").await;
    assert_eq!(refused.phase, OperationPhase::Rejected);
    assert!(refused.settled);
    fixture.engine.shutdown().await.unwrap();
}

/// A listener's Up Next filter click does not take over: the agent's insert waiting for its
/// observation still verifies, and the filter, already selected, settles after it with
/// nothing left pending.
#[tokio::test]
async fn a_manual_up_next_filter_leaves_agent_queue_edits_running() {
    let mut fixture = Harness::new().await;
    fixture.offer_filters().await;
    let context_token = fixture.queue_context().await;
    let (_, mut deltas) = fixture.engine.subscribe_snapshot().await.unwrap();
    fixture
        .submit_queue(
            "insert-before-filter",
            WriteAction::Enqueue {
                context_token,
                mode: ytm_control_api::QueueMode::Next,
                video_ids: video_ids(&[THIRD]),
            },
        )
        .await;
    let command = timeout(WAIT, fixture.commands.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(command.op, PlayerOp::Enqueue);
    let filter = fixture
        .engine
        .submit_player_command(PlayerCommandPayload {
            command_id: CommandId::new("fixture-ui-filter").unwrap(),
            op: PlayerOp::SetQueueFilter,
            args: Some(ytm_protocol::PlayerCommandArgs {
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
                queue_filter_id: Some("fixture-filter-Familiar".into()),
            }),
        })
        .await
        .unwrap();
    assert_eq!(
        filter.status,
        ytm_protocol::CommandLifecycleStatus::Accepted
    );

    let current = fixture.queue.current_entry_id.clone().unwrap();
    let index = fixture.position_of(&current) + 1;
    fixture.add_entry(index, THIRD);
    fixture.publish_queue().await;
    assert_eq!(
        fixture.finished("insert-before-filter").await.phase,
        OperationPhase::Verified
    );
    let settled = fixture
        .engine
        .command_receipt(filter.command_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        settled.status,
        ytm_protocol::CommandLifecycleStatus::Observed
    );
    assert!(fixture.commands.try_recv().is_err());

    // A snapshot round trip orders every earlier engine message before the drain.
    fixture.engine.snapshot().await.unwrap();
    let mut last_pending = None;
    while let Ok(delta) = deltas.try_recv() {
        if let StateDelta::PlayerChanged { pending, .. } = delta {
            last_pending = Some(pending);
        }
    }
    let last_pending = last_pending.unwrap();
    assert_eq!(
        (last_pending.pending_command_count, last_pending.active_op),
        (0, None)
    );
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

/// A prompt taken by a poll whose reply the client never got (its wait was cancelled) is offered
/// again when the next wait names the last prompt received; one naming another core epoch is
/// ignored.
#[tokio::test]
async fn a_prompt_lost_with_a_cancelled_wait_is_offered_again() {
    let mut fixture = Harness::new().await;
    fixture.long_queue(&[FIRST, SECOND, THIRD]).await;
    let quiet = DjCueInput {
        chattiness: Some(DjChattiness::Off),
        ..Default::default()
    };
    assert!(fixture.cue(quiet).await.is_none());
    let received = fixture.next_ending(0).await;
    let lost = fixture.next_ending(1).await;
    let after = |core_epoch: CoreEpoch, seq| DjCueInput {
        last_received: Some(DjCueAck { core_epoch, seq }),
        ..Default::default()
    };
    let epoch = received.core_epoch.clone().unwrap();
    let other = CoreEpoch::new("fixture-other-core").unwrap();
    assert!(fixture.cue(after(other, received.seq)).await.is_none());
    let again = fixture
        .cue(after(epoch.clone(), received.seq))
        .await
        .expect("the lost prompt comes again");
    assert_eq!(again.seq, lost.seq);
    assert!(fixture.cue(after(epoch, again.seq)).await.is_none());
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

/// When the page's current occurrence is not in the loaded window (a selection lag), the list
/// is not what follows: no up-next, no "little queued" warning, and the context says unknown.
#[tokio::test]
async fn an_unknown_current_occurrence_is_not_reported_as_up_next() {
    let mut fixture = Harness::new().await;
    fixture.long_queue(&[FIRST, SECOND, THIRD]).await;
    let quiet = DjCueInput {
        chattiness: Some(DjChattiness::Off),
        ..Default::default()
    };
    assert!(fixture.cue(quiet).await.is_none());
    fixture.queue.current_entry_id = None;
    fixture.publish_queue().await;

    // Two tracks would be "one or two queued" (75 s lead); unknown keeps the normal 45 s lead.
    fixture.clock.advance(170_000);
    fixture
        .observe_at(0, PlaybackStatus::Playing, 170_000)
        .await;
    assert!(fixture.cue(DjCueInput::default()).await.is_none());
    fixture.clock.advance(30_000);
    fixture
        .observe_at(0, PlaybackStatus::Playing, 200_000)
        .await;
    let cue = fixture.cue(DjCueInput::default()).await.unwrap();
    assert_eq!(cue.kind, DjCueKind::TrackEnding);
    assert!(cue.reasons.is_empty(), "{:?}", cue.reasons);
    assert!(
        cue.prompt
            .contains("what is queued after it is not known right now"),
        "{}",
        cue.prompt
    );
    assert_eq!(cue.upcoming, ytm_control_api::UpcomingStatus::Unknown);
    assert!(cue.up_next.is_empty() && !cue.more_queued);

    let ControlResponse::Context(context) = fixture
        .service
        .execute(
            &fixture.read,
            ControlRequest::GetContext(ContextInput {
                preview_limit: Some(5),
            }),
        )
        .await
    else {
        panic!("expected context");
    };
    assert_eq!(
        context.queue.upcoming,
        ytm_control_api::UpcomingStatus::Unknown
    );
    assert!(context.queue.entries.is_empty());
    assert!(context.queue.next_cursor.is_none());

    // The anchor comes back: what follows it is known again.
    fixture.play_from(1, 0).await;
    let ControlResponse::Context(context) = fixture
        .service
        .execute(
            &fixture.read,
            ControlRequest::GetContext(ContextInput {
                preview_limit: Some(5),
            }),
        )
        .await
    else {
        panic!("expected context");
    };
    assert_eq!(
        context.queue.upcoming,
        ytm_control_api::UpcomingStatus::Known
    );
    assert_eq!(context.queue.entries.len(), 1);
    assert_eq!(context.queue.entries[0].track.title, THIRD);
    fixture.engine.shutdown().await.unwrap();
}

fn official(status: OfficialUpcomingStatus, count: usize) -> OfficialUpcoming {
    OfficialUpcoming {
        status,
        tracks: (0..count)
            .map(|index| track(&format!("fixture-auto-{index}"), Some(200_000)))
            .collect(),
    }
}

fn filters() -> Vec<QueueFilter> {
    [("All", false), ("Familiar", true)]
        .into_iter()
        .map(|(label, selected)| QueueFilter {
            filter_id: format!("fixture-filter-{label}"),
            label: label.into(),
            selected,
        })
        .collect()
}

impl Harness {
    /// Plays the last of two explicit tracks after an autoplay tail with the given continuation,
    /// then returns the track-ending prompt.
    async fn ending_before(&mut self, continuation: OfficialUpcoming, explicit: bool) -> DjCue {
        self.long_queue(&[FIRST, SECOND]).await;
        self.queue.completeness = QueueCompleteness::Partial;
        self.queue.has_more = Some(true);
        self.queue.explicit_complete = explicit.then_some(true);
        self.queue.official_upcoming = Some(continuation);
        self.queue.filters = Some(filters());
        self.play_from(1, 0).await;
        let quiet = DjCueInput {
            chattiness: Some(DjChattiness::Off),
            ..Default::default()
        };
        assert!(self.cue(quiet).await.is_none());
        self.clock.advance(200_000);
        self.observe_at(1, PlaybackStatus::Playing, 200_000).await;
        let cue = self.cue(DjCueInput::default()).await.unwrap();
        assert_eq!(cue.kind, DjCueKind::TrackEnding);
        cue
    }
}

/// YouTube Music's observed suggestions after the last explicit track are named in the prompt
/// with the selected filter, but still do not count as queued music.
#[tokio::test]
async fn the_official_continuation_is_described_but_not_queued() {
    let mut fixture = Harness::new().await;
    let cue = fixture
        .ending_before(official(OfficialUpcomingStatus::Observed, 6), true)
        .await;
    assert_eq!(cue.reasons, vec![DjCueReason::LittleQueued]);
    assert!(cue.up_next.is_empty() && !cue.more_queued);
    assert!(
        cue.prompt.contains(
            "nothing is queued after it; then YouTube Music continues on its own with \
             \"fixture-auto-0\", \"fixture-auto-1\", \"fixture-auto-2\" and more \
             (filter: Familiar), which you may leave if that fits or steer with an Up Next \
             filter: All."
        ),
        "{}",
        cue.prompt
    );
    let up_next = cue.official_up_next.expect("official continuation");
    assert_eq!(up_next.status, OfficialUpcomingStatus::Observed);
    let titles: Vec<_> = up_next.tracks.iter().map(|t| t.title.as_str()).collect();
    assert_eq!(
        titles,
        [
            "fixture-auto-0",
            "fixture-auto-1",
            "fixture-auto-2",
            "fixture-auto-3",
            "fixture-auto-4"
        ]
    );
    assert!(up_next.more);
    assert_eq!(up_next.filter.as_deref(), Some("Familiar"));
    fixture.engine.shutdown().await.unwrap();
}

/// Autoplay off and a self-refilling radio are stated as such, even when more is queued.
#[tokio::test]
async fn an_off_or_radio_continuation_is_stated() {
    for (status, explicit, text) in [
        (
            OfficialUpcomingStatus::Off,
            true,
            "nothing is queued after it; YouTube Music autoplay is off, so nothing plays after \
             the queued tracks.",
        ),
        (
            OfficialUpcomingStatus::Radio,
            false,
            "or more tracks are queued after it; this is a YouTube Music radio that refills \
             itself.",
        ),
    ] {
        let mut fixture = Harness::new().await;
        let cue = fixture.ending_before(official(status, 0), explicit).await;
        assert!(cue.prompt.contains(text), "{}", cue.prompt);
        assert!(!cue.prompt.contains("would continue on its own"));
        let up_next = cue.official_up_next.expect("official continuation");
        assert_eq!(up_next.status, status);
        assert!(up_next.tracks.is_empty() && !up_next.more);
        fixture.engine.shutdown().await.unwrap();
    }
}

/// When what follows the current track is unknown, the page's continuation is stated as following
/// the queued tracks without naming its suggestions, keeping the advice to read the context.
#[tokio::test]
async fn an_unknown_upcoming_keeps_the_official_status_without_naming_suggestions() {
    for (status, clause) in [
        (
            OfficialUpcomingStatus::Observed,
            "after the queued tracks YouTube Music continues on its own with its suggestions",
        ),
        (
            OfficialUpcomingStatus::Loading,
            "YouTube Music is still loading what plays after the queued tracks",
        ),
        (
            OfficialUpcomingStatus::None,
            "YouTube Music has no suggestions loaded to play after the queued tracks",
        ),
    ] {
        let mut fixture = Harness::new().await;
        fixture.long_queue(&[FIRST, SECOND, THIRD]).await;
        let quiet = DjCueInput {
            chattiness: Some(DjChattiness::Off),
            ..Default::default()
        };
        assert!(fixture.cue(quiet).await.is_none());
        let count = if status == OfficialUpcomingStatus::Observed {
            3
        } else {
            0
        };
        fixture.queue.current_entry_id = None;
        fixture.queue.official_upcoming = Some(official(status, count));
        fixture.publish_queue().await;
        fixture.clock.advance(200_000);
        fixture
            .observe_at(0, PlaybackStatus::Playing, 200_000)
            .await;
        let cue = fixture.cue(DjCueInput::default()).await.unwrap();
        assert_eq!(cue.kind, DjCueKind::TrackEnding);
        let expected = format!(
            "what is queued after it is not known right now (read the context before changing \
             the queue; {clause})."
        );
        assert!(cue.prompt.contains(&expected), "{}", cue.prompt);
        assert!(!cue.prompt.contains("fixture-auto"), "{}", cue.prompt);
        assert_eq!(cue.official_up_next.expect("continuation").status, status);
        fixture.engine.shutdown().await.unwrap();
    }
}

/// Context and first queue pages carry the page's continuation, bounded by the call's limit, and
/// its filters; the continuation is known even when what follows the current track is not.
#[tokio::test]
async fn reads_carry_the_official_continuation_and_filters() {
    let mut fixture = Harness::new().await;
    fixture.long_queue(&[FIRST, SECOND, THIRD]).await;
    fixture.queue.current_entry_id = None;
    fixture.queue.official_upcoming = Some(official(OfficialUpcomingStatus::Observed, 7));
    fixture.queue.filters = Some(filters());
    fixture.publish_queue().await;
    let filter_views = vec![
        QueueFilterView {
            filter_id: "fixture-filter-All".into(),
            label: "All".into(),
            selected: false,
        },
        QueueFilterView {
            filter_id: "fixture-filter-Familiar".into(),
            label: "Familiar".into(),
            selected: true,
        },
    ];

    let ControlResponse::Context(context) = fixture
        .service
        .execute(
            &fixture.read,
            ControlRequest::GetContext(ContextInput {
                preview_limit: Some(5),
            }),
        )
        .await
    else {
        panic!("expected context");
    };
    assert_eq!(
        context.queue.upcoming,
        ytm_control_api::UpcomingStatus::Unknown
    );
    assert!(context.queue.entries.is_empty());
    let official_view = context.queue.official_upcoming.expect("continuation");
    assert_eq!(official_view.status, OfficialUpcomingStatus::Observed);
    assert_eq!(official_view.tracks.len(), 5);
    assert_eq!(official_view.tracks[0].title, "fixture-auto-0");
    assert!(official_view.more);
    assert_eq!(context.queue.filters, filter_views);

    let queue_page = |cursor: Option<String>, limit: u16| {
        fixture.service.execute(
            &fixture.read,
            ControlRequest::GetQueue(PageInput {
                cursor,
                limit: Some(limit),
            }),
        )
    };
    let ControlResponse::Queue(all) = queue_page(None, 10).await else {
        panic!("expected queue");
    };
    let official_view = all.official_upcoming.expect("continuation");
    assert_eq!(official_view.tracks.len(), 7);
    assert!(!official_view.more);
    let ControlResponse::Queue(first) = queue_page(None, 2).await else {
        panic!("expected queue");
    };
    let official_view = first.official_upcoming.expect("continuation");
    assert_eq!(official_view.tracks.len(), 2);
    assert!(official_view.more);
    assert_eq!(first.filters, filter_views);
    let ControlResponse::Queue(second) = queue_page(first.next_cursor.clone(), 2).await else {
        panic!("expected queue");
    };
    assert_eq!(second.entries.len(), 1);
    assert!(second.official_upcoming.is_none());
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

fn no_args() -> ytm_protocol::PlayerCommandArgs {
    ytm_protocol::PlayerCommandArgs {
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

fn action_kinds(
    actions: &[ytm_control_api::ListenerAction],
) -> Vec<ytm_control_api::ListenerActionKind> {
    actions.iter().map(|action| action.action).collect()
}

impl Harness {
    /// Sends a command the way the player window, media keys or MPRIS do.
    async fn by_hand(&self, id: &str, op: PlayerOp, args: Option<ytm_protocol::PlayerCommandArgs>) {
        let receipt = self
            .engine
            .submit_player_command(PlayerCommandPayload {
                command_id: CommandId::new(id).unwrap(),
                op,
                args,
            })
            .await
            .unwrap();
        assert!(receipt.error.is_none(), "{receipt:?}");
    }

    async fn context_actions(&self) -> Vec<ytm_control_api::ListenerAction> {
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
            panic!("expected a context")
        };
        context.listener_actions
    }
}

/// A volume drag is one action from its first to its last value; the agent's own skip and like
/// are not the listener's doing and are never listed.
#[tokio::test]
async fn a_volume_drag_is_one_listener_action_and_agent_commands_are_not_listed() {
    use ytm_control_api::ListenerActionKind as Kind;
    let mut fixture = Harness::new().await;
    fixture.long_queue(&[FIRST, SECOND, THIRD]).await;
    fixture
        .submit_queue("fixture-agent-next", WriteAction::Next)
        .await;
    let command = timeout(WAIT, fixture.commands.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(command.op, PlayerOp::Next);
    fixture.clock.advance(1_000);
    fixture.play_from(1, 0).await;
    fixture.finished("fixture-agent-next").await;
    fixture
        .service
        .submit(
            fixture.read.clone(),
            Arc::new(Actions(ACCOUNT_TIERS)),
            account_write(
                "fixture-agent-like",
                WriteAction::Rate {
                    target: TrackTarget::Video {
                        video_id: VideoId::new(SECOND).unwrap(),
                    },
                    rating: Rating::Liked,
                },
            ),
        )
        .await
        .unwrap();
    fixture.finished("fixture-agent-like").await;
    assert!(fixture.context_actions().await.is_empty());

    for (step, volume) in [0.6, 0.7, 0.8].into_iter().enumerate() {
        fixture.clock.advance(500);
        let args = ytm_protocol::PlayerCommandArgs {
            volume: Some(volume),
            ..no_args()
        };
        fixture
            .by_hand(
                &format!("fixture-drag-{step}"),
                PlayerOp::SetVolume,
                Some(args),
            )
            .await;
    }
    fixture.clock.advance(5_000);
    let later = ytm_protocol::PlayerCommandArgs {
        volume: Some(0.4),
        ..no_args()
    };
    fixture
        .by_hand("fixture-later-volume", PlayerOp::SetVolume, Some(later))
        .await;
    fixture
        .by_hand("fixture-pause", PlayerOp::Pause, None)
        .await;

    let actions = fixture.context_actions().await;
    assert_eq!(
        action_kinds(&actions),
        [Kind::Pause, Kind::Volume, Kind::Volume]
    );
    assert_eq!(actions[1].volume_to, Some(40));
    let drag = &actions[2];
    assert_eq!((drag.volume_from, drag.volume_to), (Some(50), Some(80)));
    assert_eq!(drag.during.as_ref().unwrap().title, SECOND);
    assert!(
        drag.ago_ms >= 5_000 && drag.ago_ms < 6_000,
        "{}",
        drag.ago_ms
    );
    fixture.engine.shutdown().await.unwrap();
}

/// A prompt lists what the listener did since this client's previous prompt, once; a prompt lost
/// with a cancelled wait brings its actions again, another client gets them too, and reading the
/// context does not use them up.
#[tokio::test]
async fn listener_actions_reach_each_client_once_even_after_a_cancelled_wait() {
    use ytm_control_api::ListenerActionKind as Kind;
    let mut fixture = Harness::new().await;
    fixture.long_queue(&[FIRST, SECOND, THIRD, FOURTH]).await;
    let quiet = DjCueInput {
        chattiness: Some(DjChattiness::Off),
        ..Default::default()
    };
    assert!(fixture.cue(quiet).await.is_none());
    fixture.clock.advance(1_000);
    let quieter = ytm_protocol::PlayerCommandArgs {
        volume: Some(0.3),
        ..no_args()
    };
    fixture
        .by_hand("fixture-volume", PlayerOp::SetVolume, Some(quieter))
        .await;
    assert_eq!(
        action_kinds(&fixture.context_actions().await),
        [Kind::Volume]
    );

    let first = fixture.next_ending(0).await;
    assert_eq!(action_kinds(&first.listener.recent_actions), [Kind::Volume]);
    assert_eq!(first.listener.recent_actions[0].volume_to, Some(30));
    assert_eq!(first.listener.actions_omitted, 0);
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
    let shared = reply.cue.unwrap();
    assert_eq!(shared.seq, first.seq);
    assert_eq!(
        action_kinds(&shared.listener.recent_actions),
        [Kind::Volume]
    );

    let mute = ytm_protocol::PlayerCommandArgs {
        muted: Some(true),
        ..no_args()
    };
    fixture
        .by_hand("fixture-mute", PlayerOp::SetMuted, Some(mute))
        .await;
    let lost = fixture.next_ending(1).await;
    assert_eq!(action_kinds(&lost.listener.recent_actions), [Kind::Mute]);
    assert_eq!(lost.listener.recent_actions[0].value.as_deref(), Some("on"));

    let after = |cue: &DjCue| DjCueInput {
        last_received: Some(DjCueAck {
            core_epoch: cue.core_epoch.clone().unwrap(),
            seq: cue.seq,
        }),
        ..Default::default()
    };
    let again = fixture.cue(after(&first)).await.unwrap();
    assert_eq!(again.seq, lost.seq);
    assert_eq!(action_kinds(&again.listener.recent_actions), [Kind::Mute]);
    assert!(fixture.cue(after(&again)).await.is_none());
    let third = fixture.next_ending(2).await;
    assert!(third.listener.recent_actions.is_empty());
    assert_eq!(third.listener.actions_omitted, 0);
    // The context still lists the mute; the volume change is over ten minutes old by now.
    assert_eq!(action_kinds(&fixture.context_actions().await), [Kind::Mute]);
    fixture.engine.shutdown().await.unwrap();
}

/// Two tracks skipped by hand in a row bring one prompt a few seconds later, at any chattiness;
/// a pause and a resume bring none and are only listed.
#[tokio::test]
async fn two_listener_skips_bring_one_skip_streak_prompt_and_a_pause_none() {
    use ytm_control_api::ListenerActionKind as Kind;
    let mut fixture = Harness::new().await;
    fixture.long_queue(&[FIRST, SECOND, THIRD, FOURTH]).await;
    let quiet = DjCueInput {
        chattiness: Some(DjChattiness::Off),
        ..Default::default()
    };
    assert!(fixture.cue(quiet).await.is_none());
    fixture.clock.advance(30_000);
    fixture.observe_at(0, PlaybackStatus::Playing, 30_000).await;

    fixture
        .by_hand("fixture-pause", PlayerOp::Pause, None)
        .await;
    fixture.observe_at(0, PlaybackStatus::Paused, 30_000).await;
    fixture.clock.advance(10_000);
    assert!(fixture.cue(DjCueInput::default()).await.is_none());
    fixture.by_hand("fixture-play", PlayerOp::Play, None).await;
    fixture.observe_at(0, PlaybackStatus::Playing, 30_000).await;
    fixture.clock.advance(10_000);
    assert!(fixture.cue(DjCueInput::default()).await.is_none());

    fixture
        .by_hand("fixture-skip-1", PlayerOp::Next, None)
        .await;
    fixture.clock.advance(1_000);
    fixture.play_from(1, 0).await;
    fixture.clock.advance(20_000);
    fixture.observe_at(1, PlaybackStatus::Playing, 20_000).await;
    assert!(fixture.cue(DjCueInput::default()).await.is_none());
    fixture
        .by_hand("fixture-skip-2", PlayerOp::Next, None)
        .await;
    fixture.clock.advance(1_000);
    fixture.play_from(2, 0).await;
    assert!(fixture.cue(DjCueInput::default()).await.is_none());
    // A third skip within the same burst makes the streak longer, not a new one.
    fixture.clock.advance(2_000);
    fixture.observe_at(2, PlaybackStatus::Playing, 2_000).await;
    fixture
        .by_hand("fixture-skip-3", PlayerOp::Next, None)
        .await;
    fixture.clock.advance(1_000);
    fixture.play_from(3, 0).await;
    assert!(fixture.cue(DjCueInput::default()).await.is_none());
    fixture.clock.advance(5_000);
    fixture.observe_at(3, PlaybackStatus::Playing, 5_000).await;

    let signal = fixture.cue(DjCueInput::default()).await.unwrap();
    assert_eq!(signal.kind, DjCueKind::ListenerSignal);
    assert_eq!(signal.reasons, [DjCueReason::SkipStreak]);
    assert!(
        signal.prompt.contains(&format!(
            "skipped 3 tracks in a row without finishing them (\"{FIRST}\", \"{SECOND}\", \
             \"{THIRD}\")."
        )),
        "{}",
        signal.prompt
    );
    assert!(signal.prompt.contains("do not undo"), "{}", signal.prompt);
    assert_eq!(
        action_kinds(&signal.listener.recent_actions),
        [Kind::Next, Kind::Next, Kind::Next, Kind::Play, Kind::Pause]
    );
    assert_eq!(signal.listener.unanswered_check_ins, 0);
    fixture.clock.advance(10_000);
    assert!(fixture.cue(DjCueInput::default()).await.is_none());
    fixture.engine.shutdown().await.unwrap();
}

/// Nothing the listener did before a DJ waited brings a prompt, and listener-signal prompts come
/// at most once every two minutes.
#[tokio::test]
async fn listener_signals_need_a_waiting_dj_and_come_at_most_every_two_minutes() {
    use ytm_control_api::ListenerActionKind as Kind;
    let repeat = |mode| ytm_protocol::PlayerCommandArgs {
        repeat: Some(mode),
        ..no_args()
    };
    let mut fixture = Harness::new().await;
    fixture.long_queue(&[FIRST, SECOND, THIRD, FOURTH]).await;
    fixture
        .by_hand(
            "fixture-repeat-early",
            PlayerOp::SetRepeat,
            Some(repeat(ytm_protocol::RepeatMode::One)),
        )
        .await;
    fixture.clock.advance(6_000);
    let quiet = DjCueInput {
        chattiness: Some(DjChattiness::Off),
        ..Default::default()
    };
    assert!(fixture.cue(quiet).await.is_none());
    fixture.clock.advance(6_000);
    assert!(fixture.cue(DjCueInput::default()).await.is_none());

    fixture
        .by_hand(
            "fixture-repeat-off",
            PlayerOp::SetRepeat,
            Some(repeat(ytm_protocol::RepeatMode::Off)),
        )
        .await;
    fixture
        .by_hand(
            "fixture-repeat-one",
            PlayerOp::SetRepeat,
            Some(repeat(ytm_protocol::RepeatMode::One)),
        )
        .await;
    fixture.clock.advance(6_000);
    let replay = fixture.cue(DjCueInput::default()).await.unwrap();
    assert_eq!(replay.kind, DjCueKind::ListenerSignal);
    assert_eq!(replay.reasons, [DjCueReason::Replay]);

    fixture.clock.advance(30_000);
    let pick = ytm_protocol::PlayerCommandArgs {
        queue_entry_id: Some(fixture.queue.entries[2].entry_id.clone()),
        ..no_args()
    };
    fixture
        .by_hand("fixture-pick", PlayerOp::SelectQueueEntry, Some(pick))
        .await;
    fixture.clock.advance(6_000);
    assert!(fixture.cue(DjCueInput::default()).await.is_none());

    // The pick inside the gap is held, not dropped, and comes once the gap has passed.
    fixture.clock.advance(84_000);
    let picked = fixture.cue(DjCueInput::default()).await.unwrap();
    assert_eq!(picked.kind, DjCueKind::ListenerSignal);
    assert_eq!(picked.reasons, [DjCueReason::ManualPick]);
    assert!(
        picked.prompt.contains(&format!(
            "In the last 2 minutes, by hand, the listener jumped to \"{THIRD}\" in the queue."
        )),
        "{}",
        picked.prompt
    );
    let actions = &picked.listener.recent_actions;
    assert_eq!(action_kinds(actions), [Kind::PickFromQueue]);
    assert_eq!(actions[0].subject.as_ref().unwrap().title, THIRD);
    fixture.clock.advance(10_000);
    assert!(fixture.cue(DjCueInput::default()).await.is_none());
    fixture.engine.shutdown().await.unwrap();
}
/// A listener signal still settling when the agent stopped waiting does not come when a DJ
/// waits again minutes later.
#[tokio::test]
async fn a_signal_left_settling_when_the_dj_went_off_never_comes() {
    let mut fixture = Harness::new().await;
    fixture.long_queue(&[FIRST, SECOND, THIRD]).await;
    let quiet = DjCueInput {
        chattiness: Some(DjChattiness::Off),
        ..Default::default()
    };
    assert!(fixture.cue(quiet).await.is_none());
    let one = ytm_protocol::PlayerCommandArgs {
        repeat: Some(ytm_protocol::RepeatMode::One),
        ..no_args()
    };
    fixture
        .by_hand("fixture-repeat-one", PlayerOp::SetRepeat, Some(one))
        .await;
    fixture.clock.advance(1_000);
    assert!(fixture.cue(DjCueInput::default()).await.is_none());
    // Paused, so the track's own ending prompt stays out of the way.
    fixture.observe_at(0, PlaybackStatus::Paused, 1_000).await;
    fixture.clock.advance(200_000);
    let back = fixture.cue(DjCueInput::default()).await;
    assert!(back.is_none(), "{back:?}");
    fixture.engine.shutdown().await.unwrap();
}

impl Harness {
    async fn cue_as(&self, grant: &ReadGrant, input: DjCueInput) -> Option<DjCue> {
        match self
            .service
            .execute(grant, ControlRequest::NextDjCue(input))
            .await
        {
            ControlResponse::DjCue(reply) => reply.cue,
            other => panic!("unexpected cue reply: {other:?}"),
        }
    }
}

/// The player bar turns repeat off by stepping through repeat-one; with repeat off in the end
/// the burst says nothing about hearing the track again.
#[tokio::test]
async fn stepping_through_repeat_one_to_off_brings_no_replay_signal() {
    let mut fixture = Harness::new().await;
    fixture.long_queue(&[FIRST, SECOND]).await;
    let quiet = DjCueInput {
        chattiness: Some(DjChattiness::Off),
        ..Default::default()
    };
    assert!(fixture.cue(quiet).await.is_none());
    use ytm_protocol::RepeatMode;
    for (step, mode) in [RepeatMode::All, RepeatMode::One, RepeatMode::Off]
        .into_iter()
        .enumerate()
    {
        fixture.clock.advance(1_000);
        let args = ytm_protocol::PlayerCommandArgs {
            repeat: Some(mode),
            ..no_args()
        };
        fixture
            .by_hand(
                &format!("fixture-repeat-{step}"),
                PlayerOp::SetRepeat,
                Some(args),
            )
            .await;
        assert!(fixture.cue(DjCueInput::default()).await.is_none());
    }
    fixture.clock.advance(6_000);
    let cue = fixture.cue(DjCueInput::default()).await;
    assert!(cue.is_none(), "{cue:?}");
    fixture.engine.shutdown().await.unwrap();
}

/// A track started from search is named in its signal, although the player learns what it is
/// only once it plays.
#[tokio::test]
async fn a_track_started_from_search_is_named_in_its_signal() {
    const SEARCHED: &str = "fixture-searched";
    let mut fixture = Harness::new().await;
    fixture.long_queue(&[FIRST, SECOND]).await;
    let quiet = DjCueInput {
        chattiness: Some(DjChattiness::Off),
        ..Default::default()
    };
    assert!(fixture.cue(quiet).await.is_none());
    fixture.clock.advance(1_000);
    let target = ytm_protocol::PlayerCommandArgs {
        target: Some(ytm_protocol::PlaybackRef {
            video_id: VideoId::new(SEARCHED).unwrap(),
            playlist_id: None,
            playlist_entry_id: None,
            browse_id: None,
            queue_entry_id: None,
            start_index: None,
        }),
        ..no_args()
    };
    fixture
        .by_hand("fixture-play-searched", PlayerOp::PlayTarget, Some(target))
        .await;
    assert!(fixture.cue(DjCueInput::default()).await.is_none());
    // The page plays it as a queue of its own.
    fixture.queue.entries = vec![QueueEntry {
        entry_id: QueueEntryId::new("fixture-searched-entry").unwrap(),
        video_id: VideoId::new(SEARCHED).unwrap(),
        track: track(SEARCHED, Some(240_000)),
        availability: TrackAvailability::Available,
    }];
    fixture.clock.advance(1_000);
    fixture.play_from(0, 0).await;
    fixture.clock.advance(5_000);
    fixture.observe_at(0, PlaybackStatus::Playing, 5_000).await;
    let signal = fixture.cue(DjCueInput::default()).await.unwrap();
    assert_eq!(signal.kind, DjCueKind::ListenerSignal);
    assert_eq!(signal.reasons, [DjCueReason::ManualPick]);
    assert!(
        signal.prompt.contains(&format!(
            "By hand, the listener just started \"{SEARCHED}\"."
        )),
        "{}",
        signal.prompt
    );
    fixture.engine.shutdown().await.unwrap();
}

/// Two quick play/pause presses while paused run as a play and then a pause, and are logged so.
#[tokio::test]
async fn two_quick_play_pause_presses_are_logged_as_they_run() {
    use ytm_control_api::ListenerActionKind as Kind;
    let mut fixture = Harness::new().await;
    fixture.long_queue(&[FIRST, SECOND]).await;
    fixture.observe_at(0, PlaybackStatus::Paused, 10_000).await;
    fixture
        .by_hand("fixture-toggle-1", PlayerOp::Toggle, None)
        .await;
    fixture
        .by_hand("fixture-toggle-2", PlayerOp::Toggle, None)
        .await;
    assert_eq!(
        action_kinds(&fixture.context_actions().await),
        [Kind::Pause, Kind::Play]
    );
    fixture.engine.shutdown().await.unwrap();
}

/// A prompt taken in the middle of a volume drag leaves the drag out; a later prompt lists it
/// once, from its first value to its last.
#[tokio::test]
async fn a_volume_drag_under_way_reaches_a_client_once_when_it_ends() {
    use ytm_control_api::ListenerActionKind as Kind;
    let mut fixture = Harness::new().await;
    fixture.long_queue(&[FIRST, SECOND]).await;
    let quiet = DjCueInput {
        chattiness: Some(DjChattiness::Off),
        ..Default::default()
    };
    assert!(fixture.cue(quiet).await.is_none());
    let other = ReadGrant::new(
        "fixture-other-agent".into(),
        Some(AccountScope::new(ACCOUNT).unwrap()),
    );
    let one = ytm_protocol::PlayerCommandArgs {
        repeat: Some(ytm_protocol::RepeatMode::One),
        ..no_args()
    };
    fixture
        .by_hand("fixture-repeat-one", PlayerOp::SetRepeat, Some(one))
        .await;
    fixture.clock.advance(6_000);
    let signal = fixture.cue(DjCueInput::default()).await.unwrap();
    assert_eq!(signal.kind, DjCueKind::ListenerSignal);

    let volume = |to| ytm_protocol::PlayerCommandArgs {
        volume: Some(to),
        ..no_args()
    };
    fixture
        .by_hand("fixture-drag-0", PlayerOp::SetVolume, Some(volume(0.6)))
        .await;
    fixture.clock.advance(1_000);
    let mid_drag = fixture.cue_as(&other, DjCueInput::default()).await.unwrap();
    assert_eq!(mid_drag.seq, signal.seq);
    assert_eq!(
        action_kinds(&mid_drag.listener.recent_actions),
        [Kind::Repeat]
    );
    fixture.clock.advance(1_000);
    fixture
        .by_hand("fixture-drag-1", PlayerOp::SetVolume, Some(volume(0.8)))
        .await;

    let mut ending = None;
    for position in [100_000, 200_000] {
        fixture.clock.advance(100_000);
        fixture
            .observe_at(0, PlaybackStatus::Playing, position)
            .await;
        ending = fixture.cue_as(&other, DjCueInput::default()).await;
        if ending.is_some() {
            break;
        }
    }
    let ending = ending.expect("expected the track-ending prompt");
    assert_eq!(ending.kind, DjCueKind::TrackEnding);
    let actions = &ending.listener.recent_actions;
    assert_eq!(action_kinds(actions), [Kind::Volume]);
    assert_eq!(
        (actions[0].volume_from, actions[0].volume_to),
        (Some(50), Some(80))
    );
    fixture.engine.shutdown().await.unwrap();
}

/// Holds each account write until the test lets it finish, with a scripted outcome.
struct GatedCatalog {
    outcome: std::sync::Mutex<ytm_protocol::MutationOutcome>,
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

const GATED_OTHER_EPOCH: &str = "fixture-gated-other-epoch";
const GATED_OTHER_ACCOUNT: &str = "fixture-gated-other-account";

impl CatalogPort for GatedCatalog {
    fn query(
        &self,
        _epoch: Option<AccountEpoch>,
        _scope: Option<AccountScope>,
        _request: CatalogQueryRequest,
        _cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<CatalogResponsePayload, QueryError>> {
        Box::pin(async { panic!("an account write test reads no catalog") })
    }

    fn mutate(
        &self,
        _epoch: AccountEpoch,
        _scope: AccountScope,
        request: MutationRequest,
        _cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<MutationResult, QueryError>> {
        Box::pin(async move {
            self.started.notify_one();
            self.release.notified().await;
            Ok(MutationResult::new(
                request.kind,
                *self.outcome.lock().unwrap(),
                None,
            ))
        })
    }

    fn apply_session_material(
        &self,
        material: SessionMaterialPayload,
        _cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<CatalogIdentity, QueryError>> {
        let account = if material.account_epoch.as_str() == EPOCH {
            ACCOUNT
        } else {
            GATED_OTHER_ACCOUNT
        };
        Box::pin(async move {
            Ok(CatalogIdentity {
                account_scope: AccountScope::new(account).unwrap(),
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
        Box::pin(async { panic!("an account write test requests no lyrics") })
    }

    fn clear_session_material(&self, _epoch: Option<AccountEpoch>) -> Result<(), QueryError> {
        Ok(())
    }
}

fn gated_session(epoch: &str) -> SessionMaterialPayload {
    SessionMaterialPayload {
        account_epoch: AccountEpoch::new(epoch).unwrap(),
        credential_revision: CredentialRevision::new("fixture-gated-credentials").unwrap(),
        cookie_pairs: Default::default(),
        auth_user_index: None,
        channel_label: None,
    }
}

impl Harness {
    /// A signed-in player whose account writes go through `catalog`.
    async fn with_gated_catalog(catalog: Arc<GatedCatalog>) -> Self {
        let root = tempfile::tempdir().unwrap();
        let clock = Arc::new(MockClock::new(1_000));
        let (sender, commands) = mpsc::channel(8);
        let engine = CoreEngine::start(
            CoreDeps {
                clock: clock.clone(),
                bridge: Arc::new(Bridge(sender)),
                catalog: Some(catalog),
                ..Default::default()
            },
            EngineConfig::default(),
        );
        engine
            .on_bridge_ready(
                1,
                BridgeReadyPayload {
                    bridge_epoch: BridgeEpoch::new(BRIDGE).unwrap(),
                    bridge_version: "0.1.0".into(),
                    capabilities: CapabilitySet {
                        play_pause: CapabilitySupport::Supported,
                        queue_mirror: CapabilitySupport::Supported,
                        ..Default::default()
                    },
                    account_hint: Some(HINT.into()),
                    logged_in_observed: true,
                },
            )
            .await
            .unwrap();
        engine
            .on_session_material(gated_session(EPOCH))
            .await
            .unwrap();
        let service = Arc::new(ControlService::new(engine.clone()));
        let read = ReadGrant::new(
            "fixture-dj-client".into(),
            Some(AccountScope::new(ACCOUNT).unwrap()),
        );
        Self {
            root,
            clock,
            engine,
            service,
            read,
            write: Arc::new(Grant { queue: false }),
            commands,
            seq: 1,
            queue: QueueSnapshot {
                bridge_epoch: Some(BridgeEpoch::new(BRIDGE).unwrap()),
                queue_revision: 0,
                current_entry_id: None,
                loaded_range: None,
                completeness: QueueCompleteness::Complete,
                has_more: Some(false),
                explicit_complete: None,
                may_load_more: None,
                official_upcoming: None,
                filters: None,
                entries: Vec::new(),
            },
        }
    }

    /// Starts a like in the app without waiting for the catalog to answer.
    async fn start_like(
        &self,
        catalog: &GatedCatalog,
        video: &str,
    ) -> tokio::task::JoinHandle<Result<MutationResult, QueryError>> {
        let engine = self.engine.clone();
        let request = MutationRequest {
            kind: ytm_protocol::MutationKind::SetLike,
            video_id: VideoId::new(video).unwrap(),
            liked: Some(true),
            playlist_id: None,
            undo_token: None,
            confirmed_by_user: true,
        };
        let running = tokio::spawn(async move {
            engine
                .mutate(request, Some(AccountEpoch::new(EPOCH).unwrap()))
                .await
        });
        timeout(WAIT, catalog.started.notified())
            .await
            .expect("the like did not reach the catalog");
        running
    }
}

/// A like made in the app is logged as of the click, with the track playing and the position
/// then, even when the next track started while the account answered. A refused like, or one
/// whose account changed meanwhile, is not logged.
#[tokio::test]
async fn a_like_in_the_app_is_logged_as_of_the_click_and_only_when_it_holds() {
    use ytm_control_api::ListenerActionKind as Kind;
    use ytm_protocol::MutationOutcome;
    let catalog = Arc::new(GatedCatalog {
        outcome: std::sync::Mutex::new(MutationOutcome::Confirmed),
        started: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let mut fixture = Harness::with_gated_catalog(catalog.clone()).await;
    fixture.long_queue(&[FIRST, SECOND]).await;
    fixture.clock.advance(30_000);
    fixture.observe_at(0, PlaybackStatus::Playing, 30_000).await;

    let like = fixture.start_like(&catalog, FIRST).await;
    fixture.clock.advance(3_000);
    fixture.play_from(1, 0).await;
    catalog.release.notify_one();
    let result = like.await.unwrap().unwrap();
    assert_eq!(result.outcome, MutationOutcome::Confirmed);
    let actions = fixture.context_actions().await;
    assert_eq!(action_kinds(&actions), [Kind::Rate]);
    let liked = &actions[0];
    assert_eq!(liked.value.as_deref(), Some("liked"));
    assert_eq!(liked.during.as_ref().unwrap().title, FIRST);
    assert_eq!(liked.subject.as_ref().unwrap().title, FIRST, "{liked:?}");
    assert_eq!(liked.position_ms, Some(30_000));
    assert!(liked.ago_ms >= 3_000, "{}", liked.ago_ms);

    *catalog.outcome.lock().unwrap() = MutationOutcome::Rejected;
    let refused = fixture.start_like(&catalog, SECOND).await;
    catalog.release.notify_one();
    let result = refused.await.unwrap().unwrap();
    assert_eq!(result.outcome, MutationOutcome::Rejected);
    assert_eq!(action_kinds(&fixture.context_actions().await), [Kind::Rate]);

    *catalog.outcome.lock().unwrap() = MutationOutcome::Confirmed;
    let switched = fixture.start_like(&catalog, SECOND).await;
    fixture
        .engine
        .on_session_material(gated_session(GATED_OTHER_EPOCH))
        .await
        .unwrap();
    catalog.release.notify_one();
    let _ = switched.await.unwrap();
    fixture.read = ReadGrant::new(
        "fixture-dj-client".into(),
        Some(AccountScope::new(GATED_OTHER_ACCOUNT).unwrap()),
    );
    let actions = fixture.context_actions().await;
    assert!(actions.is_empty(), "{actions:?}");
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

#[tokio::test]
async fn f1_redaction_f5_replay_and_moment_dj_contract() {
    use ytm_control_api::{
        DjFeedbackChange, DjFeedbackTarget, DjPreferenceStance, ListeningIntentChange,
        ListeningIntentDuration, ListeningIntentInstruction, MomentSectionStatus,
        OperationReplayInput, ReportedIntentBasis, VocalsMode, compute_action_hashes,
    };
    use ytm_core::control::journal::{OperationKey, OperationStore};
    use ytm_protocol::{
        HostEnvironmentSnapshotPayload, HostOccasion, MomentDesktopRequest, OwnerMomentConfigPatch,
        PrincipalShareUpdate,
    };

    let dir = TempDir::new().unwrap();
    let clock = Arc::new(MockClock::with_wall_offset(1_000, 1_760_000_000_000));
    let storage = open_storage(&dir, clock.clone());
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
            credential_revision: CredentialRevision::new("cred-moment-1").unwrap(),
            cookie_pairs: Default::default(),
            auth_user_index: None,
            channel_label: None,
        })
        .await
        .unwrap();
    let service = Arc::new(
        ControlService::new(engine.clone())
            .with_operations(storage.clone())
            .with_dj_store(storage.clone())
            .with_moment_store(storage.clone()),
    );
    let agent = ReadGrant::new(
        "fixture-agent".to_string(),
        Some(AccountScope::new(ACCOUNT).unwrap()),
    );
    let other_agent = ReadGrant::new(
        "fixture-other".to_string(),
        Some(AccountScope::new(ACCOUNT).unwrap()),
    );

    // 1. F1 + F5: SetDjFeedback note is scrubbed from settled operation journal while kept on
    // DjPreference and ReplayOperation hash check still validates original intent.
    struct DjGrant;
    impl DispatchAuthority for DjGrant {
        fn authorize(
            &self,
            _action: GrantAction,
        ) -> BoxFuture<'_, Result<DispatchGrant, ControlError>> {
            Box::pin(async {
                Ok(DispatchGrant {
                    deadline_wall_ms: u64::MAX,
                    editable_playlists: vec![],
                })
            })
        }
    }

    let action = WriteAction::SetDjFeedback {
        change: DjFeedbackChange::Record {
            target: DjFeedbackTarget::Video {
                video_id: VideoId::new("fixture-song-1").unwrap(),
            },
            stance: DjPreferenceStance::Prefer,
            note: Some("loves midnight synthwave".to_owned()),
        },
    };
    let (exact_hash, _) = compute_action_hashes(&action);
    let receipt = service
        .submit(
            agent.clone(),
            Arc::new(DjGrant),
            WriteInput {
                operation_id: "op-pref-1".into(),
                preconditions: Preconditions {
                    expected_account_epoch: Some(AccountEpoch::new(EPOCH).unwrap()),
                    expected_bridge_epoch: None,
                    expected_queue_revision: None,
                    expected_entity_revision: None,
                },
                action: action.clone(),
            },
        )
        .await
        .unwrap();
    assert_eq!(receipt.operation_id, "op-pref-1");
    timeout(WAIT, async {
        loop {
            if let ControlResponse::Operation(op) = service
                .execute(
                    &agent,
                    ControlRequest::GetOperation(OperationInput {
                        operation_id: "op-pref-1".into(),
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

    // Settled request in control_operations must NOT retain note text.
    let stored_req = storage
        .load_request(OperationKey {
            principal: "fixture-agent".into(),
            account_scope: Some(AccountScope::new(ACCOUNT).unwrap()),
            operation_id: "op-pref-1".into(),
        })
        .await
        .unwrap()
        .unwrap();
    let WriteAction::SetDjFeedback {
        change: DjFeedbackChange::Record { note, .. },
    } = stored_req.action
    else {
        panic!("expected SetDjFeedback");
    };
    assert_eq!(note, None, "settled operation journal must scrub note");

    // Desktop activity shows the preference note verbatim and readable operation summary.
    let activity = service
        .desktop_activity(
            Some(AccountEpoch::new(EPOCH).unwrap()),
            ytm_protocol::AgentActivityRequest::Read,
        )
        .await
        .unwrap();
    assert_eq!(
        activity.preferences[0].note.as_deref(),
        Some("loves midnight synthwave")
    );
    assert_eq!(
        activity.operations[0].action.as_deref(),
        Some("rememberTaste")
    );

    // F5 ReplayOperation accepts identical hash and rejects altered parameters.
    let ControlResponse::Operation(replayed) = service
        .execute(
            &agent,
            ControlRequest::ReplayOperation(OperationReplayInput {
                operation_id: "op-pref-1".into(),
                action_hash: exact_hash,
                token_was_missing: false,
                explicit_preconditions: None,
            }),
        )
        .await
    else {
        panic!("expected matching replay receipt");
    };
    assert_eq!(replayed.operation_id, "op-pref-1");

    let ControlResponse::Error(conflict) = service
        .execute(
            &agent,
            ControlRequest::ReplayOperation(OperationReplayInput {
                operation_id: "op-pref-1".into(),
                action_hash: "0".repeat(64),
                token_was_missing: false,
                explicit_preconditions: None,
            }),
        )
        .await
    else {
        panic!("expected IdempotencyConflict");
    };
    assert_eq!(conflict.code, ErrorCode::IdempotencyConflict);

    // 2. Moment DJ defaults to notShared for every client.
    let ControlResponse::Context(initial_ctx) = service
        .execute(
            &agent,
            ControlRequest::GetContext(ContextInput {
                preview_limit: Some(0),
            }),
        )
        .await
    else {
        panic!("expected context");
    };
    let m0 = initial_ctx.moment.unwrap();
    assert_eq!(m0.environment.status, MomentSectionStatus::NotShared);
    assert_eq!(m0.listening_intents.status, MomentSectionStatus::NotShared);

    // Unshared client cannot write listening intents.
    let _ = service
        .submit(
            agent.clone(),
            Arc::new(DjGrant),
            WriteInput {
                operation_id: "op-intent-unshared".into(),
                preconditions: Preconditions {
                    expected_account_epoch: Some(AccountEpoch::new(EPOCH).unwrap()),
                    expected_bridge_epoch: None,
                    expected_queue_revision: None,
                    expected_entity_revision: None,
                },
                action: WriteAction::SetListeningIntent {
                    change: ListeningIntentChange::Set {
                        instruction: ListeningIntentInstruction::Vocals {
                            mode: VocalsMode::PreferInstrumental,
                        },
                        duration: ListeningIntentDuration::Session,
                        basis: ReportedIntentBasis::UserRequest,
                        expected_moment_revision: None,
                    },
                },
            },
        )
        .await
        .unwrap();
    timeout(WAIT, async {
        loop {
            if let ControlResponse::Operation(op) = service
                .execute(
                    &agent,
                    ControlRequest::GetOperation(OperationInput {
                        operation_id: "op-intent-unshared".into(),
                    }),
                )
                .await
                && op.settled
            {
                assert_eq!(op.phase, OperationPhase::Rejected);
                assert_eq!(op.error.unwrap().code, ErrorCode::PermissionDenied);
                return;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();

    // Owner enables Moment sharing for `fixture-agent`, time, holidays (US), and sceneCity.
    let desktop_snap = service
        .desktop_moment_settings(
            Some(AccountEpoch::new(EPOCH).unwrap()),
            MomentDesktopRequest::Update {
                patch: OwnerMomentConfigPatch {
                    share_time: Some(true),
                    holidays_enabled: Some(true),
                    holiday_country: Some("US".into()),
                    scene_city: Some("Seattle".into()),
                    set_principal_shared: Some(PrincipalShareUpdate {
                        principal: "fixture-agent".into(),
                        shared: true,
                    }),
                    ..Default::default()
                },
            },
            vec!["fixture-agent".into(), "fixture-other".into()],
        )
        .await
        .unwrap();
    let cfg_rev = desktop_snap.config.config_revision;
    assert_eq!(cfg_rev, 1);

    // Host delivers environment snapshot matching config_revision.
    let now_wall = clock.wall_now_ms();
    engine
        .on_host_environment_snapshot(HostEnvironmentSnapshotPayload {
            config_revision: cfg_rev,
            captured_at_ms: now_wall,
            valid_until_ms: now_wall + 120_000,
            clock_ok: true,
            calendar_ok: true,
            time_zone: Some("America/Los_Angeles".into()),
            local_date: Some("2026-12-24".into()),
            weekday: Some("thursday".into()),
            day_part: Some("evening".into()),
            day_ends_at_utc_ms: Some(now_wall + 14_400_000),
            occasions: vec![HostOccasion {
                id: "occasion:us:christmas-day".into(),
                label: "Christmas Day".into(),
                phase: "eve".into(),
                kind: "public".into(),
                local_date: "2026-12-24".into(),
                actual_date: Some("2026-12-25".into()),
                provider_version: "date-holidays@3.39.2".into(),
            }],
            countries: vec![],
        })
        .await
        .unwrap();

    // `fixture-agent` sees time, occasion, sceneCity, weather_provided = false; `fixture-other`
    // STILL sees `notShared`.
    let ControlResponse::Context(shared_ctx) = service
        .execute(
            &agent,
            ControlRequest::GetContext(ContextInput {
                preview_limit: Some(0),
            }),
        )
        .await
    else {
        panic!("expected context");
    };
    let moment = shared_ctx.moment.unwrap();
    assert_eq!(moment.environment.status, MomentSectionStatus::Shared);
    assert_eq!(moment.environment.occasions.len(), 1);
    assert_eq!(
        moment.environment.scene_city.as_ref().unwrap().label,
        "Seattle"
    );
    assert!(!moment.environment.weather_provided);

    let ControlResponse::Context(other_ctx) = service
        .execute(
            &other_agent,
            ControlRequest::GetContext(ContextInput {
                preview_limit: Some(0),
            }),
        )
        .await
    else {
        panic!("expected context");
    };
    assert_eq!(
        other_ctx.moment.unwrap().environment.status,
        MomentSectionStatus::NotShared
    );

    // Owner preview matches `fixture-agent` projection.
    let owner_preview = service
        .desktop_moment_settings(
            Some(AccountEpoch::new(EPOCH).unwrap()),
            MomentDesktopRequest::Read {
                preview_principal: Some("fixture-agent".into()),
            },
            vec!["fixture-agent".into()],
        )
        .await
        .unwrap();
    assert_eq!(
        owner_preview.preview_json["environment"]["sceneCity"]["label"],
        "Seattle"
    );

    // Set `today` intent -> locks concrete `dayEndsAtUtcMs`, and `DismissTheme` hides occasion!
    let _ = service
        .submit(
            agent.clone(),
            Arc::new(DjGrant),
            WriteInput {
                operation_id: "op-intent-dismiss".into(),
                preconditions: Preconditions {
                    expected_account_epoch: Some(AccountEpoch::new(EPOCH).unwrap()),
                    expected_bridge_epoch: None,
                    expected_queue_revision: None,
                    expected_entity_revision: None,
                },
                action: WriteAction::SetListeningIntent {
                    change: ListeningIntentChange::Set {
                        instruction: ListeningIntentInstruction::DismissTheme {
                            theme_id: "occasion:us:christmas-day".into(),
                        },
                        duration: ListeningIntentDuration::Today,
                        basis: ReportedIntentBasis::UserRequest,
                        expected_moment_revision: Some(moment.revision),
                    },
                },
            },
        )
        .await
        .unwrap();
    timeout(WAIT, async {
        loop {
            if let ControlResponse::Operation(op) = service
                .execute(
                    &agent,
                    ControlRequest::GetOperation(OperationInput {
                        operation_id: "op-intent-dismiss".into(),
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

    let ControlResponse::Context(after_dismiss) = service
        .execute(
            &agent,
            ControlRequest::GetContext(ContextInput {
                preview_limit: Some(0),
            }),
        )
        .await
    else {
        panic!("expected context");
    };
    let after_moment = after_dismiss.moment.unwrap();
    assert!(
        after_moment.environment.occasions.is_empty(),
        "DismissTheme(today) must hide the matching occasion"
    );
    assert_eq!(after_moment.listening_intents.items.len(), 1);
    assert_eq!(
        after_moment.listening_intents.items[0].expires_at_ms,
        now_wall + 14_400_000
    );

    // Assistant inference cannot overwrite userRequestReportedByAssistant on same key.
    let _ = service
        .submit(
            agent.clone(),
            Arc::new(DjGrant),
            WriteInput {
                operation_id: "op-intent-clobber".into(),
                preconditions: Preconditions {
                    expected_account_epoch: Some(AccountEpoch::new(EPOCH).unwrap()),
                    expected_bridge_epoch: None,
                    expected_queue_revision: None,
                    expected_entity_revision: None,
                },
                action: WriteAction::SetListeningIntent {
                    change: ListeningIntentChange::Clear {
                        intent_key: "dismissTheme:occasion:us:christmas-day".into(),
                        intent_id: None,
                        basis: ReportedIntentBasis::Inference,
                        expected_moment_revision: Some(after_moment.revision),
                    },
                },
            },
        )
        .await
        .unwrap();
    timeout(WAIT, async {
        loop {
            if let ControlResponse::Operation(op) = service
                .execute(
                    &agent,
                    ControlRequest::GetOperation(OperationInput {
                        operation_id: "op-intent-clobber".into(),
                    }),
                )
                .await
                && op.settled
            {
                assert_eq!(op.phase, OperationPhase::Rejected);
                assert_eq!(op.error.unwrap().code, ErrorCode::Conflict);
                return;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();

    // A "today" written after the snapshot itself expired still ends at the same local midnight.
    clock.advance(200_000);
    let _ = service
        .submit(
            agent.clone(),
            Arc::new(DjGrant),
            WriteInput {
                operation_id: "op-intent-late-today".into(),
                preconditions: Preconditions {
                    expected_account_epoch: Some(AccountEpoch::new(EPOCH).unwrap()),
                    expected_bridge_epoch: None,
                    expected_queue_revision: None,
                    expected_entity_revision: None,
                },
                action: WriteAction::SetListeningIntent {
                    change: ListeningIntentChange::Set {
                        instruction: ListeningIntentInstruction::Vocals {
                            mode: VocalsMode::PreferInstrumental,
                        },
                        duration: ListeningIntentDuration::Today,
                        basis: ReportedIntentBasis::UserRequest,
                        expected_moment_revision: None,
                    },
                },
            },
        )
        .await
        .unwrap();
    timeout(WAIT, async {
        loop {
            if let ControlResponse::Operation(op) = service
                .execute(
                    &agent,
                    ControlRequest::GetOperation(OperationInput {
                        operation_id: "op-intent-late-today".into(),
                    }),
                )
                .await
                && op.settled
            {
                assert_eq!(op.phase, OperationPhase::Verified, "{op:?}");
                return;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();

    // Races on a decision: an inference must say which Moment it was decided on, and a clear of
    // an intent read earlier leaves the one that replaced it.
    let intent_write = |id: &str, change: ListeningIntentChange| WriteInput {
        operation_id: id.into(),
        preconditions: Preconditions {
            expected_account_epoch: Some(AccountEpoch::new(EPOCH).unwrap()),
            expected_bridge_epoch: None,
            expected_queue_revision: None,
            expected_entity_revision: None,
        },
        action: WriteAction::SetListeningIntent { change },
    };
    let settle = |id: &'static str| {
        let service = &service;
        let agent = &agent;
        async move {
            timeout(WAIT, async {
                loop {
                    let op = operation(service, agent, id).await;
                    if op.settled {
                        return op;
                    }
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            })
            .await
            .unwrap()
        }
    };
    let energy = |level| ListeningIntentChange::Set {
        instruction: ListeningIntentInstruction::Energy { level },
        duration: ListeningIntentDuration::Session,
        basis: ReportedIntentBasis::UserRequest,
        expected_moment_revision: None,
    };
    let read_intents = || async {
        let ControlResponse::Context(ctx) = service
            .execute(
                &agent,
                ControlRequest::GetContext(ContextInput {
                    preview_limit: Some(0),
                }),
            )
            .await
        else {
            panic!("expected context");
        };
        ctx.moment.unwrap()
    };
    let before_inference = read_intents().await;
    service
        .submit(
            agent.clone(),
            Arc::new(DjGrant),
            intent_write(
                "op-inference-no-revision",
                ListeningIntentChange::Set {
                    instruction: ListeningIntentInstruction::Energy {
                        level: ytm_control_api::EnergyLevel::Gentle,
                    },
                    duration: ListeningIntentDuration::Session,
                    basis: ReportedIntentBasis::Inference,
                    expected_moment_revision: None,
                },
            ),
        )
        .await
        .unwrap();
    let op = settle("op-inference-no-revision").await;
    assert_eq!(op.phase, OperationPhase::Rejected);
    assert_eq!(op.error.unwrap().code, ErrorCode::InvalidInput);
    // Decided on a Moment the listener changed since: stale, not applied. The inference names
    // another dimension than the listener's change, so only the revision can refuse it.
    service
        .submit(
            agent.clone(),
            Arc::new(DjGrant),
            intent_write(
                "op-energy-steady",
                energy(ytm_control_api::EnergyLevel::Steady),
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        settle("op-energy-steady").await.phase,
        OperationPhase::Verified
    );
    service
        .submit(
            agent.clone(),
            Arc::new(DjGrant),
            intent_write(
                "op-inference-stale",
                ListeningIntentChange::Set {
                    instruction: ListeningIntentInstruction::Activity {
                        kind: ytm_control_api::ActivityKind::Focus,
                    },
                    duration: ListeningIntentDuration::Session,
                    basis: ReportedIntentBasis::Inference,
                    expected_moment_revision: Some(before_inference.revision),
                },
            ),
        )
        .await
        .unwrap();
    let op = settle("op-inference-stale").await;
    assert_eq!(op.phase, OperationPhase::Rejected);
    let error = op.error.unwrap();
    assert_eq!(error.code, ErrorCode::Conflict);
    assert!(error.message.contains("read again"), "{error:?}");

    let steady = read_intents().await;
    let first_energy = steady
        .listening_intents
        .items
        .iter()
        .find(|item| matches!(item.instruction, ListeningIntentInstruction::Energy { .. }))
        .unwrap()
        .clone();
    service
        .submit(
            agent.clone(),
            Arc::new(DjGrant),
            intent_write(
                "op-energy-high",
                energy(ytm_control_api::EnergyLevel::Driving),
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        settle("op-energy-high").await.phase,
        OperationPhase::Verified
    );
    // Clearing the energy read before the replacement keeps the new one.
    service
        .submit(
            agent.clone(),
            Arc::new(DjGrant),
            intent_write(
                "op-clear-old-energy",
                ListeningIntentChange::Clear {
                    intent_key: first_energy.intent_key.clone(),
                    intent_id: Some(first_energy.intent_id.clone()),
                    basis: ReportedIntentBasis::UserRequest,
                    expected_moment_revision: None,
                },
            ),
        )
        .await
        .unwrap();
    let op = settle("op-clear-old-energy").await;
    assert_eq!(op.phase, OperationPhase::Rejected);
    assert_eq!(op.error.unwrap().code, ErrorCode::Conflict);
    let owner_clear = service
        .desktop_activity(
            Some(AccountEpoch::new(EPOCH).unwrap()),
            AgentActivityRequest::ClearListeningIntent {
                intent_key: first_energy.intent_key.clone(),
                intent_id: Some(first_energy.intent_id.clone()),
            },
        )
        .await;
    assert!(owner_clear.is_err(), "{owner_clear:?}");
    let replaced = read_intents().await;
    let kept = replaced
        .listening_intents
        .items
        .iter()
        .find(|item| item.intent_key == first_energy.intent_key)
        .expect("the replacement stays");
    assert_ne!(kept.intent_id, first_energy.intent_id);
    assert_eq!(
        kept.instruction,
        ListeningIntentInstruction::Energy {
            level: ytm_control_api::EnergyLevel::Driving
        }
    );

    // The owner stops sharing intents: the theme dismissed for today stays out of the occasions.
    let cfg_rev = service
        .desktop_moment_settings(
            Some(AccountEpoch::new(EPOCH).unwrap()),
            MomentDesktopRequest::Update {
                patch: OwnerMomentConfigPatch {
                    share_listening_intents: Some(false),
                    ..Default::default()
                },
            },
            vec!["fixture-agent".into()],
        )
        .await
        .unwrap()
        .config
        .config_revision;
    let later = clock.wall_now_ms();
    engine
        .on_host_environment_snapshot(HostEnvironmentSnapshotPayload {
            config_revision: cfg_rev,
            captured_at_ms: later,
            valid_until_ms: later + 120_000,
            clock_ok: true,
            calendar_ok: false,
            time_zone: Some("America/Los_Angeles".into()),
            local_date: Some("2026-12-24".into()),
            weekday: Some("thursday".into()),
            day_part: Some("evening".into()),
            day_ends_at_utc_ms: Some(now_wall + 14_400_000),
            occasions: vec![],
            countries: vec![],
        })
        .await
        .unwrap();
    let read_moment = || async {
        let ControlResponse::Context(ctx) = service
            .execute(
                &agent,
                ControlRequest::GetContext(ContextInput {
                    preview_limit: Some(0),
                }),
            )
            .await
        else {
            panic!("expected context");
        };
        ctx.moment.unwrap()
    };
    // A calendar failure leaves the clock shared; only the occasions are missing.
    let calendar_down = read_moment().await;
    assert_eq!(
        calendar_down.environment.status,
        MomentSectionStatus::Shared
    );
    assert!(calendar_down.environment.time.is_some());
    assert_eq!(calendar_down.environment.captured_at_ms, Some(later));
    assert_eq!(
        calendar_down.listening_intents.status,
        MomentSectionStatus::NotShared
    );
    engine
        .on_host_environment_snapshot(HostEnvironmentSnapshotPayload {
            config_revision: cfg_rev,
            captured_at_ms: later,
            valid_until_ms: later + 120_000,
            clock_ok: true,
            calendar_ok: true,
            time_zone: Some("America/Los_Angeles".into()),
            local_date: Some("2026-12-24".into()),
            weekday: Some("thursday".into()),
            day_part: Some("evening".into()),
            day_ends_at_utc_ms: Some(now_wall + 14_400_000),
            occasions: vec![HostOccasion {
                id: "occasion:us:christmas-day".into(),
                label: "Christmas Day".into(),
                phase: "eve".into(),
                kind: "public".into(),
                local_date: "2026-12-24".into(),
                actual_date: Some("2026-12-25".into()),
                provider_version: "date-holidays@3.39.2".into(),
            }],
            countries: vec![],
        })
        .await
        .unwrap();
    assert!(read_moment().await.environment.occasions.is_empty());

    // Revoking the assistant turns everything off at its next read.
    service
        .desktop_moment_settings(
            Some(AccountEpoch::new(EPOCH).unwrap()),
            MomentDesktopRequest::Update {
                patch: OwnerMomentConfigPatch {
                    set_principal_shared: Some(PrincipalShareUpdate {
                        principal: "fixture-agent".into(),
                        shared: false,
                    }),
                    ..Default::default()
                },
            },
            vec!["fixture-agent".into()],
        )
        .await
        .unwrap();
    let revoked = read_moment().await;
    assert_eq!(revoked.environment.status, MomentSectionStatus::NotShared);
    assert_eq!(
        revoked.listening_intents.status,
        MomentSectionStatus::NotShared
    );

    engine.shutdown().await.unwrap();
}
