use std::{
    io,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    net::UnixStream,
    sync::Mutex,
    time::{Instant, sleep, timeout, timeout_at},
};
use ytm_control_api::{
    CONTROL_VERSION, ControlError, ControlRequest, ControlResponse, ErrorCode, RequestFrame,
    RetryDisposition, ServerFrame,
    local::{
        ClientCredential, ClientHello, ServerHello, auto_socket_path, framed, load_private_json,
        receive, send, validate_peer, validate_private_dir, validate_socket,
    },
};

const REPLY_TIMEOUT: Duration = Duration::from_secs(22);
const START_TIMEOUT: Duration = Duration::from_secs(12);

pub struct ControlClient {
    credential: PathBuf,
    allow_start: bool,
    startup: Mutex<()>,
}

impl ControlClient {
    pub fn new(credential: PathBuf, allow_start: bool) -> Self {
        Self {
            credential,
            allow_start,
            startup: Mutex::new(()),
        }
    }

    pub async fn call(&self, request: ControlRequest) -> Result<ControlResponse, ControlError> {
        let path = self.credential.clone();
        let credential =
            tokio::task::spawn_blocking(move || load_private_json::<ClientCredential>(&path))
                .await
                .ok()
                .and_then(Result::ok)
                .ok_or_else(|| {
                    ControlError::new(
                        ErrorCode::PermissionDenied,
                        RetryDisposition::NotRetryable,
                        "a private music control credential file is required",
                    )
                })?;
        let socket =
            auto_socket_path().map_err(|_| unavailable("a private XDG_RUNTIME_DIR is required"))?;
        let submit = matches!(request, ControlRequest::Submit(_));
        let stream = self
            .attach(&socket)
            .await
            .map_err(|error| before_write(submit, error))?;
        exchange(stream, credential, request).await
    }

    async fn attach(&self, path: &Path) -> Result<UnixStream, ControlError> {
        match connect(path).await {
            Ok(stream) => return Ok(stream),
            Err(err) if self.allow_start && is_unavailable(&err) => {}
            Err(_) => {
                return Err(unavailable(
                    "YTMcp is not available on the local control socket",
                ));
            }
        }
        let _start = self.startup.lock().await;
        match connect(path).await {
            Ok(stream) => return Ok(stream),
            Err(err) if is_unavailable(&err) => {}
            Err(_) => {
                return Err(unavailable(
                    "YTMcp is not available on the local control socket",
                ));
            }
        }
        if !Path::new("/opt/ytmcp/resources/config/mcp-background-v1").is_file() {
            return Err(unavailable(
                "the installed YTMcp launcher does not declare background start support",
            ));
        }
        let mut child = tokio::process::Command::new("/usr/bin/ytmcp-player")
            .args(["--background", "--no-activate"])
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| unavailable("the installed YTMcp launcher could not be started"))?;
        // Closing the adapter does not stop the player.
        tokio::spawn(async move {
            let _ = child.wait().await;
        });
        timeout(START_TIMEOUT, async {
            loop {
                match connect(path).await {
                    Ok(stream) => return Ok(stream),
                    Err(err) if is_unavailable(&err) => sleep(Duration::from_millis(100)).await,
                    Err(_) => {
                        return Err(unavailable(
                            "the local control endpoint could not be verified",
                        ));
                    }
                }
            }
        })
        .await
        .map_err(|_| unavailable("YTMcp did not provide a local control socket after launch"))?
    }
}

/// Runs one handshake and request over `stream`. A failure before the request frame is fully
/// written stays `safeReadRetry`; for a `Submit`, a failure after that point becomes
/// `queryOperation`, because the daemon may already have accepted the operation.
async fn exchange(
    stream: UnixStream,
    credential: ClientCredential,
    request: ControlRequest,
) -> Result<ControlResponse, ControlError> {
    let submit = matches!(request, ControlRequest::Submit(_));
    let deadline = Instant::now() + REPLY_TIMEOUT;
    let timed_out = || unavailable("the music control connection timed out");
    let mut connection = framed(stream);
    let delivery = async {
        send(
            &mut connection,
            &ClientHello {
                control_version: CONTROL_VERSION,
                principal: credential.principal,
                token: credential.token,
            },
        )
        .await
        .map_err(|_| unavailable("could not send the local music control handshake"))?;
        let hello = receive::<ServerHello>(&mut connection)
            .await
            .ok()
            .flatten()
            .ok_or_else(|| unavailable("the local music control handshake did not finish"))?;
        if let Some(error) = hello.error {
            return Err(error);
        }
        if hello.control_version != CONTROL_VERSION {
            return Err(ControlError::new(
                ErrorCode::IncompatibleVersion,
                RetryDisposition::NotRetryable,
                "the player and adapter use different control protocol versions",
            ));
        }
        send(
            &mut connection,
            &RequestFrame {
                control_version: CONTROL_VERSION,
                request_id: 1,
                request,
            },
        )
        .await
        .map_err(|_| unavailable("could not send the music control request"))
    };
    timeout_at(deadline, delivery)
        .await
        .map_err(|_| timed_out())?
        .map_err(|error| before_write(submit, error))?;
    let reply = timeout_at(deadline, receive::<ServerFrame>(&mut connection))
        .await
        .map_err(|_| timed_out());
    match reply {
        Ok(Ok(Some(ServerFrame::Response {
            request_id: 1,
            response,
        }))) => Ok(response),
        Err(error) if !submit => Err(error),
        _ if submit => Err(ControlError::new(
            ErrorCode::Unavailable,
            RetryDisposition::QueryOperation,
            "the write request was sent but its response did not arrive; query the operation \
             by its operationId before resending",
        )),
        _ => Err(unavailable(
            "the local music control response did not arrive",
        )),
    }
}

/// Marks a failure that happened before any request frame reached the daemon. A `Submit` can
/// then be resent unchanged, because no operation exists yet.
fn before_write(submit: bool, mut error: ControlError) -> ControlError {
    if submit && error.retry_disposition == RetryDisposition::SafeReadRetry {
        error.message = format!(
            "{}; the write request was not sent, so the same operationId can be resent safely",
            error.message
        );
    }
    error
}

async fn connect(path: &Path) -> io::Result<UnixStream> {
    if let Some(parent) = path.parent() {
        validate_private_dir(parent)?;
    }
    validate_socket(path)?;
    let stream = UnixStream::connect(path).await?;
    validate_peer(&stream)?;
    Ok(stream)
}

fn is_unavailable(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    )
}

fn unavailable(message: &str) -> ControlError {
    ControlError::new(
        ErrorCode::Unavailable,
        RetryDisposition::SafeReadRetry,
        message,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ytm_control_api::{Preconditions, WriteAction, WriteInput};

    fn credential() -> ClientCredential {
        ClientCredential {
            principal: "test".into(),
            token: "token".into(),
        }
    }

    fn pause() -> ControlRequest {
        ControlRequest::Submit(WriteInput {
            operation_id: "op-1".into(),
            preconditions: Preconditions {
                expected_account_epoch: None,
                expected_bridge_epoch: None,
                expected_queue_revision: None,
                expected_entity_revision: None,
            },
            action: WriteAction::Pause,
        })
    }

    /// Serves the handshake, then optionally reads the request frame before hanging up.
    async fn run(request: ControlRequest, read_request: bool) -> ControlError {
        let (client, server) = UnixStream::pair().unwrap();
        let daemon = tokio::spawn(async move {
            let mut connection = framed(server);
            let _: ClientHello = receive(&mut connection).await.unwrap().unwrap();
            if read_request {
                send(
                    &mut connection,
                    &ServerHello {
                        control_version: CONTROL_VERSION,
                        error: None,
                    },
                )
                .await
                .unwrap();
                let _: RequestFrame = receive(&mut connection).await.unwrap().unwrap();
            }
        });
        let error = exchange(client, credential(), request).await.unwrap_err();
        daemon.await.unwrap();
        error
    }

    #[tokio::test]
    async fn submit_lost_before_the_frame_is_written_can_be_resent() {
        let error = run(pause(), false).await;
        assert_eq!(error.retry_disposition, RetryDisposition::SafeReadRetry);
        assert!(
            error
                .message
                .contains("same operationId can be resent safely")
        );
    }

    #[tokio::test]
    async fn submit_lost_after_the_frame_is_written_must_be_queried() {
        let error = run(pause(), true).await;
        assert_eq!(error.retry_disposition, RetryDisposition::QueryOperation);
    }

    #[tokio::test]
    async fn read_lost_after_the_frame_is_written_stays_a_safe_retry() {
        let error = run(ControlRequest::GetAccount, true).await;
        assert_eq!(error.retry_disposition, RetryDisposition::SafeReadRetry);
        assert!(!error.message.contains("operationId"));
    }
}
