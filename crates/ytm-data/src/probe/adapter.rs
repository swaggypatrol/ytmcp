use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::fmt;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::sync::{Notify, Semaphore};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};
use ytm_core::ports::{
    BoxFuture, CatalogIdentity, CatalogPort, Clock, OfficialLyrics, SystemClock,
};
use ytm_protocol::{
    AccountEpoch, AccountScope, AlbumId, ArtistId, CatalogQueryKind, CatalogQueryRequest,
    CatalogResponsePayload, CoverRef, CredentialRevision, Freshness, HomeFeed, MAX_PAGE_SIZE,
    MutationKind, MutationOutcome, MutationRequest, MutationResult, PagedResult, PlaylistDetail,
    PlaylistId, PlaylistSummary, QueryError, QueryErrorCode, RecentHistoryResult, SearchResult,
    SessionMaterialPayload, Track, VideoId,
};
use ytm_storage::{AccountCacheContext, LibraryMembershipCache, SqliteStorage};

use crate::auth::{account_hint_for, account_scope_for, derive_account_scope};
use crate::probe::backend::{
    BackendDiscoveryPage, BackendExtendedPage, CatalogBackend, CatalogMutationError,
    CatalogSession, DiscoverySurface, ExtendedMediaQuery, LibrarySurface, PROVIDER_VERSION,
    PlaylistMetadata, PlaylistMutation, PlaylistMutationAck, PlaylistOccurrence,
    ProviderDiscoveryItem, ProviderDiscoveryTarget, ProviderExtendedItem, ProviderLibraryItem,
    SongRating, YtmapiCatalogBackend, scoped_mood_category_id,
};
use crate::probe::cursor::{CursorKind, CursorPosition, decode_cursor, encode_cursor};
use crate::probe::error::make_query_error;
use crate::probe::normalize::{HOME_FALLBACK_NOTICE, SearchOverview, is_system_library_playlist};

mod typed_search;
pub use typed_search::ProviderTypedSearchPage;
use typed_search::TypedSearchWindow;

#[derive(Debug, Clone)]
pub struct YtmCatalogConfig {
    pub locale: String,
    pub max_concurrent_requests: usize,
    pub request_timeout: Duration,
}

#[derive(Debug, Clone, Copy)]
pub struct MusicPageRequest<'a> {
    pub cursor: Option<&'a str>,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreshPlaylistPage {
    /// Metadata is present only when the request reads the first upstream page.
    pub metadata: Option<PlaylistMetadata>,
    pub entries: Vec<PlaylistOccurrence>,
    pub next_cursor: Option<String>,
    /// Every provider row was represented and no additional pages remain.
    pub complete: bool,
    pub captured_at_ms: u64,
}

pub const SONG_RADIO_SOURCE: &str = "youtube_music_song_radio";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SongRadioPage {
    pub seed: VideoId,
    pub candidates: Vec<Track>,
    pub next_cursor: Option<String>,
    pub captured_at_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SongRatingEvidence {
    pub rating: SongRating,
    pub observed_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderDiscoveryPage {
    pub items: Vec<ProviderDiscoveryItem>,
    pub next_cursor: Option<String>,
    pub complete: bool,
    pub captured_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSearchSuggestions {
    pub items: Vec<String>,
    pub complete: bool,
    pub captured_at_ms: u64,
}

pub const MOOD_PLAYLISTS_SOURCE: &str = "youtube_music_mood_playlists";
pub const SEARCH_SUGGESTIONS_SOURCE: &str = "youtube_music_search_suggestions";

enum DiscoveryRead<'a> {
    Surface(DiscoverySurface),
    MoodCategory(&'a str),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderExtendedPage {
    pub entity: Option<ProviderExtendedItem>,
    pub items: Vec<ProviderExtendedItem>,
    pub next_cursor: Option<String>,
    pub complete: bool,
    pub captured_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderLibraryPage {
    pub items: Vec<ProviderLibraryItem>,
    pub next_cursor: Option<String>,
    pub complete: bool,
    pub captured_at_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SongMembershipEvidence {
    pub in_library: Option<bool>,
    pub observed_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumMembershipEvidence {
    pub in_library: Option<bool>,
    pub observed_at_ms: u64,
    pub playlist_id: PlaylistId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArtistSubscriptionEvidence {
    pub subscribed: Option<bool>,
    pub observed_at_ms: u64,
}

// Leaves room in the 64 KiB control content budget for the response envelope and opaque cursor.
const MAX_EXTENDED_PAGE_BYTES: usize = 44 * 1024;
const DISCOVERY_WINDOW_TTL_MS: u64 = 120_000;
const DISCOVERY_WINDOW_LIMIT: usize = 32;

impl Default for YtmCatalogConfig {
    fn default() -> Self {
        Self {
            locale: "en-US".to_string(),
            max_concurrent_requests: 4,
            request_timeout: Duration::from_secs(15),
        }
    }
}

#[derive(Clone)]
struct ActiveSession {
    session: Arc<dyn CatalogSession>,
    account_epoch: AccountEpoch,
    account_scope: AccountScope,
    account_hint: Option<String>,
    credential_revision: CredentialRevision,
    session_cancel: CancellationToken,
}

struct AdapterSessionState {
    active: Option<ActiveSession>,
    network_offline: bool,
    recent_history_cache: Option<RecentHistoryResult>,
    discovery_windows: VecDeque<DiscoveryWindow>,
    typed_search_windows: VecDeque<TypedSearchWindow>,
}

struct DiscoveryWindow {
    cursor: String,
    page: Arc<BackendDiscoveryPage>,
    captured_at_ms: u64,
    expires_at_ms: u64,
}

/// Formats `SessionMaterialPayload::cookie_pairs` into an HTTP `Cookie` header string in memory.
///
/// Every `name=value` pair is terminated with `;` (e.g. `k=v;`) so `ytmapi-rs`'s
/// `BrowserToken::from_str` (which parses `SAPISID` using `.split_once("SAPISID=").1.split_once(';')`)
/// succeeds even when `SAPISID` sorts last in `BTreeMap`.
pub fn format_cookie_header(cookie_pairs: &BTreeMap<String, String>) -> Result<String, QueryError> {
    let sapisid = cookie_pairs
        .get("SAPISID")
        .map(String::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            make_query_error(
                QueryErrorCode::AuthRequired,
                false,
                None,
                "SAPISID cookie is missing from session material",
            )
        })?;
    if sapisid.contains(';') || sapisid.chars().any(char::is_control) {
        return Err(make_query_error(
            QueryErrorCode::AuthRequired,
            false,
            None,
            "SAPISID cookie value contains invalid characters",
        ));
    }

    let mut header = String::new();
    for (name, value) in cookie_pairs {
        let k = name.trim();
        let v = value.trim();
        if k.is_empty()
            || k.contains([';', '=', '\r', '\n'])
            || k.chars().any(char::is_control)
            || v.contains([';', '\r', '\n'])
            || v.chars().any(char::is_control)
        {
            return Err(make_query_error(
                QueryErrorCode::AuthRequired,
                false,
                None,
                "cookie pair contains invalid header characters",
            ));
        }
        if !header.is_empty() {
            header.push(' ');
        }
        header.push_str(k);
        header.push('=');
        header.push_str(v);
        header.push(';');
    }
    Ok(header)
}

pub struct YtmCatalogAdapter {
    storage: Arc<SqliteStorage>,
    backend: Arc<dyn CatalogBackend>,
    clock: Arc<dyn Clock>,
    config: YtmCatalogConfig,
    semaphore: Arc<Semaphore>,
    state: Arc<RwLock<AdapterSessionState>>,
    network_failures: Arc<Notify>,
    session_commit: tokio::sync::Mutex<()>,
    catalog_cache_generation: tokio::sync::Mutex<u64>,
}

impl fmt::Debug for YtmCatalogAdapter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let guard = self.state.read().ok();
        let (has_session, epoch, scope, cred_rev, offline) = match guard.as_deref() {
            Some(s) => (
                s.active.is_some(),
                s.active.as_ref().map(|a| a.account_epoch.clone()),
                s.active.as_ref().map(|a| a.account_scope.clone()),
                s.active.as_ref().map(|a| a.credential_revision.clone()),
                s.network_offline,
            ),
            None => (false, None, None, None, false),
        };
        f.debug_struct("YtmCatalogAdapter")
            .field("provider", &PROVIDER_VERSION)
            .field("locale", &self.config.locale)
            .field("has_session", &has_session)
            .field("account_epoch", &epoch)
            .field("account_scope", &scope)
            .field("credential_revision", &cred_rev)
            .field("network_offline", &offline)
            .field("cookies", &"<redacted>")
            .finish()
    }
}

impl YtmCatalogAdapter {
    pub fn new(storage: Arc<SqliteStorage>) -> Result<Self, QueryError> {
        Self::with_config(
            storage,
            YtmCatalogConfig::default(),
            Arc::new(SystemClock::new()),
        )
    }

    pub fn with_config(
        storage: Arc<SqliteStorage>,
        config: YtmCatalogConfig,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, QueryError> {
        let backend = Arc::new(YtmapiCatalogBackend::new(config.request_timeout)?);
        Ok(Self::with_backend(storage, backend, config, clock))
    }

    pub fn with_backend(
        storage: Arc<SqliteStorage>,
        backend: Arc<dyn CatalogBackend>,
        config: YtmCatalogConfig,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let permits = config.max_concurrent_requests.max(1);
        Self {
            storage,
            backend,
            clock,
            config,
            semaphore: Arc::new(Semaphore::new(permits)),
            state: Arc::new(RwLock::new(AdapterSessionState {
                active: None,
                network_offline: false,
                recent_history_cache: None,
                discovery_windows: VecDeque::new(),
                typed_search_windows: VecDeque::new(),
            })),
            network_failures: Arc::new(Notify::new()),
            session_commit: tokio::sync::Mutex::new(()),
            catalog_cache_generation: tokio::sync::Mutex::new(0),
        }
    }

    /// Signalled whenever an upstream request fails in a network-class way (connect, DNS, TLS,
    /// timeout), so a reachability monitor can confirm the outage.
    pub fn network_failure_signal(&self) -> Arc<Notify> {
        Arc::clone(&self.network_failures)
    }

    fn note_upstream_result<T>(&self, result: &Result<T, QueryError>) {
        if let Err(err) = result
            && is_offline_fallback_error(err)
        {
            self.network_failures.notify_one();
        }
    }

    pub fn set_network_offline(&self, offline: bool) {
        if let Ok(mut guard) = self.state.write() {
            guard.network_offline = offline;
        }
    }

    pub fn is_network_offline(&self) -> bool {
        self.state
            .read()
            .map(|g| g.network_offline)
            .unwrap_or(false)
    }

    /// Drops the active session of `epoch` and its in-memory windows after its cookies turned
    /// out to belong to another account, so nothing is read or cached under the old scope.
    fn unbind_changed_account(&self, epoch: &AccountEpoch) {
        if let Ok(mut guard) = self.state.write()
            && guard
                .active
                .as_ref()
                .is_some_and(|a| &a.account_epoch == epoch)
        {
            if let Some(active) = guard.active.take() {
                active.session_cancel.cancel();
            }
            guard.recent_history_cache = None;
            guard.discovery_windows.clear();
            guard.typed_search_windows.clear();
        }
    }

    pub fn active_account_epoch(&self) -> Option<AccountEpoch> {
        self.state
            .read()
            .ok()
            .and_then(|g| g.active.as_ref().map(|a| a.account_epoch.clone()))
    }

    pub fn active_account_scope(&self) -> Option<AccountScope> {
        self.state
            .read()
            .ok()
            .and_then(|g| g.active.as_ref().map(|a| a.account_scope.clone()))
    }

    fn cache_context(&self, active: &ActiveSession) -> AccountCacheContext {
        AccountCacheContext {
            account_scope: active.account_scope.clone(),
            account_epoch: active.account_epoch.clone(),
            locale: self.config.locale.clone(),
            provider_version: PROVIDER_VERSION.to_string(),
        }
    }

    fn resolve_query_context(
        &self,
        req_epoch: Option<&AccountEpoch>,
        req_scope: Option<&AccountScope>,
    ) -> Result<ResolvedQueryContext, QueryError> {
        let guard = self.state.read().map_err(|_| {
            make_query_error(
                QueryErrorCode::StorageError,
                false,
                None,
                "catalog adapter lock poisoned",
            )
        })?;
        let offline = guard.network_offline;

        match (&guard.active, req_epoch, req_scope) {
            (Some(active), Some(ep), _) if &active.account_epoch != ep => Err(make_query_error(
                QueryErrorCode::StaleContext,
                false,
                None,
                "request accountEpoch does not match active catalog session",
            )),
            (Some(active), _, Some(sc)) if &active.account_scope != sc => Err(make_query_error(
                QueryErrorCode::AccountMismatch,
                false,
                None,
                "request accountScope does not match active catalog session",
            )),
            (Some(active), None, _) => Ok(ResolvedQueryContext {
                active: Some(active.clone()),
                cache_scope: Some(active.account_scope.clone()),
                offline: true,
            }),
            (Some(active), Some(_), _) => Ok(ResolvedQueryContext {
                active: Some(active.clone()),
                cache_scope: Some(active.account_scope.clone()),
                offline,
            }),
            (None, Some(_), _) => Err(make_query_error(
                QueryErrorCode::StaleContext,
                false,
                None,
                "account session has been cleared; stale accountEpoch rejected",
            )),
            (None, None, Some(sc)) => Ok(ResolvedQueryContext {
                active: None,
                cache_scope: Some(sc.clone()),
                offline: true,
            }),
            (None, None, None) => Ok(ResolvedQueryContext {
                active: None,
                cache_scope: None,
                offline: true,
            }),
        }
    }

    pub(crate) fn query_session_cancel(
        &self,
        epoch: Option<&AccountEpoch>,
        scope: Option<&AccountScope>,
    ) -> Result<CancellationToken, QueryError> {
        let ctx = self.resolve_query_context(epoch, scope)?;
        Ok(ctx.active.map(|s| s.session_cancel).unwrap_or_default())
    }

    async fn run_backend_call<F, T>(
        &self,
        active: &ActiveSession,
        cancel: &CancellationToken,
        fut: F,
    ) -> Result<T, QueryError>
    where
        F: std::future::Future<Output = Result<T, QueryError>>,
    {
        if cancel.is_cancelled() || active.session_cancel.is_cancelled() {
            return Err(make_query_error(
                QueryErrorCode::StaleContext,
                false,
                None,
                "catalog request cancelled due to account state change",
            ));
        }

        let _permit = tokio::select! {
            _ = cancel.cancelled() => {
                return Err(make_query_error(
                    QueryErrorCode::StaleContext,
                    false,
                    None,
                    "catalog request cancelled while waiting for concurrency slot",
                ));
            }
            _ = active.session_cancel.cancelled() => {
                return Err(make_query_error(
                    QueryErrorCode::StaleContext,
                    false,
                    None,
                    "catalog session invalidated while waiting for concurrency slot",
                ));
            }
            permit = self.semaphore.acquire() => {
                permit.map_err(|_| make_query_error(
                    QueryErrorCode::Busy,
                    true,
                    Some(500),
                    "catalog request concurrency limiter closed",
                ))?
            }
        };

        let result = tokio::select! {
            _ = cancel.cancelled() => {
                Err(make_query_error(
                    QueryErrorCode::StaleContext,
                    false,
                    None,
                    "catalog request cancelled in flight",
                ))
            }
            _ = active.session_cancel.cancelled() => {
                Err(make_query_error(
                    QueryErrorCode::StaleContext,
                    false,
                    None,
                    "catalog session cleared while request was in flight",
                ))
            }
            res = tokio::time::timeout(self.config.request_timeout, fut) => {
                let res = match res {
                    Ok(inner) => inner,
                    Err(_) => Err(make_query_error(
                        QueryErrorCode::Timeout,
                        true,
                        Some(1_500),
                        "catalog request exceeded bounded timeout",
                    )),
                };
                self.note_upstream_result(&res);
                res
            }
        }?;

        // Re-verify epoch hasn't changed while awaiting.
        if self.active_account_epoch().as_ref() != Some(&active.account_epoch) {
            return Err(make_query_error(
                QueryErrorCode::StaleContext,
                false,
                None,
                "accountEpoch changed while catalog request was in flight",
            ));
        }

        Ok(result)
    }

    fn decode_position(
        &self,
        request: &CatalogQueryRequest,
        kind: CursorKind,
        target: &str,
        scope: &AccountScope,
    ) -> Result<CursorPosition, QueryError> {
        match request.cursor.as_deref() {
            Some(raw) => decode_cursor(raw, kind, target, scope, &self.config.locale),
            None => Ok(CursorPosition::default()),
        }
    }

    /// Reads one bounded window of fresh provider playlist data and preserves occurrence IDs.
    pub async fn fresh_playlist(
        &self,
        epoch: &AccountEpoch,
        scope: &AccountScope,
        playlist: &PlaylistId,
        page: MusicPageRequest<'_>,
        cancel: &CancellationToken,
    ) -> Result<FreshPlaylistPage, QueryError> {
        let active = self.fresh_session(epoch, scope, page.limit)?;
        let target = format!("{epoch}/{}", playlist.as_str());
        let kind = CursorKind::FreshPlaylist;
        let position = self.fresh_position(kind, &target, scope, page.cursor)?;
        let provider = self
            .run_backend_call(
                &active,
                cancel,
                self.backend
                    .fresh_playlist(&active.session, playlist, position.token.as_deref()),
            )
            .await?;
        let incomplete = position.incomplete || provider.entries.len() != provider.source_entries;
        let (mut entries, next) = take_window(
            provider.entries,
            &position,
            page.limit as usize,
            provider.next_continuation,
        );
        let page_start = u32::try_from(position.page_start).map_err(|_| invalid_position())?;
        for entry in &mut entries {
            entry.position = entry
                .position
                .checked_add(page_start)
                .ok_or_else(invalid_position)?;
        }
        let next_cursor = next
            .map(|mut next| {
                next.incomplete = incomplete;
                next.page_start = if next.skip == 0 {
                    position
                        .page_start
                        .checked_add(provider.source_entries)
                        .ok_or_else(invalid_position)?
                } else {
                    position.page_start
                };
                encode_cursor(kind, &target, scope, &self.config.locale, &next)
            })
            .transpose()?;
        Ok(FreshPlaylistPage {
            metadata: provider.metadata,
            entries,
            complete: next_cursor.is_none() && !incomplete,
            next_cursor,
            captured_at_ms: self.clock.wall_now_ms(),
        })
    }

    /// Reads official song-radio candidates without dispatching any playback command.
    pub async fn recommend_song_radio(
        &self,
        epoch: &AccountEpoch,
        scope: &AccountScope,
        seed: &VideoId,
        page: MusicPageRequest<'_>,
        cancel: &CancellationToken,
    ) -> Result<SongRadioPage, QueryError> {
        let active = self.fresh_session(epoch, scope, page.limit)?;
        let target = format!("{epoch}/{seed}");
        let kind = CursorKind::SongRadio;
        let position = self.fresh_position(kind, &target, scope, page.cursor)?;
        let provider = self
            .run_backend_call(
                &active,
                cancel,
                self.backend
                    .song_radio(&active.session, seed, position.token.as_deref()),
            )
            .await?;
        let (candidates, next) = take_window(
            provider.items,
            &position,
            page.limit as usize,
            provider.next_continuation,
        );
        let next_cursor = next
            .map(|next| encode_cursor(kind, &target, scope, &self.config.locale, &next))
            .transpose()?;
        Ok(SongRadioPage {
            seed: seed.clone(),
            candidates,
            next_cursor,
            captured_at_ms: self.clock.wall_now_ms(),
        })
    }

    pub async fn discover(
        &self,
        epoch: &AccountEpoch,
        scope: &AccountScope,
        surface: DiscoverySurface,
        page: MusicPageRequest<'_>,
        cancel: &CancellationToken,
    ) -> Result<ProviderDiscoveryPage, QueryError> {
        self.discovery_page(epoch, scope, DiscoveryRead::Surface(surface), page, cancel)
            .await
    }

    pub async fn mood_category(
        &self,
        epoch: &AccountEpoch,
        scope: &AccountScope,
        category_id: &str,
        page: MusicPageRequest<'_>,
        cancel: &CancellationToken,
    ) -> Result<ProviderDiscoveryPage, QueryError> {
        if category_id.len() != 64
            || !category_id
                .bytes()
                .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
        {
            return Err(make_query_error(
                QueryErrorCode::InvalidPayload,
                false,
                None,
                "expected an opaque mood category ID from YouTube Music discovery",
            ));
        }
        self.discovery_page(
            epoch,
            scope,
            DiscoveryRead::MoodCategory(category_id),
            page,
            cancel,
        )
        .await
    }

    pub async fn search_suggestions(
        &self,
        epoch: &AccountEpoch,
        scope: &AccountScope,
        query: &str,
        limit: u32,
        cancel: &CancellationToken,
    ) -> Result<ProviderSearchSuggestions, QueryError> {
        let active = self.fresh_session(epoch, scope, limit)?;
        let query = query.trim();
        if query.is_empty() || query.chars().count() > 256 || query.chars().any(char::is_control) {
            return Err(make_query_error(
                QueryErrorCode::InvalidPayload,
                false,
                None,
                "expected a nonempty search prefix of at most 256 characters",
            ));
        }
        let mut items = self
            .run_backend_call(&active, cancel, self.backend.search_predictions(query))
            .await?;
        let complete = items.len() <= limit as usize;
        items.truncate(limit as usize);
        Ok(ProviderSearchSuggestions {
            items,
            complete,
            captured_at_ms: self.clock.wall_now_ms(),
        })
    }

    async fn discovery_page(
        &self,
        epoch: &AccountEpoch,
        scope: &AccountScope,
        request: DiscoveryRead<'_>,
        page: MusicPageRequest<'_>,
        cancel: &CancellationToken,
    ) -> Result<ProviderDiscoveryPage, QueryError> {
        let active = self.fresh_session(epoch, scope, page.limit)?;
        let target = match request {
            DiscoveryRead::Surface(surface) => format!("{epoch}/{}", surface.source()),
            DiscoveryRead::MoodCategory(id) => format!("{epoch}/{MOOD_PLAYLISTS_SOURCE}/{id}"),
        };
        let kind = CursorKind::Discovery;
        let position = self.fresh_position(kind, &target, scope, page.cursor)?;
        let (provider, captured_at_ms, expires_at_ms) = if position.skip > 0 {
            let state = self.state.read().map_err(|_| stale_provider_cursor())?;
            let window = state
                .discovery_windows
                .iter()
                .find(|window| {
                    Some(window.cursor.as_str()) == page.cursor
                        && window.expires_at_ms > self.clock.now_ms()
                })
                .ok_or_else(stale_provider_cursor)?;
            (
                window.page.clone(),
                window.captured_at_ms,
                window.expires_at_ms,
            )
        } else {
            let scope_key = mood_scope_key(epoch, scope, &self.config.locale);
            let future = match request {
                DiscoveryRead::Surface(surface) => {
                    self.backend
                        .discovery(&active.session, surface, position.token.as_deref())
                }
                DiscoveryRead::MoodCategory(id) if position.token.is_none() => {
                    self.backend.mood_category(&active.session, id, &scope_key)
                }
                DiscoveryRead::MoodCategory(_) => return Err(stale_provider_cursor()),
            };
            let mut provider = self
                .run_backend_call(&active, cancel, future)
                .await
                .map_err(|error| page_read_error(&position, error))?;
            for item in &mut provider.items {
                if let ProviderDiscoveryTarget::MoodCategory(id) = &mut item.target {
                    *id = scoped_mood_category_id(&scope_key, id);
                }
            }
            (
                Arc::new(provider),
                self.clock.wall_now_ms(),
                self.clock.now_ms().saturating_add(DISCOVERY_WINDOW_TTL_MS),
            )
        };
        let (items, next, complete) = bounded_provider_window(
            provider.items.clone(),
            provider.next_continuation.clone(),
            provider.fully_projected,
            &position,
            page.limit as usize,
        )?;
        let next_cursor = next
            .as_ref()
            .map(|next| encode_cursor(kind, &target, scope, &self.config.locale, next))
            .transpose()?;
        let mut state = self.state.write().map_err(|_| stale_provider_cursor())?;
        if cancel.is_cancelled()
            || !state.active.as_ref().is_some_and(|current| {
                current.account_epoch == *epoch && current.account_scope == *scope
            })
        {
            return Err(stale_provider_cursor());
        }
        if let (Some(position), Some(cursor)) = (next, next_cursor.as_ref())
            && position.skip > 0
        {
            let windows = &mut state.discovery_windows;
            windows.retain(|window| window.expires_at_ms > self.clock.now_ms());
            if !windows.iter().any(|window| window.cursor == *cursor) {
                if windows.len() == DISCOVERY_WINDOW_LIMIT {
                    windows.pop_front();
                }
                windows.push_back(DiscoveryWindow {
                    cursor: cursor.clone(),
                    page: provider,
                    captured_at_ms,
                    expires_at_ms,
                });
            }
        }
        Ok(ProviderDiscoveryPage {
            items,
            complete,
            next_cursor,
            captured_at_ms,
        })
    }

    pub async fn extended_media(
        &self,
        epoch: &AccountEpoch,
        scope: &AccountScope,
        query: ExtendedMediaQuery<'_>,
        page: MusicPageRequest<'_>,
        cancel: &CancellationToken,
    ) -> Result<ProviderExtendedPage, QueryError> {
        let active = self.fresh_session(epoch, scope, page.limit)?;
        let target = format!(
            "{epoch}/{}/{}",
            query.source(),
            query.id().map_or("", |id| id.as_str())
        );
        let kind = CursorKind::ExtendedMedia;
        let position = self.fresh_position(kind, &target, scope, page.cursor)?;
        let provider = self
            .run_backend_call(
                &active,
                cancel,
                self.backend
                    .extended_media(&active.session, query, position.token.as_deref()),
            )
            .await
            .map_err(|error| page_read_error(&position, error))?;
        let limit = extended_media_limit(&provider, position.skip, page.limit as usize)?;
        let (items, next, complete) = bounded_provider_window(
            provider.items,
            provider.next_continuation,
            provider.fully_projected,
            &position,
            limit,
        )?;
        let next_cursor = next
            .map(|next| encode_cursor(kind, &target, scope, &self.config.locale, &next))
            .transpose()?;
        Ok(ProviderExtendedPage {
            entity: provider.entity,
            items,
            complete,
            next_cursor,
            captured_at_ms: self.clock.wall_now_ms(),
        })
    }

    pub async fn library_page(
        &self,
        epoch: &AccountEpoch,
        scope: &AccountScope,
        surface: LibrarySurface,
        page: MusicPageRequest<'_>,
        cancel: &CancellationToken,
    ) -> Result<ProviderLibraryPage, QueryError> {
        let active = self.fresh_session(epoch, scope, page.limit)?;
        let target = format!("{epoch}/{}", surface.source());
        let kind = CursorKind::Library;
        let position = self.fresh_position(kind, &target, scope, page.cursor)?;
        let provider = self
            .run_backend_call(
                &active,
                cancel,
                self.backend
                    .library_page(&active.session, surface, position.token.as_deref()),
            )
            .await
            .map_err(|error| page_read_error(&position, error))?;
        let (items, next, complete) = bounded_provider_window(
            provider.items,
            provider.next_continuation,
            true,
            &position,
            page.limit as usize,
        )?;
        let next_cursor = next
            .map(|next| encode_cursor(kind, &target, scope, &self.config.locale, &next))
            .transpose()?;
        Ok(ProviderLibraryPage {
            items,
            complete,
            next_cursor,
            captured_at_ms: self.clock.wall_now_ms(),
        })
    }

    fn fresh_session(
        &self,
        epoch: &AccountEpoch,
        scope: &AccountScope,
        limit: u32,
    ) -> Result<ActiveSession, QueryError> {
        if !(1..=MAX_PAGE_SIZE).contains(&limit) {
            return Err(make_query_error(
                QueryErrorCode::InvalidPayload,
                false,
                None,
                "fresh catalog page exceeds the allowed size",
            ));
        }
        self.online_session(epoch, scope)
    }

    fn online_session(
        &self,
        epoch: &AccountEpoch,
        scope: &AccountScope,
    ) -> Result<ActiveSession, QueryError> {
        let context = self.resolve_query_context(Some(epoch), Some(scope))?;
        if context.offline {
            return Err(make_query_error(
                QueryErrorCode::NetworkOffline,
                true,
                None,
                "catalog operation requires a connection",
            ));
        }
        context.active.ok_or_else(|| {
            make_query_error(
                QueryErrorCode::AuthRequired,
                false,
                None,
                "catalog operation requires an account session",
            )
        })
    }

    fn fresh_position(
        &self,
        kind: CursorKind,
        target: &str,
        scope: &AccountScope,
        cursor: Option<&str>,
    ) -> Result<CursorPosition, QueryError> {
        cursor
            .map(|cursor| decode_cursor(cursor, kind, target, scope, &self.config.locale))
            .transpose()
            .map(Option::unwrap_or_default)
    }

    /// Executes one dispatched provider step for a control operation already journaled by core.
    pub async fn mutate_playlist_once(
        &self,
        epoch: &AccountEpoch,
        scope: &AccountScope,
        request: PlaylistMutation<'_>,
        cancel: &CancellationToken,
    ) -> Result<PlaylistMutationAck, CatalogMutationError> {
        let active = self
            .online_session(epoch, scope)
            .map_err(CatalogMutationError::Rejected)?;
        self.run_control_write(&active, cancel, async {
            self.backend.write_playlist(&active.session, request).await
        })
        .await
    }

    pub async fn fresh_rating(
        &self,
        epoch: &AccountEpoch,
        scope: &AccountScope,
        video: &VideoId,
        cancel: &CancellationToken,
    ) -> Result<SongRatingEvidence, QueryError> {
        let active = self.online_session(epoch, scope)?;
        let rating = self
            .run_backend_call(
                &active,
                cancel,
                self.backend.read_rating(&active.session, video),
            )
            .await?;
        Ok(SongRatingEvidence {
            rating,
            observed_at_ms: self.clock.wall_now_ms(),
        })
    }

    pub async fn fresh_song_membership(
        &self,
        epoch: &AccountEpoch,
        scope: &AccountScope,
        video: &VideoId,
        cancel: &CancellationToken,
    ) -> Result<SongMembershipEvidence, QueryError> {
        let active = self.online_session(epoch, scope)?;
        let in_library = self
            .run_backend_call(
                &active,
                cancel,
                self.backend.song_membership(&active.session, video),
            )
            .await?;
        Ok(SongMembershipEvidence {
            in_library,
            observed_at_ms: self.clock.wall_now_ms(),
        })
    }

    pub async fn set_song_membership_once(
        &self,
        epoch: &AccountEpoch,
        scope: &AccountScope,
        video: &VideoId,
        saved: bool,
        cancel: &CancellationToken,
    ) -> Result<(), CatalogMutationError> {
        let active = self
            .online_session(epoch, scope)
            .map_err(CatalogMutationError::Rejected)?;
        self.run_control_write(
            &active,
            cancel,
            self.backend
                .write_song_membership(&active.session, video, saved),
        )
        .await
    }

    pub async fn fresh_playlist_membership(
        &self,
        epoch: &AccountEpoch,
        scope: &AccountScope,
        playlist: &PlaylistId,
        cancel: &CancellationToken,
    ) -> Result<SongMembershipEvidence, QueryError> {
        let active = self.online_session(epoch, scope)?;
        let in_library = self
            .run_backend_call(
                &active,
                cancel,
                self.backend.playlist_membership(&active.session, playlist),
            )
            .await?;
        Ok(SongMembershipEvidence {
            in_library,
            observed_at_ms: self.clock.wall_now_ms(),
        })
    }

    pub async fn set_playlist_membership_once(
        &self,
        epoch: &AccountEpoch,
        scope: &AccountScope,
        playlist: &PlaylistId,
        saved: bool,
        cancel: &CancellationToken,
    ) -> Result<(), CatalogMutationError> {
        let active = self
            .online_session(epoch, scope)
            .map_err(CatalogMutationError::Rejected)?;
        self.run_control_write(
            &active,
            cancel,
            self.backend
                .write_playlist_membership(&active.session, playlist, saved),
        )
        .await
    }

    pub async fn fresh_album_membership(
        &self,
        epoch: &AccountEpoch,
        scope: &AccountScope,
        album: &AlbumId,
        cancel: &CancellationToken,
    ) -> Result<AlbumMembershipEvidence, QueryError> {
        let active = self.online_session(epoch, scope)?;
        let proof = self
            .run_backend_call(
                &active,
                cancel,
                self.backend.album_membership(&active.session, album),
            )
            .await?;
        Ok(AlbumMembershipEvidence {
            in_library: Some(proof.saved),
            observed_at_ms: self.clock.wall_now_ms(),
            playlist_id: proof.playlist_id,
        })
    }

    pub async fn set_album_membership_once(
        &self,
        epoch: &AccountEpoch,
        scope: &AccountScope,
        album: &AlbumId,
        expected_playlist: &PlaylistId,
        saved: bool,
        cancel: &CancellationToken,
    ) -> Result<(), CatalogMutationError> {
        let active = self
            .online_session(epoch, scope)
            .map_err(CatalogMutationError::Rejected)?;
        self.run_control_write(
            &active,
            cancel,
            self.backend
                .write_album_membership(&active.session, album, expected_playlist, saved),
        )
        .await
    }

    pub async fn fresh_artist_subscription(
        &self,
        epoch: &AccountEpoch,
        scope: &AccountScope,
        artist: &ArtistId,
        cancel: &CancellationToken,
    ) -> Result<ArtistSubscriptionEvidence, QueryError> {
        let active = self.online_session(epoch, scope)?;
        let subscribed = self
            .run_backend_call(
                &active,
                cancel,
                self.backend.artist_subscription(&active.session, artist),
            )
            .await?;
        Ok(ArtistSubscriptionEvidence {
            subscribed,
            observed_at_ms: self.clock.wall_now_ms(),
        })
    }

    pub async fn set_artist_subscription_once(
        &self,
        epoch: &AccountEpoch,
        scope: &AccountScope,
        artist: &ArtistId,
        subscribed: bool,
        cancel: &CancellationToken,
    ) -> Result<(), CatalogMutationError> {
        let active = self
            .online_session(epoch, scope)
            .map_err(CatalogMutationError::Rejected)?;
        self.run_control_write(&active, cancel, async {
            self.backend
                .write_artist_subscription(&active.session, artist, subscribed)
                .await
        })
        .await
    }

    pub async fn set_rating_once(
        &self,
        epoch: &AccountEpoch,
        scope: &AccountScope,
        video: &VideoId,
        rating: SongRating,
        cancel: &CancellationToken,
    ) -> Result<(), CatalogMutationError> {
        let active = self
            .online_session(epoch, scope)
            .map_err(CatalogMutationError::Rejected)?;
        self.run_control_write(&active, cancel, async {
            self.backend
                .write_rating(&active.session, video, rating)
                .await
        })
        .await
    }

    async fn run_control_write<T>(
        &self,
        active: &ActiveSession,
        cancel: &CancellationToken,
        write: impl std::future::Future<Output = Result<T, CatalogMutationError>>,
    ) -> Result<T, CatalogMutationError> {
        let stopped = || {
            make_query_error(
                QueryErrorCode::StaleContext,
                false,
                None,
                "catalog write context changed",
            )
        };
        let _slot = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(CatalogMutationError::Rejected(stopped())),
            _ = active.session_cancel.cancelled() => return Err(CatalogMutationError::Rejected(stopped())),
            slot = self.semaphore.acquire() => slot.map_err(|_| CatalogMutationError::Rejected(
                make_query_error(QueryErrorCode::Busy, false, None, "catalog write limiter closed")
            ))?,
        };
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(CatalogMutationError::Unknown(stopped())),
            _ = active.session_cancel.cancelled() => return Err(CatalogMutationError::Unknown(stopped())),
            result = tokio::time::timeout(
                self.config.request_timeout,
                write,
            ) => result.unwrap_or_else(|_| Err(CatalogMutationError::Unknown(make_query_error(
                QueryErrorCode::Timeout, false, None, "provider acknowledgment timed out",
            )))),
        };
        if let Err(CatalogMutationError::Unknown(error)) = &result
            && is_offline_fallback_error(error)
        {
            self.network_failures.notify_one();
        }
        result
    }

    pub async fn invalidate_playlist_cache(
        &self,
        scope: &AccountScope,
        playlist: &PlaylistId,
        library_changed: bool,
    ) -> Result<(), QueryError> {
        let mut generation = self.catalog_cache_generation.lock().await;
        self.storage
            .invalidate_playlist_cache(scope, playlist, library_changed)
            .await?;
        *generation = generation.wrapping_add(1);
        Ok(())
    }

    pub async fn invalidate_rating_cache(&self, scope: &AccountScope) -> Result<(), QueryError> {
        let mut generation = self.catalog_cache_generation.lock().await;
        self.storage.invalidate_rating_cache(scope).await?;
        if let Ok(mut state) = self.state.write()
            && state
                .active
                .as_ref()
                .is_some_and(|active| &active.account_scope == scope)
        {
            state.recent_history_cache = None;
            state.discovery_windows.clear();
        }
        *generation = generation.wrapping_add(1);
        Ok(())
    }

    pub async fn invalidate_library_membership_cache(
        &self,
        scope: &AccountScope,
        kind: LibraryMembershipCache<'_>,
    ) -> Result<(), QueryError> {
        if matches!(kind, LibraryMembershipCache::Song) {
            return self
                .storage
                .invalidate_library_membership(scope, kind)
                .await;
        }
        let mut generation = self.catalog_cache_generation.lock().await;
        self.storage
            .invalidate_library_membership(scope, kind)
            .await?;
        *generation = generation.wrapping_add(1);
        Ok(())
    }

    /// Holds the cache generation while a fresh provider result is written to the cache. An
    /// invalidation that happened during the read returns `None`: the result is still served,
    /// but it must not overwrite the invalidated cache.
    async fn catalog_cache_for(&self, expected: u64) -> Option<tokio::sync::MutexGuard<'_, u64>> {
        let generation = self.catalog_cache_generation.lock().await;
        (*generation == expected).then_some(generation)
    }

    async fn query_search_songs(
        &self,
        ctx: &ResolvedQueryContext,
        request: &CatalogQueryRequest,
        cancel: &CancellationToken,
    ) -> Result<CatalogResponsePayload, QueryError> {
        let query_str = request
            .query
            .as_deref()
            .map(str::trim)
            .filter(|q| !q.is_empty())
            .ok_or_else(|| {
                make_query_error(
                    QueryErrorCode::InvalidPayload,
                    false,
                    None,
                    "searchSongs requires a non-empty query string",
                )
            })?;

        let Some(scope) = ctx.cache_scope.as_ref() else {
            return Err(make_query_error(
                QueryErrorCode::AuthRequired,
                false,
                None,
                "authentication required to search YouTube Music catalog",
            ));
        };

        let position = self.decode_position(request, CursorKind::SearchSongs, query_str, scope)?;
        let limit = page_limit(request.limit);

        let cache_generation = *self.catalog_cache_generation.lock().await;
        if !ctx.offline
            && let Some(active) = ctx.active.as_ref()
        {
            match self
                .run_backend_call(
                    active,
                    cancel,
                    self.backend.search_songs(
                        &active.session,
                        query_str,
                        position.token.as_deref(),
                    ),
                )
                .await
            {
                Ok(page) => {
                    let (window, next) =
                        take_window(page.items, &position, limit, page.next_continuation);
                    let next_cursor = next
                        .map(|pos| {
                            encode_cursor(
                                CursorKind::SearchSongs,
                                query_str,
                                scope,
                                &self.config.locale,
                                &pos,
                            )
                        })
                        .transpose()?;
                    // Albums/artists/playlists only accompany the first page; failing to read
                    // them never fails the song search.
                    let overview = if request.cursor.is_none() {
                        self.run_backend_call(
                            active,
                            cancel,
                            self.backend.search_overview(&active.session, query_str),
                        )
                        .await
                        .unwrap_or_else(|err| {
                            debug!(error = %err.safe_message, "search overview unavailable");
                            SearchOverview::default()
                        })
                    } else {
                        SearchOverview::default()
                    };
                    let result = SearchResult {
                        query: query_str.to_string(),
                        songs: PagedResult {
                            items: window,
                            next_cursor,
                            freshness: Freshness::Fresh,
                            fetched_at_ms: self.clock.wall_now_ms(),
                            account_scope: scope.clone(),
                            provider_version: PROVIDER_VERSION.to_string(),
                        },
                        albums: overview.albums,
                        playlists: overview.playlists,
                        artists: overview.artists,
                    };
                    let cache = self.catalog_cache_for(cache_generation).await;
                    let cache_ctx = self.cache_context(active);
                    if cache.is_some()
                        && let Err(err) = self
                            .storage
                            .put_search_result(
                                &cache_ctx,
                                query_str,
                                request.cursor.as_deref(),
                                limit,
                                &result,
                                None,
                            )
                            .await
                    {
                        warn!(error = %err.safe_message, "failed to persist search cache entry");
                    }
                    return Ok(CatalogResponsePayload::SearchSongs(result));
                }
                Err(err) if is_offline_fallback_error(&err) => {
                    if let Some(mut cached) = self
                        .storage
                        .get_search_result(
                            scope,
                            &self.config.locale,
                            query_str,
                            request.cursor.as_deref(),
                            limit,
                        )
                        .await?
                    {
                        cached.value.songs.freshness = Freshness::Stale;
                        return Ok(CatalogResponsePayload::SearchSongs(cached.value));
                    }
                    return Err(err);
                }
                Err(err) => return Err(err),
            }
        }

        if let Some(mut cached) = self
            .storage
            .get_search_result(
                scope,
                &self.config.locale,
                query_str,
                request.cursor.as_deref(),
                limit,
            )
            .await?
        {
            cached.value.songs.freshness = Freshness::Stale;
            return Ok(CatalogResponsePayload::SearchSongs(cached.value));
        }

        Err(make_query_error(
            if ctx.active.is_some() {
                QueryErrorCode::NetworkOffline
            } else {
                QueryErrorCode::AuthRequired
            },
            ctx.active.is_some(),
            None,
            "no cached search results available while offline",
        ))
    }

    async fn query_library_playlists(
        &self,
        ctx: &ResolvedQueryContext,
        request: &CatalogQueryRequest,
        cancel: &CancellationToken,
    ) -> Result<CatalogResponsePayload, QueryError> {
        let Some(scope) = ctx.cache_scope.as_ref() else {
            return Err(make_query_error(
                QueryErrorCode::AuthRequired,
                false,
                None,
                "authentication required to read library playlists",
            ));
        };

        let position = self.decode_position(request, CursorKind::LibraryPlaylists, "", scope)?;
        let limit = page_limit(request.limit);

        let cache_generation = *self.catalog_cache_generation.lock().await;
        if !ctx.offline
            && let Some(active) = ctx.active.as_ref()
        {
            match self
                .run_backend_call(
                    active,
                    cancel,
                    self.backend
                        .library_playlists(&active.session, position.token.as_deref()),
                )
                .await
            {
                Ok(page) => {
                    let (window, next) =
                        take_window(page.items, &position, limit, page.next_continuation);
                    let next_cursor = next
                        .map(|pos| {
                            encode_cursor(
                                CursorKind::LibraryPlaylists,
                                "",
                                scope,
                                &self.config.locale,
                                &pos,
                            )
                        })
                        .transpose()?;
                    let result = PagedResult {
                        items: window,
                        next_cursor,
                        freshness: Freshness::Fresh,
                        fetched_at_ms: self.clock.wall_now_ms(),
                        account_scope: scope.clone(),
                        provider_version: PROVIDER_VERSION.to_string(),
                    };
                    let cache = self.catalog_cache_for(cache_generation).await;
                    let cache_ctx = self.cache_context(active);
                    if cache.is_some()
                        && let Err(err) = self
                            .storage
                            .put_library_playlists(
                                &cache_ctx,
                                request.cursor.as_deref(),
                                limit,
                                &result,
                                None,
                            )
                            .await
                    {
                        warn!(error = %err.safe_message, "failed to persist library playlists cache");
                    }
                    return Ok(CatalogResponsePayload::LibraryPlaylists(result));
                }
                Err(err) if is_offline_fallback_error(&err) => {
                    if let Some(mut cached) = self
                        .storage
                        .get_library_playlists(
                            scope,
                            &self.config.locale,
                            request.cursor.as_deref(),
                            limit,
                        )
                        .await?
                    {
                        cached.value.freshness = Freshness::Stale;
                        cached.value.items.retain(is_user_library_playlist);
                        return Ok(CatalogResponsePayload::LibraryPlaylists(cached.value));
                    }
                    return Err(err);
                }
                Err(err) => return Err(err),
            }
        }

        if let Some(mut cached) = self
            .storage
            .get_library_playlists(scope, &self.config.locale, request.cursor.as_deref(), limit)
            .await?
        {
            cached.value.freshness = Freshness::Stale;
            cached.value.items.retain(is_user_library_playlist);
            return Ok(CatalogResponsePayload::LibraryPlaylists(cached.value));
        }

        Err(make_query_error(
            if ctx.active.is_some() {
                QueryErrorCode::NetworkOffline
            } else {
                QueryErrorCode::AuthRequired
            },
            ctx.active.is_some(),
            None,
            "library playlists unavailable offline without cached data",
        ))
    }

    async fn query_playlist_tracks(
        &self,
        ctx: &ResolvedQueryContext,
        request: &CatalogQueryRequest,
        cancel: &CancellationToken,
    ) -> Result<CatalogResponsePayload, QueryError> {
        let playlist_id = request.playlist_id.as_ref().ok_or_else(|| {
            make_query_error(
                QueryErrorCode::InvalidPayload,
                false,
                None,
                "playlistTracks requires playlistId",
            )
        })?;
        let Some(scope) = ctx.cache_scope.as_ref() else {
            return Err(make_query_error(
                QueryErrorCode::AuthRequired,
                false,
                None,
                "authentication required to read playlist tracks",
            ));
        };

        let position = self.decode_position(
            request,
            CursorKind::PlaylistTracks,
            playlist_id.as_str(),
            scope,
        )?;
        let limit = page_limit(request.limit);

        let cache_generation = *self.catalog_cache_generation.lock().await;
        if !ctx.offline
            && let Some(active) = ctx.active.as_ref()
        {
            match self
                .run_backend_call(
                    active,
                    cancel,
                    self.backend.playlist_tracks(
                        &active.session,
                        playlist_id,
                        position.token.as_deref(),
                    ),
                )
                .await
            {
                Ok(page) => {
                    let (window, next) =
                        take_window(page.tracks, &position, limit, page.next_continuation);
                    let next_cursor = next
                        .map(|pos| {
                            encode_cursor(
                                CursorKind::PlaylistTracks,
                                playlist_id.as_str(),
                                scope,
                                &self.config.locale,
                                &pos,
                            )
                        })
                        .transpose()?;
                    let (title, description, author, cover) = if request.cursor.is_some() {
                        if let Ok(Some(first_page)) = self
                            .storage
                            .get_playlist_detail(
                                scope,
                                &self.config.locale,
                                playlist_id,
                                None,
                                limit,
                            )
                            .await
                        {
                            (
                                first_page.value.title,
                                first_page.value.description,
                                first_page.value.author,
                                first_page.value.cover,
                            )
                        } else {
                            (page.title, page.description, page.author, page.cover)
                        }
                    } else {
                        (page.title, page.description, page.author, page.cover)
                    };
                    let detail = PlaylistDetail {
                        playlist_id: playlist_id.clone(),
                        title,
                        description,
                        author,
                        cover,
                        tracks: PagedResult {
                            items: window,
                            next_cursor,
                            freshness: Freshness::Fresh,
                            fetched_at_ms: self.clock.wall_now_ms(),
                            account_scope: scope.clone(),
                            provider_version: PROVIDER_VERSION.to_string(),
                        },
                    };
                    let cache = self.catalog_cache_for(cache_generation).await;
                    let cache_ctx = self.cache_context(active);
                    if cache.is_some()
                        && let Err(err) = self
                            .storage
                            .put_playlist_detail(
                                &cache_ctx,
                                playlist_id,
                                request.cursor.as_deref(),
                                limit,
                                &detail,
                                None,
                            )
                            .await
                    {
                        warn!(error = %err.safe_message, "failed to persist playlist detail cache");
                    }
                    return Ok(CatalogResponsePayload::PlaylistTracks(detail));
                }
                Err(err) if is_offline_fallback_error(&err) => {
                    if let Some(mut cached) = self
                        .storage
                        .get_playlist_detail(
                            scope,
                            &self.config.locale,
                            playlist_id,
                            request.cursor.as_deref(),
                            limit,
                        )
                        .await?
                    {
                        cached.value.tracks.freshness = Freshness::Stale;
                        return Ok(CatalogResponsePayload::PlaylistTracks(cached.value));
                    }
                    return Err(err);
                }
                Err(err) => return Err(err),
            }
        }

        if let Some(mut cached) = self
            .storage
            .get_playlist_detail(
                scope,
                &self.config.locale,
                playlist_id,
                request.cursor.as_deref(),
                limit,
            )
            .await?
        {
            cached.value.tracks.freshness = Freshness::Stale;
            return Ok(CatalogResponsePayload::PlaylistTracks(cached.value));
        }

        Err(make_query_error(
            if ctx.active.is_some() {
                QueryErrorCode::NetworkOffline
            } else {
                QueryErrorCode::AuthRequired
            },
            ctx.active.is_some(),
            None,
            "playlist tracks unavailable offline without cached data",
        ))
    }

    async fn query_album_detail(
        &self,
        ctx: &ResolvedQueryContext,
        request: &CatalogQueryRequest,
        cancel: &CancellationToken,
    ) -> Result<CatalogResponsePayload, QueryError> {
        let album_id = request.album_id.as_ref().ok_or_else(|| {
            make_query_error(
                QueryErrorCode::InvalidPayload,
                false,
                None,
                "albumDetail requires albumId",
            )
        })?;
        let Some(scope) = ctx.cache_scope.as_ref() else {
            return Err(make_query_error(
                QueryErrorCode::AuthRequired,
                false,
                None,
                "authentication required to read album details",
            ));
        };

        let cache_generation = *self.catalog_cache_generation.lock().await;
        if !ctx.offline
            && let Some(active) = ctx.active.as_ref()
        {
            match self
                .run_backend_call(
                    active,
                    cancel,
                    self.backend.album_detail(&active.session, album_id),
                )
                .await
            {
                Ok(detail) => {
                    let cache = self.catalog_cache_for(cache_generation).await;
                    let cache_ctx = self.cache_context(active);
                    if cache.is_some()
                        && let Err(err) = self
                            .storage
                            .put_album_detail(&cache_ctx, album_id, &detail, None)
                            .await
                    {
                        warn!(error = %err.safe_message, "failed to persist album detail cache");
                    }
                    return Ok(CatalogResponsePayload::AlbumDetail(detail));
                }
                Err(err) if is_offline_fallback_error(&err) => {
                    if let Some(cached) = self
                        .storage
                        .get_album_detail(scope, &self.config.locale, album_id)
                        .await?
                    {
                        return Ok(CatalogResponsePayload::AlbumDetail(cached.value));
                    }
                    return Err(err);
                }
                Err(err) => return Err(err),
            }
        }

        if let Some(cached) = self
            .storage
            .get_album_detail(scope, &self.config.locale, album_id)
            .await?
        {
            return Ok(CatalogResponsePayload::AlbumDetail(cached.value));
        }

        Err(make_query_error(
            if ctx.active.is_some() {
                QueryErrorCode::NetworkOffline
            } else {
                QueryErrorCode::AuthRequired
            },
            ctx.active.is_some(),
            None,
            "album detail unavailable offline without cached data",
        ))
    }

    async fn query_artist_detail(
        &self,
        ctx: &ResolvedQueryContext,
        request: &CatalogQueryRequest,
        cancel: &CancellationToken,
    ) -> Result<CatalogResponsePayload, QueryError> {
        let artist_id = request.artist_id.as_ref().ok_or_else(|| {
            make_query_error(
                QueryErrorCode::InvalidPayload,
                false,
                None,
                "artistDetail requires artistId",
            )
        })?;
        let Some(scope) = ctx.cache_scope.as_ref() else {
            return Err(make_query_error(
                QueryErrorCode::AuthRequired,
                false,
                None,
                "authentication required to read artist details",
            ));
        };

        let cache_generation = *self.catalog_cache_generation.lock().await;
        if !ctx.offline
            && let Some(active) = ctx.active.as_ref()
        {
            match self
                .run_backend_call(
                    active,
                    cancel,
                    self.backend.artist_detail(&active.session, artist_id),
                )
                .await
            {
                Ok(detail) => {
                    let cache = self.catalog_cache_for(cache_generation).await;
                    let cache_ctx = self.cache_context(active);
                    if cache.is_some()
                        && let Err(err) = self
                            .storage
                            .put_artist_detail(&cache_ctx, artist_id, &detail, None)
                            .await
                    {
                        warn!(error = %err.safe_message, "failed to persist artist detail cache");
                    }
                    return Ok(CatalogResponsePayload::ArtistDetail(detail));
                }
                Err(err) if is_offline_fallback_error(&err) => {
                    if let Some(cached) = self
                        .storage
                        .get_artist_detail(scope, &self.config.locale, artist_id)
                        .await?
                    {
                        return Ok(CatalogResponsePayload::ArtistDetail(cached.value));
                    }
                    return Err(err);
                }
                Err(err) => return Err(err),
            }
        }

        if let Some(cached) = self
            .storage
            .get_artist_detail(scope, &self.config.locale, artist_id)
            .await?
        {
            return Ok(CatalogResponsePayload::ArtistDetail(cached.value));
        }

        Err(make_query_error(
            if ctx.active.is_some() {
                QueryErrorCode::NetworkOffline
            } else {
                QueryErrorCode::AuthRequired
            },
            ctx.active.is_some(),
            None,
            "artist detail unavailable offline without cached data",
        ))
    }

    async fn query_liked_songs(
        &self,
        ctx: &ResolvedQueryContext,
        request: &CatalogQueryRequest,
        cancel: &CancellationToken,
    ) -> Result<CatalogResponsePayload, QueryError> {
        let Some(scope) = ctx.cache_scope.as_ref() else {
            return Err(make_query_error(
                QueryErrorCode::AuthRequired,
                false,
                None,
                "authentication required to read liked songs",
            ));
        };

        let position = self.decode_position(request, CursorKind::LikedSongs, "", scope)?;
        // "Liked music" is the account's auto playlist LM (songs rated thumbs-up), which is not
        // the same set as library songs.
        let liked_music = PlaylistId::new(LIKED_MUSIC_PLAYLIST_ID).expect("literal id");
        let limit = page_limit(request.limit);

        let cache_generation = *self.catalog_cache_generation.lock().await;
        if !ctx.offline
            && let Some(active) = ctx.active.as_ref()
        {
            match self
                .run_backend_call(
                    active,
                    cancel,
                    self.backend.playlist_tracks(
                        &active.session,
                        &liked_music,
                        position.token.as_deref(),
                    ),
                )
                .await
            {
                Ok(page) => {
                    let liked = page
                        .tracks
                        .into_iter()
                        .map(|mut t| {
                            t.is_liked = Some(true);
                            t
                        })
                        .collect();
                    let (window, next) =
                        take_window(liked, &position, limit, page.next_continuation);
                    let next_cursor = next
                        .map(|pos| {
                            encode_cursor(
                                CursorKind::LikedSongs,
                                "",
                                scope,
                                &self.config.locale,
                                &pos,
                            )
                        })
                        .transpose()?;
                    let result = PagedResult {
                        items: window,
                        next_cursor,
                        freshness: Freshness::Fresh,
                        fetched_at_ms: self.clock.wall_now_ms(),
                        account_scope: scope.clone(),
                        provider_version: PROVIDER_VERSION.to_string(),
                    };
                    let cache = self.catalog_cache_for(cache_generation).await;
                    let cache_ctx = self.cache_context(active);
                    if cache.is_some()
                        && let Err(err) = self
                            .storage
                            .put_liked_songs(
                                &cache_ctx,
                                request.cursor.as_deref(),
                                limit,
                                &result,
                                None,
                            )
                            .await
                    {
                        warn!(error = %err.safe_message, "failed to persist liked songs cache");
                    }
                    return Ok(CatalogResponsePayload::LikedSongs(result));
                }
                Err(err) if is_offline_fallback_error(&err) => {
                    if let Some(mut cached) = self
                        .storage
                        .get_liked_songs(
                            scope,
                            &self.config.locale,
                            request.cursor.as_deref(),
                            limit,
                        )
                        .await?
                    {
                        cached.value.freshness = Freshness::Stale;
                        return Ok(CatalogResponsePayload::LikedSongs(cached.value));
                    }
                    return Err(err);
                }
                Err(err) => return Err(err),
            }
        }

        if let Some(mut cached) = self
            .storage
            .get_liked_songs(scope, &self.config.locale, request.cursor.as_deref(), limit)
            .await?
        {
            cached.value.freshness = Freshness::Stale;
            return Ok(CatalogResponsePayload::LikedSongs(cached.value));
        }

        Err(make_query_error(
            if ctx.active.is_some() {
                QueryErrorCode::NetworkOffline
            } else {
                QueryErrorCode::AuthRequired
            },
            ctx.active.is_some(),
            None,
            "liked songs unavailable offline without cached data",
        ))
    }

    async fn query_recent_history(
        &self,
        ctx: &ResolvedQueryContext,
        request: &CatalogQueryRequest,
        cancel: &CancellationToken,
    ) -> Result<CatalogResponsePayload, QueryError> {
        let Some(scope) = ctx.cache_scope.as_ref() else {
            return Err(make_query_error(
                QueryErrorCode::AuthRequired,
                false,
                None,
                "authentication required to read recent history",
            ));
        };

        let limit = request
            .limit
            .map(|l| l as usize)
            .unwrap_or(50)
            .min(MAX_PAGE_SIZE as usize);

        let cache_generation = *self.catalog_cache_generation.lock().await;
        if !ctx.offline
            && let Some(active) = ctx.active.as_ref()
        {
            match self
                .run_backend_call(active, cancel, self.backend.recent_history(&active.session))
                .await
            {
                Ok(mut items) => {
                    let cache = self.catalog_cache_for(cache_generation).await;
                    items.truncate(limit);
                    let result = RecentHistoryResult {
                        items,
                        freshness: Freshness::Fresh,
                    };
                    if cache.is_some()
                        && let Ok(mut guard) = self.state.write()
                        && !active.session_cancel.is_cancelled()
                    {
                        guard.recent_history_cache = Some(result.clone());
                    }
                    return Ok(CatalogResponsePayload::RecentHistory(result));
                }
                Err(err) if is_offline_fallback_error(&err) => {
                    if let Some(fallback) = self.recent_history_fallback(scope, limit).await? {
                        return Ok(CatalogResponsePayload::RecentHistory(fallback));
                    }
                    return Err(err);
                }
                Err(err) => return Err(err),
            }
        }

        if let Some(fallback) = self.recent_history_fallback(scope, limit).await? {
            return Ok(CatalogResponsePayload::RecentHistory(fallback));
        }

        Err(make_query_error(
            if ctx.active.is_some() {
                QueryErrorCode::NetworkOffline
            } else {
                QueryErrorCode::AuthRequired
            },
            ctx.active.is_some(),
            None,
            "recent history unavailable offline without cached data",
        ))
    }

    async fn recent_history_fallback(
        &self,
        scope: &AccountScope,
        limit: usize,
    ) -> Result<Option<RecentHistoryResult>, QueryError> {
        if let Ok(guard) = self.state.read()
            && guard
                .active
                .as_ref()
                .is_some_and(|a| &a.account_scope == scope)
            && let Some(cached) = &guard.recent_history_cache
        {
            let mut items = cached.items.clone();
            items.truncate(limit);
            return Ok(Some(RecentHistoryResult {
                items,
                freshness: Freshness::Stale,
            }));
        }

        let local_recent = self.storage.list_recent_playback(scope, limit).await?;
        if !local_recent.is_empty() {
            return Ok(Some(RecentHistoryResult {
                items: local_recent.into_iter().map(|r| r.track).collect(),
                freshness: Freshness::Stale,
            }));
        }

        Ok(None)
    }

    async fn query_home(
        &self,
        ctx: &ResolvedQueryContext,
        request: &CatalogQueryRequest,
        cancel: &CancellationToken,
    ) -> Result<CatalogResponsePayload, QueryError> {
        let Some(scope) = ctx.cache_scope.as_ref() else {
            return Ok(CatalogResponsePayload::Home(HomeFeed {
                personalized_supported: false,
                fallback_notice: Some(HOME_FALLBACK_NOTICE.to_string()),
                recent_tracks: Vec::new(),
                pinned_playlists: Vec::new(),
                freshness: Freshness::Stale,
            }));
        };

        let limit = request
            .limit
            .map(|l| l as usize)
            .unwrap_or(12)
            .min(MAX_PAGE_SIZE as usize);

        let local_recent = self.storage.list_recent_playback(scope, limit).await?;
        let mut recent_tracks: Vec<Track> = local_recent.into_iter().map(|r| r.track).collect();
        let mut seen_video_ids: HashSet<String> = recent_tracks
            .iter()
            .map(|t| t.video_id.as_str().to_string())
            .collect();

        let mut pinned_playlists: Vec<PlaylistSummary> = Vec::new();
        let mut freshness = Freshness::Fresh;

        let cache_generation = *self.catalog_cache_generation.lock().await;
        if !ctx.offline
            && let Some(active) = ctx.active.as_ref()
        {
            let mut provider_recent_tracks = Vec::new();
            let pl_res = self
                .run_backend_call(
                    active,
                    cancel,
                    self.backend.library_playlists(&active.session, None),
                )
                .await;
            match pl_res {
                Ok(page) => {
                    pinned_playlists = page.items.into_iter().take(limit).collect();
                }
                Err(err) if is_offline_fallback_error(&err) => {
                    freshness = Freshness::Stale;
                    if let Some(cached_pl) = self
                        .storage
                        .get_library_playlists(
                            scope,
                            &self.config.locale,
                            None,
                            MAX_PAGE_SIZE as usize,
                        )
                        .await?
                    {
                        pinned_playlists = cached_pl
                            .value
                            .items
                            .into_iter()
                            .filter(is_user_library_playlist)
                            .take(limit)
                            .collect();
                    }
                }
                Err(err) => return Err(err),
            }

            if recent_tracks.len() < limit {
                match self
                    .run_backend_call(active, cancel, self.backend.recent_history(&active.session))
                    .await
                {
                    Ok(hist_tracks) => {
                        for track in hist_tracks {
                            if recent_tracks.len() + provider_recent_tracks.len() >= limit {
                                break;
                            }
                            if seen_video_ids.insert(track.video_id.as_str().to_string()) {
                                provider_recent_tracks.push(track);
                            }
                        }
                    }
                    Err(err) if is_offline_fallback_error(&err) => {
                        freshness = Freshness::Stale;
                    }
                    Err(err) => {
                        debug!(error = %err.safe_message, "optional recent history fetch skipped on Home fallback");
                    }
                }
            }

            // Keep local history in its own table. A Home request may have read those rows
            // before a deletion, so its durable fallback must contain only provider entries.
            let mut feed = HomeFeed {
                personalized_supported: false,
                fallback_notice: Some(HOME_FALLBACK_NOTICE.to_string()),
                recent_tracks: provider_recent_tracks,
                pinned_playlists,
                freshness,
            };
            let cache = self.catalog_cache_for(cache_generation).await;
            let cache_ctx = self.cache_context(active);
            if cache.is_some()
                && let Err(err) = self.storage.put_home_feed(&cache_ctx, &feed, None).await
            {
                warn!(error = %err.safe_message, "failed to persist home feed fallback cache");
            }
            recent_tracks.append(&mut feed.recent_tracks);
            feed.recent_tracks = recent_tracks;
            return Ok(CatalogResponsePayload::Home(feed));
        }

        // Offline / cached path
        if let Some(cached_pl) = self
            .storage
            .get_library_playlists(scope, &self.config.locale, None, MAX_PAGE_SIZE as usize)
            .await?
        {
            pinned_playlists = cached_pl
                .value
                .items
                .into_iter()
                .filter(is_user_library_playlist)
                .take(limit)
                .collect();
        }
        if (recent_tracks.is_empty() || pinned_playlists.is_empty())
            && let Some(cached_home) = self
                .storage
                .get_home_feed(scope, &self.config.locale)
                .await?
        {
            let remaining = limit.saturating_sub(recent_tracks.len());
            recent_tracks.extend(
                cached_home
                    .value
                    .recent_tracks
                    .into_iter()
                    .filter(|track| seen_video_ids.insert(track.video_id.as_str().to_string()))
                    .take(remaining),
            );
            if pinned_playlists.is_empty() {
                pinned_playlists = cached_home
                    .value
                    .pinned_playlists
                    .into_iter()
                    .filter(is_user_library_playlist)
                    .take(limit)
                    .collect();
            }
        }

        Ok(CatalogResponsePayload::Home(HomeFeed {
            personalized_supported: false,
            fallback_notice: Some(HOME_FALLBACK_NOTICE.to_string()),
            recent_tracks,
            pinned_playlists,
            freshness: Freshness::Stale,
        }))
    }
}

const LIKED_MUSIC_PLAYLIST_ID: &str = "LM";

fn invalid_position() -> QueryError {
    make_query_error(
        QueryErrorCode::InvalidPayload,
        false,
        None,
        "playlist position exceeds the supported range",
    )
}

/// Every cover reference in a catalog response, so each gets a URL-derived asset id that the
/// UI can load through `app://covers/<assetId>`.
fn response_covers(response: &mut CatalogResponsePayload) -> Vec<&mut CoverRef> {
    fn tracks(items: &mut [Track]) -> impl Iterator<Item = &mut CoverRef> {
        items.iter_mut().filter_map(|t| t.cover.as_mut())
    }
    fn playlists(items: &mut [PlaylistSummary]) -> impl Iterator<Item = &mut CoverRef> {
        items.iter_mut().filter_map(|p| p.cover.as_mut())
    }
    match response {
        CatalogResponsePayload::Home(feed) => tracks(&mut feed.recent_tracks)
            .chain(playlists(&mut feed.pinned_playlists))
            .collect(),
        CatalogResponsePayload::SearchSongs(res) => tracks(&mut res.songs.items)
            .chain(playlists(&mut res.playlists))
            .collect(),
        CatalogResponsePayload::LibraryPlaylists(page) => playlists(&mut page.items).collect(),
        CatalogResponsePayload::PlaylistTracks(detail) => detail
            .cover
            .as_mut()
            .into_iter()
            .chain(tracks(&mut detail.tracks.items))
            .collect(),
        CatalogResponsePayload::AlbumDetail(album) => album
            .cover
            .as_mut()
            .into_iter()
            .chain(tracks(&mut album.tracks))
            .collect(),
        CatalogResponsePayload::ArtistDetail(artist) => artist
            .cover
            .as_mut()
            .into_iter()
            .chain(tracks(&mut artist.top_songs))
            .collect(),
        CatalogResponsePayload::LikedSongs(page) => tracks(&mut page.items).collect(),
        CatalogResponsePayload::RecentHistory(hist) => tracks(&mut hist.items).collect(),
        CatalogResponsePayload::Error(_) => Vec::new(),
    }
}

fn page_limit(limit: Option<u32>) -> usize {
    limit.unwrap_or(MAX_PAGE_SIZE) as usize
}

fn stale_provider_cursor() -> QueryError {
    make_query_error(
        QueryErrorCode::StaleContext,
        false,
        None,
        "catalog cursor expired or provider results changed; restart without a cursor",
    )
}

fn extended_media_limit(
    provider: &BackendExtendedPage,
    skip: usize,
    limit: usize,
) -> Result<usize, QueryError> {
    let exceeds_page = || {
        make_query_error(
            QueryErrorCode::InvalidPayload,
            false,
            None,
            "an extended media item is too large for a control page",
        )
    };
    let entity_size = serde_json::to_vec(&provider.entity)
        .map_err(|_| exceeds_page())?
        .len();
    let mut remaining = MAX_EXTENDED_PAGE_BYTES
        .checked_sub(entity_size)
        .ok_or_else(exceeds_page)?;
    let mut count = 0;
    for item in provider.items.iter().skip(skip).take(limit) {
        let size = serde_json::to_vec(item).map_err(|_| exceeds_page())?.len() + 1;
        let Some(budget) = remaining.checked_sub(size) else {
            if count == 0 {
                return Err(exceeds_page());
            }
            break;
        };
        remaining = budget;
        count += 1;
    }
    Ok(count.max(1))
}

fn page_read_error(position: &CursorPosition, error: QueryError) -> QueryError {
    if position.token.is_some()
        && matches!(
            error.code,
            QueryErrorCode::InvalidPayload | QueryErrorCode::UnavailableTrack
        )
    {
        stale_provider_cursor()
    } else {
        error
    }
}

fn mood_scope_key(epoch: &AccountEpoch, scope: &AccountScope, locale: &str) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"ytmcp:mood-category:v1");
    for field in [epoch.as_str(), scope.as_str(), locale] {
        hash.update((field.len() as u64).to_be_bytes());
        hash.update(field.as_bytes());
    }
    hash.finalize().into()
}

fn bounded_provider_window<T: serde::Serialize>(
    items: Vec<T>,
    upstream: Option<String>,
    fully_projected: bool,
    position: &CursorPosition,
    limit: usize,
) -> Result<(Vec<T>, Option<CursorPosition>, bool), QueryError> {
    let fingerprint = if position.skip > 0 || items.len() > limit {
        let bytes = serde_json::to_vec(&items).map_err(|_| stale_provider_cursor())?;
        Some(hex::encode(Sha256::digest(bytes)))
    } else {
        None
    };
    if position.skip > 0
        && (position.window_fingerprint != fingerprint || position.skip >= items.len())
    {
        return Err(stale_provider_cursor());
    }
    let repeated = upstream.is_some() && upstream == position.token;
    let incomplete = position.incomplete || !fully_projected || repeated;
    let (items, mut next) = take_window(items, position, limit, upstream.filter(|_| !repeated));
    if let Some(next) = &mut next {
        next.incomplete = incomplete;
        if next.skip > 0 {
            next.window_fingerprint = fingerprint;
        }
    }
    let complete = next.is_none() && !incomplete;
    Ok((items, next, complete))
}

/// Returns at most `limit` items of one upstream page starting at `position.skip`, plus where
/// the following page starts (the rest of this upstream page, or the next upstream page).
fn take_window<T>(
    items: Vec<T>,
    position: &CursorPosition,
    limit: usize,
    next_upstream: Option<String>,
) -> (Vec<T>, Option<CursorPosition>) {
    let mut rest: Vec<T> = items.into_iter().skip(position.skip).collect();
    if rest.len() > limit {
        rest.truncate(limit);
        let next = CursorPosition {
            token: position.token.clone(),
            skip: position.skip + limit,
            ..CursorPosition::default()
        };
        (rest, Some(next))
    } else {
        let next = next_upstream.map(|token| CursorPosition {
            token: Some(token),
            ..CursorPosition::default()
        });
        (rest, next)
    }
}

fn is_user_library_playlist(playlist: &PlaylistSummary) -> bool {
    !is_system_library_playlist(playlist.playlist_id.as_str())
}

struct ResolvedQueryContext {
    active: Option<ActiveSession>,
    cache_scope: Option<AccountScope>,
    offline: bool,
}

impl CatalogPort for YtmCatalogAdapter {
    fn query(
        &self,
        account_epoch: Option<AccountEpoch>,
        account_scope: Option<AccountScope>,
        request: CatalogQueryRequest,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<CatalogResponsePayload, QueryError>> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err(make_query_error(
                    QueryErrorCode::StaleContext,
                    false,
                    None,
                    "catalog query cancelled before dispatch",
                ));
            }
            if let Some(limit) = request.limit
                && (limit == 0 || limit > MAX_PAGE_SIZE)
            {
                return Err(make_query_error(
                    QueryErrorCode::InvalidPayload,
                    false,
                    None,
                    format!("catalog query limit {limit} outside [1, {MAX_PAGE_SIZE}]"),
                ));
            }

            let ctx = self.resolve_query_context(account_epoch.as_ref(), account_scope.as_ref())?;
            let mut response = match request.kind {
                CatalogQueryKind::Home => self.query_home(&ctx, &request, &cancel).await,
                CatalogQueryKind::SearchSongs => {
                    self.query_search_songs(&ctx, &request, &cancel).await
                }
                CatalogQueryKind::LibraryPlaylists => {
                    self.query_library_playlists(&ctx, &request, &cancel).await
                }
                CatalogQueryKind::PlaylistTracks => {
                    self.query_playlist_tracks(&ctx, &request, &cancel).await
                }
                CatalogQueryKind::AlbumDetail => {
                    self.query_album_detail(&ctx, &request, &cancel).await
                }
                CatalogQueryKind::ArtistDetail => {
                    self.query_artist_detail(&ctx, &request, &cancel).await
                }
                CatalogQueryKind::LikedSongs => {
                    self.query_liked_songs(&ctx, &request, &cancel).await
                }
                CatalogQueryKind::RecentHistory => {
                    self.query_recent_history(&ctx, &request, &cancel).await
                }
            }?;
            if let Err(err) = self
                .storage
                .register_cover_urls(response_covers(&mut response))
                .await
            {
                warn!(error = %err.safe_message, "failed to register cover assets");
            }
            Ok(response)
        })
    }

    fn mutate(
        &self,
        account_epoch: AccountEpoch,
        account_scope: AccountScope,
        request: MutationRequest,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<MutationResult, QueryError>> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err(make_query_error(
                    QueryErrorCode::StaleContext,
                    false,
                    None,
                    "mutation cancelled before dispatch",
                ));
            }
            if !request.confirmed_by_user {
                return Err(make_query_error(
                    QueryErrorCode::InvalidPayload,
                    false,
                    None,
                    "account mutation requires confirmedByUser=true",
                ));
            }

            let (active, offline) = {
                let guard = self.state.read().map_err(|_| {
                    make_query_error(
                        QueryErrorCode::StorageError,
                        false,
                        None,
                        "catalog adapter lock poisoned",
                    )
                })?;
                let active = guard.active.clone().ok_or_else(|| {
                    make_query_error(
                        QueryErrorCode::AuthRequired,
                        false,
                        None,
                        "authentication required before mutating YouTube Music library",
                    )
                })?;
                (active, guard.network_offline)
            };

            if active.account_epoch != account_epoch {
                return Err(make_query_error(
                    QueryErrorCode::StaleContext,
                    false,
                    None,
                    "mutation accountEpoch does not match active session",
                ));
            }
            if active.account_scope != account_scope {
                return Err(make_query_error(
                    QueryErrorCode::AccountMismatch,
                    false,
                    None,
                    "mutation accountScope does not match active session",
                ));
            }
            if offline {
                return Err(make_query_error(
                    QueryErrorCode::NetworkOffline,
                    true,
                    Some(1_000),
                    "cannot execute library mutation while offline",
                ));
            }

            let res = match request.kind {
                MutationKind::SetLike => {
                    let liked = request.liked.ok_or_else(|| {
                        make_query_error(
                            QueryErrorCode::InvalidPayload,
                            false,
                            None,
                            "setLike mutation requires liked=true|false",
                        )
                    })?;
                    self.run_backend_call(
                        &active,
                        &cancel,
                        self.backend
                            .set_like(&active.session, &request.video_id, liked),
                    )
                    .await
                }
                MutationKind::AddToPlaylist => {
                    let playlist_id = request.playlist_id.as_ref().ok_or_else(|| {
                        make_query_error(
                            QueryErrorCode::InvalidPayload,
                            false,
                            None,
                            "addToPlaylist mutation requires playlistId",
                        )
                    })?;
                    self.run_backend_call(
                        &active,
                        &cancel,
                        self.backend.add_to_playlist(
                            &active.session,
                            playlist_id,
                            &request.video_id,
                        ),
                    )
                    .await
                }
            };

            match res {
                Ok(()) => Ok(MutationResult {
                    kind: request.kind,
                    outcome: MutationOutcome::Confirmed,
                    error: None,
                }),
                // The service answered and refused: nothing was applied.
                Err(err)
                    if matches!(
                        err.code,
                        QueryErrorCode::UnavailableTrack | QueryErrorCode::RateLimited
                    ) =>
                {
                    Ok(MutationResult {
                        kind: request.kind,
                        outcome: MutationOutcome::Rejected,
                        error: Some(err),
                    })
                }
                // Credentials were rejected: surfaced as an error so the core expires the session.
                Err(err)
                    if matches!(
                        err.code,
                        QueryErrorCode::AuthExpired | QueryErrorCode::AuthRequired
                    ) =>
                {
                    Err(err)
                }
                // Timeout, transport failure, cancellation in flight or an unreadable response:
                // the request may have been applied, so the outcome is unknown and never retried.
                Err(err) => Ok(MutationResult {
                    kind: request.kind,
                    outcome: MutationOutcome::Indeterminate,
                    error: Some(err),
                }),
            }
        })
    }

    fn official_lyrics(
        &self,
        account_epoch: AccountEpoch,
        video_id: ytm_protocol::VideoId,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<Option<OfficialLyrics>, QueryError>> {
        Box::pin(async move {
            let (active, offline) = {
                let guard = self.state.read().map_err(|_| {
                    make_query_error(
                        QueryErrorCode::StorageError,
                        false,
                        None,
                        "catalog adapter lock poisoned",
                    )
                })?;
                (guard.active.clone(), guard.network_offline)
            };
            let Some(active) = active.filter(|a| a.account_epoch == account_epoch) else {
                return Err(make_query_error(
                    QueryErrorCode::AuthRequired,
                    false,
                    None,
                    "official lyrics need the active account session",
                ));
            };
            if offline {
                return Err(make_query_error(
                    QueryErrorCode::NetworkOffline,
                    true,
                    None,
                    "official lyrics unavailable offline",
                ));
            }
            self.run_backend_call(
                &active,
                &cancel,
                self.backend.official_lyrics(&active.session, &video_id),
            )
            .await
        })
    }

    fn apply_session_material(
        &self,
        material: SessionMaterialPayload,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<CatalogIdentity, QueryError>> {
        Box::pin(async move {
            let cookie_header = format_cookie_header(&material.cookie_pairs)?;
            let superseded = || {
                make_query_error(
                    QueryErrorCode::StaleContext,
                    false,
                    None,
                    "session binding superseded",
                )
            };
            let session = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(superseded()),
                result = self.backend.authenticate(&cookie_header) => result,
            };
            self.note_upstream_result(&session);
            let session = session?;
            // An epoch's identity is fixed by its first applied session. The session's own account
            // is still read every time: a different account's cookies under the same epoch (a
            // switch the host saw no sign-out for) must not be served under the epoch's scope.
            let same_epoch_identity = self.state.read().ok().and_then(|guard| {
                guard
                    .active
                    .as_ref()
                    .filter(|a| a.account_epoch == material.account_epoch)
                    .map(|a| CatalogIdentity {
                        account_scope: a.account_scope.clone(),
                        account_hint: a.account_hint.clone(),
                    })
            });
            // DATASYNC_ID is the identity key shared with the bridge. A failed read fails the apply
            // (nothing is registered, the next material retries); only a response without the
            // field makes an unverified, per-epoch scope.
            let datasync_id = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(superseded()),
                result = self.backend.fetch_datasync_id(&session) => result,
            };
            self.note_upstream_result(&datasync_id);
            let datasync_id = datasync_id?;
            let identity = match (same_epoch_identity, datasync_id.as_deref()) {
                (Some(bound), Some(id))
                    if bound.account_hint.is_some()
                        && bound.account_scope != account_scope_for(id) =>
                {
                    self.unbind_changed_account(&material.account_epoch);
                    return Err(make_query_error(
                        QueryErrorCode::AuthRequired,
                        false,
                        None,
                        "the signed-in account changed without a new account epoch",
                    ));
                }
                (Some(bound), _) => bound,
                (None, id) => {
                    if id.is_none() {
                        warn!(
                            "catalog response carried no account identity; session stays unverified"
                        );
                    }
                    CatalogIdentity {
                        account_scope: id
                            .map(account_scope_for)
                            .unwrap_or_else(|| derive_account_scope(&material.account_epoch)),
                        account_hint: id.map(account_hint_for),
                    }
                }
            };

            // Keep SQLite epoch registration and credential installation in the same bind order.
            let _commit = self.session_commit.lock().await;
            if cancel.is_cancelled() {
                return Err(superseded());
            }
            // Register first so invalidated/replayed epochs (or an epoch reused for a different
            // account) are rejected before credentials are bound.
            self.storage
                .register_account_epoch(&identity.account_scope, &material.account_epoch)
                .await?;

            let prev_active = {
                let mut guard = self.state.write().map_err(|_| {
                    make_query_error(
                        QueryErrorCode::StorageError,
                        false,
                        None,
                        "catalog adapter lock poisoned",
                    )
                })?;
                // Logout cancels the binding, then clears the active session under this lock.
                if cancel.is_cancelled() {
                    return Err(superseded());
                }
                let prev = guard.active.take();
                // Same-account credential rotation refreshes the HTTP session without cancelling
                // in-flight catalog requests; only a different accountEpoch cancels them.
                let session_cancel = match prev.as_ref() {
                    Some(p) if p.account_epoch == material.account_epoch => {
                        p.session_cancel.clone()
                    }
                    Some(p) => {
                        p.session_cancel.cancel();
                        guard.recent_history_cache = None;
                        guard.discovery_windows.clear();
                        guard.typed_search_windows.clear();
                        CancellationToken::new()
                    }
                    None => CancellationToken::new(),
                };
                guard.active = Some(ActiveSession {
                    session,
                    account_epoch: material.account_epoch.clone(),
                    account_scope: identity.account_scope.clone(),
                    account_hint: identity.account_hint.clone(),
                    credential_revision: material.credential_revision,
                    session_cancel,
                });
                prev
            };

            if let Some(prev) = prev_active
                && prev.account_epoch != material.account_epoch
            {
                let _ = self
                    .storage
                    .invalidate_account_epoch(&prev.account_epoch)
                    .await;
            }

            Ok(identity)
        })
    }

    fn fence_cache_writes(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let mut generation = self.catalog_cache_generation.lock().await;
            *generation = generation.wrapping_add(1);
        })
    }

    fn clear_memory_cache(&self, scope: Option<&AccountScope>) -> Result<(), QueryError> {
        let mut guard = self.state.write().map_err(|_| {
            make_query_error(
                QueryErrorCode::StorageError,
                false,
                None,
                "catalog adapter lock poisoned",
            )
        })?;
        if scope.is_none() || guard.active.as_ref().map(|a| &a.account_scope) == scope {
            guard.recent_history_cache = None;
            guard.discovery_windows.clear();
            guard.typed_search_windows.clear();
        }
        Ok(())
    }

    fn clear_session_material(
        &self,
        expected_epoch: Option<AccountEpoch>,
    ) -> Result<(), QueryError> {
        let mut guard = self.state.write().map_err(|_| {
            make_query_error(
                QueryErrorCode::StorageError,
                false,
                None,
                "catalog adapter lock poisoned",
            )
        })?;
        if expected_epoch
            .as_ref()
            .is_some_and(|epoch| guard.active.as_ref().map(|a| &a.account_epoch) != Some(epoch))
        {
            return Ok(());
        }
        if let Some(prev) = guard.active.take() {
            prev.session_cancel.cancel();
        }
        guard.recent_history_cache = None;
        guard.discovery_windows.clear();
        guard.typed_search_windows.clear();
        Ok(())
    }
}

fn is_offline_fallback_error(err: &QueryError) -> bool {
    matches!(
        err.code,
        QueryErrorCode::NetworkOffline | QueryErrorCode::Timeout
    )
}
