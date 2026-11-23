use serde::Serialize;
use ytm_protocol::validate_id_str;
use ytmapi_rs::common::{EpisodeID, PodcastChannelID, PodcastID, UploadAlbumID, UploadArtistID};
use ytmapi_rs::parse::{
    Episode, GetLibraryUploadAlbum, GetPodcast, ParsedPodcastChannel, TableListUploadSong,
    UploadAlbum, UploadArtist,
};
use ytmapi_rs::query::{
    GetChannelEpisodesQuery, GetChannelQuery, GetEpisodeQuery, GetLibraryPodcastsQuery,
    GetLibraryUploadAlbumQuery, GetLibraryUploadAlbumsQuery, GetLibraryUploadArtistQuery,
    GetLibraryUploadArtistsQuery, GetLibraryUploadSongsQuery, GetPodcastQuery,
};

use super::*;

const UPLOADED_ALBUM_PREFIX: &str = "FEmusic_library_privately_owned_release_detail";
const UPLOADED_ARTIST_PREFIX: &str = "FEmusic_library_privately_owned_artist_detail";
const PODCAST_SHELF: &str = concat!(
    "/contents/twoColumnBrowseResultsRenderer/secondaryContents/sectionListRenderer",
    "/contents/0/musicShelfRenderer"
);
const PODCAST_SHELF_CONTINUATION: &str = "/continuationContents/musicShelfContinuation";
const FIRST_GRID: &str = concat!(
    "/contents/singleColumnBrowseResultsRenderer/tabs/0/tabRenderer/content/sectionListRenderer",
    "/contents/0/gridRenderer"
);
const LIBRARY_GRID_CONTINUATION: &str = "/continuationContents/gridContinuation";
const NEXT_TOKEN: &str = "/continuations/0/nextContinuationData/continuation";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtendedMediaQuery<'a> {
    UploadSongs,
    UploadAlbums,
    UploadArtists,
    UploadAlbum(&'a BrowseId),
    UploadArtist(&'a BrowseId),
    LibraryPodcasts,
    Podcast(&'a BrowseId),
    PodcastChannel(&'a BrowseId),
    PodcastChannelEpisodes(&'a BrowseId),
    PodcastEpisode(&'a BrowseId),
}

impl<'a> ExtendedMediaQuery<'a> {
    pub const fn source(self) -> &'static str {
        match self {
            Self::UploadSongs => "youtube_music_uploaded_songs",
            Self::UploadAlbums => "youtube_music_uploaded_albums",
            Self::UploadArtists => "youtube_music_uploaded_artists",
            Self::UploadAlbum(_) => "youtube_music_uploaded_album",
            Self::UploadArtist(_) => "youtube_music_uploaded_artist",
            Self::LibraryPodcasts => "youtube_music_library_podcasts",
            Self::Podcast(_) => "youtube_music_podcast",
            Self::PodcastChannel(_) => "youtube_music_podcast_channel",
            Self::PodcastChannelEpisodes(_) => "youtube_music_channel_episodes",
            Self::PodcastEpisode(_) => "youtube_music_podcast_episode",
        }
    }

    pub(crate) fn id(self) -> Option<&'a BrowseId> {
        match self {
            Self::UploadAlbum(id)
            | Self::UploadArtist(id)
            | Self::Podcast(id)
            | Self::PodcastChannel(id)
            | Self::PodcastChannelEpisodes(id)
            | Self::PodcastEpisode(id) => Some(id),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum ExtendedMediaKind {
    UploadedSong,
    UploadedAlbum,
    UploadedArtist,
    Podcast,
    PodcastChannel,
    PodcastEpisode,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProviderExtendedItem {
    pub kind: ExtendedMediaKind,
    pub id: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub description: Option<String>,
    pub video_id: Option<VideoId>,
    pub related_id: Option<BrowseId>,
    pub duration: Option<String>,
    pub published: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendExtendedPage {
    pub entity: Option<ProviderExtendedItem>,
    pub items: Vec<ProviderExtendedItem>,
    pub next_continuation: Option<String>,
    pub fully_projected: bool,
}

impl BackendExtendedPage {
    fn items(
        items: Vec<ProviderExtendedItem>,
        next_continuation: Option<String>,
    ) -> BackendExtendedPage {
        Self {
            entity: None,
            items,
            next_continuation,
            fully_projected: true,
        }
    }

    fn with_entity(mut self, entity: ProviderExtendedItem) -> Self {
        self.entity = Some(entity);
        self
    }
}

impl YtmapiCatalogBackend {
    pub(super) async fn read_extended_media(
        &self,
        session: &Arc<dyn CatalogSession>,
        request: ExtendedMediaQuery<'_>,
        continuation: Option<&str>,
    ) -> Result<BackendExtendedPage, QueryError> {
        let yt = Self::downcast_session(session)?;
        use ExtendedMediaQuery::*;
        match request {
            UploadSongs => {
                let query = GetLibraryUploadSongsQuery::default();
                if continuation.is_some() {
                    return paged_upload(yt, &query, continuation, upload_song).await;
                }
                let json = read_browse(yt, &query, None).await?;
                parse_upload_songs_first(&query, json)
            }
            UploadAlbums => {
                paged_upload(
                    yt,
                    &GetLibraryUploadAlbumsQuery::default(),
                    continuation,
                    upload_album,
                )
                .await
            }
            UploadArtists => {
                paged_upload(
                    yt,
                    &GetLibraryUploadArtistsQuery::default(),
                    continuation,
                    upload_artist,
                )
                .await
            }
            UploadArtist(id) => {
                let id = check_family(id, UPLOADED_ARTIST_PREFIX)?;
                let query = GetLibraryUploadArtistQuery::new(UploadArtistID::from_raw(id));
                paged_upload(yt, &query, continuation, upload_song).await
            }
            UploadAlbum(id) => {
                one_response(continuation)?;
                let id = check_family(id, UPLOADED_ALBUM_PREFIX)?;
                let query = GetLibraryUploadAlbumQuery::new(UploadAlbumID::from_raw(id));
                let album = yt.query(query).await.map_err(classify_ytmapi_error)?;
                project_upload_album(id, album)
            }
            LibraryPodcasts => {
                let query = GetLibraryPodcastsQuery::default();
                let json = read_browse(yt, &query, continuation).await?;
                parse_library_podcasts(json.into_inner(), continuation.is_some())
            }
            Podcast(id) => {
                let raw_id = check_family(id, "MPSP")?;
                let query = GetPodcastQuery::new(PodcastID::from_raw(raw_id));
                let json = read_browse(yt, &query, continuation).await?;
                if continuation.is_some() {
                    let value = json.into_inner();
                    return parse_podcast_continuation(&value, id);
                }
                let value = serde_json::to_value(&json).map_err(|_| invalid_shape())?;
                let shelf = value.pointer(PODCAST_SHELF).ok_or_else(invalid_shape)?;
                let next = continuation_token(shelf);
                let fully_projected = next.is_some() || !has_continuation(shelf);
                let podcast = GetPodcast::parse_from(ProcessedResult {
                    query: &query,
                    source: value.to_string(),
                    json,
                })
                .map_err(classify_ytmapi_error)?;
                let mut result = project_podcast(id, podcast, next)?;
                result.fully_projected = fully_projected;
                Ok(result)
            }
            PodcastEpisode(id) => {
                one_response(continuation)?;
                let id = check_family(id, "MPED")?;
                let query = GetEpisodeQuery::new(EpisodeID::from_raw(id));
                let episode = yt.query(query).await.map_err(classify_ytmapi_error)?;
                let mut item = summary(ExtendedMediaKind::PodcastEpisode, id, episode.title)?;
                item.subtitle = nonempty(episode.podcast_name);
                item.related_id = BrowseId::new(episode.podcast_id.get_raw()).ok();
                item.description = nonempty(episode.description);
                item.duration = nonempty(episode.total_duration);
                item.published = nonempty(episode.date);
                Ok(BackendExtendedPage::items(vec![], None).with_entity(item))
            }
            PodcastChannel(id) | PodcastChannelEpisodes(id) => {
                one_response(continuation)?;
                let raw_id = check_family(id, "UC")?;
                let channel_id = PodcastChannelID::from_raw(raw_id);
                let channel = yt
                    .query(GetChannelQuery::new(channel_id.clone()))
                    .await
                    .map_err(classify_ytmapi_error)?;
                let entity = summary(ExtendedMediaKind::PodcastChannel, raw_id, channel.title)?;
                if matches!(request, PodcastChannelEpisodes(_)) {
                    let params = channel.episode_params.ok_or_else(|| {
                        make_query_error(
                            QueryErrorCode::UnsupportedCapability,
                            false,
                            None,
                            "this podcast channel did not provide an all-episodes tab",
                        )
                    })?;
                    let query = GetChannelEpisodesQuery::new(channel_id, params);
                    let json = read_browse(yt, &query, None).await?;
                    let value = serde_json::to_value(&json).map_err(|_| invalid_shape())?;
                    let grid = value.pointer(FIRST_GRID).ok_or_else(invalid_shape)?;
                    let episodes = Vec::<Episode>::parse_from(ProcessedResult {
                        query: &query,
                        source: value.to_string(),
                        json,
                    })
                    .map_err(classify_ytmapi_error)?;
                    let items = episodes
                        .into_iter()
                        .map(|episode| podcast_episode(episode, None))
                        .collect::<Result<Vec<_>, _>>()?;
                    let mut result = BackendExtendedPage::items(items, None).with_entity(entity);
                    result.fully_projected = !has_continuation(grid);
                    return Ok(result);
                }
                let mut items = channel
                    .episodes
                    .into_iter()
                    .map(|episode| podcast_episode(episode, None))
                    .collect::<Result<Vec<_>, _>>()?;
                for podcast in channel.podcasts {
                    items.push(podcast_summary(
                        podcast.podcast_id.get_raw(),
                        podcast.title,
                        &podcast.channels,
                    )?);
                }
                // The Playlists carousel is not projected, so a channel that has one is not complete.
                let has_playlists = !channel.playlists.is_empty();
                let mut result = BackendExtendedPage::items(items, None).with_entity(entity);
                result.fully_projected = channel.episode_params.is_none() && !has_playlists;
                Ok(result)
            }
        }
    }
}

fn parse_upload_songs_first(
    query: &GetLibraryUploadSongsQuery,
    json: Json,
) -> Result<BackendExtendedPage, QueryError> {
    let value = serde_json::to_value(&json).map_err(|_| invalid_shape())?;
    if authenticated_uploads_message(&value).is_some_and(is_empty_uploads_banner) {
        return Ok(BackendExtendedPage::items(Vec::new(), None));
    }
    let (items, next) = Vec::<TableListUploadSong>::parse_from_continuable(ProcessedResult {
        query,
        source: value.to_string(),
        json,
    })
    .map_err(classify_ytmapi_error)?;
    let items = items
        .into_iter()
        .map(upload_song)
        .collect::<Result<_, _>>()?;
    Ok(BackendExtendedPage::items(
        items,
        next.map(|token| token.get_raw().to_owned()),
    ))
}

fn authenticated_uploads_message(value: &Value) -> Option<&Value> {
    let log_in_flags = value
        .pointer("/responseContext/serviceTrackingParams")?
        .as_array()?
        .iter()
        .filter_map(|service| service.get("params")?.as_array())
        .flatten()
        .filter(|param| {
            matches!(
                param.get("key").and_then(Value::as_str),
                Some("logged_in" | "yt_li")
            )
        });
    let mut signed_in = false;
    for marker in log_in_flags {
        if marker.get("value").and_then(Value::as_str) != Some("1") {
            return None;
        }
        signed_in = true;
    }
    if !signed_in {
        return None;
    }

    let tabs = value
        .pointer("/contents/singleColumnBrowseResultsRenderer/tabs")?
        .as_array()?;
    let (last, others) = tabs.split_last()?;
    let tab = last.get("tabRenderer")?;
    if tab.get("selected").and_then(Value::as_bool) != Some(true)
        || others.iter().any(|other| {
            other
                .pointer("/tabRenderer/selected")
                .and_then(Value::as_bool)
                == Some(true)
        })
    {
        return None;
    }
    let section = tab.pointer("/content/sectionListRenderer")?;
    if section.get("continuations").is_some()
        || !section.get("header").is_some_and(Value::is_object)
    {
        return None;
    }
    let [entry] = section.get("contents")?.as_array()?.as_slice() else {
        return None;
    };
    let items = entry.get("itemSectionRenderer")?;
    if items.get("continuations").is_some() {
        return None;
    }
    let [message] = items.get("contents")?.as_array()?.as_slice() else {
        return None;
    };
    let message = message.get("messageRenderer")?;
    if ["button", "buttons", "navigationEndpoint", "serviceEndpoint"]
        .iter()
        .any(|key| message.get(key).is_some())
    {
        return None;
    }
    Some(message)
}

fn is_empty_uploads_banner(message: &Value) -> bool {
    if text(message, "/icon/iconType") != Some("AUDIOTRACK") || message.get("subtext").is_none() {
        return false;
    }
    runs(message, "/text/runs").is_some_and(|title| {
        matches!(
            title
                .trim()
                .trim_end_matches('.')
                .to_ascii_lowercase()
                .as_str(),
            "no songs"
                | "no songs yet"
                | "no songs here"
                | "no songs found"
                | "no songs to show"
                | "no songs to display"
                | "no songs uploaded"
                | "no songs uploaded yet"
                | "no songs in your uploads"
                | "no songs in your library"
                | "no songs in this library"
        )
    })
}

async fn read_browse<Q>(
    yt: &YtMusic<BrowserToken>,
    query: &Q,
    continuation: Option<&str>,
) -> Result<Json, QueryError>
where
    Q: PostQuery + Query<BrowserToken>,
{
    if let Some(token) = continuation {
        fetch_continuation_page(yt, query, token).await
    } else {
        yt.json_query::<Q>(query)
            .await
            .map_err(classify_ytmapi_error)
    }
}

async fn paged_upload<Q, T>(
    yt: &YtMusic<BrowserToken>,
    query: &Q,
    continuation: Option<&str>,
    project: fn(T) -> Result<ProviderExtendedItem, QueryError>,
) -> Result<BackendExtendedPage, QueryError>
where
    Q: PostQuery + Query<BrowserToken>,
    Vec<T>: ParseFromContinuable<Q> + Send,
{
    let (items, next) = run_continuable_page::<_, Vec<T>>(yt, query, continuation).await?;
    let items = items
        .into_iter()
        .map(project)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(BackendExtendedPage::items(items, next))
}

fn check_family<'a>(id: &'a BrowseId, prefix: &str) -> Result<&'a str, QueryError> {
    let id = id.as_str();
    if id.strip_prefix(prefix).is_none_or(str::is_empty) {
        return Err(make_query_error(
            QueryErrorCode::InvalidPayload,
            false,
            None,
            "the browse ID does not match the requested extended media type",
        ));
    }
    Ok(id)
}

fn one_response(continuation: Option<&str>) -> Result<(), QueryError> {
    if continuation.is_some() {
        return Err(make_query_error(
            QueryErrorCode::UnsupportedCapability,
            false,
            None,
            "this extended media view has no supported provider continuation",
        ));
    }
    Ok(())
}

fn invalid_shape() -> QueryError {
    make_query_error(
        QueryErrorCode::InvalidPayload,
        false,
        None,
        "YouTube Music did not return a recognized extended media result",
    )
}

fn summary(
    kind: ExtendedMediaKind,
    id: &str,
    title: String,
) -> Result<ProviderExtendedItem, QueryError> {
    if validate_id_str(id).is_err() || title.is_empty() {
        return Err(invalid_shape());
    }
    Ok(ProviderExtendedItem {
        kind,
        id: id.into(),
        title,
        subtitle: None,
        description: None,
        video_id: None,
        related_id: None,
        duration: None,
        published: None,
    })
}

fn upload_song(song: TableListUploadSong) -> Result<ProviderExtendedItem, QueryError> {
    let mut item = summary(
        ExtendedMediaKind::UploadedSong,
        song.video_id.get_raw(),
        song.title,
    )?;
    item.video_id = Some(VideoId::new(song.video_id.get_raw()).map_err(|_| invalid_shape())?);
    item.duration = nonempty(song.duration);
    item.related_id = song
        .album
        .and_then(|album| BrowseId::new(album.id.get_raw()).ok());
    item.subtitle = nonempty(
        song.artists
            .into_iter()
            .map(|artist| artist.name)
            .collect::<Vec<_>>()
            .join(", "),
    );
    Ok(item)
}

fn upload_album(album: UploadAlbum) -> Result<ProviderExtendedItem, QueryError> {
    let mut item = summary(
        ExtendedMediaKind::UploadedAlbum,
        album.album_id.get_raw(),
        album.title,
    )?;
    item.subtitle = album.subtitle.and_then(nonempty);
    item.published = album.year.and_then(nonempty);
    Ok(item)
}

fn upload_artist(artist: UploadArtist) -> Result<ProviderExtendedItem, QueryError> {
    let mut item = summary(
        ExtendedMediaKind::UploadedArtist,
        artist.artist_id.get_raw(),
        artist.artist_name,
    )?;
    item.subtitle = nonempty(artist.song_count);
    Ok(item)
}

fn project_upload_album(
    id: &str,
    album: GetLibraryUploadAlbum,
) -> Result<BackendExtendedPage, QueryError> {
    let mut entity = summary(ExtendedMediaKind::UploadedAlbum, id, album.title)?;
    entity.subtitle = nonempty(album.artist_name);
    entity.duration = nonempty(album.duration);
    let items = album
        .songs
        .into_iter()
        .map(|song| {
            let mut item = summary(
                ExtendedMediaKind::UploadedSong,
                song.video_id.get_raw(),
                song.title,
            )?;
            item.video_id =
                Some(VideoId::new(song.video_id.get_raw()).map_err(|_| invalid_shape())?);
            item.duration = nonempty(song.duration);
            item.related_id = BrowseId::new(song.album.id.get_raw()).ok();
            Ok(item)
        })
        .collect::<Result<Vec<_>, QueryError>>()?;
    Ok(BackendExtendedPage::items(items, None).with_entity(entity))
}

fn podcast_summary(
    id: &str,
    title: String,
    channels: &[ParsedPodcastChannel],
) -> Result<ProviderExtendedItem, QueryError> {
    let mut item = summary(ExtendedMediaKind::Podcast, id, title)?;
    item.subtitle = nonempty(
        channels
            .iter()
            .map(|channel| channel.name.as_str())
            .collect::<Vec<_>>()
            .join(""),
    );
    item.related_id = channels
        .iter()
        .filter_map(|channel| channel.id.as_ref())
        .find_map(|channel| BrowseId::new(channel.get_raw()).ok());
    Ok(item)
}

fn podcast_episode(
    episode: Episode,
    parent: Option<&BrowseId>,
) -> Result<ProviderExtendedItem, QueryError> {
    let mut item = summary(
        ExtendedMediaKind::PodcastEpisode,
        episode.episode_id.get_raw(),
        episode.title,
    )?;
    item.duration = nonempty(episode.total_duration);
    item.published = nonempty(episode.date);
    item.description = nonempty(episode.description);
    item.related_id = parent.cloned();
    Ok(item)
}

fn project_podcast(
    id: &BrowseId,
    podcast: GetPodcast,
    next: Option<String>,
) -> Result<BackendExtendedPage, QueryError> {
    let mut entity = podcast_summary(id.as_str(), podcast.title, &podcast.channels)?;
    entity.description = nonempty(podcast.description);
    let items = podcast
        .episodes
        .into_iter()
        .map(|episode| podcast_episode(episode, Some(id)))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(BackendExtendedPage::items(items, next).with_entity(entity))
}

fn parse_library_podcasts(
    value: Value,
    continuation: bool,
) -> Result<BackendExtendedPage, QueryError> {
    let grid = value
        .pointer(if continuation {
            LIBRARY_GRID_CONTINUATION
        } else {
            FIRST_GRID
        })
        .ok_or_else(invalid_shape)?;
    let cards = grid
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(invalid_shape)?;
    let mut items = Vec::new();
    for card in cards {
        let Some(row) = card.get("musicTwoRowItemRenderer") else {
            continue;
        };
        let Some(id) = text(row, "/navigationEndpoint/browseEndpoint/browseId")
            .or_else(|| {
                text(
                    row,
                    "/title/runs/0/navigationEndpoint/browseEndpoint/browseId",
                )
            })
            .filter(|id| id.starts_with("MPSP"))
        else {
            continue;
        };
        let title = runs(row, "/title/runs").ok_or_else(invalid_shape)?;
        let mut item = summary(ExtendedMediaKind::Podcast, id, title)?;
        item.subtitle = runs(row, "/subtitle/runs");
        item.related_id = text(
            row,
            "/subtitle/runs/0/navigationEndpoint/browseEndpoint/browseId",
        )
        .and_then(|id| BrowseId::new(id).ok());
        items.push(item);
    }
    Ok(browse_page(items, grid))
}

fn parse_podcast_continuation(
    value: &Value,
    parent: &BrowseId,
) -> Result<BackendExtendedPage, QueryError> {
    let shelf = value
        .pointer(PODCAST_SHELF_CONTINUATION)
        .ok_or_else(invalid_shape)?;
    let cards = shelf
        .get("contents")
        .and_then(Value::as_array)
        .ok_or_else(invalid_shape)?;
    let mut items = Vec::new();
    for card in cards {
        let Some(row) = card.get("musicMultiRowListItemRenderer") else {
            continue;
        };
        let id = text(
            row,
            "/title/runs/0/navigationEndpoint/browseEndpoint/browseId",
        )
        .filter(|id| id.starts_with("MPED"))
        .ok_or_else(invalid_shape)?;
        let mut item = summary(
            ExtendedMediaKind::PodcastEpisode,
            id,
            runs(row, "/title/runs").ok_or_else(invalid_shape)?,
        )?;
        item.description = runs(row, "/description/runs");
        item.duration = text(
            row,
            "/playbackProgress/musicPlaybackProgressRenderer/durationText/runs/1/text",
        )
        .map(str::to_owned);
        item.published = runs(row, "/subtitle/runs");
        item.related_id = Some(parent.clone());
        items.push(item);
    }
    let mut page = browse_page(items, shelf);
    page.fully_projected &= page.items.len() == cards.len();
    Ok(page)
}

fn browse_page(items: Vec<ProviderExtendedItem>, contents: &Value) -> BackendExtendedPage {
    let next = continuation_token(contents);
    let fully_projected = next.is_some() || !has_continuation(contents);
    let mut page = BackendExtendedPage::items(items, next);
    page.fully_projected = fully_projected;
    page
}

fn continuation_token(contents: &Value) -> Option<String> {
    text(contents, NEXT_TOKEN).map(str::to_owned)
}

fn has_continuation(contents: &Value) -> bool {
    contents
        .get("continuations")
        .and_then(Value::as_array)
        .is_some_and(|value| !value.is_empty())
}

fn nonempty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

fn text<'a>(value: &'a Value, pointer: &str) -> Option<&'a str> {
    value
        .pointer(pointer)?
        .as_str()
        .filter(|value| !value.is_empty())
}

fn runs(value: &Value, pointer: &str) -> Option<String> {
    nonempty(
        value
            .pointer(pointer)?
            .as_array()?
            .iter()
            .filter_map(|run| text(run, "/text"))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn upload_message_page(logged_in: &str, icon: &str, label: &str) -> Value {
        let sections = json!({
            "header": {},
            "contents": [{"itemSectionRenderer": {"contents": [{
                "messageRenderer": {
                    "icon": {"iconType": icon},
                    "text": {"runs": [{"text": label}]},
                    "subtext": {"simpleText": "Fixture message detail"}
                }
            }]}}]
        });
        json!({
            "responseContext": {"serviceTrackingParams": [{"params": [
                {"key": "logged_in", "value": logged_in},
                {"key": "yt_li", "value": logged_in}
            ]}]},
            "contents": {"singleColumnBrowseResultsRenderer": {"tabs": [
                {"tabRenderer": {"selected": false, "content": {"sectionListRenderer": {
                    "continuations": [{"nextContinuationData": {"continuation": "other-tab"}}]
                }}}},
                {"tabRenderer": {"selected": true, "content": {"sectionListRenderer": sections}}}
            ]}}
        })
    }

    #[test]
    fn signed_in_uploads_empty_panel_returns_a_complete_page() {
        let page = upload_message_page("1", "AUDIOTRACK", "No songs");
        let parsed = parse_upload_songs_first(
            &GetLibraryUploadSongsQuery::default(),
            serde_json::from_value(page).unwrap(),
        )
        .unwrap();
        assert!(parsed.items.is_empty());
        assert!(parsed.next_continuation.is_none());
        assert!(parsed.fully_projected);
    }

    #[test]
    fn generic_upload_messages_and_incomplete_pages_keep_the_parse_failure() {
        let selected_path = "/contents/singleColumnBrowseResultsRenderer/tabs/1/tabRenderer";
        let mut mixed = upload_message_page("1", "AUDIOTRACK", "No songs");
        mixed.pointer_mut(selected_path).unwrap()["content"]["sectionListRenderer"]["continuations"] =
            json!([{"nextContinuationData": {"continuation": "unexpected"}}]);
        let mut unselected = upload_message_page("1", "AUDIOTRACK", "No songs");
        unselected.pointer_mut(selected_path).unwrap()["selected"] = json!(false);
        let mut mixed_auth = upload_message_page("1", "AUDIOTRACK", "No songs");
        mixed_auth["responseContext"]["serviceTrackingParams"][0]["params"][1]["value"] =
            json!("0");
        for value in [
            upload_message_page("0", "AUDIOTRACK", "No songs"),
            upload_message_page(
                "1",
                "AUDIOTRACK",
                "No songs can be uploaded by this account",
            ),
            upload_message_page("1", "MUSIC_NOTE", "No songs"),
            mixed,
            unselected,
            mixed_auth,
            json!({}),
        ] {
            assert_eq!(
                parse_upload_songs_first(
                    &GetLibraryUploadSongsQuery::default(),
                    serde_json::from_value(value).unwrap(),
                )
                .unwrap_err()
                .code,
                QueryErrorCode::InvalidPayload
            );
        }
    }

    #[test]
    fn uploaded_song_projection_keeps_exact_id_and_omits_the_delete_handle() {
        let parsed = serde_json::from_value::<TableListUploadSong>(json!({
            "entity_id": "private-delete-fixture",
            "video_id": "upload-track-fixture",
            "album": {"name": "Album", "id": "FEmusic_library_privately_owned_release_detailFixture"},
            "duration": "4:00",
            "like_status": "INDIFFERENT",
            "title": "Fixture upload",
            "artists": [{"name": "Artist", "id": null}],
            "thumbnails": []
        }))
        .unwrap();
        let song = upload_song(parsed).unwrap();
        assert_eq!(song.kind, ExtendedMediaKind::UploadedSong);
        assert_eq!(
            song.video_id.as_ref().unwrap().as_str(),
            "upload-track-fixture"
        );
        assert!(check_family(song.related_id.as_ref().unwrap(), UPLOADED_ALBUM_PREFIX).is_ok());
        assert!(check_family(song.related_id.as_ref().unwrap(), "MPSP").is_err());
        assert!(
            !serde_json::to_string(&song)
                .unwrap()
                .contains("private-delete-fixture")
        );
    }

    #[test]
    fn podcast_library_uses_entity_types_and_preserves_first_show_without_an_action_tile() {
        let card = |id, title| {
            json!({"musicTwoRowItemRenderer": {
                "title": {"runs": [{"text": title}]},
                "navigationEndpoint": {"browseEndpoint": {"browseId": id}}
            }})
        };
        let page = json!({"contents": {"singleColumnBrowseResultsRenderer": {"tabs": [{"tabRenderer": {
            "content": {"sectionListRenderer": {"contents": [{"gridRenderer": {
                "items": [card("MPSPfixture", "Fixture show"), card("VLSE", "Saved"),
                    card("VLRDPN", "Recent"), card("MPSPfixture2", "Fixture second")],
                "continuations": [{"nextContinuationData": {"continuation": "library-next"}}]
            }}]}}
        }}]}}});
        let parsed = parse_library_podcasts(page, false).unwrap();
        assert_eq!(
            parsed
                .items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["MPSPfixture", "MPSPfixture2"]
        );
        assert_eq!(parsed.next_continuation.as_deref(), Some("library-next"));
    }

    #[test]
    fn podcast_continuation_keeps_episode_identity_and_provider_duration() {
        let podcast = BrowseId::new("MPSPfixture-show").unwrap();
        let json = json!({"continuationContents": {"musicShelfContinuation": {
            "contents": [{"musicMultiRowListItemRenderer": {
                "title": {"runs": [{"text": "Fixture episode", "navigationEndpoint": {
                    "browseEndpoint": {"browseId": "MPEDfixture-episode"}
                }}]},
                "description": {"runs": [{"text": "Description"}]},
                "subtitle": {"runs": [{"text": "Nov 7"}]},
                "playbackProgress": {"musicPlaybackProgressRenderer": {
                    "durationText": {"runs": [{"text": " • "}, {"text": "25 min"}]}
                }}
            }}],
            "continuations": [{"nextContinuationData": {"continuation": "episodes-next"}}]
        }}});
        let parsed = parse_podcast_continuation(&json, &podcast).unwrap();
        assert_eq!(parsed.next_continuation.as_deref(), Some("episodes-next"));
        assert_eq!(parsed.items[0].kind, ExtendedMediaKind::PodcastEpisode);
        assert_eq!(parsed.items[0].id, "MPEDfixture-episode");
        assert_eq!(parsed.items[0].duration.as_deref(), Some("25 min"));
        assert_eq!(parsed.items[0].related_id.as_ref(), Some(&podcast));
        assert!(parsed.items[0].video_id.is_none());
    }
}
