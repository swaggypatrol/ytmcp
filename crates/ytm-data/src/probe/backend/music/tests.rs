use super::*;

const TRACKS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/catalog-probe/playlist_tracks_page1.json"
));

#[test]
fn private_create_and_ordered_append_keep_provider_identity() {
    let create = create_private_query("Synthetic mix", None).unwrap();
    assert_eq!(create.path(), "playlist/create");
    assert_eq!(create.header()["privacyStatus"], "PRIVATE");
    let parsed = ytmapi_rs::process_json::<_, BrowserToken>(
        json!({"playlistId": "VLPL_synthetic_created"}).to_string(),
        create,
    )
    .unwrap();
    let id = playlist_id(parsed.get_raw()).unwrap();
    assert_eq!(id.as_str(), "PL_synthetic_created");
    assert!(
        ytmapi_rs::process_json::<_, BrowserToken>(
            json!({"status": "STATUS_SUCCEEDED"}).to_string(),
            create_private_query("Synthetic mix", None).unwrap(),
        )
        .is_err()
    );

    let videos = [
        video_id("fixture-a").unwrap(),
        video_id("fixture-b").unwrap(),
        video_id("fixture-a").unwrap(),
    ];
    assert!(matches!(
        append_query(&id, &videos, PlaylistAppendDuplicates::Reject),
        Err(CatalogMutationError::InvalidInput)
    ));
    let append = append_query(&id, &videos, PlaylistAppendDuplicates::Allow).unwrap();
    assert_eq!(append.header()["playlistId"], id.as_str());
    assert_eq!(
        append.header()["actions"],
        json!([
            {"action": "ACTION_ADD_VIDEO", "addedVideoId": "fixture-a", "dedupeOption": "DEDUPE_OPTION_SKIP"},
            {"action": "ACTION_ADD_VIDEO", "addedVideoId": "fixture-b", "dedupeOption": "DEDUPE_OPTION_SKIP"},
            {"action": "ACTION_ADD_VIDEO", "addedVideoId": "fixture-a", "dedupeOption": "DEDUPE_OPTION_SKIP"}
        ])
    );
    let ack = |video, occurrence| {
        json!({
            "playlistEditVideoAddedResultData": { "videoId": video, "setVideoId": occurrence }
        })
    };
    let response = json!({
        "status": "STATUS_SUCCEEDED",
        "playlistEditResults": [ack("fixture-a", "entry-1"), ack("fixture-b", "entry-2"), ack("fixture-a", "entry-3")]
    });
    let result = ytmapi_rs::process_json::<_, BrowserToken>(response.to_string(), append).unwrap();
    let entries = parse_append_response(&videos, &response).unwrap();
    assert_eq!(entries[0].video_id, videos[0]);
    assert_eq!(entries[2].video_id, videos[2]);
    assert_ne!(entries[0].entry_id, entries[2].entry_id);
    let partial = json!({
        "status": "STATUS_FAILED",
        "playlistEditResults": [
            ack("fixture-a", "entry-1"),
            {"newPlaylistEditResultData": {"status": "STATUS_FAILED"}}
        ]
    });
    assert_eq!(
        parse_append_response(&videos, &partial).unwrap(),
        entries[..1]
    );
    let mut conflicting = result;
    conflicting[2].set_video_id = conflicting[0].set_video_id.clone();
    assert!(matches!(
        normalize_added_entries(&videos, conflicting),
        Err(CatalogMutationError::Unknown(_))
    ));
}

#[test]
fn playlist_edits_target_occurrences_and_keep_unknown_responses_unknown() {
    let playlist = PlaylistId::new("VLPL_fixture").unwrap();
    let update = playlist_edit_query(PlaylistMutation::Update {
        playlist: &playlist,
        title: None,
        description: Some(""),
        privacy: Some(PlaylistPrivacy::Unlisted),
    })
    .unwrap();
    assert_eq!(update.header()["playlistId"], "PL_fixture");
    assert_eq!(
        update.header()["actions"],
        json!([
            { "action": "ACTION_SET_PLAYLIST_DESCRIPTION", "playlistDescription": "" },
            { "action": "ACTION_SET_PLAYLIST_PRIVACY", "playlistPrivacy": "UNLISTED" }
        ])
    );

    let removals = [PlaylistRemoval {
        video_id: video_id("fixture-a").unwrap(),
        entry_id: "entry-2".to_owned(),
    }];
    let remove = playlist_edit_query(PlaylistMutation::Remove {
        playlist: &playlist,
        entries: &removals,
    })
    .unwrap();
    assert_eq!(remove.header()["actions"][0]["removedVideoId"], "fixture-a");
    assert_eq!(remove.header()["actions"][0]["setVideoId"], "entry-2");
    let move_before = playlist_edit_query(PlaylistMutation::Move {
        playlist: &playlist,
        entry_id: "entry-2",
        before: Some("entry-1"),
    })
    .unwrap();
    assert_eq!(
        move_before.header()["actions"][0]["movedSetVideoIdSuccessor"],
        "entry-1"
    );

    assert!(mutation_status(&json!({"status": "STATUS_SUCCEEDED"})).is_ok());
    for response in [json!({"status": "STATUS_FAILED"}), json!({}), Value::Null] {
        assert!(matches!(
            mutation_status(&response),
            Err(CatalogMutationError::Unknown(err)) if !err.retriable
        ));
    }
    let denied = classify_mutation_error_kind(ErrorKind::OtherErrorCodeInResponse {
        code: 403,
        message: "Permission denied".to_owned(),
    });
    assert_eq!(denied, CatalogMutationError::PermissionDenied);
    let ambiguous = classify_mutation_error_kind(ErrorKind::OtherErrorCodeInResponse {
        code: 403,
        message: "Forbidden".to_owned(),
    });
    assert!(matches!(
        ambiguous,
        CatalogMutationError::Unknown(err) if err.code != QueryErrorCode::AuthExpired
    ));
    let video = video_id("fixture-a").unwrap();
    assert_eq!(
        rating_query(&video, SongRating::Liked).unwrap().path(),
        "like/like"
    );
    assert_eq!(
        rating_query(&video, SongRating::Disliked).unwrap().path(),
        "like/dislike"
    );
    assert_eq!(
        rating_query(&video, SongRating::Neutral).unwrap().path(),
        "like/removelike"
    );
}

#[test]
fn public_playlist_identity_comes_from_the_provider_shelf() {
    let requested = PlaylistId::new("PL_probe_01").unwrap();
    let mut response: Value = serde_json::from_str(TRACKS).unwrap();
    assert!(
        playlist_metadata(&requested, &response)
            .unwrap()
            .identity_confirmed
    );

    *response.pointer_mut(PLAYLIST_SHELF_ID).unwrap() = json!("PL_other");
    assert_eq!(
        playlist_metadata(&requested, &response).unwrap_err().code,
        QueryErrorCode::InvalidPayload
    );

    *response.pointer_mut(PLAYLIST_SHELF_ID).unwrap() = Value::Null;
    assert!(
        !playlist_metadata(&requested, &response)
            .unwrap()
            .identity_confirmed
    );
}

#[test]
fn fresh_playlist_read_preserves_duplicate_occurrences_and_unavailable_rows() {
    let playlist = PlaylistId::new("PL_probe_01").unwrap();
    let query = GetPlaylistTracksQuery::new(PlaylistID::from_raw("VLPL_probe_01"));
    let mut value: Value = serde_json::from_str(TRACKS).unwrap();
    let headers = value.pointer_mut(PLAYLIST_HEADER).unwrap();
    let header = headers
        .get("musicResponsiveHeaderRenderer")
        .unwrap()
        .clone();
    *headers = json!({
        "musicEditablePlaylistDetailHeaderRenderer": {
            "playlistId": "PL_probe_01",
            "header": {"musicResponsiveHeaderRenderer": header},
            "editHeader": {"musicPlaylistEditHeaderRenderer": {"privacy": "PRIVATE"}}
        }
    });
    let rows = value
        .pointer_mut(PLAYLIST_FIRST_ROWS)
        .unwrap()
        .as_array_mut()
        .unwrap();
    let mut row = rows.remove(0);
    for (index, entry) in ["entry-1", "entry-2", "entry-3"].iter().enumerate() {
        let item = &mut row["musicResponsiveListItemRenderer"];
        let menu = item["menu"]["menuRenderer"]["items"]
            .as_array_mut()
            .unwrap();
        menu.retain(|item| item.get("menuServiceItemRenderer").is_none());
        menu.push(json!({
            "menuServiceItemRenderer": {"serviceEndpoint": {"playlistEditEndpoint": {
                "actions": [
                    {"action": "ACTION_REMOVE_VIDEO", "removedVideoId": "probePlVid1", "setVideoId": entry}
                ]
            }}}
        }));
        if index == 2 {
            item["musicItemRendererDisplayPolicy"] =
                json!("MUSIC_ITEM_RENDERER_DISPLAY_POLICY_GREY_OUT");
        }
        rows.insert(index, row.clone());
    }
    let page = parse_fresh_playlist(
        &playlist,
        &query,
        serde_json::from_value(value).unwrap(),
        false,
    )
    .unwrap();
    let metadata = page.metadata.unwrap();
    assert_eq!(metadata.privacy, Some(PlaylistPrivacy::Private));
    assert_eq!(metadata.editable, Some(true));
    assert_eq!(
        page.entries[0].track.video_id,
        page.entries[1].track.video_id
    );
    assert_eq!((page.entries[0].position, page.entries[1].position), (0, 1));
    assert_ne!(page.entries[0].entry_id, page.entries[1].entry_id);
    assert_eq!(
        page.entries[2].track.availability,
        TrackAvailability::Unavailable
    );
    assert_eq!(page.entries[2].entry_id.as_deref(), Some("entry-3"));
    assert!(page.next_continuation.is_some());
}

#[test]
fn fresh_playlist_only_confirms_editability_for_a_matching_provider_id() {
    let playlist = PlaylistId::new("PL_probe_01").unwrap();
    let mut value: Value = serde_json::from_str(TRACKS).unwrap();
    let section = value.pointer_mut(PLAYLIST_HEADER).unwrap();
    let header = section["musicResponsiveHeaderRenderer"].take();
    *section = json!({
        "musicEditablePlaylistDetailHeaderRenderer": {
            "header": {"musicResponsiveHeaderRenderer": header}
        }
    });
    assert_eq!(playlist_metadata(&playlist, &value).unwrap().editable, None);

    value.pointer_mut(PLAYLIST_HEADER).unwrap()[EDITABLE_HEADER]["playlistId"] =
        json!("PL_somewhere_else");
    assert!(playlist_metadata(&playlist, &value).is_err());
    value.pointer_mut(PLAYLIST_HEADER).unwrap()[EDITABLE_HEADER]["playlistId"] =
        json!("VLPL_probe_01");
    assert_eq!(
        playlist_metadata(&playlist, &value).unwrap().editable,
        Some(true)
    );
}

#[test]
fn song_radio_reads_provider_next_without_returning_seed_or_inferred_features() {
    let seed = video_id("fixture-seed").unwrap();
    let query = GetWatchPlaylistQuery::new_from_video_id(VideoID::from_raw(seed.as_str()));
    assert_eq!(query.path(), "next");
    assert_eq!(query.header()["playlistId"], "RDAMVMfixture-seed");
    let record = |id, name| {
        json!({"playlistPanelVideoRenderer": {
            "title": {"runs": [{"text": name}]},
            "shortBylineText": {"runs": [{"text": "Fixture artist"}]},
            "lengthText": {"runs": [{"text": "3:21"}]},
            "navigationEndpoint": {"watchEndpoint": {"videoId": id}},
            "thumbnail": {"thumbnails": []}
        }})
    };
    let response = json!({
        "contents": {"singleColumnMusicWatchNextResultsRenderer": {"tabbedRenderer": {
            "watchNextTabbedResultsRenderer": {"tabs": [{"tabRenderer": {"content": {
                "musicQueueRenderer": {"content": {"playlistPanelRenderer": {
                    "contents": [
                        record(seed.as_str(), "Seed track"),
                        record("fixture-a", "Candidate"),
                        record("fixture-a", "Repeated candidate")
                    ],
                    "continuations": [{"nextRadioContinuationData": {"continuation": "fixture-radio-next"}}]
                }}}
            }}}]}
        }}}
    });
    let (tracks, next) = Vec::<WatchPlaylistTrack>::parse_from_continuable(ProcessedResult {
        source: response.to_string(),
        json: serde_json::from_value(response).unwrap(),
        query: &query,
    })
    .unwrap();
    let page = normalize_song_radio(&seed, tracks, next.map(|t| t.get_raw().to_owned())).unwrap();
    assert_eq!(
        page.next_continuation.as_deref(),
        Some("fixture-radio-next")
    );
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].video_id.as_str(), "fixture-a");
    assert_eq!(page.items[0].duration_ms, Some(201_000));
    assert_eq!(page.items[0].availability, TrackAvailability::Unknown);
    assert_eq!(page.items[0].version_tag, None);
}

#[test]
fn rating_readback_requires_an_explicit_state_for_the_requested_video() {
    let video = video_id("fixture-a").unwrap();
    let entry = |target, status| {
        json!({
            "likeButtonRenderer": {"target": {"videoId": target}, "likeStatus": status}
        })
    };
    let response = |actions| {
        json!({
            "playerOverlays": {"playerOverlayRenderer": {"actions": actions}}
        })
    };
    for (status, rating) in [
        ("LIKE", SongRating::Liked),
        ("DISLIKE", SongRating::Disliked),
        ("INDIFFERENT", SongRating::Neutral),
        ("UNKNOWN", SongRating::Unknown),
    ] {
        let json = response(vec![
            entry("fixture-unrelated", "LIKE"),
            entry(video.as_str(), status),
        ]);
        assert_eq!(song_rating_from_watch_next(&video, &json), rating);
    }
    assert_eq!(
        song_rating_from_watch_next(
            &video,
            &response(vec![entry("fixture-unrelated", "INDIFFERENT")])
        ),
        SongRating::Unknown
    );
    assert_eq!(
        song_rating_from_watch_next(
            &video,
            &response(vec![
                entry(video.as_str(), "DISLIKE"),
                entry(video.as_str(), "LIKE")
            ])
        ),
        SongRating::Unknown
    );
    assert_eq!(
        song_rating_from_watch_next(&video, &json!({})),
        SongRating::Unknown
    );
}

#[test]
fn rating_toast_is_only_acknowledged_and_neutral_empty_body_is_inconclusive() {
    assert!(
        rating_mutation_status(&json!({
            "actions": [{"addToToastAction": {"item": {"toastRenderer": {}}}}]
        }))
        .is_ok()
    );
    assert!(matches!(
        rating_mutation_status(&json!({"responseContext": {}})),
        Err(CatalogMutationError::Unknown(err)) if !err.retriable
    ));
    assert!(matches!(
        rating_mutation_status(&json!({"status": "STATUS_FAILED", "actions": [{"addToToastAction": {}}]})),
        Err(CatalogMutationError::Unknown(err)) if !err.retriable
    ));
}

/// The app's own add (`DuplicateHandlingMode::ReturnError`, parsed by the provider library as
/// `add_to_playlist` does) keeps exactly the one entry it created, and a refused duplicate still
/// fails with the error the UI reports as `rejected`.
#[test]
fn ui_add_reply_maps_to_exactly_one_created_entry() {
    let playlist = PlaylistId::new("PL_synthetic_target").unwrap();
    let video = video_id("fixture-a").unwrap();
    let reply = |response: Value| {
        let query = AddPlaylistItemsQuery::new_from_videos(
            PlaylistID::from_raw(playlist.as_str()),
            [VideoID::from_raw(video.as_str())],
            DuplicateHandlingMode::ReturnError,
        );
        ytmapi_rs::process_json::<_, BrowserToken>(response.to_string(), query)
    };
    let ack = |video: &str, entry: &str| json!({"playlistEditVideoAddedResultData": {"videoId": video, "setVideoId": entry}});
    let added = |results: Vec<Value>| {
        let parsed = reply(json!({"status": "STATUS_SUCCEEDED", "playlistEditResults": results}));
        single_created_entry(&video, parsed.unwrap())
    };
    assert_eq!(
        added(vec![ack("fixture-a", "entry-new")]).as_deref(),
        Some("entry-new")
    );
    // An acknowledgement that names nothing, another song, or two entries is no undo target.
    assert_eq!(added(vec![]), None);
    assert_eq!(added(vec![ack("fixture-b", "entry-new")]), None);
    assert_eq!(
        added(vec![
            ack("fixture-a", "entry-1"),
            ack("fixture-a", "entry-2")
        ]),
        None
    );
    assert_eq!(added(vec![ack("fixture-a", "")]), None);

    let duplicate =
        reply(json!({"status": "STATUS_FAILED", "actions": [{"addToToastAction": {}}]}));
    assert_eq!(
        crate::probe::error::classify_ytmapi_error(duplicate.unwrap_err()).code,
        QueryErrorCode::UnavailableTrack
    );
}
