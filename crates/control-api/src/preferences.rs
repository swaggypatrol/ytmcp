use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ytm_protocol::{AppSettings, LyricsKind, LyricsLine, ThemeMode, UiLanguage, VideoId};

use crate::TrackTarget;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LyricsInput {
    pub target: TrackTarget,
    pub page: Option<LyricsPageInput>,
    pub source: Option<LyricsSourcePreference>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LyricsPageInput {
    #[schemars(length(min = 1, max = 2048))]
    pub cursor: Option<String>,
    #[schemars(range(min = 1, max = 200))]
    pub limit: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum LyricsSourcePreference {
    Auto,
    Lrclib,
    YouTubeMusic,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LyricsPage {
    pub video_id: VideoId,
    pub kind: LyricsKind,
    pub source: String,
    pub language: Option<String>,
    pub match_basis: Option<String>,
    pub fetched_at_ms: u64,
    pub file_offset_ms: i64,
    pub user_offset_ms: i64,
    pub lines: Vec<LyricsLine>,
    pub plain_text: Option<String>,
    pub next_cursor: Option<String>,
    pub complete: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "action",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum LyricsOffsetAction {
    Set {
        #[schemars(range(min = -10000, max = 10000))]
        offset_ms: i64,
    },
    Reset,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SettingsPatch {
    pub theme: Option<ThemeMode>,
    pub reduce_motion: Option<bool>,
    pub reduce_transparency: Option<bool>,
    #[schemars(range(min = 75, max = 150))]
    pub ui_scale_percent: Option<u16>,
    pub autoplay_on_launch: Option<bool>,
    pub close_to_tray: Option<bool>,
    pub lrclib_enabled: Option<bool>,
    pub track_notifications: Option<bool>,
    pub ui_language: Option<UiLanguage>,
}

impl SettingsPatch {
    pub fn apply(self, mut base: AppSettings) -> AppSettings {
        base.theme = self.theme.unwrap_or(base.theme);
        base.reduce_motion = self.reduce_motion.unwrap_or(base.reduce_motion);
        base.reduce_transparency = self.reduce_transparency.unwrap_or(base.reduce_transparency);
        base.ui_scale_percent = self.ui_scale_percent.unwrap_or(base.ui_scale_percent);
        base.autoplay_on_launch = self.autoplay_on_launch.unwrap_or(base.autoplay_on_launch);
        base.close_to_tray = self.close_to_tray.unwrap_or(base.close_to_tray);
        base.lrclib_enabled = self.lrclib_enabled.unwrap_or(base.lrclib_enabled);
        base.track_notifications = self.track_notifications.unwrap_or(base.track_notifications);
        base.ui_language = self.ui_language.unwrap_or(base.ui_language);
        base
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SettingsSnapshot {
    pub settings: AppSettings,
    pub settings_revision: u64,
}
