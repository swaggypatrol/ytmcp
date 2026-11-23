use std::sync::Arc;
use ytm_core::BoxFuture;
use ytm_lyrics::{CachedLyricsEntry, LyricsCachePort};
use ytm_protocol::{QueryError, VideoId};
use ytm_storage::SqliteStorage;

/// Adapter bridging `ytm-lyrics`'s `LyricsCachePort` onto `SqliteStorage`.
#[derive(Clone)]
pub struct SqliteLyricsCacheAdapter {
    storage: Arc<SqliteStorage>,
}

impl SqliteLyricsCacheAdapter {
    pub fn new(storage: Arc<SqliteStorage>) -> Self {
        Self { storage }
    }
}

impl LyricsCachePort for SqliteLyricsCacheAdapter {
    fn get_lyrics(
        &self,
        cache_key: &str,
    ) -> BoxFuture<'_, Result<Option<CachedLyricsEntry>, QueryError>> {
        let storage = Arc::clone(&self.storage);
        let key_owned = cache_key.to_string();
        Box::pin(async move {
            let cached = storage.get_lyrics_by_key(&key_owned).await?;
            Ok(cached.map(|cv| CachedLyricsEntry {
                cache_key: key_owned,
                result: cv.value,
                stored_at_ms: cv.fetched_at_ms,
                expires_at_ms: cv
                    .fetched_at_ms
                    .saturating_add(cv.ttl_secs.saturating_mul(1_000)),
            }))
        })
    }

    fn put_lyrics(&self, entry: CachedLyricsEntry) -> BoxFuture<'_, Result<(), QueryError>> {
        let storage = Arc::clone(&self.storage);
        Box::pin(async move {
            let ttl_ms = entry.expires_at_ms.saturating_sub(entry.stored_at_ms);
            let ttl_secs = Some((ttl_ms / 1_000).max(1));
            storage
                .put_lyrics_by_key(
                    &entry.cache_key,
                    &entry.result.source,
                    &entry.result,
                    ttl_secs,
                )
                .await
        })
    }

    fn get_user_offset(
        &self,
        video_id: &VideoId,
    ) -> BoxFuture<'_, Result<Option<i64>, QueryError>> {
        let storage = Arc::clone(&self.storage);
        let vid = video_id.clone();
        Box::pin(async move {
            let offset = storage.get_lyrics_user_offset(&vid).await?;
            Ok(Some(offset))
        })
    }

    fn set_user_offset(
        &self,
        video_id: &VideoId,
        offset_ms: i64,
    ) -> BoxFuture<'_, Result<(), QueryError>> {
        let storage = Arc::clone(&self.storage);
        let vid = video_id.clone();
        Box::pin(async move { storage.set_lyrics_user_offset(&vid, offset_ms).await })
    }

    fn reset_user_offset(&self, video_id: &VideoId) -> BoxFuture<'_, Result<(), QueryError>> {
        let storage = Arc::clone(&self.storage);
        let vid = video_id.clone();
        Box::pin(async move { storage.reset_lyrics_user_offset(&vid).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;
    use ytm_core::{MockClock, StoragePort};
    use ytm_lyrics::{LYRICS_HIT_TTL_MS, LrclibConfig, LyricsService, build_lyrics_cache_key};
    use ytm_protocol::{LyricsKind, LyricsLine, LyricsMatchMetadata, LyricsResult, QueryErrorCode};
    use ytm_storage::{CoverCacheConfig, StoragePaths};

    fn test_cache() -> (std::path::PathBuf, SqliteLyricsCacheAdapter) {
        let dir = std::env::temp_dir().join(format!("ytm-daemon-lyrics-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let clock = Arc::new(MockClock::new(10_000));
        let storage = Arc::new(
            SqliteStorage::open(
                StoragePaths::for_root(&dir),
                clock.clone(),
                CoverCacheConfig::production(),
            )
            .unwrap(),
        );
        (dir, SqliteLyricsCacheAdapter::new(storage))
    }

    fn test_metadata() -> LyricsMatchMetadata {
        let vid = VideoId::new("vid-lyrics-1").unwrap();
        LyricsMatchMetadata {
            video_id: vid,
            title: "Song Title".to_string(),
            artist: "Artist Name".to_string(),
            album: Some("Album Name".to_string()),
            duration_ms: Some(210_000),
            version_tag: Some("live".to_string()),
        }
    }

    fn synced_entry(meta: &LyricsMatchMetadata, line: &str) -> CachedLyricsEntry {
        let result = LyricsResult {
            video_id: meta.video_id.clone(),
            kind: LyricsKind::Synced,
            source: "lrclib".to_string(),
            language: Some("en".to_string()),
            match_basis: Some("exact".to_string()),
            fetched_at_ms: 10_000,
            file_offset_ms: 0,
            user_offset_ms: 0,
            synced_lines: vec![LyricsLine {
                time_ms: 5_000,
                text: line.to_string(),
            }],
            plain_text: None,
            error: None,
        };

        CachedLyricsEntry {
            cache_key: build_lyrics_cache_key("lrclib", meta),
            result,
            stored_at_ms: 10_000,
            expires_at_ms: 10_000 + LYRICS_HIT_TTL_MS,
        }
    }

    #[tokio::test]
    async fn lyrics_cache_adapter_round_trips_entries_and_offsets_in_sqlite() {
        let (dir, adapter) = test_cache();
        let meta = test_metadata();
        let vid = meta.video_id.clone();
        let entry = synced_entry(&meta, "Hello");
        let key = entry.cache_key.clone();
        assert!(adapter.get_lyrics(&key).await.unwrap().is_none());
        adapter.put_lyrics(entry).await.unwrap();

        adapter.set_user_offset(&vid, 350).await.unwrap();
        let got = adapter.get_lyrics(&key).await.unwrap().unwrap();
        assert_eq!(got.result.kind, LyricsKind::Synced);
        assert_eq!(got.result.user_offset_ms, 350);
        assert_eq!(adapter.get_user_offset(&vid).await.unwrap(), Some(350));

        adapter.reset_user_offset(&vid).await.unwrap();
        assert_eq!(adapter.get_user_offset(&vid).await.unwrap(), Some(0));

        adapter.storage.clear_cache(None).await.unwrap();
        assert!(adapter.get_lyrics(&key).await.unwrap().is_none());
        drop(adapter);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn offset_operations_propagate_sqlite_write_errors() {
        let dir = std::env::temp_dir().join(format!("ytm-daemon-lyrics-test-{}", Uuid::new_v4()));
        // Open a database without migrations so writes must fail at the actual SQLite boundary.
        let storage = SqliteStorage::open_at_schema_version(
            StoragePaths::for_root(&dir),
            Arc::new(MockClock::new(10_000)),
            CoverCacheConfig::production(),
            0,
        )
        .unwrap();
        let adapter = SqliteLyricsCacheAdapter::new(Arc::new(storage));
        let service =
            LyricsService::with_cache(LrclibConfig::default(), Arc::new(adapter)).unwrap();
        let vid = test_metadata().video_id;
        assert_eq!(
            service.set_user_offset(&vid, 350).await.unwrap_err().code,
            QueryErrorCode::StorageError
        );
        assert_eq!(
            service.reset_user_offset(&vid).await.unwrap_err().code,
            QueryErrorCode::StorageError
        );
        drop(service);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn delimiter_titles_and_version_markers_survive_sqlite_round_trip() {
        let (dir, adapter) = test_cache();
        let base = LyricsMatchMetadata {
            version_tag: None,
            ..test_metadata()
        };
        let cases = [
            ("Song|a=Shift", "End", "embedded separator in title"),
            ("Song", "Shift|a=End", "embedded separator in artist"),
            ("Song - LIVE", "End", "contextual live marker"),
            (
                "Song\t-\tLIVE",
                "End",
                "same normalized title without marker",
            ),
        ];
        for (title, artist, line) in cases {
            let meta = LyricsMatchMetadata {
                title: title.into(),
                artist: artist.into(),
                ..base.clone()
            };
            adapter.put_lyrics(synced_entry(&meta, line)).await.unwrap();
        }
        for (title, artist, line) in cases {
            let meta = LyricsMatchMetadata {
                title: title.into(),
                artist: artist.into(),
                ..base.clone()
            };
            let key = build_lyrics_cache_key("lrclib", &meta);
            let hit = adapter.get_lyrics(&key).await.unwrap().unwrap();
            assert_eq!(hit.result.synced_lines[0].text, line);
        }
        drop(adapter);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
