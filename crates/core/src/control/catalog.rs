use tokio_util::sync::CancellationToken;
use ytm_control_api::{
    ControlError, DiscoveryKind, DiscoveryPage, ErrorCode, ExtendedMediaPage, ExtendedMediaTarget,
    HistoryEntry, LibraryKind, LibraryMembershipTarget, LibraryPage, MembershipEvidence, Page,
    PageInput, Playlist, PlaylistPrivacy, Rating, RecommendationPage, RetryDisposition, Track,
};
use ytm_protocol::{
    AccountEpoch, AccountScope, ArtistId, PlaylistId, QueryError, QueryErrorCode, VideoId,
};

use crate::ports::BoxFuture;

/// Set by the control service from the authenticated CoreEngine account context.
#[derive(Clone)]
pub struct CatalogReadContext {
    pub account_scope: AccountScope,
    pub account_epoch: AccountEpoch,
    pub cancel: CancellationToken,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaylistWrite {
    CreatePrivate {
        title: String,
        description: Option<String>,
    },
    Append {
        playlist: PlaylistId,
        videos: Vec<VideoId>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaylistEdit {
    Update {
        title: Option<String>,
        description: Option<String>,
    },
    SetPrivacy(PlaylistPrivacy),
    Remove(Vec<PlaylistOccurrenceIdentity>),
    Move {
        entry_id: String,
        before_entry_id: Option<String>,
    },
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistOccurrenceIdentity {
    pub video_id: VideoId,
    pub entry_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddedOccurrence {
    pub video_id: VideoId,
    pub entry_id: String,
}

/// Provider acknowledgment. ControlService still checks the actual result with a fresh read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaylistWriteAck {
    Created(PlaylistId),
    Appended(Vec<AddedOccurrence>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogWriteError {
    Rejected(ControlError),
    Unknown(ControlError),
}

pub type PlaylistWriteError = CatalogWriteError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RatingEvidence {
    pub rating: Rating,
    pub observed_at_ms: u64,
}

pub trait ControlCatalog: Send + Sync {
    fn search_entities(
        &self,
        _context: CatalogReadContext,
        _kind: ytm_control_api::EntitySearchKind,
        _query: String,
        _page: PageInput,
    ) -> BoxFuture<'_, Result<Page<ytm_control_api::SearchEntity>, QueryError>> {
        Box::pin(async { Err(library_unsupported()) })
    }

    fn mood_category(
        &self,
        _context: CatalogReadContext,
        _category_id: String,
        _page: PageInput,
    ) -> BoxFuture<'_, Result<DiscoveryPage, QueryError>> {
        Box::pin(async { Err(library_unsupported()) })
    }

    fn search_suggestions(
        &self,
        _context: CatalogReadContext,
        _query: String,
        _limit: u16,
    ) -> BoxFuture<'_, Result<Page<String>, QueryError>> {
        Box::pin(async { Err(library_unsupported()) })
    }

    fn account_library(
        &self,
        _context: CatalogReadContext,
        _kind: LibraryKind,
        _page: PageInput,
    ) -> BoxFuture<'_, Result<LibraryPage, QueryError>> {
        Box::pin(async { Err(library_unsupported()) })
    }

    fn youtube_history(
        &self,
        _context: CatalogReadContext,
        _page: PageInput,
    ) -> BoxFuture<'_, Result<Page<HistoryEntry>, QueryError>> {
        Box::pin(async { Err(library_unsupported()) })
    }

    fn song_membership(
        &self,
        _context: CatalogReadContext,
        _video: VideoId,
    ) -> BoxFuture<'_, Result<MembershipEvidence, QueryError>> {
        Box::pin(async { Err(library_unsupported()) })
    }

    fn library_membership(
        &self,
        context: CatalogReadContext,
        target: LibraryMembershipTarget,
    ) -> BoxFuture<'_, Result<MembershipEvidence, QueryError>> {
        match target {
            LibraryMembershipTarget::Song { id } => self.song_membership(context, id),
            LibraryMembershipTarget::Playlist { .. } | LibraryMembershipTarget::Album { .. } => {
                Box::pin(async { Err(library_unsupported()) })
            }
        }
    }

    fn album_membership(
        &self,
        _context: CatalogReadContext,
        _album: ytm_protocol::AlbumId,
    ) -> BoxFuture<'_, Result<(PlaylistId, MembershipEvidence), QueryError>> {
        Box::pin(async { Err(library_unsupported()) })
    }

    fn set_album_membership(
        &self,
        _context: CatalogReadContext,
        _album: ytm_protocol::AlbumId,
        _playlist: PlaylistId,
        _saved: bool,
    ) -> BoxFuture<'_, Result<(), CatalogWriteError>> {
        Box::pin(async {
            Err(CatalogWriteError::Rejected(super::map_query_error(
                library_unsupported(),
            )))
        })
    }

    fn set_library_membership(
        &self,
        _context: CatalogReadContext,
        _target: LibraryMembershipTarget,
        _saved: bool,
    ) -> BoxFuture<'_, Result<(), CatalogWriteError>> {
        Box::pin(async {
            Err(CatalogWriteError::Rejected(super::map_query_error(
                library_unsupported(),
            )))
        })
    }

    fn invalidate_library_membership(
        &self,
        _scope: AccountScope,
        _target: LibraryMembershipTarget,
    ) -> BoxFuture<'_, Result<(), QueryError>> {
        Box::pin(async { Ok(()) })
    }

    fn artist_subscription(
        &self,
        _context: CatalogReadContext,
        _artist: ArtistId,
    ) -> BoxFuture<'_, Result<MembershipEvidence, QueryError>> {
        Box::pin(async { Err(library_unsupported()) })
    }

    fn set_artist_subscription(
        &self,
        _context: CatalogReadContext,
        _artist: ArtistId,
        _subscribed: bool,
    ) -> BoxFuture<'_, Result<(), CatalogWriteError>> {
        Box::pin(async {
            Err(CatalogWriteError::Rejected(super::map_query_error(
                library_unsupported(),
            )))
        })
    }

    fn extended_media(
        &self,
        _context: CatalogReadContext,
        _target: ExtendedMediaTarget,
        _page: PageInput,
    ) -> BoxFuture<'_, Result<ExtendedMediaPage, QueryError>> {
        Box::pin(async {
            Err(QueryError {
                code: QueryErrorCode::UnsupportedCapability,
                retriable: false,
                retry_after_ms: None,
                safe_message: "uploads and podcasts are unavailable for this provider".into(),
                request_id: None,
            })
        })
    }

    fn discover(
        &self,
        _context: CatalogReadContext,
        _kind: DiscoveryKind,
        _page: PageInput,
    ) -> BoxFuture<'_, Result<DiscoveryPage, QueryError>> {
        Box::pin(async {
            Err(QueryError {
                code: QueryErrorCode::UnsupportedCapability,
                retriable: false,
                retry_after_ms: None,
                safe_message: "discovery is unavailable for this provider".into(),
                request_id: None,
            })
        })
    }

    fn fresh_playlist(
        &self,
        context: CatalogReadContext,
        playlist: PlaylistId,
        page: PageInput,
    ) -> BoxFuture<'_, Result<Playlist, QueryError>>;

    fn album_playback_target(
        &self,
        _context: CatalogReadContext,
        _album: ytm_protocol::AlbumId,
        _index: u32,
    ) -> BoxFuture<'_, Result<ytm_protocol::PlaybackRef, QueryError>> {
        Box::pin(async { Err(library_unsupported()) })
    }

    fn recommend_song_radio(
        &self,
        context: CatalogReadContext,
        seed: VideoId,
        page: PageInput,
    ) -> BoxFuture<'_, Result<RecommendationPage, QueryError>>;

    fn apply_playlist(
        &self,
        _context: CatalogReadContext,
        _write: PlaylistWrite,
    ) -> BoxFuture<'_, Result<PlaylistWriteAck, PlaylistWriteError>> {
        Box::pin(async {
            Err(PlaylistWriteError::Rejected(ControlError::new(
                ErrorCode::CapabilityUnavailable,
                RetryDisposition::NotRetryable,
                "playlist writes are unavailable for this provider",
            )))
        })
    }

    /// Dispatches one edit. Success acknowledges the provider response, without readback.
    fn edit_playlist(
        &self,
        _context: CatalogReadContext,
        _playlist: PlaylistId,
        _edit: PlaylistEdit,
    ) -> BoxFuture<'_, Result<(), CatalogWriteError>> {
        Box::pin(async {
            Err(CatalogWriteError::Rejected(ControlError::new(
                ErrorCode::CapabilityUnavailable,
                RetryDisposition::NotRetryable,
                "playlist edits are unavailable for this provider",
            )))
        })
    }

    /// Retires only local caches after the operation journal records a cloud-side effect.
    fn invalidate_playlist_cache(
        &self,
        _scope: AccountScope,
        _playlist: PlaylistId,
        _library_changed: bool,
    ) -> BoxFuture<'_, Result<(), QueryError>> {
        Box::pin(async { Ok(()) })
    }

    fn fresh_rating(
        &self,
        _context: CatalogReadContext,
        _video: VideoId,
    ) -> BoxFuture<'_, Result<RatingEvidence, QueryError>> {
        Box::pin(async {
            Err(QueryError {
                code: QueryErrorCode::UnsupportedCapability,
                retriable: false,
                retry_after_ms: None,
                safe_message: "rating reads are unavailable for this provider".into(),
                request_id: None,
            })
        })
    }

    fn set_rating(
        &self,
        _context: CatalogReadContext,
        _video: VideoId,
        _rating: Rating,
    ) -> BoxFuture<'_, Result<(), CatalogWriteError>> {
        Box::pin(async {
            Err(CatalogWriteError::Rejected(ControlError::new(
                ErrorCode::CapabilityUnavailable,
                RetryDisposition::NotRetryable,
                "rating writes are unavailable for this provider",
            )))
        })
    }

    fn invalidate_rating_cache(
        &self,
        _scope: AccountScope,
    ) -> BoxFuture<'_, Result<(), QueryError>> {
        Box::pin(async { Ok(()) })
    }
}

/// Shared projection that excludes host-only fields such as cover URLs.
fn library_unsupported() -> QueryError {
    QueryError {
        code: QueryErrorCode::UnsupportedCapability,
        retriable: false,
        retry_after_ms: None,
        safe_message: "this library operation is unavailable from the provider".into(),
        request_id: None,
    }
}

pub fn project_track(track: &ytm_protocol::Track) -> Track {
    super::state::track_projection(track)
}
