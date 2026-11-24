//! What an agent may know about the listener beyond the queue: what they just did by hand in the
//! player, and what their own library and history say about their taste.
//!
//! Both are facts, not judgements: the player reports that the volume went from 70 to 30, or that
//! nine in ten of the liked songs are written in Latin script; what that means is the agent's call.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ytm_protocol::PlaylistId;

use crate::DjTrack;

/// Most listener actions in one prompt or context read.
pub const MAX_LISTENER_ACTIONS: usize = 12;

/// Something the listener did by hand: in the player window, from the tray, with media keys or
/// through MPRIS. Commands sent by any agent are never listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListenerAction {
    pub action: ListenerActionKind,
    pub ago_ms: u64,
    /// What was playing when they acted, and where in it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub during: Option<DjTrack>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_ms: Option<u64>,
    /// The track the action was about (picked, queued, rated): the playing one when it was about
    /// that. Absent for actions with no track of their own, and while the player cannot name the
    /// track yet (for example one just picked from search).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<DjTrack>,
    /// Volume before and after, 0 to 100. A drag is one action from its first to its last value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_from: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_to: Option<u8>,
    /// Where a seek landed. A drag is one action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seek_to_ms: Option<u64>,
    /// The new setting in plain words: `on` or `off`, a repeat mode, a rating (`liked`,
    /// `disliked`, `neutral`), or the label of the chosen Up Next filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 64))]
    pub value: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ListenerActionKind {
    Play,
    Pause,
    Stop,
    /// Skipped to the next track.
    Next,
    /// Went back: to the previous track, or to the start of this one.
    Previous,
    Seek,
    Volume,
    Mute,
    Shuffle,
    Repeat,
    Autoplay,
    /// Started a particular track.
    PlayTrack,
    /// Started an album, playlist or other collection.
    PlayCollection,
    /// Jumped to an entry of the queue.
    PickFromQueue,
    AddToQueue,
    RemoveFromQueue,
    ReorderQueue,
    ClearUpcoming,
    /// Chose an Up Next filter.
    UpNextFilter,
    /// Liked, disliked or cleared a rating in the app.
    Rate,
    AddToPlaylist,
}

/// Input of a listener profile read.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListenerProfileInput {
    /// Read the library again instead of using the copy kept for this session.
    #[serde(default)]
    pub refresh: bool,
}

/// A compact picture of the signed-in listener's own music, built only from their library and
/// play history. Every part says how much it was built from; a part that could not be read is
/// named in `unavailable` and is not an empty result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListenerProfile {
    pub generated_at_ms: u64,
    pub liked: ProfileSample,
    /// Plays recorded by this player on this machine.
    pub history: ProfileSample,
    pub playlists: Vec<ProfilePlaylist>,
    /// More playlists exist than listed.
    pub more_playlists: bool,
    /// The writing systems of the titles and artist names in the sampled liked songs and plays,
    /// largest first. A track counts once, under the script of most of its letters. It describes
    /// what the listener listens to; it is not their language, country or interface language.
    pub scripts: Vec<ScriptShare>,
    /// Tracks the script figures were counted over.
    pub scripts_sampled: u32,
    /// Taste notes stored for this account (read them with the DJ prompt or the taste tools).
    pub taste_notes: u32,
    /// Parts that could not be read this time: `liked`, `history`, `playlists`, `tasteNotes`.
    pub unavailable: Vec<String>,
}

/// One source of the profile: how much of it was read, who appears most, and its newest tracks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileSample {
    /// Tracks in the source as a whole, when the source says.
    pub total: Option<u32>,
    /// Tracks actually read for this profile.
    pub sampled: u32,
    /// Artists with the most sampled tracks, most first.
    pub top_artists: Vec<ProfileArtist>,
    /// The newest sampled tracks, newest first.
    pub recent: Vec<DjTrack>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileArtist {
    pub name: String,
    pub tracks: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfilePlaylist {
    pub playlist_id: PlaylistId,
    pub title: String,
    pub track_count: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScriptShare {
    pub script: Script,
    pub tracks: u32,
    pub percent: u8,
}

/// A writing system, as told from the letters of a title and its artist names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Script {
    Latin,
    /// Chinese characters without kana: Chinese, or Japanese written only in kanji.
    Han,
    /// Japanese kana, with or without kanji.
    Kana,
    Hangul,
    Cyrillic,
    Arabic,
    Devanagari,
    Thai,
    Hebrew,
    Greek,
    /// Another script, or no letters at all.
    Other,
}
