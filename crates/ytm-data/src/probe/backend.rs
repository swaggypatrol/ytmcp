use serde_json::{Map, Value};
use std::any::Any;
use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;
use ytm_core::ports::{BoxFuture, OfficialLyrics};
use ytm_protocol::{
    AlbumDetail, AlbumId, ArtistDetail, ArtistId, BrowseId, CollectionPlayback, CoverRef,
    PlaybackRef, PlaylistId, PlaylistSummary, QueryError, QueryErrorCode, Track, VideoId,
};
use ytmapi_rs::YtMusic;
use ytmapi_rs::auth::{AuthToken, BrowserToken};
use ytmapi_rs::common::{
    AlbumID, ArtistChannelID, LikeStatus, LyricsID, PlaylistID, Thumbnail, VideoID, YoutubeID,
};
use ytmapi_rs::continuations::ParseFromContinuable;
use ytmapi_rs::json::Json;
use ytmapi_rs::parse::{
    GetArtist, GetPlaylistDetails, HistoryPeriod, ParseFrom, PlaylistItem, ProcessedResult,
    SearchResultSong, SearchResults,
};
use ytmapi_rs::query::playlist::{
    AddPlaylistItemsQuery, DuplicateHandlingMode, GetPlaylistDetailsQuery, GetPlaylistTracksQuery,
};
use ytmapi_rs::query::search::{BasicSearch, SongsFilter};
use ytmapi_rs::query::{
    GetAlbumQuery, GetArtistQuery, GetContinuationsQuery, GetHistoryQuery,
    GetLibraryPlaylistsQuery, GetLyricsIDQuery, GetLyricsQuery, GetMethod, GetQuery, PostMethod,
    PostQuery, Query, RateSongQuery, SearchQuery,
};

use crate::auth::parse_datasync_ids_response;
use crate::probe::error::{classify_ytmapi_error, make_query_error};
use crate::probe::normalize::{
    SearchOverview, best_cover_ref, ensure_vl_browse_id, is_system_library_playlist,
    lyrics_browse_id, normalize_artist_detail, normalize_history_periods, normalize_playlist_item,
    normalize_playlist_metadata, normalize_search_overview, normalize_search_song,
    parse_track_count, strip_vl_prefix,
};

mod album_playback;
mod discovery;
mod extended_media;
mod library;
mod music;
mod playlist_playback;
mod typed_search;
pub(super) use discovery::scoped_mood_category_id;
pub use discovery::{
    BackendDiscoveryPage, DiscoverySurface, ProviderDiscoveryItem, ProviderDiscoveryTarget,
};
pub use extended_media::{
    BackendExtendedPage, ExtendedMediaKind, ExtendedMediaQuery, ProviderExtendedItem,
};
pub use library::{BackendAlbumMembership, LibrarySurface, ProviderLibraryItem};
use music::unsupported_music_operation;
pub use music::{
    AddedPlaylistOccurrence, BackendFreshPlaylist, CatalogMutationError, PlaylistAppendDuplicates,
    PlaylistMetadata, PlaylistMutation, PlaylistMutationAck, PlaylistOccurrence, PlaylistPrivacy,
    PlaylistRemoval, SongRating,
};
pub use typed_search::{ProviderTypedSearchItem, TypedSearchKind};

pub const PROVIDER_NAME: &str = "ytmapi-rs";
pub const PROVIDER_VERSION: &str = "ytmapi-rs/0.3.4";
/// Albums/artists/playlists shown per search; only the first unfiltered page is read.
pub const SEARCH_OVERVIEW_MAX: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendPage<T> {
    pub items: Vec<T>,
    pub next_continuation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendPlaylistPage {
    pub title: String,
    pub description: Option<String>,
    pub author: Option<String>,
    pub cover: Option<CoverRef>,
    pub tracks: Vec<Track>,
    pub next_continuation: Option<String>,
    /// The page's native playback context, aligned with `tracks`; see `CollectionPlayback`.
    pub playback: Option<CollectionPlayback>,
}

pub trait CatalogSession: Send + Sync {
    fn as_any(&self) -> &dyn Any;
}

/// Internal provider backend trait so production uses `ytmapi-rs 0.3.4` while
/// tests can inject deterministic fixture/wiremock backends without touching Google.
pub trait CatalogBackend: Send + Sync {
    fn authenticate<'a>(
        &'a self,
        cookie_header: &'a str,
    ) -> BoxFuture<'a, Result<Arc<dyn CatalogSession>, QueryError>>;

    /// Reads the session's InnerTube `DATASYNC_ID` (raw; callers hash it immediately).
    fn fetch_datasync_id<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
    ) -> BoxFuture<'a, Result<Option<String>, QueryError>>;

    fn search_songs<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        query: &'a str,
        continuation: Option<&'a str>,
    ) -> BoxFuture<'a, Result<BackendPage<Track>, QueryError>>;

    /// First page of the unfiltered search (albums, artists, playlists), bounded to a few each.
    fn search_overview<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        query: &'a str,
    ) -> BoxFuture<'a, Result<SearchOverview, QueryError>>;

    fn typed_search<'a>(
        &'a self,
        _session: &'a Arc<dyn CatalogSession>,
        _kind: TypedSearchKind,
        _query: &'a str,
        _continuation: Option<&'a str>,
    ) -> BoxFuture<'a, Result<BackendPage<ProviderTypedSearchItem>, QueryError>> {
        Box::pin(async { Err(unsupported_music_operation()) })
    }

    fn album_playback_target<'a>(
        &'a self,
        _session: &'a Arc<dyn CatalogSession>,
        _album_id: &'a AlbumId,
        _index: u32,
    ) -> BoxFuture<'a, Result<PlaybackRef, QueryError>> {
        Box::pin(async { Err(unsupported_music_operation()) })
    }

    fn library_playlists<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        continuation: Option<&'a str>,
    ) -> BoxFuture<'a, Result<BackendPage<PlaylistSummary>, QueryError>>;

    fn playlist_tracks<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        playlist_id: &'a PlaylistId,
        continuation: Option<&'a str>,
    ) -> BoxFuture<'a, Result<BackendPlaylistPage, QueryError>>;

    fn album_detail<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        album_id: &'a AlbumId,
    ) -> BoxFuture<'a, Result<AlbumDetail, QueryError>>;

    fn artist_detail<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        artist_id: &'a ArtistId,
    ) -> BoxFuture<'a, Result<ArtistDetail, QueryError>>;

    fn recent_history<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
    ) -> BoxFuture<'a, Result<Vec<Track>, QueryError>>;

    /// Official plain lyrics via the watch-next lyrics browse id (not the unimplemented
    /// watch-playlist tracks); `None` when the track has no lyrics tab.
    fn official_lyrics<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        video_id: &'a VideoId,
    ) -> BoxFuture<'a, Result<Option<OfficialLyrics>, QueryError>>;

    fn set_like<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        video_id: &'a VideoId,
        liked: bool,
    ) -> BoxFuture<'a, Result<(), QueryError>>;

    /// Adds one song, refusing a duplicate. Returns the entry the add created when the
    /// provider's reply named exactly one for this song (needed to undo exactly that entry).
    fn add_to_playlist<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        playlist_id: &'a PlaylistId,
        video_id: &'a VideoId,
    ) -> BoxFuture<'a, Result<Option<String>, QueryError>>;

    fn discovery<'a>(
        &'a self,
        _session: &'a Arc<dyn CatalogSession>,
        _surface: DiscoverySurface,
        _continuation: Option<&'a str>,
    ) -> BoxFuture<'a, Result<BackendDiscoveryPage, QueryError>> {
        Box::pin(async { Err(unsupported_music_operation()) })
    }

    fn mood_category<'a>(
        &'a self,
        _session: &'a Arc<dyn CatalogSession>,
        _category_id: &'a str,
        _scope_key: &'a [u8; 32],
    ) -> BoxFuture<'a, Result<BackendDiscoveryPage, QueryError>> {
        Box::pin(async { Err(unsupported_music_operation()) })
    }

    fn search_predictions<'a>(
        &'a self,
        _query: &'a str,
    ) -> BoxFuture<'a, Result<Vec<String>, QueryError>> {
        Box::pin(async { Err(unsupported_music_operation()) })
    }

    fn extended_media<'a>(
        &'a self,
        _session: &'a Arc<dyn CatalogSession>,
        _query: ExtendedMediaQuery<'a>,
        _continuation: Option<&'a str>,
    ) -> BoxFuture<'a, Result<BackendExtendedPage, QueryError>> {
        Box::pin(async { Err(unsupported_music_operation()) })
    }

    fn library_page<'a>(
        &'a self,
        _session: &'a Arc<dyn CatalogSession>,
        _surface: LibrarySurface,
        _continuation: Option<&'a str>,
    ) -> BoxFuture<'a, Result<BackendPage<ProviderLibraryItem>, QueryError>> {
        Box::pin(async { Err(unsupported_music_operation()) })
    }

    fn song_membership<'a>(
        &'a self,
        _session: &'a Arc<dyn CatalogSession>,
        _video: &'a VideoId,
    ) -> BoxFuture<'a, Result<Option<bool>, QueryError>> {
        Box::pin(async { Err(unsupported_music_operation()) })
    }

    fn write_song_membership<'a>(
        &'a self,
        _session: &'a Arc<dyn CatalogSession>,
        _video: &'a VideoId,
        _saved: bool,
    ) -> BoxFuture<'a, Result<(), CatalogMutationError>> {
        Box::pin(async { Err(CatalogMutationError::Rejected(unsupported_music_operation())) })
    }

    fn playlist_membership<'a>(
        &'a self,
        _session: &'a Arc<dyn CatalogSession>,
        _playlist: &'a PlaylistId,
    ) -> BoxFuture<'a, Result<Option<bool>, QueryError>> {
        Box::pin(async { Err(unsupported_music_operation()) })
    }

    fn write_playlist_membership<'a>(
        &'a self,
        _session: &'a Arc<dyn CatalogSession>,
        _playlist: &'a PlaylistId,
        _saved: bool,
    ) -> BoxFuture<'a, Result<(), CatalogMutationError>> {
        Box::pin(async { Err(CatalogMutationError::Rejected(unsupported_music_operation())) })
    }

    fn album_membership<'a>(
        &'a self,
        _session: &'a Arc<dyn CatalogSession>,
        _album: &'a AlbumId,
    ) -> BoxFuture<'a, Result<BackendAlbumMembership, QueryError>> {
        Box::pin(async { Err(unsupported_music_operation()) })
    }

    fn write_album_membership<'a>(
        &'a self,
        _session: &'a Arc<dyn CatalogSession>,
        _album: &'a AlbumId,
        _expected_playlist: &'a PlaylistId,
        _saved: bool,
    ) -> BoxFuture<'a, Result<(), CatalogMutationError>> {
        Box::pin(async { Err(CatalogMutationError::Rejected(unsupported_music_operation())) })
    }

    fn artist_subscription<'a>(
        &'a self,
        _session: &'a Arc<dyn CatalogSession>,
        _artist: &'a ArtistId,
    ) -> BoxFuture<'a, Result<Option<bool>, QueryError>> {
        Box::pin(async { Err(unsupported_music_operation()) })
    }

    fn write_artist_subscription<'a>(
        &'a self,
        _session: &'a Arc<dyn CatalogSession>,
        _artist: &'a ArtistId,
        _subscribed: bool,
    ) -> BoxFuture<'a, Result<(), CatalogMutationError>> {
        Box::pin(async { Err(CatalogMutationError::Rejected(unsupported_music_operation())) })
    }

    #[doc(hidden)]
    fn fresh_playlist<'a>(
        &'a self,
        _session: &'a Arc<dyn CatalogSession>,
        _playlist_id: &'a PlaylistId,
        _continuation: Option<&'a str>,
    ) -> BoxFuture<'a, Result<BackendFreshPlaylist, QueryError>> {
        Box::pin(async { Err(unsupported_music_operation()) })
    }

    #[doc(hidden)]
    fn song_radio<'a>(
        &'a self,
        _session: &'a Arc<dyn CatalogSession>,
        _seed: &'a VideoId,
        _continuation: Option<&'a str>,
    ) -> BoxFuture<'a, Result<BackendPage<Track>, QueryError>> {
        Box::pin(async { Err(unsupported_music_operation()) })
    }

    fn write_playlist<'a>(
        &'a self,
        _session: &'a Arc<dyn CatalogSession>,
        _request: PlaylistMutation<'a>,
    ) -> BoxFuture<'a, Result<PlaylistMutationAck, CatalogMutationError>> {
        Box::pin(async { Err(CatalogMutationError::Rejected(unsupported_music_operation())) })
    }

    fn read_rating<'a>(
        &'a self,
        _session: &'a Arc<dyn CatalogSession>,
        _video: &'a VideoId,
    ) -> BoxFuture<'a, Result<SongRating, QueryError>> {
        Box::pin(async { Err(unsupported_music_operation()) })
    }

    fn write_rating<'a>(
        &'a self,
        _session: &'a Arc<dyn CatalogSession>,
        _video: &'a VideoId,
        _rating: SongRating,
    ) -> BoxFuture<'a, Result<(), CatalogMutationError>> {
        Box::pin(async { Err(CatalogMutationError::Rejected(unsupported_music_operation())) })
    }
}

struct YtmapiSession {
    yt: YtMusic<BrowserToken>,
}

impl CatalogSession for YtmapiSession {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Production `CatalogBackend` backed directly by `ytmapi-rs 0.3.4` (`YtMusic<BrowserToken>`).
#[derive(Clone)]
pub struct YtmapiCatalogBackend {
    client: ytmapi_rs::Client,
}

impl std::fmt::Debug for YtmapiCatalogBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("YtmapiCatalogBackend")
            .field("provider", &PROVIDER_VERSION)
            .finish()
    }
}

impl YtmapiCatalogBackend {
    pub fn new(timeout: Duration) -> Result<Self, QueryError> {
        let reqwest_client = reqwest::Client::builder()
            .use_rustls_tls()
            // music.youtube.com response headers can exceed hyper's 16 KiB HTTP/2 default.
            .http2_max_header_list_size(64 * 1024)
            .timeout(timeout)
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| {
                make_query_error(
                    QueryErrorCode::NetworkOffline,
                    false,
                    None,
                    "failed to initialize HTTPS client for YouTube Music",
                )
            })?;
        let client = ytmapi_rs::Client::new_from_reqwest_client(reqwest_client);
        Ok(Self { client })
    }

    fn downcast_session(
        session: &Arc<dyn CatalogSession>,
    ) -> Result<&YtMusic<BrowserToken>, QueryError> {
        session
            .as_any()
            .downcast_ref::<YtmapiSession>()
            .map(|s| &s.yt)
            .ok_or_else(|| {
                make_query_error(
                    QueryErrorCode::AuthRequired,
                    false,
                    None,
                    "invalid catalog session handle",
                )
            })
    }
}

#[derive(Debug)]
struct NoopParseOutput;

impl<Q> ParseFrom<DirectContinuationQuery<'_, Q>> for NoopParseOutput {
    fn parse_from(_: ProcessedResult<DirectContinuationQuery<'_, Q>>) -> ytmapi_rs::Result<Self> {
        Ok(Self)
    }
}

impl ParseFrom<DatasyncIdsQuery> for NoopParseOutput {
    fn parse_from(_: ProcessedResult<DatasyncIdsQuery>) -> ytmapi_rs::Result<Self> {
        Ok(Self)
    }
}

struct DirectContinuationQuery<'a, Q> {
    base: &'a Q,
    token: &'a str,
}

impl<Q, A> Query<A> for DirectContinuationQuery<'_, Q>
where
    Q: PostQuery + Query<A>,
    A: AuthToken,
{
    type Output = NoopParseOutput;
    type Method = PostMethod;
}

impl<Q: PostQuery> PostQuery for DirectContinuationQuery<'_, Q> {
    fn header(&self) -> serde_json::Map<String, serde_json::Value> {
        self.base.header()
    }

    fn params(&self) -> Vec<(&str, Cow<'_, str>)> {
        vec![
            ("ctoken", Cow::Borrowed(self.token)),
            ("continuation", Cow::Borrowed(self.token)),
        ]
    }

    fn path(&self) -> &str {
        self.base.path()
    }
}

/// The web client's `getDatasyncIdsEndpoint`: a small authenticated GET whose response carries
/// `responseContext.mainAppWebResponseContext.datasyncId`, the same string the page exposes as
/// ytcfg `DATASYNC_ID`. (InnerTube POST responses to this client, e.g. `account_menu` or
/// `browse`, omit `mainAppWebResponseContext` entirely.)
struct DatasyncIdsQuery;

impl<A: AuthToken> Query<A> for DatasyncIdsQuery {
    type Output = NoopParseOutput;
    type Method = GetMethod;
}

impl GetQuery for DatasyncIdsQuery {
    fn url(&self) -> &str {
        "https://music.youtube.com/getDatasyncIdsEndpoint"
    }

    fn params(&self) -> Vec<(&str, Cow<'_, str>)> {
        vec![]
    }
}

async fn fetch_continuation_page<Q>(
    yt: &YtMusic<BrowserToken>,
    query: &Q,
    token: &str,
) -> Result<Json, QueryError>
where
    Q: PostQuery + Query<BrowserToken>,
{
    let direct = DirectContinuationQuery { base: query, token };
    yt.json_query::<DirectContinuationQuery<'_, Q>>(&direct)
        .await
        .map_err(classify_ytmapi_error)
}

/// Runs a `ParseFromContinuable` query for either page 1 or an explicit continuation token.
async fn run_continuable_page<Q, T>(
    yt: &YtMusic<BrowserToken>,
    query: &Q,
    continuation: Option<&str>,
) -> Result<(T, Option<String>), QueryError>
where
    Q: PostQuery + Query<BrowserToken>,
    T: ParseFromContinuable<Q> + Default + Send,
{
    match continuation {
        None => {
            let json = yt
                .json_query::<Q>(query)
                .await
                .map_err(classify_ytmapi_error)?;
            let source = serde_json::to_string(&json).unwrap_or_default();
            let processed = ProcessedResult {
                query,
                source,
                json,
            };
            let (parsed, next) =
                T::parse_from_continuable(processed).map_err(classify_ytmapi_error)?;
            Ok((parsed, next.map(|c| c.get_raw().to_string())))
        }
        Some(token) => {
            let json = fetch_continuation_page(yt, query, token).await?;
            parse_continuation_page(query, json)
        }
    }
}

/// Parses a continuation response for `query`. An exhausted continuation is the last page:
/// YouTube Music still hands out a grid cursor after the final library item and answers it
/// with `continuationContents.gridContinuation` holding only presentation fields, which the
/// provider parser rejects for lacking `items`.
fn parse_continuation_page<Q, T>(query: &Q, json: Json) -> Result<(T, Option<String>), QueryError>
where
    T: ParseFromContinuable<Q> + Default,
{
    let value = serde_json::to_value(&json).unwrap_or_default();
    if is_exhausted_continuation(&value) {
        return Ok((T::default(), None));
    }
    let mock_cont = GetContinuationsQuery::new_mock_unchecked(query);
    let processed = ProcessedResult {
        query: &mock_cont,
        source: value.to_string(),
        json,
    };
    let (parsed, next) = T::parse_continuation(processed).map_err(classify_ytmapi_error)?;
    Ok((parsed, next.map(|c| c.get_raw().to_string())))
}

/// One parsed playlist page: its tracks, the next continuation, and the native playback
/// context read from the same rows.
pub(crate) type ParsedPlaylistPage = (Vec<Track>, Option<String>, Option<CollectionPlayback>);

/// Parses one playlist page (first page or continuation) into a `ParsedPlaylistPage`.
pub(crate) fn parse_playlist_tracks_page(
    query: &GetPlaylistTracksQuery<'_>,
    json: Json,
    continuation: bool,
) -> Result<ParsedPlaylistPage, QueryError> {
    let raw = serde_json::to_value(&json).unwrap_or_default();
    let (items, next) = parse_playlist_items_page(query, json, continuation)?;
    let tracks = items
        .into_iter()
        .map(normalize_playlist_item)
        .collect::<Result<Vec<_>, _>>()?;
    let rows = raw
        .pointer(if continuation {
            PLAYLIST_NEXT_ROWS
        } else {
            PLAYLIST_FIRST_ROWS
        })
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let playback = playlist_playback::playlist_rows_playback(rows, &tracks);
    Ok((tracks, next, playback))
}

const PLAYLIST_FIRST_ROWS: &str = concat!(
    "/contents/twoColumnBrowseResultsRenderer/secondaryContents/sectionListRenderer",
    "/contents/0/musicPlaylistShelfRenderer/contents"
);
const PLAYLIST_NEXT_ROWS: &str =
    "/onResponseReceivedActions/0/appendContinuationItemsAction/continuationItems";

fn parse_playlist_items_page(
    query: &GetPlaylistTracksQuery<'_>,
    json: Json,
    continuation: bool,
) -> Result<(Vec<PlaylistItem>, Option<String>), QueryError> {
    let mut value = json.into_inner();
    let rows = if continuation {
        PLAYLIST_NEXT_ROWS
    } else {
        PLAYLIST_FIRST_ROWS
    };
    if let Some(rows) = value.pointer_mut(rows).and_then(Value::as_array_mut) {
        allow_unlinked_playlist_video_creators(rows);
    }
    let json: Json = serde_json::from_value(value).map_err(|_| {
        make_query_error(
            QueryErrorCode::InvalidPayload,
            false,
            None,
            "upstream playlist response could not be read",
        )
    })?;
    if continuation {
        parse_continuation_page(query, json)
    } else {
        let source = serde_json::to_string(&json).unwrap_or_default();
        let (items, next) = Vec::<PlaylistItem>::parse_from_continuable(ProcessedResult {
            query,
            source,
            json,
        })
        .map_err(classify_ytmapi_error)?;
        Ok((items, next.map(|cursor| cursor.get_raw().to_string())))
    }
}

fn allow_unlinked_playlist_video_creators(rows: &mut [Value]) {
    const VIDEO_TYPE: &str = concat!(
        "/overlay/musicItemThumbnailOverlayRenderer/content/musicPlayButtonRenderer",
        "/playNavigationEndpoint/watchEndpoint/watchEndpointMusicSupportedConfigs",
        "/watchEndpointMusicConfig/musicVideoType"
    );
    const CREATOR: &str = "/flexColumns/1/musicResponsiveListItemFlexColumnRenderer/text/runs/0";

    for item in rows {
        let Some(row) = item.get_mut("musicResponsiveListItemRenderer") else {
            continue;
        };
        if !matches!(
            row.pointer(VIDEO_TYPE).and_then(Value::as_str),
            Some(
                "MUSIC_VIDEO_TYPE_UGC"
                    | "MUSIC_VIDEO_TYPE_OMV"
                    | "MUSIC_VIDEO_TYPE_SHOULDER"
                    | "MUSIC_VIDEO_TYPE_OFFICIAL_SOURCE_MUSIC"
            )
        ) {
            continue;
        }
        let Some(creator) = row.pointer_mut(CREATOR).and_then(Value::as_object_mut) else {
            continue;
        };
        if !creator.get("text").is_some_and(Value::is_string) {
            continue;
        }
        let Some(navigation) = creator
            .entry("navigationEndpoint")
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
        else {
            continue;
        };
        let Some(browse) = navigation
            .entry("browseEndpoint")
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
        else {
            continue;
        };
        // ytmapi-rs 0.3.4 requires a channel string even for plain-text creators.
        // The empty string is accepted by its parser and normalizes to ArtistRef.id = None.
        browse
            .entry("browseId")
            .or_insert_with(|| Value::String(String::new()));
    }
}

fn is_exhausted_continuation(value: &serde_json::Value) -> bool {
    const CONTENT_KEYS: [&str; 3] = ["items", "contents", "continuations"];
    value
        .pointer("/continuationContents")
        .and_then(|v| v.as_object())
        .is_some_and(|kinds| {
            !kinds.is_empty()
                && kinds.values().all(|kind| {
                    kind.as_object()
                        .is_some_and(|o| CONTENT_KEYS.iter().all(|k| !o.contains_key(*k)))
                })
        })
}

/// The upstream library parser filters on English titles before parsing ordinary playlists.
/// Read this grid directly so built-in IDs are independent of locale and user playlist titles.
pub(crate) fn parse_library_playlist_page(
    value: serde_json::Value,
    continuation: bool,
) -> Result<BackendPage<PlaylistSummary>, QueryError> {
    let invalid = || {
        make_query_error(
            QueryErrorCode::InvalidPayload,
            false,
            None,
            "upstream library playlist page has an invalid grid or card",
        )
    };
    let grid = if continuation {
        value.pointer("/continuationContents/gridContinuation")
    } else {
        value
            .pointer("/contents/singleColumnBrowseResultsRenderer/tabs/0/tabRenderer/content")
            .and_then(|content| {
                content
                    .pointer("/sectionListRenderer/contents/0/gridRenderer")
                    .or_else(|| content.pointer("/itemSectionRenderer/contents/0/gridRenderer"))
            })
    };
    let Some(grid) = grid else {
        if continuation {
            return Err(invalid());
        }
        return Ok(BackendPage {
            items: vec![],
            next_continuation: None,
        });
    };
    if continuation && is_exhausted_continuation(&value) {
        return Ok(BackendPage {
            items: vec![],
            next_continuation: None,
        });
    }
    let next_continuation = grid
        .pointer("/continuations/0/nextContinuationData/continuation")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let rows = grid["items"].as_array().ok_or_else(invalid)?;
    let mut items = Vec::new();
    for row in rows {
        let card = &row["musicTwoRowItemRenderer"];
        let text = |pointer| card.pointer(pointer).and_then(serde_json::Value::as_str);
        let endpoint = card
            .pointer("/title/runs/0/navigationEndpoint")
            .or_else(|| card.get("navigationEndpoint"))
            .ok_or_else(invalid)?;
        let Some(browse) = endpoint.get("browseEndpoint") else {
            // A create action opens a dialog rather than navigating to a playlist.
            if endpoint
                .as_object()
                .is_some_and(|object| object.keys().any(|key| key.ends_with("Endpoint")))
            {
                continue;
            }
            return Err(invalid());
        };
        let raw_id = browse
            .get("browseId")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(invalid)?;
        if raw_id == "VL_CREATE" || is_system_library_playlist(raw_id) {
            continue;
        }
        let playlist_id = PlaylistId::new(strip_vl_prefix(raw_id)).map_err(|_| invalid())?;
        let thumbnails: Vec<Thumbnail> = card
            .pointer("/thumbnailRenderer/musicThumbnailRenderer/thumbnail/thumbnails")
            .map(|value| serde_json::from_value(value.clone()))
            .transpose()
            .map_err(|_| invalid())?
            .unwrap_or_default();
        items.push(PlaylistSummary {
            playlist_id,
            browse_id: BrowseId::new(ensure_vl_browse_id(raw_id)).ok(),
            title: text("/title/runs/0/text").ok_or_else(invalid)?.to_owned(),
            subtitle: text("/subtitle/runs/0/text")
                .filter(|author| !author.trim().is_empty())
                .map(str::to_owned),
            track_count: text("/subtitle/runs/2/text").and_then(parse_track_count),
            cover: best_cover_ref(&thumbnails),
        });
    }
    Ok(BackendPage {
        items,
        next_continuation,
    })
}

impl CatalogBackend for YtmapiCatalogBackend {
    fn authenticate<'a>(
        &'a self,
        cookie_header: &'a str,
    ) -> BoxFuture<'a, Result<Arc<dyn CatalogSession>, QueryError>> {
        Box::pin(async move {
            let token = BrowserToken::from_str(cookie_header, &self.client)
                .await
                .map_err(classify_ytmapi_error)?;
            let yt = ytmapi_rs::YtMusicBuilder::new_with_client(self.client.clone())
                .with_auth_token(token)
                .build()
                .map_err(classify_ytmapi_error)?;
            Ok(Arc::new(YtmapiSession { yt }) as Arc<dyn CatalogSession>)
        })
    }

    fn fetch_datasync_id<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
    ) -> BoxFuture<'a, Result<Option<String>, QueryError>> {
        Box::pin(async move {
            let yt = Self::downcast_session(session)?;
            let body = yt
                .raw_json_query::<DatasyncIdsQuery>(&DatasyncIdsQuery)
                .await
                .map_err(classify_ytmapi_error)?;
            Ok(parse_datasync_ids_response(&body))
        })
    }

    fn search_songs<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        query_str: &'a str,
        continuation: Option<&'a str>,
    ) -> BoxFuture<'a, Result<BackendPage<Track>, QueryError>> {
        Box::pin(async move {
            let yt = Self::downcast_session(session)?;
            let query = SearchQuery::new_filtered(query_str, SongsFilter);
            let (songs, next_continuation): (Vec<SearchResultSong>, Option<String>) =
                run_continuable_page(yt, &query, continuation).await?;
            let items = songs
                .into_iter()
                .map(normalize_search_song)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(BackendPage {
                items,
                next_continuation,
            })
        })
    }

    fn search_overview<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        query_str: &'a str,
    ) -> BoxFuture<'a, Result<SearchOverview, QueryError>> {
        Box::pin(async move {
            let yt = Self::downcast_session(session)?;
            let query: SearchQuery<'_, BasicSearch> = SearchQuery::from(query_str);
            let results: SearchResults = yt.query(query).await.map_err(classify_ytmapi_error)?;
            Ok(normalize_search_overview(results, SEARCH_OVERVIEW_MAX))
        })
    }

    fn typed_search<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        kind: TypedSearchKind,
        query: &'a str,
        continuation: Option<&'a str>,
    ) -> BoxFuture<'a, Result<BackendPage<ProviderTypedSearchItem>, QueryError>> {
        Box::pin(self.read_typed_search(session, kind, query, continuation))
    }

    fn album_playback_target<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        album_id: &'a AlbumId,
        index: u32,
    ) -> BoxFuture<'a, Result<PlaybackRef, QueryError>> {
        Box::pin(self.read_album_playback_target(session, album_id, index))
    }

    fn library_playlists<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        continuation: Option<&'a str>,
    ) -> BoxFuture<'a, Result<BackendPage<PlaylistSummary>, QueryError>> {
        Box::pin(async move {
            let yt = Self::downcast_session(session)?;
            let query = GetLibraryPlaylistsQuery;
            let json = match continuation {
                Some(token) => {
                    yt.json_query::<DirectContinuationQuery<'_, GetLibraryPlaylistsQuery>>(
                        &DirectContinuationQuery {
                            base: &query,
                            token,
                        },
                    )
                    .await
                }
                None => yt.json_query::<GetLibraryPlaylistsQuery>(&query).await,
            }
            .map_err(classify_ytmapi_error)?;
            parse_library_playlist_page(json.into_inner(), continuation.is_some())
        })
    }

    fn playlist_tracks<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        playlist_id: &'a PlaylistId,
        continuation: Option<&'a str>,
    ) -> BoxFuture<'a, Result<BackendPlaylistPage, QueryError>> {
        Box::pin(async move {
            let yt = Self::downcast_session(session)?;
            let browse_id = ensure_vl_browse_id(playlist_id.as_str());
            let tracks_query =
                GetPlaylistTracksQuery::new(PlaylistID::from_raw(browse_id.as_str()));
            match continuation {
                None => {
                    let json = yt
                        .json_query::<GetPlaylistTracksQuery<'_>>(&tracks_query)
                        .await
                        .map_err(classify_ytmapi_error)?;
                    let details_query =
                        GetPlaylistDetailsQuery::new(PlaylistID::from_raw(browse_id.as_str()));
                    let details_processed = ProcessedResult {
                        query: &details_query,
                        source: serde_json::to_string(&json).unwrap_or_default(),
                        json: json.clone(),
                    };
                    let (title, description, author, cover) =
                        match GetPlaylistDetails::parse_from(details_processed) {
                            Ok(details) => normalize_playlist_metadata(details),
                            Err(_) => (playlist_id.as_str().to_string(), None, None, None),
                        };

                    let (tracks, next_continuation, playback) =
                        parse_playlist_tracks_page(&tracks_query, json, false)?;
                    Ok(BackendPlaylistPage {
                        title,
                        description,
                        author,
                        cover,
                        tracks,
                        next_continuation,
                        playback,
                    })
                }
                Some(token) => {
                    let json = fetch_continuation_page(yt, &tracks_query, token).await?;
                    let (tracks, next_continuation, playback) =
                        parse_playlist_tracks_page(&tracks_query, json, true)?;
                    Ok(BackendPlaylistPage {
                        title: playlist_id.as_str().to_string(),
                        description: None,
                        author: None,
                        cover: None,
                        tracks,
                        next_continuation,
                        playback,
                    })
                }
            }
        })
    }

    fn album_detail<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        album_id: &'a AlbumId,
    ) -> BoxFuture<'a, Result<AlbumDetail, QueryError>> {
        Box::pin(async move {
            let yt = Self::downcast_session(session)?;
            let query = GetAlbumQuery::new(AlbumID::from_raw(album_id.as_str()));
            let json = yt
                .json_query::<GetAlbumQuery<'_>>(&query)
                .await
                .map_err(classify_ytmapi_error)?;
            album_playback::album_detail_from_json(album_id, &query, json)
        })
    }

    fn artist_detail<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        artist_id: &'a ArtistId,
    ) -> BoxFuture<'a, Result<ArtistDetail, QueryError>> {
        Box::pin(async move {
            let yt = Self::downcast_session(session)?;
            let query = GetArtistQuery::new(ArtistChannelID::from_raw(artist_id.as_str()));
            let artist: GetArtist = yt.query(query).await.map_err(classify_ytmapi_error)?;
            normalize_artist_detail(artist_id, artist)
        })
    }

    fn recent_history<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
    ) -> BoxFuture<'a, Result<Vec<Track>, QueryError>> {
        Box::pin(async move {
            let yt = Self::downcast_session(session)?;
            let periods: Vec<HistoryPeriod> = yt
                .query(GetHistoryQuery)
                .await
                .map_err(classify_ytmapi_error)?;
            normalize_history_periods(periods)
        })
    }

    fn official_lyrics<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        video_id: &'a VideoId,
    ) -> BoxFuture<'a, Result<Option<OfficialLyrics>, QueryError>> {
        Box::pin(async move {
            let yt = Self::downcast_session(session)?;
            let watch_next = yt
                .json_query::<GetLyricsIDQuery<'_>>(GetLyricsIDQuery::new(VideoID::from_raw(
                    video_id.as_str(),
                )))
                .await
                .map_err(classify_ytmapi_error)?;
            let Some(browse_id) = lyrics_browse_id(&watch_next.into_inner())? else {
                return Ok(None);
            };
            let lyrics = yt
                .query(GetLyricsQuery::new(LyricsID::from_raw(browse_id)))
                .await
                .map_err(classify_ytmapi_error)?;
            if lyrics.lyrics.trim().is_empty() {
                return Ok(None);
            }
            Ok(Some(OfficialLyrics {
                text: lyrics.lyrics,
                source: lyrics.source,
            }))
        })
    }

    fn set_like<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        video_id: &'a VideoId,
        liked: bool,
    ) -> BoxFuture<'a, Result<(), QueryError>> {
        Box::pin(async move {
            let yt = Self::downcast_session(session)?;
            let status = if liked {
                LikeStatus::Liked
            } else {
                LikeStatus::Indifferent
            };
            let query = RateSongQuery::new(VideoID::from_raw(video_id.as_str()), status);
            yt.query(query).await.map_err(classify_ytmapi_error)
        })
    }

    fn add_to_playlist<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        playlist_id: &'a PlaylistId,
        video_id: &'a VideoId,
    ) -> BoxFuture<'a, Result<Option<String>, QueryError>> {
        Box::pin(async move {
            let yt = Self::downcast_session(session)?;
            let query = AddPlaylistItemsQuery::new_from_videos(
                PlaylistID::from_raw(playlist_id.as_str()),
                [VideoID::from_raw(video_id.as_str())],
                DuplicateHandlingMode::ReturnError,
            );
            // The provider parser checks the status first, so a refused duplicate still fails
            // here exactly as before; only a successful reply is read for its entry.
            let added = yt.query(query).await.map_err(classify_ytmapi_error)?;
            Ok(music::single_created_entry(video_id, added))
        })
    }

    fn fresh_playlist<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        playlist_id: &'a PlaylistId,
        continuation: Option<&'a str>,
    ) -> BoxFuture<'a, Result<BackendFreshPlaylist, QueryError>> {
        Box::pin(self.read_fresh_playlist(session, playlist_id, continuation))
    }

    fn discovery<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        surface: DiscoverySurface,
        continuation: Option<&'a str>,
    ) -> BoxFuture<'a, Result<BackendDiscoveryPage, QueryError>> {
        Box::pin(self.read_discovery(session, surface, continuation))
    }

    fn mood_category<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        category_id: &'a str,
        scope_key: &'a [u8; 32],
    ) -> BoxFuture<'a, Result<BackendDiscoveryPage, QueryError>> {
        Box::pin(self.read_mood_category(session, category_id, scope_key))
    }

    fn search_predictions<'a>(
        &'a self,
        query: &'a str,
    ) -> BoxFuture<'a, Result<Vec<String>, QueryError>> {
        Box::pin(self.read_search_predictions(query))
    }

    fn extended_media<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        query: ExtendedMediaQuery<'a>,
        continuation: Option<&'a str>,
    ) -> BoxFuture<'a, Result<BackendExtendedPage, QueryError>> {
        Box::pin(self.read_extended_media(session, query, continuation))
    }

    fn library_page<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        surface: LibrarySurface,
        continuation: Option<&'a str>,
    ) -> BoxFuture<'a, Result<BackendPage<ProviderLibraryItem>, QueryError>> {
        Box::pin(self.read_library_page(session, surface, continuation))
    }

    fn song_membership<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        video: &'a VideoId,
    ) -> BoxFuture<'a, Result<Option<bool>, QueryError>> {
        Box::pin(self.read_song_membership(session, video))
    }

    fn write_song_membership<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        video: &'a VideoId,
        saved: bool,
    ) -> BoxFuture<'a, Result<(), CatalogMutationError>> {
        Box::pin(self.write_song_membership_once(session, video, saved))
    }

    fn playlist_membership<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        playlist: &'a PlaylistId,
    ) -> BoxFuture<'a, Result<Option<bool>, QueryError>> {
        Box::pin(self.read_playlist_membership(session, playlist))
    }

    fn write_playlist_membership<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        playlist: &'a PlaylistId,
        saved: bool,
    ) -> BoxFuture<'a, Result<(), CatalogMutationError>> {
        Box::pin(self.write_playlist_membership_once(session, playlist, saved))
    }

    fn album_membership<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        album: &'a AlbumId,
    ) -> BoxFuture<'a, Result<BackendAlbumMembership, QueryError>> {
        Box::pin(self.read_album_membership(session, album))
    }

    fn write_album_membership<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        album: &'a AlbumId,
        expected_playlist: &'a PlaylistId,
        saved: bool,
    ) -> BoxFuture<'a, Result<(), CatalogMutationError>> {
        Box::pin(self.write_album_membership_once(session, album, expected_playlist, saved))
    }

    fn artist_subscription<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        artist: &'a ArtistId,
    ) -> BoxFuture<'a, Result<Option<bool>, QueryError>> {
        Box::pin(self.read_artist_subscription(session, artist))
    }

    fn write_artist_subscription<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        artist: &'a ArtistId,
        subscribed: bool,
    ) -> BoxFuture<'a, Result<(), CatalogMutationError>> {
        Box::pin(self.write_artist_subscription_once(session, artist, subscribed))
    }

    fn song_radio<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        seed: &'a VideoId,
        continuation: Option<&'a str>,
    ) -> BoxFuture<'a, Result<BackendPage<Track>, QueryError>> {
        Box::pin(self.read_song_radio(session, seed, continuation))
    }

    fn write_playlist<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        request: PlaylistMutation<'a>,
    ) -> BoxFuture<'a, Result<PlaylistMutationAck, CatalogMutationError>> {
        Box::pin(self.mutate_playlist_once(session, request))
    }

    fn read_rating<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        video: &'a VideoId,
    ) -> BoxFuture<'a, Result<SongRating, QueryError>> {
        Box::pin(self.read_song_rating(session, video))
    }

    fn write_rating<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        video: &'a VideoId,
        rating: SongRating,
    ) -> BoxFuture<'a, Result<(), CatalogMutationError>> {
        Box::pin(self.rate_song_once(session, video, rating))
    }
}
