//! Undo of the app's own playlist additions (`ui.mutation` `undoAddToPlaylist`).

use std::collections::VecDeque;

use uuid::Uuid;
use ytm_control_api::{ControlError, ErrorCode, RetryDisposition};
use ytm_protocol::{
    AccountEpoch, AccountScope, MutationKind, MutationOutcome, MutationRequest, MutationResult,
    PlaylistId, QueryError, QueryErrorCode, VideoId,
};

use crate::control::catalog::{CatalogReadContext, ControlCatalog, PlaylistOccurrenceIdentity};
use crate::control::{EntryRemoval, EntryRemovalCheck, prepare_entry_removal};

/// How long an addition can be undone after its confirmation.
pub const UNDO_LIFETIME_MS: u64 = 5 * 60 * 1000;
/// How many recent additions are kept for undo; the oldest is dropped first.
pub const UNDO_LIMIT: usize = 16;

/// The one entry a confirmed addition created, and the account session it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AddedEntry {
    pub account_scope: AccountScope,
    pub account_epoch: AccountEpoch,
    pub playlist_id: PlaylistId,
    pub video_id: VideoId,
    pub entry_id: String,
}

/// Recent additions by opaque, single-use undo token: at most `UNDO_LIMIT`, each for
/// `UNDO_LIFETIME_MS` on the core's monotonic clock.
#[derive(Default)]
pub(crate) struct AddedEntries {
    records: VecDeque<(String, u64, AddedEntry)>,
}

impl AddedEntries {
    /// Keeps `entry` and returns the token that names it.
    pub(crate) fn remember(&mut self, entry: AddedEntry, now_ms: u64) -> String {
        self.prune(now_ms);
        while self.records.len() >= UNDO_LIMIT {
            self.records.pop_front();
        }
        let token = format!("undo-{}", Uuid::new_v4().simple());
        self.records.push_back((token.clone(), now_ms, entry));
        token
    }

    fn get(&mut self, token: &str, now_ms: u64) -> Option<AddedEntry> {
        self.prune(now_ms);
        self.records
            .iter()
            .find(|(id, _, _)| id == token)
            .map(|(_, _, entry)| entry.clone())
    }

    /// Uses up the token; `false` when it was already used or has expired.
    fn take(&mut self, token: &str) -> bool {
        let before = self.records.len();
        self.records.retain(|(id, _, _)| id != token);
        self.records.len() != before
    }

    fn prune(&mut self, now_ms: u64) {
        self.records
            .retain(|(_, at, _)| now_ms.saturating_sub(*at) < UNDO_LIFETIME_MS);
    }
}

/// Takes back the entry named by `request.undo_token`, in the account session of `context`.
/// The token is checked against that session and the request's song (and playlist, when sent);
/// a fresh complete read must show the entry and prove the playlist's order before the one
/// removal is sent, and another fresh read must show only that entry gone. The token is used
/// up when the removal is sent (or the entry is found already gone), never retried, and kept
/// when nothing could be sent. Returns the result and, when something was sent, the playlist.
pub(crate) async fn undo_addition(
    added: &std::sync::Mutex<AddedEntries>,
    editor: &dyn ControlCatalog,
    request: &MutationRequest,
    context: CatalogReadContext,
    now_ms: u64,
) -> Result<(MutationResult, Option<PlaylistId>), QueryError> {
    let refused = |error: QueryError| {
        MutationResult::new(request.kind, MutationOutcome::Rejected, Some(error))
    };
    let Some(token) = request.undo_token.as_deref() else {
        return Err(invalid("undoAddToPlaylist requires undoToken"));
    };
    let Some(entry) = lock(added).get(token, now_ms) else {
        return Ok((
            refused(stale("this addition can no longer be undone")),
            None,
        ));
    };
    if entry.account_scope != context.account_scope || entry.account_epoch != context.account_epoch
    {
        lock(added).take(token);
        return Ok((
            refused(stale("the account changed since this song was added")),
            None,
        ));
    }
    if entry.video_id != request.video_id
        || request
            .playlist_id
            .as_ref()
            .is_some_and(|id| *id != entry.playlist_id)
    {
        return Err(invalid("the undo token belongs to another addition"));
    }

    let identity = PlaylistOccurrenceIdentity {
        video_id: entry.video_id.clone(),
        entry_id: entry.entry_id.clone(),
    };
    let prepared =
        match prepare_entry_removal(editor, context.clone(), &entry.playlist_id, &identity).await {
            EntryRemovalCheck::Ready(prepared) => prepared,
            EntryRemovalCheck::Absent => {
                lock(added).take(token);
                let mut result = refused(stale(
                    "the song this addition created is no longer in the playlist",
                ));
                result.already_removed = true;
                return Ok((result, None));
            }
            EntryRemovalCheck::Unsafe(error) => return Ok((refused(query_error(error)), None)),
        };
    if !lock(added).take(token) {
        return Ok((
            refused(stale("this addition can no longer be undone")),
            None,
        ));
    }
    let scope = context.account_scope.clone();
    let removal = prepared.send(editor, context).await;
    if let Err(error) = editor
        .invalidate_playlist_cache(scope, entry.playlist_id.clone(), true)
        .await
    {
        tracing::warn!(code = ?error.code, "undo cache invalidation failed");
    }
    let result = match removal {
        EntryRemoval::Removed => {
            MutationResult::new(request.kind, MutationOutcome::Confirmed, None)
        }
        EntryRemoval::Rejected(error) => refused(query_error(error)),
        EntryRemoval::Unknown(error) => MutationResult::new(
            request.kind,
            MutationOutcome::Indeterminate,
            Some(query_error(error)),
        ),
    };
    Ok((result, Some(entry.playlist_id)))
}

/// The record a confirmed addition leaves for its undo, when the provider named its entry.
pub(crate) fn added_entry(
    request: &MutationRequest,
    account_scope: &AccountScope,
    account_epoch: &AccountEpoch,
    created_entry_id: Option<String>,
) -> Option<AddedEntry> {
    (request.kind == MutationKind::AddToPlaylist).then_some(())?;
    Some(AddedEntry {
        account_scope: account_scope.clone(),
        account_epoch: account_epoch.clone(),
        playlist_id: request.playlist_id.clone()?,
        video_id: request.video_id.clone(),
        entry_id: created_entry_id?,
    })
}

fn lock(added: &std::sync::Mutex<AddedEntries>) -> std::sync::MutexGuard<'_, AddedEntries> {
    added
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The UI's error for a control-path failure: the closest code, the same message, and whether
/// reading again may help.
fn query_error(error: ControlError) -> QueryError {
    let code = match error.code {
        ErrorCode::AuthRequired => QueryErrorCode::AuthRequired,
        ErrorCode::RateLimited => QueryErrorCode::RateLimited,
        ErrorCode::Busy => QueryErrorCode::Busy,
        ErrorCode::InvalidInput => QueryErrorCode::InvalidPayload,
        ErrorCode::StorageError => QueryErrorCode::StorageError,
        ErrorCode::PermissionDenied
        | ErrorCode::NotEditable
        | ErrorCode::CapabilityUnavailable
        | ErrorCode::IncompatibleVersion
        | ErrorCode::LimitReached => QueryErrorCode::UnsupportedCapability,
        _ => QueryErrorCode::StaleContext,
    };
    QueryError {
        code,
        retriable: matches!(error.retry_disposition, RetryDisposition::SafeReadRetry),
        retry_after_ms: error.retry_after_ms,
        safe_message: error.message,
        request_id: None,
    }
}

fn stale(message: &str) -> QueryError {
    QueryError {
        code: QueryErrorCode::StaleContext,
        retriable: false,
        retry_after_ms: None,
        safe_message: message.to_owned(),
        request_id: None,
    }
}

fn invalid(message: &str) -> QueryError {
    QueryError {
        code: QueryErrorCode::InvalidPayload,
        retriable: false,
        retry_after_ms: None,
        safe_message: message.to_owned(),
        request_id: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str) -> AddedEntry {
        AddedEntry {
            account_scope: AccountScope::new("scope").unwrap(),
            account_epoch: AccountEpoch::new("epoch").unwrap(),
            playlist_id: PlaylistId::new("PL-fixture").unwrap(),
            video_id: VideoId::new("vid").unwrap(),
            entry_id: id.to_owned(),
        }
    }

    #[test]
    fn undo_tokens_expire_and_are_bounded() {
        let mut added = AddedEntries::default();
        let first = added.remember(entry("first"), 0);
        assert_eq!(
            added.get(&first, UNDO_LIFETIME_MS - 1),
            Some(entry("first"))
        );
        assert_eq!(added.get(&first, UNDO_LIFETIME_MS), None);

        let tokens: Vec<_> = (0..=UNDO_LIMIT)
            .map(|n| added.remember(entry(&n.to_string()), UNDO_LIFETIME_MS))
            .collect();
        assert_eq!(added.get(&tokens[0], UNDO_LIFETIME_MS), None);
        assert!(added.get(&tokens[UNDO_LIMIT], UNDO_LIFETIME_MS).is_some());
        assert!(added.take(&tokens[UNDO_LIMIT]));
        assert!(!added.take(&tokens[UNDO_LIMIT]));
    }
}
