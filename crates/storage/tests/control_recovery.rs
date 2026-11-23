//! The player settles operations interrupted by a restart on its own, from fresh evidence.

use std::sync::Arc;
use std::time::Duration;

use tokio::time::timeout;
use ytm_control_api::{
    HistoryEntry, ItemPhase, ItemResult, OperationPhase, OperationReceipt, OperationResource, Page,
    PageInput, Playlist, PlaylistEntry, PlaylistPrivacy, Preconditions, RecommendationPage,
    RetryDisposition, VerificationLevel, WriteAction, WriteInput,
};
use ytm_core::control::ControlService;
use ytm_core::control::catalog::{
    CatalogReadContext, ControlCatalog, PlaylistWrite, PlaylistWriteAck, PlaylistWriteError,
    project_track,
};
use ytm_core::control::journal::{Claim, OperationKey, OperationStore};
use ytm_core::{
    BoxFuture, CancellationToken, CatalogIdentity, CatalogPort, CoreDeps, CoreEngine, EngineConfig,
    MockClock, OfficialLyrics,
};
use ytm_protocol::{
    AccountEpoch, AccountScope, BridgeEpoch, BridgeReadyPayload, CatalogQueryRequest,
    CatalogResponsePayload, CredentialRevision, Freshness, MutationRequest, MutationResult,
    PlaylistId, QueryError, SessionMaterialPayload, Track, TrackAvailability, VideoId,
};
use ytm_storage::{CoverCacheConfig, SqliteStorage, StoragePaths};

const ACCOUNT: &str = "fixture-recovery-account";
const EPOCH: &str = "fixture-recovery-epoch";
const HINT: &str = "fixture-recovery-identity";
const PRINCIPAL: &str = "fixture-recovery-agent";
/// Its appended entries are visible in a fresh read.
const LANDED: &str = "PL_fixture_landed";
/// Its appended entries never appear.
const LOST: &str = "PL_fixture_lost";

fn track(id: &str) -> Track {
    Track {
        video_id: VideoId::new(id).unwrap(),
        title: id.into(),
        artists: vec![],
        album: None,
        duration_ms: Some(20_000),
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
        Box::pin(async { panic!("settlement reads through the control catalog") })
    }

    fn mutate(
        &self,
        _epoch: AccountEpoch,
        _scope: AccountScope,
        _request: MutationRequest,
        _cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<MutationResult, QueryError>> {
        Box::pin(async { panic!("settlement must never resend a write") })
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
        Box::pin(async { panic!("settlement does not read lyrics") })
    }

    fn clear_session_material(&self, _epoch: Option<AccountEpoch>) -> Result<(), QueryError> {
        Ok(())
    }
}

impl ControlCatalog for Catalog {
    fn youtube_history(
        &self,
        _context: CatalogReadContext,
        _page: PageInput,
    ) -> BoxFuture<'_, Result<Page<HistoryEntry>, QueryError>> {
        Box::pin(async { panic!("settlement does not read history") })
    }

    fn fresh_playlist(
        &self,
        context: CatalogReadContext,
        playlist: PlaylistId,
        _page: PageInput,
    ) -> BoxFuture<'_, Result<Playlist, QueryError>> {
        Box::pin(async move {
            assert_eq!(context.account_scope.as_str(), ACCOUNT);
            assert_eq!(context.account_epoch.as_str(), EPOCH);
            let items = if playlist.as_str() == LANDED {
                ["fixture-kept", "fixture-a", "fixture-b"]
                    .into_iter()
                    .enumerate()
                    .map(|(position, video)| PlaylistEntry {
                        entry_id: Some(format!("entry-{video}")),
                        position: position as u32,
                        track: project_track(&track(video)),
                    })
                    .collect()
            } else {
                vec![]
            };
            Ok(Playlist {
                playlist_id: playlist,
                identity_confirmed: true,
                title: "Fixture".into(),
                description: None,
                privacy: Some(PlaylistPrivacy::Private),
                editable: Some(true),
                snapshot_fingerprint: None,
                entries: Page {
                    items,
                    next_cursor: None,
                    complete: true,
                    source: "fixture-playlist".into(),
                    freshness: Freshness::Fresh,
                    captured_at_ms: 1_000,
                },
            })
        })
    }

    fn recommend_song_radio(
        &self,
        _context: CatalogReadContext,
        _seed: VideoId,
        _page: PageInput,
    ) -> BoxFuture<'_, Result<RecommendationPage, QueryError>> {
        Box::pin(async { panic!("settlement does not read recommendations") })
    }

    fn apply_playlist(
        &self,
        _context: CatalogReadContext,
        _write: PlaylistWrite,
    ) -> BoxFuture<'_, Result<PlaylistWriteAck, PlaylistWriteError>> {
        Box::pin(async { panic!("settlement must never resend a playlist write") })
    }
}

fn key(operation_id: &str) -> OperationKey {
    OperationKey {
        principal: PRINCIPAL.into(),
        account_scope: Some(AccountScope::new(ACCOUNT).unwrap()),
        operation_id: operation_id.into(),
    }
}

/// Journals an append to `playlist` as acknowledged with provider entry IDs, as a worker
/// leaves it when the player stops before its read-back.
async fn journal_acknowledged_append(store: &SqliteStorage, operation_id: &str, playlist: &str) {
    let playlist = PlaylistId::new(playlist).unwrap();
    let videos = ["fixture-a", "fixture-b"];
    let mut receipt = OperationReceipt {
        operation_id: operation_id.into(),
        account_scope: Some(AccountScope::new(ACCOUNT).unwrap()),
        revision: 0,
        phase: OperationPhase::Accepted,
        resource: None,
        verification: None,
        item_results: vec![],
        scheduling_stopped: false,
        settled: false,
        retry_disposition: RetryDisposition::QueryOperation,
        error: None,
        removed: vec![],
    };
    let input = WriteInput {
        operation_id: operation_id.into(),
        preconditions: Preconditions {
            expected_account_epoch: Some(AccountEpoch::new(EPOCH).unwrap()),
            expected_bridge_epoch: None,
            expected_queue_revision: None,
            expected_entity_revision: None,
        },
        action: WriteAction::AddPlaylistEntries {
            playlist_id: playlist.clone(),
            created_by_operation_id: None,
            video_ids: videos.iter().map(|id| VideoId::new(*id).unwrap()).collect(),
        },
    };
    let claim = store
        .claim(key(operation_id), input, [7; 32], receipt.clone(), 1_000)
        .await
        .unwrap();
    assert!(matches!(claim, Claim::New(_)));
    receipt.revision = 1;
    receipt.phase = OperationPhase::Acknowledged;
    receipt.resource = Some(OperationResource::Playlist { id: playlist });
    receipt.item_results = videos
        .iter()
        .enumerate()
        .map(|(index, video)| ItemResult {
            input_index: index as u32,
            phase: ItemPhase::Acknowledged,
            resource_entry_id: Some(format!("entry-{video}")),
            error: None,
        })
        .collect();
    store
        .update(key(operation_id), 0, receipt, 1_000)
        .await
        .unwrap();
}

async fn receipt(store: &SqliteStorage, operation_id: &str) -> OperationReceipt {
    store.get(key(operation_id)).await.unwrap().unwrap()
}

#[tokio::test]
async fn a_recovered_append_settles_from_fresh_evidence_once_the_account_is_bound() {
    let root = tempfile::tempdir().unwrap();
    let clock = Arc::new(MockClock::new(1_000));
    let open = || {
        Arc::new(
            SqliteStorage::open(
                StoragePaths::for_root(root.path()),
                clock.clone(),
                CoverCacheConfig::production(),
            )
            .unwrap(),
        )
    };
    {
        let before_restart = open();
        // Older unprovable operations must not use up the settlement budget.
        for index in 0..20 {
            journal_acknowledged_append(&before_restart, &format!("old-{index}"), LOST).await;
        }
        journal_acknowledged_append(&before_restart, "landed", LANDED).await;
        // Newest first, so this one is examined before the landed append.
        journal_acknowledged_append(&before_restart, "lost", LOST).await;
    }
    let storage = open();
    storage.recover(2_000).await.unwrap();
    for id in ["landed", "lost"] {
        let recovered = receipt(&storage, id).await;
        assert_eq!(recovered.phase, OperationPhase::Unknown);
        assert!(recovered.scheduling_stopped && !recovered.settled);
    }
    let lost_before = receipt(&storage, "lost").await;

    let engine = CoreEngine::start(
        CoreDeps {
            clock: clock.clone(),
            catalog: Some(Arc::new(Catalog)),
            ..Default::default()
        },
        EngineConfig::default(),
    );
    let service = Arc::new(
        ControlService::new(engine.clone())
            .with_catalog(Arc::new(Catalog))
            .with_operations(storage.clone()),
    );
    let settler = tokio::spawn(service.clone().settle_on_account_bind());
    // Nothing is bound yet, so nothing is settled.
    tokio::task::yield_now().await;
    assert_eq!(
        receipt(&storage, "landed").await.phase,
        OperationPhase::Unknown
    );

    engine
        .on_bridge_ready(
            1,
            BridgeReadyPayload {
                bridge_epoch: BridgeEpoch::new("fixture-recovery-bridge").unwrap(),
                bridge_version: "0.1.0".into(),
                capabilities: Default::default(),
                account_hint: Some(HINT.into()),
                logged_in_observed: true,
            },
        )
        .await
        .unwrap();
    engine
        .on_session_material(SessionMaterialPayload {
            account_epoch: AccountEpoch::new(EPOCH).unwrap(),
            credential_revision: CredentialRevision::new("fixture-recovery-credentials").unwrap(),
            cookie_pairs: Default::default(),
            auth_user_index: None,
            channel_label: None,
        })
        .await
        .unwrap();

    let settled = timeout(Duration::from_secs(3), async {
        loop {
            let landed = receipt(&storage, "landed").await;
            if landed.settled {
                return landed;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the bound account's recovered append was not settled");
    assert_eq!(settled.phase, OperationPhase::Verified);
    assert_eq!(
        settled.verification.unwrap().level,
        VerificationLevel::ReadBack
    );
    assert!(
        settled
            .item_results
            .iter()
            .all(|item| item.phase == ItemPhase::Verified)
    );
    // Without evidence the other append is left exactly as recovery stopped it.
    assert_eq!(receipt(&storage, "lost").await, lost_before);
    assert_eq!(
        receipt(&storage, "old-0").await.phase,
        OperationPhase::Unknown
    );

    engine.shutdown().await.unwrap();
    timeout(Duration::from_secs(3), settler)
        .await
        .expect("the settler ends with the engine")
        .unwrap();
}
