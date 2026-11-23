use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};
use ytm_core::{BoxFuture, BridgePort, OutputGatePort};
use ytm_protocol::{
    BridgeEpoch, CoreEpoch, Envelope, MessageKind, OutputGateControlPayload,
    OutputGateResultPayload, PROTOCOL_VERSION, PlayerCommandPayload, QueryError, QueryErrorCode,
};

const MAX_PENDING_OUTPUT_GATES: usize = 32;

/// Generates strictly monotonic outbound sequence numbers for the `core` epoch.
#[derive(Debug, Default)]
pub struct SeqGenerator {
    next: AtomicU64,
}

impl SeqGenerator {
    pub fn new() -> Self {
        Self {
            next: AtomicU64::new(1),
        }
    }

    pub fn next_seq(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }
}

/// Shared outbound envelope sender pointing to the single serialized stdout writer task.
#[derive(Clone)]
pub struct OutboundSender {
    tx: mpsc::Sender<Envelope>,
    core_epoch: Arc<Mutex<Option<CoreEpoch>>>,
    seq: Arc<SeqGenerator>,
}

impl OutboundSender {
    pub fn new(tx: mpsc::Sender<Envelope>) -> Self {
        Self {
            tx,
            core_epoch: Arc::new(Mutex::new(None)),
            seq: Arc::new(SeqGenerator::new()),
        }
    }

    pub fn set_core_epoch(&self, epoch: CoreEpoch) {
        if let Ok(mut guard) = self.core_epoch.lock() {
            *guard = Some(epoch);
        }
    }

    pub fn current_core_epoch(&self) -> Option<CoreEpoch> {
        self.core_epoch.lock().ok().and_then(|g| g.clone())
    }

    pub fn make_envelope(
        &self,
        kind: MessageKind,
        payload: impl serde::Serialize,
    ) -> Result<Envelope, QueryError> {
        let mut env =
            Envelope::new(kind, self.seq.next_seq(), payload).map_err(|e| QueryError {
                code: QueryErrorCode::InvalidPayload,
                retriable: false,
                retry_after_ms: None,
                safe_message: format!("failed to serialize outbound payload: {e}"),
                request_id: None,
            })?;
        env.protocol_version = PROTOCOL_VERSION;
        env.core_epoch = self.current_core_epoch();
        Ok(env)
    }

    pub async fn send_envelope(&self, mut env: Envelope) -> Result<(), QueryError> {
        if env.core_epoch.is_none() {
            env.core_epoch = self.current_core_epoch();
        }
        if env.seq == 0 {
            env.seq = self.seq.next_seq();
        }
        self.tx.send(env).await.map_err(|_| QueryError {
            code: QueryErrorCode::BridgeIncompatible,
            retriable: false,
            retry_after_ms: None,
            safe_message: "daemon stdout writer channel closed".to_string(),
            request_id: None,
        })
    }
}

/// `BridgePort` emitting `player.command` and `player.snapshotRequest` frames over stdout NDJSON.
#[derive(Clone)]
pub struct NdjsonBridgePort {
    outbound: OutboundSender,
}

impl NdjsonBridgePort {
    pub fn new(outbound: OutboundSender) -> Self {
        Self { outbound }
    }
}

impl BridgePort for NdjsonBridgePort {
    fn send_player_command(
        &self,
        bridge_epoch: BridgeEpoch,
        payload: PlayerCommandPayload,
    ) -> BoxFuture<'_, Result<(), QueryError>> {
        let outbound = self.outbound.clone();
        Box::pin(async move {
            let mut env = outbound.make_envelope(MessageKind::PlayerCommand, payload)?;
            env.bridge_epoch = Some(bridge_epoch);
            outbound.send_envelope(env).await
        })
    }

    fn request_snapshot(&self, bridge_epoch: BridgeEpoch) -> BoxFuture<'_, Result<(), QueryError>> {
        let outbound = self.outbound.clone();
        Box::pin(async move {
            let mut env = outbound
                .make_envelope(MessageKind::PlayerSnapshotRequest, serde_json::json!({}))?;
            env.bridge_epoch = Some(bridge_epoch);
            outbound.send_envelope(env).await
        })
    }
}

/// `OutputGatePort` emitting `core.outputGateControl` and awaiting `host.outputGateResult`.
#[derive(Clone)]
pub struct NdjsonOutputGatePort {
    outbound: OutboundSender,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<OutputGateResultPayload>>>>,
}

impl NdjsonOutputGatePort {
    pub fn new(outbound: OutboundSender) -> Self {
        Self {
            outbound,
            pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn complete_gate_result(&self, result: OutputGateResultPayload) -> bool {
        let sender = self
            .pending
            .lock()
            .ok()
            .and_then(|mut map| map.remove(&result.transaction_id));
        if let Some(tx) = sender {
            let _ = tx.send(result);
            true
        } else {
            false
        }
    }
}

/// Removes a pending gate transaction when its waiting future finishes or is dropped
/// (for example by the core's outer timeout), so abandoned entries never accumulate.
struct PendingGateGuard {
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<OutputGateResultPayload>>>>,
    transaction_id: String,
}

impl Drop for PendingGateGuard {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.pending.lock() {
            guard.remove(&self.transaction_id);
        }
    }
}

impl OutputGatePort for NdjsonOutputGatePort {
    fn set_output_gated(
        &self,
        transaction_id: String,
        gated: bool,
    ) -> BoxFuture<'_, Result<OutputGateResultPayload, QueryError>> {
        let outbound = self.outbound.clone();
        let pending = Arc::clone(&self.pending);
        Box::pin(async move {
            let (tx, rx) = oneshot::channel();
            {
                let mut guard = pending.lock().map_err(|_| QueryError {
                    code: QueryErrorCode::BridgeIncompatible,
                    retriable: false,
                    retry_after_ms: None,
                    safe_message: "output gate pending map poisoned".to_string(),
                    request_id: None,
                })?;
                if guard.len() >= MAX_PENDING_OUTPUT_GATES {
                    return Err(QueryError {
                        code: QueryErrorCode::Busy,
                        retriable: true,
                        retry_after_ms: Some(100),
                        safe_message: "too many concurrent output gate transactions".to_string(),
                        request_id: None,
                    });
                }
                guard.insert(transaction_id.clone(), tx);
            }
            let _cleanup = PendingGateGuard {
                pending: Arc::clone(&pending),
                transaction_id: transaction_id.clone(),
            };

            let env = outbound.make_envelope(
                MessageKind::CoreOutputGateControl,
                OutputGateControlPayload {
                    transaction_id,
                    gated,
                },
            )?;
            outbound.send_envelope(env).await?;

            rx.await.map_err(|_| QueryError {
                code: QueryErrorCode::Timeout,
                retriable: false,
                retry_after_ms: None,
                safe_message: "output gate result channel closed".to_string(),
                request_id: None,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn abandoned_gate_wait_does_not_leak_pending_entry() {
        let (tx, mut rx) = mpsc::channel(8);
        let port = NdjsonOutputGatePort::new(OutboundSender::new(tx));
        let res = tokio::time::timeout(
            Duration::from_millis(20),
            port.set_output_gated("gate-leak".to_string(), true),
        )
        .await;
        assert!(res.is_err(), "no host reply, so the outer timeout fires");
        assert!(rx.recv().await.is_some(), "control frame was emitted");
        assert!(port.pending.lock().unwrap().is_empty());
        assert!(!port.complete_gate_result(OutputGateResultPayload {
            transaction_id: "gate-leak".to_string(),
            applied: true,
            error: None,
        }));
    }
}
