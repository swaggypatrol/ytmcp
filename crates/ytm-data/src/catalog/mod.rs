use rand::RngExt as _;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::debug;
use ytm_core::ports::{BoxFuture, CatalogIdentity, CatalogPort, OfficialLyrics};
use ytm_protocol::{
    AccountEpoch, AccountScope, CatalogQueryRequest, CatalogResponsePayload, MutationRequest,
    MutationResult, QueryError, QueryErrorCode, SessionMaterialPayload,
};

use crate::cache::{CatalogQueryKey, is_retryable_read_error};
pub use crate::probe::{YtmCatalogAdapter, YtmCatalogConfig};

/// Configuration for `CatalogService` read deduplication and transient backoff.
#[derive(Debug, Clone)]
pub struct CatalogServiceConfig {
    pub max_read_retries: u32,
    pub base_backoff: Duration,
    pub max_backoff: Duration,
}

impl Default for CatalogServiceConfig {
    fn default() -> Self {
        Self {
            max_read_retries: 2,
            base_backoff: Duration::from_millis(120),
            max_backoff: Duration::from_secs(2),
        }
    }
}

type ReadSender = watch::Sender<Option<Result<CatalogResponsePayload, QueryError>>>;
type InflightReads = Arc<Mutex<HashMap<CatalogQueryKey, ReadSender>>>;

struct ReadFlight {
    key: CatalogQueryKey,
    reads: InflightReads,
    tx: ReadSender,
}

impl Drop for ReadFlight {
    fn drop(&mut self) {
        let mut reads = self.reads.lock().unwrap_or_else(|err| err.into_inner());
        if reads
            .get(&self.key)
            .is_some_and(|tx| tx.same_channel(&self.tx))
        {
            reads.remove(&self.key);
        }
    }
}

/// Production catalog facade wrapping `YtmCatalogAdapter` with:
/// 1. In-flight read request deduplication per `(account_epoch, account_scope, request)`
/// 2. Bounded read-only exponential backoff + jitter on transient upstream errors
/// 3. Strictly zero automatic retries on `MutationRequest` operations
#[derive(Clone)]
pub struct CatalogService {
    adapter: Arc<YtmCatalogAdapter>,
    config: CatalogServiceConfig,
    inflight_reads: InflightReads,
}

impl CatalogService {
    pub fn new(adapter: Arc<YtmCatalogAdapter>) -> Self {
        Self::with_config(adapter, CatalogServiceConfig::default())
    }

    pub fn with_config(adapter: Arc<YtmCatalogAdapter>, config: CatalogServiceConfig) -> Self {
        Self {
            adapter,
            config,
            inflight_reads: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn adapter(&self) -> &Arc<YtmCatalogAdapter> {
        &self.adapter
    }

    pub fn set_network_offline(&self, offline: bool) {
        self.adapter.set_network_offline(offline);
    }

    fn compute_retry_delay(&self, attempt: u32, err: &QueryError) -> Duration {
        if let Some(hint_ms) = err.retry_after_ms {
            let clamped = hint_ms.clamp(50, self.config.max_backoff.as_millis() as u64);
            return Duration::from_millis(clamped);
        }
        let exp = 1_u64.checked_shl(attempt.min(8)).unwrap_or(256);
        let base_ms = (self.config.base_backoff.as_millis() as u64).saturating_mul(exp);
        let jitter_ms = rand::rng().random_range(0..=50_u64);
        let total_ms = base_ms
            .saturating_add(jitter_ms)
            .min(self.config.max_backoff.as_millis() as u64);
        Duration::from_millis(total_ms)
    }

    async fn execute_read_with_retry(
        &self,
        account_epoch: Option<AccountEpoch>,
        account_scope: Option<AccountScope>,
        request: CatalogQueryRequest,
    ) -> Result<CatalogResponsePayload, QueryError> {
        let cancel = self
            .adapter
            .query_session_cancel(account_epoch.as_ref(), account_scope.as_ref())?;
        let mut attempt = 0_u32;
        loop {
            let res = self
                .adapter
                .query(
                    account_epoch.clone(),
                    account_scope.clone(),
                    request.clone(),
                    cancel.clone(),
                )
                .await;

            match res {
                Ok(payload) => return Ok(payload),
                Err(err) => {
                    if attempt >= self.config.max_read_retries || !is_retryable_read_error(&err) {
                        return Err(err);
                    }
                    let delay = self.compute_retry_delay(attempt, &err);
                    debug!(
                        kind = ?request.kind,
                        attempt,
                        delay_ms = delay.as_millis(),
                        code = ?err.code,
                        "retrying transient catalog read query"
                    );
                    tokio::select! {
                        _ = cancel.cancelled() => {
                            return Err(QueryError {
                                code: QueryErrorCode::StaleContext,
                                retriable: false,
                                retry_after_ms: None,
                                safe_message: "catalog query cancelled during retry backoff".to_string(),
                                request_id: None,
                            });
                        }
                        _ = tokio::time::sleep(delay) => {}
                    }
                    attempt += 1;
                }
            }
        }
    }
}

impl CatalogPort for CatalogService {
    fn query(
        &self,
        account_epoch: Option<AccountEpoch>,
        account_scope: Option<AccountScope>,
        request: CatalogQueryRequest,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<CatalogResponsePayload, QueryError>> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err(QueryError {
                    code: QueryErrorCode::StaleContext,
                    retriable: false,
                    retry_after_ms: None,
                    safe_message: "catalog query cancelled before deduplication".to_string(),
                    request_id: None,
                });
            }

            let key = CatalogQueryKey::from_request(
                account_epoch.as_ref(),
                account_scope.as_ref(),
                &request,
            );

            let mut rx = {
                let mut reads = self.inflight_reads.lock().map_err(|_| QueryError {
                    code: QueryErrorCode::StorageError,
                    retriable: false,
                    retry_after_ms: None,
                    safe_message: "catalog inflight deduplicator lock poisoned".to_string(),
                    request_id: None,
                })?;
                // Subscribe before counting so a departing last caller cannot cancel a request
                // that this caller just joined. A flight with no other waiters is abandoned.
                let joined = reads.get(&key).and_then(|tx| {
                    let rx = tx.subscribe();
                    (tx.receiver_count() > 1).then_some(rx)
                });
                if let Some(rx) = joined {
                    rx
                } else {
                    let (tx, rx) = watch::channel(None);
                    reads.insert(key.clone(), tx.clone());
                    let flight = ReadFlight {
                        key,
                        reads: self.inflight_reads.clone(),
                        tx,
                    };
                    let service = self.clone();
                    tokio::spawn(async move {
                        let read =
                            service.execute_read_with_retry(account_epoch, account_scope, request);
                        tokio::select! {
                            biased;
                            _ = flight.tx.closed() => {}
                            outcome = read => { flight.tx.send_replace(Some(outcome)); }
                        }
                        drop(flight);
                    });
                    rx
                }
            };

            loop {
                if let Some(result) = rx.borrow().clone() {
                    return result;
                }
                tokio::select! {
                    _ = cancel.cancelled() => {
                        return Err(QueryError {
                            code: QueryErrorCode::StaleContext,
                            retriable: false,
                            retry_after_ms: None,
                            safe_message: "coalesced catalog query cancelled".to_string(),
                            request_id: None,
                        });
                    }
                    changed = rx.changed() => {
                        if changed.is_err() {
                            return Err(QueryError {
                                code: QueryErrorCode::StaleContext,
                                retriable: true,
                                retry_after_ms: Some(100),
                                safe_message: "coalesced catalog query dropped before completion".to_string(),
                                request_id: None,
                            });
                        }
                    }
                }
            }
        })
    }

    fn mutate(
        &self,
        account_epoch: AccountEpoch,
        account_scope: AccountScope,
        request: MutationRequest,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<MutationResult, QueryError>> {
        // Mutations are NEVER deduplicated or automatically retried.
        self.adapter
            .mutate(account_epoch, account_scope, request, cancel)
    }

    fn official_lyrics(
        &self,
        account_epoch: AccountEpoch,
        video_id: ytm_protocol::VideoId,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<Option<OfficialLyrics>, QueryError>> {
        self.adapter
            .official_lyrics(account_epoch, video_id, cancel)
    }

    fn apply_session_material(
        &self,
        material: SessionMaterialPayload,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<CatalogIdentity, QueryError>> {
        self.adapter.apply_session_material(material, cancel)
    }

    fn clear_memory_cache(&self, scope: Option<&AccountScope>) -> Result<(), QueryError> {
        self.adapter.clear_memory_cache(scope)
    }

    fn clear_session_material(
        &self,
        account_epoch: Option<AccountEpoch>,
    ) -> Result<(), QueryError> {
        self.adapter.clear_session_material(account_epoch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::{BackendPage, BackendPlaylistPage, CatalogBackend, CatalogSession};
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use ytm_core::ports::MockClock;
    use ytm_protocol::{
        ArtistRef, CatalogQueryKind, CatalogResponsePayload, MutationKind, PlaylistId, Track,
        TrackAvailability, VideoId,
    };
    use ytm_storage::{CoverCacheConfig, SqliteStorage, StoragePaths};

    #[derive(Default)]
    struct FlakyDedupBackend {
        pending: Option<(tokio::sync::Notify, tokio::sync::Notify)>,
        search_calls: AtomicUsize,
        fail_times: AtomicUsize,
        delay_ms: u64,
    }

    struct DummySess;
    impl CatalogSession for DummySess {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    impl CatalogBackend for FlakyDedupBackend {
        fn authenticate<'a>(
            &'a self,
            _cookie_header: &'a str,
        ) -> BoxFuture<'a, Result<Arc<dyn CatalogSession>, QueryError>> {
            Box::pin(async { Ok(Arc::new(DummySess) as Arc<dyn CatalogSession>) })
        }

        fn fetch_datasync_id<'a>(
            &'a self,
            _session: &'a Arc<dyn CatalogSession>,
        ) -> BoxFuture<'a, Result<Option<String>, QueryError>> {
            Box::pin(async { Ok(Some("SyntheticUser".to_string())) })
        }

        fn search_songs<'a>(
            &'a self,
            _session: &'a Arc<dyn CatalogSession>,
            _query: &'a str,
            _continuation: Option<&'a str>,
        ) -> BoxFuture<'a, Result<BackendPage<Track>, QueryError>> {
            Box::pin(async move {
                let n = self.search_calls.fetch_add(1, Ordering::SeqCst);
                if let Some((started, release)) = &self.pending {
                    started.notify_one();
                    release.notified().await;
                }
                if self.delay_ms > 0 {
                    tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
                }
                let rem = self.fail_times.load(Ordering::SeqCst);
                if n < rem {
                    return Err(QueryError {
                        code: QueryErrorCode::RateLimited,
                        retriable: true,
                        retry_after_ms: Some(20),
                        safe_message: "synthetic 429".to_string(),
                        request_id: None,
                    });
                }
                Ok(BackendPage {
                    items: vec![Track {
                        video_id: VideoId::new("vid_dedup_1").unwrap(),
                        title: "Synthetic Track".to_string(),
                        artists: vec![ArtistRef {
                            id: None,
                            name: "Synthetic Artist".to_string(),
                        }],
                        album: None,
                        duration_ms: Some(120_000),
                        cover: None,
                        availability: TrackAvailability::Available,
                        version_tag: None,
                        is_liked: None,
                    }],
                    next_continuation: None,
                })
            })
        }

        fn library_playlists<'a>(
            &'a self,
            _session: &'a Arc<dyn CatalogSession>,
            _continuation: Option<&'a str>,
        ) -> BoxFuture<'a, Result<BackendPage<ytm_protocol::PlaylistSummary>, QueryError>> {
            Box::pin(async {
                Ok(BackendPage {
                    items: vec![],
                    next_continuation: None,
                })
            })
        }

        fn playlist_tracks<'a>(
            &'a self,
            _session: &'a Arc<dyn CatalogSession>,
            _playlist_id: &'a PlaylistId,
            _continuation: Option<&'a str>,
        ) -> BoxFuture<'a, Result<BackendPlaylistPage, QueryError>> {
            Box::pin(async { unreachable!() })
        }

        fn album_detail<'a>(
            &'a self,
            _session: &'a Arc<dyn CatalogSession>,
            _album_id: &'a ytm_protocol::AlbumId,
        ) -> BoxFuture<'a, Result<ytm_protocol::AlbumDetail, QueryError>> {
            Box::pin(async { unreachable!() })
        }

        fn artist_detail<'a>(
            &'a self,
            _session: &'a Arc<dyn CatalogSession>,
            _artist_id: &'a ytm_protocol::ArtistId,
        ) -> BoxFuture<'a, Result<ytm_protocol::ArtistDetail, QueryError>> {
            Box::pin(async { unreachable!() })
        }

        fn search_overview<'a>(
            &'a self,
            _session: &'a Arc<dyn CatalogSession>,
            _query: &'a str,
        ) -> BoxFuture<'a, Result<crate::probe::SearchOverview, QueryError>> {
            Box::pin(async { Ok(crate::probe::SearchOverview::default()) })
        }

        fn recent_history<'a>(
            &'a self,
            _session: &'a Arc<dyn CatalogSession>,
        ) -> BoxFuture<'a, Result<Vec<Track>, QueryError>> {
            Box::pin(async { Ok(vec![]) })
        }

        fn official_lyrics<'a>(
            &'a self,
            _session: &'a Arc<dyn CatalogSession>,
            _video_id: &'a VideoId,
        ) -> BoxFuture<'a, Result<Option<OfficialLyrics>, QueryError>> {
            Box::pin(async { Ok(None) })
        }

        fn set_like<'a>(
            &'a self,
            _session: &'a Arc<dyn CatalogSession>,
            _video_id: &'a VideoId,
            _liked: bool,
        ) -> BoxFuture<'a, Result<(), QueryError>> {
            Box::pin(async {
                Err(QueryError {
                    code: QueryErrorCode::RateLimited,
                    retriable: true,
                    retry_after_ms: Some(10),
                    safe_message: "mutation rate limited".to_string(),
                    request_id: None,
                })
            })
        }

        fn add_to_playlist<'a>(
            &'a self,
            _session: &'a Arc<dyn CatalogSession>,
            _playlist_id: &'a PlaylistId,
            _video_id: &'a VideoId,
        ) -> BoxFuture<'a, Result<(), QueryError>> {
            Box::pin(async { Ok(()) })
        }
    }

    fn make_test_storage(
        clock: Arc<dyn ytm_core::ports::Clock>,
    ) -> (tempfile::TempDir, Arc<SqliteStorage>) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(
            SqliteStorage::open(
                StoragePaths::for_root(dir.path()),
                clock,
                CoverCacheConfig::default(),
            )
            .unwrap(),
        );
        (dir, storage)
    }

    async fn search_fixture(
        backend: &Arc<FlakyDedupBackend>,
    ) -> (
        tempfile::TempDir,
        Arc<CatalogService>,
        AccountEpoch,
        AccountScope,
        CatalogQueryRequest,
    ) {
        let clock = Arc::new(MockClock::new(10_000));
        let (dir, storage) = make_test_storage(clock.clone());
        let adapter = Arc::new(YtmCatalogAdapter::with_backend(
            storage,
            backend.clone(),
            YtmCatalogConfig::default(),
            clock,
        ));
        let service = Arc::new(CatalogService::with_config(
            adapter,
            CatalogServiceConfig {
                max_read_retries: 2,
                base_backoff: Duration::from_millis(10),
                max_backoff: Duration::from_millis(100),
            },
        ));

        let epoch = AccountEpoch::new("acct-svc-1").unwrap();
        let scope = service
            .apply_session_material(
                SessionMaterialPayload {
                    account_epoch: epoch.clone(),
                    credential_revision: ytm_protocol::CredentialRevision::new("cred-1").unwrap(),
                    cookie_pairs: BTreeMap::from([(
                        "SAPISID".to_string(),
                        "synthetic_sapisid".to_string(),
                    )]),
                    auth_user_index: Some(0),
                    channel_label: Some("SyntheticUser".to_string()),
                },
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .account_scope;

        let req = CatalogQueryRequest {
            kind: CatalogQueryKind::SearchSongs,
            query: Some("daft".to_string()),
            playlist_id: None,
            album_id: None,
            artist_id: None,
            cursor: None,
            limit: Some(10),
        };

        (dir, service, epoch, scope, req)
    }

    #[tokio::test]
    async fn catalog_service_coalesces_concurrent_reads_and_retries_transient_errors_only() {
        let backend = Arc::new(FlakyDedupBackend {
            fail_times: AtomicUsize::new(2), // Succeeds on the third attempt.
            delay_ms: 25,
            ..Default::default()
        });
        let (_dir, service, epoch, scope, req) = search_fixture(&backend).await;
        // Fire 4 identical concurrent read queries -> must coalesce into ONE logical retry sequence (3 attempts, not 12)
        let mut handles = Vec::new();
        for _ in 0..4 {
            let svc = Arc::clone(&service);
            let ep = epoch.clone();
            let sc = scope.clone();
            let r = req.clone();
            handles.push(tokio::spawn(async move {
                svc.query(Some(ep), Some(sc), r, CancellationToken::new())
                    .await
            }));
        }
        for h in handles {
            let resp = h.await.unwrap().unwrap();
            assert!(matches!(resp, CatalogResponsePayload::SearchSongs(_)));
        }
        assert_eq!(backend.search_calls.load(Ordering::SeqCst), 3);

        // Mutations NEVER automatically retry; a 429 means the write was refused (not applied).
        let refused = service
            .mutate(
                epoch,
                scope,
                MutationRequest {
                    kind: MutationKind::SetLike,
                    video_id: VideoId::new("vid_dedup_1").unwrap(),
                    liked: Some(true),
                    playlist_id: None,
                    confirmed_by_user: true,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(refused.outcome, ytm_protocol::MutationOutcome::Rejected);
        assert_eq!(
            refused.error.map(|e| e.code),
            Some(QueryErrorCode::RateLimited)
        );
    }

    #[tokio::test]
    async fn dropped_leader_does_not_leave_a_dead_read_slot() {
        let backend = Arc::new(FlakyDedupBackend {
            pending: Some(Default::default()),
            ..Default::default()
        });
        let (_dir, service, epoch, scope, req) = search_fixture(&backend).await;
        let (started, release) = backend.pending.as_ref().unwrap();
        let query = || {
            service.query(
                Some(epoch.clone()),
                Some(scope.clone()),
                req.clone(),
                CancellationToken::new(),
            )
        };
        let mut leader = query();
        tokio::select! {
            result = &mut leader => panic!("unexpected completion: {result:?}"),
            _ = started.notified() => {}
        }
        drop(leader);
        let mut replacement = query();
        tokio::select! {
            result = &mut replacement => panic!("replacement did not issue its own read: {result:?}"),
            fired = tokio::time::timeout(Duration::from_secs(1), started.notified()) => fired.unwrap()
        }
        release.notify_one();
        assert!(matches!(
            replacement.await.unwrap(),
            CatalogResponsePayload::SearchSongs(_)
        ));
        assert_eq!(backend.search_calls.load(Ordering::SeqCst), 2);
        assert!(service.inflight_reads.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn cancelling_leader_preserves_the_followers_shared_read() {
        let backend = Arc::new(FlakyDedupBackend {
            pending: Some(Default::default()),
            ..Default::default()
        });
        let (_dir, service, epoch, scope, req) = search_fixture(&backend).await;
        let (started, release) = backend.pending.as_ref().unwrap();
        let cancel = CancellationToken::new();
        let mut leader = service.query(
            Some(epoch.clone()),
            Some(scope.clone()),
            req.clone(),
            cancel.clone(),
        );
        tokio::select! {
            result = &mut leader => panic!("unexpected completion: {result:?}"),
            _ = started.notified() => {}
        }
        let mut follower = service.query(Some(epoch), Some(scope), req, CancellationToken::new());
        tokio::select! {
            biased;
            result = &mut follower => panic!("unexpected completion: {result:?}"),
            _ = std::future::ready(()) => {}
        }
        cancel.cancel();
        assert_eq!(leader.await.unwrap_err().code, QueryErrorCode::StaleContext);
        release.notify_one();
        assert!(matches!(
            follower.await.unwrap(),
            CatalogResponsePayload::SearchSongs(_)
        ));
        assert_eq!(backend.search_calls.load(Ordering::SeqCst), 1);
        assert!(service.inflight_reads.lock().unwrap().is_empty());
    }

    #[test]
    fn scrubbed_catalog_fixture_matches_protocol() {
        let fixture_raw = include_str!("../../../../fixtures/catalog/search-songs-sample.json");
        let parsed: CatalogResponsePayload = serde_json::from_str(fixture_raw).unwrap();
        assert!(matches!(parsed, CatalogResponsePayload::SearchSongs(_)));
    }
}
