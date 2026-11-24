use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum MomentSectionStatus {
    Shared,
    NotShared,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum MomentDayPart {
    EarlyMorning,
    Morning,
    Afternoon,
    Evening,
    LateNight,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MomentTime {
    /// Local calendar date (`YYYY-MM-DD`) in `timeZone`.
    pub local_date: String,
    /// Lowercase weekday name (`monday`..`sunday`).
    pub weekday: String,
    pub day_part: MomentDayPart,
    /// IANA time zone identifier from the local device clock.
    pub time_zone: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum MomentOccasionPhase {
    Eve,
    Day,
    Observed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum MomentOccasionKind {
    Public,
    Observance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MomentOccasion {
    pub id: String,
    pub label: String,
    pub phase: MomentOccasionPhase,
    pub kind: MomentOccasionKind,
    pub local_date: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual_date: Option<String>,
    pub provider_version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SceneCitySource {
    UserConfigured,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SceneCity {
    pub label: String,
    pub source: SceneCitySource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentView {
    pub status: MomentSectionStatus,
    pub revision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<MomentTime>,
    /// The instant `time` describes (Unix ms), so an assistant can turn "tonight" or "for two
    /// hours" into an absolute `expiresAtMs`. Present only with `time`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub captured_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub occasions: Vec<MomentOccasion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_city: Option<SceneCity>,
    /// Always false: the player never fetches weather; an assistant may look it up for `sceneCity`.
    #[serde(default)]
    pub weather_provided: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum VocalsMode {
    Any,
    PreferInstrumental,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum EnergyLevel {
    Gentle,
    Steady,
    Driving,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ActivityKind {
    Gaming,
    Focus,
    Relax,
    Social,
}

/// One session-scoped listening intent dimension. Updating a dimension replaces only that dimension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "dimension",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ListeningIntentInstruction {
    Vocals {
        mode: VocalsMode,
    },
    Energy {
        level: EnergyLevel,
    },
    Activity {
        kind: ActivityKind,
    },
    MusicBrief {
        #[schemars(length(min = 1, max = 120))]
        brief: String,
    },
    DismissTheme {
        #[schemars(length(min = 1, max = 96))]
        theme_id: String,
    },
}

impl ListeningIntentInstruction {
    /// Canonical key per dimension (at most one active entry per key).
    pub fn canonical_key(&self) -> String {
        match self {
            Self::Vocals { .. } => "vocals".to_owned(),
            Self::Energy { .. } => "energy".to_owned(),
            Self::Activity { .. } => "activity".to_owned(),
            Self::MusicBrief { .. } => "musicBrief".to_owned(),
            Self::DismissTheme { theme_id } => format!("dismissTheme:{}", theme_id.trim()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ListeningIntentScope {
    Today,
    Session,
    Until,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ListeningIntentBasis {
    OwnerInput,
    UserRequestReportedByAssistant,
    AssistantInference,
}

/// Provenance accepted from external MCP clients (owners set `ownerInput` strictly from desktop UI).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ReportedIntentBasis {
    UserRequest,
    Inference,
}

impl From<ReportedIntentBasis> for ListeningIntentBasis {
    fn from(basis: ReportedIntentBasis) -> Self {
        match basis {
            ReportedIntentBasis::UserRequest => Self::UserRequestReportedByAssistant,
            ReportedIntentBasis::Inference => Self::AssistantInference,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ListeningIntentDuration {
    Today,
    Session,
    Until { expires_at_ms: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "action",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ListeningIntentChange {
    /// `expected_moment_revision` is the Moment revision the change was decided on. An
    /// inference must carry it; a listener's request may omit it.
    Set {
        instruction: ListeningIntentInstruction,
        duration: ListeningIntentDuration,
        basis: ReportedIntentBasis,
        expected_moment_revision: Option<u64>,
    },
    /// Clears the intent stored under `intent_key`. With `intent_id`, only that exact intent:
    /// one that replaced it since it was read stays.
    Clear {
        #[schemars(length(min = 1, max = 128))]
        intent_key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(length(min = 1, max = 64))]
        intent_id: Option<String>,
        basis: ReportedIntentBasis,
        expected_moment_revision: Option<u64>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListeningIntentView {
    pub intent_key: String,
    pub intent_id: String,
    pub instruction: ListeningIntentInstruction,
    pub scope: ListeningIntentScope,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
    pub basis: ListeningIntentBasis,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListeningIntentsView {
    pub status: MomentSectionStatus,
    pub revision: String,
    pub items: Vec<ListeningIntentView>,
}

/// Authoritative Moment DJ context projection delivered to one authorized MCP client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MomentView {
    pub revision: u64,
    pub environment: EnvironmentView,
    pub listening_intents: ListeningIntentsView,
}
