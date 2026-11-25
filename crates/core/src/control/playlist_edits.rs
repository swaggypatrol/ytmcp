use std::collections::HashSet;

use ytm_control_api::{
    ControlError, ErrorCode, ItemPhase, ItemResult, OperationPhase, OperationResource, PageInput,
    Playlist, PlaylistEntry, RemovedItem, RetryDisposition, VerificationLevel, WriteAction,
};
use ytm_protocol::{Freshness, PlaylistId, validate_id_str};

use super::catalog::{
    CatalogReadContext, CatalogWriteError, ControlCatalog, PlaylistEdit, PlaylistOccurrenceIdentity,
};
use super::catalog_locks::CatalogTarget;
use super::playlist::load_complete;
use super::write::{MusicOperation, Transition, permission};
use super::{invalid, map_query_error, stale, unavailable};

impl MusicOperation {
    pub(super) async fn edit_playlist(&mut self) -> Result<Transition, ControlError> {
        let (id, occurrence_edit) = match &self.input.action {
            WriteAction::UpdatePlaylist { playlist_id, .. }
            | WriteAction::SetPlaylistPrivacy { playlist_id, .. }
            | WriteAction::DeletePlaylist { playlist_id } => (playlist_id.clone(), false),
            WriteAction::RemovePlaylistEntries { playlist_id, .. }
            | WriteAction::MovePlaylistEntry { playlist_id, .. } => (playlist_id.clone(), true),
            _ => return Err(invalid("expected a playlist edit")),
        };
        let epoch = self
            .state
            .snapshot
            .account_epoch
            .clone()
            .ok_or_else(|| stale("a matching authenticated account epoch is required"))?;
        if self.input.preconditions.expected_account_epoch.as_ref() != Some(&epoch) {
            return Err(stale("a matching authenticated account epoch is required"));
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
        let lock = self
            .service
            .catalog_locks
            .for_target(scope.clone(), CatalogTarget::Playlist(Some(id.clone())))?;
        let _guard = tokio::select! {
            _ = self.cancel.cancelled() => return Ok(Transition::Cancelled),
            guard = lock.lock() => guard,
        };
        self.authorize_playlist_edit(&id).await?;
        let (metadata, before) = fresh_edit_snapshot(
            catalog.as_ref(),
            self.fresh_context().await?,
            &id,
            occurrence_edit,
        )
        .await?;
        if metadata.editable != Some(true) {
            return Err(ControlError::new(
                ErrorCode::NotEditable,
                RetryDisposition::NotRetryable,
                "the provider did not confirm that the playlist is editable",
            ));
        }
        if let Some(revision) = &self.input.preconditions.expected_entity_revision
            && metadata.snapshot_fingerprint.as_ref() != Some(revision)
        {
            return Err(stale("the playlist snapshot changed or cannot be compared"));
        }
        let prepared = PreparedEdit::new(&self.input.action, &before)?;

        let mut submitted = self.current.clone();
        submitted.revision += 1;
        submitted.phase = OperationPhase::Submitted;
        submitted.resource = Some(OperationResource::Playlist { id: id.clone() });
        submitted.item_results = prepared.item_results();
        submitted.removed = prepared.removal_ledger(&id);
        self.apply(submitted).await?;

        let eligibility = self.fresh_context().await;
        let mut context = match eligibility {
            Ok(context) => context,
            Err(error) => return self.reject_items(error).await,
        };
        if let Err(error) = self.authorize_playlist_edit(&id).await {
            return self.reject_items(error).await;
        }
        if self.cancel.is_cancelled() {
            self.mark_items(ItemPhase::Cancelled, None).await?;
            return Ok(Transition::Cancelled);
        }
        context.cancel = self.cancel.clone();
        match catalog
            .edit_playlist(context, id.clone(), prepared.write.clone())
            .await
        {
            Ok(()) => {
                let mut acknowledged = self.current.clone();
                acknowledged.revision += 1;
                acknowledged.phase = OperationPhase::Acknowledged;
                for item in &mut acknowledged.item_results {
                    item.phase = ItemPhase::Acknowledged;
                }
                self.apply(acknowledged).await?;
            }
            Err(CatalogWriteError::Rejected(error)) => return self.reject_items(error).await,
            Err(CatalogWriteError::Unknown(_)) => {}
        }

        if let Err(error) = catalog
            .invalidate_playlist_cache(scope, id.clone(), true)
            .await
        {
            tracing::warn!(code = ?error.code, "playlist edit cache invalidation failed");
        }
        for entity in ["playlistTracks", "libraryPlaylists"] {
            let _ = self
                .service
                .core
                .invalidate_control_resource(epoch.clone(), entity.into(), id.as_str().into())
                .await;
        }

        let after = match self.fresh_context().await {
            Ok(context) => {
                fresh_edit_snapshot(catalog.as_ref(), context, &id, occurrence_edit).await
            }
            Err(error) => Err(error),
        };
        let observed = after
            .ok()
            .map_or(EditReadback::Unknown, |(playlist, entries)| {
                prepared.observe(&playlist, &entries)
            });
        match observed {
            EditReadback::All => {
                self.mark_items(ItemPhase::Verified, None).await?;
                Ok(Transition::Verified(VerificationLevel::ReadBack))
            }
            EditReadback::Removed(removed) => {
                let mut result = self.current.clone();
                result.revision += 1;
                for item in &mut result.item_results {
                    if item
                        .resource_entry_id
                        .as_ref()
                        .is_some_and(|id| removed.contains(id))
                    {
                        item.phase = ItemPhase::Verified;
                    } else {
                        item.phase = ItemPhase::Unknown;
                        item.error = Some(ErrorCode::ResultUnknown);
                    }
                }
                self.apply(result).await?;
                Ok(Transition::Partial {
                    settled: false,
                    error: unavailable("fresh readback proved only some of the requested removals"),
                })
            }
            EditReadback::Unknown => {
                self.mark_items(ItemPhase::Unknown, Some(ErrorCode::ResultUnknown))
                    .await?;
                let reason = if prepared.write == PlaylistEdit::Delete {
                    "fresh YouTube Music data did not establish that the playlist was deleted"
                } else {
                    "fresh YouTube Music data did not confirm the playlist edit"
                };
                Ok(Transition::Unknown(unavailable(reason)))
            }
        }
    }

    async fn authorize_playlist_edit(&self, id: &PlaylistId) -> Result<(), ControlError> {
        let grant = self.authority.authorize(self.scope_action).await?;
        if !grant.editable_playlists.contains(id) {
            return Err(permission("this playlist is outside the grant"));
        }
        Ok(())
    }

    /// Rejects a write that was never dispatched and marks every item with the error code.
    pub(super) async fn reject_items(
        &mut self,
        error: ControlError,
    ) -> Result<Transition, ControlError> {
        self.mark_items(ItemPhase::Rejected, Some(error.code))
            .await?;
        Ok(Transition::Rejected(error))
    }

    /// Records one phase for every item of a multi-item write; no-op for item-less writes.
    pub(super) async fn mark_items(
        &mut self,
        phase: ItemPhase,
        error: Option<ErrorCode>,
    ) -> Result<(), ControlError> {
        if self.current.item_results.is_empty() {
            return Ok(());
        }
        let mut receipt = self.current.clone();
        receipt.revision += 1;
        for item in &mut receipt.item_results {
            item.phase = phase;
            item.error = error;
        }
        self.apply(receipt).await
    }
}

async fn fresh_edit_snapshot(
    catalog: &dyn ControlCatalog,
    context: CatalogReadContext,
    id: &PlaylistId,
    through_end: bool,
) -> Result<(Playlist, Vec<PlaylistEntry>), ControlError> {
    if through_end {
        return load_complete(catalog, context, id).await;
    }
    let mut playlist = catalog
        .fresh_playlist(
            context,
            id.clone(),
            PageInput {
                cursor: None,
                limit: Some(100),
            },
        )
        .await
        .map_err(map_query_error)?;
    if playlist.playlist_id != *id || playlist.entries.freshness != Freshness::Fresh {
        return Err(unavailable(
            "the provider did not return fresh data for the requested playlist",
        ));
    }
    let entries = std::mem::take(&mut playlist.entries.items);
    Ok((playlist, entries))
}

struct PreparedEdit {
    write: PlaylistEdit,
    order: Vec<PlaylistOccurrenceIdentity>,
}

enum EditReadback {
    All,
    Removed(HashSet<String>),
    Unknown,
}

impl PreparedEdit {
    fn new(action: &WriteAction, before: &[PlaylistEntry]) -> Result<Self, ControlError> {
        let mut order = Vec::new();
        let write = match action {
            WriteAction::UpdatePlaylist {
                title, description, ..
            } => {
                let title = title.as_deref().map(str::trim);
                if (title.is_none() && description.is_none())
                    || title.is_some_and(|text| {
                        text.is_empty()
                            || text.chars().count() > 150
                            || text.chars().any(char::is_control)
                            || text.contains(['<', '>'])
                    })
                    || description
                        .as_ref()
                        .is_some_and(|text| text.chars().count() > 5000)
                {
                    return Err(invalid("invalid playlist title or description patch"));
                }
                PlaylistEdit::Update {
                    title: title.map(str::to_owned),
                    description: description.clone(),
                }
            }
            WriteAction::SetPlaylistPrivacy { privacy, .. } => PlaylistEdit::SetPrivacy(*privacy),
            WriteAction::RemovePlaylistEntries { entry_ids, .. } => {
                if !(1..=ytm_control_api::MAX_WRITE_ITEMS).contains(&entry_ids.len())
                    || entry_ids.iter().collect::<HashSet<_>>().len() != entry_ids.len()
                    || entry_ids.iter().any(|id| validate_id_str(id).is_err())
                {
                    return Err(invalid(
                        "a removal must contain 1–50 distinct playlist entry IDs",
                    ));
                }
                order = exact_occurrence_order(before)?;
                let removals = entry_ids
                    .iter()
                    .map(|id| {
                        order
                            .iter()
                            .find(|entry| &entry.entry_id == id)
                            .cloned()
                            .ok_or_else(|| {
                                stale("a requested occurrence is absent from the fresh playlist")
                            })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                PlaylistEdit::Remove(removals)
            }
            WriteAction::MovePlaylistEntry {
                entry_id,
                before_entry_id,
                ..
            } => {
                if validate_id_str(entry_id).is_err()
                    || before_entry_id.as_ref().is_some_and(|anchor| {
                        anchor == entry_id || validate_id_str(anchor).is_err()
                    })
                {
                    return Err(invalid(
                        "a move requires distinct valid occurrence and anchor IDs",
                    ));
                }
                order = exact_occurrence_order(before)?;
                let old = order
                    .iter()
                    .position(|entry| &entry.entry_id == entry_id)
                    .ok_or_else(|| {
                        stale("the requested occurrence is absent from the fresh playlist")
                    })?;
                let target = order.remove(old);
                let insertion = match before_entry_id {
                    Some(anchor) => order
                        .iter()
                        .position(|entry| &entry.entry_id == anchor)
                        .ok_or_else(|| {
                            stale("the move anchor is absent from the fresh playlist")
                        })?,
                    None => order.len(),
                };
                order.insert(insertion, target);
                PlaylistEdit::Move {
                    entry_id: entry_id.clone(),
                    before_entry_id: before_entry_id.clone(),
                }
            }
            WriteAction::DeletePlaylist { .. } => PlaylistEdit::Delete,
            _ => return Err(invalid("expected a playlist edit")),
        };
        Ok(Self { write, order })
    }

    fn item_results(&self) -> Vec<ItemResult> {
        let ids = match &self.write {
            PlaylistEdit::Remove(entries) => {
                entries.iter().map(|entry| entry.entry_id.clone()).collect()
            }
            PlaylistEdit::Move { entry_id, .. } => vec![entry_id.clone()],
            _ => Vec::new(),
        };
        ids.into_iter()
            .enumerate()
            .map(|(index, id)| ItemResult {
                input_index: index as u32,
                phase: ItemPhase::Submitted,
                resource_entry_id: Some(id),
                error: None,
            })
            .collect()
    }

    /// The requested removals with their songs and positions in the playlist before the edit.
    fn removal_ledger(&self, playlist_id: &PlaylistId) -> Vec<RemovedItem> {
        let PlaylistEdit::Remove(removals) = &self.write else {
            return Vec::new();
        };
        removals
            .iter()
            .filter_map(|removal| {
                let position = self
                    .order
                    .iter()
                    .position(|entry| entry.entry_id == removal.entry_id)?;
                Some(RemovedItem::PlaylistEntry {
                    playlist_id: playlist_id.clone(),
                    entry_id: removal.entry_id.clone(),
                    video_id: removal.video_id.clone(),
                    position: u32::try_from(position).ok()?,
                })
            })
            .collect()
    }

    fn observe(&self, playlist: &Playlist, entries: &[PlaylistEntry]) -> EditReadback {
        let confirmed = match &self.write {
            PlaylistEdit::Update { title, description } => {
                title.as_ref().is_none_or(|title| title == &playlist.title)
                    && description.as_ref().is_none_or(|description| {
                        description == playlist.description.as_deref().unwrap_or_default()
                    })
            }
            PlaylistEdit::SetPrivacy(privacy) => playlist.privacy == Some(*privacy),
            PlaylistEdit::Move { .. } => {
                exact_occurrence_order(entries).is_ok_and(|after| self.order == after)
            }
            PlaylistEdit::Remove(requested) => return self.observe_removals(entries, requested),
            // A readable playlist disproves absence. A failed request is classified separately
            // as inconclusive until the provider attests to the removal of the exact resource.
            PlaylistEdit::Delete => false,
        };
        if confirmed {
            EditReadback::All
        } else {
            EditReadback::Unknown
        }
    }

    fn observe_removals(
        &self,
        entries: &[PlaylistEntry],
        requested: &[PlaylistOccurrenceIdentity],
    ) -> EditReadback {
        let Ok(after) = exact_occurrence_order(entries) else {
            return EditReadback::Unknown;
        };
        let remaining: HashSet<_> = after.iter().map(|entry| &entry.entry_id).collect();
        let removed: HashSet<_> = requested
            .iter()
            .filter(|entry| !remaining.contains(&entry.entry_id))
            .map(|entry| entry.entry_id.clone())
            .collect();
        if removed.is_empty()
            || !self
                .order
                .iter()
                .filter(|entry| !removed.contains(&entry.entry_id))
                .eq(&after)
        {
            return EditReadback::Unknown;
        }
        if removed.len() == requested.len() {
            EditReadback::All
        } else {
            EditReadback::Removed(removed)
        }
    }
}

/// Whether one exact playlist entry can be taken back, decided from a fresh complete read made
/// before anything is sent (the app's undo of its own addition).
pub(crate) enum EntryRemovalCheck {
    Ready(PreparedEntryRemoval),
    /// The entry is no longer in the playlist; there is nothing to send.
    Absent,
    /// Nothing was sent: the playlist could not be read completely, its order was not proven, or
    /// the entry now holds another song.
    Unsafe(ControlError),
}

/// How a sent removal of one exact entry ended.
pub(crate) enum EntryRemoval {
    /// A fresh complete read shows the entry gone and every other entry in its earlier order.
    Removed,
    /// The provider refused the removal.
    Rejected(ControlError),
    /// The removal may have happened; the fresh read did not prove the result.
    Unknown(ControlError),
}

/// A removal of exactly one entry, prepared against the order read before it.
pub(crate) struct PreparedEntryRemoval {
    playlist: PlaylistId,
    edit: PreparedEdit,
}

/// Reads `playlist` completely and prepares removing `entry` (that occurrence only, never
/// another copy of the same song) with the same checks as a control removal.
pub(crate) async fn prepare_entry_removal(
    catalog: &dyn ControlCatalog,
    context: CatalogReadContext,
    playlist: &PlaylistId,
    entry: &PlaylistOccurrenceIdentity,
) -> EntryRemovalCheck {
    let before = match load_complete(catalog, context, playlist).await {
        Ok((_, before)) => before,
        Err(error) => return EntryRemovalCheck::Unsafe(error),
    };
    match before
        .iter()
        .find(|found| found.entry_id.as_deref() == Some(entry.entry_id.as_str()))
    {
        None => return EntryRemovalCheck::Absent,
        Some(found) if found.track.video_id != entry.video_id => {
            return EntryRemovalCheck::Unsafe(stale("the playlist entry now holds another song"));
        }
        Some(_) => {}
    }
    let action = WriteAction::RemovePlaylistEntries {
        playlist_id: playlist.clone(),
        entry_ids: vec![entry.entry_id.clone()],
    };
    match PreparedEdit::new(&action, &before) {
        Ok(edit) => EntryRemovalCheck::Ready(PreparedEntryRemoval {
            playlist: playlist.clone(),
            edit,
        }),
        Err(error) => EntryRemovalCheck::Unsafe(error),
    }
}

impl PreparedEntryRemoval {
    /// Sends the removal once (never retried) and confirms it by a fresh complete read.
    pub(crate) async fn send(
        self,
        catalog: &dyn ControlCatalog,
        context: CatalogReadContext,
    ) -> EntryRemoval {
        match catalog
            .edit_playlist(
                context.clone(),
                self.playlist.clone(),
                self.edit.write.clone(),
            )
            .await
        {
            Ok(()) | Err(CatalogWriteError::Unknown(_)) => {}
            Err(CatalogWriteError::Rejected(error)) => return EntryRemoval::Rejected(error),
        }
        match load_complete(catalog, context, &self.playlist).await {
            Ok((playlist, entries)) => match self.edit.observe(&playlist, &entries) {
                EditReadback::All => EntryRemoval::Removed,
                EditReadback::Removed(_) | EditReadback::Unknown => EntryRemoval::Unknown(
                    unavailable("fresh YouTube Music data did not confirm the removal"),
                ),
            },
            Err(error) => EntryRemoval::Unknown(error),
        }
    }
}

fn exact_occurrence_order(
    entries: &[PlaylistEntry],
) -> Result<Vec<PlaylistOccurrenceIdentity>, ControlError> {
    let mut ids = HashSet::new();
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let id = entry
                .entry_id
                .as_deref()
                .filter(|id| {
                    validate_id_str(id).is_ok()
                        && ids.insert(*id)
                        && entry.position as usize == index
                })
                .ok_or_else(|| {
                    unavailable("the provider did not prove exact playlist occurrence order")
                })?;
            Ok(PlaylistOccurrenceIdentity {
                entry_id: id.to_owned(),
                video_id: entry.track.video_id.clone(),
            })
        })
        .collect()
}
