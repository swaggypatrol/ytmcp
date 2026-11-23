use serde::Serialize;
use ytm_protocol::{ArtistRef, TrackAvailability};
use ytmapi_rs::common::{ApiOutcome, FeedbackTokenAddToLibrary, FeedbackTokenRemoveFromLibrary};
use ytmapi_rs::parse::{HistoryItem, LibraryArtist, LibraryArtistSubscription, TableListSong};
use ytmapi_rs::query::{
    EditSongLibraryStatusQuery, GetLibraryAlbumsQuery, GetLibraryArtistSubscriptionsQuery,
    GetLibraryArtistsQuery, GetLibrarySongsQuery, GetWatchPlaylistQuery, RatePlaylistQuery,
    SubscribeArtistQuery, UnsubscribeArtistsQuery,
};

use super::music::classify_mutation_error;
use super::*;
use crate::probe::normalize::{
    like_status_to_bool, map_parsed_album, map_parsed_artists, normalize_history_item,
    parse_duration_ms, video_id_or_err,
};

mod album_membership;
mod saved_albums;
pub use album_membership::BackendAlbumMembership;

const SUBSCRIPTION_BUTTON: &str =
    "/header/musicImmersiveHeaderRenderer/subscriptionButton/subscribeButtonRenderer";
const WATCH_ITEMS: &str = concat!(
    "/contents/singleColumnMusicWatchNextResultsRenderer/tabbedRenderer",
    "/watchNextTabbedResultsRenderer/tabs/0/tabRenderer/content/musicQueueRenderer",
    "/content/playlistPanelRenderer/contents"
);
const PLAYLIST_SHELVES: &str =
    "/contents/twoColumnBrowseResultsRenderer/secondaryContents/sectionListRenderer/contents";
const PLAYLIST_HEADERS: &str = concat!(
    "/contents/twoColumnBrowseResultsRenderer/tabs/0/tabRenderer/content",
    "/sectionListRenderer/contents"
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LibrarySurface {
    Songs,
    Albums,
    Artists,
    Subscriptions,
    History,
}

impl LibrarySurface {
    pub const fn source(self) -> &'static str {
        match self {
            Self::Songs => "youtube_music_library_songs",
            Self::Albums => "youtube_music_library_albums",
            Self::Artists => "youtube_music_library_artists",
            Self::Subscriptions => "youtube_music_artist_subscriptions",
            Self::History => "youtube_music_recent_history",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum ProviderLibraryItem {
    Song(Track),
    Album {
        album_id: AlbumId,
        title: String,
        artists: Vec<ArtistRef>,
    },
    Artist {
        artist_id: ArtistId,
        name: String,
        byline: Option<String>,
    },
    History {
        track: Track,
        period: String,
    },
    HistoryEpisode {
        episode_id: BrowseId,
        title: String,
        period: String,
    },
}

impl YtmapiCatalogBackend {
    pub(super) async fn read_library_page(
        &self,
        session: &Arc<dyn CatalogSession>,
        surface: LibrarySurface,
        continuation: Option<&str>,
    ) -> Result<BackendPage<ProviderLibraryItem>, QueryError> {
        let yt = Self::downcast_session(session)?;
        match surface {
            LibrarySurface::Songs => {
                read_paginated(
                    yt,
                    &GetLibrarySongsQuery::default(),
                    continuation,
                    project_song,
                )
                .await
            }
            LibrarySurface::Albums => saved_albums::read_page(yt, continuation).await,
            LibrarySurface::Artists => {
                read_paginated(
                    yt,
                    &GetLibraryArtistsQuery::default(),
                    continuation,
                    project_artist,
                )
                .await
            }
            LibrarySurface::Subscriptions => {
                read_paginated(
                    yt,
                    &GetLibraryArtistSubscriptionsQuery::default(),
                    continuation,
                    project_subscription,
                )
                .await
            }
            LibrarySurface::History => {
                if continuation.is_some() {
                    return Err(make_query_error(
                        QueryErrorCode::UnsupportedCapability,
                        false,
                        None,
                        "recent history does not use a provider continuation",
                    ));
                }
                let periods = yt
                    .query(GetHistoryQuery)
                    .await
                    .map_err(classify_ytmapi_error)?;
                let items = project_history(periods)?;
                Ok(BackendPage {
                    items,
                    next_continuation: None,
                })
            }
        }
    }

    pub(super) async fn read_song_membership(
        &self,
        session: &Arc<dyn CatalogSession>,
        video: &VideoId,
    ) -> Result<Option<bool>, QueryError> {
        let yt = Self::downcast_session(session)?;
        let response = song_membership_response(yt, video).await?;
        Ok(song_membership(video, &response))
    }

    pub(super) async fn write_song_membership_once(
        &self,
        session: &Arc<dyn CatalogSession>,
        video: &VideoId,
        saved: bool,
    ) -> Result<(), CatalogMutationError> {
        let yt = Self::downcast_session(session).map_err(CatalogMutationError::Rejected)?;
        let response = song_membership_response(yt, video)
            .await
            .map_err(CatalogMutationError::Rejected)?;
        let proof = song_membership_proof(video, &response).ok_or_else(membership_not_available)?;
        if proof.action.saved == saved {
            return Ok(());
        }
        if !proof.unique_action {
            return Err(membership_not_available());
        }
        let action = song_membership_query(proof.action, saved);
        let outcome = yt.query(action).await.map_err(classify_mutation_error)?;
        song_membership_ack(&outcome)
    }

    pub(super) async fn read_playlist_membership(
        &self,
        session: &Arc<dyn CatalogSession>,
        playlist: &PlaylistId,
    ) -> Result<Option<bool>, QueryError> {
        let id = ordinary_playlist_id(playlist)?;
        let yt = Self::downcast_session(session)?;
        let response = playlist_membership_response(yt, id).await?;
        Ok(playlist_membership(id, &response))
    }

    pub(super) async fn write_playlist_membership_once(
        &self,
        session: &Arc<dyn CatalogSession>,
        playlist: &PlaylistId,
        saved: bool,
    ) -> Result<(), CatalogMutationError> {
        let id = ordinary_playlist_id(playlist).map_err(CatalogMutationError::Rejected)?;
        let yt = Self::downcast_session(session).map_err(CatalogMutationError::Rejected)?;
        let response = playlist_membership_response(yt, id)
            .await
            .map_err(CatalogMutationError::Rejected)?;
        let current = playlist_membership(id, &response).ok_or_else(membership_not_available)?;
        if current == saved {
            return Ok(());
        }
        yt.query(playlist_membership_query(id, saved))
            .await
            .map_err(classify_mutation_error)
    }

    pub(super) async fn read_artist_subscription(
        &self,
        session: &Arc<dyn CatalogSession>,
        artist: &ArtistId,
    ) -> Result<Option<bool>, QueryError> {
        let yt = Self::downcast_session(session)?;
        let channel = ArtistChannelID::from_raw(artist_channel(artist)?);
        let query = GetArtistQuery::new(channel);
        let json = yt
            .json_query::<GetArtistQuery<'_>>(query)
            .await
            .map_err(classify_ytmapi_error)?;
        Ok(artist_subscription(artist, &json.into_inner()))
    }

    pub(super) async fn write_artist_subscription_once(
        &self,
        session: &Arc<dyn CatalogSession>,
        artist: &ArtistId,
        subscribed: bool,
    ) -> Result<(), CatalogMutationError> {
        let yt = Self::downcast_session(session).map_err(CatalogMutationError::Rejected)?;
        let raw = artist_channel(artist).map_err(CatalogMutationError::Rejected)?;
        let channel = ArtistChannelID::from_raw(raw);
        if subscribed {
            yt.query(SubscribeArtistQuery::new(channel))
                .await
                .map_err(classify_mutation_error)
        } else {
            yt.query(UnsubscribeArtistsQuery::new([channel]))
                .await
                .map_err(classify_mutation_error)
        }
    }
}

async fn read_paginated<Q, T>(
    yt: &YtMusic<BrowserToken>,
    query: &Q,
    continuation: Option<&str>,
    project: fn(T) -> Result<ProviderLibraryItem, QueryError>,
) -> Result<BackendPage<ProviderLibraryItem>, QueryError>
where
    Q: PostQuery + Query<BrowserToken>,
    Vec<T>: ParseFromContinuable<Q> + Send,
{
    let (items, next_continuation) =
        run_continuable_page::<_, Vec<T>>(yt, query, continuation).await?;
    let items = items
        .into_iter()
        .map(project)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(BackendPage {
        items,
        next_continuation,
    })
}

fn project_song(song: TableListSong) -> Result<ProviderLibraryItem, QueryError> {
    Ok(ProviderLibraryItem::Song(Track {
        video_id: video_id_or_err(song.video_id.get_raw())?,
        title: song.title,
        artists: map_parsed_artists(&song.artists),
        album: map_parsed_album(&song.album),
        duration_ms: parse_duration_ms(&song.duration),
        cover: None,
        availability: if song.is_available {
            TrackAvailability::Available
        } else {
            TrackAvailability::Unavailable
        },
        version_tag: None,
        is_liked: like_status_to_bool(&song.like_status),
    }))
}

fn project_artist(artist: LibraryArtist) -> Result<ProviderLibraryItem, QueryError> {
    Ok(ProviderLibraryItem::Artist {
        artist_id: ArtistId::new(artist.channel_id.get_raw()).map_err(|_| invalid_id())?,
        name: artist.artist,
        byline: Some(artist.byline).filter(|label| !label.is_empty()),
    })
}

fn project_subscription(
    artist: LibraryArtistSubscription,
) -> Result<ProviderLibraryItem, QueryError> {
    Ok(ProviderLibraryItem::Artist {
        artist_id: ArtistId::new(artist.channel_id.get_raw()).map_err(|_| invalid_id())?,
        name: artist.name,
        byline: Some(artist.subscribers).filter(|label| !label.is_empty()),
    })
}

fn project_history(periods: Vec<HistoryPeriod>) -> Result<Vec<ProviderLibraryItem>, QueryError> {
    let mut entries = Vec::new();
    for period in periods {
        for item in period.items {
            let period = period.period_name.clone();
            let entry = match item {
                HistoryItem::Episode(episode) => ProviderLibraryItem::HistoryEpisode {
                    episode_id: BrowseId::new(episode.episode_id.get_raw())
                        .map_err(|_| invalid_id())?,
                    title: episode.title,
                    period,
                },
                item => {
                    let mut track = normalize_history_item(item)?;
                    track.cover = None;
                    ProviderLibraryItem::History { track, period }
                }
            };
            entries.push(entry);
        }
    }
    Ok(entries)
}

fn invalid_id() -> QueryError {
    make_query_error(
        QueryErrorCode::InvalidPayload,
        false,
        None,
        "provider returned an invalid library ID",
    )
}

fn artist_channel(artist: &ArtistId) -> Result<&str, QueryError> {
    let raw = artist.as_str();
    if raw.strip_prefix("UC").is_none_or(str::is_empty) {
        return Err(make_query_error(
            QueryErrorCode::InvalidPayload,
            false,
            None,
            "artist subscription requires the provider's channel ID",
        ));
    }
    Ok(raw)
}

fn artist_subscription(artist: &ArtistId, value: &Value) -> Option<bool> {
    let button = value.pointer(SUBSCRIPTION_BUTTON)?;
    (button.get("channelId")?.as_str()? == artist.as_str())
        .then_some(button.get("subscribed")?.as_bool()?)
}

#[derive(PartialEq, Eq)]
struct SongLibraryAction<'a> {
    saved: bool,
    default_token: &'a str,
    toggled_token: &'a str,
}

struct SongLibraryProof<'a> {
    action: SongLibraryAction<'a>,
    unique_action: bool,
}

async fn song_membership_response(
    yt: &YtMusic<BrowserToken>,
    video: &VideoId,
) -> Result<Value, QueryError> {
    let query = GetWatchPlaylistQuery::new_from_video_id(VideoID::from_raw(video.as_str()));
    yt.json_query::<GetWatchPlaylistQuery<VideoID<'_>>>(query)
        .await
        .map(Json::into_inner)
        .map_err(classify_ytmapi_error)
}

fn song_membership(video: &VideoId, value: &Value) -> Option<bool> {
    song_membership_proof(video, value).map(|proof| proof.action.saved)
}

fn song_membership_proof<'a>(video: &VideoId, value: &'a Value) -> Option<SongLibraryProof<'a>> {
    let rows = value.pointer(WATCH_ITEMS)?.as_array()?;
    let mut proof: Option<SongLibraryProof<'_>> = None;
    for row in rows.iter().filter_map(|row| {
        row.get("playlistPanelVideoRenderer").or_else(|| {
            row.pointer(
                "/playlistPanelVideoWrapperRenderer/primaryRenderer/playlistPanelVideoRenderer",
            )
        })
    }) {
        let row_id = row.get("videoId").and_then(Value::as_str);
        let watch_id = text_at(row, "/navigationEndpoint/watchEndpoint/videoId");
        let expected = Some(video.as_str());
        if row_id != expected || watch_id != expected {
            if row_id.is_some()
                && watch_id.is_some()
                && (row_id == expected || watch_id == expected)
            {
                return None;
            }
            continue;
        }
        let toggles = row
            .pointer("/menu/menuRenderer/items")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| item.get("toggleMenuServiceItemRenderer"));
        for toggle in toggles {
            let icons = library_icons(toggle);
            if !has_library_icon(icons) {
                continue;
            }
            let saved = match icons {
                (Some("LIBRARY_ADD"), Some("LIBRARY_SAVED")) => false,
                (Some("LIBRARY_SAVED"), Some("LIBRARY_ADD")) => true,
                _ => return None,
            };
            let default_token = text_at(
                toggle,
                "/defaultServiceEndpoint/feedbackEndpoint/feedbackToken",
            )?;
            let toggled_token = text_at(
                toggle,
                "/toggledServiceEndpoint/feedbackEndpoint/feedbackToken",
            )?;
            if default_token == toggled_token
                || [default_token, toggled_token]
                    .iter()
                    .any(|token| token.is_empty() || token.chars().any(char::is_whitespace))
            {
                return None;
            }
            let action = SongLibraryAction {
                saved,
                default_token,
                toggled_token,
            };
            match &mut proof {
                Some(current) if current.action.saved != saved => return None,
                Some(current) => current.unique_action &= current.action == action,
                None => {
                    proof = Some(SongLibraryProof {
                        action,
                        unique_action: true,
                    })
                }
            }
        }
    }
    proof
}

fn song_membership_query(
    action: SongLibraryAction<'_>,
    saved: bool,
) -> EditSongLibraryStatusQuery<'_> {
    debug_assert_ne!(action.saved, saved);
    if saved {
        EditSongLibraryStatusQuery::new_from_add_to_library_feedback_tokens([
            FeedbackTokenAddToLibrary::from_raw(action.default_token),
        ])
    } else {
        EditSongLibraryStatusQuery::new_from_remove_from_library_feedback_tokens([
            FeedbackTokenRemoveFromLibrary::from_raw(action.default_token),
        ])
    }
}

fn song_membership_ack(outcome: &[ApiOutcome]) -> Result<(), CatalogMutationError> {
    match outcome {
        [ApiOutcome::Success] => Ok(()),
        _ => Err(CatalogMutationError::Unknown(make_query_error(
            QueryErrorCode::InvalidPayload,
            false,
            None,
            "provider did not acknowledge processing exactly one library action",
        ))),
    }
}

fn ordinary_playlist_id(playlist: &PlaylistId) -> Result<&str, QueryError> {
    let raw = playlist.as_str();
    if raw.strip_prefix("PL").is_some_and(|suffix| {
        !suffix.is_empty()
            && suffix
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    }) {
        return Ok(raw);
    }
    Err(make_query_error(
        QueryErrorCode::UnsupportedCapability,
        false,
        None,
        "playlist membership requires an ordinary canonical playlist ID",
    ))
}

async fn playlist_membership_response(
    yt: &YtMusic<BrowserToken>,
    id: &str,
) -> Result<Value, QueryError> {
    let browse_id = ensure_vl_browse_id(id);
    let query = GetPlaylistDetailsQuery::new(PlaylistID::from_raw(&browse_id));
    yt.json_query::<GetPlaylistDetailsQuery<'_>>(query)
        .await
        .map(Json::into_inner)
        .map_err(classify_ytmapi_error)
}

fn playlist_membership(id: &str, value: &Value) -> Option<bool> {
    let mut shelves = value
        .pointer(PLAYLIST_SHELVES)?
        .as_array()?
        .iter()
        .filter_map(|row| row.get("musicPlaylistShelfRenderer"));
    let shelf_id = text_at(shelves.next()?, "/playlistId")?;
    if strip_vl_prefix(shelf_id) != id || shelves.next().is_some() {
        return None;
    }
    let headers = value.pointer(PLAYLIST_HEADERS)?.as_array()?;
    if headers.iter().any(|header| {
        header
            .get("musicEditablePlaylistDetailHeaderRenderer")
            .is_some()
    }) {
        return None;
    }
    let buttons = headers
        .iter()
        .filter_map(|header| {
            header
                .pointer("/musicResponsiveHeaderRenderer/buttons")?
                .as_array()
        })
        .flatten();
    library_button_state(buttons, id)
}

fn library_button_state<'a>(buttons: impl Iterator<Item = &'a Value>, id: &str) -> Option<bool> {
    let mut saved = None;
    for toggle in buttons.filter_map(|button| button.get("toggleButtonRenderer")) {
        let icons = library_icons(toggle);
        if ![icons.0, icons.1].into_iter().any(|icon| {
            matches!(
                icon,
                Some("LIBRARY_ADD" | "LIBRARY_SAVED" | "BOOKMARK_BORDER" | "BOOKMARK")
            )
        }) {
            continue;
        }
        if !matches!(
            icons,
            (Some("LIBRARY_ADD"), Some("LIBRARY_SAVED"))
                | (Some("BOOKMARK_BORDER"), Some("BOOKMARK"))
        ) || toggle.get("isDisabled").and_then(Value::as_bool) != Some(false)
            || !playlist_like_action(
                toggle.pointer("/defaultServiceEndpoint/likeEndpoint"),
                id,
                "LIKE",
            )
            || !playlist_like_action(
                toggle.pointer("/toggledServiceEndpoint/likeEndpoint"),
                id,
                "INDIFFERENT",
            )
            || saved.is_some()
        {
            return None;
        }
        saved = Some(toggle.get("isToggled")?.as_bool()?);
    }
    saved
}

fn playlist_like_action(action: Option<&Value>, id: &str, status: &str) -> bool {
    action.is_some_and(|action| {
        text_at(action, "/status") == Some(status)
            && action
                .get("target")
                .and_then(Value::as_object)
                .is_some_and(|target| {
                    target.len() == 1
                        && target.get("playlistId").and_then(Value::as_str) == Some(id)
                })
    })
}

fn playlist_membership_query(id: &str, saved: bool) -> RatePlaylistQuery<'_> {
    let status = if saved {
        LikeStatus::Liked
    } else {
        LikeStatus::Indifferent
    };
    RatePlaylistQuery::new(PlaylistID::from_raw(id), status)
}

fn text_at<'a>(value: &'a Value, pointer: &str) -> Option<&'a str> {
    value.pointer(pointer)?.as_str()
}

fn library_icons(toggle: &Value) -> (Option<&str>, Option<&str>) {
    (
        text_at(toggle, "/defaultIcon/iconType"),
        text_at(toggle, "/toggledIcon/iconType"),
    )
}

fn has_library_icon((default, toggled): (Option<&str>, Option<&str>)) -> bool {
    matches!(default, Some("LIBRARY_ADD" | "LIBRARY_SAVED"))
        || matches!(toggled, Some("LIBRARY_ADD" | "LIBRARY_SAVED"))
}

fn membership_not_available() -> CatalogMutationError {
    CatalogMutationError::Rejected(make_query_error(
        QueryErrorCode::UnsupportedCapability,
        false,
        None,
        "provider did not expose a unique library action for the requested target",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn subscription_readback_requires_exact_channel_and_explicit_state() {
        let artist = ArtistId::new("UCfixture").unwrap();
        let response = |id, state| {
            json!({"header": {"musicImmersiveHeaderRenderer": {
                "subscriptionButton": {"subscribeButtonRenderer": {
                    "channelId": id, "subscribed": state
                }}
            }}})
        };
        assert_eq!(
            artist_subscription(&artist, &response("UCfixture", false)),
            Some(false)
        );
        assert_eq!(
            artist_subscription(&artist, &response("UCfixture", true)),
            Some(true)
        );
        assert!(artist_subscription(&artist, &response("UCother", true)).is_none());
        assert!(artist_subscription(&artist, &json!({})).is_none());
        assert!(artist_channel(&ArtistId::new("MPLAfixture").unwrap()).is_err());

        let add = SubscribeArtistQuery::new(ArtistChannelID::from_raw(artist.as_str()));
        let remove = UnsubscribeArtistsQuery::new([ArtistChannelID::from_raw(artist.as_str())]);
        assert_eq!(add.path(), "subscription/subscribe");
        assert_eq!(remove.path(), "subscription/unsubscribe");
        assert_eq!(add.header()["channelIds"], json!(["UCfixture"]));
        assert_eq!(remove.header()["channelIds"], json!(["UCfixture"]));
    }

    #[test]
    fn membership_uses_exact_row_and_paired_feedback_actions_independently_of_likes() {
        let row = |id, default, toggled| {
            let (default_token, toggled_token) = if default == "LIBRARY_ADD" {
                ("fixture-add", "fixture-remove")
            } else {
                ("fixture-remove", "fixture-add")
            };
            json!({"playlistPanelVideoRenderer": {
                "videoId": id,
                "navigationEndpoint": {"watchEndpoint": {"videoId": id}},
                "menu": {"menuRenderer": {
                    "topLevelButtons": [{"likeButtonRenderer": {"likeStatus": "LIKE"}}],
                    "items": [{"toggleMenuServiceItemRenderer": {
                        "defaultIcon": {"iconType": default},
                        "toggledIcon": {"iconType": toggled},
                        "defaultServiceEndpoint": {"feedbackEndpoint": {"feedbackToken": default_token}},
                        "toggledServiceEndpoint": {"feedbackEndpoint": {"feedbackToken": toggled_token}}
                    }}]
                }}
            }})
        };
        let response = |items| {
            json!({"contents": {"singleColumnMusicWatchNextResultsRenderer": {
                "tabbedRenderer": {"watchNextTabbedResultsRenderer": {"tabs": [{"tabRenderer": {"content": {
                    "musicQueueRenderer": {"content": {"playlistPanelRenderer": {"contents": items}}}
                }}}]}}
            }}})
        };
        let video = VideoId::new("fixture-seed").unwrap();
        let other = row("fixture-other", "LIBRARY_SAVED", "LIBRARY_ADD");
        let absent = row(video.as_str(), "LIBRARY_ADD", "LIBRARY_SAVED");
        let saved = row(video.as_str(), "LIBRARY_SAVED", "LIBRARY_ADD");
        assert_eq!(
            song_membership(&video, &response(vec![other.clone(), absent.clone()])),
            Some(false)
        );
        assert_eq!(
            song_membership(&video, &response(vec![saved.clone()])),
            Some(true)
        );
        assert_eq!(song_membership(&video, &response(vec![other])), None);
        assert_eq!(
            song_membership(&video, &response(vec![saved.clone(), absent.clone()])),
            None
        );

        for (row, requested, expected_token) in [
            (absent.clone(), true, "fixture-add"),
            (saved, false, "fixture-remove"),
        ] {
            let value = response(vec![row]);
            let proof = song_membership_proof(&video, &value).unwrap();
            assert!(proof.unique_action);
            let query = song_membership_query(proof.action, requested);
            assert_eq!(query.path(), "feedback");
            assert_eq!(
                query.header(),
                json!({"feedbackTokens": [expected_token]})
                    .as_object()
                    .unwrap()
                    .clone()
            );
        }

        let mut conflicting = absent.clone();
        conflicting["playlistPanelVideoRenderer"]["navigationEndpoint"]["watchEndpoint"]["videoId"] =
            json!("fixture-other");
        assert!(
            song_membership_proof(&video, &response(vec![absent.clone(), conflicting])).is_none()
        );

        let mut ambiguous = absent.clone();
        let toggle = ambiguous.pointer_mut(
            "/playlistPanelVideoRenderer/menu/menuRenderer/items/0/toggleMenuServiceItemRenderer",
        ).unwrap();
        toggle["defaultServiceEndpoint"]["feedbackEndpoint"]["feedbackToken"] =
            json!("another-add");
        let two_actions = response(vec![absent.clone(), ambiguous]);
        let proof = song_membership_proof(&video, &two_actions).unwrap();
        assert!(!proof.action.saved);
        assert!(!proof.unique_action);

        let mut malformed = absent;
        malformed["playlistPanelVideoRenderer"]["menu"]["menuRenderer"]["items"][0]["toggleMenuServiceItemRenderer"]
            ["toggledServiceEndpoint"]["feedbackEndpoint"]["feedbackToken"] = json!("fixture-add");
        assert!(song_membership_proof(&video, &response(vec![malformed])).is_none());
        assert!(song_membership_ack(&[ApiOutcome::Success]).is_ok());
        for outcome in [
            &[][..],
            &[ApiOutcome::Failure][..],
            &[ApiOutcome::Success, ApiOutcome::Success][..],
        ] {
            assert!(matches!(
                song_membership_ack(outcome),
                Err(CatalogMutationError::Unknown(_))
            ));
        }
    }

    #[test]
    fn playlist_membership_requires_third_party_identity_and_paired_target_actions() {
        let id = "PL-fixture";
        let response = |shelf_id, default_id, toggled_id, state| {
            json!({
                "contents": {"twoColumnBrowseResultsRenderer": {
                    "tabs": [{"tabRenderer": {"content": {"sectionListRenderer": {"contents": [
                        {"musicResponsiveHeaderRenderer": {"buttons": [{"toggleButtonRenderer": {
                            "isDisabled": false, "isToggled": state,
                            "defaultIcon": {"iconType": "LIBRARY_ADD"},
                            "toggledIcon": {"iconType": "LIBRARY_SAVED"},
                            "defaultServiceEndpoint": {"likeEndpoint": {"status": "LIKE", "target": {"playlistId": default_id}}},
                            "toggledServiceEndpoint": {"likeEndpoint": {"status": "INDIFFERENT", "target": {"playlistId": toggled_id}}}
                        }}]}}
                    ]}}}}],
                    "secondaryContents": {"sectionListRenderer": {"contents": [
                        {"musicPlaylistShelfRenderer": {"playlistId": shelf_id}}
                    ]}}
                }}
            })
        };

        assert_eq!(
            playlist_membership(id, &response(id, id, id, false)),
            Some(false)
        );
        assert_eq!(
            playlist_membership(id, &response(id, id, id, true)),
            Some(true)
        );
        for (shelf, add, remove) in [
            ("PL-other", id, id),
            (id, "PL-other", id),
            (id, id, "PL-other"),
        ] {
            assert_eq!(
                playlist_membership(id, &response(shelf, add, remove, false)),
                None
            );
        }

        let mut editable = response(id, id, id, false);
        editable.pointer_mut(PLAYLIST_HEADERS).unwrap()[0]["musicEditablePlaylistDetailHeaderRenderer"] =
            json!({});
        assert_eq!(playlist_membership(id, &editable), None);
        let mut wrong_role = response(id, id, id, false);
        wrong_role.pointer_mut(PLAYLIST_HEADERS).unwrap()[0]["musicResponsiveHeaderRenderer"]["buttons"]
            [0]["toggleButtonRenderer"]["toggledServiceEndpoint"]["likeEndpoint"]["status"] =
            json!("LIKE");
        assert_eq!(playlist_membership(id, &wrong_role), None);

        assert_eq!(
            ordinary_playlist_id(&PlaylistId::new(id).unwrap()).unwrap(),
            id
        );
        for unsupported in ["VLPL-fixture", "RDMix-fixture", "OLAKfixture", "LM", "PL"] {
            assert!(ordinary_playlist_id(&PlaylistId::new(unsupported).unwrap()).is_err());
        }
        for (saved, path) in [(true, "like/like"), (false, "like/removelike")] {
            let query = playlist_membership_query(id, saved);
            assert_eq!(query.path(), path);
            assert_eq!(
                query.header(),
                json!({"target": {"playlistId": id}})
                    .as_object()
                    .unwrap()
                    .clone()
            );
        }
    }

    #[test]
    fn history_keeps_repeated_rows_and_typed_episode_ids_without_feedback_tokens() {
        let video = json!({"Video": {
            "video_id": "fixture-video", "duration": "1:20", "title": "Video",
            "channel_name": "Fixture channel", "channel_id": "UCfixture",
            "like_status": "INDIFFERENT", "thumbnails": [], "is_available": true,
            "playlist_id": "fixture-radio", "feedback_token_remove": "opaque-remove"
        }});
        let periods = serde_json::from_value::<Vec<HistoryPeriod>>(json!([
            {"period_name": "Today", "items": [video.clone(), {
                "Episode": {
                    "episode_id": "MPEDfixture", "date": "Live", "duration": "Live",
                    "title": "Episode", "podcast_name": "Podcast", "podcast_id": "MPSPfixture",
                    "like_status": "INDIFFERENT", "thumbnails": [], "is_available": true,
                    "feedback_token_remove": "opaque-remove"
                }
            }]},
            {"period_name": "Yesterday", "items": [video]}
        ]))
        .unwrap();
        let entries = project_history(periods).unwrap();
        assert!(matches!(
            &entries[..],
            [ProviderLibraryItem::History { track: first, period: today },
            ProviderLibraryItem::HistoryEpisode { episode_id, period, .. },
            ProviderLibraryItem::History { track: last, period: yesterday }]
            if first.video_id == last.video_id && episode_id.as_str() == "MPEDfixture"
                && today == "Today" && period == "Today" && yesterday == "Yesterday"
        ));
        assert!(
            !serde_json::to_string(&entries)
                .unwrap()
                .contains("opaque-remove")
        );
    }
}
