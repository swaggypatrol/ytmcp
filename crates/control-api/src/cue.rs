//! Prompts the player hands a DJ agent: one before each track ends, and an occasional check-in
//! whose timing follows a points-based pseudo-random heartbeat. The agent decides what to do.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ytm_protocol::{CoreEpoch, PlaybackStatus, VideoId};

use crate::{DjPreference, MomentView, Track};

/// How often the heartbeat lets the agent check in with the listener.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum DjChattiness {
    Off,
    Quiet,
    #[default]
    Normal,
    Chatty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum DjCueKind {
    /// The current track is about to end.
    TrackEnding,
    /// A good moment to ask the listener whether the music still suits them.
    CheckIn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum DjCueReason {
    /// The listener has not done or said anything for a while.
    ListenerQuiet,
    /// The current track is long.
    LongTrack,
    /// Nothing in particular; it has simply been a while.
    Routine,
    /// At most one track is queued after the current one.
    LittleQueued,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum DjTrackOutcome {
    Finished,
    SkippedByListener,
    SkippedByAgent,
    /// Ended early without a command the player could attribute.
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum DjInteraction {
    /// A command from the player window, media keys or MPRIS.
    PlayerControl,
    /// The agent reported that the listener talked to it.
    Spoke,
}

/// The few track facts a DJ prompt needs; the music tools return the full track.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DjTrack {
    pub video_id: VideoId,
    pub title: String,
    /// Artist names.
    pub artists: Vec<String>,
    /// Album title.
    pub album: Option<String>,
    pub duration_ms: Option<u64>,
}

impl From<&Track> for DjTrack {
    fn from(track: &Track) -> Self {
        Self {
            video_id: track.video_id.clone(),
            title: track.title.clone(),
            artists: track
                .artists
                .iter()
                .map(|artist| artist.name.clone())
                .collect(),
            album: track.album.as_ref().map(|album| album.title.clone()),
            duration_ms: track.duration_ms,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DjRecentTrack {
    pub track: DjTrack,
    pub listened_ms: u64,
    pub outcome: DjTrackOutcome,
    pub ended_ago_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DjNowPlaying {
    pub track: DjTrack,
    pub playback: PlaybackStatus,
    pub position_ms: u64,
    pub remaining_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DjListener {
    pub last_interaction: Option<DjInteraction>,
    pub last_interaction_ago_ms: Option<u64>,
    /// When the agent last reported speaking to the listener.
    pub last_spoken_to_ago_ms: Option<u64>,
    /// Unanswered check-ins in a row: each check-in that comes with no listener action or words
    /// since the previous check-in adds one, and any listener action or words reset it. The
    /// first check-in after a reset counts 0. Above zero, check-ins come less often.
    pub unanswered_check_ins: u32,
}

/// One prompt for the agent, with the listening context as it is at delivery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DjCue {
    pub seq: u64,
    /// Core epoch of the running daemon so background waiters can scope `(coreEpoch, seq)` safely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub core_epoch: Option<CoreEpoch>,
    pub kind: DjCueKind,
    pub reasons: Vec<DjCueReason>,
    pub age_ms: u64,
    pub prompt: String,
    pub now_playing: Option<DjNowPlaying>,
    pub up_next: Vec<DjTrack>,
    pub more_queued: bool,
    /// Newest first.
    pub recent: Vec<DjRecentTrack>,
    pub listener: DjListener,
    pub chattiness: DjChattiness,
    /// The recorded tastes, sent on this client's first prompt and whenever they changed since
    /// its previous one; empty when `preferences_unchanged` or `preferences_unavailable`.
    pub preferences: Vec<DjPreference>,
    /// The tastes are the same as in this client's previous prompt.
    pub preferences_unchanged: bool,
    /// Total recorded tastes on the account when available (more than `preferences.len()` when paged).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferences_total: Option<u32>,
    /// True when the player could not read stored tastes for this prompt; keep earlier known tastes.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub preferences_unavailable: bool,
    /// Moment DJ snapshot computed freshly when this prompt is delivered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moment: Option<MomentView>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DjCueInput {
    /// The listener talked to the agent since its previous call.
    #[serde(default)]
    pub user_spoke: bool,
    /// The agent said something to the listener since its previous call.
    #[serde(default)]
    pub spoke: bool,
    pub chattiness: Option<DjChattiness>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DjCueReply {
    pub cue: Option<DjCue>,
    pub chattiness: DjChattiness,
}
