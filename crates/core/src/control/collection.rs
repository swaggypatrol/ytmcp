use std::collections::HashSet;

use ytm_control_api::{ControlError, PageInput, PlayContextTarget};
use ytm_protocol::{Freshness, PlaybackRef, TrackAvailability};

use super::write::MusicOperation;
use super::{invalid, map_query_error, stale, unavailable};

const COLLECTION_PAGE_LIMIT: u16 = 100;
const COLLECTION_PAGE_BUDGET: usize = 20;

impl MusicOperation {
    pub(super) async fn resolve_collection(
        &self,
        target: &PlayContextTarget,
    ) -> Result<PlaybackRef, ControlError> {
        let (catalog, mut context) = self.service.catalog_context(&self.state)?;
        context.cancel = self.cancel.clone();
        let (playlist, index, entry_id) = match target {
            PlayContextTarget::Album {
                album_id,
                start_index,
            } => {
                let index = start_index.unwrap_or(0);
                let found = catalog
                    .album_playback_target(context, album_id.clone(), index)
                    .await
                    .map_err(map_query_error)?;
                if found.playlist_id.is_none() || found.start_index != Some(index) {
                    return Err(stale(
                        "the catalog did not verify the album playback context",
                    ));
                }
                self.service
                    .check_read_context(&self.grant, &self.state)
                    .await?;
                return Ok(found);
            }
            PlayContextTarget::Playlist {
                playlist_id,
                start_index,
                start_entry_id,
            } => (playlist_id.clone(), *start_index, start_entry_id.as_deref()),
            _ => return Err(invalid("a collection play requires an album or playlist")),
        };
        if index.is_some() && entry_id.is_some() {
            return Err(invalid("provide either startIndex or startEntryId"));
        }
        if playlist.as_str().starts_with("VL") {
            return Err(invalid(
                "use the playlist ID from the catalog rather than its browse ID",
            ));
        }
        if entry_id
            .is_some_and(|id| id.is_empty() || id.len() > 512 || id.chars().any(char::is_control))
        {
            return Err(invalid("invalid playlist occurrence ID"));
        }

        let mut cursor = None;
        let mut seen = HashSet::new();
        for _ in 0..COLLECTION_PAGE_BUDGET {
            if self.cancel.is_cancelled() {
                return Err(stale("collection playback was cancelled before dispatch"));
            }
            let result = catalog
                .fresh_playlist(
                    context.clone(),
                    playlist.clone(),
                    PageInput {
                        cursor,
                        limit: Some(COLLECTION_PAGE_LIMIT),
                    },
                )
                .await
                .map_err(map_query_error)?;
            if result.playlist_id != playlist
                || !result.identity_confirmed
                || result.entries.freshness != Freshness::Fresh
            {
                return Err(stale("the catalog did not verify this playlist"));
            }
            if let Some(entry) = result
                .entries
                .items
                .into_iter()
                .find(|entry| match entry_id {
                    Some(id) => entry.entry_id.as_deref() == Some(id),
                    None => entry.position == index.unwrap_or(0),
                })
            {
                if entry.track.availability != TrackAvailability::Available {
                    return Err(unavailable(
                        "the requested collection occurrence is not playable",
                    ));
                }
                self.service
                    .check_read_context(&self.grant, &self.state)
                    .await?;
                return Ok(PlaybackRef {
                    video_id: entry.track.video_id,
                    playlist_id: Some(playlist),
                    playlist_entry_id: entry_id.map(str::to_owned),
                    browse_id: None,
                    queue_entry_id: None,
                    start_index: Some(entry.position),
                });
            }
            let Some(next) = result.entries.next_cursor else {
                return if result.entries.complete {
                    Err(invalid(
                        "the requested occurrence is outside this collection",
                    ))
                } else {
                    Err(unavailable(
                        "the requested occurrence could not be verified",
                    ))
                };
            };
            if !seen.insert(next.clone()) {
                break;
            }
            cursor = Some(next);
        }
        Err(unavailable(
            "the requested occurrence is outside the collection lookup budget",
        ))
    }
}
