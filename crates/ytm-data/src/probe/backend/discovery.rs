use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use ytmapi_rs::YtMusicBuilder;
use ytmapi_rs::common::{MoodCategoryParams, SearchSuggestion, SuggestionType};
use ytmapi_rs::parse::{MoodCategorySection, MoodPlaylistCategory};
use ytmapi_rs::query::{GetMoodCategoriesQuery, GetMoodPlaylistsQuery, GetSearchSuggestionsQuery};

use super::*;

const FIRST_SECTIONS: &str = concat!(
    "/contents/singleColumnBrowseResultsRenderer/tabs/0/tabRenderer/content",
    "/sectionListRenderer"
);
const NEXT_SECTIONS: &str = "/continuationContents/sectionListContinuation";
const TITLE: &str = "/title/runs";
const ROW_TITLE: &str = "/flexColumns/0/musicResponsiveListItemFlexColumnRenderer/text/runs";
const ROW_ENDPOINT: &str = concat!(
    "/flexColumns/0/musicResponsiveListItemFlexColumnRenderer/text/runs/0",
    "/navigationEndpoint"
);
const CHART_COUNTRY_SELECTOR: &str = concat!(
    "/subheaders/0/musicSideAlignedItemRenderer/startItems/0",
    "/musicSortFilterButtonRenderer"
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiscoverySurface {
    Home,
    Explore,
    MoodsAndGenres,
    Charts,
    NewReleases,
}

impl DiscoverySurface {
    pub const fn source(self) -> &'static str {
        match self {
            Self::Home => "youtube_music_home",
            Self::Explore => "youtube_music_explore",
            Self::MoodsAndGenres => "youtube_music_moods_and_genres",
            Self::Charts => "youtube_music_charts",
            Self::NewReleases => "youtube_music_new_releases",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum ProviderDiscoveryTarget {
    Track(VideoId),
    Playlist(PlaylistId),
    Album(AlbumId),
    Artist(ArtistId),
    MoodCategory(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProviderDiscoveryItem {
    pub section: Option<String>,
    pub title: String,
    pub target: ProviderDiscoveryTarget,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendDiscoveryPage {
    pub items: Vec<ProviderDiscoveryItem>,
    pub next_continuation: Option<String>,
    pub fully_projected: bool,
}

struct DiscoveryBrowse(&'static str);

impl<A: AuthToken> Query<A> for DiscoveryBrowse {
    type Output = NoopParseOutput;
    type Method = PostMethod;
}

impl ParseFrom<DiscoveryBrowse> for NoopParseOutput {
    fn parse_from(_: ProcessedResult<DiscoveryBrowse>) -> ytmapi_rs::Result<Self> {
        Ok(Self)
    }
}

impl PostQuery for DiscoveryBrowse {
    fn header(&self) -> Map<String, Value> {
        Map::from_iter([("browseId".into(), json!(self.0))])
    }

    fn params(&self) -> Vec<(&str, Cow<'_, str>)> {
        vec![]
    }

    fn path(&self) -> &str {
        "browse"
    }
}

impl YtmapiCatalogBackend {
    pub(super) async fn read_discovery(
        &self,
        session: &Arc<dyn CatalogSession>,
        surface: DiscoverySurface,
        continuation: Option<&str>,
    ) -> Result<BackendDiscoveryPage, QueryError> {
        let yt = Self::downcast_session(session)?;
        let browse_id = match surface {
            DiscoverySurface::Home => "FEmusic_home",
            DiscoverySurface::Explore => "FEmusic_explore",
            DiscoverySurface::Charts => "FEmusic_charts",
            DiscoverySurface::NewReleases => "FEmusic_new_releases",
            DiscoverySurface::MoodsAndGenres => {
                if continuation.is_some() {
                    return Err(make_query_error(
                        QueryErrorCode::InvalidPayload,
                        false,
                        None,
                        "moods and genres does not use a provider continuation",
                    ));
                }
                let sections = yt
                    .query(GetMoodCategoriesQuery)
                    .await
                    .map_err(classify_ytmapi_error)?;
                return Ok(project_moods(sections));
            }
        };

        let query = DiscoveryBrowse(browse_id);
        let value = if let Some(token) = continuation {
            fetch_continuation_page(yt, &query, token).await?
        } else {
            yt.json_query::<DiscoveryBrowse>(query)
                .await
                .map_err(classify_ytmapi_error)?
        };
        parse_discovery(value.into_inner(), surface, continuation.is_some())
    }

    pub(super) async fn read_mood_category(
        &self,
        session: &Arc<dyn CatalogSession>,
        category_id: &str,
        scope_key: &[u8; 32],
    ) -> Result<BackendDiscoveryPage, QueryError> {
        let yt = Self::downcast_session(session)?;
        let sections = yt
            .query(GetMoodCategoriesQuery)
            .await
            .map_err(classify_ytmapi_error)?;
        let params = resolve_mood_category(sections, category_id, scope_key).ok_or_else(|| {
            make_query_error(
                QueryErrorCode::StaleContext,
                false,
                None,
                "mood category is no longer available; refresh Moods and Genres",
            )
        })?;
        let shelves = yt
            .query(GetMoodPlaylistsQuery::new(params))
            .await
            .map_err(classify_ytmapi_error)?;
        Ok(project_mood_playlists(shelves))
    }

    pub(super) async fn read_search_predictions(
        &self,
        query: &str,
    ) -> Result<Vec<String>, QueryError> {
        let yt = YtMusicBuilder::new_with_client(self.client.clone())
            .build()
            .await
            .map_err(classify_ytmapi_error)?;
        let suggestions = yt
            .query(GetSearchSuggestionsQuery::new(query))
            .await
            .map_err(classify_ytmapi_error)?;
        project_search_predictions(suggestions)
    }
}

pub(in crate::probe) fn scoped_mood_category_id(scope_key: &[u8; 32], id: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(scope_key);
    hash.update(id.as_bytes());
    hex::encode(hash.finalize())
}

fn resolve_mood_category(
    sections: Vec<MoodCategorySection>,
    category_id: &str,
    scope_key: &[u8; 32],
) -> Option<MoodCategoryParams<'static>> {
    sections
        .into_iter()
        .flat_map(|section| section.mood_categories)
        .map(|category| category.params)
        .find(|params| {
            !params.get_raw().is_empty()
                && scoped_mood_category_id(scope_key, &mood_digest(params.get_raw())) == category_id
        })
}

fn project_mood_playlists(shelves: Vec<MoodPlaylistCategory>) -> BackendDiscoveryPage {
    let mut items = Vec::new();
    let mut fully_projected = true;
    for shelf in shelves {
        for playlist in shelf.playlists {
            let id = playlist
                .playlist_id
                .get_raw()
                .strip_prefix("VL")
                .and_then(|id| PlaylistId::new(id).ok());
            match id {
                Some(id) if !playlist.title.is_empty() => items.push(ProviderDiscoveryItem {
                    section: Some(shelf.category_name.clone()),
                    title: playlist.title,
                    target: ProviderDiscoveryTarget::Playlist(id),
                }),
                _ => fully_projected = false,
            }
        }
    }
    BackendDiscoveryPage {
        items,
        next_continuation: None,
        fully_projected,
    }
}

fn project_search_predictions(
    suggestions: Vec<SearchSuggestion>,
) -> Result<Vec<String>, QueryError> {
    suggestions
        .into_iter()
        .filter(|item| item.suggestion_type == SuggestionType::Prediction)
        .map(|item| {
            let text = item.get_text();
            if text.trim().is_empty() || text.len() > 512 || text.chars().any(char::is_control) {
                return Err(make_query_error(
                    QueryErrorCode::InvalidPayload,
                    false,
                    None,
                    "YouTube Music returned an invalid search prediction",
                ));
            }
            Ok(text)
        })
        .collect()
}

fn project_moods(sections: Vec<MoodCategorySection>) -> BackendDiscoveryPage {
    let mut items = Vec::new();
    let mut fully_projected = true;
    for section in sections {
        for category in section.mood_categories {
            if let Some(target) = mood_target(category.params.get_raw()) {
                items.push(ProviderDiscoveryItem {
                    section: Some(section.section_name.clone()),
                    title: category.title,
                    target,
                });
            } else {
                fully_projected = false;
            }
        }
    }
    BackendDiscoveryPage {
        items,
        next_continuation: None,
        fully_projected,
    }
}

fn parse_discovery(
    value: Value,
    surface: DiscoverySurface,
    continuation: bool,
) -> Result<BackendDiscoveryPage, QueryError> {
    let sections = value.pointer(if continuation {
        NEXT_SECTIONS
    } else {
        FIRST_SECTIONS
    });
    let Some((list, rows)) =
        sections.and_then(|list| Some((list, list.get("contents")?.as_array()?)))
    else {
        return Err(make_query_error(
            QueryErrorCode::InvalidPayload,
            false,
            None,
            "YouTube Music did not return a recognized discovery page",
        ));
    };
    let mut next_continuation =
        string(list, "/continuations/0/nextContinuationData/continuation").map(str::to_owned);
    let advertised_more = list
        .get("continuations")
        .and_then(Value::as_array)
        .is_some_and(|continuations| !continuations.is_empty());
    let mut fully_projected = true;
    let mut items = Vec::new();
    for section in rows {
        if surface == DiscoverySurface::Charts
            && let Some(shelf) = section.get("musicShelfRenderer")
            && shelf.get("contents").is_none()
            && shelf.get("items").is_none()
            && shelf
                .pointer(CHART_COUNTRY_SELECTOR)
                .is_some_and(Value::is_object)
        {
            continue;
        }
        if let Some(inline) = section.get("continuationItemRenderer") {
            let next = string(inline, "/continuationEndpoint/continuationCommand/token");
            if next_continuation.is_none() {
                next_continuation = next.map(str::to_owned);
                fully_projected &= next.is_some();
            }
            continue;
        }
        let Some(shelf) = [
            "musicCarouselShelfRenderer",
            "musicImmersiveCarouselShelfRenderer",
            "musicShelfRenderer",
            "gridRenderer",
        ]
        .iter()
        .find_map(|key| section.get(key)) else {
            fully_projected = false;
            continue;
        };
        let heading = runs(
            shelf,
            "/header/musicCarouselShelfBasicHeaderRenderer/title/runs",
        )
        .or_else(|| runs(shelf, TITLE));
        let Some(cards) = shelf
            .get("contents")
            .or_else(|| shelf.get("items"))
            .and_then(Value::as_array)
        else {
            fully_projected = false;
            continue;
        };
        for card in cards {
            if let Some((title, target)) = discovery_card(card) {
                items.push(ProviderDiscoveryItem {
                    section: heading.clone(),
                    title,
                    target,
                });
            } else {
                fully_projected = false;
            }
        }
    }
    Ok(BackendDiscoveryPage {
        items,
        fully_projected: fully_projected && (!advertised_more || next_continuation.is_some()),
        next_continuation,
    })
}

fn discovery_card(card: &Value) -> Option<(String, ProviderDiscoveryTarget)> {
    if let Some(row) = card.get("musicTwoRowItemRenderer") {
        return Some((
            runs(row, TITLE)?,
            discovery_endpoint(row.get("navigationEndpoint")?)?,
        ));
    }
    if let Some(row) = card.get("musicResponsiveListItemRenderer") {
        let endpoint = row
            .get("navigationEndpoint")
            .or_else(|| row.pointer(ROW_ENDPOINT));
        let target = endpoint.and_then(discovery_endpoint).or_else(|| {
            VideoId::new(string(row, "/playlistItemData/videoId")?)
                .ok()
                .map(ProviderDiscoveryTarget::Track)
        });
        return Some((runs(row, ROW_TITLE)?, target?));
    }
    let button = card.get("musicNavigationButtonRenderer")?;
    let endpoint = button.pointer("/clickCommand/browseEndpoint")?;
    if string(endpoint, "/browseId") != Some("FEmusic_moods_and_genres_category") {
        return None;
    }
    Some((
        runs(button, "/buttonText/runs")?,
        mood_target(string(endpoint, "/params")?)?,
    ))
}

fn discovery_endpoint(endpoint: &Value) -> Option<ProviderDiscoveryTarget> {
    if let Some(id) = string(endpoint, "/watchEndpoint/videoId") {
        return VideoId::new(id).ok().map(ProviderDiscoveryTarget::Track);
    }
    if let Some(id) = string(endpoint, "/watchPlaylistEndpoint/playlistId") {
        return PlaylistId::new(strip_vl_prefix(id))
            .ok()
            .map(ProviderDiscoveryTarget::Playlist);
    }
    let browse = endpoint.get("browseEndpoint")?;
    let id = string(browse, "/browseId")?;
    match string(
        browse,
        "/browseEndpointContextSupportedConfigs/browseEndpointContextMusicConfig/pageType",
    )? {
        "MUSIC_PAGE_TYPE_ALBUM" => AlbumId::new(id).ok().map(ProviderDiscoveryTarget::Album),
        "MUSIC_PAGE_TYPE_ARTIST" => ArtistId::new(id).ok().map(ProviderDiscoveryTarget::Artist),
        "MUSIC_PAGE_TYPE_PLAYLIST" => PlaylistId::new(strip_vl_prefix(id))
            .ok()
            .map(ProviderDiscoveryTarget::Playlist),
        _ => None,
    }
}

fn mood_target(params: &str) -> Option<ProviderDiscoveryTarget> {
    (!params.is_empty()).then(|| ProviderDiscoveryTarget::MoodCategory(mood_digest(params)))
}

fn mood_digest(params: &str) -> String {
    hex::encode(Sha256::digest(params.as_bytes()))
}

fn string<'a>(value: &'a Value, pointer: &str) -> Option<&'a str> {
    value
        .pointer(pointer)?
        .as_str()
        .filter(|text| !text.is_empty())
}

fn runs(value: &Value, pointer: &str) -> Option<String> {
    let title: String = value
        .pointer(pointer)?
        .as_array()?
        .iter()
        .filter_map(|run| string(run, "/text"))
        .collect();
    (!title.is_empty() && title.len() <= 512).then_some(title)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mood_ids_resolve_only_against_the_current_scope_and_index() {
        let params = "opaque-upstream-mood-params";
        let index = serde_json::from_value::<Vec<MoodCategorySection>>(json!([{
            "section_name": "Moods",
            "mood_categories": [{"title": "Calm", "params": params}]
        }]))
        .unwrap();
        let digest = mood_digest(params);
        let current = [1; 32];
        let previous = [2; 32];
        let id = scoped_mood_category_id(&current, &digest);
        assert_ne!(id, digest);
        assert_eq!(
            resolve_mood_category(index.clone(), &id, &current)
                .unwrap()
                .get_raw(),
            params
        );
        assert!(resolve_mood_category(index.clone(), &id, &previous).is_none());
        assert!(resolve_mood_category(index, &digest, &current).is_none());
        assert!(resolve_mood_category(Vec::new(), &id, &current).is_none());
    }

    #[test]
    fn mood_drilldown_preserves_known_playlist_order_and_reports_invalid_rows() {
        let playlist = |id: &'static str, title: &str| ytmapi_rs::parse::MoodPlaylist {
            playlist_id: PlaylistID::from_raw(id),
            title: title.to_owned(),
            thumbnails: vec![],
            author: "Synthetic creator".to_owned(),
        };
        let page = project_mood_playlists(vec![MoodPlaylistCategory {
            category_name: "Synthetic shelf".to_owned(),
            playlists: vec![
                playlist("VLPL-second", "Second"),
                playlist("VL", "Invalid"),
                playlist("VLPL-first", "First"),
            ],
        }]);
        assert_eq!(page.items[0].section.as_deref(), Some("Synthetic shelf"));
        assert_eq!(
            page.items[0].target,
            ProviderDiscoveryTarget::Playlist(PlaylistId::new("PL-second").unwrap())
        );
        assert_eq!(
            page.items[1].target,
            ProviderDiscoveryTarget::Playlist(PlaylistId::new("PL-first").unwrap())
        );
        assert!(!page.fully_projected);
        assert!(page.next_continuation.is_none());

        let empty = json!({"contents": {"singleColumnBrowseResultsRenderer": {"tabs": [{
            "tabRenderer": {"content": {"sectionListRenderer": {"contents": []}}}
        }]}}});
        let query = GetMoodPlaylistsQuery::new(MoodCategoryParams::from_raw("fixture"));
        let shelves = ytmapi_rs::process_json::<GetMoodPlaylistsQuery<'_>, BrowserToken>(
            empty.to_string(),
            &query,
        )
        .unwrap();
        let page = project_mood_playlists(shelves);
        assert!(page.items.is_empty());
        assert!(page.fully_projected);
        assert!(
            ytmapi_rs::process_json::<GetMoodPlaylistsQuery<'_>, BrowserToken>("{}".into(), &query)
                .is_err()
        );
    }

    #[test]
    fn search_predictions_strip_history_and_recognize_only_typed_empty_sections() {
        use ytmapi_rs::auth::noauth::NoAuthToken;

        let query = GetSearchSuggestionsQuery::new("syn");
        assert_eq!(query.path(), "music/get_search_suggestions");
        assert_eq!(query.header()["input"], "syn");
        let fixture = |rows: Vec<Value>| {
            json!({"contents": [{"searchSuggestionsSectionRenderer": {"contents": rows}}]})
                .to_string()
        };
        let payload = fixture(vec![
            json!({"historySuggestionRenderer": {"suggestion": {"runs": [{"text": "Private fixture history"}]}}}),
            json!({"searchSuggestionRenderer": {"suggestion": {"runs": [{"text": "syn", "bold": true}, {"text": "thetic"}]}}}),
            json!({"searchSuggestionRenderer": {"suggestion": {"runs": [{"text": "synthetic second"}]}}}),
        ]);
        let suggestions =
            ytmapi_rs::process_json::<GetSearchSuggestionsQuery<'_>, NoAuthToken>(payload, &query)
                .unwrap();
        assert_eq!(
            project_search_predictions(suggestions).unwrap(),
            ["synthetic", "synthetic second"]
        );

        let empty = ytmapi_rs::process_json::<GetSearchSuggestionsQuery<'_>, NoAuthToken>(
            fixture(vec![]),
            &query,
        )
        .unwrap();
        assert!(project_search_predictions(empty).unwrap().is_empty());
        assert!(
            ytmapi_rs::process_json::<GetSearchSuggestionsQuery<'_>, NoAuthToken>(
                "{}".into(),
                &query
            )
            .is_err()
        );
    }

    #[test]
    fn home_projects_typed_targets_and_preserves_provider_continuation() {
        let home = DiscoveryBrowse("FEmusic_home");
        assert_eq!(home.path(), "browse");
        assert_eq!(home.header()["browseId"], "FEmusic_home");
        let page = json!({
            "contents": {"singleColumnBrowseResultsRenderer": {"tabs": [{"tabRenderer": {
                "content": {"sectionListRenderer": {
                    "contents": [{"musicCarouselShelfRenderer": {
                        "header": {"musicCarouselShelfBasicHeaderRenderer": {
                            "title": {"runs": [{"text": "Fixture picks"}]}
                        }},
                        "contents": [
                            {"musicTwoRowItemRenderer": {
                                "title": {"runs": [{"text": "Fixture album"}]},
                                "navigationEndpoint": {"browseEndpoint": {
                                    "browseId": "MPRE_fixture",
                                    "browseEndpointContextSupportedConfigs": {"browseEndpointContextMusicConfig": {
                                        "pageType": "MUSIC_PAGE_TYPE_ALBUM"
                                    }}
                                }}
                            }},
                            {"musicResponsiveListItemRenderer": {
                                "flexColumns": [{"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [
                                    {"text": "Fixture song"}
                                ]}}}],
                                "playlistItemData": {"videoId": "fixture-video"}
                            }}
                        ]
                    }}],
                    "continuations": [{"nextContinuationData": {"continuation": "fixture-next"}}]
                }}
            }}]}}
        });
        let parsed = parse_discovery(page, DiscoverySurface::Home, false).unwrap();
        assert!(parsed.fully_projected);
        assert_eq!(parsed.next_continuation.as_deref(), Some("fixture-next"));
        assert_eq!(parsed.items[0].section.as_deref(), Some("Fixture picks"));
        assert!(
            matches!(&parsed.items[0].target, ProviderDiscoveryTarget::Album(id) if id.as_str() == "MPRE_fixture")
        );
        assert!(
            matches!(&parsed.items[1].target, ProviderDiscoveryTarget::Track(id) if id.as_str() == "fixture-video")
        );
    }

    #[test]
    fn charts_and_releases_project_their_observed_shelves_in_order() {
        let page = |rows: Vec<Value>| {
            json!({"contents": {"singleColumnBrowseResultsRenderer": {"tabs": [{
                "tabRenderer": {"content": {"sectionListRenderer": {"contents": rows}}}
            }]}}})
        };
        let shelf = |title: &str, contents: Vec<Value>| {
            json!({"musicCarouselShelfRenderer": {
                "header": {"musicCarouselShelfBasicHeaderRenderer": {"title": {"runs": [{"text": title}]}}},
                "contents": contents
            }})
        };
        let card = |title: &str, endpoint: Value| {
            json!({"musicTwoRowItemRenderer": {
                "title": {"runs": [{"text": title}]}, "navigationEndpoint": endpoint
            }})
        };
        let browse_endpoint = |kind: &str, id: &str| {
            json!({"browseEndpoint": {
                "browseId": id,
                "browseEndpointContextSupportedConfigs": {"browseEndpointContextMusicConfig": {"pageType": kind}}
            }})
        };
        let albums = shelf(
            "Albums",
            vec![card(
                "First album",
                browse_endpoint("MUSIC_PAGE_TYPE_ALBUM", "MPRE_fixture"),
            )],
        );
        let chart = parse_discovery(
            page(vec![
                json!({"musicShelfRenderer": {"subheaders": [{"musicSideAlignedItemRenderer": {
                    "startItems": [{"musicSortFilterButtonRenderer": {}}]
                }}]}}),
                albums.clone(),
                shelf(
                    "Videos chart",
                    vec![card(
                        "Fixture playlist",
                        browse_endpoint("MUSIC_PAGE_TYPE_PLAYLIST", "VLPL-chart"),
                    )],
                ),
            ]),
            DiscoverySurface::Charts,
            false,
        )
        .unwrap();
        assert!(chart.fully_projected);
        assert!(matches!(
            chart.items[0].target,
            ProviderDiscoveryTarget::Album(_)
        ));
        assert!(
            matches!(&chart.items[1].target, ProviderDiscoveryTarget::Playlist(id) if id.as_str() == "PL-chart")
        );

        let releases = parse_discovery(
            page(vec![
                albums,
                shelf(
                    "New videos",
                    vec![card(
                        "Fixture video",
                        json!({"watchEndpoint": {"videoId": "release-video"}}),
                    )],
                ),
            ]),
            DiscoverySurface::NewReleases,
            false,
        )
        .unwrap();
        assert!(releases.fully_projected);
        assert_eq!(releases.items[1].section.as_deref(), Some("New videos"));
        assert!(
            matches!(&releases.items[1].target, ProviderDiscoveryTarget::Track(id) if id.as_str() == "release-video")
        );
    }

    #[test]
    fn continuation_preserves_mood_but_reports_an_unrepresented_category() {
        let raw = "synthetic-mood-payload";
        let value = json!({"continuationContents": {"sectionListContinuation": {"contents": [
            {"gridRenderer": {"items": [
                {"musicNavigationButtonRenderer": {
                    "buttonText": {"runs": [{"text": "Fixture mood"}]},
                    "clickCommand": {"browseEndpoint": {
                        "browseId": "FEmusic_moods_and_genres_category",
                        "params": raw
                    }}
                }},
                {"musicNavigationButtonRenderer": {
                    "buttonText": {"runs": [{"text": "Fixture charts"}]},
                    "clickCommand": {"browseEndpoint": {"browseId": "FEmusic_charts"}}
                }}
            ]}}
        ]}}});
        let parsed = parse_discovery(value, DiscoverySurface::Explore, true).unwrap();
        assert!(!parsed.fully_projected);
        assert!(parsed.next_continuation.is_none());
        assert_eq!(parsed.items.len(), 1);
        assert_eq!(parsed.items[0].target, mood_target(raw).unwrap());
        let target = serde_json::to_string(&parsed.items[0]).unwrap();
        assert!(!target.contains(raw));
        assert_eq!(
            parse_discovery(
                json!({"continuationContents": {}}),
                DiscoverySurface::Explore,
                true
            )
            .unwrap_err()
            .code,
            QueryErrorCode::InvalidPayload,
        );
    }
}
