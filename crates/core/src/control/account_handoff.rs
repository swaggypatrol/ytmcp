use std::time::Duration;

use ytm_control_api::{
    AccountHandoffGoal, AccountHandoffState, ControlError, ErrorCode, OperationPhase,
    OperationResource, RetryDisposition, VerificationLevel, WriteAction,
};
use ytm_protocol::{AccountScope, AuthState, AuthStatusInfo};

use super::ControlService;
use super::write::{MusicOperation, Transition};
use super::{invalid, stale, unavailable};

/// How long a handoff waits for the user before it settles as not completed.
const HANDOFF_WAIT_MS: u64 = 30 * 60 * 1000;
/// State is reread at least this often while waiting.
const HANDOFF_POLL_MS: u64 = 5_000;

impl MusicOperation {
    /// Records that the user should change the account and waits for the change to be observed.
    /// It never opens, raises or focuses a window; the request appears in the player's activity
    /// view, and the official sign-in page stays a user action.
    pub(super) async fn account_handoff(&mut self) -> Result<Transition, ControlError> {
        let WriteAction::AccountHandoff { goal } = self.input.action else {
            return Err(invalid("expected an account handoff"));
        };
        if self.input.preconditions.expected_account_epoch != self.state.snapshot.account_epoch {
            return Err(stale("the account changed before the handoff request"));
        }
        let auth = self.state.snapshot.auth_state.clone();
        let original = auth.account_scope.clone();
        let needed = match goal {
            AccountHandoffGoal::SignIn | AccountHandoffGoal::Reauthenticate => {
                auth.state != AuthState::Authenticated
            }
            AccountHandoffGoal::SwitchAccount => {
                if auth.state != AuthState::Authenticated || original.is_none() {
                    return Err(invalid("there is no signed-in account to switch from"));
                }
                true
            }
        };
        let mut next = self.current.clone();
        next.revision += 1;
        next.resource = Some(OperationResource::AccountHandoff {
            goal,
            state: if needed {
                AccountHandoffState::AwaitingUser
            } else {
                AccountHandoffState::NotNeeded
            },
        });
        if !needed {
            self.apply(next).await?;
            return Ok(Transition::Verified(VerificationLevel::Observed));
        }
        let pending = (self.grant.principal.clone(), self.grant.scope.clone());
        let Some(_waiting) = PendingHandoff::claim(&self.service, pending) else {
            return Err(ControlError::new(
                ErrorCode::Conflict,
                RetryDisposition::QueryOperation,
                "this client already has an account handoff waiting for the user",
            ));
        };
        // Acknowledged: the request is recorded and visible; nothing was sent anywhere.
        next.phase = OperationPhase::Acknowledged;
        self.apply(next).await?;
        if let Some(epoch) = self.state.snapshot.account_epoch.clone() {
            let _ = self
                .service
                .core
                .invalidate_control_resource(epoch, "accountHandoff".into(), String::new())
                .await;
        }
        // Waiting for a person must not hold one of the shared write slots.
        self.release_write_slot();

        // From here on a failure only ends the wait; nothing was sent, so it is a rejection.
        let Ok(mut watched) = self.service.core.read_control(true).await else {
            return Ok(Transition::Rejected(unavailable("player is unavailable")));
        };
        let Some(mut events) = watched.events.take() else {
            return Ok(Transition::Rejected(unavailable(
                "the player event stream is unavailable",
            )));
        };
        let clock = self.service.core.clock().clone();
        let wait_until = clock.now_ms().saturating_add(HANDOFF_WAIT_MS);
        let mut current = watched.snapshot.auth_state;
        loop {
            if reached(goal, original.as_ref(), &current) {
                let mut done = self.current.clone();
                done.revision += 1;
                done.resource = Some(OperationResource::AccountHandoff {
                    goal,
                    state: AccountHandoffState::Completed,
                });
                self.apply(done).await?;
                return Ok(Transition::Verified(VerificationLevel::Observed));
            }
            if self.cancel.is_cancelled() {
                return Ok(Transition::Cancelled);
            }
            let remaining = wait_until.saturating_sub(clock.now_ms());
            if remaining == 0 {
                // Only an account that is actually not authenticated calls for re-authentication.
                let (code, disposition) = if current.state == AuthState::Authenticated {
                    (ErrorCode::Unavailable, RetryDisposition::NewPlanRequired)
                } else {
                    (ErrorCode::AuthRequired, RetryDisposition::ReauthRequired)
                };
                return Ok(Transition::Rejected(ControlError::new(
                    code,
                    disposition,
                    "the account change was not completed in the player within 30 minutes",
                )));
            }
            tokio::select! {
                _ = self.cancel.cancelled() => {}
                _ = tokio::time::sleep(Duration::from_millis(remaining.min(HANDOFF_POLL_MS))) => {}
                received = events.recv() => {
                    if matches!(received, Err(tokio::sync::broadcast::error::RecvError::Closed)) {
                        return Ok(Transition::Rejected(unavailable("the player event stream closed")));
                    }
                }
            }
            let Ok(state) = self.service.core.read_control(false).await else {
                return Ok(Transition::Rejected(unavailable("player is unavailable")));
            };
            current = state.snapshot.auth_state;
        }
    }
}

/// Registration of a waiting handoff, removed when the wait ends however it ends.
struct PendingHandoff {
    service: std::sync::Arc<ControlService>,
    key: (String, Option<AccountScope>),
}

impl PendingHandoff {
    fn claim(
        service: &std::sync::Arc<ControlService>,
        key: (String, Option<AccountScope>),
    ) -> Option<Self> {
        let inserted = service.pending_handoffs.lock().ok()?.insert(key.clone());
        inserted.then(|| Self {
            service: service.clone(),
            key,
        })
    }
}

impl Drop for PendingHandoff {
    fn drop(&mut self) {
        if let Ok(mut pending) = self.service.pending_handoffs.lock() {
            pending.remove(&self.key);
        }
    }
}

/// Whether the observed account state satisfies the goal relative to the account at request time.
fn reached(
    goal: AccountHandoffGoal,
    original: Option<&AccountScope>,
    auth: &AuthStatusInfo,
) -> bool {
    if auth.state != AuthState::Authenticated || auth.account_scope.is_none() {
        return false;
    }
    match goal {
        AccountHandoffGoal::SignIn => true,
        // Renewal means the same account again; another account is a switch, not a renewal.
        AccountHandoffGoal::Reauthenticate => {
            original.is_none_or(|scope| auth.account_scope.as_ref() == Some(scope))
        }
        AccountHandoffGoal::SwitchAccount => auth.account_scope.as_ref() != original,
    }
}
