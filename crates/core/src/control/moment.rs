use std::sync::Arc;

use uuid::Uuid;
use ytm_control_api::{
    ActivityKind, ControlError, EnergyLevel, EnvironmentView, ErrorCode, GrantAction,
    ListeningIntentBasis, ListeningIntentChange, ListeningIntentDuration,
    ListeningIntentInstruction, ListeningIntentScope, ListeningIntentsView, MomentDayPart,
    MomentOccasion, MomentOccasionKind, MomentOccasionPhase, MomentSectionStatus, MomentTime,
    MomentView, OperationPhase, OperationResource, ReportedIntentBasis, RetryDisposition,
    SceneCity, SceneCitySource, VerificationLevel, VocalsMode, WriteAction,
};
use ytm_protocol::{
    AccountEpoch, AccountScope, HostEnvironmentSnapshotPayload, HostOccasion, MomentDesktopRequest,
    MomentDesktopSnapshot, OwnerMomentConfig,
};

use super::moment_store::{MomentStore, StoredListeningIntent};
use super::write::{MusicOperation, Transition, permission};
use super::{ControlService, ReadGrant, invalid, stale, unavailable};
use crate::engine::ControlRead;

const MAX_ACTIVE_INTENTS_PER_ACCOUNT: usize = 16;
const REV_STRIDE: u64 = 1_000_000;
const SESSION_DURATION_MS: u64 = 6 * 60 * 60 * 1000;
const MAX_UNTIL_DURATION_MS: u64 = 48 * 60 * 60 * 1000;
const MAX_BRIEF_CHARS: usize = 120;
const MAX_THEME_ID_CHARS: usize = 96;

pub(crate) fn account_scope_key(scope: Option<&AccountScope>) -> String {
    scope
        .map(|s| s.as_str().to_owned())
        .unwrap_or_else(|| "__signed_out__".to_owned())
}

pub(crate) fn is_principal_shared(
    config: &OwnerMomentConfig,
    scope: Option<&AccountScope>,
    principal: &str,
) -> bool {
    let key = account_scope_key(scope);
    config
        .shared_principals_by_account
        .get(&key)
        .is_some_and(|principals| principals.iter().any(|entry| entry == principal))
}

fn parse_day_part(raw: Option<&str>) -> Option<MomentDayPart> {
    match raw? {
        "earlyMorning" => Some(MomentDayPart::EarlyMorning),
        "morning" => Some(MomentDayPart::Morning),
        "afternoon" => Some(MomentDayPart::Afternoon),
        "evening" => Some(MomentDayPart::Evening),
        "lateNight" => Some(MomentDayPart::LateNight),
        _ => None,
    }
}

fn map_occasion(raw: &HostOccasion) -> Option<MomentOccasion> {
    let phase = match raw.phase.as_str() {
        "eve" => MomentOccasionPhase::Eve,
        "day" => MomentOccasionPhase::Day,
        "observed" => MomentOccasionPhase::Observed,
        _ => return None,
    };
    let kind = match raw.kind.as_str() {
        "public" => MomentOccasionKind::Public,
        "observance" => MomentOccasionKind::Observance,
        _ => return None,
    };
    Some(MomentOccasion {
        id: raw.id.clone(),
        label: raw.label.clone(),
        phase,
        kind,
        local_date: raw.local_date.clone(),
        actual_date: raw.actual_date.clone(),
        provider_version: raw.provider_version.clone(),
    })
}

/// An intent as `(kind, value)` keys for the activity view, which words them in its own
/// language: `("vocals", "preferInstrumental")`, `("musicBrief", <the brief as written>)`.
pub(crate) fn intent_kind_value(
    instruction: &ListeningIntentInstruction,
) -> (&'static str, String) {
    let (kind, value) = match instruction {
        ListeningIntentInstruction::Vocals { mode } => (
            "vocals",
            match mode {
                VocalsMode::Any => "any",
                VocalsMode::PreferInstrumental => "preferInstrumental",
            },
        ),
        ListeningIntentInstruction::Energy { level } => (
            "energy",
            match level {
                EnergyLevel::Gentle => "gentle",
                EnergyLevel::Steady => "steady",
                EnergyLevel::Driving => "driving",
            },
        ),
        ListeningIntentInstruction::Activity { kind } => (
            "activity",
            match kind {
                ActivityKind::Gaming => "gaming",
                ActivityKind::Focus => "focus",
                ActivityKind::Relax => "relax",
                ActivityKind::Social => "social",
            },
        ),
        ListeningIntentInstruction::MusicBrief { brief } => return ("musicBrief", brief.clone()),
        ListeningIntentInstruction::DismissTheme { theme_id } => {
            return ("dismissTheme", theme_id.clone());
        }
    };
    (kind, value.to_owned())
}

fn dismissed_themes(items: &[StoredListeningIntent]) -> Vec<&str> {
    items
        .iter()
        .filter_map(|item| match &item.instruction {
            ListeningIntentInstruction::DismissTheme { theme_id } => Some(theme_id.as_str()),
            _ => None,
        })
        .collect()
}

/// A theme matches an occasion by its full id (`occasion:us:christmas-day`), by the holiday part
/// of it (`christmas-day`), or as `holidays` / `occasion` for every occasion.
fn occasion_dismissed(occasion: &MomentOccasion, dismissed: &[&str]) -> bool {
    let holiday = occasion.id.rsplit(':').next().unwrap_or_default();
    dismissed.iter().any(|theme| {
        *theme == occasion.id || *theme == holiday || *theme == "holidays" || *theme == "occasion"
    })
}

/// Single authoritative projection of Moment DJ context for one `(account_scope, principal)`.
/// Used by `ytm_get_context`, `ytm_wait_dj_prompt` (live at delivery), AND the Settings preview.
pub(crate) async fn project_moment_for_principal(
    config: &OwnerMomentConfig,
    env: Option<&HostEnvironmentSnapshotPayload>,
    moment_store: Option<&dyn MomentStore>,
    scope: Option<&AccountScope>,
    principal: &str,
    wall_now_ms: u64,
) -> MomentView {
    if !is_principal_shared(config, scope, principal) {
        let cfg_rev = format!("cfg-{}", config.config_revision);
        return MomentView {
            revision: config.config_revision,
            environment: EnvironmentView {
                status: MomentSectionStatus::NotShared,
                revision: cfg_rev.clone(),
                time: None,
                captured_at_ms: None,
                occasions: Vec::new(),
                scene_city: None,
                weather_provided: false,
            },
            listening_intents: ListeningIntentsView {
                status: MomentSectionStatus::NotShared,
                revision: cfg_rev,
                items: Vec::new(),
            },
        };
    }

    // The active intents are read even when they are not shared: a theme the listener dismissed
    // for today stays out of the occasions either way. `None` means they could not be read.
    let stored = match (moment_store, scope) {
        (Some(store), Some(account_scope)) => store
            .list_active_intents(account_scope.clone(), wall_now_ms)
            .await
            .ok(),
        _ => None,
    };
    let (intents_status, intents_revision, active_intents, intents_rev_num) =
        match (&stored, config.share_listening_intents && scope.is_some()) {
            (_, false) => (
                MomentSectionStatus::NotShared,
                format!("cfg-{}", config.config_revision),
                Vec::new(),
                0u64,
            ),
            (Some((items, rev)), true) => (
                MomentSectionStatus::Shared,
                format!("intent-{rev}"),
                items.clone(),
                *rev,
            ),
            (None, true) => (
                MomentSectionStatus::Unavailable,
                format!("cfg-{}", config.config_revision),
                Vec::new(),
                0u64,
            ),
        };

    let dismissed = stored.as_ref().map(|(items, _)| dismissed_themes(items));
    let scene_city = config
        .scene_city
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|label| SceneCity {
            label: label.to_owned(),
            source: SceneCitySource::UserConfigured,
        });

    let wants_clock = config.share_time;
    let wants_holidays = config.holidays_enabled && config.holiday_country.is_some();
    let any_env_shared = wants_clock || wants_holidays || scene_city.is_some();

    let environment = if !any_env_shared {
        EnvironmentView {
            status: MomentSectionStatus::NotShared,
            revision: format!("cfg-{}", config.config_revision),
            time: None,
            captured_at_ms: None,
            occasions: Vec::new(),
            scene_city: None,
            weather_provided: false,
        }
    } else {
        // Each part fails on its own: a missing part is left out and the rest is still shared.
        let valid_env = env.filter(|snap| {
            snap.config_revision == config.config_revision && wall_now_ms <= snap.valid_until_ms
        });
        let time = valid_env
            .filter(|snap| wants_clock && snap.clock_ok)
            .and_then(|snap| {
                Some(MomentTime {
                    local_date: snap.local_date.clone()?,
                    weekday: snap.weekday.clone()?,
                    day_part: parse_day_part(snap.day_part.as_deref())?,
                    time_zone: snap.time_zone.clone()?,
                })
            });
        // Without the intents, a dismissed occasion cannot be told apart, so none is shown.
        let occasions: Option<Vec<MomentOccasion>> = match (valid_env, &dismissed) {
            (Some(snap), Some(dismissed)) if wants_holidays && snap.calendar_ok => Some(
                snap.occasions
                    .iter()
                    .filter_map(map_occasion)
                    .filter(|occ| {
                        config.include_cultural_observances
                            || occ.kind == MomentOccasionKind::Public
                    })
                    .filter(|occ| !occasion_dismissed(occ, dismissed))
                    .collect(),
            ),
            _ => None,
        };
        let missing = (wants_clock && time.is_none()) || (wants_holidays && occasions.is_none());
        let status = if missing && time.is_none() && occasions.is_none() && scene_city.is_none() {
            MomentSectionStatus::Unavailable
        } else {
            MomentSectionStatus::Shared
        };
        EnvironmentView {
            status,
            revision: format!(
                "cfg-{}-env-{}",
                config.config_revision,
                valid_env.map_or(0, |snap| snap.captured_at_ms)
            ),
            captured_at_ms: time.as_ref().and(valid_env).map(|snap| snap.captured_at_ms),
            time,
            occasions: occasions.unwrap_or_default(),
            scene_city,
            weather_provided: false,
        }
    };

    let revision = config
        .config_revision
        .saturating_mul(REV_STRIDE)
        .saturating_add(intents_rev_num);

    MomentView {
        revision,
        environment,
        listening_intents: ListeningIntentsView {
            status: intents_status,
            revision: intents_revision,
            items: active_intents
                .into_iter()
                .map(|item| item.to_view())
                .collect(),
        },
    }
}

impl ControlService {
    pub(super) fn moment_store(&self) -> Result<Arc<dyn MomentStore>, ControlError> {
        self.moment_store
            .clone()
            .ok_or_else(|| unavailable("Moment DJ persistence is unavailable"))
    }

    pub(super) async fn project_moment(
        &self,
        grant: &ReadGrant,
        state: &ControlRead,
    ) -> MomentView {
        let now_ms = self.core.clock().wall_now_ms();
        project_moment_for_principal(
            &state.moment_config,
            state.environment.as_ref(),
            self.moment_store.as_deref(),
            grant.scope.as_ref(),
            &grant.principal,
            now_ms,
        )
        .await
    }

    /// Called only by the desktop Settings screen (never published on the MCP control socket).
    pub async fn desktop_moment_settings(
        &self,
        expected_account_epoch: Option<AccountEpoch>,
        request: MomentDesktopRequest,
        registered_principals: Vec<String>,
    ) -> Result<MomentDesktopSnapshot, ControlError> {
        let state = self
            .core
            .read_control(false)
            .await
            .map_err(|_| unavailable("player is unavailable"))?;
        if state.snapshot.account_epoch != expected_account_epoch {
            return Err(stale("the account changed before reading Moment settings"));
        }
        let account_key = account_scope_key(state.snapshot.auth_state.account_scope.as_ref());

        let preview_principal = match request {
            MomentDesktopRequest::Read { preview_principal } => preview_principal,
            MomentDesktopRequest::Update { patch } => {
                let preview = patch.preview_principal.clone();
                self.core
                    .patch_moment_config(patch, account_key.clone())
                    .await
                    .map_err(super::map_query_error)?;
                preview
            }
        };

        let fresh = self
            .core
            .read_control(false)
            .await
            .map_err(|_| unavailable("player is unavailable"))?;
        if fresh.snapshot.account_epoch != expected_account_epoch {
            return Err(stale("the account changed while updating Moment settings"));
        }

        let mut principals = registered_principals;
        principals.sort();
        principals.dedup();

        let selected_preview = preview_principal
            .filter(|name| principals.iter().any(|p| p == name))
            .or_else(|| {
                fresh
                    .moment_config
                    .shared_principals_by_account
                    .get(&account_key)
                    .and_then(|shared| {
                        shared
                            .iter()
                            .find(|name| principals.iter().any(|p| p == *name))
                            .cloned()
                    })
            })
            .or_else(|| principals.first().cloned());

        let preview_key = selected_preview
            .as_deref()
            .unwrap_or("__unregistered_preview__");
        let preview = project_moment_for_principal(
            &fresh.moment_config,
            fresh.environment.as_ref(),
            self.moment_store.as_deref(),
            fresh.snapshot.auth_state.account_scope.as_ref(),
            preview_key,
            self.core.clock().wall_now_ms(),
        )
        .await;
        let preview_json = serde_json::to_value(&preview)
            .map_err(|_| unavailable("failed to serialize Moment preview"))?;

        Ok(MomentDesktopSnapshot {
            config: fresh.moment_config,
            current_account_key: account_key,
            registered_principals: principals,
            environment: fresh.environment,
            preview_principal: selected_preview,
            preview_json,
        })
    }
}

/// An inference is decided on what the assistant read, so it must say which Moment revision
/// that was: a change the listener made since then makes it stale rather than overwritten. A
/// stale inference is decided again on a new read, not replayed with the latest revision.
fn require_decision_revision(
    basis: ReportedIntentBasis,
    expected_moment_revision: Option<u64>,
) -> Result<(), ControlError> {
    if basis == ReportedIntentBasis::Inference && expected_moment_revision.is_none() {
        return Err(invalid(
            "an inferred listening intent change needs expectedMomentRevision from the moment it was decided on",
        ));
    }
    Ok(())
}

impl MusicOperation {
    pub(super) async fn set_listening_intent(&mut self) -> Result<Transition, ControlError> {
        if self.state.snapshot.account_epoch.is_none()
            || self.input.preconditions.expected_account_epoch != self.state.snapshot.account_epoch
        {
            return Err(permission(
                "listening intents require the registered current account",
            ));
        }
        let WriteAction::SetListeningIntent { change } = self.input.action.clone() else {
            return Err(invalid("expected listening intent change"));
        };
        let scope = self
            .grant
            .scope
            .clone()
            .ok_or_else(|| permission("listening intents require an account grant"))?;
        if !is_principal_shared(
            &self.state.moment_config,
            Some(&scope),
            &self.grant.principal,
        ) || !self.state.moment_config.share_listening_intents
        {
            return Err(permission(
                "Moment listening intents are not shared with this client; enable Moment sharing in Settings first",
            ));
        }
        self.authority.authorize(GrantAction::Dj).await?;
        let store = self.service.moment_store()?;
        let now_ms = self.service.core.clock().wall_now_ms();
        let cfg_rev = self.state.moment_config.config_revision;

        let resource = match change {
            ListeningIntentChange::Set {
                instruction,
                duration,
                basis,
                expected_moment_revision,
            } => {
                require_decision_revision(basis, expected_moment_revision)?;
                let instruction = validate_instruction(instruction)?;
                let intent_key = instruction.canonical_key();
                let (scope_kind, expires_at_ms, time_zone) =
                    self.resolve_expiry(duration, now_ms)?;
                let expected_store_rev = expected_moment_revision
                    .map(|rev| expected_store_revision(rev, cfg_rev))
                    .transpose()?;
                let basis = ListeningIntentBasis::from(basis);
                let intent = StoredListeningIntent {
                    intent_key: intent_key.clone(),
                    intent_id: Uuid::new_v4().to_string(),
                    instruction,
                    scope: scope_kind,
                    created_at_ms: now_ms,
                    expires_at_ms,
                    time_zone,
                    basis,
                    reported_by: self.grant.principal.clone(),
                };
                self.submit_intent(OperationResource::ListeningIntent {
                    intent_key: intent_key.clone(),
                    intent_id: intent.intent_id.clone(),
                    cleared: false,
                    expires_at_ms: Some(expires_at_ms),
                    moment_revision: 0,
                })
                .await?;
                if self.cancel.is_cancelled() {
                    return Ok(Transition::Cancelled);
                }
                let (saved, store_rev) = match store
                    .upsert_intent(
                        scope,
                        intent,
                        expected_store_rev,
                        MAX_ACTIVE_INTENTS_PER_ACCOUNT,
                        now_ms,
                    )
                    .await
                {
                    Ok(res) => res,
                    Err(err)
                        if matches!(
                            err.code,
                            ErrorCode::Conflict
                                | ErrorCode::InvalidInput
                                | ErrorCode::PermissionDenied
                        ) =>
                    {
                        return Ok(Transition::Rejected(err));
                    }
                    Err(err) => return Err(err),
                };
                let moment_revision = cfg_rev.saturating_mul(REV_STRIDE).saturating_add(store_rev);
                OperationResource::ListeningIntent {
                    intent_key: saved.intent_key,
                    intent_id: saved.intent_id,
                    cleared: false,
                    expires_at_ms: Some(saved.expires_at_ms),
                    moment_revision,
                }
            }
            ListeningIntentChange::Clear {
                intent_key,
                intent_id,
                basis,
                expected_moment_revision,
            } => {
                require_decision_revision(basis, expected_moment_revision)?;
                let key = intent_key.trim().to_owned();
                if key.is_empty() || key.chars().count() > 128 || key.chars().any(char::is_control)
                {
                    return Err(invalid("invalid listening intent key"));
                }
                if intent_id.as_ref().is_some_and(|id| {
                    id.is_empty() || id.chars().count() > 64 || id.chars().any(char::is_control)
                }) {
                    return Err(invalid("invalid listening intent id"));
                }
                let expected_store_rev = expected_moment_revision
                    .map(|rev| expected_store_revision(rev, cfg_rev))
                    .transpose()?;
                let basis = ListeningIntentBasis::from(basis);
                self.submit_intent(OperationResource::ListeningIntent {
                    intent_key: key.clone(),
                    intent_id: String::new(),
                    cleared: true,
                    expires_at_ms: None,
                    moment_revision: 0,
                })
                .await?;
                if self.cancel.is_cancelled() {
                    return Ok(Transition::Cancelled);
                }
                let (cleared_id, store_rev) = match store
                    .clear_intent(
                        scope,
                        key.clone(),
                        intent_id,
                        basis,
                        expected_store_rev,
                        now_ms,
                    )
                    .await
                {
                    Ok(Some(res)) => res,
                    Ok(None) => {
                        return Ok(Transition::Rejected(invalid(
                            "no active listening intent with this key in this account",
                        )));
                    }
                    Err(err)
                        if matches!(
                            err.code,
                            ErrorCode::Conflict
                                | ErrorCode::InvalidInput
                                | ErrorCode::PermissionDenied
                        ) =>
                    {
                        return Ok(Transition::Rejected(err));
                    }
                    Err(err) => return Err(err),
                };
                let moment_revision = cfg_rev.saturating_mul(REV_STRIDE).saturating_add(store_rev);
                OperationResource::ListeningIntent {
                    intent_key: key,
                    intent_id: cleared_id,
                    cleared: true,
                    expires_at_ms: None,
                    moment_revision,
                }
            }
        };

        self.current.resource = Some(resource);
        if let Some(epoch) = self.state.snapshot.account_epoch.clone() {
            let _ = self
                .service
                .core
                .invalidate_control_resource(epoch, "listeningIntents".into(), String::new())
                .await;
        }
        self.service.core.notify_moment_changed().await;
        Ok(Transition::Verified(VerificationLevel::Persisted))
    }

    fn resolve_expiry(
        &self,
        duration: ListeningIntentDuration,
        now_ms: u64,
    ) -> Result<(ListeningIntentScope, u64, Option<String>), ControlError> {
        match duration {
            ListeningIntentDuration::Today => {
                // The end of today does not move during the day, so the last snapshot's day end
                // holds until it passes, even after the snapshot itself has expired.
                let env = self
                    .state
                    .environment
                    .as_ref()
                    .filter(|env| env.clock_ok && env.day_ends_at_utc_ms.is_some_and(|end| end > now_ms))
                    .ok_or_else(|| {
                        unavailable(
                            "the device's local day end is not known yet; retry with a new operationId or use the session duration",
                        )
                    })?;
                let day_end = env.day_ends_at_utc_ms.unwrap_or_default();
                Ok((ListeningIntentScope::Today, day_end, env.time_zone.clone()))
            }
            ListeningIntentDuration::Session => Ok((
                ListeningIntentScope::Session,
                now_ms.saturating_add(SESSION_DURATION_MS),
                self.state
                    .environment
                    .as_ref()
                    .and_then(|env| env.time_zone.clone()),
            )),
            ListeningIntentDuration::Until { expires_at_ms } => {
                if expires_at_ms <= now_ms
                    || expires_at_ms > now_ms.saturating_add(MAX_UNTIL_DURATION_MS)
                {
                    return Err(invalid(
                        "expiresAtMs must be in the future and within 48 hours",
                    ));
                }
                Ok((
                    ListeningIntentScope::Until,
                    expires_at_ms,
                    self.state
                        .environment
                        .as_ref()
                        .and_then(|env| env.time_zone.clone()),
                ))
            }
        }
    }

    async fn submit_intent(&mut self, resource: OperationResource) -> Result<(), ControlError> {
        self.service
            .check_read_context(&self.grant, &self.state)
            .await?;
        let mut submitted = self.current.clone();
        submitted.revision += 1;
        submitted.phase = OperationPhase::Submitted;
        submitted.resource = Some(resource);
        self.apply(submitted).await
    }
}

fn expected_store_revision(
    expected_moment_revision: u64,
    cfg_rev: u64,
) -> Result<u64, ControlError> {
    if expected_moment_revision / REV_STRIDE != cfg_rev {
        return Err(ControlError::new(
            ErrorCode::Conflict,
            RetryDisposition::NewPlanRequired,
            "Moment DJ configuration changed; read the current context before writing",
        ));
    }
    Ok(expected_moment_revision % REV_STRIDE)
}

fn validate_instruction(
    instruction: ListeningIntentInstruction,
) -> Result<ListeningIntentInstruction, ControlError> {
    match instruction {
        ListeningIntentInstruction::MusicBrief { brief } => {
            let trimmed = brief.trim().to_owned();
            if trimmed.is_empty()
                || trimmed.chars().count() > MAX_BRIEF_CHARS
                || trimmed.chars().any(char::is_control)
            {
                return Err(invalid(
                    "musicBrief must be 1 to 120 characters without control characters",
                ));
            }
            Ok(ListeningIntentInstruction::MusicBrief { brief: trimmed })
        }
        ListeningIntentInstruction::DismissTheme { theme_id } => {
            let trimmed = theme_id.trim().to_owned();
            if trimmed.is_empty()
                || trimmed.chars().count() > MAX_THEME_ID_CHARS
                || trimmed.chars().any(char::is_control)
            {
                return Err(invalid(
                    "themeId must be 1 to 96 characters without control characters",
                ));
            }
            Ok(ListeningIntentInstruction::DismissTheme { theme_id: trimmed })
        }
        other => Ok(other),
    }
}
