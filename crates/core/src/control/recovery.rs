use std::collections::HashSet;
use std::sync::Arc;

use tokio::sync::broadcast::error::RecvError;
use tokio_util::sync::CancellationToken;
use ytm_control_api::{
    ControlError, ErrorCode, GrantAction, ItemPhase, ItemResult, OperationPhase, OperationReceipt,
    OperationResource, PlaylistEntry, PlaylistPrivacy, Rating, ResumeAction, RetryDisposition,
    TrackTarget, Verification, VerificationLevel, WriteAction,
};
use ytm_protocol::{AccountEpoch, StateDelta, VideoId};

use super::catalog::CatalogReadContext;
use super::journal::OperationKey;
use super::membership::read_membership;
use super::playlist::load_complete;
use super::write::{
    DispatchAuthority, DispatchGrant, MusicOperation, Transition, permission, required_scope,
};
use super::{ControlService, ReadGrant, invalid, map_query_error, stale, unavailable};
use crate::BoxFuture;
use crate::engine::ControlRead;

/// Most interrupted operations the player settles on its own for one bound account epoch.
const AUTO_SETTLE_LIMIT: usize = 20;
/// Unsettled journal rows scanned for them; rows without an evidence path are passed over.
const AUTO_SETTLE_SCAN: usize = 256;

impl ControlService {
    /// Settles, without a client request, the stopped and unsettled operations of the bound
    /// account whose outcome fresh evidence can establish (private playlist creation, appends,
    /// ratings, artist subscriptions, library membership), exactly as an explicit reconcile
    /// would. Nothing is resent; operations without an evidence path, or whose evidence does
    /// not establish the outcome, stay as they are. Returns how many were verified.
    pub async fn settle_interrupted_operations(self: &Arc<Self>) -> Result<usize, ControlError> {
        let store = self
            .operations
            .clone()
            .ok_or_else(|| unavailable("the operation journal is unavailable"))?;
        let state = self
            .core
            .read_control(false)
            .await
            .map_err(|_| unavailable("player is unavailable"))?;
        let Some(epoch) = bound_epoch(&state) else {
            return Ok(0);
        };
        let scope = state.snapshot.auth_state.account_scope.clone();
        let mut attempted = 0;
        let mut settled = 0;
        for entry in store.unsettled(scope.clone(), AUTO_SETTLE_SCAN).await? {
            if attempted == AUTO_SETTLE_LIMIT {
                break;
            }
            if !awaits_reconcile(&entry.receipt) {
                continue;
            }
            let key = OperationKey {
                principal: entry.principal,
                account_scope: scope.clone(),
                operation_id: entry.receipt.operation_id.clone(),
            };
            let Ok(Some(input)) = store.load_request(key.clone()).await else {
                continue;
            };
            if !has_evidence_path(&input.action) {
                continue;
            }
            attempted += 1;
            let grant = ReadGrant::new(key.principal.clone(), scope.clone());
            let state = self.authorized_state(&grant, false).await?;
            if bound_epoch(&state).as_ref() != Some(&epoch) {
                break;
            }
            let action = input.action.clone();
            let mut worker = MusicOperation {
                service: self.clone(),
                store: store.clone(),
                key: key.clone(),
                grant,
                authority: Arc::new(NoClient),
                scope_action: required_scope(&action)?,
                input,
                state,
                current: entry.receipt.clone(),
                cancel: CancellationToken::new(),
                created_playlist: None,
                _slot: None,
            };
            match worker
                .settle_from_evidence(key, &action, entry.receipt, false)
                .await
            {
                Ok(Transition::Verified(_)) => settled += 1,
                Ok(_) => {}
                Err(error) => {
                    tracing::debug!(code = ?error.code, "an interrupted music operation stays unsettled");
                }
            }
        }
        Ok(settled)
    }

    /// Runs [`Self::settle_interrupted_operations`] once for each account epoch the player
    /// binds, starting with the current one, until the engine stops.
    pub async fn settle_on_account_bind(self: Arc<Self>) {
        let Ok(mut state) = self.core.read_control(true).await else {
            return;
        };
        let Some(mut events) = state.events.take() else {
            return;
        };
        let mut settled_epoch = None;
        loop {
            if let Some(epoch) = bound_epoch(&state)
                && settled_epoch.as_ref() != Some(&epoch)
            {
                settled_epoch = Some(epoch);
                match self.settle_interrupted_operations().await {
                    Ok(0) => {}
                    Ok(count) => tracing::info!(count, "settled interrupted music operations"),
                    Err(error) => {
                        tracing::warn!(code = ?error.code, "could not settle interrupted music operations");
                    }
                }
            }
            loop {
                match events.recv().await {
                    Ok(StateDelta::AuthChanged { .. }) | Err(RecvError::Lagged(_)) => break,
                    Ok(_) => {}
                    Err(RecvError::Closed) => return,
                }
            }
            state = match self.core.read_control(false).await {
                Ok(state) => state,
                Err(_) => return,
            };
        }
    }
}

/// The account epoch once the player's session is bound to a matched catalog account.
fn bound_epoch(state: &ControlRead) -> Option<AccountEpoch> {
    let auth = &state.snapshot.auth_state;
    (auth.account_scope.is_some() && auth.player_catalog_matched)
        .then(|| state.snapshot.account_epoch.clone())
        .flatten()
}

/// A stopped, unsettled result that only fresh evidence can settle.
fn awaits_reconcile(receipt: &OperationReceipt) -> bool {
    !receipt.settled
        && receipt.scheduling_stopped
        && matches!(
            receipt.phase,
            OperationPhase::Unknown | OperationPhase::Partial
        )
}

fn has_evidence_path(action: &WriteAction) -> bool {
    matches!(
        action,
        WriteAction::CreatePrivatePlaylist { .. }
            | WriteAction::AddPlaylistEntries { .. }
            | WriteAction::Rate { .. }
            | WriteAction::SetArtistSubscription { .. }
            | WriteAction::SetLibraryMembership { .. }
    )
}

/// The authority of the player's own settlement: it never dispatches, so it grants nothing.
struct NoClient;

impl DispatchAuthority for NoClient {
    fn authorize(
        &self,
        _action: GrantAction,
    ) -> BoxFuture<'_, Result<DispatchGrant, ControlError>> {
        Box::pin(async { Err(permission("automatic settlement does not dispatch")) })
    }
}

impl MusicOperation {
    pub(super) async fn reconcile(&mut self) -> Result<Transition, ControlError> {
        let WriteAction::ResumeOperation {
            target_operation_id,
            expected_operation_revision,
            action,
        } = self.input.action.clone()
        else {
            return Err(invalid("expected an operation recovery request"));
        };
        if target_operation_id == self.key.operation_id {
            return Err(invalid(
                "operation recovery must have a different operationId",
            ));
        }
        self.current.resource = Some(OperationResource::Operation {
            id: target_operation_id.clone(),
        });
        if self.input.preconditions.expected_account_epoch != self.state.snapshot.account_epoch {
            return Err(stale("the account epoch changed before recovery"));
        }
        let target_key = OperationKey {
            operation_id: target_operation_id,
            ..self.key.clone()
        };
        let original = self
            .store
            .load_request(target_key.clone())
            .await?
            .ok_or_else(|| invalid("no target operation in this account and client"))?;
        let target = self
            .store
            .get(target_key.clone())
            .await?
            .ok_or_else(|| invalid("no target operation in this account and client"))?;
        if target.revision != expected_operation_revision {
            return Err(ControlError::new(
                ErrorCode::Conflict,
                RetryDisposition::QueryOperation,
                "the target operation revision changed",
            ));
        }
        if self.cancel.is_cancelled() {
            return Ok(Transition::Cancelled);
        }
        self.authorize_recovery(&original.action).await?;
        if action == ResumeAction::ResumeUnsent {
            return Err(ControlError::new(
                ErrorCode::CapabilityUnavailable,
                RetryDisposition::NewPlanRequired,
                "unsent work requires a new plan for this operation",
            ));
        }
        if !awaits_reconcile(&target) {
            return Err(ControlError::new(
                ErrorCode::Conflict,
                RetryDisposition::QueryOperation,
                "the target operation has no stopped, unsettled result to reconcile",
            ));
        }
        self.settle_from_evidence(target_key, &original.action, target, true)
            .await
    }

    /// Reads fresh evidence for a stopped, unsettled `target` and records it as verified by
    /// read-back when the evidence establishes the outcome; otherwise the target is unchanged.
    /// Nothing is sent. `client` re-checks the requesting client's grant around the read; the
    /// player's own settlement after a restart has no client to check.
    async fn settle_from_evidence(
        &mut self,
        target_key: OperationKey,
        action: &WriteAction,
        mut target: OperationReceipt,
        client: bool,
    ) -> Result<Transition, ControlError> {
        let expected_operation_revision = target.revision;
        let _membership_guard = if let WriteAction::SetLibraryMembership { target, .. } = action {
            let Some(guard) = self.lock_membership(target).await? else {
                return Ok(Transition::Cancelled);
            };
            Some(guard)
        } else {
            None
        };
        let mut context = self.fresh_context().await?;
        context.cancel = self.cancel.clone();
        let observed = self.read_recovery_evidence(action, &target, context).await;
        if self.cancel.is_cancelled() {
            return Ok(Transition::Cancelled);
        }
        if !observed? {
            return Err(ControlError::new(
                ErrorCode::ResultUnknown,
                RetryDisposition::ReconcileFirst,
                "fresh evidence did not establish the target operation outcome",
            ));
        }
        if client {
            self.authorize_recovery(action).await?;
        }
        self.fresh_context().await?;
        if self.cancel.is_cancelled() {
            return Ok(Transition::Cancelled);
        }

        let now = self.service.core.clock().wall_now_ms();
        target.revision += 1;
        target.phase = OperationPhase::Verified;
        target.verification = Some(Verification {
            level: VerificationLevel::ReadBack,
            observed_at_ms: now,
        });
        target.settled = true;
        target.retry_disposition = RetryDisposition::NotRetryable;
        target.error = None;
        match action {
            WriteAction::SetArtistSubscription {
                artist_id,
                subscribed,
            } => {
                target.resource = Some(OperationResource::Artist {
                    id: artist_id.clone(),
                    subscribed: Some(*subscribed),
                });
            }
            WriteAction::SetLibraryMembership { saved, .. } => {
                if let Some(OperationResource::LibraryMembership {
                    saved: observed, ..
                }) = &mut target.resource
                {
                    *observed = Some(*saved);
                }
            }
            _ => {}
        }
        for item in &mut target.item_results {
            item.phase = ItemPhase::Verified;
            item.error = None;
        }
        let settled_resource = target.resource.clone();
        self.store
            .update(target_key.clone(), expected_operation_revision, target, now)
            .await?;
        self.service
            .core
            .control_operation_changed(self.state.snapshot.account_epoch.clone(), &target_key)
            .await;
        if let Some(catalog) = self.service.catalog.as_deref() {
            match (action, &settled_resource) {
                (
                    WriteAction::CreatePrivatePlaylist { .. }
                    | WriteAction::AddPlaylistEntries { .. },
                    Some(OperationResource::Playlist { id }),
                ) => {
                    if let Some(scope) = self.grant.scope.clone() {
                        let _ = self.refresh_playlist(catalog, &scope, id).await;
                    }
                }
                (WriteAction::Rate { .. }, Some(OperationResource::Track { id })) => {
                    self.refresh_rating(catalog, id).await;
                }
                (WriteAction::SetArtistSubscription { artist_id, .. }, _) => {
                    self.refresh_artist(artist_id).await;
                }
                (WriteAction::SetLibraryMembership { target, .. }, _) => {
                    self.refresh_membership(catalog, target).await;
                }
                _ => {}
            }
        }
        Ok(Transition::Verified(VerificationLevel::ReadBack))
    }

    async fn authorize_recovery(&self, action: &WriteAction) -> Result<(), ControlError> {
        let grant = self.authority.authorize(self.scope_action).await?;
        if let WriteAction::AddPlaylistEntries {
            playlist_id,
            created_by_operation_id,
            ..
        } = action
        {
            self.check_playlist_grant(Some(playlist_id), created_by_operation_id.as_deref())
                .await?;
        }
        if grant.deadline_wall_ms <= self.service.core.clock().wall_now_ms() {
            return Err(permission("the grant expired before recovery"));
        }
        Ok(())
    }

    async fn read_recovery_evidence(
        &self,
        action: &WriteAction,
        target: &OperationReceipt,
        context: CatalogReadContext,
    ) -> Result<bool, ControlError> {
        let catalog = self
            .service
            .catalog
            .as_deref()
            .ok_or_else(|| unavailable("the music catalog is unavailable"))?;
        match (action, &target.resource) {
            (
                WriteAction::CreatePrivatePlaylist { title, description },
                Some(OperationResource::Playlist { id }),
            ) => {
                let (playlist, entries) = load_complete(catalog, context, id).await?;
                Ok(playlist.privacy == Some(PlaylistPrivacy::Private)
                    && playlist.editable == Some(true)
                    && playlist.title == title.trim()
                    && playlist.description.as_deref().unwrap_or_default()
                        == description.as_deref().unwrap_or_default()
                    && entries.is_empty())
            }
            (
                WriteAction::AddPlaylistEntries {
                    playlist_id,
                    video_ids,
                    ..
                },
                Some(OperationResource::Playlist { id }),
            ) if playlist_id == id => {
                let Some(expected) = append_evidence(&target.item_results, video_ids) else {
                    return Ok(false);
                };
                let (_, entries) = load_complete(catalog, context, id).await?;
                Ok(contains_ordered_occurrences(&entries, &expected))
            }
            (WriteAction::Rate { target, rating }, Some(OperationResource::Track { id }))
                if *rating != Rating::Unknown
                    && !matches!(target, TrackTarget::Video { video_id } if video_id != id) =>
            {
                let evidence = catalog
                    .fresh_rating(context, id.clone())
                    .await
                    .map_err(map_query_error)?;
                Ok(evidence.rating == *rating)
            }
            (
                WriteAction::SetArtistSubscription {
                    artist_id,
                    subscribed,
                },
                Some(OperationResource::Artist { id, .. }),
            ) if artist_id == id => {
                let evidence = catalog
                    .artist_subscription(context, id.clone())
                    .await
                    .map_err(map_query_error)?;
                Ok(evidence.value == Some(*subscribed))
            }
            (
                WriteAction::SetLibraryMembership { target, saved },
                Some(OperationResource::LibraryMembership {
                    target: journaled,
                    album_playlist_id,
                    ..
                }),
            ) if target == journaled => {
                let (evidence, actual_album) = read_membership(catalog, context, journaled)
                    .await
                    .map_err(map_query_error)?;
                Ok(evidence.value == Some(*saved) && &actual_album == album_playlist_id)
            }
            _ => Ok(false),
        }
    }
}

fn append_evidence<'a>(
    items: &'a [ItemResult],
    videos: &'a [VideoId],
) -> Option<Vec<(&'a str, &'a VideoId)>> {
    if items.is_empty() || items.len() != videos.len() {
        return None;
    }
    let mut seen = HashSet::new();
    items
        .iter()
        .zip(videos)
        .enumerate()
        .map(|(index, (item, video))| {
            let id = item.resource_entry_id.as_deref()?;
            (!id.is_empty()
                && item.input_index as usize == index
                && matches!(
                    item.phase,
                    ItemPhase::Acknowledged | ItemPhase::Unknown | ItemPhase::Verified
                )
                && seen.insert(id))
            .then_some((id, video))
        })
        .collect()
}

fn contains_ordered_occurrences(entries: &[PlaylistEntry], expected: &[(&str, &VideoId)]) -> bool {
    entries.windows(expected.len()).any(|window| {
        window.iter().zip(expected).all(|(entry, (id, video))| {
            entry.entry_id.as_deref() == Some(*id) && &entry.track.video_id == *video
        })
    })
}
