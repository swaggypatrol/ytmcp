use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, Notify};
use tokio_util::sync::CancellationToken;
use ytm_control_api::{
    ControlError, ControlRequest, ControlResponse, ErrorCode, GrantAction, ItemPhase,
    LibraryMembershipTarget, MembershipEvidence, OperationInput, OperationPhase, OperationReceipt,
    OperationResource, Page, PageInput, Playlist, PlaylistEntry, PlaylistPrivacy, Preconditions,
    Rating, RecommendationPage, RemovedItem, ResumeAction, RetryDisposition, Track, TrackTarget,
    VerificationLevel, WriteAction, WriteInput,
};
use ytm_core::control::catalog::{
    AddedOccurrence, CatalogReadContext, CatalogWriteError, ControlCatalog, PlaylistEdit,
    PlaylistOccurrenceIdentity, PlaylistWrite, PlaylistWriteAck, PlaylistWriteError,
    RatingEvidence,
};
use ytm_core::control::journal::{
    AccountOperation, Claim, JournalEntry, OperationKey, OperationStore,
};
use ytm_core::control::{ControlService, DispatchAuthority, DispatchGrant, ReadGrant};
use ytm_core::ports::{CatalogIdentity, CatalogPort, OfficialLyrics, PlaylistAddition};
use ytm_core::{BoxFuture, CoreDeps, CoreEngine, EngineConfig, MockClock};
use ytm_protocol::{
    AccountEpoch, AccountScope, AlbumId, ArtistId, BridgeEpoch, BridgeReadyPayload,
    CatalogQueryRequest, CatalogResponsePayload, CredentialRevision, Freshness, MutationKind,
    MutationOutcome, MutationRequest, MutationResult, PlaylistId, QueryError, QueryErrorCode,
    SessionMaterialPayload, StateDelta, TrackAvailability, VideoId,
};

const SCOPE: &str = "scope-playlist-fixture";
const EPOCH: &str = "epoch-playlist-fixture";
const HINT: &str = "synthetic-identity";
const CREATED_PLAYLIST: &str = "PL_fixture_created";
const RATED_VIDEO: &str = "rated-fixture";

struct Entry {
    key: OperationKey,
    input: WriteInput,
    hash: [u8; 32],
    receipt: OperationReceipt,
}

#[derive(Default)]
struct Journal {
    records: Mutex<Vec<Entry>>,
    changed: Notify,
}

impl OperationStore for Journal {
    fn recent(
        &self,
        scope: Option<AccountScope>,
        limit: usize,
    ) -> BoxFuture<'_, Result<Vec<JournalEntry>, ControlError>> {
        Box::pin(async move {
            Ok(self
                .records
                .lock()
                .await
                .iter()
                .rev()
                .filter(|record| record.key.account_scope == scope)
                .take(limit)
                .map(|record| JournalEntry {
                    principal: record.key.principal.clone(),
                    receipt: record.receipt.clone(),
                })
                .collect())
        })
    }

    fn account_operations(
        &self,
        scope: Option<AccountScope>,
        _since_ms: u64,
    ) -> BoxFuture<'_, Result<Vec<AccountOperation>, ControlError>> {
        Box::pin(async move {
            Ok(self
                .records
                .lock()
                .await
                .iter()
                .filter(|record| record.key.account_scope == scope)
                .map(|record| AccountOperation {
                    claimed_at_ms: 0,
                    input: record.input.clone(),
                    receipt: record.receipt.clone(),
                })
                .collect())
        })
    }

    fn unsettled(
        &self,
        scope: Option<AccountScope>,
        limit: usize,
    ) -> BoxFuture<'_, Result<Vec<JournalEntry>, ControlError>> {
        Box::pin(async move {
            Ok(self
                .records
                .lock()
                .await
                .iter()
                .rev()
                .filter(|record| record.key.account_scope == scope && !record.receipt.settled)
                .take(limit)
                .map(|record| JournalEntry {
                    principal: record.key.principal.clone(),
                    receipt: record.receipt.clone(),
                })
                .collect())
        })
    }

    fn claim(
        &self,
        key: OperationKey,
        input: WriteInput,
        hash: [u8; 32],
        initial: OperationReceipt,
        _now_ms: u64,
    ) -> BoxFuture<'_, Result<Claim, ControlError>> {
        Box::pin(async move {
            let mut rows = self.records.lock().await;
            if let Some(row) = rows.iter().find(|row| row.key == key) {
                return Ok(if row.hash == hash {
                    Claim::Existing(row.receipt.clone())
                } else {
                    Claim::Conflict
                });
            }
            rows.push(Entry {
                key,
                input,
                hash,
                receipt: initial.clone(),
            });
            Ok(Claim::New(initial))
        })
    }

    fn get(
        &self,
        key: OperationKey,
    ) -> BoxFuture<'_, Result<Option<OperationReceipt>, ControlError>> {
        Box::pin(async move {
            Ok(self
                .records
                .lock()
                .await
                .iter()
                .find(|row| row.key == key)
                .map(|row| row.receipt.clone()))
        })
    }

    fn load_request(
        &self,
        key: OperationKey,
    ) -> BoxFuture<'_, Result<Option<WriteInput>, ControlError>> {
        Box::pin(async move {
            Ok(self
                .records
                .lock()
                .await
                .iter()
                .find(|row| row.key == key)
                .map(|row| row.input.clone()))
        })
    }

    fn update(
        &self,
        key: OperationKey,
        expected_revision: u64,
        next: OperationReceipt,
        _now_ms: u64,
    ) -> BoxFuture<'_, Result<(), ControlError>> {
        Box::pin(async move {
            let mut rows = self.records.lock().await;
            let row = rows
                .iter_mut()
                .find(|row| row.key == key)
                .expect("operation should be claimed");
            assert_eq!(row.receipt.revision, expected_revision);
            assert_eq!(next.revision, expected_revision + 1);
            row.receipt = next;
            self.changed.notify_one();
            Ok(())
        })
    }

    fn recover(&self, _now_ms: u64) -> BoxFuture<'_, Result<(), ControlError>> {
        Box::pin(async { panic!("fixture only models the active service") })
    }
}

#[derive(Default)]
struct Remote {
    memberships: HashMap<LibraryMembershipTarget, bool>,
    membership_writes: Vec<(LibraryMembershipTarget, bool)>,
    membership_invalidations: Vec<LibraryMembershipTarget>,
    ignore_membership_write: bool,
    album_membership: Option<(PlaylistId, bool)>,
    reissued_album: Option<PlaylistId>,
    album_writes: Vec<PlaylistId>,
    playlist: Option<Playlist>,
    writes: Vec<PlaylistWrite>,
    edits: Vec<PlaylistEdit>,
    invalidations: Vec<PlaylistId>,
    reads: usize,
    lose_create_reply: bool,
    lose_append_reply: bool,
    reverse_append: bool,
    stale_readback: bool,
    returned_playlist_id: Option<PlaylistId>,
    partial_removal: bool,
    rating: Option<Rating>,
    rating_writes: usize,
    rating_invalidations: usize,
    subscribed: Option<bool>,
    subscription_writes: Vec<bool>,
    /// The app's own adds (`CatalogPort::add_to_playlist`), and whether their replies name the
    /// created entry.
    ui_adds: usize,
    unnamed_ui_add: bool,
    /// After a removal, the provider also swaps the first two remaining entries.
    reorder_on_removal: bool,
}

#[derive(Default)]
struct Catalog(Mutex<Remote>);

impl CatalogPort for Catalog {
    fn query(
        &self,
        _account_epoch: Option<AccountEpoch>,
        _account_scope: Option<AccountScope>,
        _request: CatalogQueryRequest,
        _cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<CatalogResponsePayload, QueryError>> {
        Box::pin(async { panic!("playlist verification must use the fresh control catalog path") })
    }

    fn mutate(
        &self,
        _account_epoch: AccountEpoch,
        _account_scope: AccountScope,
        _request: MutationRequest,
        _cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<MutationResult, QueryError>> {
        Box::pin(async { panic!("playlist writes must use the dedicated catalog port") })
    }

    /// The app's add: one new entry at the end, refused like the provider's duplicate check when
    /// the song is already there.
    fn add_to_playlist(
        &self,
        account_epoch: AccountEpoch,
        account_scope: AccountScope,
        request: MutationRequest,
        _cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<PlaylistAddition, QueryError>> {
        Box::pin(async move {
            assert_eq!(account_scope.as_str(), SCOPE);
            assert_eq!(account_epoch.as_str(), EPOCH);
            let mut remote = self.0.lock().await;
            remote.ui_adds += 1;
            let named = !remote.unnamed_ui_add;
            let stored = remote.playlist.as_mut().unwrap();
            assert_eq!(Some(&stored.playlist_id), request.playlist_id.as_ref());
            let entries = &mut stored.entries.items;
            if entries
                .iter()
                .any(|entry| entry.track.video_id == request.video_id)
            {
                let refused = QueryError {
                    code: QueryErrorCode::UnavailableTrack,
                    retriable: false,
                    retry_after_ms: None,
                    safe_message: "YouTube Music returned STATUS_FAILED".into(),
                    request_id: None,
                };
                return Ok(PlaylistAddition {
                    result: MutationResult::new(
                        request.kind,
                        MutationOutcome::Rejected,
                        Some(refused),
                    ),
                    created_entry_id: None,
                });
            }
            let entry_id = format!("ui-occurrence-{}", entries.len());
            entries.push(entry(&entry_id, request.video_id.as_str(), entries.len()));
            Ok(PlaylistAddition {
                result: MutationResult::new(request.kind, MutationOutcome::Confirmed, None),
                created_entry_id: named.then_some(entry_id),
            })
        })
    }

    fn apply_session_material(
        &self,
        _material: SessionMaterialPayload,
        _cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<CatalogIdentity, QueryError>> {
        Box::pin(async {
            Ok(CatalogIdentity {
                account_scope: AccountScope::new(SCOPE).unwrap(),
                account_hint: Some(HINT.into()),
            })
        })
    }

    fn official_lyrics(
        &self,
        _account_epoch: AccountEpoch,
        _video_id: VideoId,
        _cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<Option<OfficialLyrics>, QueryError>> {
        Box::pin(async { panic!("lyrics are outside this test") })
    }

    fn clear_session_material(
        &self,
        _account_epoch: Option<AccountEpoch>,
    ) -> Result<(), QueryError> {
        Ok(())
    }
}

impl ControlCatalog for Catalog {
    fn album_membership(
        &self,
        context: CatalogReadContext,
        album: AlbumId,
    ) -> BoxFuture<'_, Result<(PlaylistId, MembershipEvidence), QueryError>> {
        Box::pin(async move {
            assert_context(&context);
            assert_eq!(album.as_str(), "MPREb_fixture");
            let (playlist, saved) = self.0.lock().await.album_membership.clone().unwrap();
            Ok((
                playlist,
                MembershipEvidence {
                    value: Some(saved),
                    observed_at_ms: 1_000,
                },
            ))
        })
    }

    fn set_album_membership(
        &self,
        context: CatalogReadContext,
        album: AlbumId,
        playlist: PlaylistId,
        saved: bool,
    ) -> BoxFuture<'_, Result<(), CatalogWriteError>> {
        Box::pin(async move {
            assert_context(&context);
            assert_eq!(album.as_str(), "MPREb_fixture");
            let mut remote = self.0.lock().await;
            assert_eq!(&playlist, &remote.album_membership.as_ref().unwrap().0);
            remote.album_writes.push(playlist);
            remote.album_membership = Some((remote.reissued_album.take().unwrap(), saved));
            Ok(())
        })
    }

    fn library_membership(
        &self,
        context: CatalogReadContext,
        target: LibraryMembershipTarget,
    ) -> BoxFuture<'_, Result<MembershipEvidence, QueryError>> {
        Box::pin(async move {
            assert_context(&context);
            Ok(MembershipEvidence {
                value: self.0.lock().await.memberships.get(&target).copied(),
                observed_at_ms: 1_000,
            })
        })
    }

    fn set_library_membership(
        &self,
        context: CatalogReadContext,
        target: LibraryMembershipTarget,
        saved: bool,
    ) -> BoxFuture<'_, Result<(), CatalogWriteError>> {
        Box::pin(async move {
            assert_context(&context);
            let mut remote = self.0.lock().await;
            remote.membership_writes.push((target.clone(), saved));
            if !remote.ignore_membership_write {
                *remote
                    .memberships
                    .get_mut(&target)
                    .expect("a precise observed membership target") = saved;
            }
            Ok(())
        })
    }

    fn invalidate_library_membership(
        &self,
        scope: AccountScope,
        target: LibraryMembershipTarget,
    ) -> BoxFuture<'_, Result<(), QueryError>> {
        Box::pin(async move {
            assert_eq!(scope.as_str(), SCOPE);
            self.0.lock().await.membership_invalidations.push(target);
            Ok(())
        })
    }

    fn artist_subscription(
        &self,
        context: CatalogReadContext,
        artist: ArtistId,
    ) -> BoxFuture<'_, Result<MembershipEvidence, QueryError>> {
        Box::pin(async move {
            assert_context(&context);
            assert_eq!(artist.as_str(), "UC-fixture-subscription");
            Ok(MembershipEvidence {
                value: self.0.lock().await.subscribed,
                observed_at_ms: 1_000,
            })
        })
    }

    fn set_artist_subscription(
        &self,
        context: CatalogReadContext,
        artist: ArtistId,
        subscribed: bool,
    ) -> BoxFuture<'_, Result<(), CatalogWriteError>> {
        Box::pin(async move {
            assert_context(&context);
            assert_eq!(artist.as_str(), "UC-fixture-subscription");
            self.0.lock().await.subscription_writes.push(subscribed);
            Ok(())
        })
    }

    fn fresh_playlist(
        &self,
        context: CatalogReadContext,
        playlist: PlaylistId,
        page: PageInput,
    ) -> BoxFuture<'_, Result<Playlist, QueryError>> {
        Box::pin(async move {
            assert_context(&context);
            assert!(page.cursor.is_none());
            let mut remote = self.0.lock().await;
            remote.reads += 1;
            let mut existing = remote.playlist.clone().ok_or_else(|| QueryError {
                code: QueryErrorCode::UnavailableTrack,
                retriable: false,
                retry_after_ms: None,
                safe_message: "the current account cannot see the playlist".into(),
                request_id: None,
            })?;
            assert_eq!(existing.playlist_id, playlist);
            if remote.stale_readback {
                existing.entries.freshness = Freshness::Stale;
            }
            if let Some(returned_id) = &remote.returned_playlist_id {
                existing.playlist_id = returned_id.clone();
            }
            Ok(existing)
        })
    }

    fn recommend_song_radio(
        &self,
        _context: CatalogReadContext,
        _seed: VideoId,
        _page: PageInput,
    ) -> BoxFuture<'_, Result<RecommendationPage, QueryError>> {
        Box::pin(async { panic!("recommendations are outside this test") })
    }

    fn apply_playlist(
        &self,
        context: CatalogReadContext,
        write: PlaylistWrite,
    ) -> BoxFuture<'_, Result<PlaylistWriteAck, PlaylistWriteError>> {
        Box::pin(async move {
            assert_context(&context);
            let mut remote = self.0.lock().await;
            remote.writes.push(write.clone());
            match write {
                PlaylistWrite::CreatePrivate { title, description } => {
                    let id = PlaylistId::new(CREATED_PLAYLIST).unwrap();
                    remote.playlist = Some(Playlist {
                        playlist_id: id.clone(),
                        identity_confirmed: true,
                        title,
                        description,
                        privacy: Some(PlaylistPrivacy::Private),
                        editable: Some(true),
                        snapshot_fingerprint: Some("fixture-fingerprint".into()),
                        entries: Page {
                            items: vec![],
                            next_cursor: None,
                            complete: true,
                            source: "fixture".into(),
                            freshness: Freshness::Fresh,
                            captured_at_ms: 1000,
                        },
                    });
                    if remote.lose_create_reply {
                        return Err(PlaylistWriteError::Unknown(ControlError::new(
                            ErrorCode::ResultUnknown,
                            RetryDisposition::ReconcileFirst,
                            "provider response was lost after the write",
                        )));
                    }
                    Ok(PlaylistWriteAck::Created(id))
                }
                PlaylistWrite::Append {
                    playlist,
                    mut videos,
                } => {
                    if remote.reverse_append {
                        videos.reverse();
                    }
                    let stored = remote.playlist.as_mut().unwrap();
                    assert_eq!(stored.playlist_id, playlist);
                    let offset = stored.entries.items.len();
                    let mut added = Vec::new();
                    for (i, video_id) in videos.into_iter().enumerate() {
                        let entry_id = format!("occurrence-{}", offset + i);
                        stored.entries.items.push(PlaylistEntry {
                            entry_id: Some(entry_id.clone()),
                            position: (offset + i) as u32,
                            track: Track {
                                title: video_id.as_str().into(),
                                video_id: video_id.clone(),
                                artists: vec![],
                                album: None,
                                duration_ms: None,
                                availability: TrackAvailability::Available,
                                version_tag: None,
                                rating: Rating::Unknown,
                            },
                        });
                        added.push(AddedOccurrence { video_id, entry_id });
                    }
                    if remote.lose_append_reply {
                        return Err(PlaylistWriteError::Unknown(ControlError::new(
                            ErrorCode::ResultUnknown,
                            RetryDisposition::ReconcileFirst,
                            "provider response was lost after the append",
                        )));
                    }
                    // The acknowledgement order alone is never proof of playlist ordering.
                    added.reverse();
                    Ok(PlaylistWriteAck::Appended(added))
                }
            }
        })
    }

    fn invalidate_playlist_cache(
        &self,
        scope: AccountScope,
        playlist: PlaylistId,
        library_changed: bool,
    ) -> BoxFuture<'_, Result<(), QueryError>> {
        Box::pin(async move {
            assert!(library_changed);
            assert_eq!(scope.as_str(), SCOPE);
            self.0.lock().await.invalidations.push(playlist);
            Ok(())
        })
    }

    fn fresh_rating(
        &self,
        context: CatalogReadContext,
        video_id: VideoId,
    ) -> BoxFuture<'_, Result<RatingEvidence, QueryError>> {
        Box::pin(async move {
            assert_context(&context);
            assert_eq!(video_id.as_str(), RATED_VIDEO);
            Ok(RatingEvidence {
                rating: self.0.lock().await.rating.unwrap_or(Rating::Unknown),
                observed_at_ms: 1000,
            })
        })
    }

    fn set_rating(
        &self,
        context: CatalogReadContext,
        video_id: VideoId,
        rating: Rating,
    ) -> BoxFuture<'_, Result<(), CatalogWriteError>> {
        Box::pin(async move {
            assert_context(&context);
            assert_eq!(video_id.as_str(), RATED_VIDEO);
            assert_eq!(rating, Rating::Disliked);
            self.0.lock().await.rating_writes += 1;
            Err(CatalogWriteError::Unknown(ControlError::new(
                ErrorCode::ResultUnknown,
                RetryDisposition::ReconcileFirst,
                "provider response did not establish the rating",
            )))
        })
    }

    fn invalidate_rating_cache(
        &self,
        scope: AccountScope,
    ) -> BoxFuture<'_, Result<(), QueryError>> {
        Box::pin(async move {
            assert_eq!(scope.as_str(), SCOPE);
            self.0.lock().await.rating_invalidations += 1;
            Ok(())
        })
    }

    fn edit_playlist(
        &self,
        context: CatalogReadContext,
        id: PlaylistId,
        edit: PlaylistEdit,
    ) -> BoxFuture<'_, Result<(), CatalogWriteError>> {
        Box::pin(async move {
            assert_context(&context);
            let mut remote = self.0.lock().await;
            remote.edits.push(edit.clone());
            let partial = remote.partial_removal;
            let playlist = remote.playlist.as_mut().unwrap();
            assert_eq!(playlist.playlist_id, id);
            let entries = &mut playlist.entries.items;
            match edit {
                PlaylistEdit::Update { title, description } => {
                    if let Some(title) = title {
                        playlist.title = title;
                    }
                    if let Some(description) = description {
                        playlist.description = (!description.is_empty()).then_some(description);
                    }
                }
                PlaylistEdit::SetPrivacy(privacy) => playlist.privacy = Some(privacy),
                PlaylistEdit::Remove(removals) => {
                    let count = if partial { 1 } else { removals.len() };
                    for removal in removals.into_iter().take(count) {
                        let at = entries
                            .iter()
                            .position(|entry| {
                                entry.entry_id.as_deref() == Some(removal.entry_id.as_str())
                                    && entry.track.video_id == removal.video_id
                            })
                            .unwrap();
                        entries.remove(at);
                    }
                }
                PlaylistEdit::Move {
                    entry_id,
                    before_entry_id,
                } => {
                    let at = entries
                        .iter()
                        .position(|entry| entry.entry_id.as_ref() == Some(&entry_id))
                        .unwrap();
                    let moved = entries.remove(at);
                    let before = before_entry_id.map_or(entries.len(), |id| {
                        entries
                            .iter()
                            .position(|entry| entry.entry_id.as_ref() == Some(&id))
                            .unwrap()
                    });
                    entries.insert(before, moved);
                }
                PlaylistEdit::Delete => {
                    remote.playlist.take();
                    return Ok(());
                }
            }
            if remote.reorder_on_removal {
                let entries = &mut remote.playlist.as_mut().unwrap().entries.items;
                entries.swap(0, 1);
            }
            let entries = &mut remote.playlist.as_mut().unwrap().entries.items;
            for (position, entry) in entries.iter_mut().enumerate() {
                entry.position = position as u32;
            }
            if partial {
                return Err(CatalogWriteError::Unknown(ControlError::new(
                    ErrorCode::ResultUnknown,
                    RetryDisposition::ReconcileFirst,
                    "provider reply did not establish the batch outcome",
                )));
            }
            Ok(())
        })
    }
}

/// A playlist entry of `video` at `position`.
fn entry(entry_id: &str, video: &str, position: usize) -> PlaylistEntry {
    PlaylistEntry {
        entry_id: Some(entry_id.into()),
        position: position as u32,
        track: Track {
            title: video.into(),
            video_id: VideoId::new(video).unwrap(),
            artists: vec![],
            album: None,
            duration_ms: None,
            availability: TrackAvailability::Available,
            version_tag: None,
            rating: Rating::Unknown,
        },
    }
}

fn assert_context(context: &CatalogReadContext) {
    assert_eq!(context.account_scope.as_str(), SCOPE);
    assert_eq!(context.account_epoch.as_str(), EPOCH);
    assert!(!context.cancel.is_cancelled());
}

struct PrivateCreationGrant;

impl DispatchAuthority for PrivateCreationGrant {
    fn authorize(&self, action: GrantAction) -> BoxFuture<'_, Result<DispatchGrant, ControlError>> {
        Box::pin(async move {
            if action != GrantAction::CreatePrivatePlaylist {
                return Err(ControlError::new(
                    ErrorCode::PermissionDenied,
                    RetryDisposition::NotRetryable,
                    "fixture only grants private playlist creation",
                ));
            }
            Ok(DispatchGrant {
                deadline_wall_ms: u64::MAX,
                editable_playlists: vec![],
            })
        })
    }
}

/// Grants everything; used for operations that must not pass through a test gate.
struct AnyGrant;

impl DispatchAuthority for AnyGrant {
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

/// Grants private-playlist work but holds any authorization made while the gated operation is
/// journaled as Submitted, so a test can cancel it between submission and dispatch.
struct SubmittedGate {
    journal: Arc<Journal>,
    operation_id: &'static str,
    reached: Notify,
    release: Notify,
}

impl DispatchAuthority for SubmittedGate {
    fn authorize(&self, action: GrantAction) -> BoxFuture<'_, Result<DispatchGrant, ControlError>> {
        Box::pin(async move {
            let submitted = self.journal.records.lock().await.iter().any(|entry| {
                entry.key.operation_id == self.operation_id
                    && entry.receipt.phase == OperationPhase::Submitted
            });
            if submitted {
                self.reached.notify_one();
                self.release.notified().await;
            }
            PrivateCreationGrant.authorize(action).await
        })
    }
}

struct EditGrant {
    action: GrantAction,
    playlist: PlaylistId,
}

impl DispatchAuthority for EditGrant {
    fn authorize(&self, action: GrantAction) -> BoxFuture<'_, Result<DispatchGrant, ControlError>> {
        Box::pin(async move {
            if action != self.action {
                return Err(ControlError::new(
                    ErrorCode::PermissionDenied,
                    RetryDisposition::NotRetryable,
                    "fixture granted a different playlist action",
                ));
            }
            Ok(DispatchGrant {
                deadline_wall_ms: u64::MAX,
                editable_playlists: vec![self.playlist.clone()],
            })
        })
    }
}

struct Harness {
    engine: CoreEngine,
    clock: Arc<MockClock>,
    catalog: Arc<Catalog>,
    service: Arc<ControlService>,
    journal: Arc<Journal>,
    grant: ReadGrant,
}

impl Harness {
    async fn new() -> Self {
        let catalog = Arc::<Catalog>::default();
        let journal = Arc::<Journal>::default();
        let clock = Arc::new(MockClock::new(1000));
        let engine = CoreEngine::start(
            CoreDeps {
                clock: clock.clone(),
                catalog: Some(catalog.clone()),
                ..Default::default()
            },
            EngineConfig::default(),
        )
        .with_playlist_editor(catalog.clone());
        engine
            .on_bridge_ready(
                1,
                BridgeReadyPayload {
                    bridge_epoch: BridgeEpoch::new("bridge-fixture").unwrap(),
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
                credential_revision: CredentialRevision::new("credential-fixture").unwrap(),
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
        let service = Arc::new(
            ControlService::new(engine.clone())
                .with_catalog(catalog.clone())
                .with_operations(journal.clone()),
        );
        let grant = ReadGrant::new(
            "fixture-harness".into(),
            Some(AccountScope::new(SCOPE).unwrap()),
        );
        Self {
            engine,
            clock,
            catalog,
            service,
            journal,
            grant,
        }
    }

    async fn submit(
        &self,
        id: &str,
        action: WriteAction,
    ) -> Result<OperationReceipt, ControlError> {
        self.submit_as(id, action, Arc::new(PrivateCreationGrant))
            .await
    }

    async fn submit_as(
        &self,
        id: &str,
        action: WriteAction,
        authority: Arc<dyn DispatchAuthority>,
    ) -> Result<OperationReceipt, ControlError> {
        self.service
            .submit(
                self.grant.clone(),
                authority,
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
    }

    /// Resource refresh events published since `deltas` subscribed, as (kind, id) pairs.
    async fn invalidated(
        &self,
        deltas: &mut tokio::sync::broadcast::Receiver<StateDelta>,
    ) -> Vec<(String, String)> {
        // A snapshot round trip orders every earlier engine message before the drain.
        self.engine.snapshot().await.unwrap();
        let mut events = Vec::new();
        while let Ok(delta) = deltas.try_recv() {
            if let StateDelta::ResourceInvalidated {
                resource_kind,
                resource_id: Some(id),
                ..
            } = delta
            {
                events.push((resource_kind, id));
            }
        }
        events
    }

    async fn finished(&self, id: &str) -> OperationReceipt {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let response = self
                    .service
                    .execute(
                        &self.grant,
                        ControlRequest::GetOperation(OperationInput {
                            operation_id: id.into(),
                        }),
                    )
                    .await;
                let ControlResponse::Operation(receipt) = response else {
                    panic!("operation should exist: {response:?}");
                };
                if receipt.scheduling_stopped {
                    return receipt;
                }
                self.journal.changed.notified().await;
            }
        })
        .await
        .expect("operation did not complete")
    }
}

fn create(title: &str) -> WriteAction {
    WriteAction::CreatePrivatePlaylist {
        title: title.into(),
        description: Some("Synthetic playlist".into()),
    }
}

fn append(playlist_id: &str, track_ids: [&str; 3]) -> WriteAction {
    WriteAction::AddPlaylistEntries {
        playlist_id: PlaylistId::new(playlist_id).unwrap(),
        created_by_operation_id: Some("create".into()),
        video_ids: track_ids
            .into_iter()
            .map(|id| VideoId::new(id).unwrap())
            .collect(),
    }
}

fn recovery(target: &OperationReceipt, action: ResumeAction) -> WriteAction {
    WriteAction::ResumeOperation {
        target_operation_id: target.operation_id.clone(),
        expected_operation_revision: target.revision,
        action,
    }
}

#[tokio::test]
async fn private_playlist_creation_verifies_reads_deduplicates_and_limits_derived_append() {
    let harness = Harness::new().await;
    let first = harness
        .submit("create", create("Private mix"))
        .await
        .unwrap();
    assert_eq!(first.phase, OperationPhase::Accepted);

    let created = harness.finished("create").await;
    let id = PlaylistId::new(CREATED_PLAYLIST).unwrap();
    assert_eq!(created.phase, OperationPhase::Verified);
    assert!(created.settled);
    assert_eq!(
        created.resource,
        Some(OperationResource::Playlist { id: id.clone() })
    );
    assert_eq!(
        created.verification.as_ref().unwrap().level,
        VerificationLevel::ReadBack
    );
    assert_eq!(
        harness
            .submit("create", create("Private mix"))
            .await
            .unwrap(),
        created
    );
    assert_eq!(
        harness
            .submit("create", create("Changed input"))
            .await
            .unwrap_err()
            .code,
        ErrorCode::IdempotencyConflict,
    );
    assert_eq!(harness.catalog.0.lock().await.writes.len(), 1);

    harness
        .submit("wrong-resource", append("PL_not_created", ["a", "b", "c"]))
        .await
        .unwrap();
    let forbidden = harness.finished("wrong-resource").await;
    assert_eq!(forbidden.phase, OperationPhase::Rejected);
    assert_eq!(forbidden.error.unwrap().code, ErrorCode::PermissionDenied);
    assert_eq!(harness.catalog.0.lock().await.writes.len(), 1);

    harness
        .submit("append", append(CREATED_PLAYLIST, ["a", "b", "c"]))
        .await
        .unwrap();
    let appended = harness.finished("append").await;
    assert_eq!(appended.phase, OperationPhase::Verified);
    assert!(
        appended
            .item_results
            .iter()
            .all(|item| item.phase == ItemPhase::Verified)
    );
    {
        let remote = harness.catalog.0.lock().await;
        assert_eq!(remote.invalidations, vec![id.clone(), id]);
        assert_eq!(remote.reads, 3);
        let entries = &remote.playlist.as_ref().unwrap().entries.items;
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.track.video_id.as_str())
                .collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
        assert_eq!(
            appended
                .item_results
                .iter()
                .map(|item| item.resource_entry_id.as_deref().unwrap())
                .collect::<Vec<_>>(),
            ["occurrence-0", "occurrence-1", "occurrence-2"]
        );
    }
    harness.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn lost_create_ack_stays_unknown_and_never_reissues_creation() {
    let harness = Harness::new().await;
    harness.catalog.0.lock().await.lose_create_reply = true;
    harness
        .submit("create", create("Unknown private mix"))
        .await
        .unwrap();
    let unknown = harness.finished("create").await;
    assert_eq!(unknown.phase, OperationPhase::Unknown);
    assert!(!unknown.settled);
    assert_eq!(unknown.retry_disposition, RetryDisposition::ReconcileFirst);
    assert_eq!(
        harness
            .submit("create", create("Unknown private mix"))
            .await
            .unwrap(),
        unknown
    );
    harness
        .submit("reconcile", recovery(&unknown, ResumeAction::Reconcile))
        .await
        .unwrap();
    let reconcile = harness.finished("reconcile").await;
    assert_eq!(reconcile.error.unwrap().code, ErrorCode::ResultUnknown);
    assert_eq!(harness.finished("create").await, unknown);
    harness
        .submit("unsent", recovery(&unknown, ResumeAction::ResumeUnsent))
        .await
        .unwrap();
    let unsent = harness.finished("unsent").await;
    assert_eq!(unsent.retry_disposition, RetryDisposition::NewPlanRequired);
    harness
        .submit("append", append(CREATED_PLAYLIST, ["a", "b", "c"]))
        .await
        .unwrap();
    assert_eq!(
        harness.finished("append").await.phase,
        OperationPhase::Rejected
    );
    let remote = harness.catalog.0.lock().await;
    assert_eq!(remote.writes.len(), 1);
    assert!(remote.playlist.is_some());
    drop(remote);
    harness.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn acknowledged_create_cannot_verify_against_a_stale_readback() {
    let harness = Harness::new().await;
    harness.catalog.0.lock().await.stale_readback = true;
    harness
        .submit("create", create("Private mix"))
        .await
        .unwrap();

    let receipt = harness.finished("create").await;
    assert_eq!(receipt.phase, OperationPhase::Unknown);
    assert_eq!(
        receipt.resource,
        Some(OperationResource::Playlist {
            id: PlaylistId::new(CREATED_PLAYLIST).unwrap(),
        })
    );
    assert!(receipt.verification.is_none());
    assert!(!receipt.settled);
    harness.catalog.0.lock().await.stale_readback = false;
    let mut outdated = receipt.clone();
    outdated.revision -= 1;
    harness
        .submit(
            "stale-recovery",
            recovery(&outdated, ResumeAction::Reconcile),
        )
        .await
        .unwrap();
    assert_eq!(
        harness.finished("stale-recovery").await.error.unwrap().code,
        ErrorCode::Conflict
    );
    let action = recovery(&receipt, ResumeAction::Reconcile);
    harness.submit("reconcile", action.clone()).await.unwrap();
    let request = harness.finished("reconcile").await;
    assert_eq!(request.phase, OperationPhase::Verified);
    assert_eq!(
        request.resource,
        Some(OperationResource::Operation {
            id: "create".into()
        })
    );
    let reconciled = harness.finished("create").await;
    assert_eq!(reconciled.phase, OperationPhase::Verified);
    assert_eq!(reconciled.revision, receipt.revision + 1);
    assert_eq!(reconciled.resource, receipt.resource);
    assert_eq!(harness.submit("reconcile", action).await.unwrap(), request);
    let remote = harness.catalog.0.lock().await;
    assert_eq!(remote.writes.len(), 1);
    assert_eq!(remote.reads, 2);
    drop(remote);
    harness.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn reversed_remote_append_is_unknown_despite_accepting_every_item() {
    let harness = Harness::new().await;
    harness
        .submit("create", create("Private mix"))
        .await
        .unwrap();
    assert_eq!(
        harness.finished("create").await.phase,
        OperationPhase::Verified
    );

    harness.catalog.0.lock().await.reverse_append = true;
    harness
        .submit("attach", append(CREATED_PLAYLIST, ["a", "b", "c"]))
        .await
        .unwrap();
    let result = harness.finished("attach").await;
    assert_eq!(result.phase, OperationPhase::Unknown);
    assert!(
        result
            .item_results
            .iter()
            .all(|item| item.phase == ItemPhase::Unknown)
    );
    let action = recovery(&result, ResumeAction::Reconcile);
    harness
        .submit("still-reversed", action.clone())
        .await
        .unwrap();
    assert_eq!(
        harness.finished("still-reversed").await.error.unwrap().code,
        ErrorCode::ResultUnknown
    );
    assert_eq!(harness.finished("attach").await, result);
    {
        let mut remote = harness.catalog.0.lock().await;
        let entries = &mut remote.playlist.as_mut().unwrap().entries.items;
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.track.video_id.as_str())
                .collect::<Vec<_>>(),
            ["c", "b", "a"]
        );
        entries.reverse();
        for (position, entry) in entries.iter_mut().enumerate() {
            entry.position = position as u32;
        }
    }
    harness.submit("now-ordered", action).await.unwrap();
    assert_eq!(
        harness.finished("now-ordered").await.phase,
        OperationPhase::Verified
    );
    let resolved = harness.finished("attach").await;
    assert_eq!(resolved.phase, OperationPhase::Verified);
    assert_eq!(resolved.revision, result.revision + 1);
    assert!(
        resolved
            .item_results
            .iter()
            .zip(&result.item_results)
            .all(|(current, old)| {
                current.phase == ItemPhase::Verified
                    && current.resource_entry_id == old.resource_entry_id
            })
    );
    assert_eq!(harness.catalog.0.lock().await.writes.len(), 2);
    harness.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn rating_recovery_requires_the_exact_readback_and_does_not_rate_again() {
    let harness = Harness::new().await;
    let grant = Arc::new(EditGrant {
        action: GrantAction::Rating,
        playlist: PlaylistId::new(CREATED_PLAYLIST).unwrap(),
    });
    let action = WriteAction::Rate {
        target: TrackTarget::Video {
            video_id: VideoId::new(RATED_VIDEO).unwrap(),
        },
        rating: Rating::Disliked,
    };
    harness
        .submit_as("rate", action, grant.clone())
        .await
        .unwrap();
    let unknown = harness.finished("rate").await;
    assert_eq!(unknown.phase, OperationPhase::Unknown);
    harness.catalog.0.lock().await.rating = Some(Rating::Neutral);
    let action = recovery(&unknown, ResumeAction::Reconcile);
    harness
        .submit_as("wrong-rating", action.clone(), grant.clone())
        .await
        .unwrap();
    assert_eq!(
        harness.finished("wrong-rating").await.error.unwrap().code,
        ErrorCode::ResultUnknown
    );
    assert_eq!(harness.finished("rate").await, unknown);
    harness.catalog.0.lock().await.rating = Some(Rating::Disliked);
    harness
        .submit_as("exact-rating", action, grant)
        .await
        .unwrap();
    assert_eq!(
        harness.finished("exact-rating").await.phase,
        OperationPhase::Verified
    );
    let target = harness.finished("rate").await;
    assert_eq!(
        target.verification.unwrap().level,
        VerificationLevel::ReadBack
    );
    assert_eq!(harness.catalog.0.lock().await.rating_writes, 1);
    harness.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn album_membership_recovery_requires_the_original_audio_playlist_identity() {
    let h = Harness::new().await;
    let album = LibraryMembershipTarget::Album {
        id: AlbumId::new("MPREb_fixture").unwrap(),
    };
    let initial = PlaylistId::new("OLAK5uy_original_fixture").unwrap();
    let reissued = PlaylistId::new("OLAK5uy_reissued_fixture").unwrap();
    let grant = Arc::new(EditGrant {
        action: GrantAction::LibraryMembership,
        playlist: initial.clone(),
    });
    let request = WriteAction::SetLibraryMembership {
        target: album.clone(),
        saved: true,
    };
    {
        let mut remote = h.catalog.0.lock().await;
        remote.album_membership = Some((initial.clone(), false));
        remote.reissued_album = Some(reissued);
    }
    h.submit_as("save-album", request.clone(), grant.clone())
        .await
        .unwrap();
    let unknown = h.finished("save-album").await;
    assert_eq!(unknown.phase, OperationPhase::Unknown);
    assert_eq!(
        unknown.resource,
        Some(OperationResource::LibraryMembership {
            target: album.clone(),
            saved: None,
            album_playlist_id: Some(initial.clone()),
        })
    );

    h.submit_as(
        "reconcile-reissue",
        recovery(&unknown, ResumeAction::Reconcile),
        grant.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        h.finished("reconcile-reissue").await.phase,
        OperationPhase::Rejected
    );
    assert_eq!(
        h.submit_as("save-album", request.clone(), grant.clone())
            .await
            .unwrap(),
        unknown
    );

    h.catalog.0.lock().await.album_membership = Some((initial.clone(), true));
    h.submit_as(
        "reconcile-exact-release",
        recovery(&unknown, ResumeAction::Reconcile),
        grant.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        h.finished("reconcile-exact-release").await.phase,
        OperationPhase::Verified
    );
    let observed = h.submit_as("save-album", request, grant).await.unwrap();
    assert_eq!(
        observed.resource,
        Some(OperationResource::LibraryMembership {
            target: album,
            saved: Some(true),
            album_playlist_id: Some(initial.clone()),
        })
    );
    assert_eq!(h.catalog.0.lock().await.album_writes, [initial]);
    h.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn membership_writes_use_exact_target_readback_without_replaying_an_unknown_result() {
    let h = Harness::new().await;
    let song = LibraryMembershipTarget::Song {
        id: VideoId::new("song-fixture").unwrap(),
    };
    let list_id = PlaylistId::new("PL_fixture_membership").unwrap();
    let playlist = LibraryMembershipTarget::Playlist {
        id: list_id.clone(),
    };
    let grant = Arc::new(EditGrant {
        action: GrantAction::LibraryMembership,
        playlist: list_id,
    });
    let action = |target, saved| WriteAction::SetLibraryMembership { target, saved };
    let receipt_for = |target, saved| {
        Some(OperationResource::LibraryMembership {
            target,
            saved,
            album_playlist_id: None,
        })
    };
    {
        let mut remote = h.catalog.0.lock().await;
        remote.memberships.insert(song.clone(), true);
        remote.memberships.insert(playlist.clone(), false);
    }
    let wrong_grant = h
        .submit("wrong-grant", action(song.clone(), false))
        .await
        .unwrap_err();
    assert_eq!(wrong_grant.code, ErrorCode::PermissionDenied);

    let missing = LibraryMembershipTarget::Song {
        id: VideoId::new("different-song").unwrap(),
    };
    h.submit_as("missing", action(missing, true), grant.clone())
        .await
        .unwrap();
    let missing = h.finished("missing").await;
    assert_eq!(missing.phase, OperationPhase::Rejected);
    assert_eq!(
        missing.error.unwrap().code,
        ErrorCode::CapabilityUnavailable
    );
    h.submit_as("already", action(song.clone(), true), grant.clone())
        .await
        .unwrap();
    let already = h.finished("already").await;
    assert_eq!(
        already.verification.unwrap().level,
        VerificationLevel::ReadBack
    );
    assert!(h.catalog.0.lock().await.membership_writes.is_empty());

    h.submit_as("save-list", action(playlist.clone(), true), grant.clone())
        .await
        .unwrap();
    let saved = h.finished("save-list").await;
    assert_eq!(saved.resource, receipt_for(playlist.clone(), Some(true)));
    assert_eq!(
        saved.verification.unwrap().level,
        VerificationLevel::ReadBack
    );
    h.submit_as(
        "remove-list",
        action(playlist.clone(), false),
        grant.clone(),
    )
    .await
    .unwrap();
    let removed = h.finished("remove-list").await;
    assert_eq!(removed.resource, receipt_for(playlist.clone(), Some(false)));
    assert_eq!(
        removed.verification.unwrap().level,
        VerificationLevel::ReadBack
    );

    h.catalog.0.lock().await.ignore_membership_write = true;
    let request = action(song.clone(), false);
    h.submit_as("remove-song", request.clone(), grant.clone())
        .await
        .unwrap();
    let unknown = h.finished("remove-song").await;
    assert_eq!(unknown.phase, OperationPhase::Unknown);
    assert_eq!(unknown.resource, receipt_for(song.clone(), None));
    assert_eq!(
        h.submit_as("remove-song", request.clone(), grant.clone())
            .await
            .unwrap(),
        unknown
    );
    let conflict = h
        .submit_as(
            "remove-song",
            action(playlist.clone(), false),
            grant.clone(),
        )
        .await
        .unwrap_err();
    assert_eq!(conflict.code, ErrorCode::IdempotencyConflict);

    h.catalog
        .0
        .lock()
        .await
        .memberships
        .insert(song.clone(), false);
    h.submit_as(
        "reconcile-membership",
        recovery(&unknown, ResumeAction::Reconcile),
        grant.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        h.finished("reconcile-membership").await.phase,
        OperationPhase::Verified
    );
    let observed = h.submit_as("remove-song", request, grant).await.unwrap();
    assert_eq!(observed.resource, receipt_for(song.clone(), Some(false)));
    assert_eq!(
        observed.verification.unwrap().level,
        VerificationLevel::ReadBack
    );
    {
        let remote = h.catalog.0.lock().await;
        assert_eq!(
            remote.membership_writes,
            [
                (playlist.clone(), true),
                (playlist.clone(), false),
                (song.clone(), false)
            ]
        );
        assert_eq!(
            remote.membership_invalidations,
            [song.clone(), playlist.clone(), playlist, song.clone(), song]
        );
        assert!(remote.writes.is_empty());
        assert!(remote.edits.is_empty());
    }
    h.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn artist_subscription_requires_its_own_grant_and_readback_of_the_exact_channel() {
    let harness = Harness::new().await;
    let grant = Arc::new(EditGrant {
        action: GrantAction::ArtistSubscription,
        playlist: PlaylistId::new(CREATED_PLAYLIST).unwrap(),
    });
    let id = ArtistId::new("UC-fixture-subscription").unwrap();
    let action = WriteAction::SetArtistSubscription {
        artist_id: id.clone(),
        subscribed: true,
    };
    assert_eq!(
        harness
            .submit("denied-subscribe", action.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    harness
        .submit_as("unsupported-subscribe", action.clone(), grant.clone())
        .await
        .unwrap();
    assert_eq!(
        harness
            .finished("unsupported-subscribe")
            .await
            .error
            .unwrap()
            .code,
        ErrorCode::CapabilityUnavailable
    );
    assert!(
        harness
            .catalog
            .0
            .lock()
            .await
            .subscription_writes
            .is_empty()
    );
    harness.catalog.0.lock().await.subscribed = Some(false);
    harness
        .submit_as("subscribe", action.clone(), grant.clone())
        .await
        .unwrap();
    let unknown = harness.finished("subscribe").await;
    assert_eq!(unknown.phase, OperationPhase::Unknown);
    assert_eq!(
        harness
            .submit_as("subscribe", action.clone(), grant.clone())
            .await
            .unwrap(),
        unknown
    );
    harness.catalog.0.lock().await.subscribed = Some(true);
    harness
        .submit_as(
            "verify-subscribe",
            recovery(&unknown, ResumeAction::Reconcile),
            grant.clone(),
        )
        .await
        .unwrap();
    assert_eq!(
        harness.finished("verify-subscribe").await.phase,
        OperationPhase::Verified
    );
    assert_eq!(
        harness.finished("subscribe").await.resource,
        Some(OperationResource::Artist {
            id,
            subscribed: Some(true)
        })
    );
    harness
        .submit_as("already-subscribed", action, grant)
        .await
        .unwrap();
    assert_eq!(
        harness.finished("already-subscribed").await.phase,
        OperationPhase::Verified
    );
    assert_eq!(harness.catalog.0.lock().await.subscription_writes, [true]);
    harness.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn occurrence_edits_keep_duplicate_songs_distinct_and_verify_partial_removals() {
    let harness = Harness::new().await;
    let id = PlaylistId::new(CREATED_PLAYLIST).unwrap();
    harness
        .submit("create", create("Private mix"))
        .await
        .unwrap();
    assert_eq!(
        harness.finished("create").await.phase,
        OperationPhase::Verified
    );
    harness
        .submit("append", append(CREATED_PLAYLIST, ["a", "b", "c"]))
        .await
        .unwrap();
    assert_eq!(
        harness.finished("append").await.phase,
        OperationPhase::Verified
    );
    {
        let mut remote = harness.catalog.0.lock().await;
        let entries = &mut remote.playlist.as_mut().unwrap().entries.items;
        entries[1].track = entries[0].track.clone();
    }
    let grant = Arc::new(EditGrant {
        action: GrantAction::EditPlaylist,
        playlist: id.clone(),
    });
    harness
        .submit_as(
            "move",
            WriteAction::MovePlaylistEntry {
                playlist_id: id.clone(),
                entry_id: "occurrence-1".into(),
                before_entry_id: Some("occurrence-0".into()),
            },
            grant.clone(),
        )
        .await
        .unwrap();
    let moved = harness.finished("move").await;
    assert_eq!(moved.phase, OperationPhase::Verified);
    assert_eq!(
        moved.item_results[0].resource_entry_id.as_deref(),
        Some("occurrence-1")
    );

    harness.catalog.0.lock().await.partial_removal = true;
    harness
        .submit_as(
            "partial-remove",
            WriteAction::RemovePlaylistEntries {
                playlist_id: id.clone(),
                entry_ids: vec!["occurrence-0".into(), "occurrence-2".into()],
            },
            grant.clone(),
        )
        .await
        .unwrap();
    let removed = harness.finished("partial-remove").await;
    assert_eq!(removed.phase, OperationPhase::Partial);
    assert!(!removed.settled);
    assert_eq!(removed.item_results[0].phase, ItemPhase::Verified);
    assert_eq!(removed.item_results[1].phase, ItemPhase::Unknown);
    // The ledger names each requested occurrence, its song and where it stood before.
    let ledger = |entry: &str, video: &str, position| RemovedItem::PlaylistEntry {
        playlist_id: id.clone(),
        entry_id: entry.into(),
        video_id: VideoId::new(video).unwrap(),
        position,
    };
    assert_eq!(
        removed.removed,
        [
            ledger("occurrence-0", "a", 1),
            ledger("occurrence-2", "c", 2)
        ]
    );

    harness
        .submit_as(
            "stale-anchor",
            WriteAction::MovePlaylistEntry {
                playlist_id: id,
                entry_id: "occurrence-1".into(),
                before_entry_id: Some("occurrence-0".into()),
            },
            grant,
        )
        .await
        .unwrap();
    assert_eq!(
        harness.finished("stale-anchor").await.error.unwrap().code,
        ErrorCode::StaleContext
    );
    let remote = harness.catalog.0.lock().await;
    assert_eq!(remote.edits.len(), 2);
    assert_eq!(
        remote
            .playlist
            .as_ref()
            .unwrap()
            .entries
            .items
            .iter()
            .map(|entry| (
                entry.entry_id.as_deref().unwrap(),
                entry.track.video_id.as_str()
            ))
            .collect::<Vec<_>>(),
        [("occurrence-1", "a"), ("occurrence-2", "c")]
    );
    drop(remote);
    harness.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn playlist_privacy_is_refused_and_delete_needs_its_own_grant_and_unknown_absence_never_verifies()
 {
    let harness = Harness::new().await;
    let id = PlaylistId::new(CREATED_PLAYLIST).unwrap();
    harness
        .submit("create", create("Private mix"))
        .await
        .unwrap();
    assert_eq!(
        harness.finished("create").await.phase,
        OperationPhase::Verified
    );
    let edit = Arc::new(EditGrant {
        action: GrantAction::EditPlaylist,
        playlist: id.clone(),
    });
    let make_unlisted = WriteAction::SetPlaylistPrivacy {
        playlist_id: id.clone(),
        privacy: PlaylistPrivacy::Unlisted,
    };
    let error = harness
        .submit_as("privacy-blocked", make_unlisted.clone(), edit.clone())
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
    assert!(harness.catalog.0.lock().await.edits.is_empty());
    // Only the listener changes privacy; even a privacy grant naming the playlist is refused.
    let privacy = Arc::new(EditGrant {
        action: GrantAction::ChangePlaylistPrivacy,
        playlist: id.clone(),
    });
    let error = harness
        .submit_as("privacy", make_unlisted, privacy.clone())
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
    assert!(error.message.contains("only the listener"));
    assert!(harness.catalog.0.lock().await.edits.is_empty());
    harness
        .submit_as(
            "update",
            WriteAction::UpdatePlaylist {
                playlist_id: id.clone(),
                title: Some("Updated".into()),
                description: Some(String::new()),
            },
            edit,
        )
        .await
        .unwrap();
    assert_eq!(
        harness.finished("update").await.phase,
        OperationPhase::Verified
    );
    {
        let remote = harness.catalog.0.lock().await;
        let playlist = remote.playlist.as_ref().unwrap();
        assert_eq!(playlist.privacy, Some(PlaylistPrivacy::Private));
        assert!(playlist.description.is_none());
    }
    let deletion = WriteAction::DeletePlaylist {
        playlist_id: id.clone(),
    };
    let error = harness
        .submit_as("delete-blocked", deletion.clone(), privacy)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
    let delete = Arc::new(EditGrant {
        action: GrantAction::DeletePlaylist,
        playlist: id.clone(),
    });
    let other_id = PlaylistId::new("PL_other_fixture").unwrap();
    let wrong_target = Arc::new(EditGrant {
        action: GrantAction::DeletePlaylist,
        playlist: other_id.clone(),
    });
    harness
        .submit_as("delete-other-grant", deletion.clone(), wrong_target)
        .await
        .unwrap();
    assert_eq!(
        harness
            .finished("delete-other-grant")
            .await
            .error
            .unwrap()
            .code,
        ErrorCode::PermissionDenied,
    );
    {
        let mut remote = harness.catalog.0.lock().await;
        remote.returned_playlist_id = Some(other_id.clone());
    }
    harness
        .submit_as("delete-wrong-read", deletion.clone(), delete.clone())
        .await
        .unwrap();
    assert_eq!(
        harness.finished("delete-wrong-read").await.phase,
        OperationPhase::Rejected
    );
    {
        let mut remote = harness.catalog.0.lock().await;
        remote.returned_playlist_id = None;
        remote.playlist.as_mut().unwrap().editable = None;
    }
    harness
        .submit_as("delete-unconfirmed-owner", deletion.clone(), delete.clone())
        .await
        .unwrap();
    assert_eq!(
        harness
            .finished("delete-unconfirmed-owner")
            .await
            .error
            .unwrap()
            .code,
        ErrorCode::NotEditable
    );
    {
        let mut remote = harness.catalog.0.lock().await;
        assert_eq!(remote.edits.len(), 1);
        remote.playlist.as_mut().unwrap().editable = Some(true);
    }

    harness
        .submit_as("delete", deletion.clone(), delete.clone())
        .await
        .unwrap();
    let result = harness.finished("delete").await;
    assert_eq!(result.phase, OperationPhase::Unknown);
    assert_eq!(result.retry_disposition, RetryDisposition::ReconcileFirst);
    assert_eq!(result.resource, Some(OperationResource::Playlist { id }));
    assert!(result.verification.is_none());
    assert_eq!(
        harness
            .submit_as("delete", deletion, delete.clone())
            .await
            .unwrap(),
        result
    );
    assert_eq!(
        harness
            .submit_as(
                "delete",
                WriteAction::DeletePlaylist {
                    playlist_id: other_id
                },
                delete
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::IdempotencyConflict,
    );
    assert_eq!(harness.catalog.0.lock().await.edits.len(), 2);
    harness.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_append_with_a_lost_reply_still_refreshes_the_playlist() {
    let harness = Harness::new().await;
    let id = PlaylistId::new(CREATED_PLAYLIST).unwrap();
    harness
        .submit("create", create("Private mix"))
        .await
        .unwrap();
    assert_eq!(
        harness.finished("create").await.phase,
        OperationPhase::Verified
    );
    let (_, mut deltas) = harness.engine.subscribe_snapshot().await.unwrap();
    harness.catalog.0.lock().await.lose_append_reply = true;
    harness
        .submit("append", append(CREATED_PLAYLIST, ["a", "b", "c"]))
        .await
        .unwrap();

    let unknown = harness.finished("append").await;
    assert_eq!(unknown.phase, OperationPhase::Unknown);
    assert!(
        unknown
            .item_results
            .iter()
            .all(|item| item.phase == ItemPhase::Submitted)
    );
    assert_eq!(
        harness.catalog.0.lock().await.invalidations,
        vec![id.clone(), id.clone()]
    );
    let events = harness.invalidated(&mut deltas).await;
    for kind in ["playlistTracks", "libraryPlaylists"] {
        assert!(
            events.contains(&(kind.into(), id.as_str().into())),
            "{kind}: {events:?}"
        );
    }
    harness.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_append_cancelled_before_dispatch_cancels_every_item() {
    let harness = Harness::new().await;
    harness
        .submit("create", create("Private mix"))
        .await
        .unwrap();
    assert_eq!(
        harness.finished("create").await.phase,
        OperationPhase::Verified
    );
    let gate = Arc::new(SubmittedGate {
        journal: harness.journal.clone(),
        operation_id: "append",
        reached: Notify::new(),
        release: Notify::new(),
    });
    harness
        .submit_as(
            "append",
            append(CREATED_PLAYLIST, ["a", "b", "c"]),
            gate.clone(),
        )
        .await
        .unwrap();
    gate.reached.notified().await;
    harness
        .submit_as(
            "cancel-append",
            WriteAction::Cancel {
                target_operation_id: "append".into(),
            },
            Arc::new(AnyGrant),
        )
        .await
        .unwrap();
    assert_eq!(
        harness.finished("cancel-append").await.phase,
        OperationPhase::Verified
    );
    gate.release.notify_one();

    let cancelled = harness.finished("append").await;
    assert_eq!(cancelled.phase, OperationPhase::Cancelled);
    assert_eq!(cancelled.item_results.len(), 3);
    assert!(
        cancelled
            .item_results
            .iter()
            .all(|item| item.phase == ItemPhase::Cancelled)
    );
    let remote = harness.catalog.0.lock().await;
    assert_eq!(remote.writes.len(), 1);
    assert_eq!(remote.invalidations.len(), 1);
    drop(remote);
    harness.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_unconfirmed_rating_still_refreshes_and_reconcile_refreshes_again() {
    let harness = Harness::new().await;
    let grant = Arc::new(EditGrant {
        action: GrantAction::Rating,
        playlist: PlaylistId::new(CREATED_PLAYLIST).unwrap(),
    });
    let (_, mut deltas) = harness.engine.subscribe_snapshot().await.unwrap();
    harness
        .submit_as(
            "rate",
            WriteAction::Rate {
                target: TrackTarget::Video {
                    video_id: VideoId::new(RATED_VIDEO).unwrap(),
                },
                rating: Rating::Disliked,
            },
            grant.clone(),
        )
        .await
        .unwrap();

    let unknown = harness.finished("rate").await;
    assert_eq!(unknown.phase, OperationPhase::Unknown);
    assert_eq!(harness.catalog.0.lock().await.rating_invalidations, 1);
    let events = harness.invalidated(&mut deltas).await;
    for kind in ["rating", "likedSongs"] {
        assert!(
            events.contains(&(kind.into(), RATED_VIDEO.into())),
            "{kind}: {events:?}"
        );
    }

    harness.catalog.0.lock().await.rating = Some(Rating::Disliked);
    harness
        .submit_as(
            "reconcile-rate",
            recovery(&unknown, ResumeAction::Reconcile),
            grant,
        )
        .await
        .unwrap();
    assert_eq!(
        harness.finished("reconcile-rate").await.phase,
        OperationPhase::Verified
    );
    assert_eq!(harness.catalog.0.lock().await.rating_invalidations, 2);
    assert!(
        harness
            .invalidated(&mut deltas)
            .await
            .contains(&("rating".into(), RATED_VIDEO.into()))
    );
    harness.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_append_of_a_track_already_present_is_refused_before_anything_is_sent() {
    let harness = Harness::new().await;
    harness
        .submit("create", create("Private mix"))
        .await
        .unwrap();
    assert_eq!(
        harness.finished("create").await.phase,
        OperationPhase::Verified
    );
    harness
        .submit("append", append(CREATED_PLAYLIST, ["a", "b", "c"]))
        .await
        .unwrap();
    assert_eq!(
        harness.finished("append").await.phase,
        OperationPhase::Verified
    );
    let invalidations = harness.catalog.0.lock().await.invalidations.len();
    let writes = harness.catalog.0.lock().await.writes.len();
    harness
        .submit("append-again", append(CREATED_PLAYLIST, ["d", "b", "e"]))
        .await
        .unwrap();
    let refused = harness.finished("append-again").await;
    assert_eq!(refused.phase, OperationPhase::Rejected);
    assert_eq!(refused.error.unwrap().code, ErrorCode::Conflict);
    // Nothing was sent: no provider write followed and nothing needed invalidating.
    let catalog = harness.catalog.0.lock().await;
    assert_eq!(catalog.writes.len(), writes);
    assert_eq!(catalog.invalidations.len(), invalidations);
    drop(catalog);
    harness.engine.shutdown().await.unwrap();
}

const UI_PLAYLIST: &str = "PL_fixture_ui";

impl Harness {
    /// A playlist the listener can add to: `videos` in order, entry IDs `pre-<n>`.
    async fn ui_playlist(&self, videos: &[&str]) {
        self.catalog.0.lock().await.playlist = Some(Playlist {
            playlist_id: PlaylistId::new(UI_PLAYLIST).unwrap(),
            identity_confirmed: true,
            title: "Fixture mix".into(),
            description: None,
            privacy: Some(PlaylistPrivacy::Private),
            editable: Some(true),
            snapshot_fingerprint: None,
            entries: Page {
                items: videos
                    .iter()
                    .enumerate()
                    .map(|(n, video)| entry(&format!("pre-{n}"), video, n))
                    .collect(),
                next_cursor: None,
                complete: true,
                source: "fixture".into(),
                freshness: Freshness::Fresh,
                captured_at_ms: 1000,
            },
        });
    }

    /// The app's own add, as the player window sends it.
    async fn ui_add(&self, video: &str) -> MutationResult {
        let request = MutationRequest {
            kind: MutationKind::AddToPlaylist,
            video_id: VideoId::new(video).unwrap(),
            liked: None,
            playlist_id: Some(PlaylistId::new(UI_PLAYLIST).unwrap()),
            undo_token: None,
            confirmed_by_user: true,
        };
        self.engine
            .mutate(request, Some(AccountEpoch::new(EPOCH).unwrap()))
            .await
            .unwrap()
    }

    /// The Undo of an addition, in the account session `epoch`.
    async fn ui_undo(&self, video: &str, token: &str, epoch: &str) -> MutationResult {
        let request = MutationRequest {
            kind: MutationKind::UndoAddToPlaylist,
            video_id: VideoId::new(video).unwrap(),
            liked: None,
            playlist_id: Some(PlaylistId::new(UI_PLAYLIST).unwrap()),
            undo_token: Some(token.into()),
            confirmed_by_user: true,
        };
        self.engine
            .mutate(request, Some(AccountEpoch::new(epoch).unwrap()))
            .await
            .unwrap()
    }

    /// The remote playlist as `(entryId, videoId)` in order.
    async fn ui_entries(&self) -> Vec<(String, String)> {
        let remote = self.catalog.0.lock().await;
        remote
            .playlist
            .as_ref()
            .unwrap()
            .entries
            .items
            .iter()
            .map(|entry| {
                (
                    entry.entry_id.clone().unwrap(),
                    entry.track.video_id.as_str().to_owned(),
                )
            })
            .collect()
    }
}

fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
    items
        .iter()
        .map(|(entry, video)| ((*entry).to_owned(), (*video).to_owned()))
        .collect()
}

/// Undo removes the one entry the addition created even when the same song is in the playlist
/// twice, confirms it by a fresh read, and is logged as the listener's own action.
#[tokio::test]
async fn ui_undo_removes_only_the_entry_its_addition_created() {
    let harness = Harness::new().await;
    harness.ui_playlist(&["first", "second"]).await;
    let mut deltas = harness.engine.subscribe_snapshot().await.unwrap().1;
    let added = harness.ui_add("song").await;
    assert_eq!(added.outcome, MutationOutcome::Confirmed);
    let token = added.undo_token.expect("an addition that named its entry");
    assert!(!token.contains("ui-occurrence"), "the token is opaque");
    // Another device adds the same song again, ahead of the app's copy.
    {
        let mut remote = harness.catalog.0.lock().await;
        let items = &mut remote.playlist.as_mut().unwrap().entries.items;
        items.insert(1, entry("elsewhere", "song", 1));
        for (position, item) in items.iter_mut().enumerate() {
            item.position = position as u32;
        }
    }
    harness.invalidated(&mut deltas).await;

    let undone = harness.ui_undo("song", &token, EPOCH).await;
    assert_eq!(undone.outcome, MutationOutcome::Confirmed, "{undone:?}");
    assert!(!undone.already_removed && undone.undo_token.is_none());
    assert_eq!(
        harness.ui_entries().await,
        pairs(&[
            ("pre-0", "first"),
            ("elsewhere", "song"),
            ("pre-1", "second")
        ])
    );
    let remote = harness.catalog.0.lock().await;
    assert_eq!(
        remote.edits,
        [PlaylistEdit::Remove(vec![PlaylistOccurrenceIdentity {
            video_id: VideoId::new("song").unwrap(),
            entry_id: "ui-occurrence-2".into(),
        }])]
    );
    assert_eq!(
        remote.invalidations,
        [PlaylistId::new(UI_PLAYLIST).unwrap()]
    );
    drop(remote);
    assert_eq!(
        harness.invalidated(&mut deltas).await,
        [("playlistTracks".to_owned(), UI_PLAYLIST.to_owned()),]
    );
    let ControlResponse::Context(context) = harness
        .service
        .execute(
            &harness.grant,
            ControlRequest::GetContext(ytm_control_api::ContextInput {
                preview_limit: Some(0),
            }),
        )
        .await
    else {
        panic!("expected a context");
    };
    let kinds: Vec<_> = context
        .listener_actions
        .iter()
        .map(|action| action.action)
        .collect();
    // Newest first.
    assert_eq!(
        kinds,
        [
            ytm_control_api::ListenerActionKind::UndoAddToPlaylist,
            ytm_control_api::ListenerActionKind::AddToPlaylist,
        ]
    );

    // The token was used: a second undo sends nothing.
    let again = harness.ui_undo("song", &token, EPOCH).await;
    assert_eq!(again.outcome, MutationOutcome::Rejected);
    assert_eq!(again.error.unwrap().code, QueryErrorCode::StaleContext);
    assert_eq!(harness.catalog.0.lock().await.edits.len(), 1);
}

/// A token is refused once it expired or the account session changed; nothing is read or sent.
#[tokio::test]
async fn ui_undo_is_refused_after_expiry_or_an_account_change() {
    let harness = Harness::new().await;
    harness.ui_playlist(&["first"]).await;
    let expiring = harness.ui_add("song").await.undo_token.unwrap();
    harness
        .clock
        .advance(ytm_core::playlist_undo::UNDO_LIFETIME_MS);
    let expired = harness.ui_undo("song", &expiring, EPOCH).await;
    assert_eq!(expired.outcome, MutationOutcome::Rejected);
    assert_eq!(expired.error.unwrap().code, QueryErrorCode::StaleContext);

    harness.ui_playlist(&["first"]).await;
    let token = harness.ui_add("song").await.undo_token.unwrap();
    let reads = harness.catalog.0.lock().await.reads;
    harness
        .engine
        .on_session_material(SessionMaterialPayload {
            account_epoch: AccountEpoch::new("epoch-after-switch").unwrap(),
            credential_revision: CredentialRevision::new("credential-2").unwrap(),
            cookie_pairs: Default::default(),
            auth_user_index: None,
            channel_label: None,
        })
        .await
        .unwrap();
    let stale = harness.ui_undo("song", &token, "epoch-after-switch").await;
    assert_eq!(stale.outcome, MutationOutcome::Rejected);
    assert!(!stale.already_removed);
    let remote = harness.catalog.0.lock().await;
    assert!(remote.edits.is_empty());
    assert_eq!(remote.reads, reads);
}

/// The entry already gone is reported as such; a fresh read in another order stays unknown.
#[tokio::test]
async fn ui_undo_reports_already_removed_and_unknown_order() {
    let harness = Harness::new().await;
    harness.ui_playlist(&["first", "second"]).await;
    let token = harness.ui_add("song").await.undo_token.unwrap();
    harness
        .catalog
        .0
        .lock()
        .await
        .playlist
        .as_mut()
        .unwrap()
        .entries
        .items
        .pop();
    let gone = harness.ui_undo("song", &token, EPOCH).await;
    assert_eq!(gone.outcome, MutationOutcome::Rejected);
    assert!(gone.already_removed);
    assert!(harness.catalog.0.lock().await.edits.is_empty());

    let token = harness.ui_add("song").await.undo_token.unwrap();
    harness.catalog.0.lock().await.reorder_on_removal = true;
    let unknown = harness.ui_undo("song", &token, EPOCH).await;
    assert_eq!(
        unknown.outcome,
        MutationOutcome::Indeterminate,
        "{unknown:?}"
    );
    assert_eq!(harness.catalog.0.lock().await.edits.len(), 1);
    // Unknown is never retried, and the token is used up.
    let again = harness.ui_undo("song", &token, EPOCH).await;
    assert_eq!(again.outcome, MutationOutcome::Rejected);
    assert_eq!(harness.catalog.0.lock().await.edits.len(), 1);
}

/// A duplicate add keeps its refusal and gives no token that could remove the earlier copy; an
/// add whose reply names no entry is confirmed without one.
#[tokio::test]
async fn ui_add_without_a_named_new_entry_cannot_be_undone() {
    let harness = Harness::new().await;
    harness.ui_playlist(&["song", "second"]).await;
    let duplicate = harness.ui_add("song").await;
    assert_eq!(duplicate.outcome, MutationOutcome::Rejected);
    assert_eq!(
        duplicate.error.unwrap().code,
        QueryErrorCode::UnavailableTrack
    );
    assert!(duplicate.undo_token.is_none());

    harness.catalog.0.lock().await.unnamed_ui_add = true;
    let unnamed = harness.ui_add("third").await;
    assert_eq!(unnamed.outcome, MutationOutcome::Confirmed);
    assert!(unnamed.undo_token.is_none());
    assert_eq!(
        harness.ui_entries().await,
        pairs(&[
            ("pre-0", "song"),
            ("pre-1", "second"),
            ("ui-occurrence-2", "third"),
        ])
    );
    assert!(harness.catalog.0.lock().await.edits.is_empty());
}
