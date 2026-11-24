use ytm_control_api::{
    ControlError, ErrorCode, OperationReceipt, OperationReplayInput, RetryDisposition, WriteInput,
    compute_action_hashes,
};
use ytm_protocol::AccountScope;

use crate::BoxFuture;

/// The scope comes from the active account and the principal from the authenticated transport.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OperationKey {
    pub principal: String,
    pub account_scope: Option<AccountScope>,
    pub operation_id: String,
}

pub struct JournalEntry {
    pub principal: String,
    pub receipt: OperationReceipt,
}

/// An operation of any client in one account, with when it was claimed. Later updates, such as
/// settling after a restart, do not move it into a newer daily window.
pub struct AccountOperation {
    pub claimed_at_ms: u64,
    pub input: WriteInput,
    pub receipt: OperationReceipt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Claim {
    New(OperationReceipt),
    Existing(OperationReceipt),
    Conflict,
}

/// Journal storage uses compare-and-set revisions. No bearer credentials are persisted here.
pub trait OperationStore: Send + Sync {
    fn claim(
        &self,
        key: OperationKey,
        input: WriteInput,
        payload_hash: [u8; 32],
        initial: OperationReceipt,
        now_ms: u64,
    ) -> BoxFuture<'_, Result<Claim, ControlError>>;

    fn get(
        &self,
        key: OperationKey,
    ) -> BoxFuture<'_, Result<Option<OperationReceipt>, ControlError>>;

    /// Recent receipts for the desktop user's current account. Control socket reads remain principal-scoped.
    fn recent(
        &self,
        scope: Option<AccountScope>,
        limit: usize,
    ) -> BoxFuture<'_, Result<Vec<JournalEntry>, ControlError>>;

    /// Every client's operations in the account whose record changed after `since_ms`, for the
    /// daily account-change limits. Records that no longer decode are left out.
    fn account_operations(
        &self,
        scope: Option<AccountScope>,
        since_ms: u64,
    ) -> BoxFuture<'_, Result<Vec<AccountOperation>, ControlError>>;

    /// Unsettled receipts of `scope` for every client, newest first.
    fn unsettled(
        &self,
        scope: Option<AccountScope>,
        limit: usize,
    ) -> BoxFuture<'_, Result<Vec<JournalEntry>, ControlError>>;

    /// Loads the normalized business parameters for an explicit, newly authorized resume.
    fn load_request(
        &self,
        key: OperationKey,
    ) -> BoxFuture<'_, Result<Option<WriteInput>, ControlError>>;

    fn update(
        &self,
        key: OperationKey,
        expected_revision: u64,
        next: OperationReceipt,
        now_ms: u64,
    ) -> BoxFuture<'_, Result<(), ControlError>>;

    /// Resolve interrupted process-local schedules on a new daemon start. Submitted mutations
    /// remain queryable and unknown until fresh remote evidence establishes what happened.
    fn recover(&self, now_ms: u64) -> BoxFuture<'_, Result<(), ControlError>>;

    /// Validates an operation ID replay against stored business action hashes when the caller
    /// omitted bookkeeping fields (`contextToken` / `preconditions`). Returns `Ok(None)` when no
    /// operation with `key` exists yet, `Ok(Some(receipt))` when intent matches, or
    /// `Err(IdempotencyConflict)` when parameters differ.
    fn check_replay(
        &self,
        key: OperationKey,
        replay: OperationReplayInput,
    ) -> BoxFuture<'_, Result<Option<OperationReceipt>, ControlError>> {
        Box::pin(async move {
            let Some(stored) = self.load_request(key.clone()).await? else {
                return Ok(None);
            };
            let (exact_hash, tokenless_hash) = compute_action_hashes(&stored.action);
            let stored_hash = if replay.token_was_missing {
                tokenless_hash
            } else {
                exact_hash
            };
            if stored_hash != replay.action_hash {
                return Err(ControlError::new(
                    ErrorCode::IdempotencyConflict,
                    RetryDisposition::NotRetryable,
                    "the operation ID already identifies different parameters",
                ));
            }
            if let Some(expected_pre) = replay.explicit_preconditions
                && stored.preconditions != expected_pre
            {
                return Err(ControlError::new(
                    ErrorCode::IdempotencyConflict,
                    RetryDisposition::NotRetryable,
                    "the operation ID already identifies different preconditions",
                ));
            }
            self.get(key).await
        })
    }
}
