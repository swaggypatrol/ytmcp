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
    /// The listener just did something by hand that says where the music should go (see the
    /// reasons). It is a chance to adjust what plays next, not a prompt to speak.
    ListenerSignal,
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
    /// The listener skipped two or more tracks in a row.
    SkipStreak,
    /// The listener started something particular by hand: a track, a collection, a queue entry.
    ManualPick,
    /// The listener went back to hear a track again, or turned on repeat-one.
    Replay,
    /// The listener chose another Up Next filter.
    FilterChanged,
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
    /// What the listener did by hand since this client's previous prompt, newest first, at most
    /// [`crate::MAX_LISTENER_ACTIONS`]. Facts for the agent to read; never a request.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_actions: Vec<crate::ListenerAction>,
    /// Older actions since the previous prompt that are not listed.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub actions_omitted: u32,
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

/// YouTube Music's own continuation in a prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DjOfficialUpNext {
    pub status: ytm_protocol::OfficialUpcomingStatus,
    pub tracks: Vec<DjTrack>,
    /// More suggestions are loaded than listed.
    pub more: bool,
    /// The selected Up Next filter's label, when one is offered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
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
    /// Whether `up_next` is what follows the current occurrence. When unknown it is empty and
    /// says nothing about what comes next.
    #[serde(default)]
    pub upcoming: crate::UpcomingStatus,
    pub up_next: Vec<DjTrack>,
    pub more_queued: bool,
    /// What YouTube Music plays on its own after the queued tracks: status and up to 5
    /// suggestions. Absent when the page state was not readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub official_up_next: Option<DjOfficialUpNext>,
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
    /// The last prompt this client actually received. A prompt handed out after it (for
    /// example to a wait the client cancelled before the reply arrived) is offered again, so a
    /// cancelled wait loses nothing; the client skips repeats by `(coreEpoch, seq)`. Ignored
    /// when it names another core epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_received: Option<DjCueAck>,
}

/// Names one delivered prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DjCueAck {
    pub core_epoch: CoreEpoch,
    pub seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DjCueReply {
    pub cue: Option<DjCue>,
    pub chattiness: DjChattiness,
}
