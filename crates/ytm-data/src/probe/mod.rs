pub mod adapter;
pub mod backend;
pub mod cursor;
pub mod error;
pub mod normalize;

#[cfg(test)]
mod tests;

pub use adapter::{
    AlbumMembershipEvidence, ArtistSubscriptionEvidence, FreshPlaylistPage, MOOD_PLAYLISTS_SOURCE,
    MusicPageRequest, ProviderDiscoveryPage, ProviderExtendedPage, ProviderLibraryPage,
    ProviderSearchSuggestions, ProviderTypedSearchPage, SEARCH_SUGGESTIONS_SOURCE,
    SONG_RADIO_SOURCE, SongMembershipEvidence, SongRadioPage, SongRatingEvidence,
    YtmCatalogAdapter, YtmCatalogConfig, format_cookie_header,
};
pub use backend::{
    AddedPlaylistOccurrence, BackendAlbumMembership, BackendDiscoveryPage, BackendExtendedPage,
    BackendFreshPlaylist, BackendPage, BackendPlaylistPage, CatalogBackend, CatalogMutationError,
    CatalogSession, DiscoverySurface, ExtendedMediaKind, ExtendedMediaQuery, LibrarySurface,
    PROVIDER_NAME, PROVIDER_VERSION, PlaylistAppendDuplicates, PlaylistMetadata, PlaylistMutation,
    PlaylistMutationAck, PlaylistOccurrence, PlaylistPrivacy, PlaylistRemoval,
    ProviderDiscoveryItem, ProviderDiscoveryTarget, ProviderExtendedItem, ProviderLibraryItem,
    ProviderTypedSearchItem, SongRating, TypedSearchKind, YtmapiCatalogBackend,
};
pub use cursor::{CursorKind, decode_cursor, encode_cursor};
pub use error::{classify_upstream_status, classify_ytmapi_error, make_query_error};
pub use normalize::{
    HOME_FALLBACK_NOTICE, SearchOverview, best_cover_ref, ensure_vl_browse_id, lyrics_browse_id,
    normalize_album_detail, normalize_artist_detail, normalize_history_periods,
    normalize_playlist_item, normalize_playlist_metadata, normalize_search_overview,
    normalize_search_song, parse_duration_ms, parse_track_count, strip_vl_prefix,
};
