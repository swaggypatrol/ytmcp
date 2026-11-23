use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ytm_protocol::{AlbumId, ArtistId, PlaylistId};

use crate::{PlaylistPrivacy, TrackTarget};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShareInput {
    pub target: ShareTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ShareTarget {
    Track { target: TrackTarget },
    Playlist { playlist_id: PlaylistId },
    Album { album_id: AlbumId },
    Artist { artist_id: ArtistId },
}

/// Access evidence for an individual recipient remains unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ShareAccess {
    Unknown,
    Private,
    Unlisted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShareLink {
    pub url: String,
    pub visibility: Option<PlaylistPrivacy>,
    pub access: ShareAccess,
}
