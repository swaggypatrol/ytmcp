use crate::error::StorageError;
use crate::paths::{ensure_dir_0700, set_file_0600, write_atomic_0600};
use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView, ImageFormat, ImageReader, Limits};
use reqwest::redirect::Policy;
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::io::Cursor;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::debug;
use url::Url;
use ytm_core::ports::Clock;
use ytm_protocol::{AssetId, AssetResolvedPayload};

pub const DEFAULT_COVER_CACHE_MAX_BYTES: u64 = 1024 * 1024 * 1024; // 1 GiB
pub const DEFAULT_COVER_MAX_DOWNLOAD_BYTES: usize = 10 * 1024 * 1024; // 10 MiB
pub const DEFAULT_COVER_MAX_WIDTH: u32 = 4096;
pub const DEFAULT_COVER_MAX_HEIGHT: u32 = 4096;
pub const DEFAULT_COVER_TIMEOUT_SECS: u64 = 15;
pub const DEFAULT_COVER_MAX_REDIRECTS: usize = 3;

pub const DEFAULT_ALLOWED_COVER_HOSTS: &[&str] = &[
    "lh3.googleusercontent.com",
    "i.ytimg.com",
    "yt3.ggpht.com",
    "yt3.googleusercontent.com",
];

#[derive(Debug, Clone)]
pub struct CoverCacheConfig {
    pub max_cache_bytes: u64,
    pub max_download_bytes: usize,
    pub max_image_width: u32,
    pub max_image_height: u32,
    pub request_timeout: Duration,
    pub max_redirects: usize,
    pub allowed_hosts: Vec<String>,
    /// Exact socket addresses permitted ONLY for local mock server tests (e.g. `wiremock`).
    /// Always empty in production; non-matching loopback/private/link-local/ULA targets remain blocked.
    pub test_allowed_endpoints: Vec<SocketAddr>,
}

impl Default for CoverCacheConfig {
    fn default() -> Self {
        Self::production()
    }
}

impl CoverCacheConfig {
    pub fn production() -> Self {
        Self {
            max_cache_bytes: DEFAULT_COVER_CACHE_MAX_BYTES,
            max_download_bytes: DEFAULT_COVER_MAX_DOWNLOAD_BYTES,
            max_image_width: DEFAULT_COVER_MAX_WIDTH,
            max_image_height: DEFAULT_COVER_MAX_HEIGHT,
            request_timeout: Duration::from_secs(DEFAULT_COVER_TIMEOUT_SECS),
            max_redirects: DEFAULT_COVER_MAX_REDIRECTS,
            allowed_hosts: DEFAULT_ALLOWED_COVER_HOSTS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            test_allowed_endpoints: Vec::new(),
        }
    }

    /// Test-only helper to allow a specific local mock server endpoint (e.g. `wiremock` address).
    pub fn with_test_endpoint(mut self, addr: SocketAddr) -> Self {
        self.test_allowed_endpoints.push(addr);
        self
    }

    pub fn with_max_cache_bytes(mut self, max_cache_bytes: u64) -> Self {
        self.max_cache_bytes = max_cache_bytes;
        self
    }

    pub fn with_max_download_bytes(mut self, max_download_bytes: usize) -> Self {
        self.max_download_bytes = max_download_bytes;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedCoverInfo {
    pub asset_id: AssetId,
    pub file_path: PathBuf,
    pub mime_type: String,
    pub byte_size: u64,
    pub width: u32,
    pub height: u32,
    pub accent_oklch: String,
}

/// SSRF-safe cover art downloader, decoder, dominant color extractor, and LRU disk cache.
#[derive(Clone)]
pub struct CoverCache {
    covers_dir: PathBuf,
    conn: Arc<Mutex<Connection>>,
    clock: Arc<dyn Clock>,
    config: CoverCacheConfig,
    client: reqwest::Client,
    /// The now-playing cover. Capacity eviction skips it, so a long-paused track keeps the file
    /// that media controls already publish.
    pinned: Arc<Mutex<Option<String>>>,
    /// Asset id -> accent of every row in `cover_assets`, so a cached cover can be named without
    /// touching the database. Changed only while the connection lock is held, after the row
    /// change commits.
    index: Arc<Mutex<HashMap<String, String>>>,
}

impl CoverCache {
    pub fn new(
        covers_dir: PathBuf,
        conn: Arc<Mutex<Connection>>,
        clock: Arc<dyn Clock>,
        config: CoverCacheConfig,
    ) -> Result<Self, StorageError> {
        ensure_dir_0700(&covers_dir)?;
        let client = build_ssrf_safe_client(&config)?;
        let index = {
            let guard = conn
                .lock()
                .map_err(|_| StorageError::Storage("sqlite mutex poisoned".to_string()))?;
            load_index(&guard)
        };
        Ok(Self {
            covers_dir,
            conn,
            clock,
            config,
            client,
            pinned: Arc::new(Mutex::new(None)),
            index: Arc::new(Mutex::new(index)),
        })
    }

    /// The cover behind `raw_url` when it is in the cache, from memory only (no disk or network
    /// IO), so a caller on a latency-sensitive path can use it at once. `None` for a cover that
    /// is not cached, was evicted or cleared, or whose URL is not allowed.
    pub fn cached(&self, raw_url: &str) -> Option<(AssetId, String)> {
        let (asset_id, _) = self.asset_id_for_url(raw_url).ok()?;
        let accent = self.index.lock().ok()?.get(asset_id.as_str())?.clone();
        Some((asset_id, accent))
    }

    /// Marks the now-playing cover so capacity eviction keeps it; `None` releases the pin.
    pub fn pin(&self, asset_id: Option<&AssetId>) {
        if let Ok(mut pinned) = self.pinned.lock() {
            *pinned = asset_id.map(|id| id.as_str().to_owned());
        }
    }

    pub fn config(&self) -> &CoverCacheConfig {
        &self.config
    }

    pub fn covers_dir(&self) -> &Path {
        &self.covers_dir
    }

    /// Validates a URL against the strict HTTPS allowlist and private-IP blocklist without performing network IO.
    pub fn validate_url_static(&self, raw_url: &str) -> Result<Url, StorageError> {
        let parsed = Url::parse(raw_url)
            .map_err(|err| StorageError::InvalidPayload(format!("invalid cover URL: {err}")))?;
        validate_url_policy(&parsed, &self.config)?;
        Ok(parsed)
    }

    /// Deterministic asset id for a remote cover: SHA-256 (hex) of the canonical URL after the
    /// static allowlist check. Pure (no IO), so catalog normalization can assign ids eagerly.
    pub fn asset_id_for_url(&self, raw_url: &str) -> Result<(AssetId, String), StorageError> {
        let parsed = self.validate_url_static(raw_url)?;
        let canonical = canonical_cover_url(parsed);
        let asset_id = AssetId::new(hex::encode(Sha256::digest(canonical.as_bytes())))
            .map_err(|err| StorageError::Storage(format!("invalid generated asset id: {err}")))?;
        Ok((asset_id, canonical))
    }

    /// Records `asset id -> canonical URL` so the asset can be fetched lazily on first resolve.
    pub async fn register_urls(&self, entries: Vec<(AssetId, String)>) -> Result<(), StorageError> {
        if entries.is_empty() {
            return Ok(());
        }
        let conn = self.conn.clone();
        let clock = self.clock.clone();
        tokio::task::spawn_blocking(move || -> Result<(), StorageError> {
            let now_ms = i64::try_from(clock.wall_now_ms()).unwrap_or(i64::MAX);
            let mut guard = conn
                .lock()
                .map_err(|_| StorageError::Storage("sqlite mutex poisoned".to_string()))?;
            let tx = guard.transaction()?;
            for (asset_id, url) in entries {
                tx.execute(
                    "INSERT OR IGNORE INTO cover_url_map (source_url, asset_id, fetched_at_ms) VALUES (?1, ?2, ?3)",
                    params![url, asset_id.as_str(), now_ms],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
        .map_err(|err| StorageError::Storage(format!("cover register task failed: {err}")))?
    }

    /// Fetches the cover behind `raw_url` (or returns the cached copy), validates
    /// format/size/dimensions, computes `accent_oklch`, stores atomically under the URL-derived
    /// asset id, and enforces the LRU quota.
    pub async fn fetch_and_cache(&self, raw_url: &str) -> Result<CachedCoverInfo, StorageError> {
        let (asset_id, canonical) = self.asset_id_for_url(raw_url)?;
        if let Some(existing) = self.lookup_cached(&asset_id).await? {
            return Ok(existing);
        }
        let parsed = Url::parse(&canonical)
            .map_err(|err| StorageError::InvalidPayload(format!("invalid cover URL: {err}")))?;

        let bytes = tokio::time::timeout(self.config.request_timeout, self.download(parsed))
            .await
            .map_err(|_| StorageError::Timeout("cover fetch timed out".to_string()))??;
        self.store_image_bytes(asset_id, &canonical, &bytes).await
    }

    async fn download(&self, mut url: Url) -> Result<Vec<u8>, StorageError> {
        let mut redirects = 0;
        let response = loop {
            validate_url_policy_async(&url, &self.config).await?;
            let response = self
                .client
                .get(url.clone())
                .send()
                .await
                .map_err(classify_reqwest_error)?;
            let status = response.status();
            if !status.is_redirection() {
                break response;
            }
            if !matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308)
                || redirects >= self.config.max_redirects
            {
                return Err(StorageError::InvalidPayload(format!(
                    "cover redirect blocked (HTTP {status})"
                )));
            }
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| {
                    StorageError::InvalidPayload(
                        "missing or invalid cover redirect location".to_string(),
                    )
                })?;
            url = url.join(location).map_err(|_| {
                StorageError::InvalidPayload("invalid cover redirect URL".to_string())
            })?;
            redirects += 1;
        };

        if !response.status().is_success() {
            return Err(StorageError::Network(format!(
                "cover server returned HTTP {}",
                response.status().as_u16()
            )));
        }

        if let Some(len) = response.content_length()
            && len > u64::try_from(self.config.max_download_bytes).unwrap_or(u64::MAX)
        {
            return Err(StorageError::InvalidPayload(format!(
                "cover image Content-Length {len} exceeds limit {}",
                self.config.max_download_bytes
            )));
        }

        let mut bytes = Vec::new();
        let mut resp = response;
        while let Some(chunk) = resp.chunk().await.map_err(classify_reqwest_error)? {
            if bytes.len().saturating_add(chunk.len()) > self.config.max_download_bytes {
                return Err(StorageError::InvalidPayload(format!(
                    "cover image download exceeds limit {} bytes",
                    self.config.max_download_bytes
                )));
            }
            bytes.extend_from_slice(&chunk);
        }

        if bytes.is_empty() {
            return Err(StorageError::InvalidPayload(
                "cover image response body is empty".to_string(),
            ));
        }

        Ok(bytes)
    }

    async fn store_image_bytes(
        &self,
        asset_id: AssetId,
        source_url: &str,
        bytes: &[u8],
    ) -> Result<CachedCoverInfo, StorageError> {
        let max_w = self.config.max_image_width;
        let max_h = self.config.max_image_height;
        let raw_bytes = bytes.to_vec();

        // Decode & extract color off the async executor thread.
        let decoded = tokio::task::spawn_blocking(move || {
            validate_and_decode_image(&raw_bytes, max_w, max_h)
        })
        .await
        .map_err(|err| StorageError::Storage(format!("image decode task failed: {err}")))??;

        let file_name = format!("{}.{}", asset_id.as_str(), decoded.ext);
        let dest_path = self.covers_dir.join(&file_name);
        let byte_size = u64::try_from(bytes.len())
            .map_err(|_| StorageError::InvalidPayload("cover size overflow".to_string()))?;

        if byte_size > self.config.max_cache_bytes {
            return Err(StorageError::InvalidPayload(format!(
                "single cover asset ({byte_size} bytes) exceeds max cache quota ({})",
                self.config.max_cache_bytes
            )));
        }

        if !dest_path.exists() {
            write_atomic_0600(&dest_path, bytes)?;
        } else {
            set_file_0600(&dest_path)?;
        }

        let canonical_file = fs::canonicalize(&dest_path)?;
        let covers_dir = self.covers_dir.clone();
        let conn = self.conn.clone();
        let clock = self.clock.clone();
        let max_cache_bytes = self.config.max_cache_bytes;
        let pinned = self.pinned.lock().ok().and_then(|pinned| pinned.clone());
        let source_url_owned = source_url.to_string();
        let asset_id_str = asset_id.as_str().to_string();
        let mime_type = decoded.mime_type.to_string();
        let ext = decoded.ext.to_string();
        let accent_oklch = decoded.accent_oklch.clone();
        let width = decoded.width;
        let height = decoded.height;
        let index = self.index.clone();

        tokio::task::spawn_blocking(move || -> Result<(), StorageError> {
            let now_ms = i64::try_from(clock.wall_now_ms()).unwrap_or(i64::MAX);
            let mut guard = conn
                .lock()
                .map_err(|_| StorageError::Storage("sqlite mutex poisoned".to_string()))?;
            let tx = guard.transaction()?;

            tx.execute(
                r#"
INSERT INTO cover_assets
    (asset_id, rel_path, mime_type, byte_size, width, height, accent_oklch, created_at_ms, last_accessed_ms, format_ext)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, ?9)
ON CONFLICT(asset_id) DO UPDATE SET
    rel_path = excluded.rel_path,
    mime_type = excluded.mime_type,
    byte_size = excluded.byte_size,
    width = excluded.width,
    height = excluded.height,
    accent_oklch = excluded.accent_oklch,
    last_accessed_ms = excluded.last_accessed_ms,
    format_ext = excluded.format_ext
"#,
                params![
                    &asset_id_str,
                    &file_name,
                    &mime_type,
                    i64::try_from(byte_size).unwrap_or(i64::MAX),
                    width,
                    height,
                    &accent_oklch,
                    now_ms,
                    &ext
                ],
            )?;

            tx.execute(
                r#"
INSERT INTO cover_url_map (source_url, asset_id, fetched_at_ms)
VALUES (?1, ?2, ?3)
ON CONFLICT(source_url) DO UPDATE SET
    asset_id = excluded.asset_id,
    fetched_at_ms = excluded.fetched_at_ms
"#,
                params![source_url_owned, &asset_id_str, now_ms],
            )?;

            let evicted = evict_lru_in_tx(
                &tx,
                &covers_dir,
                max_cache_bytes,
                &asset_id_str,
                pinned.as_deref(),
            )?;
            tx.commit()?;
            if let Ok(mut index) = index.lock() {
                for id in &evicted {
                    index.remove(id);
                }
                index.insert(asset_id_str, accent_oklch);
            }
            Ok(())
        })
        .await
        .map_err(|err| StorageError::Storage(format!("cover db task failed: {err}")))??;

        debug!(asset_id = %asset_id, width, height, accent = %decoded.accent_oklch, "cached cover art");

        Ok(CachedCoverInfo {
            asset_id,
            file_path: canonical_file,
            mime_type: decoded.mime_type.to_string(),
            byte_size,
            width,
            height,
            accent_oklch: decoded.accent_oklch,
        })
    }

    async fn lookup_cached(
        &self,
        asset_id: &AssetId,
    ) -> Result<Option<CachedCoverInfo>, StorageError> {
        let conn = self.conn.clone();
        let clock = self.clock.clone();
        let covers_dir = self.covers_dir.clone();
        let id_owned = asset_id.clone();
        let index = self.index.clone();

        tokio::task::spawn_blocking(move || -> Result<Option<CachedCoverInfo>, StorageError> {
            let mut guard = conn
                .lock()
                .map_err(|_| StorageError::Storage("sqlite mutex poisoned".to_string()))?;
            let tx = guard.transaction()?;

            let row: Option<(String, String, i64, u32, u32, String)> = tx
                .query_row(
                    "SELECT rel_path, mime_type, byte_size, width, height, accent_oklch FROM cover_assets WHERE asset_id = ?1",
                    params![id_owned.as_str()],
                    |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                        ))
                    },
                )
                .optional()?;

            let Some((rel_path, mime_type, byte_size, width, height, accent_oklch)) = row else {
                return Ok(None);
            };

            let verified_path = match verify_asset_path(&covers_dir, id_owned.as_str(), &rel_path)
            {
                Ok(path) => path,
                // Symlinks, traversal or non-regular files are rejected, never re-fetched over.
                Err(err @ StorageError::InvalidPayload(_)) => return Err(err),
                // The file vanished: forget the row so the asset is fetched again.
                Err(_) => {
                    tx.execute(
                        "DELETE FROM cover_assets WHERE asset_id = ?1",
                        params![id_owned.as_str()],
                    )?;
                    tx.commit()?;
                    if let Ok(mut index) = index.lock() {
                        index.remove(id_owned.as_str());
                    }
                    return Ok(None);
                }
            };

            let now_ms = i64::try_from(clock.wall_now_ms()).unwrap_or(i64::MAX);
            tx.execute(
                "UPDATE cover_assets SET last_accessed_ms = ?1 WHERE asset_id = ?2",
                params![now_ms, id_owned.as_str()],
            )?;
            tx.commit()?;
            Ok(Some(CachedCoverInfo {
                asset_id: id_owned,
                file_path: verified_path,
                mime_type,
                byte_size: u64::try_from(byte_size.max(0)).unwrap_or(0),
                width,
                height,
                accent_oklch,
            }))
        })
        .await
        .map_err(|err| StorageError::Storage(format!("cover lookup task failed: {err}")))?
    }

    async fn registered_url(&self, asset_id: &AssetId) -> Result<Option<String>, StorageError> {
        let conn = self.conn.clone();
        let id_owned = asset_id.clone();
        tokio::task::spawn_blocking(move || -> Result<Option<String>, StorageError> {
            let guard = conn
                .lock()
                .map_err(|_| StorageError::Storage("sqlite mutex poisoned".to_string()))?;
            Ok(guard
                .query_row(
                    "SELECT source_url FROM cover_url_map WHERE asset_id = ?1 LIMIT 1",
                    params![id_owned.as_str()],
                    |r| r.get(0),
                )
                .optional()?)
        })
        .await
        .map_err(|err| StorageError::Storage(format!("cover url lookup failed: {err}")))?
    }

    /// Resolves an `AssetId` to a verified file inside `covers_dir` (no traversal, no symlinks),
    /// fetching the registered remote URL on first use.
    pub async fn resolve_asset(
        &self,
        asset_id: AssetId,
    ) -> Result<AssetResolvedPayload, StorageError> {
        if !is_valid_sha256_hex(asset_id.as_str()) {
            return Err(StorageError::InvalidPayload(
                "invalid assetId format; expected 64 lowercase hex characters".to_string(),
            ));
        }
        let info = match self.lookup_cached(&asset_id).await? {
            Some(info) => info,
            None => {
                let Some(url) = self.registered_url(&asset_id).await? else {
                    return Err(StorageError::Storage(format!(
                        "cover asset {} is not registered",
                        asset_id.as_str()
                    )));
                };
                let info = self.fetch_and_cache(&url).await?;
                if info.asset_id != asset_id {
                    return Err(StorageError::Storage(
                        "registered cover URL maps to a different asset".to_string(),
                    ));
                }
                info
            }
        };
        Ok(AssetResolvedPayload {
            asset_id,
            file_path: Some(info.file_path.to_string_lossy().into_owned()),
            mime_type: Some(info.mime_type),
            error: None,
        })
    }

    /// Clears all cover files in `covers_dir` and clears `cover_assets` / `cover_url_map` rows.
    pub async fn clear_all(&self) -> Result<(), StorageError> {
        let conn = self.conn.clone();
        let covers_dir = self.covers_dir.clone();
        let index = self.index.clone();

        tokio::task::spawn_blocking(move || -> Result<(), StorageError> {
            {
                let mut guard = conn
                    .lock()
                    .map_err(|_| StorageError::Storage("sqlite mutex poisoned".to_string()))?;
                let tx = guard.transaction()?;
                tx.execute("DELETE FROM cover_url_map", [])?;
                tx.execute("DELETE FROM cover_assets", [])?;
                tx.commit()?;
                if let Ok(mut index) = index.lock() {
                    index.clear();
                }
            }

            if covers_dir.exists() {
                for entry in fs::read_dir(&covers_dir)? {
                    let entry = entry?;
                    let path = entry.path();
                    if path.is_dir() {
                        fs::remove_dir_all(&path)?;
                    } else {
                        fs::remove_file(&path)?;
                    }
                }
            }
            ensure_dir_0700(&covers_dir)?;
            Ok(())
        })
        .await
        .map_err(|err| StorageError::Storage(format!("clear cover cache task failed: {err}")))?
    }

    /// Returns total cached cover bytes currently tracked in SQLite.
    pub async fn total_cached_bytes(&self) -> Result<u64, StorageError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<u64, StorageError> {
            let guard = conn
                .lock()
                .map_err(|_| StorageError::Storage("sqlite mutex poisoned".to_string()))?;
            let total: i64 = guard.query_row(
                "SELECT COALESCE(SUM(byte_size), 0) FROM cover_assets",
                [],
                |r| r.get(0),
            )?;
            Ok(u64::try_from(total.max(0)).unwrap_or(0))
        })
        .await
        .map_err(|err| StorageError::Storage(format!("cover size query failed: {err}")))?
    }
}

/// Deletes least recently used covers until the cache fits its quota; returns the evicted ids.
fn evict_lru_in_tx(
    tx: &rusqlite::Transaction<'_>,
    covers_dir: &Path,
    max_cache_bytes: u64,
    keep_asset_id: &str,
    pinned_asset_id: Option<&str>,
) -> Result<Vec<String>, StorageError> {
    let mut evicted = Vec::new();
    let mut total: i64 = tx.query_row(
        "SELECT COALESCE(SUM(byte_size), 0) FROM cover_assets",
        [],
        |r| r.get(0),
    )?;
    let max_bytes_i64 = i64::try_from(max_cache_bytes).unwrap_or(i64::MAX);
    if total <= max_bytes_i64 {
        return Ok(evicted);
    }

    let mut stmt = tx.prepare(
        "SELECT asset_id, rel_path, byte_size FROM cover_assets ORDER BY last_accessed_ms ASC, created_at_ms ASC, asset_id ASC",
    )?;
    let candidates: Vec<(String, String, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);

    for (asset_id, rel_path, byte_size) in candidates {
        if total <= max_bytes_i64 {
            break;
        }
        if asset_id == keep_asset_id || Some(asset_id.as_str()) == pinned_asset_id {
            continue;
        }
        // The URL mapping is kept so an evicted cover is fetched again on its next resolve.
        tx.execute(
            "DELETE FROM cover_assets WHERE asset_id = ?1",
            params![&asset_id],
        )?;
        if is_safe_single_filename(&rel_path) {
            let path = covers_dir.join(&rel_path);
            if path.exists() {
                let _ = fs::remove_file(&path);
            }
        }
        total = total.saturating_sub(byte_size);
        evicted.push(asset_id);
    }

    Ok(evicted)
}

/// Reads the cached covers' ids and accents; a database without the covers table (opened at an
/// early schema version) has none.
fn load_index(conn: &Connection) -> HashMap<String, String> {
    let rows = conn
        .prepare("SELECT asset_id, accent_oklch FROM cover_assets")
        .and_then(|mut stmt| {
            stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .collect::<Result<HashMap<_, _>, _>>()
        });
    rows.unwrap_or_else(|err| {
        debug!("cover index starts empty: {err}");
        HashMap::new()
    })
}

fn verify_asset_path(
    covers_dir: &Path,
    expected_asset_id: &str,
    rel_path: &str,
) -> Result<PathBuf, StorageError> {
    if !is_valid_sha256_hex(expected_asset_id) {
        return Err(StorageError::InvalidPayload(
            "invalid cover asset id".to_string(),
        ));
    }
    if !is_safe_single_filename(rel_path) || !rel_path.starts_with(expected_asset_id) {
        return Err(StorageError::InvalidPayload(
            "invalid relative cover path; potential path traversal rejected".to_string(),
        ));
    }
    let candidate = covers_dir.join(rel_path);
    let meta = fs::symlink_metadata(&candidate).map_err(|err| {
        StorageError::Storage(format!("cover asset file missing or inaccessible: {err}"))
    })?;
    if meta.file_type().is_symlink() {
        return Err(StorageError::InvalidPayload(
            "symlink cover asset rejected".to_string(),
        ));
    }
    if !meta.file_type().is_file() {
        return Err(StorageError::InvalidPayload(
            "cover asset path is not a regular file".to_string(),
        ));
    }

    let canon_dir = fs::canonicalize(covers_dir)?;
    let canon_file = fs::canonicalize(&candidate)?;
    if !canon_file.starts_with(&canon_dir) || canon_file == canon_dir {
        return Err(StorageError::InvalidPayload(
            "canonical cover path escaped covers directory".to_string(),
        ));
    }
    Ok(canon_file)
}

fn is_safe_single_filename(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains("..")
        && !name.contains('\0')
        && name != "."
}

/// Square edge requested from Google image hosts; large enough for the now-playing view and
/// MPRIS while staying far below the download limit.
pub const COVER_EDGE_PX: u32 = 544;

/// Normalizes Google image-host size suffixes (`=w60-h60-l90-rj`, `=s120`, ...) to one size so
/// every thumbnail variant of an image maps to the same asset. Other URLs are kept verbatim.
/// Idempotent: the canonical form of a canonical URL is itself.
fn canonical_cover_url(mut url: Url) -> String {
    let google_host = url.host_str().is_some_and(|h| {
        h == "lh3.googleusercontent.com" || h == "yt3.googleusercontent.com" || h == "yt3.ggpht.com"
    });
    if google_host {
        let path = url.path().to_string();
        if let Some((base, suffix)) = path.rsplit_once('=')
            && suffix
                .split('-')
                .next()
                .is_some_and(|p| p.len() > 1 && matches!(p.as_bytes()[0], b'w' | b'h' | b's'))
        {
            url.set_path(&format!("{base}=w{COVER_EDGE_PX}-h{COVER_EDGE_PX}-l90-rj"));
        }
    }
    url.set_fragment(None);
    url.to_string()
}

pub fn is_valid_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn build_ssrf_safe_client(config: &CoverCacheConfig) -> Result<reqwest::Client, StorageError> {
    // Each hop is checked asynchronously before the next request in `download`.
    let mut builder = reqwest::Client::builder().redirect(Policy::none());

    if !config.test_allowed_endpoints.is_empty() {
        builder = builder.no_proxy();
    }

    builder
        .build()
        .map_err(|err| StorageError::Storage(format!("failed to build HTTP client: {err}")))
}

fn matches_test_endpoint(url: &Url, test_endpoints: &[SocketAddr]) -> bool {
    if test_endpoints.is_empty() {
        return false;
    }
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    let Some(port) = url.port_or_known_default() else {
        return false;
    };

    let ip_opt: Option<IpAddr> = if host.eq_ignore_ascii_case("localhost") {
        Some(IpAddr::V4(Ipv4Addr::LOCALHOST))
    } else {
        host.trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
            .ok()
    };

    let Some(ip) = ip_opt else {
        return false;
    };
    let candidate = SocketAddr::new(ip, port);
    test_endpoints.contains(&candidate)
}

fn validate_url_policy(url: &Url, config: &CoverCacheConfig) -> Result<(), StorageError> {
    if !url.username().is_empty() || url.password().is_some() {
        return Err(StorageError::InvalidPayload(
            "cover URL must not contain userinfo credentials".to_string(),
        ));
    }

    if matches_test_endpoint(url, &config.test_allowed_endpoints) {
        return Ok(());
    }

    if url.scheme() != "https" {
        return Err(StorageError::InvalidPayload(format!(
            "unsupported cover URL scheme '{}'; only https is allowed",
            url.scheme()
        )));
    }

    if let Some(port) = url.port()
        && port != 443
    {
        return Err(StorageError::InvalidPayload(format!(
            "non-standard HTTPS port {port} is not allowed for cover fetch"
        )));
    }

    let raw_host = url
        .host_str()
        .ok_or_else(|| StorageError::InvalidPayload("cover URL is missing host".to_string()))?;
    let host_trimmed = raw_host.trim_end_matches('.').to_ascii_lowercase();

    // Reject IP literals (IPv4 or bracketed IPv6) outright for production hosts.
    let unbracketed = host_trimmed.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = unbracketed.parse::<IpAddr>() {
        return Err(StorageError::InvalidPayload(format!(
            "direct IP literal {ip} is blocked for cover URLs"
        )));
    }

    let is_allowed_host = config
        .allowed_hosts
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(&host_trimmed));
    if !is_allowed_host {
        return Err(StorageError::InvalidPayload(format!(
            "cover host '{host_trimmed}' is not in the HTTPS allowlist"
        )));
    }

    Ok(())
}

async fn validate_url_policy_async(
    url: &Url,
    config: &CoverCacheConfig,
) -> Result<(), StorageError> {
    validate_url_policy(url, config)?;
    if matches_test_endpoint(url, &config.test_allowed_endpoints) {
        return Ok(());
    }

    let host = url
        .host_str()
        .ok_or_else(|| StorageError::InvalidPayload("cover URL missing host".to_string()))?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    let port = url.port_or_known_default().unwrap_or(443);

    let addrs = tokio::net::lookup_host((host.as_str(), port))
        .await
        .map_err(|err| StorageError::Network(format!("DNS resolution failed: {err}")))?;
    let mut saw_any = false;
    for addr in addrs {
        saw_any = true;
        if !is_public_unicast_ip(addr.ip()) {
            return Err(StorageError::InvalidPayload(format!(
                "resolved non-public IP {} blocked by SSRF guard",
                addr.ip()
            )));
        }
    }
    if !saw_any {
        return Err(StorageError::Network(
            "DNS resolution returned no addresses".to_string(),
        ));
    }
    Ok(())
}

/// Checks that `ip` is globally routable unicast (blocks loopback, RFC1918, CGNAT, link-local, ULA, unspecified, multicast, broadcast, documentation, and IPv4-mapped equivalents).
pub fn is_public_unicast_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_unicast_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4_mapped) = v6.to_ipv4_mapped() {
                return is_public_unicast_v4(v4_mapped);
            }
            is_public_unicast_v6(v6)
        }
    }
}

fn is_public_unicast_v4(v4: Ipv4Addr) -> bool {
    let o = v4.octets();
    let is_cgnat = o[0] == 100 && (o[1] & 0b1100_0000) == 64;
    let is_ietf_protocol = o[0] == 192 && o[1] == 0 && o[2] == 0;
    let is_benchmark = o[0] == 198 && (o[1] == 18 || o[1] == 19);
    let is_reserved_240 = o[0] >= 240;

    !(v4.is_loopback()
        || v4.is_private()
        || v4.is_link_local()
        || v4.is_unspecified()
        || v4.is_broadcast()
        || v4.is_multicast()
        || v4.is_documentation()
        || is_cgnat
        || is_ietf_protocol
        || is_benchmark
        || is_reserved_240)
}

fn is_public_unicast_v6(v6: Ipv6Addr) -> bool {
    let seg0 = v6.segments()[0];
    let is_ula = (seg0 & 0xfe00) == 0xfc00;
    let is_link_local = (seg0 & 0xffc0) == 0xfe80;
    let is_documentation = seg0 == 0x2001 && v6.segments()[1] == 0x0db8;

    !(v6.is_loopback()
        || v6.is_unspecified()
        || v6.is_multicast()
        || is_ula
        || is_link_local
        || is_documentation)
}

fn classify_reqwest_error(err: reqwest::Error) -> StorageError {
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(&err);
    while let Some(source) = cur {
        if let Some(storage_err) = source.downcast_ref::<StorageError>() {
            return match storage_err {
                StorageError::InvalidPayload(msg) => StorageError::InvalidPayload(msg.clone()),
                StorageError::Timeout(msg) => StorageError::Timeout(msg.clone()),
                StorageError::Network(msg) => StorageError::Network(msg.clone()),
                StorageError::StaleContext(msg) => StorageError::StaleContext(msg.clone()),
                StorageError::AccountMismatch(msg) => StorageError::AccountMismatch(msg.clone()),
                StorageError::Storage(msg) => StorageError::Storage(msg.clone()),
            };
        }
        if let Some(req_err) = source.downcast_ref::<reqwest::Error>() {
            if req_err.is_timeout() {
                return StorageError::Timeout("cover fetch timed out".to_string());
            }
            if req_err.is_redirect() || req_err.is_builder() {
                return StorageError::InvalidPayload("cover URL/redirect blocked".to_string());
            }
        }
        cur = source.source();
    }
    StorageError::Network("cover fetch failed".to_string())
}

struct DecodedCover {
    ext: &'static str,
    mime_type: &'static str,
    width: u32,
    height: u32,
    accent_oklch: String,
}

fn validate_and_decode_image(
    bytes: &[u8],
    max_width: u32,
    max_height: u32,
) -> Result<DecodedCover, StorageError> {
    let format = image::guess_format(bytes).map_err(|err| {
        StorageError::InvalidPayload(format!("unrecognized cover image format: {err}"))
    })?;

    let (ext, mime_type) = match format {
        ImageFormat::Png => ("png", "image/png"),
        ImageFormat::Jpeg => ("jpg", "image/jpeg"),
        ImageFormat::WebP => ("webp", "image/webp"),
        other => {
            return Err(StorageError::InvalidPayload(format!(
                "unsupported cover image format: {other:?}"
            )));
        }
    };

    let mut dim_reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(max_width);
    limits.max_image_height = Some(max_height);
    limits.max_alloc = Some(128 * 1024 * 1024);
    dim_reader.limits(limits.clone());

    let (width, height) = dim_reader.into_dimensions().map_err(|err| {
        StorageError::InvalidPayload(format!("invalid cover image dimensions: {err}"))
    })?;

    if width == 0 || height == 0 || width > max_width || height > max_height {
        return Err(StorageError::InvalidPayload(format!(
            "cover dimensions {width}x{height} exceed limit {max_width}x{max_height}"
        )));
    }

    let mut decode_reader = ImageReader::with_format(Cursor::new(bytes), format);
    decode_reader.limits(limits);
    let image = decode_reader.decode().map_err(|err| {
        StorageError::InvalidPayload(format!("corrupt cover image payload: {err}"))
    })?;

    let accent_oklch = extract_dominant_accent_oklch(&image);
    Ok(DecodedCover {
        ext,
        mime_type,
        width,
        height,
        accent_oklch,
    })
}

/// Extracts a dominant accent color from `image` and formats it as CSS `oklch(L C H)`.
pub fn extract_dominant_accent_oklch(image: &DynamicImage) -> String {
    let thumb = image.resize_exact(32, 32, FilterType::Triangle);

    // Quantize sRGB into 4-bit-per-channel buckets (4096 buckets) and score by frequency + saturation.
    let mut buckets: HashMap<u16, (u32, u64, u64, u64)> = HashMap::new();
    let mut fallback_count: u64 = 0;
    let mut fallback_r: u64 = 0;
    let mut fallback_g: u64 = 0;
    let mut fallback_b: u64 = 0;

    for (_, _, px) in thumb.pixels() {
        let [r, g, b, a] = px.0;
        if a < 128 {
            continue;
        }
        fallback_count += 1;
        fallback_r += u64::from(r);
        fallback_g += u64::from(g);
        fallback_b += u64::from(b);

        let key = (u16::from(r >> 4) << 8) | (u16::from(g >> 4) << 4) | u16::from(b >> 4);
        let entry = buckets.entry(key).or_insert((0, 0, 0, 0));
        entry.0 += 1;
        entry.1 += u64::from(r);
        entry.2 += u64::from(g);
        entry.3 += u64::from(b);
    }

    if fallback_count == 0 {
        return "oklch(0.550 0.000 0.0)".to_string();
    }

    let mut best_score = -1.0_f64;
    let mut best_rgb = (
        (fallback_r / fallback_count) as u8,
        (fallback_g / fallback_count) as u8,
        (fallback_b / fallback_count) as u8,
    );

    for &(count, sum_r, sum_g, sum_b) in buckets.values() {
        let avg_r = (sum_r as f64) / f64::from(count);
        let avg_g = (sum_g as f64) / f64::from(count);
        let avg_b = (sum_b as f64) / f64::from(count);
        let (l, c, _) = srgb_to_oklch(avg_r / 255.0, avg_g / 255.0, avg_b / 255.0);

        // Favor saturated mid-tone accent pixels while keeping pixel frequency dominant for large solids.
        let lightness_weight = if (0.18..=0.88).contains(&l) {
            1.0
        } else {
            0.35
        };
        let chroma_boost = 1.0 + c * 6.0;
        let score = f64::from(count) * chroma_boost * lightness_weight;

        if score > best_score {
            best_score = score;
            best_rgb = (
                avg_r.round().clamp(0.0, 255.0) as u8,
                avg_g.round().clamp(0.0, 255.0) as u8,
                avg_b.round().clamp(0.0, 255.0) as u8,
            );
        }
    }

    let (l, c, h) = srgb_to_oklch(
        f64::from(best_rgb.0) / 255.0,
        f64::from(best_rgb.1) / 255.0,
        f64::from(best_rgb.2) / 255.0,
    );
    let hue = if c < 0.001 { 0.0 } else { h };
    format!(
        "oklch({:.3} {:.3} {:.1})",
        l.clamp(0.0, 1.0),
        c.max(0.0),
        hue
    )
}

/// Converts sRGB `[0.0, 1.0]` to OKLCH `(L, C, H_degrees)` using Björn Ottosson's OKLab matrices.
pub fn srgb_to_oklch(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let lr = srgb_to_linear(r);
    let lg = srgb_to_linear(g);
    let lb = srgb_to_linear(b);

    let l = 0.4122214708 * lr + 0.5363325363 * lg + 0.0514459929 * lb;
    let m = 0.2119034982 * lr + 0.6806995451 * lg + 0.1073969566 * lb;
    let s = 0.0883024619 * lr + 0.2817188376 * lg + 0.6299787005 * lb;

    let l_ = l.cbrt();
    let m_ = m.cbrt();
    let s_ = s.cbrt();

    let ok_l = 0.2104542553 * l_ + 0.7936177850 * m_ - 0.0040720468 * s_;
    let ok_a = 1.9779984951 * l_ - 2.4285922050 * m_ + 0.4505937099 * s_;
    let ok_b = 0.0259040371 * l_ + 0.7827717662 * m_ - 0.8086757660 * s_;

    let c = (ok_a * ok_a + ok_b * ok_b).sqrt();
    let h = ok_b.atan2(ok_a).to_degrees().rem_euclid(360.0);
    (ok_l, c, h)
}

fn srgb_to_linear(c: f64) -> f64 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}
