use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use ytm_control_api::LyricsOffsetAction;
use ytm_core::{BoxFuture, control::lyrics::ControlLyrics};
use ytm_lyrics::LyricsService;
use ytm_protocol::{LyricsMatchMetadata, LyricsResult, QueryError, VideoId};

pub struct DaemonControlLyrics {
    service: Arc<LyricsService>,
}

impl DaemonControlLyrics {
    pub fn new(service: Arc<LyricsService>) -> Self {
        Self { service }
    }
}

impl ControlLyrics for DaemonControlLyrics {
    fn read_lrclib(
        &self,
        track: LyricsMatchMetadata,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<LyricsResult, QueryError>> {
        Box::pin(self.service.read_lyrics(track, cancel))
    }

    fn user_offset(&self, video_id: VideoId) -> BoxFuture<'_, Result<i64, QueryError>> {
        Box::pin(async move { self.service.get_user_offset(&video_id).await })
    }

    fn change_offset(
        &self,
        video_id: VideoId,
        action: LyricsOffsetAction,
    ) -> BoxFuture<'_, Result<i64, QueryError>> {
        Box::pin(async move {
            match action {
                LyricsOffsetAction::Set { offset_ms } => {
                    self.service.set_user_offset(&video_id, offset_ms).await
                }
                LyricsOffsetAction::Reset => self.service.reset_user_offset(&video_id).await,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics_cache::SqliteLyricsCacheAdapter;
    use std::path::Path;
    use ytm_core::MockClock;
    use ytm_lyrics::LrclibConfig;
    use ytm_protocol::QueryErrorCode;
    use ytm_storage::{CoverCacheConfig, SqliteStorage, StoragePaths};

    fn adapter(storage: SqliteStorage) -> DaemonControlLyrics {
        let config = LrclibConfig {
            enabled: false,
            ..LrclibConfig::default()
        };
        let cache = Arc::new(SqliteLyricsCacheAdapter::new(Arc::new(storage)));
        DaemonControlLyrics::new(Arc::new(LyricsService::with_cache(config, cache).unwrap()))
    }

    fn reopened(path: &Path) -> DaemonControlLyrics {
        adapter(
            SqliteStorage::open(
                StoragePaths::for_root(path),
                Arc::new(MockClock::new(10_000)),
                CoverCacheConfig::production(),
            )
            .unwrap(),
        )
    }

    #[tokio::test]
    async fn offset_changes_survive_reopen_without_looking_up_lyrics() {
        let dir = std::env::temp_dir().join(format!("ytm-control-lyrics-{}", uuid::Uuid::new_v4()));
        let video = VideoId::new("fixture-song").unwrap();
        let lyrics = reopened(&dir);
        let value = lyrics
            .change_offset(video.clone(), LyricsOffsetAction::Set { offset_ms: -250 })
            .await
            .unwrap();
        assert_eq!(value, -250);
        drop(lyrics);
        let lyrics = reopened(&dir);
        assert_eq!(lyrics.user_offset(video.clone()).await.unwrap(), -250);
        lyrics
            .change_offset(video.clone(), LyricsOffsetAction::Reset)
            .await
            .unwrap();
        drop(lyrics);
        let lyrics = reopened(&dir);
        assert_eq!(lyrics.user_offset(video).await.unwrap(), 0);
        drop(lyrics);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn offset_does_not_acknowledge_failed_sqlite_write() {
        let dir = std::env::temp_dir().join(format!("ytm-control-lyrics-{}", uuid::Uuid::new_v4()));
        let lyrics = adapter(
            SqliteStorage::open_at_schema_version(
                StoragePaths::for_root(&dir),
                Arc::new(MockClock::new(10_000)),
                CoverCacheConfig::production(),
                0,
            )
            .unwrap(),
        );
        let video = VideoId::new("fixture-song").unwrap();
        let error = lyrics
            .change_offset(video, LyricsOffsetAction::Set { offset_ms: 250 })
            .await
            .unwrap_err();
        assert_eq!(error.code, QueryErrorCode::StorageError);
        drop(lyrics);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
