use std::collections::HashMap;
use tokio_util::sync::CancellationToken;
use ytm_protocol::{
    AccountEpoch, AccountScope, BridgeEpoch, CommandId, CommandLifecycleStatus, CommandReceipt,
    EnqueuePosition, PlaybackRef, PlaybackStatus, PlayerCommandArgs, PlayerCommandPayload,
    PlayerObservedState, PlayerOp, QueryError, QueryErrorCode, QueueCompleteness, QueueEntry,
    QueueEntryId, QueueSnapshot, QueueSummary, RepeatMode, TrackInstanceId, VideoId,
};

/// Maximum number of recent command receipts kept per `CoreEpoch` for `commandId` deduplication.
pub const MAX_DEDUP_ENTRIES: usize = 256;

/// Retention TTL for `commandId` deduplication within a single `CoreEpoch` (10 minutes).
pub const DEDUP_TTL_MS: u64 = 10 * 60 * 1000;

/// Position tolerance (in ms) when confirming `Seek` or `Stop` (seek to 0) observations.
pub const SEEK_TOLERANCE_MS: u64 = 2_000;

/// Extra forward drift allowed while playing during `Seek` observation.
pub const SEEK_PLAYING_FORWARD_DRIFT_MS: u64 = 4_000;

/// Whether an observed volume shows a `setVolume` target. The page player takes whole percent
/// (the bridge sends `round(target * 100)`), so the two match when they name the same percent.
/// A one-point step therefore never matches the observation from before the command.
fn volume_matches(observed: f64, target: f64) -> bool {
    (observed * 100.0).round() == (target * 100.0).round()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopPhase {
    PausePending,
    SeekPending,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PausedSkipPhase {
    AwaitingTrackChange,
    AwaitingRestoreSeek {
        command_id: CommandId,
        track_instance_id: TrackInstanceId,
        position_ms: u64,
        deadline_ms: u64,
        dispatch_pending: bool,
    },
    AwaitingPausedConfirmation {
        new_track_instance_id: Option<TrackInstanceId>,
    },
    ReadyToUngate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PausedSkipState {
    pub transaction_id: String,
    pub gate_engaged: bool,
    pub phase: PausedSkipPhase,
}

#[derive(Debug, Clone)]
pub enum PendingOpPlan {
    Play {
        expected_track_instance_id: Option<TrackInstanceId>,
    },
    Pause {
        expected_track_instance_id: Option<TrackInstanceId>,
    },
    Stop {
        bound_track_instance_id: Option<TrackInstanceId>,
        phase: StopPhase,
    },
    Seek {
        bound_track_instance_id: Option<TrackInstanceId>,
        position_ms: u64,
    },
    SetVolume {
        volume: f64,
    },
    SetMuted {
        muted: bool,
    },
    Next {
        prev_track_instance_id: Option<TrackInstanceId>,
        prev_queue_entry_id: Option<QueueEntryId>,
        paused_skip: Option<PausedSkipState>,
    },
    Previous {
        prev_track_instance_id: Option<TrackInstanceId>,
        prev_queue_entry_id: Option<QueueEntryId>,
        /// Estimated position at dispatch; YTM restarts the same track instead of going back
        /// when the track has played for more than a few seconds.
        prev_position_ms: u64,
        paused_skip: Option<PausedSkipState>,
    },
    PlayTarget {
        target: PlaybackRef,
        require_context: bool,
        prev_track_instance_id: Option<TrackInstanceId>,
        /// Set for core-internal restores that must land paused behind the output gate.
        paused_skip: Option<PausedSkipState>,
        restore_position_ms: Option<u64>,
    },
    SetShuffle {
        shuffle: bool,
    },
    SetRepeat {
        repeat: RepeatMode,
    },
    SetAutoplay {
        enabled: bool,
    },
    /// Observed once a queue snapshot shows this filter selected; the track, the explicit list
    /// and its revision may change meanwhile without settling it either way.
    SetQueueFilter {
        filter_id: String,
    },
    Enqueue(Option<EnqueuePlan>),
    /// `video_id` is the selected entry's video in the core's queue mirror when the command was
    /// accepted; `None` when the mirror did not hold the entry, which can then never be observed.
    SelectQueueEntry {
        entry_id: QueueEntryId,
        video_id: Option<VideoId>,
    },
    EditQueue(QueueEditPlan),
}

#[derive(Debug, Clone)]
pub struct QueueEditPlan {
    revision: u64,
    current_entry: QueueEntryId,
    track_instance: TrackInstanceId,
    /// Occurrences expected after the edit. For a clear: the loaded window through the current
    /// occurrence, starting at absolute index `start`.
    expected: Vec<(QueueEntryId, VideoId)>,
    start: usize,
    clear: Option<ClearPlan>,
    no_op_confirmed: bool,
}

/// What a best-effort clear removes and how much of the queue it can prove.
#[derive(Debug, Clone)]
struct ClearPlan {
    /// The loaded upcoming occurrences; none of them may remain.
    removed: Vec<QueueEntryId>,
    /// No explicit entry could follow the loaded ones, so the current occurrence must end up
    /// last. Otherwise entries that load later are allowed after it.
    end_in_view: bool,
}

impl QueueEditPlan {
    pub fn capture(
        op: PlayerOp,
        args: &PlayerCommandArgs,
        queue: Option<&QueueSnapshot>,
        observed: &PlayerObservedState,
    ) -> Option<Self> {
        let clearing = op == PlayerOp::ClearUpcoming;
        // Single edits address loaded positions of the whole queue; a clear only needs the
        // current occurrence and what is loaded after it.
        let queue = queue.filter(|queue| {
            (clearing || Self::finite(queue)) && Some(queue.queue_revision) == args.queue_revision
        })?;
        let current_entry = queue.current_entry_id.as_ref()?;
        let track_instance = observed.track_instance_id.as_ref()?;
        if observed.current_queue_entry_id.as_ref() != Some(current_entry)
            || !queue
                .entries
                .iter()
                .any(|entry| &entry.entry_id == current_entry)
            || (args.queue_entry_id.as_ref() == Some(current_entry)) != clearing
        {
            return None;
        }
        let mut expected: Vec<_> = Self::occurrences(queue)
            .map(|(entry, video)| (entry.clone(), video.clone()))
            .collect();
        let source = expected
            .iter()
            .position(|(entry, _)| Some(entry) == args.queue_entry_id.as_ref())?;
        let mut clear = None;
        if clearing {
            // The page removes at most the nearest `MAX_WRITE_ITEMS` upcoming entries.
            let removed = expected.split_off(source + 1);
            clear = Some(ClearPlan {
                removed: removed
                    .into_iter()
                    .take(ytm_control_api::MAX_WRITE_ITEMS)
                    .map(|(entry, _)| entry)
                    .collect(),
                end_in_view: !clear_leaves_more(queue),
            });
        } else {
            let removed = expected.remove(source);
            if op == PlayerOp::MoveQueueEntry {
                let destination = match args.before_queue_entry_id.as_ref() {
                    Some(before) => expected.iter().position(|(entry, _)| entry == before)?,
                    None => expected.len(),
                };
                if destination == source {
                    return None;
                }
                expected.insert(destination, removed);
            }
        }
        Some(Self {
            revision: queue.queue_revision,
            current_entry: current_entry.clone(),
            track_instance: track_instance.clone(),
            expected,
            start: window_start(queue),
            clear,
            no_op_confirmed: false,
        })
    }

    pub fn acknowledge_no_op(&mut self) -> bool {
        self.no_op_confirmed = self
            .clear
            .as_ref()
            .is_some_and(|clear| clear.removed.is_empty());
        self.no_op_confirmed
    }

    fn finite(queue: &QueueSnapshot) -> bool {
        queue.completeness == QueueCompleteness::Complete
            && queue.has_more == Some(false)
            && queue.loaded_range.as_ref().is_some_and(|range| {
                range.start == 0 && range.end_exclusive as usize == queue.entries.len()
            })
    }

    fn occurrences(queue: &QueueSnapshot) -> impl Iterator<Item = (&QueueEntryId, &VideoId)> {
        queue
            .entries
            .iter()
            .map(|entry| (&entry.entry_id, &entry.video_id))
    }

    fn observed(&self, queue: &QueueSnapshot, player: &PlayerObservedState) -> bool {
        let anchored = (queue.queue_revision > self.revision
            || (self.no_op_confirmed && queue.queue_revision == self.revision))
            && queue.current_entry_id.as_ref() == Some(&self.current_entry)
            && player.current_queue_entry_id.as_ref() == Some(&self.current_entry)
            && player.track_instance_id.as_ref() == Some(&self.track_instance);
        if !anchored {
            return false;
        }
        let Some(clear) = &self.clear else {
            return Self::finite(queue)
                && Self::occurrences(queue)
                    .eq(self.expected.iter().map(|(entry, video)| (entry, video)));
        };
        // Removing later entries never moves the current occurrence, so it must still sit at
        // its absolute position, with every earlier occurrence still in view where it was.
        let Some(at) = queue
            .entries
            .iter()
            .position(|entry| entry.entry_id == self.current_entry)
        else {
            return false;
        };
        let start = window_start(queue);
        let prefix_kept = start + at + 1 == self.start + self.expected.len()
            && self.expected.iter().enumerate().all(|(i, (entry, video))| {
                (self.start + i).checked_sub(start).is_none_or(|index| {
                    let observed = &queue.entries[index];
                    &observed.entry_id == entry && &observed.video_id == video
                })
            });
        let after = &queue.entries[at + 1..];
        prefix_kept
            && if clear.end_in_view {
                after.is_empty() && !upcoming_may_load(queue)
            } else {
                after
                    .iter()
                    .all(|entry| !clear.removed.contains(&entry.entry_id))
            }
    }
}

fn window_start(queue: &QueueSnapshot) -> usize {
    queue
        .loaded_range
        .as_ref()
        .map_or(0, |range| range.start as usize)
}

/// Explicit entries may exist, or load later, after the last loaded one: the page keeps a
/// continuation, the window ends before the queue does, or the queue is an endless radio.
/// An autoplay tail alone does not count; its suggestions are not queue entries.
pub(crate) fn upcoming_may_load(queue: &QueueSnapshot) -> bool {
    queue.may_load_more == Some(true)
        || !(queue.has_more == Some(false) || queue.explicit_complete == Some(true))
}

/// One clear cannot empty what comes next: explicit entries may follow or load later, or more
/// are loaded after the current occurrence than one clear removes (the nearest are removed).
pub(crate) fn clear_leaves_more(queue: &QueueSnapshot) -> bool {
    upcoming_may_load(queue)
        || queue
            .current_entry_id
            .as_ref()
            .and_then(|current| {
                queue
                    .entries
                    .iter()
                    .position(|entry| &entry.entry_id == current)
            })
            .is_none_or(|at| queue.entries.len() - at - 1 > ytm_control_api::MAX_WRITE_ITEMS)
}

#[derive(Debug, Clone)]
pub struct EnqueuePlan {
    target: VideoId,
    position: EnqueuePosition,
    revision: u64,
    current_entry: Option<QueueEntryId>,
    track_instance: Option<TrackInstanceId>,
    entries: Vec<QueueEntryId>,
    start: usize,
    insertion: usize,
    complete: bool,
}

impl EnqueuePlan {
    pub fn capture(
        target: PlaybackRef,
        position: EnqueuePosition,
        queue: Option<&QueueSnapshot>,
        track_instance: Option<TrackInstanceId>,
    ) -> Option<Self> {
        let queue = queue?;
        let complete = explicit_end_loaded(queue);
        let start = queue
            .loaded_range
            .as_ref()
            .map_or(0, |range| range.start as usize);
        let insertion = match position {
            EnqueuePosition::Last if complete => queue.entries.len(),
            EnqueuePosition::Next if complete && queue.entries.is_empty() => 0,
            EnqueuePosition::Next => {
                let anchor = queue.current_entry_id.as_ref()?;
                start
                    + queue
                        .entries
                        .iter()
                        .position(|entry| &entry.entry_id == anchor)?
                    + 1
            }
            EnqueuePosition::Last => return None,
        };
        Some(Self {
            target: target.video_id,
            position,
            revision: queue.queue_revision,
            current_entry: queue.current_entry_id.clone(),
            track_instance,
            entries: queue
                .entries
                .iter()
                .map(|entry| entry.entry_id.clone())
                .collect(),
            start,
            insertion,
            complete,
        })
    }

    pub(crate) fn observed_entry<'a>(
        &self,
        queue: &'a QueueSnapshot,
        player: &PlayerObservedState,
    ) -> Option<&'a QueueEntry> {
        if queue.queue_revision <= self.revision {
            return None;
        }
        let complete = explicit_end_loaded(queue);
        let start = queue
            .loaded_range
            .as_ref()
            .map_or(0, |range| range.start as usize);
        let index = self.insertion.checked_sub(start)?;
        let inserted = queue.entries.get(index)?;
        if inserted.video_id != self.target || self.entries.contains(&inserted.entry_id) {
            return None;
        }
        if self.position == EnqueuePosition::Last && !complete {
            return None;
        }
        if self.position == EnqueuePosition::Next {
            if let Some(anchor) = &self.current_entry {
                if queue.current_entry_id.as_ref() != Some(anchor)
                    || self.track_instance != player.track_instance_id
                    || index == 0
                    || &queue.entries[index - 1].entry_id != anchor
                {
                    return None;
                }
            } else if !complete || queue.entries.len() != 1 {
                return None;
            }
        }
        if self.complete && complete && queue.entries.len() != self.entries.len() + 1 {
            return None;
        }
        // After removing the candidate, every overlapping old position must agree. This
        // preserves occurrence order in the visible window without assuming an unseen tail.
        let retained = queue.entries.iter().enumerate().all(|(i, entry)| {
            if i == index {
                return true;
            }
            let prior_index = start + i - usize::from(i > index);
            match prior_index
                .checked_sub(self.start)
                .and_then(|i| self.entries.get(i))
            {
                Some(prior) => &entry.entry_id == prior,
                None => !self.complete,
            }
        });
        retained.then_some(inserted)
    }
}

/// The end of the explicit list is in view: either the whole queue is loaded, or the explicit
/// list is and only an autoplay tail continues after it. Appends land before that tail.
pub(crate) fn explicit_end_loaded(queue: &QueueSnapshot) -> bool {
    complete_queue(queue)
        || (queue.explicit_complete == Some(true)
            && queue
                .loaded_range
                .as_ref()
                .is_none_or(|range| range.start == 0))
}

fn complete_queue(queue: &QueueSnapshot) -> bool {
    queue.completeness == QueueCompleteness::Complete
        && queue.has_more != Some(true)
        && queue
            .loaded_range
            .as_ref()
            .is_none_or(|range| range.start == 0)
}

impl PendingOpPlan {
    /// Output-gate transaction state for plans that must complete without audible playback.
    pub fn paused_skip(&self) -> Option<&PausedSkipState> {
        match self {
            Self::Next { paused_skip, .. }
            | Self::Previous { paused_skip, .. }
            | Self::PlayTarget { paused_skip, .. } => paused_skip.as_ref(),
            _ => None,
        }
    }

    pub fn paused_skip_mut(&mut self) -> Option<&mut PausedSkipState> {
        match self {
            Self::Next { paused_skip, .. }
            | Self::Previous { paused_skip, .. }
            | Self::PlayTarget { paused_skip, .. } => paused_skip.as_mut(),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PlaybackFence {
    pub account_scope: Option<AccountScope>,
    pub account_epoch: Option<AccountEpoch>,
    pub bridge_epoch: BridgeEpoch,
    pub generation: u64,
    pub track_instance_id: Option<TrackInstanceId>,
    pub queue_revision: Option<u64>,
    /// Deadline on the CoreEngine monotonic clock.
    pub deadline_ms: u64,
    pub cancel: CancellationToken,
}

#[derive(Debug, Clone)]
pub struct ActiveCommand {
    pub receipt: CommandReceipt,
    pub plan: PendingOpPlan,
    pub bound_bridge_epoch: BridgeEpoch,
    pub successor_handoff_used: bool,
    pub deadline_ms: u64,
    /// True once a frame for this command may have reached the page; cancelling it after that
    /// point cannot prove it had no effect, so it settles as `indeterminate` (see docs/architecture.md).
    pub dispatch_started: bool,
    pub playback_fence: Option<PlaybackFence>,
}

#[derive(Debug, Clone)]
pub struct QueuedCommand {
    pub receipt: CommandReceipt,
    pub payload: PlayerCommandPayload,
    /// Core-internal flag: complete a `playTarget` paused behind the output gate.
    pub keep_paused: bool,
    pub playback_fence: Option<PlaybackFence>,
    /// Set for a play accepted before the player knew what it could play: the command stays
    /// queued (`accepted`) without being sent until then, and fails as nothing to play at this
    /// deadline (core monotonic clock) unless a launch restore carries it.
    pub hold_deadline_ms: Option<u64>,
}

/// Position at or below which a same-instance observation counts as a YTM "restart" for
/// `previous`, and the minimum prior position for that restart to be meaningful.
pub const PREVIOUS_RESTART_MAX_POSITION_MS: u64 = SEEK_TOLERANCE_MS;
pub const PREVIOUS_RESTART_MIN_PRIOR_MS: u64 = 3_000;

#[derive(Debug, Clone)]
struct DedupRecord {
    payload: PlayerCommandPayload,
    receipt: CommandReceipt,
    updated_at_ms: u64,
    update_order: u64,
}

/// Bounded (256 entries / 10 min TTL) `commandId` deduplication cache scoped to one `CoreEpoch`.
#[derive(Debug, Default)]
pub struct CommandDeduplicator {
    entries: HashMap<CommandId, DedupRecord>,
    update_order: u64,
}

impl CommandDeduplicator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn purge_expired(&mut self, now_ms: u64) {
        self.entries.retain(|_, rec| {
            !is_terminal_status(rec.receipt.status)
                || now_ms.saturating_sub(rec.updated_at_ms) < DEDUP_TTL_MS
        });
    }

    pub fn get(&mut self, command_id: &CommandId, now_ms: u64) -> Option<CommandReceipt> {
        self.purge_expired(now_ms);
        self.entries.get(command_id).map(|rec| {
            let mut receipt = rec.receipt.clone();
            receipt.deduplicated = true;
            receipt
        })
    }

    pub fn check(
        &mut self,
        payload: &PlayerCommandPayload,
        now_ms: u64,
    ) -> Result<Option<CommandReceipt>, QueryError> {
        self.purge_expired(now_ms);
        let Some(current) = self.entries.get(&payload.command_id) else {
            return Ok(None);
        };
        if &current.payload != payload {
            return Err(QueryError {
                code: QueryErrorCode::InvalidPayload,
                retriable: false,
                retry_after_ms: None,
                safe_message: "commandId is already bound to a different playback command".into(),
                request_id: None,
            });
        }
        let mut receipt = current.receipt.clone();
        receipt.deduplicated = true;
        Ok(Some(receipt))
    }

    pub fn record(&mut self, payload: &PlayerCommandPayload, receipt: CommandReceipt, now_ms: u64) {
        if self.entries.len() >= MAX_DEDUP_ENTRIES {
            // The actor's pending queue is smaller than this cache. Unfinished commands
            // must retain their original payload even when recent history rolls over.
            let oldest_terminal = self
                .entries
                .values()
                .filter(|rec| is_terminal_status(rec.receipt.status))
                .min_by_key(|rec| rec.update_order)
                .map(|rec| rec.receipt.command_id.clone());
            if let Some(id) = oldest_terminal {
                self.entries.remove(&id);
            }
        }
        self.update_order += 1;
        self.entries.insert(
            receipt.command_id.clone(),
            DedupRecord {
                payload: payload.clone(),
                receipt,
                updated_at_ms: now_ms,
                update_order: self.update_order,
            },
        );
    }

    pub fn upsert(&mut self, receipt: CommandReceipt, now_ms: u64) {
        if let Some(existing) = self.entries.get_mut(&receipt.command_id) {
            self.update_order += 1;
            existing.receipt = receipt;
            existing.updated_at_ms = now_ms;
            existing.update_order = self.update_order;
        }
    }

    /// Receipts updated within `max_age_ms`, oldest first, capped at `max` (newest kept).
    pub fn recent(&self, now_ms: u64, max_age_ms: u64, max: usize) -> Vec<CommandReceipt> {
        let mut out: Vec<(u64, CommandReceipt)> = self
            .entries
            .values()
            .filter(|rec| now_ms.saturating_sub(rec.updated_at_ms) <= max_age_ms)
            .map(|rec| (rec.update_order, rec.receipt.clone()))
            .collect();
        out.sort_by_key(|(at, _)| *at);
        let skip = out.len().saturating_sub(max);
        out.into_iter().skip(skip).map(|(_, r)| r).collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

pub fn is_non_idempotent(op: PlayerOp) -> bool {
    matches!(
        op,
        PlayerOp::Toggle
            | PlayerOp::Next
            | PlayerOp::Previous
            | PlayerOp::Enqueue
            | PlayerOp::PlayTarget
            | PlayerOp::PlayCollection
            | PlayerOp::SelectQueueEntry
            | PlayerOp::RemoveQueueEntry
            | PlayerOp::ClearUpcoming
            | PlayerOp::MoveQueueEntry
    )
}

pub fn is_coalescible(op: PlayerOp) -> bool {
    matches!(op, PlayerOp::Seek | PlayerOp::SetVolume)
}

/// Resolves `Toggle` against the latest authoritative `observed` playback state.
pub fn resolve_toggle_op(observed_playback: PlaybackStatus) -> PlayerOp {
    match observed_playback {
        PlaybackStatus::Playing | PlaybackStatus::Buffering => PlayerOp::Pause,
        PlaybackStatus::Paused
        | PlaybackStatus::Idle
        | PlaybackStatus::Ended
        | PlaybackStatus::Error => PlayerOp::Play,
    }
}

pub fn validate_command_payload(payload: &PlayerCommandPayload) -> Result<(), QueryError> {
    let invalid = |msg: &str| QueryError {
        code: QueryErrorCode::InvalidPayload,
        retriable: false,
        retry_after_ms: None,
        safe_message: msg.to_string(),
        request_id: None,
    };

    match payload.op {
        PlayerOp::Seek => {
            let Some(args) = &payload.args else {
                return Err(invalid("seek requires args with positionMs"));
            };
            if args.position_ms.is_none() {
                return Err(invalid("seek requires positionMs"));
            }
        }
        PlayerOp::SetVolume => {
            let Some(args) = &payload.args else {
                return Err(invalid("setVolume requires args with volume"));
            };
            let Some(vol) = args.volume else {
                return Err(invalid("setVolume requires volume"));
            };
            if !vol.is_finite() || !(0.0..=1.0).contains(&vol) {
                return Err(invalid("volume must be finite and within [0.0, 1.0]"));
            }
        }
        PlayerOp::SetMuted => {
            let Some(args) = &payload.args else {
                return Err(invalid("setMuted requires args with muted"));
            };
            if args.muted.is_none() {
                return Err(invalid("setMuted requires muted"));
            }
        }
        PlayerOp::SetShuffle => {
            let Some(args) = &payload.args else {
                return Err(invalid("setShuffle requires args with shuffle"));
            };
            if args.shuffle.is_none() {
                return Err(invalid("setShuffle requires shuffle"));
            }
        }
        PlayerOp::SetRepeat => {
            let Some(args) = &payload.args else {
                return Err(invalid("setRepeat requires args with repeat"));
            };
            if args.repeat.is_none() {
                return Err(invalid("setRepeat requires repeat"));
            }
        }
        PlayerOp::SetAutoplay => {
            if !payload
                .args
                .as_ref()
                .is_some_and(|args| args.autoplay.is_some())
            {
                return Err(invalid("setAutoplay requires a boolean autoplay value"));
            }
        }
        PlayerOp::SetQueueFilter => {
            let id = payload
                .args
                .as_ref()
                .and_then(|args| args.queue_filter_id.as_deref());
            if !id.is_some_and(|id| !id.is_empty() && id.len() <= 64) {
                return Err(invalid("setQueueFilter requires a queueFilterId"));
            }
        }
        PlayerOp::PlayTarget | PlayerOp::PlayCollection => {
            let Some(target) = payload.args.as_ref().and_then(|args| args.target.as_ref()) else {
                return Err(invalid("playback requires a target"));
            };
            if payload.op == PlayerOp::PlayCollection
                && (target.playlist_id.is_none() || target.start_index.is_none())
            {
                return Err(invalid(
                    "collection playback requires a playlist and start index",
                ));
            }
        }
        PlayerOp::Enqueue => {
            let Some(args) = &payload.args else {
                return Err(invalid(
                    "enqueue requires args with target and enqueuePosition",
                ));
            };
            if args.target.is_none() || args.enqueue_position.is_none() {
                return Err(invalid("enqueue requires target and enqueuePosition"));
            }
        }
        PlayerOp::SelectQueueEntry => {
            let Some(args) = &payload.args else {
                return Err(invalid("selectQueueEntry requires args with queueEntryId"));
            };
            if args.queue_entry_id.is_none() {
                return Err(invalid("selectQueueEntry requires queueEntryId"));
            }
        }
        PlayerOp::RemoveQueueEntry | PlayerOp::MoveQueueEntry | PlayerOp::ClearUpcoming => {
            let Some(args) = &payload.args else {
                return Err(invalid(
                    "queue edits require queueEntryId and queueRevision",
                ));
            };
            if args.queue_entry_id.is_none()
                || args.queue_revision.is_none()
                || (payload.op != PlayerOp::MoveQueueEntry && args.before_queue_entry_id.is_some())
                || (payload.op == PlayerOp::ClearUpcoming && args.track_instance_id.is_none())
            {
                return Err(invalid("queue edit arguments do not match the operation"));
            }
        }
        PlayerOp::Play
        | PlayerOp::Pause
        | PlayerOp::Toggle
        | PlayerOp::Stop
        | PlayerOp::Next
        | PlayerOp::Previous => {}
    }

    Ok(())
}

#[derive(Debug)]
pub enum PlanEvaluation {
    StillPending,
    AdvanceStopToSeek,
    AdvancePausedSkipToPause {
        new_track_instance_id: Option<TrackInstanceId>,
    },
    ReadyToComplete {
        sets_stopped_confirmed: bool,
        clears_stopped_confirmed: bool,
        ungate_transaction_id: Option<String>,
    },
    Failed(QueryError),
}

fn stale_error(msg: &str) -> QueryError {
    QueryError {
        code: QueryErrorCode::StaleContext,
        retriable: false,
        retry_after_ms: None,
        safe_message: msg.to_string(),
        request_id: None,
    }
}

fn track_instance_matches(
    expected: &Option<TrackInstanceId>,
    observed: &Option<TrackInstanceId>,
) -> bool {
    match expected {
        Some(exp) => observed.as_ref() == Some(exp),
        None => true,
    }
}

pub(crate) fn seek_within_tolerance(
    observed_pos_ms: u64,
    target_pos_ms: u64,
    playback: PlaybackStatus,
) -> bool {
    let lower = target_pos_ms.saturating_sub(SEEK_TOLERANCE_MS);
    let upper = match playback {
        PlaybackStatus::Playing => target_pos_ms.saturating_add(SEEK_PLAYING_FORWARD_DRIFT_MS),
        _ => target_pos_ms.saturating_add(SEEK_TOLERANCE_MS),
    };
    observed_pos_ms >= lower && observed_pos_ms <= upper
}

fn has_track_or_entry_changed(
    prev_track_instance_id: &Option<TrackInstanceId>,
    prev_queue_entry_id: &Option<QueueEntryId>,
    observed: &PlayerObservedState,
) -> bool {
    let instance_changed = match (&observed.track_instance_id, prev_track_instance_id) {
        (Some(now), Some(prev)) => now != prev,
        (Some(_), None) => true,
        _ => false,
    };
    let entry_changed = match (&observed.current_queue_entry_id, prev_queue_entry_id) {
        (Some(now), Some(prev)) => now != prev,
        _ => false,
    };
    instance_changed || entry_changed
}

fn is_paused_like(playback: PlaybackStatus) -> bool {
    matches!(
        playback,
        PlaybackStatus::Paused | PlaybackStatus::Idle | PlaybackStatus::Ended
    )
}

/// Shared completion rule for track transitions (next/previous/playTarget). Without an output
/// gate the transition completes once `reached` (optionally also requiring real playback).
/// With a gate it must settle paused on the new track before the gate is released; a transient
/// `playing` observation is never reported as success.
fn evaluate_transition(
    reached: bool,
    paused_skip: Option<&PausedSkipState>,
    observed: &PlayerObservedState,
    require_playing: bool,
) -> PlanEvaluation {
    let complete = |ungate: Option<String>| PlanEvaluation::ReadyToComplete {
        sets_stopped_confirmed: false,
        clears_stopped_confirmed: true,
        ungate_transaction_id: ungate,
    };
    let Some(skip) = paused_skip else {
        let playing_ok = !require_playing || observed.playback == PlaybackStatus::Playing;
        return if reached && playing_ok {
            complete(None)
        } else {
            PlanEvaluation::StillPending
        };
    };
    match &skip.phase {
        PausedSkipPhase::AwaitingTrackChange => {
            if !reached {
                PlanEvaluation::StillPending
            } else if is_paused_like(observed.playback) {
                complete(Some(skip.transaction_id.clone()))
            } else {
                PlanEvaluation::AdvancePausedSkipToPause {
                    new_track_instance_id: observed.track_instance_id.clone(),
                }
            }
        }
        PausedSkipPhase::AwaitingRestoreSeek { .. } => PlanEvaluation::StillPending,
        PausedSkipPhase::AwaitingPausedConfirmation {
            new_track_instance_id,
        } => {
            if track_instance_matches(new_track_instance_id, &observed.track_instance_id)
                && is_paused_like(observed.playback)
            {
                complete(Some(skip.transaction_id.clone()))
            } else {
                PlanEvaluation::StillPending
            }
        }
        PausedSkipPhase::ReadyToUngate => complete(Some(skip.transaction_id.clone())),
    }
}

pub(crate) fn collection_context_matches(
    target: &PlaybackRef,
    observed: &PlayerObservedState,
) -> bool {
    observed.playback_context.as_ref().is_some_and(|context| {
        observed.track_instance_id.is_some()
            && target.playlist_id.as_ref() == Some(&context.playlist_id)
            && target.start_index == Some(context.start_index)
            && target
                .playlist_entry_id
                .as_ref()
                .is_none_or(|id| context.playlist_entry_id.as_ref() == Some(id))
    })
}

pub fn evaluate_active_command(
    plan: &PendingOpPlan,
    observed: &PlayerObservedState,
    _queue_summary: &QueueSummary,
    full_queue: Option<&QueueSnapshot>,
    estimated_pos_ms: u64,
) -> PlanEvaluation {
    match plan {
        PendingOpPlan::Play {
            expected_track_instance_id,
        } => {
            if !track_instance_matches(expected_track_instance_id, &observed.track_instance_id) {
                return PlanEvaluation::Failed(stale_error(
                    "trackInstanceId changed before play completed",
                ));
            }
            if observed.playback == PlaybackStatus::Playing {
                PlanEvaluation::ReadyToComplete {
                    sets_stopped_confirmed: false,
                    clears_stopped_confirmed: true,
                    ungate_transaction_id: None,
                }
            } else {
                PlanEvaluation::StillPending
            }
        }
        PendingOpPlan::Pause {
            expected_track_instance_id,
        } => {
            if !track_instance_matches(expected_track_instance_id, &observed.track_instance_id) {
                return PlanEvaluation::Failed(stale_error(
                    "trackInstanceId changed before pause completed",
                ));
            }
            if matches!(
                observed.playback,
                PlaybackStatus::Paused | PlaybackStatus::Idle | PlaybackStatus::Ended
            ) {
                PlanEvaluation::ReadyToComplete {
                    sets_stopped_confirmed: false,
                    clears_stopped_confirmed: false,
                    ungate_transaction_id: None,
                }
            } else {
                PlanEvaluation::StillPending
            }
        }
        PendingOpPlan::Stop {
            bound_track_instance_id,
            phase,
        } => {
            if !track_instance_matches(bound_track_instance_id, &observed.track_instance_id) {
                return PlanEvaluation::Failed(stale_error(
                    "trackInstanceId changed during stop transaction",
                ));
            }
            match phase {
                StopPhase::PausePending => {
                    if matches!(
                        observed.playback,
                        PlaybackStatus::Paused | PlaybackStatus::Idle | PlaybackStatus::Ended
                    ) {
                        PlanEvaluation::AdvanceStopToSeek
                    } else {
                        PlanEvaluation::StillPending
                    }
                }
                StopPhase::SeekPending => {
                    if matches!(
                        observed.playback,
                        PlaybackStatus::Paused | PlaybackStatus::Idle | PlaybackStatus::Ended
                    ) && estimated_pos_ms <= SEEK_TOLERANCE_MS
                    {
                        PlanEvaluation::ReadyToComplete {
                            sets_stopped_confirmed: true,
                            clears_stopped_confirmed: false,
                            ungate_transaction_id: None,
                        }
                    } else {
                        PlanEvaluation::StillPending
                    }
                }
            }
        }
        PendingOpPlan::Seek {
            bound_track_instance_id,
            position_ms,
        } => {
            if !track_instance_matches(bound_track_instance_id, &observed.track_instance_id) {
                return PlanEvaluation::Failed(stale_error(
                    "trackInstanceId changed; rejecting stale seek",
                ));
            }
            let clamped = match observed.time_anchor.duration_ms {
                Some(dur) => (*position_ms).min(dur),
                None => *position_ms,
            };
            if seek_within_tolerance(estimated_pos_ms, clamped, observed.playback) {
                PlanEvaluation::ReadyToComplete {
                    sets_stopped_confirmed: false,
                    clears_stopped_confirmed: false,
                    ungate_transaction_id: None,
                }
            } else {
                PlanEvaluation::StillPending
            }
        }
        PendingOpPlan::SetVolume { volume } => {
            if volume_matches(observed.volume, *volume) {
                PlanEvaluation::ReadyToComplete {
                    sets_stopped_confirmed: false,
                    clears_stopped_confirmed: false,
                    ungate_transaction_id: None,
                }
            } else {
                PlanEvaluation::StillPending
            }
        }
        PendingOpPlan::SetMuted { muted } => {
            if observed.muted == *muted {
                PlanEvaluation::ReadyToComplete {
                    sets_stopped_confirmed: false,
                    clears_stopped_confirmed: false,
                    ungate_transaction_id: None,
                }
            } else {
                PlanEvaluation::StillPending
            }
        }
        PendingOpPlan::SetShuffle { shuffle } => {
            if observed.shuffle == Some(*shuffle) {
                PlanEvaluation::ReadyToComplete {
                    sets_stopped_confirmed: false,
                    clears_stopped_confirmed: false,
                    ungate_transaction_id: None,
                }
            } else {
                PlanEvaluation::StillPending
            }
        }
        PendingOpPlan::SetRepeat { repeat } => {
            if observed.repeat == Some(*repeat) {
                PlanEvaluation::ReadyToComplete {
                    sets_stopped_confirmed: false,
                    clears_stopped_confirmed: false,
                    ungate_transaction_id: None,
                }
            } else {
                PlanEvaluation::StillPending
            }
        }
        PendingOpPlan::SetAutoplay { enabled } => {
            if observed.autoplay == Some(*enabled) {
                PlanEvaluation::ReadyToComplete {
                    sets_stopped_confirmed: false,
                    clears_stopped_confirmed: false,
                    ungate_transaction_id: None,
                }
            } else {
                PlanEvaluation::StillPending
            }
        }
        PendingOpPlan::SetQueueFilter { filter_id } => {
            let selected = full_queue
                .and_then(|queue| queue.filters.as_ref())
                .is_some_and(|filters| {
                    filters
                        .iter()
                        .any(|filter| filter.selected && &filter.filter_id == filter_id)
                });
            if selected {
                PlanEvaluation::ReadyToComplete {
                    sets_stopped_confirmed: false,
                    clears_stopped_confirmed: false,
                    ungate_transaction_id: None,
                }
            } else {
                PlanEvaluation::StillPending
            }
        }
        PendingOpPlan::Next {
            prev_track_instance_id,
            prev_queue_entry_id,
            paused_skip,
        } => {
            let changed =
                has_track_or_entry_changed(prev_track_instance_id, prev_queue_entry_id, observed);
            evaluate_transition(changed, paused_skip.as_ref(), observed, false)
        }
        PendingOpPlan::Previous {
            prev_track_instance_id,
            prev_queue_entry_id,
            prev_position_ms,
            paused_skip,
        } => {
            let restarted = *prev_position_ms >= PREVIOUS_RESTART_MIN_PRIOR_MS
                && estimated_pos_ms <= PREVIOUS_RESTART_MAX_POSITION_MS
                && observed.track_instance_id == *prev_track_instance_id;
            let changed = restarted
                || has_track_or_entry_changed(
                    prev_track_instance_id,
                    prev_queue_entry_id,
                    observed,
                );
            evaluate_transition(changed, paused_skip.as_ref(), observed, false)
        }
        PendingOpPlan::PlayTarget {
            target,
            require_context,
            prev_track_instance_id,
            paused_skip,
            restore_position_ms,
        } => {
            let track_matches = observed
                .current_track
                .as_ref()
                .is_some_and(|t| t.video_id == target.video_id);
            let entry_matches = match &target.queue_entry_id {
                Some(qe) => observed.current_queue_entry_id.as_ref() == Some(qe),
                None => true,
            };
            let instance_ok = match (&observed.track_instance_id, prev_track_instance_id) {
                (Some(now), Some(prev)) => {
                    now != prev || observed.playback == PlaybackStatus::Playing
                }
                _ => true,
            };
            let context_matches = !require_context || collection_context_matches(target, observed);
            let reached = track_matches && entry_matches && instance_ok && context_matches;
            // An identified track may be reported before its media is playable. Let
            // navigation finish starting the media before a restore seek or pause.
            if restore_position_ms.is_some()
                && paused_skip
                    .as_ref()
                    .is_some_and(|skip| skip.phase == PausedSkipPhase::AwaitingTrackChange)
                && observed.playback != PlaybackStatus::Playing
            {
                return PlanEvaluation::StillPending;
            }
            evaluate_transition(reached, paused_skip.as_ref(), observed, true)
        }
        PendingOpPlan::Enqueue(proof) => {
            if proof
                .as_ref()
                .zip(full_queue)
                .and_then(|(plan, queue)| plan.observed_entry(queue, observed))
                .is_some()
            {
                PlanEvaluation::ReadyToComplete {
                    sets_stopped_confirmed: false,
                    clears_stopped_confirmed: false,
                    ungate_transaction_id: None,
                }
            } else {
                PlanEvaluation::StillPending
            }
        }
        PendingOpPlan::SelectQueueEntry { entry_id, video_id } => {
            // The selected row alone does not prove playback: it must be playing that entry's video.
            if observed.current_queue_entry_id.as_ref() == Some(entry_id)
                && video_id.is_some()
                && observed.current_track.as_ref().map(|t| &t.video_id) == video_id.as_ref()
            {
                PlanEvaluation::ReadyToComplete {
                    sets_stopped_confirmed: false,
                    clears_stopped_confirmed: true,
                    ungate_transaction_id: None,
                }
            } else {
                PlanEvaluation::StillPending
            }
        }
        PendingOpPlan::EditQueue(proof) => {
            if full_queue.is_some_and(|queue| proof.observed(queue, observed)) {
                PlanEvaluation::ReadyToComplete {
                    sets_stopped_confirmed: false,
                    clears_stopped_confirmed: false,
                    ungate_transaction_id: None,
                }
            } else {
                PlanEvaluation::StillPending
            }
        }
    }
}

pub fn is_terminal_status(status: CommandLifecycleStatus) -> bool {
    matches!(
        status,
        CommandLifecycleStatus::Observed
            | CommandLifecycleStatus::Failed
            | CommandLifecycleStatus::Indeterminate
            | CommandLifecycleStatus::Cancelled
    )
}
