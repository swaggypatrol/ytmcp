use std::fs::{self, File};
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Semaphore, broadcast};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tracing::warn;
use ytm_control_api::{
    CONTROL_VERSION, ControlError, ControlEvent, ControlRequest, ControlResponse, ErrorCode,
    EventCursor, GrantAction, MAX_CONTENT_BYTES, RequestFrame, RetryDisposition, ServerFrame,
    local::{
        ClientHello, MAX_GRANTS, Registration, ServerHello, TOKEN_BYTES, auto_socket_path, framed,
        load_private_json, receive, send, validate_peer, validate_private_dir, validate_socket,
    },
};
use ytm_core::BoxFuture;
use ytm_core::control::{ControlService, DispatchAuthority, DispatchGrant, ReadGrant};
use ytm_protocol::{AccountScope, CoreEpoch, QueueCompleteness, QueueSummary, StateDelta, VideoId};

const MAX_CLIENTS: usize = 16;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_REQUEST_ID: u64 = 9_007_199_254_740_991;
/// A complete queue with fewer upcoming entries than this raises `queueLow`.
const QUEUE_LOW_UPCOMING: usize = 3;

type Connection = tokio_util::codec::Framed<UnixStream, tokio_util::codec::LinesCodec>;

pub struct ControlServer {
    shutdown: CancellationToken,
    worker: JoinHandle<()>,
}

impl ControlServer {
    pub fn start(service: Arc<ControlService>, registrations: PathBuf) -> io::Result<Option<Self>> {
        match registrations.symlink_metadata() {
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            other => {
                other?;
            }
        }
        validate_private_dir(
            registrations
                .parent()
                .ok_or_else(|| io::Error::other("no registration directory"))?,
        )?;
        let _: Vec<Registration> = load_private_json(&registrations)?;
        let bound = BoundSocket::bind(auto_socket_path()?)?;
        let shutdown = CancellationToken::new();
        let cancel = shutdown.clone();
        let worker = tokio::spawn(async move {
            let slots = Arc::new(Semaphore::new(MAX_CLIENTS));
            let mut clients = JoinSet::new();
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = clients.join_next(), if !clients.is_empty() => {},
                    result = bound.listener.accept() => {
                        match result {
                            Ok((stream, _)) => {
                                if let Ok(slot) = slots.clone().try_acquire_owned() {
                                    let service = service.clone();
                                    let registrations = registrations.clone();
                                    clients.spawn(async move {
                                        let _slot = slot;
                                        let _ = serve_connection(stream, service, registrations).await;
                                    });
                                }
                            }
                            Err(err) => {
                                warn!(kind = ?err.kind(), "local music control listener stopped");
                                break;
                            }
                        }
                    }
                }
            }
            clients.shutdown().await;
        });
        Ok(Some(Self { shutdown, worker }))
    }

    pub async fn shutdown(self) {
        self.shutdown.cancel();
        let _ = self.worker.await;
    }
}

struct BoundSocket {
    path: PathBuf,
    listener: UnixListener,
    inode: u64,
    _lock: File,
}

impl BoundSocket {
    fn bind(path: PathBuf) -> io::Result<Self> {
        use rustix::fs::{Mode, OFlags, open};

        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("control socket has no parent"))?;
        match fs::DirBuilder::new().mode(0o700).create(parent) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
        validate_private_dir(parent)?;
        let lock = File::from(open(
            parent.join("control.lock"),
            OFlags::RDWR | OFlags::CREATE | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
            Mode::RUSR | Mode::WUSR,
        )?);
        let facts = lock.metadata()?;
        if !facts.is_file()
            || facts.uid() != rustix::process::geteuid().as_raw()
            || facts.permissions().mode() & 0o777 != 0o600
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "invalid music control lock",
            ));
        }
        lock.try_lock().map_err(io::Error::other)?;
        match validate_socket(&path) {
            Ok(_) => fs::remove_file(&path)?,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
        let listener = UnixListener::bind(&path)?;
        if let Err(error) = fs::set_permissions(&path, fs::Permissions::from_mode(0o600)) {
            let _ = fs::remove_file(&path);
            return Err(error);
        }
        let inode = validate_socket(&path)?.ino();
        Ok(Self {
            path,
            listener,
            inode,
            _lock: lock,
        })
    }
}

impl Drop for BoundSocket {
    fn drop(&mut self) {
        if validate_socket(&self.path).is_ok_and(|facts| facts.ino() == self.inode) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn now_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

#[derive(Clone)]
struct Identity {
    principal: String,
    token_sha256: [u8; 32],
}

async fn authorize(path: &Path, identity: &Identity) -> Option<Registration> {
    let path = path.to_owned();
    let registrations =
        tokio::task::spawn_blocking(move || load_private_json::<Vec<Registration>>(&path))
            .await
            .ok()?
            .ok()?;
    if registrations.len() > MAX_GRANTS {
        return None;
    }
    let mut matches = registrations
        .into_iter()
        .filter(|grant| grant.principal == identity.principal);
    let grant = matches.next()?;
    if matches.next().is_some() || grant.expires_at_ms <= now_ms() {
        return None;
    }
    let expected = hex::decode(&grant.token_sha256).ok()?;
    if bool::from(identity.token_sha256.as_slice().ct_eq(&expected)) {
        Some(grant)
    } else {
        None
    }
}

struct SocketAuthority {
    registrations: PathBuf,
    identity: Identity,
    scope: Option<AccountScope>,
}

impl DispatchAuthority for SocketAuthority {
    fn authorize(&self, action: GrantAction) -> BoxFuture<'_, Result<DispatchGrant, ControlError>> {
        Box::pin(async move {
            let grant = authorize(&self.registrations, &self.identity)
                .await
                .filter(|grant| {
                    grant.account_scope == self.scope && grant.actions.contains(&action)
                })
                .ok_or_else(|| {
                    denied("this registration does not grant the requested music operation")
                })?;
            Ok(DispatchGrant {
                deadline_wall_ms: grant.expires_at_ms,
                editable_playlists: grant.editable_playlists,
            })
        })
    }
}

async fn serve_connection(
    stream: UnixStream,
    service: Arc<ControlService>,
    registrations: PathBuf,
) -> io::Result<()> {
    validate_peer(&stream)?;
    let mut connection = framed(stream);
    let Some(hello) = timeout(HANDSHAKE_TIMEOUT, receive::<ClientHello>(&mut connection)).await??
    else {
        return Ok(());
    };
    if hello.control_version != CONTROL_VERSION {
        return send(
            &mut connection,
            &ServerHello {
                control_version: CONTROL_VERSION,
                error: Some(error(
                    ErrorCode::IncompatibleVersion,
                    "the local control protocol version differs",
                )),
            },
        )
        .await;
    }
    let identity = Identity {
        principal: hello.principal,
        token_sha256: Sha256::digest(hello.token.as_bytes()).into(),
    };
    let grant = if hello.token.len() == TOKEN_BYTES && valid_principal(&identity.principal) {
        authorize(&registrations, &identity).await
    } else {
        None
    };
    drop(hello.token);
    send(
        &mut connection,
        &ServerHello {
            control_version: CONTROL_VERSION,
            error: grant
                .is_none()
                .then(|| denied("no matching active music control registration")),
        },
    )
    .await?;
    let Some(grant) = grant else { return Ok(()) };
    let scope = grant.account_scope;
    let authority = Arc::new(SocketAuthority {
        registrations: registrations.clone(),
        identity: identity.clone(),
        scope: scope.clone(),
    });
    let mut events = None;

    loop {
        tokio::select! {
            incoming = receive::<serde_json::Value>(&mut connection) => {
                let Some(incoming) = incoming? else { break };
                let incoming = match decode_request(incoming) {
                    Ok(frame) => frame,
                    Err(Some(request_id)) => {
                        respond(&mut connection, request_id, ControlResponse::Error(error(ErrorCode::InvalidInput, "the music control request frame is not valid for this control protocol"))).await?;
                        continue;
                    }
                    Err(None) => break,
                };
                let grant = authorize(&registrations, &identity).await.filter(|grant| grant.account_scope == scope);
                let Some(grant) = grant else {
                    respond(&mut connection, incoming.request_id, ControlResponse::Error(denied("the music control grant expired or changed"))).await?;
                    break;
                };
                let access = ReadGrant::new(identity.principal.clone(), grant.account_scope);
                let response = if incoming.control_version != CONTROL_VERSION {
                    ControlResponse::Error(error(ErrorCode::IncompatibleVersion,"the local control protocol version differs"))
                } else if incoming.request_id == 0 || incoming.request_id > MAX_REQUEST_ID {
                    ControlResponse::Error(error(ErrorCode::InvalidInput,"requestId is outside the protocol range"))
                } else if let ControlRequest::Submit(input) = incoming.request {
                    match service.submit(access, authority.clone(), input).await {
                        Ok(receipt) => ControlResponse::Operation(receipt),
                        Err(err) => ControlResponse::Error(err),
                    }
                } else if let ControlRequest::Subscribe(input) = incoming.request {
                    match service.subscribe(&access, input.preview_limit).await {
                        Ok((ControlResponse::Context(snapshot), receiver)) => {
                            events = Some(Subscription {
                                receiver,
                                core_epoch: snapshot.cursor.core_epoch.clone(),
                                revision: snapshot.cursor.revision,
                                previous_track: snapshot.player.track_instance_id.clone(),
                            });
                            ControlResponse::Context(snapshot)
                        }
                        Ok((other, _)) => other,
                        Err(err) => ControlResponse::Error(err),
                    }
                } else {
                    timeout(REQUEST_TIMEOUT,service.execute(&access,incoming.request)).await
                        .unwrap_or_else(|_| ControlResponse::Error(ControlError::new(
                            ErrorCode::Unavailable, RetryDisposition::SafeReadRetry, "local control request timed out")))
                };
                respond(&mut connection, incoming.request_id,response).await?;
            }
            notification = next_event(&mut events, &scope, &identity.principal) => {
                match notification {
                    Notification::Send(event) => {
                        if !authorize(&registrations,&identity).await.is_some_and(|grant| grant.account_scope == scope) {
                            break;
                        }
                        let event = match event {
                            Pending::Ready(event) => event,
                            Pending::Queue { cursor, summary } => {
                                let access = ReadGrant::new(identity.principal.clone(), scope.clone());
                                queue_event(&service, &access, cursor, summary).await
                            }
                        };
                        timeout(WRITE_TIMEOUT,send(&mut connection,&ServerFrame::Event { event })).await??;
                    }
                    Notification::ScopeChanged => break,
                    Notification::Closed => { events = None; }
                }
            }
        }
    }
    Ok(())
}

/// Decodes a JSON line as a request frame. A frame that cannot be decoded yields its in-range
/// `requestId`, so the caller can answer with a non-retryable error instead of hanging up.
fn decode_request(line: serde_json::Value) -> Result<RequestFrame, Option<u64>> {
    let request_id = line
        .get("requestId")
        .and_then(serde_json::Value::as_u64)
        .filter(|id| (1..=MAX_REQUEST_ID).contains(id));
    serde_json::from_value(line).map_err(|_| request_id)
}

fn valid_principal(principal: &str) -> bool {
    !principal.is_empty()
        && principal.len() <= 64
        && principal
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

async fn respond(
    connection: &mut Connection,
    request_id: u64,
    mut response: ControlResponse,
) -> io::Result<()> {
    if serde_json::to_vec(&response)
        .map_err(io::Error::other)?
        .len()
        > MAX_CONTENT_BYTES
    {
        response = ControlResponse::Error(error(
            ErrorCode::InvalidInput,
            "response exceeds the content budget; request a smaller page or preview",
        ));
    }
    timeout(
        WRITE_TIMEOUT,
        send(
            connection,
            &ServerFrame::Response {
                request_id,
                response,
            },
        ),
    )
    .await?
}

struct Subscription {
    receiver: broadcast::Receiver<StateDelta>,
    core_epoch: CoreEpoch,
    revision: u64,
    previous_track: Option<ytm_protocol::TrackInstanceId>,
}

/// An event that is either final, or a queue change whose `queueLow` test may need a queue read.
enum Pending {
    Ready(ControlEvent),
    Queue {
        cursor: EventCursor,
        summary: QueueSummary,
    },
}

enum Notification {
    Send(Pending),
    ScopeChanged,
    Closed,
}

async fn next_event(
    subscription: &mut Option<Subscription>,
    scope: &Option<AccountScope>,
    principal: &str,
) -> Notification {
    let Some(sub) = subscription.as_mut() else {
        return std::future::pending().await;
    };
    loop {
        let delta = match sub.receiver.recv().await {
            Ok(delta) => delta,
            Err(broadcast::error::RecvError::Closed) => return Notification::Closed,
            Err(broadcast::error::RecvError::Lagged(_)) => {
                let cursor = EventCursor {
                    core_epoch: sub.core_epoch.clone(),
                    revision: sub.revision,
                };
                *subscription = None;
                return Notification::Send(Pending::Ready(ControlEvent::EventGap { cursor }));
            }
        };
        sub.revision = delta.revision();
        let cursor = EventCursor {
            core_epoch: sub.core_epoch.clone(),
            revision: sub.revision,
        };
        let event = match delta {
            StateDelta::ManualTakeover { generation, .. } => {
                ControlEvent::ManualTakeover { cursor, generation }
            }
            StateDelta::ControlOperationChanged {
                principal: owner,
                operation_id,
                ..
            } if owner == principal => ControlEvent::OperationChanged {
                cursor,
                operation_id,
            },
            StateDelta::AuthChanged { auth_state, .. } => {
                if &auth_state.account_scope != scope {
                    return Notification::ScopeChanged;
                }
                ControlEvent::AuthChanged {
                    cursor,
                    account: ytm_control_api::AccountState {
                        state: auth_state.state,
                        account_epoch: auth_state.account_epoch,
                        player_catalog_matched: auth_state.player_catalog_matched,
                    },
                }
            }
            StateDelta::PlayerChanged { player, .. }
                if sub.previous_track != player.track_instance_id =>
            {
                sub.previous_track = player.track_instance_id;
                let video_id: Option<VideoId> = player.current_track.map(|track| track.video_id);
                ControlEvent::TrackChanged { cursor, video_id }
            }
            StateDelta::QueueReplaced { queue_summary, .. } => {
                return Notification::Send(Pending::Queue {
                    cursor,
                    summary: queue_summary,
                });
            }
            StateDelta::ResourceInvalidated {
                resource_kind,
                resource_id,
                ..
            } => ControlEvent::ResourceInvalidated {
                cursor,
                kind: resource_kind,
                id: resource_id,
            },
            _ => continue,
        };
        return Notification::Send(Pending::Ready(event));
    }
}

async fn queue_event(
    service: &ControlService,
    access: &ReadGrant,
    cursor: EventCursor,
    summary: QueueSummary,
) -> ControlEvent {
    let queue_revision = summary.queue_revision;
    // A complete summary can be answered from the preview; otherwise (preview too short, or an
    // autoplay tail after a fully loaded explicit list) the core reads the full mirror.
    let preview = (summary.completeness == QueueCompleteness::Complete)
        .then(|| upcoming_in_preview(&summary))
        .flatten();
    let low = match preview {
        Some(upcoming) => upcoming < QUEUE_LOW_UPCOMING,
        None => timeout(
            WRITE_TIMEOUT,
            service.upcoming_in_complete_queue(access, queue_revision),
        )
        .await
        .ok()
        .flatten()
        .is_some_and(|upcoming| upcoming < QUEUE_LOW_UPCOMING),
    };
    if low {
        ControlEvent::QueueLow {
            cursor,
            queue_revision,
        }
    } else {
        ControlEvent::QueueChanged {
            cursor,
            queue_revision,
        }
    }
}

/// Upcoming entries after the current one, when the current entry is inside the preview.
fn upcoming_in_preview(summary: &QueueSummary) -> Option<usize> {
    let current = summary.current_entry_id.as_ref()?;
    summary
        .preview_entries
        .iter()
        .position(|entry| &entry.entry_id == current)
        .map(|index| (summary.loaded_count as usize).saturating_sub(index + 1))
}

fn denied(message: &str) -> ControlError {
    error(ErrorCode::PermissionDenied, message)
}

fn error(code: ErrorCode, message: &str) -> ControlError {
    ControlError::new(code, RetryDisposition::NotRetryable, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ytm_core::{CoreDeps, CoreEngine, EngineConfig};
    use ytm_protocol::{
        BridgeEpoch, BridgeReadyPayload, QueueEntry, QueueEntryId, QueueSnapshot, Track,
        TrackAvailability,
    };

    #[test]
    fn undecodable_frames_keep_their_request_id() {
        let unknown_field = serde_json::json!({
            "controlVersion": CONTROL_VERSION,
            "requestId": 7,
            "request": { "type": "getAccount" },
            "extra": true,
        });
        assert_eq!(decode_request(unknown_field).unwrap_err(), Some(7));
        let no_id = serde_json::json!({ "request": "nonsense" });
        assert_eq!(decode_request(no_id).unwrap_err(), None);
        let out_of_range = serde_json::json!({ "requestId": MAX_REQUEST_ID + 1 });
        assert_eq!(decode_request(out_of_range).unwrap_err(), None);
    }

    fn entry(index: usize) -> QueueEntry {
        let video_id = VideoId::new(format!("video{index:06}")).unwrap();
        QueueEntry {
            entry_id: QueueEntryId::new(format!("qe-{index}")).unwrap(),
            video_id: video_id.clone(),
            track: Track {
                video_id,
                title: "Synthetic song".into(),
                artists: vec![],
                album: None,
                duration_ms: None,
                cover: None,
                availability: TrackAvailability::Available,
                version_tag: None,
                is_liked: None,
            },
            availability: TrackAvailability::Available,
        }
    }

    /// Loads a complete 150-entry queue whose current entry is past the 100-entry preview.
    async fn queue_event_at(current: usize) -> ControlEvent {
        let engine = CoreEngine::start(CoreDeps::default(), EngineConfig::default());
        let bridge_epoch = BridgeEpoch::new("fixture-queue-bridge").unwrap();
        assert!(
            engine
                .on_bridge_ready(
                    1,
                    BridgeReadyPayload {
                        bridge_epoch: bridge_epoch.clone(),
                        bridge_version: "0.1.0".into(),
                        capabilities: Default::default(),
                        account_hint: None,
                        logged_in_observed: false,
                    },
                )
                .await
                .unwrap()
        );
        assert!(
            engine
                .on_bridge_queue_observed(
                    2,
                    QueueSnapshot {
                        bridge_epoch: Some(bridge_epoch),
                        queue_revision: 1,
                        current_entry_id: Some(entry(current).entry_id),
                        entries: (0..150).map(entry).collect(),
                        loaded_range: None,
                        completeness: QueueCompleteness::Complete,
                        has_more: Some(false),
                        explicit_complete: None,
                        may_load_more: None,
                    },
                )
                .await
                .unwrap()
        );
        let summary = engine.snapshot().await.unwrap().queue_summary;
        assert_eq!(summary.preview_entries.len(), 100);
        let service = ControlService::new(engine);
        let access = ReadGrant::new("fixture-client".into(), None);
        let cursor = EventCursor {
            core_epoch: CoreEpoch::new("fixture-core").unwrap(),
            revision: 1,
        };
        queue_event(&service, &access, cursor, summary).await
    }

    #[tokio::test]
    async fn queue_low_finds_a_current_entry_past_the_preview() {
        assert!(matches!(
            queue_event_at(148).await,
            ControlEvent::QueueLow { .. }
        ));
        assert!(matches!(
            queue_event_at(120).await,
            ControlEvent::QueueChanged { .. }
        ));
    }
}
