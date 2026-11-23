use mpris_server::zbus::fdo;
use mpris_server::{
    LoopStatus, Metadata, PlaybackRate, PlaybackStatus, PlayerInterface, RootInterface, Server,
    Signal, Time, TrackId, Volume,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, warn};
use ytm_core::CoreEngine;
use ytm_protocol::{
    AssetId, CommandId, CommandLifecycleStatus, PlayerCommandArgs, PlayerCommandPayload, PlayerOp,
    StateDelta,
};

use crate::metadata::{
    DEFAULT_DESKTOP_ENTRY, DEFAULT_MPRIS_BUS_NAME, DEFAULT_MPRIS_IDENTITY, MprisCachedState,
    bus_name_suffix_from_full, derive_track_id, diff_views, map_loop_status_to_repeat,
    map_playback_status, map_repeat_to_loop_status, mpris_time_to_ms, ms_to_mpris_time,
    resolve_covers_cache_root, sanitize_contained_cover_file_url,
};

/// Host-level desktop actions requested over MPRIS `RootInterface`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MprisHostAction {
    Raise,
    Quit,
}

/// Configuration for the single YTM Glass MPRIS 2.2 service.
#[derive(Debug, Clone)]
pub struct MprisConfig {
    pub bus_name: String,
    pub identity: String,
    pub desktop_entry: String,
    pub covers_cache_root: Option<PathBuf>,
}

impl Default for MprisConfig {
    fn default() -> Self {
        Self {
            bus_name: DEFAULT_MPRIS_BUS_NAME.to_string(),
            identity: DEFAULT_MPRIS_IDENTITY.to_string(),
            desktop_entry: DEFAULT_DESKTOP_ENTRY.to_string(),
            covers_cache_root: resolve_covers_cache_root(),
        }
    }
}

/// Production MPRIS 2.2 adapter strictly delegating all playback commands to `CoreEngine`
/// and host window/exit requests to a bounded `mpsc::Sender<MprisHostAction>`.
pub struct MprisService {
    core: Arc<CoreEngine>,
    config: MprisConfig,
    state: Arc<RwLock<MprisCachedState>>,
    host_action_tx: mpsc::Sender<MprisHostAction>,
    cmd_seq: AtomicU64,
}

impl MprisService {
    /// Starts the single `mpris-server` D-Bus service and background delta synchronization task.
    pub async fn start(
        core: Arc<CoreEngine>,
        config: MprisConfig,
        host_action_tx: mpsc::Sender<MprisHostAction>,
    ) -> Result<MprisHandle, mpris_server::zbus::Error> {
        let (snapshot, mut delta_rx) = core
            .subscribe_snapshot()
            .await
            .map_err(|e| mpris_server::zbus::Error::Failure(e.to_string()))?;

        let mut initial_state = MprisCachedState::from_snapshot(&snapshot);
        if let Some(asset_id) = initial_state.cover_asset_id().cloned() {
            initial_state.local_art =
                resolve_local_art(&core, config.covers_cache_root.as_deref(), asset_id).await;
        }
        let state = Arc::new(RwLock::new(initial_state));
        let imp = Self {
            core: Arc::clone(&core),
            config: config.clone(),
            state: Arc::clone(&state),
            host_action_tx,
            cmd_seq: AtomicU64::new(1),
        };

        let suffix = bus_name_suffix_from_full(&config.bus_name);
        let server = Arc::new(Server::new(suffix, imp).await?);
        let full_bus_name = server.bus_name().to_string();

        let sync_server = Arc::clone(&server);
        let sync_core = Arc::clone(&core);
        let sync_state = Arc::clone(&state);
        let covers_root = config.covers_cache_root.clone();
        let mut last_revision = snapshot.revision;

        let sync_task = tokio::spawn(async move {
            let mut last_view = sync_state.read().unwrap_or_else(recover_state).to_view();
            let mut last_seek_receipt_id = None;
            loop {
                let mut emit_seeked_ms = None;
                let asset_to_resolve = match delta_rx.recv().await {
                    Ok(delta) if delta.revision() <= last_revision => continue,
                    Ok(delta) if delta.revision() == last_revision.saturating_add(1) => {
                        last_revision = delta.revision();
                        let mut state = sync_state.write().unwrap_or_else(recover_state);
                        match &delta {
                            StateDelta::PlayerChanged {
                                player, pending, ..
                            } => {
                                let receipt = pending.last_receipt.as_ref().filter(|r| {
                                    r.status == CommandLifecycleStatus::Observed
                                        && matches!(r.op, PlayerOp::Seek | PlayerOp::Stop)
                                        && last_seek_receipt_id.as_ref() != Some(&r.command_id)
                                });
                                let new_pos = player.time_anchor.position_ms;
                                if let Some(r) = receipt {
                                    last_seek_receipt_id = Some(r.command_id.clone());
                                    emit_seeked_ms = Some(new_pos);
                                } else if player.track_instance_id.is_some()
                                    && player.track_instance_id == state.player.track_instance_id
                                    && new_pos.abs_diff(
                                        state.estimated_position_ms(sync_core.clock().now_ms()),
                                    ) > 1_500
                                {
                                    emit_seeked_ms = Some(new_pos);
                                }
                            }
                            StateDelta::CommandUpdated { receipt, .. }
                                if receipt.status == CommandLifecycleStatus::Observed
                                    && matches!(receipt.op, PlayerOp::Seek | PlayerOp::Stop)
                                    && last_seek_receipt_id.as_ref()
                                        != Some(&receipt.command_id) =>
                            {
                                last_seek_receipt_id = Some(receipt.command_id.clone());
                                emit_seeked_ms =
                                    Some(state.estimated_position_ms(sync_core.clock().now_ms()));
                            }
                            _ => {}
                        }
                        state.apply_delta(delta)
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    // A missed revision may have invalidated any previous local URI.
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        debug!("MPRIS delta gap; resyncing snapshot");
                        let Ok(fresh) = sync_core.snapshot().await else {
                            continue;
                        };
                        last_revision = fresh.revision;
                        let mut state = sync_state.write().unwrap_or_else(recover_state);
                        *state = MprisCachedState::from_snapshot(&fresh);
                        state.cover_asset_id().cloned()
                    }
                };
                if let Some(asset_id) = asset_to_resolve {
                    let art = resolve_local_art(&sync_core, covers_root.as_deref(), asset_id).await;
                    sync_state.write().unwrap_or_else(recover_state).local_art = art;
                }
                let next_view = sync_state.read().unwrap_or_else(recover_state).to_view();
                let changed = diff_views(&last_view, &next_view);
                if !changed.is_empty()
                    && let Err(err) = sync_server.properties_changed(changed).await
                {
                    warn!(error = %err, "failed to emit MPRIS PropertiesChanged");
                }
                if let Some(pos_ms) = emit_seeked_ms
                    && let Err(err) = sync_server
                        .emit(Signal::Seeked {
                            position: ms_to_mpris_time(pos_ms),
                        })
                        .await
                {
                    warn!(error = %err, "failed to emit MPRIS Seeked");
                }
                last_view = next_view;
            }
        });

        Ok(MprisHandle {
            bus_name: full_bus_name,
            server,
            sync_task,
        })
    }

    fn new_command_id(&self, prefix: &str) -> fdo::Result<CommandId> {
        let seq = self.cmd_seq.fetch_add(1, Ordering::Relaxed);
        CommandId::new(format!("{prefix}-{seq}")).map_err(|err| fdo::Error::Failed(err.to_string()))
    }

    async fn dispatch_op(
        &self,
        prefix: &str,
        op: PlayerOp,
        args: Option<PlayerCommandArgs>,
    ) -> fdo::Result<()> {
        let cmd = PlayerCommandPayload {
            command_id: self.new_command_id(prefix)?,
            op,
            args,
        };
        if let Err(err) = self.core.submit_player_command(cmd).await {
            debug!(?op, code = ?err.code, "MPRIS command rejected by core: {}", err.safe_message);
        }
        Ok(())
    }

    fn read_state<R>(&self, f: impl FnOnce(&MprisCachedState) -> R) -> fdo::Result<R> {
        let guard = self.state.read().unwrap_or_else(recover_state);
        Ok(f(&guard))
    }

    fn empty_args() -> PlayerCommandArgs {
        PlayerCommandArgs {
            track_instance_id: None,
            position_ms: None,
            volume: None,
            muted: None,
            target: None,
            shuffle: None,
            repeat: None,
            autoplay: None,
            enqueue_position: None,
            queue_entry_id: None,
            before_queue_entry_id: None,
            queue_revision: None,
        }
    }
}

fn recover_state<T>(poisoned: PoisonError<T>) -> T {
    warn!("recovering poisoned MPRIS state lock");
    poisoned.into_inner()
}

async fn resolve_local_art(
    core: &CoreEngine,
    cache_root: Option<&Path>,
    id: AssetId,
) -> Option<(AssetId, String)> {
    let root = cache_root?;
    let path = core.resolve_asset(id.clone()).await.ok()?.file_path?;
    let uri = sanitize_contained_cover_file_url(Path::new(&path), root)?;
    Some((id, uri))
}

/// Handle owning the active `mpris_server::Server` and sync task.
pub struct MprisHandle {
    bus_name: String,
    server: Arc<Server<MprisService>>,
    sync_task: JoinHandle<()>,
}

impl MprisHandle {
    pub fn bus_name(&self) -> &str {
        &self.bus_name
    }

    pub async fn shutdown(self) {
        self.sync_task.abort();
        let _ = self.server.release_bus_name().await;
    }
}

impl RootInterface for MprisService {
    async fn raise(&self) -> fdo::Result<()> {
        let _ = self.host_action_tx.try_send(MprisHostAction::Raise);
        Ok(())
    }

    async fn quit(&self) -> fdo::Result<()> {
        let _ = self.host_action_tx.try_send(MprisHostAction::Quit);
        Ok(())
    }

    async fn can_quit(&self) -> fdo::Result<bool> {
        Ok(true)
    }

    async fn fullscreen(&self) -> fdo::Result<bool> {
        Ok(false)
    }

    async fn set_fullscreen(&self, _fullscreen: bool) -> mpris_server::zbus::Result<()> {
        Ok(())
    }

    async fn can_set_fullscreen(&self) -> fdo::Result<bool> {
        Ok(false)
    }

    async fn can_raise(&self) -> fdo::Result<bool> {
        Ok(true)
    }

    async fn has_track_list(&self) -> fdo::Result<bool> {
        Ok(false)
    }

    async fn identity(&self) -> fdo::Result<String> {
        Ok(self.config.identity.clone())
    }

    async fn desktop_entry(&self) -> fdo::Result<String> {
        Ok(self.config.desktop_entry.clone())
    }

    async fn supported_uri_schemes(&self) -> fdo::Result<Vec<String>> {
        Ok(Vec::new())
    }

    async fn supported_mime_types(&self) -> fdo::Result<Vec<String>> {
        Ok(Vec::new())
    }
}

impl PlayerInterface for MprisService {
    async fn next(&self) -> fdo::Result<()> {
        if !self.can_go_next().await? {
            return Ok(());
        }
        self.dispatch_op("mpris-next", PlayerOp::Next, None).await
    }

    async fn previous(&self) -> fdo::Result<()> {
        if !self.can_go_previous().await? {
            return Ok(());
        }
        self.dispatch_op("mpris-prev", PlayerOp::Previous, None)
            .await
    }

    async fn pause(&self) -> fdo::Result<()> {
        if !self.can_pause().await? {
            return Ok(());
        }
        let tid = self.read_state(|s| s.player.track_instance_id.clone())?;
        let mut args = Self::empty_args();
        args.track_instance_id = tid;
        self.dispatch_op("mpris-pause", PlayerOp::Pause, Some(args))
            .await
    }

    async fn play_pause(&self) -> fdo::Result<()> {
        if !self.can_play().await? && !self.can_pause().await? {
            return Err(fdo::Error::NotSupported(
                "playPause is currently unavailable".to_string(),
            ));
        }
        self.dispatch_op("mpris-toggle", PlayerOp::Toggle, None)
            .await
    }

    async fn stop(&self) -> fdo::Result<()> {
        if !self.can_pause().await? || !self.can_seek().await? {
            return Err(fdo::Error::NotSupported(
                "stop is currently unavailable".to_string(),
            ));
        }
        let tid = self.read_state(|s| s.player.track_instance_id.clone())?;
        let mut args = Self::empty_args();
        args.track_instance_id = tid;
        self.dispatch_op("mpris-stop", PlayerOp::Stop, Some(args))
            .await
    }

    async fn play(&self) -> fdo::Result<()> {
        if !self.can_play().await? {
            return Ok(());
        }
        let tid = self.read_state(|s| s.player.track_instance_id.clone())?;
        let mut args = Self::empty_args();
        args.track_instance_id = tid;
        self.dispatch_op("mpris-play", PlayerOp::Play, Some(args))
            .await
    }

    async fn seek(&self, offset: Time) -> fdo::Result<()> {
        if !self.can_seek().await? {
            return Ok(());
        }
        let now_ms = self.core.clock().now_ms();
        let (tid, current_pos_ms, duration_ms, can_next) = self.read_state(|s| {
            (
                s.player.track_instance_id.clone(),
                s.estimated_position_ms(now_ms),
                s.player.time_anchor.duration_ms,
                s.supports(s.capabilities.next),
            )
        })?;

        let offset_ms = (offset.as_micros() / 1_000) as i128;
        let target_i128 = (current_pos_ms as i128).saturating_add(offset_ms);
        if let Some(dur) = duration_ms
            && target_i128 > dur as i128
        {
            if can_next {
                return self
                    .dispatch_op("mpris-seek-next", PlayerOp::Next, None)
                    .await;
            }
            let mut args = Self::empty_args();
            args.track_instance_id = tid;
            args.position_ms = Some(dur);
            return self
                .dispatch_op("mpris-seek", PlayerOp::Seek, Some(args))
                .await;
        }

        let target_ms = u64::try_from(target_i128.max(0)).unwrap_or(0);
        let mut args = Self::empty_args();
        args.track_instance_id = tid;
        args.position_ms = Some(target_ms);
        self.dispatch_op("mpris-seek", PlayerOp::Seek, Some(args))
            .await
    }

    async fn set_position(&self, track_id: TrackId, position: Time) -> fdo::Result<()> {
        if !self.can_seek().await? || track_id == TrackId::NO_TRACK {
            return Ok(());
        }
        let Some(pos_ms) = mpris_time_to_ms(position) else {
            return Ok(());
        };

        let (tid, expected_track_id, duration_ms) = self.read_state(|s| {
            (
                s.player.track_instance_id.clone(),
                derive_track_id(
                    s.player.track_instance_id.as_ref(),
                    s.player.current_track.as_ref(),
                ),
                s.player.time_anchor.duration_ms,
            )
        })?;

        if tid.is_none() || track_id != expected_track_id {
            debug!(
                supplied = %track_id,
                expected = %expected_track_id,
                "ignoring stale MPRIS SetPosition for non-matching trackId"
            );
            return Ok(());
        }

        if let Some(dur_ms) = duration_ms
            && pos_ms > dur_ms
        {
            return Ok(());
        }

        let mut args = Self::empty_args();
        args.track_instance_id = tid;
        args.position_ms = Some(pos_ms);
        self.dispatch_op("mpris-setpos", PlayerOp::Seek, Some(args))
            .await
    }

    async fn open_uri(&self, _uri: String) -> fdo::Result<()> {
        Err(fdo::Error::NotSupported(
            "OpenUri is not supported by YTM Glass".to_string(),
        ))
    }

    async fn playback_status(&self) -> fdo::Result<PlaybackStatus> {
        self.read_state(|s| map_playback_status(&s.player))
    }

    async fn loop_status(&self) -> fdo::Result<LoopStatus> {
        self.read_state(|s| map_repeat_to_loop_status(s.player.repeat))
    }

    async fn set_loop_status(&self, loop_status: LoopStatus) -> mpris_server::zbus::Result<()> {
        let supported = self
            .read_state(|s| s.supports(s.capabilities.repeat))
            .map_err(|e| mpris_server::zbus::Error::Failure(e.to_string()))?;
        if !supported {
            return Err(mpris_server::zbus::Error::FDO(Box::new(
                fdo::Error::NotSupported(
                    "repeat/loop control is unsupported by page bridge".to_string(),
                ),
            )));
        }
        let mut args = Self::empty_args();
        args.repeat = Some(map_loop_status_to_repeat(loop_status));
        self.dispatch_op("mpris-loop", PlayerOp::SetRepeat, Some(args))
            .await
            .map_err(|e| mpris_server::zbus::Error::Failure(e.to_string()))
    }

    async fn rate(&self) -> fdo::Result<PlaybackRate> {
        Ok(1.0)
    }

    async fn set_rate(&self, rate: PlaybackRate) -> mpris_server::zbus::Result<()> {
        if (rate - 1.0).abs() <= f64::EPSILON {
            Ok(())
        } else {
            Err(mpris_server::zbus::Error::FDO(Box::new(
                fdo::Error::NotSupported("playback rate is fixed at 1.0".to_string()),
            )))
        }
    }

    async fn shuffle(&self) -> fdo::Result<bool> {
        self.read_state(|s| s.player.shuffle.unwrap_or(false))
    }

    async fn set_shuffle(&self, shuffle: bool) -> mpris_server::zbus::Result<()> {
        let supported = self
            .read_state(|s| s.supports(s.capabilities.shuffle))
            .map_err(|e| mpris_server::zbus::Error::Failure(e.to_string()))?;
        if !supported {
            return Err(mpris_server::zbus::Error::FDO(Box::new(
                fdo::Error::NotSupported(
                    "shuffle control is unsupported by page bridge".to_string(),
                ),
            )));
        }
        let mut args = Self::empty_args();
        args.shuffle = Some(shuffle);
        self.dispatch_op("mpris-shuffle", PlayerOp::SetShuffle, Some(args))
            .await
            .map_err(|e| mpris_server::zbus::Error::Failure(e.to_string()))
    }

    async fn metadata(&self) -> fdo::Result<Metadata> {
        self.read_state(|s| s.build_metadata())
    }

    async fn volume(&self) -> fdo::Result<Volume> {
        self.read_state(|s| s.player.volume.clamp(0.0, 1.0))
    }

    async fn set_volume(&self, volume: Volume) -> mpris_server::zbus::Result<()> {
        let supported = self
            .read_state(|s| s.supports(s.capabilities.volume))
            .map_err(|e| mpris_server::zbus::Error::Failure(e.to_string()))?;
        if !supported {
            return Ok(());
        }
        let clamped = if volume.is_finite() {
            volume.clamp(0.0, 1.0)
        } else {
            return Err(mpris_server::zbus::Error::FDO(Box::new(
                fdo::Error::InvalidArgs("volume must be finite".to_string()),
            )));
        };
        let mut args = Self::empty_args();
        args.volume = Some(clamped);
        self.dispatch_op("mpris-volume", PlayerOp::SetVolume, Some(args))
            .await
            .map_err(|e| mpris_server::zbus::Error::Failure(e.to_string()))
    }

    async fn position(&self) -> fdo::Result<Time> {
        let now_ms = self.core.clock().now_ms();
        self.read_state(|s| ms_to_mpris_time(s.estimated_position_ms(now_ms)))
    }

    async fn minimum_rate(&self) -> fdo::Result<PlaybackRate> {
        Ok(1.0)
    }

    async fn maximum_rate(&self) -> fdo::Result<PlaybackRate> {
        Ok(1.0)
    }

    async fn can_go_next(&self) -> fdo::Result<bool> {
        self.read_state(|s| s.supports(s.capabilities.next))
    }

    async fn can_go_previous(&self) -> fdo::Result<bool> {
        self.read_state(|s| s.supports(s.capabilities.previous))
    }

    async fn can_play(&self) -> fdo::Result<bool> {
        self.read_state(|s| s.can_play())
    }

    async fn can_pause(&self) -> fdo::Result<bool> {
        self.read_state(|s| s.supports(s.capabilities.play_pause))
    }

    async fn can_seek(&self) -> fdo::Result<bool> {
        self.read_state(|s| s.supports(s.capabilities.seek))
    }

    async fn can_control(&self) -> fdo::Result<bool> {
        // MPRIS makes this an intrinsic property of the implementation. Temporary
        // bridge loss is reflected by the per-operation capabilities.
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::TRACK_PATH_PREFIX;
    use std::path::Path;
    use std::sync::Mutex;
    use ytm_core::{BoxFuture, BridgePort, CoreDeps, EngineConfig, MockClock, NoopOutputGatePort};
    use ytm_protocol::{
        ArtistRef, AssetId, BridgeEpoch, BridgePlaybackObservedPayload, BridgeReadyPayload,
        CapabilitySet, CapabilitySupport, CoverRef, PlaybackStatus as CorePlaybackStatus,
        PlayerConnectionState, QueryError, RepeatMode, Track, TrackAvailability, TrackInstanceId,
        VideoId,
    };

    #[derive(Default)]
    struct RecordingBridgePort {
        commands: Mutex<Vec<PlayerCommandPayload>>,
    }

    impl BridgePort for RecordingBridgePort {
        fn send_player_command(
            &self,
            _bridge_epoch: BridgeEpoch,
            payload: PlayerCommandPayload,
        ) -> BoxFuture<'_, Result<(), QueryError>> {
            self.commands.lock().unwrap().push(payload);
            Box::pin(async { Ok(()) })
        }

        fn request_snapshot(
            &self,
            _bridge_epoch: BridgeEpoch,
        ) -> BoxFuture<'_, Result<(), QueryError>> {
            Box::pin(async { Ok(()) })
        }
    }

    fn make_track(vid: &str, title: &str, dur_ms: u64) -> Track {
        Track {
            video_id: VideoId::new(vid).unwrap(),
            title: title.to_string(),
            artists: vec![ArtistRef {
                id: None,
                name: "Artist One".to_string(),
            }],
            album: None,
            duration_ms: Some(dur_ms),
            cover: None,
            availability: TrackAvailability::Available,
            version_tag: None,
            is_liked: None,
        }
    }

    fn supported_caps() -> CapabilitySet {
        CapabilitySet {
            play_pause: CapabilitySupport::Supported,
            seek: CapabilitySupport::Supported,
            next: CapabilitySupport::Supported,
            previous: CapabilitySupport::Supported,
            volume: CapabilitySupport::Supported,
            ..CapabilitySet::default()
        }
    }

    #[test]
    fn bus_name_suffix_and_cover_path_containment() {
        assert_eq!(
            bus_name_suffix_from_full("org.mpris.MediaPlayer2.ytm_glass"),
            "ytm_glass"
        );
        assert_eq!(bus_name_suffix_from_full("ytm_glass"), "ytm_glass");

        let test_base =
            std::env::temp_dir().join(format!("ytm-mpris-cover-{}", std::process::id()));
        let root_dir = test_base.join("covers");
        let outside_dir = test_base.join("outside");
        let _ = std::fs::remove_dir_all(&test_base);
        std::fs::create_dir_all(&root_dir).unwrap();
        std::fs::create_dir_all(&outside_dir).unwrap();

        let valid_cover = root_dir.join("cover1.jpg");
        let outside_secret = outside_dir.join("secret.txt");
        std::fs::write(&valid_cover, b"jpgdata").unwrap();
        std::fs::write(&outside_secret, b"secret").unwrap();

        let ok_url = sanitize_contained_cover_file_url(&valid_cover, &root_dir);
        assert!(ok_url.is_some());
        assert!(ok_url.unwrap().starts_with("file://"));

        // Must reject arbitrary files outside covers cache root
        assert!(sanitize_contained_cover_file_url(&outside_secret, &root_dir).is_none());
        assert!(sanitize_contained_cover_file_url(Path::new("/etc/passwd"), &root_dir).is_none());
        let _ = std::fs::remove_dir_all(&test_base);
    }

    #[tokio::test]
    async fn metadata_publishes_only_current_local_cover_and_withdraws_invalidated_uri() {
        let core = CoreEngine::start(CoreDeps::default(), EngineConfig::default());
        let mut state = MprisCachedState::from_snapshot(&core.snapshot().await.unwrap());
        let mut player = state.player.clone();
        let mut track = make_track("video1", "Song", 200_000);
        track.cover = Some(CoverRef {
            asset_id: None,
            remote_url: Some("https://lh3.googleusercontent.com/mock".into()),
            accent_oklch: None,
        });
        player.current_track = Some(track.clone());
        assert_eq!(state.update_player(player.clone()), None);
        assert!(state.build_metadata().art_url().is_none());

        let first_id = AssetId::new("asset-0").unwrap();
        for i in 0..300 {
            let id = AssetId::new(format!("asset-{i}")).unwrap();
            track.cover.as_mut().unwrap().asset_id = Some(id.clone());
            player.current_track = Some(track.clone());
            assert_eq!(state.update_player(player.clone()), Some(id.clone()));
            assert!(state.build_metadata().art_url().is_none());
            let uri = format!("file:///covers/{i}.jpg");
            state.local_art = Some((id, uri.clone()));
            assert_eq!(state.update_player(player.clone()), None);
            assert_eq!(state.build_metadata().art_url(), Some(uri));
        }
        // Returning to the first track requires a fresh, contained file lookup.
        track.cover.as_mut().unwrap().asset_id = Some(first_id.clone());
        player.current_track = Some(track);
        assert_eq!(state.update_player(player.clone()), Some(first_id.clone()));
        state.local_art = Some((first_id.clone(), "file:///covers/0.jpg".into()));
        let before = state.to_view();
        state.apply_delta(StateDelta::ResourceInvalidated {
            revision: 8,
            resource_kind: "catalog".into(),
            resource_id: None,
        });
        let after = state.to_view();
        assert!(after.metadata.art_url().is_none());
        assert!(matches!(
            &diff_views(&before, &after)[..],
            [mpris_server::Property::Metadata(_)]
        ));
        assert_eq!(state.update_player(player), Some(first_id));
        core.shutdown().await.unwrap();
    }

    #[test]
    fn track_id_derivation_and_time_units() {
        let tid = TrackInstanceId::new("ti:bridge-1:42").unwrap();
        let derived = derive_track_id(Some(&tid), None);
        assert_ne!(derived, TrackId::NO_TRACK);
        assert!(derived.as_str().starts_with(TRACK_PATH_PREFIX));
        assert!(TrackId::try_from(derived.as_str()).is_ok());

        let tid_other = TrackInstanceId::new("ti_bridge_1_42").unwrap();
        assert_ne!(derived, derive_track_id(Some(&tid_other), None));
        assert_eq!(derive_track_id(None, None), TrackId::NO_TRACK);

        assert_eq!(ms_to_mpris_time(42_150), Time::from_micros(42_150_000));
        assert_eq!(
            mpris_time_to_ms(Time::from_micros(42_150_000)),
            Some(42_150)
        );
        assert_eq!(mpris_time_to_ms(Time::from_micros(-100)), None);
    }

    #[tokio::test]
    async fn mpris_service_routes_commands_checks_capabilities_and_maps_stopped() {
        let clock = Arc::new(MockClock::new(1_000));
        let bridge = Arc::new(RecordingBridgePort::default());
        let core = Arc::new(CoreEngine::start(
            CoreDeps {
                clock: clock.clone(),
                bridge: bridge.clone(),
                output_gate: Arc::new(NoopOutputGatePort),
                ..CoreDeps::default()
            },
            EngineConfig::default(),
        ));

        let b_epoch = BridgeEpoch::new("bridge-test-1").unwrap();
        assert!(
            core.on_bridge_ready(
                1,
                BridgeReadyPayload {
                    bridge_epoch: b_epoch.clone(),
                    bridge_version: "0.1.0".to_string(),
                    capabilities: supported_caps(),
                    account_hint: None,
                    logged_in_observed: false,
                }
            )
            .await
            .unwrap()
        );

        let tid = TrackInstanceId::new("ti:bridge-test-1:1").unwrap();
        let mut track = make_track("dQw4w9WgXcQ", "Test Track", 180_000);
        track.cover = Some(CoverRef {
            asset_id: Some(AssetId::new("asset-cover-1").unwrap()),
            remote_url: Some("https://lh3.googleusercontent.com/test".to_string()),
            accent_oklch: None,
        });
        assert!(
            core.on_bridge_playback_observed(
                2,
                BridgePlaybackObservedPayload {
                    bridge_epoch: b_epoch.clone(),
                    playback: CorePlaybackStatus::Playing,
                    track_instance_id: Some(tid.clone()),
                    current_track: Some(track),
                    current_queue_entry_id: None,
                    playback_context: None,
                    position_ms: 10_000,
                    duration_ms: Some(180_000),
                    playback_rate: 1.0,
                    sample_age_ms: 0,
                    volume: 0.75,
                    muted: false,
                    shuffle: Some(false),
                    repeat: Some(RepeatMode::Off),
                    autoplay: None,
                    codec: None,
                    audio_mode: None,
                    quality_label: None,
                    drm_protected: None,
                }
            )
            .await
            .unwrap()
        );

        let snap = core.snapshot().await.unwrap();
        let (tx, mut rx) = mpsc::channel(8);
        let svc = MprisService {
            core: Arc::clone(&core),
            config: MprisConfig::default(),
            state: Arc::new(RwLock::new(MprisCachedState::from_snapshot(&snap))),
            host_action_tx: tx,
            cmd_seq: AtomicU64::new(1),
        };

        assert!(svc.can_play().await.unwrap());
        assert!(svc.can_pause().await.unwrap());
        assert!(svc.can_go_next().await.unwrap());
        assert!(svc.can_go_previous().await.unwrap());
        assert!(svc.can_seek().await.unwrap());
        // Without a loaded track, Play is not offered (the page would ignore it).
        {
            let mut guard = svc.state.write().unwrap();
            let loaded = guard.to_view();
            let track = guard.player.current_track.take();
            assert!(!guard.can_play());
            assert!(
                diff_views(&loaded, &guard.to_view())
                    .iter()
                    .any(|change| matches!(change, mpris_server::Property::CanPlay(false)))
            );
            guard.player.current_track = track;
        }
        assert_eq!(svc.identity().await.unwrap(), "YTM Glass");
        assert_eq!(svc.desktop_entry().await.unwrap(), "local.ytmglass.Desktop");
        assert!(svc.can_raise().await.unwrap());
        assert!(svc.can_quit().await.unwrap());
        assert!(!svc.can_set_fullscreen().await.unwrap());
        assert!(svc.set_loop_status(LoopStatus::Playlist).await.is_err());
        assert!(svc.set_shuffle(true).await.is_err());
        assert!(svc.set_rate(1.5).await.is_err());

        clock.advance(5_000);
        assert_eq!(svc.position().await.unwrap(), Time::from_micros(15_000_000));

        // Stale TrackId ignored
        let stale_tid = TrackId::try_from("/local/ytmglass/Desktop/track/stale").unwrap();
        svc.set_position(stale_tid, Time::from_micros(30_000_000))
            .await
            .unwrap();
        assert!(bridge.commands.lock().unwrap().is_empty());

        // Relative Seek(+20s from 15s -> 35s)
        svc.seek(Time::from_micros(20_000_000)).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        {
            let cmds = bridge.commands.lock().unwrap();
            assert_eq!(cmds.len(), 1);
            assert_eq!(cmds[0].op, PlayerOp::Seek);
            assert_eq!(cmds[0].args.as_ref().unwrap().position_ms, Some(35_000));
        }

        // Stopped confirmed overrides Paused -> MPRIS Stopped
        {
            let mut guard = svc.state.write().unwrap();
            guard.player.playback = CorePlaybackStatus::Paused;
            guard.player.stopped_confirmed = true;
        }
        assert_eq!(
            svc.playback_status().await.unwrap(),
            PlaybackStatus::Stopped
        );

        // PlayerChanged may reach the adapter while its prior bridge capabilities are
        // still cached. Getters, commands and published properties respect the loss.
        {
            let mut state = svc.state.write().unwrap();
            let ready = state.to_view();
            state.player.connection = PlayerConnectionState::Disconnected;
            let changes = diff_views(&ready, &state.to_view());
            assert_eq!(changes.len(), 5);
        }
        assert!(!svc.can_play().await.unwrap());
        assert!(!svc.can_pause().await.unwrap());
        assert!(!svc.can_go_next().await.unwrap());
        assert!(!svc.can_go_previous().await.unwrap());
        assert!(!svc.can_seek().await.unwrap());
        assert!(svc.can_control().await.unwrap());
        svc.play().await.unwrap();
        svc.next().await.unwrap();
        assert!(svc.stop().await.is_err());
        assert!(svc.play_pause().await.is_err());
        assert_eq!(bridge.commands.lock().unwrap().len(), 1);

        svc.raise().await.unwrap();
        svc.quit().await.unwrap();
        assert_eq!(rx.recv().await, Some(MprisHostAction::Raise));
        assert_eq!(rx.recv().await, Some(MprisHostAction::Quit));

        core.shutdown().await.unwrap();
    }
}
