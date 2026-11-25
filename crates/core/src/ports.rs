use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;
use ytm_protocol::{
    AccountEpoch, AccountScope, AppSettings, AssetId, AssetResolvedPayload, BridgeEpoch,
    CatalogQueryRequest, CatalogResponsePayload, LyricsQueryRequest, LyricsResult, MutationRequest,
    MutationResult, OutputGateResultPayload, OwnerMomentConfig, PlaybackRef, PlayerCommandPayload,
    QueryError, QueryErrorCode, RecentSearchesRequest, RecentSearchesSnapshot,
    SessionMaterialPayload, Track, VideoId,
};

/// Identity the catalog established for a session. `account_hint` is the opaque hash shared
/// with the bridge (`acct:<hex>`); `None` means the catalog identity could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogIdentity {
    pub account_scope: AccountScope,
    pub account_hint: Option<String>,
}

/// A track the account played, with the last persisted position.
#[derive(Debug, Clone, PartialEq)]
pub struct LastPlayed {
    pub track: Track,
    pub playback_ref: Option<PlaybackRef>,
    pub position_ms: u64,
}

/// What survives a restart for the most recently active account: its cache scope (for offline,
/// unverified browsing) and its last played track (for the paused restore on launch).
#[derive(Debug, Clone, PartialEq)]
pub struct LastSession {
    pub account_scope: AccountScope,
    pub last_played: Option<LastPlayed>,
}

/// Plain-text lyrics published by YouTube Music for a track (no timestamps).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfficialLyrics {
    pub text: String,
    /// Attribution line returned by the service (e.g. the lyrics provider).
    pub source: String,
}

/// A cover image resolved into the local cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedCover {
    pub asset_id: AssetId,
    pub accent_oklch: String,
}

/// Object-safe boxed future alias for async port traits used across `ytm-core` adapters.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Process-local monotonic clock abstraction.
pub trait Clock: Send + Sync {
    /// Returns monotonic milliseconds since an arbitrary fixed origin within this process.
    fn now_ms(&self) -> u64;

    /// Returns wall-clock milliseconds since the Unix epoch (for timestamped receipts/metadata).
    fn wall_now_ms(&self) -> u64 {
        self.now_ms()
    }
}

static PROCESS_START: OnceLock<Instant> = OnceLock::new();

fn process_origin() -> Instant {
    *PROCESS_START.get_or_init(Instant::now)
}

/// Production monotonic clock backed by `std::time::Instant`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl SystemClock {
    pub fn new() -> Self {
        let _ = process_origin();
        Self
    }
}

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        let elapsed = Instant::now().saturating_duration_since(process_origin());
        u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
    }

    fn wall_now_ms(&self) -> u64 {
        let elapsed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
    }
}

/// Deterministic clock for offline unit and integration tests.
#[derive(Debug)]
pub struct MockClock {
    monotonic_ms: AtomicU64,
    wall_offset_ms: AtomicU64,
}

impl MockClock {
    pub fn new(initial_ms: u64) -> Self {
        Self {
            monotonic_ms: AtomicU64::new(initial_ms),
            wall_offset_ms: AtomicU64::new(0),
        }
    }

    pub fn with_wall_offset(initial_ms: u64, wall_offset_ms: u64) -> Self {
        Self {
            monotonic_ms: AtomicU64::new(initial_ms),
            wall_offset_ms: AtomicU64::new(wall_offset_ms),
        }
    }

    pub fn set(&self, now_ms: u64) {
        self.monotonic_ms.store(now_ms, Ordering::SeqCst);
    }

    pub fn advance(&self, delta_ms: u64) -> u64 {
        self.monotonic_ms
            .fetch_add(delta_ms, Ordering::SeqCst)
            .saturating_add(delta_ms)
    }

    pub fn set_wall_offset_ms(&self, offset_ms: u64) {
        self.wall_offset_ms.store(offset_ms, Ordering::SeqCst);
    }
}

impl Default for MockClock {
    fn default() -> Self {
        Self::new(0)
    }
}

impl Clock for MockClock {
    fn now_ms(&self) -> u64 {
        self.monotonic_ms.load(Ordering::SeqCst)
    }

    fn wall_now_ms(&self) -> u64 {
        self.now_ms()
            .saturating_add(self.wall_offset_ms.load(Ordering::SeqCst))
    }
}

/// Port for sending typed commands and snapshot requests to the authorized player bridge.
pub trait BridgePort: Send + Sync {
    fn send_player_command(
        &self,
        bridge_epoch: BridgeEpoch,
        payload: PlayerCommandPayload,
    ) -> BoxFuture<'_, Result<(), QueryError>>;

    fn request_snapshot(&self, bridge_epoch: BridgeEpoch) -> BoxFuture<'_, Result<(), QueryError>>;
}

/// Port for host-managed audio output gating during paused Next/Previous transitions.
pub trait OutputGatePort: Send + Sync {
    fn set_output_gated(
        &self,
        transaction_id: String,
        gated: bool,
    ) -> BoxFuture<'_, Result<OutputGateResultPayload, QueryError>>;
}

/// The outcome of adding one song to a playlist and, when the provider's reply named exactly one
/// entry it created for that song, the entry's ID. The ID stays in the core: it is what an undo
/// removes, and it never reaches the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistAddition {
    pub result: MutationResult,
    pub created_entry_id: Option<String>,
}

/// Port for YouTube Music catalog queries, mutations, and in-memory session binding (`crates/ytm-data`).
pub trait CatalogPort: Send + Sync {
    fn query(
        &self,
        account_epoch: Option<AccountEpoch>,
        account_scope: Option<AccountScope>,
        request: CatalogQueryRequest,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<CatalogResponsePayload, QueryError>>;

    fn mutate(
        &self,
        account_epoch: AccountEpoch,
        account_scope: AccountScope,
        request: MutationRequest,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<MutationResult, QueryError>>;

    /// `mutate` for an `addToPlaylist`, also naming the entry the provider said the add created
    /// (only when its reply named exactly one for the song). Ports that cannot name it add
    /// through `mutate` and report none, so the addition cannot be undone.
    fn add_to_playlist(
        &self,
        account_epoch: AccountEpoch,
        account_scope: AccountScope,
        request: MutationRequest,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<PlaylistAddition, QueryError>> {
        Box::pin(async move {
            Ok(PlaylistAddition {
                result: self
                    .mutate(account_epoch, account_scope, request, cancel)
                    .await?,
                created_entry_id: None,
            })
        })
    }

    /// Binds in-memory credentials and returns the identity the catalog verified for them.
    fn apply_session_material(
        &self,
        material: SessionMaterialPayload,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<CatalogIdentity, QueryError>>;

    /// Official plain lyrics for `video_id`; `Ok(None)` when the service has none.
    fn official_lyrics(
        &self,
        account_epoch: AccountEpoch,
        video_id: VideoId,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<Option<OfficialLyrics>, QueryError>>;

    /// Drops cached catalog reads held outside persistent storage.
    fn clear_memory_cache(&self, _scope: Option<&AccountScope>) -> Result<(), QueryError> {
        Ok(())
    }

    /// Called before the persistent cache is cleared: waits for any cache write in progress and
    /// makes reads that started earlier unable to write their results afterwards.
    fn fence_cache_writes(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }

    /// Drops in-memory credentials and history belonging to the expected account epoch.
    fn clear_session_material(&self, account_epoch: Option<AccountEpoch>)
    -> Result<(), QueryError>;
}

/// Port for persistent settings, account-partitioned caches, and local cover asset resolution (`crates/storage`).
pub trait StoragePort: Send + Sync {
    fn load_settings(&self) -> BoxFuture<'_, Result<Option<AppSettings>, QueryError>>;

    fn save_settings(&self, settings: AppSettings) -> BoxFuture<'_, Result<(), QueryError>>;

    fn load_moment_config(&self) -> BoxFuture<'_, Result<OwnerMomentConfig, QueryError>> {
        Box::pin(async { Ok(OwnerMomentConfig::default()) })
    }

    fn save_moment_config(
        &self,
        _config: OwnerMomentConfig,
    ) -> BoxFuture<'_, Result<(), QueryError>> {
        Box::pin(async { Ok(()) })
    }

    fn clear_cache(
        &self,
        account_scope: Option<AccountScope>,
    ) -> BoxFuture<'_, Result<(), QueryError>>;

    /// Resolves a cover asset to a cached file, fetching it on first use.
    fn resolve_asset(
        &self,
        asset_id: AssetId,
    ) -> BoxFuture<'_, Result<AssetResolvedPayload, QueryError>>;

    /// Fetches (or reuses) the cover behind `remote_url` and returns its asset id and accent.
    fn cache_cover(&self, remote_url: String) -> BoxFuture<'_, Result<CachedCover, QueryError>>;

    /// The cover behind `remote_url` when it is already cached, answered from memory without
    /// disk or network IO (the engine calls it on a track change). `None` means "not known to
    /// be cached"; `cache_cover` remains the authority.
    fn cached_cover(&self, _remote_url: &str) -> Option<CachedCover> {
        None
    }

    /// Keeps the now-playing cover out of capacity eviction; `None` releases it.
    fn pin_cover(&self, _asset_id: Option<AssetId>) {}

    /// Logout: deletes this account's private cache and invalidates its epochs (not settings,
    /// other accounts, lyrics or covers).
    fn clear_account_data(
        &self,
        account_scope: Option<AccountScope>,
        pending_epoch: Option<AccountEpoch>,
    ) -> BoxFuture<'_, Result<(), QueryError>>;

    fn last_session(&self) -> BoxFuture<'_, Result<Option<LastSession>, QueryError>>;

    /// Appends a played track to the account's bounded local history.
    fn record_playback(
        &self,
        account_scope: AccountScope,
        account_epoch: AccountEpoch,
        played: LastPlayed,
    ) -> BoxFuture<'_, Result<(), QueryError>>;

    /// Applies one action to the account's recent searches (kept across restarts, deleted with
    /// the account's data, untouched by a cache clear) and returns the list, newest first.
    /// Writes are refused for an epoch that is no longer the account's active one.
    fn recent_searches(
        &self,
        account_scope: AccountScope,
        account_epoch: AccountEpoch,
        request: RecentSearchesRequest,
    ) -> BoxFuture<'_, Result<RecentSearchesSnapshot, QueryError>>;

    /// Updates the position of the newest history entry when it is still `video_id`.
    fn update_playback_position(
        &self,
        account_scope: AccountScope,
        account_epoch: AccountEpoch,
        video_id: VideoId,
        position_ms: u64,
    ) -> BoxFuture<'_, Result<(), QueryError>>;
}

/// Port for lyrics fetching, matching, and LRC parsing (`crates/lyrics`).
pub trait LyricsPort: Send + Sync {
    fn set_enabled(&self, _enabled: bool) {}

    fn query_lyrics(
        &self,
        request: LyricsQueryRequest,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<LyricsResult, QueryError>>;
}

/// Default bridge port returning `BridgeIncompatible` when no bridge transport is wired.
#[derive(Debug, Default)]
pub struct NoopBridgePort;

impl BridgePort for NoopBridgePort {
    fn send_player_command(
        &self,
        _bridge_epoch: BridgeEpoch,
        _payload: PlayerCommandPayload,
    ) -> BoxFuture<'_, Result<(), QueryError>> {
        Box::pin(async {
            Err(QueryError {
                code: QueryErrorCode::BridgeIncompatible,
                retriable: false,
                retry_after_ms: None,
                safe_message: "player bridge transport is not attached".to_string(),
                request_id: None,
            })
        })
    }

    fn request_snapshot(
        &self,
        _bridge_epoch: BridgeEpoch,
    ) -> BoxFuture<'_, Result<(), QueryError>> {
        Box::pin(async {
            Err(QueryError {
                code: QueryErrorCode::BridgeIncompatible,
                retriable: false,
                retry_after_ms: None,
                safe_message: "player bridge transport is not attached".to_string(),
                request_id: None,
            })
        })
    }
}

/// Default output gate port returning `UnsupportedCapability` when host gating is unavailable.
#[derive(Debug, Default)]
pub struct NoopOutputGatePort;

impl OutputGatePort for NoopOutputGatePort {
    fn set_output_gated(
        &self,
        transaction_id: String,
        _gated: bool,
    ) -> BoxFuture<'_, Result<OutputGateResultPayload, QueryError>> {
        Box::pin(async move {
            Ok(OutputGateResultPayload {
                transaction_id,
                applied: false,
                error: Some("output gate is not supported on this host".to_string()),
            })
        })
    }
}
