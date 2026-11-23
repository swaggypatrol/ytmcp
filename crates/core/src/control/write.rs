use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, broadcast};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use ytm_control_api::{
    ControlError, ErrorCode, GrantAction, OperationPhase, OperationReceipt, OperationResource,
    PlayContextTarget, RetryDisposition, Verification, VerificationLevel, WriteAction, WriteInput,
};
use ytm_protocol::{
    CommandId, CommandLifecycleStatus, PlaybackRef, PlayerCommandArgs, PlayerCommandPayload,
    PlayerOp, PlaylistId, StateDelta,
};

use super::account_limits;
use super::journal::{Claim, OperationKey, OperationStore};
use super::{ControlRead, ControlService, ReadGrant, invalid, map_query_error, stale, unavailable};
use crate::{BoxFuture, PlaybackFence};

const MAX_OPERATIONS: usize = 16;
const OBSERVATION_BUDGET_MS: u64 = 20_000;

pub struct DispatchGrant {
    pub deadline_wall_ms: u64,
    pub editable_playlists: Vec<PlaylistId>,
}

pub trait DispatchAuthority: Send + Sync {
    fn authorize(&self, action: GrantAction) -> BoxFuture<'_, Result<DispatchGrant, ControlError>>;
}

impl ControlService {
    pub async fn submit(
        self: &Arc<Self>,
        grant: ReadGrant,
        authority: Arc<dyn DispatchAuthority>,
        input: WriteInput,
    ) -> Result<OperationReceipt, ControlError> {
        self.note_agent_write();
        let cancel = CancellationToken::new();
        let store = self
            .operations
            .clone()
            .ok_or_else(|| unavailable("the operation journal is unavailable"))?;
        let scope_action = if let WriteAction::Cancel {
            target_operation_id,
        }
        | WriteAction::ResumeOperation {
            target_operation_id,
            ..
        } = &input.action
        {
            validate_id(target_operation_id)?;
            if target_operation_id == &input.operation_id {
                return Err(invalid("a cancellation must have a different operationId"));
            }
            let original = store
                .load_request(OperationKey {
                    principal: grant.principal.clone(),
                    account_scope: grant.scope.clone(),
                    operation_id: target_operation_id.clone(),
                })
                .await?
                .ok_or_else(|| invalid("no target operation in this account and client"))?;
            if matches!(
                original.action,
                WriteAction::Cancel { .. } | WriteAction::ResumeOperation { .. }
            ) {
                return Err(invalid("the target must be a business operation"));
            }
            required_scope(&original.action)?
        } else {
            required_scope(&input.action)?
        };
        validate_id(&input.operation_id)?;
        reject_browse_playlist_ids(&input.action)?;
        authority.authorize(scope_action).await?;
        let state = self.authorized_state(&grant, false).await?;
        let key = OperationKey {
            principal: grant.principal.clone(),
            account_scope: grant.scope.clone(),
            operation_id: input.operation_id.clone(),
        };
        let slot = if matches!(input.action, WriteAction::Cancel { .. }) {
            None
        } else {
            Some(self.write_slots.clone().try_acquire_owned().map_err(|_| {
                ControlError::new(
                    ErrorCode::Busy,
                    RetryDisposition::QueryOperation,
                    "too many music operations are active",
                )
            })?)
        };
        let payload = serde_json::to_vec(&(ytm_control_api::CONTROL_VERSION, &input))
            .map_err(|_| invalid("invalid operation format"))?;
        let hash = Sha256::digest(payload).into();
        let removed = self.planned_history_removal(&grant, &input.action).await?;
        let service = self.clone();

        // A disconnect cannot strand an already committed claim before its worker starts.
        tokio::spawn(async move {
            let initial = OperationReceipt {
                operation_id: key.operation_id.clone(),
                account_scope: key.account_scope.clone(),
                revision: 0,
                phase: OperationPhase::Accepted,
                resource: None,
                verification: None,
                item_results: vec![],
                scheduling_stopped: false,
                settled: false,
                retry_disposition: RetryDisposition::QueryOperation,
                error: None,
                removed,
            };
            // The check and the claim it admits are one step, so concurrent writes cannot
            // pass a limit together. A replayed operation ID is answered by its claim.
            let charge = account_limits::charge(&input, &initial);
            let limit_guard = match charge {
                Some(_) => Some(service.account_limits.lock().await),
                None => None,
            };
            if let Some((tier, items)) = charge
                && store.get(key.clone()).await?.is_none()
            {
                let now = service.core.clock().wall_now_ms();
                let recent = store
                    .account_operations(
                        key.account_scope.clone(),
                        now.saturating_sub(account_limits::WINDOW_MS),
                    )
                    .await?;
                account_limits::admit(tier, items, now, &recent)?;
            }
            let claimed = store
                .claim(
                    key.clone(),
                    input.clone(),
                    hash,
                    initial,
                    service.core.clock().wall_now_ms(),
                )
                .await?;
            drop(limit_guard);
            let accepted = match claimed {
                Claim::Existing(original) => return Ok(original),
                Claim::Conflict => {
                    return Err(ControlError::new(
                        ErrorCode::IdempotencyConflict,
                        RetryDisposition::NotRetryable,
                        "the operation ID already identifies different parameters",
                    ));
                }
                Claim::New(accepted) => accepted,
            };
            service
                .active_writes
                .lock()
                .map_err(|_| unavailable("operation scheduling is unavailable"))?
                .insert(key.clone(), cancel.clone());
            service
                .core
                .control_operation_changed(state.snapshot.account_epoch.clone(), &key)
                .await;
            let worker = MusicOperation {
                service,
                store,
                key,
                grant,
                authority,
                input,
                state,
                current: accepted.clone(),
                scope_action,
                cancel,
                created_playlist: None,
                _slot: slot,
            };
            tokio::spawn(worker.run());
            Ok(accepted)
        })
        .await
        .map_err(|_| unavailable("the operation journal task stopped"))?
    }

    pub(super) fn write_semaphore() -> Arc<Semaphore> {
        Arc::new(Semaphore::new(MAX_OPERATIONS))
    }
}

pub(super) struct MusicOperation {
    pub(super) service: Arc<ControlService>,
    pub(super) store: Arc<dyn OperationStore>,
    pub(super) key: OperationKey,
    pub(super) grant: ReadGrant,
    pub(super) authority: Arc<dyn DispatchAuthority>,
    pub(super) input: WriteInput,
    pub(super) state: ControlRead,
    pub(super) current: OperationReceipt,
    pub(super) scope_action: GrantAction,
    pub(super) cancel: CancellationToken,
    pub(super) created_playlist: Option<PlaylistId>,
    pub(super) _slot: Option<OwnedSemaphorePermit>,
}

impl MusicOperation {
    /// Gives the shared write slot back while the operation only waits for the user or for
    /// playback to progress; nothing more is sent from it during the wait.
    pub(super) fn release_write_slot(&mut self) {
        self._slot = None;
    }

    async fn run(mut self) {
        let result = match self.input.action {
            WriteAction::CreatePrivatePlaylist { .. } | WriteAction::AddPlaylistEntries { .. } => {
                self.playlist().await
            }
            WriteAction::UpdatePlaylist { .. }
            | WriteAction::SetPlaylistPrivacy { .. }
            | WriteAction::RemovePlaylistEntries { .. }
            | WriteAction::MovePlaylistEntry { .. }
            | WriteAction::DeletePlaylist { .. } => self.edit_playlist().await,
            WriteAction::Enqueue { .. } => self.enqueue().await,
            WriteAction::ReplaceUpcoming { .. } => self.replace_upcoming().await,
            WriteAction::InterruptWith { .. } => self.interrupt_with().await,
            WriteAction::Rate { .. } => self.rate().await,
            WriteAction::SetArtistSubscription { .. } => self.set_artist_subscription().await,
            WriteAction::SetLibraryMembership { .. } => self.set_library_membership().await,
            WriteAction::EditLocalHistory { .. } => self.edit_local_history().await,
            WriteAction::SetDjFeedback { .. } => self.set_dj_feedback().await,
            WriteAction::AccountHandoff { .. } => self.account_handoff().await,
            WriteAction::UpdateSettings { .. } => self.update_settings().await,
            WriteAction::SetLyricsOffset { .. } => self.set_lyrics_offset().await,
            WriteAction::Cancel { .. } => self.cancel_target().await,
            WriteAction::ResumeOperation { .. } => self.reconcile().await,
            _ => self.perform().await,
        };
        let state = match result {
            Ok(transition) => transition,
            Err(error) => {
                if matches!(
                    self.current.phase,
                    OperationPhase::Submitted | OperationPhase::Acknowledged
                ) {
                    Transition::Unknown(error)
                } else {
                    Transition::Rejected(error)
                }
            }
        };
        self.finish(state).await;
        if let Ok(mut active) = self.service.active_writes.lock() {
            active.remove(&self.key);
        }
    }

    async fn update_settings(&mut self) -> Result<Transition, ControlError> {
        let WriteAction::UpdateSettings {
            expected_settings_revision,
            patch,
        } = self.input.action.clone()
        else {
            return Err(invalid("expected a settings patch"));
        };
        if patch
            .ui_scale_percent
            .is_some_and(|value| !(75..=150).contains(&value))
        {
            return Err(invalid("UI scale must be between 75 and 150 percent"));
        }
        if self.input.preconditions.expected_account_epoch != self.state.snapshot.account_epoch
            || self.state.settings_revision != expected_settings_revision
        {
            return Err(stale(
                "the expected settings revision or account does not match",
            ));
        }
        if self.cancel.is_cancelled() {
            return Ok(Transition::Cancelled);
        }
        let mut submitted = self.current.clone();
        submitted.revision += 1;
        submitted.phase = OperationPhase::Submitted;
        submitted.resource = Some(OperationResource::Settings);
        self.apply(submitted).await?;
        self.authority.authorize(GrantAction::Preferences).await?;
        if self.cancel.is_cancelled() {
            return Ok(Transition::Cancelled);
        }
        match self
            .service
            .core
            .patch_control_settings(
                expected_settings_revision,
                patch,
                self.grant.scope.as_ref(),
                self.state.snapshot.account_epoch.as_ref(),
            )
            .await
        {
            Ok(()) => Ok(Transition::Verified(VerificationLevel::Persisted)),
            Err(error) if error.code == ytm_protocol::QueryErrorCode::StaleContext => {
                Ok(Transition::Rejected(map_query_error(error)))
            }
            Err(error) => Err(map_query_error(error)),
        }
    }

    async fn cancel_target(&mut self) -> Result<Transition, ControlError> {
        self.authority.authorize(self.scope_action).await?;
        let WriteAction::Cancel {
            target_operation_id,
        } = &self.input.action
        else {
            return Err(invalid("expected a cancellation"));
        };
        if self.input.preconditions.expected_account_epoch != self.state.snapshot.account_epoch {
            return Err(stale("the account changed before cancellation"));
        }
        self.service
            .check_read_context(&self.grant, &self.state)
            .await?;
        let key = OperationKey {
            operation_id: target_operation_id.clone(),
            ..self.key.clone()
        };
        let active = self
            .service
            .active_writes
            .lock()
            .map_err(|_| unavailable("the operation scheduler is unavailable"))?;
        if let Some(token) = active.get(&key) {
            token.cancel();
        }
        Ok(Transition::Verified(VerificationLevel::Persisted))
    }

    pub(super) async fn apply(&mut self, next: OperationReceipt) -> Result<(), ControlError> {
        let previous = self.current.revision;
        self.store
            .update(
                self.key.clone(),
                previous,
                next.clone(),
                self.service.core.clock().wall_now_ms(),
            )
            .await?;
        self.current = next;
        self.service
            .core
            .control_operation_changed(self.state.snapshot.account_epoch.clone(), &self.key)
            .await;
        Ok(())
    }

    async fn finish(&mut self, transition: Transition) {
        let mut next = self.current.clone();
        next.revision += 1;
        next.scheduling_stopped = true;
        next.settled = true;
        next.retry_disposition = RetryDisposition::NotRetryable;
        match transition {
            Transition::Verified(level) => {
                next.phase = OperationPhase::Verified;
                next.verification = Some(Verification {
                    level,
                    observed_at_ms: self.service.core.clock().wall_now_ms(),
                });
            }
            Transition::Cancelled => {
                next.phase = OperationPhase::Cancelled;
                next.removed.clear();
                for item in &mut next.item_results {
                    if item.phase == ytm_control_api::ItemPhase::NotStarted {
                        item.phase = ytm_control_api::ItemPhase::Cancelled;
                    }
                }
            }
            Transition::Rejected(error) => {
                // Nothing left the account or device, so there is nothing to restore.
                next.removed.clear();
                next.phase = OperationPhase::Rejected;
                next.retry_disposition = error.retry_disposition;
                next.error = Some(error);
            }
            Transition::Partial { settled, error } => {
                next.phase = OperationPhase::Partial;
                next.settled = settled;
                next.retry_disposition = if settled {
                    RetryDisposition::NewPlanRequired
                } else {
                    RetryDisposition::ReconcileFirst
                };
                next.error = Some(error);
            }
            Transition::Unknown(error) => {
                next.phase = OperationPhase::Unknown;
                next.settled = false;
                next.retry_disposition = RetryDisposition::ReconcileFirst;
                next.error = Some(ControlError::new(
                    ErrorCode::ResultUnknown,
                    RetryDisposition::ReconcileFirst,
                    error.message,
                ));
            }
        }
        if self.cancel.is_cancelled() {
            for item in &mut next.item_results {
                if item.phase == ytm_control_api::ItemPhase::NotStarted {
                    item.phase = ytm_control_api::ItemPhase::Cancelled;
                }
            }
        }
        if self.apply(next).await.is_err() {
            // Submitted intent remains in the journal and can be recovered after restart.
            tracing::warn!("music operation outcome could not be recorded");
        }
    }

    async fn perform(&mut self) -> Result<Transition, ControlError> {
        if self.cancel.is_cancelled() {
            return Ok(Transition::Cancelled);
        }

        self.check_preconditions()?;
        let (op, args) = self.player_action().await?;
        let track_instance_id = args
            .as_ref()
            .and_then(|args| args.track_instance_id.clone());
        let mut submitted = self.current.clone();
        submitted.revision += 1;
        submitted.phase = OperationPhase::Submitted;
        submitted.resource = Some(OperationResource::Player { op });
        self.apply(submitted).await?;

        let scope = self.scope_action;
        let outcome = self
            .dispatch_playback(
                op,
                args,
                track_instance_id,
                self.input.preconditions.expected_queue_revision,
                scope,
            )
            .await?;
        Ok(if op == PlayerOp::ClearUpcoming {
            super::queue::settle_clear(outcome, self.state.queue.as_ref())
        } else {
            outcome
        })
    }

    pub(super) async fn dispatch_playback(
        &self,
        op: PlayerOp,
        args: Option<PlayerCommandArgs>,
        track_instance_id: Option<ytm_protocol::TrackInstanceId>,
        queue_revision: Option<u64>,
        grant_action: GrantAction,
    ) -> Result<Transition, ControlError> {
        self.dispatch_playback_as(
            new_command_id()?,
            op,
            args,
            track_instance_id,
            queue_revision,
            grant_action,
        )
        .await
    }

    /// Same as `dispatch_playback` with a caller-chosen command ID, so the caller can read the
    /// engine's completion evidence for that command afterwards.
    pub(super) async fn dispatch_playback_as(
        &self,
        command_id: CommandId,
        op: PlayerOp,
        args: Option<PlayerCommandArgs>,
        track_instance_id: Option<ytm_protocol::TrackInstanceId>,
        queue_revision: Option<u64>,
        grant_action: GrantAction,
    ) -> Result<Transition, ControlError> {
        if self.cancel.is_cancelled() {
            return Ok(Transition::Cancelled);
        }
        let Some(bridge) = self.state.snapshot.player.bridge_epoch.clone() else {
            return Ok(Transition::Rejected(stale(
                "the page no longer has a playback bridge",
            )));
        };
        let payload = PlayerCommandPayload {
            command_id: command_id.clone(),
            op,
            args,
        };
        // Nothing has been sent yet, so a failed read here is a rejection, not an unknown.
        let mut state_with_receiver = match self.service.authorized_state(&self.grant, true).await {
            Ok(state) => state,
            Err(error) => return Ok(Transition::Rejected(error)),
        };
        let Some(mut receiver) = state_with_receiver.events.take() else {
            return Ok(Transition::Rejected(unavailable(
                "the player event stream is unavailable",
            )));
        };
        let grant = match self.authority.authorize(grant_action).await {
            Ok(grant) => grant,
            Err(error) => return Ok(Transition::Rejected(error)),
        };
        let clock = self.service.core.clock();
        let budget = grant
            .deadline_wall_ms
            .saturating_sub(clock.wall_now_ms())
            .min(OBSERVATION_BUDGET_MS);
        if budget == 0 {
            return Ok(Transition::Rejected(permission(
                "the grant expired before dispatch",
            )));
        }
        let fence = PlaybackFence {
            account_scope: self.grant.scope.clone(),
            account_epoch: self.state.snapshot.account_epoch.clone(),
            bridge_epoch: bridge,
            generation: self.state.playback_generation,
            track_instance_id,
            queue_revision,
            deadline_ms: clock.now_ms().saturating_add(budget),
            cancel: self.cancel.clone(),
        };
        let execute = async {
            let receipt = match self
                .service
                .core
                .submit_external_player_command(payload, fence)
                .await
            {
                Ok(receipt) => receipt,
                Err(error) if queue_clear_refused(op, &error) => {
                    return Ok(Transition::Rejected(map_query_error(error)));
                }
                Err(error) => {
                    // A command refused at admission (fence, payload, context, busy) or before
                    // its send never started a bridge dispatch, so it cannot have reached the page.
                    let dispatched = self
                        .service
                        .core
                        .may_have_reached_page(command_id.clone())
                        .await
                        .map_err(|_| unavailable("the command observation is unavailable"))?;
                    let cancelled = self
                        .service
                        .core
                        .command_receipt(command_id.clone())
                        .await
                        .map_err(|_| unavailable("the command observation is unavailable"))?
                        .is_some_and(|receipt| receipt.status == CommandLifecycleStatus::Cancelled);
                    if !dispatched || cancelled {
                        return Ok(Transition::Rejected(map_query_error(error)));
                    }
                    return Err(map_query_error(error));
                }
            };
            if let Some(result) = terminal(receipt) {
                return Ok(result);
            }
            loop {
                match receiver.recv().await {
                    Ok(StateDelta::CommandUpdated { receipt, .. })
                        if receipt.command_id == command_id =>
                    {
                        if let Some(result) = terminal(receipt) {
                            return Ok(result);
                        }
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        let latest = self
                            .service
                            .core
                            .command_receipt(command_id.clone())
                            .await
                            .map_err(|_| unavailable("the command observation is unavailable"))?;
                        if let Some(result) = latest.and_then(terminal) {
                            return Ok(result);
                        }
                    }
                    Err(_) => return Err(unavailable("the command observation stream closed")),
                }
            }
        };
        let settled = tokio::time::timeout(Duration::from_millis(budget), execute)
            .await
            .map_err(|_| unavailable("playback observation timed out"))??;
        // A failed receipt is only unknown if the page may have acted; a bridge refusal before
        // any page action is a rejection.
        if let Transition::Unknown(error) = settled {
            let reached = self
                .service
                .core
                .may_have_reached_page(command_id)
                .await
                .map_err(|_| unavailable("the command observation is unavailable"))?;
            return Ok(if reached {
                Transition::Unknown(error)
            } else {
                Transition::Rejected(error)
            });
        }
        Ok(settled)
    }

    pub(super) fn check_preconditions(&self) -> Result<(), ControlError> {
        let snapshot = &self.state.snapshot;
        let Some(bridge) = snapshot.player.bridge_epoch.as_ref() else {
            return Err(unavailable("the playback bridge is unavailable"));
        };
        let required = &self.input.preconditions;
        if required.expected_account_epoch != snapshot.account_epoch
            || required.expected_bridge_epoch.as_ref() != Some(bridge)
            || required
                .expected_queue_revision
                .is_some_and(|revision| revision != snapshot.queue_summary.queue_revision)
        {
            return Err(stale(
                "the account, playback bridge or queue revision no longer matches",
            ));
        }
        Ok(())
    }

    async fn player_action(&self) -> Result<(PlayerOp, Option<PlayerCommandArgs>), ControlError> {
        use WriteAction::*;
        let mut args = empty_args();
        let op = match &self.input.action {
            Resume => PlayerOp::Play,
            Pause => PlayerOp::Pause,
            Stop => PlayerOp::Stop,
            Next => PlayerOp::Next,
            Previous => PlayerOp::Previous,
            ClearUpcoming { context_token } => {
                let binding = self
                    .service
                    .resolve_context(&self.grant, context_token)
                    .await?;
                let player = &self.state.snapshot.player;
                let revision = self
                    .input
                    .preconditions
                    .expected_queue_revision
                    .ok_or_else(|| invalid("clearing the queue requires a queue revision"))?;
                if binding.queue_revision != revision
                    || binding.track_instance_id != player.track_instance_id
                {
                    return Err(stale(
                        "the current occurrence or queue changed since it was observed",
                    ));
                }
                args.queue_entry_id = Some(
                    player
                        .current_queue_entry_id
                        .clone()
                        .ok_or_else(|| stale("the queue has no observed current occurrence"))?,
                );
                args.track_instance_id = Some(
                    binding
                        .track_instance_id
                        .ok_or_else(|| stale("the queue has no observed playback instance"))?,
                );
                args.queue_revision = Some(revision);
                PlayerOp::ClearUpcoming
            }
            Play {
                target:
                    PlayContextTarget::QueueEntry {
                        context_token,
                        entry_id,
                    },
            }
            | SelectQueueEntry {
                context_token,
                entry_id,
            }
            | RemoveQueueEntry {
                context_token,
                entry_id,
            }
            | MoveQueueEntry {
                context_token,
                entry_id,
                ..
            } => {
                let binding = self
                    .service
                    .resolve_context(&self.grant, context_token)
                    .await?;
                let revision = self
                    .input
                    .preconditions
                    .expected_queue_revision
                    .ok_or_else(|| invalid("queue editing requires a revision"))?;
                if binding.queue_revision != revision
                    || binding.track_instance_id != self.state.snapshot.player.track_instance_id
                    || !self.state.queue.as_ref().is_some_and(|queue| {
                        queue
                            .entries
                            .iter()
                            .any(|entry| &entry.entry_id == entry_id)
                    })
                {
                    return Err(stale(
                        "the requested queue occurrence no longer matches the captured context",
                    ));
                }
                args.queue_entry_id = Some(entry_id.clone());
                args.queue_revision = Some(revision);
                args.track_instance_id = binding.track_instance_id;
                match &self.input.action {
                    MoveQueueEntry {
                        before_entry_id, ..
                    } => {
                        args.before_queue_entry_id = before_entry_id.clone();
                        PlayerOp::MoveQueueEntry
                    }
                    RemoveQueueEntry { .. } => PlayerOp::RemoveQueueEntry,
                    _ => PlayerOp::SelectQueueEntry,
                }
            }
            Seek {
                context_token,
                position_ms,
            } => {
                let binding = self
                    .service
                    .resolve_context(&self.grant, context_token)
                    .await?;
                if binding.track_instance_id.as_ref()
                    != self.state.snapshot.player.track_instance_id.as_ref()
                {
                    return Err(stale("the seek target is no longer the current track"));
                }
                args.track_instance_id = Some(
                    binding
                        .track_instance_id
                        .ok_or_else(|| invalid("the context has no playback instance"))?,
                );
                args.position_ms = Some(*position_ms);
                PlayerOp::Seek
            }
            Volume { volume } => {
                if !(0.0..=1.0).contains(volume) {
                    return Err(invalid("volume must be between 0 and 1"));
                }
                args.volume = Some(*volume);
                PlayerOp::SetVolume
            }
            Mute { muted } => {
                args.muted = Some(*muted);
                PlayerOp::SetMuted
            }
            Shuffle { enabled } => {
                args.shuffle = Some(*enabled);
                PlayerOp::SetShuffle
            }
            Repeat { mode } => {
                args.repeat = Some(*mode);
                PlayerOp::SetRepeat
            }
            Autoplay { enabled } => {
                args.autoplay = Some(*enabled);
                args.queue_revision = Some(
                    self.input
                        .preconditions
                        .expected_queue_revision
                        .ok_or_else(|| {
                            invalid("autoplay changes require the observed queue revision")
                        })?,
                );
                PlayerOp::SetAutoplay
            }
            Play {
                target:
                    target @ (PlayContextTarget::Album { .. } | PlayContextTarget::Playlist { .. }),
            } => {
                args.target = Some(self.resolve_collection(target).await?);
                PlayerOp::PlayCollection
            }
            Play {
                target: PlayContextTarget::Video { video_id },
            } => {
                args.target = Some(single_video_target(video_id.clone()));
                PlayerOp::PlayTarget
            }
            Play {
                target: PlayContextTarget::Context { context_token },
            } => {
                let video_id = self
                    .service
                    .resolve_context(&self.grant, context_token)
                    .await?
                    .video_id
                    .ok_or_else(|| invalid("the context has no track"))?;
                args.target = Some(single_video_target(video_id));
                PlayerOp::PlayTarget
            }
            _ => {
                return Err(unavailable(
                    "this operation cannot be dispatched to the player",
                ));
            }
        };
        if op == PlayerOp::Stop {
            args.track_instance_id = self.state.snapshot.player.track_instance_id.clone();
        }
        Ok((op, Some(args)))
    }
}

pub(super) enum Transition {
    Cancelled,
    Verified(VerificationLevel),
    Partial { settled: bool, error: ControlError },
    Rejected(ControlError),
    Unknown(ControlError),
}

fn terminal(receipt: ytm_protocol::CommandReceipt) -> Option<Transition> {
    let refused = receipt
        .error
        .as_ref()
        .is_some_and(|error| queue_clear_refused(receipt.op, error));
    let error = || {
        receipt
            .error
            .map(map_query_error)
            .unwrap_or_else(|| unavailable("the player did not observe the requested change"))
    };
    match receipt.status {
        CommandLifecycleStatus::Observed => Some(Transition::Verified(VerificationLevel::Observed)),
        CommandLifecycleStatus::Cancelled => Some(Transition::Rejected(error())),
        CommandLifecycleStatus::Failed if refused => Some(Transition::Rejected(error())),
        CommandLifecycleStatus::Failed | CommandLifecycleStatus::Indeterminate => {
            Some(Transition::Unknown(error()))
        }
        _ => None,
    }
}

// ClearUpcoming returns these explicit errors only before the first native queue edit.
// A timeout or an indeterminate receipt may follow a partial edit and keeps its unknown outcome.
fn queue_clear_refused(op: PlayerOp, error: &ytm_protocol::QueryError) -> bool {
    use ytm_protocol::QueryErrorCode::{InvalidPayload, StaleContext, UnsupportedCapability};
    op == PlayerOp::ClearUpcoming
        && matches!(
            error.code,
            InvalidPayload | StaleContext | UnsupportedCapability
        )
}

pub(super) fn required_scope(action: &WriteAction) -> Result<GrantAction, ControlError> {
    use WriteAction::*;
    match action {
        Play {
            target: PlayContextTarget::QueueEntry { .. },
        } => Ok(GrantAction::Queue),
        Play { .. }
        | Resume
        | Pause
        | Stop
        | Next
        | Previous
        | Seek { .. }
        | Volume { .. }
        | Mute { .. }
        | Shuffle { .. }
        | Repeat { .. } => Ok(GrantAction::Playback),
        UpdateSettings { .. } | SetLyricsOffset { .. } => Ok(GrantAction::Preferences),
        SetDjFeedback { .. } => Ok(GrantAction::Dj),
        UpdatePlaylist { .. } | RemovePlaylistEntries { .. } | MovePlaylistEntry { .. } => {
            Ok(GrantAction::EditPlaylist)
        }
        SetPlaylistPrivacy { .. } => Err(permission(
            "only the listener can change playlist privacy, in the player or on YouTube Music",
        )),
        AccountHandoff { .. } => Ok(GrantAction::AccountHandoff),
        DeletePlaylist { .. } => Ok(GrantAction::DeletePlaylist),
        Enqueue { .. }
        | ReplaceUpcoming { .. }
        | InterruptWith { .. }
        | SelectQueueEntry { .. }
        | RemoveQueueEntry { .. }
        | MoveQueueEntry { .. }
        | ClearUpcoming { .. }
        | Autoplay { .. } => Ok(GrantAction::Queue),
        Rate { .. } => Ok(GrantAction::Rating),
        SetArtistSubscription { .. } => Ok(GrantAction::ArtistSubscription),
        SetLibraryMembership { .. } => Ok(GrantAction::LibraryMembership),
        EditLocalHistory { .. } => Ok(GrantAction::LocalHistory),
        CreatePrivatePlaylist { .. } => Ok(GrantAction::CreatePrivatePlaylist),
        AddPlaylistEntries {
            created_by_operation_id: Some(_),
            ..
        } => Ok(GrantAction::CreatePrivatePlaylist),
        AddPlaylistEntries { .. } => Ok(GrantAction::AddPlaylistEntries),
        _ => Err(ControlError::new(
            ErrorCode::CapabilityUnavailable,
            RetryDisposition::NotRetryable,
            "this write action is unavailable",
        )),
    }
}

fn single_video_target(video_id: ytm_protocol::VideoId) -> PlaybackRef {
    PlaybackRef {
        video_id,
        playlist_id: None,
        playlist_entry_id: None,
        browse_id: None,
        queue_entry_id: None,
        start_index: None,
    }
}

/// Writes address playlists by their playlist ID. The `VL` browse form names the same remote
/// playlist but would get different locks, grants and cache keys.
fn reject_browse_playlist_ids(action: &WriteAction) -> Result<(), ControlError> {
    use WriteAction::*;
    let id = match action {
        AddPlaylistEntries { playlist_id, .. }
        | UpdatePlaylist { playlist_id, .. }
        | SetPlaylistPrivacy { playlist_id, .. }
        | RemovePlaylistEntries { playlist_id, .. }
        | MovePlaylistEntry { playlist_id, .. }
        | DeletePlaylist { playlist_id } => Some(playlist_id),
        SetLibraryMembership {
            target: ytm_control_api::LibraryMembershipTarget::Playlist { id },
            ..
        } => Some(id),
        _ => None,
    };
    if id.is_some_and(|id| id.as_str().starts_with("VL")) {
        return Err(invalid(
            "use the playlist ID from the catalog rather than its VL browse ID",
        ));
    }
    Ok(())
}

fn validate_id(id: &str) -> Result<(), ControlError> {
    if id.is_empty() || id.len() > 128 || id.chars().any(char::is_control) {
        return Err(invalid(
            "operationId must be 1–128 bytes without control characters",
        ));
    }
    Ok(())
}

pub(super) fn new_command_id() -> Result<CommandId, ControlError> {
    CommandId::new(format!("control-{}", Uuid::new_v4()))
        .map_err(|_| unavailable("could not create a player command ID"))
}

pub(super) fn permission(message: &str) -> ControlError {
    ControlError::new(
        ErrorCode::PermissionDenied,
        RetryDisposition::NotRetryable,
        message,
    )
}

pub(super) fn empty_args() -> PlayerCommandArgs {
    PlayerCommandArgs {
        track_instance_id: None,
        position_ms: None,
        volume: None,
        muted: None,
        target: None,
        shuffle: None,
        repeat: None,
        autoplay: None,
        enqueue_position: None,
        queue_entry_id: None,
        before_queue_entry_id: None,
        queue_revision: None,
    }
}
