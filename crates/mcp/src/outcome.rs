//! The text an agent reads back from a tool call. A write says one of four things: done, partly
//! done (naming which items), not done with nothing changed (with one way out), or unclear. The
//! receipt phases and other journal vocabulary stay in the structured content for debugging.

use rmcp::model::JsonObject;
use ytm_control_api::{
    AccountHandoffState, ControlError, ControlResponse, DjCue, DjPreferenceStance,
    DjPreferenceSubject, DjTrack, DjTrackOutcome, ErrorCode, InterruptResumeState, ItemPhase,
    ListenerAction, ListenerProfile, ListeningIntentBasis, ListeningIntentInstruction,
    ListeningIntentScope, MomentSectionStatus, MomentView, OperationPhase, OperationReceipt,
    OperationResource, ProfileSample, RetryDisposition, Script, UpcomingStatus,
};

use crate::MusicTool;

pub const UNCLEAR: &str = "Unclear whether this took effect; check the queue or state with ytm_get_context before trying again.";

/// Summarizes the result of a write tool, whether it is a receipt or an error.
pub fn write_text(tool: MusicTool, arguments: &JsonObject, response: &ControlResponse) -> String {
    match response {
        ControlResponse::Operation(receipt) => receipt_text(tool, arguments, receipt),
        ControlResponse::Error(error) => error_text(tool, arguments, error),
        // Writes answer with a receipt; anything else is not a result the agent can rely on.
        _ => UNCLEAR.to_owned(),
    }
}

/// A failed read: a way out, with the player's message when it is needed.
pub fn read_error_text(error: &ControlError) -> String {
    let hint = general_way_out(error.code);
    joined(
        "Could not read that",
        reason(error, hint.is_some()).as_deref(),
        hint,
    )
}

/// The player's message, which can carry journal vocabulary, is kept only when no way out
/// covers it or the agent needs its detail: which argument was wrong or why access was refused.
fn reason(error: &ControlError, has_hint: bool) -> Option<String> {
    (!has_hint
        || matches!(
            error.code,
            ErrorCode::PermissionDenied | ErrorCode::InvalidInput
        ))
    .then(|| sentence(&error.message))
}

/// `"{lead}: {reason} {hint}"`, leaving out what is missing.
fn joined(lead: &str, reason: Option<&str>, hint: Option<&str>) -> String {
    let mut text = match reason {
        Some(reason) => format!("{lead}: {reason}"),
        None => format!("{lead}."),
    };
    if let Some(hint) = hint {
        text.push(' ');
        text.push_str(hint);
    }
    text
}

fn error_text(tool: MusicTool, arguments: &JsonObject, error: &ControlError) -> String {
    if error.retry_disposition == RetryDisposition::QueryOperation
        || error.code == ErrorCode::ResultUnknown
    {
        return UNCLEAR.to_owned();
    }
    not_done(tool, arguments, error)
}

fn not_done(tool: MusicTool, arguments: &JsonObject, error: &ControlError) -> String {
    let hint = upcoming_way_out(tool, arguments, error.code)
        .or_else(|| way_out(tool, arguments, error.code));
    joined(
        "Not done; nothing changed",
        reason(error, hint.is_some()).as_deref(),
        hint.as_deref(),
    )
}

fn receipt_text(tool: MusicTool, arguments: &JsonObject, receipt: &OperationReceipt) -> String {
    use OperationPhase::*;
    match &receipt.resource {
        Some(OperationResource::Interruption {
            resume: InterruptResumeState::Pending,
            ..
        }) if matches!(receipt.phase, Acknowledged | Verified | Partial) => {
            return "Still running and will finish on its own: the new tracks play now and the \
                    interrupted track comes back afterwards."
                .to_owned();
        }
        Some(OperationResource::AccountHandoff {
            state: AccountHandoffState::AwaitingUser,
            ..
        }) if !matches!(receipt.phase, Rejected | Cancelled) => {
            return "Still running and will finish on its own: the request waits in the player \
                    until the listener signs in there."
                .to_owned();
        }
        _ => {}
    }
    match receipt.phase {
        Verified => match (&receipt.resource, tool) {
            (Some(OperationResource::Playlist { id }), MusicTool::CreatePlaylist) => format!(
                "Done: created playlist {id}. To add songs to it, call ytm_add_playlist_entries \
                 with createdByOperationId \"{}\".",
                receipt.operation_id
            ),
            (
                Some(OperationResource::ListeningIntent {
                    intent_key,
                    cleared,
                    expires_at_ms,
                    moment_revision,
                    ..
                }),
                MusicTool::SetListeningIntent,
            ) => {
                if *cleared {
                    format!(
                        "Done: cleared listening intent {intent_key} (momentRevision {moment_revision})."
                    )
                } else if let Some(exp) = expires_at_ms {
                    format!(
                        "Done: saved listening intent {intent_key} (expiresAtMs {exp}, momentRevision {moment_revision})."
                    )
                } else {
                    format!(
                        "Done: saved listening intent {intent_key} (momentRevision {moment_revision})."
                    )
                }
            }
            _ => "Done.".to_owned(),
        },
        Partial => partly_done(tool, arguments, receipt),
        Rejected | Cancelled if receipt.item_results.iter().any(went_through) => {
            partly_done(tool, arguments, receipt)
        }
        Rejected | Cancelled => {
            let error = receipt.error.clone().unwrap_or_else(|| {
                ControlError::new(
                    ErrorCode::Conflict,
                    RetryDisposition::NotRetryable,
                    "it was stopped before it started",
                )
            });
            ended_needs_new_operation(&error).unwrap_or_else(|| not_done(tool, arguments, &error))
        }
        Unknown | Accepted | Submitted | Acknowledged => UNCLEAR.to_owned(),
    }
}

/// Whether an item may have changed something.
fn went_through(item: &ytm_control_api::ItemResult) -> bool {
    !matches!(
        item.phase,
        ItemPhase::NotStarted | ItemPhase::Rejected | ItemPhase::Cancelled
    )
}

fn partly_done(tool: MusicTool, arguments: &JsonObject, receipt: &OperationReceipt) -> String {
    let hint = receipt
        .error
        .as_ref()
        .and_then(|error| way_out(tool, arguments, error.code));
    let items = &receipt.item_results;
    if items.is_empty() {
        let reason = receipt
            .error
            .as_ref()
            .and_then(|error| reason(error, hint.is_some()));
        return joined("Partly done", reason.as_deref(), hint.as_deref());
    }
    let mut text = {
        let videos = arguments
            .get("videoIds")
            .and_then(serde_json::Value::as_array);
        let name = |index: u32| {
            videos
                .and_then(|videos| videos.get(index as usize))
                .and_then(serde_json::Value::as_str)
                .map_or_else(|| format!("item {}", index + 1), str::to_owned)
        };
        let list = |phases: &[ItemPhase]| {
            items
                .iter()
                .filter(|item| phases.contains(&item.phase))
                .map(|item| name(item.input_index))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let done = items
            .iter()
            .filter(|item| item.phase == ItemPhase::Verified)
            .count();
        let mut text = format!("Partly done: {done} of {} items went through.", items.len());
        let failed = list(&[
            ItemPhase::NotStarted,
            ItemPhase::Rejected,
            ItemPhase::Cancelled,
        ]);
        if !failed.is_empty() {
            text.push_str(&format!(" Not done: {failed}."));
        }
        let unclear = list(&[
            ItemPhase::Submitted,
            ItemPhase::Acknowledged,
            ItemPhase::Unknown,
        ]);
        if !unclear.is_empty() {
            text.push_str(&format!(
                " Unclear: {unclear}; check the queue or state before trying them again."
            ));
        }
        // Every item went through, so only the player's reason says what was not done.
        if failed.is_empty()
            && unclear.is_empty()
            && let Some(reason) = receipt
                .error
                .as_ref()
                .and_then(|error| reason(error, hint.is_some()))
        {
            text.push(' ');
            text.push_str(&reason);
        }
        text
    };
    if let Some(hint) = hint {
        text.push(' ');
        text.push_str(&hint);
    }
    text
}

fn change_action(arguments: &JsonObject) -> Option<&str> {
    arguments
        .get("change")
        .or_else(|| arguments.get("edit"))
        .and_then(|change| change.get("action"))
        .and_then(serde_json::Value::as_str)
}

/// An operation the player ended without sending anything and that needs a new operation to
/// try again: a restart before it was sent, or a handoff the listener never completed. Its own
/// message says which; its operation ID only replays that ending.
fn ended_needs_new_operation(error: &ControlError) -> Option<String> {
    (error.code == ErrorCode::Unavailable
        && error.retry_disposition == RetryDisposition::NewPlanRequired)
        .then(|| {
            joined(
                "Not done; nothing changed",
                Some(&sentence(&error.message)),
                Some("Call again without operationId to start it as a new operation."),
            )
        })
}

/// Clearing or replacing what comes next that was refused outright (a radio queue, autoplay
/// suggestions playing); a partial clear already did what it could and gets no such hint.
fn upcoming_way_out(tool: MusicTool, arguments: &JsonObject, code: ErrorCode) -> Option<String> {
    let upcoming = matches!(tool, MusicTool::EditQueue)
        && matches!(
            change_action(arguments),
            Some("clearUpcoming" | "replaceUpcoming")
        );
    (upcoming
        && matches!(
            code,
            ErrorCode::CapabilityUnavailable | ErrorCode::PartialQueue
        ))
    .then(|| "Queue the songs with ytm_enqueue mode next instead.".to_owned())
}

/// One concrete way out of a refusal, or `None` when the player's message has to do.
fn way_out(tool: MusicTool, arguments: &JsonObject, code: ErrorCode) -> Option<String> {
    let change = change_action(arguments);
    match code {
        ErrorCode::PermissionDenied => Some(match tier(tool, arguments, change) {
            Some(tier) => format!(
                "This is a {tier}-tier change, which only the listener can allow for this \
                 client; carry on without it."
            ),
            None if change == Some("privacy") => "Only the listener can change a playlist's \
                privacy, in the player or on YouTube Music."
                .to_owned(),
            None => {
                "Only the listener can allow this for this client; carry on without it.".to_owned()
            }
        }),
        ErrorCode::NotEditable => {
            Some("That playlist cannot be edited; make your own with ytm_create_playlist.".into())
        }
        ErrorCode::IdempotencyConflict => Some("Call again without operationId.".into()),
        _ => general_way_out(code).map(str::to_owned),
    }
}

fn general_way_out(code: ErrorCode) -> Option<&'static str> {
    Some(match code {
        ErrorCode::StaleContext
        | ErrorCode::ContextExpired
        | ErrorCode::ContextChanged
        | ErrorCode::Conflict => "The player changed; read ytm_get_context and decide again.",
        ErrorCode::Busy | ErrorCode::RateLimited => "Try again in a moment.",
        ErrorCode::Unavailable => "The player may be closed or starting up; try again in a moment.",
        ErrorCode::AuthRequired => {
            "The listener needs to sign in to the player; ask them, or use ytm_account_handoff."
        }
        ErrorCode::InvalidInput => "Check the arguments against the tool's input schema.",
        _ => return None,
    })
}

/// The tier a write belongs to as the listener allows it: play, record or edit. Account handoff
/// and playlist privacy belong to none.
fn tier(tool: MusicTool, arguments: &JsonObject, change: Option<&str>) -> Option<&'static str> {
    match tool {
        MusicTool::Rating
        | MusicTool::ArtistSubscription
        | MusicTool::LibraryMembership
        | MusicTool::CreatePlaylist => Some("record"),
        MusicTool::Append if arguments.contains_key("createdByOperationId") => Some("record"),
        MusicTool::Append | MusicTool::UpdatePlaylist | MusicTool::ManageHistory => Some("edit"),
        MusicTool::EditPlaylist => (change != Some("privacy")).then_some("edit"),
        MusicTool::AccountHandoff => None,
        _ => Some("play"),
    }
}

/// The player's message as one sentence.
fn sentence(message: &str) -> String {
    let message = message.trim().trim_end_matches('.');
    let mut chars = message.chars();
    match chars.next() {
        Some(first) => format!("{}{}.", first.to_uppercase(), chars.as_str()),
        None => "The player gave no reason.".to_owned(),
    }
}

/// The prompt with a short listening context. The structured content keeps the full cue.
pub fn cue_text(cue: &DjCue) -> String {
    let mut lines = vec![cue.prompt.clone(), String::new()];
    if let Some(now) = &cue.now_playing {
        let left = now
            .remaining_ms
            .map(|ms| format!(", {} left", clock(ms)))
            .unwrap_or_default();
        let state = serde_json::to_value(now.playback)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .filter(|state| state != "playing")
            .map(|state| format!(", {state}"))
            .unwrap_or_default();
        lines.push(format!(
            "Now playing: {}{left}{state}.",
            track_label(&now.track)
        ));
    }
    lines.push(if cue.upcoming == UpcomingStatus::Unknown {
        "Up next: not known right now.".to_owned()
    } else if cue.up_next.is_empty() {
        "Up next: nothing queued.".to_owned()
    } else {
        let next: Vec<_> = cue.up_next.iter().take(5).map(track_label).collect();
        let more = if cue.more_queued || cue.up_next.len() > 5 {
            " and more"
        } else {
            ""
        };
        format!("Up next: {}{more}.", next.join("; "))
    });
    if let Some(official) = &cue.official_up_next {
        lines.push(official_text(
            official.status,
            &official.tracks,
            official.more,
            official.filter.as_deref(),
        ));
    }
    if !cue.recent.is_empty() {
        let recent: Vec<_> = cue
            .recent
            .iter()
            .take(4)
            .map(|item| {
                let outcome = match item.outcome {
                    DjTrackOutcome::Finished => "finished",
                    DjTrackOutcome::SkippedByListener => "skipped by the listener",
                    DjTrackOutcome::SkippedByAgent => "skipped by you",
                    DjTrackOutcome::Skipped => "skipped",
                };
                format!("{} ({outcome})", track_label(&item.track))
            })
            .collect();
        lines.push(format!("Recently, newest first: {}.", recent.join("; ")));
    }
    lines.push(match cue.listener.last_interaction_ago_ms {
        Some(ms) => format!("The listener has been quiet for {}.", minutes(ms)),
        None => "The listener has not done or said anything yet.".to_owned(),
    });
    lines.extend(listener_actions_text(
        &cue.listener.recent_actions,
        cue.listener.actions_omitted,
    ));
    if cue.preferences_unavailable {
        lines.push(
            "Recorded tastes: unavailable right now; keep any previously known tastes.".to_owned(),
        );
    } else if !cue.preferences_unchanged && cue.preferences.is_empty() {
        lines.push("Recorded tastes: none (all earlier taste notes were cleared).".to_owned());
    } else if !cue.preferences.is_empty() {
        let tastes: Vec<_> = cue
            .preferences
            .iter()
            .map(|preference| {
                let stance = match preference.stance {
                    DjPreferenceStance::Prefer => "prefers",
                    DjPreferenceStance::Avoid => "avoids",
                };
                let subject = match &preference.subject {
                    DjPreferenceSubject::Video { video_id, title } => {
                        title.clone().unwrap_or_else(|| video_id.to_string())
                    }
                    DjPreferenceSubject::Artist { artist_id, name } => {
                        name.clone().unwrap_or_else(|| artist_id.to_string())
                    }
                };
                match &preference.note {
                    Some(note) => format!("{stance} {subject} ({note})"),
                    None => format!("{stance} {subject}"),
                }
            })
            .collect();
        let paging = cue
            .preferences_total
            .filter(|total| usize::try_from(*total).unwrap_or(usize::MAX) > cue.preferences.len())
            .map(|total| {
                format!(
                    " (showing {} of {total}; page more with ytm_get_settings section djPreferences)",
                    cue.preferences.len()
                )
            })
            .unwrap_or_default();
        lines.push(format!("Recorded tastes: {}.{paging}", tastes.join("; ")));
    }
    if let Some(moment) = &cue.moment {
        lines.extend(moment_text_lines(moment));
    }
    lines.join("\n")
}

pub fn moment_text_lines(moment: &MomentView) -> Vec<String> {
    let mut out = Vec::new();
    let env = &moment.environment;
    match env.status {
        MomentSectionStatus::NotShared => {
            out.push("Moment environment: not shared.".to_owned());
        }
        MomentSectionStatus::Unavailable => {
            out.push("Moment environment: unavailable right now.".to_owned());
        }
        MomentSectionStatus::Shared => {
            let mut parts = Vec::new();
            if let Some(t) = &env.time {
                let day_part = match serde_json::to_value(t.day_part)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                {
                    Some(s) => s,
                    None => "day".to_owned(),
                };
                parts.push(format!(
                    "{} {day_part} ({}, {})",
                    t.weekday, t.local_date, t.time_zone
                ));
            }
            if !env.occasions.is_empty() {
                let occ: Vec<_> = env
                    .occasions
                    .iter()
                    .map(|o| {
                        let phase = serde_json::to_value(o.phase)
                            .ok()
                            .and_then(|v| v.as_str().map(str::to_owned))
                            .unwrap_or_else(|| "day".to_owned());
                        format!("{} ({phase}, id={})", o.label, o.id)
                    })
                    .collect();
                parts.push(format!("occasions: {}", occ.join(", ")));
            }
            if let Some(city) = &env.scene_city {
                parts.push(format!(
                    "scene city: {} (a mood the listener picked, not where they are; the player gives no weather)",
                    city.label
                ));
            }
            if parts.is_empty() {
                out.push("Moment environment: shared (no active signals).".to_owned());
            } else {
                out.push(format!("Moment environment: {}.", parts.join("; ")));
            }
        }
    }

    let intents = &moment.listening_intents;
    match intents.status {
        MomentSectionStatus::NotShared => {
            out.push("Listening intents: not shared.".to_owned());
        }
        MomentSectionStatus::Unavailable => {
            out.push("Listening intents: unavailable right now.".to_owned());
        }
        MomentSectionStatus::Shared => {
            if intents.items.is_empty() {
                out.push(format!(
                    "Listening intents: none active (momentRevision {}).",
                    moment.revision
                ));
            } else {
                let summary: Vec<_> = intents
                    .items
                    .iter()
                    .map(|item| {
                        let scope = match item.scope {
                            ListeningIntentScope::Today => "today",
                            ListeningIntentScope::Session => "session",
                            ListeningIntentScope::Until => "until",
                        };
                        let basis = match item.basis {
                            ListeningIntentBasis::OwnerInput => "owner",
                            ListeningIntentBasis::UserRequestReportedByAssistant => "listener",
                            ListeningIntentBasis::AssistantInference => "inferred",
                        };
                        let val = match &item.instruction {
                            ListeningIntentInstruction::Vocals { mode } => {
                                serde_json::to_value(mode)
                                    .ok()
                                    .and_then(|v| v.as_str().map(|s| format!("vocals={s}")))
                                    .unwrap_or_else(|| "vocals".into())
                            }
                            ListeningIntentInstruction::Energy { level } => {
                                serde_json::to_value(level)
                                    .ok()
                                    .and_then(|v| v.as_str().map(|s| format!("energy={s}")))
                                    .unwrap_or_else(|| "energy".into())
                            }
                            ListeningIntentInstruction::Activity { kind } => {
                                serde_json::to_value(kind)
                                    .ok()
                                    .and_then(|v| v.as_str().map(|s| format!("activity={s}")))
                                    .unwrap_or_else(|| "activity".into())
                            }
                            ListeningIntentInstruction::MusicBrief { brief } => {
                                format!("brief=\"{brief}\"")
                            }
                            ListeningIntentInstruction::DismissTheme { theme_id } => {
                                format!("dismissed={theme_id}")
                            }
                        };
                        format!(
                            "{val} ({scope}, {basis}, key={}, {})",
                            item.intent_key,
                            expires_in(item.expires_at_ms)
                        )
                    })
                    .collect();
                out.push(format!(
                    "Listening intents (momentRevision {}): {}.",
                    moment.revision,
                    summary.join("; ")
                ));
            }
        }
    }
    out
}

/// "ends in 2 h 15 min" from the adapter's own clock, for clients that read only text.
fn expires_in(expires_at_ms: u64) -> String {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    let minutes = expires_at_ms.saturating_sub(now_ms).div_ceil(60_000);
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("ends in {m} min"),
        (h, 0) => format!("ends in {h} h"),
        (h, m) => format!("ends in {h} h {m} min"),
    }
}

/// One compact line on what YouTube Music plays on its own after the queued tracks, naming at
/// most three suggestions; `more` says more are loaded than `tracks` holds, so observed
/// suggestions cut to none by a zero limit still read as suggestions.
pub fn official_text(
    status: impl serde::Serialize,
    tracks: &[DjTrack],
    more: bool,
    filter: Option<&str>,
) -> String {
    const NAMED: usize = 3;
    let status = serde_json::to_value(status).unwrap_or_default();
    let filter = filter
        .map(|label| format!(" (filter: {label})"))
        .unwrap_or_default();
    let what = match status.as_str() {
        Some("observed") if !tracks.is_empty() => {
            let named: Vec<_> = tracks.iter().take(NAMED).map(track_label).collect();
            let more = if more || tracks.len() > NAMED {
                " and more"
            } else {
                ""
            };
            format!("{}{more}{filter}", named.join("; "))
        }
        Some("observed") if more => format!("its own suggestions (not listed){filter}"),
        Some("off") => "autoplay is off".to_owned(),
        Some("radio") => "a radio that refills itself".to_owned(),
        Some("loading") => "loading".to_owned(),
        _ => "no suggestions loaded".to_owned(),
    };
    format!("Then YouTube Music: {what}.")
}

/// Listener actions a text line names; the rest are counted as "and N more".
const ACTIONS_NAMED: usize = 6;

/// One line on what the listener did by hand, newest first, naming at most [`ACTIONS_NAMED`];
/// `omitted` counts older actions the reply itself left out. `None` when there are none.
pub fn listener_actions_text(actions: &[ListenerAction], omitted: u32) -> Option<String> {
    if actions.is_empty() && omitted == 0 {
        return None;
    }
    let named: Vec<_> = actions
        .iter()
        .take(ACTIONS_NAMED)
        .map(action_words)
        .collect();
    let more = u32::try_from(actions.len().saturating_sub(named.len()))
        .unwrap_or(u32::MAX)
        .saturating_add(omitted);
    let tail = match (named.is_empty(), more) {
        (_, 0) => String::new(),
        (true, more) => format!("{more} actions"),
        (false, more) => format!("; and {more} more"),
    };
    Some(format!(
        "The listener's own actions, newest first: {}{tail}.",
        named.join("; ")
    ))
}

/// One action in plain words, with the facts that matter and how long ago when over a minute.
fn action_words(action: &ListenerAction) -> String {
    use ytm_control_api::ListenerActionKind as Kind;
    let playing = action
        .during
        .as_ref()
        .map(|track| format!("\"{}\"", one_line(&track.title)));
    let at = action.position_ms.map(position);
    // Without a subject the player cannot name the track yet, so it is "a track", with what was
    // playing then.
    let about = action
        .subject
        .as_ref()
        .map(quoted)
        .unwrap_or_else(|| "a track".to_owned());
    let unknown_subject = match (&action.subject, &playing) {
        (None, Some(track)) => format!(" while {track} played"),
        _ => String::new(),
    };
    let value = action.value.as_deref().map(one_line);
    let words = match action.action {
        Kind::Play => match &playing {
            Some(track) => format!("pressed play on {track}"),
            None => "pressed play".to_owned(),
        },
        Kind::Pause => match (&playing, &at) {
            (Some(track), Some(at)) => format!("paused {track} at {at}"),
            (Some(track), None) => format!("paused {track}"),
            _ => "paused".to_owned(),
        },
        Kind::Stop => "stopped playback".to_owned(),
        Kind::Next => match (&playing, action.position_ms) {
            (Some(track), Some(ms)) => format!("skipped {track} {} in", position(ms)),
            (Some(track), None) => format!("skipped {track}"),
            _ => "skipped to the next track".to_owned(),
        },
        Kind::Previous => match (&playing, &at) {
            (Some(track), Some(at)) => format!("pressed previous during {track} at {at}"),
            (Some(track), None) => format!("pressed previous during {track}"),
            _ => "pressed previous".to_owned(),
        },
        Kind::Seek => {
            let to = action
                .seek_to_ms
                .map(|ms| format!(" to {}", position(ms)))
                .unwrap_or_default();
            let from = at
                .as_ref()
                .map(|at| format!(" from {at}"))
                .unwrap_or_default();
            let track = playing
                .as_ref()
                .map(|track| format!(" in {track}"))
                .unwrap_or_default();
            format!("moved the position{from}{to}{track}")
        }
        Kind::Volume => match (action.volume_from, action.volume_to) {
            (Some(from), Some(to)) if to < from => {
                format!("turned the volume down from {from} to {to}")
            }
            (Some(from), Some(to)) if to > from => {
                format!("turned the volume up from {from} to {to}")
            }
            (_, Some(to)) => format!("set the volume to {to}"),
            _ => "changed the volume".to_owned(),
        },
        Kind::Mute => match value.as_deref() {
            Some("on") => "muted".to_owned(),
            Some("off") => "unmuted".to_owned(),
            _ => "changed mute".to_owned(),
        },
        Kind::Shuffle => setting("shuffle", value.as_deref()),
        Kind::Autoplay => setting("autoplay", value.as_deref()),
        Kind::Repeat => match value.as_deref() {
            Some(mode) => format!("set repeat to {mode}"),
            None => "changed repeat".to_owned(),
        },
        Kind::PlayTrack => format!("started {about}{unknown_subject}"),
        Kind::PlayCollection => match &action.subject {
            Some(track) => format!("started an album or playlist from {}", quoted(track)),
            None => format!("started an album or playlist{unknown_subject}"),
        },
        Kind::PickFromQueue => format!("jumped to {about} in the queue{unknown_subject}"),
        Kind::AddToQueue => format!("queued {about}{unknown_subject}"),
        Kind::RemoveFromQueue => format!("removed {about} from the queue{unknown_subject}"),
        Kind::ReorderQueue => format!("moved {about} in the queue{unknown_subject}"),
        Kind::ClearUpcoming => "cleared what was queued next".to_owned(),
        Kind::UpNextFilter => match value.as_deref() {
            Some(label) => format!("chose the Up Next filter {label}"),
            None => "chose another Up Next filter".to_owned(),
        },
        Kind::Rate => match value.as_deref() {
            Some("liked") => format!("liked {about}{unknown_subject}"),
            Some("disliked") => format!("disliked {about}{unknown_subject}"),
            Some("neutral") => format!("cleared the rating of {about}{unknown_subject}"),
            Some(other) => format!("rated {about} {other}{unknown_subject}"),
            None => format!("rated {about}{unknown_subject}"),
        },
        Kind::AddToPlaylist => format!("added {about} to a playlist{unknown_subject}"),
        Kind::UndoAddToPlaylist => {
            format!("took {about} back out of the playlist it was just added to{unknown_subject}")
        }
    };
    if action.ago_ms >= 60_000 {
        format!("{words} ({} ago)", minutes(action.ago_ms))
    } else {
        words
    }
}

fn setting(name: &str, value: Option<&str>) -> String {
    match value {
        Some(value) => format!("turned {name} {value}"),
        None => format!("changed {name}"),
    }
}

/// A position in a track: seconds under a minute, otherwise minutes and seconds.
fn position(ms: u64) -> String {
    if ms < 60_000 {
        format!("{} s", ms / 1000)
    } else {
        clock(ms)
    }
}

/// A track as `"title" by artists`, on one line.
fn quoted(track: &DjTrack) -> String {
    let title = format!("\"{}\"", track.title);
    one_line(&if track.artists.is_empty() {
        title
    } else {
        format!("{title} by {}", track.artists.join(", "))
    })
}

/// Page-supplied text with its line breaks turned into spaces, so a summary stays one line.
fn one_line(text: &str) -> String {
    text.replace(['\n', '\r'], " ")
}

/// A one-line summary of the listener profile: what it was built from, who appears most, the
/// playlists by name, the scripts of what they listen to and what could not be read.
pub fn profile_text(profile: &ListenerProfile) -> String {
    const ARTISTS_NAMED: usize = 5;
    const PLAYLISTS_NAMED: usize = 10;
    let missing = |part: &str| profile.unavailable.iter().any(|name| name == part);
    let read = |sample: &ProfileSample, noun: &str| match sample.total {
        Some(total) if total > sample.sampled => format!("{} of {total} {noun}", sample.sampled),
        Some(_) => format!("{} {noun}", sample.sampled),
        None => format!("the first {} {noun} (more exist)", sample.sampled),
    };
    let mut sources = Vec::new();
    if !missing("liked") {
        sources.push(read(&profile.liked, "liked songs"));
    }
    if !missing("history") {
        sources.push(read(&profile.history, "plays on this player"));
    }
    let mut out = vec![if sources.is_empty() {
        "Listener profile with neither liked songs nor plays to go on.".to_owned()
    } else {
        format!("Listener profile from {}.", sources.join(" and "))
    }];
    let artists = |sample: &ProfileSample| -> Vec<String> {
        sample
            .top_artists
            .iter()
            .take(ARTISTS_NAMED)
            .map(|artist| format!("{} ({})", one_line(&artist.name), artist.tracks))
            .collect()
    };
    for (sample, label) in [
        (&profile.liked, "Most liked artists"),
        (&profile.history, "Most played artists"),
    ] {
        let named = artists(sample);
        if !named.is_empty() {
            out.push(format!("{label}: {}.", named.join(", ")));
        }
    }
    if !missing("playlists") {
        if profile.playlists.is_empty() {
            out.push("No playlists.".to_owned());
        } else {
            let named: Vec<_> = profile
                .playlists
                .iter()
                .take(PLAYLISTS_NAMED)
                .map(|playlist| match playlist.track_count {
                    Some(count) => format!("\"{}\" ({count})", one_line(&playlist.title)),
                    None => format!("\"{}\"", one_line(&playlist.title)),
                })
                .collect();
            let more = if profile.more_playlists || profile.playlists.len() > named.len() {
                " and more"
            } else {
                ""
            };
            out.push(format!("Playlists: {}{more}.", named.join(", ")));
        }
    }
    if profile.scripts_sampled > 0 && !profile.scripts.is_empty() {
        let shares: Vec<_> = profile
            .scripts
            .iter()
            .map(|share| match share.percent {
                0 => format!("{} under 1%", script_name(share.script)),
                percent => format!("{} {percent}%", script_name(share.script)),
            })
            .collect();
        out.push(format!(
            "Scripts of what they listen to: {} (from {} tracks).",
            shares.join(", "),
            profile.scripts_sampled
        ));
    }
    if !missing("tasteNotes") {
        out.push(match profile.taste_notes {
            0 => "No taste notes are stored.".to_owned(),
            1 => "1 taste note is stored.".to_owned(),
            count => format!("{count} taste notes are stored."),
        });
    }
    if !profile.unavailable.is_empty() {
        let parts: Vec<_> = profile
            .unavailable
            .iter()
            .map(|part| match part.as_str() {
                "liked" => "liked songs",
                "history" => "play history",
                "playlists" => "playlists",
                "tasteNotes" => "taste notes",
                other => other,
            })
            .collect();
        out.push(format!("Unavailable this time: {}.", parts.join(", ")));
    }
    out.join(" ")
}

fn script_name(script: Script) -> &'static str {
    match script {
        Script::Latin => "Latin",
        Script::Han => "Han",
        Script::Kana => "Kana",
        Script::Hangul => "Hangul",
        Script::Cyrillic => "Cyrillic",
        Script::Arabic => "Arabic",
        Script::Devanagari => "Devanagari",
        Script::Thai => "Thai",
        Script::Hebrew => "Hebrew",
        Script::Greek => "Greek",
        Script::Other => "other or none",
    }
}

fn track_label(track: &DjTrack) -> String {
    if track.artists.is_empty() {
        track.title.clone()
    } else {
        format!("{} by {}", track.title, track.artists.join(", "))
    }
}

fn clock(ms: u64) -> String {
    let seconds = ms / 1000;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn minutes(ms: u64) -> String {
    match ms / 60_000 {
        0 => "under a minute".to_owned(),
        1 => "1 minute".to_owned(),
        minutes => format!("{minutes} minutes"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn receipt(
        phase: OperationPhase,
        items: &[ItemPhase],
        error: Option<ErrorCode>,
    ) -> OperationReceipt {
        OperationReceipt {
            operation_id: "auto-1".into(),
            account_scope: None,
            revision: 3,
            phase,
            resource: None,
            verification: None,
            item_results: (0..)
                .zip(items)
                .map(|(input_index, phase)| ytm_control_api::ItemResult {
                    input_index,
                    phase: *phase,
                    resource_entry_id: None,
                    error: None,
                })
                .collect(),
            scheduling_stopped: true,
            settled: true,
            retry_disposition: RetryDisposition::NotRetryable,
            error: error.map(|code| {
                ControlError::new(
                    code,
                    RetryDisposition::NotRetryable,
                    "queue continuation is not finite",
                )
            }),
            removed: vec![],
        }
    }

    fn text(tool: MusicTool, arguments: serde_json::Value, response: ControlResponse) -> String {
        write_text(tool, arguments.as_object().unwrap(), &response)
    }

    #[test]
    fn a_partial_enqueue_names_the_tracks_that_did_not_go_through() {
        use ItemPhase::*;
        let partial = receipt(
            OperationPhase::Partial,
            &[Verified, Verified, Rejected],
            None,
        );
        let summary = text(
            MusicTool::Enqueue,
            json!({"mode": "next", "videoIds": ["A", "B", "C"]}),
            ControlResponse::Operation(partial),
        );
        assert_eq!(
            summary,
            "Partly done: 2 of 3 items went through. Not done: C."
        );
    }

    #[test]
    fn a_cancelled_write_with_sent_items_is_partly_done() {
        use ItemPhase::*;
        let cancelled = receipt(OperationPhase::Cancelled, &[Verified, Cancelled], None);
        let summary = text(
            MusicTool::Append,
            json!({"videoIds": ["A", "B"]}),
            ControlResponse::Operation(cancelled),
        );
        assert_eq!(
            summary,
            "Partly done: 1 of 2 items went through. Not done: B."
        );
    }

    #[test]
    fn a_partial_clear_or_replacement_says_why_without_pointing_away() {
        let more = |items: &[ItemPhase]| OperationReceipt {
            error: Some(ControlError::new(
                ErrorCode::PartialQueue,
                RetryDisposition::NewPlanRequired,
                "the loaded upcoming entries were cleared; more may load later",
            )),
            ..receipt(OperationPhase::Partial, items, None)
        };
        let cleared = text(
            MusicTool::EditQueue,
            json!({"change": {"action": "clearUpcoming"}}),
            ControlResponse::Operation(more(&[])),
        );
        assert_eq!(
            cleared,
            "Partly done: The loaded upcoming entries were cleared; more may load later."
        );
        let replaced = text(
            MusicTool::EditQueue,
            json!({"change": {"action": "replaceUpcoming"}, "videoIds": ["A", "B"]}),
            ControlResponse::Operation(more(&[ItemPhase::Verified, ItemPhase::Verified])),
        );
        assert_eq!(
            replaced,
            "Partly done: 2 of 2 items went through. The loaded upcoming entries were cleared; \
             more may load later."
        );
    }

    #[test]
    fn a_refused_clear_upcoming_points_to_enqueue_next() {
        for code in [ErrorCode::CapabilityUnavailable, ErrorCode::PartialQueue] {
            let refused = receipt(OperationPhase::Rejected, &[], Some(code));
            let summary = text(
                MusicTool::EditQueue,
                json!({"change": {"action": "clearUpcoming"}}),
                ControlResponse::Operation(refused),
            );
            assert_eq!(
                summary,
                "Not done; nothing changed. Queue the songs with ytm_enqueue mode next instead."
            );
        }
        // The hint belongs to clearing and replacing, not to every unavailable capability.
        let refused = receipt(
            OperationPhase::Rejected,
            &[],
            Some(ErrorCode::CapabilityUnavailable),
        );
        let summary = text(
            MusicTool::Seek,
            json!({"positionMs": 1}),
            ControlResponse::Operation(refused),
        );
        assert!(!summary.contains("ytm_enqueue"), "{summary}");
    }

    #[test]
    fn a_permission_refusal_names_the_tier_and_not_the_grant() {
        let cases = [
            (MusicTool::LibraryMembership, json!({}), "record-tier"),
            (
                MusicTool::Append,
                json!({"createdByOperationId": "x"}),
                "record-tier",
            ),
            (MusicTool::Append, json!({}), "edit-tier"),
            (MusicTool::ManageHistory, json!({}), "edit-tier"),
            (MusicTool::Enqueue, json!({}), "play-tier"),
            (
                MusicTool::EditPlaylist,
                json!({"edit": {"action": "privacy"}}),
                "privacy",
            ),
        ];
        for (tool, arguments, expected) in cases {
            let error = ControlError::new(
                ErrorCode::PermissionDenied,
                RetryDisposition::NotRetryable,
                "this registration does not grant the requested music operation",
            );
            let summary = text(tool, arguments, ControlResponse::Error(error));
            assert!(
                summary.starts_with("Not done; nothing changed"),
                "{summary}"
            );
            assert!(summary.contains(expected), "{summary}");
            assert!(!summary.contains("libraryMembership"), "{summary}");
        }
    }

    #[test]
    fn unsettled_or_lost_writes_are_unclear_and_other_codes_keep_their_message() {
        let pending = receipt(OperationPhase::Acknowledged, &[], None);
        let summary = text(
            MusicTool::Playback,
            json!({}),
            ControlResponse::Operation(pending),
        );
        assert_eq!(summary, UNCLEAR);
        let lost = ControlError::new(
            ErrorCode::Unavailable,
            RetryDisposition::QueryOperation,
            "the response did not arrive; its operationId is auto-1",
        );
        let summary = text(MusicTool::Playback, json!({}), ControlResponse::Error(lost));
        assert_eq!(summary, UNCLEAR);
        let storage = ControlError::new(
            ErrorCode::StorageError,
            RetryDisposition::NotRetryable,
            "disk full",
        );
        let summary = text(
            MusicTool::Playback,
            json!({}),
            ControlResponse::Error(storage),
        );
        assert_eq!(summary, "Not done; nothing changed: Disk full.");
    }

    #[test]
    fn a_write_ended_by_a_restart_asks_for_a_new_operation() {
        let ended = |phase, items: &[ItemPhase], message: &str| {
            ControlResponse::Operation(OperationReceipt {
                error: Some(ControlError::new(
                    ErrorCode::Unavailable,
                    RetryDisposition::NewPlanRequired,
                    message,
                )),
                ..receipt(phase, items, None)
            })
        };
        let unsent = ended(
            OperationPhase::Cancelled,
            &[ItemPhase::Cancelled, ItemPhase::Cancelled],
            "the player restarted before this operation was sent; start a new operation if \
             still needed",
        );
        let summary = text(
            MusicTool::Enqueue,
            json!({"mode": "next", "videoIds": ["A", "B"], "operationId": "auto-1"}),
            unsent,
        );
        assert_eq!(
            summary,
            "Not done; nothing changed: The player restarted before this operation was sent; start \
             a new operation if still needed. Call again without operationId to start it as a new \
             operation."
        );
        let mut handoff = ended(
            OperationPhase::Rejected,
            &[],
            "the player restarted while the account handoff waited for the user; nothing was sent",
        );
        // Recovery keeps the waiting handoff resource; the ended wait is not still running.
        if let ControlResponse::Operation(receipt) = &mut handoff {
            receipt.resource = Some(OperationResource::AccountHandoff {
                goal: ytm_control_api::AccountHandoffGoal::SwitchAccount,
                state: AccountHandoffState::AwaitingUser,
            });
        }
        assert_eq!(
            text(MusicTool::AccountHandoff, json!({}), handoff),
            "Not done; nothing changed: The player restarted while the account handoff waited for \
             the user; nothing was sent. Call again without operationId to start it as a new \
             operation."
        );
        // A handoff that simply timed out in a running player says so, not that it restarted.
        let timed_out = ended(
            OperationPhase::Rejected,
            &[],
            "the account change was not completed in the player within 30 minutes",
        );
        assert_eq!(
            text(MusicTool::AccountHandoff, json!({}), timed_out),
            "Not done; nothing changed: The account change was not completed in the player within \
             30 minutes. Call again without operationId to start it as a new operation."
        );
        // A live outage keeps its plain advice.
        let down = ControlError::new(
            ErrorCode::Unavailable,
            RetryDisposition::SafeReadRetry,
            "the music catalog is unavailable",
        );
        assert_eq!(
            text(MusicTool::Enqueue, json!({}), ControlResponse::Error(down)),
            "Not done; nothing changed. The player may be closed or starting up; try again in a \
             moment."
        );
    }

    #[test]
    fn a_stale_refusal_says_what_to_do_without_journal_words() {
        let stale = ControlError::new(
            ErrorCode::StaleContext,
            RetryDisposition::NotRetryable,
            "the account, playback bridge or queue revision no longer matches",
        );
        let summary = text(MusicTool::Enqueue, json!({}), ControlResponse::Error(stale));
        assert_eq!(
            summary,
            "Not done; nothing changed. The player changed; read ytm_get_context and decide again."
        );
    }

    #[test]
    fn a_waiting_interruption_will_finish_on_its_own() {
        let mut playing = receipt(OperationPhase::Verified, &[], None);
        playing.resource = Some(OperationResource::Interruption {
            policy: ytm_control_api::InterruptResumePolicy::ResumeInterrupted,
            interrupted_entry_id: serde_json::from_value(json!("entry-1")).unwrap(),
            interrupted_video_id: serde_json::from_value(json!("video-1")).unwrap(),
            resume_position_ms: Some(1),
            resume_entry_id: None,
            resume: InterruptResumeState::Pending,
        });
        let summary = text(
            MusicTool::Interrupt,
            json!({}),
            ControlResponse::Operation(playing),
        );
        assert!(
            summary.starts_with("Still running and will finish on its own"),
            "{summary}"
        );
    }

    #[test]
    fn a_prompt_reads_as_a_short_context_instead_of_json() {
        let track = |id: &str, title: &str| {
            json!({"videoId": id, "title": title, "artists": ["Band"], "album": null,
                   "durationMs": 200000})
        };
        let cue: DjCue = serde_json::from_value(json!({
            "seq": 1, "kind": "trackEnding", "reasons": ["routine"], "ageMs": 0,
            "prompt": "The current track ends soon.",
            "nowPlaying": {"track": track("a", "Alpha"), "playback": "playing",
                           "positionMs": 155000, "remainingMs": 45000},
            "upcoming": "known", "upNext": [track("b", "Beta")], "moreQueued": false,
            "recent": [{"track": track("c", "Gamma"), "listenedMs": 30000,
                        "outcome": "skippedByListener", "endedAgoMs": 200000}],
            "listener": {"lastInteraction": "playerControl", "lastInteractionAgoMs": 720000,
                         "lastSpokenToAgoMs": null, "unansweredCheckIns": 0},
            "chattiness": "normal", "preferences": [], "preferencesUnchanged": true,
        }))
        .unwrap();
        assert_eq!(
            cue_text(&cue),
            "The current track ends soon.\n\n\
             Now playing: Alpha by Band, 0:45 left.\n\
             Up next: Beta by Band.\n\
             Recently, newest first: Gamma by Band (skipped by the listener).\n\
             The listener has been quiet for 12 minutes."
        );
    }

    #[test]
    fn cue_text_adds_one_line_for_youtube_musics_own_continuation() {
        let song = |id: &str, artist: &str| json!({"videoId": id, "title": id, "artists": [artist], "album": null, "durationMs": null});
        let cue = |official: serde_json::Value| -> String {
            let cue: DjCue = serde_json::from_value(json!({
                "seq": 3, "kind": "trackEnding", "reasons": [], "ageMs": 0, "prompt": "Ends soon.",
                "nowPlaying": null, "upcoming": "known", "upNext": [], "moreQueued": false,
                "officialUpNext": official, "recent": [],
                "listener": {"lastInteraction": null, "lastInteractionAgoMs": null,
                             "lastSpokenToAgoMs": null, "unansweredCheckIns": 0},
                "chattiness": "normal", "preferences": [], "preferencesUnchanged": true
            }))
            .unwrap();
            cue_text(&cue)
        };
        let observed = cue(json!({
            "status": "observed",
            "tracks": [song("autoAAAAAAA", "X"), song("autoBBBBBBB", "Y")],
            "more": true, "filter": "Familiar"
        }));
        assert!(
            observed.contains(
                "Up next: nothing queued.\nThen YouTube Music: autoAAAAAAA by X; autoBBBBBBB by Y \
                 and more (filter: Familiar)."
            ),
            "{observed}"
        );
        let off = cue(json!({"status": "off", "tracks": [], "more": false}));
        assert!(
            off.contains("Then YouTube Music: autoplay is off."),
            "{off}"
        );
        let radio = cue(json!({"status": "radio", "tracks": [], "more": false}));
        assert!(
            radio.contains("Then YouTube Music: a radio that refills itself."),
            "{radio}"
        );
        assert!(!cue(serde_json::Value::Null).contains("Then YouTube Music"));
    }

    #[test]
    fn cue_text_distinguishes_cleared_unavailable_and_paged_tastes_plus_moment() {
        let base = json!({
            "seq": 2, "kind": "checkIn", "reasons": ["routine"], "ageMs": 0,
            "prompt": "Check in.",
            "nowPlaying": null, "upcoming": "known", "upNext": [], "moreQueued": false, "recent": [],
            "listener": {"lastInteraction": null, "lastInteractionAgoMs": null,
                         "lastSpokenToAgoMs": null, "unansweredCheckIns": 0},
            "chattiness": "normal", "preferences": [], "preferencesUnchanged": false
        });

        let cleared: DjCue = serde_json::from_value(base.clone()).unwrap();
        assert!(
            cue_text(&cleared)
                .contains("Recorded tastes: none (all earlier taste notes were cleared).")
        );

        let mut unavail_val = base.clone();
        unavail_val["preferencesUnavailable"] = json!(true);
        unavail_val["preferencesUnchanged"] = json!(true);
        let unavail: DjCue = serde_json::from_value(unavail_val).unwrap();
        assert!(
            cue_text(&unavail).contains(
                "Recorded tastes: unavailable right now; keep any previously known tastes."
            )
        );

        let mut moment_val = base;
        moment_val["moment"] = json!({
            "revision": 5,
            "environment": {
                "status": "shared",
                "revision": "cfg-1-env-1",
                "time": { "localDate": "2026-12-24", "weekday": "thursday", "dayPart": "evening", "timeZone": "America/New_York" },
                "occasions": [{ "id": "occasion:us:christmas-day", "label": "Christmas Eve", "phase": "eve", "kind": "public", "localDate": "2026-12-24", "providerVersion": "date-holidays@3.39.2" }],
                "sceneCity": { "label": "Tokyo", "source": "userConfigured" },
                "weatherProvided": false
            },
            "listeningIntents": { "status": "notShared", "revision": "cfg-1", "items": [] }
        });
        let with_moment: DjCue = serde_json::from_value(moment_val).unwrap();
        let txt = cue_text(&with_moment);
        assert!(
            txt.contains("thursday evening (2026-12-24, America/New_York)"),
            "{txt}"
        );
        assert!(
            txt.contains("Christmas Eve (eve, id=occasion:us:christmas-day)"),
            "{txt}"
        );
        assert!(txt.contains("scene city: Tokyo"), "{txt}");
        assert!(txt.contains("Listening intents: not shared."), "{txt}");
    }

    fn song(id: &str, title: &str) -> serde_json::Value {
        json!({"videoId": id, "title": title, "artists": ["Band"], "album": null,
               "durationMs": 200000})
    }

    #[test]
    fn listener_actions_read_as_plain_facts_newest_first_and_capped() {
        let actions: Vec<ListenerAction> = serde_json::from_value(json!([
            {"action": "next", "agoMs": 2000, "during": song("x", "X"), "positionMs": 12400},
            {"action": "volume", "agoMs": 9000, "volumeFrom": 70, "volumeTo": 30},
            {"action": "upNextFilter", "agoMs": 20000, "value": "Familiar"},
            {"action": "rate", "agoMs": 30000, "during": song("x", "X"),
             "subject": song("y", "Y\nline"), "value": "liked"},
            {"action": "seek", "agoMs": 40000, "during": song("x", "X"), "positionMs": 61000,
             "seekToMs": 5000},
            {"action": "pause", "agoMs": 125000, "during": song("x", "X"), "positionMs": 83000},
            {"action": "mute", "agoMs": 130000, "value": "on"},
            {"action": "stop", "agoMs": 140000}
        ]))
        .unwrap();
        assert_eq!(
            listener_actions_text(&actions, 3).as_deref(),
            Some(
                "The listener's own actions, newest first: skipped \"X\" 12 s in; turned the \
                 volume down from 70 to 30; chose the Up Next filter Familiar; liked \"Y line\" \
                 by Band; moved the position from 1:01 to 5 s in \"X\"; paused \"X\" at 1:23 \
                 (2 minutes ago); and 5 more."
            )
        );
        assert_eq!(listener_actions_text(&[], 0), None);
        // Without a subject the action is not pinned on the track that was playing.
        let unknown: Vec<ListenerAction> = serde_json::from_value(json!([
            {"action": "rate", "agoMs": 1000, "during": song("x", "X"), "value": "liked"},
            {"action": "playTrack", "agoMs": 2000, "during": song("x", "X")}
        ]))
        .unwrap();
        assert_eq!(
            listener_actions_text(&unknown, 0).as_deref(),
            Some(
                "The listener's own actions, newest first: liked a track while \"X\" played; \
                 started a track while \"X\" played."
            )
        );
        assert_eq!(
            listener_actions_text(&[], 4).as_deref(),
            Some("The listener's own actions, newest first: 4 actions.")
        );
    }

    #[test]
    fn a_listener_signal_prompt_lists_what_the_listener_did() {
        let cue: DjCue = serde_json::from_value(json!({
            "seq": 7, "kind": "listenerSignal", "reasons": ["skipStreak"], "ageMs": 0,
            "prompt": "By hand, the listener just skipped 2 tracks in a row.",
            "nowPlaying": null, "upcoming": "known", "upNext": [], "moreQueued": false,
            "recent": [],
            "listener": {"lastInteraction": "playerControl", "lastInteractionAgoMs": 4000,
                         "lastSpokenToAgoMs": null, "unansweredCheckIns": 0,
                         "recentActions": [
                             {"action": "next", "agoMs": 4000, "during": song("b", "B"),
                              "positionMs": 8000},
                             {"action": "next", "agoMs": 9000, "during": song("a", "A"),
                              "positionMs": 75000}
                         ],
                         "actionsOmitted": 1},
            "chattiness": "off", "preferences": [], "preferencesUnchanged": true
        }))
        .unwrap();
        assert_eq!(
            cue_text(&cue),
            "By hand, the listener just skipped 2 tracks in a row.\n\n\
             Up next: nothing queued.\n\
             The listener has been quiet for under a minute.\n\
             The listener's own actions, newest first: skipped \"B\" 8 s in; skipped \"A\" \
             1:15 in; and 1 more."
        );
    }

    #[test]
    fn profile_text_names_its_sources_scripts_and_what_could_not_be_read() {
        let profile: ListenerProfile = serde_json::from_value(json!({
            "generatedAtMs": 1,
            "liked": {"total": null, "sampled": 200,
                      "topArtists": [{"name": "A", "tracks": 12}, {"name": "B", "tracks": 9}],
                      "recent": []},
            "history": {"total": 1530, "sampled": 200,
                        "topArtists": [{"name": "C", "tracks": 30}], "recent": []},
            "playlists": [{"playlistId": "PLfixture", "title": "Road trip", "trackCount": 40},
                          {"playlistId": "PLother", "title": "Late", "trackCount": null}],
            "morePlaylists": true,
            "scripts": [{"script": "latin", "tracks": 218, "percent": 91},
                        {"script": "kana", "tracks": 15, "percent": 6},
                        {"script": "han", "tracks": 7, "percent": 3}],
            "scriptsSampled": 240, "tasteNotes": 0, "unavailable": ["tasteNotes"]
        }))
        .unwrap();
        assert_eq!(
            profile_text(&profile),
            "Listener profile from the first 200 liked songs (more exist) and 200 of 1530 plays \
             on this player. Most liked artists: A (12), B (9). Most played artists: C (30). \
             Playlists: \"Road trip\" (40), \"Late\" and more. Scripts of what they listen to: \
             Latin 91%, Kana 6%, Han 3% (from 240 tracks). Unavailable this time: taste notes."
        );
        let unread: ListenerProfile = serde_json::from_value(json!({
            "generatedAtMs": 1,
            "liked": {"total": null, "sampled": 0, "topArtists": [], "recent": []},
            "history": {"total": 0, "sampled": 0, "topArtists": [], "recent": []},
            "playlists": [], "morePlaylists": false, "scripts": [], "scriptsSampled": 0,
            "tasteNotes": 2, "unavailable": ["liked", "playlists"]
        }))
        .unwrap();
        assert_eq!(
            profile_text(&unread),
            "Listener profile from 0 plays on this player. 2 taste notes are stored. \
             Unavailable this time: liked songs, playlists."
        );
    }
}
