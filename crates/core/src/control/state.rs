use ytm_control_api::{
    AccountState, Album, Artist, AudioObservation, Capability, CapabilityArea, ContextSnapshot,
    EventCursor, PendingPlayback, PlayerInclude, PlayerState, PlayerView, QueueOccurrence,
    QueueProjection, Rating, Track,
};
use ytm_protocol::{
    AppSnapshot, CapabilitySupport, Freshness, PlayerConnectionState, QueueSnapshot,
};

pub(super) fn track_projection(track: &ytm_protocol::Track) -> Track {
    Track {
        video_id: track.video_id.clone(),
        title: track.title.clone(),
        artists: track
            .artists
            .iter()
            .map(|artist| Artist {
                id: artist.id.clone(),
                name: artist.name.clone(),
            })
            .collect(),
        album: track.album.as_ref().map(|album| Album {
            id: album.id.clone(),
            title: album.title.clone(),
        }),
        duration_ms: track.duration_ms,
        availability: track.availability,
        version_tag: track.version_tag.clone(),
        rating: match track.is_liked {
            Some(true) => Rating::Liked,
            _ => Rating::Unknown,
        },
    }
}

pub(super) fn context_projection(
    snapshot: &AppSnapshot,
    queue: Option<&QueueSnapshot>,
    captured_at_ms: u64,
    context_token: String,
    preview_limit: usize,
) -> ContextSnapshot {
    let player = &snapshot.player;
    let start = queue
        .and_then(|queue| {
            queue
                .entries
                .iter()
                .position(|entry| Some(&entry.entry_id) == queue.current_entry_id.as_ref())
        })
        .map_or(0, |index| index + 1);
    ContextSnapshot {
        cursor: EventCursor {
            core_epoch: snapshot.core_epoch.clone(),
            revision: snapshot.revision,
        },
        bridge_epoch: player.bridge_epoch.clone(),
        captured_at_ms,
        context_token,
        freshness: player_freshness(snapshot),
        player: player_projection(snapshot),
        account: account_projection(&snapshot.auth_state),
        queue: queue_projection(snapshot, queue, start, preview_limit),
        capabilities: capabilities(snapshot),
    }
}

fn player_freshness(snapshot: &AppSnapshot) -> Freshness {
    if snapshot.player.connection == PlayerConnectionState::Ready {
        Freshness::Fresh
    } else {
        Freshness::Stale
    }
}

fn player_projection(snapshot: &AppSnapshot) -> PlayerState {
    let player = &snapshot.player;
    PlayerState {
        connection: player.connection,
        playback: player.playback,
        track_instance_id: player.track_instance_id.clone(),
        track: player.current_track.as_ref().map(track_projection),
        playback_context: player.playback_context.clone(),
        time_anchor: player.time_anchor.clone(),
        volume: player.volume,
        muted: player.muted,
        shuffle: player.shuffle,
        repeat: player.repeat,
        autoplay: player.autoplay,
        active_op: snapshot.pending.active_op,
        pending_commands: snapshot.pending.pending_command_count,
        stopped_confirmed: player.stopped_confirmed,
    }
}

pub(super) fn player_view(snapshot: &AppSnapshot, include: PlayerInclude) -> PlayerView {
    let player = &snapshot.player;
    let include_state = include != PlayerInclude::Audio;
    PlayerView {
        bridge_epoch: player.bridge_epoch.clone(),
        freshness: player_freshness(snapshot),
        observed: include_state.then(|| player_projection(snapshot)),
        pending: include_state.then_some(PendingPlayback {
            active_op: snapshot.pending.active_op,
            commands: snapshot.pending.pending_command_count,
            desired_playback: snapshot.pending.desired_playback,
            desired_position_ms: snapshot.pending.desired_position_ms,
            desired_volume: snapshot.pending.desired_volume,
        }),
        audio: (include != PlayerInclude::State).then(|| AudioObservation {
            codec: player.codec.clone(),
            audio_mode: player.audio_mode.clone(),
            quality_label: player.quality_label.clone(),
            drm_protected: player.drm_protected,
        }),
    }
}

pub(super) fn account_projection(auth: &ytm_protocol::AuthStatusInfo) -> AccountState {
    AccountState {
        state: auth.state,
        account_epoch: auth.account_epoch.clone(),
        player_catalog_matched: auth.player_catalog_matched,
    }
}

pub(super) fn queue_projection(
    snapshot: &AppSnapshot,
    queue: Option<&QueueSnapshot>,
    start: usize,
    limit: usize,
) -> QueueProjection {
    let summary = &snapshot.queue_summary;
    let entries: Vec<_> = queue
        .into_iter()
        .flat_map(|queue| &queue.entries)
        .skip(start)
        .take(limit)
        .map(|entry| QueueOccurrence {
            entry_id: entry.entry_id.clone(),
            track: track_projection(&entry.track),
        })
        .collect();
    let next_index = start + entries.len();
    let next_cursor = queue.and_then(|queue| {
        let bridge = queue.bridge_epoch.as_ref()?;
        (limit > 0 && next_index < summary.loaded_count as usize).then(|| {
            format!(
                "{}/{}/{}/{}",
                snapshot.core_epoch, bridge, summary.queue_revision, next_index
            )
        })
    });
    QueueProjection {
        revision: summary.queue_revision,
        current_entry_id: summary.current_entry_id.clone(),
        entries,
        next_cursor,
        completeness: summary.completeness,
        has_more: summary.has_more,
        // Host v1 does not identify an infinite queue, even when its current window is partial.
        infinite: None,
        loaded_start: queue.and_then(|queue| queue.loaded_range.as_ref().map(|range| range.start)),
        loaded_end_exclusive: queue
            .and_then(|queue| queue.loaded_range.as_ref().map(|range| range.end_exclusive)),
    }
}

pub(super) fn capabilities(snapshot: &AppSnapshot) -> Vec<Capability> {
    let caps = &snapshot.capabilities;
    let playback = CapabilityArea::Playback;
    let queue = CapabilityArea::Queue;
    let catalog = CapabilityArea::Catalog;
    [
        ("artistDetail", catalog, true, caps.artist_detail),
        ("albumDetail", catalog, true, caps.album_detail),
        ("catalogSearch", catalog, true, caps.search),
        (
            "playlistRead",
            CapabilityArea::Playlists,
            true,
            caps.playlist_tracks,
        ),
        (
            "playlistCreate",
            CapabilityArea::Playlists,
            true,
            CapabilitySupport::Unverified,
        ),
        (
            "recommendations",
            catalog,
            true,
            CapabilitySupport::Unverified,
        ),
        ("shuffle", playback, true, caps.shuffle),
        ("playPause", playback, true, caps.play_pause),
        ("next", playback, true, caps.next),
        ("previous", playback, true, caps.previous),
        ("seek", playback, true, caps.seek),
        ("volume", playback, true, caps.volume),
        ("repeat", playback, true, caps.repeat),
        ("queueMirror", queue, true, caps.queue_mirror),
        ("queueEnqueue", queue, true, caps.queue_enqueue),
        ("queueReorder", queue, true, caps.queue_reorder),
        ("queueRemove", queue, true, caps.queue_remove),
        // An interruption inserts next and selects the inserted occurrence.
        ("queueInterrupt", queue, true, caps.queue_enqueue),
        // Clearing and replacing are best effort; the page offers them except on a radio queue
        // or while autoplay suggestions play.
        (
            "queueClearUpcoming",
            queue,
            true,
            caps.queue_clear.unwrap_or(CapabilitySupport::Unverified),
        ),
        (
            "queueReplaceUpcoming",
            queue,
            true,
            caps.queue_clear.unwrap_or(CapabilitySupport::Unverified),
        ),
        (
            "queueAutoplay",
            queue,
            true,
            caps.queue_autoplay.unwrap_or(CapabilitySupport::Unverified),
        ),
    ]
    .into_iter()
    .map(|(id, area, implemented, support)| {
        let available_now = implemented && support == CapabilitySupport::Supported;
        let reason = match (implemented, support) {
            (false, _) => Some("not_implemented".into()),
            (true, CapabilitySupport::Unsupported) => Some("unavailable_in_current_session".into()),
            (true, CapabilitySupport::Unverified) => Some("unverified_in_current_session".into()),
            (true, CapabilitySupport::Supported) => None,
        };
        Capability {
            id: id.into(),
            area,
            implemented,
            verified_at_ms: None,
            available_now,
            reason,
        }
    })
    .collect()
}
