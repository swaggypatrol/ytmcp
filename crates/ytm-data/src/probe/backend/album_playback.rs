use std::sync::Arc;

use serde_json::Value;
use ytm_protocol::{
    AlbumDetail, AlbumId, PlaybackRef, PlaylistId, QueryError, QueryErrorCode, VideoId,
    validate_id_str,
};
use ytmapi_rs::auth::BrowserToken;
use ytmapi_rs::common::{AlbumID, YoutubeID};
use ytmapi_rs::json::Json;
use ytmapi_rs::query::GetAlbumQuery;

use super::{CatalogSession, YtmapiCatalogBackend};
use crate::probe::error::{classify_ytmapi_error, make_query_error};
use crate::probe::normalize::{normalize_album_detail, strip_vl_prefix};

const ALBUM_BUTTONS: &str = concat!(
    "/tabs/0/tabRenderer/content/sectionListRenderer/contents/0",
    "/musicResponsiveHeaderRenderer/buttons"
);
const ALBUM_ROWS: &str =
    "/secondaryContents/sectionListRenderer/contents/0/musicShelfRenderer/contents";
const TRACK_WATCH_ENDPOINT: &str = concat!(
    "/overlay/musicItemThumbnailOverlayRenderer/content/musicPlayButtonRenderer",
    "/playNavigationEndpoint/watchEndpoint"
);

impl YtmapiCatalogBackend {
    pub(super) async fn read_album_playback_target(
        &self,
        session: &Arc<dyn CatalogSession>,
        album: &AlbumId,
        index: u32,
    ) -> Result<PlaybackRef, QueryError> {
        if !album.as_str().starts_with("MPREb_") {
            return Err(invalid(
                "album playback requires a YouTube Music album browse ID",
            ));
        }
        let yt = Self::downcast_session(session)?;
        let query = GetAlbumQuery::new(AlbumID::from_raw(album.as_str()));
        let response = yt
            .json_query::<GetAlbumQuery<'_>>(&query)
            .await
            .map_err(classify_ytmapi_error)?;
        album_playback_target(&response.into_inner(), index)
    }
}

fn album_playback_target(value: &Value, index: u32) -> Result<PlaybackRef, QueryError> {
    let album = album_renderer(value)?;
    let audio_playlist = album_audio_playlist(album)?;
    let rows = album_rows(album)?;
    let mut matches = playable_rows(rows).filter(|(_, endpoint)| {
        endpoint.get("index").and_then(Value::as_u64) == Some(u64::from(index))
    });
    let (row, endpoint) = matches.next().ok_or_else(|| {
        unsupported("YouTube Music did not expose a playable album occurrence at that index")
    })?;
    if matches.next().is_some() {
        return Err(invalid(
            "the album returned more than one playable occurrence at that index",
        ));
    }
    let row_playlist = endpoint
        .get("playlistId")
        .and_then(Value::as_str)
        .map(strip_vl_prefix);
    if row_playlist != Some(audio_playlist) {
        return Err(invalid(
            "the album track did not match the album audio playlist",
        ));
    }
    let video_id = endpoint
        .get("videoId")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("the album track did not identify a playable video"))?;
    if row
        .pointer("/playlistItemData/videoId")
        .and_then(Value::as_str)
        .is_some_and(|id| id != video_id)
    {
        return Err(invalid(
            "the album track and its playback button identified different videos",
        ));
    }
    let row_entry = row
        .pointer("/playlistItemData/playlistSetVideoId")
        .and_then(Value::as_str);
    let endpoint_entry = endpoint.get("playlistSetVideoId").and_then(Value::as_str);
    if row_entry
        .zip(endpoint_entry)
        .is_some_and(|(row, endpoint)| row != endpoint)
    {
        return Err(invalid(
            "the album track and its playback button identified different occurrences",
        ));
    }
    let playlist_entry_id = endpoint_entry.or(row_entry);
    if playlist_entry_id.is_some_and(|id| validate_id_str(id).is_err()) {
        return Err(invalid("the album track identified an invalid occurrence"));
    }

    Ok(PlaybackRef {
        video_id: VideoId::new(video_id)
            .map_err(|_| invalid("the album track identified an invalid video"))?,
        playlist_id: Some(
            PlaylistId::new(audio_playlist)
                .map_err(|_| invalid("the album header identified an invalid playlist"))?,
        ),
        playlist_entry_id: playlist_entry_id.map(str::to_owned),
        browse_id: None,
        queue_entry_id: None,
        start_index: Some(index),
    })
}

/// Parses an album detail and its playback context from one raw album response.
/// A missing or inconsistent context leaves `audio_playlist_id` empty instead of failing.
pub(super) fn album_detail_from_json(
    album_id: &AlbumId,
    query: &GetAlbumQuery<'_>,
    json: Json,
) -> Result<AlbumDetail, QueryError> {
    let value = json.into_inner();
    let context = album_playback_rows(&value);
    let album =
        ytmapi_rs::process_json::<GetAlbumQuery<'_>, BrowserToken>(value.to_string(), query)
            .map_err(classify_ytmapi_error)?;
    let mut detail = normalize_album_detail(album_id, album)?;
    if let Some(context) = context
        && context.rows.len() == detail.tracks.len()
        && context
            .rows
            .iter()
            .zip(&detail.tracks)
            .all(|((video_id, _), track)| track.video_id.as_str() == video_id)
    {
        detail.playback_indexes = context.rows.into_iter().map(|(_, index)| index).collect();
        detail.audio_playlist_id = Some(context.audio_playlist);
    }
    Ok(detail)
}

/// Playback context of an album: its audio playlist and, in display order, the
/// `(videoId, provider watch index)` of every playable (non-greyed) row.
#[derive(Debug, PartialEq, Eq)]
struct AlbumPlaybackRows {
    pub audio_playlist: PlaylistId,
    pub rows: Vec<(String, u32)>,
}

/// Reads the album playback context from a raw album response, or `None` when any
/// playable row lacks a unique native index, a video, or the album audio playlist.
fn album_playback_rows(value: &Value) -> Option<AlbumPlaybackRows> {
    let album = album_renderer(value).ok()?;
    let audio_playlist = album_audio_playlist(album).ok()?;
    let mut seen = std::collections::HashSet::new();
    let rows = playable_rows(album_rows(album).ok()?)
        .map(|(row, endpoint)| {
            let index = u32::try_from(endpoint.get("index")?.as_u64()?).ok()?;
            let playlist = endpoint.get("playlistId")?.as_str().map(strip_vl_prefix)?;
            let video_id = endpoint.get("videoId")?.as_str()?;
            let row_video = row
                .pointer("/playlistItemData/videoId")
                .and_then(Value::as_str);
            (playlist == audio_playlist
                && row_video.is_none_or(|id| id == video_id)
                && seen.insert(index))
            .then(|| (video_id.to_owned(), index))
        })
        .collect::<Option<Vec<_>>>()?;
    Some(AlbumPlaybackRows {
        audio_playlist: PlaylistId::new(audio_playlist).ok()?,
        rows,
    })
}

fn album_renderer(value: &Value) -> Result<&Value, QueryError> {
    value
        .pointer("/contents/twoColumnBrowseResultsRenderer")
        .ok_or_else(|| invalid("YouTube Music did not return an album response"))
}

fn album_audio_playlist(album: &Value) -> Result<&str, QueryError> {
    let buttons = album
        .pointer(ALBUM_BUTTONS)
        .and_then(Value::as_array)
        .ok_or_else(|| unsupported("the album did not expose a playback button"))?;
    let mut audio_playlists = buttons
        .iter()
        .filter_map(|button| button.pointer("/musicPlayButtonRenderer/playNavigationEndpoint"))
        .flat_map(|endpoint| {
            [
                "/watchEndpoint/playlistId",
                "/watchPlaylistEndpoint/playlistId",
            ]
            .into_iter()
            .filter_map(move |path| endpoint.pointer(path).and_then(Value::as_str))
        })
        .map(strip_vl_prefix);
    let audio_playlist = audio_playlists
        .next()
        .ok_or_else(|| unsupported("the album did not expose an audio playlist ID"))?;
    if !audio_playlist.starts_with("OLAK5uy_") || audio_playlists.any(|id| id != audio_playlist) {
        return Err(invalid(
            "the album header did not identify a consistent audio playlist",
        ));
    }
    Ok(audio_playlist)
}

fn album_rows(album: &Value) -> Result<&[Value], QueryError> {
    album
        .pointer(ALBUM_ROWS)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| invalid("YouTube Music did not return an album track shelf"))
}

/// Non-greyed track rows paired with their play button's watch endpoint.
fn playable_rows(rows: &[Value]) -> impl Iterator<Item = (&Value, &Value)> {
    rows.iter().filter_map(|item| {
        let row = item.get("musicResponsiveListItemRenderer")?;
        if row
            .get("musicItemRendererDisplayPolicy")
            .and_then(Value::as_str)
            == Some("MUSIC_ITEM_RENDERER_DISPLAY_POLICY_GREY_OUT")
        {
            return None;
        }
        Some((row, row.pointer(TRACK_WATCH_ENDPOINT)?))
    })
}

fn invalid(message: &str) -> QueryError {
    make_query_error(QueryErrorCode::InvalidPayload, false, None, message)
}

fn unsupported(message: &str) -> QueryError {
    make_query_error(QueryErrorCode::UnsupportedCapability, false, None, message)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const AUDIO: &str = "OLAK5uy_fixtureAudio";

    fn album(button: &str, playlist: &str, rows: Vec<Value>) -> Value {
        json!({"contents": {"twoColumnBrowseResultsRenderer": {
            "tabs": [{"tabRenderer": {"content": {"sectionListRenderer": {"contents": [{
                "musicResponsiveHeaderRenderer": {"buttons": [{"musicPlayButtonRenderer": {
                    "playNavigationEndpoint": {(button): {"playlistId": playlist}}
                }}]}
            }]}}}}],
            "secondaryContents": {"sectionListRenderer": {"contents": [{
                "musicShelfRenderer": {"contents": rows}
            }]}}
        }}})
    }

    fn row(display_number: &str, video_id: &str, endpoint: Value) -> Value {
        json!({"musicResponsiveListItemRenderer": {
            "index": {"runs": [{"text": display_number}]},
            "playlistItemData": {"videoId": video_id, "playlistSetVideoId": "fixture-entry"},
            "overlay": {"musicItemThumbnailOverlayRenderer": {"content": {"musicPlayButtonRenderer": {
                "playNavigationEndpoint": {"watchEndpoint": endpoint}
            }}}}
        }})
    }

    fn watch(video_id: &str, index: u32) -> Value {
        json!({"videoId": video_id, "playlistId": AUDIO, "index": index, "playlistSetVideoId": "fixture-entry"})
    }

    #[test]
    fn album_playback_uses_native_index_after_an_unavailable_display_row() {
        let unavailable = json!({"musicResponsiveListItemRenderer": {
            "index": {"runs": [{"text": "2"}]},
            "musicItemRendererDisplayPolicy": "MUSIC_ITEM_RENDERER_DISPLAY_POLICY_GREY_OUT"
        }});
        let value = album(
            "watchPlaylistEndpoint",
            &format!("VL{AUDIO}"),
            vec![
                row("1", "first-video", watch("first-video", 0)),
                unavailable,
                row("3", "second-playable", watch("second-playable", 1)),
            ],
        );
        let target = album_playback_target(&value, 1).unwrap();
        assert_eq!(target.video_id.as_str(), "second-playable");
        assert_eq!(target.playlist_id.unwrap().as_str(), AUDIO);
        assert_eq!(target.start_index, Some(1));
        assert_eq!(target.playlist_entry_id.as_deref(), Some("fixture-entry"));
    }

    #[test]
    fn album_playback_accepts_the_other_native_header_button_shape() {
        let value = album(
            "watchEndpoint",
            AUDIO,
            vec![row("1", "song", watch("song", 0))],
        );
        assert_eq!(
            album_playback_target(&value, 0).unwrap().video_id.as_str(),
            "song"
        );
    }

    #[test]
    fn album_playback_does_not_guess_a_missing_native_index_from_display_number() {
        let mut endpoint = watch("song", 1);
        endpoint.as_object_mut().unwrap().remove("index");
        let value = album(
            "watchPlaylistEndpoint",
            AUDIO,
            vec![row("2", "song", endpoint)],
        );
        assert_eq!(
            album_playback_target(&value, 1).unwrap_err().code,
            QueryErrorCode::UnsupportedCapability
        );
    }

    #[test]
    fn album_playback_rejects_an_ambiguous_native_index() {
        let value = album(
            "watchPlaylistEndpoint",
            AUDIO,
            vec![
                row("1", "first-video", watch("first-video", 0)),
                row("2", "second-video", watch("second-video", 0)),
            ],
        );
        assert_eq!(
            album_playback_target(&value, 0).unwrap_err().code,
            QueryErrorCode::InvalidPayload
        );
    }

    #[test]
    fn album_playback_rejects_conflicting_playlist_video_or_occurrence_ids() {
        for (field, wrong) in [
            ("playlistId", "OLAK5uy_other"),
            ("videoId", "other-video"),
            ("playlistSetVideoId", "other-entry"),
        ] {
            let mut endpoint = watch("song", 0);
            endpoint[field] = json!(wrong);
            let value = album(
                "watchPlaylistEndpoint",
                AUDIO,
                vec![row("1", "song", endpoint)],
            );
            assert_eq!(
                album_playback_target(&value, 0).unwrap_err().code,
                QueryErrorCode::InvalidPayload,
                "{field}"
            );
        }
    }

    /// A track row: its video (`None` = greyed out) and endpoint `(playlistId, index)`.
    type FixtureRow<'a> = (Option<&'a str>, Option<(&'a str, u32)>);

    /// The scrubbed catalog-probe album, with its rows replaced by `rows`, each
    /// cloned from the fixture's track row and given the endpoint fields provided.
    fn fixture_album(rows: &[FixtureRow<'_>]) -> Json {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/catalog-probe/album_detail.json"
        );
        let mut value: Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let shelf = value
            .pointer_mut(&format!(
                "/contents/twoColumnBrowseResultsRenderer{ALBUM_ROWS}"
            ))
            .unwrap();
        let template = shelf[0].clone();
        *shelf = Value::Array(
            rows.iter()
                .enumerate()
                .map(|(n, (video, endpoint))| {
                    let mut item = template.clone();
                    let row = &mut item["musicResponsiveListItemRenderer"];
                    row["index"] = json!({"runs": [{"text": (n + 1).to_string()}]});
                    let watch = row.pointer_mut(TRACK_WATCH_ENDPOINT).unwrap();
                    match video {
                        Some(video) => watch["videoId"] = json!(video),
                        None => {
                            row["musicItemRendererDisplayPolicy"] =
                                json!("MUSIC_ITEM_RENDERER_DISPLAY_POLICY_GREY_OUT")
                        }
                    }
                    if let Some((playlist, index)) = endpoint {
                        let watch = row.pointer_mut(TRACK_WATCH_ENDPOINT).unwrap();
                        watch["playlistId"] = json!(playlist);
                        watch["index"] = json!(index);
                    }
                    item
                })
                .collect(),
        );
        serde_json::from_value(value).unwrap()
    }

    fn detail(rows: &[FixtureRow<'_>]) -> AlbumDetail {
        let album = AlbumId::new("MPREb_fixture").unwrap();
        let query = GetAlbumQuery::new(AlbumID::from_raw(album.as_str()));
        album_detail_from_json(&album, &query, fixture_album(rows)).unwrap()
    }

    #[test]
    fn album_detail_carries_native_indexes_aligned_with_tracks() {
        let probe = "OLAK5uy_probe";
        let album = detail(&[
            (Some("probeAlbVid1"), Some((probe, 0))),
            (None, None),
            (Some("probeAlbVid3"), Some((probe, 1))),
        ]);
        let videos: Vec<_> = album.tracks.iter().map(|t| t.video_id.as_str()).collect();
        assert_eq!(videos, ["probeAlbVid1", "probeAlbVid3"]);
        assert_eq!(album.audio_playlist_id.unwrap().as_str(), probe);
        assert_eq!(album.playback_indexes, [0, 1]);
    }

    #[test]
    fn album_detail_without_a_consistent_context_still_loads_single_tracks() {
        let probe = "OLAK5uy_probe";
        for rows in [
            vec![
                (Some("probeAlbVid1"), Some((probe, 0))),
                (Some("probeAlbVid2"), Some(("OLAK5uy_other", 1))),
            ],
            vec![
                (Some("probeAlbVid1"), Some((probe, 0))),
                (Some("probeAlbVid2"), Some((probe, 0))),
            ],
            vec![
                (Some("probeAlbVid1"), Some((probe, 0))),
                (Some("probeAlbVid2"), None),
            ],
        ] {
            let album = detail(&rows);
            assert_eq!(album.tracks.len(), 2);
            assert!(album.audio_playlist_id.is_none(), "{rows:?}");
            assert!(album.playback_indexes.is_empty());
        }
    }
}
