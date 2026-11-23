//! The text an agent reads back from a tool call. A write says one of four things: done, partly
//! done (naming which items), not done with nothing changed (with one way out), or unclear. The
//! receipt phases and other journal vocabulary stay in the structured content for debugging.

use rmcp::model::JsonObject;
use ytm_control_api::{
    AccountHandoffState, ControlError, ControlResponse, DjCue, DjPreferenceStance,
    DjPreferenceSubject, DjTrack, DjTrackOutcome, ErrorCode, InterruptResumeState, ItemPhase,
    OperationPhase, OperationReceipt, OperationResource, RetryDisposition,
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
            not_done(tool, arguments, &error)
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
    lines.push(if cue.up_next.is_empty() {
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
    if !cue.preferences.is_empty() {
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
        lines.push(format!("Recorded tastes: {}.", tastes.join("; ")));
    }
    lines.join("\n")
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
            "upNext": [track("b", "Beta")], "moreQueued": false,
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
}
