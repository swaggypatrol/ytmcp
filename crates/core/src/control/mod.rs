mod account_handoff;
mod account_limits;
mod activity;
pub mod catalog;
mod catalog_locks;
mod catalog_views;
mod collection;
mod cues;
mod dj_feedback;
pub mod dj_store;
pub mod history;
pub mod journal;
pub mod lyrics;
mod membership;
mod moment;
pub mod moment_store;
mod playlist;
mod playlist_edits;
mod queue;
mod rating;
mod recovery;
mod share;
mod state;
mod subscription;
mod write;
pub use write::{DispatchAuthority, DispatchGrant};

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use ytm_control_api::{
    ControlError, ControlRequest, ControlResponse, ErrorCode, Page, PageInput, RetryDisposition,
};
use ytm_protocol::{
    AccountEpoch, AccountScope, BridgeEpoch, CatalogQueryKind, CatalogQueryRequest,
    CatalogResponsePayload, CoreEpoch, QueryError, QueryErrorCode, StateDelta, TrackInstanceId,
    VideoId,
};

use crate::{CoreEngine, engine::ControlRead};
use catalog::{CatalogReadContext, ControlCatalog};
use journal::{OperationKey, OperationStore};
use state::{
    account_projection, capabilities, context_projection, player_view, queue_projection,
    track_projection,
};

const CONTEXT_TTL_MS: u64 = 5 * 60 * 1000;
const CONTEXT_LIMIT: usize = 512;

/// Created by a trusted transport only. A signed-out grant cannot follow subsequent sign-ins.
#[derive(Clone, Debug)]
pub struct ReadGrant {
    principal: String,
    scope: Option<AccountScope>,
}

impl ReadGrant {
    pub fn new(principal: String, scope: Option<AccountScope>) -> Self {
        Self { principal, scope }
    }
}

#[derive(Clone)]
pub struct ContextBinding {
    pub core_epoch: CoreEpoch,
    pub account_epoch: Option<AccountEpoch>,
    pub bridge_epoch: Option<BridgeEpoch>,
    pub track_instance_id: Option<TrackInstanceId>,
    pub video_id: Option<VideoId>,
    pub track: Option<ytm_control_api::Track>,
    pub queue_revision: u64,
}

struct SavedContext {
    grant: ReadGrant,
    binding: ContextBinding,
    expires_at: u64,
}

pub struct ControlService {
    core: CoreEngine,
    catalog: Option<Arc<dyn ControlCatalog>>,
    lyrics: Option<Arc<dyn lyrics::ControlLyrics>>,
    operations: Option<Arc<dyn OperationStore>>,
    dj_store: Option<Arc<dyn dj_store::DjStore>>,
    moment_store: Option<Arc<dyn moment_store::MomentStore>>,
    history: Option<Arc<dyn history::LocalHistoryStore>>,
    write_slots: Arc<tokio::sync::Semaphore>,
    catalog_locks: catalog_locks::CatalogLocks,
    active_writes: Mutex<HashMap<OperationKey, CancellationToken>>,
    contexts: Mutex<HashMap<String, SavedContext>>,
    /// Clients with an account handoff waiting for the user; one at a time per client.
    pending_handoffs: Mutex<std::collections::HashSet<(String, Option<AccountScope>)>>,
    /// Serializes the daily-limit check with the journal claim it admits.
    account_limits: tokio::sync::Mutex<()>,
    cues: cues::SharedCues,
}

impl ControlService {
    pub fn new(core: CoreEngine) -> Self {
        Self {
            core,
            catalog: None,
            lyrics: None,
            operations: None,
            dj_store: None,
            moment_store: None,
            history: None,
            write_slots: Self::write_semaphore(),
            catalog_locks: catalog_locks::CatalogLocks::default(),
            active_writes: Mutex::new(HashMap::new()),
            contexts: Mutex::new(HashMap::new()),
            pending_handoffs: Mutex::new(std::collections::HashSet::new()),
            account_limits: tokio::sync::Mutex::new(()),
            cues: cues::new_cues(),
        }
    }

    pub fn with_lyrics(mut self, lyrics: Arc<dyn lyrics::ControlLyrics>) -> Self {
        self.lyrics = Some(lyrics);
        self
    }

    pub fn with_catalog(mut self, catalog: Arc<dyn ControlCatalog>) -> Self {
        self.catalog = Some(catalog);
        self
    }

    pub fn with_dj_store(mut self, store: Arc<dyn dj_store::DjStore>) -> Self {
        self.dj_store = Some(store);
        self
    }

    pub fn with_moment_store(mut self, store: Arc<dyn moment_store::MomentStore>) -> Self {
        self.moment_store = Some(store);
        self
    }

    pub fn with_local_history(mut self, history: Arc<dyn history::LocalHistoryStore>) -> Self {
        self.history = Some(history);
        self
    }

    pub fn with_operations(mut self, operations: Arc<dyn OperationStore>) -> Self {
        self.operations = Some(operations);
        self
    }

    pub async fn execute(&self, grant: &ReadGrant, request: ControlRequest) -> ControlResponse {
        self.read(grant, request)
            .await
            .unwrap_or_else(ControlResponse::Error)
    }

    async fn read(
        &self,
        grant: &ReadGrant,
        request: ControlRequest,
    ) -> Result<ControlResponse, ControlError> {
        use ControlRequest::*;
        let state = self.authorized_state(grant, false).await?;
        match request {
            GetTrack { target } => {
                use ytm_control_api::TrackTarget;
                let observed = state.snapshot.player.current_track.as_ref();
                let mut track = match target {
                    TrackTarget::Video { video_id } => observed
                        .filter(|track| track.video_id == video_id)
                        .map(track_projection),
                    TrackTarget::Context { context_token } => {
                        self.resolve_context(grant, &context_token).await?.track
                    }
                }
                .ok_or_else(|| {
                    ControlError::new(
                        ErrorCode::CapabilityUnavailable,
                        RetryDisposition::NewPlanRequired,
                        "this track is outside the current playback context",
                    )
                })?;
                let (rating, membership) =
                    if let Ok((catalog, context)) = self.catalog_context(&state) {
                        let (rating, membership) = tokio::join!(
                            catalog.fresh_rating(context.clone(), track.video_id.clone()),
                            catalog.song_membership(context, track.video_id.clone()),
                        );
                        (rating.ok(), membership.ok())
                    } else {
                        (None, None)
                    };
                self.check_read_context(grant, &state).await?;
                if let Some(evidence) = &rating {
                    track.rating = evidence.rating;
                }
                Ok(ControlResponse::Track(ytm_control_api::TrackView {
                    freshness: if observed.is_some_and(|current| current.video_id == track.video_id)
                        && state.snapshot.player.connection
                            == ytm_protocol::PlayerConnectionState::Ready
                    {
                        ytm_protocol::Freshness::Fresh
                    } else {
                        ytm_protocol::Freshness::Stale
                    },
                    track,
                    source: "observed_playback_context".into(),
                    rating_observed_at_ms: rating.map(|evidence| evidence.observed_at_ms),
                    membership,
                }))
            }
            GetDiscovery(input) => {
                let (catalog, context) = self.catalog_context(&state)?;
                page_size(input.page.as_ref())?;
                let snapshot = catalog
                    .discover(context, input.kind, input.page.unwrap_or_else(default_page))
                    .await
                    .map_err(map_query_error)?;
                self.check_read_context(grant, &state).await?;
                Ok(ControlResponse::Discovery(snapshot))
            }
            GetMoodCategory(input) => {
                let (catalog, context) = self.catalog_context(&state)?;
                page_size(input.page.as_ref())?;
                let results = catalog
                    .mood_category(
                        context,
                        input.category_id,
                        input.page.unwrap_or_else(default_page),
                    )
                    .await
                    .map_err(map_query_error)?;
                self.check_read_context(grant, &state).await?;
                Ok(ControlResponse::Discovery(results))
            }
            GetSearchSuggestions(input) => {
                let (catalog, context) = self.catalog_context(&state)?;
                let limit = page_size(Some(&PageInput {
                    cursor: None,
                    limit: input.limit,
                }))? as u16;
                let results = catalog
                    .search_suggestions(context, input.query, limit)
                    .await
                    .map_err(map_query_error)?;
                self.check_read_context(grant, &state).await?;
                Ok(ControlResponse::SearchSuggestions(results))
            }
            GetExtendedMedia(input) => self
                .read_extended_media(grant, &state, input)
                .await
                .map(Box::new)
                .map(ControlResponse::ExtendedMedia),
            GetHistory(input) => self.read_history(grant, &state, input).await,
            GetAlbum(_) | GetArtist(_) | GetHome(_) | GetLibrary(_) => {
                self.read_catalog_view(grant, &state, request).await
            }
            GetLyrics(input) => self
                .read_lyrics(grant, &state, input)
                .await
                .map(ControlResponse::Lyrics),
            GetSettings => Ok(ControlResponse::Settings(
                ytm_control_api::SettingsSnapshot {
                    settings: state.snapshot.settings,
                    settings_revision: state.settings_revision,
                },
            )),
            GetCapabilities { area } => Ok(ControlResponse::Capabilities(
                capabilities(&state.snapshot)
                    .into_iter()
                    .filter(|cap| area.is_none_or(|a| a == cap.area))
                    .collect(),
            )),
            GetContext(input) => self.make_context(grant, &state, input.preview_limit).await,
            GetPlayer { include } => Ok(ControlResponse::Player(Box::new(player_view(
                &state.snapshot,
                include.unwrap_or_default(),
            )))),
            GetAccount => Ok(ControlResponse::Account(account_projection(
                &state.snapshot.auth_state,
            ))),
            GetQueue(input) => {
                let limit = page_size(Some(&input))?;
                let from = if let Some(cursor) = input.cursor.as_deref() {
                    validate_queue_cursor(cursor, &state)?
                } else {
                    0
                };
                Ok(ControlResponse::Queue(queue_projection(
                    &state.snapshot,
                    state.queue.as_ref(),
                    from,
                    limit,
                )))
            }
            Search(input) => {
                let query = checked_search_query(&input.query)?;
                let limit = page_size(input.page.as_ref())?;
                let request = CatalogQueryRequest {
                    kind: CatalogQueryKind::SearchSongs,
                    query: Some(query.to_owned()),
                    cursor: input.page.and_then(|page| page.cursor),
                    limit: Some(limit as u32),
                    playlist_id: None,
                    album_id: None,
                    artist_id: None,
                };
                let result = self
                    .core
                    .catalog_query(request, state.snapshot.account_epoch.clone())
                    .await
                    .map_err(map_query_error)?;
                let after = self.authorized_state(grant, false).await?;
                if state.snapshot.account_epoch != after.snapshot.account_epoch {
                    return Err(stale("account changed during search"));
                }
                let CatalogResponsePayload::SearchSongs(response) = result else {
                    return Err(unavailable("catalog returned an unexpected search result"));
                };
                let songs = response.songs;
                if songs.items.len() > limit {
                    return Err(unavailable(
                        "catalog did not preserve the requested page limit",
                    ));
                }
                Ok(ControlResponse::Search(Page {
                    items: songs.items.iter().map(track_projection).collect(),
                    complete: songs.next_cursor.is_none(),
                    next_cursor: songs.next_cursor,
                    source: "youtube_music_catalog".to_owned(),
                    freshness: songs.freshness,
                    captured_at_ms: songs.fetched_at_ms,
                }))
            }
            SearchEntities(input) => {
                let query = checked_search_query(&input.query)?.to_owned();
                let limit = page_size(input.page.as_ref())?;
                let (catalog, context) = self.catalog_context(&state)?;
                let page = catalog
                    .search_entities(
                        context,
                        input.kind,
                        query,
                        input.page.unwrap_or_else(default_page),
                    )
                    .await
                    .map_err(map_query_error)?;
                self.check_read_context(grant, &state).await?;
                if page.items.len() > limit {
                    return Err(unavailable(
                        "catalog did not preserve the requested page limit",
                    ));
                }
                Ok(ControlResponse::SearchEntities(page))
            }
            GetShareLink(input) => self
                .read_share_link(grant, &state, input)
                .await
                .map(ControlResponse::ShareLink),
            GetPlaylist(ytm_control_api::PlaylistInput { playlist_id, page }) => {
                let (catalog, context) = self.catalog_context(&state)?;
                page_size(page.as_ref())?;
                let playlist = catalog
                    .fresh_playlist(context, playlist_id, page.unwrap_or_else(default_page))
                    .await
                    .map_err(map_query_error)?;
                self.check_read_context(grant, &state).await?;
                Ok(ControlResponse::Playlist(playlist))
            }
            Recommend(ytm_control_api::RecommendationsInput { seed, page }) => {
                let (catalog, context) = self.catalog_context(&state)?;
                page_size(page.as_ref())?;
                let video_id = match seed {
                    ytm_control_api::TrackTarget::Video { video_id } => video_id,
                    ytm_control_api::TrackTarget::Context { context_token } => self
                        .resolve_context(grant, &context_token)
                        .await?
                        .video_id
                        .ok_or_else(|| invalid("the context has no track"))?,
                };
                let recommendations = catalog
                    .recommend_song_radio(context, video_id, page.unwrap_or_else(default_page))
                    .await
                    .map_err(map_query_error)?;
                self.check_read_context(grant, &state).await?;
                Ok(ControlResponse::Recommendations(recommendations))
            }
            GetOperation(ytm_control_api::OperationInput { operation_id }) => {
                let key = OperationKey {
                    principal: grant.principal.clone(),
                    account_scope: grant.scope.clone(),
                    operation_id,
                };
                let store = self
                    .operations
                    .as_ref()
                    .ok_or_else(|| unavailable("the operation journal is unavailable"))?;
                store
                    .get(key)
                    .await?
                    .map(ControlResponse::Operation)
                    .ok_or_else(|| invalid("no operation with this ID for this client and account"))
            }
            ReplayOperation(replay) => {
                let key = OperationKey {
                    principal: grant.principal.clone(),
                    account_scope: grant.scope.clone(),
                    operation_id: replay.operation_id.clone(),
                };
                let store = self
                    .operations
                    .as_ref()
                    .ok_or_else(|| unavailable("the operation journal is unavailable"))?;
                store
                    .check_replay(key, replay)
                    .await?
                    .map(ControlResponse::Operation)
                    .ok_or_else(|| invalid("no operation with this ID for this client and account"))
            }
            GetDjPreferences(input) => self
                .read_dj_preferences(grant, input)
                .await
                .map(ControlResponse::DjPreferences),
            NextDjCue(input) => self
                .next_dj_cue(grant, input)
                .await
                .map(|reply| ControlResponse::DjCue(Box::new(reply))),
            _ => Err(ControlError::new(
                ErrorCode::CapabilityUnavailable,
                RetryDisposition::NotRetryable,
                "this control method is unavailable",
            )),
        }
    }

    fn catalog_context(
        &self,
        state: &ControlRead,
    ) -> Result<(&dyn ControlCatalog, CatalogReadContext), ControlError> {
        let catalog = self
            .catalog
            .as_deref()
            .ok_or_else(|| unavailable("the music catalog is unavailable"))?;
        let auth = &state.snapshot.auth_state;
        let (Some(account_epoch), Some(account_scope)) =
            (auth.account_epoch.clone(), auth.account_scope.clone())
        else {
            return Err(ControlError::new(
                ErrorCode::AuthRequired,
                RetryDisposition::ReauthRequired,
                "sign in to YouTube Music first",
            ));
        };
        Ok((
            catalog,
            CatalogReadContext {
                account_scope,
                account_epoch,
                cancel: CancellationToken::new(),
            },
        ))
    }

    async fn check_read_context(
        &self,
        grant: &ReadGrant,
        state: &ControlRead,
    ) -> Result<(), ControlError> {
        let after = self.authorized_state(grant, false).await?;
        if after.snapshot.account_epoch != state.snapshot.account_epoch {
            return Err(stale("account changed during the catalog request"));
        }
        Ok(())
    }

    pub async fn subscribe(
        &self,
        grant: &ReadGrant,
        preview_limit: Option<u8>,
    ) -> Result<(ControlResponse, broadcast::Receiver<StateDelta>), ControlError> {
        let mut state = self.authorized_state(grant, true).await?;
        let events = state
            .events
            .take()
            .ok_or_else(|| unavailable("no event stream"))?;
        Ok((
            self.make_context(grant, &state, preview_limit).await?,
            events,
        ))
    }

    async fn authorized_state(
        &self,
        grant: &ReadGrant,
        subscribe: bool,
    ) -> Result<ControlRead, ControlError> {
        let state = self
            .core
            .read_control(subscribe)
            .await
            .map_err(|_| unavailable("player is unavailable"))?;
        if state.snapshot.auth_state.account_scope != grant.scope {
            return Err(ControlError::new(
                ErrorCode::PermissionDenied,
                RetryDisposition::NotRetryable,
                "the player account does not match the client grant",
            ));
        }
        Ok(state)
    }

    /// Entries after the current occurrence at `revision`, counted on the full queue mirror
    /// rather than the bounded preview, when the explicit list's end is loaded (an autoplay
    /// tail after it does not count).
    pub async fn upcoming_in_complete_queue(
        &self,
        grant: &ReadGrant,
        revision: u64,
    ) -> Option<usize> {
        let state = self.authorized_state(grant, false).await.ok()?;
        let queue = state.queue.as_ref().filter(|queue| {
            queue.queue_revision == revision && crate::commands::explicit_end_loaded(queue)
        })?;
        let current = queue.current_entry_id.as_ref()?;
        let index = queue
            .entries
            .iter()
            .position(|entry| &entry.entry_id == current)?;
        Some(queue.entries.len() - index - 1)
    }

    pub async fn account_is_allowed(&self, grant: &ReadGrant) -> bool {
        self.authorized_state(grant, false).await.is_ok()
    }

    async fn make_context(
        &self,
        grant: &ReadGrant,
        state: &ControlRead,
        preview_limit: Option<u8>,
    ) -> Result<ControlResponse, ControlError> {
        let limit = usize::from(preview_limit.unwrap_or(ytm_control_api::DEFAULT_PAGE_SIZE as u8));
        if limit > usize::from(ytm_control_api::DEFAULT_PAGE_SIZE) {
            return Err(invalid("previewLimit must be between 0 and 20"));
        }
        let snapshot = &state.snapshot;
        let token = Uuid::new_v4().to_string();
        let binding = ContextBinding {
            core_epoch: snapshot.core_epoch.clone(),
            account_epoch: snapshot.account_epoch.clone(),
            bridge_epoch: snapshot.player.bridge_epoch.clone(),
            track_instance_id: snapshot.player.track_instance_id.clone(),
            video_id: snapshot
                .player
                .current_track
                .as_ref()
                .map(|track| track.video_id.clone()),
            track: snapshot.player.current_track.as_ref().map(track_projection),
            queue_revision: snapshot.queue_summary.queue_revision,
        };
        let now = self.core.clock().now_ms();
        {
            let mut contexts = self
                .contexts
                .lock()
                .map_err(|_| unavailable("context storage is unavailable"))?;
            contexts.retain(|_, saved| saved.expires_at > now);
            if contexts.len() == CONTEXT_LIMIT {
                let oldest = contexts
                    .iter()
                    .min_by_key(|(_, context)| context.expires_at)
                    .map(|(token, _)| token.clone());
                if let Some(oldest) = oldest {
                    contexts.remove(&oldest);
                }
            }
            contexts.insert(
                token.clone(),
                SavedContext {
                    grant: grant.clone(),
                    binding,
                    expires_at: now.saturating_add(CONTEXT_TTL_MS),
                },
            );
        }
        let moment = Some(self.project_moment(grant, state).await);
        Ok(ControlResponse::Context(Box::new(context_projection(
            snapshot,
            state.queue.as_ref(),
            state.captured_at_ms,
            token,
            limit,
            moment,
        ))))
    }

    pub async fn resolve_context(
        &self,
        grant: &ReadGrant,
        token: &str,
    ) -> Result<ContextBinding, ControlError> {
        let state = self.authorized_state(grant, false).await?;
        let contexts = self
            .contexts
            .lock()
            .map_err(|_| unavailable("context storage is unavailable"))?;
        let saved = contexts
            .get(token)
            .filter(|saved| {
                saved.grant.principal == grant.principal
                    && saved.grant.scope == grant.scope
                    && saved.expires_at > self.core.clock().now_ms()
            })
            .ok_or_else(|| {
                ControlError::new(
                    ErrorCode::ContextExpired,
                    RetryDisposition::NewPlanRequired,
                    "the context token expired or does not belong to this client",
                )
            })?;
        if saved.binding.account_epoch != state.snapshot.account_epoch
            || saved.binding.bridge_epoch != state.snapshot.player.bridge_epoch
        {
            return Err(stale(
                "the playback or account session changed since context capture",
            ));
        }
        Ok(saved.binding.clone())
    }
}

fn validate_queue_cursor(cursor: &str, state: &ControlRead) -> Result<usize, ControlError> {
    let bridge = state
        .queue
        .as_ref()
        .and_then(|queue| queue.bridge_epoch.as_ref())
        .ok_or_else(|| stale("a queue cursor requires a live bridge"))?;
    let prefix = format!(
        "{}/{}/{}/",
        state.snapshot.core_epoch, bridge, state.snapshot.queue_summary.queue_revision
    );
    cursor
        .strip_prefix(&prefix)
        .and_then(|offset| offset.parse::<usize>().ok())
        .filter(|offset| *offset < state.snapshot.queue_summary.loaded_count as usize)
        .ok_or_else(|| stale("queue cursor no longer addresses the loaded queue revision"))
}

fn checked_search_query(query: &str) -> Result<&str, ControlError> {
    let query = query.trim();
    if query.is_empty() || query.chars().count() > 256 || query.chars().any(char::is_control) {
        return Err(invalid("search query must be 1–256 characters"));
    }
    Ok(query)
}

fn default_page() -> PageInput {
    PageInput {
        cursor: None,
        limit: Some(ytm_control_api::DEFAULT_PAGE_SIZE),
    }
}

fn page_size(page: Option<&PageInput>) -> Result<usize, ControlError> {
    let limit = page
        .and_then(|value| value.limit)
        .unwrap_or(ytm_control_api::DEFAULT_PAGE_SIZE);
    if !(1..=ytm_control_api::MAX_PAGE_SIZE).contains(&limit)
        || page
            .and_then(|value| value.cursor.as_ref())
            .is_some_and(|cursor| cursor.is_empty() || cursor.len() > 2048)
    {
        return Err(invalid("invalid page size or cursor"));
    }
    Ok(usize::from(limit))
}

fn invalid(message: &str) -> ControlError {
    ControlError::new(
        ErrorCode::InvalidInput,
        RetryDisposition::NotRetryable,
        message,
    )
}

fn stale(message: &str) -> ControlError {
    ControlError::new(
        ErrorCode::StaleContext,
        RetryDisposition::NewPlanRequired,
        message,
    )
}

fn unavailable(message: &str) -> ControlError {
    ControlError::new(
        ErrorCode::Unavailable,
        RetryDisposition::SafeReadRetry,
        message,
    )
}

fn map_query_error(error: QueryError) -> ControlError {
    let (code, action, message) = match error.code {
        QueryErrorCode::AuthRequired | QueryErrorCode::AuthExpired => (
            ErrorCode::AuthRequired,
            RetryDisposition::ReauthRequired,
            "sign in to YouTube Music to use this operation",
        ),
        QueryErrorCode::AccountMismatch | QueryErrorCode::StaleContext => (
            ErrorCode::StaleContext,
            RetryDisposition::NewPlanRequired,
            "the account or playback context changed",
        ),
        QueryErrorCode::RateLimited => (
            ErrorCode::RateLimited,
            RetryDisposition::SafeReadRetry,
            "YouTube Music asked this client to wait",
        ),
        QueryErrorCode::Busy => (
            ErrorCode::Busy,
            RetryDisposition::SafeReadRetry,
            "the player is busy",
        ),
        QueryErrorCode::InvalidPayload => (
            ErrorCode::InvalidInput,
            RetryDisposition::NotRetryable,
            "the catalog could not accept this query or cursor",
        ),
        QueryErrorCode::UnsupportedCapability => (
            ErrorCode::CapabilityUnavailable,
            RetryDisposition::NotRetryable,
            "this feature is not available",
        ),
        QueryErrorCode::StorageError => (
            ErrorCode::StorageError,
            RetryDisposition::SafeReadRetry,
            "the local music cache is unavailable",
        ),
        _ => (
            ErrorCode::Unavailable,
            RetryDisposition::SafeReadRetry,
            "the music provider is temporarily unavailable",
        ),
    };
    let mut result = ControlError::new(code, action, message);
    result.retry_after_ms = error.retry_after_ms;
    result
}
