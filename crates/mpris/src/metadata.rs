use mpris_server::{LoopStatus, Metadata, PlaybackStatus, Property, Time, TrackId, Volume};
use std::path::{Path, PathBuf};
use url::Url;
use ytm_protocol::{
    AppSnapshot, AssetId, CapabilitySet, CapabilitySupport, PlaybackStatus as CorePlaybackStatus,
    PlayerConnectionState, PlayerObservedState, RepeatMode, StateDelta, Track, TrackInstanceId,
};

pub const MPRIS_BUS_PREFIX: &str = "org.mpris.MediaPlayer2.";
pub const DEFAULT_MPRIS_BUS_NAME: &str = "org.mpris.MediaPlayer2.ytmcp";
pub const DEFAULT_MPRIS_IDENTITY: &str = "YTMcp";
pub const DEFAULT_DESKTOP_ENTRY: &str = "local.ytmcp.Desktop";
pub const TRACK_PATH_PREFIX: &str = "/local/ytmcp/Desktop/track/";

/// Resolves the canonical covers cache root respecting `YTMCP_DATA_DIR`,
/// `XDG_CACHE_HOME`, and `$HOME/.cache/ytmcp/covers`.
pub fn resolve_covers_cache_root() -> Option<PathBuf> {
    if let Ok(custom) = std::env::var("YTMCP_DATA_DIR") {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed).join("cache/ytmcp/covers"));
        }
    }
    if let Ok(xdg_cache) = std::env::var("XDG_CACHE_HOME") {
        let trimmed = xdg_cache.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed).join("ytmcp/covers"));
        }
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache/ytmcp/covers"))
}

/// Validates that `candidate_file` exists inside canonical `cache_root` before converting to `file://` URI.
/// Prevents arbitrary local paths or symlink escapes from leaking into `mpris:artUrl`.
pub fn sanitize_contained_cover_file_url(
    candidate_file: &Path,
    cache_root: &Path,
) -> Option<String> {
    let canon_root = std::fs::canonicalize(cache_root).ok()?;
    let canon_file = std::fs::canonicalize(candidate_file).ok()?;
    if !canon_file.starts_with(&canon_root) || !canon_file.is_file() {
        return None;
    }
    Url::from_file_path(&canon_file).ok().map(|u| u.to_string())
}

/// Strips the `"org.mpris.MediaPlayer2."` prefix because `mpris_server::Server::new`
/// prepends it automatically.
pub fn bus_name_suffix_from_full(full_bus_name: &str) -> &str {
    full_bus_name
        .strip_prefix(MPRIS_BUS_PREFIX)
        .unwrap_or(full_bus_name)
}

/// Sanitizes an arbitrary identifier into a valid D-Bus object path segment `[A-Za-z0-9_]+`.
/// Uses bijective `_xx` hex escaping so distinct track instances never collide.
pub fn sanitize_dbus_segment(raw: &str) -> String {
    if raw.is_empty() {
        return "unknown".to_string();
    }
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' => out.push(byte as char),
            _ => {
                use std::fmt::Write as _;
                let _ = write!(out, "_{byte:02x}");
            }
        }
    }
    out
}

/// Derives a valid D-Bus `TrackId` from `TrackInstanceId` (or falls back to `TrackId::NO_TRACK`).
pub fn derive_track_id(
    track_instance_id: Option<&TrackInstanceId>,
    current_track: Option<&Track>,
) -> TrackId {
    if let Some(tid) = track_instance_id {
        let path = format!("{TRACK_PATH_PREFIX}{}", sanitize_dbus_segment(tid.as_str()));
        return TrackId::try_from(path).unwrap_or(TrackId::NO_TRACK);
    }
    if let Some(track) = current_track {
        let path = format!(
            "{TRACK_PATH_PREFIX}vid_{}",
            sanitize_dbus_segment(track.video_id.as_str())
        );
        return TrackId::try_from(path).unwrap_or(TrackId::NO_TRACK);
    }
    TrackId::NO_TRACK
}

/// Converts internal milliseconds (`u64`) to MPRIS microseconds (`Time`).
pub fn ms_to_mpris_time(ms: u64) -> Time {
    let max_ms = u64::try_from(i64::MAX / 1_000).unwrap_or(u64::MAX);
    let clamped = ms.min(max_ms);
    let micros = i64::try_from(clamped).unwrap_or(i64::MAX) * 1_000;
    Time::from_micros(micros)
}

/// Converts MPRIS microseconds (`Time`) to non-negative internal milliseconds (`Option<u64>`).
pub fn mpris_time_to_ms(time: Time) -> Option<u64> {
    if time.is_negative() {
        return None;
    }
    u64::try_from(time.as_micros() / 1_000).ok()
}

pub fn map_playback_status(player: &PlayerObservedState) -> PlaybackStatus {
    if player.stopped_confirmed {
        return PlaybackStatus::Stopped;
    }
    match player.playback {
        CorePlaybackStatus::Playing | CorePlaybackStatus::Buffering => PlaybackStatus::Playing,
        CorePlaybackStatus::Paused => PlaybackStatus::Paused,
        CorePlaybackStatus::Idle | CorePlaybackStatus::Ended | CorePlaybackStatus::Error => {
            PlaybackStatus::Stopped
        }
    }
}

pub fn map_repeat_to_loop_status(repeat: Option<RepeatMode>) -> LoopStatus {
    match repeat {
        Some(RepeatMode::One) => LoopStatus::Track,
        Some(RepeatMode::All) => LoopStatus::Playlist,
        Some(RepeatMode::Off) | None => LoopStatus::None,
    }
}

pub fn map_loop_status_to_repeat(loop_status: LoopStatus) -> RepeatMode {
    match loop_status {
        LoopStatus::None => RepeatMode::Off,
        LoopStatus::Track => RepeatMode::One,
        LoopStatus::Playlist => RepeatMode::All,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MprisSnapshotView {
    pub playback_status: PlaybackStatus,
    pub loop_status: LoopStatus,
    pub shuffle: bool,
    pub volume: Volume,
    pub metadata: Metadata,
    pub can_play: bool,
    pub can_pause: bool,
    pub can_go_next: bool,
    pub can_go_previous: bool,
    pub can_seek: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct MprisCachedState {
    pub player: PlayerObservedState,
    pub capabilities: CapabilitySet,
    /// Only the now-playing cover is exposed on D-Bus.
    pub local_art: Option<(AssetId, String)>,
}

impl MprisCachedState {
    pub fn from_snapshot(snapshot: &AppSnapshot) -> Self {
        Self {
            player: snapshot.player.clone(),
            capabilities: snapshot.capabilities.clone(),
            local_art: None,
        }
    }

    pub fn supports(&self, capability: CapabilitySupport) -> bool {
        capability == CapabilitySupport::Supported
            && !matches!(
                self.player.connection,
                PlayerConnectionState::Disconnected
                    | PlayerConnectionState::Incompatible
                    | PlayerConnectionState::Recovering
            )
    }

    pub fn estimated_position_ms(&self, clock_now_ms: u64) -> u64 {
        let anchor = &self.player.time_anchor;
        if !anchor.extrapolating {
            return match anchor.duration_ms {
                Some(dur) => anchor.position_ms.min(dur),
                None => anchor.position_ms,
            };
        }
        let elapsed = clock_now_ms.saturating_sub(anchor.anchor_core_monotonic_ms);
        let rate = if anchor.playback_rate.is_finite() && anchor.playback_rate > 0.0 {
            anchor.playback_rate
        } else {
            1.0
        };
        let advanced = (elapsed as f64) * rate;
        let delta = if advanced.is_finite() && advanced > 0.0 {
            advanced.round().min(u64::MAX as f64) as u64
        } else {
            0
        };
        let pos = anchor.position_ms.saturating_add(delta);
        match anchor.duration_ms {
            Some(dur) => pos.min(dur),
            None => pos,
        }
    }

    pub fn update_player(&mut self, player: PlayerObservedState) -> Option<AssetId> {
        self.player = player;
        if self.cover_asset_id() == self.local_art.as_ref().map(|(id, _)| id) {
            return None;
        }
        self.local_art = None;
        self.cover_asset_id().cloned()
    }

    pub fn apply_delta(&mut self, delta: StateDelta) -> Option<AssetId> {
        match delta {
            StateDelta::PlayerChanged { player, .. } => return self.update_player(player),
            StateDelta::CapabilitiesChanged { capabilities, .. } => {
                self.capabilities = capabilities
            }
            StateDelta::ResourceInvalidated { resource_kind, .. }
                if matches!(resource_kind.as_str(), "catalog" | "cover") =>
            {
                self.local_art = None
            }
            _ => {}
        }
        None
    }

    /// Play needs a loaded track: the official page ignores play with nothing loaded, and the
    /// core refuses it, so media controls should not offer it.
    pub fn can_play(&self) -> bool {
        self.supports(self.capabilities.play_pause) && self.player.current_track.is_some()
    }

    pub fn cover_asset_id(&self) -> Option<&AssetId> {
        self.player
            .current_track
            .as_ref()?
            .cover
            .as_ref()?
            .asset_id
            .as_ref()
    }

    pub fn build_metadata(&self) -> Metadata {
        let Some(track) = self.player.current_track.as_ref() else {
            return Metadata::builder().trackid(TrackId::NO_TRACK).build();
        };

        let track_id = derive_track_id(self.player.track_instance_id.as_ref(), Some(track));
        let mut builder = Metadata::builder().trackid(track_id).title(&track.title);

        if !track.artists.is_empty() {
            let names: Vec<&str> = track.artists.iter().map(|a| a.name.as_str()).collect();
            builder = builder.artist(names);
        }
        if let Some(album) = &track.album {
            builder = builder.album(&album.title);
        }
        if let Some(dur_ms) = self.player.time_anchor.duration_ms.or(track.duration_ms) {
            builder = builder.length(ms_to_mpris_time(dur_ms));
        }
        if let Some((asset_id, uri)) = &self.local_art
            && self.cover_asset_id() == Some(asset_id)
        {
            builder = builder.art_url(uri);
        }
        builder.build()
    }

    pub fn to_view(&self) -> MprisSnapshotView {
        MprisSnapshotView {
            playback_status: map_playback_status(&self.player),
            loop_status: map_repeat_to_loop_status(self.player.repeat),
            shuffle: self.player.shuffle.unwrap_or(false),
            volume: self.player.volume.clamp(0.0, 1.0),
            metadata: self.build_metadata(),
            can_play: self.can_play(),
            can_pause: self.supports(self.capabilities.play_pause),
            can_go_next: self.supports(self.capabilities.next),
            can_go_previous: self.supports(self.capabilities.previous),
            can_seek: self.supports(self.capabilities.seek),
        }
    }
}

pub(crate) fn diff_views(prev: &MprisSnapshotView, next: &MprisSnapshotView) -> Vec<Property> {
    let mut changed = Vec::new();
    if prev.playback_status != next.playback_status {
        changed.push(Property::PlaybackStatus(next.playback_status));
    }
    if prev.loop_status != next.loop_status {
        changed.push(Property::LoopStatus(next.loop_status));
    }
    if prev.shuffle != next.shuffle {
        changed.push(Property::Shuffle(next.shuffle));
    }
    if (prev.volume - next.volume).abs() > 0.001 {
        changed.push(Property::Volume(next.volume));
    }
    if prev.metadata != next.metadata {
        changed.push(Property::Metadata(next.metadata.clone()));
    }
    if prev.can_play != next.can_play {
        changed.push(Property::CanPlay(next.can_play));
    }
    if prev.can_pause != next.can_pause {
        changed.push(Property::CanPause(next.can_pause));
    }
    if prev.can_go_next != next.can_go_next {
        changed.push(Property::CanGoNext(next.can_go_next));
    }
    if prev.can_go_previous != next.can_go_previous {
        changed.push(Property::CanGoPrevious(next.can_go_previous));
    }
    if prev.can_seek != next.can_seek {
        changed.push(Property::CanSeek(next.can_seek));
    }
    changed
}
