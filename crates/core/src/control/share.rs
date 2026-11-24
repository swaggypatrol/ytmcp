use ytm_control_api::{
    ControlError, ErrorCode, PageInput, Playlist, PlaylistPrivacy, RetryDisposition, ShareAccess,
    ShareInput, ShareLink, ShareTarget, TrackTarget,
};
use ytm_protocol::{Freshness, PlayerConnectionState, PlaylistId};

use super::{ControlRead, ControlService, ReadGrant, invalid, map_query_error};

impl ControlService {
    pub(super) async fn read_share_link(
        &self,
        grant: &ReadGrant,
        state: &ControlRead,
        input: ShareInput,
    ) -> Result<ShareLink, ControlError> {
        if state.snapshot.mock_mode {
            return Err(unsupported(
                "share links require observed YouTube Music entities",
            ));
        }
        let link = match input.target {
            ShareTarget::Track { target } => {
                let url = match target {
                    TrackTarget::Video { video_id } => {
                        let url = track_url(video_id.as_str())?;
                        let player = &state.snapshot.player;
                        if player.connection != PlayerConnectionState::Ready
                            || player.track_instance_id.is_none()
                            || !player
                                .current_track
                                .as_ref()
                                .is_some_and(|track| track.video_id == video_id)
                        {
                            return Err(unsupported(
                                "the video is outside the observed playback context",
                            ));
                        }
                        url
                    }
                    TrackTarget::Context { context_token } => {
                        let binding = self.resolve_context(grant, &context_token).await?;
                        let video_id = binding
                            .video_id
                            .filter(|_| {
                                binding.track_instance_id.is_some()
                                    && binding.bridge_epoch.is_some()
                            })
                            .ok_or_else(|| {
                                unsupported(
                                    "the context did not capture an observed playback instance",
                                )
                            })?;
                        track_url(video_id.as_str())?
                    }
                };
                link(url, None)
            }
            ShareTarget::Playlist { playlist_id } => {
                let url = playlist_url(playlist_id.as_str())?;
                let (catalog, context) = self.catalog_context(state)?;
                let fresh = catalog
                    .fresh_playlist(
                        context,
                        playlist_id.clone(),
                        PageInput {
                            cursor: None,
                            limit: Some(1),
                        },
                    )
                    .await
                    .map_err(map_query_error)?;
                link(url, playlist_visibility(&playlist_id, &fresh)?)
            }
            ShareTarget::Album { .. } => {
                return Err(unsupported(
                    "the catalog has not independently verified album share identities",
                ));
            }
            ShareTarget::Artist { .. } => {
                return Err(unsupported(
                    "the YouTube Music artist share route has not been verified",
                ));
            }
        };
        self.check_read_context(grant, state).await?;
        Ok(link)
    }
}

fn track_url(id: &str) -> Result<String, ControlError> {
    if id.len() != 11 || !url_id(id) {
        return Err(invalid(
            "a shareable video ID must have 11 URL-safe characters",
        ));
    }
    Ok(format!("https://music.youtube.com/watch?v={id}"))
}

fn playlist_url(id: &str) -> Result<String, ControlError> {
    if !url_id(id) {
        return Err(invalid("a playlist share target must be a URL-safe ID"));
    }
    if !id.starts_with("PL") || id.len() <= 2 {
        return Err(unsupported(
            "only canonical PL playlist IDs are supported by this share route",
        ));
    }
    Ok(format!("https://music.youtube.com/playlist?list={id}"))
}

fn url_id(id: &str) -> bool {
    id.bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn playlist_visibility(
    requested: &PlaylistId,
    fresh: &Playlist,
) -> Result<Option<PlaylistPrivacy>, ControlError> {
    // This catalog sets editable=true only on an owner header carrying the matching ID.
    // Other playlist responses may project the requested ID without an independent witness.
    if fresh.entries.freshness != Freshness::Fresh
        || fresh.playlist_id != *requested
        || fresh.editable != Some(true)
    {
        return Err(unsupported(
            "the provider did not independently confirm the playlist ID",
        ));
    }
    Ok(fresh.privacy)
}

fn link(url: String, visibility: Option<PlaylistPrivacy>) -> ShareLink {
    let access = match visibility {
        Some(PlaylistPrivacy::Private) => ShareAccess::Private,
        Some(PlaylistPrivacy::Unlisted) => ShareAccess::Unlisted,
        Some(PlaylistPrivacy::Public) | None => ShareAccess::Unknown,
    };
    ShareLink {
        url,
        visibility,
        access,
    }
}

fn unsupported(message: &str) -> ControlError {
    ControlError::new(
        ErrorCode::CapabilityUnavailable,
        RetryDisposition::NotRetryable,
        message,
    )
}

#[cfg(test)]
mod tests {
    use ytm_control_api::{ContextInput, ControlRequest, ControlResponse, Page};
    use ytm_protocol::{
        BridgeEpoch, BridgePlaybackObservedPayload, BridgeReadyPayload, PlaybackStatus, Track,
        TrackAvailability, TrackInstanceId, VideoId,
    };

    use super::*;
    use crate::{CoreDeps, CoreEngine, EngineConfig};

    #[test]
    fn urls_are_single_fixed_origin_and_single_target_parameter() {
        assert_eq!(
            track_url("ShareSong_A").unwrap(),
            "https://music.youtube.com/watch?v=ShareSong_A"
        );
        let playlist = "PL_Example0123456789abcdef012345678";
        assert_eq!(
            playlist_url(playlist).unwrap(),
            format!("https://music.youtube.com/playlist?list={playlist}")
        );
        for id in [
            "https://example.test",
            "hello&list=A",
            "hello?v=ABC",
            "hello#other",
            "hello%23abc",
            "bad/id",
        ] {
            assert_eq!(track_url(id).unwrap_err().code, ErrorCode::InvalidInput);
            assert_eq!(playlist_url(id).unwrap_err().code, ErrorCode::InvalidInput);
        }
        for id in ["VLPL_example", "RDAMVMexample", "OLAK5_example", "LM", "PL"] {
            assert_eq!(
                playlist_url(id).unwrap_err().code,
                ErrorCode::CapabilityUnavailable
            );
        }
        assert!(serde_json::from_value::<ShareInput>(serde_json::json!({
            "target": { "kind": "playlist", "playlistId": playlist, "url": "https://example.test" }
        })).is_err());
    }

    #[test]
    fn playlist_share_uses_proven_identity_and_preserves_visibility() {
        let id = PlaylistId::new("PL_fixture").unwrap();
        let mut observed = Playlist {
            playlist_id: id.clone(),
            identity_confirmed: true,
            title: "Synthetic playlist".into(),
            description: None,
            privacy: Some(PlaylistPrivacy::Private),
            editable: Some(true),
            snapshot_fingerprint: None,
            entries: Page {
                items: vec![],
                next_cursor: None,
                complete: true,
                source: "fixture".into(),
                freshness: Freshness::Fresh,
                captured_at_ms: 1000,
            },
        };
        let url = playlist_url(id.as_str()).unwrap();
        assert_eq!(
            link(url.clone(), playlist_visibility(&id, &observed).unwrap()).access,
            ShareAccess::Private
        );
        observed.privacy = Some(PlaylistPrivacy::Unlisted);
        let unlisted = link(url.clone(), playlist_visibility(&id, &observed).unwrap());
        assert_eq!(unlisted.access, ShareAccess::Unlisted);
        assert_eq!(unlisted.visibility, Some(PlaylistPrivacy::Unlisted));
        observed.privacy = Some(PlaylistPrivacy::Public);
        assert_eq!(
            link(url, playlist_visibility(&id, &observed).unwrap()).access,
            ShareAccess::Unknown
        );
        let other = PlaylistId::new("PL_other").unwrap();
        assert_eq!(
            playlist_visibility(&other, &observed).unwrap_err().code,
            ErrorCode::CapabilityUnavailable
        );
        observed.entries.freshness = Freshness::Stale;
        assert!(playlist_visibility(&id, &observed).is_err());
        observed.entries.freshness = Freshness::Fresh;
        observed.editable = None;
        assert!(playlist_visibility(&id, &observed).is_err());
    }

    async fn observe(engine: &CoreEngine, seq: u64, video_id: &str, bridge_epoch: &BridgeEpoch) {
        let track = Track {
            video_id: VideoId::new(video_id).unwrap(),
            title: "Synthetic song".into(),
            artists: vec![],
            album: None,
            duration_ms: Some(20_000),
            cover: None,
            availability: TrackAvailability::Available,
            version_tag: None,
            is_liked: None,
        };
        assert!(
            engine
                .on_bridge_playback_observed(
                    seq,
                    BridgePlaybackObservedPayload {
                        bridge_epoch: bridge_epoch.clone(),
                        playback: PlaybackStatus::Playing,
                        track_instance_id: Some(
                            TrackInstanceId::new(format!("share-occurrence-{seq}")).unwrap()
                        ),
                        current_track: Some(track),
                        current_queue_entry_id: None,
                        playback_context: None,
                        position_ms: 0,
                        duration_ms: Some(20_000),
                        buffered_ms: None,
                        playback_rate: 1.0,
                        sample_age_ms: 0,
                        volume: 0.5,
                        muted: false,
                        shuffle: None,
                        repeat: None,
                        autoplay: None,
                        codec: None,
                        audio_mode: None,
                        quality_label: None,
                        audio_itag: None,
                        audio_quality: None,
                        audio_bitrate_kbps: None,
                        drm_protected: None,
                    }
                )
                .await
                .unwrap()
        );
    }

    fn explicit(video_id: &str) -> ShareInput {
        ShareInput {
            target: ShareTarget::Track {
                target: TrackTarget::Video {
                    video_id: VideoId::new(video_id).unwrap(),
                },
            },
        }
    }

    #[tokio::test]
    async fn track_share_refuses_unobserved_ids_and_keeps_the_original_context() {
        let engine = CoreEngine::start(CoreDeps::default(), EngineConfig::default());
        let epoch = BridgeEpoch::new("fixture-share-bridge").unwrap();
        assert!(
            engine
                .on_bridge_ready(
                    1,
                    BridgeReadyPayload {
                        bridge_epoch: epoch.clone(),
                        bridge_version: "0.1.0".into(),
                        capabilities: Default::default(),
                        account_hint: None,
                        logged_in_observed: false,
                    }
                )
                .await
                .unwrap()
        );
        observe(&engine, 2, "ShareSong_A", &epoch).await;
        let service = ControlService::new(engine.clone());
        let grant = ReadGrant::new("fixture-client".into(), None);
        let state = service.authorized_state(&grant, false).await.unwrap();
        assert_eq!(
            service
                .read_share_link(&grant, &state, explicit("ShareSong_A"))
                .await
                .unwrap()
                .access,
            ShareAccess::Unknown
        );
        let error = service
            .read_share_link(&grant, &state, explicit("ShareSong_B"))
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::CapabilityUnavailable);
        let ControlResponse::Context(context) = service
            .execute(
                &grant,
                ControlRequest::GetContext(ContextInput {
                    preview_limit: Some(0),
                }),
            )
            .await
        else {
            panic!("expected a captured share context");
        };
        let input = ShareInput {
            target: ShareTarget::Track {
                target: TrackTarget::Context {
                    context_token: context.context_token,
                },
            },
        };
        observe(&engine, 3, "ShareSong_B", &epoch).await;
        let state = service.authorized_state(&grant, false).await.unwrap();
        let original = service
            .read_share_link(&grant, &state, input.clone())
            .await
            .unwrap();
        assert_eq!(
            original.url,
            "https://music.youtube.com/watch?v=ShareSong_A"
        );
        assert_eq!(original.visibility, None);
        assert_eq!(
            service
                .read_share_link(&grant, &state, explicit("ShareSong_B"))
                .await
                .unwrap()
                .url,
            "https://music.youtube.com/watch?v=ShareSong_B"
        );
        let unrelated = ReadGrant::new("another-client".into(), None);
        let error = service
            .read_share_link(&unrelated, &state, input)
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::ContextExpired);
        let mut mocked = state;
        mocked.snapshot.mock_mode = true;
        let error = service
            .read_share_link(&grant, &mocked, explicit("ShareSong_B"))
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::CapabilityUnavailable);
        engine.shutdown().await.unwrap();
    }
}
