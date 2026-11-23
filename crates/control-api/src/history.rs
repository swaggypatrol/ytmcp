use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ytm_protocol::PlaybackRef;

use crate::{ControlError, HistoryEntry, Page, PageInput, Track};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum HistorySource {
    Local,
    Youtube,
    Combined,
}

/// The local recording timestamp is known. YouTube history may only have a period label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LocalHistoryRange {
    pub from_ms: Option<u64>,
    pub until_ms: Option<u64>,
}

impl LocalHistoryRange {
    pub fn contains(&self, timestamp_ms: u64) -> bool {
        self.from_ms.is_none_or(|from| from <= timestamp_ms)
            && self.until_ms.is_none_or(|until| timestamp_ms < until)
    }

    pub fn is_ordered(self) -> bool {
        self.from_ms
            .zip(self.until_ms)
            .is_none_or(|(from, until)| from < until)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HistoryInput {
    pub source: Option<HistorySource>,
    pub page: Option<PageInput>,
    #[schemars(range(min = 1, max = 100))]
    pub limit: Option<u16>,
    pub local_cursor: Option<String>,
    pub youtube_cursor: Option<String>,
    pub range: Option<LocalHistoryRange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LocalHistoryEntry {
    pub entry_id: String,
    pub track: Track,
    pub playback_ref: Option<PlaybackRef>,
    pub position_ms: u64,
    pub duration_ms: Option<u64>,
    pub recorded_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LocalHistoryView {
    pub revision: String,
    pub retention_limit: u32,
    pub entries: Page<LocalHistoryEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CombinedHistory {
    pub local: LocalHistoryView,
    pub youtube: Option<Page<HistoryEntry>>,
    pub youtube_error: Option<ControlError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum LocalHistoryEdit {
    Remove {
        #[schemars(length(min = 1, max = 50))]
        entry_ids: Vec<String>,
    },
    Clear {
        range: Option<LocalHistoryRange>,
    },
}
