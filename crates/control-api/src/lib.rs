//! Public music-control contract. Host and bridge frames are internal to the player.

#[cfg(all(unix, feature = "local"))]
pub mod local;

mod cue;
mod dj;
mod extended_media;
mod history;
mod operation;
mod preferences;
mod read;
mod request;
mod search;
mod share;

pub use cue::*;
pub use dj::*;
pub use extended_media::*;
pub use history::*;
pub use operation::*;
pub use preferences::*;
pub use read::*;
pub use request::*;
pub use search::*;
pub use share::*;
pub use ytm_protocol::{ArtistId, PlaylistId, QueueEntryId, RepeatMode, VideoId};

pub const CONTROL_VERSION: u32 = 1;
pub const DEFAULT_PAGE_SIZE: u16 = 20;
pub const MAX_PAGE_SIZE: u16 = 100;
pub const MAX_WRITE_ITEMS: usize = 50;
pub const MAX_CONTENT_BYTES: usize = 64 * 1024;
pub const MAX_FRAME_BYTES: usize = MAX_CONTENT_BYTES + 4096;
