pub mod cache;
pub mod lrc;
pub mod lrclib;
pub mod matcher;
pub mod service;

pub use cache::{
    CachedLyricsEntry, InMemoryLyricsCache, LYRICS_HIT_TTL_MS, LYRICS_NEGATIVE_TTL_MS,
    LyricsCachePort, LyricsCachePort as LyricsCacheStore, build_lyrics_cache_key,
};
pub use lrc::{LrcMetadata, ParsedLyrics, effective_line_time_ms, parse_lyrics};
pub use lrclib::{
    DEFAULT_LRCLIB_BASE_URL, DEFAULT_MAX_CONCURRENT_REQUESTS, DEFAULT_REQUEST_TIMEOUT,
    DEFAULT_USER_AGENT, LrclibClient, LrclibConfig, LrclibLookupOutcome, MatchedLyrics,
    parse_retry_after_ms,
};
pub use matcher::{
    DEFAULT_MAX_DURATION_DELTA_MS, DEFAULT_MIN_CONFIDENCE_SCORE, LyricsCandidate, MatchConfig,
    MatchEvaluation, MatchRejectionReason, VersionMarker, evaluate_candidate,
    extract_candidate_markers, extract_requested_markers, select_best_candidate,
};
pub use service::LyricsService;

pub fn protocol_version() -> u32 {
    ytm_core::protocol_version()
}
