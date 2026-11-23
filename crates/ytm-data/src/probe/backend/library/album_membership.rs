use url::Url;
use ytmapi_rs::query::search::{AlbumsFilter, FilteredSearch};

use super::*;

const ALBUM_HEADER: &str = concat!(
    "/contents/twoColumnBrowseResultsRenderer/tabs/0/tabRenderer/content",
    "/sectionListRenderer/contents/0/musicResponsiveHeaderRenderer"
);
const CANONICAL_URL: &str = "/microformat/microformatDataRenderer/urlCanonical";
const SEARCH_SECTIONS: &str = concat!(
    "/contents/tabbedSearchResultsRenderer/tabs/0/tabRenderer/content",
    "/sectionListRenderer/contents"
);
const SEARCH_CONTINUATION: &str = "/continuationContents/musicShelfContinuation";
const ALBUM_BROWSE: &str = "/navigationEndpoint/browseEndpoint";
const PAGE_TYPE: &str =
    "/browseEndpointContextSupportedConfigs/browseEndpointContextMusicConfig/pageType";
const ALBUM_OVERLAY: &str = concat!(
    "/overlay/musicItemThumbnailOverlayRenderer/content",
    "/musicPlayButtonRenderer/playNavigationEndpoint"
);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendAlbumMembership {
    pub playlist_id: PlaylistId,
    pub saved: bool,
}

struct AlbumDetailEvidence {
    title: String,
    proof: BackendAlbumMembership,
}

enum SearchEvidence {
    Bound,
    Missing { continuation: Option<String> },
}

impl YtmapiCatalogBackend {
    pub(in crate::probe::backend) async fn read_album_membership(
        &self,
        session: &Arc<dyn CatalogSession>,
        album: &AlbumId,
    ) -> Result<BackendAlbumMembership, QueryError> {
        let yt = Self::downcast_session(session)?;
        resolve_album_membership(yt, album).await
    }

    pub(in crate::probe::backend) async fn write_album_membership_once(
        &self,
        session: &Arc<dyn CatalogSession>,
        album: &AlbumId,
        expected_playlist: &PlaylistId,
        saved: bool,
    ) -> Result<(), CatalogMutationError> {
        let yt = Self::downcast_session(session).map_err(CatalogMutationError::Rejected)?;
        let proof = resolve_album_membership(yt, album)
            .await
            .map_err(CatalogMutationError::Rejected)?;
        if let Some(query) = check_write_proof(&proof, expected_playlist, saved)? {
            yt.query(query).await.map_err(classify_mutation_error)?;
        }
        Ok(())
    }
}

async fn resolve_album_membership(
    yt: &YtMusic<BrowserToken>,
    album: &AlbumId,
) -> Result<BackendAlbumMembership, QueryError> {
    if !has_safe_suffix(album.as_str(), "MPREb_") {
        return Err(make_query_error(
            QueryErrorCode::InvalidPayload,
            false,
            None,
            "album membership requires a canonical album browse ID",
        ));
    }
    let detail = yt
        .json_query::<GetAlbumQuery<'_>>(GetAlbumQuery::new(AlbumID::from_raw(album.as_str())))
        .await
        .map_err(classify_ytmapi_error)?;
    let detail = album_detail_evidence(&detail.into_inner()).ok_or_else(unproven_album)?;
    let search = SearchQuery::new_filtered(detail.title.as_str(), AlbumsFilter);
    let first = yt
        .json_query::<SearchQuery<'_, FilteredSearch<AlbumsFilter>>>(&search)
        .await
        .map_err(classify_ytmapi_error)?;
    let playlist = &detail.proof.playlist_id;
    match album_search_evidence(album, playlist, &first.into_inner(), false) {
        Some(SearchEvidence::Bound) => return Ok(detail.proof),
        Some(SearchEvidence::Missing {
            continuation: Some(token),
        }) => {
            let next = fetch_continuation_page(yt, &search, &token).await?;
            if matches!(
                album_search_evidence(album, playlist, &next.into_inner(), true),
                Some(SearchEvidence::Bound)
            ) {
                return Ok(detail.proof);
            }
        }
        _ => {}
    }
    Err(unproven_album())
}

fn album_detail_evidence(value: &Value) -> Option<AlbumDetailEvidence> {
    let url = Url::parse(text_at(value, CANONICAL_URL)?).ok()?;
    if url.scheme() != "https"
        || url.host_str() != Some("music.youtube.com")
        || url.path() != "/playlist"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let mut params = url.query_pairs();
    let (name, id) = params.next()?;
    if name != "list" || params.next().is_some() || !has_safe_suffix(&id, "OLAK5uy_") {
        return None;
    }

    let header = value.pointer(ALBUM_HEADER)?;
    let title: String = header
        .pointer("/title/runs")?
        .as_array()?
        .iter()
        .map(|run| text_at(run, "/text"))
        .collect::<Option<_>>()?;
    let title = title.trim();
    if title.is_empty() || title.chars().count() > 256 || title.chars().any(char::is_control) {
        return None;
    }
    let buttons = header.get("buttons")?.as_array()?;
    let mut play = buttons
        .iter()
        .filter_map(|button| button.get("musicPlayButtonRenderer"));
    let play_id = play_playlist_id(play.next()?.get("playNavigationEndpoint")?)?;
    if play_id != id || play.next().is_some() {
        return None;
    }
    let saved = library_button_state(buttons.iter(), &id)?;
    let playlist_id = PlaylistId::new(id.into_owned()).ok()?;
    Some(AlbumDetailEvidence {
        title: title.to_owned(),
        proof: BackendAlbumMembership { playlist_id, saved },
    })
}

fn album_search_evidence(
    album: &AlbumId,
    playlist: &PlaylistId,
    value: &Value,
    continuation: bool,
) -> Option<SearchEvidence> {
    let shelf = if continuation {
        value.pointer(SEARCH_CONTINUATION)?
    } else {
        let mut shelves = value
            .pointer(SEARCH_SECTIONS)?
            .as_array()?
            .iter()
            .filter_map(|section| section.get("musicShelfRenderer"));
        let shelf = shelves.next()?;
        if shelves.next().is_some() {
            return None;
        }
        shelf
    };
    let mut bound = false;
    for row in shelf.get("contents")?.as_array()? {
        let Some(row) = row.get("musicResponsiveListItemRenderer") else {
            continue;
        };
        let Some(endpoint) = row.pointer(ALBUM_BROWSE) else {
            continue;
        };
        if text_at(endpoint, "/browseId") != Some(album.as_str()) {
            continue;
        }
        if text_at(endpoint, PAGE_TYPE) != Some("MUSIC_PAGE_TYPE_ALBUM")
            || play_playlist_id(row.pointer(ALBUM_OVERLAY)?) != Some(playlist.as_str())
        {
            return None;
        }
        bound = true;
    }
    if bound {
        return Some(SearchEvidence::Bound);
    }

    let continuation = match shelf.get("continuations") {
        None => None,
        Some(value) => match value.as_array()?.as_slice() {
            [] => None,
            [entry] => Some(text_at(entry, "/nextContinuationData/continuation")?.to_owned()),
            _ => return None,
        },
    };
    Some(SearchEvidence::Missing {
        continuation: continuation.filter(|token| !token.is_empty()),
    })
}

fn play_playlist_id(endpoint: &Value) -> Option<&str> {
    let mut id = None;
    for path in [
        "/watchEndpoint/playlistId",
        "/watchEndpoint/watchPlaylistId",
        "/watchPlaylistEndpoint/playlistId",
        "/watchPlaylistEndpoint/watchPlaylistId",
    ] {
        if let Some(candidate) = endpoint.pointer(path) {
            let candidate = candidate.as_str()?;
            if !has_safe_suffix(candidate, "OLAK5uy_") || id.is_some_and(|known| known != candidate)
            {
                return None;
            }
            id = Some(candidate);
        }
    }
    id
}

fn has_safe_suffix(id: &str, prefix: &str) -> bool {
    id.strip_prefix(prefix).is_some_and(|suffix| {
        !suffix.is_empty()
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    })
}

fn check_write_proof<'a>(
    proof: &'a BackendAlbumMembership,
    expected_playlist: &PlaylistId,
    saved: bool,
) -> Result<Option<RatePlaylistQuery<'a>>, CatalogMutationError> {
    if &proof.playlist_id != expected_playlist {
        return Err(CatalogMutationError::Rejected(make_query_error(
            QueryErrorCode::StaleContext,
            false,
            None,
            "album audio playlist changed before membership dispatch",
        )));
    }
    Ok(
        (proof.saved != saved)
            .then(|| playlist_membership_query(proof.playlist_id.as_str(), saved)),
    )
}

fn unproven_album() -> QueryError {
    make_query_error(
        QueryErrorCode::UnsupportedCapability,
        false,
        None,
        "provider did not confirm the same album identity and library action",
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const ALBUM: &str = "MPREb_fixture";
    const PLAYLIST: &str = "OLAK5uy_fixture";
    const OTHER_PLAYLIST: &str = "OLAK5uy_reissued";

    fn detail(canonical: &str, play: &str, add: &str, remove: &str, saved: bool) -> Value {
        json!({
            "microformat": {"microformatDataRenderer": {"urlCanonical": canonical}},
            "contents": {"twoColumnBrowseResultsRenderer": {"tabs": [{"tabRenderer": {
                "content": {"sectionListRenderer": {"contents": [{
                    "musicResponsiveHeaderRenderer": {
                        "title": {"runs": [{"text": "Synthetic "}, {"text": "Album"}]},
                        "buttons": [
                            {"musicPlayButtonRenderer": {"playNavigationEndpoint": {
                                "watchPlaylistEndpoint": {"watchPlaylistId": play}
                            }}},
                            {"toggleButtonRenderer": {
                                "isToggled": saved, "isDisabled": false,
                                "defaultIcon": {"iconType": "LIBRARY_ADD"},
                                "toggledIcon": {"iconType": "LIBRARY_SAVED"},
                                "defaultServiceEndpoint": {"likeEndpoint": {"status": "LIKE", "target": {"playlistId": add}}},
                                "toggledServiceEndpoint": {"likeEndpoint": {"status": "INDIFFERENT", "target": {"playlistId": remove}}}
                            }}
                        ]
                    }
                }]}}
            }}]}}
        })
    }

    fn search_row(album: &str, playlist: &str) -> Value {
        json!({"musicResponsiveListItemRenderer": {
            "navigationEndpoint": {"browseEndpoint": {
                "browseId": album,
                "browseEndpointContextSupportedConfigs": {
                    "browseEndpointContextMusicConfig": {"pageType": "MUSIC_PAGE_TYPE_ALBUM"}
                }
            }},
            "overlay": {"musicItemThumbnailOverlayRenderer": {"content": {
                "musicPlayButtonRenderer": {"playNavigationEndpoint": {
                    "watchEndpoint": {"playlistId": playlist}
                }}
            }}}
        }})
    }

    fn search_page(rows: Vec<Value>) -> Value {
        json!({"contents": {"tabbedSearchResultsRenderer": {"tabs": [{"tabRenderer": {
            "content": {"sectionListRenderer": {"contents": [{"musicShelfRenderer": {
                "contents": rows,
                "continuations": [{"nextContinuationData": {"continuation": "fixture-next"}}]
            }}]}}
        }}]}}})
    }

    #[test]
    fn detail_requires_the_same_canonical_play_and_library_playlist() {
        let url = format!("https://music.youtube.com/playlist?list={PLAYLIST}");
        for saved in [false, true] {
            let proof =
                album_detail_evidence(&detail(&url, PLAYLIST, PLAYLIST, PLAYLIST, saved)).unwrap();
            assert_eq!(proof.title, "Synthetic Album");
            assert_eq!(proof.proof.playlist_id.as_str(), PLAYLIST);
            assert_eq!(proof.proof.saved, saved);
        }
        for (play, add, remove) in [
            (OTHER_PLAYLIST, PLAYLIST, PLAYLIST),
            (PLAYLIST, OTHER_PLAYLIST, PLAYLIST),
            (PLAYLIST, PLAYLIST, OTHER_PLAYLIST),
        ] {
            assert!(album_detail_evidence(&detail(&url, play, add, remove, false)).is_none());
        }
        for canonical in [
            format!("https://other.example/playlist?list={PLAYLIST}"),
            format!("{url}&list={OTHER_PLAYLIST}"),
        ] {
            assert!(
                album_detail_evidence(&detail(&canonical, PLAYLIST, PLAYLIST, PLAYLIST, false))
                    .is_none()
            );
        }
        let mut unsigned = detail(&url, PLAYLIST, PLAYLIST, PLAYLIST, false);
        let toggle =
            unsigned.pointer_mut(ALBUM_HEADER).unwrap()["buttons"][1]["toggleButtonRenderer"]
                .as_object_mut()
                .unwrap();
        toggle.remove("defaultServiceEndpoint");
        assert!(album_detail_evidence(&unsigned).is_none());
    }

    #[test]
    fn bookmark_toggle_needs_the_complete_icon_pair_and_exact_actions() {
        let url = format!("https://music.youtube.com/playlist?list={PLAYLIST}");
        let mut album = detail(&url, PLAYLIST, PLAYLIST, PLAYLIST, false);
        let pointer = format!("{ALBUM_HEADER}/buttons/1/toggleButtonRenderer");
        let toggle = album.pointer_mut(&pointer).unwrap();
        toggle["defaultIcon"]["iconType"] = json!("BOOKMARK_BORDER");
        toggle["toggledIcon"]["iconType"] = json!("BOOKMARK");
        assert!(!album_detail_evidence(&album).unwrap().proof.saved);

        album.pointer_mut(&pointer).unwrap()["isToggled"] = json!(true);
        assert!(album_detail_evidence(&album).unwrap().proof.saved);

        let mut conflicting_pair = album.clone();
        conflicting_pair.pointer_mut(&pointer).unwrap()["toggledIcon"]["iconType"] =
            json!("LIBRARY_SAVED");
        assert!(album_detail_evidence(&conflicting_pair).is_none());

        album.pointer_mut(&pointer).unwrap()["toggledServiceEndpoint"]["likeEndpoint"]["target"] =
            json!({"playlistId": OTHER_PLAYLIST});
        assert!(album_detail_evidence(&album).is_none());
    }

    #[test]
    fn albums_filter_binds_only_the_requested_row_and_rejects_conflicts() {
        let album = AlbumId::new(ALBUM).unwrap();
        let playlist = PlaylistId::new(PLAYLIST).unwrap();
        let unrelated = search_row("MPREb_other", PLAYLIST);
        let exact = search_row(ALBUM, PLAYLIST);
        let conflicting = search_row(ALBUM, OTHER_PLAYLIST);
        let missing = search_page(vec![unrelated.clone()]);
        assert!(
            matches!(album_search_evidence(&album, &playlist, &missing, false), Some(SearchEvidence::Missing { continuation: Some(token) }) if token == "fixture-next")
        );
        let present = search_page(vec![unrelated, exact.clone()]);
        assert!(matches!(
            album_search_evidence(&album, &playlist, &present, false),
            Some(SearchEvidence::Bound)
        ));

        let conflicting_page = search_page(vec![exact.clone(), conflicting]);
        assert!(album_search_evidence(&album, &playlist, &conflicting_page, false).is_none());
        let continuation =
            json!({"continuationContents": {"musicShelfContinuation": {"contents": [exact]}}});
        assert!(matches!(
            album_search_evidence(&album, &playlist, &continuation, true),
            Some(SearchEvidence::Bound)
        ));
    }

    #[test]
    fn membership_write_is_bound_to_the_journaled_playlist_even_when_already_satisfied() {
        let playlist = PlaylistId::new(PLAYLIST).unwrap();
        let other = PlaylistId::new(OTHER_PLAYLIST).unwrap();
        let proof = BackendAlbumMembership {
            playlist_id: playlist.clone(),
            saved: false,
        };
        assert!(
            check_write_proof(&proof, &playlist, false)
                .unwrap()
                .is_none()
        );
        assert!(
            matches!(check_write_proof(&proof, &other, false), Err(CatalogMutationError::Rejected(error)) if error.code == QueryErrorCode::StaleContext)
        );
        let query = check_write_proof(&proof, &playlist, true).unwrap().unwrap();
        assert_eq!(query.path(), "like/like");
        assert_eq!(query.header()["target"], json!({"playlistId": PLAYLIST}));
        let saved = BackendAlbumMembership {
            saved: true,
            ..proof
        };
        assert_eq!(
            check_write_proof(&saved, &playlist, false)
                .unwrap()
                .unwrap()
                .path(),
            "like/removelike"
        );
    }
}
