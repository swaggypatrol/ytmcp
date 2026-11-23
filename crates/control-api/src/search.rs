use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ytm_protocol::{AlbumId, ArtistId, PlaylistId};

use crate::PageInput;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum EntitySearchKind {
    Album,
    Artist,
    Playlist,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EntitySearchInput {
    #[schemars(length(min = 1, max = 256))]
    pub query: String,
    pub kind: EntitySearchKind,
    pub page: Option<PageInput>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum SearchEntity {
    Album {
        album_id: AlbumId,
        title: String,
        artist: Option<String>,
        year: Option<u16>,
    },
    Artist {
        artist_id: ArtistId,
        name: String,
        subscribers: Option<String>,
    },
    Playlist {
        playlist_id: PlaylistId,
        title: String,
        byline: Option<String>,
        track_count: Option<u32>,
    },
}
