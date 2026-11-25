pub mod cover;
mod dj_store;
pub mod error;
mod local_history;
pub mod migrations;
mod moment_store;
mod operation_store;
pub mod paths;
pub mod repo;

use crate::error::StorageError;
use crate::migrations::{LATEST_SCHEMA_VERSION, open_and_migrate};
use crate::paths::write_atomic_0600;
use crate::repo::{
    LIBRARY_PLAYLIST_PAGE_KIND, LIKED_SONGS_PAGE_KIND, PLAYLIST_DETAIL_PAGE_KIND, SEARCH_PAGE_KIND,
    album_cache_key, apply_recent_searches_tx, artist_cache_key, catalog_page_cache_key,
    clear_account_data_tx, clear_pending_account_tx, get_catalog_entry_tx, get_lyrics_conn,
    get_lyrics_user_offset_conn, invalidate_account_epoch_tx, invalidate_library_playlist_views_tx,
    last_active_account_scope_conn, list_recent_playback_conn, patch_home_freshness,
    patch_paged_freshness, patch_playlist_detail_freshness, patch_search_freshness,
    playlist_detail_cache_key, put_catalog_entry_tx, put_library_playlists_tx, put_lyrics_tx,
    put_playlist_detail_tx, record_recent_playback_tx, register_account_epoch_tx,
    reset_lyrics_user_offset_tx, search_cache_key, set_lyrics_user_offset_tx,
    update_latest_playback_position_tx,
};
use rusqlite::{Connection, OpenFlags, params};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use ytm_core::ports::{
    BoxFuture, CachedCover, Clock, LastPlayed, LastSession, StoragePort, SystemClock,
};
use ytm_protocol::{
    AccountEpoch, AccountScope, AlbumDetail, AlbumId, AppSettings, ArtistDetail, ArtistId, AssetId,
    AssetResolvedPayload, HomeFeed, LikedSongsPage, LyricsMatchMetadata, LyricsResult,
    OwnerMomentConfig, PagedResult, PlaylistDetail, PlaylistId, PlaylistSummary, QueryError,
    RecentSearchesRequest, RecentSearchesSnapshot, SearchResult, VideoId,
};

pub use cover::{
    CachedCoverInfo, CoverCache, CoverCacheConfig, DEFAULT_ALLOWED_COVER_HOSTS,
    DEFAULT_COVER_CACHE_MAX_BYTES, DEFAULT_COVER_MAX_DOWNLOAD_BYTES, DEFAULT_COVER_MAX_HEIGHT,
    DEFAULT_COVER_MAX_WIDTH, extract_dominant_accent_oklch, is_public_unicast_ip,
    is_valid_sha256_hex, srgb_to_oklch,
};
pub use paths::{APP_XDG_DIR_NAME, StoragePaths};
pub use repo::{
    AccountCacheContext, CachedValue, DEFAULT_DETAIL_TTL_SECS, DEFAULT_LIBRARY_LIST_TTL_SECS,
    DEFAULT_LYRICS_HIT_TTL_SECS, DEFAULT_LYRICS_NEGATIVE_TTL_SECS, RecentPlaybackRecord,
    compute_lyrics_cache_key, evaluate_freshness,
};

pub enum LibraryMembershipCache<'a> {
    Song,
    Playlist(&'a PlaylistId),
}

/// Persistent SQLite + XDG filesystem storage adapter implementing `ytm_core::ports::StoragePort`
/// and exposing typed repositories for `ytm-data` and `ytm-lyrics`.
#[derive(Clone)]
pub struct SqliteStorage {
    paths: StoragePaths,
    conn: Arc<Mutex<Connection>>,
    journal: Arc<Mutex<Connection>>,
    clock: Arc<dyn Clock>,
    covers: CoverCache,
    backup_paths: Arc<Vec<PathBuf>>,
}

impl SqliteStorage {
    /// Opens storage at the standard XDG directories with the system clock and production cover policy.
    pub fn open_default() -> Result<Self, QueryError> {
        let paths = StoragePaths::from_xdg().map_err(QueryError::from)?;
        Self::open(
            paths,
            Arc::new(SystemClock::new()),
            CoverCacheConfig::production(),
        )
    }

    /// Opens storage with explicit paths, clock, and cover cache policy (recommended for tests).
    pub fn open(
        paths: StoragePaths,
        clock: Arc<dyn Clock>,
        cover_config: CoverCacheConfig,
    ) -> Result<Self, QueryError> {
        Self::open_at_schema_version(paths, clock, cover_config, LATEST_SCHEMA_VERSION)
    }

    /// Opens storage migrating only up to `target_max_version` for migration tests.
    pub fn open_at_schema_version(
        paths: StoragePaths,
        clock: Arc<dyn Clock>,
        cover_config: CoverCacheConfig,
        target_max_version: u32,
    ) -> Result<Self, QueryError> {
        let (conn, backups) = open_and_migrate(&paths, clock.as_ref(), target_max_version)
            .map_err(QueryError::from)?;
        paths::ensure_dir_0700(&paths.config_dir).map_err(QueryError::from)?;
        paths::ensure_dir_0700(&paths.cache_dir).map_err(QueryError::from)?;

        // Use full WAL durability for the pre-dispatch marker without slowing cache writes.
        let journal =
            Connection::open_with_flags(paths.database_file(), OpenFlags::SQLITE_OPEN_READ_WRITE)
                .map_err(StorageError::from)?;
        journal
            .busy_timeout(Duration::from_secs(5))
            .map_err(StorageError::from)?;
        journal
            .pragma_update(None, "synchronous", "FULL")
            .map_err(StorageError::from)?;
        let conn_arc = Arc::new(Mutex::new(conn));
        let covers = CoverCache::new(
            paths.covers_dir(),
            conn_arc.clone(),
            clock.clone(),
            cover_config,
        )
        .map_err(QueryError::from)?;

        Ok(Self {
            paths,
            conn: conn_arc,
            journal: Arc::new(Mutex::new(journal)),
            clock,
            covers,
            backup_paths: Arc::new(backups),
        })
    }

    pub fn paths(&self) -> &StoragePaths {
        &self.paths
    }

    pub fn covers(&self) -> &CoverCache {
        &self.covers
    }

    /// Paths of any online SQLite `.bak` backups created while opening/migrating this instance.
    pub fn created_backups(&self) -> &[PathBuf] {
        &self.backup_paths
    }

    pub fn current_schema_version(&self) -> Result<u32, QueryError> {
        let guard = self.conn.lock().map_err(|_| {
            StorageError::Storage("sqlite mutex poisoned".to_string()).into_query_error()
        })?;
        let ver: u32 = guard
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_version",
                [],
                |r| r.get(0),
            )
            .map_err(|e| StorageError::from(e).into_query_error())?;
        Ok(ver)
    }

    /// Registers an active `(account_scope, account_epoch)` pair, superseding earlier active epochs for that scope.
    pub async fn register_account_epoch(
        &self,
        account_scope: &AccountScope,
        account_epoch: &AccountEpoch,
    ) -> Result<(), QueryError> {
        let conn = self.conn.clone();
        let clock = self.clock.clone();
        let scope = account_scope.clone();
        let epoch = account_epoch.clone();
        run_blocking(move || {
            let now_ms = clock.wall_now_ms();
            let mut guard = lock_conn(&conn)?;
            let tx = guard.transaction()?;
            register_account_epoch_tx(&tx, &scope, &epoch, now_ms)?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// Marks an `account_epoch` as invalidated so late in-flight writes are rejected with `StaleContext`.
    pub async fn invalidate_account_epoch(
        &self,
        account_epoch: &AccountEpoch,
    ) -> Result<(), QueryError> {
        let conn = self.conn.clone();
        let clock = self.clock.clone();
        let epoch = account_epoch.clone();
        run_blocking(move || {
            let now_ms = clock.wall_now_ms();
            let mut guard = lock_conn(&conn)?;
            let tx = guard.transaction()?;
            invalidate_account_epoch_tx(&tx, &epoch, now_ms)?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// Logs out `account_scope`: invalidates `account_epoch` (if supplied) and deletes only that scope's private cached data.
    pub async fn logout_account(
        &self,
        account_scope: &AccountScope,
        account_epoch: Option<&AccountEpoch>,
    ) -> Result<(), QueryError> {
        let conn = self.conn.clone();
        let clock = self.clock.clone();
        let scope = account_scope.clone();
        let epoch = account_epoch.cloned();
        run_blocking(move || {
            let now_ms = clock.wall_now_ms();
            let mut guard = lock_conn(&conn)?;
            let tx = guard.transaction()?;
            if let Some(ep) = epoch.as_ref() {
                invalidate_account_epoch_tx(&tx, ep, now_ms)?;
            }
            clear_account_data_tx(&tx, &scope, now_ms)?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    // ---- Catalog Repository API (for T04 / T10 `ytm-data`) ----

    pub async fn put_library_playlists(
        &self,
        ctx: &AccountCacheContext,
        cursor: Option<&str>,
        limit: usize,
        page: &PagedResult<PlaylistSummary>,
        ttl_secs: Option<u64>,
    ) -> Result<(), QueryError> {
        let conn = self.conn.clone();
        let clock = self.clock.clone();
        let ctx = ctx.clone();
        let cursor_key = catalog_page_cache_key(cursor, limit);
        let page = page.clone();
        let ttl = ttl_secs.unwrap_or(repo::DEFAULT_LIBRARY_LIST_TTL_SECS);
        run_blocking(move || {
            let now_ms = clock.wall_now_ms();
            let mut guard = lock_conn(&conn)?;
            let tx = guard.transaction()?;
            put_library_playlists_tx(&tx, &ctx, &cursor_key, &page, ttl, now_ms)?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    pub async fn get_library_playlists(
        &self,
        account_scope: &AccountScope,
        locale: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<Option<CachedValue<PagedResult<PlaylistSummary>>>, QueryError> {
        let conn = self.conn.clone();
        let clock = self.clock.clone();
        let scope = account_scope.clone();
        let locale = locale.to_string();
        let cursor_key = catalog_page_cache_key(cursor, limit);
        run_blocking(move || {
            let now_ms = clock.wall_now_ms();
            let guard = lock_conn(&conn)?;
            let mut cached: Option<CachedValue<PagedResult<PlaylistSummary>>> =
                get_catalog_entry_tx(
                    &guard,
                    &scope,
                    &locale,
                    LIBRARY_PLAYLIST_PAGE_KIND,
                    &cursor_key,
                    now_ms,
                )?;
            if let Some(ref mut entry) = cached {
                patch_paged_freshness(&mut entry.value, entry.freshness);
            }
            Ok(cached)
        })
        .await
    }

    pub async fn put_playlist_detail(
        &self,
        ctx: &AccountCacheContext,
        playlist_id: &PlaylistId,
        cursor: Option<&str>,
        limit: usize,
        detail: &PlaylistDetail,
        ttl_secs: Option<u64>,
    ) -> Result<(), QueryError> {
        let conn = self.conn.clone();
        let clock = self.clock.clone();
        let ctx = ctx.clone();
        let playlist_id = playlist_id.clone();
        let cursor_key = catalog_page_cache_key(cursor, limit);
        let detail = detail.clone();
        let ttl = ttl_secs.unwrap_or(repo::DEFAULT_DETAIL_TTL_SECS);
        run_blocking(move || {
            let now_ms = clock.wall_now_ms();
            let mut guard = lock_conn(&conn)?;
            let tx = guard.transaction()?;
            put_playlist_detail_tx(&tx, &ctx, &playlist_id, &cursor_key, &detail, ttl, now_ms)?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    pub async fn get_playlist_detail(
        &self,
        account_scope: &AccountScope,
        locale: &str,
        playlist_id: &PlaylistId,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<Option<CachedValue<PlaylistDetail>>, QueryError> {
        let conn = self.conn.clone();
        let clock = self.clock.clone();
        let scope = account_scope.clone();
        let locale = locale.to_string();
        let cache_key =
            playlist_detail_cache_key(playlist_id, &catalog_page_cache_key(cursor, limit));
        run_blocking(move || {
            let now_ms = clock.wall_now_ms();
            let guard = lock_conn(&conn)?;
            let mut cached: Option<CachedValue<PlaylistDetail>> = get_catalog_entry_tx(
                &guard,
                &scope,
                &locale,
                PLAYLIST_DETAIL_PAGE_KIND,
                &cache_key,
                now_ms,
            )?;
            if let Some(ref mut entry) = cached {
                patch_playlist_detail_freshness(&mut entry.value, entry.freshness);
            }
            Ok(cached)
        })
        .await
    }

    /// Removes cached playlist pages after the caller has verified a remote write.
    pub async fn invalidate_playlist_cache(
        &self,
        account_scope: &AccountScope,
        playlist_id: &PlaylistId,
        library_changed: bool,
    ) -> Result<(), QueryError> {
        let conn = self.conn.clone();
        let scope = account_scope.clone();
        let playlist_id = playlist_id.clone();
        run_blocking(move || {
            let mut guard = lock_conn(&conn)?;
            let tx = guard.transaction()?;
            let scope = scope.as_str();
            let id = playlist_id.as_str();
            tx.execute(
                "DELETE FROM catalog_cache
                 WHERE account_scope = ?1 AND entry_kind = ?2 AND json_extract(cache_key, '$[0]') = ?3",
                params![scope, PLAYLIST_DETAIL_PAGE_KIND, id],
            )?;
            tx.execute(
                "DELETE FROM playlist_tracks WHERE account_scope = ?1 AND playlist_id = ?2",
                params![scope, id],
            )?;
            if library_changed {
                invalidate_library_playlist_views_tx(&tx, scope, id)?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// Removes durable account-library views after a verified membership change.
    pub async fn invalidate_library_membership(
        &self,
        account_scope: &AccountScope,
        kind: LibraryMembershipCache<'_>,
    ) -> Result<(), QueryError> {
        // Saved song and artist windows are fetched directly from the provider.
        let LibraryMembershipCache::Playlist(playlist_id) = kind else {
            return Ok(());
        };
        let conn = self.conn.clone();
        let scope = account_scope.clone();
        let playlist_id = playlist_id.clone();
        run_blocking(move || {
            let mut guard = lock_conn(&conn)?;
            let tx = guard.transaction()?;
            invalidate_library_playlist_views_tx(&tx, scope.as_str(), playlist_id.as_str())?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    pub async fn put_search_result(
        &self,
        ctx: &AccountCacheContext,
        query: &str,
        cursor: Option<&str>,
        limit: usize,
        result: &SearchResult,
        ttl_secs: Option<u64>,
    ) -> Result<(), QueryError> {
        let conn = self.conn.clone();
        let clock = self.clock.clone();
        let ctx = ctx.clone();
        let key = search_cache_key(query, cursor, limit);
        let result = result.clone();
        let ttl = ttl_secs.unwrap_or(repo::DEFAULT_LIBRARY_LIST_TTL_SECS);
        run_blocking(move || {
            let now_ms = clock.wall_now_ms();
            let mut guard = lock_conn(&conn)?;
            let tx = guard.transaction()?;
            put_catalog_entry_tx(&tx, &ctx, SEARCH_PAGE_KIND, &key, &result, ttl, now_ms)?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    pub async fn get_search_result(
        &self,
        account_scope: &AccountScope,
        locale: &str,
        query: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<Option<CachedValue<SearchResult>>, QueryError> {
        let conn = self.conn.clone();
        let clock = self.clock.clone();
        let scope = account_scope.clone();
        let locale = locale.to_string();
        let key = search_cache_key(query, cursor, limit);
        run_blocking(move || {
            let now_ms = clock.wall_now_ms();
            let guard = lock_conn(&conn)?;
            let mut cached: Option<CachedValue<SearchResult>> =
                get_catalog_entry_tx(&guard, &scope, &locale, SEARCH_PAGE_KIND, &key, now_ms)?;
            if let Some(ref mut entry) = cached {
                patch_search_freshness(&mut entry.value, entry.freshness);
            }
            Ok(cached)
        })
        .await
    }

    pub async fn put_album_detail(
        &self,
        ctx: &AccountCacheContext,
        album_id: &AlbumId,
        detail: &AlbumDetail,
        ttl_secs: Option<u64>,
    ) -> Result<(), QueryError> {
        let conn = self.conn.clone();
        let clock = self.clock.clone();
        let ctx = ctx.clone();
        let key = album_cache_key(album_id);
        let detail = detail.clone();
        let ttl = ttl_secs.unwrap_or(repo::DEFAULT_DETAIL_TTL_SECS);
        run_blocking(move || {
            let now_ms = clock.wall_now_ms();
            let mut guard = lock_conn(&conn)?;
            let tx = guard.transaction()?;
            put_catalog_entry_tx(&tx, &ctx, "album_detail", &key, &detail, ttl, now_ms)?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    pub async fn get_album_detail(
        &self,
        account_scope: &AccountScope,
        locale: &str,
        album_id: &AlbumId,
    ) -> Result<Option<CachedValue<AlbumDetail>>, QueryError> {
        let conn = self.conn.clone();
        let clock = self.clock.clone();
        let scope = account_scope.clone();
        let locale = locale.to_string();
        let key = album_cache_key(album_id);
        run_blocking(move || {
            let now_ms = clock.wall_now_ms();
            let guard = lock_conn(&conn)?;
            get_catalog_entry_tx(&guard, &scope, &locale, "album_detail", &key, now_ms)
        })
        .await
    }

    pub async fn put_artist_detail(
        &self,
        ctx: &AccountCacheContext,
        artist_id: &ArtistId,
        detail: &ArtistDetail,
        ttl_secs: Option<u64>,
    ) -> Result<(), QueryError> {
        let conn = self.conn.clone();
        let clock = self.clock.clone();
        let ctx = ctx.clone();
        let key = artist_cache_key(artist_id);
        let detail = detail.clone();
        let ttl = ttl_secs.unwrap_or(repo::DEFAULT_DETAIL_TTL_SECS);
        run_blocking(move || {
            let now_ms = clock.wall_now_ms();
            let mut guard = lock_conn(&conn)?;
            let tx = guard.transaction()?;
            put_catalog_entry_tx(&tx, &ctx, "artist_detail", &key, &detail, ttl, now_ms)?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    pub async fn get_artist_detail(
        &self,
        account_scope: &AccountScope,
        locale: &str,
        artist_id: &ArtistId,
    ) -> Result<Option<CachedValue<ArtistDetail>>, QueryError> {
        let conn = self.conn.clone();
        let clock = self.clock.clone();
        let scope = account_scope.clone();
        let locale = locale.to_string();
        let key = artist_cache_key(artist_id);
        run_blocking(move || {
            let now_ms = clock.wall_now_ms();
            let guard = lock_conn(&conn)?;
            get_catalog_entry_tx(&guard, &scope, &locale, "artist_detail", &key, now_ms)
        })
        .await
    }

    pub async fn put_liked_songs(
        &self,
        ctx: &AccountCacheContext,
        cursor: Option<&str>,
        limit: usize,
        page: &LikedSongsPage,
        ttl_secs: Option<u64>,
    ) -> Result<(), QueryError> {
        let conn = self.conn.clone();
        let clock = self.clock.clone();
        let ctx = ctx.clone();
        let key = catalog_page_cache_key(cursor, limit);
        let page = page.clone();
        let ttl = ttl_secs.unwrap_or(repo::DEFAULT_LIBRARY_LIST_TTL_SECS);
        run_blocking(move || {
            let now_ms = clock.wall_now_ms();
            let mut guard = lock_conn(&conn)?;
            let tx = guard.transaction()?;
            put_catalog_entry_tx(&tx, &ctx, LIKED_SONGS_PAGE_KIND, &key, &page, ttl, now_ms)?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    pub async fn get_liked_songs(
        &self,
        account_scope: &AccountScope,
        locale: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<Option<CachedValue<LikedSongsPage>>, QueryError> {
        let conn = self.conn.clone();
        let clock = self.clock.clone();
        let scope = account_scope.clone();
        let locale = locale.to_string();
        let key = catalog_page_cache_key(cursor, limit);
        run_blocking(move || {
            let now_ms = clock.wall_now_ms();
            let guard = lock_conn(&conn)?;
            let mut cached: Option<CachedValue<LikedSongsPage>> =
                get_catalog_entry_tx(&guard, &scope, &locale, LIKED_SONGS_PAGE_KIND, &key, now_ms)?;
            if let Some(ref mut entry) = cached {
                patch_paged_freshness(&mut entry.value.page, entry.freshness);
            }
            Ok(cached)
        })
        .await
    }

    /// Drops current-account read models that can embed a stale track rating.
    pub async fn invalidate_rating_cache(
        &self,
        account_scope: &AccountScope,
    ) -> Result<(), QueryError> {
        let conn = self.conn.clone();
        let scope = account_scope.clone();
        run_blocking(move || {
            let mut guard = lock_conn(&conn)?;
            let tx = guard.transaction()?;
            tx.execute(
                "DELETE FROM catalog_cache WHERE account_scope = ?1
                 AND entry_kind IN (?2, ?3, ?4, 'album_detail', 'artist_detail', 'home_feed')",
                params![
                    scope.as_str(),
                    SEARCH_PAGE_KIND,
                    LIKED_SONGS_PAGE_KIND,
                    PLAYLIST_DETAIL_PAGE_KIND
                ],
            )?;
            tx.execute(
                "DELETE FROM playlist_tracks WHERE account_scope = ?1",
                [scope.as_str()],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    pub async fn put_home_feed(
        &self,
        ctx: &AccountCacheContext,
        feed: &HomeFeed,
        ttl_secs: Option<u64>,
    ) -> Result<(), QueryError> {
        let conn = self.conn.clone();
        let clock = self.clock.clone();
        let ctx = ctx.clone();
        let feed = feed.clone();
        let ttl = ttl_secs.unwrap_or(repo::DEFAULT_LIBRARY_LIST_TTL_SECS);
        run_blocking(move || {
            let now_ms = clock.wall_now_ms();
            let mut guard = lock_conn(&conn)?;
            let tx = guard.transaction()?;
            put_catalog_entry_tx(&tx, &ctx, "home_feed", "default", &feed, ttl, now_ms)?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    pub async fn get_home_feed(
        &self,
        account_scope: &AccountScope,
        locale: &str,
    ) -> Result<Option<CachedValue<HomeFeed>>, QueryError> {
        let conn = self.conn.clone();
        let clock = self.clock.clone();
        let scope = account_scope.clone();
        let locale = locale.to_string();
        run_blocking(move || {
            let now_ms = clock.wall_now_ms();
            let guard = lock_conn(&conn)?;
            let mut cached: Option<CachedValue<HomeFeed>> =
                get_catalog_entry_tx(&guard, &scope, &locale, "home_feed", "default", now_ms)?;
            if let Some(ref mut entry) = cached {
                patch_home_freshness(&mut entry.value, entry.freshness);
            }
            Ok(cached)
        })
        .await
    }

    pub async fn record_recent_playback(
        &self,
        account_scope: &AccountScope,
        account_epoch: &AccountEpoch,
        entry: RecentPlaybackRecord,
    ) -> Result<(), QueryError> {
        let conn = self.conn.clone();
        let clock = self.clock.clone();
        let scope = account_scope.clone();
        let epoch = account_epoch.clone();
        run_blocking(move || {
            let now_ms = clock.wall_now_ms();
            let mut guard = lock_conn(&conn)?;
            let tx = guard.transaction()?;
            record_recent_playback_tx(&tx, &scope, &epoch, &entry, now_ms)?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    pub async fn get_latest_playback(
        &self,
        account_scope: &AccountScope,
    ) -> Result<Option<RecentPlaybackRecord>, QueryError> {
        let mut items = self.list_recent_playback(account_scope, 1).await?;
        Ok(items.pop())
    }

    pub async fn list_recent_playback(
        &self,
        account_scope: &AccountScope,
        limit: usize,
    ) -> Result<Vec<RecentPlaybackRecord>, QueryError> {
        let conn = self.conn.clone();
        let scope = account_scope.clone();
        run_blocking(move || {
            let guard = lock_conn(&conn)?;
            list_recent_playback_conn(&guard, &scope, limit)
        })
        .await
    }

    // ---- Lyrics Repository API (for T15 `ytm-lyrics`) ----

    pub async fn put_lyrics(
        &self,
        meta: &LyricsMatchMetadata,
        provider: &str,
        result: &LyricsResult,
        ttl_secs: Option<u64>,
    ) -> Result<(), QueryError> {
        self.put_lyrics_by_key(
            &repo::lyrics_metadata_key(meta, provider),
            provider,
            result,
            ttl_secs,
        )
        .await
    }

    pub async fn put_lyrics_by_key(
        &self,
        cache_key: &str,
        provider: &str,
        result: &LyricsResult,
        ttl_secs: Option<u64>,
    ) -> Result<(), QueryError> {
        let conn = self.conn.clone();
        let clock = self.clock.clone();
        let cache_key = cache_key.to_owned();
        let provider = provider.to_string();
        let result = result.clone();
        run_blocking(move || {
            let now_ms = clock.wall_now_ms();
            let mut guard = lock_conn(&conn)?;
            let tx = guard.transaction()?;
            put_lyrics_tx(&tx, &cache_key, &provider, &result, ttl_secs, now_ms)?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    pub async fn get_lyrics(
        &self,
        meta: &LyricsMatchMetadata,
        provider: &str,
    ) -> Result<Option<CachedValue<LyricsResult>>, QueryError> {
        self.get_lyrics_by_key(&repo::lyrics_metadata_key(meta, provider))
            .await
    }

    pub async fn get_lyrics_by_key(
        &self,
        cache_key: &str,
    ) -> Result<Option<CachedValue<LyricsResult>>, QueryError> {
        let conn = self.conn.clone();
        let clock = self.clock.clone();
        let cache_key = cache_key.to_owned();
        run_blocking(move || {
            let now_ms = clock.wall_now_ms();
            let guard = lock_conn(&conn)?;
            get_lyrics_conn(&guard, &cache_key, now_ms)
        })
        .await
    }

    pub async fn get_lyrics_user_offset(&self, video_id: &VideoId) -> Result<i64, QueryError> {
        let conn = self.conn.clone();
        let video_id = video_id.clone();
        run_blocking(move || {
            let guard = lock_conn(&conn)?;
            get_lyrics_user_offset_conn(&guard, &video_id)
        })
        .await
    }

    pub async fn set_lyrics_user_offset(
        &self,
        video_id: &VideoId,
        offset_ms: i64,
    ) -> Result<(), QueryError> {
        let conn = self.conn.clone();
        let clock = self.clock.clone();
        let video_id = video_id.clone();
        run_blocking(move || {
            let now_ms = clock.wall_now_ms();
            let mut guard = lock_conn(&conn)?;
            let tx = guard.transaction()?;
            set_lyrics_user_offset_tx(&tx, &video_id, offset_ms, now_ms)?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    pub async fn reset_lyrics_user_offset(&self, video_id: &VideoId) -> Result<(), QueryError> {
        let conn = self.conn.clone();
        let video_id = video_id.clone();
        run_blocking(move || {
            let mut guard = lock_conn(&conn)?;
            let tx = guard.transaction()?;
            reset_lyrics_user_offset_tx(&tx, &video_id)?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    // ---- Cover Cache Convenience API ----

    /// Assigns URL-derived asset ids to covers and registers them for lazy fetching.
    pub async fn register_cover_urls(
        &self,
        covers: Vec<&mut ytm_protocol::CoverRef>,
    ) -> Result<(), QueryError> {
        let mut entries = Vec::new();
        for cover in covers {
            let Some(url) = cover.remote_url.as_deref() else {
                continue;
            };
            match self.covers.asset_id_for_url(url) {
                Ok((asset_id, canonical)) => {
                    cover.asset_id = Some(asset_id.clone());
                    entries.push((asset_id, canonical));
                }
                Err(err) => tracing::debug!(error = %err, "cover URL rejected by policy"),
            }
        }
        self.covers
            .register_urls(entries)
            .await
            .map_err(QueryError::from)
    }

    pub async fn fetch_and_cache_cover(&self, url: &str) -> Result<CachedCoverInfo, QueryError> {
        self.covers
            .fetch_and_cache(url)
            .await
            .map_err(QueryError::from)
    }
}

impl StoragePort for SqliteStorage {
    fn load_settings(&self) -> BoxFuture<'_, Result<Option<AppSettings>, QueryError>> {
        let path = self.paths.settings_file();
        Box::pin(async move {
            run_blocking(move || {
                if !path.exists() {
                    return Ok(None);
                }
                let raw = fs::read_to_string(&path)?;
                let parsed: AppSettings = serde_json::from_str(&raw)?;
                Ok(Some(parsed))
            })
            .await
        })
    }

    fn save_settings(&self, settings: AppSettings) -> BoxFuture<'_, Result<(), QueryError>> {
        let path = self.paths.settings_file();
        Box::pin(async move {
            run_blocking(move || {
                let json = serde_json::to_vec_pretty(&settings)?;
                write_atomic_0600(&path, &json)?;
                Ok(())
            })
            .await
        })
    }

    fn load_moment_config(&self) -> BoxFuture<'_, Result<OwnerMomentConfig, QueryError>> {
        let path = self.paths.moment_config_file();
        Box::pin(async move {
            run_blocking(move || {
                if !path.exists() {
                    return Ok(OwnerMomentConfig::default());
                }
                let raw = fs::read_to_string(&path)?;
                let parsed: OwnerMomentConfig = serde_json::from_str(&raw)?;
                Ok(parsed)
            })
            .await
        })
    }

    fn save_moment_config(
        &self,
        config: OwnerMomentConfig,
    ) -> BoxFuture<'_, Result<(), QueryError>> {
        let path = self.paths.moment_config_file();
        Box::pin(async move {
            run_blocking(move || {
                let json = serde_json::to_vec_pretty(&config)?;
                write_atomic_0600(&path, &json)?;
                Ok(())
            })
            .await
        })
    }

    fn clear_cache(
        &self,
        account_scope: Option<AccountScope>,
    ) -> BoxFuture<'_, Result<(), QueryError>> {
        let conn = self.conn.clone();
        let covers = self.covers.clone();
        Box::pin(async move {
            covers.clear_all().await.map_err(QueryError::from)?;
            run_blocking(move || {
                let mut guard = lock_conn(&conn)?;
                let tx = guard.transaction()?;
                match account_scope {
                    Some(scope) => {
                        let s = scope.as_str();
                        tx.execute("DELETE FROM playlists WHERE account_scope = ?1", params![s])?;
                        tx.execute(
                            "DELETE FROM playlist_tracks WHERE account_scope = ?1",
                            params![s],
                        )?;
                        tx.execute(
                            "DELETE FROM catalog_cache WHERE account_scope = ?1",
                            params![s],
                        )?;
                    }
                    None => {
                        tx.execute("DELETE FROM playlists", [])?;
                        tx.execute("DELETE FROM playlist_tracks", [])?;
                        tx.execute("DELETE FROM catalog_cache", [])?;
                    }
                }
                tx.execute("DELETE FROM lyrics_cache", [])?;
                tx.commit()?;
                Ok(())
            })
            .await
        })
    }

    fn resolve_asset(
        &self,
        asset_id: AssetId,
    ) -> BoxFuture<'_, Result<AssetResolvedPayload, QueryError>> {
        let covers = self.covers.clone();
        Box::pin(async move {
            covers
                .resolve_asset(asset_id)
                .await
                .map_err(QueryError::from)
        })
    }

    fn pin_cover(&self, asset_id: Option<AssetId>) {
        self.covers.pin(asset_id.as_ref());
    }

    fn cached_cover(&self, remote_url: &str) -> Option<CachedCover> {
        self.covers
            .cached(remote_url)
            .map(|(asset_id, accent_oklch)| CachedCover {
                asset_id,
                accent_oklch,
            })
    }

    fn cache_cover(&self, remote_url: String) -> BoxFuture<'_, Result<CachedCover, QueryError>> {
        Box::pin(async move {
            let info = self.fetch_and_cache_cover(&remote_url).await?;
            Ok(CachedCover {
                asset_id: info.asset_id,
                accent_oklch: info.accent_oklch,
            })
        })
    }

    fn clear_account_data(
        &self,
        account_scope: Option<AccountScope>,
        pending_epoch: Option<AccountEpoch>,
    ) -> BoxFuture<'_, Result<(), QueryError>> {
        let conn = self.conn.clone();
        let clock = self.clock.clone();
        Box::pin(run_blocking(move || {
            let now_ms = clock.wall_now_ms();
            let mut guard = lock_conn(&conn)?;
            let tx = guard.transaction()?;
            if let Some(scope) = account_scope {
                clear_account_data_tx(&tx, &scope, now_ms)?;
            }
            if let Some(epoch) = pending_epoch {
                clear_pending_account_tx(&tx, &epoch, now_ms)?;
            }
            tx.commit()?;
            Ok(())
        }))
    }

    fn last_session(&self) -> BoxFuture<'_, Result<Option<LastSession>, QueryError>> {
        let conn = self.conn.clone();
        Box::pin(async move {
            let scope = run_blocking(move || {
                let guard = lock_conn(&conn)?;
                last_active_account_scope_conn(&guard)
            })
            .await?;
            let Some(account_scope) = scope else {
                return Ok(None);
            };
            let last_played =
                self.get_latest_playback(&account_scope)
                    .await?
                    .map(|rec| LastPlayed {
                        track: rec.track,
                        playback_ref: rec.playback_ref,
                        position_ms: rec.position_ms,
                    });
            Ok(Some(LastSession {
                account_scope,
                last_played,
            }))
        })
    }

    fn record_playback(
        &self,
        account_scope: AccountScope,
        account_epoch: AccountEpoch,
        played: LastPlayed,
    ) -> BoxFuture<'_, Result<(), QueryError>> {
        Box::pin(async move {
            let entry = RecentPlaybackRecord {
                duration_ms: played.track.duration_ms,
                track: played.track,
                playback_ref: played.playback_ref,
                position_ms: played.position_ms,
                recorded_at_ms: self.clock.wall_now_ms(),
                source_context: None,
            };
            self.record_recent_playback(&account_scope, &account_epoch, entry)
                .await
        })
    }

    fn recent_searches(
        &self,
        account_scope: AccountScope,
        account_epoch: AccountEpoch,
        request: RecentSearchesRequest,
    ) -> BoxFuture<'_, Result<RecentSearchesSnapshot, QueryError>> {
        let conn = self.conn.clone();
        let clock = self.clock.clone();
        Box::pin(run_blocking(move || {
            let now_ms = clock.wall_now_ms();
            let mut guard = lock_conn(&conn)?;
            let tx = guard.transaction()?;
            let snapshot =
                apply_recent_searches_tx(&tx, &account_scope, &account_epoch, &request, now_ms)?;
            tx.commit()?;
            Ok(snapshot)
        }))
    }

    fn update_playback_position(
        &self,
        account_scope: AccountScope,
        account_epoch: AccountEpoch,
        video_id: VideoId,
        position_ms: u64,
    ) -> BoxFuture<'_, Result<(), QueryError>> {
        let conn = self.conn.clone();
        let clock = self.clock.clone();
        Box::pin(async move {
            run_blocking(move || {
                let now_ms = clock.wall_now_ms();
                let mut guard = lock_conn(&conn)?;
                let tx = guard.transaction()?;
                update_latest_playback_position_tx(
                    &tx,
                    &account_scope,
                    &account_epoch,
                    &video_id,
                    position_ms,
                    now_ms,
                )?;
                tx.commit()?;
                Ok(())
            })
            .await
        })
    }
}

fn lock_conn(
    conn: &Arc<Mutex<Connection>>,
) -> Result<std::sync::MutexGuard<'_, Connection>, StorageError> {
    conn.lock()
        .map_err(|_| StorageError::Storage("sqlite connection mutex poisoned".to_string()))
}

async fn run_blocking<F, T>(f: F) -> Result<T, QueryError>
where
    F: FnOnce() -> Result<T, StorageError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|err| {
            StorageError::Storage(format!("blocking storage task panicked: {err}"))
                .into_query_error()
        })?
        .map_err(QueryError::from)
}
