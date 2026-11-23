use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tracing::warn;
use ytm_protocol::{
    CodecError, Envelope, MAX_FRAME_BYTES, QueryErrorCode, RequestId, decode_ndjson_bytes,
    encode_ndjson_line,
};

#[derive(Debug, PartialEq, Eq)]
pub enum ReadFrameResult {
    Frame(Vec<u8>),
    Oversized(usize),
    Eof,
}

/// Reads a single LF-delimited NDJSON line without allocating beyond `MAX_FRAME_BYTES`.
/// Must enforce `MAX_FRAME_BYTES` BEFORE any JSON parsing occurs.
pub async fn read_bounded_ndjson_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    max_bytes: usize,
) -> std::io::Result<ReadFrameResult> {
    let mut buf = Vec::new();
    let mut total_bytes: usize = 0;
    let mut oversized = false;

    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if total_bytes == 0 && buf.is_empty() {
                return Ok(ReadFrameResult::Eof);
            }
            if oversized {
                return Ok(ReadFrameResult::Oversized(total_bytes));
            }
            return Ok(ReadFrameResult::Frame(buf));
        }

        if let Some(newline_pos) = available.iter().position(|&b| b == b'\n') {
            let chunk = &available[..newline_pos];
            let effective_chunk = chunk.strip_suffix(b"\r").unwrap_or(chunk);
            total_bytes = total_bytes.saturating_add(effective_chunk.len());
            if oversized || total_bytes > max_bytes {
                reader.consume(newline_pos + 1);
                return Ok(ReadFrameResult::Oversized(total_bytes));
            }
            buf.extend_from_slice(effective_chunk);
            reader.consume(newline_pos + 1);
            return Ok(ReadFrameResult::Frame(buf));
        }

        let len = available.len();
        total_bytes = total_bytes.saturating_add(len);
        if !oversized {
            // Allow +1 byte for a trailing `\r` right before `\n` on the next buffer fill.
            if total_bytes > max_bytes.saturating_add(1) {
                oversized = true;
                buf.clear();
                buf.shrink_to_fit();
            } else {
                buf.extend_from_slice(available);
            }
        }
        reader.consume(len);
    }
}

/// Best-effort extraction of `requestId` from an already size-bounded JSON line when full decoding fails.
pub fn extract_request_id_bounded(line: &[u8]) -> Option<RequestId> {
    if line.len() > MAX_FRAME_BYTES {
        return None;
    }
    let val: serde_json::Value = serde_json::from_slice(line).ok()?;
    let rid = val.get("requestId")?.as_str()?;
    RequestId::new(rid).ok()
}

/// Decodes a bounded raw line into an `Envelope`, mapping `UnsupportedProtocolVersion`
/// to `BRIDGE_INCOMPATIBLE` and malformed/invalid frames to `INVALID_PAYLOAD`.
pub fn decode_bounded_frame(
    line: &[u8],
) -> Result<Envelope, (QueryErrorCode, String, Option<RequestId>)> {
    if line.len() > MAX_FRAME_BYTES {
        return Err((
            QueryErrorCode::InvalidPayload,
            format!(
                "frame exceeds maximum size of {MAX_FRAME_BYTES} bytes (got {})",
                line.len()
            ),
            None,
        ));
    }
    match decode_ndjson_bytes(line) {
        Ok(env) => Ok(env),
        Err(err) => {
            let request_id = extract_request_id_bounded(line);
            let code = match err {
                CodecError::UnsupportedProtocolVersion(_) => QueryErrorCode::BridgeIncompatible,
                _ => QueryErrorCode::InvalidPayload,
            };
            Err((code, err.to_string(), request_id))
        }
    }
}

/// Single serialized writer loop ensuring NDJSON frames never interleave on `stdout`.
/// An envelope that cannot be encoded (for example a response above `MAX_FRAME_BYTES`) is
/// replaced by a `core.error` carrying the same `requestId`, so the requester never hangs.
pub async fn run_ndjson_writer<W: AsyncWrite + Unpin>(
    mut rx: mpsc::Receiver<Envelope>,
    mut writer: W,
) {
    while let Some(env) = rx.recv().await {
        let line = match encode_ndjson_line(&env) {
            Ok(line) => line,
            Err(err) => {
                warn!(kind = ?env.kind, error = %err, "replacing unencodable outbound envelope with core.error");
                let mut reply = Envelope::error_response(
                    env.seq,
                    env.request_id.clone(),
                    QueryErrorCode::InvalidPayload,
                    format!("response {:?} could not be sent: {err}", env.kind),
                    false,
                );
                reply.core_epoch = env.core_epoch.clone();
                match encode_ndjson_line(&reply) {
                    Ok(line) => line,
                    Err(_) => continue,
                }
            }
        };
        if let Err(err) = writer.write_all(line.as_bytes()).await {
            warn!(error = %err, "stdout writer failed; exiting writer loop");
            break;
        }
        if let Err(err) = writer.flush().await {
            warn!(error = %err, "stdout flush failed; exiting writer loop");
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::BufReader;

    #[tokio::test]
    async fn bounded_reader_rejects_oversized_frame_without_full_allocation() {
        let oversized = vec![b'a'; MAX_FRAME_BYTES + 128];
        let mut stream = oversized;
        stream.push(b'\n');
        stream.extend_from_slice(br#"{"protocolVersion":1,"kind":"ui.ping","seq":1,"payload":{}}"#);
        stream.push(b'\n');

        let mut reader = BufReader::new(stream.as_slice());
        let first = read_bounded_ndjson_line(&mut reader, MAX_FRAME_BYTES)
            .await
            .unwrap();
        assert_eq!(first, ReadFrameResult::Oversized(MAX_FRAME_BYTES + 128));

        let second = read_bounded_ndjson_line(&mut reader, MAX_FRAME_BYTES)
            .await
            .unwrap();
        match second {
            ReadFrameResult::Frame(bytes) => {
                assert!(decode_bounded_frame(&bytes).is_ok());
            }
            other => panic!("expected second valid frame, got {other:?}"),
        }

        let eof = read_bounded_ndjson_line(&mut reader, MAX_FRAME_BYTES)
            .await
            .unwrap();
        assert_eq!(eof, ReadFrameResult::Eof);
    }

    #[tokio::test]
    async fn oversized_outbound_response_becomes_error_for_same_request() {
        let (tx, rx) = mpsc::channel(4);
        let mut env = Envelope::new(
            ytm_protocol::MessageKind::CoreCatalogResponse,
            7,
            serde_json::json!({ "blob": "x".repeat(MAX_FRAME_BYTES + 10) }),
        )
        .unwrap();
        env.request_id = Some(RequestId::new("ui-cat-1").unwrap());
        tx.send(env).await.unwrap();
        drop(tx);
        let mut out = Vec::new();
        run_ndjson_writer(rx, &mut out).await;
        let reply = decode_ndjson_bytes(&out).unwrap();
        assert_eq!(reply.kind, ytm_protocol::MessageKind::CoreError);
        assert_eq!(
            reply.request_id.as_ref().map(|r| r.as_str()),
            Some("ui-cat-1")
        );
    }

    #[test]
    fn unsupported_protocol_version_maps_to_bridge_incompatible() {
        let bad = br#"{"protocolVersion":99,"kind":"host.hello","requestId":"req-99","seq":1,"payload":{}}"#;
        let err = decode_bounded_frame(bad).unwrap_err();
        assert_eq!(err.0, QueryErrorCode::BridgeIncompatible);
        assert_eq!(err.2.as_ref().map(|r| r.as_str()), Some("req-99"));
    }
}
