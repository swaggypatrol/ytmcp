//! A short log of what the listener did by hand: commands from the player window, the tray,
//! media keys or MPRIS, and ratings or playlist additions made in the app. Agent commands never
//! reach it. The engine records each action as it admits it, with the track playing and the
//! position then; DJ prompts and context reads project it as facts for the agent.
//!
//! The log is small (the newest 64 actions within 30 minutes) and belongs to one account: it is
//! emptied when the player shows another account or none.

use std::collections::VecDeque;
use std::sync::Arc;

use ytm_control_api::{DjTrack, ListenerAction, ListenerActionKind, MAX_LISTENER_ACTIONS};
use ytm_protocol::{
    AccountScope, MutationKind, MutationRequest, PlaybackStatus, PlayerCommandPayload,
    PlayerObservedState, PlayerOp, QueueEntryId, QueueSnapshot, RepeatMode, VideoId,
};

use super::state::track_projection;
use crate::commands::resolve_toggle_op;

const LOG_LIMIT: usize = 64;
const LOG_AGE_MS: u64 = 30 * 60_000;
/// Volume or seek changes this close together are one drag.
const DRAG_MS: u64 = 2_000;
/// How far back a context read, or a client's first prompt, looks.
pub(crate) const RECENT_WINDOW_MS: u64 = 10 * 60_000;
const VALUE_CHARS: usize = 64;

/// One logged action. `id` grows with every record, including a drag's later changes, so a
/// reader can ask for what came after the last id it saw.
#[derive(Debug, Clone)]
pub(crate) struct LoggedAction {
    pub id: u64,
    /// When it happened; for a drag, its last change.
    pub at_ms: u64,
    pub kind: ListenerActionKind,
    pub during: Option<DjTrack>,
    pub position_ms: Option<u64>,
    pub subject: Option<DjTrack>,
    /// The subject's video while its track facts are not known.
    subject_video: Option<VideoId>,
    /// The action was about the track playing then, so it has no subject of its own.
    about_playing: bool,
    pub volume_from: Option<u8>,
    pub volume_to: Option<u8>,
    pub seek_to_ms: Option<u64>,
    pub value: Option<String>,
}

impl LoggedAction {
    /// The track the action was about: its subject, or the track playing when it was that one.
    pub(crate) fn about(&self) -> Option<&DjTrack> {
        self.subject
            .as_ref()
            .or(self.during.as_ref().filter(|_| self.about_playing))
    }

    pub(crate) fn view(&self, now_ms: u64) -> ListenerAction {
        ListenerAction {
            action: self.kind,
            ago_ms: now_ms.saturating_sub(self.at_ms),
            during: self.during.clone(),
            position_ms: self.position_ms,
            subject: self.about().cloned(),
            volume_from: self.volume_from,
            volume_to: self.volume_to,
            seek_to_ms: self.seek_to_ms,
            value: self.value.clone(),
        }
    }
}

/// An action about to be logged, as of when the listener made it.
pub(crate) struct Draft(LoggedAction);

impl Draft {
    fn new(
        kind: ListenerActionKind,
        player: &PlayerObservedState,
        at_ms: u64,
        position_ms: u64,
    ) -> Self {
        let during = player
            .current_track
            .as_ref()
            .map(|track| DjTrack::from(&track_projection(track)));
        Self(LoggedAction {
            id: 0,
            at_ms,
            kind,
            position_ms: during.as_ref().map(|_| position_ms),
            during,
            subject: None,
            subject_video: None,
            about_playing: false,
            volume_from: None,
            volume_to: None,
            seek_to_ms: None,
            value: None,
        })
    }

    fn value(mut self, value: &str) -> Self {
        self.0.value = Some(value.chars().take(VALUE_CHARS).collect());
        self
    }

    /// The track the action was about, unless it is the one playing.
    fn subject(mut self, video: &VideoId, found: Option<DjTrack>) -> Self {
        if self.0.during.as_ref().map(|track| &track.video_id) == Some(video) {
            self.0.about_playing = true;
        } else {
            match found {
                Some(track) => self.0.subject = Some(track),
                None => self.0.subject_video = Some(video.clone()),
            }
        }
        self
    }
}

fn percent(volume: f64) -> u8 {
    (volume.clamp(0.0, 1.0) * 100.0).round() as u8
}

fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

/// The track facts the player knows for a video: playing, queued or suggested next.
fn find_track(
    video: &VideoId,
    player: &PlayerObservedState,
    queue: Option<&QueueSnapshot>,
) -> Option<DjTrack> {
    let queued = queue.into_iter().flat_map(|queue| {
        let official = queue
            .official_upcoming
            .iter()
            .flat_map(|official| &official.tracks);
        queue
            .entries
            .iter()
            .map(|entry| &entry.track)
            .chain(official)
    });
    player
        .current_track
        .iter()
        .chain(queued)
        .find(|track| &track.video_id == video)
        .map(|track| DjTrack::from(&track_projection(track)))
}

fn entry_track(entry: Option<&QueueEntryId>, queue: Option<&QueueSnapshot>) -> Option<DjTrack> {
    let entry = entry?;
    queue?
        .entries
        .iter()
        .find(|candidate| &candidate.entry_id == entry)
        .map(|found| DjTrack::from(&track_projection(&found.track)))
}

/// What an admitted manual command says the listener did. `pending_volume` is the volume an
/// earlier command is still setting, if any; `playback` is the playback a toggle will find
/// when it runs, after the commands ahead of it.
pub(crate) fn command_draft(
    payload: &PlayerCommandPayload,
    player: &PlayerObservedState,
    at_ms: u64,
    position_ms: u64,
    pending_volume: Option<f64>,
    playback: PlaybackStatus,
    queue: Option<&QueueSnapshot>,
) -> Option<Draft> {
    use ListenerActionKind as Kind;
    let args = payload.args.as_ref();
    let op = match payload.op {
        PlayerOp::Toggle => resolve_toggle_op(playback),
        op => op,
    };
    let draft = |kind| Draft::new(kind, player, at_ms, position_ms);
    let draft = match op {
        PlayerOp::Play => draft(Kind::Play),
        PlayerOp::Pause => draft(Kind::Pause),
        PlayerOp::Stop => draft(Kind::Stop),
        PlayerOp::Next => draft(Kind::Next),
        PlayerOp::Previous => draft(Kind::Previous),
        PlayerOp::Seek => {
            let mut seek = draft(Kind::Seek);
            seek.0.seek_to_ms = args?.position_ms;
            seek
        }
        PlayerOp::SetVolume => {
            let mut volume = draft(Kind::Volume);
            volume.0.volume_from = Some(percent(pending_volume.unwrap_or(player.volume)));
            volume.0.volume_to = Some(percent(args?.volume?));
            volume
        }
        PlayerOp::SetMuted => draft(Kind::Mute).value(on_off(args?.muted?)),
        PlayerOp::SetShuffle => draft(Kind::Shuffle).value(on_off(args?.shuffle?)),
        PlayerOp::SetAutoplay => draft(Kind::Autoplay).value(on_off(args?.autoplay?)),
        PlayerOp::SetRepeat => draft(Kind::Repeat).value(match args?.repeat? {
            RepeatMode::Off => "off",
            RepeatMode::All => "all",
            RepeatMode::One => "one",
        }),
        PlayerOp::PlayTarget | PlayerOp::PlayCollection | PlayerOp::Enqueue => {
            let target = args?.target.as_ref()?;
            let kind = match op {
                PlayerOp::PlayTarget => Kind::PlayTrack,
                PlayerOp::PlayCollection => Kind::PlayCollection,
                _ => Kind::AddToQueue,
            };
            let action = draft(kind);
            let found = find_track(&target.video_id, player, queue);
            action.subject(&target.video_id, found)
        }
        PlayerOp::SelectQueueEntry | PlayerOp::RemoveQueueEntry | PlayerOp::MoveQueueEntry => {
            let kind = match op {
                PlayerOp::SelectQueueEntry => Kind::PickFromQueue,
                PlayerOp::RemoveQueueEntry => Kind::RemoveFromQueue,
                _ => Kind::ReorderQueue,
            };
            let action = draft(kind);
            match entry_track(args?.queue_entry_id.as_ref(), queue) {
                Some(track) => {
                    let video = track.video_id.clone();
                    action.subject(&video, Some(track))
                }
                None => action,
            }
        }
        PlayerOp::ClearUpcoming => draft(Kind::ClearUpcoming),
        PlayerOp::SetQueueFilter => {
            let id = args?.queue_filter_id.as_deref()?;
            let label = queue
                .and_then(|queue| queue.filters.as_ref())
                .into_iter()
                .flatten()
                .find(|filter| filter.filter_id == id)
                .map_or(id, |filter| filter.label.as_str());
            draft(Kind::UpNextFilter).value(label)
        }
        PlayerOp::Toggle => return None,
    };
    Some(draft)
}

/// A rating or playlist addition the listener made in the app, drafted at the click and
/// recorded once the account confirms it.
pub(crate) fn mutation_draft(
    request: &MutationRequest,
    player: &PlayerObservedState,
    at_ms: u64,
    position_ms: u64,
    queue: Option<&QueueSnapshot>,
) -> Draft {
    let found = find_track(&request.video_id, player, queue);
    let draft = match request.kind {
        MutationKind::SetLike => Draft::new(ListenerActionKind::Rate, player, at_ms, position_ms)
            .value(if request.liked == Some(true) {
                "liked"
            } else {
                "neutral"
            }),
        MutationKind::AddToPlaylist => Draft::new(
            ListenerActionKind::AddToPlaylist,
            player,
            at_ms,
            position_ms,
        ),
    };
    draft.subject(&request.video_id, found)
}

/// The listener's recent actions for one account, oldest first.
#[derive(Default)]
pub(crate) struct ListenerLog {
    scope: Option<AccountScope>,
    last_id: u64,
    entries: VecDeque<LoggedAction>,
}

impl ListenerLog {
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    /// Starts over when the account is not the one the log belongs to.
    fn bind(&mut self, scope: Option<&AccountScope>) {
        if self.scope.as_ref() != scope {
            self.scope = scope.cloned();
            self.entries.clear();
        }
    }

    fn prune(&mut self, now_ms: u64) {
        while self.entries.len() > LOG_LIMIT
            || self
                .entries
                .front()
                .is_some_and(|entry| now_ms.saturating_sub(entry.at_ms) > LOG_AGE_MS)
        {
            self.entries.pop_front();
        }
    }

    /// Logs one action at its draft's time. A volume or seek change right after one of the same
    /// kind continues that drag: one action from where it started to where it ended, under a
    /// new id.
    pub(crate) fn record(&mut self, scope: Option<&AccountScope>, draft: Draft) {
        self.bind(scope);
        let mut action = draft.0;
        let now_ms = action.at_ms;
        if matches!(
            action.kind,
            ListenerActionKind::Volume | ListenerActionKind::Seek
        ) && let Some(last) = self.entries.back()
            && last.kind == action.kind
            && now_ms.saturating_sub(last.at_ms) <= DRAG_MS
        {
            let started = self.entries.pop_back().expect("checked above");
            action.during = started.during;
            action.position_ms = started.position_ms;
            action.volume_from = started.volume_from;
        }
        self.last_id += 1;
        action.id = self.last_id;
        self.entries.push_back(action);
        self.prune(now_ms);
    }

    /// The account's actions, oldest first. Subjects named only by video are filled in once the
    /// player knows the track, for example after it started playing.
    pub(crate) fn snapshot(
        &mut self,
        scope: Option<&AccountScope>,
        now_ms: u64,
        player: &PlayerObservedState,
        queue: Option<&QueueSnapshot>,
    ) -> Arc<[LoggedAction]> {
        self.bind(scope);
        self.prune(now_ms);
        for entry in &mut self.entries {
            if let Some(video) = &entry.subject_video
                && let Some(track) = find_track(video, player, queue)
            {
                entry.subject = Some(track);
                entry.subject_video = None;
            }
        }
        self.entries.iter().cloned().collect()
    }
}

/// The actions without a volume or seek drag that may still go on, so a prompt never hands out
/// a drag it would have to hand out again once it ends.
pub(crate) fn settled(actions: &[LoggedAction], now_ms: u64) -> &[LoggedAction] {
    match actions.split_last() {
        Some((last, before))
            if matches!(
                last.kind,
                ListenerActionKind::Volume | ListenerActionKind::Seek
            ) && now_ms.saturating_sub(last.at_ms) <= DRAG_MS =>
        {
            before
        }
        _ => actions,
    }
}

/// Actions after `after_id` (all when `None`) and no older than `since_ms`, newest first, at
/// most [`MAX_LISTENER_ACTIONS`], with the number left out.
pub(crate) fn newest(
    actions: &[LoggedAction],
    after_id: Option<u64>,
    since_ms: Option<u64>,
    now_ms: u64,
) -> (Vec<ListenerAction>, u32) {
    let matching = actions.iter().rev().filter(|action| {
        after_id.is_none_or(|after| action.id > after)
            && since_ms.is_none_or(|since| action.at_ms >= since)
    });
    let mut listed = Vec::new();
    let mut omitted = 0_u32;
    for action in matching {
        if listed.len() < MAX_LISTENER_ACTIONS {
            listed.push(action.view(now_ms));
        } else {
            omitted = omitted.saturating_add(1);
        }
    }
    (listed, omitted)
}

/// What a context read lists: the last ten minutes, newest first.
pub(crate) fn context_actions(actions: &[LoggedAction], now_ms: u64) -> Vec<ListenerAction> {
    newest(
        actions,
        None,
        Some(now_ms.saturating_sub(RECENT_WINDOW_MS)),
        now_ms,
    )
    .0
}

#[cfg(test)]
mod tests {
    use super::*;
    use ytm_protocol::{
        PlaybackTimeAnchor, PlayerCommandArgs, PlayerConnectionState, Track, TrackAvailability,
    };

    fn song(id: &str) -> Track {
        Track {
            video_id: VideoId::new(id).unwrap(),
            title: id.into(),
            artists: vec![],
            album: None,
            duration_ms: Some(200_000),
            cover: None,
            availability: TrackAvailability::Available,
            version_tag: None,
            is_liked: None,
        }
    }

    fn player(playing: &str) -> PlayerObservedState {
        let mut player: PlayerObservedState = serde_json::from_value(serde_json::json!({
            "connection": "ready",
            "playback": "playing",
            "timeAnchor": PlaybackTimeAnchor {
                position_ms: 0,
                duration_ms: None,
                playback_rate: 1.0,
                anchor_core_monotonic_ms: 0,
                extrapolating: false,
            },
            "volume": 0.5,
            "muted": false,
            "stoppedConfirmed": false,
        }))
        .unwrap();
        assert_eq!(player.connection, PlayerConnectionState::Ready);
        assert_eq!(player.playback, PlaybackStatus::Playing);
        player.current_track = Some(song(playing));
        player
    }

    fn volume(to: f64) -> PlayerCommandPayload {
        PlayerCommandPayload {
            command_id: ytm_protocol::CommandId::new("fixture-volume").unwrap(),
            op: PlayerOp::SetVolume,
            args: Some(PlayerCommandArgs {
                track_instance_id: None,
                position_ms: None,
                volume: Some(to),
                muted: None,
                target: None,
                shuffle: None,
                repeat: None,
                autoplay: None,
                enqueue_position: None,
                queue_entry_id: None,
                before_queue_entry_id: None,
                queue_revision: None,
                queue_filter_id: None,
            }),
        }
    }

    fn scope(name: &str) -> AccountScope {
        AccountScope::new(name).unwrap()
    }

    /// A like made in the app is logged with the song it was about; liking the song playing
    /// needs no separate subject.
    #[test]
    fn a_like_in_the_app_names_its_song_also_when_that_is_the_playing_one() {
        let player = player("fixture-playing");
        let mut log = ListenerLog::default();
        let account = scope("fixture-account");
        for video in ["fixture-other", "fixture-playing"] {
            let request = MutationRequest {
                kind: MutationKind::SetLike,
                video_id: VideoId::new(video).unwrap(),
                liked: Some(true),
                playlist_id: None,
                confirmed_by_user: true,
            };
            log.record(
                Some(&account),
                mutation_draft(&request, &player, 1_000, 30_000, None),
            );
        }
        // The other song becomes known once it plays.
        let later = self::player("fixture-other");
        let actions = log.snapshot(Some(&account), 2_000, &later, None);
        let views: Vec<_> = actions.iter().map(|action| action.view(2_000)).collect();
        assert_eq!(views[0].action, ListenerActionKind::Rate);
        assert_eq!(views[0].value.as_deref(), Some("liked"));
        assert_eq!(views[0].subject.as_ref().unwrap().title, "fixture-other");
        assert_eq!(views[0].position_ms, Some(30_000));
        // A like of the playing song names that song.
        assert_eq!(views[1].subject.as_ref().unwrap().title, "fixture-playing");
        assert_eq!(views[1].during.as_ref().unwrap().title, "fixture-playing");
    }

    /// Volume changes more than the drag window apart are separate actions, and another account
    /// never sees the log.
    #[test]
    fn separate_changes_stay_apart_and_the_log_belongs_to_one_account() {
        let player = player("fixture-playing");
        let mut log = ListenerLog::default();
        let account = scope("fixture-account");
        let draft = |at, to| {
            command_draft(
                &volume(to),
                &player,
                at,
                0,
                None,
                PlaybackStatus::Playing,
                None,
            )
            .unwrap()
        };
        log.record(Some(&account), draft(0, 0.2));
        log.record(Some(&account), draft(1_500, 0.3));
        log.record(Some(&account), draft(5_000, 0.9));
        let actions = log.snapshot(Some(&account), 5_000, &player, None);
        let volumes: Vec<_> = actions
            .iter()
            .map(|action| (action.volume_from, action.volume_to))
            .collect();
        assert_eq!(
            volumes,
            [(Some(50), Some(30)), (Some(50), Some(90))],
            "{actions:?}"
        );
        assert!(
            log.snapshot(Some(&scope("fixture-other")), 5_000, &player, None)
                .is_empty()
        );
        assert!(
            log.snapshot(Some(&account), 5_000, &player, None)
                .is_empty()
        );
    }
}
