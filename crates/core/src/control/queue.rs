use std::time::Duration;

use ytm_control_api::{
    ControlError, ErrorCode, GrantAction, InterruptResumePolicy, InterruptResumeState, ItemPhase,
    ItemResult, OperationPhase, OperationResource, QueueMode, RetryDisposition, VerificationLevel,
    WriteAction,
};
use ytm_protocol::{
    EnqueuePosition, PlaybackRef, PlayerOp, QueueEntryId, QueueSnapshot, TrackInstanceId, VideoId,
};

use crate::commands::{EnqueuePlan, clear_leaves_more, upcoming_may_load};

use super::write::{MusicOperation, Transition, empty_args, new_command_id};
use super::{invalid, stale, unavailable};

/// Longest wait for the replay occurrence of an interrupted track, bounded again by the grant.
const RESUME_WAIT_CAP_MS: u64 = 6 * 60 * 60 * 1000;
/// State is reread at least this often while an interruption waits for its replay occurrence.
const RESUME_POLL_MS: u64 = 5_000;

/// Observed occurrence that every following queue step must still address.
struct QueueAnchor {
    track: TrackInstanceId,
    entry: QueueEntryId,
    revision: u64,
}

enum Inserted {
    /// Every item is verified in place, in input order, at the returned queue revision.
    All {
        revision: u64,
        entries: Vec<QueueEntryId>,
    },
    /// The sequence stopped; the transition reflects the recorded item results.
    Stopped(Transition),
}

/// One insertion; `receipt` is the caller's input index, absent for internal replay items.
struct InsertItem {
    receipt: Option<usize>,
    video_id: VideoId,
}

impl MusicOperation {
    pub(super) async fn enqueue(&mut self) -> Result<Transition, ControlError> {
        self.check_preconditions()?;
        let WriteAction::Enqueue {
            context_token,
            mode,
            video_ids,
        } = self.input.action.clone()
        else {
            return Err(invalid("expected an ordered enqueue operation"));
        };
        check_item_count(&video_ids)?;
        let anchor = self.queue_anchor(&context_token).await?;
        self.stage_items(video_ids.len(), OperationResource::Queue)
            .await?;
        let items = caller_items(&video_ids, 0);
        Ok(match self.insert_ordered(&items, mode, &anchor).await? {
            Inserted::All { .. } => Transition::Verified(VerificationLevel::Observed),
            Inserted::Stopped(transition) => transition,
        })
    }

    /// Clears the loaded upcoming entries best effort, then inserts the tracks in order. When
    /// more entries may load after the cleared ones, the inserts still happen and the result is
    /// partial.
    pub(super) async fn replace_upcoming(&mut self) -> Result<Transition, ControlError> {
        self.check_preconditions()?;
        let WriteAction::ReplaceUpcoming {
            context_token,
            video_ids,
        } = self.input.action.clone()
        else {
            return Err(invalid("expected an upcoming queue replacement"));
        };
        check_item_count(&video_ids)?;
        let anchor = self.queue_anchor(&context_token).await?;
        let open = self
            .state
            .queue
            .as_ref()
            .ok_or_else(|| stale("the playback queue is not available"))
            .map(clear_leaves_more)?;
        self.stage_items(video_ids.len(), OperationResource::Queue)
            .await?;

        let mut submitted = self.current.clone();
        submitted.revision += 1;
        submitted.phase = OperationPhase::Submitted;
        self.apply(submitted).await?;
        let mut args = empty_args();
        args.queue_entry_id = Some(anchor.entry.clone());
        args.track_instance_id = Some(anchor.track.clone());
        args.queue_revision = Some(anchor.revision);
        match self
            .dispatch_playback(
                PlayerOp::ClearUpcoming,
                Some(args),
                Some(anchor.track.clone()),
                Some(anchor.revision),
                GrantAction::Queue,
            )
            .await
        {
            Ok(Transition::Verified(_)) => {}
            Ok(other) => return Ok(other),
            Err(error) => return Ok(Transition::Unknown(error)),
        }

        let cleared = self.service.authorized_state(&self.grant, false).await?;
        // Inserting next after entries that arrived since the clear would verify a queue that
        // is not the replacement, so the inserts need the current entry to be the last one.
        // An open queue cannot promise that; its result stays partial instead.
        if !open
            && !cleared
                .queue
                .as_ref()
                .is_some_and(|queue| only_current_remains(queue, &anchor.entry))
        {
            return Ok(Transition::Partial {
                settled: true,
                error: stale(
                    "the queue changed after its upcoming entries were cleared; nothing was inserted",
                ),
            });
        }
        let anchor = QueueAnchor {
            revision: cleared.snapshot.queue_summary.queue_revision,
            ..anchor
        };
        let items = caller_items(&video_ids, 0);
        Ok(
            match self
                .insert_ordered(&items, QueueMode::Next, &anchor)
                .await?
            {
                Inserted::All { .. } if open => more_may_load(),
                Inserted::All { .. } => Transition::Verified(VerificationLevel::Observed),
                // The upcoming entries are already gone, so a stop is never a clean rejection.
                Inserted::Stopped(Transition::Rejected(error)) => Transition::Partial {
                    settled: true,
                    error,
                },
                Inserted::Stopped(Transition::Unknown(error)) => Transition::Partial {
                    settled: false,
                    error,
                },
                Inserted::Stopped(Transition::Cancelled) => Transition::Partial {
                    settled: true,
                    error: stale("insertion was cancelled after the upcoming entries were cleared"),
                },
                Inserted::Stopped(other) => other,
            },
        )
    }

    /// Switches playback to the inserted tracks now and applies the explicit resume policy.
    pub(super) async fn interrupt_with(&mut self) -> Result<Transition, ControlError> {
        self.check_preconditions()?;
        let WriteAction::InterruptWith {
            context_token,
            video_ids,
            resume_policy,
        } = self.input.action.clone()
        else {
            return Err(invalid("expected an immediate interruption"));
        };
        check_item_count(&video_ids)?;
        let anchor = self.queue_anchor(&context_token).await?;
        let queue = self
            .state
            .queue
            .as_ref()
            .ok_or_else(|| stale("the playback queue is not available"))?;
        let interrupted_video_id = queue
            .entries
            .iter()
            .find(|entry| entry.entry_id == anchor.entry)
            .map(|entry| entry.video_id.clone())
            .ok_or_else(|| stale("the current occurrence is outside the observed queue"))?;
        // Restoring requires knowing what follows; radio and partial windows cannot promise it.
        // An autoplay tail is fine: the replay occurrence sits inside the explicit list.
        if resume_policy != InterruptResumePolicy::StayOnInsertedContext
            && !crate::commands::explicit_end_loaded(queue)
        {
            return Err(partial_queue(
                "this queue cannot be restored after an interruption; choose stayOnInsertedContext",
            ));
        }
        self.stage_items(
            video_ids.len(),
            OperationResource::Interruption {
                policy: resume_policy,
                interrupted_entry_id: anchor.entry.clone(),
                interrupted_video_id: interrupted_video_id.clone(),
                resume_position_ms: None,
                resume_entry_id: None,
                resume: match resume_policy {
                    InterruptResumePolicy::ResumeInterrupted => InterruptResumeState::Pending,
                    _ => InterruptResumeState::NotApplicable,
                },
            },
        )
        .await?;
        // The switch is a playback change; it must be permitted before any queue write.
        self.authority.authorize(GrantAction::Playback).await?;
        let outcome = match resume_policy {
            InterruptResumePolicy::StayOnInsertedContext => {
                self.interrupt_with_new_context(&video_ids, &anchor).await
            }
            _ => {
                self.interrupt_in_queue(&video_ids, anchor, resume_policy, interrupted_video_id)
                    .await
            }
        };
        // A failed read or journal write after the first step must still end the resume plan
        // and report what was already verified.
        match outcome {
            Ok(transition) => Ok(transition),
            Err(error) => {
                let stopped = self.queue_interrupted(error, false);
                Ok(self.abandon_resume(stopped).await)
            }
        }
    }

    async fn interrupt_in_queue(
        &mut self,
        video_ids: &[VideoId],
        anchor: QueueAnchor,
        policy: InterruptResumePolicy,
        interrupted_video_id: VideoId,
    ) -> Result<Transition, ControlError> {
        let first = caller_items(&video_ids[..1], 0);
        let (revision, first_entry) = match self
            .insert_ordered(&first, QueueMode::Next, &anchor)
            .await?
        {
            Inserted::All { revision, entries } => (revision, entries[0].clone()),
            Inserted::Stopped(transition) => return Ok(self.abandon_resume(transition).await),
        };

        let before = self.service.authorized_state(&self.grant, false).await?;
        if !self.same_session(&before)
            || before.snapshot.player.track_instance_id.as_ref() != Some(&anchor.track)
            || before.snapshot.queue_summary.current_entry_id.as_ref() != Some(&anchor.entry)
            || before.snapshot.queue_summary.queue_revision != revision
        {
            let stopped =
                self.queue_interrupted(stale("playback changed before the interruption"), true);
            return Ok(self.abandon_resume(stopped).await);
        }
        let position_ms = before
            .snapshot
            .player
            .time_anchor
            .position_ms
            .saturating_add(if before.snapshot.player.time_anchor.extrapolating {
                self.service
                    .core
                    .clock()
                    .now_ms()
                    .saturating_sub(before.snapshot.player.time_anchor.anchor_core_monotonic_ms)
            } else {
                0
            });
        let resumes = policy == InterruptResumePolicy::ResumeInterrupted;
        let mut switching = self.current.clone();
        switching.revision += 1;
        switching.phase = OperationPhase::Submitted;
        if let Some(OperationResource::Interruption {
            resume_position_ms, ..
        }) = switching.resource.as_mut()
        {
            *resume_position_ms = resumes.then_some(position_ms);
        }
        self.apply(switching).await?;

        let mut args = empty_args();
        args.queue_entry_id = Some(first_entry.clone());
        args.queue_revision = Some(revision);
        args.track_instance_id = Some(anchor.track.clone());
        let switched = self
            .dispatch_playback(
                PlayerOp::SelectQueueEntry,
                Some(args),
                Some(anchor.track.clone()),
                Some(revision),
                GrantAction::Playback,
            )
            .await;
        let stopped = match switched {
            Ok(Transition::Verified(_)) => None,
            Ok(Transition::Rejected(error)) => Some(Transition::Partial {
                settled: true,
                error,
            }),
            Ok(Transition::Cancelled) => Some(Transition::Partial {
                settled: true,
                error: stale("the interruption was cancelled before playback switched"),
            }),
            Ok(Transition::Unknown(error)) | Err(error) => Some(Transition::Partial {
                settled: false,
                error,
            }),
            Ok(other) => Some(other),
        };
        if let Some(stopped) = stopped {
            return Ok(self.abandon_resume(stopped).await);
        }

        let playing = self.service.authorized_state(&self.grant, false).await?;
        let Some(track) = playing.snapshot.player.track_instance_id.clone() else {
            let stopped =
                self.queue_interrupted(unavailable("the inserted track has no instance"), false);
            return Ok(self.abandon_resume(stopped).await);
        };
        let mut rest = caller_items(&video_ids[1..], 1);
        if resumes {
            rest.push(InsertItem {
                receipt: None,
                video_id: interrupted_video_id,
            });
        }
        let mut inserted = vec![first_entry.clone()];
        if !rest.is_empty() {
            let following = QueueAnchor {
                track,
                entry: first_entry,
                revision: playing.snapshot.queue_summary.queue_revision,
            };
            match self
                .insert_ordered(&rest, QueueMode::Next, &following)
                .await?
            {
                Inserted::All { entries, .. } => inserted.extend(entries),
                Inserted::Stopped(transition) => return Ok(self.abandon_resume(transition).await),
            }
        }
        if !resumes {
            return Ok(Transition::Verified(VerificationLevel::Observed));
        }
        let resume_entry = inserted
            .pop()
            .ok_or_else(|| unavailable("no replay entry"))?;
        let mut waiting = self.current.clone();
        waiting.revision += 1;
        waiting.phase = OperationPhase::Acknowledged;
        if let Some(OperationResource::Interruption {
            resume_entry_id, ..
        }) = waiting.resource.as_mut()
        {
            *resume_entry_id = Some(resume_entry.clone());
        }
        self.apply(waiting).await?;
        self.await_resume(resume_entry, inserted, position_ms).await
    }

    /// Plays the first track as a new context, fenced to the verified occurrence and queue
    /// revision so a track change before dispatch refuses it.
    async fn interrupt_with_new_context(
        &mut self,
        video_ids: &[VideoId],
        interrupted: &QueueAnchor,
    ) -> Result<Transition, ControlError> {
        let mut submitted = self.current.clone();
        submitted.revision += 1;
        submitted.phase = OperationPhase::Submitted;
        submitted.item_results[0].phase = ItemPhase::Submitted;
        self.apply(submitted).await?;
        let mut args = empty_args();
        args.target = Some(PlaybackRef {
            video_id: video_ids[0].clone(),
            playlist_id: None,
            playlist_entry_id: None,
            browse_id: None,
            queue_entry_id: None,
            start_index: None,
        });
        let played = self
            .dispatch_playback(
                PlayerOp::PlayTarget,
                Some(args),
                Some(interrupted.track.clone()),
                Some(interrupted.revision),
                GrantAction::Playback,
            )
            .await;
        match played {
            Ok(Transition::Verified(_)) => {}
            Ok(Transition::Cancelled) => {
                self.record_queue_item(0, ItemPhase::Cancelled, None, None)
                    .await?;
                return Ok(Transition::Cancelled);
            }
            Ok(Transition::Rejected(error)) => {
                self.record_queue_item(0, ItemPhase::Rejected, None, Some(error.code))
                    .await?;
                return Ok(Transition::Rejected(error));
            }
            Ok(Transition::Unknown(error)) | Err(error) => {
                self.record_queue_item(0, ItemPhase::Unknown, None, Some(error.code))
                    .await?;
                return Ok(Transition::Unknown(error));
            }
            Ok(other) => return Ok(other),
        }

        let playing = self.service.authorized_state(&self.grant, false).await?;
        let player = &playing.snapshot.player;
        let (Some(track), Some(entry)) = (
            player.track_instance_id.clone(),
            player.current_queue_entry_id.clone(),
        ) else {
            let error = unavailable("the new playback context has no observed occurrence");
            self.record_queue_item(0, ItemPhase::Unknown, None, Some(error.code))
                .await?;
            return Ok(Transition::Unknown(error));
        };
        if !self.same_session(&playing)
            || player.current_track.as_ref().map(|track| &track.video_id) != Some(&video_ids[0])
        {
            let error = unavailable("the observed track is not the requested interruption");
            self.record_queue_item(0, ItemPhase::Unknown, None, Some(error.code))
                .await?;
            return Ok(Transition::Unknown(error));
        }
        self.record_queue_item(
            0,
            ItemPhase::Verified,
            Some(entry.as_str().to_owned()),
            None,
        )
        .await?;
        if video_ids.len() == 1 {
            return Ok(Transition::Verified(VerificationLevel::Observed));
        }
        let anchor = QueueAnchor {
            track,
            entry,
            revision: playing.snapshot.queue_summary.queue_revision,
        };
        let rest = caller_items(&video_ids[1..], 1);
        Ok(
            match self.insert_ordered(&rest, QueueMode::Next, &anchor).await? {
                Inserted::All { .. } => Transition::Verified(VerificationLevel::Observed),
                Inserted::Stopped(transition) => transition,
            },
        )
    }

    /// Waits until the replay occurrence starts, then seeks it to the saved position.
    /// Manual control, any divergence from the inserted run, cancellation or expiry abandons it.
    async fn await_resume(
        &mut self,
        resume_entry: QueueEntryId,
        inserted: Vec<QueueEntryId>,
        position_ms: u64,
    ) -> Result<Transition, ControlError> {
        let mut watched = self.service.authorized_state(&self.grant, true).await?;
        let mut events = watched
            .events
            .take()
            .ok_or_else(|| unavailable("the player event stream is unavailable"))?;
        let grant = match self.authority.authorize(GrantAction::Playback).await {
            Ok(grant) => grant,
            Err(error) => return Ok(self.abandon_resume(partial(error)).await),
        };
        // The wait can last as long as the inserted tracks play; it sends nothing meanwhile.
        self.release_write_slot();
        let clock = self.service.core.clock().clone();
        let wait_until = grant
            .deadline_wall_ms
            .min(clock.wall_now_ms().saturating_add(RESUME_WAIT_CAP_MS));
        loop {
            if self.cancel.is_cancelled() {
                let error = stale("the resume plan was cancelled");
                return Ok(self.abandon_resume(partial(error)).await);
            }
            let state = match self.service.authorized_state(&self.grant, false).await {
                Ok(state) => state,
                Err(error) => return Ok(self.abandon_resume(partial(error)).await),
            };
            if !self.same_session(&state) {
                let error = stale("manual control took over before the interrupted track resumed");
                return Ok(self.abandon_resume(partial(error)).await);
            }
            let player = &state.snapshot.player;
            match player.current_queue_entry_id.as_ref() {
                Some(current) if current == &resume_entry => {
                    if let Some(track) = player.track_instance_id.clone() {
                        return self.resume_at(track, position_ms).await;
                    }
                }
                Some(current) if inserted.contains(current) => {}
                Some(_) => {
                    let error = stale("playback left the interruption before it resumed");
                    return Ok(self.abandon_resume(partial(error)).await);
                }
                None => {}
            }
            if !state.queue.as_ref().is_some_and(|queue| {
                queue
                    .entries
                    .iter()
                    .any(|entry| entry.entry_id == resume_entry)
            }) {
                let error = stale("the replay occurrence left the queue");
                return Ok(self.abandon_resume(partial(error)).await);
            }
            let remaining = wait_until.saturating_sub(clock.wall_now_ms());
            if remaining == 0 {
                let error = ControlError::new(
                    ErrorCode::PermissionDenied,
                    RetryDisposition::NewPlanRequired,
                    "the grant or resume window expired before the interrupted track resumed",
                );
                return Ok(self.abandon_resume(partial(error)).await);
            }
            let wait = Duration::from_millis(remaining.min(RESUME_POLL_MS));
            tokio::select! {
                _ = self.cancel.cancelled() => {}
                _ = tokio::time::sleep(wait) => {}
                received = events.recv() => {
                    if matches!(received, Err(tokio::sync::broadcast::error::RecvError::Closed)) {
                        let error = unavailable("the player event stream closed");
                        return Ok(self.abandon_resume(partial(error)).await);
                    }
                }
            }
        }
    }

    async fn resume_at(
        &mut self,
        track: TrackInstanceId,
        position_ms: u64,
    ) -> Result<Transition, ControlError> {
        let mut args = empty_args();
        args.track_instance_id = Some(track.clone());
        args.position_ms = Some(position_ms);
        let sought = self
            .dispatch_playback(
                PlayerOp::Seek,
                Some(args),
                Some(track),
                None,
                GrantAction::Playback,
            )
            .await;
        let transition = match sought {
            Ok(Transition::Verified(level)) => {
                self.set_resume_state(InterruptResumeState::Resumed).await?;
                return Ok(Transition::Verified(level));
            }
            Ok(Transition::Rejected(error)) => partial(error),
            Ok(Transition::Cancelled) => partial(stale("the resume seek was cancelled")),
            Ok(Transition::Unknown(error)) | Err(error) => Transition::Partial {
                settled: false,
                error,
            },
            Ok(other) => other,
        };
        Ok(self.abandon_resume(transition).await)
    }

    /// Records a stopped resume plan; the transition itself is returned unchanged.
    async fn abandon_resume(&mut self, transition: Transition) -> Transition {
        let pending = matches!(
            self.current.resource,
            Some(OperationResource::Interruption {
                resume: InterruptResumeState::Pending,
                ..
            })
        );
        if pending
            && self
                .set_resume_state(InterruptResumeState::Abandoned)
                .await
                .is_err()
        {
            tracing::warn!("interruption resume state could not be recorded");
        }
        transition
    }

    async fn record_replay_entry(&mut self, entry: QueueEntryId) -> Result<(), ControlError> {
        let mut next = self.current.clone();
        let Some(OperationResource::Interruption {
            resume_entry_id, ..
        }) = next.resource.as_mut()
        else {
            return Ok(());
        };
        *resume_entry_id = Some(entry);
        next.revision += 1;
        self.apply(next).await
    }

    async fn set_resume_state(&mut self, state: InterruptResumeState) -> Result<(), ControlError> {
        let mut next = self.current.clone();
        let Some(OperationResource::Interruption { resume, .. }) = next.resource.as_mut() else {
            return Ok(());
        };
        *resume = state;
        next.revision += 1;
        self.apply(next).await
    }

    fn same_session(&self, state: &crate::engine::ControlRead) -> bool {
        state.playback_generation == self.state.playback_generation
            && state.snapshot.account_epoch == self.state.snapshot.account_epoch
            && state.snapshot.player.bridge_epoch == self.state.snapshot.player.bridge_epoch
    }

    async fn queue_anchor(&self, context_token: &str) -> Result<QueueAnchor, ControlError> {
        let bound = self
            .service
            .resolve_context(&self.grant, context_token)
            .await?;
        let original = &self.state.snapshot;
        let track = original
            .player
            .track_instance_id
            .clone()
            .ok_or_else(|| stale("the queue has no playing instance anchor"))?;
        let entry = original
            .queue_summary
            .current_entry_id
            .clone()
            .ok_or_else(|| stale("the queue has no current occurrence anchor"))?;
        let revision = self
            .input
            .preconditions
            .expected_queue_revision
            .ok_or_else(|| invalid("queue operations require a queue revision"))?;
        if bound.queue_revision != revision || bound.track_instance_id.as_ref() != Some(&track) {
            return Err(stale(
                "the context token addresses another occurrence or queue revision",
            ));
        }
        Ok(QueueAnchor {
            track,
            entry,
            revision,
        })
    }

    async fn stage_items(
        &mut self,
        count: usize,
        resource: OperationResource,
    ) -> Result<(), ControlError> {
        let mut staged = self.current.clone();
        staged.revision += 1;
        staged.resource = Some(resource);
        staged.item_results = (0..count)
            .map(|index| ItemResult {
                input_index: index as u32,
                phase: ItemPhase::NotStarted,
                resource_entry_id: None,
                error: None,
            })
            .collect();
        self.apply(staged).await
    }

    /// Inserts the items one verified step at a time. Repeated insert-next targets the same
    /// anchor, so dispatching in reverse preserves input order; each step proves its position.
    async fn insert_ordered(
        &mut self,
        items: &[InsertItem],
        mode: QueueMode,
        anchor: &QueueAnchor,
    ) -> Result<Inserted, ControlError> {
        let position = match mode {
            QueueMode::Next => EnqueuePosition::Next,
            QueueMode::Last => EnqueuePosition::Last,
        };
        let order: Vec<_> = match mode {
            QueueMode::Next => (0..items.len()).rev().collect(),
            QueueMode::Last => (0..items.len()).collect(),
        };
        let mut revision = anchor.revision;
        let mut entries: Vec<Option<QueueEntryId>> = vec![None; items.len()];
        for index in order {
            let item = &items[index];
            if self.cancel.is_cancelled() {
                return Ok(Inserted::Stopped(self.cancelled_steps()));
            }
            let before = self.service.authorized_state(&self.grant, false).await?;
            if !self.same_session(&before)
                || before.snapshot.player.track_instance_id.as_ref() != Some(&anchor.track)
                || before.snapshot.queue_summary.current_entry_id.as_ref() != Some(&anchor.entry)
                || before.snapshot.queue_summary.queue_revision != revision
            {
                return Ok(Inserted::Stopped(self.queue_interrupted(
                    stale("the playback anchor or queue changed before the next step"),
                    true,
                )));
            }
            let Some(queue) = before.queue.as_ref() else {
                return Ok(Inserted::Stopped(self.queue_interrupted(
                    stale("the playback queue is not available"),
                    true,
                )));
            };
            if mode == QueueMode::Last && !crate::commands::explicit_end_loaded(queue) {
                return Ok(Inserted::Stopped(self.queue_interrupted(
                    partial_queue("the actual end of this queue cannot be verified"),
                    true,
                )));
            }
            let target = PlaybackRef {
                video_id: item.video_id.clone(),
                playlist_id: None,
                playlist_entry_id: None,
                browse_id: None,
                queue_entry_id: None,
                start_index: None,
            };
            let Some(proof) = EnqueuePlan::capture(
                target.clone(),
                position,
                Some(queue),
                Some(anchor.track.clone()),
            ) else {
                return Ok(Inserted::Stopped(self.queue_interrupted(
                    stale("the next insertion position is outside the observed queue window"),
                    true,
                )));
            };

            let mut submitted = self.current.clone();
            submitted.phase = OperationPhase::Submitted;
            submitted.revision += 1;
            if let Some(receipt) = item.receipt {
                submitted.item_results[receipt].phase = ItemPhase::Submitted;
            }
            self.apply(submitted).await?;
            let mut args = empty_args();
            args.target = Some(target);
            args.enqueue_position = Some(position);
            args.queue_revision = Some(revision);
            let command_id = new_command_id()?;
            let result = self
                .dispatch_playback_as(
                    command_id.clone(),
                    PlayerOp::Enqueue,
                    Some(args),
                    Some(anchor.track.clone()),
                    Some(revision),
                    GrantAction::Queue,
                )
                .await;
            let transition = match result {
                Ok(transition) => transition,
                Err(error) => Transition::Unknown(error),
            };
            if !matches!(transition, Transition::Verified(_)) {
                let (phase, error, settled) = match transition {
                    Transition::Rejected(error) => (ItemPhase::Rejected, error, true),
                    Transition::Cancelled => {
                        if let Some(receipt) = item.receipt {
                            self.record_queue_item(receipt, ItemPhase::Cancelled, None, None)
                                .await?;
                        }
                        return Ok(Inserted::Stopped(self.cancelled_steps()));
                    }
                    Transition::Unknown(error) => (ItemPhase::Unknown, error, false),
                    _ => (
                        ItemPhase::Unknown,
                        unavailable("queue step could not be verified"),
                        false,
                    ),
                };
                if let Some(receipt) = item.receipt {
                    self.record_queue_item(receipt, phase, None, Some(error.code))
                        .await?;
                }
                return Ok(Inserted::Stopped(self.queue_interrupted(error, settled)));
            }

            // The engine proved the position at completion; a later read may have moved on.
            let recorded = self
                .service
                .core
                .enqueue_proof(command_id)
                .await
                .map_err(|_| unavailable("the queue observation is unavailable"))?;
            let proven = match recorded {
                Some(proven) => Some(proven),
                None => {
                    let after = self.service.authorized_state(&self.grant, false).await?;
                    after.queue.as_ref().and_then(|queue| {
                        proof
                            .observed_entry(queue, &after.snapshot.player)
                            .map(|entry| (entry.entry_id.clone(), queue.queue_revision))
                    })
                }
            };
            let Some((entry_id, next_revision)) = proven else {
                let error = unavailable("the latest queue does not prove the inserted occurrence");
                if let Some(receipt) = item.receipt {
                    self.record_queue_item(receipt, ItemPhase::Unknown, None, Some(error.code))
                        .await?;
                }
                return Ok(Inserted::Stopped(self.queue_interrupted(error, false)));
            };
            match item.receipt {
                Some(receipt) => {
                    self.record_queue_item(
                        receipt,
                        ItemPhase::Verified,
                        Some(entry_id.as_str().to_owned()),
                        None,
                    )
                    .await?;
                }
                // The only internal item is the interrupted track's replay; record it at once so
                // a later stop still reports the extra occurrence it left in the queue.
                None => self.record_replay_entry(entry_id.clone()).await?,
            }
            entries[index] = Some(entry_id);
            revision = next_revision;
        }
        Ok(Inserted::All {
            revision,
            entries: entries.into_iter().flatten().collect(),
        })
    }

    fn cancelled_steps(&self) -> Transition {
        if self
            .current
            .item_results
            .iter()
            .any(|item| item.phase == ItemPhase::Verified)
        {
            Transition::Partial {
                settled: true,
                error: stale("remaining queue steps were cancelled"),
            }
        } else {
            Transition::Cancelled
        }
    }

    fn queue_interrupted(&self, error: ControlError, settled: bool) -> Transition {
        if self
            .current
            .item_results
            .iter()
            .any(|item| item.phase == ItemPhase::Verified)
        {
            Transition::Partial { settled, error }
        } else if settled {
            Transition::Rejected(error)
        } else {
            Transition::Unknown(error)
        }
    }

    async fn record_queue_item(
        &mut self,
        index: usize,
        phase: ItemPhase,
        entry: Option<String>,
        error: Option<ErrorCode>,
    ) -> Result<(), ControlError> {
        let mut next = self.current.clone();
        next.revision += 1;
        let item = &mut next.item_results[index];
        item.phase = phase;
        item.resource_entry_id = entry;
        item.error = error;
        self.apply(next).await
    }
}

fn caller_items(video_ids: &[VideoId], first_index: usize) -> Vec<InsertItem> {
    video_ids
        .iter()
        .enumerate()
        .map(|(offset, video_id)| InsertItem {
            receipt: Some(first_index + offset),
            video_id: video_id.clone(),
        })
        .collect()
}

fn check_item_count(video_ids: &[VideoId]) -> Result<(), ControlError> {
    if !(1..=ytm_control_api::MAX_WRITE_ITEMS).contains(&video_ids.len()) {
        return Err(invalid("queue operations accept 1–50 video IDs"));
    }
    Ok(())
}

/// The outcome of a clear the page confirmed: the upcoming entries it was to remove are gone.
/// When more may follow or load later (judged from the queue before the clear), it is only
/// partly done.
pub(super) fn settle_clear(cleared: Transition, before: Option<&QueueSnapshot>) -> Transition {
    match cleared {
        Transition::Verified(_) if before.is_none_or(clear_leaves_more) => more_may_load(),
        other => other,
    }
}

fn more_may_load() -> Transition {
    partial(partial_queue(
        "the upcoming entries in view were cleared (at most 50 at a time); more may still follow \
         the current track or load later",
    ))
}

/// A queue whose explicit list ends with its current entry.
fn only_current_remains(queue: &QueueSnapshot, current: &QueueEntryId) -> bool {
    !upcoming_may_load(queue)
        && queue.current_entry_id.as_ref() == Some(current)
        && queue.entries.last().map(|entry| &entry.entry_id) == Some(current)
}

fn partial_queue(message: &str) -> ControlError {
    ControlError::new(
        ErrorCode::PartialQueue,
        RetryDisposition::NewPlanRequired,
        message,
    )
}

/// The interruption itself happened; only its restoration or remaining steps stopped.
fn partial(error: ControlError) -> Transition {
    Transition::Partial {
        settled: true,
        error,
    }
}
