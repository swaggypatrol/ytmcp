//! Prompts for a DJ agent. Before each track ends the agent gets one "continue" prompt; between
//! them a check-in heartbeat accumulates points while music plays and fires with a probability
//! proportional to those points (a DotA-style pseudo-random distribution), so the longer nobody
//! has asked, the likelier a question becomes. A listener's action or an agent's words reset it.
//! Check-ins nobody answers stretch the gaps (doubling up to eightfold) and are held to three
//! per hour until the listener acts or speaks again.
//!
//! Prompts stay small: tracks carry only the facts a DJ needs, and the recorded tastes come with
//! a client's first prompt and again only after they change.
//!
//! The DJ is on while an agent waits: with no wait call for a few minutes nothing accumulates
//! and no prompt is made. The agent decides what to do with a prompt, including nothing.
//!
//! Every prompt lists what the listener did by hand since that client's previous prompt. A few
//! of those actions say where the music should go (skipping track after track, picking
//! something, going back to hear a track again, choosing another Up Next filter); a few seconds
//! after such a burst the agent gets a listener-signal prompt, at most one every two minutes.

use std::collections::{HashMap, VecDeque};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ytm_control_api::{
    ControlError, DjChattiness, DjCue, DjCueInput, DjCueKind, DjCueReason, DjCueReply,
    DjInteraction, DjListener, DjNowPlaying, DjOfficialUpNext, DjPreference, DjRecentTrack,
    DjTrack, DjTrackOutcome, Track, UpcomingStatus,
};
use ytm_protocol::{AccountScope, OfficialUpcomingStatus, PlaybackStatus, TrackInstanceId};

use super::listener_log::{self, LoggedAction};
use super::state::track_projection;
use super::{ControlService, ReadGrant, unavailable};
use crate::CoreEngine;
use crate::engine::ControlRead;
use crate::queue_window::{Upcoming, upcoming};

const SAMPLE_MS: u64 = 1_000;
pub(crate) const TICK_MS: u64 = 15_000;
/// The DJ counts as on while a wait call was seen this recently.
const ACTIVE_MS: u64 = 180_000;
const CHECK_IN_TTL_MS: u64 = 180_000;
/// No check-in this soon after a track change.
const QUIET_AFTER_CHANGE_MS: u64 = 60_000;
const LEAD_MS: u64 = 45_000;
/// Lead when at most one track is queued, so the agent has time to find and queue more.
const LOW_QUEUE_LEAD_MS: u64 = 75_000;
const LONG_TRACK_MS: u64 = 7 * 60_000;
const QUIET_LISTENER_MS: u64 = 12 * 60_000;
const SILENT_LISTENER_MS: u64 = 25 * 60_000;
/// A track that stopped within this much of its end counts as finished.
const FINISH_SLACK_MS: u64 = 20_000;
/// Position changes beyond the time played by more than this are seeks, not listening.
const SEEK_SLACK_MS: u64 = 3_000;
/// A command this close before a track change is taken as its cause.
const ATTRIBUTION_MS: u64 = 8_000;
const RECENT_LIMIT: usize = 8;
const PENDING_LIMIT: usize = 8;
const UP_NEXT_LIMIT: usize = 5;
/// Official suggestions named in a prompt's text.
const PROMPT_OFFICIAL_LIMIT: usize = 3;
/// Unselected Up Next filters a prompt offers for steering those suggestions.
const PROMPT_FILTER_LIMIT: usize = 4;
/// Each unanswered check-in doubles the minimum and maximum gaps, at most this many times (8x).
const UNANSWERED_DOUBLINGS: u32 = 3;
/// At most this many check-ins within `CHECK_IN_WINDOW_MS` while nobody answers.
const UNANSWERED_CHECK_IN_LIMIT: usize = 3;
const CHECK_IN_WINDOW_MS: u64 = 60 * 60_000;
/// A listener signal waits this long after the listener's last action, so a burst is one prompt.
const SIGNAL_SETTLE_MS: u64 = 5_000;
/// At most one listener signal this often.
const SIGNAL_GAP_MS: u64 = 120_000;
/// Listener skips this close together count as a streak.
const SKIP_STREAK_MS: u64 = 180_000;
/// Going back after this much of a track is hearing it again.
const REPLAY_HEARD_MS: u64 = 10_000;
/// Going back this early in a track goes to the previous one rather than restarting it.
const PREVIOUS_TRACK_MS: u64 = 3_000;
/// Actions a listener-signal prompt describes in words; the rest are only listed.
const SIGNAL_NOTE_LIMIT: usize = 3;
/// A signal prompt names at most this many of the tracks skipped in a row.
const SKIPS_NAMED: usize = 3;
/// A signal this soon after its first trigger says the listener "just" did it.
const SIGNAL_JUST_MS: u64 = 15_000;
/// Prompts per client whose action marks are kept for a lost reply.
const HANDOUT_LIMIT: usize = 16;

struct Pace {
    coefficient: f64,
    floor_ms: u64,
    cap_ms: u64,
}

fn pace(level: DjChattiness) -> Option<Pace> {
    let (coefficient, floor_ms, cap_ms) = match level {
        DjChattiness::Off => return None,
        DjChattiness::Quiet => (0.0006, 8 * 60_000, 25 * 60_000),
        DjChattiness::Normal => (0.0016, 4 * 60_000, 15 * 60_000),
        DjChattiness::Chatty => (0.004, 150_000, 9 * 60_000),
    };
    Some(Pace {
        coefficient,
        floor_ms,
        cap_ms,
    })
}

/// What the heartbeat needs to know for each tick.
pub(crate) struct Beat {
    pub playing: bool,
    /// Right after a track change or inside a track-ending window.
    pub suppressed: bool,
    /// Since the listener last acted or the DJ started, whichever is later.
    pub idle_ms: u64,
    pub long_track: bool,
    /// Multiplies the minimum and maximum gaps.
    pub stretch: u64,
}

/// Points-based check-in heartbeat. Points and the minimum and maximum gaps count playing time
/// only, so a long pause neither triggers nor counts toward a question.
#[derive(Default)]
pub(crate) struct Heartbeat {
    points: f64,
    played_ms: u64,
    next_tick_ms: Option<u64>,
}

impl Heartbeat {
    pub(crate) fn reset(&mut self) {
        self.points = 0.0;
        self.played_ms = 0;
    }

    /// Stops the tick clock while the DJ is off, so turning it back on does not catch up.
    pub(crate) fn hold(&mut self) {
        self.next_tick_ms = None;
    }

    /// Runs every whole tick up to `now_ms` and returns the reasons of a check-in if one fired.
    pub(crate) fn advance(
        &mut self,
        now_ms: u64,
        beat: &Beat,
        level: DjChattiness,
        roll: &mut impl FnMut() -> f64,
    ) -> Option<Vec<DjCueReason>> {
        let mut next = self.next_tick_ms.unwrap_or(now_ms + TICK_MS);
        let mut fired = None;
        while next <= now_ms {
            next += TICK_MS;
            if let Some(reasons) = self.tick(beat, level, roll) {
                fired = Some(reasons);
            }
        }
        self.next_tick_ms = Some(next);
        fired
    }

    fn tick(
        &mut self,
        beat: &Beat,
        level: DjChattiness,
        roll: &mut impl FnMut() -> f64,
    ) -> Option<Vec<DjCueReason>> {
        if !beat.playing {
            return None;
        }
        self.played_ms += TICK_MS;
        let mut weight = if beat.idle_ms >= SILENT_LISTENER_MS {
            2.0
        } else if beat.idle_ms >= QUIET_LISTENER_MS {
            1.5
        } else {
            1.0
        };
        if beat.long_track {
            weight += 0.5;
        }
        self.points += weight;
        let pace = pace(level)?;
        if beat.suppressed || self.played_ms < pace.floor_ms * beat.stretch {
            return None;
        }
        if self.played_ms < pace.cap_ms * beat.stretch && roll() >= pace.coefficient * self.points {
            return None;
        }
        self.reset();
        let mut reasons = Vec::new();
        if beat.idle_ms >= QUIET_LISTENER_MS {
            reasons.push(DjCueReason::ListenerQuiet);
        }
        if beat.long_track {
            reasons.push(DjCueReason::LongTrack);
        }
        if reasons.is_empty() {
            reasons.push(DjCueReason::Routine);
        }
        Some(reasons)
    }
}

/// Whether at most one track is known to follow. Unknown is not little: it gets the normal
/// lead and no low-queue reason.
fn little_queued(queued_after: Option<usize>) -> bool {
    queued_after.is_some_and(|count| count <= 1)
}

/// How long before the end of a track its prompt is due.
pub(crate) fn ending_lead_ms(duration_ms: u64, queued_after: Option<usize>) -> u64 {
    let lead = if little_queued(queued_after) {
        LOW_QUEUE_LEAD_MS
    } else {
        LEAD_MS
    };
    lead.min(duration_ms / 3)
}

/// From the ending prompt's lead until the track is over, including its very end.
fn near_end(duration_ms: Option<u64>, position_ms: u64, queued_after: Option<usize>) -> bool {
    duration_ms.is_some_and(|duration| {
        position_ms.saturating_add(ending_lead_ms(duration, queued_after)) >= duration
    })
}

fn in_ending_window(
    duration_ms: Option<u64>,
    position_ms: u64,
    queued_after: Option<usize>,
) -> bool {
    near_end(duration_ms, position_ms, queued_after)
        && duration_ms.is_some_and(|duration| position_ms < duration)
}

/// The part of a control read the cues use.
pub(crate) struct Sample {
    pub scope: Option<AccountScope>,
    pub playback: PlaybackStatus,
    pub instance: Option<TrackInstanceId>,
    pub track: Option<Track>,
    pub position_ms: u64,
    pub duration_ms: Option<u64>,
    /// Tracks queued after the current occurrence; `None` when the current occurrence is not
    /// in the loaded window, so nothing is known about what follows.
    pub queued_after: Option<usize>,
    pub up_next: Vec<Track>,
    pub more_queued: bool,
    /// YouTube Music's own continuation after the explicit list; `None` when not readable.
    pub official: Option<DjOfficialUpNext>,
    /// Labels of the Up Next filters offered but not selected, at most [`PROMPT_FILTER_LIMIT`].
    pub filter_choices: Vec<String>,
    pub last_manual_ms: Option<u64>,
    /// What the listener did by hand, oldest first.
    pub actions: Arc<[LoggedAction]>,
}

impl Sample {
    fn from_read(read: &ControlRead) -> Self {
        let player = &read.snapshot.player;
        let track = player.current_track.as_ref().map(track_projection);
        let (mut after, mut has_more, mut known) = (&[][..], false, false);
        if let Some(queue) = read.queue.as_ref()
            && let Upcoming::Known { after: loaded, .. } = upcoming(queue)
        {
            (after, known) = (loaded, true);
            // An autoplay tail after a fully loaded list is not queued music: YouTube Music only
            // adds suggestions there, which the prompt mentions separately.
            has_more = queue.has_more == Some(true) && queue.explicit_complete != Some(true);
        }
        let official = read.queue.as_ref().and_then(|queue| {
            let official = queue.official_upcoming.as_ref()?;
            Some(DjOfficialUpNext {
                status: official.status,
                tracks: official
                    .tracks
                    .iter()
                    .take(UP_NEXT_LIMIT)
                    .map(|track| DjTrack::from(&track_projection(track)))
                    .collect(),
                more: official.tracks.len() > UP_NEXT_LIMIT,
                filter: queue
                    .filters
                    .iter()
                    .flatten()
                    .find(|filter| filter.selected)
                    .map(|filter| filter.label.clone()),
            })
        });
        let filter_choices = read
            .queue
            .as_ref()
            .and_then(|queue| queue.filters.as_ref())
            .into_iter()
            .flatten()
            .filter(|filter| !filter.selected)
            .take(PROMPT_FILTER_LIMIT)
            .map(|filter| filter.label.clone())
            .collect();
        let duration_ms = player
            .time_anchor
            .duration_ms
            .or_else(|| track.as_ref().and_then(|track| track.duration_ms))
            .filter(|duration| *duration > 0);
        Self {
            scope: read.snapshot.auth_state.account_scope.clone(),
            playback: player.playback,
            instance: player.track_instance_id.clone(),
            position_ms: player.time_anchor.position_ms,
            duration_ms,
            queued_after: known.then(|| {
                if has_more {
                    after.len().max(2)
                } else {
                    after.len()
                }
            }),
            more_queued: has_more || after.len() > UP_NEXT_LIMIT,
            up_next: after
                .iter()
                .take(UP_NEXT_LIMIT)
                .map(|entry| track_projection(&entry.track))
                .collect(),
            official,
            filter_choices,
            track,
            last_manual_ms: read.last_manual_command_ms,
            actions: read.listener_actions.clone(),
        }
    }
}

struct Watch {
    instance: TrackInstanceId,
    track: Track,
    duration_ms: Option<u64>,
    /// Forward progress made by playing, without seeks.
    listened_ms: u64,
    position_ms: u64,
    sampled_ms: u64,
    playing: bool,
    /// The latest seek landed in the track's last stretch.
    jumped_into_end: bool,
    /// Playing (not a seek) reached the last stretch at some point.
    played_into_end: bool,
}

impl Watch {
    fn start(sample: &Sample, now_ms: u64) -> Option<Self> {
        let (instance, track) = sample.instance.clone().zip(sample.track.clone())?;
        Some(Self {
            instance,
            track,
            duration_ms: sample.duration_ms,
            listened_ms: 0,
            position_ms: sample.position_ms,
            sampled_ms: now_ms,
            playing: sample.playback == PlaybackStatus::Playing,
            jumped_into_end: false,
            played_into_end: false,
        })
    }

    fn in_end(&self, position_ms: u64) -> bool {
        self.duration_ms
            .is_some_and(|duration| position_ms.saturating_add(FINISH_SLACK_MS) >= duration)
    }

    /// Counts the progress since the previous sample as listening when playing could explain
    /// it, and anything else as a seek.
    fn advance(&mut self, sample: &Sample, now_ms: u64) {
        let played = if self.playing {
            now_ms.saturating_sub(self.sampled_ms)
        } else {
            0
        };
        self.duration_ms = self.duration_ms.or(sample.duration_ms);
        let position = sample.position_ms;
        let progress = position.checked_sub(self.position_ms);
        match progress {
            Some(progress) if progress <= played + SEEK_SLACK_MS => {
                self.listened_ms += progress;
                if !self.jumped_into_end && self.in_end(position) {
                    self.played_into_end = true;
                }
            }
            None if self.position_ms - position <= SEEK_SLACK_MS => {}
            _ => self.jumped_into_end = self.in_end(position),
        }
        self.position_ms = position;
        self.sampled_ms = now_ms;
        self.playing = sample.playback == PlaybackStatus::Playing;
    }

    /// Reached the last stretch by playing into it, whatever came after (a rewind, a reset
    /// position before the next track); `None` when the duration is unknown.
    fn played_to_end(&self) -> Option<bool> {
        self.duration_ms.map(|_| self.played_into_end)
    }
}

struct Ended {
    track: Track,
    listened_ms: u64,
    outcome: DjTrackOutcome,
    ended_ms: u64,
}

#[derive(Clone)]
pub(crate) struct Pending {
    pub seq: u64,
    pub kind: DjCueKind,
    pub reasons: Vec<DjCueReason>,
    pub fired_ms: u64,
    instance: Option<TrackInstanceId>,
    /// What the listener did, for a listener-signal prompt.
    note: Option<String>,
}

/// A listener signal waiting for its burst to settle, or for the gap since the previous one.
struct SignalDue {
    due_ms: u64,
    triggers: Vec<Trigger>,
}

/// What in a burst says where the music should go.
enum Trigger {
    /// The run of unfinished skips, as long as it has grown within the burst.
    Streak {
        at_ms: u64,
        left: Vec<Option<DjTrack>>,
    },
    /// A pick, a filter choice or repeat-one, worded from the action as it stands when the
    /// signal fires, when its subject is known.
    Action(Box<LoggedAction>),
    /// Going back to hear a track again, named when it happened.
    Replay { at_ms: u64, title: String },
}

impl Trigger {
    fn at_ms(&self) -> u64 {
        match self {
            Self::Streak { at_ms, .. } | Self::Replay { at_ms, .. } => *at_ms,
            Self::Action(action) => action.at_ms,
        }
    }

    /// The reason this trigger gives and what the listener did, in words.
    fn words(&self, actions: &[LoggedAction]) -> (DjCueReason, String) {
        use ytm_control_api::ListenerActionKind as Kind;
        let action = match self {
            Self::Streak { left, .. } => {
                let names: Vec<_> = left.iter().flatten().map(track_title).collect();
                let shown = &names[names.len().saturating_sub(SKIPS_NAMED)..];
                let named = match names.len() - shown.len() {
                    _ if shown.is_empty() => String::new(),
                    0 => format!(" ({})", shown.join(", ")),
                    more => format!(" ({}, and {more} before them)", shown.join(", ")),
                };
                return (
                    DjCueReason::SkipStreak,
                    format!(
                        "skipped {} tracks in a row without finishing them{named}",
                        left.len()
                    ),
                );
            }
            Self::Replay { title, .. } => {
                return (
                    DjCueReason::Replay,
                    format!("went back to hear {title} again"),
                );
            }
            Self::Action(held) => actions
                .iter()
                .find(|action| action.id == held.id)
                .unwrap_or(held),
        };
        let subject = action.about().map(track_title);
        match action.kind {
            Kind::Repeat => (
                DjCueReason::Replay,
                "turned on repeat for the current track".to_owned(),
            ),
            Kind::UpNextFilter => (
                DjCueReason::FilterChanged,
                format!(
                    "chose the Up Next filter {}",
                    action.value.as_deref().unwrap_or("another one")
                ),
            ),
            Kind::PlayCollection => (
                DjCueReason::ManualPick,
                match subject {
                    Some(track) => format!("started an album or playlist from {track}"),
                    None => "started an album or playlist".to_owned(),
                },
            ),
            Kind::PickFromQueue => (
                DjCueReason::ManualPick,
                format!(
                    "jumped to {} in the queue",
                    subject.as_deref().unwrap_or("a track")
                ),
            ),
            _ => (
                DjCueReason::ManualPick,
                format!("started {}", subject.as_deref().unwrap_or("a track")),
            ),
        }
    }
}

/// The opening sentence of a listener-signal prompt; a signal held for the gap says how long
/// ago it began.
fn signal_sentence(age_ms: u64, what: &str) -> String {
    if age_ms <= SIGNAL_JUST_MS {
        return format!("By hand, the listener just {what}.");
    }
    let when = if age_ms < 60_000 {
        format!("{} seconds", age_ms.div_ceil(10_000) * 10)
    } else {
        match minutes(age_ms) {
            1 => "minute".to_owned(),
            count => format!("{count} minutes"),
        }
    };
    format!("In the last {when}, by hand, the listener {what}.")
}

pub(crate) struct CueState {
    /// The account whose listening this state describes.
    scope: Option<AccountScope>,
    observing: bool,
    started_ms: Option<u64>,
    last_wait_ms: Option<u64>,
    seq: u64,
    pending: VecDeque<Pending>,
    delivered: HashMap<String, u64>,
    heartbeat: Heartbeat,
    chattiness: DjChattiness,
    watch: Option<Watch>,
    ending_sent: Option<TrackInstanceId>,
    recent: VecDeque<Ended>,
    last_change_ms: Option<u64>,
    listener: Option<(DjInteraction, u64)>,
    spoke_ms: Option<u64>,
    /// Check-ins since the listener last acted. A check-in counts as soon as it fires, so the gap
    /// after an unanswered one is already longer; any listener action resets it.
    unanswered: Option<u32>,
    /// When check-ins fired since the listener last acted, within the last window.
    unanswered_fired: VecDeque<u64>,
    /// Per client, the revision of the tastes its previous prompt carried.
    preference_revisions: HashMap<String, u64>,
    agent_write_ms: Option<u64>,
    manual_seen_ms: Option<u64>,
    /// The newest listener action the signal detector has looked at.
    signal_scanned: u64,
    signal_due: Option<SignalDue>,
    last_signal_ms: Option<u64>,
    /// The listener's unfinished skips in a row: when, and the track left.
    skips: Vec<(u64, Option<DjTrack>)>,
    /// Per client, the newest listener action its prompts listed.
    action_marks: HashMap<String, u64>,
    /// Per client, each recent prompt's seq and the mark before it, so a prompt whose reply
    /// never arrived lists its actions again.
    handouts: HashMap<String, VecDeque<(u64, Option<u64>)>>,
    rng: u64,
}

impl CueState {
    pub(crate) fn new(seed: u64) -> Self {
        Self {
            scope: None,
            observing: false,
            started_ms: None,
            last_wait_ms: None,
            seq: 0,
            pending: VecDeque::new(),
            delivered: HashMap::new(),
            heartbeat: Heartbeat::default(),
            chattiness: DjChattiness::default(),
            watch: None,
            ending_sent: None,
            recent: VecDeque::new(),
            last_change_ms: None,
            listener: None,
            spoke_ms: None,
            unanswered: None,
            unanswered_fired: VecDeque::new(),
            preference_revisions: HashMap::new(),
            agent_write_ms: None,
            manual_seen_ms: None,
            signal_scanned: 0,
            signal_due: None,
            last_signal_ms: None,
            skips: Vec::new(),
            action_marks: HashMap::new(),
            handouts: HashMap::new(),
            rng: seed | 1,
        }
    }

    fn active(&self, now_ms: u64) -> bool {
        self.last_wait_ms
            .is_some_and(|at| now_ms.saturating_sub(at) <= ACTIVE_MS)
    }

    /// Starts the history over when the player shows another account (or none), so one
    /// account's tracks, prompts and listener activity never reach another's agent. The DJ's
    /// on state, its settings and the manual-command watermark are not account data and stay.
    pub(crate) fn bind(&mut self, scope: Option<&AccountScope>, now_ms: u64) {
        if self.scope.as_ref() == scope {
            return;
        }
        *self = Self {
            scope: scope.cloned(),
            observing: self.observing,
            started_ms: Some(now_ms),
            last_wait_ms: self.last_wait_ms,
            seq: self.seq,
            chattiness: self.chattiness,
            manual_seen_ms: self.manual_seen_ms,
            rng: self.rng,
            ..Self::new(self.rng)
        };
    }

    /// Records one wait call and what the agent reported with it.
    pub(crate) fn waited(&mut self, now_ms: u64, input: &DjCueInput) {
        if !self.active(now_ms) {
            // Nothing was sampled while the DJ was off, so the last watched track's end is
            // unknown; starting over keeps it out of `recent` and makes the DJ's start count as a
            // track change. The tick clock starts over too, or the missed ticks would catch up.
            self.watch = None;
            self.started_ms = Some(now_ms);
            self.heartbeat.reset();
            self.heartbeat.hold();
            // Likely a new conversation, which has not seen the tastes yet.
            self.preference_revisions.clear();
            // What the listener did while nobody waited makes no prompt.
            self.signal_due = None;
            self.skips.clear();
        }
        self.last_wait_ms = Some(now_ms);
        if let Some(level) = input.chattiness {
            self.chattiness = level;
        }
        if input.user_spoke {
            self.listener_acted(DjInteraction::Spoke, now_ms);
        }
        if input.spoke {
            self.spoke_ms = Some(now_ms);
            self.heartbeat.reset();
        }
    }

    pub(crate) fn agent_wrote(&mut self, now_ms: u64) {
        self.agent_write_ms = Some(now_ms);
    }

    fn listener_acted(&mut self, kind: DjInteraction, at_ms: u64) {
        self.listener = Some((kind, at_ms));
        self.heartbeat.reset();
        self.unanswered = None;
        self.unanswered_fired.clear();
    }

    /// Folds one playback sample into the history and fires any prompt that is due.
    pub(crate) fn observe(&mut self, now_ms: u64, sample: &Sample) {
        self.bind(sample.scope.as_ref(), now_ms);
        if let Some(at) = sample.last_manual_ms
            && self.manual_seen_ms.is_none_or(|seen| at > seen)
        {
            self.manual_seen_ms = Some(at);
            self.listener_acted(DjInteraction::PlayerControl, at);
        }
        // Before the track change is folded in, so `recent` still ends with the track a
        // "previous" went back to.
        self.scan_actions(now_ms, &sample.actions);
        if self.watch.as_ref().map(|watch| &watch.instance) != sample.instance.as_ref() {
            if let Some(previous) = self.watch.take() {
                self.finish(previous, now_ms);
            }
            self.watch = Watch::start(sample, now_ms);
            self.last_change_ms = Some(now_ms);
        } else if let Some(watch) = self.watch.as_mut() {
            watch.advance(sample, now_ms);
        }
        if !self.active(now_ms) {
            self.heartbeat.hold();
            return;
        }
        let playing = sample.playback == PlaybackStatus::Playing;
        let ending = in_ending_window(sample.duration_ms, sample.position_ms, sample.queued_after);
        if playing && ending && sample.instance.is_some() && self.ending_sent != sample.instance {
            self.ending_sent = sample.instance.clone();
            let reasons = if little_queued(sample.queued_after) {
                vec![DjCueReason::LittleQueued]
            } else {
                Vec::new()
            };
            self.push(
                DjCueKind::TrackEnding,
                reasons,
                now_ms,
                sample.instance.clone(),
            );
        }
        while self
            .unanswered_fired
            .front()
            .is_some_and(|at| now_ms.saturating_sub(*at) >= CHECK_IN_WINDOW_MS)
        {
            self.unanswered_fired.pop_front();
        }
        let held = self.unanswered_fired.len() >= UNANSWERED_CHECK_IN_LIMIT;
        let since = self
            .listener
            .map(|(_, at)| at)
            .max(self.started_ms)
            .unwrap_or(now_ms);
        let beat = Beat {
            playing,
            // Quiet through the end of a track and the start of the next one, and while the
            // unanswered check-ins of the last hour are used up.
            suppressed: held
                || near_end(sample.duration_ms, sample.position_ms, sample.queued_after)
                || sample.playback == PlaybackStatus::Ended
                || self
                    .last_change_ms
                    .is_some_and(|at| now_ms < at.saturating_add(QUIET_AFTER_CHANGE_MS)),
            idle_ms: now_ms.saturating_sub(since),
            long_track: sample
                .duration_ms
                .is_some_and(|duration| duration > LONG_TRACK_MS),
            stretch: 1 << self.unanswered_streak().min(UNANSWERED_DOUBLINGS),
        };
        let mut rng = self.rng;
        let fired = self
            .heartbeat
            .advance(now_ms, &beat, self.chattiness, &mut || unit(&mut rng));
        self.rng = rng;
        if let Some(reasons) = fired {
            self.unanswered = Some(self.unanswered.map_or(1, |streak| streak.saturating_add(1)));
            self.unanswered_fired.push_back(now_ms);
            self.push(DjCueKind::CheckIn, reasons, now_ms, None);
        }
    }

    fn unanswered_streak(&self) -> u32 {
        self.unanswered.unwrap_or(0)
    }

    /// The tastes to put in this client's prompt and whether they are left out as unchanged.
    /// `None` means they could not be read: nothing is sent, and a client that saw them before
    /// is told to keep what it has rather than that they were cleared.
    pub(crate) fn preferences_for(
        &mut self,
        principal: &str,
        loaded: Option<Vec<DjPreference>>,
    ) -> (Vec<DjPreference>, bool) {
        let Some(preferences) = loaded else {
            return (
                Vec::new(),
                self.preference_revisions.contains_key(principal),
            );
        };
        let mut hasher = DefaultHasher::new();
        for preference in &preferences {
            preference.preference_id.hash(&mut hasher);
            preference.recorded_at_ms.hash(&mut hasher);
        }
        let revision = hasher.finish();
        if self
            .preference_revisions
            .insert(principal.to_owned(), revision)
            == Some(revision)
        {
            (Vec::new(), true)
        } else {
            (preferences, false)
        }
    }

    /// A track finished when it was played into its last stretch; with no known duration, when
    /// no listener or agent command came just before it ended.
    fn finish(&mut self, watch: Watch, now_ms: u64) {
        let recent =
            |at: Option<u64>| at.is_some_and(|at| now_ms.saturating_sub(at) <= ATTRIBUTION_MS);
        let by_listener = recent(self.manual_seen_ms);
        let by_agent = recent(self.agent_write_ms);
        let finished = watch.played_to_end().unwrap_or(!by_listener && !by_agent);
        let outcome = if finished {
            DjTrackOutcome::Finished
        } else if by_listener {
            DjTrackOutcome::SkippedByListener
        } else if by_agent {
            DjTrackOutcome::SkippedByAgent
        } else {
            DjTrackOutcome::Skipped
        };
        if finished {
            self.skips.clear();
        }
        self.recent.push_front(Ended {
            track: watch.track,
            listened_ms: watch.listened_ms,
            outcome,
            ended_ms: now_ms,
        });
        self.recent.truncate(RECENT_LIMIT);
    }

    fn push(
        &mut self,
        kind: DjCueKind,
        reasons: Vec<DjCueReason>,
        now_ms: u64,
        instance: Option<TrackInstanceId>,
    ) {
        self.push_noted(kind, reasons, now_ms, instance, None);
    }

    fn push_noted(
        &mut self,
        kind: DjCueKind,
        reasons: Vec<DjCueReason>,
        now_ms: u64,
        instance: Option<TrackInstanceId>,
        note: Option<String>,
    ) {
        self.seq += 1;
        self.pending.push_back(Pending {
            seq: self.seq,
            kind,
            reasons,
            fired_ms: now_ms,
            instance,
            note,
        });
        while self.pending.len() > PENDING_LIMIT {
            self.pending.pop_front();
        }
    }

    /// The oldest prompt this client has not received that is still relevant. A track-ending
    /// prompt lapses once its track is no longer current, a check-in after a few minutes.
    /// `received` is the last seq the client says it got: a prompt handed out after it (to a
    /// wait whose reply never arrived) is offered again, with the tastes that reply may have
    /// carried. It never skips a prompt.
    pub(crate) fn take(
        &mut self,
        principal: &str,
        now_ms: u64,
        current: Option<&TrackInstanceId>,
        received: Option<u64>,
    ) -> Option<Pending> {
        self.pending.retain(|cue| match cue.kind {
            DjCueKind::TrackEnding => cue.instance.as_ref() == current,
            DjCueKind::CheckIn | DjCueKind::ListenerSignal => {
                now_ms.saturating_sub(cue.fired_ms) < CHECK_IN_TTL_MS
            }
        });
        let delivered = self.delivered.get(principal).copied().unwrap_or(0);
        let after = match received {
            Some(received) if received < delivered => {
                self.preference_revisions.remove(principal);
                self.rewind_actions(principal, received);
                received
            }
            _ => delivered,
        };
        let cue = self.pending.iter().find(|cue| cue.seq > after).cloned()?;
        self.delivered.insert(principal.to_owned(), cue.seq);
        Some(cue)
    }

    fn recent_tracks(&self, now_ms: u64) -> Vec<DjRecentTrack> {
        self.recent
            .iter()
            .map(|ended| DjRecentTrack {
                track: DjTrack::from(&ended.track),
                listened_ms: ended.listened_ms,
                outcome: ended.outcome,
                ended_ago_ms: now_ms.saturating_sub(ended.ended_ms),
            })
            .collect()
    }

    fn listener_view(&self, now_ms: u64) -> DjListener {
        DjListener {
            last_interaction: self.listener.map(|(kind, _)| kind),
            last_interaction_ago_ms: self.listener.map(|(_, at)| now_ms.saturating_sub(at)),
            last_spoken_to_ago_ms: self.spoke_ms.map(|at| now_ms.saturating_sub(at)),
            unanswered_check_ins: self.unanswered_streak(),
            recent_actions: Vec::new(),
            actions_omitted: 0,
        }
    }

    /// The listener's actions for prompt `seq` to this client: those after the ones its
    /// previous prompt listed, or the last few minutes for its first, newest first. They count
    /// as listed from here on.
    fn actions_for(
        &mut self,
        principal: &str,
        seq: u64,
        actions: &[LoggedAction],
        now_ms: u64,
    ) -> (Vec<ytm_control_api::ListenerAction>, u32) {
        let actions = listener_log::settled(actions, now_ms);
        let mark = self.action_marks.get(principal).copied();
        let handouts = self.handouts.entry(principal.to_owned()).or_default();
        handouts.push_back((seq, mark));
        while handouts.len() > HANDOUT_LIMIT {
            handouts.pop_front();
        }
        let since = mark
            .is_none()
            .then(|| now_ms.saturating_sub(listener_log::RECENT_WINDOW_MS));
        let listed = listener_log::newest(actions, mark, since, now_ms);
        if let Some(newest) = actions.last() {
            self.action_marks
                .insert(principal.to_owned(), newest.id.max(mark.unwrap_or(0)));
        }
        listed
    }

    /// Takes back what the prompts after `received` listed, since the client never got them.
    fn rewind_actions(&mut self, principal: &str, received: u64) {
        let Some(handouts) = self.handouts.get_mut(principal) else {
            return;
        };
        let Some(lost) = handouts.iter().position(|(seq, _)| *seq > received) else {
            return;
        };
        let (_, mark) = handouts[lost];
        handouts.truncate(lost);
        match mark {
            Some(mark) => self.action_marks.insert(principal.to_owned(), mark),
            None => self.action_marks.remove(principal),
        };
    }

    /// Looks at the listener actions not seen yet and fires a listener signal once a burst that
    /// says where the music should go has settled and the previous signal is at least the gap
    /// ago; inside the gap the burst is held and keeps growing. Only actions made while a DJ
    /// waits count. The words are written when it fires, from the actions as they stand then.
    fn scan_actions(&mut self, now_ms: u64, actions: &[LoggedAction]) {
        let active = self.active(now_ms);
        let since = self.started_ms.unwrap_or(0);
        for action in actions {
            if action.id <= self.signal_scanned {
                continue;
            }
            self.signal_scanned = action.id;
            if active && action.at_ms >= since {
                self.add_to_burst(action);
            }
        }
        if !active {
            self.signal_due = None;
            self.skips.clear();
            return;
        }
        let Some(due) = self.signal_due.as_ref().filter(|due| due.due_ms <= now_ms) else {
            return;
        };
        if due.triggers.is_empty() {
            // Undone within the burst, as repeat-one turned off again.
            self.signal_due = None;
            return;
        }
        if self
            .last_signal_ms
            .is_some_and(|at| now_ms.saturating_sub(at) < SIGNAL_GAP_MS)
        {
            return;
        }
        let due = self.signal_due.take().expect("checked above");
        self.last_signal_ms = Some(now_ms);
        self.skips.clear();
        let mut reasons = Vec::new();
        let mut notes = Vec::new();
        for trigger in &due.triggers {
            let (reason, note) = trigger.words(actions);
            if !reasons.contains(&reason) {
                reasons.push(reason);
            }
            notes.push(note);
        }
        let first_ms = due
            .triggers
            .iter()
            .map(Trigger::at_ms)
            .min()
            .unwrap_or(now_ms);
        let what = notes[notes.len().saturating_sub(SIGNAL_NOTE_LIMIT)..].join("; then ");
        let note = signal_sentence(now_ms.saturating_sub(first_ms), &what);
        self.push_noted(DjCueKind::ListenerSignal, reasons, now_ms, None, Some(note));
    }

    /// Adds one listener action to the burst: it moves the burst's settling time and, when it
    /// says where the music should go, becomes one of its triggers.
    fn add_to_burst(&mut self, action: &LoggedAction) {
        use ytm_control_api::ListenerActionKind as Kind;
        let settles = action.at_ms.saturating_add(SIGNAL_SETTLE_MS);
        if let Some(due) = self.signal_due.as_mut() {
            due.due_ms = due.due_ms.max(settles);
        }
        // Choosing what to hear breaks a run of skips.
        if matches!(
            action.kind,
            Kind::PlayTrack | Kind::PlayCollection | Kind::PickFromQueue | Kind::Previous
        ) {
            self.skips.clear();
        }
        let trigger = match action.kind {
            Kind::Next => {
                if !left_unfinished(action) {
                    self.skips.clear();
                    return;
                }
                self.skips
                    .retain(|(at, _)| action.at_ms.saturating_sub(*at) <= SKIP_STREAK_MS);
                self.skips.push((action.at_ms, action.during.clone()));
                if self.skips.len() < 2 {
                    return;
                }
                let run: Vec<_> = self.skips.iter().map(|(_, track)| track.clone()).collect();
                // The run growing within the same burst is still one streak; a run that started
                // over after a pick or a finished track is another.
                let held = self.signal_due.as_mut().and_then(|due| {
                    due.triggers
                        .iter_mut()
                        .rev()
                        .find_map(|trigger| match trigger {
                            Trigger::Streak { left, .. } if left.len() < run.len() => Some(left),
                            _ => None,
                        })
                });
                if let Some(left) = held {
                    *left = run;
                    return;
                }
                Trigger::Streak {
                    at_ms: action.at_ms,
                    left: run,
                }
            }
            Kind::PlayTrack | Kind::PlayCollection | Kind::PickFromQueue | Kind::UpNextFilter => {
                Trigger::Action(Box::new(action.clone()))
            }
            Kind::Repeat => {
                // Only where repeat ends up counts: the player bar turns repeat off by way of
                // repeat-one.
                if let Some(due) = self.signal_due.as_mut() {
                    due.triggers.retain(|trigger| {
                        !matches!(trigger, Trigger::Action(earlier) if earlier.kind == Kind::Repeat)
                    });
                }
                if action.value.as_deref() != Some("one") {
                    return;
                }
                Trigger::Action(Box::new(action.clone()))
            }
            Kind::Previous => {
                // Late in a track "previous" restarts it; at its very start it goes to the
                // track before, which counts when they had heard some of it.
                let position = action.position_ms.unwrap_or(0);
                let back = if position >= REPLAY_HEARD_MS {
                    action.during.as_ref().map(track_title)
                } else if position < PREVIOUS_TRACK_MS {
                    self.recent
                        .front()
                        .filter(|ended| ended.listened_ms >= REPLAY_HEARD_MS)
                        .map(|ended| track_title(&DjTrack::from(&ended.track)))
                } else {
                    None
                };
                let Some(title) = back else { return };
                Trigger::Replay {
                    at_ms: action.at_ms,
                    title,
                }
            }
            _ => return,
        };
        self.signal_due
            .get_or_insert(SignalDue {
                due_ms: settles,
                triggers: Vec::new(),
            })
            .triggers
            .push(trigger);
    }
}

/// A skip left the track before its last stretch; with no known duration, it did.
fn left_unfinished(action: &LoggedAction) -> bool {
    let duration = action.during.as_ref().and_then(|track| track.duration_ms);
    match (duration, action.position_ms) {
        (Some(duration), Some(position)) => position.saturating_add(FINISH_SLACK_MS) < duration,
        _ => true,
    }
}

/// xorshift64*; plenty for spacing out questions.
fn unit(state: &mut u64) -> f64 {
    *state ^= *state >> 12;
    *state ^= *state << 25;
    *state ^= *state >> 27;
    (state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1_u64 << 53) as f64
}

pub(super) type SharedCues = Arc<Mutex<CueState>>;

pub(super) fn new_cues() -> SharedCues {
    let seed = uuid::Uuid::new_v4().as_u64_pair().0;
    Arc::new(Mutex::new(CueState::new(seed)))
}

/// Samples playback once a second while the DJ is on, so history and prompts do not depend on
/// when the agent happens to call.
fn observe_in_background(core: CoreEngine, cues: SharedCues) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(SAMPLE_MS)).await;
            let now = core.clock().now_ms();
            {
                let Ok(mut state) = cues.lock() else { return };
                if !state.active(now) {
                    state.observing = false;
                    return;
                }
            }
            let Ok(read) = core.read_control(false).await else {
                continue;
            };
            let sample = Sample::from_read(&read);
            let now = core.clock().now_ms();
            let Ok(mut state) = cues.lock() else { return };
            state.observe(now, &sample);
        }
    });
}

impl ControlService {
    /// Notes an agent write so an early track end right after it is attributed to the agent.
    pub(super) fn note_agent_write(&self) {
        let now = self.core.clock().now_ms();
        if let Ok(mut state) = self.cues.lock() {
            state.agent_wrote(now);
        }
    }

    /// One poll of an agent's wait: records what it reported and returns the next prompt, if any.
    pub(super) async fn next_dj_cue(
        &self,
        grant: &ReadGrant,
        input: DjCueInput,
    ) -> Result<DjCueReply, ControlError> {
        let state = self.authorized_state(grant, false).await?;
        let sample = Sample::from_read(&state);
        let now = self.core.clock().now_ms();
        let (start, pending, recent, listener, chattiness) = {
            let mut cues = self
                .cues
                .lock()
                .map_err(|_| unavailable("DJ prompts are unavailable"))?;
            // Bound first, so what this agent reports is kept for its own account.
            cues.bind(sample.scope.as_ref(), now);
            cues.waited(now, &input);
            cues.observe(now, &sample);
            let start = !cues.observing;
            cues.observing = true;
            // An acknowledgement from another core run names another seq sequence.
            let received = input
                .last_received
                .as_ref()
                .filter(|ack| ack.core_epoch == state.snapshot.core_epoch)
                .map(|ack| ack.seq);
            let pending = cues.take(&grant.principal, now, sample.instance.as_ref(), received);
            let mut listener = cues.listener_view(now);
            if let Some(pending) = &pending {
                (listener.recent_actions, listener.actions_omitted) =
                    cues.actions_for(&grant.principal, pending.seq, &sample.actions, now);
            }
            (
                start,
                pending,
                cues.recent_tracks(now),
                listener,
                cues.chattiness,
            )
        };
        if start {
            observe_in_background(self.core.clone(), self.cues.clone());
        }
        let Some(pending) = pending else {
            return Ok(DjCueReply {
                cue: None,
                chattiness,
            });
        };
        let (loaded, preferences_total, preferences_unavailable) =
            match (&grant.scope, self.dj_store.is_some()) {
                (Some(scope), true) => match self.applied_preferences(scope).await {
                    Ok(applied) => (
                        Some(applied.visible(&grant.principal)),
                        Some(applied.total),
                        false,
                    ),
                    Err(_) => (None, None, true),
                },
                _ => (Some(Vec::new()), Some(0), false),
            };
        let (preferences, preferences_unchanged) = self
            .cues
            .lock()
            .map_err(|_| unavailable("DJ prompts are unavailable"))?
            .preferences_for(&grant.principal, loaded);
        let now_playing = sample.track.as_ref().map(|track| DjNowPlaying {
            track: DjTrack::from(track),
            playback: sample.playback,
            position_ms: sample.position_ms,
            remaining_ms: sample
                .duration_ms
                .map(|duration| duration.saturating_sub(sample.position_ms)),
        });
        let prompt = prompt_text(&pending, &sample, &listener, state.snapshot.player.autoplay);
        let moment = self.project_moment(grant, &state).await;
        Ok(DjCueReply {
            cue: Some(DjCue {
                seq: pending.seq,
                core_epoch: Some(state.snapshot.core_epoch.clone()),
                kind: pending.kind,
                reasons: pending.reasons,
                age_ms: now.saturating_sub(pending.fired_ms),
                prompt,
                now_playing,
                upcoming: if sample.queued_after.is_some() {
                    UpcomingStatus::Known
                } else {
                    UpcomingStatus::Unknown
                },
                up_next: sample.up_next.iter().map(DjTrack::from).collect(),
                more_queued: sample.more_queued,
                official_up_next: sample.official.clone(),
                recent,
                listener,
                chattiness,
                preferences,
                preferences_unchanged,
                preferences_total,
                preferences_unavailable,
                moment: Some(moment),
            }),
            chattiness,
        })
    }
}

const REPORT_BACK: &str = "On your next ytm_wait_dj_prompt call, set spoke if you said something \
to the listener and userSpoke if they talked to you.";

const KEEP_LOOPING: &str = "After you say something, call ytm_wait_dj_prompt again right away \
instead of stopping for an answer; the listener's reply arrives with a later result.";

fn minutes(ms: u64) -> u64 {
    (ms + 30_000) / 60_000
}

/// A track as `"title" by artists` for prompt text.
fn track_title(track: &DjTrack) -> String {
    if track.artists.is_empty() {
        format!("\"{}\"", track.title)
    } else {
        format!("\"{}\" by {}", track.title, track.artists.join(", "))
    }
}

/// What a track-ending prompt says when the page's own continuation is not readable.
fn autoplay_flag_text(queued_after: Option<usize>, autoplay: Option<bool>) -> &'static str {
    match (queued_after, autoplay) {
        (Some(count), Some(true)) if count <= 1 => {
            " (YouTube Music autoplay would continue on its own)"
        }
        (None, Some(true)) => {
            " (YouTube Music autoplay is on; read the context before changing the queue)"
        }
        (None, Some(false)) => {
            " (YouTube Music autoplay is off; read the context before changing the queue)"
        }
        (None, None) => " (read the context before changing the queue)",
        _ => "",
    }
}

/// What YouTube Music plays on its own after the queued tracks, as observed on the page.
/// `name_tracks` names the first few suggestions, when the end of the queue is near, and the
/// Up Next filters (`filter_choices`) that could steer them.
fn official_clause(
    official: &DjOfficialUpNext,
    filter_choices: &[String],
    name_tracks: bool,
) -> String {
    let filter = official
        .filter
        .as_ref()
        .map(|label| format!(" (filter: {label})"))
        .unwrap_or_default();
    match official.status {
        OfficialUpcomingStatus::Observed if !official.tracks.is_empty() => {
            if !name_tracks {
                return "after the queued tracks YouTube Music continues on its own with its \
                        suggestions"
                    .to_owned();
            }
            let named: Vec<_> = official
                .tracks
                .iter()
                .take(PROMPT_OFFICIAL_LIMIT)
                .map(track_title)
                .collect();
            let more = if official.more || official.tracks.len() > PROMPT_OFFICIAL_LIMIT {
                " and more"
            } else {
                ""
            };
            let steer = if filter_choices.is_empty() {
                String::new()
            } else {
                format!(
                    " or steer with an Up Next filter: {}",
                    filter_choices.join(", ")
                )
            };
            format!(
                "then YouTube Music continues on its own with {}{more}{filter}, which you may \
                 leave if that fits{steer}",
                named.join(", ")
            )
        }
        OfficialUpcomingStatus::Off => {
            "YouTube Music autoplay is off, so nothing plays after the queued tracks".to_owned()
        }
        OfficialUpcomingStatus::Radio => "this is a YouTube Music radio that refills itself".into(),
        OfficialUpcomingStatus::Loading => {
            "YouTube Music is still loading what plays after the queued tracks".to_owned()
        }
        OfficialUpcomingStatus::Observed | OfficialUpcomingStatus::None => {
            "YouTube Music has no suggestions loaded to play after the queued tracks".to_owned()
        }
    }
}

/// The instruction that goes with a prompt; the facts it mentions are as of delivery.
fn prompt_text(
    cue: &Pending,
    sample: &Sample,
    listener: &DjListener,
    autoplay: Option<bool>,
) -> String {
    match cue.kind {
        DjCueKind::TrackEnding => {
            let title = sample.track.as_ref().map_or_else(
                || "The current track".to_owned(),
                |track| track_title(&DjTrack::from(track)),
            );
            let left = sample
                .duration_ms
                .map(|duration| duration.saturating_sub(sample.position_ms).div_ceil(1000))
                .unwrap_or(0);
            let queued = match sample.queued_after {
                None => "what is queued after it is not known right now".to_owned(),
                Some(0) => "nothing is queued after it".to_owned(),
                Some(1) => "one track is queued after it".to_owned(),
                Some(count) if sample.more_queued => {
                    format!("{count} or more tracks are queued after it")
                }
                Some(count) => format!("{count} tracks are queued after it"),
            };
            let autoplay = match (sample.queued_after, sample.official.as_ref()) {
                (Some(count), Some(official))
                    if count <= 1
                        || !matches!(
                            official.status,
                            OfficialUpcomingStatus::Observed | OfficialUpcomingStatus::None
                        ) =>
                {
                    format!(
                        "; {}",
                        official_clause(official, &sample.filter_choices, true)
                    )
                }
                (None, Some(official)) => format!(
                    " (read the context before changing the queue; {})",
                    official_clause(official, &sample.filter_choices, false)
                ),
                (Some(_), Some(_)) => String::new(),
                (_, None) => autoplay_flag_text(sample.queued_after, autoplay).to_owned(),
            };
            let unanswered = match listener.unanswered_check_ins {
                0 => "",
                1 => {
                    " The listener has not answered your last remark, so stay quiet unless \
                       something matters."
                }
                _ => {
                    " The listener has not answered your last remarks, so stay quiet unless \
                      something matters."
                }
            };
            format!(
                "{title} ends in about {left} seconds and {queued}{autoplay}. You are this \
                 listener's DJ: if what comes next should change, queue, swap or reorder songs \
                 now; if it already fits, do nothing. Say a short line to the listener only if \
                 it adds something, in their language.{unanswered} {KEEP_LOOPING} {REPORT_BACK}"
            )
        }
        DjCueKind::CheckIn => {
            let mut why = Vec::new();
            for reason in &cue.reasons {
                match reason {
                    DjCueReason::ListenerQuiet => {
                        why.push(match listener.last_interaction_ago_ms {
                            Some(ago) => format!(
                                "the listener has not done or said anything for {} minutes",
                                minutes(ago)
                            ),
                            None => {
                                "the listener has not done or said anything for a while".to_owned()
                            }
                        })
                    }
                    DjCueReason::LongTrack => why.push(format!(
                        "this track runs {} minutes",
                        minutes(sample.duration_ms.unwrap_or(0))
                    )),
                    DjCueReason::Routine | DjCueReason::LittleQueued => {
                        why.push("it has been a while since anyone checked in".to_owned())
                    }
                    // Listener-signal reasons never come with a check-in.
                    DjCueReason::SkipStreak
                    | DjCueReason::ManualPick
                    | DjCueReason::Replay
                    | DjCueReason::FilterChanged => {}
                }
            }
            format!(
                "A good moment to check in: {}. If it feels right, ask in one natural line \
                 whether the music still suits them, shaped by what is playing, what came \
                 before and any skips, in their language and a tone that fits the mood. Skip it \
                 if the moment is wrong. {KEEP_LOOPING} {REPORT_BACK}",
                why.join("; ")
            )
        }
        DjCueKind::ListenerSignal => {
            let what = cue
                .note
                .as_deref()
                .unwrap_or("By hand, the listener just changed what is playing.");
            format!(
                "{what} If that says what should come next, you may adjust the queue to match; \
                 do not undo what they just chose. Say nothing unless it adds something. \
                 {KEEP_LOOPING} {REPORT_BACK}"
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quiet_beat(idle_ms: u64) -> Beat {
        Beat {
            playing: true,
            suppressed: false,
            idle_ms,
            long_track: false,
            stretch: 1,
        }
    }

    /// Plays ticks until the heartbeat fires and returns the playing time it took. The listener
    /// stays silent, so the idle time grows from `idle_ms` as it would in use.
    fn gap(idle_ms: u64, long_track: bool, level: DjChattiness, rng: &mut u64) -> u64 {
        let mut heartbeat = Heartbeat::default();
        let beat = |now: u64| Beat {
            long_track,
            ..quiet_beat(idle_ms + now)
        };
        heartbeat.advance(0, &beat(0), level, &mut || unit(rng));
        let mut now = 0;
        loop {
            now += TICK_MS;
            if heartbeat
                .advance(now, &beat(now), level, &mut || unit(rng))
                .is_some()
            {
                return now;
            }
        }
    }

    #[test]
    fn heartbeat_respects_the_floor_and_cap_and_averages_as_documented() {
        let mut rng = 7;
        for (level, floor, cap, low, high) in [
            (DjChattiness::Quiet, 8 * 60_000, 25 * 60_000, 13.5, 16.5),
            (DjChattiness::Normal, 4 * 60_000, 15 * 60_000, 7.8, 9.6),
            (DjChattiness::Chatty, 150_000, 9 * 60_000, 4.8, 6.0),
        ] {
            let gaps: Vec<u64> = (0..4000).map(|_| gap(0, false, level, &mut rng)).collect();
            assert!(gaps.iter().all(|gap| (floor..=cap).contains(gap)));
            let mean = gaps.iter().sum::<u64>() as f64 / gaps.len() as f64 / 60_000.0;
            assert!((low..high).contains(&mean), "{level:?} mean {mean}");
        }
    }

    #[test]
    fn a_quiet_listener_and_a_long_track_bring_questions_sooner() {
        let mut rng = 11;
        let mean = |idle_ms: u64, long: bool, rng: &mut u64| {
            (0..3000)
                .map(|_| gap(idle_ms, long, DjChattiness::Normal, rng))
                .sum::<u64>() as f64
                / 3000.0
        };
        let attentive = mean(0, false, &mut rng);
        let silent = mean(SILENT_LISTENER_MS, false, &mut rng);
        let long = mean(0, true, &mut rng);
        assert!(silent < attentive * 0.85, "{silent} vs {attentive}");
        assert!(long < attentive * 0.95, "{long} vs {attentive}");
    }

    #[test]
    fn pauses_suppression_and_off_never_fire_and_reset_starts_over() {
        let mut rng = 3;
        let mut heartbeat = Heartbeat::default();
        let paused = Beat {
            playing: false,
            ..quiet_beat(0)
        };
        heartbeat.advance(0, &paused, DjChattiness::Normal, &mut || unit(&mut rng));
        assert!(
            heartbeat
                .advance(3_600_000, &paused, DjChattiness::Normal, &mut || unit(
                    &mut rng
                ))
                .is_none()
        );
        let suppressed = Beat {
            suppressed: true,
            ..quiet_beat(0)
        };
        assert!(
            heartbeat
                .advance(7_200_000, &suppressed, DjChattiness::Normal, &mut || unit(
                    &mut rng
                ))
                .is_none()
        );
        // Once the suppression ends the overdue question comes at the next tick.
        assert!(
            heartbeat
                .advance(7_215_000, &quiet_beat(0), DjChattiness::Normal, &mut || {
                    unit(&mut rng)
                })
                .is_some()
        );
        let mut off = Heartbeat::default();
        off.advance(0, &quiet_beat(0), DjChattiness::Off, &mut || 0.0);
        assert!(
            off.advance(7_200_000, &quiet_beat(0), DjChattiness::Off, &mut || 0.0)
                .is_none()
        );
        // A reset restarts the minimum gap.
        let mut fresh = Heartbeat::default();
        fresh.advance(0, &quiet_beat(0), DjChattiness::Normal, &mut || 0.0);
        fresh.advance(200_000, &quiet_beat(0), DjChattiness::Normal, &mut || 1.0);
        fresh.reset();
        assert!(
            fresh
                .advance(400_000, &quiet_beat(0), DjChattiness::Normal, &mut || 0.0)
                .is_none()
        );
        assert!(
            fresh
                .advance(440_000, &quiet_beat(0), DjChattiness::Normal, &mut || 0.0)
                .is_some()
        );
    }

    fn song(id: &str, duration_ms: Option<u64>) -> Track {
        Track {
            video_id: ytm_protocol::VideoId::new(id).unwrap(),
            title: id.into(),
            artists: vec![],
            album: None,
            duration_ms,
            availability: ytm_protocol::TrackAvailability::Available,
            version_tag: None,
            rating: ytm_control_api::Rating::Unknown,
        }
    }

    fn playing(instance: &str, duration_ms: Option<u64>, position_ms: u64) -> Sample {
        Sample {
            scope: Some(AccountScope::new("fixture-account-a").unwrap()),
            playback: PlaybackStatus::Playing,
            instance: Some(TrackInstanceId::new(instance).unwrap()),
            track: Some(song(instance, duration_ms)),
            position_ms,
            duration_ms,
            queued_after: Some(3),
            up_next: vec![],
            more_queued: false,
            official: None,
            filter_choices: vec![],
            last_manual_ms: None,
            actions: Vec::new().into(),
        }
    }

    fn on(state: &mut CueState, now_ms: u64) {
        state.waited(
            now_ms,
            &DjCueInput {
                chattiness: Some(DjChattiness::Normal),
                ..Default::default()
            },
        );
    }

    /// Seeking to the last seconds and then skipping is not a finished track, and the jump does
    /// not count as listening.
    #[test]
    fn a_seek_to_the_end_then_skip_counts_only_the_time_played() {
        let mut state = CueState::new(5);
        on(&mut state, 0);
        for second in 0..=10 {
            state.observe(
                second * 1_000,
                &playing("fixture-a", Some(240_000), second * 1_000),
            );
        }
        state.observe(11_000, &playing("fixture-a", Some(240_000), 235_000));
        state.observe(12_000, &playing("fixture-a", Some(240_000), 236_000));
        state.observe(13_000, &playing("fixture-b", Some(240_000), 0));
        let recent = state.recent_tracks(13_000);
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].outcome, DjTrackOutcome::Skipped);
        assert_eq!(recent[0].listened_ms, 11_000);
    }

    /// A rewind inside the last stretch or a reset position just before the next track does
    /// not undo a track that was played into its end.
    #[test]
    fn a_track_played_into_its_end_stays_finished() {
        let mut state = CueState::new(5);
        on(&mut state, 0);
        let duration = Some(240_000);
        for (now, position) in [(0, 0), (230_000, 230_000), (231_000, 225_000)] {
            state.observe(now, &playing("fixture-a", duration, position));
        }
        state.observe(246_000, &playing("fixture-a", duration, 240_000));
        state.observe(247_000, &playing("fixture-a", duration, 0));
        state.observe(248_000, &playing("fixture-b", duration, 0));
        state.observe(300_000, &playing("fixture-b", duration, 52_000));
        state.observe(301_000, &playing("fixture-b", duration, 239_000));
        state.observe(302_000, &playing("fixture-b", duration, 3_000));
        state.observe(303_000, &playing("fixture-b", duration, 4_000));
        state.observe(304_000, &playing("fixture-c", duration, 0));
        let recent = state.recent_tracks(304_000);
        let outcomes: Vec<_> = recent.iter().map(|ended| ended.outcome).collect();
        // The second track was jumped to its end and back, then skipped.
        assert_eq!(
            outcomes,
            [DjTrackOutcome::Skipped, DjTrackOutcome::Finished]
        );
        assert_eq!(recent[1].listened_ms, 245_000);
    }

    /// Without a duration the end is judged by what caused it: a command just before it is a
    /// skip, otherwise the track ran out.
    #[test]
    fn an_unknown_duration_is_judged_by_attribution() {
        let mut state = CueState::new(5);
        on(&mut state, 0);
        state.observe(0, &playing("fixture-a", None, 0));
        state.observe(60_000, &playing("fixture-a", None, 60_000));
        state.agent_wrote(61_000);
        state.observe(62_000, &playing("fixture-b", None, 0));
        state.observe(150_000, &playing("fixture-b", None, 88_000));
        state.observe(151_000, &playing("fixture-c", None, 0));
        let outcomes: Vec<_> = state
            .recent_tracks(151_000)
            .iter()
            .map(|ended| ended.outcome)
            .collect();
        assert_eq!(
            outcomes,
            [DjTrackOutcome::Finished, DjTrackOutcome::SkippedByAgent]
        );
    }

    /// With the DJ off for half an hour mid-track, turning it back on starts the minimum gap
    /// over instead of catching up the missed ticks.
    #[test]
    fn coming_back_on_does_not_catch_up_missed_check_ins() {
        let mut state = CueState::new(9);
        on(&mut state, 0);
        let long = Some(3_600_000);
        for tick in 0..=4 {
            state.observe(tick * 15_000, &playing("fixture-a", long, tick * 15_000));
        }
        let back = 60_000 + 30 * 60_000;
        on(&mut state, back);
        let mut now = back;
        while now < back + 4 * 60_000 - TICK_MS {
            state.observe(now, &playing("fixture-a", long, now));
            now += 1_000;
        }
        assert!(
            state
                .pending
                .iter()
                .all(|cue| cue.kind != DjCueKind::CheckIn),
            "a check-in came before the minimum gap"
        );
    }

    /// A prompt after the player changed account carries nothing the previous account's
    /// listening left behind.
    #[test]
    fn an_account_change_starts_the_dj_history_over() {
        let mut state = CueState::new(5);
        on(&mut state, 0);
        state.observe(1_000, &playing("fixture-a", Some(240_000), 0));
        state.waited(
            2_000,
            &DjCueInput {
                user_spoke: true,
                spoke: true,
                ..Default::default()
            },
        );
        state.observe(30_000, &playing("fixture-a", Some(240_000), 28_000));
        state.observe(31_000, &playing("fixture-b", Some(240_000), 0));
        state.push(DjCueKind::CheckIn, vec![DjCueReason::Routine], 31_000, None);
        assert!(state.take("fixture-agent", 31_000, None, None).is_some());
        assert_eq!(state.recent_tracks(31_000).len(), 1);
        assert!(state.listener_view(31_000).last_spoken_to_ago_ms.is_some());
        state.push(DjCueKind::CheckIn, vec![DjCueReason::Routine], 32_000, None);

        for scope in [Some("fixture-account-b"), None] {
            let mut other = playing("fixture-b", Some(240_000), 5_000);
            other.scope = scope.map(|scope| AccountScope::new(scope).unwrap());
            state.observe(36_000, &other);
            assert!(state.recent_tracks(36_000).is_empty());
            let listener = state.listener_view(36_000);
            assert_eq!(listener.last_interaction, None);
            assert_eq!(listener.last_spoken_to_ago_ms, None);
            assert!(state.pending.is_empty() && state.delivered.is_empty());
        }
    }

    /// A prompt handed to a wait whose reply never arrived comes again once the client names the
    /// last one it got; an acknowledgement ahead of what was handed out skips nothing.
    #[test]
    fn an_acknowledgement_offers_a_lost_prompt_again_but_never_skips_one() {
        let mut state = CueState::new(7);
        for _ in 0..2 {
            state.push(DjCueKind::CheckIn, vec![DjCueReason::Routine], 0, None);
        }
        let take = |state: &mut CueState, received| {
            state
                .take("fixture-agent", 1_000, None, received)
                .map(|cue| cue.seq)
        };
        let first = take(&mut state, None).unwrap();
        let lost = take(&mut state, None).unwrap();
        assert_eq!(take(&mut state, None), None);
        let tastes = vec![taste("fixture-taste", 1)];
        assert!(
            !state
                .preferences_for("fixture-agent", Some(tastes.clone()))
                .1
        );
        assert_eq!(take(&mut state, Some(first)), Some(lost));
        // The lost reply carried the tastes, so the prompt offered again carries them too.
        assert!(!state.preferences_for("fixture-agent", Some(tastes)).1);
        assert_eq!(take(&mut state, Some(lost)), None);
        state.push(DjCueKind::CheckIn, vec![DjCueReason::Routine], 0, None);
        assert_eq!(take(&mut state, Some(lost + 5)), Some(lost + 1));
    }

    #[test]
    fn ending_lead_is_longer_when_little_is_queued_and_bounded_by_short_tracks() {
        assert_eq!(ending_lead_ms(240_000, Some(3)), 45_000);
        assert_eq!(ending_lead_ms(240_000, Some(1)), 75_000);
        assert_eq!(ending_lead_ms(90_000, Some(5)), 30_000);
        assert!(!in_ending_window(None, 230_000, Some(3)));
        assert!(!in_ending_window(Some(240_000), 190_000, Some(3)));
        assert!(in_ending_window(Some(240_000), 196_000, Some(3)));
        assert!(in_ending_window(Some(240_000), 170_000, Some(0)));
        assert!(!in_ending_window(Some(240_000), 240_000, Some(3)));
        assert!(near_end(Some(240_000), 240_000, Some(3)));
        assert!(!near_end(None, 240_000, Some(3)));
        // Unknown is not "little queued": the normal lead, so the 170 s mark is not yet near.
        assert_eq!(ending_lead_ms(240_000, None), 45_000);
        assert!(!in_ending_window(Some(240_000), 170_000, None));
    }

    const HOURS_LONG: Option<u64> = Some(6 * 3_600_000);

    /// Plays one very long track second by second, with the agent waiting every minute without
    /// anything to report, and returns when check-ins fired.
    fn check_ins_between(state: &mut CueState, from_ms: u64, until_ms: u64) -> Vec<u64> {
        let mut fired = Vec::new();
        let mut now = from_ms;
        while now < until_ms {
            if now.is_multiple_of(60_000) {
                state.waited(now, &DjCueInput::default());
            }
            let seq = state.seq;
            state.observe(now, &playing("fixture-long", HOURS_LONG, now));
            if state.seq != seq {
                assert_eq!(state.pending.back().unwrap().kind, DjCueKind::CheckIn);
                fired.push(now);
            }
            now += 1_000;
        }
        fired
    }

    fn chatty(state: &mut CueState, now_ms: u64) {
        state.waited(
            now_ms,
            &DjCueInput {
                chattiness: Some(DjChattiness::Chatty),
                ..Default::default()
            },
        );
    }

    /// Unanswered check-ins double the minimum gap each time, up to eightfold, and never more
    /// than three come within an hour; the track-ending prompt then asks the agent to hold back.
    #[test]
    fn unanswered_check_ins_come_less_often_and_at_most_three_an_hour() {
        let mut state = CueState::new(21);
        chatty(&mut state, 0);
        let fired = check_ins_between(&mut state, 0, 4 * 3_600_000);
        assert!(fired.len() >= 5, "{fired:?}");
        let floor = 150_000;
        let mut previous = 0;
        for (index, at) in fired.iter().enumerate() {
            let stretch = 1 << index.min(3);
            assert!(at - previous >= floor * stretch, "{fired:?}");
            previous = *at;
            let window = fired
                .iter()
                .filter(|other| (at.saturating_sub(CHECK_IN_WINDOW_MS - 1)..=*at).contains(other))
                .count();
            assert!(window <= 3, "{fired:?}");
        }
        let streak = u32::try_from(fired.len()).unwrap();
        assert_eq!(state.listener_view(0).unanswered_check_ins, streak);

        let ending = Pending {
            seq: 1,
            kind: DjCueKind::TrackEnding,
            reasons: Vec::new(),
            fired_ms: 0,
            instance: None,
            note: None,
        };
        let sample = playing("fixture-long", Some(240_000), 200_000);
        let text = prompt_text(&ending, &sample, &state.listener_view(0), None);
        assert!(
            text.contains("has not answered your last remarks"),
            "{text}"
        );
        let answered = DjListener {
            unanswered_check_ins: 0,
            ..state.listener_view(0)
        };
        let text = prompt_text(&ending, &sample, &answered, None);
        assert!(!text.contains("has not answered"), "{text}");
    }

    /// Once the listener speaks the streak and the hourly hold are gone and the next check-in
    /// falls inside the normal gaps again.
    #[test]
    fn a_listener_interaction_restores_the_normal_pace() {
        let mut state = CueState::new(33);
        chatty(&mut state, 0);
        let start = 3 * 3_600_000;
        assert!(check_ins_between(&mut state, 0, start).len() >= 4);
        assert!(state.listener_view(start).unanswered_check_ins >= 3);
        state.waited(
            start,
            &DjCueInput {
                user_spoke: true,
                ..Default::default()
            },
        );
        assert_eq!(state.listener_view(start).unanswered_check_ins, 0);
        let fired = check_ins_between(&mut state, start, start + 10 * 60_000);
        let first = fired.first().expect("a check-in within the chatty cap") - start;
        assert!((150_000..=9 * 60_000).contains(&first), "{first}");
        // That check-in is the only one still waiting for an answer.
        assert_eq!(state.listener_view(start).unanswered_check_ins, 1);
    }

    fn taste(id: &str, at_ms: u64) -> DjPreference {
        DjPreference {
            preference_id: id.into(),
            subject: ytm_control_api::DjPreferenceSubject::Video {
                video_id: ytm_protocol::VideoId::new("fixture-v").unwrap(),
                title: None,
            },
            stance: ytm_control_api::DjPreferenceStance::Prefer,
            note: None,
            recorded_by: "fixture-agent".into(),
            recorded_at_ms: at_ms,
        }
    }

    /// Tastes go with a client's first prompt and after each change; a failed read sends none
    /// without claiming a change, and a new conversation or account gets them again.
    #[test]
    fn preferences_are_sent_first_and_after_a_change_only() {
        let mut state = CueState::new(5);
        on(&mut state, 0);
        let one = vec![taste("fixture-p1", 10)];
        assert_eq!(
            state.preferences_for("agent", Some(one.clone())),
            (one.clone(), false)
        );
        assert_eq!(
            state.preferences_for("agent", Some(one.clone())),
            (vec![], true)
        );
        assert_eq!(
            state.preferences_for("other", Some(one.clone())),
            (one.clone(), false)
        );
        assert_eq!(state.preferences_for("agent", None), (vec![], true));
        let two = vec![taste("fixture-p2", 20), taste("fixture-p1", 10)];
        assert_eq!(
            state.preferences_for("agent", Some(two.clone())),
            (two.clone(), false)
        );
        assert_eq!(
            state.preferences_for("agent", Some(vec![])),
            (vec![], false)
        );
        assert_eq!(state.preferences_for("new", None), (vec![], false));

        // Off for longer than the active window, then a new conversation.
        state.preferences_for("agent", Some(one.clone()));
        on(&mut state, ACTIVE_MS + 1);
        assert_eq!(
            state.preferences_for("agent", Some(one.clone())),
            (one.clone(), false)
        );
        let mut other = playing("fixture-a", Some(240_000), 0);
        other.scope = Some(AccountScope::new("fixture-account-b").unwrap());
        state.observe(ACTIVE_MS + 2, &other);
        assert_eq!(
            state.preferences_for("agent", Some(one.clone())),
            (one, false)
        );
    }
}
