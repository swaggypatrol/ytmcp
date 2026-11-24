use std::sync::Arc;

use uuid::Uuid;
use ytm_control_api::{
    ControlError, DjFeedbackChange, DjFeedbackTarget, DjPreference, DjPreferenceSubject,
    DjPreferencesInput, GrantAction, OperationPhase, OperationResource, Page, VerificationLevel,
    WriteAction,
};
use ytm_protocol::{AccountScope, Freshness, Track};

use super::dj_store::{DjStore, PreferenceCursor};
use super::write::{MusicOperation, Transition, permission};
use super::{ControlService, ReadGrant, invalid, page_size, unavailable};

/// Entries per account.
pub(super) const PREFERENCE_LIMIT: usize = 500;
const NOTE_LIMIT: usize = 200;
/// Preferences shown in one DJ prompt.
const VISIBLE_LIMIT: usize = 50;

/// The account's DJ preferences as handed to a DJ agent.
pub(super) struct AppliedPreferences {
    entries: Vec<DjPreference>,
    pub(super) total: u32,
}

impl AppliedPreferences {
    /// Entries for one client to see; other clients' registered names are not revealed.
    pub(super) fn visible(&self, principal: &str) -> Vec<DjPreference> {
        self.entries
            .iter()
            .take(VISIBLE_LIMIT)
            .cloned()
            .map(|entry| redacted(entry, principal))
            .collect()
    }
}

/// Control clients may not enumerate other principals; their entries show a neutral source.
fn redacted(mut entry: DjPreference, principal: &str) -> DjPreference {
    if entry.recorded_by != principal {
        entry.recorded_by = "anotherClient".into();
    }
    entry
}

impl ControlService {
    pub(super) fn dj_store(&self) -> Result<Arc<dyn DjStore>, ControlError> {
        self.dj_store
            .clone()
            .ok_or_else(|| unavailable("DJ persistence is unavailable"))
    }

    /// The newest preferences of the account plus total recorded count.
    pub(super) async fn applied_preferences(
        &self,
        scope: &AccountScope,
    ) -> Result<AppliedPreferences, ControlError> {
        let store = self.dj_store()?;
        let (entries, _) = store
            .list_preferences(scope.clone(), None, VISIBLE_LIMIT)
            .await?;
        let total = store.count_preferences(scope.clone()).await?;
        Ok(AppliedPreferences { entries, total })
    }

    pub(super) async fn read_dj_preferences(
        &self,
        grant: &ReadGrant,
        input: DjPreferencesInput,
    ) -> Result<Page<DjPreference>, ControlError> {
        let scope = grant
            .scope
            .clone()
            .ok_or_else(|| permission("DJ preferences require an account grant"))?;
        let limit = page_size(input.page.as_ref())?;
        let after = input
            .page
            .as_ref()
            .and_then(|page| page.cursor.as_deref())
            .map(parse_cursor)
            .transpose()?;
        let (items, more) = self
            .dj_store()?
            .list_preferences(scope, after, limit)
            .await?;
        let next_cursor = items
            .last()
            .filter(|_| more)
            .map(|last| format_cursor(&PreferenceCursor::after(last)));
        let items = items
            .into_iter()
            .map(|entry| redacted(entry, &grant.principal))
            .collect();
        Ok(Page {
            items,
            next_cursor,
            complete: !more,
            source: "local".into(),
            freshness: Freshness::Fresh,
            captured_at_ms: self.core.clock().wall_now_ms(),
        })
    }
}

impl MusicOperation {
    pub(super) async fn set_dj_feedback(&mut self) -> Result<Transition, ControlError> {
        if self.state.snapshot.account_epoch.is_none()
            || self.input.preconditions.expected_account_epoch != self.state.snapshot.account_epoch
        {
            return Err(permission(
                "DJ preferences require the registered current account",
            ));
        }
        let WriteAction::SetDjFeedback { change } = self.input.action.clone() else {
            return Err(invalid("expected DJ feedback"));
        };
        let scope = self
            .grant
            .scope
            .clone()
            .ok_or_else(|| permission("DJ preferences require an account grant"))?;
        let store = self.service.dj_store()?;
        match change {
            DjFeedbackChange::Record {
                target,
                stance,
                note,
            } => {
                let note = checked_note(note)?;
                let label = self.known_label(&scope, &target).await;
                let subject = match target {
                    DjFeedbackTarget::Video { video_id } => DjPreferenceSubject::Video {
                        video_id,
                        title: label,
                    },
                    DjFeedbackTarget::Artist { artist_id } => DjPreferenceSubject::Artist {
                        artist_id,
                        name: label,
                    },
                };
                let preference = DjPreference {
                    preference_id: Uuid::new_v4().to_string(),
                    subject,
                    stance,
                    note,
                    recorded_by: self.grant.principal.clone(),
                    recorded_at_ms: self.service.core.clock().wall_now_ms(),
                };
                self.submit_preference(OperationResource::DjPreference {
                    id: preference.preference_id.clone(),
                    cleared: false,
                })
                .await?;
                if self.cancel.is_cancelled() {
                    return Ok(Transition::Cancelled);
                }
                store
                    .put_preference(scope, preference, PREFERENCE_LIMIT)
                    .await?;
            }
            DjFeedbackChange::Clear { preference_id } => {
                if preference_id.is_empty()
                    || preference_id.len() > 64
                    || preference_id.chars().any(char::is_control)
                {
                    return Err(invalid("invalid DJ preference ID"));
                }
                store
                    .get_preference(scope.clone(), preference_id.clone())
                    .await?
                    .ok_or_else(|| invalid("no DJ preference with this ID in this account"))?;
                self.submit_preference(OperationResource::DjPreference {
                    id: preference_id.clone(),
                    cleared: true,
                })
                .await?;
                if self.cancel.is_cancelled() {
                    return Ok(Transition::Cancelled);
                }
                store.delete_preference(scope, preference_id).await?;
            }
        }
        if let Some(epoch) = self.state.snapshot.account_epoch.clone() {
            let _ = self
                .service
                .core
                .invalidate_control_resource(epoch, "djPreferences".into(), String::new())
                .await;
        }
        Ok(Transition::Verified(VerificationLevel::Persisted))
    }

    /// Names the subject from tracks the player already knows: the current track, the
    /// observed queue, then local history. An unknown subject stays unnamed; a label is never
    /// a reason to fail the write.
    async fn known_label(&self, scope: &AccountScope, target: &DjFeedbackTarget) -> Option<String> {
        let player = self.state.snapshot.player.current_track.iter();
        let queue = self
            .state
            .queue
            .iter()
            .flat_map(|queue| queue.entries.iter().map(|entry| &entry.track));
        if let Some(label) = label_in(player.chain(queue), target) {
            return Some(label);
        }
        let history = self.service.history.as_ref()?;
        let snapshot = history.snapshot(scope.clone()).await.ok()?;
        label_in(snapshot.records.iter().map(|record| &record.track), target)
    }

    async fn submit_preference(&mut self, resource: OperationResource) -> Result<(), ControlError> {
        self.service
            .check_read_context(&self.grant, &self.state)
            .await?;
        self.authority.authorize(GrantAction::Dj).await?;
        let mut submitted = self.current.clone();
        submitted.revision += 1;
        submitted.phase = OperationPhase::Submitted;
        submitted.resource = Some(resource);
        self.apply(submitted).await
    }
}

/// The title of the target video, or the name of the target artist, from the first track
/// that carries one.
fn label_in<'a>(
    tracks: impl IntoIterator<Item = &'a Track>,
    target: &DjFeedbackTarget,
) -> Option<String> {
    tracks.into_iter().find_map(|track| {
        let label = match target {
            DjFeedbackTarget::Video { video_id } => {
                (&track.video_id == video_id).then_some(track.title.as_str())
            }
            DjFeedbackTarget::Artist { artist_id } => track
                .artists
                .iter()
                .find(|artist| artist.id.as_ref() == Some(artist_id))
                .map(|artist| artist.name.as_str()),
        }?;
        Some(label.trim())
            .filter(|label| !label.is_empty())
            .map(str::to_owned)
    })
}

fn format_cursor(cursor: &PreferenceCursor) -> String {
    format!(
        "djPreferences/{}/{}",
        cursor.recorded_at_ms, cursor.preference_id
    )
}

fn parse_cursor(cursor: &str) -> Result<PreferenceCursor, ControlError> {
    cursor
        .strip_prefix("djPreferences/")
        .and_then(|rest| rest.split_once('/'))
        .and_then(|(recorded_at_ms, preference_id)| {
            Some(PreferenceCursor {
                recorded_at_ms: recorded_at_ms.parse().ok()?,
                preference_id: Some(preference_id)
                    .filter(|id| !id.is_empty() && id.len() <= 64)?
                    .to_owned(),
            })
        })
        .ok_or_else(|| invalid("invalid DJ preference cursor"))
}

fn checked_note(note: Option<String>) -> Result<Option<String>, ControlError> {
    let Some(note) = note else {
        return Ok(None);
    };
    let note = note.trim();
    if note.chars().count() > NOTE_LIMIT || note.chars().any(char::is_control) {
        return Err(invalid(
            "a DJ preference note is at most 200 characters without control characters",
        ));
    }
    Ok((!note.is_empty()).then(|| note.to_owned()))
}
