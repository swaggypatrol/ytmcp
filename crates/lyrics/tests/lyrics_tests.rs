use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};
use ytm_core::{BoxFuture, LyricsPort, MockClock};
use ytm_lyrics::{
    CachedLyricsEntry, DEFAULT_USER_AGENT, InMemoryLyricsCache, LYRICS_HIT_TTL_MS,
    LYRICS_NEGATIVE_TTL_MS, LrclibClient, LrclibConfig, LyricsCachePort, LyricsCandidate,
    LyricsService, MatchConfig, ParsedLyrics, VersionMarker, build_lyrics_cache_key,
    effective_line_time_ms, evaluate_candidate, extract_requested_markers, parse_lyrics,
};
use ytm_protocol::{
    LyricsKind, LyricsLine, LyricsMatchMetadata, LyricsQueryRequest, QueryError, QueryErrorCode,
    VideoId,
};

fn sample_track(video_id: &str, title: &str, artist: &str) -> LyricsMatchMetadata {
    LyricsMatchMetadata {
        video_id: VideoId::new(video_id).unwrap(),
        title: title.to_string(),
        artist: artist.to_string(),
        album: Some("Neon City".to_string()),
        duration_ms: Some(210_000),
        version_tag: None,
    }
}

#[test]
fn lrc_parser_handles_calibrated_bom_crlf_multitag_duplicates_out_of_order_and_corrupt_lines() {
    let raw = include_str!("../../../fixtures/lyrics/calibrated_bom_crlf.lrc");
    assert!(raw.starts_with('\u{FEFF}'), "fixture must start with BOM");
    assert!(raw.contains("\r\n"), "fixture must use CRLF");

    let parsed = parse_lyrics(raw);
    match parsed {
        ParsedLyrics::Synced {
            lines,
            file_offset_ms,
            metadata,
        } => {
            assert_eq!(file_offset_ms, 250);
            assert_eq!(metadata.title.as_deref(), Some("Midnight Glass"));
            assert_eq!(metadata.artist.as_deref(), Some("The Night Runners"));
            assert_eq!(metadata.album.as_deref(), Some("Neon City"));
            assert_eq!(metadata.language.as_deref(), Some("en"));

            assert_eq!(
                lines,
                vec![
                    LyricsLine {
                        time_ms: 5_250,
                        text: "First line in centiseconds".to_string(),
                    },
                    LyricsLine {
                        time_ms: 12_000,
                        text: "Chorus repeats twice".to_string(),
                    },
                    LyricsLine {
                        time_ms: 18_500,
                        text: "Second verse arrives before first in raw file".to_string(),
                    },
                    LyricsLine {
                        time_ms: 25_000,
                        text: "Lead vocal at exact same timestamp".to_string(),
                    },
                    LyricsLine {
                        time_ms: 25_000,
                        text: "Harmony vocal at exact same timestamp".to_string(),
                    },
                    LyricsLine {
                        time_ms: 30_000,
                        text: "Whole seconds without fraction".to_string(),
                    },
                    LyricsLine {
                        time_ms: 35_000,
                        text: "<b>Literal</b> & <script>safe</script>".to_string(),
                    },
                    LyricsLine {
                        time_ms: 65_500,
                        text: "Chorus repeats twice".to_string(),
                    },
                ]
            );

            // The fixture's `[offset:+250]` shows lyrics 250 ms earlier (LRC convention); a
            // positive user offset delays them. The two stay distinct and saturate at 0.
            assert_eq!(effective_line_time_ms(5_250, file_offset_ms, 0), 5_000);
            assert_eq!(effective_line_time_ms(5_250, file_offset_ms, 300), 5_300);
            assert_eq!(effective_line_time_ms(5_250, file_offset_ms, -300), 4_700);
            assert_eq!(effective_line_time_ms(100, 500, -200), 0);
            assert_eq!(effective_line_time_ms(100, -500, 0), 600);
        }
        other => panic!("expected Synced, got {other:?}"),
    }
}

#[test]
fn lrc_parser_falls_back_to_plain_verbatim_without_html_escaping() {
    let raw = include_str!("../../../fixtures/lyrics/plain_only.txt");
    let parsed = parse_lyrics(raw);
    match parsed {
        ParsedLyrics::Plain { text } => {
            assert!(
                text.contains("<b>Preserved HTML</b> & entities &amp; <script>alert(1)</script>")
            );
            assert!(!text.contains("&lt;b&gt;"));
        }
        other => panic!("expected Plain fallback, got {other:?}"),
    }

    let neg_offset = parse_lyrics("[offset:-450]\n[00:01.5]Tenths");
    match neg_offset {
        ParsedLyrics::Synced {
            lines,
            file_offset_ms,
            ..
        } => {
            assert_eq!(file_offset_ms, -450);
            assert_eq!(lines[0].time_ms, 1_500);
        }
        other => panic!("expected Synced, got {other:?}"),
    }
}

#[test]
fn matcher_extracts_all_version_markers_and_avoids_live_verb_false_positive() {
    let track_with_tags = LyricsMatchMetadata {
        video_id: VideoId::new("vid-1").unwrap(),
        title: "Track (Acoustic) [2020 Remaster] - Radio Edit".to_string(),
        artist: "Artist".to_string(),
        album: Some("Demo Sessions (Sped Up / Slowed + Remix)".to_string()),
        duration_ms: Some(200_000),
        version_tag: Some("live karaoke instrumental".to_string()),
    };
    let markers = extract_requested_markers(&track_with_tags);
    assert_eq!(
        markers,
        vec![
            VersionMarker::Live,
            VersionMarker::Acoustic,
            VersionMarker::Remaster,
            VersionMarker::Remix,
            VersionMarker::Instrumental,
            VersionMarker::Karaoke,
            VersionMarker::RadioEdit,
            VersionMarker::Demo,
            VersionMarker::SpedUp,
            VersionMarker::Slowed,
        ]
    );

    // "Live Forever" is a studio song title using the verb "live", NOT a live recording.
    let live_forever = sample_track("vid-2", "Live Forever", "Oasis");
    assert!(extract_requested_markers(&live_forever).is_empty());

    let live_recording = sample_track("vid-3", "Live Forever (Live at Maine Road)", "Oasis");
    assert_eq!(
        extract_requested_markers(&live_recording),
        vec![VersionMarker::Live]
    );
}

#[test]
fn matcher_rejects_version_and_duration_and_artist_mismatches() {
    let studio_track = sample_track("vid-studio", "Midnight Glass", "The Night Runners");

    let live_candidate = LyricsCandidate {
        id: Some(1),
        track_name: "Midnight Glass (Live at Wembley)".to_string(),
        artist_name: "The Night Runners".to_string(),
        album_name: Some("Live at Wembley".to_string()),
        duration_ms: Some(210_000),
        instrumental: false,
        synced_lyrics: Some("[00:10.00]Live line".to_string()),
        plain_lyrics: None,
    };
    let eval_live = evaluate_candidate(
        &studio_track,
        &live_candidate,
        "lrclib:get",
        MatchConfig::default(),
    );
    assert!(!eval_live.accepted);
    assert!(eval_live.match_basis.contains("version_mismatch"));

    let long_candidate = LyricsCandidate {
        id: Some(2),
        track_name: "Midnight Glass".to_string(),
        artist_name: "The Night Runners".to_string(),
        album_name: Some("Neon City".to_string()),
        duration_ms: Some(225_000), // 15s delta > 8s threshold
        instrumental: false,
        synced_lyrics: Some("[00:10.00]Studio line".to_string()),
        plain_lyrics: None,
    };
    let eval_dur = evaluate_candidate(
        &studio_track,
        &long_candidate,
        "lrclib:get",
        MatchConfig::default(),
    );
    assert!(!eval_dur.accepted);
    assert!(eval_dur.match_basis.contains("duration_mismatch"));

    let wrong_artist = LyricsCandidate {
        id: Some(3),
        track_name: "Midnight Glass".to_string(),
        artist_name: "Completely Different Band".to_string(),
        album_name: Some("Other Album".to_string()),
        duration_ms: Some(210_000),
        instrumental: false,
        synced_lyrics: Some("[00:10.00]Wrong song".to_string()),
        plain_lyrics: None,
    };
    let eval_artist = evaluate_candidate(
        &studio_track,
        &wrong_artist,
        "lrclib:search",
        MatchConfig::default(),
    );
    assert!(!eval_artist.accepted);
    assert!(eval_artist.match_basis.contains("low_confidence"));
}

#[tokio::test]
async fn lrclib_get_and_search_fallback_and_anonymous_headers() {
    let server = MockServer::start().await;
    let calibrated = include_str!("../../../fixtures/lyrics/calibrated_bom_crlf.lrc");

    // 1) Exact /api/get hit verifies compliant User-Agent & no Cookie header.
    Mock::given(method("GET"))
        .and(path("/api/get"))
        .and(query_param("track_name", "Midnight Glass"))
        .and(query_param("artist_name", "The Night Runners"))
        .and(header("user-agent", DEFAULT_USER_AGENT))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": 101,
            "trackName": "Midnight Glass",
            "artistName": "The Night Runners",
            "albumName": "Neon City",
            "duration": 210.0,
            "instrumental": false,
            "syncedLyrics": calibrated
        })))
        .mount(&server)
        .await;

    let mut cfg = LrclibConfig::with_base_url(&server.uri()).unwrap();
    cfg.enabled = true;
    let service = LyricsService::new(cfg).unwrap();

    let res = service
        .query_lyrics(
            LyricsQueryRequest {
                track: sample_track("vid-101", "Midnight Glass", "The Night Runners"),
                set_user_offset_ms: None,
                reset_user_offset: false,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(res.kind, LyricsKind::Synced);
    assert_eq!(res.source, "lrclib");
    assert_eq!(res.file_offset_ms, 250);
    assert_eq!(res.user_offset_ms, 0);
    assert_eq!(res.synced_lines.len(), 8);

    // Verify no cookies or auth headers were ever sent.
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1);
    assert!(!reqs[0].headers.contains_key("cookie"));
    assert!(!reqs[0].headers.contains_key("authorization"));

    // 2) On /api/get 404, /api/search accepts both title fields and matches using trackName.
    let server2 = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/get"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server2)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "id": 201,
                "trackName": "Orion (Live)",
                "name": "Orion",
                "artistName": "Synth Collective",
                "duration": 180.0,
                "instrumental": false,
                "syncedLyrics": "[00:05.00]Wrong live"
            },
            {
                "id": 202,
                "trackName": "Orion",
                "name": "Orion (Live)",
                "artistName": "Synth Collective",
                "albumName": "Pulse",
                "duration": 180.0,
                "instrumental": true
            }
        ])))
        .mount(&server2)
        .await;

    let service2 =
        LyricsService::new(LrclibConfig::with_base_url(&server2.uri()).unwrap()).unwrap();
    let res2 = service2
        .query_lyrics(
            LyricsQueryRequest {
                track: LyricsMatchMetadata {
                    video_id: VideoId::new("vid-inst").unwrap(),
                    title: "Orion".to_string(),
                    artist: "Synth Collective".to_string(),
                    album: Some("Pulse".to_string()),
                    duration_ms: Some(180_000),
                    version_tag: None,
                },
                set_user_offset_ms: None,
                reset_user_offset: false,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(res2.kind, LyricsKind::Instrumental);
    assert!(res2.synced_lines.is_empty());
}

#[tokio::test]
async fn live_vs_studio_mismatch_returns_not_found_with_clear_match_basis() {
    let server = MockServer::start().await;
    // Legacy `name` without `trackName` must still preserve the title's live marker.
    Mock::given(method("GET"))
        .and(path("/api/get"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": 301,
            "name": "Midnight Glass (Live at Budokan)",
            "artistName": "The Night Runners",
            "albumName": "Neon City",
            "duration": 210.0,
            "instrumental": false,
            "syncedLyrics": "[00:10.00]Crowd cheers"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "id": 301,
                "trackName": "Midnight Glass (Live at Budokan)",
                "artistName": "The Night Runners",
                "albumName": "Live at Budokan",
                "duration": 210.0,
                "instrumental": false,
                "syncedLyrics": "[00:10.00]Crowd cheers"
            }
        ])))
        .mount(&server)
        .await;

    let service = LyricsService::new(LrclibConfig::with_base_url(&server.uri()).unwrap()).unwrap();
    let res = service
        .query_lyrics(
            LyricsQueryRequest {
                track: sample_track("vid-mismatch", "Midnight Glass", "The Night Runners"),
                set_user_offset_ms: None,
                reset_user_offset: false,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(res.kind, LyricsKind::NotFound);
    assert!(res.synced_lines.is_empty());
    let basis = res.match_basis.as_deref().unwrap_or("");
    assert!(
        basis.contains("version_mismatch"),
        "expected version_mismatch in match_basis, got: {basis}"
    );
}

#[tokio::test]
async fn http_429_parses_retry_after_and_does_not_poison_cache() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/get"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "17"))
        .mount(&server)
        .await;

    let cfg = LrclibConfig::with_base_url(&server.uri()).unwrap();
    let client = LrclibClient::new(cfg.clone()).unwrap();
    let track = sample_track("vid-429", "Midnight Glass", "The Night Runners");

    // Direct client returns QueryError::RateLimited with exact retry_after_ms.
    let err = client
        .fetch(&track, CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(err.code, QueryErrorCode::RateLimited);
    assert!(err.retriable);
    assert_eq!(err.retry_after_ms, Some(17_000));

    // The 429 starts a global cooldown: nothing is sent until Retry-After has elapsed.
    let other = sample_track("vid-429b", "Other Song", "Other Artist");
    let cooled = client
        .fetch(&other, CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(cooled.code, QueryErrorCode::RateLimited);
    assert!(
        cooled
            .retry_after_ms
            .is_some_and(|ms| ms > 0 && ms <= 17_000)
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 1);

    // Service returns LyricsKind::Unavailable with QueryError attached and does NOT cache it.
    let cache = Arc::new(InMemoryLyricsCache::new());
    let service = LyricsService::with_cache(cfg, cache.clone()).unwrap();
    let res = service
        .query_lyrics(
            LyricsQueryRequest {
                track,
                set_user_offset_ms: None,
                reset_user_offset: false,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(res.kind, LyricsKind::Unavailable);
    let inner_err = res.error.expect("error must be populated on Unavailable");
    assert_eq!(inner_err.code, QueryErrorCode::RateLimited);
    assert_eq!(inner_err.retry_after_ms, Some(17_000));
    assert_eq!(cache.entry_count(), 0);
}

#[tokio::test]
async fn disabled_setting_makes_zero_network_requests() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let mut cfg = LrclibConfig::with_base_url(&server.uri()).unwrap();
    cfg.enabled = false;
    let service = LyricsService::new(cfg).unwrap();

    let res = service
        .query_lyrics(
            LyricsQueryRequest {
                track: sample_track("vid-dis", "Midnight Glass", "The Night Runners"),
                set_user_offset_ms: Some(500),
                reset_user_offset: false,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(res.kind, LyricsKind::Disabled);
    assert_eq!(server.received_requests().await.unwrap().len(), 0);
}

#[tokio::test]
async fn user_offset_persistence_and_reset_separate_from_file_offset() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/get"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": 401,
            "trackName": "Midnight Glass",
            "artistName": "The Night Runners",
            "albumName": "Neon City",
            "duration": 210.0,
            "instrumental": false,
            "syncedLyrics": "[offset:+300]\n[00:10.00]Line"
        })))
        .expect(1) // Subsequent offset changes hit cache without extra network requests!
        .mount(&server)
        .await;

    let service = LyricsService::new(LrclibConfig::with_base_url(&server.uri()).unwrap()).unwrap();
    let track = sample_track("vid-off", "Midnight Glass", "The Night Runners");

    let first = service
        .query_lyrics(
            LyricsQueryRequest {
                track: track.clone(),
                set_user_offset_ms: Some(-450),
                reset_user_offset: false,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(first.kind, LyricsKind::Synced);
    assert_eq!(first.file_offset_ms, 300);
    assert_eq!(first.user_offset_ms, -450);

    // Subsequent read preserves persisted user offset without network call.
    let second = service
        .read_lyrics(track.clone(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(second.file_offset_ms, 300);
    assert_eq!(second.user_offset_ms, -450);

    // The write API is independent of lyric matching and provider fetches.
    assert_eq!(
        service.set_user_offset(&track.video_id, 250).await.unwrap(),
        250
    );
    assert_eq!(service.get_user_offset(&track.video_id).await.unwrap(), 250);
    assert_eq!(service.reset_user_offset(&track.video_id).await.unwrap(), 0);
    let reset = service
        .read_lyrics(track, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(reset.file_offset_ms, 300);
    assert_eq!(reset.user_offset_ms, 0);
}

#[derive(Default)]
struct FailingOffsetStore {
    cache: InMemoryLyricsCache,
    writes: AtomicUsize,
}

impl FailingOffsetStore {
    fn reject_write(&self) -> BoxFuture<'_, Result<(), QueryError>> {
        Box::pin(async {
            self.writes.fetch_add(1, Ordering::Relaxed);
            Err(QueryError {
                code: QueryErrorCode::StorageError,
                retriable: false,
                retry_after_ms: None,
                safe_message: "fixture offset write failed".into(),
                request_id: None,
            })
        })
    }
}

impl LyricsCachePort for FailingOffsetStore {
    fn get_lyrics(
        &self,
        key: &str,
    ) -> BoxFuture<'_, Result<Option<CachedLyricsEntry>, QueryError>> {
        self.cache.get_lyrics(key)
    }

    fn put_lyrics(&self, entry: CachedLyricsEntry) -> BoxFuture<'_, Result<(), QueryError>> {
        self.cache.put_lyrics(entry)
    }

    fn get_user_offset(&self, _id: &VideoId) -> BoxFuture<'_, Result<Option<i64>, QueryError>> {
        Box::pin(async { Ok(Some(350)) })
    }

    fn set_user_offset(&self, _id: &VideoId, _ms: i64) -> BoxFuture<'_, Result<(), QueryError>> {
        self.reject_write()
    }

    fn reset_user_offset(&self, _id: &VideoId) -> BoxFuture<'_, Result<(), QueryError>> {
        self.reject_write()
    }
}

#[tokio::test]
async fn legacy_lyrics_offset_commands_report_failed_writes() {
    let server = MockServer::start().await;
    let cache = Arc::new(FailingOffsetStore::default());
    let service = LyricsService::with_cache(
        LrclibConfig::with_base_url(&server.uri()).unwrap(),
        cache.clone(),
    )
    .unwrap();
    let track = sample_track("vid-offset-failure", "Midnight Glass", "The Night Runners");

    for (set_user_offset_ms, reset_user_offset) in [(Some(450), false), (None, true)] {
        let err = service
            .query_lyrics(
                LyricsQueryRequest {
                    track: track.clone(),
                    set_user_offset_ms,
                    reset_user_offset,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, QueryErrorCode::StorageError);
    }
    assert_eq!(cache.writes.load(Ordering::Relaxed), 2);
    assert!(server.received_requests().await.unwrap().is_empty());

    let read = service
        .read_lyrics(track, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(read.user_offset_ms, 350);
    assert_eq!(cache.writes.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn fast_switch_cancellation_drops_late_response() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/get"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(250))
                .set_body_json(serde_json::json!({
                    "id": 501,
                    "trackName": "Slow Song",
                    "artistName": "Artist",
                    "duration": 210.0,
                    "instrumental": false,
                    "syncedLyrics": "[00:05.00]Too late"
                })),
        )
        .mount(&server)
        .await;

    let service =
        Arc::new(LyricsService::new(LrclibConfig::with_base_url(&server.uri()).unwrap()).unwrap());
    let port: Arc<dyn LyricsPort> = service;

    let cancel = CancellationToken::new();
    let cancel_trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        cancel_trigger.cancel();
    });

    let err = port
        .query_lyrics(
            LyricsQueryRequest {
                track: sample_track("vid-slow", "Slow Song", "Artist"),
                set_user_offset_ms: None,
                reset_user_offset: false,
            },
            cancel,
        )
        .await
        .unwrap_err();

    assert_eq!(err.code, QueryErrorCode::StaleContext);
}

#[tokio::test]
async fn single_flight_deduplicates_concurrent_requests_and_ttls_expire_properly() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/get"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(60))
                .set_body_json(serde_json::json!({
                    "id": 601,
                    "trackName": "Dedup Song",
                    "artistName": "Artist",
                    "duration": 210.0,
                    "instrumental": false,
                    "syncedLyrics": "[00:02.00]Deduped"
                })),
        )
        .mount(&server)
        .await;

    let client = LrclibClient::new(LrclibConfig::with_base_url(&server.uri()).unwrap()).unwrap();
    let track = sample_track("vid-dedup", "Dedup Song", "Artist");

    let (r1, r2, r3, r4) = tokio::join!(
        client.fetch(&track, CancellationToken::new()),
        client.fetch(&track, CancellationToken::new()),
        client.fetch(&track, CancellationToken::new()),
        client.fetch(&track, CancellationToken::new()),
    );
    assert!(r1.is_ok() && r2.is_ok() && r3.is_ok() && r4.is_ok());
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        1,
        "concurrent identical lookups must collapse to 1 HTTP request"
    );

    // Verify 30d hit TTL & 24h negative TTL + version metadata cache-key invalidation.
    let clock = Arc::new(MockClock::with_wall_offset(1_000, 1_000_000));
    let cache = Arc::new(InMemoryLyricsCache::new());

    let ttl_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/get"))
        .and(query_param("track_name", "Hit Track"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": 701,
            "trackName": "Hit Track",
            "artistName": "Artist",
            "duration": 210.0,
            "instrumental": false,
            "syncedLyrics": "[00:01.00]Hit"
        })))
        .mount(&ttl_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/get"))
        .and(query_param("track_name", "Missing Track"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&ttl_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/search"))
        .and(query_param("track_name", "Missing Track"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&ttl_server)
        .await;

    let svc = LyricsService::with_cache_and_clock(
        LrclibConfig::with_base_url(&ttl_server.uri()).unwrap(),
        cache,
        clock.clone(),
    )
    .unwrap();

    let hit_track = sample_track("vid-ttl", "Hit Track", "Artist");
    let miss_track = sample_track("vid-miss", "Missing Track", "Artist");

    // Changing version_tag on the same video_id produces a distinct cache key.
    let mut hit_live = hit_track.clone();
    hit_live.version_tag = Some("live".to_string());
    assert_ne!(
        build_lyrics_cache_key("lrclib", &hit_track),
        build_lyrics_cache_key("lrclib", &hit_live)
    );

    svc.query_lyrics(
        LyricsQueryRequest {
            track: hit_track.clone(),
            set_user_offset_ms: None,
            reset_user_offset: false,
        },
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let miss_res = svc
        .query_lyrics(
            LyricsQueryRequest {
                track: miss_track.clone(),
                set_user_offset_ms: None,
                reset_user_offset: false,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(miss_res.kind, LyricsKind::NotFound);

    let count_after_initial = ttl_server.received_requests().await.unwrap().len();
    assert_eq!(count_after_initial, 3); // 1 hit /api/get + 1 miss /api/get + 1 miss /api/search

    // Advance 23h -> both hit and negative remain cached.
    clock.advance(23 * 60 * 60 * 1_000);
    svc.query_lyrics(
        LyricsQueryRequest {
            track: hit_track.clone(),
            set_user_offset_ms: None,
            reset_user_offset: false,
        },
        CancellationToken::new(),
    )
    .await
    .unwrap();
    svc.query_lyrics(
        LyricsQueryRequest {
            track: miss_track.clone(),
            set_user_offset_ms: None,
            reset_user_offset: false,
        },
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        ttl_server.received_requests().await.unwrap().len(),
        count_after_initial
    );

    // Advance past 24h -> negative cache expires, positive 30d hit remains cached.
    clock.advance(LYRICS_NEGATIVE_TTL_MS);
    svc.query_lyrics(
        LyricsQueryRequest {
            track: hit_track.clone(),
            set_user_offset_ms: None,
            reset_user_offset: false,
        },
        CancellationToken::new(),
    )
    .await
    .unwrap();
    svc.query_lyrics(
        LyricsQueryRequest {
            track: miss_track,
            set_user_offset_ms: None,
            reset_user_offset: false,
        },
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let count_after_24h = ttl_server.received_requests().await.unwrap().len();
    assert_eq!(count_after_24h, count_after_initial + 2); // miss re-queried (/api/get + /api/search)

    // Advance past 30d -> hit cache expires too.
    clock.advance(LYRICS_HIT_TTL_MS);
    svc.query_lyrics(
        LyricsQueryRequest {
            track: hit_track,
            set_user_offset_ms: None,
            reset_user_offset: false,
        },
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        ttl_server.received_requests().await.unwrap().len(),
        count_after_24h + 1
    );
}
