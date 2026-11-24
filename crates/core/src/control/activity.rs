use ytm_control_api::{
    ControlError, ItemPhase, ListeningIntentBasis, ListeningIntentScope, OperationPhase,
    OperationResource,
};
use ytm_protocol::{
    AccountEpoch, AgentActivityRequest, AgentActivitySnapshot, AgentListeningIntentSummary,
    AgentOperationPhase, AgentOperationSummary, AgentPreferenceStance, AgentPreferenceSummary,
    PlayerOp,
};

use super::dj_feedback::PREFERENCE_LIMIT as STORED_PREFERENCES;
use super::journal::{JournalEntry, OperationKey};
use super::{ControlRead, ControlService, stale, unavailable};

const ACTIVITY_LIMIT: usize = 24;
/// Preferences listed on the activity page by default, newest first.
const DEFAULT_SHOWN_PREFERENCES: usize = 50;

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
        let mut shown_preferences = DEFAULT_SHOWN_PREFERENCES;
        match request {
            AgentActivityRequest::Read => {}
            AgentActivityRequest::ReadWithOptions { preferences_limit } => {
                if let Some(limit) = preferences_limit {
                    shown_preferences = usize::from(limit).clamp(1, STORED_PREFERENCES);
                }
            }
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
                if let Some(epoch) = state.snapshot.account_epoch.clone() {
                    let _ = self
                        .core
                        .invalidate_control_resource(epoch, "djPreferences".into(), String::new())
                        .await;
                }
            }
            AgentActivityRequest::ClearListeningIntent {
                intent_key,
                intent_id,
            } => {
                self.check_activity_account(&state).await?;
                let scope = scope
                    .clone()
                    .ok_or_else(|| stale("listening intents need a signed-in account"))?;
                let now_ms = self.core.clock().wall_now_ms();
                self.moment_store()?
                    .clear_intent(
                        scope,
                        intent_key,
                        intent_id,
                        ListeningIntentBasis::OwnerInput,
                        None,
                        now_ms,
                    )
                    .await?
                    .ok_or_else(|| stale("the listening intent was already removed or expired"))?;
                if let Some(epoch) = state.snapshot.account_epoch.clone() {
                    let _ = self
                        .core
                        .invalidate_control_resource(
                            epoch,
                            "listeningIntents".into(),
                            String::new(),
                        )
                        .await;
                }
                self.core.notify_moment_changed().await;
            }
            AgentActivityRequest::ClearAllListeningIntents => {
                self.check_activity_account(&state).await?;
                let scope = scope
                    .clone()
                    .ok_or_else(|| stale("listening intents need a signed-in account"))?;
                let now_ms = self.core.clock().wall_now_ms();
                self.moment_store()?
                    .clear_all_intents(scope, now_ms)
                    .await?;
                if let Some(epoch) = state.snapshot.account_epoch.clone() {
                    let _ = self
                        .core
                        .invalidate_control_resource(
                            epoch,
                            "listeningIntents".into(),
                            String::new(),
                        )
                        .await;
                }
                self.core.notify_moment_changed().await;
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
        let listening_intents = match (&scope, self.moment_store.as_ref()) {
            (Some(scope), Some(store)) => store
                .list_active_intents(scope.clone(), self.core.clock().wall_now_ms())
                .await?
                .0
                .into_iter()
                .map(project_intent)
                .collect(),
            _ => Vec::new(),
        };
        Ok(AgentActivitySnapshot {
            operations,
            preferences_total: u32::try_from(stored.len()).unwrap_or(u32::MAX),
            preferences: stored
                .into_iter()
                .take(shown_preferences)
                .map(project_preference)
                .collect(),
            listening_intents,
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

fn project_intent(
    entry: super::moment_store::StoredListeningIntent,
) -> AgentListeningIntentSummary {
    let (kind, value) = super::moment::intent_kind_value(&entry.instruction);
    AgentListeningIntentSummary {
        intent_key: entry.intent_key,
        intent_id: entry.intent_id,
        kind: kind.to_owned(),
        value,
        scope_kind: match entry.scope {
            ListeningIntentScope::Today => "today",
            ListeningIntentScope::Session => "session",
            ListeningIntentScope::Until => "until",
        }
        .to_owned(),
        expires_at_ms: entry.expires_at_ms,
        basis: match entry.basis {
            ListeningIntentBasis::OwnerInput => "ownerInput",
            ListeningIntentBasis::UserRequestReportedByAssistant => {
                "userRequestReportedByAssistant"
            }
            ListeningIntentBasis::AssistantInference => "assistantInference",
        }
        .to_owned(),
        reported_by: entry.reported_by,
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
        note: entry.note,
        recorded_by: entry.recorded_by,
    }
}

/// What the operation did, as a stable key the interface words in the listener's language.
fn operation_action(receipt: &ytm_control_api::OperationReceipt) -> Option<String> {
    let action = match receipt.resource.as_ref()? {
        OperationResource::Player { op } => match op {
            PlayerOp::Play | PlayerOp::PlayTarget | PlayerOp::PlayCollection => "play",
            PlayerOp::Pause => "pause",
            PlayerOp::Toggle => "togglePlayback",
            PlayerOp::Stop => "stop",
            PlayerOp::Next => "next",
            PlayerOp::Previous => "previous",
            PlayerOp::Seek => "seek",
            PlayerOp::SetVolume | PlayerOp::SetMuted => "volume",
            PlayerOp::SetShuffle | PlayerOp::SetRepeat | PlayerOp::SetAutoplay => "playbackMode",
            PlayerOp::SetQueueFilter => "upNextFilter",
            PlayerOp::Enqueue => "enqueue",
            PlayerOp::SelectQueueEntry
            | PlayerOp::RemoveQueueEntry
            | PlayerOp::MoveQueueEntry
            | PlayerOp::ClearUpcoming => "editQueue",
        },
        OperationResource::Queue => "editQueue",
        OperationResource::Playlist { .. } => "playlist",
        OperationResource::Track { .. } => "rateTrack",
        OperationResource::Artist { subscribed, .. } => match subscribed {
            Some(false) => "unsubscribeArtist",
            _ => "subscribeArtist",
        },
        OperationResource::LibraryMembership { saved, .. } => match saved {
            Some(false) => "removeFromLibrary",
            _ => "saveToLibrary",
        },
        OperationResource::LocalHistory { .. } => "history",
        OperationResource::Settings => "settings",
        OperationResource::LyricsOffset { .. } => "lyricsOffset",
        OperationResource::Operation { .. } => "manageOperation",
        OperationResource::Interruption { .. } => "interrupt",
        OperationResource::DjPreference { cleared: true, .. } => "forgetTaste",
        OperationResource::DjPreference { .. } => "rememberTaste",
        OperationResource::ListeningIntent { cleared: true, .. } => "clearIntent",
        OperationResource::ListeningIntent { .. } => "setIntent",
        OperationResource::AccountHandoff { .. } => "accountHandoff",
    };
    Some(action.to_owned())
}

fn project_operation(record: JournalEntry, can_cancel: bool) -> AgentOperationSummary {
    let receipt = record.receipt;
    let action = operation_action(&receipt);
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
                OperationResource::ListeningIntent { .. } => "listeningIntent",
                OperationResource::AccountHandoff { .. } => "accountHandoff",
            }
            .to_owned()
        }),
        action,
        verified_items: receipt
            .item_results
            .iter()
            .filter(|item| item.phase == ItemPhase::Verified)
            .count(),
        total_items: receipt.item_results.len(),
        can_cancel,
    }
}
