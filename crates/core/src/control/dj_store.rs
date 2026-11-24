use ytm_control_api::{ControlError, DjPreference};
use ytm_protocol::AccountScope;

use crate::BoxFuture;

/// Position of a listed preference: newest first, then by ID. Later writes cannot shift a page
/// boundary expressed this way.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreferenceCursor {
    pub recorded_at_ms: u64,
    pub preference_id: String,
}

impl PreferenceCursor {
    pub fn after(preference: &DjPreference) -> Self {
        Self {
            recorded_at_ms: preference.recorded_at_ms,
            preference_id: preference.preference_id.clone(),
        }
    }
}

/// Persistent DJ preferences of each account.
pub trait DjStore: Send + Sync {
    /// Records a preference for the account. A previous entry for the same subject is replaced;
    /// a new subject beyond `limit` entries is refused.
    fn put_preference(
        &self,
        account_scope: AccountScope,
        preference: DjPreference,
        limit: usize,
    ) -> BoxFuture<'_, Result<(), ControlError>>;
    fn get_preference(
        &self,
        account_scope: AccountScope,
        preference_id: String,
    ) -> BoxFuture<'_, Result<Option<DjPreference>, ControlError>>;
    /// Removes one preference and returns it, if it existed in this account.
    fn delete_preference(
        &self,
        account_scope: AccountScope,
        preference_id: String,
    ) -> BoxFuture<'_, Result<Option<DjPreference>, ControlError>>;
    /// Lists preferences newest first, strictly after `after` in that order. Returns the page
    /// and whether more entries follow.
    fn list_preferences(
        &self,
        account_scope: AccountScope,
        after: Option<PreferenceCursor>,
        limit: usize,
    ) -> BoxFuture<'_, Result<(Vec<DjPreference>, bool), ControlError>>;

    /// Counts all recorded preferences in the account.
    fn count_preferences(
        &self,
        account_scope: AccountScope,
    ) -> BoxFuture<'_, Result<u32, ControlError>> {
        Box::pin(async move {
            let (items, _) = self.list_preferences(account_scope, None, 500).await?;
            Ok(u32::try_from(items.len()).unwrap_or(u32::MAX))
        })
    }
}
