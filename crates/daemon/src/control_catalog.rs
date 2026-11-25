use std::collections::HashSet;
use std::sync::Arc;

use sha2::{Digest, Sha256};
use ytm_control_api::{
    Album, Artist, ControlError, DiscoveryItem, DiscoveryKind, DiscoveryPage, DiscoveryTarget,
    ErrorCode, ExtendedMediaItem, ExtendedMediaKind, ExtendedMediaPage, ExtendedMediaTarget,
    HistoryEntry, HistoryMedia, LibraryAlbum, LibraryArtist, LibraryKind, LibraryPage,
    MembershipEvidence, Page, PageInput, Playlist, PlaylistEntry, PlaylistPrivacy, Rating,
    RecommendationPage, RetryDisposition,
};
use ytm_core::control::catalog::{
    AddedOccurrence, CatalogReadContext, CatalogWriteError, ControlCatalog, PlaylistEdit,
    PlaylistWrite, PlaylistWriteAck, PlaylistWriteError, RatingEvidence, project_track,
};
use ytm_core::ports::BoxFuture;
use ytm_data::YtmCatalogAdapter;
use ytm_data::probe::{
    CatalogMutationError, DiscoverySurface, ExtendedMediaKind as ProviderMediaKind,
    ExtendedMediaQuery, LibrarySurface, MOOD_PLAYLISTS_SOURCE, MusicPageRequest, PROVIDER_NAME,
    PlaylistAppendDuplicates, PlaylistMutation, PlaylistMutationAck,
    PlaylistPrivacy as ProviderPrivacy, PlaylistRemoval, ProviderDiscoveryPage,
    ProviderDiscoveryTarget, ProviderExtendedItem, ProviderLibraryItem, ProviderLibraryPage,
    SEARCH_SUGGESTIONS_SOURCE, SONG_RADIO_SOURCE, SongRating, make_query_error,
};
use ytm_protocol::{
    AccountScope, ArtistId, Freshness, PlaylistId, QueryError, QueryErrorCode, VideoId,
};

pub struct YtmControlCatalog {
    adapter: Arc<YtmCatalogAdapter>,
}

impl YtmControlCatalog {
    pub fn new(adapter: Arc<YtmCatalogAdapter>) -> Self {
        Self { adapter }
    }

    async fn library_page(
        &self,
        context: CatalogReadContext,
        surface: LibrarySurface,
        request: PageInput,
    ) -> Result<ProviderLibraryPage, QueryError> {
        self.adapter
            .library_page(
                &context.account_epoch,
                &context.account_scope,
                surface,
                page_request(&request),
                &context.cancel,
            )
            .await
    }
}

impl ControlCatalog for YtmControlCatalog {
    fn library_membership(
        &self,
        context: CatalogReadContext,
        target: ytm_control_api::LibraryMembershipTarget,
    ) -> BoxFuture<'_, Result<MembershipEvidence, QueryError>> {
        use ytm_control_api::LibraryMembershipTarget::*;
        match target {
            Song { id } => self.song_membership(context, id),
            Album { id } => Box::pin(async move {
                self.album_membership(context, id)
                    .await
                    .map(|(_, evidence)| evidence)
            }),
            Playlist { id } => Box::pin(async move {
                let evidence = self
                    .adapter
                    .fresh_playlist_membership(
                        &context.account_epoch,
                        &context.account_scope,
                        &id,
                        &context.cancel,
                    )
                    .await?;
                Ok(MembershipEvidence {
                    value: evidence.in_library,
                    observed_at_ms: evidence.observed_at_ms,
                })
            }),
        }
    }

    fn set_library_membership(
        &self,
        context: CatalogReadContext,
        target: ytm_control_api::LibraryMembershipTarget,
        saved: bool,
    ) -> BoxFuture<'_, Result<(), CatalogWriteError>> {
        use ytm_control_api::LibraryMembershipTarget::*;

        Box::pin(async move {
            let epoch = &context.account_epoch;
            let scope = &context.account_scope;
            let cancel = &context.cancel;
            match target {
                Song { id } => {
                    self.adapter
                        .set_song_membership_once(epoch, scope, &id, saved, cancel)
                        .await
                }
                Playlist { id } => {
                    self.adapter
                        .set_playlist_membership_once(epoch, scope, &id, saved, cancel)
                        .await
                }
                Album { .. } => Err(CatalogMutationError::InvalidInput),
            }
            .map_err(|error| map_catalog_write_error(error, ErrorCode::PermissionDenied))
        })
    }

    fn album_membership(
        &self,
        context: CatalogReadContext,
        album: ytm_protocol::AlbumId,
    ) -> BoxFuture<'_, Result<(PlaylistId, MembershipEvidence), QueryError>> {
        Box::pin(async move {
            let result = self
                .adapter
                .fresh_album_membership(
                    &context.account_epoch,
                    &context.account_scope,
                    &album,
                    &context.cancel,
                )
                .await?;
            Ok((
                result.playlist_id,
                MembershipEvidence {
                    value: result.in_library,
                    observed_at_ms: result.observed_at_ms,
                },
            ))
        })
    }

    fn set_album_membership(
        &self,
        context: CatalogReadContext,
        album: ytm_protocol::AlbumId,
        playlist: PlaylistId,
        saved: bool,
    ) -> BoxFuture<'_, Result<(), CatalogWriteError>> {
        Box::pin(async move {
            self.adapter
                .set_album_membership_once(
                    &context.account_epoch,
                    &context.account_scope,
                    &album,
                    &playlist,
                    saved,
                    &context.cancel,
                )
                .await
                .map_err(|error| map_catalog_write_error(error, ErrorCode::PermissionDenied))
        })
    }

    fn invalidate_library_membership(
        &self,
        scope: AccountScope,
        target: ytm_control_api::LibraryMembershipTarget,
    ) -> BoxFuture<'_, Result<(), QueryError>> {
        use ytm_control_api::LibraryMembershipTarget::*;
        use ytm_storage::LibraryMembershipCache;

        Box::pin(async move {
            let kind = match &target {
                Song { .. } => LibraryMembershipCache::Song,
                Playlist { id } => LibraryMembershipCache::Playlist(id),
                Album { .. } => return Ok(()),
            };
            self.adapter
                .invalidate_library_membership_cache(&scope, kind)
                .await
        })
    }

    fn album_playback_target(
        &self,
        context: CatalogReadContext,
        album: ytm_protocol::AlbumId,
        index: u32,
    ) -> BoxFuture<'_, Result<ytm_protocol::PlaybackRef, QueryError>> {
        Box::pin(async move {
            self.adapter
                .album_playback_target(
                    &context.account_epoch,
                    &context.account_scope,
                    &album,
                    index,
                    &context.cancel,
                )
                .await
        })
    }

    fn search_entities(
        &self,
        context: CatalogReadContext,
        kind: ytm_control_api::EntitySearchKind,
        query: String,
        request: PageInput,
    ) -> BoxFuture<'_, Result<Page<ytm_control_api::SearchEntity>, QueryError>> {
        use ytm_control_api::{EntitySearchKind, SearchEntity};
        use ytm_data::probe::{ProviderTypedSearchItem, TypedSearchKind};

        Box::pin(async move {
            let kind = match kind {
                EntitySearchKind::Album => TypedSearchKind::Albums,
                EntitySearchKind::Artist => TypedSearchKind::Artists,
                EntitySearchKind::Playlist => TypedSearchKind::Playlists,
            };
            let page = self
                .adapter
                .typed_search(
                    &context.account_epoch,
                    &context.account_scope,
                    kind,
                    &query,
                    page_request(&request),
                    &context.cancel,
                )
                .await?;
            let items = page
                .items
                .into_iter()
                .map(|item| match item {
                    ProviderTypedSearchItem::Album {
                        album_id,
                        title,
                        artist,
                        year,
                        ..
                    } => SearchEntity::Album {
                        album_id,
                        title,
                        artist,
                        year,
                    },
                    ProviderTypedSearchItem::Artist {
                        artist_id,
                        name,
                        subscribers,
                        ..
                    } => SearchEntity::Artist {
                        artist_id,
                        name,
                        subscribers,
                    },
                    ProviderTypedSearchItem::Playlist(item) => SearchEntity::Playlist {
                        playlist_id: item.playlist_id,
                        title: item.title,
                        byline: item.subtitle,
                        track_count: item.track_count,
                    },
                })
                .collect();
            Ok(Page {
                items,
                next_cursor: page.next_cursor,
                complete: page.complete,
                source: kind.source().into(),
                freshness: Freshness::Fresh,
                captured_at_ms: page.captured_at_ms,
            })
        })
    }

    fn mood_category(
        &self,
        context: CatalogReadContext,
        category_id: String,
        request: PageInput,
    ) -> BoxFuture<'_, Result<DiscoveryPage, QueryError>> {
        Box::pin(async move {
            self.adapter
                .mood_category(
                    &context.account_epoch,
                    &context.account_scope,
                    &category_id,
                    page_request(&request),
                    &context.cancel,
                )
                .await
                .map(|page| discovery_page(page, MOOD_PLAYLISTS_SOURCE, Some(false)))
        })
    }

    fn search_suggestions(
        &self,
        context: CatalogReadContext,
        query: String,
        limit: u16,
    ) -> BoxFuture<'_, Result<Page<String>, QueryError>> {
        Box::pin(async move {
            let suggestions = self
                .adapter
                .search_suggestions(
                    &context.account_epoch,
                    &context.account_scope,
                    &query,
                    u32::from(limit),
                    &context.cancel,
                )
                .await?;
            Ok(Page {
                items: suggestions.items,
                next_cursor: None,
                complete: suggestions.complete,
                source: SEARCH_SUGGESTIONS_SOURCE.into(),
                freshness: Freshness::Fresh,
                captured_at_ms: suggestions.captured_at_ms,
            })
        })
    }

    fn account_library(
        &self,
        context: CatalogReadContext,
        kind: LibraryKind,
        request: PageInput,
    ) -> BoxFuture<'_, Result<LibraryPage, QueryError>> {
        Box::pin(async move {
            let surface = match kind {
                LibraryKind::Songs => LibrarySurface::Songs,
                LibraryKind::Albums => LibrarySurface::Albums,
                LibraryKind::Artists => LibrarySurface::Artists,
                LibraryKind::Subscriptions => LibrarySurface::Subscriptions,
                _ => return Err(library_shape_error()),
            };
            let provider = self.library_page(context, surface, request).await?;
            match surface {
                LibrarySurface::Songs => project_library_page(provider, surface, |row| match row {
                    ProviderLibraryItem::Song(track) => Some(project_track(&track)),
                    _ => None,
                })
                .map(LibraryPage::Songs),
                LibrarySurface::Albums => {
                    project_library_page(provider, surface, |row| match row {
                        ProviderLibraryItem::Album {
                            album_id,
                            title,
                            artists,
                        } => Some(LibraryAlbum {
                            album: Album {
                                id: Some(album_id),
                                title,
                            },
                            artists: artists
                                .into_iter()
                                .map(|artist| Artist {
                                    id: artist.id,
                                    name: artist.name,
                                })
                                .collect(),
                        }),
                        _ => None,
                    })
                    .map(LibraryPage::Albums)
                }
                LibrarySurface::Artists | LibrarySurface::Subscriptions => {
                    let page = project_library_page(provider, surface, |row| match row {
                        ProviderLibraryItem::Artist {
                            artist_id,
                            name,
                            byline,
                        } => Some(LibraryArtist {
                            artist: Artist {
                                id: Some(artist_id),
                                name,
                            },
                            byline,
                        }),
                        _ => None,
                    })?;
                    Ok(if surface == LibrarySurface::Artists {
                        LibraryPage::Artists(page)
                    } else {
                        LibraryPage::Subscriptions(page)
                    })
                }
                LibrarySurface::History => Err(library_shape_error()),
            }
        })
    }

    fn youtube_history(
        &self,
        context: CatalogReadContext,
        request: PageInput,
    ) -> BoxFuture<'_, Result<Page<HistoryEntry>, QueryError>> {
        Box::pin(async move {
            let surface = LibrarySurface::History;
            let page = self.library_page(context, surface, request).await?;
            project_library_page(page, surface, |row| match row {
                ProviderLibraryItem::History { track, period } => Some(HistoryEntry {
                    media: HistoryMedia::Song {
                        track: Box::new(project_track(&track)),
                    },
                    period,
                }),
                ProviderLibraryItem::HistoryEpisode {
                    episode_id,
                    title,
                    period,
                } => Some(HistoryEntry {
                    media: HistoryMedia::PodcastEpisode { episode_id, title },
                    period,
                }),
                _ => None,
            })
        })
    }

    fn song_membership(
        &self,
        context: CatalogReadContext,
        video: VideoId,
    ) -> BoxFuture<'_, Result<MembershipEvidence, QueryError>> {
        Box::pin(async move {
            let evidence = self
                .adapter
                .fresh_song_membership(
                    &context.account_epoch,
                    &context.account_scope,
                    &video,
                    &context.cancel,
                )
                .await?;
            Ok(MembershipEvidence {
                value: evidence.in_library,
                observed_at_ms: evidence.observed_at_ms,
            })
        })
    }

    fn artist_subscription(
        &self,
        context: CatalogReadContext,
        artist: ArtistId,
    ) -> BoxFuture<'_, Result<MembershipEvidence, QueryError>> {
        Box::pin(async move {
            let evidence = self
                .adapter
                .fresh_artist_subscription(
                    &context.account_epoch,
                    &context.account_scope,
                    &artist,
                    &context.cancel,
                )
                .await?;
            Ok(MembershipEvidence {
                value: evidence.subscribed,
                observed_at_ms: evidence.observed_at_ms,
            })
        })
    }

    fn set_artist_subscription(
        &self,
        context: CatalogReadContext,
        artist: ArtistId,
        subscribed: bool,
    ) -> BoxFuture<'_, Result<(), CatalogWriteError>> {
        Box::pin(async move {
            self.adapter
                .set_artist_subscription_once(
                    &context.account_epoch,
                    &context.account_scope,
                    &artist,
                    subscribed,
                    &context.cancel,
                )
                .await
                .map_err(|err| map_catalog_write_error(err, ErrorCode::PermissionDenied))
        })
    }

    fn extended_media(
        &self,
        context: CatalogReadContext,
        target: ExtendedMediaTarget,
        request: PageInput,
    ) -> BoxFuture<'_, Result<ExtendedMediaPage, QueryError>> {
        Box::pin(async move {
            use ExtendedMediaTarget as Target;
            let query = match &target {
                Target::UploadSongs => ExtendedMediaQuery::UploadSongs,
                Target::UploadAlbums => ExtendedMediaQuery::UploadAlbums,
                Target::UploadArtists => ExtendedMediaQuery::UploadArtists,
                Target::UploadAlbum { browse_id } => ExtendedMediaQuery::UploadAlbum(browse_id),
                Target::UploadArtist { browse_id } => ExtendedMediaQuery::UploadArtist(browse_id),
                Target::LibraryPodcasts => ExtendedMediaQuery::LibraryPodcasts,
                Target::Podcast { browse_id } => ExtendedMediaQuery::Podcast(browse_id),
                Target::PodcastChannel { browse_id } => {
                    ExtendedMediaQuery::PodcastChannel(browse_id)
                }
                Target::PodcastChannelEpisodes { browse_id } => {
                    ExtendedMediaQuery::PodcastChannelEpisodes(browse_id)
                }
                Target::PodcastEpisode { browse_id } => {
                    ExtendedMediaQuery::PodcastEpisode(browse_id)
                }
            };
            let page = self
                .adapter
                .extended_media(
                    &context.account_epoch,
                    &context.account_scope,
                    query,
                    page_request(&request),
                    &context.cancel,
                )
                .await?;
            Ok(ExtendedMediaPage {
                entity: page.entity.map(project_extended_media),
                entries: Page {
                    items: page.items.into_iter().map(project_extended_media).collect(),
                    next_cursor: page.next_cursor,
                    complete: page.complete,
                    source: query.source().into(),
                    freshness: Freshness::Fresh,
                    captured_at_ms: page.captured_at_ms,
                },
            })
        })
    }

    fn discover(
        &self,
        context: CatalogReadContext,
        kind: DiscoveryKind,
        request: PageInput,
    ) -> BoxFuture<'_, Result<DiscoveryPage, QueryError>> {
        Box::pin(async move {
            let surface = match kind {
                DiscoveryKind::Home => DiscoverySurface::Home,
                DiscoveryKind::Explore => DiscoverySurface::Explore,
                DiscoveryKind::MoodsAndGenres => DiscoverySurface::MoodsAndGenres,
                DiscoveryKind::Charts => DiscoverySurface::Charts,
                DiscoveryKind::NewReleases => DiscoverySurface::NewReleases,
            };
            let page = self
                .adapter
                .discover(
                    &context.account_epoch,
                    &context.account_scope,
                    surface,
                    page_request(&request),
                    &context.cancel,
                )
                .await?;
            Ok(discovery_page(
                page,
                surface.source(),
                (!matches!(
                    surface,
                    DiscoverySurface::Home | DiscoverySurface::NewReleases
                ))
                .then_some(false),
            ))
        })
    }

    fn fresh_playlist(
        &self,
        context: CatalogReadContext,
        playlist: PlaylistId,
        request: PageInput,
    ) -> BoxFuture<'_, Result<Playlist, QueryError>> {
        Box::pin(async move {
            let page = self
                .adapter
                .fresh_playlist(
                    &context.account_epoch,
                    &context.account_scope,
                    &playlist,
                    page_request(&request),
                    &context.cancel,
                )
                .await?;
            let complete_first_read = request.cursor.is_none() && page.complete;
            let metadata = if let Some(metadata) = page.metadata {
                metadata
            } else {
                // The provider continuation carries only table rows.
                self.adapter
                    .fresh_playlist(
                        &context.account_epoch,
                        &context.account_scope,
                        &playlist,
                        MusicPageRequest {
                            cursor: None,
                            limit: 1,
                        },
                        &context.cancel,
                    )
                    .await?
                    .metadata
                    .ok_or_else(|| {
                        make_query_error(
                            QueryErrorCode::InvalidPayload,
                            false,
                            None,
                            "playlist provider did not return fresh metadata",
                        )
                    })?
            };
            let entries = page
                .entries
                .into_iter()
                .map(|item| {
                    let mut track = project_track(&item.track);
                    track.rating = project_rating(item.rating);
                    PlaylistEntry {
                        entry_id: item.entry_id,
                        position: item.position,
                        track,
                        playback_index: item.playback_index,
                    }
                })
                .collect();
            let mut result = Playlist {
                playlist_id: metadata.playlist_id,
                identity_confirmed: metadata.identity_confirmed,
                title: metadata.title,
                description: metadata.description,
                privacy: metadata.privacy.map(|privacy| match privacy {
                    ProviderPrivacy::Private => PlaylistPrivacy::Private,
                    ProviderPrivacy::Unlisted => PlaylistPrivacy::Unlisted,
                    ProviderPrivacy::Public => PlaylistPrivacy::Public,
                }),
                editable: metadata.editable,
                snapshot_fingerprint: None,
                entries: Page {
                    items: entries,
                    next_cursor: page.next_cursor,
                    complete: page.complete,
                    source: PROVIDER_NAME.to_owned(),
                    freshness: Freshness::Fresh,
                    captured_at_ms: page.captured_at_ms,
                },
            };
            if complete_first_read {
                result.snapshot_fingerprint = playlist_fingerprint(&result);
            }
            Ok(result)
        })
    }

    fn recommend_song_radio(
        &self,
        context: CatalogReadContext,
        seed: VideoId,
        request: PageInput,
    ) -> BoxFuture<'_, Result<RecommendationPage, QueryError>> {
        Box::pin(async move {
            let page = self
                .adapter
                .recommend_song_radio(
                    &context.account_epoch,
                    &context.account_scope,
                    &seed,
                    page_request(&request),
                    &context.cancel,
                )
                .await?;
            Ok(RecommendationPage {
                seed: page.seed,
                candidates: Page {
                    items: page.candidates.iter().map(project_track).collect(),
                    complete: page.next_cursor.is_none(),
                    next_cursor: page.next_cursor,
                    source: SONG_RADIO_SOURCE.to_owned(),
                    freshness: Freshness::Fresh,
                    captured_at_ms: page.captured_at_ms,
                },
            })
        })
    }

    fn apply_playlist(
        &self,
        context: CatalogReadContext,
        write: PlaylistWrite,
    ) -> BoxFuture<'_, Result<PlaylistWriteAck, PlaylistWriteError>> {
        Box::pin(async move {
            let request = match &write {
                PlaylistWrite::CreatePrivate { title, description } => {
                    PlaylistMutation::CreatePrivate {
                        title,
                        description: description.as_deref(),
                    }
                }
                PlaylistWrite::Append { playlist, videos } => PlaylistMutation::Append {
                    playlist,
                    videos,
                    duplicates: PlaylistAppendDuplicates::Reject,
                },
            };
            match self
                .adapter
                .mutate_playlist_once(
                    &context.account_epoch,
                    &context.account_scope,
                    request,
                    &context.cancel,
                )
                .await
                .map_err(|error| map_catalog_write_error(error, ErrorCode::NotEditable))?
            {
                PlaylistMutationAck::Created(playlist) => Ok(PlaylistWriteAck::Created(playlist)),
                PlaylistMutationAck::Appended(entries) => Ok(PlaylistWriteAck::Appended(
                    entries
                        .into_iter()
                        .map(|entry| AddedOccurrence {
                            video_id: entry.video_id,
                            entry_id: entry.entry_id,
                        })
                        .collect(),
                )),
                PlaylistMutationAck::Accepted => {
                    Err(PlaylistWriteError::Unknown(ControlError::new(
                        ErrorCode::ResultUnknown,
                        RetryDisposition::ReconcileFirst,
                        "playlist provider response did not identify the written resources",
                    )))
                }
            }
        })
    }

    fn invalidate_playlist_cache(
        &self,
        scope: AccountScope,
        playlist: PlaylistId,
        library_changed: bool,
    ) -> BoxFuture<'_, Result<(), QueryError>> {
        Box::pin(async move {
            self.adapter
                .invalidate_playlist_cache(&scope, &playlist, library_changed)
                .await
        })
    }

    fn edit_playlist(
        &self,
        context: CatalogReadContext,
        playlist: PlaylistId,
        edit: PlaylistEdit,
    ) -> BoxFuture<'_, Result<(), CatalogWriteError>> {
        Box::pin(async move {
            let removal;
            let request = match &edit {
                PlaylistEdit::Update { title, description } => PlaylistMutation::Update {
                    playlist: &playlist,
                    title: title.as_deref(),
                    description: description.as_deref(),
                    privacy: None,
                },
                PlaylistEdit::SetPrivacy(privacy) => PlaylistMutation::Update {
                    playlist: &playlist,
                    title: None,
                    description: None,
                    privacy: Some(match privacy {
                        PlaylistPrivacy::Private => ProviderPrivacy::Private,
                        PlaylistPrivacy::Unlisted => ProviderPrivacy::Unlisted,
                        PlaylistPrivacy::Public => ProviderPrivacy::Public,
                    }),
                },
                PlaylistEdit::Remove(entries) => {
                    removal = entries
                        .iter()
                        .map(|entry| PlaylistRemoval {
                            video_id: entry.video_id.clone(),
                            entry_id: entry.entry_id.clone(),
                        })
                        .collect::<Vec<_>>();
                    PlaylistMutation::Remove {
                        playlist: &playlist,
                        entries: &removal,
                    }
                }
                PlaylistEdit::Move {
                    entry_id,
                    before_entry_id,
                } => PlaylistMutation::Move {
                    playlist: &playlist,
                    entry_id,
                    before: before_entry_id.as_deref(),
                },
                PlaylistEdit::Delete => PlaylistMutation::Delete {
                    playlist: &playlist,
                },
            };
            let ack = self
                .adapter
                .mutate_playlist_once(
                    &context.account_epoch,
                    &context.account_scope,
                    request,
                    &context.cancel,
                )
                .await
                .map_err(|error| map_catalog_write_error(error, ErrorCode::NotEditable))?;
            if ack != PlaylistMutationAck::Accepted {
                return Err(CatalogWriteError::Unknown(ControlError::new(
                    ErrorCode::ResultUnknown,
                    RetryDisposition::ReconcileFirst,
                    "YouTube Music returned an unexpected playlist edit acknowledgment",
                )));
            }
            Ok(())
        })
    }

    fn fresh_rating(
        &self,
        context: CatalogReadContext,
        video: VideoId,
    ) -> BoxFuture<'_, Result<RatingEvidence, QueryError>> {
        Box::pin(async move {
            let observed = self
                .adapter
                .fresh_rating(
                    &context.account_epoch,
                    &context.account_scope,
                    &video,
                    &context.cancel,
                )
                .await?;
            Ok(RatingEvidence {
                rating: project_rating(observed.rating),
                observed_at_ms: observed.observed_at_ms,
            })
        })
    }

    fn set_rating(
        &self,
        context: CatalogReadContext,
        video: VideoId,
        rating: Rating,
    ) -> BoxFuture<'_, Result<(), CatalogWriteError>> {
        Box::pin(async move {
            let rating = match rating {
                Rating::Liked => SongRating::Liked,
                Rating::Disliked => SongRating::Disliked,
                Rating::Neutral => SongRating::Neutral,
                Rating::Unknown => {
                    return Err(CatalogWriteError::Rejected(ControlError::new(
                        ErrorCode::InvalidInput,
                        RetryDisposition::NotRetryable,
                        "unknown cannot be written as a rating",
                    )));
                }
            };
            self.adapter
                .set_rating_once(
                    &context.account_epoch,
                    &context.account_scope,
                    &video,
                    rating,
                    &context.cancel,
                )
                .await
                .map_err(|err| map_catalog_write_error(err, ErrorCode::PermissionDenied))
        })
    }

    fn invalidate_rating_cache(
        &self,
        scope: AccountScope,
    ) -> BoxFuture<'_, Result<(), QueryError>> {
        Box::pin(async move { self.adapter.invalidate_rating_cache(&scope).await })
    }
}

fn discovery_page(
    page: ProviderDiscoveryPage,
    source: &str,
    personalized: Option<bool>,
) -> DiscoveryPage {
    DiscoveryPage {
        items: Page {
            items: page
                .items
                .into_iter()
                .map(|item| DiscoveryItem {
                    section: item.section,
                    title: item.title,
                    target: match item.target {
                        ProviderDiscoveryTarget::Track(video_id) => {
                            DiscoveryTarget::Track { video_id }
                        }
                        ProviderDiscoveryTarget::Playlist(playlist_id) => {
                            DiscoveryTarget::Playlist { playlist_id }
                        }
                        ProviderDiscoveryTarget::Album(album_id) => {
                            DiscoveryTarget::Album { album_id }
                        }
                        ProviderDiscoveryTarget::Artist(artist_id) => {
                            DiscoveryTarget::Artist { artist_id }
                        }
                        ProviderDiscoveryTarget::MoodCategory(category_id) => {
                            DiscoveryTarget::MoodCategory { category_id }
                        }
                    },
                })
                .collect(),
            next_cursor: page.next_cursor,
            complete: page.complete,
            source: source.into(),
            freshness: Freshness::Fresh,
            captured_at_ms: page.captured_at_ms,
        },
        personalized,
    }
}

fn project_extended_media(item: ProviderExtendedItem) -> ExtendedMediaItem {
    let kind = match item.kind {
        ProviderMediaKind::UploadedSong => ExtendedMediaKind::UploadedSong,
        ProviderMediaKind::UploadedAlbum => ExtendedMediaKind::UploadedAlbum,
        ProviderMediaKind::UploadedArtist => ExtendedMediaKind::UploadedArtist,
        ProviderMediaKind::Podcast => ExtendedMediaKind::Podcast,
        ProviderMediaKind::PodcastChannel => ExtendedMediaKind::PodcastChannel,
        ProviderMediaKind::PodcastEpisode => ExtendedMediaKind::PodcastEpisode,
    };
    ExtendedMediaItem {
        kind,
        id: item.id,
        title: item.title,
        subtitle: item.subtitle,
        description: item.description,
        video_id: item.video_id,
        related_id: item.related_id,
        duration_label: item.duration,
        published_label: item.published,
    }
}

fn project_library_page<T>(
    page: ProviderLibraryPage,
    surface: LibrarySurface,
    project: impl Fn(ProviderLibraryItem) -> Option<T>,
) -> Result<Page<T>, QueryError> {
    let items = page
        .items
        .into_iter()
        .map(|item| project(item).ok_or_else(library_shape_error))
        .collect::<Result<_, _>>()?;
    Ok(Page {
        items,
        next_cursor: page.next_cursor,
        complete: page.complete,
        source: surface.source().into(),
        freshness: Freshness::Fresh,
        captured_at_ms: page.captured_at_ms,
    })
}

fn library_shape_error() -> QueryError {
    make_query_error(
        QueryErrorCode::InvalidPayload,
        false,
        None,
        "the library response did not match the requested surface",
    )
}

fn project_rating(rating: SongRating) -> Rating {
    match rating {
        SongRating::Liked => Rating::Liked,
        SongRating::Disliked => Rating::Disliked,
        SongRating::Neutral => Rating::Neutral,
        SongRating::Unknown => Rating::Unknown,
    }
}

fn map_catalog_write_error(
    error: CatalogMutationError,
    permission: ErrorCode,
) -> CatalogWriteError {
    let (code, message) = match error {
        CatalogMutationError::InvalidInput => (
            ErrorCode::InvalidInput,
            "catalog write arguments are invalid",
        ),
        CatalogMutationError::PermissionDenied => (permission, "YouTube Music denied this change"),
        CatalogMutationError::Rejected(error) => {
            let (code, retry) = match error.code {
                QueryErrorCode::AuthRequired | QueryErrorCode::AuthExpired => {
                    (ErrorCode::AuthRequired, RetryDisposition::ReauthRequired)
                }
                QueryErrorCode::AccountMismatch | QueryErrorCode::StaleContext => {
                    (ErrorCode::StaleContext, RetryDisposition::NewPlanRequired)
                }
                QueryErrorCode::RateLimited => {
                    (ErrorCode::RateLimited, RetryDisposition::NotRetryable)
                }
                QueryErrorCode::UnsupportedCapability => (
                    ErrorCode::CapabilityUnavailable,
                    RetryDisposition::NotRetryable,
                ),
                QueryErrorCode::Busy => (ErrorCode::Busy, RetryDisposition::NotRetryable),
                _ => (ErrorCode::Unavailable, RetryDisposition::NotRetryable),
            };
            let mut rejected = ControlError::new(code, retry, error.safe_message);
            rejected.retry_after_ms = error.retry_after_ms;
            return PlaylistWriteError::Rejected(rejected);
        }
        CatalogMutationError::Unknown(error) => {
            return PlaylistWriteError::Unknown(ControlError::new(
                ErrorCode::ResultUnknown,
                RetryDisposition::ReconcileFirst,
                error.safe_message,
            ));
        }
    };
    PlaylistWriteError::Rejected(ControlError::new(
        code,
        RetryDisposition::NotRetryable,
        message,
    ))
}

fn page_request(input: &PageInput) -> MusicPageRequest<'_> {
    MusicPageRequest {
        cursor: input.cursor.as_deref(),
        limit: input.limit.map_or(20, u32::from),
    }
}

/// A fingerprint represents a complete ordering of unique provider occurrences.
fn playlist_fingerprint(playlist: &Playlist) -> Option<String> {
    let mut seen = HashSet::new();
    let ordered_ids = playlist
        .entries
        .items
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let id = entry.entry_id.as_deref()?;
            (entry.position as usize == index && seen.insert(id))
                .then_some((id, &entry.track.video_id))
        })
        .collect::<Option<Vec<_>>>()?;
    let snapshot = serde_json::to_vec(&(
        &playlist.playlist_id,
        &playlist.title,
        &playlist.description,
        &playlist.privacy,
        &playlist.editable,
        ordered_ids,
    ))
    .ok()?;
    Some(format!(
        "ytm-playlist-v1:{}",
        hex::encode(Sha256::digest(snapshot))
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ytm_control_api::Track;
    use ytm_protocol::TrackAvailability;

    #[test]
    fn fingerprint_tracks_occurrence_order_and_rejects_incomplete_identity() {
        let track = Track {
            video_id: VideoId::new("synthetic-song").unwrap(),
            title: "Song".into(),
            artists: Vec::new(),
            album: None,
            duration_ms: None,
            availability: TrackAvailability::Unknown,
            version_tag: None,
            rating: Rating::Unknown,
        };
        let mut playlist = Playlist {
            playlist_id: PlaylistId::new("PL_synthetic").unwrap(),
            identity_confirmed: true,
            title: "Test".into(),
            description: None,
            privacy: Some(PlaylistPrivacy::Private),
            editable: Some(true),
            snapshot_fingerprint: None,
            entries: Page {
                items: vec![
                    PlaylistEntry {
                        entry_id: Some("one".into()),
                        position: 0,
                        track: track.clone(),
                        playback_index: None,
                    },
                    PlaylistEntry {
                        entry_id: Some("two".into()),
                        position: 1,
                        track,
                        playback_index: None,
                    },
                ],
                next_cursor: None,
                complete: true,
                source: "test".into(),
                freshness: Freshness::Fresh,
                captured_at_ms: 100,
            },
        };
        let original = playlist_fingerprint(&playlist).unwrap();
        playlist.entries.captured_at_ms += 10;
        assert_eq!(
            playlist_fingerprint(&playlist).as_deref(),
            Some(original.as_str())
        );
        playlist.entries.items.swap(0, 1);
        assert!(playlist_fingerprint(&playlist).is_none());
        playlist.entries.items[0].position = 0;
        playlist.entries.items[1].position = 1;
        assert_ne!(playlist_fingerprint(&playlist).unwrap(), original);
        playlist.entries.items[0].entry_id = playlist.entries.items[1].entry_id.clone();
        assert!(playlist_fingerprint(&playlist).is_none());
        playlist.entries.items[0].entry_id = None;
        assert!(playlist_fingerprint(&playlist).is_none());
    }
}
