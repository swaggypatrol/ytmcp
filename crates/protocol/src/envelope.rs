use crate::domain::{
    AgentActivityRequest, AgentActivitySnapshot, AppSettings, AppSettingsPatch, AppSnapshot,
    AssetResolvedPayload, BridgeCommandAckPayload, BridgeHealthPayload, BridgeLifecyclePayload,
    BridgePlaybackObservedPayload, BridgeReadyPayload, CatalogQueryRequest, CatalogResponsePayload,
    CommandReceipt, CoreReadyPayload, EnvironmentConfigPayload, HostEnvironmentSnapshotPayload,
    HostHelloPayload, HostWindowVisibilityPayload, LyricsQueryRequest, LyricsResult,
    MomentDesktopRequest, MomentDesktopSnapshot, MutationRequest, MutationResult, NetworkState,
    OpenPlayerWindowPayload, OutputGateControlPayload, OutputGateResultPayload,
    PlayerCommandPayload, QueryError, QueryErrorCode, QueueSnapshot, RecentSearchesRequest,
    RecentSearchesSnapshot, ResolveAssetPayload, SessionMaterialPayload, StateDelta,
};
use crate::ids::{AccountEpoch, BridgeEpoch, CoreEpoch, RequestId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_FRAME_BYTES: usize = 1_048_576; // 1 MiB
pub const MAX_PENDING_REQUESTS: usize = 128;
pub const MAX_COMMAND_QUEUE: usize = 64;
pub const MAX_PAGE_SIZE: u32 = 100;
pub const MAX_SAFE_JS_INT: i64 = 9_007_199_254_740_991; // 2^53 - 1

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum MessageKind {
    // Main UI -> Host/Core
    #[serde(rename = "ui.subscribeSnapshot")]
    UiSubscribeSnapshot,
    #[serde(rename = "ui.unsubscribe")]
    UiUnsubscribe,
    #[serde(rename = "ui.playerCommand")]
    UiPlayerCommand,
    #[serde(rename = "ui.catalogQuery")]
    UiCatalogQuery,
    #[serde(rename = "ui.lyricsQuery")]
    UiLyricsQuery,
    #[serde(rename = "ui.mutation")]
    UiMutation,
    #[serde(rename = "ui.agentActivity")]
    UiAgentActivity,
    #[serde(rename = "ui.settingsUpdate")]
    UiSettingsUpdate,
    #[serde(rename = "ui.momentSettings")]
    UiMomentSettings,
    #[serde(rename = "ui.recentSearches")]
    UiRecentSearches,
    #[serde(rename = "ui.openPlayerWindow")]
    UiOpenPlayerWindow,
    #[serde(rename = "ui.logout")]
    UiLogout,
    #[serde(rename = "ui.clearCache")]
    UiClearCache,
    #[serde(rename = "ui.ping")]
    UiPing,

    // Core -> Main UI
    #[serde(rename = "core.snapshot")]
    CoreSnapshot,
    #[serde(rename = "core.delta")]
    CoreDelta,
    #[serde(rename = "core.commandReceipt")]
    CoreCommandReceipt,
    #[serde(rename = "core.catalogResponse")]
    CoreCatalogResponse,
    #[serde(rename = "core.lyricsResponse")]
    CoreLyricsResponse,
    #[serde(rename = "core.mutationResponse")]
    CoreMutationResponse,
    #[serde(rename = "core.agentActivity")]
    CoreAgentActivity,
    #[serde(rename = "core.settingsUpdated")]
    CoreSettingsUpdated,
    #[serde(rename = "core.momentSettings")]
    CoreMomentSettings,
    #[serde(rename = "core.recentSearches")]
    CoreRecentSearches,
    #[serde(rename = "core.error")]
    CoreError,
    #[serde(rename = "core.pong")]
    CorePong,

    // Host <-> Core
    #[serde(rename = "host.hello")]
    HostHello,
    #[serde(rename = "core.ready")]
    CoreReady,
    #[serde(rename = "host.windowVisibility")]
    HostWindowVisibility,
    #[serde(rename = "host.powerResume")]
    HostPowerResume,
    #[serde(rename = "host.networkChanged")]
    HostNetworkChanged,
    #[serde(rename = "host.sessionMaterial")]
    HostSessionMaterial,
    #[serde(rename = "host.sessionCleared")]
    HostSessionCleared,
    #[serde(rename = "host.bridgeLifecycle")]
    HostBridgeLifecycle,
    #[serde(rename = "core.outputGateControl")]
    CoreOutputGateControl,
    #[serde(rename = "host.outputGateResult")]
    HostOutputGateResult,
    #[serde(rename = "core.resolveAsset")]
    CoreResolveAsset,
    #[serde(rename = "core.assetResolved")]
    CoreAssetResolved,
    #[serde(rename = "core.environmentConfig")]
    CoreEnvironmentConfig,
    #[serde(rename = "host.environmentSnapshot")]
    HostEnvironmentSnapshot,
    #[serde(rename = "host.shutdown")]
    HostShutdown,

    // Bridge <-> Host/Core
    #[serde(rename = "bridge.ready")]
    BridgeReady,
    #[serde(rename = "bridge.playbackObserved")]
    BridgePlaybackObserved,
    #[serde(rename = "bridge.queueObserved")]
    BridgeQueueObserved,
    #[serde(rename = "bridge.health")]
    BridgeHealth,
    #[serde(rename = "bridge.commandAck")]
    BridgeCommandAck,
    #[serde(rename = "bridge.error")]
    BridgeError,
    #[serde(rename = "player.command")]
    PlayerCommand,
    #[serde(rename = "player.snapshotRequest")]
    PlayerSnapshotRequest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SenderRole {
    MainUi,
    PlayerBridge,
    Host,
    Core,
}

impl MessageKind {
    pub fn is_allowed_from(self, role: SenderRole) -> bool {
        match role {
            SenderRole::MainUi => matches!(
                self,
                Self::UiSubscribeSnapshot
                    | Self::UiUnsubscribe
                    | Self::UiPlayerCommand
                    | Self::UiCatalogQuery
                    | Self::UiLyricsQuery
                    | Self::UiMutation
                    | Self::UiAgentActivity
                    | Self::UiSettingsUpdate
                    | Self::UiMomentSettings
                    | Self::UiRecentSearches
                    | Self::UiOpenPlayerWindow
                    | Self::UiLogout
                    | Self::UiClearCache
                    | Self::UiPing
            ),
            SenderRole::PlayerBridge => matches!(
                self,
                Self::BridgeReady
                    | Self::BridgePlaybackObserved
                    | Self::BridgeQueueObserved
                    | Self::BridgeHealth
                    | Self::BridgeCommandAck
                    | Self::BridgeError
            ),
            SenderRole::Host => matches!(
                self,
                Self::HostHello
                    | Self::HostWindowVisibility
                    | Self::HostPowerResume
                    | Self::HostNetworkChanged
                    | Self::HostSessionMaterial
                    | Self::HostSessionCleared
                    | Self::HostBridgeLifecycle
                    | Self::HostOutputGateResult
                    | Self::HostEnvironmentSnapshot
                    | Self::CoreResolveAsset
                    | Self::HostShutdown
            ),
            SenderRole::Core => matches!(
                self,
                Self::CoreReady
                    | Self::CoreSnapshot
                    | Self::CoreDelta
                    | Self::CoreCommandReceipt
                    | Self::CoreCatalogResponse
                    | Self::CoreLyricsResponse
                    | Self::CoreMutationResponse
                    | Self::CoreAgentActivity
                    | Self::CoreSettingsUpdated
                    | Self::CoreMomentSettings
                    | Self::CoreRecentSearches
                    | Self::CoreEnvironmentConfig
                    | Self::CoreError
                    | Self::CorePong
                    | Self::CoreOutputGateControl
                    | Self::CoreAssetResolved
                    | Self::PlayerCommand
                    | Self::PlayerSnapshotRequest
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Envelope {
    pub protocol_version: u32,
    pub kind: MessageKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub core_epoch: Option<CoreEpoch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_epoch: Option<AccountEpoch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge_epoch: Option<BridgeEpoch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<RequestId>,
    pub seq: u64,
    pub payload: serde_json::Value,
}

#[derive(Debug, Error)]
pub enum CodecError {
    #[error("frame exceeds maximum size of {MAX_FRAME_BYTES} bytes (got {0})")]
    OversizedFrame(usize),
    #[error("frame contains unescaped raw newline")]
    RawNewline,
    #[error("invalid UTF-8 frame: {0}")]
    InvalidUtf8(#[from] std::str::Utf8Error),
    #[error("invalid JSON frame: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("unsupported protocolVersion {0}; expected {PROTOCOL_VERSION}")]
    UnsupportedProtocolVersion(u32),
    #[error("numeric value out of safe JS integer or finite range: {0}")]
    InvalidNumber(String),
    #[error("invalid payload for {kind:?}: {reason}")]
    InvalidPayload { kind: MessageKind, reason: String },
}

fn validate_json_numbers(val: &serde_json::Value) -> Result<(), CodecError> {
    match val {
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                if !(-MAX_SAFE_JS_INT..=MAX_SAFE_JS_INT).contains(&i) {
                    return Err(CodecError::InvalidNumber(format!(
                        "integer {i} exceeds safe JS range"
                    )));
                }
            } else if let Some(u) = n.as_u64() {
                if u > MAX_SAFE_JS_INT as u64 {
                    return Err(CodecError::InvalidNumber(format!(
                        "unsigned integer {u} exceeds safe JS range"
                    )));
                }
            } else if let Some(f) = n.as_f64() {
                if !f.is_finite() {
                    return Err(CodecError::InvalidNumber(format!("non-finite float {f}")));
                }
            } else {
                return Err(CodecError::InvalidNumber(n.to_string()));
            }
            Ok(())
        }
        serde_json::Value::Array(items) => {
            for item in items {
                validate_json_numbers(item)?;
            }
            Ok(())
        }
        serde_json::Value::Object(map) => {
            for v in map.values() {
                validate_json_numbers(v)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Validates envelope bytes BEFORE JSON parsing, checks protocolVersion, numeric bounds,
/// and validates the typed payload matching `envelope.kind`.
pub fn decode_ndjson_bytes(line: &[u8]) -> Result<Envelope, CodecError> {
    let trimmed = line.strip_suffix(b"\n").unwrap_or(line);
    let trimmed = trimmed.strip_suffix(b"\r").unwrap_or(trimmed);

    if trimmed.len() > MAX_FRAME_BYTES {
        return Err(CodecError::OversizedFrame(trimmed.len()));
    }
    if trimmed.contains(&b'\n') || trimmed.contains(&b'\r') {
        return Err(CodecError::RawNewline);
    }

    let text = std::str::from_utf8(trimmed)?;
    let raw_val: serde_json::Value = serde_json::from_str(text)?;
    validate_json_numbers(&raw_val)?;

    if let Some(pv) = raw_val.get("protocolVersion").and_then(|v| v.as_u64())
        && pv != u64::from(PROTOCOL_VERSION)
    {
        return Err(CodecError::UnsupportedProtocolVersion(
            u32::try_from(pv).unwrap_or(u32::MAX),
        ));
    }

    let env: Envelope = serde_json::from_value(raw_val)?;
    if env.protocol_version != PROTOCOL_VERSION {
        return Err(CodecError::UnsupportedProtocolVersion(env.protocol_version));
    }
    if env.seq > MAX_SAFE_JS_INT as u64 {
        return Err(CodecError::InvalidNumber(format!(
            "seq {} exceeds safe JS range",
            env.seq
        )));
    }
    validate_typed_payload(&env)?;
    Ok(env)
}

pub fn decode_ndjson_str(line: &str) -> Result<Envelope, CodecError> {
    decode_ndjson_bytes(line.as_bytes())
}

pub fn encode_ndjson_line(env: &Envelope) -> Result<String, CodecError> {
    if env.protocol_version != PROTOCOL_VERSION {
        return Err(CodecError::UnsupportedProtocolVersion(env.protocol_version));
    }
    let val = serde_json::to_value(env)?;
    validate_json_numbers(&val)?;
    let mut line = serde_json::to_string(&val)?;
    if line.len() > MAX_FRAME_BYTES {
        return Err(CodecError::OversizedFrame(line.len()));
    }
    line.push('\n');
    Ok(line)
}

pub fn validate_typed_payload(env: &Envelope) -> Result<(), CodecError> {
    let kind = env.kind;
    let map_err = |e: serde_json::Error| CodecError::InvalidPayload {
        kind,
        reason: e.to_string(),
    };

    match kind {
        MessageKind::UiSubscribeSnapshot
        | MessageKind::UiUnsubscribe
        | MessageKind::UiLogout
        | MessageKind::UiClearCache
        | MessageKind::UiPing
        | MessageKind::CorePong
        | MessageKind::HostPowerResume
        | MessageKind::HostSessionCleared
        | MessageKind::HostShutdown
        | MessageKind::PlayerSnapshotRequest => {
            if !env.payload.is_object() && !env.payload.is_null() {
                return Err(CodecError::InvalidPayload {
                    kind,
                    reason: "expected object or null payload".to_string(),
                });
            }
        }
        MessageKind::UiPlayerCommand | MessageKind::PlayerCommand => {
            let cmd: PlayerCommandPayload =
                serde_json::from_value(env.payload.clone()).map_err(map_err)?;
            if let Some(args) = &cmd.args
                && let Some(vol) = args.volume
                && !(0.0..=1.0).contains(&vol)
            {
                return Err(CodecError::InvalidPayload {
                    kind,
                    reason: format!("volume {vol} outside [0.0, 1.0]"),
                });
            }
        }
        MessageKind::UiCatalogQuery => {
            let req: CatalogQueryRequest =
                serde_json::from_value(env.payload.clone()).map_err(map_err)?;
            if let Some(limit) = req.limit
                && (limit == 0 || limit > MAX_PAGE_SIZE)
            {
                return Err(CodecError::InvalidPayload {
                    kind,
                    reason: format!("limit {limit} outside [1, {MAX_PAGE_SIZE}]"),
                });
            }
        }
        MessageKind::UiLyricsQuery => {
            let _: LyricsQueryRequest =
                serde_json::from_value(env.payload.clone()).map_err(map_err)?;
        }
        MessageKind::UiMutation => {
            let _: MutationRequest =
                serde_json::from_value(env.payload.clone()).map_err(map_err)?;
        }
        MessageKind::UiAgentActivity => {
            let _: AgentActivityRequest =
                serde_json::from_value(env.payload.clone()).map_err(map_err)?;
        }
        MessageKind::UiSettingsUpdate => {
            let patch: AppSettingsPatch =
                serde_json::from_value(env.payload.clone()).map_err(map_err)?;
            if let Some(scale) = patch.ui_scale_percent
                && !(75..=150).contains(&scale)
            {
                return Err(CodecError::InvalidPayload {
                    kind,
                    reason: format!("uiScalePercent {scale} outside [75, 150]"),
                });
            }
        }
        MessageKind::CoreSettingsUpdated => {
            let _: AppSettings = serde_json::from_value(env.payload.clone()).map_err(map_err)?;
        }
        MessageKind::UiMomentSettings => {
            let req: MomentDesktopRequest =
                serde_json::from_value(env.payload.clone()).map_err(map_err)?;
            if let MomentDesktopRequest::Update { patch } = &req
                && let Some(city) = &patch.scene_city
                && city.chars().count() > 80
            {
                return Err(CodecError::InvalidPayload {
                    kind,
                    reason: "sceneCity exceeds 80 characters".to_string(),
                });
            }
        }
        MessageKind::CoreMomentSettings => {
            let _: MomentDesktopSnapshot =
                serde_json::from_value(env.payload.clone()).map_err(map_err)?;
        }
        MessageKind::UiRecentSearches => {
            let req: RecentSearchesRequest =
                serde_json::from_value(env.payload.clone()).map_err(map_err)?;
            req.validate()
                .map_err(|reason| CodecError::InvalidPayload { kind, reason })?;
        }
        MessageKind::CoreRecentSearches => {
            let _: RecentSearchesSnapshot =
                serde_json::from_value(env.payload.clone()).map_err(map_err)?;
        }
        MessageKind::CoreEnvironmentConfig => {
            let _: EnvironmentConfigPayload =
                serde_json::from_value(env.payload.clone()).map_err(map_err)?;
        }
        MessageKind::HostEnvironmentSnapshot => {
            let _: HostEnvironmentSnapshotPayload =
                serde_json::from_value(env.payload.clone()).map_err(map_err)?;
        }
        MessageKind::UiOpenPlayerWindow => {
            let _: OpenPlayerWindowPayload =
                serde_json::from_value(env.payload.clone()).map_err(map_err)?;
        }
        MessageKind::CoreSnapshot => {
            let _: AppSnapshot = serde_json::from_value(env.payload.clone()).map_err(map_err)?;
        }
        MessageKind::CoreDelta => {
            let _: StateDelta = serde_json::from_value(env.payload.clone()).map_err(map_err)?;
        }
        MessageKind::CoreCommandReceipt => {
            let _: CommandReceipt = serde_json::from_value(env.payload.clone()).map_err(map_err)?;
        }
        MessageKind::CoreCatalogResponse => {
            let _: CatalogResponsePayload =
                serde_json::from_value(env.payload.clone()).map_err(map_err)?;
        }
        MessageKind::CoreLyricsResponse => {
            let _: LyricsResult = serde_json::from_value(env.payload.clone()).map_err(map_err)?;
        }
        MessageKind::CoreMutationResponse => {
            let _: MutationResult = serde_json::from_value(env.payload.clone()).map_err(map_err)?;
        }
        MessageKind::CoreAgentActivity => {
            let _: AgentActivitySnapshot =
                serde_json::from_value(env.payload.clone()).map_err(map_err)?;
        }
        MessageKind::CoreError | MessageKind::BridgeError => {
            let _: QueryError = serde_json::from_value(env.payload.clone()).map_err(map_err)?;
        }
        MessageKind::HostHello => {
            let _: HostHelloPayload =
                serde_json::from_value(env.payload.clone()).map_err(map_err)?;
        }
        MessageKind::CoreReady => {
            let _: CoreReadyPayload =
                serde_json::from_value(env.payload.clone()).map_err(map_err)?;
        }
        MessageKind::HostWindowVisibility => {
            let _: HostWindowVisibilityPayload =
                serde_json::from_value(env.payload.clone()).map_err(map_err)?;
        }
        MessageKind::HostNetworkChanged => {
            let _: NetworkState = serde_json::from_value(env.payload.clone()).map_err(map_err)?;
        }
        MessageKind::HostSessionMaterial => {
            let _: SessionMaterialPayload =
                serde_json::from_value(env.payload.clone()).map_err(map_err)?;
        }
        MessageKind::HostBridgeLifecycle => {
            let _: BridgeLifecyclePayload =
                serde_json::from_value(env.payload.clone()).map_err(map_err)?;
        }
        MessageKind::CoreOutputGateControl => {
            let _: OutputGateControlPayload =
                serde_json::from_value(env.payload.clone()).map_err(map_err)?;
        }
        MessageKind::HostOutputGateResult => {
            let _: OutputGateResultPayload =
                serde_json::from_value(env.payload.clone()).map_err(map_err)?;
        }
        MessageKind::CoreResolveAsset => {
            let _: ResolveAssetPayload =
                serde_json::from_value(env.payload.clone()).map_err(map_err)?;
        }
        MessageKind::CoreAssetResolved => {
            let _: AssetResolvedPayload =
                serde_json::from_value(env.payload.clone()).map_err(map_err)?;
        }
        MessageKind::BridgeReady => {
            let _: BridgeReadyPayload =
                serde_json::from_value(env.payload.clone()).map_err(map_err)?;
        }
        MessageKind::BridgePlaybackObserved => {
            let obs: BridgePlaybackObservedPayload =
                serde_json::from_value(env.payload.clone()).map_err(map_err)?;
            if !(0.0..=1.0).contains(&obs.volume) {
                return Err(CodecError::InvalidPayload {
                    kind,
                    reason: format!("volume {} outside [0.0, 1.0]", obs.volume),
                });
            }
            if obs.playback_rate <= 0.0 || obs.playback_rate > 16.0 {
                return Err(CodecError::InvalidPayload {
                    kind,
                    reason: format!("playbackRate {} out of range", obs.playback_rate),
                });
            }
        }
        MessageKind::BridgeQueueObserved => {
            let _: QueueSnapshot = serde_json::from_value(env.payload.clone()).map_err(map_err)?;
        }
        MessageKind::BridgeHealth => {
            let _: BridgeHealthPayload =
                serde_json::from_value(env.payload.clone()).map_err(map_err)?;
        }
        MessageKind::BridgeCommandAck => {
            let _: BridgeCommandAckPayload =
                serde_json::from_value(env.payload.clone()).map_err(map_err)?;
        }
    }
    Ok(())
}

impl Envelope {
    pub fn new(
        kind: MessageKind,
        seq: u64,
        payload: impl Serialize,
    ) -> Result<Self, serde_json::Error> {
        Ok(Self {
            protocol_version: PROTOCOL_VERSION,
            kind,
            core_epoch: None,
            account_epoch: None,
            bridge_epoch: None,
            request_id: None,
            seq,
            payload: serde_json::to_value(payload)?,
        })
    }

    pub fn decode_payload<T: for<'de> Deserialize<'de>>(&self) -> Result<T, CodecError> {
        serde_json::from_value(self.payload.clone()).map_err(|e| CodecError::InvalidPayload {
            kind: self.kind,
            reason: e.to_string(),
        })
    }

    pub fn error_response(
        seq: u64,
        request_id: Option<RequestId>,
        code: QueryErrorCode,
        safe_message: impl Into<String>,
        retriable: bool,
    ) -> Self {
        let err = QueryError {
            code,
            retriable,
            retry_after_ms: None,
            safe_message: safe_message.into(),
            request_id: request_id.clone(),
        };
        Self {
            protocol_version: PROTOCOL_VERSION,
            kind: MessageKind::CoreError,
            core_epoch: None,
            account_epoch: None,
            bridge_epoch: None,
            request_id,
            seq,
            payload: serde_json::to_value(err).unwrap_or(serde_json::Value::Null),
        }
    }
}

/// Root schema bundle exported for JSON Schema -> TypeScript generation.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProtocolSchemaRoot {
    pub envelope: Envelope,
    pub app_snapshot: AppSnapshot,
    pub state_delta: StateDelta,
    pub player_command: PlayerCommandPayload,
    pub command_receipt: CommandReceipt,
    pub catalog_query: CatalogQueryRequest,
    pub catalog_response: CatalogResponsePayload,
    pub lyrics_query: LyricsQueryRequest,
    pub lyrics_result: LyricsResult,
    pub mutation_request: MutationRequest,
    pub mutation_result: MutationResult,
    pub agent_activity_request: AgentActivityRequest,
    pub agent_activity_snapshot: AgentActivitySnapshot,
    pub app_settings: AppSettings,
    pub app_settings_patch: AppSettingsPatch,
    pub moment_desktop_request: MomentDesktopRequest,
    pub moment_desktop_snapshot: MomentDesktopSnapshot,
    pub recent_searches_request: RecentSearchesRequest,
    pub recent_searches_snapshot: RecentSearchesSnapshot,
    pub environment_config: EnvironmentConfigPayload,
    pub host_environment_snapshot: HostEnvironmentSnapshotPayload,
    pub host_hello: HostHelloPayload,
    pub core_ready: CoreReadyPayload,
    pub host_window_visibility: HostWindowVisibilityPayload,
    pub bridge_lifecycle: BridgeLifecyclePayload,
    pub output_gate_control: OutputGateControlPayload,
    pub output_gate_result: OutputGateResultPayload,
    pub resolve_asset: ResolveAssetPayload,
    pub asset_resolved: AssetResolvedPayload,
    pub open_player_window: OpenPlayerWindowPayload,
    pub bridge_ready: BridgeReadyPayload,
    pub bridge_playback_observed: BridgePlaybackObservedPayload,
    pub bridge_queue_observed: QueueSnapshot,
    pub bridge_health: BridgeHealthPayload,
    pub bridge_command_ack: BridgeCommandAckPayload,
    pub session_material: SessionMaterialPayload,
    pub query_error: QueryError,
}
