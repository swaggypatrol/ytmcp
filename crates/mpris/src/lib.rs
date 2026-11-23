pub mod metadata;
pub mod probe;
pub mod service;

pub use metadata::{
    DEFAULT_DESKTOP_ENTRY, DEFAULT_MPRIS_BUS_NAME, DEFAULT_MPRIS_IDENTITY, MPRIS_BUS_PREFIX,
    TRACK_PATH_PREFIX, bus_name_suffix_from_full, derive_track_id, map_loop_status_to_repeat,
    map_playback_status, map_repeat_to_loop_status, mpris_time_to_ms, ms_to_mpris_time,
    resolve_covers_cache_root, sanitize_contained_cover_file_url, sanitize_dbus_segment,
};
pub use service::{MprisConfig, MprisHandle, MprisHostAction, MprisService};

pub fn protocol_version() -> u32 {
    ytm_core::protocol_version()
}
