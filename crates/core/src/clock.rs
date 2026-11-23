use ytm_protocol::{
    BridgePlaybackObservedPayload, PlaybackStatus, PlaybackTimeAnchor, PlayerConnectionState,
};

fn clamp_to_duration(position_ms: u64, duration_ms: Option<u64>) -> u64 {
    match duration_ms {
        Some(dur) => position_ms.min(dur),
        None => position_ms,
    }
}

fn sanitize_rate(rate: f64) -> f64 {
    if rate.is_finite() && rate > 0.0 && rate <= 16.0 {
        rate
    } else {
        1.0
    }
}

/// Monotonic process-local playback clock that anchors observed playback positions
/// and extrapolates strictly when `Playing && Ready`.
#[derive(Debug, Clone)]
pub struct PlaybackClock {
    anchor: PlaybackTimeAnchor,
}

impl PlaybackClock {
    pub fn new(now_ms: u64) -> Self {
        Self {
            anchor: PlaybackTimeAnchor {
                position_ms: 0,
                duration_ms: None,
                playback_rate: 1.0,
                anchor_core_monotonic_ms: now_ms,
                extrapolating: false,
            },
        }
    }

    pub fn anchor(&self) -> &PlaybackTimeAnchor {
        &self.anchor
    }

    /// Computes estimated playback position at `now_ms`, clamping to `duration_ms` when known.
    pub fn estimated_position_ms(&self, now_ms: u64) -> u64 {
        if !self.anchor.extrapolating {
            return clamp_to_duration(self.anchor.position_ms, self.anchor.duration_ms);
        }

        let elapsed = now_ms.saturating_sub(self.anchor.anchor_core_monotonic_ms);
        let rate = sanitize_rate(self.anchor.playback_rate);
        let advanced = (elapsed as f64) * rate;
        let delta = if advanced.is_finite() && advanced > 0.0 {
            advanced.round().min(u64::MAX as f64) as u64
        } else {
            0
        };
        clamp_to_duration(
            self.anchor.position_ms.saturating_add(delta),
            self.anchor.duration_ms,
        )
    }

    /// Returns a `PlaybackTimeAnchor` evaluated at `now_ms` for outgoing snapshots/deltas.
    pub fn snapshot_anchor(&self, now_ms: u64) -> PlaybackTimeAnchor {
        PlaybackTimeAnchor {
            position_ms: self.estimated_position_ms(now_ms),
            duration_ms: self.anchor.duration_ms,
            playback_rate: self.anchor.playback_rate,
            anchor_core_monotonic_ms: now_ms,
            extrapolating: self.anchor.extrapolating,
        }
    }

    /// Re-anchors from an authoritative `BridgePlaybackObservedPayload`.
    /// Handles track changes, seeks, duration changes, and play/pause/buffering transitions.
    pub fn update_from_observation(
        &mut self,
        obs: &BridgePlaybackObservedPayload,
        connection: PlayerConnectionState,
        now_ms: u64,
    ) {
        let rate = sanitize_rate(obs.playback_rate);
        let extrapolating = obs.playback == PlaybackStatus::Playing
            && connection == PlayerConnectionState::Ready
            && obs.playback_rate.is_finite()
            && obs.playback_rate > 0.0;
        let position_ms = clamp_to_duration(obs.position_ms, obs.duration_ms);

        self.anchor = PlaybackTimeAnchor {
            position_ms,
            duration_ms: obs.duration_ms,
            playback_rate: rate,
            anchor_core_monotonic_ms: now_ms,
            extrapolating,
        };
    }

    /// Freezes or resumes extrapolation when connection or playback readiness changes.
    pub fn on_connection_changed(
        &mut self,
        connection: PlayerConnectionState,
        playback: PlaybackStatus,
        now_ms: u64,
    ) {
        let current = self.estimated_position_ms(now_ms);
        let extrapolating =
            connection == PlayerConnectionState::Ready && playback == PlaybackStatus::Playing;
        self.anchor.position_ms = current;
        self.anchor.anchor_core_monotonic_ms = now_ms;
        self.anchor.extrapolating = extrapolating;
    }

    /// Re-anchors explicitly on a confirmed seek position.
    pub fn reanchor_seek(&mut self, position_ms: u64, now_ms: u64) {
        self.anchor.position_ms = clamp_to_duration(position_ms, self.anchor.duration_ms);
        self.anchor.anchor_core_monotonic_ms = now_ms;
    }

    /// Re-anchors when duration metadata changes.
    pub fn on_duration_changed(&mut self, duration_ms: Option<u64>, now_ms: u64) {
        let current = self.estimated_position_ms(now_ms);
        self.anchor.duration_ms = duration_ms;
        self.anchor.position_ms = clamp_to_duration(current, duration_ms);
        self.anchor.anchor_core_monotonic_ms = now_ms;
    }

    /// Drops extrapolation on system power suspend/resume so sleep time is never
    /// added to playback progress. Caller must request a fresh bridge snapshot.
    pub fn on_power_resume(&mut self, now_ms: u64) {
        self.anchor.position_ms =
            clamp_to_duration(self.anchor.position_ms, self.anchor.duration_ms);
        self.anchor.anchor_core_monotonic_ms = now_ms;
        self.anchor.extrapolating = false;
    }

    /// Resets the clock anchor when a new `bridgeEpoch` replaces the player document.
    pub fn on_bridge_epoch_rebuild(&mut self, now_ms: u64) {
        self.anchor = PlaybackTimeAnchor {
            position_ms: 0,
            duration_ms: None,
            playback_rate: 1.0,
            anchor_core_monotonic_ms: now_ms,
            extrapolating: false,
        };
    }
}
