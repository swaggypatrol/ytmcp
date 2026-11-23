use super::*;

const LIBRARY_ALBUMS_BROWSE_ID: &str = "FEmusic_liked_albums";
const FIRST_TABS: &str = "/contents/singleColumnBrowseResultsRenderer/tabs";
const NEXT_CONTINUATION: &str = "/nextContinuationData/continuation";
const INLINE_CONTINUATION: &str = "/continuationCommand/token";
const COLUMN_TITLE: &str = "/flexColumns/0/musicResponsiveListItemFlexColumnRenderer/text";
const COLUMN_ARTIST_RUNS: &str =
    "/flexColumns/1/musicResponsiveListItemFlexColumnRenderer/text/runs";

pub(super) async fn read_page(
    yt: &YtMusic<BrowserToken>,
    continuation: Option<&str>,
) -> Result<BackendPage<ProviderLibraryItem>, QueryError> {
    let query = GetLibraryAlbumsQuery::default();
    let json = if let Some(continuation) = continuation {
        fetch_continuation_page(yt, &query, continuation).await?
    } else {
        yt.json_query::<GetLibraryAlbumsQuery>(&query)
            .await
            .map_err(classify_ytmapi_error)?
    };
    parse_page(&json.into_inner(), continuation.is_some())
}

fn parse_page(
    value: &Value,
    continuation: bool,
) -> Result<BackendPage<ProviderLibraryItem>, QueryError> {
    let mut page = BackendPage {
        items: Vec::new(),
        next_continuation: None,
    };
    if continuation {
        let (renderer, rows_key) = continuation_rows(value).ok_or_else(invalid_page)?;
        append_rows(&mut page, renderer, rows_key)?;
        return Ok(page);
    }

    let tab = selected_tab(value).ok_or_else(invalid_page)?;
    let content = tab.get("content").ok_or_else(invalid_page)?;
    if explicit_library_context(value) && is_empty_saved_albums_tab(tab, content) {
        return Ok(page);
    }
    let sections = content
        .pointer("/sectionListRenderer/contents")
        .or_else(|| content.pointer("/itemSectionRenderer/contents"))
        .and_then(Value::as_array)
        .ok_or_else(invalid_page)?;
    let mut found_section = false;
    for part in sections.iter().flat_map(|section| {
        section
            .pointer("/itemSectionRenderer/contents")
            .and_then(Value::as_array)
            .map_or_else(
                || std::slice::from_ref(section).iter(),
                |items| items.iter(),
            )
    }) {
        if let Some(renderer) = part.get("gridRenderer") {
            append_rows(&mut page, renderer, "items")?;
            found_section = true;
        } else if let Some(renderer) = part.get("musicShelfRenderer") {
            append_rows(&mut page, renderer, "contents")?;
            found_section = true;
        }
    }
    let has_results = !page.items.is_empty() || page.next_continuation.is_some();
    if found_section && (has_results || explicit_library_context(value)) {
        Ok(page)
    } else {
        Err(invalid_page())
    }
}

fn selected_tab(value: &Value) -> Option<&Value> {
    let tabs = value.pointer(FIRST_TABS)?.as_array()?;
    if let [only] = tabs.as_slice() {
        return only.get("tabRenderer");
    }
    let mut selected = tabs.iter().filter_map(|tab| {
        let renderer = tab.get("tabRenderer")?;
        let selected = renderer.get("selected").and_then(Value::as_bool) == Some(true);
        selected.then_some(renderer)
    });
    let tab = selected.next()?;
    selected.next().is_none().then_some(tab)
}

fn is_empty_saved_albums_tab(tab: &Value, content: &Value) -> bool {
    let Some(message) = empty_message(tab, content) else {
        return false;
    };
    let label = message.get("text").and_then(display_text);
    label.is_some_and(|label| {
        label
            .trim()
            .trim_end_matches('.')
            .eq_ignore_ascii_case("No albums yet")
    })
}

fn empty_message<'a>(tab: &Value, content: &'a Value) -> Option<&'a Value> {
    if tab.get("selected").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let sections = content.get("sectionListRenderer")?;
    if sections.get("continuations").is_some()
        || !sections.get("header").is_some_and(Value::is_object)
    {
        return None;
    }
    let [section] = sections.get("contents")?.as_array()?.as_slice() else {
        return None;
    };
    let items = section.get("itemSectionRenderer")?;
    if items.get("continuations").is_some() {
        return None;
    }
    let [item] = items.get("contents")?.as_array()?.as_slice() else {
        return None;
    };
    let message = item.get("messageRenderer")?;
    if [
        "button",
        "buttons",
        "navigationEndpoint",
        "serviceEndpoint",
        "command",
    ]
    .iter()
    .any(|key| message.get(key).is_some())
        || message
            .pointer("/text/runs")
            .and_then(Value::as_array)
            .is_some_and(|runs| {
                runs.iter()
                    .any(|run| run.get("navigationEndpoint").is_some())
            })
    {
        return None;
    }
    Some(message)
}

fn continuation_rows(value: &Value) -> Option<(&Value, &'static str)> {
    [
        ("/continuationContents/gridContinuation", "items"),
        ("/continuationContents/musicShelfContinuation", "contents"),
    ]
    .into_iter()
    .find_map(|(path, rows)| Some((value.pointer(path)?, rows)))
    .or_else(|| {
        let mut actions = ["onResponseReceivedActions", "onResponseReceivedEndpoints"]
            .into_iter()
            .filter_map(|key| value.get(key)?.as_array())
            .flatten()
            .filter_map(|action| action.get("appendContinuationItemsAction"));
        let action = actions.next()?;
        actions
            .next()
            .is_none()
            .then_some((action, "continuationItems"))
    })
}

fn append_rows(
    page: &mut BackendPage<ProviderLibraryItem>,
    renderer: &Value,
    rows_key: &str,
) -> Result<(), QueryError> {
    let rows = renderer
        .get(rows_key)
        .and_then(Value::as_array)
        .ok_or_else(invalid_page)?;
    if let Some(continuations) = renderer.get("continuations") {
        for continuation in continuations.as_array().ok_or_else(invalid_page)? {
            remember_continuation(page, text(continuation, NEXT_CONTINUATION))?;
        }
    }
    for row in rows {
        if let Some(sentinel) = row.get("continuationItemRenderer") {
            let endpoint = sentinel
                .get("continuationEndpoint")
                .ok_or_else(invalid_page)?;
            let token = text(endpoint, INLINE_CONTINUATION).or_else(|| {
                endpoint
                    .pointer("/commandExecutorCommand/commands")?
                    .as_array()?
                    .iter()
                    .find_map(|command| text(command, INLINE_CONTINUATION))
            });
            remember_continuation(page, token)?;
        } else {
            page.items.push(parse_album(row).ok_or_else(invalid_page)?);
        }
    }
    Ok(())
}

fn remember_continuation(
    page: &mut BackendPage<ProviderLibraryItem>,
    token: Option<&str>,
) -> Result<(), QueryError> {
    let token = token.ok_or_else(invalid_page)?;
    if let Some(existing) = &page.next_continuation {
        if existing != token {
            return Err(invalid_page());
        }
    } else {
        page.next_continuation = Some(token.into());
    }
    Ok(())
}

fn parse_album(row: &Value) -> Option<ProviderLibraryItem> {
    let (card, title, byline, plain_artist) = if let Some(card) = row.get("musicTwoRowItemRenderer")
    {
        (
            card,
            card.get("title")?,
            card.pointer("/subtitle/runs"),
            album_artist_label(card),
        )
    } else {
        let card = row.get("musicResponsiveListItemRenderer")?;
        (
            card,
            card.pointer(COLUMN_TITLE)?,
            card.pointer(COLUMN_ARTIST_RUNS),
            None,
        )
    };
    let id = text(card, "/navigationEndpoint/browseEndpoint/browseId")
        .or_else(|| text(title, "/runs/0/navigationEndpoint/browseEndpoint/browseId"))?;
    if id.strip_prefix("MPRE").is_none_or(str::is_empty) {
        return None;
    }
    let album_id = AlbumId::new(id).ok()?;
    let mut artists = byline
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|run| {
            let id = text(run, "/navigationEndpoint/browseEndpoint/browseId")?;
            if !id.starts_with("UC") {
                return None;
            }
            Some(ArtistRef {
                id: Some(ArtistId::new(id).ok()?),
                name: text(run, "/text")?.into(),
            })
        })
        .collect::<Vec<_>>();
    if artists.is_empty()
        && let Some(name) = plain_artist
    {
        artists.push(ArtistRef {
            id: None,
            name: name.into(),
        });
    }
    Some(ProviderLibraryItem::Album {
        album_id,
        title: display_text(title)?,
        artists,
    })
}

fn album_artist_label(card: &Value) -> Option<&str> {
    let type_label = text(card, "/subtitle/runs/0/text")?;
    if !matches!(
        type_label.to_ascii_lowercase().as_str(),
        "album" | "single" | "ep"
    ) {
        return None;
    }
    let artist = text(card, "/subtitle/runs/2/text")?;
    (!artist.bytes().all(|byte| byte.is_ascii_digit())).then_some(artist)
}

fn display_text(value: &Value) -> Option<String> {
    if let Some(text) = text(value, "/simpleText") {
        return Some(text.into());
    }
    let joined: String = value
        .get("runs")?
        .as_array()?
        .iter()
        .filter_map(|run| text(run, "/text"))
        .collect();
    (!joined.is_empty()).then_some(joined)
}

fn explicit_library_context(value: &Value) -> bool {
    let tracking = value
        .pointer("/responseContext/serviceTrackingParams")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|service| service.get("params")?.as_array())
        .flatten();
    let mut logged_in = false;
    let mut route_matched = false;
    for entry in tracking {
        match text(entry, "/key") {
            Some("logged_in" | "yt_li") if text(entry, "/value") == Some("1") => logged_in = true,
            Some("browse_id") if text(entry, "/value") == Some(LIBRARY_ALBUMS_BROWSE_ID) => {
                route_matched = true;
            }
            Some("logged_in" | "yt_li" | "browse_id") => return false,
            _ => {}
        }
    }
    logged_in && route_matched
}

fn text<'a>(value: &'a Value, pointer: &str) -> Option<&'a str> {
    value
        .pointer(pointer)?
        .as_str()
        .filter(|text| !text.is_empty())
}

fn invalid_page() -> QueryError {
    make_query_error(
        QueryErrorCode::InvalidPayload,
        false,
        None,
        "YouTube Music did not return a recognized saved album page",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn album(id: &str, name: &str) -> Value {
        json!({"musicTwoRowItemRenderer": {
            "title": {"runs": [{"text": name}]},
            "navigationEndpoint": {"browseEndpoint": {"browseId": id}}
        }})
    }

    fn first(sections: Vec<Value>) -> Value {
        json!({
            "contents": {"singleColumnBrowseResultsRenderer": {"tabs": [
                {"tabRenderer": {"selected": false}},
                {"tabRenderer": {"selected": true, "content": {"sectionListRenderer": {
                    "contents": sections
                }}}}
            ]}}
        })
    }

    fn authenticated_page_with_message(text: &str) -> Value {
        let mut page = first(vec![json!({"itemSectionRenderer": {"contents": [{
            "messageRenderer": {"text": {"runs": [{"text": text}]}}
        }]}})]);
        page["responseContext"]["serviceTrackingParams"] = json!([{"params": [
            {"key": "logged_in", "value": "1"},
            {"key": "browse_id", "value": "FEmusic_liked_albums"}
        ]}]);
        page["contents"]["singleColumnBrowseResultsRenderer"]["tabs"][1]["tabRenderer"]["content"]
            ["sectionListRenderer"]["header"] = json!({});
        page
    }

    #[test]
    fn saved_album_grid_uses_current_route_and_optional_display_metadata() {
        let query = GetLibraryAlbumsQuery::default();
        assert_eq!(query.path(), "browse");
        assert_eq!(query.header()["browseId"], "FEmusic_liked_albums");

        let first = first(vec![
            json!({"musicHeaderRenderer": {}}),
            json!({"itemSectionRenderer": {"contents": [{"gridRenderer": {
                "items": [album("MPREfixture", "Saved album")],
                "continuations": [{"nextContinuationData": {"continuation": "grid-next"}}]
            }}]}}),
        ]);
        let page = parse_page(&first, false).unwrap();
        assert!(matches!(
            &page.items[..],
            [ProviderLibraryItem::Album {album_id, title, artists}]
                if album_id.as_str() == "MPREfixture" && title == "Saved album" && artists.is_empty()
        ));
        assert_eq!(page.next_continuation.as_deref(), Some("grid-next"));
    }

    #[test]
    fn saved_albums_preserve_native_continuations_and_linked_artist_ids() {
        let linked_artist = json!({"text": "Artist", "navigationEndpoint": {
            "browseEndpoint": {"browseId": "UCfixture"}
        }});
        let mut legacy = album("MPRElegacy", "Legacy");
        legacy["musicTwoRowItemRenderer"]["subtitle"]["runs"] = json!([linked_artist.clone()]);
        let legacy_value =
            json!({"continuationContents": {"gridContinuation": {"items": [legacy]}}});
        let legacy_page = parse_page(&legacy_value, true).unwrap();
        assert!(legacy_page.next_continuation.is_none());
        assert!(
            matches!(&legacy_page.items[0], ProviderLibraryItem::Album {artists, ..}
            if artists[0].id.as_ref().unwrap().as_str() == "UCfixture")
        );

        let responsive = json!({"musicResponsiveListItemRenderer": {
            "flexColumns": [
                {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "Next"}]}}},
                {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [linked_artist]}}}
            ],
            "navigationEndpoint": {"browseEndpoint": {"browseId": "MPREnext"}}
        }});
        let further = json!({"continuationItemRenderer": {"continuationEndpoint": {
            "commandExecutorCommand": {"commands": [{"continuationCommand": {"token": "more"}}]}
        }}});
        let append = json!({"onResponseReceivedActions": [{"appendContinuationItemsAction": {
            "continuationItems": [responsive, further]
        }}]});
        let page = parse_page(&append, true).unwrap();
        assert_eq!(page.next_continuation.as_deref(), Some("more"));
        assert!(
            matches!(&page.items[0], ProviderLibraryItem::Album { title, artists, .. }
            if title == "Next" && artists[0].id.as_ref().unwrap().as_str() == "UCfixture")
        );
    }

    #[test]
    fn saved_album_parser_rejects_ambiguous_empty_pages_and_invalid_rows() {
        let mut empty = first(vec![json!({"gridRenderer": {"items": []}})]);
        assert!(parse_page(&empty, false).is_err());
        empty["responseContext"]["serviceTrackingParams"] = json!([{"params": [
            {"key": "logged_in", "value": "1"},
            {"key": "browse_id", "value": "FEmusic_liked_albums"}
        ]}]);
        let page = parse_page(&empty, false).unwrap();
        assert!(page.items.is_empty());
        assert!(page.next_continuation.is_none());
        empty["responseContext"]["serviceTrackingParams"][0]["params"][0]["value"] = json!("0");
        assert!(parse_page(&empty, false).is_err());
        assert!(parse_page(&first(vec![json!({"messageRenderer": {}})]), false).is_err());
        assert!(
            parse_page(
                &first(vec![
                    json!({"gridRenderer": {"items": [album("VLnot-album", "Playlist")]}})
                ]),
                false,
            )
            .is_err()
        );
        assert!(
            parse_page(
                &json!({"continuationContents": {"gridContinuation": {
                    "items": [{"continuationItemRenderer": {"continuationEndpoint": {}}}]
                }}}),
                true
            )
            .is_err()
        );
    }

    #[test]
    fn saved_album_banner_requires_authenticated_exact_context_and_no_action() {
        let page = authenticated_page_with_message("No albums yet");
        let empty = parse_page(&page, false).unwrap();
        assert!(empty.items.is_empty());
        assert!(empty.next_continuation.is_none());

        let mut wrong_route = page.clone();
        wrong_route["responseContext"]["serviceTrackingParams"][0]["params"][1]["value"] =
            json!("FEmusic_liked_videos");
        let mut action = page;
        action["contents"]["singleColumnBrowseResultsRenderer"]["tabs"][1]["tabRenderer"]["content"]
            ["sectionListRenderer"]["contents"][0]["itemSectionRenderer"]["contents"][0]["messageRenderer"]
            ["text"]["runs"][0]["navigationEndpoint"] = json!({});
        for value in [
            wrong_route,
            action,
            authenticated_page_with_message("No albums can be saved by this account"),
        ] {
            assert_eq!(
                parse_page(&value, false).unwrap_err().code,
                QueryErrorCode::InvalidPayload
            );
        }
    }
}
