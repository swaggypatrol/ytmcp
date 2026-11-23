//! Combines the host's OS connectivity signal with the daemon's own traffic-based reachability
//! into the one `networkState` the core publishes and the catalog obeys.

use std::sync::{Arc, Mutex};
use ytm_core::CoreEngine;
use ytm_data::CatalogService;
use ytm_protocol::NetworkState;

/// Offline when either source says so; otherwise the host's view.
pub fn effective_network(host: NetworkState, traffic_offline: bool) -> NetworkState {
    if traffic_offline {
        NetworkState::Offline
    } else {
        host
    }
}

pub struct NetworkStatus {
    sources: Mutex<(NetworkState, bool)>,
    catalog: Arc<CatalogService>,
    core: Arc<CoreEngine>,
}

impl NetworkStatus {
    pub fn new(catalog: Arc<CatalogService>, core: Arc<CoreEngine>) -> Self {
        Self {
            sources: Mutex::new((NetworkState::Unknown, false)),
            catalog,
            core,
        }
    }

    /// Records the host's state (hello or change) and returns the effective state. The caller
    /// passes it to the core (host.hello carries it in its payload).
    pub fn set_host(&self, host: NetworkState) -> NetworkState {
        let effective = {
            let mut s = self.sources.lock().unwrap_or_else(|e| e.into_inner());
            s.0 = host;
            effective_network(s.0, s.1)
        };
        self.catalog
            .set_network_offline(effective == NetworkState::Offline);
        effective
    }

    pub async fn set_traffic_offline(&self, offline: bool) {
        let effective = {
            let mut s = self.sources.lock().unwrap_or_else(|e| e.into_inner());
            s.1 = offline;
            effective_network(s.0, s.1)
        };
        self.catalog
            .set_network_offline(effective == NetworkState::Offline);
        let _ = self.core.on_host_network_changed(effective).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn either_source_offline_wins() {
        assert_eq!(
            effective_network(NetworkState::Online, true),
            NetworkState::Offline
        );
        assert_eq!(
            effective_network(NetworkState::Offline, false),
            NetworkState::Offline
        );
        assert_eq!(
            effective_network(NetworkState::Online, false),
            NetworkState::Online
        );
        assert_eq!(
            effective_network(NetworkState::Unknown, false),
            NetworkState::Unknown
        );
    }
}
