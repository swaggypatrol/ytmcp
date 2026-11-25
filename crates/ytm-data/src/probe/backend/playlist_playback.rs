use std::collections::HashSet;

use serde_json::Value;
use ytm_protocol::{
    CollectionPlayback, CollectionPlaybackEntry, PlaylistId, Track, validate_id_str,
};

use super::album_playback::playable_rows;
use crate::probe::normalize::strip_vl_prefix;

const ROW_TITLE: &str = "/flexColumns/0/musicResponsiveListItemFlexColumnRenderer/text/runs/0/text";

/// Reads the native playback context of one page of playlist rows (first page or continuation)
/// from each row's own play button, aligned 1:1 with `tracks`, the rows the provider parser
/// kept. It keeps the rows that parser keeps (not greyed out, not "Song deleted"), so a native
/// index is never inferred from a row number. Returns `None` for the whole page when a kept row
/// lacks a playlist ID, native index or video, the rows name different playlists, an index
/// repeats, a row's own identity disagrees with its play button, or the rows do not line up
/// with `tracks`.
pub(super) fn playlist_rows_playback(
    rows: &[Value],
    tracks: &[Track],
) -> Option<CollectionPlayback> {
    let mut playlist: Option<&str> = None;
    let mut indexes = HashSet::new();
    let mut entries = Vec::with_capacity(tracks.len());
    let kept = playable_rows(rows)
        .filter(|(row, _)| row.pointer(ROW_TITLE).and_then(Value::as_str) != Some("Song deleted"));
    let mut tracks_left = tracks.iter();
    for (row, endpoint) in kept {
        let track = tracks_left.next()?;
        let video_id = endpoint.get("videoId")?.as_str()?;
        let row_video = row
            .pointer("/playlistItemData/videoId")
            .and_then(Value::as_str);
        if video_id != track.video_id.as_str() || row_video.is_some_and(|id| id != video_id) {
            return None;
        }
        let row_playlist = strip_vl_prefix(endpoint.get("playlistId")?.as_str()?);
        if *playlist.get_or_insert(row_playlist) != row_playlist {
            return None;
        }
        let index = u32::try_from(endpoint.get("index")?.as_u64()?).ok()?;
        if !indexes.insert(index) {
            return None;
        }
        let endpoint_entry = endpoint.get("playlistSetVideoId").and_then(Value::as_str);
        let row_entry = row
            .pointer("/playlistItemData/playlistSetVideoId")
            .and_then(Value::as_str);
        if endpoint_entry
            .zip(row_entry)
            .is_some_and(|(endpoint, row)| endpoint != row)
        {
            return None;
        }
        let entry_id = endpoint_entry.or(row_entry);
        if entry_id.is_some_and(|id| validate_id_str(id).is_err()) {
            return None;
        }
        entries.push(CollectionPlaybackEntry {
            start_index: index,
            playlist_entry_id: entry_id.map(str::to_owned),
        });
    }
    if tracks_left.next().is_some() {
        return None;
    }
    Some(CollectionPlayback {
        playlist_id: PlaylistId::new(playlist?).ok()?,
        entries,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use ytmapi_rs::common::{PlaylistID, YoutubeID};
    use ytmapi_rs::query::playlist::GetPlaylistTracksQuery;

    use super::super::album_playback::TRACK_WATCH_ENDPOINT;
    use super::super::parse_playlist_tracks_page;
    use super::*;

    const FIRST_ROWS: &str = concat!(
        "/contents/twoColumnBrowseResultsRenderer/secondaryContents/sectionListRenderer",
        "/contents/0/musicPlaylistShelfRenderer/contents"
    );
    const NEXT_ROWS: &str =
        "/onResponseReceivedActions/0/appendContinuationItemsAction/continuationItems";

    /// A row: its video (`None` = greyed out) and its play button's `(playlistId, index)`.
    type Row<'a> = (Option<&'a str>, Option<(&'a str, u32)>);

    /// The scrubbed catalog-probe playlist page (first or continuation), its rows replaced by
    /// `rows`, each cloned from the fixture's track row and given the endpoint fields provided.
    /// The recorded fixture's play buttons carry only a video ID, so the playlist ID, index and
    /// entry ID shapes here follow the album fixture's, not a recorded Liked page.
    fn page(continuation: bool, rows: &[Row<'_>]) -> Value {
        let (file, pointer) = if continuation {
            ("playlist_tracks_page2.json", NEXT_ROWS)
        } else {
            ("playlist_tracks_page1.json", FIRST_ROWS)
        };
        let path = format!(
            "{}/../../fixtures/catalog-probe/{file}",
            env!("CARGO_MANIFEST_DIR")
        );
        let mut value: Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let shelf = value.pointer_mut(pointer).unwrap();
        let template = shelf[0].clone();
        let mut items: Vec<Value> = rows
            .iter()
            .enumerate()
            .map(|(n, (video, endpoint))| {
                let mut item = template.clone();
                let row = &mut item["musicResponsiveListItemRenderer"];
                row["flexColumns"][0]["musicResponsiveListItemFlexColumnRenderer"]["text"]["runs"]
                    [0]["text"] = json!(format!("Row {}", n + 1));
                let watch = row.pointer_mut(TRACK_WATCH_ENDPOINT).unwrap();
                match video {
                    Some(video) => {
                        watch["videoId"] = json!(video);
                        watch["playlistSetVideoId"] = json!(format!("set-{video}"));
                    }
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
            .collect();
        items.push(
            json!({"continuationItemRenderer": {"continuationEndpoint": {
                "continuationCommand": {"token": "fixtureNext"}
            }}}),
        );
        *shelf = Value::Array(items);
        value
    }

    fn parse(continuation: bool, rows: &[Row<'_>]) -> (Vec<Track>, Option<CollectionPlayback>) {
        let query = GetPlaylistTracksQuery::new(PlaylistID::from_raw("VLLM"));
        let page = parse_playlist_tracks_page(
            &query,
            serde_json::from_value(page(continuation, rows)).unwrap(),
            continuation,
        )
        .unwrap();
        (page.0, page.2)
    }

    #[test]
    fn liked_rows_carry_native_indexes_after_a_greyed_out_row() {
        // A continuation page's rows sit further down the playlist: its indexes are its own.
        for (continuation, first) in [(false, 0), (true, 100)] {
            let (tracks, playback) = parse(
                continuation,
                &[
                    (Some("likedVid1"), Some(("LM", first))),
                    (None, None),
                    (Some("likedVid3"), Some(("VLLM", first + 1))),
                ],
            );
            let videos: Vec<_> = tracks.iter().map(|t| t.video_id.as_str()).collect();
            assert_eq!(videos, ["likedVid1", "likedVid3"]);
            let playback = playback.expect("a consistent context");
            assert_eq!(playback.playlist_id.as_str(), "LM");
            assert_eq!(
                playback.entries,
                [
                    CollectionPlaybackEntry {
                        start_index: first,
                        playlist_entry_id: Some("set-likedVid1".into()),
                    },
                    // The third row on screen plays from the next native index, not from its
                    // row number.
                    CollectionPlaybackEntry {
                        start_index: first + 1,
                        playlist_entry_id: Some("set-likedVid3".into()),
                    },
                ]
            );
        }
    }

    #[test]
    fn liked_page_without_a_consistent_context_still_loads_its_tracks() {
        for rows in [
            // A playable row's play button has no playlist or index.
            vec![
                (Some("likedVid1"), Some(("LM", 0))),
                (Some("likedVid2"), None),
            ],
            // Rows name different playlists.
            vec![
                (Some("likedVid1"), Some(("LM", 0))),
                (Some("likedVid2"), Some(("PL_other", 1))),
            ],
            // A native index repeats.
            vec![
                (Some("likedVid1"), Some(("LM", 4))),
                (Some("likedVid2"), Some(("LM", 4))),
            ],
        ] {
            let (tracks, playback) = parse(false, &rows);
            assert_eq!(tracks.len(), 2, "{rows:?}");
            assert!(playback.is_none(), "{rows:?}");
        }
    }

    #[test]
    fn liked_context_requires_rows_that_line_up_with_the_parsed_tracks() {
        let rows = page(false, &[(Some("likedVid1"), Some(("LM", 0)))]);
        let rows = rows.pointer(FIRST_ROWS).and_then(Value::as_array).unwrap();
        let query = GetPlaylistTracksQuery::new(PlaylistID::from_raw("VLLM"));
        let (tracks, _, _) = parse_playlist_tracks_page(
            &query,
            serde_json::from_value(page(
                false,
                &[
                    (Some("likedVid1"), Some(("LM", 0))),
                    (Some("likedVid2"), Some(("LM", 1))),
                ],
            ))
            .unwrap(),
            false,
        )
        .unwrap();
        // One row for two tracks, and a row whose button names another video, are both refused.
        assert!(playlist_rows_playback(rows, &tracks).is_none());
        assert!(playlist_rows_playback(rows, &tracks[1..]).is_none());
        assert!(playlist_rows_playback(rows, &tracks[..1]).is_some());

        let mut conflicting = rows.clone();
        conflicting[0]["musicResponsiveListItemRenderer"]["playlistItemData"] =
            json!({"videoId": "likedVid1", "playlistSetVideoId": "another-set"});
        assert!(playlist_rows_playback(&conflicting, &tracks[..1]).is_none());
    }
}
