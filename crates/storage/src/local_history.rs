use std::collections::HashSet;

use rusqlite::{TransactionBehavior, params};
use sha2::{Digest, Sha256};
use ytm_control_api::{ControlError, ErrorCode, LocalHistoryEdit, RetryDisposition};
use ytm_core::BoxFuture;
use ytm_core::control::history::{LocalHistoryRecord, LocalHistorySnapshot, LocalHistoryStore};
use ytm_protocol::{AccountEpoch, AccountScope};

use crate::SqliteStorage;
use crate::operation_store::{error_storage, journal_task};
use crate::repo::{
    MAX_RECENT_PLAYBACK_PER_ACCOUNT, RecentPlaybackRecord, list_recent_playback_with_ids,
};

const RETENTION_LIMIT: usize = MAX_RECENT_PLAYBACK_PER_ACCOUNT as usize;

impl LocalHistoryStore for SqliteStorage {
    fn snapshot(
        &self,
        scope: AccountScope,
    ) -> BoxFuture<'_, Result<LocalHistorySnapshot, ControlError>> {
        journal_task(self.journal.clone(), move |conn| {
            let records = list_recent_playback_with_ids(conn, &scope, RETENTION_LIMIT)
                .map_err(|_| error_storage())?;
            Ok(LocalHistorySnapshot {
                revision: revision(&scope, &records),
                retention_limit: RETENTION_LIMIT as u32,
                records: records
                    .into_iter()
                    .map(|(id, record)| LocalHistoryRecord {
                        entry_id: entry_id(&scope, id),
                        track: record.track,
                        playback_ref: record.playback_ref,
                        position_ms: record.position_ms,
                        duration_ms: record.duration_ms,
                        recorded_at_ms: record.recorded_at_ms,
                    })
                    .collect(),
            })
        })
    }

    fn edit(
        &self,
        scope: AccountScope,
        epoch: AccountEpoch,
        expected_revision: String,
        action: LocalHistoryEdit,
    ) -> BoxFuture<'_, Result<Vec<String>, ControlError>> {
        journal_task(self.journal.clone(), move |conn| {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|_| error_storage())?;
            let authorized: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM account_epochs WHERE account_epoch = ?1 AND account_scope = ?2 AND state = 'active')",
                params![epoch.as_str(), scope.as_str()], |row| row.get(0),
            ).map_err(|_| error_storage())?;
            if !authorized {
                return Err(stale("the local history account epoch is no longer active"));
            }
            let records = list_recent_playback_with_ids(&tx, &scope, RETENTION_LIMIT)
                .map_err(|_| error_storage())?;
            if revision(&scope, &records) != expected_revision {
                return Err(stale(
                    "the local history changed; read it again before editing",
                ));
            }
            let selected = match action {
                LocalHistoryEdit::Remove { entry_ids } => {
                    let unique: HashSet<_> = entry_ids.iter().collect();
                    if unique.len() != entry_ids.len() {
                        return Err(ControlError::new(
                            ErrorCode::InvalidInput,
                            RetryDisposition::NotRetryable,
                            "local history entry IDs must be distinct",
                        ));
                    }
                    let available: std::collections::HashMap<_, _> = records
                        .iter()
                        .map(|(id, _)| (entry_id(&scope, *id), *id))
                        .collect();
                    entry_ids
                        .into_iter()
                        .map(|key| {
                            let id = *available.get(&key).ok_or_else(|| {
                                stale("a local history occurrence is no longer available")
                            })?;
                            Ok((id, key))
                        })
                        .collect::<Result<Vec<_>, ControlError>>()?
                }
                LocalHistoryEdit::Clear { range } => records
                    .into_iter()
                    .filter(|(_, record)| {
                        range.is_none_or(|range| range.contains(record.recorded_at_ms))
                    })
                    .map(|(id, _)| (id, entry_id(&scope, id)))
                    .collect(),
            };
            {
                let mut delete = tx
                    .prepare("DELETE FROM recent_playback WHERE account_scope = ?1 AND id = ?2")
                    .map_err(|_| error_storage())?;
                for (id, _) in &selected {
                    let removed = delete
                        .execute(params![scope.as_str(), id])
                        .map_err(|_| error_storage())?;
                    if removed != 1 {
                        return Err(error_storage());
                    }
                }
            }
            if !selected.is_empty() {
                tx.execute(
                    "DELETE FROM catalog_cache WHERE account_scope = ?1 AND entry_kind = 'home_feed'",
                    [scope.as_str()],
                )
                .map_err(|_| error_storage())?;
            }
            tx.commit().map_err(|_| error_storage())?;
            Ok(selected.into_iter().map(|(_, key)| key).collect())
        })
    }
}

fn revision(scope: &AccountScope, records: &[(i64, RecentPlaybackRecord)]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"local history revision\0");
    digest.update(scope.as_str().as_bytes());
    digest.update([0]);
    for (id, _) in records {
        digest.update(id.to_be_bytes());
    }
    hex::encode(digest.finalize())
}

fn entry_id(scope: &AccountScope, id: i64) -> String {
    let mut digest = Sha256::new();
    digest.update(b"local history entry\0");
    digest.update(scope.as_str().as_bytes());
    digest.update([0]);
    digest.update(id.to_be_bytes());
    hex::encode(digest.finalize())
}

fn stale(message: &str) -> ControlError {
    ControlError::new(
        ErrorCode::StaleContext,
        RetryDisposition::NewPlanRequired,
        message,
    )
}
