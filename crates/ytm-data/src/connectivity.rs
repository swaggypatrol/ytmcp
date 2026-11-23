//! Traffic-based reachability of YouTube Music.
//!
//! The OS link state misses outages that leave an interface up (a per-app firewall cut, a VPN
//! interface keeping an address while Wi-Fi is down). The monitor therefore watches the daemon's
//! own traffic: a network-class failure of a catalog request (connect/DNS/TLS/timeout) triggers
//! an immediate bounded probe of `music.youtube.com`, a low-rate probe notices an idle outage,
//! and while offline it probes more often until a probe succeeds again.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Notify, watch};
use tokio_util::sync::CancellationToken;
use tracing::info;
use ytm_core::ports::BoxFuture;

/// One bounded reachability check; `true` when the origin answered at all.
pub trait ReachabilityProbe: Send + Sync {
    fn probe(&self) -> BoxFuture<'_, bool>;
}

/// Unauthenticated `HEAD https://music.youtube.com/generate_204`; any HTTP status counts as
/// reachable, only transport failures (connect, DNS, TLS, timeout) count as unreachable.
pub struct HttpReachabilityProbe {
    client: reqwest::Client,
    url: String,
}

pub const MUSIC_PROBE_URL: &str = "https://music.youtube.com/generate_204";

impl HttpReachabilityProbe {
    pub fn new(url: impl Into<String>, timeout: Duration) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder()
            .use_rustls_tls()
            .timeout(timeout)
            .connect_timeout(timeout)
            .build()?;
        Ok(Self {
            client,
            url: url.into(),
        })
    }
}

impl ReachabilityProbe for HttpReachabilityProbe {
    fn probe(&self) -> BoxFuture<'_, bool> {
        Box::pin(async move { self.client.head(&self.url).send().await.is_ok() })
    }
}

#[derive(Debug, Clone)]
pub struct ConnectivityConfig {
    /// Probe period while reachable, bounding how late an idle outage is noticed.
    pub online_interval: Duration,
    /// Probe period while unreachable, bounding how late recovery is noticed.
    pub offline_interval: Duration,
    /// Gap before the confirming second probe; one lost probe is not an outage.
    pub confirm_gap: Duration,
}

impl Default for ConnectivityConfig {
    fn default() -> Self {
        // With a 5 s probe timeout an idle outage is reported within ~15 + 5 + 2 + 5 s.
        Self {
            online_interval: Duration::from_secs(15),
            offline_interval: Duration::from_secs(5),
            confirm_gap: Duration::from_secs(2),
        }
    }
}

/// Runs until `cancel`; publishes `true` on `offline` when two consecutive probes fail and
/// `false` once one succeeds. Catalog failures and host online events trigger immediate probes.
/// An online event replaces an in-flight probe started before the host link recovered.
pub async fn run_connectivity_monitor(
    probe: Arc<dyn ReachabilityProbe>,
    failures: Arc<Notify>,
    host_online: Arc<Notify>,
    config: ConnectivityConfig,
    offline: watch::Sender<bool>,
    cancel: CancellationToken,
) {
    loop {
        let is_offline = *offline.borrow();
        let wait = if is_offline {
            config.offline_interval
        } else {
            config.online_interval
        };
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(wait) => {}
            _ = failures.notified() => {}
            _ = host_online.notified() => {}
        }
        let reachable = loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return,
                _ = host_online.notified() => continue,
                r = probe_confirmed(probe.as_ref(), &config, is_offline) => break r,
            }
        };
        if reachable == is_offline {
            info!(reachable, "YouTube Music reachability changed");
            let _ = offline.send(!reachable);
        }
    }
}

async fn probe_confirmed(
    probe: &dyn ReachabilityProbe,
    config: &ConnectivityConfig,
    is_offline: bool,
) -> bool {
    if probe.probe().await {
        return true;
    }
    if is_offline {
        return false;
    }
    tokio::time::sleep(config.confirm_gap).await;
    probe.probe().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct FakeProbe {
        reachable: AtomicBool,
        calls: AtomicUsize,
    }

    impl ReachabilityProbe for FakeProbe {
        fn probe(&self) -> BoxFuture<'_, bool> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let r = self.reachable.load(Ordering::SeqCst);
            Box::pin(async move { r })
        }
    }

    async fn wait_for(rx: &mut watch::Receiver<bool>, want: bool) {
        tokio::time::timeout(Duration::from_secs(5), rx.wait_for(|v| *v == want))
            .await
            .expect("state change in time")
            .expect("sender alive");
    }

    #[tokio::test]
    async fn failure_signal_probes_at_once_and_recovery_is_noticed_while_offline() {
        let probe = Arc::new(FakeProbe {
            reachable: AtomicBool::new(true),
            calls: AtomicUsize::new(0),
        });
        let failures = Arc::new(Notify::new());
        let (tx, mut rx) = watch::channel(false);
        let cancel = CancellationToken::new();
        let config = ConnectivityConfig {
            // Only the failure signal can trigger a probe while online in this test.
            online_interval: Duration::from_secs(3600),
            offline_interval: Duration::from_millis(20),
            confirm_gap: Duration::from_millis(5),
        };
        let task = tokio::spawn(run_connectivity_monitor(
            probe.clone(),
            failures.clone(),
            Arc::new(Notify::new()),
            config,
            tx,
            cancel.clone(),
        ));

        // A request failure while the origin still answers keeps the state online.
        failures.notify_one();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(probe.calls.load(Ordering::SeqCst) >= 1);
        assert!(!*rx.borrow());

        // Unreachable: the next failure signal is confirmed by a second probe, then offline.
        probe.reachable.store(false, Ordering::SeqCst);
        let before = probe.calls.load(Ordering::SeqCst);
        failures.notify_one();
        wait_for(&mut rx, true).await;
        assert!(probe.calls.load(Ordering::SeqCst) >= before + 2);

        // Offline probes keep running without any request traffic; the first success recovers.
        probe.reachable.store(true, Ordering::SeqCst);
        wait_for(&mut rx, false).await;

        cancel.cancel();
        task.await.unwrap();
    }

    struct InterruptedProbe {
        first_started: Notify,
        calls: AtomicUsize,
    }

    impl ReachabilityProbe for InterruptedProbe {
        fn probe(&self) -> BoxFuture<'_, bool> {
            Box::pin(async move {
                if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    self.first_started.notify_one();
                    std::future::pending().await
                } else {
                    true
                }
            })
        }
    }

    #[tokio::test]
    async fn host_online_starts_a_new_probe_without_waiting_for_the_old_one() {
        let probe = Arc::new(InterruptedProbe {
            first_started: Notify::new(),
            calls: AtomicUsize::new(0),
        });
        let host_online = Arc::new(Notify::new());
        let (tx, mut rx) = watch::channel(true);
        let cancel = CancellationToken::new();
        let task = tokio::spawn(run_connectivity_monitor(
            probe.clone(),
            Arc::new(Notify::new()),
            host_online.clone(),
            ConnectivityConfig {
                offline_interval: Duration::from_secs(3600),
                ..ConnectivityConfig::default()
            },
            tx,
            cancel.clone(),
        ));

        host_online.notify_one();
        tokio::time::timeout(Duration::from_secs(5), probe.first_started.notified())
            .await
            .expect("host online should wake an idle monitor");
        host_online.notify_one();
        wait_for(&mut rx, false).await;
        assert_eq!(probe.calls.load(Ordering::SeqCst), 2);

        cancel.cancel();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn idle_outage_is_noticed_by_the_low_rate_probe() {
        let probe = Arc::new(FakeProbe {
            reachable: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
        });
        let (tx, mut rx) = watch::channel(false);
        let cancel = CancellationToken::new();
        let config = ConnectivityConfig {
            online_interval: Duration::from_millis(30),
            offline_interval: Duration::from_millis(30),
            confirm_gap: Duration::from_millis(5),
        };
        let task = tokio::spawn(run_connectivity_monitor(
            probe,
            Arc::new(Notify::new()),
            Arc::new(Notify::new()),
            config,
            tx,
            cancel.clone(),
        ));
        wait_for(&mut rx, true).await;
        cancel.cancel();
        task.await.unwrap();
    }
}
