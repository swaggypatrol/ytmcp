use std::collections::HashSet;

use ytm_control_api::{
    ControlError, ErrorCode, GrantAction, ItemPhase, ItemResult, OperationPhase, OperationResource,
    PageInput, Playlist, PlaylistEntry, PlaylistPrivacy, RetryDisposition, VerificationLevel,
    WriteAction,
};
use ytm_protocol::{AccountScope, PlaylistId, VideoId};

use super::catalog::{
    AddedOccurrence, CatalogReadContext, ControlCatalog, PlaylistWrite, PlaylistWriteAck,
    PlaylistWriteError,
};
use super::catalog_locks::CatalogTarget;
use super::journal::OperationKey;
use super::write::{MusicOperation, Transition, permission};
use super::{invalid, map_query_error, stale, unavailable};

impl MusicOperation {
    pub(super) async fn playlist(&mut self) -> Result<Transition, ControlError> {
        let initial_epoch = self.state.snapshot.account_epoch.as_ref();
        if self.input.preconditions.expected_account_epoch.as_ref() != initial_epoch
            || initial_epoch.is_none()
        {
            return Err(stale("a matching authenticated account epoch is required"));
        }
        if !self.state.snapshot.auth_state.player_catalog_matched {
            return Err(permission("the player and catalog accounts do not match"));
        }
        let scope = self
            .grant
            .scope
            .clone()
            .ok_or_else(|| permission("a matching account grant is required"))?;
        let catalog = self
            .service
            .catalog
            .clone()
            .ok_or_else(|| unavailable("the music catalog is unavailable"))?;
        let (write, playlist, derived) = match &self.input.action {
            WriteAction::CreatePrivatePlaylist { title, description } => {
                let title = title.trim();
                if title.is_empty()
                    || title.chars().count() > 150
                    || title.chars().any(char::is_control)
                    || description
                        .as_ref()
                        .is_some_and(|text| text.chars().count() > 5000)
                {
                    return Err(invalid(
                        "invalid title or description for the private playlist",
                    ));
                }
                (
                    PlaylistWrite::CreatePrivate {
                        title: title.into(),
                        description: description.clone(),
                    },
                    None,
                    None,
                )
            }
            WriteAction::AddPlaylistEntries {
                playlist_id,
                created_by_operation_id,
                video_ids,
            } => {
                if !(1..=ytm_control_api::MAX_WRITE_ITEMS).contains(&video_ids.len()) {
                    return Err(invalid("a playlist append must contain 1–50 tracks"));
                }
                if video_ids.iter().collect::<HashSet<_>>().len() != video_ids.len() {
                    return Err(invalid(
                        "one ordered append cannot contain repeated video IDs",
                    ));
                }
                (
                    PlaylistWrite::Append {
                        playlist: playlist_id.clone(),
                        videos: video_ids.clone(),
                    },
                    Some(playlist_id.clone()),
                    created_by_operation_id.clone(),
                )
            }
            _ => return Err(invalid("expected a playlist operation")),
        };

        let lock = self
            .service
            .catalog_locks
            .for_target(scope.clone(), CatalogTarget::Playlist(playlist.clone()))?;
        let _guard = lock.lock().await;
        self.check_playlist_grant(playlist.as_ref(), derived.as_deref())
            .await?;
        let before = if let Some(id) = &playlist {
            let ctx = self.fresh_context().await?;
            let loaded = load_complete(catalog.as_ref(), ctx, id).await?;
            if loaded.0.editable != Some(true) {
                return Err(ControlError::new(
                    ErrorCode::NotEditable,
                    RetryDisposition::NotRetryable,
                    "the provider did not confirm that this playlist can be edited",
                ));
            }
            if let Some(fingerprint) = self.input.preconditions.expected_entity_revision.as_ref()
                && loaded.0.snapshot_fingerprint.as_ref() != Some(fingerprint)
            {
                return Err(stale("the playlist changed since it was read"));
            }
            // The provider refuses a whole append that contains a track already present, without
            // a recognizable reply; refuse it here, before anything is sent.
            if let PlaylistWrite::Append { videos, .. } = &write
                && let Some(present) = videos
                    .iter()
                    .find(|video| loaded.1.iter().any(|entry| &entry.track.video_id == *video))
            {
                return Err(ControlError::new(
                    ErrorCode::Conflict,
                    RetryDisposition::NewPlanRequired,
                    format!("{present} is already in this playlist; leave it out of the append"),
                ));
            }
            Some(loaded.1)
        } else {
            None
        };

        let mut submitted = self.current.clone();
        submitted.revision += 1;
        submitted.phase = OperationPhase::Submitted;
        submitted.resource = playlist
            .clone()
            .map(|id| OperationResource::Playlist { id });
        if let PlaylistWrite::Append { videos, .. } = &write {
            submitted.item_results = (0..videos.len())
                .map(|index| ItemResult {
                    input_index: index as u32,
                    phase: ItemPhase::Submitted,
                    resource_entry_id: None,
                    error: None,
                })
                .collect();
        }
        self.apply(submitted).await?;
        if self.cancel.is_cancelled() {
            self.mark_items(ItemPhase::Cancelled, None).await?;
            return Ok(Transition::Cancelled);
        }
        let context = match self.fresh_context().await {
            Ok(context) => context,
            Err(error) => return self.reject_items(error).await,
        };
        if let Err(error) = self
            .check_playlist_grant(playlist.as_ref(), derived.as_deref())
            .await
        {
            return self.reject_items(error).await;
        }

        if self.cancel.is_cancelled() {
            self.mark_items(ItemPhase::Cancelled, None).await?;
            return Ok(Transition::Cancelled);
        }
        let ack = match catalog.apply_playlist(context, write.clone()).await {
            Ok(ack) => ack,
            Err(PlaylistWriteError::Rejected(error)) => return self.reject_items(error).await,
            Err(PlaylistWriteError::Unknown(error)) => {
                // A lost reply to a creation carries no playlist ID to invalidate.
                if let Some(id) = &playlist {
                    let _ = self.refresh_playlist(catalog.as_ref(), &scope, id).await;
                }
                return Ok(Transition::Unknown(error));
            }
        };
        let id = match &ack {
            PlaylistWriteAck::Created(id) => id.clone(),
            PlaylistWriteAck::Appended(_) => playlist
                .clone()
                .ok_or_else(|| unavailable("playlist provider returned an unexpected result"))?,
        };
        let refreshed = self.refresh_playlist(catalog.as_ref(), &scope, &id).await;
        let mut acknowledged = self.current.clone();
        acknowledged.revision += 1;
        acknowledged.phase = OperationPhase::Acknowledged;
        acknowledged.resource = Some(OperationResource::Playlist { id: id.clone() });
        if let (PlaylistWriteAck::Appended(added), PlaylistWrite::Append { videos, .. }) =
            (&ack, &write)
        {
            for (item, video) in acknowledged.item_results.iter_mut().zip(videos) {
                if let Some(entry) = added.iter().find(|entry| &entry.video_id == video) {
                    item.phase = ItemPhase::Acknowledged;
                    item.resource_entry_id = Some(entry.entry_id.clone());
                } else {
                    item.phase = ItemPhase::Unknown;
                }
            }
        }
        self.apply(acknowledged).await?;
        refreshed?;

        let verification_context = self.fresh_context().await?;
        let (snapshot, after) = load_complete(catalog.as_ref(), verification_context, &id).await?;
        match (write, ack, before) {
            (
                PlaylistWrite::CreatePrivate { title, description },
                PlaylistWriteAck::Created(_),
                None,
            ) if snapshot.playlist_id == id
                && snapshot.privacy == Some(PlaylistPrivacy::Private)
                && snapshot.editable == Some(true)
                && snapshot.title == title
                && snapshot.description.as_deref().unwrap_or_default()
                    == description.as_deref().unwrap_or_default() =>
            {
                Ok(Transition::Verified(VerificationLevel::ReadBack))
            }
            (
                PlaylistWrite::Append { videos, .. },
                PlaylistWriteAck::Appended(added),
                Some(before),
            ) => {
                let observed = verify_append(&before, &after, &videos, &added);
                let mut result = self.current.clone();
                result.revision += 1;
                for (item, entry) in result.item_results.iter_mut().zip(&observed) {
                    if let Some(entry) = entry {
                        item.phase = ItemPhase::Verified;
                        item.resource_entry_id = entry.entry_id.clone();
                    } else {
                        item.phase = ItemPhase::Unknown;
                        item.error = Some(ErrorCode::ResultUnknown);
                    }
                }
                self.apply(result).await?;
                let verified = observed.iter().flatten().count();
                if verified == videos.len() {
                    Ok(Transition::Verified(VerificationLevel::ReadBack))
                } else if verified > 0 {
                    Ok(Transition::Partial {
                        settled: false,
                        error: unavailable("the ordered playlist append is only partially proven"),
                    })
                } else {
                    Ok(Transition::Unknown(unavailable(
                        "fresh playlist readback did not prove the requested append",
                    )))
                }
            }
            _ => Ok(Transition::Unknown(unavailable(
                "fresh playlist readback did not confirm all requested details",
            ))),
        }
    }

    /// Retires cached pages and publishes refresh events for a playlist that a sent write may
    /// have changed. Cache failures are logged; the error reports an unpublished refresh event.
    pub(super) async fn refresh_playlist(
        &self,
        catalog: &dyn ControlCatalog,
        scope: &AccountScope,
        id: &PlaylistId,
    ) -> Result<(), ControlError> {
        if let Err(error) = catalog
            .invalidate_playlist_cache(scope.clone(), id.clone(), true)
            .await
        {
            tracing::warn!(code = ?error.code, "playlist changed but cached pages could not be invalidated");
        }
        let Some(epoch) = self.state.snapshot.account_epoch.clone() else {
            return Ok(());
        };
        let mut published = Ok(());
        for entity in ["playlistTracks", "libraryPlaylists"] {
            if self
                .service
                .core
                .invalidate_control_resource(epoch.clone(), entity.into(), id.as_str().into())
                .await
                .is_err()
            {
                published = Err(unavailable(
                    "the playlist changed before a refresh event could be published",
                ));
            }
        }
        published
    }

    pub(super) async fn fresh_context(&self) -> Result<CatalogReadContext, ControlError> {
        let state = self.service.authorized_state(&self.grant, false).await?;
        if state.snapshot.account_epoch != self.state.snapshot.account_epoch
            || !state.snapshot.auth_state.player_catalog_matched
        {
            return Err(stale("the signed-in player account changed"));
        }
        self.service
            .catalog_context(&state)
            .map(|(_, context)| context)
    }

    pub(super) async fn check_playlist_grant(
        &self,
        playlist_id: Option<&PlaylistId>,
        creation: Option<&str>,
    ) -> Result<(), ControlError> {
        match (playlist_id, creation) {
            (None, _) => {
                self.authority
                    .authorize(GrantAction::CreatePrivatePlaylist)
                    .await?;
            }
            (Some(id), Some(operation)) => {
                self.authority
                    .authorize(GrantAction::CreatePrivatePlaylist)
                    .await?;
                let prior = self
                    .store
                    .get(OperationKey {
                        principal: self.key.principal.clone(),
                        account_scope: self.key.account_scope.clone(),
                        operation_id: operation.to_owned(),
                    })
                    .await?;
                let input = self
                    .store
                    .load_request(OperationKey {
                        principal: self.key.principal.clone(),
                        account_scope: self.key.account_scope.clone(),
                        operation_id: operation.to_owned(),
                    })
                    .await?;
                if !input.is_some_and(|input| {
                    matches!(input.action, WriteAction::CreatePrivatePlaylist { .. })
                }) || !prior.is_some_and(|prior| {
                    prior.phase == OperationPhase::Verified
                        && prior.resource == Some(OperationResource::Playlist { id: id.clone() })
                }) {
                    return Err(permission(
                        "the client did not create this private playlist under the specified operation",
                    ));
                }
            }
            (Some(id), None) if self.created_playlist.as_ref() == Some(id) => {
                self.authority
                    .authorize(GrantAction::CreatePrivatePlaylist)
                    .await?;
            }
            (Some(id), None) => {
                let grant = self
                    .authority
                    .authorize(GrantAction::AddPlaylistEntries)
                    .await?;
                if !grant.editable_playlists.contains(id) {
                    return Err(permission("this playlist is outside the grant"));
                }
            }
        }
        Ok(())
    }
}

pub(super) async fn load_complete(
    catalog: &dyn ControlCatalog,
    context: CatalogReadContext,
    id: &PlaylistId,
) -> Result<(Playlist, Vec<PlaylistEntry>), ControlError> {
    let mut page = catalog
        .fresh_playlist(
            context.clone(),
            id.clone(),
            PageInput {
                cursor: None,
                limit: Some(100),
            },
        )
        .await
        .map_err(map_query_error)?;
    let mut metadata = page.clone();
    metadata.entries.items.clear();
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    for _ in 0..20 {
        if page.playlist_id != *id || page.entries.freshness != ytm_protocol::Freshness::Fresh {
            return Err(unavailable(
                "the provider did not return a fresh playlist page",
            ));
        }
        entries.append(&mut page.entries.items);
        let Some(cursor) = page.entries.next_cursor.take() else {
            if page.entries.complete {
                return Ok((metadata, entries));
            }
            break;
        };
        if !seen.insert(cursor.clone()) {
            break;
        }
        page = catalog
            .fresh_playlist(
                context.clone(),
                id.clone(),
                PageInput {
                    cursor: Some(cursor),
                    limit: Some(100),
                },
            )
            .await
            .map_err(map_query_error)?;
    }
    Err(unavailable(
        "the playlist cannot be read completely within the verification budget",
    ))
}

fn verify_append<'a>(
    before: &[PlaylistEntry],
    after: &'a [PlaylistEntry],
    requested: &[VideoId],
    ack: &[AddedOccurrence],
) -> Vec<Option<&'a PlaylistEntry>> {
    let stable = after.get(..before.len()).is_some_and(|prefix| {
        before.iter().zip(prefix).all(|(old, current)| {
            old.entry_id.is_some()
                && old.entry_id == current.entry_id
                && old.track.video_id == current.track.video_id
        })
    });
    if !stable {
        return vec![None; requested.len()];
    }
    let suffix = &after[before.len()..];
    if suffix.len() > requested.len()
        || !suffix
            .iter()
            .map(|entry| &entry.track.video_id)
            .eq(requested
                .iter()
                .filter(|video| ack.iter().any(|entry| &entry.video_id == *video)))
    {
        return vec![None; requested.len()];
    }
    requested
        .iter()
        .map(|video| {
            let ack = ack.iter().find(|entry| &entry.video_id == video)?;
            suffix.iter().find(|entry| {
                entry.entry_id.as_deref() == Some(ack.entry_id.as_str())
                    && &entry.track.video_id == video
            })
        })
        .collect()
}
