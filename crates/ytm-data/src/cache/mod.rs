pub use crate::probe::{CursorKind, HOME_FALLBACK_NOTICE, decode_cursor, encode_cursor};
use ytm_protocol::{
    AccountEpoch, AccountScope, CatalogQueryKind, CatalogQueryRequest, QueryError, QueryErrorCode,
};

/// Normalized deduplication key for read-only catalog queries scoped to an account session.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CatalogQueryKey {
    pub account_epoch: Option<String>,
    pub account_scope: Option<String>,
    pub kind_tag: &'static str,
    pub target_id: String,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

impl CatalogQueryKey {
    pub fn from_request(
        account_epoch: Option<&AccountEpoch>,
        account_scope: Option<&AccountScope>,
        request: &CatalogQueryRequest,
    ) -> Self {
        let kind_tag = match request.kind {
            CatalogQueryKind::Home => "home",
            CatalogQueryKind::SearchSongs => "searchSongs",
            CatalogQueryKind::LibraryPlaylists => "libraryPlaylists",
            CatalogQueryKind::PlaylistTracks => "playlistTracks",
            CatalogQueryKind::AlbumDetail => "albumDetail",
            CatalogQueryKind::ArtistDetail => "artistDetail",
            CatalogQueryKind::LikedSongs => "likedSongs",
            CatalogQueryKind::RecentHistory => "recentHistory",
        };
        let target_id = match request.kind {
            CatalogQueryKind::SearchSongs => request
                .query
                .as_deref()
                .map(str::trim)
                .unwrap_or_default()
                .to_string(),
            CatalogQueryKind::PlaylistTracks => request
                .playlist_id
                .as_ref()
                .map(|id| id.as_str().to_string())
                .unwrap_or_default(),
            CatalogQueryKind::AlbumDetail => request
                .album_id
                .as_ref()
                .map(|id| id.as_str().to_string())
                .unwrap_or_default(),
            CatalogQueryKind::ArtistDetail => request
                .artist_id
                .as_ref()
                .map(|id| id.as_str().to_string())
                .unwrap_or_default(),
            CatalogQueryKind::Home
            | CatalogQueryKind::LibraryPlaylists
            | CatalogQueryKind::LikedSongs
            | CatalogQueryKind::RecentHistory => String::new(),
        };

        Self {
            account_epoch: account_epoch.map(|e| e.as_str().to_string()),
            account_scope: account_scope.map(|s| s.as_str().to_string()),
            kind_tag,
            target_id,
            cursor: request.cursor.clone(),
            limit: request.limit,
        }
    }
}

/// Returns `true` strictly for transient read errors safe to retry with backoff.
/// Never retries authentication, mismatch, invalid payload, unsupported capability, or stale context errors.
pub fn is_retryable_read_error(err: &QueryError) -> bool {
    if !err.retriable {
        return false;
    }
    matches!(
        err.code,
        QueryErrorCode::RateLimited | QueryErrorCode::Timeout | QueryErrorCode::Busy
    )
}
