use std::sync::Arc;

use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use ytm_control_api::{
    ControlError, ErrorCode, ListeningIntentBasis, ListeningIntentInstruction,
    ListeningIntentScope, RetryDisposition,
};
use ytm_core::BoxFuture;
use ytm_core::control::moment_store::{MomentStore, StoredListeningIntent};
use ytm_protocol::AccountScope;

use crate::SqliteStorage;
use crate::operation_store::{decode, encode, error_storage, journal_task};

fn scope_kind_str(scope: ListeningIntentScope) -> &'static str {
    match scope {
        ListeningIntentScope::Today => "today",
        ListeningIntentScope::Session => "session",
        ListeningIntentScope::Until => "until",
    }
}

fn parse_scope_kind(raw: &str) -> Result<ListeningIntentScope, ControlError> {
    match raw {
        "today" => Ok(ListeningIntentScope::Today),
        "session" => Ok(ListeningIntentScope::Session),
        "until" => Ok(ListeningIntentScope::Until),
        _ => Err(error_storage()),
    }
}

fn basis_str(basis: ListeningIntentBasis) -> &'static str {
    match basis {
        ListeningIntentBasis::OwnerInput => "ownerInput",
        ListeningIntentBasis::UserRequestReportedByAssistant => "userRequestReportedByAssistant",
        ListeningIntentBasis::AssistantInference => "assistantInference",
    }
}

fn parse_basis(raw: &str) -> Result<ListeningIntentBasis, ControlError> {
    match raw {
        "ownerInput" => Ok(ListeningIntentBasis::OwnerInput),
        "userRequestReportedByAssistant" => {
            Ok(ListeningIntentBasis::UserRequestReportedByAssistant)
        }
        "assistantInference" => Ok(ListeningIntentBasis::AssistantInference),
        _ => Err(error_storage()),
    }
}

fn current_revision_tx(tx: &Transaction<'_>, scope: &str) -> Result<u64, ControlError> {
    let rev: Option<i64> = tx
        .query_row(
            "SELECT revision FROM control_moment_revisions WHERE account_scope = ?1",
            params![scope],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| error_storage())?;
    Ok(u64::try_from(rev.unwrap_or(0)).unwrap_or(0))
}

fn bump_revision_tx(tx: &Transaction<'_>, scope: &str) -> Result<u64, ControlError> {
    let next = current_revision_tx(tx, scope)?.saturating_add(1);
    let next_i64 = i64::try_from(next).unwrap_or(i64::MAX);
    tx.execute(
        "INSERT INTO control_moment_revisions (account_scope, revision) VALUES (?1, ?2)
         ON CONFLICT (account_scope) DO UPDATE SET revision = excluded.revision",
        params![scope, next_i64],
    )
    .map_err(|_| error_storage())?;
    Ok(next)
}

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredListeningIntent> {
    let intent_key: String = row.get(0)?;
    let intent_id: String = row.get(1)?;
    let instruction_json: String = row.get(2)?;
    let scope_kind: String = row.get(3)?;
    let created_at_ms: i64 = row.get(4)?;
    let expires_at_ms: i64 = row.get(5)?;
    let time_zone: Option<String> = row.get(6)?;
    let basis_raw: String = row.get(7)?;
    let reported_by: String = row.get(8)?;

    let instruction: ListeningIntentInstruction =
        decode(&instruction_json).map_err(|_| rusqlite::Error::InvalidQuery)?;
    let scope = parse_scope_kind(&scope_kind).map_err(|_| rusqlite::Error::InvalidQuery)?;
    let basis = parse_basis(&basis_raw).map_err(|_| rusqlite::Error::InvalidQuery)?;

    Ok(StoredListeningIntent {
        intent_key,
        intent_id,
        instruction,
        scope,
        created_at_ms: u64::try_from(created_at_ms).unwrap_or(0),
        expires_at_ms: u64::try_from(expires_at_ms).unwrap_or(0),
        time_zone,
        basis,
        reported_by,
    })
}

fn conflict_error(message: &str) -> ControlError {
    ControlError::new(
        ErrorCode::Conflict,
        RetryDisposition::NewPlanRequired,
        message,
    )
}

impl MomentStore for SqliteStorage {
    fn list_active_intents(
        &self,
        account_scope: AccountScope,
        now_ms: u64,
    ) -> BoxFuture<'_, Result<(Vec<StoredListeningIntent>, u64), ControlError>> {
        journal_task(Arc::clone(&self.journal), move |conn| {
            let now_i64 = i64::try_from(now_ms).unwrap_or(i64::MAX);
            let scope = account_scope.as_str();
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|_| error_storage())?;
            tx.execute(
                "DELETE FROM control_listening_intents WHERE account_scope = ?1 AND expires_at_ms <= ?2",
                params![scope, now_i64],
            )
            .map_err(|_| error_storage())?;
            let items = {
                let mut stmt = tx
                    .prepare(
                        "SELECT intent_key, intent_id, instruction_json, scope_kind, created_at_ms,
                                expires_at_ms, time_zone, basis, reported_by
                         FROM control_listening_intents
                         WHERE account_scope = ?1 AND expires_at_ms > ?2
                         ORDER BY intent_key ASC",
                    )
                    .map_err(|_| error_storage())?;
                let rows = stmt
                    .query_map(params![scope, now_i64], map_row)
                    .map_err(|_| error_storage())?;
                rows.collect::<Result<Vec<_>, _>>()
                    .map_err(|_| error_storage())?
            };
            let rev = current_revision_tx(&tx, scope)?;
            tx.commit().map_err(|_| error_storage())?;
            Ok((items, rev))
        })
    }

    fn moment_revision(
        &self,
        account_scope: AccountScope,
    ) -> BoxFuture<'_, Result<u64, ControlError>> {
        journal_task(Arc::clone(&self.journal), move |conn| {
            let rev: Option<i64> = conn
                .query_row(
                    "SELECT revision FROM control_moment_revisions WHERE account_scope = ?1",
                    params![account_scope.as_str()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|_| error_storage())?;
            Ok(u64::try_from(rev.unwrap_or(0)).unwrap_or(0))
        })
    }

    fn upsert_intent(
        &self,
        account_scope: AccountScope,
        intent: StoredListeningIntent,
        expected_revision: Option<u64>,
        max_active: usize,
        now_ms: u64,
    ) -> BoxFuture<'_, Result<(StoredListeningIntent, u64), ControlError>> {
        journal_task(Arc::clone(&self.journal), move |conn| {
            let now_i64 = i64::try_from(now_ms).unwrap_or(i64::MAX);
            let created_i64 = i64::try_from(intent.created_at_ms).unwrap_or(i64::MAX);
            let expires_i64 = i64::try_from(intent.expires_at_ms).unwrap_or(i64::MAX);
            let scope = account_scope.as_str();
            let instruction_json = encode(&intent.instruction)?;
            let scope_kind = scope_kind_str(intent.scope);
            let basis = basis_str(intent.basis);

            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|_| error_storage())?;
            tx.execute(
                "DELETE FROM control_listening_intents WHERE account_scope = ?1 AND expires_at_ms <= ?2",
                params![scope, now_i64],
            )
            .map_err(|_| error_storage())?;

            let rev = current_revision_tx(&tx, scope)?;
            if let Some(expected) = expected_revision
                && expected != rev
            {
                return Err(conflict_error(
                    "Moment DJ state changed before this intent was saved; read again first",
                ));
            }

            let existing_basis: Option<String> = tx
                .query_row(
                    "SELECT basis FROM control_listening_intents
                     WHERE account_scope = ?1 AND intent_key = ?2",
                    params![scope, intent.intent_key],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|_| error_storage())?;
            if let Some(existing_raw) = &existing_basis {
                let existing = parse_basis(existing_raw)?;
                if intent.basis == ListeningIntentBasis::AssistantInference
                    && matches!(
                        existing,
                        ListeningIntentBasis::OwnerInput
                            | ListeningIntentBasis::UserRequestReportedByAssistant
                    )
                {
                    return Err(conflict_error(
                        "assistant inference cannot overwrite an explicit listener or owner intent",
                    ));
                }
            } else {
                let count: i64 = tx
                    .query_row(
                        "SELECT COUNT(*) FROM control_listening_intents WHERE account_scope = ?1",
                        params![scope],
                        |row| row.get(0),
                    )
                    .map_err(|_| error_storage())?;
                if usize::try_from(count).unwrap_or(usize::MAX) >= max_active {
                    return Err(conflict_error(
                        "too many active listening intents for this account; clear one first",
                    ));
                }
            }

            tx.execute(
                "INSERT INTO control_listening_intents
                 (account_scope, intent_key, intent_id, instruction_json, scope_kind,
                  created_at_ms, expires_at_ms, time_zone, basis, reported_by, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT (account_scope, intent_key) DO UPDATE SET
                    intent_id = excluded.intent_id,
                    instruction_json = excluded.instruction_json,
                    scope_kind = excluded.scope_kind,
                    created_at_ms = excluded.created_at_ms,
                    expires_at_ms = excluded.expires_at_ms,
                    time_zone = excluded.time_zone,
                    basis = excluded.basis,
                    reported_by = excluded.reported_by,
                    updated_at_ms = excluded.updated_at_ms",
                params![
                    scope,
                    intent.intent_key,
                    intent.intent_id,
                    instruction_json,
                    scope_kind,
                    created_i64,
                    expires_i64,
                    intent.time_zone,
                    basis,
                    intent.reported_by,
                    now_i64
                ],
            )
            .map_err(|_| error_storage())?;

            let new_rev = bump_revision_tx(&tx, scope)?;
            tx.commit().map_err(|_| error_storage())?;
            Ok((intent, new_rev))
        })
    }

    fn clear_intent(
        &self,
        account_scope: AccountScope,
        intent_key: String,
        expected_intent_id: Option<String>,
        basis: ListeningIntentBasis,
        expected_revision: Option<u64>,
        now_ms: u64,
    ) -> BoxFuture<'_, Result<Option<(String, u64)>, ControlError>> {
        journal_task(Arc::clone(&self.journal), move |conn| {
            let now_i64 = i64::try_from(now_ms).unwrap_or(i64::MAX);
            let scope = account_scope.as_str();
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|_| error_storage())?;
            tx.execute(
                "DELETE FROM control_listening_intents WHERE account_scope = ?1 AND expires_at_ms <= ?2",
                params![scope, now_i64],
            )
            .map_err(|_| error_storage())?;

            let rev = current_revision_tx(&tx, scope)?;
            if let Some(expected) = expected_revision
                && expected != rev
            {
                return Err(conflict_error(
                    "Moment DJ state changed before clearing this intent; read again first",
                ));
            }

            let existing: Option<(String, String)> = tx
                .query_row(
                    "SELECT intent_id, basis FROM control_listening_intents
                     WHERE account_scope = ?1 AND intent_key = ?2",
                    params![scope, intent_key],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(|_| error_storage())?;
            let Some((intent_id, existing_basis_raw)) = existing else {
                tx.commit().map_err(|_| error_storage())?;
                return Ok(None);
            };
            if expected_intent_id
                .as_ref()
                .is_some_and(|expected| expected != &intent_id)
            {
                return Err(conflict_error(
                    "this listening intent was replaced since it was read; read again first",
                ));
            }
            let existing_basis = parse_basis(&existing_basis_raw)?;
            if basis == ListeningIntentBasis::AssistantInference
                && matches!(
                    existing_basis,
                    ListeningIntentBasis::OwnerInput
                        | ListeningIntentBasis::UserRequestReportedByAssistant
                )
            {
                return Err(conflict_error(
                    "assistant inference cannot clear an explicit listener or owner intent",
                ));
            }

            tx.execute(
                "DELETE FROM control_listening_intents WHERE account_scope = ?1 AND intent_key = ?2",
                params![scope, intent_key],
            )
            .map_err(|_| error_storage())?;
            let new_rev = bump_revision_tx(&tx, scope)?;
            tx.commit().map_err(|_| error_storage())?;
            Ok(Some((intent_id, new_rev)))
        })
    }

    fn clear_all_intents(
        &self,
        account_scope: AccountScope,
        _now_ms: u64,
    ) -> BoxFuture<'_, Result<u64, ControlError>> {
        journal_task(Arc::clone(&self.journal), move |conn| {
            let scope = account_scope.as_str();
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|_| error_storage())?;
            tx.execute(
                "DELETE FROM control_listening_intents WHERE account_scope = ?1",
                params![scope],
            )
            .map_err(|_| error_storage())?;
            let new_rev = bump_revision_tx(&tx, scope)?;
            tx.commit().map_err(|_| error_storage())?;
            Ok(new_rev)
        })
    }
}
