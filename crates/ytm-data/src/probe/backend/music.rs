use std::collections::HashSet;

use serde_json::json;
use ytm_protocol::{ArtistRef, TrackAvailability, validate_id_str};
use ytmapi_rs::common::{ApiOutcome, SetVideoID};
use ytmapi_rs::error::ErrorKind;
use ytmapi_rs::parse::{AddPlaylistItem, WatchPlaylistTrack};
use ytmapi_rs::query::playlist::{CreatePlaylistQuery, PrivacyStatus};
use ytmapi_rs::query::{DeletePlaylistQuery, GetWatchPlaylistQuery};

use super::*;
use crate::probe::error::classify_ytmapi_error_kind;
use crate::probe::normalize::parse_duration_ms;

const MAX_MUTATION_ITEMS: usize = 50;
const PLAYLIST_HEADER: &str = concat!(
    "/contents/twoColumnBrowseResultsRenderer/tabs/0/tabRenderer/content",
    "/sectionListRenderer/contents/0"
);
const EDITABLE_HEADER: &str = "musicEditablePlaylistDetailHeaderRenderer";
const PLAYLIST_SHELF_ID: &str = concat!(
    "/contents/twoColumnBrowseResultsRenderer/secondaryContents/sectionListRenderer",
    "/contents/0/musicPlaylistShelfRenderer/playlistId"
);
const PLAYER_ACTIONS: &str = "/playerOverlays/playerOverlayRenderer/actions";

/// Source facts observed by this provider. Authorization belongs to the control service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaylistPrivacy {
    Private,
    Unlisted,
    Public,
}

impl PlaylistPrivacy {
    fn as_inner_tube(self) -> &'static str {
        match self {
            Self::Private => "PRIVATE",
            Self::Unlisted => "UNLISTED",
            Self::Public => "PUBLIC",
        }
    }

    fn from_inner_tube(value: &str) -> Option<Self> {
        match value {
            "PRIVATE" => Some(Self::Private),
            "UNLISTED" => Some(Self::Unlisted),
            "PUBLIC" => Some(Self::Public),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SongRating {
    Liked,
    Disliked,
    Neutral,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistOccurrence {
    pub entry_id: Option<String>,
    /// Zero-based within this provider page until the account-scoped adapter offsets it.
    pub position: u32,
    pub track: Track,
    pub rating: SongRating,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistMetadata {
    pub playlist_id: PlaylistId,
    pub identity_confirmed: bool,
    pub title: String,
    pub description: Option<String>,
    pub privacy: Option<PlaylistPrivacy>,
    pub editable: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendFreshPlaylist {
    pub metadata: Option<PlaylistMetadata>,
    pub entries: Vec<PlaylistOccurrence>,
    pub next_continuation: Option<String>,
    pub source_entries: usize,
}

/// A resolved occurrence for write endpoints that require both identities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistRemoval {
    pub video_id: VideoId,
    pub entry_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaylistAppendDuplicates {
    Reject,
    Allow,
}

#[derive(Debug)]
pub enum PlaylistMutation<'a> {
    CreatePrivate {
        title: &'a str,
        description: Option<&'a str>,
    },
    Append {
        playlist: &'a PlaylistId,
        videos: &'a [VideoId],
        duplicates: PlaylistAppendDuplicates,
    },
    Update {
        playlist: &'a PlaylistId,
        title: Option<&'a str>,
        description: Option<&'a str>,
        privacy: Option<PlaylistPrivacy>,
    },
    Move {
        playlist: &'a PlaylistId,
        entry_id: &'a str,
        before: Option<&'a str>,
    },
    Remove {
        playlist: &'a PlaylistId,
        entries: &'a [PlaylistRemoval],
    },
    Delete {
        playlist: &'a PlaylistId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddedPlaylistOccurrence {
    pub video_id: VideoId,
    pub entry_id: String,
}

/// Acknowledgments are provider responses. They still require a fresh read to prove the effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaylistMutationAck {
    Created(PlaylistId),
    Appended(Vec<AddedPlaylistOccurrence>),
    Accepted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogMutationError {
    InvalidInput,
    PermissionDenied,
    Rejected(QueryError),
    /// Side effects cannot be ruled out. The coordinator should reconcile instead of retrying.
    Unknown(QueryError),
}

fn invalid_music_response(message: &str) -> QueryError {
    make_query_error(QueryErrorCode::InvalidPayload, false, None, message)
}

pub(crate) fn unsupported_music_operation() -> QueryError {
    make_query_error(
        QueryErrorCode::UnsupportedCapability,
        false,
        None,
        "catalog backend does not provide this music operation",
    )
}

impl YtmapiCatalogBackend {
    pub(super) async fn read_fresh_playlist(
        &self,
        session: &Arc<dyn CatalogSession>,
        playlist: &PlaylistId,
        continuation: Option<&str>,
    ) -> Result<BackendFreshPlaylist, QueryError> {
        let yt = Self::downcast_session(session)?;
        let browse = ensure_vl_browse_id(playlist.as_str());
        let query = GetPlaylistTracksQuery::new(PlaylistID::from_raw(browse.as_str()));
        let json = if let Some(token) = continuation {
            fetch_continuation_page(yt, &query, token).await?
        } else {
            yt.json_query::<GetPlaylistTracksQuery<'_>>(&query)
                .await
                .map_err(classify_ytmapi_error)?
        };
        parse_fresh_playlist(playlist, &query, json, continuation.is_some())
    }

    pub(super) async fn read_song_radio(
        &self,
        session: &Arc<dyn CatalogSession>,
        seed: &VideoId,
        continuation: Option<&str>,
    ) -> Result<BackendPage<Track>, QueryError> {
        let yt = Self::downcast_session(session)?;
        let query = GetWatchPlaylistQuery::new_from_video_id(VideoID::from_raw(seed.as_str()));
        let (tracks, next) =
            run_continuable_page::<_, Vec<WatchPlaylistTrack>>(yt, &query, continuation).await?;
        normalize_song_radio(seed, tracks, next)
    }

    pub(super) async fn read_song_rating(
        &self,
        session: &Arc<dyn CatalogSession>,
        video: &VideoId,
    ) -> Result<SongRating, QueryError> {
        let yt = Self::downcast_session(session)?;
        let query = GetWatchPlaylistQuery::new_from_video_id(VideoID::from_raw(video.as_str()));
        let json = yt
            .json_query::<GetWatchPlaylistQuery<VideoID<'_>>>(query)
            .await
            .map_err(classify_ytmapi_error)?;
        Ok(song_rating_from_watch_next(video, &json.into_inner()))
    }

    /// Dispatches one typed provider write. This layer performs no automatic retry.
    pub async fn mutate_playlist_once(
        &self,
        session: &Arc<dyn CatalogSession>,
        request: PlaylistMutation<'_>,
    ) -> Result<PlaylistMutationAck, CatalogMutationError> {
        // Nothing has been sent when the session cannot be used.
        let yt = Self::downcast_session(session).map_err(CatalogMutationError::Rejected)?;
        match request {
            PlaylistMutation::CreatePrivate { title, description } => {
                let query = create_private_query(title, description)?;
                let result = yt.query(query).await.map_err(classify_mutation_error)?;
                playlist_id(result.get_raw())
                    .map(PlaylistMutationAck::Created)
                    .map_err(CatalogMutationError::Unknown)
            }
            PlaylistMutation::Append {
                playlist,
                videos,
                duplicates,
            } => {
                let query = append_query(playlist, videos, duplicates)?;
                let value =
                    yt.json_query::<AddPlaylistItemsQuery<
                        '_,
                        ytmapi_rs::query::playlist::AddVideosToPlaylist<'_>,
                    >>(query)
                        .await
                        .map_err(classify_mutation_error)?;
                parse_append_response(videos, &value.into_inner())
                    .map(PlaylistMutationAck::Appended)
            }
            PlaylistMutation::Delete { playlist } => {
                let query = DeletePlaylistQuery::new(validate_target(playlist)?);
                let value = yt
                    .json_query::<DeletePlaylistQuery<'_>>(query)
                    .await
                    .map_err(classify_mutation_error)?;
                mutation_status(&value.into_inner())?;
                Ok(PlaylistMutationAck::Accepted)
            }
            request => {
                let query = playlist_edit_query(request)?;
                let value = yt
                    .json_query::<PlaylistEdit<'_>>(query)
                    .await
                    .map_err(classify_mutation_error)?;
                mutation_status(&value.into_inner())?;
                Ok(PlaylistMutationAck::Accepted)
            }
        }
    }

    /// Configured identity is the same session used for catalogue reads and playlist mutations.
    /// The provider response is deliberately returned as no read-back evidence for ratings.
    pub async fn rate_song_once(
        &self,
        session: &Arc<dyn CatalogSession>,
        video: &VideoId,
        rating: SongRating,
    ) -> Result<(), CatalogMutationError> {
        // Nothing has been sent when the session cannot be used.
        let yt = Self::downcast_session(session).map_err(CatalogMutationError::Rejected)?;
        let response = yt
            .json_query::<RateSongQuery<'_>>(rating_query(video, rating)?)
            .await
            .map_err(classify_mutation_error)?;
        rating_mutation_status(&response.into_inner())
    }
}

fn song_rating_from_watch_next(video: &VideoId, response: &Value) -> SongRating {
    let Some(actions) = response.pointer(PLAYER_ACTIONS).and_then(Value::as_array) else {
        return SongRating::Unknown;
    };
    let mut matches = actions
        .iter()
        .filter_map(|action| action.get("likeButtonRenderer"))
        .filter(|renderer| string_at(renderer, "/target/videoId") == Some(video.as_str()))
        .map(|renderer| parse_rating(string_at(renderer, "/likeStatus")));
    let Some(observed) = matches.next() else {
        return SongRating::Unknown;
    };
    if matches.any(|rating| rating != observed) {
        return SongRating::Unknown;
    }
    observed
}

fn parse_rating(value: Option<&str>) -> SongRating {
    match value {
        Some("LIKE") => SongRating::Liked,
        Some("DISLIKE") => SongRating::Disliked,
        Some("INDIFFERENT") => SongRating::Neutral,
        _ => SongRating::Unknown,
    }
}

fn normalize_song_radio(
    seed: &VideoId,
    tracks: Vec<WatchPlaylistTrack>,
    next_continuation: Option<String>,
) -> Result<BackendPage<Track>, QueryError> {
    let mut seen = HashSet::from([seed.clone()]);
    let mut items = Vec::new();
    for track in tracks {
        let video_id = video_id(track.video_id.get_raw())?;
        if !seen.insert(video_id.clone()) {
            continue;
        }
        items.push(Track {
            video_id,
            title: track.title,
            artists: vec![ArtistRef {
                id: None,
                name: track.author,
            }],
            album: None,
            duration_ms: parse_duration_ms(&track.duration),
            cover: best_cover_ref(&track.thumbnails),
            availability: TrackAvailability::Unknown,
            version_tag: None,
            is_liked: None,
        });
    }
    Ok(BackendPage {
        items,
        next_continuation,
    })
}

fn parse_fresh_playlist(
    playlist: &PlaylistId,
    query: &GetPlaylistTracksQuery<'_>,
    json: Json,
    continuation: bool,
) -> Result<BackendFreshPlaylist, QueryError> {
    let value = serde_json::to_value(&json)
        .map_err(|_| invalid_music_response("playlist page could not be decoded"))?;
    let metadata = if continuation {
        None
    } else {
        Some(playlist_metadata(playlist, &value)?)
    };
    let raw_rows = value
        .pointer(if continuation {
            PLAYLIST_NEXT_ROWS
        } else {
            PLAYLIST_FIRST_ROWS
        })
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let (items, next_continuation) = parse_playlist_items_page(query, json, continuation)?;
    let mut items = items.into_iter().peekable();
    let mut entries = Vec::new();
    let mut source_entries = 0;
    for (index, item) in raw_rows.iter().enumerate() {
        if item.get("continuationItemRenderer").is_some() {
            continue;
        }
        let position = source_entries;
        source_entries += 1;
        let Some(row) = item.get("musicResponsiveListItemRenderer") else {
            continue;
        };
        let remove = remove_action(row);
        let track = if items
            .peek()
            .is_some_and(|item| item_index(item) == index + 1)
        {
            normalize_playlist_item(items.next().expect("checked item index"))?
        } else if let Some(track) = unavailable_playlist_track(row, remove)? {
            track
        } else {
            continue;
        };
        let entry_id = remove
            .filter(|action| string_at(action, "/removedVideoId") == Some(track.video_id.as_str()))
            .and_then(|action| string_at(action, "/setVideoId"))
            .filter(|id| validate_id_str(id).is_ok())
            .map(str::to_owned);
        let rating = parse_rating(string_at(
            row,
            "/menu/menuRenderer/topLevelButtons/0/likeButtonRenderer/likeStatus",
        ));
        let position = u32::try_from(position)
            .map_err(|_| invalid_music_response("playlist page position exceeded bounds"))?;
        entries.push(PlaylistOccurrence {
            entry_id,
            position,
            track,
            rating,
        });
    }
    Ok(BackendFreshPlaylist {
        metadata,
        entries,
        next_continuation,
        source_entries,
    })
}

fn item_index(item: &PlaylistItem) -> usize {
    match item {
        PlaylistItem::Song(item) => item.track_no,
        PlaylistItem::Video(item) => item.track_no,
        PlaylistItem::Episode(item) => item.track_no,
        PlaylistItem::UploadSong(item) => item.track_no,
    }
}

fn unavailable_playlist_track(
    row: &Value,
    remove: Option<&Value>,
) -> Result<Option<Track>, QueryError> {
    let Some(id) = remove
        .and_then(|action| string_at(action, "/removedVideoId"))
        .or_else(|| string_at(row, "/playlistItemData/videoId"))
    else {
        return Ok(None);
    };
    let Some(title) = string_at(
        row,
        "/flexColumns/0/musicResponsiveListItemFlexColumnRenderer/text/runs/0/text",
    ) else {
        return Ok(None);
    };
    Ok(Some(Track {
        video_id: video_id(id)?,
        title: title.to_owned(),
        artists: Vec::new(),
        album: None,
        duration_ms: None,
        cover: None,
        availability: TrackAvailability::Unavailable,
        version_tag: None,
        is_liked: None,
    }))
}

fn playlist_metadata(playlist: &PlaylistId, value: &Value) -> Result<PlaylistMetadata, QueryError> {
    let section = value.pointer(PLAYLIST_HEADER).unwrap_or(&Value::Null);
    let requested_id = strip_vl_prefix(playlist.as_str());
    let shelf_id = string_at(value, PLAYLIST_SHELF_ID);
    if shelf_id.is_some_and(|id| strip_vl_prefix(id) != requested_id) {
        return Err(invalid_music_response(
            "playlist shelf returned a different ID",
        ));
    }
    let (header, privacy, editable) = if let Some(editable) = section.get(EDITABLE_HEADER) {
        let id = string_at(editable, "/playlistId");
        if let Some(id) = id
            && strip_vl_prefix(id) != strip_vl_prefix(playlist.as_str())
        {
            return Err(invalid_music_response(
                "playlist metadata returned a different ID",
            ));
        }
        (
            editable.pointer("/header/musicResponsiveHeaderRenderer"),
            string_at(
                editable,
                "/editHeader/musicPlaylistEditHeaderRenderer/privacy",
            )
            .and_then(PlaylistPrivacy::from_inner_tube),
            id.map(|_| true),
        )
    } else {
        (section.get("musicResponsiveHeaderRenderer"), None, None)
    };
    let metadata = PlaylistMetadata {
        playlist_id: playlist.clone(),
        identity_confirmed: shelf_id.is_some() || editable == Some(true),
        title: join_runs(header.and_then(|h| h.pointer("/title/runs")))
            .ok_or_else(|| invalid_music_response("playlist metadata did not include a title"))?,
        description: join_runs(header.and_then(|h| {
            h.pointer("/description/musicDescriptionShelfRenderer/description/runs")
        })),
        privacy,
        editable,
    };
    Ok(metadata)
}

fn remove_action(row: &Value) -> Option<&Value> {
    row.pointer("/menu/menuRenderer/items")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(|item| {
            item.pointer("/menuServiceItemRenderer/serviceEndpoint/playlistEditEndpoint/actions")
                .and_then(Value::as_array)
        })
        .flatten()
        .find(|action| string_at(action, "/action") == Some("ACTION_REMOVE_VIDEO"))
}

fn join_runs(runs: Option<&Value>) -> Option<String> {
    let runs = runs?.as_array()?;
    Some(
        runs.iter()
            .filter_map(|run| string_at(run, "/text"))
            .collect(),
    )
}

fn string_at<'a>(value: &'a Value, pointer: &str) -> Option<&'a str> {
    value.pointer(pointer)?.as_str()
}

fn video_id(id: &str) -> Result<VideoId, QueryError> {
    VideoId::new(id).map_err(|_| invalid_music_response("provider returned an invalid video ID"))
}

fn playlist_id(id: &str) -> Result<PlaylistId, QueryError> {
    PlaylistId::new(strip_vl_prefix(id))
        .map_err(|_| invalid_music_response("provider returned an invalid playlist ID"))
}

fn validate_target(playlist: &PlaylistId) -> Result<PlaylistID<'_>, CatalogMutationError> {
    let id = strip_vl_prefix(playlist.as_str());
    if id.is_empty() || is_system_library_playlist(id) {
        return Err(CatalogMutationError::InvalidInput);
    }
    Ok(PlaylistID::from_raw(id))
}

fn validate_title(title: &str) -> Result<(), CatalogMutationError> {
    if title.trim().is_empty() || title.contains(['<', '>']) {
        return Err(CatalogMutationError::InvalidInput);
    }
    Ok(())
}

fn create_private_query<'a>(
    title: &'a str,
    description: Option<&'a str>,
) -> Result<
    CreatePlaylistQuery<'a, ytmapi_rs::query::playlist::BasicCreatePlaylist>,
    CatalogMutationError,
> {
    validate_title(title)?;
    Ok(CreatePlaylistQuery::new(
        title,
        description,
        PrivacyStatus::Private,
    ))
}

fn append_query<'a>(
    playlist: &'a PlaylistId,
    videos: &'a [VideoId],
    duplicates: PlaylistAppendDuplicates,
) -> Result<
    AddPlaylistItemsQuery<'a, ytmapi_rs::query::playlist::AddVideosToPlaylist<'a>>,
    CatalogMutationError,
> {
    validate_batch(videos)?;
    let mode = match duplicates {
        PlaylistAppendDuplicates::Allow => DuplicateHandlingMode::Unhandled,
        PlaylistAppendDuplicates::Reject => {
            if videos.iter().collect::<HashSet<_>>().len() != videos.len() {
                return Err(CatalogMutationError::InvalidInput);
            }
            DuplicateHandlingMode::ReturnError
        }
    };
    Ok(AddPlaylistItemsQuery::new_from_videos(
        validate_target(playlist)?,
        videos.iter().map(|video| VideoID::from_raw(video.as_str())),
        mode,
    ))
}

fn validate_batch<T>(items: &[T]) -> Result<(), CatalogMutationError> {
    if items.is_empty() || items.len() > MAX_MUTATION_ITEMS {
        return Err(CatalogMutationError::InvalidInput);
    }
    Ok(())
}

fn validate_occurrence_id(id: &str) -> Result<(), CatalogMutationError> {
    validate_id_str(id).map_err(|_| CatalogMutationError::InvalidInput)
}

fn normalize_added_entries(
    requested: &[VideoId],
    response: Vec<AddPlaylistItem>,
) -> Result<Vec<AddedPlaylistOccurrence>, CatalogMutationError> {
    let mut occurrences = HashSet::new();
    let mut remaining = std::collections::HashMap::new();
    for id in requested {
        *remaining.entry(id.as_str()).or_insert(0_u32) += 1;
    }
    response
        .into_iter()
        .map(|item| {
            let id = item.video_id.get_raw();
            let entry_id = item.set_video_id.get_raw();
            let Some(count) = remaining.get_mut(id) else {
                return Err(unknown_response(
                    "provider acknowledged an unrequested video",
                ));
            };
            if *count == 0
                || validate_id_str(entry_id).is_err()
                || !occurrences.insert(entry_id.to_owned())
            {
                return Err(unknown_response(
                    "provider returned inconsistent playlist append acknowledgments",
                ));
            }
            *count -= 1;
            Ok(AddedPlaylistOccurrence {
                video_id: video_id(id).map_err(CatalogMutationError::Unknown)?,
                entry_id: entry_id.to_owned(),
            })
        })
        .collect()
}

/// The one entry an add of `video` created, when the provider's acknowledgement names exactly
/// one valid entry for that song and nothing else.
pub(super) fn single_created_entry(video: &VideoId, added: Vec<AddPlaylistItem>) -> Option<String> {
    match normalize_added_entries(std::slice::from_ref(video), added)
        .ok()?
        .as_slice()
    {
        [entry] => Some(entry.entry_id.clone()),
        _ => None,
    }
}

fn parse_append_response(
    requested: &[VideoId],
    response: &Value,
) -> Result<Vec<AddedPlaylistOccurrence>, CatalogMutationError> {
    let Some(results) = response
        .get("playlistEditResults")
        .and_then(Value::as_array)
    else {
        return Err(unknown_response(
            "provider returned no playlist append results",
        ));
    };
    // Individual successful actions can identify occurrences even if another action failed.
    let entries = results
        .iter()
        .filter_map(|result| result.get("playlistEditVideoAddedResultData"))
        .filter_map(|entry| {
            Some(AddPlaylistItem {
                video_id: VideoID::from_raw(string_at(entry, "/videoId")?.to_owned()),
                set_video_id: SetVideoID::from_raw(string_at(entry, "/setVideoId")?.to_owned()),
            })
        })
        .collect::<Vec<AddPlaylistItem>>();
    if entries.is_empty() {
        mutation_status(response)?;
    }
    normalize_added_entries(requested, entries)
}

/// Narrow InnerTube edits. The SDK's privacy action is duplicated and serializes a mixed-case
/// enum, and its remove request lacks the video ID associated with the occurrence.
struct PlaylistEdit<'a> {
    playlist: PlaylistID<'a>,
    actions: Vec<Value>,
}

impl Query<BrowserToken> for PlaylistEdit<'_> {
    type Output = NoopParseOutput;
    type Method = PostMethod;
}

impl PostQuery for PlaylistEdit<'_> {
    fn header(&self) -> Map<String, Value> {
        Map::from_iter([
            ("playlistId".to_owned(), json!(self.playlist.get_raw())),
            ("actions".to_owned(), json!(self.actions)),
        ])
    }

    fn path(&self) -> &str {
        "browse/edit_playlist"
    }

    fn params(&self) -> Vec<(&str, Cow<'_, str>)> {
        Vec::new()
    }
}

impl ParseFrom<PlaylistEdit<'_>> for NoopParseOutput {
    fn parse_from(_: ProcessedResult<PlaylistEdit<'_>>) -> ytmapi_rs::Result<Self> {
        Ok(Self)
    }
}

fn playlist_edit_query(
    request: PlaylistMutation<'_>,
) -> Result<PlaylistEdit<'_>, CatalogMutationError> {
    let (playlist, actions) = match request {
        PlaylistMutation::Update {
            playlist,
            title,
            description,
            privacy,
        } => {
            let mut actions = Vec::new();
            if let Some(title) = title {
                validate_title(title)?;
                actions.push(json!({"action": "ACTION_SET_PLAYLIST_NAME", "playlistName": title}));
            }
            if let Some(description) = description {
                actions.push(json!({"action": "ACTION_SET_PLAYLIST_DESCRIPTION", "playlistDescription": description}));
            }
            if let Some(privacy) = privacy {
                actions.push(json!({"action": "ACTION_SET_PLAYLIST_PRIVACY", "playlistPrivacy": privacy.as_inner_tube()}));
            }
            validate_batch(&actions)?;
            (playlist, actions)
        }
        PlaylistMutation::Move {
            playlist,
            entry_id,
            before,
        } => {
            validate_occurrence_id(entry_id)?;
            if let Some(anchor) = before {
                validate_occurrence_id(anchor)?;
                if anchor == entry_id {
                    return Err(CatalogMutationError::InvalidInput);
                }
            }
            let mut body = json!({"action": "ACTION_MOVE_VIDEO_BEFORE", "setVideoId": entry_id});
            if let Some(anchor) = before {
                body["movedSetVideoIdSuccessor"] = json!(anchor);
            }
            (playlist, vec![body])
        }
        PlaylistMutation::Remove { playlist, entries } => {
            validate_batch(entries)?;
            let mut occurrences = HashSet::new();
            let mut actions = Vec::with_capacity(entries.len());
            for item in entries {
                validate_occurrence_id(&item.entry_id)?;
                if !occurrences.insert(&item.entry_id) {
                    return Err(CatalogMutationError::InvalidInput);
                }
                actions.push(json!({
                    "action": "ACTION_REMOVE_VIDEO",
                    "setVideoId": item.entry_id,
                    "removedVideoId": item.video_id.as_str(),
                }));
            }
            (playlist, actions)
        }
        _ => return Err(CatalogMutationError::InvalidInput),
    };
    Ok(PlaylistEdit {
        playlist: validate_target(playlist)?,
        actions,
    })
}

fn rating_query<'a>(
    video: &'a VideoId,
    rating: SongRating,
) -> Result<RateSongQuery<'a>, CatalogMutationError> {
    let rating = match rating {
        SongRating::Liked => LikeStatus::Liked,
        SongRating::Disliked => LikeStatus::Disliked,
        SongRating::Neutral => LikeStatus::Indifferent,
        SongRating::Unknown => return Err(CatalogMutationError::InvalidInput),
    };
    Ok(RateSongQuery::new(
        VideoID::from_raw(video.as_str()),
        rating,
    ))
}

fn mutation_status(value: &Value) -> Result<(), CatalogMutationError> {
    let status = value
        .get("status")
        .cloned()
        .and_then(|status| serde_json::from_value(status).ok())
        .ok_or_else(|| unknown_response("provider returned no explicit mutation status"))?;
    require_mutation_success(status)
}

fn rating_mutation_status(value: &Value) -> Result<(), CatalogMutationError> {
    if value.get("status").is_none()
        && value
            .get("actions")
            .and_then(Value::as_array)
            .is_some_and(|actions| {
                actions
                    .iter()
                    .any(|action| action.get("addToToastAction").is_some())
            })
    {
        // The provider response acknowledged the action. Only direct watch-next readback proves
        // the rating for the requested video ID.
        return Ok(());
    }
    mutation_status(value)
}

fn require_mutation_success(status: ApiOutcome) -> Result<(), CatalogMutationError> {
    match status {
        ApiOutcome::Success => Ok(()),
        ApiOutcome::Failure => Err(unknown_response(
            "provider reported a mutation failure without per-action results",
        )),
    }
}

fn unknown_response(message: &str) -> CatalogMutationError {
    CatalogMutationError::Unknown(invalid_music_response(message))
}

pub(super) fn classify_mutation_error(error: ytmapi_rs::Error) -> CatalogMutationError {
    classify_mutation_error_kind(error.into_kind())
}

fn classify_mutation_error_kind(kind: ErrorKind) -> CatalogMutationError {
    if let ErrorKind::OtherErrorCodeInResponse { code: 403, message } = &kind {
        let message = message.to_ascii_lowercase();
        if message.contains("permission") && !message.contains("login_required") {
            return CatalogMutationError::PermissionDenied;
        }
        if !message.contains("login_required")
            && !message.contains("unauthenticated")
            && !message.contains("quota")
            && !message.contains("rate")
            && !message.contains("limit")
        {
            return unknown_response(
                "provider denied the mutation without an authentication reason",
            );
        }
    }
    if matches!(
        kind,
        ErrorKind::OtherErrorCodeInResponse {
            code: 401 | 403 | 429,
            ..
        } | ErrorKind::Header
            | ErrorKind::OAuthTokenExpired { .. }
    ) {
        return CatalogMutationError::Rejected(classify_ytmapi_error_kind(kind));
    }
    let mut error = classify_ytmapi_error_kind(kind);
    error.retriable = false;
    CatalogMutationError::Unknown(error)
}

#[cfg(test)]
mod tests;
