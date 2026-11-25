use sha2::{Digest, Sha256};
use ytm_control_api::{
    Album, AlbumPage, Artist, ArtistOverview, ControlError, ControlRequest, ControlResponse,
    ExtendedMediaInput, ExtendedMediaPage, ExtendedMediaTarget, HomeView, LibraryKind, LibraryPage,
    Page, PageInput, PlaylistSummary,
};
use ytm_protocol::{CatalogQueryKind, CatalogQueryRequest, CatalogResponsePayload};

use super::{
    ControlRead, ControlService, ReadGrant, default_page, invalid, map_query_error, page_size,
    stale, track_projection, unavailable,
};

impl ControlService {
    pub(super) async fn read_extended_media(
        &self,
        grant: &ReadGrant,
        state: &ControlRead,
        input: ExtendedMediaInput,
    ) -> Result<ExtendedMediaPage, ControlError> {
        let limit = page_size(input.page.as_ref())?;
        let (catalog, context) = self.catalog_context(state)?;
        let page = catalog
            .extended_media(
                context,
                input.target,
                input.page.unwrap_or_else(default_page),
            )
            .await
            .map_err(map_query_error)?;
        self.check_read_context(grant, state).await?;
        if page.entries.items.len() > limit {
            return Err(unavailable(
                "the provider exceeded the upload and podcast page size",
            ));
        }
        Ok(page)
    }

    pub(super) async fn read_catalog_view(
        &self,
        grant: &ReadGrant,
        state: &ControlRead,
        request: ControlRequest,
    ) -> Result<ControlResponse, ControlError> {
        let (mut query, requested_page) = match &request {
            ControlRequest::GetAlbum(args) => (
                query(
                    CatalogQueryKind::AlbumDetail,
                    None,
                    None,
                    Some(args.album_id.clone()),
                    None,
                ),
                args.page.clone(),
            ),
            ControlRequest::GetArtist(args) => (
                query(
                    CatalogQueryKind::ArtistDetail,
                    None,
                    None,
                    None,
                    Some(args.artist_id.clone()),
                ),
                Some(PageInput {
                    cursor: None,
                    limit: args.limit,
                }),
            ),
            ControlRequest::GetLibrary(args) => (
                query(
                    match args.kind {
                        LibraryKind::Playlists => CatalogQueryKind::LibraryPlaylists,
                        LibraryKind::Liked => CatalogQueryKind::LikedSongs,
                        LibraryKind::Songs
                        | LibraryKind::Albums
                        | LibraryKind::Artists
                        | LibraryKind::Subscriptions => {
                            let (catalog, context) = self.catalog_context(state)?;
                            page_size(args.page.as_ref())?;
                            let page = catalog
                                .account_library(
                                    context,
                                    args.kind,
                                    args.page.clone().unwrap_or_else(default_page),
                                )
                                .await
                                .map_err(map_query_error)?;
                            self.check_read_context(grant, state).await?;
                            return Ok(ControlResponse::Library(page));
                        }
                        LibraryKind::Uploads | LibraryKind::Podcasts => {
                            let uploads = args.kind == LibraryKind::Uploads;
                            let target = if uploads {
                                ExtendedMediaTarget::UploadSongs
                            } else {
                                ExtendedMediaTarget::LibraryPodcasts
                            };
                            let page = self
                                .read_extended_media(
                                    grant,
                                    state,
                                    ExtendedMediaInput {
                                        target,
                                        page: args.page.clone(),
                                    },
                                )
                                .await?;
                            return Ok(ControlResponse::Library(if uploads {
                                LibraryPage::Uploads(Box::new(page))
                            } else {
                                LibraryPage::Podcasts(Box::new(page))
                            }));
                        }
                    },
                    None,
                    None,
                    None,
                    None,
                ),
                args.page.clone(),
            ),
            ControlRequest::GetHistory(args) => {
                let page = args.page.clone().unwrap_or(PageInput {
                    cursor: None,
                    limit: args.limit,
                });
                if page.cursor.is_some() {
                    return Err(invalid("history continuation requires an explicit source"));
                }
                (
                    query(CatalogQueryKind::RecentHistory, None, None, None, None),
                    Some(page),
                )
            }
            ControlRequest::GetHome(args) => (
                query(CatalogQueryKind::Home, None, None, None, None),
                Some(PageInput {
                    cursor: None,
                    limit: args.limit,
                }),
            ),
            _ => return Err(invalid("expected a music catalog view")),
        };
        let limit = page_size(requested_page.as_ref())?;
        query.limit = Some(limit as u32);
        let cursor = requested_page.and_then(|page| page.cursor);
        if matches!(request, ControlRequest::GetLibrary(_)) {
            query.cursor = cursor.clone();
        }
        let raw = self
            .core
            .catalog_query(query, state.snapshot.account_epoch.clone())
            .await
            .map_err(map_query_error)?;
        self.check_read_context(grant, state).await?;
        let result = match raw {
            CatalogResponsePayload::AlbumDetail(album) => {
                let membership = if matches!(&request, ControlRequest::GetAlbum(args) if args.include_membership == Some(true))
                {
                    let (catalog, context) = self.catalog_context(state)?;
                    let (_, evidence) = catalog
                        .album_membership(context, album.album_id.clone())
                        .await
                        .map_err(map_query_error)?;
                    self.check_read_context(grant, state).await?;
                    Some(evidence)
                } else {
                    None
                };
                let mut digest = Sha256::new();
                digest.update(state.snapshot.core_epoch.as_str());
                digest.update(
                    state
                        .snapshot
                        .account_epoch
                        .as_ref()
                        .map_or("", |epoch| epoch.as_str()),
                );
                digest.update(album.album_id.as_str());
                for track in &album.tracks {
                    digest.update(track.video_id.as_str());
                    digest.update([0]);
                }
                let key = hex::encode(digest.finalize());
                let offset = cursor
                    .as_deref()
                    .map(|cursor| {
                        cursor
                            .strip_prefix(&key)
                            .and_then(|part| part.strip_prefix(':'))
                            .and_then(|part| part.parse::<usize>().ok())
                            .filter(|i| *i < album.tracks.len())
                            .ok_or_else(|| {
                                stale("the album cursor no longer matches the returned track list")
                            })
                    })
                    .transpose()?
                    .unwrap_or(0);
                let from = &album.tracks[offset..];
                let items: Vec<_> = from.iter().take(limit).map(track_projection).collect();
                let next = offset + items.len();
                ControlResponse::Album(AlbumPage {
                    album: Album {
                        id: Some(album.album_id),
                        title: album.title,
                    },
                    artists: album
                        .artists
                        .into_iter()
                        .map(|artist| Artist {
                            id: artist.id,
                            name: artist.name,
                        })
                        .collect(),
                    year: album.year,
                    tracks: items,
                    next_cursor: (next < album.tracks.len()).then(|| format!("{key}:{next}")),
                    complete: next == album.tracks.len(),
                    source: "youtube_music_catalog_or_cache".into(),
                    freshness: None,
                    membership,
                })
            }
            CatalogResponsePayload::ArtistDetail(artist) => {
                let subscription = if let Ok((catalog, context)) = self.catalog_context(state) {
                    let result = catalog
                        .artist_subscription(context, artist.artist_id.clone())
                        .await
                        .ok();
                    self.check_read_context(grant, state).await?;
                    result
                } else {
                    None
                };
                ControlResponse::Artist(ArtistOverview {
                    artist: Artist {
                        id: Some(artist.artist_id),
                        name: artist.name,
                    },
                    description: artist.description,
                    top_songs: artist
                        .top_songs
                        .iter()
                        .take(limit)
                        .map(track_projection)
                        .collect(),
                    albums: artist
                        .albums
                        .into_iter()
                        .take(limit)
                        .map(|album| Album {
                            id: album.id,
                            title: album.title,
                        })
                        .collect(),
                    source: "youtube_music_catalog_or_cache".into(),
                    freshness: None,
                    complete: false,
                    subscription,
                })
            }
            CatalogResponsePayload::LibraryPlaylists(playlists) => {
                let page = map_page(playlists, playlist_summary);
                if page.items.len() > limit {
                    return Err(unavailable(
                        "the catalog did not honor the library page budget",
                    ));
                }
                ControlResponse::Library(LibraryPage::Playlists(page))
            }
            CatalogResponsePayload::LikedSongs(liked) => {
                let page = map_page(liked.page, |track| track_projection(&track));
                if page.items.len() > limit {
                    return Err(unavailable(
                        "the catalog did not honor the liked music page budget",
                    ));
                }
                ControlResponse::Library(LibraryPage::Liked(page))
            }
            CatalogResponsePayload::RecentHistory(history) => ControlResponse::History(Page {
                items: history
                    .items
                    .iter()
                    .take(limit)
                    .map(track_projection)
                    .collect(),
                next_cursor: None,
                complete: false,
                source: "available_recent_history".into(),
                freshness: history.freshness,
                captured_at_ms: self.core.clock().wall_now_ms(),
            }),
            CatalogResponsePayload::Home(home) => ControlResponse::Home(HomeView {
                personalized: home.personalized_supported,
                freshness: home.freshness,
                source: if home.personalized_supported {
                    "youtube_music_home".into()
                } else {
                    "library_and_recent_history".into()
                },
                recent: home
                    .recent_tracks
                    .iter()
                    .take(limit)
                    .map(track_projection)
                    .collect(),
                playlists: home
                    .pinned_playlists
                    .into_iter()
                    .take(limit)
                    .map(playlist_summary)
                    .collect(),
            }),
            _ => return Err(unavailable("the catalog returned an unexpected result")),
        };
        Ok(result)
    }
}

fn query(
    kind: CatalogQueryKind,
    playlist_id: Option<ytm_protocol::PlaylistId>,
    cursor: Option<String>,
    album_id: Option<ytm_protocol::AlbumId>,
    artist_id: Option<ytm_protocol::ArtistId>,
) -> CatalogQueryRequest {
    CatalogQueryRequest {
        kind,
        query: None,
        playlist_id,
        album_id,
        artist_id,
        cursor,
        limit: None,
    }
}

fn map_page<T, U>(page: ytm_protocol::PagedResult<T>, map: impl Fn(T) -> U) -> Page<U> {
    Page {
        items: page.items.into_iter().map(map).collect(),
        complete: page.next_cursor.is_none(),
        next_cursor: page.next_cursor,
        source: "youtube_music_catalog".into(),
        freshness: page.freshness,
        captured_at_ms: page.fetched_at_ms,
    }
}

fn playlist_summary(playlist: ytm_protocol::PlaylistSummary) -> PlaylistSummary {
    PlaylistSummary {
        playlist_id: playlist.playlist_id,
        title: playlist.title,
        track_count: playlist.track_count,
    }
}
