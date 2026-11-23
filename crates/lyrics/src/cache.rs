use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;
use ytm_core::{BoxFuture, Clock};
use ytm_protocol::{LyricsMatchMetadata, LyricsResult, QueryError, VideoId};

use crate::matcher::extract_requested_markers;

/// Cache TTL for positive lyrics hits (`Synced`, `Plain`, `Instrumental`): 30 days.
pub const LYRICS_HIT_TTL_MS: u64 = 30 * 24 * 60 * 60 * 1_000;

/// Cache TTL for negative lyrics lookups (`NotFound`): 24 hours.
pub const LYRICS_NEGATIVE_TTL_MS: u64 = 24 * 60 * 60 * 1_000;

/// Serialized cache entry stored by `LyricsCachePort`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedLyricsEntry {
    pub cache_key: String,
    pub result: LyricsResult,
    pub stored_at_ms: u64,
    pub expires_at_ms: u64,
}

impl CachedLyricsEntry {
    pub fn is_expired(&self, wall_now_ms: u64) -> bool {
        wall_now_ms >= self.expires_at_ms
    }
}

/// Narrow storage abstraction for persistent lyrics caching and per-track user offsets.
///
/// Implemented by `ytm-storage` / `ytm-daemon` (T11) or `InMemoryLyricsCache` for offline tests.
pub trait LyricsCachePort: Send + Sync {
    fn get_lyrics(
        &self,
        cache_key: &str,
    ) -> BoxFuture<'_, Result<Option<CachedLyricsEntry>, QueryError>>;

    fn put_lyrics(&self, entry: CachedLyricsEntry) -> BoxFuture<'_, Result<(), QueryError>>;

    fn get_user_offset(&self, video_id: &VideoId)
    -> BoxFuture<'_, Result<Option<i64>, QueryError>>;

    fn set_user_offset(
        &self,
        video_id: &VideoId,
        offset_ms: i64,
    ) -> BoxFuture<'_, Result<(), QueryError>>;

    fn reset_user_offset(&self, video_id: &VideoId) -> BoxFuture<'_, Result<(), QueryError>>;
}

/// Builds a deterministic cache key incorporating provider, video ID, normalized title/artist/album,
/// duration, explicit version tag, and extracted version markers.
///
/// Updating version metadata or duration on the same `video_id` produces a distinct cache key so
/// stale mismatched entries are not reused.
pub fn build_lyrics_cache_key(provider: &str, track: &LyricsMatchMetadata) -> String {
    let markers: Vec<_> = extract_requested_markers(track)
        .into_iter()
        .map(|marker| marker.as_str())
        .collect();
    serde_json::json!([
        "v2",
        normalize_key_part(provider),
        track.video_id.as_str(),
        normalize_key_part(&track.title),
        normalize_key_part(&track.artist),
        track.album.as_deref().map(normalize_key_part),
        track.duration_ms,
        track.version_tag.as_deref().map(normalize_key_part),
        markers,
    ])
    .to_string()
}

fn normalize_key_part(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// In-memory `LyricsCachePort` implementation useful for standalone services and deterministic tests.
#[derive(Debug, Default)]
pub struct InMemoryLyricsCache {
    entries: Mutex<HashMap<String, CachedLyricsEntry>>,
    user_offsets: Mutex<HashMap<String, i64>>,
}

impl InMemoryLyricsCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn entry_count(&self) -> usize {
        self.entries.lock().expect("entries lock poisoned").len()
    }

    pub fn purge_expired(&self, clock: &dyn Clock) {
        let now = clock.wall_now_ms();
        let mut guard = self.entries.lock().expect("entries lock poisoned");
        guard.retain(|_, entry| !entry.is_expired(now));
    }
}

impl LyricsCachePort for InMemoryLyricsCache {
    fn get_lyrics(
        &self,
        cache_key: &str,
    ) -> BoxFuture<'_, Result<Option<CachedLyricsEntry>, QueryError>> {
        let key = cache_key.to_string();
        Box::pin(async move {
            let guard = self.entries.lock().expect("entries lock poisoned");
            Ok(guard.get(&key).cloned())
        })
    }

    fn put_lyrics(&self, entry: CachedLyricsEntry) -> BoxFuture<'_, Result<(), QueryError>> {
        Box::pin(async move {
            let mut guard = self.entries.lock().expect("entries lock poisoned");
            guard.insert(entry.cache_key.clone(), entry);
            Ok(())
        })
    }

    fn get_user_offset(
        &self,
        video_id: &VideoId,
    ) -> BoxFuture<'_, Result<Option<i64>, QueryError>> {
        let vid = video_id.as_str().to_string();
        Box::pin(async move {
            let guard = self.user_offsets.lock().expect("offsets lock poisoned");
            Ok(guard.get(&vid).copied())
        })
    }

    fn set_user_offset(
        &self,
        video_id: &VideoId,
        offset_ms: i64,
    ) -> BoxFuture<'_, Result<(), QueryError>> {
        let vid = video_id.as_str().to_string();
        Box::pin(async move {
            let mut guard = self.user_offsets.lock().expect("offsets lock poisoned");
            guard.insert(vid, offset_ms);
            Ok(())
        })
    }

    fn reset_user_offset(&self, video_id: &VideoId) -> BoxFuture<'_, Result<(), QueryError>> {
        let vid = video_id.as_str().to_string();
        Box::pin(async move {
            let mut guard = self.user_offsets.lock().expect("offsets lock poisoned");
            guard.remove(&vid);
            Ok(())
        })
    }
}
