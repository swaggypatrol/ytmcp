use ytm_control_api::{
    ControlError, ListeningIntentBasis, ListeningIntentInstruction, ListeningIntentScope,
    ListeningIntentView,
};
use ytm_protocol::AccountScope;

use crate::BoxFuture;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredListeningIntent {
    pub intent_key: String,
    pub intent_id: String,
    pub instruction: ListeningIntentInstruction,
    pub scope: ListeningIntentScope,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
    pub time_zone: Option<String>,
    pub basis: ListeningIntentBasis,
    pub reported_by: String,
}

impl StoredListeningIntent {
    /// Converts to the MCP-visible view, stripping `reported_by` so principals never see
    /// other registered client names.
    pub fn to_view(&self) -> ListeningIntentView {
        ListeningIntentView {
            intent_key: self.intent_key.clone(),
            intent_id: self.intent_id.clone(),
            instruction: self.instruction.clone(),
            scope: self.scope,
            created_at_ms: self.created_at_ms,
            expires_at_ms: self.expires_at_ms,
            basis: self.basis,
        }
    }
}

/// Persistent session/day-scoped listening intents partitioned by account scope.
pub trait MomentStore: Send + Sync {
    /// Lists unexpired intents (`expires_at_ms > now_ms`), ordered deterministically by key,
    /// along with the account's current monotonic moment revision.
    fn list_active_intents(
        &self,
        account_scope: AccountScope,
        now_ms: u64,
    ) -> BoxFuture<'_, Result<(Vec<StoredListeningIntent>, u64), ControlError>>;

    fn moment_revision(
        &self,
        account_scope: AccountScope,
    ) -> BoxFuture<'_, Result<u64, ControlError>>;

    /// Inserts or replaces an intent on `intent.intent_key`, checking `expected_revision` (if
    /// provided) and enforcing that `AssistantInference` cannot overwrite an unexpired
    /// `OwnerInput` or `UserRequestReportedByAssistant` entry. Returns `(stored, new_revision)`.
    fn upsert_intent(
        &self,
        account_scope: AccountScope,
        intent: StoredListeningIntent,
        expected_revision: Option<u64>,
        max_active: usize,
        now_ms: u64,
    ) -> BoxFuture<'_, Result<(StoredListeningIntent, u64), ControlError>>;

    /// Clears the intent under `intent_key`, honoring `basis` precedence. With `intent_id`, a
    /// different intent under that key (one that replaced the expected one) is a conflict.
    /// Returns `Some((cleared_intent_id, new_revision))` if one existed.
    fn clear_intent(
        &self,
        account_scope: AccountScope,
        intent_key: String,
        intent_id: Option<String>,
        basis: ListeningIntentBasis,
        expected_revision: Option<u64>,
        now_ms: u64,
    ) -> BoxFuture<'_, Result<Option<(String, u64)>, ControlError>>;

    /// Owner clears all temporary intents for the account. Returns new monotonic revision.
    fn clear_all_intents(
        &self,
        account_scope: AccountScope,
        now_ms: u64,
    ) -> BoxFuture<'_, Result<u64, ControlError>>;
}
