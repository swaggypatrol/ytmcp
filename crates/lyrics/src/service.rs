use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::warn;
use ytm_core::{BoxFuture, Clock, LyricsPort, SystemClock};
use ytm_protocol::{
    LyricsKind, LyricsMatchMetadata, LyricsQueryRequest, LyricsResult, QueryError, QueryErrorCode,
    VideoId,
};

use crate::cache::{
    CachedLyricsEntry, InMemoryLyricsCache, LYRICS_HIT_TTL_MS, LYRICS_NEGATIVE_TTL_MS,
    LyricsCachePort, build_lyrics_cache_key,
};
use crate::lrc::{ParsedLyrics, parse_lyrics};
use crate::lrclib::{LrclibClient, LrclibConfig, LrclibLookupOutcome};
use crate::matcher::{LyricsCandidate, MatchEvaluation};

/// High-level lyrics coordinator implementing `ytm_core::ports::LyricsPort`.
///
/// Orchestrates:
/// - `enabled` gating (`LyricsKind::Disabled` with zero network requests)
/// - Per-track `user_offset_ms` persistence & `reset_user_offset`
/// - TTL-backed caching via `LyricsCachePort` (30d hits, 24h negative `NotFound`)
/// - Anonymous LRCLIB lookups with version-aware confidence scoring and `CancellationToken` checks
#[derive(Clone)]
pub struct LyricsService {
    client: LrclibClient,
    cache: Arc<dyn LyricsCachePort>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for LyricsService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LyricsService")
            .field("client", &self.client)
            .finish()
    }
}

impl LyricsService {
    pub fn new(config: LrclibConfig) -> Result<Self, QueryError> {
        Self::with_cache_and_clock(
            config,
            Arc::new(InMemoryLyricsCache::new()),
            Arc::new(SystemClock::new()),
        )
    }

    pub fn with_cache(
        config: LrclibConfig,
        cache: Arc<dyn LyricsCachePort>,
    ) -> Result<Self, QueryError> {
        Self::with_cache_and_clock(config, cache, Arc::new(SystemClock::new()))
    }

    pub fn with_cache_and_clock(
        config: LrclibConfig,
        cache: Arc<dyn LyricsCachePort>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, QueryError> {
        let client = LrclibClient::new(config)?;
        Ok(Self {
            client,
            cache,
            clock,
        })
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.client.set_enabled(enabled);
    }

    pub fn is_enabled(&self) -> bool {
        self.client.is_enabled()
    }

    pub fn client(&self) -> &LrclibClient {
        &self.client
    }

    pub fn cache(&self) -> &Arc<dyn LyricsCachePort> {
        &self.cache
    }

    pub async fn get_user_offset(&self, video_id: &VideoId) -> Result<i64, QueryError> {
        Ok(self
            .cache
            .get_user_offset(video_id)
            .await?
            .unwrap_or_default())
    }

    /// Persists a synchronization preference without querying or fetching lyrics.
    pub async fn set_user_offset(
        &self,
        video_id: &VideoId,
        offset_ms: i64,
    ) -> Result<i64, QueryError> {
        self.cache.set_user_offset(video_id, offset_ms).await?;
        Ok(offset_ms)
    }

    /// Removes the saved synchronization preference without querying or fetching lyrics.
    pub async fn reset_user_offset(&self, video_id: &VideoId) -> Result<i64, QueryError> {
        self.cache.reset_user_offset(video_id).await?;
        Ok(0)
    }

    /// Reads the current lyrics and offset. Cache misses may populate the lyrics cache.
    pub async fn read_lyrics(
        &self,
        track: LyricsMatchMetadata,
        cancel: CancellationToken,
    ) -> Result<LyricsResult, QueryError> {
        if cancel.is_cancelled() {
            return Err(cancelled_error());
        }

        if !self.client.is_enabled() {
            return Ok(disabled_result(track.video_id, self.clock.wall_now_ms()));
        }

        let user_offset_ms = match self.get_user_offset(&track.video_id).await {
            Ok(offset) => offset,
            Err(err) => {
                warn!(error = %err.safe_message, "failed to read user lyrics offset");
                0
            }
        };

        if cancel.is_cancelled() {
            return Err(cancelled_error());
        }

        let cache_key = build_lyrics_cache_key("lrclib", &track);
        let now_ms = self.clock.wall_now_ms();

        match self.cache.get_lyrics(&cache_key).await {
            Ok(Some(entry)) if !entry.is_expired(now_ms) => {
                if cancel.is_cancelled() {
                    return Err(cancelled_error());
                }
                let mut result = entry.result;
                result.user_offset_ms = user_offset_ms;
                return Ok(result);
            }
            Ok(_) => {}
            Err(err) => {
                warn!(error = %err.safe_message, "lyrics cache read failed; continuing live fetch");
            }
        }

        let fetch_res = self.client.fetch(&track, cancel.clone()).await;

        if cancel.is_cancelled() {
            return Err(cancelled_error());
        }

        let fetched_at_ms = self.clock.wall_now_ms();

        let (mut result, ttl_ms) = match fetch_res {
            Err(err) if err.code == QueryErrorCode::StaleContext => return Err(err),
            Err(err) => {
                // Transient/network/rate-limit failures degrade to Unavailable and are NOT cached.
                return Ok(unavailable_result(
                    track.video_id,
                    fetched_at_ms,
                    user_offset_ms,
                    err,
                ));
            }
            Ok(LrclibLookupOutcome::Disabled) => {
                return Ok(disabled_result(track.video_id, fetched_at_ms));
            }
            Ok(LrclibLookupOutcome::NotFound { match_basis }) => (
                LyricsResult {
                    video_id: track.video_id.clone(),
                    kind: LyricsKind::NotFound,
                    source: "lrclib".to_string(),
                    language: None,
                    match_basis: Some(match_basis),
                    fetched_at_ms,
                    file_offset_ms: 0,
                    user_offset_ms: 0,
                    synced_lines: Vec::new(),
                    plain_text: None,
                    error: None,
                },
                LYRICS_NEGATIVE_TTL_MS,
            ),
            Ok(LrclibLookupOutcome::Matched(matched)) => {
                let result = build_matched_result(
                    &track,
                    matched.candidate,
                    matched.evaluation,
                    fetched_at_ms,
                );
                let ttl = match result.kind {
                    LyricsKind::NotFound => LYRICS_NEGATIVE_TTL_MS,
                    _ => LYRICS_HIT_TTL_MS,
                };
                (result, ttl)
            }
        };

        let cache_entry = CachedLyricsEntry {
            cache_key,
            result: result.clone(),
            stored_at_ms: fetched_at_ms,
            expires_at_ms: fetched_at_ms.saturating_add(ttl_ms),
        };
        if let Err(err) = self.cache.put_lyrics(cache_entry).await {
            warn!(error = %err.safe_message, "lyrics cache write failed");
        }

        if cancel.is_cancelled() {
            return Err(cancelled_error());
        }

        result.user_offset_ms = user_offset_ms;
        Ok(result)
    }
}

impl LyricsPort for LyricsService {
    fn set_enabled(&self, enabled: bool) {
        LyricsService::set_enabled(self, enabled);
    }

    /// Keep desktop host protocol v1 compatible by applying its optional edit before reading.
    fn query_lyrics(
        &self,
        request: LyricsQueryRequest,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<LyricsResult, QueryError>> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err(cancelled_error());
            }
            if self.is_enabled() {
                if request.reset_user_offset {
                    self.reset_user_offset(&request.track.video_id).await?;
                } else if let Some(offset_ms) = request.set_user_offset_ms {
                    self.set_user_offset(&request.track.video_id, offset_ms)
                        .await?;
                }
            }
            self.read_lyrics(request.track, cancel).await
        })
    }
}

fn build_matched_result(
    track: &LyricsMatchMetadata,
    candidate: LyricsCandidate,
    evaluation: MatchEvaluation,
    fetched_at_ms: u64,
) -> LyricsResult {
    if candidate.instrumental {
        return LyricsResult {
            video_id: track.video_id.clone(),
            kind: LyricsKind::Instrumental,
            source: "lrclib".to_string(),
            language: None,
            match_basis: Some(evaluation.match_basis),
            fetched_at_ms,
            file_offset_ms: 0,
            user_offset_ms: 0,
            synced_lines: Vec::new(),
            plain_text: None,
            error: None,
        };
    }

    if let Some(synced_raw) = candidate
        .synced_lyrics
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        match parse_lyrics(synced_raw) {
            ParsedLyrics::Synced {
                lines,
                file_offset_ms,
                metadata,
            } => {
                return LyricsResult {
                    video_id: track.video_id.clone(),
                    kind: LyricsKind::Synced,
                    source: "lrclib".to_string(),
                    language: metadata.language,
                    match_basis: Some(evaluation.match_basis),
                    fetched_at_ms,
                    file_offset_ms,
                    user_offset_ms: 0,
                    synced_lines: lines,
                    plain_text: None,
                    error: None,
                };
            }
            ParsedLyrics::Plain { text } => {
                return LyricsResult {
                    video_id: track.video_id.clone(),
                    kind: LyricsKind::Plain,
                    source: "lrclib".to_string(),
                    language: None,
                    match_basis: Some(format!(
                        "{}; fallback=plain_no_valid_timestamps",
                        evaluation.match_basis
                    )),
                    fetched_at_ms,
                    file_offset_ms: 0,
                    user_offset_ms: 0,
                    synced_lines: Vec::new(),
                    plain_text: Some(text),
                    error: None,
                };
            }
            ParsedLyrics::Empty => {}
        }
    }

    if let Some(plain_raw) = candidate
        .plain_lyrics
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        let normalized = plain_raw
            .strip_prefix('\u{FEFF}')
            .unwrap_or(plain_raw)
            .replace("\r\n", "\n")
            .replace('\r', "\n");
        return LyricsResult {
            video_id: track.video_id.clone(),
            kind: LyricsKind::Plain,
            source: "lrclib".to_string(),
            language: None,
            match_basis: Some(format!("{}; mode=plain", evaluation.match_basis)),
            fetched_at_ms,
            file_offset_ms: 0,
            user_offset_ms: 0,
            synced_lines: Vec::new(),
            plain_text: Some(normalized),
            error: None,
        };
    }

    LyricsResult {
        video_id: track.video_id.clone(),
        kind: LyricsKind::NotFound,
        source: "lrclib".to_string(),
        language: None,
        match_basis: Some("lrclib: matched candidate contained no usable lyrics".to_string()),
        fetched_at_ms,
        file_offset_ms: 0,
        user_offset_ms: 0,
        synced_lines: Vec::new(),
        plain_text: None,
        error: None,
    }
}

fn disabled_result(video_id: VideoId, fetched_at_ms: u64) -> LyricsResult {
    LyricsResult {
        video_id,
        kind: LyricsKind::Disabled,
        source: "disabled".to_string(),
        language: None,
        match_basis: Some("lrclib disabled by settings".to_string()),
        fetched_at_ms,
        file_offset_ms: 0,
        user_offset_ms: 0,
        synced_lines: Vec::new(),
        plain_text: None,
        error: None,
    }
}

fn unavailable_result(
    video_id: VideoId,
    fetched_at_ms: u64,
    user_offset_ms: i64,
    error: QueryError,
) -> LyricsResult {
    LyricsResult {
        video_id,
        kind: LyricsKind::Unavailable,
        source: "lrclib".to_string(),
        language: None,
        match_basis: None,
        fetched_at_ms,
        file_offset_ms: 0,
        user_offset_ms,
        synced_lines: Vec::new(),
        plain_text: None,
        error: Some(error),
    }
}

fn cancelled_error() -> QueryError {
    QueryError {
        code: QueryErrorCode::StaleContext,
        retriable: false,
        retry_after_ms: None,
        safe_message: "lyrics query cancelled due to track switch".to_string(),
        request_id: None,
    }
}
