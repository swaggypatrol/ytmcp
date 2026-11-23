//! Daily limits on account changes made by control clients.
//!
//! Account writes fall into two tiers. The record tier (ratings, library saves, artist
//! subscriptions, new private playlists and appends to them) leaves entries that can be undone
//! one by one; the edit tier (changes to existing playlists and the local history) can lose
//! things. Each tier has a per-account budget of items over a rolling 24 hours, counted from
//! the operation journal so it survives restarts. Playback and everything else is not limited.

use ytm_control_api::{
    ControlError, ErrorCode, OperationPhase, OperationReceipt, RetryDisposition, WriteAction,
    WriteInput,
};

use super::journal::AccountOperation;

pub(super) const WINDOW_MS: u64 = 24 * 60 * 60 * 1000;
const RECORD_DAILY_ITEMS: u32 = 100;
const EDIT_DAILY_ITEMS: u32 = 20;
const EDIT_ITEMS_PER_CALL: u32 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tier {
    Record,
    Edit,
}

impl Tier {
    fn name(self) -> &'static str {
        match self {
            Tier::Record => "record",
            Tier::Edit => "edit",
        }
    }

    fn daily_items(self) -> u32 {
        match self {
            Tier::Record => RECORD_DAILY_ITEMS,
            Tier::Edit => EDIT_DAILY_ITEMS,
        }
    }
}

/// The tier and item count of an account write, or `None` for writes without a daily limit.
/// A local-history edit counts the entries its receipt records as removed.
pub(super) fn charge(input: &WriteInput, receipt: &OperationReceipt) -> Option<(Tier, u32)> {
    use WriteAction::*;
    let count = |items: usize| u32::try_from(items).unwrap_or(u32::MAX);
    match &input.action {
        Rate { .. }
        | SetLibraryMembership { .. }
        | SetArtistSubscription { .. }
        | CreatePrivatePlaylist { .. } => Some((Tier::Record, 1)),
        // The worker refuses the append unless that operation verifiably created the playlist.
        AddPlaylistEntries {
            created_by_operation_id: Some(_),
            video_ids,
            ..
        } => Some((Tier::Record, count(video_ids.len()))),
        AddPlaylistEntries { video_ids, .. } => Some((Tier::Edit, count(video_ids.len()))),
        RemovePlaylistEntries { entry_ids, .. } => Some((Tier::Edit, count(entry_ids.len()))),
        MovePlaylistEntry { .. }
        | UpdatePlaylist { .. }
        | DeletePlaylist { .. }
        | SetPlaylistPrivacy { .. } => Some((Tier::Edit, 1)),
        EditLocalHistory { .. } => Some((Tier::Edit, count(receipt.removed.len()))),
        _ => None,
    }
}

/// Whether a journaled operation uses its budget. Refused and cancelled operations sent nothing
/// that changed the account; an accepted one that a restart stopped was never sent. A live
/// accepted operation is reserved, so concurrent submissions cannot pass the limit together.
fn counts(receipt: &OperationReceipt) -> bool {
    match receipt.phase {
        OperationPhase::Rejected | OperationPhase::Cancelled => false,
        OperationPhase::Accepted => !receipt.scheduling_stopped,
        _ => true,
    }
}

/// Refuses a write of `items` in `tier` that would pass the per-call or the daily limit, given
/// the account's operations of the last 24 hours.
pub(super) fn admit(
    tier: Tier,
    items: u32,
    now_ms: u64,
    recent: &[AccountOperation],
) -> Result<(), ControlError> {
    if tier == Tier::Edit && items > EDIT_ITEMS_PER_CALL {
        return Err(ControlError::new(
            ErrorCode::LimitReached,
            RetryDisposition::NotRetryable,
            format!(
                "one edit-tier call may change at most {EDIT_ITEMS_PER_CALL} items and this one \
                 would change {items}; nothing was sent. Split it into smaller calls"
            ),
        ));
    }
    let limit = tier.daily_items();
    let mut used: Vec<(u64, u32)> = recent
        .iter()
        .filter(|operation| counts(&operation.receipt))
        .filter_map(|operation| {
            let (charged, count) = charge(&operation.input, &operation.receipt)?;
            (charged == tier && count > 0).then_some((operation.claimed_at_ms, count))
        })
        .collect();
    let total: u64 = used.iter().map(|(_, count)| u64::from(*count)).sum();
    if total + u64::from(items) <= u64::from(limit) {
        return Ok(());
    }
    used.sort_unstable();
    let mut remaining = total;
    let free_at = used.iter().find_map(|(at, count)| {
        remaining -= u64::from(*count);
        (remaining + u64::from(items) <= u64::from(limit)).then_some(at + WINDOW_MS)
    });
    let retry_after_ms = free_at.map(|at| at.saturating_sub(now_ms).max(1));
    let when = retry_after_ms.map_or_else(
        || "it does not fit within one day's limit".to_owned(),
        |wait| format!("room frees up in about {}", approximate(wait)),
    );
    let mut error = ControlError::new(
        ErrorCode::LimitReached,
        RetryDisposition::NotRetryable,
        format!(
            "the {} tier allows {limit} account changes per rolling 24 hours and {total} are \
             already used, so these {items} cannot be made now; nothing was sent, and {when}",
            tier.name()
        ),
    );
    error.retry_after_ms = retry_after_ms;
    Err(error)
}

fn approximate(wait_ms: u64) -> String {
    let minutes = wait_ms.div_ceil(60_000);
    match (minutes / 60, minutes % 60) {
        (0, minutes) => format!("{minutes} min"),
        (hours, 0) => format!("{hours} h"),
        (hours, minutes) => format!("{hours} h {minutes} min"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ytm_control_api::Preconditions;

    fn operation(at: u64, action: WriteAction, phase: OperationPhase) -> AccountOperation {
        AccountOperation {
            claimed_at_ms: at,
            input: WriteInput {
                operation_id: format!("op-{at}"),
                preconditions: Preconditions {
                    expected_account_epoch: None,
                    expected_bridge_epoch: None,
                    expected_queue_revision: None,
                    expected_entity_revision: None,
                },
                action,
            },
            receipt: OperationReceipt {
                operation_id: format!("op-{at}"),
                account_scope: None,
                revision: 1,
                phase,
                resource: None,
                verification: None,
                item_results: vec![],
                scheduling_stopped: phase != OperationPhase::Accepted,
                settled: true,
                retry_disposition: RetryDisposition::NotRetryable,
                error: None,
                removed: vec![],
            },
        }
    }

    fn removal(count: usize) -> WriteAction {
        WriteAction::RemovePlaylistEntries {
            playlist_id: ytm_protocol::PlaylistId::new("PL_fixture").unwrap(),
            entry_ids: (0..count).map(|index| format!("entry-{index}")).collect(),
        }
    }

    #[test]
    fn the_window_frees_when_enough_earlier_items_expire() {
        let recent = [
            operation(1_000, removal(8), OperationPhase::Verified),
            operation(2_000, removal(10), OperationPhase::Unknown),
            operation(3_000, removal(9), OperationPhase::Rejected),
        ];
        assert!(admit(Tier::Edit, 2, 5_000, &recent).is_ok());
        let error = admit(Tier::Edit, 3, 5_000, &recent).unwrap_err();
        assert_eq!(error.code, ErrorCode::LimitReached);
        assert_eq!(error.retry_after_ms, Some(1_000 + WINDOW_MS - 5_000));
        assert!(error.message.contains("edit tier allows 20"));
        // Record-tier usage does not take from the edit budget.
        assert!(admit(Tier::Record, 100, 5_000, &recent).is_ok());
    }

    #[test]
    fn a_live_accepted_write_is_reserved_but_one_a_restart_stopped_is_not() {
        let mut live = operation(1_000, removal(10), OperationPhase::Accepted);
        live.receipt.scheduling_stopped = false;
        let mut stopped = operation(2_000, removal(10), OperationPhase::Accepted);
        stopped.receipt.scheduling_stopped = true;
        assert!(admit(Tier::Edit, 10, 5_000, &[live, stopped]).is_ok());
        let mut second = operation(3_000, removal(10), OperationPhase::Accepted);
        second.receipt.scheduling_stopped = false;
        let mut first = operation(1_000, removal(10), OperationPhase::Accepted);
        first.receipt.scheduling_stopped = false;
        assert_eq!(
            admit(Tier::Edit, 1, 5_000, &[first, second])
                .unwrap_err()
                .code,
            ErrorCode::LimitReached
        );
    }
}
