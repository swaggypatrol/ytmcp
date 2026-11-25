use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use ytm_core::ports::{BoxFuture, CatalogPort, Clock, MockClock, StoragePort};
use ytm_protocol::{
    AccountEpoch, AccountScope, AlbumDetail, AlbumId, ArtistDetail, ArtistId, BrowseId,
    CatalogQueryKind, CatalogQueryRequest, CatalogResponsePayload, CredentialRevision, Freshness,
    MutationKind, MutationOutcome, MutationRequest, PlaylistId, QueryError, QueryErrorCode,
    SessionMaterialPayload, Track, TrackAvailability, VideoId,
};
use ytm_storage::{CoverCacheConfig, RecentPlaybackRecord, SqliteStorage, StoragePaths};
use ytmapi_rs::auth::BrowserToken;
use ytmapi_rs::common::{AlbumID, ArtistChannelID, PlaylistID, YoutubeID};
use ytmapi_rs::continuations::ParseFromContinuable;
use ytmapi_rs::parse::{
    GetAlbum, GetArtist, GetPlaylistDetails, HistoryPeriod, ParseFrom, ProcessedResult,
    SearchResultSong,
};
use ytmapi_rs::query::search::SongsFilter;
use ytmapi_rs::query::{
    GetAlbumQuery, GetArtistQuery, GetContinuationsQuery, GetHistoryQuery, GetPlaylistDetailsQuery,
    GetPlaylistTracksQuery, SearchQuery,
};

use crate::auth::{account_hint_for, account_scope_for, derive_account_scope};
use crate::probe::backend::{
    BackendDiscoveryPage, BackendExtendedPage, BackendFreshPlaylist, BackendPage,
    BackendPlaylistPage, CatalogBackend, CatalogMutationError, CatalogSession, DiscoverySurface,
    ExtendedMediaKind, ExtendedMediaQuery, LibrarySurface, PlaylistMutation, PlaylistMutationAck,
    PlaylistOccurrence, ProviderDiscoveryItem, ProviderDiscoveryTarget, ProviderExtendedItem,
    ProviderLibraryItem, ProviderTypedSearchItem, SongRating, TypedSearchKind,
    parse_library_playlist_page, parse_playlist_tracks_page,
};
use crate::probe::normalize::SearchOverview;
use crate::probe::{
    CursorKind, HOME_FALLBACK_NOTICE, MusicPageRequest, YtmCatalogAdapter, YtmCatalogConfig,
    classify_upstream_status, classify_ytmapi_error, decode_cursor, ensure_vl_browse_id,
    format_cookie_header, lyrics_browse_id, make_query_error, normalize_album_detail,
    normalize_artist_detail, normalize_history_periods, normalize_playlist_metadata,
    normalize_search_song,
};

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("catalog-probe")
}

fn read_fixture(name: &str) -> String {
    let p = fixture_dir().join(name);
    std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("failed to read fixture {}: {e}", p.display()))
}

fn make_storage(clock: Arc<dyn Clock>) -> (TempDir, Arc<SqliteStorage>) {
    let dir = TempDir::new().expect("tempdir");
    let paths = StoragePaths {
        config_dir: dir.path().join("config"),
        data_dir: dir.path().join("data"),
        cache_dir: dir.path().join("cache"),
    };
    let storage =
        SqliteStorage::open(paths, clock, CoverCacheConfig::production()).expect("open storage");
    (dir, Arc::new(storage))
}

fn make_session_material(
    epoch: &str,
    rev: &str,
    sapisid: &str,
    channel_label: Option<&str>,
) -> SessionMaterialPayload {
    let mut cookie_pairs = BTreeMap::new();
    cookie_pairs.insert("APISID".to_string(), "synthetic_apisid_01".to_string());
    cookie_pairs.insert("HSID".to_string(), "synthetic_hsid_01".to_string());
    cookie_pairs.insert("LOGIN_INFO".to_string(), "synthetic_login_01".to_string());
    cookie_pairs.insert("SAPISID".to_string(), sapisid.to_string());
    SessionMaterialPayload {
        account_epoch: AccountEpoch::new(epoch).unwrap(),
        credential_revision: CredentialRevision::new(rev).unwrap(),
        cookie_pairs,
        auth_user_index: Some(0),
        channel_label: channel_label.map(str::to_string),
    }
}

fn parse_fixture_json<Q, T>(fixture_name: &str, query: &Q) -> T
where
    Q: ytmapi_rs::query::Query<BrowserToken, Output = T>,
    T: ParseFrom<Q>,
{
    let raw = read_fixture(fixture_name);
    ytmapi_rs::process_json::<Q, BrowserToken>(raw, query)
        .unwrap_or_else(|e| panic!("failed to parse {fixture_name}: {e}"))
}

fn parse_continuable_page1<Q, T>(fixture_name: &str, query: &Q) -> (T, Option<String>)
where
    T: ParseFromContinuable<Q>,
{
    let raw = read_fixture(fixture_name);
    let val: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let json: ytmapi_rs::json::Json = serde_json::from_value(val).unwrap();
    let processed = ProcessedResult {
        query,
        source: raw,
        json,
    };
    let (items, cont) = T::parse_from_continuable(processed).unwrap();
    (items, cont.map(|c| c.get_raw().to_string()))
}

fn parse_continuable_page2<Q, T>(fixture_name: &str, query: &Q) -> (T, Option<String>)
where
    T: ParseFromContinuable<Q>,
{
    let raw = read_fixture(fixture_name);
    let val: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let json: ytmapi_rs::json::Json = serde_json::from_value(val).unwrap();
    let mock_cont = GetContinuationsQuery::new_mock_unchecked(query);
    let processed = ProcessedResult {
        query: &mock_cont,
        source: raw,
        json,
    };
    let (items, cont) = T::parse_continuation(processed).unwrap();
    (items, cont.map(|c| c.get_raw().to_string()))
}

struct MockSession {
    tag: String,
}

impl CatalogSession for MockSession {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[derive(Default)]
struct MockBackendState {
    pending_identity: Option<Arc<(Notify, Notify)>>,
    pending_playlist_read: Option<Arc<(Notify, Notify)>>,
    pending_playlist_write: Option<Arc<(Notify, Notify)>>,
    forced_error: Option<QueryError>,
    catalog_identity: Option<String>,
    identity_error: Option<QueryError>,
    playlists_by_tag: std::collections::HashMap<
        String,
        (
            Vec<ytm_protocol::PlaylistSummary>,
            Vec<ytm_protocol::PlaylistSummary>,
        ),
    >,
    slow_delay_ms: u64,
    slow_started: Option<Arc<Notify>>,
    mutations: Vec<(String, String)>,
    ratings_by_session: std::collections::HashMap<(String, VideoId), SongRating>,
    playlist_requests: Vec<String>,
    discovery_pages: std::collections::HashMap<Option<String>, BackendDiscoveryPage>,
    typed_search_pages:
        std::collections::HashMap<Option<String>, BackendPage<ProviderTypedSearchItem>>,
    artist_subscriptions: std::collections::HashMap<(String, ArtistId), bool>,
    /// Replaces the first playlist page's playback context, which the recorded rows lack.
    first_page_playback: Option<ytm_protocol::CollectionPlayback>,
}

struct MockCatalogBackend {
    state: Mutex<MockBackendState>,
    calls: AtomicUsize,
}

impl MockCatalogBackend {
    fn new() -> Self {
        Self {
            state: Mutex::new(MockBackendState::default()),
            calls: AtomicUsize::new(0),
        }
    }

    fn set_identity(&self, label: Option<&str>) {
        let mut state = self.state.lock().unwrap();
        state.catalog_identity = label.map(str::to_string);
        state.identity_error = None;
    }

    fn set_identity_error(&self, err: QueryError) {
        self.state.lock().unwrap().identity_error = Some(err);
    }

    fn set_forced_error(&self, err: Option<QueryError>) {
        self.state.lock().unwrap().forced_error = err;
    }

    fn set_slow(&self, delay_ms: u64, started: Arc<Notify>) {
        let mut g = self.state.lock().unwrap();
        g.slow_delay_ms = delay_ms;
        g.slow_started = Some(started);
    }

    fn set_playlists_for_sapisid(
        &self,
        sapisid: &str,
        p1: Vec<ytm_protocol::PlaylistSummary>,
        p2: Vec<ytm_protocol::PlaylistSummary>,
    ) {
        self.state
            .lock()
            .unwrap()
            .playlists_by_tag
            .insert(sapisid.to_string(), (p1, p2));
    }

    fn mutations(&self) -> Vec<(String, String)> {
        self.state.lock().unwrap().mutations.clone()
    }

    fn maybe_err(&self) -> Result<(), QueryError> {
        if let Some(err) = self.state.lock().unwrap().forced_error.clone() {
            return Err(err);
        }
        Ok(())
    }

    fn session_tag(session: &Arc<dyn CatalogSession>) -> &str {
        session
            .as_any()
            .downcast_ref::<MockSession>()
            .map(|s| s.tag.as_str())
            .unwrap_or("default")
    }
}

impl CatalogBackend for MockCatalogBackend {
    fn authenticate<'a>(
        &'a self,
        cookie_header: &'a str,
    ) -> BoxFuture<'a, Result<Arc<dyn CatalogSession>, QueryError>> {
        Box::pin(async move {
            let sapisid = cookie_header
                .split_once("SAPISID=")
                .and_then(|(_, rest)| rest.split_once(';'))
                .map(|(v, _)| v.to_string())
                .ok_or_else(|| {
                    make_query_error(
                        QueryErrorCode::AuthRequired,
                        false,
                        None,
                        "SAPISID missing or unterminated",
                    )
                })?;
            Ok(Arc::new(MockSession { tag: sapisid }) as Arc<dyn CatalogSession>)
        })
    }

    fn fetch_datasync_id<'a>(
        &'a self,
        _session: &'a Arc<dyn CatalogSession>,
    ) -> BoxFuture<'a, Result<Option<String>, QueryError>> {
        Box::pin(async move {
            let hold = self.state.lock().unwrap().pending_identity.take();
            if let Some(hold) = hold {
                hold.0.notify_one();
                hold.1.notified().await;
            }
            let state = self.state.lock().unwrap();
            match &state.identity_error {
                Some(err) => Err(err.clone()),
                None => Ok(state.catalog_identity.clone()),
            }
        })
    }

    fn search_songs<'a>(
        &'a self,
        _session: &'a Arc<dyn CatalogSession>,
        _query: &'a str,
        continuation: Option<&'a str>,
    ) -> BoxFuture<'a, Result<BackendPage<Track>, QueryError>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let (delay_ms, started) = {
                let g = self.state.lock().unwrap();
                (g.slow_delay_ms, g.slow_started.clone())
            };
            if delay_ms > 0 {
                if let Some(n) = started {
                    n.notify_waiters();
                }
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }
            self.maybe_err()?;
            let q = SearchQuery::new_filtered("glass", SongsFilter);
            match continuation {
                None => {
                    let (songs, next): (Vec<SearchResultSong>, Option<String>) =
                        parse_continuable_page1("search_songs_page1.json", &q);
                    let items = songs
                        .into_iter()
                        .map(normalize_search_song)
                        .collect::<Result<Vec<_>, _>>()?;
                    Ok(BackendPage {
                        items,
                        next_continuation: next,
                    })
                }
                Some(_) => {
                    let (songs, next): (Vec<SearchResultSong>, Option<String>) =
                        parse_continuable_page2("search_songs_page2.json", &q);
                    let items = songs
                        .into_iter()
                        .map(normalize_search_song)
                        .collect::<Result<Vec<_>, _>>()?;
                    Ok(BackendPage {
                        items,
                        next_continuation: next,
                    })
                }
            }
        })
    }

    fn library_playlists<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        continuation: Option<&'a str>,
    ) -> BoxFuture<'a, Result<BackendPage<ytm_protocol::PlaylistSummary>, QueryError>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.maybe_err()?;
            let tag = Self::session_tag(session);
            if let Some((p1, p2)) = self
                .state
                .lock()
                .unwrap()
                .playlists_by_tag
                .get(tag)
                .cloned()
            {
                return match continuation {
                    None => Ok(BackendPage {
                        items: p1,
                        next_continuation: Some(format!("cont_{tag}_p2")),
                    }),
                    Some(_) => Ok(BackendPage {
                        items: p2,
                        next_continuation: None,
                    }),
                };
            }
            let file = if continuation.is_some() {
                "library_playlists_page2.json"
            } else {
                "library_playlists_page1.json"
            };
            parse_library_playlist_page(
                serde_json::from_str(&read_fixture(file)).unwrap(),
                continuation.is_some(),
            )
        })
    }

    fn playlist_tracks<'a>(
        &'a self,
        _session: &'a Arc<dyn CatalogSession>,
        playlist_id: &'a PlaylistId,
        continuation: Option<&'a str>,
    ) -> BoxFuture<'a, Result<BackendPlaylistPage, QueryError>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.maybe_err()?;
            let pause = self.state.lock().unwrap().pending_playlist_read.take();
            if let Some(gate) = pause {
                gate.0.notify_one();
                gate.1.notified().await;
            }
            self.state
                .lock()
                .unwrap()
                .playlist_requests
                .push(ensure_vl_browse_id(playlist_id.as_str()));
            let q = GetPlaylistTracksQuery::new(PlaylistID::from_raw(playlist_id.as_str()));
            match continuation {
                None => {
                    let dq =
                        GetPlaylistDetailsQuery::new(PlaylistID::from_raw(playlist_id.as_str()));
                    let details: GetPlaylistDetails =
                        parse_fixture_json("playlist_tracks_page1.json", &dq);
                    let (title, description, author, cover) = normalize_playlist_metadata(details);
                    let json =
                        serde_json::from_str(&read_fixture("playlist_tracks_page1.json")).unwrap();
                    let (tracks, next_continuation, parsed) =
                        parse_playlist_tracks_page(&q, json, false)?;
                    let playback = self
                        .state
                        .lock()
                        .unwrap()
                        .first_page_playback
                        .clone()
                        .or(parsed);
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
                Some(_) => {
                    let json =
                        serde_json::from_str(&read_fixture("playlist_tracks_page2.json")).unwrap();
                    let (tracks, next_continuation, playback) =
                        parse_playlist_tracks_page(&q, json, true)?;
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
        _session: &'a Arc<dyn CatalogSession>,
        album_id: &'a AlbumId,
    ) -> BoxFuture<'a, Result<AlbumDetail, QueryError>> {
        Box::pin(async move {
            self.maybe_err()?;
            let q = GetAlbumQuery::new(AlbumID::from_raw(album_id.as_str()));
            let album: GetAlbum = parse_fixture_json("album_detail.json", &q);
            normalize_album_detail(album_id, album)
        })
    }

    fn artist_detail<'a>(
        &'a self,
        _session: &'a Arc<dyn CatalogSession>,
        artist_id: &'a ArtistId,
    ) -> BoxFuture<'a, Result<ArtistDetail, QueryError>> {
        Box::pin(async move {
            self.maybe_err()?;
            let q = GetArtistQuery::new(ArtistChannelID::from_raw(artist_id.as_str()));
            let artist: GetArtist = parse_fixture_json("artist_detail.json", &q);
            normalize_artist_detail(artist_id, artist)
        })
    }

    fn search_overview<'a>(
        &'a self,
        _session: &'a Arc<dyn CatalogSession>,
        _query: &'a str,
    ) -> BoxFuture<'a, Result<SearchOverview, QueryError>> {
        Box::pin(async move {
            self.maybe_err()?;
            Ok(SearchOverview {
                albums: vec![ytm_protocol::AlbumRef {
                    id: Some(AlbumId::new("MPREb_probe").unwrap()),
                    browse_id: None,
                    title: "Probe Album".to_string(),
                }],
                artists: Vec::new(),
                playlists: Vec::new(),
            })
        })
    }

    fn recent_history<'a>(
        &'a self,
        _session: &'a Arc<dyn CatalogSession>,
    ) -> BoxFuture<'a, Result<Vec<Track>, QueryError>> {
        Box::pin(async move {
            self.maybe_err()?;
            let periods: Vec<HistoryPeriod> =
                parse_fixture_json("recent_history.json", &GetHistoryQuery);
            normalize_history_periods(periods)
        })
    }

    fn official_lyrics<'a>(
        &'a self,
        _session: &'a Arc<dyn CatalogSession>,
        _video_id: &'a VideoId,
    ) -> BoxFuture<'a, Result<Option<ytm_core::ports::OfficialLyrics>, QueryError>> {
        Box::pin(async { Ok(None) })
    }

    fn set_like<'a>(
        &'a self,
        _session: &'a Arc<dyn CatalogSession>,
        video_id: &'a VideoId,
        liked: bool,
    ) -> BoxFuture<'a, Result<(), QueryError>> {
        Box::pin(async move {
            self.maybe_err()?;
            self.state.lock().unwrap().mutations.push((
                "setLike".to_string(),
                format!("{}:{liked}", video_id.as_str()),
            ));
            Ok(())
        })
    }

    fn add_to_playlist<'a>(
        &'a self,
        _session: &'a Arc<dyn CatalogSession>,
        playlist_id: &'a PlaylistId,
        video_id: &'a VideoId,
    ) -> BoxFuture<'a, Result<Option<String>, QueryError>> {
        Box::pin(async move {
            self.maybe_err()?;
            let mut state = self.state.lock().unwrap();
            state.mutations.push((
                "addToPlaylist".to_string(),
                format!("{}:{}", playlist_id.as_str(), video_id.as_str()),
            ));
            Ok(Some(format!("created-{}", state.mutations.len())))
        })
    }

    fn fresh_playlist<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        playlist_id: &'a PlaylistId,
        continuation: Option<&'a str>,
    ) -> BoxFuture<'a, Result<BackendFreshPlaylist, QueryError>> {
        Box::pin(async move {
            let page = self
                .playlist_tracks(session, playlist_id, continuation)
                .await?;
            let source_entries = page.tracks.len();
            Ok(BackendFreshPlaylist {
                metadata: None,
                entries: page
                    .tracks
                    .into_iter()
                    .enumerate()
                    .map(|(position, track)| PlaylistOccurrence {
                        entry_id: None,
                        position: u32::try_from(position).unwrap(),
                        playback_index: None,
                        track,
                        rating: SongRating::Unknown,
                    })
                    .collect(),
                next_continuation: page.next_continuation,
                source_entries,
            })
        })
    }

    fn write_playlist<'a>(
        &'a self,
        _session: &'a Arc<dyn CatalogSession>,
        request: PlaylistMutation<'a>,
    ) -> BoxFuture<'a, Result<PlaylistMutationAck, CatalogMutationError>> {
        Box::pin(async move {
            let PlaylistMutation::CreatePrivate { title, .. } = request else {
                return Err(CatalogMutationError::InvalidInput);
            };
            let wait = {
                let mut state = self.state.lock().unwrap();
                state
                    .mutations
                    .push(("create".to_owned(), title.to_owned()));
                state.pending_playlist_write.take()
            };
            if let Some(gate) = wait {
                gate.0.notify_one();
                gate.1.notified().await;
            }
            Ok(PlaylistMutationAck::Created(
                PlaylistId::new("PL_synthetic_remote").unwrap(),
            ))
        })
    }

    fn read_rating<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        video: &'a VideoId,
    ) -> BoxFuture<'a, Result<SongRating, QueryError>> {
        Box::pin(async move {
            Ok(self
                .state
                .lock()
                .unwrap()
                .ratings_by_session
                .get(&(Self::session_tag(session).to_owned(), video.clone()))
                .copied()
                .unwrap_or(SongRating::Unknown))
        })
    }

    fn write_rating<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        video: &'a VideoId,
        rating: SongRating,
    ) -> BoxFuture<'a, Result<(), CatalogMutationError>> {
        Box::pin(async move {
            self.state.lock().unwrap().ratings_by_session.insert(
                (Self::session_tag(session).to_owned(), video.clone()),
                rating,
            );
            Ok(())
        })
    }

    fn discovery<'a>(
        &'a self,
        _session: &'a Arc<dyn CatalogSession>,
        _surface: DiscoverySurface,
        continuation: Option<&'a str>,
    ) -> BoxFuture<'a, Result<BackendDiscoveryPage, QueryError>> {
        Box::pin(async move {
            self.state
                .lock()
                .unwrap()
                .discovery_pages
                .get(&continuation.map(str::to_owned))
                .cloned()
                .ok_or_else(|| {
                    make_query_error(
                        QueryErrorCode::InvalidPayload,
                        false,
                        None,
                        "fixture continuation missing",
                    )
                })
        })
    }

    fn typed_search<'a>(
        &'a self,
        _session: &'a Arc<dyn CatalogSession>,
        _kind: TypedSearchKind,
        _query: &'a str,
        continuation: Option<&'a str>,
    ) -> BoxFuture<'a, Result<BackendPage<ProviderTypedSearchItem>, QueryError>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.state
                .lock()
                .unwrap()
                .typed_search_pages
                .get(&continuation.map(str::to_owned))
                .cloned()
                .ok_or_else(|| {
                    make_query_error(
                        QueryErrorCode::InvalidPayload,
                        false,
                        None,
                        "fixture continuation missing",
                    )
                })
        })
    }

    fn extended_media<'a>(
        &'a self,
        _session: &'a Arc<dyn CatalogSession>,
        query: ExtendedMediaQuery<'a>,
        continuation: Option<&'a str>,
    ) -> BoxFuture<'a, Result<BackendExtendedPage, QueryError>> {
        Box::pin(async move {
            let ExtendedMediaQuery::Podcast(parent) = query else {
                panic!("unexpected provider route")
            };
            let next = continuation.is_none();
            let item = |id: &str| ProviderExtendedItem {
                kind: ExtendedMediaKind::PodcastEpisode,
                id: id.into(),
                title: id.into(),
                subtitle: None,
                description: Some("x".repeat(25_000)),
                video_id: None,
                related_id: Some(parent.clone()),
                duration: None,
                published: None,
            };
            let items = if next {
                vec![item("MPEDfixture-one"), item("MPEDfixture-two")]
            } else {
                assert_eq!(continuation, Some("episode-next"));
                vec![item("MPEDfixture-three")]
            };
            Ok(BackendExtendedPage {
                entity: None,
                items,
                next_continuation: next.then(|| "episode-next".into()),
                fully_projected: true,
            })
        })
    }

    fn library_page<'a>(
        &'a self,
        _session: &'a Arc<dyn CatalogSession>,
        surface: LibrarySurface,
        continuation: Option<&'a str>,
    ) -> BoxFuture<'a, Result<BackendPage<ProviderLibraryItem>, QueryError>> {
        Box::pin(async move {
            assert_eq!(surface, LibrarySurface::Albums);
            let (title, next) = match continuation {
                None => ("First", Some("fixture-library-next".into())),
                Some("fixture-library-next") => ("Second", None),
                _ => panic!("unexpected continuation"),
            };
            Ok(BackendPage {
                items: vec![ProviderLibraryItem::Album {
                    album_id: AlbumId::new(format!("fixture-album-{title}")).unwrap(),
                    title: title.into(),
                    artists: vec![],
                }],
                next_continuation: next,
            })
        })
    }

    fn artist_subscription<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        artist: &'a ArtistId,
    ) -> BoxFuture<'a, Result<Option<bool>, QueryError>> {
        Box::pin(async move {
            Ok(self
                .state
                .lock()
                .unwrap()
                .artist_subscriptions
                .get(&(Self::session_tag(session).to_owned(), artist.clone()))
                .copied())
        })
    }

    fn write_artist_subscription<'a>(
        &'a self,
        session: &'a Arc<dyn CatalogSession>,
        artist: &'a ArtistId,
        subscribed: bool,
    ) -> BoxFuture<'a, Result<(), CatalogMutationError>> {
        Box::pin(async move {
            self.state.lock().unwrap().artist_subscriptions.insert(
                (Self::session_tag(session).to_owned(), artist.clone()),
                subscribed,
            );
            Ok(())
        })
    }
}

#[tokio::test]
async fn library_pages_and_artist_readback_stay_on_the_authenticated_account() {
    let clock = Arc::new(MockClock::new(1_000));
    let (_dir, storage) = make_storage(clock.clone());
    let backend = Arc::new(MockCatalogBackend::new());
    backend.set_identity(Some("Fixture account A"));
    let adapter = YtmCatalogAdapter::with_backend(
        storage,
        backend.clone(),
        YtmCatalogConfig::default(),
        clock,
    );
    let cancel = CancellationToken::new();
    let material = make_session_material("epoch-library-a", "rev-a", "synthetic_a", None);
    let epoch_a = material.account_epoch.clone();
    let scope_a = adapter
        .apply_session_material(material, cancel.clone())
        .await
        .unwrap()
        .account_scope;
    let albums = LibrarySurface::Albums;
    let first = adapter
        .library_page(
            &epoch_a,
            &scope_a,
            albums,
            MusicPageRequest {
                cursor: None,
                limit: 1,
            },
            &cancel,
        )
        .await
        .unwrap();
    let next = MusicPageRequest {
        cursor: first.next_cursor.as_deref(),
        limit: 1,
    };
    assert!(
        adapter
            .library_page(&epoch_a, &scope_a, LibrarySurface::Artists, next, &cancel)
            .await
            .is_err()
    );
    let second = adapter
        .library_page(&epoch_a, &scope_a, albums, next, &cancel)
        .await
        .unwrap();
    assert!(
        matches!(&second.items[0], ProviderLibraryItem::Album {title, ..} if title == "Second")
    );
    assert!(second.complete);
    let artist = ArtistId::new("UCfixture").unwrap();
    for subscribed in [true, false] {
        adapter
            .set_artist_subscription_once(&epoch_a, &scope_a, &artist, subscribed, &cancel)
            .await
            .unwrap();
        assert_eq!(
            adapter
                .fresh_artist_subscription(&epoch_a, &scope_a, &artist, &cancel)
                .await
                .unwrap()
                .subscribed,
            Some(subscribed)
        );
    }

    backend.set_identity(Some("Fixture account B"));
    let other = make_session_material("epoch-library-b", "rev-b", "synthetic_b", None);
    let epoch_b = other.account_epoch.clone();
    let scope_b = adapter
        .apply_session_material(other, cancel.clone())
        .await
        .unwrap()
        .account_scope;
    assert!(
        adapter
            .library_page(&epoch_b, &scope_b, albums, next, &cancel)
            .await
            .is_err()
    );
    assert_eq!(
        adapter
            .fresh_artist_subscription(&epoch_b, &scope_b, &artist, &cancel)
            .await
            .unwrap()
            .subscribed,
        None
    );
}

#[tokio::test]
async fn extended_media_pages_honor_byte_budget_and_bind_account_epoch_and_entity() {
    let clock = Arc::new(MockClock::new(1_000));
    let (_dir, storage) = make_storage(clock.clone());
    let backend = Arc::new(MockCatalogBackend::new());
    backend.set_identity(Some("Fixture authority"));
    let adapter =
        YtmCatalogAdapter::with_backend(storage, backend, YtmCatalogConfig::default(), clock);
    let cancel = CancellationToken::new();
    let material = make_session_material("epoch-media-a", "rev-a", "synthetic", None);
    let epoch = material.account_epoch.clone();
    let scope = adapter
        .apply_session_material(material, cancel.clone())
        .await
        .unwrap()
        .account_scope;
    let podcast = BrowseId::new("MPSPfixture").unwrap();
    let query = ExtendedMediaQuery::Podcast(&podcast);
    let first = adapter
        .extended_media(
            &epoch,
            &scope,
            query,
            MusicPageRequest {
                cursor: None,
                limit: 100,
            },
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(first.items[0].id, "MPEDfixture-one");
    assert_eq!(first.items.len(), 1);
    let page = MusicPageRequest {
        cursor: first.next_cursor.as_deref(),
        limit: 100,
    };
    let other = BrowseId::new("MPSPother").unwrap();
    assert!(
        adapter
            .extended_media(
                &epoch,
                &scope,
                ExtendedMediaQuery::Podcast(&other),
                page,
                &cancel
            )
            .await
            .is_err()
    );
    let second = adapter
        .extended_media(&epoch, &scope, query, page, &cancel)
        .await
        .unwrap();
    assert_eq!(second.items[0].id, "MPEDfixture-two");
    let third = adapter
        .extended_media(
            &epoch,
            &scope,
            query,
            MusicPageRequest {
                cursor: second.next_cursor.as_deref(),
                limit: 100,
            },
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(third.items[0].id, "MPEDfixture-three");
    assert!(third.complete);
    assert!(third.next_cursor.is_none());
    let new = make_session_material("epoch-media-b", "rev-b", "synthetic", None);
    let new_epoch = new.account_epoch.clone();
    assert_eq!(
        adapter
            .apply_session_material(new, cancel.clone())
            .await
            .unwrap()
            .account_scope,
        scope
    );
    assert!(
        adapter
            .extended_media(&new_epoch, &scope, query, page, &cancel)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn typed_search_keeps_provider_order_and_dedupes_across_scoped_pages() {
    let clock = Arc::new(MockClock::new(1_000));
    let (_dir, storage) = make_storage(clock.clone());
    let backend = Arc::new(MockCatalogBackend::new());
    backend.set_identity(Some("Synthetic artist channel"));
    let artist = |id: &str| ProviderTypedSearchItem::Artist {
        artist_id: ArtistId::new(id).unwrap(),
        name: id.into(),
        subscribers: None,
        cover: None,
    };
    backend.state.lock().unwrap().typed_search_pages.extend([
        (
            None,
            BackendPage {
                items: vec![artist("artist-a"), artist("artist-a"), artist("artist-b")],
                next_continuation: Some("provider-fixture-next".into()),
            },
        ),
        (
            Some("provider-fixture-next".into()),
            BackendPage {
                items: vec![artist("artist-b"), artist("artist-c"), artist("artist-a")],
                next_continuation: None,
            },
        ),
    ]);
    let adapter = YtmCatalogAdapter::with_backend(
        storage,
        backend.clone(),
        YtmCatalogConfig::default(),
        clock.clone(),
    );
    let cancel = CancellationToken::new();
    let material = make_session_material("ep-typed-search", "cred-a", "synthetic_a", None);
    let epoch = material.account_epoch.clone();
    let scope = adapter
        .apply_session_material(material, cancel.clone())
        .await
        .unwrap()
        .account_scope;
    let read = |kind, query, cursor| {
        adapter.typed_search(
            &epoch,
            &scope,
            kind,
            query,
            MusicPageRequest { cursor, limit: 1 },
            &cancel,
        )
    };
    let first = read(TypedSearchKind::Artists, "synthetic artist", None)
        .await
        .unwrap();
    assert_eq!(first.items, [artist("artist-a")]);
    assert!(!first.complete);
    let first_cursor = first.next_cursor.as_deref();
    let calls_before_invalid = backend.calls.load(Ordering::SeqCst);
    for (kind, query) in [
        (TypedSearchKind::Albums, "synthetic artist"),
        (TypedSearchKind::Artists, "synthetic different query"),
    ] {
        assert_eq!(
            read(kind, query, first_cursor).await.unwrap_err().code,
            QueryErrorCode::StaleContext
        );
    }
    assert_eq!(backend.calls.load(Ordering::SeqCst), calls_before_invalid);
    let second = read(TypedSearchKind::Artists, "synthetic artist", first_cursor)
        .await
        .unwrap();
    assert_eq!(second.items, [artist("artist-b")]);
    assert_eq!(second.captured_at_ms, first.captured_at_ms);
    assert_eq!(backend.calls.load(Ordering::SeqCst), calls_before_invalid);
    let third = read(
        TypedSearchKind::Artists,
        "synthetic artist",
        second.next_cursor.as_deref(),
    )
    .await
    .unwrap();
    assert_eq!(third.items, [artist("artist-c")]);
    assert!(third.complete);
    assert!(third.next_cursor.is_none());

    clock.advance(120_001);
    assert_eq!(
        read(TypedSearchKind::Artists, "synthetic artist", first_cursor)
            .await
            .unwrap_err()
            .code,
        QueryErrorCode::StaleContext
    );

    let fresh = read(TypedSearchKind::Artists, "synthetic artist", None)
        .await
        .unwrap();
    backend.set_identity(Some("Synthetic other channel"));
    let material = make_session_material("ep-typed-search-other", "cred-b", "synthetic_b", None);
    let other_epoch = material.account_epoch.clone();
    let other_scope = adapter
        .apply_session_material(material, cancel.clone())
        .await
        .unwrap()
        .account_scope;
    assert_ne!(other_scope, scope);
    assert_eq!(
        adapter
            .typed_search(
                &other_epoch,
                &other_scope,
                TypedSearchKind::Artists,
                "synthetic artist",
                MusicPageRequest {
                    cursor: fresh.next_cursor.as_deref(),
                    limit: 1
                },
                &cancel,
            )
            .await
            .unwrap_err()
            .code,
        QueryErrorCode::StaleContext
    );
}

#[tokio::test]
async fn discovery_windows_keep_the_observed_order_until_expiration() {
    let clock = Arc::new(MockClock::new(1_000));
    let (_dir, storage) = make_storage(clock.clone());
    let backend = Arc::new(MockCatalogBackend::new());
    backend.set_identity(Some("Probe Discovery Channel"));
    let item = |id: &str| ProviderDiscoveryItem {
        section: Some("Fixture picks".into()),
        title: id.into(),
        target: ProviderDiscoveryTarget::Track(VideoId::new(id).unwrap()),
    };
    {
        let mut state = backend.state.lock().unwrap();
        state.discovery_pages.insert(
            None,
            BackendDiscoveryPage {
                items: vec![item("song-a"), item("song-b")],
                next_continuation: Some("provider-next".into()),
                fully_projected: true,
            },
        );
        state.discovery_pages.insert(
            Some("provider-next".into()),
            BackendDiscoveryPage {
                items: vec![item("song-c")],
                next_continuation: None,
                fully_projected: true,
            },
        );
    }
    let adapter = YtmCatalogAdapter::with_backend(
        storage,
        backend.clone(),
        YtmCatalogConfig::default(),
        clock.clone(),
    );
    let cancel = CancellationToken::new();
    let material = make_session_material("ep-discovery", "cred-a", "synthetic_a", None);
    let epoch = material.account_epoch.clone();
    let scope = adapter
        .apply_session_material(material, cancel.clone())
        .await
        .unwrap()
        .account_scope;
    let read = |surface, cursor| {
        adapter.discover(
            &epoch,
            &scope,
            surface,
            MusicPageRequest { cursor, limit: 1 },
            &cancel,
        )
    };
    let home = DiscoverySurface::Home;
    let first = read(home, None).await.unwrap();
    assert_eq!(first.items, vec![item("song-a")]);
    assert!(!first.complete);
    let cursor = first.next_cursor.as_deref();
    assert!(read(DiscoverySurface::Explore, cursor).await.is_err());
    backend
        .state
        .lock()
        .unwrap()
        .discovery_pages
        .get_mut(&None)
        .unwrap()
        .items
        .reverse();
    let second = read(home, cursor).await.unwrap();
    assert_eq!(second.items, vec![item("song-b")]);
    assert_eq!(second.captured_at_ms, first.captured_at_ms);
    let third = read(home, second.next_cursor.as_deref()).await.unwrap();
    assert_eq!(third.items, vec![item("song-c")]);
    assert!(third.complete);
    assert!(third.next_cursor.is_none());

    {
        let mut state = backend.state.lock().unwrap();
        state.discovery_pages.remove(&Some("provider-next".into()));
    }
    clock.advance(120_001);
    for stale_cursor in [cursor, second.next_cursor.as_deref()] {
        let error = read(home, stale_cursor).await.unwrap_err();
        assert_eq!(error.code, QueryErrorCode::StaleContext);
        assert!(error.safe_message.contains("restart without a cursor"));
    }
    adapter.set_network_offline(true);
    assert_eq!(
        read(home, None).await.unwrap_err().code,
        QueryErrorCode::NetworkOffline
    );
}

#[tokio::test]
async fn direct_rating_readback_distinguishes_neutral_and_isolates_the_account() {
    let clock = Arc::new(MockClock::new(1_000));
    let (_dir, storage) = make_storage(clock.clone());
    let backend = Arc::new(MockCatalogBackend::new());
    backend.set_identity(Some("Probe Channel A"));
    let adapter = YtmCatalogAdapter::with_backend(
        storage,
        backend.clone(),
        YtmCatalogConfig::default(),
        clock,
    );
    let cancel = CancellationToken::new();
    let material = make_session_material("ep-rating-a", "cred-a", "synthetic_a", None);
    let epoch_a = material.account_epoch.clone();
    let scope_a = adapter
        .apply_session_material(material, cancel.clone())
        .await
        .unwrap()
        .account_scope;
    let track = VideoId::new("fixture-rating-a").unwrap();
    assert_eq!(
        adapter
            .fresh_rating(&epoch_a, &scope_a, &track, &cancel)
            .await
            .unwrap()
            .rating,
        SongRating::Unknown
    );
    for rating in [SongRating::Liked, SongRating::Disliked, SongRating::Neutral] {
        adapter
            .set_rating_once(&epoch_a, &scope_a, &track, rating, &cancel)
            .await
            .unwrap();
        assert_eq!(
            adapter
                .fresh_rating(&epoch_a, &scope_a, &track, &cancel)
                .await
                .unwrap()
                .rating,
            rating
        );
    }
    backend.set_identity(Some("Probe Channel B"));
    let material = make_session_material("ep-rating-b", "cred-b", "synthetic_b", None);
    let epoch_b = material.account_epoch.clone();
    let scope_b = adapter
        .apply_session_material(material, cancel.clone())
        .await
        .unwrap()
        .account_scope;
    assert_eq!(
        adapter
            .fresh_rating(&epoch_a, &scope_a, &track, &cancel)
            .await
            .unwrap_err()
            .code,
        QueryErrorCode::StaleContext
    );
    assert_eq!(
        adapter
            .fresh_rating(&epoch_b, &scope_b, &track, &cancel)
            .await
            .unwrap()
            .rating,
        SongRating::Unknown
    );
}

#[tokio::test]
async fn catalog_mutation_classifies_cancellation_at_the_dispatch_boundary() {
    let clock = Arc::new(MockClock::new(1_000));
    let (_dir, storage) = make_storage(clock.clone());
    let backend = Arc::new(MockCatalogBackend::new());
    backend.set_identity(Some("Probe Channel"));
    let adapter = YtmCatalogAdapter::with_backend(
        storage,
        backend.clone(),
        YtmCatalogConfig::default(),
        clock,
    );
    let material = make_session_material("ep-write", "cred-write", "synthetic", None);
    let epoch = material.account_epoch.clone();
    let identity = adapter
        .apply_session_material(material, CancellationToken::new())
        .await
        .unwrap();
    let scope = identity.account_scope;
    let request = || PlaylistMutation::CreatePrivate {
        title: "Synthetic DJ test",
        description: None,
    };
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        adapter.mutate_playlist_once(&epoch, &scope, request(), &cancel).await,
        Err(CatalogMutationError::Rejected(err)) if err.code == QueryErrorCode::StaleContext
    ));
    assert!(backend.mutations().is_empty());

    let gate = Arc::new((Notify::new(), Notify::new()));
    backend.state.lock().unwrap().pending_playlist_write = Some(gate.clone());
    let cancel = CancellationToken::new();
    let (result, ()) = tokio::join!(
        adapter.mutate_playlist_once(&epoch, &scope, request(), &cancel),
        async {
            gate.0.notified().await;
            cancel.cancel();
        },
    );
    assert!(matches!(
        result, Err(CatalogMutationError::Unknown(err)) if err.code == QueryErrorCode::StaleContext
    ));
    assert_eq!(backend.mutations().len(), 1);
}

#[tokio::test]
async fn playlist_invalidation_cannot_be_overwritten_by_an_older_inflight_page() {
    let clock = Arc::new(MockClock::new(1_000));
    let (_dir, storage) = make_storage(clock.clone());
    let backend = Arc::new(MockCatalogBackend::new());
    backend.set_identity(Some("Probe Channel"));
    let adapter = YtmCatalogAdapter::with_backend(
        storage,
        backend.clone(),
        YtmCatalogConfig::default(),
        clock,
    );
    let material = make_session_material("ep-invalidate", "cred-invalidate", "synthetic", None);
    let epoch = material.account_epoch.clone();
    let scope = adapter
        .apply_session_material(material, CancellationToken::new())
        .await
        .unwrap()
        .account_scope;
    let playlist = PlaylistId::new("PL_probe_01").unwrap();
    let page = || {
        adapter.query(
            Some(epoch.clone()),
            Some(scope.clone()),
            CatalogQueryRequest {
                kind: CatalogQueryKind::PlaylistTracks,
                query: None,
                playlist_id: Some(playlist.clone()),
                album_id: None,
                artist_id: None,
                cursor: None,
                limit: Some(10),
            },
            CancellationToken::new(),
        )
    };
    assert!(matches!(
        page().await.unwrap(),
        CatalogResponsePayload::PlaylistTracks(_)
    ));
    let gate = Arc::new((Notify::new(), Notify::new()));
    backend.state.lock().unwrap().pending_playlist_read = Some(gate.clone());
    let (old_page, ()) = tokio::join!(page(), async {
        gate.0.notified().await;
        adapter
            .invalidate_playlist_cache(&scope, &playlist, true)
            .await
            .unwrap();
        gate.1.notify_one();
    });
    // The read that raced the invalidation is still served, but it must not refill the cache:
    // an offline read afterwards finds nothing cached.
    assert!(matches!(
        old_page.unwrap(),
        CatalogResponsePayload::PlaylistTracks(_)
    ));
    adapter.set_network_offline(true);
    assert_eq!(
        page().await.unwrap_err().code,
        QueryErrorCode::NetworkOffline
    );
}

/// Clearing the cache fences off reads that were already in flight: their results are served but
/// never written back into the cleared cache.
#[tokio::test]
async fn a_cache_clear_cannot_be_refilled_by_an_older_inflight_page() {
    let clock = Arc::new(MockClock::new(1_000));
    let (_dir, storage) = make_storage(clock.clone());
    let backend = Arc::new(MockCatalogBackend::new());
    backend.set_identity(Some("Probe Channel"));
    let adapter = YtmCatalogAdapter::with_backend(
        storage.clone(),
        backend.clone(),
        YtmCatalogConfig::default(),
        clock,
    );
    let material = make_session_material("ep-clear", "cred-clear", "synthetic", None);
    let epoch = material.account_epoch.clone();
    let scope = adapter
        .apply_session_material(material, CancellationToken::new())
        .await
        .unwrap()
        .account_scope;
    let playlist = PlaylistId::new("PL_probe_01").unwrap();
    let page = || {
        adapter.query(
            Some(epoch.clone()),
            Some(scope.clone()),
            CatalogQueryRequest {
                kind: CatalogQueryKind::PlaylistTracks,
                query: None,
                playlist_id: Some(playlist.clone()),
                album_id: None,
                artist_id: None,
                cursor: None,
                limit: Some(10),
            },
            CancellationToken::new(),
        )
    };
    let gate = Arc::new((Notify::new(), Notify::new()));
    backend.state.lock().unwrap().pending_playlist_read = Some(gate.clone());
    let (old_page, ()) = tokio::join!(page(), async {
        gate.0.notified().await;
        adapter.fence_cache_writes().await;
        storage.clear_cache(Some(scope.clone())).await.unwrap();
        adapter.clear_memory_cache(Some(&scope)).unwrap();
        gate.1.notify_one();
    });
    assert!(matches!(
        old_page.unwrap(),
        CatalogResponsePayload::PlaylistTracks(_)
    ));
    adapter.set_network_offline(true);
    assert_eq!(
        page().await.unwrap_err().code,
        QueryErrorCode::NetworkOffline
    );
}

#[tokio::test]
async fn fresh_playlist_uses_bounded_account_scoped_pages_with_absolute_positions() {
    let clock = Arc::new(MockClock::new(1_000));
    let (_dir, storage) = make_storage(clock.clone());
    let backend = Arc::new(MockCatalogBackend::new());
    backend.set_identity(Some("Probe Channel"));
    let adapter =
        YtmCatalogAdapter::with_backend(storage, backend, YtmCatalogConfig::default(), clock);
    let material = make_session_material("ep-fresh", "cred-fresh", "synthetic", None);
    let epoch = material.account_epoch.clone();
    let cancel = CancellationToken::new();
    let identity = adapter
        .apply_session_material(material, cancel.clone())
        .await
        .unwrap();
    let scope = &identity.account_scope;
    let playlist = PlaylistId::new("PL_probe_01").unwrap();
    let mut cursor = None;
    for position in 0..3 {
        let page = adapter
            .fresh_playlist(
                &epoch,
                scope,
                &playlist,
                MusicPageRequest {
                    cursor: cursor.as_deref(),
                    limit: 1,
                },
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries[0].position, position);
        assert_eq!(page.complete, position == 2);
        if position == 0 {
            let other = PlaylistId::new("PL_other").unwrap();
            let err = adapter
                .fresh_playlist(
                    &epoch,
                    scope,
                    &other,
                    MusicPageRequest {
                        cursor: page.next_cursor.as_deref(),
                        limit: 1,
                    },
                    &cancel,
                )
                .await
                .unwrap_err();
            assert_eq!(err.code, QueryErrorCode::InvalidPayload);
        }
        cursor = page.next_cursor;
    }
    assert!(cursor.is_none());
    adapter.set_network_offline(true);
    let err = adapter
        .fresh_playlist(
            &epoch,
            scope,
            &playlist,
            MusicPageRequest {
                cursor: None,
                limit: 1,
            },
            &cancel,
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, QueryErrorCode::NetworkOffline);
}

#[tokio::test]
async fn offline_search_uses_an_exact_page_limit_and_cursor() {
    let clock = Arc::new(MockClock::new(1_000));
    let (_dir, storage) = make_storage(clock.clone());
    let backend = Arc::new(MockCatalogBackend::new());
    backend.set_identity(Some("Probe Channel"));
    let adapter =
        YtmCatalogAdapter::with_backend(storage, backend, YtmCatalogConfig::default(), clock);
    let material = make_session_material("ep-offline-page", "cred-offline", "synthetic", None);
    let epoch = material.account_epoch.clone();
    let scope = adapter
        .apply_session_material(material, CancellationToken::new())
        .await
        .unwrap()
        .account_scope;
    let request = |limit, cursor| {
        adapter.query(
            Some(epoch.clone()),
            Some(scope.clone()),
            CatalogQueryRequest {
                kind: CatalogQueryKind::SearchSongs,
                query: Some("glass".into()),
                limit: Some(limit),
                cursor,
                playlist_id: None,
                album_id: None,
                artist_id: None,
            },
            CancellationToken::new(),
        )
    };
    let songs = |response| match response {
        CatalogResponsePayload::SearchSongs(page) => page.songs,
        unexpected => panic!("unexpected payload: {unexpected:?}"),
    };
    assert_eq!(songs(request(100, None).await.unwrap()).items.len(), 2);
    adapter.set_network_offline(true);
    // A larger cached window cannot provide an exact continuation for the smaller budget.
    assert_eq!(
        request(1, None).await.unwrap_err().code,
        QueryErrorCode::NetworkOffline
    );
    adapter.set_network_offline(false);
    let first = songs(request(1, None).await.unwrap());
    let next = first
        .next_cursor
        .clone()
        .expect("second slice of same provider page");
    let second = songs(request(1, Some(next.clone())).await.unwrap());
    adapter.set_network_offline(true);
    let offline = songs(request(1, None).await.unwrap());
    assert_eq!(offline.freshness, Freshness::Stale);
    assert_eq!(offline.items.len(), 1);
    assert_eq!(offline.next_cursor, Some(next.clone()));
    let offline_second = songs(request(1, Some(next)).await.unwrap());
    assert_eq!(offline_second.items[0].video_id, second.items[0].video_id);
    assert_ne!(offline_second.items[0].video_id, offline.items[0].video_id);
}

#[tokio::test]
async fn two_page_continuation_pagination_without_duplicates_or_drops() {
    let clock = Arc::new(MockClock::new(1_000));
    let (_dir, storage) = make_storage(clock.clone());
    let backend = Arc::new(MockCatalogBackend::new());
    backend.set_identity(Some("Probe Channel"));

    let adapter =
        YtmCatalogAdapter::with_backend(storage, backend, YtmCatalogConfig::default(), clock);
    let material = make_session_material("ep-p1", "cred-1", "sapisid_p1", Some("Probe Channel"));
    let epoch = material.account_epoch.clone();
    let scope = adapter
        .apply_session_material(material, CancellationToken::new())
        .await
        .unwrap()
        .account_scope;

    // 1. SearchSongs 2 pages
    let p1 = match adapter
        .query(
            Some(epoch.clone()),
            Some(scope.clone()),
            CatalogQueryRequest {
                kind: CatalogQueryKind::SearchSongs,
                query: Some("glass".to_string()),
                playlist_id: None,
                album_id: None,
                artist_id: None,
                cursor: None,
                limit: Some(50),
            },
            CancellationToken::new(),
        )
        .await
        .unwrap()
    {
        CatalogResponsePayload::SearchSongs(res) => res,
        other => panic!("unexpected payload: {other:?}"),
    };
    assert_eq!(p1.songs.items.len(), 2);
    assert_eq!(p1.songs.freshness, Freshness::Fresh);
    let cursor = p1.songs.next_cursor.clone().expect("page 1 cursor");
    assert!(cursor.starts_with("ytmg1:"));

    // Cross-query cursor reuse must fail with InvalidPayload.
    let cross_query_err = adapter
        .query(
            Some(epoch.clone()),
            Some(scope.clone()),
            CatalogQueryRequest {
                kind: CatalogQueryKind::SearchSongs,
                query: Some("different query".to_string()),
                playlist_id: None,
                album_id: None,
                artist_id: None,
                cursor: Some(cursor.clone()),
                limit: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(cross_query_err.code, QueryErrorCode::InvalidPayload);

    let p2 = match adapter
        .query(
            Some(epoch.clone()),
            Some(scope.clone()),
            CatalogQueryRequest {
                kind: CatalogQueryKind::SearchSongs,
                query: Some("glass".to_string()),
                playlist_id: None,
                album_id: None,
                artist_id: None,
                cursor: Some(cursor),
                limit: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap()
    {
        CatalogResponsePayload::SearchSongs(res) => res,
        other => panic!("unexpected payload: {other:?}"),
    };
    assert_eq!(p2.songs.items.len(), 2);
    assert!(p2.songs.next_cursor.is_none());

    let search_ids: Vec<&str> = p1
        .songs
        .items
        .iter()
        .chain(p2.songs.items.iter())
        .map(|t| t.video_id.as_str())
        .collect();
    assert_eq!(
        search_ids,
        vec!["probeVid001", "probeVid002", "probeVid003", "probeVid004"]
    );
    assert_eq!(search_ids.iter().copied().collect::<HashSet<_>>().len(), 4);

    // 2. LibraryPlaylists 2 pages
    let lp1 = match adapter
        .query(
            Some(epoch.clone()),
            Some(scope.clone()),
            CatalogQueryRequest {
                kind: CatalogQueryKind::LibraryPlaylists,
                query: None,
                playlist_id: None,
                album_id: None,
                artist_id: None,
                cursor: None,
                limit: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap()
    {
        CatalogResponsePayload::LibraryPlaylists(p) => p,
        other => panic!("unexpected payload: {other:?}"),
    };
    assert_eq!(lp1.items.len(), 1);
    assert_eq!(lp1.items[0].playlist_id.as_str(), "PL_probe_01");
    assert_eq!(lp1.items[0].track_count, Some(4));

    let lp2 = match adapter
        .query(
            Some(epoch.clone()),
            Some(scope.clone()),
            CatalogQueryRequest {
                kind: CatalogQueryKind::LibraryPlaylists,
                query: None,
                playlist_id: None,
                album_id: None,
                artist_id: None,
                cursor: lp1.next_cursor,
                limit: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap()
    {
        CatalogResponsePayload::LibraryPlaylists(p) => p,
        other => panic!("unexpected payload: {other:?}"),
    };
    assert_eq!(lp2.items.len(), 1);
    assert_eq!(lp2.items[0].playlist_id.as_str(), "PL_probe_02");
    assert!(lp2.next_cursor.is_none());

    // 3. PlaylistTracks 2 pages without duplicates or drops
    let pt1 = match adapter
        .query(
            Some(epoch.clone()),
            Some(scope.clone()),
            CatalogQueryRequest {
                kind: CatalogQueryKind::PlaylistTracks,
                query: None,
                playlist_id: Some(PlaylistId::new("PL_probe_01").unwrap()),
                album_id: None,
                artist_id: None,
                cursor: None,
                limit: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap()
    {
        CatalogResponsePayload::PlaylistTracks(d) => d,
        other => panic!("unexpected payload: {other:?}"),
    };
    assert_eq!(pt1.title, "Synthetic Focus Mix");
    assert_eq!(pt1.tracks.items.len(), 2);

    let pt2 = match adapter
        .query(
            Some(epoch),
            Some(scope),
            CatalogQueryRequest {
                kind: CatalogQueryKind::PlaylistTracks,
                query: None,
                playlist_id: Some(PlaylistId::new("PL_probe_01").unwrap()),
                album_id: None,
                artist_id: None,
                cursor: pt1.tracks.next_cursor,
                limit: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap()
    {
        CatalogResponsePayload::PlaylistTracks(d) => d,
        other => panic!("unexpected payload: {other:?}"),
    };
    assert_eq!(pt2.title, "Synthetic Focus Mix");
    assert_eq!(pt2.tracks.items.len(), 1);
    assert!(pt2.tracks.next_cursor.is_none());

    let pt_ids: Vec<&str> = pt1
        .tracks
        .items
        .iter()
        .chain(pt2.tracks.items.iter())
        .map(|t| t.video_id.as_str())
        .collect();
    assert_eq!(pt_ids, vec!["probePlVid1", "probePlVid2", "probePlVid3"]);
}

fn search_request(cursor: Option<String>, limit: Option<u32>) -> CatalogQueryRequest {
    CatalogQueryRequest {
        kind: CatalogQueryKind::SearchSongs,
        query: Some("glass".to_string()),
        playlist_id: None,
        album_id: None,
        artist_id: None,
        cursor,
        limit,
    }
}

#[tokio::test]
async fn limit_below_upstream_page_size_pages_without_gaps_or_duplicates() {
    let clock = Arc::new(MockClock::new(1_000));
    let (_dir, storage) = make_storage(clock.clone());
    let backend = Arc::new(MockCatalogBackend::new());
    let adapter =
        YtmCatalogAdapter::with_backend(storage, backend, YtmCatalogConfig::default(), clock);
    let material = make_session_material("ep-limit", "cred-1", "sapisid_limit", None);
    let epoch = material.account_epoch.clone();
    let scope = adapter
        .apply_session_material(material, CancellationToken::new())
        .await
        .unwrap()
        .account_scope;
    let search = |cursor: Option<String>, limit: Option<u32>| {
        let adapter = &adapter;
        let (epoch, scope) = (epoch.clone(), scope.clone());
        async move {
            match adapter
                .query(
                    Some(epoch),
                    Some(scope),
                    search_request(cursor, limit),
                    CancellationToken::new(),
                )
                .await
                .unwrap()
            {
                CatalogResponsePayload::SearchSongs(res) => res,
                other => panic!("unexpected payload: {other:?}"),
            }
        }
    };

    // Reference: all songs of both upstream pages with no limit.
    let full_p1 = search(None, None).await;
    let full_p2 = search(full_p1.songs.next_cursor.clone(), None).await;
    let expected: Vec<String> = full_p1
        .songs
        .items
        .iter()
        .chain(&full_p2.songs.items)
        .map(|t| t.video_id.as_str().to_string())
        .collect();
    assert!(expected.len() >= 3, "fixture must span more than one limit");
    assert_eq!(
        full_p1.albums.len(),
        1,
        "overview accompanies the first page"
    );
    assert!(full_p2.albums.is_empty());

    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let page = search(cursor, Some(1)).await;
        assert!(page.songs.items.len() <= 1, "limit is honored");
        seen.extend(
            page.songs
                .items
                .iter()
                .map(|t| t.video_id.as_str().to_string()),
        );
        match page.songs.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
        assert!(seen.len() <= expected.len());
    }
    assert_eq!(seen, expected);
}

#[tokio::test]
async fn liked_pages_keep_each_row_with_its_native_entry_through_windows_and_cache() {
    let clock = Arc::new(MockClock::new(5_000));
    let (_dir, storage) = make_storage(clock.clone());
    let backend = Arc::new(MockCatalogBackend::new());
    backend.set_identity(Some("Probe Channel"));
    let entry = |index, id: &str| ytm_protocol::CollectionPlaybackEntry {
        start_index: index,
        playlist_entry_id: Some(id.to_string()),
    };
    // The fixture's two playable rows; the second plays from native index 2 (a greyed row
    // sits between them upstream).
    backend.state.lock().unwrap().first_page_playback = Some(ytm_protocol::CollectionPlayback {
        playlist_id: PlaylistId::new("LM").unwrap(),
        entries: vec![entry(0, "set-a"), entry(2, "set-b")],
    });
    let adapter = YtmCatalogAdapter::with_backend(
        storage,
        backend.clone(),
        YtmCatalogConfig::default(),
        clock,
    );
    let material =
        make_session_material("ep-liked", "cred-1", "sapisid_liked", Some("Probe Channel"));
    let epoch = material.account_epoch.clone();
    let scope = adapter
        .apply_session_material(material, CancellationToken::new())
        .await
        .unwrap()
        .account_scope;
    let read = |cursor: Option<String>| {
        adapter.query(
            Some(epoch.clone()),
            Some(scope.clone()),
            CatalogQueryRequest {
                kind: CatalogQueryKind::LikedSongs,
                query: None,
                playlist_id: None,
                album_id: None,
                artist_id: None,
                cursor,
                limit: Some(1),
            },
            CancellationToken::new(),
        )
    };
    let liked = |payload| match payload {
        CatalogResponsePayload::LikedSongs(page) => page,
        other => panic!("unexpected: {other:?}"),
    };

    let first = liked(read(None).await.unwrap());
    assert_eq!(first.page.items[0].video_id.as_str(), "probePlVid1");
    assert_eq!(
        first.playback.as_ref().unwrap().entries,
        [entry(0, "set-a")]
    );
    let second = liked(read(first.page.next_cursor.clone()).await.unwrap());
    assert_eq!(second.page.items[0].video_id.as_str(), "probePlVid2");
    let playback = second.playback.unwrap();
    assert_eq!(playback.playlist_id.as_str(), "LM");
    assert_eq!(playback.entries, [entry(2, "set-b")]);

    // The cached page keeps its context and is served stale when offline.
    adapter.set_network_offline(true);
    let cached = liked(read(None).await.unwrap());
    assert_eq!(cached.page.freshness, Freshness::Stale);
    assert_eq!(cached.playback, first.playback);
}

#[tokio::test]
async fn album_artist_liked_and_history_normalize_without_leaking_provider_types() {
    let clock = Arc::new(MockClock::new(5_000));
    let (_dir, storage) = make_storage(clock.clone());
    let backend = Arc::new(MockCatalogBackend::new());
    backend.set_identity(Some("Probe Channel"));

    let adapter = YtmCatalogAdapter::with_backend(
        storage,
        backend.clone(),
        YtmCatalogConfig::default(),
        clock,
    );
    let material =
        make_session_material("ep-norm", "cred-1", "sapisid_norm", Some("Probe Channel"));
    let epoch = material.account_epoch.clone();
    let scope = adapter
        .apply_session_material(material, CancellationToken::new())
        .await
        .unwrap()
        .account_scope;

    let album = match adapter
        .query(
            Some(epoch.clone()),
            Some(scope.clone()),
            CatalogQueryRequest {
                kind: CatalogQueryKind::AlbumDetail,
                query: None,
                playlist_id: None,
                album_id: Some(AlbumId::new("MPREb_probeAlb1").unwrap()),
                artist_id: None,
                cursor: None,
                limit: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap()
    {
        CatalogResponsePayload::AlbumDetail(a) => a,
        other => panic!("unexpected: {other:?}"),
    };
    assert_eq!(album.title, "Probe Album One");
    assert_eq!(album.year, Some(2025));
    assert_eq!(album.tracks.len(), 1);
    assert_eq!(album.tracks[0].video_id.as_str(), "probeAlbVid1");
    assert_eq!(album.tracks[0].duration_ms, Some(200_000));

    let artist = match adapter
        .query(
            Some(epoch.clone()),
            Some(scope.clone()),
            CatalogQueryRequest {
                kind: CatalogQueryKind::ArtistDetail,
                query: None,
                playlist_id: None,
                album_id: None,
                artist_id: Some(ArtistId::new("UC_probe_art1").unwrap()),
                cursor: None,
                limit: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap()
    {
        CatalogResponsePayload::ArtistDetail(a) => a,
        other => panic!("unexpected: {other:?}"),
    };
    assert_eq!(artist.name, "Probe Artist One");
    assert_eq!(artist.top_songs.len(), 1);
    assert_eq!(artist.albums.len(), 1);

    let liked = match adapter
        .query(
            Some(epoch.clone()),
            Some(scope.clone()),
            CatalogQueryRequest {
                kind: CatalogQueryKind::LikedSongs,
                query: None,
                playlist_id: None,
                album_id: None,
                artist_id: None,
                cursor: None,
                limit: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap()
    {
        CatalogResponsePayload::LikedSongs(p) => p,
        other => panic!("unexpected: {other:?}"),
    };
    // Liked songs are the "Liked music" auto playlist, browsed as VLLM (not library songs).
    assert_eq!(
        backend
            .state
            .lock()
            .unwrap()
            .playlist_requests
            .last()
            .map(String::as_str),
        Some("VLLM")
    );
    assert!(!liked.page.items.is_empty());
    assert!(liked.page.items.iter().all(|t| t.is_liked == Some(true)));
    // The recorded rows' play buttons name only a video, so no playback context is invented.
    assert!(liked.playback.is_none());

    let hist = match adapter
        .query(
            Some(epoch),
            Some(scope),
            CatalogQueryRequest {
                kind: CatalogQueryKind::RecentHistory,
                query: None,
                playlist_id: None,
                album_id: None,
                artist_id: None,
                cursor: None,
                limit: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap()
    {
        CatalogResponsePayload::RecentHistory(h) => h,
        other => panic!("unexpected: {other:?}"),
    };
    assert_eq!(hist.items.len(), 1);
    assert_eq!(hist.items[0].video_id.as_str(), "probeHist1");
    assert_eq!(hist.freshness, Freshness::Fresh);
}

#[tokio::test]
async fn two_account_scopes_zero_cache_bleed_and_stale_epoch_rejection() {
    let clock = Arc::new(MockClock::new(10_000));
    let (_dir, storage) = make_storage(clock.clone());
    let backend = Arc::new(MockCatalogBackend::new());
    backend.set_playlists_for_sapisid(
        "sapisid_acct1",
        vec![ytm_protocol::PlaylistSummary {
            playlist_id: PlaylistId::new("PL_acct1_private").unwrap(),
            browse_id: None,
            title: "Account 1 Private".to_string(),
            subtitle: None,
            track_count: Some(5),
            cover: None,
        }],
        vec![],
    );
    backend.set_playlists_for_sapisid(
        "sapisid_acct2",
        vec![ytm_protocol::PlaylistSummary {
            playlist_id: PlaylistId::new("PL_acct2_private").unwrap(),
            browse_id: None,
            title: "Account 2 Private".to_string(),
            subtitle: None,
            track_count: Some(9),
            cover: None,
        }],
        vec![],
    );

    let adapter = YtmCatalogAdapter::with_backend(
        storage,
        backend.clone(),
        YtmCatalogConfig::default(),
        clock,
    );

    // Account 1 login & query
    backend.set_identity(Some("User One"));
    let mat1 = make_session_material("ep-acct-1", "rev-1", "sapisid_acct1", Some("User One"));
    let ep1 = mat1.account_epoch.clone();
    let sc1 = adapter
        .apply_session_material(mat1.clone(), CancellationToken::new())
        .await
        .unwrap()
        .account_scope;

    let res1 = match adapter
        .query(
            Some(ep1.clone()),
            Some(sc1.clone()),
            CatalogQueryRequest {
                kind: CatalogQueryKind::LibraryPlaylists,
                query: None,
                playlist_id: None,
                album_id: None,
                artist_id: None,
                cursor: None,
                limit: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap()
    {
        CatalogResponsePayload::LibraryPlaylists(p) => p,
        other => panic!("unexpected: {other:?}"),
    };
    assert_eq!(res1.items[0].playlist_id.as_str(), "PL_acct1_private");
    let acct1_cursor = res1.next_cursor.clone().unwrap();

    // Switch to Account 2
    backend.set_identity(Some("User Two"));
    let mat2 = make_session_material("ep-acct-2", "rev-2", "sapisid_acct2", Some("User Two"));
    let ep2 = mat2.account_epoch.clone();
    let sc2 = adapter
        .apply_session_material(mat2, CancellationToken::new())
        .await
        .unwrap()
        .account_scope;

    // Account 1 epoch must now be rejected as StaleContext
    let stale_err = adapter
        .query(
            Some(ep1.clone()),
            Some(sc1.clone()),
            CatalogQueryRequest {
                kind: CatalogQueryKind::LibraryPlaylists,
                query: None,
                playlist_id: None,
                album_id: None,
                artist_id: None,
                cursor: None,
                limit: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(stale_err.code, QueryErrorCode::StaleContext);

    // Account 1 cursor under Account 2 session must be rejected as StaleContext
    let cursor_err = decode_cursor(
        &acct1_cursor,
        CursorKind::LibraryPlaylists,
        "",
        &sc2,
        "en-US",
    )
    .unwrap_err();
    assert_eq!(cursor_err.code, QueryErrorCode::StaleContext);

    // Account 2 sees only Account 2 playlists
    let res2 = match adapter
        .query(
            Some(ep2.clone()),
            Some(sc2.clone()),
            CatalogQueryRequest {
                kind: CatalogQueryKind::LibraryPlaylists,
                query: None,
                playlist_id: None,
                album_id: None,
                artist_id: None,
                cursor: None,
                limit: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap()
    {
        CatalogResponsePayload::LibraryPlaylists(p) => p,
        other => panic!("unexpected: {other:?}"),
    };
    assert_eq!(res2.items[0].playlist_id.as_str(), "PL_acct2_private");

    // Offline under Account 2 still sees ONLY Account 2 cache (zero bleed from Account 1)
    adapter.set_network_offline(true);
    let res2_off = match adapter
        .query(
            Some(ep2.clone()),
            Some(sc2.clone()),
            CatalogQueryRequest {
                kind: CatalogQueryKind::LibraryPlaylists,
                query: None,
                playlist_id: None,
                album_id: None,
                artist_id: None,
                cursor: None,
                limit: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap()
    {
        CatalogResponsePayload::LibraryPlaylists(p) => p,
        other => panic!("unexpected: {other:?}"),
    };
    assert_eq!(res2_off.freshness, Freshness::Stale);
    assert_eq!(res2_off.items.len(), 1);
    assert_eq!(res2_off.items[0].playlist_id.as_str(), "PL_acct2_private");

    // Attempting to re-apply invalidated ep1 is rejected with StaleContext
    let replay_err = adapter
        .apply_session_material(mat1, CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(replay_err.code, QueryErrorCode::StaleContext);
}

#[tokio::test]
async fn offline_fallback_reads_sqlite_storage_and_builds_home_fallback() {
    let clock = Arc::new(MockClock::new(20_000));
    let (_dir, storage) = make_storage(clock.clone());
    let backend = Arc::new(MockCatalogBackend::new());
    backend.set_identity(Some("Probe Channel"));

    let adapter = YtmCatalogAdapter::with_backend(
        storage.clone(),
        backend.clone(),
        YtmCatalogConfig::default(),
        clock.clone(),
    );
    let material = make_session_material("ep-off", "rev-1", "sapisid_off", Some("Probe Channel"));
    let epoch = material.account_epoch.clone();
    let scope = adapter
        .apply_session_material(material, CancellationToken::new())
        .await
        .unwrap()
        .account_scope;

    // Seed local recent playback in SQLite
    storage
        .record_recent_playback(
            &scope,
            &epoch,
            RecentPlaybackRecord {
                track: Track {
                    video_id: VideoId::new("recentLocal1").unwrap(),
                    title: "Local Recent Track".to_string(),
                    artists: Vec::new(),
                    album: None,
                    duration_ms: Some(180_000),
                    cover: None,
                    availability: TrackAvailability::Available,
                    version_tag: None,
                    is_liked: None,
                },
                playback_ref: None,
                position_ms: 45_000,
                duration_ms: Some(180_000),
                recorded_at_ms: 19_000,
                source_context: Some("probe".to_string()),
            },
        )
        .await
        .unwrap();

    // Warm online cache for playlists, playlist tracks, search, liked songs, and home
    for req in [
        CatalogQueryRequest {
            kind: CatalogQueryKind::LibraryPlaylists,
            query: None,
            playlist_id: None,
            album_id: None,
            artist_id: None,
            cursor: None,
            limit: None,
        },
        CatalogQueryRequest {
            kind: CatalogQueryKind::PlaylistTracks,
            query: None,
            playlist_id: Some(PlaylistId::new("PL_probe_01").unwrap()),
            album_id: None,
            artist_id: None,
            cursor: None,
            limit: None,
        },
        CatalogQueryRequest {
            kind: CatalogQueryKind::Home,
            query: None,
            playlist_id: None,
            album_id: None,
            artist_id: None,
            cursor: None,
            limit: Some(10),
        },
    ] {
        adapter
            .query(
                Some(epoch.clone()),
                Some(scope.clone()),
                req,
                CancellationToken::new(),
            )
            .await
            .unwrap();
    }

    let cached_home = storage
        .get_home_feed(&scope, &YtmCatalogConfig::default().locale)
        .await
        .unwrap()
        .expect("online Home should persist its provider fallback");
    assert!(!cached_home.value.recent_tracks.is_empty());
    assert!(
        cached_home
            .value
            .recent_tracks
            .iter()
            .all(|track| track.video_id.as_str() != "recentLocal1")
    );

    // Simulate network drop via backend error -> adapter must fall back to SQLite with Freshness::Stale
    backend.set_forced_error(Some(make_query_error(
        QueryErrorCode::NetworkOffline,
        true,
        Some(1_000),
        "simulated offline",
    )));

    let pl_off = match adapter
        .query(
            Some(epoch.clone()),
            Some(scope.clone()),
            CatalogQueryRequest {
                kind: CatalogQueryKind::LibraryPlaylists,
                query: None,
                playlist_id: None,
                album_id: None,
                artist_id: None,
                cursor: None,
                limit: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap()
    {
        CatalogResponsePayload::LibraryPlaylists(p) => p,
        other => panic!("unexpected: {other:?}"),
    };
    assert_eq!(pl_off.freshness, Freshness::Stale);
    assert_eq!(pl_off.items[0].playlist_id.as_str(), "PL_probe_01");

    let pt_off = match adapter
        .query(
            Some(epoch.clone()),
            Some(scope.clone()),
            CatalogQueryRequest {
                kind: CatalogQueryKind::PlaylistTracks,
                query: None,
                playlist_id: Some(PlaylistId::new("PL_probe_01").unwrap()),
                album_id: None,
                artist_id: None,
                cursor: None,
                limit: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap()
    {
        CatalogResponsePayload::PlaylistTracks(d) => d,
        other => panic!("unexpected: {other:?}"),
    };
    assert_eq!(pt_off.tracks.freshness, Freshness::Stale);
    assert_eq!(pt_off.tracks.items.len(), 2);

    let home_off = match adapter
        .query(
            Some(epoch),
            Some(scope),
            CatalogQueryRequest {
                kind: CatalogQueryKind::Home,
                query: None,
                playlist_id: None,
                album_id: None,
                artist_id: None,
                cursor: None,
                limit: Some(10),
            },
            CancellationToken::new(),
        )
        .await
        .unwrap()
    {
        CatalogResponsePayload::Home(h) => h,
        other => panic!("unexpected: {other:?}"),
    };
    assert!(!home_off.personalized_supported);
    assert_eq!(
        home_off.fallback_notice.as_deref(),
        Some(HOME_FALLBACK_NOTICE)
    );
    assert_eq!(home_off.freshness, Freshness::Stale);
    assert_eq!(home_off.recent_tracks[0].video_id.as_str(), "recentLocal1");
    assert_eq!(
        home_off.pinned_playlists[0].playlist_id.as_str(),
        "PL_probe_01"
    );
}

/// The host can miss a sign-out between two cookie reads and deliver another account's cookies
/// under the same accountEpoch. The session's own identity is read again, so that account is never
/// served or cached under the epoch's scope; the same account's refresh keeps its scope.
#[tokio::test]
async fn another_account_under_the_same_epoch_is_unbound_instead_of_served() {
    let clock = Arc::new(MockClock::new(1_000));
    let (_dir, storage) = make_storage(clock.clone());
    let backend = Arc::new(MockCatalogBackend::new());
    let adapter = YtmCatalogAdapter::with_backend(
        storage,
        backend.clone(),
        YtmCatalogConfig::default(),
        clock,
    );
    let account_a = "synthetic-account-a||";
    backend.set_identity(Some(account_a));
    let first = adapter
        .apply_session_material(
            make_session_material("epoch-shared", "rev-1", "sapisid_a", None),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(first.account_scope, account_scope_for(account_a));

    let refreshed = adapter
        .apply_session_material(
            make_session_material("epoch-shared", "rev-2", "sapisid_a", None),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(refreshed.account_scope, first.account_scope);

    backend.set_identity(Some("synthetic-account-b||"));
    let switched = adapter
        .apply_session_material(
            make_session_material("epoch-shared", "rev-3", "sapisid_b", None),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(switched.code, QueryErrorCode::AuthRequired);
    assert!(adapter.active_account_epoch().is_none());
}

#[tokio::test]
async fn session_scope_is_stable_per_account_and_mutation_outcomes_are_honest() {
    let clock = Arc::new(MockClock::new(1_000));
    let (_dir, storage) = make_storage(clock.clone());
    let backend = Arc::new(MockCatalogBackend::new());
    let adapter = YtmCatalogAdapter::with_backend(
        storage,
        backend.clone(),
        YtmCatalogConfig::default(),
        clock,
    );

    // Without a readable identity the session is unverified and scoped to its epoch.
    backend.set_identity(None);
    let mat = make_session_material("ep-unverified", "rev-1", "sapisid_a", None);
    let unverified = adapter
        .apply_session_material(mat.clone(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(unverified.account_hint, None);
    assert_eq!(
        unverified.account_scope,
        derive_account_scope(&mat.account_epoch)
    );

    // The same DATASYNC_ID gives the same scope for every new epoch (caches survive restarts).
    let datasync = "synthetic-datasync-id||";
    backend.set_identity(Some(datasync));
    let first = adapter
        .apply_session_material(
            make_session_material("ep-run-1", "rev-1", "sapisid_a", None),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let second = adapter
        .apply_session_material(
            make_session_material("ep-run-2", "rev-1", "sapisid_a", None),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(first.account_scope, account_scope_for(datasync));
    assert_eq!(second.account_scope, first.account_scope);
    assert_eq!(
        second.account_hint.as_deref(),
        Some(account_hint_for(datasync).as_str())
    );
    let dbg = format!("{adapter:?}");
    assert!(!dbg.contains(datasync), "raw DATASYNC_ID must not leak");

    let epoch = AccountEpoch::new("ep-run-2").unwrap();
    let scope = second.account_scope;
    let like = MutationRequest {
        kind: MutationKind::SetLike,
        video_id: VideoId::new("probeVid001").unwrap(),
        liked: Some(true),
        playlist_id: None,
        undo_token: None,
        confirmed_by_user: true,
    };
    let mutate = |err: Option<QueryError>| {
        backend.set_forced_error(err);
        adapter.mutate(
            epoch.clone(),
            scope.clone(),
            like.clone(),
            CancellationToken::new(),
        )
    };
    assert_eq!(
        mutate(None).await.unwrap().outcome,
        MutationOutcome::Confirmed
    );
    let timeout = make_query_error(QueryErrorCode::Timeout, true, None, "timed out");
    assert_eq!(
        mutate(Some(timeout)).await.unwrap().outcome,
        MutationOutcome::Indeterminate
    );
    let rate = make_query_error(QueryErrorCode::RateLimited, true, Some(10), "429");
    assert_eq!(
        mutate(Some(rate)).await.unwrap().outcome,
        MutationOutcome::Rejected
    );
    let expired = make_query_error(QueryErrorCode::AuthExpired, false, None, "401");
    assert_eq!(
        mutate(Some(expired)).await.unwrap_err().code,
        QueryErrorCode::AuthExpired
    );
    assert_eq!(backend.mutations().len(), 1, "no automatic retries");

    // The app's add names the one entry it created; a refused duplicate (the provider's
    // STATUS_FAILED) is still rejected and names none.
    let add = MutationRequest {
        kind: MutationKind::AddToPlaylist,
        liked: None,
        playlist_id: Some(PlaylistId::new("PL_fixture_target").unwrap()),
        ..like.clone()
    };
    let add_once = |err: Option<QueryError>| {
        backend.set_forced_error(err);
        adapter.add_to_playlist(
            epoch.clone(),
            scope.clone(),
            add.clone(),
            CancellationToken::new(),
        )
    };
    let added = add_once(None).await.unwrap();
    assert_eq!(added.result.outcome, MutationOutcome::Confirmed);
    assert_eq!(added.created_entry_id.as_deref(), Some("created-2"));
    let duplicate = make_query_error(QueryErrorCode::UnavailableTrack, false, None, "failed");
    let refused = add_once(Some(duplicate)).await.unwrap();
    assert_eq!(refused.result.outcome, MutationOutcome::Rejected);
    assert_eq!(refused.created_entry_id, None);
    backend.set_forced_error(None);

    // A failed identity read fails the apply without pinning the epoch to a fallback scope,
    // so the next material for that epoch still becomes verified.
    backend.set_identity_error(make_query_error(
        QueryErrorCode::NetworkOffline,
        true,
        None,
        "offline",
    ));
    let blip = make_session_material("ep-run-3", "rev-1", "sapisid_a", None);
    assert_eq!(
        adapter
            .apply_session_material(blip.clone(), CancellationToken::new())
            .await
            .unwrap_err()
            .code,
        QueryErrorCode::NetworkOffline
    );
    backend.set_identity(Some(datasync));
    let recovered = adapter
        .apply_session_material(blip, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(recovered.account_scope, scope);
    // A credential refresh whose identity cannot be read is not installed: the previous session
    // of the same account keeps serving, and the retriable error lets the host send it again.
    backend.set_identity_error(make_query_error(
        QueryErrorCode::Timeout,
        true,
        None,
        "timed out",
    ));
    let unverified_refresh = adapter
        .apply_session_material(
            make_session_material("ep-run-3", "rev-2", "sapisid_b", None),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(unverified_refresh.code, QueryErrorCode::Timeout);
    assert_eq!(
        adapter.active_account_epoch(),
        Some(AccountEpoch::new("ep-run-3").unwrap())
    );
    backend.set_identity(Some(datasync));
    let refreshed = adapter
        .apply_session_material(
            make_session_material("ep-run-3", "rev-2", "sapisid_b", None),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(refreshed, recovered);
}

#[tokio::test]
async fn error_classification_and_wiremock_http_boundaries() {
    assert_eq!(
        classify_upstream_status(401, "unauthorized").code,
        QueryErrorCode::AuthExpired
    );
    assert_eq!(
        classify_upstream_status(403, "forbidden").code,
        QueryErrorCode::AuthExpired
    );
    assert_eq!(
        classify_upstream_status(429, "too many requests").code,
        QueryErrorCode::RateLimited
    );
    assert_eq!(
        classify_upstream_status(503, "service unavailable").code,
        QueryErrorCode::NetworkOffline
    );
    assert_eq!(
        classify_upstream_status(504, "gateway timeout").code,
        QueryErrorCode::Timeout
    );
    assert_eq!(
        classify_upstream_status(404, "video unavailable").code,
        QueryErrorCode::UnavailableTrack
    );

    // Only explicit evidence expires a session; a page without ytcfg or an HTML body that
    // mentions "sign in" is retried or treated as a bad payload instead.
    use ytmapi_rs::error::ErrorKind;
    let ytcfg = classify_ytmapi_error(
        ErrorKind::UnableToParseYtCfg {
            ytcfg: String::new(),
        }
        .into(),
    );
    assert_eq!(ytcfg.code, QueryErrorCode::InvalidPayload);
    assert!(ytcfg.retriable);
    let html = classify_ytmapi_error(
        ErrorKind::InvalidResponse {
            response: "<html>Sign in to continue, error 4019</html>".to_string(),
        }
        .into(),
    );
    assert_ne!(html.code, QueryErrorCode::AuthExpired);
    let login = classify_ytmapi_error(
        ErrorKind::InvalidResponse {
            response: "reason LOGIN_REQUIRED".to_string(),
        }
        .into(),
    );
    assert_eq!(login.code, QueryErrorCode::AuthExpired);

    // Verify ytmapi-rs error mapping without raw JSON/response leakage
    let bad_json_err = ytmapi_rs::process_json::<_, BrowserToken>(
        r#"{"secret_cookie":"SAPISID=LEAKED_VALUE"}"#.to_string(),
        SearchQuery::new_filtered("q", SongsFilter),
    )
    .unwrap_err();
    let classified = classify_ytmapi_error(bad_json_err);
    assert_eq!(classified.code, QueryErrorCode::InvalidPayload);
    assert!(!classified.safe_message.contains("LEAKED_VALUE"));
    assert!(!classified.safe_message.contains("SAPISID"));

    // Wiremock HTTP boundary verification (429 rate-limit & 401 auth-expired)
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/youtubei/v1/search"))
        .respond_with(ResponseTemplate::new(429).set_body_string("rate limited"))
        .mount(&server)
        .await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/youtubei/v1/search", server.uri()))
        .send()
        .await
        .unwrap();
    let err = classify_upstream_status(u64::from(resp.status().as_u16()), "rate limited");
    assert_eq!(err.code, QueryErrorCode::RateLimited);
    assert!(err.retriable);
}

#[tokio::test]
async fn cookie_trailing_semicolon_edge_case_and_zero_secret_leakage() {
    let mut pairs = BTreeMap::new();
    pairs.insert("APISID".to_string(), "val_api".to_string());
    pairs.insert("HSID".to_string(), "val_hsid".to_string());
    pairs.insert("PREF".to_string(), "tz=UTC".to_string());
    // "SAPISID" sorts LAST alphabetically in this BTreeMap!
    pairs.insert(
        "SAPISID".to_string(),
        "super_secret_sapisid_value".to_string(),
    );

    let formatted = format_cookie_header(&pairs).unwrap();
    assert!(
        formatted.ends_with(';'),
        "formatted cookie header must end with trailing ';': {formatted}"
    );

    // Exact extraction logic used by ytmapi-rs 0.3.4 BrowserToken::from_str
    let extracted = formatted
        .split_once("SAPISID=")
        .and_then(|(_, rest)| rest.split_once(';'))
        .map(|(val, _)| val);
    assert_eq!(extracted, Some("super_secret_sapisid_value"));

    // Missing SAPISID rejected with AuthRequired
    let mut missing = pairs.clone();
    missing.remove("SAPISID");
    let err = format_cookie_header(&missing).unwrap_err();
    assert_eq!(err.code, QueryErrorCode::AuthRequired);

    // Zero secret leakage in Debug of payload, adapter, or errors
    let clock = Arc::new(MockClock::new(1_000));
    let (_dir, storage) = make_storage(clock.clone());
    let backend = Arc::new(MockCatalogBackend::new());
    let adapter =
        YtmCatalogAdapter::with_backend(storage, backend, YtmCatalogConfig::default(), clock);
    let payload = make_session_material("ep-sec", "rev-1", "super_secret_sapisid_value", None);
    let payload_dbg = format!("{payload:?}");
    assert!(!payload_dbg.contains("super_secret_sapisid_value"));
    assert!(!payload_dbg.contains("synthetic_hsid_01"));

    adapter
        .apply_session_material(payload, CancellationToken::new())
        .await
        .unwrap();
    let adapter_dbg = format!("{adapter:?}");
    assert!(!adapter_dbg.contains("super_secret_sapisid_value"));
    assert!(!adapter_dbg.contains("synthetic_hsid_01"));
    assert!(adapter_dbg.contains("<redacted>"));
}

#[tokio::test]
async fn logout_and_new_binding_supersede_pending_credentials_in_the_adapter() {
    for (logout, new_epoch) in [
        (false, "new-epoch"),
        (true, "new-epoch"),
        (false, "old-epoch"),
    ] {
        let clock = Arc::new(MockClock::new(1_000));
        let (_dir, storage) = make_storage(clock.clone());
        let backend = Arc::new(MockCatalogBackend::new());
        let adapter = Arc::new(YtmCatalogAdapter::with_backend(
            storage,
            backend.clone(),
            YtmCatalogConfig::default(),
            clock,
        ));
        let engine = ytm_core::CoreEngine::start(
            ytm_core::CoreDeps {
                catalog: Some(adapter.clone()),
                ..Default::default()
            },
            Default::default(),
        );
        let first = make_session_material("initial-epoch", "initial-rev", "synthetic-first", None);
        engine.on_session_material(first).await.unwrap();
        let hold = Arc::new((Notify::new(), Notify::new()));
        backend.state.lock().unwrap().pending_identity = Some(hold.clone());
        let pending = engine
            .enqueue_session_material(make_session_material(
                "old-epoch",
                "old-rev",
                "synthetic-old",
                None,
            ))
            .await
            .unwrap();
        hold.0.notified().await;
        if logout {
            // The normal host delivers both ui.logout and host.sessionCleared.
            engine.on_logout().await.unwrap();
            engine.on_logout().await.unwrap();
            assert!(adapter.active_account_epoch().is_none());
        }
        let new = make_session_material(new_epoch, "new-rev", "synthetic-new", None);
        let epoch = new.account_epoch.clone();
        engine.on_session_material(new).await.unwrap();
        hold.1.notify_one();
        assert_eq!(
            pending.await.unwrap_err().code,
            QueryErrorCode::StaleContext
        );
        assert_eq!(adapter.active_account_epoch(), Some(epoch.clone()));
        assert_eq!(
            engine.snapshot().await.unwrap().auth_state.account_epoch,
            Some(epoch)
        );
        engine.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn logout_removes_an_unpublished_sqlite_epoch_before_restart() {
    // Replay both sides of credential installation while the actor is still awaiting B.
    for (has_projected_account, installed) in
        [(false, false), (false, true), (true, false), (true, true)]
    {
        let clock = Arc::new(MockClock::new(1_000));
        let (_dir, storage) = make_storage(clock.clone());
        let backend = Arc::new(MockCatalogBackend::new());
        let adapter = Arc::new(YtmCatalogAdapter::with_backend(
            storage.clone(),
            backend.clone(),
            YtmCatalogConfig::default(),
            clock.clone(),
        ));
        let engine = ytm_core::CoreEngine::start(
            ytm_core::CoreDeps {
                storage: Some(storage.clone()),
                catalog: Some(adapter.clone()),
                ..Default::default()
            },
            Default::default(),
        );
        backend.set_identity(Some("Account A"));
        engine
            .on_session_material(make_session_material(
                "epoch-a",
                "rev-a",
                "synthetic-a",
                None,
            ))
            .await
            .unwrap();
        if !has_projected_account {
            engine.on_logout().await.unwrap();
        }

        backend.set_identity(Some("Account B"));
        let hold = Arc::new((Notify::new(), Notify::new()));
        backend.state.lock().unwrap().pending_identity = Some(hold.clone());
        let material = make_session_material("epoch-b", "rev-b", "synthetic-b", None);
        let epoch_b = material.account_epoch.clone();
        let pending = engine
            .enqueue_session_material(material.clone())
            .await
            .unwrap();
        hold.0.notified().await;
        let scope_b = account_scope_for("Account B");
        if installed {
            // The adapter has installed B but the core still has no binding receipt.
            adapter
                .apply_session_material(material, CancellationToken::new())
                .await
                .unwrap();
            assert_eq!(adapter.active_account_epoch(), Some(epoch_b.clone()));
        } else {
            // SQLite committed B, before the adapter acquired its session write lock.
            storage
                .register_account_epoch(&scope_b, &epoch_b)
                .await
                .unwrap();
        }
        let projected = engine.snapshot().await.unwrap().auth_state;
        assert_ne!(projected.account_scope, Some(scope_b.clone()));
        assert_eq!(projected.account_scope.is_some(), has_projected_account);
        engine.on_logout().await.unwrap();
        assert_eq!(
            pending.await.unwrap_err().code,
            QueryErrorCode::StaleContext
        );
        assert!(adapter.active_account_epoch().is_none());
        engine.shutdown().await.unwrap();
        let reopened =
            SqliteStorage::open(storage.paths().clone(), clock, CoverCacheConfig::default())
                .unwrap();
        assert!(reopened.last_session().await.unwrap().is_none());
        assert_eq!(
            reopened
                .register_account_epoch(&scope_b, &epoch_b)
                .await
                .unwrap_err()
                .code,
            QueryErrorCode::StaleContext
        );
    }
}

#[tokio::test]
async fn expiry_of_the_previous_epoch_does_not_abort_the_new_binding() {
    let clock = Arc::new(MockClock::new(1_000));
    let (_dir, storage) = make_storage(clock.clone());
    let backend = Arc::new(MockCatalogBackend::new());
    let adapter = Arc::new(YtmCatalogAdapter::with_backend(
        storage,
        backend.clone(),
        YtmCatalogConfig::default(),
        clock,
    ));
    let engine = ytm_core::CoreEngine::start(
        ytm_core::CoreDeps {
            catalog: Some(adapter.clone()),
            ..Default::default()
        },
        Default::default(),
    );
    let first = make_session_material("epoch-a", "rev-a", "synthetic-a", None);
    let previous = first.account_epoch.clone();
    engine.on_session_material(first).await.unwrap();
    let hold = Arc::new((Notify::new(), Notify::new()));
    backend.state.lock().unwrap().pending_identity = Some(hold.clone());
    let new_login = engine.clone();
    let new = make_session_material("epoch-b", "rev-b", "synthetic-b", None);
    let expected = new.account_epoch.clone();
    let pending = tokio::spawn(async move { new_login.on_session_material(new).await });
    hold.0.notified().await;
    engine.on_auth_expired(Some(previous), None).await.unwrap();
    assert_eq!(
        engine.snapshot().await.unwrap().auth_state.state,
        ytm_protocol::AuthState::Expired
    );
    hold.1.notify_one();
    pending.await.unwrap().unwrap();
    assert_eq!(adapter.active_account_epoch(), Some(expected.clone()));
    assert_eq!(
        engine.snapshot().await.unwrap().auth_state.account_epoch,
        Some(expected)
    );
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn stale_logout_preserves_new_session_and_history_is_evicted_on_clear() {
    let clock = Arc::new(MockClock::new(1_000));
    let (_dir, storage) = make_storage(clock.clone());
    let backend = Arc::new(MockCatalogBackend::new());
    let adapter = YtmCatalogAdapter::with_backend(
        storage,
        backend.clone(),
        YtmCatalogConfig::default(),
        clock,
    );
    let history = |epoch: Option<AccountEpoch>, scope: &AccountScope| {
        adapter.query(
            epoch,
            Some(scope.clone()),
            CatalogQueryRequest {
                kind: CatalogQueryKind::RecentHistory,
                query: None,
                playlist_id: None,
                album_id: None,
                artist_id: None,
                cursor: None,
                limit: None,
            },
            CancellationToken::new(),
        )
    };
    let first = make_session_material("epoch-a", "rev-a", "synthetic-a", None);
    let epoch_a = first.account_epoch.clone();
    let scope_a = adapter
        .apply_session_material(first, CancellationToken::new())
        .await
        .unwrap()
        .account_scope;
    assert!(
        matches!(history(Some(epoch_a.clone()), &scope_a).await.unwrap(), CatalogResponsePayload::RecentHistory(r) if !r.items.is_empty())
    );

    backend.set_identity(Some("User B"));
    let second = make_session_material("epoch-b", "rev-b", "synthetic-b", None);
    let epoch_b = second.account_epoch.clone();
    let scope_b = adapter
        .apply_session_material(second, CancellationToken::new())
        .await
        .unwrap()
        .account_scope;
    adapter.clear_session_material(Some(epoch_a)).unwrap();
    assert_eq!(adapter.active_account_epoch(), Some(epoch_b.clone()));
    history(Some(epoch_b.clone()), &scope_b).await.unwrap();
    adapter.clear_memory_cache(Some(&scope_a)).unwrap();
    assert!(
        matches!(history(None, &scope_b).await.unwrap(), CatalogResponsePayload::RecentHistory(r) if !r.items.is_empty())
    );
    adapter.clear_memory_cache(Some(&scope_b)).unwrap();
    assert_eq!(
        history(None, &scope_b).await.unwrap_err().code,
        QueryErrorCode::NetworkOffline
    );
    history(Some(epoch_b.clone()), &scope_b).await.unwrap();
    adapter.clear_session_material(Some(epoch_b)).unwrap();
    for scope in [scope_a, scope_b] {
        assert_eq!(
            history(None, &scope).await.unwrap_err().code,
            QueryErrorCode::AuthRequired
        );
    }
}

#[tokio::test]
async fn clear_session_material_drops_inflight_and_allows_same_epoch_recovery() {
    let clock = Arc::new(MockClock::new(1_000));
    let (_dir, storage) = make_storage(clock.clone());
    let backend = Arc::new(MockCatalogBackend::new());
    let started = Arc::new(Notify::new());
    backend.set_slow(500, started.clone());

    let adapter = Arc::new(YtmCatalogAdapter::with_backend(
        storage.clone(),
        backend,
        YtmCatalogConfig::default(),
        clock,
    ));
    let mat = make_session_material("ep-cancel", "rev-1", "sapisid_cancel", None);
    let epoch = mat.account_epoch.clone();
    let scope = adapter
        .apply_session_material(mat.clone(), CancellationToken::new())
        .await
        .unwrap()
        .account_scope;

    let adapter_cloned = adapter.clone();
    let ep_cloned = epoch.clone();
    let sc_cloned = scope.clone();
    let handle = tokio::spawn(async move {
        adapter_cloned
            .query(
                Some(ep_cloned),
                Some(sc_cloned),
                CatalogQueryRequest {
                    kind: CatalogQueryKind::SearchSongs,
                    query: Some("slow".to_string()),
                    playlist_id: None,
                    album_id: None,
                    artist_id: None,
                    cursor: None,
                    limit: None,
                },
                CancellationToken::new(),
            )
            .await
    });

    started.notified().await;
    adapter.clear_session_material(Some(epoch.clone())).unwrap();
    assert!(adapter.active_account_epoch().is_none());

    let inflight_err = handle.await.unwrap().unwrap_err();
    assert_eq!(inflight_err.code, QueryErrorCode::StaleContext);

    // Clearing only drops credentials (expiry); refreshed material for the same epoch recovers.
    // Logout invalidation is the storage's job (`StoragePort::clear_account_data`).
    adapter
        .apply_session_material(mat, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(adapter.active_account_epoch(), Some(epoch));
}

#[tokio::test]
async fn same_epoch_credential_rotation_preserves_inflight_catalog_queries() {
    let clock = Arc::new(MockClock::new(2_000));
    let (_dir, storage) = make_storage(clock.clone());
    let backend = Arc::new(MockCatalogBackend::new());
    let started = Arc::new(Notify::new());
    backend.set_slow(120, started.clone());

    let adapter = Arc::new(YtmCatalogAdapter::with_backend(
        storage.clone(),
        backend,
        YtmCatalogConfig::default(),
        clock,
    ));
    let mat1 = make_session_material("ep-rotate", "rev-1", "sapisid_rotate_1", None);
    let epoch = mat1.account_epoch.clone();
    let scope = adapter
        .apply_session_material(mat1, CancellationToken::new())
        .await
        .unwrap()
        .account_scope;

    // Start slow searchSongs on ep-rotate
    let adapter_q1 = adapter.clone();
    let ep1 = epoch.clone();
    let sc1 = scope.clone();
    let inflight_same = tokio::spawn(async move {
        adapter_q1
            .query(
                Some(ep1),
                Some(sc1),
                CatalogQueryRequest {
                    kind: CatalogQueryKind::SearchSongs,
                    query: Some("daft".to_string()),
                    playlist_id: None,
                    album_id: None,
                    artist_id: None,
                    cursor: None,
                    limit: None,
                },
                CancellationToken::new(),
            )
            .await
    });

    started.notified().await;
    // Rotate credential_revision within SAME account_epoch: in-flight query must still succeed.
    let mat2 = make_session_material("ep-rotate", "rev-2", "sapisid_rotate_2", None);
    adapter
        .apply_session_material(mat2, CancellationToken::new())
        .await
        .unwrap();

    let res = inflight_same.await.unwrap();
    assert!(
        res.is_ok(),
        "same-epoch credential rotation should preserve in-flight query: {res:?}"
    );

    // Switching to a DIFFERENT account_epoch must still cancel an in-flight query cleanly.
    let adapter_q2 = adapter.clone();
    let ep2 = epoch.clone();
    let sc2 = scope.clone();
    let inflight_diff = tokio::spawn(async move {
        adapter_q2
            .query(
                Some(ep2),
                Some(sc2),
                CatalogQueryRequest {
                    kind: CatalogQueryKind::SearchSongs,
                    query: Some("justice".to_string()),
                    playlist_id: None,
                    album_id: None,
                    artist_id: None,
                    cursor: None,
                    limit: None,
                },
                CancellationToken::new(),
            )
            .await
    });

    started.notified().await;
    let mat_other = make_session_material("ep-other", "rev-3", "sapisid_other", None);
    adapter
        .apply_session_material(mat_other, CancellationToken::new())
        .await
        .unwrap();

    let err = inflight_diff.await.unwrap().unwrap_err();
    assert_eq!(err.code, QueryErrorCode::StaleContext);
}

#[tokio::test]
#[ignore = "requires YTMCP_ALLOW_LIVE=1 and real YouTube Music session cookie"]
async fn live_catalog_probe() {
    if std::env::var("YTMCP_ALLOW_LIVE").as_deref() != Ok("1") {
        println!("NOT_RUN: YTMCP_ALLOW_LIVE != 1");
        return;
    }

    let cookie_raw = if let Ok(path) = std::env::var("YTMCP_LIVE_COOKIE_FILE") {
        std::fs::read_to_string(path).expect("read YTMCP_LIVE_COOKIE_FILE")
    } else {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .expect("read cookie header from stdin");
        buf
    };

    let mut pairs = BTreeMap::new();
    for part in cookie_raw.split(';') {
        if let Some((k, v)) = part.trim().split_once('=') {
            pairs.insert(k.trim().to_string(), v.trim().to_string());
        }
    }

    let clock = Arc::new(MockClock::new(1_000));
    let (_dir, storage) = make_storage(clock.clone());
    let adapter = YtmCatalogAdapter::new(storage).expect("init live adapter");
    let epoch = AccountEpoch::new("live-probe-epoch").unwrap();
    let material = SessionMaterialPayload {
        account_epoch: epoch.clone(),
        credential_revision: CredentialRevision::new("live-probe-rev").unwrap(),
        cookie_pairs: pairs,
        auth_user_index: Some(0),
        channel_label: None,
    };

    let identity = adapter
        .apply_session_material(material, CancellationToken::new())
        .await
        .expect("apply live session material");
    let scope = identity.account_scope.clone();

    let search_res = adapter
        .query(
            Some(epoch.clone()),
            Some(scope.clone()),
            CatalogQueryRequest {
                kind: CatalogQueryKind::SearchSongs,
                query: Some("Nujabes".to_string()),
                playlist_id: None,
                album_id: None,
                artist_id: None,
                cursor: None,
                limit: Some(20),
            },
            CancellationToken::new(),
        )
        .await
        .expect("live SearchSongs");

    let playlists_res = adapter
        .query(
            Some(epoch.clone()),
            Some(scope.clone()),
            CatalogQueryRequest {
                kind: CatalogQueryKind::LibraryPlaylists,
                query: None,
                playlist_id: None,
                album_id: None,
                artist_id: None,
                cursor: None,
                limit: None,
            },
            CancellationToken::new(),
        )
        .await
        .expect("live LibraryPlaylists");

    let first_pl = match &playlists_res {
        CatalogResponsePayload::LibraryPlaylists(p) => p.items.first().cloned(),
        _ => None,
    };
    let mut playlist_track_count = 0usize;
    if let Some(pl) = first_pl {
        let pt_res = adapter
            .query(
                Some(epoch),
                Some(scope),
                CatalogQueryRequest {
                    kind: CatalogQueryKind::PlaylistTracks,
                    query: None,
                    playlist_id: Some(pl.playlist_id),
                    album_id: None,
                    artist_id: None,
                    cursor: None,
                    limit: None,
                },
                CancellationToken::new(),
            )
            .await
            .expect("live PlaylistTracks");
        if let CatalogResponsePayload::PlaylistTracks(detail) = pt_res {
            playlist_track_count = detail.tracks.items.len();
        }
    }

    let search_count = match search_res {
        CatalogResponsePayload::SearchSongs(s) => s.songs.items.len(),
        _ => 0,
    };
    let playlist_count = match playlists_res {
        CatalogResponsePayload::LibraryPlaylists(p) => p.items.len(),
        _ => 0,
    };
    println!(
        "PASS_TARGET_PROBE provider={} search_songs={} library_playlists={} first_playlist_tracks={} identity_verified={}",
        crate::probe::PROVIDER_VERSION,
        search_count,
        playlist_count,
        playlist_track_count,
        identity.account_hint.is_some(),
    );
}

#[allow(dead_code)]
fn _assert_types_clean(_: &AtomicBool) {}

#[test]
fn official_lyrics_absence_is_only_the_endpointless_lyrics_tab() {
    let watch_next = |lyrics_tab: serde_json::Value| {
        serde_json::json!({"contents": {"singleColumnMusicWatchNextResultsRenderer": {
        "tabbedRenderer": {"watchNextTabbedResultsRenderer": {"tabs": [
            {"tabRenderer": {"title": "Up next"}},
            lyrics_tab,
        ]}}}}})
    };
    let with_lyrics = watch_next(serde_json::json!({"tabRenderer": {
        "endpoint": {"browseEndpoint": {"browseId": "MPLYt_synthetic"}}}}));
    assert_eq!(
        lyrics_browse_id(&with_lyrics).unwrap().as_deref(),
        Some("MPLYt_synthetic")
    );
    let without = watch_next(serde_json::json!({"tabRenderer": {"unselectable": true}}));
    assert_eq!(lyrics_browse_id(&without).unwrap(), None);
    // A changed response shape is an error, not "this song has no lyrics".
    let changed = serde_json::json!({"contents": {}});
    assert_eq!(
        lyrics_browse_id(&changed).unwrap_err().code,
        QueryErrorCode::InvalidPayload
    );
}

#[test]
fn exhausted_library_continuation_is_an_empty_last_page() {
    // Live shape after the last library playlist: the grid cursor answers with presentation
    // fields only. The provider parser alone reports it as an unparseable payload.
    let exhausted = serde_json::json!({
        "responseContext": { "responseId": "r" },
        "continuationContents": { "gridContinuation": { "itemSize": "COLLECTION_STYLE_ITEM_SIZE_MEDIUM" } }
    });
    let page = parse_library_playlist_page(exhausted, true).unwrap();
    assert!(page.items.is_empty());
    assert_eq!(page.next_continuation, None);

    // A continuation that carries no recognizable contents but an error stays an error.
    let broken =
        serde_json::json!({ "continuationContents": { "gridContinuation": { "items": "oops" } } });
    assert_eq!(
        parse_library_playlist_page(broken, true).unwrap_err().code,
        QueryErrorCode::InvalidPayload
    );
}

#[test]
fn playlist_pages_keep_videos_with_unlinked_creator_text_and_preserve_the_cursor() {
    const INITIAL_ROWS: &str = concat!(
        "/contents/twoColumnBrowseResultsRenderer/secondaryContents/sectionListRenderer",
        "/contents/0/musicPlaylistShelfRenderer/contents"
    );
    const CONTINUATION_ROWS: &str =
        "/onResponseReceivedActions/0/appendContinuationItemsAction/continuationItems";
    const WATCH: &str = concat!(
        "/overlay/musicItemThumbnailOverlayRenderer/content/musicPlayButtonRenderer",
        "/playNavigationEndpoint/watchEndpoint"
    );
    const CREATOR: &str = "/flexColumns/1/musicResponsiveListItemFlexColumnRenderer/text/runs/0";

    let query = GetPlaylistTracksQuery::new(PlaylistID::from_raw("VLsynthetic"));
    for (file, pointer, continuation) in [
        ("playlist_tracks_page1.json", INITIAL_ROWS, false),
        ("playlist_tracks_page2.json", CONTINUATION_ROWS, true),
    ] {
        let mut page: Value = serde_json::from_str(&read_fixture(file)).unwrap();
        let rows = page.pointer_mut(pointer).unwrap().as_array_mut().unwrap();
        let mut video = rows[0].clone();
        let row = &mut video["musicResponsiveListItemRenderer"];
        let watch = row.pointer_mut(WATCH).unwrap();
        watch["videoId"] = json!("syntheticUnlinkedVideo");
        watch["watchEndpointMusicSupportedConfigs"]["watchEndpointMusicConfig"]["musicVideoType"] =
            json!("MUSIC_VIDEO_TYPE_UGC");
        row["fixedColumns"][0]["musicResponsiveListItemFixedColumnRenderer"]["text"] =
            json!({"runs": [{ "text": "4:01" }]});
        row["menu"]["menuRenderer"]["topLevelButtons"][0]["likeButtonRenderer"]["likeStatus"] =
            json!("LIKE");

        let mut linked_video = video.clone();
        linked_video["musicResponsiveListItemRenderer"]
            .pointer_mut(WATCH)
            .unwrap()["videoId"] = json!("syntheticLinkedVideo");
        *video["musicResponsiveListItemRenderer"]
            .pointer_mut(CREATOR)
            .unwrap() = json!({ "text": "Unlinked Creator" });

        *rows = vec![
            video,
            linked_video,
            json!({
                "continuationItemRenderer": {
                    "continuationEndpoint": {
                        "continuationCommand": { "token": "syntheticNextPage" }
                    }
                }
            }),
        ];
        let (tracks, next, _) =
            parse_playlist_tracks_page(&query, serde_json::from_value(page).unwrap(), continuation)
                .unwrap();

        assert_eq!(next.as_deref(), Some("syntheticNextPage"));
        let [unlinked, linked] = tracks.as_slice() else {
            panic!("expected both video rows in order");
        };
        assert_eq!(unlinked.video_id.as_str(), "syntheticUnlinkedVideo");
        assert_eq!(unlinked.artists.len(), 1);
        assert_eq!(unlinked.artists[0].name, "Unlinked Creator");
        assert_eq!(unlinked.artists[0].id, None);
        assert_eq!(unlinked.availability, TrackAvailability::Available);
        assert_eq!(unlinked.duration_ms, Some(241_000));
        assert_eq!(unlinked.is_liked, Some(true));
        assert_eq!(linked.video_id.as_str(), "syntheticLinkedVideo");
        assert_eq!(
            linked.artists[0].id.as_ref().map(ArtistId::as_str),
            Some("UC_probe_art1")
        );
    }
}

#[test]
fn library_pages_filter_system_playlist_ids_and_keep_matching_custom_titles() {
    let card = |id: &str, title: &str| {
        serde_json::json!({"musicTwoRowItemRenderer": {"title": {"runs": [{
            "text": title,
            "navigationEndpoint": {"browseEndpoint": {"browseId": id}}
        }]}}})
    };
    let cases = [
        (
            "library_playlists_page1.json",
            "/contents/singleColumnBrowseResultsRenderer/tabs/0/tabRenderer/content/sectionListRenderer/contents/0/gridRenderer/items",
            false,
            "VLLM",
            "PL_probe_01",
        ),
        (
            "library_playlists_page2.json",
            "/continuationContents/gridContinuation/items",
            true,
            "LM",
            "PL_probe_02",
        ),
    ];
    for (file, path, continuation, liked_id, ordinary_id) in cases {
        let mut value: serde_json::Value = serde_json::from_str(&read_fixture(file)).unwrap();
        let rows = value.pointer_mut(path).unwrap().as_array_mut().unwrap();
        // System rows carry YouTube Music's Chinese-language titles: filtering must key on the id.
        rows.insert(usize::from(!continuation), card(liked_id, "已点赞的音乐"));
        rows.push(card("VLSE", "稍后收听的单集"));
        rows.push(card("VLPL_user_liked", "Liked music"));
        rows.push(card("VLPL_user_episodes", "Episodes for later"));

        let page = parse_library_playlist_page(value, continuation).unwrap();
        let ids: Vec<_> = page.items.iter().map(|p| p.playlist_id.as_str()).collect();
        assert_eq!(ids, [ordinary_id, "PL_user_liked", "PL_user_episodes"]);
        assert_eq!(page.next_continuation.is_some(), !continuation);
    }
}

#[test]
fn library_first_page_keeps_the_first_playlist_without_a_leading_action_tile() {
    let value: serde_json::Value =
        serde_json::from_str(&read_fixture("library_playlists_page1.json")).unwrap();
    let items_path = "/contents/singleColumnBrowseResultsRenderer/tabs/0/tabRenderer/content/sectionListRenderer/contents/0/gridRenderer/items";
    for with_actions in [false, true] {
        let mut page = value.clone();
        let grid = page
            .pointer_mut(items_path)
            .unwrap()
            .as_array_mut()
            .unwrap();
        let create = grid.remove(0);
        if with_actions {
            grid.push(create);
            grid.push(serde_json::json!({"musicTwoRowItemRenderer":{"navigationEndpoint":{"createPlaylistEndpoint":{}}}}));
        }
        let page = parse_library_playlist_page(page, false).unwrap();
        assert_eq!(
            page.items
                .iter()
                .map(|p| p.playlist_id.as_str())
                .collect::<Vec<_>>(),
            ["PL_probe_01"]
        );
        assert_eq!(page.next_continuation.as_deref(), Some("probe_pl_cont_p2"));
    }
    for malformed in [
        serde_json::json!({}),
        serde_json::json!({"musicTwoRowItemRenderer": "broken"}),
        serde_json::json!({"musicTwoRowItemRenderer": {"navigationEndpoint": {"browseEndpoint": {}}}}),
    ] {
        let mut page = value.clone();
        page.pointer_mut(items_path)
            .unwrap()
            .as_array_mut()
            .unwrap()
            .push(malformed);
        assert_eq!(
            parse_library_playlist_page(page, false).unwrap_err().code,
            QueryErrorCode::InvalidPayload
        );
    }
}
