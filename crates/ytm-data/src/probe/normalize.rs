use ytm_protocol::{
    AlbumDetail, AlbumId, AlbumRef, ArtistDetail, ArtistId, ArtistRef, BrowseId, CoverRef,
    PlaylistId, PlaylistSummary, QueryError, QueryErrorCode, Track, TrackAvailability, VideoId,
};
use ytmapi_rs::common::{LikeStatus, Thumbnail, YoutubeID};
use ytmapi_rs::parse::{
    AlbumSong, BasicSearchResultCommunityPlaylist, GetAlbum, GetArtist, GetPlaylistDetails,
    HistoryItem, HistoryPeriod, ParsedSongAlbum, ParsedSongArtist, PlaylistItem, SearchResultSong,
    SearchResults,
};

use crate::probe::error::make_query_error;

pub const HOME_FALLBACK_NOTICE: &str = "Personalized recommendations are not available; showing your verified library and recent plays";

pub fn parse_duration_ms(raw: &str) -> Option<u64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut parts = trimmed.split(':').map(str::trim);
    let nums: Option<Vec<u64>> = parts.try_fold(Vec::new(), |mut acc, p| {
        let n = p.parse::<u64>().ok()?;
        acc.push(n);
        Some(acc)
    });
    let nums = nums?;
    let total_secs = match nums.as_slice() {
        [secs] => *secs,
        [mins, secs] if *secs < 60 => mins.saturating_mul(60).saturating_add(*secs),
        [hours, mins, secs] if *mins < 60 && *secs < 60 => hours
            .saturating_mul(3_600)
            .saturating_add(mins.saturating_mul(60))
            .saturating_add(*secs),
        _ => return None,
    };
    Some(total_secs.saturating_mul(1_000))
}

pub fn parse_track_count(raw: &str) -> Option<u32> {
    let digits: String = raw
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit() || *c == ',')
        .filter(char::is_ascii_digit)
        .collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse::<u32>().ok()
    }
}

/// Browse id for a playlist id. Auto playlists are no exception: "Liked music" (`LM`) is
/// browsed as `VLLM`, like every other playlist.
pub fn ensure_vl_browse_id(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.starts_with("VL") || trimmed.starts_with("FE") {
        trimmed.to_string()
    } else {
        format!("VL{trimmed}")
    }
}

/// Lyrics browse id from a watch-next response. The lyrics tab (index 1) without an endpoint
/// is YouTube Music's "no lyrics" shape (`None`); a response without that tab at all is a
/// changed shape and an error, not "no lyrics".
pub fn lyrics_browse_id(watch_next: &serde_json::Value) -> Result<Option<String>, QueryError> {
    let tab = watch_next.pointer(
        "/contents/singleColumnMusicWatchNextResultsRenderer/tabbedRenderer/watchNextTabbedResultsRenderer/tabs/1/tabRenderer",
    );
    let Some(tab) = tab else {
        return Err(make_query_error(
            QueryErrorCode::InvalidPayload,
            false,
            None,
            "watch-next response has no lyrics tab",
        ));
    };
    Ok(tab
        .pointer("/endpoint/browseEndpoint/browseId")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string))
}

pub fn strip_vl_prefix(raw: &str) -> &str {
    raw.trim().strip_prefix("VL").unwrap_or_else(|| raw.trim())
}

pub(crate) fn is_system_library_playlist(raw: &str) -> bool {
    matches!(strip_vl_prefix(raw), "LM" | "SE")
}

pub fn best_cover_ref(thumbnails: &[Thumbnail]) -> Option<CoverRef> {
    let best = thumbnails
        .iter()
        .filter(|t| {
            let u = t.url.trim();
            u.starts_with("https://") || u.starts_with("http://")
        })
        .max_by_key(|t| t.width.saturating_mul(t.height))?;
    Some(CoverRef {
        asset_id: None,
        remote_url: Some(best.url.clone()),
        accent_oklch: None,
    })
}

pub(super) fn like_status_to_bool(status: &LikeStatus) -> Option<bool> {
    match status {
        LikeStatus::Liked => Some(true),
        LikeStatus::Disliked | LikeStatus::Indifferent => Some(false),
    }
}

pub(super) fn map_parsed_artists(artists: &[ParsedSongArtist]) -> Vec<ArtistRef> {
    artists
        .iter()
        .filter(|a| !a.name.trim().is_empty())
        .map(|a| ArtistRef {
            id: a
                .id
                .as_ref()
                .and_then(|id| ArtistId::new(id.get_raw()).ok()),
            name: a.name.clone(),
        })
        .collect()
}

pub(super) fn map_parsed_album(album: &ParsedSongAlbum) -> Option<AlbumRef> {
    if album.name.trim().is_empty() {
        return None;
    }
    let raw = album.id.get_raw();
    Some(AlbumRef {
        id: AlbumId::new(raw).ok(),
        browse_id: BrowseId::new(raw).ok(),
        title: album.name.clone(),
    })
}

pub(super) fn video_id_or_err(raw: &str) -> Result<VideoId, QueryError> {
    VideoId::new(raw).map_err(|_| {
        make_query_error(
            QueryErrorCode::InvalidPayload,
            false,
            None,
            "upstream item has invalid videoId",
        )
    })
}

pub fn normalize_search_song(song: SearchResultSong) -> Result<Track, QueryError> {
    let video_id = video_id_or_err(song.video_id.get_raw())?;
    let artists = if song.artist.trim().is_empty() {
        Vec::new()
    } else {
        vec![ArtistRef {
            id: None,
            name: song.artist,
        }]
    };
    Ok(Track {
        video_id,
        title: song.title,
        artists,
        album: song.album.as_ref().and_then(map_parsed_album),
        duration_ms: parse_duration_ms(&song.duration),
        cover: best_cover_ref(&song.thumbnails),
        availability: TrackAvailability::Available,
        version_tag: None,
        is_liked: None,
    })
}

pub fn normalize_playlist_item(item: PlaylistItem) -> Result<Track, QueryError> {
    match item {
        PlaylistItem::Song(song) => Ok(Track {
            video_id: video_id_or_err(song.video_id.get_raw())?,
            title: song.title,
            artists: map_parsed_artists(&song.artists),
            album: map_parsed_album(&song.album),
            duration_ms: parse_duration_ms(&song.duration),
            cover: best_cover_ref(&song.thumbnails),
            availability: if song.is_available {
                TrackAvailability::Available
            } else {
                TrackAvailability::Unavailable
            },
            version_tag: None,
            is_liked: like_status_to_bool(&song.like_status),
        }),
        PlaylistItem::Video(video) => Ok(Track {
            video_id: video_id_or_err(video.video_id.get_raw())?,
            title: video.title,
            artists: if video.channel_name.trim().is_empty() {
                Vec::new()
            } else {
                vec![ArtistRef {
                    id: ArtistId::new(video.channel_id.get_raw()).ok(),
                    name: video.channel_name,
                }]
            },
            album: None,
            duration_ms: parse_duration_ms(&video.duration),
            cover: best_cover_ref(&video.thumbnails),
            availability: if video.is_available {
                TrackAvailability::Available
            } else {
                TrackAvailability::Unavailable
            },
            version_tag: None,
            is_liked: like_status_to_bool(&video.like_status),
        }),
        PlaylistItem::UploadSong(upload) => Ok(Track {
            video_id: video_id_or_err(upload.video_id.get_raw())?,
            title: upload.title,
            artists: upload
                .artists
                .into_iter()
                .filter(|a| !a.name.trim().is_empty())
                .map(|a| ArtistRef {
                    id: a
                        .id
                        .as_ref()
                        .and_then(|id| ArtistId::new(id.get_raw()).ok()),
                    name: a.name,
                })
                .collect(),
            album: upload.album.map(|a| AlbumRef {
                id: AlbumId::new(a.id.get_raw()).ok(),
                browse_id: BrowseId::new(a.id.get_raw()).ok(),
                title: a.name,
            }),
            duration_ms: parse_duration_ms(&upload.duration),
            cover: best_cover_ref(&upload.thumbnails),
            availability: TrackAvailability::Available,
            version_tag: None,
            is_liked: like_status_to_bool(&upload.like_status),
        }),
        PlaylistItem::Episode(ep) => {
            let duration_ms = match &ep.duration {
                ytmapi_rs::parse::EpisodeDuration::Live => None,
                ytmapi_rs::parse::EpisodeDuration::Recorded { duration } => {
                    parse_duration_ms(duration)
                }
            };
            Ok(Track {
                video_id: video_id_or_err(ep.episode_id.get_raw())?,
                title: ep.title,
                artists: if ep.podcast_name.trim().is_empty() {
                    Vec::new()
                } else {
                    vec![ArtistRef {
                        id: None,
                        name: ep.podcast_name,
                    }]
                },
                album: None,
                duration_ms,
                cover: best_cover_ref(&ep.thumbnails),
                availability: if ep.is_available {
                    TrackAvailability::Available
                } else {
                    TrackAvailability::Unavailable
                },
                version_tag: None,
                is_liked: like_status_to_bool(&ep.like_status),
            })
        }
    }
}

pub fn normalize_playlist_metadata(
    details: GetPlaylistDetails,
) -> (String, Option<String>, Option<String>, Option<CoverRef>) {
    (
        details.title,
        details.description,
        (!details.author.trim().is_empty()).then_some(details.author),
        best_cover_ref(&details.thumbnails),
    )
}

pub fn normalize_album_detail(
    requested_album_id: &AlbumId,
    album: GetAlbum,
) -> Result<AlbumDetail, QueryError> {
    let artists = map_parsed_artists(&album.artists);
    let cover = best_cover_ref(&album.thumbnails);
    let album_ref = AlbumRef {
        id: Some(requested_album_id.clone()),
        browse_id: BrowseId::new(requested_album_id.as_str()).ok(),
        title: album.title.clone(),
    };
    let tracks = album
        .tracks
        .into_iter()
        .map(|t| normalize_album_song(t, &artists, &album_ref, &cover))
        .collect::<Result<Vec<_>, _>>()?;
    let year = album.year.trim().parse::<u16>().ok();
    Ok(AlbumDetail {
        album_id: requested_album_id.clone(),
        title: album.title,
        artists,
        year,
        cover,
        tracks,
        audio_playlist_id: None,
        playback_indexes: Vec::new(),
    })
}

fn normalize_album_song(
    song: AlbumSong,
    album_artists: &[ArtistRef],
    album_ref: &AlbumRef,
    album_cover: &Option<CoverRef>,
) -> Result<Track, QueryError> {
    Ok(Track {
        video_id: video_id_or_err(song.video_id.get_raw())?,
        title: song.title,
        artists: album_artists.to_vec(),
        album: Some(album_ref.clone()),
        duration_ms: parse_duration_ms(&song.duration),
        cover: album_cover.clone(),
        availability: TrackAvailability::Available,
        version_tag: None,
        is_liked: like_status_to_bool(&song.like_status),
    })
}

pub fn normalize_artist_detail(
    requested_artist_id: &ArtistId,
    artist: GetArtist,
) -> Result<ArtistDetail, QueryError> {
    let artist_id =
        ArtistId::new(artist.channel_id.get_raw()).unwrap_or_else(|_| requested_artist_id.clone());
    let cover = best_cover_ref(&artist.thumbnails);

    let mut top_songs = Vec::new();
    if let Some(songs) = artist.top_releases.songs {
        for s in songs.results {
            top_songs.push(Track {
                video_id: video_id_or_err(s.video_id.get_raw())?,
                title: s.title,
                artists: map_parsed_artists(&s.artists),
                album: map_parsed_album(&s.album),
                duration_ms: None,
                cover: cover.clone(),
                availability: TrackAvailability::Available,
                version_tag: None,
                is_liked: like_status_to_bool(&s.like_status),
            });
        }
    }

    let mut albums = Vec::new();
    if let Some(alb) = artist.top_releases.albums {
        for a in alb.results {
            let raw = a.album_id.get_raw();
            albums.push(AlbumRef {
                id: AlbumId::new(raw).ok(),
                browse_id: BrowseId::new(raw).ok(),
                title: a.title,
            });
        }
    }
    if let Some(singles) = artist.top_releases.singles {
        for a in singles.results {
            let raw = a.album_id.get_raw();
            albums.push(AlbumRef {
                id: AlbumId::new(raw).ok(),
                browse_id: BrowseId::new(raw).ok(),
                title: a.title,
            });
        }
    }

    Ok(ArtistDetail {
        artist_id,
        name: artist.name,
        description: artist.description,
        cover,
        top_songs,
        albums,
    })
}

/// Albums, artists and playlists from the first page of an unfiltered search, at most `max` each.
/// Entries with ids that fail validation are skipped rather than failing the search.
pub fn normalize_search_overview(results: SearchResults, max: usize) -> SearchOverview {
    let albums = results
        .albums
        .into_iter()
        .filter_map(|a| {
            Some(AlbumRef {
                id: Some(AlbumId::new(a.album_id.get_raw()).ok()?),
                browse_id: None,
                title: a.title,
            })
        })
        .take(max)
        .collect();
    let artists = results
        .artists
        .into_iter()
        .filter_map(|a| {
            Some(ArtistRef {
                id: Some(ArtistId::new(a.browse_id.get_raw()).ok()?),
                name: a.artist,
            })
        })
        .take(max)
        .collect();
    let featured = results.featured_playlists.into_iter().map(|p| {
        (
            p.playlist_id.get_raw().to_string(),
            p.title,
            p.author,
            parse_track_count(&p.songs),
            p.thumbnails,
        )
    });
    let community = results
        .community_playlists
        .into_iter()
        .filter_map(|p| match p {
            BasicSearchResultCommunityPlaylist::Playlist(p) => Some((
                p.playlist_id.get_raw().to_string(),
                p.title,
                p.author,
                None,
                p.thumbnails,
            )),
            _ => None,
        });
    let playlists = featured
        .chain(community)
        .filter_map(|(id, title, author, track_count, thumbnails)| {
            Some(PlaylistSummary {
                playlist_id: PlaylistId::new(strip_vl_prefix(&id)).ok()?,
                browse_id: None,
                title,
                subtitle: Some(author).filter(|a| !a.trim().is_empty()),
                track_count,
                cover: best_cover_ref(&thumbnails),
            })
        })
        .take(max)
        .collect();
    SearchOverview {
        albums,
        artists,
        playlists,
    }
}

/// Non-song sections of a search, normalized to protocol types.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchOverview {
    pub albums: Vec<AlbumRef>,
    pub artists: Vec<ArtistRef>,
    pub playlists: Vec<PlaylistSummary>,
}

pub fn normalize_history_periods(periods: Vec<HistoryPeriod>) -> Result<Vec<Track>, QueryError> {
    periods
        .into_iter()
        .flat_map(|period| period.items)
        .map(normalize_history_item)
        .collect()
}

pub(super) fn normalize_history_item(item: HistoryItem) -> Result<Track, QueryError> {
    Ok(match item {
        HistoryItem::Song(s) => Track {
            video_id: video_id_or_err(s.video_id.get_raw())?,
            title: s.title,
            artists: map_parsed_artists(&s.artists),
            album: map_parsed_album(&s.album),
            duration_ms: parse_duration_ms(&s.duration),
            cover: best_cover_ref(&s.thumbnails),
            availability: if s.is_available {
                TrackAvailability::Available
            } else {
                TrackAvailability::Unavailable
            },
            version_tag: None,
            is_liked: like_status_to_bool(&s.like_status),
        },
        HistoryItem::Video(v) => Track {
            video_id: video_id_or_err(v.video_id.get_raw())?,
            title: v.title,
            artists: if v.channel_name.trim().is_empty() {
                Vec::new()
            } else {
                vec![ArtistRef {
                    id: ArtistId::new(v.channel_id.get_raw()).ok(),
                    name: v.channel_name,
                }]
            },
            album: None,
            duration_ms: parse_duration_ms(&v.duration),
            cover: best_cover_ref(&v.thumbnails),
            availability: if v.is_available {
                TrackAvailability::Available
            } else {
                TrackAvailability::Unavailable
            },
            version_tag: None,
            is_liked: like_status_to_bool(&v.like_status),
        },
        HistoryItem::Episode(ep) => {
            let duration_ms = match &ep.duration {
                ytmapi_rs::parse::EpisodeDuration::Live => None,
                ytmapi_rs::parse::EpisodeDuration::Recorded { duration } => {
                    parse_duration_ms(duration)
                }
            };
            Track {
                video_id: video_id_or_err(ep.episode_id.get_raw())?,
                title: ep.title,
                artists: if ep.podcast_name.trim().is_empty() {
                    Vec::new()
                } else {
                    vec![ArtistRef {
                        id: None,
                        name: ep.podcast_name,
                    }]
                },
                album: None,
                duration_ms,
                cover: best_cover_ref(&ep.thumbnails),
                availability: if ep.is_available {
                    TrackAvailability::Available
                } else {
                    TrackAvailability::Unavailable
                },
                version_tag: None,
                is_liked: like_status_to_bool(&ep.like_status),
            }
        }
        HistoryItem::UploadSong(u) => Track {
            video_id: video_id_or_err(u.video_id.get_raw())?,
            title: u.title,
            artists: u
                .artists
                .into_iter()
                .filter(|a| !a.name.trim().is_empty())
                .map(|a| ArtistRef {
                    id: a
                        .id
                        .as_ref()
                        .and_then(|id| ArtistId::new(id.get_raw()).ok()),
                    name: a.name,
                })
                .collect(),
            album: Some(AlbumRef {
                id: AlbumId::new(u.album.id.get_raw()).ok(),
                browse_id: BrowseId::new(u.album.id.get_raw()).ok(),
                title: u.album.name,
            }),
            duration_ms: parse_duration_ms(&u.duration),
            cover: best_cover_ref(&u.thumbnails),
            availability: TrackAvailability::Available,
            version_tag: None,
            is_liked: like_status_to_bool(&u.like_status),
        },
    })
}
