//! Where the current occurrence sits in the loaded queue window, and what follows it.
//!
//! Every reader of "what plays next" (the UI preview, control context, cues) goes through
//! [`upcoming`]: the entries after the current occurrence are known only when that exact
//! occurrence is in the window. A matching video id elsewhere does not count, since the same
//! track can legitimately be queued twice.

use std::ops::Range;

use ytm_protocol::{QueueEntry, QueueSnapshot};

/// Most entries a queue preview carries.
pub const MAX_PREVIEW_ENTRIES: usize = 100;
/// Most played entries kept before the current one in a preview.
pub const PREVIEW_PLAYED_ENTRIES: usize = 10;

/// What follows the current occurrence in the loaded window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upcoming<'a> {
    /// The current occurrence is at `index` in the window; `after` is everything loaded after
    /// it (empty when it is the last loaded entry).
    Known {
        index: usize,
        after: &'a [QueueEntry],
    },
    /// The current occurrence is not in the window (no current entry, a selection lag, or a
    /// current entry outside the loaded range). Nothing is known about what follows.
    Unknown,
}

/// The entries loaded after the current occurrence, when that occurrence is in the window.
pub fn upcoming(queue: &QueueSnapshot) -> Upcoming<'_> {
    let Some(current) = queue.current_entry_id.as_ref() else {
        return Upcoming::Unknown;
    };
    match queue
        .entries
        .iter()
        .position(|entry| &entry.entry_id == current)
    {
        Some(index) => Upcoming::Known {
            index,
            after: &queue.entries[index + 1..],
        },
        None => Upcoming::Unknown,
    }
}

/// The slice of the loaded window a preview shows: up to [`PREVIEW_PLAYED_ENTRIES`] played
/// entries, the current one and what follows, at most [`MAX_PREVIEW_ENTRIES`] in all. When
/// fewer entries follow than fit, more played ones fill the space. Without a known current
/// occurrence it is the window's start.
pub fn preview_range(queue: &QueueSnapshot) -> Range<usize> {
    let len = queue.entries.len();
    let start = match upcoming(queue) {
        Upcoming::Known { index, .. } => index
            .saturating_sub(PREVIEW_PLAYED_ENTRIES)
            .min(len.saturating_sub(MAX_PREVIEW_ENTRIES)),
        Upcoming::Unknown => 0,
    };
    start..len.min(start + MAX_PREVIEW_ENTRIES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ytm_protocol::{QueueCompleteness, QueueEntryId, Track, TrackAvailability, VideoId};

    fn queue(len: usize, current: Option<&str>) -> QueueSnapshot {
        QueueSnapshot {
            bridge_epoch: None,
            queue_revision: 1,
            current_entry_id: current.map(|id| QueueEntryId::new(id).unwrap()),
            entries: (0..len)
                .map(|i| {
                    // Every entry is the same track: only occurrences tell them apart.
                    let video_id = VideoId::new("sameVideo01").unwrap();
                    QueueEntry {
                        entry_id: QueueEntryId::new(format!("e{i}")).unwrap(),
                        video_id: video_id.clone(),
                        track: Track {
                            video_id,
                            title: "Same".to_owned(),
                            artists: Vec::new(),
                            album: None,
                            duration_ms: None,
                            cover: None,
                            availability: TrackAvailability::Available,
                            version_tag: None,
                            is_liked: None,
                        },
                        availability: TrackAvailability::Available,
                    }
                })
                .collect(),
            loaded_range: None,
            completeness: QueueCompleteness::Partial,
            has_more: None,
            explicit_complete: None,
            may_load_more: None,
            official_upcoming: None,
            filters: None,
        }
    }

    #[test]
    fn a_current_occurrence_deep_in_a_long_window_keeps_itself_and_what_follows() {
        // 200 loaded, current at window index 190 (absolute 220 of 230).
        let q = queue(200, Some("e190"));
        let range = preview_range(&q);
        assert_eq!(range, 100..200);
        assert!(range.contains(&190));
        assert_eq!(
            upcoming(&q),
            Upcoming::Known {
                index: 190,
                after: &q.entries[191..]
            }
        );
    }

    #[test]
    fn a_current_occurrence_near_the_start_keeps_ten_played_entries_and_fills_ahead() {
        assert_eq!(preview_range(&queue(200, Some("e50"))), 40..140);
        assert_eq!(preview_range(&queue(200, Some("e3"))), 0..100);
        assert_eq!(preview_range(&queue(30, Some("e20"))), 0..30);
    }

    #[test]
    fn the_last_loaded_entry_is_known_with_nothing_after_it() {
        let q = queue(5, Some("e4"));
        assert_eq!(
            upcoming(&q),
            Upcoming::Known {
                index: 4,
                after: &[]
            }
        );
    }

    #[test]
    fn a_missing_or_absent_current_occurrence_is_unknown_not_the_whole_list() {
        assert_eq!(upcoming(&queue(5, None)), Upcoming::Unknown);
        // Selection lag: the page's current entry is outside the loaded window.
        let q = queue(150, Some("elsewhere"));
        assert_eq!(upcoming(&q), Upcoming::Unknown);
        assert_eq!(preview_range(&q), 0..100);
    }
}
