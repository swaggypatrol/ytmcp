use std::sync::Arc;

use rusqlite::{OptionalExtension, TransactionBehavior, params};
use ytm_control_api::{
    ControlError, DjPreference, DjPreferenceSubject, ErrorCode, RetryDisposition,
};
use ytm_core::BoxFuture;
use ytm_core::control::dj_store::{DjStore, PreferenceCursor};
use ytm_protocol::AccountScope;

use crate::SqliteStorage;
use crate::operation_store::{encode, error_storage, journal_task};

// Preferences are persistent only. The `session_id` column of schema v6 is always empty for
// them; rows of the former session-scoped preferences are ignored and never decoded.

/// Decodes a stored preference. Entries written before preferences became persistent-only
/// also carry `skipReason` and `sessionId`, which were always null for persistent entries.
fn decode_preference(encoded: &str) -> Result<DjPreference, ControlError> {
    let mut value: serde_json::Value =
        serde_json::from_str(encoded).map_err(|_| error_storage())?;
    if let Some(object) = value.as_object_mut() {
        object.remove("skipReason");
        object.remove("sessionId");
    }
    serde_json::from_value(value).map_err(|_| error_storage())
}

impl DjStore for SqliteStorage {
    fn put_preference(
        &self,
        account_scope: AccountScope,
        preference: DjPreference,
        limit: usize,
    ) -> BoxFuture<'_, Result<(), ControlError>> {
        journal_task(Arc::clone(&self.journal), move |conn| {
            let (kind, subject) = subject_key(&preference.subject);
            let recorded = i64::try_from(preference.recorded_at_ms).unwrap_or(i64::MAX);
            let payload = encode(&preference)?;
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|_| error_storage())?;
            let replaces: bool = tx
                .query_row(
                    "SELECT EXISTS (SELECT 1 FROM control_dj_preferences
                     WHERE account_scope = ?1 AND session_id = '' AND subject_kind = ?2
                       AND subject_id = ?3)",
                    params![account_scope.as_str(), kind, subject],
                    |row| row.get(0),
                )
                .map_err(|_| error_storage())?;
            if !replaces {
                let count: i64 = tx
                    .query_row(
                        "SELECT COUNT(*) FROM control_dj_preferences
                         WHERE account_scope = ?1 AND session_id = ''",
                        params![account_scope.as_str()],
                        |row| row.get(0),
                    )
                    .map_err(|_| error_storage())?;
                if usize::try_from(count).unwrap_or(usize::MAX) >= limit {
                    return Err(ControlError::new(
                        ErrorCode::Conflict,
                        RetryDisposition::NotRetryable,
                        "the DJ preference list is full; clear an entry first",
                    ));
                }
            }
            tx.execute(
                "INSERT INTO control_dj_preferences
                 (account_scope, preference_id, session_id, subject_kind, subject_id,
                  recorded_at_ms, preference_json)
                 VALUES (?1, ?2, '', ?3, ?4, ?5, ?6)
                 ON CONFLICT (account_scope, session_id, subject_kind, subject_id) DO UPDATE SET
                    preference_id = excluded.preference_id,
                    recorded_at_ms = excluded.recorded_at_ms,
                    preference_json = excluded.preference_json",
                params![
                    account_scope.as_str(),
                    preference.preference_id,
                    kind,
                    subject,
                    recorded,
                    payload
                ],
            )
            .map_err(|_| error_storage())?;
            tx.commit().map_err(|_| error_storage())
        })
    }

    fn get_preference(
        &self,
        account_scope: AccountScope,
        preference_id: String,
    ) -> BoxFuture<'_, Result<Option<DjPreference>, ControlError>> {
        journal_task(Arc::clone(&self.journal), move |conn| {
            let encoded: Option<String> = conn
                .query_row(
                    "SELECT preference_json FROM control_dj_preferences
                     WHERE account_scope = ?1 AND session_id = '' AND preference_id = ?2",
                    params![account_scope.as_str(), preference_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|_| error_storage())?;
            encoded.as_deref().map(decode_preference).transpose()
        })
    }

    fn delete_preference(
        &self,
        account_scope: AccountScope,
        preference_id: String,
    ) -> BoxFuture<'_, Result<Option<DjPreference>, ControlError>> {
        journal_task(Arc::clone(&self.journal), move |conn| {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|_| error_storage())?;
            let encoded: Option<String> = tx
                .query_row(
                    "SELECT preference_json FROM control_dj_preferences
                     WHERE account_scope = ?1 AND session_id = '' AND preference_id = ?2",
                    params![account_scope.as_str(), preference_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|_| error_storage())?;
            let Some(encoded) = encoded else {
                return Ok(None);
            };
            tx.execute(
                "DELETE FROM control_dj_preferences
                 WHERE account_scope = ?1 AND session_id = '' AND preference_id = ?2",
                params![account_scope.as_str(), preference_id],
            )
            .map_err(|_| error_storage())?;
            tx.commit().map_err(|_| error_storage())?;
            decode_preference(&encoded).map(Some)
        })
    }

    fn list_preferences(
        &self,
        account_scope: AccountScope,
        after: Option<PreferenceCursor>,
        limit: usize,
    ) -> BoxFuture<'_, Result<(Vec<DjPreference>, bool), ControlError>> {
        journal_task(Arc::clone(&self.journal), move |conn| {
            let fetch = i64::try_from(limit.saturating_add(1)).map_err(|_| error_storage())?;
            let (has_cursor, recorded, id) = match after {
                Some(cursor) => (
                    true,
                    i64::try_from(cursor.recorded_at_ms).unwrap_or(i64::MAX),
                    cursor.preference_id,
                ),
                None => (false, 0, String::new()),
            };
            // Keyset paging in the listed order, so writes between pages cannot shift them.
            let mut stmt = conn
                .prepare(
                    "SELECT preference_json FROM control_dj_preferences
                     WHERE account_scope = ?1 AND session_id = ''
                       AND (?3 = 0 OR (-recorded_at_ms, preference_id) > (-?4, ?5))
                     ORDER BY recorded_at_ms DESC, preference_id
                     LIMIT ?2",
                )
                .map_err(|_| error_storage())?;
            let rows: Vec<String> = stmt
                .query_map(
                    params![account_scope.as_str(), fetch, has_cursor, recorded, id],
                    |row| row.get(0),
                )
                .map_err(|_| error_storage())?
                .collect::<Result<_, _>>()
                .map_err(|_| error_storage())?;
            let more = rows.len() > limit;
            let items = rows
                .iter()
                .take(limit)
                .map(|row| decode_preference(row))
                .collect::<Result<Vec<_>, _>>()?;
            Ok((items, more))
        })
    }
}

fn subject_key(subject: &DjPreferenceSubject) -> (&'static str, String) {
    match subject {
        DjPreferenceSubject::Video { video_id, .. } => ("video", video_id.as_str().to_owned()),
        DjPreferenceSubject::Artist { artist_id, .. } => ("artist", artist_id.as_str().to_owned()),
    }
}
