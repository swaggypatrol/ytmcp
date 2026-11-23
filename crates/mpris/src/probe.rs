pub use crate::metadata::{
    DEFAULT_DESKTOP_ENTRY, DEFAULT_MPRIS_BUS_NAME, DEFAULT_MPRIS_IDENTITY, MPRIS_BUS_PREFIX,
    TRACK_PATH_PREFIX, bus_name_suffix_from_full, derive_track_id, map_loop_status_to_repeat,
    map_playback_status, map_repeat_to_loop_status, mpris_time_to_ms, ms_to_mpris_time,
    sanitize_contained_cover_file_url, sanitize_dbus_segment,
};
pub use crate::service::{MprisConfig, MprisHandle, MprisHostAction, MprisService};

/// Compatibility alias preserving T11 probe imports; there is strictly ONE MPRIS service implementation.
pub type MprisProbe = MprisService;
