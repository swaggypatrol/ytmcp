use ytm_control_api::{ControlError, ItemPhase, OperationPhase, OperationResource};
use ytm_protocol::{
    AccountEpoch, AgentActivityRequest, AgentActivitySnapshot, AgentOperationPhase,
    AgentOperationSummary, AgentPreferenceStance, AgentPreferenceSummary,
};

use super::dj_feedback::PREFERENCE_LIMIT as STORED_PREFERENCES;
use super::journal::{JournalEntry, OperationKey};
use super::{ControlRead, ControlService, stale, unavailable};

const ACTIVITY_LIMIT: usize = 24;
/// Preferences listed on the activity page, newest first.
const SHOWN_PREFERENCES: usize = 50;

impl ControlService {
    /// Called only by the local desktop endpoint. Control clients cannot enumerate other principals.
    pub async fn desktop_activity(
        &self,
        expected_account_epoch: Option<AccountEpoch>,
        request: AgentActivityRequest,
    ) -> Result<AgentActivitySnapshot, ControlError> {
        let state = self
            .core
            .read_control(false)
            .await
            .map_err(|_| unavailable("player is unavailable"))?;
        if state.snapshot.account_epoch != expected_account_epoch {
            return Err(stale("the account changed before reading desktop activity"));
        }
        let scope = state.snapshot.auth_state.account_scope.clone();
        let records = self
            .operations
            .as_ref()
            .ok_or_else(|| unavailable("the operation journal is unavailable"))?
            .recent(scope.clone(), ACTIVITY_LIMIT)
            .await?;
        match request {
            AgentActivityRequest::Read => {}
            AgentActivityRequest::Cancel {
                principal,
                operation_id,
            } => {
                self.check_activity_account(&state).await?;
                let active = self
                    .active_writes
                    .lock()
                    .map_err(|_| unavailable("the operation scheduler is unavailable"))?;
                let key = OperationKey {
                    principal,
                    account_scope: scope.clone(),
                    operation_id,
                };
                let token = active
                    .get(&key)
                    .ok_or_else(|| stale("the operation already left the scheduler"))?;
                token.cancel();
            }
            AgentActivityRequest::ClearDjPreference { preference_id } => {
                // The desktop is the user's own trusted input; it needs no client grant.
                self.check_activity_account(&state).await?;
                let scope = scope
                    .clone()
                    .ok_or_else(|| stale("DJ preferences need a signed-in account"))?;
                self.dj_store()?
                    .delete_preference(scope, preference_id)
                    .await?
                    .ok_or_else(|| stale("the DJ preference was already removed"))?;
            }
        }

        self.check_activity_account(&state).await?;
        let operations = {
            // The scheduler lock is released before any later await.
            let active = self
                .active_writes
                .lock()
                .map_err(|_| unavailable("the operation scheduler is unavailable"))?;
            records
                .into_iter()
                .map(|record| {
                    let key = OperationKey {
                        principal: record.principal.clone(),
                        account_scope: scope.clone(),
                        operation_id: record.receipt.operation_id.clone(),
                    };
                    let can_cancel = !record.receipt.scheduling_stopped
                        && active.get(&key).is_some_and(|token| !token.is_cancelled());
                    project_operation(record, can_cancel)
                })
                .collect()
        };
        // The account holds at most `STORED_PREFERENCES`; reading them all gives the total.
        let stored = match (&scope, self.dj_store.as_ref()) {
            (Some(scope), Some(store)) => {
                store
                    .list_preferences(scope.clone(), None, STORED_PREFERENCES)
                    .await?
                    .0
            }
            _ => Vec::new(),
        };
        Ok(AgentActivitySnapshot {
            operations,
            preferences_total: u32::try_from(stored.len()).unwrap_or(u32::MAX),
            preferences: stored
                .into_iter()
                .take(SHOWN_PREFERENCES)
                .map(project_preference)
                .collect(),
        })
    }

    pub(super) async fn check_activity_account(
        &self,
        before: &ControlRead,
    ) -> Result<(), ControlError> {
        let after = self
            .core
            .read_control(false)
            .await
            .map_err(|_| unavailable("player is unavailable"))?;
        if before.snapshot.account_epoch != after.snapshot.account_epoch
            || before.snapshot.auth_state.account_scope != after.snapshot.auth_state.account_scope
        {
            return Err(stale("the account changed during desktop activity"));
        }
        Ok(())
    }
}

fn project_preference(entry: ytm_control_api::DjPreference) -> AgentPreferenceSummary {
    use ytm_control_api::{DjPreferenceStance, DjPreferenceSubject};
    let (subject_kind, subject_id, label) = match entry.subject {
        DjPreferenceSubject::Video { video_id, title } => {
            ("video", video_id.as_str().to_owned(), title)
        }
        DjPreferenceSubject::Artist { artist_id, name } => {
            ("artist", artist_id.as_str().to_owned(), name)
        }
    };
    AgentPreferenceSummary {
        preference_id: entry.preference_id,
        subject_kind: subject_kind.into(),
        subject_id,
        label,
        stance: match entry.stance {
            DjPreferenceStance::Prefer => AgentPreferenceStance::Prefer,
            DjPreferenceStance::Avoid => AgentPreferenceStance::Avoid,
        },
        recorded_by: entry.recorded_by,
    }
}

fn project_operation(record: JournalEntry, can_cancel: bool) -> AgentOperationSummary {
    let receipt = record.receipt;
    AgentOperationSummary {
        principal: record.principal,
        operation_id: receipt.operation_id,
        phase: match receipt.phase {
            OperationPhase::Accepted => AgentOperationPhase::Accepted,
            OperationPhase::Submitted => AgentOperationPhase::Submitted,
            OperationPhase::Acknowledged => AgentOperationPhase::Acknowledged,
            OperationPhase::Verified => AgentOperationPhase::Verified,
            OperationPhase::Unknown => AgentOperationPhase::Unknown,
            OperationPhase::Partial => AgentOperationPhase::Partial,
            OperationPhase::Rejected => AgentOperationPhase::Rejected,
            OperationPhase::Cancelled => AgentOperationPhase::Cancelled,
        },
        resource: receipt.resource.map(|resource| {
            match resource {
                OperationResource::Player { .. } => "player",
                OperationResource::Queue => "queue",
                OperationResource::Playlist { .. } => "playlist",
                OperationResource::Track { .. } => "track",
                OperationResource::Artist { .. } => "artist",
                OperationResource::LibraryMembership { .. } => "libraryMembership",
                OperationResource::LocalHistory { .. } => "localHistory",
                OperationResource::Settings => "settings",
                OperationResource::LyricsOffset { .. } => "lyricsOffset",
                OperationResource::Operation { .. } => "operation",
                OperationResource::Interruption { .. } => "interruption",
                OperationResource::DjPreference { .. } => "djPreference",
                OperationResource::AccountHandoff { .. } => "accountHandoff",
            }
            .to_owned()
        }),
        verified_items: receipt
            .item_results
            .iter()
            .filter(|item| item.phase == ItemPhase::Verified)
            .count(),
        total_items: receipt.item_results.len(),
        can_cancel,
    }
}
