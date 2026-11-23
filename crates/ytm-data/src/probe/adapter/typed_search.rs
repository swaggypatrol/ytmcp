use ytm_protocol::{AlbumId, PlaybackRef};

use super::*;
use crate::probe::backend::{BackendPage, ProviderTypedSearchItem, TypedSearchKind};

const CURSOR_TTL_MS: u64 = 120_000;
const MAX_WINDOWS: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderTypedSearchPage {
    pub items: Vec<ProviderTypedSearchItem>,
    pub next_cursor: Option<String>,
    pub complete: bool,
    pub captured_at_ms: u64,
}

#[derive(PartialEq, Eq)]
struct SearchBinding {
    scope: AccountScope,
    epoch: AccountEpoch,
    kind: TypedSearchKind,
    query: [u8; 32],
}

pub(super) struct TypedSearchWindow {
    cursor: String,
    binding: SearchBinding,
    progress: SearchProgress,
    expires_at_ms: u64,
}

#[derive(Clone, Default)]
struct SearchProgress {
    pending: VecDeque<ProviderTypedSearchItem>,
    continuation: Option<String>,
    seen_ids: HashSet<String>,
    seen_tokens: HashSet<String>,
    incomplete: bool,
    captured_at_ms: u64,
}

impl SearchProgress {
    fn accept(&mut self, page: BackendPage<ProviderTypedSearchItem>, captured_at_ms: u64) {
        self.pending.extend(
            page.items
                .into_iter()
                .filter(|item| self.seen_ids.insert(item.id().to_owned())),
        );
        self.continuation = page.next_continuation.filter(|token| {
            let unseen = !token.is_empty() && self.seen_tokens.insert(token.clone());
            self.incomplete |= !unseen;
            unseen
        });
        self.captured_at_ms = captured_at_ms;
    }
}

impl YtmCatalogAdapter {
    pub async fn typed_search(
        &self,
        epoch: &AccountEpoch,
        scope: &AccountScope,
        kind: TypedSearchKind,
        query: &str,
        page: MusicPageRequest<'_>,
        cancel: &CancellationToken,
    ) -> Result<ProviderTypedSearchPage, QueryError> {
        let active = self.fresh_session(epoch, scope, page.limit)?;
        let query = query.trim();
        if query.is_empty() || query.chars().count() > 256 || query.chars().any(char::is_control) {
            return Err(make_query_error(
                QueryErrorCode::InvalidPayload,
                false,
                None,
                "expected a nonempty search query of at most 256 characters",
            ));
        }
        let binding = SearchBinding {
            scope: scope.clone(),
            epoch: epoch.clone(),
            kind,
            query: Sha256::digest(query.as_bytes()).into(),
        };
        let (mut progress, expires_at_ms) = if let Some(cursor) = page.cursor {
            let state = self.state.read().map_err(|_| stale_provider_cursor())?;
            let window = state
                .typed_search_windows
                .iter()
                .find(|window| {
                    window.cursor == cursor
                        && window.binding == binding
                        && window.expires_at_ms > self.clock.now_ms()
                })
                .ok_or_else(stale_provider_cursor)?;
            (window.progress.clone(), window.expires_at_ms)
        } else {
            (
                SearchProgress::default(),
                self.clock.now_ms().saturating_add(CURSOR_TTL_MS),
            )
        };
        if progress.pending.is_empty() {
            let provider = self
                .run_backend_call(
                    &active,
                    cancel,
                    self.backend.typed_search(
                        &active.session,
                        kind,
                        query,
                        progress.continuation.as_deref(),
                    ),
                )
                .await?;
            progress.accept(provider, self.clock.wall_now_ms());
        }

        let end = progress.pending.len().min(page.limit as usize);
        let items = progress.pending.drain(..end).collect();
        let more = !progress.pending.is_empty() || progress.continuation.is_some();
        let complete = !more && !progress.incomplete;
        let captured_at_ms = progress.captured_at_ms;

        let mut state = self.state.write().map_err(|_| stale_provider_cursor())?;
        let now_ms = self.clock.now_ms();
        if cancel.is_cancelled()
            || active.session_cancel.is_cancelled()
            || (more && expires_at_ms <= now_ms)
            || !state.active.as_ref().is_some_and(|current| {
                current.account_epoch == *epoch && current.account_scope == *scope
            })
        {
            return Err(stale_provider_cursor());
        }
        let next_cursor = more.then(|| {
            let cursor = format!("ytm-search:{}", hex::encode(rand::random::<[u8; 16]>()));
            let windows = &mut state.typed_search_windows;
            windows.retain(|window| window.expires_at_ms > now_ms);
            if windows.len() == MAX_WINDOWS {
                windows.pop_front();
            }
            windows.push_back(TypedSearchWindow {
                cursor: cursor.clone(),
                binding,
                progress,
                expires_at_ms,
            });
            cursor
        });
        Ok(ProviderTypedSearchPage {
            items,
            next_cursor,
            complete,
            captured_at_ms,
        })
    }

    pub async fn album_playback_target(
        &self,
        epoch: &AccountEpoch,
        scope: &AccountScope,
        album: &AlbumId,
        index: u32,
        cancel: &CancellationToken,
    ) -> Result<PlaybackRef, QueryError> {
        let active = self.online_session(epoch, scope)?;
        self.run_backend_call(
            &active,
            cancel,
            self.backend
                .album_playback_target(&active.session, album, index),
        )
        .await
    }
}
