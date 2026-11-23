use reqwest::{Client, StatusCode};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use tokio::sync::{Semaphore, watch};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};
use url::Url;
use ytm_protocol::{LyricsMatchMetadata, QueryError, QueryErrorCode};

use crate::cache::build_lyrics_cache_key;
use crate::matcher::{
    LyricsCandidate, MatchConfig, MatchEvaluation, evaluate_candidate, select_best_candidate,
};

pub const DEFAULT_LRCLIB_BASE_URL: &str = "https://lrclib.net";
pub const DEFAULT_USER_AGENT: &str = "YTMcp/0.1.0 (https://local.ytmcp.Desktop)";
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
pub const DEFAULT_MAX_CONCURRENT_REQUESTS: usize = 2;
pub const DEFAULT_RETRY_AFTER_MS: u64 = 60_000;

/// Configuration for `LrclibClient`.
#[derive(Debug, Clone)]
pub struct LrclibConfig {
    pub base_url: Url,
    pub user_agent: String,
    pub enabled: bool,
    pub request_timeout: Duration,
    pub max_concurrent_requests: usize,
    pub match_config: MatchConfig,
}

impl Default for LrclibConfig {
    fn default() -> Self {
        Self {
            base_url: Url::parse(DEFAULT_LRCLIB_BASE_URL).expect("valid default LRCLIB URL"),
            user_agent: DEFAULT_USER_AGENT.to_string(),
            enabled: true,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            max_concurrent_requests: DEFAULT_MAX_CONCURRENT_REQUESTS,
            match_config: MatchConfig::default(),
        }
    }
}

impl LrclibConfig {
    pub fn with_base_url(base_url: &str) -> Result<Self, url::ParseError> {
        Ok(Self {
            base_url: Url::parse(base_url)?,
            ..Self::default()
        })
    }
}

/// Matched candidate and evaluation details returned by `LrclibClient::fetch`.
#[derive(Debug, Clone)]
pub struct MatchedLyrics {
    pub candidate: LyricsCandidate,
    pub evaluation: MatchEvaluation,
}

/// Outcome of a single LRCLIB lookup (hit, rejected/not-found, or disabled).
#[derive(Debug, Clone)]
pub enum LrclibLookupOutcome {
    Disabled,
    Matched(Box<MatchedLyrics>),
    NotFound { match_basis: String },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LrclibTrackDto {
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    track_name: Option<String>,
    // Current responses can carry both names. Older responses can provide only `name`.
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    artist_name: Option<String>,
    #[serde(default)]
    album_name: Option<String>,
    #[serde(default)]
    duration: Option<f64>,
    #[serde(default)]
    instrumental: bool,
    #[serde(default)]
    plain_lyrics: Option<String>,
    #[serde(default)]
    synced_lyrics: Option<String>,
}

impl LrclibTrackDto {
    fn into_candidate(self, fallback_title: &str, fallback_artist: &str) -> LyricsCandidate {
        let duration_ms = self
            .duration
            .filter(|d| d.is_finite() && *d >= 0.0)
            .map(|secs| (secs * 1000.0).round() as u64);
        LyricsCandidate {
            id: self.id,
            track_name: self
                .track_name
                .or(self.name)
                .unwrap_or_else(|| fallback_title.to_string()),
            artist_name: self
                .artist_name
                .unwrap_or_else(|| fallback_artist.to_string()),
            album_name: self.album_name,
            duration_ms,
            instrumental: self.instrumental,
            synced_lyrics: self.synced_lyrics,
            plain_lyrics: self.plain_lyrics,
        }
    }
}

type InFlightValue = Option<Result<LrclibLookupOutcome, QueryError>>;

struct InFlightGuard {
    key: String,
    in_flight: Arc<Mutex<HashMap<String, watch::Receiver<InFlightValue>>>>,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        if let Ok(mut map) = self.in_flight.lock() {
            map.remove(&self.key);
        }
    }
}

/// Anonymous LRCLIB HTTP client with single-flight deduplication and bounded concurrency.
#[derive(Clone)]
pub struct LrclibClient {
    http: Client,
    base_url: Url,
    enabled: Arc<AtomicBool>,
    semaphore: Arc<Semaphore>,
    match_config: MatchConfig,
    in_flight: Arc<Mutex<HashMap<String, watch::Receiver<InFlightValue>>>>,
    /// After an HTTP 429 no request is sent until this instant (honors `Retry-After` globally).
    cooldown_until: Arc<Mutex<Option<std::time::Instant>>>,
}

impl std::fmt::Debug for LrclibClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LrclibClient")
            .field("base_url", &self.base_url.as_str())
            .field("enabled", &self.is_enabled())
            .finish()
    }
}

impl LrclibClient {
    pub fn new(config: LrclibConfig) -> Result<Self, QueryError> {
        // `reqwest` is built without the `cookies` feature; no cookie store or auth headers are used.
        let http = Client::builder()
            .user_agent(config.user_agent)
            .timeout(config.request_timeout)
            .build()
            .map_err(|_| QueryError {
                code: QueryErrorCode::UnsupportedCapability,
                retriable: false,
                retry_after_ms: None,
                safe_message: "failed to initialize anonymous LRCLIB HTTP client".to_string(),
                request_id: None,
            })?;

        let concurrency = config.max_concurrent_requests.max(1);
        Ok(Self {
            http,
            base_url: config.base_url,
            enabled: Arc::new(AtomicBool::new(config.enabled)),
            semaphore: Arc::new(Semaphore::new(concurrency)),
            match_config: config.match_config,
            in_flight: Arc::new(Mutex::new(HashMap::new())),
            cooldown_until: Arc::new(Mutex::new(None)),
        })
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::SeqCst);
    }

    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    /// Queries LRCLIB (`/api/get` then cautious `/api/search` fallback) with single-flight
    /// deduplication, bounded concurrency, anonymous access only, and cancellation support.
    pub async fn fetch(
        &self,
        track: &LyricsMatchMetadata,
        cancel: CancellationToken,
    ) -> Result<LrclibLookupOutcome, QueryError> {
        if !self.is_enabled() {
            return Ok(LrclibLookupOutcome::Disabled);
        }
        if cancel.is_cancelled() {
            return Err(cancelled_error());
        }
        if let Some(remaining) = self.cooldown_remaining() {
            return Err(QueryError {
                code: QueryErrorCode::RateLimited,
                retriable: true,
                retry_after_ms: Some(u64::try_from(remaining.as_millis()).unwrap_or(u64::MAX)),
                safe_message: "LRCLIB rate-limit cooldown active".to_string(),
                request_id: None,
            });
        }

        let flight_key = build_lyrics_cache_key("lrclib", track);
        let mut rx = {
            let mut map = self.in_flight.lock().expect("in_flight lock poisoned");
            if let Some(existing) = map.get(&flight_key) {
                existing.clone()
            } else {
                let (tx, rx) = watch::channel(None);
                map.insert(flight_key.clone(), rx.clone());
                self.spawn_lookup(flight_key, track.clone(), tx);
                rx
            }
        };

        loop {
            if let Some(result) = rx.borrow().clone() {
                return result;
            }
            tokio::select! {
                _ = cancel.cancelled() => {
                    return Err(cancelled_error());
                }
                changed = rx.changed() => {
                    if changed.is_err() {
                        return Err(QueryError {
                            code: QueryErrorCode::Busy,
                            retriable: true,
                            retry_after_ms: Some(200),
                            safe_message: "LRCLIB single-flight request terminated".to_string(),
                            request_id: None,
                        });
                    }
                }
            }
        }
    }

    fn cooldown_remaining(&self) -> Option<std::time::Duration> {
        let guard = self.cooldown_until.lock().ok()?;
        let until = (*guard)?;
        until.checked_duration_since(std::time::Instant::now())
    }

    fn spawn_lookup(
        &self,
        flight_key: String,
        track: LyricsMatchMetadata,
        tx: watch::Sender<InFlightValue>,
    ) {
        let http = self.http.clone();
        let base_url = self.base_url.clone();
        let semaphore = self.semaphore.clone();
        let match_config = self.match_config;
        let in_flight = self.in_flight.clone();
        let enabled = self.enabled.clone();
        let cooldown_until = self.cooldown_until.clone();

        tokio::spawn(async move {
            let _guard = InFlightGuard {
                key: flight_key,
                in_flight,
            };

            let outcome = tokio::select! {
                _ = tx.closed() => {
                    debug!("all callers cancelled LRCLIB lookup; aborting flight");
                    return;
                }
                res = execute_lookup(http, base_url, semaphore, match_config, enabled, track) => res,
            };

            if let Err(err) = &outcome
                && err.code == QueryErrorCode::RateLimited
                && let Ok(mut until) = cooldown_until.lock()
            {
                let wait = std::time::Duration::from_millis(err.retry_after_ms.unwrap_or(60_000));
                *until = Some(std::time::Instant::now() + wait);
            }
            let _ = tx.send(Some(outcome));
        });
    }
}

async fn execute_lookup(
    http: Client,
    base_url: Url,
    semaphore: Arc<Semaphore>,
    match_config: MatchConfig,
    enabled: Arc<AtomicBool>,
    track: LyricsMatchMetadata,
) -> Result<LrclibLookupOutcome, QueryError> {
    if !enabled.load(Ordering::SeqCst) {
        return Ok(LrclibLookupOutcome::Disabled);
    }

    let _permit = semaphore.acquire_owned().await.map_err(|_| QueryError {
        code: QueryErrorCode::Busy,
        retriable: true,
        retry_after_ms: Some(100),
        safe_message: "LRCLIB concurrency limiter closed".to_string(),
        request_id: None,
    })?;

    if !enabled.load(Ordering::SeqCst) {
        return Ok(LrclibLookupOutcome::Disabled);
    }

    let mut get_rejection_basis: Option<String> = None;

    match fetch_get(&http, &base_url, &track).await? {
        Some(dto) => {
            let candidate = dto.into_candidate(&track.title, &track.artist);
            let eval = evaluate_candidate(&track, &candidate, "lrclib:get", match_config);
            if eval.accepted {
                return Ok(LrclibLookupOutcome::Matched(Box::new(MatchedLyrics {
                    candidate,
                    evaluation: eval,
                })));
            }
            get_rejection_basis = Some(eval.match_basis);
        }
        None => {
            debug!("LRCLIB /api/get returned 404; trying cautious /api/search fallback");
        }
    }

    // Cautious search fallback when /api/get returned 404 or a rejected version/duration candidate.
    let search_dtos = fetch_search(&http, &base_url, &track).await?;
    if search_dtos.is_empty() {
        return Ok(LrclibLookupOutcome::NotFound {
            match_basis: get_rejection_basis
                .unwrap_or_else(|| "lrclib: no results from /api/get or /api/search".to_string()),
        });
    }

    let candidates: Vec<LyricsCandidate> = search_dtos
        .into_iter()
        .map(|dto| dto.into_candidate(&track.title, &track.artist))
        .collect();

    match select_best_candidate(&track, &candidates, "lrclib:search", match_config) {
        Ok((idx, eval)) => Ok(LrclibLookupOutcome::Matched(Box::new(MatchedLyrics {
            candidate: candidates[idx].clone(),
            evaluation: eval,
        }))),
        Err(search_basis) => Ok(LrclibLookupOutcome::NotFound {
            match_basis: match get_rejection_basis {
                Some(get_basis) => format!("{get_basis}; {search_basis}"),
                None => search_basis,
            },
        }),
    }
}

async fn fetch_get(
    http: &Client,
    base_url: &Url,
    track: &LyricsMatchMetadata,
) -> Result<Option<LrclibTrackDto>, QueryError> {
    let mut url = join_api_path(base_url, "api/get")?;
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("track_name", &track.title);
        pairs.append_pair("artist_name", &track.artist);
        if let Some(album) = &track.album
            && !album.trim().is_empty()
        {
            pairs.append_pair("album_name", album);
        }
        if let Some(duration_ms) = track.duration_ms {
            let secs = (duration_ms.saturating_add(500)) / 1_000;
            pairs.append_pair("duration", &secs.to_string());
        }
    }

    let resp = http.get(url).send().await.map_err(map_reqwest_error)?;
    let status = resp.status();
    if status == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        return Err(rate_limited_error(resp.headers().get("retry-after")));
    }
    if !status.is_success() {
        return Err(http_status_error(status));
    }

    let dto = resp
        .json::<LrclibTrackDto>()
        .await
        .map_err(|_| QueryError {
            code: QueryErrorCode::InvalidPayload,
            retriable: false,
            retry_after_ms: None,
            safe_message: "LRCLIB /api/get returned invalid JSON payload".to_string(),
            request_id: None,
        })?;
    Ok(Some(dto))
}

async fn fetch_search(
    http: &Client,
    base_url: &Url,
    track: &LyricsMatchMetadata,
) -> Result<Vec<LrclibTrackDto>, QueryError> {
    let mut url = join_api_path(base_url, "api/search")?;
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("track_name", &track.title);
        pairs.append_pair("artist_name", &track.artist);
    }

    let resp = http.get(url).send().await.map_err(map_reqwest_error)?;
    let status = resp.status();
    if status == StatusCode::NOT_FOUND {
        return Ok(Vec::new());
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        return Err(rate_limited_error(resp.headers().get("retry-after")));
    }
    if !status.is_success() {
        return Err(http_status_error(status));
    }

    let list = resp
        .json::<Vec<LrclibTrackDto>>()
        .await
        .map_err(|_| QueryError {
            code: QueryErrorCode::InvalidPayload,
            retriable: false,
            retry_after_ms: None,
            safe_message: "LRCLIB /api/search returned invalid JSON payload".to_string(),
            request_id: None,
        })?;
    Ok(list)
}

fn join_api_path(base_url: &Url, path: &str) -> Result<Url, QueryError> {
    let mut normalized_base = base_url.clone();
    if !normalized_base.path().ends_with('/') {
        let with_slash = format!("{}/", normalized_base.path());
        normalized_base.set_path(&with_slash);
    }
    normalized_base.join(path).map_err(|_| QueryError {
        code: QueryErrorCode::InvalidPayload,
        retriable: false,
        retry_after_ms: None,
        safe_message: "invalid LRCLIB endpoint URL".to_string(),
        request_id: None,
    })
}

fn rate_limited_error(retry_after: Option<&reqwest::header::HeaderValue>) -> QueryError {
    let retry_after_ms = retry_after
        .and_then(|v| v.to_str().ok())
        .and_then(parse_retry_after_ms)
        .unwrap_or(DEFAULT_RETRY_AFTER_MS);
    warn!(retry_after_ms, "LRCLIB returned HTTP 429 Too Many Requests");
    QueryError {
        code: QueryErrorCode::RateLimited,
        retriable: true,
        retry_after_ms: Some(retry_after_ms),
        safe_message: "LRCLIB rate limit exceeded".to_string(),
        request_id: None,
    }
}

/// Parses `Retry-After` header values in integer seconds or IMF-fixdate format into milliseconds.
pub fn parse_retry_after_ms(raw: &str) -> Option<u64> {
    let trimmed = raw.trim();
    if let Ok(seconds) = trimmed.parse::<u64>() {
        return Some(seconds.saturating_mul(1_000));
    }
    parse_imf_fixdate_delta_ms(trimmed, SystemTime::now())
}

fn parse_imf_fixdate_delta_ms(date_str: &str, now: SystemTime) -> Option<u64> {
    // Format: "Wed, 21 Oct 2015 07:28:00 GMT"
    let (_, rest) = date_str.split_once(',')?;
    let parts: Vec<&str> = rest.split_whitespace().collect();
    if parts.len() != 5 || parts[4] != "GMT" {
        return None;
    }
    let day: i64 = parts[0].parse().ok()?;
    let month: i64 = match parts[1] {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year: i64 = parts[2].parse().ok()?;
    let time_parts: Vec<&str> = parts[3].split(':').collect();
    if time_parts.len() != 3 {
        return None;
    }
    let hour: i64 = time_parts[0].parse().ok()?;
    let minute: i64 = time_parts[1].parse().ok()?;
    let second: i64 = time_parts[2].parse().ok()?;
    if !(1..=31).contains(&day)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=60).contains(&second)
    {
        return None;
    }

    let days = days_from_civil(year, month, day);
    let target_secs = days
        .checked_mul(86_400)?
        .checked_add(hour * 3_600 + minute * 60 + second)?;
    if target_secs < 0 {
        return None;
    }
    let now_secs = now.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_secs();
    let target_u64 = target_secs as u64;
    Some(target_u64.saturating_sub(now_secs).saturating_mul(1_000))
}

// Howard Hinnant's civil-from-days inverse algorithm (days since 1970-01-01).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn http_status_error(status: StatusCode) -> QueryError {
    let retriable = status.is_server_error() || status == StatusCode::REQUEST_TIMEOUT;
    let code = if status == StatusCode::REQUEST_TIMEOUT || status == StatusCode::GATEWAY_TIMEOUT {
        QueryErrorCode::Timeout
    } else if status.is_server_error() {
        QueryErrorCode::Busy
    } else {
        QueryErrorCode::NetworkOffline
    };
    QueryError {
        code,
        retriable,
        retry_after_ms: if retriable { Some(2_000) } else { None },
        safe_message: format!("LRCLIB returned HTTP {}", status.as_u16()),
        request_id: None,
    }
}

fn map_reqwest_error(err: reqwest::Error) -> QueryError {
    if err.is_timeout() {
        QueryError {
            code: QueryErrorCode::Timeout,
            retriable: true,
            retry_after_ms: Some(1_000),
            safe_message: "LRCLIB request timed out".to_string(),
            request_id: None,
        }
    } else {
        QueryError {
            code: QueryErrorCode::NetworkOffline,
            retriable: true,
            retry_after_ms: Some(2_000),
            safe_message: "LRCLIB network request failed".to_string(),
            request_id: None,
        }
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
