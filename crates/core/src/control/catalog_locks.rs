use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use ytm_control_api::ControlError;
use ytm_protocol::{AccountScope, AlbumId, PlaylistId, VideoId};

#[derive(PartialEq, Eq, Hash)]
pub(super) enum CatalogTarget {
    Playlist(Option<PlaylistId>),
    Song(VideoId),
    Album(AlbumId),
}

type ResourceKey = (AccountScope, CatalogTarget);

#[derive(Default)]
pub(super) struct CatalogLocks(Mutex<HashMap<ResourceKey, Weak<tokio::sync::Mutex<()>>>>);

impl CatalogLocks {
    pub(super) fn for_target(
        &self,
        scope: AccountScope,
        target: CatalogTarget,
    ) -> Result<Arc<tokio::sync::Mutex<()>>, ControlError> {
        let key = (scope, target);
        let mut locks = self
            .0
            .lock()
            .map_err(|_| super::unavailable("catalog scheduler is unavailable"))?;
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(existing) = locks.get(&key).and_then(Weak::upgrade) {
            return Ok(existing);
        }
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        locks.insert(key, Arc::downgrade(&lock));
        Ok(lock)
    }
}
