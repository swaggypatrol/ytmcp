pub mod clock;
pub mod commands;
pub mod control;
pub mod engine;
pub mod error;
pub mod playlist_undo;
pub mod ports;
pub mod queue_window;

pub use clock::PlaybackClock;
pub use commands::{
    CommandDeduplicator, DEDUP_TTL_MS, MAX_DEDUP_ENTRIES, PlaybackFence, SEEK_TOLERANCE_MS,
};
pub use engine::{CoreDeps, CoreDiagnostics, CoreEngine, DEFAULT_CONTROL_TIMEOUT_MS, EngineConfig};
pub use error::CoreError;
pub use ports::{
    BoxFuture, BridgePort, CachedCover, CatalogIdentity, CatalogPort, Clock, LastPlayed,
    LastSession, LyricsPort, MockClock, NoopBridgePort, NoopOutputGatePort, OfficialLyrics,
    OutputGatePort, StoragePort, SystemClock,
};
pub use tokio_util::sync::CancellationToken;

pub fn protocol_version() -> u32 {
    ytm_protocol::PROTOCOL_VERSION
}
