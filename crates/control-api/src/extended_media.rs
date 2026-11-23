use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ytm_protocol::{BrowseId, VideoId};

use crate::{Page, PageInput};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ExtendedMediaTarget {
    UploadSongs,
    UploadAlbums,
    UploadArtists,
    UploadAlbum { browse_id: BrowseId },
    UploadArtist { browse_id: BrowseId },
    LibraryPodcasts,
    Podcast { browse_id: BrowseId },
    PodcastChannel { browse_id: BrowseId },
    PodcastChannelEpisodes { browse_id: BrowseId },
    PodcastEpisode { browse_id: BrowseId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtendedMediaInput {
    pub target: ExtendedMediaTarget,
    pub page: Option<PageInput>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ExtendedMediaKind {
    UploadedSong,
    UploadedAlbum,
    UploadedArtist,
    Podcast,
    PodcastChannel,
    PodcastEpisode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtendedMediaItem {
    pub kind: ExtendedMediaKind,
    pub id: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub description: Option<String>,
    pub video_id: Option<VideoId>,
    pub related_id: Option<BrowseId>,
    pub duration_label: Option<String>,
    pub published_label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtendedMediaPage {
    pub entity: Option<ExtendedMediaItem>,
    pub entries: Page<ExtendedMediaItem>,
}
