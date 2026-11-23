pub mod auth;
pub mod cache;
pub mod catalog;
pub mod connectivity;
pub mod probe;

pub use auth::{account_hint_for, account_scope_for, derive_account_scope};
pub use cache::{
    CatalogQueryKey, CursorKind, HOME_FALLBACK_NOTICE, decode_cursor, encode_cursor,
    is_retryable_read_error,
};
pub use catalog::{CatalogService, CatalogServiceConfig, YtmCatalogAdapter, YtmCatalogConfig};
pub use connectivity::{
    ConnectivityConfig, HttpReachabilityProbe, MUSIC_PROBE_URL, ReachabilityProbe,
    run_connectivity_monitor,
};
pub use probe::format_cookie_header;

pub fn protocol_version() -> u32 {
    ytm_core::protocol_version()
}
