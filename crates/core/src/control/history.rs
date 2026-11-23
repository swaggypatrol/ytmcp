use sha2::{Digest, Sha256};
use ytm_control_api::{
    CombinedHistory, ControlError, ControlRequest, ControlResponse, ErrorCode, GrantAction,
    HistoryEntry, HistoryInput, HistorySource, ItemPhase, ItemResult, LocalHistoryEdit,
    LocalHistoryEntry, LocalHistoryRange, LocalHistoryView, OperationPhase, OperationResource,
    Page, PageInput, RemovedItem, RetryDisposition, VerificationLevel, WriteAction,
};
use ytm_protocol::{AccountEpoch, AccountScope, Freshness, PlaybackRef, Track};

use super::write::{MusicOperation, Transition};
use super::{
    ControlRead, ControlService, ReadGrant, invalid, map_query_error, page_size, stale, unavailable,
};
use crate::BoxFuture;

pub struct LocalHistoryRecord {
    pub entry_id: String,
    pub track: Track,
    pub playback_ref: Option<PlaybackRef>,
    pub position_ms: u64,
    pub duration_ms: Option<u64>,
    pub recorded_at_ms: u64,
}

pub struct LocalHistorySnapshot {
    pub revision: String,
    pub retention_limit: u32,
    pub records: Vec<LocalHistoryRecord>,
}

pub trait LocalHistoryStore: Send + Sync {
    fn snapshot(
        &self,
        scope: AccountScope,
    ) -> BoxFuture<'_, Result<LocalHistorySnapshot, ControlError>>;

    /// The revision check and edit share one transaction. Returns the observed removals.
    fn edit(
        &self,
        scope: AccountScope,
        epoch: AccountEpoch,
        expected_revision: String,
        action: LocalHistoryEdit,
    ) -> BoxFuture<'_, Result<Vec<String>, ControlError>>;
}

impl ControlService {
    pub(super) async fn read_history(
        &self,
        grant: &ReadGrant,
        state: &ControlRead,
        input: HistoryInput,
    ) -> Result<ControlResponse, ControlError> {
        let page = input.page.clone().unwrap_or(PageInput {
            cursor: None,
            limit: input.limit,
        });
        if input.range.is_some_and(|range| !range.is_ordered()) {
            return Err(invalid(
                "the local history range must have fromMs before untilMs",
            ));
        }
        if input.range.is_some() && input.source != Some(HistorySource::Local) {
            return Err(invalid("history ranges require an explicit local source"));
        }
        if input.source != Some(HistorySource::Combined)
            && (input.local_cursor.is_some() || input.youtube_cursor.is_some())
        {
            return Err(invalid("source-specific cursors require combined history"));
        }
        let response = match input.source {
            Some(HistorySource::Local) => ControlResponse::LocalHistory(Box::new(
                self.read_local_history(grant, page, input.range).await?,
            )),
            Some(HistorySource::Youtube) => {
                ControlResponse::HistoryEntries(self.read_youtube_history(state, page).await?)
            }
            Some(HistorySource::Combined) => {
                if page.cursor.is_some() {
                    return Err(invalid(
                        "use localCursor and youtubeCursor for combined history",
                    ));
                }
                let local_page = PageInput {
                    cursor: input.local_cursor,
                    limit: page.limit,
                };
                let youtube_page = PageInput {
                    cursor: input.youtube_cursor,
                    limit: page.limit,
                };
                page_size(Some(&local_page))?;
                page_size(Some(&youtube_page))?;
                let (local, youtube) = tokio::join!(
                    self.read_local_history(grant, local_page, None),
                    self.read_youtube_history(state, youtube_page),
                );
                let (youtube, youtube_error) = match youtube {
                    Ok(page) => (Some(page), None),
                    Err(error) => (None, Some(error)),
                };
                ControlResponse::CombinedHistory(Box::new(CombinedHistory {
                    local: local?,
                    youtube,
                    youtube_error,
                }))
            }
            None => {
                return self
                    .read_catalog_view(grant, state, ControlRequest::GetHistory(input))
                    .await;
            }
        };
        self.check_read_context(grant, state).await?;
        Ok(response)
    }

    async fn read_youtube_history(
        &self,
        state: &ControlRead,
        page: PageInput,
    ) -> Result<Page<HistoryEntry>, ControlError> {
        page_size(Some(&page))?;
        let (catalog, context) = self.catalog_context(state)?;
        catalog
            .youtube_history(context, page)
            .await
            .map_err(map_query_error)
    }

    async fn read_local_history(
        &self,
        grant: &ReadGrant,
        page: PageInput,
        range: Option<LocalHistoryRange>,
    ) -> Result<LocalHistoryView, ControlError> {
        let scope = grant.scope.clone().ok_or_else(no_history_account)?;
        let limit = page_size(Some(&page))?;
        let history = self.history_store()?.snapshot(scope).await?;
        let mut digest = Sha256::new();
        digest.update(history.revision.as_bytes());
        digest.update(
            range
                .and_then(|range| range.from_ms)
                .unwrap_or(0)
                .to_be_bytes(),
        );
        digest.update(
            range
                .and_then(|range| range.until_ms)
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
        let bookmark = hex::encode(digest.finalize());
        let records: Vec<_> = history
            .records
            .into_iter()
            .filter(|record| range.is_none_or(|range| range.contains(record.recorded_at_ms)))
            .collect();
        let start = match page.cursor {
            None => 0,
            Some(cursor) => cursor
                .strip_prefix(&bookmark)
                .and_then(|part| part.strip_prefix(':'))
                .and_then(|part| part.parse::<usize>().ok())
                .filter(|offset| *offset > 0 && *offset < records.len())
                .ok_or_else(|| {
                    stale("the local history cursor changed; restart at the first page")
                })?,
        };
        let next = start + limit;
        let next_cursor = (next < records.len()).then(|| format!("{bookmark}:{next}"));
        let items = records
            .into_iter()
            .skip(start)
            .take(limit)
            .map(|record| LocalHistoryEntry {
                entry_id: record.entry_id,
                track: super::track_projection(&record.track),
                playback_ref: record.playback_ref,
                position_ms: record.position_ms,
                duration_ms: record.duration_ms,
                recorded_at_ms: record.recorded_at_ms,
            })
            .collect();
        Ok(LocalHistoryView {
            revision: history.revision,
            retention_limit: history.retention_limit,
            entries: Page {
                items,
                complete: next_cursor.is_none(),
                next_cursor,
                source: "local_playback_history".into(),
                freshness: Freshness::Fresh,
                captured_at_ms: self.core.clock().wall_now_ms(),
            },
        })
    }

    /// The local history entries an edit would remove, read before anything is claimed so the
    /// receipt carries them as its removal ledger and the daily edit limit can count them.
    pub(super) async fn planned_history_removal(
        &self,
        grant: &ReadGrant,
        action: &WriteAction,
    ) -> Result<Vec<RemovedItem>, ControlError> {
        let WriteAction::EditLocalHistory { edit } = action else {
            return Ok(Vec::new());
        };
        // Without an account the worker refuses the edit before anything is changed.
        let Some(scope) = grant.scope.clone() else {
            return Ok(Vec::new());
        };
        let history = self.history_store()?.snapshot(scope).await?;
        Ok(history
            .records
            .into_iter()
            .filter(|record| match edit {
                LocalHistoryEdit::Remove { entry_ids } => entry_ids.contains(&record.entry_id),
                LocalHistoryEdit::Clear { range } => {
                    range.is_none_or(|range| range.contains(record.recorded_at_ms))
                }
            })
            .map(|record| RemovedItem::LocalHistoryEntry {
                entry_id: record.entry_id,
                video_id: record.track.video_id,
                recorded_at_ms: record.recorded_at_ms,
            })
            .collect())
    }

    fn history_store(&self) -> Result<&dyn LocalHistoryStore, ControlError> {
        self.history
            .as_deref()
            .ok_or_else(|| unavailable("local playback history is unavailable"))
    }
}

impl MusicOperation {
    pub(super) async fn edit_local_history(&mut self) -> Result<Transition, ControlError> {
        let WriteAction::EditLocalHistory { edit: action } = &self.input.action else {
            return Err(invalid("expected a local history edit"));
        };
        let action = action.clone();
        let epoch = self
            .state
            .snapshot
            .account_epoch
            .clone()
            .ok_or_else(no_history_account)?;
        let scope = self.grant.scope.clone().ok_or_else(no_history_account)?;
        if self.input.preconditions.expected_account_epoch.as_ref() != Some(&epoch) {
            return Err(stale(
                "the local history edit requires the current account epoch",
            ));
        }
        let revision = self
            .input
            .preconditions
            .expected_entity_revision
            .clone()
            .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .ok_or_else(|| {
                invalid("the local history edit requires the current history revision")
            })?;
        match &action {
            LocalHistoryEdit::Remove { entry_ids }
                if entry_ids.is_empty() || entry_ids.len() > 50 =>
            {
                return Err(invalid(
                    "remove requires between 1 and 50 local history entries",
                ));
            }
            LocalHistoryEdit::Clear { range } if range.is_some_and(|range| !range.is_ordered()) => {
                return Err(invalid(
                    "the local history range must have fromMs before untilMs",
                ));
            }
            _ => {}
        }
        self.service.history_store()?;
        let mut submitted = self.current.clone();
        submitted.revision += 1;
        submitted.phase = OperationPhase::Submitted;
        submitted.resource = Some(OperationResource::LocalHistory {
            removed_count: None,
        });
        self.apply(submitted).await?;
        if let Err(error) = self
            .service
            .check_read_context(&self.grant, &self.state)
            .await
        {
            return Ok(Transition::Rejected(error));
        }
        if let Err(error) = self.authority.authorize(GrantAction::LocalHistory).await {
            return Ok(Transition::Rejected(error));
        }
        if self.cancel.is_cancelled() {
            return Ok(Transition::Cancelled);
        }
        let report_items = matches!(action, LocalHistoryEdit::Remove { .. });
        let removed = match self
            .service
            .history_store()?
            .edit(scope, epoch.clone(), revision, action)
            .await
        {
            Ok(removed) => removed,
            Err(error)
                if matches!(
                    error.code,
                    ErrorCode::StaleContext | ErrorCode::InvalidInput
                ) =>
            {
                return Ok(Transition::Rejected(error));
            }
            Err(error) => return Err(error),
        };
        self.current.resource = Some(OperationResource::LocalHistory {
            removed_count: Some(removed.len() as u32),
        });
        self.current.removed.retain(|item| {
            matches!(item, RemovedItem::LocalHistoryEntry { entry_id, .. } if removed.contains(entry_id))
        });
        if report_items {
            self.current.item_results = removed
                .into_iter()
                .enumerate()
                .map(|(index, entry_id)| ItemResult {
                    input_index: index as u32,
                    phase: ItemPhase::Verified,
                    resource_entry_id: Some(entry_id),
                    error: None,
                })
                .collect();
        }
        let _ = self
            .service
            .core
            .invalidate_control_resource(epoch, "localHistory".into(), "local".into())
            .await;
        Ok(Transition::Verified(VerificationLevel::Persisted))
    }
}

fn no_history_account() -> ControlError {
    ControlError::new(
        ErrorCode::AuthRequired,
        RetryDisposition::ReauthRequired,
        "local playback history requires a matching account",
    )
}
