use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::sync::{Arc, Mutex};
use ytm_control_api::{
    AccountHandoffState, ControlError, ErrorCode, InterruptResumeState, ItemPhase, OperationPhase,
    OperationReceipt, OperationResource, RetryDisposition, WriteInput,
};
use ytm_core::BoxFuture;
use ytm_core::control::journal::{
    AccountOperation, Claim, JournalEntry, OperationKey, OperationStore,
};
use ytm_protocol::AccountScope;

use crate::{SqliteStorage, lock_conn};

pub(crate) fn error_storage() -> ControlError {
    ControlError::new(
        ErrorCode::StorageError,
        RetryDisposition::QueryOperation,
        "could not read or persist local music control state",
    )
}

fn error_identity() -> ControlError {
    ControlError::new(
        ErrorCode::InvalidInput,
        RetryDisposition::NotRetryable,
        "journal request or receipt has a different operation identity",
    )
}

fn error_revision() -> ControlError {
    ControlError::new(
        ErrorCode::Conflict,
        RetryDisposition::QueryOperation,
        "operation journal revision changed",
    )
}

fn key_fields(key: &OperationKey) -> (&str, &str, &str) {
    // The validated account ID cannot be empty. An empty scope denotes signed-out actions.
    (
        &key.principal,
        key.account_scope
            .as_ref()
            .map_or("", |scope| scope.as_str()),
        &key.operation_id,
    )
}

fn receipt_matches(key: &OperationKey, receipt: &OperationReceipt) -> bool {
    receipt.operation_id == key.operation_id && receipt.account_scope == key.account_scope
}

fn sqlite_revision(revision: u64) -> Result<i64, ControlError> {
    i64::try_from(revision).map_err(|_| error_revision())
}

pub(crate) fn encode<T: Serialize>(payload: &T) -> Result<String, ControlError> {
    serde_json::to_string(payload).map_err(|_| error_storage())
}

pub(crate) fn decode<T: DeserializeOwned>(encoded: &str) -> Result<T, ControlError> {
    serde_json::from_str(encoded).map_err(|_| error_storage())
}

/// Decodes a stored receipt. `None` marks a receipt of a retired DJ plan or session
/// operation, which lists and recovery leave out; any other undecodable receipt is an error.
fn decode_receipt(encoded: &str) -> Result<Option<OperationReceipt>, ControlError> {
    let value: serde_json::Value = serde_json::from_str(encoded).map_err(|_| error_storage())?;
    let retired = matches!(
        value
            .pointer("/resource/kind")
            .and_then(serde_json::Value::as_str),
        Some("djPlan" | "djSession")
    );
    if retired {
        return Ok(None);
    }
    serde_json::from_value(value)
        .map(Some)
        .map_err(|_| error_storage())
}

pub(crate) fn journal_task<T, F>(
    journal: Arc<Mutex<Connection>>,
    task: F,
) -> BoxFuture<'static, Result<T, ControlError>>
where
    F: FnOnce(&mut Connection) -> Result<T, ControlError> + Send + 'static,
    T: Send + 'static,
{
    Box::pin(async move {
        tokio::task::spawn_blocking(move || {
            let mut conn = lock_conn(&journal).map_err(|_| error_storage())?;
            task(&mut conn)
        })
        .await
        .map_err(|_| error_storage())?
    })
}

impl SqliteStorage {
    fn get_journal_record<T: DeserializeOwned + Send + 'static>(
        &self,
        sql: &'static str,
        key: OperationKey,
    ) -> BoxFuture<'_, Result<Option<T>, ControlError>> {
        journal_task(Arc::clone(&self.journal), move |conn| {
            let (principal, scope, id) = key_fields(&key);
            let record: Option<String> = conn
                .query_row(sql, params![principal, scope, id], |row| row.get(0))
                .optional()
                .map_err(|_| error_storage())?;
            record.as_deref().map(decode).transpose()
        })
    }
}

impl OperationStore for SqliteStorage {
    fn recent(
        &self,
        scope: Option<AccountScope>,
        limit: usize,
    ) -> BoxFuture<'_, Result<Vec<JournalEntry>, ControlError>> {
        journal_task(Arc::clone(&self.journal), move |conn| {
            let scope = scope.as_ref().map_or("", |scope| scope.as_str());
            let mut query = conn
                .prepare(
                    "SELECT principal, receipt_json FROM control_operations
                 WHERE account_scope = ?1 ORDER BY updated_at_ms DESC, rowid DESC LIMIT ?2",
                )
                .map_err(|_| error_storage())?;
            let rows = query
                .query_map(params![scope, limit as i64], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|_| error_storage())?;
            rows.filter_map(|row| {
                let (principal, json) = match row {
                    Ok(row) => row,
                    Err(_) => return Some(Err(error_storage())),
                };
                decode_receipt(&json)
                    .transpose()
                    .map(|receipt| receipt.map(|receipt| JournalEntry { principal, receipt }))
            })
            .collect()
        })
    }

    fn account_operations(
        &self,
        scope: Option<AccountScope>,
        since_ms: u64,
    ) -> BoxFuture<'_, Result<Vec<AccountOperation>, ControlError>> {
        journal_task(Arc::clone(&self.journal), move |conn| {
            let scope = scope.as_ref().map_or("", |scope| scope.as_str());
            let since = i64::try_from(since_ms).unwrap_or(i64::MAX);
            let mut query = conn
                .prepare(
                    "SELECT claimed_at_ms, request_json, receipt_json FROM control_operations
                 WHERE account_scope = ?1 AND claimed_at_ms > ?2",
                )
                .map_err(|_| error_storage())?;
            let rows = query
                .query_map(params![scope, since], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })
                .map_err(|_| error_storage())?;
            let mut operations = Vec::new();
            for row in rows {
                let (claimed_at_ms, request, receipt) = row.map_err(|_| error_storage())?;
                // Retired action kinds no longer decode; they cannot change the account.
                let (Ok(input), Ok(Some(receipt))) =
                    (decode::<WriteInput>(&request), decode_receipt(&receipt))
                else {
                    continue;
                };
                operations.push(AccountOperation {
                    claimed_at_ms: u64::try_from(claimed_at_ms).unwrap_or(0),
                    input,
                    receipt,
                });
            }
            Ok(operations)
        })
    }

    fn unsettled(
        &self,
        scope: Option<AccountScope>,
        limit: usize,
    ) -> BoxFuture<'_, Result<Vec<JournalEntry>, ControlError>> {
        journal_task(Arc::clone(&self.journal), move |conn| {
            let scope = scope.as_ref().map_or("", |scope| scope.as_str());
            let mut query = conn
                .prepare(
                    "SELECT principal, receipt_json FROM control_operations
                 WHERE settled = 0 AND account_scope = ?1 ORDER BY rowid DESC LIMIT ?2",
                )
                .map_err(|_| error_storage())?;
            let rows = query
                .query_map(params![scope, limit as i64], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|_| error_storage())?;
            rows.filter_map(|row| {
                let (principal, json) = match row {
                    Ok(row) => row,
                    Err(_) => return Some(Err(error_storage())),
                };
                decode_receipt(&json)
                    .transpose()
                    .map(|receipt| receipt.map(|receipt| JournalEntry { principal, receipt }))
            })
            .collect()
        })
    }

    fn claim(
        &self,
        key: OperationKey,
        input: WriteInput,
        payload_hash: [u8; 32],
        initial: OperationReceipt,
        now_ms: u64,
    ) -> BoxFuture<'_, Result<Claim, ControlError>> {
        journal_task(Arc::clone(&self.journal), move |conn| {
            if !receipt_matches(&key, &initial) || key.operation_id != input.operation_id {
                return Err(error_identity());
            }
            let now_ms = i64::try_from(now_ms).unwrap_or(i64::MAX);
            let (principal, scope, id) = key_fields(&key);
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|_| error_storage())?;
            let existing: Option<(Vec<u8>, String)> = tx.query_row(
                "SELECT payload_hash, receipt_json FROM control_operations WHERE principal = ?1 AND account_scope = ?2 AND operation_id = ?3",
                params![principal, scope, id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional().map_err(|_| error_storage())?;
            if let Some((hash, receipt)) = existing {
                return if hash == payload_hash {
                    Ok(Claim::Existing(decode(&receipt)?))
                } else {
                    Ok(Claim::Conflict)
                };
            }
            tx.execute(
                "INSERT INTO control_operations (principal, account_scope, operation_id, payload_hash, revision, settled, updated_at_ms, request_json, receipt_json, claimed_at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?7)",
                params![principal, scope, id, payload_hash.as_slice(), sqlite_revision(initial.revision)?, initial.settled, now_ms, encode(&input)?, encode(&initial)?],
            ).map_err(|_| error_storage())?;
            tx.commit().map_err(|_| error_storage())?;
            Ok(Claim::New(initial))
        })
    }

    fn get(
        &self,
        key: OperationKey,
    ) -> BoxFuture<'_, Result<Option<OperationReceipt>, ControlError>> {
        self.get_journal_record(
            "SELECT receipt_json FROM control_operations WHERE principal = ?1 AND account_scope = ?2 AND operation_id = ?3",
            key,
        )
    }

    fn load_request(
        &self,
        key: OperationKey,
    ) -> BoxFuture<'_, Result<Option<WriteInput>, ControlError>> {
        self.get_journal_record(
            "SELECT request_json FROM control_operations WHERE principal = ?1 AND account_scope = ?2 AND operation_id = ?3",
            key,
        )
    }

    fn update(
        &self,
        key: OperationKey,
        expected_revision: u64,
        next: OperationReceipt,
        now_ms: u64,
    ) -> BoxFuture<'_, Result<(), ControlError>> {
        journal_task(Arc::clone(&self.journal), move |conn| {
            if !receipt_matches(&key, &next) {
                return Err(error_identity());
            }
            if expected_revision.checked_add(1) != Some(next.revision) {
                return Err(error_revision());
            }
            let now_ms = i64::try_from(now_ms).unwrap_or(i64::MAX);
            let (principal, scope, id) = key_fields(&key);
            let changes = conn.execute(
                "UPDATE control_operations SET revision = ?5, settled = ?6, updated_at_ms = ?7, receipt_json = ?8
                 WHERE principal = ?1 AND account_scope = ?2 AND operation_id = ?3 AND revision = ?4",
                params![principal, scope, id, sqlite_revision(expected_revision)?, sqlite_revision(next.revision)?, next.settled, now_ms, encode(&next)?],
            ).map_err(|_| error_storage())?;
            if changes == 0 {
                return Err(error_revision());
            }
            Ok(())
        })
    }

    fn recover(&self, now_ms: u64) -> BoxFuture<'_, Result<(), ControlError>> {
        journal_task(Arc::clone(&self.journal), move |conn| {
            let now_ms = i64::try_from(now_ms).unwrap_or(i64::MAX);
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|_| error_storage())?;
            let mut through = 0;
            loop {
                let page: Vec<(i64, String)> = {
                    let mut stmt = tx.prepare(
                        "SELECT rowid, receipt_json FROM control_operations WHERE settled = 0 AND rowid > ?1 ORDER BY rowid LIMIT 128",
                    ).map_err(|_| error_storage())?;
                    stmt.query_map([through], |row| Ok((row.get(0)?, row.get(1)?)))
                        .map_err(|_| error_storage())?
                        .collect::<Result<_, _>>()
                        .map_err(|_| error_storage())?
                };
                if page.is_empty() {
                    break;
                }
                for (rowid, encoded) in page {
                    through = rowid;
                    let Some(mut receipt) = decode_receipt(&encoded)? else {
                        continue;
                    };
                    let old = receipt.clone();
                    recover_receipt(&mut receipt);
                    if receipt == old {
                        continue;
                    }
                    receipt.revision =
                        receipt.revision.checked_add(1).ok_or_else(error_revision)?;
                    tx.execute(
                        "UPDATE control_operations SET revision = ?2, settled = ?3, updated_at_ms = ?4, receipt_json = ?5 WHERE rowid = ?1",
                        params![rowid, sqlite_revision(receipt.revision)?, receipt.settled, now_ms, encode(&receipt)?],
                    ).map_err(|_| error_storage())?;
                }
            }
            tx.commit().map_err(|_| error_storage())
        })
    }
}

fn recover_receipt(receipt: &mut OperationReceipt) {
    receipt.scheduling_stopped = true;
    // A pending resume plan lives only in the running worker. Once the switch was observed
    // (acknowledged), restart ends the plan as a settled partial result instead of an unknown.
    if let Some(OperationResource::Interruption { resume, .. }) = receipt.resource.as_mut()
        && *resume == InterruptResumeState::Pending
    {
        *resume = InterruptResumeState::Abandoned;
        if receipt.phase == OperationPhase::Acknowledged
            && receipt
                .item_results
                .iter()
                .all(|item| item.phase == ItemPhase::Verified)
        {
            receipt.phase = OperationPhase::Partial;
            receipt.settled = true;
            receipt.retry_disposition = RetryDisposition::NewPlanRequired;
            receipt.error = Some(ControlError::new(
                ErrorCode::StaleContext,
                RetryDisposition::NewPlanRequired,
                "the player restarted before the interrupted track resumed",
            ));
            return;
        }
    }
    // A handoff waiting for the user never sends anything; the wait lived only in the worker,
    // so restart ends it cleanly and a new handoff can be requested. Earlier versions left such
    // a wait unknown; those are settled the same way.
    if matches!(
        receipt.resource,
        Some(OperationResource::AccountHandoff {
            state: AccountHandoffState::AwaitingUser,
            ..
        })
    ) && matches!(
        receipt.phase,
        OperationPhase::Acknowledged | OperationPhase::Unknown
    ) {
        receipt.phase = OperationPhase::Rejected;
        receipt.settled = true;
        receipt.retry_disposition = RetryDisposition::NewPlanRequired;
        receipt.error = Some(ControlError::new(
            ErrorCode::Unavailable,
            RetryDisposition::NewPlanRequired,
            "the player restarted while the account handoff waited for the user; nothing was sent",
        ));
        return;
    }
    for item in &mut receipt.item_results {
        if matches!(item.phase, ItemPhase::Submitted | ItemPhase::Acknowledged) {
            item.phase = ItemPhase::Unknown;
            item.error = Some(ErrorCode::ResultUnknown);
        }
    }
    let unknown = matches!(
        receipt.phase,
        OperationPhase::Submitted | OperationPhase::Acknowledged | OperationPhase::Unknown
    ) || receipt
        .item_results
        .iter()
        .any(|item| item.phase == ItemPhase::Unknown);
    if unknown {
        receipt.phase = if receipt
            .item_results
            .iter()
            .any(|item| item.phase == ItemPhase::Verified)
        {
            OperationPhase::Partial
        } else {
            OperationPhase::Unknown
        };
        receipt.retry_disposition = RetryDisposition::ReconcileFirst;
        receipt.settled = false;
        receipt.error.get_or_insert_with(|| {
            ControlError::new(
                ErrorCode::ResultUnknown,
                RetryDisposition::ReconcileFirst,
                "operation was interrupted after it may have been sent",
            )
        });
    }
}
