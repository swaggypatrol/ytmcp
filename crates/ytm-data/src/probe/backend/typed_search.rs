use ytmapi_rs::parse::{SearchResultAlbum, SearchResultArtist, SearchResultPlaylist};
use ytmapi_rs::query::search::{AlbumsFilter, ArtistsFilter, PlaylistsFilter};

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypedSearchKind {
    Albums,
    Artists,
    Playlists,
}

impl TypedSearchKind {
    pub const fn source(self) -> &'static str {
        match self {
            Self::Albums => "youtube_music_search_albums",
            Self::Artists => "youtube_music_search_artists",
            Self::Playlists => "youtube_music_search_playlists",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderTypedSearchItem {
    Album {
        album_id: AlbumId,
        title: String,
        artist: Option<String>,
        year: Option<u16>,
        cover: Option<CoverRef>,
    },
    Artist {
        artist_id: ArtistId,
        name: String,
        subscribers: Option<String>,
        cover: Option<CoverRef>,
    },
    Playlist(PlaylistSummary),
}

impl ProviderTypedSearchItem {
    pub(in crate::probe) fn id(&self) -> &str {
        match self {
            Self::Album { album_id, .. } => album_id.as_str(),
            Self::Artist { artist_id, .. } => artist_id.as_str(),
            Self::Playlist(playlist) => playlist.playlist_id.as_str(),
        }
    }
}

impl YtmapiCatalogBackend {
    pub(super) async fn read_typed_search(
        &self,
        session: &Arc<dyn CatalogSession>,
        kind: TypedSearchKind,
        query: &str,
        continuation: Option<&str>,
    ) -> Result<BackendPage<ProviderTypedSearchItem>, QueryError> {
        let yt = Self::downcast_session(session)?;
        let (items, next_continuation) = match kind {
            TypedSearchKind::Albums => {
                let query = SearchQuery::new_filtered(query, AlbumsFilter);
                let (rows, next): (Vec<SearchResultAlbum>, _) =
                    run_continuable_page(yt, &query, continuation).await?;
                let items = rows
                    .into_iter()
                    .map(project_album)
                    .collect::<Result<_, _>>()?;
                (items, next)
            }
            TypedSearchKind::Artists => {
                let query = SearchQuery::new_filtered(query, ArtistsFilter);
                let (rows, next): (Vec<SearchResultArtist>, _) =
                    run_continuable_page(yt, &query, continuation).await?;
                let items = rows
                    .into_iter()
                    .map(project_artist)
                    .collect::<Result<_, _>>()?;
                (items, next)
            }
            TypedSearchKind::Playlists => {
                let query = SearchQuery::new_filtered(query, PlaylistsFilter);
                let (rows, next): (Vec<SearchResultPlaylist>, _) =
                    run_continuable_page(yt, &query, continuation).await?;
                let items = rows
                    .into_iter()
                    .filter_map(|row| project_playlist(row).transpose())
                    .collect::<Result<_, _>>()?;
                (items, next)
            }
        };
        Ok(BackendPage {
            items,
            next_continuation,
        })
    }
}

fn project_album(row: SearchResultAlbum) -> Result<ProviderTypedSearchItem, QueryError> {
    Ok(ProviderTypedSearchItem::Album {
        album_id: AlbumId::new(row.album_id.get_raw()).map_err(|_| invalid_row())?,
        title: nonempty(row.title).ok_or_else(invalid_row)?,
        artist: nonempty(row.artist),
        year: row.year.trim().parse().ok(),
        cover: best_cover_ref(&row.thumbnails),
    })
}

fn project_artist(row: SearchResultArtist) -> Result<ProviderTypedSearchItem, QueryError> {
    Ok(ProviderTypedSearchItem::Artist {
        artist_id: ArtistId::new(row.browse_id.get_raw()).map_err(|_| invalid_row())?,
        name: nonempty(row.artist).ok_or_else(invalid_row)?,
        subscribers: row.subscribers.and_then(nonempty),
        cover: best_cover_ref(&row.thumbnails),
    })
}

fn project_playlist(
    row: SearchResultPlaylist,
) -> Result<Option<ProviderTypedSearchItem>, QueryError> {
    let (id, title, author, track_count, thumbnails) = match row {
        SearchResultPlaylist::Featured(row) => (
            row.playlist_id,
            row.title,
            row.author,
            exact_song_count(&row.songs),
            row.thumbnails,
        ),
        SearchResultPlaylist::Community(row) => {
            (row.playlist_id, row.title, row.author, None, row.thumbnails)
        }
        SearchResultPlaylist::Podcast(_) => return Ok(None),
        _ => return Err(invalid_row()),
    };
    let browse_id = id.get_raw();
    let playlist_id = browse_id
        .strip_prefix("VL")
        .and_then(|id| PlaylistId::new(id).ok())
        .ok_or_else(invalid_row)?;
    Ok(Some(ProviderTypedSearchItem::Playlist(PlaylistSummary {
        playlist_id,
        browse_id: Some(BrowseId::new(browse_id).map_err(|_| invalid_row())?),
        title: nonempty(title).ok_or_else(invalid_row)?,
        subtitle: nonempty(author),
        track_count,
        cover: best_cover_ref(&thumbnails),
    })))
}

fn exact_song_count(value: &str) -> Option<u32> {
    let (number, label) = value.trim().rsplit_once(' ')?;
    if !matches!(label, "song" | "songs") {
        return None;
    }
    let mut groups = number.split(',');
    let first = groups.next()?;
    if first.is_empty() || !first.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut count = first.parse::<u32>().ok()?;
    for group in groups {
        if first.len() > 3 || group.len() != 3 || !group.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        count = count.checked_mul(1_000)?.checked_add(group.parse().ok()?)?;
    }
    Some(count)
}

fn nonempty(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}

fn invalid_row() -> QueryError {
    make_query_error(
        QueryErrorCode::InvalidPayload,
        false,
        None,
        "YouTube Music returned an invalid typed search result",
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn playlist_projection_uses_browse_identity_and_only_exact_known_counts() {
        let playlist =
            |row| project_playlist(serde_json::from_value::<SearchResultPlaylist>(row).unwrap());
        let featured = playlist(json!({"Featured": {
            "playlist_id": "VLRD-fixture", "title": "Featured fixture", "author": "Synthetic label",
            "songs": "1,234 songs", "thumbnails": []
        }}))
        .unwrap()
        .unwrap();
        let ProviderTypedSearchItem::Playlist(featured) = featured else {
            panic!()
        };
        assert_eq!(featured.playlist_id.as_str(), "RD-fixture");
        assert_eq!(featured.browse_id.unwrap().as_str(), "VLRD-fixture");
        assert_eq!(featured.track_count, Some(1_234));

        let community = playlist(json!({"Community": {
            "playlist_id": "VLPL-fixture", "title": "Community fixture", "author": "Synthetic label",
            "views": "100 views", "thumbnails": []
        }})).unwrap().unwrap();
        let ProviderTypedSearchItem::Playlist(community) = community else {
            panic!()
        };
        assert_eq!(community.playlist_id.as_str(), "PL-fixture");
        assert_eq!(community.track_count, None);

        assert!(playlist(json!({"Podcast": {
            "podcast_id": "MPSP-fixture", "title": "Podcast fixture", "publisher": "Synthetic label",
            "thumbnails": []
        }})).unwrap().is_none());
        assert_eq!(exact_song_count("1 song"), Some(1));
        assert_eq!(exact_song_count("1.2K songs"), None);
        assert_eq!(exact_song_count("1,23 songs"), None);
        assert_eq!(exact_song_count("123 views"), None);
    }
}
