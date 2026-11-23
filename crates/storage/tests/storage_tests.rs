use image::{ImageBuffer, ImageFormat, Rgba};
use rusqlite::{Connection, params};
use std::fs;
use std::io::Cursor;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use ytm_core::ports::{Clock, MockClock, StoragePort};
use ytm_protocol::{
    AccountEpoch, AccountScope, AlbumDetail, AlbumId, AppSettings, ArtistDetail, ArtistId,
    ArtistRef, AssetId, Freshness, HomeFeed, LyricsKind, LyricsLine, LyricsMatchMetadata,
    LyricsResult, PagedResult, PlaylistDetail, PlaylistId, PlaylistSummary, QueryErrorCode,
    SearchResult, ThemeMode, Track, TrackAvailability, UiLanguage, VideoId,
};
use ytm_storage::{
    AccountCacheContext, CoverCacheConfig, LibraryMembershipCache, RecentPlaybackRecord,
    SqliteStorage, StoragePaths, is_public_unicast_ip,
};

fn fixture_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/storage")
        .join(rel)
}

fn make_track(video_id: &str, title: &str) -> Track {
    Track {
        video_id: VideoId::new(video_id).unwrap(),
        title: title.to_string(),
        artists: vec![ArtistRef {
            id: Some(ArtistId::new("UC_SYNTH_1").unwrap()),
            name: "Synthetic Artist".to_string(),
        }],
        album: None,
        duration_ms: Some(210_000),
        cover: None,
        availability: TrackAvailability::Available,
        version_tag: None,
        is_liked: Some(false),
    }
}

fn make_solid_png(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
    let img: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::from_pixel(width, height, Rgba(rgba));
    let mut buf = Cursor::new(Vec::new());
    img.write_to(&mut buf, ImageFormat::Png).unwrap();
    buf.into_inner()
}

fn make_solid_webp(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
    let img: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::from_pixel(width, height, Rgba(rgba));
    let mut buf = Cursor::new(Vec::new());
    img.write_to(&mut buf, ImageFormat::WebP).unwrap();
    buf.into_inner()
}

#[tokio::test]
async fn settings_saved_before_the_ui_language_existed_load_as_english() {
    let tmp = TempDir::new().unwrap();
    let paths = StoragePaths::for_root(tmp.path());
    let clock = Arc::new(MockClock::new(1_700_000_000_000));
    let storage =
        SqliteStorage::open(paths.clone(), clock, CoverCacheConfig::production()).unwrap();
    let old = r#"{
  "theme": "light",
  "reduceMotion": true,
  "reduceTransparency": false,
  "uiScalePercent": 125,
  "autoplayOnLaunch": false,
  "closeToTray": true,
  "lrclibEnabled": false,
  "trackNotifications": true,
  "nativeWindowDecorations": true
}"#;
    fs::write(paths.settings_file(), old).unwrap();

    let loaded = storage.load_settings().await.unwrap().unwrap();
    assert_eq!(loaded.ui_language, UiLanguage::English);
    assert_eq!(loaded.theme, ThemeMode::Light);
    assert_eq!(loaded.ui_scale_percent, 125);
    assert!(!loaded.lrclib_enabled);

    let chinese = AppSettings {
        ui_language: UiLanguage::SimplifiedChinese,
        ..loaded
    };
    storage.save_settings(chinese.clone()).await.unwrap();
    let raw = fs::read_to_string(paths.settings_file()).unwrap();
    assert!(raw.contains(r#""uiLanguage": "zh-CN""#), "{raw}");
    assert_eq!(storage.load_settings().await.unwrap().unwrap(), chinese);
}

#[tokio::test]
async fn xdg_paths_permissions_and_atomic_settings_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let paths = StoragePaths::for_root(tmp.path());
    let clock = Arc::new(MockClock::new(1_700_000_000_000));
    let storage =
        SqliteStorage::open(paths.clone(), clock, CoverCacheConfig::production()).unwrap();

    assert!(storage.load_settings().await.unwrap().is_none());

    let settings = AppSettings {
        theme: ThemeMode::Light,
        reduce_motion: true,
        reduce_transparency: true,
        ui_scale_percent: 150,
        autoplay_on_launch: false,
        close_to_tray: false,
        lrclib_enabled: true,
        track_notifications: true,
        native_window_decorations: false,
        ui_language: UiLanguage::SimplifiedChinese,
    };
    storage.save_settings(settings.clone()).await.unwrap();
    let loaded = storage.load_settings().await.unwrap().unwrap();
    assert_eq!(loaded, settings);

    let config_mode = fs::metadata(&paths.config_dir)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    let data_mode = fs::metadata(&paths.data_dir).unwrap().permissions().mode() & 0o777;
    let covers_mode = fs::metadata(paths.covers_dir())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    let settings_mode = fs::metadata(paths.settings_file())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    let db_mode = fs::metadata(paths.database_file())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;

    assert_eq!(config_mode, 0o700);
    assert_eq!(data_mode, 0o700);
    assert_eq!(covers_mode, 0o700);
    assert_eq!(settings_mode, 0o600);
    assert_eq!(db_mode, 0o600);

    // Ensure no leftover temp files in config_dir.
    let entries: Vec<_> = fs::read_dir(&paths.config_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(entries, vec!["settings.json"]);

    // Verify covers dir is strictly under cache_dir, separate from data_dir/config_dir.
    assert!(paths.covers_dir().starts_with(&paths.cache_dir));
    assert!(!paths.covers_dir().starts_with(&paths.data_dir));

    // Verify from_xdg resolves `ytmcp` app-identity directory names cleanly.
    let xdg = StoragePaths::from_xdg().unwrap();
    assert_eq!(
        xdg.config_dir.file_name().and_then(|s| s.to_str()),
        Some("ytmcp")
    );
    assert_eq!(
        xdg.data_dir.file_name().and_then(|s| s.to_str()),
        Some("ytmcp")
    );
    assert_eq!(
        xdg.cache_dir.file_name().and_then(|s| s.to_str()),
        Some("ytmcp")
    );
}

#[tokio::test]
async fn a_database_from_before_the_rename_is_adopted_with_its_wal() {
    // An install from before the rename (YTM Glass) left `ytm-glass.db` with its WAL and
    // shared-memory files; opening the storage takes them over under the current name and keeps
    // what was committed, including what still sits in the WAL.
    let tmp = TempDir::new().unwrap();
    let paths = StoragePaths::for_root(tmp.path());
    fs::create_dir_all(&paths.data_dir).unwrap();
    let legacy = paths.data_dir.join("ytm-glass.db");
    {
        let conn = Connection::open(&legacy).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
        conn.execute_batch(
            "CREATE TABLE kept (v TEXT); INSERT INTO kept VALUES ('from the old name');",
        )
        .unwrap();
        assert!(paths.data_dir.join("ytm-glass.db-wal").exists());
        // Leave the WAL in place, as a crash before a checkpoint would.
        std::mem::forget(conn);
    }

    let clock = Arc::new(MockClock::new(1_700_000_123_456));
    let storage =
        SqliteStorage::open(paths.clone(), clock, CoverCacheConfig::production()).unwrap();
    drop(storage);

    assert!(paths.database_file().exists());
    assert!(!legacy.exists());
    assert!(!paths.data_dir.join("ytm-glass.db-wal").exists());
    let conn = Connection::open(paths.database_file()).unwrap();
    let kept: String = conn
        .query_row("SELECT v FROM kept", [], |r| r.get(0))
        .unwrap();
    assert_eq!(kept, "from the old name");

    // A second open, or a fresh install without a legacy file, changes nothing.
    assert!(!paths.adopt_legacy_database().unwrap());
}

#[tokio::test]
async fn migration_from_v1_creates_online_backup_and_preserves_data() {
    let tmp = TempDir::new().unwrap();
    let paths = StoragePaths::for_root(tmp.path());
    fs::create_dir_all(&paths.data_dir).unwrap();

    // Seed an existing v1 database using the synthetic SQL fixture.
    let v1_sql = fs::read_to_string(fixture_path("v1_legacy.sql")).unwrap();
    {
        let conn = Connection::open(paths.database_file()).unwrap();
        conn.execute_batch(&v1_sql).unwrap();
    }

    let clock = Arc::new(MockClock::new(1_700_000_123_456));
    let storage =
        SqliteStorage::open(paths.clone(), clock, CoverCacheConfig::production()).unwrap();

    assert_eq!(storage.current_schema_version().unwrap(), 7);
    assert_eq!(storage.created_backups().len(), 1);
    let backup_path = &storage.created_backups()[0];
    assert!(backup_path.exists());
    assert_eq!(
        fs::metadata(backup_path).unwrap().permissions().mode() & 0o777,
        0o600
    );

    // Backup must contain the exact pre-migration v1 schema version and rows.
    let backup_conn = Connection::open(backup_path).unwrap();
    let backup_ver: u32 = backup_conn
        .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(backup_ver, 1);

    // Live DB must retain all v1 data after migration to the current schema.
    let scope = AccountScope::new("scope-alpha").unwrap();
    let latest = storage.get_latest_playback(&scope).await.unwrap().unwrap();
    assert_eq!(latest.track.video_id.as_str(), "vid_legacy_1");
    assert_eq!(latest.position_ms, 42_000);

    let offset = storage
        .get_lyrics_user_offset(&VideoId::new("vid_legacy_1").unwrap())
        .await
        .unwrap();
    assert_eq!(offset, -250);
}

#[tokio::test]
async fn corrupt_and_readonly_databases_return_storage_error_without_modification() {
    // 1. Corrupt DB must NEVER be deleted or overwritten.
    let tmp = TempDir::new().unwrap();
    let paths = StoragePaths::for_root(tmp.path());
    fs::create_dir_all(&paths.data_dir).unwrap();
    let garbage = b"NOT_A_VALID_SQLITE_DATABASE_HEADER_OR_PAGES_1234567890";
    fs::write(paths.database_file(), garbage).unwrap();

    let clock = Arc::new(MockClock::new(1_700_000_000_000));
    let err =
        match SqliteStorage::open(paths.clone(), clock.clone(), CoverCacheConfig::production()) {
            Ok(_) => panic!("corrupt db must fail to open"),
            Err(e) => e,
        };
    assert_eq!(err.code, QueryErrorCode::StorageError);
    assert!(paths.database_file().exists());
    assert_eq!(fs::read(paths.database_file()).unwrap(), garbage);

    // 2. Read-only DB needing migration (v1 -> v2) must fail cleanly without deleting/overwriting.
    let tmp_ro = TempDir::new().unwrap();
    let ro_paths = StoragePaths::for_root(tmp_ro.path());
    {
        let storage_v1 = SqliteStorage::open_at_schema_version(
            ro_paths.clone(),
            clock.clone(),
            CoverCacheConfig::production(),
            1,
        )
        .unwrap();
        assert_eq!(storage_v1.current_schema_version().unwrap(), 1);
    }
    let pre_bytes = fs::read(ro_paths.database_file()).unwrap();
    fs::set_permissions(ro_paths.database_file(), fs::Permissions::from_mode(0o444)).unwrap();

    let ro_err = match SqliteStorage::open(ro_paths.clone(), clock, CoverCacheConfig::production())
    {
        Ok(_) => panic!("read-only db must fail to open read-write"),
        Err(e) => e,
    };
    assert_eq!(ro_err.code, QueryErrorCode::StorageError);
    assert!(ro_paths.database_file().exists());
    assert_eq!(fs::read(ro_paths.database_file()).unwrap(), pre_bytes);
    // Restore permissions so TempDir cleanup succeeds cleanly.
    fs::set_permissions(ro_paths.database_file(), fs::Permissions::from_mode(0o600)).unwrap();
}

#[tokio::test]
async fn two_account_isolation_and_late_write_rejection_after_logout() {
    let tmp = TempDir::new().unwrap();
    let paths = StoragePaths::for_root(tmp.path());
    let clock = Arc::new(MockClock::new(1_700_000_000_000));
    let storage =
        SqliteStorage::open(paths, clock.clone(), CoverCacheConfig::production()).unwrap();

    let fixture_raw = fs::read_to_string(fixture_path("sample_catalog.json")).unwrap();
    let fixture_val: serde_json::Value = serde_json::from_str(&fixture_raw).unwrap();
    assert!(fixture_val["accounts"]["alpha"].is_object());

    let scope_a = AccountScope::new("scope-alpha-001").unwrap();
    let epoch_a1 = AccountEpoch::new("epoch-alpha-001").unwrap();
    let ctx_a = AccountCacheContext {
        account_scope: scope_a.clone(),
        account_epoch: epoch_a1.clone(),
        locale: "zh-CN".to_string(),
        provider_version: "ytm-mock-1.0".to_string(),
    };

    let scope_b = AccountScope::new("scope-beta-002").unwrap();
    let epoch_b1 = AccountEpoch::new("epoch-beta-001").unwrap();
    let ctx_b = AccountCacheContext {
        account_scope: scope_b.clone(),
        account_epoch: epoch_b1.clone(),
        locale: "en-US".to_string(),
        provider_version: "ytm-mock-1.0".to_string(),
    };

    storage
        .register_account_epoch(&scope_a, &epoch_a1)
        .await
        .unwrap();
    storage
        .register_account_epoch(&scope_b, &epoch_b1)
        .await
        .unwrap();

    let page_a = PagedResult {
        items: vec![PlaylistSummary {
            playlist_id: PlaylistId::new("PL_ALPHA").unwrap(),
            browse_id: None,
            title: "Alpha Private Mix".to_string(),
            subtitle: None,
            track_count: Some(1),
            cover: None,
        }],
        next_cursor: None,
        freshness: Freshness::Fresh,
        fetched_at_ms: clock.now_ms(),
        account_scope: scope_a.clone(),
        provider_version: "ytm-mock-1.0".to_string(),
    };
    storage
        .put_library_playlists(&ctx_a, None, 100, &page_a, None)
        .await
        .unwrap();

    let page_b = PagedResult {
        items: vec![PlaylistSummary {
            playlist_id: PlaylistId::new("PL_BETA").unwrap(),
            browse_id: None,
            title: "Beta Private Mix".to_string(),
            subtitle: None,
            track_count: Some(1),
            cover: None,
        }],
        next_cursor: None,
        freshness: Freshness::Fresh,
        fetched_at_ms: clock.now_ms(),
        account_scope: scope_b.clone(),
        provider_version: "ytm-mock-1.0".to_string(),
    };
    storage
        .put_library_playlists(&ctx_b, None, 100, &page_b, None)
        .await
        .unwrap();

    // Also test playlist_detail, search, album, artist, liked_songs, home_feed, recent_playback.
    let pl_id_a = PlaylistId::new("PL_ALPHA").unwrap();
    let detail_a = PlaylistDetail {
        playlist_id: pl_id_a.clone(),
        title: "Alpha Private Mix".to_string(),
        description: None,
        author: Some("Alpha".to_string()),
        cover: None,
        tracks: PagedResult {
            items: vec![make_track("vid_a1", "Alpha Song 1")],
            next_cursor: Some("cursor_2".to_string()),
            freshness: Freshness::Fresh,
            fetched_at_ms: clock.now_ms(),
            account_scope: scope_a.clone(),
            provider_version: "ytm-mock-1.0".to_string(),
        },
    };
    storage
        .put_playlist_detail(&ctx_a, &pl_id_a, None, 100, &detail_a, None)
        .await
        .unwrap();

    // Account B cannot see Account A's playlists or details.
    assert!(
        storage
            .get_library_playlists(&scope_b, "zh-CN", None, 100)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        storage
            .get_playlist_detail(&scope_b, "zh-CN", &pl_id_a, None, 100)
            .await
            .unwrap()
            .is_none()
    );
    let got_b = storage
        .get_library_playlists(&scope_b, "en-US", None, 100)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got_b.value.items[0].playlist_id.as_str(), "PL_BETA");

    // Cross-account epoch mismatch must fail.
    let mismatched_ctx = AccountCacheContext {
        account_scope: scope_b.clone(),
        account_epoch: epoch_a1.clone(),
        locale: "en-US".to_string(),
        provider_version: "ytm-mock-1.0".to_string(),
    };
    let mismatch_err = storage
        .put_library_playlists(&mismatched_ctx, None, 100, &page_b, None)
        .await
        .unwrap_err();
    assert_eq!(mismatch_err.code, QueryErrorCode::AccountMismatch);

    // Logout Account A: clears Account A data and invalidates epoch_a1, leaving Account B intact.
    storage
        .logout_account(&scope_a, Some(&epoch_a1))
        .await
        .unwrap();
    assert!(
        storage
            .get_library_playlists(&scope_a, "zh-CN", None, 100)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        storage
            .get_library_playlists(&scope_b, "en-US", None, 100)
            .await
            .unwrap()
            .is_some()
    );

    // Late write from logged-out epoch_a1 MUST be rejected with StaleContext and NOT mutate cache.
    let late_err = storage
        .put_library_playlists(&ctx_a, None, 100, &page_a, None)
        .await
        .unwrap_err();
    assert_eq!(late_err.code, QueryErrorCode::StaleContext);
    assert!(
        storage
            .get_library_playlists(&scope_a, "zh-CN", None, 100)
            .await
            .unwrap()
            .is_none()
    );

    // Even if Account A logs back in with a NEW epoch_a2, stale writes carrying epoch_a1 still fail.
    let epoch_a2 = AccountEpoch::new("epoch-alpha-002").unwrap();
    storage
        .register_account_epoch(&scope_a, &epoch_a2)
        .await
        .unwrap();
    let late_err_2 = storage
        .record_recent_playback(
            &scope_a,
            &epoch_a1,
            RecentPlaybackRecord {
                track: make_track("vid_late", "Should Be Rejected"),
                playback_ref: None,
                position_ms: 1000,
                duration_ms: Some(10000),
                recorded_at_ms: clock.now_ms(),
                source_context: None,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(late_err_2.code, QueryErrorCode::StaleContext);
}

#[tokio::test]
async fn logout_retires_pending_epochs_on_either_side_of_registration() {
    let tmp = TempDir::new().unwrap();
    let storage = SqliteStorage::open(
        StoragePaths::for_root(tmp.path()),
        Arc::new(MockClock::new(10)),
        CoverCacheConfig::production(),
    )
    .unwrap();
    let previous = AccountScope::new("previous").unwrap();
    let pending = AccountScope::new("pending").unwrap();
    let other = AccountScope::new("other").unwrap();
    let early_epoch = AccountEpoch::new("before-registration").unwrap();
    let pending_epoch = AccountEpoch::new("before-publication").unwrap();
    let other_epoch = AccountEpoch::new("other-epoch").unwrap();

    storage
        .clear_account_data(None, Some(early_epoch.clone()))
        .await
        .unwrap();
    let err = storage
        .register_account_epoch(&pending, &early_epoch)
        .await
        .unwrap_err();
    assert_eq!(err.code, QueryErrorCode::StaleContext);
    assert!(storage.last_session().await.unwrap().is_none());

    for (scope, epoch) in [
        (&previous, AccountEpoch::new("previous-epoch").unwrap()),
        (&pending, pending_epoch.clone()),
        (&other, other_epoch.clone()),
    ] {
        storage.register_account_epoch(scope, &epoch).await.unwrap();
        storage
            .record_recent_playback(
                scope,
                &epoch,
                RecentPlaybackRecord {
                    track: make_track("vid-history", "Local history"),
                    playback_ref: None,
                    position_ms: 100,
                    duration_ms: None,
                    recorded_at_ms: 10,
                    source_context: None,
                },
            )
            .await
            .unwrap();
    }
    storage
        .clear_account_data(Some(previous.clone()), Some(pending_epoch.clone()))
        .await
        .unwrap();
    assert!(
        storage
            .get_latest_playback(&previous)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        storage
            .get_latest_playback(&pending)
            .await
            .unwrap()
            .is_none()
    );
    assert!(storage.get_latest_playback(&other).await.unwrap().is_some());
    assert_eq!(
        storage.last_session().await.unwrap().unwrap().account_scope,
        other
    );
    let err = storage
        .register_account_epoch(&pending, &pending_epoch)
        .await
        .unwrap_err();
    assert_eq!(err.code, QueryErrorCode::StaleContext);

    let rejoined = AccountEpoch::new("rejoined").unwrap();
    storage
        .register_account_epoch(&pending, &rejoined)
        .await
        .unwrap();
    storage
        .clear_account_data(None, Some(pending_epoch))
        .await
        .unwrap();
    storage
        .register_account_epoch(&pending, &rejoined)
        .await
        .unwrap();
}

#[tokio::test]
async fn persisted_catalog_page_size_does_not_alias_another_window() {
    let tmp = TempDir::new().unwrap();
    let storage = SqliteStorage::open(
        StoragePaths::for_root(tmp.path()),
        Arc::new(MockClock::new(1_700_000_000_000)),
        CoverCacheConfig::production(),
    )
    .unwrap();
    let scope = AccountScope::new("scope-paged").unwrap();
    let ctx = AccountCacheContext {
        account_scope: scope.clone(),
        account_epoch: AccountEpoch::new("epoch-paged").unwrap(),
        locale: "en-US".into(),
        provider_version: "fixture".into(),
    };
    let search = |limit| SearchResult {
        query: "synthetic".into(),
        songs: PagedResult {
            items: (0..limit)
                .map(|i| make_track(&format!("synthetic-{i}"), "Fixture"))
                .collect(),
            next_cursor: Some(format!("after-{limit}")),
            freshness: Freshness::Fresh,
            fetched_at_ms: 1_700_000_000_000,
            account_scope: scope.clone(),
            provider_version: ctx.provider_version.clone(),
        },
        albums: vec![],
        playlists: vec![],
        artists: vec![],
    };
    storage
        .put_search_result(&ctx, "synthetic", None, 100, &search(100), None)
        .await
        .unwrap();
    assert!(
        storage
            .get_search_result(&scope, "en-US", "synthetic", None, 10)
            .await
            .unwrap()
            .is_none()
    );

    storage
        .put_search_result(&ctx, "synthetic", None, 10, &search(10), None)
        .await
        .unwrap();
    for limit in [10, 100] {
        let page = storage
            .get_search_result(&scope, "en-US", "  SYNTHETIC  ", None, limit)
            .await
            .unwrap()
            .unwrap()
            .value
            .songs;
        assert_eq!(page.items.len(), limit);
        assert_eq!(page.next_cursor, Some(format!("after-{limit}")));
    }
}

#[tokio::test]
async fn playlist_membership_clears_library_fallbacks_across_locales_only_for_its_account() {
    let tmp = TempDir::new().unwrap();
    let paths = StoragePaths::for_root(tmp.path());
    let storage = SqliteStorage::open(
        paths.clone(),
        Arc::new(MockClock::new(1_700_000_000_000)),
        CoverCacheConfig::production(),
    )
    .unwrap();
    let connection = Connection::open(paths.database_file()).unwrap();
    let scope = AccountScope::new("scope-membership").unwrap();
    let other = AccountScope::new("scope-other").unwrap();
    let playlist = PlaylistId::new("PL-changed").unwrap();
    let independent = PlaylistId::new("PL-independent").unwrap();
    let membership_kinds = [
        "library_playlists_page_v2",
        "library_playlists",
        "home_feed",
    ];
    let independent_kinds = [
        "playlist_detail_page_v2",
        "liked_songs_page_v2",
        "search_page_v2",
    ];
    for account in [&scope, &other] {
        for locale in ["en-US", "zh-CN"] {
            for kind in membership_kinds.into_iter().chain(independent_kinds) {
                for window in ["first", "next"] {
                    connection
                        .execute(
                            "INSERT INTO catalog_cache VALUES (?1, ?2, ?3, ?4, '{}', 'fixture', 0, 3600)",
                            params![account.as_str(), locale, kind, window],
                        )
                        .unwrap();
                }
            }
            for id in [&playlist, &independent] {
                connection
                    .execute(
                        "INSERT INTO playlists
                         (account_scope, locale, playlist_id, title, position_idx, provider_version, fetched_at_ms, ttl_secs)
                         VALUES (?1, ?2, ?3, 'Fixture', 0, 'fixture', 0, 3600)",
                        params![account.as_str(), locale, id.as_str()],
                    )
                    .unwrap();
            }
        }
    }
    let windows = |account: &AccountScope, kind: &str| -> u32 {
        connection
            .query_row(
                "SELECT count(*) FROM catalog_cache WHERE account_scope = ?1 AND entry_kind = ?2",
                params![account.as_str(), kind],
                |row| row.get(0),
            )
            .unwrap()
    };
    let summaries = |account: &AccountScope, id: &PlaylistId| -> u32 {
        connection
            .query_row(
                "SELECT count(*) FROM playlists WHERE account_scope = ?1 AND playlist_id = ?2",
                params![account.as_str(), id.as_str()],
                |row| row.get(0),
            )
            .unwrap()
    };

    storage
        .invalidate_library_membership(&scope, LibraryMembershipCache::Song)
        .await
        .unwrap();
    for kind in membership_kinds.into_iter().chain(independent_kinds) {
        assert_eq!(windows(&scope, kind), 4);
    }
    storage
        .invalidate_library_membership(&scope, LibraryMembershipCache::Playlist(&playlist))
        .await
        .unwrap();

    for kind in membership_kinds {
        assert_eq!(windows(&scope, kind), 0);
        assert_eq!(windows(&other, kind), 4);
    }
    for kind in independent_kinds {
        assert_eq!(windows(&scope, kind), 4);
        assert_eq!(windows(&other, kind), 4);
    }
    assert_eq!(summaries(&scope, &playlist), 0);
    assert_eq!(summaries(&scope, &independent), 2);
    assert_eq!(summaries(&other, &playlist), 2);
}

#[tokio::test]
async fn verified_playlist_mutation_invalidates_all_targeted_windows_within_one_account() {
    let tmp = TempDir::new().unwrap();
    let clock = Arc::new(MockClock::new(1_700_000_000_000));
    let storage = SqliteStorage::open(
        StoragePaths::for_root(tmp.path()),
        clock.clone(),
        CoverCacheConfig::production(),
    )
    .unwrap();
    let ctx = AccountCacheContext {
        account_scope: AccountScope::new("scope-write").unwrap(),
        account_epoch: AccountEpoch::new("epoch-write").unwrap(),
        locale: "en-US".into(),
        provider_version: "fixture".into(),
    };
    let other = AccountCacheContext {
        account_scope: AccountScope::new("scope-other").unwrap(),
        account_epoch: AccountEpoch::new("epoch-other").unwrap(),
        ..ctx.clone()
    };
    let target = PlaylistId::new("playlist-edited").unwrap();
    let unrelated = PlaylistId::new("playlist-independent").unwrap();
    let detail = |id: &PlaylistId, cache: &AccountCacheContext| PlaylistDetail {
        playlist_id: id.clone(),
        title: "Fixture".into(),
        description: None,
        author: None,
        cover: None,
        tracks: PagedResult {
            items: vec![make_track("video1", "Fixture")],
            next_cursor: None,
            freshness: Freshness::Fresh,
            fetched_at_ms: clock.now_ms(),
            account_scope: cache.account_scope.clone(),
            provider_version: cache.provider_version.clone(),
        },
    };
    let library = |cache: &AccountCacheContext| PagedResult {
        items: vec![PlaylistSummary {
            playlist_id: target.clone(),
            browse_id: None,
            title: "Fixture".into(),
            subtitle: None,
            track_count: Some(1),
            cover: None,
        }],
        next_cursor: Some("playlist-next".into()),
        freshness: Freshness::Fresh,
        fetched_at_ms: clock.now_ms(),
        account_scope: cache.account_scope.clone(),
        provider_version: cache.provider_version.clone(),
    };
    for cache in [&ctx, &other] {
        for (cursor, limit) in [(None, 10), (Some("another-page"), 100)] {
            storage
                .put_playlist_detail(cache, &target, cursor, limit, &detail(&target, cache), None)
                .await
                .unwrap();
            storage
                .put_library_playlists(cache, cursor, limit, &library(cache), None)
                .await
                .unwrap();
        }
        let home_feed = HomeFeed {
            personalized_supported: false,
            fallback_notice: None,
            recent_tracks: vec![],
            pinned_playlists: library(cache).items,
            freshness: Freshness::Fresh,
        };
        storage
            .put_home_feed(cache, &home_feed, None)
            .await
            .unwrap();
    }
    storage
        .put_playlist_detail(&ctx, &unrelated, None, 10, &detail(&unrelated, &ctx), None)
        .await
        .unwrap();

    storage
        .invalidate_playlist_cache(&ctx.account_scope, &target, true)
        .await
        .unwrap();
    for (cursor, limit) in [(None, 10), (Some("another-page"), 100)] {
        assert!(
            storage
                .get_playlist_detail(&ctx.account_scope, &ctx.locale, &target, cursor, limit)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            storage
                .get_library_playlists(&ctx.account_scope, &ctx.locale, cursor, limit)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            storage
                .get_playlist_detail(&other.account_scope, &other.locale, &target, cursor, limit)
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            storage
                .get_library_playlists(&other.account_scope, &other.locale, cursor, limit)
                .await
                .unwrap()
                .is_some()
        );
    }
    assert!(
        storage
            .get_playlist_detail(&ctx.account_scope, &ctx.locale, &unrelated, None, 10)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        storage
            .get_home_feed(&ctx.account_scope, &ctx.locale)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        storage
            .get_home_feed(&other.account_scope, &other.locale)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn catalog_ttl_lyrics_ttl_and_user_offset_separation() {
    let tmp = TempDir::new().unwrap();
    let paths = StoragePaths::for_root(tmp.path());
    let clock = Arc::new(MockClock::new(1_700_000_000_000));
    let storage =
        SqliteStorage::open(paths, clock.clone(), CoverCacheConfig::production()).unwrap();

    let scope = AccountScope::new("scope-ttl").unwrap();
    let epoch = AccountEpoch::new("epoch-ttl").unwrap();
    let ctx = AccountCacheContext {
        account_scope: scope.clone(),
        account_epoch: epoch.clone(),
        locale: "en-US".to_string(),
        provider_version: "ytm-mock-1.0".to_string(),
    };

    // Catalog search / album / artist / liked / home TTL (15m = 900s, 1h = 3600s).
    let search = SearchResult {
        query: "city pop".to_string(),
        songs: PagedResult {
            items: vec![make_track("vid_cp", "Plastic Love")],
            next_cursor: None,
            freshness: Freshness::Fresh,
            fetched_at_ms: clock.now_ms(),
            account_scope: scope.clone(),
            provider_version: "ytm-mock-1.0".to_string(),
        },
        albums: vec![],
        playlists: vec![],
        artists: vec![],
    };
    storage
        .put_search_result(&ctx, "city pop", None, 100, &search, None)
        .await
        .unwrap();

    let album_id = AlbumId::new("ALB_1").unwrap();
    let album = AlbumDetail {
        album_id: album_id.clone(),
        title: "Synthetic Album".to_string(),
        artists: vec![],
        year: Some(2025),
        cover: None,
        tracks: vec![make_track("vid_cp", "Plastic Love")],
        audio_playlist_id: None,
        playback_indexes: Vec::new(),
    };
    storage
        .put_album_detail(&ctx, &album_id, &album, None)
        .await
        .unwrap();

    let artist_id = ArtistId::new("ART_1").unwrap();
    let artist = ArtistDetail {
        artist_id: artist_id.clone(),
        name: "Synthetic Artist".to_string(),
        description: None,
        cover: None,
        top_songs: vec![],
        albums: vec![],
    };
    storage
        .put_artist_detail(&ctx, &artist_id, &artist, None)
        .await
        .unwrap();

    let home = HomeFeed {
        personalized_supported: false,
        fallback_notice: Some("Using non-personalized fallback".to_string()),
        recent_tracks: vec![],
        pinned_playlists: vec![],
        freshness: Freshness::Fresh,
    };
    storage.put_home_feed(&ctx, &home, None).await.unwrap();
    storage
        .put_liked_songs(&ctx, None, 100, &search.songs, None)
        .await
        .unwrap();

    assert_eq!(
        storage
            .get_search_result(&scope, "en-US", "city pop", None, 100)
            .await
            .unwrap()
            .unwrap()
            .freshness,
        Freshness::Fresh
    );
    assert_eq!(
        storage
            .get_album_detail(&scope, "en-US", &album_id)
            .await
            .unwrap()
            .unwrap()
            .freshness,
        Freshness::Fresh
    );

    // Advance 16 minutes -> search (15m TTL) is Stale, album (1h TTL) is still Fresh.
    clock.advance(16 * 60 * 1000);
    let stale_search = storage
        .get_search_result(&scope, "en-US", "city pop", None, 100)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stale_search.freshness, Freshness::Stale);
    assert_eq!(stale_search.value.songs.freshness, Freshness::Stale);
    assert_eq!(
        storage
            .get_album_detail(&scope, "en-US", &album_id)
            .await
            .unwrap()
            .unwrap()
            .freshness,
        Freshness::Fresh
    );

    let other = AccountCacheContext {
        account_scope: AccountScope::new("scope-other-rating").unwrap(),
        account_epoch: AccountEpoch::new("epoch-other-rating").unwrap(),
        ..ctx.clone()
    };
    let other_songs = PagedResult {
        account_scope: other.account_scope.clone(),
        ..search.songs.clone()
    };
    storage
        .put_liked_songs(&other, None, 100, &other_songs, None)
        .await
        .unwrap();
    storage.invalidate_rating_cache(&scope).await.unwrap();
    assert!(
        storage
            .get_search_result(&scope, "en-US", "city pop", None, 100)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        storage
            .get_liked_songs(&scope, "en-US", None, 100)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        storage
            .get_liked_songs(&other.account_scope, "en-US", None, 100)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        storage
            .get_album_detail(&scope, "en-US", &album_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        storage
            .get_artist_detail(&scope, "en-US", &artist_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        storage
            .get_home_feed(&scope, "en-US")
            .await
            .unwrap()
            .is_none()
    );

    // Lyrics hit TTL (30 days) vs negative NotFound TTL (24h) + user offset separation.
    let vid_hit = VideoId::new("vid_lyrics_hit").unwrap();
    let meta_hit = LyricsMatchMetadata {
        video_id: vid_hit.clone(),
        title: "Track One".to_string(),
        artist: "Artist One".to_string(),
        album: None,
        duration_ms: Some(180_000),
        version_tag: Some("studio".to_string()),
    };
    let hit_res = LyricsResult {
        video_id: vid_hit.clone(),
        kind: LyricsKind::Synced,
        source: "lrclib".to_string(),
        language: Some("en".to_string()),
        match_basis: Some("exact".to_string()),
        fetched_at_ms: clock.now_ms(),
        file_offset_ms: 500,
        user_offset_ms: 0,
        synced_lines: vec![LyricsLine {
            time_ms: 1000,
            text: "Hello line".to_string(),
        }],
        plain_text: None,
        error: None,
    };
    storage
        .put_lyrics(&meta_hit, "lrclib", &hit_res, None)
        .await
        .unwrap();

    storage
        .set_lyrics_user_offset(&vid_hit, -350)
        .await
        .unwrap();
    let loaded_hit = storage
        .get_lyrics(&meta_hit, "lrclib")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(loaded_hit.freshness, Freshness::Fresh);
    assert_eq!(loaded_hit.value.file_offset_ms, 500);
    assert_eq!(loaded_hit.value.user_offset_ms, -350);

    // Changing version_tag for the same video_id uses a separate cache key!
    let mut meta_live = meta_hit.clone();
    meta_live.version_tag = Some("live".to_string());
    assert!(
        storage
            .get_lyrics(&meta_live, "lrclib")
            .await
            .unwrap()
            .is_none()
    );

    // Resetting user offset restores 0 without touching file_offset_ms.
    storage.reset_lyrics_user_offset(&vid_hit).await.unwrap();
    let after_reset = storage
        .get_lyrics(&meta_hit, "lrclib")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after_reset.value.file_offset_ms, 500);
    assert_eq!(after_reset.value.user_offset_ms, 0);

    let vid_miss = VideoId::new("vid_lyrics_miss").unwrap();
    let meta_miss = LyricsMatchMetadata {
        video_id: vid_miss.clone(),
        title: "Unknown".to_string(),
        artist: "Unknown".to_string(),
        album: None,
        duration_ms: None,
        version_tag: None,
    };
    let miss_res = LyricsResult {
        video_id: vid_miss,
        kind: LyricsKind::NotFound,
        source: "lrclib".to_string(),
        language: None,
        match_basis: Some("not found".to_string()),
        fetched_at_ms: clock.now_ms(),
        file_offset_ms: 0,
        user_offset_ms: 0,
        synced_lines: vec![],
        plain_text: None,
        error: None,
    };
    storage
        .put_lyrics(&meta_miss, "lrclib", &miss_res, None)
        .await
        .unwrap();

    // Advance 25 hours -> NotFound (24h TTL) becomes Stale, Synced (30d TTL) stays Fresh.
    clock.advance(25 * 60 * 60 * 1000);
    assert_eq!(
        storage
            .get_lyrics(&meta_miss, "lrclib")
            .await
            .unwrap()
            .unwrap()
            .freshness,
        Freshness::Stale
    );
    assert_eq!(
        storage
            .get_lyrics(&meta_hit, "lrclib")
            .await
            .unwrap()
            .unwrap()
            .freshness,
        Freshness::Fresh
    );

    // Advance another 30 days -> Synced becomes Stale.
    clock.advance(30 * 24 * 60 * 60 * 1000);
    assert_eq!(
        storage
            .get_lyrics(&meta_hit, "lrclib")
            .await
            .unwrap()
            .unwrap()
            .freshness,
        Freshness::Stale
    );
}

#[tokio::test]
async fn clear_cache_preserves_settings_and_profile_session() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/keep.png"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(make_solid_png(
            16,
            16,
            [220, 40, 60, 255],
        )))
        .mount(&server)
        .await;
    let tmp = TempDir::new().unwrap();
    let paths = StoragePaths::for_root(tmp.path());
    let clock = Arc::new(MockClock::new(1_700_000_000_000));
    let storage = SqliteStorage::open(
        paths.clone(),
        clock.clone(),
        CoverCacheConfig::production().with_test_endpoint(*server.address()),
    )
    .unwrap();

    let scope = AccountScope::new("scope-keep").unwrap();
    let epoch = AccountEpoch::new("epoch-keep").unwrap();
    storage
        .register_account_epoch(&scope, &epoch)
        .await
        .unwrap();

    let settings = AppSettings {
        theme: ThemeMode::System,
        ..AppSettings::default()
    };
    storage.save_settings(settings.clone()).await.unwrap();

    let ctx = AccountCacheContext {
        account_scope: scope.clone(),
        account_epoch: epoch.clone(),
        locale: "en-US".to_string(),
        provider_version: "ytm-mock-1.0".to_string(),
    };
    let page = PagedResult {
        items: vec![],
        next_cursor: None,
        freshness: Freshness::Fresh,
        fetched_at_ms: clock.now_ms(),
        account_scope: scope.clone(),
        provider_version: "ytm-mock-1.0".to_string(),
    };
    storage
        .put_library_playlists(&ctx, None, 100, &page, None)
        .await
        .unwrap();
    storage
        .record_recent_playback(
            &scope,
            &epoch,
            RecentPlaybackRecord {
                track: make_track("vid_keep", "Keep Recent"),
                playback_ref: None,
                position_ms: 5_000,
                duration_ms: Some(120_000),
                recorded_at_ms: clock.now_ms(),
                source_context: Some("paused-shutdown".to_string()),
            },
        )
        .await
        .unwrap();

    let cover = storage
        .fetch_and_cache_cover(&format!("{}/keep.png", server.uri()))
        .await
        .unwrap();
    assert!(cover.file_path.exists());

    let other_ctx = AccountCacheContext {
        account_scope: AccountScope::new("scope-other").unwrap(),
        account_epoch: AccountEpoch::new("epoch-other").unwrap(),
        ..ctx.clone()
    };
    storage
        .register_account_epoch(&other_ctx.account_scope, &other_ctx.account_epoch)
        .await
        .unwrap();
    let other_page = PagedResult {
        account_scope: other_ctx.account_scope.clone(),
        ..page.clone()
    };
    storage
        .put_library_playlists(&other_ctx, None, 100, &other_page, None)
        .await
        .unwrap();
    let video_id = VideoId::new("vid_shared_lyrics").unwrap();
    storage
        .set_lyrics_user_offset(&video_id, -200)
        .await
        .unwrap();
    for (key, kind) in [("hit", LyricsKind::Plain), ("miss", LyricsKind::NotFound)] {
        let result = LyricsResult {
            video_id: video_id.clone(),
            kind,
            source: "lrclib".into(),
            language: None,
            match_basis: None,
            fetched_at_ms: clock.now_ms(),
            file_offset_ms: 0,
            user_offset_ms: 0,
            synced_lines: vec![],
            plain_text: (kind == LyricsKind::Plain).then(|| "Synthetic line".into()),
            error: None,
        };
        storage
            .put_lyrics_by_key(key, "lrclib", &result, None)
            .await
            .unwrap();
        assert!(storage.get_lyrics_by_key(key).await.unwrap().is_some());
    }

    // Clear cache for scope.
    storage.clear_cache(Some(scope.clone())).await.unwrap();

    // Cover file and cached playlists are cleared.
    assert!(!cover.file_path.exists());
    assert!(
        storage
            .get_library_playlists(&scope, "en-US", None, 100)
            .await
            .unwrap()
            .is_none()
    );

    for key in ["hit", "miss"] {
        assert!(storage.get_lyrics_by_key(key).await.unwrap().is_none());
    }
    assert_eq!(
        storage.get_lyrics_user_offset(&video_id).await.unwrap(),
        -200
    );
    assert_eq!(
        storage
            .get_library_playlists(&other_ctx.account_scope, "en-US", None, 100)
            .await
            .unwrap()
            .unwrap()
            .value,
        other_page
    );

    // Settings, recent playback restore state, and active account epoch (not logged out!) are preserved!
    assert_eq!(storage.load_settings().await.unwrap().unwrap(), settings);
    assert!(storage.get_latest_playback(&scope).await.unwrap().is_some());
    storage
        .put_library_playlists(&ctx, None, 100, &page, None)
        .await
        .expect("clearing cache must not invalidate active account epoch");
}

#[tokio::test]
async fn covers_follow_relative_redirects_up_to_the_configured_limit() {
    let server = MockServer::start().await;
    for (from, to) in [
        ("/extra", "/start"),
        ("/start", "next"),
        ("/next", "/cover.png"),
    ] {
        Mock::given(method("GET"))
            .and(path(from))
            .respond_with(ResponseTemplate::new(302).insert_header("location", to))
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/cover.png"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(make_solid_png(
            8,
            8,
            [20, 40, 60, 255],
        )))
        .expect(1)
        .mount(&server)
        .await;
    let tmp = TempDir::new().unwrap();
    let config = CoverCacheConfig {
        max_redirects: 2,
        ..CoverCacheConfig::production().with_test_endpoint(*server.address())
    };
    let storage = SqliteStorage::open(
        StoragePaths::for_root(tmp.path()),
        Arc::new(MockClock::new(0)),
        config,
    )
    .unwrap();
    let cover = storage
        .fetch_and_cache_cover(&format!("{}/start", server.uri()))
        .await
        .unwrap();
    assert!(cover.file_path.is_file());
    let err = storage
        .fetch_and_cache_cover(&format!("{}/extra", server.uri()))
        .await
        .unwrap_err();
    assert_eq!(err.code, QueryErrorCode::InvalidPayload);
}

#[tokio::test]
async fn ssrf_redirect_blocking_and_url_allowlist_via_wiremock() {
    let allowed_server = MockServer::start().await;
    let forbidden_target = MockServer::start().await;

    // Forbidden target MUST receive ZERO requests across all redirect tests.
    Mock::given(method("GET"))
        .and(path("/secret"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(make_solid_png(
            8,
            8,
            [0, 0, 0, 255],
        )))
        .expect(0)
        .mount(&forbidden_target)
        .await;

    let forbidden_url = format!("{}/secret", forbidden_target.uri());
    let redirect_cases = [
        ("/redir-local", forbidden_url.as_str()),
        ("/redir-meta", "http://169.254.169.254/latest/meta-data/"),
        ("/redir-rfc1918", "https://10.0.0.1/cover.png"),
        ("/redir-v6-loop", "https://[::1]/cover.png"),
        ("/redir-v6-ula", "https://[fd12:3456::1]/cover.png"),
        ("/redir-dns-local", "https://localhost/cover.png"),
        ("/redir-file", "file:///etc/passwd"),
        ("/redir-evil-host", "https://evil.example.com/cover.png"),
    ];

    for (route, target) in redirect_cases {
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(ResponseTemplate::new(302).insert_header("location", target))
            .mount(&allowed_server)
            .await;
    }

    let tmp = TempDir::new().unwrap();
    let paths = StoragePaths::for_root(tmp.path());
    let clock = Arc::new(MockClock::new(1_700_000_000_000));
    let mut cover_cfg =
        CoverCacheConfig::production().with_test_endpoint(*allowed_server.address());
    cover_cfg.allowed_hosts.push("localhost".into());
    let storage = SqliteStorage::open(paths, clock, cover_cfg).unwrap();

    for (route, _) in redirect_cases {
        let url = format!(
            "{}{}?key=synthetic-cover-material",
            allowed_server.uri(),
            route
        );
        let err = storage
            .fetch_and_cache_cover(&url)
            .await
            .expect_err(&format!("redirect {route} must be blocked"));
        assert_eq!(
            err.code,
            QueryErrorCode::InvalidPayload,
            "route={route} err={err:?}"
        );
        assert!(!err.safe_message.contains("synthetic-cover-material"));
    }

    // Production allowlist static checks.
    let prod_tmp = TempDir::new().unwrap();
    let prod_storage = SqliteStorage::open(
        StoragePaths::for_root(prod_tmp.path()),
        Arc::new(MockClock::new(1_700_000_000_000)),
        CoverCacheConfig::production(),
    )
    .unwrap();
    assert!(
        prod_storage
            .covers()
            .validate_url_static("https://lh3.googleusercontent.com/abc=w544-h544")
            .is_ok()
    );
    assert!(
        prod_storage
            .covers()
            .validate_url_static("https://i.ytimg.com/vi/abc/maxresdefault.jpg")
            .is_ok()
    );
    assert!(
        prod_storage
            .covers()
            .validate_url_static("http://lh3.googleusercontent.com/abc")
            .is_err()
    );
    assert!(
        prod_storage
            .covers()
            .validate_url_static("https://127.0.0.1/abc.png")
            .is_err()
    );
    assert!(
        prod_storage
            .covers()
            .validate_url_static("https://evil.example.com/abc.png")
            .is_err()
    );
    assert!(
        prod_storage
            .covers()
            .validate_url_static("file:///etc/passwd")
            .is_err()
    );

    // Direct IP classification checks.
    assert!(!is_public_unicast_ip(IpAddr::V4(Ipv4Addr::new(
        127, 0, 0, 1
    ))));
    assert!(!is_public_unicast_ip(IpAddr::V4(Ipv4Addr::new(
        10, 1, 2, 3
    ))));
    assert!(!is_public_unicast_ip(IpAddr::V4(Ipv4Addr::new(
        172, 16, 0, 1
    ))));
    assert!(!is_public_unicast_ip(IpAddr::V4(Ipv4Addr::new(
        192, 168, 1, 1
    ))));
    assert!(!is_public_unicast_ip(IpAddr::V4(Ipv4Addr::new(
        169, 254, 169, 254
    ))));
    assert!(!is_public_unicast_ip(IpAddr::V4(Ipv4Addr::new(
        100, 64, 0, 1
    ))));
    assert!(!is_public_unicast_ip(IpAddr::V6(Ipv6Addr::LOCALHOST)));
    assert!(!is_public_unicast_ip(IpAddr::V6(
        "fe80::1".parse().unwrap()
    )));
    assert!(!is_public_unicast_ip(IpAddr::V6(
        "fd00::1".parse().unwrap()
    )));
    assert!(!is_public_unicast_ip(IpAddr::V6(
        "::ffff:127.0.0.1".parse().unwrap()
    )));
    assert!(is_public_unicast_ip(IpAddr::V4(Ipv4Addr::new(
        142, 250, 190, 46
    ))));
}

#[tokio::test]
async fn cover_oversized_bad_image_dominant_color_lru_and_traversal_rejection() {
    let server = MockServer::start().await;

    let red_png = make_solid_png(32, 32, [230, 35, 45, 255]);
    let blue_webp = make_solid_webp(32, 32, [35, 95, 235, 255]);
    let gray_png = make_solid_png(32, 32, [120, 120, 120, 255]);
    let too_wide_png = make_solid_png(4097, 8, [10, 200, 10, 255]);
    let oversized_body = vec![0x89_u8; 4096];
    let bad_payload = b"<html><body>not an image</body></html>".to_vec();

    Mock::given(method("GET"))
        .and(path("/red.png"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(red_png.clone()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/blue.webp"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(blue_webp.clone()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/gray.png"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(gray_png.clone()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/wide.png"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(too_wide_png))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/oversized.png"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(oversized_body))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/bad.png"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(bad_payload))
        .mount(&server)
        .await;

    let tmp = TempDir::new().unwrap();
    let paths = StoragePaths::for_root(tmp.path());
    let clock = Arc::new(MockClock::new(1_700_000_000_000));

    // Set max_cache_bytes so any 2 of the 3 valid covers fit, forcing LRU eviction of 1 on the 3rd.
    let two_cover_budget =
        (red_png.len() + blue_webp.len()).max(red_png.len() + gray_png.len()) as u64 + 10;
    let cover_cfg = CoverCacheConfig::production()
        .with_test_endpoint(*server.address())
        .with_max_download_bytes(2048)
        .with_max_cache_bytes(two_cover_budget);
    let storage = SqliteStorage::open(paths.clone(), clock.clone(), cover_cfg).unwrap();

    // Reject oversized body (>2048), >4096px wide image, and bad image payload.
    assert_eq!(
        storage
            .fetch_and_cache_cover(&format!("{}/oversized.png", server.uri()))
            .await
            .unwrap_err()
            .code,
        QueryErrorCode::InvalidPayload
    );
    assert_eq!(
        storage
            .fetch_and_cache_cover(&format!("{}/wide.png", server.uri()))
            .await
            .unwrap_err()
            .code,
        QueryErrorCode::InvalidPayload
    );
    assert_eq!(
        storage
            .fetch_and_cache_cover(&format!("{}/bad.png", server.uri()))
            .await
            .unwrap_err()
            .code,
        QueryErrorCode::InvalidPayload
    );

    // Fetch red (t=0) and blue (t=1000).
    let red_info = storage
        .fetch_and_cache_cover(&format!("{}/red.png", server.uri()))
        .await
        .unwrap();
    assert_eq!(red_info.mime_type, "image/png");
    assert!(red_info.accent_oklch.starts_with("oklch("));
    assert_eq!(
        fs::metadata(&red_info.file_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    clock.advance(1_000);
    let blue_info = storage
        .fetch_and_cache_cover(&format!("{}/blue.webp", server.uri()))
        .await
        .unwrap();
    assert_eq!(blue_info.mime_type, "image/webp");
    assert_ne!(red_info.accent_oklch, blue_info.accent_oklch);

    // Access red at t=2000 so blue becomes the least-recently-used entry!
    clock.advance(1_000);
    let resolved_red = storage
        .resolve_asset(red_info.asset_id.clone())
        .await
        .unwrap();
    assert_eq!(resolved_red.mime_type.as_deref(), Some("image/png"));
    assert!(resolved_red.file_path.is_some());

    // Fetch gray at t=3000 -> exceeds budget, so blue (LRU) must be evicted while red & gray stay!
    clock.advance(1_000);
    let gray_info = storage
        .fetch_and_cache_cover(&format!("{}/gray.png", server.uri()))
        .await
        .unwrap();
    assert!(gray_info.accent_oklch.contains(" 0.000 "));

    assert!(
        red_info.file_path.exists(),
        "recently accessed red cover must remain"
    );
    assert!(
        gray_info.file_path.exists(),
        "newly inserted gray cover must remain"
    );
    assert!(
        !blue_info.file_path.exists(),
        "LRU blue cover must be evicted from disk"
    );
    assert!(storage.covers().total_cached_bytes().await.unwrap() <= two_cover_budget);
    // Eviction keeps the URL mapping: resolving the evicted asset fetches it again lazily.
    let refetched = storage
        .resolve_asset(blue_info.asset_id.clone())
        .await
        .unwrap();
    assert!(std::path::Path::new(refetched.file_path.as_deref().unwrap()).exists());
    assert!(storage.covers().total_cached_bytes().await.unwrap() <= two_cover_budget);

    // The now-playing cover is pinned: even as the least recently used entry it stays on disk,
    // because media controls may still publish its file.
    storage.covers().pin(Some(&gray_info.asset_id));
    clock.advance(1_000);
    storage
        .resolve_asset(blue_info.asset_id.clone())
        .await
        .unwrap();
    clock.advance(1_000);
    storage
        .resolve_asset(red_info.asset_id.clone())
        .await
        .unwrap();
    assert!(
        gray_info.file_path.exists(),
        "the pinned now-playing cover must survive capacity eviction"
    );
    assert!(
        !blue_info.file_path.exists(),
        "the unpinned least recently used cover is evicted instead"
    );
    storage.covers().pin(None);

    // Path traversal & symlink rejection in resolve_asset.
    assert_eq!(
        storage
            .resolve_asset(AssetId::new("../etc/passwd").unwrap())
            .await
            .unwrap_err()
            .code,
        QueryErrorCode::InvalidPayload
    );
    assert_eq!(
        storage
            .resolve_asset(AssetId::new("..\\secret.json").unwrap())
            .await
            .unwrap_err()
            .code,
        QueryErrorCode::InvalidPayload
    );

    // Create a symlink named like a valid SHA-256 hex ID and ensure resolve_asset rejects it if referenced.
    let fake_id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let secret_outside = tmp.path().join("outside_secret.png");
    fs::write(&secret_outside, &red_png).unwrap();
    let symlink_inside = paths.covers_dir().join(format!("{fake_id}.png"));
    symlink(&secret_outside, &symlink_inside).unwrap();

    // Even if an entry existed in DB pointing to a symlink, verify_asset_path rejects symlinks.
    {
        let conn = Connection::open(paths.database_file()).unwrap();
        conn.execute(
            "INSERT INTO cover_assets (asset_id, rel_path, mime_type, byte_size, width, height, accent_oklch, created_at_ms, last_accessed_ms, format_ext) VALUES (?1, ?2, 'image/png', 100, 32, 32, 'oklch(0.500 0.000 0.0)', 1, 1, 'png')",
            rusqlite::params![fake_id, format!("{fake_id}.png")],
        )
        .unwrap();
    }
    let sym_err = storage
        .resolve_asset(AssetId::new(fake_id).unwrap())
        .await
        .unwrap_err();
    assert_eq!(sym_err.code, QueryErrorCode::InvalidPayload);
}

#[tokio::test]
async fn cover_asset_id_is_url_derived_and_resolved_lazily_after_registration() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/art.png"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(make_solid_png(
            24,
            24,
            [30, 90, 200, 255],
        )))
        .expect(1)
        .mount(&server)
        .await;
    let tmp = TempDir::new().unwrap();
    let storage = SqliteStorage::open(
        StoragePaths::for_root(tmp.path()),
        Arc::new(MockClock::new(1_700_000_000_000)),
        CoverCacheConfig::production().with_test_endpoint(*server.address()),
    )
    .unwrap();

    let url = format!("{}/art.png", server.uri());
    let mut cover = ytm_protocol::CoverRef {
        asset_id: None,
        remote_url: Some(url.clone()),
        accent_oklch: None,
    };
    let mut rejected = ytm_protocol::CoverRef {
        asset_id: None,
        remote_url: Some("http://169.254.169.254/latest".to_string()),
        accent_oklch: None,
    };
    storage
        .register_cover_urls(vec![&mut cover, &mut rejected])
        .await
        .unwrap();
    let asset_id = cover.asset_id.clone().expect("allowed URL gets an id");
    assert!(
        rejected.asset_id.is_none(),
        "policy-rejected URL gets no id"
    );
    assert!(ytm_storage::is_valid_sha256_hex(asset_id.as_str()));
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        0,
        "no eager fetch"
    );

    // First resolve downloads; the same URL always maps to the same id; second resolve is cached.
    let resolved = storage.resolve_asset(asset_id.clone()).await.unwrap();
    assert!(std::path::Path::new(resolved.file_path.as_deref().unwrap()).exists());
    let cached = storage.fetch_and_cache_cover(&url).await.unwrap();
    assert_eq!(cached.asset_id, asset_id);
    storage.resolve_asset(asset_id).await.unwrap();
}

#[test]
fn google_thumbnail_sizes_map_to_one_canonical_asset() {
    let tmp = TempDir::new().unwrap();
    let storage = SqliteStorage::open(
        StoragePaths::for_root(tmp.path()),
        Arc::new(MockClock::new(1)),
        CoverCacheConfig::production(),
    )
    .unwrap();
    let small = storage
        .covers()
        .asset_id_for_url("https://lh3.googleusercontent.com/abcDEF=w60-h60-l90-rj")
        .unwrap();
    let large = storage
        .covers()
        .asset_id_for_url("https://lh3.googleusercontent.com/abcDEF=w544-h544-l90-rj")
        .unwrap();
    assert_eq!(small.0, large.0);
    assert!(small.1.ends_with("=w544-h544-l90-rj"));
    assert!(
        storage
            .covers()
            .asset_id_for_url("https://evil.example/cover.png")
            .is_err()
    );
}

#[tokio::test]
async fn play_history_is_bounded_restorable_and_cleared_on_logout() {
    let tmp = TempDir::new().unwrap();
    let clock = Arc::new(MockClock::with_wall_offset(1, 1_700_000_000_000));
    let storage = SqliteStorage::open(
        StoragePaths::for_root(tmp.path()),
        clock.clone(),
        CoverCacheConfig::production(),
    )
    .unwrap();
    assert!(storage.last_session().await.unwrap().is_none());

    let scope = AccountScope::new("scope-hist").unwrap();
    let epoch = AccountEpoch::new("epoch-hist").unwrap();
    storage
        .register_account_epoch(&scope, &epoch)
        .await
        .unwrap();
    for i in 0..205 {
        clock.advance(10);
        storage
            .record_playback(
                scope.clone(),
                epoch.clone(),
                ytm_core::LastPlayed {
                    track: make_track(&format!("vid_{i}"), "Track"),
                    playback_ref: None,
                    position_ms: 0,
                },
            )
            .await
            .unwrap();
    }
    let all = storage.list_recent_playback(&scope, 1_000).await.unwrap();
    assert_eq!(all.len(), 200, "history is pruned to its bound");
    assert_eq!(all[0].track.video_id.as_str(), "vid_204");

    // Position updates apply only while the newest entry is still that track.
    storage
        .update_playback_position(
            scope.clone(),
            epoch.clone(),
            VideoId::new("vid_100").unwrap(),
            9_000,
        )
        .await
        .unwrap();
    storage
        .update_playback_position(
            scope.clone(),
            epoch.clone(),
            VideoId::new("vid_204").unwrap(),
            61_000,
        )
        .await
        .unwrap();
    let session = storage.last_session().await.unwrap().unwrap();
    assert_eq!(session.account_scope, scope);
    let last = session.last_played.unwrap();
    assert_eq!(last.track.video_id.as_str(), "vid_204");
    assert_eq!(last.position_ms, 61_000);

    storage
        .clear_account_data(Some(scope.clone()), None)
        .await
        .unwrap();
    assert!(storage.last_session().await.unwrap().is_none());
    assert!(
        storage
            .list_recent_playback(&scope, 10)
            .await
            .unwrap()
            .is_empty()
    );
}
