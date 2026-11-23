use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ytm_protocol::{ArtistId, VideoId};

/// What a feedback entry is about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum DjFeedbackTarget {
    Video { video_id: VideoId },
    Artist { artist_id: ArtistId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum DjPreferenceStance {
    Prefer,
    Avoid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "action",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum DjFeedbackChange {
    /// Kept for the account until cleared; replaces an earlier entry for the same subject.
    Record {
        target: DjFeedbackTarget,
        stance: DjPreferenceStance,
        #[schemars(length(max = 200))]
        note: Option<String>,
    },
    Clear {
        #[schemars(length(min = 1, max = 64))]
        preference_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum DjPreferenceSubject {
    Video {
        video_id: VideoId,
        title: Option<String>,
    },
    Artist {
        artist_id: ArtistId,
        name: Option<String>,
    },
}

/// One recorded DJ preference of the current account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DjPreference {
    pub preference_id: String,
    pub subject: DjPreferenceSubject,
    pub stance: DjPreferenceStance,
    pub note: Option<String>,
    /// The registered client that recorded it; not a claim about a human.
    pub recorded_by: String,
    pub recorded_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DjPreferencesInput {
    pub page: Option<crate::PageInput>,
}
